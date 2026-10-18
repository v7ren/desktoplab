//! Low-level hooks. The callback only enqueues; a slow hook is removed by Windows.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use crossbeam_channel::Sender;
use hop_core::{chord_matches, Chord, HotAction};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLKHF_UP, MSLLHOOKSTRUCT,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WINDOW_EX_STYLE, WM_DISPLAYCHANGE, WM_MOUSEHWHEEL, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_QUIT, WNDCLASSW,
};

use crate::local_keys::{LocalKeyState, MODIFIER_VKS};
use crate::park_clip::CLIP_SPAN as DEFAULT_PARK_SPAN;
use crate::remote_motion::should_forward_remote_motion;
use crate::windows::display::list;
use crate::InputEvent;

const LLMHF_INJECTED: u32 = 1;

struct Shared {
    tx: Sender<InputEvent>,
    hotkeys: ArcSwap<Vec<(Chord, HotAction)>>,
    remote: AtomicBool,
    park_x: AtomicI32,
    park_y: AtomicI32,
    last_x: AtomicI32,
    last_y: AtomicI32,
    local_keys: Mutex<LocalKeyState>,
    /// Monotonic ms when remote was enabled; 0 = not remote.
    remote_enabled_ms: AtomicU64,
}

static SHARED: OnceLock<Shared> = OnceLock::new();
static MONO_START: OnceLock<Instant> = OnceLock::new();
static PARK_SPAN: AtomicI32 = AtomicI32::new(DEFAULT_PARK_SPAN);

pub fn set_park_span(span: i32) {
    PARK_SPAN.store(span, Ordering::SeqCst);
}

fn park_span() -> i32 {
    PARK_SPAN.load(Ordering::SeqCst)
}
static THREAD: AtomicU32 = AtomicU32::new(0);
static MOUSE_HOOK: AtomicIsizePtr = AtomicIsizePtr::new();
static KEY_HOOK: AtomicIsizePtr = AtomicIsizePtr::new();

struct AtomicIsizePtr {
    value: std::sync::atomic::AtomicIsize,
}

impl AtomicIsizePtr {
    const fn new() -> Self {
        Self {
            value: std::sync::atomic::AtomicIsize::new(0),
        }
    }
    fn store(&self, hook: HHOOK) {
        self.value.store(hook.0 as isize, Ordering::SeqCst);
    }
    fn load(&self) -> HHOOK {
        HHOOK(self.value.load(Ordering::SeqCst) as *mut _)
    }
}

pub fn set_hotkeys(chords: Vec<(Chord, HotAction)>) {
    if let Some(shared) = SHARED.get() {
        shared.hotkeys.store(std::sync::Arc::new(chords));
    }
}

pub fn set_remote(enabled: bool, park_x: i32, park_y: i32) {
    if let Some(shared) = SHARED.get() {
        shared.park_x.store(park_x, Ordering::SeqCst);
        shared.park_y.store(park_y, Ordering::SeqCst);
        shared.last_x.store(park_x, Ordering::SeqCst);
        shared.last_y.store(park_y, Ordering::SeqCst);
        if enabled {
            shared
                .remote_enabled_ms
                .store(monotonic_ms(), Ordering::SeqCst);
            shared.remote.store(true, Ordering::SeqCst);
            let local_down = lock_local_keys(shared).local_down_snapshot();
            let modifiers = modifier_snapshot();
            tracing::info!(
                target: "devhop.platform",
                park_x,
                park_y,
                local_down = ?local_down,
                modifiers = ?modifiers,
                "set_remote(true)"
            );
            crate::windows::cursor::warp(park_x, park_y);
            let span = park_span();
            crate::windows::cursor::clip(Some((
                park_x.saturating_sub(span),
                park_y.saturating_sub(span),
                span * 2,
                span * 2,
            )));
            crate::windows::cursor::hide();
        } else {
            shared.remote.store(false, Ordering::SeqCst);
            shared.remote_enabled_ms.store(0, Ordering::SeqCst);
            let (local_down, releases) = {
                let keys = lock_local_keys(shared);
                (
                    keys.local_down_snapshot(),
                    keys.keys_to_release_on_remote_end(physical_key_down),
                )
            };
            let modifiers = modifier_snapshot();
            tracing::info!(
                target: "devhop.platform",
                local_down = ?local_down,
                modifiers = ?modifiers,
                releases = ?releases,
                "set_remote(false)"
            );
            for vk in releases {
                crate::windows::inject::key_vk(vk, false);
            }
            lock_local_keys(shared).clear();
            crate::windows::cursor::clip(None);
            crate::windows::cursor::show();
        }
    }
}

pub fn start(tx: Sender<InputEvent>) -> Result<(), String> {
    if THREAD.load(Ordering::SeqCst) != 0 {
        return Ok(());
    }
    let _ = SHARED.get_or_init(|| Shared {
        tx,
        hotkeys: ArcSwap::from_pointee(Vec::new()),
        remote: AtomicBool::new(false),
        park_x: AtomicI32::new(0),
        park_y: AtomicI32::new(0),
        last_x: AtomicI32::new(0),
        last_y: AtomicI32::new(0),
        local_keys: Mutex::new(LocalKeyState::default()),
        remote_enabled_ms: AtomicU64::new(0),
    });
    let (ready_tx, ready_rx) = mpsc::channel();
    thread::Builder::new()
        .name("devhop-hook".into())
        .spawn(move || hook_thread(ready_tx))
        .map_err(|err| err.to_string())?;
    ready_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .map_err(|_| "hook thread did not start".to_string())?
}

pub fn stop() {
    let id = THREAD.load(Ordering::SeqCst);
    if id != 0 {
        unsafe {
            let _ = PostThreadMessageW(id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
}

fn hook_thread(ready: mpsc::Sender<Result<(), String>>) {
    unsafe {
        let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), None, 0);
        let key = SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_proc), None, 0);
        match (mouse, key) {
            (Ok(mouse), Ok(key)) => {
                MOUSE_HOOK.store(mouse);
                KEY_HOOK.store(key);
                THREAD.store(
                    windows::Win32::System::Threading::GetCurrentThreadId(),
                    Ordering::SeqCst,
                );
                let _ = ready.send(Ok(()));
                let _ = install_display_window();
                let mut msg = windows::Win32::UI::WindowsAndMessaging::MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                let _ = UnhookWindowsHookEx(mouse);
                let _ = UnhookWindowsHookEx(key);
                THREAD.store(0, Ordering::SeqCst);
            }
            (Err(err), _) | (_, Err(err)) => {
                let _ = ready.send(Err(err.to_string()));
            }
        }
    }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(Some(MOUSE_HOOK.load()), code, wparam, lparam);
    }
    let Some(shared) = SHARED.get() else {
        return CallNextHookEx(Some(MOUSE_HOOK.load()), code, wparam, lparam);
    };
    let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
    if info.flags & LLMHF_INJECTED != 0 {
        return CallNextHookEx(Some(MOUSE_HOOK.load()), code, wparam, lparam);
    }
    let x = info.pt.x;
    let y = info.pt.y;
    let msg = wparam.0 as u32;
    let event = match msg {
        WM_MOUSEMOVE => {
            let remote = shared.remote.load(Ordering::SeqCst);
            let (dx, dy) = if remote {
                let park_x = shared.park_x.load(Ordering::SeqCst);
                let park_y = shared.park_y.load(Ordering::SeqCst);
                let ms_since = ms_since_remote_enabled(shared);
                if !should_forward_remote_motion(x, y, park_x, park_y, park_span(), ms_since) {
                    (0, 0)
                } else {
                    (x - park_x, y - park_y)
                }
            } else {
                (
                    x - shared.last_x.swap(x, Ordering::SeqCst),
                    y - shared.last_y.swap(y, Ordering::SeqCst),
                )
            };
            if remote && dx == 0 && dy == 0 {
                None
            } else {
                Some(InputEvent::Motion { x, y, dx, dy })
            }
        }
        WM_MOUSEWHEEL => Some(InputEvent::Wheel {
            dx: 0,
            dy: wheel_delta(info.mouseData),
        }),
        WM_MOUSEHWHEEL => Some(InputEvent::Wheel {
            dx: wheel_delta(info.mouseData),
            dy: 0,
        }),
        0x0201 => Some(InputEvent::Button {
            button: 0,
            down: true,
        }),
        0x0202 => Some(InputEvent::Button {
            button: 0,
            down: false,
        }),
        0x0204 => Some(InputEvent::Button {
            button: 1,
            down: true,
        }),
        0x0205 => Some(InputEvent::Button {
            button: 1,
            down: false,
        }),
        0x0207 => Some(InputEvent::Button {
            button: 2,
            down: true,
        }),
        0x0208 => Some(InputEvent::Button {
            button: 2,
            down: false,
        }),
        _ => None,
    };
    if let Some(event) = event {
        let _ = shared.tx.try_send(event);
    }
    if shared.remote.load(Ordering::SeqCst) {
        let px = shared.park_x.load(Ordering::SeqCst);
        let py = shared.park_y.load(Ordering::SeqCst);
        crate::windows::cursor::warp(px, py);
        shared.last_x.store(px, Ordering::SeqCst);
        shared.last_y.store(py, Ordering::SeqCst);
        return LRESULT(1);
    }
    CallNextHookEx(Some(MOUSE_HOOK.load()), code, wparam, lparam)
}

unsafe extern "system" fn key_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(Some(KEY_HOOK.load()), code, wparam, lparam);
    }
    let Some(shared) = SHARED.get() else {
        return CallNextHookEx(Some(KEY_HOOK.load()), code, wparam, lparam);
    };
    let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
    if info.flags.contains(LLKHF_UP) && info.flags.contains(LLKHF_INJECTED) {
        return CallNextHookEx(Some(KEY_HOOK.load()), code, wparam, lparam);
    }
    let vk = info.vkCode as u16;
    let down = !info.flags.contains(LLKHF_UP);
    let injected = info.flags.contains(LLKHF_INJECTED);
    if injected {
        return CallNextHookEx(Some(KEY_HOOK.load()), code, wparam, lparam);
    }
    let remote = shared.remote.load(Ordering::SeqCst);
    if down {
        if let Some(hid) = crate::keys_util::from_vk(vk) {
            let (ctrl, alt, shift, gui) = modifiers();
            let table = shared.hotkeys.load();
            if let Some((_, action)) = table
                .iter()
                .find(|(chord, _)| chord_matches(chord, ctrl, alt, shift, gui, hid))
            {
                let _ = shared.tx.try_send(InputEvent::Hotkey(*action));
                return LRESULT(1);
            }
            send_key_event(shared, InputEvent::Key { hid, down: true });
        }
    } else if let Some(hid) = crate::keys_util::from_vk(vk) {
        send_key_event(shared, InputEvent::Key { hid, down: false });
    }
    if remote {
        if !down {
            match shared.local_keys.try_lock() {
                Ok(mut keys) => {
                    if keys.should_pass_up_to_os_while_remote(vk) {
                        keys.on_passed_up(vk);
                        return CallNextHookEx(Some(KEY_HOOK.load()), code, wparam, lparam);
                    }
                }
                Err(_) => {
                    return CallNextHookEx(Some(KEY_HOOK.load()), code, wparam, lparam);
                }
            }
        }
        return LRESULT(1);
    }
    if let Ok(mut keys) = shared.local_keys.try_lock() {
        if down {
            keys.on_passed_down(vk);
        } else {
            keys.on_passed_up(vk);
        }
    }
    CallNextHookEx(Some(KEY_HOOK.load()), code, wparam, lparam)
}

fn monotonic_ms() -> u64 {
    MONO_START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

fn ms_since_remote_enabled(shared: &Shared) -> Option<u64> {
    let at = shared.remote_enabled_ms.load(Ordering::SeqCst);
    if at == 0 {
        None
    } else {
        Some(monotonic_ms().saturating_sub(at))
    }
}

fn lock_local_keys(shared: &Shared) -> MutexGuard<'_, LocalKeyState> {
    shared
        .local_keys
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn send_key_event(shared: &Shared, event: InputEvent) {
    let _ = shared.tx.send_timeout(event, Duration::from_millis(5));
}

fn physical_key_down(vk: u16) -> bool {
    unsafe { GetAsyncKeyState(i32::from(vk)) < 0 }
}

fn modifier_snapshot() -> Vec<(u16, bool)> {
    MODIFIER_VKS
        .iter()
        .map(|vk| (*vk, physical_key_down(*vk)))
        .collect()
}

fn modifiers() -> (bool, bool, bool, bool) {
    unsafe {
        let down = |vk: u16| GetAsyncKeyState(i32::from(vk)) < 0;
        (
            down(VK_CONTROL.0),
            down(VK_MENU.0),
            down(VK_SHIFT.0),
            down(VK_LWIN.0) || down(VK_RWIN.0),
        )
    }
}

fn wheel_delta(mouse_data: u32) -> i32 {
    (mouse_data >> 16) as i16 as i32
}

unsafe fn install_display_window() -> windows::core::Result<()> {
    let class_name: Vec<u16> = "DevHopDisplay"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let class = WNDCLASSW {
        lpfnWndProc: Some(display_proc),
        lpszClassName: windows::core::PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    let _ = windows::Win32::UI::WindowsAndMessaging::RegisterClassW(&class);
    let _ = windows::Win32::UI::WindowsAndMessaging::CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        windows::core::PCWSTR(class_name.as_ptr()),
        windows::core::PCWSTR(class_name.as_ptr()),
        Default::default(),
        0,
        0,
        0,
        0,
        Some(windows::Win32::UI::WindowsAndMessaging::HWND_MESSAGE),
        None,
        None,
        None,
    )?;
    Ok(())
}

unsafe extern "system" fn display_proc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_DISPLAYCHANGE {
        if let Some(shared) = SHARED.get() {
            let _ = shared.tx.try_send(InputEvent::Displays(list()));
        }
        return LRESULT(0);
    }
    windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, msg, wparam, lparam)
}
