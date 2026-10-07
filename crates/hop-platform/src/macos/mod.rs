//! macOS host. AppKit pasteboard calls are confined to this module so the
//! engine can ask for them without linking AppKit itself.

mod capture;
mod clipboard;
mod clipboard_files;
mod cursor;
mod display;
mod drag_source;
mod drag_target;
mod inject;
mod perm;

use std::path::PathBuf;

use crossbeam_channel::Sender;
use hop_core::{Chord, HotAction};

use crate::{ClipboardItem, DisplayInfo, Host, InputEvent, PermissionStatus};

pub struct MacHost;

impl MacHost {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MacHost {
    fn default() -> Self {
        Self::new()
    }
}

impl Host for MacHost {
    fn list_displays(&self) -> Vec<DisplayInfo> {
        display::list()
    }
    fn warp(&self, x: i32, y: i32) {
        cursor::warp(x, y);
    }
    fn cursor_pos(&self) -> Option<(i32, i32)> {
        cursor::position()
    }
    fn hide_cursor(&self) {
        cursor::hide();
    }
    fn show_cursor(&self) {
        cursor::show();
    }
    fn clip_rect(&self, _rect: Option<(i32, i32, i32, i32)>) {}
    fn set_remote(&self, enabled: bool, park_x: i32, park_y: i32) {
        capture::set_remote(enabled, park_x, park_y);
    }
    fn set_hotkeys(&self, chords: Vec<(Chord, HotAction)>) {
        capture::set_hotkeys(chords);
    }
    fn inject_rel(&self, dx: i32, dy: i32) {
        inject::rel_move(dx, dy);
    }
    fn inject_button(&self, button: u8, down: bool) {
        inject::button(button, down);
    }
    fn inject_key(&self, hid: u16, down: bool) {
        inject::key(hid, down);
    }
    fn inject_wheel(&self, dx: i32, dy: i32) {
        inject::wheel(dx, dy);
    }
    fn release_inputs(&self) {
        inject::release_all();
    }
    fn clipboard_read(&self) -> Vec<ClipboardItem> {
        let mut items = clipboard::read_items();
        let files = clipboard_files::read_files();
        if !files.is_empty() {
            items.push(ClipboardItem::Files(files));
        }
        items
    }
    fn clipboard_write(&self, items: &[ClipboardItem]) -> Result<(), String> {
        clipboard::write_items(items)?;
        for item in items {
            if let ClipboardItem::Files(paths) = item {
                clipboard_files::offer_files(paths)?;
            }
        }
        Ok(())
    }
    fn clipboard_files(&self) -> Vec<PathBuf> {
        clipboard_files::read_files()
    }
    fn clipboard_offer_files(&self, paths: &[PathBuf]) -> Result<(), String> {
        clipboard_files::offer_files(paths)
    }
    fn read_edge_drag(&self) -> Vec<PathBuf> {
        drag_source::read_drag()
    }
    fn begin_file_drag(&self, paths: &[PathBuf]) -> Result<(), String> {
        match drag_target::begin(paths) {
            Ok(()) => Ok(()),
            Err(err) => {
                let _ = drag_target::ensure_fallback(paths)?;
                Err(err)
            }
        }
    }
    fn permissions(&self) -> PermissionStatus {
        PermissionStatus {
            accessibility: perm::accessibility(),
            input_monitoring: perm::input_monitoring(),
        }
    }
    fn prompt_permissions(&self) {
        perm::prompt();
    }
    fn start(&self, tx: Sender<InputEvent>) -> Result<(), String> {
        if !self.permissions().granted() {
            return Err("Accessibility and Input Monitoring are required".into());
        }
        capture::start(tx)
    }
    fn stop(&self) {
        capture::stop();
    }
}

pub use drag_target::fallback_dir;
