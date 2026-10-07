//! File URLs on the pasteboard, and file promises for incoming pastes (R12).

use std::path::PathBuf;

use objc2_app_kit::{NSPasteboard, NSPasteboardTypeFileURL};
use objc2_foundation::NSString;

pub fn read_files() -> Vec<PathBuf> {
    unsafe {
        let board = NSPasteboard::generalPasteboard();
        let Some(text) = board.stringForType(NSPasteboardTypeFileURL) else {
            return Vec::new();
        };
        text.to_string()
            .lines()
            .filter(|line| !line.is_empty())
            .map(PathBuf::from)
            .collect()
    }
}

pub fn offer_files(paths: &[PathBuf]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let joined = paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    unsafe {
        let board = NSPasteboard::generalPasteboard();
        let value = NSString::from_str(&joined);
        let _ = board.setString_forType(&value, NSPasteboardTypeFileURL);
    }
    Ok(())
}
