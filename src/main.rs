#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod applog;
mod bluetooth;
mod codec;
mod control;
mod install;
mod instance;
mod notify;
mod protocol;
mod settings;
mod tray;
mod widgets;
mod win;

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Margin, RichText, Vec2, ViewportCommand};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_DEFBUTTON2, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_ICONWARNING, MB_OK, MB_YESNO,
};

use bluetooth::BudsClient;
use codec::CodecState;
use control::AppControl;
use instance::SingleInstanceGuard;
use notify::Notifier;
use protocol::{
    set_ambient_during_calls, set_ambient_volume, set_anc_level, set_customize_ambient,
    set_gaming_mode, set_noise_control, set_one_earbud_noise_control, set_siren_detect,
    set_voice_detect, set_voice_detect_duration, BudsState, NoiseControlMode, PlacementState,
};
use settings::{SettingsStore, BATTERY_THRESHOLDS};
use widgets::*;
use win::VERSION;

/// The buds push their full settings right after RFCOMM connects. If that packet never
/// arrives, unlock the controls after this long rather than leaving them disabled forever.
const INITIAL_SYNC_FALLBACK: Duration = Duration::from_secs(4);
/// How long installer/uninstaller wait for a running copy to exit.
const REPLACE_TIMEOUT: Duration = Duration::from_secs(10);

const BOOST_LABELS: [&str; 3] = ["-1", "Normal", "+1"];
const TONE_LABELS: [&str; 5] = ["Soft +2", "Soft +1", "Neutral", "Clear +1", "Clear +2"];

const ICON_PNG: &[u8] = include_bytes!("../assets/pods4-256.png");

// ---------------------------------------------------------------- command line

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    /// Start hidden in the tray (used by "Start with Windows").
    tray: bool,
    /// Install this exe for the current user, then launch the installed copy.
    install: bool,
    /// Remove Pods4 (run from Settings › Apps).
    uninstall: bool,
    /// Close a running copy and take over (used after an in-app install).
    replace: bool,
    /// No confirmation or result dialogs (`QuietUninstallString`, scripted installs).
    quiet: bool,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Args {
    let mut a = Args::default();
    for arg in args {
        match arg.trim_start_matches(['-', '/']).to_ascii_lowercase().as_str() {
            "tray" | "minimized" => a.tray = true,
            "install" => a.install = true,
            "uninstall" => a.uninstall = true,
            "replace" => a.replace = true,
            "quiet" | "silent" => a.quiet = true,
            _ => {}
        }
    }
    a
}

// ---------------------------------------------------------------- install / uninstall flows

/// Shows a message box unless running quietly; returns the button pressed (IDYES when quiet).
fn tell(quiet: bool, text: &str, flags: u32) -> i32 {
    if quiet { IDYES } else { win::message_box(text, flags) }
}

const STILL_RUNNING: &str = "Pods4 is still running and could not be closed.\n\nExit it from the tray icon and try again.";

fn run_cli_install(quiet: bool) {
    let Some(guard) = instance::acquire_replacing(REPLACE_TIMEOUT) else {
        tell(quiet, STILL_RUNNING, MB_OK | MB_ICONERROR);
        return;
    };
    match install::install() {
        Ok(exe) => {
            drop(guard);
            if let Err(e) = install::launch(&exe, &[]) {
                tell(quiet, &format!("Pods4 was installed but could not be started:\n\n{e}"), MB_OK | MB_ICONWARNING);
            }
        }
        Err(e) => {
            log_warn!("Install failed: {e}");
            tell(quiet, &format!("Pods4 could not be installed:\n\n{e}"), MB_OK | MB_ICONERROR);
        }
    }
}

fn run_uninstall(quiet: bool) {
    let mut text = "Remove Pods4 from this PC?\n\nThis removes the app, its Start menu and startup entries, \
                    and its settings and logs."
        .to_string();
    if codec::read_sbc_forced() {
        text += "\n\nWindows is still set to use the SBC codec instead of AAC. To switch back, turn off \
                 \"Force SBC codec\" in Pods4 first.";
    }
    if tell(quiet, &text, MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2) != IDYES {
        return;
    }
    let Some(guard) = instance::acquire_replacing(REPLACE_TIMEOUT) else {
        tell(quiet, STILL_RUNNING, MB_OK | MB_ICONERROR);
        return;
    };
    let result = install::uninstall();
    drop(guard);
    match result {
        Ok(()) => tell(quiet, "Pods4 has been removed.", MB_OK | MB_ICONINFORMATION),
        Err(e) => tell(
            quiet,
            &format!("Pods4 was removed, but some files could not be deleted:\n\n{e}"),
            MB_OK | MB_ICONWARNING,
        ),
    };
}

/// A second launch from a different copy than the installed one: offer to update the install.
fn offer_update_of_installed() -> bool {
    if !install::is_installed() || install::is_running_installed() {
        return false;
    }
    let installed = install::installed_version().unwrap_or_else(|| "unknown".into());
    let text = format!(
        "Pods4 is already running.\n\nReplace the installed copy (version {installed}) with this one (version {VERSION})?"
    );
    win::message_box(&text, MB_YESNO | MB_ICONQUESTION) == IDYES
}

// ---------------------------------------------------------------- app

enum InstallStatus {
    Idle,
    Busy,
    Failed(String),
}

struct Pods4App {
    control: AppControl,
    client: BudsClient,
    settings: Arc<SettingsStore>,
    notifier: Notifier,
    tray: tray::Tray,
    _instance: SingleInstanceGuard,

    ui_state: BudsState,
    slider_dragging: bool,
    /// When the current connection came up; drives the initial-sync fallback.
    connected_since: Option<Instant>,
    codec: Arc<Mutex<CodecState>>,

    autostart: bool,
    autostart_note: Option<String>,
    running_installed: bool,
    installed_version: Option<String>,
    install_status: Arc<Mutex<InstallStatus>>,
}

impl Pods4App {
    fn new(cc: &eframe::CreationContext<'_>, client: BudsClient, instance: SingleInstanceGuard) -> Result<Self, String> {
        let hwnd: isize = cc
            .window_handle()
            .ok()
            .and_then(|h| match h.as_raw() {
                RawWindowHandle::Win32(w) => Some(w.hwnd.get()),
                _ => None,
            })
            .unwrap_or(0);

        apply_theme(&cc.egui_ctx);

        let control = AppControl::new(cc.egui_ctx.clone(), client.clone(), hwnd);
        let settings = Arc::new(SettingsStore::load(win::data_dir().join("settings.ini")));
        let notifier = Notifier::spawn(client.clone(), settings.clone());
        let tray = tray::spawn(control.clone())?;
        control.set_tray_thread(tray.refresher.thread_id());

        // Bluetooth updates: repaint the window, refresh the tray, check battery alerts.
        let ctx = cc.egui_ctx.clone();
        let refresher = tray.refresher;
        let n = notifier.clone();
        client.set_on_update(Arc::new(move || {
            ctx.request_repaint();
            refresher.refresh();
            n.state_changed();
        }));

        instance.watch(control.clone());
        client.connect_auto_async();

        let installed_version = install::is_installed().then(install::installed_version).flatten();
        Ok(Self {
            control,
            client,
            settings,
            notifier,
            tray,
            _instance: instance,
            ui_state: BudsState::default(),
            slider_dragging: false,
            connected_since: None,
            codec: Arc::new(Mutex::new(CodecState::load())),
            autostart: install::autostart_enabled(),
            autostart_note: None,
            running_installed: install::is_running_installed(),
            installed_version,
            install_status: Arc::new(Mutex::new(InstallStatus::Idle)),
        })
    }

    fn hide_to_tray(&self) {
        self.control.hide();
        if !self.settings.get().tray_hint_shown {
            self.notifier.toast(
                "Pods4 is still running",
                "It keeps your buds connected from the system tray. Right-click the tray icon to exit.",
            );
            self.settings.update(|s| s.tray_hint_shown = true);
        }
    }

    /// Installs this copy, then hands over to the installed exe and exits.
    fn start_install(&self) {
        {
            let mut st = self.install_status.lock().unwrap_or_else(|e| e.into_inner());
            if matches!(*st, InstallStatus::Busy) {
                return;
            }
            *st = InstallStatus::Busy;
        }
        let status = self.install_status.clone();
        let control = self.control.clone();
        thread::spawn(move || {
            let result = install::install().and_then(|exe| {
                install::launch(&exe, &["--replace"]).map_err(|e| format!("Installed, but could not start it: {e}"))
            });
            match result {
                // The new copy asks us to exit too; exiting right away is just faster.
                Ok(()) => control.exit(),
                Err(e) => {
                    log_warn!("Install failed: {e}");
                    *status.lock().unwrap_or_else(|e| e.into_inner()) = InstallStatus::Failed(e);
                    control.ctx.request_repaint();
                }
            }
        });
    }

    fn install_label(&self) -> &'static str {
        if self.installed_version.is_some() { "Update" } else { "Install" }
    }

    fn header(&mut self, ui: &mut egui::Ui, snap: &bluetooth::ConnectionSnapshot, status_text: &str) {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_max_width((ui.available_width() - 105.0).max(160.0));
                ui.add(
                    egui::Label::new(RichText::new(&snap.device_name).size(27.0).strong().color(TEXT_PRIMARY))
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
                    ui.label(RichText::new(status_text).size(12.5).color(TEXT_MUTED));
                });
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if action_button(ui, "Reconnect", BG_CARD, !snap.connecting).clicked() {
                    self.client.connect_auto_async();
                }
            });
        });
        ui.add_space(16.0);
    }

    /// Shown by a portable copy until it is installed or dismissed.
    fn install_banner(&mut self, ui: &mut egui::Ui) {
        if self.running_installed || self.settings.get().install_banner_dismissed {
            return;
        }
        let busy = matches!(*self.install_status.lock().unwrap_or_else(|e| e.into_inner()), InstallStatus::Busy);
        card_filled(ui, egui::Color32::from_rgb(0x14, 0x24, 0x3D), |ui| {
            let (title, subtitle) = match &self.installed_version {
                Some(v) => (
                    format!("Update installed Pods4 ({v} → {VERSION})"),
                    "Replaces the installed copy with this one.".to_string(),
                ),
                None => (
                    "Install Pods4".to_string(),
                    "Adds it to the Start menu, lets it start with Windows, and puts an uninstaller in \
                     Settings › Apps. No admin needed."
                        .to_string(),
                ),
            };
            ui.label(RichText::new(title).size(14.5).strong().color(TEXT_PRIMARY));
            ui.add_space(2.0);
            ui.label(RichText::new(subtitle).size(11.5).color(TEXT_MUTED));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if primary_button(ui, if busy { "Installing..." } else { self.install_label() }, !busy).clicked() {
                    self.start_install();
                }
                if action_button(ui, "Not now", BG_INSET, !busy).clicked() {
                    self.settings.update(|s| s.install_banner_dismissed = true);
                }
            });
        });
    }

    fn app_card(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            section_title(ui, "App");

            let mut autostart = self.autostart;
            if toggle_row(ui, "Start with Windows", "Opens quietly in the tray when you sign in", &mut autostart) {
                match install::set_autostart(autostart) {
                    Ok(()) => {
                        self.autostart = autostart;
                        self.autostart_note = None;
                    }
                    Err(e) => {
                        log_warn!("Could not change autostart: {e}");
                        self.autostart_note = Some(e);
                    }
                }
            }
            if let Some(note) = &self.autostart_note {
                ui.label(RichText::new(note).size(11.5).color(ACCENT_RED));
            }

            divider(ui);
            let prefs = self.settings.get();
            let mut alerts = prefs.low_battery_alerts;
            let subtitle = format!("Notify when an earbud drops to {}%", prefs.low_battery_threshold);
            if toggle_row(ui, "Low battery alerts", &subtitle, &mut alerts) {
                self.settings.update(|s| s.low_battery_alerts = alerts);
            }
            ui.add_space(8.0);
            ui.add_enabled_ui(alerts, |ui| {
                ui.columns(BATTERY_THRESHOLDS.len(), |cols| {
                    for (i, t) in BATTERY_THRESHOLDS.into_iter().enumerate() {
                        let active = prefs.low_battery_threshold == t;
                        if pill_button(&mut cols[i], &format!("{t}%"), active, 30.0, 12.0).clicked() && !active {
                            self.settings.update(|s| s.low_battery_threshold = t);
                        }
                    }
                });
            });

            divider(ui);
            let status_subtitle = if self.running_installed {
                "Installed · remove it from Settings › Apps".to_string()
            } else if let Some(v) = &self.installed_version {
                format!("Portable copy · installed version is {v}")
            } else {
                "Portable copy · not installed".to_string()
            };
            let busy = matches!(*self.install_status.lock().unwrap_or_else(|e| e.into_inner()), InstallStatus::Busy);
            labeled_row(ui, &format!("Pods4 {VERSION}"), &status_subtitle, |ui| {
                if !self.running_installed
                    && primary_button(ui, if busy { "Installing..." } else { self.install_label() }, !busy).clicked()
                {
                    self.start_install();
                }
                if action_button(ui, "Logs", BG_INSET, true).clicked() {
                    win::shell_open(win::log_dir());
                }
            });
            if let InstallStatus::Failed(e) = &*self.install_status.lock().unwrap_or_else(|e| e.into_inner()) {
                ui.add_space(4.0);
                ui.label(RichText::new(e).size(11.5).color(ACCENT_RED));
            }
        });
    }
}

impl eframe::App for Pods4App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Window X button hides to the tray instead of exiting.
        if ctx.input(|i| i.viewport().close_requested()) && !self.control.is_exiting() {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.hide_to_tray();
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

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BG_MAIN).inner_margin(Margin::symmetric(16, 10)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    // 1. Header: large One UI style title + connection status
                    self.header(ui, &snap, &status_text);
                    self.install_banner(ui);

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
                            let c = self.codec.lock().unwrap_or_else(|e| e.into_inner());
                            (c.force_sbc, c.busy, c.note.clone())
                        };
                        ui.add_enabled_ui(!busy, |ui| {
                            if toggle_row(
                                ui,
                                "Force SBC codec (disable AAC)",
                                "Cuts Windows Bluetooth lag by ~100–150 ms; asks for admin once",
                                &mut force_sbc,
                            ) {
                                codec::apply_sbc_forced_async(
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
                        labeled_row(ui, "Audio output & enhancements", "Bypass Voicemeeter and turn Enhancements off", |ui| {
                            if action_button(ui, "Open", BG_INSET, true).clicked() {
                                win::shell_open("ms-settings:sound");
                            }
                        });
                    });

                    // 8. App: autostart, alerts, install status
                    self.app_card(ui);
                    ui.add_space(6.0);
                });
            });
        self.slider_dragging = dragging_now;
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.control.exit();
        self.tray.wait_closed();
    }
}

fn main() -> eframe::Result<()> {
    let args = parse_args(std::env::args().skip(1));
    notify::set_process_app_id();

    if args.uninstall {
        run_uninstall(args.quiet);
        return Ok(());
    }

    applog::init(&win::log_dir());
    log_info!(
        "Pods4 {VERSION} starting from {} {args:?}",
        std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default()
    );

    if args.install {
        run_cli_install(args.quiet);
        return Ok(());
    }

    let guard = if args.replace { instance::acquire_replacing(REPLACE_TIMEOUT) } else { instance::try_acquire() };
    let Some(guard) = guard else {
        if args.replace {
            log_warn!("Could not take over from the running copy");
            win::message_box(
                "The previous copy of Pods4 is still running.\n\nExit it from the tray icon and start Pods4 again.",
                MB_OK | MB_ICONWARNING,
            );
        } else if offer_update_of_installed() {
            run_cli_install(false);
        } else {
            instance::signal_show();
        }
        return Ok(());
    };

    notify::register_app_id();
    let client = BudsClient::new();

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Pods4")
        .with_inner_size([430.0, 760.0])
        .with_min_inner_size([380.0, 560.0])
        .with_resizable(true)
        .with_visible(!args.tray);
    match eframe::icon_data::from_png_bytes(ICON_PNG) {
        Ok(icon) => viewport = viewport.with_icon(Arc::new(icon)),
        Err(e) => log_warn!("Could not decode window icon: {e}"),
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };

    let res = eframe::run_native(
        "Pods4",
        options,
        Box::new(move |cc| Ok(Box::new(Pods4App::new(cc, client, guard)?))),
    );
    if let Err(e) = &res {
        log_warn!("Pods4 failed to start: {e}");
        win::message_box(&format!("Pods4 failed to start:\n\n{e}"), MB_OK | MB_ICONERROR);
    }
    log_info!("Stopped");
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Args {
        parse_args(list.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_flags_in_any_style() {
        assert_eq!(args(&[]), Args::default());
        assert_eq!(args(&["--tray"]), Args { tray: true, ..Args::default() });
        assert_eq!(args(&["/Uninstall"]), Args { uninstall: true, ..Args::default() });
        assert_eq!(
            args(&["-install", "--REPLACE", "whatever"]),
            Args { install: true, replace: true, ..Args::default() }
        );
        assert_eq!(
            args(&["--uninstall", "--quiet"]),
            Args { uninstall: true, quiet: true, ..Args::default() }
        );
    }
}
