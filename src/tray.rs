//! System tray icon on its own thread with its own message loop.
//!
//! egui only runs while the window is visible, so the tray used to show a stale icon and
//! tooltip whenever Pods4 sat hidden in the tray. Owning the tray here lets Bluetooth updates
//! refresh it at any time (`TrayRefresher::refresh` posts a message to this thread).

use std::sync::mpsc::{channel, Receiver};
use std::thread;
use std::time::Duration;

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, PeekMessageW, PostThreadMessageW, TranslateMessage, MSG,
    PM_NOREMOVE, WM_APP, WM_USER,
};

use crate::bluetooth::ConnectionSnapshot;
use crate::control::AppControl;
use crate::protocol::{set_noise_control, NoiseControlMode, PlacementState};

const WM_APP_REFRESH: u32 = WM_APP + 1;

const MODES: [(NoiseControlMode, &str); 4] = [
    (NoiseControlMode::Anc, "Noise cancelling"),
    (NoiseControlMode::Ambient, "Ambient sound"),
    (NoiseControlMode::Adaptive, "Adaptive"),
    (NoiseControlMode::Off, "Off"),
];

/// Cheap, copyable handle for asking the tray to re-read the connection state.
#[derive(Clone, Copy)]
pub struct TrayRefresher {
    thread_id: u32,
}

impl TrayRefresher {
    pub fn thread_id(&self) -> u32 {
        self.thread_id
    }

    pub fn refresh(&self) {
        unsafe {
            PostThreadMessageW(self.thread_id, WM_APP_REFRESH, 0, 0);
        }
    }
}

pub struct Tray {
    pub refresher: TrayRefresher,
    done: Receiver<()>,
}

impl Tray {
    /// Waits briefly for the tray thread to remove its icon (after `AppControl::exit`).
    pub fn wait_closed(&self) {
        let _ = self.done.recv_timeout(Duration::from_millis(800));
    }
}

fn make_dot_icon(r: u8, g: u8, b: u8) -> Icon {
    const S: usize = 32;
    let mut rgba = vec![0u8; S * S * 4];
    let c = 15.5_f32;
    for y in 0..S {
        for x in 0..S {
            let (dx, dy) = (x as f32 - c, y as f32 - c);
            let dist = (dx * dx + dy * dy).sqrt();
            let idx = (y * S + x) * 4;
            if dist <= 13.0 {
                let ring = (6.0..=8.4).contains(&dist);
                let (pr, pg, pb) = if ring { (255, 255, 255) } else { (r, g, b) };
                rgba[idx] = pr;
                rgba[idx + 1] = pg;
                rgba[idx + 2] = pb;
                rgba[idx + 3] = if dist > 12.0 { ((13.0 - dist) * 255.0) as u8 } else { 255 };
            }
        }
    }
    Icon::from_rgba(rgba, S as u32, S as u32).expect("valid RGBA icon")
}

fn mode_rgb(mode: NoiseControlMode) -> (u8, u8, u8) {
    match mode {
        NoiseControlMode::Ambient => (0x10, 0xB9, 0x81),
        NoiseControlMode::Anc => (0x3B, 0x82, 0xF6),
        NoiseControlMode::Adaptive => (0x8B, 0x5C, 0xF6),
        NoiseControlMode::Off => (0x64, 0x74, 0x8B),
    }
}

const OFFLINE_RGB: (u8, u8, u8) = (0x64, 0x74, 0x8B);

/// Tooltip text, e.g. "Galaxy Buds4 Pro — ANC · L 80% · R 75%".
pub fn tooltip(snap: &ConnectionSnapshot) -> String {
    if !snap.connected {
        return format!("Pods4 — {}", if snap.connecting { "Connecting..." } else { "Disconnected" });
    }
    let s = &snap.state;
    let mut tip = format!("{} — {}", snap.device_name, s.mode.label());
    if s.placement_l != PlacementState::Disconnected {
        tip += &format!(" · L {}%", s.battery_l);
    }
    if s.placement_r != PlacementState::Disconnected {
        tip += &format!(" · R {}%", s.battery_r);
    }
    // The shell truncates tooltips at 127 characters.
    tip.chars().take(127).collect()
}

struct Items {
    modes: Vec<CheckMenuItem>,
}

fn build() -> Result<(TrayIcon, Items, AllIds), String> {
    let menu = Menu::new();
    let show = MenuItem::new("Show Pods4", true, None);
    let modes: Vec<CheckMenuItem> = MODES.iter().map(|(_, label)| CheckMenuItem::new(*label, false, false, None)).collect();
    let reconnect = MenuItem::new("Reconnect", true, None);
    let exit = MenuItem::new("Exit", true, None);

    let ids = AllIds {
        show: show.id().clone(),
        modes: modes.iter().map(|m| m.id().clone()).collect(),
        reconnect: reconnect.id().clone(),
        exit: exit.id().clone(),
    };

    menu.append(&show).map_err(|e| e.to_string())?;
    menu.append(&PredefinedMenuItem::separator()).map_err(|e| e.to_string())?;
    for m in &modes {
        menu.append(m).map_err(|e| e.to_string())?;
    }
    menu.append(&PredefinedMenuItem::separator()).map_err(|e| e.to_string())?;
    menu.append(&reconnect).map_err(|e| e.to_string())?;
    menu.append(&exit).map_err(|e| e.to_string())?;

    let (r, g, b) = OFFLINE_RGB;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("Pods4")
        .with_icon(make_dot_icon(r, g, b))
        .build()
        .map_err(|e| format!("Failed to create system tray icon: {e}"))?;
    Ok((tray, Items { modes }, ids))
}

#[derive(Clone)]
struct AllIds {
    show: tray_icon::menu::MenuId,
    modes: Vec<tray_icon::menu::MenuId>,
    reconnect: tray_icon::menu::MenuId,
    exit: tray_icon::menu::MenuId,
}

fn post_refresh_to_self() {
    unsafe {
        PostThreadMessageW(GetCurrentThreadId(), WM_APP_REFRESH, 0, 0);
    }
}

pub fn spawn(control: AppControl) -> Result<Tray, String> {
    let (ready_tx, ready_rx) = channel::<Result<u32, String>>();
    let (done_tx, done_rx) = channel::<()>();

    thread::spawn(move || {
        // Create this thread's message queue before anyone posts to it.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            PeekMessageW(&mut msg, std::ptr::null_mut(), WM_USER, WM_USER, PM_NOREMOVE);
        }
        let (tray, items, ids) = match build() {
            Ok(t) => t,
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };
        let _ = ready_tx.send(Ok(unsafe { GetCurrentThreadId() }));

        let c = control.clone();
        TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                c.toggle();
            }
        }));

        let c = control.clone();
        let handler_ids = ids.clone();
        MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
            let id = ev.id();
            if *id == handler_ids.show {
                c.show();
            } else if let Some(i) = handler_ids.modes.iter().position(|m| m == id) {
                let mode = MODES[i].0;
                c.client.send_with(set_noise_control(mode), |st| st.mode = mode);
                // The menu toggles the check itself; re-sync it with the real state.
                post_refresh_to_self();
            } else if *id == handler_ids.reconnect {
                c.client.connect_auto_async();
            } else if *id == handler_ids.exit {
                c.exit();
            }
        }));

        let mut last: Option<((u8, u8, u8), String)> = None;
        let mut refresh = |snap: &ConnectionSnapshot| {
            let rgb = if snap.connected { mode_rgb(snap.state.mode) } else { OFFLINE_RGB };
            let tip = tooltip(snap);
            if last.as_ref() != Some(&(rgb, tip.clone())) {
                let _ = tray.set_icon(Some(make_dot_icon(rgb.0, rgb.1, rgb.2)));
                let _ = tray.set_tooltip(Some(&tip));
                last = Some((rgb, tip));
            }
            for (item, (mode, _)) in items.modes.iter().zip(MODES) {
                item.set_enabled(snap.connected);
                item.set_checked(snap.connected && snap.state.mode == mode);
            }
        };
        refresh(&control.client.snapshot());

        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                if msg.hwnd.is_null() && msg.message == WM_APP_REFRESH {
                    refresh(&control.client.snapshot());
                    continue;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        TrayIconEvent::set_event_handler(None::<fn(TrayIconEvent)>);
        MenuEvent::set_event_handler(None::<fn(MenuEvent)>);
        drop(tray); // removes the icon from the notification area
        let _ = done_tx.send(());
    });

    let thread_id = ready_rx.recv().map_err(|_| "Tray thread failed to start".to_string())??;
    Ok(Tray { refresher: TrayRefresher { thread_id }, done: done_rx })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::BudsState;

    fn snap(connected: bool) -> ConnectionSnapshot {
        ConnectionSnapshot {
            connected,
            connecting: false,
            device_name: "Galaxy Buds4 Pro".into(),
            device_address: None,
            status_text: String::new(),
            state: BudsState {
                mode: NoiseControlMode::Anc,
                battery_l: 80,
                battery_r: 75,
                placement_l: PlacementState::Wearing,
                placement_r: PlacementState::Disconnected,
                ..BudsState::default()
            },
        }
    }

    #[test]
    fn tooltip_shows_mode_and_known_batteries() {
        assert_eq!(tooltip(&snap(true)), "Galaxy Buds4 Pro — ANC · L 80%");
        assert_eq!(tooltip(&snap(false)), "Pods4 — Disconnected");
    }
}
