//! Agents cannot start, stop or delete things on a printer without the user's approval.
//! Each test runs the real binary with a throwaway ONLY3D_HOME (no printer configured), so
//! nothing ever reaches a machine: an approved call fails later with "no printer" instead.

use std::io::{BufRead, BufReader, Write};
use only3d_studio::live::UnixStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

fn home(tag: &str) -> std::path::PathBuf {
    let h = std::env::temp_dir().join(format!("osa-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&h).unwrap();
    h
}

struct Mcp {
    child: Child,
    out: BufReader<std::process::ChildStdout>,
}

impl Mcp {
    fn start(home: &std::path::Path, elicitation: bool) -> Mcp {
        let mut child = Command::new(env!("CARGO_BIN_EXE_only3d"))
            .args(["mcp", "--no-live"])
            .env("ONLY3D_HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut m = Mcp { child, out };
        let caps = if elicitation { json!({"elicitation": {}}) } else { json!({}) };
        m.send(json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": caps, "clientInfo": {"name": "approval-test", "version": "0"}}}));
        m.read();
        m
    }
    fn send(&mut self, v: Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    }
    fn read(&mut self) -> Value {
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
    fn call(&mut self, id: u64, tool: &str, args: Value) {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": tool, "arguments": args}}));
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn error_code(result: &Value) -> String {
    result.pointer("/result/structuredContent/error/code").and_then(Value::as_str).unwrap_or("").to_string()
}

#[test]
fn without_a_way_to_ask_the_user_the_answer_is_no() {
    let h = home("noask");
    let mut m = Mcp::start(&h, false);
    // the agent sets confirm itself: still refused
    m.call(1, "printer_skip_objects", json!({"object_ids": [1], "confirm": true}));
    let r = m.read();
    assert_eq!(error_code(&r), "approval_required", "{r}");
    // ungated tools are unaffected
    m.call(2, "describe", json!({}));
    assert_eq!(m.read()["result"]["isError"], false);
    let _ = std::fs::remove_dir_all(h);
}

#[test]
fn the_client_asks_its_user_and_their_answer_decides() {
    let h = home("ask");
    let mut m = Mcp::start(&h, true);

    // declined -> denied_by_user
    m.call(1, "printer_control", json!({"action": "stop"}));
    let ask = m.read();
    assert_eq!(ask["method"], "elicitation/create", "{ask}");
    assert!(ask["params"]["message"].as_str().unwrap().contains("approval-test wants to: Stop the running print"), "{ask}");
    m.send(json!({"jsonrpc": "2.0", "id": ask["id"], "result": {"action": "decline"}}));
    let r = m.read();
    assert_eq!(error_code(&r), "denied_by_user", "{r}");

    // accepted -> the call goes through (and fails only because no printer is configured)
    m.call(2, "printer_control", json!({"action": "stop"}));
    let ask = m.read();
    m.send(json!({"jsonrpc": "2.0", "id": ask["id"], "result": {"action": "accept", "content": {"approve": true}}}));
    let r = m.read();
    let code = error_code(&r);
    assert!(!code.is_empty() && code != "denied_by_user" && code != "approval_required" && code != "confirm_required", "approved stop should reach the printer layer, got {r}");
    let _ = std::fs::remove_dir_all(h);
}

#[test]
fn the_live_socket_gates_whatever_actor_a_caller_claims() {
    let h = home("live");
    let sock = h.join("live.sock");
    let mut serve = Command::new(env!("CARGO_BIN_EXE_only3d"))
        .arg("serve")
        .env("ONLY3D_HOME", &h)
        .env("ONLY3D_SOCKET", &sock)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    let stream = loop {
        if let Ok(s) = UnixStream::connect(&sock) {
            break s;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "only3d serve did not start");
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut w = stream.try_clone().unwrap();
    let mut r = BufReader::new(stream);
    // a program pretending to be the user, asking to delete a printer file
    writeln!(w, "{}", json!({"id": 1, "tool": "printer_file", "args": {"action": "delete", "path": "/x.3mf", "confirm": true}, "actor": "user"})).unwrap();
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    let v: Value = serde_json::from_str(&line).unwrap();
    // `only3d serve` has no window to ask in, so the answer is no
    assert_eq!(v["error"]["code"], "approval_required", "{v}");
    let _ = serve.kill();
    let _ = serve.wait();
    let _ = std::fs::remove_dir_all(h);
}
