//! Saving and exporting, in the app's own look.
//!
//! **Save** (⌘S, Save As) is just a name and a folder.
//!
//! **Export** (⇧⌘E, File ▸ Export, right-click ▸ Export Selection) starts from what you want
//! to do, like Keynote's or Figma's export:
//!
//! - **Tabs by purpose:** For printing, 3D model, Picture, Project. Each shows its own formats
//!   as cards with a line on what they are for, one marked Recommended.
//! - **A preview** of what goes out (rendered by only3d), with its count and size.
//! - **Export:** the selection, this plate or all plates, with counts; choices that can't apply
//!   say why on hover.
//! - **One checkbox** for a separate file per object (or per plate, for all plates).
//! - **Options** only where they matter (STL binary or text; picture size and view; whether the
//!   plate is sliced), and warnings from the checks (objects that aren't watertight).
//! - **Save as … in …**, "Show in Finder afterwards", and a button that says what happens.
//!
//! The panel only picks; the work is the usual tool call (`save_project`, `export`,
//! `export_bambu_project`, `preset_export`), so agents can do all of it too.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use eframe::egui::{self, Rect, RichText, Sense};
use serde_json::{json, Value};

use crate::app::{App, Dialog};
use crate::theme::{self, pal, Icon};
use crate::worker::Kind;

/// Where the panel was opened from.
#[derive(Clone, Debug)]
pub enum SaveWhat {
    Project,
    /// An export, starting on this extension (stl, 3mf, scad, gcode.3mf).
    Export(&'static str),
    /// An export starting on the last format used.
    ExportLast,
    /// An export of the selected objects.
    Selection,
    BambuProject,
    Preset { kind: String, name: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Print,
    Model,
    Picture,
    Project,
}

impl Tab {
    const ALL: [(Tab, &'static str, Icon); 4] = [(Tab::Print, "For printing", Icon::Printer), (Tab::Model, "3D model", Icon::Cube), (Tab::Picture, "Picture", Icon::FileImage), (Tab::Project, "Project", Icon::Folder)];

    fn formats(self) -> &'static [Format] {
        match self {
            Tab::Print => &[Format::PrintFile, Format::Bambu, Format::Gcode],
            Tab::Model => &[Format::ThreeMf, Format::Stl, Format::Obj, Format::Scad],
            Tab::Picture => &[Format::Png],
            Tab::Project => &[Format::Zip, Format::ProjectCopy],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    PrintFile,
    Bambu,
    Gcode,
    ThreeMf,
    Stl,
    Obj,
    Scad,
    Png,
    Zip,
    ProjectCopy,
}

impl Format {
    const ALL: [Format; 10] = [Format::PrintFile, Format::Bambu, Format::Gcode, Format::ThreeMf, Format::Stl, Format::Obj, Format::Scad, Format::Png, Format::Zip, Format::ProjectCopy];

    fn tab(self) -> Tab {
        match self {
            Format::PrintFile | Format::Bambu | Format::Gcode => Tab::Print,
            Format::ThreeMf | Format::Stl | Format::Obj | Format::Scad => Tab::Model,
            Format::Png => Tab::Picture,
            Format::Zip | Format::ProjectCopy => Tab::Project,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Format::PrintFile => "print_file",
            Format::Bambu => "bambu",
            Format::Gcode => "gcode",
            Format::ThreeMf => "3mf",
            Format::Stl => "stl",
            Format::Obj => "obj",
            Format::Scad => "scad",
            Format::Png => "png",
            Format::Zip => "zip",
            Format::ProjectCopy => "project",
        }
    }

    fn from_id(s: &str) -> Option<Format> {
        Format::ALL.into_iter().find(|f| f.id() == s)
    }

    fn title(self) -> &'static str {
        match self {
            Format::PrintFile => "Sliced plate for Bambu printers",
            Format::Bambu => "Bambu Studio project",
            Format::Gcode => "Plain G-code",
            Format::ThreeMf => "3MF",
            Format::Stl => "STL",
            Format::Obj => "OBJ",
            Format::Scad => "OpenSCAD",
            Format::Png => "PNG picture",
            Format::Zip => "Project ZIP",
            Format::ProjectCopy => "only3d project",
        }
    }

    fn blurb(self) -> &'static str {
        match self {
            Format::PrintFile => "Ready to print on the H2D; holds colours and the plate picture",
            Format::Bambu => "Opens in Bambu Studio, for tree supports or painting",
            Format::Gcode => "For other printers and tools",
            Format::ThreeMf => "Keeps objects, parts and colours; best for slicers",
            Format::Stl => "One mesh that opens almost anywhere; no colours",
            Format::Obj => "For Blender and other 3D apps",
            Format::Scad => "Editable code with your parameters",
            Format::Png => "A render of what you export",
            Format::Zip => "The project and every model it imports, to share or archive",
            Format::ProjectCopy => "Save the project under a new name; you carry on in the new file",
        }
    }

    fn recommended(self) -> bool {
        matches!(self, Format::PrintFile | Format::ThreeMf | Format::Png | Format::Zip)
    }

    pub fn ext(self) -> &'static str {
        match self {
            Format::PrintFile => "gcode.3mf",
            Format::Bambu | Format::ThreeMf => "3mf",
            Format::Gcode => "gcode",
            Format::Stl => "stl",
            Format::Obj => "obj",
            Format::Scad => "scad",
            Format::Png => "png",
            Format::Zip => "zip",
            Format::ProjectCopy => "only3d.json",
        }
    }

    fn from_ext(e: &str) -> Format {
        match e {
            "stl" => Format::Stl,
            "obj" => Format::Obj,
            "scad" => Format::Scad,
            "png" => Format::Png,
            "gcode.3mf" => Format::PrintFile,
            "gcode" => Format::Gcode,
            "zip" => Format::Zip,
            _ => Format::ThreeMf,
        }
    }

    /// The whole project, whatever is selected.
    fn whole(self) -> bool {
        matches!(self, Format::Bambu | Format::Zip | Format::ProjectCopy)
    }

    /// One plate per file (several plates: one file each).
    fn one_plate_per_file(self) -> bool {
        matches!(self, Format::PrintFile | Format::Gcode | Format::Png | Format::Scad)
    }

    /// A whole plate, never part of one.
    fn plates_only(self) -> bool {
        matches!(self, Format::PrintFile | Format::Gcode)
    }

    /// What the export button says.
    fn button(self, files: usize) -> String {
        let ext = match self {
            Format::Png => return if files > 1 { format!("Save {files} Pictures") } else { "Save Picture".into() },
            Format::ProjectCopy => return "Save As".into(),
            Format::PrintFile => "Print File",
            Format::Bambu => "Bambu Project",
            Format::Gcode => "G-code",
            Format::ThreeMf => "3MF",
            Format::Stl => "STL",
            Format::Obj => "OBJ",
            Format::Scad => "OpenSCAD",
            Format::Zip => "ZIP",
        };
        if files > 1 {
            format!("Export {files} {ext} Files")
        } else {
            format!("Export {ext}")
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    Selection,
    Plate,
    AllPlates,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Mode {
    /// ⌘S / Save As: a name and a folder.
    Save,
    Export,
    Preset { kind: String, name: String },
}

pub struct SaveDialog {
    pub mode: Mode,
    pub format: Format,
    pub scope: Scope,
    /// A file per object (per plate when exporting all plates).
    pub separate: bool,
    pub ascii: bool,
    pub pic_size: u32,
    pub pic_view: &'static str,
    pub name: String,
    pub folder: PathBuf,
    pub places: Vec<(String, PathBuf)>,
    pub focus: bool,
    pub reveal: bool,
    /// The preview: what it shows (a hash of the choices) and the picture.
    pub preview_key: u64,
    pub preview: Option<egui::TextureHandle>,
    pub preview_loading: bool,
}

impl SaveDialog {
    /// Why `scope` can't be used with this format and selection, if it can't.
    fn scope_block(&self, s: Scope, n_sel: usize, plates: u32) -> Option<&'static str> {
        match s {
            Scope::Selection if n_sel == 0 => Some("Nothing is selected"),
            Scope::Selection if self.format.plates_only() => Some("A print file holds a whole plate"),
            Scope::AllPlates if plates < 2 => Some("This project has one plate"),
            _ => None,
        }
    }

    /// The separate-files box: (label, forced value if it can't be changed).
    fn separate_rule(&self, objects: usize) -> (&'static str, Option<bool>) {
        if self.scope == Scope::AllPlates {
            ("A separate file for each plate", if self.format.one_plate_per_file() { Some(true) } else { None })
        } else if self.format.plates_only() || objects < 2 {
            ("A separate file for each object", Some(false))
        } else {
            ("A separate file for each object", None)
        }
    }

    /// Keep the choices consistent after one changed.
    fn settle(&mut self, n_sel: usize, plates: u32, objects: usize) {
        if self.scope_block(self.scope, n_sel, plates).is_some() {
            self.scope = Scope::Plate;
        }
        if let (_, Some(v)) = self.separate_rule(objects) {
            self.separate = v;
        }
    }

    /// The `export` tool's arguments for these choices.
    fn export_args(&self, path: &str, selection: &[String]) -> Value {
        let mut a = json!({ "path": path });
        if self.scope == Scope::Selection {
            a["ids"] = json!(selection);
        }
        match (self.scope == Scope::AllPlates, self.separate) {
            (true, true) => a["each"] = json!("plate"),
            (true, false) => a["all_plates"] = json!(true),
            (false, true) => a["each"] = json!("object"),
            (false, false) => {}
        }
        if self.format == Format::Stl && self.ascii {
            a["ascii"] = json!(true);
        }
        if self.format == Format::Png {
            a["size"] = json!(self.pic_size);
            a["view"] = json!(self.pic_view);
        }
        a
    }
}

/// `name` without the ending (and without characters a file name can't have).
fn base_name(name: &str, ext: &str) -> String {
    let n = name.trim().replace(['/', ':'], "-");
    let lower = n.to_lowercase();
    let dot = format!(".{ext}");
    if lower.ends_with(&dot) {
        n[..n.len() - dot.len()].to_string()
    } else if ext == "only3d.json" && lower.ends_with(".json") {
        n[..n.len() - 5].to_string()
    } else {
        n
    }
}

fn safe(n: &str) -> String {
    n.trim().replace(['/', '\\', ':'], "-")
}

/// An object in reach of an export.
struct Item {
    name: String,
    plate: u32,
    watertight: bool,
    bounds: only3d_geom::Aabb,
}

impl App {
    /// Open the panel, starting on `what`.
    pub(crate) fn save_panel(&mut self, what: SaveWhat) {
        let project_dir = self.snap.project_path.as_ref().and_then(|p| std::fs::canonicalize(p).ok()).and_then(|p| p.parent().map(Path::to_path_buf));
        let open_dir = self.workspace_root();
        let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let mut places: Vec<(String, PathBuf)> = Vec::new();
        let add = |label: String, p: PathBuf, places: &mut Vec<(String, PathBuf)>| {
            if p.is_dir() && !places.iter().any(|(_, q)| *q == p) {
                places.push((label, p));
            }
        };
        let dir_name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string());
        if let Some(d) = &project_dir {
            add(format!("{}  (this project)", dir_name(d)), d.clone(), &mut places);
        }
        if let Some(d) = &open_dir {
            add(format!("{}  (open folder)", dir_name(d)), d.clone(), &mut places);
        }
        if let Some(d) = self.cfg.last_save_dir.as_ref().map(PathBuf::from) {
            add(format!("{}  (last used)", dir_name(&d)), d, &mut places);
        }
        for r in self.cfg.recent_projects.clone() {
            if let Some(d) = Path::new(&r).parent() {
                add(dir_name(d), d.to_path_buf(), &mut places);
            }
            if places.len() >= 7 {
                break;
            }
        }
        for sub in ["Desktop", "Documents", "Downloads"] {
            add(sub.to_string(), home.join(sub), &mut places);
        }
        let mode = match &what {
            SaveWhat::Project => Mode::Save,
            SaveWhat::Preset { kind, name } => Mode::Preset { kind: kind.clone(), name: name.clone() },
            _ => Mode::Export,
        };
        let last = self.cfg.export_format.as_deref().and_then(Format::from_id);
        let format = match &what {
            SaveWhat::Export(e) => Format::from_ext(e),
            SaveWhat::ExportLast => last.unwrap_or(Format::ThreeMf),
            SaveWhat::Selection => last.filter(|f| f.tab() == Tab::Model).unwrap_or(Format::ThreeMf),
            SaveWhat::BambuProject => Format::Bambu,
            _ => Format::ProjectCopy,
        };
        let folder = self.cfg.last_save_dir.as_ref().map(PathBuf::from).filter(|d| d.is_dir() && mode != Mode::Save).or(project_dir).or(open_dir).unwrap_or_else(|| home.join("Documents"));
        // the saved file's own name first (so Save As starts from it), else the project's name
        let stem = self.snap.project_path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().trim_end_matches(".only3d.json").trim_end_matches(".json").to_string());
        let project = stem.or_else(|| if self.snap.project.name.is_empty() || self.snap.project.name == "untitled" { None } else { Some(self.snap.project.name.clone()) });
        let name = match &mode {
            Mode::Preset { name, .. } => safe(name),
            Mode::Save => project.unwrap_or_else(|| "untitled".into()),
            Mode::Export => project.unwrap_or_else(|| "plate".into()),
        };
        let n_sel = self.snap.selection.len();
        let scope = if n_sel > 0 && !format.plates_only() { Scope::Selection } else { Scope::Plate };
        let reveal = self.cfg.sections.get("export.reveal").copied().unwrap_or(true);
        let mut d = SaveDialog { mode, format, scope, separate: false, ascii: false, pic_size: 1600, pic_view: "iso", name, folder, places, focus: true, reveal, preview_key: 0, preview: None, preview_loading: false };
        let objects = self.export_items(scope).len();
        d.settle(n_sel, self.snap.project.plate_count(), objects);
        self.dialog = Some(Dialog::Save(d));
    }

    /// The visible objects an export covers.
    fn export_items(&self, scope: Scope) -> Vec<Item> {
        self.snap
            .objects
            .iter()
            .chain(self.snap.others.iter())
            .filter(|o| !o.hidden)
            .filter(|o| match scope {
                Scope::Selection => self.snap.selection.contains(&o.id),
                Scope::Plate => o.plate == self.snap.plate,
                Scope::AllPlates => true,
            })
            .map(|o| Item { name: if o.name.is_empty() { o.id.clone() } else { o.name.clone() }, plate: o.plate, watertight: o.report.watertight, bounds: o.report.bounds })
            .collect()
    }

    /// Draws the panel; true when it should close.
    pub(crate) fn save_dialog_ui(&mut self, ui: &mut egui::Ui, d: &mut SaveDialog, enter: bool) -> bool {
        egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| if d.mode == Mode::Export { self.export_body(ui, d, enter) } else { self.save_body(ui, d, enter) }).inner
    }

    /// ⌘S / Save As and presets: a name and a folder.
    fn save_body(&mut self, ui: &mut egui::Ui, d: &mut SaveDialog, enter: bool) -> bool {
        let p = pal();
        ui.set_width(460.0);
        let (title, ext) = match &d.mode {
            Mode::Preset { .. } => ("Export preset", "json"),
            _ => ("Save project", "only3d.json"),
        };
        ui.label(RichText::new(title).family(theme::medium()).size(15.0));
        ui.add_space(14.0);
        let name_ok = self.save_row(ui, d, ext, false);
        let path = d.folder.join(format!("{}.{ext}", base_name(&d.name, ext)));
        if path.exists() {
            ui.add_space(6.0);
            ui.label(RichText::new("A file with this name is already there; saving replaces it.").size(12.0).color(p.warning));
        }
        ui.add_space(16.0);
        let ok = !base_name(&d.name, ext).is_empty() && d.folder.is_dir();
        let mut close = false;
        let mut go = ok && enter && name_ok;
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                close = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = match &d.mode {
                    Mode::Preset { .. } => "Export",
                    _ if path.exists() => "Replace",
                    _ => "Save",
                };
                if ui.add_enabled(ok, theme::primary_button(label)).clicked() {
                    go = true;
                }
            });
        });
        if go {
            let s = path.display().to_string();
            match &d.mode {
                Mode::Preset { kind, name } => self.worker.call("preset_export", json!({ "kind": kind, "name": name, "path": s }), Kind::Report),
                _ => self.worker.call("save_project", json!({ "path": s }), Kind::Save),
            }
            self.cfg.last_save_dir = Some(d.folder.display().to_string());
            self.save_ui_now();
            close = true;
        }
        close
    }

    /// Save as [name] .ext in [folder ▾]. Returns whether Enter left the name field.
    fn save_row(&mut self, ui: &mut egui::Ui, d: &mut SaveDialog, ext: &str, per_object: bool) -> bool {
        let p = pal();
        let mut entered = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Save as").size(12.5).color(p.muted));
            if per_object {
                ui.label(RichText::new("each object's name").size(12.5));
                ui.label(RichText::new(format!(".{ext}")).size(12.5).color(p.placeholder));
            }
            let r = if per_object { ui.allocate_response(egui::Vec2::ZERO, Sense::hover()) } else { ui.add(egui::TextEdit::singleline(&mut d.name).desired_width(180.0)) };
            if d.focus && !per_object {
                r.request_focus();
                // select the name so typing replaces it
                if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), r.id) {
                    st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(d.name.chars().count()))));
                    st.store(ui.ctx(), r.id);
                }
                d.focus = false;
            }
            entered = r.lost_focus();
            if !per_object {
                ui.label(RichText::new(format!(".{ext}")).size(12.5).color(p.placeholder));
            }
            ui.label(RichText::new("in").size(12.5).color(p.muted));
            let current = d.places.iter().find(|(_, q)| *q == d.folder).map(|(l, _)| l.clone()).unwrap_or_else(|| d.folder.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| d.folder.display().to_string()));
            let mut other = false;
            egui::ComboBox::from_id_salt("o3d_save_where").width(170.0).selected_text(current).show_ui(ui, |ui| {
                for (label, path) in d.places.clone() {
                    ui.selectable_value(&mut d.folder, path.clone(), label).on_hover_text(crate::chrome::tilde(&path));
                }
                ui.separator();
                if ui.button("Other folder…").clicked() {
                    other = true;
                }
            })
            .response
            .on_hover_text(crate::chrome::tilde(&d.folder));
            if other {
                if let Some(dir) = rfd::FileDialog::new().set_title("Choose a folder").set_directory(&d.folder).pick_folder() {
                    let label = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| dir.display().to_string());
                    if !d.places.iter().any(|(_, q)| *q == dir) {
                        d.places.insert(0, (label, dir.clone()));
                    }
                    d.folder = dir;
                }
            }
        });
        entered
    }

    fn export_body(&mut self, ui: &mut egui::Ui, d: &mut SaveDialog, enter: bool) -> bool {
        let p = pal();
        let mut close = false;
        let n_sel = self.snap.selection.len();
        let plates = self.snap.project.plate_count();
        ui.set_width(640.0);
        ui.label(RichText::new("Export").family(theme::medium()).size(15.0));
        ui.add_space(10.0);

        // tabs by purpose
        let tab = d.format.tab();
        let (bar, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 32.0), Sense::hover());
        ui.painter().rect_filled(bar, 6.0, p.bg);
        let w = (bar.width() - 6.0) / 4.0;
        for (k, (t, label, icon)) in Tab::ALL.iter().enumerate() {
            let r = Rect::from_min_size(bar.min + egui::vec2(3.0 + w * k as f32, 3.0), egui::vec2(w, 26.0));
            let resp = ui.interact(r, egui::Id::unique(("o3d_export_tab", k)), Sense::click());
            let on = *t == tab;
            if on {
                ui.painter().rect(r, 5.0, p.elevated, egui::Stroke::new(1.0, p.border_variant), egui::StrokeKind::Inside);
            } else if resp.hovered() {
                ui.painter().rect_filled(r, 5.0, p.hover);
            }
            let g = ui.painter().layout_no_wrap(label.to_string(), egui::FontId::proportional(12.5), if on { p.text } else { p.muted });
            let x0 = r.center().x - (g.size().x + 20.0) * 0.5;
            theme::paint_icon(ui.painter(), Rect::from_center_size(egui::pos2(x0 + 7.0, r.center().y), egui::vec2(13.0, 13.0)), *icon, if on { p.accent } else { p.placeholder });
            ui.painter().galley(egui::pos2(x0 + 20.0, r.center().y - g.size().y * 0.5), g, p.text);
            if resp.clicked() && !on {
                d.format = t.formats()[0];
            }
        }
        ui.add_space(14.0);

        let items = self.export_items(if d.format.whole() { Scope::AllPlates } else { d.scope });
        d.settle(n_sel, plates, items.len());

        // the preview: a render of what goes out, refreshed when the choices change
        let view = if d.format == Format::Png { d.pic_view } else { "iso" };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (d.scope, d.format.whole(), view, &self.snap.selection, self.snap.plate, self.snap.revision).hash(&mut h);
        let key = h.finish();
        if key != d.preview_key {
            d.preview_key = key;
            d.preview_loading = true;
            let mut args = json!({ "width": 400, "height": 300, "view": view });
            if d.scope == Scope::Selection && !d.format.whole() {
                args["ids"] = json!(self.snap.selection);
            }
            self.read_tool("render", args, Kind::ExportPreview(key));
        }

        let mut go = false;
        ui.horizontal_top(|ui| {
            // left: the preview and what it holds
            ui.vertical(|ui| {
                ui.set_width(210.0);
                let (r, _) = ui.allocate_exact_size(egui::vec2(210.0, 158.0), Sense::hover());
                ui.painter().rect_filled(r, 8.0, p.bg);
                match &d.preview {
                    Some(t) => {
                        let s = t.size_vec2();
                        let k = (r.width() / s.x).min(r.height() / s.y);
                        ui.painter().image(t.id(), Rect::from_center_size(r.center(), s * k), Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                    }
                    None if d.preview_loading => {
                        ui.put(Rect::from_center_size(r.center(), egui::vec2(18.0, 18.0)), egui::Spinner::new().color(p.accent));
                    }
                    None => {}
                }
                let info = if d.format.whole() {
                    format!("{} · {plates} plate{} · {} object{}", self.project_name().trim_end_matches(".only3d.json"), if plates == 1 { "" } else { "s" }, items.len(), if items.len() == 1 { "" } else { "s" })
                } else {
                    let where_ = match d.scope {
                        Scope::Selection => "Selection".to_string(),
                        Scope::Plate => self.snap.project.plate_name(self.snap.plate),
                        Scope::AllPlates => format!("{plates} plates"),
                    };
                    let mut bb = only3d_geom::Aabb::EMPTY;
                    for it in items.iter().filter(|i| i.plate == self.snap.plate) {
                        bb = bb.union(&it.bounds);
                    }
                    let size = if bb.is_empty() || d.scope == Scope::AllPlates { String::new() } else { format!(" · {:.0} × {:.0} × {:.0} mm", bb.max.x - bb.min.x, bb.max.y - bb.min.y, bb.max.z - bb.min.z) };
                    format!("{where_} · {} object{}{size}", items.len(), if items.len() == 1 { "" } else { "s" })
                };
                ui.add_space(6.0);
                ui.label(RichText::new(info).size(11.5).color(p.muted));
                if d.scope == Scope::AllPlates && !d.format.whole() {
                    ui.label(RichText::new(format!("{} shown", self.snap.project.plate_name(self.snap.plate))).size(11.0).color(p.placeholder));
                }
            });
            ui.add_space(16.0);
            // right: format, what, options, warnings
            ui.vertical(|ui| {
                // as tall as the tallest tab, so the tabs don't jump when you switch
                ui.set_min_height(372.0);
                ui.label(RichText::new("Format").size(12.0).color(p.muted));
                ui.add_space(2.0);
                for f in tab.formats() {
                    let on = *f == d.format;
                    let r = egui::Frame::new()
                        .fill(if on { theme::soft(p.accent) } else { p.bg })
                        .stroke(egui::Stroke::new(if on { 1.5 } else { 1.0 }, if on { p.accent } else { p.border_variant }))
                        .corner_radius(6)
                        .inner_margin(egui::Margin::symmetric(10, 7))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                let (dot, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), Sense::hover());
                                ui.painter().circle_stroke(dot.center(), 5.5, egui::Stroke::new(1.5, if on { p.accent } else { p.placeholder }));
                                if on {
                                    ui.painter().circle_filled(dot.center(), 3.0, p.accent);
                                }
                                ui.label(RichText::new(f.title()).size(13.0).family(theme::medium()));
                                ui.label(RichText::new(format!(".{}", f.ext())).size(11.5).color(p.placeholder));
                                if f.recommended() {
                                    theme::chip(ui, "Recommended", p.accent, theme::soft(p.accent));
                                }
                            });
                            ui.label(RichText::new(f.blurb()).size(11.5).color(p.muted));
                        })
                        .response
                        .interact(Sense::click());
                    if r.clicked() {
                        d.format = *f;
                        d.settle(n_sel, plates, items.len());
                    }
                    ui.add_space(4.0);
                }
                ui.add_space(6.0);
                if !d.format.whole() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Export").size(12.5).color(p.muted));
                        let count = |s: Scope| self.export_items(s).len();
                        let label = |s: Scope| match s {
                            Scope::Selection => format!("Selection ({n_sel} object{})", if n_sel == 1 { "" } else { "s" }),
                            Scope::Plate => format!("{} ({} object{})", self.snap.project.plate_name(self.snap.plate), count(Scope::Plate), if count(Scope::Plate) == 1 { "" } else { "s" }),
                            Scope::AllPlates => format!("All plates ({plates} plates, {} objects)", count(Scope::AllPlates)),
                        };
                        let current = label(d.scope);
                        let mut pick = None;
                        egui::ComboBox::from_id_salt("o3d_export_scope").width(260.0).selected_text(current).show_ui(ui, |ui| {
                            for s in [Scope::Selection, Scope::Plate, Scope::AllPlates] {
                                let block = d.scope_block(s, n_sel, plates);
                                let r = ui.add_enabled(block.is_none(), egui::Button::selectable(d.scope == s, label(s)));
                                let r = match block {
                                    Some(why) => r.on_disabled_hover_text(why),
                                    None => r,
                                };
                                if r.clicked() {
                                    pick = Some(s);
                                }
                            }
                        });
                        if let Some(s) = pick {
                            d.scope = s;
                            let n = self.export_items(s).len();
                            d.settle(n_sel, plates, n);
                        }
                    });
                    // the box only when it is a real choice
                    match d.separate_rule(items.len()) {
                        (label, None) => {
                            ui.checkbox(&mut d.separate, RichText::new(label).size(12.5));
                        }
                        (_, Some(true)) => {
                            ui.label(RichText::new("One file per plate: this format holds a single plate").size(12.0).color(p.muted));
                        }
                        (_, Some(false)) => {}
                    }
                }
                // options only where they matter
                match d.format {
                    Format::Stl => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("STL").size(12.5).color(p.muted));
                            if ui.add(egui::Button::selectable(!d.ascii, "Binary")).on_hover_text("Small; what slicers expect").clicked() {
                                d.ascii = false;
                            }
                            if ui.add(egui::Button::selectable(d.ascii, "Text")).on_hover_text("Larger, but readable and diffable").clicked() {
                                d.ascii = true;
                            }
                        });
                    }
                    Format::Png => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Size").size(12.5).color(p.muted));
                            for (w, t) in [(800u32, "Small"), (1600, "Medium"), (2048, "Large")] {
                                if ui.add(egui::Button::selectable(d.pic_size == w, t)).on_hover_text(format!("{w} × {} px", w * 3 / 4)).clicked() {
                                    d.pic_size = w;
                                }
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("View").size(12.5).color(p.muted));
                            for (v, t) in [("iso", "Iso"), ("front", "Front"), ("top", "Top"), ("left", "Left"), ("right", "Right")] {
                                if ui.add(egui::Button::selectable(d.pic_view == v, t)).clicked() {
                                    d.pic_view = v;
                                }
                            }
                        });
                    }
                    Format::PrintFile | Format::Gcode => {
                        let (text, col) = match (&self.snap.last_slice, self.snap.slice_current) {
                            (Some(sl), true) => (format!("Sliced · {} · {:.1} g", crate::app::fmt_duration(sl.result.stats.print_time_s), sl.result.stats.filament_g), p.success),
                            _ => ("Slices first: the plate changed since the last slice".to_string(), p.muted),
                        };
                        let text = if d.scope == Scope::AllPlates { "Each plate is sliced in turn".to_string() } else { text };
                        ui.label(RichText::new(text).size(12.0).color(col));
                    }
                    _ => {}
                }
                // warnings from the checks
                let leaky = items.iter().filter(|i| !i.watertight).count();
                if leaky > 0 && matches!(d.format.tab(), Tab::Model | Tab::Print) {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        theme::paint_icon(ui.painter(), Rect::from_min_size(ui.cursor().min + egui::vec2(0.0, 2.0), egui::vec2(12.0, 12.0)), Icon::Info, p.warning);
                        ui.add_space(16.0);
                        let what = if leaky == 1 { "1 object isn't watertight".to_string() } else { format!("{leaky} objects aren't watertight") };
                        ui.label(RichText::new(format!("{what}; some apps may refuse it.")).size(12.0).color(p.warning));
                    });
                }
            });
        });

        // the files it writes
        let ext = d.format.ext();
        let base = base_name(&d.name, ext);
        let per_object = !d.format.whole() && d.separate && d.scope != Scope::AllPlates;
        let targets: Vec<PathBuf> = if d.format.whole() || !d.separate {
            vec![d.folder.join(format!("{base}.{ext}"))]
        } else if d.scope == Scope::AllPlates {
            let mut ks: Vec<u32> = items.iter().map(|i| i.plate).collect();
            ks.sort_unstable();
            ks.dedup();
            ks.into_iter().map(|k| d.folder.join(format!("{base} - {}.{ext}", safe(&self.snap.project.plate_name(k))))).collect()
        } else {
            items.iter().map(|i| d.folder.join(format!("{}.{ext}", safe(&i.name)))).collect()
        };
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);
        let entered = self.save_row(ui, d, ext, per_object);
        let replaces = targets.iter().filter(|t| t.exists()).count();
        if per_object || targets.len() > 1 {
            let names: Vec<String> = targets.iter().take(3).filter_map(|t| t.file_name().map(|n| n.to_string_lossy().to_string())).collect();
            ui.label(RichText::new(format!("Writes {}{}", names.join(", "), if targets.len() > 3 { format!(" and {} more", targets.len() - 3) } else { String::new() })).size(11.5).color(p.placeholder));
        }
        if replaces > 0 {
            ui.label(RichText::new(if replaces == 1 { "Replaces a file that is already there.".to_string() } else { format!("Replaces {replaces} files that are already there.") }).size(12.0).color(p.warning));
        }
        let nothing = !d.format.whole() && items.is_empty();
        if nothing {
            ui.label(RichText::new("Nothing to export here: no visible objects.").size(12.0).color(p.warning));
        }
        ui.add_space(12.0);
        let ok = !nothing && d.folder.is_dir() && (per_object || !base.is_empty());
        ui.horizontal(|ui| {
            if ui.checkbox(&mut d.reveal, RichText::new(format!("{} afterwards", crate::os::show_in())).size(12.0)).changed() {
                self.cfg.sections.insert("export.reveal".into(), d.reveal);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(ok, theme::primary_button(d.format.button(targets.len()))).clicked() {
                    go = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if ok && enter && entered {
            go = true;
        }
        if go {
            let path = d.folder.join(format!("{base}.{ext}")).display().to_string();
            match d.format {
                Format::ProjectCopy => self.worker.call("save_project", json!({ "path": path }), Kind::Save),
                Format::Bambu => self.worker.call("export_bambu_project", json!({ "path": path }), Kind::Exported(d.reveal)),
                Format::Zip => self.worker.call("export", json!({ "path": path }), Kind::Exported(d.reveal)),
                _ => {
                    let args = d.export_args(&path, &self.snap.selection);
                    self.worker.call("export", args, Kind::Exported(d.reveal));
                }
            }
            self.cfg.export_format = Some(d.format.id().to_string());
            self.cfg.last_save_dir = Some(d.folder.display().to_string());
            self.save_ui_now();
            close = true;
        }
        close
    }

    /// The preview render for the open panel.
    pub(crate) fn export_preview_done(&mut self, key: u64, png: Option<Vec<u8>>) {
        let tex = png.and_then(|b| crate::approvals::decode(&b, "image/png").ok()).map(|img| self.egui.load_texture("o3d-export-preview", img, egui::TextureOptions::LINEAR));
        if let Some(Dialog::Save(d)) = &mut self.dialog {
            if d.preview_key == key {
                d.preview = tex;
                d.preview_loading = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog(format: Format, scope: Scope) -> SaveDialog {
        SaveDialog { mode: Mode::Export, format, scope, separate: false, ascii: false, pic_size: 1600, pic_view: "iso", name: "x".into(), folder: PathBuf::new(), places: Vec::new(), focus: false, reveal: true, preview_key: 0, preview: None, preview_loading: false }
    }

    #[test]
    fn names_lose_their_ending_once() {
        assert_eq!(base_name("tray.only3d.json", "only3d.json"), "tray");
        assert_eq!(base_name("tray.json", "only3d.json"), "tray");
        assert_eq!(base_name("plate.gcode.3mf", "gcode.3mf"), "plate");
        assert_eq!(base_name("a/b", "stl"), "a-b");
    }

    #[test]
    fn every_tab_has_a_recommended_format_and_formats_know_their_tab() {
        for (t, _, _) in Tab::ALL {
            assert!(t.formats().iter().any(|f| f.recommended()));
            assert!(t.formats().iter().all(|f| f.tab() == t));
        }
        assert!(Format::ALL.iter().all(|f| Format::from_id(f.id()) == Some(*f)));
    }

    #[test]
    fn choices_settle_and_map_to_the_tool() {
        // a print file can't be part of a plate, and several plates are one file each
        let mut d = dialog(Format::PrintFile, Scope::Selection);
        d.settle(2, 2, 3);
        assert_eq!(d.scope, Scope::Plate);
        d.scope = Scope::AllPlates;
        d.settle(2, 2, 3);
        assert!(d.separate, "a print file per plate");
        assert_eq!(d.export_args("/x/p.gcode.3mf", &[])["each"], "plate");
        // STL: all plates in one file, or one per object of the selection
        let mut d = dialog(Format::Stl, Scope::AllPlates);
        d.settle(0, 2, 4);
        assert!(!d.separate);
        assert_eq!(d.export_args("/x/a.stl", &[])["all_plates"], true);
        d.scope = Scope::Selection;
        d.separate = true;
        d.ascii = true;
        let a = d.export_args("/x/a.stl", &["a".into(), "b".into()]);
        assert_eq!((a["each"].as_str(), a["ids"].as_array().map(Vec::len), a["ascii"].as_bool()), (Some("object"), Some(2), Some(true)));
        // one object: nothing to separate
        let mut d = dialog(Format::ThreeMf, Scope::Plate);
        d.separate = true;
        d.settle(0, 1, 1);
        assert!(!d.separate);
        // a picture carries its size and view
        let mut d = dialog(Format::Png, Scope::Plate);
        d.pic_view = "top";
        assert_eq!(d.export_args("/x/p.png", &[])["view"], "top");
        assert_eq!(Format::Stl.button(3), "Export 3 STL Files");
        assert_eq!(Format::Png.button(1), "Save Picture");
    }
}
