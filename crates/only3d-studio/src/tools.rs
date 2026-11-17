//! The tool registry: only3d's single API surface. Each tool has a name, a description
//! written for both people and models, a JSON Schema for its input, and a handler. MCP, the
//! CLI, the live socket and the GUI all go through here.

use std::collections::BTreeMap;
use std::sync::Arc;

use only3d_model::{FilamentSlot, Node, NozzleSel, Object, Severity};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::render::RenderRequest;
use crate::{describe, render, SliceOutput, Studio};

/// Who called a tool (shown in the journal and the GUI's activity feed).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    User,
    Agent,
    Script,
}

#[derive(Clone, Debug, Serialize)]
pub struct ImageOut {
    /// Image bytes (PNG unless `mime` says otherwise).
    #[serde(skip)]
    pub png: Vec<u8>,
    pub caption: String,
    /// "image/png" or "image/jpeg".
    pub mime: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ToolOutput {
    /// Short human/LLM-readable summary.
    pub text: String,
    /// Structured result.
    pub data: Value,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageOut>,
}

impl ToolOutput {
    pub fn text(t: impl Into<String>) -> Self {
        ToolOutput { text: t.into(), data: Value::Null, images: Vec::new() }
    }
    pub fn with_data(mut self, d: Value) -> Self {
        self.data = d;
        self
    }
    pub fn with_image(mut self, png: Vec<u8>, caption: impl Into<String>) -> Self {
        self.images.push(ImageOut { png, caption: caption.into(), mime: "image/png".into() });
        self
    }
    pub fn with_jpeg(mut self, jpg: Vec<u8>, caption: impl Into<String>) -> Self {
        self.images.push(ImageOut { png: jpg, caption: caption.into(), mime: "image/jpeg".into() });
        self
    }
}

/// A failure an agent can act on: stable `code`, what happened, and what to do next.
#[derive(Clone, Debug, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message} (hint: {hint})")]
pub struct ToolError {
    pub code: String,
    pub message: String,
    pub hint: String,
}

impl ToolError {
    pub fn new(code: impl Into<String>, message: impl Into<String>, hint: impl Into<String>) -> Self {
        ToolError { code: code.into(), message: message.into(), hint: hint.into() }
    }
}

impl From<only3d_model::ModelError> for ToolError {
    fn from(e: only3d_model::ModelError) -> Self {
        let hint = match e.code() {
            "bad_expression" => "check parameter names with `describe`; expressions support + - * / ^ min max sqrt sin cos",
            "boolean_failed" => "booleans need closed solids; avoid exactly coincident faces (overlap cutters by 0.01 mm or more)",
            "import_failed" => "check the path (relative to the project file) and the file format (stl, obj, 3mf, scad)",
            _ => "check the shape against the `add_object` schema",
        };
        ToolError::new(e.code(), e.to_string(), hint)
    }
}

impl From<only3d_slice::SliceError> for ToolError {
    fn from(e: only3d_slice::SliceError) -> Self {
        ToolError::new(e.code(), e.to_string(), "check `check` findings and slice settings")
    }
}

pub type Handler = Box<dyn Fn(&mut Studio, Value) -> Result<ToolOutput, ToolError> + Send + Sync>;

pub struct ToolDef {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// Changes the project (gets an undo step).
    pub mutates: bool,
    /// Has effects outside the document (files, printer).
    pub side_effects: bool,
    pub schema: Value,
    pub handler: Handler,
}

pub struct Registry {
    pub tools: Vec<ToolDef>,
}

impl Registry {
    pub fn get(&self, name: &str) -> Option<&ToolDef> {
        self.tools.iter().find(|t| t.name == name)
    }

    pub fn add(&mut self, t: ToolDef) {
        self.tools.retain(|x| x.name != t.name);
        self.tools.push(t);
    }

    /// JSON description of every tool (for `only3d tools` and MCP `tools/list`).
    pub fn manifest(&self) -> Value {
        Value::Array(
            self.tools
                .iter()
                .map(|t| json!({"name": t.name, "title": t.title, "description": t.description, "mutates": t.mutates, "side_effects": t.side_effects, "inputSchema": t.schema}))
                .collect(),
        )
    }

    pub fn builtin() -> Registry {
        let mut r = Registry { tools: Vec::new() };
        register_builtin(&mut r);
        crate::printer::register(&mut r);
        crate::spools::register(&mut r);
        crate::presets::register(&mut r);
        crate::workspace::register(&mut r);
        crate::fit::register(&mut r);
        crate::combine::register(&mut r);
        // The shape grammar is spelled out once, in add_object; repeating it in every tool that
        // carries a shape tripled the manifest an agent has to read. Inputs are still validated
        // against the full types.
        r.loosen("update_object", "shape", "New shape tree (add_object grammar).");
        r.loosen_with("update_object", "parts", json!({"type": "array", "items": {"type": "object"}, "description": "Replace the extra bodies: [{\"name\", \"shape\" (add_object grammar), \"filament\"}]; [] removes them."}));
        r.loosen("set_project", "project", "The whole project document, as get_project returns it.");
        r
    }

    /// Replace one input property's schema with a plain object described in words.
    fn loosen(&mut self, tool: &str, prop: &str, description: &str) {
        self.loosen_with(tool, prop, json!({"type": "object", "description": description}));
    }

    fn loosen_with(&mut self, tool: &str, prop: &str, schema: Value) {
        if let Some(t) = self.tools.iter_mut().find(|t| t.name == tool) {
            if let Some(p) = t.schema.get_mut("properties").and_then(|p| p.get_mut(prop)) {
                *p = schema;
            }
            prune_defs(&mut t.schema);
        }
    }
}

pub fn schema_of<I: JsonSchema>() -> Value {
    let mut v = serde_json::to_value(schemars::schema_for!(I)).unwrap_or(json!({"type": "object"}));
    if let Some(o) = v.as_object_mut() {
        o.remove("$schema");
        o.remove("title");
        if !o.contains_key("type") {
            o.insert("type".into(), json!("object"));
        }
    }
    compact_schema(&mut v);
    hoist_fixed_arrays(&mut v);
    v
}

/// `[x, y, z]`-style fixed-length arrays of a `$defs` type repeat dozens of times in the shape
/// grammar; give each one a shared definition (`Num3` = three `Num`s) and reference it.
fn hoist_fixed_arrays(schema: &mut Value) {
    if schema.get("$defs").is_none() {
        return;
    }
    let mut new_defs: Vec<(String, Value)> = Vec::new();
    fn walk(v: &mut Value, new_defs: &mut Vec<(String, Value)>) {
        if let Value::Object(o) = v {
            let fixed = o.get("type").and_then(Value::as_str) == Some("array")
                && o.get("minItems").is_some()
                && o.get("minItems") == o.get("maxItems")
                && o.get("items").and_then(|i| i.get("$ref")).is_some();
            if fixed {
                let n = o["minItems"].as_u64().unwrap_or(0);
                let item = o["items"]["$ref"].as_str().unwrap_or("").trim_start_matches("#/$defs/").to_string();
                let name = format!("{item}{n}");
                if !new_defs.iter().any(|(k, _)| *k == name) {
                    new_defs.push((name.clone(), json!({"type": "array", "items": o["items"].clone(), "minItems": n, "maxItems": n})));
                }
                for k in ["type", "items", "minItems", "maxItems"] {
                    o.remove(k);
                }
                o.insert("$ref".into(), json!(format!("#/$defs/{name}")));
                return;
            }
            for (_, c) in o.iter_mut() {
                walk(c, new_defs);
            }
        } else if let Value::Array(a) = v {
            a.iter_mut().for_each(|c| walk(c, new_defs));
        }
    }
    walk(schema, &mut new_defs);
    if let Some(Value::Object(d)) = schema.get_mut("$defs") {
        for (k, v) in new_defs {
            d.entry(k).or_insert(v);
        }
    }
}

/// Make a generated schema cheaper for an agent to read without changing what it accepts in
/// practice: optional fields are already optional (not `required`), so `null` alternatives,
/// number `format`s and `default: null` add nothing. Saves roughly a third of the tokens.
pub fn compact_schema(v: &mut Value) {
    match v {
        Value::Object(o) => {
            o.remove("format");
            if o.get("default").is_some_and(Value::is_null) {
                o.remove("default");
            }
            // "type": ["X", "null"] -> "X"
            if let Some(Value::Array(ts)) = o.get("type") {
                let kept: Vec<Value> = ts.iter().filter(|t| t.as_str() != Some("null")).cloned().collect();
                if kept.len() == 1 {
                    o.insert("type".into(), kept[0].clone());
                }
            }
            // anyOf/oneOf [A, {type: null}] -> A (keeping this node's description)
            for key in ["anyOf", "oneOf"] {
                if let Some(Value::Array(alts)) = o.get(key) {
                    let kept: Vec<Value> = alts.iter().filter(|a| a.get("type").and_then(Value::as_str) != Some("null")).cloned().collect();
                    if kept.len() == 1 && alts.len() == 2 {
                        o.remove(key);
                        if let Value::Object(inner) = &kept[0] {
                            for (k, val) in inner {
                                o.entry(k.clone()).or_insert_with(|| val.clone());
                            }
                        }
                    } else if kept.len() != alts.len() {
                        o.insert(key.into(), Value::Array(kept));
                    }
                }
            }
            for (_, child) in o.iter_mut() {
                compact_schema(child);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(compact_schema),
        _ => {}
    }
}

/// Drop `$defs` entries nothing references any more.
fn prune_defs(schema: &mut Value) {
    fn refs(v: &Value, out: &mut std::collections::BTreeSet<String>) {
        match v {
            Value::Object(o) => {
                if let Some(r) = o.get("$ref").and_then(Value::as_str) {
                    if let Some(n) = r.strip_prefix("#/$defs/") {
                        out.insert(n.to_string());
                    }
                }
                o.iter().filter(|(k, _)| *k != "$defs").for_each(|(_, c)| refs(c, out));
            }
            Value::Array(a) => a.iter().for_each(|c| refs(c, out)),
            _ => {}
        }
    }
    let Some(defs) = schema.get("$defs").cloned() else { return };
    let mut used = std::collections::BTreeSet::new();
    refs(schema, &mut used);
    // follow references between definitions
    loop {
        let before = used.len();
        for n in used.clone() {
            if let Some(d) = defs.get(&n) {
                refs(d, &mut used);
            }
        }
        if used.len() == before {
            break;
        }
    }
    if let Some(Value::Object(d)) = schema.get_mut("$defs") {
        d.retain(|k, _| used.contains(k));
        if d.is_empty() {
            schema.as_object_mut().unwrap().remove("$defs");
        }
    }
}

pub fn def<I: DeserializeOwned + JsonSchema + 'static>(
    name: &'static str,
    title: &'static str,
    description: &'static str,
    mutates: bool,
    side_effects: bool,
    f: fn(&mut Studio, I) -> Result<ToolOutput, ToolError>,
) -> ToolDef {
    ToolDef {
        name,
        title,
        description,
        mutates,
        side_effects,
        schema: schema_of::<I>(),
        handler: Box::new(move |s, v| {
            let v = if v.is_null() { json!({}) } else { v };
            let input: I = serde_json::from_value(v).map_err(|e| ToolError::new("bad_input", format!("{name}: {e}"), format!("see the input schema of `{name}` (only3d tools)")))?;
            f(s, input)
        }),
    }
}

// ---------------------------------------------------------------------------------------------
// inputs

#[derive(Deserialize, JsonSchema, Default)]
pub struct Empty {}

#[derive(Deserialize, JsonSchema)]
pub struct DescribeIn {
    /// Also return an iso render of the plate.
    #[serde(default)]
    pub render: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetProjectIn {
    /// The complete project document (same shape `get_project` returns).
    pub project: only3d_model::Project,
}

#[derive(Deserialize, JsonSchema)]
pub struct NewProjectIn {
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PathIn {
    /// File path (absolute, `~/…`, or relative to the working directory).
    pub path: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct SaveIn {
    /// Where to save (`*.only3d.json`). Defaults to the file the project was opened from.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetParamsIn {
    /// Values; null removes one. Example: {"wall": 2.4}.
    pub params: BTreeMap<String, Value>,
    /// A .scad object: set its variables instead (null = file value).
    #[serde(default)]
    pub object: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct AddObjectIn {
    /// Unique id (letters, digits, _ and -). Generated from the shape type when omitted.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// Shape tree. Primitives are centred on X/Y and rest on Z=0; booleans: union / difference
    /// (first child minus the rest) / intersection / hull; patterns: array / polar. Numbers may be
    /// expressions over project params, e.g. "wall*2".
    pub shape: Node,
    /// Plate position [x, y] of the footprint centre (default: auto-arranged).
    #[serde(default)]
    pub position: Option<[f32; 2]>,
    /// Degrees about X, Y, Z.
    #[serde(default)]
    pub rotation: Option<[f32; 3]>,
    /// 1-based filament slot (default 1).
    #[serde(default)]
    pub filament: Option<u32>,
    /// Extra bodies in other filaments (e.g. a text inlay), same coordinates as `shape`:
    /// [{"shape": {...}, "filament": 2}].
    #[serde(default)]
    pub parts: Option<Vec<only3d_model::Part>>,
    /// Return an iso render of the new object.
    #[serde(default)]
    pub render: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct UpdateObjectIn {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    /// Replace the whole shape tree.
    #[serde(default)]
    pub shape: Option<Node>,
    #[serde(default)]
    pub position: Option<[f32; 2]>,
    #[serde(default)]
    pub rotation: Option<[f32; 3]>,
    /// Scale factors [x, y, z] (1 = unchanged).
    #[serde(default)]
    pub scale: Option<[f32; 3]>,
    #[serde(default)]
    pub filament: Option<u32>,
    /// Print it with this inventory spool (its slot, added if needed).
    #[serde(default)]
    pub spool: Option<String>,
    #[serde(default)]
    pub hidden: Option<bool>,
    /// Lock in place (no dragging; arrange skips it).
    #[serde(default)]
    pub locked: Option<bool>,
    /// Replace the object's extra bodies (each with its own filament); [] removes them.
    #[serde(default)]
    pub parts: Option<Vec<only3d_model::Part>>,
    /// Pull (+) or push (−) the flat face at a point on it.
    #[serde(default)]
    pub push_pull: Option<PushPullIn>,
    /// Mirror it left–right ("x") or front–back ("y").
    #[serde(default)]
    pub mirror: Option<String>,
    /// Per-object slicer overrides (replaces the map).
    #[serde(default)]
    pub settings: Option<BTreeMap<String, Value>>,
    #[serde(default)]
    pub render: bool,
}

#[derive(Deserialize, JsonSchema)]
#[schemars(inline)]
pub struct PushPullIn {
    /// A point on the face, plate mm.
    pub at: [f32; 3],
    /// mm out (+) or in (−).
    pub distance: f32,
}

#[derive(Deserialize, JsonSchema)]
pub struct IdsIn {
    pub ids: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct IdIn {
    pub id: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct DuplicateIn {
    pub id: String,
    /// default 1
    #[serde(default = "one")]
    pub copies: u32,
    /// A row: +x | -x | +y | -y
    #[serde(default)]
    pub direction: Option<String>,
    /// Or a grid (matrix) [rows, cols] from the original.
    #[serde(default)]
    pub grid: Option<[u32; 2]>,
    /// Or as many copies as fit in the free space.
    #[serde(default)]
    pub fill: bool,
    /// Or each copy [x, y] mm on from the last.
    #[serde(default)]
    pub offset: Option<[f32; 2]>,
    /// mm (default 0 in a row, 5 otherwise)
    #[serde(default)]
    pub gap: Option<f32>,
}

fn one() -> u32 {
    1
}

#[derive(Deserialize, JsonSchema)]
pub struct ImportIn {
    /// File path.
    pub path: String,
    #[serde(default)]
    pub id: Option<String>,
    /// For 3MF files with several objects: import only this one.
    #[serde(default)]
    pub object: Option<String>,
    #[serde(default)]
    pub position: Option<[f32; 2]>,
    /// Only this plate of a multi-plate file (default: all, each as a plate).
    #[serde(default)]
    pub plate: Option<u32>,
    /// Close holes, turn faces outward (the file isn't changed).
    #[serde(default)]
    pub repair: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct OrientIn {
    /// Objects to orient (default: all).
    #[serde(default)]
    pub ids: Option<Vec<String>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SectionIn {
    /// Object to cut (default: all visible objects).
    #[serde(default)]
    pub id: Option<String>,
    /// Height of the cut in mm above the plate.
    pub z: f64,
}

#[derive(Deserialize, JsonSchema)]
pub struct ClearanceIn {
    /// e.g. the pin
    pub a: String,
    /// e.g. the socket; measured as placed
    pub b: String,
    /// default: 9 across their shared heights
    #[serde(default)]
    pub z: Option<Vec<f64>>,
    /// Move A by [x, y, z] for the check only (e.g. lift a pin into its socket).
    #[serde(default)]
    pub a_shift: Option<[f32; 3]>,
    #[serde(default)]
    pub b_shift: Option<[f32; 3]>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetPrinterIn {
    /// Bambu machine preset, e.g. "Bambu Lab H2D 0.4 nozzle".
    #[serde(default)]
    pub machine: Option<String>,
    /// "Textured PEI Plate" | "Cool Plate" | "Engineering Plate" | "High Temp Plate" | "Smooth PEI Plate".
    #[serde(default)]
    pub bed_type: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetFilamentIn {
    /// 1-based slot; use (current count + 1) to add a slot.
    pub slot: u32,
    /// Bambu filament preset, e.g. "Bambu PETG Basic @BBL H2D 0.4 nozzle".
    #[serde(default)]
    pub preset: Option<String>,
    /// "#RRGGBB"
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub nozzle: Option<NozzleSel>,
    /// Inventory spool to use for this slot (sets preset and colour from the spool); "" clears.
    #[serde(default)]
    pub spool: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetProcessIn {
    /// Bambu process preset, e.g. "0.16mm Optimal @BBL H2D".
    #[serde(default)]
    pub preset: Option<String>,
    /// Merged into the current ones (null removes a key), e.g. {"infill_density": 0.25}.
    #[serde(default)]
    pub overrides: BTreeMap<String, Value>,
    /// Drop all existing overrides first.
    #[serde(default)]
    pub clear: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct ListPresetsIn {
    /// "machine" | "process" | "filament"
    pub kind: String,
    /// Case-insensitive substring filter, e.g. "H2D", "PETG".
    #[serde(default)]
    pub query: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SliceIn {
    /// Also return a toolpath preview image.
    #[serde(default)]
    pub preview: bool,
    /// Plate to slice (1-based); it becomes the active plate. Default: the active plate.
    #[serde(default)]
    pub plate: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ExportIn {
    /// Output file; its extension picks the format: .stl .obj .3mf .scad .png, .gcode .gcode.3mf (print), .zip (project + models).
    pub path: String,
    /// Only these objects (model formats).
    #[serde(default)]
    pub ids: Option<Vec<String>>,
    /// Every plate, not just the active one.
    #[serde(default)]
    pub all_plates: bool,
    /// "object" or "plate": one file each, beside `path`.
    #[serde(default)]
    pub each: Option<String>,
    /// Text STL.
    #[serde(default)]
    pub ascii: bool,
    /// .png width in px (default 1600).
    #[serde(default)]
    pub size: Option<u32>,
    /// .png view: iso, front, top…
    #[serde(default)]
    pub view: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct BambuExportIn {
    /// Output .3mf (default: next to the project, or in ~/.only3d/exports).
    #[serde(default)]
    pub path: Option<String>,
    /// Let Bambu Studio slice it too (single-filament plates only).
    #[serde(default)]
    pub slice: bool,
    /// Open the project in the Bambu Studio app afterwards, to slice and print it there.
    #[serde(default)]
    pub open: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct BatchCall {
    pub tool: String,
    #[serde(default)]
    pub args: Value,
}

#[derive(Deserialize, JsonSchema)]
pub struct BatchIn {
    /// Tool calls to run in order. All-or-nothing: if one fails, every change is rolled back.
    pub calls: Vec<BatchCall>,
}

#[derive(Deserialize, JsonSchema)]
pub struct HistoryIn {
    #[serde(default)]
    pub limit: Option<usize>,
}

// ---------------------------------------------------------------------------------------------
// helpers

pub(crate) fn obj_line(o: &only3d_model::EvaluatedObject) -> String {
    let r = &o.report;
    format!(
        "'{}': {:.1} x {:.1} x {:.1} mm at ({:.1}, {:.1}), {:.2} cm³, {}",
        o.id,
        r.size.x,
        r.size.y,
        r.size.z,
        (r.bounds.min.x + r.bounds.max.x) / 2.0,
        (r.bounds.min.y + r.bounds.max.y) / 2.0,
        r.volume_mm3 / 1000.0,
        if r.watertight { "watertight" } else { "NOT watertight" }
    )
}

/// Working on an object brings its plate up, so tools that name an object on another plate act
/// on it there (and the window shows it).
fn focus_object(s: &mut Studio, id: &str) {
    if let Some(p) = s.project.object(id).map(|o| o.plate) {
        s.set_plate(p);
    }
}

fn object_report(s: &mut Studio, id: &str, with_render: bool) -> Result<ToolOutput, ToolError> {
    focus_object(s, id);
    let objs = s.evaluated()?;
    let o = objs.iter().find(|o| o.id == id).ok_or_else(|| ToolError::new("not_found", format!("no object '{id}'"), "call `describe` to list ids"))?;
    let findings: Vec<_> = only3d_model::check(&s.project, &objs, &s.bed).into_iter().filter(|f| f.object.as_deref() == Some(id)).collect();
    let mut text = obj_line(o);
    for f in findings.iter().filter(|f| f.severity != Severity::Info) {
        text.push_str(&format!("\n{:?} {}: {} → {}", f.severity, f.code, f.message, f.hint));
    }
    let mut out = ToolOutput::text(text).with_data(json!({"id": id, "report": o.report, "findings": findings}));
    if with_render {
        let png = render::render(s, &RenderRequest { ids: Some(vec![id.to_string()]), ..Default::default() })?;
        out = out.with_image(png, format!("{id} (iso)"));
    }
    Ok(out)
}

fn need_object<'a>(s: &'a mut Studio, id: &str) -> Result<&'a mut Object, ToolError> {
    let ids: Vec<String> = s.project.objects.iter().map(|o| o.id.clone()).collect();
    s.project.object_mut(id).ok_or_else(|| ToolError::new("not_found", format!("no object '{id}'"), format!("existing ids: {}", ids.join(", "))))
}

fn shape_kind(n: &Node) -> &'static str {
    use only3d_model::Shape::*;
    match &n.shape {
        Box { .. } => "box",
        Cylinder { .. } => "cylinder",
        Sphere { .. } => "sphere",
        Torus { .. } => "torus",
        Extrude { .. } => "extrusion",
        Revolve { .. } => "revolve",
        Import { .. } => "part",
        Union { .. } => "union",
        Difference { .. } => "part",
        Intersection { .. } => "part",
        Hull { .. } => "hull",
        Array { .. } => "array",
        Polar { .. } => "polar",
        PushPull { child, .. } => shape_kind(child),
        Slot { .. } => "slot",
        Tube { .. } => "tube",
        Polygon { .. } => "prism",
        Wedge { .. } => "wedge",
        Text { .. } => "text",
        Shell { .. } => "shell",
        Hole { .. } => "hole",
    }
}

pub(crate) fn expand(p: &str) -> std::path::PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(h) = std::env::home_dir() {
            return h.join(rest);
        }
    }
    std::path::PathBuf::from(p)
}

/// Slice the current project with the configured machine provider and package the result.
pub fn run_slice(s: &mut Studio) -> Result<Arc<SliceOutput>, ToolError> {
    let objs = s.evaluated()?;
    let findings = only3d_model::check(&s.project, &objs, &s.bed);
    if let Some(f) = findings.iter().find(|f| f.severity == Severity::Error) {
        return Err(ToolError::new(f.code.clone(), format!("cannot slice: {}", f.message), f.hint.clone()));
    }
    let visible: Vec<only3d_model::EvaluatedObject> = objs.iter().filter(|o| !o.hidden).cloned().collect();
    let setup = s.machine.setup(&s.project, &visible)?;
    let multi = !setup.filament_settings.is_empty();
    // one body per object, or with several filaments one per part (each in its filament)
    let mut bodies = Vec::new();
    for (gi, (o, st)) in visible.iter().zip(&setup.object_settings).enumerate() {
        let mut st = st.clone();
        st.extruder = setup.settings.extruder;
        if multi && !o.parts.is_empty() {
            for (k, part) in o.parts.iter().enumerate() {
                let mut b = only3d_slice::PlateObject::new(format!("{}#{k}", o.id), part.mesh.clone(), st.clone(), gi);
                b.name = if part.name.is_empty() || part.name == o.name { o.name.clone() } else { format!("{} · {}", o.name, part.name) };
                b.filament = part.filament as usize - 1;
                bodies.push(b);
            }
        } else {
            let mut b = only3d_slice::PlateObject::new(o.id.clone(), o.mesh.clone(), st, gi);
            b.name = o.name.clone();
            b.filament = if multi { o.filament as usize - 1 } else { 0 };
            bodies.push(b);
        }
    }
    let job = only3d_slice::SliceJob {
        objects: bodies,
        settings: setup.settings.clone(),
        bed_type: s.project.printer.bed_type.clone(),
        filament_settings: setup.filament_settings.clone(),
        tower: setup.tower.clone(),
        nozzles: setup.nozzles.clone(),
    };
    let mut result = only3d_slice::slice(&job, setup.hooks.as_ref())?;
    result.warnings.extend(setup.notes.iter().cloned());
    // enough filament on each spool?
    let needs: Vec<(usize, f64)> = if multi { result.stats.filaments.iter().map(|(f, _, g)| (*f, *g)).collect() } else { vec![(visible.first().map(|o| o.filament as usize - 1).unwrap_or(0), result.stats.filament_g)] };
    for (slot_idx, need) in needs {
        if let Some(sp) = crate::spools::spool_for_slot(&s.project, slot_idx) {
            if sp.remaining_g < need * 1.05 {
                result.warnings.push(format!("spool '{}' has {:.0} g left but this plate needs {:.1} g of it — swap or weigh the spool (spool_weigh)", sp.id, sp.remaining_g, need));
            }
        }
    }
    let thumb = render::thumbnail(s, 512)?;
    let small = render::thumbnail(s, 128)?;
    let pkg_objs: Vec<only3d_slice::package::PackageObject> = visible.iter().map(|o| only3d_slice::package::PackageObject { name: &o.name, mesh: &o.mesh }).collect();
    let package = only3d_slice::package::write_gcode_3mf(&only3d_slice::package::PackageInfo {
        gcode: &result.gcode,
        stats: &result.stats,
        objects: pkg_objs,
        thumbnail_png: thumb,
        thumbnail_small_png: small,
        filaments: setup
            .filaments
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let (mm, g) = if multi { result.stats.filaments.iter().find(|x| x.0 == i).map(|x| (x.1, x.2)).unwrap_or((0.0, 0.0)) } else { (result.stats.filament_mm, result.stats.filament_g) };
                only3d_slice::package::PackageFilament { kind: &f.kind, color: &f.color, tray_info_idx: &f.id, extruder: f.extruder, used_m: mm / 1000.0, used_g: g }
            })
            .collect(),
        sequence: if multi { result.job.filament_sequence.clone() } else { vec![0] },
        layer_filaments: result.job.layer_filaments.clone(),
        nozzle_diameter: setup.settings.nozzle_diameter,
        bed_type_key: only3d_slice::package::bed_type_key(&s.project.printer.bed_type),
        layer_height: setup.settings.layer_height,
        project_settings: setup.project_settings.clone(),
    })
    .map_err(|e| ToolError::new("package_failed", e, "report this as a bug"))?;
    // extrusions must stay where the nozzles printing them reach (several nozzles: their union)
    let exts: Vec<usize> = if multi { result.job.used_filaments.iter().filter_map(|f| setup.filaments.get(*f).map(|x| x.extruder)).collect() } else { vec![setup.settings.extruder as usize] };
    let area = exts
        .iter()
        .filter_map(|e| s.bed.nozzle_areas.get(*e))
        .map(|a| [a[0] as f64, a[1] as f64, a[2] as f64, a[3] as f64])
        .reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])])
        .unwrap_or([0.0, 0.0, s.bed.size[0] as f64, s.bed.size[1] as f64]);
    let lint = only3d_slice::gcode::lint(&result.gcode, area, 1000.0, 0.15);
    let out = Arc::new(SliceOutput {
        lint,
        revision: s.revision,
        plate: s.plate,
        result,
        package,
        settings_source: setup.source.clone(),
        engine: if setup.printer_safe { "native+bambu-templates".into() } else { "native+generic".into() },
    });
    s.last_slice = Some(out.clone());
    let rev = s.revision;
    s.emit(crate::Event::Sliced { revision: rev });
    Ok(out)
}

fn slice_summary(o: &SliceOutput) -> (String, Value) {
    let st = &o.result.stats;
    let mut text = format!(
        "Sliced: {} layers, est. {}h{:02}m, {:.1} g / {:.2} m filament (~{:.2} cost units), max Z {:.2} mm. Settings: {}. Took {:.0} ms.",
        st.layers,
        (st.print_time_s / 60.0).round() as i64 / 60,
        (st.print_time_s / 60.0).round() as i64 % 60,
        st.filament_g,
        st.filament_mm / 1000.0,
        st.filament_cost,
        st.max_z,
        o.settings_source,
        o.result.timings.slice_ms + o.result.timings.plan_ms + o.result.timings.gcode_ms
    );
    if st.filaments.len() > 1 {
        let parts: Vec<String> = st.filaments.iter().map(|(f, mm, g)| format!("filament {} {:.1} g / {:.2} m", f + 1, g, mm / 1000.0)).collect();
        text.push_str(&format!(
            "\nFilaments: {}; {} filament changes, prime tower {:.1} g{}.",
            parts.join(", "),
            st.toolchanges,
            st.tower_g,
            o.result.job.tower.as_ref().map(|t| format!(" at x {:.0}–{:.0}, y {:.0}–{:.0}", t.min[0], t.max[0], t.min[1], t.max[1])).unwrap_or_default()
        ));
    }
    if o.lint.ok {
        text.push_str(&format!("\nG-code check passed: {} extrusion moves inside x {:.1}..{:.1}, y {:.1}..{:.1}; max {:.0} mm/s.", o.lint.extrusion_moves, o.lint.extrude_bounds[0], o.lint.extrude_bounds[2], o.lint.extrude_bounds[1], o.lint.extrude_bounds[3], o.lint.max_feed_mm_s));
    } else {
        text.push_str(&format!("\nG-code check FAILED: {}", o.lint.problems.join("; ")));
    }
    for w in &o.result.warnings {
        text.push_str(&format!("\nWarning: {w}"));
    }
    let data = json!({
        "layers": st.layers, "print_time_s": st.print_time_s, "filament_g": st.filament_g, "filament_m": st.filament_mm / 1000.0,
        "filament_cost": st.filament_cost, "max_z": st.max_z, "warnings": o.result.warnings, "timings_ms": o.result.timings,
        "engine": o.engine, "settings_source": o.settings_source, "feature_time_s": st.feature_time_s,
        "package_bytes": o.package.len(), "gcode_bytes": o.result.gcode.len(), "lint": o.lint,
        "filaments": st.filaments.iter().map(|(f, mm, g)| json!({"slot": f + 1, "grams": g, "metres": mm / 1000.0})).collect::<Vec<_>>(),
        "filament_changes": st.toolchanges, "tower_g": st.tower_g,
    });
    (text, data)
}

// ---------------------------------------------------------------------------------------------
// tool implementations

fn t_describe(s: &mut Studio, i: DescribeIn) -> Result<ToolOutput, ToolError> {
    let (t, d) = describe::describe(s)?;
    let mut out = ToolOutput::text(t).with_data(d);
    if i.render {
        let png = render::render(s, &RenderRequest::default())?;
        out = out.with_image(png, "plate (iso)");
    }
    Ok(out)
}

fn t_get_project(s: &mut Studio, _: Empty) -> Result<ToolOutput, ToolError> {
    let v = serde_json::to_value(&s.project).unwrap_or(Value::Null);
    Ok(ToolOutput::text(serde_json::to_string_pretty(&s.project).unwrap_or_default()).with_data(v))
}

fn t_set_project(s: &mut Studio, i: SetProjectIn) -> Result<ToolOutput, ToolError> {
    let mut ids = std::collections::HashSet::new();
    for o in &i.project.objects {
        if !ids.insert(o.id.clone()) {
            return Err(ToolError::new("duplicate_id", format!("object id '{}' is used twice", o.id), "object ids must be unique"));
        }
    }
    s.project = i.project;
    // evaluate now so errors surface (and roll back) immediately
    s.changed("set_project");
    let objs = s.evaluated()?;
    Ok(ToolOutput::text(format!("Project replaced: {} object(s).\n{}", objs.len(), objs.iter().map(obj_line).collect::<Vec<_>>().join("\n"))))
}

fn t_new_project(s: &mut Studio, i: NewProjectIn) -> Result<ToolOutput, ToolError> {
    s.project = only3d_model::Project { name: i.name.unwrap_or_else(|| "untitled".into()), ..Default::default() };
    s.project_path = None;
    s.selection.clear();
    s.last_slice = None;
    s.plate = 1;
    Ok(ToolOutput::text(format!("New project \"{}\" for {}.", s.project.name, s.project.printer.machine)))
}

fn t_open_project(s: &mut Studio, i: PathIn) -> Result<ToolOutput, ToolError> {
    s.load(&expand(&i.path))?;
    let (t, d) = describe::describe(s)?;
    Ok(ToolOutput::text(format!("Opened {}.\n{t}", i.path)).with_data(d))
}

/// The .scad files an object is made from (its shape and its parts).
pub fn scad_files(o: &Object) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in std::iter::once(&o.shape).chain(o.parts.iter().map(|p| &p.shape)) {
        n.visit(&mut |n| {
            if let only3d_model::Shape::Import { path, .. } = &n.shape {
                if only3d_model::scad::is_scad(std::path::Path::new(path)) && !out.contains(path) {
                    out.push(path.clone());
                }
            }
        });
    }
    out
}

/// One line per variable: name=value, with the file's value when it was changed.
fn scad_vars_line(vars: &[only3d_model::scad::ScadVar], set: &BTreeMap<String, Value>) -> String {
    fn show(v: &Value) -> String {
        match v {
            Value::String(t) => format!("{t:?}"),
            Value::Number(n) => only3d_model::scad::fmt(n.as_f64().unwrap_or(0.0)),
            Value::Array(a) => format!("[{}]", a.iter().map(show).collect::<Vec<_>>().join(", ")),
            other => other.to_string(),
        }
    }
    vars.iter()
        .map(|v| {
            let range = match (v.min, v.max) {
                (Some(a), Some(b)) => format!(" [{a}–{b}]"),
                _ if !v.options.is_empty() => format!(" [{}]", v.options.iter().map(|o| show(&o.1)).collect::<Vec<_>>().join("|")),
                _ => String::new(),
            };
            match set.get(&v.name) {
                Some(x) => format!("{}={} (file {}){range}", v.name, show(x), show(&v.initial)),
                None => format!("{}={}{range}", v.name, show(&v.initial)),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// set_params with `object`: the OpenSCAD variables of an object's .scad import.
fn set_scad_vars(s: &mut Studio, id: &str, vals: BTreeMap<String, Value>) -> Result<ToolOutput, ToolError> {
    let o = s.project.object(id).ok_or_else(|| ToolError::new("not_found", format!("no object '{id}'"), "call `describe` to list ids"))?;
    let files = scad_files(o);
    let Some(file) = files.first().cloned() else {
        return Err(ToolError::new("not_scad", format!("{id} is not made from a .scad file"), "without `object`, set_params sets the project's parameters"));
    };
    focus_object(s, id);
    let decl = s.evaluator.scad_vars(&file)?;
    let names = || decl.iter().map(|v| v.name.as_str()).collect::<Vec<_>>().join(", ");
    for (k, v) in &vals {
        let Some(d) = decl.iter().find(|d| &d.name == k) else {
            return Err(ToolError::new("unknown_variable", format!("{file} has no customizer variable '{k}' (it has: {})", names()), "names are case-sensitive; variables in its [Hidden] section can't be set"));
        };
        let bad = |why: &str| ToolError::new("bad_value", format!("{k}: {why}"), "numbers may be expressions over the project parameters, e.g. \"wall*2\"");
        let num_ok = |x: &Value| match x {
            Value::Number(_) => Ok(()),
            Value::String(e) => only3d_model::expr::eval(e, &s.project.params).map(|_| ()).map_err(|e| bad(&e.to_string())),
            _ => Err(bad("expected a number")),
        };
        match (d.kind.as_str(), v) {
            (_, Value::Null) => {}
            ("number", x) => num_ok(x)?,
            ("vector", Value::Array(a)) if a.len() == d.initial.as_array().map_or(0, |i| i.len()) => a.iter().try_for_each(num_ok)?,
            ("vector", _) => return Err(bad(&format!("expected a list of {} numbers", d.initial.as_array().map_or(0, |i| i.len())))),
            ("boolean", Value::Bool(_)) | ("string", Value::String(_)) => {}
            (kind, _) => return Err(bad(&format!("expected a {kind}"))),
        }
    }
    let initial: BTreeMap<String, Value> = decl.iter().map(|d| (d.name.clone(), d.initial.clone())).collect();
    let o = s.project.object_mut(id).expect("checked");
    let mut now = BTreeMap::new();
    for n in std::iter::once(&mut o.shape).chain(o.parts.iter_mut().map(|p| &mut p.shape)) {
        n.visit_mut(&mut |n| {
            if let only3d_model::Shape::Import { path, vars, .. } = &mut n.shape {
                if only3d_model::scad::is_scad(std::path::Path::new(path)) {
                    for (k, v) in &vals {
                        // the file's own value needs no override
                        if v.is_null() || initial.get(k).is_some_and(|i| i == v || (i.as_f64().is_some() && i.as_f64() == v.as_f64())) {
                            vars.remove(k);
                        } else {
                            vars.insert(k.clone(), v.clone());
                        }
                    }
                    now = vars.clone();
                }
            }
        });
    }
    s.changed("set_params");
    let objs = s.evaluated()?;
    let line = objs.iter().find(|o| o.id == id).map(obj_line).unwrap_or_default();
    Ok(ToolOutput::text(format!("{id} ({file}): {}\n{line}", scad_vars_line(&decl, &now))).with_data(json!({"id": id, "vars": now})))
}

fn t_save_project(s: &mut Studio, i: SaveIn) -> Result<ToolOutput, ToolError> {
    let p = s.save(i.path.as_deref().map(expand).as_deref())?;
    Ok(ToolOutput::text(format!("Saved to {}", p.display())).with_data(json!({"path": p})))
}

fn t_set_params(s: &mut Studio, i: SetParamsIn) -> Result<ToolOutput, ToolError> {
    if let Some(id) = &i.object {
        return set_scad_vars(s, id, i.params);
    }
    for (k, v) in i.params {
        if !k.chars().all(|c| c.is_alphanumeric() || c == '_') || k.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return Err(ToolError::new("bad_param_name", format!("'{k}' is not a valid parameter name"), "use letters, digits and _ (not starting with a digit)"));
        }
        match v {
            Value::Null => {
                s.project.params.remove(&k);
            }
            Value::Number(n) => {
                s.project.params.insert(k, n.as_f64().unwrap_or(0.0));
            }
            other => return Err(ToolError::new("bad_value", format!("{k}: parameters are numbers, not {other}"), "give a number (null removes it); for a .scad object's variables pass `object`")),
        }
    }
    s.changed("set_params");
    let objs = s.evaluated()?;
    Ok(ToolOutput::text(format!(
        "Params: {}\n{}",
        s.project.params.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(", "),
        objs.iter().map(obj_line).collect::<Vec<_>>().join("\n")
    )))
}

fn t_add_object(s: &mut Studio, i: AddObjectIn) -> Result<ToolOutput, ToolError> {
    let id = match i.id {
        Some(id) => {
            if s.project.object(&id).is_some() {
                return Err(ToolError::new("duplicate_id", format!("object '{id}' already exists"), "pick another id or use update_object"));
            }
            if id.is_empty() || !id.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
                return Err(ToolError::new("bad_id", format!("'{id}' is not a valid id"), "use letters, digits, _ and -"));
            }
            id
        }
        None => s.project.unique_id(i.name.as_deref().unwrap_or(shape_kind(&i.shape))),
    };
    let mut o = Object::new(id.clone(), i.shape);
    o.plate = s.plate;
    if let Some(n) = i.name {
        o.name = n;
    }
    o.position = i.position;
    if let Some(r) = i.rotation {
        o.rotation = r;
    }
    if let Some(f) = i.filament {
        o.filament = f;
    }
    o.parts = i.parts.unwrap_or_default();
    let auto_place = o.position.is_none();
    s.project.objects.push(o);
    let mut placed = None;
    if auto_place {
        s.changed("add_object");
        placed = place_new(s, &id)?;
    }
    s.changed("add_object");
    let mut out = object_report(s, &id, i.render)?;
    out.text = format!("Added {}", out.text);
    if let Some(n) = placed {
        out.text.push_str(&format!("\n{n}"));
        if let Some(o) = out.data.as_object_mut() {
            o.insert("placed".into(), json!(n));
        }
    }
    Ok(out)
}

/// Place a new object next to existing ones without moving them: try plate centre, then a ring of
/// candidate spots, else the nearest free spot or a new plate (see `settle_import`). Returns a
/// note when it had to go onto a new plate.
fn place_new(s: &mut Studio, id: &str) -> Result<Option<String>, ToolError> {
    let objs = s.evaluated()?;
    let me = objs.iter().find(|o| o.id == id).cloned().ok_or_else(|| ToolError::new("not_found", id.to_string(), ""))?;
    let others: Vec<_> = objs.iter().filter(|o| o.id != id && !o.hidden).map(|o| o.report.bounds).collect();
    let size = me.report.size;
    let gap = 6.0;
    let c = s.bed.center();
    let fits = |x: f32, y: f32| {
        let (x0, x1, y0, y1) = (x - size.x / 2.0, x + size.x / 2.0, y - size.y / 2.0, y + size.y / 2.0);
        x0 >= 25.0 + gap && x1 <= 325.0 - gap && y0 >= gap && y1 <= s.bed.size[1] - gap && others.iter().all(|b| x1 + gap <= b.min.x || x0 - gap >= b.max.x || y1 + gap <= b.min.y || y0 - gap >= b.max.y)
    };
    let mut best = None;
    'outer: for r in 0..40 {
        let rr = r as f32 * 8.0;
        let n = if r == 0 { 1 } else { 8 + r * 4 };
        for k in 0..n {
            let a = k as f32 / n as f32 * std::f32::consts::TAU;
            let (x, y) = (c[0] + rr * a.cos(), c[1] + rr * a.sin());
            if fits(x, y) {
                best = Some([(x * 10.0).round() / 10.0, (y * 10.0).round() / 10.0]);
                break 'outer;
            }
        }
    }
    match best {
        Some(p) => {
            if let Some(o) = s.project.object_mut(id) {
                o.position = Some(p);
            }
            Ok(None)
        }
        None => {
            if let Some(o) = s.project.object_mut(id) {
                o.position = Some(c);
            }
            s.changed("add_object");
            let plate = s.plate;
            let st = settle_import(s, &[id.to_string()], plate, None)?;
            if st.plate != s.plate {
                s.set_plate(st.plate);
            }
            Ok(st.note)
        }
    }
}

/// Where an import's objects ended up.
struct Settled {
    /// Said to the user when they did not land where asked.
    note: Option<String>,
    plate: u32,
    /// Too big for the plate as laid out: the plate was arranged.
    arranged: bool,
}

/// Keep freshly added objects `ids` (on `plate`) clear of what was already there. They move as
/// one block, so a file's layout is kept: to `want` (else where they are) when that is free,
/// else the nearest free spot, else onto a new plate. Nothing that was there before moves.
fn settle_import(s: &mut Studio, ids: &[String], plate: u32, want: Option<[f32; 2]>) -> Result<Settled, ToolError> {
    let all = s.evaluated_all()?;
    let rect = |b: &only3d_geom::Aabb| [b.min.x, b.min.y, b.max.x, b.max.y];
    let mine: Vec<&only3d_model::EvaluatedObject> = all.iter().filter(|o| o.plate == plate && ids.contains(&o.id)).collect();
    if mine.is_empty() {
        return Ok(Settled { note: None, plate, arranged: false });
    }
    let label = |o: &only3d_model::EvaluatedObject| if o.name.is_empty() { o.id.clone() } else { o.name.clone() };
    let others: Vec<(String, [f32; 4])> = all.iter().filter(|o| o.plate == plate && !o.hidden && !ids.contains(&o.id)).map(|o| (label(o), rect(&o.report.bounds))).collect();
    let g = mine.iter().fold(only3d_geom::Aabb::EMPTY, |g, o| g.union(&o.report.bounds));
    let (w, d) = (g.max.x - g.min.x, g.max.y - g.min.y);
    let centre = [(g.min.x + g.max.x) / 2.0, (g.min.y + g.max.y) / 2.0];
    let at = want.unwrap_or(centre);
    let r = [at[0] - w / 2.0, at[1] - d / 2.0, at[0] + w / 2.0, at[1] + d / 2.0];
    let bed = s.bed.size;
    let inside = r[0] >= -1e-3 && r[1] >= -1e-3 && r[2] <= bed[0] + 1e-3 && r[3] <= bed[1] + 1e-3;
    let mut hit: Vec<String> = others.iter().filter(|(_, t)| r[0] < t[2] && r[2] > t[0] && r[1] < t[3] && r[3] > t[1]).map(|(n, _)| format!("'{n}'")).collect();
    hit.dedup();
    let (what, it) = if mine.len() == 1 { (format!("'{}'", label(mine[0])), "it") } else { (format!("the {} new objects", mine.len()), "they") };
    let centres: Vec<(String, [f32; 2])> = mine.iter().map(|o| (o.id.clone(), [(o.report.bounds.min.x + o.report.bounds.max.x) / 2.0, (o.report.bounds.min.y + o.report.bounds.max.y) / 2.0])).collect();
    let shift = |s: &mut Studio, by: [f32; 2]| {
        for (id, c) in &centres {
            if let Some(o) = s.project.object_mut(id) {
                let p = o.position.unwrap_or(*c);
                o.position = Some([((p[0] + by[0]) * 100.0).round() / 100.0, ((p[1] + by[1]) * 100.0).round() / 100.0]);
            }
        }
        s.changed("import_file");
    };
    if inside && hit.is_empty() {
        if want.is_some() {
            shift(s, [at[0] - centre[0], at[1] - centre[1]]);
        }
        return Ok(Settled { note: None, plate, arranged: false });
    }
    let taken: Vec<[f32; 4]> = others.iter().map(|(_, t)| *t).collect();
    let area = crate::arrange::reach(s);
    if let Some(spot) = crate::arrange::free_spots_in(area, taken, [w, d], at, 1, 6.0).first() {
        shift(s, [spot[0] - centre[0], spot[1] - centre[1]]);
        let note = (!hit.is_empty()).then(|| format!("{what} would have landed on {}, so {it} moved to the nearest free spot.", hit.join(", ")));
        return Ok(Settled { note, plate, arranged: false });
    }
    let arrange = |s: &mut Studio, plate: u32| -> Result<(), ToolError> {
        let mut ev = std::mem::replace(&mut s.evaluator, only3d_model::Evaluator::new("."));
        let r = s.project.arrange(&mut ev, &s.bed, 6.0, plate);
        s.evaluator = ev;
        s.changed("import_file");
        r?;
        Ok(())
    };
    if others.is_empty() {
        // alone on the plate and bigger than the area both nozzles reach as laid out
        arrange(s, plate)?;
        return Ok(Settled { note: None, plate, arranged: true });
    }
    // no room left: a plate of its own, nothing else moves
    let np = s.project.plate_count() + 1;
    s.project.ensure_plates(np);
    for o in s.project.objects.iter_mut().filter(|o| ids.contains(&o.id)) {
        o.plate = np;
    }
    let fits = w <= area[2] - area[0] && d <= area[3] - area[1];
    if fits {
        shift(s, [(area[0] + area[2]) / 2.0 - centre[0], (area[1] + area[3]) / 2.0 - centre[1]]);
    } else {
        arrange(s, np)?;
    }
    Ok(Settled { note: Some(format!("Plate {plate} had no room for {what}, so {it} went onto a new plate {np}.")), plate: np, arranged: !fits })
}

/// The filament slot that prints with inventory spool `id`: the slot already linked to it, or
/// a new slot set up from the spool (preset, colour, link).
pub fn slot_for_spool(s: &mut Studio, id: &str) -> Result<u32, ToolError> {
    if let Some(k) = s.project.filaments.iter().position(|f| f.spool.as_deref() == Some(id)) {
        return Ok(k as u32 + 1);
    }
    let sp = crate::spools::Inventory::load().get(id).cloned().ok_or_else(|| ToolError::new("not_found", format!("no spool '{id}'"), "see spool_list"))?;
    // an unlinked slot with the same preset and colour takes the spool rather than a new slot
    let same = s.project.filaments.iter().position(|f| f.spool.is_none() && f.color.eq_ignore_ascii_case(&sp.color) && sp.preset.as_ref().is_none_or(|p| *p == f.preset));
    let k = match same {
        Some(k) => k,
        None => {
            s.project.filaments.push(FilamentSlot { preset: sp.preset.clone().unwrap_or_else(|| "Bambu PLA Basic @BBL H2D".into()), color: sp.color.clone(), nozzle: NozzleSel::Auto, spool: None });
            s.project.filaments.len() - 1
        }
    };
    let f = &mut s.project.filaments[k];
    f.spool = Some(sp.id.clone());
    f.color = sp.color.clone();
    if let Some(p) = &sp.preset {
        f.preset = p.clone();
    }
    Ok(k as u32 + 1)
}

fn t_update_object(s: &mut Studio, i: UpdateObjectIn) -> Result<ToolOutput, ToolError> {
    need_object(s, &i.id)?;
    let spool_slot = match i.spool.as_deref() {
        Some(sp) => Some(slot_for_spool(s, sp)?),
        None => None,
    };
    let o = need_object(s, &i.id)?;
    if let Some(k) = spool_slot {
        o.filament = k;
    }
    if let Some(v) = i.name {
        o.name = v;
    }
    if let Some(v) = i.shape {
        o.shape = v;
    }
    if let Some(v) = i.position {
        o.position = Some(v);
    }
    if let Some(v) = i.rotation {
        o.rotation = v;
    }
    if let Some(v) = i.scale {
        if v.iter().any(|x| *x <= 0.0) {
            return Err(ToolError::new("bad_scale", "scale factors must be > 0", "use mirror in the shape for flipping"));
        }
        o.scale = v;
    }
    if let Some(v) = i.filament {
        o.filament = v;
    }
    if let Some(v) = i.hidden {
        o.hidden = v;
    }
    if let Some(v) = i.locked {
        o.locked = v;
    }
    if let Some(v) = i.parts {
        o.parts = v;
    }
    if let Some(axis) = &i.mirror {
        let normal = match axis.as_str() {
            "x" => [1.0, 0.0, 0.0],
            "y" => [0.0, 1.0, 0.0],
            other => return Err(ToolError::new("bad_axis", format!("mirror '{other}'"), "x (left–right) or y (front–back)")),
        };
        // mirror the body and every part the same way, in the object's own frame
        let wrap = |n: Node| -> Node { Node { shape: only3d_model::Shape::Union { children: vec![n] }, at: None, rotate: None, scale: None, mirror: Some(normal), label: Some("mirrored".into()) } };
        o.shape = wrap(o.shape.clone());
        for part in o.parts.iter_mut() {
            part.shape = wrap(part.shape.clone());
        }
    }
    if let Some(v) = i.settings {
        o.settings = v;
    }
    s.changed("update_object");
    let pulled = match &i.push_pull {
        Some(pp) => Some(push_pull(s, &i.id, pp.at, pp.distance)?),
        None => None,
    };
    let mut out = object_report(s, &i.id, i.render)?;
    out.text = match pulled {
        Some(t) => format!("{t}\n{}", out.text),
        None => format!("Updated {}", out.text),
    };
    Ok(out)
}

/// Push/pull: wrap the body under `at` (plate mm) in a `push_pull` step, in the object's own
/// frame, and keep the rest of the object where it was.
fn push_pull(s: &mut Studio, id: &str, at: [f32; 3], distance: f32) -> Result<String, ToolError> {
    if !distance.is_finite() || distance.abs() < 0.001 || distance.abs() > 1000.0 {
        return Err(ToolError::new("bad_distance", format!("distance {distance}"), "a distance in mm, + pulls out, − pushes in"));
    }
    let o = need_object(s, id)?.clone();
    let l = o.local_transform();
    let mut bodies: Vec<Node> = vec![o.shape.clone()];
    bodies.extend(o.parts.iter().map(|p| p.shape.clone()));
    let params = s.project.params.clone();
    let place = |s: &mut Studio, bodies: &[Node]| -> Result<(Vec<only3d_geom::Mesh>, only3d_geom::Aabb), ToolError> {
        let mut out = Vec::new();
        let mut bb = only3d_geom::Aabb::EMPTY;
        for b in bodies {
            let m = s.evaluator.eval(b, &params)?.transformed(&l);
            bb = bb.union(&m.bounds());
            out.push(m);
        }
        Ok((out, bb))
    };
    let (placed, bb) = place(s, &bodies)?;
    let pos = o.position.unwrap_or_else(|| s.bed.center());
    let shift = glam::Vec3::new(pos[0] - (bb.min.x + bb.max.x) / 2.0, pos[1] - (bb.min.y + bb.max.y) / 2.0, -bb.min.z);
    let p = glam::Vec3::from(at);
    let mut best: Option<(f32, usize, only3d_geom::faces::FlatFace, glam::Vec3)> = None;
    for (k, m) in placed.iter().enumerate() {
        let mut w = m.clone();
        w.translate(shift);
        if let Some(f) = only3d_geom::faces::face_near(&w, p, 1.0, 0.01) {
            let c = f.centre(&w);
            let d = (p - c).dot(f.normal).abs();
            if best.as_ref().is_none_or(|b| d < b.0) {
                best = Some((d, k, f, c));
            }
        }
    }
    let Some((_, k, face, c)) = best else {
        return Err(ToolError::new("no_face", format!("no face of '{id}' at {at:?}"), "pass a point on a flat face, in plate mm (e.g. a face centre)"));
    };
    let n_w = face.normal;
    // the point on the face's plane, in the object's own frame
    let on = p - n_w * (p - c).dot(n_w);
    let at_l = l.inverse().transform_point3(on - shift);
    let mt = glam::Mat3::from_mat4(l).transpose() * n_w;
    let (n_l, d_l) = (mt.normalize(), distance / mt.length());
    let r4 = |v: f32| (v * 10000.0).round() / 10000.0;
    let wrapped = Node::from(only3d_model::Shape::PushPull { child: Box::new(bodies[k].clone()), point: at_l.to_array().map(r4), normal: n_l.to_array().map(r4), distance: (r4(d_l) as f64).into() });
    bodies[k] = wrapped;
    let (_, nb) = place(s, &bodies)?;
    let o = need_object(s, id)?;
    if k == 0 {
        o.shape = bodies[0].clone();
    } else {
        o.parts[k - 1].shape = bodies[k].clone();
    }
    // the far side stays put: re-centre on the new footprint
    let np = [pos[0] - (bb.min.x + bb.max.x) / 2.0 + (nb.min.x + nb.max.x) / 2.0, pos[1] - (bb.min.y + bb.max.y) / 2.0 + (nb.min.y + nb.max.y) / 2.0];
    o.position = Some([(np[0] * 100.0).round() / 100.0, (np[1] * 100.0).round() / 100.0]);
    s.changed("update_object");
    let side = match n_w {
        n if n.z > 0.999 => "top",
        n if n.z < -0.999 => "bottom",
        n if n.x > 0.999 => "right (+X)",
        n if n.x < -0.999 => "left (−X)",
        n if n.y > 0.999 => "back (+Y)",
        n if n.y < -0.999 => "front (−Y)",
        _ => "slanted",
    };
    Ok(format!("{} the {side} face ({:.0} mm²) of '{id}' {} {} mm.", if distance > 0.0 { "Pulled" } else { "Pushed" }, face.area, if distance > 0.0 { "out" } else { "in" }, r4(distance.abs())))
}

/// Copies laid out next to the original (a row in a direction, or a grid), not re-packed.
fn duplicate_placed(s: &mut Studio, src: &Object, i: &DuplicateIn) -> Result<ToolOutput, ToolError> {
    focus_object(s, &src.id);
    let objs = s.evaluated()?;
    let me = objs.iter().find(|o| o.id == src.id).ok_or_else(|| ToolError::new("not_found", src.id.clone(), ""))?;
    let b = me.report.bounds;
    let (w, d) = (b.max.x - b.min.x, b.max.y - b.min.y);
    let (cx, cy) = ((b.min.x + b.max.x) / 2.0, (b.min.y + b.max.y) / 2.0);
    let mut spots: Vec<[f32; 2]> = Vec::new();
    match (&i.direction, i.grid) {
        (_, Some([rows, cols])) => {
            if rows == 0 || cols == 0 || rows * cols > 201 {
                return Err(ToolError::new("bad_grid", "grid must be at least [1, 1] and at most 200 copies", ""));
            }
            let g = i.gap.unwrap_or(5.0);
            for r in 0..rows {
                for c in 0..cols {
                    if r + c > 0 {
                        spots.push([cx + c as f32 * (w + g), cy + r as f32 * (d + g)]);
                    }
                }
            }
        }
        (Some(dir), None) => {
            if i.copies == 0 || i.copies > 200 {
                return Err(ToolError::new("bad_count", "copies must be 1..200", ""));
            }
            let g = i.gap.unwrap_or(0.0);
            let step = match dir.as_str() {
                "+x" | "right" => [w + g, 0.0],
                "-x" | "left" => [-(w + g), 0.0],
                "+y" | "back" => [0.0, d + g],
                "-y" | "front" => [0.0, -(d + g)],
                other => return Err(ToolError::new("bad_direction", format!("'{other}'"), "+x | -x | +y | -y")),
            };
            for k in 1..=i.copies {
                spots.push([cx + step[0] * k as f32, cy + step[1] * k as f32]);
            }
        }
        _ => unreachable!(),
    }
    let mut new_ids = Vec::new();
    for spot in spots {
        let mut c = src.clone();
        c.id = s.project.unique_id(&src.id);
        if src.name.is_empty() || src.name == src.id {
            c.name = c.id.clone();
        }
        c.position = Some([(spot[0] * 100.0).round() / 100.0, (spot[1] * 100.0).round() / 100.0]);
        c.locked = false;
        new_ids.push(c.id.clone());
        s.project.objects.push(c);
    }
    s.changed("duplicate_object");
    let objs = s.evaluated()?;
    let bed = s.bed.size;
    let off: Vec<&str> = objs.iter().filter(|o| new_ids.contains(&o.id)).filter(|o| { let b = o.report.bounds; b.min.x < 0.0 || b.min.y < 0.0 || b.max.x > bed[0] || b.max.y > bed[1] }).map(|o| o.id.as_str()).collect();
    let mut text = format!("Added {} cop{} next to {}: {}.", new_ids.len(), if new_ids.len() == 1 { "y" } else { "ies" }, src.id, new_ids.join(", "));
    if !off.is_empty() {
        text.push_str(&format!(" Off the plate: {} (try another direction, a smaller gap, or arrange).", off.join(", ")));
    }
    Ok(ToolOutput::text(text).with_data(json!({"ids": new_ids, "off_plate": off})))
}

#[derive(Deserialize, JsonSchema)]
pub struct SplitIn {
    pub id: String,
    /// x | y | z
    pub axis: String,
    /// plate mm (default: the middle)
    #[serde(default)]
    pub at: Option<f32>,
    /// default 5
    #[serde(default)]
    pub gap: Option<f32>,
}

fn t_split(s: &mut Studio, i: SplitIn) -> Result<ToolOutput, ToolError> {
    let src = need_object(s, &i.id)?.clone();
    if !src.parts.is_empty() {
        return Err(ToolError::new("has_parts", format!("'{}' has parts in other filaments", i.id), "split works on single-body objects for now"));
    }
    let axis = match i.axis.as_str() {
        "x" => 0usize,
        "y" => 1,
        "z" => 2,
        other => return Err(ToolError::new("bad_axis", format!("axis '{other}'"), "x | y | z")),
    };
    focus_object(s, &i.id);
    let objs = s.evaluated()?;
    let world = objs.iter().find(|o| o.id == i.id).ok_or_else(|| ToolError::new("not_found", i.id.clone(), ""))?.report.bounds;
    // the shape with its turn and scale baked in, so the cut is along plate axes
    let baked = Node { shape: only3d_model::Shape::Union { children: vec![src.shape.clone()] }, at: None, rotate: Some(src.rotation.map(|v| (v as f64).into())), scale: Some(src.scale.map(|v| (v as f64).into())), mirror: None, label: None };
    let local = s.evaluator.eval(&baked, &s.project.params)?.bounds();
    let (wmin, wmax) = (world.min[axis], world.max[axis]);
    let cut_w = i.at.unwrap_or((wmin + wmax) / 2.0);
    if cut_w <= wmin + 0.1 || cut_w >= wmax - 0.1 {
        return Err(ToolError::new("outside", format!("{cut_w} mm is outside '{}' ({wmin:.1}–{wmax:.1} along {})", i.id, i.axis), "pick a point inside the object"));
    }
    let cut = local.min[axis] + (cut_w - wmin);
    // half-space boxes in the baked frame
    let big = 10_000.0f64;
    let half = |below: bool| -> Node {
        let lo = if below { local.min[axis] as f64 - 1.0 } else { cut as f64 };
        let hi = if below { cut as f64 } else { local.max[axis] as f64 + 1.0 };
        let mut size = [big, big, big];
        size[axis] = hi - lo;
        let mut at = [0.0f64; 3];
        at[axis] = if axis == 2 { lo } else { (lo + hi) / 2.0 };
        if axis != 2 {
            at[2] = -big / 2.0;
        }
        Node { shape: only3d_model::Shape::Box { size: size.map(|v| v.into()), radius: None, chamfer: None, corner_radius: None, chamfer_top: None, chamfer_bottom: None, fillet_top: None, fillet_bottom: None, center: false }, at: Some(at.map(|v| v.into())), rotate: None, scale: None, mirror: None, label: None }
    };
    let gap = i.gap.unwrap_or(5.0);
    let (cx, cy) = ((world.min.x + world.max.x) / 2.0, (world.min.y + world.max.y) / 2.0);
    let mut ids = Vec::new();
    for (k, below) in [(0, true), (1, false)] {
        let mut o = src.clone();
        o.id = s.project.unique_id(&format!("{}_{}", src.id, if k == 0 { "a" } else { "b" }));
        o.name = if src.name.is_empty() || src.name == src.id { o.id.clone() } else { format!("{} ({})", src.name, if k == 0 { "a" } else { "b" }) };
        o.shape = Node { shape: only3d_model::Shape::Intersection { children: vec![baked.clone(), half(below)] }, at: None, rotate: None, scale: None, mirror: None, label: None };
        o.rotation = [0.0; 3];
        o.scale = [1.0; 3];
        // the halves sit where they were, pulled apart by the gap (a top half goes beside)
        let mut c = [cx, cy];
        if axis < 2 {
            let mid_lo = (wmin + cut_w) / 2.0;
            let mid_hi = (cut_w + wmax) / 2.0;
            c[axis] = if below { mid_lo - gap / 2.0 } else { mid_hi + gap / 2.0 };
        } else if !below {
            c[0] = world.max.x + gap + (world.max.x - world.min.x) / 2.0;
        }
        o.position = Some([(c[0] * 100.0).round() / 100.0, (c[1] * 100.0).round() / 100.0]);
        ids.push(o.id.clone());
        s.project.objects.push(o);
    }
    s.project.objects.retain(|o| o.id != i.id);
    s.selection.retain(|x| x != &i.id);
    s.changed("split_object");
    let objs = s.evaluated()?;
    let lines: Vec<String> = objs.iter().filter(|o| ids.contains(&o.id)).map(obj_line).collect();
    Ok(ToolOutput::text(format!("Split '{}' at {cut_w:.1} mm along {} into {}:\n{}", i.id, i.axis, ids.join(" and "), lines.join("\n"))).with_data(json!({"ids": ids})))
}

fn t_remove_object(s: &mut Studio, i: IdsIn) -> Result<ToolOutput, ToolError> {
    for id in &i.ids {
        need_object(s, id)?;
    }
    s.project.objects.retain(|o| !i.ids.contains(&o.id));
    s.selection.retain(|x| !i.ids.contains(x));
    Ok(ToolOutput::text(format!("Removed {}. {} object(s) left.", i.ids.join(", "), s.project.objects.len())))
}

fn t_duplicate(s: &mut Studio, i: DuplicateIn) -> Result<ToolOutput, ToolError> {
    let src = need_object(s, &i.id)?.clone();
    if i.direction.is_some() || i.grid.is_some() {
        return duplicate_placed(s, &src, &i);
    }
    if !i.fill && (i.copies == 0 || i.copies > 200) {
        return Err(ToolError::new("bad_count", "copies must be 1..200", ""));
    }
    if let Some(off) = i.offset {
        // a fixed step (an ⌥-drag copy, or repeating one): exactly where asked
        focus_object(s, &src.id);
        let c0 = match src.position {
            Some(p) => p,
            None => s.evaluated()?.iter().find(|o| o.id == src.id).map(|o| [(o.report.bounds.min.x + o.report.bounds.max.x) / 2.0, (o.report.bounds.min.y + o.report.bounds.max.y) / 2.0]).unwrap_or(s.bed.center()),
        };
        let mut new_ids = Vec::new();
        for k in 1..=i.copies {
            let mut c = src.clone();
            c.id = s.project.unique_id(&src.id);
            if src.name.is_empty() || src.name == src.id {
                c.name = c.id.clone();
            }
            let f = k as f32;
            c.position = Some([((c0[0] + off[0] * f) * 100.0).round() / 100.0, ((c0[1] + off[1] * f) * 100.0).round() / 100.0]);
            c.locked = false;
            new_ids.push(c.id.clone());
            s.project.objects.push(c);
        }
        s.changed("duplicate_object");
        let n = new_ids.len();
        return Ok(ToolOutput::text(format!("Added {n} cop{} of {}, each [{}, {}] mm on from the last: {}.", if n == 1 { "y" } else { "ies" }, src.id, off[0], off[1], new_ids.join(", "))).with_data(json!({"ids": new_ids, "not_placed": 0})));
    }
    // copies go into free space around the original; nothing already on the plate moves
    focus_object(s, &src.id);
    let want = if i.fill { 200 } else { i.copies as usize };
    let spots = crate::arrange::free_spots(s, &src.id, want, i.gap.unwrap_or(5.0))?;
    if spots.is_empty() {
        return Err(ToolError::new("no_room", format!("no free space on plate {} for another '{}'", s.plate, src.id), "free some space, use a smaller gap, `arrange` the plate, or duplicate onto a new plate (plate {\"action\": \"move\"})"));
    }
    let mut new_ids = Vec::new();
    for spot in &spots {
        let mut c = src.clone();
        c.id = s.project.unique_id(&src.id);
        // an object named after its id gets its own: ghost, ghost_2, ghost_3 (not ghost three times)
        if src.name.is_empty() || src.name == src.id {
            c.name = c.id.clone();
        }
        c.position = Some(*spot);
        c.locked = false;
        new_ids.push(c.id.clone());
        s.project.objects.push(c);
    }
    s.changed("duplicate_object");
    let n = new_ids.len();
    let shown = if n > 6 { format!("{} … {}", new_ids[..3].join(", "), new_ids[n - 1]) } else { new_ids.join(", ") };
    let mut text = if i.fill { format!("Filled plate {} with {n} cop{} of {}: {shown}.", s.plate, if n == 1 { "y" } else { "ies" }, src.id) } else { format!("Added {n} cop{} next to {}: {shown}. Nothing else moved.", if n == 1 { "y" } else { "ies" }, src.id) };
    let missing = if i.fill { 0 } else { i.copies as usize - n };
    if missing > 0 {
        text.push_str(&format!(" {missing} more did not fit on the plate."));
    }
    Ok(ToolOutput::text(text).with_data(json!({"ids": new_ids, "not_placed": missing})))
}

fn t_import(s: &mut Studio, i: ImportIn) -> Result<ToolOutput, ToolError> {
    let full = only3d_model::shape::resolve_path(&s.evaluator.base_dir, &i.path);
    if !full.exists() {
        return Err(ToolError::new("not_found", format!("{} does not exist", full.display()), "paths are relative to the project file (or absolute)"));
    }
    let stem = full.file_stem().and_then(|x| x.to_str()).unwrap_or("part").to_string();
    let objs = s.evaluator.load_file(&full.display().to_string())?;
    // colours and filament slots from slicer projects (Bambu Studio / Orca) or 3MF materials
    let FileFilaments { slots: slot_of, note: mut colour_note, plates, warnings, map: slot_map } = import_filaments(s, &full, &objs);
    // multi-part objects keep their parts, one body each (as in the slicer), so each is checked
    // on its own and can have its own filament
    let split = |k: usize, main: Option<u32>| -> Option<(Vec<usize>, Vec<only3d_model::Part>)> {
        let o = objs.get(k)?;
        if o.parts.len() < 2 {
            return None;
        }
        let slot = |f: Option<u32>| f.and_then(|f| slot_map.get(f as usize - 1).copied()).or(main).unwrap_or(1);
        // the main body: the biggest part in the object's own filament (else the biggest)
        let size = |j: usize| o.parts[j].mesh.indices.len();
        let main_j = (0..o.parts.len()).filter(|j| Some(slot(o.parts[*j].filament)) == main).max_by_key(|j| size(*j)).or_else(|| (0..o.parts.len()).max_by_key(|j| size(*j)))?;
        let path = full.display().to_string();
        let parts = (0..o.parts.len())
            .filter(|j| *j != main_j)
            .map(|j| only3d_model::Part { name: o.parts[j].name.clone(), shape: only3d_model::Shape::Import { vars: Default::default(), path: path.clone(), object: None, index: Some(k), parts: Some(vec![j]), repair: i.repair }.into(), filament: slot(o.parts[j].filament) })
            .collect();
        Some((vec![main_j], parts))
    };
    // multi-plate projects: every plate comes in as a plate here (or just `plate`, onto the
    // active plate). Into an empty project the numbers are kept; otherwise they are appended.
    let n_plates = plates.iter().flatten().copied().max().unwrap_or(1);
    let fresh = s.project.objects.is_empty();
    let base = s.project.plate_count();
    let active = s.plate;
    let plate_for = |k: usize| -> Option<u32> {
        let fp = plates.get(k).copied().flatten().unwrap_or(1);
        match i.plate {
            Some(want) => (fp == want).then_some(active),
            None if n_plates <= 1 => Some(active),
            None if fresh => Some(fp),
            None => Some(base + fp),
        }
    };
    if n_plates > 1 {
        match i.plate {
            Some(want) => colour_note.push_str(&format!("\nThe file has {n_plates} plates; imported its plate {want} onto plate {active}.")),
            None if fresh => colour_note.push_str(&format!("\nThe file's {n_plates} plates are plates 1–{n_plates} here (see `plates`).")),
            None => colour_note.push_str(&format!("\nThe file's {n_plates} plates were added as plates {}–{} (see `plates`).", base + 1, base + n_plates)),
        }
    }
    if objs.len() > 1 && i.object.is_none() {
        // one only3d object per file object, keeping the file's layout when it fits the plate
        let mut ids = Vec::new();
        let mut touched: std::collections::BTreeSet<u32> = Default::default();
        for (k, o) in objs.iter().enumerate() {
            let Some(target) = plate_for(k) else { continue };
            let b = o.mesh.bounds();
            touched.insert(target);
            let id = s.project.unique_id(&format!("{stem}_{}", k + 1));
            let main = slot_of.get(k).copied().flatten();
            let (shape, parts) = match split(k, main) {
                Some((mp, parts)) => (only3d_model::Shape::Import { vars: Default::default(), path: full.display().to_string(), object: None, index: Some(k), parts: Some(mp), repair: i.repair }, parts),
                None => (only3d_model::Shape::Import { vars: Default::default(), path: full.display().to_string(), object: None, index: Some(k), parts: None, repair: i.repair }, Vec::new()),
            };
            let mut obj = Object::new(id.clone(), shape.into());
            obj.parts = parts;
            obj.name = if o.name.starts_with("object_") || o.name.is_empty() { id.clone() } else { o.name.clone() };
            obj.position = Some([(b.min.x + b.max.x) / 2.0, (b.min.y + b.max.y) / 2.0]);
            obj.plate = target;
            if let Some(f) = slot_of.get(k).copied().flatten() {
                obj.filament = f;
            }
            s.project.objects.push(obj);
            ids.push(id);
        }
        let top = touched.iter().copied().max().unwrap_or(1);
        s.project.ensure_plates(top);
        if !fresh && i.plate.is_none() && n_plates > 1 {
            for fp in 1..=n_plates {
                let n = (base + fp) as usize;
                if s.project.plates[n - 1].name.is_empty() {
                    s.project.plates[n - 1].name = format!("{stem} {fp}");
                }
            }
        }
        s.changed("import_file");
        // each plate's share keeps the file's layout as one block, clear of what was there
        let mut all_fit = true;
        let mut placed: Vec<String> = Vec::new();
        let mut landed: std::collections::BTreeSet<u32> = Default::default();
        for plate in touched.iter().copied() {
            let mine: Vec<String> = s.project.objects.iter().filter(|o| o.plate == plate && ids.contains(&o.id)).map(|o| o.id.clone()).collect();
            let st = settle_import(s, &mine, plate, i.position)?;
            all_fit &= !st.arranged;
            placed.extend(st.note);
            landed.insert(st.plate);
        }
        if let Some(first) = landed.first() {
            if !landed.contains(&s.plate) || landed.iter().any(|p| !touched.contains(p)) {
                s.set_plate(*landed.iter().find(|p| !touched.contains(p)).unwrap_or(first));
            }
        }
        if !placed.is_empty() {
            colour_note.push_str(&format!("\n{}", placed.join("\n")));
        }
        let objs = s.evaluated_all()?;
        let tris: usize = objs.iter().filter(|o| ids.contains(&o.id)).map(|o| o.report.triangles).sum();
        let leaky: Vec<&str> = objs.iter().filter(|o| ids.contains(&o.id) && !o.report.watertight).map(|o| o.id.as_str()).collect();
        if !leaky.is_empty() && !i.repair {
            colour_note.push_str(&format!("\nNot watertight: {} (import with \"repair\": true to close holes and turn faces outward).", leaky.join(", ")));
        }
        return Ok(ToolOutput::text(format!("Imported {} objects from {} ({} triangles){}: {}{colour_note}", ids.len(), full.display(), tris, if all_fit { ", layout kept" } else { ", arranged" }, ids.join(", "))).with_data(json!({"ids": ids, "warnings": warnings, "placed": placed.join(" ")})));
    }
    let filament = slot_of.first().copied().flatten();
    let (node, parts): (Node, Option<Vec<only3d_model::Part>>) = match (objs.len(), &i.object, split(0, filament)) {
        (1, None, Some((mp, parts))) => (only3d_model::Shape::Import { vars: Default::default(), path: full.display().to_string(), object: None, index: Some(0), parts: Some(mp), repair: i.repair }.into(), Some(parts)),
        _ => (only3d_model::Shape::Import { vars: Default::default(), path: full.display().to_string(), object: i.object.clone(), index: None, parts: None, repair: i.repair }.into(), None),
    };
    let mut out = t_add_object(s, AddObjectIn { id: i.id, name: Some(stem), shape: node, position: i.position, rotation: None, filament, parts, render: false })?;
    if let (Some(at), Some(id)) = (i.position, out.data.get("id").and_then(|v| v.as_str()).map(String::from)) {
        let st = settle_import(s, std::slice::from_ref(&id), s.plate, Some(at))?;
        if let Some(n) = st.note {
            if st.plate != s.plate {
                s.set_plate(st.plate);
            }
            out.text.push_str(&format!("\n{n}"));
            if let Some(o) = out.data.as_object_mut() {
                o.insert("placed".into(), json!(n));
            }
        }
    }
    out.text.push_str(&colour_note);
    if only3d_model::scad::is_scad(&full) {
        if let Ok(vars) = s.evaluator.scad_vars(&full.display().to_string()) {
            if !vars.is_empty() {
                out.text.push_str(&format!("\nOpenSCAD variables (set_params with \"object\"): {}", scad_vars_line(&vars, &BTreeMap::new())));
                if let Some(o) = out.data.as_object_mut() {
                    o.insert("scad_vars".into(), json!(*vars));
                }
            }
        }
    }
    if !warnings.is_empty() {
        if let Some(o) = out.data.as_object_mut() {
            o.insert("warnings".into(), json!(warnings));
        }
    }
    Ok(out)
}

/// Bring a model file's filament slots (colour + preset) into the project. Returns the project
/// slot for each file object, a note for the tool output, each object's plate, and warnings
/// about what the import could not keep (shown to the user by the window).
struct FileFilaments {
    slots: Vec<Option<u32>>,
    note: String,
    plates: Vec<Option<u32>>,
    warnings: Vec<String>,
    /// File filament slot (1-based, index + 1) → project slot.
    map: Vec<u32>,
}

fn import_filaments(s: &mut Studio, path: &std::path::Path, objs: &[only3d_geom::io::ImportedObject]) -> FileFilaments {
    let is_3mf = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("3mf"));
    let meta = if is_3mf { std::fs::read(path).ok().and_then(|b| only3d_geom::io::threemf::read_with_meta(&b).ok()).map(|(_, m)| m) } else { None };
    // file slots: from the project settings, else one per distinct material colour
    type Slots = (Vec<(String, Option<String>)>, Vec<Option<u32>>);
    let (file_slots, obj_slot): Slots = match &meta {
        Some(m) if !m.filaments.is_empty() => (m.filaments.iter().map(|f| (f.color.clone(), f.preset.clone())).collect(), m.object_filaments.clone()),
        _ => {
            let mut colours: Vec<String> = Vec::new();
            let slots = objs
                .iter()
                .map(|o| {
                    o.color.map(|c| {
                        let hex = format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]);
                        let k = colours.iter().position(|x| *x == hex).unwrap_or_else(|| {
                            colours.push(hex.clone());
                            colours.len() - 1
                        });
                        k as u32 + 1
                    })
                })
                .collect();
            (colours.into_iter().map(|c| (c, None)).collect(), slots)
        }
    };
    let plates = meta.as_ref().map(|m| m.object_plates.clone()).unwrap_or_default();
    if file_slots.is_empty() {
        return FileFilaments { slots: vec![None; objs.len()], note: String::new(), plates, warnings: Vec::new(), map: Vec::new() };
    }
    let mut warnings: Vec<String> = Vec::new();
    let known = |s: &Studio, p: &str| s.machine.list_presets("filament", p).iter().any(|n| n == p);
    let fresh = s.project.objects.is_empty();
    let mut map: Vec<u32> = Vec::new();
    let mut unknown_presets = Vec::new();
    for (k, (colour, preset)) in file_slots.iter().enumerate() {
        let preset_ok = preset.as_ref().filter(|p| known(s, p)).cloned();
        if preset.is_some() && preset_ok.is_none() {
            unknown_presets.push(preset.clone().unwrap_or_default());
        }
        let target = if fresh && k < s.project.filaments.len() {
            let f = &mut s.project.filaments[k];
            f.color = colour.clone();
            if let Some(p) = &preset_ok {
                f.preset = p.clone();
            }
            f.spool = None;
            k as u32 + 1
        } else if let Some(j) = s.project.filaments.iter().position(|f| f.color.eq_ignore_ascii_case(colour) && preset_ok.as_ref().is_none_or(|p| *p == f.preset)) {
            j as u32 + 1
        } else {
            let base = s.project.filaments.first().map(|f| f.preset.clone()).unwrap_or_else(|| "Bambu PLA Basic @BBL H2D".into());
            s.project.filaments.push(FilamentSlot { preset: preset_ok.clone().unwrap_or(base), color: colour.clone(), nozzle: NozzleSel::Auto, spool: None });
            s.project.filaments.len() as u32
        };
        map.push(target);
    }
    let slots: Vec<Option<u32>> = obj_slot.iter().map(|o| o.and_then(|k| map.get(k as usize - 1).copied())).collect();
    let used: std::collections::BTreeSet<u32> = slots.iter().flatten().copied().collect();
    let mut note = format!(
        "\nFilaments from the file: {}.",
        used.iter().map(|k| { let f = &s.project.filaments[*k as usize - 1]; format!("slot {k} {} {}", f.color, f.preset) }).collect::<Vec<_>>().join(", ")
    );
    if !unknown_presets.is_empty() {
        note.push_str(&format!(" Presets not found here (kept the slot's preset; import them with preset_import): {}.", unknown_presets.join(", ")));
    }
    if meta.as_ref().is_some_and(|m| m.painted) {
        warnings.push("The file has colour painting. only3d can't hold painted faces yet, so each object comes in as one filament. To print in colour, open the file in Bambu Studio.".into());
    }
    if let Some(m) = &meta {
        let multi = m.object_extra_filaments.iter().filter(|x| !x.is_empty()).count();
        if multi > 0 {
            note.push_str(&format!(" {multi} multi-colour object(s) keep their parts, each in its own filament."));
        }
    }
    for w in &warnings {
        note.push(' ');
        note.push_str(w);
    }
    FileFilaments { slots, note, plates, warnings, map }
}

fn need_plate(s: &Studio, plate: u32) -> Result<(), ToolError> {
    let n = s.project.plate_count();
    if plate == 0 || plate > n {
        return Err(ToolError::new("no_such_plate", format!("there is no plate {plate} (the project has {n})"), "list them with `plates`, add one with plate {\"action\": \"add\"}"));
    }
    Ok(())
}

fn plates_report(s: &Studio) -> (String, Value) {
    let p = &s.project;
    let mut lines = Vec::new();
    let mut rows = Vec::new();
    for n in 1..=p.plate_count() {
        let ids: Vec<&str> = p.objects.iter().filter(|o| o.plate == n).map(|o| o.id.as_str()).collect();
        let fil: std::collections::BTreeSet<u32> = p.objects.iter().filter(|o| o.plate == n && !o.hidden).flat_map(|o| std::iter::once(o.filament).chain(o.parts.iter().map(|x| x.filament))).collect();
        lines.push(format!(
            "{}{n}. {}: {} object{}{}{}",
            if n == s.plate { "▸ " } else { "  " },
            p.plate_name(n),
            ids.len(),
            if ids.len() == 1 { "" } else { "s" },
            if ids.is_empty() { String::new() } else { format!(" ({})", ids.join(", ")) },
            if fil.len() > 1 { format!(", {} filaments", fil.len()) } else { String::new() }
        ));
        if let Some(i) = p.queue.iter().position(|k| *k == n) {
            if let Some(l) = lines.last_mut() {
                l.push_str(&format!(" · queued #{}", i + 1));
            }
        }
        rows.push(json!({"plate": n, "name": p.plate_name(n), "objects": ids, "active": n == s.plate, "filaments": fil, "queued": p.queue.iter().position(|k| *k == n).map(|i| i + 1)}));
    }
    (format!("{} plate(s), working on plate {}:\n{}", p.plate_count(), s.plate, lines.join("\n")), json!({"active": s.plate, "plates": rows}))
}

fn t_plates(s: &mut Studio, _: Empty) -> Result<ToolOutput, ToolError> {
    let (t, d) = plates_report(s);
    Ok(ToolOutput::text(t).with_data(d))
}

#[derive(Deserialize, JsonSchema)]
pub struct PlateIn {
    /// add | select | rename | duplicate | remove (objects go to `to`, else deleted) | move
    /// (`ids` onto `plate`) | queue | unqueue
    pub action: String,
    /// 1-based. Default: the active plate.
    #[serde(default)]
    pub plate: Option<u32>,
    /// add, rename
    #[serde(default)]
    pub name: Option<String>,
    /// move
    #[serde(default)]
    pub ids: Option<Vec<String>>,
    /// remove
    #[serde(default)]
    pub to: Option<u32>,
}

fn t_plate(s: &mut Studio, i: PlateIn) -> Result<ToolOutput, ToolError> {
    let target = i.plate.unwrap_or(s.plate);
    let text = match i.action.as_str() {
        "add" => {
            let n = s.project.plate_count() + 1;
            s.project.ensure_plates(n);
            s.project.plates[n as usize - 1].name = i.name.clone().unwrap_or_default();
            s.changed("plate");
            s.set_plate(n);
            format!("Added {} (plate {n}); it's the active plate.", s.project.plate_name(n))
        }
        "select" => {
            need_plate(s, target)?;
            s.set_plate(target);
            format!("Working on {} (plate {target}).", s.project.plate_name(target))
        }
        "rename" => {
            need_plate(s, target)?;
            let name = i.name.clone().filter(|n| !n.trim().is_empty()).ok_or_else(|| ToolError::new("missing_name", "rename needs a name", "pass name"))?;
            s.project.ensure_plates(target);
            s.project.plates[target as usize - 1].name = name.trim().to_string();
            s.changed("plate");
            format!("Plate {target} is now '{}'.", name.trim())
        }
        "duplicate" => {
            need_plate(s, target)?;
            let n = s.project.plate_count() + 1;
            s.project.ensure_plates(n);
            let src_name = s.project.plate_name(target);
            s.project.plates[n as usize - 1].name = i.name.clone().unwrap_or_else(|| format!("{src_name} copy"));
            let copies: Vec<Object> = s.project.objects.iter().filter(|o| o.plate == target).cloned().collect();
            let mut ids = Vec::new();
            for mut c in copies {
                let named_by_id = c.name.is_empty() || c.name == c.id;
                c.id = s.project.unique_id(&c.id);
                if named_by_id {
                    c.name = c.id.clone();
                }
                c.plate = n;
                ids.push(c.id.clone());
                s.project.objects.push(c);
            }
            s.changed("plate");
            s.set_plate(n);
            format!("Duplicated {src_name} as plate {n} ({} object(s): {}); it's the active plate.", ids.len(), ids.join(", "))
        }
        "remove" => {
            need_plate(s, target)?;
            if s.project.plate_count() == 1 {
                return Err(ToolError::new("last_plate", "a project always has one plate", "remove its objects instead"));
            }
            if let Some(to) = i.to {
                need_plate(s, to)?;
                if to == target {
                    return Err(ToolError::new("bad_target", "`to` is the plate being removed", "pick another plate"));
                }
            }
            let name = s.project.plate_name(target);
            let moved: Vec<String> = s.project.objects.iter().filter(|o| o.plate == target).map(|o| o.id.clone()).collect();
            match i.to {
                Some(to) => s.project.objects.iter_mut().filter(|o| o.plate == target).for_each(|o| o.plate = to),
                None => s.project.objects.retain(|o| o.plate != target),
            }
            s.selection.retain(|id| !moved.contains(id) || i.to.is_some());
            // later plates move up one
            s.project.ensure_plates(s.project.plate_count());
            s.project.plates.remove(target as usize - 1);
            s.project.queue.retain(|k| *k != target);
            for k in s.project.queue.iter_mut().filter(|k| **k > target) {
                *k -= 1;
            }
            for o in s.project.objects.iter_mut().filter(|o| o.plate > target) {
                o.plate -= 1;
            }
            s.changed("plate");
            let now = if s.plate > target { s.plate - 1 } else { s.plate.min(s.project.plate_count()) };
            s.plate = 0; // force the change event
            s.set_plate(now.max(1));
            match i.to {
                Some(to) => format!("Removed {name}; its {} object(s) moved to plate {}.", moved.len(), if to > target { to - 1 } else { to }),
                None => format!("Removed {name} and its {} object(s).", moved.len()),
            }
        }
        "queue" => {
            need_plate(s, target)?;
            if !s.project.queue.contains(&target) {
                s.project.queue.push(target);
                s.changed("plate");
            }
            let q: Vec<String> = s.project.queue.iter().map(|k| s.project.plate_name(*k)).collect();
            format!("Queued {}. Print order: {}. When a print finishes, only3d says which plate is next; you start each one.", s.project.plate_name(target), q.join(" → "))
        }
        "unqueue" => {
            s.project.queue.retain(|k| *k != target);
            s.changed("plate");
            format!("{} is no longer queued.", s.project.plate_name(target))
        }
        "move" => {
            need_plate(s, target)?;
            let ids = i.ids.clone().filter(|v| !v.is_empty()).ok_or_else(|| ToolError::new("missing_ids", "move needs ids", "pass the object ids to move"))?;
            for id in &ids {
                need_object(s, id)?;
            }
            s.project.objects.iter_mut().filter(|o| ids.contains(&o.id)).for_each(|o| o.plate = target);
            s.selection.retain(|id| !ids.contains(id) || target == s.plate);
            s.changed("plate");
            format!("Moved {} to {} (plate {target}). They keep their positions; `arrange` that plate if they overlap.", ids.join(", "), s.project.plate_name(target))
        }
        other => return Err(ToolError::new("bad_action", format!("unknown plate action '{other}'"), "add | select | rename | duplicate | remove | move | queue | unqueue")),
    };
    let (t, d) = plates_report(s);
    Ok(ToolOutput::text(format!("{text}\n{t}")).with_data(d))
}

fn t_arrange(s: &mut Studio, i: crate::arrange::ArrangeIn) -> Result<ToolOutput, ToolError> {
    if i.op.as_deref().is_some_and(|o| o != "pack") {
        return crate::arrange::run(s, i);
    }
    let mut ev = std::mem::replace(&mut s.evaluator, only3d_model::Evaluator::new("."));
    let r = s.project.arrange(&mut ev, &s.bed, i.gap.or(i.spacing).unwrap_or(6.0), s.plate);
    s.evaluator = ev;
    let failed = r?;
    s.changed("arrange");
    let objs = s.evaluated()?;
    let mut text = format!("Arranged {} object(s).", objs.len());
    if !failed.is_empty() {
        text.push_str(&format!(" Did not fit on the plate: {} (consider a second plate or scaling).", failed.join(", ")));
    }
    Ok(ToolOutput::text(text).with_data(json!({"not_placed": failed, "positions": s.project.objects.iter().map(|o| json!({"id": o.id, "position": o.position})).collect::<Vec<_>>()})))
}

fn t_orient(s: &mut Studio, i: OrientIn) -> Result<ToolOutput, ToolError> {
    let ids: Vec<String> = i.ids.unwrap_or_else(|| s.project.objects.iter().map(|o| o.id.clone()).collect());
    let mut lines = Vec::new();
    let mut data = Vec::new();
    for id in ids {
        let o = need_object(s, &id)?.clone();
        let local = s.evaluator.eval(&o.shape, &s.project.params)?;
        let scaled = local.transformed(&glam::Mat4::from_scale(glam::Vec3::from(o.scale)));
        let (before, _) = {
            let m = scaled.transformed(&glam::Mat4::from_quat(only3d_model::shape::euler_xyz(glam::Vec3::from(o.rotation))));
            ((m.overhang_area(45.0), m.bed_contact_area()), ())
        };
        let (best, _) = only3d_model::best_orientation(&scaled, [0.0; 3]);
        let o2 = need_object(s, &id)?;
        o2.rotation = best.rotation;
        lines.push(format!(
            "'{id}': rotation {:?} → overhang {:.0} mm² (was {:.0}), bed contact {:.0} mm² (was {:.0})",
            best.rotation, best.overhang_mm2, before.0, best.contact_mm2, before.1
        ));
        data.push(json!({"id": id, "rotation": best.rotation, "overhang_mm2": best.overhang_mm2, "contact_mm2": best.contact_mm2}));
    }
    Ok(ToolOutput::text(lines.join("\n")).with_data(Value::Array(data)))
}

fn t_select(s: &mut Studio, i: IdsIn) -> Result<ToolOutput, ToolError> {
    for id in &i.ids {
        if s.project.object(id).is_none() {
            return Err(ToolError::new("not_found", format!("no object '{id}'"), "call `describe`"));
        }
    }
    // selecting objects on another plate goes to that plate (all must share one)
    let plates: std::collections::BTreeSet<u32> = i.ids.iter().filter_map(|id| s.project.object(id)).map(|o| o.plate).collect();
    if plates.len() > 1 {
        return Err(ToolError::new("several_plates", "those objects are on different plates", "select objects from one plate at a time (see `plates`)"));
    }
    if let Some(p) = plates.first() {
        s.set_plate(*p);
    }
    s.set_selection(i.ids.clone());
    Ok(ToolOutput::text(format!("Selected: {}", if i.ids.is_empty() { "nothing".into() } else { i.ids.join(", ") })))
}

fn t_get_selection(s: &mut Studio, _: Empty) -> Result<ToolOutput, ToolError> {
    let sel = s.selection.clone();
    if sel.is_empty() {
        return Ok(ToolOutput::text("Nothing is selected.").with_data(json!({"ids": []})));
    }
    let objs = s.evaluated()?;
    let lines: Vec<String> = objs.iter().filter(|o| sel.contains(&o.id)).map(obj_line).collect();
    let shapes: Vec<Value> = s.project.objects.iter().filter(|o| sel.contains(&o.id)).map(|o| serde_json::to_value(o).unwrap_or(Value::Null)).collect();
    Ok(ToolOutput::text(format!("Selected (by the user):\n{}", lines.join("\n"))).with_data(json!({"ids": sel, "objects": shapes})))
}

#[derive(Deserialize, JsonSchema, Default)]
pub struct CheckIn {
    /// Save a design check with the project.
    #[serde(default)]
    pub add: Option<only3d_model::Spec>,
    /// A saved check's id.
    #[serde(default)]
    pub remove: Option<String>,
}

fn t_check(s: &mut Studio, i: CheckIn) -> Result<ToolOutput, ToolError> {
    let mut head = String::new();
    if let Some(id) = &i.remove {
        let before = s.project.specs.len();
        s.project.specs.retain(|x| &x.id != id);
        if s.project.specs.len() == before {
            return Err(ToolError::new("not_found", format!("no saved check '{id}'"), "run check to see the saved checks and their ids"));
        }
        s.changed("check");
        head.push_str(&format!("Removed check {id}.\n"));
    }
    if let Some(mut sp) = i.add {
        // the check must make sense now (objects exist, kind known), even if it fails
        let objs = s.evaluated_all()?;
        if let Err(e) = crate::specs::evaluate(&sp, &objs) {
            return Err(ToolError::new("bad_check", e, "kinds: size {object, axis, min/max}, clearance {object, other, min}, wall {object, z, min}, overhang {object, max}"));
        }
        if sp.min.is_none() && sp.max.is_none() {
            return Err(ToolError::new("no_limit", "a check needs min and/or max", "e.g. {\"kind\": \"wall\", \"object\": \"cup\", \"min\": 1.6}"));
        }
        let n = (1..).find(|k| !s.project.specs.iter().any(|x| x.id == format!("c{k}"))).unwrap_or(1);
        sp.id = format!("c{n}");
        head.push_str(&format!("Saved check {}.\n", sp.id));
        s.project.specs.push(sp);
        s.changed("check");
    }
    let f = s.findings()?;
    let text = if f.is_empty() {
        "All checks passed.".to_string()
    } else {
        f.iter().map(|x| format!("{:?} {}{}: {} → {}", x.severity, x.code, x.object.as_ref().map(|o| format!(" [{o}]")).unwrap_or_default(), x.message, x.hint)).collect::<Vec<_>>().join("\n")
    };
    Ok(ToolOutput::text(format!("{head}{text}")).with_data(serde_json::to_value(&f).unwrap_or(Value::Null)))
}

fn t_measure(s: &mut Studio, i: IdIn) -> Result<ToolOutput, ToolError> {
    focus_object(s, &i.id);
    let objs = s.evaluated()?;
    let o = objs.iter().find(|o| o.id == i.id).ok_or_else(|| ToolError::new("not_found", format!("no object '{}'", i.id), "call `describe`"))?;
    let r = &o.report;
    let overhang = o.mesh.overhang_area(45.0);
    let contact = o.mesh.bed_contact_area();
    let c = o.mesh.centroid();
    let text = format!(
        "{}\narea {:.0} mm², overhang (>45°) {:.0} mm², bed contact {:.0} mm², centroid ({:.1}, {:.1}, {:.1}), {} body(ies), mass if solid PLA {:.1} g",
        obj_line(o),
        r.area_mm2,
        overhang,
        contact,
        c.x,
        c.y,
        c.z,
        r.components,
        r.volume_mm3 / 1000.0 * 1.24
    );
    Ok(ToolOutput::text(text).with_data(json!({"report": r, "overhang_mm2": overhang, "bed_contact_mm2": contact, "centroid": c})))
}

fn t_section(s: &mut Studio, i: SectionIn) -> Result<ToolOutput, ToolError> {
    use only3d_slice::poly;
    if let Some(id) = &i.id {
        focus_object(s, id);
    }
    let objs = s.evaluated()?;
    let sel: Vec<_> = objs.iter().filter(|o| !o.hidden && i.id.as_ref().is_none_or(|id| &o.id == id)).collect();
    if sel.is_empty() {
        return Err(ToolError::new("not_found", "no matching object", "call `describe`"));
    }
    let mut region: poly::Region = Vec::new();
    for o in &sel {
        let (r, _) = only3d_slice::slicer::slice_mesh(&o.mesh, &[i.z]);
        region = poly::union(&region, &r[0]);
    }
    let area = poly::region_area(&region);
    let holes: usize = region.iter().map(|sh| sh.len().saturating_sub(1)).sum();
    // material thinner than two 0.42 mm walls disappears under a morphological opening
    let w = 0.42 * 2.0;
    let opened = poly::offset(&poly::offset(&region, -w / 2.0), w / 2.0);
    let thin = (area - poly::region_area(&opened)).max(0.0);
    let mut text = format!("Section at z={:.2}: {} island(s), {} hole(s), area {:.1} mm².", i.z, region.len(), holes, area);
    if thin > 0.5 {
        text.push_str(&format!(" {:.1} mm² is thinner than {:.2} mm (two walls) and may print poorly.", thin, w));
    }
    let contours: Vec<Value> = region
        .iter()
        .map(|sh| {
            let b = sh[0].iter().fold([f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY], |b, p| [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])]);
            json!({"bounds": b, "area": poly::contour_area(&sh[0]), "holes": sh.len() - 1})
        })
        .collect();
    Ok(ToolOutput::text(text).with_data(json!({"z": i.z, "area_mm2": area, "islands": contours, "thin_area_mm2": thin})))
}

/// Minimum horizontal gap between two parts at several heights (negative = they overlap by
/// that much). Uses polygon offsets of the cross-sections, accurate to ~0.005 mm.
fn t_clearance(s: &mut Studio, i: ClearanceIn) -> Result<ToolOutput, ToolError> {
    use only3d_slice::poly;
    focus_object(s, &i.a);
    let objs = s.evaluated()?;
    let get = |id: &str| objs.iter().find(|o| o.id == id).cloned().ok_or_else(|| ToolError::new("not_found", format!("no object '{id}'"), "call `describe`"));
    let (mut a, mut b) = (get(&i.a)?, get(&i.b)?);
    if let Some(d) = i.a_shift {
        let mut m = (*a.mesh).clone();
        m.translate(glam::Vec3::from(d));
        a.report.bounds = m.bounds();
        a.mesh = std::sync::Arc::new(m);
    }
    if let Some(d) = i.b_shift {
        let mut m = (*b.mesh).clone();
        m.translate(glam::Vec3::from(d));
        b.report.bounds = m.bounds();
        b.mesh = std::sync::Arc::new(m);
    }
    let (ba, bb) = (a.report.bounds, b.report.bounds);
    let (z0, z1) = (ba.min.z.max(bb.min.z) as f64, ba.max.z.min(bb.max.z) as f64);
    if z1 <= z0 {
        return Err(ToolError::new("no_common_height", format!("'{}' and '{}' do not share any height range", i.a, i.b), "position them assembled first"));
    }
    let zs: Vec<f64> = i.z.unwrap_or_else(|| (0..9).map(|k| z0 + (z1 - z0) * (k as f64 + 0.5) / 9.0).collect());
    let mut rows = Vec::new();
    let mut worst = f64::INFINITY;
    for z in zs {
        let (ra, _) = only3d_slice::slicer::slice_mesh(&a.mesh, &[z]);
        let (rb, _) = only3d_slice::slicer::slice_mesh(&b.mesh, &[z]);
        let (ra, rb) = (&ra[0], &rb[0]);
        if ra.is_empty() || rb.is_empty() {
            continue;
        }
        let overlap = poly::region_area(&poly::intersection(ra, rb));
        let gap = if overlap > 1e-6 {
            // interference: how far must A shrink before it stops touching B?
            let (mut lo, mut hi) = (0.0f64, 5.0f64);
            for _ in 0..20 {
                let mid = (lo + hi) / 2.0;
                if poly::region_area(&poly::intersection(&poly::offset(ra, -mid), rb)) > 1e-6 { lo = mid } else { hi = mid }
            }
            -hi
        } else {
            let (mut lo, mut hi) = (0.0f64, 20.0f64);
            if poly::region_area(&poly::intersection(&poly::offset(ra, hi), rb)) <= 1e-6 {
                f64::INFINITY
            } else {
                for _ in 0..20 {
                    let mid = (lo + hi) / 2.0;
                    if poly::region_area(&poly::intersection(&poly::offset(ra, mid), rb)) > 1e-6 { hi = mid } else { lo = mid }
                }
                hi
            }
        };
        worst = worst.min(gap);
        rows.push(json!({"z": (z * 100.0).round() / 100.0, "gap_mm": if gap.is_finite() { json!((gap * 1000.0).round() / 1000.0) } else { json!(null) }}));
    }
    let text = if !worst.is_finite() {
        format!("'{}' and '{}' are more than 20 mm apart at every checked height.", i.a, i.b)
    } else if worst < 0.0 {
        format!("'{}' and '{}' INTERFERE by up to {:.3} mm. Per height: {}", i.a, i.b, -worst, serde_json::to_string(&rows).unwrap_or_default())
    } else {
        format!("Minimum gap between '{}' and '{}': {:.3} mm (FDM fits usually need 0.15–0.3 mm per side). Per height: {}", i.a, i.b, worst, serde_json::to_string(&rows).unwrap_or_default())
    };
    Ok(ToolOutput::text(text).with_data(json!({"min_gap_mm": if worst.is_finite() { json!(worst) } else { json!(null) }, "heights": rows})))
}

fn t_render(s: &mut Studio, i: RenderRequest) -> Result<ToolOutput, ToolError> {
    let png = render::render(s, &i)?;
    let caption = format!("{} view{}", serde_json::to_value(i.view).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_else(|| "custom".into()), if i.toolpaths { " (toolpaths)" } else { "" });
    Ok(ToolOutput::text(format!("Rendered {caption}, {}x{} px.", i.width, i.height)).with_image(png, caption))
}

fn t_settings_schema(s: &mut Studio, _: Empty) -> Result<ToolOutput, ToolError> {
    let objs = s.evaluated()?;
    let visible: Vec<_> = objs.iter().filter(|o| !o.hidden).cloned().collect();
    let effective = s.machine.setup(&s.project, &visible).map(|m| (m.settings, m.source));
    let schema = schema_of::<only3d_slice::SliceSettings>();
    let (eff, src) = match effective {
        Ok((st, src)) => (serde_json::to_value(st).unwrap_or(Value::Null), src),
        Err(e) => (json!({"error": e}), String::new()),
    };
    Ok(ToolOutput::text(format!(
        "Process override keys (set with set_process overrides or per object settings). Effective values from {src}:\n{}",
        serde_json::to_string(&eff).unwrap_or_default()
    ))
    .with_data(json!({"schema": schema, "effective": eff})))
}

fn t_settings_catalog(s: &mut Studio, _: Empty) -> Result<ToolOutput, ToolError> {
    let objs = s.evaluated()?;
    let visible: Vec<_> = objs.iter().filter(|o| !o.hidden).cloned().collect();
    let (values, source) = match s.machine.setup(&s.project, &visible) {
        Ok(m) => (serde_json::to_value(&m.settings).unwrap_or(Value::Null), m.source),
        Err(e) => (json!({"error": e}), String::new()),
    };
    let groups: Vec<Value> = only3d_slice::settings::GROUPS.iter().map(|(id, label)| json!({"id": id, "label": label})).collect();
    let settings = serde_json::to_value(only3d_slice::settings::catalog()).unwrap_or(Value::Null);
    let n = settings.as_array().map(|a| a.len()).unwrap_or(0);
    Ok(ToolOutput::text(format!(
        "{n} settings in {} groups. Effective values come from {source}; project overrides: {}. Change them with set_process {{\"overrides\": {{key: value}}}} (null resets) or per object with update_object settings.",
        groups.len(),
        serde_json::to_string(&s.project.process.overrides).unwrap_or_default()
    ))
    .with_data(json!({"groups": groups, "settings": settings, "values": values, "overrides": s.project.process.overrides, "source": source})))
}

fn t_set_printer(s: &mut Studio, i: SetPrinterIn) -> Result<ToolOutput, ToolError> {
    if let Some(m) = i.machine {
        let known = s.machine.list_presets("machine", &m);
        if !known.is_empty() && !known.contains(&m) {
            return Err(ToolError::new("unknown_preset", format!("no machine preset '{m}'"), format!("did you mean: {}", known.iter().take(5).cloned().collect::<Vec<_>>().join(", "))));
        }
        s.project.printer.machine = m;
    }
    if let Some(b) = i.bed_type {
        const OK: [&str; 6] = ["Textured PEI Plate", "Cool Plate", "Engineering Plate", "High Temp Plate", "Smooth PEI Plate", "Cool Plate (SuperTack)"];
        if !OK.contains(&b.as_str()) {
            return Err(ToolError::new("bad_bed_type", format!("unknown bed type '{b}'"), format!("one of: {}", OK.join(", "))));
        }
        s.project.printer.bed_type = b;
    }
    Ok(ToolOutput::text(format!("Printer: {} on {}", s.project.printer.machine, s.project.printer.bed_type)))
}

fn t_set_filament(s: &mut Studio, i: SetFilamentIn) -> Result<ToolOutput, ToolError> {
    let n = s.project.filaments.len() as u32;
    if i.slot == 0 || i.slot > n + 1 {
        return Err(ToolError::new("bad_slot", format!("slot {} out of range (have {n})", i.slot), format!("use 1..{} (n+1 adds a slot)", n + 1)));
    }
    if i.slot == n + 1 {
        s.project.filaments.push(FilamentSlot { preset: i.preset.clone().unwrap_or_else(|| "Bambu PLA Basic @BBL H2D".into()), color: "#D9CBB5".into(), nozzle: NozzleSel::Auto, spool: None });
    }
    if let Some(p) = &i.preset {
        let known = s.machine.list_presets("filament", p);
        if !known.is_empty() && !known.contains(p) {
            return Err(ToolError::new("unknown_preset", format!("no filament preset '{p}'"), format!("did you mean: {}", known.iter().take(6).cloned().collect::<Vec<_>>().join(", "))));
        }
    }
    let spool = match i.spool.as_deref() {
        Some("") | None => None,
        Some(id) => Some(crate::spools::Inventory::load().get(id).cloned().ok_or_else(|| ToolError::new("not_found", format!("no spool '{id}'"), "see spool_list"))?),
    };
    let f = &mut s.project.filaments[i.slot as usize - 1];
    if let Some(sp) = &spool {
        f.spool = Some(sp.id.clone());
        f.color = sp.color.clone();
        if let Some(p) = &sp.preset {
            f.preset = p.clone();
        }
    } else if i.spool.as_deref() == Some("") {
        f.spool = None;
    }
    if let Some(p) = i.preset {
        f.preset = p;
    }
    if let Some(c) = i.color {
        if !(c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|x| x.is_ascii_hexdigit())) {
            return Err(ToolError::new("bad_color", format!("'{c}' is not #RRGGBB"), "e.g. #3F8E43"));
        }
        f.color = c;
    }
    if let Some(nz) = i.nozzle {
        f.nozzle = nz;
    }
    Ok(ToolOutput::text(format!(
        "Filament {}: {} {} ({:?} nozzle){}",
        i.slot,
        f.preset,
        f.color,
        f.nozzle,
        spool.map(|sp| format!(", spool {} with {:.0} g left{}", sp.id, sp.remaining_g, sp.location.map(|l| format!(" in {l}")).unwrap_or_default())).unwrap_or_default()
    )))
}

fn t_set_process(s: &mut Studio, i: SetProcessIn) -> Result<ToolOutput, ToolError> {
    if let Some(p) = i.preset {
        let known = s.machine.list_presets("process", &p);
        if !known.is_empty() && !known.contains(&p) {
            return Err(ToolError::new("unknown_preset", format!("no process preset '{p}'"), format!("did you mean: {}", known.iter().take(6).cloned().collect::<Vec<_>>().join(", "))));
        }
        s.project.process.preset = p;
    }
    if i.clear {
        s.project.process.overrides.clear();
    }
    let valid = schema_of::<only3d_slice::SliceSettings>();
    let props = valid.get("properties").and_then(|p| p.as_object()).cloned().unwrap_or_default();
    for (k, v) in i.overrides {
        if !props.contains_key(&k) {
            let mut close: Vec<&String> = props.keys().filter(|p| p.contains(k.split('_').next().unwrap_or(&k))).collect();
            close.truncate(6);
            return Err(ToolError::new("unknown_setting", format!("unknown setting '{k}'"), format!("similar: {}", close.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "))));
        }
        if v.is_null() {
            s.project.process.overrides.remove(&k);
        } else {
            s.project.process.overrides.insert(k, v);
        }
    }
    // validate by building settings once
    let objs = s.evaluated()?;
    let visible: Vec<_> = objs.iter().filter(|o| !o.hidden).cloned().collect();
    s.machine.setup(&s.project, &visible)?;
    Ok(ToolOutput::text(format!("Process: {} with overrides {}", s.project.process.preset, serde_json::to_string(&s.project.process.overrides).unwrap_or_default())))
}

fn t_list_presets(s: &mut Studio, i: ListPresetsIn) -> Result<ToolOutput, ToolError> {
    if !["machine", "process", "filament"].contains(&i.kind.as_str()) {
        return Err(ToolError::new("bad_kind", format!("unknown kind '{}'", i.kind), "machine | process | filament"));
    }
    let list = s.machine.list_presets(&i.kind, i.query.as_deref().unwrap_or(""));
    Ok(ToolOutput::text(format!("{} {} preset(s):\n{}", list.len(), i.kind, list.iter().take(80).cloned().collect::<Vec<_>>().join("\n"))).with_data(json!(list)))
}

fn t_slice(s: &mut Studio, i: SliceIn) -> Result<ToolOutput, ToolError> {
    if let Some(p) = i.plate {
        need_plate(s, p)?;
        s.set_plate(p);
    }
    let o = run_slice(s)?;
    let (mut text, mut data) = slice_summary(&o);
    // what finished prints taught about the real time on the printer
    if let Some(c) = crate::printer::load_book().default_name().and_then(|p| crate::timing::correction(&p)) {
        let exp = c.expected(o.result.stats.print_time_s);
        text.push_str(&format!(
            "\nOn the printer ≈ {}: {} start sequence + printing × {:.2} (learned from {} print{}{}).",
            crate::timing::fmt_dur(exp),
            crate::timing::fmt_dur(c.overhead_s),
            c.ratio,
            c.prints,
            if c.prints == 1 { "" } else { "s" },
            if c.prints < 3 { ", still rough" } else { "" }
        ));
        data["expected_on_printer_s"] = json!(exp);
    }
    let mut out = ToolOutput::text(text).with_data(data);
    if i.preview {
        let png = render::render(s, &RenderRequest { toolpaths: true, ..Default::default() })?;
        out = out.with_image(png, "toolpath preview");
    }
    Ok(out)
}

fn t_export(s: &mut Studio, i: ExportIn) -> Result<ToolOutput, ToolError> {
    let path = expand(&i.path);
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| ToolError::new("io_error", format!("cannot create {}: {e}", dir.display()), "pick a folder you can write to"))?;
    }
    if path.to_string_lossy().to_ascii_lowercase().ends_with(".zip") {
        return export_zip(s, &path);
    }
    match i.each.as_deref() {
        None => export_one(s, &path, i.ids.as_deref(), i.all_plates, &Fmt::of(&i)),
        Some(each @ ("object" | "plate")) => export_each(s, &path, &i, each == "plate"),
        Some(other) => Err(ToolError::new("bad_each", format!("each: '{other}'"), "use \"object\" or \"plate\"")),
    }
}

/// A file's name before its ending, and the ending (`.gcode.3mf` and `.only3d.json` whole).
fn split_ext(path: &std::path::Path) -> (String, String) {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("export").to_string();
    let lower = name.to_ascii_lowercase();
    for e in [".gcode.3mf", ".only3d.json"] {
        if lower.ends_with(e) {
            return (name[..name.len() - e.len()].to_string(), e[1..].to_string());
        }
    }
    match name.rsplit_once('.') {
        Some((a, b)) => (a.to_string(), b.to_ascii_lowercase()),
        None => (name, String::new()),
    }
}

fn file_safe(n: &str) -> String {
    n.trim().replace(['/', '\\', ':'], "-")
}

/// One file per plate (`<name> - Plate 2.ext`) or per object (`<object>.ext`), beside `path`.
fn export_each(s: &mut Studio, path: &std::path::Path, i: &ExportIn, by_plate: bool) -> Result<ToolOutput, ToolError> {
    let dir = path.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
    let (stem, ext) = split_ext(path);
    let mut written: Vec<String> = Vec::new();
    let keep = |o: &only3d_model::EvaluatedObject| !o.hidden && i.ids.as_ref().is_none_or(|ids| ids.contains(&o.id));
    if by_plate {
        let all = s.evaluated_all()?;
        let active = s.plate;
        let mut failed = None;
        for k in 1..=s.project.plate_count() {
            if !all.iter().any(|o| o.plate == k && keep(o)) {
                continue;
            }
            s.set_plate(k);
            let p = dir.join(format!("{stem} - {}.{ext}", file_safe(&s.project.plate_name(k))));
            if let Err(e) = export_one(s, &p, i.ids.as_deref(), false, &Fmt::of(i)) {
                failed = Some(e);
                break;
            }
            written.push(p.display().to_string());
        }
        s.set_plate(active);
        if let Some(e) = failed {
            return Err(e);
        }
    } else {
        if ext == "gcode" || ext == "gcode.3mf" {
            return Err(ToolError::new("bad_each", "a print file holds a whole plate", "use each: \"plate\" for one print file per plate"));
        }
        let objs = if i.all_plates { s.evaluated_all()? } else { s.evaluated()? };
        let picks: Vec<(String, String)> = objs.iter().filter(|o| keep(o)).map(|o| (o.id.clone(), if o.name.is_empty() { o.id.clone() } else { o.name.clone() })).collect();
        let mut used = std::collections::HashSet::new();
        for (id, name) in picks {
            let mut base = file_safe(&name);
            if !used.insert(base.clone()) {
                base = format!("{base} ({id})");
                used.insert(base.clone());
            }
            let p = dir.join(format!("{base}.{ext}"));
            export_one(s, &p, Some(std::slice::from_ref(&id)), i.all_plates, &Fmt::of(i))?;
            written.push(p.display().to_string());
        }
    }
    if written.is_empty() {
        return Err(ToolError::new("nothing_to_export", "no matching visible objects", "check ids with `describe`"));
    }
    let names: Vec<String> = written.iter().filter_map(|w| std::path::Path::new(w).file_name().map(|n| n.to_string_lossy().to_string())).collect();
    Ok(ToolOutput::text(format!("Wrote {} file(s) to {}:\n{}", written.len(), dir.display(), names.join("\n"))).with_data(json!({"paths": written})))
}

/// How a file is written: text STL, picture size and view.
struct Fmt {
    ascii: bool,
    size: Option<u32>,
    view: Option<String>,
}

impl Fmt {
    fn of(i: &ExportIn) -> Fmt {
        Fmt { ascii: i.ascii, size: i.size, view: i.view.clone() }
    }
}

/// One file: the plate's (or every plate's) visible objects, or just `ids`.
fn export_one(s: &mut Studio, path: &std::path::Path, ids: Option<&[String]>, all_plates: bool, fmt: &Fmt) -> Result<ToolOutput, ToolError> {
    let ascii = fmt.ascii;
    let lower = path.to_string_lossy().to_ascii_lowercase();
    if lower.ends_with(".gcode") || lower.ends_with(".gcode.3mf") {
        if all_plates {
            return Err(ToolError::new("one_plate", "a print file holds one plate", "use each: \"plate\" for one print file per plate"));
        }
        if !s.slice_is_current() {
            run_slice(s)?;
        }
        let sl = s.last_slice.clone().expect("sliced");
        let bytes: &[u8] = if lower.ends_with(".gcode") { sl.result.gcode.as_bytes() } else { &sl.package };
        std::fs::write(path, bytes).map_err(|e| ToolError::new("io_error", format!("{}: {e}", path.display()), "check the folder exists"))?;
        // register the export so a later print of this file (from Bambu Studio/Handy) traces back here
        let stem = path.file_name().and_then(|n| n.to_str()).unwrap_or("").trim_end_matches(".gcode.3mf").trim_end_matches(".gcode").to_string();
        let slots: Vec<u32> = if sl.result.stats.filaments.len() > 1 { sl.result.stats.filaments.iter().map(|f| f.0 as u32 + 1).collect() } else { s.evaluated()?.iter().find(|o| !o.hidden).map(|o| vec![o.filament]).unwrap_or_default() };
        let trays: Vec<(u32, i32)> = slots.iter().filter_map(|k| crate::spools::spool_for_slot(&s.project, *k as usize - 1).and_then(|sp| sp.location).and_then(|l| crate::spools::location_tray(&l)).map(|t| (*k, t))).collect();
        let traced = crate::spools::record_print(s, "EXPORTED", &stem, path.file_name().and_then(|n| n.to_str()), None, None, &trays, false).ok();
        let (summary, data) = slice_summary(&sl);
        return Ok(ToolOutput::text(format!("Wrote {} ({} KB).{} {}", path.display(), bytes.len() / 1024, traced.as_ref().map(|p| format!(" Registered as {p} (traced when it prints)." )).unwrap_or_default(), summary)).with_data(json!({"path": path, "bytes": bytes.len(), "slice": data, "print_id": traced})));
    }
    if all_plates && (lower.ends_with(".scad") || lower.ends_with(".png")) {
        return Err(ToolError::new("one_plate", "this format covers one plate", "use each: \"plate\" for one file per plate"));
    }
    if lower.ends_with(".scad") {
        return export_scad(s, path, ids);
    }
    if lower.ends_with(".png") {
        let view = match &fmt.view {
            Some(v) => serde_json::from_value(json!(v)).map_err(|_| ToolError::new("bad_view", format!("view '{v}'"), "iso, front, back, left, right, top or bottom"))?,
            None => only3d_geom::raster::View::Iso,
        };
        let width = fmt.size.unwrap_or(1600).clamp(200, 2048);
        let req = RenderRequest { view, width, height: width * 3 / 4, ids: ids.map(<[String]>::to_vec), ..Default::default() };
        let png = render::render(s, &req)?;
        std::fs::write(path, &png).map_err(|e| ToolError::new("io_error", format!("{}: {e}", path.display()), "check the folder exists"))?;
        return Ok(ToolOutput::text(format!("Rendered {} ({} KB)", path.display(), png.len() / 1024)).with_data(json!({"path": path})));
    }
    let objs = if all_plates { s.evaluated_all()? } else { s.evaluated()? };
    // multi-part objects are written part by part, each in its filament's colour
    let colour = |f: u32| s.project.filaments.get(f as usize - 1).map(|x| render::parse_hex(&x.color));
    // several plates in one file sit side by side, as in the all-plates view
    let gap = s.bed.size[0] + 20.0;
    let mut list: Vec<only3d_geom::io::ImportedObject> = Vec::new();
    for o in objs.iter().filter(|o| !o.hidden && ids.is_none_or(|ids| ids.contains(&o.id))) {
        let shift = glam::Vec3::new(if all_plates { o.plate.saturating_sub(1) as f32 * gap } else { 0.0 }, 0.0, 0.0);
        let placed = |m: &only3d_geom::Mesh| {
            let mut m = m.clone();
            if shift.x != 0.0 {
                m.translate(shift);
            }
            m
        };
        if o.parts.is_empty() {
            list.push(only3d_geom::io::ImportedObject { name: o.name.clone(), mesh: placed(&o.mesh), color: colour(o.filament), parts: Vec::new() });
        } else {
            for p in &o.parts {
                list.push(only3d_geom::io::ImportedObject { name: format!("{} · {}", o.name, p.name), mesh: placed(&p.mesh), color: colour(p.filament), parts: Vec::new() });
            }
        }
    }
    if list.is_empty() {
        return Err(ToolError::new("nothing_to_export", "no matching visible objects", "check ids with `describe`"));
    }
    if ascii && lower.ends_with(".stl") {
        let mut m = only3d_geom::Mesh::new();
        list.iter().for_each(|o| m.append(&o.mesh));
        std::fs::write(path, only3d_geom::io::stl::write_ascii(&m, &split_ext(path).0)).map_err(|e| ToolError::new("io_error", format!("{}: {e}", path.display()), "check the folder exists"))?;
    } else {
        only3d_geom::io::save(path, &list).map_err(|e| ToolError::new("export_failed", e.to_string(), "use .stl, .obj, .3mf, .scad, .png, .zip, .gcode or .gcode.3mf"))?;
    }
    Ok(ToolOutput::text(format!("Exported {} object(s) to {}", list.len(), path.display())).with_data(json!({"path": path})))
}

/// The project and the model files it imports, in one zip: `<name>.only3d.json` plus
/// `models/…`, with the imports pointing there, so it opens anywhere once unzipped.
fn export_zip(s: &mut Studio, path: &std::path::Path) -> Result<ToolOutput, ToolError> {
    use std::io::Write;
    let io = |e: &dyn std::fmt::Display| ToolError::new("io_error", format!("{}: {e}", path.display()), "pick a folder you can write to");
    let base = s.project_path.as_ref().and_then(|p| p.parent()).filter(|p| !p.as_os_str().is_empty()).map(std::path::Path::to_path_buf).unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let mut doc = serde_json::to_value(&s.project).map_err(|e| io(&e))?;
    fn walk(v: &mut Value, base: &std::path::Path, files: &mut Vec<(String, std::path::PathBuf)>) {
        match v {
            Value::Object(m) => {
                if m.get("type").and_then(|t| t.as_str()) == Some("import") {
                    if let Some(p) = m.get("path").and_then(|p| p.as_str()).map(String::from) {
                        let src = expand(&p);
                        let src = if src.is_absolute() { src } else { base.join(src) };
                        if src.is_file() {
                            let entry = match files.iter().find(|(_, f)| *f == src) {
                                Some((n, _)) => n.clone(),
                                None => {
                                    let name = src.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "model".into());
                                    let mut n = format!("models/{name}");
                                    let mut k = 2;
                                    while files.iter().any(|(e, _)| *e == n) {
                                        n = format!("models/{k}-{name}");
                                        k += 1;
                                    }
                                    files.push((n.clone(), src));
                                    n
                                }
                            };
                            m.insert("path".into(), Value::String(entry));
                        }
                    }
                }
                for (_, c) in m.iter_mut() {
                    walk(c, base, files);
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|c| walk(c, base, files)),
            _ => {}
        }
    }
    let mut files = Vec::new();
    walk(&mut doc, &base, &mut files);
    let stem = split_ext(path).0;
    let mut z = zip::ZipWriter::new(std::fs::File::create(path).map_err(|e| io(&e))?);
    let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    z.start_file(format!("{stem}.only3d.json"), opt).map_err(|e| io(&e))?;
    z.write_all(serde_json::to_string_pretty(&doc).map_err(|e| io(&e))?.as_bytes()).map_err(|e| io(&e))?;
    for (name, src) in &files {
        z.start_file(name.as_str(), opt).map_err(|e| io(&e))?;
        z.write_all(&std::fs::read(src).map_err(|e| io(&e))?).map_err(|e| io(&e))?;
    }
    z.finish().map_err(|e| io(&e))?;
    let scad = files.iter().any(|(n, _)| n.ends_with(".scad"));
    Ok(ToolOutput::text(format!("Wrote {}: the project and {} model file(s){}", path.display(), files.len(), if scad { " (a .scad file's own includes are not bundled)" } else { "" })).with_data(json!({"path": path, "files": files.len()})))
}

/// The active plate's visible objects (or just `ids`) as OpenSCAD source, plus STL files beside it
/// for meshes too big to write inline.
fn export_scad(s: &mut Studio, path: &std::path::Path, ids: Option<&[String]>) -> Result<ToolOutput, ToolError> {
    let plate = s.plate;
    let objs: Vec<&Object> = s.project.objects.iter().filter(|o| match ids {
        Some(ids) => ids.contains(&o.id),
        None => o.plate == plate && !o.hidden,
    }).collect();
    if objs.is_empty() {
        return Err(ToolError::new("nothing_to_export", "no matching visible objects", "check ids with `describe`"));
    }
    let n = objs.len();
    let x = only3d_model::scad::export(&s.project, &objs, &mut s.evaluator, &s.bed, path)?;
    let dir = path.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
    let io = |p: &std::path::Path, e: String| ToolError::new("io_error", format!("{}: {e}", p.display()), "pick a folder you can write to");
    for (name, m) in &x.sidecars {
        let f = dir.join(name);
        only3d_geom::io::save(&f, &[only3d_geom::io::ImportedObject::new(name.clone(), m.clone())]).map_err(|e| io(&f, e.to_string()))?;
    }
    std::fs::write(path, &x.source).map_err(|e| io(path, e.to_string()))?;
    let mut text = format!("Wrote {} ({} object{}, {} KB of OpenSCAD).", path.display(), n, if n == 1 { "" } else { "s" }, x.source.len().div_ceil(1024));
    if !x.meshed.is_empty() {
        text.push_str(&format!(" Written as meshes, since OpenSCAD has no shape for them: {}.", x.meshed.join(", ")));
    }
    if !x.sidecars.is_empty() {
        text.push_str(&format!(" It imports {} STL file{} saved beside it: {}.", x.sidecars.len(), if x.sidecars.len() == 1 { "" } else { "s" }, x.sidecars.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")));
    }
    Ok(ToolOutput::text(text).with_data(json!({"path": path, "bytes": x.source.len(), "meshed": x.meshed, "files": x.sidecars.iter().map(|(n, _)| n).collect::<Vec<_>>()})))
}

fn t_export_bambu(s: &mut Studio, i: BambuExportIn) -> Result<ToolOutput, ToolError> {
    let path = match &i.path {
        Some(p) => expand(p),
        None => {
            let name = if s.project.name.is_empty() { "plate".to_string() } else { s.project.name.replace(['/', '\\', ':'], "_") };
            match &s.project_path {
                Some(pp) => {
                    let stem = pp.file_name().and_then(|n| n.to_str()).unwrap_or("plate").trim_end_matches(".json").trim_end_matches(".only3d").to_string();
                    pp.with_file_name(format!("{stem}.bambu.3mf"))
                }
                None => {
                    let dir = crate::config_dir().join("exports");
                    std::fs::create_dir_all(&dir).map_err(|e| ToolError::new("io_error", e.to_string(), ""))?;
                    dir.join(format!("{name}.3mf"))
                }
            }
        }
    };
    let mut out = crate::bambu_export::export_bambu_project(s, &path, i.slice)?;
    if i.open {
        crate::bambu_export::open_in_bambu_studio(&path)?;
        out.text.push_str(" Opened in Bambu Studio: press Slice there, then Print.");
    }
    Ok(out)
}

fn t_batch(s: &mut Studio, i: BatchIn) -> Result<ToolOutput, ToolError> {
    if i.calls.len() > 100 {
        return Err(ToolError::new("batch_too_large", "at most 100 calls per batch", "split the batch"));
    }
    if let Some(c) = i.calls.iter().find(|c| c.tool == "batch" || c.tool == "print") {
        return Err(ToolError::new("not_batchable", format!("'{}' cannot run inside a batch", c.tool), "call it on its own"));
    }
    let snapshot = s.project.clone();
    let reg = s.registry();
    let mut texts = Vec::new();
    let mut images = Vec::new();
    let mut results = Vec::new();
    for (k, c) in i.calls.into_iter().enumerate() {
        let tool = reg.get(&c.tool).ok_or_else(|| ToolError::new("unknown_tool", format!("call {k}: no tool '{}'", c.tool), "see `only3d tools`"))?;
        match (tool.handler)(s, if c.args.is_null() { json!({}) } else { c.args }) {
            Ok(o) => {
                texts.push(format!("[{k}] {}: {}", c.tool, o.text));
                results.push(o.data);
                images.extend(o.images);
            }
            Err(e) => {
                s.project = snapshot;
                s.changed("batch");
                return Err(ToolError::new(e.code, format!("call {k} ({}) failed, batch rolled back: {}", c.tool, e.message), e.hint));
            }
        }
    }
    let mut out = ToolOutput::text(texts.join("\n")).with_data(Value::Array(results));
    out.images = images;
    Ok(out)
}

#[derive(Deserialize, JsonSchema)]
pub struct WaitEventIn {
    /// The `next_since` of your last call; omit to wait for the next event.
    #[serde(default)]
    pub since: Option<u64>,
    /// Only these kinds: project_changed, selection_changed, sliced, approval, printer_state,
    /// printer_stage, printer_nozzle, printer_alert, print_progress, print_finished, print_failed,
    /// printer_connection. Empty = all.
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Give up after this many seconds (default 120, max 1800).
    #[serde(default)]
    pub timeout_s: Option<u64>,
}

fn t_wait_event(s: &mut Studio, i: WaitEventIn) -> Result<ToolOutput, ToolError> {
    let log = s.events.clone();
    crate::events::wait_tool(&log, &json!({"since": i.since, "kinds": i.kinds, "timeout_s": i.timeout_s}))
}

fn t_undo(s: &mut Studio, _: Empty) -> Result<ToolOutput, ToolError> {
    if s.undo() {
        Ok(ToolOutput::text(format!("Undone (rev {}).", s.revision)))
    } else {
        Err(ToolError::new("nothing_to_undo", "history is empty", ""))
    }
}

fn t_redo(s: &mut Studio, _: Empty) -> Result<ToolOutput, ToolError> {
    if s.redo() {
        Ok(ToolOutput::text(format!("Redone (rev {}).", s.revision)))
    } else {
        Err(ToolError::new("nothing_to_redo", "nothing to redo", ""))
    }
}

fn t_history(s: &mut Studio, i: HistoryIn) -> Result<ToolOutput, ToolError> {
    let n = i.limit.unwrap_or(20);
    let items: Vec<_> = s.journal.iter().rev().take(n).collect();
    let text = items.iter().map(|e| format!("rev {} {:?} {}{}: {}", e.revision, e.actor, e.tool, if e.ok { "" } else { " (failed)" }, e.summary)).collect::<Vec<_>>().join("\n");
    Ok(ToolOutput::text(if text.is_empty() { "No history yet.".into() } else { text }).with_data(serde_json::to_value(&items).unwrap_or(Value::Null)))
}

fn register_builtin(r: &mut Registry) {
    let tools = vec![
        def::<DescribeIn>("describe", "Describe session", "Start here: the project (printer, filaments, params, plates, objects with size and health), the selection, findings, last slice. render=true adds a picture.", false, false, t_describe),
        def::<Empty>("get_project", "Get project JSON", "Return the full project document (JSON). Edit it and send it back with set_project for large changes.", false, false, t_get_project),
        def::<SetProjectIn>("set_project", "Replace project", "Replace the whole project document in one step (undoable). Validates and evaluates every object.", true, false, t_set_project),
        def::<NewProjectIn>("new_project", "New project", "Start an empty project (Bambu Lab H2D, Textured PEI Plate, PLA Basic).", true, false, t_new_project),
        def::<PathIn>("open_project", "Open project", "Open an only3d project file (*.only3d.json).", true, true, t_open_project),
        def::<SaveIn>("save_project", "Save project", "Save the project as JSON (*.only3d.json).", false, true, t_save_project),
        def::<SetParamsIn>("set_params", "Set parameters", "Set or remove project parameters used in shape expressions (wall=2.4); dependents rebuild. With `object`: a .scad object's variables.", true, false, t_set_params),
        def::<AddObjectIn>("add_object", "Add object", "Add a parametric object to the active plate; returns size, volume, health, findings. Example shape: {\"type\":\"difference\",\"children\":[{\"type\":\"box\",\"size\":[60,40,8]},{\"type\":\"cylinder\",\"d\":5,\"h\":20}]}.", true, false, t_add_object),
        def::<UpdateObjectIn>("update_object", "Update object", "Change an object's shape, place, turn, scale, filament or spool, visibility or slicer settings.", true, false, t_update_object),
        def::<IdsIn>("remove_object", "Remove objects", "Delete objects from the plate (undoable).", true, false, t_remove_object),
        def::<DuplicateIn>("duplicate_object", "Duplicate object", "Add copies of an object in free space next to it (nothing moves), in a row, a grid, or filling the plate.", true, false, t_duplicate),
        def::<SplitIn>("split_object", "Split object", "Cut an object in two along a plane: halves `<id>_a` and `<id>_b`, side by side.", true, false, t_split),
        def::<ImportIn>("import_file", "Import file", "Add a model file (STL, OBJ, 3MF, STEP, .scad) as an object; file edits are picked up. Lands clear of other objects.", true, false, t_import),
        def::<crate::arrange::ArrangeIn>("arrange", "Arrange", "Pack the active plate (default), or place `ids` against an anchor (object or plate) with `op`. Left/right = -X/+X, front/back = -Y/+Y (front edge y=0). Locked objects stay.", true, false, t_arrange),
        def::<Empty>("plates", "Plates", "Build plates: names, objects on each, the active one (slice, check, print, arrange and render act on it).", false, false, t_plates),
        def::<PlateIn>("plate", "Plate", "Add, select, rename, duplicate or remove a build plate, or move objects onto one. Tools that name an object switch to its plate.", true, false, t_plate),
        def::<OrientIn>("orient", "Auto-orient", "Rotate objects to minimise overhangs (support) and maximise bed contact.", true, false, t_orient),
        def::<IdsIn>("select", "Select", "Select objects (highlighted in the GUI). Use [] to clear.", false, false, t_select),
        def::<Empty>("get_selection", "Get selection", "The user's GUI selection with the objects' full definitions, for 'this' / 'that part'.", false, false, t_get_selection),
        def::<CheckIn>("check", "Check", "Printability checks (reach, height, watertight, overhangs, tipping, overlaps) plus saved design checks (`add`/`remove`).", true, false, t_check),
        def::<IdIn>("measure", "Measure object", "Detailed metrics for one object: bounds, volume, area, overhang and bed-contact areas, centroid, bodies, estimated mass.", false, false, t_measure),
        def::<SectionIn>("section", "Cross-section", "Cut the model horizontally at height z: islands, holes, area, and material thinner than two walls.", false, false, t_section),
        def::<ClearanceIn>("clearance", "Fit clearance", "Horizontal gap (negative = interference) between two objects at several heights: pin/socket, lid/body and press fits.", false, false, t_clearance),
        def::<RenderRequest>("render", "Render", "Render the plate (or chosen objects, or the sliced toolpaths) to a PNG image from a view.", false, false, t_render),
        def::<Empty>("settings_schema", "Settings schema", "List every slicer setting you can override (JSON Schema) and the effective values for the current project.", false, false, t_settings_schema),
        def::<Empty>("settings_catalog", "Settings catalog", "Every slicer setting by group with label, unit, range, explanation, the effective value and the project's overrides.", false, false, t_settings_catalog),
        def::<SetPrinterIn>("set_printer", "Set printer", "Choose the machine preset and build plate type.", true, false, t_set_printer),
        def::<SetFilamentIn>("set_filament", "Set filament", "Set a filament slot's preset, colour and nozzle (H2D: left/right/auto). Use slot n+1 to add a slot.", true, false, t_set_filament),
        def::<SetProcessIn>("set_process", "Set process", "Choose the process preset and override settings (layer_height, wall_loops, infill_density 0..1, brim_width, speeds, temperatures...).", true, false, t_set_process),
        def::<ListPresetsIn>("list_presets", "List presets", "List Bambu machine/process/filament presets available to only3d.", false, false, t_list_presets),
        def::<SliceIn>("slice", "Slice", "Slice the plate natively: layers, print time, filament grams/metres/cost, warnings; keeps a print-ready .gcode.3mf for export or printing.", false, false, t_slice),
        def::<ExportIn>("export", "Export", "Write the model, a picture, the project or the print (slices first) to a file.", false, true, t_export),
        def::<BambuExportIn>("export_bambu_project", "Export Bambu Studio project", "Write a Bambu Studio .3mf (presets, colours, plates, overrides) for what the native slicer can't do yet (tree supports). open: true opens it there.", false, true, t_export_bambu),
        def::<BatchIn>("batch", "Batch", "Run several tool calls in one step, all-or-nothing (one undo step). Example: {\"calls\":[{\"tool\":\"set_params\",\"args\":{\"params\":{\"wall\":3}}},{\"tool\":\"check\"}]}.", true, false, t_batch),
        def::<WaitEventIn>("wait_for_event", "Wait for event", "Wait for what happens next (edits, selection, slices, approvals, printer state, alerts, layers, finish or failure). Returns the events after `since` and `next_since`.", false, false, t_wait_event),
        def::<Empty>("undo", "Undo", "Undo the last change.", false, false, t_undo),
        def::<Empty>("redo", "Redo", "Redo the last undone change.", false, false, t_redo),
        def::<HistoryIn>("history", "History", "Recent actions by the user and agents, newest first.", false, false, t_history),
    ];
    for t in tools {
        r.add(t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 3MF of 20 mm cubes centred at `at` (file coordinates), in a fresh temp folder.
    fn cubes_3mf(name: &str, at: &[[f32; 2]]) -> String {
        let dir = std::env::temp_dir().join(format!("o3d_import_{}_{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let objs: Vec<only3d_geom::io::ImportedObject> = at
            .iter()
            .enumerate()
            .map(|(k, c)| {
                let mut m = only3d_geom::shapes::cuboid(glam::Vec3::new(20.0, 20.0, 10.0)).unwrap();
                m.translate(glam::Vec3::new(c[0], c[1], 0.0));
                only3d_geom::io::ImportedObject { name: format!("cube{k}"), mesh: m, color: None, parts: Vec::new() }
            })
            .collect();
        let path = dir.join(format!("{name}.3mf"));
        std::fs::write(&path, only3d_geom::io::threemf::write(&objs).unwrap()).unwrap();
        path.display().to_string()
    }

    fn footprint(s: &mut Studio, id: &str) -> (u32, [f32; 4]) {
        let o = s.evaluated_all().unwrap().iter().find(|o| o.id == id).cloned().unwrap();
        let b = o.report.bounds;
        (o.plate, [b.min.x, b.min.y, b.max.x, b.max.y])
    }

    fn overlaps(a: [f32; 4], b: [f32; 4]) -> bool {
        a[0] < b[2] && a[2] > b[0] && a[1] < b[3] && a[3] > b[1]
    }

    #[test]
    fn step_files_import_as_solids() {
        let mut s = Studio::new();
        let f = concat!(env!("CARGO_MANIFEST_DIR"), "/../only3d-geom/tests/data/plate_hole.step");
        let r = s.call("import_file", json!({"path": f, "id": "plate"}), Actor::User).unwrap();
        let o = s.evaluated().unwrap().iter().find(|o| o.id == "plate").cloned().unwrap();
        assert!(o.report.watertight, "{}", r.text);
        assert!((o.report.size.x - 20.0).abs() < 1e-3 && (o.report.volume_mm3 - 937.2).abs() < 1.5, "{:?}", o.report);
    }

    #[test]
    fn scad_objects_take_their_variables_through_set_params() {
        if std::process::Command::new(only3d_model::scad::exe()).arg("--version").output().is_err() {
            return; // no OpenSCAD here
        }
        let dir = std::env::temp_dir().join(format!("o3d_tools_scad_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("tag.scad");
        std::fs::write(&f, "width = 40; // [10:100]\nthick = 2;\nround = false;\nsize = [5, 6];\ncube([width, size[1], thick]);\n").unwrap();
        let mut s = Studio::new();
        let r = s.call("import_file", json!({"path": f, "id": "tag"}), Actor::User).unwrap();
        assert!(r.text.contains("width=40 [10–100]") && r.data["scad_vars"].as_array().is_some_and(|v| v.len() == 4), "{}", r.text);
        s.call("set_params", json!({"params": {"t": 1.5}}), Actor::User).unwrap();
        let r = s.call("set_params", json!({"object": "tag", "params": {"width": 55, "thick": "t*2"}}), Actor::Agent).unwrap();
        assert!(r.text.contains("width=55 (file 40)"), "{}", r.text);
        let size = |s: &mut Studio| s.evaluated().unwrap().iter().find(|o| o.id == "tag").unwrap().report.size;
        let z = size(&mut s);
        assert!((z.x - 55.0).abs() < 1e-3 && (z.z - 3.0).abs() < 1e-3, "{z:?}");
        // a project parameter the expression uses rebuilds it
        s.call("set_params", json!({"params": {"t": 2}}), Actor::User).unwrap();
        assert!((size(&mut s).z - 4.0).abs() < 1e-3);
        // back to the file's value: the override goes
        s.call("set_params", json!({"object": "tag", "params": {"width": 40, "thick": null}}), Actor::User).unwrap();
        let only3d_model::Shape::Import { vars, .. } = &s.project.object("tag").unwrap().shape.shape else { panic!() };
        assert!(vars.is_empty(), "{vars:?}");
        for (bad, code) in [(json!({"widht": 3}), "unknown_variable"), (json!({"width": "nope"}), "bad_value"), (json!({"size": [1, 2, 3]}), "bad_value"), (json!({"round": 1}), "bad_value")] {
            let e = s.call("set_params", json!({"object": "tag", "params": bad}), Actor::Agent).unwrap_err();
            assert_eq!(e.code, code, "{bad}: {}", e.message);
        }
        s.call("add_object", json!({"id": "cube", "shape": {"type": "box", "size": [5, 5, 5]}}), Actor::User).unwrap();
        assert_eq!(s.call("set_params", json!({"object": "cube", "params": {"w": 1}}), Actor::User).unwrap_err().code, "not_scad");
        // and the plate goes out as OpenSCAD that includes the file
        let out = dir.join("sub").join("out.scad");
        let r = s.call("export", json!({"path": out}), Actor::User).unwrap();
        let src = std::fs::read_to_string(&out).unwrap();
        assert!(src.contains("include <../tag.scad>") && src.contains("module cube_part()") && r.data["meshed"].as_array().unwrap().is_empty(), "{}\n{src}", r.text);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exports_per_object_per_plate_and_as_a_bundle() {
        let dir = std::env::temp_dir().join(format!("o3d_export_{}_{}", std::process::id(), crate::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = Studio::new();
        // an imported model, so the bundle has a file to carry
        let model = dir.join("peg.stl");
        std::fs::write(&model, only3d_geom::io::stl::write_binary(&only3d_geom::shapes::cuboid(glam::Vec3::splat(6.0)).unwrap())).unwrap();
        s.call("add_object", json!({"id": "a", "name": "Block A", "shape": {"type": "box", "size": [10, 10, 10]}}), Actor::User).unwrap();
        s.call("add_object", json!({"id": "peg", "shape": {"type": "import", "path": model.display().to_string()}}), Actor::User).unwrap();
        s.call("plate", json!({"action": "add"}), Actor::User).unwrap();
        s.call("plate", json!({"action": "move", "plate": 2, "ids": ["peg"]}), Actor::User).unwrap();
        s.call("plate", json!({"action": "select", "plate": 1}), Actor::User).unwrap();
        // one file per object, across plates, as text STL
        let r = s.call("export", json!({"path": dir.join("x.stl"), "each": "object", "all_plates": true, "ascii": true}), Actor::User).unwrap();
        assert_eq!(r.data["paths"].as_array().unwrap().len(), 2, "{}", r.text);
        assert!(std::fs::read_to_string(dir.join("Block A.stl")).unwrap().starts_with("solid "));
        // one file per plate
        let r = s.call("export", json!({"path": dir.join("set.3mf"), "each": "plate"}), Actor::User).unwrap();
        assert!(dir.join("set - Plate 1.3mf").exists() && dir.join("set - Plate 2.3mf").exists(), "{}", r.text);
        assert_eq!(s.plate, 1, "the active plate comes back");
        // all plates in one file, side by side; a picture; and a bundle with the model file
        s.call("export", json!({"path": dir.join("all.stl"), "all_plates": true}), Actor::User).unwrap();
        let both = only3d_geom::io::load_merged(&dir.join("all.stl")).unwrap().bounds();
        assert!(both.max.x - both.min.x > 300.0, "plates side by side: {both:?}");
        s.call("export", json!({"path": dir.join("pic.png"), "size": 400, "view": "top"}), Actor::User).unwrap();
        let png = std::fs::read(dir.join("pic.png")).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
        assert_eq!((u32::from_be_bytes(png[16..20].try_into().unwrap()), u32::from_be_bytes(png[20..24].try_into().unwrap())), (400, 300), "size sets the width");
        assert_eq!(s.call("export", json!({"path": dir.join("p.png"), "view": "sideways"}), Actor::User).unwrap_err().code, "bad_view");
        let r = s.call("export", json!({"path": dir.join("bundle.zip")}), Actor::User).unwrap();
        assert_eq!(r.data["files"], 1, "{}", r.text);
        let mut z = zip::ZipArchive::new(std::fs::File::open(dir.join("bundle.zip")).unwrap()).unwrap();
        assert!(z.by_name("models/peg.stl").is_ok());
        let mut doc = String::new();
        std::io::Read::read_to_string(&mut z.by_name("bundle.only3d.json").unwrap(), &mut doc).unwrap();
        assert!(doc.contains("\"models/peg.stl\""), "{doc}");
        // nonsense is refused with a hint
        assert_eq!(s.call("export", json!({"path": dir.join("x.gcode.3mf"), "each": "object"}), Actor::User).unwrap_err().code, "bad_each");
        assert_eq!(s.call("export", json!({"path": dir.join("x.scad"), "all_plates": true}), Actor::User).unwrap_err().code, "one_plate");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn imports_land_clear_of_what_is_there_and_say_so() {
        let mut s = Studio::new();
        // a two-object file, twice: the second copy keeps its layout as a block, beside the first
        let pair = cubes_3mf("pair", &[[150.0, 160.0], [200.0, 160.0]]);
        let a = s.call("import_file", json!({"path": pair}), Actor::User).unwrap();
        let a_ids: Vec<String> = serde_json::from_value(a.data["ids"].clone()).unwrap();
        assert_eq!(a.data["placed"], "", "{}", a.text);
        let (_, a0) = footprint(&mut s, &a_ids[0]);
        assert!((a0[0] - 140.0).abs() < 0.01, "the file's layout is kept: {a0:?}");
        let b = s.call("import_file", json!({"path": pair}), Actor::User).unwrap();
        let b_ids: Vec<String> = serde_json::from_value(b.data["ids"].clone()).unwrap();
        assert!(b.text.contains("would have landed on") && b.data["placed"].as_str().unwrap().contains("nearest free spot"), "{}", b.text);
        let (_, b0) = footprint(&mut s, &b_ids[0]);
        let (_, b1) = footprint(&mut s, &b_ids[1]);
        assert!(((b1[0] - b0[0]) - 50.0).abs() < 0.01 && (b1[1] - b0[1]).abs() < 0.01, "the block keeps its spacing: {b0:?} {b1:?}");
        for x in &a_ids {
            let (_, fa) = footprint(&mut s, x);
            assert!(!overlaps(fa, b0) && !overlaps(fa, b1), "no overlap with {x}");
            assert_eq!(footprint(&mut s, x).1, fa);
        }
        assert!((footprint(&mut s, &a_ids[0]).1[0] - 140.0).abs() < 0.01, "what was there did not move");
        // a single object dropped at a spot that is taken goes to the nearest free one
        let one = cubes_3mf("one", &[[0.0, 0.0]]);
        let c = s.call("import_file", json!({"path": one, "id": "drop", "position": [150, 160]}), Actor::User).unwrap();
        assert!(c.data["placed"].as_str().is_some_and(|t| t.contains("'drop'") || t.contains("would have landed")), "{}", c.text);
        let (_, d) = footprint(&mut s, "drop");
        assert!(a_ids.iter().chain(&b_ids).all(|x| !overlaps(footprint(&mut s, x).1, d)));
        // a free spot is used as given
        let e = s.call("import_file", json!({"path": one, "id": "free", "position": [60, 280]}), Actor::User).unwrap();
        assert!(e.data.get("placed").is_none(), "{}", e.text);
        let (_, f) = footprint(&mut s, "free");
        assert!((f[0] - 50.0).abs() < 0.01 && (f[1] - 270.0).abs() < 0.01, "{f:?}");
    }

    #[test]
    fn copies_by_offset_step_from_the_original() {
        let mut s = Studio::new();
        s.call("add_object", json!({"id": "peg", "shape": {"type": "box", "size": [10, 10, 10]}, "position": [100, 100]}), Actor::User).unwrap();
        let r = s.call("duplicate_object", json!({"id": "peg", "offset": [15, 0], "copies": 2}), Actor::User).unwrap();
        let ids: Vec<String> = serde_json::from_value(r.data["ids"].clone()).unwrap();
        assert_eq!(ids.len(), 2);
        assert_eq!(s.project.object(&ids[0]).unwrap().position, Some([115.0, 100.0]));
        assert_eq!(s.project.object(&ids[1]).unwrap().position, Some([130.0, 100.0]));
        assert_eq!(s.project.object("peg").unwrap().position, Some([100.0, 100.0]), "the original stays");
    }

    #[test]
    fn push_pull_moves_the_face_and_nothing_else() {
        let mut s = Studio::new();
        s.call("add_object", json!({"id": "b", "shape": {"type": "box", "size": [20, 10, 5]}, "position": [100, 100]}), Actor::User).unwrap();
        // pull the right side 6 mm: the left side stays at x = 90
        let r = s.call("update_object", json!({"id": "b", "push_pull": {"at": [110, 101, 2], "distance": 6}}), Actor::User).unwrap();
        assert!(r.text.contains("Pulled the right (+X) face"), "{}", r.text);
        let (_, f) = footprint(&mut s, "b");
        assert!((f[0] - 90.0).abs() < 0.02 && (f[2] - 116.0).abs() < 0.02, "{f:?}");
        // turned 90°: the plate's top is still the top, push it down 2 mm
        s.call("update_object", json!({"id": "b", "rotation": [0, 0, 90]}), Actor::User).unwrap();
        let (_, g) = footprint(&mut s, "b");
        s.call("update_object", json!({"id": "b", "push_pull": {"at": [(g[0] + g[2]) / 2.0, (g[1] + g[3]) / 2.0, 5.0], "distance": -2}}), Actor::User).unwrap();
        let o = s.evaluated().unwrap().iter().find(|o| o.id == "b").cloned().unwrap();
        assert!((o.report.size.z - 3.0).abs() < 0.02 && o.report.watertight, "{:?}", o.report);
        assert_eq!(footprint(&mut s, "b").1, g, "the footprint did not move");
        // undo takes the step back
        s.call("undo", json!({}), Actor::User).unwrap();
        assert!((s.evaluated().unwrap().iter().find(|o| o.id == "b").unwrap().report.size.z - 5.0).abs() < 0.02);
        // nothing there
        assert_eq!(s.call("update_object", json!({"id": "b", "push_pull": {"at": [10, 10, 40], "distance": 2}}), Actor::User).unwrap_err().code, "no_face");
    }

    #[test]
    fn a_full_plate_sends_new_objects_to_a_new_plate() {
        let mut s = Studio::new();
        s.call("add_object", json!({"id": "slab", "shape": {"type": "box", "size": [300, 320, 2]}, "position": [175, 160]}), Actor::User).unwrap();
        let r = s.call("add_object", json!({"id": "cube", "shape": {"type": "box", "size": [20, 20, 20]}}), Actor::User).unwrap();
        assert!(r.text.contains("new plate 2"), "{}", r.text);
        assert_eq!(footprint(&mut s, "cube").0, 2);
        assert_eq!(s.plate, 2, "the new plate is shown");
        assert_eq!(footprint(&mut s, "slab").1, [25.0, 0.0, 325.0, 320.0], "the slab did not move");
    }

    #[test]
    fn fit_coupons_and_calibrated_holes() {
        let mut s = Studio::new();
        let r = s.call("fit", json!({"action": "coupon", "nominal": 5}), Actor::Agent).unwrap();
        assert!(r.text.contains("fit_coupon") && r.text.contains("fit_pin"), "{}", r.text);
        let objs = s.evaluated().unwrap();
        let bar = objs.iter().find(|o| o.id == "fit_coupon").unwrap();
        assert!(bar.report.watertight, "{:?}", bar.report);
        assert!(bar.report.size.x > 70.0 && (bar.report.size.z - 6.0).abs() < 0.01);
        // a calibrated fit widens holes that don't set their own (the coupon's don't change)
        s.call("add_object", json!({"id": "plate", "shape": {"type": "difference", "children": [{"type": "box", "size": [20, 20, 5]}, {"type": "hole", "d": 5, "depth": 6, "at": [0, 0, 5]}]}}), Actor::User).unwrap();
        let vol = |s: &mut Studio, id: &str| s.evaluated_all().unwrap().iter().find(|o| o.id == id).unwrap().report.volume_mm3;
        let (v0, c0) = (vol(&mut s, "plate"), vol(&mut s, "fit_coupon"));
        s.fit_cache = Some(((s.project.printer.machine.clone(), s.project.filaments[0].preset.clone()), 0.3));
        s.changed("test");
        let (v1, c1) = (vol(&mut s, "plate"), vol(&mut s, "fit_coupon"));
        let want = std::f64::consts::PI * (2.65f64.powi(2) - 2.5f64.powi(2)) * 5.0;
        assert!(((v0 - v1) - want).abs() < 0.6, "hole grew by the fit: {} vs {want}", v0 - v1);
        assert!((c0 - c1).abs() < 1e-6, "the coupon keeps its raw sizes");
        assert_eq!(s.call("fit", json!({"action": "record"}), Actor::User).unwrap_err().code, "missing_fit");
    }

    #[test]
    fn saved_design_checks_run_after_every_change() {
        let mut s = Studio::new();
        s.call("add_object", json!({"id": "cup", "shape": {"type": "shell", "child": {"type": "cylinder", "d": 60, "h": 80}, "wall": 2}, "position": [100, 160]}), Actor::User).unwrap();
        // a plug in the cup (both rest on the plate, so measure above the cup's 2 mm floor)
        s.call("add_object", json!({"id": "lid", "shape": {"type": "cylinder", "d": 55.4, "h": 30}, "position": [100, 160]}), Actor::User).unwrap();
        let r = s.call("check", json!({"add": {"kind": "size", "object": "cup", "axis": "z", "max": 90, "note": "fits the shelf"}}), Actor::Agent).unwrap();
        assert!(r.text.contains("Saved check c1"), "{}", r.text);
        s.call("check", json!({"add": {"kind": "wall", "object": "cup", "z": 40, "min": 1.8}}), Actor::Agent).unwrap();
        s.call("check", json!({"add": {"kind": "clearance", "object": "lid", "other": "cup", "z": 10, "min": 0.2, "max": 0.5}}), Actor::Agent).unwrap();
        let f = s.findings().unwrap();
        let ok: Vec<&str> = f.iter().filter(|x| x.code == "spec_ok").map(|x| x.message.as_str()).collect();
        assert_eq!(ok.len(), 3, "{f:?}");
        assert!(ok.iter().any(|m| m.contains("thinnest wall") && m.contains("2.0")), "{ok:?}");
        assert!(ok.iter().any(|m| m.contains("0.29 mm gap") || m.contains("0.30 mm gap")), "lid 55.4 in a 56 bore (facets take a little): {ok:?}");
        // the cup grows: the size check fails at once
        s.call("update_object", json!({"id": "cup", "shape": {"type": "shell", "child": {"type": "cylinder", "d": 60, "h": 95}, "wall": 2}}), Actor::User).unwrap();
        assert!(s.findings().unwrap().iter().any(|x| x.code == "spec_failed" && x.message.contains("fits the shelf")));
        // checks are saved with the project and can be removed
        assert_eq!(s.project.specs.len(), 3);
        s.call("check", json!({"remove": "c1"}), Actor::User).unwrap();
        assert!(!s.findings().unwrap().iter().any(|x| x.code == "spec_failed"));
        assert_eq!(s.call("check", json!({"add": {"kind": "size", "object": "nope", "max": 1}}), Actor::User).unwrap_err().code, "bad_check");
        assert_eq!(s.call("check", json!({"add": {"kind": "size", "object": "cup"}}), Actor::User).unwrap_err().code, "no_limit");
    }

    #[test]
    fn split_and_mirror() {
        let mut s = Studio::new();
        s.call("add_object", json!({"id": "bar", "shape": {"type": "box", "size": [100, 20, 10]}, "position": [175, 160]}), Actor::User).unwrap();
        let r = s.call("split_object", json!({"id": "bar", "axis": "x", "at": 145}), Actor::User).unwrap();
        let objs = s.evaluated().unwrap();
        let (a, b) = (objs.iter().find(|o| o.id == "bar_a").unwrap(), objs.iter().find(|o| o.id == "bar_b").unwrap());
        assert!((a.report.size.x - 20.0).abs() < 0.05 && (b.report.size.x - 80.0).abs() < 0.05, "{}", r.text);
        assert!((b.report.bounds.min.x - a.report.bounds.max.x - 5.0).abs() < 0.05, "5 mm apart");
        assert!(a.report.watertight && b.report.watertight);
        // a turned object splits along the plate's axes
        s.call("add_object", json!({"id": "post", "shape": {"type": "box", "size": [10, 10, 60]}, "rotation": [0, 0, 45], "position": [100, 60]}), Actor::User).unwrap();
        s.call("split_object", json!({"id": "post", "axis": "z", "at": 20}), Actor::User).unwrap();
        let objs = s.evaluated().unwrap();
        let hz: Vec<f32> = ["post_a", "post_b"].iter().map(|id| objs.iter().find(|o| o.id == *id).unwrap().report.size.z).collect();
        assert!((hz[0] - 20.0).abs() < 0.05 && (hz[1] - 40.0).abs() < 0.05, "{hz:?}");
        assert_eq!(s.call("split_object", json!({"id": "bar_a", "axis": "x", "at": 500}), Actor::User).unwrap_err().code, "outside");
        // mirroring keeps the size and flips the shape
        s.call("add_object", json!({"id": "l", "shape": {"type": "union", "children": [{"type": "box", "size": [30, 10, 5]}, {"type": "box", "size": [10, 30, 5], "at": [-10, 10, 0]}]}, "position": [250, 250]}), Actor::User).unwrap();
        let before = s.evaluated().unwrap().iter().find(|o| o.id == "l").unwrap().report.size;
        s.call("update_object", json!({"id": "l", "mirror": "x"}), Actor::User).unwrap();
        let after = s.evaluated().unwrap().iter().find(|o| o.id == "l").unwrap().report.clone();
        assert!((after.size - before).length() < 0.05 && after.watertight && !after.inverted);
    }

    #[test]
    fn parts_print_in_their_own_filament() {
        let mut s = Studio::new();
        s.call("set_filament", json!({"slot": 2, "preset": "Bambu PLA Basic @BBL H2D", "color": "#E05A2B"}), Actor::User).unwrap();
        let r = s
            .call(
                "add_object",
                json!({"id": "tag", "shape": {"type": "box", "size": [40, 20, 3]},
                       "parts": [{"name": "inlay", "shape": {"type": "box", "size": [20, 6, 1], "at": [0, 0, 3]}, "filament": 2}]}),
                Actor::Agent,
            )
            .unwrap();
        assert!(r.text.contains("40.0 x 20.0 x 4.0"), "parts count towards the size: {}", r.text);
        let objs = s.evaluated().unwrap();
        let o = &objs[0];
        assert_eq!(o.parts.len(), 2, "the body and the inlay");
        assert_eq!(o.filaments(), vec![1, 2]);
        assert!(o.report.watertight, "touching bodies are each watertight");
        // the inlay sits on the body, wherever the object goes
        let (body, inlay) = (o.parts[0].mesh.bounds(), o.parts[1].mesh.bounds());
        assert!((inlay.min.z - body.max.z).abs() < 1e-3 && ((inlay.min.x + inlay.max.x) - (body.min.x + body.max.x)).abs() < 1e-3);
        let (t, d) = crate::describe::describe(&mut s).unwrap();
        assert!(t.contains("parts: tag (filament 1), inlay (filament 2)") || t.contains("inlay (filament 2)"), "{t}");
        assert_eq!(d["objects"][0]["parts"][1]["filament"], 2);
        // both filaments print natively: one per nozzle, changing on the layers that need both,
        // priming on a tower, through Bambu's own change template
        let r = s.call("slice", json!({}), Actor::Agent).unwrap();
        assert!(r.text.contains("filament changes"), "{}", r.text);
        let out = s.last_slice.clone().unwrap();
        let g = &out.result.gcode;
        assert!(out.lint.ok, "{:?}", out.lint.problems);
        assert!(g.contains("; filament: 1,2"), "header lists both filaments");
        assert!(g.contains("; filament_map = 2,1"), "filament 1 on the right nozzle, 2 on the left");
        assert!(g.contains("; FilamentChangeGCodeStart") && g.contains("M620 S1A"), "the H2D change template");
        assert!(g.contains("; FEATURE: Prime tower"));
        assert!(g.contains("Multi extruder pre"), "the idle nozzle waits cooler");
        // the inlay is only on the top layers: changes happen there, not on every layer
        let changes = g.matches("; FilamentChangeGCodeStart").count();
        assert!((1..=12).contains(&changes), "{changes} changes");
        let st = &out.result.stats;
        assert_eq!(st.filaments.len(), 2);
        assert!(st.filaments[1].2 > 0.05 && st.filaments[1].2 < st.filaments[0].2, "{:?}", st.filaments);
        assert!(st.tower_g > 0.0);
        // parts can be recoloured or removed
        s.call("update_object", json!({"id": "tag", "parts": [{"name": "inlay", "shape": {"type": "box", "size": [20, 6, 1], "at": [0, 0, 3]}, "filament": 1}]}), Actor::User).unwrap();
        assert_eq!(s.evaluated().unwrap()[0].filaments(), vec![1]);
        s.call("update_object", json!({"id": "tag", "parts": []}), Actor::User).unwrap();
        assert!(s.evaluated().unwrap()[0].parts.is_empty());
        assert!(s.call("slice", json!({}), Actor::User).is_ok());
    }

    #[test]
    fn a_queue_of_plates() {
        let mut s = Studio::new();
        for _ in 0..2 {
            s.call("plate", json!({"action": "add"}), Actor::User).unwrap();
        }
        s.call("plate", json!({"action": "queue", "plate": 3}), Actor::User).unwrap();
        let r = s.call("plate", json!({"action": "queue", "plate": 2}), Actor::User).unwrap();
        assert!(r.text.contains("Plate 3 → Plate 2"), "{}", r.text);
        assert_eq!(s.project.queue, vec![3, 2]);
        // removing plate 2 drops it from the queue and renumbers the rest
        s.call("plate", json!({"action": "remove", "plate": 2}), Actor::User).unwrap();
        assert_eq!(s.project.queue, vec![2]);
        s.call("plate", json!({"action": "unqueue", "plate": 2}), Actor::User).unwrap();
        assert!(s.project.queue.is_empty());
    }

    #[test]
    fn plates_scope_what_tools_act_on() {
        let mut s = Studio::new();
        let bx = |id: &str| json!({"id": id, "shape": {"type": "box", "size": [20, 20, 10]}});
        s.call("add_object", bx("a"), Actor::User).unwrap();
        s.call("add_object", bx("b"), Actor::User).unwrap();
        let out = s.call("plate", json!({"action": "add", "name": "Ghosts"}), Actor::Agent).unwrap();
        assert_eq!(s.plate, 2, "{}", out.text);
        s.call("add_object", bx("c"), Actor::Agent).unwrap();
        assert_eq!(s.project.object("c").unwrap().plate, 2);
        // the active plate is what evaluates, checks and arranges
        assert_eq!(s.evaluated().unwrap().iter().map(|o| o.id.as_str()).collect::<Vec<_>>(), vec!["c"]);
        assert_eq!(s.evaluated_all().unwrap().len(), 3);
        s.call("arrange", json!({}), Actor::User).unwrap();
        // naming an object on another plate goes there
        s.call("select", json!({"ids": ["a"]}), Actor::User).unwrap();
        assert_eq!(s.plate, 1);
        assert_eq!(s.call("select", json!({"ids": ["a", "c"]}), Actor::User).unwrap_err().code, "several_plates");
        s.call("measure", json!({"id": "c"}), Actor::User).unwrap();
        assert_eq!(s.plate, 2);
        // move, duplicate, rename, remove
        s.call("plate", json!({"action": "move", "ids": ["b"], "plate": 2}), Actor::User).unwrap();
        assert_eq!(s.project.objects.iter().filter(|o| o.plate == 2).count(), 2);
        let d = s.call("plate", json!({"action": "duplicate", "plate": 2}), Actor::User).unwrap();
        assert_eq!(s.plate, 3, "{}", d.text);
        assert_eq!(s.project.objects.iter().filter(|o| o.plate == 3).count(), 2);
        assert_eq!(s.project.plate_name(3), "Ghosts copy");
        s.call("plate", json!({"action": "rename", "plate": 1, "name": "Tiles"}), Actor::User).unwrap();
        s.call("plate", json!({"action": "remove", "plate": 2, "to": 1}), Actor::User).unwrap();
        assert_eq!(s.project.plate_count(), 2);
        assert_eq!(s.project.objects.iter().filter(|o| o.plate == 1).count(), 3, "plate 2's objects moved to plate 1");
        assert_eq!(s.project.plate_name(2), "Ghosts copy", "later plates move up");
        assert_eq!(s.plate, 2);
        let (text, data) = crate::describe::describe(&mut s).unwrap();
        assert!(text.contains("Plates: 1 'Tiles' (3 objects), ▸2 'Ghosts copy' (2 objects)"), "{text}");
        assert_eq!(data["plates"].as_array().unwrap().len(), 2);
        assert_eq!(s.call("plate", json!({"action": "select", "plate": 9}), Actor::User).unwrap_err().code, "no_such_plate");
        // undoing the plates away leaves a valid active plate
        while s.project.plate_count() > 1 && s.undo() {}
        let _ = s.call("plates", json!({}), Actor::User).unwrap();
        assert!(s.plate <= s.project.plate_count());
        // a project file without plates still loads, everything on plate 1
        let p: only3d_model::Project = serde_json::from_value(json!({"objects": [{"id": "x", "shape": {"type": "box", "size": [1, 1, 1]}}]})).unwrap();
        assert_eq!((p.objects[0].plate, p.plate_count()), (1, 1));
        assert!(!serde_json::to_string(&p).unwrap().contains("plate"), "single-plate files stay as before");
    }

    #[test]
    fn the_journal_says_which_objects_a_call_touched() {
        let mut s = Studio::new();
        s.call("add_object", json!({"id": "a", "shape": {"type": "box", "size": [10, 10, 5]}}), Actor::User).unwrap();
        s.call("add_object", json!({"id": "b", "shape": {"type": "box", "size": [10, 10, 5]}}), Actor::Agent).unwrap();
        assert_eq!(s.journal.last().unwrap().objects, vec!["b".to_string()]);
        s.call("update_object", json!({"id": "a", "position": [40, 40]}), Actor::User).unwrap();
        assert_eq!(s.journal.last().unwrap().objects, vec!["a".to_string()]);
        // read-only calls point at what they were about; unknown ids are dropped
        s.call("measure", json!({"id": "b"}), Actor::User).ok();
        assert_eq!(s.journal.last().unwrap().objects, vec!["b".to_string()]);
        s.call("select", json!({"ids": ["a", "b"]}), Actor::User).unwrap();
        assert_eq!(s.journal.last().unwrap().objects, vec!["a".to_string(), "b".to_string()]);
        let e = s.call("update_object", json!({"id": "zzz", "position": [1, 1]}), Actor::User).unwrap_err();
        let last = s.journal.last().unwrap();
        assert!(last.objects.is_empty() && !last.ok);
        assert!(last.detail.contains(&e.hint), "errors keep their hint for the details view");
    }

    #[test]
    fn agent_session_end_to_end() {
        let mut s = Studio::new();
        let out = s
            .call(
                "add_object",
                json!({"id": "bracket", "shape": {"type": "difference", "children": [
                    {"type": "box", "size": [60, 30, "t"], "radius": 2},
                    {"type": "cylinder", "d": 5.4, "h": 20, "at": [-20, 0, -1]},
                    {"type": "cylinder", "d": 5.4, "h": 20, "at": [20, 0, -1]}]}}),
                Actor::Agent,
            )
            .unwrap_err();
        assert_eq!(out.code, "bad_expression"); // param 't' not defined yet → rolled back
        assert!(s.project.objects.is_empty());
        s.call("set_params", json!({"params": {"t": 5}}), Actor::Agent).unwrap();
        let out = s
            .call(
                "add_object",
                json!({"id": "bracket", "render": true, "shape": {"type": "difference", "children": [
                    {"type": "box", "size": [60, 30, "t"], "radius": 2},
                    {"type": "cylinder", "d": 5.4, "h": 20, "at": [-20, 0, -1]},
                    {"type": "cylinder", "d": 5.4, "h": 20, "at": [20, 0, -1]}]}}),
                Actor::Agent,
            )
            .unwrap();
        assert!(out.text.contains("watertight"), "{}", out.text);
        assert_eq!(out.images.len(), 1);
        s.call("duplicate_object", json!({"id": "bracket", "copies": 3}), Actor::Agent).unwrap();
        let d = s.call("describe", json!({}), Actor::Agent).unwrap();
        assert!(d.text.contains("bracket_4"), "{}", d.text);
        let sl = s.call("slice", json!({"preview": true}), Actor::Agent).unwrap();
        assert!(sl.text.contains("layers"), "{}", sl.text);
        assert_eq!(sl.images.len(), 1);
        let sec = s.call("section", json!({"id": "bracket", "z": 2.5}), Actor::Agent).unwrap();
        assert!(sec.text.contains("2 hole(s)"), "{}", sec.text);
        s.call("undo", json!({}), Actor::Agent).unwrap();
        assert_eq!(s.project.objects.len(), 1);
        let err = s.call("set_process", json!({"overrides": {"infil_density": 0.2}}), Actor::Agent).unwrap_err();
        assert_eq!(err.code, "unknown_setting");
        assert!(Registry::builtin().manifest().as_array().unwrap().len() >= 30);
        // a failed batch must not leave stale caches behind
        let rev = s.revision;
        let objs_before = s.evaluated().unwrap().len();
        let e = s.call("batch", json!({"calls": [{"tool": "remove_object", "args": {"ids": ["bracket"]}}, {"tool": "describe"}, {"tool": "update_object", "args": {"id": "missing"}}]}), Actor::Agent).unwrap_err();
        assert_eq!(e.code, "not_found");
        assert!(s.revision > rev);
        assert_eq!(s.evaluated().unwrap().len(), objs_before);
    }
}
