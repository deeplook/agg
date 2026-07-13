//! End-to-end tests for inline image rendering.
//!
//! Renders casts embedding images (iTerm2 OSC 1337 and the kitty graphics
//! protocol) through the public `run` API and confirms images are composited
//! onto the GIF, on both backends, by diffing the output against a run with
//! inline images disabled.

use std::io::Cursor;

use image::{AnimationDecoder, RgbaImage};

const ITERM2_CAST: &[u8] = include_bytes!("assets/images-iterm2.cast");
const KITTY_CAST: &[u8] = include_bytes!("assets/images-kitty.cast");

fn render(cast: &[u8], renderer: agg::Renderer, inline_images: bool) -> Vec<u8> {
    let config = agg::Config {
        renderer,
        inline_images,
        // Keep the test quiet and deterministic.
        show_progress_bar: false,
        ..Default::default()
    };

    let mut out = Vec::new();
    agg::run(Cursor::new(cast), &mut out, config).expect("run should succeed");
    out
}

/// The last frame of a GIF, as RGBA. Images in the cast are emitted at t=0, so
/// the final frame shows them all.
fn last_frame(gif: &[u8]) -> RgbaImage {
    let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(gif)).unwrap();
    let frames = decoder.into_frames().collect_frames().unwrap();
    frames.last().unwrap().buffer().clone()
}

fn count_differing_pixels(a: &RgbaImage, b: &RgbaImage) -> usize {
    assert_eq!(a.dimensions(), b.dimensions());
    a.pixels().zip(b.pixels()).filter(|(x, y)| x != y).count()
}

fn assert_composites_images(cast: &[u8], renderer: agg::Renderer) {
    let with = last_frame(&render(cast, renderer.clone(), true));
    let without = last_frame(&render(cast, renderer, false));

    // Compositing the inline images must change a substantial number of pixels
    // relative to the text-only render.
    let diff = count_differing_pixels(&with, &without);
    assert!(
        diff > 500,
        "expected inline images to change many pixels, got {diff}"
    );
}

#[test]
fn swash_composites_iterm2_images() {
    assert_composites_images(ITERM2_CAST, agg::Renderer::Swash);
}

#[test]
fn resvg_composites_iterm2_images() {
    assert_composites_images(ITERM2_CAST, agg::Renderer::Resvg);
}

#[test]
fn swash_composites_kitty_images() {
    assert_composites_images(KITTY_CAST, agg::Renderer::Swash);
}

#[test]
fn resvg_composites_kitty_images() {
    assert_composites_images(KITTY_CAST, agg::Renderer::Resvg);
}

// --- Animated GIF playback -------------------------------------------------

/// A 3-frame animated GIF (red, green, blue), 200ms per frame.
fn animated_gif() -> Vec<u8> {
    use image::codecs::gif::{GifEncoder, Repeat};
    use image::{Delay, Frame};

    let mut out = Vec::new();
    {
        let mut encoder = GifEncoder::new(Cursor::new(&mut out));
        encoder.set_repeat(Repeat::Infinite).unwrap();
        for c in [[230u8, 40, 40], [40, 200, 80], [60, 110, 230]] {
            let buf = RgbaImage::from_pixel(8, 8, image::Rgba([c[0], c[1], c[2], 255]));
            let frame = Frame::from_parts(buf, 0, 0, Delay::from_numer_denom_ms(200, 1));
            encoder.encode_frame(frame).unwrap();
        }
    }
    out
}

/// A cast that displays the animated GIF at t=0, then sits idle until t=3 —
/// leaving a gap the animation should play through.
fn animated_gif_cast() -> Vec<u8> {
    use base64::Engine;

    let b64 = base64::engine::general_purpose::STANDARD.encode(animated_gif());
    let seq = format!("\x1b]1337;File=inline=1;height=3:{b64}\x07");

    let mut cast = String::from("{\"version\":2,\"width\":40,\"height\":20}\n");
    for event in [
        serde_json::to_string(&(0.0f64, "o", "anim:\r\n")).unwrap(),
        serde_json::to_string(&(0.0f64, "o", seq)).unwrap(),
        serde_json::to_string(&(3.0f64, "o", "done\r\n")).unwrap(),
    ] {
        cast.push_str(&event);
        cast.push('\n');
    }
    cast.into_bytes()
}

fn all_frames(gif: &[u8]) -> Vec<RgbaImage> {
    let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(gif)).unwrap();
    decoder
        .into_frames()
        .collect_frames()
        .unwrap()
        .into_iter()
        .map(|f| f.into_buffer())
        .collect()
}

/// Coarse average color of the image region, bucketed so minor dithering noise
/// doesn't register as a change.
fn region_color(img: &RgbaImage) -> (u8, u8, u8) {
    let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
    for y in 36..84 {
        for x in 14..60 {
            let p = img.get_pixel(x, y);
            r += p[0] as u64;
            g += p[1] as u64;
            b += p[2] as u64;
            n += 1;
        }
    }
    let bucket = |v: u64| ((v / n / 48) * 48) as u8;
    (bucket(r), bucket(g), bucket(b))
}

fn assert_animation_plays(renderer: agg::Renderer) {
    use std::collections::HashSet;

    let frames = all_frames(&render(&animated_gif_cast(), renderer, true));
    assert!(frames.len() > 1, "expected multiple output frames");

    let colors: HashSet<(u8, u8, u8)> = frames.iter().map(region_color).collect();
    assert!(
        colors.len() >= 2,
        "expected the image region to change color across frames, saw {colors:?}"
    );
}

#[test]
fn swash_plays_animated_gif() {
    assert_animation_plays(agg::Renderer::Swash);
}

#[test]
fn resvg_plays_animated_gif() {
    assert_animation_plays(agg::Renderer::Resvg);
}
