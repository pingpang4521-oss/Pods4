//! User preferences stored as `key=value` lines in `%LOCALAPPDATA%\Pods4\settings.ini`.
//! "Start with Windows" is not stored here: the Run registry value is the source of truth.

use std::path::PathBuf;
use std::sync::Mutex;

use crate::log_warn;

/// Low-battery alert levels offered in the UI.
pub const BATTERY_THRESHOLDS: [u8; 4] = [10, 15, 20, 30];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub low_battery_alerts: bool,
    pub low_battery_threshold: u8,
    /// The "still running in the tray" notification has been shown once.
    pub tray_hint_shown: bool,
    /// The user dismissed the "Install Pods4" banner of a portable copy.
    pub install_banner_dismissed: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            low_battery_alerts: true,
            low_battery_threshold: 15,
            tray_hint_shown: false,
            install_banner_dismissed: false,
        }
    }
}

impl Settings {
    /// Unknown keys and malformed values are ignored, so older or hand-edited files still load.
    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let (key, value) = (key.trim(), value.trim());
            let flag = matches!(value, "1" | "true" | "yes");
            match key {
                "low_battery_alerts" => s.low_battery_alerts = flag,
                "low_battery_threshold" => {
                    if let Ok(v) = value.parse::<u8>() {
                        s.low_battery_threshold = v.clamp(5, 50);
                    }
                }
                "tray_hint_shown" => s.tray_hint_shown = flag,
                "install_banner_dismissed" => s.install_banner_dismissed = flag,
                _ => {}
            }
        }
        s
    }

    pub fn to_text(&self) -> String {
        format!(
            "low_battery_alerts={}\nlow_battery_threshold={}\ntray_hint_shown={}\ninstall_banner_dismissed={}\n",
            self.low_battery_alerts, self.low_battery_threshold, self.tray_hint_shown, self.install_banner_dismissed
        )
    }
}

/// Shared, persisted settings. Every `update` writes the file.
pub struct SettingsStore {
    path: PathBuf,
    current: Mutex<Settings>,
}

impl SettingsStore {
    pub fn load(path: PathBuf) -> Self {
        let current = std::fs::read_to_string(&path).map(|t| Settings::parse(&t)).unwrap_or_default();
        Self { path, current: Mutex::new(current) }
    }

    pub fn get(&self) -> Settings {
        self.current.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn update(&self, change: impl FnOnce(&mut Settings)) {
        let text = {
            let mut s = self.current.lock().unwrap_or_else(|e| e.into_inner());
            change(&mut s);
            s.to_text()
        };
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&self.path, text) {
            log_warn!("Could not save settings to {}: {e}", self.path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_text() {
        let s = Settings {
            low_battery_alerts: false,
            low_battery_threshold: 20,
            tray_hint_shown: true,
            install_banner_dismissed: true,
        };
        assert_eq!(Settings::parse(&s.to_text()), s);
    }

    #[test]
    fn ignores_junk_and_clamps_values() {
        let s = Settings::parse("garbage\nlow_battery_threshold=200\nunknown=1\nlow_battery_alerts = no\n");
        assert_eq!(s.low_battery_threshold, 50);
        assert!(!s.low_battery_alerts);
        assert!(!s.tray_hint_shown);
        assert_eq!(Settings::parse(""), Settings::default());
    }
}
