//! `AppControl`: show, hide and exit the app from any thread (tray, second-instance watcher,
//! UI). It drives the window through Win32 directly because egui doesn't run while hidden.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use eframe::egui::{self, ViewportCommand};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IsIconic, IsWindowVisible, PostMessageW, PostThreadMessageW, SetForegroundWindow, SetWindowPos,
    ShowWindow, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_RESTORE, SW_SHOW,
    WM_CLOSE, WM_QUIT,
};

use crate::bluetooth::BudsClient;
use crate::log_info;

#[derive(Clone)]
pub struct AppControl {
    pub ctx: egui::Context,
    pub client: BudsClient,
    hwnd: isize,
    exiting: Arc<AtomicBool>,
    /// Thread id of the tray's message loop (0 until the tray is up).
    tray_thread: Arc<AtomicU32>,
}

impl AppControl {
    pub fn new(ctx: egui::Context, client: BudsClient, hwnd: isize) -> Self {
        Self {
            ctx,
            client,
            hwnd,
            exiting: Arc::new(AtomicBool::new(false)),
            tray_thread: Arc::new(AtomicU32::new(0)),
        }
    }

    pub fn set_tray_thread(&self, id: u32) {
        self.tray_thread.store(id, Ordering::SeqCst);
    }

    pub fn is_exiting(&self) -> bool {
        self.exiting.load(Ordering::SeqCst)
    }

    pub fn is_window_showing(&self) -> bool {
        if self.hwnd == 0 {
            return false;
        }
        let h = self.hwnd as HWND;
        unsafe { IsWindowVisible(h) != 0 && IsIconic(h) == 0 }
    }

    /// Brings the window forward and reconnects if the buds dropped.
    pub fn show(&self) {
        if self.hwnd != 0 {
            let h = self.hwnd as HWND;
            unsafe {
                ShowWindow(h, SW_SHOW);
                if IsIconic(h) != 0 {
                    ShowWindow(h, SW_RESTORE);
                }
                // Topmost toggle lets a background thread raise the window past focus rules.
                SetWindowPos(h, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
                SetWindowPos(h, HWND_NOTOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
                SetForegroundWindow(h);
            }
        }
        self.ctx.request_repaint();
        if !self.client.is_connected() {
            self.client.connect_auto_async();
        }
    }

    pub fn hide(&self) {
        if self.hwnd != 0 {
            unsafe {
                ShowWindow(self.hwnd as HWND, SW_HIDE);
            }
        }
    }

    pub fn toggle(&self) {
        if self.is_window_showing() {
            self.hide();
        } else {
            self.show();
        }
    }

    /// Quits the whole app: closes the link, removes the tray icon and closes the window.
    pub fn exit(&self) {
        if self.exiting.swap(true, Ordering::SeqCst) {
            return;
        }
        log_info!("Exiting");
        self.client.disconnect();
        let tray = self.tray_thread.load(Ordering::SeqCst);
        if tray != 0 {
            unsafe {
                PostThreadMessageW(tray, WM_QUIT, 0, 0);
            }
        }
        if self.hwnd != 0 {
            unsafe {
                PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0);
            }
        }
        self.ctx.send_viewport_cmd(ViewportCommand::Close);
        self.ctx.request_repaint();
        // If the hidden window never processes the close, don't linger as an invisible process.
        thread::spawn(|| {
            thread::sleep(Duration::from_secs(5));
            log_info!("Window did not close in time; forcing exit");
            std::process::exit(0);
        });
    }
}
