//! Tray, settings window, and the bridge into hop-engine.

use std::thread;

use hop_engine::{spawn, Cmd, EngineHandle, UiEvent};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager};

struct Eng(EngineHandle);

#[tauri::command]
fn snapshot(eng: tauri::State<'_, Eng>) -> hop_engine::Snapshot {
    eng.0.snapshot()
}

#[tauri::command]
fn command(eng: tauri::State<'_, Eng>, cmd: Cmd) {
    eng.0.command(cmd);
}

pub fn run() {
    let (engine, events) = spawn();
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_opener::init())
        .manage(Eng(engine))
        .invoke_handler(tauri::generate_handler![snapshot, command])
        .setup(move |app| {
            let show = MenuItem::with_id(app, "show", "Settings", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            let mut tray = TrayIconBuilder::with_id("devhop")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;
            let handle = app.handle().clone();
            thread::spawn(move || {
                while let Ok(event) = events.recv() {
                    match event {
                        UiEvent::FindCursor { x, y } => {
                            let _ =
                                handle.emit("find-cursor", serde_json::json!({ "x": x, "y": y }));
                        }
                        UiEvent::Status(text) => {
                            let _ = handle.emit("status", text);
                        }
                    }
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("devhop window failed");
}
