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
    let _ = std::fs::remove_dir_all(home);
}
