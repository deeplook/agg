//! Frame-timing extraction for animated GIF and APNG images.
//!
//! Reads per-frame delays so [`crate::output::expand_animation`] can pick which
//! embedded frame to show over time. Pixel data is decoded separately, by the
//! renderer's cache; this only needs the timings. Both use the `image` crate,
//! so frame ordering matches.

use std::io::Cursor;

use image::AnimationDecoder;

use super::{AnimationInfo, Mime};

/// Very short/zero frame delays are clamped up, matching how browsers treat
/// 0-delay GIF frames, so an animation can't demand an absurd frame rate.
const MIN_DELAY: f64 = 0.02;
const CLAMPED_DELAY: f64 = 0.1;

/// Extract animation timing for a multi-frame GIF/APNG, or `None` for static
/// images and other formats.
pub fn parse(data: &[u8], mime: Mime) -> Option<AnimationInfo> {
    let delays = match mime {
        Mime::Gif => {
            let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(data)).ok()?;
            frame_delays(decoder.into_frames())
        }
        Mime::Png => {
            let decoder = image::codecs::png::PngDecoder::new(Cursor::new(data)).ok()?;
            if !decoder.is_apng().ok()? {
                return None;
            }
            frame_delays(decoder.apng().ok()?.into_frames())
        }
        _ => return None,
    }?;

    if delays.len() <= 1 {
        return None;
    }

    let total = delays.iter().sum();
    Some(AnimationInfo { delays, total })
}

fn frame_delays(frames: image::Frames<'_>) -> Option<Vec<f64>> {
    let frames = frames.collect_frames().ok()?;

    Some(
        frames
            .iter()
            .map(|frame| {
                let (numer, denom) = frame.delay().numer_denom_ms();
                let seconds = if denom == 0 {
                    0.0
                } else {
                    numer as f64 / denom as f64 / 1000.0
                };

                if seconds < MIN_DELAY {
                    CLAMPED_DELAY
                } else {
                    seconds
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Encode an animated GIF with `frames` solid-color frames, each `delay_ms`.
    fn animated_gif(colors: &[[u8; 3]], delay_ms: u16) -> Vec<u8> {
        use image::codecs::gif::{GifEncoder, Repeat};
        use image::{Delay, Frame, RgbaImage};

        let mut out = Vec::new();
        {
            let mut encoder = GifEncoder::new(Cursor::new(&mut out));
            encoder.set_repeat(Repeat::Infinite).unwrap();
            for color in colors {
                let buf =
                    RgbaImage::from_pixel(4, 4, image::Rgba([color[0], color[1], color[2], 255]));
                let frame =
                    Frame::from_parts(buf, 0, 0, Delay::from_numer_denom_ms(delay_ms as u32, 1));
                encoder.encode_frame(frame).unwrap();
            }
        }
        out
    }

    #[test]
    fn parses_multi_frame_gif_delays() {
        let gif = animated_gif(&[[255, 0, 0], [0, 255, 0], [0, 0, 255]], 80);
        let info = parse(&gif, Mime::Gif).expect("animated gif should parse");

        assert_eq!(info.delays.len(), 3);
        for d in &info.delays {
            assert!((d - 0.08).abs() < 1e-6, "expected 80ms delay, got {d}");
        }
        assert!((info.total - 0.24).abs() < 1e-6);
    }

    #[test]
    fn clamps_zero_delay_frames() {
        let gif = animated_gif(&[[255, 0, 0], [0, 255, 0]], 0);
        let info = parse(&gif, Mime::Gif).unwrap();

        for d in &info.delays {
            assert_eq!(*d, CLAMPED_DELAY);
        }
    }

    #[test]
    fn single_frame_gif_is_not_animated() {
        let gif = animated_gif(&[[255, 0, 0]], 80);
        assert!(parse(&gif, Mime::Gif).is_none());
    }

    #[test]
    fn parses_apng_delays() {
        use base64::Engine;

        // A 2x2, 2-frame APNG (red, blue), 120ms per frame.
        const APNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAACGFjVEwAAAACAAAAAPONk3AAAAAaZmNUTAAAAAAAAAACAAAAAgAAAAAAAAAAAAMAGQAAjPSSSQAAABRJREFUeJxj/M/A8J+BgYGBiQEKAB8XAgJPlM6+AAAAGmZjVEwAAAABAAAAAgAAAAIAAAAAAAAAAAADABkAABeHeJ0AAAAYZmRBVAAAAAJ4nGNkYPj/n4GBgYGJAQoAHRkCAunm7jEAAAAASUVORK5CYII=";
        let data = base64::engine::general_purpose::STANDARD
            .decode(APNG_B64)
            .unwrap();

        let info = parse(&data, Mime::Png).expect("apng should parse");
        assert_eq!(info.delays.len(), 2);
        // 120ms frames, well above the clamp threshold, so parsed as-is.
        for d in &info.delays {
            assert!((0.11..0.13).contains(d), "expected ~0.12s, got {d}");
        }
    }

    #[test]
    fn static_png_is_not_animated() {
        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(2, 2)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        assert!(parse(&png.into_inner(), Mime::Png).is_none());
    }

    #[test]
    fn frame_at_loops_and_selects() {
        let info = AnimationInfo {
            delays: vec![0.1, 0.1, 0.1],
            total: 0.3,
        };

        assert_eq!(info.frame_at(0.0), 0);
        assert_eq!(info.frame_at(0.15), 1);
        assert_eq!(info.frame_at(0.25), 2);
        // Loops back around: 0.35 % 0.3 = 0.05 -> frame 0, 0.45 -> frame 1.
        assert_eq!(info.frame_at(0.35), 0);
        assert_eq!(info.frame_at(0.45), 1);
        assert_eq!(info.frame_at(0.55), 2);
    }
}
