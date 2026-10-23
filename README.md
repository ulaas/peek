# Peek

A fast, quiet remote desktop client written in Rust with egui. RDP first, VNC second.

Website: <https://ulaas.github.io/peek/> (source in `site/`, published by `.github/workflows/pages.yml`).

## Status

Working today:

- **RDP** on IronRDP: TLS + NLA (CredSSP), RemoteFX, dynamic resize, text and file clipboard, certificate trust on first use
- **Audio** (RDP): own low-latency output in the device's native format, Opus or PCM from the server, remote volume, per-session volume and mute, speaker indicator on tabs
- **VNC**: in-house RFB 3.3–3.8 client with None/VNC auth and Raw, CopyRect, RRE, Hextile, ZRLE, Tight + JPEG decoders
- Tabs, connection library (groups, favorites, search, recent), OS keychain for passwords, `.rdp` import
- User names as `DOMAIN\user`, `user@domain` or `MicrosoftAccount\you@outlook.com` (Microsoft accounts are sent as a UPN)
- Per-connection screen size: match the window, a preset or custom (default 1920 × 1080)
- Command palette (Ctrl+K), floating session toolbar, full screen, live thumbnails of open sessions
- Keyboard grab on Windows: Windows key, Alt+Tab and Print Screen go to the remote computer
- Auto-reconnect with backoff, Wake-on-LAN, screenshot to clipboard
- Session recording (`.peekrec`: lossless picture, Opus sound) with a built-in player and MP4 export (H.264 + AAC)

Not yet: SSH, RD Gateway, H.264 (EGFX).

The RDP path has not yet been tested against a live Windows host; VNC is tested end to end against the bundled test server.

## Download

Builds for Windows (x64), Linux (x64) and macOS (Apple silicon, preview) are on the [releases page](https://github.com/ulaas/peek/releases/latest). They are not code-signed yet; the release notes say how to get past the first-launch warning on each system. On Linux, run `./install.sh` from the extracted archive to add Peek to the application menu.

## Use

```
peek                          # open the app
peek server01                 # connect (RDP by default)
peek vnc://pi.local:1         # VNC display 1 = port 5901
peek alice@10.0.0.5:3390      # RDP with user name and port
peek Work.rdp                 # import an mstsc connection file
peek session.peekrec          # play a recording
peek --version                # version and the commit it was built from
```

| Shortcut | Action |
|---|---|
| Ctrl+K | Command palette |
| Ctrl+L | Quick connect |
| Ctrl+Alt+Enter | Full screen (works inside a session) |
| Ctrl+N / Ctrl+W / Ctrl+Tab | New connection / close tab / next tab |

Shortcuts other than Ctrl+Alt+Enter go to the remote computer while a session has focus. Click outside the session to use them locally.

## Build

```
./scripts/dev.ps1 build            # cargo build, then target/ hygiene
./scripts/dev.ps1 test --workspace
cargo run -p peek-vnc --example testserver -- 5999 [password]   # local VNC test server
cargo run -p peek-record --features mp4 --example export -- in.peekrec out.mp4 [--check frame.bmp]
```

Building needs Rust 1.90+ and CMake (for the bundled Opus); on Linux also `libasound2-dev` and `pkg-config`.

Releases: bump `version` in `Cargo.toml`, add a `## <version>` section to `CHANGELOG.md`, commit, then push a tag `v<version>`. `.github/workflows/release.yml` builds, tests and publishes the release with that section as its notes. Running the workflow by hand from the Actions tab is a dry run that only uploads the packages as artifacts.

Disk use is kept in check by the build profiles (dependencies built without debug info) and `scripts/hygiene.ps1`. The script removes superseded build outputs older than 3 days and runs `cargo clean` if `target/` passes 12 GB. `dev.ps1` runs it after every cargo command.

## Layout

| Crate | Role |
|---|---|
| `peek-core` | Framebuffer with dirty rectangles, input model (scancodes, keysyms), endpoints, session contract |
| `peek-rdp` | RDP backend on IronRDP; copies only updated regions |
| `peek-vnc` | RFB client and decoders; test server example |
| `peek-record` | `.peekrec` writer and player |
| `peek-store` | Library, settings, known hosts, keychain vault, `.rdp` import |
| `peek-app` | The egui application (`peek` binary) |

Logging: set `PEEK_LOG=debug`. Rendering defaults to DX12 and the power-saving GPU on Windows; override with `WGPU_BACKEND` / `WGPU_POWER_PREF`.

Icons: [Phosphor](https://phosphoricons.com) regular and fill (MIT).
