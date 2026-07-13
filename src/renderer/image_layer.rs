//! Decoding and compositing of inline images over a rendered frame.
//!
//! Shared by both renderers: each decodes an [`Image`] once (cached by id) into
//! straight-alpha RGBA, then blits every active [`Placement`] onto the frame
//! buffer using cell geometry supplied by the caller. This mirrors
//! asciinema-player's overlay: image height spans `display_rows` cells, width
//! follows the natural aspect ratio (letterboxed to fit the space remaining to
//! the right edge), and placements scrolled above the top are clipped.

use std::collections::HashMap;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use rgb::RGBA8;

use crate::graphics::{Image, Mime, Placement};

/// A decoded image in straight-alpha RGBA8, ready to sample.
pub struct DecodedImage {
    width: usize,
    height: usize,
    pixels: Vec<RGBA8>,
}

/// Per-renderer cache of decoded images, keyed by [`Image::id`]. A failed decode
/// is cached as `None` so it is attempted only once.
#[derive(Default)]
pub struct DecodeCache {
    cache: HashMap<u64, Option<DecodedImage>>,
}

impl DecodeCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn get(&mut self, image: &Image) -> Option<&DecodedImage> {
        self.cache
            .entry(image.id)
            .or_insert_with(|| decode(image))
            .as_ref()
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
        if let Some(image) = cache.get(&placement.image) {
            draw(buf, grid, placement, image);
        }
    }
}

fn draw(buf: &mut [RGBA8], grid: &Grid, placement: &Placement, image: &DecodedImage) {
    if placement.col >= grid.cols || image.width == 0 || image.height == 0 {
        return;
    }

    let aspect = image.width as f64 / image.height as f64;

    // Height spans display_rows cells; width follows aspect, clamped to the
    // space remaining to the right edge (letterboxed, matching object-fit
    // contain).
    let box_h = placement.display_rows as f64 * grid.char_h;
    let avail_w = (grid.cols - placement.col) as f64 * grid.char_w;

    let mut draw_h = box_h;
    let mut draw_w = draw_h * aspect;
    if draw_w > avail_w {
        draw_w = avail_w;
        draw_h = draw_w / aspect;
    }

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

fn decode(image: &Image) -> Option<DecodedImage> {
    match image.mime {
        Mime::Png | Mime::Jpeg | Mime::Gif | Mime::Webp | Mime::Bmp => decode_raster(&image.data),
        Mime::Svg => decode_svg(&image.data),
        Mime::Pdf => decode_pdf(&image.data),
        Mime::Unknown => None,
    }
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

/// Best-effort PDF rendering: shell out to `pdftoppm` (poppler) or `mutool`
/// (mupdf) to rasterize the first page, then decode the resulting PNG. If no
/// converter is available the image is skipped and a warning is logged once.
fn decode_pdf(data: &[u8]) -> Option<DecodedImage> {
    match pdf_to_png(data) {
        Some(png) => decode_raster(&png),
        None => {
            if !PDF_WARNED.swap(true, Ordering::Relaxed) {
                log::warn!(
                    "skipping inline PDF image(s): install `pdftoppm` (poppler) or `mutool` (mupdf) to render them"
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

    // pdftoppm writes "<prefix>.png" with -singlefile; give it the prefix.
    let out_prefix = base.to_string_lossy().into_owned();

    let png = run_converter("pdftoppm", &["-png", "-singlefile", "-r", "150", &in_str, &out_prefix])
        .and_then(|()| std::fs::read(&out_path).ok())
        .or_else(|| {
            run_converter(
                "mutool",
                &["draw", "-r", "150", "-o", &out_str, &in_str, "1"],
            )
            .and_then(|()| std::fs::read(&out_path).ok())
        });

    let _ = std::fs::remove_file(&in_path);
    let _ = std::fs::remove_file(&out_path);

    png
}

fn run_converter(program: &str, args: &[&str]) -> Option<()> {
    let status = Command::new(program).args(args).status().ok()?;
    status.success().then_some(())
}
