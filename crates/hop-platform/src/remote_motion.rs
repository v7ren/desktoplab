//! Filter stale absolute positions while Windows is in remote (parked) mode.

pub const REMOTE_MOTION_SLACK: i32 = 2;
pub const REMOTE_SETTLE_MS: u64 = 10;

/// Whether a WM_MOUSEMOVE at absolute `(x,y)` should produce a forwarded delta.
pub fn should_forward_remote_motion(
    x: i32,
    y: i32,
    park_x: i32,
    park_y: i32,
    span: i32,
    ms_since_remote_enabled: Option<u64>,
) -> bool {
    if ms_since_remote_enabled.is_some_and(|ms| ms < REMOTE_SETTLE_MS) {
        return false;
    }
    let limit = span + REMOTE_MOTION_SLACK;
    (x - park_x).abs() <= limit && (y - park_y).abs() <= limit
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::park_clip::CLIP_SPAN;

    #[test]
    fn stale_pre_warp_position_is_dropped() {
        assert!(!should_forward_remote_motion(
            100,
            540,
            1760,
            540,
            CLIP_SPAN,
            Some(50)
        ));
    }

    #[test]
    fn normal_delta_inside_clip_is_kept() {
        assert!(should_forward_remote_motion(
            1761,
            541,
            1760,
            540,
            CLIP_SPAN,
            Some(50)
        ));
    }

    #[test]
    fn settling_window_drops_motion() {
        assert!(!should_forward_remote_motion(
            1760,
            540,
            1760,
            540,
            CLIP_SPAN,
            Some(5)
        ));
    }

    #[test]
    fn boundary_at_span_plus_slack() {
        let park = (1000, 500);
        let limit = CLIP_SPAN + REMOTE_MOTION_SLACK;
        assert!(should_forward_remote_motion(
            park.0 + limit,
            park.1,
            park.0,
            park.1,
            CLIP_SPAN,
            Some(20),
        ));
        assert!(!should_forward_remote_motion(
            park.0 + limit + 1,
            park.1,
            park.0,
            park.1,
            CLIP_SPAN,
            Some(20),
        ));
    }

    #[test]
    fn custom_span_limits_motion() {
        let span = 40;
        assert!(should_forward_remote_motion(
            1881,
            300,
            1880,
            300,
            span,
            Some(20)
        ));
        assert!(should_forward_remote_motion(
            1922,
            300,
            1880,
            300,
            span,
            Some(20)
        ));
        assert!(!should_forward_remote_motion(
            1923,
            300,
            1880,
            300,
            span,
            Some(20)
        ));
    }
}
