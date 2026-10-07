//! NSPasteboard for text, HTML, RTF, and PNG. File promises are in `clipboard_files`.

use objc2::rc::Retained;
use objc2_app_kit::{
    NSPasteboard, NSPasteboardTypeHTML, NSPasteboardTypePNG, NSPasteboardTypeRTF,
    NSPasteboardTypeString,
};
use objc2_foundation::{NSData, NSString};

use crate::ClipboardItem;

pub fn read_items() -> Vec<ClipboardItem> {
    let mut items = Vec::new();
    unsafe {
        let board = NSPasteboard::generalPasteboard();
        if let Some(text) = board.stringForType(NSPasteboardTypeString) {
            items.push(ClipboardItem::Text(text.to_string()));
        }
        if let Some(html) = board.stringForType(NSPasteboardTypeHTML) {
            items.push(ClipboardItem::Html(html.to_string()));
        }
        if let Some(rtf) = board.dataForType(NSPasteboardTypeRTF) {
            items.push(ClipboardItem::Rtf(rtf.to_vec()));
        }
        if let Some(png) = board.dataForType(NSPasteboardTypePNG) {
            items.push(ClipboardItem::Png(png.to_vec()));
        }
    }
    items
}

pub fn write_items(items: &[ClipboardItem]) -> Result<(), String> {
    unsafe {
        let board = NSPasteboard::generalPasteboard();
        let _ = board.clearContents();
        for item in items {
            let ok = match item {
                ClipboardItem::Text(text) => {
                    board.setString_forType(&NSString::from_str(text), NSPasteboardTypeString)
                }
                ClipboardItem::Html(html) => {
                    board.setString_forType(&NSString::from_str(html), NSPasteboardTypeHTML)
                }
                ClipboardItem::Rtf(bytes) => {
                    board.setData_forType(Some(&NSData::with_bytes(bytes)), NSPasteboardTypeRTF)
                }
                ClipboardItem::Png(bytes) => {
                    board.setData_forType(Some(&NSData::with_bytes(bytes)), NSPasteboardTypePNG)
                }
                ClipboardItem::Files(_) => true,
            };
            if !ok {
                return Err("pasteboard write failed".into());
            }
        }
    }
    Ok(())
}

pub fn read_text_retained() -> Option<Retained<NSString>> {
    unsafe { NSPasteboard::generalPasteboard().stringForType(NSPasteboardTypeString) }
}
