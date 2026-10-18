//! TOML config. First launch writes the defaults.

use std::fs;
use std::path::Path;

use hop_core::{Chord, CrossMode, HopMode, HotAction, ModifierMap, ScrollNorm};
use serde::{Deserialize, Serialize};

use crate::hotkeys::expand;

pub const PARK_INSET_MIN: i32 = 40;
pub const PARK_INSET_MAX: i32 = 400;
pub const PARK_INSET_DEFAULT: i32 = 160;

pub fn clamp_park_inset(px: i32) -> i32 {
    px.clamp(PARK_INSET_MIN, PARK_INSET_MAX)
}

fn default_park_inset_px() -> i32 {
    PARK_INSET_DEFAULT
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub device_id: String,
    pub device_name: String,
    pub hop_mode: HopMode,
    pub wrap: bool,
    pub paused: bool,
    pub hotkeys: HotkeyConfig,
    pub clipboard_sync: bool,
    pub modifiers: ModifierMap,
    pub scroll: ScrollNorm,
    /// When another computer controls this machine, negate wheel deltas before inject.
    #[serde(default)]
    pub scroll_reverse_incoming: bool,
    #[serde(default = "default_cross_mode")]
    pub cross_mode: CrossMode,
    pub port: u16,
    /// Announce this computer and look for others on the LAN. Off only in
    /// tests, so test runs never show up as computers on a real network.
    #[serde(default = "yes")]
    pub discovery: bool,
    /// Inset from the monitor edge where the cursor parks while controlling a peer (Windows).
    #[serde(default = "default_park_inset_px")]
    pub park_inset_px: i32,
}

fn yes() -> bool {
    true
}

fn default_cross_mode() -> CrossMode {
    CrossMode::Layout
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HotkeyConfig {
    pub next_monitor: String,
    pub prev_monitor: String,
    pub monitor_n: String,
    pub center: String,
    pub find_cursor: String,
    pub lock_monitor: String,
    pub jump_device_n: String,
    pub return_home: String,
    pub panic: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            next_monitor: "ctrl+alt+right".into(),
            prev_monitor: "ctrl+alt+left".into(),
            monitor_n: "ctrl+alt+{n}".into(),
            center: "ctrl+alt+c".into(),
            find_cursor: "ctrl+alt+f".into(),
            lock_monitor: "scrolllock".into(),
            jump_device_n: "ctrl+alt+shift+{n}".into(),
            return_home: "ctrl+alt+shift+h".into(),
            panic: "ctrl+alt+esc".into(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device_id: uuid::Uuid::new_v4().to_string(),
            device_name: hop_net::device_name(),
            hop_mode: HopMode::Relative,
            wrap: false,
            paused: false,
            hotkeys: HotkeyConfig::default(),
            clipboard_sync: true,
            modifiers: ModifierMap::default(),
            scroll: ScrollNorm::default(),
            scroll_reverse_incoming: false,
            cross_mode: CrossMode::Layout,
            port: 42424,
            discovery: true,
            park_inset_px: PARK_INSET_DEFAULT,
        }
    }
}

impl Config {
    pub fn normalize(&mut self) {
        self.park_inset_px = clamp_park_inset(self.park_inset_px);
    }

    pub fn load_or_create(dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(dir).map_err(|err| err.to_string())?;
        let path = dir.join("config.toml");
        if path.exists() {
            let text = fs::read_to_string(&path).map_err(|err| err.to_string())?;
            let mut config: Config = toml::from_str(&text).map_err(|err| err.to_string())?;
            config.normalize();
            Ok(config)
        } else {
            let config = Config::default();
            config.save(dir)?;
            Ok(config)
        }
    }

    pub fn save(&self, dir: &Path) -> Result<(), String> {
        fs::create_dir_all(dir).map_err(|err| err.to_string())?;
        let text = toml::to_string_pretty(self).map_err(|err| err.to_string())?;
        fs::write(dir.join("config.toml"), text).map_err(|err| err.to_string())
    }

    pub fn chords(&self) -> Vec<(Chord, HotAction)> {
        expand(&self.hotkeys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_writes_defaults_once() {
        let dir = tempfile::tempdir().unwrap();
        let first = Config::load_or_create(dir.path()).unwrap();
        assert!(dir.path().join("config.toml").exists());
        assert_eq!(first.port, 42424);
        assert!(first.clipboard_sync);
        let mut edited = first.clone();
        edited.wrap = true;
        edited.hop_mode = HopMode::Memory;
        edited.hotkeys.panic = "ctrl+alt+f12".into();
        edited.save(dir.path()).unwrap();
        let loaded = Config::load_or_create(dir.path()).unwrap();
        assert!(loaded.wrap);
        assert_eq!(loaded.hop_mode, HopMode::Memory);
        assert_eq!(loaded.device_id, first.device_id);
        assert_eq!(loaded.hotkeys.panic, "ctrl+alt+f12");
        assert!(!loaded.chords().is_empty());
    }

    #[test]
    fn scroll_reverse_incoming_defaults_when_missing_from_toml() {
        let dir = tempfile::tempdir().unwrap();
        Config::default().save(dir.path()).unwrap();
        let path = dir.path().join("config.toml");
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .filter(|line| !line.trim_start().starts_with("scroll_reverse_incoming"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("scroll_reverse_incoming"));
        std::fs::write(&path, &text).unwrap();
        let loaded = Config::load_or_create(dir.path()).unwrap();
        assert!(!loaded.scroll_reverse_incoming);

        std::fs::write(&path, format!("scroll_reverse_incoming = true\n{}", text)).unwrap();
        let loaded = Config::load_or_create(dir.path()).unwrap();
        assert!(loaded.scroll_reverse_incoming);
    }

    #[test]
    fn park_inset_defaults_and_clamps_on_load() {
        assert_eq!(Config::default().park_inset_px, PARK_INSET_DEFAULT);
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.park_inset_px = 999;
        cfg.save(dir.path()).unwrap();
        let loaded = Config::load_or_create(dir.path()).unwrap();
        assert_eq!(loaded.park_inset_px, PARK_INSET_MAX);

        let path = dir.path().join("config.toml");
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .filter(|line| !line.trim_start().starts_with("park_inset_px"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, text).unwrap();
        let loaded = Config::load_or_create(dir.path()).unwrap();
        assert_eq!(loaded.park_inset_px, PARK_INSET_DEFAULT);
    }

    #[test]
    fn clamp_park_inset_bounds() {
        assert_eq!(clamp_park_inset(10), PARK_INSET_MIN);
        assert_eq!(clamp_park_inset(160), 160);
        assert_eq!(clamp_park_inset(500), PARK_INSET_MAX);
    }

    /// The old `ctrl_cmd` key still loads into the renamed `alt_cmd` field, so
    /// an existing config.toml keeps its setting after the upgrade.
    #[test]
    fn old_ctrl_cmd_key_still_loads_as_alt_cmd() {
        let dir = tempfile::tempdir().unwrap();
        Config::default().save(dir.path()).unwrap();
        let path = dir.path().join("config.toml");
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("alt_cmd = true", "ctrl_cmd = false");
        std::fs::write(&path, text).unwrap();
        let loaded = Config::load_or_create(dir.path()).unwrap();
        assert!(!loaded.modifiers.alt_cmd);
        assert!(loaded.modifiers.win_option);
    }
}
