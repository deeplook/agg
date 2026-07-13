//! Image format detection and natural-dimension extraction from raw file bytes.
//!
//! Ported from `inferMimeType`/`getImageDimensions` in asciinema-player's
//! `src/image/osc1337.js`. Formats are identified by magic bytes (not by the
//! sequence's parameters), matching the player.

use std::io::Cursor;

use super::Mime;

/// Detect the container format from the payload's leading bytes.
pub fn detect(data: &[u8]) -> Mime {
    if data.len() < 4 {
        return Mime::Unknown;
    }

    match data {
        [0x89, 0x50, 0x4E, 0x47, ..] => Mime::Png,
        [0xFF, 0xD8, 0xFF, ..] => Mime::Jpeg,
        [0x47, 0x49, 0x46, 0x38, ..] => Mime::Gif,
        [0x42, 0x4D, ..] => Mime::Bmp,
        [0x25, 0x50, 0x44, 0x46, ..] => Mime::Pdf,
        [0x52, 0x49, 0x46, 0x46, ..] if data.len() >= 12 && &data[8..12] == b"WEBP" => Mime::Webp,
        _ if looks_like_svg(data) => Mime::Svg,
        _ => Mime::Unknown,
    }
}

fn looks_like_svg(data: &[u8]) -> bool {
    // Match the player: an `<svg` tag near the start (possibly after `<?xml`).
    let head_len = data.len().min(256);
    let Ok(head) = std::str::from_utf8(&data[..head_len]) else {
        return false;
    };
    let trimmed = head.trim_start();
    (trimmed.starts_with("<svg") || trimmed.starts_with("<?xml")) && head.contains("<svg")
}

/// Read the image's natural pixel dimensions from its header, when the format
/// exposes them cheaply. Returns `None` when unknown (e.g. PDF, or a header we
/// can't parse); callers fall back to other sizing hints.
pub fn natural_dimensions(data: &[u8], mime: Mime) -> Option<(u32, u32)> {
    match mime {
        Mime::Png | Mime::Jpeg | Mime::Gif | Mime::Webp | Mime::Bmp => {
            image::ImageReader::new(Cursor::new(data))
                .with_guessed_format()
                .ok()?
                .into_dimensions()
                .ok()
        }
        Mime::Svg => svg_dimensions(data),
        // Raw kitty formats carry dimensions in the protocol, not the data;
        // PDF and unknown expose none here.
        Mime::Rgb | Mime::Rgba | Mime::Pdf | Mime::Unknown => None,
    }
}

/// Parse `width`/`height` (or `viewBox`) off the opening `<svg>` tag.
fn svg_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let text = std::str::from_utf8(data).ok()?;
    let start = text.find("<svg")?;
    let tag_end = text[start..].find('>')? + start;
    let tag = &text[start..=tag_end];

    if let (Some(w), Some(h)) = (attr_number(tag, "width"), attr_number(tag, "height")) {
        return Some((w.ceil() as u32, h.ceil() as u32));
    }

    // Fall back to viewBox="minX minY width height".
    let vb = tag_attr(tag, "viewBox")?;
    let mut nums = vb.split_whitespace();
    let _min_x = nums.next()?;
    let _min_y = nums.next()?;
    let w: f64 = nums.next()?.parse().ok()?;
    let h: f64 = nums.next()?.parse().ok()?;
    Some((w.ceil() as u32, h.ceil() as u32))
}

/// Extract the raw value of `name="..."` (single or double quoted) from a tag.
fn tag_attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let mut search_from = 0;
    while let Some(rel) = tag[search_from..].find(name) {
        let idx = search_from + rel;
        // Require a boundary before the attribute name so `width` doesn't match
        // inside another attribute.
        let boundary_ok = idx == 0 || !tag.as_bytes()[idx - 1].is_ascii_alphanumeric();
        let rest = tag[idx + name.len()..].trim_start();
        if boundary_ok {
            if let Some(rest) = rest.strip_prefix('=') {
                let rest = rest.trim_start();
                let quote = rest.chars().next()?;
                if quote == '"' || quote == '\'' {
                    let value = &rest[1..];
                    let end = value.find(quote)?;
                    return Some(&value[..end]);
                }
            }
        }
        search_from = idx + name.len();
    }
    None
}

/// Parse an attribute whose value is a number with an optional `px` suffix.
fn attr_number(tag: &str, name: &str) -> Option<f64> {
    let value = tag_attr(tag, name)?.trim();
    let value = value.strip_suffix("px").unwrap_or(value).trim();
    value.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(w, h)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        buf.into_inner()
    }

    #[test]
    fn detects_png_and_reads_dimensions() {
        let png = png_bytes(2, 3);
        assert_eq!(detect(&png), Mime::Png);
        assert_eq!(natural_dimensions(&png, Mime::Png), Some((2, 3)));
    }

    #[test]
    fn detects_gif_by_magic() {
        assert_eq!(detect(b"GIF89a\x0a\x00\x0a\x00"), Mime::Gif);
    }

    #[test]
    fn detects_bmp_and_pdf() {
        assert_eq!(detect(b"BM....................."), Mime::Bmp);
        assert_eq!(detect(b"%PDF-1.7"), Mime::Pdf);
    }

    #[test]
    fn detects_webp_only_with_webp_tag() {
        assert_eq!(detect(b"RIFF\x00\x00\x00\x00WEBPVP8 "), Mime::Webp);
        assert_eq!(detect(b"RIFF\x00\x00\x00\x00WAVEfmt "), Mime::Unknown);
    }

    #[test]
    fn reads_svg_dimensions_from_attrs_and_viewbox() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="60"></svg>"#;
        assert_eq!(detect(svg), Mime::Svg);
        assert_eq!(natural_dimensions(svg, Mime::Svg), Some((120, 60)));

        let vb = br#"<?xml version="1.0"?><svg viewBox="0 0 200 100"></svg>"#;
        assert_eq!(detect(vb), Mime::Svg);
        assert_eq!(natural_dimensions(vb, Mime::Svg), Some((200, 100)));
    }

    #[test]
    fn unknown_for_short_or_unrecognized() {
        assert_eq!(detect(b"ab"), Mime::Unknown);
        assert_eq!(detect(b"not an image at all"), Mime::Unknown);
    }
}
