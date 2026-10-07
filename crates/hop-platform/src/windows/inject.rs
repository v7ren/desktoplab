//! SendInput injection. Held keys are remembered so a disconnect can release them.

use std::collections::HashSet;
use std::mem::size_of;
use std::sync::Mutex;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT, VIRTUAL_KEY,
};

use crate::keys_util::to_vk;

static HELD_KEYS: Mutex<Vec<u16>> = Mutex::new(Vec::new());
static HELD_BUTTONS: Mutex<Vec<u8>> = Mutex::new(Vec::new());

pub fn rel_move(dx: i32, dy: i32) {
    send_mouse(dx, dy, MOUSEEVENTF_MOVE, 0);
}

pub fn button(button: u8, down: bool) {
    let flag = match (button, down) {
        (0, true) => MOUSEEVENTF_LEFTDOWN,
        (0, false) => MOUSEEVENTF_LEFTUP,
        (1, true) => MOUSEEVENTF_RIGHTDOWN,
        (1, false) => MOUSEEVENTF_RIGHTUP,
        (_, true) => MOUSEEVENTF_MIDDLEDOWN,
        (_, false) => MOUSEEVENTF_MIDDLEUP,
    };
    send_mouse(0, 0, flag, 0);
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
    if dy != 0 {
        send_mouse(0, 0, MOUSEEVENTF_WHEEL, dy as u32);
    }
    if dx != 0 {
        send_mouse(0, 0, MOUSEEVENTF_HWHEEL, dx as u32);
    }
}

pub fn key(hid: u16, down: bool) {
    let Some(vk) = to_vk(hid) else {
        return;
    };
    let mut flags = if down {
        Default::default()
    } else {
        KEYEVENTF_KEYUP
    };
    if is_extended(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(&[input], size_of::<INPUT>() as i32);
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
    let keys: Vec<u16> = std::mem::take(&mut *HELD_KEYS.lock().expect("keys"));
    let buttons: Vec<u8> = std::mem::take(&mut *HELD_BUTTONS.lock().expect("buttons"));
    for hid in keys {
        key(hid, false);
    }
    for button_id in buttons {
        button(button_id, false);
    }
    let _ = HashSet::<u16>::new();
}

fn send_mouse(
    dx: i32,
    dy: i32,
    flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS,
    data: u32,
) {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(&[input], size_of::<INPUT>() as i32);
    }
}

fn is_extended(vk: u16) -> bool {
    matches!(
        vk,
        0x21..=0x28 | 0x2D | 0x2E | 0x5B | 0x5C | 0xA3 | 0xA5 | 0x6F
    )
}
