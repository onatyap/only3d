//! A crash leaves a report the next window can offer.

use std::process::Command;

#[test]
fn a_crash_leaves_a_report() {
    let home = std::env::temp_dir().join(format!("o3d-crash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let out = Command::new(env!("CARGO_BIN_EXE_only3d")).arg("--version").env("ONLY3D_HOME", &home).env("ONLY3D_TEST_PANIC", "1").output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("ONLY3D_TEST_PANIC is set"), "the usual panic message still prints");
    let reports: Vec<_> = std::fs::read_dir(home.join("crashes")).unwrap().flatten().map(|e| e.path()).collect();
    assert_eq!(reports.len(), 1, "{reports:?}");
    let text = std::fs::read_to_string(&reports[0]).unwrap();
    assert!(text.contains(&format!("only3d {} stopped", env!("CARGO_PKG_VERSION"))), "{text}");
    assert!(text.contains("what: ONLY3D_TEST_PANIC is set") && text.contains("where: ") && text.contains("backtrace:"), "{text}");
    let _ = std::fs::remove_dir_all(home);
}
