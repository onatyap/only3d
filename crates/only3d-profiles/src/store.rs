//! Loading Bambu Studio vendor presets and resolving `inherits` + `include`.
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

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::config::{parse_f64, Config, ConfigValue};
use crate::error::ProfileError;
use crate::template::{self, Context, Value};

include!(concat!(env!("OUT_DIR"), "/embedded_profiles.rs"));

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
        let value: serde_json::Value = serde_json::from_str(json)
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

/// All presets of one vendor bundle, indexed by kind and name.
#[derive(Clone, Debug)]
pub struct ProfileStore {
    source: ProfileSource,
    presets: BTreeMap<(PresetKind, String), RawPreset>,
}

/// Environment variable that overrides the Bambu profile directory used by [`ProfileStore::system`].
pub const PROFILE_DIR_ENV: &str = "ONLY3D_BAMBU_PROFILES";

impl ProfileStore {
    /// An empty store (add presets with [`ProfileStore::insert_json`]).
    pub fn empty(source: ProfileSource) -> Self {
        ProfileStore { source, presets: BTreeMap::new() }
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
        let mut store = ProfileStore::empty(ProfileSource::Directory(dir.clone()));
        for rel in files {
            let full = dir.join(&rel);
            let text = std::fs::read_to_string(&full)
                .map_err(|source| ProfileError::Io { path: full.clone(), source })?;
            store.insert_json(&rel, &text)?;
        }
        Ok(store)
    }

    /// The presets compiled into this crate (H2D 0.4 nozzle, 0.20mm Standard @BBL H2D and a
    /// few Bambu filaments; see `profiles/bambu/README.md`).
    pub fn embedded() -> Result<Self, ProfileError> {
        let mut store = ProfileStore::empty(ProfileSource::Embedded);
        for (rel, text) in EMBEDDED_FILES {
            store.insert_json(rel, text)?;
        }
        Ok(store)
    }

    /// Names (relative paths) of the embedded preset files.
    pub fn embedded_files() -> impl Iterator<Item = &'static str> {
        EMBEDDED_FILES.iter().map(|(p, _)| *p)
    }

    /// The Bambu Studio presets of this machine, falling back to the embedded copy.
    ///
    /// Search order: `$ONLY3D_BAMBU_PROFILES`; then whichever of Bambu Studio's *user data*
    /// copy (`<data dir>/BambuStudio/system/BBL`, which Bambu Studio itself loads and updates
    /// online) and the *install* copy (`.../BambuStudio.app/Contents/Resources/profiles/BBL`)
    /// has the higher `BBL.json` version (ties prefer the data copy); then [`ProfileStore::embedded`].
    pub fn system() -> Result<Self, ProfileError> {
        for dir in Self::system_dirs() {
            if let Ok(store) = Self::load_dir(&dir) {
                if store.contains(PresetKind::Machine, "fdm_machine_common") {
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

    /// Adds one preset JSON file. `rel_path` must start with `machine/`, `process/` or
    /// `filament/`; other files are ignored. The first preset with a given name wins
    /// (as in Bambu's `config_maps.emplace`).
    pub fn insert_json(&mut self, rel_path: &str, json: &str) -> Result<(), ProfileError> {
        if let Some(preset) = RawPreset::parse(rel_path, json)? {
            self.presets.entry((preset.kind, preset.name.clone())).or_insert(preset);
        }
        Ok(())
    }

    /// Adds (or replaces) a preset.
    pub fn insert(&mut self, preset: RawPreset) {
        self.presets.insert((preset.kind, preset.name.clone()), preset);
    }

    pub fn contains(&self, kind: PresetKind, name: &str) -> bool {
        self.presets.contains_key(&(kind, name.to_string()))
    }

    /// The unresolved preset.
    pub fn raw(&self, kind: PresetKind, name: &str) -> Option<&RawPreset> {
        self.presets.get(&(kind, name.to_string()))
    }

    fn raw_or_err(&self, kind: PresetKind, name: &str) -> Result<&RawPreset, ProfileError> {
        self.raw(kind, name)
            .ok_or_else(|| ProfileError::NotFound { kind, name: name.to_string() })
    }

    /// Fully resolved configuration of a preset (inherits chain + includes + own keys).
    pub fn resolve(&self, kind: PresetKind, name: &str) -> Result<Config, ProfileError> {
        let mut stack = Vec::new();
        self.resolve_inner(kind, name, &mut stack)
    }

    fn resolve_inner(
        &self,
        kind: PresetKind,
        name: &str,
        stack: &mut Vec<String>,
    ) -> Result<Config, ProfileError> {
        if stack.iter().any(|n| n == name) || stack.len() > 64 {
            return Err(ProfileError::Cycle { kind, name: name.to_string() });
        }
        let raw = self.raw_or_err(kind, name)?;
        stack.push(name.to_string());
        let mut config = match &raw.inherits {
            Some(parent) => {
                if !self.contains(kind, parent) {
                    return Err(ProfileError::MissingParent {
                        kind,
                        name: name.to_string(),
                        parent: parent.clone(),
                    });
                }
                self.resolve_inner(kind, parent, stack)?
            }
            None => Config::new(),
        };
        for include in &raw.includes {
            if !self.contains(kind, include) {
                return Err(ProfileError::MissingInclude {
                    kind,
                    name: name.to_string(),
                    include: include.clone(),
                });
            }
            let inc = self.resolve_inner(kind, include, stack)?;
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
        let mut cur = Some(name.to_string());
        while let Some(n) = cur {
            if !seen.insert(n.clone()) {
                return Err(ProfileError::Cycle { kind, name: n });
            }
            let raw = self.raw_or_err(kind, &n)?;
            out.push(n);
            out.extend(raw.includes.iter().rev().cloned());
            cur = raw.inherits.clone();
        }
        Ok(out)
    }

    /// Value of one key as resolution would produce it, without resolving the whole preset.
    pub fn effective_value(&self, kind: PresetKind, name: &str, key: &str) -> Option<ConfigValue> {
        let mut seen = HashSet::new();
        let mut cur = Some(name.to_string());
        while let Some(n) = cur {
            if !seen.insert(n.clone()) {
                return None;
            }
            let raw = self.raw(kind, &n)?;
            if let Some(v) = raw.config.get(key) {
                return Some(v.clone());
            }
            for inc in raw.includes.iter().rev() {
                if let Some(v) = self.effective_value(kind, inc, key) {
                    return Some(v);
                }
            }
            cur = raw.inherits.clone();
        }
        None
    }

    /// Presets of `kind` matching `filter`, sorted by name.
    pub fn list(&self, kind: PresetKind, filter: &ListFilter<'_>) -> Vec<&RawPreset> {
        let printer_ctx = filter
            .compatible_printer
            .and_then(|p| self.resolve(PresetKind::Machine, p).ok())
            .map(|cfg| Context::from_config_guess_types(&cfg));
        self.presets
            .range((kind, String::new())..)
            .take_while(|((k, _), _)| *k == kind)
            .map(|(_, p)| p)
            .filter(|p| filter.include_abstract || p.is_instantiable())
            .filter(|p| filter.name_contains.is_none_or(|s| p.name.contains(s)))
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
        let mut ctx = Context::new();
        for (k, v) in config.iter() {
            let value = match v {
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
