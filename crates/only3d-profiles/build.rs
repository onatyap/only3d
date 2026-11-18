//! Packs every JSON preset under `<workspace>/profiles/bambu` (Bambu Studio's whole BBL bundle:
//! every printer, process and filament) and `<workspace>/profiles/orca` (OrcaSlicer's other
//! makers, stored as `orca/<Vendor>/...`) into a deflated zip in `OUT_DIR`, and generates
//! `embedded_profiles.rs`, which includes it. The crate then works on machines without a Bambu
//! Studio or OrcaSlicer installation, for about 11 MB instead of 33 MB of plain JSON.

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
    // OrcaSlicer's makers: every vendor folder's machine/process/filament presets
    let orca = manifest_dir.join("..").join("..").join("profiles").join("orca");
    println!("cargo:rerun-if-changed={}", orca.display());
    let mut vendors: Vec<PathBuf> = std::fs::read_dir(&orca).map(|d| d.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default();
    vendors.sort();
    for v in vendors {
        let name = v.file_name().expect("vendor dir").to_string_lossy().into_owned();
        let mut vf = Vec::new();
        for kind in ["machine", "process", "filament"] {
            let dir = v.join(kind);
            println!("cargo:rerun-if-changed={}", dir.display());
            collect(&dir, &v, &mut vf);
        }
        files.extend(vf.into_iter().map(|(rel, p)| (format!("orca/{name}/{rel}"), p)));
    }
    files.sort();
    if files.is_empty() {
        println!(
            "cargo:warning=only3d-profiles: no embedded Bambu profiles found under {}",
            root.display()
        );
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let zip_path = out_dir.join("profiles.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).expect("create profiles.zip"));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).compression_level(Some(9));
    for (rel, path) in &files {
        println!("cargo:rerun-if-changed={}", path.display());
        zip.start_file(rel.as_str(), opts).expect("zip entry");
        std::io::Write::write_all(&mut zip, &std::fs::read(path).expect("read preset")).expect("zip write");
    }
    zip.finish().expect("finish profiles.zip");

    // an index of Orca's presets (zip entry, kind, name, user-selectable), so the store can list
    // them without parsing 12,000 files: it parses one only when it is used
    let mut index = String::new();
    for (rel, path) in files.iter().filter(|(r, _)| r.starts_with("orca/")) {
        let text = std::fs::read_to_string(path).expect("read preset");
        let Ok(serde_json::Value::Object(o)) = serde_json::from_str::<serde_json::Value>(text.trim_start_matches('\u{feff}')) else {
            println!("cargo:warning=only3d-profiles: skipping unreadable preset {rel}");
            continue;
        };
        let dir_kind = rel.split('/').nth(2).unwrap_or("");
        let kind = match (dir_kind, o.get("type").and_then(|v| v.as_str())) {
            ("machine", Some("machine_model")) => "machine_model",
            (k, _) => k,
        };
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let name = o.get("name").and_then(|v| v.as_str()).unwrap_or(&stem);
        let inst = o.get("instantiation").and_then(|v| v.as_str()).unwrap_or("");
        assert!(!rel.contains('\t') && !name.contains('\t') && !name.contains('\n'), "tab or newline in {rel}");
        writeln!(index, "{rel}\t{kind}\t{name}\t{inst}").unwrap();
    }
    std::fs::write(out_dir.join("orca_index.tsv"), index).expect("write orca_index.tsv");

    let mut src = String::from("/// Every embedded preset, as a zip of `profiles/bambu`-relative paths (see build.rs).\n");
    writeln!(src, "pub(crate) static EMBEDDED_ZIP: &[u8] = include_bytes!({:?});", zip_path.to_string_lossy()).unwrap();
    src.push_str("/// Orca's presets: `zip entry\\tkind\\tname\\tinstantiation` per line.\n");
    writeln!(src, "pub(crate) static ORCA_INDEX: &str = include_str!({:?});", out_dir.join("orca_index.tsv").to_string_lossy()).unwrap();
    std::fs::write(out_dir.join("embedded_profiles.rs"), src).expect("write embedded_profiles.rs");
}
