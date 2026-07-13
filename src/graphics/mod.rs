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

mod format;
mod layout;
mod osc1337;
mod store;

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

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

/// Image container format, detected from the payload's magic bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mime {
    Png,
    Jpeg,
    Gif,
    Webp,
    Bmp,
    Svg,
    Pdf,
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
}

impl PartialEq for Placement {
    fn eq(&self, other: &Self) -> bool {
        self.image.id == other.image.id
            && self.col == other.col
            && self.row == other.row
            && self.display_rows == other.display_rows
    }
}

/// One piece of parsed terminal output: either text to feed to the VT, or a
/// completed image to place at the current cursor.
pub enum Segment {
    Text(String),
    Image(Image),
}
