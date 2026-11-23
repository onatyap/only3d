//! The run-tool form: a tool's arguments as fields built from its JSON Schema (text, numbers
//! with their unit, yes/no, choices, lists), with the same arguments as JSON one click away,
//! and the tool's answer as its text plus the data it returned as a tree.

use eframe::egui::{self, Align, Layout, RichText, Ui};
use serde_json::{Map, Value};

use crate::palette::{self, ToolInfo};
use crate::panels::muted;
use crate::theme::{self, pal};

#[derive(Clone, Debug, PartialEq)]
pub enum FieldKind {
    Text,
    /// A whole number when `int`.
    Number { int: bool },
    /// Yes / no, with the tool's default when the schema states one.
    Bool { default: Option<bool> },
    /// One of these values, each with what it means ("" when the schema doesn't say).
    Choice(Vec<(String, String)>),
    /// Comma-separated strings, or numbers.
    List { numbers: bool },
    /// Anything else, typed as JSON.
    Json,
}

/// One argument of the tool.
#[derive(Clone, Debug)]
pub struct Field {
    pub key: String,
    pub desc: String,
    pub required: bool,
    pub kind: FieldKind,
    /// What is typed (text, number, list, JSON) or picked (choice); "" = not given.
    pub text: String,
    /// Yes / no; `None` = not given, so the tool's default holds.
    pub flag: Option<bool>,
    /// Shown above "more options": required, or filled in when the form opened.
    pub main: bool,
}

/// What a tool answered.
pub struct Answer {
    pub text: String,
    pub data: Value,
}

pub struct ToolForm {
    pub tool: ToolInfo,
    pub fields: Vec<Field>,
    /// Arguments the schema doesn't list (kept as they came).
    extra: Map<String, Value>,
    /// The arguments as JSON, while `as_json`.
    pub text: String,
    pub as_json: bool,
    pub result: Option<Result<Answer, String>>,
    pub running: bool,
    pub focus: bool,
}

impl ToolForm {
    /// The form for `tool`, with `args` filled in.
    pub fn new(tool: ToolInfo, args: &Value) -> Self {
        let (fields, extra) = fields_from(&tool.schema, args);
        ToolForm { tool, fields, extra, text: String::new(), as_json: false, result: None, running: false, focus: true }
    }

    /// The arguments to call the tool with, or what is wrong with them.
    pub fn args(&self) -> Result<Value, String> {
        if self.as_json {
            return serde_json::from_str(if self.text.trim().is_empty() { "{}" } else { &self.text }).map_err(|e| format!("invalid JSON: {e}"));
        }
        collect(&self.fields, &self.extra)
    }

    /// Switch between the fields and the same arguments as JSON; a value that can't be read
    /// keeps the current view and is reported like a failed run.
    pub fn toggle_json(&mut self) {
        match self.args() {
            Ok(args) if self.as_json => {
                (self.fields, self.extra) = fields_from(&self.tool.schema, &args);
                self.as_json = false;
            }
            Ok(args) => {
                self.text = serde_json::to_string_pretty(&args).unwrap_or_default();
                self.as_json = true;
            }
            Err(e) => self.result = Some(Err(e)),
        }
    }
}

/// A property with a local `$ref` followed and `Option<T>`'s `anyOf [T, null]` unwrapped.
fn resolve(p: &Value, root: &Value) -> Value {
    let not_null = |x: &&Value| x.get("type").and_then(Value::as_str) != Some("null");
    let p = p.get("anyOf").and_then(Value::as_array).and_then(|a| a.iter().find(not_null)).unwrap_or(p);
    let Some(name) = p.get("$ref").and_then(Value::as_str).map(|r| r.rsplit('/').next().unwrap_or("")) else { return p.clone() };
    root.get("$defs").or_else(|| root.get("definitions")).and_then(|d| d.get(name)).cloned().unwrap_or(Value::Null)
}

fn type_of(p: &Value) -> &str {
    match p.get("type") {
        Some(Value::String(s)) => s,
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).find(|s| *s != "null").unwrap_or(""),
        _ => "",
    }
}

fn kind_of(p: &Value) -> FieldKind {
    let strings = |v: &Value| v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>()).unwrap_or_default();
    if let Some(e) = p.get("enum") {
        return FieldKind::Choice(strings(e).into_iter().map(|v| (v, String::new())).collect());
    }
    // an enum whose variants carry their own description
    if let Some(alts) = p.get("oneOf").and_then(Value::as_array) {
        let mut out = Vec::new();
        for a in alts {
            let desc = a.get("description").and_then(Value::as_str).unwrap_or("").to_string();
            match (a.get("const").and_then(Value::as_str), a.get("enum")) {
                (Some(c), _) => out.push((c.to_string(), desc)),
                (None, Some(e)) => out.extend(strings(e).into_iter().map(|v| (v, desc.clone()))),
                _ => return FieldKind::Json,
            }
        }
        return FieldKind::Choice(out);
    }
    match type_of(p) {
        "string" => FieldKind::Text,
        "integer" => FieldKind::Number { int: true },
        "number" => FieldKind::Number { int: false },
        "boolean" => FieldKind::Bool { default: p.get("default").and_then(Value::as_bool) },
        "array" => match p.get("items").map(type_of) {
            Some("string") => FieldKind::List { numbers: false },
            Some("integer" | "number") => FieldKind::List { numbers: true },
            _ => FieldKind::Json,
        },
        _ => FieldKind::Json,
    }
}

/// The schema's properties as fields (required first, then the ones `args` fills in, then the
/// rest), and the arguments the schema doesn't list.
pub fn fields_from(schema: &Value, args: &Value) -> (Vec<Field>, Map<String, Value>) {
    let props = schema.get("properties").and_then(Value::as_object).cloned().unwrap_or_default();
    let required: Vec<&str> = schema.get("required").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let mut fields = Vec::new();
    for (key, raw) in &props {
        let p = resolve(raw, schema);
        let kind = kind_of(&p);
        let required = required.contains(&key.as_str());
        let given = args.get(key).filter(|v| !v.is_null());
        let mut f = Field {
            key: key.clone(),
            desc: raw.get("description").or_else(|| p.get("description")).and_then(Value::as_str).unwrap_or("").to_string(),
            required,
            text: String::new(),
            flag: None,
            main: required || given.is_some(),
            kind,
        };
        match (&f.kind, given) {
            (FieldKind::Bool { .. }, v) => f.flag = v.and_then(Value::as_bool).or(required.then_some(false)),
            (FieldKind::Choice(c), v) => f.text = v.and_then(Value::as_str).map(String::from).or_else(|| c.first().filter(|_| required).map(|c| c.0.clone())).unwrap_or_default(),
            (_, None) => {}
            (FieldKind::Text, Some(v)) => f.text = v.as_str().map(String::from).unwrap_or_else(|| v.to_string()),
            (FieldKind::List { .. }, Some(Value::Array(a))) => f.text = a.iter().map(|x| x.as_str().map(String::from).unwrap_or_else(|| x.to_string())).collect::<Vec<_>>().join(", "),
            (_, Some(v)) => f.text = v.to_string(),
        }
        fields.push(f);
    }
    // a stable sort: the schema's order holds within each group
    fields.sort_by_key(|f| (!f.required, !f.main));
    let extra = args.as_object().map(|a| a.iter().filter(|(k, _)| !props.contains_key(*k)).map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
    (fields, extra)
}

fn number(key: &str, t: &str, int: bool) -> Result<Value, String> {
    let t = t.trim().replace(',', ".");
    if int {
        t.parse::<i64>().map(Value::from).map_err(|_| format!("{}: '{t}' is not a whole number", label_unit(key).0))
    } else {
        t.parse::<f64>().ok().filter(|x| x.is_finite()).map(Value::from).ok_or_else(|| format!("{}: '{t}' is not a number", label_unit(key).0))
    }
}

/// The call's arguments from the fields: what isn't given is left out, so the tool's own
/// defaults hold.
pub fn collect(fields: &[Field], extra: &Map<String, Value>) -> Result<Value, String> {
    let mut out = extra.clone();
    for f in fields {
        let t = f.text.trim();
        let missing = || format!("{} is needed", label_unit(&f.key).0);
        let v = match &f.kind {
            FieldKind::Bool { .. } => f.flag.map(Value::from),
            FieldKind::Text | FieldKind::Choice(_) if t.is_empty() => f.required.then(|| Value::from("")),
            FieldKind::Text | FieldKind::Choice(_) => Some(Value::from(f.text.as_str())),
            FieldKind::List { numbers } => {
                let items: Vec<&str> = t.split(',').map(str::trim).filter(|x| !x.is_empty()).collect();
                match (items.is_empty(), numbers) {
                    (true, _) => f.required.then(|| Value::Array(Vec::new())),
                    (false, false) => Some(Value::from(items)),
                    (false, true) => Some(Value::Array(items.iter().map(|x| number(&f.key, x, !x.contains(['.', 'e', 'E']))).collect::<Result<_, _>>()?)),
                }
            }
            _ if t.is_empty() && f.required => return Err(missing()),
            _ if t.is_empty() => None,
            FieldKind::Number { int } => Some(number(&f.key, t, *int)?),
            FieldKind::Json => Some(serde_json::from_str(t).map_err(|e| format!("{}: invalid JSON ({e})", label_unit(&f.key).0))?),
        };
        if let Some(v) = v {
            out.insert(f.key.clone(), v);
        }
    }
    Ok(Value::Object(out))
}

/// A field's label and unit from its key: "gross_g" is "Gross" in g, "bed_temp" "Bed temp" in °C.
pub fn label_unit(key: &str) -> (String, &'static str) {
    let (stem, unit) = match key {
        k if k.ends_with("_per_kg") => (k, ""),
        k if k.ends_with("_g") => (&k[..k.len() - 2], "g"),
        k if k.ends_with("_mm") => (&k[..k.len() - 3], "mm"),
        k if k.ends_with("_temp") => (k, "°C"),
        k if k.ends_with("_hours") => (k, "h"),
        k => (k, ""),
    };
    let words: Vec<String> = stem
        .split('_')
        .enumerate()
        .map(|(i, w)| match w {
            "ams" | "id" | "rfid" | "url" | "ip" => w.to_uppercase(),
            _ if i == 0 => w[..1.min(w.len())].to_uppercase() + &w[1.min(w.len())..],
            _ => w.to_string(),
        })
        .collect();
    (words.join(" "), unit)
}

/// Typed out of sight: access codes and API keys.
fn secret(key: &str) -> bool {
    ["access_code", "api_key", "password", "token"].iter().any(|s| key.contains(s))
}

const LABEL_W: f32 = 132.0;

fn field_ui(ui: &mut Ui, f: &mut Field, focus: &mut bool) {
    let p = pal();
    let (label, unit) = label_unit(&f.key);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(LABEL_W, 22.0), Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_width(LABEL_W);
            ui.add(egui::Label::new(RichText::new(label).size(12.5).color(if f.required { p.text } else { p.muted })).truncate()).on_hover_text(format!("{}{}", f.key, if f.required { " · required" } else { "" }));
        });
        ui.vertical(|ui| {
            let mut first = |r: egui::Response| {
                if *focus {
                    r.request_focus();
                    *focus = false;
                }
            };
            let hint = if f.required { "" } else { "not set" };
            match &f.kind {
                FieldKind::Text => first(ui.add(egui::TextEdit::singleline(&mut f.text).password(secret(&f.key)).hint_text(hint).desired_width(f32::INFINITY))),
                FieldKind::Number { .. } => {
                    ui.horizontal(|ui| {
                        first(ui.add(egui::TextEdit::singleline(&mut f.text).hint_text(hint).desired_width(96.0)));
                        ui.label(muted(unit).size(12.0));
                    });
                }
                FieldKind::Bool { default } => {
                    ui.horizontal(|ui| match (f.required, default) {
                        // one box when leaving it alone has a known meaning
                        (true, _) | (_, Some(_)) => {
                            let mut on = f.flag.or(*default).unwrap_or(false);
                            if theme::checkbox(ui, &mut on, "").changed() {
                                f.flag = Some(on);
                            }
                        }
                        _ => {
                            for (v, text) in [(None, "default"), (Some(true), "yes"), (Some(false), "no")] {
                                if ui.selectable_label(f.flag == v, RichText::new(text).size(12.0)).clicked() {
                                    f.flag = v;
                                }
                            }
                        }
                    });
                }
                FieldKind::Choice(choices) => {
                    egui::ComboBox::from_id_salt(("o3d_tool_choice", &f.key)).selected_text(if f.text.is_empty() { "not set" } else { f.text.as_str() }).width(220.0).show_ui(ui, |ui| {
                        if !f.required {
                            ui.selectable_value(&mut f.text, String::new(), "not set");
                        }
                        for (v, what) in choices {
                            let r = ui.selectable_value(&mut f.text, v.clone(), v.as_str());
                            if !what.is_empty() {
                                r.on_hover_text(what.as_str());
                            }
                        }
                    });
                    // what the picked value does
                    if let Some((_, what)) = choices.iter().find(|(v, what)| *v == f.text && !what.is_empty()) {
                        ui.add(egui::Label::new(RichText::new(what.as_str()).size(11.0).color(p.text)).wrap());
                    }
                }
                FieldKind::List { .. } => first(ui.add(egui::TextEdit::singleline(&mut f.text).hint_text("separated by commas").desired_width(f32::INFINITY))),
                FieldKind::Json => first(ui.add(egui::TextEdit::multiline(&mut f.text).code_editor().hint_text("JSON").desired_rows(2).desired_width(f32::INFINITY))),
            }
            if !f.desc.is_empty() {
                ui.add(egui::Label::new(RichText::new(&f.desc).size(11.0).color(p.placeholder)).wrap());
            }
        });
    });
}

/// The arguments: fields, or the JSON editor. `[ json ]` switches.
pub fn args_ui(ui: &mut Ui, f: &mut ToolForm) {
    let p = pal();
    ui.horizontal(|ui| {
        theme::section_label(ui, if f.as_json { "Arguments (JSON)" } else { "Arguments" });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (to, tip) = if f.as_json { ("fields", "Back to the fields") } else { ("json", "The same arguments as JSON, as agents send them") };
            if theme::bracket_button(ui, to).on_hover_text(tip).clicked() {
                f.toggle_json();
                f.focus = true;
            }
        });
    });
    if f.as_json {
        let r = ui.add(egui::TextEdit::multiline(&mut f.text).code_editor().desired_rows(6).desired_width(f32::INFINITY));
        if f.focus {
            r.request_focus();
            f.focus = false;
        }
        let opt = palette::optional_args(&f.tool.schema);
        if !opt.is_empty() {
            egui::CollapsingHeader::new(RichText::new(format!("{} optional argument{}", opt.len(), if opt.len() == 1 { "" } else { "s" })).size(11.5).color(p.muted)).id_salt("tool_opt").show(ui, |ui| {
                for o in opt {
                    ui.label(RichText::new(o).size(11.0).monospace().color(p.muted));
                }
            });
        }
        return;
    }
    let max_h = (ui.ctx().content_rect().height() * 0.42).clamp(160.0, 420.0);
    egui::ScrollArea::vertical().id_salt("o3d_tool_fields").max_height(max_h).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        let mut focus = f.focus;
        for x in f.fields.iter_mut().filter(|x| x.main) {
            field_ui(ui, x, &mut focus);
        }
        let more = f.fields.iter().filter(|x| !x.main).count();
        if more > 0 {
            let all = more == f.fields.len();
            let title = if all { format!("{more} option{}", if more == 1 { "" } else { "s" }) } else { format!("{more} more option{}", if more == 1 { "" } else { "s" }) };
            egui::CollapsingHeader::new(RichText::new(title).size(11.5).color(p.muted)).id_salt(("o3d_tool_more", &f.tool.name)).default_open(all).show(ui, |ui| {
                for x in f.fields.iter_mut().filter(|x| !x.main) {
                    field_ui(ui, x, &mut focus);
                }
            });
        }
        f.focus = false;
    });
}

/// The answer: the tool's text, then the data it returned as a tree; an error with its hint.
pub fn result_ui(ui: &mut Ui, r: &Result<Answer, String>) {
    let p = pal();
    ui.add_space(4.0);
    egui::Frame::new().fill(if r.is_err() { theme::soft(p.error) } else { p.bg }).stroke(egui::Stroke::new(1.0, p.border_variant)).corner_radius(4).inner_margin(8).show(ui, |ui| {
        egui::ScrollArea::vertical().id_salt("o3d_tool_result").max_height(300.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            match r {
                Err(e) => {
                    let (what, hint) = e.split_once("\n→ ").unwrap_or((e.as_str(), ""));
                    ui.add(egui::Label::new(RichText::new(what).size(12.0).color(p.error)).wrap().selectable(true));
                    if !hint.is_empty() {
                        ui.add(egui::Label::new(RichText::new(format!("→ {hint}")).size(11.5).color(p.text)).wrap().selectable(true));
                    }
                }
                Ok(a) => {
                    ui.add(egui::Label::new(RichText::new(&a.text).monospace().size(11.5).color(p.text)).wrap().selectable(true));
                    if !a.data.is_null() {
                        ui.add_space(4.0);
                        ui.separator();
                        theme::section_label(ui, "Data");
                        match &a.data {
                            Value::Object(m) => m.iter().for_each(|(k, v)| json_tree(ui, egui::Id::unique("o3d_tool_data"), k, v, 0)),
                            v => json_tree(ui, egui::Id::unique("o3d_tool_data"), "result", v, 0),
                        }
                    }
                }
            }
        });
    });
    if let Ok(a) = r {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if theme::bracket_button(ui, "copy").on_hover_text("Copy the answer's text").clicked() {
                ui.ctx().copy_text(a.text.clone());
            }
            if !a.data.is_null() && theme::bracket_button(ui, "copy data").on_hover_text("Copy the returned data as JSON").clicked() {
                ui.ctx().copy_text(serde_json::to_string_pretty(&a.data).unwrap_or_default());
            }
        });
    }
}

/// Children shown per list or object before "… n more".
const TREE_MAX: usize = 200;

/// A JSON value as a tree: objects and lists fold, the rest is a `key  value` line.
pub fn json_tree(ui: &mut Ui, parent: egui::Id, key: &str, v: &Value, depth: usize) {
    let p = pal();
    let mono = |t: String, c| RichText::new(t).monospace().size(11.0).color(c);
    // a short list of plain values reads better on its line
    let inline = |a: &[Value]| a.len() <= 8 && a.iter().all(|x| !x.is_object() && !x.is_array());
    let children: Vec<(String, &Value)> = match v {
        Value::Object(m) => m.iter().map(|(k, x)| (k.clone(), x)).collect(),
        Value::Array(a) if !inline(a) => a.iter().enumerate().map(|(i, x)| (i.to_string(), x)).collect(),
        _ => Vec::new(),
    };
    if children.is_empty() {
        let (text, col) = match v {
            Value::String(s) => (s.clone(), p.text),
            Value::Null => ("—".to_string(), p.placeholder),
            Value::Bool(_) | Value::Number(_) => (v.to_string(), p.accent),
            other => (other.to_string(), p.text),
        };
        ui.horizontal_top(|ui| {
            ui.label(mono(key.to_string(), p.muted));
            ui.add(egui::Label::new(mono(text, col)).wrap().selectable(true));
        });
        return;
    }
    let id = parent.with(key);
    let count = if v.is_array() { format!("[{}]", children.len()) } else { format!("{{{}}}", children.len()) };
    let mut title = egui::text::LayoutJob::default();
    title.append(key, 0.0, egui::TextFormat::simple(egui::FontId::monospace(11.0), p.text));
    title.append(&count, 6.0, egui::TextFormat::simple(egui::FontId::monospace(10.5), p.placeholder));
    egui::CollapsingHeader::new(title).id_salt(id).default_open(depth == 0 && children.len() <= 12).show(ui, |ui| {
        for (k, x) in children.iter().take(TREE_MAX) {
            json_tree(ui, id, k, x, depth + 1);
        }
        if children.len() > TREE_MAX {
            ui.label(mono(format!("… {} more (copy data has them all)", children.len() - TREE_MAX), p.placeholder));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "$defs": {
                "Speed": {"type": "string", "enum": ["silent", "standard"]},
                "AmsAction": {"oneOf": [{"type": "string", "enum": ["dry", "stop_drying"]}, {"description": "Load a slot.", "type": "string", "const": "load"}]}
            },
            "type": "object",
            "required": ["action", "ids", "confirm"],
            "properties": {
                "name": {"type": "string"},
                "action": {"$ref": "#/$defs/AmsAction"},
                "speed": {"description": "Speed profile.", "$ref": "#/$defs/Speed"},
                "bed_temp": {"type": "integer", "minimum": 0},
                "gross_g": {"type": ["number", "null"]},
                "ids": {"type": "array", "items": {"type": "integer"}},
                "tags": {"type": "array", "items": {"type": "string"}},
                "confirm": {"type": "boolean"},
                "use_ams": {"type": ["boolean", "null"]},
                "rotate": {"type": "boolean", "default": false},
                "calibration": {"anyOf": [{"type": "object"}, {"type": "null"}]}
            }
        })
    }

    fn field<'a>(f: &'a mut [Field], key: &str) -> &'a mut Field {
        f.iter_mut().find(|x| x.key == key).unwrap()
    }

    #[test]
    fn fields_follow_the_schema() {
        let (mut f, extra) = fields_from(&schema(), &json!({"bed_temp": 60, "note": "kept"}));
        // required first, then what was filled in, then the rest
        assert_eq!(f.iter().map(|x| x.key.as_str()).take(4).collect::<Vec<_>>(), ["action", "ids", "confirm", "bed_temp"]);
        assert_eq!(extra.get("note"), Some(&json!("kept")));
        assert_eq!(field(&mut f, "action").kind, FieldKind::Choice(vec![("dry".into(), "".into()), ("stop_drying".into(), "".into()), ("load".into(), "Load a slot.".into())]));
        assert_eq!(field(&mut f, "action").text, "dry", "a required choice starts on its first value");
        assert_eq!(field(&mut f, "speed").kind, FieldKind::Choice(vec![("silent".into(), "".into()), ("standard".into(), "".into())]));
        assert_eq!(field(&mut f, "speed").desc, "Speed profile.");
        assert_eq!(field(&mut f, "gross_g").kind, FieldKind::Number { int: false });
        assert_eq!(field(&mut f, "ids").kind, FieldKind::List { numbers: true });
        assert_eq!(field(&mut f, "rotate").kind, FieldKind::Bool { default: Some(false) });
        assert_eq!(field(&mut f, "calibration").kind, FieldKind::Json);
        assert!(field(&mut f, "bed_temp").main && !field(&mut f, "name").main);
    }

    #[test]
    fn collects_only_what_is_given() {
        let (mut f, extra) = fields_from(&schema(), &json!({"bed_temp": 60, "note": "kept"}));
        assert_eq!(collect(&f, &extra).unwrap(), json!({"action": "dry", "ids": [], "confirm": false, "bed_temp": 60, "note": "kept"}));
        field(&mut f, "ids").text = "3, 5,".into();
        field(&mut f, "tags").text = "a, b".into();
        field(&mut f, "gross_g").text = "412,5".into();
        field(&mut f, "use_ams").flag = Some(false);
        field(&mut f, "calibration").text = r#"{"flow_ratio": 0.98}"#.into();
        field(&mut f, "bed_temp").text.clear();
        assert_eq!(
            collect(&f, &extra).unwrap(),
            json!({"action": "dry", "ids": [3, 5], "confirm": false, "note": "kept", "tags": ["a", "b"], "gross_g": 412.5, "use_ams": false, "calibration": {"flow_ratio": 0.98}})
        );
        field(&mut f, "bed_temp").text = "hot".into();
        assert_eq!(collect(&f, &extra).unwrap_err(), "Bed temp: 'hot' is not a whole number");
    }

    #[test]
    fn json_and_fields_round_trip() {
        let tool = ToolInfo { name: "t".into(), title: "T".into(), description: String::new(), schema: schema(), mutates: false, side_effects: false };
        let mut form = ToolForm::new(tool, &json!({"action": "load", "ids": [1], "confirm": true, "name": "h2d"}));
        let before = form.args().unwrap();
        form.toggle_json();
        assert!(form.as_json);
        assert_eq!(form.args().unwrap(), before);
        form.text = r#"{"action": "load", "ids": [2], "confirm": true}"#.into();
        form.toggle_json();
        assert!(!form.as_json);
        assert_eq!(form.args().unwrap(), json!({"action": "load", "ids": [2], "confirm": true}));
        form.as_json = true;
        form.text = "{".into();
        form.toggle_json();
        assert!(form.as_json && matches!(form.result, Some(Err(_))), "bad JSON keeps the editor up");
    }

    #[test]
    fn labels_carry_units() {
        assert_eq!(label_unit("gross_g"), ("Gross".into(), "g"));
        assert_eq!(label_unit("bed_temp"), ("Bed temp".into(), "°C"));
        assert_eq!(label_unit("ams_id"), ("AMS ID".into(), ""));
        assert_eq!(label_unit("cost_per_kg"), ("Cost per kg".into(), ""));
        assert_eq!(label_unit("dry_hours"), ("Dry hours".into(), "h"));
    }
}
