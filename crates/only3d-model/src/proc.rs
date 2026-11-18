//! Running helper programs (OpenSCAD, ffmpeg, curl, claude). On Windows a console program
//! started from the window gets a console window of its own; these never show one.

use std::ffi::OsStr;
use std::process::Command;

/// `Command::new(program)`, without a console window on Windows.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW
        c.creation_flags(0x0800_0000);
    }
    c
}
