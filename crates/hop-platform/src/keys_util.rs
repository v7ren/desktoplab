//! Shared helpers for turning OS key codes into HID usages.

use hop_proto::{hid_to_mac, hid_to_vk, mac_to_hid, vk_to_hid};

pub fn from_vk(vk: u16) -> Option<u16> {
    vk_to_hid(vk)
}

pub fn to_vk(hid: u16) -> Option<u16> {
    hid_to_vk(hid)
}

pub fn from_mac(code: u16) -> Option<u16> {
    mac_to_hid(code)
}

pub fn to_mac(hid: u16) -> Option<u16> {
    hid_to_mac(hid)
}
