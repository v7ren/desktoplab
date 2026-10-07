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
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

use crate::clipboard::prepare;
use crate::config::Config;
use crate::dnd;
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
    pub os: String,
    pub code: Option<String>,
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
    ConfirmPair(String),
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
    cmd: std::sync::mpsc::Sender<Cmd>,
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

pub fn config_dir() -> PathBuf {
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
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
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
    cmds: std::sync::mpsc::Receiver<Cmd>,
    ui: std::sync::mpsc::Sender<UiEvent>,
    snap: Arc<Mutex<Snapshot>>,
) {
    log::init_tracing();
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
    let (net, net_rx) = if let Some(identity) = identity {
        netloop::spawn(
            dir.clone(),
            incoming,
            identity,
            config.device_id.clone(),
            config.device_name.clone(),
            config.port,
            peers.clone(),
        )
    } else {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        drop(rx);
        let (_tx, evt_rx) = std::sync::mpsc::channel();
        (tx, evt_rx)
    };
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
        discovered: Vec::new(),
        button: false,
        remote: None,
        controlled: false,
        net,
        ui,
        snap,
        clips: Vec::new(),
        status: "ready".into(),
    };
    engine.publish();
    loop {
        let mut idle = true;
        while let Ok(event) = input_rx.try_recv() {
            idle = false;
            engine.on_input(event);
        }
        while let Ok(cmd) = cmds.try_recv() {
            idle = false;
            if matches!(cmd, Cmd::PromptPermissions) {
                engine.host.prompt_permissions();
            } else {
                engine.on_cmd(cmd);
            }
        }
        while let Ok(evt) = net_rx.try_recv() {
            idle = false;
            engine.on_net(evt);
        }
        if idle {
            thread::sleep(Duration::from_millis(8));
        }
        engine.publish();
    }
}

struct Online {
    name: String,
    os: OsKind,
    fingerprint: String,
    addr: String,
    code: Option<String>,
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
    discovered: Vec<hop_net::PeerAdvert>,
    button: bool,
    remote: Option<RemoteSession>,
    controlled: bool,
    net: UnboundedSender<NetCmd>,
    ui: std::sync::mpsc::Sender<UiEvent>,
    snap: Arc<Mutex<Snapshot>>,
    clips: Vec<(u64, ClipKind, Vec<u8>)>,
    status: String,
}

impl Engine {
    fn on_input(&mut self, event: InputEvent) {
        match event {
            InputEvent::Motion { x, y, dx, dy } => self.on_motion(x, y, dx, dy),
            InputEvent::Button { button, down } => self.on_button(button, down),
            InputEvent::Key { hid, down } => self.on_key(hid, down),
            InputEvent::Wheel { dx, dy } => self.on_wheel(dx, dy),
            InputEvent::Hotkey(action) => self.on_hotkey(action),
            InputEvent::Displays(displays) => {
                self.refresh_displays(&displays);
            }
        }
    }

    fn on_motion(&mut self, x: i32, y: i32, dx: i32, dy: i32) {
        if let Some(peer) = self.controlling() {
            let _ = self.net.send(NetCmd::SendMotion { peer, dx, dy });
            return;
        }
        if self.controlled {
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
    }

    fn on_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::SetPaused(paused) => self.config.paused = paused,
            Cmd::SetHopMode(mode) => self.config.hop_mode = mode,
            Cmd::SetWrap(wrap) => {
                self.config.wrap = wrap;
                self.layout.wrap = wrap;
            }
            Cmd::SetHotkey { action, chord } => {
                set_hotkey(&mut self.config, &action, &chord);
                self.host.set_hotkeys(self.config.chords());
            }
            Cmd::SetClipboard(enabled) => self.config.clipboard_sync = enabled,
            Cmd::SetModifiers(map) => self.config.modifiers = map,
            Cmd::SetScroll(scroll) => self.config.scroll = scroll,
            Cmd::ConfirmPair(id) => {
                let _ = self.net.send(NetCmd::Confirm(id));
            }
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
            Cmd::PromptPermissions => self.host.prompt_permissions(),
        }
        let _ = self.config.save(&self.dir);
    }

    fn on_net(&mut self, evt: NetEvt) {
        match evt {
            NetEvt::Advert(advert) => {
                self.discovered
                    .retain(|known| known.device_id != advert.device_id);
                self.discovered.push(advert);
            }
            NetEvt::Online {
                id,
                name,
                os,
                fingerprint,
                addr,
                code,
            } => {
                self.online.insert(
                    id.clone(),
                    Online {
                        name,
                        os,
                        fingerprint,
                        addr,
                        code,
                    },
                );
                self.send_control(&id, layout_msg(&self.local_layout()));
                self.status = format!("connected {id}");
            }
            NetEvt::Offline(id) => {
                self.online.remove(&id);
                let commands = self
                    .control
                    .handle(ControlEvent::Disconnect { peer: DeviceId(id) });
                self.apply(commands);
            }
            NetEvt::Datagram { peer, bytes } => {
                let _ = peer;
                if let Some(Datagram::Motion { dx, dy, .. }) = netloop::decode_packet(&bytes) {
                    if self.controlled {
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
        }
    }

    fn on_msg(&mut self, peer: &str, msg: StreamMsg) {
        match msg {
            StreamMsg::Key { hid, down } => {
                if self.controlled {
                    self.host.inject_key(hid, down);
                }
            }
            StreamMsg::Button { button, down } => {
                if self.controlled {
                    self.host.inject_button(button, down);
                }
            }
            StreamMsg::Wheel { dx, dy } => {
                if self.controlled {
                    self.host.inject_wheel(dx, dy);
                }
            }
            StreamMsg::ReleaseAll | StreamMsg::Leave => {
                self.controlled = false;
                self.host.release_inputs();
                self.host.show_cursor();
            }
            StreamMsg::Enter {
                monitor_id,
                nx_milli,
                ny_milli,
            } => {
                self.controlled = true;
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
                self.controlled = false;
            }
            StreamMsg::DragDrop { .. } => {
                let paths = self.host.clipboard_files();
                if !paths.is_empty() {
                    let _ = self.host.begin_file_drag(&paths);
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
            StreamMsg::PairConfirm => {
                if let Some(online) = self.online.get_mut(peer) {
                    online.code = None;
                }
            }
            StreamMsg::Hello { .. }
            | StreamMsg::HelloAck { .. }
            | StreamMsg::PairReject
            | StreamMsg::InputRejected => {}
        }
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
        let _ = self.host.clipboard_write(&[item]);
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
        let prepared = prepare(&self.host.clipboard_read());
        if prepared.announce.is_empty() {
            return;
        }
        let token = 1;
        self.send_control(
            peer,
            StreamMsg::ClipboardAnnounce {
                token,
                items: prepared.announce,
            },
        );
        for (kind, bytes) in prepared.inline {
            self.send_control(
                peer,
                StreamMsg::ClipboardChunk {
                    token,
                    kind,
                    seq: 0,
                    last: true,
                    bytes,
                },
            );
        }
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

    fn map_out(&self, peer: &str, hid: u16) -> u16 {
        let Some(online) = self.online.get(peer) else {
            return hid;
        };
        let Some(remote) = as_host_os(online.os) else {
            return hid;
        };
        translate_hid(self.config.modifiers, local_host_os(), remote, hid)
    }

    fn publish(&self) {
        let perms = self.host.permissions();
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
                os: online.map(|p| format!("{:?}", p.os)).unwrap_or_default(),
                code: online.and_then(|p| p.code.clone()),
            });
        }
        for (id, online) in &self.online {
            if peers.iter().any(|peer| &peer.id == id) {
                continue;
            }
            peers.push(PeerView {
                id: id.clone(),
                name: online.name.clone(),
                addr: online.addr.clone(),
                fingerprint: online.fingerprint.clone(),
                paired: self.peers.by_fingerprint(&online.fingerprint).is_some()
                    || online.code.is_none(),
                online: true,
                os: format!("{:?}", online.os),
                code: online.code.clone(),
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
            accessibility: perms.accessibility,
            input_monitoring: perms.input_monitoring,
            status: self.status.clone(),
            port: self.config.port,
        };
        *self.snap.lock().expect("snapshot") = snapshot;
    }
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
}
