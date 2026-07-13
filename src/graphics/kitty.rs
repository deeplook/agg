//! Kitty graphics protocol parser.
//!
//! Handles the terminal graphics protocol's APC sequences
//! (`ESC _ G <control> ; <base64 payload> ESC \`), producing the same
//! [`Segment`] stream as the iTerm2 parser so images flow through the identical
//! placement and compositing path.
//!
//! Supported: direct transmission (`t=d`, the only medium meaningful for a
//! recording) of PNG (`f=100`), raw RGB (`f=24`) and raw RGBA (`f=32`) data,
//! optionally zlib-compressed (`o=z`) and/or split across chunks (`m=1`);
//! transmit-and-display (`a=T`), transmit-then-put (`a=t` then `a=p`, keyed by
//! image id), cell-sized placement (`c`/`r`), and delete-all (`a=d`).
//!
//! Not modelled (a recording-to-GIF renderer doesn't need them): file/shared-
//! memory media, animation frames, unicode placeholders, selective deletes
//! (which are treated as no-ops so nothing is wrongly removed), and the
//! do-not-move-cursor flag.
//!
//! Spec: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>

use std::collections::HashMap;
use std::io::Read;
use std::mem;

use base64::Engine;

use super::{animation, format, Dim, Image, Mime, Segment};

type Control = HashMap<char, String>;

/// Raw bytes, format, and (declared or header-read) pixel dimensions of an
/// assembled image payload.
type Assembled = (Vec<u8>, Mime, Option<(u32, u32)>);

/// Reusable kitty parser. Holds cross-call state: a buffer for an incomplete
/// trailing sequence, an in-progress chunked transfer, and images transmitted
/// (`a=t`) but not yet displayed, keyed by image id.
pub struct KittyParser {
    buffer: String,
    transfer: Option<Transfer>,
    stored: HashMap<u32, StoredImage>,
}

struct Transfer {
    control: Control,
    payload: String,
}

struct StoredImage {
    data: Vec<u8>,
    mime: Mime,
    natural: Option<(u32, u32)>,
}

impl KittyParser {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            transfer: None,
            stored: HashMap::new(),
        }
    }

    /// Drop all cross-call state. Called on terminal reset (`ESC c`).
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.transfer = None;
        self.stored.clear();
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
            let Some(start) = find(bytes, b"\x1b_G", i) else {
                current.push_str(&data[i..]);
                break;
            };

            current.push_str(&data[i..start]);

            let Some((end, term_len)) = find_apc_end(bytes, start + 3) else {
                // Incomplete sequence: buffer it (with its ESC _ G) for next call.
                self.buffer = data[start..].to_owned();
                break;
            };

            let content = &data[start + 3..end];
            if let Some(segment) = self.process(content) {
                if !current.is_empty() {
                    segments.push(Segment::Text(mem::take(&mut current)));
                }
                segments.push(segment);
            }

            i = end + term_len;
        }

        if !current.is_empty() {
            segments.push(Segment::Text(current));
        }

        segments
    }

    /// Process one APC sequence body (control block `;` optional payload).
    fn process(&mut self, content: &str) -> Option<Segment> {
        let (control_str, payload) = match content.split_once(';') {
            Some((c, p)) => (c, p),
            None => (content, ""),
        };
        let control = parse_control(control_str);

        let more = control.get(&'m').map(String::as_str) == Some("1");

        // Accumulate chunked transfers; the full control block arrives with the
        // first chunk, continuation chunks carry only `m` and payload.
        let (control, payload) = match self.transfer.take() {
            Some(mut transfer) => {
                transfer.payload.push_str(payload);
                if more {
                    self.transfer = Some(transfer);
                    return None;
                }
                (transfer.control, transfer.payload)
            }
            None => {
                // Delete needs no payload, so handle it before buffering.
                if control.get(&'a').map(String::as_str) == Some("d") {
                    return delete(&control);
                }
                if more {
                    self.transfer = Some(Transfer {
                        control,
                        payload: payload.to_owned(),
                    });
                    return None;
                }
                (control, payload.to_owned())
            }
        };

        self.dispatch(&control, &payload)
    }

    fn dispatch(&mut self, control: &Control, payload: &str) -> Option<Segment> {
        let action = control.get(&'a').map(String::as_str).unwrap_or("t");

        match action {
            // Display a previously transmitted image, sized by this command.
            "p" => {
                let key = image_key(control)?;
                let stored = self.stored.get(&key)?;
                Some(Segment::Image(build(
                    stored.data.clone(),
                    stored.mime,
                    stored.natural,
                    control,
                )))
            }

            "d" => delete(control),

            // Transmit ("t") or transmit-and-display ("T").
            "t" | "T" => {
                let (data, mime, natural) = assemble(control, payload)?;

                if let Some(key) = image_key(control) {
                    self.stored.insert(
                        key,
                        StoredImage {
                            data: data.clone(),
                            mime,
                            natural,
                        },
                    );
                }

                (action == "T").then(|| Segment::Image(build(data, mime, natural, control)))
            }

            // query, animation, compose, ... — nothing to render.
            _ => None,
        }
    }
}

impl Default for KittyParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Delete: only the "all" forms clear placements. Selective deletes (by id,
/// position, ...) are treated as no-ops so nothing visible is wrongly removed.
fn delete(control: &Control) -> Option<Segment> {
    match control.get(&'d').map(String::as_str) {
        None | Some("a") | Some("A") => Some(Segment::ClearImages),
        _ => None,
    }
}

/// Assemble the raw file/pixel bytes from a (possibly compressed) base64
/// payload and determine its format.
fn assemble(control: &Control, payload: &str) -> Option<Assembled> {
    // Only direct transmission carries data inline; other media reference the
    // recorder's filesystem, which isn't available at render time.
    if let Some(medium) = control.get(&'t') {
        if medium != "d" {
            return None;
        }
    }

    let mut bytes = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .ok()?;

    if control.get(&'o').map(String::as_str) == Some("z") {
        let mut decoder = flate2::read::ZlibDecoder::new(&bytes[..]);
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).ok()?;
        bytes = out;
    }

    if bytes.is_empty() {
        return None;
    }

    let format = control.get(&'f').and_then(|f| f.parse::<u32>().ok());
    let declared = control
        .get(&'s')
        .zip(control.get(&'v'))
        .and_then(|(s, v)| Some((s.parse().ok()?, v.parse().ok()?)));

    match format {
        Some(100) => Some((
            bytes.clone(),
            Mime::Png,
            format::natural_dimensions(&bytes, Mime::Png),
        )),
        Some(24) => declared.map(|d| (bytes, Mime::Rgb, Some(d))),
        Some(32) => declared.map(|d| (bytes, Mime::Rgba, Some(d))),
        None => {
            // No explicit format: sniff for a container, else fall back to the
            // protocol default of raw RGBA when dimensions are declared.
            let mime = format::detect(&bytes);
            if mime != Mime::Unknown {
                let natural = format::natural_dimensions(&bytes, mime);
                Some((bytes, mime, natural))
            } else {
                declared.map(|d| (bytes, Mime::Rgba, Some(d)))
            }
        }
        _ => None,
    }
}

/// Build an [`Image`], taking display sizing from the command's `c`/`r`
/// (columns/rows). Absent dimensions become [`Dim::Auto`], matching the iTerm2
/// path so natural size drives the cell span.
fn build(data: Vec<u8>, mime: Mime, natural: Option<(u32, u32)>, control: &Control) -> Image {
    let cells = |key: char| {
        control
            .get(&key)
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
            .map(Dim::Cells)
            .unwrap_or(Dim::Auto)
    };

    let animation = animation::parse(&data, mime);

    Image {
        id: Image::next_id(),
        data,
        mime,
        natural,
        width: cells('c'),
        height: cells('r'),
        preserve_aspect: true,
        animation,
    }
}

/// The image key used to store/retrieve transmitted images: id (`i`) preferred,
/// else image number (`I`).
fn image_key(control: &Control) -> Option<u32> {
    control
        .get(&'i')
        .or_else(|| control.get(&'I'))
        .and_then(|v| v.parse().ok())
}

fn parse_control(control: &str) -> Control {
    let mut map = HashMap::new();

    for pair in control.split(',') {
        if let Some((key, value)) = pair.split_once('=') {
            if let Some(key) = key.trim().chars().next() {
                map.insert(key, value.trim().to_owned());
            }
        }
    }

    map
}

/// Find the end of an APC sequence: ST (`ESC \`, len 2) or BEL (`\x07`, len 1),
/// whichever comes first.
fn find_apc_end(data: &[u8], start: usize) -> Option<(usize, usize)> {
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

    // 1x1 PNG, base64-encoded.
    const PNG_B64: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    fn images(segs: &[Segment]) -> Vec<&Image> {
        segs.iter()
            .filter_map(|s| match s {
                Segment::Image(img) => Some(img),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn transmits_and_displays_png() {
        let mut p = KittyParser::new();
        let segs = p.parse(&format!("x\x1b_Ga=T,f=100;{PNG_B64}\x1b\\y"));

        let imgs = images(&segs);
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].mime, Mime::Png);
        assert_eq!(imgs[0].natural, Some((1, 1)));

        let text: String = segs
            .iter()
            .filter_map(|s| match s {
                Segment::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "xy");
    }

    #[test]
    fn honors_cell_sizing() {
        let mut p = KittyParser::new();
        let segs = p.parse(&format!("\x1b_Ga=T,f=100,c=5,r=3;{PNG_B64}\x1b\\"));
        let imgs = images(&segs);
        assert_eq!(imgs[0].width, Dim::Cells(5.0));
        assert_eq!(imgs[0].height, Dim::Cells(3.0));
    }

    #[test]
    fn reassembles_chunked_transmission() {
        let mut p = KittyParser::new();
        let (a, b) = PNG_B64.split_at(PNG_B64.len() / 2);
        // First chunk carries the control block with m=1; the last carries m=0.
        let input = format!("\x1b_Ga=T,f=100,m=1;{a}\x1b\\\x1b_Gm=0;{b}\x1b\\");
        let segs = p.parse(&input);
        let imgs = images(&segs);
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].natural, Some((1, 1)));
    }

    #[test]
    fn transmit_then_put_displays_stored_image() {
        let mut p = KittyParser::new();

        // Transmit only (a=t): stored, not displayed.
        let segs = p.parse(&format!("\x1b_Ga=t,f=100,i=7;{PNG_B64}\x1b\\"));
        assert!(images(&segs).is_empty());

        // Put (a=p) by id: displayed, sized by this command.
        let segs = p.parse("\x1b_Ga=p,i=7,c=4\x1b\\");
        let imgs = images(&segs);
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].width, Dim::Cells(4.0));
    }

    #[test]
    fn raw_rgba_uses_declared_dimensions() {
        let mut p = KittyParser::new();
        // 2x1 RGBA: red, green.
        let raw: Vec<u8> = vec![255, 0, 0, 255, 0, 255, 0, 255];
        let b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
        let segs = p.parse(&format!("\x1b_Ga=T,f=32,s=2,v=1;{b64}\x1b\\"));
        let imgs = images(&segs);
        assert_eq!(imgs[0].mime, Mime::Rgba);
        assert_eq!(imgs[0].natural, Some((2, 1)));
    }

    #[test]
    fn zlib_compressed_payload_is_inflated() {
        use flate2::{write::ZlibEncoder, Compression};
        use std::io::Write;

        let raw: Vec<u8> = vec![10, 20, 30, 40, 50, 60]; // 2x1 RGB
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&raw).unwrap();
        let compressed = encoder.finish().unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&compressed);

        let mut p = KittyParser::new();
        let segs = p.parse(&format!("\x1b_Ga=T,f=24,s=2,v=1,o=z;{b64}\x1b\\"));
        let imgs = images(&segs);
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].mime, Mime::Rgb);
        assert_eq!(imgs[0].data, raw);
    }

    #[test]
    fn delete_all_clears_images() {
        let mut p = KittyParser::new();
        assert!(matches!(
            p.parse("\x1b_Ga=d\x1b\\").as_slice(),
            [Segment::ClearImages]
        ));
        assert!(matches!(
            p.parse("\x1b_Ga=d,d=a\x1b\\").as_slice(),
            [Segment::ClearImages]
        ));
        // Selective delete is a no-op.
        assert!(p.parse("\x1b_Ga=d,d=i,i=3\x1b\\").is_empty());
    }

    #[test]
    fn buffers_sequence_split_across_calls() {
        let mut p = KittyParser::new();
        let full = format!("\x1b_Ga=T,f=100;{PNG_B64}\x1b\\");
        let split = full.len() / 2;

        assert!(images(&p.parse(&full[..split])).is_empty());
        assert_eq!(images(&p.parse(&full[split..])).len(), 1);
    }

    #[test]
    fn passes_through_non_graphics_text() {
        let mut p = KittyParser::new();
        let segs = p.parse("plain text with no graphics");
        assert!(images(&segs).is_empty());
        let text: String = segs
            .iter()
            .filter_map(|s| match s {
                Segment::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "plain text with no graphics");
    }
}
