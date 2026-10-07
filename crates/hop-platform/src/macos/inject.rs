//! CGEvent injection. Held keys are tracked so a disconnect can release them.

use std::sync::Mutex;

use core_graphics::event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;

use crate::keys_util::to_mac;

static HELD_KEYS: Mutex<Vec<u16>> = Mutex::new(Vec::new());
static HELD_BUTTONS: Mutex<Vec<u8>> = Mutex::new(Vec::new());

fn source() -> Option<CGEventSource> {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()
}

pub fn rel_move(dx: i32, dy: i32) {
    let Some(source) = source() else { return };
    let Ok(event) = CGEvent::new_mouse_event(
        source,
        CGEventType::MouseMoved,
        CGPoint::new(0.0, 0.0),
        CGMouseButton::Left,
    ) else {
        return;
    };
    event.set_integer_value_field(
        core_graphics::event::EventField::MOUSE_EVENT_DELTA_X,
        dx as i64,
    );
    event.set_integer_value_field(
        core_graphics::event::EventField::MOUSE_EVENT_DELTA_Y,
        dy as i64,
    );
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
    if let Ok(event) = CGEvent::new_mouse_event(source, kind, CGPoint::new(0.0, 0.0), mouse) {
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
    for hid in keys {
        key(hid, false);
    }
    for button_id in buttons {
        button(button_id, false);
    }
}
