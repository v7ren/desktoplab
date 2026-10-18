//! Event tap. The callback only enqueues, and a timeout-disabled tap is turned back on.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Mutex, OnceLock};

use arc_swap::ArcSwap;
use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
use core_graphics::event::{
    CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventType,
};
use crossbeam_channel::Sender;
use hop_core::{chord_matches, Chord, HotAction};

use crate::keys_util::from_mac;
use crate::InputEvent;

static LOOP: Mutex<Option<CFRunLoop>> = Mutex::new(None);

struct Shared {
    tx: Sender<InputEvent>,
    hotkeys: ArcSwap<Vec<(Chord, HotAction)>>,
    remote: AtomicBool,
    park_x: AtomicI32,
    park_y: AtomicI32,
    ctrl: AtomicBool,
    alt: AtomicBool,
    shift: AtomicBool,
    gui: AtomicBool,
}

static SHARED: OnceLock<Shared> = OnceLock::new();
static STOP: AtomicBool = AtomicBool::new(false);

pub fn set_hotkeys(chords: Vec<(Chord, HotAction)>) {
    if let Some(shared) = SHARED.get() {
        shared.hotkeys.store(std::sync::Arc::new(chords));
    }
}

pub fn set_remote(enabled: bool, park_x: i32, park_y: i32) {
    if let Some(shared) = SHARED.get() {
        shared.park_x.store(park_x, Ordering::SeqCst);
        shared.park_y.store(park_y, Ordering::SeqCst);
        shared.remote.store(enabled, Ordering::SeqCst);
    }
    if enabled {
        // Reset a stuck disassociated cursor, then disassociate for parked remote control.
        crate::macos::cursor::associate(true);
        crate::macos::cursor::associate(false);
    } else {
        crate::macos::cursor::associate(true);
    }
    if enabled {
        crate::macos::cursor::warp(park_x, park_y);
        crate::macos::cursor::hide();
    } else {
        crate::macos::cursor::show();
    }
}

pub fn start(tx: Sender<InputEvent>) -> Result<(), String> {
    let _ = SHARED.get_or_init(|| Shared {
        tx,
        hotkeys: ArcSwap::from_pointee(Vec::new()),
        remote: AtomicBool::new(false),
        park_x: AtomicI32::new(0),
        park_y: AtomicI32::new(0),
        ctrl: AtomicBool::new(false),
        alt: AtomicBool::new(false),
        shift: AtomicBool::new(false),
        gui: AtomicBool::new(false),
    });
    std::thread::Builder::new()
        .name("devhop-tap".into())
        .spawn(tap_thread)
        .map_err(|err| err.to_string())?;
    Ok(())
}

pub fn stop() {
    STOP.store(true, Ordering::SeqCst);
    if let Some(run) = LOOP.lock().expect("run loop").as_ref() {
        run.stop();
    }
}

fn tap_thread() {
    let events = vec![
        CGEventType::MouseMoved,
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGEventType::RightMouseDown,
        CGEventType::RightMouseUp,
        CGEventType::OtherMouseDown,
        CGEventType::OtherMouseUp,
        CGEventType::ScrollWheel,
        CGEventType::KeyDown,
        CGEventType::KeyUp,
        CGEventType::FlagsChanged,
    ];
    let tap = CGEventTap::new(
        CGEventTapLocation::HID,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default,
        events,
        |_proxy, kind, event| on_event(kind, event),
    );
    let Ok(tap) = tap else {
        return;
    };
    unsafe {
        let source = tap
            .mach_port
            .create_runloop_source(0)
            .expect("runloop source");
        let current = CFRunLoop::get_current();
        *LOOP.lock().expect("run loop") = Some(current.clone());
        current.add_source(&source, kCFRunLoopCommonModes);
        tap.enable();
        while !STOP.load(Ordering::SeqCst) {
            CFRunLoop::run_in_mode(
                kCFRunLoopCommonModes,
                std::time::Duration::from_millis(200),
                true,
            );
            if !STOP.load(Ordering::SeqCst) {
                tap.enable();
            }
        }
    }
}

fn on_event(
    kind: CGEventType,
    event: &core_graphics::event::CGEvent,
) -> Option<core_graphics::event::CGEvent> {
    let Some(shared) = SHARED.get() else {
        return Some(event.clone());
    };
    let remote = shared.remote.load(Ordering::SeqCst);
    match kind {
        CGEventType::MouseMoved
        | CGEventType::LeftMouseDragged
        | CGEventType::RightMouseDragged
        | CGEventType::OtherMouseDragged => {
            let point = event.location();
            let dx = event
                .get_integer_value_field(core_graphics::event::EventField::MOUSE_EVENT_DELTA_X)
                as i32;
            let dy = event
                .get_integer_value_field(core_graphics::event::EventField::MOUSE_EVENT_DELTA_Y)
                as i32;
            let _ = shared.tx.try_send(InputEvent::Motion {
                x: point.x.round() as i32,
                y: point.y.round() as i32,
                dx,
                dy,
            });
            if remote {
                crate::macos::cursor::warp(
                    shared.park_x.load(Ordering::SeqCst),
                    shared.park_y.load(Ordering::SeqCst),
                );
                return None;
            }
        }
        CGEventType::LeftMouseDown => {
            button(shared, 0, true);
            if remote {
                return None;
            }
        }
        CGEventType::LeftMouseUp => {
            button(shared, 0, false);
            if remote {
                return None;
            }
        }
        CGEventType::RightMouseDown => {
            button(shared, 1, true);
            if remote {
                return None;
            }
        }
        CGEventType::RightMouseUp => {
            button(shared, 1, false);
            if remote {
                return None;
            }
        }
        CGEventType::OtherMouseDown => {
            button(shared, 2, true);
            if remote {
                return None;
            }
        }
        CGEventType::OtherMouseUp => {
            button(shared, 2, false);
            if remote {
                return None;
            }
        }
        CGEventType::ScrollWheel => {
            let dy = event.get_integer_value_field(
                core_graphics::event::EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1,
            ) as i32;
            let dx = event.get_integer_value_field(
                core_graphics::event::EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2,
            ) as i32;
            let _ = shared.tx.try_send(InputEvent::Wheel { dx, dy });
            if remote {
                return None;
            }
        }
        CGEventType::KeyDown | CGEventType::KeyUp => {
            let code = event
                .get_integer_value_field(core_graphics::event::EventField::KEYBOARD_EVENT_KEYCODE)
                as u16;
            let down = matches!(kind, CGEventType::KeyDown);
            if let Some(hid) = from_mac(code) {
                if down {
                    let table = shared.hotkeys.load();
                    if let Some((_, action)) = table.iter().find(|(chord, _)| {
                        chord_matches(
                            chord,
                            shared.ctrl.load(Ordering::SeqCst),
                            shared.alt.load(Ordering::SeqCst),
                            shared.shift.load(Ordering::SeqCst),
                            shared.gui.load(Ordering::SeqCst),
                            hid,
                        )
                    }) {
                        let _ = shared.tx.try_send(InputEvent::Hotkey(*action));
                        return None;
                    }
                }
                let _ = shared.tx.try_send(InputEvent::Key { hid, down });
            }
            if remote {
                return None;
            }
        }
        CGEventType::FlagsChanged => {
            let flags = event.get_flags();
            // Modifiers arrive as flag changes, not key-down events. Without
            // this, Option and Command never leave the Mac.
            let next = [
                (
                    flags.contains(CGEventFlags::CGEventFlagShift),
                    &shared.shift,
                    0xE1,
                ),
                (
                    flags.contains(CGEventFlags::CGEventFlagControl),
                    &shared.ctrl,
                    0xE0,
                ),
                (
                    flags.contains(CGEventFlags::CGEventFlagAlternate),
                    &shared.alt,
                    0xE2,
                ),
                (
                    flags.contains(CGEventFlags::CGEventFlagCommand),
                    &shared.gui,
                    0xE3,
                ),
            ];
            for (down, slot, hid) in next {
                let was = slot.swap(down, Ordering::SeqCst);
                if was != down && remote {
                    let _ = shared.tx.try_send(InputEvent::Key { hid, down });
                }
            }
            if remote {
                return None;
            }
        }
        _ => {}
    }
    Some(event.clone())
}

fn button(shared: &Shared, button: u8, down: bool) {
    let _ = shared.tx.try_send(InputEvent::Button { button, down });
}
