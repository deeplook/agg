//! How many terminal rows an inline image occupies.
//!
//! Ported from `calculateImageRows` in asciinema-player's
//! `src/image/interceptor.js`. The priority ladder (explicit cells → explicit
//! px → aspect from natural dims → fallbacks) is kept identical so agg and the
//! player reserve the same amount of vertical space for the same image.

use super::{Dim, Image};

/// Compute the number of grid rows `image` should span, given the terminal
/// width in columns and the pixel size of one cell.
pub fn image_rows(image: &Image, cols: usize, char_w: f64, char_h: f64) -> usize {
    // Priority 1: explicit height in cells.
    if let Dim::Cells(v) = image.height {
        return (v.ceil() as usize).max(1);
    }

    let natural_w = image.natural.map(|(w, _)| w as f64);
    let natural_h = image.natural.map(|(_, h)| h as f64);
    let terminal_w = cols as f64 * char_w;

    let display_w = match image.width {
        Dim::Px(v) => v,
        Dim::Cells(v) => v * char_w,
        Dim::Percent(v) => (v / 100.0) * terminal_w,
        Dim::Auto => match natural_w {
            Some(w) => w.min(terminal_w),
            None => terminal_w,
        },
    };

    let display_h = match image.height {
        Dim::Px(v) => v,
        Dim::Percent(_) => 200.0,
        // Cells was handled above; Auto falls through to aspect/natural.
        Dim::Cells(_) | Dim::Auto => match (natural_w, natural_h) {
            (Some(w), Some(h)) if image.preserve_aspect && w > 0.0 => display_w * (h / w),
            _ => match natural_h {
                Some(h) => h,
                None => display_w * 0.75,
            },
        },
    };

    ((display_h / char_h).ceil() as usize).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::Mime;

    fn image(width: Dim, height: Dim, natural: Option<(u32, u32)>) -> Image {
        Image {
            id: 0,
            data: Vec::new(),
            mime: Mime::Png,
            natural,
            width,
            height,
            preserve_aspect: true,
        }
    }

    #[test]
    fn explicit_height_in_cells_wins() {
        let img = image(Dim::Auto, Dim::Cells(3.2), Some((100, 100)));
        assert_eq!(image_rows(&img, 80, 9.0, 20.0), 4);
    }

    #[test]
    fn aspect_ratio_from_natural_dims() {
        // Square image, auto width clamps to natural 40px wide → 40px tall → 2 rows.
        let img = image(Dim::Auto, Dim::Auto, Some((40, 40)));
        assert_eq!(image_rows(&img, 80, 9.0, 20.0), 2);
    }

    #[test]
    fn explicit_pixel_height() {
        let img = image(Dim::Auto, Dim::Px(45.0), None);
        assert_eq!(image_rows(&img, 80, 9.0, 20.0), 3);
    }

    #[test]
    fn fallback_when_no_dims() {
        // No natural size: display width = full terminal (80*9=720), height =
        // 720*0.75 = 540 → ceil(540/20) = 27 rows.
        let img = image(Dim::Auto, Dim::Auto, None);
        assert_eq!(image_rows(&img, 80, 9.0, 20.0), 27);
    }

    #[test]
    fn always_at_least_one_row() {
        let img = image(Dim::Px(1.0), Dim::Px(1.0), None);
        assert_eq!(image_rows(&img, 80, 9.0, 20.0), 1);
    }
}
