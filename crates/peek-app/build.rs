//! Stamps the binary with the commit it was built from (shown in About and
//! `peek --version`). Git is only read; optional locks are off so `git status`
//! never rewrites the index. On Windows it also embeds the Peek mark as the
//! executable's icon, which Explorer and pinned taskbar entries show while
//! Peek is not running.

use std::process::Command;

#[path = "src/mark.rs"]
mod mark;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).env("GIT_OPTIONAL_LOCKS", "0").output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let sha = git(&["rev-parse", "--short=9", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    println!("cargo:rustc-env=PEEK_GIT_SHA={sha}{}", if dirty { "-dirty" } else { "" });
    println!("cargo:rustc-env=PEEK_BUILD_DATE={}", today());
    println!("cargo:rustc-env=PEEK_PROFILE={}", std::env::var("PROFILE").unwrap_or_default());

    // Re-stamp when HEAD moves or the index changes (commit, checkout, add).
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        for f in ["HEAD", "index", "refs/heads"] {
            println!("cargo:rerun-if-changed={dir}/{f}");
        }
        if let Some(head) = git(&["symbolic-ref", "-q", "HEAD"]) {
            println!("cargo:rerun-if-changed={dir}/{head}");
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/mark.rs");

    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target == "windows" && env == "msvc" {
        let res = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("peek-icon.res");
        std::fs::write(&res, icon_res()).expect("write icon resource");
        // link.exe takes compiled resource files directly.
        println!("cargo:rustc-link-arg-bins={}", res.display());
    }
}

/// A compiled resource file (.res) holding the mark as icon group 1, so no
/// resource compiler is needed. Each size is a 32-bit DIB with alpha.
fn icon_res() -> Vec<u8> {
    const RT_ICON: u16 = 3;
    const RT_GROUP_ICON: u16 = 14;
    const SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

    fn entry(out: &mut Vec<u8>, kind: u16, id: u16, flags: u16, data: &[u8]) {
        out.extend((data.len() as u32).to_le_bytes());
        out.extend(32u32.to_le_bytes()); // header size
        out.extend([0xFFFF, kind, 0xFFFF, id].iter().flat_map(|v: &u16| v.to_le_bytes()));
        out.extend(0u32.to_le_bytes()); // data version
        out.extend(flags.to_le_bytes());
        out.extend(0u16.to_le_bytes()); // language neutral
        out.extend([0u8; 8]); // version, characteristics
        out.extend(data);
        out.resize(out.len().next_multiple_of(4), 0);
    }

    let mut out = Vec::new();
    entry(&mut out, 0, 0, 0, &[]); // a .res file starts with an empty entry
    let mut group = Vec::new();
    group.extend([0u16, 1, SIZES.len() as u16].iter().flat_map(|v| v.to_le_bytes()));
    for (i, &size) in SIZES.iter().enumerate() {
        let rgba = mark::rgba(size);
        let mask_row = (size.div_ceil(32) * 4) as usize;
        let mut dib = Vec::new();
        // BITMAPINFOHEADER; the height counts the colour and mask halves.
        for v in [40u32, size, size * 2] {
            dib.extend(v.to_le_bytes());
        }
        dib.extend(1u16.to_le_bytes());
        dib.extend(32u16.to_le_bytes());
        dib.extend([0u8; 24]);
        // Rows bottom-up, BGRA.
        for y in (0..size as usize).rev() {
            for px in rgba[y * size as usize * 4..(y + 1) * size as usize * 4].as_chunks::<4>().0 {
                dib.extend([px[2], px[1], px[0], px[3]]);
            }
        }
        dib.resize(dib.len() + mask_row * size as usize, 0); // AND mask unused with alpha
        let id = i as u16 + 1;
        entry(&mut out, RT_ICON, id, 0x1010, &dib);
        let dim = if size >= 256 { 0 } else { size as u8 };
        group.extend([dim, dim, 0, 0]);
        group.extend(1u16.to_le_bytes());
        group.extend(32u16.to_le_bytes());
        group.extend((dib.len() as u32).to_le_bytes());
        group.extend(id.to_le_bytes());
    }
    entry(&mut out, RT_GROUP_ICON, 1, 0x1030, &group);
    out
}

/// UTC date as YYYY-MM-DD without pulling in a date crate.
fn today() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let days = (secs / 86_400) as i64;
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}
