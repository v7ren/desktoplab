//! A thin edge window that implements `IDropTarget` and records CF_HDROP paths
//! while a drag is still in progress (R13).

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Mutex;

use windows::core::{implement, BOOL};
use windows::Win32::Foundation::{HWND, POINT, POINTL};
use windows::Win32::System::Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
use windows::Win32::System::Ole::{
    IDropTarget, IDropTarget_Impl, RegisterDragDrop, RevokeDragDrop, CF_HDROP, DROPEFFECT,
    DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::HDROP;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, RegisterClassW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use super::clipboard_files::paths_from_hdrop;

static LAST_DRAG: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static EDGE: Mutex<Option<isize>> = Mutex::new(None);

pub fn last_files() -> Vec<PathBuf> {
    LAST_DRAG.lock().expect("drag").clone()
}

pub fn clear() {
    LAST_DRAG.lock().expect("drag").clear();
}

/// Park a 4px strip on the given screen edge. `side` is 0 left, 1 right, 2 top, 3 bottom.
pub fn place_strip(x: i32, y: i32, width: i32, height: i32) -> Result<(), String> {
    ensure_window()?;
    let hwnd = HWND(EDGE.lock().expect("edge").unwrap_or(0) as *mut _);
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowPos(
            hwnd,
            Some(windows::Win32::UI::WindowsAndMessaging::HWND_TOPMOST),
            x,
            y,
            width.max(4),
            height.max(4),
            windows::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE
                | windows::Win32::UI::WindowsAndMessaging::SWP_SHOWWINDOW,
        );
    }
    Ok(())
}

pub fn hide_strip() {
    if let Some(raw) = *EDGE.lock().expect("edge") {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                HWND(raw as *mut _),
                windows::Win32::UI::WindowsAndMessaging::SW_HIDE,
            );
        }
    }
}

fn ensure_window() -> Result<(), String> {
    if EDGE.lock().expect("edge").is_some() {
        return Ok(());
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("devhop-drop".into())
        .spawn(move || unsafe { drop_thread(tx) })
        .map_err(|err| err.to_string())?;
    let hwnd = rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .map_err(|_| "drop target did not start".to_string())??;
    *EDGE.lock().expect("edge") = Some(hwnd);
    Ok(())
}

unsafe fn drop_thread(ready: std::sync::mpsc::Sender<Result<isize, String>>) {
    let _ = windows::Win32::System::Ole::OleInitialize(None);
    let class_name: Vec<u16> = "DevHopDrop"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let class = windows::Win32::UI::WindowsAndMessaging::WNDCLASSW {
        lpfnWndProc: Some(drop_proc),
        lpszClassName: windows::core::PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    let _ = RegisterClassW(&class);
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
        windows::core::PCWSTR(class_name.as_ptr()),
        windows::core::PCWSTR(class_name.as_ptr()),
        WS_POPUP,
        0,
        0,
        4,
        4,
        None,
        None,
        None,
        None,
    );
    let Ok(hwnd) = hwnd else {
        let _ = ready.send(Err("CreateWindowExW failed".into()));
        return;
    };
    let target: IDropTarget = EdgeDrop.into();
    if let Err(err) = RegisterDragDrop(hwnd, &target) {
        let _ = ready.send(Err(err.to_string()));
        return;
    }
    let _ = ready.send(Ok(hwnd.0 as isize));
    let mut msg = windows::Win32::UI::WindowsAndMessaging::MSG::default();
    while windows::Win32::UI::WindowsAndMessaging::GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&msg);
        windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&msg);
    }
    let _ = RevokeDragDrop(hwnd);
}

unsafe extern "system" fn drop_proc(
    hwnd: HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

#[implement(IDropTarget)]
struct EdgeDrop;

impl IDropTarget_Impl for EdgeDrop_Impl {
    fn DragEnter(
        &self,
        pdataobj: windows::core::Ref<'_, IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        if let Some(obj) = pdataobj.as_ref() {
            if let Some(files) = files_from(obj) {
                *LAST_DRAG.lock().expect("drag") = files;
            }
        }
        unsafe {
            if !pdweffect.is_null() {
                *pdweffect = DROPEFFECT_COPY;
            }
        }
        Ok(())
    }

    fn DragOver(
        &self,
        grfkeystate: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let effect = if (grfkeystate.0 & MK_LBUTTON.0) != 0 {
            DROPEFFECT_COPY
        } else {
            DROPEFFECT_NONE
        };
        unsafe {
            if !pdweffect.is_null() {
                *pdweffect = effect;
            }
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn Drop(
        &self,
        pdataobj: windows::core::Ref<'_, IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        if let Some(obj) = pdataobj.as_ref() {
            if let Some(files) = files_from(obj) {
                *LAST_DRAG.lock().expect("drag") = files;
            }
        }
        unsafe {
            if !pdweffect.is_null() {
                *pdweffect = DROPEFFECT_COPY;
            }
        }
        Ok(())
    }
}

fn files_from(obj: &IDataObject) -> Option<Vec<PathBuf>> {
    unsafe {
        let format = FORMATETC {
            cfFormat: CF_HDROP.0,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        };
        let medium = obj.GetData(&format).ok()?;
        let hglobal = medium.u.hGlobal;
        let files = paths_from_hdrop(HDROP(hglobal.0));
        windows::Win32::System::Ole::ReleaseStgMedium(&medium as *const _ as *mut _);
        if files.is_empty() {
            None
        } else {
            Some(files)
        }
    }
}

#[allow(dead_code)]
fn _point(pt: POINT) -> (i32, i32) {
    (pt.x, pt.y)
}

#[allow(dead_code)]
fn _bool_unused(v: BOOL) -> bool {
    v.as_bool()
}
