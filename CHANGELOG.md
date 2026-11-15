# Changelog

The release workflow publishes the section for the tag being released as its notes.

## Unreleased

### Changed

- macOS: **Update and restart** replaces Peek.app in place, as on Windows and Linux, and asks first when sessions are open. The app in the new disk image must be Peek at the new version, signed with the same Developer ID team as the running Peek and notarized; it is copied next to the installed one and swapped in with one rename, so a failed update leaves the installed Peek as it was. A Peek run from its download location, from a disk image or read-only disk, from a folder you cannot write to, or one without a Developer ID signature still downloads, checks and opens the new disk image, and says why

## 0.6.4

Peek for the Mac, ready to download.

### Changed

- macOS: the disk image is signed with a Developer ID and notarized by Apple, so Peek opens without a warning. It is built for Apple silicon and runs on macOS 11 and later
- macOS: the window has the Mac's own frame. The traffic lights sit in Peek's title bar, and resizing, zoom and full screen are the system's
- macOS: double-clicking a `.peekrec` recording in Finder opens it in Peek, and Peek is offered under Open With for `.rdp` files
- macOS: the log is in `~/Library/Logs/Peek` and update downloads in `~/Library/Caches/Peek`
- macOS: shortcut hints say Cmd, and Ctrl+Tab also goes to the next tab, because Cmd+Tab switches apps
- macOS: Peek says why it asks for local network access, which macOS 15 and later ask about before it reaches a computer on your network

### Fixed

- macOS: in an RDP session Cmd is sent as Ctrl, so Cmd+C, Cmd+V and Cmd+X copy, paste and cut on the remote computer. Before, Cmd was the Windows key, so Cmd+V opened the clipboard history

## 0.6.3

### Fixed

- Windows: the part of the window that a resize uncovers shows Peek's background instead of flashing white until the next frame

## 0.6.2

### Changed

- About: the mark, name and version sit in a header above the build's facts (commit, build, platform, renderer, settings folder), the license and the update check. **Copy info** puts the facts on the clipboard for a bug report

## 0.6.1

A small release, and the first one Peek 0.6.0 can install by itself with **Update and restart**.

### Fixed

- Updating an MSI install: the downloaded installer is checked against its SHA-256 again at the moment it is run, and held so that nothing can change it while Windows Installer reads it. Before, it was checked once after the download and then waited in a folder any program of yours could write to

## 0.6.0

Peek keeps itself up to date.

### New in this release

- Update check: Peek asks GitHub for a new version once a day when it starts (switch under Settings → Updates) and on request from About, Settings or the command palette. The request carries only a `peek/<version>` User-Agent
- One-click update: **Update and restart** downloads the new version, installs it only if its SHA-256 matches `SHA256SUMS.txt` and GitHub's digest of the file, and restarts Peek. Nothing is downloaded before the click, and Peek asks first while sessions are connected
- Portable Windows and Linux copies are replaced in place, an MSI install runs the new MSI, macOS opens the new disk image; Scoop, Homebrew and system packages are left to their package manager

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

| System | Installer | Portable |
|---|---|---|
| Windows 10 and 11 (x64) | `peek-windows-x64.msi` | `peek-windows-x64.zip` |
| Debian, Ubuntu (x64) | `peek-linux-x64.deb` | `peek-linux-x64.tar.gz` |
| Fedora, openSUSE (x64) | `peek-linux-x64.rpm` | `peek-linux-x64.tar.gz` |
| macOS 11 or newer (Apple silicon) | `peek-macos-arm64.dmg` | `peek-macos-arm64.zip` |

Linux builds need glibc 2.39 or newer (Ubuntu 24.04, Debian 13, Fedora 40 and later).

- **Windows:** the MSI installs Peek for all users, adds it to the Start menu and the `PATH`, opens `.peekrec` recordings and lists Peek under *Open with* for `.rdp` files. Peek is not code-signed yet, so SmartScreen may warn on first launch: choose *More info*, then *Run anyway*.
- **Linux:** `sudo apt install ./peek-linux-x64.deb` or `sudo dnf install ./peek-linux-x64.rpm`. From the portable archive, run `./install.sh` to add Peek to your application menu, or run `./peek` directly.
- **macOS:** open the disk image and drag Peek to Applications. Peek is not notarized, so the first launch is blocked: open *System Settings → Privacy & Security* and choose *Open Anyway*, or run `xattr -dr com.apple.quarantine /Applications/Peek.app`. The macOS build has not been tested on a Mac yet; reports are welcome.

`SHA256SUMS.txt` lists a checksum for every file.

### Not yet

SSH, RD Gateway, H.264 for RDP (EGFX), keyboard grab on macOS and Linux.

## 0.5.0

Installers for every system.

### New in this release

- Installers: an MSI for Windows, `.deb` and `.rpm` packages for Linux and a disk image for macOS
- The Windows installer adds Peek to the Start menu and the `PATH`, opens `.peekrec` recordings and lists Peek under *Open with* for `.rdp` files
- Linux packages add Peek to the application menu and register `.peekrec` files

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

| System | Installer | Portable |
|---|---|---|
| Windows 10 and 11 (x64) | `peek-windows-x64.msi` | `peek-windows-x64.zip` |
| Debian, Ubuntu (x64) | `peek-linux-x64.deb` | `peek-linux-x64.tar.gz` |
| Fedora, openSUSE (x64) | `peek-linux-x64.rpm` | `peek-linux-x64.tar.gz` |
| macOS 11 or newer (Apple silicon) | `peek-macos-arm64.dmg` | `peek-macos-arm64.zip` |

Linux builds need glibc 2.39 or newer (Ubuntu 24.04, Debian 13, Fedora 40 and later).

- **Windows:** the MSI installs Peek for all users, adds it to the Start menu and the `PATH`, opens `.peekrec` recordings and lists Peek under *Open with* for `.rdp` files. Peek is not code-signed yet, so SmartScreen may warn on first launch: choose *More info*, then *Run anyway*.
- **Linux:** `sudo apt install ./peek-linux-x64.deb` or `sudo dnf install ./peek-linux-x64.rpm`. From the portable archive, run `./install.sh` to add Peek to your application menu, or run `./peek` directly.
- **macOS:** open the disk image and drag Peek to Applications. Peek is not notarized, so the first launch is blocked: open *System Settings → Privacy & Security* and choose *Open Anyway*, or run `xattr -dr com.apple.quarantine /Applications/Peek.app`. The macOS build has not been tested on a Mac yet; reports are welcome.

`SHA256SUMS.txt` lists a checksum for every file.

### Not yet

SSH, RD Gateway, H.264 for RDP (EGFX), keyboard grab on macOS and Linux.

## 0.1.0

The first public release: portable builds for Windows, Linux and macOS.
