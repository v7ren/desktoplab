//! QUIC sessions, discovery, and the pairing gate.
//!
//! The engine thread sends [`NetCmd`] and reads [`NetEvt`]. Mouse motion is a
//! datagram. Each file transfer opens its own low-priority stream.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hop_net::{
    read_msg, should_dial, write_msg, Discovery, Endpoint, HeartbeatWatch, Identity, Link,
    PairGate, PeerAdvert, SendStream,
};
use hop_proto::{decode_datagram, encode_datagram, Datagram, OsKind, StreamMsg, PROTO_VERSION};
use hop_transfer::{recv_all, send_all};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::peers::{PeerStore, StoredPeer};

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
    Confirm(String),
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
    Offline(String),
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
    Log(String),
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
}

struct Session {
    id: String,
    name: String,
    os: OsKind,
    link: Link,
    send: SendStream,
    gate: PairGate,
    beats: HeartbeatWatch,
}

pub fn spawn(
    dir: PathBuf,
    incoming: PathBuf,
    identity: Identity,
    device_id: String,
    device_name: String,
    port: u16,
    peers: PeerStore,
) -> (UnboundedSender<NetCmd>, std::sync::mpsc::Receiver<NetEvt>) {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
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
    mut peers: PeerStore,
    mut cmds: UnboundedReceiver<NetCmd>,
    evts: std::sync::mpsc::Sender<NetEvt>,
) {
    let endpoint = match Endpoint::bind(&identity, port) {
        Ok(endpoint) => endpoint,
        Err(err) => {
            let _ = evts.send(NetEvt::Log(format!("bind: {err}")));
            return;
        }
    };
    let discovery = Discovery::start(&device_name, &device_id, &identity.fingerprint, port).ok();
    let mut sessions: HashMap<String, Session> = HashMap::new();
    let mut known: HashMap<String, PeerAdvert> = HashMap::new();
    let mut cancels: HashMap<u64, Arc<AtomicBool>> = HashMap::new();
    let (internal_tx, mut internal_rx) = tokio::sync::mpsc::unbounded_channel();
    let started = Instant::now();
    loop {
        let now_ms = started.elapsed().as_millis() as u64;
        tokio::select! {
            incoming_conn = endpoint.accept() => {
                if let Ok(link) = incoming_conn {
                    accept_link(
                        &evts, &internal_tx, &mut sessions, &peers, &dir, &incoming,
                        link, &device_id, &device_name, &identity.fingerprint, now_ms,
                    ).await;
                }
            }
            cmd = cmds.recv() => {
                let Some(cmd) = cmd else { break };
                if !handle_cmd(
                    &endpoint, &evts, &internal_tx, &mut sessions, &mut peers, &dir, &incoming,
                    &mut known, &mut cancels, &device_id, &device_name, &identity.fingerprint,
                    now_ms, cmd,
                ).await {
                    break;
                }
            }
            internal = internal_rx.recv() => {
                if let Some(internal) = internal {
                    on_internal(&evts, &mut sessions, &mut peers, &dir, &mut known, now_ms, internal);
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                discover(
                    &endpoint, discovery.as_ref(), &evts, &internal_tx, &mut sessions, &peers,
                    &dir, &incoming, &mut known, &device_id, &device_name, &identity.fingerprint, now_ms,
                ).await;
                poll_sessions(&evts, &mut sessions, now_ms).await;
            }
        }
    }
}

fn on_internal(
    evts: &std::sync::mpsc::Sender<NetEvt>,
    sessions: &mut HashMap<String, Session>,
    peers: &mut PeerStore,
    dir: &Path,
    known: &mut HashMap<String, PeerAdvert>,
    now_ms: u64,
    internal: Internal,
) {
    match internal {
        Internal::Datagram { peer, bytes } => {
            if let Some(session) = sessions.get_mut(&peer) {
                session.beats.beat(now_ms);
            }
            let _ = evts.send(NetEvt::Datagram { peer, bytes });
        }
        Internal::Message { peer, msg } => {
            if let Some(session) = sessions.get_mut(&peer) {
                session.beats.beat(now_ms);
                match &msg {
                    StreamMsg::PairConfirm => {
                        session.gate.confirm_remote();
                        if session.gate.ready_to_pin() {
                            pin(peers, dir, session, known);
                        }
                    }
                    StreamMsg::PairReject => session.gate.reject(),
                    _ => {}
                }
            }
            let _ = evts.send(NetEvt::Message { peer, msg });
        }
        Internal::Transfer {
            id,
            done,
            total,
            path,
            state,
        } => {
            let _ = evts.send(NetEvt::Transfer {
                id,
                done,
                total,
                path,
                state,
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_cmd(
    endpoint: &Endpoint,
    evts: &std::sync::mpsc::Sender<NetEvt>,
    internal_tx: &UnboundedSender<Internal>,
    sessions: &mut HashMap<String, Session>,
    peers: &mut PeerStore,
    dir: &Path,
    incoming: &Path,
    known: &mut HashMap<String, PeerAdvert>,
    cancels: &mut HashMap<u64, Arc<AtomicBool>>,
    device_id: &str,
    device_name: &str,
    fingerprint: &str,
    now_ms: u64,
    cmd: NetCmd,
) -> bool {
    match cmd {
        NetCmd::Shutdown => return false,
        NetCmd::Dial(addr) => {
            if let Ok(link) = endpoint.dial(addr).await {
                accept_link(
                    evts,
                    internal_tx,
                    sessions,
                    peers,
                    dir,
                    incoming,
                    link,
                    device_id,
                    device_name,
                    fingerprint,
                    now_ms,
                )
                .await;
            }
        }
        NetCmd::SendMotion { peer, dx, dy } => {
            if let Some(session) = sessions.get(&peer) {
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
            if let Some(session) = sessions.get_mut(&peer) {
                let allowed = session.gate.accepts_input()
                    || matches!(msg, StreamMsg::PairConfirm | StreamMsg::PairReject);
                if allowed {
                    let _ = write_msg(&mut session.send, &msg).await;
                } else {
                    let _ = write_msg(&mut session.send, &StreamMsg::InputRejected).await;
                }
            }
        }
        NetCmd::SendFiles { peer, id, paths } => {
            if let Some(session) = sessions.get(&peer) {
                if session.gate.accepts_input() {
                    let cancel = Arc::new(AtomicBool::new(false));
                    cancels.insert(id, cancel.clone());
                    let link = session.link.clone();
                    let tx = internal_tx.clone();
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
            }
        }
        NetCmd::CancelTransfer(id) => {
            if let Some(flag) = cancels.get(&id) {
                flag.store(true, Ordering::Relaxed);
            }
        }
        NetCmd::Confirm(id) => {
            if let Some(session) = sessions.get_mut(&id) {
                session.gate.confirm_local();
                let _ = write_msg(&mut session.send, &StreamMsg::PairConfirm).await;
                if session.gate.ready_to_pin() {
                    pin(peers, dir, session, known);
                    let _ = evts.send(online_evt(session, None));
                }
            }
        }
        NetCmd::Reject(id) => {
            if let Some(session) = sessions.get_mut(&id) {
                session.gate.reject();
                let _ = write_msg(&mut session.send, &StreamMsg::PairReject).await;
            }
        }
        NetCmd::Forget(id) => {
            peers.forget(&id);
            let _ = peers.save(dir);
            if let Some(session) = sessions.remove(&id) {
                session.link.close();
            }
            let _ = evts.send(NetEvt::Offline(id));
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
async fn discover(
    endpoint: &Endpoint,
    discovery: Option<&Discovery>,
    evts: &std::sync::mpsc::Sender<NetEvt>,
    internal_tx: &UnboundedSender<Internal>,
    sessions: &mut HashMap<String, Session>,
    peers: &PeerStore,
    dir: &Path,
    incoming: &Path,
    known: &mut HashMap<String, PeerAdvert>,
    device_id: &str,
    device_name: &str,
    fingerprint: &str,
    now_ms: u64,
) {
    let Some(discovery) = discovery else { return };
    for advert in discovery.peers() {
        let _ = evts.send(NetEvt::Advert(advert.clone()));
        known.insert(advert.device_id.clone(), advert.clone());
        if peers.by_fingerprint(&advert.fingerprint).is_some()
            && should_dial(device_id, &advert.device_id)
            && !sessions.contains_key(&advert.device_id)
        {
            if let Ok(link) = endpoint.dial(advert.addr).await {
                accept_link(
                    evts,
                    internal_tx,
                    sessions,
                    peers,
                    dir,
                    incoming,
                    link,
                    device_id,
                    device_name,
                    fingerprint,
                    now_ms,
                )
                .await;
            }
        }
    }
}

async fn poll_sessions(
    evts: &std::sync::mpsc::Sender<NetEvt>,
    sessions: &mut HashMap<String, Session>,
    now_ms: u64,
) {
    let mut dead = Vec::new();
    for (id, session) in sessions.iter_mut() {
        if session.beats.poll(now_ms) {
            dead.push(id.clone());
            continue;
        }
        let beat = Datagram::Heartbeat {
            seq: now_ms,
            echo: None,
            sent_us: 0,
        };
        if let Ok(bytes) = encode_datagram(&beat) {
            let _ = session.link.send_datagram(&bytes);
        }
    }
    for id in dead {
        if let Some(session) = sessions.remove(&id) {
            session.link.close();
        }
        let _ = evts.send(NetEvt::Offline(id));
    }
}

#[allow(clippy::too_many_arguments)]
async fn accept_link(
    evts: &std::sync::mpsc::Sender<NetEvt>,
    internal_tx: &UnboundedSender<Internal>,
    sessions: &mut HashMap<String, Session>,
    peers: &PeerStore,
    dir: &Path,
    incoming: &Path,
    link: Link,
    device_id: &str,
    device_name: &str,
    fingerprint: &str,
    now_ms: u64,
) {
    let handshake = tokio::time::timeout(Duration::from_secs(8), async {
        tokio::try_join!(link.open_control(), link.accept_bi())
    })
    .await;
    let Ok(Ok(((mut send, _local_recv), (_remote_send, mut recv)))) = handshake else {
        let _ = evts.send(NetEvt::Log("handshake timed out".into()));
        return;
    };
    let hello = StreamMsg::Hello {
        version: PROTO_VERSION,
        device_id: device_id.into(),
        name: device_name.into(),
        os: local_os(),
    };
    let (wrote, read) = tokio::join!(write_msg(&mut send, &hello), read_msg(&mut recv));
    if wrote.is_err() {
        return;
    }
    let Some(
        StreamMsg::Hello {
            device_id: peer_id,
            name,
            os,
            version,
        }
        | StreamMsg::HelloAck {
            device_id: peer_id,
            name,
            os,
            version,
        },
    ) = read.ok().flatten()
    else {
        return;
    };
    if version != PROTO_VERSION {
        let _ = evts.send(NetEvt::Log(format!(
            "peer {peer_id} speaks version {version}"
        )));
        return;
    }
    let pinned = peers.by_fingerprint(&link.peer_fingerprint).is_some();
    let gate = PairGate::new(pinned, fingerprint, &link.peer_fingerprint);
    let code = if pinned {
        None
    } else {
        Some(gate.code().to_string())
    };
    let peer_id2 = peer_id.clone();
    let tx = internal_tx.clone();
    tokio::spawn(async move {
        let mut reader = hop_net::FrameReader::new();
        while let Ok(Some(msg)) = reader.next(&mut recv).await {
            if tx
                .send(Internal::Message {
                    peer: peer_id2.clone(),
                    msg,
                })
                .is_err()
            {
                break;
            }
        }
    });
    let dgram_link = link.clone();
    let dgram_peer = peer_id.clone();
    let dgram_tx = internal_tx.clone();
    tokio::spawn(async move {
        while let Ok(bytes) = dgram_link.read_datagram().await {
            if dgram_tx
                .send(Internal::Datagram {
                    peer: dgram_peer.clone(),
                    bytes,
                })
                .is_err()
            {
                break;
            }
        }
    });
    spawn_incoming(
        link.clone(),
        incoming.to_path_buf(),
        dir.join("staging"),
        internal_tx.clone(),
    );
    let session = Session {
        id: peer_id.clone(),
        name: name.clone(),
        os,
        link,
        send,
        gate,
        beats: HeartbeatWatch::new(now_ms),
    };
    let _ = evts.send(online_evt(&session, code));
    sessions.insert(peer_id, session);
}

fn spawn_incoming(link: Link, dest: PathBuf, staging: PathBuf, tx: UnboundedSender<Internal>) {
    tokio::spawn(async move {
        loop {
            let Ok((_send, mut recv)) = link.accept_bi().await else {
                break;
            };
            let cancel = Arc::new(AtomicBool::new(false));
            let tx_progress = tx.clone();
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
    });
}

fn pin(peers: &mut PeerStore, dir: &Path, session: &Session, known: &HashMap<String, PeerAdvert>) {
    let name = known
        .get(&session.id)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| session.name.clone());
    peers.pin(StoredPeer {
        id: session.id.clone(),
        name,
        fingerprint: session.link.peer_fingerprint.clone(),
        last_addr: session.link.peer_addr.to_string(),
    });
    let _ = peers.save(dir);
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
