# Changelog

The release workflow publishes the section for the tag being released as its notes.

## 0.1.0

The first public release.

### Connect

- RDP on IronRDP: TLS and NLA (CredSSP), RemoteFX, the remote screen resizing with the window, certificates trusted on first use
- VNC: Peek's own RFB 3.3–3.8 client with None and VNC password auth and Raw, CopyRect, RRE, Hextile, ZRLE and Tight + JPEG
- Connect by name, address or URL: `server01`, `alice@10.0.0.5:3390`, `vnc://pi.local:1`
- User names as `DOMAIN\user`, `user@domain` or `MicrosoftAccount\you@outlook.com`
- `.rdp` import by drag and drop, the command palette or the command line

### Work

- Tabs with live thumbnails, a floating session toolbar, full screen (Ctrl+Alt+Enter)
- Command palette (Ctrl+K) and quick connect (Ctrl+L)
- Connection library with groups, favorites, search and recents; passwords in the OS keychain
- Text and file clipboard in both directions (RDP)
- Remote audio through a low-latency pipeline, with per-session volume and mute (RDP)
- Keyboard grab on Windows: the Windows key, Alt+Tab and Print Screen go to the remote computer
- Auto-reconnect with backoff, Wake-on-LAN, screenshot to clipboard
- Per-connection screen size and RDP experience settings

### Record

- Session recording to `.peekrec` (lossless picture, Opus sound), a built-in player and MP4 export (H.264 + AAC)

### Downloads

| System | File |
|---|---|
| Windows 10 and 11 (x64) | `peek-windows-x64.zip` |
| Linux (x64, glibc 2.39 or newer: Ubuntu 24.04, Debian 13, Fedora 40 and later) | `peek-linux-x64.tar.gz` |
| macOS 11 or newer (Apple silicon) | `peek-macos-arm64.zip` |

The builds are not code-signed yet:

- **Windows:** SmartScreen may warn on first launch. Choose *More info*, then *Run anyway*.
- **macOS:** the first launch is blocked. Open *System Settings → Privacy & Security* and choose *Open Anyway*, or run `xattr -dr com.apple.quarantine Peek.app`. This is the first macOS build and it has not been tested on a Mac yet; reports are welcome.
- **Linux:** extract the archive and run `./install.sh` to add Peek to your application menu, or run `./peek` directly.

`SHA256SUMS.txt` lists a checksum for every file.

### Not yet

SSH, RD Gateway, H.264 for RDP (EGFX), keyboard grab on macOS and Linux.
