//! Cursor warp and the remote-mode disconnect between mouse and cursor.

use core_graphics::display::CGDisplay;
use core_graphics::event::CGEvent;
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;

pub fn position() -> Option<(i32, i32)> {
    let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState).ok()?;
    let event = CGEvent::new(source).ok()?;
    let point = event.location();
    Some((point.x.round() as i32, point.y.round() as i32))
}

pub fn warp(x: i32, y: i32) {
    let _ = CGDisplay::warp_mouse_cursor_position(CGPoint::new(x as f64, y as f64));
}

pub fn hide() {
    let _ = CGDisplay::main().hide_cursor();
}

pub fn show() {
    let _ = CGDisplay::main().show_cursor();
}

/// `false` parks the cursor: later mouse events report deltas but the pointer stays.
pub fn associate(connected: bool) {
    let _ = CGDisplay::associate_mouse_and_mouse_cursor_position(connected);
}
