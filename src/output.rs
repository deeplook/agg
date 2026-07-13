//! Output frame preparation.

use crate::frames::Frame;
use crate::terminal::Snapshot;

/// Drop frames whose terminal state matches the previously emitted frame. Kept
/// frames keep their original timestamps, so the delay to the next change is
/// preserved.
pub fn dedupe_visual_changes(frames: impl Iterator<Item = Frame>) -> impl Iterator<Item = Frame> {
    let mut frames = frames;
    let mut held: Option<Frame> = None;

    std::iter::from_fn(move || {
        for frame in frames.by_ref() {
            match &held {
                Some(h) if h.same_visual(&frame) => continue,
                Some(_) => return Some(held.replace(frame).unwrap()),
                None => held = Some(frame),
            }
        }

        held.take()
    })
}

/// Shift timestamps so the first selected frame starts at `0`, preserving the
/// spacing between later frames.
pub fn adjust_timeline_timestamps(
    frames: impl Iterator<Item = Frame>,
) -> impl Iterator<Item = Frame> {
    let mut offset = None;

    frames.map(move |mut f| {
        let offset = *offset.get_or_insert(f.time);
        f.time -= offset;

        f
    })
}

/// Assign sequential output timestamps using a fixed per-frame duration. This
/// preserves selection order rather than source-time spacing.
pub fn adjust_discrete_timestamps(
    frames: impl Iterator<Item = Frame>,
    frame_duration: f64,
) -> impl Iterator<Item = Frame> {
    frames.enumerate().map(move |(i, mut f)| {
        f.time = i as f64 * frame_duration;

        f
    })
}

/// Resolve each placement's `anim_frame` from a frame's time (used by the
/// discrete/positions path, which has no idle gaps to fill).
pub fn set_anim_frames(frames: impl Iterator<Item = Frame>) -> impl Iterator<Item = Frame> {
    frames.map(|mut f| {
        let time = f.time;
        resolve_anim_frames(&mut f.snapshot, time);
        f
    })
}

/// Make embedded animated GIF/APNG images play: resolve each placement's
/// `anim_frame`, and inject extra frames into the idle gaps between existing
/// frames so the animation actually advances there. Sampling is bounded by
/// `fps_cap`, and a frame is only inserted when the visible embedded frame
/// changes. No frames are appended after the last one (animation is confined to
/// gaps that already exist in the recording).
pub fn expand_animation(frames: Vec<Frame>, fps_cap: u8) -> Vec<Frame> {
    let has_animation = frames
        .iter()
        .any(|f| f.snapshot.images.iter().any(is_animated));

    if !has_animation {
        return frames;
    }

    let fps = if fps_cap == 0 { 30.0 } else { fps_cap as f64 };
    let step = 1.0 / fps;

    let mut out = Vec::with_capacity(frames.len());

    for i in 0..frames.len() {
        let mut frame = frames[i].clone();
        resolve_anim_frames(&mut frame.snapshot, frame.time);
        let base_time = frame.time;
        out.push(frame);

        let Some(next) = frames.get(i + 1) else {
            break; // no trailing playback past the last frame
        };

        if !frames[i].snapshot.images.iter().any(is_animated) {
            continue;
        }

        // Fill the gap: emit a frame whenever the visible embedded frame(s) change.
        let base = &frames[i].snapshot;
        let mut last_sig = anim_signature(base, base_time);
        let mut t = base_time + step;

        while t < next.time {
            let sig = anim_signature(base, t);
            if sig != last_sig {
                let mut f = frames[i].clone();
                f.time = t;
                resolve_anim_frames(&mut f.snapshot, t);
                out.push(f);
                last_sig = sig;
            }
            t += step;
        }
    }

    out
}

fn is_animated(placement: &crate::graphics::Placement) -> bool {
    placement.image.animation.is_some()
}

/// The embedded frame index of every animated placement at `time` — the
/// signature used to detect when the visible animation changes.
fn anim_signature(snapshot: &Snapshot, time: f64) -> Vec<usize> {
    snapshot
        .images
        .iter()
        .filter_map(|p| {
            p.image
                .animation
                .as_ref()
                .map(|a| a.frame_at(time - p.start_time))
        })
        .collect()
}

fn resolve_anim_frames(snapshot: &mut Snapshot, time: f64) {
    for placement in &mut snapshot.images {
        if let Some(animation) = &placement.image.animation {
            placement.anim_frame = animation.frame_at(time - placement.start_time);
        }
    }
}

/// Reduce frames to at most one per `1/fps_cap` interval. Each window keeps the
/// latest terminal state, timestamped at the window's start.
pub fn cap_fps(frames: impl Iterator<Item = Frame>, fps_cap: u8) -> impl Iterator<Item = Frame> {
    let max_frame_time = 1.0 / (fps_cap as f64);
    let mut frames = frames;
    let mut window: Option<Frame> = None;

    std::iter::from_fn(move || {
        for frame in frames.by_ref() {
            match &mut window {
                None => window = Some(frame),

                Some(w) if frame.time - w.time < max_frame_time => {
                    w.snapshot = frame.snapshot;
                }

                Some(_) => return Some(window.replace(frame).unwrap()),
            }
        }

        window.take()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::Snapshot;

    /// A frame tagged via its cursor, for time/order assertions where terminal
    /// content is irrelevant.
    fn tagged(time: f64, tag: usize) -> Frame {
        Frame {
            time,
            snapshot: Snapshot {
                lines: Vec::new(),
                cursor: Some((tag, 0)),
                images: Vec::new(),
            },
        }
    }

    fn times(frames: &[Frame]) -> Vec<f64> {
        frames.iter().map(|f| f.time).collect()
    }

    fn tags(frames: &[Frame]) -> Vec<usize> {
        frames
            .iter()
            .map(|f| f.snapshot.cursor.unwrap().0)
            .collect()
    }

    /// A frame holding one placement, optionally animated (delays 0.1s each).
    fn frame_with_image(time: f64, animated: bool) -> Frame {
        use crate::graphics::{AnimationInfo, Dim, Image, Mime, Placement};
        use std::rc::Rc;

        let animation = animated.then(|| AnimationInfo {
            delays: vec![0.1, 0.1, 0.1],
            total: 0.3,
        });

        let image = Rc::new(Image {
            id: 1,
            data: Vec::new(),
            mime: Mime::Gif,
            natural: Some((4, 4)),
            width: Dim::Auto,
            height: Dim::Auto,
            preserve_aspect: true,
            animation,
        });

        Frame {
            time,
            snapshot: Snapshot {
                lines: Vec::new(),
                cursor: None,
                images: vec![Placement {
                    image,
                    col: 0,
                    row: 0,
                    display_rows: 1,
                    start_time: 0.0,
                    anim_frame: 0,
                }],
            },
        }
    }

    fn anim_frames(frames: &[Frame]) -> Vec<usize> {
        frames
            .iter()
            .map(|f| f.snapshot.images[0].anim_frame)
            .collect()
    }

    #[test]
    fn expand_animation_injects_frames_into_idle_gap() {
        // An animated image at t=0, then a plain event at t=1 leaves a 1s gap.
        let frames = vec![frame_with_image(0.0, true), frame_with_image(1.0, true)];
        let out = expand_animation(frames, 30);

        // Frames were injected into the gap...
        assert!(out.len() > 2, "expected injected frames, got {}", out.len());
        // ...times stay sorted and within [0, 1]...
        assert!(out.windows(2).all(|w| w[0].time <= w[1].time));
        assert!(out.iter().all(|f| f.time <= 1.0));
        // ...and the visible animation frame advances through all three frames.
        let seen = anim_frames(&out);
        assert!(seen.contains(&0) && seen.contains(&1) && seen.contains(&2));
    }

    #[test]
    fn expand_animation_is_a_noop_for_static_images() {
        let frames = vec![frame_with_image(0.0, false), frame_with_image(1.0, false)];
        let out = expand_animation(frames, 30);

        assert_eq!(out.len(), 2);
        assert_eq!(anim_frames(&out), vec![0, 0]);
    }

    #[test]
    fn dedupe_keeps_first_of_each_visual_run() {
        let frames = vec![
            tagged(0.0, 0),
            tagged(1.0, 0),
            tagged(2.0, 1),
            tagged(3.0, 1),
        ];

        let frames: Vec<_> = dedupe_visual_changes(frames.into_iter()).collect();

        assert_eq!(times(&frames), vec![0.0, 2.0]);
        assert_eq!(tags(&frames), vec![0, 1]);
    }

    #[test]
    fn empty_input_yields_empty_output() {
        assert!(dedupe_visual_changes(Vec::<Frame>::new().into_iter())
            .next()
            .is_none());

        assert!(adjust_timeline_timestamps(Vec::<Frame>::new().into_iter())
            .next()
            .is_none());

        assert!(
            adjust_discrete_timestamps(Vec::<Frame>::new().into_iter(), 3.0)
                .next()
                .is_none()
        );

        assert!(cap_fps(Vec::<Frame>::new().into_iter(), 30)
            .next()
            .is_none());
    }

    #[test]
    fn timeline_adjustment_subtracts_first_timestamp() {
        let frames = vec![tagged(5.0, 0), tagged(8.0, 1), tagged(10.0, 2)];
        let frames: Vec<_> = adjust_timeline_timestamps(frames.into_iter()).collect();

        assert_eq!(times(&frames), vec![0.0, 3.0, 5.0]);
    }

    #[test]
    fn cap_fps_keeps_latest_state_per_interval_at_window_start() {
        let frames = vec![
            tagged(0.0, 0),
            tagged(0.033, 1),
            tagged(0.066, 2),
            tagged(1.0, 3),
        ];

        let frames: Vec<_> = cap_fps(frames.into_iter(), 30).collect();

        assert_eq!(times(&frames), vec![0.0, 0.066, 1.0]);
        assert_eq!(tags(&frames), vec![1, 2, 3]);
    }

    #[test]
    fn cap_fps_keeps_widely_spaced_frames() {
        let frames = vec![tagged(0.0, 0), tagged(1.0, 1), tagged(2.0, 2)];
        let frames: Vec<_> = cap_fps(frames.into_iter(), 30).collect();

        assert_eq!(times(&frames), vec![0.0, 1.0, 2.0]);
        assert_eq!(tags(&frames), vec![0, 1, 2]);
    }

    #[test]
    fn discrete_timestamps_are_sequential() {
        let frames = vec![tagged(2.0, 0), tagged(5.0, 1), tagged(10.0, 2)];
        let frames: Vec<_> = adjust_discrete_timestamps(frames.into_iter(), 3.0).collect();

        assert_eq!(times(&frames), vec![0.0, 3.0, 6.0]);
    }
}
