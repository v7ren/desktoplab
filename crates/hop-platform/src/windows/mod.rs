//! Windows host. Hook installation, injection, clipboard, and drag.

mod capture;
mod clipboard;
mod clipboard_files;
mod cursor;
mod display;
mod drag_source;
mod drag_target;
mod inject;

use std::path::PathBuf;
use std::sync::Mutex;

use crossbeam_channel::Sender;
use hop_core::{Chord, HotAction};

use crate::{ClipboardItem, DisplayInfo, Host, InputEvent, PermissionStatus};

pub struct WindowsHost {
    hotkeys: Mutex<Vec<(Chord, HotAction)>>,
}

impl WindowsHost {
    pub fn new() -> Self {
        display::enable_per_monitor_dpi();
        Self {
            hotkeys: Mutex::new(Vec::new()),
        }
    }
}

impl Default for WindowsHost {
    fn default() -> Self {
        Self::new()
    }
}

impl Host for WindowsHost {
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

    fn clip_rect(&self, rect: Option<(i32, i32, i32, i32)>) {
        cursor::clip(rect);
    }

    fn set_remote(&self, enabled: bool, park_x: i32, park_y: i32) {
        capture::set_remote(enabled, park_x, park_y);
    }

    fn set_hotkeys(&self, chords: Vec<(Chord, HotAction)>) {
        *self.hotkeys.lock().expect("hotkeys") = chords.clone();
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
                clipboard_files::offer_real_files(paths)?;
            }
        }
        Ok(())
    }

    fn clipboard_files(&self) -> Vec<PathBuf> {
        clipboard_files::read_files()
    }

    fn clipboard_offer_files(&self, paths: &[PathBuf]) -> Result<(), String> {
        clipboard_files::offer_real_files(paths)
    }

    fn read_edge_drag(&self) -> Vec<PathBuf> {
        drag_source::last_files()
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
        PermissionStatus::ok()
    }

    fn prompt_permissions(&self) {}

    fn start(&self, tx: Sender<InputEvent>) -> Result<(), String> {
        capture::start(tx)?;
        capture::set_hotkeys(self.hotkeys.lock().expect("hotkeys").clone());
        Ok(())
    }

    fn stop(&self) {
        capture::stop();
        drag_source::hide_strip();
    }
}

pub use drag_source::{hide_strip, place_strip};
pub use drag_target::fallback_dir;
