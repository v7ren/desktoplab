//! mDNS `_devhop._udp` plus a manual address for networks that block it.

use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::local_code;

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use socket2::{Domain, Protocol, Socket, Type};

use crate::NetError;

pub fn service_type() -> &'static str {
    "_devhop._udp.local."
}

/// UDP port used to find other computers on this LAN. The QUIC port stays separate.
pub const BEACON_PORT: u16 = 42425;

/// A computer found by beacon disappears from the list after this long
/// without a beacon (they arrive every second).
const BEACON_EXPIRY: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Beacon,
    Mdns,
}

#[derive(Clone, Debug)]
struct Seen {
    advert: PeerAdvert,
    at: Instant,
    source: Source,
    /// Set for mDNS entries so [`ServiceEvent::ServiceRemoved`] matches exactly.
    mdns_fullname: Option<String>,
    /// When set (from the session layer), beacon packets must not change [`PeerAdvert::addr`].
    addr_locked: bool,
}

type PeerMap = Arc<Mutex<HashMap<String, Seen>>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerAdvert {
    pub name: String,
    pub device_id: String,
    pub fingerprint: String,
    pub addr: SocketAddr,
}

pub struct Discovery {
    daemon: Option<ServiceDaemon>,
    peers: PeerMap,
    beacon_stop: Arc<AtomicBool>,
    beacon_thread: Option<JoinHandle<()>>,
    note: Option<String>,
}

impl Discovery {
    pub fn start(
        name: &str,
        device_id: &str,
        fingerprint: &str,
        port: u16,
    ) -> Result<Self, NetError> {
        let peers = Arc::new(Mutex::new(HashMap::new()));
        let beacon_stop = Arc::new(AtomicBool::new(false));
        let (beacon_thread, note) = match spawn_beacon(
            name,
            device_id,
            fingerprint,
            port,
            Arc::clone(&peers),
            Arc::clone(&beacon_stop),
        ) {
            Ok(handle) => (Some(handle), None),
            Err(err) => (
                None,
                Some(format!("cannot listen for other computers: {err}")),
            ),
        };
        let daemon = start_mdns(name, device_id, fingerprint, port, Arc::clone(&peers));
        if daemon.is_none() && beacon_thread.is_none() {
            return Err(NetError::Mdns(
                note.unwrap_or_else(|| "discovery failed".into()),
            ));
        }
        Ok(Self {
            daemon,
            peers,
            beacon_stop,
            beacon_thread,
            note,
        })
    }

    pub fn note(&self) -> Option<String> {
        self.note.clone()
    }

    /// Computers seen recently. Beacon entries expire on their own; mDNS
    /// entries stay until the computer says goodbye or [`Discovery::forget`]
    /// drops them after repeated failed dials.
    pub fn peers(&self) -> Vec<PeerAdvert> {
        let Ok(mut map) = self.peers.lock() else {
            return Vec::new();
        };
        map.retain(|_, seen| seen.source == Source::Mdns || seen.at.elapsed() < BEACON_EXPIRY);
        map.values().map(|seen| seen.advert.clone()).collect()
    }

    pub fn forget(&self, device_id: &str) {
        if let Ok(mut map) = self.peers.lock() {
            map.remove(device_id);
        }
    }

    /// Stop beacon adverts from changing this peer's address (N5.4). Call when a
    /// session is connected so a forged LAN beacon cannot redirect QUIC.
    pub fn lock_peer_address(&self, device_id: &str) {
        if let Ok(mut map) = self.peers.lock() {
            if let Some(seen) = map.get_mut(device_id) {
                seen.addr_locked = true;
            }
        }
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.beacon_stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.beacon_thread.take() {
            let _ = thread.join();
        }
        if let Some(daemon) = &self.daemon {
            let _ = daemon.shutdown();
        }
    }
}

pub fn manual_peer(addr: SocketAddr) -> PeerAdvert {
    PeerAdvert {
        name: addr.ip().to_string(),
        device_id: String::new(),
        fingerprint: String::new(),
        addr,
    }
}

fn advert_from(info: &ServiceInfo) -> Option<PeerAdvert> {
    let device_id = prop(info, "id");
    if device_id.is_empty() {
        return None;
    }
    let ip = info.get_addresses().iter().copied().next()?;
    Some(PeerAdvert {
        name: prop(info, "nm"),
        fingerprint: prop(info, "fp"),
        device_id,
        addr: SocketAddr::new(ip, info.get_port()),
    })
}

fn prop(info: &ServiceInfo, key: &str) -> String {
    info.get_property_val_str(key).unwrap_or("").to_string()
}

fn sanitize(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "devhop".into()
    } else {
        trimmed.to_string()
    }
}

pub fn advertised_ip() -> IpAddr {
    lan_ip().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

fn lan_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    Some(socket.local_addr().ok()?.ip())
}

fn start_mdns(
    name: &str,
    device_id: &str,
    fingerprint: &str,
    port: u16,
    peers: PeerMap,
) -> Option<ServiceDaemon> {
    let daemon = ServiceDaemon::new().ok()?;
    let ip = advertised_ip();
    let instance = sanitize(name);
    let host = format!("{}.local.", sanitize(device_id));
    let info = ServiceInfo::new(
        service_type(),
        &instance,
        &host,
        ip,
        port,
        &[("id", device_id), ("fp", fingerprint), ("nm", name)][..],
    )
    .ok()?;
    daemon.register(info).ok()?;
    let receiver = daemon.browse(service_type()).ok()?;
    let mine = device_id.to_string();
    std::thread::Builder::new()
        .name("devhop-mdns".into())
        .spawn(move || {
            while let Ok(event) = receiver.recv() {
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        if let Some(peer) = advert_from(&info) {
                            if peer.device_id != mine && !peer.device_id.is_empty() {
                                let fullname = info.get_fullname().to_string();
                                remember_mdns(&peers, peer, fullname);
                            }
                        }
                    }
                    ServiceEvent::ServiceRemoved(_, fullname) => {
                        if let Ok(mut map) = peers.lock() {
                            retain_after_mdns_removal(&mut map, &fullname);
                        }
                    }
                    _ => {}
                }
            }
        })
        .ok()?;
    Some(daemon)
}

fn spawn_beacon(
    name: &str,
    device_id: &str,
    fingerprint: &str,
    quic_port: u16,
    peers: PeerMap,
    stop: Arc<AtomicBool>,
) -> Result<JoinHandle<()>, std::io::Error> {
    let socket = bind_beacon_socket()?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(Duration::from_millis(200)))?;
    let name = name.to_string();
    let device_id = device_id.to_string();
    let fingerprint = fingerprint.to_string();
    std::thread::Builder::new()
        .name("devhop-beacon".into())
        .spawn(move || {
            beacon_loop(socket, name, device_id, fingerprint, quic_port, peers, stop);
        })
}

fn beacon_loop(
    socket: UdpSocket,
    name: String,
    device_id: String,
    fingerprint: String,
    quic_port: u16,
    peers: PeerMap,
    stop: Arc<AtomicBool>,
) {
    let packet = beacon_packet(&name, &device_id, &fingerprint, quic_port);
    let mut last_send = Instant::now() - Duration::from_secs(2);
    let mut buf = [0u8; 1024];
    while !stop.load(Ordering::Relaxed) {
        if last_send.elapsed() >= Duration::from_secs(1) {
            for target in broadcast_targets() {
                let _ = socket.send_to(packet.as_bytes(), target);
            }
            last_send = Instant::now();
        }
        match socket.recv_from(&mut buf) {
            Ok((size, src)) => {
                if let Some(peer) = parse_beacon(&buf[..size], src) {
                    if peer.device_id != device_id {
                        remember(&peers, peer, Source::Beacon);
                    }
                }
            }
            Err(err) if beacon_recv_is_retryable(&err) => {}
            Err(err) => {
                eprintln!("devhop beacon recv error (retrying in 1s): {err}");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn bind_beacon_socket() -> Result<UdpSocket, std::io::Error> {
    let raw = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    raw.set_reuse_address(true)?;
    let addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, BEACON_PORT));
    raw.bind(&addr.into())?;
    Ok(raw.into())
}

/// Errors on beacon `recv_from` that should not tear down the listener thread.
pub(crate) fn beacon_recv_is_retryable(err: &Error) -> bool {
    matches!(
        err.kind(),
        ErrorKind::WouldBlock
            | ErrorKind::TimedOut
            | ErrorKind::Interrupted
            | ErrorKind::ConnectionReset
    )
}

fn remember_mdns(peers: &Mutex<HashMap<String, Seen>>, advert: PeerAdvert, fullname: String) {
    let Ok(mut map) = peers.lock() else {
        return;
    };
    let at = Instant::now();
    match map.entry(advert.device_id.clone()) {
        std::collections::hash_map::Entry::Occupied(mut slot) => {
            if slot.get().source == Source::Mdns {
                let seen = slot.get_mut();
                seen.advert = advert;
                seen.at = at;
                seen.mdns_fullname = Some(fullname);
            }
        }
        std::collections::hash_map::Entry::Vacant(slot) => {
            slot.insert(Seen {
                advert,
                at,
                source: Source::Mdns,
                mdns_fullname: Some(fullname),
                addr_locked: false,
            });
        }
    }
}

/// Drop only the mDNS row whose stored fullname matches (N5.2).
fn retain_after_mdns_removal(map: &mut HashMap<String, Seen>, fullname: &str) {
    map.retain(|_, seen| {
        seen.mdns_fullname.as_deref() != Some(fullname) || seen.source != Source::Mdns
    });
}

/// Beacon updates name and fingerprint; the address is the packet source unless
/// [`Seen::addr_locked`]. mDNS only fills in peers the beacon has not seen yet.
///
/// A forged beacon cannot redirect an established peer once the session layer
/// calls [`Discovery::lock_peer_address`]. An attacker on the LAN can still
/// spam beacons and delay discovery of the real address (beacons are not signed).
fn remember(peers: &Mutex<HashMap<String, Seen>>, advert: PeerAdvert, source: Source) {
    let Ok(mut map) = peers.lock() else {
        return;
    };
    let at = Instant::now();
    match source {
        Source::Beacon => {
            let merged = if let Some(existing) = map.get(&advert.device_id) {
                let addr = if existing.addr_locked {
                    existing.advert.addr
                } else {
                    advert.addr
                };
                PeerAdvert {
                    name: advert.name,
                    device_id: advert.device_id,
                    fingerprint: advert.fingerprint,
                    addr,
                }
            } else {
                advert
            };
            let device_id = merged.device_id.clone();
            let mdns_fullname = map.get(&device_id).and_then(|s| s.mdns_fullname.clone());
            let addr_locked = map.get(&device_id).is_some_and(|s| s.addr_locked);
            map.insert(
                device_id,
                Seen {
                    advert: merged,
                    at,
                    source,
                    mdns_fullname,
                    addr_locked,
                },
            );
        }
        Source::Mdns => {
            // Handled in remember_mdns.
        }
    }
}

/// `true` when `addr` is one of this computer's own addresses, so DevHop
/// never dials itself (a stale advert can point back at us).
pub fn is_own_addr(addr: SocketAddr, own_port: u16) -> bool {
    if addr.port() != own_port {
        return false;
    }
    let ip = addr.ip();
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    if_addrs::get_if_addrs()
        .map(|ifaces| ifaces.iter().any(|iface| iface.ip() == ip))
        .unwrap_or(false)
}

fn beacon_packet(name: &str, device_id: &str, fingerprint: &str, port: u16) -> String {
    let name = name.replace(['\n', '\r'], " ");
    let code = local_code(fingerprint);
    format!("DH1\n{code}\n{device_id}\n{fingerprint}\n{port}\n{name}\n")
}

fn parse_beacon(buf: &[u8], src: SocketAddr) -> Option<PeerAdvert> {
    let text = std::str::from_utf8(buf).ok()?;
    let mut lines = text.split('\n');
    if lines.next()? != "DH1" {
        return None;
    }
    let code = lines.next()?.trim();
    let device_id = lines.next()?.trim();
    let fingerprint = lines.next()?.trim();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let name = lines.next()?.trim();
    if device_id.is_empty() || fingerprint.is_empty() || local_code(fingerprint) != code {
        return None;
    }
    if src.ip().is_unspecified() || src.ip().is_multicast() {
        return None;
    }
    Some(PeerAdvert {
        name: if name.is_empty() {
            device_id.to_string()
        } else {
            name.to_string()
        },
        device_id: device_id.to_string(),
        fingerprint: fingerprint.to_string(),
        addr: SocketAddr::new(src.ip(), port),
    })
}

fn broadcast_targets() -> Vec<SocketAddr> {
    let mut targets = Vec::new();
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in ifaces {
            let if_addrs::IfAddr::V4(addr) = iface.addr else {
                continue;
            };
            if addr.ip.is_loopback() || addr.ip.is_unspecified() {
                continue;
            }
            if let Some(broadcast) = addr.broadcast {
                targets.push(SocketAddr::new(IpAddr::V4(broadcast), BEACON_PORT));
            }
        }
    }
    if targets.is_empty() {
        targets.push(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::BROADCAST),
            BEACON_PORT,
        ));
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_address_keeps_the_socket() {
        let peer = manual_peer("192.0.2.10:42424".parse().unwrap());
        assert_eq!(peer.addr.port(), 42424);
        assert!(peer.device_id.is_empty());
    }

    #[test]
    fn sanitize_strips_punctuation() {
        assert_eq!(sanitize("Ren's Mac"), "Ren-s-Mac");
        assert_eq!(sanitize("..."), "devhop");
    }

    #[test]
    fn this_computer_is_recognised() {
        assert!(is_own_addr("127.0.0.1:42424".parse().unwrap(), 42424));
        assert!(!is_own_addr("127.0.0.1:42430".parse().unwrap(), 42424));
        assert!(!is_own_addr("192.0.2.77:42424".parse().unwrap(), 42424));
    }

    #[test]
    fn beacon_entries_expire_and_mdns_entries_wait_for_forget() {
        let map: PeerMap = Arc::default();
        let advert = |id: &str| PeerAdvert {
            name: id.into(),
            device_id: id.into(),
            fingerprint: "f".into(),
            addr: "192.0.2.1:42424".parse().unwrap(),
        };
        remember(&map, advert("beacon"), Source::Beacon);
        remember_mdns(&map, advert("mdns"), "mdns-id._devhop._udp.local.".into());
        map.lock().unwrap().get_mut("beacon").unwrap().at -= BEACON_EXPIRY;
        map.lock().unwrap().get_mut("mdns").unwrap().at -= BEACON_EXPIRY;
        let discovery = Discovery {
            daemon: None,
            peers: map,
            beacon_stop: Arc::new(AtomicBool::new(true)),
            beacon_thread: None,
            note: None,
        };
        let ids: Vec<_> = discovery.peers().into_iter().map(|p| p.device_id).collect();
        assert_eq!(ids, vec!["mdns".to_string()]);
        discovery.forget("mdns");
        assert!(discovery.peers().is_empty());
    }

    #[test]
    #[ignore = "announces fake computers on the real LAN; run by hand"]
    fn two_advertisements_see_each_other() {
        let left = Discovery::start("alpha", "device-alpha", "aa", 42424).unwrap();
        let right = Discovery::start("beta", "device-beta", "bb", 42425).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        let mut saw_alpha = false;
        let mut saw_beta = false;
        while std::time::Instant::now() < deadline && !(saw_alpha && saw_beta) {
            saw_beta = right.peers().iter().any(|p| p.device_id == "device-alpha");
            saw_alpha = left.peers().iter().any(|p| p.device_id == "device-beta");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(saw_alpha, "alpha did not see beta: {:?}", left.peers());
        assert!(saw_beta, "beta did not see alpha: {:?}", right.peers());
    }

    #[test]
    fn beacon_uses_the_address_the_packet_came_from() {
        let packet = beacon_packet("Ren PC", "dev-1", "abc", 42424);
        let peer = parse_beacon(packet.as_bytes(), "192.168.1.20:42425".parse().unwrap()).unwrap();
        assert_eq!(peer.addr, "192.168.1.20:42424".parse().unwrap());
        assert_eq!(peer.name, "Ren PC");
        assert_eq!(peer.fingerprint, "abc");
    }

    #[test]
    fn beacon_rejects_a_code_that_does_not_match_the_certificate() {
        let packet = beacon_packet("A", "dev-1", "abc", 42424);
        let bad = packet.replacen(&local_code("abc"), "000000", 1);
        let src = "192.168.1.20:42425".parse().unwrap();
        assert!(parse_beacon(bad.as_bytes(), src).is_none());
    }

    #[test]
    fn two_beacon_sockets_can_share_the_port() {
        let _first = bind_beacon_socket().expect("first bind");
        let _second = bind_beacon_socket().expect("second bind with SO_REUSEADDR");
    }

    #[test]
    fn beacon_recv_retryable_classification() {
        assert!(beacon_recv_is_retryable(&Error::from(
            ErrorKind::WouldBlock
        )));
        assert!(beacon_recv_is_retryable(&Error::from(ErrorKind::TimedOut)));
        assert!(beacon_recv_is_retryable(&Error::from(
            ErrorKind::Interrupted
        )));
        assert!(beacon_recv_is_retryable(&Error::from(
            ErrorKind::ConnectionReset
        )));
        assert!(!beacon_recv_is_retryable(&Error::from(
            ErrorKind::PermissionDenied
        )));
    }

    #[test]
    fn mdns_removal_matches_stored_fullname_not_display_name() {
        let shared_name = "Ren Mac";
        let peers: PeerMap = Arc::new(Mutex::new(HashMap::new()));
        let insert = |id: &str, fullname: &str| {
            remember_mdns(
                &peers,
                PeerAdvert {
                    name: shared_name.into(),
                    device_id: id.into(),
                    fingerprint: "f".into(),
                    addr: "192.0.2.2:42424".parse().unwrap(),
                },
                fullname.to_string(),
            );
        };
        insert("device-a", "Ren-Mac._devhop._udp.local.");
        insert("device-b", "Ren-Mac-2._devhop._udp.local.");
        retain_after_mdns_removal(&mut peers.lock().unwrap(), "Ren-Mac._devhop._udp.local.");
        let ids: Vec<_> = peers.lock().unwrap().keys().cloned().collect();
        assert_eq!(ids, vec!["device-b".to_string()]);
    }

    #[test]
    fn locked_beacon_does_not_change_peer_address() {
        let peers: PeerMap = Arc::new(Mutex::new(HashMap::new()));
        let first = PeerAdvert {
            name: "A".into(),
            device_id: "dev-1".into(),
            fingerprint: "f".into(),
            addr: "192.0.2.10:42424".parse().unwrap(),
        };
        remember(&peers, first, Source::Beacon);
        peers.lock().unwrap().get_mut("dev-1").unwrap().addr_locked = true;
        let spoofed = PeerAdvert {
            name: "A".into(),
            device_id: "dev-1".into(),
            fingerprint: "f".into(),
            addr: "192.0.2.99:42424".parse().unwrap(),
        };
        remember(&peers, spoofed, Source::Beacon);
        assert_eq!(
            peers.lock().unwrap().get("dev-1").unwrap().advert.addr,
            "192.0.2.10:42424".parse().unwrap()
        );
    }
}
