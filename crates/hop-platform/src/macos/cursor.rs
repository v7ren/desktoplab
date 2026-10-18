//! Cursor warp and the remote-mode disconnect between mouse and cursor.
//! All x/y values are in **points** (CGEvent / CGDisplay space).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use core_graphics::display::CGDisplay;
use core_graphics::event::CGEvent;
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;

struct Tracked {
    x: i32,
    y: i32,
    at: Instant,
}

static TRACKED: Mutex<Option<Tracked>> = Mutex::new(None);

pub const TRACKED_MAX_AGE: Duration = Duration::from_millis(150);

fn note_position(x: i32, y: i32) {
    *TRACKED.lock().expect("tracked") = Some(Tracked {
        x,
        y,
        at: Instant::now(),
    });
}

/// Recent warp/inject position when the OS read would lag behind `warp`.
pub fn recent_tracked(max_age: Duration) -> Option<(i32, i32)> {
    let guard = TRACKED.lock().expect("tracked");
    let t = guard.as_ref()?;
    if t.at.elapsed() <= max_age {
        Some((t.x, t.y))
    } else {
        None
    }
}

pub fn position() -> Option<(i32, i32)> {
    if let Some(pos) = recent_tracked(TRACKED_MAX_AGE) {
        return Some(pos);
    }
    let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState).ok()?;
    let event = CGEvent::new(source).ok()?;
    let point = event.location();
    Some((point.x.round() as i32, point.y.round() as i32))
}

pub fn warp(x: i32, y: i32) {
    note_position(x, y);
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

/// Called after posting a synthetic move at `(x, y)` so `rel_move` stays coherent.
pub fn note_injected_position(x: i32, y: i32) {
    note_position(x, y);
}
