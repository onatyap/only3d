//! Crash reports. A panic anywhere in the program writes a short report to
//! `~/.only3d/crashes` (version, system, where and what, and a backtrace; no project data), and
//! the next window offers it, so a crash on a machine with no terminal isn't silent.

use std::path::{Path, PathBuf};

/// Where reports go.
pub fn dir() -> PathBuf {
    crate::config_dir().join("crashes")
}

/// Write a report for every panic, then let the usual message print. Call once, at start.
pub fn install(version: &'static str) {
    let usual = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let what = match info.payload().downcast_ref::<&str>() {
            Some(s) => s.to_string(),
            None => info.payload().downcast_ref::<String>().cloned().unwrap_or_else(|| "a panic with no message".into()),
        };
        let place = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let _ = write_report(&dir(), version, &what, &place);
        usual(info);
    }));
}

/// The report's text and file, in `dir`. Returns the file.
pub fn write_report(dir: &Path, version: &str, what: &str, place: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let ms = crate::now_ms();
    let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
    let backtrace = std::backtrace::Backtrace::force_capture();
    let text = format!(
        "only3d {version} stopped with an error.\n\nwhat: {what}\nwhere: {place}\nthread: {thread}\nsystem: {} {}\nwhen: {} (UTC)\n\nbacktrace:\n{backtrace}\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        utc(ms),
    );
    let file = dir.join(format!("crash-{ms}.txt"));
    std::fs::write(&file, text)?;
    Ok(file)
}

/// The newest report the user hasn't been shown, with its one-line "what"; marks it shown.
pub fn unseen() -> Option<(PathBuf, String)> {
    unseen_in(&dir())
}

pub fn unseen_in(dir: &Path) -> Option<(PathBuf, String)> {
    let newest = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("crash-")) && p.extension().is_some_and(|e| e == "txt"))
        .max()?;
    let name = newest.file_name()?.to_string_lossy().to_string();
    let seen = dir.join("last-shown");
    if std::fs::read_to_string(&seen).is_ok_and(|s| s.trim() == name) {
        return None;
    }
    let _ = std::fs::write(&seen, &name);
    let what = std::fs::read_to_string(&newest).ok()?.lines().find_map(|l| l.strip_prefix("what: ").map(str::to_string)).unwrap_or_default();
    Some((newest, what))
}

/// "2026-11-18 21:04:05" from Unix milliseconds (UTC).
fn utc(ms: u128) -> String {
    let secs = (ms / 1000) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // days since 1970-01-01 to a civil date (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_is_offered_once() {
        let d = std::env::temp_dir().join(format!("o3d_crash_{}_{}", std::process::id(), crate::now_ms()));
        assert!(unseen_in(&d).is_none(), "no folder, nothing to show");
        let f = write_report(&d, "9.9.9", "index out of bounds", "src/x.rs:3").unwrap();
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.starts_with("only3d 9.9.9 stopped") && text.contains("where: src/x.rs:3") && text.contains("backtrace:"));
        assert_eq!(unseen_in(&d), Some((f, "index out of bounds".to_string())));
        assert!(unseen_in(&d).is_none(), "shown once");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn utc_dates() {
        assert_eq!(utc(0), "1970-01-01 00:00:00");
        assert_eq!(utc(1_709_210_096_000), "2024-02-29 12:34:56");
    }
}
