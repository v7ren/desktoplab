//! Windows display enumeration in physical pixels (Per-Monitor DPI Aware v2).

use std::mem::size_of;

use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI,
};

use crate::DisplayInfo;

pub fn enable_per_monitor_dpi() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

pub fn list() -> Vec<DisplayInfo> {
    enable_per_monitor_dpi();
    let mut out = Vec::new();
    unsafe {
        let data: *mut Vec<DisplayInfo> = &mut out;
        let _ = EnumDisplayMonitors(
            None::<HDC>,
            None::<*const RECT>,
            Some(enum_proc),
            LPARAM(data as isize),
        );
    }
    if !out.iter().any(|d| d.primary) {
        if let Some(first) = out.first_mut() {
            first.primary = true;
        }
    }
    out
}

unsafe extern "system" fn enum_proc(
    monitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    let list = &mut *(data.0 as *mut Vec<DisplayInfo>);
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    if GetMonitorInfoW(
        monitor,
        &mut info as *mut MONITORINFOEXW as *mut MONITORINFO,
    )
    .as_bool()
    {
        let rc = info.monitorInfo.rcMonitor;
        let mut dpi_x = 96u32;
        let mut dpi_y = 96u32;
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let name = device_name(&info.szDevice);
        let width = rc.right.saturating_sub(rc.left).max(0) as u32;
        let height = rc.bottom.saturating_sub(rc.top).max(0) as u32;
        list.push(DisplayInfo {
            id: name.clone(),
            name,
            x: rc.left,
            y: rc.top,
            width,
            height,
            scale: dpi_x as f64 / 96.0,
            primary: info.monitorInfo.dwFlags & 1 != 0,
        });
    }
    true.into()
}

fn device_name(raw: &[u16]) -> String {
    let end = raw.iter().position(|c| *c == 0).unwrap_or(raw.len());
    String::from_utf16_lossy(&raw[..end])
}
