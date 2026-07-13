use avt::Vt;

use crate::graphics::{image_rows, ImageStore, Osc1337Parser, Placement, Segment};

pub fn build(terminal_size: (usize, usize)) -> Vt {
    Vt::builder()
        .size(terminal_size.0, terminal_size.1)
        .scrollback_limit(0)
        .build()
}

/// Pixel metrics of one terminal cell, needed to decide how many grid rows an
/// inline image spans. Present only when inline images are enabled.
#[derive(Clone, Copy)]
pub struct ImageConfig {
    pub char_w: f64,
    pub char_h: f64,
}

/// A terminal emulator plus, when inline images are enabled, the image parser
/// and the set of active image placements. Owns the split-feed logic that
/// interleaves text with image placement during a replay pass.
pub struct TerminalState {
    vt: Vt,
    cols: usize,
    images: Option<ImageState>,
}

struct ImageState {
    parser: Osc1337Parser,
    store: ImageStore,
    config: ImageConfig,
}

impl TerminalState {
    pub fn new(terminal_size: (usize, usize), image_config: Option<ImageConfig>) -> Self {
        TerminalState {
            vt: build(terminal_size),
            cols: terminal_size.0,
            images: image_config.map(|config| ImageState {
                parser: Osc1337Parser::new(),
                store: ImageStore::new(),
                config,
            }),
        }
    }

    /// Feed a chunk of recorded output. Without image support this is a plain
    /// VT feed; with it, output is split at image boundaries so each image is
    /// anchored to the cursor position reached by the preceding text, and image
    /// placements are scrolled to track the terminal (see the player's
    /// `core.js` split-feed).
    pub fn feed_str(&mut self, data: &str) {
        let Some(images) = &mut self.images else {
            self.vt.feed_str(data);
            return;
        };

        // Terminal reset clears the screen and all tracked images.
        if data.contains('\u{1b}') && data.contains("\u{1b}c") {
            images.store.clear();
            images.parser.reset();
        }

        for segment in images.parser.parse(data) {
            match segment {
                Segment::Text(text) => {
                    let scrolled = self.vt.feed_str(&text).scrollback.count();
                    if scrolled > 0 {
                        images.store.scroll(scrolled);
                    }
                }

                Segment::Image(image) => {
                    let cursor = self.vt.cursor();
                    let (col, row) = (cursor.col, cursor.row);

                    let display_rows =
                        image_rows(&image, self.cols, images.config.char_w, images.config.char_h);

                    // Reserve vertical space by advancing the cursor, which may
                    // scroll the viewport; existing images scroll with it.
                    let newlines = "\n".repeat(display_rows);
                    let scrolled = self.vt.feed_str(&newlines).scrollback.count();
                    if scrolled > 0 {
                        images.store.scroll(scrolled);
                    }

                    let adjusted_row = row as isize - scrolled as isize;
                    images
                        .store
                        .add(std::rc::Rc::new(image), col, adjusted_row, display_rows);
                }
            }
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            lines: self.vt.view().cloned().collect(),
            cursor: self.vt.cursor().into(),
            images: self
                .images
                .as_ref()
                .map(|i| i.store.snapshot())
                .unwrap_or_default(),
        }
    }
}

#[derive(Clone)]
pub struct Snapshot {
    pub lines: Vec<avt::Line>,
    pub cursor: Option<(usize, usize)>,
    pub images: Vec<Placement>,
}

impl Snapshot {
    pub fn same_visual(&self, other: &Snapshot) -> bool {
        self.lines == other.lines && self.cursor == other.cursor && self.images == other.images
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::*;
    use crate::graphics::{Dim, Image, Mime, Placement};

    fn placement(id: u64) -> Placement {
        Placement {
            image: Rc::new(Image {
                id,
                data: Vec::new(),
                mime: Mime::Png,
                natural: None,
                width: Dim::Auto,
                height: Dim::Auto,
                preserve_aspect: true,
            }),
            col: 0,
            row: 0,
            display_rows: 1,
        }
    }

    fn snapshot(images: Vec<Placement>) -> Snapshot {
        Snapshot {
            lines: Vec::new(),
            cursor: Some((0, 0)),
            images,
        }
    }

    #[test]
    fn same_visual_distinguishes_image_only_changes() {
        let base = snapshot(vec![]);
        let with_image = snapshot(vec![placement(1)]);

        assert!(base.same_visual(&snapshot(vec![])));
        assert!(!base.same_visual(&with_image));
        assert!(with_image.same_visual(&snapshot(vec![placement(1)])));
    }
}
