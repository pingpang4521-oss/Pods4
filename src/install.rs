//! Per-user install with no admin rights and no external installer:
//! - copies the exe to `%LOCALAPPDATA%\Programs\Pods4\Pods4.exe`
//! - adds a Start menu shortcut (with the app's AppUserModelID, so toasts and pins group)
//! - registers an uninstall entry under Settings › Apps (`Pods4.exe --uninstall`)
//! - manages "Start with Windows" through the HKCU Run key.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use crate::win::{self, APP_NAME, VERSION};
use crate::{log_info, log_warn, notify};

const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Pods4";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Task Manager's Startup tab records "disabled" here without touching the Run value.
const STARTUP_APPROVED_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const EXE_NAME: &str = "Pods4.exe";

pub fn install_dir() -> PathBuf {
    win::user_programs_dir().join(APP_NAME)
}

pub fn installed_exe() -> PathBuf {
    install_dir().join(EXE_NAME)
}

fn shortcut_path() -> Option<PathBuf> {
    win::start_menu_programs_dir().map(|d| d.join("Pods4.lnk"))
}

fn current_exe() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

/// True when this process is the installed copy.
pub fn is_running_installed() -> bool {
    current_exe().is_some_and(|exe| win::same_file(&exe, &installed_exe()))
}

/// True when an installed copy exists (whether or not this process is it).
pub fn is_installed() -> bool {
    installed_exe().is_file() && win::reg_key_exists(UNINSTALL_KEY)
}

pub fn installed_version() -> Option<String> {
    win::reg_get_string(UNINSTALL_KEY, "DisplayVersion")
}

/// The exe autostart should launch: the installed copy if there is one, else this one.
fn preferred_exe() -> Option<PathBuf> {
    if installed_exe().is_file() {
        Some(installed_exe())
    } else {
        current_exe()
    }
}

// ---------------------------------------------------------------- autostart

/// Task Manager's flag byte: 01/02/06 mean enabled, 03/07 disabled.
fn startup_flag_disabled(flag: u8) -> bool {
    flag & 0x03 == 0x03
}

pub fn autostart_enabled() -> bool {
    let registered = win::reg_get_string(RUN_KEY, APP_NAME).is_some();
    let disabled = win::reg_get_binary(STARTUP_APPROVED_KEY, APP_NAME)
        .and_then(|b| b.first().copied())
        .is_some_and(startup_flag_disabled);
    registered && !disabled
}

pub fn autostart_command(exe: &Path) -> String {
    format!("\"{}\" --tray", exe.display())
}

pub fn set_autostart(on: bool) -> Result<(), String> {
    if on {
        let exe = preferred_exe().ok_or("Could not find Pods4.exe")?;
        win::reg_set_values(RUN_KEY, &[(APP_NAME, &autostart_command(&exe))], &[])?;
        // Clear a "disabled" mark left by Task Manager, or the Run value would be ignored.
        win::reg_delete_value(STARTUP_APPROVED_KEY, APP_NAME);
    } else {
        win::reg_delete_value(RUN_KEY, APP_NAME);
        win::reg_delete_value(STARTUP_APPROVED_KEY, APP_NAME);
    }
    log_info!("Start with Windows {}", if on { "enabled" } else { "disabled" });
    Ok(())
}

// ---------------------------------------------------------------- install

/// Runs `f` with COM initialized on this thread (needed for the shell link objects).
fn with_com<T>(f: impl FnOnce() -> T) -> T {
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let out = f();
    if hr.is_ok() {
        unsafe { CoUninitialize() };
    }
    out
}

fn create_shortcut(lnk: &Path, target: &Path) -> windows::core::Result<()> {
    use windows::core::{Interface, HSTRING};
    use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
    use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PROPVARIANT};
    use windows::Win32::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER};
    use windows::Win32::System::Variant::VT_LPWSTR;
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{IShellLinkW, SHStrDupW, ShellLink};

    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(target))?;
        if let Some(dir) = target.parent() {
            link.SetWorkingDirectory(&HSTRING::from(dir))?;
        }
        link.SetDescription(&HSTRING::from("Galaxy Buds noise controls"))?;
        link.SetIconLocation(&HSTRING::from(target), 0)?;

        // Same AppUserModelID as the process, so taskbar pins and notifications line up.
        let store: IPropertyStore = link.cast()?;
        // The string must come from the COM allocator (as InitPropVariantFromString does):
        // pointing the PROPVARIANT at Rust-owned memory corrupts the heap.
        let mut pv = PROPVARIANT::default();
        (*pv.Anonymous.Anonymous).vt = VT_LPWSTR;
        (*pv.Anonymous.Anonymous).Anonymous.pwszVal = SHStrDupW(&HSTRING::from(notify::AUMID))?;
        let set = store.SetValue(&PKEY_AppUserModel_ID, &pv).and_then(|_| store.Commit());
        let _ = PropVariantClear(&mut pv);
        set?;

        let file: IPersistFile = link.cast()?;
        file.Save(&HSTRING::from(lnk), true)?;
    }
    Ok(())
}

fn dir_size_kb(dir: &Path) -> u32 {
    let bytes: u64 = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum())
        .unwrap_or(0);
    (bytes / 1024).min(u32::MAX as u64) as u32
}

fn copy_with_retry(src: &Path, dst: &Path) -> std::io::Result<()> {
    let mut last = None;
    // An old copy may still be shutting down and holding the file open.
    for _ in 0..20 {
        match std::fs::copy(src, dst) {
            Ok(_) => return Ok(()),
            Err(e) => last = Some(e),
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err(last.unwrap())
}

/// Installs (or updates) this exe for the current user and returns the installed exe path.
/// The caller must make sure no other Pods4 is running from the install folder.
pub fn install() -> Result<PathBuf, String> {
    let src = current_exe().ok_or("Could not locate the running Pods4.exe")?;
    let dir = install_dir();
    let dst = dir.join(EXE_NAME);
    log_info!("Installing {} -> {}", src.display(), dst.display());

    std::fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    if !win::same_file(&src, &dst) {
        copy_with_retry(&src, &dst).map_err(|e| format!("Could not copy Pods4 to {}: {e}", dst.display()))?;
    }

    if let Some(lnk) = shortcut_path() {
        match with_com(|| create_shortcut(&lnk, &dst)) {
            Ok(()) => log_info!("Start menu shortcut: {}", lnk.display()),
            Err(e) => log_warn!("Could not create Start menu shortcut: {e}"),
        }
    }

    let exe = dst.display().to_string();
    win::reg_set_values(
        UNINSTALL_KEY,
        &[
            ("DisplayName", APP_NAME),
            ("DisplayVersion", VERSION),
            ("Publisher", APP_NAME),
            ("DisplayIcon", &format!("{exe},0")),
            ("InstallLocation", &dir.display().to_string()),
            ("UninstallString", &format!("\"{exe}\" --uninstall")),
            ("QuietUninstallString", &format!("\"{exe}\" --uninstall --quiet")),
            ("Comments", "Galaxy Buds noise controls"),
        ],
        &[("NoModify", 1), ("NoRepair", 1), ("EstimatedSize", dir_size_kb(&dir))],
    )?;

    // Autostart set up from a portable copy should now launch the installed one.
    if autostart_enabled() {
        set_autostart(true)?;
    }
    log_info!("Installed version {VERSION}");
    Ok(dst)
}

/// Starts `exe` detached (used to hand over to the freshly installed copy).
pub fn launch(exe: &Path, args: &[&str]) -> std::io::Result<()> {
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    Command::new(exe).args(args).creation_flags(DETACHED_PROCESS).spawn().map(|_| ())
}

// ---------------------------------------------------------------- uninstall

/// Guards every recursive delete: only ever our own `...\Pods4` folders.
fn is_safe_app_dir(dir: &Path) -> bool {
    dir.is_absolute()
        && dir.file_name().is_some_and(|n| n.eq_ignore_ascii_case(APP_NAME))
        && dir.components().count() > 3
}

/// Removes the install folder after this process exits (a running exe can't delete itself).
fn schedule_dir_removal(dir: &Path) -> Result<(), String> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    let d = dir.display().to_string();
    // Inside quotes `&` and `^` are literal for cmd, but `%` would still expand variables.
    if !is_safe_app_dir(dir) || d.chars().any(|c| matches!(c, '"' | '%' | '!')) {
        return Err(format!("Refusing to schedule removal of {d}"));
    }
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    // Outer quotes: see the `cmd /c` quote-stripping rule in codec.rs. Two attempts in case the
    // first one runs before this process has fully exited.
    let cmdline = format!(
        "/d /c \"ping -n 3 127.0.0.1 >nul & rmdir /s /q \"{d}\" & ping -n 3 127.0.0.1 >nul & rmdir /s /q \"{d}\"\""
    );
    Command::new(format!(r"{root}\System32\cmd.exe"))
        .raw_arg(cmdline)
        .current_dir(std::env::temp_dir())
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not schedule removal of {d}: {e}"))
}

/// Removes everything `install()` and the app created. Call only when no other instance runs.
pub fn uninstall() -> Result<(), String> {
    let _ = set_autostart(false);
    if let Some(lnk) = shortcut_path() {
        let _ = std::fs::remove_file(lnk);
    }
    win::reg_delete_tree(UNINSTALL_KEY);
    notify::unregister_app_id();

    let data = win::data_dir();
    if is_safe_app_dir(&data) && data.exists() {
        let _ = std::fs::remove_dir_all(&data);
    }

    let dir = install_dir();
    if !dir.exists() {
        return Ok(());
    }
    let running_from_install = current_exe().is_some_and(|exe| exe.starts_with(&dir) || win::same_file(&exe, &installed_exe()));
    if running_from_install {
        schedule_dir_removal(&dir)
    } else if is_safe_app_dir(&dir) {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("Could not remove {}: {e}", dir.display()))
    } else {
        Err(format!("Refusing to remove {}", dir.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_our_own_folders_are_deleted() {
        assert!(is_safe_app_dir(Path::new(r"C:\Users\me\AppData\Local\Programs\Pods4")));
        assert!(is_safe_app_dir(Path::new(r"C:\Users\me\AppData\Local\pods4")));
        assert!(!is_safe_app_dir(Path::new(r"C:\Users\me\AppData\Local\Programs")));
        assert!(!is_safe_app_dir(Path::new(r"C:\Pods4")));
        assert!(!is_safe_app_dir(Path::new(r"Pods4")));
        assert!(!is_safe_app_dir(Path::new(r"C:\Users\me\Pods4Backup")));
    }

    /// Touches the real shell; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn creates_a_shortcut_with_app_id() {
        let lnk = std::env::temp_dir().join("pods4-test.lnk");
        let target = std::env::current_exe().unwrap();
        with_com(|| create_shortcut(&lnk, &target)).unwrap();
        assert!(lnk.is_file());
        let _ = std::fs::remove_file(lnk);
    }

    #[test]
    fn reads_task_manager_startup_flags() {
        // Values seen in the wild: Steam 01, Parsec 02 (enabled); Discord, Docker 03 (disabled).
        for enabled in [0x01, 0x02, 0x06] {
            assert!(!startup_flag_disabled(enabled), "{enabled:#04x}");
        }
        for disabled in [0x03, 0x07] {
            assert!(startup_flag_disabled(disabled), "{disabled:#04x}");
        }
    }

    #[test]
    fn autostart_launches_quoted_exe_in_tray_mode() {
        let cmd = autostart_command(Path::new(r"C:\Users\A B\AppData\Local\Programs\Pods4\Pods4.exe"));
        assert_eq!(cmd, r#""C:\Users\A B\AppData\Local\Programs\Pods4\Pods4.exe" --tray"#);
    }
}
