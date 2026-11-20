# Pods4

Lightweight standalone **Rust** Windows desktop & system-tray controller for **Samsung Galaxy Buds4 Pro** (and compatible Samsung Galaxy Buds) passthrough and noise controls over native Bluetooth Classic RFCOMM.

## Features

- **Noise Control Modes**: Off, Active Noise Cancelling (ANC), Ambient Sound (Passthrough), and Adaptive — two-way live sync with the earbuds.
- **Passthrough & ANC Levels**: 5-step Ambient Sound Volume (`Level 1`–`Level 5`) and 5-step ANC Strength (`Level 1`–`Level 5`).
- **Customize Ambient Sound**: Per-ear boost (Left / Right: `-1`, `Normal`, `+1`) and Ambient Tone (`Soft +2` to `Clear +2`).
- **Auto Switch & Passthrough Options**:
  - **Voice Detect** (auto-switch to Ambient Sound when speaking, `5s` / `10s` / `15s` timeout)
  - **Siren Detect** (auto-switch to Ambient Sound on alarms & sirens)
  - **Ambient Sound During Calls** (sidetone)
  - **Noise Controls with One Earbud**
- **Low Latency & Sync**: One-click Windows Bluetooth **Force SBC Codec (Disable AAC)** toggle (with UAC & A2DP restart), **Earbud Gaming Mode** (`ADJUST_SOUND_SYNC`), and quick access to Windows Sound Settings.
- **Live Status & Single-Instance System Tray**: Battery & wear/charging badges for Left, Right, and Case; closing minimizes to tray and relaunching wakes the connected window without duplicating Bluetooth connections.
- **Single Standalone `.exe`**: Pure Rust (`eframe`/`egui` + `tray-icon` + native Win32 `ws2_32.dll` & `bthprops.cpl`) — no runtime dependencies required to run.

## Requirements

- Windows 10 / 11 with Bluetooth
- [Rust](https://rustup.rs/) (to build from source)
- Paired & connected Samsung Galaxy Buds4 Pro (or compatible Buds exposing Samsung SPP UUID `2e73a4ad-332d-41fc-90e2-16bef06523f2`)

## Build & Run

```powershell
cargo test
cargo build --release
.\target\release\pods4.exe
```

## License

[MIT](LICENSE)
