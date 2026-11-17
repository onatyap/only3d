//! Agents wait for events on a shared (live) session without blocking anyone else, and wake
//! up as soon as something happens. Runs `only3d serve` with a throwaway home.

use std::io::{BufRead, BufReader, Write};
use only3d_studio::live::UnixStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

fn call(sock: &std::path::Path, tool: &str, args: Value) -> Value {
    let s = UnixStream::connect(sock).unwrap();
    let mut w = s.try_clone().unwrap();
    writeln!(w, "{}", json!({"id": 1, "tool": tool, "args": args, "actor": "agent", "client": "events-test"})).unwrap();
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn a_waiting_agent_wakes_on_an_edit_and_blocks_nobody() {
    let home = std::env::temp_dir().join(format!("ose-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    // macOS limits socket paths to ~104 bytes: keep it short and relative to the test's cwd
    let sock = std::path::PathBuf::from(format!("ose-{}.sock", std::process::id()));
    let mut serve = Command::new(env!("CARGO_BIN_EXE_only3d"))
        .arg("serve")
        .env("ONLY3D_HOME", &home)
        .env("ONLY3D_SOCKET", &sock)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    while UnixStream::connect(&sock).is_err() {
        assert!(t0.elapsed() < Duration::from_secs(10), "serve did not start");
        std::thread::sleep(Duration::from_millis(50));
    }

    // agent A waits for the next project change (up to 20 s)
    let s2 = sock.clone();
    let waiter = std::thread::spawn(move || {
        let t = Instant::now();
        let r = call(&s2, "wait_for_event", json!({"kinds": ["project_changed"], "timeout_s": 20}));
        (r, t.elapsed())
    });
    std::thread::sleep(Duration::from_millis(300));

    // agent B edits meanwhile: must not be blocked by A's wait
    let t = Instant::now();
    let r = call(&sock, "add_object", json!({"id": "peg", "shape": {"type": "cylinder", "d": 6, "h": 10}}));
    assert_eq!(r["ok"], true, "{r}");
    assert!(t.elapsed() < Duration::from_secs(5), "an edit waited {:?} behind someone's wait_for_event", t.elapsed());

    let (r, waited) = waiter.join().unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let events = r["data"]["events"].as_array().unwrap();
    assert_eq!(events[0]["kind"], "project_changed", "{r}");
    assert_eq!(events[0]["by"], "agent");
    assert!(events[0]["summary"].as_str().unwrap().contains("add_object"));
    assert!(waited < Duration::from_secs(5), "the waiter woke after {waited:?}");
    assert!(r["data"]["next_since"].as_u64().unwrap() >= 1);

    // nothing new: times out cleanly
    let r = call(&sock, "wait_for_event", json!({"since": r["data"]["next_since"], "timeout_s": 1}));
    assert_eq!(r["data"]["timed_out"], true, "{r}");

    let _ = serve.kill();
    let _ = serve.wait();
    let _ = std::fs::remove_file(&sock);
    let _ = std::fs::remove_dir_all(home);
}
