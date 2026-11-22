//! Dock panel contents: Objects, Parameters, Inspector, Print, Spools, Printer, Checks and
//! Activity (Settings is in `settings_ui.rs`). Every edit is a tool call.

use eframe::egui::{self, Align, Color32, Layout, Margin, RichText, Sense, Ui};
use only3d_model::Severity;
use serde_json::{json, Value};

use crate::app::{fmt_duration, App, Dialog, PresetInfo, ToolForm, XformLocal};
use crate::dock::PanelId;
use crate::keymap::Action;
use crate::palette;
use crate::settings_ui::short;
use crate::theme::{self, pal, Icon};
use crate::worker::Kind;

const BED_TYPES: [&str; 6] = ["Textured PEI Plate", "Cool Plate", "Engineering Plate", "High Temp Plate", "Smooth PEI Plate", "Cool Plate (SuperTack)"];

/// A value as OpenSCAD would print it: 2.4, "text", [1, 2].
fn show_value(v: &Value) -> String {
    match v {
        Value::String(t) => format!("{t:?}"),
        Value::Number(n) => only3d_model::scad::fmt(n.as_f64().unwrap_or(0.0)),
        Value::Array(a) => format!("[{}]", a.iter().map(show_value).collect::<Vec<_>>().join(", ")),
        other => other.to_string(),
    }
}

fn drag_speed(v: f64) -> f64 {
    (v.abs() * 0.01).max(0.01)
}

pub(crate) fn swatch(ui: &mut Ui, c: Color32, size: f32) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(egui::vec2(size, size), Sense::hover());
    ui.painter().rect_filled(r, 2.0, c);
    ui.painter().rect_stroke(r, 2.0, egui::Stroke::new(1.0, Color32::from_black_alpha(50)), egui::StrokeKind::Inside);
    resp
}

pub(crate) fn muted(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).size(12.0).color(pal().muted)
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

pub(crate) fn scroll(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
        ui.add_space(8.0);
    });
}

impl App {
    pub fn panel_ui(&mut self, ui: &mut Ui, panel: PanelId) {
        match panel {
            PanelId::Plates => self.plates_panel(ui),
            PanelId::Files => self.files_panel(ui),
            PanelId::Prints => self.prints_panel(ui),
            PanelId::Parameters => self.params_panel(ui),
            PanelId::Inspector => self.inspector_panel(ui),
            PanelId::Print => self.print_panel(ui),
            PanelId::Settings => self.settings_panel(ui),
            PanelId::Spools => self.spools_panel(ui),
            PanelId::Printer => self.printer_panel(ui),
            PanelId::Checks => self.checks_panel(ui),
            PanelId::Activity => self.activity_panel(ui),
        }
    }

    /// Open the run-tool form with arguments filled in.
    pub fn tool_form_with(&mut self, name: &str, args: Value) {
        if let Some(t) = self.tools.iter().find(|t| t.name == name).cloned() {
            self.dialog = Some(Dialog::ToolForm(ToolForm { tool: t, text: serde_json::to_string_pretty(&args).unwrap_or_default(), result: None, running: false, focus: true }));
        } else {
            self.toast(format!("'{name}' is not available in this build of only3d"), true);
        }
    }

    // ------------------------------------------------------------------------------------------
    // parameters

    fn params_panel(&mut self, ui: &mut Ui) {
        let p = pal();
        let params = self.snap.project.params.clone();
        let busy = self.key_busy("params");
        scroll(ui, "params_scroll", |ui| {
            if params.is_empty() {
                ui.label(muted("Named numbers used in shape expressions, e.g. wall = 2.4 → \"size\": [40, 30, \"wall*2\"]."));
            }
            egui::Grid::new("params_grid").num_columns(3).spacing([8.0, 4.0]).show(ui, |ui| {
                for (k, v) in &params {
                    let lv = self.params_local.entry(k.clone()).or_insert(*v);
                    if !busy {
                        *lv = *v;
                    }
                    ui.label(RichText::new(k).monospace().color(p.text));
                    let r = ui.add(egui::DragValue::new(lv).speed(drag_speed(*v)).max_decimals(3).min_decimals(1));
                    if r.changed() {
                        self.throttle.merge("params", "set_params", json!({ "params": { k.clone(): *lv } }));
                    }
                    if r.dragged() || r.has_focus() {
                        self.active_now.insert("params".into());
                    }
                    if theme::icon_button(ui, Icon::Close, "Remove parameter").clicked() {
                        self.worker.call("set_params", json!({ "params": { k.clone(): Value::Null } }), Kind::Plain);
                    }
                    ui.end_row();
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut self.new_param.0).hint_text("new parameter").desired_width(110.0));
                ui.add(egui::DragValue::new(&mut self.new_param.1).speed(0.1).max_decimals(3));
                let ok = !self.new_param.0.trim().is_empty();
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.add_enabled(ok, egui::Button::new("Add")).clicked() || (ok && enter) {
                    let name = self.new_param.0.trim().to_string();
                    self.worker.call("set_params", json!({ "params": { name: self.new_param.1 } }), Kind::Plain);
                    self.new_param.0.clear();
                }
            });
            for o in self.snap.scad.clone() {
                ui.add_space(8.0);
                self.scad_section(ui, &o);
            }
        });
    }

    /// An object made from a .scad file: its OpenSCAD variables, edited in place (the file stays
    /// as it is; only3d renders it with these values).
    fn scad_section(&mut self, ui: &mut Ui, o: &crate::app::ScadObject) {
        let p = pal();
        let changed = o.set.len();
        let title = format!("{} · {}", o.name, o.file);
        let open = self.section_header(ui, &format!("params.scad.{}", o.id), &title, true, false, |ui| {
            if changed > 0 {
                ui.label(RichText::new(format!("{changed} changed")).size(10.5).color(p.accent));
            }
        });
        if !open {
            return;
        }
        if let Some(e) = &o.error {
            ui.label(RichText::new(e).size(11.0).color(p.error));
            return;
        }
        if o.vars.is_empty() {
            ui.label(muted("This file has no customizer variables (top-level assignments like `width = 40;`)."));
            return;
        }
        let key = format!("scad:{}", o.id);
        let busy = self.key_busy(&key);
        let groups: Vec<String> = o.vars.iter().fold(Vec::new(), |mut g, v| {
            if !g.contains(&v.group) {
                g.push(v.group.clone());
            }
            g
        });
        for g in &groups {
            if groups.len() > 1 {
                ui.label(RichText::new(g).size(10.5).color(p.placeholder));
            }
            egui::Grid::new(format!("scad_grid_{}_{g}", o.id)).num_columns(3).spacing([8.0, 4.0]).show(ui, |ui| {
                for v in o.vars.iter().filter(|v| &v.group == g) {
                    let remote = o.set.get(&v.name).cloned().unwrap_or_else(|| v.initial.clone());
                    let lk = (o.id.clone(), v.name.clone());
                    let mut cur = match self.scad_local.get(&lk) {
                        Some(x) if busy => x.clone(),
                        _ => remote.clone(),
                    };
                    let overridden = o.set.contains_key(&v.name);
                    let name = RichText::new(&v.name).monospace().color(if overridden { p.accent } else { p.text });
                    let r = ui.label(name);
                    if !v.caption.is_empty() {
                        r.on_hover_text(&v.caption);
                    }
                    let mut send: Option<(Value, bool)> = None; // (value, throttled)
                    ui.horizontal(|ui| {
                        if !v.options.is_empty() {
                            let label = v.options.iter().find(|(_, x)| x == &cur).map(|(l, _)| l.clone()).unwrap_or_else(|| show_value(&cur));
                            egui::ComboBox::from_id_salt(("scad_opt", &o.id, &v.name)).selected_text(label).show_ui(ui, |ui| {
                                for (l, x) in &v.options {
                                    if ui.selectable_label(*x == cur, l).clicked() {
                                        send = Some((x.clone(), false));
                                    }
                                }
                            });
                            return;
                        }
                        match (v.kind.as_str(), &mut cur) {
                            ("boolean", Value::Bool(b)) => {
                                if ui.checkbox(b, "").changed() {
                                    send = Some((Value::Bool(*b), false));
                                }
                            }
                            ("number", Value::Number(n)) => {
                                let mut x = n.as_f64().unwrap_or(0.0);
                                let speed = v.step.unwrap_or_else(|| drag_speed(x));
                                let mut dv = egui::DragValue::new(&mut x).speed(speed).max_decimals(3);
                                if let (Some(a), Some(b)) = (v.min, v.max) {
                                    dv = dv.range(a..=b);
                                }
                                let r = ui.add(dv);
                                if r.changed() {
                                    send = Some((json!(x), true));
                                }
                                if r.dragged() || r.has_focus() {
                                    self.active_now.insert(key.clone());
                                }
                            }
                            ("vector", Value::Array(a)) => {
                                let mut out = a.clone();
                                let mut any = false;
                                for x in out.iter_mut() {
                                    if let Some(mut f) = x.as_f64() {
                                        let speed = v.step.unwrap_or_else(|| drag_speed(f));
                                        let r = ui.add(egui::DragValue::new(&mut f).speed(speed).max_decimals(3));
                                        if r.changed() {
                                            *x = json!(f);
                                            any = true;
                                        }
                                        if r.dragged() || r.has_focus() {
                                            self.active_now.insert(key.clone());
                                        }
                                    } else {
                                        ui.label(RichText::new(show_value(x)).monospace().size(11.0));
                                    }
                                }
                                if any {
                                    send = Some((Value::Array(out), true));
                                }
                            }
                            (_, x) => {
                                // text, and numbers given as expressions over the project parameters
                                let mut t = match x {
                                    Value::String(t) => t.clone(),
                                    other => show_value(other),
                                };
                                let r = ui.add(egui::TextEdit::singleline(&mut t).desired_width(120.0).font(egui::TextStyle::Monospace));
                                if r.changed() {
                                    self.scad_local.insert(lk.clone(), Value::String(t.clone()));
                                }
                                if r.has_focus() || r.changed() {
                                    self.active_now.insert(key.clone());
                                }
                                // a plain number typed for a number is a number again
                                let typed = match t.trim().parse::<f64>() {
                                    Ok(n) if v.kind == "number" => json!(n),
                                    _ => Value::String(t),
                                };
                                if r.lost_focus() && typed != remote {
                                    send = Some((typed, false));
                                }
                            }
                        }
                    });
                    if overridden {
                        if theme::icon_button(ui, Icon::Reset, &format!("Back to the file's value ({})", show_value(&v.initial))).clicked() {
                            self.scad_local.remove(&lk);
                            self.worker.call("set_params", json!({ "object": o.id, "params": { v.name.clone(): Value::Null } }), Kind::Plain);
                        }
                    } else {
                        ui.label("");
                    }
                    ui.end_row();
                    if let Some((val, throttled)) = send {
                        self.scad_local.insert(lk.clone(), val.clone());
                        if throttled {
                            self.throttle.merge(&key, "set_params", json!({ "object": o.id, "params": { v.name.clone(): val } }));
                        } else {
                            self.worker.call("set_params", json!({ "object": o.id, "params": { v.name.clone(): val } }), Kind::Plain);
                        }
                    }
                }
            });
        }
        ui.label(RichText::new("Changes render the file with these values; the file itself stays as it is.").size(10.5).color(p.placeholder));
    }

    // ------------------------------------------------------------------------------------------
    // inspector

    fn inspector_panel(&mut self, ui: &mut Ui) {
        let p = pal();
        let sel = self.snap.selection.clone();
        if sel.is_empty() {
            ui.add_space(2.0);
            ui.label(muted("Select an object in the viewport or the list. Agents read your selection with get_selection — “make this thicker”."));
            return;
        }
        if sel.len() > 1 {
            theme::section_label(ui, &format!("{} objects selected", sel.len()));
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if ui.button("Auto-orient").clicked() {
                    self.run(Action::PlaceOnFace);
                }
                if ui.button("Duplicate").clicked() {
                    self.run(Action::Duplicate);
                }
                if ui.button("Delete").clicked() {
                    self.run(Action::Delete);
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(muted("Filament"));
                for s in 1..=self.snap.project.filaments.len() as u32 {
                    let c = self.snap.filament_color(s);
                    if swatch(ui, c, 14.0).interact(Sense::click()).on_hover_text(format!("Set filament {s}")).clicked() {
                        self.run(Action::SetFilament(s as u8));
                    }
                }
            });
            ui.add_space(6.0);
            self.arrange_section(ui);
            return;
        }
        let id = sel[0].clone();
        let Some(o) = self.snap.project.object(&id).cloned() else { return };
        let ev = self.snap.evaluated(&id).cloned();
        scroll(ui, "inspector_scroll", |ui| {
            self.arrange_section(ui);
            if self.section_header(ui, "inspector.object", "Object", true, false, |_| {}) {
                if self.name_edit.0 != id {
                    self.name_edit = (id.clone(), o.name.clone());
                }
                ui.horizontal(|ui| {
                    ui.label(muted("Name"));
                    let r = ui.add(egui::TextEdit::singleline(&mut self.name_edit.1).desired_width(f32::INFINITY));
                    if !r.has_focus() && !r.lost_focus() && self.name_edit.1 != o.name {
                        self.name_edit.1 = o.name.clone();
                    }
                    if r.lost_focus() && self.name_edit.1 != o.name {
                        self.worker.call("update_object", json!({ "id": id, "name": self.name_edit.1 }), Kind::Plain);
                    }
                });
                ui.label(RichText::new(format!("id {id}")).size(10.5).monospace().color(p.placeholder));
                if let Some(sc) = self.snap.scad.iter().find(|x| x.id == id) {
                    let n = sc.vars.len();
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("OpenSCAD · {} · {n} variable{}", sc.file, if n == 1 { "" } else { "s" })).size(11.5).color(p.muted));
                        if n > 0 && ui.small_button("Edit in Parameters").clicked() {
                            self.cfg.layout.show(PanelId::Parameters);
                        }
                    });
                }
                if let Some(ev) = &ev {
                    let r = &ev.report;
                    ui.label(
                        RichText::new(format!("{:.1} × {:.1} × {:.1} mm · {:.2} cm³ · {}", r.size.x, r.size.y, r.size.z, r.volume_mm3 / 1000.0, if r.watertight { "watertight" } else { "not watertight" }))
                            .size(11.5)
                            .color(if r.watertight { p.muted } else { p.error }),
                    );
                }
            }

            if self.section_header(ui, "inspector.transform", "Transform", true, false, |_| {}) {
                let key = format!("xf:{id}");
                let center = ev.as_ref().map(|e| [(e.report.bounds.min.x + e.report.bounds.max.x) * 0.5, (e.report.bounds.min.y + e.report.bounds.max.y) * 0.5]).unwrap_or([0.0, 0.0]);
                let remote = XformLocal { pos: o.position.unwrap_or(center), rot: o.rotation, scale: o.scale };
                let busy = self.key_busy(&key);
                let mut xf = match &self.xf_local {
                    Some((xid, x)) if *xid == id && busy => *x,
                    _ => remote,
                };
                let mut changed: Vec<(&str, Value)> = Vec::new();
                let mut active = false;
                egui::Grid::new("xf_grid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                    ui.label(muted("Position"));
                    ui.horizontal(|ui| {
                        let a = ui.add(egui::DragValue::new(&mut xf.pos[0]).speed(0.5).prefix("x ").suffix(" mm").max_decimals(2));
                        let b = ui.add(egui::DragValue::new(&mut xf.pos[1]).speed(0.5).prefix("y ").suffix(" mm").max_decimals(2));
                        if a.changed() || b.changed() {
                            changed.push(("position", json!(xf.pos)));
                        }
                        active |= a.dragged() || b.dragged() || a.has_focus() || b.has_focus();
                    });
                    ui.end_row();
                    ui.label(muted("Rotation"));
                    let mut any = false;
                    ui.horizontal(|ui| {
                        for (i, ax) in ["x ", "y ", "z "].iter().enumerate() {
                            let r = ui.add(egui::DragValue::new(&mut xf.rot[i]).speed(1.0).prefix(*ax).suffix("°").max_decimals(1));
                            any |= r.changed();
                            active |= r.dragged() || r.has_focus();
                        }
                    });
                    ui.end_row();
                    if any {
                        changed.push(("rotation", json!(xf.rot)));
                    }
                    ui.label(muted("Scale"));
                    let mut any = false;
                    ui.horizontal(|ui| {
                        let mut pct = xf.scale.map(|s| s * 100.0);
                        let n = if self.uniform_scale { 1 } else { 3 };
                        for i in 0..n {
                            let ax = if self.uniform_scale { "" } else { ["x ", "y ", "z "][i] };
                            let r = ui.add(egui::DragValue::new(&mut pct[i]).speed(0.5).range(1.0..=10000.0).prefix(ax).suffix(" %").max_decimals(1));
                            if r.changed() {
                                any = true;
                                if self.uniform_scale {
                                    pct = [pct[0]; 3];
                                }
                            }
                            active |= r.dragged() || r.has_focus();
                        }
                        xf.scale = pct.map(|p| p / 100.0);
                        ui.checkbox(&mut self.uniform_scale, RichText::new("uniform").size(12.0));
                    });
                    ui.end_row();
                    if any {
                        changed.push(("scale", json!(xf.scale)));
                    }
                });
                if active {
                    self.active_now.insert(key.clone());
                }
                if !changed.is_empty() {
                    let mut args = serde_json::Map::new();
                    args.insert("id".into(), json!(id));
                    for (k, v) in changed {
                        args.insert(k.into(), v);
                    }
                    self.throttle.merge(&key, "update_object", Value::Object(args));
                }
                self.xf_local = Some((id.clone(), xf));
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Lay flat").on_hover_text("orient: minimise overhangs, maximise bed contact").clicked() {
                        self.run(Action::PlaceOnFace);
                    }
                    if ui.button("Duplicate").clicked() {
                        self.run(Action::Duplicate);
                    }
                    let t = self.keymap.label_for(Action::ToolRotate).map(|k| format!("Rotate tool  {k}")).unwrap_or_default();
                    if ui.button("Rotate tool").on_hover_text(t).clicked() {
                        self.run(Action::ToolRotate);
                    }
                });
            }

            if self.section_header(ui, "inspector.filament", "Filament", true, false, |_| {}) {
                ui.horizontal_wrapped(|ui| {
                    let n = self.snap.project.filaments.len() as u32;
                    for s in 1..=n.max(1) {
                        let on = o.filament == s;
                        let c = self.snap.filament_color(s);
                        let (r, resp) = ui.allocate_exact_size(egui::vec2(28.0, 22.0), Sense::click());
                        ui.painter().rect(r, 3.0, if on { p.selected } else { Color32::TRANSPARENT }, egui::Stroke::new(1.0, if on { p.accent } else { p.border_variant }), egui::StrokeKind::Inside);
                        ui.painter().rect_filled(egui::Rect::from_center_size(r.center() - egui::vec2(5.0, 0.0), egui::vec2(9.0, 9.0)), 2.0, c);
                        ui.painter().text(r.center() + egui::vec2(6.0, 0.0), egui::Align2::CENTER_CENTER, s.to_string(), egui::FontId::monospace(11.0), if on { p.text } else { p.muted });
                        let f = self.snap.project.filaments.get(s as usize - 1).map(|f| short(&f.preset)).unwrap_or_default();
                        if resp.on_hover_text(format!("Filament {s}: {f}")).clicked() && !on {
                            self.worker.call("update_object", json!({ "id": id, "filament": s }), Kind::Plain);
                        }
                    }
                });
            }

            let n_set = o.settings.len();
            if self.section_header(ui, "inspector.settings", &format!("Object settings · {n_set}"), n_set > 0, false, |_| {}) {
                if o.settings.is_empty() {
                    ui.label(muted("No per-object overrides."));
                }
                for (k, v) in &o.settings {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(k).monospace().size(11.5));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if theme::icon_button(ui, Icon::Reset, "Remove this override").clicked() {
                                let mut m = o.settings.clone();
                                m.remove(k);
                                self.worker.call("update_object", json!({ "id": id, "settings": m }), Kind::Plain);
                            }
                            ui.label(RichText::new(v.to_string()).monospace().size(11.5).color(p.accent));
                        });
                    });
                }
                if theme::bracket_button(ui, "edit in settings").clicked() {
                    self.settings_object = true;
                    self.cfg.layout.show(PanelId::Settings);
                }
            }

            ui.add_space(2.0);
            let dirty = self.shape_ed.dirty;
            if self.section_header(ui, "inspector.shape", "Shape tree (JSON)", true, false, |ui| {
                if dirty {
                    ui.label(RichText::new("edited").size(10.5).color(p.warning));
                }
            }) {
                let src = serde_json::to_string_pretty(&o.shape).unwrap_or_default();
                if self.shape_ed.id != id || (!self.shape_ed.dirty && self.shape_ed.source != src) {
                    self.shape_ed.id = id.clone();
                    self.shape_ed.text = src.clone();
                    self.shape_ed.source = src.clone();
                    self.shape_ed.dirty = false;
                    self.shape_ed.error = None;
                }
                let r = ui.add(egui::TextEdit::multiline(&mut self.shape_ed.text).code_editor().desired_rows(10).desired_width(f32::INFINITY));
                if r.changed() {
                    self.shape_ed.dirty = self.shape_ed.text != self.shape_ed.source;
                    self.shape_ed.error = None;
                }
                ui.horizontal(|ui| {
                    let can = self.shape_ed.dirty && !self.worker.busy_with(|k| matches!(k, Kind::ShapeApply(_)));
                    if ui.add_enabled(can, theme::primary_button("Apply")).clicked() {
                        match serde_json::from_str::<Value>(&self.shape_ed.text) {
                            Ok(shape) => self.worker.call("update_object", json!({ "id": id, "shape": shape }), Kind::ShapeApply(id.clone())),
                            Err(e) => self.shape_ed.error = Some(format!("invalid JSON: {e}")),
                        }
                    }
                    if ui.add_enabled(self.shape_ed.dirty, egui::Button::new("Revert")).clicked() {
                        self.shape_ed.text = self.shape_ed.source.clone();
                        self.shape_ed.dirty = false;
                        self.shape_ed.error = None;
                    }
                });
                if let Some(e) = &self.shape_ed.error {
                    egui::Frame::new().fill(theme::soft(p.error)).corner_radius(4).inner_margin(8).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(e).color(p.error).size(12.0));
                    });
                }
            }
        });
    }

    // ------------------------------------------------------------------------------------------
    // print: printer, filaments, process, slice, export, print

    fn print_panel(&mut self, ui: &mut Ui) {
        scroll(ui, "print_scroll", |ui| {
            self.printer_section(ui);
            self.filament_section(ui);
            self.process_section(ui);
            self.slice_section(ui);
        });
    }

    /// A grouped, filterable preset picker. Returns the picked preset.
    fn preset_picker(&mut self, ui: &mut Ui, id: &str, kind: &str, current: &str, width: f32) -> Option<String> {
        let p = pal();
        let list = self.presets.get(kind).cloned().unwrap_or_default();
        if list.is_empty() {
            ui.label(RichText::new(short(current)).size(12.5)).on_hover_text(format!("{current}\n\nNo preset library found: presets can't be listed, the name is kept as-is."));
            return None;
        }
        let fid = egui::Id::unique(("preset_filter", id));
        let mut filter: String = ui.data_mut(|d| d.get_temp::<String>(fid).unwrap_or_default());
        let mut picked = None;
        egui::ComboBox::from_id_salt(id)
            .selected_text(RichText::new(short(current)).size(12.5))
            .width(width)
            .height(440.0)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show_ui(ui, |ui| {
                ui.set_min_width(width.max(280.0));
                if list.len() > 10 {
                    ui.add(egui::TextEdit::singleline(&mut filter).hint_text("Filter presets…").desired_width(f32::INFINITY));
                }
                let groups = [("only3d", "only3d presets"), ("bambu_user", "Your Bambu Studio presets"), ("system", "System presets")];
                let known: Vec<&str> = groups.iter().map(|g| g.0).collect();
                let mut sections: Vec<(&str, Vec<&PresetInfo>)> = groups.iter().map(|(src, title)| (*title, list.iter().filter(|p| p.source == *src).collect())).collect();
                sections.push(("Other", list.iter().filter(|p| !known.contains(&p.source.as_str())).collect()));
                for (title, items) in sections {
                    let items: Vec<&&PresetInfo> = items.iter().filter(|p| filter.trim().is_empty() || palette::fuzzy_score(&filter, &p.name).is_some()).take(400).collect();
                    if items.is_empty() {
                        continue;
                    }
                    ui.add_space(2.0);
                    theme::section_label(ui, title);
                    for it in items {
                        let on = it.name == current;
                        if ui.selectable_label(on, RichText::new(&it.name).size(12.0).color(if on { p.text } else { p.text.gamma_multiply(0.9) })).clicked() {
                            if !on {
                                picked = Some(it.name.clone());
                            }
                            ui.close();
                        }
                    }
                }
            });
        ui.data_mut(|d| d.insert_temp(fid, filter));
        picked
    }

    fn preset_menu(&mut self, ui: &mut Ui, kind: &str, current: &str) {
        let resp = theme::icon_button(ui, Icon::More, &format!("{} presets", cap(kind)));
        let current = current.to_string();
        let kind = kind.to_string();
        let source = self.presets.get(&kind).and_then(|l| l.iter().find(|p| p.name == current)).map(|p| p.source.clone()).unwrap_or_else(|| "system".into());
        egui::Popup::menu(&resp).show(|ui| {
            if kind == "process" && ui.button("Save current settings as preset…").clicked() {
                self.dialog = Some(Dialog::SavePreset { kind: kind.clone(), name: format!("{} (custom)", short(&current)), focus: true });
                ui.close();
            }
            if ui.button("Import preset…").on_hover_text("Bambu Studio .json / .bbscfg / .zip bundle / .3mf project").clicked() {
                ui.close();
                if let Some(path) = rfd::FileDialog::new().set_title("Import preset").add_filter("Bambu presets", &["json", "bbscfg", "bbsflmt", "zip", "3mf"]).pick_file() {
                    self.worker.call("preset_import", json!({ "path": path.display().to_string() }), Kind::PresetChanged(String::new()));
                }
            }
            if ui.button(format!("Export “{}”…", short(&current))).clicked() {
                ui.close();
                self.save_panel(crate::save_panel::SaveWhat::Preset { kind: kind.to_string(), name: current.clone() });
            }
            if source != "system" && ui.button(format!("Delete “{}”…", short(&current))).clicked() {
                self.dialog = Some(Dialog::Confirm {
                    title: format!("Delete {kind} preset?"),
                    body: format!("“{current}” will be removed from your presets. Projects that use it keep its name but fall back to its parent."),
                    tool: "preset_delete".into(),
                    args: json!({ "kind": kind, "name": current }),
                });
                ui.close();
            }
            ui.separator();
            if ui.button("Refresh list").clicked() {
                self.refresh_presets(&kind);
                ui.close();
            }
        });
    }

    fn printer_section(&mut self, ui: &mut Ui) {
        if !self.section_header(ui, "print.printer", "Printer", true, false, |_| {}) {
            return;
        }
        let printer = self.snap.project.printer.clone();
        let w = ui.available_width() - 4.0;
        if let Some(m) = self.preset_picker(ui, "machine", "machine", &printer.machine, w) {
            self.call("set_printer", json!({ "machine": m }));
        }
        ui.horizontal(|ui| {
            ui.label(muted("Plate"));
            egui::ComboBox::from_id_salt("bed_type").selected_text(RichText::new(&printer.bed_type).size(12.5)).width(ui.available_width() - 4.0).show_ui(ui, |ui| {
                for b in BED_TYPES {
                    if ui.selectable_label(printer.bed_type == b, b).clicked() && printer.bed_type != b {
                        self.worker.call("set_printer", json!({ "bed_type": b }), Kind::Plain);
                    }
                }
            });
        });
        ui.add_space(4.0);
    }

    fn filament_section(&mut self, ui: &mut Ui) {
        let p = pal();
        let fils = self.snap.project.filaments.clone();
        let first = fils.first().map(|f| f.preset.clone()).unwrap_or_default();
        let open = self.section_header(ui, "print.filaments", &format!("Filaments · {}", fils.len()), true, false, |ui| {
            ui.label("");
        });
        if !open {
            return;
        }
        let spools = self.spools();
        for (i, f) in fils.iter().enumerate() {
            let slot = i as u32 + 1;
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{slot}")).monospace().color(p.muted));
                let remote = {
                    let c = crate::scene::parse_hex(&f.color);
                    Color32::from_rgb(c[0], c[1], c[2])
                };
                let key = format!("fil:{slot}");
                let busy = self.key_busy(&key);
                let mut c = if busy { *self.color_edit.get(&slot).unwrap_or(&remote) } else { remote };
                let r = egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::Opaque);
                if r.changed() {
                    self.color_edit.insert(slot, c);
                    let hex = format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b());
                    self.throttle.merge(&key, "set_filament", json!({ "slot": slot, "color": hex }));
                }
                let w = (ui.available_width() - 74.0).max(80.0);
                if let Some(pr) = self.preset_picker(ui, &format!("fil_preset_{slot}"), "filament", &f.preset, w) {
                    self.call("set_filament", json!({ "slot": slot, "preset": pr }));
                }
                let cur = serde_json::to_value(f.nozzle).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_else(|| "auto".into());
                egui::ComboBox::from_id_salt(("fil_nozzle", slot))
                    .selected_text(RichText::new(cap(&cur)).size(12.0))
                    .width(56.0)
                    .show_ui(ui, |ui| {
                        for n in ["auto", "left", "right"] {
                            if ui.selectable_label(cur == n, cap(n)).clicked() && cur != n {
                                self.worker.call("set_filament", json!({ "slot": slot, "nozzle": n }), Kind::Plain);
                            }
                        }
                    })
                    .response
                    .on_hover_text("H2D nozzle for this filament");
            });
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                let cur = f.spool.clone();
                let label = cur
                    .as_ref()
                    .map(|id| spools.iter().find(|s| &s.id == id).map(|s| format!("{} · {:.0} g", s.id, s.remaining_g)).unwrap_or_else(|| format!("{id} (missing)")))
                    .unwrap_or_else(|| "no spool".into());
                egui::ComboBox::from_id_salt(("fil_spool", slot))
                    .selected_text(RichText::new(label).size(11.5).color(p.muted))
                    .width(ui.available_width() - 6.0)
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(cur.is_none(), "no spool").clicked() && cur.is_some() {
                            self.worker.call("set_filament", json!({ "slot": slot, "spool": "" }), Kind::Plain);
                        }
                        for sp in &spools {
                            let txt = format!("{} · {} · {:.0} g{}", sp.id, sp.material, sp.remaining_g, sp.location.as_ref().map(|l| format!(" · {l}")).unwrap_or_default());
                            if ui.selectable_label(cur.as_deref() == Some(sp.id.as_str()), txt).clicked() && cur.as_deref() != Some(sp.id.as_str()) {
                                self.worker.call("set_filament", json!({ "slot": slot, "spool": sp.id }), Kind::Plain);
                            }
                        }
                    })
                    .response
                    .on_hover_text("Spool loaded for this slot: preset and colour follow it, slicing warns when it runs short, printing charges it");
            });
            ui.add_space(2.0);
        }
        ui.horizontal(|ui| {
            if theme::bracket_button(ui, "add slot").clicked() {
                self.worker.call("set_filament", json!({ "slot": fils.len() + 1 }), Kind::Plain);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                self.preset_menu(ui, "filament", &first);
            });
        });
        ui.add_space(4.0);
    }

    pub fn spools(&mut self) -> Vec<only3d_studio::spools::Spool> {
        let fresh = self.spool_cache.as_ref().is_some_and(|(t, _)| t.elapsed() < std::time::Duration::from_secs(2));
        if !fresh {
            let list: Vec<_> = only3d_studio::spools::Inventory::load().spools.into_iter().filter(|s| !s.archived).collect();
            self.spool_cache = Some((std::time::Instant::now(), list));
        }
        self.spool_cache.as_ref().map(|(_, l)| l.clone()).unwrap_or_default()
    }

    fn setting(&self, key: &str) -> Option<f64> {
        self.snap.project.process.overrides.get(key).and_then(|v| v.as_f64()).or_else(|| self.effective.as_ref().and_then(|e| e.get(key)).and_then(|v| v.as_f64()))
    }

    fn process_section(&mut self, ui: &mut Ui) {
        let p = pal();
        let n_over = self.snap.project.process.overrides.len();
        let open = self.section_header(ui, "print.process", "Process", true, false, |ui| {
            if n_over > 0 {
                ui.label(RichText::new(format!("{n_over} overridden")).size(10.5).color(p.accent));
            }
        });
        if !open {
            return;
        }
        let preset = self.snap.project.process.preset.clone();
        ui.horizontal(|ui| {
            let w = ui.available_width() - 30.0;
            if let Some(pr) = self.preset_picker(ui, "process", "process", &preset, w) {
                self.call("set_process", json!({ "preset": pr }));
            }
            self.preset_menu(ui, "process", &preset);
        });
        if !self.key_busy("process") {
            self.proc_local.layer_height = self.setting("layer_height").unwrap_or(0.2);
            self.proc_local.wall_loops = self.setting("wall_loops").unwrap_or(2.0) as u32;
            self.proc_local.infill_pct = self.setting("infill_density").unwrap_or(0.15) * 100.0;
            self.proc_local.brim = self.setting("brim_width").unwrap_or(0.0);
            self.proc_local.support = self
                .snap
                .project
                .process
                .overrides
                .get("support")
                .and_then(|v| v.as_bool())
                .or_else(|| self.effective.as_ref().and_then(|e| e.get("support")).and_then(|v| v.as_bool()))
                .unwrap_or(false);
        }
        let mut pl = self.proc_local;
        let mut send: Vec<(&str, Value)> = Vec::new();
        let mut active = false;
        let over = |k: &str| self.snap.project.process.overrides.contains_key(k);
        let lab = |k: &str, t: &str| RichText::new(t).size(12.5).color(if over(k) { p.text } else { p.muted });
        let preset = |k: &str| self.effective.as_ref().and_then(|e| e.get("__preset")).and_then(|v| v.get(k)).and_then(|v| v.as_f64());
        let presets = [preset("layer_height"), preset("wall_loops"), preset("infill_density").map(|v| v * 100.0), preset("brim_width")];
        let overridden = ["layer_height", "wall_loops", "infill_density", "brim_width"].map(over);
        egui::Grid::new("proc_grid").num_columns(2).spacing([10.0, 5.0]).show(ui, |ui| {
            let mut row = |ui: &mut Ui, key: &'static str, label: &str, v: &mut f64, range: std::ops::RangeInclusive<f64>, step: f64, suffix: &str, dec: usize, k: usize, to_json: &dyn Fn(f64) -> Value| {
                ui.label(lab(key, label));
                let r = theme::slider(ui, v, range, step, suffix, dec, presets[k], overridden[k]);
                if r.reset {
                    send.push((key, Value::Null));
                } else if r.changed {
                    send.push((key, to_json(*v)));
                }
                active |= r.active;
                ui.end_row();
            };
            row(ui, "layer_height", "Layer height", &mut pl.layer_height, 0.06..=0.32, 0.02, " mm", 2, 0, &|v| json!((v * 100.0).round() / 100.0));
            let mut wl = pl.wall_loops as f64;
            row(ui, "wall_loops", "Wall loops", &mut wl, 1.0..=8.0, 1.0, "", 0, 1, &|v| json!(v.round() as i64));
            pl.wall_loops = wl.round() as _;
            row(ui, "infill_density", "Infill", &mut pl.infill_pct, 0.0..=100.0, 1.0, " %", 0, 2, &|v| json!((v / 100.0 * 100.0).round() / 100.0));
            row(ui, "brim_width", "Brim", &mut pl.brim, 0.0..=10.0, 0.5, " mm", 1, 3, &|v| json!(v));
            ui.label(lab("support", "Supports"));
            if ui.checkbox(&mut pl.support, RichText::new("under overhangs").size(12.5)).changed() {
                send.push(("support", json!(pl.support)));
            }
            ui.end_row();
        });
        self.proc_local = pl;
        if active {
            self.active_now.insert("process".into());
        }
        if !send.is_empty() {
            let mut o = serde_json::Map::new();
            for (k, v) in send {
                o.insert(k.into(), v);
            }
            self.throttle.merge("process", "set_process", json!({ "overrides": Value::Object(o) }));
        }
        ui.horizontal(|ui| {
            if theme::bracket_button(ui, "all settings").on_hover_text("Open the Settings panel: every slicer setting, searchable").clicked() {
                self.settings_object = false;
                self.cfg.layout.show(PanelId::Settings);
            }
            if !self.effective_source.is_empty() {
                let src = self.effective_source.clone();
                let short_src = if src.contains("BambuStudio") || src.contains("Bambu Studio") { "Bambu Studio profiles".to_string() } else { src.chars().take(40).collect::<String>() };
                ui.label(RichText::new(short_src).size(10.5).color(p.placeholder)).on_hover_text(src);
            }
        });
        ui.add_space(4.0);
    }

    fn slice_section(&mut self, ui: &mut Ui) {
        let p = pal();
        if !self.section_header(ui, "print.slice", "Slice & export", true, false, |_| {}) {
            return;
        }
        if self.snap.last_slice.is_none() && self.slice_error.is_none() {
            let k = self.keymap.label_for(Action::Slice).map(|k| format!(" ({k})")).unwrap_or_default();
            ui.label(muted(format!("Slice and Print are at the bottom right of the view{k}.")));
        }
        if let Some(e) = self.slice_error.clone() {
            egui::Frame::new().fill(theme::soft(p.error)).corner_radius(4).inner_margin(8).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(e).color(p.error).size(12.0));
            });
        }
        if let Some(sl) = self.snap.last_slice.clone() {
            let st = &sl.result.stats;
            egui::Frame::new().fill(p.bg).stroke(egui::Stroke::new(1.0, p.border_variant)).corner_radius(4).inner_margin(10).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(fmt_duration(st.print_time_s)).size(20.0).family(theme::medium()));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if self.snap.slice_current {
                            theme::chip(ui, "current", p.success, theme::soft(p.success));
                        } else {
                            theme::chip(ui, "out of date", p.warning, theme::soft(p.warning));
                        }
                    });
                });
                ui.label(RichText::new(format!("{:.1} g · {:.2} m · {} layers", st.filament_g, st.filament_mm / 1000.0, st.layers)).size(12.0).monospace());
                let t = &sl.result.timings;
                ui.label(RichText::new(format!("max Z {:.1} mm · {} · {:.0} ms · {} KB", st.max_z, sl.engine, t.slice_ms + t.plan_ms + t.gcode_ms, sl.package.len() / 1024)).size(10.5).color(p.placeholder));
                for wn in sl.result.warnings.iter().take(6) {
                    ui.label(RichText::new(wn).size(11.5).color(p.warning));
                }
            });
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if theme::bracket_button(ui, "stl").on_hover_text("Export the model as STL").clicked() {
                self.export_dialog("stl");
            }
            if theme::bracket_button(ui, "3mf").on_hover_text("Export the model as 3MF").clicked() {
                self.export_dialog("3mf");
            }
            if theme::bracket_button(ui, "scad").on_hover_text("Export the plate as OpenSCAD source: parameters as variables, each object a module").clicked() {
                self.export_dialog("scad");
            }
            let t = self.keymap.label_for(Action::ExportPrint).map(|k| format!("Print file for Bambu printers (slices first if needed)  {k}")).unwrap_or_default();
            if theme::bracket_button(ui, "gcode.3mf").on_hover_text(t).clicked() {
                self.export_dialog("gcode.3mf");
            }
            if theme::bracket_button(ui, "bambu project").on_hover_text("export_bambu_project: hand the plate to Bambu Studio (painted colours)").clicked() {
                self.save_panel(crate::save_panel::SaveWhat::BambuProject);
            }
        });
    }

    // ------------------------------------------------------------------------------------------
    // spools

    fn spools_panel(&mut self, ui: &mut Ui) {
        let p = pal();
        let spools = self.spools();
        ui.horizontal(|ui| {
            theme::section_label(ui, &format!("Spools · {:.2} kg", spools.iter().map(|s| s.remaining_g).sum::<f64>() / 1000.0));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::icon_toggle(ui, Icon::Plus, "Add a spool", self.spool_form.open && self.spool_form.editing.is_none()).clicked() {
                    let adding = self.spool_form.open && self.spool_form.editing.is_none();
                    self.close_spool_form();
                    self.spool_form.open = !adding;
                }
                if ui.add(theme::ghost_button(RichText::new("Sync AMS").size(12.0))).on_hover_text("spool_sync: read the printer's AMS — RFID spools are registered / updated").clicked() {
                    self.worker.call("spool_sync", json!({}), Kind::Spools);
                }
            });
        });
        // standing issues from the last sync: a spool the printer has that the inventory doesn't
        let mut register: Option<only3d_studio::spools::SpoolNotice> = None;
        for n in self.snap.spool_notices.clone() {
            egui::Frame::new().fill(theme::soft(p.warning)).stroke(egui::Stroke::new(1.0, p.warning.gamma_multiply(0.5))).corner_radius(4).inner_margin(8).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let c = crate::scene::parse_hex(&n.color);
                    let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), Sense::hover());
                    ui.painter().rect(r, 2.0, Color32::from_rgb(c[0], c[1], c[2]), egui::Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
                    ui.add(egui::Label::new(RichText::new(&n.message).size(12.0).color(p.text)).wrap());
                });
                match n.kind {
                    only3d_studio::spools::NoticeKind::Unregistered => {
                        if ui.add(theme::primary_button("Register this spool")).on_hover_text("Opens the spool form filled in from what the printer reports").clicked() {
                            register = Some(n.clone());
                        }
                    }
                    only3d_studio::spools::NoticeKind::WrongSpool => {
                        ui.label(muted("Move that spool (spool_update location), or register the loaded one."));
                    }
                    only3d_studio::spools::NoticeKind::Unmeasured => {
                        ui.label(muted("Weigh it (spool_weigh), or set what's left (spool_update remaining_g)."));
                    }
                }
            });
            ui.add_space(4.0);
        }
        if let Some(n) = register {
            let c = crate::scene::parse_hex(&n.color);
            self.close_spool_form();
            self.spool_form.material = n.material.clone();
            self.spool_form.color = Color32::from_rgb(c[0], c[1], c[2]);
            self.spool_form.location = n.location.clone();
            self.spool_form.grams = 1000.0;
            self.spool_form.open = true;
        }
        if self.spool_form.open {
            egui::Frame::new().fill(p.bg).stroke(egui::Stroke::new(1.0, p.border_variant)).corner_radius(4).inner_margin(8).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let presets: Vec<String> = self.presets.get("filament").map(|v| v.iter().map(|p| p.name.clone()).collect()).unwrap_or_default();
                let mut form = std::mem::take(&mut self.spool_form);
                egui::Grid::new("spool_form").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                    // the name: brand + line + colour unless typed (empty = automatic)
                    let c = form.color;
                    let hex = format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b());
                    let line = form.line.trim();
                    // spool_add files an unbranded spool under "Generic"
                    let brand = if form.brand.trim().is_empty() && form.editing.is_none() { "Generic" } else { form.brand.trim() };
                    let auto = only3d_studio::spool_names::auto_name(brand, (!line.is_empty()).then_some(line), form.material.trim(), &hex, form.filament_id.as_deref());
                    ui.label(muted("Name"));
                    ui.horizontal(|ui| {
                        let custom = !form.name.trim().is_empty();
                        let w = if custom { 168.0 } else { 190.0 };
                        ui.add(egui::TextEdit::singleline(&mut form.name).hint_text(RichText::new(&auto).color(p.placeholder)).desired_width(w))
                            .on_hover_text("Leave it empty to name the spool from brand, line and colour; it then follows them when they change");
                        if custom && theme::icon_button(ui, Icon::Reset, &format!("Back to the automatic name: {auto}")).clicked() {
                            form.name.clear();
                        }
                    });
                    ui.end_row();
                    ui.label(muted("Brand"));
                    ui.add(egui::TextEdit::singleline(&mut form.brand).hint_text("Generic").desired_width(190.0));
                    ui.end_row();
                    ui.label(muted("Line"));
                    let mat = if form.material.trim().is_empty() { "PETG".to_string() } else { form.material.trim().to_string() };
                    ui.add(egui::TextEdit::singleline(&mut form.line).hint_text(format!("e.g. {mat} Translucent")).desired_width(190.0))
                        .on_hover_text("The product line, as on the label (PETG Basic, PLA Matte). Empty: just the material");
                    ui.end_row();
                    ui.label(muted("Material"));
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("spool_mat").selected_text(form.material.as_str()).width(90.0).show_ui(ui, |ui| {
                            for m in crate::design_ui::MATERIALS {
                                ui.selectable_value(&mut form.material, m.to_string(), m);
                            }
                        });
                        ui.add(egui::TextEdit::singleline(&mut form.material).desired_width(70.0));
                    });
                    ui.end_row();
                    ui.label(muted("Preset"));
                    let matching: Vec<&String> = presets.iter().filter(|p| p.to_ascii_uppercase().contains(&form.material.to_ascii_uppercase())).collect();
                    egui::ComboBox::from_id_salt("spool_preset").selected_text(if form.preset.is_empty() { "pick later".to_string() } else { short(&form.preset) }).width(190.0).show_ui(ui, |ui| {
                        ui.selectable_value(&mut form.preset, String::new(), "pick later");
                        for p in matching.iter().take(60) {
                            ui.selectable_value(&mut form.preset, (*p).clone(), short(p));
                        }
                    });
                    ui.end_row();
                    ui.label(muted("Colour"));
                    egui::color_picker::color_edit_button_srgba(ui, &mut form.color, egui::color_picker::Alpha::Opaque);
                    ui.end_row();
                    ui.label(muted("Net weight"));
                    ui.add(egui::DragValue::new(&mut form.grams).range(10.0..=10000.0).speed(10.0).suffix(" g"));
                    ui.end_row();
                    if form.editing.is_none() {
                        ui.label(muted("Count"));
                        form.count = form.count.max(1);
                        ui.add(egui::DragValue::new(&mut form.count).range(1..=50).speed(0.1).suffix(" spools")).on_hover_text("Identical spools (a multi-pack); each gets its own id");
                        ui.end_row();
                    }
                    ui.label(muted("Empty spool"));
                    ui.horizontal(|ui| {
                        let mut has = form.tare.is_some();
                        if ui.checkbox(&mut has, "").on_hover_text("The empty spool's weight, so weighing it later tells what's left (Bambu reusable ≈ 250 g)").changed() {
                            form.tare = has.then_some(form.tare.unwrap_or(250.0));
                        }
                        if let Some(t) = form.tare.as_mut() {
                            ui.add(egui::DragValue::new(t).range(0.0..=2000.0).speed(1.0).suffix(" g"));
                        }
                    });
                    ui.end_row();
                    ui.label(muted("Location"));
                    self.location_combo(ui, "spool_loc", &mut form.location);
                    ui.end_row();
                    ui.label(muted("Price"));
                    ui.horizontal(|ui| {
                        let mut has = form.cost_per_kg.is_some();
                        if ui.checkbox(&mut has, "").changed() {
                            form.cost_per_kg = has.then_some(form.cost_per_kg.unwrap_or(20.0));
                        }
                        if let Some(c) = form.cost_per_kg.as_mut() {
                            ui.add(egui::DragValue::new(c).range(0.0..=500.0).speed(0.5).suffix(" /kg"));
                        }
                    });
                    ui.end_row();
                });
                self.spool_form = form;
                ui.horizontal(|ui| {
                    let editing = self.spool_form.editing.clone();
                    let label = if editing.is_some() { "Save" } else { "Add spool" };
                    if ui.add_enabled(!self.spool_form.material.trim().is_empty(), theme::primary_button(label)).clicked() {
                        let f = &self.spool_form;
                        let c = f.color;
                        let mut args = json!({ "material": f.material.trim(), "color": format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b()), "net_weight_g": f.grams });
                        if !f.location.trim().is_empty() {
                            args["location"] = json!(f.location.trim());
                        }
                        if !f.preset.is_empty() {
                            args["preset"] = json!(f.preset);
                        }
                        if let Some(t) = f.tare {
                            args["tare_g"] = json!(t);
                        }
                        if let Some(c) = f.cost_per_kg {
                            args["cost_per_kg"] = json!(c);
                        }
                        match &editing {
                            // "" for the name means automatic, for the line just the material
                            Some(id) => {
                                args["id"] = json!(id);
                                args["name"] = json!(f.name.trim());
                                args["brand"] = json!(f.brand.trim());
                                args["sub_brand"] = json!(f.line.trim());
                                self.worker.call("spool_update", args, Kind::Spools);
                            }
                            None => {
                                for (k, v) in [("name", f.name.trim()), ("brand", f.brand.trim()), ("sub_brand", f.line.trim())] {
                                    if !v.is_empty() {
                                        args[k] = json!(v);
                                    }
                                }
                                if f.count > 1 {
                                    args["quantity"] = json!(f.count);
                                }
                                self.worker.call("spool_add", args, Kind::Spools);
                            }
                        }
                        self.close_spool_form();
                    }
                    if ui.button("Cancel").clicked() {
                        self.close_spool_form();
                    }
                });
            });
        }
        scroll(ui, "spools_scroll", |ui| {
            if spools.is_empty() {
                ui.label(muted("No spools yet. Sync AMS registers Bambu RFID spools; add others with +, or ask an agent (spool_add)."));
            }
            for sp in &spools {
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.0), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(rect, 3.0, p.hover);
                }
                let c = crate::scene::parse_hex(&sp.color);
                let col = Color32::from_rgb(c[0], c[1], c[2]);
                ui.painter().circle_filled(egui::pos2(rect.min.x + 14.0, rect.center().y), 7.0, col);
                ui.painter().circle_stroke(egui::pos2(rect.min.x + 14.0, rect.center().y), 7.0, egui::Stroke::new(1.0, Color32::from_black_alpha(60)));
                ui.painter().circle_filled(egui::pos2(rect.min.x + 14.0, rect.center().y), 2.0, p.surface);
                let name = if sp.name.trim().is_empty() { sp.auto_name() } else { sp.name.clone() };
                let mut loc = format!("{} · {}", sp.id, sp.location.as_deref().map(only3d_studio::spools::location_name).unwrap_or_else(|| "—".into()));
                if !sp.calibration.is_empty() {
                    loc.push_str("  · calibrated");
                }
                // truncated before the grams bar
                let text_w = (rect.width() - 30.0 - 104.0).max(40.0);
                for (text, y, size, col) in [(name, 12.0, 12.5, p.text), (loc, 28.0, 11.0, p.muted)] {
                    let mut job = egui::text::LayoutJob::simple_singleline(text, egui::FontId::proportional(size), col);
                    job.wrap = egui::text::TextWrapping::truncate_at_width(text_w);
                    let g = ui.ctx().fonts_mut(|f| f.layout_job(job));
                    ui.painter().galley(egui::pos2(rect.min.x + 30.0, rect.min.y + y - g.size().y * 0.5), g, col);
                }
                let frac = (sp.remaining_g / sp.net_weight_g.max(1.0)).clamp(0.0, 1.0) as f32;
                let bar = egui::Rect::from_min_size(egui::pos2(rect.max.x - 96.0, rect.center().y + 4.0), egui::vec2(86.0, 4.0));
                ui.painter().rect_filled(bar, 2.0, p.border_variant);
                ui.painter().rect_filled(egui::Rect::from_min_size(bar.min, egui::vec2(bar.width() * frac, 4.0)), 2.0, if sp.low() { p.warning } else { p.accent });
                ui.painter().text(egui::pos2(rect.max.x - 10.0, rect.center().y - 6.0), egui::Align2::RIGHT_CENTER, format!("{:.0} g", sp.remaining_g), egui::FontId::monospace(11.0), if sp.low() { p.warning } else { p.text });
                let id = sp.id.clone();
                resp.on_hover_text(sp.line()).context_menu(|ui| {
                    ui.menu_button("Use in filament slot", |ui| {
                        for s in 1..=self.snap.project.filaments.len() {
                            if ui.button(format!("Slot {s}")).clicked() {
                                self.worker.call("set_filament", json!({ "slot": s, "spool": id }), Kind::Plain);
                                ui.close();
                            }
                        }
                    });
                    ui.menu_button("Move to", |ui| {
                        let mut loc = sp.location.clone().unwrap_or_default();
                        if self.location_combo(ui, &format!("move_{id}"), &mut loc) && !loc.is_empty() {
                            self.worker.call("spool_update", json!({ "id": id, "location": loc }), Kind::Spools);
                        }
                    });
                    if ui.button("Calibration…").on_hover_text("Flow ratio, pressure advance (K) and max volumetric speed for this spool; used when it is in a filament slot").clicked() {
                        let c = &sp.calibration;
                        self.tool_form_with("spool_update", json!({ "id": id, "calibration": { "flow_ratio": c.flow_ratio.unwrap_or(1.0), "pressure_advance": c.pressure_advance.unwrap_or(0.02), "max_volumetric_speed": c.max_volumetric_speed.unwrap_or(15.0) } }));
                        ui.close();
                    }
                    if ui.button("Weigh… (kitchen scale)").clicked() {
                        self.tool_form_with("spool_weigh", json!({ "id": id, "gross_g": 0 }));
                        ui.close();
                    }
                    if ui.button("Edit…").on_hover_text("Name, brand, line, colour, preset, weights and price").clicked() {
                        let c = crate::scene::parse_hex(&sp.color);
                        self.spool_form = crate::app::SpoolForm {
                            material: sp.material.clone(),
                            color: Color32::from_rgb(c[0], c[1], c[2]),
                            grams: sp.net_weight_g,
                            location: sp.location.clone().unwrap_or_default(),
                            open: true,
                            preset: sp.preset.clone().unwrap_or_default(),
                            tare: sp.tare_g,
                            cost_per_kg: sp.cost_per_kg,
                            brand: sp.brand.clone(),
                            line: sp.sub_brand.clone().unwrap_or_default(),
                            name: if sp.name_auto { String::new() } else { sp.name.clone() },
                            editing: Some(id.clone()),
                            filament_id: sp.filament_id.clone(),
                            count: 0,
                        };
                        ui.close();
                    }
                    if ui.button("Set what's left…").clicked() {
                        self.tool_form_with("spool_update", json!({ "id": id, "remaining_g": sp.remaining_g.round() }));
                        ui.close();
                    }
                    if ui.button("Ledger").clicked() {
                        self.tool_form_with("spool_detail", json!({ "id": id }));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Archive…").clicked() {
                        self.dialog = Some(Dialog::Confirm { title: "Archive spool?".into(), body: format!("{id} is hidden from the inventory (its history is kept)."), tool: "spool_update".into(), args: json!({ "id": id, "archived": true }) });
                        ui.close();
                    }
                });
            }
            ui.add_space(8.0);
            self.fit_section(ui);
        });
    }

    /// Close the spool form and leave edit mode.
    fn close_spool_form(&mut self) {
        let f = &mut self.spool_form;
        f.open = false;
        f.editing = None;
        f.name.clear();
        f.filament_id = None;
        f.count = 0;
    }

    // ------------------------------------------------------------------------------------------
    // printer

    /// State, job, progress and temperatures from the latest `printer_status`.
    fn printer_live(&mut self, ui: &mut Ui, d: &Value) {
        let p = pal();
        let state = d.get("gcode_state").and_then(Value::as_str).unwrap_or("?").to_ascii_uppercase();
        let busy = matches!(state.as_str(), "RUNNING" | "PREPARE" | "PAUSE" | "SLICING");
        let colour = match state.as_str() {
            "RUNNING" | "PREPARE" | "SLICING" => p.accent,
            "PAUSE" => p.warning,
            "FAILED" => p.error,
            "FINISH" => p.success,
            _ => p.placeholder,
        };
        let label = match state.as_str() {
            "FAILED" => "stopped / failed".to_string(),
            "FINISH" => "finished".to_string(),
            "IDLE" => "idle".to_string(),
            other => other.to_lowercase(),
        };
        ui.horizontal(|ui| {
            theme::dot_chip(ui, &label, colour);
            if let Some(j) = d.get("subtask_name").and_then(Value::as_str).filter(|j| !j.is_empty()) {
                ui.label(RichText::new(j).strong().size(12.5));
            }
        });
        // the machine's current stage: heating, leveling, purifying the air after a job...
        if let Some(sn) = d.get("stage_name").and_then(Value::as_str).filter(|n| *n != "Printing") {
            ui.label(RichText::new(sn).size(12.0).color(p.accent));
        }
        let pct = d.get("progress_percent").and_then(Value::as_u64).unwrap_or(0);
        if busy || state == "FINISH" {
            let mut text = format!("{pct}%");
            if let (Some(l), Some(t)) = (d.get("layer").and_then(Value::as_u64), d.get("total_layers").and_then(Value::as_u64)) {
                text.push_str(&format!(" · layer {l}/{t}"));
            }
            if busy {
                if let Some(m) = d.get("remaining_minutes").and_then(Value::as_u64) {
                    text.push_str(&format!(" · {} left", fmt_duration(m as f64 * 60.0)));
                }
            }
            ui.add(egui::ProgressBar::new(pct as f32 / 100.0).text(RichText::new(text).size(11.5)).fill(colour).desired_height(18.0));
        }
        let t = |v: Option<&Value>| -> Option<String> {
            let v = v?;
            let cur = v.get("current").and_then(Value::as_f64)?;
            Some(match v.get("target").and_then(Value::as_f64).filter(|x| *x > 0.0) {
                Some(tg) => format!("{cur:.0} → {tg:.0} °C"),
                None => format!("{cur:.0} °C"),
            })
        };
        egui::Grid::new("printer_temps").num_columns(2).spacing([12.0, 2.0]).show(ui, |ui| {
            let mut nozzles: Vec<&Value> = d.get("nozzles").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
            // left before right, like the machine
            nozzles.sort_by_key(|n| n.get("side").and_then(Value::as_str) != Some("left"));
            for n in nozzles {
                let side = n.get("side").and_then(Value::as_str).unwrap_or("");
                let active = n.get("active").and_then(Value::as_bool).unwrap_or(false);
                ui.label(muted(format!("{} nozzle{}", if side.is_empty() { "" } else { side }, if active { " ●" } else { "" })));
                ui.label(RichText::new(t(n.get("temperature")).unwrap_or("—".into())).monospace().size(12.0));
                ui.end_row();
            }
            for (k, l) in [("bed", "bed"), ("chamber", "chamber")] {
                if let Some(v) = t(d.get(k)) {
                    ui.label(muted(l));
                    ui.label(RichText::new(v).monospace().size(12.0));
                    ui.end_row();
                }
            }
            // fans and doors of the air duct that are running (e.g. the purifier after a job)
            if let Some(parts) = d.pointer("/air/parts").and_then(Value::as_array) {
                for part in parts.iter().filter(|q| q.get("percent").and_then(Value::as_u64).unwrap_or(0) > 0) {
                    ui.label(muted(part.get("name").and_then(Value::as_str).unwrap_or("air")));
                    ui.label(RichText::new(format!("{}%", part.get("percent").and_then(Value::as_u64).unwrap_or(0))).monospace().size(12.0));
                    ui.end_row();
                }
            }
        });
    }

    /// Each AMS unit: model, the nozzle it feeds, humidity and temperature, its slots, and drying.
    fn printer_ams(&mut self, ui: &mut Ui, d: &Value) {
        let p = pal();
        let dual = d.get("nozzles").and_then(Value::as_array).is_some_and(|n| n.len() >= 2);
        let units = d.get("ams").and_then(Value::as_array).cloned().unwrap_or_default();
        for (k, u) in units.iter().enumerate() {
            if k > 0 {
                ui.add_space(6.0);
            }
            let s = |key: &str| u.get(key).and_then(Value::as_str).filter(|v| !v.is_empty());
            let name = s("name").map(String::from).unwrap_or_else(|| u.get("id").and_then(Value::as_u64).unwrap_or(0).to_string());
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{name} · {}", s("model").unwrap_or("AMS"))).strong().size(12.5));
                // firmware numbering: extruder 0 is the right nozzle, 1 the left
                if let Some(e) = u.get("extruder").and_then(Value::as_u64).filter(|_| dual) {
                    ui.label(muted(format!("→ {} nozzle", if e == 0 { "right" } else { "left" })).size(11.5));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let climate: Vec<String> = [
                        u.get("temperature").and_then(Value::as_f64).map(|c| format!("{c:.0} °C")),
                        u.get("humidity_percent").and_then(Value::as_u64).map(|h| format!("{h}% RH")).or_else(|| u.get("humidity_level").and_then(Value::as_u64).map(|l| format!("humidity {l}/5"))),
                    ]
                    .into_iter()
                    .flatten()
                    .collect();
                    ui.label(RichText::new(climate.join("  ")).monospace().size(11.5).color(p.muted));
                });
            });
            // one slot per row: a filament name doesn't fit beside three others in the side panel
            for t in u.get("trays").and_then(Value::as_array).into_iter().flatten() {
                ui.horizontal(|ui| {
                    let slot = t.get("slot_id").and_then(Value::as_u64).unwrap_or(0);
                    let label = if name.starts_with("HT") { name.clone() } else { format!("{name}{}", slot + 1) };
                    let loaded = t.get("loaded").and_then(Value::as_bool).unwrap_or(false);
                    let reading = t.get("reading").and_then(Value::as_bool).unwrap_or(false);
                    let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), Sense::hover());
                    match t.get("color").and_then(Value::as_str).filter(|_| loaded) {
                        Some(c) => {
                            let c = crate::scene::parse_hex(c);
                            ui.painter().rect(r, 2.0, Color32::from_rgb(c[0], c[1], c[2]), egui::Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
                        }
                        None => {
                            ui.painter().rect_stroke(r, 2.0, egui::Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
                        }
                    }
                    let what = if reading {
                        "reading…".to_string()
                    } else if loaded {
                        [t.get("sub_brand").or(t.get("filament_type")).and_then(Value::as_str).map(String::from), t.get("remain_percent").and_then(Value::as_u64).map(|r| format!("{r}%"))]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(" ")
                    } else {
                        "empty".to_string()
                    };
                    ui.label(RichText::new(label).monospace().size(11.5).color(p.muted));
                    let text = RichText::new(what).size(11.5);
                    ui.label(if loaded || reading { text.color(p.text) } else { text.color(p.placeholder) });
                });
            }
            if let Some(dr) = u.get("drying") {
                let state = dr.get("state").and_then(Value::as_str).unwrap_or("off");
                let summary = dr.get("summary").and_then(Value::as_str).unwrap_or("");
                match state {
                    "checking" | "drying" | "stopping" => {
                        let left = dr.get("remaining_minutes").and_then(Value::as_f64);
                        let total = dr.get("duration_hours").and_then(Value::as_f64).map(|h| h * 60.0).filter(|t| *t > 0.0);
                        match (left, total) {
                            (Some(l), Some(t)) => {
                                ui.add(egui::ProgressBar::new((1.0 - l / t).clamp(0.0, 1.0) as f32).text(RichText::new(summary).size(11.5)).fill(p.accent).desired_height(18.0));
                            }
                            _ => {
                                ui.label(RichText::new(summary).size(11.5).color(p.accent));
                            }
                        }
                    }
                    "error" | "heater_fault" => {
                        ui.label(RichText::new(summary).size(11.5).color(p.error));
                    }
                    _ => {
                        ui.label(muted(summary).size(11.5));
                    }
                }
            }
        }
        if let Some(st) = d.get("ams_state") {
            if let Some(act) = st.get("activity").and_then(Value::as_str) {
                ui.label(RichText::new(format!("AMS is {act}")).size(11.5).color(p.accent));
            }
            if st.get("read_rfid_on_insert").and_then(Value::as_bool) == Some(false) {
                ui.label(RichText::new("\"Insertion update\" is off: Bambu spools aren't read when inserted.").size(11.5).color(p.warning));
            }
        }
    }

    fn printer_panel(&mut self, ui: &mut Ui) {
        let p = pal();
        scroll(ui, "printer_scroll", |ui| {
            if self.printers.is_empty() {
                theme::section_label(ui, "Printer");
                ui.label(muted("No printer configured. Discover printers on your network, then add one with its 8-digit LAN access code (printer screen → Settings → Network) via the command palette: printer_add."));
                ui.add_space(4.0);
                if ui.button("Discover printers…").clicked() {
                    self.worker.call("printer_discover", json!({}), Kind::PrinterInfo);
                }
                if ui.button("Add printer…").clicked() {
                    self.tool_form_with("printer_add", json!({ "name": "H2D", "host": "", "serial": "", "access_code": "" }));
                }
            } else {
                let name = self.default_printer.clone().unwrap_or_else(|| self.printers[0].clone());
                ui.horizontal(|ui| {
                    theme::section_label(ui, &name);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let checking = self.worker.busy_with(|k| *k == Kind::PrinterStatus);
                        if checking {
                            ui.add(egui::Spinner::new().size(12.0).color(p.accent));
                        } else if ui.add(theme::ghost_button(RichText::new("Refresh").size(12.0))).on_hover_text("printer_status").clicked() {
                            self.worker.call("printer_status", json!({}), Kind::PrinterStatus);
                        }
                    });
                });
                self.printer_panel_frame = self.frames;
                if let Some(d) = self.printer_data.clone() {
                    self.printer_live(ui, &d);
                }
                // alerts the printer raised (HMS, print errors, missing storage), decoded
                let alerts: Vec<String> = match &self.printer_status {
                    Some(Ok(t)) => t.lines().filter(|l| l.starts_with("HMS") || l.starts_with("Print error") || l.starts_with("No removable storage")).map(str::to_string).collect(),
                    _ => Vec::new(),
                };
                if !alerts.is_empty() {
                    ui.add_space(4.0);
                    egui::Frame::new().fill(theme::soft(p.warning)).stroke(egui::Stroke::new(1.0, p.warning)).corner_radius(4).inner_margin(8).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        for a in &alerts {
                            ui.label(RichText::new(a).size(11.5).color(p.text));
                        }
                    });
                }
                if let Some(d) = self.printer_data.clone().filter(|d| d.get("ams").and_then(Value::as_array).is_some_and(|a| !a.is_empty())) {
                    ui.add_space(4.0);
                    if self.section_header(ui, "printer.ams", "AMS", true, false, |_| {}) {
                        self.printer_ams(ui, &d);
                    }
                }
                ui.add_space(4.0);
                if self.section_header(ui, "printer.camera", "Camera", true, false, |_| {}) {
                    self.printer_cam.want(ui.ctx(), Some(name.clone()));
                    let (tex, age, err) = self.printer_cam.frame(ui.ctx());
                    if let Some(t) = &tex {
                        let w = ui.available_width();
                        let sz = t.size_vec2();
                        ui.add(egui::Image::new(t).fit_to_exact_size(egui::vec2(w, w * sz.y / sz.x.max(1.0))).corner_radius(4));
                    }
                    match (&err, age) {
                        (Some(e), _) => {
                            ui.label(RichText::new(e).size(11.0).color(p.error));
                        }
                        (None, Some(a)) => {
                            ui.label(muted(format!("live · updated {:.0} s ago", a.as_secs_f32())).size(11.0));
                        }
                        (None, None) => {
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new().size(12.0).color(p.accent));
                                ui.label(muted("Connecting to the chamber camera…").size(11.0));
                            });
                        }
                    }
                }
                if self.section_header(ui, "printer.details", "Details", false, false, |_| {}) {
                    if let Some(st) = &self.printer_status {
                        let (text, err) = match st {
                            Ok(t) => (t.clone(), false),
                            Err(e) => (e.clone(), true),
                        };
                        egui::Frame::new().fill(if err { theme::soft(p.error) } else { p.bg }).stroke(egui::Stroke::new(1.0, p.border_variant)).corner_radius(4).inner_margin(8).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new(text).monospace().size(11.0).color(if err { p.error } else { p.text }));
                        });
                    } else {
                        ui.label(muted("Status not read yet — Refresh."));
                    }
                }
                if let Some(Err(e)) = &self.printer_status {
                    ui.label(RichText::new(e).size(11.5).color(p.error));
                }
                ui.add_space(4.0);
                self.timelapses_section(ui, &name);
                ui.add_space(4.0);
                if self.section_header(ui, "printer.controls", "Controls", true, false, |_| {}) {
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Pause").clicked() {
                            self.worker.call("printer_control", json!({ "action": "pause" }), Kind::Report);
                        }
                        if ui.button("Resume").clicked() {
                            self.worker.call("printer_control", json!({ "action": "resume" }), Kind::Report);
                        }
                        if ui.button(RichText::new("Stop…").color(p.error)).clicked() {
                            self.dialog = Some(Dialog::Confirm { title: "Stop the print?".into(), body: "The running print is cancelled. This cannot be undone.".into(), tool: "printer_control".into(), args: json!({ "action": "stop", "confirm": true }) });
                        }
                        if ui.button("Light on").clicked() {
                            self.worker.call("printer_control", json!({ "action": "light_on" }), Kind::Report);
                        }
                        if ui.button("Light off").clicked() {
                            self.worker.call("printer_control", json!({ "action": "light_off" }), Kind::Report);
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        if theme::bracket_button(ui, "temperatures & fans").clicked() {
                            self.tool_form_with("printer_set", json!({}));
                        }
                        if theme::bracket_button(ui, "ams").clicked() {
                            self.tool_form_with("printer_ams", json!({ "action": "set_slot", "ams_id": 0, "slot": 0 }));
                        }
                        if theme::bracket_button(ui, "files").clicked() {
                            self.open_tool("printer_files");
                        }
                    });
                }
                ui.add_space(4.0);
                let w = ui.available_width();
                if ui.add_enabled(self.print_modal.is_none() && !self.snap.plate_is_empty(), theme::primary_button("Print plate…").min_size(egui::vec2(w, 28.0))).clicked() {
                    self.start_print();
                }
                ui.add_space(6.0);
                if self.section_header(ui, "printer.list", "Printers", false, false, |_| {}) {
                    for pr in self.printers.clone() {
                        let d = self.default_printer.as_deref() == Some(pr.as_str());
                        ui.label(RichText::new(format!("{pr}{}", if d { "  (default)" } else { "" })).size(12.5));
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Discover…").clicked() {
                            self.worker.call("printer_discover", json!({}), Kind::PrinterInfo);
                        }
                        if ui.button("Add…").clicked() {
                            self.tool_form_with("printer_add", json!({ "name": "", "host": "", "serial": "", "access_code": "" }));
                        }
                    });
                }
            }
            if let Some(t) = &self.printer_list_text {
                ui.add_space(4.0);
                egui::Frame::new().fill(p.bg).stroke(egui::Stroke::new(1.0, p.border_variant)).corner_radius(4).inner_margin(8).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(t).monospace().size(11.0));
                });
            }
            if self.worker.busy_with(|k| *k == Kind::PrinterInfo) {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(12.0).color(p.accent));
                    ui.label(muted("Listening for printers…"));
                });
            }
        });
    }

    // ------------------------------------------------------------------------------------------
    // checks + activity

    fn checks_panel(&mut self, ui: &mut Ui) {
        let p = pal();
        let f = self.snap.findings.clone();
        let (e, w) = (f.iter().filter(|x| x.severity == Severity::Error).count(), f.iter().filter(|x| x.severity == Severity::Warning).count());
        ui.horizontal(|ui| {
            if e > 0 {
                theme::chip(ui, &format!("{e} error{}", if e == 1 { "" } else { "s" }), p.error, theme::soft(p.error));
            }
            if w > 0 {
                theme::chip(ui, &format!("{w} warning{}", if w == 1 { "" } else { "s" }), p.warning, theme::soft(p.warning));
            }
            if e == 0 && w == 0 {
                let ready = if self.snap.project.objects.iter().any(|o| o.plate == self.snap.plate) { "ready to slice" } else { "ready to start" };
                theme::chip(ui, ready, p.success, theme::soft(p.success));
            }
        });
        let mut pick: Option<String> = None;
        scroll(ui, "findings", |ui| {
            if f.is_empty() {
                ui.label(muted("All checks passed."));
            }
            for x in f.iter() {
                let col = match x.severity {
                    Severity::Error => p.error,
                    Severity::Warning => p.warning,
                    Severity::Info if x.code == "spec_ok" || x.code == "empty_plate" => p.success,
                    Severity::Info => p.accent,
                };
                if x.code == "empty_plate" {
                    ui.horizontal(|ui| {
                        let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 16.0), Sense::hover());
                        ui.painter().rect_filled(egui::Rect::from_center_size(dot.center(), egui::vec2(6.0, 6.0)), 1.0, col);
                        ui.label(RichText::new("No objects, ready to start").size(12.5).color(col));
                    })
                    .response
                    .on_hover_text(format!("Add a primitive, import a model ({}) or drop a file on the plate", crate::keymap::cmd_key("I")));
                    continue;
                }
                // saved design checks: their id leads the message ("✓ c2 (…): …")
                let spec_id = x.code.starts_with("spec_").then(|| x.message.trim_start_matches(['✓', '✗', ' ']).split([' ', ':']).next().unwrap_or("").to_string()).filter(|s| !s.is_empty());
                let r = ui
                    .horizontal(|ui| {
                        let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 16.0), Sense::hover());
                        ui.painter().rect_filled(egui::Rect::from_center_size(dot.center(), egui::vec2(6.0, 6.0)), 1.0, col);
                        if let Some(id) = &spec_id {
                            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                                if theme::icon_button(ui, Icon::Close, &format!("Remove check {id}")).clicked() {
                                    self.worker.call("check", json!({ "remove": id }), Kind::Report);
                                }
                                ui.add(egui::Label::new(RichText::new(&x.message).size(12.5)).wrap().sense(Sense::click()))
                            })
                            .inner
                        } else {
                            ui.add(egui::Label::new(RichText::new(&x.message).size(12.5)).wrap().sense(Sense::click()))
                        }
                    })
                    .inner;
                let r = r.on_hover_text(format!("{} → {}", x.code, x.hint));
                if r.clicked() {
                    pick = x.object.clone();
                }
            }
        });
        self.add_check_form(ui);
        if let Some(id) = pick {
            self.select(vec![id]);
        }
    }

    fn activity_panel(&mut self, ui: &mut Ui) {
        let p = pal();
        let now = only3d_studio::now_ms();
        let mut hovered: Option<Vec<String>> = None;
        let mut toggle: Option<(u128, String)> = None;
        let mut select: Option<Vec<String>> = None;
        egui::ScrollArea::vertical().id_salt("activity").auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            if self.snap.journal.is_empty() {
                ui.label(muted("Every tool call — yours and your agents' — shows up here."));
            }
            for e in self.snap.journal.iter().rev().take(200).collect::<Vec<_>>().into_iter().rev() {
                let open = self.activity_open.as_ref().is_some_and(|(at, tool)| *at == e.at_ms && *tool == e.tool);
                let who = match e.actor {
                    only3d_studio::Actor::User => "you",
                    only3d_studio::Actor::Agent => "agent",
                    only3d_studio::Actor::Script => "auto",
                };
                // the line: click opens the details, right-click copies
                let line = egui::Frame::new().inner_margin(Margin::symmetric(4, 2)).corner_radius(3.0).fill(if open { p.hover } else { Color32::TRANSPARENT }).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let age = now.saturating_sub(e.at_ms) / 1000;
                        let t = if age < 10 {
                            "now".to_string()
                        } else if age < 60 {
                            format!("{}s", age / 10 * 10)
                        } else if age < 3600 {
                            format!("{}m", age / 60)
                        } else {
                            format!("{}h", age / 3600)
                        };
                        ui.add_sized([30.0, 16.0], egui::Label::new(RichText::new(t).size(10.5).monospace().color(p.placeholder)).selectable(false));
                        let col = match e.actor {
                            only3d_studio::Actor::Agent => p.accent,
                            _ => p.muted,
                        };
                        ui.add_sized([42.0, 16.0], egui::Label::new(RichText::new(who).size(10.5).monospace().color(col)).selectable(false));
                        ui.add(egui::Label::new(RichText::new(&e.tool).size(12.0).monospace().color(if e.ok { p.text } else { p.error })).selectable(false));
                        let summary: String = e.summary.chars().take(150).collect();
                        ui.add(egui::Label::new(RichText::new(summary).size(12.0).color(if e.ok { p.muted } else { p.error })).truncate().selectable(false));
                    });
                });
                let r = ui.interact(line.response.rect, egui::Id::unique(("activity_row", e.at_ms, &e.tool)), Sense::click());
                if r.hovered() {
                    if !open {
                        ui.painter().rect_filled(line.response.rect, 3.0, p.hover.gamma_multiply(0.6));
                    }
                    hovered = Some(e.objects.clone());
                }
                let text = activity_text(e, who);
                let r = r.on_hover_text_at_pointer(if open { "Click to close · right-click to copy" } else { "Click for the full call and result · right-click to copy" });
                if r.clicked() {
                    toggle = Some((e.at_ms, e.tool.clone()));
                }
                r.context_menu(|ui| {
                    if ui.button("Copy line").clicked() {
                        ui.ctx().copy_text(format!("{} {}: {}", who, e.tool, e.summary));
                        ui.close();
                    }
                    if ui.button("Copy call and result").clicked() {
                        ui.ctx().copy_text(text.clone());
                        ui.close();
                    }
                    if !e.args.is_empty() && ui.button("Copy arguments (JSON)").clicked() {
                        ui.ctx().copy_text(e.args.clone());
                        ui.close();
                    }
                    if !e.objects.is_empty() && ui.button(format!("Select {}", e.objects.join(", "))).clicked() {
                        select = Some(e.objects.clone());
                        ui.close();
                    }
                });
                // the details: plain text you can select and copy, nothing else happens
                if open && self.activity_reveal {
                    ui.scroll_to_rect(line.response.rect, Some(Align::TOP));
                    self.activity_reveal = false;
                }
                if open {
                    egui::Frame::new().fill(p.bg).stroke(egui::Stroke::new(1.0, p.border_variant)).corner_radius(4).inner_margin(Margin::symmetric(8, 6)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.add(egui::Label::new(RichText::new(&text).size(11.5).monospace().color(if e.ok { p.text } else { p.error })).wrap().selectable(true));
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            if theme::bracket_button(ui, "copy").on_hover_text("Copy the call and its result").clicked() {
                                ui.ctx().copy_text(text.clone());
                            }
                            if !e.args.is_empty() && theme::bracket_button(ui, "copy json").on_hover_text("Copy the arguments, to run the call again").clicked() {
                                ui.ctx().copy_text(e.args.clone());
                            }
                            if !e.objects.is_empty() && theme::bracket_button(ui, "select").on_hover_text(e.objects.join(", ")).clicked() {
                                select = Some(e.objects.clone());
                            }
                        });
                    });
                }
            }
        });
        if let Some(ids) = hovered {
            self.hover_objects = (ids, ui.ctx().cumulative_pass_nr());
        }
        if let Some((at, tool)) = toggle {
            let was_open = self.activity_open.as_ref().is_some_and(|(a, t)| *a == at && *t == tool);
            self.activity_open = if was_open { None } else { Some((at, tool)) };
            self.activity_reveal = !was_open;
        }
        if let Some(ids) = select {
            let present: Vec<String> = ids.into_iter().filter(|id| self.snap.objects.iter().any(|o| o.id == *id)).collect();
            if !present.is_empty() {
                self.select(present);
            }
        }
        // the "now / 12s / 3m" ages: redraw while they change, and not otherwise
        if let Some(last) = self.snap.journal.last() {
            let age = now.saturating_sub(last.at_ms) as u64;
            let step = if age < 60_000 { 10_000 } else if age < 3_600_000 { 60_000 } else { 3_600_000 };
            let next = step - age % step;
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(next));
        }
    }
}

/// One activity entry as plain text: who called what with which arguments, and what came back.
fn activity_text(e: &only3d_studio::JournalEntry, who: &str) -> String {
    let mut t = format!("{who} · {}{}", e.tool, if e.args.is_empty() { String::new() } else { format!(" {}", e.args) });
    t.push('\n');
    t.push_str(if e.detail.is_empty() { &e.summary } else { &e.detail });
    if !e.objects.is_empty() {
        t.push_str(&format!("\nobjects: {}", e.objects.join(", ")));
    }
    t.push_str(&format!("\nrevision {}", e.revision));
    t
}
