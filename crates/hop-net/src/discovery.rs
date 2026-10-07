//! mDNS `_devhop._udp` plus a manual address for networks that block it.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::NetError;

pub fn service_type() -> &'static str {
    "_devhop._udp.local."
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerAdvert {
    pub name: String,
    pub device_id: String,
    pub fingerprint: String,
    pub addr: SocketAddr,
}

pub struct Discovery {
    daemon: ServiceDaemon,
    peers: Arc<Mutex<HashMap<String, PeerAdvert>>>,
}

impl Discovery {
    pub fn start(
        name: &str,
        device_id: &str,
        fingerprint: &str,
        port: u16,
    ) -> Result<Self, NetError> {
        let daemon = ServiceDaemon::new().map_err(|err| NetError::Mdns(err.to_string()))?;
        let ip = lan_ip().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
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
        .map_err(|err| NetError::Mdns(err.to_string()))?;
        daemon
            .register(info)
            .map_err(|err| NetError::Mdns(err.to_string()))?;
        let receiver = daemon
            .browse(service_type())
            .map_err(|err| NetError::Mdns(err.to_string()))?;
        let peers = Arc::new(Mutex::new(HashMap::new()));
        let slot = Arc::clone(&peers);
        let mine = device_id.to_string();
        std::thread::Builder::new()
            .name("devhop-mdns".into())
            .spawn(move || {
                while let Ok(event) = receiver.recv() {
                    if let ServiceEvent::ServiceResolved(info) = event {
                        if let Some(peer) = advert_from(&info) {
                            if peer.device_id != mine && !peer.device_id.is_empty() {
                                if let Ok(mut map) = slot.lock() {
                                    map.insert(peer.device_id.clone(), peer);
                                }
                            }
                        }
                    }
                }
            })
            .map_err(NetError::Io)?;
        Ok(Self { daemon, peers })
    }

    pub fn peers(&self) -> Vec<PeerAdvert> {
        self.peers
            .lock()
            .map(|guard| guard.values().cloned().collect())
            .unwrap_or_default()
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
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

fn lan_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    Some(socket.local_addr().ok()?.ip())
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
}
