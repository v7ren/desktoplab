//! Process entry. Local hops, remote control, clipboard, and the network thread.

use hop_core::{
    denormalize, normalize, translate_hid, ControlCommand, ControlEvent, ControlMachine, Device,
    DeviceId, HopMode, HostOs, Layout, ModifierMap, Monitor, MonitorId, PhysRect, ScrollNorm,
};
use hop_net::Identity;
use hop_platform::{install, ClipboardItem, DisplayInfo, Host, InputEvent};
use hop_proto::{ClipKind, Datagram, FileAnnounce, OsKind, StreamMsg};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

use crate::clipboard::prepare;
use crate::config::Config;
use crate::dnd;
use crate::host_worker::{spawn as spawn_host_worker, HostJob};
use crate::local::{on_hotkey, on_motion, LocalSession, MotionEffect};
use crate::log::{self, Log};
use crate::netloop::{self, NetCmd, NetEvt};
use crate::peers::PeerStore;
use crate::remote::RemoteSession;
use crate::transfer::TransferBook;

#[derive(Clone, Debug, Serialize)]
pub struct PeerView {
    pub id: String,
    pub name: String,
    pub addr: String,
    pub fingerprint: String,
    pub paired: bool,
    pub online: bool,
    /// One status per peer, derived from the session state machine (N4.2):
    /// `discovered`, `connecting`, `unpaired`, `paired`, or `offline`.
    pub status: String,
    pub os: String,
    pub code: Option<String>,
    /// QUIC connection `stable_id` of the live link, if any.
    pub stable_id: Option<usize>,
    /// Heartbeat RTT percentiles in milliseconds, measured by the net thread.
    pub rtt_p50_ms: Option<f64>,
    pub rtt_p95_ms: Option<f64>,
    /// Lost heartbeats since the session started, as a percentage.
    pub loss_percent: f64,
    /// Largest gap between heartbeats *sent* by this side, in milliseconds
    /// (N2.4: whether the heartbeat task was starved).
    pub max_beat_gap_ms: u64,
    /// Largest gap between heartbeats *received*; includes datagram loss.
    pub max_beat_recv_gap_ms: u64,
    /// No heartbeat for three intervals while the link is still open.
    pub stalled: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct MonitorView {
    pub id: String,
    pub device_id: String,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct TransferView {
    pub id: u64,
    pub label: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub bytes_per_sec: u64,
    pub state: String,
    pub path: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub device_id: String,
    pub device_name: String,
    pub paused: bool,
    pub locked: bool,
    pub remote: Option<String>,
    pub hop_mode: String,
    pub wrap: bool,
    pub clipboard_sync: bool,
    pub modifiers: ModifierMap,
    pub scroll: ScrollNorm,
    pub hotkeys: crate::config::HotkeyConfig,
    pub peers: Vec<PeerView>,
    pub monitors: Vec<MonitorView>,
    pub transfers: Vec<TransferView>,
    pub pairing_code: Option<String>,
    pub local_code: String,
    pub local_addr: String,
    pub accessibility: bool,
    pub input_monitoring: bool,
    pub status: String,
    pub port: u16,
}

#[derive(Clone, Debug, Deserialize)]
pub enum Cmd {
    SetPaused(bool),
    SetHopMode(HopMode),
    SetWrap(bool),
    SetHotkey { action: String, chord: String },
    SetClipboard(bool),
    SetModifiers(ModifierMap),
    SetScroll(ScrollNorm),
    ConfirmPair { id: String, code: String },
    PairByCode(String),
    RejectPair(String),
    Unpair(String),
    AddIp(String),
    MoveMonitor { id: String, x: i32, y: i32 },
    CancelTransfer(u64),
    OpenTransfer(u64),
    PromptPermissions,
}

#[derive(Clone, Debug)]
pub enum UiEvent {
    FindCursor { x: i32, y: i32 },
    Status(String),
}

pub struct EngineHandle {
    cmd: crossbeam_channel::Sender<Cmd>,
    snap: Arc<Mutex<Snapshot>>,
}

impl EngineHandle {
    pub fn snapshot(&self) -> Snapshot {
        self.snap.lock().expect("snapshot").clone()
    }

    pub fn command(&self, cmd: Cmd) {
        let _ = self.cmd.send(cmd);
    }
}

/// `DEVHOP_HOME` overrides the folder, so two copies can run side by side on
/// one computer for testing (give the second copy its own `port` too).
pub fn config_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("DEVHOP_HOME") {
        return PathBuf::from(home);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("devhop")
}

pub fn spawn() -> (EngineHandle, std::sync::mpsc::Receiver<UiEvent>) {
    start_in(config_dir(), install())
}

pub fn start_in(
    dir: PathBuf,
    host: Arc<dyn Host>,
) -> (EngineHandle, std::sync::mpsc::Receiver<UiEvent>) {
    let snap = Arc::new(Mutex::new(empty_snapshot()));
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let (ui_tx, ui_rx) = std::sync::mpsc::channel();
    let snap_thread = snap.clone();
    thread::Builder::new()
        .name("devhop-engine".into())
        .spawn(move || engine_main(dir, host, cmd_rx, ui_tx, snap_thread))
        .ok();
    (EngineHandle { cmd: cmd_tx, snap }, ui_rx)
}

fn engine_main(
    dir: PathBuf,
    host: Arc<dyn Host>,
    cmds: crossbeam_channel::Receiver<Cmd>,
    ui: std::sync::mpsc::Sender<UiEvent>,
    snap: Arc<Mutex<Snapshot>>,
) {
    log::init(&dir);
    let _ = std::fs::create_dir_all(&dir);
    let config = Config::load_or_create(&dir).unwrap_or_default();
    let log = Log::open(&dir).ok();
    let peers = PeerStore::load(&dir);
    let identity = Identity::load_or_create(&dir).ok();
    let displays = host.list_displays();
    let device = DeviceId(config.device_id.clone());
    let layout = layout_from(&device, &config.device_name, &displays, config.wrap);
    let primary = layout
        .monitors
        .first()
        .map(|m| m.id.clone())
        .unwrap_or_else(|| MonitorId("local:0".into()));
    let session = LocalSession {
        device: device.clone(),
        monitor: primary,
        nx: 0.5,
        ny: 0.5,
        memory: hop_core::PositionMemory::default(),
        locked: false,
    };
    let (input_tx, input_rx) = crossbeam_channel::unbounded();
    // Keep one sender alive so the select loop never spins on a disconnected
    // input channel if the host backend stops delivering events.
    let _input_keepalive = input_tx.clone();
    if let Err(err) = host.start(input_tx) {
        if let Some(log) = &log {
            log.write_line(&format!("host start: {err}"));
        }
    }
    host.set_hotkeys(config.chords());
    let incoming = dirs::download_dir()
        .unwrap_or_else(|| dir.join("Downloads"))
        .join("DevHop");
    let _ = std::fs::create_dir_all(&incoming);
    let local_code = identity
        .as_ref()
        .map(|id| hop_net::local_code(&id.fingerprint))
        .unwrap_or_default();
    let (net, net_rx, _net_evt_keepalive) = if let Some(identity) = identity {
        let (net, net_rx) = netloop::spawn(
            dir.clone(),
            incoming,
            identity,
            config.device_id.clone(),
            config.device_name.clone(),
            config.port,
            config.discovery,
            peers.clone(),
        );
        (net, net_rx, None)
    } else {
        // No identity: no network thread. Hold the event sender so the select
        // loop never sees a disconnected receiver.
        let (tx, _cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (evt_tx, evt_rx) = crossbeam_channel::unbounded();
        (tx, evt_rx, Some(evt_tx))
    };
    let (host_jobs, host_results) = spawn_host_worker(host.clone());
    let permissions = host.permissions();
    let mut engine = Engine {
        dir,
        config,
        layout,
        session,
        control: ControlMachine::new(),
        host,
        peers,
        transfers: TransferBook::new(),
        log,
        online: HashMap::new(),
        peer_status: HashMap::new(),
        discovered: Vec::new(),
        button: false,
        remote: None,
        controller: None,
        net,
        ui,
        snap,
        clips: Vec::new(),
        status: "ready".into(),
        local_code,
        pending_pair: None,
        wanted_code: None,
        host_jobs,
        pending_clipboard_peer: None,
        snapshot_dirty: true,
        last_publish: Instant::now() - Duration::from_secs(1),
        config_pending: false,
        config_deadline: Instant::now(),
        permissions,
    };
    engine.publish_if_dirty();
    let publish_tick = crossbeam_channel::tick(Duration::from_millis(50));
    run_engine_loop(
        &mut engine,
        &input_rx,
        &host_results,
        &net_rx,
        &cmds,
        &publish_tick,
    );
    engine.flush_config();
    engine.publish_if_dirty_force();
}

/// The engine's only loop. It blocks in `select!` on every event source and on
/// a 50 ms timer; there is no idle sleep and no `try_recv` polling. The timer
/// drives the 50 ms publish cap and the 500 ms config-save debounce even when
/// nothing else arrives, so an idle engine still publishes and saves.
fn run_engine_loop(
    engine: &mut Engine,
    input_rx: &crossbeam_channel::Receiver<InputEvent>,
    host_results: &crossbeam_channel::Receiver<InputEvent>,
    net_rx: &crossbeam_channel::Receiver<NetEvt>,
    cmds: &crossbeam_channel::Receiver<Cmd>,
    publish_tick: &crossbeam_channel::Receiver<Instant>,
) {
    'running: loop {
        crossbeam_channel::select! {
            recv(input_rx) -> event => {
                if let Ok(event) = event {
                    engine.on_input(event);
                }
            }
            recv(host_results) -> event => {
                if let Ok(event) = event {
                    engine.on_input(event);
                }
            }
            recv(net_rx) -> evt => {
                if let Ok(evt) = evt {
                    engine.on_net(evt);
                }
            }
            recv(cmds) -> cmd => match cmd {
                Ok(cmd) => engine.on_cmd(cmd),
                Err(_) => break 'running,
            },
            recv(publish_tick) -> _ => {}
        }
        engine.maybe_save_config();
        engine.publish_if_dirty();
    }
}

struct Online {
    name: String,
    os: OsKind,
    fingerprint: String,
    addr: String,
    code: Option<String>,
    stable_id: Option<usize>,
    rtt_p50_us: Option<u64>,
    rtt_p95_us: Option<u64>,
    loss_percent: f64,
    max_beat_gap_ms: u64,
    max_beat_recv_gap_ms: u64,
    stalled: bool,
}

struct Engine {
    dir: PathBuf,
    config: Config,
    layout: Layout,
    session: LocalSession,
    control: ControlMachine,
    host: Arc<dyn Host>,
    peers: PeerStore,
    transfers: TransferBook,
    log: Option<Log>,
    online: HashMap<String, Online>,
    /// Latest session status per peer id, from `NetEvt::Phase` (N4.2).
    peer_status: HashMap<String, String>,
    discovered: Vec<hop_net::PeerAdvert>,
    button: bool,
    remote: Option<RemoteSession>,
    /// The peer whose mouse is driving this computer right now.
    controller: Option<String>,
    net: UnboundedSender<NetCmd>,
    ui: std::sync::mpsc::Sender<UiEvent>,
    snap: Arc<Mutex<Snapshot>>,
    clips: Vec<(u64, ClipKind, Vec<u8>)>,
    status: String,
    local_code: String,
    pending_pair: Option<(String, String)>,
    wanted_code: Option<String>,
    host_jobs: crossbeam_channel::Sender<HostJob>,
    pending_clipboard_peer: Option<String>,
    snapshot_dirty: bool,
    last_publish: Instant,
    config_pending: bool,
    config_deadline: Instant,
    permissions: hop_platform::PermissionStatus,
}

impl Engine {
    fn mark_snapshot_dirty(&mut self) {
        self.snapshot_dirty = true;
    }

    fn schedule_config_save(&mut self) {
        self.config_pending = true;
        self.config_deadline = Instant::now() + Duration::from_millis(500);
    }

    fn maybe_save_config(&mut self) {
        if self.config_pending && Instant::now() >= self.config_deadline {
            let _ = self.config.save(&self.dir);
            self.config_pending = false;
        }
    }

    fn flush_config(&mut self) {
        if self.config_pending {
            let _ = self.config.save(&self.dir);
            self.config_pending = false;
        }
    }

    fn publish_if_dirty(&mut self) {
        if !self.snapshot_dirty {
            return;
        }
        if Instant::now().duration_since(self.last_publish) < Duration::from_millis(50) {
            return;
        }
        self.publish_snapshot();
    }

    fn publish_if_dirty_force(&mut self) {
        if self.snapshot_dirty {
            self.publish_snapshot();
        }
    }

    fn on_input(&mut self, event: InputEvent) {
        match event {
            InputEvent::Motion { x, y, dx, dy } => self.on_motion(x, y, dx, dy),
            InputEvent::Button { button, down } => self.on_button(button, down),
            InputEvent::Key { hid, down } => self.on_key(hid, down),
            InputEvent::Wheel { dx, dy } => self.on_wheel(dx, dy),
            InputEvent::Hotkey(action) => self.on_hotkey(action),
            InputEvent::Displays(displays) => {
                self.refresh_displays(&displays);
                self.mark_snapshot_dirty();
            }
            InputEvent::ClipboardRead { items } => self.on_clipboard_read(items),
            InputEvent::ClipboardWritten { ok } => {
                if !ok {
                    self.status = "clipboard write failed".into();
                    self.mark_snapshot_dirty();
                }
            }
            InputEvent::DragFinished { accepted } => {
                if !accepted {
                    self.status = "drag could not start".into();
                    self.mark_snapshot_dirty();
                }
            }
            InputEvent::PermissionsChanged(perms) => {
                self.permissions = perms;
                self.mark_snapshot_dirty();
            }
        }
    }

    fn on_clipboard_read(&mut self, items: Vec<ClipboardItem>) {
        let Some(peer) = self.pending_clipboard_peer.take() else {
            return;
        };
        let prepared = prepare(&items);
        if prepared.announce.is_empty() {
            return;
        }
        let token = 1;
        self.send_control(
            &peer,
            StreamMsg::ClipboardAnnounce {
                token,
                items: prepared.announce,
            },
        );
        for (kind, bytes) in prepared.inline {
            self.send_control(
                &peer,
                StreamMsg::ClipboardChunk {
                    token,
                    kind,
                    seq: 0,
                    last: true,
                    bytes,
                },
            );
        }
        self.mark_snapshot_dirty();
    }

    fn on_motion(&mut self, x: i32, y: i32, dx: i32, dy: i32) {
        if let Some(peer) = self.controlling() {
            let _ = self.net.send(NetCmd::SendMotion { peer, dx, dy });
            return;
        }
        if self.controller.is_some() {
            return;
        }
        let online = self.crossable();
        match on_motion(
            &self.layout,
            &mut self.session,
            x,
            y,
            dx,
            dy,
            self.config.paused,
            &online,
        ) {
            MotionEffect::Stay => {}
            MotionEffect::Warp { x, y, monitor } => {
                self.session.monitor = monitor;
                self.host.warp(x, y);
            }
            MotionEffect::Handoff {
                peer,
                x,
                y,
                monitor,
            } => {
                let decision = dnd::begin_if_holding(&mut self.control, peer.clone(), self.button);
                let commands = decision.commands;
                self.apply(commands);
                self.send_clipboard(&peer.0);
                self.send_control(&peer.0, enter_msg(&self.layout, &monitor, x, y));
                self.remote = Some(RemoteSession::new(peer));
            }
        }
        self.mark_snapshot_dirty();
    }

    fn on_button(&mut self, button: u8, down: bool) {
        if button == 0 {
            self.button = down;
        }
        let Some(peer) = self.controlling() else {
            return;
        };
        let event = if down {
            ControlEvent::ButtonDown(button)
        } else {
            ControlEvent::ButtonUp(button)
        };
        let commands = self.control.handle(event);
        self.apply(commands);
        self.send_control(&peer, StreamMsg::Button { button, down });
    }

    fn on_key(&mut self, hid: u16, down: bool) {
        let Some(peer) = self.controlling() else {
            return;
        };
        if hid == 0x29 && down {
            let commands = dnd::cancel(&mut self.control);
            self.apply(commands);
        }
        let event = if down {
            ControlEvent::KeyDown(hid)
        } else {
            ControlEvent::KeyUp(hid)
        };
        let commands = self.control.handle(event);
        self.apply(commands);
        let mapped = self.map_out(&peer, hid);
        self.send_control(&peer, StreamMsg::Key { hid: mapped, down });
    }

    fn on_wheel(&mut self, dx: i32, dy: i32) {
        let Some(peer) = self.controlling() else {
            return;
        };
        let (dx, dy) = hop_core::normalize_wheel(self.config.scroll, dx, dy);
        self.send_control(&peer, StreamMsg::Wheel { dx, dy });
    }

    fn on_hotkey(&mut self, action: hop_core::HotAction) {
        if matches!(action, hop_core::HotAction::Panic) {
            let commands = self.control.handle(ControlEvent::Panic);
            self.apply(commands);
            self.host.release_inputs();
            self.status = "panic: inputs released".into();
            self.mark_snapshot_dirty();
            return;
        }
        if matches!(action, hop_core::HotAction::ReturnHome) {
            let commands = dnd::home(&mut self.control);
            self.apply(commands);
            return;
        }
        let Some(effect) = on_hotkey(
            &self.layout,
            &mut self.session,
            self.config.hop_mode,
            action,
        ) else {
            return;
        };
        match effect {
            crate::local::HotEffect::Warp { x, y, monitor } => {
                self.session.monitor = monitor;
                self.host.warp(x, y);
            }
            crate::local::HotEffect::FindCursor { x, y } => {
                let _ = self.ui.send(UiEvent::FindCursor { x, y });
            }
            crate::local::HotEffect::Lock(locked) => {
                self.session.locked = locked;
            }
            crate::local::HotEffect::Panic | crate::local::HotEffect::ReturnHome => {}
            crate::local::HotEffect::JumpDevice(_) => {}
        }
        self.mark_snapshot_dirty();
    }

    fn on_cmd(&mut self, cmd: Cmd) {
        let mut save_config = false;
        match cmd {
            Cmd::SetPaused(paused) => {
                self.config.paused = paused;
                save_config = true;
            }
            Cmd::SetHopMode(mode) => {
                self.config.hop_mode = mode;
                save_config = true;
            }
            Cmd::SetWrap(wrap) => {
                self.config.wrap = wrap;
                self.layout.wrap = wrap;
                save_config = true;
            }
            Cmd::SetHotkey { action, chord } => {
                set_hotkey(&mut self.config, &action, &chord);
                self.host.set_hotkeys(self.config.chords());
                save_config = true;
            }
            Cmd::SetClipboard(enabled) => {
                self.config.clipboard_sync = enabled;
                save_config = true;
            }
            Cmd::SetModifiers(map) => {
                self.config.modifiers = map;
                save_config = true;
            }
            Cmd::SetScroll(scroll) => {
                self.config.scroll = scroll;
                save_config = true;
            }
            Cmd::ConfirmPair { id, code } => {
                self.pending_pair = Some((id.clone(), code.clone()));
                if self.online.contains_key(&id) {
                    let _ = self.net.send(NetCmd::Confirm { id, code });
                    self.pending_pair = None;
                } else if self.discovered.iter().any(|item| item.device_id == id) {
                    // Discovery owns dialing; this confirm is sent when the
                    // session appears (`NetEvt::Online`).
                    self.status = "connecting to that computer".into();
                } else {
                    self.status = "that computer is not on the network yet".into();
                }
            }
            Cmd::PairByCode(code) => self.begin_pair(&code),
            Cmd::RejectPair(id) => {
                let _ = self.net.send(NetCmd::Reject(id));
            }
            Cmd::Unpair(id) => {
                self.peers.forget(&id);
                let _ = self.peers.save(&self.dir);
                let _ = self.net.send(NetCmd::Forget(id));
            }
            Cmd::AddIp(text) => {
                if let Ok(addr) = parse_addr(&text, self.config.port) {
                    let _ = self.net.send(NetCmd::Dial(addr));
                } else {
                    self.status = format!("could not parse {text}");
                }
            }
            Cmd::MoveMonitor { id, x, y } => {
                if let Some(monitor) = self.layout.monitors.iter_mut().find(|m| m.id.0 == id) {
                    monitor.bounds.x = x;
                    monitor.bounds.y = y;
                }
                self.broadcast_layout();
            }
            Cmd::CancelTransfer(id) => {
                if self.transfers.cancel_flag(id) {
                    let _ = self.net.send(NetCmd::CancelTransfer(id));
                }
            }
            Cmd::OpenTransfer(id) => {
                if let Some(item) = self.transfers.items.iter().find(|item| item.id == id) {
                    self.status = format!("open {}", item.path);
                    let _ = self.ui.send(UiEvent::Status(self.status.clone()));
                }
            }
            Cmd::PromptPermissions => {
                let _ = self.host_jobs.send(HostJob::PromptPermissions);
            }
        }
        if save_config {
            self.schedule_config_save();
        }
        self.mark_snapshot_dirty();
    }

    fn on_net(&mut self, evt: NetEvt) {
        match evt {
            NetEvt::Advert(advert) => {
                let matches = self
                    .wanted_code
                    .as_deref()
                    .is_some_and(|code| device_code(&advert.fingerprint).as_deref() == Some(code));
                let code = self.wanted_code.clone();
                self.discovered
                    .retain(|known| known.device_id != advert.device_id);
                self.discovered.push(advert);
                if matches {
                    if let Some(code) = code {
                        self.begin_pair(&code);
                    }
                }
            }
            NetEvt::Online {
                id,
                name,
                os,
                fingerprint,
                addr,
                code,
            } => {
                let fingerprint_code = device_code(&fingerprint);
                self.online.insert(
                    id.clone(),
                    Online {
                        name,
                        os,
                        fingerprint,
                        addr,
                        code,
                        stable_id: None,
                        rtt_p50_us: None,
                        rtt_p95_us: None,
                        loss_percent: 0.0,
                        max_beat_gap_ms: 0,
                        max_beat_recv_gap_ms: 0,
                        stalled: false,
                    },
                );
                self.send_control(&id, layout_msg(&self.local_layout()));
                self.peers = PeerStore::load(&self.dir);
                // N4.1: a code typed before the session existed is applied here,
                // as soon as the peer comes online (never by dialing).
                let mut confirm: Option<(String, String)> = None;
                if let Some((pending_id, pending_code)) = self.pending_pair.clone() {
                    if pending_id == id {
                        confirm = Some((pending_id, pending_code));
                    }
                }
                if confirm.is_none() {
                    if let Some(wanted) = self.wanted_code.clone() {
                        if fingerprint_code.as_deref() == Some(wanted.as_str()) {
                            confirm = Some((id.clone(), wanted));
                        }
                    }
                }
                if let Some((confirm_id, confirm_code)) = confirm {
                    self.pending_pair = None;
                    self.wanted_code = None;
                    let _ = self.net.send(NetCmd::Confirm {
                        id: confirm_id,
                        code: confirm_code,
                    });
                }
                let name = self.online.get(&id).map_or(id.clone(), |p| p.name.clone());
                let paired = self.online.get(&id).is_some_and(|p| p.code.is_none());
                self.status = if paired {
                    format!("connected to {name}")
                } else {
                    format!("connected to {name}. Type its code above to pair.")
                };
            }
            NetEvt::Phase { id, status } => {
                self.peer_status.insert(id, status);
            }
            NetEvt::AdvertGone(id) => {
                self.discovered.retain(|known| known.device_id != id);
            }
            NetEvt::Paired(id) => {
                self.peers = PeerStore::load(&self.dir);
                if let Some(online) = self.online.get_mut(&id) {
                    online.code = None;
                }
                self.pending_pair = None;
                self.wanted_code = None;
            }
            NetEvt::Stalled(id) => self.lose_peer(&id),
            NetEvt::Offline(id) => {
                self.online.remove(&id);
                self.lose_peer(&id);
            }
            NetEvt::Datagram { peer, bytes } => {
                let _ = peer;
                if let Some(Datagram::Motion { dx, dy, .. }) = netloop::decode_packet(&bytes) {
                    if self.controller.is_some() {
                        self.host.inject_rel(dx, dy);
                    }
                } else if let Some(Datagram::Heartbeat { .. }) = netloop::decode_packet(&bytes) {
                    if self.controlling().is_some() {
                        let commands = self.control.handle(ControlEvent::Heartbeat);
                        self.apply(commands);
                    }
                }
            }
            NetEvt::Message { peer, msg } => self.on_msg(&peer, msg),
            NetEvt::Transfer {
                id,
                done,
                total,
                path,
                state,
            } => {
                if self.transfers.items.iter().all(|item| item.id != id) {
                    let assigned = self.transfers.start(path.clone(), total);
                    let _ = assigned;
                }
                if state == "done" || state == "error" {
                    self.transfers.finish(id, path, &state);
                } else {
                    self.transfers.progress(id, done, total, 0);
                }
            }
            NetEvt::Log(line) => {
                self.status = line.clone();
                if let Some(log) = &self.log {
                    log.write_line(&line);
                }
            }
            NetEvt::Stats {
                peer,
                stable_id,
                rtt_p50_us,
                rtt_p95_us,
                loss_percent,
                max_beat_gap_ms,
                max_beat_recv_gap_ms,
                stalled,
            } => {
                if let Some(online) = self.online.get_mut(&peer) {
                    online.stable_id = Some(stable_id);
                    online.rtt_p50_us = rtt_p50_us;
                    online.rtt_p95_us = rtt_p95_us;
                    online.loss_percent = loss_percent;
                    online.max_beat_gap_ms = max_beat_gap_ms;
                    online.max_beat_recv_gap_ms = max_beat_recv_gap_ms;
                    online.stalled = stalled;
                }
            }
        }
        self.mark_snapshot_dirty();
    }

    fn on_msg(&mut self, peer: &str, msg: StreamMsg) {
        match msg {
            StreamMsg::Key { hid, down } => {
                if self.controller.is_some() {
                    self.host.inject_key(hid, down);
                }
            }
            StreamMsg::Button { button, down } => {
                if self.controller.is_some() {
                    self.host.inject_button(button, down);
                }
            }
            StreamMsg::Wheel { dx, dy } => {
                if self.controller.is_some() {
                    self.host.inject_wheel(dx, dy);
                }
            }
            StreamMsg::ReleaseAll | StreamMsg::Leave => self.release_controller(),
            StreamMsg::Enter {
                monitor_id,
                nx_milli,
                ny_milli,
            } => {
                self.controller = Some(peer.to_string());
                if let Some(monitor) = self.layout.monitor(&MonitorId(monitor_id)).cloned() {
                    let (x, y) = denormalize(
                        monitor.bounds,
                        f64::from(nx_milli) / 1000.0,
                        f64::from(ny_milli) / 1000.0,
                    );
                    self.host.warp(x, y);
                }
            }
            StreamMsg::ClipboardAnnounce { token, items: _ } => {
                self.clips.retain(|(id, _, _)| *id != token);
            }
            StreamMsg::ClipboardChunk {
                token,
                kind,
                bytes,
                last,
                ..
            } => {
                if let Some((_, _, buf)) = self
                    .clips
                    .iter_mut()
                    .find(|(id, k, _)| *id == token && *k == kind)
                {
                    buf.extend(bytes);
                } else {
                    self.clips.push((token, kind, bytes));
                }
                if last {
                    self.flush_clip(token, kind);
                }
            }
            StreamMsg::Layout { bytes } => self.merge_layout(&bytes),
            StreamMsg::DragStart { token, files } => {
                self.status = format!("drag {token} ({} files)", files.len());
            }
            StreamMsg::DragCancel { .. } => {
                self.controller = None;
            }
            StreamMsg::DragDrop { .. } => {
                let paths = self.host.clipboard_files();
                if !paths.is_empty() {
                    let _ = self.host_jobs.send(HostJob::BeginFileDrag(paths));
                }
            }
            StreamMsg::TransferRequest { .. } => {
                let paths = self.host.clipboard_files();
                if !paths.is_empty() {
                    let id = self.transfers.start("clipboard".into(), 0);
                    let _ = self.net.send(NetCmd::SendFiles {
                        peer: peer.into(),
                        id,
                        paths,
                    });
                }
            }
            StreamMsg::PairConfirm
            | StreamMsg::Hello { .. }
            | StreamMsg::HelloAck { .. }
            | StreamMsg::PairReject
            | StreamMsg::InputRejected => {}
        }
    }

    /// R5: the peer went quiet, so hand control back and release every key on
    /// whichever side it was driving.
    fn lose_peer(&mut self, id: &str) {
        if self.controlling().as_deref() == Some(id) {
            let commands = self.control.handle(ControlEvent::Disconnect {
                peer: DeviceId(id.to_string()),
            });
            self.apply(commands);
        }
        if self.controller.as_deref() == Some(id) {
            self.release_controller();
        }
    }

    fn release_controller(&mut self) {
        self.controller = None;
        self.host.release_inputs();
        self.host.show_cursor();
    }

    fn flush_clip(&mut self, token: u64, kind: ClipKind) {
        let Some(index) = self
            .clips
            .iter()
            .position(|(id, k, _)| *id == token && *k == kind)
        else {
            return;
        };
        let (_, _, bytes) = self.clips.remove(index);
        let item = match kind {
            ClipKind::Text => ClipboardItem::Text(String::from_utf8_lossy(&bytes).into_owned()),
            ClipKind::Html => ClipboardItem::Html(String::from_utf8_lossy(&bytes).into_owned()),
            ClipKind::Rtf => ClipboardItem::Rtf(bytes),
            ClipKind::Png => ClipboardItem::Png(bytes),
            ClipKind::Files => return,
        };
        let _ = self.host_jobs.send(HostJob::ClipboardWrite(vec![item]));
    }

    fn apply(&mut self, commands: Vec<ControlCommand>) {
        for command in commands {
            match command {
                ControlCommand::HideCursor => self.host.hide_cursor(),
                ControlCommand::ShowCursor => {
                    self.host.show_cursor();
                    self.host.set_remote(false, 0, 0);
                }
                ControlCommand::ParkCursor => {
                    let (x, y) = self.host.cursor_pos().unwrap_or((0, 0));
                    self.host.set_remote(true, x, y);
                }
                ControlCommand::Forward { peer } => {
                    self.remote = Some(RemoteSession::new(peer));
                }
                ControlCommand::StopForward => {
                    if let Some(peer) = self.controlling() {
                        self.send_control(&peer, StreamMsg::Leave);
                    }
                    self.remote = None;
                }
                ControlCommand::ReleaseAll { peer } => {
                    self.send_control(&peer.0, StreamMsg::ReleaseAll);
                    self.host.release_inputs();
                }
                ControlCommand::BeginDrag { peer } => {
                    let files = drag_files(&self.host.read_edge_drag());
                    self.send_control(&peer.0, StreamMsg::DragStart { token: 1, files });
                }
                ControlCommand::FinishDrag { peer } => {
                    self.send_control(&peer.0, StreamMsg::DragDrop { token: 1 });
                }
                ControlCommand::CancelDrag { peer } => {
                    self.send_control(&peer.0, StreamMsg::DragCancel { token: 1 });
                }
            }
        }
    }

    fn send_clipboard(&mut self, peer: &str) {
        if !self.config.clipboard_sync {
            return;
        }
        self.pending_clipboard_peer = Some(peer.to_string());
        let _ = self.host_jobs.send(HostJob::ClipboardRead);
    }

    fn send_control(&self, peer: &str, msg: StreamMsg) {
        let _ = self.net.send(NetCmd::SendControl {
            peer: peer.into(),
            msg,
        });
    }

    fn broadcast_layout(&self) {
        let msg = layout_msg(&self.local_layout());
        for id in self.online.keys() {
            self.send_control(id, msg.clone());
        }
    }

    fn merge_layout(&mut self, bytes: &[u8]) {
        let Ok(remote) = serde_json::from_slice::<Layout>(bytes) else {
            return;
        };
        let ids: HashSet<_> = remote.devices.iter().map(|d| d.id.clone()).collect();
        self.layout.monitors.retain(|m| !ids.contains(&m.device_id));
        self.layout.devices.retain(|d| !ids.contains(&d.id));
        let shift = self
            .layout
            .monitors
            .iter()
            .map(|m| m.bounds.right())
            .max()
            .unwrap_or(0);
        let remote_left = remote
            .monitors
            .iter()
            .map(|m| m.bounds.x)
            .min()
            .unwrap_or(0);
        let dx = shift.saturating_sub(remote_left);
        for mut monitor in remote.monitors {
            monitor.bounds.x = monitor.bounds.x.saturating_add(dx);
            self.layout.monitors.push(monitor);
        }
        self.layout.devices.extend(remote.devices);
    }

    fn refresh_displays(&mut self, displays: &[DisplayInfo]) {
        let device = self.session.device.clone();
        let name = self.config.device_name.clone();
        let fresh = layout_from(&device, &name, displays, self.layout.wrap);
        self.layout.monitors.retain(|m| m.device_id != device);
        self.layout.devices.retain(|d| d.id != device);
        self.layout.devices.extend(fresh.devices);
        self.layout.monitors.extend(fresh.monitors);
    }

    fn local_layout(&self) -> Layout {
        let device = &self.session.device;
        Layout {
            devices: self
                .layout
                .devices
                .iter()
                .filter(|d| &d.id == device)
                .cloned()
                .collect(),
            monitors: self
                .layout
                .monitors
                .iter()
                .filter(|m| &m.device_id == device)
                .cloned()
                .collect(),
            wrap: self.layout.wrap,
        }
    }

    fn controlling(&self) -> Option<String> {
        match self.control.phase() {
            hop_core::Phase::Local => None,
            hop_core::Phase::Remote { peer } => Some(peer.0.clone()),
        }
    }

    fn crossable(&self) -> HashSet<DeviceId> {
        self.online
            .iter()
            .filter(|(_, peer)| peer.code.is_none())
            .map(|(id, _)| DeviceId(id.clone()))
            .collect()
    }

    /// One status per peer, taken from `NetEvt::Phase` (which the net thread
    /// derives from the session state machine) when known, else from what the
    /// engine has observed (N4.2).
    fn peer_status_of(&self, id: &str, online: bool, paired: bool) -> String {
        match self.peer_status.get(id) {
            Some(status) => status.clone(),
            None if online && paired => "paired".into(),
            None if online => "unpaired".into(),
            None => "discovered".into(),
        }
    }

    fn map_out(&self, peer: &str, hid: u16) -> u16 {
        let Some(online) = self.online.get(peer) else {
            return hid;
        };
        let Some(remote) = as_host_os(online.os) else {
            return hid;
        };
        translate_hid(self.config.modifiers, local_host_os(), remote, hid)
    }

    fn begin_pair(&mut self, code: &str) {
        self.wanted_code = Some(code.to_string());
        let connected = self
            .online
            .iter()
            .find(|(_, peer)| device_code(&peer.fingerprint).as_deref() == Some(code))
            .map(|(id, peer)| hop_net::PeerAdvert {
                name: peer.name.clone(),
                device_id: id.clone(),
                fingerprint: peer.fingerprint.clone(),
                addr: peer
                    .addr
                    .parse()
                    .unwrap_or_else(|_| ([0, 0, 0, 0], 0).into()),
            });
        let Some(advert) = connected.or_else(|| {
            self.discovered
                .iter()
                .find(|item| device_code(&item.fingerprint).as_deref() == Some(code))
                .cloned()
        }) else {
            self.status = "Looking for that code. Leave DevHop open on the other computer.".into();
            return;
        };
        if self.peers.by_fingerprint(&advert.fingerprint).is_some() {
            self.status = format!("{} is already paired", advert.name);
            self.wanted_code = None;
            return;
        }
        if self.online.contains_key(&advert.device_id) {
            let _ = self.net.send(NetCmd::Confirm {
                id: advert.device_id,
                code: code.to_string(),
            });
            self.wanted_code = None;
            self.status = format!("pairing with {}", advert.name);
        } else {
            // N4.1: pairing never dials. Remember the code and let discovery
            // (or AddIp) bring the session up; the confirm goes out from
            // `NetEvt::Online` when the peer appears.
            self.pending_pair = Some((advert.device_id, code.to_string()));
            self.status = format!("connecting to {}", advert.name);
        }
    }

    fn publish_snapshot(&mut self) {
        let perms = &self.permissions;
        let mut peers = Vec::new();
        for advert in &self.discovered {
            let online = self.online.get(&advert.device_id);
            let paired = self.peers.by_fingerprint(&advert.fingerprint).is_some();
            peers.push(PeerView {
                id: advert.device_id.clone(),
                name: advert.name.clone(),
                addr: advert.addr.to_string(),
                fingerprint: advert.fingerprint.clone(),
                paired,
                online: online.is_some(),
                status: self.peer_status_of(&advert.device_id, online.is_some(), paired),
                os: online.map(|p| format!("{:?}", p.os)).unwrap_or_default(),
                code: device_code(&advert.fingerprint),
                stable_id: online.and_then(|p| p.stable_id),
                rtt_p50_ms: online.and_then(|p| p.rtt_p50_us).map(micros_to_ms),
                rtt_p95_ms: online.and_then(|p| p.rtt_p95_us).map(micros_to_ms),
                loss_percent: online.map_or(0.0, |p| p.loss_percent),
                max_beat_gap_ms: online.map_or(0, |p| p.max_beat_gap_ms),
                max_beat_recv_gap_ms: online.map_or(0, |p| p.max_beat_recv_gap_ms),
                stalled: online.is_some_and(|p| p.stalled),
            });
        }
        for (id, online) in &self.online {
            if peers.iter().any(|peer| &peer.id == id) {
                continue;
            }
            let paired =
                self.peers.by_fingerprint(&online.fingerprint).is_some() || online.code.is_none();
            peers.push(PeerView {
                id: id.clone(),
                name: online.name.clone(),
                addr: online.addr.clone(),
                fingerprint: online.fingerprint.clone(),
                paired,
                online: true,
                status: self.peer_status_of(id, true, paired),
                os: format!("{:?}", online.os),
                code: device_code(&online.fingerprint),
                stable_id: online.stable_id,
                rtt_p50_ms: online.rtt_p50_us.map(micros_to_ms),
                rtt_p95_ms: online.rtt_p95_us.map(micros_to_ms),
                loss_percent: online.loss_percent,
                max_beat_gap_ms: online.max_beat_gap_ms,
                max_beat_recv_gap_ms: online.max_beat_recv_gap_ms,
                stalled: online.stalled,
            });
        }
        let code = self.online.values().find_map(|peer| peer.code.clone());
        let snapshot = Snapshot {
            device_id: self.config.device_id.clone(),
            device_name: self.config.device_name.clone(),
            paused: self.config.paused,
            locked: self.session.locked,
            remote: self.controlling(),
            hop_mode: format!("{:?}", self.config.hop_mode),
            wrap: self.config.wrap,
            clipboard_sync: self.config.clipboard_sync,
            modifiers: self.config.modifiers,
            scroll: self.config.scroll,
            hotkeys: self.config.hotkeys.clone(),
            peers,
            monitors: self.layout.monitors.iter().map(monitor_view).collect(),
            transfers: self
                .transfers
                .items
                .iter()
                .cloned()
                .map(transfer_view)
                .collect(),
            pairing_code: code,
            local_code: self.local_code.clone(),
            local_addr: format!("{}:{}", hop_net::advertised_ip(), self.config.port),
            accessibility: perms.accessibility,
            input_monitoring: perms.input_monitoring,
            status: self.status.clone(),
            port: self.config.port,
        };
        *self.snap.lock().expect("snapshot") = snapshot;
        self.last_publish = Instant::now();
        self.snapshot_dirty = false;
    }
}

fn device_code(fingerprint: &str) -> Option<String> {
    if fingerprint.is_empty() {
        None
    } else {
        Some(hop_net::local_code(fingerprint))
    }
}

fn micros_to_ms(us: u64) -> f64 {
    us as f64 / 1000.0
}

fn transfer_view(item: crate::transfer::TransferSnap) -> TransferView {
    TransferView {
        id: item.id,
        label: item.label,
        bytes_done: item.bytes_done,
        bytes_total: item.bytes_total,
        bytes_per_sec: item.bytes_per_sec,
        state: item.state,
        path: item.path,
    }
}

fn monitor_view(monitor: &Monitor) -> MonitorView {
    MonitorView {
        id: monitor.id.0.clone(),
        device_id: monitor.device_id.0.clone(),
        name: monitor.name.clone(),
        x: monitor.bounds.x,
        y: monitor.bounds.y,
        width: monitor.bounds.width,
        height: monitor.bounds.height,
        scale: monitor.scale,
    }
}

fn layout_from(device: &DeviceId, name: &str, displays: &[DisplayInfo], wrap: bool) -> Layout {
    Layout {
        devices: vec![Device {
            id: device.clone(),
            name: name.into(),
        }],
        monitors: displays
            .iter()
            .map(|display| Monitor {
                id: MonitorId(format!("{}:{}", device.0, display.id)),
                device_id: device.clone(),
                name: display.name.clone(),
                bounds: PhysRect::new(display.x, display.y, display.width, display.height),
                scale: display.scale,
            })
            .collect(),
        wrap,
    }
}

fn enter_msg(layout: &Layout, monitor: &MonitorId, x: i32, y: i32) -> StreamMsg {
    let bounds = layout
        .monitor(monitor)
        .map(|m| m.bounds)
        .unwrap_or_else(|| PhysRect::new(0, 0, 1, 1));
    let (nx, ny) = normalize(bounds, x, y);
    StreamMsg::Enter {
        monitor_id: monitor.0.clone(),
        nx_milli: (nx.clamp(0.0, 1.0) * 1000.0) as u16,
        ny_milli: (ny.clamp(0.0, 1.0) * 1000.0) as u16,
    }
}

fn layout_msg(layout: &Layout) -> StreamMsg {
    StreamMsg::Layout {
        bytes: serde_json::to_vec(layout).unwrap_or_default(),
    }
}

fn drag_files(paths: &[PathBuf]) -> Vec<FileAnnounce> {
    paths
        .iter()
        .map(|path| FileAnnounce {
            rel_path: path.to_string_lossy().into_owned(),
            bytes: std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
            is_dir: path.is_dir(),
            mtime_ms: 0,
        })
        .collect()
}

fn set_hotkey(config: &mut Config, action: &str, chord: &str) {
    let hotkeys = &mut config.hotkeys;
    match action {
        "next_monitor" => hotkeys.next_monitor = chord.into(),
        "prev_monitor" => hotkeys.prev_monitor = chord.into(),
        "monitor_n" => hotkeys.monitor_n = chord.into(),
        "center" => hotkeys.center = chord.into(),
        "find_cursor" => hotkeys.find_cursor = chord.into(),
        "lock_monitor" => hotkeys.lock_monitor = chord.into(),
        "jump_device_n" => hotkeys.jump_device_n = chord.into(),
        "return_home" => hotkeys.return_home = chord.into(),
        "panic" => hotkeys.panic = chord.into(),
        _ => {}
    }
}

fn parse_addr(text: &str, default_port: u16) -> Result<SocketAddr, ()> {
    if let Ok(addr) = text.parse() {
        return Ok(addr);
    }
    let text = text.trim();
    if let Ok(ip) = text.parse::<std::net::IpAddr>() {
        return Ok(SocketAddr::new(ip, default_port));
    }
    Err(())
}

fn as_host_os(kind: OsKind) -> Option<HostOs> {
    match kind {
        OsKind::Windows => Some(HostOs::Windows),
        OsKind::Macos => Some(HostOs::Macos),
        OsKind::Other => None,
    }
}

fn local_host_os() -> HostOs {
    if cfg!(windows) {
        HostOs::Windows
    } else {
        HostOs::Macos
    }
}

fn empty_snapshot() -> Snapshot {
    Snapshot {
        device_id: String::new(),
        device_name: String::new(),
        paused: false,
        locked: false,
        remote: None,
        hop_mode: "Relative".into(),
        wrap: false,
        clipboard_sync: true,
        modifiers: ModifierMap::default(),
        scroll: ScrollNorm::default(),
        hotkeys: crate::config::HotkeyConfig::default(),
        peers: Vec::new(),
        monitors: Vec::new(),
        transfers: Vec::new(),
        pairing_code: None,
        local_code: String::new(),
        local_addr: String::new(),
        accessibility: true,
        input_monitoring: true,
        status: "starting".into(),
        port: 42424,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hop_platform::MockHost;

    #[test]
    fn pause_command_reaches_the_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let host = Arc::new(MockHost::new(vec![DisplayInfo {
            id: "1".into(),
            name: "Main".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            scale: 1.0,
            primary: true,
        }]));
        let config = Config {
            discovery: false,
            port: 47910,
            ..Config::default()
        };
        config.save(dir.path()).unwrap();
        let (handle, _ui) = start_in(dir.path().to_path_buf(), host);
        let mut ready = false;
        for _ in 0..100 {
            if !handle.snapshot().device_id.is_empty() {
                ready = true;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(ready);
        handle.command(Cmd::SetPaused(true));
        let mut paused = false;
        for _ in 0..100 {
            if handle.snapshot().paused {
                paused = true;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(paused);
        assert_eq!(handle.snapshot().monitors.len(), 1);
    }

    fn engine_on(port: u16) -> (EngineHandle, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            port,
            discovery: false,
            ..Config::default()
        };
        config.save(dir.path()).unwrap();
        let host = Arc::new(MockHost::new(vec![DisplayInfo {
            id: "1".into(),
            name: "Main".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            scale: 1.0,
            primary: true,
        }]));
        let (handle, _ui) = start_in(dir.path().to_path_buf(), host);
        (handle, dir)
    }

    /// Like [`engine_on`], but hands back the mock host so a test can inspect
    /// injected keys and releases.
    fn engine_on_with_host(port: u16) -> (EngineHandle, Arc<MockHost>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            port,
            discovery: false,
            ..Config::default()
        };
        config.save(dir.path()).unwrap();
        let host = Arc::new(MockHost::new(vec![DisplayInfo {
            id: "1".into(),
            name: "Main".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            scale: 1.0,
            primary: true,
        }]));
        let (handle, _ui) = start_in(dir.path().to_path_buf(), host.clone());
        (handle, host, dir)
    }

    /// Whether `hid` is currently held down on the mock host (the last injected
    /// event for it is a key-down).
    fn mock_key_held(host: &MockHost, hid: u16) -> bool {
        host.state()
            .injected_keys
            .iter()
            .rev()
            .find(|(key, _)| *key == hid)
            .is_some_and(|(_, down)| *down)
    }

    fn wait_until(handle: &EngineHandle, secs: u64, test: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let deadline = std::time::Instant::now() + Duration::from_secs(secs);
        loop {
            let snap = handle.snapshot();
            if test(&snap) || std::time::Instant::now() > deadline {
                return snap;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// The same commands the Pair button sends, through two whole engines.
    #[test]
    fn two_engines_pair_with_the_codes_from_each_screen() {
        let (a, a_dir) = engine_on(47911);
        let (b, b_dir) = engine_on(47912);
        let a_code = wait_until(&a, 5, |s| !s.local_code.is_empty()).local_code;
        let b_code = wait_until(&b, 5, |s| !s.local_code.is_empty()).local_code;
        assert!(!a_code.is_empty() && !b_code.is_empty());

        a.command(Cmd::AddIp("127.0.0.1:47912".into()));
        let snap = wait_until(&a, 10, |s| s.peers.iter().any(|p| p.online));
        assert!(
            snap.peers.iter().any(|p| p.online),
            "a never connected: {}",
            snap.status
        );
        let snap = wait_until(&b, 10, |s| s.peers.iter().any(|p| p.online));
        assert!(
            snap.peers.iter().any(|p| p.online),
            "b never connected: {}",
            snap.status
        );

        a.command(Cmd::PairByCode(b_code));
        b.command(Cmd::PairByCode(a_code));

        let paired = |s: &Snapshot| s.peers.iter().any(|p| p.online && p.paired);
        let snap = wait_until(&a, 10, paired);
        assert!(paired(&snap), "a not paired: {}", snap.status);
        let snap = wait_until(&b, 10, paired);
        assert!(paired(&snap), "b not paired: {}", snap.status);
        assert!(std::fs::read_to_string(a_dir.path().join("peers.toml")).is_ok());
        assert!(std::fs::read_to_string(b_dir.path().join("peers.toml")).is_ok());
    }

    /// Counts the netloop's user-visible "dialing <addr>" lines in an engine's
    /// log. Each one is one dial (AddIp or discovery); pairing must add none.
    fn count_dials(dir: &std::path::Path) -> usize {
        let Ok(text) = std::fs::read_to_string(dir.join("logs").join("devhop.log")) else {
            return 0;
        };
        text.lines()
            .filter(|line| line.contains("dialing "))
            .count()
    }

    /// N4.1: clicking Pair must never dial. Two engines (discovery off) are
    /// connected by AddIp, which dials once per side. Both then click Pair
    /// within a second. They must pair on the first try, ten times in a row,
    /// and each log must still show exactly one dial (the AddIp, not the Pair).
    #[test]
    fn pairing_never_dials_and_pairs_first_try_ten_times() {
        for run in 0..10u16 {
            let a_port = 47950 + run * 2;
            let b_port = a_port + 1;
            let (a, a_dir) = engine_on(a_port);
            let (b, b_dir) = engine_on(b_port);
            let a_code = wait_until(&a, 5, |s| !s.local_code.is_empty()).local_code;
            let b_code = wait_until(&b, 5, |s| !s.local_code.is_empty()).local_code;
            assert!(
                !a_code.is_empty() && !b_code.is_empty(),
                "run {run}: an engine never produced a code"
            );

            // AddIp is the only dialer here (discovery is off in tests).
            a.command(Cmd::AddIp(format!("127.0.0.1:{b_port}")));
            b.command(Cmd::AddIp(format!("127.0.0.1:{a_port}")));
            let online = |s: &Snapshot| s.peers.iter().any(|p| p.online);
            assert!(
                online(&wait_until(&a, 10, online)),
                "run {run}: a never connected: {}",
                a.snapshot().status
            );
            assert!(
                online(&wait_until(&b, 10, online)),
                "run {run}: b never connected: {}",
                b.snapshot().status
            );

            // Both people click Pair within a second.
            a.command(Cmd::PairByCode(b_code.clone()));
            b.command(Cmd::PairByCode(a_code.clone()));

            let paired = |s: &Snapshot| s.peers.iter().any(|p| p.online && p.paired);
            assert!(
                paired(&wait_until(&a, 10, paired)),
                "run {run}: a never paired: {}",
                a.snapshot().status
            );
            assert!(
                paired(&wait_until(&b, 10, paired)),
                "run {run}: b never paired: {}",
                b.snapshot().status
            );

            assert_eq!(
                count_dials(a_dir.path()),
                1,
                "run {run}: side a dialed more than once (pairing must not dial)"
            );
            assert_eq!(
                count_dials(b_dir.path()),
                1,
                "run {run}: side b dialed more than once (pairing must not dial)"
            );
        }
    }

    fn test_engine(
        dir: &std::path::Path,
        host: Arc<MockHost>,
    ) -> (
        Engine,
        crossbeam_channel::Receiver<InputEvent>,
        tokio::sync::mpsc::UnboundedReceiver<NetCmd>,
    ) {
        let config = Config::load_or_create(dir).unwrap_or_default();
        let device = DeviceId(config.device_id.clone());
        let displays = host.list_displays();
        let layout = layout_from(&device, &config.device_name, &displays, config.wrap);
        let primary = layout
            .monitors
            .first()
            .map(|m| m.id.clone())
            .unwrap_or_else(|| MonitorId("local:0".into()));
        let session = LocalSession {
            device: device.clone(),
            monitor: primary,
            nx: 0.5,
            ny: 0.5,
            memory: hop_core::PositionMemory::default(),
            locked: false,
        };
        let (net, net_rx) = {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            (tx, rx)
        };
        let (ui_tx, _ui_rx) = std::sync::mpsc::channel();
        let snap = Arc::new(Mutex::new(empty_snapshot()));
        let (host_jobs, host_results) = spawn_host_worker(host.clone());
        let permissions = host.permissions();
        let engine = Engine {
            dir: dir.to_path_buf(),
            config,
            layout,
            session,
            control: ControlMachine::new(),
            host,
            peers: PeerStore::load(dir),
            transfers: TransferBook::new(),
            log: None,
            online: HashMap::new(),
            peer_status: HashMap::new(),
            discovered: Vec::new(),
            button: false,
            remote: None,
            controller: None,
            net,
            ui: ui_tx,
            snap,
            clips: Vec::new(),
            status: "test".into(),
            local_code: String::new(),
            pending_pair: None,
            wanted_code: None,
            host_jobs,
            pending_clipboard_peer: None,
            snapshot_dirty: true,
            last_publish: Instant::now() - Duration::from_secs(1),
            config_pending: false,
            config_deadline: Instant::now(),
            permissions,
        };
        (engine, host_results, net_rx)
    }

    #[test]
    fn motion_is_injected_while_slow_file_drag_runs() {
        use hop_proto::{encode_datagram, Datagram};

        let dir = tempfile::tempdir().unwrap();
        let host = Arc::new(MockHost::new(vec![DisplayInfo {
            id: "1".into(),
            name: "Main".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            scale: 1.0,
            primary: true,
        }]));
        host.set_drag_delay_ms(400);
        host.clipboard_write(&[ClipboardItem::Files(vec![PathBuf::from("test.txt")])])
            .unwrap();
        let (mut engine, host_results, _net_rx) = test_engine(dir.path(), host.clone());
        engine.controller = Some("peer".into());
        engine.on_msg("peer", StreamMsg::DragDrop { token: 1 });

        // The host worker is now blocked for 400 ms inside `begin_file_drag`.
        // The engine thread must still inject motion during that window
        // (N3.3) instead of waiting for the drag to finish.
        let motion = encode_datagram(&Datagram::Motion {
            seq: 1,
            dx: 17,
            dy: 3,
            sent_us: 0,
        })
        .unwrap();
        thread::sleep(Duration::from_millis(100));
        engine.on_net(NetEvt::Datagram {
            peer: "peer".into(),
            bytes: motion,
        });
        assert_eq!(
            host.cursor_pos().unwrap(),
            (17, 3),
            "engine should inject motion while drag blocks the host worker"
        );

        // The drag eventually reports back without us having waited for it.
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut finished = false;
        while Instant::now() < deadline && !finished {
            while let Ok(event) = host_results.try_recv() {
                engine.on_input(event);
            }
            finished = host.state().drag == vec![PathBuf::from("test.txt")];
            thread::sleep(Duration::from_millis(5));
        }
        assert!(finished, "the drag never reached the host worker");
    }

    /// N3.1: the engine's own input-to-send time. Timestamp an input event as it
    /// is enqueued on the engine's input channel (what the OS hook does) and
    /// again when the engine has handed the resulting `NetCmd::SendMotion` to
    /// the net thread's command channel. This measures the engine loop only
    /// (wake from `select!`, geometry, route); it is not network latency. Uses
    /// the mock host; no sockets are opened.
    #[test]
    fn engine_input_to_net_send_latency_p95_is_under_half_a_millisecond() {
        let dir = tempfile::tempdir().unwrap();
        let host = Arc::new(MockHost::new(vec![DisplayInfo {
            id: "1".into(),
            name: "Main".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            scale: 1.0,
            primary: true,
        }]));
        let (input_tx, input_rx) = crossbeam_channel::unbounded::<InputEvent>();
        let (_net_evt_tx, net_evt_rx) = crossbeam_channel::unbounded::<NetEvt>();
        let (cmds_tx, cmds_rx) = crossbeam_channel::unbounded::<Cmd>();
        let (mut engine, host_results, mut net_rx) = test_engine(dir.path(), host);
        // Put the engine in the "driving a peer" state so motion is forwarded
        // instead of handled as a local hop.
        engine.control.handle(ControlEvent::CrossTo {
            peer: DeviceId("peer".into()),
            button_held: false,
        });
        assert_eq!(engine.controlling().as_deref(), Some("peer"));

        let tick = crossbeam_channel::tick(Duration::from_millis(50));
        let loop_thread = thread::spawn(move || {
            run_engine_loop(
                &mut engine,
                &input_rx,
                &host_results,
                &net_evt_rx,
                &cmds_rx,
                &tick,
            );
        });

        const N: usize = 3000;
        let mut samples = Vec::with_capacity(N);
        for i in 0..N {
            let enqueued = Instant::now();
            input_tx
                .send(InputEvent::Motion {
                    x: i as i32,
                    y: 0,
                    dx: 1,
                    dy: 0,
                })
                .unwrap();
            match net_rx.blocking_recv() {
                Some(NetCmd::SendMotion { .. }) => samples.push(enqueued.elapsed()),
                _ => panic!("expected the engine to hand a SendMotion to the net thread"),
            }
        }
        drop(cmds_tx);
        loop_thread.join().unwrap();

        samples.sort_unstable();
        let idx = ((samples.len() as f64 * 0.95).ceil() as usize).saturating_sub(1);
        let p95 = samples[idx.min(samples.len() - 1)];
        let p50 = samples[samples.len() / 2];
        eprintln!("N3.1 engine input-to-send: p50 {p50:?}, p95 {p95:?} over {N} events");
        assert!(
            p95 < Duration::from_micros(500),
            "engine input-to-send p95 {p95:?} is not under 500 us (p50 {p50:?}) over {N} events"
        );
    }

    #[test]
    fn layout_editor_moves_do_not_debounce_save_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            discovery: false,
            port: 47920,
            ..Config::default()
        };
        config.save(dir.path()).unwrap();
        let before = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        let host = Arc::new(MockHost::new(vec![DisplayInfo {
            id: "1".into(),
            name: "Main".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            scale: 1.0,
            primary: true,
        }]));
        let (handle, _ui) = start_in(dir.path().to_path_buf(), host);
        wait_until(&handle, 5, |s| !s.device_id.is_empty());
        let monitor_id = handle.snapshot().monitors[0].id.clone();
        for i in 0..30 {
            handle.command(Cmd::MoveMonitor {
                id: monitor_id.clone(),
                x: i * 10,
                y: 0,
            });
            thread::sleep(Duration::from_millis(5));
        }
        thread::sleep(Duration::from_millis(700));
        let after = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn config_changes_debounce_before_disk_write() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            discovery: false,
            port: 47921,
            ..Config::default()
        };
        config.save(dir.path()).unwrap();
        let host = Arc::new(MockHost::new(vec![DisplayInfo {
            id: "1".into(),
            name: "Main".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            scale: 1.0,
            primary: true,
        }]));
        let (handle, _ui) = start_in(dir.path().to_path_buf(), host);
        wait_until(&handle, 5, |s| !s.device_id.is_empty());
        handle.command(Cmd::SetPaused(true));
        thread::sleep(Duration::from_millis(100));
        let early = Config::load_or_create(dir.path()).unwrap();
        assert!(
            !early.paused,
            "config should not be written before debounce elapses"
        );
        thread::sleep(Duration::from_millis(500));
        let late = Config::load_or_create(dir.path()).unwrap();
        assert!(late.paused);
    }

    /// N1.4: a peer whose connection dies must make the engine report `Offline`
    /// within 3.5 s and release any keys it was holding within 300 ms. The
    /// controller here is a raw hop-net link driven by the test, so we can pull
    /// the plug without going through the engine's own APIs.
    #[test]
    fn abrupt_disconnect_goes_offline_and_releases_held_keys() {
        let port = 47930;
        let (handle, host, _dir) = engine_on_with_host(port);
        let b_code = wait_until(&handle, 5, |s| !s.local_code.is_empty()).local_code;
        assert!(!b_code.is_empty(), "engine never produced a code");

        let ctrl_dir = tempfile::tempdir().unwrap();
        let ctrl = hop_net::Identity::load_or_create(ctrl_dir.path()).unwrap();
        let ctrl_code = hop_net::local_code(&ctrl.fingerprint);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let endpoint = hop_net::Endpoint::bind(&ctrl, 0).unwrap();
            let link = endpoint
                .dial(format!("127.0.0.1:{port}").parse().unwrap())
                .await
                .unwrap();
            let hello = StreamMsg::Hello {
                version: hop_proto::PROTO_VERSION,
                device_id: "controller".into(),
                name: "controller".into(),
                os: OsKind::Windows,
            };
            let opener = link.clone();
            let writing = tokio::spawn(async move {
                let (mut send, recv) = opener.open_control().await.unwrap();
                hop_net::write_msg(&mut send, &hello).await.unwrap();
                (send, recv)
            });
            let (kind, _b_send, mut b_recv) = link.accept_typed().await.unwrap();
            assert_eq!(kind, hop_net::STREAM_CONTROL);
            assert!(hop_net::read_msg(&mut b_recv).await.unwrap().is_some());
            let (mut send, _recv) = writing.await.unwrap();

            let online = wait_engine(&handle, 5, |s| {
                s.peers.iter().any(|p| p.online && p.id == "controller")
            })
            .await;
            assert!(online, "controller never came online");

            // The person at the controlled machine types the code they see.
            handle.command(Cmd::ConfirmPair {
                id: "controller".into(),
                code: ctrl_code,
            });
            hop_net::write_msg(&mut send, &StreamMsg::PairConfirm)
                .await
                .unwrap();
            let paired = wait_engine(&handle, 5, |s| {
                s.peers.iter().any(|p| p.paired && p.id == "controller")
            })
            .await;
            assert!(paired, "never paired");

            // Enter makes this machine the controlled one; then hold a key.
            let hid: u16 = 0x04;
            hop_net::write_msg(
                &mut send,
                &StreamMsg::Enter {
                    monitor_id: "controller:1".into(),
                    nx_milli: 500,
                    ny_milli: 500,
                },
            )
            .await
            .unwrap();
            hop_net::write_msg(&mut send, &StreamMsg::Key { hid, down: true })
                .await
                .unwrap();
            let mut held = false;
            for _ in 0..400 {
                if mock_key_held(&host, hid) {
                    held = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert!(held, "the held key was never injected");

            // Pull the plug: close the connection and drop the endpoint.
            let dropped = Instant::now();
            link.close();
            drop(send);
            drop(link);
            drop(endpoint);

            let mut released = None;
            while dropped.elapsed() < Duration::from_millis(300) {
                if !mock_key_held(&host, hid) {
                    released = Some(dropped.elapsed());
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert!(
                released.is_some(),
                "held key was not released within 300 ms of the disconnect"
            );
        });

        let offline = wait_until(&handle, 4, |s| {
            !s.peers.iter().any(|p| p.online && p.id == "controller")
        });
        assert!(
            !offline
                .peers
                .iter()
                .any(|p| p.online && p.id == "controller"),
            "engine never reported the peer offline: {}",
            offline.status
        );
    }

    /// A small async wait for an engine snapshot predicate (so an async test can
    /// keep the controller's QUIC driver running while it polls).
    async fn wait_engine(
        handle: &EngineHandle,
        secs: u64,
        test: impl Fn(&Snapshot) -> bool,
    ) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if test(&handle.snapshot()) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
