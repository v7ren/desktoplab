# DevHop spec

> Status: **Implemented.** Windows library tests pass. macOS backends are in the tree and have not been compiled on this machine. Two-computer manual checks are listed in `docs/manual-tests.md`.

## Goal
A Rust + Tauri desktop app for Windows and macOS that matches **CursorHop**. One keyboard and mouse control several machines on the LAN: push the cursor off a screen edge and it lands on the next machine (a software KVM). The clipboard (including files) and file drag-and-drop work across machines and across OSes. On top of that, hotkeys move the cursor between monitors on one machine.

## Reference products (the features we are matching)
| Product | What we take from it |
|---|---|
| **CursorHop (primary target)** | Edge crossing between Windows and Mac, shared keyboard, copy and paste between OSes, **file drag-and-drop over the LAN**, mDNS discovery, end-to-end encryption, Rust with very low latency |
| Cursor Teleporter / LittleBigMouse | Hotkey jumps between monitors, smooth crossing between monitors with different DPI |
| Mouse Without Borders / Input Leap / Deskflow / Synergy | Edge crossing between machines, layout editor, clipboard sync, hotkey to jump to a machine |
| Apple Universal Control | Zero-config discovery, "push through the edge" resistance, drag-and-drop between machines |

---

## Feature set

### A. Local monitor hopping (one machine) — v1
1. **Hop to the next or previous monitor** with a hotkey. The cursor keeps its relative position, e.g. 30% from the left on monitor 1 lands 30% from the left on monitor 2.
2. **Hop to monitor N** directly (`Ctrl+Alt+1..9` by default).
3. **Position memory per monitor**: hopping back returns the cursor to where it last was on that monitor. You can choose relative, memory or center mode.
4. **Center hop**: put the cursor in the middle of the current or target monitor.
5. **DPI-aware crossing**: when monitors have different sizes or DPI, map the edge proportionally so the cursor doesn't snag on mismatched edges (the main LittleBigMouse feature).
6. **Edge wrap** (optional): leaving the far right edge comes back in on the far left.
7. **Find my cursor**: a hotkey or a mouse shake briefly shows a ring around the cursor.
8. **Lock cursor to the current monitor**: a toggle for games and full-screen apps (`ScrollLock` by default).

### B. Cross-device control (several machines) — v1
1. **LAN discovery** with mDNS (`_devhop._udp`). Devices on the network show up on their own.
2. **Pairing**: show a 6-digit code on both machines, confirm, then pin each other's certificate fingerprint (TOFU). Unpaired machines can never take control.
3. **Encrypted transport**: QUIC with TLS 1.3 (`quinn`). Mouse motion goes as unreliable datagrams; keys, buttons and clipboard go on reliable streams.
4. **Edge crossing**: push the cursor off a configured edge and it appears on the other machine. While remote, the local cursor is hidden and parked, and raw deltas are forwarded.
5. **Layout editor**: drag every machine's monitors into place on one canvas. The layout is shared with all peers.
6. **Hotkey jump to a machine** (`Ctrl+Alt+Shift+1..9`), plus a "return home" hotkey.
7. **Clipboard sync, between Windows and Mac**: plain text, rich text (HTML, plus RTF where both sides support it), images (sent as PNG and converted to each OS's native format) and **files and folders**. Copy files in Explorer, paste in Finder, and the reverse. Small items go when the cursor crosses. Files are announced when the cursor crosses but only transferred when you paste (lazy), using CF_HDROP and delayed rendering on Windows and file promises on macOS. If lazy paste isn't possible, the files are staged first.
8. **Key mapping**: map Ctrl↔Cmd and Win↔Option per peer pair. Shortcuts like copy and paste translate to the target's OS.
9. **Scroll normalization**: a per-device natural or reversed setting and speed multiplier, with high-resolution wheel deltas.
10. **Safety**: on disconnect or timeout, release every held key and button on both sides (no stuck keys). A panic hotkey (`Ctrl+Alt+Esc`) always returns control to the local machine.
11. **Auto-reconnect** with backoff, and a connection-state icon in the tray.
12. **File drag-and-drop across devices, between Windows and Mac**: start dragging files or folders on machine A, carry them across the edge with the button held, and drop them into any app or folder on machine B.
    - **Source side**: when the cursor reaches a peer edge with the left button held, read the drag payload.
      - Windows: a transparent OLE drop-target window sits at the edge and receives the `IDataObject` (CF_HDROP).
      - macOS: read `NSPasteboard(name: .drag)` for file URLs.
    - **Target side**: start a native drag under the remote cursor from an invisible helper window.
      - Windows: `DoDragDrop` with `CFSTR_FILEDESCRIPTORW` + `CFSTR_FILECONTENTS` (virtual files that stream on drop).
      - macOS: `NSDraggingSession` with `NSFilePromiseProvider`.
    - **Transfer**: chunked over a QUIC stream with a BLAKE3 check, plus progress in a toast or tray and a cancel button. Folder structure, file names (Unicode, including NFC↔NFD) and modified times are kept.
    - **Fallback**: if the drop target refuses the virtual or promised files, or you release over the desktop, the files land in `Downloads/DevHop/` and the folder opens.
13. **Transfer manager**: a list of active and recent transfers (clipboard files and drag-and-drop), with progress, speed, cancel and "show in folder".

### C. App shell — v1
1. **Tray app**: shows status, pause/resume and the list of peers.
2. **Tauri settings window**: Layout, Hotkeys, Devices, Clipboard and About pages.
3. **Start at login** (optional).
4. **macOS permissions onboarding**: guide the user through granting Accessibility and Input Monitoring, and detect when they are granted.
5. **Config** stored as TOML in `%APPDATA%\devhop\` or `~/Library/Application Support/devhop/`.
6. **Logs** in a rolling file, with a "copy diagnostics" button.

### D. Extra ideas (not in v1, ranked by value ÷ effort)
| # | Idea | Why |
|---|---|---|
| 1 | **CLI and local IPC** (`devhop jump --monitor 2`, `devhop jump --device laptop`) | Lets Stream Deck, AutoHotkey, Raycast and scripts drive it for free |
| 2 | **Push-through resistance**: the cursor has to press against an edge for N ms or N px before crossing | Stops accidental crossings, like Universal Control |
| 3 | **Profiles** (home, office), picked automatically from the Wi-Fi SSID or the attached monitor set | Laptop users change layouts every day |
| 4 | **Per-app exclusions**: no crossing while a full-screen game or chosen app has focus | Stops gaming accidents without the manual lock |
| 5 | **Hop to an app's window**: a hotkey sends the cursor to the monitor or machine where app X is focused | Faster than hunting for windows |
| 6 | **Resume interrupted file transfers** | Large files on flaky Wi-Fi |
| 7 | **Clipboard history across devices** | Small add-on on top of clipboard sync |
| 8 | **Shared lock and sleep**: locking one machine locks them all; Wake-on-LAN the target when you cross to it | Office hygiene and convenience |
| 9 | **Mouse-button gestures**: hold mouse 4 and flick to hop | Users without a free hand for the keyboard |
| 10 | **Latency HUD**: an overlay with RTT and packet loss per peer | Debugging and trust |
| 11 | **Cursor throw with momentum**: a fast flick carries the cursor across monitors | Delightful, cheap once (A) exists |
| 12 | **Phone companion as a trackpad and keyboard** | Large new surface; keep the protocol open for it |
| 13 | **Linux peer** (X11 first, Wayland via libei later) | The platform trait makes it possible |
| 14 | **Relay through the internet** (not just the LAN) | Needs NAT traversal, so it's a later project |

---

## Out of scope for v1
- Linux, mobile, and controlling over the internet (LAN only)
- Resuming interrupted transfers (a failed transfer restarts from zero)
- Drag-and-drop of things that aren't files (dragging text or images between apps); files and folders only
- Dropping into elevated (admin) apps on Windows (an OS limit)
- Controlling the login screen, the Windows secure desktop (UAC prompts) or macOS secure input fields. These are OS limits, and we document them.
- Screen sharing or video of any kind
- More than 8 peers in one layout
- Clipboard formats beyond plain text, HTML/RTF, images and files (for example app-private formats such as Office's internal ones)

## Requirements (EARS)
- **R1** When the user presses the next-monitor hotkey, the system shall move the cursor to the next monitor within 16 ms, keeping its relative position (or using memory or center mode if configured).
- **R2** When the cursor crosses between two local monitors with different DPI, the system shall map the exit point proportionally onto the entry edge.
- **R3** When the cursor reaches an edge mapped to a paired peer that is online, the system shall hand control to that peer, hide the local cursor and forward input until the cursor exits back.
- **R4** While controlling a peer on the LAN, the system shall keep input latency from motion to remote cursor move at or below 10 ms p95.
- **R5** When the connection to the controlled peer drops or misses three heartbeats (300 ms), the system shall release every injected key and button on the peer, return control to the local machine and show the cursor.
- **R6** When the panic hotkey is pressed, the system shall return control to the local machine immediately, whatever the network state.
- **R7** When an unpaired device connects, the system shall refuse all input messages until the user approves pairing on both machines.
- **R8** When the cursor crosses to a peer and clipboard sync is on, the system shall send the current clipboard text, rich text and images (up to 32 MB inline) to that peer. File lists are announced but not transferred yet.
- **R12** When the user pastes files on machine B that were copied on machine A, the system shall stream the files from A and finish the paste in B's file manager (Explorer or Finder) once the transfer completes.
- **R13** When the cursor crosses a peer edge with the left button held during a file drag, the system shall carry the drag to the peer and start a native drag of the same files under the remote cursor.
- **R14** When files are dropped on the target, the system shall transfer them with a BLAKE3 check and write them only after the check passes. Partial files are deleted on failure or cancel.
- **R15** While a transfer is running, the system shall show progress and speed and allow cancelling from both machines.
- **R16** If the drop target refuses the virtual or promised files, the system shall save them to `Downloads/DevHop/` and tell the user.
- **R9** When a modifier mapping is configured, the system shall translate key events before injecting them on the target.
- **R10** If macOS Accessibility or Input Monitoring permission is missing, the system shall show the onboarding screen and turn off capture until the permission is granted.
- **R11** When the monitor setup changes (plug, unplug, resolution or DPI), the system shall enumerate the displays again and update the layout within 1 s.

## Architecture
```
devhop/
├─ crates/
│  ├─ hop-core/       pure logic: layout geometry, edge mapping, hop modes, the control state machine (no OS, no network; unit-testable)
│  ├─ hop-proto/      wire messages (serde + postcard), version handshake, HID key codes
│  ├─ hop-platform/   traits Displays / CursorCtl / InputCapture / InputInject / Clipboard / DragSource / DragTarget
│  │   ├─ windows/    windows-rs: EnumDisplayMonitors, SetCursorPos, WH_MOUSE_LL/WH_KEYBOARD_LL, Raw Input, SendInput,
│  │   │              OLE clipboard + IDropTarget/IDataObject/DoDragDrop (CF_HDROP, FILEDESCRIPTOR/FILECONTENTS)
│  │   └─ macos/      core-graphics + objc2-app-kit: CGGetActiveDisplayList, CGWarpMouseCursorPosition, CGEventTap,
│  │                  CGEventPost, NSPasteboard, NSDraggingSession, NSFilePromiseProvider (AppKit calls on the main thread)
│  ├─ hop-transfer/   file manifest (tree, sizes, mtimes), chunked streaming, BLAKE3, staging dir, progress/cancel
│  ├─ hop-net/        quinn QUIC, rcgen self-signed identity, mdns-sd discovery, pairing, heartbeats
│  └─ hop-engine/     wires core + platform + net on dedicated threads; exposes an async API and event stream
└─ app/               Tauri v2 shell: tray, settings UI (TypeScript + Svelte), commands and events into hop-engine
```

**Key design decisions**
- **Coordinate model**: every display goes into one global layout in *physical pixels* plus a scale factor. Windows runs Per-Monitor DPI Aware v2; macOS points × backing scale. Edge mapping uses each monitor's normalized coordinates (0..1), so DPI mismatch never matters.
- **Canonical keys**: USB HID usage codes on the wire, each OS mapping its own codes to and from them. Never send OS virtual-key codes.
- **Hook threads stay tiny**: the Windows low-level hook callback only pushes to a lock-free channel and returns, because Windows silently removes hooks that are slower than `LowLevelHooksTimeout`. The same applies to the macOS event-tap callback, plus re-enabling the tap on `kCGEventTapDisabledByTimeout`.
- **Remote mode**: Windows clips the cursor, hides it and reads deltas through Raw Input. macOS calls `CGAssociateMouseAndMouseCursorPosition(false)` and reads event delta fields.
- **Channels**: one QUIC connection per peer. Input goes as datagrams plus a high-priority stream. Clipboard and control use their own stream. **Each file transfer gets its own low-priority stream**, so a 4 GB copy never delays mouse motion.
- **Encryption**: CursorHop uses Noise. We use QUIC with TLS 1.3 and pinned self-signed certs, which gives the same guarantees (mutual auth, forward secrecy), and we get congestion control for file transfers for free.
- **AppKit on the main thread**: macOS pasteboard and drag APIs have to run on the main thread, which Tauri owns. Platform code dispatches through `tauri::AppHandle::run_on_main_thread`, behind a trait so hop-platform stays free of Tauri.
- **Engine and UI split**: hop-engine has no dependency on Tauri. That keeps a future CLI or daemon, and the IPC idea (D1), cheap.

## Data
- **Inputs**: local mouse and keyboard events, display configuration, clipboard, user settings, peer messages.
- **Outputs**: cursor warps, injected input on peers, clipboard writes, UI events.
- **Storage**:
  - `config.toml`: hotkeys, hop mode, layout, per-peer settings (mapping, scroll)
  - `identity.key` / `identity.crt`: this device's keypair (file permissions 0600 / user-only ACL)
  - `peers.toml`: paired peers (id, name, cert fingerprint, last address)
  - `logs/devhop.log`: rolling, 5 × 5 MB
  - `staging/`: incoming files that haven't been verified yet (cleared at startup)
  - `Downloads/DevHop/`: fallback location for files whose drop failed

## Assumptions (risky ones in **bold**)
- **Unsigned dev builds on macOS lose their Accessibility grant on every rebuild.** Day-to-day development needs a stable code-signing identity, even a self-signed one.
- **Shipping on macOS outside the App Store needs a paid Apple Developer ID for notarization.** The App Store sandbox would block event taps.
- **Windows can't inject input into elevated (admin) windows unless DevHop itself runs elevated.** v1 documents this rather than shipping an elevated helper.
- **Drag-and-drop between machines is the riskiest feature.** Picking up a drag that's already in progress at the screen edge, then re-creating it under the remote cursor with virtual or promised files, uses fiddly OLE and AppKit APIs. Some apps don't accept virtual files (FILECONTENTS) or file promises, which is why the `Downloads/DevHop` fallback is required, not optional.
- **Lazy file paste needs a delayed-render clipboard on Windows and file promises on macOS.** Some apps may read the files before the transfer finishes. Fallback: stage the whole transfer first, then put real paths on the clipboard.
- Peers sit on the same L2/L3 LAN and mDNS isn't blocked. A manual "add by IP" fallback covers networks that block it.
- Frontend: Svelte + TypeScript inside Tauri (swappable for React with no impact on the spec).
- Rust stable, edition 2021, MSRV = latest stable at project start.

## Done when
- [ ] Every R1 to R16 has an automated test (hop-core and hop-proto) or a written manual test script that passes on Windows 11 and macOS 14+.
- [ ] A Windows machine and a Mac pair, cross edges both ways, sync text and images, and survive pulling the network cable without stuck keys.
- [ ] Copy a folder of 100 files plus one 2 GB file in Explorer, paste it in Finder (and the reverse), and the BLAKE3 checks match.
- [ ] Drag files from Explorer onto the Mac desktop and into Finder, and from Finder into an Explorer folder, and they arrive intact. Dropping into an app that refuses virtual files falls back to `Downloads/DevHop`.
- [ ] Mouse motion stays smooth (no p95 regression) while a 2 GB transfer is running.
- [ ] Local hops work on a setup of 3 monitors with mixed DPI on each OS.
- [ ] `cargo test --workspace` and `cargo clippy -D warnings` are green in CI on windows-latest and macos-latest.
- [ ] Installable `.msi` and `.dmg` artifacts come out of CI.
