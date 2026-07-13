//! Active inline-image placements and their lifecycle across scrolling.
//!
//! Ported from asciinema-player's `src/image/store.js` (position tracking) plus
//! the scroll-adjustment logic in `_scrollImages` in `core.js`. Placements are
//! anchored to grid rows; as terminal content scrolls up, their rows decrease,
//! and a placement scrolled entirely above the viewport is dropped.

use std::rc::Rc;

use super::{Image, Placement};

#[derive(Default)]
pub struct ImageStore {
    placements: Vec<Placement>,
}

impl ImageStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a new placement at the given grid position. `start_time` is the
    /// recording time the image was emitted, anchoring animation playback.
    pub fn add(
        &mut self,
        image: Rc<Image>,
        col: usize,
        row: isize,
        display_rows: usize,
        start_time: f64,
    ) {
        self.placements.push(Placement {
            image,
            col,
            row,
            display_rows,
            start_time,
            anim_frame: 0,
        });
    }

    /// Shift every placement up by `n` rows (content scrolled up by `n`),
    /// dropping any placement now entirely above the top of the viewport.
    pub fn scroll(&mut self, n: usize) {
        let n = n as isize;

        for p in &mut self.placements {
            p.row -= n;
        }

        self.placements
            .retain(|p| p.row + p.display_rows as isize > 0);
    }

    /// Drop all placements (terminal reset / full clear).
    pub fn clear(&mut self) {
        self.placements.clear();
    }

    /// A cheap clone of the currently-active placements for a frame snapshot.
    pub fn snapshot(&self) -> Vec<Placement> {
        self.placements.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::{Dim, Mime};

    fn image() -> Rc<Image> {
        Rc::new(Image {
            id: Image::next_id(),
            data: Vec::new(),
            mime: Mime::Png,
            natural: None,
            width: Dim::Auto,
            height: Dim::Auto,
            preserve_aspect: true,
            animation: None,
        })
    }

    #[test]
    fn scroll_decrements_rows() {
        let mut store = ImageStore::new();
        store.add(image(), 0, 5, 3, 0.0);
        store.scroll(2);
        assert_eq!(store.snapshot()[0].row, 3);
    }

    #[test]
    fn scroll_drops_placements_fully_above_viewport() {
        let mut store = ImageStore::new();
        store.add(image(), 0, 1, 2, 0.0); // spans rows 1..3
        store.scroll(3); // row -> -2, bottom edge -2+2 = 0, not > 0
        assert!(store.snapshot().is_empty());
    }

    #[test]
    fn scroll_keeps_partially_visible_placements() {
        let mut store = ImageStore::new();
        store.add(image(), 0, 1, 3, 0.0); // spans rows 1..4
        store.scroll(2); // row -> -1, bottom edge -1+3 = 2 > 0
        assert_eq!(store.snapshot()[0].row, -1);
    }

    #[test]
    fn clear_removes_everything() {
        let mut store = ImageStore::new();
        store.add(image(), 0, 0, 1, 0.0);
        store.clear();
        assert!(store.snapshot().is_empty());
    }
}
