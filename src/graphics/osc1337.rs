//! iTerm2 inline-image protocol (OSC 1337) parser.
//!
//! Ported from asciinema-player's `src/image/osc1337.js`. Supports both the
//! simple `File=` form and the chunked `MultipartFile`/`FilePart`/`FileEnd`
//! form, buffers sequences split across `parse` calls, and passes non-image
//! OSC sequences through unchanged so the VT still sees them.
//!
//! Terminal output is turned into an ordered stream of [`Segment`]s: text runs
//! to feed to the VT, interleaved with completed [`Image`]s to place at the
//! cursor position reached by the preceding text.

use std::mem;

use base64::Engine;

use super::{animation, format, Dim, Image, Mime, Segment};

/// Reusable OSC 1337 parser. Holds cross-call state: a buffer for an incomplete
/// trailing sequence and any in-progress multipart transfer.
pub struct Osc1337Parser {
    buffer: String,
    multipart: Option<Multipart>,
}

struct Multipart {
    width: Dim,
    height: Dim,
    preserve_aspect: bool,
    chunks: String,
}

impl Osc1337Parser {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            multipart: None,
        }
    }

    /// Drop all cross-call state. Called on terminal reset (`ESC c`).
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.multipart = None;
    }

    /// Parse a chunk of terminal output into interleaved text/image segments.
    pub fn parse(&mut self, text: &str) -> Vec<Segment> {
        let mut segments = Vec::new();
        let mut current = String::new();

        // Prepend any buffered incomplete sequence from the previous call.
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
            let Some(esc) = find(bytes, b"\x1b]", i) else {
                // No more OSC sequences; the rest is plain text.
                current.push_str(&data[i..]);
                break;
            };

            current.push_str(&data[i..esc]);

            let Some((end, term_len)) = find_osc_end(bytes, esc) else {
                // Incomplete sequence: buffer it (with its ESC ]) for next call.
                self.buffer = data[esc..].to_owned();
                break;
            };

            let content = &data[esc + 2..end];

            match self.process_osc(content) {
                Some(image) => {
                    if !current.is_empty() {
                        segments.push(Segment::Text(mem::take(&mut current)));
                    }
                    segments.push(Segment::Image(image));
                }
                None => {
                    // Not an image sequence — pass it through to the VT verbatim.
                    current.push_str(&data[esc..end + term_len]);
                }
            }

            i = end + term_len;
        }

        if !current.is_empty() {
            segments.push(Segment::Text(current));
        }

        segments
    }

    /// Process one OSC sequence's content (between `ESC ]` and its terminator).
    /// Returns a completed image when one becomes ready.
    fn process_osc(&mut self, content: &str) -> Option<Image> {
        let payload = content.strip_prefix("1337;")?;

        // Simple form: File=[params]:base64data
        if let Some(rest) = payload.strip_prefix("File=") {
            let colon = rest.find(':')?;
            let params = parse_params(&rest[..colon]);
            let base64_data = &rest[colon + 1..];

            if params.get("inline").map(String::as_str) != Some("1") {
                return None;
            }

            return create_image(&params, base64_data);
        }

        // Multipart form: MultipartFile=[params] then FilePart=... then FileEnd
        if let Some(rest) = payload.strip_prefix("MultipartFile=") {
            let params = parse_params(rest);

            if params.get("inline").map(String::as_str) != Some("1") {
                self.multipart = None;
                return None;
            }

            self.multipart = Some(Multipart {
                width: parse_dimension(params.get("width")),
                height: parse_dimension(params.get("height")),
                preserve_aspect: params.get("preserveaspectratio").map(String::as_str) != Some("0"),
                chunks: String::new(),
            });
            return None;
        }

        if let Some(chunk) = payload.strip_prefix("FilePart=") {
            if let Some(mp) = &mut self.multipart {
                mp.chunks.push_str(chunk);
            }
            return None;
        }

        if payload == "FileEnd" {
            let mp = self.multipart.take()?;
            return build_image(mp.width, mp.height, mp.preserve_aspect, &mp.chunks);
        }

        None
    }
}

impl Default for Osc1337Parser {
    fn default() -> Self {
        Self::new()
    }
}

/// Find the end of an OSC sequence started at `start`. Terminators, in priority
/// of earliest position: BEL (`\x07`, len 1), ST (`ESC \`, len 2), or the next
/// `ESC ]` (len 0, not consumed) for sequences chained without a terminator.
fn find_osc_end(data: &[u8], start: usize) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;

    let mut consider = |idx: Option<usize>, len: usize| {
        if let Some(idx) = idx {
            if best.is_none_or(|(b, _)| idx < b) {
                best = Some((idx, len));
            }
        }
    };

    consider(find(data, b"\x07", start), 1);
    consider(find(data, b"\x1b\\", start), 2);
    consider(find(data, b"\x1b]", start + 2), 0);

    best
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

fn parse_params(param_str: &str) -> std::collections::HashMap<String, String> {
    let mut params = std::collections::HashMap::new();

    for pair in param_str.split(';') {
        if let Some(eq) = pair.find('=') {
            if eq > 0 {
                params.insert(pair[..eq].to_ascii_lowercase(), pair[eq + 1..].to_owned());
            }
        }
    }

    params
}

/// Parse an iTerm2 dimension: `N` (cells), `Npx`, `N%`, or `auto`/absent.
fn parse_dimension(value: Option<&String>) -> Dim {
    let Some(value) = value else {
        return Dim::Auto;
    };
    let trimmed = value.trim();

    if trimmed.is_empty() || trimmed == "auto" {
        return Dim::Auto;
    }

    if let Some(num) = trimmed.strip_suffix('%') {
        return num.trim().parse().map(Dim::Percent).unwrap_or(Dim::Auto);
    }

    if let Some(num) = trimmed.strip_suffix("px") {
        return num.trim().parse().map(Dim::Px).unwrap_or(Dim::Auto);
    }

    trimmed.parse().map(Dim::Cells).unwrap_or(Dim::Auto)
}

fn create_image(
    params: &std::collections::HashMap<String, String>,
    base64_data: &str,
) -> Option<Image> {
    build_image(
        parse_dimension(params.get("width")),
        parse_dimension(params.get("height")),
        params.get("preserveaspectratio").map(String::as_str) != Some("0"),
        base64_data,
    )
}

fn build_image(width: Dim, height: Dim, preserve_aspect: bool, base64_data: &str) -> Option<Image> {
    // iTerm2 encodes payloads with standard base64. Reject rather than place a
    // broken image when decoding fails.
    let data = base64::engine::general_purpose::STANDARD
        .decode(base64_data.trim())
        .ok()?;

    if data.is_empty() {
        return None;
    }

    let mime = format::detect(&data);
    if mime == Mime::Unknown {
        return None;
    }

    let natural = format::natural_dimensions(&data, mime);
    let animation = animation::parse(&data, mime);

    Some(Image {
        id: Image::next_id(),
        data,
        mime,
        natural,
        width,
        height,
        preserve_aspect,
        animation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // 1x1 PNG, base64-encoded.
    const PNG_B64: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    fn image_segments(segs: &[Segment]) -> Vec<&Image> {
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

    #[test]
    fn parses_simple_inline_image_and_keeps_surrounding_text() {
        let mut p = Osc1337Parser::new();
        let input = format!("before\x1b]1337;File=inline=1:{PNG_B64}\x07after");
        let segs = p.parse(&input);

        let imgs = image_segments(&segs);
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].mime, Mime::Png);
        assert_eq!(imgs[0].natural, Some((1, 1)));
        assert_eq!(text(&segs), "beforeafter");

        // Text precedes the image in order.
        assert!(matches!(segs[0], Segment::Text(_)));
        assert!(matches!(segs[1], Segment::Image(_)));
    }

    #[test]
    fn ignores_non_inline_images() {
        let mut p = Osc1337Parser::new();
        let input = format!("\x1b]1337;File=inline=0:{PNG_B64}\x07x");
        let segs = p.parse(&input);
        // No image is produced; the sequence is passed through to the VT
        // verbatim (which ignores it), matching the player.
        assert!(image_segments(&segs).is_empty());
        assert_eq!(text(&segs), input);
    }

    #[test]
    fn passes_through_non_1337_osc() {
        let mut p = Osc1337Parser::new();
        // OSC 0 (window title) must reach the VT unchanged.
        let segs = p.parse("a\x1b]0;title\x07b");
        assert!(image_segments(&segs).is_empty());
        assert_eq!(text(&segs), "a\x1b]0;title\x07b");
    }

    #[test]
    fn buffers_sequence_split_across_calls() {
        let mut p = Osc1337Parser::new();
        let full = format!("\x1b]1337;File=inline=1:{PNG_B64}\x07");
        let split = full.len() / 2;

        let segs1 = p.parse(&full[..split]);
        assert!(image_segments(&segs1).is_empty());

        let segs2 = p.parse(&full[split..]);
        assert_eq!(image_segments(&segs2).len(), 1);
    }

    #[test]
    fn assembles_multipart_transfer() {
        let mut p = Osc1337Parser::new();
        let (a, b) = PNG_B64.split_at(PNG_B64.len() / 2);
        let input = format!(
            "\x1b]1337;MultipartFile=inline=1\x07\x1b]1337;FilePart={a}\x07\x1b]1337;FilePart={b}\x07\x1b]1337;FileEnd\x07"
        );
        let segs = p.parse(&input);
        let imgs = image_segments(&segs);
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].natural, Some((1, 1)));
    }

    #[test]
    fn parses_dimension_forms() {
        assert_eq!(parse_dimension(Some(&"10".to_owned())), Dim::Cells(10.0));
        assert_eq!(parse_dimension(Some(&"40px".to_owned())), Dim::Px(40.0));
        assert_eq!(parse_dimension(Some(&"50%".to_owned())), Dim::Percent(50.0));
        assert_eq!(parse_dimension(Some(&"auto".to_owned())), Dim::Auto);
        assert_eq!(parse_dimension(None), Dim::Auto);
    }
}
