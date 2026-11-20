#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bluetooth;
mod protocol;

use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke, Vec2, ViewportCommand};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, HANDLE, HWND,
};
use windows_sys::Win32::System::Registry::{RegGetValueA, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD};
use windows_sys::Win32::System::Threading::{
    CreateEventA, CreateMutexA, SetEvent, WaitForSingleObject, INFINITE,
};
use windows_sys::Win32::UI::Shell::{
    ShellExecuteExW, ShellExecuteW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IsIconic, IsWindowVisible, PostMessageW, SetForegroundWindow, SetWindowPos, ShowWindow,
    HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_RESTORE, SW_SHOW,
    SW_SHOWNORMAL, WM_CLOSE,
};

use bluetooth::BudsClient;
use protocol::{
    set_ambient_during_calls, set_ambient_volume, set_anc_level, set_customize_ambient,
    set_gaming_mode, set_noise_control, set_one_earbud_noise_control, set_siren_detect,
    set_voice_detect, set_voice_detect_duration, BudsState, NoiseControlMode, PlacementState,
};

const MUTEX_NAME: &[u8] = b"Local\\Pods4_SingleInstance\0";
const SHOW_EVENT_NAME: &[u8] = b"Local\\Pods4_ShowWindow\0";

const BG_MAIN: Color32 = Color32::from_rgb(0x11, 0x14, 0x1B);
const BG_CARD: Color32 = Color32::from_rgb(0x1A, 0x1F, 0x29);
const BG_INSET: Color32 = Color32::from_rgb(0x13, 0x17, 0x20);
const BORDER_CARD: Color32 = Color32::from_rgb(0x28, 0x30, 0x40);
const TEXT_MUTED: Color32 = Color32::from_rgb(0x94, 0xA3, 0xB8);
const TEXT_DIM: Color32 = Color32::from_rgb(0x64, 0x74, 0x8B);
const ACCENT_BLUE: Color32 = Color32::from_rgb(0x25, 0x63, 0xEB);
const ACCENT_BLUE_HI: Color32 = Color32::from_rgb(0x60, 0xA5, 0xFA);
const ACCENT_GREEN: Color32 = Color32::from_rgb(0x05, 0x96, 0x69);
const ACCENT_GREEN_HI: Color32 = Color32::from_rgb(0x34, 0xD3, 0x99);
const ACCENT_AMBER: Color32 = Color32::from_rgb(0xFB, 0xBF, 0x24);

const BOOST_LABELS: [&str; 3] = ["-1", "Normal", "+1"];
const TONE_LABELS: [&str; 5] = ["Soft +2", "Soft +1", "Neutral", "Clear +1", "Clear +2"];

struct SingleInstanceGuard {
    mutex: HANDLE,
    show_event: HANDLE,
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        unsafe {
            if !self.show_event.is_null() { CloseHandle(self.show_event); }
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

fn build_tray() -> (TrayIcon, TrayMenuIds) {
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
        .expect("create tray icon");

    (tray, ids)
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
            b"SYSTEM\\CurrentControlSet\\Services\\BthA2dp\\Parameters\0".as_ptr(),
            b"BluetoothAacEnable\0".as_ptr(),
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
        let cmd = format!(
            "/c reg add \"HKLM\\SYSTEM\\CurrentControlSet\\Services\\BthA2dp\\Parameters\" /v BluetoothAacEnable /t REG_DWORD /d {aac_val} /f && pnputil /restart-device /deviceid \"BTHENUM\\{{0000110b-0000-1000-8000-00805f9b34fb}}\""
        );
        let verb = wide("runas");
        let file = wide("cmd.exe");
        let params = wide(&cmd);

        let mut sei: SHELLEXECUTEINFOW = unsafe { zeroed() };
        sei.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        sei.fMask = SEE_MASK_NOCLOSEPROCESS;
        sei.lpVerb = verb.as_ptr();
        sei.lpFile = file.as_ptr();
        sei.lpParameters = params.as_ptr();
        sei.nShow = SW_HIDE;

        let ok = unsafe { ShellExecuteExW(&mut sei) != 0 };
        if ok && !sei.hProcess.is_null() {
            unsafe {
                WaitForSingleObject(sei.hProcess, INFINITE);
                CloseHandle(sei.hProcess);
            }
        }

        let actual = read_sbc_forced();
        {
            let mut st = codec.lock().unwrap();
            st.force_sbc = actual;
            st.busy = false;
            st.note = if !ok {
                "Permission canceled — codec unchanged.".into()
            } else if actual == force_sbc {
                if actual {
                    "SBC active (A2DP restarted). Reboot PC once if delay doesn't drop.".into()
                } else {
                    "AAC restored (A2DP restarted).".into()
                }
            } else {
                "Registry update failed — try running as administrator.".into()
            };
        }
        ctx.request_repaint();

        if ok {
            thread::sleep(std::time::Duration::from_millis(800));
            client.connect_auto_async();
        }
    });
}

struct Pods4App {
    client: BudsClient,
    ui_state: BudsState,
    last_mode: NoiseControlMode,
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
    ) -> Self {
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
        visuals.widgets.inactive.bg_fill = Color32::from_rgb(0x23, 0x2B, 0x3A);
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(0x2E, 0x38, 0x4D);
        visuals.selection.bg_fill = ACCENT_BLUE;
        cc.egui_ctx.set_visuals(visuals);

        let (tray, menu_ids) = build_tray();
        let exiting = Arc::new(AtomicBool::new(false));

        // Repaint UI whenever Bluetooth state changes
        let ctx_bt = cc.egui_ctx.clone();
        client.set_on_update(Arc::new(move || ctx_bt.request_repaint()));

        // Watch second-instance wake signal (directly calls Win32 ShowWindow so it works while hidden)
        let ctx_wake = cc.egui_ctx.clone();
        let client_wake = client.clone();
        let show_event_addr = show_event as usize;
        thread::spawn(move || {
            let h = show_event_addr as HANDLE;
            loop {
                unsafe { WaitForSingleObject(h, INFINITE); }
                show_hwnd(hwnd, &ctx_wake);
                if !client_wake.is_connected() {
                    client_wake.connect_auto_async();
                }
            }
        });

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
                } else if ev.id == menu_ids.ambient {
                    client_tray.send_async(set_noise_control(NoiseControlMode::Ambient));
                } else if ev.id == menu_ids.anc {
                    client_tray.send_async(set_noise_control(NoiseControlMode::Anc));
                } else if ev.id == menu_ids.adaptive {
                    client_tray.send_async(set_noise_control(NoiseControlMode::Adaptive));
                } else if ev.id == menu_ids.off {
                    client_tray.send_async(set_noise_control(NoiseControlMode::Off));
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

        Self {
            client,
            ui_state: BudsState::default(),
            last_mode: NoiseControlMode::Off,
            codec: Arc::new(Mutex::new(CodecState {
                force_sbc: read_sbc_forced(),
                busy: false,
                note: String::new(),
            })),
            tray,
            hwnd,
            exiting,
        }
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
        self.ui_state = snap.state.clone();

        if self.ui_state.mode != self.last_mode {
            self.last_mode = self.ui_state.mode;
            let (r, g, b) = mode_rgb(self.last_mode);
            let _ = self.tray.set_icon(Some(make_dot_icon(r, g, b)));
            let _ = self.tray.set_tooltip(Some(format!("{} — {}", snap.device_name, self.last_mode.label())));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BG_MAIN).inner_margin(Margin::same(14)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // 1. Device & Battery Header Card
                    card(ui, |ui| {
                        ui.horizontal(|ui| {
                            let dot_color = if snap.connected {
                                ACCENT_GREEN_HI
                            } else if snap.connecting {
                                ACCENT_AMBER
                            } else {
                                Color32::from_rgb(0xEF, 0x44, 0x44)
                            };
                            let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                            ui.painter().circle_filled(rect.center(), 4.5, dot_color);

                            ui.vertical(|ui| {
                                ui.label(RichText::new(&snap.device_name).size(15.0).strong().color(Color32::WHITE));
                                ui.label(RichText::new(&snap.status_text).size(11.5).color(TEXT_MUTED));
                            });

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add_enabled(!snap.connecting, egui::Button::new("Reconnect")).clicked() {
                                    self.client.connect_auto_async();
                                }
                            });
                        });

                        ui.add_space(10.0);
                        ui.columns(3, |cols| {
                            let l_bat = if self.ui_state.placement_l == PlacementState::Disconnected {
                                "—".to_string()
                            } else {
                                format!("{}%", self.ui_state.battery_l)
                            };
                            let r_bat = if self.ui_state.placement_r == PlacementState::Disconnected {
                                "—".to_string()
                            } else {
                                format!("{}%", self.ui_state.battery_r)
                            };
                            let c_bat = if (1..=100).contains(&self.ui_state.battery_case) {
                                format!("{}%", self.ui_state.battery_case)
                            } else {
                                "—".to_string()
                            };
                            let c_sub = if self.ui_state.is_case_charging {
                                "Charging"
                            } else if (1..=100).contains(&self.ui_state.battery_case) {
                                "Ready"
                            } else {
                                "Closed"
                            };

                            battery_badge(&mut cols[0], "LEFT", &l_bat, self.ui_state.placement_l.label());
                            battery_badge(&mut cols[1], "RIGHT", &r_bat, self.ui_state.placement_r.label());
                            battery_badge(&mut cols[2], "CASE", &c_bat, c_sub);
                        });
                    });

                    ui.add_enabled_ui(snap.connected, |ui| {
                        // 2. Noise Controls Card
                        card(ui, |ui| {
                            section_title(ui, "NOISE CONTROLS");
                            let modes = [
                                (NoiseControlMode::Off, "Off", false),
                                (NoiseControlMode::Anc, "ANC", false),
                                (NoiseControlMode::Ambient, "Ambient", true),
                                (NoiseControlMode::Adaptive, "Adaptive", false),
                            ];
                            ui.columns(4, |cols| {
                                for (i, (mode, label, is_ambient)) in modes.into_iter().enumerate() {
                                    let active = self.ui_state.mode == mode;
                                    let fill = if active {
                                        if is_ambient { ACCENT_GREEN } else { ACCENT_BLUE }
                                    } else {
                                        BG_INSET
                                    };
                                    let border = if active {
                                        if is_ambient { ACCENT_GREEN_HI } else { ACCENT_BLUE_HI }
                                    } else {
                                        BORDER_CARD
                                    };
                                    let btn = egui::Button::new(
                                        RichText::new(label)
                                            .size(12.5)
                                            .strong()
                                            .color(if active { Color32::WHITE } else { Color32::from_rgb(0xCB, 0xD5, 0xE1) }),
                                    )
                                    .fill(fill)
                                    .stroke(Stroke::new(1.0_f32, border))
                                    .corner_radius(CornerRadius::same(8))
                                    .min_size(Vec2::new(cols[i].available_width(), 34.0));

                                    if cols[i].add(btn).clicked() && !active {
                                        self.client.send_async(set_noise_control(mode));
                                    }
                                }
                            });

                            let one_bud_worn = (self.ui_state.placement_l == PlacementState::Wearing)
                                ^ (self.ui_state.placement_r == PlacementState::Wearing);
                            if one_bud_worn && !self.ui_state.one_earbud_noise_control {
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new("Tip: Only 1 earbud is in ear — enable 'Noise Controls with One Earbud' below to use ANC/Adaptive with one bud.")
                                        .size(11.0)
                                        .color(ACCENT_AMBER),
                                );
                            }
                        });

                        // 3. Passthrough & ANC Levels Card
                        card(ui, |ui| {
                            section_title(ui, "PASSTHROUGH & ANC LEVELS");

                            let mut amb_vol = self.ui_state.ambient_volume;
                            slider_header(ui, "Ambient Sound Volume (Passthrough)", &format!("Level {}", amb_vol + 1), ACCENT_GREEN_HI);
                            if full_width_slider(ui, &mut amb_vol, 0..=4) {
                                self.client.send_async(set_ambient_volume(amb_vol));
                            }

                            ui.add_space(8.0);

                            let mut anc_lvl = self.ui_state.anc_level;
                            slider_header(ui, "Active Noise Cancelling Level", &format!("Level {}", anc_lvl + 1), ACCENT_BLUE_HI);
                            if full_width_slider(ui, &mut anc_lvl, 0..=4) {
                                self.client.send_async(set_anc_level(anc_lvl));
                            }
                        });

                        // 4. Customize Ambient Sound Card
                        card(ui, |ui| {
                            let mut on = self.ui_state.custom_ambient_on;
                            let mut left = self.ui_state.custom_ambient_left;
                            let mut right = self.ui_state.custom_ambient_right;
                            let mut tone = self.ui_state.custom_ambient_tone;
                            let mut changed = false;

                            if toggle_row(ui, "Customize Ambient Sound", "Per-ear passthrough boost & sound tone", &mut on) {
                                changed = true;
                            }

                            ui.add_space(6.0);
                            ui.add_enabled_ui(on, |ui| {
                                slider_header(ui, "Left Ear Boost", BOOST_LABELS[left.min(2) as usize], ACCENT_GREEN_HI);
                                if full_width_slider(ui, &mut left, 0..=2) { changed = true; }
                                ui.add_space(4.0);

                                slider_header(ui, "Right Ear Boost", BOOST_LABELS[right.min(2) as usize], ACCENT_GREEN_HI);
                                if full_width_slider(ui, &mut right, 0..=2) { changed = true; }
                                ui.add_space(4.0);

                                slider_header(ui, "Ambient Tone (Soft <-> Clear)", TONE_LABELS[tone.min(4) as usize], ACCENT_GREEN_HI);
                                if full_width_slider(ui, &mut tone, 0..=4) { changed = true; }
                            });

                            if changed {
                                self.client.send_async(set_customize_ambient(on, left, right, tone));
                            }
                        });

                        // 5. Auto Switch & Passthrough Options Card
                        card(ui, |ui| {
                            section_title(ui, "AUTO SWITCH & PASSTHROUGH OPTIONS");

                            let mut vd = self.ui_state.voice_detect;
                            if toggle_row(ui, "Voice Detect", "Auto-switch to Ambient Sound when you speak", &mut vd) {
                                self.client.send_async(set_voice_detect(vd));
                            }

                            ui.add_space(4.0);
                            ui.add_enabled_ui(vd, |ui| {
                                ui.columns(3, |cols| {
                                    for (i, label) in ["5 sec", "10 sec", "15 sec"].into_iter().enumerate() {
                                        let active = self.ui_state.voice_detect_duration == i as u8;
                                        let btn = egui::Button::new(
                                            RichText::new(label)
                                                .size(11.5)
                                                .strong()
                                                .color(if active { Color32::WHITE } else { TEXT_MUTED }),
                                        )
                                        .fill(if active { ACCENT_BLUE } else { BG_INSET })
                                        .stroke(Stroke::new(1.0_f32, if active { ACCENT_BLUE_HI } else { BORDER_CARD }))
                                        .corner_radius(CornerRadius::same(7))
                                        .min_size(Vec2::new(cols[i].available_width(), 26.0));
                                        if cols[i].add(btn).clicked() && !active {
                                            self.client.send_async(set_voice_detect_duration(i as u8));
                                        }
                                    }
                                });
                            });

                            ui.add_space(10.0);
                            let mut siren = self.ui_state.siren_detect;
                            if toggle_row(ui, "Siren Detect", "Auto-switch to Ambient Sound on alarms & sirens", &mut siren) {
                                self.client.send_async(set_siren_detect(siren));
                            }

                            ui.add_space(8.0);
                            let mut sidetone = self.ui_state.ambient_during_calls;
                            if toggle_row(ui, "Ambient Sound During Calls", "Hear your own voice naturally during calls", &mut sidetone) {
                                self.client.send_async(set_ambient_during_calls(sidetone));
                            }

                            ui.add_space(8.0);
                            let mut one_bud = self.ui_state.one_earbud_noise_control;
                            if toggle_row(ui, "Noise Controls with One Earbud", "Allow ANC & Adaptive while wearing only one bud", &mut one_bud) {
                                self.client.send_async(set_one_earbud_noise_control(one_bud));
                            }
                        });
                    });

                    // 6. Low Latency & Codec Card (Windows SBC toggle works even while disconnected)
                    card(ui, |ui| {
                        section_title(ui, "LOW LATENCY & SYNC");

                        let (mut force_sbc, busy, note) = {
                            let c = self.codec.lock().unwrap();
                            (c.force_sbc, c.busy, c.note.clone())
                        };
                        ui.add_enabled_ui(!busy, |ui| {
                            if toggle_row(
                                ui,
                                "Force SBC Codec (Disable AAC)",
                                "Cuts Windows Bluetooth lag (~100–150 ms); asks admin once",
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
                            ui.label(RichText::new(note).size(11.0).color(ACCENT_AMBER));
                        }

                        ui.add_space(8.0);
                        ui.add_enabled_ui(snap.connected, |ui| {
                            let mut gm = self.ui_state.gaming_mode;
                            if toggle_row(
                                ui,
                                "Earbud Gaming Mode",
                                "Samsung low-latency sync flag (small effect on Windows)",
                                &mut gm,
                            ) {
                                self.client.send_async(set_gaming_mode(gm));
                            }
                        });

                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new("Audio Output & Enhancements").size(13.0).color(Color32::from_rgb(0xF1, 0xF5, 0xF9)));
                                ui.label(RichText::new("Bypass Voicemeeter & turn Enhancements Off").size(11.0).color(TEXT_MUTED));
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("Sound Settings").clicked() {
                                    open_sound_settings();
                                }
                            });
                        });
                    });
                });
            });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.client.disconnect();
    }
}

fn card(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE
        .fill(BG_CARD)
        .stroke(Stroke::new(1.0_f32, BORDER_CARD))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(13))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui);
        });
    ui.add_space(10.0);
}

fn section_title(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).size(10.5).strong().color(TEXT_MUTED));
    ui.add_space(8.0);
}

fn battery_badge(ui: &mut egui::Ui, title: &str, value: &str, sub: &str) {
    egui::Frame::NONE
        .fill(BG_INSET)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).size(9.5).strong().color(TEXT_DIM));
            ui.label(RichText::new(value).size(14.0).strong().color(Color32::WHITE));
            ui.label(RichText::new(sub).size(10.5).color(TEXT_MUTED));
        });
}

fn slider_header(ui: &mut egui::Ui, label: &str, value: &str, accent: Color32) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(12.5).color(Color32::from_rgb(0xE2, 0xE8, 0xF0)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(value).size(12.0).strong().color(accent));
        });
    });
}

fn full_width_slider(ui: &mut egui::Ui, val: &mut u8, range: std::ops::RangeInclusive<u8>) -> bool {
    let prev = *val;
    ui.spacing_mut().slider_width = (ui.available_width() - 8.0).max(80.0);
    ui.add(egui::Slider::new(val, range).show_value(false));
    *val != prev
}

fn toggle_row(ui: &mut egui::Ui, title: &str, subtitle: &str, on: &mut bool) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(RichText::new(title).size(13.0).color(Color32::from_rgb(0xF1, 0xF5, 0xF9)));
            ui.label(RichText::new(subtitle).size(11.0).color(TEXT_MUTED));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if pill_switch(ui, on).clicked() {
                clicked = true;
            }
        });
    });
    clicked
}

fn pill_switch(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let desired = Vec2::new(40.0, 22.0);
    let (rect, mut resp) = ui.allocate_exact_size(desired, egui::Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let bg = if *on { ACCENT_GREEN } else { Color32::from_rgb(0x33, 0x41, 0x55) };
    let r = rect.height() * 0.5;
    ui.painter().rect_filled(rect, CornerRadius::same(r as u8), bg);
    let cx = if *on { rect.right() - r } else { rect.left() + r };
    ui.painter().circle_filled(egui::pos2(cx, rect.center().y), r - 2.5, Color32::WHITE);
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

    eframe::run_native(
        "Pods4",
        options,
        Box::new(move |cc| Ok(Box::new(Pods4App::new(cc, client, show_event)))),
    )
}
