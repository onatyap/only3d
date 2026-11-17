//! Window parts for functional design: adding the project's own checks (Checks panel) and fit
//! calibration (Spools panel). Both are thin forms over the `check` and `fit` tools.

use eframe::egui::{self, RichText};
use serde_json::{json, Value};

use crate::app::App;
use crate::panels::muted;
use crate::theme::{self, pal};
use crate::worker::Kind;

/// The "Add a check" form.
#[derive(Clone, Debug)]
pub struct CheckForm {
    pub open: bool,
    pub kind: String,
    pub object: String,
    pub other: String,
    pub axis: String,
    pub z: f64,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub note: String,
}

impl Default for CheckForm {
    fn default() -> Self {
        CheckForm { open: false, kind: "size".into(), object: String::new(), other: String::new(), axis: "x".into(), z: 5.0, min: None, max: Some(100.0), note: String::new() }
    }
}

/// The fit calibration form: offsets found on the printed coupon.
#[derive(Clone, Debug, Default)]
pub struct FitForm {
    pub slip: Option<f64>,
    pub press: Option<f64>,
    /// The last `fit show` text.
    pub shown: Option<String>,
}

fn limit(ui: &mut egui::Ui, label: &str, v: &mut Option<f64>, unit: &str) {
    let mut on = v.is_some();
    if ui.checkbox(&mut on, label).changed() {
        *v = if on { Some(v.unwrap_or(1.0)) } else { None };
    }
    if let Some(x) = v.as_mut() {
        ui.add(egui::DragValue::new(x).speed(0.05).max_decimals(2).suffix(unit));
    }
}

impl App {
    /// Below the findings: a remove button per saved check is in the list; this is the form.
    pub(crate) fn add_check_form(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        ui.add_space(6.0);
        if !self.check_form.open {
            if theme::bracket_button(ui, "add a check").on_hover_text("Save a design check with the project: it reruns after every change").clicked() {
                self.check_form.open = true;
                if let Some(id) = self.snap.selection.first() {
                    self.check_form.object = id.clone();
                }
            }
            return;
        }
        let ids: Vec<String> = self.snap.plate_objects().map(|o| o.id.clone()).collect();
        let mut form = self.check_form.clone();
        let f = &mut form;
        let (mut save, mut cancel) = (false, false);
        egui::Frame::new().fill(p.bg).stroke(egui::Stroke::new(1.0, p.border_variant)).corner_radius(4).inner_margin(8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::Grid::new("check_form").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                ui.label("Check");
                egui::ComboBox::from_id_salt("check_kind").selected_text(f.kind.as_str()).show_ui(ui, |ui| {
                    for (k, what) in [("size", "an object's size along an axis"), ("clearance", "the gap between two objects, as placed"), ("wall", "the thinnest wall at a height"), ("overhang", "steep overhang area")] {
                        ui.selectable_value(&mut f.kind, k.to_string(), format!("{k} · {what}"));
                    }
                });
                ui.end_row();
                ui.label("Object");
                egui::ComboBox::from_id_salt("check_obj").selected_text(f.object.as_str()).show_ui(ui, |ui| {
                    for id in &ids {
                        ui.selectable_value(&mut f.object, id.clone(), id.as_str());
                    }
                });
                ui.end_row();
                match f.kind.as_str() {
                    "size" => {
                        ui.label("Axis");
                        ui.horizontal(|ui| {
                            for a in ["x", "y", "z"] {
                                ui.selectable_value(&mut f.axis, a.to_string(), a);
                            }
                        });
                        ui.end_row();
                    }
                    "clearance" => {
                        ui.label("To");
                        egui::ComboBox::from_id_salt("check_other").selected_text(f.other.as_str()).show_ui(ui, |ui| {
                            for id in &ids {
                                ui.selectable_value(&mut f.other, id.clone(), id.as_str());
                            }
                        });
                        ui.end_row();
                    }
                    "wall" => {
                        ui.label("At height");
                        ui.add(egui::DragValue::new(&mut f.z).speed(0.5).suffix(" mm"));
                        ui.end_row();
                    }
                    _ => {}
                }
                let unit = if f.kind == "overhang" { " mm²" } else { " mm" };
                ui.label("Limits");
                ui.horizontal(|ui| {
                    limit(ui, "min", &mut f.min, unit);
                    limit(ui, "max", &mut f.max, unit);
                });
                ui.end_row();
                ui.label("Why");
                ui.add(egui::TextEdit::singleline(&mut f.note).hint_text("fits the 60 mm drawer").desired_width(f32::INFINITY));
                ui.end_row();
            });
            ui.horizontal(|ui| {
                let ready = !f.object.is_empty() && (f.min.is_some() || f.max.is_some()) && (f.kind != "clearance" || !f.other.is_empty());
                save = ui.add_enabled(ready, theme::primary_button("Save check")).clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        if save {
            let f = &form;
            {
                {
                    let mut spec = json!({"kind": f.kind, "object": f.object});
                    match f.kind.as_str() {
                        "size" => spec["axis"] = json!(f.axis),
                        "clearance" => spec["other"] = json!(f.other),
                        "wall" => spec["z"] = json!(f.z),
                        _ => {}
                    }
                    if let Some(v) = f.min {
                        spec["min"] = json!(v);
                    }
                    if let Some(v) = f.max {
                        spec["max"] = json!(v);
                    }
                    if !f.note.trim().is_empty() {
                        spec["note"] = json!(f.note.trim());
                    }
                    self.worker.call("check", json!({ "add": spec }), Kind::Report);
                }
            }
            form.open = false;
        }
        if cancel {
            form.open = false;
        }
        self.check_form = form;
    }

    /// The Spools panel's fit calibration: what is calibrated, a coupon to print, and what was
    /// found on it.
    pub(crate) fn fit_section(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        if !self.section_header(ui, "spools.fit", "Fit calibration", false, false, |_| {}) {
            return;
        }
        let fil = self.snap.project.filaments.first().map(|f| crate::settings_ui::short(&f.preset)).unwrap_or_default();
        ui.label(muted(format!("Holes get the slip fit learned for this printer and {fil}. Print the coupon, push the pin into each hole, then note which one it slides into and which one holds it.")));
        ui.horizontal(|ui| {
            if ui.button("Add a test coupon").on_hover_text("A bar with 5 mm holes at +0 … +0.5 mm (labelled) and a 5 mm pin, on this plate").clicked() {
                self.worker.call("fit", json!({"action": "coupon"}), Kind::Report);
            }
            if ui.button("Show fits").clicked() {
                self.read_tool("fit", json!({"action": "show"}), Kind::FitShown);
            }
        });
        let offs = only3d_studio::fit::OFFSETS;
        let pick = |ui: &mut egui::Ui, id: &str, v: &mut Option<f64>| {
            egui::ComboBox::from_id_salt(id).selected_text(v.map(|x| format!("+{x:.1}")).unwrap_or_else(|| "—".into())).show_ui(ui, |ui| {
                ui.selectable_value(v, None, "—");
                for o in offs {
                    ui.selectable_value(v, Some(o), format!("+{o:.1}"));
                }
            });
        };
        let mut f = self.fit_form.clone();
        egui::Grid::new("fit_form").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            ui.label("Slides in");
            pick(ui, "fit_slip", &mut f.slip);
            ui.end_row();
            ui.label("Held by");
            pick(ui, "fit_press", &mut f.press);
            ui.end_row();
        });
        if ui.add_enabled(f.slip.is_some() || f.press.is_some(), theme::primary_button("Save fit")).clicked() {
            let mut args = json!({"action": "record"});
            if let Some(v) = f.slip {
                args["slip"] = json!(v);
            }
            if let Some(v) = f.press {
                args["press"] = json!(v);
            }
            self.worker.call("fit", args, Kind::Report);
            f.shown = None;
        }
        if let Some(t) = &f.shown {
            ui.add(egui::Label::new(RichText::new(t).size(11.0).monospace().color(p.text)).wrap().selectable(true));
        }
        self.fit_form = f;
        let _: Option<Value> = None;
    }
}

/// The Copies window: how many copies of an object, and where they go.
#[derive(Clone, Debug)]
pub struct CopiesForm {
    pub id: String,
    /// "free" (nearest free space) | "row" | "grid" | "fill"
    pub mode: &'static str,
    pub count: u32,
    pub direction: &'static str,
    pub rows: u32,
    pub cols: u32,
    pub gap: f32,
}

impl CopiesForm {
    pub fn new(id: &str) -> Self {
        CopiesForm { id: id.to_string(), mode: "free", count: 3, direction: "+x", rows: 3, cols: 3, gap: 5.0 }
    }

    /// The `duplicate_object` arguments for the form.
    pub fn args(&self) -> Value {
        let mut a = json!({"id": self.id, "gap": self.gap});
        match self.mode {
            "row" => {
                a["copies"] = json!(self.count);
                a["direction"] = json!(self.direction);
            }
            "grid" => a["grid"] = json!([self.rows, self.cols]),
            "fill" => a["fill"] = json!(true),
            _ => a["copies"] = json!(self.count),
        }
        a
    }

    /// What pressing the button will add, in words.
    pub fn summary(&self) -> String {
        match self.mode {
            "row" => format!("{} cop{} in a row {}", self.count, if self.count == 1 { "y" } else { "ies" }, match self.direction {
                "+x" => "to the right",
                "-x" => "to the left",
                "+y" => "to the back",
                _ => "to the front",
            }),
            "grid" => format!("a {} × {} grid with the original in the front-left corner ({} new)", self.rows, self.cols, (self.rows * self.cols).saturating_sub(1)),
            "fill" => "as many copies as fit in the plate's free space".into(),
            _ => format!("{} cop{} in the nearest free space; nothing else moves", self.count, if self.count == 1 { "y" } else { "ies" }),
        }
    }
}

impl App {
    /// Returns true when the window should close.
    pub(crate) fn copies_dialog(&mut self, ui: &mut egui::Ui, f: &mut CopiesForm, enter: bool) -> bool {
        let p = pal();
        ui.set_width(420.0);
        let name = self.snap.project.object(&f.id).map(|o| if o.name.is_empty() { o.id.clone() } else { o.name.clone() }).unwrap_or_else(|| f.id.clone());
        ui.label(RichText::new(format!("Copies of {name}")).family(theme::medium()).size(14.0));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            for (m, label) in [("free", "Next to it"), ("row", "In a row"), ("grid", "Grid (matrix)"), ("fill", "Fill the plate")] {
                ui.selectable_value(&mut f.mode, m, label);
            }
        });
        ui.add_space(6.0);
        egui::Grid::new("copies_form").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            match f.mode {
                "free" | "row" => {
                    ui.label("Copies");
                    ui.add(egui::DragValue::new(&mut f.count).range(1..=200).speed(0.2));
                    ui.end_row();
                }
                "grid" => {
                    ui.label("Rows × columns");
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut f.rows).range(1..=20).speed(0.1));
                        ui.label("×");
                        ui.add(egui::DragValue::new(&mut f.cols).range(1..=20).speed(0.1));
                    });
                    ui.end_row();
                }
                _ => {}
            }
            if f.mode == "row" {
                ui.label("Direction");
                ui.horizontal(|ui| {
                    for (d, label) in [("+x", "Right"), ("-x", "Left"), ("+y", "Back"), ("-y", "Front")] {
                        ui.selectable_value(&mut f.direction, d, label);
                    }
                });
                ui.end_row();
            }
            ui.label("Gap");
            ui.add(egui::DragValue::new(&mut f.gap).range(0.0..=50.0).speed(0.1).suffix(" mm"));
            ui.end_row();
        });
        ui.add_space(4.0);
        ui.label(RichText::new(f.summary()).size(11.5).color(p.muted));
        ui.add_space(8.0);
        let (mut go, mut close) = (false, false);
        ui.horizontal(|ui| {
            go = ui.add(theme::primary_button("Add copies")).clicked();
            close = ui.button("Cancel").clicked();
        });
        if go || enter {
            self.worker.call("duplicate_object", f.args(), Kind::Plain);
            return true;
        }
        close
    }
}

/// Common materials for the spool form (any text works too).
pub const MATERIALS: [&str; 10] = ["PLA", "PETG", "ABS", "ASA", "TPU", "PA", "PC", "PLA-CF", "PETG-CF", "PVA"];

impl App {
    /// Where a spool can be: the AMS slots and the external holders, every place used before,
    /// and a new one. Returns true when the value changed.
    pub(crate) fn location_combo(&self, ui: &mut egui::Ui, id: &str, value: &mut String) -> bool {
        let mut known: Vec<String> = Vec::new();
        let units = self.printer_data.as_ref().and_then(|d| d.get("ams")).and_then(|a| a.as_array()).map(|a| a.len()).unwrap_or(1).max(1);
        for u in 0..units {
            for k in 1..=4 {
                known.push(format!("ams{u}:{k}"));
            }
        }
        known.extend(["ext:left".to_string(), "ext:right".to_string()]);
        for sp in self.spool_cache.as_ref().map(|c| c.1.clone()).unwrap_or_default() {
            if let Some(l) = sp.location.filter(|l| !l.is_empty() && !known.contains(l)) {
                known.push(l);
            }
        }
        let mut changed = false;
        let custom = !value.is_empty() && !known.contains(value);
        let label = |l: &str| -> String { format!("{} ({l})", only3d_studio::spools::location_name(l)).replace(" (ams", " (ams").replace("the ", "") };
        let shown = if value.is_empty() { "—".to_string() } else if custom { format!("{value} (new)") } else { label(value) };
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt(id).selected_text(shown).width(170.0).show_ui(ui, |ui| {
                if ui.selectable_label(value.is_empty(), "—").clicked() {
                    value.clear();
                    changed = true;
                }
                for l in &known {
                    let text = if l.contains(':') { label(l) } else { l.clone() };
                    if ui.selectable_label(value == l, text).clicked() {
                        *value = l.clone();
                        changed = true;
                    }
                }
                ui.separator();
                if ui.selectable_label(custom, "New location…").clicked() {
                    *value = "shelf".into();
                    changed = true;
                }
            });
            if custom && ui.add(egui::TextEdit::singleline(value).desired_width(90.0).hint_text("dry box")).changed() {
                changed = true;
            }
        });
        changed
    }
}
