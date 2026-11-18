//! The workspace: the folder you work in, like VS Code's explorer. Lists it one level at a time
//! (folders first, then project, model, print and image files), so the window can show a lazy
//! tree and agents can find models without a shell. The root is kept in
//! `~/.only3d/workspace.json`. Until one is chosen the window shows no folder (like a code
//! editor's explorer); the tool then lists the open project's folder so agents can still look.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::tools::{def, Registry, ToolError, ToolOutput};
use crate::Studio;

/// What a file is, from its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Folder,
    /// An only3d project (`.only3d.json`).
    Project,
    /// Something to import: STL, 3MF, OBJ, STEP, SCAD.
    Model,
    /// Something to print or hand on: `.gcode.3mf`, `.gcode`, a Bambu project written by only3d.
    Print,
    Image,
    /// Vector drawings: SVG, DXF.
    Vector,
    /// Notes: text, Markdown, logs.
    Text,
    /// Documents: PDF, Office, Pages.
    Document,
    /// Data: JSON, YAML, TOML, CSV, XML.
    Data,
    /// Scripts and code.
    Code,
    Video,
    Archive,
    Other,
}

pub fn kind_of(name: &str) -> FileKind {
    let n = name.to_ascii_lowercase();
    if n.ends_with(".only3d.json") {
        FileKind::Project
    } else if n.ends_with(".gcode.3mf") || n.ends_with(".gcode") || n.ends_with(".bgcode") || n.ends_with(".bambu.3mf") {
        FileKind::Print
    } else if [".stl", ".3mf", ".obj", ".step", ".stp", ".scad"].iter().any(|e| n.ends_with(e)) {
        FileKind::Model
    } else {
        let ext = n.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        match ext {
            "png" | "jpg" | "jpeg" | "webp" | "gif" | "heic" | "tif" | "tiff" | "bmp" => FileKind::Image,
            "svg" | "dxf" => FileKind::Vector,
            "txt" | "md" | "markdown" | "log" | "rtf" => FileKind::Text,
            "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "pages" | "numbers" | "key" | "odt" => FileKind::Document,
            "json" | "yaml" | "yml" | "toml" | "csv" | "tsv" | "xml" | "ini" | "ipynb" => FileKind::Data,
            "py" | "sh" | "zsh" | "js" | "ts" | "rs" | "swift" | "c" | "h" | "cpp" | "hpp" | "html" | "css" | "lua" | "rb" | "go" | "java" => FileKind::Code,
            "mp4" | "mov" | "m4v" | "webm" | "avi" | "mkv" => FileKind::Video,
            "zip" | "tar" | "gz" | "tgz" | "7z" | "rar" => FileKind::Archive,
            _ => FileKind::Other,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub kind: FileKind,
    /// Bytes (0 for folders).
    pub size: u64,
    /// Seconds since the Unix epoch.
    pub modified: u64,
}

/// Folders that are never interesting in a design workspace.
const SKIP: &[&str] = &["target", "node_modules", "__pycache__", ".git", ".venv", "venv"];

/// One folder's entries: folders first, then files (all kinds when `all`, else only project,
/// model, print and image files), each sorted by name. Hidden entries are left out.
pub fn list(dir: &Path, all: bool) -> Result<Vec<Entry>, ToolError> {
    let rd = std::fs::read_dir(dir).map_err(|e| ToolError::new("io_error", format!("cannot list {}: {e}", dir.display()), "check the folder exists"))?;
    let mut out: Vec<Entry> = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || SKIP.contains(&name.as_str()) {
            continue;
        }
        let Ok(md) = e.metadata() else { continue };
        let kind = if md.is_dir() { FileKind::Folder } else { kind_of(&name) };
        if !all && !matches!(kind, FileKind::Folder | FileKind::Project | FileKind::Model | FileKind::Print | FileKind::Image) {
            continue;
        }
        let modified = md.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
        out.push(Entry { name, path: e.path(), kind, size: if md.is_dir() { 0 } else { md.len() }, modified });
    }
    out.sort_by(|a, b| (a.kind != FileKind::Folder).cmp(&(b.kind != FileKind::Folder)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(out)
}

fn config_path() -> PathBuf {
    crate::config_dir().join("workspace.json")
}

/// The chosen workspace folder, if any.
pub fn saved_root() -> Option<PathBuf> {
    let text = std::fs::read_to_string(config_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("root").and_then(|r| r.as_str()).map(PathBuf::from).filter(|p| p.is_dir())
}

/// The workspace folder: the chosen one, else the open project's folder, else the current one.
pub fn root(s: &Studio) -> PathBuf {
    saved_root()
        .or_else(|| s.project_path.as_ref().and_then(|p| p.parent()).filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Close the workspace folder: none is open until one is chosen again.
pub fn clear_root() -> Result<(), ToolError> {
    match std::fs::remove_file(config_path()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(ToolError::new("io_error", e.to_string(), "")),
        _ => Ok(()),
    }
}

pub fn set_root(dir: &Path) -> Result<PathBuf, ToolError> {
    let dir = dunce::canonicalize(dir).map_err(|e| ToolError::new("not_found", format!("{}: {e}", dir.display()), "pass an existing folder"))?;
    if !dir.is_dir() {
        return Err(ToolError::new("not_a_folder", format!("{} is not a folder", dir.display()), "pass a folder"));
    }
    std::fs::create_dir_all(crate::config_dir()).map_err(|e| ToolError::new("io_error", e.to_string(), ""))?;
    std::fs::write(config_path(), json!({"root": dir}).to_string()).map_err(|e| ToolError::new("io_error", e.to_string(), ""))?;
    Ok(dir)
}

#[derive(Deserialize, JsonSchema)]
pub struct WorkspaceIn {
    /// Folder to list: relative to the workspace root, or absolute. Default: the root.
    #[serde(default)]
    pub path: Option<String>,
    /// Make this folder the workspace root (remembered), then list it; "" closes it.
    #[serde(default)]
    pub set_root: Option<String>,
    /// Include every file, not just projects, models, print files and images.
    #[serde(default)]
    pub all: bool,
}

fn t_workspace(s: &mut Studio, i: WorkspaceIn) -> Result<ToolOutput, ToolError> {
    if i.set_root.as_deref() == Some("") {
        clear_root()?;
        return Ok(ToolOutput::text("Workspace folder closed: the window shows none until one is opened.").with_data(json!({"root": null})));
    }
    let chosen = i.set_root.is_some() || saved_root().is_some();
    let root = match &i.set_root {
        Some(r) => set_root(&crate::tools::expand(r))?,
        None => root(s),
    };
    let dir = match &i.path {
        Some(p) if Path::new(p).is_absolute() => PathBuf::from(p),
        Some(p) => root.join(p),
        None => root.clone(),
    };
    let entries = list(&dir, i.all)?;
    let lines: Vec<String> = entries
        .iter()
        .map(|e| match e.kind {
            FileKind::Folder => format!("{}/", e.name),
            k => format!("{}  ({}, {})", e.name, serde_json::to_value(k).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default(), human_size(e.size)),
        })
        .collect();
    let note = if i.set_root.is_some() {
        "Workspace root set. "
    } else if !chosen {
        "No workspace folder is open in the window; listing the project's folder. "
    } else {
        ""
    };
    let text = format!("{note}{} ({} entr{})\n{}", dir.display(), entries.len(), if entries.len() == 1 { "y" } else { "ies" }, lines.join("\n"));
    Ok(ToolOutput::text(text.trim_end().to_string()).with_data(json!({"root": root, "chosen": chosen, "path": dir, "entries": entries})))
}

pub fn human_size(b: u64) -> String {
    match b {
        0..=1023 => format!("{b} B"),
        1024..=1_048_575 => format!("{:.0} KB", b as f64 / 1024.0),
        _ => format!("{:.1} MB", b as f64 / 1_048_576.0),
    }
}

pub fn register(r: &mut Registry) {
    r.add(def::<WorkspaceIn>(
        "workspace",
        "Workspace",
        "List the workspace folder (or `path` in it); set_root picks it. Then open_project or import_file.",
        false,
        true,
        t_workspace,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_folders_first_and_only_useful_files() {
        let dir = std::env::temp_dir().join(format!("o3d_ws_test_{}_{}", std::process::id(), crate::now_ms()));
        std::fs::create_dir_all(dir.join("orchid")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        for f in ["hook.stl", "Plate.3mf", "tile.gcode.3mf", "notes.txt", "render.png", "a.only3d.json", "two.bambu.3mf", ".DS_Store"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let e = list(&dir, false).unwrap();
        let names: Vec<&str> = e.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["orchid", "a.only3d.json", "hook.stl", "Plate.3mf", "render.png", "tile.gcode.3mf", "two.bambu.3mf"]);
        assert_eq!(e[0].kind, FileKind::Folder);
        assert_eq!(kind_of("two.bambu.3mf"), FileKind::Print);
        assert_eq!(kind_of("Plate.3MF"), FileKind::Model);
        for (name, kind) in [("notes.md", FileKind::Text), ("spec.yaml", FileKind::Data), ("data.json", FileKind::Data), ("a.only3d.json", FileKind::Project), ("logo.svg", FileKind::Vector), ("manual.pdf", FileKind::Document), ("slice.py", FileKind::Code), ("timelapse.mp4", FileKind::Video), ("model.zip", FileKind::Archive), ("photo.HEIC", FileKind::Image), ("README", FileKind::Other)] {
            assert_eq!(kind_of(name), kind, "{name}");
        }
        assert_eq!(list(&dir, true).unwrap().len(), 8, "all: notes.txt too");
        let _ = std::fs::remove_dir_all(dir);
    }
}
