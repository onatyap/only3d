//! only3d application core.
//!
//! [`Studio`] is one editing session: the project document, the evaluated geometry, undo
//! history, the last slice and the printer link. Everything a person can do in the GUI is a
//! named **tool** with a JSON-Schema'd input ([`tools`]); the GUI, the CLI, the MCP server and
//! the live socket all call the same tools, so agents are first-class users, not an add-on.

pub mod agents;
pub mod bambu;
pub mod bambu_export;
pub mod combine;
pub mod describe;
pub mod inventory;
pub mod live;
pub mod machine;
pub mod presets;
pub mod approval;
pub mod arrange;
pub mod events;
pub mod fit;
pub mod printer;
pub mod render;
pub mod specs;
pub mod spool_names;
pub mod spools;
pub mod timing;
pub mod tools;
pub mod workspace;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use only3d_model::{Bed, EvaluatedObject, Evaluator, Finding, Project};
use serde::Serialize;

pub use tools::{Actor, ToolDef, ToolError, ToolOutput};

/// The output of the last successful slice.
pub struct SliceOutput {
    pub revision: u64,
    /// The plate that was sliced.
    pub plate: u32,
    pub result: only3d_slice::SliceResult,
    /// `.gcode.3mf` bytes, ready to upload.
    pub package: Vec<u8>,
    pub settings_source: String,
    pub engine: String,
    /// Independent check of the emitted G-code.
    pub lint: only3d_slice::gcode::LintReport,
}

#[derive(Clone, Debug, Serialize)]
pub struct JournalEntry {
    pub revision: u64,
    pub actor: Actor,
    pub tool: String,
    pub summary: String,
    pub ok: bool,
    pub at_ms: u128,
    /// Objects the call added, changed or was about (so the activity feed can point at them).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<String>,
    /// The full result (or error and hint) when it says more than `summary`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// The call's arguments as compact JSON (cut at 1500 characters), for copying.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub args: String,
}

impl JournalEntry {
    pub fn new(revision: u64, actor: Actor, tool: &str, summary: String, ok: bool) -> Self {
        JournalEntry { revision, actor, tool: tool.to_string(), summary, ok, at_ms: now_ms(), objects: Vec::new(), detail: String::new(), args: String::new() }
    }
}

/// Ids a call was about: `id`, `ids` or `object` in its arguments or its result data.
fn ids_in(v: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    for k in ["id", "object", "anchor"] {
        if let Some(x) = v.get(k).and_then(|x| x.as_str()) {
            out.push(x.to_string());
        }
    }
    for k in ["ids", "objects"] {
        for x in v.get(k).and_then(|x| x.as_array()).into_iter().flatten() {
            if let Some(x) = x.as_str() {
                out.push(x.to_string());
            }
        }
    }
    out
}

/// Something changed; the GUI repaints, live clients refresh.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    ProjectChanged { revision: u64, by: Actor, tool: String },
    SelectionChanged { ids: Vec<String> },
    Sliced { revision: u64 },
    PlateChanged { plate: u32 },
}

pub struct Studio {
    pub project: Project,
    pub project_path: Option<PathBuf>,
    pub evaluator: Evaluator,
    pub bed: Bed,
    pub revision: u64,
    pub selection: Vec<String>,
    pub journal: Vec<JournalEntry>,
    /// Standing mismatches between the printer and the inventory (an unregistered spool in a
    /// holder), from the last sync. Logged once; the Spools panel shows them until fixed.
    pub spool_notices: Vec<spools::SpoolNotice>,
    /// Who is calling the tool currently running.
    pub current_actor: Actor,
    pub last_slice: Option<Arc<SliceOutput>>,
    pub machine: Box<dyn machine::MachineProvider>,
    pub printer: Option<printer::PrinterSession>,
    evaluated: Option<(u64, Arc<Vec<EvaluatedObject>>)>,
    /// The active plate's share of `evaluated`, keyed by (revision, plate).
    evaluated_plate: Option<(u64, u32, Arc<Vec<EvaluatedObject>>)>,
    /// (printer, filament) → calibrated slip fit, so evaluation doesn't hit the database.
    pub fit_cache: Option<((String, String), f64)>,
    /// The plate being worked on (1-based): what the viewport shows and what slice, check, print,
    /// arrange and render act on. Session state like the selection, so switching isn't undone.
    pub plate: u32,
    undo: Vec<Project>,
    redo: Vec<Project>,
    listeners: Vec<std::sync::mpsc::Sender<Event>>,
    /// What happened, for agents to wait on (`wait_for_event`); shared with watcher threads.
    pub events: Arc<events::EventLog>,
    /// Inside `call`: project changes are logged once when the call ends, not per revision.
    in_call: bool,
    registry: Arc<tools::Registry>,
}

impl Default for Studio {
    fn default() -> Self {
        Self::new()
    }
}

impl Studio {
    pub fn new() -> Self {
        Studio {
            project: Project::default(),
            project_path: None,
            evaluator: Evaluator::new(std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
            bed: Bed::h2d(),
            revision: 0,
            selection: Vec::new(),
            journal: Vec::new(),
            spool_notices: Vec::new(),
            current_actor: Actor::User,
            last_slice: None,
            machine: machine::default_provider(),
            printer: None,
            evaluated: None,
            evaluated_plate: None,
            fit_cache: None,
            plate: 1,
            undo: Vec::new(),
            redo: Vec::new(),
            listeners: Vec::new(),
            events: Arc::new(events::EventLog::default()),
            in_call: false,
            registry: Arc::new(tools::Registry::builtin()),
        }
    }

    /// Open a project file (`*.only3d.json`).
    pub fn open(path: &Path) -> Result<Self, ToolError> {
        let mut s = Studio::new();
        s.load(path)?;
        Ok(s)
    }

    pub fn load(&mut self, path: &Path) -> Result<(), ToolError> {
        let text = std::fs::read_to_string(path).map_err(|e| ToolError::new("io_error", format!("cannot read {}: {e}", path.display()), "check the path"))?;
        let project: Project = serde_json::from_str(&text).map_err(|e| ToolError::new("bad_project", format!("{}: {e}", path.display()), "the file must be an only3d project (*.only3d.json)"))?;
        self.project = project;
        self.project_path = Some(path.to_path_buf());
        if self.project.id.is_empty() {
            // older files: keep the identity this path had before, or mint one (saved on next save)
            self.project.id = inventory::Db::open().ok().and_then(|db| db.project_id_for_path(&std::fs::canonicalize(path).unwrap_or(path.to_path_buf())).ok().flatten()).unwrap_or_else(only3d_model::new_project_id);
        }
        self.evaluator = Evaluator::new(path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")));
        self.evaluated = None;
        self.plate = 1;
        self.undo.clear();
        self.redo.clear();
        self.revision += 1;
        Ok(())
    }

    pub fn save(&mut self, path: Option<&Path>) -> Result<PathBuf, ToolError> {
        let p = match (path, &self.project_path) {
            (Some(p), _) => p.to_path_buf(),
            (None, Some(p)) => p.clone(),
            (None, None) => return Err(ToolError::new("no_path", "project has no file yet", "pass a path ending in .only3d.json")),
        };
        let text = serde_json::to_string_pretty(&self.project).expect("project serialises");
        std::fs::write(&p, text + "\n").map_err(|e| ToolError::new("io_error", format!("cannot write {}: {e}", p.display()), "check the folder exists and is writable"))?;
        if self.project_path.as_deref() != Some(p.as_path()) {
            self.evaluator.base_dir = p.parent().filter(|x| !x.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf();
        }
        self.project_path = Some(p.clone());
        if let Ok(db) = inventory::Db::open() {
            let _ = db.project_version(&self.project, Some(&std::fs::canonicalize(&p).unwrap_or(p.clone())), "save");
        }
        Ok(p)
    }

    pub fn registry(&self) -> Arc<tools::Registry> {
        self.registry.clone()
    }

    /// Call a tool by name. Mutating tools are atomic: on error the project is restored.
    pub fn call(&mut self, name: &str, args: serde_json::Value, actor: Actor) -> Result<ToolOutput, ToolError> {
        let reg = self.registry.clone();
        let tool = reg.get(name).ok_or_else(|| {
            let mut names: Vec<_> = reg.tools.iter().map(|t| t.name).collect();
            names.sort();
            ToolError::new("unknown_tool", format!("no tool named '{name}'"), format!("available: {}", names.join(", ")))
        })?;
        self.current_actor = actor;
        let before = if tool.mutates { Some(self.project.clone()) } else { None };
        let rev_before = self.revision;
        let outer = !self.in_call;
        self.in_call = true;
        let mut about = ids_in(&args);
        let args_text: String = match &args {
            serde_json::Value::Null => String::new(),
            serde_json::Value::Object(m) if m.is_empty() => String::new(),
            a => a.to_string().chars().take(1500).collect(),
        };
        let res = (tool.handler)(self, args);
        let ok = res.is_ok();
        if let Ok(o) = &res {
            about.extend(ids_in(&o.data));
        }
        match (&res, before) {
            (Ok(_), Some(prev)) => {
                if prev != self.project {
                    // objects this call added or changed
                    for o in &self.project.objects {
                        if prev.objects.iter().find(|p| p.id == o.id) != Some(o) {
                            about.push(o.id.clone());
                        }
                    }
                    self.undo.push(prev);
                    if self.undo.len() > 200 {
                        self.undo.remove(0);
                    }
                    self.redo.clear();
                    self.touch(actor, name);
                }
            }
            (Err(_), Some(prev)) => {
                self.project = prev;
                // Revisions only ever increase: caches (evaluation, slice) keyed by a revision
                // seen during the failed call must never match the restored state.
                if self.revision != rev_before {
                    self.revision += 1;
                    self.evaluated = None;
                }
            }
            _ => {}
        }
        let (summary, full): (String, String) = match &res {
            Ok(o) => (o.text.lines().next().unwrap_or("").chars().take(160).collect(), o.text.trim().to_string()),
            Err(e) => (format!("error {}: {}", e.code, e.message), format!("error {}: {}\n→ {}", e.code, e.message, e.hint)),
        };
        let mut entry = JournalEntry::new(self.revision, actor, name, summary, ok);
        if full.chars().count() > entry.summary.chars().count() {
            entry.detail = full.chars().take(1500).collect();
        }
        let mut objects: Vec<String> = Vec::new();
        for id in about {
            if !objects.contains(&id) && self.project.objects.iter().any(|o| o.id == id) {
                objects.push(id);
            }
        }
        entry.objects = objects;
        entry.args = args_text;
        self.journal.push(entry);
        if self.journal.len() > 1000 {
            self.journal.drain(..200);
        }
        if self.plate > self.project.plate_count() {
            let n = self.project.plate_count();
            self.set_plate(n);
        }
        if outer {
            self.in_call = false;
            // one event per call that changed the project, however many revisions it took
            if ok && self.revision != rev_before {
                self.events.push("project_changed", Some(actor), format!("{name} (rev {})", self.revision), serde_json::json!({"revision": self.revision, "tool": name}));
            }
        }
        res
    }

    /// Mark the project as changed by whoever is calling the current tool.
    /// Record something that is not a tool call (e.g. the user approving an agent's request) in
    /// the session journal, so the activity feed shows it.
    pub fn note(&mut self, actor: Actor, what: &str, summary: &str, ok: bool) {
        self.journal.push(JournalEntry::new(self.revision, actor, what, summary.to_string(), ok));
        self.events.push(what, Some(actor), summary, serde_json::json!({"ok": ok}));
    }

    pub fn changed(&mut self, tool: &str) {
        self.touch(self.current_actor, tool);
    }

    /// Mark the project as changed (bumps revision, notifies listeners).
    pub fn touch(&mut self, actor: Actor, tool: &str) {
        self.revision += 1;
        self.emit(Event::ProjectChanged { revision: self.revision, by: actor, tool: tool.to_string() });
    }

    pub fn undo(&mut self) -> bool {
        if let Some(p) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.project, p));
            self.revision += 1;
            self.emit(Event::ProjectChanged { revision: self.revision, by: Actor::User, tool: "undo".into() });
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if let Some(p) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.project, p));
            self.revision += 1;
            self.emit(Event::ProjectChanged { revision: self.revision, by: Actor::User, tool: "redo".into() });
            true
        } else {
            false
        }
    }

    pub fn can_undo(&self) -> (bool, bool) {
        (!self.undo.is_empty(), !self.redo.is_empty())
    }

    pub fn subscribe(&mut self) -> std::sync::mpsc::Receiver<Event> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.listeners.push(tx);
        rx
    }

    pub fn emit(&mut self, e: Event) {
        match &e {
            // inside a tool call it is logged once when the call ends (see `call`)
            Event::ProjectChanged { revision, by, tool } if !self.in_call => {
                self.events.push("project_changed", Some(*by), format!("{tool} (rev {revision})"), serde_json::json!({"revision": revision, "tool": tool}));
            }
            Event::ProjectChanged { .. } => {}
            Event::SelectionChanged { ids } => {
                let what = if ids.is_empty() { "nothing".to_string() } else { ids.join(", ") };
                self.events.push("selection_changed", Some(self.current_actor), format!("selected {what}"), serde_json::json!({"ids": ids}));
            }
            Event::PlateChanged { plate } => {
                let name = self.project.plate_name(*plate);
                self.events.push("plate_changed", Some(self.current_actor), format!("working on {name}"), serde_json::json!({"plate": plate, "name": name}));
            }
            Event::Sliced { revision } => {
                let summary = self.last_slice.as_ref().map(|s| format!("{} layers, {:.0} min, {:.1} g (rev {revision})", s.result.stats.layers, s.result.stats.print_time_s / 60.0, s.result.stats.filament_g)).unwrap_or_else(|| format!("rev {revision}"));
                self.events.push("sliced", Some(self.current_actor), summary, serde_json::json!({"revision": revision}));
            }
        }
        self.listeners.retain(|l| l.send(e.clone()).is_ok());
    }

    pub fn set_selection(&mut self, ids: Vec<String>) {
        if ids != self.selection {
            self.selection = ids.clone();
            self.emit(Event::SelectionChanged { ids });
        }
    }

    /// Evaluated, placed objects for the current revision (memoised).
    /// The active plate's objects, evaluated: what slicing, checks, printing and the viewport
    /// work on.
    pub fn evaluated(&mut self) -> Result<Arc<Vec<EvaluatedObject>>, ToolError> {
        if let Some((rev, plate, objs)) = &self.evaluated_plate {
            if *rev == self.revision && *plate == self.plate {
                return Ok(objs.clone());
            }
        }
        let all = self.evaluated_all()?;
        let objs: Arc<Vec<EvaluatedObject>> = Arc::new(all.iter().filter(|o| o.plate == self.plate).cloned().collect());
        self.evaluated_plate = Some((self.revision, self.plate, objs.clone()));
        Ok(objs)
    }

    /// Every object on every plate, evaluated.
    pub fn evaluated_all(&mut self) -> Result<Arc<Vec<EvaluatedObject>>, ToolError> {
        if let Some((rev, objs)) = &self.evaluated {
            if *rev == self.revision {
                return Ok(objs.clone());
            }
        }
        // holes get the slip fit calibrated for this printer and the first filament
        let fit = self.calibrated_fit();
        if (fit - self.evaluator.fit_mm).abs() > 1e-9 {
            self.evaluator.fit_mm = fit;
            self.evaluator.clear_cache();
        }
        let objs = Arc::new(self.project.evaluate(&mut self.evaluator, &self.bed).map_err(ToolError::from)?);
        self.evaluated = Some((self.revision, objs.clone()));
        Ok(objs)
    }

    /// The calibrated slip fit (mm added to holes) for this printer and filament slot 1, read
    /// once per (printer, filament) and after `fit` records a new one.
    pub fn calibrated_fit(&mut self) -> f64 {
        let key = (self.project.printer.machine.clone(), self.project.filaments.first().map(|f| f.preset.clone()).unwrap_or_default());
        if let Some((k, v)) = &self.fit_cache {
            if *k == key {
                return *v;
            }
        }
        // unit tests never read the user's inventory (integration tests set ONLY3D_HOME)
        let v = (!cfg!(test))
            .then(inventory::Db::open)
            .and_then(Result::ok)
            .and_then(|db| db.conn.query_row("SELECT offset FROM fits WHERE printer=?1 AND filament=?2 AND fit='slip'", [&key.0, &key.1], |r| r.get::<_, f64>(0)).ok())
            .unwrap_or(0.0);
        self.fit_cache = Some((key, v));
        v
    }

    /// Work on another plate. Objects not on it leave the selection.
    pub fn set_plate(&mut self, plate: u32) {
        let plate = plate.clamp(1, self.project.plate_count());
        if plate == self.plate {
            return;
        }
        self.plate = plate;
        let on: Vec<String> = self.project.objects.iter().filter(|o| o.plate == plate).map(|o| o.id.clone()).collect();
        let before = self.selection.len();
        self.selection.retain(|id| on.contains(id));
        if self.selection.len() != before {
            let ids = self.selection.clone();
            self.emit(Event::SelectionChanged { ids });
        }
        self.emit(Event::PlateChanged { plate });
    }

    pub fn findings(&mut self) -> Result<Vec<Finding>, ToolError> {
        let objs = self.evaluated()?;
        let mut f = only3d_model::check(&self.project, &objs, &self.bed);
        f.extend(specs::run(self));
        Ok(f)
    }

    pub fn slice_is_current(&self) -> bool {
        self.last_slice.as_ref().is_some_and(|s| s.revision == self.revision && s.plate == self.plate)
    }
}

pub fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

/// Where only3d keeps user config (printers, preferences): `~/.only3d/`.
pub fn config_dir() -> PathBuf {
    std::env::var_os("ONLY3D_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::home_dir().unwrap_or_default().join(".only3d"))
}
