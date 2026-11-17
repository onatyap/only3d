//! Right-click menus in the viewport: on objects (the selection) and on empty space. Every entry
//! is an action (with its shortcut shown) or a tool call, the same ones the palette and agents
//! use.

use eframe::egui;
use serde_json::json;

use crate::app::{App, PRIMITIVES};
use crate::arrange_ops::ArrangeOp;
use crate::keymap::Action;
use crate::menubar::Entry;
use crate::settings_ui::short;
use crate::worker::Kind;

/// What a right-click in the viewport landed on.
#[derive(Clone, Copy, Debug)]
pub enum CtxTarget {
    /// An object (now selected): the menu acts on the selection.
    Objects,
    /// Empty space, with the plate point under the pointer.
    Empty(Option<[f32; 2]>),
}

impl App {
    /// A menu entry for an action: its label, its shortcut, greyed when it would do nothing.
    fn menu_action(&mut self, ui: &mut egui::Ui, a: Action, label: Option<&str>) {
        let text = label.map(String::from).unwrap_or_else(|| a.label());
        let mut b = egui::Button::new(text);
        if let Some(k) = self.keymap.label_for(a) {
            b = b.shortcut_text(k);
        }
        if ui.add_enabled(self.applicable(a), b).clicked() {
            self.run(a);
            ui.close();
        }
    }

    pub(crate) fn viewport_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(220.0);
        match self.ctx_target {
            Some(CtxTarget::Objects) if !self.snap.selection.is_empty() => self.objects_menu(ui),
            Some(CtxTarget::Empty(at)) => self.empty_menu(ui, at),
            _ => {
                ui.close();
            }
        }
    }

    /// Right-click on objects: the Edit commands, the whole Object menu and the Arrange menu
    /// (the menu bar's own lists, so the two never drift), greyed where they don't apply.
    fn objects_menu(&mut self, ui: &mut egui::Ui) {
        let sel = self.snap.selection.clone();
        let n = sel.len();
        let head = if n == 1 { self.snap.project.object(&sel[0]).map(|o| if o.name.is_empty() { o.id.clone() } else { o.name.clone() }).unwrap_or_default() } else { format!("{n} objects") };
        ui.label(egui::RichText::new(head).size(11.0).color(crate::theme::pal().placeholder));
        for (a, label) in [(Action::Cut, "Cut"), (Action::Copy, "Copy"), (Action::Paste, "Paste"), (Action::Duplicate, "Duplicate"), (Action::Op(ArrangeOp::Copies), "Copies…")] {
            self.menu_entry(ui, a, label, false);
        }
        ui.separator();
        let spec = crate::menubar::spec();
        let entries = |title: &str| spec.iter().find(|t| t.title == title).map(|t| t.entries.clone()).unwrap_or_default();
        // the Object menu, without Add Shape (that's for empty space)
        let object: Vec<Entry> = entries("Object").into_iter().skip_while(|e| !matches!(e, Entry::Check(..))).collect();
        self.spec_menu(ui, &object, &sel);
        ui.separator();
        let arrange = entries("Arrange");
        ui.menu_button("Arrange", |ui| self.spec_menu(ui, &arrange, &sel));
        self.menu_entry(ui, Action::FrameSelection, "Frame in View", false);
        ui.separator();
        self.menu_entry(ui, Action::Delete, "Delete", false);
    }

    /// One menu-bar entry list drawn as an egui menu.
    fn spec_menu(&mut self, ui: &mut egui::Ui, entries: &[Entry], sel: &[String]) {
        for e in entries {
            match e {
                Entry::Act(a, label) => self.menu_entry(ui, *a, label, false),
                Entry::Check(a, label) => self.menu_entry(ui, *a, label, true),
                Entry::Sub(label, _) if label == "Filament" => {
                    ui.menu_button("Filament", |ui| self.filament_menu(ui, sel));
                }
                Entry::Sub(label, es) => {
                    ui.menu_button(label.as_str(), |ui| self.spec_menu(ui, es, sel));
                }
                Entry::Sep => {
                    ui.separator();
                }
                Entry::MoveToPlate => {
                    ui.menu_button("Move to Plate", |ui| self.move_to_plate_menu(ui, sel));
                }
                Entry::Sys(_) | Entry::Recent | Entry::Panel(_) => {}
            }
        }
    }

    /// An action in a right-click menu: its label, its shortcut, greyed when it can't run, and
    /// highlighted when it is a tool or toggle that is on.
    fn menu_entry(&mut self, ui: &mut egui::Ui, a: Action, label: &str, check: bool) {
        let mut b = egui::Button::new(label);
        if let Some(k) = self.keymap.label_for(a) {
            b = b.shortcut_text(k);
        }
        if check {
            b = b.selected(self.menu_checked(a));
        }
        if ui.add_enabled(self.menu_enabled(a), b).clicked() {
            self.run(a);
            ui.close();
        }
    }

    fn move_to_plate_menu(&mut self, ui: &mut egui::Ui, sel: &[String]) {
        let plates = self.snap.project.plate_count();
        for k in (1..=plates).filter(|k| *k != self.snap.plate) {
            if ui.button(self.snap.project.plate_name(k)).clicked() {
                self.worker.call("plate", json!({"action": "move", "plate": k, "ids": sel}), Kind::Plain);
                ui.close();
            }
        }
        if ui.button("New Plate").clicked() {
            self.worker.call("plate", json!({"action": "add"}), Kind::Plain);
            self.worker.call("plate", json!({"action": "move", "plate": plates + 1, "ids": sel}), Kind::Plain);
            ui.close();
        }
    }

    /// The project's filament slots, then the spools on hand: picking a spool gives the
    /// selection its slot (adding one when no slot holds that spool yet).
    fn filament_menu(&mut self, ui: &mut egui::Ui, sel: &[String]) {
        ui.set_width(MENU_W);
        let p = crate::theme::pal();
        let current: Vec<u32> = sel.iter().filter_map(|id| self.snap.project.object(id)).map(|o| o.filament).collect();
        let spools = self.spools();
        ui.label(egui::RichText::new("In this project").size(11.0).color(p.placeholder));
        for k in 1..=self.snap.project.filaments.len().min(9) as u8 {
            let f = self.snap.project.filaments[k as usize - 1].clone();
            let on = !current.is_empty() && current.iter().all(|c| *c == k as u32);
            let spool = f.spool.as_ref().and_then(|id| spools.iter().find(|s| &s.id == id));
            let text = match spool {
                Some(sp) => format!("{k} · {}", if sp.name.is_empty() { &sp.id } else { &sp.name }),
                None => format!("{k} · {}", short(&f.preset)),
            };
            let key = self.keymap.label_for(Action::SetFilament(k));
            if swatch_row(ui, &f.color, &text, key.as_deref(), on).clicked() {
                self.run(Action::SetFilament(k));
                ui.close();
            }
        }
        let linked: Vec<&String> = self.snap.project.filaments.iter().filter_map(|f| f.spool.as_ref()).collect();
        let mut rest: Vec<&only3d_studio::spools::Spool> = spools.iter().filter(|s| !linked.contains(&&s.id) && s.remaining_g > 0.0).collect();
        // what is loaded in the printer first, then by name
        rest.sort_by_key(|s| (s.location.as_deref().is_none_or(|l| !(l.starts_with("ams") || l.starts_with("ext:"))), s.name.to_lowercase()));
        ui.separator();
        ui.label(egui::RichText::new(if rest.is_empty() { "No other spools on hand" } else { "Your spools" }).size(11.0).color(p.placeholder));
        let mut pick: Option<String> = None;
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            for sp in rest {
                let name = if sp.name.is_empty() { sp.id.clone() } else { sp.name.clone() };
                let at = sp.location.as_deref().filter(|l| !l.is_empty()).map(|l| format!(" · {}", only3d_studio::spools::location_name(l).replace("the ", ""))).unwrap_or_default();
                let text = format!("{name} · {} · {:.0} g{at}", sp.material, sp.remaining_g);
                if swatch_row(ui, &sp.color, &text, None, false).on_hover_text("Print the selection with this spool (adds a filament slot for it)").clicked() {
                    pick = Some(sp.id.clone());
                }
            }
        });
        if let Some(id) = pick {
            let calls = sel.iter().map(|o| ("update_object".to_string(), json!({"id": o, "spool": id}))).collect();
            self.run_calls(calls, Kind::Plain);
            ui.close();
        }
    }

    fn empty_menu(&mut self, ui: &mut egui::Ui, at: Option<[f32; 2]>) {
        let bed = self.snap.bed.size;
        let on_plate = at.filter(|p| p[0] >= 0.0 && p[1] >= 0.0 && p[0] <= bed[0] && p[1] <= bed[1]);
        ui.menu_button(if on_plate.is_some() { "Add here" } else { "Add" }, |ui| {
            for (name, shape) in PRIMITIVES {
                if ui.button(name).clicked() {
                    let shape: serde_json::Value = serde_json::from_str(shape).unwrap_or_default();
                    let mut args = json!({"shape": shape, "name": name.to_ascii_lowercase().replace(' ', "_")});
                    if let Some(p) = on_plate {
                        args["position"] = json!([(p[0] * 10.0).round() / 10.0, (p[1] * 10.0).round() / 10.0]);
                    }
                    self.worker.call("add_object", args, Kind::AddObject);
                    ui.close();
                }
            }
        });
        self.menu_action(ui, Action::Import, Some("Import a model…"));
        self.menu_action(ui, Action::Paste, Some("Paste"));
        ui.separator();
        self.menu_action(ui, Action::SelectAll, Some("Select all"));
        self.menu_action(ui, Action::Arrange, Some("Arrange this plate"));
        if ui.button("Add a plate").clicked() {
            self.worker.call("plate", json!({"action": "add"}), Kind::Plain);
            ui.close();
        }
        ui.separator();
        ui.menu_button("View", |ui| {
            for a in [Action::ViewIso, Action::ViewTop, Action::ViewFront, Action::ViewLeft, Action::ViewRight] {
                self.menu_action(ui, a, None);
            }
            self.menu_action(ui, Action::ToggleOverhangs, None);
        });
        self.menu_action(ui, Action::Slice, Some("Slice plate"));
    }
}

/// Does this shape tree load a model file?
pub fn has_import(n: &only3d_model::Node) -> bool {
    serde_json::to_value(n).map(|v| v.to_string().contains("\"type\":\"import\"")).unwrap_or(false)
}

/// The same shape tree with `repair` on every imported mesh.
pub fn with_repair(n: &only3d_model::Node) -> serde_json::Value {
    fn walk(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Object(m) => {
                if m.get("type").and_then(|t| t.as_str()) == Some("import") {
                    m.insert("repair".into(), serde_json::Value::Bool(true));
                }
                for (_, c) in m.iter_mut() {
                    walk(c);
                }
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(walk),
            _ => {}
        }
    }
    let mut v = serde_json::to_value(n).unwrap_or_default();
    walk(&mut v);
    v
}

/// Width of the filament menu.
const MENU_W: f32 = 300.0;

/// A menu row with a colour swatch, a label, an optional shortcut and a tick when current.
fn swatch_row(ui: &mut egui::Ui, color: &str, text: &str, key: Option<&str>, on: bool) -> egui::Response {
    let p = crate::theme::pal();
    let c = crate::scene::parse_hex(color);
    let (r, resp) = ui.allocate_exact_size(egui::vec2(MENU_W, 22.0), egui::Sense::click());
    let text: String = if text.chars().count() > 46 { format!("{}…", text.chars().take(45).collect::<String>()) } else { text.to_string() };
    if resp.hovered() {
        ui.painter().rect_filled(r, 3.0, p.hover);
    }
    let sw = egui::Rect::from_center_size(egui::pos2(r.left() + 12.0, r.center().y), egui::vec2(11.0, 11.0));
    ui.painter().rect_filled(sw, 2.0, egui::Color32::from_rgb(c[0], c[1], c[2]));
    ui.painter().rect_stroke(sw, 2.0, egui::Stroke::new(1.0, egui::Color32::from_black_alpha(60)), egui::StrokeKind::Inside);
    let font = egui::FontId::proportional(12.5);
    ui.painter().text(egui::pos2(r.left() + 24.0, r.center().y), egui::Align2::LEFT_CENTER, text, font.clone(), if on { p.accent } else { p.text });
    let right = if on { Some("✓".to_string()) } else { key.map(String::from) };
    if let Some(k) = right {
        ui.painter().text(egui::pos2(r.right() - 6.0, r.center().y), egui::Align2::RIGHT_CENTER, k, egui::FontId::proportional(11.5), p.placeholder);
    }
    resp
}
