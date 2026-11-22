//! Windows toast notifications (low battery, "still running in the tray").
//!
//! An unpackaged app can only show toasts under a registered AppUserModelID, so the ID is
//! registered under `HKCU\Software\Classes\AppUserModelId` at every start (portable copies
//! included) and set as the process's explicit ID.

use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::thread;

use crate::bluetooth::BudsClient;
use crate::protocol::{BudsState, PlacementState};
use crate::settings::SettingsStore;
use crate::win::{self, APP_NAME};
use crate::{log_info, log_warn};

pub const AUMID: &str = "Pods4.GalaxyBudsControls";
const AUMID_KEY: &str = r"Software\Classes\AppUserModelId\Pods4.GalaxyBudsControls";
const ICON_PNG: &[u8] = include_bytes!("../assets/pods4-256.png");

/// Gives this process our AppUserModelID (taskbar grouping, toast attribution).
pub fn set_process_app_id() {
    use windows::core::HSTRING;
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
    unsafe {
        let _ = SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(AUMID));
    }
}

/// Registers the toast sender name and icon. Cheap and idempotent; done at every start.
pub fn register_app_id() {
    let icon = win::data_dir().join("pods4.png");
    if !icon.is_file() {
        let _ = std::fs::create_dir_all(win::data_dir());
        let _ = std::fs::write(&icon, ICON_PNG);
    }
    if let Err(e) = win::reg_set_values(
        AUMID_KEY,
        &[("DisplayName", APP_NAME), ("IconUri", &icon.display().to_string())],
        &[],
    ) {
        log_warn!("Could not register notification identity: {e}");
    }
}

pub fn unregister_app_id() {
    win::reg_delete_tree(AUMID_KEY);
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

fn toast_xml(title: &str, body: &str) -> String {
    format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        xml_escape(title),
        xml_escape(body)
    )
}

fn show_toast(title: &str, body: &str) -> windows::core::Result<()> {
    use windows::core::HSTRING;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};

    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(toast_xml(title, body)))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?.Show(&toast)
}

// ---------------------------------------------------------------- low battery

/// Decides when to warn about low earbud battery: once per discharge per bud, only while the
/// bud is out of the case and not charging, re-armed after it charges or climbs well above
/// the threshold.
#[derive(Default)]
pub struct LowBatteryWatch {
    alerted_l: bool,
    alerted_r: bool,
}

fn check_bud(alerted: &mut bool, level: u8, placement: PlacementState, charging: bool, threshold: u8) -> Option<u8> {
    let out_of_case = matches!(placement, PlacementState::Wearing | PlacementState::Idle);
    if charging || placement == PlacementState::Charging || level > threshold.saturating_add(5) {
        *alerted = false;
        return None;
    }
    if !out_of_case || level == 0 || level > threshold || *alerted {
        return None;
    }
    *alerted = true;
    Some(level)
}

impl LowBatteryWatch {
    /// Returns the toast body when a new low-battery warning is due.
    pub fn check(&mut self, s: &BudsState, connected: bool, threshold: u8) -> Option<String> {
        if !connected {
            return None;
        }
        let l = check_bud(&mut self.alerted_l, s.battery_l, s.placement_l, s.is_left_charging, threshold);
        let r = check_bud(&mut self.alerted_r, s.battery_r, s.placement_r, s.is_right_charging, threshold);
        match (l, r) {
            (Some(l), Some(r)) => Some(format!("Left earbud {l}%, right earbud {r}%.")),
            (Some(l), None) => Some(format!("Left earbud is at {l}%.")),
            (None, Some(r)) => Some(format!("Right earbud is at {r}%.")),
            (None, None) => None,
        }
    }
}

// ---------------------------------------------------------------- notifier thread

enum Msg {
    StateChanged,
    Toast(String, String),
}

/// Handle to the notification thread (WinRT calls stay off the UI and Bluetooth threads).
#[derive(Clone)]
pub struct Notifier {
    tx: Sender<Msg>,
}

impl Notifier {
    pub fn spawn(client: BudsClient, settings: Arc<SettingsStore>) -> Self {
        let (tx, rx) = channel::<Msg>();
        thread::spawn(move || {
            use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
            unsafe {
                let _ = RoInitialize(RO_INIT_MULTITHREADED);
            }
            let mut watch = LowBatteryWatch::default();
            while let Ok(msg) = rx.recv() {
                let toast = match msg {
                    Msg::Toast(title, body) => Some((title, body)),
                    Msg::StateChanged => {
                        // Coalesce a burst of state updates into one check.
                        while let Ok(Msg::StateChanged) = rx.try_recv() {}
                        let prefs = settings.get();
                        let snap = client.snapshot();
                        let due = watch.check(&snap.state, snap.connected, prefs.low_battery_threshold);
                        due.filter(|_| prefs.low_battery_alerts)
                            .map(|body| (format!("{} battery low", snap.device_name), body))
                    }
                };
                if let Some((title, body)) = toast {
                    match show_toast(&title, &body) {
                        Ok(()) => log_info!("Notification: {title} — {body}"),
                        Err(e) => log_warn!("Could not show notification: {e}"),
                    }
                }
            }
        });
        Self { tx }
    }

    /// Call whenever the buds' state may have changed.
    pub fn state_changed(&self) {
        let _ = self.tx.send(Msg::StateChanged);
    }

    pub fn toast(&self, title: &str, body: &str) {
        let _ = self.tx.send(Msg::Toast(title.into(), body.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(l: u8, pl: PlacementState, r: u8, pr: PlacementState) -> BudsState {
        BudsState { battery_l: l, placement_l: pl, battery_r: r, placement_r: pr, ..BudsState::default() }
    }

    #[test]
    fn warns_once_per_discharge_and_rearms_after_charging() {
        use PlacementState::*;
        let mut w = LowBatteryWatch::default();
        assert_eq!(w.check(&state(40, Wearing, 50, Wearing), true, 15), None);
        assert_eq!(w.check(&state(15, Wearing, 50, Wearing), true, 15).as_deref(), Some("Left earbud is at 15%."));
        assert_eq!(w.check(&state(12, Wearing, 50, Wearing), true, 15), None, "no repeat");
        // Charging in the case re-arms; dropping low again warns again.
        assert_eq!(w.check(&state(16, Charging, 50, Wearing), true, 15), None);
        assert!(w.check(&state(14, Wearing, 50, Wearing), true, 15).is_some());
    }

    #[test]
    fn combines_both_buds_and_ignores_case_or_unknown() {
        use PlacementState::*;
        let mut w = LowBatteryWatch::default();
        assert_eq!(w.check(&state(10, Case, 0, Disconnected), true, 15), None);
        assert_eq!(w.check(&state(10, Idle, 9, Wearing), false, 15), None, "disconnected");
        assert_eq!(
            w.check(&state(10, Idle, 9, Wearing), true, 15).as_deref(),
            Some("Left earbud 10%, right earbud 9%.")
        );
    }

    #[test]
    fn toast_text_is_escaped() {
        let xml = toast_xml("Rock & Roll <Buds>", "\"5%\"");
        assert!(xml.contains("Rock &amp; Roll &lt;Buds&gt;"));
        assert!(xml.contains("&quot;5%&quot;"));
    }
}
