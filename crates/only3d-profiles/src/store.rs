//! Loading Bambu Studio's and OrcaSlicer's vendor presets and resolving `inherits` + `include`.
//!
//! # Vendors
//!
//! Bambu's bundle (`BBL`) and OrcaSlicer's other makers (`Creality`, `Prusa`, `Voron`, ...) share
//! one store. Every maker has its own base presets, and their names repeat across makers
//! (`fdm_filament_pla`), so a preset's `inherits` and `include` resolve within its own maker
//! first, then in Orca's shared filament library (`OrcaFilamentLibrary`), then anywhere. The
//! presets a user picks have unique names across all makers.
//!
//! # Resolution order (matches Bambu Studio)
//!
//! From `PresetBundle::load_vendor_configs_from_json` in
//! `src/libslic3r/PresetBundle.cpp` (BambuStudio master, the `parse_config` lambda):
//!
//! ```text
//! config = resolved(inherits)            // or the built-in defaults when there is no parent
//! for name in include (in list order):   // later includes override earlier ones
//!     config.apply_only(include_config, keys where include_config differs from defaults)
//! config.apply(own keys)                 // the preset's own keys always win
//! ```
//!
//! so precedence is **own keys > includes (last wins) > inherits chain**. Includes are looked up
//! in the same preset collection (machine includes are machine-type files). An include whose name
//! contains `gcode` and has `instantiation: "false"` is a *pure template*: only its own keys are
//! used. Other includes (e.g. the filament template `fdm_filament_template_direct_dual_e3d`) are
//! stored fully resolved. Because parents are resolved before children, includes declared on a
//! parent propagate to all descendants.
//!
//! The one simplification here: Bambu applies only include keys that *differ from its built-in
//! defaults*; this crate has no defaults table and applies all keys of the (resolved) include.
//! For every include shipped in the BBL bundle this is identical. Unlike Bambu (which only logs
//! an error) a missing include is a hard error, because silently dropping it is exactly what makes
//! a naive loader fall back to the generic `fdm_bbl_3dp_002_common` start G-code.
//!
//! Meta keys (`name`, `type`, `from`, `inherits`, `include`, `instantiation`, `setting_id`,
//! `description`, ...) are not part of the resolved [`Config`]; `filament_id` is kept and
//! inherited like Bambu's `filament_id_maps`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::config::{parse_f64, Config, ConfigValue};
use crate::error::ProfileError;
use crate::template::{self, Context, Value};

include!(concat!(env!("OUT_DIR"), "/embedded_profiles.rs"));

/// Bambu's bundle.
pub const BAMBU_VENDOR: &str = "BBL";
/// OrcaSlicer's filament library, which every other maker's filaments may inherit from.
pub const LIBRARY_VENDOR: &str = "OrcaFilamentLibrary";

/// `(path, JSON)` for every embedded preset file: Bambu's as `machine/...`, Orca's other makers
/// as `orca/<Vendor>/machine/...`.
fn embedded_entries(orca: bool) -> Result<Vec<(String, String)>, ProfileError> {
    use std::io::Read;
    let bad = |e: &dyn std::fmt::Display| ProfileError::Io { path: PathBuf::from("<embedded profiles>"), source: std::io::Error::other(e.to_string()) };
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(EMBEDDED_ZIP)).map_err(|e| bad(&e))?;
    let mut out = Vec::with_capacity(zip.len());
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| bad(&e))?;
        if f.name().starts_with("orca/") != orca {
            continue;
        }
        let mut text = String::new();
        f.read_to_string(&mut text).map_err(|e| bad(&e))?;
        out.push((f.name().to_string(), text));
    }
    Ok(out)
}

/// One embedded file's text, by its archive name (Orca's presets are read when first used).
fn embedded_entry(name: &str) -> Option<String> {
    use std::io::Read;
    type Archive = zip::ZipArchive<std::io::Cursor<&'static [u8]>>;
    static ZIP: OnceLock<Option<Mutex<Archive>>> = OnceLock::new();
    let zip = ZIP.get_or_init(|| zip::ZipArchive::new(std::io::Cursor::new(EMBEDDED_ZIP)).ok().map(Mutex::new)).as_ref()?;
    let mut zip = zip.lock().unwrap_or_else(|p| p.into_inner());
    let mut f = zip.by_name(name).ok()?;
    let mut text = String::new();
    f.read_to_string(&mut text).ok()?;
    Some(text)
}

/// A preset in the store: parsed when added (Bambu's, the user's) or when first used (Orca's,
/// from the embedded archive), so a store of 14,000 presets costs only what is used.
#[derive(Debug)]
struct Slot {
    kind: PresetKind,
    vendor: String,
    name: String,
    instantiable: bool,
    /// The archive entry to parse it from, for a preset not parsed yet.
    entry: Option<String>,
    cell: OnceLock<Option<RawPreset>>,
}

impl Slot {
    fn ready(preset: RawPreset) -> Arc<Slot> {
        let slot = Slot { kind: preset.kind, vendor: preset.vendor.clone(), name: preset.name.clone(), instantiable: preset.is_instantiable(), entry: None, cell: OnceLock::new() };
        let _ = slot.cell.set(Some(preset));
        Arc::new(slot)
    }

    fn get(&self) -> Option<&RawPreset> {
        self.cell
            .get_or_init(|| {
                let entry = self.entry.as_deref()?;
                let rest = entry.strip_prefix("orca/")?.split_once('/')?.1;
                let mut p = RawPreset::parse(rest, &embedded_entry(entry)?).ok()??;
                p.vendor = self.vendor.clone();
                Some(p)
            })
            .as_ref()
    }
}

/// Preset collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PresetKind {
    /// Printer preset (`type: machine`, e.g. `Bambu Lab H2D 0.4 nozzle`) and its templates.
    Machine,
    /// Printer model description (`type: machine_model`, e.g. `Bambu Lab H2D`).
    MachineModel,
    /// Process / print settings (`0.20mm Standard @BBL H2D`).
    Process,
    /// Filament (`Bambu PLA Basic @BBL H2D`).
    Filament,
}

impl PresetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PresetKind::Machine => "machine",
            PresetKind::MachineModel => "machine_model",
            PresetKind::Process => "process",
            PresetKind::Filament => "filament",
        }
    }
}

impl fmt::Display for PresetKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Keys that describe the preset itself and never end up in a resolved [`Config`].
pub const META_KEYS: &[&str] = &[
    "name",
    "type",
    "from",
    "inherits",
    "include",
    "instantiation",
    "setting_id",
    "description",
    "version",
    "url",
    "is_custom_defined",
    "base_id",
    "user_id",
    "updated_time",
];

/// Keys old profiles gave as a percentage, dropped when a value is one (`handle_legacy`).
const LEGACY_PERCENT_KEYS: &[&str] = &["initial_layer_print_height", "initial_layer_speed", "internal_solid_infill_speed", "top_surface_speed", "support_interface_speed", "outer_wall_speed", "support_object_xy_distance"];

/// One preset file as stored on disk (own keys only).
#[derive(Clone, Debug)]
pub struct RawPreset {
    pub kind: PresetKind,
    pub name: String,
    /// Path relative to the vendor directory, e.g. `machine/Bambu Lab H2D 0.4 nozzle.json`.
    pub path: String,
    pub inherits: Option<String>,
    pub includes: Vec<String>,
    /// `instantiation` field (`"true"` for user-selectable presets).
    pub instantiation: Option<String>,
    pub from: Option<String>,
    pub setting_id: Option<String>,
    pub description: Option<String>,
    /// The preset's own (non-meta) keys.
    pub config: Config,
    /// The maker bundle it comes from (`BBL`, `Creality`, ...); empty for the user's own presets.
    pub vendor: String,
}

impl RawPreset {
    /// True for presets a user can select (`instantiation: "true"`).
    pub fn is_instantiable(&self) -> bool {
        self.instantiation.as_deref() == Some("true")
    }

    /// Parses one preset JSON document. `rel_path` decides the kind (`machine/`, `process/`,
    /// `filament/`); machine files with `type: machine_model` become [`PresetKind::MachineModel`].
    pub fn parse(rel_path: &str, json: &str) -> Result<Option<RawPreset>, ProfileError> {
        let top = rel_path.split(['/', '\\']).next().unwrap_or("");
        let dir_kind = match top {
            "machine" => PresetKind::Machine,
            "process" => PresetKind::Process,
            "filament" => PresetKind::Filament,
            _ => return Ok(None),
        };
        let value: serde_json::Value = serde_json::from_str(json.trim_start_matches('\u{feff}'))
            .map_err(|source| ProfileError::Json { path: rel_path.to_string(), source })?;
        let serde_json::Value::Object(obj) = value else {
            return Err(ProfileError::NotAnObject { path: rel_path.to_string() });
        };
        let get = |k: &str| obj.get(k).and_then(|v| v.as_str()).map(str::to_string);
        let kind = match (dir_kind, get("type").as_deref()) {
            (PresetKind::Machine, Some("machine_model")) => PresetKind::MachineModel,
            (k, _) => k,
        };
        let name = get("name").unwrap_or_else(|| {
            let file = rel_path.rsplit(['/', '\\']).next().unwrap_or(rel_path);
            file.strip_suffix(".json").unwrap_or(file).to_string()
        });
        let includes = match obj.get("include") {
            Some(serde_json::Value::String(s)) if !s.is_empty() => vec![s.clone()],
            Some(serde_json::Value::Array(items)) => {
                items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
            }
            _ => Vec::new(),
        };
        let mut config = Config::new();
        for (k, v) in &obj {
            // Bambu's and Orca's `handle_legacy`: old profiles gave these as a percentage, which
            // no longer means anything; the key is dropped so the parent's value applies
            let percent = |v: &serde_json::Value| match v {
                serde_json::Value::String(s) => s.contains('%'),
                serde_json::Value::Array(a) => a.iter().any(|x| x.as_str().is_some_and(|s| s.contains('%'))),
                _ => false,
            };
            if LEGACY_PERCENT_KEYS.contains(&k.as_str()) && percent(v) {
                continue;
            }
            if !META_KEYS.contains(&k.as_str()) {
                config.insert(k.clone(), ConfigValue::from_json(v));
            }
        }
        Ok(Some(RawPreset {
            kind,
            name,
            path: rel_path.replace('\\', "/"),
            inherits: get("inherits").filter(|s| !s.is_empty()),
            includes,
            instantiation: get("instantiation"),
            from: get("from"),
            setting_id: get("setting_id"),
            description: get("description"),
            config,
            vendor: String::new(),
        }))
    }
}

/// Where a [`ProfileStore`] was loaded from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileSource {
    /// A vendor directory on disk (containing `machine/`, `process/`, `filament/`).
    Directory(PathBuf),
    /// The copy compiled into this crate (`<workspace>/profiles/bambu`).
    Embedded,
}

/// Options for [`ProfileStore::list`].
#[derive(Clone, Debug, Default)]
pub struct ListFilter<'a> {
    /// Include abstract presets (`instantiation != "true"`, e.g. `fdm_filament_pla`, templates).
    pub include_abstract: bool,
    /// Only presets compatible with this printer preset (Bambu's rule: `compatible_printers`
    /// contains the printer name, else `compatible_printers_condition` evaluates true, else
    /// compatible). Only meaningful for process and filament presets.
    pub compatible_printer: Option<&'a str>,
    /// Only names containing this substring (e.g. `"@BBL H2D"`; note it also matches `@BBL H2DP`).
    pub name_contains: Option<&'a str>,
}

impl<'a> ListFilter<'a> {
    /// User-selectable presets compatible with `printer` (e.g. `"Bambu Lab H2D 0.4 nozzle"`).
    pub fn compatible_with(printer: &'a str) -> Self {
        ListFilter { compatible_printer: Some(printer), ..Default::default() }
    }
}

/// Presets of one or more vendor bundles, indexed by kind and name (the first preset of a name
/// wins) and by kind, vendor and name (for resolving each maker's own base presets).
#[derive(Clone, Debug)]
pub struct ProfileStore {
    source: ProfileSource,
    presets: BTreeMap<(PresetKind, String), Arc<Slot>>,
    scoped: HashMap<(PresetKind, String, String), Arc<Slot>>,
}

/// Environment variable that overrides the Bambu profile directory used by [`ProfileStore::system`].
pub const PROFILE_DIR_ENV: &str = "ONLY3D_BAMBU_PROFILES";

impl ProfileStore {
    /// An empty store (add presets with [`ProfileStore::insert_json`]).
    pub fn empty(source: ProfileSource) -> Self {
        ProfileStore { source, presets: BTreeMap::new(), scoped: HashMap::new() }
    }

    pub fn source(&self) -> &ProfileSource {
        &self.source
    }

    /// Loads every preset under a vendor directory. Accepts the vendor directory itself
    /// (`.../profiles/BBL`, containing `machine/`, `process/`, `filament/`) or its parent
    /// (`.../profiles`, containing `BBL/`).
    pub fn load_dir(path: impl AsRef<Path>) -> Result<Self, ProfileError> {
        let mut dir = path.as_ref().to_path_buf();
        let is_vendor = |d: &Path| ["machine", "process", "filament"].iter().any(|k| d.join(k).is_dir());
        if !is_vendor(&dir) && is_vendor(&dir.join("BBL")) {
            dir = dir.join("BBL");
        }
        if !is_vendor(&dir) {
            return Err(ProfileError::NotAProfileDir(dir));
        }
        let mut files = Vec::new();
        for kind in ["machine", "process", "filament"] {
            collect_json(&dir.join(kind), &dir, &mut files)?;
        }
        files.sort();
        let vendor = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| BAMBU_VENDOR.into());
        let mut store = ProfileStore::empty(ProfileSource::Directory(dir.clone()));
        for rel in files {
            let full = dir.join(&rel);
            let text = std::fs::read_to_string(&full)
                .map_err(|source| ProfileError::Io { path: full.clone(), source })?;
            store.insert_vendor_json(&vendor, &rel, &text)?;
        }
        Ok(store)
    }

    /// The presets compiled into this crate: Bambu Studio's whole BBL bundle and OrcaSlicer's
    /// other makers (every printer, process and filament; see `profiles/bambu/README.md` and
    /// `profiles/orca/README.md`).
    pub fn embedded() -> Result<Self, ProfileError> {
        let mut store = ProfileStore::empty(ProfileSource::Embedded);
        for (rel, text) in embedded_entries(false)? {
            store.insert_vendor_json(BAMBU_VENDOR, &rel, &text)?;
        }
        store.add_embedded_orca()?;
        Ok(store)
    }

    /// Adds the embedded OrcaSlicer makers (everything but Bambu's own bundle), each preset
    /// parsed when first used.
    pub fn add_embedded_orca(&mut self) -> Result<(), ProfileError> {
        for line in ORCA_INDEX.lines() {
            let mut f = line.split('\t');
            let (Some(entry), Some(kind), Some(name), inst) = (f.next(), f.next(), f.next(), f.next()) else { continue };
            let kind = match kind {
                "machine" => PresetKind::Machine,
                "machine_model" => PresetKind::MachineModel,
                "process" => PresetKind::Process,
                "filament" => PresetKind::Filament,
                _ => continue,
            };
            let Some(vendor) = entry.strip_prefix("orca/").and_then(|r| r.split_once('/')).map(|(v, _)| v.to_string()) else { continue };
            let slot = Arc::new(Slot { kind, vendor, name: name.to_string(), instantiable: inst == Some("true"), entry: Some(entry.to_string()), cell: OnceLock::new() });
            self.scoped.entry((kind, slot.vendor.clone(), slot.name.clone())).or_insert_with(|| slot.clone());
            self.presets.entry((kind, slot.name.clone())).or_insert(slot);
        }
        Ok(())
    }

    /// Paths (relative to `profiles/bambu`) of the embedded Bambu preset files.
    pub fn embedded_files() -> Result<Vec<String>, ProfileError> {
        Ok(embedded_entries(false)?.into_iter().map(|(p, _)| p).collect())
    }

    /// The Bambu Studio presets of this machine, falling back to the embedded copy.
    ///
    /// Search order: `$ONLY3D_BAMBU_PROFILES`; then whichever of Bambu Studio's *user data*
    /// copy (`<data dir>/BambuStudio/system/BBL`, which Bambu Studio itself loads and updates
    /// online) and the *install* copy (`.../BambuStudio.app/Contents/Resources/profiles/BBL`)
    /// has the higher `BBL.json` version (ties prefer the data copy); then [`ProfileStore::embedded`].
    pub fn system() -> Result<Self, ProfileError> {
        for dir in Self::system_dirs() {
            if let Ok(mut store) = Self::load_dir(&dir) {
                if store.contains(PresetKind::Machine, "fdm_machine_common") {
                    // a Bambu Studio installation brings Bambu's printers; the other makers
                    // always come from the embedded copy
                    if dir.file_name().is_some_and(|n| n == BAMBU_VENDOR) {
                        store.add_embedded_orca()?;
                    }
                    return Ok(store);
                }
            }
        }
        Self::embedded()
    }

    /// Candidate vendor directories for [`ProfileStore::system`], best first.
    pub fn system_dirs() -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Some(p) = std::env::var_os(PROFILE_DIR_ENV) {
            out.push(PathBuf::from(p));
        }
        let mut found: Vec<(Vec<u32>, usize, PathBuf)> = Vec::new();
        for (rank, dir) in default_install_dirs().into_iter().enumerate() {
            if dir.join("machine").is_dir() {
                found.push((bundle_version(&dir).unwrap_or_default(), rank, dir));
            }
        }
        // Highest version first, then the earlier candidate (data dir before install dir).
        found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        out.extend(found.into_iter().map(|(_, _, d)| d));
        out
    }

    /// Adds one preset JSON file of Bambu's bundle. `rel_path` must start with `machine/`,
    /// `process/` or `filament/`; other files are ignored. The first preset with a given name
    /// wins (as in Bambu's `config_maps.emplace`).
    pub fn insert_json(&mut self, rel_path: &str, json: &str) -> Result<(), ProfileError> {
        self.insert_vendor_json(BAMBU_VENDOR, rel_path, json)
    }

    /// [`ProfileStore::insert_json`] for a preset file of maker bundle `vendor`.
    pub fn insert_vendor_json(&mut self, vendor: &str, rel_path: &str, json: &str) -> Result<(), ProfileError> {
        if let Some(mut preset) = RawPreset::parse(rel_path, json)? {
            preset.vendor = vendor.to_string();
            let slot = Slot::ready(preset);
            self.scoped.entry((slot.kind, slot.vendor.clone(), slot.name.clone())).or_insert_with(|| slot.clone());
            self.presets.entry((slot.kind, slot.name.clone())).or_insert(slot);
        }
        Ok(())
    }

    /// Adds (or replaces) a preset.
    pub fn insert(&mut self, preset: RawPreset) {
        let slot = Slot::ready(preset);
        self.scoped.insert((slot.kind, slot.vendor.clone(), slot.name.clone()), slot.clone());
        self.presets.insert((slot.kind, slot.name.clone()), slot);
    }

    pub fn contains(&self, kind: PresetKind, name: &str) -> bool {
        self.presets.contains_key(&(kind, name.to_string()))
    }

    /// The unresolved preset.
    pub fn raw(&self, kind: PresetKind, name: &str) -> Option<&RawPreset> {
        self.presets.get(&(kind, name.to_string())).and_then(|p| p.get())
    }

    /// The preset a preset of maker `vendor` means by `name` (in `inherits` or `include`): its
    /// maker's own, else Orca's filament library's, else any.
    fn lookup(&self, kind: PresetKind, vendor: &str, name: &str) -> Option<&RawPreset> {
        if vendor.is_empty() {
            return self.raw(kind, name);
        }
        let key = |v: &str| (kind, v.to_string(), name.to_string());
        self.scoped
            .get(&key(vendor))
            .or_else(|| self.scoped.get(&key(LIBRARY_VENDOR)))
            .and_then(|p| p.get())
            .or_else(|| self.raw(kind, name))
    }

    /// The maker bundle of printer preset `printer` (empty for the user's own presets that
    /// don't inherit from one).
    pub fn vendor_of(&self, printer: &str) -> String {
        let mut seen = HashSet::new();
        let mut cur = self.raw(PresetKind::Machine, printer);
        while let Some(p) = cur {
            if !p.vendor.is_empty() || !seen.insert(p.name.clone()) {
                return p.vendor.clone();
            }
            cur = p.inherits.as_deref().and_then(|n| self.raw(PresetKind::Machine, n));
        }
        String::new()
    }

    fn raw_or_err(&self, kind: PresetKind, name: &str) -> Result<&RawPreset, ProfileError> {
        self.raw(kind, name)
            .ok_or_else(|| ProfileError::NotFound { kind, name: name.to_string() })
    }

    /// Fully resolved configuration of a preset (inherits chain + includes + own keys).
    pub fn resolve(&self, kind: PresetKind, name: &str) -> Result<Config, ProfileError> {
        let mut stack = Vec::new();
        let raw = self.raw_or_err(kind, name)?;
        self.resolve_inner(raw, &mut stack)
    }

    fn resolve_inner(&self, raw: &RawPreset, stack: &mut Vec<String>) -> Result<Config, ProfileError> {
        let (kind, name) = (raw.kind, raw.name.as_str());
        if stack.iter().any(|n| n == name) || stack.len() > 64 {
            return Err(ProfileError::Cycle { kind, name: name.to_string() });
        }
        stack.push(name.to_string());
        let mut config = match &raw.inherits {
            Some(parent) => {
                let Some(p) = self.lookup(kind, &raw.vendor, parent) else {
                    return Err(ProfileError::MissingParent { kind, name: name.to_string(), parent: parent.clone() });
                };
                self.resolve_inner(p, stack)?
            }
            None => Config::new(),
        };
        for include in &raw.includes {
            let Some(inc) = self.lookup(kind, &raw.vendor, include) else {
                return Err(ProfileError::MissingInclude { kind, name: name.to_string(), include: include.clone() });
            };
            let inc = self.resolve_inner(inc, stack)?;
            config.merge(&inc);
        }
        config.merge(&raw.config);
        stack.pop();
        Ok(config)
    }

    /// The names a preset is built from, most-derived first:
    /// `[name, its includes.., parent, parent's includes.., ...]`.
    pub fn resolution_chain(&self, kind: PresetKind, name: &str) -> Result<Vec<String>, ProfileError> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut raw = self.raw_or_err(kind, name)?;
        loop {
            if !seen.insert(raw.name.clone()) {
                return Err(ProfileError::Cycle { kind, name: raw.name.clone() });
            }
            out.push(raw.name.clone());
            out.extend(raw.includes.iter().rev().cloned());
            let Some(parent) = &raw.inherits else { break };
            raw = self.lookup(kind, &raw.vendor, parent).ok_or_else(|| ProfileError::NotFound { kind, name: parent.clone() })?;
        }
        Ok(out)
    }

    /// Value of one key as resolution would produce it, without resolving the whole preset.
    pub fn effective_value(&self, kind: PresetKind, name: &str, key: &str) -> Option<ConfigValue> {
        self.effective_value_of(self.raw(kind, name)?, key, &mut HashSet::new())
    }

    fn effective_value_of(&self, raw: &RawPreset, key: &str, seen: &mut HashSet<String>) -> Option<ConfigValue> {
        let mut raw = raw;
        loop {
            if !seen.insert(raw.name.clone()) {
                return None;
            }
            if let Some(v) = raw.config.get(key) {
                return Some(v.clone());
            }
            for inc in raw.includes.iter().rev() {
                if let Some(v) = self.lookup(raw.kind, &raw.vendor, inc).and_then(|i| self.effective_value_of(i, key, seen)) {
                    return Some(v);
                }
            }
            raw = self.lookup(raw.kind, &raw.vendor, raw.inherits.as_deref()?)?;
        }
    }

    /// Presets of `kind` matching `filter`, sorted by name.
    pub fn list(&self, kind: PresetKind, filter: &ListFilter<'_>) -> Vec<&RawPreset> {
        let printer_ctx = filter
            .compatible_printer
            .and_then(|p| self.resolve(PresetKind::Machine, p).ok())
            .map(|cfg| Context::from_config_guess_types(&cfg));
        let printer_vendor = filter.compatible_printer.map(|p| self.vendor_of(p));
        // a user's preset takes the maker of the system preset it is made from
        let vendor = |p: &Slot| -> String {
            if !p.vendor.is_empty() {
                return p.vendor.clone();
            }
            let mut seen = HashSet::new();
            let mut cur = p.get();
            while let Some(r) = cur {
                if !r.vendor.is_empty() || !seen.insert(r.name.clone()) {
                    return r.vendor.clone();
                }
                cur = r.inherits.as_deref().and_then(|n| self.raw(r.kind, n));
            }
            String::new()
        };
        self.presets
            .range((kind, String::new())..)
            .take_while(|((k, _), _)| *k == kind)
            .map(|(_, p)| &**p)
            // a maker's presets go with its own printers; Orca's filament library with every
            // printer but Bambu's (which have their own generic filaments)
            .filter(|p| match &printer_vendor {
                Some(v) if !v.is_empty() => {
                    let pv = vendor(p);
                    pv.is_empty() || pv == *v || (pv == LIBRARY_VENDOR && v != BAMBU_VENDOR)
                }
                _ => true,
            })
            .filter(|p| filter.include_abstract || p.instantiable)
            .filter(|p| filter.name_contains.is_none_or(|s| p.name.contains(s)))
            .filter_map(|p| p.get())
            .filter(|p| match filter.compatible_printer {
                None => true,
                Some(printer) => self.is_compatible(p, printer, printer_ctx.as_ref()),
            })
            .collect()
    }

    /// Names of [`ProfileStore::list`].
    pub fn names(&self, kind: PresetKind, filter: &ListFilter<'_>) -> Vec<String> {
        self.list(kind, filter).into_iter().map(|p| p.name.clone()).collect()
    }

    /// Bambu's `PresetCollection::is_compatible_with_printer` rule.
    fn is_compatible(&self, p: &RawPreset, printer: &str, printer_ctx: Option<&Context>) -> bool {
        let list = self.effective_value(p.kind, &p.name, "compatible_printers");
        if let Some(list) = list.as_ref().filter(|l| !l.is_empty() && l.as_slice().iter().any(|s| !s.is_empty())) {
            return list.as_slice().iter().any(|s| s == printer);
        }
        let cond = self
            .effective_value(p.kind, &p.name, "compatible_printers_condition")
            .and_then(|v| v.first().map(str::to_string))
            .unwrap_or_default();
        if cond.trim().is_empty() {
            return true;
        }
        match printer_ctx {
            Some(ctx) => template::eval_condition(&cond, ctx).unwrap_or(false),
            None => false,
        }
    }

    /// Number of presets of `kind` (including abstract ones).
    pub fn count(&self, kind: PresetKind) -> usize {
        self.presets.keys().filter(|(k, _)| *k == kind).count()
    }
}

fn collect_json(dir: &Path, root: &Path, out: &mut Vec<String>) -> Result<(), ProfileError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(ProfileError::Io { path: dir.to_path_buf(), source }),
    };
    for entry in entries {
        let entry = entry.map_err(|source| ProfileError::Io { path: dir.to_path_buf(), source })?;
        let path = entry.path();
        if path.is_dir() {
            collect_json(&path, root, out)?;
        } else if path.extension().is_some_and(|e| e == "json") {
            if let Ok(rel) = path.strip_prefix(root) {
                let rel = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push(rel);
            }
        }
    }
    Ok(())
}

/// `BBL.json` (next to the vendor dir) version as numbers, e.g. `02.09.00.75` -> `[2, 9, 0, 75]`.
fn bundle_version(vendor_dir: &Path) -> Option<Vec<u32>> {
    let name = vendor_dir.file_name()?.to_string_lossy().into_owned();
    let index = vendor_dir.parent()?.join(format!("{name}.json"));
    let text = std::fs::read_to_string(index).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let version = v.get("version")?.as_str()?;
    Some(version.split('.').map(|p| p.parse().unwrap_or(0)).collect())
}

/// Default Bambu Studio locations: user data copy first, then the install copy.
fn default_install_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    let home = std::env::home_dir();
    if cfg!(target_os = "macos") {
        if let Some(h) = &home {
            v.push(h.join("Library/Application Support/BambuStudio/system/BBL"));
        }
        v.push(PathBuf::from("/Applications/BambuStudio.app/Contents/Resources/profiles/BBL"));
        if let Some(h) = &home {
            v.push(h.join("Applications/BambuStudio.app/Contents/Resources/profiles/BBL"));
        }
    } else if cfg!(target_os = "windows") {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            v.push(PathBuf::from(appdata).join("BambuStudio").join("system").join("BBL"));
        }
        for pf in ["ProgramFiles", "ProgramW6432"] {
            if let Some(p) = std::env::var_os(pf) {
                v.push(PathBuf::from(p).join("Bambu Studio").join("resources").join("profiles").join("BBL"));
            }
        }
    } else {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".config")));
        if let Some(c) = config {
            v.push(c.join("BambuStudio/system/BBL"));
        }
        if let Some(h) = &home {
            v.push(h.join(".var/app/com.bambulab.BambuStudio/config/BambuStudio/system/BBL"));
        }
        v.push(PathBuf::from("/opt/bambu-studio/resources/profiles/BBL"));
        v.push(PathBuf::from("/usr/share/BambuStudio/profiles/BBL"));
    }
    v
}

impl Context {
    /// Builds a template context from a resolved config, *guessing* types (integers -> int,
    /// other numbers -> float, everything else -> string; arrays likewise). Good enough for
    /// `compatible_*_condition` expressions; G-code templates need the exact types produced by
    /// [`crate::job::build_print_context`].
    pub fn from_config_guess_types(config: &Config) -> Context {
        fn guess(items: &[String]) -> Value {
            if !items.is_empty() && items.iter().all(|s| s.trim().parse::<i64>().is_ok()) {
                Value::Ints(items.iter().map(|s| s.trim().parse().unwrap_or(0)).collect())
            } else if !items.is_empty() && items.iter().all(|s| parse_f64(s).is_some()) {
                Value::Floats(items.iter().map(|s| parse_f64(s).unwrap_or(0.0)).collect())
            } else {
                Value::Strs(items.to_vec())
            }
        }
        // a single value stored as a one-element list (Prusa's `printer_notes`) is a single
        // value where Bambu's or OrcaSlicer's option type says so: `=~` needs a string
        let scalar = |k: &str| {
            use crate::option_types::OptType::*;
            match crate::gcode_config::option_type(k) {
                Some(t) => matches!(t, Float | Int | String | Percent | FloatOrPercent | Point | Point3 | Bool | Enum),
                None => crate::job::orca_type(k).is_some_and(|t| !t.ends_with('s')),
            }
        };
        let mut ctx = Context::new();
        for (k, v) in config.iter() {
            let value = match v {
                ConfigValue::Array(items) if items.len() == 1 && scalar(k) => match guess(items) {
                    Value::Ints(i) => Value::Int(i[0]),
                    Value::Floats(f) => Value::Float(f[0]),
                    _ => Value::Str(items[0].clone()),
                },
                ConfigValue::Array(items) => guess(items),
                ConfigValue::Scalar(s) => match guess(std::slice::from_ref(s)) {
                    Value::Ints(i) => Value::Int(i[0]),
                    Value::Floats(f) => Value::Float(f[0]),
                    _ => Value::Str(s.clone()),
                },
            };
            ctx.set(k, value);
        }
        ctx
    }
}
