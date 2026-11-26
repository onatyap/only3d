//! Printer actions that need the user's approval can't go around it through the command line:
//! `only3d call -p` without a window asks on the terminal, and without one (an agent's shell)
//! refuses; a batch can't carry one either. The test home has no printer, so nothing could
//! reach a printer even if a gate failed.

use std::process::{Command, Stdio};

fn run(home: &std::path::Path, tool: &str, args: &str) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_only3d"))
        .args(["call", tool, args, "-p"])
        .arg(home.join("p.only3d.json"))
        .env("ONLY3D_HOME", home)
        .env("ONLY3D_SOCKET", home.join("none.sock"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

#[test]
fn gated_calls_need_a_person() {
    let home = std::env::temp_dir().join(format!("o3d-cli-approval-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let (ok, out) = run(&home, "printer_control", r#"{"action": "stop"}"#);
    assert!(!ok && out.contains("approval_required") && out.contains("run the same command in a terminal"), "{out}");
    let (ok, out) = run(&home, "print", r#"{"confirm": true}"#);
    assert!(!ok && !out.contains("Uploaded"), "{out}");
    let (ok, out) = run(&home, "batch", r#"{"calls": [{"tool": "printer_file", "args": {"action": "delete", "path": "/x.3mf"}}]}"#);
    assert!(!ok && out.contains("not_batchable"), "{out}");
    // heating an idle printer needs a person too; turning a heater off doesn't (it fails here
    // only because the test home has no printer)
    let (ok, out) = run(&home, "printer_set", r#"{"nozzle_temp": 220}"#);
    assert!(!ok && out.contains("approval_required"), "{out}");
    let (ok, out) = run(&home, "batch", r#"{"calls": [{"tool": "printer_set", "args": {"bed_temp": 60}}]}"#);
    assert!(!ok && out.contains("not_batchable"), "{out}");
    let (ok, out) = run(&home, "printer_set", r#"{"nozzle_temp": 0}"#);
    assert!(!ok && !out.contains("approval_required"), "{out}");
    // positional arguments would reach the tool as its fields in order, past the gate, which
    // reads them by name: they are refused as input, directly and inside a batch. (A regression
    // here would show as no_printer: the test home has none.)
    for (tool, args) in [
        ("print", "[null, true]"),
        ("printer_control", r#"["stop", null, true]"#),
        ("printer_set", "[null, null, null, 120, 300]"),
        ("printer_file", r#"[null, "delete", "/x.3mf", null, true]"#),
        ("printer_ams", r#"[null, "load", 0, 0]"#),
        ("printer_alert", r#"[null, "resume"]"#),
        ("printer_skip_objects", "[null, [1], true]"),
        ("batch", r#"{"calls": [{"tool": "printer_control", "args": ["stop", null, true]}]}"#),
        ("batch", r#"{"calls": [{"tool": "printer_set", "args": [null, null, null, 60]}]}"#),
    ] {
        let (ok, out) = run(&home, tool, args);
        assert!(!ok && out.contains("bad_input") && out.contains("not an array"), "{tool} {args}: {out}");
    }
    let _ = std::fs::remove_dir_all(home);
}

/// F-27: a saved Bambu printer's address can change without asking for its access code again;
/// its certificate pin stays.
#[test]
fn a_printer_can_move_to_a_new_address() {
    let home = std::env::temp_dir().join(format!("o3d-cli-move-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let (ok, out) = run(&home, "printer_add", r#"{"name": "h2d", "host": "192.168.1.20", "serial": "0948AB0C1200123", "access_code": "12345678"}"#);
    assert!(ok, "{out}");
    let book = home.join("printers.json");
    let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&book).unwrap()).unwrap();
    v["printers"][0]["cert_sha256"] = serde_json::json!("ab".repeat(32));
    std::fs::write(&book, v.to_string()).unwrap();
    let (ok, out) = run(&home, "printer_add", r#"{"name": "h2d", "host": "10.0.0.5"}"#);
    assert!(ok && out.contains("Moved printer 'h2d' from 192.168.1.20 to 10.0.0.5"), "{out}");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&book).unwrap()).unwrap();
    let p = &v["printers"][0];
    assert_eq!((p["host"].as_str(), p["serial"].as_str(), p["access_code"].as_str()), (Some("10.0.0.5"), Some("0948AB0C1200123"), Some("12345678")), "{v}");
    assert_eq!(p["cert_sha256"].as_str(), Some("ab".repeat(32).as_str()), "the pin is kept: {v}");
    assert_eq!(v["printers"].as_array().unwrap().len(), 1);
    // a new printer still needs its code
    let (ok, out) = run(&home, "printer_add", r#"{"name": "x1", "host": "10.0.0.8", "serial": "00M09A000000001"}"#);
    assert!(!ok && out.contains("missing_field"), "{out}");
    let _ = std::fs::remove_dir_all(home);
}
