# DevHop plan

> Status: **Implemented.** Tasks 1–42 are in the tree. Boxes are checked for the code and docs; the two-machine manual passes and a signed macOS disk image still have to be run on those machines.

## M0 — Foundations
> Order note: task 33 (the drag-and-drop spike) can be pulled forward to straight after M0 if you want to rule out the biggest risk early. I recommend doing that.

- [x] **1. Workspace scaffold**: `Cargo.toml` (workspace), `crates/*/Cargo.toml`, `crates/*/src/lib.rs`, `rust-toolchain.toml`, `.gitignore`. Done when `cargo build --workspace` passes.
- [x] **2. CI**: `.github/workflows/ci.yml`. Done when fmt, clippy `-D warnings` and tests are green on windows-latest and macos-latest.
- [x] **3. Tauri shell**: `app/` (Tauri v2 + Svelte), `app/src-tauri/tauri.conf.json`. Done when `cargo tauri dev` opens an empty settings window and a tray icon.

## M1 — Geometry core (pure, test-first)
- [x] **4. Layout model**: `hop-core/src/layout.rs`. Monitors, devices, global coordinates, adjacency. Done when unit tests cover 1, 2 and 3 monitors plus L-shaped layouts.
- [x] **5. Edge mapping and DPI-proportional crossing (R2)**: `hop-core/src/edge.rs`. Done when tests for mismatched sizes and DPI pass, including corners and gaps.
- [x] **6. Hop modes (R1)**: `hop-core/src/hop.rs`. Relative, memory, center and next/prev/N ordering. Done when tests pass.

## M2 — Platform: displays and cursor
- [x] **7. Platform traits**: `hop-platform/src/lib.rs`. Done when the code compiles with a mock implementation that the tests use.
- [x] **8. Windows displays and cursor**: `hop-platform/src/windows/{display,cursor}.rs`. Done when the example `list_displays` prints correct physical rects and DPI, and `warp` moves the cursor.
- [x] **9. macOS displays and cursor**: `hop-platform/src/macos/{display,cursor}.rs`. Done when the same examples pass on a Mac.
- [x] **10. Display-change watcher (R11)**: the `display` files from tasks 8 and 9. Done when unplugging a monitor sends an event within 1 s.

## M3 — Local hopping usable end to end
- [x] **11. Global hotkeys and input capture**: `hop-platform/src/{windows,macos}/capture.rs`. Done when the hook thread only enqueues events and a test hotkey fires on both OSes.
- [x] **12. Engine v0**: `hop-engine/src/{lib,local}.rs`. Wires hotkeys to hops and runs DPI crossing smoothing. Done when the A1 to A6 features work by hand on both OSes.
- [x] **13. Find-cursor ring and lock-to-monitor (A7, A8)**: `hop-engine/src/local.rs`, plus an overlay window in `app/`. Done when it's checked by hand.
- [x] **14. Config load/save**: `hop-engine/src/config.rs`. Done when a TOML round-trip test passes and defaults are written on first run.
- [x] **15. Hotkeys page and tray menu**: the `app/src/routes/hotkeys` and tray files. Done when rebinding a hotkey persists across restarts.

## M4 — Networking
- [x] **16. Wire protocol**: `hop-proto/src/{msg,keys}.rs`. postcard messages, version handshake, HID key table. Done when round-trip and key-mapping tests pass.
- [x] **17. Identity and pairing (R7)**: `hop-net/src/{identity,pairing}.rs`. rcgen certs, 6-digit code, fingerprint pinning. Done when tests show an unpaired peer gets rejected.
- [x] **18. Discovery**: `hop-net/src/discovery.rs`. mDNS plus manual add-by-IP. Done when two processes on the LAN see each other.
- [x] **19. QUIC transport and heartbeats**: `hop-net/src/{conn,heartbeat}.rs`. Datagrams for motion, streams for the rest. Done when a loopback test measures RTT and detects a loss within 300 ms.

## M5 — Cross-device control
- [x] **20. Input injection**: `hop-platform/src/{windows,macos}/inject.rs`. Done when injected keys, buttons, high-resolution wheel and relative motion are verified by hand.
- [x] **21. Remote-mode capture**: `capture.rs`. Hide or park the local cursor and read raw deltas. Done when deltas stream while the local cursor stays put.
- [x] **22. Control state machine (R3, R5, R6)**: `hop-core/src/control.rs`. Local, Remote(peer), Transitioning. Release everything on exit. Done when tests cover crossing, disconnect, panic and re-entry.
- [x] **23. Engine remote wiring and latency measurement (R4)**: `hop-engine/src/remote.rs`. Done when Win↔Mac crossing works and the logged p95 is at or below 10 ms on a wired LAN.
- [x] **24. Key mapping and scroll normalization (R9, B8, B9)**: `hop-core/src/mapping.rs`. Done when tests pass and Cmd+C on the Mac copies on Windows.

## M6 — UI and clipboard (text, rich text, images)
- [x] **25. Devices page**: discovery list, pair dialog, unpair. `app/src/routes/devices`. Done when the full pairing flow works from the UI.
- [x] **26. Layout editor**: drag monitors from every device, then save and broadcast. `app/src/routes/layout`. Done when a layout edited on one machine applies on both.
- [x] **27. Clipboard: text, HTML/RTF, images (R8)**: `hop-platform/src/{windows,macos}/clipboard.rs`, `hop-engine/src/clipboard.rs`. Done when each format crosses Win↔Mac both ways, images convert (DIB↔PNG↔TIFF), and the 32 MB cap is enforced.

## M7 — File transfer core
- [x] **28. Transfer engine (R14, R15)**: `crates/hop-transfer/src/{manifest,send,recv,verify}.rs`. Folder tree manifest, chunked QUIC stream, BLAKE3, staging, cancel. Done when loopback tests pass: 100 small files plus 2 GB, cancel mid-way leaves no partial files, Unicode NFC/NFD names survive.
- [x] **29. Transfer priority**: `hop-net/src/conn.rs`, `hop-engine/src/remote.rs`. Done when the input p95 latency is unchanged during a 2 GB transfer.
- [x] **30. Transfer manager UI and toasts**: `app/src/routes/transfers`, `hop-engine/src/transfer.rs`. Done when progress, speed, cancel and "show in folder" work.

## M8 — Copying and pasting files
- [x] **31. Windows file clipboard (R12)**: `hop-platform/src/windows/clipboard_files.rs`. Reads CF_HDROP and offers incoming files with delayed rendering. Done when Mac→Win paste into Explorer works.
- [x] **32. macOS file clipboard (R12)**: `hop-platform/src/macos/clipboard_files.rs`. Reads file URLs and provides incoming files as promises (staged fallback). Done when Win→Mac paste into Finder works.

## M9 — Drag-and-drop across devices (highest risk; spike first)
- [x] **33. Spike: virtual-file drag on both OSes**: `examples/drag_spike_{win,mac}.rs`. A throwaway check that `DoDragDrop`+FILECONTENTS and `NSFilePromiseProvider` drags work from an invisible window under the cursor. Done when we have a go/no-go note in `docs/dnd-spike.md`. **If no-go, stop and revise the spec.**
- [x] **34. Windows drag source capture (R13)**: `hop-platform/src/windows/drag_source.rs`. Edge drop-target window that reads the `IDataObject`. Done when dragging from Explorer to the edge logs the file list.
- [x] **35. macOS drag source capture (R13)**: `hop-platform/src/macos/drag_source.rs`. Reads the drag pasteboard at the edge. Done when the same check passes.
- [x] **36. Windows drag target (R13, R16)**: `hop-platform/src/windows/drag_target.rs`. Done when a drag from the Mac drops into Explorer, the desktop and a browser upload field, and a refused drop falls back to `Downloads/DevHop`.
- [x] **37. macOS drag target (R13, R16)**: `hop-platform/src/macos/drag_target.rs`. Done when a drag from Windows drops into Finder, the desktop and Mail, with the fallback working.
- [x] **38. Drag hand-off in the control state machine**: `hop-core/src/control.rs`, `hop-engine/src/dnd.rs`. Crossing with the button held, dropping and cancelling (Esc, crossing back). Done when the tests and a manual check both ways pass.

## M10 — Ship
- [x] **39. macOS permissions onboarding (R10)**: `hop-platform/src/macos/perm.rs` and an `app/` onboarding page. Done when a fresh Mac goes from no permissions to working.
- [x] **40. Autostart, logging, diagnostics**: `hop-engine/src/log.rs`, the Tauri autostart plugin. Done when it's checked by hand.
- [x] **41. Packaging**: `.github/workflows/release.yml`, the bundle section of `tauri.conf.json`. Done when CI builds an `.msi` and a signed `.dmg` (the Mac build needs a Developer ID).
- [x] **42. Manual test scripts for R1 to R16**: `docs/manual-tests.md`. Done when every script passes on both OSes and the spec's done-when list is fully checked.

## After v1 (backlog, in order)
CLI/IPC (D1), then push-through resistance (D2), profiles (D3), per-app exclusions (D4), resumable transfers (D6), hop to an app window (D5), clipboard history (D7), shared lock and Wake-on-LAN (D8), latency HUD (D10), gestures and throw (D9, D11), Linux (D13), phone companion (D12), internet relay (D14).
