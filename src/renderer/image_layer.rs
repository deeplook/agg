//! Decoding and compositing of inline images over a rendered frame.
//!
//! Shared by both renderers: each decodes an [`Image`] once (cached by id) into
//! straight-alpha RGBA, then blits every active [`Placement`] onto the frame
//! buffer using cell geometry supplied by the caller. This mirrors
//! asciinema-player's overlay: image height spans `display_rows` cells, width
//! follows the natural aspect ratio (letterboxed to fit the space remaining to
//! the right edge), and placements scrolled above the top are clipped.

use std::collections::HashMap;
use std::io::Cursor;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use image::AnimationDecoder;
use rgb::RGBA8;

use crate::graphics::{Dim, Image, Mime, Placement};

/// A decoded image in straight-alpha RGBA8, ready to sample.
pub struct DecodedImage {
    width: usize,
    height: usize,
    pixels: Vec<RGBA8>,
}

/// Per-renderer cache of decoded image frames, keyed by [`Image::id`]. Static
/// images decode to a single frame; animated GIF/APNG to one per frame. A
/// failed decode is cached as `None` so it is attempted only once.
#[derive(Default)]
pub struct DecodeCache {
    cache: HashMap<u64, Option<Vec<DecodedImage>>>,
}

impl DecodeCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn get(&mut self, image: &Image) -> Option<&[DecodedImage]> {
        self.cache
            .entry(image.id)
            .or_insert_with(|| decode(image))
            .as_deref()
    }
}

/// Cell geometry of a rendered frame. The top-left pixel of grid cell
/// `(col, row)` is `(margin_l + col * char_w, margin_t + row * char_h)`.
pub struct Grid {
    pub char_w: f64,
    pub char_h: f64,
    pub margin_l: f64,
    pub margin_t: f64,
    pub cols: usize,
    pub pixel_width: usize,
    pub pixel_height: usize,
}

/// Composite every placement onto `buf` (row-major RGBA8, `pixel_width` wide).
pub fn composite(buf: &mut [RGBA8], grid: &Grid, placements: &[Placement], cache: &mut DecodeCache) {
    for placement in placements {
        if let Some(frames) = cache.get(&placement.image) {
            if let Some(frame) = frames.get(placement.anim_frame.min(frames.len().saturating_sub(1)))
            {
                draw(buf, grid, placement, frame);
            }
        }
    }
}

fn draw(buf: &mut [RGBA8], grid: &Grid, placement: &Placement, image: &DecodedImage) {
    if placement.col >= grid.cols || image.width == 0 || image.height == 0 {
        return;
    }

    let iw = image.width as f64;
    let ih = image.height as f64;

    // The placement box: `display_rows` cells tall, and as wide as the image's
    // declared width (kitty `c` columns / iTerm2 width), else the space
    // remaining to the right edge. The image is scaled to fit inside the box
    // preserving aspect ratio (letterboxed, matching object-fit contain).
    let box_h = placement.display_rows as f64 * grid.char_h;
    let terminal_w = grid.cols as f64 * grid.char_w;
    let avail_w = (grid.cols - placement.col) as f64 * grid.char_w;

    let box_w = match placement.image.width {
        Dim::Cells(c) => c * grid.char_w,
        Dim::Px(p) => p,
        Dim::Percent(pc) => pc / 100.0 * terminal_w,
        Dim::Auto => avail_w,
    }
    .min(avail_w);

    if box_w < 1.0 || box_h < 1.0 {
        return;
    }

    let scale = (box_w / iw).min(box_h / ih);
    let draw_w = iw * scale;
    let draw_h = ih * scale;

    if draw_w < 1.0 || draw_h < 1.0 {
        return;
    }

    let x0 = grid.margin_l + placement.col as f64 * grid.char_w;
    let y0 = grid.margin_t + placement.row as f64 * grid.char_h;

    let x_start = x0.floor().max(0.0) as usize;
    let x_end = ((x0 + draw_w).ceil() as usize).min(grid.pixel_width);
    let y_start = y0.floor().max(0.0) as usize;
    let y_end = ((y0 + draw_h).ceil() as usize).min(grid.pixel_height);

    for y in y_start..y_end {
        // Sample at the pixel center, mapped into source space.
        let v = ((y as f64 + 0.5 - y0) / draw_h) * image.height as f64 - 0.5;

        for x in x_start..x_end {
            let u = ((x as f64 + 0.5 - x0) / draw_w) * image.width as f64 - 0.5;
            let src = sample_bilinear(image, u, v);
            let a = src.a as u32;

            if a == 0 {
                continue;
            }

            let idx = y * grid.pixel_width + x;
            let dst = buf[idx];

            buf[idx] = RGBA8::new(
                over(src.r, dst.r, a),
                over(src.g, dst.g, a),
                over(src.b, dst.b, a),
                255,
            );
        }
    }
}

/// Source-over blend of one channel: `src * a + dst * (1 - a)`, alpha in 0..=255.
fn over(src: u8, dst: u8, a: u32) -> u8 {
    ((src as u32 * a + dst as u32 * (255 - a) + 127) / 255) as u8
}

fn sample_bilinear(image: &DecodedImage, u: f64, v: f64) -> RGBA8 {
    let max_x = image.width - 1;
    let max_y = image.height - 1;

    let ux = u.clamp(0.0, max_x as f64);
    let vy = v.clamp(0.0, max_y as f64);

    let x0 = ux.floor() as usize;
    let y0 = vy.floor() as usize;
    let x1 = (x0 + 1).min(max_x);
    let y1 = (y0 + 1).min(max_y);
    let fx = ux - x0 as f64;
    let fy = vy - y0 as f64;

    let p = |x: usize, y: usize| image.pixels[y * image.width + x];
    let (p00, p10, p01, p11) = (p(x0, y0), p(x1, y0), p(x0, y1), p(x1, y1));

    let lerp = |a: u8, b: u8, f: f64| a as f64 * (1.0 - f) + b as f64 * f;
    let bilerp = |c: fn(RGBA8) -> u8| {
        let top = lerp(c(p00), c(p10), fx);
        let bottom = lerp(c(p01), c(p11), fx);
        (top * (1.0 - fy) + bottom * fy).round() as u8
    };

    RGBA8::new(
        bilerp(|p| p.r),
        bilerp(|p| p.g),
        bilerp(|p| p.b),
        bilerp(|p| p.a),
    )
}

fn decode(image: &Image) -> Option<Vec<DecodedImage>> {
    // Animated GIF/APNG decode to all their frames; everything else is one.
    if image.animation.is_some() {
        return decode_animated(&image.data, image.mime);
    }

    let single = match image.mime {
        Mime::Png | Mime::Jpeg | Mime::Gif | Mime::Webp | Mime::Bmp => decode_raster(&image.data),
        Mime::Svg => decode_svg(&image.data),
        Mime::Pdf => decode_pdf(&image.data),
        Mime::Rgb => decode_raw(image, 3),
        Mime::Rgba => decode_raw(image, 4),
        Mime::Unknown => None,
    }?;

    Some(vec![single])
}

/// Decode every frame of an animated GIF/APNG into full-canvas RGBA (the `image`
/// crate coalesces frame disposal/blending for us).
fn decode_animated(data: &[u8], mime: Mime) -> Option<Vec<DecodedImage>> {
    let frames = match mime {
        Mime::Gif => image::codecs::gif::GifDecoder::new(Cursor::new(data))
            .ok()?
            .into_frames()
            .collect_frames()
            .ok()?,
        Mime::Png => image::codecs::png::PngDecoder::new(Cursor::new(data))
            .ok()?
            .apng()
            .ok()?
            .into_frames()
            .collect_frames()
            .ok()?,
        _ => return None,
    };

    let decoded: Vec<DecodedImage> = frames
        .into_iter()
        .map(|frame| {
            let buffer = frame.into_buffer();
            let (w, h) = buffer.dimensions();
            DecodedImage {
                width: w as usize,
                height: h as usize,
                pixels: buffer
                    .pixels()
                    .map(|p| RGBA8::new(p[0], p[1], p[2], p[3]))
                    .collect(),
            }
        })
        .collect();

    (!decoded.is_empty()).then_some(decoded)
}

/// Decode kitty raw pixel data (`f=24`/`f=32`) using the sender-declared
/// dimensions carried in [`Image::natural`].
fn decode_raw(image: &Image, channels: usize) -> Option<DecodedImage> {
    let (w, h) = image.natural?;
    let (w, h) = (w as usize, h as usize);
    let needed = w.checked_mul(h)?.checked_mul(channels)?;

    if image.data.len() < needed {
        return None;
    }

    let pixels = (0..w * h)
        .map(|i| {
            let o = i * channels;
            let d = &image.data;
            if channels == 4 {
                RGBA8::new(d[o], d[o + 1], d[o + 2], d[o + 3])
            } else {
                RGBA8::new(d[o], d[o + 1], d[o + 2], 255)
            }
        })
        .collect();

    Some(DecodedImage {
        width: w,
        height: h,
        pixels,
    })
}

fn decode_raster(data: &[u8]) -> Option<DecodedImage> {
    let rgba = image::load_from_memory(data).ok()?.to_rgba8();
    let (w, h) = rgba.dimensions();

    Some(DecodedImage {
        width: w as usize,
        height: h as usize,
        pixels: rgba
            .pixels()
            .map(|p| RGBA8::new(p[0], p[1], p[2], p[3]))
            .collect(),
    })
}

fn decode_svg(data: &[u8]) -> Option<DecodedImage> {
    let options = usvg::Options::default();
    let tree = usvg::Tree::from_data(data, &options).ok()?;
    let size = tree.size();
    let w = (size.width().ceil() as u32).max(1);
    let h = (size.height().ceil() as u32).max(1);

    let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());

    Some(pixmap_to_decoded(&pixmap))
}

fn pixmap_to_decoded(pixmap: &tiny_skia::Pixmap) -> DecodedImage {
    let pixels = pixmap
        .pixels()
        .iter()
        .map(|p| {
            // tiny-skia pixels are premultiplied; convert back to straight alpha.
            let a = p.alpha();
            let demul = |c: u8| {
                if a == 0 {
                    0
                } else {
                    ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8
                }
            };
            RGBA8::new(demul(p.red()), demul(p.green()), demul(p.blue()), a)
        })
        .collect();

    DecodedImage {
        width: pixmap.width() as usize,
        height: pixmap.height() as usize,
        pixels,
    }
}

static PDF_WARNED: AtomicBool = AtomicBool::new(false);

/// Best-effort PDF rendering: shell out to an available PDF rasterizer to render
/// the first page, then decode the resulting PNG. If none is available the image
/// is skipped and a warning is logged once.
fn decode_pdf(data: &[u8]) -> Option<DecodedImage> {
    match pdf_to_png(data) {
        Some(png) => decode_raster(&png),
        None => {
            if !PDF_WARNED.swap(true, Ordering::Relaxed) {
                log::warn!(
                    "skipping inline PDF image(s): install `pdftoppm` (poppler), `gs` (ghostscript), or `mutool` (mupdf) to render them (macOS `sips` is used automatically)"
                );
            }
            None
        }
    }
}

fn pdf_to_png(data: &[u8]) -> Option<Vec<u8>> {
    let dir = std::env::temp_dir();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let base = dir.join(format!("agg-pdf-{}-{stamp}", std::process::id()));
    let in_path = base.with_extension("pdf");
    let out_path = base.with_extension("png");

    std::fs::write(&in_path, data).ok()?;

    let in_str = in_path.to_string_lossy().into_owned();
    let out_str = out_path.to_string_lossy().into_owned();

    // pdftoppm writes "<prefix>.png" with -singlefile; give it the prefix
    // (which equals out_path without the extension).
    let out_prefix = base.to_string_lossy().into_owned();

    // Ordered fallbacks, first available wins. pdftoppm / gs / mutool rasterize
    // at 150 DPI; macOS's built-in `sips` renders at the PDF's native size, so
    // it goes last as a zero-install, lower-resolution safety net.
    let attempts: [(&str, Vec<&str>); 4] = [
        (
            "pdftoppm",
            vec!["-png", "-singlefile", "-r", "150", &in_str, &out_prefix],
        ),
        (
            "gs",
            vec![
                "-q",
                "-dNOPAUSE",
                "-dBATCH",
                "-sDEVICE=png16m",
                "-dFirstPage=1",
                "-dLastPage=1",
                "-r150",
                "-o",
                &out_str,
                &in_str,
            ],
        ),
        ("mutool", vec!["draw", "-r", "150", "-o", &out_str, &in_str, "1"]),
        ("sips", vec!["-s", "format", "png", &in_str, "--out", &out_str]),
    ];

    let png = attempts.iter().find_map(|(program, args)| {
        // Avoid reading a stale output from an earlier failed attempt.
        let _ = std::fs::remove_file(&out_path);
        run_converter(program, args)?;
        std::fs::read(&out_path).ok()
    });

    let _ = std::fs::remove_file(&in_path);
    let _ = std::fs::remove_file(&out_path);

    png
}

fn run_converter(program: &str, args: &[&str]) -> Option<()> {
    let status = Command::new(program).args(args).status().ok()?;
    status.success().then_some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::{AnimationInfo, Dim};

    fn animated_gif(colors: &[[u8; 3]]) -> Vec<u8> {
        use image::codecs::gif::{GifEncoder, Repeat};
        use image::{Delay, Frame, RgbaImage};

        let mut out = Vec::new();
        {
            let mut encoder = GifEncoder::new(Cursor::new(&mut out));
            encoder.set_repeat(Repeat::Infinite).unwrap();
            for c in colors {
                let buf = RgbaImage::from_pixel(4, 4, image::Rgba([c[0], c[1], c[2], 255]));
                let frame = Frame::from_parts(buf, 0, 0, Delay::from_numer_denom_ms(80, 1));
                encoder.encode_frame(frame).unwrap();
            }
        }
        out
    }

    #[test]
    fn decodes_every_frame_of_an_animated_gif() {
        let data = animated_gif(&[[255, 0, 0], [0, 255, 0], [0, 0, 255]]);
        let image = Image {
            id: 1,
            data,
            mime: Mime::Gif,
            natural: Some((4, 4)),
            width: Dim::Auto,
            height: Dim::Auto,
            preserve_aspect: true,
            animation: Some(AnimationInfo {
                delays: vec![0.08, 0.08, 0.08],
                total: 0.24,
            }),
        };

        let frames = decode(&image).expect("animated gif should decode");
        assert_eq!(frames.len(), 3);
        // First frame is solid red.
        assert_eq!(frames[0].pixels[0], RGBA8::new(255, 0, 0, 255));
        assert_eq!(frames[2].pixels[0], RGBA8::new(0, 0, 255, 255));
    }
}
