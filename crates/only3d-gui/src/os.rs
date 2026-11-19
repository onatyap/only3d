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

/// Whether the project has unsaved changes, for the quit question AppKit asks
/// ([`guard_quit`]), which comes outside any frame.
static UNSAVED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Say whether the project has unsaved changes: on macOS the window's close button shows a dot,
/// and quitting from the Dock or at log out asks first.
pub fn set_edited(edited: bool) {
    UNSAVED.store(edited, std::sync::atomic::Ordering::Relaxed);
    #[cfg(target_os = "macos")]
    if let Some(mtm) = objc2::MainThreadMarker::new() {
        for w in objc2_app_kit::NSApplication::sharedApplication(mtm).windows().iter() {
            w.setDocumentEdited(edited);
        }
    }
}

/// macOS asks the app before it quits from the Dock, an Apple Event, log out or shutdown, and
/// winit answers "now" without asking the window. Teach its delegate to answer instead: with
/// unsaved changes, cancel and close the window, which asks Save / Don't Save / Cancel
/// (`App::guard_close`); otherwise quit as before. ⌘Q is a menu item that closes the window,
/// so it never comes here.
#[cfg(target_os = "macos")]
pub fn guard_quit(ctx: &eframe::egui::Context) {
    use objc2::runtime::{AnyObject, Imp, Sel};
    use std::sync::OnceLock;

    static CTX: OnceLock<eframe::egui::Context> = OnceLock::new();
    const TERMINATE_CANCEL: usize = 0;
    const TERMINATE_NOW: usize = 1;

    extern "C-unwind" fn should_terminate(_this: &AnyObject, _cmd: Sel, _sender: *mut AnyObject) -> usize {
        if !UNSAVED.load(std::sync::atomic::Ordering::Relaxed) {
            return TERMINATE_NOW;
        }
        if let Some(ctx) = CTX.get() {
            ctx.send_viewport_cmd(eframe::egui::ViewportCommand::Close);
            ctx.request_repaint();
        }
        TERMINATE_CANCEL
    }

    if CTX.set(ctx.clone()).is_err() {
        return;
    }
    let Some(mtm) = objc2::MainThreadMarker::new() else { return };
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    let Some(delegate) = app.delegate() else { return };
    let obj: &AnyObject = delegate.as_ref();
    let class = obj.class();
    let f: extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> usize = should_terminate;
    // SAFETY: the function matches the selector's signature, `-(NSApplicationTerminateReply)
    // applicationShouldTerminate:(NSApplication *)sender` (NSUInteger is `Q` on 64-bit macOS),
    // and class_addMethod leaves a class that already has the method unchanged.
    let added = unsafe {
        let imp = std::mem::transmute::<extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> usize, Imp>(f);
        objc2::ffi::class_addMethod(class as *const _ as *mut _, objc2::sel!(applicationShouldTerminate:), imp, c"Q@:@".as_ptr())
    };
    if added.as_bool() {
        // AppKit notes what its delegate answers when it is set: set it again so it asks
        app.setDelegate(Some(&delegate));
    }
}

#[cfg(not(target_os = "macos"))]
pub fn guard_quit(_ctx: &eframe::egui::Context) {}
