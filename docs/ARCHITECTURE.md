# DevHop architecture (target, v2)

This is the design the v2 plan builds toward. It keeps the crate split from `SPEC.md` and changes how the engine and the network core are put together, because the current shape is why two real machines cannot hold a connection (see "What is broken" in `PLAN.md`).

## 1. Process and thread map

```
Tauri main thread          UI, tray, AppKit calls on macOS. Talks to the engine only through
                           EngineHandle { command(), snapshot() } and the UiEvent channel.

devhop-hook (Win) /        OS hook callback. Enqueues InputEvent on a crossbeam channel and returns.
CFRunLoop thread (mac)     Never allocates, never locks anything else.

devhop-engine              The only owner of engine state. crossbeam select! over
                           { input events, UI commands, net events, host results }. No sleep, no poll.

devhop-host                Host calls that can block (clipboard read/write, DoDragDrop, permissions
                           prompt). Takes a job, posts the result back as an InputEvent.

devhop-net (tokio)         Network core. The main task only routes; every await on a socket or
                           stream lives in a per-session task.
```

Rules:
- One thread owns each piece of mutable state. Threads talk through channels, never through shared mutexes on the hot path. The only shared mutex is the `Snapshot` the UI reads.
- Nothing on the engine thread or the net main task waits on the OS, the disk, or the network.
- Channels on the input path are bounded. When a bounded channel is full, motion is dropped and counted, never queued without limit.

## 2. Input path and latency budget

```
controller                                            controlled
hook ──crossbeam──▶ engine ──bounded mpsc──▶ session tx task ──datagram──▶
                                                                      datagram rx task ──crossbeam──▶ engine ──▶ inject
```

| Hop | Budget |
|---|---|
| hook to engine (channel wake) | < 0.1 ms |
| engine (geometry, control machine) | < 0.1 ms |
| engine to session task to UDP | < 0.2 ms |
| LAN (wired) | 0.2 to 1 ms |
| datagram task to engine to SendInput / CGEventPost | < 0.3 ms |
| **total, p95 target (R4)** | **≤ 10 ms**, expected 1 to 3 ms wired |

The current code spends up to 8 ms in each engine's idle sleep, twice per packet. That alone can exceed the budget.

## 3. Network core (`hop-net` + `hop-engine::netloop`)

### 3.1 Objects

- **Endpoint**: one quinn endpoint, bound once. The accept loop is its own task.
- **Link**: one QUIC connection. Carries `stable_id`, `dialed: bool`, peer fingerprint, peer address.
- **Session**: one per peer id. Owns exactly one Link and the tasks below. Replaced only by the rule in 3.3.
- **PairGate**: as today. Lives in the session; parked by fingerprint while a peer is offline and not yet pinned.

Per-session tasks (all `tokio::spawn`, all end when the connection closes):

| Task | Owns | Does |
|---|---|---|
| control writer | `SendStream` (control) | drains a bounded `mpsc<StreamMsg>`; `write_all` happens here and nowhere else |
| control reader | `RecvStream` (control) | frames messages, forwards `Message` events |
| datagram reader | `Connection` clone | forwards motion and heartbeat echoes; its end is the session's end |
| heartbeat | `Connection` clone | 100 ms `interval`, sends `Heartbeat{seq, sent_us}`, records RTT from echoes |
| stream acceptor | `Connection` clone | `accept_bi`, reads the kind byte, routes (3.4) |

The net main task holds `HashMap<peer_id, Session>` and only pushes into channels or spawns tasks. It never awaits a stream.

### 3.2 Session states

```
Discovered ──dial──▶ Dialing ──link──▶ Handshaking ──hello ok──▶ Connected{unpaired}
   ▲                    │                   │                          │ both confirm
   │                    │ fail (backoff)    │ fail / timeout           ▼
   └────────────────────┴───────────────────┴──────────────── Connected{paired}
                                                                       │ connection error
                                                                       ▼
                                                                    Closing ──▶ Discovered (redial after backoff)
```

Pure, in `hop-net::session`, with unit tests. `netloop.rs` applies its outputs (spawn, close, emit).

### 3.3 Which connection survives when there are two

Both machines must reach the same answer without talking about it.

1. If a `Connected` session already exists for this peer and its link is open, **the new link is closed**. First healthy link wins. (Today the newest wins, and the two sides disagree about which is newest.)
2. If two links are both still in `Handshaking`, keep the one opened by the device with the smaller id (`should_dial`), close the other. Both sides can compute this from the ids alone.
3. A link whose handshake says `peer_id == our id` is closed and the address is marked "self".
4. Dial at most once per peer: `dialing` is keyed by device id when the id is known (discovery, pairing) and by address only for a manual `AddIp` whose id is unknown. An `AddIp` to an address that `known` maps to a peer with a session or a dial in flight is a no-op.

### 3.4 Streams and datagrams

- **Datagrams**: `Motion` and `Heartbeat` only. Unreliable, unordered, tiny.
- **Bidirectional streams** carry a one-byte kind as their first byte:

| kind | opened by | priority | payload |
|---|---|---|---|
| `1` control | the dialer, once per link | 100 | `StreamMsg` frames: hello, keys, buttons, wheel, enter/leave, pairing, layout, drag control |
| `2` clipboard | the sender, one per payload | 50 | `ClipboardAnnounce` then `ClipboardChunk` × n (64 KiB each) |
| `3` transfer | the sender, one per transfer | 0 | manifest then chunks (as `hop-transfer` does today) |

Today every accepted stream is treated as a transfer. The kind byte removes that ambiguity and lets the clipboard leave the control stream, which is what stalls heartbeats now.

### 3.5 Liveness

- quinn `max_idle_timeout = 3 s`, `keep_alive_interval = 500 ms`. A dead peer becomes a `ConnectionError` on every reader within about 3 s, with no application timer to get wrong.
- The 100 ms heartbeat datagram stays for two reasons: RTT measurement (R4) and the 300 ms `Stalled` signal while controlling (R5: release keys fast even though the connection is not yet dead).
- `SESSION_TIMEOUT_MS` goes away. "Session ended" has one source: the connection closed.

### 3.6 Discovery

- mDNS (`_devhop._udp`) is primary; the UDP beacon on 42425 is the fallback for networks that block multicast.
- The beacon loop treats `ConnectionReset`, `WouldBlock`, `TimedOut` and `Interrupted` as retryable and backs off on anything else. It never exits silently.
- The beacon socket is bound with `SO_REUSEADDR` so two instances on one machine can both listen.
- mDNS removals are matched on the stored mDNS fullname, not on a sanitized display name.
- A beacon never changes the address of a peer that has a live session.
- `discovery = false` in `config.toml` keeps tests and headless runs off the real LAN.

## 4. Engine (`hop-engine::runtime`)

```rust
loop {
    crossbeam_channel::select! {
        recv(input)  -> ev  => engine.on_input(ev?),
        recv(cmds)   -> cmd => engine.on_cmd(cmd?),
        recv(net)    -> evt => engine.on_net(evt?),
        recv(host)   -> res => engine.on_host(res?),
    }
    engine.publish_if_dirty();   // at most every 50 ms
}
```

- `Snapshot` is rebuilt only when something changed, and at most 20 times a second.
- `config.toml` is written on a 500 ms debounce.
- Host calls that can block go to the host thread and come back as events: `ClipboardRead{items}`, `ClipboardWritten{ok}`, `DragFinished{accepted}`, `PermissionsChanged{..}`. The engine keeps injecting motion while a drag runs; that is what makes a remote drop possible (R13).
- Control (`hop-core::ControlMachine`) stays pure and owns held keys and buttons. `Stalled`, `Offline`, panic and `CrossHome` all go through it so release logic has one path (R5, R6).

## 5. Pairing flow

1. Discovery (or `AddIp`) produces a `Discovered` peer. Only discovery dials, and only when `should_dial(our_id, their_id)`; the other side waits to be dialed. (`AddIp` dials regardless, once.)
2. The link connects; hello exchanges ids, names, OS, protocol version.
3. Both UIs show the other machine's `local_code` (hash of its certificate fingerprint). Each person types the code they see on the *other* screen. `Confirm` checks the typed code against the fingerprint the connection actually presented, so a code can only confirm the certificate it came from.
4. When both have confirmed, both pin the fingerprint in `peers.toml` and the gate opens. Until then no input message is delivered to the engine (R7).
5. Pairing never dials. If the peer is not connected yet, the UI says "connecting" and the confirm is sent when it is.

## 6. Logging and diagnostics

- `tracing` with a rolling file appender (`tracing-appender`, 5 × 5 MiB) initialised once in `log::init`. Every thread logs directly; the engine's status line is separate.
- Every network line carries the connection `stable_id` and the peer id, so a replaced link can be told from the live one.
- A headless soak binary (`PLAN.md` N0.2) runs the engine without Tauri against a given address, prints RTT p50/p95, datagram loss, and every session event. It is the tool for the two-machine checks in N6.

## 7. Testing

| Level | What | Where |
|---|---|---|
| unit | geometry, hop modes, control machine, key mapping, framing, session state machine, link-selection rule | `hop-core`, `hop-proto`, `hop-net::session` |
| loopback | two net nodes: both dial at once plus a duplicate dial, pair, 30 s motion flood, 4 MB clipboard every 5 s, 2 GB transfer; zero `Offline`/`Stalled`, p95 < 5 ms, heartbeat gap < 150 ms | `netloop.rs` tests, run in CI |
| two engines | the UI's commands end to end through `start_in` with the mock host | `runtime.rs` tests |
| two machines | `docs/manual-tests.md` R1 to R16, run on the real PC and Mac, results recorded with log tails | by hand, gates N6 and N7 |

## 8. What does not change

Crate layout, the `Host` trait and its OS backends, `hop-core` geometry, the `hop-proto` message set (plus the stream kind byte), `hop-transfer`, identity and TOFU pairing with the 6-digit code, the Tauri shell and the Svelte UI.
