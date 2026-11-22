#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bluetooth;
mod protocol;

use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke, Vec2, ViewportCommand};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, HANDLE, HWND, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Registry::{RegGetValueA, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD};
use windows_sys::Win32::System::Threading::{
    CreateEventA, CreateMutexA, GetExitCodeProcess, SetEvent, WaitForSingleObject, INFINITE,
};
use windows_sys::Win32::UI::Shell::{
    ShellExecuteExW, ShellExecuteW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IsIconic, IsWindowVisible, MessageBoxW, PostMessageW, SetForegroundWindow, SetWindowPos,
    ShowWindow, HWND_NOTOPMOST, HWND_TOPMOST, MB_ICONERROR, MB_OK, SWP_NOMOVE, SWP_NOSIZE,
    SW_HIDE, SW_RESTORE, SW_SHOW, SW_SHOWNORMAL, WM_CLOSE,
};

use bluetooth::BudsClient;
use protocol::{
    set_ambient_during_calls, set_ambient_volume, set_anc_level, set_customize_ambient,
    set_gaming_mode, set_noise_control, set_one_earbud_noise_control, set_siren_detect,
    set_voice_detect, set_voice_detect_duration, BudsState, NoiseControlMode, PlacementState,
};

const MUTEX_NAME: &[u8] = b"Local\\Pods4_SingleInstance\0";
const SHOW_EVENT_NAME: &[u8] = b"Local\\Pods4_ShowWindow\0";

// One UI-inspired dark palette: true-black canvas, soft charcoal cards, Samsung blue accent.
const BG_MAIN: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
const BG_CARD: Color32 = Color32::from_rgb(0x1E, 0x1E, 0x1E);
const BG_INSET: Color32 = Color32::from_rgb(0x2C, 0x2C, 0x2E);
const DIVIDER: Color32 = Color32::from_rgb(0x38, 0x38, 0x3A);
const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xFA, 0xFA, 0xFA);
const TEXT_MUTED: Color32 = Color32::from_rgb(0xA0, 0xA0, 0xA5);
const TEXT_DIM: Color32 = Color32::from_rgb(0x70, 0x70, 0x75);
const ACCENT_BLUE: Color32 = Color32::from_rgb(0x38, 0x87, 0xF5);
const ACCENT_GREEN_HI: Color32 = Color32::from_rgb(0x4C, 0xD9, 0x64);
const ACCENT_AMBER: Color32 = Color32::from_rgb(0xFF, 0xB3, 0x40);
const ACCENT_RED: Color32 = Color32::from_rgb(0xFF, 0x45, 0x3A);

/// The buds push their full settings right after RFCOMM connects. If that packet never
/// arrives, unlock the controls after this long rather than leaving them disabled forever.
const INITIAL_SYNC_FALLBACK: Duration = Duration::from_secs(4);

const BOOST_LABELS: [&str; 3] = ["-1", "Normal", "+1"];
const TONE_LABELS: [&str; 5] = ["Soft +2", "Soft +1", "Neutral", "Clear +1", "Clear +2"];

struct SingleInstanceGuard {
    mutex: HANDLE,
    show_event: HANDLE,
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        unsafe {
            if !self.show_event.is_null() {
                SetEvent(self.show_event);
                CloseHandle(self.show_event);
            }
            if !self.mutex.is_null() { CloseHandle(self.mutex); }
        }
    }
}

fn acquire_single_instance() -> Option<SingleInstanceGuard> {
    unsafe {
        let mutex = CreateMutexA(null(), 1, MUTEX_NAME.as_ptr());
        let already = GetLastError() == ERROR_ALREADY_EXISTS;
        let show_event = CreateEventA(null(), 0, 0, SHOW_EVENT_NAME.as_ptr());
        if already {
            if !show_event.is_null() {
                SetEvent(show_event);
                CloseHandle(show_event);
            }
            if !mutex.is_null() {
                CloseHandle(mutex);
            }
            return None;
        }
        Some(SingleInstanceGuard { mutex, show_event })
    }
}

fn make_dot_icon(r: u8, g: u8, b: u8) -> Icon {
    const S: usize = 32;
    let mut rgba = vec![0u8; S * S * 4];
    let cx = 15.5_f32;
    let cy = 15.5_f32;
    for y in 0..S {
        for x in 0..S {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            let idx = (y * S + x) * 4;
            if dist <= 13.0 {
                let is_ring = (6.0..=8.4).contains(&dist);
                let (pr, pg, pb) = if is_ring { (255, 255, 255) } else { (r, g, b) };
                let alpha = if dist > 12.0 { ((13.0 - dist) * 255.0) as u8 } else { 255 };
                rgba[idx] = pr;
                rgba[idx + 1] = pg;
                rgba[idx + 2] = pb;
                rgba[idx + 3] = alpha;
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

struct TrayMenuIds {
    show: MenuId,
    ambient: MenuId,
    anc: MenuId,
    adaptive: MenuId,
    off: MenuId,
    reconnect: MenuId,
    exit: MenuId,
}

fn build_tray() -> Result<(TrayIcon, TrayMenuIds), String> {
    let menu = Menu::new();
    let show = MenuItem::new("Show Controls", true, None);
    let ambient = MenuItem::new("Ambient Sound (Passthrough)", true, None);
    let anc = MenuItem::new("Active Noise Cancelling", true, None);
    let adaptive = MenuItem::new("Adaptive", true, None);
    let off = MenuItem::new("Off", true, None);
    let reconnect = MenuItem::new("Reconnect", true, None);
    let exit = MenuItem::new("Exit", true, None);

    let ids = TrayMenuIds {
        show: show.id().clone(),
        ambient: ambient.id().clone(),
        anc: anc.id().clone(),
        adaptive: adaptive.id().clone(),
        off: off.id().clone(),
        reconnect: reconnect.id().clone(),
        exit: exit.id().clone(),
    };

    let _ = menu.append_items(&[
        &show,
        &PredefinedMenuItem::separator(),
        &ambient,
        &anc,
        &adaptive,
        &off,
        &PredefinedMenuItem::separator(),
        &reconnect,
        &exit,
    ]);

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("Pods4 — Passthrough & Noise Controls")
        .with_icon(make_dot_icon(0x64, 0x74, 0x8B))
        .build()
        .map_err(|e| format!("Failed to create system tray icon: {e}"))?;

    Ok((tray, ids))
}

fn show_error_box(msg: &str) {
    let title = wide("Pods4 — Startup Error");
    let body = wide(msg);
    unsafe {
        MessageBoxW(null_mut(), body.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR);
    }
}

fn show_hwnd(hwnd: isize, ctx: &egui::Context) {
    if hwnd == 0 {
        return;
    }
    let h = hwnd as HWND;
    unsafe {
        ShowWindow(h, SW_SHOW);
        if IsIconic(h) != 0 {
            ShowWindow(h, SW_RESTORE);
        }
        SetWindowPos(h, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
        SetWindowPos(h, HWND_NOTOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
        SetForegroundWindow(h);
    }
    ctx.request_repaint();
}

fn hide_hwnd(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    unsafe {
        ShowWindow(hwnd as HWND, SW_HIDE);
    }
}

fn is_hwnd_showing(hwnd: isize) -> bool {
    if hwnd == 0 {
        return false;
    }
    let h = hwnd as HWND;
    unsafe { IsWindowVisible(h) != 0 && IsIconic(h) == 0 }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Returns true when Windows Bluetooth A2DP AAC is disabled (forcing lower-latency SBC).
fn read_sbc_forced() -> bool {
    let mut val: u32 = 1;
    let mut size: u32 = size_of::<u32>() as u32;
    let rc = unsafe {
        RegGetValueA(
            HKEY_LOCAL_MACHINE,
            c"SYSTEM\\CurrentControlSet\\Services\\BthA2dp\\Parameters".as_ptr().cast(),
            c"BluetoothAacEnable".as_ptr().cast(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&mut val as *mut u32).cast::<c_void>(),
            &mut size,
        )
    };
    rc == ERROR_SUCCESS && val == 0
}

fn open_sound_settings() {
    let verb = wide("open");
    let uri = wide("ms-settings:sound");
    unsafe {
        ShellExecuteW(null_mut(), verb.as_ptr(), uri.as_ptr(), null(), null(), SW_SHOWNORMAL);
    }
}

struct CodecState {
    force_sbc: bool,
    busy: bool,
    note: String,
}

fn system32_exe(name: &str) -> String {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    format!(r"{root}\System32\{name}")
}

/// Picks the A2DP sink instance ID belonging to one of `addresses` (in priority order) from
/// `pnputil /enum-devices` output. Several headsets can be connected at once, so the Bluetooth
/// address embedded in the ID (`...&0&5CD33D4AA290_C00000000`) must match, or we'd restart the
/// wrong device.
fn pick_a2dp_instance_id(pnputil_output: &str, addresses: &[u64]) -> Option<String> {
    const PREFIX: &str = r"bthenum\{0000110b-0000-1000-8000-00805f9b34fb}_";
    let ids: Vec<&str> = pnputil_output
        .lines()
        .filter_map(|line| {
            let pos = line.to_ascii_lowercase().find(PREFIX)?;
            let id = line[pos..].trim();
            // The ID is pasted into an elevated cmd line: refuse anything that could escape quoting.
            let safe = !id.is_empty() && !id.chars().any(|c| matches!(c, '"' | '%' | '!' | '^' | '\r' | '\n'));
            safe.then_some(id)
        })
        .collect();
    addresses.iter().find_map(|&addr| {
        let needle = format!("&{addr:012x}_");
        ids.iter().find(|id| id.to_ascii_lowercase().contains(&needle)).map(|id| id.to_string())
    })
}

fn find_a2dp_instance_id(addresses: &[u64]) -> Option<String> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    if addresses.is_empty() {
        return None;
    }
    let out = Command::new(system32_exe("pnputil.exe"))
        .args(["/enum-devices", "/connected"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    pick_a2dp_instance_id(&String::from_utf8_lossy(&out.stdout), addresses)
}

/// Builds the `cmd.exe` parameters. The whole command is wrapped in one extra pair of quotes:
/// with more than two quotes, `cmd /c` strips the first and last one, which would otherwise
/// mangle the quoted `reg.exe` path and fail with "The system cannot find the path specified".
fn sbc_command_line(reg_exe: &str, pnputil_exe: &str, aac_val: u32, instance_id: Option<&str>) -> String {
    let reg = format!(
        "\"{reg_exe}\" add \"HKLM\\SYSTEM\\CurrentControlSet\\Services\\BthA2dp\\Parameters\" /v BluetoothAacEnable /t REG_DWORD /d {aac_val} /f"
    );
    match instance_id {
        Some(id) => format!("/c \"{reg} && \"{pnputil_exe}\" /restart-device \"{id}\"\""),
        None => format!("/c \"{reg}\""),
    }
}

fn apply_sbc_forced_async(
    force_sbc: bool,
    codec: Arc<Mutex<CodecState>>,
    client: BudsClient,
    ctx: egui::Context,
) {
    {
        let mut st = codec.lock().unwrap();
        if st.busy {
            return;
        }
        st.busy = true;
        st.note = "Waiting for Windows admin permission...".into();
    }
    ctx.request_repaint();

    thread::spawn(move || {
        let aac_val = if force_sbc { 0 } else { 1 };
        // Prefer the buds we're (or were last) connected to, then any paired buds.
        let mut addresses: Vec<u64> = client.snapshot().device_address.into_iter().collect();
        for d in bluetooth::rank_candidates(bluetooth::find_paired_devices()) {
            if !addresses.contains(&d.address) {
                addresses.push(d.address);
            }
        }
        let instance_id = find_a2dp_instance_id(&addresses);
        let cmd = sbc_command_line(
            &system32_exe("reg.exe"),
            &system32_exe("pnputil.exe"),
            aac_val,
            instance_id.as_deref(),
        );
        let verb = wide("runas");
        let file = wide(&system32_exe("cmd.exe"));
        let params = wide(&cmd);

        let mut sei: SHELLEXECUTEINFOW = unsafe { zeroed() };
        sei.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        sei.fMask = SEE_MASK_NOCLOSEPROCESS;
        sei.lpVerb = verb.as_ptr();
        sei.lpFile = file.as_ptr();
        sei.lpParameters = params.as_ptr();
        sei.nShow = SW_HIDE;

        let ok = unsafe { ShellExecuteExW(&mut sei) != 0 };
        let mut wait_rc = u32::MAX;
        let mut exit_code = 1u32;
        if ok && !sei.hProcess.is_null() {
            unsafe {
                wait_rc = WaitForSingleObject(sei.hProcess, 60_000);
                if wait_rc == WAIT_OBJECT_0 {
                    let _ = GetExitCodeProcess(sei.hProcess, &mut exit_code);
                }
                CloseHandle(sei.hProcess);
            }
        }

        let actual = read_sbc_forced();
        let restarted = instance_id.is_some() && ok && wait_rc == WAIT_OBJECT_0 && exit_code == 0;
        {
            let mut st = codec.lock().unwrap();
            st.force_sbc = actual;
            st.busy = false;
            st.note = if !ok {
                "Permission canceled — codec unchanged.".into()
            } else if wait_rc != WAIT_OBJECT_0 {
                "Timed out waiting for Windows codec update.".into()
            } else if actual != force_sbc {
                "Registry update failed — try running as administrator.".into()
            } else if exit_code != 0 {
                "Registry updated, but A2DP restart failed — reconnect Buds or reboot.".into()
            } else if restarted {
                if actual {
                    "SBC active (A2DP restarted). Reboot PC once if delay doesn't drop.".into()
                } else {
                    "AAC restored (A2DP restarted).".into()
                }
            } else if actual {
                "SBC active (reconnect Buds to apply).".into()
            } else {
                "AAC restored (reconnect Buds to apply).".into()
            };
        }
        ctx.request_repaint();

        if restarted {
            thread::sleep(std::time::Duration::from_millis(800));
            client.connect_auto_async();
        }
    });
}

struct Pods4App {
    client: BudsClient,
    ui_state: BudsState,
    last_mode: NoiseControlMode,
    last_connected: bool,
    slider_dragging: bool,
    /// When the current connection came up; drives the initial-sync fallback.
    connected_since: Option<Instant>,
    codec: Arc<Mutex<CodecState>>,
    tray: TrayIcon,
    hwnd: isize,
    exiting: Arc<AtomicBool>,
}

impl Pods4App {
    fn new(
        cc: &eframe::CreationContext<'_>,
        client: BudsClient,
        show_event: HANDLE,
    ) -> Result<Self, String> {
        let hwnd: isize = cc
            .window_handle()
            .ok()
            .and_then(|h| match h.as_raw() {
                RawWindowHandle::Win32(w) => Some(w.hwnd.get()),
                _ => None,
            })
            .unwrap_or(0);

        // Dark theme styling
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = BG_MAIN;
        visuals.window_fill = BG_MAIN;
        visuals.widgets.noninteractive.bg_fill = BG_CARD;
        visuals.widgets.inactive.bg_fill = BG_INSET;
        visuals.widgets.inactive.weak_bg_fill = BG_INSET;
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(0x3A, 0x3A, 0x3C);
        visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x3A, 0x3A, 0x3C);
        visuals.widgets.active.bg_fill = Color32::from_rgb(0x48, 0x48, 0x4A);
        visuals.selection.bg_fill = ACCENT_BLUE;
        visuals.slider_trailing_fill = true;
        cc.egui_ctx.set_visuals(visuals);

        let (tray, menu_ids) = build_tray()?;
        let exiting = Arc::new(AtomicBool::new(false));

        // Repaint UI whenever Bluetooth state changes
        let ctx_bt = cc.egui_ctx.clone();
        client.set_on_update(Arc::new(move || ctx_bt.request_repaint()));

        // Watch second-instance wake signal (directly calls Win32 ShowWindow so it works while hidden)
        if !show_event.is_null() {
            let ctx_wake = cc.egui_ctx.clone();
            let client_wake = client.clone();
            let exit_wake = exiting.clone();
            let show_event_addr = show_event as usize;
            thread::spawn(move || {
                let h = show_event_addr as HANDLE;
                loop {
                    let rc = unsafe { WaitForSingleObject(h, INFINITE) };
                    if rc != WAIT_OBJECT_0 || exit_wake.load(Ordering::SeqCst) {
                        break;
                    }
                    show_hwnd(hwnd, &ctx_wake);
                    if !client_wake.is_connected() {
                        client_wake.connect_auto_async();
                    }
                }
            });
        }

        // Watch tray clicks & menu actions (works reliably even when main window is hidden)
        let ctx_tray = cc.egui_ctx.clone();
        let exit_tray = exiting.clone();
        let client_tray = client.clone();
        thread::spawn(move || loop {
            let mut handled = false;
            while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
                handled = true;
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = ev
                {
                    if is_hwnd_showing(hwnd) {
                        hide_hwnd(hwnd);
                    } else {
                        show_hwnd(hwnd, &ctx_tray);
                        if !client_tray.is_connected() {
                            client_tray.connect_auto_async();
                        }
                    }
                }
            }

            while let Ok(ev) = MenuEvent::receiver().try_recv() {
                handled = true;
                if ev.id == menu_ids.show {
                    show_hwnd(hwnd, &ctx_tray);
                    if !client_tray.is_connected() {
                        client_tray.connect_auto_async();
                    }
                } else if let Some(mode) = [
                    (&menu_ids.ambient, NoiseControlMode::Ambient),
                    (&menu_ids.anc, NoiseControlMode::Anc),
                    (&menu_ids.adaptive, NoiseControlMode::Adaptive),
                    (&menu_ids.off, NoiseControlMode::Off),
                ]
                .into_iter()
                .find_map(|(id, mode)| (ev.id == *id).then_some(mode))
                {
                    client_tray.send_with(set_noise_control(mode), |st| st.mode = mode);
                } else if ev.id == menu_ids.reconnect {
                    client_tray.connect_auto_async();
                } else if ev.id == menu_ids.exit {
                    exit_tray.store(true, Ordering::SeqCst);
                    client_tray.disconnect();
                    if hwnd != 0 {
                        unsafe { PostMessageW(hwnd as HWND, WM_CLOSE, 0, 0); }
                    }
                    ctx_tray.send_viewport_cmd(ViewportCommand::Close);
                    ctx_tray.request_repaint();
                    return;
                }
            }

            thread::sleep(std::time::Duration::from_millis(if handled { 15 } else { 60 }));
        });

        client.connect_auto_async();

        Ok(Self {
            client,
            ui_state: BudsState::default(),
            last_mode: NoiseControlMode::Off,
            last_connected: false,
            slider_dragging: false,
            connected_since: None,
            codec: Arc::new(Mutex::new(CodecState {
                force_sbc: read_sbc_forced(),
                busy: false,
                note: String::new(),
            })),
            tray,
            hwnd,
            exiting,
        })
    }
}

impl eframe::App for Pods4App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Intercept window X button -> hide to system tray instead of exiting
        if ctx.input(|i| i.viewport().close_requested()) && !self.exiting.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            hide_hwnd(self.hwnd);
            return;
        }

        let snap = self.client.snapshot();
        if !self.slider_dragging {
            self.ui_state = snap.state.clone();
        }
        let mut dragging_now = false;

        // Don't offer controls until the buds have reported their real settings, otherwise the
        // defaults in BudsState would be shown (and could be sent) as if they were current.
        let connected_for = if snap.connected {
            Some(self.connected_since.get_or_insert_with(Instant::now).elapsed())
        } else {
            self.connected_since = None;
            None
        };
        let synced = match connected_for {
            Some(_) if self.ui_state.has_initial_sync => true,
            Some(t) if t >= INITIAL_SYNC_FALLBACK => true,
            Some(t) => {
                ctx.request_repaint_after(INITIAL_SYNC_FALLBACK - t);
                false
            }
            None => false,
        };
        let status_text = if snap.connected && !synced {
            format!("{} — reading settings...", snap.status_text)
        } else {
            snap.status_text.clone()
        };

        if self.ui_state.mode != self.last_mode || snap.connected != self.last_connected {
            self.last_mode = self.ui_state.mode;
            self.last_connected = snap.connected;
            let (r, g, b) = if snap.connected {
                mode_rgb(self.last_mode)
            } else {
                (0x64, 0x74, 0x8B)
            };
            let tip = if snap.connected {
                format!("{} — {}", snap.device_name, self.last_mode.label())
            } else {
                format!("{} — Disconnected", snap.device_name)
            };
            let _ = self.tray.set_icon(Some(make_dot_icon(r, g, b)));
            let _ = self.tray.set_tooltip(Some(tip));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BG_MAIN).inner_margin(Margin::symmetric(16, 10)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // 1. Header: large One UI style title + connection status
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.set_max_width((ui.available_width() - 105.0).max(160.0));
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&snap.device_name).size(27.0).strong().color(TEXT_PRIMARY),
                                )
                                .truncate(),
                            );
                            ui.horizontal(|ui| {
                                let dot_color = if snap.connected {
                                    ACCENT_GREEN_HI
                                } else if snap.connecting {
                                    ACCENT_AMBER
                                } else {
                                    ACCENT_RED
                                };
                                let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                                ui.painter().circle_filled(rect.center(), 4.0, dot_color);
                                ui.label(RichText::new(&status_text).size(12.5).color(TEXT_MUTED));
                            });
                        });

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let btn = egui::Button::new(RichText::new("Reconnect").size(12.5).strong().color(ACCENT_BLUE))
                                .fill(BG_CARD)
                                .corner_radius(CornerRadius::same(16))
                                .min_size(Vec2::new(0.0, 32.0));
                            if ui.add_enabled(!snap.connecting, btn).clicked() {
                                self.client.connect_auto_async();
                            }
                        });
                    });
                    ui.add_space(16.0);

                    // 2. Battery card
                    ui.add_enabled_ui(snap.connected, |ui| {
                        card(ui, |ui| {
                            let s = &self.ui_state;
                            let l_pct = (snap.connected && s.placement_l != PlacementState::Disconnected).then_some(s.battery_l);
                            let r_pct = (snap.connected && s.placement_r != PlacementState::Disconnected).then_some(s.battery_r);
                            let c_pct = (snap.connected && (1..=100).contains(&s.battery_case)).then_some(s.battery_case);
                            let l_sub = if snap.connected { s.placement_l.label() } else { "Offline" };
                            let r_sub = if snap.connected { s.placement_r.label() } else { "Offline" };
                            let c_sub = if !snap.connected {
                                "Offline"
                            } else if s.is_case_charging {
                                "Charging"
                            } else if c_pct.is_some() {
                                "Ready"
                            } else {
                                "Closed"
                            };

                            ui.columns(3, |cols| {
                                battery_badge(&mut cols[0], "Left", l_pct, l_sub);
                                battery_badge(&mut cols[1], "Right", r_pct, r_sub);
                                battery_badge(&mut cols[2], "Case", c_pct, c_sub);
                            });
                        });
                    });

                    ui.add_enabled_ui(synced, |ui| {
                        // 3. Noise controls
                        card(ui, |ui| {
                            section_title(ui, "Noise controls");
                            let modes = [
                                (NoiseControlMode::Off, "Off"),
                                (NoiseControlMode::Anc, "ANC"),
                                (NoiseControlMode::Ambient, "Ambient"),
                                (NoiseControlMode::Adaptive, "Adaptive"),
                            ];
                            ui.columns(4, |cols| {
                                for (i, (mode, label)) in modes.into_iter().enumerate() {
                                    let active = self.ui_state.mode == mode;
                                    if pill_button(&mut cols[i], label, active, 40.0, 13.0).clicked() && !active {
                                        self.client.send_with(set_noise_control(mode), |st| st.mode = mode);
                                    }
                                }
                            });

                            ui.add_space(10.0);
                            let desc = match self.ui_state.mode {
                                NoiseControlMode::Off => "Noise controls are off.",
                                NoiseControlMode::Anc => "Block out background noise around you.",
                                NoiseControlMode::Ambient => "Hear your surroundings while wearing your buds.",
                                NoiseControlMode::Adaptive => "Automatically adjusts to the noise around you.",
                            };
                            ui.label(RichText::new(desc).size(12.0).color(TEXT_MUTED));

                            let one_bud_worn = (self.ui_state.placement_l == PlacementState::Wearing)
                                ^ (self.ui_state.placement_r == PlacementState::Wearing);
                            if one_bud_worn && !self.ui_state.one_earbud_noise_control {
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new("Only 1 earbud is in your ear. Turn on 'Noise controls with one earbud' below to use ANC or Adaptive.")
                                        .size(11.5)
                                        .color(ACCENT_AMBER),
                                );
                            }
                        });

                        // 4. Levels
                        card(ui, |ui| {
                            section_title(ui, "Levels");

                            let amb_vol = &mut self.ui_state.ambient_volume;
                            slider_header(ui, "Ambient sound volume", &format!("Level {}", *amb_vol + 1));
                            let r_amb = full_width_slider(ui, amb_vol, 0..=4);
                            dragging_now |= r_amb.dragged();
                            if r_amb.drag_stopped() || (r_amb.changed() && !r_amb.dragged()) {
                                let v = *amb_vol;
                                self.client.send_with(set_ambient_volume(v), |st| st.ambient_volume = v);
                            }

                            ui.add_space(10.0);

                            let anc_lvl = &mut self.ui_state.anc_level;
                            slider_header(ui, "Noise cancelling level", &format!("Level {}", *anc_lvl + 1));
                            let r_anc = full_width_slider(ui, anc_lvl, 0..=4);
                            dragging_now |= r_anc.dragged();
                            if r_anc.drag_stopped() || (r_anc.changed() && !r_anc.dragged()) {
                                let v = *anc_lvl;
                                self.client.send_with(set_anc_level(v), |st| st.anc_level = v);
                            }
                        });

                        // 5. Customize ambient sound
                        card(ui, |ui| {
                            let s = &mut self.ui_state;
                            let mut send_custom = false;

                            if toggle_row(ui, "Customize ambient sound", "Per-ear boost and sound tone", &mut s.custom_ambient_on) {
                                send_custom = true;
                            }

                            ui.add_space(8.0);
                            ui.add_enabled_ui(s.custom_ambient_on, |ui| {
                                slider_header(ui, "Left ear", BOOST_LABELS[s.custom_ambient_left.min(2) as usize]);
                                let r_l = full_width_slider(ui, &mut s.custom_ambient_left, 0..=2);
                                dragging_now |= r_l.dragged();
                                if r_l.drag_stopped() || (r_l.changed() && !r_l.dragged()) { send_custom = true; }
                                ui.add_space(6.0);

                                slider_header(ui, "Right ear", BOOST_LABELS[s.custom_ambient_right.min(2) as usize]);
                                let r_r = full_width_slider(ui, &mut s.custom_ambient_right, 0..=2);
                                dragging_now |= r_r.dragged();
                                if r_r.drag_stopped() || (r_r.changed() && !r_r.dragged()) { send_custom = true; }
                                ui.add_space(6.0);

                                slider_header(ui, "Ambient tone", TONE_LABELS[s.custom_ambient_tone.min(4) as usize]);
                                let r_t = full_width_slider(ui, &mut s.custom_ambient_tone, 0..=4);
                                dragging_now |= r_t.dragged();
                                if r_t.drag_stopped() || (r_t.changed() && !r_t.dragged()) { send_custom = true; }
                            });

                            if send_custom {
                                let (on, l, r, t) = (
                                    s.custom_ambient_on,
                                    s.custom_ambient_left,
                                    s.custom_ambient_right,
                                    s.custom_ambient_tone,
                                );
                                self.client.send_with(set_customize_ambient(on, l, r, t), |st| {
                                    st.custom_ambient_on = on;
                                    st.custom_ambient_left = l;
                                    st.custom_ambient_right = r;
                                    st.custom_ambient_tone = t;
                                });
                            }
                        });

                        // 6. Auto switch & options
                        card(ui, |ui| {
                            section_title(ui, "Auto switch & options");

                            let mut vd = self.ui_state.voice_detect;
                            if toggle_row(ui, "Voice detect", "Switch to Ambient sound when you speak", &mut vd) {
                                self.client.send_with(set_voice_detect(vd), |st| st.voice_detect = vd);
                            }

                            ui.add_space(8.0);
                            ui.add_enabled_ui(vd, |ui| {
                                ui.columns(3, |cols| {
                                    for (i, label) in ["5 sec", "10 sec", "15 sec"].into_iter().enumerate() {
                                        let idx = i as u8;
                                        let active = self.ui_state.voice_detect_duration == idx;
                                        if pill_button(&mut cols[i], label, active, 30.0, 12.0).clicked() && !active {
                                            self.client.send_with(set_voice_detect_duration(idx), |st| {
                                                st.voice_detect_duration = idx
                                            });
                                        }
                                    }
                                });
                            });

                            divider(ui);
                            let mut siren = self.ui_state.siren_detect;
                            if toggle_row(ui, "Siren detect", "Switch to Ambient sound on alarms and sirens", &mut siren) {
                                self.client.send_with(set_siren_detect(siren), |st| st.siren_detect = siren);
                            }

                            divider(ui);
                            let mut sidetone = self.ui_state.ambient_during_calls;
                            if toggle_row(ui, "Ambient sound during calls", "Hear your own voice naturally", &mut sidetone) {
                                self.client.send_with(set_ambient_during_calls(sidetone), |st| {
                                    st.ambient_during_calls = sidetone
                                });
                            }

                            divider(ui);
                            let mut one_bud = self.ui_state.one_earbud_noise_control;
                            if toggle_row(ui, "Noise controls with one earbud", "Allow ANC and Adaptive with a single bud", &mut one_bud) {
                                self.client.send_with(set_one_earbud_noise_control(one_bud), |st| {
                                    st.one_earbud_noise_control = one_bud
                                });
                            }
                        });
                    });

                    // 7. Low latency (Windows SBC toggle works even while disconnected)
                    card(ui, |ui| {
                        section_title(ui, "Low latency & sync");

                        let (mut force_sbc, busy, note) = {
                            let c = self.codec.lock().unwrap();
                            (c.force_sbc, c.busy, c.note.clone())
                        };
                        ui.add_enabled_ui(!busy, |ui| {
                            if toggle_row(
                                ui,
                                "Force SBC codec (disable AAC)",
                                "Cuts Windows Bluetooth lag by ~100–150 ms; asks for admin once",
                                &mut force_sbc,
                            ) {
                                apply_sbc_forced_async(
                                    force_sbc,
                                    self.codec.clone(),
                                    self.client.clone(),
                                    ctx.clone(),
                                );
                            }
                        });
                        if !note.is_empty() {
                            ui.add_space(4.0);
                            ui.label(RichText::new(note).size(11.5).color(ACCENT_AMBER));
                        }

                        divider(ui);
                        ui.add_enabled_ui(synced, |ui| {
                            let mut gm = self.ui_state.gaming_mode;
                            if toggle_row(
                                ui,
                                "Earbud gaming mode",
                                "Samsung low-latency sync flag (small effect on Windows)",
                                &mut gm,
                            ) {
                                self.client.send_with(set_gaming_mode(gm), |st| st.gaming_mode = gm);
                            }
                        });

                        divider(ui);
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new("Audio output & enhancements").size(14.0).color(TEXT_PRIMARY));
                                ui.label(RichText::new("Bypass Voicemeeter and turn Enhancements off").size(11.5).color(TEXT_MUTED));
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let btn = egui::Button::new(RichText::new("Open").size(12.5).strong().color(ACCENT_BLUE))
                                    .fill(BG_INSET)
                                    .corner_radius(CornerRadius::same(16))
                                    .min_size(Vec2::new(0.0, 32.0));
                                if ui.add(btn).clicked() {
                                    open_sound_settings();
                                }
                            });
                        });
                    });
                    ui.add_space(6.0);
                });
            });
        self.slider_dragging = dragging_now;
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.exiting.store(true, Ordering::SeqCst);
        self.client.disconnect();
    }
}

fn card(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE
        .fill(BG_CARD)
        .corner_radius(CornerRadius::same(24))
        .inner_margin(Margin::same(18))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui);
        });
    ui.add_space(12.0);
}

fn section_title(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).size(13.5).strong().color(ACCENT_BLUE));
    ui.add_space(10.0);
}

fn divider(ui: &mut egui::Ui) {
    ui.add_space(10.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, DIVIDER);
    ui.add_space(10.0);
}

fn battery_badge(ui: &mut egui::Ui, title: &str, pct: Option<u8>, sub: &str) {
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(title).size(12.0).color(TEXT_MUTED));
        let value = pct.map_or("—".to_string(), |p| format!("{p}%"));
        ui.label(RichText::new(value).size(24.0).strong().color(TEXT_PRIMARY));

        // Thin battery bar
        let w = (ui.available_width() - 16.0).clamp(20.0, 90.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 5.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, CornerRadius::same(3), BG_INSET);
        if let Some(p) = pct {
            let fill_w = rect.width() * (p.min(100) as f32 / 100.0);
            let color = match p {
                0..=15 => ACCENT_RED,
                16..=30 => ACCENT_AMBER,
                _ => ACCENT_GREEN_HI,
            };
            let fill = egui::Rect::from_min_size(rect.min, Vec2::new(fill_w, rect.height()));
            ui.painter().rect_filled(fill, CornerRadius::same(3), color);
        }
        ui.add_space(2.0);
        ui.label(RichText::new(sub).size(11.0).color(TEXT_DIM));
    });
}

/// Rounded One UI style choice button (filled blue when selected).
fn pill_button(ui: &mut egui::Ui, label: &str, active: bool, height: f32, size: f32) -> egui::Response {
    let btn = egui::Button::new(
        RichText::new(label)
            .size(size)
            .strong()
            .color(if active { Color32::WHITE } else { TEXT_MUTED }),
    )
    .fill(if active { ACCENT_BLUE } else { BG_INSET })
    .stroke(Stroke::NONE)
    .corner_radius(CornerRadius::same((height / 2.0) as u8))
    .min_size(Vec2::new(ui.available_width(), height));
    ui.add(btn)
}

fn slider_header(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(13.5).color(TEXT_PRIMARY));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(value).size(12.5).strong().color(ACCENT_BLUE));
        });
    });
}

fn full_width_slider(ui: &mut egui::Ui, val: &mut u8, range: std::ops::RangeInclusive<u8>) -> egui::Response {
    ui.spacing_mut().slider_width = (ui.available_width() - 8.0).max(80.0);
    ui.add(egui::Slider::new(val, range).show_value(false).trailing_fill(true))
}

fn toggle_row(ui: &mut egui::Ui, title: &str, subtitle: &str, on: &mut bool) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_max_width((ui.available_width() - 60.0).max(120.0));
            ui.label(RichText::new(title).size(14.0).color(TEXT_PRIMARY));
            ui.label(RichText::new(subtitle).size(11.5).color(TEXT_MUTED));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if pill_switch(ui, on).clicked() {
                clicked = true;
            }
        });
    });
    clicked
}

/// One UI style switch: blue track when on, grey when off, white thumb.
fn pill_switch(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let desired = Vec2::new(46.0, 26.0);
    let (rect, mut resp) = ui.allocate_exact_size(desired, egui::Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    let off_bg = Color32::from_rgb(0x58, 0x58, 0x5C);
    let bg = Color32::from_rgb(
        (off_bg.r() as f32 + (ACCENT_BLUE.r() as f32 - off_bg.r() as f32) * t) as u8,
        (off_bg.g() as f32 + (ACCENT_BLUE.g() as f32 - off_bg.g() as f32) * t) as u8,
        (off_bg.b() as f32 + (ACCENT_BLUE.b() as f32 - off_bg.b() as f32) * t) as u8,
    );
    let r = rect.height() * 0.5;
    ui.painter().rect_filled(rect, CornerRadius::same(r as u8), bg);
    let cx = rect.left() + r + (rect.width() - 2.0 * r) * t;
    ui.painter().circle_filled(egui::pos2(cx, rect.center().y), r - 3.0, Color32::WHITE);
    resp
}

fn main() -> eframe::Result<()> {
    let Some(_guard) = acquire_single_instance() else {
        return Ok(());
    };
    let show_event = _guard.show_event;
    let client = BudsClient::new();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Pods4 — Passthrough & Noise Controls")
            .with_inner_size([430.0, 740.0])
            .with_min_inner_size([380.0, 560.0])
            .with_resizable(true),
        ..Default::default()
    };

    let res = eframe::run_native(
        "Pods4",
        options,
        Box::new(move |cc| Ok(Box::new(Pods4App::new(cc, client, show_event)?))),
    );
    if let Err(e) = &res {
        show_error_box(&format!("Pods4 failed to start:\n\n{e}"));
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed real `pnputil /enum-devices /connected` output with three A2DP sinks connected.
    const PNPUTIL: &str = r"
Instance ID:                BTHENUM\{0000110b-0000-1000-8000-00805f9b34fb}_VID&000105d6_PID&000a\7&b44bb26&0&4CFE2E270C51_C00000000
Device Description:         JVC HA-A7T2
Instance ID:                BTHENUM\{0000110b-0000-1000-8000-00805f9b34fb}_VID&00010057_PID&0023\7&b44bb26&0&004279FDAFFE_C00000000
Device Description:         JBL Charge 3
Instance ID:
BTHENUM\{0000110b-0000-1000-8000-00805f9b34fb}_VID&00010075_PID&a013\7&b44bb26&0&5CD33D4AA290_C00000000
Device Description:         Galaxy Buds4 Pro
";

    #[test]
    fn picks_the_instance_matching_the_buds_address() {
        let id = pick_a2dp_instance_id(PNPUTIL, &[0x5CD3_3D4A_A290]).unwrap();
        assert!(id.ends_with("5CD33D4AA290_C00000000"), "{id}");
        assert!(id.starts_with(r"BTHENUM\{0000110b"), "{id}");
    }

    #[test]
    fn address_order_decides_and_unknown_addresses_match_nothing() {
        let id = pick_a2dp_instance_id(PNPUTIL, &[0x1234, 0x0042_79FD_AFFE, 0x5CD3_3D4A_A290]).unwrap();
        assert!(id.contains("004279FDAFFE"), "{id}");
        assert_eq!(pick_a2dp_instance_id(PNPUTIL, &[0xDEAD_BEEF]), None);
        assert_eq!(pick_a2dp_instance_id(PNPUTIL, &[]), None);
    }

    #[test]
    fn rejects_ids_that_could_break_cmd_quoting() {
        let evil = r#"Instance ID: BTHENUM\{0000110b-0000-1000-8000-00805f9b34fb}_x" & calc &0&5CD33D4AA290_C0"#;
        assert_eq!(pick_a2dp_instance_id(evil, &[0x5CD3_3D4A_A290]), None);
    }

    #[test]
    fn sbc_command_line_is_wrapped_in_outer_quotes() {
        let cmd = sbc_command_line(r"C:\W\reg.exe", r"C:\W\pnputil.exe", 0, Some(r"BTHENUM\X&0&AB_C0"));
        assert_eq!(
            cmd,
            r#"/c ""C:\W\reg.exe" add "HKLM\SYSTEM\CurrentControlSet\Services\BthA2dp\Parameters" /v BluetoothAacEnable /t REG_DWORD /d 0 /f && "C:\W\pnputil.exe" /restart-device "BTHENUM\X&0&AB_C0"""#
        );
        let no_restart = sbc_command_line(r"C:\W\reg.exe", r"C:\W\pnputil.exe", 1, None);
        assert!(no_restart.starts_with(r#"/c ""C:\W\reg.exe""#));
        assert!(no_restart.ends_with("/d 1 /f\""));
    }
}
