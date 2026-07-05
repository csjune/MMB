# MMB

Multi Monitor Brightness is a Windows tray app for adjusting monitor brightness across multiple displays.

## Features

- Tray popup with per-monitor brightness sliders
- Optional synchronized brightness changes for all monitors
- Delayed brightness apply to avoid excessive monitor updates
- Refresh control for re-detecting connected monitors
- Windows light/dark mode toggle
- Separate tray icons for light and dark taskbar modes

## Build

```powershell
cargo build --release --locked
```
