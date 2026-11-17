//! The platform's way of showing and opening things: Finder on macOS, Explorer on Windows, the
//! desktop's file manager (`xdg-open`) on Linux. Nothing here waits or fails loudly: if the
//! system can't do it, nothing happens.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

/// What the file manager is called, for "Show in …".
pub const FILE_MANAGER: &str = if cfg!(target_os = "macos") {
    "Finder"
} else if cfg!(windows) {
    "Explorer"
} else {
    "Files"
};

/// "Show in Finder" (or Explorer, or Files).
pub fn show_in() -> String {
    format!("Show in {FILE_MANAGER}")
}

/// Show `path` in the file manager, selected where the platform can.
pub fn reveal(path: &Path) {
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // explorer wants `/select,"<path>"` as written, not quoted as one argument
        let _ = Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display())).spawn();
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let dir = if path.is_dir() { path } else { path.parent().unwrap_or(path) };
        let _ = Command::new("xdg-open").arg(dir).spawn();
    }
}

/// Open a file, folder or URL with its default app.
pub fn open(target: impl AsRef<OsStr>) {
    let t = target.as_ref();
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(t).spawn();
    #[cfg(windows)]
    let _ = Command::new("explorer").arg(t).spawn();
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = Command::new("xdg-open").arg(t).spawn();
}

/// Open a file in a text editor (the default one on macOS and Linux, Notepad on Windows).
pub fn open_text(path: &Path) {
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg("-t").arg(path).spawn();
    #[cfg(windows)]
    let _ = Command::new("notepad").arg(path).spawn();
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = Command::new("xdg-open").arg(path).spawn();
}

/// A desktop notification (Notification Center on macOS, `notify-send` on Linux; Windows shows
/// only the in-app message for now).
pub fn notify(title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    {
        let esc = |t: &str| t.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!("display notification \"{}\" with title \"only3d\" subtitle \"{}\"", esc(body), esc(title));
        let _ = Command::new("osascript").arg("-e").arg(script).spawn();
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = Command::new("notify-send").arg("--app-name=only3d").arg(title).arg(body).spawn();
    #[cfg(windows)]
    let _ = (title, body);
}
