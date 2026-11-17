//! The application state, per-frame session sync, keyboard dispatch, the viewport and direct
//! manipulation. Chrome (title bar, docks, status bar, dialogs) is in `chrome.rs`, panel contents
//! in `panels.rs` and `settings_ui.rs`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense};
use glam::Vec3;
use only3d_model::{Bed, EvaluatedObject, Finding, Object, Project};
use only3d_studio::{JournalEntry, SliceOutput, Studio};
use serde_json::{json, Value};

use crate::actions::{self, Gizmo, Plan};
use crate::camera::{OrbitCamera, ViewPreset};
use crate::config::UiConfig;
use crate::dock::{Layout, PanelId};
use crate::gpu::{self, DrawObj, FrameData, Globals, ObjInstance, PlateData, ToolpathData, ToolpathDraw, ViewportCallback};
use crate::keymap::{Action, Keymap, Mods, When};
use crate::palette::ToolInfo;
use crate::scene;
use crate::settings_ui::Catalog;
use crate::theme::{self, pal, Icon, Palette, ThemeChoice};
use crate::worker::{Done, Kind, Worker};
use crate::GuiOptions;

/// A copy of the session state the UI reads, refreshed whenever the studio changes.
pub struct Snapshot {
    pub revision: u64,
    pub project: Project,
    pub project_path: Option<PathBuf>,
    pub bed: Bed,
    pub objects: Arc<Vec<EvaluatedObject>>,
    pub eval_error: Option<String>,
    pub findings: Vec<Finding>,
    pub selection: Vec<String>,
    pub journal: Vec<JournalEntry>,
    pub journal_len: usize,
    pub journal_last: u128,
    pub last_slice: Option<Arc<SliceOutput>>,
    pub slice_current: bool,
    pub can_undo: (bool, bool),
    pub spool_notices: Vec<only3d_studio::spools::SpoolNotice>,
    /// The active plate (1-based); `objects` holds only its objects.
    pub plate: u32,
    /// Objects on the other plates (for the all-plates view).
    pub others: Arc<Vec<EvaluatedObject>>,
    /// The active plate's objects made from .scad files, with their OpenSCAD variables.
    pub scad: Vec<ScadObject>,
}

/// An object made from a .scad file: the file's customizer variables and the values it sets.
#[derive(Clone)]
pub struct ScadObject {
    pub id: String,
    pub name: String,
    pub file: String,
    pub vars: Arc<Vec<only3d_model::scad::ScadVar>>,
    pub set: BTreeMap<String, Value>,
    /// Why its variables can't be read (OpenSCAD missing, a syntax error).
    pub error: Option<String>,
}

impl Snapshot {
    fn empty() -> Self {
        Snapshot {
            revision: u64::MAX,
            project: Project::default(),
            project_path: None,
            bed: Bed::h2d(),
            objects: Arc::new(Vec::new()),
            eval_error: None,
            findings: Vec::new(),
            selection: Vec::new(),
            journal: Vec::new(),
            journal_len: 0,
            journal_last: 0,
            last_slice: None,
            slice_current: false,
            can_undo: (false, false),
            spool_notices: Vec::new(),
            plate: 1,
            others: Arc::new(Vec::new()),
            scad: Vec::new(),
        }
    }

    /// The active plate's objects (project order).
    pub fn plate_objects(&self) -> impl Iterator<Item = &Object> {
        self.project.objects.iter().filter(move |o| o.plate == self.plate)
    }

    pub fn plate_is_empty(&self) -> bool {
        self.plate_objects().next().is_none()
    }

    pub fn evaluated(&self, id: &str) -> Option<&EvaluatedObject> {
        self.objects.iter().find(|o| o.id == id)
    }

    pub fn filament_color(&self, slot: u32) -> Color32 {
        let c = self.project.filaments.get((slot as usize).wrapping_sub(1)).map(|f| scene::parse_hex(&f.color)).unwrap_or([200, 190, 170]);
        Color32::from_rgb(c[0], c[1], c[2])
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Model,
    Toolpaths,
}

pub enum LiveState {
    Off,
    Running(PathBuf),
    Failed(String),
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    /// Something the user should know (an import that lost colours): amber, stays longer.
    pub warn: bool,
    pub at: Instant,
}

/// The JSON editor for the selected object's shape tree.
#[derive(Default)]
pub struct ShapeEditor {
    pub id: String,
    pub text: String,
    /// The snapshot text the editor was loaded from.
    pub source: String,
    pub dirty: bool,
    pub error: Option<String>,
}

pub enum PrintStage {
    Checking,
    Ready(String),
    Failed(String),
    Sending,
    Sent(String),
    SendFailed(String),
}

/// Edit streams (drag values) are coalesced per key: at most one call in flight, the latest
/// values merged into the next one.
#[derive(Default)]
pub struct Throttle {
    pub pending: BTreeMap<String, (String, serde_json::Map<String, Value>)>,
}

impl Throttle {
    pub fn merge(&mut self, key: &str, tool: &str, args: Value) {
        let e = self.pending.entry(key.to_string()).or_insert_with(|| (tool.to_string(), serde_json::Map::new()));
        if let Value::Object(m) = args {
            for (k, v) in m {
                match (e.1.get_mut(&k), v) {
                    (Some(Value::Object(old)), Value::Object(new)) => {
                        for (k2, v2) in new {
                            old.insert(k2, v2);
                        }
                    }
                    (_, v) => {
                        e.1.insert(k, v);
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct ProcLocal {
    pub layer_height: f64,
    pub wall_loops: u32,
    pub infill_pct: f64,
    pub brim: f64,
    pub support: bool,
}

#[derive(Clone, Copy)]
pub struct XformLocal {
    pub pos: [f32; 2],
    pub rot: [f32; 3],
    pub scale: [f32; 3],
}

/// A preset as listed by `preset_list` (or `list_presets`, which has no source).
#[derive(Clone, Debug, PartialEq)]
pub struct PresetInfo {
    pub name: String,
    pub source: String,
}

/// Dialogs that sit over the window (one at a time).
pub enum Dialog {
    Rename { id: String, text: String, focus: bool },
    SavePreset { kind: String, name: String, focus: bool },
    ToolForm(ToolForm),
    Confirm { title: String, body: String, tool: String, args: Value },
    Copies(crate::design_ui::CopiesForm),
    /// File > Revert to Version: the project's saved versions.
    Versions(Vec<Value>),
    /// only3d > Connect an Agent.
    ConnectAgent(crate::app_menus::AgentSetup),
    /// Help > Agent Tools Reference, with its filter text.
    Tools(String),
    /// The Save panel (project saves and every export).
    Save(crate::save_panel::SaveDialog),
}

pub struct ToolForm {
    pub tool: ToolInfo,
    pub text: String,
    pub result: Option<Result<String, String>>,
    pub running: bool,
    pub focus: bool,
}

#[derive(Default)]
pub struct PaletteUi {
    pub open: bool,
    pub query: String,
    pub selected: usize,
    pub focus: bool,
    /// Most recently used first.
    pub recent: Vec<String>,
}

#[derive(Default)]
pub struct SpoolForm {
    pub material: String,
    pub color: Color32,
    pub grams: f64,
    pub location: String,
    pub open: bool,
    /// Bambu filament preset to slice with ("" = pick later).
    pub preset: String,
    /// Empty-spool weight, for weigh-ins.
    pub tare: Option<f64>,
    pub cost_per_kg: Option<f64>,
    pub brand: String,
    /// Product line ("PETG Translucent"); "" = the material.
    pub line: String,
    /// "" = automatic (brand + line + colour).
    pub name: String,
    /// The spool being edited (None: adding a new one).
    pub editing: Option<String>,
    /// Bambu filament id of the edited spool, for its colour names.
    pub filament_id: Option<String>,
}

/// The workspace panel's state: search, open folders, cached listings and prints.
#[derive(Default)]
pub struct WorkspaceUi {
    pub query: String,
    pub open_dirs: std::collections::HashSet<PathBuf>,
    pub listings: HashMap<PathBuf, (Instant, Result<Vec<only3d_studio::workspace::Entry>, String>)>,
    /// The chosen workspace folder (`None` inside: no folder open), re-read every few seconds.
    pub root: Option<(Instant, Option<PathBuf>)>,
    /// Plates folded in the tree.
    pub folded: std::collections::HashSet<u32>,
    /// Image thumbnails for the Files panel's hover card.
    pub thumbs: HashMap<PathBuf, crate::workspace_panel::Thumb>,
    pub prints: Option<(Instant, Vec<Value>)>,
    /// A print opened for details: (id, text).
    pub print_open: Option<(String, String)>,
}

/// Object ids dragged from the workspace tree (dropped on a plate row to move them there).
pub struct DragObjects(pub Vec<String>);

/// Files dragged from the Files panel: models dropped on a plate row, the Plates panel or the
/// viewport are imported there; other files open as tabs in the main area.
pub struct DragFiles(pub Vec<PathBuf>);

impl DragFiles {
    /// Every file is something to import (a model or a project).
    pub fn all_models(&self) -> bool {
        self.0.iter().all(|p| matches!(only3d_studio::workspace::kind_of(&p.to_string_lossy()), only3d_studio::workspace::FileKind::Model))
    }
}

/// The same ids, in any order.
fn same_set(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().all(|x| b.contains(x))
}

/// A file drag from the Files panel over `rect`: outline it, and on release hand over the files.
pub fn file_drop_zone(ui: &egui::Ui, rect: Rect) -> Option<Vec<PathBuf>> {
    let payload = egui::DragAndDrop::payload::<DragFiles>(ui.ctx()).filter(|f| f.all_models())?;
    if !ui.ctx().pointer_latest_pos().is_some_and(|p| rect.contains(p)) {
        return None;
    }
    let p = pal();
    ui.painter().rect_stroke(rect.shrink(2.0), 4.0, egui::Stroke::new(1.5, p.accent), egui::StrokeKind::Inside);
    if ui.ctx().input(|i| i.pointer.any_released()) {
        egui::DragAndDrop::clear_payload(ui.ctx());
        return Some(payload.0.clone());
    }
    None
}

/// A live drag of selected objects (move), of the rotate ring, or of a size handle.
#[derive(Clone, Debug, Default)]
struct ObjDrag {
    ids: Vec<String>,
    rotate: bool,
    /// World point grabbed (on the plane z = `plane_z`).
    start: Vec3,
    plane_z: f32,
    /// 0 = x only, 1 = y only.
    axis: Option<usize>,
    pivot: [f32; 2],
    delta: [f32; 2],
    angle_deg: f32,
    /// A size handle is being dragged.
    size: Option<SizeDrag>,
    /// Smart guides found for this drag: lines (world XY, from → to) and gaps (from, to, mm).
    guides: Vec<([f32; 2], [f32; 2])>,
    gaps: Vec<([f32; 2], [f32; 2], f32)>,
    /// A value typed while dragging (move: Δ along `typed_axis`; size: the new size).
    typed: String,
    typed_axis: usize,
    /// ⌥ held: letting go leaves the originals and drops copies here.
    copy: bool,
}

/// A flat face under the pointer (world mm), for Push/Pull.
#[derive(Clone)]
struct PpFace {
    id: String,
    /// The evaluated mesh it was found on (a new evaluation means a new face).
    mesh: usize,
    tris: Vec<usize>,
    normal: Vec3,
    /// Where the pointer meets it.
    hit: Vec3,
    fill: Vec<[Vec3; 3]>,
    edges: Vec<[Vec3; 2]>,
}

/// Dragging a face out or in.
struct PpDrag {
    face: PpFace,
    press: Pos2,
    /// Screen points per mm along the face normal.
    px_per_mm: egui::Vec2,
    dist: f32,
    /// A distance typed while dragging.
    typed: String,
}

/// What ⌘D repeats: the last copies made and the step from their sources, like PowerPoint's
/// and Figma's smart duplicate (⌥-drag a copy, or ⌘D then move the copy; ⌘D again steps on).
#[derive(Clone, Debug, Default)]
pub struct RepeatCopy {
    /// Each source's centre when the copies were asked for, in order.
    from: Vec<[f32; 2]>,
    /// The copies, in the same order (filled in when the call returns).
    ids: Vec<String>,
    /// The step to repeat, once known.
    offset: Option<[f32; 2]>,
}

/// Dragging one of the box handles: a side (axis 0/1, sign ±1) or the top (axis 2).
#[derive(Clone, Debug, Default)]
struct SizeDrag {
    id: String,
    axis: usize,
    sign: f32,
    /// World bounds before the drag: min x, y, z, max x, y, z.
    orig: [f32; 6],
    /// Screen pixels per mm along +Z at the top handle (for the height handle).
    px_per_mm: [f32; 2],
    press: Pos2,
    /// mm added to the size along `axis`.
    amount: f32,
}

struct Shot {
    path: PathBuf,
    /// Dev knobs for performance checks: ONLY3D_GUI_SHOT_FRAMES, ONLY3D_GUI_SPIN.
    min_frames: u32,
    min_time: Duration,
    started: Instant,
    spin: bool,
    frames: u32,
    requested: bool,
    image: Arc<Mutex<Option<Arc<egui::ColorImage>>>>,
}

pub struct App {
    pub studio: Arc<Mutex<Studio>>,
    pub worker: Worker,
    pub snap: Snapshot,
    pub title: Option<String>,
    pub cam: OrbitCamera,
    pub cam_ready: bool,
    pub mode: Mode,
    // toolpaths
    pub toolpaths: Option<(usize, Arc<ToolpathData>)>,
    tp_rx: Option<Receiver<(usize, Arc<ToolpathData>)>>,
    pub tp_building: Option<usize>,
    pub layer_lo: usize,
    pub layer_hi: usize,
    pub one_layer: bool,
    pub feature_mask: u32,
    /// Preview colours: by filament (Some(true)), by feature (Some(false)); None = by filament
    /// when the plate uses several.
    pub color_by_filament: Option<bool>,
    plate: Arc<PlateData>,
    plate_for: Option<(u32, [u32; 2], usize, &'static str, u64)>,
    gamma_out: bool,
    last_title: String,
    pub vp_aspect: f32,
    pub vp_rect: Rect,
    // ui state
    pub toasts: Vec<Toast>,
    pub shape_ed: ShapeEditor,
    pub name_edit: (String, String),
    pub params_local: BTreeMap<String, f64>,
    /// OpenSCAD variables being edited, by (object, variable).
    pub scad_local: HashMap<(String, String), Value>,
    pub new_param: (String, f64),
    pub xf_local: Option<(String, XformLocal)>,
    pub uniform_scale: bool,
    pub proc_local: ProcLocal,
    pub throttle: Throttle,
    pub active_keys: HashSet<String>,
    pub active_now: HashSet<String>,
    pub effective: Option<Value>,
    pub effective_source: String,
    /// Filament inventory, re-read from disk at most every 2 s.
    pub spool_cache: Option<(Instant, Vec<only3d_studio::spools::Spool>)>,
    /// Plates hidden in the all-plates view (like a layer's eye).
    pub plate_hidden: std::collections::BTreeSet<u32>,
    /// The plate the camera was last placed for, and a pan in progress (from, to, start).
    view_plate: u32,
    pan_to: Option<(Vec3, Vec3, Instant)>,
    effective_rev: u64,
    pub presets: HashMap<String, Vec<PresetInfo>>,
    pub printers: Vec<String>,
    pub default_printer: Option<String>,
    pub printer_status: Option<Result<String, String>>,
    pub printer_data: Option<Value>,
    /// Agent requests waiting for the user's approval.
    pub approvals: crate::approvals::ApprovalQueue,
    /// Live chamber camera for the Printer panel.
    pub printer_cam: crate::printer_live::CameraFeed,
    /// Frame number the Printer panel was last drawn in (it polls only while on screen).
    pub printer_panel_frame: u64,
    printer_polled: Option<Instant>,
    /// UI scale being dragged in Preferences; applied on release so the slider doesn't rescale
    /// under the pointer.
    pub(crate) ui_scale_pending: Option<f32>,
    /// Objects to highlight because an activity row is hovered, with the pass it was set in.
    pub(crate) hover_objects: (Vec<String>, u64),
    /// The activity row opened for details (its time and tool).
    pub(crate) activity_open: Option<(u128, String)>,
    /// Scroll the just-opened activity row into view on the next frame.
    pub(crate) activity_reveal: bool,
    /// Where the primary action (Slice → Print… → printing) sat last frame, so the layer slider
    /// and toasts keep clear of it.
    primary_rect: Option<Rect>,
    /// A plate tab being renamed: (plate, text).
    pub(crate) plate_rename: Option<(u32, String)>,
    pub(crate) ws: WorkspaceUi,
    /// The object whose place, turn and scale "paste transform" applies.
    pub(crate) transform_source: Option<String>,
    pub(crate) check_form: crate::design_ui::CheckForm,
    pub(crate) fit_form: crate::design_ui::FitForm,
    /// The learned time correction for the default printer, re-read now and then.
    pub(crate) time_correction: Option<(Instant, Option<only3d_studio::timing::Correction>)>,
    /// Last studio event looked at for notifications.
    pub(crate) event_seq: u64,
    /// Notifications found while the studio was locked, sent right after.
    pub(crate) notes_pending: Vec<(String, String, bool)>,
    /// What a right-click in the viewport was on.
    pub(crate) ctx_target: Option<crate::context_menu::CtxTarget>,
    pub print_modal: Option<PrintStage>,
    pub slice_error: Option<String>,
    pub live: LiveState,
    pub agent_flash: Option<(Instant, String)>,
    pub color_edit: HashMap<u32, Color32>,
    // chrome
    pub cfg: UiConfig,
    cfg_saved: UiConfig,
    cfg_changed: Option<Instant>,
    persist: bool,
    pub keymap: Keymap,
    pub pal: Palette,
    pub pending: Vec<Action>,
    paste_text: Option<String>,
    /// The native menu bar (macOS), when its state last reached it, and whether text editing
    /// had its chords back then.
    pub(crate) menubar: crate::menubar::MenuBar,
    pub(crate) menu_synced: Option<Instant>,
    pub(crate) menu_bypass: bool,
    /// Key presses a menu pick hands to the focused text field (Undo, Select All while typing).
    pub(crate) menu_keys: Vec<egui::Event>,
    /// The main area's tabs, the files open in them, and where they sit this frame.
    pub(crate) editor: crate::editor::Editor,
    pub(crate) files_open: HashMap<u64, crate::preview::FilePreview>,
    pub(crate) regions: crate::editor::Regions,
    pub(crate) egui: egui::Context,
    /// Text to put on the system clipboard this frame (copied objects as JSON).
    copy_out: Option<String>,
    pub gizmo: Gizmo,
    pub overhangs: bool,
    pub clipboard: Vec<Object>,
    drag: Option<ObjDrag>,
    held: Option<(u64, ObjDrag)>,
    /// The last copies, for ⌘D to repeat their step.
    repeat: Option<RepeatCopy>,
    /// Push/Pull: the face under the pointer, and the face being dragged.
    pp_hover: Option<PpFace>,
    pp: Option<PpDrag>,
    pub centers: HashMap<String, [f32; 2]>,
    pub tools: Vec<ToolInfo>,
    pub palette: PaletteUi,
    pub dialog: Option<Dialog>,
    pub show_prefs: bool,
    /// First launch (no ui.json yet): the "which app do you know best?" picker.
    pub(crate) show_welcome: bool,
    /// The two first-launch steps (interface, printer).
    pub(crate) onboard: crate::welcome::Onboarding,
    pub show_shortcuts: bool,
    pub shortcut_query: String,
    pub catalog: Option<Catalog>,
    pub catalog_rev: u64,
    pub catalog_error: Option<String>,
    pub settings_query: String,
    pub settings_object: bool,
    pub settings_local: HashMap<String, Value>,
    pub layout_gen: u32,
    pub drop_targets: Vec<crate::chrome::DropZone>,
    pub spool_form: SpoolForm,
    pub printer_list_text: Option<String>,
    /// Dev knobs ONLY3D_GUI_KEYS / ONLY3D_GUI_DRAG: synthetic input, one batch of events
    /// per frame once the session is loaded.
    inject: std::collections::VecDeque<Vec<egui::Event>>,
    /// ONLY3D_GUI_DRAG_MODS=alt,shift,cmd: modifiers held through the scripted input.
    inject_mods: Option<egui::Modifiers>,
    shot: Option<Shot>,
    scroll_anchor: Option<(Vec3, Instant, Pos2)>,
    pub(crate) frames: u64,
    frame_times: std::collections::VecDeque<f32>,
    last_frame: Option<Instant>,
    show_stats: bool,
    started_ms: u128,
}

pub const PRIMITIVES: [(&str, &str); 12] = [
    ("Box", r#"{"type":"box","size":[30,30,20]}"#),
    ("Rounded box", r#"{"type":"box","size":[40,30,16],"radius":3}"#),
    ("Cylinder", r#"{"type":"cylinder","d":24,"h":30}"#),
    ("Sphere", r#"{"type":"sphere","d":30}"#),
    ("Torus", r#"{"type":"torus","major":18,"minor":6}"#),
    ("Slot", r#"{"type":"slot","length":40,"width":12,"h":6}"#),
    ("Tube", r#"{"type":"tube","d":24,"wall":2,"h":30}"#),
    ("Hex prism", r#"{"type":"polygon","sides":6,"flats":20,"h":10}"#),
    ("Wedge", r#"{"type":"wedge","size":[30,30,15]}"#),
    ("Tray", r#"{"type":"shell","child":{"type":"box","size":[80,50,20],"corner_radius":6},"wall":2}"#),
    ("Plate with M3 holes", r#"{"type":"difference","children":[{"type":"box","size":[60,30,5],"corner_radius":4},{"type":"hole","screw":"M3","kind":"countersink","depth":6,"at":[-22,0,5]},{"type":"hole","screw":"M3","kind":"countersink","depth":6,"at":[22,0,5]}]}"#),
    ("Text", r#"{"type":"text","text":"only3d","size":8,"h":2}"#),
];

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, studio: Arc<Mutex<Studio>>, opts: GuiOptions) -> anyhow::Result<Self> {
        theme::install_fonts(&cc.egui_ctx);
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        // screenshots use the defaults (plus env knobs), never the user's saved layout
        let persist = opts.screenshot.is_none();
        let (mut cfg, cfg_err) = if persist { UiConfig::load() } else { (UiConfig::default(), None) };
        if let Some(t) = env("ONLY3D_GUI_THEME").and_then(|t| ThemeChoice::parse(&t)) {
            cfg.theme = t;
        }
        if let Some(k) = env("ONLY3D_GUI_KEYMAP").and_then(|k| crate::keymap::Preset::parse(&k)) {
            cfg.keymap = k;
        }
        let mut layout_err = None;
        if let Some(script) = env("ONLY3D_GUI_LAYOUT") {
            if let Err(e) = cfg.layout.apply_script(&script) {
                layout_err = Some(format!("ONLY3D_GUI_LAYOUT: {e}"));
            }
        }
        let os_dark = cc.egui_ctx.system_theme().is_none_or(|t| t == egui::Theme::Dark);
        let p = cfg.theme.palette(os_dark);
        theme::apply(&cc.egui_ctx, p);
        cc.egui_ctx.set_zoom_factor(cfg.ui_scale);

        let rs = cc.wgpu_render_state.as_ref().ok_or_else(|| anyhow::anyhow!("only3d needs the wgpu renderer"))?;
        let gamma_out = !rs.target_format.is_srgb();
        let scene_gpu = gpu::SceneGpu::new(&rs.device, rs.target_format, 1);
        rs.renderer.write().callback_resources.insert(scene_gpu);

        // agents' risky printer requests wait for the user's answer in this window
        let approvals = crate::approvals::ApprovalQueue::default();
        let live = if opts.start_live_server {
            let path = only3d_studio::live::socket_path();
            match only3d_studio::live::serve_with(studio.clone(), &path, Some(approvals.approver(cc.egui_ctx.clone()))) {
                Ok(()) => LiveState::Running(path),
                Err(e) => LiveState::Failed(e.to_string()),
            }
        } else {
            LiveState::Off
        };

        let tools: Vec<ToolInfo> = {
            let reg = studio.lock().map(|s| s.registry()).ok();
            reg.map(|r| {
                r.tools
                    .iter()
                    .map(|t| ToolInfo { name: t.name.to_string(), title: t.title.to_string(), description: t.description.to_string(), schema: t.schema.clone(), mutates: t.mutates, side_effects: t.side_effects })
                    .collect()
            })
            .unwrap_or_default()
        };

        let book = only3d_studio::printer::load_book();
        let worker = Worker::new(studio.clone(), cc.egui_ctx.clone());
        let mode = if env("ONLY3D_GUI_VIEW").is_some_and(|v| v.eq_ignore_ascii_case("toolpaths") || v.eq_ignore_ascii_case("preview")) { Mode::Toolpaths } else { Mode::Model };
        let keymap = Keymap::load(cfg.keymap);
        let mut app = App {
            studio,
            worker,
            snap: Snapshot::empty(),
            title: opts.project_title,
            cam: OrbitCamera::default(),
            cam_ready: false,
            mode,
            toolpaths: None,
            tp_rx: None,
            tp_building: None,
            layer_lo: 0,
            layer_hi: usize::MAX,
            one_layer: false,
            feature_mask: u32::MAX,
            color_by_filament: None,
            plate: Arc::new(scene::build_plate(&Bed::h2d(), 2.0, &p)),
            plate_for: None,
            gamma_out,
            last_title: String::new(),
            vp_aspect: 1.6,
            vp_rect: Rect::NOTHING,
            toasts: Vec::new(),
            shape_ed: ShapeEditor::default(),
            name_edit: (String::new(), String::new()),
            params_local: BTreeMap::new(),
            scad_local: HashMap::new(),
            new_param: (String::new(), 2.0),
            xf_local: None,
            uniform_scale: true,
            proc_local: ProcLocal::default(),
            throttle: Throttle::default(),
            active_keys: HashSet::new(),
            active_now: HashSet::new(),
            effective: None,
            effective_source: String::new(),
            spool_cache: None,
            plate_hidden: Default::default(),
            view_plate: 1,
            pan_to: None,
            effective_rev: u64::MAX,
            presets: HashMap::new(),
            printers: book.printers.iter().map(|p| p.name.clone()).collect(),
            default_printer: book.default.clone(),
            printer_status: None,
            printer_data: None,
            approvals,
            printer_cam: Default::default(),
            printer_panel_frame: 0,
            printer_polled: None,
            ui_scale_pending: None,
            hover_objects: (Vec::new(), 0),
            activity_open: None,
            activity_reveal: false,
            primary_rect: None,
            plate_rename: None,
            ws: WorkspaceUi::default(),
            transform_source: None,
            check_form: Default::default(),
            fit_form: Default::default(),
            time_correction: None,
            event_seq: u64::MAX,
            notes_pending: Vec::new(),
            ctx_target: None,
            print_modal: None,
            slice_error: None,
            live,
            agent_flash: None,
            color_edit: HashMap::new(),
            cfg_saved: cfg.clone(),
            cfg,
            cfg_changed: None,
            persist,
            keymap,
            pal: p,
            pending: Vec::new(),
            paste_text: None,
            menubar: crate::menubar::MenuBar::install(&cc.egui_ctx),
            menu_synced: None,
            menu_bypass: false,
            menu_keys: Vec::new(),
            editor: crate::editor::Editor::default(),
            files_open: HashMap::new(),
            regions: crate::editor::Regions::default(),
            egui: cc.egui_ctx.clone(),
            copy_out: None,
            gizmo: Gizmo::Select,
            overhangs: false,
            clipboard: Vec::new(),
            drag: None,
            held: None,
            repeat: None,
            pp_hover: None,
            pp: None,
            centers: HashMap::new(),
            tools,
            palette: PaletteUi::default(),
            dialog: None,
            show_prefs: false,
            show_welcome: opts.screenshot.is_none() && !crate::config::path().exists(),
            onboard: crate::welcome::Onboarding::default(),
            show_shortcuts: false,
            shortcut_query: String::new(),
            catalog: None,
            catalog_rev: u64::MAX,
            catalog_error: None,
            settings_query: String::new(),
            settings_object: false,
            settings_local: HashMap::new(),
            layout_gen: 0,
            drop_targets: Vec::new(),
            spool_form: SpoolForm { material: "PLA".into(), color: Color32::from_rgb(0xD9, 0xCB, 0xB5), grams: 1000.0, ..Default::default() },
            printer_list_text: None,
            inject: synthetic_input(),
            inject_mods: env("ONLY3D_GUI_DRAG_MODS").map(|v| {
                let mut m = egui::Modifiers::NONE;
                for k in v.split(',') {
                    match k.trim() {
                        "alt" => m.alt = true,
                        "shift" => m.shift = true,
                        "cmd" => {
                            m.mac_cmd = true;
                            m.command = true;
                        }
                        _ => {}
                    }
                }
                m
            }),
            shot: opts.screenshot.map(|path| Shot {
                path,
                min_frames: env("ONLY3D_GUI_SHOT_FRAMES").and_then(|v| v.parse().ok()).unwrap_or(12),
                min_time: Duration::from_millis(env("ONLY3D_GUI_SHOT_DELAY_MS").and_then(|v| v.parse().ok()).unwrap_or(500)),
                started: Instant::now(),
                spin: env("ONLY3D_GUI_SPIN").is_some_and(|v| v != "0"),
                frames: 0,
                requested: false,
                image: Arc::new(Mutex::new(None)),
            }),
            scroll_anchor: None,
            frames: 0,
            frame_times: std::collections::VecDeque::new(),
            last_frame: None,
            show_stats: env("ONLY3D_GUI_STATS").is_some_and(|v| v != "0"),
            started_ms: only3d_studio::now_ms(),
        };
        // dev knob: files open in tabs (screenshot runs), comma-separated
        if let Some(list) = env("ONLY3D_GUI_PREVIEW") {
            for p in list.split(',').filter(|p| !p.is_empty()) {
                let kind = only3d_studio::workspace::kind_of(p);
                app.open_file_tab(&cc.egui_ctx, &PathBuf::from(p), kind, true, None);
            }
        }
        for e in app.keymap.errors.clone() {
            app.toast(e, true);
        }
        for e in [cfg_err, layout_err].into_iter().flatten() {
            app.toast(e, true);
        }
        for kind in ["machine", "process", "filament"] {
            app.refresh_presets(kind);
        }
        // dev knob: a camera position to show (screenshot runs don't read ui.json)
        if let Some(c) = env("ONLY3D_GUI_CAMERA") {
            let v: Vec<f32> = c.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            if v.len() == 2 {
                app.cfg.camera_xy = Some([v[0], v[1]]);
                app.cfg.show_camera = true;
            }
        }
        // dev knobs for screenshots: open an overlay, pick a tool
        if let Some(open) = env("ONLY3D_GUI_OPEN") {
            for what in open.split(',').map(str::trim) {
                match what {
                    // a sample agent request, to check the approval dialog in screenshots
                    "approval" => {
                        let images = env("ONLY3D_GUI_APPROVAL_IMAGE")
                            .and_then(|p| std::fs::read(p).ok())
                            .map(|b| vec![only3d_studio::tools::ImageOut { png: b, caption: "plate".into(), mime: "image/jpeg".into() }])
                            .unwrap_or_default();
                        // same description and model picture a real request gets, from the open project
                        let st = app.studio.clone();
                        let mut call = |t: &str, a: Value| st.lock().map_err(|_| only3d_studio::ToolError::new("poisoned", "", "")).and_then(|mut s| s.call(t, a, only3d_studio::Actor::User));
                        let (what, name) = only3d_studio::approval::describe_job(&mut call);
                        let mut all = call("render", json!({"view": "iso", "width": 720, "height": 420})).map(|o| o.images).unwrap_or_default();
                        all.extend(images);
                        app.approvals.demo(only3d_studio::approval::ApprovalRequest {
                            requester: "claude-code".into(),
                            tool: "print".into(),
                            title: name.map(|n| format!("Print '{n}' on h2d")).unwrap_or_else(|| "Start a print".into()),
                            details: format!("{what}Ready to print on 'h2d' (15 layers, ~6 min, 3.6 g):\n[x] G-code uses Bambu's own H2D start/end sequence\n[x] G-code check: 8651 extrusions inside the nozzle's reach\n[x] printability checks pass\n[x] printer state IDLE\n[x] nozzle: prints with the right nozzle, which tray 2 feeds\n[x] spool petg-7f7e83-1: 375 g left, job needs 3.6 g\n[x] tray 2: PETG 7F7E83FF (project expects PETG)"),
                            images: all,
                        });
                    }
                    "palette" => {
                        app.palette.open = true;
                        app.palette.focus = true;
                        app.palette.query = env("ONLY3D_GUI_PALETTE_QUERY").unwrap_or_default();
                    }
                    // a sample unregistered spool, to check the Spools panel in screenshots
                    "spool_notice" => {
                        if let Ok(mut st) = app.studio.lock() {
                            st.spool_notices.push(only3d_studio::spools::SpoolNotice {
                                location: "ext:left".into(),
                                material: "PETG".into(),
                                color: "#FFFFFF".into(),
                                spool: None,
                                message: "PETG #FFFFFF in the left external holder has no RFID and isn't registered".into(),
                            });
                        }
                        app.cfg.layout.show(PanelId::Spools);
                    }
                    // a print-finished event a moment after start, to check notifications
                    "print_finished" => {
                        let ev = app.studio.lock().map(|st| st.events.clone());
                        if let Ok(ev) = ev {
                            std::thread::spawn(move || {
                                std::thread::sleep(Duration::from_millis(600));
                                ev.push("print_finished", None, "h2d: 'keychains' finished", json!({"job": "keychains"}));
                            });
                        }
                    }
                    "welcome" => app.show_welcome = true,
                    "welcome_printer" => {
                        app.show_welcome = true;
                        app.onboard.step = 2;
                        app.onboard.setup = true;
                    }
                    "welcome_printer_ask" => {
                        app.show_welcome = true;
                        app.onboard.step = 2;
                    }
                    "welcome_agent" => {
                        app.show_welcome = true;
                        app.onboard.step = 3;
                    }
                    "shortcuts" => app.show_shortcuts = true,
                    "agent" => app.dialog = Some(Dialog::ConnectAgent(Default::default())),
                    "preferences" | "prefs" => app.show_prefs = true,
                    "rotate" => app.gizmo = Gizmo::Rotate,
                    "move" => app.gizmo = Gizmo::Move,
                    "overhangs" => app.overhangs = true,
                    p => {
                        if let Some(panel) = PanelId::parse(p) {
                            app.cfg.layout.show(panel);
                        }
                    }
                }
            }
        }
        if let Some(q) = env("ONLY3D_GUI_SETTINGS_QUERY") {
            app.settings_query = q;
        }
        Ok(app)
    }

    pub fn refresh_presets(&mut self, kind: &str) {
        self.read_tool("preset_list", json!({ "kind": kind }), Kind::Presets(kind.to_string()));
    }

    /// Run a read-only tool for the UI's own bookkeeping (preset lists, the settings catalog)
    /// straight through the registry: same handler, but not journaled as a user action.
    pub fn read_tool(&mut self, tool: &str, args: Value, kind: Kind) {
        let name = tool.to_string();
        self.worker.read(
            tool,
            kind,
            Box::new(move |s: &mut Studio| {
                let reg = s.registry();
                match reg.get(&name) {
                    Some(t) => (t.handler)(s, args),
                    None => Err(only3d_studio::ToolError::new("unknown_tool", format!("no tool named '{name}'"), "")),
                }
            }),
        );
    }

    // ------------------------------------------------------------------------------------------
    // session sync

    /// Copy what changed from the studio. Never blocks: if the session is busy (a slice, an
    /// agent call) we keep showing the previous state.
    fn refresh(&mut self) {
        let Ok(mut s) = self.studio.try_lock() else { return };
        if s.revision != self.snap.revision || s.plate != self.snap.plate {
            self.snap.plate = s.plate;
            self.snap.revision = s.revision;
            self.snap.project = s.project.clone();
            self.snap.project_path = s.project_path.clone();
            self.snap.bed = s.bed.clone();
            self.snap.can_undo = s.can_undo();
            match s.evaluated() {
                Ok(objs) => {
                    let _ = &objs;
                    self.snap.findings = s.findings().unwrap_or_default();
                    self.centers = objs.iter().map(|o| (o.id.clone(), [(o.report.bounds.min.x + o.report.bounds.max.x) * 0.5, (o.report.bounds.min.y + o.report.bounds.max.y) * 0.5])).collect();
                    self.snap.objects = objs;
                    self.snap.eval_error = None;
                    let plate = s.plate;
                    self.snap.others = if s.project.plate_count() > 1 { s.evaluated_all().map(|all| Arc::new(all.iter().filter(|o| o.plate != plate).cloned().collect())).unwrap_or_default() } else { Arc::new(Vec::new()) };
                }
                Err(e) => {
                    self.snap.eval_error = Some(format!("{}: {}", e.code, e.message));
                }
            }
            // .scad objects: their variables (read once per file version, then cached)
            let plate = s.plate;
            let found: Vec<(String, String, String, BTreeMap<String, Value>)> = s
                .project
                .objects
                .iter()
                .filter(|o| o.plate == plate)
                .filter_map(|o| {
                    let file = only3d_studio::tools::scad_files(o).into_iter().next()?;
                    let mut set = BTreeMap::new();
                    o.shape.visit(&mut |n| {
                        if let only3d_model::Shape::Import { path, vars, .. } = &n.shape {
                            if *path == file {
                                set = vars.clone();
                            }
                        }
                    });
                    Some((o.id.clone(), if o.name.is_empty() { o.id.clone() } else { o.name.clone() }, file, set))
                })
                .collect();
            self.snap.scad = found
                .into_iter()
                .map(|(id, name, file, set)| {
                    let (vars, error) = match s.evaluator.scad_vars(&file) {
                        Ok(v) => (v, None),
                        Err(e) => (Arc::new(Vec::new()), Some(e.to_string())),
                    };
                    let file = std::path::Path::new(&file).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or(file);
                    ScadObject { id, name, file, vars, set, error }
                })
                .collect();
            // a committed drag stays drawn at its new place until the edit lands
            if self.held.as_ref().is_some_and(|(rev, _)| *rev != s.revision) {
                self.held = None;
            }
        }
        if s.selection != self.snap.selection && !self.select_inflight() {
            self.snap.selection = s.selection.clone();
        }
        // prints that finished or failed: tell the user even when the window is in the back
        let last = s.events.last_seq();
        if self.event_seq == u64::MAX {
            self.event_seq = last;
        } else if last > self.event_seq {
            let kinds = ["print_finished".to_string(), "print_failed".to_string()];
            let next = s.project.queue.first().map(|k| s.project.plate_name(*k));
            for e in s.events.since(self.event_seq, &kinds) {
                let failed = e.kind == "print_failed";
                let mut body = e.summary.clone();
                if let (false, Some(n)) = (failed, &next) {
                    body.push_str(&format!(". Next in the queue: {n} (clear the plate first)"));
                }
                self.notes_pending.push((if failed { "Print stopped" } else { "Print finished" }.to_string(), body, failed));
            }
            self.event_seq = last;
        }
        if s.spool_notices != self.snap.spool_notices {
            self.snap.spool_notices = s.spool_notices.clone();
        }
        let last = s.journal.last().map(|e| e.at_ms).unwrap_or(0);
        if s.journal.len() != self.snap.journal_len || last != self.snap.journal_last {
            let start = s.journal.len().saturating_sub(400);
            let fresh: Vec<JournalEntry> = s.journal[start..].iter().filter(|e| e.at_ms > self.snap.journal_last).cloned().collect();
            if let Some(e) = fresh.iter().rev().find(|e| e.actor != only3d_studio::Actor::User && e.at_ms >= self.started_ms) {
                let who = match e.actor {
                    only3d_studio::Actor::Agent => "Agent",
                    _ => "Script",
                };
                self.agent_flash = Some((Instant::now(), format!("{who} · {} — {}", e.tool, e.summary)));
            }
            self.snap.journal = s.journal[start..].to_vec();
            self.snap.journal_len = s.journal.len();
            self.snap.journal_last = last;
        }
        let same = match (&s.last_slice, &self.snap.last_slice) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.snap.last_slice = s.last_slice.clone();
        }
        self.snap.slice_current = s.slice_is_current();
    }

    fn select_inflight(&self) -> bool {
        self.worker.inflight.iter().any(|(t, _, _)| t == "select")
    }

    fn handle_done(&mut self) {
        for d in self.worker.poll() {
            self.on_done(d);
        }
    }

    fn on_done(&mut self, d: Done) {
        match (&d.kind, d.result) {
            (Kind::Presets(k), Ok(o)) => {
                let list: Vec<PresetInfo> = match &o.data {
                    Value::Array(a) => a
                        .iter()
                        .filter_map(|v| match v {
                            Value::String(s) => Some(PresetInfo { name: s.clone(), source: "system".into() }),
                            Value::Object(m) => Some(PresetInfo { name: m.get("name")?.as_str()?.to_string(), source: m.get("source").and_then(|s| s.as_str()).unwrap_or("system").to_string() }),
                            _ => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                self.presets.insert(k.clone(), list);
            }
            (Kind::Presets(k), Err(e)) if e.code == "unknown_tool" => {
                // older backend: plain list_presets (names only)
                self.read_tool("list_presets", json!({ "kind": k }), Kind::Presets(k.clone()));
            }
            (Kind::PresetChanged(kind), r) => match r {
                Ok(o) => {
                    let k = kind.clone();
                    self.toast(o.text.lines().next().unwrap_or("Done").to_string(), false);
                    if k.is_empty() {
                        for kind in ["process", "filament", "machine"] {
                            self.refresh_presets(kind);
                        }
                    } else {
                        self.refresh_presets(&k);
                    }
                }
                Err(e) => self.toast(tool_error_text(&d.tool, &e), true),
            },
            (Kind::Effective(rev), r) => {
                self.effective_rev = *rev;
                if let Ok(o) = r {
                    self.effective_source = o.text;
                    self.effective = Some(o.data);
                }
            }
            (Kind::Catalog(rev), r) => {
                self.catalog_rev = *rev;
                match r {
                    Ok(o) => {
                        self.catalog = Some(Catalog::from_tool(&o.data));
                        self.catalog_error = None;
                    }
                    Err(e) if e.code == "unknown_tool" => {
                        self.read_tool("settings_schema", json!({}), Kind::Schema(*rev));
                    }
                    Err(e) => self.catalog_error = Some(format!("{}: {}", e.code, e.message)),
                }
            }
            (Kind::Schema(rev), r) => {
                self.catalog_rev = *rev;
                match r {
                    Ok(o) => {
                        let overrides = serde_json::to_value(&self.snap.project.process.overrides).unwrap_or(Value::Null);
                        self.catalog = Some(Catalog::from_schema(&o.data, &overrides));
                    }
                    Err(e) => self.catalog_error = Some(format!("{}: {}", e.code, e.message)),
                }
            }
            (Kind::Copied, Ok(o)) => {
                // one call's {"ids": [..]} or a batch's list of them, in the order asked
                let mut ids: Vec<String> = Vec::new();
                let each: Vec<&Value> = match &o.data {
                    Value::Array(v) => v.iter().collect(),
                    d => vec![d],
                };
                for d in each {
                    ids.extend(d.get("ids").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|v| v.as_str().map(String::from)));
                }
                if let Some(r) = &mut self.repeat {
                    r.ids = ids.clone();
                }
                if !ids.is_empty() {
                    self.select(ids);
                }
            }
            (Kind::AddObject, Ok(o)) => {
                if let Some(id) = o.data.get("id").and_then(|v| v.as_str()) {
                    self.select(vec![id.to_string()]);
                } else if let Some(ids) = o.data.get("ids").and_then(|v| v.as_array()) {
                    self.select(ids.iter().filter_map(|v| v.as_str().map(String::from)).collect());
                }
                for w in o.data.get("warnings").and_then(|v| v.as_array()).into_iter().flatten() {
                    if let Some(w) = w.as_str() {
                        self.warn(w.to_string());
                    }
                }
                // it landed somewhere other than asked (on top of something, or a full plate)
                if let Some(t) = o.data.get("placed").and_then(|v| v.as_str()).filter(|t| !t.is_empty()) {
                    self.toast(t.to_string(), false);
                }
            }
            (Kind::ShapeApply(id), r) => {
                if self.shape_ed.id == *id {
                    match r {
                        Ok(_) => {
                            self.shape_ed.dirty = false;
                            self.shape_ed.error = None;
                        }
                        Err(e) => self.shape_ed.error = Some(format!("{}: {}\n→ {}", e.code, e.message, e.hint)),
                    }
                }
            }
            (Kind::Slice, r) => match r {
                Ok(_) => {
                    self.slice_error = None;
                    self.mode = Mode::Toolpaths;
                    self.gizmo = Gizmo::Select;
                    self.layer_hi = usize::MAX;
                    self.layer_lo = 0;
                }
                Err(e) => {
                    self.toast(format!("Slice failed — {}", e.message), true);
                    self.slice_error = Some(format!("{}\n→ {}", e.message, e.hint));
                }
            },
            (Kind::Commit, Err(e)) => {
                self.held = None;
                self.toast(tool_error_text(&d.tool, &e), true);
            }
            (Kind::PrintPreflight, r) => {
                // the user may have cancelled while the printer was being contacted
                if !matches!(self.print_modal, Some(PrintStage::Checking)) {
                    return;
                }
                self.print_modal = Some(match r {
                    Ok(o) => PrintStage::Ready(o.text),
                    Err(e) => PrintStage::Failed(format!("{}\n\n→ {}", e.message, e.hint)),
                });
            }
            (Kind::PrintConfirm, r) => {
                self.print_modal = Some(match r {
                    Ok(o) => PrintStage::Sent(o.text),
                    Err(e) => PrintStage::SendFailed(format!("{}\n\n→ {}", e.message, e.hint)),
                });
            }
            (Kind::PrinterStatus, r) => {
                if let Ok(o) = &r {
                    self.printer_data = Some(o.data.clone());
                }
                self.printer_status = Some(r.map(|o| o.text).map_err(|e| format!("{}\n→ {}", e.message, e.hint)));
            }
            (Kind::PrinterInfo, r) => {
                self.printer_list_text = Some(match r {
                    Ok(o) => o.text,
                    Err(e) => format!("{}: {}\n→ {}", e.code, e.message, e.hint),
                });
            }
            (Kind::ToolForm, r) => {
                if let Some(Dialog::ToolForm(f)) = &mut self.dialog {
                    f.running = false;
                    f.result = Some(match r {
                        Ok(o) => Ok(if o.data.is_null() { o.text } else { format!("{}\n\n{}", o.text, serde_json::to_string_pretty(&o.data).unwrap_or_default().chars().take(6000).collect::<String>()) }),
                        Err(e) => Err(format!("{}: {}\n→ {}", e.code, e.message, e.hint)),
                    });
                } else if let Err(e) = r {
                    self.toast(tool_error_text(&d.tool, &e), true);
                } else if let Ok(o) = r {
                    self.toast(o.text.lines().next().unwrap_or("Done").to_string(), false);
                }
            }
            (Kind::Report, r) => match r {
                Ok(o) => self.toast(o.text.lines().next().unwrap_or("Done").to_string(), false),
                Err(e) => self.toast(tool_error_text(&d.tool, &e), true),
            },
            (Kind::Spools, r) => {
                self.spool_cache = None;
                match r {
                    Ok(o) => self.toast(o.text.lines().next().unwrap_or("Done").to_string(), false),
                    Err(e) => self.toast(tool_error_text(&d.tool, &e), true),
                }
            }
            (Kind::Export | Kind::Save, r) => match r {
                Ok(o) => self.toast(o.text.lines().next().unwrap_or("Done").to_string(), false),
                Err(e) => self.toast(format!("{}: {}", e.code, e.message), true),
            },
            (Kind::ExportPreview(key), r) => {
                let key = *key;
                let png = r.ok().and_then(|o| o.images.into_iter().next()).map(|i| i.png);
                self.export_preview_done(key, png);
            }
            (Kind::Exported(reveal), r) => match r {
                Ok(o) => {
                    self.toast(o.text.lines().next().unwrap_or("Exported").to_string(), false);
                    let first = o.data.get("path").or_else(|| o.data.get("paths").and_then(|p| p.get(0))).and_then(|p| p.as_str()).map(PathBuf::from);
                    if let (true, Some(p)) = (*reveal, first) {
                        crate::os::reveal(std::path::Path::new(&p));
                    }
                }
                Err(e) => self.toast(tool_error_text(&d.tool, &e), true),
            },
            (Kind::Quiet, _) => {}
            (Kind::PrintDetail(id), r) => match r {
                Ok(o) => self.ws.print_open = Some((id.clone(), o.text)),
                Err(e) => self.toast(tool_error_text(&d.tool, &e), true),
            },
            (Kind::Reprint, r) => match r {
                Ok(o) if o.data.is_object() => {
                    // the version as it was printed, then the usual checklist (undo restores)
                    self.worker.call("set_project", json!({ "project": o.data }), Kind::Plain);
                    self.print_modal = Some(PrintStage::Checking);
                    self.worker.call("print", json!({}), Kind::PrintPreflight);
                }
                Ok(_) => self.toast("That print has no saved project version".into(), true),
                Err(e) => self.toast(tool_error_text(&d.tool, &e), true),
            },
            (Kind::Onboard(what), r) => {
                let what = what.clone();
                self.onboard_done(&what, r);
            }
            (Kind::Versions, r) => match r {
                Ok(o) => self.dialog = Some(Dialog::Versions(o.data.as_array().cloned().unwrap_or_default())),
                Err(e) => self.toast(tool_error_text(&d.tool, &e), true),
            },
            (Kind::Restore(v), r) => match r {
                Ok(o) if o.data.is_object() => {
                    let v = *v;
                    self.worker.call("set_project", json!({ "project": o.data }), Kind::Plain);
                    self.toast(format!("Restored version {v}. Undo brings back what you had."), false);
                }
                Ok(_) => self.toast("That version could not be read".into(), true),
                Err(e) => self.toast(tool_error_text(&d.tool, &e), true),
            },
            (Kind::FitShown, r) => match r {
                Ok(o) => self.fit_form.shown = Some(o.text),
                Err(e) => self.toast(tool_error_text(&d.tool, &e), true),
            },
            (Kind::Workspace, r) => {
                self.ws.listings.clear();
                self.ws.root = None;
                if let Err(e) = r {
                    self.toast(tool_error_text(&d.tool, &e), true);
                }
            }
            (_, Err(e)) => {
                self.toast(tool_error_text(&d.tool, &e), true);
            }
            _ => {}
        }
    }

    pub fn toast(&mut self, text: String, error: bool) {
        self.toasts.push(Toast { text, error, warn: false, at: Instant::now() });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    /// A print finished or failed: a toast, a macOS notification and, if set, a push to the
    /// user's own URL (ntfy-style POST).
    pub fn notify(&mut self, title: &str, body: &str, failed: bool) {
        if failed {
            self.toast(format!("{title}: {body}"), true);
        } else {
            self.warn(format!("{title}: {body}"));
        }
        if self.cfg.notify_desktop && self.shot.is_none() {
            crate::os::notify(title, body);
        }
        let url = self.cfg.notify_url.trim().to_string();
        if !url.is_empty() && url.starts_with("https://") && self.shot.is_none() {
            let msg = format!("{title}: {body}");
            std::thread::spawn(move || {
                let _ = std::process::Command::new("curl").args(["-s", "-m", "10", "-H", "Title: only3d", "--data-binary"]).arg(msg).arg(url).output();
            });
        }
    }

    pub fn warn(&mut self, text: String) {
        self.toast(text, false);
        if let Some(t) = self.toasts.last_mut() {
            t.warn = true;
        }
    }

    /// Dispatch coalesced edit streams whose previous call has finished.
    fn pump_throttle(&mut self) {
        let keys: Vec<String> = self.throttle.pending.keys().cloned().collect();
        for key in keys {
            let k = Kind::Throttled(key.clone());
            if self.worker.busy_with(|x| *x == k) {
                continue;
            }
            if let Some((tool, args)) = self.throttle.pending.remove(&key) {
                self.worker.call(&tool, Value::Object(args), k);
            }
        }
    }

    pub fn key_busy(&self, key: &str) -> bool {
        self.throttle.pending.contains_key(key) || self.worker.busy_with(|k| matches!(k, Kind::Throttled(x) if x == key)) || self.active_keys.contains(key)
    }

    /// Keep the printer status fresh: every 3 s while the Printer panel is on screen, every 15 s
    /// while a job runs elsewhere (the status-bar chip shows its progress), and every 30 s after
    /// an error, so an offline printer never ties up the worker.
    fn poll_printer(&mut self) {
        if self.printers.is_empty() || self.worker.busy_with(|k| *k == Kind::PrinterStatus) {
            return;
        }
        let shown = self.frames.saturating_sub(self.printer_panel_frame) <= 2;
        let busy = self.printer_data.as_ref().and_then(|d| d.get("gcode_state")).and_then(Value::as_str).is_some_and(|s| matches!(s, "RUNNING" | "PREPARE" | "PAUSE" | "SLICING"));
        let failing = matches!(self.printer_status, Some(Err(_)));
        let every = match (shown, busy, failing) {
            (_, _, true) if shown || busy => Duration::from_secs(30),
            (true, _, _) => Duration::from_secs(3),
            (false, true, _) => Duration::from_secs(15),
            _ => return,
        };
        if self.printer_polled.is_none_or(|t| t.elapsed() >= every) {
            self.printer_polled = Some(Instant::now());
            // a quiet read: polling must not flood the activity feed
            self.worker.read("printer_status", Kind::PrinterStatus, Box::new(|s| only3d_studio::printer::status(s, None)));
        }
    }

    fn maybe_refresh_effective(&mut self) {
        if self.effective_rev == self.snap.revision || self.worker.busy_with(|k| matches!(k, Kind::Effective(_))) || self.snap.revision == u64::MAX {
            return;
        }
        let rev = self.snap.revision;
        self.worker.read(
            "effective settings",
            Kind::Effective(rev),
            Box::new(|s: &mut Studio| {
                let objs = s.evaluated()?;
                let vis: Vec<EvaluatedObject> = objs.iter().filter(|o| !o.hidden).cloned().collect();
                let setup = s.machine.setup(&s.project, &vis)?;
                let mut data = serde_json::to_value(&setup.settings).unwrap_or(Value::Null);
                // the preset's own values (without the project's overrides), for the sliders' ticks
                if !s.project.process.overrides.is_empty() {
                    let mut bare = s.project.clone();
                    bare.process.overrides.clear();
                    if let Ok(ps) = s.machine.setup(&bare, &vis) {
                        if let Some(o) = data.as_object_mut() {
                            o.insert("__preset".into(), serde_json::to_value(&ps.settings).unwrap_or(Value::Null));
                        }
                    }
                } else if let Some(o) = data.as_object_mut() {
                    let copy = Value::Object(o.clone());
                    o.insert("__preset".into(), copy);
                }
                Ok(only3d_studio::ToolOutput::text(setup.source.clone()).with_data(data))
            }),
        );
    }

    /// The settings catalog is re-read when the project changes and the Settings panel is shown.
    fn maybe_refresh_catalog(&mut self) {
        let wanted = self.cfg.layout.is_showing(PanelId::Settings) || self.catalog.is_none();
        if !wanted || self.catalog_rev == self.snap.revision || self.snap.revision == u64::MAX || self.worker.busy_with(|k| matches!(k, Kind::Catalog(_) | Kind::Schema(_))) || self.key_busy("settings") {
            return;
        }
        self.read_tool("settings_catalog", json!({}), Kind::Catalog(self.snap.revision));
    }

    fn poll_toolpaths(&mut self) {
        if let Some(rx) = &self.tp_rx {
            if let Ok((key, data)) = rx.try_recv() {
                let n = data.layers();
                let follow = self.layer_hi == usize::MAX || self.toolpaths.as_ref().is_some_and(|(_, d)| self.layer_hi + 1 >= d.layers());
                if follow || self.layer_hi >= n {
                    self.layer_hi = n.saturating_sub(1);
                }
                // dev knob for screenshots: start at this (1-based) layer
                if let Some(l) = env("ONLY3D_GUI_LAYER").and_then(|v| v.parse::<usize>().ok()).filter(|_| self.toolpaths.is_none()) {
                    self.layer_hi = l.clamp(1, n.max(1)) - 1;
                }
                self.layer_lo = self.layer_lo.min(self.layer_hi);
                self.toolpaths = Some((key, data));
                self.tp_rx = None;
                self.tp_building = None;
            }
        }
        // only this plate's slice is previewed (rebuilt when the colouring changes)
        let by_fil = self.preview_by_filament();
        let want = self.snap.last_slice.as_ref().filter(|s| s.plate == self.snap.plate).map(|s| (Arc::as_ptr(s) as usize) << 1 | usize::from(by_fil));
        let have = self.toolpaths.as_ref().map(|t| t.0);
        if want.is_none() {
            self.toolpaths = None;
            return;
        }
        if want != have && want != self.tp_building {
            let s = self.snap.last_slice.clone().expect("slice");
            let key = want.unwrap_or(0);
            let (tx, rx) = std::sync::mpsc::channel();
            self.tp_rx = Some(rx);
            self.tp_building = Some(key);
            let colors: Vec<[u8; 3]> = self.snap.project.filaments.iter().map(|f| scene::parse_hex(&f.color)).collect();
            std::thread::spawn(move || {
                let of: Vec<usize> = if s.result.job.is_multi() { s.result.job.object_filament.clone() } else { vec![0; s.result.job.object_group.len()] };
                // a single-filament slice prints with its plate's one filament
                let colors = if s.result.job.is_multi() { colors } else { vec![colors.get(s.result.job.filament_sequence.first().copied().unwrap_or(0)).copied().unwrap_or([200, 190, 170])] };
                let data = Arc::new(scene::build_toolpaths(&s.result.layers, by_fil.then_some((of.as_slice(), colors.as_slice()))));
                let _ = tx.send((key, data));
            });
        }
    }

    // ------------------------------------------------------------------------------------------
    // theme / config

    fn sync_theme(&mut self, ctx: &egui::Context) {
        let os_dark = ctx.system_theme().is_none_or(|t| t == egui::Theme::Dark);
        let want = self.cfg.theme.palette(os_dark);
        if want != self.pal {
            self.pal = want;
            theme::apply(ctx, want);
        }
        if (ctx.zoom_factor() - self.cfg.ui_scale).abs() > 1e-3 {
            ctx.set_zoom_factor(self.cfg.ui_scale);
        }
    }

    pub fn set_theme(&mut self, t: ThemeChoice) {
        self.cfg.theme = t;
    }

    /// Write ui.json now (the welcome picker: so it doesn't come back next launch).
    pub(crate) fn save_ui_now(&mut self) {
        if self.persist {
            let _ = self.cfg.save();
            self.cfg_saved = self.cfg.clone();
        }
    }

    pub fn set_keymap(&mut self, p: crate::keymap::Preset) {
        self.cfg.keymap = p;
        self.keymap = Keymap::load(p);
        for e in self.keymap.errors.clone() {
            self.toast(e, true);
        }
    }

    pub fn reset_layout(&mut self) {
        self.cfg.layout = Layout::default();
        self.layout_gen += 1;
    }

    fn save_config(&mut self) {
        if !self.persist {
            return;
        }
        if self.cfg != self.cfg_saved {
            let t = *self.cfg_changed.get_or_insert_with(Instant::now);
            if t.elapsed() > Duration::from_millis(600) {
                if let Err(e) = self.cfg.save() {
                    self.toast(format!("Could not save UI settings: {e}"), true);
                }
                self.cfg_saved = self.cfg.clone();
                self.cfg_changed = None;
            }
        }
    }

    pub fn section_open(&self, id: &str, default: bool) -> bool {
        self.cfg.sections.get(id).copied().unwrap_or(default)
    }

    pub fn set_section(&mut self, id: &str, open: bool) {
        self.cfg.sections.insert(id.to_string(), open);
    }

    // ------------------------------------------------------------------------------------------
    // actions (all edits go through tools)

    pub fn call(&mut self, tool: &str, args: Value) {
        self.worker.call(tool, args, Kind::Plain);
    }

    pub fn select(&mut self, ids: Vec<String>) {
        let ids = self.on_one_plate(ids);
        if ids == self.snap.selection && !self.select_inflight() {
            return;
        }
        self.snap.selection = ids.clone();
        self.worker.call("select", json!({ "ids": ids }), Kind::Quiet);
    }

    /// A selection lives on one plate (the session refuses more, and the window would show a
    /// selection that never happened): keep the ids on the active plate, else on the first
    /// id's plate. Read from the live session, since results arrive before the snapshot.
    fn on_one_plate(&self, ids: Vec<String>) -> Vec<String> {
        let (plates, active): (HashMap<String, u32>, u32) = match self.studio.try_lock() {
            Ok(s) => (ids.iter().filter_map(|i| s.project.object(i).map(|o| (i.clone(), o.plate))).collect(), s.plate),
            Err(_) => (ids.iter().filter_map(|i| self.snap.project.object(i).map(|o| (i.clone(), o.plate))).collect(), self.snap.plate),
        };
        let distinct: std::collections::BTreeSet<u32> = plates.values().copied().collect();
        if distinct.len() < 2 {
            return ids;
        }
        let keep = if distinct.contains(&active) { active } else { ids.iter().find_map(|i| plates.get(i).copied()).unwrap_or(active) };
        ids.into_iter().filter(|i| plates.get(i).is_none_or(|p| *p == keep)).collect()
    }

    pub fn add_primitive(&mut self, name: &str, shape: &str) {
        let shape: Value = serde_json::from_str(shape).unwrap_or(Value::Null);
        let id_base = name.to_ascii_lowercase().replace(' ', "_");
        self.worker.call("add_object", json!({ "shape": shape, "name": id_base }), Kind::AddObject);
    }

    /// Import model files onto `plate` (default: the active one), centred at `at` on it when
    /// given (the import moves them to the nearest free spot if something is there).
    pub fn import_files_onto(&mut self, paths: &[PathBuf], plate: Option<u32>, at: Option<[f32; 2]>) {
        if let Some(k) = plate.filter(|k| *k != self.snap.plate) {
            self.worker.call("plate", json!({"action": "select", "plate": k}), Kind::Plain);
        }
        for p in paths {
            let mut args = json!({ "path": p.display().to_string() });
            if let Some(a) = at {
                args["position"] = json!([(a[0] * 10.0).round() / 10.0, (a[1] * 10.0).round() / 10.0]);
            }
            self.worker.call("import_file", args, Kind::AddObject);
        }
    }

    pub fn import_paths(&mut self, paths: Vec<PathBuf>) {
        for p in paths {
            let s = p.display().to_string();
            if s.to_ascii_lowercase().ends_with(".only3d.json") {
                self.worker.call("open_project", json!({ "path": s }), Kind::Plain);
                self.cam_ready = false;
            } else {
                self.worker.call("import_file", json!({ "path": s }), Kind::AddObject);
            }
        }
    }

    pub fn slice(&mut self) {
        if self.slicing() || self.snap.plate_is_empty() {
            return;
        }
        self.slice_error = None;
        self.worker.call("slice", json!({}), Kind::Slice);
    }

    /// Write the plate as a Bambu Studio project (next to the saved project) and open it there.
    pub fn open_in_bambu(&mut self) {
        if !self.worker.busy_with(|k| *k == Kind::Export) {
            self.worker.call("export_bambu_project", json!({ "open": true }), Kind::Export);
        }
    }

    /// Whether the preview shows filament colours (the default with several filaments).
    pub fn preview_by_filament(&self) -> bool {
        self.color_by_filament.unwrap_or_else(|| self.snap.last_slice.as_ref().is_some_and(|s| s.result.stats.filaments.len() > 1))
    }

    pub fn slicing(&self) -> bool {
        self.worker.busy_with(|k| *k == Kind::Slice)
    }

    pub fn start_print(&mut self) {
        if self.print_modal.is_none() && !self.snap.plate_is_empty() {
            self.print_modal = Some(PrintStage::Checking);
            self.worker.call("print", json!({}), Kind::PrintPreflight);
        }
    }

    pub fn open_dialog(&mut self) {
        if let Some(p) = rfd::FileDialog::new().set_title("Open project").add_filter("only3d project", &["json"]).pick_file() {
            self.worker.call("open_project", json!({ "path": p.display().to_string() }), Kind::Plain);
            self.cam_ready = false;
        }
    }

    pub fn save(&mut self, save_as: bool) {
        if save_as || self.snap.project_path.is_none() {
            self.save_panel(crate::save_panel::SaveWhat::Project);
        } else {
            self.worker.call("save_project", json!({}), Kind::Save);
        }
    }

    pub fn import_dialog(&mut self) {
        if let Some(ps) = rfd::FileDialog::new().set_title("Import model").add_filter("Models", &["stl", "3mf", "obj", "step", "stp", "scad"]).pick_files() {
            self.import_paths(ps);
        }
    }

    pub fn export_dialog(&mut self, ext: &'static str) {
        self.save_panel(crate::save_panel::SaveWhat::Export(ext));
    }

    pub fn frame_selection(&mut self, aspect: f32) {
        let ids = &self.snap.selection;
        let mut bb = only3d_geom::Aabb::EMPTY;
        for o in self.snap.objects.iter().filter(|o| !o.hidden && (ids.is_empty() || ids.contains(&o.id))) {
            bb = bb.union(&o.report.bounds);
        }
        if bb.is_empty() {
            let b = &self.snap.bed;
            bb = only3d_geom::Aabb::new(Vec3::ZERO, Vec3::new(b.size[0], b.size[1], 0.0));
        }
        self.cam.frame(bb.min, bb.max, aspect, 1.15);
    }

    /// The other plates drawn in the all-plates view: (plate, offset from the active plate).
    pub fn shown_plates(&self) -> Vec<(u32, [f32; 2])> {
        let n = self.snap.project.plate_count();
        if !self.cfg.show_all_plates || n < 2 || self.mode != Mode::Model {
            return Vec::new();
        }
        (1..=n).filter(|k| *k != self.snap.plate && !self.plate_hidden.contains(k)).map(|k| (k, scene::plate_offset(k, self.snap.plate, n, &self.snap.bed))).collect()
    }

    /// Frame every plate shown (the active one included).
    pub fn frame_all_plates(&mut self, aspect: f32) {
        let b = self.snap.bed.size;
        let (mut lo, mut hi) = (Vec3::new(0.0, 0.0, 0.0), Vec3::new(b[0], b[1], 40.0));
        for (_, o) in self.shown_plates() {
            lo = lo.min(Vec3::new(o[0], o[1], 0.0));
            hi = hi.max(Vec3::new(o[0] + b[0], o[1] + b[1], 40.0));
        }
        self.cam.frame(lo, hi, aspect, plate_margin(aspect));
        self.cam.target.z = 0.0;
    }

    pub fn frame_plate(&mut self, aspect: f32) {
        let b = &self.snap.bed;
        self.cam.frame(Vec3::new(0.0, 0.0, 0.0), Vec3::new(b.size[0], b.size[1], 40.0), aspect, plate_margin(aspect));
        self.cam.target.z = 0.0;
    }

    pub fn set_view(&mut self, p: ViewPreset) {
        self.cam.set_preset(p);
    }

    /// Plate axes closest to the screen's right / up (arrow keys move objects as they look).
    fn screen_axes(&self) -> ([f32; 2], [f32; 2]) {
        let (right, up, back) = self.cam.basis();
        let snap = |v: Vec3| -> [f32; 2] {
            if v.x.abs() >= v.y.abs() {
                [v.x.signum(), 0.0]
            } else {
                [0.0, v.y.signum()]
            }
        };
        let r = snap(right);
        // "up" on screen: the camera's up, or (looking straight down) its forward direction
        let u_vec = if up.z.abs() > 0.9 { -back } else { up };
        let mut u = snap(Vec3::new(u_vec.x, u_vec.y, 0.0));
        if u == r || u == [-r[0], -r[1]] {
            u = [-r[1], r[0]];
        }
        (r, u)
    }

    fn ctx_plan(&self, action: Action) -> Plan {
        let (r, u) = self.screen_axes();
        let cx = actions::Ctx {
            project: &self.snap.project,
            centers: &self.centers,
            selection: &self.snap.selection,
            plate: self.snap.plate,
            preview: self.mode == Mode::Toolpaths,
            gizmo: self.gizmo,
            screen_right: r,
            screen_up: u,
            can_undo: self.snap.can_undo,
            clipboard: &self.clipboard,
        };
        actions::plan(action, &cx)
    }

    /// Would this action do anything right now? (Keys that don't are left to egui.)
    pub(crate) fn applicable(&self, a: Action) -> bool {
        if a == Action::Paste {
            return true;
        }
        !matches!(self.ctx_plan(a), Plan::NotApplicable)
    }

    pub fn run_calls(&mut self, calls: Vec<(String, Value)>, kind: Kind) {
        match calls.len() {
            0 => {}
            1 => {
                let (t, a) = calls.into_iter().next().expect("one call");
                self.worker.call(&t, a, kind);
            }
            _ => {
                let calls: Vec<Value> = calls.into_iter().map(|(t, a)| json!({ "tool": t, "args": a })).collect();
                self.worker.call("batch", json!({ "calls": calls }), kind);
            }
        }
    }

    /// Run a UI action (from a shortcut, a menu, the palette or a button).
    pub fn run(&mut self, a: Action) {
        let id = a.id();
        self.palette.recent.retain(|r| *r != id);
        self.palette.recent.insert(0, id);
        self.palette.recent.truncate(8);
        if a == Action::Paste {
            if let Some(text) = self.paste_text.take() {
                if let Some(objs) = parse_clipboard(&text) {
                    self.clipboard = objs;
                }
            }
        }
        if a == Action::Cut {
            self.copy_selection();
            if let Plan::Calls(c) = self.ctx_plan(Action::Delete) {
                self.run_calls(c, Kind::Plain);
            }
            return;
        }
        if a == Action::Duplicate {
            // ⌘D repeats the last copy's step (smart duplicate); it needs the GUI's memory
            if let Plan::Calls(_) = self.ctx_plan(a) {
                self.duplicate_selection();
            }
            return;
        }
        match self.ctx_plan(a) {
            Plan::Calls(calls) => {
                let pasted: Vec<String> = if a == Action::Paste { calls.iter().filter(|c| c.0 == "add_object").filter_map(|c| c.1.get("id").and_then(|v| v.as_str()).map(String::from)).collect() } else { Vec::new() };
                self.run_calls(calls, Kind::Plain);
                if !pasted.is_empty() {
                    self.select(pasted);
                }
            }
            Plan::Select(ids) => self.select(ids),
            Plan::NotApplicable => {}
            Plan::Ui => self.ui_action(a),
        }
    }

    fn copy_selection(&mut self) {
        let objs: Vec<Object> = self.snap.project.objects.iter().filter(|o| self.snap.selection.contains(&o.id)).cloned().collect();
        if objs.is_empty() {
            return;
        }
        let n = objs.len();
        // also on the system clipboard, so objects paste into another only3d window
        self.copy_out = serde_json::to_string_pretty(&json!({ "only3d_objects": objs })).ok();
        self.clipboard = objs;
        self.toast(format!("Copied {n} object{}", if n == 1 { "" } else { "s" }), false);
    }

    fn ui_action(&mut self, a: Action) {
        use Action::*;
        let aspect = self.vp_aspect;
        match a {
            Open => self.open_dialog(),
            Save => self.save(false),
            SaveAs => self.save(true),
            Import => self.import_dialog(),
            ExportPrint => self.export_dialog("gcode.3mf"),
            ExportStl => self.export_dialog("stl"),
            Export3mf => self.export_dialog("3mf"),
            ExportScad => self.export_dialog("scad"),
            Copy => self.copy_selection(),
            Rename => {
                if let Some(id) = self.snap.selection.first().cloned() {
                    let text = self.snap.project.object(&id).map(|o| if o.name.is_empty() { o.id.clone() } else { o.name.clone() }).unwrap_or_default();
                    self.dialog = Some(Dialog::Rename { id, text, focus: true });
                }
            }
            Deselect => self.gizmo = Gizmo::Select,
            ToolSelect => self.gizmo = Gizmo::Select,
            ToolMove => self.gizmo = if self.gizmo == Gizmo::Move { Gizmo::Select } else { Gizmo::Move },
            ToolRotate => self.gizmo = if self.gizmo == Gizmo::Rotate { Gizmo::Select } else { Gizmo::Rotate },
            ToolScale => self.gizmo = if self.gizmo == Gizmo::Scale { Gizmo::Select } else { Gizmo::Scale },
            ToolPushPull => {
                self.gizmo = if self.gizmo == Gizmo::PushPull { Gizmo::Select } else { Gizmo::PushPull };
                self.pp = None;
                self.pp_hover = None;
            }
            ViewHome => {
                self.set_view(ViewPreset::Iso);
                if self.shown_plates().is_empty() {
                    self.frame_plate(aspect);
                } else {
                    self.frame_all_plates(aspect);
                }
            }
            Op(o) => self.transform_clip(o),
            ViewIso => self.set_view(ViewPreset::Iso),
            ViewTop => self.set_view(ViewPreset::Top),
            ViewBottom => self.set_view(ViewPreset::Bottom),
            ViewFront => self.set_view(ViewPreset::Front),
            ViewBack => self.set_view(ViewPreset::Back),
            ViewLeft => self.set_view(ViewPreset::Left),
            ViewRight => self.set_view(ViewPreset::Right),
            FrameSelection => self.frame_selection(aspect),
            TogglePreview => self.set_mode(if self.mode == Mode::Model { Mode::Toolpaths } else { Mode::Model }),
            Prepare => self.set_mode(Mode::Model),
            Preview => self.set_mode(Mode::Toolpaths),
            ToggleAllPlates => {
                self.cfg.show_all_plates = !self.cfg.show_all_plates;
                self.set_mode(Mode::Model);
                if self.cfg.show_all_plates {
                    self.frame_all_plates(aspect);
                } else {
                    self.frame_plate(aspect);
                }
                self.save_ui_now();
            }
            ToggleOverhangs => {
                self.overhangs = !self.overhangs;
                if self.overhangs {
                    self.set_mode(Mode::Model);
                }
            }
            LayerUp => self.step_layer(1),
            LayerDown => self.step_layer(-1),
            LayerUpFast => self.step_layer(5),
            LayerDownFast => self.step_layer(-5),
            OneLayer => {
                self.one_layer = !self.one_layer;
                if !self.one_layer {
                    self.layer_lo = 0;
                }
            }
            Slice => self.slice(),
            Print => self.start_print(),
            Palette => {
                self.palette.open = !self.palette.open;
                if self.palette.open {
                    self.palette.query.clear();
                    self.palette.selected = 0;
                    self.palette.focus = true;
                }
            }
            Preferences => self.show_prefs = !self.show_prefs,
            Shortcuts => self.show_shortcuts = !self.show_shortcuts,
            ToggleDock(side) => self.cfg.layout.toggle_dock(side),
            Zen => self.cfg.layout.zen = !self.cfg.layout.zen,
            ResetLayout => {
                self.reset_layout();
                self.toast("Layout reset".into(), false);
            }
            ToggleTheme => {
                let next = if self.pal.dark { ThemeChoice::Light } else { ThemeChoice::Dark };
                self.set_theme(next);
            }
            ZoomIn => self.cfg.ui_scale = ((self.cfg.ui_scale * 10.0).round() / 10.0 + 0.1).min(2.0),
            ZoomOut => self.cfg.ui_scale = ((self.cfg.ui_scale * 10.0).round() / 10.0 - 0.1).max(0.6),
            ZoomReset => self.cfg.ui_scale = 1.0,
            ShowPanel(p) => self.cfg.layout.show(p),
            _ => self.menu_ui_action(a),
        }
    }

    pub fn set_mode(&mut self, m: Mode) {
        if m == Mode::Toolpaths && self.snap.last_slice.is_none() && !self.slicing() {
            self.slice();
        }
        if m == Mode::Toolpaths {
            self.gizmo = Gizmo::Select;
        }
        self.mode = m;
    }

    fn step_layer(&mut self, d: i64) {
        let n = self.toolpaths.as_ref().map(|t| t.1.layers()).unwrap_or(0);
        if n == 0 {
            return;
        }
        let hi = (self.layer_hi.min(n - 1) as i64 + d).clamp(0, n as i64 - 1) as usize;
        self.layer_hi = hi;
        if self.one_layer {
            self.layer_lo = hi;
        } else {
            self.layer_lo = self.layer_lo.min(hi);
        }
    }

    fn drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if !dropped.is_empty() {
            self.drop_files(ctx, dropped);
        }
    }

    /// Any of our overlays that takes the keyboard (the palette, dialogs)?
    pub fn overlay_open(&self) -> bool {
        self.palette.open || self.dialog.is_some() || self.show_prefs || self.print_modal.is_some()
    }

    // ------------------------------------------------------------------------------------------
    // keyboard: chords are resolved before egui sees the events, so focus navigation (Tab,
    // arrows) and widgets never fight the shortcuts, and text fields keep their keys.

    fn intercept_keys(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        // synthetic key presses for the screenshot self-test, through the same path as real ones
        if !self.inject.is_empty() && self.frames > 6 && self.snap.revision != u64::MAX && self.worker.inflight.is_empty() && (self.cam_ready || !self.editor.plate_visible()) {
            if let Some(evs) = self.inject.pop_front() {
                if let Some(m) = self.inject_mods {
                    raw.events.push(egui::Event::ModifiersChanged(m));
                }
                raw.events.extend(evs);
            }
        }
        let overlay = self.overlay_open();
        let typing = ctx.text_edit_focused();
        let popup = egui::Popup::is_any_open(ctx);
        let view = if self.mode == Mode::Toolpaths { When::Preview } else { When::Prepare };
        let mut events = std::mem::take(&mut raw.events);
        // Push/Pull drag: digits . - type the distance, Enter applies, Esc cancels
        if self.pp.is_some() && !overlay && !typing {
            let (mut commit, mut cancel) = (false, false);
            events.retain(|e| match e {
                egui::Event::Text(t) => {
                    if let Some(d) = self.pp.as_mut() {
                        for ch in t.chars() {
                            if ch.is_ascii_digit() || (ch == '.' && !d.typed.contains('.')) || (ch == '-' && d.typed.is_empty()) {
                                d.typed.push(ch);
                            }
                        }
                    }
                    false
                }
                egui::Event::Key { key, pressed: true, .. } => {
                    match key {
                        egui::Key::Backspace => {
                            if let Some(d) = self.pp.as_mut() {
                                d.typed.pop();
                            }
                        }
                        egui::Key::Enter => commit = true,
                        egui::Key::Escape => cancel = true,
                        _ => {}
                    }
                    false
                }
                egui::Event::Key { .. } => false,
                _ => true,
            });
            if cancel {
                self.pp = None;
            } else if commit {
                self.commit_push_pull();
            }
            raw.events = events;
            return;
        }
        // while dragging, the keyboard types an exact value: digits . -, Tab switches X/Y,
        // Enter applies, Esc cancels; shortcuts wait until the drag ends
        if self.drag.is_some() && !overlay && !typing {
            let (mut commit, mut cancel) = (false, false);
            events.retain(|e| match e {
                egui::Event::Text(t) => {
                    if let Some(d) = self.drag.as_mut() {
                        for ch in t.chars() {
                            if ch.is_ascii_digit() || (ch == '.' && !d.typed.contains('.')) || (ch == '-' && d.typed.is_empty()) {
                                d.typed.push(ch);
                            }
                        }
                    }
                    false
                }
                egui::Event::Key { key, pressed: true, .. } => {
                    if let Some(d) = self.drag.as_mut() {
                        match key {
                            egui::Key::Backspace => {
                                d.typed.pop();
                            }
                            egui::Key::Tab if !d.rotate && d.size.is_none() && d.axis.is_none() => {
                                d.typed_axis = 1 - d.typed_axis;
                                d.typed.clear();
                            }
                            egui::Key::Enter => commit = true,
                            egui::Key::Escape => cancel = true,
                            _ => {}
                        }
                    }
                    false
                }
                egui::Event::Key { .. } => false,
                _ => true,
            });
            self.apply_typed();
            if cancel {
                self.drag = None;
            } else if commit {
                self.commit_object_drag();
            }
            raw.events = events;
            return;
        }
        events.retain(|e| {
            let (key, phys, mods, repeat) = match e {
                egui::Event::Key { key, physical_key, pressed: true, modifiers, repeat } => (*key, *physical_key, *modifiers, *repeat),
                egui::Event::Copy => (egui::Key::C, Some(egui::Key::C), egui::Modifiers::COMMAND, false),
                egui::Event::Cut => (egui::Key::X, Some(egui::Key::X), egui::Modifiers::COMMAND, false),
                egui::Event::Paste(_) => (egui::Key::V, Some(egui::Key::V), egui::Modifiers::COMMAND, false),
                _ => return true,
            };
            if popup && matches!(key, egui::Key::Escape | egui::Key::ArrowUp | egui::Key::ArrowDown | egui::Key::Enter | egui::Key::Space) {
                return true;
            }
            let Some(action) = self.keymap.lookup(key, phys, Mods::from_egui(mods), view) else { return true };
            if overlay {
                // only the palette toggle works over overlays (and only over the palette itself)
                if action == Action::Palette && self.palette.open && self.dialog.is_none() {
                    self.pending.push(action);
                    return false;
                }
                return true;
            }
            if typing && !action.while_typing() {
                return true;
            }
            if !self.applicable(action) {
                return true;
            }
            let repeatable = matches!(
                action,
                Action::MoveLeft
                    | Action::MoveRight
                    | Action::MoveUp
                    | Action::MoveDown
                    | Action::MoveLeftBig
                    | Action::MoveRightBig
                    | Action::MoveUpBig
                    | Action::MoveDownBig
                    | Action::LayerUp
                    | Action::LayerDown
                    | Action::LayerUpFast
                    | Action::LayerDownFast
                    | Action::RotateCcw
                    | Action::RotateCw
                    | Action::ZoomIn
                    | Action::ZoomOut
            );
            if repeat && !repeatable {
                return false;
            }
            if let egui::Event::Paste(text) = e {
                self.paste_text = Some(text.clone());
            }
            self.pending.push(action);
            false
        });
        raw.events = events;
    }

    // ------------------------------------------------------------------------------------------
    // viewport

    fn ray_at(&self, p: Pos2, rect: Rect) -> (Vec3, Vec3) {
        self.cam.ray(glam::Vec2::new(p.x - rect.min.x, p.y - rect.min.y), glam::Vec2::new(rect.width(), rect.height()))
    }

    fn project(&self, w: Vec3, rect: Rect) -> Option<Pos2> {
        self.cam.project(w, glam::Vec2::new(rect.width(), rect.height())).map(|p| rect.min + egui::vec2(p.x, p.y))
    }

    /// Centre and radius (mm) of the selection's footprint, for the gizmos.
    fn selection_ring(&self) -> Option<([f32; 2], f32)> {
        let mut bb = only3d_geom::Aabb::EMPTY;
        for o in self.snap.objects.iter().filter(|o| self.snap.selection.contains(&o.id) && !o.hidden) {
            bb = bb.union(&o.report.bounds);
        }
        if bb.is_empty() {
            return None;
        }
        let c = [(bb.min.x + bb.max.x) * 0.5, (bb.min.y + bb.max.y) * 0.5];
        let r = ((bb.max.x - bb.min.x).hypot(bb.max.y - bb.min.y) * 0.5 + 8.0).max(18.0);
        Some((c, r))
    }

    fn ring_points(&self, c: [f32; 2], r: f32, rect: Rect) -> Vec<Pos2> {
        (0..=72).filter_map(|k| {
            let a = k as f32 / 72.0 * std::f32::consts::TAU;
            self.project(Vec3::new(c[0] + r * a.cos(), c[1] + r * a.sin(), 0.0), rect)
        })
        .collect()
    }

    fn near_polyline(p: Pos2, pts: &[Pos2], tol: f32) -> bool {
        pts.windows(2).any(|w| {
            let (a, b) = (w[0], w[1]);
            let ab = b - a;
            let t = ((p - a).dot(ab) / ab.length_sq().max(1e-6)).clamp(0.0, 1.0);
            (a + ab * t).distance(p) < tol
        })
    }

    /// The move gizmo's axis handles: (axis, from, to) in screen space.
    fn move_handles(&self, rect: Rect) -> Vec<(usize, Pos2, Pos2)> {
        let Some((c, r)) = self.selection_ring() else { return Vec::new() };
        let mut v = Vec::new();
        let z = 0.5;
        for (axis, d) in [(0usize, Vec3::X), (1usize, Vec3::Y)] {
            let o = Vec3::new(c[0], c[1], z);
            if let (Some(a), Some(b)) = (self.project(o + d * (r * 0.35), rect), self.project(o + d * (r + 10.0), rect)) {
                v.push((axis, a, b));
            }
        }
        v
    }

    fn begin_object_drag(&mut self, press: Pos2, rect: Rect, cmd: bool) -> bool {
        if self.mode != Mode::Model {
            return false;
        }
        // size handles (the box around one selected object)
        for (axis, sign, at) in self.size_handles(rect) {
            if at.distance(press) < 9.0 {
                let Some(id) = self.snap.selection.first().cloned() else { break };
                if self.snap.project.object(&id).is_some_and(|o| o.locked) {
                    self.toast("Locked: unlock it in the Workspace panel to resize it".into(), false);
                    return true;
                }
                let Some(ev) = self.snap.evaluated(&id) else { break };
                let b = ev.report.bounds;
                let top = Vec3::new((b.min.x + b.max.x) * 0.5, (b.min.y + b.max.y) * 0.5, b.max.z);
                let ppm = match (self.project(top, rect), self.project(top + Vec3::Z, rect)) {
                    (Some(a), Some(c)) => [c.x - a.x, c.y - a.y],
                    _ => [0.0, -1.0],
                };
                let (o, d) = self.ray_at(press, rect);
                let start = scene::ray_plane_z(o, d, 0.5).unwrap_or(Vec3::ZERO);
                self.drag = Some(ObjDrag {
                    ids: vec![id.clone()],
                    start,
                    plane_z: 0.5,
                    size: Some(SizeDrag { id, axis, sign, orig: [b.min.x, b.min.y, b.min.z, b.max.x, b.max.y, b.max.z], px_per_mm: ppm, press, amount: 0.0 }),
                    ..Default::default()
                });
                return true;
            }
        }
        // rotate ring
        if self.gizmo == Gizmo::Rotate || self.triad() {
            if let Some((c, r)) = self.selection_ring() {
                let pts = self.ring_points(c, r, rect);
                if Self::near_polyline(press, &pts, 9.0) {
                    let (o, d) = self.ray_at(press, rect);
                    if let Some(h) = scene::ray_plane_z(o, d, 0.0) {
                        self.drag = Some(ObjDrag { ids: self.snap.selection.clone(), rotate: true, start: h, plane_z: 0.0, axis: None, pivot: c, delta: [0.0; 2], angle_deg: 0.0, ..Default::default() });
                        return true;
                    }
                }
            }
        }
        // move handles
        if self.gizmo == Gizmo::Move || self.triad() {
            for (axis, a, b) in self.move_handles(rect) {
                if Self::near_polyline(press, &[a, b], 8.0) {
                    let (o, d) = self.ray_at(press, rect);
                    if let Some(h) = scene::ray_plane_z(o, d, 0.5) {
                        self.drag = Some(ObjDrag { ids: self.snap.selection.clone(), rotate: false, start: h, plane_z: 0.5, axis: Some(axis), pivot: [0.0; 2], delta: [0.0; 2], angle_deg: 0.0, ..Default::default() });
                        return true;
                    }
                }
            }
        }
        // grab an object
        let (o, d) = self.ray_at(press, rect);
        let Some((i, t)) = scene::pick(&self.snap.objects, o, d) else { return false };
        let id = self.snap.objects[i].id.clone();
        let mut ids = self.snap.selection.clone();
        if !ids.contains(&id) {
            if cmd {
                ids.push(id.clone());
            } else {
                ids = vec![id.clone()];
            }
            self.select(ids.clone());
        }
        let hit = o + d * t;
        // locked objects stay put (they can still be selected)
        let ids: Vec<String> = ids.into_iter().filter(|id| !self.snap.project.object(id).is_some_and(|o| o.locked)).collect();
        if ids.is_empty() {
            self.toast("Locked: unlock it in the Workspace panel to move it".into(), false);
            return true;
        }
        self.drag = Some(ObjDrag { ids, rotate: false, start: hit, plane_z: hit.z, axis: None, pivot: [0.0; 2], delta: [0.0; 2], angle_deg: 0.0, ..Default::default() });
        true
    }

    /// `lock`: ⇧, keep a move to X or Y. `free`: ⌘, no snapping and no guides. `copy`: ⌥, the
    /// originals stay and copies land where you let go.
    fn update_object_drag(&mut self, p: Pos2, rect: Rect, lock: bool, free: bool, copy: bool) {
        let (o, d) = self.ray_at(p, rect);
        let prefs = self.cfg.gizmo.clone();
        let mm_per_px = self.mm_per_px(rect);
        let Some(drag) = &mut self.drag else { return };
        let Some(q) = scene::ray_plane_z(o, d, drag.plane_z) else { return };
        drag.copy = copy && !drag.rotate && drag.size.is_none();
        let mstep = if free { 0.01 } else { prefs.move_snap_mm.max(0.01) };
        let snap = |v: f32, step: f32| (v / step).round() * step;
        if let Some(sd) = &mut drag.size {
            let cur = sd.orig[3 + sd.axis] - sd.orig[sd.axis];
            sd.amount = if let Ok(v) = drag.typed.parse::<f32>() {
                v.max(0.5) - cur
            } else if sd.axis == 2 {
                let n = (sd.px_per_mm[0] * sd.px_per_mm[0] + sd.px_per_mm[1] * sd.px_per_mm[1]).max(1e-6);
                let mv = p - sd.press;
                snap((mv.x * sd.px_per_mm[0] + mv.y * sd.px_per_mm[1]) / n, mstep)
            } else {
                snap((q[sd.axis] - drag.start[sd.axis]) * sd.sign, mstep)
            }
            .max(0.5 - cur);
            return;
        }
        if drag.rotate {
            let a0 = (drag.start.y - drag.pivot[1]).atan2(drag.start.x - drag.pivot[0]);
            let a1 = (q.y - drag.pivot[1]).atan2(q.x - drag.pivot[0]);
            let mut deg = (a1 - a0).to_degrees();
            deg = actions::wrap_deg(deg);
            let step = if free { 1.0 } else { prefs.rotate_snap_deg.max(0.1) };
            drag.angle_deg = match drag.typed.parse::<f32>() {
                Ok(v) => v,
                Err(_) => (deg / step).round() * step,
            };
            return;
        }
        let mut dx = (q.x - drag.start.x).clamp(-2000.0, 2000.0);
        let mut dy = (q.y - drag.start.y).clamp(-2000.0, 2000.0);
        match drag.axis {
            Some(0) => dy = 0.0,
            Some(_) => dx = 0.0,
            // ⇧: along whichever of X and Y the pointer has gone further
            None if lock => {
                if dx.abs() >= dy.abs() {
                    dy = 0.0;
                } else {
                    dx = 0.0;
                }
            }
            None => {}
        }
        drag.delta = [snap(dx, mstep), snap(dy, mstep)];
        drag.guides.clear();
        drag.gaps.clear();
        if let Ok(v) = drag.typed.parse::<f32>() {
            let axis = drag.axis.unwrap_or(drag.typed_axis);
            drag.delta[axis] = v;
            return;
        }
        if !prefs.smart_guides || free {
            return;
        }
        // smart guides: line the dragged group's edges and centre up with other objects' and
        // the plate's, within about 6 px, then measure the gaps to the nearest neighbours
        let ids = drag.ids.clone();
        let mut g = only3d_geom::Aabb::EMPTY;
        let mut others: Vec<only3d_geom::Aabb> = Vec::new();
        for o in self.snap.objects.iter().filter(|o| !o.hidden) {
            if ids.contains(&o.id) {
                g = g.union(&o.report.bounds);
            } else {
                others.push(o.report.bounds);
            }
        }
        if g.is_empty() {
            return;
        }
        let bed = self.snap.bed.size;
        let tol = 6.0 * mm_per_px;
        let Some(drag) = &mut self.drag else { return };
        for axis in 0..2 {
            if drag.axis.is_some_and(|a| a != axis) {
                continue;
            }
            let (gmin, gmax) = (g.min[axis] + drag.delta[axis], g.max[axis] + drag.delta[axis]);
            let feats = [gmin, (gmin + gmax) * 0.5, gmax];
            let mut cands: Vec<(f32, Option<only3d_geom::Aabb>)> = vec![(bed[axis] * 0.5, None)];
            for b in &others {
                cands.extend([(b.min[axis], Some(*b)), ((b.min[axis] + b.max[axis]) * 0.5, Some(*b)), (b.max[axis], Some(*b))]);
            }
            let best = feats.iter().flat_map(|f| cands.iter().map(move |c| (c.0 - f, c))).filter(|(d, _)| d.abs() < tol).min_by(|a, b| a.0.abs().total_cmp(&b.0.abs()));
            if let Some((shift, (at, other))) = best {
                drag.delta[axis] += shift;
                let o = 1 - axis;
                let (lo, hi) = match other {
                    Some(b) => (b.min[o].min(g.min[o] + drag.delta[o]), b.max[o].max(g.max[o] + drag.delta[o])),
                    None => (0.0, bed[o]),
                };
                let mk = |v: f32, w: f32| if axis == 0 { [v, w] } else { [w, v] };
                drag.guides.push((mk(*at, lo - 4.0), mk(*at, hi + 4.0)));
            }
        }
        // gaps to the nearest neighbour on each side (objects overlapping along the other axis)
        let (gx0, gy0, gx1, gy1) = (g.min.x + drag.delta[0], g.min.y + drag.delta[1], g.max.x + drag.delta[0], g.max.y + drag.delta[1]);
        type Gap = Option<(f32, [f32; 2], [f32; 2])>;
        let mut near: [Gap; 4] = [None; 4];
        for b in &others {
            let ov_y = b.max.y > gy0 && b.min.y < gy1;
            let ov_x = b.max.x > gx0 && b.min.x < gx1;
            let my = (gy0.max(b.min.y) + gy1.min(b.max.y)) * 0.5;
            let mx = (gx0.max(b.min.x) + gx1.min(b.max.x)) * 0.5;
            let cands = [
                (ov_y, b.min.x - gx1, 0usize, [gx1, my], [b.min.x, my]),
                (ov_y, gx0 - b.max.x, 1, [b.max.x, my], [gx0, my]),
                (ov_x, b.min.y - gy1, 2, [mx, gy1], [mx, b.min.y]),
                (ov_x, gy0 - b.max.y, 3, [mx, b.max.y], [mx, gy0]),
            ];
            for (ok, gap, k, a, c) in cands {
                if ok && gap > 0.05 && gap < 150.0 && near[k].is_none_or(|n| gap < n.0) {
                    near[k] = Some((gap, a, c));
                }
            }
        }
        drag.gaps = near.iter().flatten().map(|(gap, a, c)| (*a, *c, *gap)).collect();
    }

    /// Apply a value typed during a drag straight away (the pointer may be still).
    fn apply_typed(&mut self) {
        let Some(d) = self.drag.as_mut() else { return };
        let Ok(v) = d.typed.parse::<f32>() else { return };
        if let Some(sd) = d.size.as_mut() {
            let cur = sd.orig[3 + sd.axis] - sd.orig[sd.axis];
            sd.amount = v.max(0.5) - cur;
        } else if d.rotate {
            d.angle_deg = v;
        } else {
            let axis = d.axis.unwrap_or(d.typed_axis);
            d.delta[axis] = v;
            d.guides.clear();
            d.gaps.clear();
        }
    }

    /// The single object the box handles belong to (triad style, or the Scale tool).
    fn size_handles(&self, rect: Rect) -> Vec<(usize, f32, Pos2)> {
        if !(self.triad() || self.gizmo == Gizmo::Scale) || self.snap.selection.len() != 1 {
            return Vec::new();
        }
        let Some(ev) = self.snap.evaluated(&self.snap.selection[0]) else { return Vec::new() };
        let b = ev.report.bounds;
        let (cx, cy) = ((b.min.x + b.max.x) * 0.5, (b.min.y + b.max.y) * 0.5);
        let pts = [
            (0usize, 1.0f32, Vec3::new(b.max.x, cy, 0.5)),
            (0, -1.0, Vec3::new(b.min.x, cy, 0.5)),
            (1, 1.0, Vec3::new(cx, b.max.y, 0.5)),
            (1, -1.0, Vec3::new(cx, b.min.y, 0.5)),
            (2, 1.0, Vec3::new(cx, cy, b.max.z)),
        ];
        pts.iter().filter_map(|(a, s, w)| self.project(*w, rect).map(|p| (*a, *s, p))).collect()
    }

    fn triad(&self) -> bool {
        self.gizmo == Gizmo::Select && self.cfg.gizmo.style == "triad"
    }

    /// World mm per screen pixel at the selection (for pixel-sized snapping tolerances).
    fn mm_per_px(&self, rect: Rect) -> f32 {
        let c = self.selection_ring().map(|(c, _)| c).unwrap_or([self.snap.bed.size[0] * 0.5, self.snap.bed.size[1] * 0.5]);
        match (self.project(Vec3::new(c[0], c[1], 0.0), rect), self.project(Vec3::new(c[0] + 10.0, c[1], 0.0), rect)) {
            (Some(a), Some(b)) => 10.0 / a.distance(b).max(1e-3),
            _ => 1.0,
        }
    }

    fn commit_object_drag(&mut self) {
        let Some(drag) = self.drag.take() else { return };
        if let Some(sd) = &drag.size {
            if sd.amount.abs() < 1e-3 {
                return;
            }
            let Some(o) = self.snap.project.object(&sd.id).cloned() else { return };
            let cur = sd.orig[3 + sd.axis] - sd.orig[sd.axis];
            let factor = (cur + sd.amount) / cur.max(1e-6);
            // world axis → the object's own scale axis (turned in 90° steps about Z only)
            let quarter = (o.rotation[2] / 90.0).round();
            if sd.axis < 2 && (o.rotation[0].abs() > 0.01 || o.rotation[1].abs() > 0.01 || (o.rotation[2] - quarter * 90.0).abs() > 0.01) {
                self.toast("Sizing by handle works on objects turned in 90° steps; use Scale in the Inspector".into(), true);
                return;
            }
            let local = if sd.axis < 2 && (quarter as i32).rem_euclid(2) == 1 { 1 - sd.axis } else { sd.axis };
            let mut scale = o.scale;
            scale[local] = ((scale[local] * factor) * 10000.0).round() / 10000.0;
            let mut args = json!({ "id": sd.id, "scale": scale });
            if sd.axis < 2 {
                // the opposite side stays where it was
                let mut c = [(sd.orig[0] + sd.orig[3]) * 0.5, (sd.orig[1] + sd.orig[4]) * 0.5];
                c[sd.axis] += sd.amount * 0.5 * sd.sign;
                args["position"] = json!([actions::jf(c[0], 2), actions::jf(c[1], 2)]);
            }
            self.run_calls(vec![("update_object".to_string(), args)], Kind::Plain);
            return;
        }
        if drag.copy && !drag.rotate && (drag.delta[0].abs() > 1e-3 || drag.delta[1].abs() > 1e-3) {
            // ⌥: copies where the pointer let go; the originals stay
            let off = [actions::jf(drag.delta[0], 2) as f32, actions::jf(drag.delta[1], 2) as f32];
            self.copy_by(&drag.ids, off);
            return;
        }
        // moving the copies just made sets the step ⌘D repeats
        if !drag.rotate {
            if let Some(r) = self.repeat.as_mut().filter(|r| !r.ids.is_empty() && same_set(&r.ids, &drag.ids) && r.from.len() == r.ids.len()) {
                let first = &r.ids[0];
                let p = self.snap.project.object(first).and_then(|o| o.position).or_else(|| self.centers.get(first).copied());
                if let Some(p) = p {
                    r.offset = Some([p[0] + drag.delta[0] - r.from[0][0], p[1] + drag.delta[1] - r.from[0][1]]);
                }
            }
        }
        let mut calls = Vec::new();
        for id in &drag.ids {
            let Some(o) = self.snap.project.object(id) else { continue };
            if drag.rotate {
                if drag.angle_deg.abs() < 1e-3 {
                    continue;
                }
                let r = o.rotation;
                calls.push(("update_object".to_string(), json!({ "id": id, "rotation": [actions::jf(r[0], 2), actions::jf(r[1], 2), actions::jf(actions::wrap_deg(r[2] + drag.angle_deg), 2)] })));
            } else {
                if drag.delta[0].abs() < 1e-3 && drag.delta[1].abs() < 1e-3 {
                    continue;
                }
                let p = o.position.or_else(|| self.centers.get(id).copied()).unwrap_or([0.0, 0.0]);
                let np = [actions::jf(p[0] + drag.delta[0], 2), actions::jf(p[1] + drag.delta[1], 2)];
                calls.push(("update_object".to_string(), json!({ "id": id, "position": np })));
            }
        }
        if !calls.is_empty() {
            self.held = Some((self.snap.revision, drag));
            self.run_calls(calls, Kind::Commit);
        }
    }

    /// The flat face under a screen point (reusing the last one while the pointer stays on it).
    fn face_under(&self, q: Pos2, rect: Rect) -> Option<PpFace> {
        let (o, d) = self.ray_at(q, rect);
        let (i, _) = scene::pick(&self.snap.objects, o, d)?;
        let ob = &self.snap.objects[i];
        let (t, tri) = scene::ray_mesh_tri(&ob.mesh, o, d)?;
        let ptr = Arc::as_ptr(&ob.mesh) as usize;
        if let Some(h) = self.pp_hover.as_ref().filter(|h| h.id == ob.id && h.mesh == ptr && h.tris.binary_search(&tri).is_ok()) {
            let mut h = h.clone();
            h.hit = o + d * t;
            return Some(h);
        }
        let f = only3d_geom::faces::flat_region(&ob.mesh, tri, 0.01)?;
        let fill = f.tris.iter().take(4000).map(|t| ob.mesh.triangle(*t)).collect();
        Some(PpFace { id: ob.id.clone(), mesh: ptr, tris: f.tris, normal: f.normal, hit: o + d * t, fill, edges: f.edges })
    }

    fn commit_push_pull(&mut self) {
        let Some(d) = self.pp.take() else { return };
        let dist = d.typed.parse::<f32>().unwrap_or(d.dist);
        if !dist.is_finite() || dist.abs() < 0.005 {
            return;
        }
        let r = |v: f32| (v * 1000.0).round() / 1000.0;
        let at = d.face.hit;
        self.worker.call("update_object", json!({ "id": d.face.id, "push_pull": { "at": [r(at.x), r(at.y), r(at.z)], "distance": r(dist) } }), Kind::Plain);
    }

    /// The face under the pointer, lit; while dragging, where it goes and how far.
    fn push_pull_overlay(&self, ui: &egui::Ui, rect: Rect, hover: Option<Pos2>) {
        if self.gizmo != Gizmo::PushPull || self.mode != Mode::Model {
            return;
        }
        let p = pal();
        let painter = ui.painter().with_clip_rect(rect);
        let (face, dist) = match (&self.pp, &self.pp_hover) {
            (Some(d), _) => (&d.face, d.typed.parse::<f32>().unwrap_or(d.dist)),
            (None, Some(h)) => (h, 0.0),
            _ => return,
        };
        let off = face.normal * dist;
        let proj = |w: Vec3| self.project(w, rect);
        let fill = if dist > 0.0 { p.success } else if dist < 0.0 { p.error } else { p.accent }.gamma_multiply(0.5);
        for t in &face.fill {
            if let (Some(a), Some(b), Some(c)) = (proj(t[0] + off), proj(t[1] + off), proj(t[2] + off)) {
                painter.add(egui::Shape::convex_polygon(vec![a, b, c], fill, egui::Stroke::NONE));
            }
        }
        let line = egui::Stroke::new(2.5, p.accent);
        for e in &face.edges {
            if let (Some(a), Some(b)) = (proj(e[0] + off), proj(e[1] + off)) {
                painter.line_segment([a, b], line);
            }
        }
        if dist.abs() > 1e-4 {
            let thin = egui::Stroke::new(1.0, p.accent.gamma_multiply(0.7));
            for e in face.edges.iter().take(256) {
                if let (Some(a), Some(b)) = (proj(e[0]), proj(e[0] + off)) {
                    painter.line_segment([a, b], thin);
                }
            }
        }
        if let (Some(d), Some(q)) = (&self.pp, hover) {
            let text = if d.typed.is_empty() { format!("{}{} mm", if dist > 0.0 { "+" } else { "" }, (dist * 100.0).round() / 100.0) } else { format!("{}▏ mm", d.typed) };
            let g = painter.layout_no_wrap(text, egui::FontId::monospace(12.5), p.text);
            let r = Rect::from_min_size(q + egui::vec2(16.0, 12.0), g.size() + egui::vec2(12.0, 8.0));
            painter.rect(r, 4.0, p.elevated, egui::Stroke::new(1.0, p.accent), egui::StrokeKind::Inside);
            painter.galley(r.min + egui::vec2(6.0, 4.0), g, p.text);
        }
    }

    /// An object's centre on its plate.
    fn centre_of(&self, id: &str) -> Option<[f32; 2]> {
        self.snap.project.object(id).and_then(|o| o.position).or_else(|| self.centers.get(id).copied())
    }

    /// Copy `ids` one `off` step on (an ⌥-drag, or ⌘D repeating one); the copies get selected.
    fn copy_by(&mut self, ids: &[String], off: [f32; 2]) {
        let from: Vec<[f32; 2]> = ids.iter().map(|id| self.centre_of(id).unwrap_or([0.0, 0.0])).collect();
        let calls: Vec<(String, Value)> = ids.iter().map(|id| ("duplicate_object".to_string(), json!({ "id": id, "offset": [off[0], off[1]] }))).collect();
        self.repeat = Some(RepeatCopy { from, ids: Vec::new(), offset: Some(off) });
        self.run_calls(calls, Kind::Copied);
    }

    /// ⌘D: repeat the last copy's step when the copies are what's selected, else copy the
    /// selection into the nearest free space.
    pub fn duplicate_selection(&mut self) {
        let sel = self.snap.selection.clone();
        if sel.is_empty() {
            return;
        }
        if let Some(off) = self.repeat.as_ref().filter(|r| !r.ids.is_empty() && same_set(&r.ids, &sel)).and_then(|r| r.offset) {
            self.copy_by(&sel, off);
            return;
        }
        let from: Vec<[f32; 2]> = sel.iter().map(|id| self.centre_of(id).unwrap_or([0.0, 0.0])).collect();
        let calls: Vec<(String, Value)> = sel.iter().map(|id| ("duplicate_object".to_string(), json!({ "id": id }))).collect();
        self.repeat = Some(RepeatCopy { from, ids: Vec::new(), offset: None });
        self.run_calls(calls, Kind::Copied);
    }

    /// The live preview transform of an object: (dx, dy, angle rad, pivot).
    fn preview_xf(&self, id: &str) -> Option<([f32; 4], [f32; 2])> {
        let d = self.drag.as_ref().or(self.held.as_ref().map(|h| &h.1))?;
        if !d.ids.iter().any(|x| x == id) {
            return None;
        }
        if d.rotate {
            let c = self.centers.get(id).copied().unwrap_or(d.pivot);
            Some(([0.0, 0.0, d.angle_deg.to_radians(), 0.0], c))
        } else {
            Some(([d.delta[0], d.delta[1], 0.0, 0.0], [0.0, 0.0]))
        }
    }

    pub fn viewport(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        self.vp_rect = rect;
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        let size = rect.size();
        let aspect = size.x / size.y.max(1.0);
        self.vp_aspect = aspect;
        let ppp = ui.ctx().pixels_per_point();
        if !self.cam_ready && size.x > 10.0 && self.snap.revision != u64::MAX {
            self.frame_plate(aspect);
            self.cam_ready = true;
            self.view_plate = self.snap.plate;
        }
        // another plate became active: everything is drawn relative to the active plate, so
        // shift the camera to keep the picture still, then glide over to the new plate
        if self.view_plate != self.snap.plate {
            let n = self.snap.project.plate_count();
            if self.cfg.show_all_plates && n > 1 && self.view_plate <= n {
                let d = scene::plate_offset(self.snap.plate, self.view_plate, n, &self.snap.bed);
                self.cam.target -= Vec3::new(d[0], d[1], 0.0);
                let b = self.snap.bed.size;
                let to = Vec3::new(b[0] * 0.5, b[1] * 0.5, 0.0);
                self.pan_to = Some((self.cam.target, to, Instant::now()));
            }
            self.view_plate = self.snap.plate;
        }
        if let Some((from, to, t0)) = self.pan_to {
            let t = (t0.elapsed().as_secs_f32() / 0.35).min(1.0);
            let e = 1.0 - (1.0 - t).powi(3);
            self.cam.target = from.lerp(to, e);
            if t >= 1.0 {
                self.pan_to = None;
            } else {
                ui.ctx().request_repaint();
            }
        }
        let shown_plates = self.shown_plates();

        // --- input
        let local = |p: Pos2| glam::Vec2::new(p.x - rect.min.x, p.y - rect.min.y);
        let gsize = glam::Vec2::new(size.x, size.y);
        let (mods, drag_delta, scroll, zoom, hover, press) = ui.input(|i| (i.modifiers, i.pointer.delta(), i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos(), i.pointer.press_origin()));
        let shift = mods.shift;
        let d = glam::Vec2::new(drag_delta.x, drag_delta.y);
        let pp_mode = self.gizmo == Gizmo::PushPull && self.mode == Mode::Model;
        if pp_mode {
            // Push/Pull: a drag on a flat face moves it along its normal
            if self.pp.is_none() {
                self.pp_hover = hover.filter(|q| rect.contains(*q)).and_then(|q| self.face_under(q, rect));
            }
            if resp.drag_started_by(egui::PointerButton::Primary) && !shift {
                if let Some((p, f)) = press.and_then(|p| self.face_under(p, rect).map(|f| (p, f))) {
                    let flat = 1.0 / self.mm_per_px(rect).max(1e-4);
                    let ppm = match (self.project(f.hit, rect), self.project(f.hit + f.normal, rect)) {
                        (Some(a), Some(b)) if (b - a).length() > flat * 0.25 => b - a,
                        // looking straight at the face: drag up to pull
                        _ => egui::vec2(0.0, -flat),
                    };
                    if !self.snap.selection.contains(&f.id) {
                        self.select(vec![f.id.clone()]);
                    }
                    self.pp = Some(PpDrag { face: f, press: p, px_per_mm: ppm, dist: 0.0, typed: String::new() });
                }
            }
            let step = if mods.command { 0.01 } else { self.cfg.gizmo.move_snap_mm.max(0.01) };
            if let (Some(d), Some(p)) = (self.pp.as_mut(), hover) {
                if resp.dragged_by(egui::PointerButton::Primary) && d.typed.is_empty() {
                    let raw = (p - d.press).dot(d.px_per_mm) / d.px_per_mm.length_sq().max(1e-6);
                    d.dist = (raw / step).round() * step;
                }
            }
            if self.pp.is_some() && (resp.drag_stopped() || !ui.input(|i| i.pointer.primary_down())) {
                self.commit_push_pull();
            }
        } else {
            // a drag that starts on an object moves it (⇧ locks X or Y, ⌥ copies, ⌘ no snapping);
            // on empty space it orbits, or pans with ⇧
            if resp.drag_started_by(egui::PointerButton::Primary) {
                if let Some(p) = press {
                    self.begin_object_drag(p, rect, mods.command && !shift);
                }
            }
            if let Some(p) = hover.filter(|_| self.drag.is_some()) {
                if resp.dragged_by(egui::PointerButton::Primary) {
                    self.update_object_drag(p, rect, shift, mods.command, mods.alt);
                }
            }
        }
        if self.drag.is_some() && (resp.drag_stopped() || !ui.input(|i| i.pointer.primary_down())) {
            self.commit_object_drag();
        } else if self.drag.is_none() && self.pp.is_none() {
            if resp.dragged_by(egui::PointerButton::Primary) && !shift {
                self.cam.orbit(d);
            } else if resp.dragged_by(egui::PointerButton::Secondary) || resp.dragged_by(egui::PointerButton::Middle) || (resp.dragged_by(egui::PointerButton::Primary) && shift) {
                self.cam.pan(d, size.y);
            }
        }
        if resp.contains_pointer() {
            let mut factor = 1.0;
            if scroll.y.abs() > 0.0 {
                factor *= (-scroll.y * 0.0022).exp();
            }
            if (zoom - 1.0).abs() > 1e-4 {
                factor /= zoom;
            }
            if (factor - 1.0f32).abs() > 1e-5 {
                if let Some(hp) = hover {
                    let anchor = match self.scroll_anchor {
                        Some((a, t, p)) if t.elapsed() < Duration::from_millis(250) && p.distance(hp) < 3.0 => a,
                        _ => self.anchor_at(local(hp), gsize),
                    };
                    self.scroll_anchor = Some((anchor, Instant::now(), hp));
                    self.cam.zoom_about(factor, anchor);
                }
            }
        }
        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let (o, dir) = self.cam.ray(local(p), gsize);
                let add = shift || mods.command;
                match self.pick_any(o, dir, &shown_plates) {
                    // on another plate: that plate becomes active, with the object selected
                    Some((id, true)) => self.select(vec![id]),
                    Some((id, false)) if add => {
                        let mut sel = self.snap.selection.clone();
                        if let Some(i) = sel.iter().position(|x| *x == id) {
                            sel.remove(i);
                        } else {
                            sel.push(id);
                        }
                        self.select(sel);
                    }
                    Some((id, false)) => self.select(vec![id]),
                    None => match self.plate_under(o, dir, &shown_plates) {
                        Some(k) => self.worker.call("plate", json!({"action": "select", "plate": k}), Kind::Plain),
                        None if !add => self.select(Vec::new()),
                        None => {}
                    },
                }
            }
        }
        if resp.double_clicked() {
            self.frame_selection(aspect);
        }
        // right-click: the object under the pointer (selected first), or the empty spot
        if resp.secondary_clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let (o, dir) = self.cam.ray(local(p), gsize);
                let hit = self.pick_any(o, dir, &shown_plates).map(|(id, _)| id);
                let ground = scene::ray_plane_z(o, dir, 0.0).map(|q| [q.x, q.y]);
                self.ctx_target = Some(match hit {
                    Some(id) => {
                        if !self.snap.selection.contains(&id) {
                            self.select(vec![id]);
                        }
                        crate::context_menu::CtxTarget::Objects
                    }
                    None => crate::context_menu::CtxTarget::Empty(ground),
                });
            }
        }
        resp.context_menu(|ui| self.viewport_menu(ui));
        if let Some(hp) = hover.filter(|_| resp.hovered() && self.drag.is_none() && self.mode == Mode::Model) {
            let over_ring = (self.gizmo == Gizmo::Rotate || self.triad()) && self.selection_ring().is_some_and(|(c, r)| Self::near_polyline(hp, &self.ring_points(c, r, rect), 9.0));
            let over_handle = (self.gizmo == Gizmo::Move || self.triad()) && self.move_handles(rect).iter().any(|(_, a, b)| Self::near_polyline(hp, &[*a, *b], 8.0));
            let over_size = self.size_handles(rect).iter().find(|(_, _, at)| at.distance(hp) < 9.0).map(|(a, _, _)| *a);
            if let Some(a) = over_size {
                ui.ctx().set_cursor_icon(if a == 0 { egui::CursorIcon::ResizeHorizontal } else { egui::CursorIcon::ResizeVertical });
            } else if over_ring || over_handle {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
            }
        }
        if self.drag.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }

        // --- frame data
        let p = self.pal;
        let offs: Vec<[f32; 2]> = shown_plates.iter().map(|(_, o)| *o).collect();
        let offs_key = offs.iter().fold(0u64, |h, o| h.wrapping_mul(1_000_003) ^ ((o[0].to_bits() as u64) << 32 | o[1].to_bits() as u64));
        let plate_for = (ppp.to_bits(), self.snap.bed.size.map(f32::to_bits), self.snap.bed.nozzle_areas.len(), p.name, offs_key);
        if self.plate_for != Some(plate_for) {
            self.plate = Arc::new(scene::build_plates(&self.snap.bed, ppp, &p, &offs));
            self.plate_for = Some(plate_for);
        }
        let size_px = [(size.x * ppp).round().max(1.0) as u32, (size.y * ppp).round().max(1.0) as u32];
        let vp = self.cam.view_proj(aspect);
        let (right, up, back) = self.cam.basis();
        let key = (-right * 0.45 + up * 0.55 + back * 0.65 + Vec3::Z * 0.35).normalize();
        let fill = (right * 0.75 - up * 0.1 + back * 0.45).normalize();
        let lin = scene::srgb_to_linear;
        let mut objects = Vec::new();
        let mut keep = Vec::new();
        // an activity row hovered this pass or the last one (panels may draw after the viewport)
        let hover_live = ui.ctx().cumulative_pass_nr().saturating_sub(self.hover_objects.1) <= 1;
        for o in self.snap.objects.iter() {
            if o.hidden || self.mode == Mode::Toolpaths {
                keep.push(o.mesh.clone());
                continue;
            }
            let selected = self.snap.selection.contains(&o.id);
            let hovered = hover_live && self.hover_objects.0.contains(&o.id);
            // the anchor for arranging: the last-selected of several
            let anchor = self.snap.selection.len() > 1 && self.snap.selection.last() == Some(&o.id);
            let xf = self.preview_xf(&o.id);
            let copying = xf.is_some() && self.drag.as_ref().is_some_and(|d| d.copy && d.ids.contains(&o.id));
            // multi-part objects: each body in its own filament's colour
            let bodies: Vec<(Arc<only3d_geom::Mesh>, u32)> = if o.parts.is_empty() { vec![(o.mesh.clone(), o.filament)] } else { o.parts.iter().map(|p| (p.mesh.clone(), p.filament)).collect() };
            for (mesh, filament) in bodies {
                let c = self.snap.project.filaments.get((filament as usize).wrapping_sub(1)).map(|f| scene::parse_hex(&f.color)).unwrap_or([200, 190, 170]);
                let mut inst = ObjInstance::plain(lin(c), if selected { 2 } else { 0 } | if hovered { 4 } else { 0 } | if anchor { 8 } else { 0 });
                if copying {
                    // ⌥-drag: the original stays where it is, the copy follows the pointer
                    objects.push(DrawObj { mesh: mesh.clone(), inst: ObjInstance::plain(lin(c), 0) });
                }
                if let Some((x, pivot)) = xf {
                    inst.xf = x;
                    inst.pivot = pivot;
                }
                objects.push(DrawObj { mesh, inst });
            }
        }
        // the other plates' objects, dimmed, where their plates are drawn
        for (k, off) in &shown_plates {
            for o in self.snap.others.iter().filter(|o| o.plate == *k && !o.hidden) {
                let bodies: Vec<(Arc<only3d_geom::Mesh>, u32)> = if o.parts.is_empty() { vec![(o.mesh.clone(), o.filament)] } else { o.parts.iter().map(|p| (p.mesh.clone(), p.filament)).collect() };
                for (mesh, filament) in bodies {
                    let c = self.snap.project.filaments.get((filament as usize).wrapping_sub(1)).map(|f| scene::parse_hex(&f.color)).unwrap_or([200, 190, 170]);
                    let l = lin(c);
                    let mut inst = ObjInstance::plain([l[0] * 0.55, l[1] * 0.55, l[2] * 0.55, l[3]], 0);
                    inst.xf = [off[0], off[1], 0.0, 0.0];
                    objects.push(DrawObj { mesh, inst });
                }
            }
        }
        let mut toolpath = None;
        let mut hl_end = 0u32;
        if self.mode == Mode::Toolpaths {
            if let Some((_, data)) = &self.toolpaths {
                let n = data.layers();
                if n > 0 {
                    self.layer_hi = self.layer_hi.min(n - 1);
                    if self.one_layer {
                        self.layer_lo = self.layer_hi;
                    }
                    self.layer_lo = self.layer_lo.min(self.layer_hi);
                    let (range, top_end) = data.range(self.layer_lo, self.layer_hi);
                    hl_end = top_end;
                    toolpath = Some(ToolpathDraw { data: data.clone(), range });
                }
            }
        }
        let dots = lin(p.dots);
        let over = lin(p.overhang_3d);
        let globals = Globals {
            view_proj: vp.to_cols_array_2d(),
            eye: self.cam.eye().extend(1.0).to_array(),
            viewport: [size_px[0] as f32, size_px[1] as f32, self.cam.focal_px(size_px[1] as f32), (2.2 * ppp).clamp(2.0, 5.0)],
            key_dir: key.extend(0.0).to_array(),
            fill_dir: fill.extend(0.0).to_array(),
            accent: lin(p.accent_3d),
            outline: lin(p.outline_3d),
            bg_top: lin(p.view_top),
            bg_bottom: lin(p.view_bottom),
            flags: [self.feature_mask, hl_end, u32::from(self.gamma_out), 0],
            dots: [dots[0], dots[1], dots[2], (22.0 * ppp).round()],
            overhang: [over[0], over[1], over[2], if self.overhangs && self.mode == Mode::Model { 1.0 } else { 0.0 }],
        };
        let frame = Arc::new(FrameData { globals, size_px, objects, keep, plate: self.plate.clone(), toolpath });
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(rect, ViewportCallback { frame }));

        // --- overlays
        self.gizmo_overlay(ui, rect, hover);
        self.push_pull_overlay(ui, rect, hover);
        self.viewport_overlays(ui, rect, gsize);
        self.plate_labels(ui, rect, gsize, &shown_plates);
        // a model dragged from the Files panel: import it where it is let go
        if let Some(paths) = egui::DragAndDrop::payload::<DragFiles>(ui.ctx()).filter(|f| f.all_models()).map(|f| f.0.clone()) {
            if let Some(hp) = ui.ctx().pointer_latest_pos().filter(|q| rect.contains(*q)) {
                let (o, d) = self.ray_at(hp, rect);
                let b = self.snap.bed.size;
                let on = |g: Vec3, off: [f32; 2]| g.x >= off[0] && g.x <= off[0] + b[0] && g.y >= off[1] && g.y <= off[1] + b[1];
                let spot = scene::ray_plane_z(o, d, 0.0).and_then(|g| {
                    std::iter::once((self.snap.plate, [0.0f32, 0.0])).chain(shown_plates.iter().copied()).find(|(_, off)| on(g, *off)).map(|(k, off)| (k, [g.x - off[0], g.y - off[1]]))
                });
                ui.painter().rect_filled(rect.shrink(10.0), 8.0, p.accent.gamma_multiply(0.08));
                ui.painter().rect_stroke(rect.shrink(10.0), 8.0, egui::Stroke::new(1.5, p.accent), egui::StrokeKind::Inside);
                let what = paths.first().and_then(|f| f.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let label = match spot {
                    Some((k, _)) if k != self.snap.plate => format!("Drop {what} onto {}", self.snap.project.plate_name(k)),
                    Some(_) => format!("Drop {what} here"),
                    None => format!("Drop {what} onto this plate"),
                };
                let g = ui.painter().layout_no_wrap(label, egui::FontId::proportional(13.0), p.accent);
                let r = Rect::from_min_size(hp + egui::vec2(14.0, 10.0), g.size() + egui::vec2(12.0, 8.0));
                ui.painter().rect(r, 4.0, p.elevated, egui::Stroke::new(1.0, p.accent), egui::StrokeKind::Inside);
                ui.painter().galley(r.min + egui::vec2(6.0, 4.0), g, p.accent);
                if ui.ctx().input(|i| i.pointer.any_released()) {
                    egui::DragAndDrop::clear_payload(ui.ctx());
                    self.import_files_onto(&paths, spot.map(|s| s.0), spot.map(|s| s.1));
                }
            }
        }
        // files from Finder: say what dropping them does (models import, the rest open as tabs)
        let hovered: Vec<PathBuf> = ui.ctx().input(|i| i.raw.hovered_files.iter().filter_map(|f| f.path.clone()).collect());
        let hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
        if hovering {
            let kinds: Vec<only3d_studio::workspace::FileKind> = hovered.iter().map(|f| only3d_studio::workspace::kind_of(&f.to_string_lossy())).collect();
            let models = kinds.iter().filter(|k| matches!(k, only3d_studio::workspace::FileKind::Model | only3d_studio::workspace::FileKind::Project)).count();
            let text = if kinds.is_empty() || models == kinds.len() {
                "Drop to import onto the plate"
            } else if models == 0 {
                "Drop to open as a tab"
            } else {
                "Drop: models import, other files open as tabs"
            };
            ui.painter().rect_filled(rect.shrink(10.0), 8.0, p.accent.gamma_multiply(0.12));
            ui.painter().rect_stroke(rect.shrink(10.0), 8.0, egui::Stroke::new(1.5, p.accent), egui::StrokeKind::Inside);
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(18.0), p.accent);
        }
    }

    /// The object under a ray: on the active plate, or (all-plates view) on another plate shown.
    /// Returns its id and whether it is on another plate.
    fn pick_any(&self, o: Vec3, dir: Vec3, shown: &[(u32, [f32; 2])]) -> Option<(String, bool)> {
        if self.mode != Mode::Model {
            return None;
        }
        let mut best = scene::pick(&self.snap.objects, o, dir).map(|(i, t)| (self.snap.objects[i].id.clone(), t, false));
        for (k, off) in shown {
            let objs: Vec<EvaluatedObject> = self.snap.others.iter().filter(|x| x.plate == *k && !x.hidden).cloned().collect();
            if let Some((i, t)) = scene::pick(&objs, o - Vec3::new(off[0], off[1], 0.0), dir) {
                if best.as_ref().is_none_or(|b| t < b.1) {
                    best = Some((objs[i].id.clone(), t, true));
                }
            }
        }
        best.map(|(id, _, other)| (id, other))
    }

    /// The other plate whose bed is under a ray, if any.
    fn plate_under(&self, o: Vec3, dir: Vec3, shown: &[(u32, [f32; 2])]) -> Option<u32> {
        let g = scene::ray_plane_z(o, dir, 0.0)?;
        let b = self.snap.bed.size;
        shown.iter().find(|(_, off)| g.x >= off[0] && g.x <= off[0] + b[0] && g.y >= off[1] && g.y <= off[1] + b[1]).map(|(k, _)| *k)
    }

    /// The world point under the cursor for zooming: the model if hit, else the plate.
    fn anchor_at(&self, p: glam::Vec2, size: glam::Vec2) -> Vec3 {
        let (o, d) = self.cam.ray(p, size);
        if self.mode == Mode::Model {
            if let Some((_, t)) = scene::pick(&self.snap.objects, o, d) {
                return o + d * t;
            }
        }
        if let Some(h) = scene::ray_plane_z(o, d, 0.0) {
            if (h - self.cam.eye()).length() < self.cam.dist * 4.0 {
                return h;
            }
        }
        // the plane through the orbit target facing the camera
        let n = self.cam.dir_from_target();
        let denom = d.dot(n);
        if denom.abs() > 1e-6 {
            let t = (self.cam.target - o).dot(n) / denom;
            if t > 0.0 {
                return o + d * t;
            }
        }
        self.cam.target
    }

    /// Rotate ring / move handles / scale hint drawn over the selection.
    fn gizmo_overlay(&mut self, ui: &mut egui::Ui, rect: Rect, hover: Option<Pos2>) {
        if self.mode != Mode::Model {
            return;
        }
        let p = pal();
        let painter = ui.painter_at(rect);
        let chip = |painter: &egui::Painter, at: Pos2, t: String| {
            let g = painter.layout_no_wrap(t, egui::FontId::monospace(12.0), p.on_accent);
            let r = Rect::from_min_size(at + egui::vec2(14.0, -22.0), g.size() + egui::vec2(10.0, 4.0));
            painter.rect_filled(r, 3.0, p.accent);
            painter.galley(r.min + egui::vec2(5.0, 2.0), g, p.on_accent);
        };
        // smart guides and the gaps to neighbours, while a move drag runs
        if let Some(d) = self.drag.as_ref().filter(|d| !d.rotate && d.size.is_none()) {
            for (a, b) in &d.guides {
                if let (Some(a), Some(b)) = (self.project(Vec3::new(a[0], a[1], 0.3), rect), self.project(Vec3::new(b[0], b[1], 0.3), rect)) {
                    painter.add(egui::Shape::dashed_line(&[a, b], egui::Stroke::new(1.2, p.warning), 5.0, 3.0));
                }
            }
            for (a, b, gap) in &d.gaps {
                if let (Some(a), Some(b)) = (self.project(Vec3::new(a[0], a[1], 0.3), rect), self.project(Vec3::new(b[0], b[1], 0.3), rect)) {
                    painter.line_segment([a, b], egui::Stroke::new(1.0, p.warning.gamma_multiply(0.9)));
                    for e in [a, b] {
                        painter.circle_filled(e, 2.0, p.warning);
                    }
                    let mid = a + (b - a) * 0.5;
                    let g = painter.layout_no_wrap(format!("{gap:.1}"), egui::FontId::monospace(10.5), p.text);
                    let r = Rect::from_center_size(mid, g.size() + egui::vec2(8.0, 2.0));
                    painter.rect(r, 3.0, p.elevated, egui::Stroke::new(1.0, p.warning.gamma_multiply(0.6)), egui::StrokeKind::Inside);
                    painter.galley(r.min + egui::vec2(4.0, 1.0), g, p.text);
                }
            }
        }
        if self.snap.selection.is_empty() {
            return;
        }
        let triad = self.triad();
        let typed = self.drag.as_ref().map(|d| d.typed.clone()).unwrap_or_default();
        // while objects move, their handles would sit at the old place: show only the value
        if let (Some(d), Some(hp)) = (self.drag.as_ref().filter(|d| !d.rotate && d.size.is_none()), hover) {
            let t = if typed.is_empty() {
                format!("Δ {:+.1}, {:+.1} mm", d.delta[0], d.delta[1])
            } else {
                format!("Δ{} = {typed}_ mm  ⇥ ⏎", if d.axis.unwrap_or(d.typed_axis) == 0 { "x" } else { "y" })
            };
            chip(&painter, hp, t);
            return;
        }
        // rotate ring
        if self.gizmo == Gizmo::Rotate || triad {
            if let Some((c, r)) = self.selection_ring() {
                let pts = self.ring_points(c, r, rect);
                let active = self.drag.as_ref().is_some_and(|d| d.rotate);
                let hot = active || (self.drag.is_none() && hover.is_some_and(|h| Self::near_polyline(h, &pts, 9.0)));
                let col = if triad && !hot { p.accent.gamma_multiply(0.55) } else { p.accent };
                painter.add(egui::Shape::line(pts, egui::Stroke::new(if hot { 3.0 } else if triad { 1.5 } else { 2.0 }, col)));
                if !triad || hot {
                    for k in 0..24 {
                        let a = k as f32 / 24.0 * std::f32::consts::TAU;
                        let len = if k % 6 == 0 { 5.0 } else { 2.5 };
                        if let (Some(a0), Some(a1)) = (self.project(Vec3::new(c[0] + r * a.cos(), c[1] + r * a.sin(), 0.0), rect), self.project(Vec3::new(c[0] + (r + len) * a.cos(), c[1] + (r + len) * a.sin(), 0.0), rect)) {
                            painter.line_segment([a0, a1], egui::Stroke::new(1.2, p.accent.gamma_multiply(0.8)));
                        }
                    }
                }
                if let (Some(d), Some(hp)) = (self.drag.as_ref().filter(|d| d.rotate), hover) {
                    chip(&painter, hp, if typed.is_empty() { format!("{:+.0}°", d.angle_deg) } else { format!("{typed}_°  ⏎") });
                }
            }
        }
        // move arrows
        if self.gizmo == Gizmo::Move || triad {
            let active_axis = self.drag.as_ref().filter(|d| d.size.is_none()).and_then(|d| d.axis);
            for (axis, a, b) in self.move_handles(rect) {
                let col = if axis == 0 { p.error } else { p.success };
                let hot = active_axis == Some(axis) || (self.drag.is_none() && hover.is_some_and(|h| Self::near_polyline(h, &[a, b], 8.0)));
                let st = egui::Stroke::new(if hot { 3.5 } else { 2.5 }, col);
                painter.line_segment([a, b], st);
                let dir = (b - a).normalized();
                let n = egui::vec2(-dir.y, dir.x);
                painter.add(egui::Shape::convex_polygon(vec![b + dir * 9.0, b + n * 5.0, b - n * 5.0], col, egui::Stroke::NONE));
                painter.text(b + dir * 18.0, egui::Align2::CENTER_CENTER, if axis == 0 { "X" } else { "Y" }, egui::FontId::monospace(11.0), col);
            }
        }
        // box handles: sides for width and depth, the top for height
        let sizing = self.drag.as_ref().and_then(|d| d.size.clone());
        for (axis, sign, at) in self.size_handles(rect) {
            let active = sizing.as_ref().is_some_and(|sd| sd.axis == axis && sd.sign == sign);
            let hot = active || (self.drag.is_none() && hover.is_some_and(|h| h.distance(at) < 9.0));
            let r = Rect::from_center_size(at, egui::vec2(if hot { 10.0 } else { 8.0 }, if hot { 10.0 } else { 8.0 }));
            let col = if axis == 2 { p.accent } else if axis == 0 { p.error } else { p.success };
            painter.rect(r, 2.0, if hot { col } else { p.elevated }, egui::Stroke::new(1.5, col), egui::StrokeKind::Middle);
        }
        // while sizing: the new box and its size
        if let Some(sd) = sizing {
            let o = sd.orig;
            let (mut lo, mut hi) = ([o[0], o[1], o[2]], [o[3], o[4], o[5]]);
            if sd.axis == 2 {
                hi[2] += sd.amount;
            } else if sd.sign > 0.0 {
                hi[sd.axis] += sd.amount;
            } else {
                lo[sd.axis] -= sd.amount;
            }
            let corners = [[lo[0], lo[1]], [hi[0], lo[1]], [hi[0], hi[1]], [lo[0], hi[1]], [lo[0], lo[1]]];
            for z in [0.4, hi[2]] {
                let pts: Vec<Pos2> = corners.iter().filter_map(|c| self.project(Vec3::new(c[0], c[1], z), rect)).collect();
                if pts.len() == 5 {
                    painter.add(egui::Shape::dashed_line(&pts, egui::Stroke::new(1.2, p.accent), 5.0, 3.0));
                }
            }
            let name = ["W", "D", "H"][sd.axis];
            let size = hi[sd.axis] - lo[sd.axis];
            if let Some(hp) = hover {
                chip(&painter, hp, if typed.is_empty() { format!("{name} {size:.1} mm ({:+.1})", sd.amount) } else { format!("{name} = {typed}_ mm  ⏎") });
            }
        }
    }

    /// In the all-plates view: each plate's name on its front edge (the active one at the back,
    /// in the accent colour). Clicking a name makes that plate active.
    fn plate_labels(&mut self, ui: &mut egui::Ui, rect: Rect, gsize: glam::Vec2, shown: &[(u32, [f32; 2])]) {
        if shown.is_empty() {
            return;
        }
        let p = pal();
        let mut pick: Option<u32> = None;
        let mut items: Vec<(u32, [f32; 2])> = shown.to_vec();
        items.push((self.snap.plate, [0.0, 0.0]));
        for (k, off) in items {
            let active = k == self.snap.plate;
            let fg = if active { p.accent } else { p.muted };
            let Some((r, galley)) = self.plate_label_rect(ui, k, off, rect, gsize, fg) else { continue };
            let resp = ui.interact(r, egui::Id::unique(("plate_label", k)), Sense::click());
            let fill = if active { theme::soft(p.accent) } else if resp.hovered() { p.hover } else { p.elevated };
            ui.painter().rect(r, 4.0, fill, egui::Stroke::new(1.0, if active { p.accent } else { p.border_variant }), egui::StrokeKind::Inside);
            ui.painter().galley(r.min + egui::vec2(7.0, 4.0), galley, fg);
            if resp.on_hover_text(if active { "The active plate: edits, slicing and printing act on it" } else { "Make this the active plate" }).clicked() && !active {
                pick = Some(k);
            }
        }
        if let Some(k) = pick {
            self.worker.call("plate", json!({"action": "select", "plate": k}), Kind::Plain);
        }
    }

    /// Where plate `k`'s name pill goes (just behind its back edge), with its text. None when it
    /// would leave the viewport or sit under the toolbar row at the top.
    fn plate_label_rect(&self, ui: &egui::Ui, k: u32, off: [f32; 2], rect: Rect, gsize: glam::Vec2, fg: Color32) -> Option<(Rect, Arc<egui::Galley>)> {
        let b = self.snap.bed.size;
        let q = self.cam.project(Vec3::new(off[0] + b[0] * 0.5, off[1] + b[1] + 12.0, 0.0), gsize)?;
        let count = self.snap.project.objects.iter().filter(|o| o.plate == k).count();
        let text = format!("{k} · {}  {count}", self.snap.project.plate_name(k));
        let galley = ui.painter().layout_no_wrap(text, egui::FontId::proportional(12.0), fg);
        let r = Rect::from_center_size(rect.min + egui::vec2(q.x, q.y), galley.size() + egui::vec2(14.0, 8.0));
        let free = Rect::from_min_max(rect.min + egui::vec2(0.0, 44.0), rect.max);
        free.contains_rect(r).then_some((r, galley))
    }

    fn viewport_overlays(&mut self, ui: &mut egui::Ui, rect: Rect, gsize: glam::Vec2) {
        let p = pal();
        let painter = ui.painter_at(rect);
        // plate labels, on small pills so they read over the plate or the background
        let bed = self.snap.bed.clone();
        let pill = |pos: Pos2, text: String, fg: Color32| {
            let galley = painter.layout_no_wrap(text, egui::FontId::monospace(10.5), fg);
            let r = Rect::from_center_size(pos, galley.size() + egui::vec2(12.0, 6.0));
            if rect.contains_rect(r) {
                painter.rect(r, 3.0, p.elevated, egui::Stroke::new(1.0, p.border_variant), egui::StrokeKind::Inside);
                painter.galley(r.min + egui::vec2(6.0, 3.0), galley, fg);
            }
        };
        // zoomed out (all plates in view), the plate's own labels would only crowd the names
        let plate_px = match (self.cam.project(Vec3::ZERO, gsize), self.cam.project(Vec3::new(bed.size[0], 0.0, 0.0), gsize)) {
            (Some(a), Some(b)) => (b - a).length(),
            _ => f32::INFINITY,
        };
        let detail = plate_px > 460.0 || self.snap.project.plate_count() < 2 || !self.cfg.show_all_plates;
        for (x0, x1, label) in scene::nozzle_strips(&bed).into_iter().filter(|_| detail) {
            if let Some(q) = self.cam.project(Vec3::new((x0 + x1) * 0.5, -14.0, 0.0), gsize) {
                pill(rect.min + egui::vec2(q.x, q.y), label.to_string(), p.warning);
            }
        }
        if let Some(q) = self.cam.project(Vec3::new(bed.size[0] * 0.5, -14.0, 0.0), gsize).filter(|_| detail) {
            pill(rect.min + egui::vec2(q.x, q.y), format!("{} · {:.0} × {:.0} mm", short_machine(&self.snap.project.printer.machine), bed.size[0], bed.size[1]), p.muted);
        }

        // the printer's built-in camera, as a vertical line where it sits
        if let (true, Some(c)) = (self.cfg.show_camera, self.cfg.camera_xy) {
            let h = self.snap.bed.max_height.max(50.0);
            if let (Some(a), Some(b)) = (self.cam.project(Vec3::new(c[0], c[1], 0.0), gsize), self.cam.project(Vec3::new(c[0], c[1], h), gsize)) {
                let (a, b) = (rect.min + egui::vec2(a.x, a.y), rect.min + egui::vec2(b.x, b.y));
                painter.add(egui::Shape::dashed_line(&[a, b], egui::Stroke::new(1.5, p.accent), 6.0, 4.0));
                painter.circle_filled(a, 3.0, p.accent);
                pill(b + egui::vec2(0.0, -10.0), "camera".into(), p.accent);
            }
        }

        // floating tool groups
        let mut top_left = rect.min.y + 10.0;
        if self.mode == Mode::Model {
            let r = self.tool_strip(ui, egui::pos2(rect.min.x + 10.0, rect.min.y + 10.0));
            top_left = r.max.y + 8.0;
        }
        self.view_strip(ui, rect);

        // plate tabs (top centre), then the agent activity pill under them
        let mut top_center = self.plate_tabs(ui, rect).max.y + 6.0;
        if let Some((t, text)) = &self.agent_flash {
            let age = t.elapsed().as_secs_f32();
            if age < 6.0 {
                let alpha = ((6.0 - age) / 1.0).clamp(0.0, 1.0);
                let text: String = text.chars().take(90).collect();
                let galley = painter.layout_no_wrap(text, egui::FontId::proportional(12.0), p.text.gamma_multiply(alpha));
                let r = Rect::from_center_size(egui::pos2(rect.center().x, top_center + 13.0), galley.size() + egui::vec2(30.0, 10.0));
                painter.rect(r, 4.0, p.elevated.gamma_multiply(alpha), egui::Stroke::new(1.0, p.accent.gamma_multiply(alpha)), egui::StrokeKind::Inside);
                painter.circle_filled(egui::pos2(r.min.x + 11.0, r.center().y), 3.0, p.accent.gamma_multiply(alpha));
                painter.galley(egui::pos2(r.min.x + 20.0, r.min.y + 5.0), galley, p.text);
                top_center = r.max.y + 6.0;
                ui.ctx().request_repaint_after(Duration::from_millis(50));
            }
        }

        // the one primary action, bottom right: Slice → Print… → Printing 42%
        self.primary_rect = self.primary_action(ui, rect);

        // toolpath legend + layer sliders
        if self.mode == Mode::Toolpaths {
            self.toolpath_overlay(ui, rect, top_left, top_center);
        }

        // gizmo hint (bottom-left)
        if self.mode == Mode::Model {
            let k = |a: Action| self.keymap.label_for(a).unwrap_or_else(|| "—".into());
            let hint = match (self.gizmo, self.snap.selection.is_empty()) {
                (_, true) if self.snap.plate_is_empty() => None,
                (Gizmo::PushPull, _) => Some(format!("Push/Pull · point at a flat face, drag it out or in ({} mm steps) · type a distance while dragging · ⌘ no snapping · Esc exits", self.cfg.gizmo.move_snap_mm)),
                (_, true) => Some(format!("Click an object to select · drag it to move · ⇧/⌘-click adds · {} select all", k(Action::SelectAll))),
                (Gizmo::Select, false) if self.triad() => Some(format!(
                    "Drag to move ({} mm) · {} push/pull a face · ⇧ X or Y only · ⌥ copy · ⌘ no snapping · ring turns · squares size · {} lay flat",
                    self.cfg.gizmo.move_snap_mm,
                    k(Action::ToolPushPull),
                    k(Action::PlaceOnFace)
                )),
                (Gizmo::Select, false) => Some(format!("Drag to move · arrows nudge · {} move · {} rotate · {} scale · {} push/pull a face · {} lay flat · Esc deselect", k(Action::ToolMove), k(Action::ToolRotate), k(Action::ToolScale), k(Action::ToolPushPull), k(Action::PlaceOnFace))),
                (Gizmo::Move, false) => Some("Move · drag the X / Y handles or hold ⇧ to lock an axis · ⌥ copy · ⌘ no snapping · Esc exits".to_string()),
                (Gizmo::Rotate, false) => Some("Rotate · drag the ring (15° steps, ⌘ for 1°) · ← → 15° · ⇧ 90° · Esc exits".to_string()),
                (Gizmo::Scale, false) => Some(format!("Scale · ↑ / ↓ 5% · ⇧ 25% · exact values in the Inspector · to lengthen one side, {} push/pull · Esc exits", k(Action::ToolPushPull))),
            };
            if let Some(h) = hint {
                // one line, cut short before the Slice / Print card
                let right = self.primary_rect.filter(|r| r.max.y > rect.max.y - 70.0).map(|r| r.min.x - 12.0).unwrap_or(rect.max.x - 12.0);
                let mut job = egui::text::LayoutJob::simple_singleline(h, egui::FontId::monospace(10.5), p.muted);
                job.wrap = egui::text::TextWrapping::truncate_at_width((right - rect.min.x - 12.0).max(60.0));
                let g = painter.layout_job(job);
                painter.galley(egui::pos2(rect.min.x + 12.0, rect.max.y - 12.0 - g.size().y), g, p.muted);
            }
            if self.overhangs {
                let g = painter.layout_job(theme::label_job("overhangs > 45°", p.overhang_3d_color(), 10.5));
                let r = Rect::from_min_size(egui::pos2(rect.min.x + 12.0, rect.max.y - 40.0), g.size() + egui::vec2(10.0, 6.0));
                painter.rect(r, 3.0, p.elevated, egui::Stroke::new(1.0, p.border_variant), egui::StrokeKind::Inside);
                painter.galley(r.min + egui::vec2(5.0, 3.0), g, p.text);
            }
        }

        // empty state
        if self.snap.plate_is_empty() && self.mode == Mode::Model {
            // on the active plate itself (with other plates shown, the middle of the view can be
            // another plate), as wide as the plate looks, and clear of the plate's name
            let b = self.snap.bed.size;
            let centre = self.cam.project(Vec3::new(b[0] * 0.5, b[1] * 0.5, 0.0), gsize).map(|q| rect.min + egui::vec2(q.x, q.y)).unwrap_or(rect.center());
            let plate_w = match (self.cam.project(Vec3::new(0.0, b[1] * 0.5, 0.0), gsize), self.cam.project(Vec3::new(b[0], b[1] * 0.5, 0.0), gsize)) {
                (Some(a), Some(c)) => (c - a).length(),
                _ => 400.0,
            };
            let compact = plate_w < 360.0;
            let w = (plate_w * 0.8).clamp(240.0, 400.0);
            let h = if compact { 64.0 } else { 120.0 };
            let mut r = Rect::from_center_size(centre, egui::vec2(w, h));
            if let Some((label, _)) = self.plate_label_rect(ui, self.snap.plate, [0.0, 0.0], rect, gsize, p.accent) {
                if label.expand(6.0).intersects(r) {
                    r = r.translate(egui::vec2(0.0, label.max.y + 8.0 - r.min.y));
                }
            }
            // inside the view: below the toolbar row, above the slice button row
            let room = Rect::from_min_max(rect.min + egui::vec2(12.0, 48.0), rect.max - egui::vec2(12.0, 64.0));
            let dx = (room.min.x - r.min.x).max(0.0) + (room.max.x - r.max.x).min(0.0);
            let dy = (room.min.y - r.min.y).max(0.0) + (room.max.y - r.max.y).min(0.0);
            let r = r.translate(egui::vec2(dx, dy));
            ui.scope_builder(egui::UiBuilder::new().max_rect(r), |ui| {
                egui::Frame::new().fill(p.elevated).stroke(egui::Stroke::new(1.0, p.border)).corner_radius(6).inner_margin(if compact { 10 } else { 14 }).show(ui, |ui| {
                    ui.set_width(w - if compact { 20.0 } else { 28.0 });
                    ui.vertical_centered(|ui| {
                        ui.label(theme::label_job("an empty plate", p.muted, 10.5));
                        ui.add_space(4.0);
                        if compact {
                            ui.label(RichText::new("Add or import a model (⌘I), or drop files here.").size(12.0).color(p.muted));
                        } else {
                            ui.label(RichText::new("Add a primitive or import a model (⌘I), or drop STL / 3MF / OBJ / SCAD files here. Agents on the live link build here too.").color(p.muted));
                            ui.add_space(4.0);
                            let palette = self.keymap.label_for(Action::Palette).unwrap_or_default();
                            ui.label(RichText::new(format!("{palette}  command palette")).monospace().size(11.0).color(p.placeholder));
                        }
                    });
                });
            });
        }

        // eval error banner
        if let Some(e) = &self.snap.eval_error {
            let right = self.primary_rect.map(|r| r.min.x - 12.0).unwrap_or(rect.max.x - 12.0);
            let r = Rect::from_min_max(egui::pos2(rect.min.x + 12.0, rect.max.y - 60.0), egui::pos2(right.max(rect.min.x + 200.0), rect.max.y - 30.0));
            painter.rect(r, 4.0, theme::soft(p.error), egui::Stroke::new(1.0, p.error.gamma_multiply(0.6)), egui::StrokeKind::Inside);
            painter.text(r.left_center() + egui::vec2(10.0, 0.0), egui::Align2::LEFT_CENTER, format!("Model error — {e}"), egui::FontId::proportional(12.0), p.error);
        }

        if self.show_stats {
            let tris: usize = self.snap.objects.iter().filter(|o| !o.hidden).map(|o| o.mesh.indices.len()).sum();
            let segs = self.toolpaths.as_ref().map(|t| t.1.segs.len()).unwrap_or(0);
            let n = self.frame_times.len().max(1) as f32;
            let avg = self.frame_times.iter().sum::<f32>() / n;
            let worst = self.frame_times.iter().copied().fold(0.0f32, f32::max);
            let text = format!("{:.2}M triangles · {:.2}M toolpath segments · {:.1} ms avg / {:.1} ms worst (last {} frames)", tris as f64 / 1e6, segs as f64 / 1e6, avg, worst, self.frame_times.len());
            painter.text(egui::pos2(rect.min.x + 12.0, rect.max.y - 28.0), egui::Align2::LEFT_BOTTOM, text, egui::FontId::monospace(11.0), p.text);
            ui.ctx().request_repaint();
        }

        // toasts (bottom-right)
        self.toasts.retain(|t| t.at.elapsed() < Duration::from_secs(if t.warn { 15 } else if t.error { 8 } else { 4 }));
        let mut ty = self.primary_rect.map(|r| r.min.y - 8.0).unwrap_or(rect.max.y - 12.0);
        for t in self.toasts.iter().rev() {
            let col = if t.error { p.error } else { p.text };
            let accent = if t.error { p.error } else if t.warn { p.warning } else { p.success };
            let galley = painter.layout(t.text.clone(), egui::FontId::proportional(12.0), col, 420.0);
            let sz = galley.size() + egui::vec2(28.0, 12.0);
            let r = Rect::from_min_size(egui::pos2(rect.max.x - 12.0 - sz.x, ty - sz.y), sz);
            painter.rect(r, 4.0, p.elevated, egui::Stroke::new(1.0, if t.error || t.warn { accent.gamma_multiply(0.7) } else { p.border }), egui::StrokeKind::Inside);
            painter.rect_filled(Rect::from_min_size(r.min + egui::vec2(8.0, r.height() * 0.5 - 3.0), egui::vec2(6.0, 6.0)), 1.0, accent);
            painter.galley(r.min + egui::vec2(20.0, 6.0), galley, col);
            ty = r.min.y - 6.0;
        }
        if !self.toasts.is_empty() {
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
    }

    /// The one primary action, at the bottom right of the viewport. It follows the job: Slice
    /// plate → (time, grams) Print… → while the printer works, its progress (click for the
    /// Printer panel). Returns where it was drawn.
    fn primary_action(&mut self, ui: &mut egui::Ui, rect: Rect) -> Option<Rect> {
        let p = pal();
        let printing = self.printer_data.as_ref().filter(|_| matches!(self.printer_status, Some(Ok(_)))).and_then(|d| {
            let state = d.get("gcode_state").and_then(|s| s.as_str()).unwrap_or("").to_ascii_uppercase();
            matches!(state.as_str(), "RUNNING" | "PREPARE" | "PAUSE").then(|| {
                let pct = d.get("progress_percent").and_then(|v| v.as_u64()).unwrap_or(0);
                let left = d.get("remaining_minutes").and_then(|v| v.as_u64());
                let name = d.get("subtask_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                (state, pct, left, name)
            })
        });
        if self.snap.plate_is_empty() && printing.is_none() {
            return None;
        }
        let width = 210.0;
        // keep clear of the preview's "From layer" bar at the bottom centre
        let mut bottom = rect.max.y - 12.0;
        if self.mode == Mode::Toolpaths && rect.center().x + 218.0 > rect.max.x - 12.0 - width {
            bottom = rect.max.y - 60.0;
        }
        let slicing = self.slicing();
        let current = self.snap.slice_current && self.snap.last_slice.is_some();
        // what finished prints taught (re-read every minute; cheap)
        if self.time_correction.as_ref().is_none_or(|(t, _)| t.elapsed() > Duration::from_secs(60)) {
            let name = self.default_printer.clone().or_else(|| self.printers.first().cloned());
            self.time_correction = Some((Instant::now(), name.and_then(|n| only3d_studio::timing::correction(&n))));
        }
        let learned = self.time_correction.as_ref().and_then(|(_, c)| c.clone());
        let mut act: Option<&'static str> = None;
        let area = egui::Area::new(egui::Id::unique("o3d_primary_action")).order(egui::Order::Middle).pivot(egui::Align2::RIGHT_BOTTOM).fixed_pos(egui::pos2(rect.max.x - 12.0, bottom)).interactable(true);
        let shown = area.show(ui.ctx(), |ui| {
            egui::Frame::new().fill(p.elevated).stroke(egui::Stroke::new(1.0, p.border)).corner_radius(6).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                ui.set_width(width - 16.0);
                ui.spacing_mut().item_spacing.y = 6.0;
                if let Some((state, pct, left, name)) = &printing {
                    let label = match state.as_str() {
                        "PAUSE" => format!("Paused at {pct}%"),
                        "PREPARE" => "Preparing to print".to_string(),
                        _ => format!("Printing {pct}%"),
                    };
                    let tip = if name.is_empty() { "Open the Printer panel".to_string() } else { format!("{name} · open the Printer panel") };
                    let r = ui
                        .vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(label).size(12.5).family(theme::medium()).color(if state == "PAUSE" { p.warning } else { p.text }));
                                if let Some(m) = left {
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        ui.label(RichText::new(if *m >= 60 { format!("{}h {:02}m left", m / 60, m % 60) } else { format!("{m} min left") }).size(11.0).color(p.muted));
                                    });
                                }
                            });
                            ui.add(egui::ProgressBar::new(*pct as f32 / 100.0).desired_height(4.0).fill(if state == "PAUSE" { p.warning } else { p.accent }));
                        })
                        .response;
                    if ui.interact(r.rect, egui::Id::unique("o3d_primary_printing"), Sense::click()).on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        act = Some("printer");
                    }
                    if !self.snap.plate_is_empty() {
                        ui.separator();
                    }
                }
                if self.snap.plate_is_empty() {
                    return;
                }
                if let Some(e) = &self.slice_error {
                    let short: String = e.lines().next().unwrap_or("").chars().take(120).collect();
                    ui.add(egui::Label::new(RichText::new(short).size(11.0).color(p.error)).wrap()).on_hover_text(e.as_str());
                } else if let (true, Some(sl)) = (current, &self.snap.last_slice) {
                    let st = &sl.result.stats;
                    ui.label(RichText::new(format!("{} · {:.1} g · {} layers", fmt_duration(st.print_time_s), st.filament_g, st.layers)).size(11.5).monospace().color(p.text));
                    if st.filaments.len() > 1 {
                        // each filament's share, in its colour, and the changes between them
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            for (f, _, g) in &st.filaments {
                                crate::panels::swatch(ui, self.snap.filament_color(*f as u32 + 1), 9.0);
                                ui.label(RichText::new(format!("{:.1} g", g)).size(11.0).monospace().color(p.muted));
                            }
                            ui.label(RichText::new(format!("· {} changes · tower {:.1} g", st.toolchanges, st.tower_g)).size(11.0).color(p.muted));
                        });
                    }
                    if let Some(c) = learned.as_ref() {
                        ui.label(RichText::new(format!("≈ {} on the printer", only3d_studio::timing::fmt_dur(c.expected(st.print_time_s))))
                            .size(11.0)
                            .color(p.muted))
                            .on_hover_text(format!("{} start sequence + printing × {:.2}, learned from your last {} print{}", only3d_studio::timing::fmt_dur(c.overhead_s), c.ratio, c.prints, if c.prints == 1 { "" } else { "s" }));
                    }
                } else if self.snap.last_slice.is_some() && !slicing {
                    ui.label(RichText::new("The plate changed since the last slice").size(11.0).color(p.warning));
                }
                let full = egui::vec2(ui.available_width(), 30.0);
                if slicing {
                    let secs = self.worker.slow_job().filter(|(l, _)| *l == "slice").map(|(_, s)| s).unwrap_or(0.0);
                    ui.vertical_centered_justified(|ui| ui.add_enabled(false, theme::primary_button(format!("Slicing… {secs:.0}s")).min_size(full)));
                    ui.ctx().request_repaint_after(Duration::from_millis(250));
                } else if current {
                    let busy = self.print_modal.is_some();
                    let t = self.keymap.label_for(Action::Print).map(|k| format!("Checks the printer and the plate first; nothing is sent until you confirm  {k}")).unwrap_or_default();
                    if ui.vertical_centered_justified(|ui| ui.add_enabled(!busy, theme::primary_button("Print…").min_size(full))).inner.on_hover_text(t).clicked() {
                        act = Some("print");
                    }
                } else {
                    let t = self.keymap.label_for(Action::Slice).map(|k| format!("Slice the plate for time and grams  {k}")).unwrap_or_default();
                    if ui.vertical_centered_justified(|ui| ui.add(theme::primary_button("Slice plate").min_size(full))).inner.on_hover_text(t).clicked() {
                        act = Some("slice");
                    }
                }
            });
        });
        match act {
            Some("slice") => self.slice(),
            Some("bambu") => self.open_in_bambu(),
            Some("print") => self.start_print(),
            Some("printer") => self.cfg.layout.show(crate::dock::PanelId::Printer),
            _ => {}
        }
        Some(shown.response.rect)
    }

    /// Floating tool strip (top-left of the viewport): manipulation tools and plate actions.
    fn tool_strip(&mut self, ui: &mut egui::Ui, at: Pos2) -> Rect {
        let p = pal();
        let mut out = Rect::from_min_size(at, egui::Vec2::ZERO);
        let tip = |app: &App, a: Action, text: &str| match app.keymap.label_for(a) {
            Some(k) => format!("{text}  {k}"),
            None => text.to_string(),
        };
        let area = egui::Area::new(egui::Id::unique("o3d_tool_strip")).order(egui::Order::Middle).fixed_pos(at).interactable(true);
        area.show(ui.ctx(), |ui| {
            let r = egui::Frame::new().fill(p.elevated).stroke(egui::Stroke::new(1.0, p.border)).corner_radius(6).inner_margin(egui::Margin::same(3)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for (g, icon, a) in [(Gizmo::Select, Icon::Pointer, Action::ToolSelect), (Gizmo::Move, Icon::Move, Action::ToolMove), (Gizmo::Rotate, Icon::Rotate, Action::ToolRotate), (Gizmo::Scale, Icon::Scale, Action::ToolScale), (Gizmo::PushPull, Icon::PushPull, Action::ToolPushPull)] {
                        let t = tip(self, a, g.label());
                        if theme::icon_toggle(ui, icon, &t, self.gizmo == g).clicked() {
                            self.gizmo = g;
                        }
                    }
                    ui.add(egui::Separator::default().vertical().spacing(6.0));
                    let t = tip(self, Action::Arrange, "Arrange plate");
                    if theme::icon_button(ui, Icon::Grid, &t).clicked() {
                        self.run(Action::Arrange);
                    }
                    let t = tip(self, Action::Orient, "Auto-orient / lay flat (selection or all)");
                    if theme::icon_button(ui, Icon::LayFlat, &t).clicked() {
                        self.run(Action::Orient);
                    }
                    let t = tip(self, Action::ToggleOverhangs, "Show overhangs");
                    if theme::icon_toggle(ui, Icon::Overhang, &t, self.overhangs).clicked() {
                        self.run(Action::ToggleOverhangs);
                    }
                });
            });
            out = r.response.rect;
        });
        out
    }

    /// Floating view strip (top-right of the viewport).
    /// Plate tabs at the top centre of the viewport: click to work on a plate, double-click to
    /// rename, right-click for more, + to add one. Returns where they were drawn.
    fn plate_tabs(&mut self, ui: &mut egui::Ui, rect: Rect) -> Rect {
        let p = pal();
        let n = self.snap.project.plate_count();
        let active = self.snap.plate;
        let selection = self.snap.selection.clone();
        let mut call: Option<Value> = None;
        let mut toggle_all = false;
        let area = egui::Area::new(egui::Id::unique("o3d_plate_tabs")).order(egui::Order::Middle).pivot(egui::Align2::CENTER_TOP).fixed_pos(egui::pos2(rect.center().x, rect.min.y + 10.0)).interactable(true);
        let shown = area.show(ui.ctx(), |ui| {
            egui::Frame::new().fill(p.elevated).stroke(egui::Stroke::new(1.0, p.border)).corner_radius(6).inner_margin(egui::Margin::same(3)).show(ui, |ui| {
                ui.set_max_width((rect.width() - 440.0).max(160.0));
                egui::ScrollArea::horizontal().id_salt("plate_tabs").show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        for k in 1..=n {
                            let name = self.snap.project.plate_name(k);
                            let count = self.snap.project.objects.iter().filter(|o| o.plate == k).count();
                            if let Some((rk, text)) = self.plate_rename.as_mut().filter(|(rk, _)| *rk == k) {
                                let r = ui.add(egui::TextEdit::singleline(text).desired_width(110.0).font(egui::TextStyle::Small));
                                r.request_focus();
                                let (enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
                                if enter || (r.lost_focus() && !esc) {
                                    if !text.trim().is_empty() {
                                        call = Some(json!({"action": "rename", "plate": *rk, "name": text.trim()}));
                                    }
                                    self.plate_rename = None;
                                } else if esc {
                                    self.plate_rename = None;
                                }
                                continue;
                            }
                            let on = k == active;
                            let qpos = self.snap.project.queue.iter().position(|q| *q == k);
                            let label = RichText::new(match qpos { Some(i) => format!("{name}  {count}  ⏵{}", i + 1), None => format!("{name}  {count}") }).size(11.5).color(if on { p.text } else { p.muted });
                            let r = ui.add(egui::Button::new(label).fill(if on { p.selected } else { Color32::TRANSPARENT }).stroke(egui::Stroke::NONE).corner_radius(4));
                            let r = r.on_hover_text(format!("Plate {k}: {count} object{} · double-click to rename · right-click for more", if count == 1 { "" } else { "s" }));
                            if r.double_clicked() {
                                self.plate_rename = Some((k, name.clone()));
                            } else if r.clicked() && !on {
                                call = Some(json!({"action": "select", "plate": k}));
                            }
                            r.context_menu(|ui| {
                                if ui.button("Rename…").clicked() {
                                    self.plate_rename = Some((k, name.clone()));
                                    ui.close();
                                }
                                if ui.button("Duplicate plate").clicked() {
                                    call = Some(json!({"action": "duplicate", "plate": k}));
                                    ui.close();
                                }
                                if qpos.is_none() && ui.button("Add to the print queue").on_hover_text("Print it after the others: when a print finishes, only3d says which plate is next").clicked() {
                                    call = Some(json!({"action": "queue", "plate": k}));
                                    ui.close();
                                }
                                if qpos.is_some() && ui.button("Take out of the queue").clicked() {
                                    call = Some(json!({"action": "unqueue", "plate": k}));
                                    ui.close();
                                }
                                if k != active && !selection.is_empty() && ui.button(format!("Move selection here ({})", selection.len())).clicked() {
                                    call = Some(json!({"action": "move", "plate": k, "ids": selection}));
                                    ui.close();
                                }
                                if n > 1 {
                                    ui.separator();
                                    let to = if k == 1 { 2 } else { 1 };
                                    if count > 0 && ui.button(format!("Remove, keep objects on {}", self.snap.project.plate_name(to))).clicked() {
                                        call = Some(json!({"action": "remove", "plate": k, "to": to}));
                                        ui.close();
                                    }
                                    if ui.button(if count > 0 { format!("Remove with its {count} object(s)") } else { "Remove plate".into() }).clicked() {
                                        call = Some(json!({"action": "remove", "plate": k}));
                                        ui.close();
                                    }
                                }
                            });
                        }
                        if theme::icon_button(ui, Icon::Plus, "Add a plate").clicked() {
                            call = Some(json!({"action": "add"}));
                        }
                        if n > 1 {
                            let tip = self.keymap.label_for(Action::ToggleAllPlates).map(|k| format!("Show all plates side by side  {k}")).unwrap_or_else(|| "Show all plates side by side".into());
                            if theme::icon_toggle(ui, Icon::Layers, &tip, self.cfg.show_all_plates).clicked() {
                                toggle_all = true;
                            }
                        }
                    });
                });
            });
        });
        if let Some(args) = call {
            self.worker.call("plate", args, Kind::Plain);
        }
        if toggle_all {
            self.run(Action::ToggleAllPlates);
        }
        shown.response.rect
    }

    fn view_strip(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let p = pal();
        let views = [(ViewPreset::Iso, Action::ViewIso), (ViewPreset::Top, Action::ViewTop), (ViewPreset::Front, Action::ViewFront), (ViewPreset::Left, Action::ViewLeft), (ViewPreset::Right, Action::ViewRight)];
        let area = egui::Area::new(egui::Id::unique("o3d_view_strip")).order(egui::Order::Middle).anchor(egui::Align2::RIGHT_TOP, egui::vec2(-(ui.ctx().content_rect().max.x - rect.max.x) - 10.0, rect.min.y + 10.0 - ui.ctx().content_rect().min.y)).interactable(true);
        area.show(ui.ctx(), |ui| {
            egui::Frame::new().fill(p.elevated).stroke(egui::Stroke::new(1.0, p.border)).corner_radius(6).inner_margin(egui::Margin::same(3)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 1.0;
                    for (v, a) in views {
                        let tip = self.keymap.label_for(a).map(|k| format!("{} view  {k}", v.label())).unwrap_or_else(|| format!("{} view", v.label()));
                        if ui.add(theme::ghost_button(RichText::new(v.label()).size(11.5).color(p.muted))).on_hover_text(tip).clicked() {
                            self.set_view(v);
                        }
                    }
                    let cam_tip = match self.cfg.camera_xy {
                        Some(c) => format!("Show where the printer's camera is (x {:.0}, y {:.0} mm)", c[0], c[1]),
                        None => "Show where the printer's camera is (set its position in Preferences first)".into(),
                    };
                    if theme::icon_toggle(ui, Icon::Camera, &cam_tip, self.cfg.show_camera).clicked() {
                        self.cfg.show_camera = !self.cfg.show_camera;
                        if self.cfg.show_camera && self.cfg.camera_xy.is_none() {
                            self.toast("Measure the camera's position from the plate's front-left corner and enter it in Preferences → Viewport".into(), false);
                            self.show_prefs = true;
                        }
                    }
                    let tip = self.keymap.label_for(Action::FrameSelection).map(|k| format!("Frame selection  {k}")).unwrap_or_else(|| "Frame selection".into());
                    if theme::icon_button(ui, Icon::Zen, &tip).clicked() {
                        self.frame_selection(self.vp_aspect);
                    }
                });
            });
        });
    }

    fn toolpath_overlay(&mut self, ui: &mut egui::Ui, rect: Rect, top_y: f32, top_center: f32) {
        let p = pal();
        let Some((_, data)) = self.toolpaths.clone() else {
            let msg = if self.tp_building.is_some() {
                "Preparing toolpaths…"
            } else if self.slicing() {
                "Slicing…"
            } else {
                "Slice the plate to preview toolpaths."
            };
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(14.0), p.muted);
            return;
        };
        let n = data.layers();
        if n == 0 {
            return;
        }
        // legend
        let nfil = if self.preview_by_filament() { self.snap.last_slice.as_ref().map(|s| s.result.stats.filaments.len()).unwrap_or(0) } else { 0 };
        let legend_rect = Rect::from_min_size(egui::pos2(rect.min.x + 10.0, top_y), egui::vec2(if nfil > 0 { 270.0 } else { 206.0 }, 360.0 + 22.0 * nfil as f32));
        let stats = self.snap.last_slice.as_ref().map(|s| s.result.stats.feature_time_s.clone()).unwrap_or_default();
        ui.scope_builder(egui::UiBuilder::new().max_rect(legend_rect), |ui| {
            egui::Frame::new().fill(p.elevated).stroke(egui::Stroke::new(1.0, p.border)).corner_radius(6).inner_margin(10).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                // colour by: features, or the filaments that print them
                let by_fil = self.preview_by_filament();
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for (on, label) in [(false, "Features"), (true, "Filaments")] {
                        if ui.selectable_label(by_fil == on, RichText::new(label).size(11.5)).clicked() && by_fil != on {
                            self.color_by_filament = Some(on);
                        }
                    }
                });
                if by_fil {
                    let fils = self.snap.last_slice.as_ref().map(|s| s.result.stats.filaments.clone()).unwrap_or_default();
                    for (f, mm, g) in fils {
                        ui.horizontal(|ui| {
                            crate::panels::swatch(ui, self.snap.filament_color(f as u32 + 1), 11.0);
                            let name: String = self.snap.project.filaments.get(f).map(|x| crate::settings_ui::short(&x.preset).trim_start_matches("Bambu ").to_string()).unwrap_or_default();
                            let name = if name.chars().count() > 16 { format!("{}…", name.chars().take(15).collect::<String>()) } else { name };
                            ui.label(RichText::new(format!("{} · {name}", f + 1)).size(12.0).color(p.text)).on_hover_text(self.snap.project.filaments.get(f).map(|x| x.preset.clone()).unwrap_or_default());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(RichText::new(format!("{g:.1} g · {:.1} m", mm / 1000.0)).size(11.0).monospace().color(p.muted));
                            });
                        });
                    }
                    ui.add_space(4.0);
                }
                theme::section_label(ui, "Features");
                for (i, f) in only3d_slice::Feature::ALL.iter().enumerate() {
                    let c = f.color();
                    let on = self.feature_mask >> i & 1 == 1;
                    let t = stats.iter().find(|(name, _)| name == f.bambu_name()).map(|(_, s)| *s).unwrap_or(0.0);
                    ui.horizontal(|ui| {
                        let (r, resp) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), Sense::click());
                        let col = if by_fil { p.muted } else { Color32::from_rgb(c[0], c[1], c[2]) };
                        if on {
                            ui.painter().rect_filled(r.shrink(1.0), 2.0, col);
                        } else {
                            ui.painter().rect_stroke(r.shrink(1.5), 2.0, egui::Stroke::new(1.3, col), egui::StrokeKind::Inside);
                        }
                        let label = ui.add(egui::Label::new(RichText::new(f.bambu_name()).color(if on { p.text } else { p.muted }).size(12.0)).sense(Sense::click()));
                        if t > 0.5 {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(RichText::new(fmt_duration(t)).size(11.0).monospace().color(p.muted));
                            });
                        }
                        if resp.clicked() || label.clicked() {
                            self.feature_mask ^= 1 << i;
                        }
                    });
                }
            });
        });

        // layer range: vertical slider on the right for the top layer, small one for the bottom
        let bottom = self.primary_rect.map(|r| r.min.y - 10.0).unwrap_or(rect.max.y - 70.0).min(rect.max.y - 70.0);
        let sr = Rect::from_min_max(egui::pos2(rect.max.x - 70.0, rect.min.y + 50.0), egui::pos2(rect.max.x - 10.0, bottom.max(rect.min.y + 220.0)));
        ui.scope_builder(egui::UiBuilder::new().max_rect(sr), |ui| {
            egui::Frame::new().fill(p.elevated).stroke(egui::Stroke::new(1.0, p.border)).corner_radius(6).inner_margin(egui::Margin::symmetric(6, 8)).show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    let z = data.layer_z.get(self.layer_hi).copied().unwrap_or(0.0);
                    ui.label(RichText::new(format!("{}", self.layer_hi + 1)).monospace().size(13.0).color(p.text));
                    ui.label(RichText::new(format!("{z:.2}")).size(10.5).monospace().color(p.muted));
                    let h = (sr.height() - 104.0).max(60.0);
                    ui.spacing_mut().slider_width = h;
                    let mut hi = self.layer_hi;
                    ui.add(egui::Slider::new(&mut hi, 0..=n - 1).vertical().show_value(false));
                    if hi != self.layer_hi {
                        self.layer_hi = hi.max(if self.one_layer { 0 } else { self.layer_lo });
                    }
                    ui.label(RichText::new(format!("of {n}")).size(10.5).color(p.muted));
                    let t = self.keymap.label_for(Action::OneLayer).map(|k| format!("One-layer mode  {k}")).unwrap_or_else(|| "One-layer mode".into());
                    if theme::icon_toggle(ui, Icon::Layers, &t, self.one_layer).clicked() {
                        self.ui_action(Action::OneLayer);
                    }
                });
            });
        });
        let br = Rect::from_min_size(egui::pos2(rect.center().x - 210.0, rect.max.y - 50.0), egui::vec2(420.0, 40.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(br), |ui| {
            egui::Frame::new().fill(p.elevated).stroke(egui::Stroke::new(1.0, p.border)).corner_radius(6).inner_margin(egui::Margin::symmetric(10, 5)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("From layer").size(12.0).color(p.muted));
                    ui.spacing_mut().slider_width = 200.0;
                    let mut lo = self.layer_lo;
                    ui.add_enabled(!self.one_layer, egui::Slider::new(&mut lo, 0..=n - 1).custom_formatter(|v, _| format!("{}", v as usize + 1)));
                    if !self.one_layer {
                        self.layer_lo = lo.min(self.layer_hi);
                    }
                    if ui.small_button("All").clicked() {
                        self.one_layer = false;
                        self.layer_lo = 0;
                        self.layer_hi = n - 1;
                    }
                });
            });
        });
        if !self.snap.slice_current {
            let painter = ui.painter_at(rect);
            let g = painter.layout_no_wrap("The model changed since this slice — slice again (⌘R)".into(), egui::FontId::proportional(12.0), p.warning);
            let r = Rect::from_center_size(egui::pos2(rect.center().x, top_center + 13.0), g.size() + egui::vec2(24.0, 10.0));
            painter.rect(r, 4.0, theme::soft(p.warning), egui::Stroke::new(1.0, p.warning.gamma_multiply(0.6)), egui::StrokeKind::Inside);
            painter.galley(r.min + egui::vec2(12.0, 5.0), g, p.warning);
        }
    }

    // ------------------------------------------------------------------------------------------
    // screenshot self-test

    fn screenshot_logic(&mut self, ctx: &egui::Context) {
        let Some(shot) = &mut self.shot else { return };
        shot.frames += 1;
        if shot.frames == shot.min_frames.saturating_sub(120) {
            self.frame_times.clear();
        }
        ctx.request_repaint();
        let settled = self.worker.inflight.is_empty() && self.tp_building.is_none() && self.snap.revision != u64::MAX && self.inject.is_empty() && self.held.is_none();
        if shot.spin {
            self.cam.yaw += 0.01;
        }
        if !shot.requested && shot.frames > shot.min_frames && shot.started.elapsed() >= shot.min_time && settled {
            let slot = shot.image.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
                if let Ok(mut s) = slot.lock() {
                    *s = Some(img);
                }
            })));
            shot.requested = true;
        }
        let img = shot.image.lock().ok().and_then(|mut s| s.take());
        if let Some(img) = img {
            let path = shot.path.clone();
            match save_png(&path, &img) {
                Ok(()) => eprintln!("only3d-gui: screenshot saved to {}", path.display()),
                Err(e) => eprintln!("only3d-gui: screenshot failed: {e}"),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if shot.started.elapsed() > shot.min_time + Duration::from_secs(30) {
            eprintln!("only3d-gui: screenshot timed out");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn window_title(&self) -> String {
        format!("{} — only3d", self.project_name())
    }

    pub fn project_name(&self) -> String {
        self.title.clone().unwrap_or_else(|| {
            self.snap.project_path.as_ref().and_then(|p| p.file_name()).map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| if self.snap.project.name.is_empty() { "untitled".into() } else { self.snap.project.name.clone() })
        })
    }
}

impl Palette {
    pub fn overhang_3d_color(&self) -> Color32 {
        Color32::from_rgb(self.overhang_3d[0], self.overhang_3d[1], self.overhang_3d[2])
    }
}

/// Synthetic input for the screenshot self-test: `ONLY3D_GUI_KEYS=cmd-a,r,right` presses
/// chords, `ONLY3D_GUI_CLICK=x,y` clicks and `ONLY3D_GUI_DRAG=x0,y0,x1,y1` drags the
/// primary button between two window points.
fn synthetic_input() -> std::collections::VecDeque<Vec<egui::Event>> {
    let mut q = std::collections::VecDeque::new();
    if let Some(c) = env("ONLY3D_GUI_CLICK") {
        let v: Vec<f32> = c.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        if v.len() == 2 {
            let pos = egui::pos2(v[0], v[1]);
            let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            q.push_back(vec![egui::Event::PointerMoved(pos)]);
            q.push_back(vec![button(true)]);
            q.push_back(vec![button(false)]);
        }
    }
    if let Some(keys) = env("ONLY3D_GUI_KEYS") {
        for c in keys.split(',').filter_map(crate::keymap::Chord::parse) {
            let modifiers = c.egui_modifiers();
            q.push_back(vec![egui::Event::Key { key: c.key, physical_key: Some(c.key), pressed: true, repeat: false, modifiers }, egui::Event::Key { key: c.key, physical_key: Some(c.key), pressed: false, repeat: false, modifiers }]);
        }
    }
    // the pointer resting on a point (hover cards and tooltips)
    if let Some(c) = env("ONLY3D_GUI_HOVER") {
        let v: Vec<f32> = c.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        if v.len() == 2 {
            for _ in 0..4 {
                q.push_back(vec![egui::Event::PointerMoved(egui::pos2(v[0], v[1]))]);
            }
        }
    }
    // a right-click (context menus), after the keys
    if let Some(c) = env("ONLY3D_GUI_RCLICK") {
        let v: Vec<f32> = c.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        if v.len() == 2 {
            let pos = egui::pos2(v[0], v[1]);
            let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE };
            for _ in 0..4 {
                q.push_back(vec![egui::Event::PointerMoved(pos)]);
            }
            q.push_back(vec![button(true)]);
            q.push_back(vec![button(false)]);
        }
    }
    // a click after the keys (e.g. on the activity row a chord just produced); the pointer stays
    // there, so the screenshot shows the hover state too
    // several clicks in turn: `x,y;x,y`
    for c in env("ONLY3D_GUI_THEN_CLICK").unwrap_or_default().split(';') {
        let v: Vec<f32> = c.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        if v.len() == 2 {
            let pos = egui::pos2(v[0], v[1]);
            let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            for _ in 0..6 {
                q.push_back(vec![egui::Event::PointerMoved(pos)]);
            }
            q.push_back(vec![button(true)]);
            q.push_back(vec![button(false)]);
        }
    }
    if let Some(d) = env("ONLY3D_GUI_DRAG") {
        let v: Vec<f32> = d.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        if v.len() == 4 {
            let (a, b) = (egui::pos2(v[0], v[1]), egui::pos2(v[2], v[3]));
            let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            q.push_back(vec![egui::Event::PointerMoved(a)]);
            q.push_back(vec![button(a, true)]);
            for k in 1..=16 {
                q.push_back(vec![egui::Event::PointerMoved(a + (b - a) * (k as f32 / 16.0))]);
            }
            // ONLY3D_GUI_DRAG_HOLD=1 keeps the button down, so the screenshot shows the drag
            if env("ONLY3D_GUI_DRAG_HOLD").is_none() {
                q.push_back(vec![button(b, false)]);
                q.push_back(vec![egui::Event::PointerMoved(b + egui::vec2(0.0, 40.0))]);
            }
        }
    }
    q
}

/// Objects copied to the system clipboard by only3d (`{"only3d_objects": [...]}`), or
/// a bare object / array of objects.
pub fn parse_clipboard(text: &str) -> Option<Vec<Object>> {
    let v: Value = serde_json::from_str(text.trim()).ok()?;
    let list = match v {
        Value::Object(ref m) if m.contains_key("only3d_objects") => m.get("only3d_objects").cloned()?,
        Value::Array(_) => v,
        Value::Object(_) => Value::Array(vec![v]),
        _ => return None,
    };
    let objs: Vec<Object> = serde_json::from_value(list).ok()?;
    (!objs.is_empty()).then_some(objs)
}

/// How snugly a plate fills the view: tight in a wide view, where the bounding sphere is
/// generous; looser in a tall, narrow one (side by side), where it would crop the corners.
fn plate_margin(aspect: f32) -> f32 {
    if aspect < 1.0 {
        1.06
    } else {
        0.93
    }
}

pub fn tool_error_text(tool: &str, e: &only3d_studio::ToolError) -> String {
    if e.code == "unknown_tool" {
        format!("'{tool}' is not available in this build of only3d")
    } else {
        format!("{tool} failed — {}: {}", e.code, e.message)
    }
}

fn save_png(path: &std::path::Path, img: &egui::ColorImage) -> anyhow::Result<()> {
    let [w, h] = img.size;
    let mut bytes = Vec::with_capacity(w * h * 4);
    for p in &img.pixels {
        let [r, g, b, _] = p.to_srgba_unmultiplied();
        bytes.extend_from_slice(&[r, g, b, 255]);
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(f, w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header()?;
    wr.write_image_data(&bytes)?;
    Ok(())
}

pub fn fmt_duration(s: f64) -> String {
    let m = (s / 60.0).round() as u64;
    if m >= 60 {
        format!("{}h {:02}m", m / 60, m % 60)
    } else if m >= 1 {
        format!("{m}m")
    } else {
        format!("{:.0}s", s)
    }
}

pub fn short_machine(m: &str) -> String {
    m.trim_start_matches("Bambu Lab ").split(" 0.").next().unwrap_or(m).to_string()
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        raw_input.events.append(&mut self.menu_keys);
        self.intercept_keys(ctx, raw_input);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.frames += 1;
        let now = Instant::now();
        if let Some(t) = self.last_frame.replace(now) {
            self.frame_times.push_back((now - t).as_secs_f32() * 1000.0);
            if self.frame_times.len() > 120 {
                self.frame_times.pop_front();
            }
        }
        self.sync_theme(&ctx);
        self.handle_done();
        self.poll_printer();
        self.refresh();
        for (title, body, failed) in std::mem::take(&mut self.notes_pending) {
            self.notify(&title, &body, failed);
        }
        self.poll_toolpaths();
        self.maybe_refresh_effective();
        self.maybe_refresh_catalog();
        self.active_keys = std::mem::take(&mut self.active_now);
        self.drops(&ctx);
        for a in std::mem::take(&mut self.pending) {
            self.run(a);
        }
        self.menu_frame(&ctx);
        if let Some(t) = self.copy_out.take() {
            ctx.copy_text(t);
        }

        let title = self.window_title();
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }

        self.chrome(ui);
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| self.center(ui));
        self.overlays(&ctx);

        self.pump_throttle();
        self.save_config();
        self.screenshot_logic(&ctx);
        let busy = !self.worker.inflight.is_empty() || self.tp_building.is_some() || self.drag.is_some();
        ctx.request_repaint_after(if busy { Duration::from_millis(60) } else { Duration::from_millis(250) });
    }

    fn on_exit(&mut self) {
        if self.persist && self.cfg != self.cfg_saved {
            let _ = self.cfg.save();
        }
    }
}
