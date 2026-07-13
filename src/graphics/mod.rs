//! Inline terminal graphics (iTerm2 OSC 1337, kitty-ready).
//!
//! Recordings can embed images via terminal escape sequences. `avt`, agg's VT
//! emulator, ignores those sequences, so images are parsed out of the output
//! stream here, tracked against the terminal grid as it scrolls, and composited
//! over the rendered text (see [`crate::renderer::image_layer`]).
//!
//! The parser layer is protocol-agnostic: each protocol turns raw output into a
//! stream of [`Segment`]s. Only iTerm2 OSC 1337 is implemented today
//! ([`osc1337`]); kitty is a planned addition wired in through the same
//! `Segment` interface.

mod animation;
mod format;
mod kitty;
mod layout;
mod osc1337;
mod store;

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

pub use kitty::KittyParser;
pub use layout::image_rows;
pub use osc1337::Osc1337Parser;
pub use store::ImageStore;

/// An iTerm2 dimension spec (`width=`/`height=` parameter value).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dim {
    Cells(f64),
    Px(f64),
    Percent(f64),
    Auto,
}

/// Image container format, detected from the payload's magic bytes, or (for the
/// kitty protocol's raw transmission formats) declared by the sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mime {
    Png,
    Jpeg,
    Gif,
    Webp,
    Bmp,
    Svg,
    Pdf,
    /// Raw 24-bit RGB pixels (kitty `f=24`); dimensions come from [`Image::natural`].
    Rgb,
    /// Raw 32-bit RGBA pixels (kitty `f=32`); dimensions come from [`Image::natural`].
    Rgba,
    Unknown,
}

/// A decoded inline image: the raw file bytes plus the sizing hints needed to
/// place and scale it. Wrapped in an [`Rc`] once stored so cloning a
/// [`Placement`] into every frame's snapshot is cheap.
#[derive(Debug)]
pub struct Image {
    /// Process-unique id, used as the render decode-cache key and for cheap
    /// equality in [`Placement`].
    pub id: u64,
    /// Decoded (base64-decoded) file bytes.
    pub data: Vec<u8>,
    pub mime: Mime,
    /// Natural pixel dimensions read from the file header, when available.
    pub natural: Option<(u32, u32)>,
    pub width: Dim,
    pub height: Dim,
    pub preserve_aspect: bool,
    /// Per-frame timing for animated GIF/APNG; `None` for static images.
    pub animation: Option<AnimationInfo>,
}

/// Frame timing for an animated image, used to pick which frame to show at a
/// given elapsed time.
#[derive(Debug, Clone)]
pub struct AnimationInfo {
    /// Seconds each frame is shown, in order.
    pub delays: Vec<f64>,
    /// Sum of `delays` (one loop's duration).
    pub total: f64,
}

impl AnimationInfo {
    /// Frame index to display `elapsed` seconds after the animation started,
    /// looping indefinitely.
    pub fn frame_at(&self, elapsed: f64) -> usize {
        if self.total <= 0.0 || self.delays.len() <= 1 {
            return 0;
        }

        let mut e = elapsed.rem_euclid(self.total);
        for (i, delay) in self.delays.iter().enumerate() {
            e -= delay;
            if e < 0.0 {
                return i;
            }
        }

        self.delays.len() - 1
    }
}

static NEXT_IMAGE_ID: AtomicU64 = AtomicU64::new(1);

impl Image {
    fn next_id() -> u64 {
        NEXT_IMAGE_ID.fetch_add(1, Ordering::Relaxed)
    }
}

/// An image anchored to the terminal grid. `row` is signed so a placement that
/// has partially scrolled above the top of the viewport (negative `row`) stays
/// representable and can be top-clipped when rendered.
#[derive(Clone)]
pub struct Placement {
    pub image: Rc<Image>,
    pub col: usize,
    pub row: isize,
    pub display_rows: usize,
    /// Recording time (adjusted timeline) at which the image was placed; the
    /// anchor for animation playback.
    pub start_time: f64,
    /// Which animation frame to display; resolved during frame post-processing
    /// (see [`crate::output::expand_animation`]). Always 0 for static images.
    pub anim_frame: usize,
}

impl PartialEq for Placement {
    // Intentionally excludes `start_time`/`anim_frame`: visual dedupe of
    // terminal content runs before animation frames are resolved, and must
    // still collapse otherwise-identical placements.
    fn eq(&self, other: &Self) -> bool {
        self.image.id == other.image.id
            && self.col == other.col
            && self.row == other.row
            && self.display_rows == other.display_rows
    }
}

/// One piece of parsed terminal output.
pub enum Segment {
    /// Text to feed to the VT.
    Text(String),
    /// A completed image to place at the current cursor.
    Image(Image),
    /// Drop all active image placements (kitty delete-all, `a=d`).
    ClearImages,
}
