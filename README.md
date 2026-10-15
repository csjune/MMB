# MMB

Multi Monitor Brightness is a Windows tray app for adjusting monitor brightness across multiple displays.

## Features

- **Popup:** Click the tray icon to open a popup with a brightness slider for each monitor.
  - Adjust a slider by dragging, clicking, or scrolling. Scrolling moves it in 5% steps.
- **Tray wheel:** Scroll the mouse wheel over the tray icon to change brightness instantly.
- **Tooltip:** Hover over the tray icon to see the current brightness.
  - Shows a single line when all monitors match, or one line per monitor otherwise.
- **Sync monitors button:** Keeps all monitors at the same brightness. On by default.
- **Windows theme button:** Switches Windows between light and dark mode. The app also follows the system theme automatically.
- **Lightweight:** Uses very little memory while it waits in the tray.

## Requirements

- Windows
- Segoe Fluent Icons font

## Build

```powershell
cargo build --release --locked
```
