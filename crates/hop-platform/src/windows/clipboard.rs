//! Text, HTML, RTF, and PNG clipboard. Files live in `clipboard_files`.

use std::mem::size_of;

use image::ImageFormat;
use windows::core::w;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_DIB;
use windows::Win32::System::Ole::CF_UNICODETEXT;

use crate::ClipboardItem;

pub fn read_items() -> Vec<ClipboardItem> {
    let mut items = Vec::new();
    unsafe {
        if OpenClipboard(None).is_err() {
            return items;
        }
        if let Some(text) = read_text() {
            items.push(ClipboardItem::Text(text));
        }
        if let Some(html) = read_format("HTML Format") {
            items.push(ClipboardItem::Html(extract_html(&html)));
        }
        if let Some(rtf) = read_format("Rich Text Format") {
            items.push(ClipboardItem::Rtf(rtf));
        }
        if let Some(png) = read_format("PNG") {
            items.push(ClipboardItem::Png(png));
        } else if let Some(png) = dib_as_png() {
            items.push(ClipboardItem::Png(png));
        }
        let _ = CloseClipboard();
    }
    items
}

pub fn write_items(items: &[ClipboardItem]) -> Result<(), String> {
    unsafe {
        OpenClipboard(None).map_err(|err| err.to_string())?;
        let _ = EmptyClipboard();
        for item in items {
            match item {
                ClipboardItem::Text(text) => set_text(text)?,
                ClipboardItem::Html(html) => set_bytes("HTML Format", &html_clipboard(html))?,
                ClipboardItem::Rtf(bytes) => set_bytes("Rich Text Format", bytes)?,
                ClipboardItem::Png(bytes) => set_bytes("PNG", bytes)?,
                ClipboardItem::Files(_) => {}
            }
        }
        let _ = CloseClipboard();
    }
    Ok(())
}

unsafe fn read_text() -> Option<String> {
    if IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).is_err() {
        return None;
    }
    let handle = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
    let ptr = GlobalLock(HGLOBAL(handle.0));
    if ptr.is_null() {
        return None;
    }
    let mut len = 0usize;
    let words = ptr as *const u16;
    while *words.add(len) != 0 {
        len += 1;
        if len > 16 * 1024 * 1024 {
            break;
        }
    }
    let text = String::from_utf16_lossy(std::slice::from_raw_parts(words, len));
    let _ = GlobalUnlock(HGLOBAL(handle.0));
    Some(text)
}

unsafe fn read_format(name: &str) -> Option<Vec<u8>> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let fmt = RegisterClipboardFormatW(windows::core::PCWSTR(wide.as_ptr()));
    if fmt == 0 {
        return None;
    }
    if IsClipboardFormatAvailable(fmt).is_err() {
        return None;
    }
    let handle = GetClipboardData(fmt).ok()?;
    let hglobal = HGLOBAL(handle.0);
    let ptr = GlobalLock(hglobal);
    if ptr.is_null() {
        return None;
    }
    let size = windows::Win32::System::Memory::GlobalSize(hglobal);
    let bytes = std::slice::from_raw_parts(ptr as *const u8, size).to_vec();
    let _ = GlobalUnlock(hglobal);
    Some(bytes)
}

unsafe fn dib_as_png() -> Option<Vec<u8>> {
    if IsClipboardFormatAvailable(CF_DIB.0 as u32).is_err() {
        return None;
    }
    let handle = GetClipboardData(CF_DIB.0 as u32).ok()?;
    let hglobal = HGLOBAL(handle.0);
    let ptr = GlobalLock(hglobal);
    if ptr.is_null() {
        return None;
    }
    let size = windows::Win32::System::Memory::GlobalSize(hglobal);
    let dib = std::slice::from_raw_parts(ptr as *const u8, size);
    let png = dib_to_png(dib);
    let _ = GlobalUnlock(hglobal);
    png
}

fn dib_to_png(dib: &[u8]) -> Option<Vec<u8>> {
    if dib.len() < 40 {
        return None;
    }
    let header_size = u32::from_le_bytes(dib[0..4].try_into().ok()?) as usize;
    if header_size < 40 || dib.len() < header_size {
        return None;
    }
    let pixel_offset = 14 + header_size;
    let file_size = 14 + dib.len();
    let mut bmp = Vec::with_capacity(file_size);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&(file_size as u32).to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&(pixel_offset as u32).to_le_bytes());
    bmp.extend_from_slice(dib);
    let image = image::load_from_memory_with_format(&bmp, ImageFormat::Bmp).ok()?;
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), ImageFormat::Png)
        .ok()?;
    Some(png)
}

unsafe fn set_text(text: &str) -> Result<(), String> {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
    set_raw(CF_UNICODETEXT.0 as u32, bytes)
}

unsafe fn set_bytes(format: &str, bytes: &[u8]) -> Result<(), String> {
    let wide: Vec<u16> = format.encode_utf16().chain(std::iter::once(0)).collect();
    let fmt = RegisterClipboardFormatW(windows::core::PCWSTR(wide.as_ptr()));
    if fmt == 0 {
        return Err(format!("unknown clipboard format {format}"));
    }
    set_raw(fmt, bytes)
}

unsafe fn set_raw(format: u32, bytes: &[u8]) -> Result<(), String> {
    let handle = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).map_err(|err| err.to_string())?;
    let ptr = GlobalLock(handle);
    if ptr.is_null() {
        let _ = GlobalFree(Some(handle));
        return Err("GlobalLock failed".into());
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
    let _ = GlobalUnlock(handle);
    SetClipboardData(format, Some(HANDLE(handle.0))).map_err(|err| err.to_string())?;
    let _ = size_of::<HANDLE>();
    let _ = w!("PNG");
    Ok(())
}

fn extract_html(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let start = text
        .find("<!--StartFragment-->")
        .map(|i| i + "<!--StartFragment-->".len());
    let end = text.find("<!--EndFragment-->");
    if let (Some(start), Some(end)) = (start, end) {
        if end > start {
            return text[start..end].to_string();
        }
    }
    text.into_owned()
}

fn html_clipboard(fragment: &str) -> Vec<u8> {
    let body = format!("<!--StartFragment-->{fragment}<!--EndFragment-->");
    let prefix = "Version:0.9\r\nStartHTML:00000000\r\nEndHTML:00000000\r\nStartFragment:00000000\r\nEndFragment:00000000\r\n";
    let html = format!("<html><body>{body}</body></html>");
    let start_html = prefix.len();
    let start_fragment = start_html + "<html><body>".len();
    let end_fragment = start_fragment + body.len();
    let end_html = start_html + html.len();
    let header = format!(
        "Version:0.9\r\nStartHTML:{start_html:08}\r\nEndHTML:{end_html:08}\r\nStartFragment:{start_fragment:08}\r\nEndFragment:{end_fragment:08}\r\n"
    );
    let mut out = header.into_bytes();
    out.extend_from_slice(html.as_bytes());
    out
}
