//! Minimal log file at `%LOCALAPPDATA%\Pods4\logs\pods4.log`. The release build is a windowed
//! app with no console, so this is the only place connection and install problems show up.

use std::fmt::Arguments;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

/// Rotate to `pods4.old.log` at startup once the log passes this size.
const MAX_BYTES: u64 = 1024 * 1024;

static LOG: Mutex<Option<File>> = Mutex::new(None);

pub fn init(dir: &Path) {
    let _ = fs::create_dir_all(dir);
    let path = dir.join("pods4.log");
    if fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = fs::rename(&path, dir.join("pods4.old.log"));
    }
    if let Ok(f) = OpenOptions::new().create(true).append(true).open(&path) {
        *LOG.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    }
}

pub fn write(level: &str, args: Arguments) {
    let line = format!("{} {level:<5} {args}", timestamp());
    if cfg!(debug_assertions) {
        eprintln!("{line}");
    }
    if let Some(f) = LOG.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        let _ = writeln!(f, "{line}");
    }
}

fn timestamp() -> String {
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    let t = unsafe {
        let mut t = std::mem::zeroed();
        GetLocalTime(&mut t);
        t
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

#[macro_export]
macro_rules! log_info {
    ($($t:tt)*) => { $crate::applog::write("INFO", format_args!($($t)*)) };
}

#[macro_export]
macro_rules! log_warn {
    ($($t:tt)*) => { $crate::applog::write("WARN", format_args!($($t)*)) };
}
