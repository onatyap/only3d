//! The macOS menu bar: only3d, File, Edit, Object, Arrange, Plate, View, Printer, Window, Help.
//!
//! Menus are data ([`spec`]). Every entry is an [`Action`], the same ones the palette, the
//! shortcuts and the right-click menus run, or a list that changes (recent projects, plates,
//! panels). A click arrives as an id the app turns back into an action ([`Cmd::parse`]), so a
//! menu never edits the project itself: it goes through `App::run` and the tools like every
//! other entry point.
//!
//! Shortcuts come from the active keymap. A ⌘ or ⌃ chord becomes the item's key equivalent, so
//! AppKit runs it from the menu and the window never sees the key. Plain keys (M, F, 1) are only
//! given key equivalents when [`plain_keys`] is on. While a text field has focus, the app takes off every chord that text editing needs.

use crate::arrange_ops::ArrangeOp as AO;
use crate::dock::{PanelId, Side};
use crate::keymap::{Action, Chord};
use crate::theme::ThemeChoice;

/// Give plain keys (M, F, ↑) key equivalents too. Off: they stay with the app's keymap, which
/// knows their Prepare / Preview context. `ONLY3D_GUI_MENU_PLAIN=1` turns it on (a dev knob
/// for checking how AppKit routes them); `ONLY3D_GUI_MENU_LOG=1` prints every menu pick.
pub fn plain_keys() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("ONLY3D_GUI_MENU_PLAIN").is_ok_and(|v| v == "1"))
}

fn log_picks() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("ONLY3D_GUI_MENU_LOG").is_ok_and(|v| v == "1"))
}

/// A standard macOS item AppKit provides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sys {
    About,
    Services,
    Hide,
    HideOthers,
    ShowAll,
    Quit,
    Minimize,
    Zoom,
    FullScreen,
    BringAllToFront,
}

#[derive(Clone, Debug)]
pub enum Entry {
    /// An action, with the menu's own label.
    Act(Action, String),
    /// An action shown with a tick when it is on (a tool, a mode, a toggle).
    Check(Action, String),
    Sub(String, Vec<Entry>),
    Sep,
    Sys(Sys),
    /// File > Open Recent.
    Recent,
    /// Object > Move to Plate.
    MoveToPlate,
    /// Window > Panels: ticked when showing; a click shows or hides it.
    Panel(PanelId),
}

pub struct Top {
    pub title: &'static str,
    pub entries: Vec<Entry>,
}

fn a(action: Action, label: &str) -> Entry {
    Entry::Act(action, label.to_string())
}

fn c(action: Action, label: &str) -> Entry {
    Entry::Check(action, label.to_string())
}

fn sub(label: &str, entries: Vec<Entry>) -> Entry {
    Entry::Sub(label.to_string(), entries)
}

/// Title Case for a menu label ("Plate with M3 holes" → "Plate with M3 Holes").
pub fn title_case(s: &str) -> String {
    const SMALL: [&str; 9] = ["a", "an", "and", "the", "with", "to", "of", "on", "in"];
    s.split(' ')
        .enumerate()
        .map(|(i, w)| {
            if i > 0 && SMALL.contains(&w) {
                return w.to_string();
            }
            let mut ch = w.chars();
            match ch.next() {
                Some(f) => f.to_uppercase().chain(ch).collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The whole menu bar, in order. The first menu is the app menu (macOS titles it "only3d").
pub fn spec() -> Vec<Top> {
    use Action::*;
    use Entry::{Sep, Sys as S};
    let shapes = (0..crate::app::PRIMITIVES.len() as u8).map(|i| a(AddShape(i), &title_case(crate::app::PRIMITIVES[i as usize].0))).collect();
    let filaments = (1..=9).map(|k| c(SetFilament(k), &format!("Filament {k}"))).collect();
    vec![
        Top {
            title: "only3d",
            entries: vec![S(Sys::About), Sep, a(Preferences, "Settings…"), a(ConnectAgent, "Connect an Agent…"), Sep, S(Sys::Services), Sep, S(Sys::Hide), S(Sys::HideOthers), S(Sys::ShowAll), Sep, S(Sys::Quit)],
        },
        Top {
            title: "File",
            entries: vec![
                a(New, "New Project"),
                a(Open, "Open…"),
                a(OpenFolder, "Open Folder…"),
                Entry::Recent,
                Sep,
                a(Save, "Save"),
                a(SaveAs, "Save As…"),
                a(Versions, "Revert to Version…"),
                a(ShowInFinder, "Show in Finder"),
                Sep,
                a(Import, "Import…"),
                sub(
                    "Export",
                    vec![
                        a(ExportAny, "Export…"),
                        a(ExportSelection, "Selection…"),
                        Sep,
                        a(ExportStl, "STL…"),
                        a(Export3mf, "3MF…"),
                        a(ExportScad, "OpenSCAD (.scad)…"),
                        a(ExportBambu, "Bambu Studio Project…"),
                        Sep,
                        a(ExportPrint, "Sliced Plate (.gcode.3mf)…"),
                    ],
                ),
                Sep,
                a(CloseTab, "Close Tab"),
                a(ReopenTab, "Reopen Closed Tab"),
                a(CloseFolder, "Close Folder"),
                a(CloseWindow, "Close Window"),
            ],
        },
        Top {
            title: "Edit",
            entries: vec![
                a(Undo, "Undo"),
                a(Redo, "Redo"),
                Sep,
                a(Cut, "Cut"),
                a(Copy, "Copy"),
                a(Paste, "Paste"),
                a(Duplicate, "Duplicate"),
                a(Delete, "Delete"),
                a(DeleteAll, "Delete All"),
                Sep,
                a(SelectAll, "Select All"),
                a(Deselect, "Deselect"),
                Sep,
                a(Palette, "Command Palette…"),
            ],
        },
        Top {
            title: "Object",
            entries: vec![
                sub("Add Shape", shapes),
                Sep,
                c(ToolSelect, "Select"),
                c(ToolMove, "Move"),
                c(ToolRotate, "Rotate"),
                c(ToolScale, "Scale"),
                c(ToolPushPull, "Push/Pull a Face"),
                Sep,
                a(PlaceOnFace, "Lay Flat on Face"),
                a(Orient, "Auto-Orient"),
                a(RotateCcw, "Rotate 15° Counter-Clockwise"),
                a(RotateCw, "Rotate 15° Clockwise"),
                a(ScaleUp, "Scale Up 5%"),
                a(ScaleDown, "Scale Down 5%"),
                Sep,
                a(Op(AO::Assemble), "Group"),
                a(Op(AO::Explode), "Ungroup"),
                sub("Combine Solids", vec![a(Op(AO::Union), "Union"), a(Op(AO::Subtract), "Subtract from Anchor"), a(Op(AO::Intersect), "Intersect")]),
                a(RepairMesh, "Repair Mesh"),
                Sep,
                sub("Filament", filaments),
                Entry::MoveToPlate,
                a(ToggleHidden, "Hide / Show"),
                a(ToggleLock, "Lock / Unlock"),
                a(Rename, "Rename…"),
                a(ObjectSettings, "Object Settings…"),
                a(ExportSelection, "Export Selection…"),
            ],
        },
        Top {
            title: "Arrange",
            entries: vec![
                a(Arrange, "Arrange All"),
                Sep,
                sub("Align", vec![a(Op(AO::AlignLeft), "Left Edges"), a(Op(AO::AlignCenterX), "Centres Left–Right"), a(Op(AO::AlignRight), "Right Edges"), Sep, a(Op(AO::AlignFront), "Front Edges"), a(Op(AO::AlignMiddleY), "Centres Front–Back"), a(Op(AO::AlignBack), "Back Edges")]),
                sub(
                    "Distribute",
                    vec![
                        a(Op(AO::DistributeX), "Left to Right"),
                        a(Op(AO::DistributeY), "Front to Back"),
                        Sep,
                        a(Op(AO::StackX), "Stack Left to Right"),
                        a(Op(AO::StackY), "Stack Front to Back"),
                        a(Op(AO::Grid), "Grid"),
                        a(Op(AO::Circle), "Circle Around Anchor"),
                    ],
                ),
                sub("Dock to Anchor", vec![a(Op(AO::DockLeft), "Left"), a(Op(AO::DockRight), "Right"), a(Op(AO::DockFront), "Front"), a(Op(AO::DockBack), "Back")]),
                sub("Match Anchor", vec![a(Op(AO::MatchWidth), "Width"), a(Op(AO::MatchDepth), "Depth"), a(Op(AO::MatchHeight), "Height"), a(Op(AO::MatchSize), "Size")]),
                sub(
                    "Stretch to Anchor",
                    vec![a(Op(AO::StretchX), "Stretch Left–Right"), a(Op(AO::StretchY), "Stretch Front–Back"), Sep, a(Op(AO::FillX), "Extend Left–Right"), a(Op(AO::FillY), "Extend Front–Back")],
                ),
                Sep,
                a(Op(AO::Copies), "Copies…"),
                a(Op(AO::FillPlate), "Fill the Plate with Copies"),
                a(Op(AO::DuplicateRight), "Duplicate to the Right"),
                a(Op(AO::DuplicateBack), "Duplicate Behind"),
                a(Op(AO::Swap), "Swap Positions"),
                a(Op(AO::Snap), "Snap to 10 mm Grid"),
                Sep,
                a(Op(AO::CopyTransform), "Copy Position and Rotation"),
                a(Op(AO::PasteTransform), "Paste Position and Rotation"),
            ],
        },
        Top {
            title: "Plate",
            entries: vec![
                a(PlateNew, "New Plate"),
                a(PlateDuplicate, "Duplicate Plate"),
                a(PlateRename, "Rename Plate…"),
                a(PlateDelete, "Delete Plate…"),
                Sep,
                a(PlateNext, "Next Plate"),
                a(PlatePrev, "Previous Plate"),
                c(ToggleAllPlates, "Show All Plates"),
                Sep,
                a(CheckPlate, "Check Printability"),
                a(FitCoupon, "Add Fit Test Coupon"),
                Sep,
                a(Slice, "Slice Plate"),
                a(Print, "Print Plate…"),
            ],
        },
        Top {
            title: "View",
            entries: vec![
                c(Prepare, "Prepare"),
                c(Preview, "Preview"),
                Sep,
                a(ViewHome, "Default View"),
                a(ViewIso, "Isometric"),
                a(ViewFront, "Front"),
                a(ViewBack, "Back"),
                a(ViewRight, "Right"),
                a(ViewLeft, "Left"),
                a(ViewTop, "Top"),
                a(ViewBottom, "Bottom"),
                a(FrameSelection, "Frame Selection"),
                Sep,
                c(SideBySide, "Side by Side"),
                a(NextTab, "Next Tab"),
                a(PrevTab, "Previous Tab"),
                c(ToggleOverhangs, "Show Overhangs"),
                sub("Layers", vec![a(LayerUp, "Up"), a(LayerDown, "Down"), a(LayerUpFast, "Up 5"), a(LayerDownFast, "Down 5"), Sep, c(OneLayer, "One-Layer Mode")]),
                Sep,
                sub("Theme", ThemeChoice::ALL.iter().map(|t| c(Theme(*t), if *t == ThemeChoice::System { "System" } else { t.label() })).collect()),
                a(ZoomIn, "Zoom In"),
                a(ZoomOut, "Zoom Out"),
                a(ZoomReset, "Actual Size"),
                Sep,
                S(Sys::FullScreen),
            ],
        },
        Top {
            title: "Printer",
            entries: vec![
                a(ShowPanel(PanelId::Printer), "Printer Status"),
                a(PrinterCamera, "Camera"),
                Sep,
                a(PrinterPause, "Pause"),
                a(PrinterResume, "Resume"),
                a(PrinterStop, "Stop…"),
                a(SkipObjects, "Skip Objects…"),
                Sep,
                a(LoadFilament, "Load Filament…"),
                a(UnloadFilament, "Unload Filament…"),
                a(SyncSpools, "Sync AMS Spools"),
                Sep,
                a(PrinterFiles, "Files on Printer…"),
                a(ShowPanel(PanelId::Spools), "Spools"),
                a(ShowPanel(PanelId::Prints), "Print History"),
                Sep,
                a(AddPrinter, "Add Printer…"),
            ],
        },
        Top {
            title: "Window",
            entries: vec![
                S(Sys::Minimize),
                S(Sys::Zoom),
                Sep,
                sub("Panels", PanelId::ALL.iter().map(|p| Entry::Panel(*p)).collect()),
                Sep,
                c(ToggleDock(Side::Left), "Left Sidebar"),
                c(ToggleDock(Side::Right), "Right Sidebar"),
                c(ToggleDock(Side::Bottom), "Bottom Panel"),
                c(Zen, "Zen Mode"),
                a(ResetLayout, "Reset Layout"),
                Sep,
                S(Sys::BringAllToFront),
            ],
        },
        Top {
            title: "Help",
            entries: vec![a(OpenGuide, "only3d Guide"), a(Shortcuts, "Keyboard Shortcuts"), a(ToolsReference, "Agent Tools Reference"), Sep, a(ShowDataFolder, "Show Data Folder"), a(ReportIssue, "Report an Issue")],
        },
    ]
}

/// What a menu click asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    Run(Action),
    OpenRecent(String),
    ClearRecent,
    MoveToPlate(u32),
    MoveToNewPlate,
    TogglePanel(PanelId),
}

impl Cmd {
    pub fn id(&self) -> String {
        match self {
            Cmd::Run(a) => a.id(),
            Cmd::OpenRecent(p) => format!("recent:{p}"),
            Cmd::ClearRecent => "recent-clear".into(),
            Cmd::MoveToPlate(k) => format!("move-to-plate:{k}"),
            Cmd::MoveToNewPlate => "move-to-plate:new".into(),
            Cmd::TogglePanel(p) => format!("toggle-panel:{}", p.id()),
        }
    }

    pub fn parse(id: &str) -> Option<Cmd> {
        if let Some(p) = id.strip_prefix("recent:") {
            return Some(Cmd::OpenRecent(p.to_string()));
        }
        if id == "recent-clear" {
            return Some(Cmd::ClearRecent);
        }
        if let Some(k) = id.strip_prefix("move-to-plate:") {
            return if k == "new" { Some(Cmd::MoveToNewPlate) } else { k.parse().ok().map(Cmd::MoveToPlate) };
        }
        if let Some(p) = id.strip_prefix("toggle-panel:") {
            return PanelId::parse(p).map(Cmd::TogglePanel);
        }
        Action::from_id(id).map(Cmd::Run)
    }
}

/// Every action the menus can run.
#[cfg(test)]
pub fn actions() -> Vec<Action> {
    fn walk(es: &[Entry], out: &mut Vec<Action>) {
        for e in es {
            match e {
                Entry::Act(a, _) | Entry::Check(a, _) => out.push(*a),
                Entry::Sub(_, es) => walk(es, out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    for t in spec() {
        walk(&t.entries, &mut out);
    }
    out
}

/// The state the app gives each action entry.
#[derive(Clone, Debug, PartialEq)]
pub struct ItemState {
    pub enabled: bool,
    pub checked: bool,
    /// The key equivalent (a ⌘ / ⌃ chord, or a plain key when [`plain_keys`]).
    pub chord: Option<Chord>,
    /// A label that changes (Filament 2 · PLA Basic); `None` keeps the menu's own.
    pub text: Option<String>,
}

/// The installed menu bar (a no-op off macOS, and before `install`).
#[derive(Default)]
pub struct MenuBar {
    #[cfg(target_os = "macos")]
    native: Option<mac::Native>,
}

impl MenuBar {
    /// Build the menu bar and hand it to AppKit. Call on the main thread (eframe's creation
    /// callback); `ctx` is woken when a menu item is picked.
    pub fn install(ctx: &eframe::egui::Context) -> MenuBar {
        #[cfg(target_os = "macos")]
        {
            if std::thread::current().name() == Some("main") {
                return MenuBar { native: Some(mac::Native::build(ctx.clone())) };
            }
        }
        let _ = ctx;
        MenuBar::default()
    }

    pub fn installed(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.native.is_some()
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    /// Menu picks since the last call.
    pub fn take_commands(&mut self) -> Vec<Cmd> {
        #[cfg(target_os = "macos")]
        if let Some(n) = &self.native {
            return n.take();
        }
        Vec::new()
    }

    /// Bring every action entry up to date (only what changed reaches AppKit).
    pub fn sync(&mut self, state: impl Fn(Action) -> ItemState) {
        #[cfg(target_os = "macos")]
        if let Some(n) = &mut self.native {
            n.sync(&state);
        }
        let _ = state;
    }

    /// File > Open Recent (newest first).
    pub fn set_recent(&mut self, paths: &[String]) {
        #[cfg(target_os = "macos")]
        if let Some(n) = &mut self.native {
            n.set_recent(paths);
        }
        let _ = paths;
    }

    /// Object > Move to Plate: every plate but the active one, then New Plate.
    pub fn set_plates(&mut self, plates: &[(u32, String)], enabled: bool) {
        #[cfg(target_os = "macos")]
        if let Some(n) = &mut self.native {
            n.set_plates(plates, enabled);
        }
        let _ = (plates, enabled);
    }

    /// Window > Panels ticks.
    pub fn set_panels(&mut self, showing: impl Fn(PanelId) -> bool) {
        #[cfg(target_os = "macos")]
        if let Some(n) = &mut self.native {
            n.set_panels(&showing);
        }
        let _ = showing;
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::mpsc::{channel, Receiver};

    use muda::accelerator::{Key, KeyAccelerator, Modifiers, NamedKey};
    use muda::{AboutMetadata, CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};

    use super::{log_picks, plain_keys, spec, Cmd, Entry, ItemState, Sys};
    use crate::dock::PanelId;
    use crate::keymap::{Action, Chord};

    enum Kind {
        Plain(MenuItem),
        Check(CheckMenuItem),
    }

    struct Item {
        action: Action,
        kind: Kind,
        label: String,
        last: Option<ItemState>,
    }

    pub struct Native {
        _menu: Menu,
        /// eframe renames the app menu after the window title (the project file) on its first
        /// frames; the first syncs put "only3d" back.
        app_menu: Option<Submenu>,
        title_fixes: u32,
        items: Vec<Item>,
        recent: Submenu,
        recent_now: Vec<String>,
        plates: Submenu,
        plates_now: Option<(Vec<(u32, String)>, bool)>,
        panels: Vec<(PanelId, CheckMenuItem, Option<bool>)>,
        rx: Receiver<String>,
    }

    fn sys_item(s: Sys) -> PredefinedMenuItem {
        match s {
            Sys::About => PredefinedMenuItem::about(
                Some("About only3d"),
                Some(AboutMetadata {
                    name: Some("only3d".into()),
                    version: Some(env!("CARGO_PKG_VERSION").into()),
                    comments: Some("3D modelling, slicing and printing for the Bambu Lab H2D, built for AI agents.".into()),
                    license: Some("AGPL-3.0-or-later".into()),
                    website: Some("https://github.com/onatyap/only3d".into()),
                    ..Default::default()
                }),
            ),
            Sys::Services => PredefinedMenuItem::services(None),
            Sys::Hide => PredefinedMenuItem::hide(Some("Hide only3d")),
            Sys::HideOthers => PredefinedMenuItem::hide_others(None),
            Sys::ShowAll => PredefinedMenuItem::show_all(None),
            Sys::Quit => PredefinedMenuItem::quit(Some("Quit only3d")),
            Sys::Minimize => PredefinedMenuItem::minimize(None),
            Sys::Zoom => PredefinedMenuItem::maximize(Some("Zoom")),
            Sys::FullScreen => PredefinedMenuItem::fullscreen(Some("Enter Full Screen")),
            Sys::BringAllToFront => PredefinedMenuItem::bring_all_to_front(None),
        }
    }

    /// The key equivalent for a chord, or `None` when AppKit can't show it.
    pub fn accelerator(ch: &Chord) -> Option<KeyAccelerator> {
        if !ch.is_command() && !plain_keys() {
            return None;
        }
        use eframe::egui::Key as K;
        let key = match ch.key {
            K::ArrowUp => Key::Named(NamedKey::ArrowUp),
            K::ArrowDown => Key::Named(NamedKey::ArrowDown),
            K::ArrowLeft => Key::Named(NamedKey::ArrowLeft),
            K::ArrowRight => Key::Named(NamedKey::ArrowRight),
            K::Enter => Key::Named(NamedKey::Enter),
            K::Tab => Key::Named(NamedKey::Tab),
            K::Backspace => Key::Named(NamedKey::Backspace),
            K::Delete => Key::Named(NamedKey::Delete),
            K::Escape => Key::Named(NamedKey::Escape),
            K::Home => Key::Named(NamedKey::Home),
            K::End => Key::Named(NamedKey::End),
            K::PageUp => Key::Named(NamedKey::PageUp),
            K::PageDown => Key::Named(NamedKey::PageDown),
            K::F1 => Key::Named(NamedKey::F1),
            K::F2 => Key::Named(NamedKey::F2),
            K::F3 => Key::Named(NamedKey::F3),
            K::F4 => Key::Named(NamedKey::F4),
            K::F5 => Key::Named(NamedKey::F5),
            K::F6 => Key::Named(NamedKey::F6),
            K::F7 => Key::Named(NamedKey::F7),
            K::F8 => Key::Named(NamedKey::F8),
            K::F9 => Key::Named(NamedKey::F9),
            K::F10 => Key::Named(NamedKey::F10),
            K::F11 => Key::Named(NamedKey::F11),
            K::F12 => Key::Named(NamedKey::F12),
            K::Space => Key::Character(" ".into()),
            k => {
                let s = k.symbol_or_name();
                if s.chars().count() != 1 {
                    return None;
                }
                Key::Character(s.to_lowercase())
            }
        };
        let mut mods = Modifiers::empty();
        if ch.mods.cmd {
            mods |= Modifiers::META;
        }
        if ch.mods.ctrl {
            mods |= Modifiers::CONTROL;
        }
        if ch.mods.alt {
            mods |= Modifiers::ALT;
        }
        if ch.mods.shift {
            mods |= Modifiers::SHIFT;
        }
        Some(KeyAccelerator::new(mods, key))
    }

    impl Native {
        pub fn build(ctx: eframe::egui::Context) -> Native {
            let (tx, rx) = channel::<String>();
            let tx = std::sync::Mutex::new(tx);
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                if let Ok(tx) = tx.lock() {
                    let _ = tx.send(e.id().0.clone());
                }
                ctx.request_repaint();
            }));
            let menu = Menu::new();
            let mut items = Vec::new();
            let mut panels = Vec::new();
            let recent = Submenu::new("Open Recent", true);
            let plates = Submenu::new("Move to Plate", true);
            let mut window_menu = None;
            let mut help_menu = None;
            let mut app_menu = None;

            fn fill(parent: &Submenu, entries: &[Entry], items: &mut Vec<Item>, panels: &mut Vec<(PanelId, CheckMenuItem, Option<bool>)>, recent: &Submenu, plates: &Submenu) {
                for e in entries {
                    let r: muda::Result<()> = match e {
                        Entry::Act(a, label) => {
                            let it = MenuItem::with_id(a.id(), label, true, None);
                            let r = parent.append(&it);
                            items.push(Item { action: *a, kind: Kind::Plain(it), label: label.clone(), last: None });
                            r
                        }
                        Entry::Check(a, label) => {
                            let it = CheckMenuItem::with_id(a.id(), label, true, false, None);
                            let r = parent.append(&it);
                            items.push(Item { action: *a, kind: Kind::Check(it), label: label.clone(), last: None });
                            r
                        }
                        Entry::Sub(label, es) => {
                            let s = Submenu::new(label, true);
                            fill(&s, es, items, panels, recent, plates);
                            parent.append(&s)
                        }
                        Entry::Sep => parent.append(&PredefinedMenuItem::separator()),
                        Entry::Sys(s) => parent.append(&sys_item(*s)),
                        Entry::Recent => parent.append(recent),
                        Entry::MoveToPlate => parent.append(plates),
                        Entry::Panel(p) => {
                            let it = CheckMenuItem::with_id(Cmd::TogglePanel(*p).id(), p.label(), true, false, None);
                            let r = parent.append(&it);
                            panels.push((*p, it, None));
                            r
                        }
                    };
                    if let Err(err) = r {
                        eprintln!("only3d: menu: {err}");
                    }
                }
            }

            for top in spec() {
                let s = Submenu::new(top.title, true);
                fill(&s, &top.entries, &mut items, &mut panels, &recent, &plates);
                if let Err(err) = menu.append(&s) {
                    eprintln!("only3d: menu: {err}");
                }
                match top.title {
                    "Window" => window_menu = Some(s),
                    "Help" => help_menu = Some(s),
                    _ if app_menu.is_none() => app_menu = Some(s),
                    _ => {}
                }
            }
            menu.init_for_nsapp();
            // AppKit adds the open windows to the Window menu and a search field to Help
            if let Some(w) = &window_menu {
                w.set_as_windows_menu_for_nsapp();
            }
            if let Some(h) = &help_menu {
                h.set_as_help_menu_for_nsapp();
            }
            let mut n = Native { _menu: menu, app_menu, title_fixes: 0, items, recent, recent_now: vec!["-".into()], plates, plates_now: None, panels, rx };
            n.set_recent(&[]);
            n
        }

        pub fn take(&self) -> Vec<Cmd> {
            self.rx
                .try_iter()
                .filter_map(|id| {
                    if log_picks() {
                        eprintln!("only3d: menu pick {id}");
                    }
                    Cmd::parse(&id)
                })
                .collect()
        }

        pub fn sync(&mut self, state: &dyn Fn(Action) -> ItemState) {
            if self.title_fixes < 40 {
                if let Some(m) = &self.app_menu {
                    m.set_text("only3d");
                }
                self.title_fixes += 1;
            }
            for it in &mut self.items {
                let s = state(it.action);
                if it.last.as_ref() == Some(&s) {
                    continue;
                }
                let prev = it.last.take();
                let text = s.text.clone().unwrap_or_else(|| it.label.clone());
                let accel = s.chord.as_ref().and_then(accelerator);
                match &it.kind {
                    Kind::Plain(m) => {
                        if prev.as_ref().map(|p| p.enabled) != Some(s.enabled) {
                            m.set_enabled(s.enabled);
                        }
                        if prev.as_ref().map(|p| &p.chord) != Some(&s.chord) {
                            let _ = m.set_key_accelerator(accel);
                        }
                        if prev.as_ref().map(|p| &p.text) != Some(&s.text) {
                            m.set_text(&text);
                        }
                    }
                    Kind::Check(m) => {
                        if prev.as_ref().map(|p| p.enabled) != Some(s.enabled) {
                            m.set_enabled(s.enabled);
                        }
                        if prev.as_ref().map(|p| p.checked) != Some(s.checked) || m.is_checked() != s.checked {
                            m.set_checked(s.checked);
                        }
                        if prev.as_ref().map(|p| &p.chord) != Some(&s.chord) {
                            let _ = m.set_key_accelerator(accel);
                        }
                        if prev.as_ref().map(|p| &p.text) != Some(&s.text) {
                            m.set_text(&text);
                        }
                    }
                }
                it.last = Some(s);
            }
        }

        fn clear(s: &Submenu) {
            while s.remove_at(0).is_some() {}
        }

        pub fn set_recent(&mut self, paths: &[String]) {
            if self.recent_now == paths {
                return;
            }
            self.recent_now = paths.to_vec();
            Self::clear(&self.recent);
            let home = std::env::var("HOME").unwrap_or_default();
            for p in paths {
                let name = std::path::Path::new(p).file_name().map(|f| f.to_string_lossy().trim_end_matches(".only3d.json").trim_end_matches(".json").to_string()).unwrap_or_else(|| p.clone());
                let dir = std::path::Path::new(p).parent().map(|d| d.display().to_string()).unwrap_or_default();
                let dir = if !home.is_empty() && dir.starts_with(&home) { format!("~{}", &dir[home.len()..]) } else { dir };
                let _ = self.recent.append(&MenuItem::with_id(Cmd::OpenRecent(p.clone()).id(), format!("{name}  —  {dir}"), true, None));
            }
            if !paths.is_empty() {
                let _ = self.recent.append(&PredefinedMenuItem::separator());
            }
            let _ = self.recent.append(&MenuItem::with_id(Cmd::ClearRecent.id(), "Clear Menu", !paths.is_empty(), None));
        }

        pub fn set_plates(&mut self, plates: &[(u32, String)], enabled: bool) {
            let now = (plates.to_vec(), enabled);
            if self.plates_now.as_ref() == Some(&now) {
                return;
            }
            self.plates_now = Some(now);
            Self::clear(&self.plates);
            self.plates.set_enabled(enabled);
            for (k, name) in plates {
                let _ = self.plates.append(&MenuItem::with_id(Cmd::MoveToPlate(*k).id(), name, true, None));
            }
            if !plates.is_empty() {
                let _ = self.plates.append(&PredefinedMenuItem::separator());
            }
            let _ = self.plates.append(&MenuItem::with_id(Cmd::MoveToNewPlate.id(), "New Plate", true, None));
        }

        pub fn set_panels(&mut self, showing: &dyn Fn(PanelId) -> bool) {
            for (p, it, last) in &mut self.panels {
                let on = showing(*p);
                if *last != Some(on) || it.is_checked() != on {
                    it.set_checked(on);
                    *last = Some(on);
                }
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_menu_id_round_trips() {
        for a in actions() {
            assert_eq!(Cmd::parse(&Cmd::Run(a).id()), Some(Cmd::Run(a)), "{}", a.id());
        }
        for c in [Cmd::OpenRecent("/tmp/x y.only3d.json".into()), Cmd::ClearRecent, Cmd::MoveToPlate(3), Cmd::MoveToNewPlate, Cmd::TogglePanel(PanelId::Spools)] {
            assert_eq!(Cmd::parse(&c.id()), Some(c.clone()));
        }
    }

    #[test]
    fn menus_cover_the_actions() {
        let shown = actions();
        // only the arrow-key nudges and the dark/light flip stay keyboard / palette only
        let keyboard_only = |a: &Action| {
            matches!(
                a,
                Action::MoveLeft | Action::MoveRight | Action::MoveUp | Action::MoveDown | Action::MoveLeftBig | Action::MoveRightBig | Action::MoveUpBig | Action::MoveDownBig | Action::TogglePreview | Action::ToggleTheme | Action::ShowPanel(_)
            )
        };
        let missing: Vec<String> = Action::all().into_iter().filter(|a| !keyboard_only(a) && !shown.contains(a)).map(|a| a.id()).collect();
        assert!(missing.is_empty(), "not in any menu: {missing:?}");
    }

    #[test]
    fn no_action_twice_in_one_menu() {
        for top in spec() {
            let mut seen = std::collections::HashSet::new();
            fn walk(es: &[Entry], seen: &mut std::collections::HashSet<String>, title: &str) {
                for e in es {
                    match e {
                        Entry::Act(a, _) | Entry::Check(a, _) => assert!(seen.insert(a.id()), "{} twice in {title}", a.id()),
                        Entry::Sub(_, es) => walk(es, seen, title),
                        _ => {}
                    }
                }
            }
            walk(&top.entries, &mut seen, top.title);
        }
    }

    #[test]
    fn labels_are_title_case() {
        assert_eq!(title_case("Plate with M3 holes"), "Plate with M3 Holes");
        assert_eq!(title_case("Rounded box"), "Rounded Box");
    }
}
