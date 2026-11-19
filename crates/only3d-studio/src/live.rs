//! Live sessions. Every only3d window (and `only3d serve`) is a session with a short id, like
//! `k7m2`, shown on the window's agent link chip. It listens on its own local Unix-domain socket
//! (Windows 10 and later have them too, through `uds_windows`) and lists itself in
//! [`sessions_dir`]; the MCP server and CLI find it there and attach, so an agent's edits appear
//! on the user's screen as they happen and the agent sees what the user selected.
//!
//! The list is per user, whatever ONLY3D_HOME says: agents find the window the user is looking
//! at even when it runs with another profile. With several open, a client never guesses; it
//! asks which one (by id).
//!
//! Protocol: one JSON object per line.
//!   → {"id": 1, "tool": "add_object", "args": {...}, "actor": "agent", "client": "Codex"}
//!   ← {"id": 1, "ok": true, "text": "...", "data": {...}, "images": [{"png_base64": "...", "caption": "..."}]}
//!   ← {"id": 1, "ok": false, "error": {"code": "...", "message": "...", "hint": "..."}}
//! Besides the tools: `__session` (which session this is, without waiting for a busy one),
//! `__attach` (an agent binds to it for good: the window shows its name) and `__manifest`.

use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
pub use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(windows)]
pub use uds_windows::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::approval::{gated_call, ApprovalRequest, Decision};
use crate::tools::{Actor, ImageOut, ToolError, ToolOutput};
use crate::Studio;

/// Where sessions list themselves (`<id>.json`, with `<id>.sock` beside it). Per user, not per
/// ONLY3D_HOME; ONLY3D_SESSIONS moves it (tests).
pub fn sessions_dir() -> PathBuf {
    match std::env::var_os("ONLY3D_SESSIONS").filter(|v| !v.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => std::env::home_dir().unwrap_or_default().join(".only3d").join("sessions"),
    }
}

/// ONLY3D_SOCKET: one fixed socket (tests, evals). A session served there isn't listed, and a
/// client attaches to that one or to none.
pub fn explicit_socket() -> Option<PathBuf> {
    std::env::var_os("ONLY3D_SOCKET").filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn socket_for(id: &str) -> PathBuf {
    let p = sessions_dir().join(format!("{id}.sock"));
    // Unix socket paths are limited to ~104 bytes on macOS
    if p.as_os_str().len() < 100 {
        return p;
    }
    let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "user".into());
    if cfg!(unix) {
        PathBuf::from(format!("/tmp/only3d-{user}-{id}.sock"))
    } else {
        std::env::temp_dir().join(format!("only3d-{user}-{id}.sock"))
    }
}

/// Letters and digits that can't be mistaken for each other (no 0/o, 1/l/i).
const ID_CHARS: &[u8] = b"23456789abcdefghjkmnpqrstuvwxyz";

fn new_id() -> String {
    use std::hash::BuildHasher;
    // RandomState is seeded randomly per process; the time keeps two in one process apart
    let mut h = std::collections::hash_map::RandomState::new().hash_one((std::process::id(), std::time::SystemTime::now()));
    (0..4)
        .map(|_| {
            let c = ID_CHARS[(h % ID_CHARS.len() as u64) as usize] as char;
            h /= ID_CHARS.len() as u64;
            c
        })
        .collect()
}

/// What a session writes to [`sessions_dir`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    /// "window" or "serve".
    pub kind: String,
    pub pid: u32,
    pub socket: PathBuf,
    pub started_ms: u64,
}

/// A session as a client sees it (from its record and its `__session` answer).
#[derive(Clone, Debug, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub kind: String,
    pub pid: u32,
    pub socket: PathBuf,
    /// The project's name; `None` while the session is too busy to say.
    pub title: Option<String>,
    pub path: Option<PathBuf>,
    pub objects: Option<usize>,
    /// The agents attached to it right now.
    pub agents: Vec<String>,
    /// Its ONLY3D_HOME (profile: printers, spools, settings).
    pub home: Option<PathBuf>,
    #[serde(skip)]
    pub started_ms: u64,
}

impl SessionInfo {
    /// `window k7m2 · bracket (3 objects)`.
    pub fn label(&self) -> String {
        let mut s = format!("{} {}", self.kind, self.id);
        if let Some(t) = &self.title {
            s.push_str(&format!(" · {t}"));
        }
        match self.objects {
            Some(1) => s.push_str(" (1 object)"),
            Some(n) => s.push_str(&format!(" ({n} objects)")),
            None => s.push_str(" (busy)"),
        }
        if !self.agents.is_empty() {
            s.push_str(&format!(", used by {}", self.agents.join(", ")));
        }
        s
    }

    fn fill(&mut self, v: &Value) {
        self.title = v.get("title").and_then(Value::as_str).map(str::to_string);
        self.path = v.get("path").and_then(Value::as_str).map(PathBuf::from);
        self.objects = v.get("objects").and_then(Value::as_u64).map(|n| n as usize);
        self.agents = v.get("agents").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
        self.home = v.get("home").and_then(Value::as_str).map(PathBuf::from);
    }
}

/// Ask the session on `socket` who it is. `None` if nothing listens there; a session too busy
/// to answer within a couple of seconds comes back without its project details.
pub fn probe(socket: &Path) -> Option<Value> {
    let stream = UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut w = stream.try_clone().ok()?;
    writeln!(w, "{}", json!({"id": 0, "tool": "__session"})).ok()?;
    let mut line = String::new();
    match BufReader::new(stream).read_line(&mut line) {
        Ok(n) if n > 0 => serde_json::from_str::<Value>(&line).ok().and_then(|v| v.get("data").cloned()).or(Some(Value::Null)),
        _ => Some(Value::Null),
    }
}

/// The open sessions, oldest first. Records of sessions that are gone (a crash, a kill) are
/// removed on the way.
pub fn list_sessions() -> Vec<SessionInfo> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir(sessions_dir()) else { return out };
    for e in dir.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Some(rec) = std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<SessionRecord>(&t).ok()) else { continue };
        match probe(&rec.socket) {
            None => {
                // a session binds its socket before writing its record, so nothing listening
                // means it is gone
                let _ = std::fs::remove_file(&p);
                let _ = std::fs::remove_file(&rec.socket);
            }
            Some(v) => {
                let mut info = SessionInfo { id: rec.id, kind: rec.kind, pid: rec.pid, socket: rec.socket, title: None, path: None, objects: None, agents: Vec::new(), home: None, started_ms: rec.started_ms };
                info.fill(&v);
                out.push(info);
            }
        }
    }
    out.sort_by_key(|s| (s.started_ms, s.id.clone()));
    out
}

/// The session on an explicit socket (ONLY3D_SOCKET), if one listens there.
pub fn session_at(socket: &Path) -> Option<SessionInfo> {
    let v = probe(socket)?;
    let mut info = SessionInfo {
        id: v.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
        kind: v.get("kind").and_then(Value::as_str).unwrap_or("session").to_string(),
        pid: v.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32,
        socket: socket.to_path_buf(),
        title: None,
        path: None,
        objects: None,
        agents: Vec::new(),
        home: None,
        started_ms: 0,
    };
    info.fill(&v);
    Some(info)
}

/// Shared between a session's connections and its window: who it is and who is attached.
pub struct LiveMeta {
    pub id: String,
    pub kind: String,
    agents: Mutex<Vec<(u64, String)>>,
    next_conn: AtomicU64,
}

impl LiveMeta {
    fn new(kind: &str) -> LiveMeta {
        LiveMeta { id: new_id(), kind: kind.to_string(), agents: Mutex::new(Vec::new()), next_conn: AtomicU64::new(1) }
    }

    /// The agents attached right now (an agent app's MCP server stays attached until it exits).
    pub fn agents(&self) -> Vec<String> {
        let mut v: Vec<String> = self.agents.lock().map(|a| a.iter().map(|(_, n)| n.clone()).collect()).unwrap_or_default();
        v.dedup();
        v
    }

    fn info(&self, studio: &Mutex<Studio>) -> Value {
        let mut v = json!({"id": self.id, "kind": self.kind, "pid": std::process::id(), "home": crate::config_dir(), "agents": self.agents()});
        // never wait for a busy session: a client is only asking who we are
        if let Ok(s) = studio.try_lock() {
            v["title"] = json!(s.project.name);
            v["path"] = json!(s.project_path);
            v["objects"] = json!(s.project.objects.len());
            v["revision"] = json!(s.revision);
        }
        v
    }
}

/// A session being served: removes its record and socket when dropped.
pub struct Hosted {
    pub meta: Arc<LiveMeta>,
    pub socket: PathBuf,
    record: Option<PathBuf>,
}

impl Hosted {
    pub fn id(&self) -> &str {
        &self.meta.id
    }
}

impl Drop for Hosted {
    fn drop(&mut self) {
        if let Some(r) = &self.record {
            let _ = std::fs::remove_file(r);
        }
        let _ = std::fs::remove_file(&self.socket);
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

/// Serve `studio` as a live session (`kind`: "window" or "serve") on its own socket and list it,
/// or on ONLY3D_SOCKET unlisted. Spawns a thread and returns at once; keep the [`Hosted`]: dropping
/// it takes the session off the list. `approver` (the window's dialog) answers gated printer
/// actions; without one (`only3d serve`) they are refused.
pub fn host(studio: Arc<Mutex<Studio>>, kind: &str, approver: Option<Approver>) -> std::io::Result<Hosted> {
    let meta = Arc::new(LiveMeta::new(kind));
    if let Some(path) = explicit_socket() {
        listen(studio, &path, approver, meta.clone())?;
        return Ok(Hosted { meta, socket: path, record: None });
    }
    let dir = sessions_dir();
    std::fs::create_dir_all(&dir)?;
    let mut meta = meta;
    // ids are random; on the rare clash with a listed session, draw again
    for _ in 0..20 {
        if !dir.join(format!("{}.json", meta.id)).exists() {
            break;
        }
        meta = Arc::new(LiveMeta::new(kind));
    }
    let socket = socket_for(&meta.id);
    listen(studio, &socket, approver, meta.clone())?;
    let rec = SessionRecord { id: meta.id.clone(), kind: kind.to_string(), pid: std::process::id(), socket: socket.clone(), started_ms: crate::now_ms() as u64 };
    let path = dir.join(format!("{}.json", meta.id));
    // write then rename, so a client never reads half a record
    let tmp = dir.join(format!(".{}.json.tmp", meta.id));
    std::fs::write(&tmp, serde_json::to_vec(&rec).unwrap_or_default())?;
    std::fs::rename(&tmp, &path)?;
    Ok(Hosted { meta, socket, record: Some(path) })
}

fn listen(studio: Arc<Mutex<Studio>>, path: &Path, approver: Option<Approver>, meta: Arc<LiveMeta>) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
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
            let m = meta.clone();
            std::thread::spawn(move || handle(st, conn, ap, m));
        }
    })?;
    Ok(())
}

fn handle(studio: Arc<Mutex<Studio>>, conn: UnixStream, approver: Option<Approver>, meta: Arc<LiveMeta>) {
    let mut w = match conn.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let conn_id = meta.next_conn.fetch_add(1, Ordering::Relaxed);
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
                if tool == "__session" {
                    let r = Ok(ToolOutput::text(format!("{} {}", meta.kind, meta.id)).with_data(meta.info(&studio)));
                    if writeln!(w, "{}", output_to_json(&id, &r)).is_err() {
                        break;
                    }
                    continue;
                }
                if tool == "__attach" {
                    if let Ok(mut a) = meta.agents.lock() {
                        a.retain(|(c, _)| *c != conn_id);
                        a.push((conn_id, requester.clone()));
                    }
                    if let Ok(mut s) = studio.lock() {
                        s.note(Actor::Agent, "agent_attached", &format!("{requester} is working in this window"), true);
                    }
                    let r = Ok(ToolOutput::text(format!("attached to {} {}", meta.kind, meta.id)).with_data(meta.info(&studio)));
                    if writeln!(w, "{}", output_to_json(&id, &r)).is_err() {
                        break;
                    }
                    continue;
                }
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
    let left: Vec<String> = match meta.agents.lock() {
        Ok(mut a) => {
            let gone = a.iter().filter(|(c, _)| *c == conn_id).map(|(_, n)| n.clone()).collect();
            a.retain(|(c, _)| *c != conn_id);
            gone
        }
        Err(_) => Vec::new(),
    };
    // an event, so the window redraws its agent link chip
    for name in left {
        if let Ok(s) = studio.lock() {
            s.events.push("agent_detached", Some(Actor::Agent), format!("{name} stopped working in this window"), json!({}));
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
