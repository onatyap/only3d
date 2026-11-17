//! Generates `embedded_profiles.rs`: a static list of `(relative_path, include_str!(..))`
//! for every JSON preset under `<workspace>/profiles/bambu`, so the crate works on machines
//! without a Bambu Studio installation.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn collect(dir: &Path, root: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, root, out);
        } else if path.extension().is_some_and(|e| e == "json") {
            let rel = path
                .strip_prefix(root)
                .expect("path under root")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.push((rel, path));
        }
    }
}

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let root = manifest_dir.join("..").join("..").join("profiles").join("bambu");
    println!("cargo:rerun-if-changed={}", root.display());
    println!("cargo:rerun-if-changed=build.rs");

    let mut files = Vec::new();
    for kind in ["machine", "process", "filament"] {
        let dir = root.join(kind);
        println!("cargo:rerun-if-changed={}", dir.display());
        collect(&dir, &root, &mut files);
    }
    files.sort();
    if files.is_empty() {
        println!(
            "cargo:warning=only3d-profiles: no embedded Bambu profiles found under {}",
            root.display()
        );
    }

    let mut src = String::from(
        "/// `(path relative to profiles/bambu, file contents)` for every embedded preset.\n\
         pub(crate) static EMBEDDED_FILES: &[(&str, &str)] = &[\n",
    );
    for (rel, path) in &files {
        println!("cargo:rerun-if-changed={}", path.display());
        let abs = path.canonicalize().unwrap_or_else(|_| path.clone());
        writeln!(src, "    ({rel:?}, include_str!({:?})),", abs.to_string_lossy()).unwrap();
    }
    src.push_str("];\n");

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("embedded_profiles.rs");
    std::fs::write(out, src).expect("write embedded_profiles.rs");
}
