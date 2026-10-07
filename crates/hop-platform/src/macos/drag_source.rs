//! Drag pasteboard. macOS exposes the in-progress drag on `NSPasteboardNameDrag`.

use std::path::PathBuf;

use objc2_app_kit::{NSPasteboard, NSPasteboardNameDrag};
use objc2_foundation::NSURL;

static LAST: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

pub fn read_drag() -> Vec<PathBuf> {
    let fresh = unsafe { urls_from_drag() };
    if !fresh.is_empty() {
        *LAST.lock().expect("drag") = fresh.clone();
    }
    LAST.lock().expect("drag").clone()
}

unsafe fn urls_from_drag() -> Vec<PathBuf> {
    let board = NSPasteboard::pasteboardWithName(NSPasteboardNameDrag);
    let class_array = objc2_foundation::NSArray::from_slice(&[NSURL::class()]);
    let Some(objects) = board.readObjectsForClasses_options(&class_array, None) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for object in objects.iter() {
        if let Some(url) = object.downcast_ref::<NSURL>() {
            if let Some(path) = url.path() {
                out.push(PathBuf::from(path.to_string()));
            }
        }
    }
    out
}
