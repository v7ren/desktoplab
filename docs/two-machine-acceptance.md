# Two-machine acceptance (N6) and latency soak (N3.4 / R4)

Run these on one **Windows PC** and one **Mac** on the **same wired LAN** (Ethernet on both). Record every run in the tables below. Do not fill in expected outcomes as results; leave result cells blank until you run the test.

The DevHop **UI** tests (N6.1–N6.5) use the normal app. The **soak** tests (N3.4 / R4) use the headless example binary only (no Tauri).

Related plan items: `PLAN.md` N0 (logging and soak tool), N3.4, N6, N7. Design context: `docs/ARCHITECTURE.md`.

---

## Before you start

| Item | What to do |
|------|------------|
| Network | Same subnet, **wired** Ethernet on PC and Mac. Note each machine’s LAN IP (e.g. `192.168.1.10`, `192.168.1.20`). |
| Repo | Same commit on both machines. Build DevHop and the soak example when you are ready to test. |
| UI app | Start DevHop on **both** machines (your usual release or dev build). Leave discovery enabled unless you are only running soak. |
| Firewall | Allow DevHop / UDP on the engine port (default **42424**) if the OS prompts or blocks traffic. |
| Soak ports | Engine uses `--port P` (default **42424**). One UDP port per machine; there is **no separate probe port**. |

### Where logs are written

Paths come from `hop-engine::runtime::config_dir()` and `hop-engine::log`:

| App | Config / state directory | Log directory | Main files |
|-----|--------------------------|---------------|------------|
| **DevHop UI** (default) | `%APPDATA%\devhop` on Windows | `%APPDATA%\devhop\logs` | `devhop.log` (status lines); `tracing` also writes rolling files under the same `logs` folder (daily rotation, prefix `devhop`). |
| **DevHop UI** (default) | `~/Library/Application Support/devhop` on macOS | `~/Library/Application Support/devhop/logs` | Same file names as Windows. |
| **Soak example** (default `--dir`) | `%TEMP%\devhop-soak` on Windows | `%TEMP%\devhop-soak\logs` | Soak sets `DEVHOP_HOME` to `--dir`; summary lines include `soak summary:` and `soak events:`. |
| **Soak example** (default `--dir`) | `$TMPDIR/devhop-soak` on macOS (or `/var/folders/...` from `temp_dir()`) | `<that-dir>/logs` | Same as Windows soak. |

Override for either UI or soak: set environment variable **`DEVHOP_HOME`** to a folder path before launch (and use a distinct `--port` if two engines run on one machine).

**Copy the last part of a log (examples):**

- Windows (PowerShell): `Get-Content -Path "$env:APPDATA\devhop\logs\devhop.log" -Tail 80`
- macOS: `tail -80 ~/Library/Application\ Support/devhop/logs/devhop.log`

For soak, replace the path with your `--dir` plus `\logs` or `/logs`.

### Result table (copy per test)

| Result (pass / fail / blocked) | Tester / date | Notes | Log tail (last ~50–80 lines, or terminal output for soak) |
|-------------------------------|---------------|-------|-----------------------------------------------------------|
| | | | |

---

## N6.1 — Pair from the UI (both click Pair, first try)

**Goal (PLAN N6.1):** Pair Windows and Mac from the UI, both clicking Pair, first try.

| Step | Action | Expected result |
|------|--------|-----------------|
| 1 | On **both** machines, open DevHop and go to the **Devices** (or pairing) screen. | Each machine shows the other (or discovers it within a short time). |
| 2 | Confirm the **same 6-digit code** appears for the connection on **both** screens (see R7 in `docs/manual-tests.md`). | Codes match; input does not cross yet. |
| 3 | Within about one second, click **Pair** (or confirm pairing) on **Windows**, then on **Mac** (both sides actively confirm). | Status moves to paired / connected; no repeated connect–disconnect loop. |
| 4 | Move the mouse across the edge to the other machine once. | Cursor crosses; control feels on the remote machine. |
| 5 | Open the log on **both** machines (paths above). | Lines show dial/accept/handshake/pairing (no endless `lost the connection` / `bye` spam for this attempt). |

**Log:** `%APPDATA%\devhop\logs` (Windows UI) and `~/Library/Application Support/devhop/logs` (Mac UI).

| Result (pass / fail / blocked) | Tester / date | Notes | Log tail |
|-------------------------------|---------------|-------|----------|
| | | | |

---

## N6.2 — Cross both ways 50 times

**Goal (PLAN N6.2):** Cross both ways 50 times with no `Offline`; keys and buttons work; record p95.

| Step | Action | Expected result |
|------|--------|-----------------|
| 1 | Stay **paired** from N6.1. | Peer shows online on both UIs. |
| 2 | Count **50 complete crossings**: PC → Mac → PC counts as two crossings, or define one crossing as one edge cross; use the same rule for the whole run and note it in **Notes**. | 50 crossings completed without giving up. |
| 3 | During crossings, press a few **keys** and **mouse buttons** on each side while controlling the other machine. | Keys and buttons affect the machine under the cursor. |
| 4 | After the run, search both logs for `Offline`, `Stalled`, and `lost the connection`. | **Zero** `Offline` / session-drop events attributable to this test (per N6.2). |
| 5 | Record **p95**: if the log or UI shows RTT/latency/p95 for the session, copy those values into **Notes**. If not logged during UI use, run the **soak** block below afterward and put soak **RTT p95** in **Notes** as the recorded p95. | p95 value written in **Notes** (source: log line or soak summary). |

**Log:** UI log paths above.

| Result (pass / fail / blocked) | Tester / date | Notes (crossing rule, p95 source and value) | Log tail |
|-------------------------------|---------------|-----------------------------------------------|----------|
| | | | |

---

## N6.3 — 8 MB image clipboard while crossing

**Goal (PLAN N6.3):** Copy an 8 MB image, cross, paste on the other side, no `Stalled`.

| Step | Action | Expected result |
|------|--------|-----------------|
| 1 | On machine **A**, prepare or copy an **image file of about 8 MB** (e.g. PNG). Copy it to the clipboard (file copy or image copy, whichever you use for R8-style image paste). | Clipboard holds the image on A. |
| 2 | Cross to machine **B** with the cursor. | Control is on B; connection stays up. |
| 3 | Paste on **B** (e.g. into an image editor or Preview). | Image appears on B (correct size roughly 8 MB). |
| 4 | Repeat **B → A** once (copy on B, cross to A, paste on A). | Works both directions. |
| 5 | Check logs on both machines for `Stalled` during the test. | **No** `Stalled` tied to this clipboard/cross sequence. |

**Log:** UI log paths above.

| Result (pass / fail / blocked) | Tester / date | Notes | Log tail |
|-------------------------------|---------------|-------|----------|
| | | | |

---

## N6.4 — Cable pull while holding a key

**Goal (PLAN N6.4):** Pull the cable while holding a key: release within 300 ms, `Offline` within 3.5 s, reconnect on replug without clicking anything.

Use the **Ethernet** cable on the machine that is **being controlled** (the one receiving injected keys), unless you are testing Wi‑Fi; note which side in **Notes**.

| Step | Action | Expected result |
|------|--------|-----------------|
| 1 | Pair and cross so **PC controls Mac** or **Mac controls PC** (pick one; note in **Notes**). | Remote control active. |
| 2 | On the **controller**, hold a harmless key (e.g. **Shift**). Keep it held. | Key stays down on the **controlled** machine. |
| 3 | **Unplug Ethernet** on the controlled machine (or the link you are testing). Start a timer. | Within **300 ms**, the held key **releases** on the controlled machine (R3 / N1.4 intent). |
| 4 | Keep cable unplugged briefly; watch UI and log. | Within **3.5 s**, peer shows **Offline** / connection lost in log. |
| 5 | **Plug cable back in**; do **not** click Pair again. Wait for discovery/reconnect. | Session comes back without manual re-pair (may take up to discovery/backoff time; note how long in **Notes**). |
| 6 | Cross again once. | Cursor crosses again after reconnect. |

**Log:** UI log paths above.

| Result (pass / fail / blocked) | Tester / date | Notes (which side controlled, cable side) | Log tail |
|-------------------------------|---------------|-------------------------------------------|----------|
| | | | |

---

## N6.5 — One hour idle

**Goal (PLAN N6.5):** Run for one hour idle: zero `Offline`, log under 1 MB.

| Step | Action | Expected result |
|------|--------|-----------------|
| 1 | Leave both machines **paired** and DevHop **running**. Do not move the mouse across edges; no clipboard tests. | Apps stay open on both. |
| 2 | Wait **60 minutes** (note start/end time in **Notes**). | No interaction required during the hour. |
| 3 | After 60 minutes, search logs on **both** machines for `Offline` during the window. | **Zero** `Offline` during idle (N6.5). |
| 4 | Check total size of the UI log folder or main `devhop.log` (+ rotated/tracing files if you count them). | Combined new log growth for the hour **under 1 MB** (per N6.5). |

**Log:** UI log paths above.

| Result (pass / fail / blocked) | Tester / date | Notes (start/end time, how log size was measured) | Log tail |
|-------------------------------|---------------|-----------------------------------------------------|----------|
| | | | |

---

## N3.4 / R4 — Headless soak over wired LAN

**Goal (PLAN N3.4):** On both machines, run the soak example against each other on a **wired LAN**. **RTT p95 ≤ 10 ms** (R4). Write numbers into `docs/manual-tests.md` **R4** table and the soak section below.

**Source of truth for flags:** `crates/hop-engine/examples/soak.rs` (`USAGE` in that file).

### Soak command-line (exact)

```
usage: soak [--secs N] [--connect host:port] [--port P] [--dir PATH]

  --secs N          forward motion for N seconds (default 10)
  --connect addr    dial the other machine's engine port (default: listen)
  --port P          engine UDP port (default 42424)
  --dir PATH        state directory (default: %TEMP%/devhop-soak)
```

Help:

```text
cargo run -p hop-engine --example soak -- --help
```

Release binary (after `cargo build -p hop-engine --example soak`):

```text
# Windows (path varies)
.\target\release\examples\soak.exe --help

# macOS
./target/release/examples/soak --help
```

### Two-machine layout (from soak.rs)

Default engine port: **42424** on both machines. There is **no separate probe
connection**: the RTT, loss and gap numbers come from the heartbeat echo on the
live engine session.

**Listener (machine B)** — listens on `P`:

```text
cargo run -p hop-engine --example soak -- --secs 60 --port 42424
```

**Dialer (machine A)** — dials B’s IP on the engine port:

```text
cargo run -p hop-engine --example soak -- --secs 60 --port 42424 --connect 192.168.1.20
```

Use B’s real LAN IP. You can pass `host:port` or just `host` (port defaults to `--port`).

Optional: separate state directory per run:

```text
cargo run -p hop-engine --example soak -- --secs 60 --dir C:\Temp\devhop-soak-run1 --connect 192.168.1.20
```

```text
cargo run -p hop-engine --example soak -- --secs 60 --dir /tmp/devhop-soak-run1
```

Soak turns **discovery off**; only the given `--connect` / listen address is used.

### What to look for

| Step | Action | Expected result |
|------|--------|-----------------|
| 1 | Close DevHop UI on both machines **or** use different `--port` / `DEVHOP_HOME` so nothing else binds **42424**. | Soak starts without “address in use”. |
| 2 | Start **listener** on B; wait for `engine: listening on UDP port 42424`. | B is waiting. |
| 3 | Start **dialer** on A with `--connect <B-ip>`. | A prints `engine: dialing <B-ip>:42424`, then `engine Online` lines with the peer id and address. |
| 4 | Let the run finish `--secs` (use **≥ 60** for a stable p95 sample). | Terminal prints `==== soak summary ====` with `RTT p50/p95` in ms, `heartbeat loss` %, `max beat gap` (sent/received), `engine motion` events, and `session events`. |
| 5 | No spurious session drops during soak. | The event list shows no `Offline` / `Stalled` during a good run (N0.2 intent). |
| 6 | Read **RTT p95** from the terminal summary. | **p95 ≤ 10.0 ms** on wired LAN (R4 / N3.4). |
| 7 | Open soak log dir: `<--dir>/logs` (default `%TEMP%\devhop-soak\logs` or macOS temp). | Lines contain `soak summary:` with the same RTT p50/p95 and `soak events:` with `stable_id` and peer id. |

**Log (soak):** `<--dir>/logs` (not the UI `%APPDATA%\devhop` path unless you set `--dir` there on purpose).

### Reverse direction (optional second run)

Swap roles: A listens, B dials with `--connect <A-ip>`. Record both p95 values if you want uplink/downlink symmetry.

| Result (pass / fail / blocked) | Tester / date | RTT p50 (ms) | RTT p95 (ms) | Heartbeat loss % | Session events (from terminal) | Log tail (`soak summary` / `soak events`) |
|-------------------------------|---------------|--------------|--------------|------------------|--------------------------------|---------------------------------------------|
| B listen, A dial | | | | | | |
| A listen, B dial (optional) | | | | | | |

---

## Gate (N6 + N3.4)

Per `PLAN.md`, N6.1–N6.5 and N3.4/R4 should pass before treating v1 features in N7 as verified. Feature checks remain in `docs/manual-tests.md` (R1–R16).
