//! QUIC sessions, discovery, and the pairing gate.
//!
//! The engine thread sends [`NetCmd`] and reads [`NetEvt`]. Mouse motion is a
//! datagram. Each file transfer opens its own low-priority stream.
//!
//! Nothing in the main loop awaits the network for long: accepting, dialing and
//! the hello exchange run in their own tasks and report back as [`Internal`]
//! messages. A handshake that sat inside `select!` would be cancelled whenever
//! another branch fired first, and would freeze heartbeats while it waited.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hop_net::{
    decide_install, dial_key, is_own_addr, local_code, read_msg, should_dial, write_msg,
    DialLedger, Discovery, Endpoint, Identity, InstallAction, Link, NetError, PairGate, PeerAdvert,
    Phase, RecvStream, RttProbe, SendStream, BEACON_PORT, CLIPBOARD_CHUNK_BYTES, HEARTBEAT_MS,
    MISS_LIMIT, STREAM_CLIPBOARD, STREAM_CONTROL, STREAM_TRANSFER,
};
use hop_proto::{decode_datagram, encode_datagram, Datagram, OsKind, StreamMsg, PROTO_VERSION};
use hop_transfer::{recv_all, send_all};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::MissedTickBehavior;
use tracing::Instrument;

use crate::log::{conn_span, Conn};
use crate::peers::{PeerStore, StoredPeer};

/// Silence after which control is handed back and keys are released (R5).
const STALL_MS: u64 = HEARTBEAT_MS * MISS_LIMIT as u64;
const CONTROL_QUEUE: usize = 256;
/// Wait before dialing a computer again after a failed attempt.
const REDIAL_MS: u64 = 3_000;
const HANDSHAKE_SECS: u64 = 8;
/// Failed dials in a row before a discovered computer is dropped from the list.
const FORGET_AFTER_FAILS: u32 = 3;

pub enum NetCmd {
    Dial(SocketAddr),
    SendMotion {
        peer: String,
        dx: i32,
        dy: i32,
    },
    SendControl {
        peer: String,
        msg: StreamMsg,
    },
    SendFiles {
        peer: String,
        id: u64,
        paths: Vec<PathBuf>,
    },
    CancelTransfer(u64),
    Confirm {
        id: String,
        code: String,
    },
    Reject(String),
    Forget(String),
    #[allow(dead_code)]
    Shutdown,
}

#[derive(Clone, Debug)]
pub enum NetEvt {
    Advert(PeerAdvert),
    Online {
        id: String,
        name: String,
        os: OsKind,
        fingerprint: String,
        addr: String,
        code: Option<String>,
    },
    /// A discovered computer has not been seen for a while and left the list.
    AdvertGone(String),
    /// Both people confirmed and the peer is now pinned in `peers.toml`.
    Paired(String),
    /// No heartbeat for [`STALL_MS`]. The session is still open, but control
    /// must go back to the local machine.
    Stalled(String),
    Offline(String),
    /// The peer's session status, derived from [`Phase`] (N4.2): one of
    /// `discovered`, `connecting`, `unpaired`, `paired`, `offline`.
    Phase {
        id: String,
        status: String,
    },
    Datagram {
        peer: String,
        bytes: Vec<u8>,
    },
    Message {
        peer: String,
        msg: StreamMsg,
    },
    Transfer {
        id: u64,
        done: u64,
        total: u64,
        path: String,
        state: String,
    },
    /// Shown to the user in the status line (and written to the log).
    Log(String),
    /// Per-session heartbeat RTT and loss, sampled by the net thread. The engine
    /// mirrors it into the snapshot so the soak tool and the UI can read live
    /// numbers off the engine link (N0.2, no separate probe connection).
    Stats {
        peer: String,
        stable_id: usize,
        rtt_p50_us: Option<u64>,
        rtt_p95_us: Option<u64>,
        loss_percent: f64,
        /// Largest gap between heartbeats *sent* by this side (N2.4).
        max_beat_gap_ms: u64,
        /// Largest gap between heartbeats *received*; includes datagram loss.
        max_beat_recv_gap_ms: u64,
        stalled: bool,
    },
}

enum Internal {
    Message {
        peer: String,
        msg: StreamMsg,
    },
    Datagram {
        peer: String,
        bytes: Vec<u8>,
    },
    Transfer {
        id: u64,
        done: u64,
        total: u64,
        path: String,
        state: String,
    },
    DialFinished {
        key: String,
        result: Result<Link, String>,
    },
    Accepted(Link),
    Handshake(Box<Handshake>),
    HandshakeFailed(String),
    Closed {
        peer: String,
        conn: usize,
    },
}

struct Handshake {
    link: Link,
    send: SendStream,
    recv: RecvStream,
    peer_id: String,
    name: String,
    os: OsKind,
}

/// Heartbeat bookkeeping shared between the per-session heartbeat task (which
/// records when each beat was sent) and the datagram reader (which records the
/// echo, the RTT, and the gap between received beats). Small and touched at
/// most a few times a second, never on the motion path.
#[derive(Default)]
struct BeatStats {
    rtt: RttProbe,
    beats_sent: u64,
    beats_echoed: u64,
    next_seq: u64,
    /// Send time (ms since the node started) of the last beat we sent.
    last_send_ms: u64,
    /// Largest gap between two beats we *sent*: this is whether the heartbeat
    /// task itself was starved by other work (N2.4).
    max_send_gap_ms: u64,
    /// Arrival time (ms since the node started) of the last beat request we saw.
    last_request_ms: u64,
    /// Largest gap between two beat requests we *received*; includes datagram
    /// loss, so it is informational only.
    max_recv_gap_ms: u64,
}

type BeatShared = Arc<std::sync::Mutex<BeatStats>>;

struct Session {
    id: String,
    name: String,
    os: OsKind,
    link: Link,
    gate: PairGate,
    phase: Phase,
    last_heard: u64,
    stalled: bool,
    control_tx: mpsc::Sender<StreamMsg>,
    clip_announce: Option<StreamMsg>,
    beats: BeatShared,
}

struct Net {
    endpoint: Endpoint,
    evts: crossbeam_channel::Sender<NetEvt>,
    internal_tx: UnboundedSender<Internal>,
    dir: PathBuf,
    incoming: PathBuf,
    device_id: String,
    device_name: String,
    fingerprint: String,
    peers: PeerStore,
    sessions: HashMap<String, Session>,
    known: HashMap<String, PeerAdvert>,
    cancels: HashMap<u64, Arc<AtomicBool>>,
    /// Dial dedupe, retry backoff and failure counts (pure logic in `hop-net`).
    dialing: DialLedger,
    /// Pairing progress for unpinned peers whose connection dropped, by fingerprint.
    parked_gates: HashMap<String, PairGate>,
    /// Device ids to drop from discovery on the next tick.
    forget_adverts: Vec<String>,
    /// LAN discovery. Owned here so a session that becomes `Connected` can lock
    /// its peer's address immediately (N5.4).
    discovery: Option<Discovery>,
    port: u16,
    started: Instant,
}

#[allow(clippy::too_many_arguments)]
pub fn spawn(
    dir: PathBuf,
    incoming: PathBuf,
    identity: Identity,
    device_id: String,
    device_name: String,
    port: u16,
    discovery: bool,
    peers: PeerStore,
) -> (UnboundedSender<NetCmd>, crossbeam_channel::Receiver<NetEvt>) {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (evt_tx, evt_rx) = crossbeam_channel::unbounded();
    std::thread::Builder::new()
        .name("devhop-net".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(err) => {
                    let _ = evt_tx.send(NetEvt::Log(format!("runtime: {err}")));
                    return;
                }
            };
            rt.block_on(run(
                dir,
                incoming,
                identity,
                device_id,
                device_name,
                port,
                discovery,
                peers,
                cmd_rx,
                evt_tx,
            ));
        })
        .ok();
    (cmd_tx, evt_rx)
}

#[allow(clippy::too_many_arguments)]
async fn run(
    dir: PathBuf,
    incoming: PathBuf,
    identity: Identity,
    device_id: String,
    device_name: String,
    port: u16,
    discover: bool,
    peers: PeerStore,
    mut cmds: UnboundedReceiver<NetCmd>,
    evts: crossbeam_channel::Sender<NetEvt>,
) {
    let endpoint = match Endpoint::bind(&identity, port) {
        Ok(endpoint) => endpoint,
        Err(err) => {
            let _ = evts.send(NetEvt::Log(format!(
                "cannot listen on UDP port {port}: {err}. Is another copy of DevHop running?"
            )));
            return;
        }
    };
    let started = if discover {
        Some(Discovery::start(
            &device_name,
            &device_id,
            &identity.fingerprint,
            port,
        ))
    } else {
        None
    };
    let discovery = match started {
        None => None,
        Some(Ok(discovery)) => Some(discovery),
        Some(Err(err)) => {
            let _ = evts.send(NetEvt::Log(format!(
                "discovery is off ({err}). Add the other computer by IP address."
            )));
            None
        }
    };
    tracing::info!(
        port,
        code = %local_code(&identity.fingerprint),
        "netloop listening; discovery is {}",
        if discovery.is_some() { "on" } else { "off" }
    );
    if discover {
        for port in [port, BEACON_PORT] {
            if let Some(note) = allow_inbound(port) {
                let _ = evts.send(NetEvt::Log(note));
            }
        }
    }
    if let Some(note) = discovery.as_ref().and_then(Discovery::note) {
        let _ = evts.send(NetEvt::Log(note));
    }
    let (internal_tx, mut internal_rx) = tokio::sync::mpsc::unbounded_channel();
    spawn_accept_loop(endpoint.clone(), internal_tx.clone());
    let started = Instant::now();
    let mut net = Net {
        endpoint,
        evts,
        internal_tx,
        dir,
        incoming,
        device_id,
        device_name,
        fingerprint: identity.fingerprint.clone(),
        peers,
        sessions: HashMap::new(),
        known: HashMap::new(),
        cancels: HashMap::new(),
        dialing: DialLedger::new(),
        parked_gates: HashMap::new(),
        forget_adverts: Vec::new(),
        discovery,
        port,
        started,
    };
    // An interval keeps its schedule no matter how busy the other branches are.
    // A fresh `sleep` in every `select!` pass would never fire under a steady
    // stream of mouse datagrams, so heartbeats would stop and the peer would
    // drop the session.
    let mut tick = tokio::time::interval(Duration::from_millis(HEARTBEAT_MS));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            cmd = cmds.recv() => {
                let Some(cmd) = cmd else { break };
                if !net.handle_cmd(cmd) {
                    break;
                }
            }
            internal = internal_rx.recv() => {
                if let Some(internal) = internal {
                    net.on_internal(internal);
                }
            }
            _ = tick.tick() => {
                net.discover();
                net.poll_sessions();
            }
        }
    }
}

impl Net {
    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn log(&self, line: impl Into<String>) {
        let _ = self.evts.send(NetEvt::Log(line.into()));
    }

    fn on_internal(&mut self, internal: Internal) {
        match internal {
            Internal::Accepted(link) => {
                let addr = link.peer_addr.to_string();
                Conn::new(link.id(), &addr).info("accepted incoming connection");
                self.start_handshake(link, addr);
            }
            Internal::DialFinished { key, result } => {
                match result {
                    Ok(link) => {
                        self.dialing.succeeded(&key);
                        Conn::new(link.id(), &key).info("dial succeeded");
                        self.start_handshake(link, key);
                    }
                    Err(err) => {
                        let forget =
                            self.dialing
                                .failed(&key, self.now_ms(), REDIAL_MS, FORGET_AFTER_FAILS);
                        if forget && self.known.contains_key(&key) {
                            self.forget_adverts.push(key.clone());
                        }
                        let name = self.known.get(&key).map_or(key.clone(), |a| a.name.clone());
                        let line = format!("could not reach {name}: {err}");
                        tracing::warn!(peer = %key, "dial failed: {err}");
                        // Keys that parse as an address came from "Add by IP"; the
                        // user is waiting on those. Automatic redials stay quiet.
                        if key.parse::<SocketAddr>().is_ok() {
                            self.log(line);
                        }
                    }
                }
            }
            Internal::Handshake(handshake) => self.install(*handshake),
            Internal::HandshakeFailed(reason) => tracing::warn!("{reason}"),
            Internal::Closed { peer, conn } => {
                let current = self
                    .sessions
                    .get(&peer)
                    .is_some_and(|session| session.link.id() == conn);
                if current {
                    Conn::new(conn, &peer).warn("connection closed by peer or transport");
                    self.drop_session(&peer, "connection closed");
                } else {
                    Conn::new(conn, &peer).debug("late close for a replaced link");
                }
            }
            Internal::Datagram { peer, bytes } => {
                self.heard(&peer);
                if self.trusted(&peer) {
                    let _ = self.evts.send(NetEvt::Datagram { peer, bytes });
                }
            }
            Internal::Message { peer, msg } => {
                self.heard(&peer);
                let handshake = matches!(
                    msg,
                    StreamMsg::PairConfirm
                        | StreamMsg::PairReject
                        | StreamMsg::Hello { .. }
                        | StreamMsg::HelloAck { .. }
                        | StreamMsg::InputRejected
                );
                // R7: until both people confirm, nothing from this peer may
                // reach the engine, where it would be injected as input.
                if !handshake && !self.trusted(&peer) {
                    return;
                }
                match &msg {
                    StreamMsg::PairConfirm => self.remote_confirmed(&peer),
                    StreamMsg::PairReject => {
                        if let Some(session) = self.sessions.get_mut(&peer) {
                            session.gate.reject();
                        }
                        self.log("the other computer rejected pairing");
                    }
                    _ => {}
                }
                let _ = self.evts.send(NetEvt::Message { peer, msg });
            }
            Internal::Transfer {
                id,
                done,
                total,
                path,
                state,
            } => {
                let _ = self.evts.send(NetEvt::Transfer {
                    id,
                    done,
                    total,
                    path,
                    state,
                });
            }
        }
    }

    fn trusted(&self, peer: &str) -> bool {
        self.sessions
            .get(peer)
            .is_some_and(|session| session.gate.accepts_input())
    }

    fn heard(&mut self, peer: &str) {
        let now = self.now_ms();
        if let Some(session) = self.sessions.get_mut(peer) {
            session.last_heard = now;
            session.stalled = false;
        }
    }

    fn remote_confirmed(&mut self, peer: &str) {
        let Some(session) = self.sessions.get_mut(peer) else {
            return;
        };
        session.gate.confirm_remote();
        if session.gate.ready_to_pin() {
            self.pin(peer);
        } else if !session.gate.accepts_input() {
            let name = session.name.clone();
            self.log(format!(
                "{name} entered this computer's code. Type {name}'s code here to finish pairing."
            ));
        }
    }

    fn handle_cmd(&mut self, cmd: NetCmd) -> bool {
        match cmd {
            NetCmd::Shutdown => return false,
            NetCmd::Dial(addr) => {
                if is_own_addr(addr, self.port) {
                    self.log(format!(
                        "{addr} is this computer. Type the other computer's address."
                    ));
                    return true;
                }
                let peer_id = self
                    .known
                    .values()
                    .find(|advert| advert.addr == addr)
                    .map(|advert| advert.device_id.clone());
                if peer_id
                    .as_ref()
                    .is_some_and(|id| self.sessions.contains_key(id))
                {
                    return true;
                }
                let key = dial_key(peer_id.as_deref(), &addr.to_string());
                if self.dialing.in_flight(&key) {
                    return true;
                }
                self.log(format!("dialing {addr}"));
                self.dialing.succeeded(&key);
                self.start_dial(addr, peer_id, key);
            }
            NetCmd::SendMotion { peer, dx, dy } => {
                if let Some(session) = self.sessions.get(&peer) {
                    if session.gate.accepts_input() {
                        let msg = Datagram::Motion {
                            seq: 0,
                            dx,
                            dy,
                            sent_us: 0,
                        };
                        if let Ok(bytes) = encode_datagram(&msg) {
                            let _ = session.link.send_datagram(&bytes);
                        }
                    }
                }
            }
            NetCmd::SendControl { peer, msg } => {
                if let Some(session) = self.sessions.get_mut(&peer) {
                    let allowed = session.gate.accepts_input()
                        || matches!(msg, StreamMsg::PairConfirm | StreamMsg::PairReject);
                    if allowed {
                        let tx = self.internal_tx.clone();
                        send_stream_msg(tx, session, msg);
                    }
                }
            }
            NetCmd::SendFiles { peer, id, paths } => self.send_files(&peer, id, paths),
            NetCmd::CancelTransfer(id) => {
                if let Some(flag) = self.cancels.get(&id) {
                    flag.store(true, Ordering::Relaxed);
                }
            }
            NetCmd::Confirm { id, code } => self.confirm(&id, &code),
            NetCmd::Reject(id) => {
                if let Some(session) = self.sessions.get_mut(&id) {
                    session.gate.reject();
                    let _ = enqueue_control(&session.control_tx, StreamMsg::PairReject);
                }
                self.parked_gates.retain(|_, gate| !gate.accepts_input());
            }
            NetCmd::Forget(id) => {
                self.peers.forget(&id);
                let _ = self.peers.save(&self.dir);
                // Unpairing must discard any half-finished (or already paired)
                // gate, or a reconnect would come back paired without either
                // person confirming again.
                let fingerprint = self
                    .sessions
                    .get(&id)
                    .map(|session| session.link.peer_fingerprint.clone());
                self.drop_session(&id, "forgotten by the user");
                if let Some(fingerprint) = fingerprint {
                    self.parked_gates.remove(&fingerprint);
                }
            }
        }
        true
    }

    fn confirm(&mut self, id: &str, code: &str) {
        let Some(session) = self.sessions.get_mut(id) else {
            self.log("not connected to that computer yet. Keep DevHop open on both computers.");
            return;
        };
        let expected = local_code(&session.link.peer_fingerprint);
        if expected != code.trim() {
            self.log("that code does not match the computer it came from. Check the digits.");
            return;
        }
        session.gate.confirm_local();
        if enqueue_control(&session.control_tx, StreamMsg::PairConfirm).is_err() {
            self.log("could not send the confirmation: control channel closed");
            return;
        }
        if session.gate.ready_to_pin() {
            self.pin(id);
        } else {
            let name = session.name.clone();
            self.log(format!(
                "Code accepted. Now type this computer's code on {name} to finish."
            ));
        }
    }

    fn send_files(&mut self, peer: &str, id: u64, paths: Vec<PathBuf>) {
        let Some(session) = self.sessions.get(peer) else {
            return;
        };
        if !session.gate.accepts_input() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels.insert(id, cancel.clone());
        let link = session.link.clone();
        let tx = self.internal_tx.clone();
        let label = paths
            .first()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        tokio::spawn(async move {
            match link.open_transfer().await {
                Ok((mut send, _recv)) => {
                    let result = send_all(&mut send, id, &paths, &cancel, |progress| {
                        let _ = tx.send(Internal::Transfer {
                            id: progress.id,
                            done: progress.bytes_done,
                            total: progress.bytes_total,
                            path: label.clone(),
                            state: "sending".into(),
                        });
                    })
                    .await;
                    let state = if result.is_ok() { "done" } else { "error" };
                    let _ = tx.send(Internal::Transfer {
                        id,
                        done: 0,
                        total: 0,
                        path: label,
                        state: state.into(),
                    });
                }
                Err(err) => {
                    let _ = tx.send(Internal::Transfer {
                        id,
                        done: 0,
                        total: 0,
                        path: err.to_string(),
                        state: "error".into(),
                    });
                }
            }
        });
    }

    fn discover(&mut self) {
        let forget_ids: Vec<String> = self.forget_adverts.drain(..).collect();
        for id in &forget_ids {
            if let Some(discovery) = self.discovery.as_ref() {
                discovery.forget(id);
            }
            self.dialing.forget(id);
        }
        let current = {
            let Some(discovery) = self.discovery.as_ref() else {
                return;
            };
            discovery.peers()
        };
        let now = self.now_ms();
        let gone: Vec<String> = self
            .known
            .keys()
            .filter(|id| !current.iter().any(|advert| &advert.device_id == *id))
            .cloned()
            .collect();
        for id in gone {
            self.known.remove(&id);
            let _ = self.evts.send(NetEvt::AdvertGone(id));
        }
        for advert in current {
            if is_own_addr(advert.addr, self.port) {
                continue;
            }
            let _ = self.evts.send(NetEvt::Advert(advert.clone()));
            self.known.insert(advert.device_id.clone(), advert.clone());
            let key = dial_key(Some(&advert.device_id), &advert.addr.to_string());
            let waiting = self.dialing.waiting(&key, now);
            if should_dial(&self.device_id, &advert.device_id)
                && !self.sessions.contains_key(&advert.device_id)
                && !self.dialing.in_flight(&key)
                && !advert.addr.ip().is_unspecified()
                && !waiting
            {
                self.start_dial(advert.addr, Some(advert.device_id.clone()), key);
            }
        }
    }

    fn poll_sessions(&mut self) {
        let now = self.now_ms();
        for (id, session) in self.sessions.iter_mut() {
            let silent = now.saturating_sub(session.last_heard);
            if silent >= STALL_MS && !session.stalled {
                session.stalled = true;
                Conn::new(session.link.id(), id)
                    .warn(format!("no heartbeat for {silent} ms; stalled"));
                let _ = self.evts.send(NetEvt::Stalled(id.clone()));
            }
            let (p50, p95, loss, send_gap, recv_gap) = {
                let beats = session.beats.lock().expect("beats");
                let loss = if beats.beats_sent == 0 {
                    0.0
                } else {
                    beats.beats_sent.saturating_sub(beats.beats_echoed) as f64 * 100.0
                        / beats.beats_sent as f64
                };
                (
                    beats.rtt.p50_us(),
                    beats.rtt.p95_us(),
                    loss,
                    beats.max_send_gap_ms,
                    beats.max_recv_gap_ms,
                )
            };
            let _ = self.evts.send(NetEvt::Stats {
                peer: id.clone(),
                stable_id: session.link.id(),
                rtt_p50_us: p50,
                rtt_p95_us: p95,
                loss_percent: loss,
                max_beat_gap_ms: send_gap,
                max_beat_recv_gap_ms: recv_gap,
                stalled: session.stalled,
            });
        }
    }

    fn drop_session(&mut self, id: &str, reason: &str) {
        let conn = self.sessions.get(id).map(|session| session.link.id());
        if let Some(conn) = conn {
            Conn::new(conn, id).warn(format!("closing session: {reason}"));
        }
        if let Some(session) = self.sessions.remove(id) {
            session.link.close();
            let pinned = self
                .peers
                .by_fingerprint(&session.link.peer_fingerprint)
                .is_some();
            if !pinned {
                self.parked_gates
                    .insert(session.link.peer_fingerprint.clone(), session.gate);
            }
        }
        let _ = self.evts.send(NetEvt::Phase {
            id: id.to_string(),
            status: "offline".to_string(),
        });
        let _ = self.evts.send(NetEvt::Offline(id.to_string()));
    }

    fn start_dial(&mut self, addr: SocketAddr, peer_id: Option<String>, key: String) {
        if addr.ip().is_unspecified() {
            return;
        }
        if peer_id
            .as_ref()
            .is_some_and(|id| self.sessions.contains_key(id))
        {
            return;
        }
        if !self.dialing.begin(&key) {
            return;
        }
        if let Some(id) = peer_id.as_deref() {
            let _ = self.evts.send(NetEvt::Phase {
                id: id.to_string(),
                status: "connecting".to_string(),
            });
        }
        Conn::new(0, &key).info(format!("dialing {addr}"));
        let endpoint = self.endpoint.clone();
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            let result = match tokio::time::timeout(Duration::from_secs(5), endpoint.dial(addr))
                .await
            {
                Ok(Ok(link)) => Ok(link),
                Ok(Err(err)) => Err(err.to_string()),
                Err(_) => Err("no answer in 5 s (firewall, or DevHop not running there)".into()),
            };
            let _ = tx.send(Internal::DialFinished { key, result });
        });
    }

    fn start_handshake(&self, link: Link, label: String) {
        let hello = StreamMsg::Hello {
            version: PROTO_VERSION,
            device_id: self.device_id.clone(),
            name: self.device_name.clone(),
            os: local_os(),
        };
        let tx = self.internal_tx.clone();
        let span = conn_span(link.id(), &label);
        tracing::info!("starting handshake");
        tokio::spawn(
            async move {
                match handshake(link, hello).await {
                    Ok(done) => {
                        let _ = tx.send(Internal::Handshake(Box::new(done)));
                    }
                    Err(reason) => {
                        tracing::warn!("handshake failed: {reason}");
                    }
                }
            }
            .instrument(span),
        );
    }

    fn install(&mut self, handshake: Handshake) {
        let Handshake {
            link,
            send,
            recv,
            peer_id,
            name,
            os,
        } = handshake;
        if peer_id == self.device_id {
            Conn::new(link.id(), &peer_id).info(format!(
                "{} is this computer; closing the loopback link",
                link.peer_addr
            ));
            link.close();
            return;
        }
        let mut carried_gate = None;
        if let Some(existing) = self.sessions.get(&peer_id) {
            let link_open = !existing.link.is_closed();
            let action = decide_install(
                &existing.phase,
                link_open,
                link.dialed,
                &self.device_id,
                &peer_id,
            );
            let old_conn = existing.link.id();
            match action {
                InstallAction::RejectNew => {
                    Conn::new(link.id(), &peer_id).info(format!(
                        "closing duplicate link; keeping conn {old_conn} (first healthy link wins)"
                    ));
                    link.close();
                    return;
                }
                InstallAction::ReplaceExisting => {
                    Conn::new(link.id(), &peer_id)
                        .info(format!("replacing existing conn {old_conn}"));
                    if let Some(old) = self.sessions.remove(&peer_id) {
                        old.link.close();
                        if old.link.peer_fingerprint == link.peer_fingerprint {
                            carried_gate = Some(old.gate);
                        }
                    }
                }
                InstallAction::Accept => {
                    Conn::new(link.id(), &peer_id).info(format!(
                        "old conn {old_conn} is closed; installing this link"
                    ));
                    if let Some(old) = self.sessions.remove(&peer_id) {
                        old.link.close();
                        if old.link.peer_fingerprint == link.peer_fingerprint {
                            carried_gate = Some(old.gate);
                        }
                    }
                }
            }
        }
        let pinned = self.peers.by_fingerprint(&link.peer_fingerprint).is_some();
        let gate = if pinned {
            PairGate::new(true, &self.fingerprint, &link.peer_fingerprint)
        } else {
            carried_gate
                .or_else(|| self.parked_gates.remove(&link.peer_fingerprint))
                .unwrap_or_else(|| PairGate::new(false, &self.fingerprint, &link.peer_fingerprint))
        };
        let paired = gate.accepts_input();
        let code = (!pinned).then(|| gate.code().to_string());
        let conn_id = link.id();
        let dialed = link.dialed;
        let beats: BeatShared = Arc::new(std::sync::Mutex::new(BeatStats::default()));
        let (control_tx, control_rx) = mpsc::channel(CONTROL_QUEUE);
        spawn_control_writer(send, control_rx);
        spawn_heartbeat(
            link.clone(),
            peer_id.clone(),
            self.started,
            Arc::clone(&beats),
        );
        spawn_readers(
            &link,
            &peer_id,
            conn_id,
            recv,
            self.internal_tx.clone(),
            Arc::clone(&beats),
            self.started,
        );
        spawn_stream_acceptor(
            link.clone(),
            &peer_id,
            self.incoming.clone(),
            self.dir.join("staging"),
            self.internal_tx.clone(),
        );
        let key = dial_key(Some(&peer_id), &link.peer_addr.to_string());
        self.dialing.succeeded(&key);
        let session = Session {
            id: peer_id.clone(),
            name,
            os,
            link,
            gate,
            phase: Phase::Connected {
                paired,
                link_id: conn_id,
                dialed,
            },
            last_heard: self.now_ms(),
            stalled: false,
            control_tx,
            clip_announce: None,
            beats,
        };
        Conn::new(conn_id, &peer_id).info(format!(
            "session connected with {} ({}){}",
            session.name,
            session.link.peer_addr,
            if paired { ", paired" } else { ", unpaired" }
        ));
        // N5.4: a beacon must never change this peer's address while we have a
        // live session, so a forged beacon cannot redirect QUIC.
        if let Some(discovery) = self.discovery.as_ref() {
            discovery.lock_peer_address(&peer_id);
        }
        let _ = self.evts.send(NetEvt::Phase {
            id: peer_id.clone(),
            status: session.phase.status().to_string(),
        });
        let _ = self.evts.send(online_evt(&session, code));
        self.sessions.insert(peer_id, session);
    }

    fn pin(&mut self, id: &str) {
        let Some(session) = self.sessions.get(id) else {
            return;
        };
        let name = self
            .known
            .get(&session.id)
            .map(|a| a.name.clone())
            .unwrap_or_else(|| session.name.clone());
        self.peers.pin(StoredPeer {
            id: session.id.clone(),
            name: name.clone(),
            fingerprint: session.link.peer_fingerprint.clone(),
            last_addr: session.link.peer_addr.to_string(),
        });
        if let Err(err) = self.peers.save(&self.dir) {
            self.log(format!("paired, but could not save peers.toml: {err}"));
        }
        self.parked_gates.remove(&session.link.peer_fingerprint);
        let (online, status) = {
            let session = self.sessions.get_mut(id).expect("session");
            session.phase = session.phase.clone().set_paired();
            (
                online_evt(session, None),
                session.phase.status().to_string(),
            )
        };
        let _ = self.evts.send(NetEvt::Phase {
            id: id.to_string(),
            status,
        });
        let _ = self.evts.send(online);
        let _ = self.evts.send(NetEvt::Paired(id.to_string()));
        self.log(format!("paired with {name}"));
    }
}

/// Routes a control-stream message: clipboard payloads open their own stream
/// from the link, everything else goes through the session's control writer.
fn send_stream_msg(tx: UnboundedSender<Internal>, session: &mut Session, msg: StreamMsg) {
    match msg {
        StreamMsg::ClipboardAnnounce { .. } => {
            session.clip_announce = Some(msg);
        }
        StreamMsg::ClipboardChunk {
            token,
            kind,
            bytes,
            last,
            ..
        } => {
            let announce = session.clip_announce.clone();
            let link = session.link.clone();
            let peer = session.id.clone();
            let frames = chunk_clipboard(token, kind, bytes, last);
            tokio::spawn(async move {
                if let Err(err) = send_clipboard_frames(link, announce, frames).await {
                    let _ = tx.send(Internal::HandshakeFailed(format!(
                        "clipboard to {peer} failed: {err}"
                    )));
                }
            });
        }
        other => {
            let _ = enqueue_control(&session.control_tx, other);
        }
    }
}

/// Both sides open a control stream and send hello at the same time, then
/// read the other side's hello from the stream it opened.
async fn handshake(link: Link, hello: StreamMsg) -> Result<Handshake, String> {
    let deadline = Duration::from_secs(HANDSHAKE_SECS);
    let opener = link.clone();
    let writing = tokio::spawn(async move {
        let (mut send, recv) = opener.open_control().await?;
        write_msg(&mut send, &hello).await?;
        Ok::<_, NetError>((send, recv))
    });
    let (_remote_send, mut recv) = match tokio::time::timeout(deadline, link.accept_typed()).await {
        Ok(Ok((kind, _send, recv))) if kind == STREAM_CONTROL => (_send, recv),
        Ok(Ok((kind, _, _))) => {
            link.close();
            return Err(format!(
                "handshake with {} used unexpected stream kind {kind}",
                link.peer_addr
            ));
        }
        Ok(Err(err)) => {
            link.close();
            return Err(format!("handshake with {} failed: {err}", link.peer_addr));
        }
        Err(_) => {
            link.close();
            return Err(format!("handshake with {} timed out", link.peer_addr));
        }
    };
    let read = tokio::time::timeout(deadline, read_msg(&mut recv)).await;
    let send = match writing.await {
        Ok(Ok((send, _local_recv))) => send,
        _ => {
            link.close();
            return Err("could not open the control stream".into());
        }
    };
    let (peer_id, name, os, version) = match read {
        Ok(Ok(Some(StreamMsg::Hello {
            device_id,
            name,
            os,
            version,
        })))
        | Ok(Ok(Some(StreamMsg::HelloAck {
            device_id,
            name,
            os,
            version,
        }))) => (device_id, name, os, version),
        _ => {
            link.close();
            return Err(format!("{} did not say hello", link.peer_addr));
        }
    };
    if version != PROTO_VERSION {
        link.close();
        return Err(format!(
            "{name} runs DevHop protocol {version}, this computer runs {PROTO_VERSION}. Update both."
        ));
    }
    Ok(Handshake {
        link,
        send,
        recv,
        peer_id,
        name,
        os,
    })
}

fn spawn_accept_loop(endpoint: Endpoint, tx: UnboundedSender<Internal>) {
    tokio::spawn(async move {
        while let Ok(incoming) = endpoint.incoming().await {
            let tx = tx.clone();
            tokio::spawn(async move {
                let msg = match Endpoint::complete(incoming).await {
                    Ok(link) => Internal::Accepted(link),
                    Err(err) => {
                        Internal::HandshakeFailed(format!("incoming connection failed: {err}"))
                    }
                };
                let _ = tx.send(msg);
            });
        }
    });
}

fn enqueue_control(
    tx: &mpsc::Sender<StreamMsg>,
    msg: StreamMsg,
) -> Result<(), mpsc::error::TrySendError<StreamMsg>> {
    match tx.try_send(msg) {
        Ok(()) => Ok(()),
        Err(mpsc::error::TrySendError::Full(msg)) => {
            if matches!(msg, StreamMsg::Wheel { .. }) {
                Ok(())
            } else {
                tx.try_send(msg)
            }
        }
        Err(err) => Err(err),
    }
}

fn spawn_control_writer(mut send: SendStream, mut rx: mpsc::Receiver<StreamMsg>) {
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if write_msg(&mut send, &msg).await.is_err() {
                break;
            }
        }
    });
}

fn spawn_heartbeat(link: Link, peer: String, started: Instant, stats: BeatShared) {
    let span = conn_span(link.id(), &peer);
    tokio::spawn(
        async move {
            let mut tick = tokio::time::interval(Duration::from_millis(HEARTBEAT_MS));
            tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let now_us = started.elapsed().as_micros() as u64;
                let now_ms = now_us / 1000;
                let seq = {
                    let mut beats = stats.lock().expect("beats");
                    beats.next_seq += 1;
                    let seq = beats.next_seq;
                    beats.rtt.sent(seq, now_us);
                    beats.beats_sent += 1;
                    if beats.last_send_ms != 0 {
                        let gap = now_ms.saturating_sub(beats.last_send_ms);
                        if gap > beats.max_send_gap_ms {
                            beats.max_send_gap_ms = gap;
                        }
                    }
                    beats.last_send_ms = now_ms;
                    seq
                };
                let beat = Datagram::Heartbeat {
                    seq,
                    echo: None,
                    sent_us: now_us,
                };
                if let Ok(bytes) = encode_datagram(&beat) {
                    if link.send_datagram(&bytes).is_err() {
                        break;
                    }
                }
            }
        }
        .instrument(span),
    );
}

fn spawn_readers(
    link: &Link,
    peer_id: &str,
    conn_id: usize,
    mut recv: RecvStream,
    tx: UnboundedSender<Internal>,
    stats: BeatShared,
    started: Instant,
) {
    let peer = peer_id.to_string();
    let control_tx = tx.clone();
    let control_span = conn_span(conn_id, &peer);
    tokio::spawn(
        async move {
            let mut reader = hop_net::FrameReader::new();
            while let Ok(Some(msg)) = reader.next(&mut recv).await {
                let sent = control_tx.send(Internal::Message {
                    peer: peer.clone(),
                    msg,
                });
                if sent.is_err() {
                    break;
                }
            }
            let _ = control_tx.send(Internal::Closed {
                peer,
                conn: conn_id,
            });
        }
        .instrument(control_span),
    );
    let dgram_link = link.clone();
    let peer = peer_id.to_string();
    let span = conn_span(conn_id, &peer);
    tokio::spawn(
        async move {
            while let Ok(bytes) = dgram_link.read_datagram().await {
                // Heartbeats are answered here, on the reader task, so RTT and
                // the heartbeat gap are measured at arrival, not in the main
                // loop (N2.4). Motion still goes on to the engine.
                if let Ok(Datagram::Heartbeat { seq, echo, .. }) = decode_datagram(&bytes) {
                    match echo {
                        Some(request) => {
                            let mut beats = stats.lock().expect("beats");
                            let rtt = beats
                                .rtt
                                .echo(request, started.elapsed().as_micros() as u64);
                            if rtt.is_some() {
                                beats.beats_echoed += 1;
                            }
                        }
                        None => {
                            let now_ms = started.elapsed().as_millis() as u64;
                            {
                                let mut beats = stats.lock().expect("beats");
                                if beats.last_request_ms != 0 {
                                    let gap = now_ms.saturating_sub(beats.last_request_ms);
                                    if gap > beats.max_recv_gap_ms {
                                        beats.max_recv_gap_ms = gap;
                                    }
                                }
                                beats.last_request_ms = now_ms;
                            }
                            let reply = Datagram::Heartbeat {
                                seq: 0,
                                echo: Some(seq),
                                sent_us: 0,
                            };
                            if let Ok(reply) = encode_datagram(&reply) {
                                let _ = dgram_link.send_datagram(&reply);
                            }
                        }
                    }
                }
                let sent = tx.send(Internal::Datagram {
                    peer: peer.clone(),
                    bytes,
                });
                if sent.is_err() {
                    return;
                }
            }
            let _ = tx.send(Internal::Closed {
                peer,
                conn: dgram_link.id(),
            });
        }
        .instrument(span),
    );
}

fn spawn_stream_acceptor(
    link: Link,
    peer_id: &str,
    dest: PathBuf,
    staging: PathBuf,
    tx: UnboundedSender<Internal>,
) {
    let peer = peer_id.to_string();
    tokio::spawn(async move {
        loop {
            let accepted = link.accept_typed().await;
            let Ok((kind, _send, mut recv)) = accepted else {
                break;
            };
            match kind {
                STREAM_CLIPBOARD => {
                    let peer = peer.clone();
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let mut reader = hop_net::FrameReader::new();
                        while let Ok(Some(msg)) = reader.next(&mut recv).await {
                            let _ = tx.send(Internal::Message {
                                peer: peer.clone(),
                                msg,
                            });
                        }
                    });
                }
                STREAM_TRANSFER => {
                    let cancel = Arc::new(AtomicBool::new(false));
                    let tx_progress = tx.clone();
                    let dest = dest.clone();
                    let result = recv_all(&mut recv, &staging, &dest, &cancel, |progress| {
                        let _ = tx_progress.send(Internal::Transfer {
                            id: progress.id,
                            done: progress.bytes_done,
                            total: progress.bytes_total,
                            path: dest.display().to_string(),
                            state: "receiving".into(),
                        });
                    })
                    .await;
                    let state = if result.is_ok() { "done" } else { "error" };
                    let id = result.as_ref().map(|m| m.id).unwrap_or(0);
                    let _ = tx.send(Internal::Transfer {
                        id,
                        done: 0,
                        total: 0,
                        path: dest.display().to_string(),
                        state: state.into(),
                    });
                }
                STREAM_CONTROL => {
                    // Extra control streams are ignored (dialer opens the only one).
                }
                _ => {}
            }
        }
    });
}

fn chunk_clipboard(
    token: u64,
    kind: hop_proto::ClipKind,
    bytes: Vec<u8>,
    last: bool,
) -> Vec<StreamMsg> {
    if bytes.len() <= CLIPBOARD_CHUNK_BYTES {
        return vec![StreamMsg::ClipboardChunk {
            token,
            kind,
            seq: 0,
            last,
            bytes,
        }];
    }
    let parts: Vec<_> = bytes.chunks(CLIPBOARD_CHUNK_BYTES).collect();
    let n = parts.len();
    parts
        .into_iter()
        .enumerate()
        .map(|(i, part)| StreamMsg::ClipboardChunk {
            token,
            kind,
            seq: i as u32,
            last: last && i + 1 == n,
            bytes: part.to_vec(),
        })
        .collect()
}

async fn send_clipboard_frames(
    link: Link,
    announce: Option<StreamMsg>,
    frames: Vec<StreamMsg>,
) -> Result<(), NetError> {
    let (mut send, _recv) = link.open_clipboard().await?;
    if let Some(msg) = announce {
        write_msg(&mut send, &msg).await?;
    }
    for frame in frames {
        write_msg(&mut send, &frame).await?;
    }
    Ok(())
}

fn online_evt(session: &Session, code: Option<String>) -> NetEvt {
    NetEvt::Online {
        id: session.id.clone(),
        name: session.name.clone(),
        os: session.os,
        fingerprint: session.link.peer_fingerprint.clone(),
        addr: session.link.peer_addr.to_string(),
        code,
    }
}

fn local_os() -> OsKind {
    if cfg!(windows) {
        OsKind::Windows
    } else if cfg!(target_os = "macos") {
        OsKind::Macos
    } else {
        OsKind::Other
    }
}

/// Heartbeats and motion share a datagram. The engine uses this to tell them apart.
pub fn decode_packet(bytes: &[u8]) -> Option<Datagram> {
    decode_datagram(bytes).ok()
}

/// Adds a Windows Firewall rule. `netsh` needs admin rights, so for a normal
/// user this fails and Windows shows its own "allow access" prompt instead.
/// Returns a note for the user when the rule could not be added.
fn allow_inbound(port: u16) -> Option<String> {
    #[cfg(windows)]
    {
        let name = format!("name=DevHop {port}");
        let exists = std::process::Command::new("netsh")
            .args(["advfirewall", "firewall", "show", "rule", name.as_str()])
            .output()
            .is_ok_and(|out| out.status.success());
        if exists {
            return None;
        }
        let localport = format!("localport={port}");
        let added = std::process::Command::new("netsh")
            .args([
                "advfirewall",
                "firewall",
                "add",
                "rule",
                name.as_str(),
                "dir=in",
                "action=allow",
                "protocol=UDP",
                localport.as_str(),
            ])
            .output()
            .is_ok_and(|out| out.status.success());
        if added {
            None
        } else {
            Some(format!(
                "Windows Firewall may block UDP {port}. If the other computer cannot connect, \
                 allow DevHop on Private networks (Windows Security > Firewall > Allow an app)."
            ))
        }
    }
    #[cfg(not(windows))]
    {
        let _ = port;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::Receiver;

    /// Every netloop test spins up two tokio runtimes with full worker pools.
    /// Run the heavy ones one at a time so a busy machine cannot starve a
    /// heartbeat and turn scheduling delay into a spurious `Offline`/`Stalled`.
    static NET_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        NET_TEST_LOCK.lock().unwrap_or_else(|err| err.into_inner())
    }

    struct Node {
        cmds: UnboundedSender<NetCmd>,
        evts: Receiver<NetEvt>,
        fingerprint: String,
        dir: tempfile::TempDir,
    }

    fn node(id: &str, port: u16) -> Node {
        let dir = tempfile::tempdir().unwrap();
        let identity = Identity::generate(id).unwrap();
        let fingerprint = identity.fingerprint.clone();
        let (cmds, evts) = spawn(
            dir.path().to_path_buf(),
            dir.path().join("incoming"),
            identity,
            id.into(),
            id.into(),
            port,
            false,
            PeerStore::default(),
        );
        Node {
            cmds,
            evts,
            fingerprint,
            dir,
        }
    }

    /// Reads events until `want` matches or the deadline passes. Every event seen is returned.
    fn wait_for(node: &Node, secs: u64, want: impl Fn(&NetEvt) -> bool) -> (bool, Vec<NetEvt>) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        let mut seen = Vec::new();
        loop {
            if let Ok(evt) = node.evts.recv_timeout(Duration::from_millis(50)) {
                let hit = want(&evt);
                seen.push(evt);
                if hit {
                    return (true, seen);
                }
            } else if Instant::now() >= deadline {
                return (false, seen);
            }
        }
    }

    fn drain(node: &Node, secs: u64) -> Vec<NetEvt> {
        wait_for(node, secs, |_| false).1
    }

    #[test]
    fn two_computers_pair_and_stay_connected() {
        let _serial = serial();
        let a = node("aaa", 47901);
        let b = node("bbb", 47902);
        a.cmds
            .send(NetCmd::Dial("127.0.0.1:47902".parse().unwrap()))
            .unwrap();
        let (a_up, seen) = wait_for(
            &a,
            10,
            |e| matches!(e, NetEvt::Online { id, .. } if id == "bbb"),
        );
        assert!(a_up, "a never saw b online: {seen:?}");
        let (b_up, seen) = wait_for(
            &b,
            10,
            |e| matches!(e, NetEvt::Online { id, .. } if id == "aaa"),
        );
        assert!(b_up, "b never saw a online: {seen:?}");

        // Each person types the code shown on the other screen.
        a.cmds
            .send(NetCmd::Confirm {
                id: "bbb".into(),
                code: local_code(&b.fingerprint),
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(700));
        b.cmds
            .send(NetCmd::Confirm {
                id: "aaa".into(),
                code: local_code(&a.fingerprint),
            })
            .unwrap();

        let (a_paired, seen) = wait_for(&a, 5, |e| matches!(e, NetEvt::Paired(id) if id == "bbb"));
        assert!(a_paired, "a never paired: {seen:?}");
        let (b_paired, seen) = wait_for(&b, 5, |e| matches!(e, NetEvt::Paired(id) if id == "aaa"));
        assert!(b_paired, "b never paired: {seen:?}");
        assert!(PeerStore::load(a.dir.path())
            .by_fingerprint(&b.fingerprint)
            .is_some());
        assert!(PeerStore::load(b.dir.path())
            .by_fingerprint(&a.fingerprint)
            .is_some());

        // Heartbeats keep the session alive while nothing else happens.
        // Other tests run nodes on this machine too, and discovery finds them,
        // so only count drops of the peer this test paired with.
        let dropped = |evts: &[NetEvt], peer: &str| {
            evts.iter()
                .any(|e| matches!(e, NetEvt::Offline(id) | NetEvt::Stalled(id) if id == peer))
        };
        assert!(!dropped(&drain(&a, 2), "bbb"), "a dropped b");
        assert!(!dropped(&drain(&b, 0), "aaa"), "b dropped a");
    }

    #[test]
    fn heartbeats_survive_a_flood_of_motion() {
        let _serial = serial();
        let a = node("ccc", 47903);
        let b = node("ddd", 47904);
        a.cmds
            .send(NetCmd::Dial("127.0.0.1:47904".parse().unwrap()))
            .unwrap();
        assert!(wait_for(&a, 10, |e| matches!(e, NetEvt::Online { .. })).0);
        assert!(wait_for(&b, 10, |e| matches!(e, NetEvt::Online { .. })).0);
        a.cmds
            .send(NetCmd::Confirm {
                id: "ddd".into(),
                code: local_code(&b.fingerprint),
            })
            .unwrap();
        b.cmds
            .send(NetCmd::Confirm {
                id: "ccc".into(),
                code: local_code(&a.fingerprint),
            })
            .unwrap();
        assert!(wait_for(&a, 5, |e| matches!(e, NetEvt::Paired(_))).0);
        assert!(wait_for(&b, 5, |e| matches!(e, NetEvt::Paired(_))).0);

        // A 1 kHz mouse for two seconds must not starve heartbeats on either side.
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(2) {
            let _ = a.cmds.send(NetCmd::SendMotion {
                peer: "ddd".into(),
                dx: 1,
                dy: 0,
            });
            std::thread::sleep(Duration::from_millis(1));
        }
        let a_seen = drain(&a, 1);
        let b_seen = drain(&b, 0);
        let lost = |evts: &[NetEvt], peer: &str| {
            evts.iter()
                .any(|e| matches!(e, NetEvt::Offline(id) | NetEvt::Stalled(id) if id == peer))
        };
        assert!(!lost(&a_seen, "ddd"), "a lost b under load");
        assert!(!lost(&b_seen, "ccc"), "b lost a under load");
        let motion = b_seen
            .iter()
            .filter(|e| match e {
                NetEvt::Datagram { bytes, .. } => {
                    matches!(decode_packet(bytes), Some(Datagram::Motion { .. }))
                }
                _ => false,
            })
            .count();
        assert!(motion > 100, "only {motion} motion packets arrived");
    }

    /// Real use: both people click Pair at the same moment, and discovery dials
    /// too, so each side ends up with two or three connections to the same peer.
    /// Both sides must settle on one of them and keep it.
    ///
    /// This is the N0.3 loopback soak: both dial, a duplicate dial arrives by a
    /// different key, they pair, then 30 s of 1 kHz motion with a 4 MB clipboard
    /// chunk every 5 s. It asserts zero `Offline`/`Stalled`, more than 100 motion
    /// datagrams, and heartbeat RTT p95 under 5 ms (N1.1, N0.3).
    #[test]
    fn both_computers_dialing_at_once_settle_on_one_connection() {
        let _serial = serial();
        let a = node("eee", 47905);
        let b = node("fff", 47906);
        a.cmds
            .send(NetCmd::Dial("127.0.0.1:47906".parse().unwrap()))
            .unwrap();
        b.cmds
            .send(NetCmd::Dial("127.0.0.1:47905".parse().unwrap()))
            .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        // discover() dials by device id, a different key from the address.
        a.cmds
            .send(NetCmd::Dial("127.0.0.1:47906".parse().unwrap()))
            .unwrap();
        let (a_up, seen) = wait_for(
            &a,
            10,
            |e| matches!(e, NetEvt::Online { id, .. } if id == "fff"),
        );
        assert!(a_up, "a never saw b online: {seen:?}");
        let (b_up, seen) = wait_for(
            &b,
            10,
            |e| matches!(e, NetEvt::Online { id, .. } if id == "eee"),
        );
        assert!(b_up, "b never saw a online: {seen:?}");
        a.cmds
            .send(NetCmd::Confirm {
                id: "fff".into(),
                code: local_code(&b.fingerprint),
            })
            .unwrap();
        b.cmds
            .send(NetCmd::Confirm {
                id: "eee".into(),
                code: local_code(&a.fingerprint),
            })
            .unwrap();
        let (a_paired, seen) = wait_for(&a, 8, |e| matches!(e, NetEvt::Paired(id) if id == "fff"));
        assert!(a_paired, "a never paired: {seen:?}");
        let (b_paired, seen) = wait_for(&b, 8, |e| matches!(e, NetEvt::Paired(id) if id == "eee"));
        assert!(b_paired, "b never paired: {seen:?}");

        let dropped = |evts: &[NetEvt], peer: &str| {
            evts.iter()
                .any(|e| matches!(e, NetEvt::Offline(id) | NetEvt::Stalled(id) if id == peer))
        };
        let summary = |evts: &[NetEvt]| -> Vec<String> {
            evts.iter()
                .filter_map(|e| match e {
                    NetEvt::Online { id, .. } => Some(format!("Online({id})")),
                    NetEvt::Offline(id) => Some(format!("Offline({id})")),
                    NetEvt::Stalled(id) => Some(format!("Stalled({id})")),
                    NetEvt::Paired(id) => Some(format!("Paired({id})")),
                    NetEvt::Log(l) => Some(format!("log: {l}")),
                    _ => None,
                })
                .collect()
        };

        let big = vec![0xABu8; 4 * 1024 * 1024];
        let soak_start = Instant::now();
        let mut last_clip = Instant::now();
        while soak_start.elapsed() < Duration::from_secs(30) {
            let _ = a.cmds.send(NetCmd::SendMotion {
                peer: "fff".into(),
                dx: 1,
                dy: 0,
            });
            if last_clip.elapsed() >= Duration::from_secs(5) {
                let _ = a.cmds.send(NetCmd::SendControl {
                    peer: "fff".into(),
                    msg: StreamMsg::ClipboardAnnounce {
                        token: 42,
                        items: vec![hop_proto::ClipAnnounce {
                            kind: hop_proto::ClipKind::Png,
                            bytes: big.len() as u64,
                        }],
                    },
                });
                let _ = a.cmds.send(NetCmd::SendControl {
                    peer: "fff".into(),
                    msg: StreamMsg::ClipboardChunk {
                        token: 42,
                        kind: hop_proto::ClipKind::Png,
                        seq: 0,
                        last: true,
                        bytes: big.clone(),
                    },
                });
                last_clip = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(1));
        }

        let a_seen = drain(&a, 1);
        let b_seen = drain(&b, 1);
        assert!(
            !dropped(&a_seen, "fff"),
            "a dropped b during soak: {:?}",
            summary(&a_seen)
        );
        assert!(
            !dropped(&b_seen, "eee"),
            "b dropped a during soak: {:?}",
            summary(&b_seen)
        );

        // N0.3: the heartbeat RTT the net thread measures must stay under 5 ms
        // p95 across the whole soak, on both sides.
        let worst_p95 = a_seen
            .iter()
            .chain(b_seen.iter())
            .filter_map(|e| match e {
                NetEvt::Stats {
                    rtt_p95_us: Some(us),
                    ..
                } => Some(*us),
                _ => None,
            })
            .max();
        assert!(
            worst_p95.is_some_and(|us| us < 5_000),
            "heartbeat RTT p95 {worst_p95:?} us is not under 5 ms: {:?}",
            summary(&b_seen)
        );

        let motion = b_seen
            .iter()
            .filter(|e| match e {
                NetEvt::Datagram { bytes, .. } => {
                    matches!(decode_packet(bytes), Some(Datagram::Motion { .. }))
                }
                _ => false,
            })
            .count();
        assert!(
            motion > 100,
            "only {motion} motion datagrams during soak: {:?}",
            summary(&b_seen)
        );
    }

    /// N1.1: duplicate dials must not flap the session across many runs.
    #[test]
    fn both_computers_dialing_stress_twenty_runs() {
        let _serial = serial();
        for run in 0..20 {
            let a_port = 48000 + (run as u16) * 2;
            let b_port = a_port + 1;
            let a = node("stress-a", a_port);
            let b = node("stress-b", b_port);
            a.cmds
                .send(NetCmd::Dial(format!("127.0.0.1:{b_port}").parse().unwrap()))
                .unwrap();
            b.cmds
                .send(NetCmd::Dial(format!("127.0.0.1:{a_port}").parse().unwrap()))
                .unwrap();
            std::thread::sleep(Duration::from_millis(100));
            a.cmds
                .send(NetCmd::Dial(format!("127.0.0.1:{b_port}").parse().unwrap()))
                .unwrap();
            assert!(
                wait_for(
                    &a,
                    12,
                    |e| matches!(e, NetEvt::Online { id, .. } if id == "stress-b")
                )
                .0,
                "run {run}: a never saw b"
            );
            assert!(
                wait_for(
                    &b,
                    12,
                    |e| matches!(e, NetEvt::Online { id, .. } if id == "stress-a")
                )
                .0,
                "run {run}: b never saw a"
            );
            a.cmds
                .send(NetCmd::Confirm {
                    id: "stress-b".into(),
                    code: local_code(&b.fingerprint),
                })
                .unwrap();
            b.cmds
                .send(NetCmd::Confirm {
                    id: "stress-a".into(),
                    code: local_code(&a.fingerprint),
                })
                .unwrap();
            assert!(
                wait_for(
                    &a,
                    8,
                    |e| matches!(e, NetEvt::Paired(id) if id == "stress-b")
                )
                .0,
                "run {run}: a never paired"
            );

            let soak = Instant::now();
            while soak.elapsed() < Duration::from_secs(5) {
                let _ = a.cmds.send(NetCmd::SendMotion {
                    peer: "stress-b".into(),
                    dx: 1,
                    dy: 0,
                });
                std::thread::sleep(Duration::from_millis(1));
            }
            let dropped = |evts: &[NetEvt], peer: &str| {
                evts.iter()
                    .any(|e| matches!(e, NetEvt::Offline(id) | NetEvt::Stalled(id) if id == peer))
            };
            let a_seen = drain(&a, 0);
            let b_seen = drain(&b, 0);
            assert!(
                !dropped(&a_seen, "stress-b"),
                "run {run}: a dropped b: {:?}",
                event_kinds(&a_seen)
            );
            assert!(
                !dropped(&b_seen, "stress-a"),
                "run {run}: b dropped a: {:?}",
                event_kinds(&b_seen)
            );
        }
    }

    /// A compact view of the session events in a drain, for test failures.
    fn event_kinds(evts: &[NetEvt]) -> Vec<String> {
        evts.iter()
            .filter_map(|e| match e {
                NetEvt::Online { id, .. } => Some(format!("Online({id})")),
                NetEvt::Offline(id) => Some(format!("Offline({id})")),
                NetEvt::Stalled(id) => Some(format!("Stalled({id})")),
                NetEvt::Paired(id) => Some(format!("Paired({id})")),
                _ => None,
            })
            .collect()
    }

    /// Dials `a` at `b`, waits for both `Online`, then confirms both codes so
    /// the pairing gate opens (R7).
    fn pair_nodes(a: &Node, a_id: &str, b: &Node, b_id: &str, b_port: u16) {
        a.cmds
            .send(NetCmd::Dial(format!("127.0.0.1:{b_port}").parse().unwrap()))
            .unwrap();
        assert!(
            wait_for(
                a,
                10,
                |e| matches!(e, NetEvt::Online { id, .. } if id == b_id)
            )
            .0,
            "{a_id} never saw {b_id} online"
        );
        assert!(
            wait_for(
                b,
                10,
                |e| matches!(e, NetEvt::Online { id, .. } if id == a_id)
            )
            .0,
            "{b_id} never saw {a_id} online"
        );
        a.cmds
            .send(NetCmd::Confirm {
                id: b_id.into(),
                code: local_code(&b.fingerprint),
            })
            .unwrap();
        b.cmds
            .send(NetCmd::Confirm {
                id: a_id.into(),
                code: local_code(&a.fingerprint),
            })
            .unwrap();
        assert!(
            wait_for(a, 8, |e| matches!(e, NetEvt::Paired(id) if id == b_id)).0,
            "{a_id} never paired"
        );
        assert!(
            wait_for(b, 8, |e| matches!(e, NetEvt::Paired(id) if id == a_id)).0,
            "{b_id} never paired"
        );
    }

    /// Like [`wait_for`], but appends every event seen (including the hit) to
    /// `seen`, so a test can assert on a status sequence after the fact.
    fn wait_collect(
        node: &Node,
        secs: u64,
        seen: &mut Vec<NetEvt>,
        want: impl Fn(&NetEvt) -> bool,
    ) -> bool {
        let (ok, mut events) = wait_for(node, secs, want);
        seen.append(&mut events);
        ok
    }

    fn phase_statuses(events: &[NetEvt]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                NetEvt::Phase { status, .. } => Some(status.clone()),
                _ => None,
            })
            .collect()
    }

    /// N4.2: the per-peer status the UI shows is derived from the session state
    /// machine. Connect, pair, drop the link and reconnect, then assert the
    /// status values arrived in order on both sides. (`discovered` and
    /// `connecting` are covered by `every_phase_maps_to_a_ui_status` in
    /// `hop-net`; a test node has discovery off, so the first dial is by
    /// address with the peer id not yet known.)
    #[test]
    fn peer_status_reports_a_full_pair_drop_reconnect_cycle() {
        let _serial = serial();
        let a = node("stat-a", 47970);
        let b = node("stat-b", 47971);
        let mut a_seen = Vec::new();
        let mut b_seen = Vec::new();

        // Connect: A dials B.
        a.cmds
            .send(NetCmd::Dial("127.0.0.1:47971".parse().unwrap()))
            .unwrap();
        assert!(wait_collect(
            &a,
            10,
            &mut a_seen,
            |e| matches!(e, NetEvt::Online { id, .. } if id == "stat-b")
        ));
        assert!(wait_collect(
            &b,
            10,
            &mut b_seen,
            |e| matches!(e, NetEvt::Online { id, .. } if id == "stat-a")
        ));

        // Pair both ways.
        a.cmds
            .send(NetCmd::Confirm {
                id: "stat-b".into(),
                code: local_code(&b.fingerprint),
            })
            .unwrap();
        b.cmds
            .send(NetCmd::Confirm {
                id: "stat-a".into(),
                code: local_code(&a.fingerprint),
            })
            .unwrap();
        assert!(wait_collect(&a, 8, &mut a_seen, |e| matches!(
            e,
            NetEvt::Paired(id) if id == "stat-b"
        )));
        assert!(wait_collect(&b, 8, &mut b_seen, |e| matches!(
            e,
            NetEvt::Paired(id) if id == "stat-a"
        )));

        // Drop the link: B forgets A, which closes the connection.
        b.cmds.send(NetCmd::Forget("stat-a".into())).unwrap();
        assert!(wait_collect(&a, 5, &mut a_seen, |e| matches!(
            e,
            NetEvt::Offline(id) if id == "stat-b"
        )));
        assert!(wait_collect(&b, 5, &mut b_seen, |e| matches!(
            e,
            NetEvt::Offline(id) if id == "stat-a"
        )));

        // Reconnect: B dials A again. A still has B pinned, so A comes back
        // straight to `paired`; B forgot the peer, so B comes back `unpaired`.
        b.cmds
            .send(NetCmd::Dial("127.0.0.1:47970".parse().unwrap()))
            .unwrap();
        assert!(wait_collect(
            &a,
            10,
            &mut a_seen,
            |e| matches!(e, NetEvt::Online { id, .. } if id == "stat-b")
        ));
        assert!(wait_collect(
            &b,
            10,
            &mut b_seen,
            |e| matches!(e, NetEvt::Online { id, .. } if id == "stat-a")
        ));

        assert_eq!(
            phase_statuses(&a_seen),
            vec!["unpaired", "paired", "offline", "paired"],
            "a status order: {a_seen:?}"
        );
        assert_eq!(
            phase_statuses(&b_seen),
            vec!["unpaired", "paired", "offline", "unpaired"],
            "b status order: {b_seen:?}"
        );
    }

    /// N2.2: a clipboard stream and a transfer stream opened at the same moment
    /// must each arrive in the right handler, never parsed as the other.
    #[test]
    fn clipboard_and_transfer_streams_reach_the_right_handlers() {
        let _serial = serial();
        let a = node("cb-a", 47940);
        let b = node("cb-b", 47941);
        pair_nodes(&a, "cb-a", &b, "cb-b", 47941);

        let payload = vec![0x5Au8; 128 * 1024];
        let file = a.dir.path().join("payload.bin");
        std::fs::write(&file, &payload).unwrap();

        // Open both streams back to back, with no wait in between.
        a.cmds
            .send(NetCmd::SendControl {
                peer: "cb-b".into(),
                msg: StreamMsg::ClipboardAnnounce {
                    token: 7,
                    items: vec![hop_proto::ClipAnnounce {
                        kind: hop_proto::ClipKind::Text,
                        bytes: 5,
                    }],
                },
            })
            .unwrap();
        a.cmds
            .send(NetCmd::SendControl {
                peer: "cb-b".into(),
                msg: StreamMsg::ClipboardChunk {
                    token: 7,
                    kind: hop_proto::ClipKind::Text,
                    seq: 0,
                    last: true,
                    bytes: b"hello".to_vec(),
                },
            })
            .unwrap();
        a.cmds
            .send(NetCmd::SendFiles {
                peer: "cb-b".into(),
                id: 11,
                paths: vec![file],
            })
            .unwrap();

        let (clip_ok, seen) = wait_for(&b, 10, |e| {
            matches!(
                e,
                NetEvt::Message { peer, msg: StreamMsg::ClipboardChunk { token: 7, .. } }
                    if peer == "cb-a"
            )
        });
        assert!(
            clip_ok,
            "clipboard chunk never reached the message handler: {seen:?}"
        );

        let (done, seen) = wait_for(
            &b,
            20,
            |e| matches!(e, NetEvt::Transfer { id: 11, state, .. } if state == "done"),
        );
        assert!(done, "transfer never completed: {seen:?}");
    }

    /// N2.4: a 1 kHz motion flood plus a large transfer must not stretch the
    /// gap between heartbeats past 150 ms. PLAN.md names 2 GB; we use 256 MiB
    /// here so the test stays fast (documented reduction).
    #[test]
    fn heartbeat_gap_stays_under_150_ms_under_load() {
        let _serial = serial();
        let a = node("gap-a", 47942);
        let b = node("gap-b", 47943);
        pair_nodes(&a, "gap-a", &b, "gap-b", 47943);

        const TRANSFER_BYTES: u64 = 256 * 1024 * 1024;
        let file = a.dir.path().join("big.bin");
        let f = std::fs::File::create(&file).unwrap();
        f.set_len(TRANSFER_BYTES).unwrap();
        drop(f);
        a.cmds
            .send(NetCmd::SendFiles {
                peer: "gap-b".into(),
                id: 1,
                paths: vec![file],
            })
            .unwrap();

        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(6) {
            let _ = a.cmds.send(NetCmd::SendMotion {
                peer: "gap-b".into(),
                dx: 1,
                dy: 0,
            });
            std::thread::sleep(Duration::from_millis(1));
        }

        let a_seen = drain(&a, 1);
        let b_seen = drain(&b, 1);
        // N2.4 is about the heartbeat task not being starved by the transfer, so
        // assert on the gap between beats the *sender* emitted. The received gap
        // would also include ordinary datagram loss.
        let max_send_gap = a_seen
            .iter()
            .filter_map(|e| match e {
                NetEvt::Stats {
                    max_beat_gap_ms, ..
                } => Some(*max_beat_gap_ms),
                _ => None,
            })
            .max();
        assert!(
            max_send_gap.is_some(),
            "no heartbeat stats reached the test"
        );
        let max_send_gap = max_send_gap.unwrap();
        assert!(max_send_gap > 0, "heartbeat stats never recorded a gap");
        assert!(
            max_send_gap < 150,
            "heartbeat send gap {max_send_gap} ms exceeded 150 ms under load"
        );
        assert!(
            !b_seen
                .iter()
                .any(|e| matches!(e, NetEvt::Offline(id) | NetEvt::Stalled(id) if id == "gap-a")),
            "the session dropped under the motion + transfer load"
        );
    }
}
