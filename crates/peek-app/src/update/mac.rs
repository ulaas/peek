//! Replacing Peek.app in place on macOS.
//!
//! The verified disk image is mounted read-only and hidden, and the app in it
//! has to be Peek (same bundle identifier, the release's version), pass
//! `codesign --verify --deep --strict`, be signed with a Developer ID of the
//! same team as the running Peek, and be accepted by Gatekeeper as notarized.
//! Only then is it copied next to the installed app and swapped in with one
//! rename. Anything that fails leaves the installed app as it was.
//!
//! The parts that only look at text and paths compile everywhere, so they are
//! tested on every system; running Apple's tools is macOS only.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::path::{Path, PathBuf};

/// Full paths: an app started from Finder gets a minimal PATH.
#[cfg(target_os = "macos")]
mod tool {
    pub const HDIUTIL: &str = "/usr/bin/hdiutil";
    pub const CODESIGN: &str = "/usr/bin/codesign";
    pub const SPCTL: &str = "/usr/sbin/spctl";
    pub const DITTO: &str = "/usr/bin/ditto";
    pub const PLISTBUDDY: &str = "/usr/libexec/PlistBuddy";
}

/// The `.app` an executable in `<Name>.app/Contents/MacOS/` belongs to.
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    let named = |p: &Path, name: &str| p.file_name().is_some_and(|n| n.eq_ignore_ascii_case(name));
    let is_app = app.extension().is_some_and(|e| e.eq_ignore_ascii_case("app"));
    (named(macos, "MacOS") && named(contents, "Contents") && is_app).then(|| app.to_path_buf())
}

/// The signing team in what `codesign -dv` prints; `None` for an ad-hoc or
/// unsigned app, whose TeamIdentifier is "not set" or missing.
pub fn team_id(codesign_dv: &str) -> Option<String> {
    let id = codesign_dv.lines().find_map(|l| l.trim().strip_prefix("TeamIdentifier="))?.trim();
    // Apple's team identifiers are ten capital letters and digits; anything else is not one.
    (id.len() == 10 && id.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())).then(|| id.to_string())
}

/// A code requirement that only a Developer ID application certificate of
/// `team` satisfies: Apple's anchor, the Developer ID intermediate, the
/// Developer ID Application leaf, and the team in the leaf's subject.
pub fn requirement(team: &str) -> String {
    format!(
        "anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and \
         certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"{team}\""
    )
}

fn unescape_xml(text: &str) -> String {
    text.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// The `<string>` values of `key` in a property list, in order.
fn plist_strings(plist: &str, key: &str) -> Vec<String> {
    let tag = format!("<key>{key}</key>");
    plist
        .match_indices(&tag)
        .filter_map(|(at, _)| {
            let rest = plist[at + tag.len()..].trim_start().strip_prefix("<string>")?;
            Some(unescape_xml(&rest[..rest.find("</string>")?]))
        })
        .collect()
}

/// Where `hdiutil attach -plist` mounted the image (under `root`), and the
/// device to detach if it names no mount point.
pub fn attached(plist: &str, root: &Path) -> (Option<PathBuf>, Option<String>) {
    let mount = plist_strings(plist, "mount-point").into_iter().map(PathBuf::from).find(|p| p.starts_with(root));
    let device = plist_strings(plist, "dev-entry").into_iter().find(|d| d.starts_with("/dev/disk"));
    (mount, device)
}

/// The one app at the top of the disk image, out of the image's entries.
pub fn single_app(entries: &[PathBuf]) -> Result<PathBuf, String> {
    let apps: Vec<&PathBuf> =
        entries.iter().filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("app"))).collect();
    match apps.as_slice() {
        [app] => Ok(app.to_path_buf()),
        [] => Err("the disk image holds no app".into()),
        _ => Err("the disk image holds more than one app".into()),
    }
}

/// The new app has to be this Peek, at the version the release names.
pub fn check_identity(new_id: &str, new_version: &str, own_id: &str, version: &str) -> Result<(), String> {
    if new_id != own_id {
        return Err(format!("the app in the disk image is {new_id:?}, not {own_id:?}; nothing was installed"));
    }
    if new_version != version {
        return Err(format!(
            "the app in the disk image is version {new_version}, not {version}; nothing was installed"
        ));
    }
    Ok(())
}

fn hidden_sibling(app: &Path, what: &str, pid: u32) -> PathBuf {
    let name = app.file_name().unwrap_or_default().to_string_lossy();
    app.with_file_name(format!(".{name}.{what}-{pid}"))
}

/// Where the new app is copied before the swap: `.Peek.app.update-<pid>` next to Peek.app.
pub fn staging_path(app: &Path, pid: u32) -> PathBuf {
    hidden_sibling(app, "update", pid)
}

/// Where the previous app waits for deletion when the swap takes two renames.
pub fn backup_path(app: &Path, pid: u32) -> PathBuf {
    hidden_sibling(app, "old", pid)
}

/// What an earlier update left next to `app`: its staging copies and backups,
/// with the process that made each, so a running update's are left alone.
pub fn leftovers(app: &Path) -> Vec<(PathBuf, u32)> {
    let (Some(dir), Some(name)) = (app.parent(), app.file_name()) else { return Vec::new() };
    let name = name.to_string_lossy();
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().into_owned();
            let rest = file.strip_prefix(&format!(".{name}."))?;
            let pid = rest.strip_prefix("update-").or_else(|| rest.strip_prefix("old-"))?.parse().ok()?;
            Some((e.path(), pid))
        })
        .collect()
}

/// Exchange two paths in one step (`renamex_np` with `RENAME_SWAP`), where
/// the file system can. `None` where it cannot.
#[cfg(target_os = "macos")]
fn exchange(a: &Path, b: &Path) -> Option<std::io::Result<()>> {
    use std::os::unix::ffi::OsStrExt;
    let a = std::ffi::CString::new(a.as_os_str().as_bytes()).ok()?;
    let b = std::ffi::CString::new(b.as_os_str().as_bytes()).ok()?;
    // SAFETY: both are NUL-terminated paths that outlive the call.
    if unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) } == 0 {
        return Some(Ok(()));
    }
    let e = std::io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::ENOTSUP | libc::EINVAL | libc::EXDEV) => None,
        _ => Some(Err(e)),
    }
}

#[cfg(not(target_os = "macos"))]
fn exchange(_: &Path, _: &Path) -> Option<std::io::Result<()>> {
    None
}

/// Put the complete bundle at `staged` in place of `app`, all or nothing. On
/// success the previous app is deleted; on failure `app` is what it was and
/// `staged` is gone. `atomic` tries the one-step exchange first; without it,
/// or where the file system has none, `app` is renamed to `backup` and
/// `staged` to `app`, and a failure in between renames it back.
pub fn swap_bundle(app: &Path, staged: &Path, backup: &Path, atomic: bool) -> Result<(), String> {
    let fail = |what: &str, e: std::io::Error| {
        let _ = std::fs::remove_dir_all(staged);
        Err(format!("{what} {}: {e}", app.display()))
    };
    if !app.is_dir() {
        return fail("cannot replace", std::io::ErrorKind::NotFound.into());
    }
    let previous = match atomic.then(|| exchange(staged, app)).flatten() {
        Some(Ok(())) => staged,
        Some(Err(e)) => return fail("cannot replace", e),
        None => {
            if backup.exists() {
                let _ = std::fs::remove_dir_all(backup);
            }
            if let Err(e) = std::fs::rename(app, backup) {
                return fail("cannot set aside", e);
            }
            if let Err(e) = std::fs::rename(staged, app) {
                if let Err(r) = std::fs::rename(backup, app) {
                    let _ = std::fs::remove_dir_all(staged);
                    return Err(format!(
                        "cannot replace {} ({e}), and the previous app could not be put back ({r}); it is at {}",
                        app.display(),
                        backup.display()
                    ));
                }
                return fail("cannot replace", e);
            }
            backup
        }
    };
    // The running Peek keeps its open files; the next start retries what is left.
    if let Err(e) = std::fs::remove_dir_all(previous) {
        tracing::warn!("update: cannot remove the previous app at {}: {e}", previous.display());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Apple's tools

#[cfg(target_os = "macos")]
fn run(tool: &str, args: &[&std::ffi::OsStr]) -> Result<std::process::Output, String> {
    std::process::Command::new(tool)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {tool}: {e}"))
}

#[cfg(target_os = "macos")]
fn os(s: &str) -> &std::ffi::OsStr {
    s.as_ref()
}

/// Run a check; its failure, with what the tool said, is the error.
#[cfg(target_os = "macos")]
fn must_pass(tool: &str, args: &[&std::ffi::OsStr], what: &str) -> Result<(), String> {
    let out = run(tool, args)?;
    if out.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&out.stderr);
    let said = said.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("no reason given");
    Err(format!("{what} ({said}); nothing was installed"))
}

/// The team that signed `app` with a Developer ID, `None` if it is ad-hoc signed or unsigned.
#[cfg(target_os = "macos")]
pub fn signing_team(app: &Path) -> Option<String> {
    let out = run(tool::CODESIGN, &[os("-dv"), os("--verbose=2"), app.as_os_str()]).ok()?;
    // codesign describes the signature on stderr.
    out.status.success().then(|| team_id(&String::from_utf8_lossy(&out.stderr))).flatten()
}

/// One string out of an app's Info.plist.
#[cfg(target_os = "macos")]
fn info(app: &Path, key: &str) -> Result<String, String> {
    let plist = app.join("Contents/Info.plist");
    let out = run(tool::PLISTBUDDY, &[os("-c"), os(&format!("Print :{key}")), plist.as_os_str()])?;
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && !value.is_empty() { Ok(value) } else { Err(format!("{} has no {key}", app.display())) }
}

/// Whether the volume `path` is on is mounted read-only.
#[cfg(target_os = "macos")]
pub fn read_only_volume(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: statfs fills the zeroed struct it is given; the path is NUL-terminated.
    unsafe {
        let mut fs: libc::statfs = std::mem::zeroed();
        libc::statfs(c.as_ptr(), &mut fs) == 0 && fs.f_flags & libc::MNT_RDONLY as u32 != 0
    }
}

/// Whether process `pid` is still running.
#[cfg(target_os = "macos")]
pub fn alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    // SAFETY: signal 0 only asks whether the process exists.
    unsafe { libc::kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

/// A disk image attached read-only and hidden from Finder; detached when dropped.
#[cfg(target_os = "macos")]
struct Mounted {
    /// What `hdiutil detach` is given: the mount point, or the device.
    target: std::ffi::OsString,
    path: Option<PathBuf>,
}

#[cfg(target_os = "macos")]
impl Mounted {
    fn attach(dmg: &Path, root: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
        let out = run(
            tool::HDIUTIL,
            &[
                os("attach"),
                os("-nobrowse"),
                os("-readonly"),
                os("-noautoopen"),
                os("-mountrandom"),
                root.as_os_str(),
                os("-plist"),
                dmg.as_os_str(),
            ],
        )?;
        let (path, device) = attached(&String::from_utf8_lossy(&out.stdout), root);
        let target = match (&path, device) {
            (Some(p), _) => p.clone().into_os_string(),
            (None, Some(d)) => d.into(),
            (None, None) => {
                let said = String::from_utf8_lossy(&out.stderr);
                return Err(format!("the disk image could not be opened ({})", said.trim()));
            }
        };
        let mounted = Self { target, path };
        if mounted.path.is_none() {
            return Err("the disk image has no volume Peek can read".into());
        }
        Ok(mounted)
    }
}

#[cfg(target_os = "macos")]
impl Drop for Mounted {
    fn drop(&mut self) {
        let detach = |force: bool| {
            let mut args = vec![os("detach"), self.target.as_os_str()];
            if force {
                args.push(os("-force"));
            }
            run(tool::HDIUTIL, &args).is_ok_and(|o| o.status.success())
        };
        if !detach(false) && !detach(true) {
            tracing::warn!("update: could not detach {:?}", self.target);
        }
    }
}

/// Every signature check the new app has to pass: intact and complete,
/// a Developer ID of `team`, and notarized as far as Gatekeeper is concerned.
#[cfg(target_os = "macos")]
fn verify_signature(app: &Path, team: &str, gatekeeper: bool) -> Result<(), String> {
    let path = app.as_os_str();
    must_pass(
        tool::CODESIGN,
        &[os("--verify"), os("--deep"), os("--strict"), path],
        "the new app's signature is not valid",
    )?;
    let req = format!("={}", requirement(team));
    must_pass(
        tool::CODESIGN,
        &[os("--verify"), os("-R"), os(&req), path],
        &format!("the new app is not signed with the Developer ID of team {team}"),
    )?;
    if gatekeeper {
        must_pass(
            tool::SPCTL,
            &[os("--assess"), os("--type"), os("execute"), path],
            "Gatekeeper does not accept the new app as notarized",
        )?;
    }
    Ok(())
}

/// Install the app in the verified disk image `dmg` in place of `app`, which
/// must be signed with a Developer ID. `work` is this update's own folder.
#[cfg(target_os = "macos")]
pub fn install(app: &Path, dmg: &Path, version: &str, work: &Path) -> Result<(), String> {
    let team = signing_team(app).ok_or("this Peek is not signed with a Developer ID, so it does not replace itself")?;
    let own_id = info(app, "CFBundleIdentifier")?;
    let image = Mounted::attach(dmg, &work.join("mnt"))?;
    let root = image.path.as_deref().ok_or("the disk image has no volume")?;
    let entries: Vec<PathBuf> = std::fs::read_dir(root)
        .map_err(|e| format!("cannot read the disk image: {e}"))?
        .flatten()
        .map(|e| e.path())
        .collect();
    let new = single_app(&entries)?;
    check_identity(&info(&new, "CFBundleIdentifier")?, &info(&new, "CFBundleShortVersionString")?, &own_id, version)?;
    verify_signature(&new, &team, true)?;

    // Copied next to the installed app, so the swap is a rename on one volume.
    let pid = std::process::id();
    let staged = staging_path(app, pid);
    let _ = std::fs::remove_dir_all(&staged);
    let copied = must_pass(tool::DITTO, &[new.as_os_str(), staged.as_os_str()], "the new app could not be copied")
        // The copy is what goes in place: it has to be complete and intact too.
        .and_then(|()| verify_signature(&staged, &team, false));
    drop(image);
    if let Err(e) = copied {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(e);
    }
    swap_bundle(app, &staged, &backup_path(app, pid), true)
}

/// Start a helper that waits for this Peek to exit and then opens `app`, the
/// new version, through Launch Services. It has its own session, no
/// terminal and no output; `note` is passed to the new Peek's environment.
#[cfg(target_os = "macos")]
pub fn relaunch_after_exit(app: &Path, note: &str) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    // `open --env` is newer than some systems Peek runs on; without it, Peek starts without the note.
    const SCRIPT: &str = r#"n=0; while kill -0 "$1" 2>/dev/null; do [ $n -ge 3000 ] && exit 0; n=$((n+1)); /bin/sleep 0.2; done
/usr/bin/open -n --env "$2" "$3" 2>/dev/null || /usr/bin/open -n "$3""#;
    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(SCRIPT)
        .arg("peek-relaunch")
        .arg(std::process::id().to_string())
        .arg(note)
        .arg(app)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .current_dir("/")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: setsid is async-signal-safe and touches no memory of this process.
    unsafe {
        cmd.pre_exec(|| if libc::setsid() == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) });
    }
    let child = cmd.spawn().map_err(|e| format!("cannot start the new version: {e}"))?;
    // It outlives Peek on purpose; nothing waits for it here.
    drop(child);
    Ok(())
}
