//! Sixel graphics parser.
//!
//! Sixel images arrive as a DCS sequence (`ESC P <params> q <data> ESC \`).
//! Unlike PNG/GIF, the payload is a palette + run-length band bitmap, so it is
//! decoded straight to pixels here (via the `sixel-image` crate, the decoder
//! used by Zellij) and wrapped as a raw-RGBA [`Image`] — reusing the same
//! placement and compositing path as every other inline image.
//!
//! Only sixel DCS sequences are intercepted; other DCS sequences pass through
//! to the VT unchanged. Spec: <https://vt100.net/docs/vt3xx-gp/chapter14.html>

use std::mem;

use sixel_image::{SixelColor, SixelImage};

use super::{Dim, Image, Mime, Segment};

/// Reusable sixel parser. Buffers an incomplete trailing sequence across calls.
pub struct SixelParser {
    buffer: String,
}

impl SixelParser {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
        }
    }

    /// Drop the incomplete-sequence buffer (terminal reset).
    pub fn reset(&mut self) {
        self.buffer.clear();
    }

    /// Parse a chunk of terminal output into interleaved text/image segments.
    pub fn parse(&mut self, text: &str) -> Vec<Segment> {
        let mut segments = Vec::new();
        let mut current = String::new();

        let data = if self.buffer.is_empty() {
            text.to_owned()
        } else {
            let mut d = mem::take(&mut self.buffer);
            d.push_str(text);
            d
        };
        let bytes = data.as_bytes();

        let mut i = 0;
        while i < bytes.len() {
            let Some(start) = find(bytes, b"\x1bP", i) else {
                current.push_str(&data[i..]);
                break;
            };

            current.push_str(&data[i..start]);

            let Some((end, term_len)) = find_dcs_end(bytes, start + 2) else {
                // Incomplete sequence: buffer it (with its ESC P) for next call.
                self.buffer = data[start..].to_owned();
                break;
            };

            let sequence = &data[start..end + term_len];
            let inner = &data[start + 2..end];

            match decode_sixel(inner, sequence) {
                Some(image) => {
                    if !current.is_empty() {
                        segments.push(Segment::Text(mem::take(&mut current)));
                    }
                    segments.push(Segment::Image(image));
                }
                // Not a sixel (or failed to decode): leave the DCS in the text
                // stream; the VT ignores DCS sequences it doesn't handle.
                None => current.push_str(sequence),
            }

            i = end + term_len;
        }

        if !current.is_empty() {
            segments.push(Segment::Text(current));
        }

        segments
    }
}

impl Default for SixelParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Decode a sixel DCS into a raw-RGBA [`Image`]. `inner` is the body between
/// `ESC P` and the terminator; `sequence` is the whole DCS (what the decoder
/// wants). Returns `None` for non-sixel DCS or a payload that fails to decode.
fn decode_sixel(inner: &str, sequence: &str) -> Option<Image> {
    // The sixel introducer is `<numeric params> q`; anything else is a
    // different DCS (e.g. DECRQSS) that we must not consume.
    let q = inner.find('q')?;
    if !inner[..q].bytes().all(|b| b.is_ascii_digit() || b == b';') {
        return None;
    }

    let sixel = SixelImage::new(sequence.as_bytes()).ok()?;
    let (height, width) = sixel.pixel_size();
    if width == 0 || height == 0 {
        return None;
    }

    let mut data = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        let row = sixel.pixels.get(y);
        for x in 0..width {
            match row.and_then(|r| r.get(x)) {
                Some(pixel) if pixel.on => {
                    let (r, g, b) = resolve_color(&sixel, pixel.color);
                    data.extend_from_slice(&[r, g, b, 255]);
                }
                // Unset pixels are transparent so the terminal shows through.
                _ => data.extend_from_slice(&[0, 0, 0, 0]),
            }
        }
    }

    Some(Image {
        id: Image::next_id(),
        data,
        mime: Mime::Rgba,
        natural: Some((width as u32, height as u32)),
        width: Dim::Auto,
        height: Dim::Auto,
        preserve_aspect: true,
        animation: None,
    })
}

fn resolve_color(sixel: &SixelImage, register: u16) -> (u8, u8, u8) {
    match sixel.color_registers.get(&register) {
        // Sixel RGB components are percentages (0..=100).
        Some(SixelColor::Rgb(r, g, b)) => (pct(*r), pct(*g), pct(*b)),
        // Sixel stores HLS as (hue 0..360, lightness 0..100, saturation 0..100).
        Some(SixelColor::Hsl(h, l, s)) => hls_to_rgb(*h, *l, *s),
        None => (0, 0, 0),
    }
}

fn pct(v: u8) -> u8 {
    ((v.min(100) as u16 * 255 + 50) / 100) as u8
}

fn hls_to_rgb(h: u16, l: u8, s: u8) -> (u8, u8, u8) {
    let h = (h % 360) as f64;
    let l = (l.min(100) as f64) / 100.0;
    let s = (s.min(100) as f64) / 100.0;

    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;

    let (r, g, b) = match h as u16 / 60 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };

    let to_u8 = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (to_u8(r), to_u8(g), to_u8(b))
}

/// Find the end of a DCS sequence: ST (`ESC \`, len 2) or BEL (`\x07`, len 1).
fn find_dcs_end(data: &[u8], start: usize) -> Option<(usize, usize)> {
    let st = find(data, b"\x1b\\", start);
    let bel = find(data, b"\x07", start);

    match (st, bel) {
        (Some(s), Some(b)) if b < s => Some((b, 1)),
        (Some(s), _) => Some((s, 2)),
        (None, Some(b)) => Some((b, 1)),
        (None, None) => None,
    }
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn images(segs: &[Segment]) -> Vec<&Image> {
        segs.iter()
            .filter_map(|s| match s {
                Segment::Image(img) => Some(img),
                _ => None,
            })
            .collect()
    }

    fn text(segs: &[Segment]) -> String {
        segs.iter()
            .filter_map(|s| match s {
                Segment::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    // Register 0 = red (RGB 100,0,0); `!4~` paints 4 columns of a full
    // 6-pixel band -> a 4x6 red block.
    const RED_BLOCK: &str = "\x1bPq#0;2;100;0;0#0!4~\x1b\\";

    #[test]
    fn decodes_a_sixel_block_to_rgba() {
        let mut p = SixelParser::new();
        let segs = p.parse(&format!("before{RED_BLOCK}after"));

        let imgs = images(&segs);
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].mime, Mime::Rgba);
        assert_eq!(imgs[0].natural, Some((4, 6)));
        // Top-left pixel is opaque red.
        assert_eq!(&imgs[0].data[0..4], &[255, 0, 0, 255]);
        // Surrounding text survives, sixel stripped.
        assert_eq!(text(&segs), "beforeafter");
    }

    #[test]
    fn passes_through_non_sixel_dcs() {
        let mut p = SixelParser::new();
        // DECRQSS-style DCS (starts with `$q`, not a sixel `q`): must reach the VT.
        let input = "x\x1bP$qm\x1b\\y";
        let segs = p.parse(input);
        assert!(images(&segs).is_empty());
        assert_eq!(text(&segs), input);
    }

    #[test]
    fn buffers_sequence_split_across_calls() {
        let mut p = SixelParser::new();
        let split = RED_BLOCK.len() / 2;

        assert!(images(&p.parse(&RED_BLOCK[..split])).is_empty());
        assert_eq!(images(&p.parse(&RED_BLOCK[split..])).len(), 1);
    }

    #[test]
    fn hls_primary_hue_is_red() {
        // HLS hue 0, lightness 50, saturation 100 -> pure red.
        assert_eq!(hls_to_rgb(0, 50, 100), (255, 0, 0));
    }
}
