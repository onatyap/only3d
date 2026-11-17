//! The parametric shape tree. JSON-native so agents can read, write and diff it directly.
//!
//! Conventions (keep these in mind when writing shapes by hand):
//! * millimetres, Z up;
//! * every primitive is centred on X/Y and rests on Z=0 at its local origin (`center: true`
//!   centres it on Z as well);
//! * node transforms apply in the order scale → rotate (degrees, X then Y then Z) → mirror → `at`.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use glam::{EulerRot, Mat4, Quat, Vec2, Vec3};
use only3d_geom::shapes::{self, EdgeFinish};
use only3d_geom::{Aabb, Mesh};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::csg::{self, BoolOp};
use crate::expr::{Num, Params};
use crate::ModelError;

/// A shape plus an optional local transform.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Node {
    #[serde(flatten)]
    pub shape: Shape,
    /// Move by [x, y, z] mm (applied last).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<[Num; 3]>,
    /// Rotate by [x, y, z] degrees (X first, then Y, then Z).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotate: Option<[Num; 3]>,
    /// Scale factors [x, y, z].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<[Num; 3]>,
    /// Mirror across the plane through the local origin with this normal, e.g. [1, 0, 0].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror: Option<[f32; 3]>,
    /// Optional human name for this sub-part ("screw hole", "lid lip").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl From<Shape> for Node {
    fn from(shape: Shape) -> Self {
        Node { shape, at: None, rotate: None, scale: None, mirror: None, label: None }
    }
}

impl Node {
    pub fn at(mut self, x: f64, y: f64, z: f64) -> Self {
        self.at = Some([x.into(), y.into(), z.into()]);
        self
    }

    /// The nodes directly inside this one.
    pub fn children(&self) -> Vec<&Node> {
        match &self.shape {
            Shape::Union { children } | Shape::Difference { children } | Shape::Intersection { children } | Shape::Hull { children } => children.iter().collect(),
            Shape::Array { child, .. } | Shape::Polar { child, .. } | Shape::Shell { child, .. } | Shape::PushPull { child, .. } => vec![&**child],
            _ => Vec::new(),
        }
    }

    pub fn children_mut(&mut self) -> Vec<&mut Node> {
        match &mut self.shape {
            Shape::Union { children } | Shape::Difference { children } | Shape::Intersection { children } | Shape::Hull { children } => children.iter_mut().collect(),
            Shape::Array { child, .. } | Shape::Polar { child, .. } | Shape::Shell { child, .. } | Shape::PushPull { child, .. } => vec![&mut **child],
            _ => Vec::new(),
        }
    }

    /// This node and every node inside it, depth first.
    pub fn visit(&self, f: &mut dyn FnMut(&Node)) {
        f(self);
        for c in self.children() {
            c.visit(f);
        }
    }

    pub fn visit_mut(&mut self, f: &mut dyn FnMut(&mut Node)) {
        f(self);
        for c in self.children_mut() {
            c.visit_mut(f);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Shape {
    /// Box of `size` [x, y, z]. `radius` rounds all edges, `chamfer` bevels them 45°.
    /// `corner_radius` rounds only the 4 vertical edges (trays, lids), with optional
    /// `chamfer_top`/`chamfer_bottom`/`fillet_top`/`fillet_bottom`.
    Box {
        size: [Num; 3],
        #[serde(default, skip_serializing_if = "Option::is_none")]
        radius: Option<Num>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chamfer: Option<Num>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        corner_radius: Option<Num>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chamfer_top: Option<Num>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chamfer_bottom: Option<Num>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fillet_top: Option<Num>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fillet_bottom: Option<Num>,
        #[serde(default, skip_serializing_if = "is_false")]
        center: bool,
    },
    /// Cylinder (or cone/frustum with `d2`). Give `d` or `r`, and height `h`. Optional 45° chamfer or
    /// round fillet on the top/bottom outer edge.
    Cylinder {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        d: Option<Num>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        r: Option<Num>,
        h: Num,
        /// Top diameter (cone/frustum). Defaults to `d`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        d2: Option<Num>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chamfer_top: Option<Num>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chamfer_bottom: Option<Num>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fillet_top: Option<Num>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fillet_bottom: Option<Num>,
        /// Facets around; automatic (~0.05 mm chord error) when omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        segments: Option<u32>,
        #[serde(default, skip_serializing_if = "is_false")]
        center: bool,
    },
    /// Sphere; give `d` or `r`.
    Sphere {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        d: Option<Num>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        r: Option<Num>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        segments: Option<u32>,
        #[serde(default, skip_serializing_if = "is_false")]
        center: bool,
    },
    /// Ring around Z: `major` = ring radius, `minor` = tube radius.
    Torus {
        major: Num,
        minor: Num,
        #[serde(default, skip_serializing_if = "is_false")]
        center: bool,
    },
    /// Extrude a simple 2D polygon (XY points, no self-intersections) up by `height`;
    /// `corner_radius` rounds its corners.
    Extrude {
        points: Vec<[Num; 2]>,
        height: Num,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        corner_radius: Option<Num>,
    },
    /// Obround prism along X.
    Slot { length: Num, width: Num, h: Num },
    /// Tube: outer diameter `d`, `wall` thickness, height `h`.
    Tube { d: Num, wall: Num, h: Num },
    /// Regular prism: `d` across corners or `flats`.
    Polygon {
        sides: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        d: Option<Num>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        flats: Option<Num>,
        h: Num,
    },
    /// Ramp: full height at the back (+Y), zero at the front.
    Wedge { size: [Num; 3] },
    /// Raised text, capitals `size` mm tall, `h` high, font sans|mono; subtract to engrave.
    Text {
        text: String,
        size: Num,
        h: Num,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        font: Option<String>,
    },
    /// Cutter (use in a difference): `screw` M2–M8 or `d`; `kind` clearance | tap | countersink |
    /// counterbore | insert | nut. Goes `depth` down from its origin; `axis` x/y = horizontal
    /// teardrop. `fit` adds mm (default: calibrated).
    Hole {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        screw: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        d: Option<Num>,
        depth: Num,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        axis: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fit: Option<Num>,
    },
    /// Hollow a box/cylinder/slot/polygon/extrude: `wall`, `floor` (default wall), open unless `closed`.
    Shell {
        child: Box<Node>,
        wall: Num,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        floor: Option<Num>,
        #[serde(default, skip_serializing_if = "is_false")]
        closed: bool,
    },
    /// Revolve a profile of [radius, z] points around Z (first/last point on the axis for a solid).
    Revolve {
        profile: Vec<[Num; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        segments: Option<u32>,
    },
    /// Mesh from a file (stl, obj, 3mf, step, scad), relative to the project file.
    Import {
        path: String,
        /// .scad: variables to set, e.g. {"wall": 3}.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        vars: BTreeMap<String, serde_json::Value>,
        /// 3MF object name (default: all, merged).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        object: Option<String>,
        /// 0-based object index.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
        /// Only these parts (with `index`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parts: Option<Vec<usize>>,
        /// Close holes, turn faces outward.
        #[serde(default, skip_serializing_if = "is_false")]
        repair: bool,
    },
    Union { children: Vec<Node> },
    /// First child minus all the others.
    Difference { children: Vec<Node> },
    Intersection { children: Vec<Node> },
    /// Convex hull of all children (soft, organic blends between primitives).
    Hull { children: Vec<Node> },
    /// Grid copies of `child`: `count` [nx, ny, nz] with `spacing` [dx, dy, dz] mm.
    Array {
        child: Box<Node>,
        count: [u32; 3],
        spacing: [Num; 3],
    },
    /// `count` copies of `child` rotated about Z over `angle` degrees (default 360).
    Polar {
        child: Box<Node>,
        count: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        angle: Option<Num>,
    },
    /// Pull (+) or push (−) `child`'s flat face at `point` (outward `normal`) by `distance` mm.
    PushPull { child: Box<Node>, point: [f32; 3], normal: [f32; 3], distance: Num },
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Evaluates shape trees with memoisation, so editing one parameter or child only recomputes
/// the parts that changed.
pub struct Evaluator {
    pub base_dir: PathBuf,
    cache: HashMap<u64, Arc<Mesh>>,
    /// Placed (world) meshes and their reports, keyed by local mesh + placement.
    pub(crate) placed: HashMap<(usize, [u32; 8]), (Arc<Mesh>, Arc<Mesh>, only3d_geom::MeshReport)>,
    /// Analysis of each body of a multi-part object, by local mesh and scale (moving or turning
    /// a body doesn't change whether it is watertight, or its volume).
    pub(crate) part_reports: HashMap<(usize, [u32; 3]), (Arc<Mesh>, only3d_geom::MeshReport)>,
    /// Extra mm on every `hole` without its own `fit` (the calibrated fit for this printer and
    /// filament; 0 until calibrated). Changing it clears the cache.
    pub fit_mm: f64,
    /// Loaded model files by path and OpenSCAD variables, with the file's modification time.
    imports: HashMap<(PathBuf, String), (std::time::SystemTime, Arc<Vec<only3d_geom::io::ImportedObject>>)>,
    /// The customizer variables of .scad files.
    scad_vars: HashMap<PathBuf, (std::time::SystemTime, Arc<Vec<crate::scad::ScadVar>>)>,
    pub stats: EvalStats,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct EvalStats {
    pub cache_hits: u64,
    pub evaluated: u64,
    pub booleans: u64,
    pub boolean_ms: f64,
}

impl Evaluator {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self { base_dir: base_dir.into(), cache: HashMap::new(), placed: HashMap::new(), part_reports: HashMap::new(), fit_mm: 0.0, imports: HashMap::new(), scad_vars: HashMap::new(), stats: EvalStats::default() }
    }

    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.placed.clear();
        self.part_reports.clear();
        self.imports.clear();
        self.scad_vars.clear();
    }

    pub fn eval(&mut self, node: &Node, params: &Params) -> Result<Arc<Mesh>, ModelError> {
        let key = {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            serde_json::to_string(node).unwrap_or_default().hash(&mut h);
            for (k, v) in params {
                k.hash(&mut h);
                v.to_bits().hash(&mut h);
            }
            h.finish()
        };
        if let Some(m) = self.cache.get(&key) {
            self.stats.cache_hits += 1;
            return Ok(m.clone());
        }
        self.stats.evaluated += 1;
        let mut m = self.eval_shape(&node.shape, params)?;
        let t = node_transform(node, params)?;
        if t != Mat4::IDENTITY {
            m.transform(&t);
        }
        let m = Arc::new(m);
        if self.cache.len() > 4096 {
            self.cache.clear();
            self.placed.clear();
        }
        self.cache.insert(key, m.clone());
        Ok(m)
    }

    fn eval_shape(&mut self, s: &Shape, p: &Params) -> Result<Mesh, ModelError> {
        let n = |v: &Num| v.eval(p);
        let opt = |v: &Option<Num>| -> Result<Option<f64>, ModelError> { v.as_ref().map(|x| x.eval(p)).transpose() };
        let geo = |r: only3d_geom::Result<Mesh>| r.map_err(|e| ModelError::Shape(e.to_string()));
        let gerr = |e: only3d_geom::GeomError| ModelError::Shape(e.to_string());
        Ok(match s {
            Shape::Box { size, radius, chamfer, corner_radius, chamfer_top, chamfer_bottom, fillet_top, fillet_bottom, center } => {
                let sz = Vec3::new(n(&size[0])? as f32, n(&size[1])? as f32, n(&size[2])? as f32);
                let finish = |c: Option<f64>, f: Option<f64>| match (c, f) {
                    (_, Some(f)) if f > 0.0 => EdgeFinish::Fillet(f as f32),
                    (Some(c), _) if c > 0.0 => EdgeFinish::Chamfer(c as f32),
                    _ => EdgeFinish::Sharp,
                };
                let (fb, ft) = (finish(opt(chamfer_bottom)?, opt(fillet_bottom)?), finish(opt(chamfer_top)?, opt(fillet_top)?));
                let cr = opt(corner_radius)?;
                let mut m = match (opt(radius)?, opt(chamfer)?) {
                    (Some(r), _) if r > 0.0 => geo(shapes::rounded_box(sz, r as f32, (shapes::auto_segments(r as f32) / 4).max(3)))?,
                    (_, Some(c)) if c > 0.0 => geo(shapes::rounded_box(sz, c as f32, 1))?,
                    _ if cr.is_some() || fb != EdgeFinish::Sharp || ft != EdgeFinish::Sharp => geo(shapes::rounded_slab(sz, cr.unwrap_or(0.0) as f32, fb, ft))?,
                    _ => geo(shapes::cuboid(sz))?,
                };
                if *center {
                    m.translate(Vec3::new(0.0, 0.0, -sz.z / 2.0));
                }
                m
            }
            Shape::Cylinder { d, r, h, d2, chamfer_top, chamfer_bottom, fillet_top, fillet_bottom, segments, center } => {
                let r1 = match (opt(d)?, opt(r)?) {
                    (Some(d), _) => d / 2.0,
                    (None, Some(r)) => r,
                    _ => return Err(ModelError::Shape("cylinder needs `d` or `r`".into())),
                };
                let r2 = opt(d2)?.map(|d| d / 2.0).unwrap_or(r1);
                let h = n(h)?;
                let finish = |c: Option<f64>, f: Option<f64>| match (c, f) {
                    (_, Some(f)) if f > 0.0 => EdgeFinish::Fillet(f as f32),
                    (Some(c), _) if c > 0.0 => EdgeFinish::Chamfer(c as f32),
                    _ => EdgeFinish::Sharp,
                };
                let mut m = geo(shapes::cylinder(
                    r1 as f32,
                    r2 as f32,
                    h as f32,
                    *segments,
                    finish(opt(chamfer_bottom)?, opt(fillet_bottom)?),
                    finish(opt(chamfer_top)?, opt(fillet_top)?),
                ))?;
                if *center {
                    m.translate(Vec3::new(0.0, 0.0, -h as f32 / 2.0));
                }
                m
            }
            Shape::Sphere { d, r, segments, center } => {
                let rad = match (opt(d)?, opt(r)?) {
                    (Some(d), _) => d / 2.0,
                    (None, Some(r)) => r,
                    _ => return Err(ModelError::Shape("sphere needs `d` or `r`".into())),
                };
                let mut m = geo(shapes::sphere(rad as f32, *segments))?;
                if *center {
                    m.translate(Vec3::new(0.0, 0.0, -rad as f32));
                }
                m
            }
            Shape::Torus { major, minor, center } => {
                let (a, b) = (n(major)? as f32, n(minor)? as f32);
                let mut m = geo(shapes::torus(a, b, None))?;
                if *center {
                    m.translate(Vec3::new(0.0, 0.0, -b));
                }
                m
            }
            Shape::Extrude { points, height, corner_radius } => {
                let pts = points.iter().map(|q| Ok(Vec2::new(n(&q[0])? as f32, n(&q[1])? as f32))).collect::<Result<Vec<_>, ModelError>>()?;
                let pts = match opt(corner_radius)? {
                    Some(r) if r > 0.0 => {
                        let ccw = if shapes::signed_area(&pts) < 0.0 { pts.iter().rev().copied().collect() } else { pts };
                        shapes::round_corners(&ccw, r as f32)
                    }
                    _ => pts,
                };
                geo(shapes::extrude(&pts, n(height)? as f32))?
            }
            Shape::Slot { length, width, h } => geo(shapes::extrude(&shapes::slot_outline(n(length)? as f32, n(width)? as f32).map_err(gerr)?, n(h)? as f32))?,
            Shape::Tube { d, wall, h } => {
                let (d, w, h) = (n(d)? as f32, n(wall)? as f32, n(h)? as f32);
                if !(w > 0.0 && 2.0 * w < d) {
                    return Err(ModelError::Shape("tube needs 0 < wall < d/2".into()));
                }
                let outer = geo(shapes::cylinder(d / 2.0, d / 2.0, h, None, EdgeFinish::Sharp, EdgeFinish::Sharp))?;
                let mut inner = geo(shapes::cylinder(d / 2.0 - w, d / 2.0 - w, h + 2.0, None, EdgeFinish::Sharp, EdgeFinish::Sharp))?;
                inner.translate(Vec3::new(0.0, 0.0, -1.0));
                csg::boolean(&outer, &inner, BoolOp::Subtract, &mut self.stats)?
            }
            Shape::Polygon { sides, d, flats, h } => {
                let r = match (opt(d)?, opt(flats)?) {
                    (Some(d), _) => d / 2.0,
                    (None, Some(f)) => f / 2.0 / (std::f64::consts::PI / *sides as f64).cos(),
                    _ => return Err(ModelError::Shape("polygon needs `d` (across corners) or `flats`".into())),
                };
                geo(shapes::extrude(&shapes::regular_polygon(*sides, r as f32).map_err(gerr)?, n(h)? as f32))?
            }
            Shape::Wedge { size } => {
                let (x, y, z) = (n(&size[0])? as f32, n(&size[1])? as f32, n(&size[2])? as f32);
                // the triangle in (y, z), extruded by x, then axes permuted: (u, v, w) → (w, u, v)
                let tri = [Vec2::new(-y / 2.0, 0.0), Vec2::new(y / 2.0, 0.0), Vec2::new(y / 2.0, z)];
                let mut m = geo(shapes::extrude(&tri, x))?;
                m.transform(&glam::Mat4::from_cols(glam::Vec4::new(0.0, 1.0, 0.0, 0.0), glam::Vec4::new(0.0, 0.0, 1.0, 0.0), glam::Vec4::new(1.0, 0.0, 0.0, 0.0), glam::Vec4::W));
                m.translate(Vec3::new(-x / 2.0, 0.0, 0.0));
                m
            }
            Shape::Hole { screw, d, depth, kind, axis, fit } => self.hole(screw.as_deref(), opt(d)?, n(depth)?, kind.as_deref().unwrap_or("clearance"), axis.as_deref().unwrap_or("z"), opt(fit)?.unwrap_or(self.fit_mm))?,
            Shape::Text { text, size, h, font } => self.text_mesh(text, n(size)? as f32, n(h)? as f32, font.as_deref().unwrap_or("sans"))?,
            Shape::Shell { child, wall, floor, closed } => self.shell(child, n(wall)?, opt(floor)?, *closed, p)?,
            Shape::Revolve { profile, segments } => {
                let pts = profile.iter().map(|q| Ok(Vec2::new(n(&q[0])? as f32, n(&q[1])? as f32))).collect::<Result<Vec<_>, ModelError>>()?;
                let seg = segments.unwrap_or_else(|| shapes::auto_segments(pts.iter().map(|v| v.x).fold(1.0, f32::max)));
                geo(shapes::revolve_closed(&pts, seg))?
            }
            Shape::Import { path, vars, object, index, parts, repair } => {
                let mut m = self.import(path, vars, p, object.as_deref(), *index, parts.as_deref())?;
                if *repair {
                    only3d_geom::repair::repair(&mut m, 400);
                }
                m
            }
            Shape::Union { children } => {
                let ms = self.eval_children(children, p)?;
                csg::union_all(ms, &mut self.stats)?
            }
            Shape::Difference { children } => {
                let mut ms = self.eval_children(children, p)?;
                if ms.is_empty() {
                    return Err(ModelError::Shape("difference needs at least one child".into()));
                }
                let base = ms.remove(0);
                let bb = base.bounds();
                let cutters: Vec<Arc<Mesh>> = ms.into_iter().filter(|m| m.bounds().overlaps(&bb)).collect();
                if cutters.is_empty() {
                    (*base).clone()
                } else {
                    let cut = csg::union_all(cutters, &mut self.stats)?;
                    csg::boolean(&base, &cut, BoolOp::Subtract, &mut self.stats)?
                }
            }
            Shape::Intersection { children } => {
                let ms = self.eval_children(children, p)?;
                let mut it = ms.into_iter();
                let mut acc = (*it.next().ok_or_else(|| ModelError::Shape("intersection needs children".into()))?).clone();
                for m in it {
                    acc = csg::boolean(&acc, &m, BoolOp::Intersect, &mut self.stats)?;
                }
                acc
            }
            Shape::Hull { children } => {
                let ms = self.eval_children(children, p)?;
                let pts: Vec<Vec3> = ms.iter().flat_map(|m| m.positions.iter().copied()).collect();
                only3d_geom::hull::convex_hull(&pts).ok_or_else(|| ModelError::Shape("hull of flat/degenerate children".into()))?
            }
            Shape::Array { child, count, spacing } => {
                let m = self.eval(child, p)?;
                let d = Vec3::new(n(&spacing[0])? as f32, n(&spacing[1])? as f32, n(&spacing[2])? as f32);
                let total = count[0].max(1) as usize * count[1].max(1) as usize * count[2].max(1) as usize;
                if total > 10_000 {
                    return Err(ModelError::Shape(format!("array of {total} copies is too large (max 10000)")));
                }
                let mut copies = Vec::with_capacity(total);
                for k in 0..count[2].max(1) {
                    for j in 0..count[1].max(1) {
                        for i in 0..count[0].max(1) {
                            let mut c = (*m).clone();
                            c.translate(d * Vec3::new(i as f32, j as f32, k as f32));
                            copies.push(Arc::new(c));
                        }
                    }
                }
                csg::union_all(copies, &mut self.stats)?
            }
            Shape::PushPull { child, point: at, normal, distance } => {
                let m = self.eval(child, p)?;
                let d = n(distance)? as f32;
                let face = only3d_geom::faces::face_at(&m, Vec3::from(*at), Vec3::from(*normal), 0.01).ok_or_else(|| {
                    ModelError::Shape(format!("push_pull: no flat face at {at:?} facing {normal:?} any more (the shape under it changed); pick the face again or remove this step"))
                })?;
                if d.abs() < 1e-4 {
                    (*m).clone()
                } else if d > 0.0 {
                    // a hair into the solid, so the new block fuses rather than touches
                    let block = only3d_geom::faces::prism(&m, &face, -0.01, d);
                    csg::boolean(&m, &block, BoolOp::Union, &mut self.stats)?
                } else {
                    let block = only3d_geom::faces::prism(&m, &face, d, 0.01);
                    csg::boolean(&m, &block, BoolOp::Subtract, &mut self.stats)?
                }
            }
            Shape::Polar { child, count, angle } => {
                let m = self.eval(child, p)?;
                let ang = opt(angle)?.unwrap_or(360.0);
                let c = (*count).clamp(1, 1000);
                let step = if (ang - 360.0).abs() < 1e-9 { ang / c as f64 } else { ang / (c.max(2) - 1) as f64 };
                let copies = (0..c)
                    .map(|i| Arc::new(m.transformed(&Mat4::from_rotation_z((step * i as f64).to_radians() as f32))))
                    .collect();
                csg::union_all(copies, &mut self.stats)?
            }
        })
    }

    fn eval_children(&mut self, children: &[Node], p: &Params) -> Result<Vec<Arc<Mesh>>, ModelError> {
        children
            .iter()
            .enumerate()
            .map(|(i, c)| {
                self.eval(c, p).map_err(|e| match e {
                    ModelError::At { path, source } => ModelError::At { path: format!("children[{i}].{path}"), source },
                    other => ModelError::At { path: format!("children[{i}]"), source: Box::new(other) },
                })
            })
            .collect()
    }

    /// Load (cached by modification time) the objects of a model file.
    pub fn load_file(&mut self, path: &str) -> Result<Arc<Vec<only3d_geom::io::ImportedObject>>, ModelError> {
        self.load_file_with(path, &BTreeMap::new(), &Params::new())
    }

    /// Load a model file; a .scad file is rendered with these variables set (numbers may be
    /// expressions over the project parameters).
    pub fn load_file_with(&mut self, path: &str, vars: &BTreeMap<String, serde_json::Value>, params: &Params) -> Result<Arc<Vec<only3d_geom::io::ImportedObject>>, ModelError> {
        let full = resolve_path(&self.base_dir, path);
        let meta = std::fs::metadata(&full).map_err(|e| ModelError::Import { path: full.display().to_string(), message: e.to_string() })?;
        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        let scad = crate::scad::is_scad(&full);
        if !scad && !vars.is_empty() {
            return Err(ModelError::Import { path: full.display().to_string(), message: "`vars` only apply to .scad files".into() });
        }
        let defines = if scad && !vars.is_empty() { self.scad_defines(path, vars, params)? } else { Vec::new() };
        let key = (full.clone(), defines.iter().map(|(k, v)| format!("{k}={v};")).collect::<String>());
        let objs = match self.imports.get(&key) {
            Some((t, o)) if *t == mtime => o.clone(),
            _ => {
                let objs = if scad {
                    vec![only3d_geom::io::ImportedObject::new("scad", crate::scad::render(&full, &defines)?)]
                } else {
                    only3d_geom::io::load(&full).map_err(|e| ModelError::Import { path: full.display().to_string(), message: e.to_string() })?
                };
                let objs = Arc::new(objs);
                if self.imports.len() > 64 {
                    self.imports.retain(|(p, _), _| *p != full);
                }
                self.imports.insert(key, (mtime, objs.clone()));
                objs
            }
        };
        Ok(objs)
    }

    /// The customizer variables of a .scad file (cached by modification time).
    pub fn scad_vars(&mut self, path: &str) -> Result<Arc<Vec<crate::scad::ScadVar>>, ModelError> {
        let full = resolve_path(&self.base_dir, path);
        let mtime = std::fs::metadata(&full).and_then(|m| m.modified()).map_err(|e| ModelError::Import { path: full.display().to_string(), message: e.to_string() })?;
        if let Some((t, v)) = self.scad_vars.get(&full) {
            if *t == mtime {
                return Ok(v.clone());
            }
        }
        let v = Arc::new(crate::scad::describe(&full)?);
        self.scad_vars.insert(full, (mtime, v.clone()));
        Ok(v)
    }

    /// `-D name=value` pairs for these variables: each value as the OpenSCAD literal for its
    /// declared type (a string for a number variable is an expression over the parameters).
    fn scad_defines(&mut self, path: &str, vars: &BTreeMap<String, serde_json::Value>, params: &Params) -> Result<Vec<(String, String)>, ModelError> {
        let decl = self.scad_vars(path).ok();
        vars.iter()
            .map(|(k, v)| {
                let d = decl.as_ref().and_then(|d| d.iter().find(|x| &x.name == k));
                Ok((k.clone(), crate::scad::literal(v, d, params)?))
            })
            .collect()
    }

    /// A hole cutter: the shaft plus the head pocket for its kind, down from the origin (or a
    /// horizontal teardrop along X/Y centred on it).
    fn hole(&mut self, screw: Option<&str>, d: Option<f64>, depth: f64, kind: &str, axis: &str, fit: f64) -> Result<Mesh, ModelError> {
        let gerr = |e: only3d_geom::GeomError| ModelError::Shape(format!("hole: {e}"));
        match hole_plan(screw, d, depth, kind, axis, fit)? {
            HolePlan::Teardrop { r, depth: dep, axis } => {
                // the round bottom plus a 45° roof, so no support is needed
                let mut outline = Vec::new();
                let seg = shapes::auto_segments(r).max(16);
                for k in 0..=seg {
                    let a = std::f32::consts::FRAC_PI_4 * 3.0 + (std::f32::consts::TAU - std::f32::consts::FRAC_PI_2) * k as f32 / seg as f32;
                    outline.push(Vec2::new(r * a.cos(), r * a.sin()));
                }
                outline.push(Vec2::new(0.0, r * std::f32::consts::SQRT_2));
                if shapes::signed_area(&outline) < 0.0 {
                    outline.reverse();
                }
                let mut m = shapes::extrude(&outline, dep).map_err(gerr)?;
                // profile (u, v) with v up, extruded along w: (u, v, w) → along X: (w, u, v); along Y: (u, w, v)
                let xf = if axis == 'x' {
                    glam::Mat4::from_cols(glam::Vec4::new(0.0, 1.0, 0.0, 0.0), glam::Vec4::new(0.0, 0.0, 1.0, 0.0), glam::Vec4::new(1.0, 0.0, 0.0, 0.0), glam::Vec4::W)
                } else {
                    glam::Mat4::from_cols(glam::Vec4::new(1.0, 0.0, 0.0, 0.0), glam::Vec4::new(0.0, 0.0, 1.0, 0.0), glam::Vec4::new(0.0, -1.0, 0.0, 0.0), glam::Vec4::W)
                };
                m.transform(&xf);
                let b = m.bounds();
                m.translate(Vec3::new(if axis == 'x' { -(b.min.x + b.max.x) / 2.0 } else { 0.0 }, if axis == 'y' { -(b.min.y + b.max.y) / 2.0 } else { 0.0 }, 0.0));
                Ok(m)
            }
            HolePlan::Down { cyls, nut } => {
                let mut parts = Vec::new();
                for [r, r2, h, z0] in cyls {
                    let mut m = shapes::cylinder(r, r2, h, None, EdgeFinish::Sharp, EdgeFinish::Sharp).map_err(gerr)?;
                    m.translate(Vec3::new(0.0, 0.0, z0));
                    parts.push(Arc::new(m));
                }
                if let Some((flats, h)) = nut {
                    let hex = shapes::regular_polygon(6, flats / 2.0 / (std::f32::consts::PI / 6.0).cos()).map_err(gerr)?;
                    let mut m = shapes::extrude(&hex, h + 0.5).map_err(gerr)?;
                    m.translate(Vec3::new(0.0, 0.0, -h));
                    parts.push(Arc::new(m));
                }
                if parts.len() == 1 {
                    return Ok((*parts[0]).clone());
                }
                csg::union_all(parts, &mut self.stats)
            }
        }
    }

    /// Raised text: each glyph's outlines extruded, the counters (holes of O, A…) cut out.
    fn text_mesh(&mut self, text: &str, size: f32, h: f32, font: &str) -> Result<Mesh, ModelError> {
        let gerr = |e: only3d_geom::GeomError| ModelError::Shape(format!("text: {e}"));
        if h.is_nan() || h <= 0.0 {
            return Err(ModelError::Shape("text needs a height h > 0".into()));
        }
        let (solids, holes) = only3d_geom::text::classify(only3d_geom::text::outlines(text, size, font).map_err(gerr)?);
        let mut parts = Vec::new();
        for c in &solids {
            parts.push(Arc::new(shapes::extrude(c, h).map_err(gerr)?));
        }
        let body = csg::union_all(parts, &mut self.stats)?;
        if holes.is_empty() {
            return Ok(body);
        }
        let mut cuts = Vec::new();
        for (_, c) in &holes {
            let mut m = shapes::extrude(c, h + 2.0).map_err(gerr)?;
            m.translate(Vec3::new(0.0, 0.0, -1.0));
            cuts.push(Arc::new(m));
        }
        let cut = csg::union_all(cuts, &mut self.stats)?;
        csg::boolean(&body, &cut, BoolOp::Subtract, &mut self.stats)
    }

    /// Hollow a profile primitive: the same shape made smaller by the wall (and lifted by the
    /// floor) is cut out of it.
    fn shell(&mut self, child: &Node, wall: f64, floor: Option<f64>, closed: bool, p: &Params) -> Result<Mesh, ModelError> {
        let inner = shell_inner(child, wall, floor, closed, p)?;
        let outer = self.eval(child, p)?;
        let inner = self.eval(&inner, p)?;
        csg::boolean(&outer, &inner, BoolOp::Subtract, &mut self.stats)
    }

    fn import(&mut self, path: &str, vars: &BTreeMap<String, serde_json::Value>, params: &Params, object: Option<&str>, index: Option<usize>, parts: Option<&[usize]>) -> Result<Mesh, ModelError> {
        let full = resolve_path(&self.base_dir, path);
        let objs = self.load_file_with(path, vars, params)?;
        if let Some(i) = index {
            let o = objs.get(i).ok_or_else(|| ModelError::Import { path: full.display().to_string(), message: format!("object index {i} out of range (file has {})", objs.len()) })?;
            let Some(sel) = parts else { return Ok(o.mesh.clone()) };
            let mut m = Mesh::new();
            for &k in sel {
                let part = o.parts.get(k).ok_or_else(|| ModelError::Import { path: full.display().to_string(), message: format!("object {i} has no part {k} (it has {})", o.parts.len()) })?;
                m.append(&part.mesh);
            }
            return Ok(m);
        }
        let mut m = Mesh::new();
        for o in objs.iter().filter(|o| object.is_none_or(|n| o.name == n)) {
            m.append(&o.mesh);
        }
        if m.is_empty() {
            return Err(ModelError::Import {
                path: full.display().to_string(),
                message: match object {
                    Some(n) => format!("no object named '{n}' (have: {})", objs.iter().map(|o| o.name.as_str()).collect::<Vec<_>>().join(", ")),
                    None => "file contains no triangles".into(),
                },
            });
        }
        Ok(m)
    }
}

/// What `shell` cuts out of its child: the same shape made smaller by the wall and lifted by the
/// floor (open at the top unless `closed`).
pub fn shell_inner(child: &Node, wall: f64, floor: Option<f64>, closed: bool, p: &Params) -> Result<Node, ModelError> {
    if child.rotate.is_some() || child.scale.is_some() || child.mirror.is_some() {
        return Err(ModelError::Shape("shell's child can be moved (`at`) but not turned or scaled; turn or scale the shell instead".into()));
    }
    if wall.is_nan() || wall <= 0.0 {
        return Err(ModelError::Shape("shell needs wall > 0".into()));
    }
    let fl = floor.unwrap_or(wall);
    let num = |x: f64| -> Num { x.into() };
    let ev = |v: &Num| v.eval(p);
    let small = |what: &str| ModelError::Shape(format!("shell: the {what} is too small for a {wall} mm wall"));
    // inner height: up through the open top, or under a ceiling as thick as the wall
    let inner_h = |h: f64| if closed { h - fl - wall } else { h - fl + 1.0 };
    let (inner, base): (Shape, f64) = match &child.shape {
        Shape::Box { size, radius, corner_radius, center, .. } => {
            let (x, y, z) = (ev(&size[0])?, ev(&size[1])?, ev(&size[2])?);
            let ih = inner_h(z);
            if x - 2.0 * wall <= 0.0 || y - 2.0 * wall <= 0.0 || ih <= 0.0 {
                return Err(small("box"));
            }
            let r = radius.as_ref().map(ev).transpose()?.map(|r| num((r - wall).max(0.0)));
            let cr = corner_radius.as_ref().map(ev).transpose()?.map(|r| num((r - wall).max(0.0)));
            let shape = Shape::Box { size: [num(x - 2.0 * wall), num(y - 2.0 * wall), num(ih)], radius: if closed { r } else { None }, chamfer: None, corner_radius: cr.or(if closed { None } else { radius.as_ref().map(ev).transpose()?.map(|r| num((r - wall).max(0.0))) }), chamfer_top: None, chamfer_bottom: None, fillet_top: None, fillet_bottom: None, center: false };
            (shape, if *center { -z / 2.0 } else { 0.0 } + fl)
        }
        Shape::Cylinder { d, r, h, d2, segments, center, .. } => {
            let r1 = match (d.as_ref().map(ev).transpose()?, r.as_ref().map(ev).transpose()?) {
                (Some(d), _) => d / 2.0,
                (None, Some(r)) => r,
                _ => return Err(ModelError::Shape("cylinder needs `d` or `r`".into())),
            };
            let r2 = d2.as_ref().map(ev).transpose()?.map(|d| d / 2.0).unwrap_or(r1);
            let hh = ev(h)?;
            let ih = inner_h(hh);
            if r1 - wall <= 0.0 || r2 - wall <= 0.0 || ih <= 0.0 {
                return Err(small("cylinder"));
            }
            // keep the side slope: the inner radius at the floor and at its top
            let at_z = |z: f64| r1 + (r2 - r1) * (z / hh) - wall;
            let shape = Shape::Cylinder { d: Some(num(2.0 * at_z(fl))), r: None, h: num(ih), d2: Some(num(2.0 * at_z(fl + ih))), chamfer_top: None, chamfer_bottom: None, fillet_top: None, fillet_bottom: None, segments: *segments, center: false };
            (shape, if *center { -hh / 2.0 } else { 0.0 } + fl)
        }
        Shape::Slot { length, width, h } => {
            let (l, w, hh) = (ev(length)?, ev(width)?, ev(h)?);
            if w - 2.0 * wall <= 0.0 || inner_h(hh) <= 0.0 {
                return Err(small("slot"));
            }
            (Shape::Slot { length: num(l - 2.0 * wall), width: num(w - 2.0 * wall), h: num(inner_h(hh)) }, fl)
        }
        Shape::Polygon { sides, d, flats, h } => {
            let cos = (std::f64::consts::PI / *sides as f64).cos();
            let f = match (d.as_ref().map(ev).transpose()?, flats.as_ref().map(ev).transpose()?) {
                (Some(d), _) => d * cos,
                (None, Some(f)) => f,
                _ => return Err(ModelError::Shape("polygon needs `d` or `flats`".into())),
            };
            let hh = ev(h)?;
            if f - 2.0 * wall <= 0.0 || inner_h(hh) <= 0.0 {
                return Err(small("polygon"));
            }
            (Shape::Polygon { sides: *sides, d: None, flats: Some(num(f - 2.0 * wall)), h: num(inner_h(hh)) }, fl)
        }
        Shape::Extrude { points, height, corner_radius } => {
            let mut pts = points.iter().map(|q| Ok(Vec2::new(ev(&q[0])? as f32, ev(&q[1])? as f32))).collect::<Result<Vec<_>, ModelError>>()?;
            if shapes::signed_area(&pts) < 0.0 {
                pts.reverse();
            }
            if let Some(r) = corner_radius.as_ref().map(ev).transpose()?.filter(|r| *r > 0.0) {
                pts = shapes::round_corners(&pts, r as f32);
            }
            let hh = ev(height)?;
            let ins = shapes::inset(&pts, wall as f32).ok_or_else(|| small("outline"))?;
            if inner_h(hh) <= 0.0 {
                return Err(small("extrusion"));
            }
            (Shape::Extrude { points: ins.iter().map(|v| [num(v.x as f64), num(v.y as f64)]).collect(), height: num(inner_h(hh)), corner_radius: None }, fl)
        }
        _ => return Err(ModelError::Shape("shell works on box, cylinder, slot, polygon and extrude".into())),
    };
    let at = match &child.at {
        Some(a) => [ev(&a[0])?, ev(&a[1])?, ev(&a[2])?],
        None => [0.0; 3],
    };
    Ok(Node { shape: inner, at: Some([num(at[0]), num(at[1]), num(at[2] + base)]), rotate: None, scale: None, mirror: None, label: None })
}

/// The pieces of a hole cutter (shared by the evaluator and the OpenSCAD export).
#[derive(Clone, Debug, PartialEq)]
pub enum HolePlan {
    /// Horizontal teardrop of radius `r`, `depth` long along `axis` ('x' or 'y'), centred.
    Teardrop { r: f32, depth: f32, axis: char },
    /// Straight down from the origin: cylinders [r_bottom, r_top, height, z of the bottom], and a
    /// nut pocket (across flats, height) under the surface.
    Down { cyls: Vec<[f32; 4]>, nut: Option<(f32, f32)> },
}

pub fn hole_plan(screw: Option<&str>, d: Option<f64>, depth: f64, kind: &str, axis: &str, fit: f64) -> Result<HolePlan, ModelError> {
    let spec = screw.map(screw_spec).transpose()?;
    if depth.is_nan() || depth <= 0.0 {
        return Err(ModelError::Shape("hole needs depth > 0".into()));
    }
    // shaft diameter for the kind, then the fit
    let shaft = match (kind, spec, d) {
        (_, None, Some(d)) => d,
        ("tap", Some(s), _) => s.tap,
        ("insert", Some(s), _) => s.insert_d,
        (_, Some(s), _) => s.clearance,
        (_, None, None) => return Err(ModelError::Shape("hole needs `screw` (e.g. \"M3\") or `d`".into())),
    } + fit;
    let (r, dep) = ((shaft / 2.0) as f32, depth as f32);
    match axis {
        "z" => {}
        "x" | "y" => return Ok(HolePlan::Teardrop { r, depth: dep, axis: if axis == "x" { 'x' } else { 'y' } }),
        other => return Err(ModelError::Shape(format!("hole axis '{other}' (x, y or z)"))),
    }
    // straight down from the surface, poking 0.5 mm above it so the cut is clean
    let mut cyls = vec![[r, r, dep + 0.5, -dep]];
    let mut nut = None;
    match (kind, spec) {
        ("countersink", Some(s)) => {
            let head = (s.csk_head + fit) as f32 / 2.0;
            // a 90° cone from the head diameter at the surface down to the shaft
            cyls.push([r, head, head - r, -(head - r)]);
            cyls.push([head, head, 0.5, 0.0]);
        }
        ("counterbore", Some(s)) => {
            let head = (s.cbore_d + fit) as f32 / 2.0;
            let deep = s.cbore_depth as f32;
            cyls.push([head, head, deep + 0.5, -deep]);
        }
        ("nut", Some(s)) => nut = Some(((s.nut_flats + fit) as f32, s.nut_h as f32)),
        ("countersink" | "counterbore" | "nut", None) => return Err(ModelError::Shape(format!("a {kind} hole needs `screw` (e.g. \"M3\")"))),
        ("clearance" | "tap" | "insert" | "plain", _) => {}
        (other, _) => return Err(ModelError::Shape(format!("hole kind '{other}' (clearance, tap, countersink, counterbore, insert, nut)"))),
    }
    Ok(HolePlan::Down { cyls, nut })
}

pub fn resolve_path(base: &Path, p: &str) -> PathBuf {
    let pb = PathBuf::from(shellexpand_home(p));
    if pb.is_absolute() {
        pb
    } else {
        base.join(pb)
    }
}

fn shellexpand_home(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(h) = std::env::home_dir() {
            return h.join(rest).to_string_lossy().to_string();
        }
    }
    p.to_string()
}

/// scale → rotate → mirror → translate
pub fn node_transform(node: &Node, p: &Params) -> Result<Mat4, ModelError> {
    let v3 = |a: &[Num; 3]| -> Result<Vec3, ModelError> { Ok(Vec3::new(a[0].eval(p)? as f32, a[1].eval(p)? as f32, a[2].eval(p)? as f32)) };
    let mut m = Mat4::IDENTITY;
    if let Some(s) = &node.scale {
        m = Mat4::from_scale(v3(s)?) * m;
    }
    if let Some(r) = &node.rotate {
        let r = v3(r)?;
        m = Mat4::from_quat(euler_xyz(r)) * m;
    }
    if let Some(n) = node.mirror {
        m = mirror_matrix(Vec3::from(n)) * m;
    }
    if let Some(a) = &node.at {
        m = Mat4::from_translation(v3(a)?) * m;
    }
    Ok(m)
}

/// Rotation about X, then Y, then Z (extrinsic), degrees — OpenSCAD's `rotate([x,y,z])`.
pub fn euler_xyz(deg: Vec3) -> Quat {
    Quat::from_euler(EulerRot::ZYX, deg.z.to_radians(), deg.y.to_radians(), deg.x.to_radians())
}

pub fn quat_to_euler_xyz(q: Quat) -> Vec3 {
    let (z, y, x) = q.to_euler(EulerRot::ZYX);
    Vec3::new(x.to_degrees(), y.to_degrees(), z.to_degrees())
}

pub fn mirror_matrix(n: Vec3) -> Mat4 {
    let n = n.normalize_or_zero();
    if n == Vec3::ZERO {
        return Mat4::IDENTITY;
    }
    let m3 = glam::Mat3::IDENTITY - 2.0 * glam::Mat3::from_cols(n * n.x, n * n.y, n * n.z);
    Mat4::from_mat3(m3)
}

pub fn bounds_of(meshes: &[Arc<Mesh>]) -> Aabb {
    meshes.iter().fold(Aabb::EMPTY, |b, m| b.union(&m.bounds()))
}

/// Metric screw sizes for holes (mm): ISO 273 medium clearance, a self-tapping pilot for
/// plastic, ISO 10642 countersunk heads, ISO 4762 socket-head counterbores, heat-set insert holes
/// (common M2–M5 inserts), ISO 4032 nut flats and heights.
#[derive(Clone, Copy, Debug)]
pub struct ScrewSpec {
    pub clearance: f64,
    pub tap: f64,
    pub csk_head: f64,
    pub cbore_d: f64,
    pub cbore_depth: f64,
    pub insert_d: f64,
    pub nut_flats: f64,
    pub nut_h: f64,
}

pub fn screw_spec(name: &str) -> Result<ScrewSpec, ModelError> {
    let s = |clearance, tap, csk_head, cbore_d, cbore_depth, insert_d, nut_flats, nut_h| ScrewSpec { clearance, tap, csk_head, cbore_d, cbore_depth, insert_d, nut_flats, nut_h };
    Ok(match name.trim().to_ascii_uppercase().as_str() {
        "M2" => s(2.4, 1.6, 4.4, 4.4, 2.2, 3.2, 4.0, 1.6),
        "M2.5" => s(2.9, 2.1, 5.5, 5.5, 2.7, 3.6, 5.0, 2.0),
        "M3" => s(3.4, 2.5, 6.7, 6.5, 3.2, 4.0, 5.5, 2.4),
        "M4" => s(4.5, 3.3, 9.0, 8.0, 4.2, 5.6, 7.0, 3.2),
        "M5" => s(5.5, 4.2, 11.2, 9.5, 5.2, 6.4, 8.0, 4.7),
        "M6" => s(6.6, 5.0, 13.4, 11.0, 6.2, 8.0, 10.0, 5.2),
        "M8" => s(9.0, 6.8, 17.9, 14.0, 8.2, 9.6, 13.0, 6.8),
        other => return Err(ModelError::Shape(format!("unknown screw '{other}' (M2, M2.5, M3, M4, M5, M6, M8)"))),
    })
}

#[cfg(test)]
mod new_shape_tests {
    use super::*;

    fn eval(json: serde_json::Value) -> Arc<Mesh> {
        let node: Node = serde_json::from_value(json).unwrap();
        let mut ev = Evaluator::new(".");
        ev.eval(&node, &Params::new()).unwrap_or_else(|e| panic!("{e}"))
    }

    fn size(m: &Mesh) -> [f32; 3] {
        let b = m.bounds();
        [b.max.x - b.min.x, b.max.y - b.min.y, b.max.z - b.min.z]
    }

    fn near(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.05)
    }

    #[test]
    fn slot_tube_polygon_wedge() {
        let s = eval(serde_json::json!({"type": "slot", "length": 40, "width": 10, "h": 3}));
        assert!(near(size(&s), [40.0, 10.0, 3.0]) && s.analyze().watertight);
        let t = eval(serde_json::json!({"type": "tube", "d": 20, "wall": 2, "h": 10}));
        let r = t.analyze();
        let want = std::f64::consts::PI * (100.0 - 64.0) * 10.0;
        assert!(r.watertight && (r.volume_mm3 - want).abs() / want < 0.02, "{} vs {want}", r.volume_mm3);
        let hex = eval(serde_json::json!({"type": "polygon", "sides": 6, "flats": 10, "h": 4}));
        assert!((size(&hex)[1] - 10.0).abs() < 0.01, "flats along Y: {:?}", size(&hex));
        let w = eval(serde_json::json!({"type": "wedge", "size": [30, 20, 10]}));
        assert!(near(size(&w), [30.0, 20.0, 10.0]), "{:?}", size(&w));
        let wr = w.analyze();
        assert!(wr.watertight && !wr.inverted && (wr.volume_mm3 - 3000.0).abs() < 1.0);
        // tall at the back (+Y): the highest points have y > 0
        let top = w.positions.iter().filter(|p| p.z > 9.9).map(|p| p.y).fold(f32::MIN, f32::max);
        assert!(top > 9.9, "the high edge is at the back");
    }

    #[test]
    fn holes_for_screws() {
        // an M3 countersunk hole through a 6 mm plate: 3.4 shaft, 6.7 head at the surface
        let plate = serde_json::json!({"type": "box", "size": [30, 30, 6]});
        let cut = |hole: serde_json::Value| eval(serde_json::json!({"type": "difference", "children": [plate.clone(), hole]}));
        let solid = eval(plate.clone()).analyze().volume_mm3;
        let csk = cut(serde_json::json!({"type": "hole", "screw": "M3", "depth": 10, "kind": "countersink", "at": [0, 0, 6]})).analyze();
        assert!(csk.watertight, "{csk:?}");
        let shaft = std::f64::consts::PI * 1.7f64.powi(2) * 6.0;
        assert!(solid - csk.volume_mm3 > shaft * 1.2, "the countersink takes more than the shaft");
        let plain = cut(serde_json::json!({"type": "hole", "d": 5, "depth": 10, "at": [0, 0, 6]})).analyze();
        assert!(((solid - plain.volume_mm3) - std::f64::consts::PI * 6.25 * 6.0).abs() < 2.0);
        for kind in ["clearance", "tap", "counterbore", "insert", "nut"] {
            let m = cut(serde_json::json!({"type": "hole", "screw": "M4", "depth": 10, "kind": kind, "at": [0, 0, 6]})).analyze();
            assert!(m.watertight && m.volume_mm3 < solid, "{kind}");
        }
        // a horizontal teardrop through the plate along X: its roof peaks at r·√2
        let td = eval(serde_json::json!({"type": "hole", "d": 4, "depth": 40, "axis": "x"}));
        let b = td.bounds();
        assert!((b.max.z - 2.0 * std::f32::consts::SQRT_2).abs() < 0.05 && (b.min.z + 2.0).abs() < 0.05, "{b:?}");
        assert!((b.max.x - b.min.x - 40.0).abs() < 0.05);
        let node: Node = serde_json::from_value(serde_json::json!({"type": "hole", "screw": "M7", "depth": 5})).unwrap();
        assert!(Evaluator::new(".").eval(&node, &Params::new()).is_err());
    }

    #[test]
    fn rounded_profiles_text_and_shells() {
        let e = eval(serde_json::json!({"type": "extrude", "points": [[0, 0], [30, 0], [30, 20], [0, 20]], "height": 5, "corner_radius": 4}));
        let want = (600.0 - (4.0 - std::f64::consts::PI) * 16.0) * 5.0;
        assert!((e.analyze().volume_mm3 - want).abs() / want < 0.01);
        let t = eval(serde_json::json!({"type": "text", "text": "OA", "size": 10, "h": 1.5}));
        let r = t.analyze();
        assert!(r.watertight, "{r:?}");
        assert!((size(&t)[2] - 1.5).abs() < 0.01);
        // an open box: outer minus inner, floor = wall
        let b = eval(serde_json::json!({"type": "shell", "child": {"type": "box", "size": [40, 30, 20], "corner_radius": 5}, "wall": 2}));
        let r = b.analyze();
        assert!(r.watertight, "{r:?}");
        assert!(near(size(&b), [40.0, 30.0, 20.0]));
        let solid = eval(serde_json::json!({"type": "box", "size": [40, 30, 20], "corner_radius": 5})).analyze().volume_mm3;
        assert!(r.volume_mm3 < solid * 0.4, "hollow: {} of {solid}", r.volume_mm3);
        // a closed cylinder and a hollowed slot and outline
        assert!(eval(serde_json::json!({"type": "shell", "child": {"type": "cylinder", "d": 30, "h": 20}, "wall": 1.6, "closed": true})).analyze().watertight);
        assert!(eval(serde_json::json!({"type": "shell", "child": {"type": "slot", "length": 50, "width": 20, "h": 10}, "wall": 2})).analyze().watertight);
        assert!(eval(serde_json::json!({"type": "shell", "child": {"type": "extrude", "points": [[0, 0], [40, 0], [40, 30], [0, 30]], "height": 15, "corner_radius": 6}, "wall": 2})).analyze().watertight);
        let node: Node = serde_json::from_value(serde_json::json!({"type": "shell", "child": {"type": "sphere", "d": 20}, "wall": 2})).unwrap();
        assert!(Evaluator::new(".").eval(&node, &Params::new()).is_err(), "spheres can't be shelled this way");
    }
}
