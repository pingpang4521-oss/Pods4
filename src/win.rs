//! Small Win32 helpers shared across the app: wide strings, HKCU registry access, known
//! folders, message boxes and shell launching.

use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW,
    RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE,
    REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_BINARY, RRF_RT_REG_SZ,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, SW_SHOWNORMAL};

pub const APP_NAME: &str = "Pods4";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// NUL-terminated UTF-16 for Win32 `W` APIs.
pub fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------- message boxes

pub fn message_box(text: &str, flags: u32) -> i32 {
    let title = wide(APP_NAME);
    let body = wide(text);
    unsafe { MessageBoxW(null_mut(), body.as_ptr(), title.as_ptr(), flags) }
}

// ---------------------------------------------------------------- shell

/// Opens a folder, file or URI with its default handler (Explorer for folders).
pub fn shell_open(target: impl AsRef<std::ffi::OsStr>) {
    let verb = wide("open");
    let file = wide(target);
    unsafe {
        ShellExecuteW(null_mut(), verb.as_ptr(), file.as_ptr(), null(), null(), SW_SHOWNORMAL);
    }
}

// ---------------------------------------------------------------- known folders

fn known_folder(id: &windows::core::GUID) -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

/// `%LOCALAPPDATA%`
pub fn local_app_data() -> PathBuf {
    known_folder(&windows::Win32::UI::Shell::FOLDERID_LocalAppData)
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir)
}

/// Settings, logs and the notification icon: `%LOCALAPPDATA%\Pods4`.
pub fn data_dir() -> PathBuf {
    local_app_data().join(APP_NAME)
}

pub fn log_dir() -> PathBuf {
    data_dir().join("logs")
}

/// Per-user program files: `%LOCALAPPDATA%\Programs`.
pub fn user_programs_dir() -> PathBuf {
    known_folder(&windows::Win32::UI::Shell::FOLDERID_UserProgramFiles)
        .unwrap_or_else(|| local_app_data().join("Programs"))
}

/// The user's Start menu programs folder.
pub fn start_menu_programs_dir() -> Option<PathBuf> {
    known_folder(&windows::Win32::UI::Shell::FOLDERID_Programs)
}

/// Case-insensitive path equality after resolving both sides (false if either is missing).
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a.as_os_str().eq_ignore_ascii_case(b.as_os_str()),
        _ => false,
    }
}

// ---------------------------------------------------------------- HKCU registry

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

fn create_key(path: &str) -> Option<Key> {
    let p = wide(path);
    let mut hkey: HKEY = null_mut();
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            p.as_ptr(),
            0,
            null(),
            REG_OPTION_NON_VOLATILE,
            KEY_READ | KEY_WRITE,
            null(),
            &mut hkey,
            null_mut(),
        )
    };
    (rc == ERROR_SUCCESS).then_some(Key(hkey))
}

pub fn reg_key_exists(path: &str) -> bool {
    let p = wide(path);
    let mut hkey: HKEY = null_mut();
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, p.as_ptr(), 0, KEY_READ, &mut hkey) };
    if rc == ERROR_SUCCESS {
        drop(Key(hkey));
        true
    } else {
        false
    }
}

/// Writes string and DWORD values under `HKCU\path`, creating the key.
pub fn reg_set_values(path: &str, strings: &[(&str, &str)], dwords: &[(&str, u32)]) -> Result<(), String> {
    let key = create_key(path).ok_or_else(|| format!(r"Could not open HKCU\{path}"))?;
    for (name, value) in strings {
        let n = wide(name);
        let v = wide(value);
        let rc = unsafe {
            RegSetValueExW(key.0, n.as_ptr(), 0, REG_SZ, v.as_ptr().cast(), (v.len() * 2) as u32)
        };
        if rc != ERROR_SUCCESS {
            return Err(format!(r"Could not write HKCU\{path}\{name} ({rc})"));
        }
    }
    for (name, value) in dwords {
        let n = wide(name);
        let rc = unsafe {
            RegSetValueExW(key.0, n.as_ptr(), 0, REG_DWORD, (value as *const u32).cast(), 4)
        };
        if rc != ERROR_SUCCESS {
            return Err(format!(r"Could not write HKCU\{path}\{name} ({rc})"));
        }
    }
    Ok(())
}

fn reg_get_raw(path: &str, name: &str, flags: u32) -> Option<Vec<u8>> {
    let p = wide(path);
    let n = wide(name);
    let mut size: u32 = 0;
    let rc = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, p.as_ptr(), n.as_ptr(), flags, null_mut(), null_mut(), &mut size)
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            p.as_ptr(),
            n.as_ptr(),
            flags,
            null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    buf.truncate(size as usize);
    Some(buf)
}

pub fn reg_get_string(path: &str, name: &str) -> Option<String> {
    let raw = reg_get_raw(path, name, RRF_RT_REG_SZ)?;
    let units: Vec<u16> = raw.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    let end = units.iter().position(|&c| c == 0).unwrap_or(units.len());
    Some(String::from_utf16_lossy(&units[..end]))
}

pub fn reg_get_binary(path: &str, name: &str) -> Option<Vec<u8>> {
    reg_get_raw(path, name, RRF_RT_REG_BINARY)
}

pub fn reg_delete_value(path: &str, name: &str) {
    let p = wide(path);
    let n = wide(name);
    unsafe {
        RegDeleteKeyValueW(HKEY_CURRENT_USER, p.as_ptr(), n.as_ptr());
    }
}

pub fn reg_delete_tree(path: &str) {
    let p = wide(path);
    unsafe {
        RegDeleteTreeW(HKEY_CURRENT_USER, p.as_ptr());
    }
    // RegDeleteTreeW empties the key; remove the (now empty) key itself too.
    if let Some((parent, leaf)) = path.rsplit_once('\\') {
        let pp = wide(parent);
        let pl = wide(leaf);
        let mut hkey: HKEY = null_mut();
        unsafe {
            if RegOpenKeyExW(HKEY_CURRENT_USER, pp.as_ptr(), 0, KEY_READ | KEY_WRITE, &mut hkey) == ERROR_SUCCESS {
                windows_sys::Win32::System::Registry::RegDeleteKeyW(hkey, pl.as_ptr());
                RegCloseKey(hkey);
            }
        }
    }
}
