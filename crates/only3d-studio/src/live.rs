//! Live session socket. A running only3d window (or `only3d serve`) listens on a local
//! Unix-domain socket (Windows 10 and later have them too, through `uds_windows`); the MCP server and CLI attach to it, so an agent's edits appear on the user's
//! screen as they happen and the agent sees what the user selected.
//!
//! Protocol: one JSON object per line.
//!   → {"id": 1, "tool": "add_object", "args": {...}, "actor": "agent"}
//!   ← {"id": 1, "ok": true, "text": "...", "data": {...}, "images": [{"png_base64": "...", "caption": "..."}]}
//!   ← {"id": 1, "ok": false, "error": {"code": "...", "message": "...", "hint": "..."}}

use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
pub use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(windows)]
pub use uds_windows::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use serde_json::{json, Value};

use crate::approval::{gated_call, ApprovalRequest, Decision};
use crate::tools::{Actor, ImageOut, ToolError, ToolOutput};
use crate::Studio;

pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("ONLY3D_SOCKET") {
        return PathBuf::from(p);
    }
    let p = crate::config_dir().join("live.sock");
    // Unix socket paths are limited to ~104 bytes on macOS
    if p.as_os_str().len() < 100 {
        p
    } else {
        let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "user".into());
        if cfg!(unix) {
            PathBuf::from(format!("/tmp/only3d-{user}.sock"))
        } else {
            std::env::temp_dir().join(format!("only3d-{user}.sock"))
        }
    }
}

pub fn output_to_json(id: &Value, r: &Result<ToolOutput, ToolError>) -> Value {
    match r {
        Ok(o) => json!({
            "id": id, "ok": true, "text": o.text, "data": o.data,
            "images": o.images.iter().map(|i| json!({"png_base64": base64::engine::general_purpose::STANDARD.encode(&i.png), "caption": i.caption, "mime": i.mime})).collect::<Vec<_>>()
        }),
        Err(e) => json!({"id": id, "ok": false, "error": e}),
    }
}

pub fn json_to_output(v: &Value) -> Result<ToolOutput, ToolError> {
    if v.get("ok").and_then(|x| x.as_bool()) == Some(true) {
        let images = v
            .get("images")
            .and_then(|i| i.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|i| {
                        let png = base64::engine::general_purpose::STANDARD.decode(i.get("png_base64")?.as_str()?).ok()?;
                        Some(ImageOut { png, caption: i.get("caption").and_then(|c| c.as_str()).unwrap_or("").to_string(), mime: i.get("mime").and_then(|c| c.as_str()).unwrap_or("image/png").to_string() })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(ToolOutput { text: v.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string(), data: v.get("data").cloned().unwrap_or(Value::Null), images })
    } else {
        let e = v.get("error").cloned().unwrap_or(Value::Null);
        Err(serde_json::from_value(e.clone()).unwrap_or_else(|_| ToolError::new("live_error", e.to_string(), "")))
    }
}

/// Asks the user to approve an agent's request (see [`crate::approval`]); blocks until answered.
pub type Approver = Arc<dyn Fn(ApprovalRequest) -> Decision + Send + Sync>;

/// Serve a shared session on the socket (spawns a thread; returns immediately). Without an
/// approver (`only3d serve`), gated printer actions are refused.
pub fn serve(studio: Arc<Mutex<Studio>>, path: &Path) -> std::io::Result<()> {
    serve_with(studio, path, None)
}

/// [`serve`], asking `approver` (the window's dialog) before gated printer actions.
pub fn serve_with(studio: Arc<Mutex<Studio>>, path: &Path, approver: Option<Approver>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if path.exists() {
        // stale socket from a crashed session?
        if UnixStream::connect(path).is_ok() {
            return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, format!("another only3d session is live at {}", path.display())));
        }
        let _ = std::fs::remove_file(path);
    }
    let listener = UnixListener::bind(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    std::thread::Builder::new().name("only3d-live".into()).spawn(move || {
        for conn in listener.incoming().flatten() {
            let st = studio.clone();
            let ap = approver.clone();
            std::thread::spawn(move || handle(st, conn, ap));
        }
    })?;
    Ok(())
}

fn handle(studio: Arc<Mutex<Studio>>, conn: UnixStream, approver: Option<Approver>) {
    let mut w = match conn.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    for line in BufReader::new(conn).lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Value>(&line) {
            Ok(req) => {
                let id = req.get("id").cloned().unwrap_or(Value::Null);
                let tool = req.get("tool").and_then(|t| t.as_str()).unwrap_or("");
                let args = req.get("args").cloned().unwrap_or(json!({}));
                let actor: Actor = req.get("actor").cloned().and_then(|a| serde_json::from_value(a).ok()).unwrap_or(Actor::Agent);
                let requester = req.get("client").and_then(|c| c.as_str()).unwrap_or("an agent").to_string();
                // the lock is taken per tool call and never held while the user decides
                let mut call = |t: &str, a: Value| match studio.lock() {
                    Ok(mut s) => {
                        if t == "__manifest" {
                            Ok(ToolOutput::text("manifest").with_data(s.registry().manifest()))
                        } else {
                            s.call(t, a, actor)
                        }
                    }
                    Err(_) => Err(ToolError::new("session_poisoned", "the live session crashed", "restart only3d")),
                };
                // Everything arriving here comes from another program, whatever actor it claims:
                // the window's own actions never use the socket. So the gate always applies.
                let mut approve = |r: ApprovalRequest| match &approver {
                    Some(f) => f(r),
                    None => Decision::Unavailable,
                };
                let mut note = |what: &str, ok: bool| {
                    if let Ok(mut s) = studio.lock() {
                        s.note(Actor::User, "approval", what, ok);
                    }
                };
                // waiting calls must not hold the session lock (the window would stall)
                let r = match tool {
                    "wait_for_event" => match studio.lock() {
                        Ok(s) => {
                            let log = s.events.clone();
                            drop(s);
                            crate::events::wait_tool(&log, &args)
                        }
                        Err(_) => Err(ToolError::new("session_poisoned", "the live session crashed", "restart only3d")),
                    },
                    "printer_wait" => crate::printer::wait_unlocked(&studio, args, actor),
                    _ => gated_call(tool, args, &requester, &mut call, &mut approve, &mut note),
                };
                output_to_json(&id, &r)
            }
            Err(e) => json!({"id": null, "ok": false, "error": {"code": "bad_request", "message": e.to_string(), "hint": "send one JSON object per line"}}),
        };
        if writeln!(w, "{resp}").is_err() {
            break;
        }
    }
}

/// Client side of the live socket.
pub struct LiveClient {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: u64,
    /// Shown to the user in approval requests (e.g. the MCP client's name).
    pub client: Option<String>,
}

impl LiveClient {
    pub fn connect(path: &Path) -> Option<LiveClient> {
        let stream = UnixStream::connect(path).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(600))).ok()?;
        let reader = BufReader::new(stream.try_clone().ok()?);
        Some(LiveClient { stream, reader, next_id: 1, client: None })
    }

    pub fn call(&mut self, tool: &str, args: Value, actor: Actor) -> Result<ToolOutput, ToolError> {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({"id": id, "tool": tool, "args": args, "actor": actor, "client": self.client});
        writeln!(self.stream, "{req}").map_err(|e| ToolError::new("live_disconnected", e.to_string(), "the only3d window was closed"))?;
        let mut line = String::new();
        self.reader.read_line(&mut line).map_err(|e| ToolError::new("live_disconnected", e.to_string(), "the only3d window was closed"))?;
        if line.is_empty() {
            return Err(ToolError::new("live_disconnected", "connection closed", "the only3d window was closed"));
        }
        let v: Value = serde_json::from_str(&line).map_err(|e| ToolError::new("live_protocol", e.to_string(), ""))?;
        json_to_output(&v)
    }
}
