//! Per-peer monitor arrangement persisted beside `config.toml`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use hop_core::{DeviceId, Layout, Monitor};

const LAYOUTS_DIR: &str = "layouts";

pub fn layouts_dir(config_dir: &Path) -> PathBuf {
    config_dir.join(LAYOUTS_DIR)
}

pub fn load_all(dir: &Path) -> HashMap<String, Layout> {
    let root = layouts_dir(dir);
    let Ok(entries) = fs::read_dir(&root) else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            let peer = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            if let Ok(text) = fs::read_to_string(&path) {
                if let Ok(layout) = serde_json::from_str::<Layout>(&text) {
                    out.insert(peer, layout);
                }
            }
        }
    }
    out
}

pub fn save_peer(dir: &Path, peer_id: &str, remote: &Layout) -> Result<(), String> {
    let root = layouts_dir(dir);
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(remote).map_err(|e| e.to_string())?;
    fs::write(root.join(format!("{peer_id}.json")), text).map_err(|e| e.to_string())
}

/// Remote devices and their monitors (no local device).
pub fn remote_fragment(layout: &Layout, local: &DeviceId) -> Layout {
    Layout {
        devices: layout
            .devices
            .iter()
            .filter(|d| &d.id != local)
            .cloned()
            .collect(),
        monitors: layout
            .monitors
            .iter()
            .filter(|m| &m.device_id != local)
            .cloned()
            .collect(),
        wrap: layout.wrap,
    }
}

pub fn apply_saved(local: &mut Layout, saved: &Layout) {
    let ids: Vec<DeviceId> = saved.devices.iter().map(|d| d.id.clone()).collect();
    local.devices.retain(|d| !ids.contains(&d.id));
    local.monitors.retain(|m| !ids.contains(&m.device_id));
    local.devices.extend(saved.devices.clone());
    local.monitors.extend(saved.monitors.clone());
}

pub fn translate_monitors(monitors: &[Monitor], dx: i32, dy: i32) -> Vec<Monitor> {
    monitors
        .iter()
        .map(|m| {
            let mut copy = m.clone();
            copy.bounds.x = copy.bounds.x.saturating_add(dx);
            copy.bounds.y = copy.bounds.y.saturating_add(dy);
            copy
        })
        .collect()
}

pub fn alignment_delta(received: &Layout, local: &Layout, device: &DeviceId) -> Option<(i32, i32)> {
    for rm in received.monitors.iter().filter(|m| &m.device_id == device) {
        if let Some(here) = local.monitor(&rm.id) {
            return Some((here.bounds.x - rm.bounds.x, here.bounds.y - rm.bounds.y));
        }
    }
    None
}

/// Place `remote` beside `local` when the message has no local monitors yet.
pub fn default_place(local: &Layout, device: &DeviceId, from_peer: &str, remote: Layout) -> Layout {
    let mine = local.device_monitors(device);
    if mine.is_empty() || remote.monitors.is_empty() {
        return remote;
    }
    let local_left = mine.iter().map(|m| m.bounds.x).min().unwrap_or(0);
    let local_right = mine.iter().map(|m| m.bounds.right()).max().unwrap_or(0);
    let local_top = mine.iter().map(|m| m.bounds.y).min().unwrap_or(0);
    let remote_left = remote
        .monitors
        .iter()
        .map(|m| m.bounds.x)
        .min()
        .unwrap_or(0);
    let remote_right = remote
        .monitors
        .iter()
        .map(|m| m.bounds.right())
        .max()
        .unwrap_or(0);
    let remote_top = remote
        .monitors
        .iter()
        .map(|m| m.bounds.y)
        .min()
        .unwrap_or(0);
    let (dx, dy) = if device.as_str() < from_peer {
        (local_right - remote_left, local_top - remote_top)
    } else {
        (local_left - remote_right, local_top - remote_top)
    };
    Layout {
        devices: remote.devices,
        monitors: translate_monitors(&remote.monitors, dx, dy),
        wrap: remote.wrap,
    }
}

/// True when the sender included our monitors (matched by id) in the payload.
pub fn received_includes_local(received: &Layout, local_device: &DeviceId) -> bool {
    received
        .monitors
        .iter()
        .any(|m| &m.device_id == local_device)
}

/// Shift the sender's full layout so our monitors land on physical coordinates.
pub fn align_incoming_layout(
    received: &Layout,
    local: &Layout,
    local_device: &DeviceId,
) -> Option<Layout> {
    let Some((dx, dy)) = alignment_delta(received, local, local_device) else {
        return None;
    };
    Some(Layout {
        devices: received.devices.clone(),
        monitors: translate_monitors(&received.monitors, dx, dy),
        wrap: received.wrap,
    })
}

pub fn merge_remote_into(local: &mut Layout, device: &DeviceId, incoming: Layout) {
    let remote_ids: Vec<DeviceId> = incoming
        .devices
        .iter()
        .filter(|d| &d.id != device)
        .map(|d| d.id.clone())
        .collect();
    if remote_ids.is_empty() {
        return;
    }
    local.devices.retain(|d| !remote_ids.contains(&d.id));
    local
        .monitors
        .retain(|m| !remote_ids.contains(&m.device_id));
    local
        .devices
        .extend(incoming.devices.into_iter().filter(|d| &d.id != device));
    local.monitors.extend(
        incoming
            .monitors
            .into_iter()
            .filter(|m| &m.device_id != device),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use hop_core::{Device, Monitor, MonitorId, PhysRect};

    fn mon(id: &str, dev: &str, x: i32, y: i32) -> Monitor {
        Monitor {
            id: MonitorId(id.into()),
            device_id: DeviceId(dev.into()),
            name: id.into(),
            bounds: PhysRect::new(x, y, 100, 100),
            scale: 1.0,
        }
    }

    #[test]
    fn default_placement_is_mirror_consistent() {
        let pc = DeviceId("pc".into());
        let mac = DeviceId("mac".into());
        let local_pc = Layout {
            devices: vec![Device {
                id: pc.clone(),
                name: "pc".into(),
            }],
            monitors: vec![mon("pc:1", "pc", 0, 0)],
            wrap: false,
        };
        let remote_only = Layout {
            devices: vec![Device {
                id: mac.clone(),
                name: "mac".into(),
            }],
            monitors: vec![mon("mac:1", "mac", 0, 0)],
            wrap: false,
        };
        let on_pc = default_place(&local_pc, &pc, "mac", remote_only.clone());
        assert_eq!(on_pc.monitors[0].bounds.x, -100, "mac sits left of pc");
        let local_mac = Layout {
            devices: vec![Device {
                id: mac.clone(),
                name: "mac".into(),
            }],
            monitors: vec![mon("mac:1", "mac", 0, 0)],
            wrap: false,
        };
        let on_mac = default_place(&local_mac, &mac, "pc", remote_only);
        assert_eq!(on_mac.monitors[0].bounds.x, 100, "pc sits right of mac");
    }
}
