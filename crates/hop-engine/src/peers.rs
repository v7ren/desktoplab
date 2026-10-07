//! Paired peers on disk.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PeerStore {
    pub peers: Vec<StoredPeer>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredPeer {
    pub id: String,
    pub name: String,
    pub fingerprint: String,
    pub last_addr: String,
}

impl PeerStore {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("peers.toml");
        fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> Result<(), String> {
        fs::create_dir_all(dir).map_err(|err| err.to_string())?;
        let text = toml::to_string_pretty(self).map_err(|err| err.to_string())?;
        fs::write(dir.join("peers.toml"), text).map_err(|err| err.to_string())
    }

    pub fn pin(&mut self, peer: StoredPeer) {
        self.peers
            .retain(|existing| existing.id != peer.id && existing.fingerprint != peer.fingerprint);
        self.peers.push(peer);
    }

    pub fn forget(&mut self, id: &str) {
        self.peers.retain(|peer| peer.id != id);
    }

    pub fn by_fingerprint(&self, fingerprint: &str) -> Option<&StoredPeer> {
        self.peers
            .iter()
            .find(|peer| peer.fingerprint == fingerprint)
    }
}
