//! The Settings panel: a searchable, grouped editor over every slicer setting, built from the
//! `settings_catalog` tool (or, on older backends, from `settings_schema`). Edits go through
//! `set_process {"overrides": …}` (or per object through `update_object {"settings": …}`),
//! coalesced while dragging; overridden values are marked and can be reset.

use serde_json::{json, Map, Value};

use eframe::egui::{self, RichText, Ui};

use crate::app::App;
use crate::palette::fuzzy_score;
use crate::theme::{self, pal, Icon};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SKind {
    Number,
    Integer,
    Bool,
    Enum,
    Text,
}

#[derive(Clone, Debug)]
pub struct SettingDef {
    pub key: String,
    pub label: String,
    pub group: String,
    pub kind: SKind,
    pub unit: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub options: Vec<String>,
    pub description: String,
    pub advanced: bool,
    /// Stored as a 0..1 fraction, shown as a percentage.
    pub percent: bool,
    /// Can be set for one object; otherwise only for the whole plate.
    pub per_object: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub groups: Vec<(String, String)>,
    pub settings: Vec<SettingDef>,
    pub values: Map<String, Value>,
    pub source: String,
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn humanize(key: &str) -> String {
    let t = key.replace('_', " ");
    let mut c = t.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

const PERCENT_KEYS: [&str; 5] = ["infill_density", "infill_wall_overlap", "fan_min", "fan_max", "aux_fan"];
const BASIC_KEYS: [&str; 20] = [
    "layer_height",
    "seam",
    "ironing",
    "wall_loops",
    "top_layers",
    "bottom_layers",
    "infill_density",
    "infill_pattern",
    "support",
    "support_type",
    "support_angle",
    "brim_width",
    "nozzle_temp",
    "bed_temp",
    "fan_min",
    "fan_max",
    "retract_length",
    "outer_wall_speed",
    "inner_wall_speed",
    "sparse_infill_speed",
];

fn fallback_group(key: &str) -> &'static str {
    if key.starts_with("support") || key.starts_with("tree_") {
        "support"
    } else if key.starts_with("brim") || key.starts_with("skirt") {
        "adhesion"
    } else if key.ends_with("_accel") {
        "acceleration"
    } else if key.ends_with("_speed") && !key.starts_with("min_print") && !key.contains("volumetric") && !key.contains("retract") {
        "speed"
    } else if key.contains("temp") {
        "temperature"
    } else if key.starts_with("fan") || key == "aux_fan" || key.starts_with("min_layer_time") || key == "min_print_speed" {
        "cooling"
    } else if key.contains("retract") || key == "z_hop" || key.starts_with("wipe") {
        "retraction"
    } else if key.starts_with("infill") {
        "infill"
    } else if key.starts_with("wall") || key.ends_with("_layers") {
        "walls"
    } else if key.ends_with("_width") || key.contains("layer_height") || key == "seam" || key == "ironing" {
        "quality"
    } else {
        "filament"
    }
}

impl Catalog {
    /// From `settings_catalog` data.
    pub fn from_tool(data: &Value) -> Catalog {
        let groups = data.get("groups").and_then(|g| g.as_array()).map(|a| a.iter().map(|g| (s(g, "id"), s(g, "label"))).collect()).unwrap_or_default();
        let settings = data
            .get("settings")
            .and_then(|g| g.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|d| {
                        let key = s(d, "key");
                        if key.is_empty() {
                            return None;
                        }
                        let kind = match s(d, "kind").as_str() {
                            "integer" => SKind::Integer,
                            "bool" | "boolean" => SKind::Bool,
                            "enum" => SKind::Enum,
                            "number" => SKind::Number,
                            _ => SKind::Text,
                        };
                        Some(SettingDef {
                            label: Some(s(d, "label")).filter(|l| !l.is_empty()).unwrap_or_else(|| humanize(&key)),
                            group: Some(s(d, "group")).filter(|l| !l.is_empty()).unwrap_or_else(|| fallback_group(&key).to_string()),
                            kind,
                            unit: s(d, "unit"),
                            min: d.get("min").and_then(|x| x.as_f64()),
                            max: d.get("max").and_then(|x| x.as_f64()),
                            step: d.get("step").and_then(|x| x.as_f64()),
                            options: d.get("options").and_then(|o| o.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
                            description: s(d, "description"),
                            advanced: d.get("advanced").and_then(|x| x.as_bool()).unwrap_or(false),
                            percent: d.get("percent").and_then(|x| x.as_bool()).unwrap_or(false),
                            // older backends don't say: offer everything, as before
                            per_object: d.get("per_object").and_then(|x| x.as_bool()).unwrap_or(true),
                            key,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Catalog { groups, settings, values: data.get("values").and_then(|v| v.as_object()).cloned().unwrap_or_default(), source: s(data, "source") }
    }

    /// From `settings_schema` data (JSON Schema + effective values).
    pub fn from_schema(data: &Value, _overrides: &Value) -> Catalog {
        let schema = data.get("schema").cloned().unwrap_or(Value::Null);
        let defs = schema.get("$defs").cloned().unwrap_or(Value::Null);
        let props = schema.get("properties").and_then(|p| p.as_object()).cloned().unwrap_or_default();
        let mut settings = Vec::new();
        for (key, p) in props {
            let ty = p.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let mut options = Vec::new();
            if let Some(name) = p.get("$ref").and_then(|r| r.as_str()).and_then(|r| r.rsplit('/').next()) {
                if let Some(arr) = defs.get(name).and_then(|d| d.get("oneOf")).and_then(|o| o.as_array()) {
                    for o in arr {
                        if let Some(c) = o.get("const").and_then(|c| c.as_str()) {
                            options.push(c.to_string());
                        } else if let Some(e) = o.get("enum").and_then(|e| e.as_array()) {
                            options.extend(e.iter().filter_map(|x| x.as_str().map(String::from)));
                        }
                    }
                }
            }
            let kind = if !options.is_empty() {
                SKind::Enum
            } else {
                match ty {
                    "integer" => SKind::Integer,
                    "number" => SKind::Number,
                    "boolean" => SKind::Bool,
                    _ => SKind::Text,
                }
            };
            let percent = PERCENT_KEYS.contains(&key.as_str());
            settings.push(SettingDef {
                label: humanize(&key),
                group: fallback_group(&key).to_string(),
                kind,
                unit: if percent { "%".into() } else { String::new() },
                min: p.get("minimum").and_then(|x| x.as_f64()).or(if percent { Some(0.0) } else { None }),
                max: if percent { Some(1.0) } else { None },
                step: None,
                options,
                description: s(&p, "description"),
                advanced: !BASIC_KEYS.contains(&key.as_str()),
                percent,
                per_object: true,
                key,
            });
        }
        let groups = [
            ("quality", "Quality"),
            ("walls", "Walls & shells"),
            ("infill", "Infill"),
            ("support", "Support"),
            ("adhesion", "Adhesion (brim & skirt)"),
            ("speed", "Speed"),
            ("acceleration", "Acceleration"),
            ("temperature", "Temperature"),
            ("cooling", "Cooling"),
            ("retraction", "Retraction & travel"),
            ("filament", "Filament & machine"),
        ]
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
        Catalog { groups, settings, values: data.get("effective").and_then(|v| v.as_object()).cloned().unwrap_or_default(), source: "settings_schema".into() }
    }
}

fn fmt_value(d: &SettingDef, v: &Value) -> String {
    match v {
        Value::Number(n) => {
            let f = n.as_f64().unwrap_or(0.0);
            if d.percent {
                format!("{:.0} %", f * 100.0)
            } else if d.kind == SKind::Integer {
                format!("{f:.0}")
            } else {
                let t = format!("{f:.3}");
                let t = t.trim_end_matches('0').trim_end_matches('.').to_string();
                if d.unit.is_empty() {
                    t
                } else {
                    format!("{t} {}", d.unit)
                }
            }
        }
        Value::Bool(b) => if *b { "on" } else { "off" }.into(),
        Value::String(s) => s.clone(),
        Value::Null => "—".into(),
        v => v.to_string(),
    }
}

impl App {
    pub fn settings_panel(&mut self, ui: &mut Ui) {
        let p = pal();
        let sel: Vec<String> = self.snap.selection.iter().filter(|id| self.snap.project.object(id).is_some()).cloned().collect();
        let obj_mode = self.settings_object && !sel.is_empty();

        // header: search + toggles (stays put while the list scrolls)
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 22.0), egui::Sense::hover());
            theme::paint_icon(ui.painter(), r.shrink2(egui::vec2(0.0, 3.0)), Icon::Search, p.muted);
            ui.add(egui::TextEdit::singleline(&mut self.settings_query).hint_text("Search settings…").desired_width(f32::INFINITY));
        });
        ui.horizontal_wrapped(|ui| {
            theme::checkbox(ui, &mut self.cfg.show_advanced, RichText::new("Advanced").size(12.0));
            let label = if sel.len() > 1 { format!("These {} objects only", sel.len()) } else { "This object only".to_string() };
            ui.add_enabled_ui(!sel.is_empty(), |ui| {
                theme::checkbox(ui, &mut self.settings_object, RichText::new(label).size(12.0)).on_hover_text("Write per-object overrides (update_object settings) instead of project-wide ones (set_process). Settings that only apply to the whole plate (layer height, temperatures, cooling, retraction) are hidden.");
            });
        });
        let overrides: Map<String, Value> = if obj_mode {
            self.snap.project.object(&sel[0]).map(|o| o.settings.clone().into_iter().collect()).unwrap_or_default()
        } else {
            self.snap.project.process.overrides.clone().into_iter().collect()
        };
        ui.horizontal(|ui| {
            let scope = if obj_mode { format!("object '{}'", sel[0]) } else { short(&self.snap.project.process.preset) };
            ui.label(RichText::new(scope).size(11.5).color(p.muted));
            if !overrides.is_empty() {
                theme::chip(ui, &format!("{} overridden", overrides.len()), p.accent, theme::soft(p.accent));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::bracket_button(ui, "reset all").on_hover_text("Remove every override (undoable)").clicked() {
                        if obj_mode {
                            let calls = sel.iter().map(|id| ("update_object".to_string(), json!({ "id": id, "settings": {} }))).collect();
                            self.run_calls(calls, crate::worker::Kind::Plain);
                        } else {
                            self.throttle.pending.remove("settings");
                            self.call("set_process", json!({ "clear": true }));
                        }
                        self.settings_local.clear();
                    }
                });
            }
        });
        ui.add_space(2.0);
        let Some(cat) = self.catalog.clone() else {
            ui.add_space(8.0);
            match &self.catalog_error {
                Some(e) => ui.label(RichText::new(e).color(p.error).size(12.0)),
                None => ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(12.0).color(p.accent));
                    ui.label(RichText::new("Loading settings…").color(p.muted))
                })
                .inner,
            };
            return;
        };
        let query = self.settings_query.trim().to_string();
        let busy = self.key_busy("settings") || sel.iter().any(|id| self.key_busy(&format!("objset:{id}")));
        if !busy {
            self.settings_local.clear();
        }
        egui::ScrollArea::vertical().id_salt("settings_scroll").auto_shrink([false, false]).show(ui, |ui| {
            let mut shown_any = false;
            for (gid, glabel) in &cat.groups {
                let rows: Vec<&SettingDef> = cat
                    .settings
                    .iter()
                    .filter(|d| &d.group == gid)
                    // one object takes only what the slicer applies per object (a stored plate-wide
                    // key still shows, so it can be reset)
                    .filter(|d| !obj_mode || d.per_object || overrides.contains_key(&d.key))
                    .filter(|d| {
                        if query.is_empty() {
                            self.cfg.show_advanced || !d.advanced || overrides.contains_key(&d.key)
                        } else {
                            fuzzy_score(&query, &d.label).is_some() || d.key.contains(&query.to_ascii_lowercase().replace(' ', "_")) || (query.len() > 3 && d.description.to_ascii_lowercase().contains(&query.to_ascii_lowercase()))
                        }
                    })
                    .collect();
                if rows.is_empty() {
                    continue;
                }
                shown_any = true;
                let n_over = rows.iter().filter(|d| overrides.contains_key(&d.key)).count();
                let sid = format!("settings.{gid}");
                let open = self.section_header(ui, &sid, glabel, true, !query.is_empty(), |ui| {
                    if n_over > 0 {
                        ui.label(RichText::new(format!("{n_over}")).size(10.5).monospace().color(p.accent)).on_hover_text("settings overridden in this group");
                    }
                });
                if !open {
                    continue;
                }
                for d in rows {
                    self.setting_row(ui, d, &cat, &overrides, &sel, obj_mode);
                }
                ui.add_space(4.0);
            }
            if !shown_any {
                ui.add_space(8.0);
                ui.label(RichText::new(if query.is_empty() { "No settings." } else { "No setting matches — try “Advanced”." }).color(p.muted));
            }
            ui.add_space(8.0);
            if !cat.source.is_empty() {
                ui.label(RichText::new(format!("Effective values from {}", cat.source)).size(10.5).color(p.placeholder)).on_hover_text(&cat.source);
            }
        });
    }

    fn setting_row(&mut self, ui: &mut Ui, d: &SettingDef, cat: &Catalog, overrides: &Map<String, Value>, sel: &[String], obj_mode: bool) {
        let p = pal();
        let overridden = overrides.contains_key(&d.key);
        let effective = overrides.get(&d.key).or_else(|| cat.values.get(&d.key)).cloned().unwrap_or(Value::Null);
        let mut v = self.settings_local.get(&d.key).cloned().unwrap_or_else(|| effective.clone());
        let row_h = 24.0;
        let (row, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::hover());
        let hovered = ui.rect_contains_pointer(row);
        if hovered {
            ui.painter().rect_filled(row, 3.0, p.hover);
        }
        if overridden {
            ui.painter().rect_filled(egui::Rect::from_min_size(row.min + egui::vec2(0.0, 4.0), egui::vec2(2.0, row_h - 8.0)), 1.0, p.accent);
        }
        let mut changed = false;
        let mut active = false;
        let mut reset = false;
        let mut row_ui = ui.new_child(egui::UiBuilder::new().max_rect(row.shrink2(egui::vec2(8.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        {
            let ui = &mut row_ui;
            let label_w = (ui.available_width() * 0.52).max(90.0);
            let tip = if d.description.is_empty() { d.key.clone() } else { format!("{}\n\n{}", d.description, d.key) };
            let col = if overridden { p.text } else { p.muted };
            ui.allocate_ui_with_layout(egui::vec2(label_w, row_h), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_min_size(egui::vec2(label_w, row_h));
                ui.add(egui::Label::new(RichText::new(&d.label).size(12.5).color(col)).truncate()).on_hover_text(tip);
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if overridden {
                    let base = if obj_mode { "the project value" } else { "the preset value" };
                    if theme::icon_button(ui, Icon::Reset, &format!("Reset to {base}")).clicked() {
                        reset = true;
                    }
                } else {
                    ui.add_space(26.0);
                }
                match d.kind {
                    SKind::Bool => {
                        let mut b = v.as_bool().unwrap_or(false);
                        if theme::checkbox(ui, &mut b, "").changed() {
                            v = json!(b);
                            changed = true;
                        }
                    }
                    SKind::Enum => {
                        let cur = v.as_str().unwrap_or("").to_string();
                        egui::ComboBox::from_id_salt(("set_enum", &d.key)).selected_text(RichText::new(&cur).size(12.0)).width(110.0).show_ui(ui, |ui| {
                            for o in &d.options {
                                if ui.selectable_label(*o == cur, o).clicked() && *o != cur {
                                    v = json!(o);
                                    changed = true;
                                }
                            }
                        });
                    }
                    SKind::Integer => {
                        let mut n = v.as_f64().unwrap_or(0.0).round() as i64;
                        let lo = d.min.unwrap_or(0.0) as i64;
                        let hi = d.max.map(|m| m as i64).unwrap_or(i64::MAX / 4);
                        let r = ui.add(egui::DragValue::new(&mut n).range(lo..=hi).speed(0.1).suffix(if d.unit.is_empty() { String::new() } else { format!(" {}", d.unit) }));
                        if r.changed() {
                            v = json!(n);
                            changed = true;
                        }
                        active |= r.dragged() || r.has_focus();
                    }
                    SKind::Number => {
                        let f = v.as_f64().unwrap_or(0.0);
                        let (scale, unit) = if d.percent { (100.0, "%".to_string()) } else { (1.0, d.unit.clone()) };
                        let mut shown = f * scale;
                        let lo = d.min.map(|m| m * scale).unwrap_or(f64::MIN);
                        let hi = d.max.map(|m| m * scale).unwrap_or(f64::MAX);
                        let step = d.step.map(|s| s * scale).unwrap_or_else(|| (shown.abs() * 0.01).max(0.01));
                        let decimals = if step >= 1.0 { 0 } else if step >= 0.1 { 1 } else if step >= 0.01 { 2 } else { 3 };
                        let r = ui.add(egui::DragValue::new(&mut shown).range(lo..=hi).speed(step * 0.5).max_decimals(decimals).min_decimals(decimals).suffix(if unit.is_empty() { String::new() } else { format!(" {unit}") }));
                        if r.changed() {
                            let raw = shown / scale;
                            let q = d.step.filter(|s| *s > 0.0).map(|s| (raw / s).round() * s).unwrap_or(raw);
                            v = json!((q * 1e6).round() / 1e6);
                            changed = true;
                        }
                        active |= r.dragged() || r.has_focus();
                    }
                    SKind::Text => {
                        let mut t = v.as_str().map(String::from).unwrap_or_else(|| v.to_string());
                        let r = ui.add(egui::TextEdit::singleline(&mut t).desired_width(110.0));
                        if r.lost_focus() && Some(t.as_str()) != v.as_str() {
                            v = json!(t);
                            changed = true;
                        }
                    }
                }
                if overridden && !obj_mode {
                    if let Some(base) = cat.values.get(&d.key).filter(|b| **b != effective) {
                        ui.label(RichText::new(fmt_value(d, base)).size(10.5).color(p.placeholder).strikethrough()).on_hover_text("value from the preset");
                    }
                }
            });
        }
        if active {
            self.active_now.insert("settings".into());
            for id in sel {
                self.active_now.insert(format!("objset:{id}"));
            }
        }
        if changed {
            self.settings_local.insert(d.key.clone(), v.clone());
            if obj_mode {
                for id in sel {
                    let mut m: Map<String, Value> = self.snap.project.object(id).map(|o| o.settings.clone().into_iter().collect()).unwrap_or_default();
                    for (k, lv) in &self.settings_local {
                        m.insert(k.clone(), lv.clone());
                    }
                    self.throttle.merge(&format!("objset:{id}"), "update_object", json!({ "id": id, "settings": Value::Object(m) }));
                }
            } else {
                self.throttle.merge("settings", "set_process", json!({ "overrides": { d.key.clone(): v } }));
            }
        }
        if reset {
            self.settings_local.remove(&d.key);
            if obj_mode {
                for id in sel {
                    self.throttle.pending.remove(&format!("objset:{id}"));
                    let mut m: Map<String, Value> = self.snap.project.object(id).map(|o| o.settings.clone().into_iter().collect()).unwrap_or_default();
                    m.remove(&d.key);
                    self.call("update_object", json!({ "id": id, "settings": Value::Object(m) }));
                }
            } else {
                self.throttle.merge("settings", "set_process", json!({ "overrides": { d.key.clone(): Value::Null } }));
            }
        }
    }
}

pub fn short(p: &str) -> String {
    p.split(" @").next().unwrap_or(p).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_from_tool_and_schema() {
        let data = json!({
            "groups": [{"id": "infill", "label": "Infill"}],
            "settings": [
                {"key": "infill_density", "label": "Sparse infill density", "group": "infill", "kind": "number", "unit": "%", "min": 0.0, "max": 1.0, "step": 0.01, "options": [], "description": "", "advanced": false, "percent": true},
                {"key": "infill_pattern", "kind": "enum", "options": ["grid", "concentric"]},
                {"kind": "number"}
            ],
            "values": {"infill_density": 0.15},
            "overrides": {},
            "source": "test"
        });
        let c = Catalog::from_tool(&data);
        assert_eq!(c.settings.len(), 2);
        assert!(c.settings[0].percent);
        assert_eq!(c.settings[1].kind, SKind::Enum);
        assert_eq!(c.settings[1].label, "Infill pattern");
        assert_eq!(c.settings[1].group, "infill");
        assert_eq!(fmt_value(&c.settings[0], &json!(0.15)), "15 %");

        let schema = json!({"schema": {"properties": {
            "wall_loops": {"type": "integer", "minimum": 0},
            "seam": {"$ref": "#/$defs/Seam"},
            "outer_wall_speed": {"type": "number"},
            "fan_max": {"type": "number"}
        }, "$defs": {"Seam": {"oneOf": [{"const": "rear"}, {"type": "string", "enum": ["nearest"]}]}}}, "effective": {"wall_loops": 2}});
        let c = Catalog::from_schema(&schema, &json!({}));
        let get = |k: &str| c.settings.iter().find(|d| d.key == k).unwrap().clone();
        assert_eq!(get("seam").options, vec!["rear", "nearest"]);
        assert_eq!(get("seam").group, "quality");
        assert_eq!(get("wall_loops").group, "walls");
        assert_eq!(get("outer_wall_speed").group, "speed");
        assert!(get("fan_max").percent);
        assert_eq!(get("fan_max").group, "cooling");
        assert!(!get("wall_loops").advanced);
        assert_eq!(c.values.get("wall_loops"), Some(&json!(2)));
    }
}
