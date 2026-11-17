//! `only3d-app`: what the Windows Start menu and desktop shortcut open. It starts the only3d
//! window without a console window next to it. The main `only3d` program stays a console program,
//! so terminals and agents can use its CLI and MCP server (`only3d mcp` talks over stdio).
//! On macOS and Linux the packages start `only3d` directly; this works there too.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    let name = if cfg!(windows) { "only3d.exe" } else { "only3d" };
    let Some(exe) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(name))) else {
        return;
    };
    let mut cmd = std::process::Command::new(exe);
    // a file opened with the app (a double-clicked project) goes along
    cmd.arg("gui").args(std::env::args_os().skip(1));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let _ = cmd.spawn();
}
