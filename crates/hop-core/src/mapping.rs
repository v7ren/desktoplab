//! Modifier translation and scroll normalization (R9).
//!
//! HID usages travel on the wire. When the machines differ, Ctrl and Cmd can
//! swap (Windows Ctrl ↔ Mac Command) and Win and Option can swap
//! (Windows GUI ↔ Mac Alt). The two swaps touch different usages, so both
//! can be on at once.

use serde::{Deserialize, Serialize};

pub const HID_A: u16 = 0x04;
pub const HID_C: u16 = 0x06;
pub const HID_LCTRL: u16 = 0xE0;
pub const HID_LSHIFT: u16 = 0xE1;
pub const HID_LALT: u16 = 0xE2;
pub const HID_LGUI: u16 = 0xE3;
pub const HID_RCTRL: u16 = 0xE4;
pub const HID_RSHIFT: u16 = 0xE5;
pub const HID_RALT: u16 = 0xE6;
pub const HID_RGUI: u16 = 0xE7;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostOs {
    Windows,
    Macos,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModifierMap {
    pub ctrl_cmd: bool,
    pub win_option: bool,
}

impl Default for ModifierMap {
    fn default() -> Self {
        Self {
            ctrl_cmd: true,
            win_option: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScrollNorm {
    /// When true, the wheel sign is flipped (macOS "natural" scrolling).
    pub natural: bool,
    pub speed: f64,
}

impl Default for ScrollNorm {
    fn default() -> Self {
        Self {
            natural: false,
            speed: 1.0,
        }
    }
}

pub fn translate_hid(map: ModifierMap, from: HostOs, to: HostOs, hid: u16) -> u16 {
    if from == to {
        return hid;
    }
    match (from, to) {
        (HostOs::Windows, HostOs::Macos) => windows_to_mac(map, hid),
        (HostOs::Macos, HostOs::Windows) => mac_to_windows(map, hid),
        (HostOs::Windows, HostOs::Windows) | (HostOs::Macos, HostOs::Macos) => hid,
    }
}

fn windows_to_mac(map: ModifierMap, hid: u16) -> u16 {
    if map.ctrl_cmd && hid == HID_LCTRL {
        return HID_LGUI;
    }
    if map.ctrl_cmd && hid == HID_RCTRL {
        return HID_RGUI;
    }
    if map.win_option && hid == HID_LGUI {
        return HID_LALT;
    }
    if map.win_option && hid == HID_RGUI {
        return HID_RALT;
    }
    hid
}

fn mac_to_windows(map: ModifierMap, hid: u16) -> u16 {
    if map.ctrl_cmd && hid == HID_LGUI {
        return HID_LCTRL;
    }
    if map.ctrl_cmd && hid == HID_RGUI {
        return HID_RCTRL;
    }
    if map.win_option && hid == HID_LALT {
        return HID_LGUI;
    }
    if map.win_option && hid == HID_RALT {
        return HID_RGUI;
    }
    hid
}

/// Apply natural-scroll and the speed multiplier. High-resolution wheels are
/// just larger integer deltas; they scale the same way.
pub fn normalize_wheel(cfg: ScrollNorm, dx: i32, dy: i32) -> (i32, i32) {
    let sign = if cfg.natural { -1.0 } else { 1.0 };
    let speed = if cfg.speed.is_finite() && cfg.speed > 0.0 {
        cfg.speed
    } else {
        1.0
    };
    let scale = |v: i32| (v as f64 * sign * speed).round() as i32;
    (scale(dx), scale(dy))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_c_on_the_mac_becomes_ctrl_c_on_windows() {
        let map = ModifierMap {
            ctrl_cmd: true,
            win_option: true,
        };
        assert_eq!(
            translate_hid(map, HostOs::Macos, HostOs::Windows, HID_LGUI),
            HID_LCTRL
        );
        assert_eq!(
            translate_hid(map, HostOs::Macos, HostOs::Windows, HID_C),
            HID_C
        );
        assert_eq!(
            translate_hid(map, HostOs::Windows, HostOs::Macos, HID_LCTRL),
            HID_LGUI
        );
    }

    #[test]
    fn win_and_option_swap_without_touching_ctrl() {
        let map = ModifierMap {
            ctrl_cmd: true,
            win_option: true,
        };
        assert_eq!(
            translate_hid(map, HostOs::Windows, HostOs::Macos, HID_LGUI),
            HID_LALT
        );
        assert_eq!(
            translate_hid(map, HostOs::Macos, HostOs::Windows, HID_LALT),
            HID_LGUI
        );
        assert_eq!(
            translate_hid(map, HostOs::Macos, HostOs::Windows, HID_LCTRL),
            HID_LCTRL
        );
    }

    #[test]
    fn same_os_and_disabled_maps_leave_usages_alone() {
        let off = ModifierMap {
            ctrl_cmd: false,
            win_option: false,
        };
        assert_eq!(
            translate_hid(off, HostOs::Windows, HostOs::Macos, HID_LCTRL),
            HID_LCTRL
        );
        let on = ModifierMap::default();
        assert_eq!(
            translate_hid(on, HostOs::Windows, HostOs::Windows, HID_LCTRL),
            HID_LCTRL
        );
        assert_eq!(
            translate_hid(on, HostOs::Macos, HostOs::Macos, HID_A),
            HID_A
        );
    }

    #[test]
    fn scroll_speed_and_natural_direction() {
        let cfg = ScrollNorm {
            natural: false,
            speed: 1.5,
        };
        assert_eq!(normalize_wheel(cfg, 0, 120), (0, 180));
        let natural = ScrollNorm {
            natural: true,
            speed: 1.0,
        };
        assert_eq!(normalize_wheel(natural, 8, -4), (-8, 4));
        let bad = ScrollNorm {
            natural: false,
            speed: f64::NAN,
        };
        assert_eq!(normalize_wheel(bad, 3, 4), (3, 4));
    }
}
