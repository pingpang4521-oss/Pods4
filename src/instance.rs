//! Single-instance lock plus two named events other Pods4 processes use to talk to the running
//! one: "show your window" (a second launch) and "exit" (installer, updater, uninstaller).

use std::ptr::null;
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, SetEvent, WaitForMultipleObjects, INFINITE,
};

use crate::control::AppControl;
use crate::win::wide;

const MUTEX_NAME: &str = r"Local\Pods4_SingleInstance";
const SHOW_EVENT_NAME: &str = r"Local\Pods4_ShowWindow";
const EXIT_EVENT_NAME: &str = r"Local\Pods4_Exit";

pub struct SingleInstanceGuard {
    mutex: HANDLE,
    show_event: HANDLE,
    exit_event: HANDLE,
}

// The raw handles are only used for kernel waits/signals, which are thread-safe.
unsafe impl Send for SingleInstanceGuard {}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        unsafe {
            // Wake the watcher thread (it sees `exiting` and stops) before the handles go away.
            if !self.show_event.is_null() {
                SetEvent(self.show_event);
                CloseHandle(self.show_event);
            }
            if !self.exit_event.is_null() {
                CloseHandle(self.exit_event);
            }
            if !self.mutex.is_null() {
                CloseHandle(self.mutex);
            }
        }
    }
}

fn create_event(name: &str) -> HANDLE {
    let n = wide(name);
    // Auto-reset, initially unsignaled; opens the existing event if another process made it.
    unsafe { CreateEventW(null(), 0, 0, n.as_ptr()) }
}

fn signal(name: &str) {
    let ev = create_event(name);
    if !ev.is_null() {
        unsafe {
            SetEvent(ev);
            CloseHandle(ev);
        }
    }
}

/// Takes the single-instance lock, or returns `None` if Pods4 is already running.
pub fn try_acquire() -> Option<SingleInstanceGuard> {
    let n = wide(MUTEX_NAME);
    unsafe {
        let mutex = CreateMutexW(null(), 1, n.as_ptr());
        let already = GetLastError() == ERROR_ALREADY_EXISTS;
        if already || mutex.is_null() {
            if !mutex.is_null() {
                CloseHandle(mutex);
            }
            return None;
        }
        Some(SingleInstanceGuard {
            mutex,
            show_event: create_event(SHOW_EVENT_NAME),
            exit_event: create_event(EXIT_EVENT_NAME),
        })
    }
}

/// Asks the running instance to bring its window forward.
pub fn signal_show() {
    signal(SHOW_EVENT_NAME);
}

/// Asks any running instance to exit, then waits for its lock to be released.
pub fn acquire_replacing(timeout: Duration) -> Option<SingleInstanceGuard> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(g) = try_acquire() {
            return Some(g);
        }
        signal(EXIT_EVENT_NAME);
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(200));
    }
}

impl SingleInstanceGuard {
    /// Reacts to signals from other Pods4 processes until the app exits.
    pub fn watch(&self, control: AppControl) {
        if self.show_event.is_null() || self.exit_event.is_null() {
            return;
        }
        let handles = [self.show_event as usize, self.exit_event as usize];
        thread::spawn(move || {
            let hs: [HANDLE; 2] = [handles[0] as HANDLE, handles[1] as HANDLE];
            loop {
                let rc = unsafe { WaitForMultipleObjects(2, hs.as_ptr(), 0, INFINITE) };
                if control.is_exiting() {
                    break;
                }
                if rc == WAIT_OBJECT_0 {
                    control.show();
                } else if rc == WAIT_OBJECT_0 + 1 {
                    crate::log_info!("Exit requested by another Pods4 process");
                    control.exit();
                    break;
                } else {
                    break;
                }
            }
        });
    }
}
