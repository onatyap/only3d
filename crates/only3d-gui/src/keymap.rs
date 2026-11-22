//! Actions, key chords and keymaps.
//!
//! Every UI command is an [`Action`] with a stable id ("edit.select_all"). A [`Keymap`] binds
//! chords to actions, optionally only in the Prepare (model) or Preview (toolpath) view. Two
//! presets ship: **only3d** (macOS-like, the default) and **Bambu Studio** (from Bambu
//! Studio's keyboard-shortcut list). User overrides live in `~/.only3d/keymap.json`:
//!
//! ```json
//! {
//!   "bindings": { "cmd-d": "edit.duplicate", "cmd-k": null },
//!   "prepare":  { "g": "tool.move" },
//!   "preview":  { "j": "preview.layer_down", "k": "preview.layer_up" }
//! }
//! ```
//!
//! Keys are chords like `cmd-shift-p`, `alt-1`, `shift-up`, `?`, `escape` (modifiers: `cmd`,
//! `ctrl`, `alt`/`opt`, `shift`; `-` or `+` between parts). `null` removes a preset binding.
//! `bindings` applies everywhere; `prepare` / `preview` only in that view (and win over
//! `bindings`). Action ids are listed in the Shortcuts sheet (`?`) and the command palette.

use eframe::egui::{Key, Modifiers};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::dock::{PanelId, Side};
use crate::theme::ThemeChoice;

const MAC: bool = cfg!(target_os = "macos");

/// Modifier names for hints written by hand: ⌘ ⌥ ⇧ on macOS, Ctrl Alt Shift elsewhere (egui's
/// "command" modifier is Ctrl off macOS).
pub const CMD: &str = if MAC { "⌘" } else { "Ctrl" };
pub const ALT: &str = if MAC { "⌥" } else { "Alt" };
pub const SHIFT: &str = if MAC { "⇧" } else { "Shift" };

/// A command-key shortcut for a hint: "⌘I" on macOS, "Ctrl+I" elsewhere.
pub fn cmd_key(key: &str) -> String {
    if MAC {
        format!("⌘{key}")
    } else {
        format!("Ctrl+{key}")
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Mods {
    /// ⌘ on macOS, Ctrl elsewhere.
    pub cmd: bool,
    /// The Control key on macOS (folded into `cmd` elsewhere).
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    pub fn from_egui(m: Modifiers) -> Self {
        Mods { cmd: m.command, ctrl: MAC && m.ctrl, alt: m.alt, shift: m.shift }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub key: Key,
    pub mods: Mods,
}

/// Keys typed with Shift on a US layout: Shift is implied, so it is ignored when matching.
fn shifted_punct(k: Key) -> bool {
    matches!(k, Key::Questionmark | Key::Exclamationmark | Key::Colon | Key::Plus | Key::Pipe | Key::OpenCurlyBracket | Key::CloseCurlyBracket)
}

fn alnum(k: Key) -> bool {
    let n = k.name();
    n.len() == 1 && n.chars().all(|c| c.is_ascii_alphanumeric())
}

fn key_from_config(name: &str) -> Option<Key> {
    let l = name.to_ascii_lowercase();
    let k = match l.as_str() {
        "esc" | "escape" => Key::Escape,
        "del" | "delete" => Key::Delete,
        "backspace" | "bksp" => Key::Backspace,
        "enter" | "return" => Key::Enter,
        "tab" => Key::Tab,
        "space" => Key::Space,
        "up" => Key::ArrowUp,
        "down" => Key::ArrowDown,
        "left" => Key::ArrowLeft,
        "right" => Key::ArrowRight,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" | "pgup" => Key::PageUp,
        "pagedown" | "pgdn" => Key::PageDown,
        "insert" => Key::Insert,
        _ => {
            if l.chars().count() == 1 {
                return Key::from_name(&l.to_ascii_uppercase()).or_else(|| Key::from_name(name));
            }
            let mut cap = l.clone();
            if let Some(f) = cap.get_mut(0..1) {
                f.make_ascii_uppercase();
            }
            return Key::from_name(name).or_else(|| Key::from_name(&cap)).or_else(|| Key::from_name(&l.to_ascii_uppercase()));
        }
    };
    Some(k)
}

fn key_config_name(k: Key) -> String {
    match k {
        Key::Escape => "escape".into(),
        Key::Delete => "delete".into(),
        Key::Backspace => "backspace".into(),
        Key::Enter => "enter".into(),
        Key::Tab => "tab".into(),
        Key::Space => "space".into(),
        Key::ArrowUp => "up".into(),
        Key::ArrowDown => "down".into(),
        Key::ArrowLeft => "left".into(),
        Key::ArrowRight => "right".into(),
        Key::Home => "home".into(),
        Key::End => "end".into(),
        Key::PageUp => "pageup".into(),
        Key::PageDown => "pagedown".into(),
        Key::Minus => "-".into(),
        k => {
            let s = k.symbol_or_name();
            if s.chars().count() == 1 {
                s.to_ascii_lowercase()
            } else {
                k.name().to_ascii_lowercase()
            }
        }
    }
}

fn key_symbol(k: Key) -> String {
    match k {
        Key::ArrowUp => "↑".into(),
        Key::ArrowDown => "↓".into(),
        Key::ArrowLeft => "←".into(),
        Key::ArrowRight => "→".into(),
        Key::Escape => "Esc".into(),
        Key::Delete => if MAC { "⌦" } else { "Del" }.into(),
        Key::Backspace => if MAC { "⌫" } else { "Backspace" }.into(),
        Key::Enter => if MAC { "⏎" } else { "Enter" }.into(),
        Key::Tab => "Tab".into(),
        Key::Space => "Space".into(),
        Key::PageUp => "PgUp".into(),
        Key::PageDown => "PgDn".into(),
        k => k.symbol_or_name().to_string(),
    }
}

impl Chord {
    pub const fn new(key: Key, mods: Mods) -> Self {
        Chord { key, mods }
    }

    /// Parse `cmd-shift-p`, `Cmd+Shift+P`, `alt-1`, `shift-?`, `cmd--`, `escape`…
    pub fn parse(s: &str) -> Option<Chord> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let mut mods = Mods::default();
        let mut rest = s;
        'outer: loop {
            for (name, which) in [("cmd", 0), ("command", 0), ("super", 0), ("meta", 0), ("ctrl", 1), ("control", 1), ("alt", 2), ("opt", 2), ("option", 2), ("shift", 3)] {
                if rest.len() > name.len() + 1 && rest[..name.len()].eq_ignore_ascii_case(name) && matches!(rest.as_bytes()[name.len()], b'-' | b'+') {
                    match which {
                        0 => mods.cmd = true,
                        1 if MAC => mods.ctrl = true,
                        1 => mods.cmd = true,
                        2 => mods.alt = true,
                        _ => mods.shift = true,
                    }
                    rest = &rest[name.len() + 1..];
                    continue 'outer;
                }
            }
            break;
        }
        let key = key_from_config(rest)?;
        Some(Chord { key, mods })
    }

    /// Canonical config form, e.g. `cmd-shift-p`.
    pub fn config(&self) -> String {
        let mut s = String::new();
        if self.mods.ctrl {
            s.push_str("ctrl-");
        }
        if self.mods.alt {
            s.push_str("alt-");
        }
        if self.mods.shift {
            s.push_str("shift-");
        }
        if self.mods.cmd {
            s.push_str(if MAC { "cmd-" } else { "ctrl-" });
        }
        s.push_str(&key_config_name(self.key));
        s
    }

    /// Display form: `⌃⌥⇧⌘P` on macOS, `Ctrl+Shift+P` elsewhere.
    pub fn label(&self) -> String {
        let key = key_symbol(self.key);
        let shift = self.mods.shift && !shifted_punct(self.key);
        if MAC {
            let mut s = String::new();
            if self.mods.ctrl {
                s.push('⌃');
            }
            if self.mods.alt {
                s.push('⌥');
            }
            if shift {
                s.push('⇧');
            }
            if self.mods.cmd {
                s.push('⌘');
            }
            s + &key
        } else {
            let mut parts: Vec<String> = Vec::new();
            if self.mods.cmd {
                parts.push("Ctrl".into());
            }
            if self.mods.alt {
                parts.push("Alt".into());
            }
            if shift {
                parts.push("Shift".into());
            }
            parts.push(key);
            parts.join("+")
        }
    }

    /// egui modifiers that produce this chord (for injecting synthetic key presses).
    /// Held with ⌘ (or ⌃ on macOS): a chord no text field types.
    pub fn is_command(&self) -> bool {
        self.mods.cmd || self.mods.ctrl
    }

    pub fn egui_modifiers(&self) -> Modifiers {
        let m = self.mods;
        Modifiers { alt: m.alt, ctrl: if MAC { m.ctrl } else { m.cmd || m.ctrl }, shift: m.shift, mac_cmd: MAC && m.cmd, command: m.cmd }
    }

    /// Does a key press (logical key, physical key, modifiers) trigger this chord?
    pub fn matches(&self, key: Key, physical: Option<Key>, mods: Mods) -> bool {
        // Shift+1 arrives as "!" on a US layout: fall back to the physical key for non-letters
        let key_ok = key == self.key || (physical == Some(self.key) && !alnum(key));
        if !key_ok {
            return false;
        }
        let m = self.mods;
        if shifted_punct(self.key) {
            m.cmd == mods.cmd && m.ctrl == mods.ctrl && m.alt == mods.alt
        } else {
            m == mods
        }
    }
}

/// Where a binding applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum When {
    Always,
    /// The model view (Prepare).
    Prepare,
    /// The toolpath view (Preview).
    Preview,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    New,
    Open,
    Save,
    SaveAs,
    Import,
    ExportPrint,
    ExportStl,
    Export3mf,
    ExportScad,
    Undo,
    Redo,
    SelectAll,
    Deselect,
    Delete,
    DeleteAll,
    Duplicate,
    Copy,
    Cut,
    Paste,
    Rename,
    ToggleHidden,
    SetFilament(u8),
    Arrange,
    Orient,
    PlaceOnFace,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    MoveLeftBig,
    MoveRightBig,
    MoveUpBig,
    MoveDownBig,
    RotateCcw,
    RotateCw,
    ScaleUp,
    ScaleDown,
    ToolSelect,
    ToolMove,
    ToolRotate,
    ToolScale,
    ToolPushPull,
    ViewHome,
    ViewIso,
    ViewTop,
    ViewBottom,
    ViewFront,
    ViewBack,
    ViewLeft,
    ViewRight,
    FrameSelection,
    TogglePreview,
    Prepare,
    Preview,
    ToggleOverhangs,
    ToggleAllPlates,
    LayerUp,
    LayerDown,
    LayerUpFast,
    LayerDownFast,
    OneLayer,
    Slice,
    Print,
    Palette,
    Preferences,
    Shortcuts,
    ToggleDock(Side),
    Zen,
    ResetLayout,
    ToggleTheme,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ShowPanel(PanelId),
    /// An Efficient Elements-style arrange action (align, dock, distribute…).
    Op(crate::arrange_ops::ArrangeOp),
    /// Add one of the starter shapes (`app::PRIMITIVES`, by index).
    AddShape(u8),
    /// The 3D plate and the previewed file side by side.
    SideBySide,
    /// The Save panel set to export.
    ExportAny,
    ExportSelection,
    CloseTab,
    ReopenTab,
    NextTab,
    PrevTab,
    CloseWindow,
    /// Quit only3d: closes the window, which asks first when the project has unsaved changes.
    Quit,
    OpenFolder,
    CloseFolder,
    ExportBambu,
    ShowInFinder,
    Versions,
    ToggleLock,
    RepairMesh,
    ObjectSettings,
    PlateNew,
    PlateDuplicate,
    PlateRename,
    PlateDelete,
    PlateNext,
    PlatePrev,
    CheckPlate,
    FitCoupon,
    Theme(ThemeChoice),
    PrinterCamera,
    PrinterPause,
    PrinterResume,
    PrinterStop,
    SkipObjects,
    LoadFilament,
    UnloadFilament,
    SyncSpools,
    PrinterFiles,
    PrinterTimelapses,
    AddPrinter,
    ConnectAgent,
    OpenGuide,
    ToolsReference,
    ShowDataFolder,
    ReportIssue,
}

impl Action {
    pub fn all() -> Vec<Action> {
        use Action::*;
        let mut v = vec![
            New, Open, Save, SaveAs, Import, ExportPrint, ExportStl, Export3mf, ExportScad, Undo, Redo, SelectAll, Deselect, Delete, DeleteAll, Duplicate, Copy, Cut, Paste, Rename, ToggleHidden,
        ];
        v.extend((1..=9).map(SetFilament));
        v.extend(crate::arrange_ops::ArrangeOp::ALL.map(Op));
        v.extend([
            Arrange,
            Orient,
            PlaceOnFace,
            MoveLeft,
            MoveRight,
            MoveUp,
            MoveDown,
            MoveLeftBig,
            MoveRightBig,
            MoveUpBig,
            MoveDownBig,
            RotateCcw,
            RotateCw,
            ScaleUp,
            ScaleDown,
            ToolSelect,
            ToolMove,
            ToolRotate,
            ToolScale,
            ToolPushPull,
            ViewHome,
            ViewIso,
            ViewTop,
            ViewBottom,
            ViewFront,
            ViewBack,
            ViewLeft,
            ViewRight,
            FrameSelection,
            TogglePreview,
            Prepare,
            Preview,
            ToggleOverhangs,
            ToggleAllPlates,
            LayerUp,
            LayerDown,
            LayerUpFast,
            LayerDownFast,
            OneLayer,
            Slice,
            Print,
            Palette,
            Preferences,
            Shortcuts,
            ToggleDock(Side::Left),
            ToggleDock(Side::Right),
            ToggleDock(Side::Bottom),
            Zen,
            ResetLayout,
            ToggleTheme,
            ZoomIn,
            ZoomOut,
            ZoomReset,
        ]);
        v.extend(PanelId::ALL.into_iter().map(ShowPanel));
        v.extend((0..crate::app::PRIMITIVES.len() as u8).map(AddShape));
        v.extend([
            SideBySide,
            ExportAny,
            ExportSelection,
            CloseTab,
            ReopenTab,
            NextTab,
            PrevTab,
            CloseWindow,
            Quit,
            OpenFolder,
            CloseFolder,
            ExportBambu,
            ShowInFinder,
            Versions,
            ToggleLock,
            RepairMesh,
            ObjectSettings,
            PlateNew,
            PlateDuplicate,
            PlateRename,
            PlateDelete,
            PlateNext,
            PlatePrev,
            CheckPlate,
            FitCoupon,
        ]);
        v.extend(ThemeChoice::ALL.map(Theme));
        v.extend([PrinterCamera, PrinterPause, PrinterResume, PrinterStop, SkipObjects, LoadFilament, UnloadFilament, SyncSpools, PrinterFiles, PrinterTimelapses, AddPrinter, ConnectAgent, OpenGuide, ToolsReference, ShowDataFolder, ReportIssue]);
        v
    }

    pub fn id(self) -> String {
        use Action::*;
        let s = match self {
            New => "file.new",
            Open => "file.open",
            Save => "file.save",
            SaveAs => "file.save_as",
            Import => "file.import",
            ExportPrint => "file.export_print",
            ExportStl => "file.export_stl",
            Export3mf => "file.export_3mf",
            ExportScad => "file.export_scad",
            Undo => "edit.undo",
            Redo => "edit.redo",
            SelectAll => "edit.select_all",
            Deselect => "edit.deselect",
            Delete => "edit.delete",
            DeleteAll => "edit.delete_all",
            Duplicate => "edit.duplicate",
            Copy => "edit.copy",
            Cut => "edit.cut",
            Paste => "edit.paste",
            Rename => "edit.rename",
            ToggleHidden => "edit.toggle_hidden",
            SetFilament(n) => return format!("edit.filament_{n}"),
            Op(o) => o.id(),
            Arrange => "object.arrange",
            Orient => "object.orient",
            PlaceOnFace => "object.place_on_face",
            MoveLeft => "object.move_left",
            MoveRight => "object.move_right",
            MoveUp => "object.move_up",
            MoveDown => "object.move_down",
            MoveLeftBig => "object.move_left_10",
            MoveRightBig => "object.move_right_10",
            MoveUpBig => "object.move_up_10",
            MoveDownBig => "object.move_down_10",
            RotateCcw => "object.rotate_ccw",
            RotateCw => "object.rotate_cw",
            ScaleUp => "object.scale_up",
            ScaleDown => "object.scale_down",
            ToolSelect => "tool.select",
            ToolMove => "tool.move",
            ToolRotate => "tool.rotate",
            ToolScale => "tool.scale",
            ToolPushPull => "tool.push_pull",
            ViewHome => "view.home",
            ViewIso => "view.iso",
            ViewTop => "view.top",
            ViewBottom => "view.bottom",
            ViewFront => "view.front",
            ViewBack => "view.back",
            ViewLeft => "view.left",
            ViewRight => "view.right",
            FrameSelection => "view.frame_selection",
            TogglePreview => "view.toggle_preview",
            Prepare => "view.prepare",
            Preview => "view.preview",
            ToggleOverhangs => "view.overhangs",
            ToggleAllPlates => "view.all_plates",
            LayerUp => "preview.layer_up",
            LayerDown => "preview.layer_down",
            LayerUpFast => "preview.layer_up_5",
            LayerDownFast => "preview.layer_down_5",
            OneLayer => "preview.one_layer",
            Slice => "slice.slice",
            Print => "slice.print",
            Palette => "app.command_palette",
            Preferences => "app.preferences",
            Shortcuts => "app.shortcuts",
            ToggleDock(Side::Left) => "dock.toggle_left",
            ToggleDock(Side::Right) => "dock.toggle_right",
            ToggleDock(Side::Bottom) => "dock.toggle_bottom",
            Zen => "app.zen_mode",
            ResetLayout => "app.reset_layout",
            ToggleTheme => "app.toggle_theme",
            ZoomIn => "app.zoom_in",
            ZoomOut => "app.zoom_out",
            ZoomReset => "app.zoom_reset",
            ShowPanel(p) => return format!("panel.{}", p.id()),
            AddShape(i) => return format!("object.add_{}", shape_slug(i)),
            SideBySide => "view.side_by_side",
            ExportAny => "file.export",
            CloseTab => "view.close_tab",
            ReopenTab => "view.reopen_tab",
            NextTab => "view.next_tab",
            PrevTab => "view.previous_tab",
            CloseWindow => "app.close_window",
            Quit => "app.quit",
            ExportSelection => "file.export_selection",
            OpenFolder => "file.open_folder",
            CloseFolder => "file.close_folder",
            ExportBambu => "file.export_bambu",
            ShowInFinder => "file.show_in_finder",
            Versions => "file.versions",
            ToggleLock => "object.toggle_lock",
            RepairMesh => "object.repair_mesh",
            ObjectSettings => "object.settings",
            PlateNew => "plate.new",
            PlateDuplicate => "plate.duplicate",
            PlateRename => "plate.rename",
            PlateDelete => "plate.delete",
            PlateNext => "plate.next",
            PlatePrev => "plate.previous",
            CheckPlate => "plate.check",
            FitCoupon => "plate.fit_coupon",
            Theme(t) => return format!("app.theme_{}", t.id()),
            PrinterCamera => "printer.camera",
            PrinterPause => "printer.pause",
            PrinterResume => "printer.resume",
            PrinterStop => "printer.stop",
            SkipObjects => "printer.skip_objects",
            LoadFilament => "printer.load_filament",
            UnloadFilament => "printer.unload_filament",
            SyncSpools => "printer.sync_spools",
            PrinterFiles => "printer.files",
            PrinterTimelapses => "printer.timelapses",
            AddPrinter => "printer.add",
            ConnectAgent => "app.connect_agent",
            OpenGuide => "help.guide",
            ToolsReference => "help.tools",
            ShowDataFolder => "help.data_folder",
            ReportIssue => "help.report_issue",
        };
        s.to_string()
    }

    pub fn from_id(id: &str) -> Option<Action> {
        Action::all().into_iter().find(|a| a.id() == id)
    }

    pub fn label(self) -> String {
        use Action::*;
        let s = match self {
            New => "New project",
            Open => "Open project…",
            Save => "Save",
            SaveAs => "Save as…",
            Import => "Import model…",
            ExportPrint => "Export plate sliced file (.gcode.3mf)…",
            ExportStl => "Export STL…",
            Export3mf => "Export 3MF…",
            ExportScad => "Export OpenSCAD…",
            Undo => "Undo",
            Redo => "Redo",
            SelectAll => "Select all",
            Deselect => "Deselect / exit tool",
            Delete => "Delete selected",
            DeleteAll => "Delete all objects",
            Duplicate => "Duplicate selected",
            Copy => "Copy objects",
            Cut => "Cut objects",
            Paste => "Paste objects",
            Rename => "Rename selected",
            ToggleHidden => "Hide / show selected",
            SetFilament(n) => return format!("Set filament {n} for selection"),
            Op(o) => return format!("Arrange: {}", o.label()),
            Arrange => "Arrange all objects",
            Orient => "Auto-orient (selection or all)",
            PlaceOnFace => "Place on face (lay flat)",
            MoveLeft => "Nudge left 1 mm",
            MoveRight => "Nudge right 1 mm",
            MoveUp => "Nudge back 1 mm",
            MoveDown => "Nudge forward 1 mm",
            MoveLeftBig => "Nudge left 10 mm",
            MoveRightBig => "Nudge right 10 mm",
            MoveUpBig => "Nudge back 10 mm",
            MoveDownBig => "Nudge forward 10 mm",
            RotateCcw => "Rotate 15° counter-clockwise",
            RotateCw => "Rotate 15° clockwise",
            ScaleUp => "Scale up 5%",
            ScaleDown => "Scale down 5%",
            ToolSelect => "Select tool",
            ToolMove => "Move tool",
            ToolRotate => "Rotate tool",
            ToolScale => "Scale tool",
            ToolPushPull => "Push/Pull tool",
            ViewHome => "Default view (whole plate)",
            ViewIso => "Isometric view",
            ViewTop => "Top view",
            ViewBottom => "Bottom view",
            ViewFront => "Front view",
            ViewBack => "Back view",
            ViewLeft => "Left view",
            ViewRight => "Right view",
            FrameSelection => "Frame selection (or everything)",
            TogglePreview => "Toggle Prepare / Preview",
            Prepare => "Prepare (model view)",
            Preview => "Preview (toolpaths)",
            ToggleOverhangs => "Show overhangs",
            ToggleAllPlates => "Show all plates",
            LayerUp => "Layer up",
            LayerDown => "Layer down",
            LayerUpFast => "Layer up ×5",
            LayerDownFast => "Layer down ×5",
            OneLayer => "One-layer mode",
            Slice => "Slice plate",
            Print => "Print plate…",
            Palette => "Command palette",
            Preferences => "Preferences…",
            Shortcuts => "Keyboard shortcuts",
            ToggleDock(Side::Left) => "Toggle left dock",
            ToggleDock(Side::Right) => "Toggle right dock",
            ToggleDock(Side::Bottom) => "Toggle bottom dock",
            Zen => "Zen mode (hide all docks)",
            ResetLayout => "Reset layout",
            ToggleTheme => "Toggle dark / light theme",
            ZoomIn => "Zoom UI in",
            ZoomOut => "Zoom UI out",
            ZoomReset => "Reset UI zoom",
            ShowPanel(p) => return format!("Show {} panel", p.label()),
            AddShape(i) => return format!("Add {}", crate::app::PRIMITIVES.get(i as usize).map(|p| p.0.to_lowercase()).unwrap_or_default()),
            SideBySide => "Plate and file side by side",
            ExportAny => "Export…",
            CloseTab => "Close tab",
            ReopenTab => "Reopen closed tab",
            NextTab => "Next tab",
            PrevTab => "Previous tab",
            CloseWindow => "Close window",
            Quit => "Quit only3d",
            ExportSelection => "Export the selection…",
            OpenFolder => "Open a folder…",
            CloseFolder => "Close the folder",
            ExportBambu => "Export Bambu Studio project…",
            ShowInFinder => if cfg!(target_os = "macos") { "Show project in Finder" } else if cfg!(windows) { "Show project in Explorer" } else { "Show project in Files" },
            Versions => "Revert to a saved version…",
            ToggleLock => "Lock / unlock selected",
            RepairMesh => "Repair imported meshes",
            ObjectSettings => "Settings for this object…",
            PlateNew => "New plate",
            PlateDuplicate => "Duplicate plate",
            PlateRename => "Rename plate…",
            PlateDelete => "Delete plate…",
            PlateNext => "Next plate",
            PlatePrev => "Previous plate",
            CheckPlate => "Check printability",
            FitCoupon => "Add a fit test coupon",
            Theme(t) => return format!("Theme: {}", t.label()),
            PrinterCamera => "Printer camera",
            PrinterPause => "Pause print",
            PrinterResume => "Resume print",
            PrinterStop => "Stop print…",
            SkipObjects => "Skip objects…",
            LoadFilament => "Load filament…",
            UnloadFilament => "Unload filament…",
            SyncSpools => "Sync AMS spools",
            PrinterFiles => "Files on the printer…",
            PrinterTimelapses => "Timelapses",
            AddPrinter => "Add a printer…",
            ConnectAgent => "Connect an agent…",
            OpenGuide => "only3d guide",
            ToolsReference => "Agent tools reference",
            ShowDataFolder => "Show data folder",
            ReportIssue => "Report an issue",
        };
        s.to_string()
    }

    pub fn category(self) -> &'static str {
        use Action::*;
        match self {
            New | Open | Save | SaveAs | Import | ExportPrint | ExportStl | Export3mf | ExportScad => "File",
            Undo | Redo | SelectAll | Deselect | Delete | DeleteAll | Duplicate | Copy | Cut | Paste | Rename | ToggleHidden | SetFilament(_) => "Edit",
            Arrange | Orient | PlaceOnFace | MoveLeft | MoveRight | MoveUp | MoveDown | MoveLeftBig | MoveRightBig | MoveUpBig | MoveDownBig | RotateCcw | RotateCw | ScaleUp | ScaleDown => "Object",
            ToolSelect | ToolMove | ToolRotate | ToolScale | ToolPushPull => "Tools",
            ViewHome | ViewIso | ViewTop | ViewBottom | ViewFront | ViewBack | ViewLeft | ViewRight | FrameSelection | TogglePreview | Prepare | Preview | ToggleOverhangs | ToggleAllPlates | SideBySide | CloseTab | ReopenTab | NextTab | PrevTab => "View",
            LayerUp | LayerDown | LayerUpFast | LayerDownFast | OneLayer => "Preview",
            Slice | Print => "Slice & print",
            Op(_) => "Arrange",
            Palette | Preferences | Shortcuts | ToggleDock(_) | Zen | ResetLayout | ToggleTheme | ZoomIn | ZoomOut | ZoomReset | ShowPanel(_) | Theme(_) | CloseWindow | Quit => "Window",
            OpenFolder | CloseFolder | ExportBambu | ShowInFinder | Versions | ExportAny | ExportSelection => "File",
            AddShape(_) | ToggleLock | RepairMesh | ObjectSettings => "Object",
            PlateNew | PlateDuplicate | PlateRename | PlateDelete | PlateNext | PlatePrev | CheckPlate | FitCoupon => "Plate",
            PrinterCamera | PrinterPause | PrinterResume | PrinterStop | SkipObjects | LoadFilament | UnloadFilament | SyncSpools | PrinterFiles | PrinterTimelapses | AddPrinter => "Printer",
            ConnectAgent | OpenGuide | ToolsReference | ShowDataFolder | ReportIssue => "Help",
        }
    }

    /// May fire while a text field has keyboard focus (only ⌘-chords that text editing does not use).
    pub fn while_typing(self) -> bool {
        use Action::*;
        matches!(self, Palette | Quit | Save | SaveAs | Open | New | Preferences | ToggleDock(_) | Zen | Slice | Print | ZoomIn | ZoomOut | ZoomReset | ExportPrint)
    }
}

/// `Rounded box` → `rounded_box` (the id of an add-shape action).
fn shape_slug(i: u8) -> String {
    crate::app::PRIMITIVES.get(i as usize).map(|p| p.0.to_lowercase().replace(' ', "_")).unwrap_or_else(|| i.to_string())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    #[default]
    Only3d,
    #[serde(alias = "bambu_studio")]
    Bambu,
}

impl Preset {
    pub const ALL: [Preset; 2] = [Preset::Only3d, Preset::Bambu];
    pub fn label(self) -> &'static str {
        match self {
            Preset::Only3d => if MAC { "only3d (macOS)" } else { "only3d" },
            Preset::Bambu => "Bambu Studio",
        }
    }
    pub fn parse(s: &str) -> Option<Preset> {
        match s.trim().to_ascii_lowercase().replace([' ', '-'], "_").as_str() {
            "only3d" | "default" => Some(Preset::Only3d),
            "bambu" | "bambu_studio" => Some(Preset::Bambu),
            _ => None,
        }
    }

    /// (context, chord, action id) rows. Arrow keys nudge relative to the screen: only3d
    /// moves 1 mm (Shift: 10 mm), Bambu Studio 10 mm (Shift: 1 mm).
    pub fn table(self) -> Vec<(When, &'static str, &'static str)> {
        use When::*;
        let mut t: Vec<(When, &'static str, &'static str)> = Vec::new();
        match self {
            Preset::Only3d => {
                t.extend([
                    (Always, "cmd-n", "file.new"),
                    (Always, "cmd-o", "file.open"),
                    (Always, "cmd-s", "file.save"),
                    (Always, "cmd-shift-s", "file.save_as"),
                    (Always, "cmd-i", "file.import"),
                    (Always, "cmd-e", "file.export_print"),
                    (Always, "cmd-shift-e", "file.export"),
                    (Always, "cmd-z", "edit.undo"),
                    (Always, "cmd-shift-z", "edit.redo"),
                    (Always, "cmd-y", "edit.redo"),
                    (Always, "cmd-a", "edit.select_all"),
                    (Always, "escape", "edit.deselect"),
                    (Always, "delete", "edit.delete"),
                    (Always, "backspace", "edit.delete"),
                    (Always, "cmd-shift-backspace", "edit.delete_all"),
                    (Always, "cmd-d", "edit.duplicate"),
                    (Always, "cmd-c", "edit.copy"),
                    (Always, "cmd-x", "edit.cut"),
                    (Always, "cmd-v", "edit.paste"),
                    (Always, "f2", "edit.rename"),
                    (Always, "enter", "edit.rename"),
                    (Always, "h", "edit.toggle_hidden"),
                    (Always, "shift-a", "object.arrange"),
                    (Always, "shift-r", "object.orient"),
                    (Always, "shift-f", "object.place_on_face"),
                    (Prepare, "left", "object.move_left"),
                    (Prepare, "right", "object.move_right"),
                    (Prepare, "up", "object.move_up"),
                    (Prepare, "down", "object.move_down"),
                    (Prepare, "shift-left", "object.move_left_10"),
                    (Prepare, "shift-right", "object.move_right_10"),
                    (Prepare, "shift-up", "object.move_up_10"),
                    (Prepare, "shift-down", "object.move_down_10"),
                    (Prepare, "[", "object.rotate_ccw"),
                    (Prepare, "]", "object.rotate_cw"),
                    (Prepare, "q", "tool.select"),
                    (Prepare, "m", "tool.move"),
                    (Prepare, "r", "tool.rotate"),
                    (Prepare, "s", "tool.scale"),
                    (Prepare, "p", "tool.push_pull"),
                    (Always, "0", "view.iso"),
                    (Always, "1", "view.front"),
                    (Always, "shift-1", "view.back"),
                    (Always, "3", "view.right"),
                    (Always, "shift-3", "view.left"),
                    (Always, "7", "view.top"),
                    (Always, "shift-7", "view.bottom"),
                    (Always, "home", "view.home"),
                    (Always, "f", "view.frame_selection"),
                    (Always, "tab", "view.toggle_preview"),
                    (Always, "cmd-l", "view.overhangs"),
                    (Preview, "up", "preview.layer_up"),
                    (Preview, "down", "preview.layer_down"),
                    (Preview, "shift-up", "preview.layer_up_5"),
                    (Preview, "shift-down", "preview.layer_down_5"),
                    (Preview, "l", "preview.one_layer"),
                    (Always, "cmd-r", "slice.slice"),
                    (Always, "cmd-p", "slice.print"),
                    (Always, "cmd-k", "app.command_palette"),
                    (Always, "cmd-shift-p", "app.command_palette"),
                    (Always, "cmd-,", "app.preferences"),
                    (Always, "?", "app.shortcuts"),
                    (Always, "cmd-/", "app.shortcuts"),
                    (Always, "cmd-b", "dock.toggle_left"),
                    (Always, "cmd-alt-b", "dock.toggle_right"),
                    (Always, "cmd-j", "dock.toggle_bottom"),
                    (Always, "cmd-shift-enter", "app.zen_mode"),
                    (Always, "cmd-\\", "view.side_by_side"),
                    (Always, "cmd-w", "view.close_tab"),
                    (Always, "cmd-shift-t", "view.reopen_tab"),
                    (Always, "ctrl-tab", "view.next_tab"),
                    (Always, "ctrl-shift-tab", "view.previous_tab"),
                    (Always, "cmd-shift-w", "app.close_window"),
                    (Always, "cmd-q", "app.quit"),
                    (Always, "cmd-alt-right", "plate.next"),
                    (Always, "cmd-alt-left", "plate.previous"),
                ]);
                for n in 1..=9 {
                    t.push((Always, ALT_DIGITS[n - 1], FILAMENT_IDS[n - 1]));
                }
            }
            Preset::Bambu => {
                t.extend([
                    (Always, "cmd-n", "file.new"),
                    (Always, "cmd-o", "file.open"),
                    (Always, "cmd-s", "file.save"),
                    (Always, "cmd-shift-s", "file.save_as"),
                    (Always, "cmd-i", "file.import"),
                    (Always, "cmd-g", "file.export_print"),
                    (Always, "cmd-r", "slice.slice"),
                    (Always, "cmd-shift-g", "slice.print"),
                    (Always, "cmd-x", "edit.cut"),
                    (Always, "cmd-c", "edit.copy"),
                    (Always, "cmd-v", "edit.paste"),
                    (Always, "cmd-,", "app.preferences"),
                    (Always, "ctrl-tab", "view.toggle_preview"),
                    (Always, "backspace", "edit.delete"),
                    (Always, "delete", "edit.delete"),
                    (Always, "shift-?", "app.shortcuts"),
                    (Always, "a", "object.arrange"),
                    (Always, "shift-a", "object.arrange"),
                    (Always, "shift-r", "object.orient"),
                    (Always, "shift-tab", "dock.toggle_right"),
                    (Prepare, "left", "object.move_left_10"),
                    (Prepare, "right", "object.move_right_10"),
                    (Prepare, "up", "object.move_up_10"),
                    (Prepare, "down", "object.move_down_10"),
                    (Prepare, "shift-left", "object.move_left"),
                    (Prepare, "shift-right", "object.move_right"),
                    (Prepare, "shift-up", "object.move_up"),
                    (Prepare, "shift-down", "object.move_down"),
                    (Always, "escape", "edit.deselect"),
                    (Always, "cmd-0", "view.home"),
                    (Always, "cmd-1", "view.top"),
                    (Always, "cmd-2", "view.bottom"),
                    (Always, "cmd-3", "view.front"),
                    (Always, "cmd-4", "view.back"),
                    (Always, "cmd-5", "view.left"),
                    (Always, "cmd-6", "view.right"),
                    (Always, "cmd-7", "view.iso"),
                    (Always, "z", "view.frame_selection"),
                    (Always, "cmd-l", "view.overhangs"),
                    (Always, "cmd-a", "edit.select_all"),
                    (Always, "cmd-shift-d", "edit.delete_all"),
                    (Always, "cmd-z", "edit.undo"),
                    (Always, "cmd-shift-z", "edit.redo"),
                    (Always, "cmd-y", "edit.redo"),
                    (Prepare, "m", "tool.move"),
                    (Prepare, "s", "tool.scale"),
                    (Prepare, "r", "tool.rotate"),
                    (Prepare, "p", "tool.push_pull"),
                    (Prepare, "f", "object.place_on_face"),
                    (Always, "cmd-k", "edit.duplicate"),
                    (Always, "space", "edit.rename"),
                    (Preview, "up", "preview.layer_up"),
                    (Preview, "down", "preview.layer_down"),
                    (Preview, "shift-up", "preview.layer_up_5"),
                    (Preview, "shift-down", "preview.layer_down_5"),
                    (Preview, "l", "preview.one_layer"),
                    // only3d extras that don't collide with Bambu Studio's list
                    (Always, "cmd-shift-p", "app.command_palette"),
                    (Always, "cmd-b", "dock.toggle_left"),
                    (Always, "cmd-alt-b", "dock.toggle_right"),
                    (Always, "cmd-j", "dock.toggle_bottom"),
                    (Always, "cmd-shift-enter", "app.zen_mode"),
                    (Always, "cmd-\\", "view.side_by_side"),
                    (Always, "cmd-w", "view.close_tab"),
                    (Always, "cmd-shift-t", "view.reopen_tab"),
                    (Always, "ctrl-tab", "view.next_tab"),
                    (Always, "ctrl-shift-tab", "view.previous_tab"),
                    (Always, "cmd-shift-w", "app.close_window"),
                    (Always, "cmd-q", "app.quit"),
                    (Always, "cmd-alt-right", "plate.next"),
                    (Always, "cmd-alt-left", "plate.previous"),
                ]);
                for n in 1..=9 {
                    t.push((Prepare, DIGITS[n - 1], FILAMENT_IDS[n - 1]));
                }
            }
        }
        t.extend([(Always, "cmd-=", "app.zoom_in"), (Always, "cmd-+", "app.zoom_in"), (Always, "cmd--", "app.zoom_out")]);
        if self == Preset::Only3d {
            t.push((Always, "cmd-0", "app.zoom_reset"));
        }
        t
    }
}

const DIGITS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];
const ALT_DIGITS: [&str; 9] = ["alt-1", "alt-2", "alt-3", "alt-4", "alt-5", "alt-6", "alt-7", "alt-8", "alt-9"];
const FILAMENT_IDS: [&str; 9] = ["edit.filament_1", "edit.filament_2", "edit.filament_3", "edit.filament_4", "edit.filament_5", "edit.filament_6", "edit.filament_7", "edit.filament_8", "edit.filament_9"];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Binding {
    pub when: When,
    pub chord: Chord,
    pub action: Action,
}

#[derive(Clone, Debug)]
pub struct Keymap {
    pub preset: Preset,
    pub bindings: Vec<Binding>,
    /// Problems found in keymap.json (shown once as a toast).
    pub errors: Vec<String>,
}

impl Keymap {
    pub fn preset(preset: Preset) -> Keymap {
        let mut bindings = Vec::new();
        let mut errors = Vec::new();
        for (when, chord, id) in preset.table() {
            match (Chord::parse(chord), Action::from_id(id)) {
                (Some(c), Some(a)) => bindings.push(Binding { when, chord: c, action: a }),
                _ => errors.push(format!("preset binding {chord} → {id} is invalid")),
            }
        }
        Keymap { preset, bindings, errors }
    }

    /// Apply user overrides (the keymap.json format in the module docs).
    pub fn apply_overrides(&mut self, v: &Value) {
        let Some(obj) = v.as_object() else {
            self.errors.push("keymap.json must be a JSON object".into());
            return;
        };
        for (section, when) in [("bindings", When::Always), ("prepare", When::Prepare), ("preview", When::Preview)] {
            let Some(map) = obj.get(section) else { continue };
            let Some(map) = map.as_object() else {
                self.errors.push(format!("keymap.json: \"{section}\" must be an object of chord → action id"));
                continue;
            };
            for (chord_s, act) in map {
                let Some(chord) = Chord::parse(chord_s) else {
                    self.errors.push(format!("keymap.json: unknown key chord \"{chord_s}\""));
                    continue;
                };
                let action = match act {
                    Value::Null => None,
                    Value::String(id) => match Action::from_id(id) {
                        Some(a) => Some(a),
                        None => {
                            self.errors.push(format!("keymap.json: unknown action \"{id}\""));
                            continue;
                        }
                    },
                    _ => {
                        self.errors.push(format!("keymap.json: \"{chord_s}\" must map to an action id or null"));
                        continue;
                    }
                };
                self.bindings.retain(|b| !(b.chord == chord && b.when == when));
                if let Some(a) = action {
                    self.bindings.push(Binding { when, chord, action: a });
                }
            }
        }
        for k in obj.keys() {
            if !["bindings", "prepare", "preview", "$schema", "comment"].contains(&k.as_str()) {
                self.errors.push(format!("keymap.json: unknown section \"{k}\" (use bindings / prepare / preview)"));
            }
        }
    }

    /// The preset plus `~/.only3d/keymap.json`, if present.
    pub fn load(preset: Preset) -> Keymap {
        let mut k = Keymap::preset(preset);
        let path = keymap_path();
        if let Ok(text) = std::fs::read_to_string(&path) {
            match serde_json::from_str::<Value>(&text) {
                Ok(v) => k.apply_overrides(&v),
                Err(e) => k.errors.push(format!("{}: {e}", path.display())),
            }
        }
        k
    }

    /// The action a key press triggers in this view. View-specific bindings win.
    pub fn lookup(&self, key: Key, physical: Option<Key>, mods: Mods, view: When) -> Option<Action> {
        let mut any = None;
        for b in &self.bindings {
            if b.chord.matches(key, physical, mods) {
                if b.when == view {
                    return Some(b.action);
                }
                if b.when == When::Always && any.is_none() {
                    any = Some(b.action);
                }
            }
        }
        any
    }

    pub fn chords(&self, action: Action) -> Vec<(When, Chord)> {
        self.bindings.iter().filter(|b| b.action == action).map(|b| (b.when, b.chord)).collect()
    }

    /// The first chord's label, for menus and the palette.
    pub fn label_for(&self, action: Action) -> Option<String> {
        self.bindings.iter().find(|b| b.action == action).map(|b| b.chord.label())
    }

    /// The shortcut a menu item shows for this action: the first ⌘/⌃ binding that works
    /// everywhere, else the first binding of any kind (a plain key like M, which the menu only
    /// displays; see `menubar`).
    pub fn menu_chord(&self, action: Action) -> Option<(Chord, When)> {
        let mine = || self.bindings.iter().filter(|b| b.action == action);
        mine().find(|b| b.chord.is_command() && b.when == When::Always).or_else(|| mine().next()).map(|b| (b.chord, b.when))
    }
}

pub fn keymap_path() -> std::path::PathBuf {
    only3d_studio::config_dir().join("keymap.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(cmd: bool, shift: bool) -> Mods {
        Mods { cmd, shift, ..Default::default() }
    }

    #[test]
    fn parse_and_format_round_trip() {
        for s in ["cmd-shift-p", "alt-1", "shift-up", "escape", "cmd--", "cmd-+", "cmd-,", "?", "f2", "cmd-alt-b", "shift-backspace", "cmd-shift-enter", "[", "space"] {
            let c = Chord::parse(s).unwrap_or_else(|| panic!("parse {s}"));
            let back = Chord::parse(&c.config()).unwrap();
            assert_eq!(c, back, "{s} → {}", c.config());
        }
        assert_eq!(Chord::parse("Cmd+Shift+P"), Chord::parse("cmd-shift-p"));
        assert_eq!(Chord::parse("cmd--").unwrap().key, Key::Minus);
        assert_eq!(Chord::parse("cmd-+").unwrap().key, Key::Plus);
        assert_eq!(Chord::parse("+").unwrap().key, Key::Plus);
        assert_eq!(Chord::parse("shift-?").unwrap().key, Key::Questionmark);
        assert!(Chord::parse("cmd-").is_none() || Chord::parse("cmd-").unwrap().key == Key::Minus);
        assert!(Chord::parse("cmd-notakey").is_none());
        assert!(Chord::parse("").is_none());
        let p = Chord::parse("cmd-shift-p").unwrap();
        if MAC {
            assert_eq!(p.label(), "⇧⌘P");
            assert_eq!(Chord::parse("?").unwrap().label(), "?");
        } else {
            assert_eq!(p.label(), "Ctrl+Shift+P");
        }
    }

    #[test]
    fn matching_handles_shifted_keys() {
        let q = Chord::parse("?").unwrap();
        // "?" typed as Shift+/ on a US layout
        assert!(q.matches(Key::Questionmark, Some(Key::Slash), m(false, true)));
        assert!(q.matches(Key::Questionmark, Some(Key::Slash), m(false, false)));
        assert!(!q.matches(Key::Questionmark, Some(Key::Slash), m(true, true)));
        // Shift+1 arrives as "!" with the physical 1 key
        let back = Chord::parse("shift-1").unwrap();
        assert!(back.matches(Key::Exclamationmark, Some(Key::Num1), m(false, true)));
        assert!(!back.matches(Key::Num1, Some(Key::Num1), m(false, false)));
        // letters never fall back to the physical key (other layouts)
        let a = Chord::parse("a").unwrap();
        assert!(!a.matches(Key::Q, Some(Key::A), Mods::default()));
        assert!(a.matches(Key::A, Some(Key::A), Mods::default()));
        assert!(!a.matches(Key::A, Some(Key::A), m(true, false)));
    }

    #[test]
    fn presets_are_valid_and_cover_the_basics() {
        for p in Preset::ALL {
            let k = Keymap::preset(p);
            assert!(k.errors.is_empty(), "{p:?}: {:?}", k.errors);
            let cmd = |c: Key| (c, None, m(true, false));
            let look = |(key, phys, mods): (Key, Option<Key>, Mods), w| k.lookup(key, phys, mods, w);
            assert_eq!(look(cmd(Key::A), When::Prepare), Some(Action::SelectAll), "{p:?} cmd-a");
            assert_eq!(look(cmd(Key::Z), When::Prepare), Some(Action::Undo));
            assert_eq!(k.lookup(Key::Z, None, m(true, true), When::Prepare), Some(Action::Redo));
            assert_eq!(look(cmd(Key::S), When::Prepare), Some(Action::Save));
            assert_eq!(look(cmd(Key::R), When::Prepare), Some(Action::Slice));
            assert_eq!(k.lookup(Key::Escape, None, Mods::default(), When::Prepare), Some(Action::Deselect));
            assert_eq!(k.lookup(Key::Delete, None, Mods::default(), When::Prepare), Some(Action::Delete));
            assert_eq!(k.lookup(Key::P, None, m(true, true), When::Preview), Some(Action::Palette));
            // preview: arrows move the layer slider, not the selection
            assert_eq!(k.lookup(Key::ArrowUp, None, Mods::default(), When::Preview), Some(Action::LayerUp));
            assert_eq!(k.lookup(Key::ArrowUp, None, m(false, true), When::Preview), Some(Action::LayerUpFast));
            // every action id resolves back to itself
            for a in Action::all() {
                assert_eq!(Action::from_id(&a.id()), Some(a));
            }
        }
        let os = Keymap::preset(Preset::Only3d);
        let bb = Keymap::preset(Preset::Bambu);
        assert_eq!(os.lookup(Key::ArrowLeft, None, Mods::default(), When::Prepare), Some(Action::MoveLeft));
        assert_eq!(os.lookup(Key::ArrowLeft, None, m(false, true), When::Prepare), Some(Action::MoveLeftBig));
        assert_eq!(bb.lookup(Key::ArrowLeft, None, Mods::default(), When::Prepare), Some(Action::MoveLeftBig));
        assert_eq!(bb.lookup(Key::ArrowLeft, None, m(false, true), When::Prepare), Some(Action::MoveLeft));
        assert_eq!(os.lookup(Key::K, None, m(true, false), When::Prepare), Some(Action::Palette));
        assert_eq!(bb.lookup(Key::K, None, m(true, false), When::Prepare), Some(Action::Duplicate));
        assert_eq!(os.lookup(Key::F, None, Mods::default(), When::Prepare), Some(Action::FrameSelection));
        assert_eq!(bb.lookup(Key::F, None, Mods::default(), When::Prepare), Some(Action::PlaceOnFace));
        assert_eq!(bb.lookup(Key::Num3, None, Mods::default(), When::Prepare), Some(Action::SetFilament(3)));
        assert_eq!(os.lookup(Key::Num3, None, Mods::default(), When::Prepare), Some(Action::ViewRight));
        assert_eq!(bb.lookup(Key::Num3, None, m(true, false), When::Prepare), Some(Action::ViewFront));
        assert_eq!(os.lookup(Key::Num0, None, m(true, false), When::Prepare), Some(Action::ZoomReset));
        assert_eq!(bb.lookup(Key::Num0, None, m(true, false), When::Prepare), Some(Action::ViewHome));
        assert_eq!(bb.lookup(Key::Questionmark, Some(Key::Slash), m(false, true), When::Prepare), Some(Action::Shortcuts));
        assert_eq!(os.lookup(Key::Questionmark, Some(Key::Slash), m(false, true), When::Prepare), Some(Action::Shortcuts));
        assert_eq!(os.lookup(Key::Num1, None, Mods { alt: true, ..Default::default() }, When::Prepare), Some(Action::SetFilament(1)));
    }

    #[test]
    fn overrides_rebind_and_unbind() {
        let mut k = Keymap::preset(Preset::Only3d);
        let v: Value = serde_json::from_str(
            r#"{"bindings": {"cmd-k": null, "g": "tool.move", "cmd-shift-x": "edit.cut"},
                "preview": {"j": "preview.layer_down"},
                "prepare": {"x": "not.an.action", "cmd-": "edit.copy"},
                "oops": {}}"#,
        )
        .unwrap();
        k.apply_overrides(&v);
        assert_eq!(k.lookup(Key::K, None, m(true, false), When::Prepare), None);
        assert_eq!(k.lookup(Key::G, None, Mods::default(), When::Prepare), Some(Action::ToolMove));
        assert_eq!(k.lookup(Key::X, None, m(true, true), When::Prepare), Some(Action::Cut));
        assert_eq!(k.lookup(Key::J, None, Mods::default(), When::Preview), Some(Action::LayerDown));
        assert_eq!(k.lookup(Key::J, None, Mods::default(), When::Prepare), None);
        // the palette is still on ⌘⇧P
        assert_eq!(k.lookup(Key::P, None, m(true, true), When::Prepare), Some(Action::Palette));
        assert!(k.errors.iter().any(|e| e.contains("not.an.action")));
        assert!(k.errors.iter().any(|e| e.contains("oops")));
    }

    #[test]
    fn view_specific_bindings_win() {
        let k = Keymap::preset(Preset::Only3d);
        assert_eq!(k.lookup(Key::ArrowUp, None, Mods::default(), When::Prepare), Some(Action::MoveUp));
        assert_eq!(k.lookup(Key::ArrowUp, None, Mods::default(), When::Preview), Some(Action::LayerUp));
        assert_eq!(k.lookup(Key::L, None, Mods::default(), When::Prepare), None);
        assert_eq!(k.lookup(Key::L, None, Mods::default(), When::Preview), Some(Action::OneLayer));
    }
}
