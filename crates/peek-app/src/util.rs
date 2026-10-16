use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn human_rate(bytes_per_sec: f64) -> String {
    match bytes_per_sec {
        b if b >= 1e6 => format!("{:.1} MB/s", b / 1e6),
        b if b >= 1e3 => format!("{:.0} kB/s", b / 1e3),
        b => format!("{b:.0} B/s"),
    }
}

pub fn human_bytes(b: u64) -> String {
    match b {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.0} kB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

pub fn clock(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{:02}:{:02}", s / 60, s % 60) }
}

/// Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `2026-10-16_14-03-27` in UTC, safe for file names.
pub fn file_stamp() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (y, m, d) = civil(secs.div_euclid(86_400));
    let t = secs.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02}_{:02}-{:02}-{:02}", t / 3600, t / 60 % 60, t % 60)
}

pub fn ago(unix: u64) -> String {
    let now = peek_store::now_unix();
    let s = now.saturating_sub(unix);
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86_400 => format!("{} h ago", s / 3600),
        _ => format!("{} d ago", s / 86_400),
    }
}

/// Open a file with its default application.
pub fn open_file(path: &std::path::Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

/// Show a file selected in the file manager (its folder elsewhere).
pub fn reveal_file(path: &std::path::Path) {
    #[cfg(windows)]
    {
        let mut arg = std::ffi::OsString::from("/select,");
        arg.push(path);
        let _ = std::process::Command::new("explorer").arg(arg).spawn();
    }
    #[cfg(not(windows))]
    if let Some(dir) = path.parent() {
        open_file(dir);
    }
}

pub fn safe_file_name(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_742), (2026, 10, 16));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }
}

/// Current text on the OS clipboard, if any.
pub fn read_clipboard_text() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok().filter(|t| !t.is_empty())
}
