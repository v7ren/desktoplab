//! Park point for Windows remote clip (box size = 2× inset from centre).

use crate::DisplayInfo;

pub const CLIP_SPAN: i32 = 160;

/// Clamp `exit` so a `(2×span)`×`(2×span)` clip centred on the result fits in the monitor.
/// `monitor` is physical `(left, top, width, height)`.
pub fn park_point_for_clip(
    exit_x: i32,
    exit_y: i32,
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    span: i32,
) -> (i32, i32) {
    let w = width as i32;
    let h = height as i32;
    let clip_size = span * 2;
    let right = left + w;
    let bottom = top + h;
    let x = if w < clip_size {
        left + w / 2
    } else {
        exit_x.clamp(left + span, right - span)
    };
    let y = if h < clip_size {
        top + h / 2
    } else {
        exit_y.clamp(top + span, bottom - span)
    };
    (x, y)
}

pub fn monitor_phys_containing(displays: &[DisplayInfo], x: i32, y: i32) -> (i32, i32, u32, u32) {
    for d in displays {
        let w = d.width as i32;
        let h = d.height as i32;
        if x >= d.x && y >= d.y && x < d.x + w && y < d.y + h {
            return (d.x, d.y, d.width, d.height);
        }
    }
    if let Some(d) = displays.first() {
        (d.x, d.y, d.width, d.height)
    } else {
        (0, 0, 1920, 1080)
    }
}

pub fn park_point_for_remote(
    exit_x: i32,
    exit_y: i32,
    displays: &[DisplayInfo],
    span: i32,
) -> (i32, i32) {
    let (left, top, w, h) = monitor_phys_containing(displays, exit_x, exit_y);
    park_point_for_clip(exit_x, exit_y, left, top, w, h, span)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn right_edge_exit_parks_160_inside_at_same_y() {
        let (x, y) = park_point_for_clip(1919, 400, 0, 0, 1920, 1080, CLIP_SPAN);
        assert_eq!(x, 1920 - CLIP_SPAN);
        assert_eq!(y, 400);
    }

    #[test]
    fn bottom_right_corner_clamps_both_axes() {
        let (x, y) = park_point_for_clip(1919, 1079, 0, 0, 1920, 1080, CLIP_SPAN);
        assert_eq!(x, 1920 - CLIP_SPAN);
        assert_eq!(y, 1080 - CLIP_SPAN);
    }

    #[test]
    fn small_monitor_uses_centre() {
        let (x, y) = park_point_for_clip(50, 60, 0, 0, 200, 150, CLIP_SPAN);
        assert_eq!((x, y), (100, 75));
    }

    #[test]
    fn small_monitor_centres_when_span_too_large() {
        let (x, y) = park_point_for_clip(10, 20, 0, 0, 200, 150, 400);
        assert_eq!((x, y), (100, 75));
    }

    #[test]
    fn inset_40_at_right_edge() {
        let (x, y) = park_point_for_clip(1919, 300, 0, 0, 1920, 1080, 40);
        assert_eq!((x, y), (1880, 300));
    }

    #[test]
    fn inset_400_at_right_edge() {
        let (x, y) = park_point_for_clip(1919, 500, 0, 0, 1920, 1080, 400);
        assert_eq!((x, y), (1520, 500));
        let (_, y_clamped) = park_point_for_clip(1919, 300, 0, 0, 1920, 1080, 400);
        assert_eq!(y_clamped, 400);
    }

    #[test]
    fn negative_origin_monitor() {
        let (x, y) = park_point_for_clip(-1900, 300, -1920, 100, 1920, 1080, CLIP_SPAN);
        assert_eq!(x, -1920 + CLIP_SPAN);
        assert_eq!(y, 300);
        let (x, y) = park_point_for_clip(-1, 300, -1920, 100, 1920, 1080, CLIP_SPAN);
        assert_eq!(x, -CLIP_SPAN);
        assert_eq!(y, 300);
    }

    #[test]
    fn second_monitor_to_the_right() {
        let (x, y) = park_point_for_clip(3839, 360, 1920, 0, 1920, 1080, CLIP_SPAN);
        assert_eq!(x, 3840 - CLIP_SPAN);
        assert_eq!(y, 360);
    }
}
