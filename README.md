# RDP Center

Native Windows 10/11 IT Asset & Remote Launcher (**RDP**, **SSH**, **Web Admin**) built with **Rust** and **WinUI 3** (`windows-reactor` / Windows App SDK 2.x with Windows 11 Mica) for fast fuzzy search and instant remote connections.

## Features

- **Fuzzy Search + Bold Highlighting**: Multi-token search across machine name, IP/host, category, protocol, username, and notes with Thai/English support (e.g. `nas backup`, `.10`, `บัญชี`).
- **One-Key / One-Click Remote Launch**: Press `Enter` or click `Launch` to start:
  - **RDP** (`mstsc.exe`)
  - **SSH** (`wt.exe` Windows Terminal / `ssh.exe`)
  - **Web Admin** (`http://` / `https://` via native `ShellExecuteW`)
- **Live Online Status Indicator**: Background concurrent TCP connect probes (`3389`, `22`, `80`/`443`, or custom ports) with latency (ms), auto-refreshing every 30 seconds while visible.
- **`.rdp` Importer (Drag & Drop)**: Drag `.rdp` files or folders onto the window (or click `Import .rdp`) supporting UTF-16 LE/BE and UTF-8, `server port:i:`, deduplication, symlink/reparse filtering, and a 64 KiB read cap.
- **System / Light / Dark Theme**: Defaults to Windows `System` theme and cycles `System -> Light -> Dark`, saved to `settings.json`.
- **Adaptive Monochrome System Tray & Windows Startup**:
  - **Quick Remote Right-Click Menu**: Launch RDP, SSH, and Web targets directly from the tray icon (`Open RDP Center`, `Hide to Tray`, `Quick Remote`, `Exit`), with automatic Dark/Light taskbar icon switching.
  - **Minimize / Close to Tray, Start with Windows & Start Minimized**: Configurable in `Settings` (`HKCU\...\Run`, `--minimized`).

## Keyboard Shortcuts

| Shortcut | Action |
|---|---|
| `Enter` | Launch the selected machine (from search box or list) |
| `Ctrl + R` | Refresh online status immediately |

## Configuration (`assets.json` & `settings.json`)

Configuration lookup order:

1. `--config <path>` CLI argument
2. `RDP_CENTER_CONFIG` environment variable
3. `%APPDATA%\RDPCenter\assets.json` (with `settings.json` stored in the same directory; no config files are created beside `RDPCenter.exe`)

If no config file exists on first launch, starter sample entries are generated automatically.

## Requirements & Build

- **Windows Runtime**: Requires **Windows App Runtime 2.x** (`Microsoft.WindowsAppRuntime.2`, version `2.4+`) on Windows 10/11.

```bash
# Clippy check
cargo clippy --target x86_64-pc-windows-gnu --all-targets -- -D warnings

# Compile Windows unit tests
cargo test --no-run --target x86_64-pc-windows-gnu

# Build release executable
cargo build --release --target x86_64-pc-windows-gnu
# Output: target/x86_64-pc-windows-gnu/release/RDPCenter.exe
```
