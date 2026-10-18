//! CGEvent injection. Held keys are tracked so a disconnect can release them.

use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use core_graphics::event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton, EventField};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;

use crate::keys_util::to_mac;
use crate::pointer::{drag_action, ClickTracker, PointerAction};

static HELD_KEYS: Mutex<Vec<u16>> = Mutex::new(Vec::new());
static HELD_BUTTONS: Mutex<Vec<u8>> = Mutex::new(Vec::new());
/// The current click sequence, for `MOUSE_EVENT_CLICK_STATE` (double/triple).
static CLICKS: Mutex<ClickTracker> = Mutex::new(ClickTracker::new());

fn source() -> Option<CGEventSource> {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()
}

/// Milliseconds since the first injection. Monotonic, so a wall-clock change
/// never collapses or stretches a click sequence.
fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// The current cursor position, in the same coordinate space `rel_move` uses.
/// Falls back to asking a fresh CGEvent where the cursor is. Returns `None`
/// rather than `(0, 0)`, because posting at the origin teleports the pointer to
/// the top-left corner of the screen.
fn cursor_xy() -> Option<(i32, i32)> {
    if let Some(pos) = super::cursor::position() {
        return Some(pos);
    }
    let source = source()?;
    let event = CGEvent::new(source).ok()?;
    let point = event.location();
    Some((point.x.round() as i32, point.y.round() as i32))
}

/// Event type and button for a relative move. While a button is held macOS
/// expects the matching `*MouseDragged` event, not `MouseMoved`, so drags and
/// text selection work.
fn drag_event() -> (CGEventType, CGMouseButton) {
    let held = HELD_BUTTONS.lock().expect("buttons");
    match drag_action(&held) {
        PointerAction::Move => (CGEventType::MouseMoved, CGMouseButton::Left),
        PointerAction::LeftDrag => (CGEventType::LeftMouseDragged, CGMouseButton::Left),
        PointerAction::RightDrag => (CGEventType::RightMouseDragged, CGMouseButton::Right),
        PointerAction::OtherDrag => (CGEventType::OtherMouseDragged, CGMouseButton::Center),
    }
}

pub fn rel_move(dx: i32, dy: i32) {
    if dx == 0 && dy == 0 {
        return;
    }
    // The event location is where the cursor goes. Posting at (0, 0) with only
    // deltas parks the pointer in the corner after the first move.
    let Some((x, y)) = cursor_xy() else { return };
    let nx = x.saturating_add(dx);
    let ny = y.saturating_add(dy);
    let (cx, cy) = super::display::clamp_point_to_display(x, y, nx, ny);
    super::cursor::warp(cx, cy);
    let Some(source) = source() else { return };
    let (kind, mouse) = drag_event();
    let Ok(event) =
        CGEvent::new_mouse_event(source, kind, CGPoint::new(cx as f64, cy as f64), mouse)
    else {
        return;
    };
    event.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_X, dx as i64);
    event.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y, dy as i64);
    event.post(CGEventTapLocation::HID);
}

pub fn button(button: u8, down: bool) {
    let Some(source) = source() else { return };
    let kind = match (button, down) {
        (0, true) => CGEventType::LeftMouseDown,
        (0, false) => CGEventType::LeftMouseUp,
        (1, true) => CGEventType::RightMouseDown,
        (1, false) => CGEventType::RightMouseUp,
        (_, true) => CGEventType::OtherMouseDown,
        (_, false) => CGEventType::OtherMouseUp,
    };
    let mouse = match button {
        0 => CGMouseButton::Left,
        1 => CGMouseButton::Right,
        _ => CGMouseButton::Center,
    };
    // Post at the cursor, never at (0, 0): a click at the origin teleports the
    // pointer to the top-left corner of the screen.
    let Some((x, y)) = cursor_xy() else { return };
    // Count down events so double and triple clicks register; the up event
    // carries the same state so the pair is a well-formed click sequence.
    let click_state = {
        let mut clicks = CLICKS.lock().expect("clicks");
        if down {
            clicks.down(button, x, y, now_ms())
        } else {
            clicks.current()
        }
    };
    if let Ok(event) =
        CGEvent::new_mouse_event(source, kind, CGPoint::new(x as f64, y as f64), mouse)
    {
        event.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, click_state as i64);
        event.post(CGEventTapLocation::HID);
    }
    let mut held = HELD_BUTTONS.lock().expect("buttons");
    if down {
        if !held.contains(&button) {
            held.push(button);
        }
    } else {
        held.retain(|b| *b != button);
    }
}

pub fn wheel(dx: i32, dy: i32) {
    let Some(source) = source() else { return };
    if let Ok(event) = CGEvent::new_scroll_event(
        source,
        core_graphics::event::ScrollEventUnit::PIXEL,
        2,
        dy,
        dx,
        0,
    ) {
        event.post(CGEventTapLocation::HID);
    }
}

pub fn key(hid: u16, down: bool) {
    let Some(code) = to_mac(hid) else { return };
    let Some(source) = source() else { return };
    if let Ok(event) = CGEvent::new_keyboard_event(source, code, down) {
        event.post(CGEventTapLocation::HID);
    }
    let mut held = HELD_KEYS.lock().expect("keys");
    if down {
        if !held.contains(&hid) {
            held.push(hid);
        }
    } else {
        held.retain(|k| *k != hid);
    }
}

pub fn release_all() {
    let keys = std::mem::take(&mut *HELD_KEYS.lock().expect("keys"));
    let buttons = std::mem::take(&mut *HELD_BUTTONS.lock().expect("buttons"));
    CLICKS.lock().expect("clicks").reset();
    for hid in keys {
        key(hid, false);
    }
    for button_id in buttons {
        button(button_id, false);
    }
}
