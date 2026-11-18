//! The Plates, Files and Prints panels (VS Code explorer-style trees): the project's plates and
//! their objects, the workspace folder as a lazy file tree, and recent prints. Each is a panel of
//! its own, so they can be docked, stacked or hidden independently. Every action goes through the same tools agents use (`plate`, `select`,
//! `update_object`, `import_file`, `open_project`, `workspace`, `print_detail`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Layout, RichText, Sense, Ui};
use only3d_studio::workspace::{Entry, FileKind};
use serde_json::{json, Value};

use crate::app::{file_drop_zone, App, Dialog, DragFiles, DragObjects, PRIMITIVES};
use crate::dock::PanelId;
use crate::keymap::Action;
use crate::panels::{muted, scroll, swatch};
use crate::settings_ui::short;
use crate::theme::{self, pal, Icon};
use crate::worker::Kind;

const ROW_H: f32 = 24.0;

/// Keeps the Files panel current without redrawing an idle window: a background thread lists
/// the folders on screen every few seconds and wakes the window only when one changed.
#[derive(Clone, Default)]
pub struct FolderWatch(std::sync::Arc<std::sync::Mutex<WatchState>>);

#[derive(Default)]
struct WatchState {
    started: bool,
    /// (folder, every kind of file) → (when the panel last showed it, its listing as text)
    dirs: std::collections::HashMap<(PathBuf, bool), (Instant, String)>,
    changed: Vec<PathBuf>,
}

fn fingerprint(l: &Result<Vec<Entry>, String>) -> String {
    format!("{l:?}")
}

impl FolderWatch {
    fn state(&self) -> std::sync::MutexGuard<'_, WatchState> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The panel shows `dir`, listed as `l` (`None`: from its cache, nothing new).
    fn shown(&self, ctx: &egui::Context, dir: &Path, all: bool, l: Option<&Result<Vec<Entry>, String>>) {
        let mut g = self.state();
        let e = g.dirs.entry((dir.to_path_buf(), all)).or_insert_with(|| (Instant::now(), String::new()));
        e.0 = Instant::now();
        if let Some(l) = l {
            e.1 = fingerprint(l);
        }
        if !g.started {
            g.started = true;
            let (me, ctx) = (self.clone(), ctx.clone());
            let _ = std::thread::Builder::new().name("only3d-folder-watch".into()).spawn(move || me.run(ctx));
        }
    }

    /// Folders whose listing changed since the panel read them.
    fn take_changed(&self) -> Vec<PathBuf> {
        std::mem::take(&mut self.state().changed)
    }

    fn run(&self, ctx: egui::Context) {
        loop {
            std::thread::sleep(Duration::from_secs(3));
            // folders off screen for a while are dropped; the panel lists them again when shown
            let todo: Vec<(PathBuf, bool, String)> = {
                let mut g = self.state();
                g.dirs.retain(|_, (seen, _)| seen.elapsed() < Duration::from_secs(20));
                g.dirs.iter().map(|((d, all), (_, f))| (d.clone(), *all, f.clone())).collect()
            };
            let mut woke = false;
            for (dir, all, before) in todo {
                let now = fingerprint(&only3d_studio::workspace::list(&dir, all).map_err(|e| e.message));
                if now != before {
                    let mut g = self.state();
                    if let Some(e) = g.dirs.get_mut(&(dir.clone(), all)) {
                        e.1 = now;
                    }
                    g.changed.push(dir);
                    woke = true;
                }
            }
            if woke {
                ctx.request_repaint();
            }
        }
    }
}

impl App {
    pub(crate) fn plates_panel(&mut self, ui: &mut Ui) {
        let rect = ui.max_rect();
        scroll(ui, "plates", |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            self.plates_section(ui);
        });
        // files from the Files panel let go anywhere else here: onto the active plate
        if let Some(paths) = file_drop_zone(ui, rect) {
            self.import_files_onto(&paths, None, None);
        }
    }

    pub(crate) fn files_panel(&mut self, ui: &mut Ui) {
        scroll(ui, "files", |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            self.folder_section(ui);
        });
    }

    pub(crate) fn prints_panel(&mut self, ui: &mut Ui) {
        scroll(ui, "prints", |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            self.prints_section(ui);
        });
    }

    // ------------------------------------------------------------------------------------------
    // plates → objects

    fn plates_section(&mut self, ui: &mut Ui) {
        let name = if self.snap.project.name.is_empty() { "untitled".to_string() } else { self.snap.project.name.clone() };
        let mut add_plate = false;
        let mut import = false;
        let mut primitive: Option<(&str, &str)> = None;
        let t = self.keymap.label_for(Action::Import).map(|k| format!("Import STL / 3MF / OBJ / SCAD onto this plate  {k}")).unwrap_or_default();
        let open = true;
        self.panel_bar(ui, &format!("Project · {name}"), |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            if theme::icon_button(ui, Icon::Grid, "Add a plate").clicked() {
                add_plate = true;
            }
            if theme::icon_button(ui, Icon::Import, &t).clicked() {
                import = true;
            }
            let add = theme::icon_button(ui, Icon::Plus, "Add a primitive to this plate");
            egui::Popup::menu(&add).show(|ui| {
                for (name, shape) in PRIMITIVES {
                    if ui.button(name).clicked() {
                        primitive = Some((name, shape));
                        ui.close();
                    }
                }
            });
        });
        if add_plate {
            self.worker.call("plate", json!({"action": "add"}), Kind::Plain);
        }
        if import {
            self.import_dialog();
        }
        if let Some((name, shape)) = primitive {
            self.add_primitive(name, shape);
        }
        if !open {
            return;
        }
        if self.snap.project.objects.len() > 6 || !self.ws.query.is_empty() {
            ui.add(egui::TextEdit::singleline(&mut self.ws.query).hint_text("Filter objects").desired_width(f32::INFINITY));
            ui.add_space(2.0);
        }
        let q = self.ws.query.trim().to_lowercase();
        let n = self.snap.project.plate_count();
        let mods = ui.input(|i| i.modifiers);
        let mut calls: Vec<(&'static str, Value, Kind)> = Vec::new();
        let mut click: Option<(String, u32)> = None;
        let mut rename: Option<String> = None;
        for k in 1..=n {
            let objs: Vec<only3d_model::Object> = self
                .snap
                .project
                .objects
                .iter()
                .filter(|o| o.plate == k)
                .filter(|o| q.is_empty() || o.id.to_lowercase().contains(&q) || o.name.to_lowercase().contains(&q))
                .cloned()
                .collect();
            if !q.is_empty() && objs.is_empty() {
                continue;
            }
            let total = self.snap.project.objects.iter().filter(|o| o.plate == k).count();
            let folded = self.ws.folded.contains(&k) && q.is_empty();
            let active = k == self.snap.plate;
            // the plate row: click works on it, the chevron folds it, objects can be dropped on it
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::click());
            let p = pal();
            let drop = resp.dnd_hover_payload::<DragObjects>().is_some() || resp.dnd_hover_payload::<DragFiles>().is_some_and(|f| f.all_models());
            if drop {
                ui.painter().rect(rect, 3.0, theme::soft(p.accent), egui::Stroke::new(1.0, p.accent), egui::StrokeKind::Inside);
            } else if active {
                ui.painter().rect_filled(rect, 3.0, p.hover);
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, 3.0, p.hover.gamma_multiply(0.6));
            }
            let chev = egui::Rect::from_center_size(egui::pos2(rect.min.x + 9.0, rect.center().y), egui::vec2(12.0, 12.0));
            theme::paint_icon(ui.painter(), chev, if folded { Icon::ChevronRight } else { Icon::ChevronDown }, p.placeholder);
            let icon = egui::Rect::from_center_size(egui::pos2(rect.min.x + 25.0, rect.center().y), egui::vec2(13.0, 13.0));
            theme::paint_icon(ui.painter(), icon, Icon::Grid, if active { p.accent } else { p.muted });
            let pname = self.snap.project.plate_name(k);
            ui.painter().text(egui::pos2(rect.min.x + 38.0, rect.center().y), egui::Align2::LEFT_CENTER, &pname, egui::FontId::new(12.5, if active { theme::medium() } else { egui::FontFamily::Proportional }), if active { p.text } else { p.text.gamma_multiply(0.9) });
            ui.painter().text(egui::pos2(rect.max.x - 8.0, rect.center().y), egui::Align2::RIGHT_CENTER, total.to_string(), egui::FontId::monospace(10.5), p.placeholder);
            // like a layer's eye: show or hide this plate in the all-plates view (⌥: only this one)
            let mut eye_hit = false;
            if n > 1 && self.cfg.show_all_plates {
                let hidden = self.plate_hidden.contains(&k) && !active;
                let er = egui::Rect::from_center_size(egui::pos2(rect.max.x - 30.0, rect.center().y), egui::vec2(18.0, 18.0));
                let eresp = ui.interact(er, egui::Id::unique(("plate_eye", k)), Sense::click());
                if hidden || eresp.hovered() || resp.hovered() || active {
                    theme::paint_icon(ui.painter(), er.shrink(3.0), if hidden { Icon::EyeOff } else { Icon::Eye }, if active { p.placeholder.gamma_multiply(0.6) } else if eresp.hovered() { p.text } else { p.placeholder });
                }
                let alt = crate::keymap::ALT;
                let tip = if active { "The active plate is always shown".to_string() } else if hidden { format!("Show this plate in the all-plates view ({alt}-click: only this one)") } else { format!("Hide this plate in the all-plates view ({alt}-click: only this one)") };
                if eresp.on_hover_text(tip).clicked() {
                    eye_hit = true;
                    if mods.alt {
                        // solo: only this plate (and the active one) stay visible; again = all
                        let others: std::collections::BTreeSet<u32> = (1..=n).filter(|x| *x != k).collect();
                        if self.plate_hidden == others {
                            self.plate_hidden.clear();
                        } else {
                            self.plate_hidden = others;
                        }
                    } else if !active && !self.plate_hidden.remove(&k) {
                        self.plate_hidden.insert(k);
                    }
                }
            }
            if let Some(ids) = resp.dnd_release_payload::<DragObjects>() {
                calls.push(("plate", json!({"action": "move", "plate": k, "ids": ids.0}), Kind::Plain));
            }
            if let Some(files) = resp.dnd_release_payload::<DragFiles>().filter(|f| f.all_models()) {
                if k != self.snap.plate {
                    calls.push(("plate", json!({"action": "select", "plate": k}), Kind::Plain));
                }
                for f in &files.0 {
                    calls.push(("import_file", json!({"path": f.display().to_string()}), Kind::AddObject));
                }
            }
            if resp.clicked() && !eye_hit {
                let on_chevron = resp.interact_pointer_pos().is_some_and(|p| p.x < rect.min.x + 18.0);
                if on_chevron || active {
                    if !self.ws.folded.remove(&k) {
                        self.ws.folded.insert(k);
                    }
                } else {
                    calls.push(("plate", json!({"action": "select", "plate": k}), Kind::Plain));
                    self.ws.folded.remove(&k);
                }
            }
            if resp.double_clicked() {
                self.plate_rename_start(k);
            }
            let resp = resp.on_hover_text(format!("{pname}: {total} object{} · click to work on it · drop objects here to move them, or model files to import them", if total == 1 { "" } else { "s" }));
            resp.context_menu(|ui| {
                if ui.button("Rename…").clicked() {
                    self.plate_rename_start(k);
                    ui.close();
                }
                if ui.button("Duplicate plate").clicked() {
                    calls.push(("plate", json!({"action": "duplicate", "plate": k}), Kind::Plain));
                    ui.close();
                }
                if ui.button("Arrange this plate").clicked() {
                    if !active {
                        calls.push(("plate", json!({"action": "select", "plate": k}), Kind::Plain));
                    }
                    calls.push(("arrange", json!({}), Kind::Plain));
                    ui.close();
                }
                if ui.button("Add a plate").clicked() {
                    calls.push(("plate", json!({"action": "add"}), Kind::Plain));
                    ui.close();
                }
                if n > 1 {
                    ui.separator();
                    let to = if k == 1 { 2 } else { 1 };
                    if total > 0 && ui.button(format!("Remove, keep objects on {}", self.snap.project.plate_name(to))).clicked() {
                        calls.push(("plate", json!({"action": "remove", "plate": k, "to": to}), Kind::Plain));
                        ui.close();
                    }
                    if ui.button(if total > 0 { format!("Remove with its {total} object(s)") } else { "Remove plate".into() }).clicked() {
                        calls.push(("plate", json!({"action": "remove", "plate": k}), Kind::Plain));
                        ui.close();
                    }
                }
            });
            if folded {
                continue;
            }
            if objs.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(38.0);
                    ui.label(muted("Empty: add a primitive (+), import a model, or drop objects here."));
                });
                continue;
            }
            for o in objs {
                self.object_row(ui, &o, n, &mut calls, &mut click, &mut rename);
                if !o.parts.is_empty() {
                    self.part_rows(ui, &o, &mut calls);
                }
            }
        }
        for (tool, args, kind) in calls {
            self.worker.call(tool, args, kind);
        }
        if let Some((id, plate)) = click {
            // several objects can be selected on one plate; another plate starts afresh
            if (mods.shift || mods.command) && plate == self.snap.plate {
                let mut sel = self.snap.selection.clone();
                if let Some(i) = sel.iter().position(|x| *x == id) {
                    sel.remove(i);
                } else {
                    sel.push(id);
                }
                self.select(sel);
            } else {
                self.select(vec![id]);
            }
        }
        if let Some(id) = rename {
            let text = self.snap.project.object(&id).map(|o| if o.name.is_empty() { o.id.clone() } else { o.name.clone() }).unwrap_or_default();
            self.select(vec![id.clone()]);
            self.dialog = Some(Dialog::Rename { id, text, focus: true });
        }
    }

    pub(crate) fn plate_rename_start(&mut self, k: u32) {
        self.plate_rename = Some((k, self.snap.project.plate_name(k)));
    }

    #[allow(clippy::too_many_arguments)]
    fn object_row(&mut self, ui: &mut Ui, o: &only3d_model::Object, plates: u32, calls: &mut Vec<(&'static str, Value, Kind)>, click: &mut Option<(String, u32)>, rename: &mut Option<String>) {
        let p = pal();
        let id = o.id.clone();
        let selected = self.snap.selection.contains(&id);
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::click_and_drag());
        let hovered = resp.hovered() || resp.dragged();
        if selected {
            ui.painter().rect_filled(rect, 3.0, p.selected);
            ui.painter().rect_filled(egui::Rect::from_min_size(rect.min + egui::vec2(20.0, 5.0), egui::vec2(2.0, ROW_H - 10.0)), 1.0, p.accent);
        } else if hovered {
            ui.painter().rect_filled(rect, 3.0, p.hover);
        }
        if resp.drag_started() {
            let ids = if selected { self.snap.selection.clone() } else { vec![id.clone()] };
            resp.dnd_set_drag_payload(DragObjects(ids));
        }
        let mut row = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(8.0, 0.0))).layout(Layout::left_to_right(Align::Center)));
        {
            let ui = &mut row;
            ui.add_space(22.0);
            swatch(ui, self.snap.filament_color(o.filament).gamma_multiply(if o.hidden { 0.4 } else { 1.0 }), 10.0);
            let name = if o.name.is_empty() { o.id.clone() } else { o.name.clone() };
            let col = if o.hidden { p.placeholder } else if selected { p.text } else { p.text.gamma_multiply(0.92) };
            let label = ui.add(egui::Label::new(RichText::new(&name).size(12.5).color(col)).truncate().selectable(false));
            if name != o.id {
                label.on_hover_text(&o.id);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                if hovered || selected {
                    if theme::icon_button(ui, Icon::Trash, "Delete").clicked() {
                        calls.push(("remove_object", json!({"ids": [id]}), Kind::Plain));
                    }
                    if theme::icon_button(ui, Icon::Copy, "Duplicate").clicked() {
                        calls.push(("duplicate_object", json!({"id": id}), Kind::Plain));
                    }
                }
                if hovered || selected || o.locked {
                    let tip = if o.locked { "Locked: it stays put (click to unlock)" } else { "Lock it in place" };
                    let r = theme::icon_button(ui, Icon::Lock, tip);
                    if r.clicked() {
                        calls.push(("update_object", json!({"id": id, "locked": !o.locked}), Kind::Plain));
                    }
                }
                if hovered || selected || o.hidden {
                    let eye = if o.hidden { Icon::EyeOff } else { Icon::Eye };
                    if theme::icon_button(ui, eye, if o.hidden { "Show" } else { "Hide" }).clicked() {
                        calls.push(("update_object", json!({"id": id, "hidden": !o.hidden}), Kind::Plain));
                    }
                }
                if !(hovered || selected) {
                    if let Some(ev) = self.snap.evaluated(&id) {
                        let s = ev.report.size;
                        ui.label(RichText::new(format!("{:.0}×{:.0}×{:.0}", s.x, s.y, s.z)).size(10.5).monospace().color(p.placeholder));
                    }
                }
            });
        }
        if resp.clicked() {
            *click = Some((id.clone(), o.plate));
        }
        if resp.double_clicked() {
            *rename = Some(id.clone());
        }
        resp.context_menu(|ui| {
            if !selected {
                *click = Some((id.clone(), o.plate));
            }
            if ui.button("Rename…").clicked() {
                *rename = Some(id.clone());
                ui.close();
            }
            if ui.button("Duplicate").clicked() {
                calls.push(("duplicate_object", json!({"id": id}), Kind::Plain));
                ui.close();
            }
            if ui.button(if o.hidden { "Show" } else { "Hide" }).clicked() {
                calls.push(("update_object", json!({"id": id, "hidden": !o.hidden}), Kind::Plain));
                ui.close();
            }
            if ui.button(if o.locked { "Unlock" } else { "Lock in place" }).clicked() {
                calls.push(("update_object", json!({"id": id, "locked": !o.locked}), Kind::Plain));
                ui.close();
            }
            ui.menu_button("Filament", |ui| {
                for s in 1..=self.snap.project.filaments.len() as u32 {
                    let f = &self.snap.project.filaments[s as usize - 1];
                    if ui.button(format!("{s} · {}", short(&f.preset))).clicked() {
                        calls.push(("update_object", json!({"id": id, "filament": s}), Kind::Plain));
                        ui.close();
                    }
                }
            });
            ui.menu_button("Move to plate", |ui| {
                let ids = if selected { self.snap.selection.clone() } else { vec![id.clone()] };
                for k in (1..=plates).filter(|k| *k != o.plate) {
                    if ui.button(self.snap.project.plate_name(k)).clicked() {
                        calls.push(("plate", json!({"action": "move", "plate": k, "ids": ids}), Kind::Plain));
                        ui.close();
                    }
                }
                if ui.button("New plate").clicked() {
                    calls.push(("plate", json!({"action": "add"}), Kind::Plain));
                    calls.push(("plate", json!({"action": "move", "plate": plates + 1, "ids": ids}), Kind::Plain));
                    ui.close();
                }
            });
            if ui.button("Settings for this object…").clicked() {
                self.settings_object = true;
                self.cfg.layout.show(PanelId::Settings);
                ui.close();
            }
            ui.separator();
            if ui.button("Delete").clicked() {
                calls.push(("remove_object", json!({"ids": [id]}), Kind::Plain));
                ui.close();
            }
        });
    }

    /// The bodies of a multi-part object, under its row: name and filament, which can change.
    fn part_rows(&mut self, ui: &mut Ui, o: &only3d_model::Object, calls: &mut Vec<(&'static str, Value, Kind)>) {
        let p = pal();
        let bodies: Vec<(String, u32)> = std::iter::once(("body".to_string(), o.filament)).chain(o.parts.iter().enumerate().map(|(k, x)| (if x.name.is_empty() { format!("part {}", k + 1) } else { x.name.clone() }, x.filament))).collect();
        for (k, (name, filament)) in bodies.into_iter().enumerate() {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(rect, 3.0, p.hover);
            }
            let x = rect.min.x + 44.0;
            ui.painter().line_segment([egui::pos2(x - 8.0, rect.min.y), egui::pos2(x - 8.0, rect.center().y)], egui::Stroke::new(1.0, p.border));
            ui.painter().line_segment([egui::pos2(x - 8.0, rect.center().y), egui::pos2(x - 2.0, rect.center().y)], egui::Stroke::new(1.0, p.border));
            let sw = egui::Rect::from_center_size(egui::pos2(x + 5.0, rect.center().y), egui::vec2(8.0, 8.0));
            ui.painter().rect_filled(sw, 2.0, self.snap.filament_color(filament));
            let clip = egui::Rect::from_min_max(egui::pos2(x + 12.0, rect.min.y), egui::pos2(rect.max.x - 72.0, rect.max.y));
            ui.painter().with_clip_rect(clip).text(egui::pos2(x + 14.0, rect.center().y), egui::Align2::LEFT_CENTER, &name, egui::FontId::proportional(11.5), p.muted);
            ui.painter().text(egui::pos2(rect.max.x - 8.0, rect.center().y), egui::Align2::RIGHT_CENTER, format!("filament {filament}"), egui::FontId::monospace(10.0), p.placeholder);
            let resp = resp.on_hover_text("A part with its own filament · right-click to change it");
            let mut set: Option<u32> = None;
            resp.context_menu(|ui| {
                ui.label(RichText::new(&name).size(11.0).color(p.placeholder));
                for f in 1..=self.snap.project.filaments.len() as u32 {
                    let fs = &self.snap.project.filaments[f as usize - 1];
                    if ui.button(format!("Filament {f} · {}", short(&fs.preset))).clicked() {
                        set = Some(f);
                        ui.close();
                    }
                }
            });
            if let Some(f) = set {
                if k == 0 {
                    calls.push(("update_object", json!({"id": o.id, "filament": f}), Kind::Plain));
                } else {
                    let mut parts = o.parts.clone();
                    parts[k - 1].filament = f;
                    calls.push(("update_object", json!({"id": o.id, "parts": parts}), Kind::Plain));
                }
            }
        }
    }

    // ------------------------------------------------------------------------------------------
    // the workspace folder

    /// The folder the user opened, if any. Nothing is opened for them: like a code editor's
    /// explorer, the panel offers "Open Folder…" until one is chosen.
    pub(crate) fn workspace_root(&mut self) -> Option<PathBuf> {
        if let Some((t, r)) = &self.ws.root {
            if t.elapsed() < Duration::from_secs(3) {
                return r.clone();
            }
        }
        let r = only3d_studio::workspace::saved_root();
        self.ws.root = Some((Instant::now(), r.clone()));
        r
    }

    /// Every file in the Files panel, or only projects, models, print files and images.
    fn files_only_3d(&self) -> bool {
        self.cfg.sections.get("files.only_3d").copied().unwrap_or(false)
    }

    /// A thumbnail of a PNG or JPEG for the hover card, decoded off the UI thread and cached.
    fn image_preview(&mut self, ui: &mut Ui, path: &Path) {
        let p = pal();
        let lower = path.to_string_lossy().to_lowercase();
        let mime = if lower.ends_with(".png") {
            "image/png"
        } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
            "image/jpeg"
        } else {
            ui.label(RichText::new("No preview for this format: double-click opens it in Preview.").size(11.0).color(p.placeholder));
            return;
        };
        if self.ws.thumbs.len() > 64 {
            self.ws.thumbs.clear();
        }
        let slot = self.ws.thumbs.entry(path.to_path_buf()).or_insert_with(|| {
            let out: std::sync::Arc<std::sync::Mutex<Option<Result<egui::ColorImage, String>>>> = Default::default();
            let (o, path, ctx) = (out.clone(), path.to_path_buf(), ui.ctx().clone());
            std::thread::spawn(move || {
                let r = std::fs::metadata(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|m| if m.len() > 40_000_000 { Err("too large to preview".to_string()) } else { std::fs::read(&path).map_err(|e| e.to_string()) })
                    .and_then(|b| crate::approvals::decode(&b, mime))
                    .map(|img| shrink(img, 256));
                *o.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                ctx.request_repaint();
            });
            Thumb::Loading(out)
        });
        if let Thumb::Loading(out) = slot {
            let done = out.lock().unwrap_or_else(|e| e.into_inner()).take();
            match done {
                Some(Ok(img)) => *slot = Thumb::Ready(ui.ctx().load_texture(format!("thumb {}", path.display()), img, egui::TextureOptions::LINEAR)),
                Some(Err(e)) => *slot = Thumb::Failed(e),
                None => {}
            }
        }
        match slot {
            Thumb::Ready(t) => {
                ui.add(egui::Image::new((t.id(), t.size_vec2())).corner_radius(4));
            }
            Thumb::Failed(e) => {
                ui.label(RichText::new(format!("No preview: {e}")).size(11.0).color(p.placeholder));
            }
            Thumb::Loading(_) => {
                ui.add(egui::Spinner::new().size(14.0).color(p.accent));
            }
        }
    }

    /// File > Open Folder… and the panel's button.
    pub(crate) fn open_folder_dialog(&mut self) {
        let start = self.workspace_root().or_else(|| self.snap.project_path.as_ref().and_then(|p| p.parent()).map(Path::to_path_buf));
        let mut d = rfd::FileDialog::new().set_title("Open a folder");
        if let Some(s) = start.filter(|s| s.is_dir()) {
            d = d.set_directory(s);
        }
        if let Some(dir) = d.pick_folder() {
            self.open_folder(&dir);
        }
    }

    fn open_folder(&mut self, dir: &Path) {
        self.worker.call("workspace", json!({"set_root": dir.display().to_string()}), Kind::Workspace);
        self.cfg.layout.show(crate::dock::PanelId::Files);
    }

    /// No folder open: what VS Code's explorer shows, with this project's folder and recent ones
    /// one click away.
    fn no_folder(&mut self, ui: &mut Ui) {
        let p = pal();
        self.panel_bar(ui, "No folder open", |_| {});
        egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
            ui.label(RichText::new("Open the folder you keep your projects and models in. They show up here, and agents can find them too.").size(12.0).color(p.muted));
            ui.add_space(8.0);
            let w = ui.available_width();
            if ui.add(theme::primary_button("Open Folder…").min_size(egui::vec2(w, 26.0))).clicked() {
                self.open_folder_dialog();
            }
            // this project's folder, then the folders of recent projects
            let mut dirs: Vec<PathBuf> = Vec::new();
            let here = self.snap.project_path.as_ref().and_then(|p| dunce::canonicalize(p).ok()).and_then(|p| p.parent().map(Path::to_path_buf));
            for d in here.into_iter().chain(self.cfg.recent_projects.iter().filter_map(|r| Path::new(r).parent().map(Path::to_path_buf))) {
                if d.is_dir() && !dirs.contains(&d) {
                    dirs.push(d);
                }
                if dirs.len() == 4 {
                    break;
                }
            }
            if !dirs.is_empty() {
                ui.add_space(10.0);
                ui.label(RichText::new("Recent").size(11.0).color(p.placeholder));
                let mut pick = None;
                for d in &dirs {
                    let name = d.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| d.display().to_string());
                    let r = ui.add(egui::Label::new(RichText::new(&name).size(12.5).color(p.accent)).sense(Sense::click())).on_hover_text(crate::chrome::tilde(d));
                    if r.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if r.clicked() {
                        pick = Some(d.clone());
                    }
                }
                if let Some(d) = pick {
                    self.open_folder(&d);
                }
            }
        });
    }

    /// A folder's entries, cached until the folder watcher sees it change.
    fn listing(&mut self, dir: &Path) -> Result<Vec<Entry>, String> {
        let all = !self.files_only_3d();
        if let Some((_, l)) = self.ws.listings.get(dir) {
            let l = l.clone();
            self.ws.watch.shown(&self.egui, dir, all, None);
            return l;
        }
        let l = only3d_studio::workspace::list(dir, all).map_err(|e| e.message);
        self.ws.listings.insert(dir.to_path_buf(), (Instant::now(), l.clone()));
        self.ws.watch.shown(&self.egui, dir, all, Some(&l));
        l
    }

    fn folder_section(&mut self, ui: &mut Ui) {
        for d in self.ws.watch.take_changed() {
            self.ws.listings.remove(&d);
        }
        let Some(root) = self.workspace_root() else {
            self.no_folder(ui);
            return;
        };
        let title = root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| root.display().to_string());
        let mut pick = false;
        let mut refresh = false;
        let mut close = false;
        let only = self.files_only_3d();
        let mut flip_only = false;
        let open = true;
        self.panel_bar(ui, &format!("Folder · {title}"), |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            if theme::icon_button(ui, Icon::Reset, "Refresh").clicked() {
                refresh = true;
            }
            if theme::icon_button(ui, Icon::Folder, "Open another folder…").clicked() {
                pick = true;
            }
            if theme::icon_toggle(ui, Icon::Cube, if only { "Showing only 3D files: click to show every file" } else { "Show only 3D files (projects, models, print files, images)" }, only).clicked() {
                flip_only = true;
            }
            if theme::icon_button(ui, Icon::Close, "Close the folder").clicked() {
                close = true;
            }
        });
        if close {
            self.worker.call("workspace", json!({"set_root": ""}), Kind::Workspace);
            return;
        }
        if flip_only {
            self.cfg.sections.insert("files.only_3d".into(), !only);
            refresh = true;
        }
        if refresh {
            self.ws.listings.clear();
        }
        if pick {
            self.open_folder_dialog();
        }
        if !open {
            return;
        }
        let mut actions: Vec<(Entry, &'static str)> = Vec::new();
        self.folder_rows(ui, &root, 0, &mut actions);
        for (e, what) in actions {
            if what == "preview" || what == "keep" {
                self.open_file_tab(&ui.ctx().clone(), &e.path, e.kind, what == "keep", None);
            } else {
                self.file_action(&e, what);
            }
        }
    }

    fn folder_rows(&mut self, ui: &mut Ui, dir: &Path, depth: usize, actions: &mut Vec<(Entry, &'static str)>) {
        let p = pal();
        let entries = match self.listing(dir) {
            Ok(e) => e,
            Err(e) => {
                ui.label(RichText::new(e).size(11.5).color(p.error));
                return;
            }
        };
        if entries.is_empty() && depth == 0 {
            ui.label(muted("No projects or models here. Open another folder (folder icon above)."));
        }
        let current = self.snap.project_path.clone();
        for e in entries {
            let indent = 6.0 + depth as f32 * 14.0;
            // any file can be dragged: models onto a plate to import, the rest into the main area
            let file = e.kind != FileKind::Folder;
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), if file { Sense::click_and_drag() } else { Sense::click() });
            if file && resp.drag_started() {
                resp.dnd_set_drag_payload(DragFiles(vec![e.path.clone()]));
            }
            let is_current = current.as_deref().is_some_and(|c| c == e.path) || self.editor.active_files().contains(&e.path);
            if is_current {
                ui.painter().rect_filled(rect, 3.0, p.selected);
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, 3.0, p.hover);
            }
            let x = rect.min.x + indent;
            let folder = e.kind == FileKind::Folder;
            let open = folder && self.ws.open_dirs.contains(&e.path);
            if folder {
                theme::paint_icon(ui.painter(), egui::Rect::from_center_size(egui::pos2(x + 5.0, rect.center().y), egui::vec2(11.0, 11.0)), if open { Icon::ChevronDown } else { Icon::ChevronRight }, p.placeholder);
            }
            let (icon, col) = match e.kind {
                FileKind::Folder => (Icon::Folder, p.muted),
                FileKind::Project => (Icon::Cube, p.accent),
                FileKind::Model => (Icon::Cube, p.text.gamma_multiply(0.8)),
                FileKind::Print => (Icon::Printer, p.success),
                FileKind::Image | FileKind::Vector | FileKind::Video => (Icon::FileImage, p.warning.gamma_multiply(0.85)),
                FileKind::Text | FileKind::Document => (Icon::FileText, p.muted),
                FileKind::Data | FileKind::Code => (Icon::FileCode, p.muted),
                FileKind::Archive | FileKind::Other => (Icon::File, p.placeholder),
            };
            theme::paint_icon(ui.painter(), egui::Rect::from_center_size(egui::pos2(x + 19.0, rect.center().y), egui::vec2(13.0, 13.0)), icon, col);
            let three_d = matches!(e.kind, FileKind::Folder | FileKind::Project | FileKind::Model | FileKind::Print);
            let name_col = if three_d { p.text.gamma_multiply(0.92) } else { p.muted };
            let max_w = (rect.max.x - x - 32.0 - 56.0).max(40.0);
            // one line, cut short with … (the full name is in the hover card)
            let mut job = egui::text::LayoutJob::single_section(e.name.clone(), egui::TextFormat::simple(egui::FontId::proportional(12.5), name_col));
            job.wrap = egui::text::TextWrapping::truncate_at_width(max_w);
            let g = ui.painter().layout_job(job);
            ui.painter().galley(egui::pos2(x + 30.0, rect.center().y - g.size().y * 0.5), g, name_col);
            if !folder {
                ui.painter().text(egui::pos2(rect.max.x - 6.0, rect.center().y), egui::Align2::RIGHT_CENTER, only3d_studio::workspace::human_size(e.size), egui::FontId::monospace(10.0), p.placeholder);
            }
            let tip = match e.kind {
                FileKind::Folder => "Click to open · right-click for more",
                FileKind::Project => "Click to preview · double-click to keep it open · right-click to open the project",
                FileKind::Model => "Click to preview · double-click to keep it open · drag onto the plate to import · right-click for more",
                _ => "Click to preview · double-click to keep it open · right-click for more",
            };
            let resp = if e.kind == FileKind::Image {
                let path = e.path.clone();
                resp.on_hover_ui(|ui| {
                    ui.label(format!("{}\n{tip}", crate::chrome::tilde(&path)));
                    self.image_preview(ui, &path);
                })
            } else {
                resp.on_hover_text(format!("{}\n{tip}", crate::chrome::tilde(&e.path)))
            };
            if folder && resp.clicked() && !self.ws.open_dirs.remove(&e.path) {
                self.ws.open_dirs.insert(e.path.clone());
            }
            if !folder && resp.clicked() {
                actions.push((e.clone(), "preview"));
            }
            if !folder && resp.double_clicked() {
                actions.push((e.clone(), "keep"));
            }
            resp.context_menu(|ui| {
                match e.kind {
                    FileKind::Project => {
                        if ui.button("Open project").clicked() {
                            actions.push((e.clone(), "open"));
                            ui.close();
                        }
                    }
                    FileKind::Model => {
                        if ui.button("Import onto this plate").clicked() {
                            actions.push((e.clone(), "open"));
                            ui.close();
                        }
                        if ui.button("Import onto a new plate").clicked() {
                            actions.push((e.clone(), "new_plate"));
                            ui.close();
                        }
                    }
                    FileKind::Folder => {
                        if ui.button("Make this the workspace folder").clicked() {
                            actions.push((e.clone(), "root"));
                            ui.close();
                        }
                    }
                    FileKind::Print => {}
                    _ => {
                        if ui.button("Open").on_hover_text("In its usual app (Preview, a text editor…)").clicked() {
                            actions.push((e.clone(), "open"));
                            ui.close();
                        }
                    }
                }
                if ui.button(crate::os::show_in()).clicked() {
                    actions.push((e.clone(), "reveal"));
                    ui.close();
                }
                if ui.button("Copy path").clicked() {
                    ui.ctx().copy_text(e.path.display().to_string());
                    ui.close();
                }
            });
            if open {
                self.folder_rows(ui, &e.path.clone(), depth + 1, actions);
            }
        }
    }

    fn file_action(&mut self, e: &Entry, what: &str) {
        let path = e.path.display().to_string();
        match (what, e.kind) {
            ("open", FileKind::Project) => {
                self.worker.call("open_project", json!({"path": path}), Kind::Plain);
                self.cam_ready = false;
                self.editor.show_plate();
            }
            ("open", FileKind::Model) => {
                self.worker.call("import_file", json!({"path": path}), Kind::AddObject);
                self.editor.show_plate();
            }
            ("new_plate", _) => {
                self.editor.show_plate();
                self.worker.call("plate", json!({"action": "add"}), Kind::Plain);
                self.worker.call("import_file", json!({"path": path}), Kind::AddObject);
            }
            ("root", _) => self.worker.call("workspace", json!({"set_root": path}), Kind::Workspace),
            ("open", k) if !matches!(k, FileKind::Print | FileKind::Folder) => {
                crate::os::open(&e.path);
            }
            _ => {
                crate::os::reveal(std::path::Path::new(&e.path));
            }
        }
    }

    // ------------------------------------------------------------------------------------------
    // recent prints

    fn prints_section(&mut self, ui: &mut Ui) {
        let p = pal();
        self.panel_bar(ui, "Recent prints", |_| {});
        let stale = self.ws.prints.as_ref().is_none_or(|(t, _)| t.elapsed() > Duration::from_secs(10));
        if stale {
            let rows = only3d_studio::inventory::Db::open_read_only()
                .ok()
                .and_then(|c| {
                    only3d_studio::inventory::query_json(&c, "SELECT p.id, p.job_name, p.state, p.started_at, p.project_version_id AS version, (SELECT ROUND(SUM(grams),1) FROM print_usage u WHERE u.print_id=p.id) AS grams FROM prints p ORDER BY p.started_at DESC LIMIT 12", &[]).ok()
                })
                .unwrap_or_default();
            self.ws.prints = Some((Instant::now(), rows));
        }
        let rows = self.ws.prints.as_ref().map(|(_, r)| r.clone()).unwrap_or_default();
        if rows.is_empty() {
            ui.label(muted("No prints yet. Prints you send from only3d (or that the printer runs) show up here."));
            return;
        }
        for r in rows {
            let id = r["id"].as_str().unwrap_or("").to_string();
            let state = r["state"].as_str().unwrap_or("").to_string();
            let open = self.ws.print_open.as_ref().is_some_and(|(i, _)| *i == id);
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), Sense::click());
            if open || resp.hovered() {
                ui.painter().rect_filled(rect, 3.0, p.hover);
            }
            let col = match state.as_str() {
                "FINISH" => p.success,
                "FAILED" | "CANCELLED" => p.error,
                "RUNNING" | "PAUSE" | "PREPARE" => p.accent,
                _ => p.placeholder,
            };
            ui.painter().circle_filled(egui::pos2(rect.min.x + 10.0, rect.center().y), 3.0, col);
            ui.painter().text(egui::pos2(rect.min.x + 20.0, rect.center().y), egui::Align2::LEFT_CENTER, &id, egui::FontId::monospace(11.0), p.muted);
            let job: String = r["job_name"].as_str().unwrap_or("").chars().take(40).collect();
            ui.painter().with_clip_rect(rect.shrink2(egui::vec2(60.0, 0.0))).text(egui::pos2(rect.min.x + 70.0, rect.center().y), egui::Align2::LEFT_CENTER, job, egui::FontId::proportional(12.0), p.text.gamma_multiply(0.92));
            if let Some(g) = r["grams"].as_f64() {
                ui.painter().text(egui::pos2(rect.max.x - 6.0, rect.center().y), egui::Align2::RIGHT_CENTER, format!("{g:.1} g"), egui::FontId::monospace(10.0), p.placeholder);
            }
            let resp = resp.on_hover_text(format!("{state} · started {} · click for the full trace", r["started_at"].as_str().unwrap_or("")));
            if resp.clicked() {
                if open {
                    self.ws.print_open = None;
                } else {
                    self.read_tool("print_detail", json!({"id": id}), Kind::PrintDetail(id.clone()));
                }
            }
            let version = r["version"].as_i64();
            resp.context_menu(|ui| {
                if let Some(v) = version {
                    if ui.button("Print again…").on_hover_text("Opens the exact project version this was printed from (undo brings back what you had), then the print checklist").clicked() {
                        self.read_tool("project_versions", json!({"version": v}), Kind::Reprint);
                        ui.close();
                    }
                }
                if ui.button("Copy id").clicked() {
                    ui.ctx().copy_text(id.clone());
                    ui.close();
                }
            });
            if open {
                let text = self.ws.print_open.as_ref().map(|(_, t)| t.clone()).unwrap_or_default();
                egui::Frame::new().fill(p.bg).stroke(egui::Stroke::new(1.0, p.border_variant)).corner_radius(4).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(egui::Label::new(RichText::new(&text).size(11.0).monospace().color(p.text)).wrap().selectable(true));
                    if theme::bracket_button(ui, "copy").clicked() {
                        ui.ctx().copy_text(text.clone());
                    }
                });
            }
        }
    }
}

/// A hover-card thumbnail.
pub enum Thumb {
    Loading(std::sync::Arc<std::sync::Mutex<Option<Result<egui::ColorImage, String>>>>),
    Ready(egui::TextureHandle),
    Failed(String),
}

/// Scale an image down so its longer side is at most `max` (box filter).
pub(crate) fn shrink(img: egui::ColorImage, max: usize) -> egui::ColorImage {
    let [w, h] = img.size;
    let k = w.max(h).div_ceil(max).max(1);
    if k == 1 {
        return img;
    }
    let (nw, nh) = (w / k, h / k);
    let mut px = Vec::with_capacity(nw * nh);
    for y in 0..nh {
        for x in 0..nw {
            let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32);
            for dy in 0..k {
                for dx in 0..k {
                    let c = img.pixels[(y * k + dy) * w + x * k + dx];
                    r += c.r() as u32;
                    g += c.g() as u32;
                    b += c.b() as u32;
                    a += c.a() as u32;
                }
            }
            let n = (k * k) as u32;
            px.push(egui::Color32::from_rgba_premultiplied((r / n) as u8, (g / n) as u8, (b / n) as u8, (a / n) as u8));
        }
    }
    egui::ColorImage::new([nw.max(1), nh.max(1)], px)
}

#[cfg(test)]
mod thumb_tests {
    #[test]
    fn shrink_keeps_aspect_and_averages() {
        let img = eframe::egui::ColorImage::new([600, 300], vec![eframe::egui::Color32::from_rgb(200, 100, 0); 600 * 300]);
        let t = super::shrink(img, 256);
        assert_eq!(t.size, [200, 100]);
        assert_eq!(t.pixels[0], eframe::egui::Color32::from_rgb(200, 100, 0));
    }
}
