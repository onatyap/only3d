//! Which live session a client (the MCP server, `only3d call`) works in, never by luck: the one
//! open session is used; with several, the user says which (by id, shown on each window's agent
//! link chip); with none, a window is opened. Work without a window happens only when asked
//! for (`--no-live`, `--project`, `session {headless: true}`), so an agent can't build something
//! the user never sees.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use only3d_studio::live::{self, LiveClient, SessionInfo};
use only3d_studio::{Actor, ToolError};
use serde_json::json;

pub enum Found {
    One(SessionInfo),
    Several(Vec<SessionInfo>),
    None,
}

/// The sessions a client may use: only the one on ONLY3D_SOCKET when that is set.
pub fn open_sessions() -> Vec<SessionInfo> {
    match live::explicit_socket() {
        Some(p) => live::session_at(&p).into_iter().collect(),
        None => live::list_sessions(),
    }
}

/// The session to use: the one named `id`, else the only one open.
pub fn find(id: Option<&str>) -> Result<Found, ToolError> {
    let all = open_sessions();
    if let Some(id) = id {
        let want = id.trim().to_ascii_lowercase();
        return match all.iter().find(|s| s.id == want) {
            Some(s) => Ok(Found::One(s.clone())),
            None => Err(not_found(id, &all)),
        };
    }
    let mut all = all;
    Ok(match all.len() {
        0 => Found::None,
        1 => Found::One(all.remove(0)),
        _ => Found::Several(all),
    })
}

pub fn listing(all: &[SessionInfo]) -> String {
    all.iter().map(|s| format!("- {}", s.label())).collect::<Vec<_>>().join("\n")
}

/// Several sessions are open: the caller has to say which. `how` says how (a tool call, a flag).
pub fn choice_needed(all: &[SessionInfo], how: &str) -> ToolError {
    ToolError::new(
        "session_choice_needed",
        format!("{} only3d sessions are open, so it isn't clear which one the user means:\n{}", all.len(), listing(all)),
        format!("nothing was done; ask the user which one (each window shows its id on its agent link chip, top right), then {how}"),
    )
}

fn not_found(id: &str, all: &[SessionInfo]) -> ToolError {
    if all.is_empty() {
        ToolError::new("session_not_found", format!("no only3d session {id} is open; none is"), "ask the user to open only3d, or whether to open a new window")
    } else {
        ToolError::new("session_not_found", format!("no only3d session {id} is open. Open ones:\n{}", listing(all)), "ask the user which one they mean")
    }
}

/// Whether this process can open a window, and if not, why.
pub fn can_open_window() -> Result<(), String> {
    if !cfg!(feature = "gui") {
        return Err("this only3d build has no window".into());
    }
    if std::env::var_os("ONLY3D_NO_WINDOW").is_some_and(|v| !v.is_empty() && v != "0") {
        return Err("ONLY3D_NO_WINDOW is set".into());
    }
    if let Some(p) = live::explicit_socket() {
        return Err(format!("ONLY3D_SOCKET names the one session to use ({})", p.display()));
    }
    if cfg!(all(unix, not(target_os = "macos"))) && std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return Err("there is no display here".into());
    }
    Ok(())
}

/// Open a new only3d window (this same program, `gui`) and wait until it is listed.
pub fn open_window() -> Result<SessionInfo, ToolError> {
    can_open_window().map_err(|why| ToolError::new("no_window", format!("can't open an only3d window: {why}"), "ask the user to open only3d themselves, then call again"))?;
    let exe = std::env::current_exe().map_err(|e| ToolError::new("window_failed", format!("can't find the only3d program: {e}"), "ask the user to open only3d themselves, then call again"))?;
    let mut child = spawn_window(&exe).map_err(|e| ToolError::new("window_failed", format!("couldn't start the only3d window: {e}"), "ask the user to open only3d themselves, then call again"))?;
    let pid = child.id();
    let t0 = Instant::now();
    let found = loop {
        if let Some(s) = live::list_sessions().into_iter().find(|s| s.pid == pid) {
            break Ok(s);
        }
        if let Ok(Some(status)) = child.try_wait() {
            break Err(ToolError::new("window_failed", format!("the only3d window closed as it started ({status})"), "ask the user to open only3d themselves, then call again"));
        }
        if t0.elapsed() > Duration::from_secs(30) {
            break Err(ToolError::new("window_failed", "the only3d window didn't come up within 30 s", "ask the user whether a window opened; if one did, call again"));
        }
        std::thread::sleep(Duration::from_millis(150));
    };
    // reap it when it closes (a long-running MCP server would otherwise keep a zombie)
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    found
}

/// The window outlives whoever opened it: its own process group (no hangup when an agent's
/// terminal or app closes), and on Windows no console.
fn spawn_window(exe: &std::path::Path) -> std::io::Result<std::process::Child> {
    let mut cmd = Command::new(exe);
    cmd.arg("gui").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        // out of the agent app's job, if it allows that, so closing the app leaves the window
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        if let Ok(c) = cmd.spawn() {
            return Ok(c);
        }
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn()
}

/// Connect to `info` and attach as `client` (the window shows the name until we disconnect).
pub fn attach(info: &SessionInfo, client: &str) -> Result<LiveClient, ToolError> {
    let mut c = LiveClient::connect(&info.socket).ok_or_else(|| ToolError::new("window_closed", format!("only3d {} closed just now", info.label()), "ask the user whether to reopen only3d"))?;
    c.client = Some(client.to_string());
    c.call("__attach", json!({}), Actor::Agent)?;
    Ok(c)
}
