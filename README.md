# Pods4

Lightweight, zero-dependency Windows desktop & system-tray controller for **Samsung Galaxy Buds4 Pro** (and compatible Samsung Galaxy Buds) passthrough and noise controls over native Bluetooth Classic RFCOMM.

## Features

- **Noise Control Modes**: Off, Active Noise Cancelling (ANC), Ambient Sound (Passthrough), and Adaptive — two-way live sync with the earbuds.
- **Passthrough & ANC Levels**: 5-step Ambient Sound Volume (`Level 1`–`Level 5`) and 5-step ANC Strength (`Level 1`–`Level 5`).
- **Customize Ambient Sound**: Per-ear boost (Left / Right: `-1`, `Normal`, `+1`) and Ambient Tone (`Soft +2` to `Clear +2`).
- **Auto Switch & Passthrough Options**:
  - **Voice Detect** (auto-switch to Ambient Sound when speaking, `5s` / `10s` / `15s` timeout)
  - **Siren Detect** (auto-switch to Ambient Sound on alarms & sirens)
  - **Ambient Sound During Calls** (sidetone)
  - **Noise Controls with One Earbud**
- **Live Status & System Tray**: Battery & wear/charging status for Left, Right, and Case; single-instance tray mode with one-click mode switching.
- **Zero External Dependencies**: Pure .NET 8 WPF + Win32 `ws2_32.dll` (RFCOMM) and `bthprops.cpl` (Bluetooth device discovery).

## Requirements

- Windows 10 / 11 with Bluetooth
- [.NET 8 SDK](https://dotnet.microsoft.com/download/dotnet/8.0) (to build from source)
- Paired & connected Samsung Galaxy Buds4 Pro (or compatible Buds exposing Samsung SPP UUID `2e73a4ad-332d-41fc-90e2-16bef06523f2`)

## Build & Run

```powershell
dotnet build -c Release
.\bin\Release\net8.0-windows\Pods4.exe
```

Run the built-in protocol self-check:

```powershell
.\bin\Release\net8.0-windows\Pods4.exe --self-check
```

## License

[MIT](LICENSE)
