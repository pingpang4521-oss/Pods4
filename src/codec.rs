//! Windows Bluetooth codec switch: disabling AAC forces the lower-latency SBC codec. Needs an
//! elevated `reg.exe` and an A2DP restart of the buds, so it runs `cmd.exe` through UAC.

use std::mem::{size_of, zeroed};
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::ptr::null_mut;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use eframe::egui;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, WAIT_OBJECT_0};
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

use crate::bluetooth::{self, BudsClient};
use crate::win::wide;
use crate::{log_info, log_warn};

pub struct CodecState {
    pub force_sbc: bool,
    pub busy: bool,
    pub note: String,
}

impl CodecState {
    pub fn load() -> Self {
        Self { force_sbc: read_sbc_forced(), busy: false, note: String::new() }
    }
}

/// True when Windows Bluetooth A2DP AAC is disabled (forcing lower-latency SBC).
pub fn read_sbc_forced() -> bool {
    let key = wide(r"SYSTEM\CurrentControlSet\Services\BthA2dp\Parameters");
    let name = wide("BluetoothAacEnable");
    let mut val: u32 = 1;
    let mut size: u32 = size_of::<u32>() as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&mut val as *mut u32).cast(),
            &mut size,
        )
    };
    rc == ERROR_SUCCESS && val == 0
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

pub fn apply_sbc_forced_async(
    force_sbc: bool,
    codec: Arc<Mutex<CodecState>>,
    client: BudsClient,
    ctx: egui::Context,
) {
    {
        let mut st = codec.lock().unwrap_or_else(|e| e.into_inner());
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
        log_info!(
            "Setting BluetoothAacEnable={aac_val}; A2DP restart target: {}",
            instance_id.as_deref().unwrap_or("none found")
        );
        let cmd = sbc_command_line(
            &system32_exe("reg.exe"),
            &system32_exe("pnputil.exe"),
            aac_val,
            instance_id.as_deref(),
        );
        let verb = wide("runas");
        let file = wide(system32_exe("cmd.exe"));
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
        let note: String = if !ok {
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
        if ok && wait_rc == WAIT_OBJECT_0 && exit_code == 0 {
            log_info!("Codec change done: {note}");
        } else {
            log_warn!("Codec change: {note} (launched={ok}, wait={wait_rc}, exit={exit_code})");
        }
        {
            let mut st = codec.lock().unwrap_or_else(|e| e.into_inner());
            st.force_sbc = actual;
            st.busy = false;
            st.note = note;
        }
        ctx.request_repaint();

        if restarted {
            thread::sleep(Duration::from_millis(800));
            client.connect_auto_async();
        }
    });
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
