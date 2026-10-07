//! CF_HDROP read, and a clipboard owner that delays rendering until the files exist.
//!
//! Delayed rendering is how a paste into Explorer waits for a transfer (R12).
//! If the owner window cannot be created, the caller stages the files first and
//! puts real paths on the clipboard.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Mutex;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::System::Ole::CF_HDROP;
use windows::Win32::UI::Shell::{DragQueryFileW, DROPFILES, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, RegisterClassW, HWND_MESSAGE, WINDOW_EX_STYLE,
    WM_RENDERALLFORMATS, WM_RENDERFORMAT, WNDCLASSW,
};

struct Pending {
    request: Sender<()>,
    ready: Receiver<Vec<PathBuf>>,
}

static PENDING: Mutex<Option<Pending>> = Mutex::new(None);
static OWNER: Mutex<Option<isize>> = Mutex::new(None);

pub fn read_files() -> Vec<PathBuf> {
    unsafe {
        if OpenClipboard(None).is_err() {
            return Vec::new();
        }
        let files = if IsClipboardFormatAvailable(CF_HDROP.0 as u32).is_ok() {
            GetClipboardData(CF_HDROP.0 as u32)
                .ok()
                .map(|handle| paths_from_hdrop(HDROP(handle.0)))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let _ = CloseClipboard();
        files
    }
}

pub fn offer_real_files(paths: &[PathBuf]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    unsafe {
        OpenClipboard(None).map_err(|err| err.to_string())?;
        let _ = EmptyClipboard();
        let handle = hdrop_from_paths(paths).map_err(|err| err.to_string())?;
        SetClipboardData(CF_HDROP.0 as u32, Some(HANDLE(handle.0)))
            .map_err(|err| err.to_string())?;
        let _ = CloseClipboard();
    }
    Ok(())
}

/// Arm delayed rendering. When an app pastes, `on_paste` is signaled and this
/// thread blocks until `supply` receives the staged paths.
pub fn arm_delayed(on_paste: Sender<()>, supply: Receiver<Vec<PathBuf>>) -> Result<(), String> {
    *PENDING.lock().expect("pending") = Some(Pending {
        request: on_paste,
        ready: supply,
    });
    ensure_owner()?;
    unsafe {
        let bits = OWNER.lock().expect("owner").unwrap_or(0);
        let owner = HWND(bits as *mut std::ffi::c_void);
        OpenClipboard(Some(owner)).map_err(|err| err.to_string())?;
        let _ = EmptyClipboard();
        // NULL data means "ask me later" (WM_RENDERFORMAT).
        SetClipboardData(CF_HDROP.0 as u32, None).map_err(|err| err.to_string())?;
        let _ = CloseClipboard();
    }
    Ok(())
}

fn ensure_owner() -> Result<(), String> {
    if OWNER.lock().expect("owner").is_some() {
        return Ok(());
    }
    std::thread::Builder::new()
        .name("devhop-clip".into())
        .spawn(|| unsafe { owner_thread() })
        .map_err(|err| err.to_string())?;
    for _ in 0..50 {
        if OWNER.lock().expect("owner").is_some() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Err("clipboard owner did not start".into())
}

unsafe fn owner_thread() {
    let class_name: Vec<u16> = "DevHopClip"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let class = WNDCLASSW {
        lpfnWndProc: Some(owner_proc),
        lpszClassName: windows::core::PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    let _ = RegisterClassW(&class);
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        windows::core::PCWSTR(class_name.as_ptr()),
        windows::core::PCWSTR(class_name.as_ptr()),
        Default::default(),
        0,
        0,
        0,
        0,
        Some(HWND_MESSAGE),
        None,
        None,
        None,
    );
    if let Ok(hwnd) = hwnd {
        *OWNER.lock().expect("owner") = Some(hwnd.0 as isize);
        let mut msg = windows::Win32::UI::WindowsAndMessaging::MSG::default();
        while windows::Win32::UI::WindowsAndMessaging::GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&msg);
            windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn owner_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_RENDERFORMAT || msg == WM_RENDERALLFORMATS {
        render_now();
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn render_now() {
    let pending = PENDING.lock().expect("pending").take();
    let Some(pending) = pending else {
        return;
    };
    let _ = pending.request.send(());
    let paths = pending
        .ready
        .recv_timeout(std::time::Duration::from_secs(120))
        .unwrap_or_default();
    if paths.is_empty() {
        return;
    }
    unsafe {
        if let Ok(handle) = hdrop_from_paths(&paths) {
            let _ = SetClipboardData(CF_HDROP.0 as u32, Some(HANDLE(handle.0)));
        }
    }
}

pub fn paths_from_hdrop(drop: HDROP) -> Vec<PathBuf> {
    unsafe {
        let count = DragQueryFileW(drop, 0xFFFF_FFFF, None);
        let mut out = Vec::with_capacity(count as usize);
        for index in 0..count {
            let len = DragQueryFileW(drop, index, None) as usize;
            let mut buf = vec![0u16; len + 1];
            DragQueryFileW(drop, index, Some(&mut buf));
            if let Some(end) = buf.iter().position(|c| *c == 0) {
                buf.truncate(end);
            }
            out.push(PathBuf::from(String::from_utf16_lossy(&buf)));
        }
        out
    }
}

fn hdrop_from_paths(paths: &[PathBuf]) -> windows::core::Result<HGLOBAL> {
    let mut wide_bytes = Vec::new();
    for path in paths {
        let text = path_to_string(path);
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        wide.push(0);
        for unit in &wide {
            wide_bytes.extend_from_slice(&unit.to_le_bytes());
        }
    }
    wide_bytes.extend_from_slice(&0u16.to_le_bytes());
    let header = size_header();
    let total = header + wide_bytes.len();
    unsafe {
        let handle = GlobalAlloc(GMEM_MOVEABLE, total)?;
        let ptr = GlobalLock(handle);
        if ptr.is_null() {
            let _ = GlobalFree(Some(handle));
            return Err(windows::core::Error::from_win32());
        }
        let drop = DROPFILES {
            pFiles: header as u32,
            pt: Default::default(),
            fNC: false.into(),
            fWide: true.into(),
        };
        std::ptr::write(ptr as *mut DROPFILES, drop);
        std::ptr::copy_nonoverlapping(
            wide_bytes.as_ptr(),
            (ptr as *mut u8).add(header),
            wide_bytes.len(),
        );
        let _ = GlobalUnlock(handle);
        let _ = GlobalSize(handle);
        Ok(handle)
    }
}

fn size_header() -> usize {
    std::mem::size_of::<DROPFILES>()
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[allow(dead_code)]
pub fn channel() -> (Sender<Vec<PathBuf>>, Receiver<Vec<PathBuf>>) {
    mpsc::channel()
}
