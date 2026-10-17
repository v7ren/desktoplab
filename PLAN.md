# DevHop plan (v2)

> Status: **not working on two real machines.** Every crate compiles, `cargo test --workspace` is green, and two engines pair over loopback. On a Windows PC and a Mac on the same LAN the connection flaps: the log fills with `lost the connection to …` and `handshake with … failed: connection: closed by peer: bye`, and the cursor never crosses reliably. This plan replaces the v1 task list. The v1 tasks are listed at the end with what is actually verified.

The design the new milestones build toward is in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). Read that first.

## What is broken, and why (diagnosis, 2026-10-15)

Evidence: `%APPDATA%\devhop\logs\devhop.log` from the 2026-10-07/08 runs, the code in `crates/hop-engine/src/{netloop,runtime}.rs`, and a new loopback test that reproduces the flap.

1. **Duplicate connections replace healthy sessions, and the two sides pick different survivors.** `install()` in `netloop.rs` keeps the newest link whenever the new and the existing link have the same direction (both "preferred"). In real use each peer has two or three links to the other at once: the user clicks Pair on both machines (`begin_pair` dials), `discover()` dials by device id, and `AddIp` dials by address. `dialing` dedupes by key, and the device-id key and the address key are different, so the same peer is dialed twice. Whichever handshake finishes last wins, independently on each side. Side A keeps link 2 and closes link 1; side B keeps link 1 and closes link 2. Each side then sees its session die (`Internal::Closed`), reports `Offline`, redials, and the cycle repeats. The test `both_computers_dialing_at_once_settle_on_one_connection` (ignored for now) fails about one run in four on loopback, where handshakes take milliseconds. On a LAN the window is hundreds of milliseconds, so it fails most of the time. This is the main cause of the `bye` and `lost the connection` spam.

2. **The network loop awaits stream writes inline, so one slow write stops every heartbeat.** `handle_cmd` and `confirm` call `write_msg(..).await` inside the single `select!` loop. A `ClipboardChunk` is sent as one frame of up to 32 MiB on crossing. quinn's default stream receive window is 1.25 MB, so `write_all` blocks until the peer has acknowledged all but the last 1.25 MB. On Wi-Fi that is seconds. During that time no heartbeat datagram is sent and no internal event is processed. The peer sees 5 s of silence (`SESSION_TIMEOUT_MS`) and drops the session with `lost the connection`. Copying an image, then crossing, is enough to trigger this.

3. **The engine thread polls with an 8 ms sleep.** `engine_main` drains three channels with `try_recv` and sleeps 8 ms when idle. Every motion packet pays up to 8 ms on the controller and up to 8 ms again on the controlled machine before injection. R4 (10 ms p95) is unreachable with this loop. It also rebuilds and publishes a full `Snapshot` every iteration.

4. **The drag-and-drop target freezes the engine.** `StreamMsg::DragDrop` calls `host.begin_file_drag`, which waits up to 30 s for `DoDragDrop` to finish. While it waits the engine thread processes nothing, so the controller's motion is never injected and the drag cannot move. R13 cannot work in this shape.

5. **Any bidirectional stream the peer opens is treated as a file transfer.** `spawn_incoming` accepts every stream after the handshake and hands it to `recv_all`. There is no stream type tag, so a second control stream or a clipboard stream would be parsed as a transfer manifest.

6. **Discovery is fragile.** The beacon thread exits silently on any `recv_from` error other than a timeout; on Windows a UDP socket reports `ConnectionReset` after a broadcast hits a closed port, which kills both sending and receiving. The beacon port is fixed, so a second instance on one machine falls back to mDNS only. mDNS removal matches on a sanitized display name, not the device id. Test nodes (`aaa`, `bbb`, `ccc`, `ddd`, `device-beta`) have shown up on the real LAN; the new `discovery = false` config flag stops that.

7. **Smaller things that hide the real problems.** The Mac reports its name as `devhop` because `HOSTNAME` is not set for GUI apps. `on_cmd` writes `config.toml` on every command, including every pointer move in the layout editor. Logging from the net thread goes through the engine thread's event channel, so when the engine is busy or blocked the log is late or empty. `tracing-appender` is a dependency but the tracing subscriber writes to stderr, not the rolling file. The working tree was not formatted (`cargo fmt --check` failed; fixed in this pass).

## Ground rules for v2

- A task is checked only when its named verification has been run. "Code is in the tree" is not done.
- Two-machine checks are done on the real Windows PC and the real Mac, and the result (pass or fail, with the log tail) is written into `docs/manual-tests.md`.
- No new feature work until N0 to N3 pass. The feature code from v1 stays in the tree but is treated as untested.

## N0 — Make the failure visible
- [ ] **N0.1 File logging from every thread.** `hop-engine/src/log.rs`: route `tracing` to the rolling file through `tracing-appender`, with a per-connection id (`stable_id`) in every network line, and log from the net thread directly. Keep `NetEvt::Log` for the status line only. Done when a run on both machines produces a log that shows every dial, accept, handshake result, install, replace, close and the reason, with timestamps.
- [ ] **N0.2 Headless soak binary.** `crates/hop-engine/examples/soak.rs`: runs the engine with the mock host, no Tauri, pairs with a given address, forwards synthetic motion at 1 kHz for N seconds, prints RTT p50/p95, datagram loss, and every `Online`/`Offline`/`Stalled` event. Done when it runs on both machines against each other and the numbers are in the log.
- [x] **N0.3 Loopback soak test in CI.** `netloop.rs` test: two nodes, both dial each other, plus one extra dial by the other key, pair, then 30 s with motion and a 4 MB clipboard chunk every 5 s. Asserts zero `Offline`/`Stalled` and p95 under 5 ms. This is the un-ignored version of `both_computers_dialing_at_once_settle_on_one_connection`. It fails until N1 and N2 land, which is the point.

## N1 — One connection per peer, chosen the same way on both sides
- [x] **N1.1 First healthy link wins.** `install()`: if a session for the peer exists and its link is open, close the new link, always. The direction tie-break (`is_preferred`) applies only when both links are still in handshake, and it is decided from the device ids alone, so both sides agree. Done when N0.3 shows zero `Offline` across 20 runs.
- [ ] **N1.2 Dial at most once per peer.** Key `dialing` and `redial_after` by device id when known and by address otherwise, and look the address up in `known` before dialing, so an `AddIp` or `begin_pair` dial for a peer that already has a session or a dial in flight is a no-op. Done when the log of a double Pair click shows one dial per side.
- [x] **N1.3 Session state machine with tests.** Pull the per-peer bookkeeping out of `Net` into a new `hop-net::session` module: `Discovered → Dialing → Handshaking → Connected{unpaired|paired} → Closing`, pure, with the link-selection rule as a function of (existing state, new link direction, ids). Unit tests cover: both dial at once, the same side dials twice, a late handshake after pairing, a peer restarting with the same id and a new link. Done when the tests pass and `netloop.rs` only calls into it.
- [ ] **N1.4 Liveness from QUIC, not only datagrams.** Set `max_idle_timeout` to 3 s and `keep_alive_interval` to 500 ms in `conn.rs`, and treat `ConnectionError` from any reader as the session end. Keep the 100 ms datagram heartbeat for RTT and for the 300 ms `Stalled` signal while controlling (R5). Drop `SESSION_TIMEOUT_MS`. Done when pulling the network cable produces `Offline` within 3.5 s on both machines and the held key releases within 300 ms on the controlled side.

## N2 — Nothing in the network loop awaits I/O
- [ ] **N2.1 Per-session writer task.** Each session gets a bounded `mpsc` for control messages and a writer task that owns the `SendStream`. The main loop only pushes. A full channel drops motion-class messages and logs; it never blocks. Done when the loop body contains no `.await` on a stream.
- [x] **N2.2 Typed streams.** The first byte of every bidirectional stream is its kind: `1` control, `2` clipboard, `3` transfer. The accept task routes by kind. Exactly one control stream per link, opened by the dialer; the acceptor only accepts. Done when the loopback test opens a clipboard stream and a transfer stream at once and both arrive in the right handler.
- [x] **N2.3 Clipboard on its own stream, chunked.** `send_clipboard` opens a clipboard stream per payload and streams 64 KiB chunks from a task; the receiver reassembles per `(token, kind)`. The 32 MiB cap stays. Done when N0.3's 4 MB chunks cause no `Stalled` and the p95 is unchanged.
- [x] **N2.4 Heartbeats in their own task.** One `interval` task per session sends the heartbeat datagram and reads the echo for RTT. Done when the loopback test under a 1 kHz motion flood and a 2 GB transfer shows the heartbeat gap never above 150 ms.

## N3 — Event-driven engine, latency budget
- [x] **N3.1 Replace the 8 ms poll.** `engine_main` uses `crossbeam_channel::select!` over input, commands and net events, with no sleep. Net events arrive on a crossbeam channel instead of `std::sync::mpsc`. Done when the engine's own measured input-to-send time is under 0.5 ms p95.
- [x] **N3.2 Publish on change.** `publish()` runs when state changed and at most every 50 ms, not every iteration. Config is saved on a 500 ms debounce, not per command.
- [x] **N3.3 Non-blocking host calls.** `begin_file_drag`, `clipboard_read`, `clipboard_write` and `prompt_permissions` never block the engine thread: they run on a host worker thread and report back as `InputEvent`s (`DragFinished{accepted}`, `ClipboardRead{items}`). Done when the engine keeps injecting motion while a drag is in progress on the same machine.
- [ ] **N3.4 Measure R4 for real.** With N0.2 on both machines over a wired LAN: p95 at or below 10 ms. Write the numbers into `docs/manual-tests.md` R4. If it fails, the fix goes here, before any feature work.

## N4 — Pairing that cannot double-dial and tells the truth
- [x] **N4.1 Pairing driven by the session state machine.** `PairByCode` and `ConfirmPair` only ever call `Confirm` on an existing session or set `wanted_code`; dialing is discovery's job (and `AddIp`'s). Done when clicking Pair on both machines within a second pairs them on the first try, ten times in a row.
- [x] **N4.2 Status that matches the state.** One status line per peer derived from the state machine, not from the last log line. Done when the Devices page shows `discovered / connecting / unpaired (code …) / paired / offline` correctly through a full pair, unplug, replug cycle.
- [ ] **N4.3 Real device names.** Use `gethostname` on both OSes (the `hostname` crate, or `scutil --get ComputerName` on macOS). Done when the Mac shows as its own name in the Windows list.

## N5 — Discovery that survives real networks
- [ ] **N5.1 The beacon loop never exits on a recoverable error.** Treat `ConnectionReset`, `WouldBlock`, `TimedOut` and `Interrupted` as retryable; log and back off 1 s on anything else; never `break` silently. Done when a Windows run with a closed port on the LAN keeps discovering.
- [ ] **N5.2 Match mDNS removals by device id.** Store the mDNS fullname in `Seen` and match on it. Done when two peers with the same display name do not drop each other.
- [ ] **N5.3 A second instance on one machine.** Bind the beacon with `SO_REUSEADDR` so two instances can both listen; use `DEVHOP_HOME` and `port` for the rest. Done when the loopback soak can run with discovery on.
- [ ] **N5.4 Beacon spoofing is only a nuisance.** Keep the rule that a beacon never changes the address of a peer that has a live session, so a forged beacon cannot redirect an established peer. Document that an attacker on the LAN can still delay discovery.

## N6 — Two-machine acceptance (gate before any feature work)
- [ ] **N6.1** Pair Windows and Mac from the UI, both clicking Pair, first try.
- [ ] **N6.2** Cross both ways 50 times with no `Offline`; keys and buttons work; p95 recorded.
- [ ] **N6.3** Copy an 8 MB image, cross, paste on the other side, no `Stalled`.
- [ ] **N6.4** Pull the cable while holding a key: release within 300 ms, `Offline` within 3.5 s, reconnect on replug without clicking anything.
- [ ] **N6.5** Run for one hour idle: zero `Offline`, log under 1 MB.

## N7 — Then the v1 feature list, verified one by one
In this order, each gated on its manual test in `docs/manual-tests.md` being run on both machines and recorded: R8 clipboard formats, R9 mapping and scroll, R12 file clipboard, R14/R15 transfers and priority, R13/R16 drag-and-drop (with the N3.3 non-blocking target), R10 macOS permissions, R11 display changes, R1/R2 local hops on mixed DPI, packaging.

## v1 tasks and what is actually verified

| v1 tasks | Code in tree | Verified by |
|---|---|---|
| 1–3 scaffold, CI, Tauri shell | yes | CI compiles; UI opens on Windows |
| 4–6 geometry, edges, hop modes | yes | unit tests (hop-core) |
| 7–10 platform traits, displays, cursor, watcher | yes | Windows by hand; macOS never compiled on this machine |
| 11–15 capture, engine v0, find cursor, config, hotkeys UI | yes | Windows by hand; config round-trip test |
| 16–19 protocol, identity, discovery, QUIC | yes | unit and loopback tests; discovery not tested with two instances |
| 20–24 injection, remote capture, control machine, remote wiring, mapping | yes | control machine and mapping unit tests; remote crossing **fails on two machines** (this plan) |
| 25–27 devices page, layout editor, clipboard | yes | not verified across machines |
| 28–30 transfer core, priority, transfer UI | yes | loopback tests for the core; priority not measured |
| 31–32 file clipboard | yes | not verified |
| 33–38 drag-and-drop | partial | spike says go; the target path blocks the engine (finding 4); not verified |
| 39–42 permissions, logging, packaging, manual scripts | partial | file log exists but misses net-thread detail; release workflow never run; manual scripts never run |

## After v2 (unchanged backlog)
CLI/IPC (D1), push-through resistance (D2), profiles (D3), per-app exclusions (D4), resumable transfers (D6), hop to an app window (D5), clipboard history (D7), shared lock and Wake-on-LAN (D8), latency HUD (D10), gestures and throw (D9, D11), Linux (D13), phone companion (D12), internet relay (D14).
