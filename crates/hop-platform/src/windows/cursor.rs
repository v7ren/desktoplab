//! Cursor warp, hide, and clip. Hiding uses the ShowCursor counter.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::UI::WindowsAndMessaging::{ClipCursor, GetCursorPos, SetCursorPos, ShowCursor};

static VISIBLE: AtomicBool = AtomicBool::new(true);

pub fn position() -> Option<(i32, i32)> {
    let mut pt = POINT::default();
    unsafe {
        GetCursorPos(&mut pt).ok()?;
    }
    Some((pt.x, pt.y))
}

pub fn warp(x: i32, y: i32) {
    unsafe {
        let _ = SetCursorPos(x, y);
    }
}

pub fn hide() {
    if VISIBLE.swap(false, Ordering::SeqCst) {
        unsafe {
            let mut guard = 0;
            while ShowCursor(false) >= 0 && guard < 16 {
                guard += 1;
            }
        }
    }
}

pub fn show() {
    if !VISIBLE.swap(true, Ordering::SeqCst) {
        unsafe {
            let mut guard = 0;
            while ShowCursor(true) < 0 && guard < 16 {
                guard += 1;
            }
        }
    }
}

/// `None` releases the clip. The rectangle is inclusive-exclusive, like `PhysRect`.
pub fn clip(rect: Option<(i32, i32, i32, i32)>) {
    unsafe {
        match rect {
            Some((x, y, w, h)) => {
                let r = RECT {
                    left: x,
                    top: y,
                    right: x.saturating_add(w),
                    bottom: y.saturating_add(h),
                };
                let _ = ClipCursor(Some(&r));
            }
            None => {
                let _ = ClipCursor(None);
            }
        }
    }
}
