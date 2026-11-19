//! Agents never work by luck: with one session open they use it, with several they ask which
//! one, with none they don't quietly work somewhere the user can't see, and a closed window
//! is reported instead of replaced. Sessions here are `only3d serve` listed in a throwaway
//! ONLY3D_SESSIONS folder, and ONLY3D_NO_WINDOW keeps the tests from opening windows.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

struct Env {
    dir: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Env {
        // short: sockets live in the sessions folder, and macOS caps socket paths at ~104 bytes
        let dir = std::env::temp_dir().join(format!("o3s-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("home")).unwrap();
        Env { dir }
    }

    fn sessions(&self) -> PathBuf {
        self.dir.join("s")
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_only3d"));
        c.env("ONLY3D_HOME", self.dir.join("home")).env("ONLY3D_SESSIONS", self.sessions()).env("ONLY3D_NO_WINDOW", "1").env_remove("ONLY3D_SOCKET");
        c
    }

    /// `only3d serve`, once it is listed, and its id.
    fn serve(&self) -> (Served, String) {
        let served = Served(self.cmd().arg("serve").stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
        let t0 = Instant::now();
        loop {
            if let Some(id) = self.record_of(served.0.id()) {
                return (served, id);
            }
            assert!(t0.elapsed() < Duration::from_secs(15), "only3d serve was never listed");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn record_of(&self, pid: u32) -> Option<String> {
        for e in std::fs::read_dir(self.sessions()).ok()?.flatten() {
            let Ok(v) = serde_json::from_str::<Value>(&std::fs::read_to_string(e.path()).unwrap_or_default()) else { continue };
            if v["pid"] == json!(pid) {
                return v["id"].as_str().map(str::to_string);
            }
        }
        None
    }

    /// How many objects session `id` holds, asked over its own socket.
    fn objects(&self, id: &str) -> u64 {
        let rec: Value = serde_json::from_str(&std::fs::read_to_string(self.sessions().join(format!("{id}.json"))).unwrap()).unwrap();
        let s = only3d_studio::live::UnixStream::connect(rec["socket"].as_str().unwrap()).unwrap();
        let mut w = s.try_clone().unwrap();
        writeln!(w, "{}", json!({"id": 1, "tool": "__session"})).unwrap();
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        v["data"]["objects"].as_u64().unwrap()
    }

    fn cli(&self, args: &[&str]) -> (bool, String) {
        let out = self.cmd().args(args).stdin(Stdio::null()).output().unwrap();
        (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// An `only3d serve`, stopped when dropped (a failing test leaves nothing running).
struct Served(Child);

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn stop(s: Served) {
    drop(s);
}

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
    next: u64,
}

impl Mcp {
    fn start(env: &Env) -> Mcp {
        let mut child = env.cmd().arg("mcp").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut m = Mcp { child, stdin, out, next: 1 };
        m.request("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "TestAgent", "version": "0"}}));
        m
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        writeln!(self.stdin, "{}", json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).unwrap();
        self.stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(self.out.read_line(&mut line).unwrap() > 0, "server closed stdout");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == json!(id) {
                return v["result"].clone();
            }
        }
    }

    fn call(&mut self, tool: &str, args: Value) -> Value {
        self.request("tools/call", json!({"name": tool, "arguments": args}))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text(r: &Value) -> String {
    r["content"][0]["text"].as_str().unwrap_or("").to_string()
}

fn code(r: &Value) -> String {
    if r["isError"] != json!(true) {
        return String::new();
    }
    r["structuredContent"]["error"]["code"].as_str().unwrap_or("").to_string()
}

fn peg(id: &str) -> Value {
    json!({"id": id, "shape": {"type": "cylinder", "d": 6, "h": 10}})
}

#[test]
fn an_agent_works_in_the_one_open_session_and_says_so() {
    let env = Env::new("one");
    let mut m = Mcp::start(&env);

    // nothing open, and no window may be opened: nothing happens anywhere
    let r = m.call("add_object", peg("lost"));
    assert_eq!(code(&r), "no_window", "{r}");
    assert!(text(&r).contains("ask the user to open only3d"), "{r}");

    // the session tool lists itself, and tools/list stays affordable with it
    let list = m.request("tools/list", json!({}));
    assert!(list["tools"].as_array().unwrap().iter().any(|t| t["name"] == "session"));

    // one session open: used, and the first result says which, so the agent can tell the user
    let (serve, id) = env.serve();
    let r = m.call("add_object", peg("a"));
    assert_eq!(code(&r), "", "{r}");
    assert!(text(&r).starts_with(&format!("Working in only3d serve {id}")), "{r}");
    assert_eq!(env.objects(&id), 1);
    // said once
    let r = m.call("describe", json!({}));
    assert!(!text(&r).contains("Working in"), "{r}");

    // the session shows who is attached
    let (ok, out) = env.cli(&["sessions"]);
    assert!(ok && out.contains(&id) && out.contains("TestAgent"), "{out}");

    // the window goes away: reported, never carried on in an empty session
    stop(serve);
    let r = m.call("add_object", peg("b"));
    assert_eq!(code(&r), "window_closed", "{r}");
    let r = m.call("add_object", peg("c"));
    assert_eq!(code(&r), "no_window", "{r}");
    assert!(text(&r).contains("was closed"), "{r}");

    // reopened: the next call attaches to it, and says so
    let (serve, id2) = env.serve();
    let r = m.call("add_object", peg("d"));
    assert_eq!(code(&r), "", "{r}");
    assert!(text(&r).contains(&id2), "{r}");
    assert_eq!(env.objects(&id2), 1);
    stop(serve);
}

#[test]
fn with_several_sessions_open_the_agent_asks_which() {
    let env = Env::new("several");
    let (a, id_a) = env.serve();
    let (b, id_b) = env.serve();
    let mut m = Mcp::start(&env);

    let r = m.call("add_object", peg("p"));
    assert_eq!(code(&r), "session_choice_needed", "{r}");
    assert!(text(&r).contains(&id_a) && text(&r).contains(&id_b), "{r}");
    assert_eq!(env.objects(&id_a) + env.objects(&id_b), 0, "nothing may land anywhere");

    let r = m.call("session", json!({}));
    assert_eq!(r["structuredContent"]["sessions"].as_array().unwrap().len(), 2, "{r}");
    assert_eq!(r["structuredContent"]["current"], Value::Null, "{r}");

    let r = m.call("session", json!({"id": "zzzz"}));
    assert_eq!(code(&r), "session_not_found", "{r}");

    // the user said b
    let r = m.call("session", json!({"id": id_b.to_uppercase()}));
    assert_eq!(code(&r), "", "{r}");
    assert!(text(&r).contains(&id_b), "{r}");
    let r = m.call("add_object", peg("p"));
    assert_eq!(code(&r), "", "{r}");
    assert_eq!((env.objects(&id_a), env.objects(&id_b)), (0, 1));

    // the command line asks the same way
    let (ok, out) = env.cli(&["call", "add_object", &peg("q").to_string()]);
    assert!(!ok && out.contains("session_choice_needed") && out.contains("--session"), "{out}");
    let (ok, out) = env.cli(&["call", "add_object", &peg("q").to_string(), "--session", &id_a]);
    assert!(ok, "{out}");
    assert_eq!((env.objects(&id_a), env.objects(&id_b)), (1, 1));

    stop(a);
    stop(b);
}

#[test]
fn headless_is_only_on_request_and_says_so() {
    let env = Env::new("headless");
    let mut m = Mcp::start(&env);
    let r = m.call("session", json!({"headless": true}));
    assert_eq!(code(&r), "", "{r}");
    let r = m.call("add_object", peg("h"));
    assert_eq!(code(&r), "", "{r}");
    let r = m.call("describe", json!({}));
    assert!(text(&r).contains("headless session"), "{r}");

    // the command line: a change with no window to show it in is refused, reads still work
    let (ok, out) = env.cli(&["call", "add_object", &peg("x").to_string()]);
    assert!(!ok && out.contains("no_window") && out.contains("-p"), "{out}");
    let (ok, out) = env.cli(&["call", "settings_catalog", "{}"]);
    assert!(ok, "{out}");
}

#[test]
fn sessions_that_are_gone_drop_off_the_list() {
    let env = Env::new("stale");
    std::fs::create_dir_all(env.sessions()).unwrap();
    let dead = env.sessions().join("dead.json");
    let sock: &Path = &env.sessions().join("dead.sock");
    std::fs::write(&dead, json!({"id": "dead", "kind": "window", "pid": 1, "socket": sock, "started_ms": 0}).to_string()).unwrap();
    let (ok, out) = env.cli(&["sessions"]);
    assert!(ok && out.contains("No only3d session is open"), "{out}");
    assert!(!dead.exists(), "a crashed window's record should be cleaned up");
}
