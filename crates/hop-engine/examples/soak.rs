//! Headless soak tool (PLAN.md N0.2).
//!
//! Runs the real engine with the mock host (no Tauri) and forwards synthetic
//! motion at 1 kHz, while reading heartbeat RTT, datagram loss, the largest
//! heartbeat gap, and every `Online` / `Offline` / `Stalled` transition from
//! the engine link itself. The numbers come from the net thread's heartbeat
//! echo (`NetEvt::Stats` mirrored into the snapshot), so there is no second
//! probe connection. The summary is written to the rolling log (N0.1) with the
//! connection's `stable_id` and the peer id as fields.
//!
//! Two machines, same `--port P` (default 42424):
//!
//! ```text
//! B> soak.exe --secs 30                  # listens on P
//! A> soak.exe --secs 30 --connect <B-ip> # dials B:P
//! ```
//!
//! The engine's motion only crosses to the peer while this machine is
//! controlling it, but heartbeats (and therefore RTT, loss and the gap) flow
//! whenever a session is live.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use hop_core::{Chord, HotAction};
use hop_engine::{start_in, Cmd, Config, Conn, EngineHandle};
use hop_platform::{ClipboardItem, DisplayInfo, Host, InputEvent, MockHost, PermissionStatus};

const DEFAULT_PORT: u16 = 42424;
const DEFAULT_SECS: u64 = 10;

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("soak: {err}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(err) = run(args) {
        eprintln!("soak: {err}");
        std::process::exit(1);
    }
}

const USAGE: &str = "\
usage: soak [--secs N] [--connect host:port] [--port P] [--dir PATH]

  --secs N          forward motion for N seconds (default 10)
  --connect addr    dial the other machine's engine port (default: listen)
  --port P          engine UDP port (default 42424)
  --dir PATH        state directory (default: %TEMP%/devhop-soak)";

struct Args {
    secs: u64,
    connect: Option<SocketAddr>,
    port: u16,
    dir: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let mut secs = DEFAULT_SECS;
    let mut port = DEFAULT_PORT;
    let mut dir = std::env::temp_dir().join("devhop-soak");
    let mut connect_raw: Option<String> = None;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--secs" => {
                secs = value(&mut it, "--secs")?
                    .parse()
                    .map_err(|_| "--secs needs a number".to_string())?;
            }
            "--connect" => connect_raw = Some(value(&mut it, "--connect")?),
            "--port" => {
                port = value(&mut it, "--port")?
                    .parse()
                    .map_err(|_| "--port needs a number".to_string())?;
            }
            "--dir" => dir = PathBuf::from(value(&mut it, "--dir")?),
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let connect = match connect_raw {
        Some(text) => Some(parse_addr(&text, port)?),
        None => None,
    };
    Ok(Args {
        secs,
        connect,
        port,
        dir,
    })
}

fn value(it: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{flag} needs a value"))
}

fn parse_addr(text: &str, default_port: u16) -> Result<SocketAddr, String> {
    if let Ok(addr) = text.parse() {
        return Ok(addr);
    }
    if let Ok(ip) = text.parse::<std::net::IpAddr>() {
        return Ok(SocketAddr::new(ip, default_port));
    }
    Err(format!("could not parse address {text}"))
}

fn run(args: Args) -> Result<(), String> {
    std::fs::create_dir_all(&args.dir).map_err(|e| e.to_string())?;
    // So log::init (called by the engine thread) writes next to us.
    std::env::set_var("DEVHOP_HOME", &args.dir);
    hop_engine::init(&args.dir);

    // Keep the engine off the real LAN: this tool talks only to the address it
    // is given.
    let config = Config {
        port: args.port,
        discovery: false,
        ..Config::default()
    };
    config.save(&args.dir).map_err(|e| e)?;

    let displays = vec![DisplayInfo {
        id: "1".into(),
        name: "soak".into(),
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
        scale: 1.0,
        primary: true,
    }];
    let capture = Arc::new(CaptureHost::new(displays.clone()));
    let host: Arc<dyn Host> = capture.clone();
    let (handle, _ui) = start_in(args.dir.clone(), host);
    wait_ready(&handle)?;

    if let Some(addr) = args.connect {
        println!("engine: dialing {addr}");
        handle.command(Cmd::AddIp(addr.to_string()));
    } else {
        println!("engine: listening on UDP port {}", args.port);
    }

    let motion = spawn_motion(
        capture.clone(),
        displays[0].width,
        displays[0].height,
        args.secs,
    );
    let monitor = spawn_engine_monitor(handle, args.secs + 2);

    let sent = motion.join().unwrap_or(0);
    let report = monitor.join().unwrap_or_default();
    print_report(&report, sent);
    log_report(&report, sent);
    Ok(())
}

fn wait_ready(handle: &EngineHandle) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if !handle.snapshot().device_id.is_empty() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err("engine did not start".into())
}

/// A `MockHost` that keeps the crossbeam sender the engine's `start` hands it,
/// so the soak can push synthetic motion into the engine. This is the only way
/// to reach the engine's input path through public APIs: `EngineHandle` has no
/// motion command.
struct CaptureHost {
    inner: MockHost,
    tx: Mutex<Option<crossbeam_channel::Sender<InputEvent>>>,
}

impl CaptureHost {
    fn new(displays: Vec<DisplayInfo>) -> Self {
        Self {
            inner: MockHost::new(displays),
            tx: Mutex::new(None),
        }
    }

    fn input(&self) -> Option<crossbeam_channel::Sender<InputEvent>> {
        self.tx.lock().expect("capture").clone()
    }
}

impl Host for CaptureHost {
    fn list_displays(&self) -> Vec<DisplayInfo> {
        self.inner.list_displays()
    }
    fn warp(&self, x: i32, y: i32) {
        self.inner.warp(x, y);
    }
    fn cursor_pos(&self) -> Option<(i32, i32)> {
        self.inner.cursor_pos()
    }
    fn hide_cursor(&self) {
        self.inner.hide_cursor();
    }
    fn show_cursor(&self) {
        self.inner.show_cursor();
    }
    fn clip_rect(&self, rect: Option<(i32, i32, i32, i32)>) {
        self.inner.clip_rect(rect);
    }
    fn set_remote(&self, enabled: bool, park_x: i32, park_y: i32) {
        self.inner.set_remote(enabled, park_x, park_y);
    }
    fn set_hotkeys(&self, chords: Vec<(Chord, HotAction)>) {
        self.inner.set_hotkeys(chords);
    }
    fn inject_rel(&self, dx: i32, dy: i32) {
        self.inner.inject_rel(dx, dy);
    }
    fn inject_button(&self, button: u8, down: bool) {
        self.inner.inject_button(button, down);
    }
    fn inject_key(&self, hid: u16, down: bool) {
        self.inner.inject_key(hid, down);
    }
    fn inject_wheel(&self, dx: i32, dy: i32) {
        self.inner.inject_wheel(dx, dy);
    }
    fn release_inputs(&self) {
        self.inner.release_inputs();
    }
    fn clipboard_read(&self) -> Vec<ClipboardItem> {
        self.inner.clipboard_read()
    }
    fn clipboard_write(&self, items: &[ClipboardItem]) -> Result<(), String> {
        self.inner.clipboard_write(items)
    }
    fn clipboard_files(&self) -> Vec<PathBuf> {
        self.inner.clipboard_files()
    }
    fn clipboard_offer_files(&self, paths: &[PathBuf]) -> Result<(), String> {
        self.inner.clipboard_offer_files(paths)
    }
    fn read_edge_drag(&self) -> Vec<PathBuf> {
        self.inner.read_edge_drag()
    }
    fn begin_file_drag(&self, paths: &[PathBuf]) -> Result<(), String> {
        self.inner.begin_file_drag(paths)
    }
    fn permissions(&self) -> PermissionStatus {
        self.inner.permissions()
    }
    fn prompt_permissions(&self) {
        self.inner.prompt_permissions();
    }
    fn start(&self, tx: crossbeam_channel::Sender<InputEvent>) -> Result<(), String> {
        *self.tx.lock().expect("capture") = Some(tx);
        Ok(())
    }
    fn stop(&self) {
        self.inner.stop();
    }
}

/// 1 kHz synthetic motion into the engine, as a real hook would deliver it.
fn spawn_motion(
    host: Arc<CaptureHost>,
    width: u32,
    height: u32,
    secs: u64,
) -> thread::JoinHandle<u64> {
    thread::spawn(move || {
        let tx = loop {
            if let Some(tx) = host.input() {
                break tx;
            }
            thread::sleep(Duration::from_millis(10));
        };
        let (w, h) = (width.max(2) as i32, height.max(2) as i32);
        let (mut x, y, mut sent) = (w / 2, h / 2, 0u64);
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(secs) {
            x = (x + 1) % w;
            let _ = tx.send(InputEvent::Motion { x, y, dx: 1, dy: 0 });
            sent += 1;
            thread::sleep(Duration::from_millis(1));
        }
        sent
    })
}

/// The engine-link numbers the monitor collected.
#[derive(Default)]
struct EngineReport {
    peer: String,
    stable_id: Option<usize>,
    rtt_p50_ms: Option<f64>,
    rtt_p95_ms: Option<f64>,
    loss_percent: f64,
    max_beat_gap_ms: u64,
    max_beat_recv_gap_ms: u64,
    stalled: bool,
    events: Vec<String>,
}

/// Watches the engine snapshot: prints every peer `Online` / `Offline` /
/// `Stalled` transition and keeps the latest heartbeat stats off the live link.
fn spawn_engine_monitor(handle: EngineHandle, secs: u64) -> thread::JoinHandle<EngineReport> {
    thread::spawn(move || {
        let start = Instant::now();
        let mut online: HashMap<String, bool> = HashMap::new();
        let mut stalled: HashMap<String, bool> = HashMap::new();
        let mut report = EngineReport::default();
        while start.elapsed() < Duration::from_secs(secs) {
            let snap = handle.snapshot();
            for peer in &snap.peers {
                match online.get(&peer.id).copied() {
                    Some(true) if !peer.online => {
                        let line = format!(
                            "{:>6.2}s engine Offline  {}",
                            start.elapsed().as_secs_f64(),
                            peer.id
                        );
                        println!("{line}");
                        report.events.push(format!("Offline({})", peer.id));
                    }
                    Some(false) | None if peer.online => {
                        let line = format!(
                            "{:>6.2}s engine Online   {} ({})",
                            start.elapsed().as_secs_f64(),
                            peer.id,
                            peer.addr
                        );
                        println!("{line}");
                        report.events.push(format!("Online({})", peer.id));
                    }
                    _ => {}
                }
                online.insert(peer.id.clone(), peer.online);
                if peer.online {
                    let was = stalled
                        .insert(peer.id.clone(), peer.stalled)
                        .unwrap_or(false);
                    if peer.stalled && !was {
                        let line = format!(
                            "{:>6.2}s engine Stalled  {} ({} ms since last beat)",
                            start.elapsed().as_secs_f64(),
                            peer.id,
                            peer.max_beat_gap_ms
                        );
                        println!("{line}");
                        report.events.push(format!("Stalled({})", peer.id));
                    }
                    report.peer = peer.id.clone();
                    report.stable_id = peer.stable_id.or(report.stable_id);
                    report.rtt_p50_ms = peer.rtt_p50_ms.or(report.rtt_p50_ms);
                    report.rtt_p95_ms = peer.rtt_p95_ms.or(report.rtt_p95_ms);
                    report.loss_percent = peer.loss_percent;
                    report.max_beat_gap_ms = report.max_beat_gap_ms.max(peer.max_beat_gap_ms);
                    report.max_beat_recv_gap_ms =
                        report.max_beat_recv_gap_ms.max(peer.max_beat_recv_gap_ms);
                    report.stalled = peer.stalled;
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
        report
    })
}

fn fmt_ms(value: Option<f64>) -> String {
    match value {
        Some(ms) => format!("{ms:.2}"),
        None => "n/a".into(),
    }
}

fn print_report(report: &EngineReport, engine_motion: u64) {
    println!("\n==== soak summary ====");
    println!(
        "peer:              {} (conn {})",
        if report.peer.is_empty() {
            "(none)"
        } else {
            &report.peer
        },
        report
            .stable_id
            .map_or("n/a".to_string(), |id| id.to_string())
    );
    println!(
        "RTT p50/p95:       {} / {} ms",
        fmt_ms(report.rtt_p50_ms),
        fmt_ms(report.rtt_p95_ms)
    );
    println!("heartbeat loss:    {:.2}%", report.loss_percent);
    println!(
        "max beat gap:      {} ms sent / {} ms received  (stalled: {})",
        report.max_beat_gap_ms, report.max_beat_recv_gap_ms, report.stalled
    );
    println!("engine motion:     {engine_motion} events injected");
    println!("session events:    {:?}", report.events);
}

/// The same numbers go into the rolling log with the connection fields.
fn log_report(report: &EngineReport, engine_motion: u64) {
    let conn = Conn::new(report.stable_id.unwrap_or(0), &report.peer);
    conn.info(format!(
        "soak summary: RTT p50={}ms p95={}ms; loss={:.2}%; max beat gap={}ms sent/{}ms recv stalled={}; engine injected {engine_motion}",
        fmt_ms(report.rtt_p50_ms),
        fmt_ms(report.rtt_p95_ms),
        report.loss_percent,
        report.max_beat_gap_ms,
        report.max_beat_recv_gap_ms,
        report.stalled,
    ));
    conn.info(format!("soak events: {:?}", report.events));
}
