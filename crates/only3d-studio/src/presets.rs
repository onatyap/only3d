//! Presets as data agents can read, write and create: process, filament and machine presets in
//! Bambu Studio's JSON format (so they round-trip with Bambu Studio), layered as
//! system → the user's Bambu Studio presets → only3d's own (`~/.only3d/presets`).

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use only3d_profiles::{Config, ConfigValue, PresetKind};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::bambu::{kind_of, only3d_preset_dir, presets, PresetSource};
use crate::tools::{def, Registry, ToolError, ToolOutput};
use crate::Studio;

fn kind_dir(k: PresetKind) -> &'static str {
    match k {
        PresetKind::Machine | PresetKind::MachineModel => "machine",
        PresetKind::Process => "process",
        PresetKind::Filament => "filament",
    }
}

fn perr(e: only3d_profiles::ProfileError) -> ToolError {
    ToolError::new("profile_error", e.to_string(), "check names with preset_list")
}

fn safe_file_name(name: &str) -> String {
    name.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '\0') { '_' } else { c }).collect()
}

/// Format a JSON value the way Bambu stores settings (strings; arrays of strings).
fn to_bambu_value(v: &Value, base: Option<&ConfigValue>) -> Result<ConfigValue, String> {
    let scalar = |x: &Value| -> Result<String, String> {
        Ok(match x {
            Value::String(s) => s.clone(),
            Value::Bool(b) => if *b { "1".into() } else { "0".into() },
            Value::Number(n) => {
                let f = n.as_f64().unwrap_or(0.0);
                if f.fract() == 0.0 { format!("{}", f as i64) } else { format!("{f}") }
            }
            other => return Err(format!("unsupported value {other}")),
        })
    };
    match v {
        Value::Array(a) => Ok(ConfigValue::from(a.iter().map(scalar).collect::<Result<Vec<_>, _>>()?)),
        other => {
            let mut s = scalar(other)?;
            // percent-style keys keep their % ("15" → "15%")
            if let Some(b) = base {
                if b.first().is_some_and(|f| f.ends_with('%')) && !s.ends_with('%') {
                    s.push('%');
                }
                // per-variant arrays keep their length
                if b.is_array() {
                    return Ok(ConfigValue::from(vec![s; b.len().max(1)]));
                }
            }
            Ok(ConfigValue::from(s))
        }
    }
}

/// Write an only3d preset (Bambu user-preset format: inherits + changed keys).
fn write_preset(kind: PresetKind, name: &str, inherits: &str, changes: &Config) -> Result<PathBuf, ToolError> {
    let dir = only3d_preset_dir().join(kind_dir(kind));
    std::fs::create_dir_all(&dir).map_err(|e| ToolError::new("io_error", e.to_string(), ""))?;
    let mut obj = Map::new();
    if let Value::Object(m) = changes.to_json() {
        obj.extend(m);
    }
    obj.insert("type".into(), json!(kind_dir(kind)));
    obj.insert("name".into(), json!(name));
    obj.insert("inherits".into(), json!(inherits));
    obj.insert("from".into(), json!("User"));
    obj.insert("instantiation".into(), json!("true"));
    obj.insert("version".into(), json!("2.8.0.6"));
    let id_key = match kind {
        PresetKind::Process => "print_settings_id",
        PresetKind::Filament => "filament_settings_id",
        _ => "printer_settings_id",
    };
    obj.insert(id_key.into(), if kind == PresetKind::Filament { json!([name]) } else { json!(name) });
    let path = dir.join(format!("{}.json", safe_file_name(name)));
    std::fs::write(&path, serde_json::to_string_pretty(&Value::Object(obj)).unwrap_or_default()).map_err(|e| ToolError::new("io_error", e.to_string(), ""))?;
    Ok(path)
}

/// Apply Bambu-key settings and only3d-key overrides to `changes`, validated against the
/// resolved base.
fn apply_changes(kind: PresetKind, base: &Config, changes: &mut Config, settings: &BTreeMap<String, Value>, overrides: &BTreeMap<String, Value>) -> Result<Vec<String>, ToolError> {
    let mut applied = Vec::new();
    for (k, v) in settings {
        if !base.contains_key(k) {
            let similar: Vec<&str> = base.keys().filter(|x| x.contains(k.split('_').next().unwrap_or(k))).take(6).collect();
            return Err(ToolError::new("unknown_setting", format!("'{k}' is not a {} setting of the base preset", kind_dir(kind)), format!("Bambu keys like: {}", similar.join(", "))));
        }
        let val = to_bambu_value(v, base.get(k)).map_err(|e| ToolError::new("bad_value", format!("{k}: {e}"), ""))?;
        changes.insert(k.clone(), val);
        applied.push(k.clone());
    }
    if !overrides.is_empty() {
        // only3d keys (infill_density 0..1, wall_loops, brim_width, support, temps...) → Bambu keys
        let mut proc_cfg = base.clone();
        let mut fils = vec![base.clone()];
        let unmapped = crate::bambu_export::map_overrides(overrides, &mut proc_cfg, &mut fils);
        if !unmapped.is_empty() {
            return Err(ToolError::new("unmapped_setting", format!("no Bambu equivalent for: {}", unmapped.join(", ")), "use Bambu keys in `settings` (see preset_get)"));
        }
        let mapped = if kind == PresetKind::Filament { &fils[0] } else { &proc_cfg };
        for (k, v) in mapped.iter() {
            if base.get(k) != Some(v) {
                changes.insert(k.to_string(), v.clone());
                applied.push(k.to_string());
            }
        }
    }
    Ok(applied)
}

// ------------------------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub struct ListIn {
    /// "process" | "filament" | "machine"
    pub kind: String,
    #[serde(default)]
    pub query: Option<String>,
    /// Only user presets (Bambu Studio user presets + only3d presets).
    #[serde(default)]
    pub user_only: bool,
}

fn t_list(s: &mut Studio, i: ListIn) -> Result<ToolOutput, ToolError> {
    let k = kind_of(&i.kind)?;
    let p = presets()?;
    let names = s.machine.list_presets(&i.kind, i.query.as_deref().unwrap_or(""));
    let rows: Vec<Value> = names
        .iter()
        .filter_map(|n| {
            let src = p.sources.get(&(k, n.clone())).copied().unwrap_or(PresetSource::System);
            if i.user_only && src == PresetSource::System {
                return None;
            }
            let raw = p.store.raw(k, n);
            Some(json!({"name": n, "source": src, "inherits": raw.and_then(|r| r.inherits.clone())}))
        })
        .collect();
    let text = rows
        .iter()
        .map(|r| format!("{} [{}]{}", r["name"].as_str().unwrap_or(""), r["source"].as_str().unwrap_or(""), r["inherits"].as_str().map(|b| format!(" ← {b}")).unwrap_or_default()))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(ToolOutput::text(format!("{} {} preset(s):\n{text}", rows.len(), i.kind)).with_data(Value::Array(rows)))
}

#[derive(Deserialize, JsonSchema)]
pub struct GetIn {
    pub kind: String,
    pub name: String,
    /// Also return the fully resolved configuration (large).
    #[serde(default)]
    pub resolved: bool,
}

fn t_get(_s: &mut Studio, i: GetIn) -> Result<ToolOutput, ToolError> {
    let k = kind_of(&i.kind)?;
    let p = presets()?;
    let raw = p.store.raw(k, &i.name).ok_or_else(|| ToolError::new("not_found", format!("no {} preset '{}'", i.kind, i.name), "see preset_list"))?;
    let src = p.sources.get(&(k, i.name.clone())).copied().unwrap_or(PresetSource::System);
    let own = raw.config.to_json();
    let mut data = json!({"name": i.name, "kind": i.kind, "source": src, "inherits": raw.inherits, "path": p.files.get(&(k, i.name.clone())), "changes": own});
    if i.resolved {
        data["resolved"] = p.store.resolve(k, &i.name).map_err(perr)?.to_json();
    }
    let keys: Vec<String> = own.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
    Ok(ToolOutput::text(format!(
        "{} preset '{}' [{:?}]{}: {} own setting(s): {}",
        i.kind,
        i.name,
        src,
        raw.inherits.as_ref().map(|b| format!(" inherits '{b}'")).unwrap_or_default(),
        keys.len(),
        keys.join(", ")
    ))
    .with_data(data))
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateIn {
    pub kind: String,
    /// New preset name.
    pub name: String,
    /// Preset to inherit from (default: the project's).
    #[serde(default)]
    pub base: Option<String>,
    /// Include the project's process overrides.
    #[serde(default)]
    pub from_project: bool,
    /// Bambu keys, e.g. {"sparse_infill_density": "25%"}.
    #[serde(default)]
    pub settings: BTreeMap<String, Value>,
    /// only3d keys (as in set_process).
    #[serde(default)]
    pub overrides: BTreeMap<String, Value>,
    /// Switch the open project to the new preset.
    #[serde(default)]
    pub apply: bool,
}

fn current_of(s: &Studio, k: PresetKind) -> String {
    match k {
        PresetKind::Process => s.project.process.preset.clone(),
        PresetKind::Filament => s.project.filaments.first().map(|f| f.preset.clone()).unwrap_or_default(),
        _ => s.project.printer.machine.clone(),
    }
}

fn t_create(s: &mut Studio, i: CreateIn) -> Result<ToolOutput, ToolError> {
    let k = kind_of(&i.kind)?;
    let p = presets()?;
    if p.sources.get(&(k, i.name.clone())).is_some_and(|src| *src == PresetSource::Only3d) || p.store.raw(k, &i.name).is_some() {
        return Err(ToolError::new("exists", format!("a {} preset named '{}' already exists", i.kind, i.name), "pick another name or use preset_update"));
    }
    let base_name = i.base.clone().unwrap_or_else(|| current_of(s, k));
    let base = p.store.resolve(k, &base_name).map_err(perr)?;
    let mut overrides = i.overrides.clone();
    if i.from_project && k == PresetKind::Process {
        for (key, v) in &s.project.process.overrides {
            overrides.entry(key.clone()).or_insert(v.clone());
        }
    }
    let mut changes = Config::new();
    let applied = apply_changes(k, &base, &mut changes, &i.settings, &overrides)?;
    let path = write_preset(k, &i.name, &base_name, &changes)?;
    if i.apply {
        match k {
            PresetKind::Process => {
                s.project.process.preset = i.name.clone();
                if i.from_project {
                    s.project.process.overrides.clear();
                }
            }
            PresetKind::Filament => {
                if let Some(f) = s.project.filaments.first_mut() {
                    f.preset = i.name.clone();
                }
            }
            _ => s.project.printer.machine = i.name.clone(),
        }
        s.changed("preset_create");
    }
    Ok(ToolOutput::text(format!(
        "Created {} preset '{}' (inherits '{}') with {} change(s): {}. Saved to {}{}",
        i.kind,
        i.name,
        base_name,
        applied.len(),
        applied.join(", "),
        path.display(),
        if i.apply { ". The project now uses it." } else { "." }
    ))
    .with_data(json!({"name": i.name, "path": path, "changes": changes.to_json()})))
}

#[derive(Deserialize, JsonSchema)]
pub struct UpdateIn {
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub settings: BTreeMap<String, Value>,
    #[serde(default)]
    pub overrides: BTreeMap<String, Value>,
    /// Keys to drop (back to the inherited value).
    #[serde(default)]
    pub remove: Vec<String>,
}

fn own_preset(k: PresetKind, name: &str) -> Result<(PathBuf, Config, String), ToolError> {
    let p = presets()?;
    match p.sources.get(&(k, name.to_string())) {
        Some(PresetSource::Only3d) => {}
        Some(PresetSource::BambuUser) => return Err(ToolError::new("read_only", format!("'{name}' belongs to Bambu Studio"), "copy it: preset_create with base = this preset")),
        _ => return Err(ToolError::new("read_only", format!("'{name}' is not an only3d preset"), "create one with preset_create")),
    }
    let raw = p.store.raw(k, name).ok_or_else(|| ToolError::new("not_found", name.to_string(), ""))?;
    let path = p.files.get(&(k, name.to_string())).cloned().ok_or_else(|| ToolError::new("not_found", name.to_string(), ""))?;
    Ok((path, raw.config.clone(), raw.inherits.clone().unwrap_or_default()))
}

fn t_update(_s: &mut Studio, i: UpdateIn) -> Result<ToolOutput, ToolError> {
    let k = kind_of(&i.kind)?;
    let (_, mut changes, inherits) = own_preset(k, &i.name)?;
    let base = presets()?.store.resolve(k, &inherits).map_err(perr)?;
    for key in &i.remove {
        changes.remove(key);
    }
    // id/meta keys are rewritten by write_preset
    for meta in ["print_settings_id", "filament_settings_id", "printer_settings_id"] {
        changes.remove(meta);
    }
    let applied = apply_changes(k, &base, &mut changes, &i.settings, &i.overrides)?;
    write_preset(k, &i.name, &inherits, &changes)?;
    Ok(ToolOutput::text(format!("Updated '{}': set {}, removed {}.", i.name, if applied.is_empty() { "nothing".into() } else { applied.join(", ") }, if i.remove.is_empty() { "nothing".into() } else { i.remove.join(", ") })))
}

#[derive(Deserialize, JsonSchema)]
pub struct DeleteIn {
    pub kind: String,
    pub name: String,
}

fn t_delete(s: &mut Studio, i: DeleteIn) -> Result<ToolOutput, ToolError> {
    let k = kind_of(&i.kind)?;
    let (path, _, _) = own_preset(k, &i.name)?;
    if current_of(s, k) == i.name {
        return Err(ToolError::new("in_use", format!("the open project uses '{}'", i.name), "switch the project to another preset first"));
    }
    std::fs::remove_file(&path).map_err(|e| ToolError::new("io_error", e.to_string(), ""))?;
    Ok(ToolOutput::text(format!("Deleted {} preset '{}'.", i.kind, i.name)))
}

#[derive(Deserialize, JsonSchema)]
pub struct ImportIn {
    /// Preset .json, bundle (.bbscfg/.bbsflmt/.zip) or project .3mf (its changed settings).
    pub path: String,
    /// For .3mf: name prefix for the created presets (default: the file name).
    #[serde(default)]
    pub name: Option<String>,
}

fn import_json(text: &str, hint_kind: Option<PresetKind>, imported: &mut Vec<String>) -> Result<(), ToolError> {
    let v: Value = serde_json::from_str(text).map_err(|e| ToolError::new("bad_preset", e.to_string(), "expected a preset JSON object"))?;
    let o = v.as_object().ok_or_else(|| ToolError::new("bad_preset", "not a JSON object", ""))?;
    let kind = match o.get("type").and_then(|t| t.as_str()) {
        Some(t) => kind_of(t)?,
        None => hint_kind
            .or_else(|| if o.contains_key("print_settings_id") || o.contains_key("wall_loops") { Some(PresetKind::Process) } else if o.contains_key("filament_type") { Some(PresetKind::Filament) } else if o.contains_key("printable_area") { Some(PresetKind::Machine) } else { None })
            .ok_or_else(|| ToolError::new("bad_preset", "cannot tell whether this is a process, filament or machine preset", "add a \"type\" field"))?,
    };
    let name = o.get("name").and_then(|n| n.as_str()).ok_or_else(|| ToolError::new("bad_preset", "preset has no name", ""))?.to_string();
    let dir = only3d_preset_dir().join(kind_dir(kind));
    std::fs::create_dir_all(&dir).map_err(|e| ToolError::new("io_error", e.to_string(), ""))?;
    let mut o = o.clone();
    o.insert("type".into(), json!(kind_dir(kind)));
    o.insert("instantiation".into(), json!("true"));
    std::fs::write(dir.join(format!("{}.json", safe_file_name(&name))), serde_json::to_string_pretty(&Value::Object(o)).unwrap_or_default()).map_err(|e| ToolError::new("io_error", e.to_string(), ""))?;
    imported.push(format!("{} '{}'", kind_dir(kind), name));
    Ok(())
}

fn t_import(_s: &mut Studio, i: ImportIn) -> Result<ToolOutput, ToolError> {
    let path = crate::tools::expand(&i.path);
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let bytes = std::fs::read(&path).map_err(|e| ToolError::new("io_error", format!("{}: {e}", path.display()), ""))?;
    let mut imported = Vec::new();
    let mut notes = Vec::new();
    match ext.as_str() {
        "json" => import_json(&String::from_utf8_lossy(&bytes), None, &mut imported)?,
        "bbscfg" | "bbsflmt" | "zip" => {
            let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| ToolError::new("bad_bundle", e.to_string(), ""))?;
            for n in 0..z.len() {
                let mut f = z.by_index(n).map_err(|e| ToolError::new("bad_bundle", e.to_string(), ""))?;
                let fname = f.name().to_string();
                if !fname.ends_with(".json") || fname.ends_with("bundle_structure.json") {
                    continue;
                }
                let hint = if fname.contains("process") || fname.contains("print/") { Some(PresetKind::Process) } else if fname.contains("filament") { Some(PresetKind::Filament) } else if fname.contains("machine") || fname.contains("printer") { Some(PresetKind::Machine) } else { None };
                let mut text = String::new();
                f.read_to_string(&mut text).map_err(|e| ToolError::new("bad_bundle", e.to_string(), ""))?;
                if let Err(e) = import_json(&text, hint, &mut imported) {
                    notes.push(format!("{fname}: {}", e.message));
                }
            }
        }
        "3mf" => {
            let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| ToolError::new("bad_project", e.to_string(), ""))?;
            let mut text = String::new();
            z.by_name("Metadata/project_settings.config").map_err(|_| ToolError::new("bad_project", "no Metadata/project_settings.config (not a Bambu Studio / Orca project)", ""))?.read_to_string(&mut text).map_err(|e| ToolError::new("bad_project", e.to_string(), ""))?;
            let ps: Value = serde_json::from_str(&text).map_err(|e| ToolError::new("bad_project", e.to_string(), ""))?;
            let prefix = i.name.clone().unwrap_or_else(|| path.file_stem().and_then(|s| s.to_str()).unwrap_or("project").trim_end_matches(".gcode").to_string());
            let changed: Vec<String> = ps.get("different_settings_to_system").and_then(|d| d.as_array()).map(|a| a.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect()).unwrap_or_default();
            let store = presets()?.store.clone();
            let cfg = Config::from_json_object(ps.as_object().unwrap_or(&Map::new()));
            let keys = |k: usize| -> Vec<String> { changed.get(k).map(|s| s.split(';').filter(|x| !x.is_empty()).map(String::from).collect()).unwrap_or_default() };
            // process
            if let Some(base) = ps.get("print_settings_id").and_then(|v| v.as_str()) {
                let base = if store.raw(PresetKind::Process, base).is_some() { base.to_string() } else { "0.20mm Standard @BBL H2D".into() };
                let mut changes = Config::new();
                for k in keys(0) {
                    if let Some(v) = cfg.get(&k) {
                        changes.insert(k, v.clone());
                    }
                }
                let name = format!("{prefix} - process");
                write_preset(PresetKind::Process, &name, &base, &changes)?;
                imported.push(format!("process '{name}' ← {base} ({} change(s))", changes.len()));
            }
            // filaments (one preset per slot with changes)
            let fil_ids: Vec<String> = ps.get("filament_settings_id").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            for (n, base) in fil_ids.iter().enumerate() {
                let ks = keys(n + 1);
                if ks.is_empty() || store.raw(PresetKind::Filament, base).is_none() {
                    continue;
                }
                let mut changes = Config::new();
                for k in ks {
                    if let Some(v) = cfg.get(&k) {
                        // project arrays hold one entry per filament slot: take this slot's value
                        let one = if v.is_array() && v.len() == fil_ids.len() { v.at(n).map(|x| ConfigValue::from(vec![x.to_string()])).unwrap_or(v.clone()) } else { v.clone() };
                        changes.insert(k, one);
                    }
                }
                let name = format!("{prefix} - filament {}", n + 1);
                write_preset(PresetKind::Filament, &name, base, &changes)?;
                imported.push(format!("filament '{name}' ← {base} ({} change(s))", changes.len()));
            }
            if imported.is_empty() {
                notes.push("the project uses unmodified system presets; nothing to import".into());
            }
        }
        other => return Err(ToolError::new("bad_format", format!("cannot import '.{other}'"), ".json, .bbscfg, .bbsflmt, .zip or .3mf")),
    }
    let mut text = format!("Imported {} preset(s): {}", imported.len(), imported.join("; "));
    if !notes.is_empty() {
        text.push_str(&format!("\nNotes: {}", notes.join("; ")));
    }
    Ok(ToolOutput::text(text).with_data(json!({"imported": imported, "notes": notes})))
}

#[derive(Deserialize, JsonSchema)]
pub struct ExportIn {
    pub kind: String,
    pub name: String,
    /// Output .json (importable in Bambu Studio via File → Import → Import Configs).
    pub path: String,
    /// Write the fully resolved settings instead of inherits + changes.
    #[serde(default)]
    pub flatten: bool,
}

fn t_export(_s: &mut Studio, i: ExportIn) -> Result<ToolOutput, ToolError> {
    let k = kind_of(&i.kind)?;
    let p = presets()?;
    let raw = p.store.raw(k, &i.name).ok_or_else(|| ToolError::new("not_found", format!("no {} preset '{}'", i.kind, i.name), "see preset_list"))?;
    let mut obj = Map::new();
    let cfg = if i.flatten { p.store.resolve(k, &i.name).map_err(perr)? } else { raw.config.clone() };
    if let Value::Object(m) = cfg.to_json() {
        obj.extend(m);
    }
    obj.insert("type".into(), json!(kind_dir(k)));
    obj.insert("name".into(), json!(i.name));
    obj.insert("from".into(), json!(if i.flatten { "system" } else { "User" }));
    obj.insert("instantiation".into(), json!("true"));
    if !i.flatten {
        if let Some(b) = &raw.inherits {
            obj.insert("inherits".into(), json!(b));
        }
    }
    let out = Path::new(&i.path);
    std::fs::write(out, serde_json::to_string_pretty(&Value::Object(obj)).unwrap_or_default()).map_err(|e| ToolError::new("io_error", e.to_string(), ""))?;
    Ok(ToolOutput::text(format!("Exported {} preset '{}' to {}{}", i.kind, i.name, out.display(), if i.flatten { " (fully resolved)" } else { " (inherits + changes; import it in Bambu Studio via File → Import → Import Configs)" })))
}

pub fn register(r: &mut Registry) {
    r.add(def::<ListIn>("preset_list", "List presets", "Process / filament / machine presets: user and only3d presets first, then compatible system presets, with source and parent.", false, false, t_list));
    r.add(def::<GetIn>("preset_get", "Read preset", "A preset's own settings (what it changes vs its parent), its parent and source; resolved: true adds every effective setting.", false, false, t_get));
    r.add(def::<CreateIn>("preset_create", "Create preset", "Create an only3d preset (Bambu format) from a base preset plus settings (Bambu or only3d keys) or the project's overrides.", true, true, t_create));
    r.add(def::<UpdateIn>("preset_update", "Update preset", "Change or remove settings of an only3d preset.", false, true, t_update));
    r.add(def::<DeleteIn>("preset_delete", "Delete preset", "Delete an only3d preset (Bambu Studio's own presets are never touched).", false, true, t_delete));
    r.add(def::<ImportIn>("preset_import", "Import presets", "Import presets: a Bambu / Orca preset .json, a bundle (.bbscfg/.bbsflmt/.zip) or a slicer project's changed settings (.3mf).", false, true, t_import));
    r.add(def::<ExportIn>("preset_export", "Export preset", "Write a preset as Bambu Studio JSON (inherits + changes, or fully resolved with flatten).", false, true, t_export));
}
