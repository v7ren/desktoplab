//! TOML config. First launch writes the defaults.

use std::fs;
use std::path::Path;

use hop_core::{Chord, HopMode, HotAction, ModifierMap, ScrollNorm};
use serde::{Deserialize, Serialize};

use crate::hotkeys::expand;

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
    pub port: u16,
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
            device_name: hostname(),
            hop_mode: HopMode::Relative,
            wrap: false,
            paused: false,
            hotkeys: HotkeyConfig::default(),
            clipboard_sync: true,
            modifiers: ModifierMap::default(),
            scroll: ScrollNorm::default(),
            port: 42424,
        }
    }
}

impl Config {
    pub fn load_or_create(dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(dir).map_err(|err| err.to_string())?;
        let path = dir.join("config.toml");
        if path.exists() {
            let text = fs::read_to_string(&path).map_err(|err| err.to_string())?;
            toml::from_str(&text).map_err(|err| err.to_string())
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

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "devhop".into())
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
}
