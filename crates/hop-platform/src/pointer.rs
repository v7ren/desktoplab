//! Pure pointer bookkeeping shared by the OS backends. There are no OS calls
//! here, so the click-count and drag-event rules are unit-tested on every
//! platform (including Windows CI, where the macOS backend itself cannot build).

/// The double-click interval. Two button-downs closer than this (and within a
/// few pixels) belong to the same click sequence.
pub const DOUBLE_CLICK_INTERVAL_MS: u64 = 500;

/// Two clicks within this many pixels (Chebyshev distance) are "the same spot"
/// for click counting.
pub const CLICK_DISTANCE_PX: i32 = 4;

/// Tracks a run of button-downs so a backend can set the click-state field
/// (`1` single, `2` double, `3` triple).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClickTracker {
    last_ms: Option<u64>,
    last_x: i32,
    last_y: i32,
    last_button: u8,
    count: u8,
}

impl ClickTracker {
    pub const fn new() -> Self {
        Self {
            last_ms: None,
            last_x: 0,
            last_y: 0,
            last_button: 0,
            count: 0,
        }
    }

    /// Record a button-down at `(x, y)` at `now_ms` and return the click state
    /// to post for it (`1` single, `2` double, `3` triple, capped at 3).
    pub fn down(&mut self, button: u8, x: i32, y: i32, now_ms: u64) -> u8 {
        let near = self.last_button == button
            && (x - self.last_x).abs() <= CLICK_DISTANCE_PX
            && (y - self.last_y).abs() <= CLICK_DISTANCE_PX;
        let soon = self
            .last_ms
            .is_some_and(|last| now_ms.saturating_sub(last) <= DOUBLE_CLICK_INTERVAL_MS);
        self.count = if near && soon {
            (self.count + 1).min(3)
        } else {
            1
        };
        self.last_ms = Some(now_ms);
        self.last_x = x;
        self.last_y = y;
        self.last_button = button;
        self.count
    }

    /// The state a matching button-up should carry (at least 1).
    pub fn current(&self) -> u8 {
        self.count.max(1)
    }

    /// Forget the current sequence, e.g. after releasing all inputs.
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

impl Default for ClickTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Which relative-move event a backend should post, given the buttons currently
/// held (`0` left, `1` right, anything else "other").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerAction {
    Move,
    LeftDrag,
    RightDrag,
    OtherDrag,
}

/// While a button is held, the matching `*MouseDragged` event is required
/// instead of a plain `MouseMoved` so drags and text selection work. Left wins
/// over right if both are somehow held.
pub fn drag_action(held: &[u8]) -> PointerAction {
    if held.contains(&0) {
        PointerAction::LeftDrag
    } else if held.contains(&1) {
        PointerAction::RightDrag
    } else if !held.is_empty() {
        PointerAction::OtherDrag
    } else {
        PointerAction::Move
    }
}

/// macOS pointer injection modes: `0` warp then post at the HID tap (the old
/// behaviour), `1` post only at the HID tap (default), `2` post only at the
/// Session tap from a combined-session source.
pub const POINTER_MODE_DEFAULT: u8 = 0;

/// Keeps a stored pointer mode in range. Anything outside `0..=2` becomes the
/// default, so a hand-edited or unknown value never disables injection.
pub fn clamp_pointer_mode(mode: i64) -> u8 {
    if (0..=2).contains(&mode) {
        mode as u8
    } else {
        POINTER_MODE_DEFAULT
    }
}

/// Maps the HID usages DevHop has injected as held to a modifier bitmask:
/// Control `1`, Shift `2`, Alt/Option `4`, Command `8`. Both left and right
/// variants count. Non-modifier usages are ignored.
pub fn modifier_bits_from_held(held_hids: &[u16]) -> u8 {
    held_hids.iter().fold(0u8, |bits, hid| {
        bits | match hid {
            0xE0 | 0xE4 => 1,
            0xE1 | 0xE5 => 2,
            0xE2 | 0xE6 => 4,
            0xE3 | 0xE7 => 8,
            _ => 0,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_pointer_mode_keeps_known_modes() {
        assert_eq!(clamp_pointer_mode(0), 0);
        assert_eq!(clamp_pointer_mode(1), 1);
        assert_eq!(clamp_pointer_mode(2), 2);
    }

    #[test]
    fn clamp_pointer_mode_maps_unknown_values_to_default() {
        assert_eq!(clamp_pointer_mode(9), POINTER_MODE_DEFAULT);
        assert_eq!(clamp_pointer_mode(-1), POINTER_MODE_DEFAULT);
        assert_eq!(clamp_pointer_mode(255), POINTER_MODE_DEFAULT);
        assert_eq!(POINTER_MODE_DEFAULT, 0);
    }

    #[test]
    fn modifier_bits_none_held_is_zero() {
        assert_eq!(modifier_bits_from_held(&[]), 0);
    }

    #[test]
    fn modifier_bits_left_ctrl_is_one() {
        assert_eq!(modifier_bits_from_held(&[0xE0]), 1);
        assert_eq!(modifier_bits_from_held(&[0xE4]), 1);
    }

    #[test]
    fn modifier_bits_right_ctrl_and_left_shift_is_three() {
        assert_eq!(modifier_bits_from_held(&[0xE4, 0xE1]), 3);
    }

    #[test]
    fn modifier_bits_ignore_non_modifier_usages() {
        assert_eq!(modifier_bits_from_held(&[0x04, 0x2C, 0x00]), 0);
        assert_eq!(modifier_bits_from_held(&[0x04, 0xE2]), 4);
    }

    #[test]
    fn modifier_bits_all_four_is_fifteen() {
        assert_eq!(modifier_bits_from_held(&[0xE0, 0xE1, 0xE2, 0xE3]), 15);
        assert_eq!(modifier_bits_from_held(&[0xE7, 0xE6, 0xE5, 0xE4]), 15);
    }

    #[test]
    fn clicks_within_the_interval_and_pixels_count_up_to_three() {
        let mut tracker = ClickTracker::new();
        assert_eq!(tracker.down(0, 100, 200, 0), 1);
        assert_eq!(tracker.down(0, 101, 201, 200), 2);
        assert_eq!(tracker.down(0, 102, 202, 400), 3);
        // A fourth click stays at a triple click.
        assert_eq!(tracker.down(0, 102, 202, 600), 3);
    }

    #[test]
    fn a_slow_second_click_starts_over() {
        let mut tracker = ClickTracker::new();
        assert_eq!(tracker.down(0, 10, 10, 0), 1);
        assert_eq!(tracker.down(0, 10, 10, DOUBLE_CLICK_INTERVAL_MS + 1), 1);
    }

    #[test]
    fn a_second_click_exactly_at_the_limit_still_counts() {
        let mut tracker = ClickTracker::new();
        assert_eq!(tracker.down(0, 10, 10, 0), 1);
        assert_eq!(tracker.down(0, 10 + CLICK_DISTANCE_PX, 10, 500), 2);
    }

    #[test]
    fn a_distant_second_click_starts_over() {
        let mut tracker = ClickTracker::new();
        assert_eq!(tracker.down(0, 10, 10, 0), 1);
        assert_eq!(tracker.down(0, 10 + CLICK_DISTANCE_PX + 1, 10, 100), 1);
    }

    #[test]
    fn a_different_button_starts_over() {
        let mut tracker = ClickTracker::new();
        assert_eq!(tracker.down(0, 10, 10, 0), 1);
        assert_eq!(tracker.down(1, 10, 10, 100), 1);
    }

    #[test]
    fn current_is_one_before_any_click_and_after_reset() {
        let mut tracker = ClickTracker::new();
        assert_eq!(tracker.current(), 1);
        tracker.down(0, 5, 5, 0);
        tracker.down(0, 5, 5, 100);
        assert_eq!(tracker.current(), 2);
        tracker.reset();
        assert_eq!(tracker.current(), 1);
    }

    #[test]
    fn drag_action_follows_the_held_button() {
        assert_eq!(drag_action(&[]), PointerAction::Move);
        assert_eq!(drag_action(&[0]), PointerAction::LeftDrag);
        assert_eq!(drag_action(&[1]), PointerAction::RightDrag);
        assert_eq!(drag_action(&[2]), PointerAction::OtherDrag);
        // Left wins if both are held (should not happen, but must be defined).
        assert_eq!(drag_action(&[1, 0]), PointerAction::LeftDrag);
    }
}
