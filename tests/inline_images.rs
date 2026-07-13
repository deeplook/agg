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
