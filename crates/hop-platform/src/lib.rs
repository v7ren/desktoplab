//! OS backends behind one set of traits. `install` picks Windows or macOS.
//! Tests use [`MockHost`], which never touches the real cursor.

pub mod keys_util;

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "macos")]
pub mod macos;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crossbeam_channel::Sender;
use hop_core::{Chord, HotAction};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub id: String,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub primary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardItem {
    Text(String),
    Html(String),
    Rtf(Vec<u8>),
    Png(Vec<u8>),
    Files(Vec<PathBuf>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    Motion { x: i32, y: i32, dx: i32, dy: i32 },
    Button { button: u8, down: bool },
    Key { hid: u16, down: bool },
    Wheel { dx: i32, dy: i32 },
    Hotkey(HotAction),
    Displays(Vec<DisplayInfo>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionStatus {
    pub accessibility: bool,
    pub input_monitoring: bool,
}

impl PermissionStatus {
    pub fn ok() -> Self {
        Self {
            accessibility: true,
            input_monitoring: true,
        }
    }

    pub fn granted(self) -> bool {
        self.accessibility && self.input_monitoring
    }
}

/// What the engine needs from an operating system. Every method returns quickly;
/// the Windows hook callback only enqueues and returns.
pub trait Host: Send + Sync {
    fn list_displays(&self) -> Vec<DisplayInfo>;
    fn warp(&self, x: i32, y: i32);
    fn cursor_pos(&self) -> Option<(i32, i32)>;
    fn hide_cursor(&self);
    fn show_cursor(&self);
    fn clip_rect(&self, rect: Option<(i32, i32, i32, i32)>);
    fn set_remote(&self, enabled: bool, park_x: i32, park_y: i32);
    fn set_hotkeys(&self, chords: Vec<(Chord, HotAction)>);
    fn inject_rel(&self, dx: i32, dy: i32);
    fn inject_button(&self, button: u8, down: bool);
    fn inject_key(&self, hid: u16, down: bool);
    fn inject_wheel(&self, dx: i32, dy: i32);
    fn release_inputs(&self);
    fn clipboard_read(&self) -> Vec<ClipboardItem>;
    fn clipboard_write(&self, items: &[ClipboardItem]) -> Result<(), String>;
    fn clipboard_files(&self) -> Vec<PathBuf>;
    fn clipboard_offer_files(&self, paths: &[PathBuf]) -> Result<(), String>;
    fn read_edge_drag(&self) -> Vec<PathBuf>;
    fn begin_file_drag(&self, paths: &[PathBuf]) -> Result<(), String>;
    fn permissions(&self) -> PermissionStatus;
    fn prompt_permissions(&self);
    fn start(&self, tx: Sender<InputEvent>) -> Result<(), String>;
    fn stop(&self);
}

#[derive(Clone, Debug)]
pub struct MockHost {
    inner: Arc<Mutex<MockState>>,
}

#[derive(Clone, Debug)]
pub struct MockState {
    pub displays: Vec<DisplayInfo>,
    pub cursor: (i32, i32),
    pub hidden: bool,
    pub remote: bool,
    pub injected_keys: Vec<(u16, bool)>,
    pub injected_buttons: Vec<(u8, bool)>,
    pub clipboard: Vec<ClipboardItem>,
    pub drag: Vec<PathBuf>,
    pub hotkeys: Vec<(Chord, HotAction)>,
}

impl MockHost {
    pub fn new(displays: Vec<DisplayInfo>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(MockState {
                displays,
                cursor: (0, 0),
                hidden: false,
                remote: false,
                injected_keys: Vec::new(),
                injected_buttons: Vec::new(),
                clipboard: Vec::new(),
                drag: Vec::new(),
                hotkeys: Vec::new(),
            })),
        }
    }

    pub fn state(&self) -> MockState {
        self.inner.lock().expect("mock").clone()
    }
}

impl Host for MockHost {
    fn list_displays(&self) -> Vec<DisplayInfo> {
        self.inner.lock().expect("mock").displays.clone()
    }
    fn warp(&self, x: i32, y: i32) {
        self.inner.lock().expect("mock").cursor = (x, y);
    }
    fn cursor_pos(&self) -> Option<(i32, i32)> {
        Some(self.inner.lock().expect("mock").cursor)
    }
    fn hide_cursor(&self) {
        self.inner.lock().expect("mock").hidden = true;
    }
    fn show_cursor(&self) {
        self.inner.lock().expect("mock").hidden = false;
    }
    fn clip_rect(&self, _rect: Option<(i32, i32, i32, i32)>) {}
    fn set_remote(&self, enabled: bool, park_x: i32, park_y: i32) {
        let mut g = self.inner.lock().expect("mock");
        g.remote = enabled;
        if enabled {
            g.cursor = (park_x, park_y);
            g.hidden = true;
        }
    }
    fn set_hotkeys(&self, chords: Vec<(Chord, HotAction)>) {
        self.inner.lock().expect("mock").hotkeys = chords;
    }
    fn inject_rel(&self, dx: i32, dy: i32) {
        let mut g = self.inner.lock().expect("mock");
        g.cursor.0 += dx;
        g.cursor.1 += dy;
    }
    fn inject_button(&self, button: u8, down: bool) {
        self.inner
            .lock()
            .expect("mock")
            .injected_buttons
            .push((button, down));
    }
    fn inject_key(&self, hid: u16, down: bool) {
        self.inner
            .lock()
            .expect("mock")
            .injected_keys
            .push((hid, down));
    }
    fn inject_wheel(&self, _dx: i32, _dy: i32) {}
    fn release_inputs(&self) {
        let mut g = self.inner.lock().expect("mock");
        let keys: Vec<u16> = g
            .injected_keys
            .iter()
            .filter(|(_, d)| *d)
            .map(|(k, _)| *k)
            .collect();
        for key in keys {
            g.injected_keys.push((key, false));
        }
        g.injected_buttons.clear();
    }
    fn clipboard_read(&self) -> Vec<ClipboardItem> {
        self.inner.lock().expect("mock").clipboard.clone()
    }
    fn clipboard_write(&self, items: &[ClipboardItem]) -> Result<(), String> {
        self.inner.lock().expect("mock").clipboard = items.to_vec();
        Ok(())
    }
    fn clipboard_files(&self) -> Vec<PathBuf> {
        self.inner
            .lock()
            .expect("mock")
            .clipboard
            .iter()
            .find_map(|item| {
                if let ClipboardItem::Files(paths) = item {
                    Some(paths.clone())
                } else {
                    None
                }
            })
            .unwrap_or_default()
    }
    fn clipboard_offer_files(&self, paths: &[PathBuf]) -> Result<(), String> {
        self.inner
            .lock()
            .expect("mock")
            .clipboard
            .push(ClipboardItem::Files(paths.to_vec()));
        Ok(())
    }
    fn read_edge_drag(&self) -> Vec<PathBuf> {
        self.inner.lock().expect("mock").drag.clone()
    }
    fn begin_file_drag(&self, paths: &[PathBuf]) -> Result<(), String> {
        self.inner.lock().expect("mock").drag = paths.to_vec();
        Ok(())
    }
    fn permissions(&self) -> PermissionStatus {
        PermissionStatus::ok()
    }
    fn prompt_permissions(&self) {}
    fn start(&self, _tx: Sender<InputEvent>) -> Result<(), String> {
        Ok(())
    }
    fn stop(&self) {}
}

pub fn install() -> Arc<dyn Host> {
    #[cfg(target_os = "windows")]
    {
        Arc::new(windows::WindowsHost::new())
    }
    #[cfg(target_os = "macos")]
    {
        Arc::new(macos::MacHost::new())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Arc::new(MockHost::new(Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_warp_and_clipboard_round_trip() {
        let host = MockHost::new(vec![DisplayInfo {
            id: "1".into(),
            name: "main".into(),
            x: 0,
            y: 0,
            width: 100,
            height: 100,
            scale: 1.0,
            primary: true,
        }]);
        host.warp(30, 40);
        assert_eq!(host.cursor_pos(), Some((30, 40)));
        host.clipboard_write(&[ClipboardItem::Text("hi".into())])
            .unwrap();
        assert_eq!(
            host.clipboard_read(),
            vec![ClipboardItem::Text("hi".into())]
        );
        host.set_remote(true, 5, 6);
        assert!(host.state().hidden);
        assert_eq!(host.cursor_pos(), Some((5, 6)));
    }
}
