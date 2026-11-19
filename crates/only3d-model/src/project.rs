//! The project document: parameters, printer/process/filament selection and the objects on the
//! plate. Serialised as `*.only3d.json` — plain JSON, diffable, written by humans and agents alike.

use std::collections::BTreeMap;
use std::sync::Arc;

use glam::{Mat4, Quat, Vec3};
use only3d_geom::{Aabb, Mesh, MeshReport};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::expr::{Num, Params};
use crate::shape::{euler_xyz, quat_to_euler_xyz, Evaluator, Node};
use crate::ModelError;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Project {
    #[serde(default = "one")]
    pub version: u32,
    /// Stable identity of this design (prints and filament usage are traced back to it).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// Named numbers usable in any shape expression, e.g. {"wall": 2.4}.
    #[serde(default)]
    pub params: Params,
    #[serde(default)]
    pub printer: PrinterSel,
    #[serde(default)]
    pub process: ProcessSel,
    /// Filament slots, 1-based in `Object::filament`.
    #[serde(default = "default_filaments")]
    pub filaments: Vec<FilamentSlot>,
    #[serde(default)]
    pub objects: Vec<Object>,
    /// Build plates, 1-based in `Object::plate`. Empty means a single, unnamed plate; objects
    /// may name a plate past the end (it then exists with a default name).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plates: Vec<PlateInfo>,
    /// Checks that belong to this design ("the lid fits the box with 0.3 mm", "walls ≥ 1.6 mm"),
    /// run after every change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub specs: Vec<Spec>,
    /// Plates waiting to be printed, in order. A plate leaves the queue when its print starts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queue: Vec<u32>,
    /// How the printed parts go together, with the hardware that isn't printed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assembly: Option<Assembly>,
}

/// How the printed parts go together, with hardware that is not printed (a parachute, a motor,
/// bearings, a board). `render {"assembly": true}` draws it; nothing here is placed on a plate,
/// sliced or checked for printing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Assembly {
    /// Printed objects in their assembled place (one object may be placed several times).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<AssemblyPart>,
    /// Reference hardware: drawn with the parts, never printed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reference: Vec<ReferencePart>,
}

/// A printed object in the assembly: the object in its own modelling frame (its shape's
/// coordinates, with its scale but not the turn it prints with) turned by `rotate`, then moved
/// by `at`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AssemblyPart {
    /// Object id.
    pub object: String,
    /// Move by [x, y, z] mm, after turning (numbers or expressions over params).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<[Num; 3]>,
    /// Turn by [x, y, z] degrees (X first, then Y, then Z).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotate: Option<[Num; 3]>,
}

/// A piece of bought hardware shown in the assembly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReferencePart {
    /// "parachute", "motor 18x70".
    pub name: String,
    /// Its shape in the add_object grammar (an `import` for a vendor model), in assembly
    /// coordinates.
    pub shape: Node,
    /// "#rrggbb" (default steel grey).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// A design check saved with the project.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Spec {
    #[serde(default)]
    pub id: String,
    /// size (along `axis`) | clearance (`object` to `other`, as placed) | wall (at `z`) |
    /// overhang (mm²); within `min`/`max`
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axis: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// A body of a multi-part object with its own filament.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Part {
    #[serde(default)]
    pub name: String,
    pub shape: Node,
    /// 1-based filament slot.
    #[serde(default = "default_slot")]
    pub filament: u32,
}

/// One build plate: a name, and (later) its own settings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlateInfo {
    /// Shown in the plate tabs and the workspace tree ("Ghosts", "Plate 2" when empty).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
}

fn one() -> u32 {
    1
}

fn is_one(v: &u32) -> bool {
    *v == 1
}

/// A fresh project id, e.g. "prj-3f9a1c2e7b40".
pub fn new_project_id() -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0).hash(&mut h);
    std::process::id().hash(&mut h);
    std::thread::current().id().hash(&mut h);
    format!("prj-{:012x}", h.finish() & 0xffff_ffff_ffff)
}

impl Default for Project {
    fn default() -> Self {
        Project {
            version: 1,
            id: new_project_id(),
            name: "untitled".into(),
            params: Params::new(),
            printer: PrinterSel::default(),
            process: ProcessSel::default(),
            filaments: default_filaments(),
            objects: Vec::new(),
            plates: Vec::new(),
            specs: Vec::new(),
            queue: Vec::new(),
            assembly: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PrinterSel {
    /// Bambu Studio machine preset name.
    #[serde(default = "default_machine")]
    pub machine: String,
    /// "Textured PEI Plate", "Cool Plate", "Engineering Plate", "High Temp Plate", "Smooth PEI Plate".
    #[serde(default = "default_bed_type")]
    pub bed_type: String,
}

impl Default for PrinterSel {
    fn default() -> Self {
        Self { machine: default_machine(), bed_type: default_bed_type() }
    }
}

fn default_machine() -> String {
    "Bambu Lab H2D 0.4 nozzle".into()
}
fn default_bed_type() -> String {
    "Textured PEI Plate".into()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProcessSel {
    /// Bambu Studio process preset name.
    #[serde(default = "default_process")]
    pub preset: String,
    /// only3d process overrides (see the slicer's `SliceSettings`), e.g. {"layer_height": 0.16, "infill_density": 0.25}.
    #[serde(default)]
    pub overrides: BTreeMap<String, serde_json::Value>,
}

impl Default for ProcessSel {
    fn default() -> Self {
        Self { preset: default_process(), overrides: BTreeMap::new() }
    }
}

fn default_process() -> String {
    "0.20mm Standard @BBL H2D".into()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NozzleSel {
    #[default]
    Auto,
    Left,
    Right,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FilamentSlot {
    /// Bambu Studio filament preset name, e.g. "Bambu PLA Basic @BBL H2D".
    pub preset: String,
    /// Display colour "#RRGGBB".
    #[serde(default = "default_color")]
    pub color: String,
    #[serde(default)]
    pub nozzle: NozzleSel,
    /// Inventory spool id loaded for this slot (see the spool tools); optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spool: Option<String>,
}

fn default_color() -> String {
    "#D9CBB5".into()
}

fn default_filaments() -> Vec<FilamentSlot> {
    vec![FilamentSlot { preset: "Bambu PLA Basic @BBL H2D".into(), color: default_color(), nozzle: NozzleSel::Auto, spool: None }]
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Object {
    /// Stable id (unique in the project), e.g. "bracket".
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub shape: Node,
    /// Where the centre of the object's footprint sits on the plate [x, y] (mm). Objects always
    /// rest on the bed. `null` = let `arrange` place it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f32; 2]>,
    /// Orientation on the plate, degrees about X, Y, Z (applied before placement).
    #[serde(default, skip_serializing_if = "is_zero3")]
    pub rotation: [f32; 3],
    /// Uniform or per-axis scale of the finished shape.
    #[serde(default = "ones", skip_serializing_if = "is_one3")]
    pub scale: [f32; 3],
    /// 1-based filament slot.
    #[serde(default = "default_slot")]
    pub filament: u32,
    /// 1-based build plate the object sits on.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub plate: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    /// Locked objects stay where they are: the window won't drag or nudge them and `arrange`
    /// leaves them in place.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
    /// More bodies that belong to this object, each in its own filament (a multi-colour print:
    /// the pink lip of an orchid, a text inlay). Same coordinates as `shape`; they move, turn
    /// and scale with the object.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<Part>,
    /// Per-object slicer overrides, e.g. {"infill_density": 0.4, "wall_loops": 4}.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub settings: BTreeMap<String, serde_json::Value>,
}

fn is_zero3(v: &[f32; 3]) -> bool {
    v.iter().all(|x| *x == 0.0)
}
fn is_one3(v: &[f32; 3]) -> bool {
    v.iter().all(|x| *x == 1.0)
}
fn ones() -> [f32; 3] {
    [1.0; 3]
}
fn default_slot() -> u32 {
    1
}

impl Object {
    pub fn new(id: impl Into<String>, shape: Node) -> Self {
        let id = id.into();
        Object { name: id.clone(), id, shape, position: None, rotation: [0.0; 3], scale: [1.0; 3], filament: 1, plate: 1, hidden: false, locked: false, parts: Vec::new(), settings: BTreeMap::new() }
    }

    /// Orientation + scale (no placement).
    pub fn local_transform(&self) -> Mat4 {
        Mat4::from_quat(euler_xyz(Vec3::from(self.rotation))) * Mat4::from_scale(Vec3::from(self.scale))
    }
}

/// The printable volume. Defaults to the Bambu Lab H2D.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Bed {
    /// [x, y] mm of the whole plate.
    pub size: [f32; 2],
    pub max_height: f32,
    /// Reachable XY rectangle per nozzle [x0, y0, x1, y1], index 0 = left nozzle, 1 = right.
    pub nozzle_areas: Vec<[f32; 4]>,
    /// The nozzle a single filament prints with when the plate lets it choose (the H2D's right).
    #[serde(default)]
    pub main_nozzle: usize,
    /// Areas nothing may print in, as polygons (even-odd, as the printer profile's
    /// `bed_exclude_area`): the X1 and P1's front-left corner where they wipe the nozzle, the
    /// Kobra 3's 2 mm rim.
    #[serde(default)]
    pub exclude: Vec<Vec<[f32; 2]>>,
    /// Where the plate's corner (only3d's 0,0) is in the printer's own coordinates: a delta's
    /// round bed is centred on 0,0, so its corner is at (-r, -r). G-code adds it to every
    /// position; everything else in only3d works from the corner.
    #[serde(default)]
    pub origin: [f32; 2],
    /// The printable outline when it isn't the whole rectangle (a delta's circle).
    #[serde(default)]
    pub shape: Vec<[f32; 2]>,
}

impl Default for Bed {
    fn default() -> Self {
        Bed::h2d()
    }
}

impl Bed {
    /// Bambu Lab H2D: 350 x 320 plate; the left nozzle reaches x 0..325, the right x 25..350.
    pub fn h2d() -> Self {
        Bed { size: [350.0, 320.0], max_height: 325.0, nozzle_areas: vec![[0.0, 0.0, 325.0, 320.0], [25.0, 0.0, 350.0, 320.0]], main_nozzle: 1, exclude: Vec::new(), origin: [0.0, 0.0], shape: Vec::new() }
    }

    pub fn center(&self) -> [f32; 2] {
        [self.size[0] / 2.0, self.size[1] / 2.0]
    }

    /// Whether footprint [x0, y0, x1, y1] lies inside the printable outline (always, on a
    /// rectangular plate; the plate's own bounds are checked apart).
    pub fn within_shape(&self, r: [f32; 4]) -> bool {
        self.shape.len() < 3 || [[r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]]].iter().all(|c| point_in_poly(&self.shape, c[0], c[1]))
    }

    /// The bounds of the excluded area a footprint [x0, y0, x1, y1] reaches into, if any.
    pub fn excluded(&self, r: [f32; 4]) -> Option<[f32; 4]> {
        self.exclude.iter().find(|p| poly_hits_rect(p, r)).map(|p| {
            p.iter().fold([f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY], |b, q| [b[0].min(q[0]), b[1].min(q[1]), b[2].max(q[0]), b[3].max(q[1])])
        })
    }
}

/// Whether point (x, y) is inside polygon `poly` (even-odd).
fn point_in_poly(poly: &[[f32; 2]], x: f32, y: f32) -> bool {
    (0..poly.len()).filter(|&i| {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        (a[1] > y) != (b[1] > y) && x < a[0] + (y - a[1]) * (b[0] - a[0]) / (b[1] - a[1])
    }).count() % 2 == 1
}

/// Whether polygon `poly` (even-odd fill) overlaps rectangle `r` [x0, y0, x1, y1] by more than
/// a touch.
fn poly_hits_rect(poly: &[[f32; 2]], r: [f32; 4]) -> bool {
    let r = [r[0] + 0.01, r[1] + 0.01, r[2] - 0.01, r[3] - 0.01];
    if poly.len() < 3 || r[0] >= r[2] || r[1] >= r[3] {
        return false;
    }
    let edges = || (0..poly.len()).map(|i| (poly[i], poly[(i + 1) % poly.len()]));
    // a corner of the polygon inside the rectangle
    if poly.iter().any(|p| p[0] > r[0] && p[0] < r[2] && p[1] > r[1] && p[1] < r[3]) {
        return true;
    }
    // a corner of the rectangle inside the polygon
    let inside = |x: f32, y: f32| edges().filter(|(a, b)| (a[1] > y) != (b[1] > y) && x < a[0] + (y - a[1]) * (b[0] - a[0]) / (b[1] - a[1])).count() % 2 == 1;
    let corners = [[r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]]];
    if corners.iter().any(|c| inside(c[0], c[1])) {
        return true;
    }
    // or their edges cross
    let cross = |p: [f32; 2], q: [f32; 2], a: [f32; 2], b: [f32; 2]| {
        let o = |u: [f32; 2], v: [f32; 2], w: [f32; 2]| (v[0] - u[0]) * (w[1] - u[1]) - (v[1] - u[1]) * (w[0] - u[0]);
        let (d1, d2, d3, d4) = (o(a, b, p), o(a, b, q), o(p, q, a), o(p, q, b));
        (d1 > 0.0) != (d2 > 0.0) && (d3 > 0.0) != (d4 > 0.0)
    };
    edges().any(|(a, b)| (0..4).any(|i| cross(corners[i], corners[(i + 1) % 4], a, b)))
}

/// An object after evaluation and placement.
#[derive(Clone, Debug)]
pub struct EvaluatedObject {
    pub id: String,
    pub name: String,
    pub filament: u32,
    pub plate: u32,
    /// For multi-part objects: every body (the main shape first) placed in the world, with its
    /// filament. Empty for single-body objects; `mesh` is always all of them together.
    pub parts: Vec<EvaluatedPart>,
    /// World-space mesh, resting on the bed at its plate position.
    pub mesh: Arc<Mesh>,
    pub report: MeshReport,
    pub hidden: bool,
}

/// One body of a multi-part object, placed in the world.
#[derive(Clone, Debug)]
pub struct EvaluatedPart {
    pub name: String,
    pub filament: u32,
    pub mesh: Arc<Mesh>,
}

impl EvaluatedObject {
    /// Every filament this object prints with.
    pub fn filaments(&self) -> Vec<u32> {
        let mut f: Vec<u32> = if self.parts.is_empty() { vec![self.filament] } else { self.parts.iter().map(|p| p.filament).collect() };
        f.sort();
        f.dedup();
        f
    }
}

impl Project {
    pub fn object(&self, id: &str) -> Option<&Object> {
        self.objects.iter().find(|o| o.id == id)
    }

    pub fn object_mut(&mut self, id: &str) -> Option<&mut Object> {
        self.objects.iter_mut().find(|o| o.id == id)
    }

    /// A fresh unique id derived from `base` ("box", "box_2", ...).
    pub fn unique_id(&self, base: &str) -> String {
        let base: String = base.chars().map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c.to_ascii_lowercase() } else { '_' }).collect();
        let base = if base.is_empty() { "object".to_string() } else { base };
        if self.object(&base).is_none() {
            return base;
        }
        (2..).map(|i| format!("{base}_{i}")).find(|c| self.object(c).is_none()).unwrap()
    }

    /// Evaluate one object to a placed world mesh.
    pub fn evaluate_object(&self, o: &Object, ev: &mut Evaluator, bed: &Bed) -> Result<EvaluatedObject, ModelError> {
        let local = ev.eval(&o.shape, &self.params).map_err(|e| ModelError::At { path: format!("objects[{}].shape", o.id), source: Box::new(e) })?;
        if !o.parts.is_empty() {
            return self.evaluate_parts(o, local, ev, bed);
        }
        let pos = o.position.unwrap_or_else(|| bed.center());
        let key = (
            Arc::as_ptr(&local) as usize,
            [o.rotation[0].to_bits(), o.rotation[1].to_bits(), o.rotation[2].to_bits(), o.scale[0].to_bits(), o.scale[1].to_bits(), o.scale[2].to_bits(), pos[0].to_bits(), pos[1].to_bits()],
        );
        if let Some((_, world, report)) = ev.placed.get(&key) {
            return Ok(EvaluatedObject { id: o.id.clone(), name: o.name.clone(), filament: o.filament, plate: o.plate, parts: Vec::new(), mesh: world.clone(), report: report.clone(), hidden: o.hidden });
        }
        let mut m = local.transformed(&o.local_transform());
        let b = m.bounds();
        m.translate(Vec3::new(pos[0] - (b.min.x + b.max.x) / 2.0, pos[1] - (b.min.y + b.max.y) / 2.0, -b.min.z));
        // topology and volume don't change under rotation/translation: reuse the analysis of the
        // same local mesh at the same scale when only the placement changed
        let reuse = ev.placed.iter().find(|((p, k), _)| *p == key.0 && k[3..6] == key.1[3..6]).map(|(_, v)| v.2.clone());
        let report = match reuse {
            Some(mut r) => {
                r.bounds = m.bounds();
                r.size = r.bounds.size();
                r
            }
            None => m.analyze(),
        };
        let world = Arc::new(m);
        if ev.placed.len() > 256 {
            ev.placed.clear();
        }
        ev.placed.insert(key, (local, world.clone(), report.clone()));
        Ok(EvaluatedObject { id: o.id.clone(), name: o.name.clone(), filament: o.filament, plate: o.plate, parts: Vec::new(), mesh: world, report, hidden: o.hidden })
    }

    /// A multi-part object: every body gets the object's orientation and scale, then one shared
    /// placement (the footprint of all of them centred on `position`, resting on the bed).
    fn evaluate_parts(&self, o: &Object, main: Arc<Mesh>, ev: &mut Evaluator, bed: &Bed) -> Result<EvaluatedObject, ModelError> {
        let mut locals = vec![(if o.name.is_empty() { o.id.clone() } else { o.name.clone() }, o.filament, main)];
        for (k, p) in o.parts.iter().enumerate() {
            let l = ev.eval(&p.shape, &self.params).map_err(|e| ModelError::At { path: format!("objects[{}].parts[{k}].shape", o.id), source: Box::new(e) })?;
            locals.push((if p.name.is_empty() { format!("part {}", k + 1) } else { p.name.clone() }, p.filament, l));
        }
        let xf = o.local_transform();
        let mut pieces: Vec<Mesh> = locals.iter().map(|(_, _, l)| l.transformed(&xf)).collect();
        let mut bb = Aabb::EMPTY;
        for m in &pieces {
            let b = m.bounds();
            if b.min.x.is_finite() {
                bb.extend(b.min);
                bb.extend(b.max);
            }
        }
        let pos = o.position.unwrap_or_else(|| bed.center());
        let shift = Vec3::new(pos[0] - (bb.min.x + bb.max.x) / 2.0, pos[1] - (bb.min.y + bb.max.y) / 2.0, -bb.min.z);
        let mut all = Mesh::new();
        for m in &mut pieces {
            m.translate(shift);
            all.append(m);
        }
        // each body is checked on its own: bodies touching each other are not a defect
        let scale = [o.scale[0].to_bits(), o.scale[1].to_bits(), o.scale[2].to_bits()];
        let bounds = all.bounds();
        let mut report = MeshReport {
            triangles: 0,
            vertices: 0,
            bounds,
            size: bounds.size(),
            volume_mm3: 0.0,
            area_mm2: 0.0,
            watertight: true,
            boundary_edges: 0,
            non_manifold_edges: 0,
            flipped_edges: 0,
            degenerate_triangles: 0,
            components: 0,
            inverted: false,
        };
        for ((_, _, local), m) in locals.iter().zip(&pieces) {
            let key = (Arc::as_ptr(local) as usize, scale);
            let r = match ev.part_reports.get(&key) {
                Some((_, r)) => r.clone(),
                None => {
                    let r = m.analyze();
                    if ev.part_reports.len() > 512 {
                        ev.part_reports.clear();
                    }
                    ev.part_reports.insert(key, (local.clone(), r.clone()));
                    r
                }
            };
            report.triangles += r.triangles;
            report.vertices += r.vertices;
            report.volume_mm3 += r.volume_mm3;
            report.area_mm2 += r.area_mm2;
            report.watertight &= r.watertight;
            report.boundary_edges += r.boundary_edges;
            report.non_manifold_edges += r.non_manifold_edges;
            report.flipped_edges += r.flipped_edges;
            report.degenerate_triangles += r.degenerate_triangles;
            report.components += r.components;
            report.inverted |= r.inverted;
        }
        report.size = report.bounds.size();
        let parts = locals.into_iter().zip(pieces).map(|((name, filament, _), m)| EvaluatedPart { name, filament, mesh: Arc::new(m) }).collect();
        Ok(EvaluatedObject { id: o.id.clone(), name: o.name.clone(), filament: o.filament, plate: o.plate, parts, mesh: Arc::new(all), report, hidden: o.hidden })
    }

    pub fn evaluate(&self, ev: &mut Evaluator, bed: &Bed) -> Result<Vec<EvaluatedObject>, ModelError> {
        self.objects.iter().map(|o| self.evaluate_object(o, ev, bed)).collect()
    }

    /// Number of plates: the named ones, or as many as objects use (at least one).
    pub fn plate_count(&self) -> u32 {
        (self.plates.len() as u32).max(self.objects.iter().map(|o| o.plate).max().unwrap_or(1)).max(1)
    }

    /// A plate's name, "Plate n" when it has none.
    pub fn plate_name(&self, plate: u32) -> String {
        self.plates.get(plate as usize - 1).map(|p| p.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| format!("Plate {plate}"))
    }

    /// Make `plates` hold an entry for every plate up to `n`.
    pub fn ensure_plates(&mut self, n: u32) {
        while (self.plates.len() as u32) < n {
            self.plates.push(PlateInfo::default());
        }
    }

    /// Pack the visible objects of one plate (rows, spacing in mm), centred as a block. Returns
    /// the ids that did not fit.
    pub fn arrange(&mut self, ev: &mut Evaluator, bed: &Bed, spacing: f32, plate: u32) -> Result<Vec<String>, ModelError> {
        let mut items: Vec<(usize, Vec3)> = Vec::new();
        for (i, o) in self.objects.iter().enumerate() {
            if o.hidden || o.locked || o.plate != plate {
                continue;
            }
            let local = ev.eval(&o.shape, &self.params)?;
            let size = local.transformed(&o.local_transform()).bounds().size();
            items.push((i, size));
        }
        // tallest-footprint first
        items.sort_by(|a, b| b.1.y.total_cmp(&a.1.y).then(b.1.x.total_cmp(&a.1.x)));
        let area = bed.nozzle_areas.iter().fold([0.0f32, 0.0, bed.size[0], bed.size[1]], |a, n| [a[0].max(n[0]), a[1].max(n[1]), a[2].min(n[2]), a[3].min(n[3])]);
        let (w, h) = (area[2] - area[0] - 2.0 * spacing, area[3] - area[1] - 2.0 * spacing);
        let mut placed: Vec<(usize, [f32; 2], Vec3)> = Vec::new();
        let mut failed = Vec::new();
        let (mut x, mut y, mut row_h) = (0.0f32, 0.0f32, 0.0f32);
        for (i, s) in items {
            if s.x > w || s.y > h {
                failed.push(self.objects[i].id.clone());
                continue;
            }
            if x + s.x > w {
                x = 0.0;
                y += row_h + spacing;
                row_h = 0.0;
            }
            if y + s.y > h {
                failed.push(self.objects[i].id.clone());
                continue;
            }
            placed.push((i, [x + s.x / 2.0, y + s.y / 2.0], s));
            x += s.x + spacing;
            row_h = row_h.max(s.y);
        }
        // centre the block on the shared printable area
        let mut bb = Aabb::EMPTY;
        for (_, c, s) in &placed {
            bb.extend(Vec3::new(c[0] - s.x / 2.0, c[1] - s.y / 2.0, 0.0));
            bb.extend(Vec3::new(c[0] + s.x / 2.0, c[1] + s.y / 2.0, 0.0));
        }
        let off = [(area[0] + area[2]) / 2.0 - bb.center().x, (area[1] + area[3]) / 2.0 - bb.center().y];
        for (i, c, s) in placed {
            let at = [c[0] + off[0], c[1] + off[1]];
            // a full plate can reach the printer's excluded corner: that object doesn't fit
            if bed.excluded([at[0] - s.x / 2.0, at[1] - s.y / 2.0, at[0] + s.x / 2.0, at[1] + s.y / 2.0]).is_some() {
                failed.push(self.objects[i].id.clone());
                continue;
            }
            self.objects[i].position = Some([round2(at[0]), round2(at[1])]);
        }
        Ok(failed)
    }
}

fn round2(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

/// Pick the orientation that minimises support (overhang area) and maximises bed contact.
/// Returns the new rotation (degrees) and a score breakdown.
pub fn best_orientation(mesh: &Mesh, current: [f32; 3]) -> (OrientResult, Vec<OrientResult>) {
    let base = euler_xyz(Vec3::from(current));
    let mut candidates: Vec<Quat> = vec![
        Quat::IDENTITY,
        Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
        Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
        Quat::from_rotation_x(std::f32::consts::PI),
        Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
        Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2),
    ];
    // largest flat faces of the hull as candidate bottoms
    if let Some(h) = only3d_geom::hull::convex_hull(&mesh.positions) {
        let mut faces: Vec<(f32, Vec3)> = (0..h.indices.len())
            .map(|i| {
                let [a, b, c] = h.triangle(i);
                let cr = (b - a).cross(c - a);
                (cr.length() * 0.5, cr.normalize_or_zero())
            })
            .collect();
        // merge coplanar hull triangles by normal
        faces.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut groups: Vec<(f32, Vec3)> = Vec::new();
        for (a, n) in faces {
            if let Some(g) = groups.iter_mut().find(|g| g.1.dot(n) > 0.9995) {
                g.0 += a;
            } else {
                groups.push((a, n));
            }
        }
        groups.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, n) in groups.into_iter().take(8) {
            candidates.push(Quat::from_rotation_arc(n, -Vec3::Z));
        }
    }
    let mut results: Vec<OrientResult> = candidates
        .into_iter()
        .map(|q| {
            let rot = q * base;
            let m = mesh.transformed(&Mat4::from_quat(q));
            let overhang = m.overhang_area(45.0);
            let contact = m.bed_contact_area();
            let height = m.bounds().size().z as f64;
            let score = overhang - 0.5 * contact + 0.05 * height;
            let e = quat_to_euler_xyz(rot);
            OrientResult { rotation: [snap(e.x), snap(e.y), snap(e.z)], overhang_mm2: overhang, contact_mm2: contact, height_mm: height, score }
        })
        .collect();
    results.sort_by(|a, b| a.score.total_cmp(&b.score));
    results.dedup_by(|a, b| (a.score - b.score).abs() < 1e-6);
    (results[0].clone(), results)
}

fn snap(v: f32) -> f32 {
    let r = (v * 100.0).round() / 100.0;
    if r == -0.0 {
        0.0
    } else {
        r
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct OrientResult {
    pub rotation: [f32; 3],
    pub overhang_mm2: f64,
    pub contact_mm2: f64,
    pub height_mm: f64,
    pub score: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// One printability finding. `code` is stable for agents; `hint` says what to do.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Finding {
    pub severity: Severity,
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    pub message: String,
    pub hint: String,
    /// What the slice already does about it (a brim, supports): the finding stays listed, as Info.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mitigation: Option<String>,
}

/// Printability checks on evaluated objects.
pub fn check(project: &Project, objs: &[EvaluatedObject], bed: &Bed) -> Vec<Finding> {
    let mut f = Vec::new();
    let mut push = |s, code: &str, o: Option<&str>, m: String, h: &str| {
        f.push(Finding { severity: s, code: code.into(), object: o.map(String::from), message: m, hint: h.into(), mitigation: None })
    };
    // an empty plate is a starting point, not a fault (slicing it still says nothing_to_slice)
    if objs.is_empty() {
        push(Severity::Info, "empty_plate", None, "no objects yet: the plate is ready to start".into(), "add an object (add_object) or import one (import_file)");
    } else if objs.iter().all(|o| o.hidden) {
        push(Severity::Warning, "all_hidden", None, format!("all {} objects on this plate are hidden, so nothing would print", objs.len()), "unhide one (update_object with hidden: false)");
    }
    for o in objs.iter().filter(|o| !o.hidden) {
        let r = &o.report;
        let id = Some(o.id.as_str());
        if r.triangles == 0 {
            push(Severity::Error, "empty_mesh", id, format!("'{}' has no geometry", o.id), "check the shape: a difference may have removed everything");
            continue;
        }
        if !r.watertight {
            push(
                Severity::Warning,
                "not_watertight",
                id,
                format!("'{}' is not watertight ({} open edges, {} non-manifold, {} flipped)", o.id, r.boundary_edges, r.non_manifold_edges, r.flipped_edges),
                "the slicer may misread walls; rebuild from primitives or repair the source mesh",
            );
        }
        if r.components > 1 {
            push(Severity::Info, "multiple_bodies", id, format!("'{}' consists of {} separate bodies", o.id, r.components), "fine if intended; otherwise check that parts overlap");
        }
        let b = r.bounds;
        if b.min.x < 0.0 || b.min.y < 0.0 || b.max.x > bed.size[0] || b.max.y > bed.size[1] {
            push(Severity::Error, "off_plate", id, format!("'{}' extends outside the {}x{} mm plate (bounds x {:.1}..{:.1}, y {:.1}..{:.1})", o.id, bed.size[0], bed.size[1], b.min.x, b.max.x, b.min.y, b.max.y), "move it (set position) or run arrange");
        }
        if !bed.within_shape([b.min.x, b.min.y, b.max.x, b.max.y]) {
            push(Severity::Error, "off_plate", id, format!("'{}' reaches past the edge of the printer's round plate", o.id), "move it towards the middle, or scale it down");
        }
        if let Some(e) = bed.excluded([b.min.x, b.min.y, b.max.x, b.max.y]) {
            push(Severity::Error, "excluded_area", id, format!("'{}' reaches into an area the printer keeps clear (within x {:.0}..{:.0}, y {:.0}..{:.0} mm; on the X1 and P1 the corner where it wipes the nozzle)", o.id, e[0], e[2], e[1], e[3]), "move it (set position) or run arrange");
        }
        if b.max.z > bed.max_height {
            push(Severity::Error, "too_tall", id, format!("'{}' is {:.1} mm tall, max {:.0} mm", o.id, b.max.z, bed.max_height), "rotate it to lie flat or scale it down");
        }
        let slot = project.filaments.get(o.filament as usize - 1);
        if slot.is_none() {
            push(Severity::Error, "bad_filament_slot", id, format!("'{}' uses filament slot {} but the project has {}", o.id, o.filament, project.filaments.len()), "set the object's filament to an existing slot");
        }
        // nozzle reach on dual-nozzle machines
        if bed.nozzle_areas.len() > 1 {
            let reach: Vec<usize> = bed.nozzle_areas.iter().enumerate().filter(|(_, a)| b.min.x >= a[0] && b.max.x <= a[2] && b.min.y >= a[1] && b.max.y <= a[3]).map(|(i, _)| i).collect();
            let wanted = slot.map(|s| s.nozzle).unwrap_or_default();
            let ok = match wanted {
                NozzleSel::Auto => !reach.is_empty(),
                NozzleSel::Left => reach.contains(&0),
                NozzleSel::Right => reach.contains(&1),
            };
            if !ok {
                push(Severity::Error, "nozzle_cannot_reach", id, format!("'{}' (x {:.1}..{:.1}) is outside the area the {:?} nozzle can reach", o.id, b.min.x, b.max.x, wanted), "move it towards the plate centre (left nozzle: x 0-325, right: x 25-350)");
            }
        }
        let overhang = o.mesh.overhang_area(45.0);
        if overhang > 25.0 {
            push(Severity::Warning, "needs_support", id, format!("'{}' has {:.0} mm² of overhang steeper than 45°", o.id, overhang), "run orient to find a better orientation, or enable supports: set_process overrides {\"support\": true}");
        }
        let contact = o.mesh.bed_contact_area();
        if contact < 1.0 {
            push(Severity::Error, "no_bed_contact", id, format!("'{}' touches the bed with almost no flat area ({contact:.1} mm²)", o.id), "orient it onto a flat face, cut a flat bottom, or add supports/brim");
        } else if (b.max.z as f64) > 4.0 * contact.sqrt() && contact < 400.0 {
            push(Severity::Warning, "tip_over_risk", id, format!("'{}' is tall ({:.0} mm) for its {:.0} mm² footprint", o.id, b.max.z, contact), "use a brim (brim_width 5) or orient it lower");
        }
    }
    // overlaps between objects
    let vis: Vec<&EvaluatedObject> = objs.iter().filter(|o| !o.hidden).collect();
    for i in 0..vis.len() {
        for j in i + 1..vis.len() {
            let (a, b) = (vis[i].report.bounds, vis[j].report.bounds);
            let gap_x = (b.min.x - a.max.x).max(a.min.x - b.max.x);
            let gap_y = (b.min.y - a.max.y).max(a.min.y - b.max.y);
            if gap_x < 0.0 && gap_y < 0.0 {
                push(Severity::Warning, "objects_overlap", Some(&vis[i].id), format!("'{}' and '{}' overlap on the plate", vis[i].id, vis[j].id), "run arrange or move one of them");
            }
        }
    }
    f.sort_by(|a, b| b.severity.cmp(&a.severity));
    f
}
