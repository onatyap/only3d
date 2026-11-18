//! OpenSCAD both ways: .scad files render through the OpenSCAD command line (their customizer
//! variables set with `-D`), and only3d objects are written out as OpenSCAD source.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use glam::Vec2;
use only3d_geom::{shapes, Aabb, Mesh};
use serde::Serialize;
use serde_json::Value;

use crate::expr::{self, Num, Params};
use crate::project::{Bed, Object, Project};
use crate::shape::{self, Evaluator, HolePlan, Node, Shape};
use crate::ModelError;

pub fn is_scad(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("scad"))
}

/// The OpenSCAD program: Homebrew's, /usr/local's, the macOS app's, the Windows installer's,
/// else `openscad` on PATH.
pub fn exe() -> &'static str {
    ["/opt/homebrew/bin/openscad", "/usr/local/bin/openscad", "/Applications/OpenSCAD.app/Contents/MacOS/OpenSCAD", r"C:\Program Files\OpenSCAD\openscad.exe"].into_iter().find(|p| Path::new(p).exists()).unwrap_or("openscad")
}

fn temp(ext: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!("only3d_scad_{}_{}.{ext}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)))
}

fn run(path: &Path, out: &Path, args: &[String]) -> Result<(), ModelError> {
    let err = |message: String| ModelError::Import { path: path.display().to_string(), message };
    let r = crate::proc::command(exe()).args(args).arg("-o").arg(out).arg(path).output().map_err(|e| err(format!("could not run OpenSCAD ({}): {e}", exe())))?;
    if !r.status.success() || !out.exists() {
        let text = String::from_utf8_lossy(&r.stderr);
        // its errors first, else the last lines it printed
        let mut lines: Vec<&str> = text.lines().filter(|l| l.contains("ERROR") || l.contains("WARNING")).take(4).collect();
        if lines.is_empty() {
            lines = text.lines().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
        }
        let _ = std::fs::remove_file(out);
        return Err(err(format!("OpenSCAD failed: {}", lines.join(" | "))));
    }
    Ok(())
}

/// Render a .scad file to a mesh with these `-D name=value` variables.
pub fn render(path: &Path, defines: &[(String, String)]) -> Result<Mesh, ModelError> {
    let out = temp("stl");
    let args: Vec<String> = defines.iter().flat_map(|(k, v)| ["-D".to_string(), format!("{k}={v}")]).collect();
    run(path, &out, &args)?;
    let m = only3d_geom::io::load_merged(&out).map_err(|e| ModelError::Import { path: path.display().to_string(), message: e.to_string() });
    let _ = std::fs::remove_file(&out);
    m
}

/// A customizer variable of a .scad file (a top-level assignment outside the `[Hidden]` tab).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ScadVar {
    pub name: String,
    /// The comment above it in the file.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub caption: String,
    /// Its customizer tab ("Parameters" when the file names none).
    pub group: String,
    /// The value in the file.
    pub initial: Value,
    /// number | vector | string | boolean
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    /// Drop-down choices (label, value).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<(String, Value)>,
}

/// The customizer variables of a .scad file, as OpenSCAD reads them.
pub fn describe(path: &Path) -> Result<Vec<ScadVar>, ModelError> {
    let out = temp("json");
    run(path, &out, &["--export-format".into(), "param".into()])?;
    let text = std::fs::read_to_string(&out).unwrap_or_default();
    let _ = std::fs::remove_file(&out);
    let v: Value = serde_json::from_str(&text).map_err(|e| ModelError::Import { path: path.display().to_string(), message: format!("OpenSCAD's parameter list: {e}") })?;
    Ok(parse_params(&v))
}

fn parse_params(v: &Value) -> Vec<ScadVar> {
    let f = |p: &Value, k: &str| p.get(k).and_then(Value::as_f64);
    v.get("parameters")
        .and_then(Value::as_array)
        .map(|ps| {
            ps.iter()
                .filter_map(|p| {
                    let initial = p.get("initial")?.clone();
                    let ty = p.get("type").and_then(Value::as_str).unwrap_or("number");
                    let kind = if initial.is_array() { "vector" } else { ty };
                    Some(ScadVar {
                        name: p.get("name")?.as_str()?.to_string(),
                        caption: p.get("caption").and_then(Value::as_str).unwrap_or("").to_string(),
                        group: p.get("group").and_then(Value::as_str).unwrap_or("Parameters").to_string(),
                        initial,
                        kind: kind.to_string(),
                        min: f(p, "min"),
                        max: f(p, "max"),
                        step: f(p, "step"),
                        options: p
                            .get("options")
                            .and_then(Value::as_array)
                            .map(|os| os.iter().filter_map(|o| Some((o.get("name").map(|n| n.as_str().map(String::from).unwrap_or_else(|| n.to_string())).unwrap_or_default(), o.get("value")?.clone()))).collect())
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The OpenSCAD literal for a variable's value. A string given for a number (or inside a vector)
/// is an expression over the project parameters.
pub fn literal(v: &Value, decl: Option<&ScadVar>, params: &Params) -> Result<String, ModelError> {
    let numeric = decl.is_some_and(|d| d.kind == "number" || d.kind == "vector");
    Ok(match v {
        Value::Null => "undef".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => fmt(n.as_f64().unwrap_or(0.0)),
        Value::String(s) if numeric => fmt(expr::eval(s, params)?),
        Value::String(s) => quote(s),
        Value::Array(a) => format!("[{}]", a.iter().map(|x| literal(x, decl, params)).collect::<Result<Vec<_>, _>>()?.join(", ")),
        Value::Object(_) => return Err(ModelError::Shape("OpenSCAD variables take numbers, text, true/false or lists".into())),
    })
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A number as short as it can be written (to a millionth).
pub fn fmt(v: f64) -> String {
    let r = (v * 1e6).round() / 1e6;
    if r == 0.0 {
        "0".into()
    } else {
        format!("{r}")
    }
}

fn fmt3(v: [f32; 3]) -> String {
    format!("[{}, {}, {}]", fmt4(v[0]), fmt4(v[1]), fmt4(v[2]))
}

/// `s` as an operand: bare when it is a number or a name, else in parentheses.
fn paren(s: &str) -> String {
    let t = s.strip_prefix('-').unwrap_or(s);
    if !t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') {
        s.to_string()
    } else {
        format!("({s})")
    }
}

fn indent(s: &str) -> String {
    s.lines().map(|l| if l.is_empty() { String::new() } else { format!("    {l}") }).collect::<Vec<_>>().join("\n")
}

fn block(head: &str, children: &[String]) -> String {
    format!("{head} {{\n{}\n}}", children.iter().map(|c| indent(c)).collect::<Vec<_>>().join("\n"))
}

/// A list of items, wrapped onto lines of about 100 characters when long.
fn list(items: &[String]) -> String {
    let one = items.join(", ");
    if one.len() <= 90 {
        return format!("[{one}]");
    }
    let mut lines = vec![String::new()];
    for it in items {
        let cur = lines.last_mut().unwrap();
        if !cur.is_empty() && cur.len() + it.len() > 96 {
            lines.push(String::new());
        }
        let cur = lines.last_mut().unwrap();
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(it);
        cur.push(',');
    }
    format!("[\n{}\n]", lines.iter().map(|l| format!("    {}", l.trim_end_matches(','))).collect::<Vec<_>>().join(",\n"))
}

/// The result of writing objects as OpenSCAD.
pub struct ScadExport {
    pub source: String,
    /// Meshes too big to write inline, saved as STL files next to the .scad (by file name).
    pub sidecars: Vec<(String, Mesh)>,
    /// What went out as a mesh rather than as OpenSCAD shapes.
    pub meshed: Vec<String>,
}

/// Biggest mesh written inline as a `polyhedron`; bigger ones go to an STL file beside it.
const INLINE_TRIS: usize = 1500;

const BUILTINS: &[&str] = &[
    "cube", "sphere", "cylinder", "polyhedron", "square", "circle", "polygon", "text", "import", "surface", "union", "difference", "intersection", "hull", "minkowski", "translate", "rotate", "scale", "resize", "mirror", "multmatrix", "color", "offset", "projection", "linear_extrude", "rotate_extrude", "render", "children", "echo", "assert", "for", "intersection_for", "if", "let", "each", "include", "use", "module", "function",
];

/// The helper modules, in the order they are written. Each is included only when used.
const HELPERS: &[(&str, &str)] = &[
    ("o3d_box", "module o3d_box(size, center = false) translate([0, 0, center ? 0 : size.z / 2]) cube(size, center = true);"),
    (
        "o3d_rounded_box",
        "// every edge rounded by r\nmodule o3d_rounded_box(size, r, center = false)\n    translate([0, 0, center ? -size.z / 2 : 0]) hull() for (x = [-1, 1], y = [-1, 1], z = [0, 1])\n        translate([x * (size.x / 2 - r), y * (size.y / 2 - r), r + z * (size.z - 2 * r)]) sphere(r);",
    ),
    (
        "o3d_chamfer_box",
        "// every edge bevelled 45° by c\nmodule o3d_chamfer_box(size, c, center = false)\n    translate([0, 0, center ? 0 : size.z / 2]) hull() {\n        cube([size.x, size.y - 2 * c, size.z - 2 * c], center = true);\n        cube([size.x - 2 * c, size.y, size.z - 2 * c], center = true);\n        cube([size.x - 2 * c, size.y - 2 * c, size.z], center = true);\n    }",
    ),
    (
        "o3d_turned",
        "// edge finishes: [0, 0] sharp, [1, s] 45° chamfer of s mm, [2, s] round fillet of radius s\nfunction o3d_edge(r, z, f, up, n = 8) =\n    f[0] == 2 ? [for (i = [0 : n]) let (a = 90 * i / n)\n        up ? [r - f[1] + f[1] * cos(a), z - f[1] + f[1] * sin(a)] : [r - f[1] + f[1] * sin(a), z + f[1] - f[1] * cos(a)]]\n    : f[0] == 1 ? (up ? [[r, z - f[1]], [r - f[1], z]] : [[r - f[1], z], [r, z + f[1]]])\n    : [[r, z]];\n// a cylinder or cone (radius r1 at the bottom, r2 at the top) with finished edges\nmodule o3d_turned(r1, r2, h, bottom = [0, 0], top = [0, 0], center = false)\n    translate([0, 0, center ? -h / 2 : 0]) rotate_extrude()\n        polygon(concat([[0, 0]], o3d_edge(r1, 0, bottom, false), o3d_edge(r2, h, top, true), [[0, h]]));",
    ),
    (
        "o3d_slab",
        "// a box with its 4 upright edges rounded by r, top and bottom edges finished (see o3d_edge)\nmodule o3d_slab(size, r, bottom = [0, 0], top = [0, 0], center = false)\n    translate([0, 0, center ? -size.z / 2 : 0]) hull() for (x = [-1, 1], y = [-1, 1])\n        translate([x * (size.x / 2 - r), y * (size.y / 2 - r), 0]) o3d_turned(r, r, size.z, bottom, top);",
    ),
    ("o3d_sphere", "module o3d_sphere(r, center = false) translate([0, 0, center ? 0 : r]) sphere(r);"),
    ("o3d_torus", "module o3d_torus(major, minor, center = false) translate([0, 0, center ? 0 : minor]) rotate_extrude() translate([major, 0]) circle(r = minor);"),
    ("o3d_slot", "// obround along X, `length` end to end\nmodule o3d_slot(length, width, h) hull() for (x = [-1, 1]) translate([x * (length - width) / 2, 0, 0]) cylinder(h = h, d = width);"),
    ("o3d_tube", "module o3d_tube(d, wall, h) difference() {\n    cylinder(h = h, d = d);\n    translate([0, 0, -1]) cylinder(h = h + 2, d = d - 2 * wall);\n}"),
    ("o3d_prism", "// regular prism, r across corners, a flat side facing -Y\nmodule o3d_prism(sides, r, h) rotate([0, 0, 180 / sides - 90]) cylinder(h = h, r = r, $fn = sides);"),
    ("o3d_wedge", "// ramp: full height at the back (+Y), zero at the front\nmodule o3d_wedge(size) rotate([90, 0, 90]) linear_extrude(size.x, center = true) polygon([[-size.y / 2, 0], [size.y / 2, 0], [size.y / 2, size.z]]);"),
    (
        "o3d_text",
        "// capitals `size` mm tall, centred (OpenSCAD's fonts differ a little from only3d's)\nmodule o3d_text(t, size, h, font = \"Liberation Sans\") linear_extrude(h) text(t, size = size / 0.955, font = font, halign = \"center\", valign = \"center\");",
    ),
    ("o3d_teardrop", "// a hole along X or Y with a 45° roof, so it prints without support\nmodule o3d_teardrop(r, length, axis = \"x\") rotate(axis == \"x\" ? [90, 0, 90] : [90, 0, 0])\n    linear_extrude(length, center = true) union() { circle(r); rotate(45) square(r); }"),
];

struct Writer<'a> {
    ev: &'a mut Evaluator,
    params: &'a Params,
    dir: PathBuf,
    stem: String,
    helpers: BTreeSet<&'static str>,
    /// Wrappers for included .scad files.
    includes: Vec<String>,
    sidecars: Vec<(String, Mesh)>,
    meshed: Vec<String>,
}

impl Writer<'_> {
    fn num(&self, n: &Num) -> String {
        match n {
            Num::Value(v) => fmt(*v),
            Num::Expr(e) => expr::to_scad(e).unwrap_or_else(|_| n.eval(self.params).map(fmt).unwrap_or_else(|_| "undef".into())),
        }
    }
    fn arg(&self, n: &Num) -> String {
        paren(&self.num(n))
    }
    fn val(&self, n: &Num) -> Result<f64, ModelError> {
        n.eval(self.params)
    }
    fn positive(&self, n: &Option<Num>) -> Result<bool, ModelError> {
        Ok(match n {
            Some(n) => self.val(n)? > 0.0,
            None => false,
        })
    }
    /// Half of a diameter: worked out when it is a number, else `d / 2`.
    fn half(&self, d: &Num) -> String {
        match d {
            Num::Value(v) => fmt(v / 2.0),
            _ => format!("{} / 2", self.arg(d)),
        }
    }
    fn v3(&self, a: &[Num; 3]) -> String {
        format!("[{}, {}, {}]", self.num(&a[0]), self.num(&a[1]), self.num(&a[2]))
    }
    fn helper(&mut self, name: &'static str) {
        self.helpers.insert(name);
        if name == "o3d_slab" {
            self.helpers.insert("o3d_turned");
        }
    }

    /// A path as written in the .scad: relative to its folder when near it (up to 3 levels up),
    /// so the two can move together; absolute otherwise.
    fn rel(&self, full: &Path) -> String {
        let real = dunce::canonicalize(full).unwrap_or_else(|_| full.to_path_buf());
        let (a, b): (Vec<_>, Vec<_>) = (self.dir.components().collect(), real.components().collect());
        let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
        if common <= 1 || a.len() - common > 3 {
            return full.display().to_string();
        }
        let mut p = PathBuf::new();
        for _ in common..a.len() {
            p.push("..");
        }
        for c in &b[common..] {
            p.push(c);
        }
        p.display().to_string()
    }

    fn node(&mut self, n: &Node) -> Result<String, ModelError> {
        let body = self.shape(&n.shape)?;
        let mut pre = String::new();
        if let Some(a) = &n.at {
            pre += &format!("translate({}) ", self.v3(a));
        }
        if let Some(m) = n.mirror {
            pre += &format!("mirror({}) ", fmt3(m));
        }
        if let Some(r) = &n.rotate {
            pre += &format!("rotate({}) ", self.v3(r));
        }
        if let Some(s) = &n.scale {
            pre += &format!("scale({}) ", self.v3(s));
        }
        // the shape's own notes go above its transforms
        let (mut notes, mut rest) = (Vec::new(), body.as_str());
        while let Some(line) = rest.strip_prefix("// ") {
            let (l, r) = line.split_once('\n').unwrap_or((line, ""));
            notes.push(format!("// {l}"));
            rest = r;
        }
        if let Some(l) = n.label.as_ref().filter(|l| !l.trim().is_empty()) {
            notes.insert(0, format!("// {}", l.replace('\n', " ")));
        }
        notes.push(format!("{pre}{rest}"));
        Ok(notes.join("\n"))
    }

    /// This shape as a mesh (when OpenSCAD has nothing that makes it the same way).
    fn as_mesh(&mut self, s: &Shape, what: &str) -> Result<String, ModelError> {
        let m = self.ev.eval(&Node::from(s.clone()), self.params)?;
        Ok(self.mesh(&m, what))
    }

    fn mesh(&mut self, m: &Mesh, what: &str) -> String {
        self.meshed.push(what.to_string());
        if m.triangle_count() > INLINE_TRIS {
            let name = format!("{}_{}.stl", self.stem, self.sidecars.len() + 1);
            self.sidecars.push((name.clone(), m.clone()));
            return format!("// {what} (a mesh)\nimport({});", quote(&name));
        }
        let pts: Vec<String> = m.positions.iter().map(|p| format!("[{}, {}, {}]", fmt4(p.x), fmt4(p.y), fmt4(p.z))).collect();
        // OpenSCAD wants each face clockwise seen from outside
        let faces: Vec<String> = m.indices.iter().map(|[a, b, c]| format!("[{a}, {c}, {b}]")).collect();
        format!("// {what} (a mesh)\npolyhedron(points = {}, faces = {});", list(&pts), list(&faces))
    }

    fn finish(&self, c: &Option<Num>, f: &Option<Num>) -> Result<Option<(u8, String, f64)>, ModelError> {
        if self.positive(f)? {
            let f = f.as_ref().unwrap();
            return Ok(Some((2, self.num(f), self.val(f)?)));
        }
        if self.positive(c)? {
            let c = c.as_ref().unwrap();
            return Ok(Some((1, self.num(c), self.val(c)?)));
        }
        Ok(None)
    }

    fn shape(&mut self, s: &Shape) -> Result<String, ModelError> {
        let p = self.params;
        let center = |c: bool| if c { ", center = true" } else { "" };
        let seg = |s: &Option<u32>| s.map(|n| format!(", $fn = {n}")).unwrap_or_default();
        Ok(match s {
            Shape::Box { size, radius, chamfer, corner_radius, chamfer_top, chamfer_bottom, fillet_top, fillet_bottom, center: c } => {
                let sz = self.v3(size);
                if self.positive(radius)? {
                    self.helper("o3d_rounded_box");
                    format!("o3d_rounded_box({sz}, {}{});", self.num(radius.as_ref().unwrap()), center(*c))
                } else if self.positive(chamfer)? {
                    self.helper("o3d_chamfer_box");
                    format!("o3d_chamfer_box({sz}, {}{});", self.num(chamfer.as_ref().unwrap()), center(*c))
                } else {
                    let (fb, ft) = (self.finish(chamfer_bottom, fillet_bottom)?, self.finish(chamfer_top, fillet_top)?);
                    let cr = corner_radius.as_ref().map(|n| self.val(n)).transpose()?.unwrap_or(0.0);
                    let need = fb.iter().chain(ft.iter()).map(|f| f.2).fold(0.0, f64::max);
                    if fb.is_none() && ft.is_none() && cr <= 0.0 {
                        self.helper("o3d_box");
                        format!("o3d_box({sz}{});", center(*c))
                    } else if cr > 0.0 && cr + 1e-9 >= need {
                        self.helper("o3d_slab");
                        let e = |name: &str, f: &Option<(u8, String, f64)>| f.as_ref().map(|(k, s, _)| format!(", {name} = [{k}, {s}]")).unwrap_or_default();
                        format!("o3d_slab({sz}, {}{}{}{});", self.num(corner_radius.as_ref().unwrap()), e("bottom", &fb), e("top", &ft), center(*c))
                    } else {
                        self.as_mesh(s, "box with finished edges")?
                    }
                }
            }
            Shape::Cylinder { d, r, h, d2, chamfer_top, chamfer_bottom, fillet_top, fillet_bottom, segments, center: c } => {
                let (fb, ft) = (self.finish(chamfer_bottom, fillet_bottom)?, self.finish(chamfer_top, fillet_top)?);
                let r1 = match (d, r) {
                    (Some(d), _) => self.half(d),
                    (None, Some(r)) => self.num(r),
                    _ => return Err(ModelError::Shape("cylinder needs `d` or `r`".into())),
                };
                if fb.is_none() && ft.is_none() {
                    let base = match (d, r) {
                        (Some(d), _) if d2.is_some() => format!("d1 = {}", self.num(d)),
                        (Some(d), _) => format!("d = {}", self.num(d)),
                        (None, Some(r)) if d2.is_some() => format!("r1 = {}", self.num(r)),
                        _ => format!("r = {r1}"),
                    };
                    let top = d2.as_ref().map(|d2| format!(", d2 = {}", self.num(d2))).unwrap_or_default();
                    format!("cylinder(h = {}, {base}{top}{}{});", self.num(h), center(*c), seg(segments))
                } else {
                    self.helper("o3d_turned");
                    let r2 = d2.as_ref().map(|d2| self.half(d2)).unwrap_or_else(|| r1.clone());
                    let e = |name: &str, f: &Option<(u8, String, f64)>| f.as_ref().map(|(k, s, _)| format!(", {name} = [{k}, {s}]")).unwrap_or_default();
                    format!("o3d_turned({r1}, {r2}, {}{}{}{}{});", self.num(h), e("bottom", &fb), e("top", &ft), center(*c), seg(segments))
                }
            }
            Shape::Sphere { d, r, segments, center: c } => {
                self.helper("o3d_sphere");
                let rad = match (d, r) {
                    (Some(d), _) => self.half(d),
                    (None, Some(r)) => self.num(r),
                    _ => return Err(ModelError::Shape("sphere needs `d` or `r`".into())),
                };
                format!("o3d_sphere({rad}{}{});", center(*c), seg(segments))
            }
            Shape::Torus { major, minor, center: c } => {
                self.helper("o3d_torus");
                format!("o3d_torus({}, {}{});", self.num(major), self.num(minor), center(*c))
            }
            Shape::Extrude { points, height, corner_radius } => {
                let rounded = corner_radius.as_ref().map(|r| self.val(r)).transpose()?.filter(|r| *r > 0.0);
                let pts: Vec<String> = match rounded {
                    // the rounded outline itself (only3d rounds inner and outer corners alike)
                    Some(r) => {
                        let v = points.iter().map(|q| Ok(Vec2::new(self.val(&q[0])? as f32, self.val(&q[1])? as f32))).collect::<Result<Vec<_>, ModelError>>()?;
                        let ccw = if shapes::signed_area(&v) < 0.0 { v.iter().rev().copied().collect() } else { v };
                        shapes::round_corners(&ccw, r as f32).iter().map(|q| format!("[{}, {}]", fmt4(q.x), fmt4(q.y))).collect()
                    }
                    None => points.iter().map(|q| format!("[{}, {}]", self.num(&q[0]), self.num(&q[1]))).collect(),
                };
                let note = if rounded.is_some() { format!("// corners rounded by {}\n", self.num(corner_radius.as_ref().unwrap())) } else { String::new() };
                format!("{note}linear_extrude(height = {}) polygon({});", self.num(height), list(&pts))
            }
            Shape::Slot { length, width, h } => {
                self.helper("o3d_slot");
                format!("o3d_slot({}, {}, {});", self.num(length), self.num(width), self.num(h))
            }
            Shape::Tube { d, wall, h } => {
                self.helper("o3d_tube");
                format!("o3d_tube({}, {}, {});", self.num(d), self.num(wall), self.num(h))
            }
            Shape::Polygon { sides, d, flats, h } => {
                self.helper("o3d_prism");
                let r = match (d, flats) {
                    (Some(d), _) => self.half(d),
                    (None, Some(Num::Value(f))) => fmt(f / 2.0 / (std::f64::consts::PI / *sides as f64).cos()),
                    (None, Some(f)) => format!("{} / 2 / cos(180 / {sides})", self.arg(f)),
                    _ => return Err(ModelError::Shape("polygon needs `d` or `flats`".into())),
                };
                format!("o3d_prism({sides}, {r}, {});", self.num(h))
            }
            Shape::Wedge { size } => {
                self.helper("o3d_wedge");
                format!("o3d_wedge({});", self.v3(size))
            }
            Shape::Text { text, size, h, font } => {
                self.helper("o3d_text");
                let f = if font.as_deref() == Some("mono") { ", \"Liberation Mono\"" } else { "" };
                format!("o3d_text({}, {}, {}{f});", quote(text), self.num(size), self.num(h))
            }
            Shape::Hole { screw, d, depth, kind, axis, fit } => {
                let fit = fit.as_ref().map(|f| self.val(f)).transpose()?.unwrap_or(self.ev.fit_mm);
                let dv = d.as_ref().map(|d| self.val(d)).transpose()?;
                let kind = kind.as_deref().unwrap_or("clearance");
                let what = format!("// {} {kind} hole, {} mm deep", screw.as_deref().map(String::from).unwrap_or_else(|| format!("{} mm", fmt(dv.unwrap_or(0.0)))), self.num(depth));
                let stmt = match shape::hole_plan(screw.as_deref(), dv, self.val(depth)?, kind, axis.as_deref().unwrap_or("z"), fit)? {
                    HolePlan::Teardrop { r, depth, axis } => {
                        self.helper("o3d_teardrop");
                        format!("o3d_teardrop({}, {}, \"{axis}\");", fmt(r as f64), fmt(depth as f64))
                    }
                    HolePlan::Down { cyls, nut } => {
                        let mut parts: Vec<String> = cyls
                            .iter()
                            .map(|[r1, r2, h, z0]| {
                                let rr = if r1 == r2 { format!("r = {}", fmt(*r1 as f64)) } else { format!("r1 = {}, r2 = {}", fmt(*r1 as f64), fmt(*r2 as f64)) };
                                format!("translate([0, 0, {}]) cylinder(h = {}, {rr});", fmt(*z0 as f64), fmt(*h as f64))
                            })
                            .collect();
                        if let Some((flats, h)) = nut {
                            self.helper("o3d_prism");
                            let r = flats as f64 / 2.0 / (std::f64::consts::PI / 6.0).cos();
                            parts.push(format!("translate([0, 0, {}]) o3d_prism(6, {}, {});", fmt(-h as f64), fmt(r), fmt(h as f64 + 0.5)));
                        }
                        if parts.len() == 1 {
                            parts.remove(0)
                        } else {
                            block("union()", &parts)
                        }
                    }
                };
                format!("{what}\n{stmt}")
            }
            Shape::Shell { child, wall, floor, closed } => {
                let inner = shape::shell_inner(child, self.val(wall)?, floor.as_ref().map(|f| self.val(f)).transpose()?, *closed, p)?;
                let outer = self.node(child)?;
                let inner = self.node(&inner)?;
                block("difference()", &[outer, format!("// hollow: {} mm walls\n{inner}", self.num(wall))])
            }
            Shape::Revolve { profile, segments } => {
                let pts: Vec<String> = profile.iter().map(|q| format!("[{}, {}]", self.num(&q[0]), self.num(&q[1]))).collect();
                format!("rotate_extrude({}) polygon({});", segments.map(|n| format!("$fn = {n}")).unwrap_or_default(), list(&pts))
            }
            Shape::Import { path, vars, object, index, parts, repair } => {
                let full = shape::resolve_path(&self.ev.base_dir, path);
                let whole = object.is_none() && index.is_none() && parts.is_none() && !repair;
                let ext = full.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
                if whole && ext == "scad" {
                    // the file itself, its variables set after it (the last assignment wins)
                    let decl = self.ev.scad_vars(path).ok();
                    let mut sets = Vec::new();
                    for (k, v) in vars {
                        let d = decl.as_ref().and_then(|d| d.iter().find(|x| &x.name == k));
                        let numeric = d.is_some_and(|d| d.kind == "number");
                        let val = match v {
                            Value::String(e) if numeric => expr::to_scad(e).unwrap_or_else(|_| literal(v, d, p).unwrap_or_else(|_| "undef".into())),
                            _ => literal(v, d, p)?,
                        };
                        sets.push(format!("    {k} = {val};"));
                    }
                    let name = format!("o3d_scad_{}", self.includes.len() + 1);
                    self.includes.push(format!("// {}\nmodule {name}() {{\n    include <{}>\n{}}}", full.file_name().and_then(|n| n.to_str()).unwrap_or("file"), self.rel(&full), sets.iter().map(|s| format!("{s}\n")).collect::<String>()));
                    format!("{name}();")
                } else if whole && matches!(ext.as_str(), "stl" | "obj" | "3mf") {
                    format!("import({});", quote(&self.rel(&full)))
                } else {
                    let name = full.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
                    self.as_mesh(s, &name)?
                }
            }
            Shape::Union { children } | Shape::Difference { children } | Shape::Intersection { children } | Shape::Hull { children } => {
                let head = match s {
                    Shape::Union { .. } => "union()",
                    Shape::Difference { .. } => "difference()",
                    Shape::Intersection { .. } => "intersection()",
                    _ => "hull()",
                };
                let kids = children.iter().map(|c| self.node(c)).collect::<Result<Vec<_>, _>>()?;
                if kids.len() == 1 && head != "hull()" {
                    kids.into_iter().next().unwrap()
                } else {
                    block(head, &kids)
                }
            }
            Shape::Array { child, count, spacing } => {
                let body = self.node(child)?;
                let names = ["ix", "iy", "iz"].map(|n| if p.contains_key(n) { format!("o3d_{n}") } else { n.to_string() });
                let (mut loops, mut offs) = (Vec::new(), Vec::new());
                for a in 0..3 {
                    let n = count[a].max(1);
                    if n > 1 {
                        loops.push(format!("{} = [0 : {}]", names[a], n - 1));
                        offs.push(format!("{} * {}", names[a], self.arg(&spacing[a])));
                    } else {
                        offs.push("0".into());
                    }
                }
                if loops.is_empty() {
                    body
                } else {
                    format!("for ({}) translate([{}]) {body}", loops.join(", "), offs.join(", "))
                }
            }
            Shape::Polar { child, count, angle } => {
                let body = self.node(child)?;
                let c = (*count).clamp(1, 1000);
                let ang = angle.as_ref().map(|a| self.val(a)).transpose()?.unwrap_or(360.0);
                if c == 1 {
                    body
                } else {
                    let step = if (ang - 360.0).abs() < 1e-9 { fmt(360.0 / c as f64) } else { format!("{} / {}", self.arg(angle.as_ref().unwrap()), c.max(2) - 1) };
                    let i = if p.contains_key("ia") { "o3d_ia" } else { "ia" };
                    format!("for ({i} = [0 : {}]) rotate([0, 0, {i} * {}]) {body}", c - 1, paren(&step))
                }
            }
            Shape::PushPull { child, point, normal, distance } => {
                let body = self.node(child)?;
                let d = self.val(distance)? as f32;
                if d.abs() < 1e-4 {
                    return Ok(body);
                }
                let m = self.ev.eval(child, p)?;
                let face = only3d_geom::faces::face_at(&m, glam::Vec3::from(*point), glam::Vec3::from(*normal), 0.01).ok_or_else(|| ModelError::Shape("push_pull: its face is gone (the shape under it changed)".into()))?;
                let block_m = if d > 0.0 { only3d_geom::faces::prism(&m, &face, -0.01, d) } else { only3d_geom::faces::prism(&m, &face, d, 0.01) };
                let what = format!("Push/Pull: a flat face {} by {} mm", if d > 0.0 { "pulled out" } else { "pushed in" }, fmt(d.abs() as f64));
                let piece = self.mesh(&block_m, &what);
                block(if d > 0.0 { "union()" } else { "difference()" }, &[body, piece])
            }
        })
    }
}

fn fmt4(v: f32) -> String {
    let r = ((v as f64) * 1e4).round() / 1e4;
    if r == 0.0 {
        "0".into()
    } else {
        format!("{r}")
    }
}

/// A module name for an object id: a valid OpenSCAD name that hides no built-in and no other.
fn module_name(id: &str, taken: &mut BTreeSet<String>) -> String {
    let mut n: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    if n.is_empty() || n.starts_with(|c: char| c.is_ascii_digit()) {
        n = format!("obj_{n}");
    }
    if BUILTINS.contains(&n.as_str()) || n.starts_with("o3d_") {
        n = format!("{n}_part");
    }
    let mut out = n.clone();
    let mut k = 2;
    while !taken.insert(out.clone()) {
        out = format!("{n}_{k}");
        k += 1;
    }
    out
}

/// Write `objects` (as placed on their plate) as an OpenSCAD file at `out`: the project's
/// parameters as variables, each object as a module, and the lines that place them.
pub fn export(project: &Project, objects: &[&Object], ev: &mut Evaluator, bed: &Bed, out: &Path) -> Result<ScadExport, ModelError> {
    let params = &project.params;
    let dir = out.parent().map(Path::to_path_buf).unwrap_or_default();
    let dir = dunce::canonicalize(&dir).unwrap_or(dir);
    let stem = out.file_stem().and_then(|s| s.to_str()).unwrap_or("model").to_string();
    let mut w = Writer { ev, params, dir, stem, helpers: BTreeSet::new(), includes: Vec::new(), sidecars: Vec::new(), meshed: Vec::new() };
    let mut taken = BTreeSet::new();
    let (mut modules, mut placed) = (Vec::new(), Vec::new());
    for o in objects {
        let at = |e: ModelError| ModelError::At { path: format!("objects[{}]", o.id), source: Box::new(e) };
        let mut bodies: Vec<(String, u32, &Node, String)> = vec![(module_name(&o.id, &mut taken), o.filament, &o.shape, if o.name.is_empty() { o.id.clone() } else { o.name.clone() })];
        for (k, part) in o.parts.iter().enumerate() {
            let label = if part.name.is_empty() { format!("part {}", k + 2) } else { part.name.clone() };
            bodies.push((module_name(&format!("{}_{}", o.id, label), &mut taken), part.filament, &part.shape, format!("{} · {label}", o.name)));
        }
        // placed as only3d places it: footprint centred on `position`, resting on the bed
        let xf = o.local_transform();
        let mut bb = Aabb::EMPTY;
        for (name, _, node, label) in &bodies {
            let code = w.node(node).map_err(at)?;
            modules.push(format!("// {label}\nmodule {name}() {{\n{}\n}}", indent(&code)));
            let b = w.ev.eval(node, params).map_err(at)?.transformed(&xf).bounds();
            if b.min.x.is_finite() {
                bb.extend(b.min);
                bb.extend(b.max);
            }
        }
        let pos = o.position.unwrap_or_else(|| bed.center());
        let shift = [pos[0] - (bb.min.x + bb.max.x) / 2.0, pos[1] - (bb.min.y + bb.max.y) / 2.0, -bb.min.z];
        let mut pre = format!("translate({})", fmt3(shift));
        if o.rotation.iter().any(|r| *r != 0.0) {
            pre += &format!(" rotate({})", fmt3(o.rotation));
        }
        if o.scale.iter().any(|s| *s != 1.0) {
            pre += &format!(" scale({})", fmt3(o.scale));
        }
        for (name, fil, _, _) in &bodies {
            let colour = project.filaments.get(*fil as usize - 1).map(|f| f.color.clone()).filter(|c| c.starts_with('#'));
            placed.push(format!("{}{pre} {name}();", colour.map(|c| format!("color({}) ", quote(&c))).unwrap_or_default()));
        }
    }

    let mut src = String::new();
    let title = if project.name.trim().is_empty() { "only3d model".to_string() } else { project.name.clone() };
    src += &format!("// {title}, exported from only3d\n// Millimetres, Z up. Every object is a module; the lines at the end place them as on the plate.\n\n");
    src += "$fa = 2;    // circles: at most 2° per facet\n$fs = 0.4;  // and facets no shorter than 0.4 mm\n\n";
    if !params.is_empty() {
        src += "/* [Parameters] */\n";
        for (k, v) in params {
            if k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !k.starts_with(|c: char| c.is_ascii_digit()) {
                src += &format!("{k} = {};\n", fmt(*v));
            } else {
                src += &format!("// {k} = {} (not a valid OpenSCAD name)\n", fmt(*v));
            }
        }
        src += "\n";
    }
    let helpers: Vec<&str> = HELPERS.iter().filter(|(n, _)| w.helpers.contains(n)).map(|(_, code)| *code).collect();
    if !helpers.is_empty() {
        src += "// only3d's shapes stand centred on X/Y with their base at Z = 0\n";
        src += &helpers.join("\n");
        src += "\n\n";
    }
    for inc in &w.includes {
        src += inc;
        src += "\n\n";
    }
    src += &modules.join("\n\n");
    src += "\n\n";
    src += &placed.join("\n");
    src += "\n";
    Ok(ScadExport { source: src, sidecars: w.sidecars, meshed: w.meshed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Object;

    #[test]
    fn values_become_openscad_literals() {
        let mut p = Params::new();
        p.insert("wall".into(), 2.0);
        let num = ScadVar { name: "w".into(), caption: String::new(), group: "Parameters".into(), initial: Value::from(1.0), kind: "number".into(), min: None, max: None, step: None, options: vec![] };
        assert_eq!(literal(&Value::from("wall*2"), Some(&num), &p).unwrap(), "4");
        assert_eq!(literal(&Value::from("say \"hi\""), None, &p).unwrap(), "\"say \\\"hi\\\"\"");
        assert_eq!(literal(&serde_json::json!([1, 2.5, true]), None, &p).unwrap(), "[1, 2.5, true]");
        assert!(literal(&Value::from("nope"), Some(&num), &p).is_err());
    }

    #[test]
    fn openscad_parameter_lists_are_read() {
        let v: Value = serde_json::from_str(r#"{"parameters":[{"caption":"Width","group":"Parameters","initial":40.0,"max":100.0,"min":10.0,"name":"width","step":1.0,"type":"number"},{"group":"Walls","initial":[10,20],"name":"size","type":"number"},{"group":"Walls","initial":"a","name":"style","options":[{"name":"a","value":"a"},{"name":"b","value":"b"}],"type":"string"}]}"#).unwrap();
        let ps = parse_params(&v);
        assert_eq!(ps.len(), 3);
        assert_eq!((ps[0].name.as_str(), ps[0].kind.as_str(), ps[0].min, ps[0].max), ("width", "number", Some(10.0), Some(100.0)));
        assert_eq!(ps[1].kind, "vector");
        assert_eq!(ps[2].options.len(), 2);
    }

    #[test]
    fn scad_imports_render_with_their_variables_set() {
        if crate::proc::command(exe()).arg("--version").output().is_err() {
            return; // no OpenSCAD here
        }
        let dir = std::env::temp_dir().join(format!("o3d_scad_vars_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("box.scad"), "// Width of the box\nwidth = 40; // [10:1:100]\nwall = 2;\nlabel = \"A\";\ncube([width, 10, wall]);\n").unwrap();
        let mut ev = Evaluator::new(&dir);
        let vars = ev.scad_vars("box.scad").unwrap();
        assert_eq!(vars.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(), ["width", "wall", "label"]);
        assert_eq!((vars[0].caption.as_str(), vars[0].min, vars[0].max), ("Width of the box", Some(10.0), Some(100.0)));
        let mut params = Params::new();
        params.insert("t".into(), 3.0);
        let size = |ev: &mut Evaluator, j: Value, p: &Params| ev.eval(&serde_json::from_value(j).unwrap(), p).map(|m| m.bounds().size());
        let s0 = size(&mut ev, serde_json::json!({"type": "import", "path": "box.scad"}), &params).unwrap();
        assert!((s0 - glam::Vec3::new(40.0, 10.0, 2.0)).length() < 1e-3, "{s0:?}");
        // a number, and an expression over the project's parameters
        let s1 = size(&mut ev, serde_json::json!({"type": "import", "path": "box.scad", "vars": {"width": 60, "wall": "t*2"}}), &params).unwrap();
        assert!((s1 - glam::Vec3::new(60.0, 10.0, 6.0)).length() < 1e-3, "{s1:?}");
        params.insert("t".into(), 4.0);
        let s2 = size(&mut ev, serde_json::json!({"type": "import", "path": "box.scad", "vars": {"width": 60, "wall": "t*2"}}), &params).unwrap();
        assert!((s2.z - 8.0).abs() < 1e-3, "follows the parameter: {s2:?}");
        assert!(size(&mut ev, serde_json::json!({"type": "import", "path": "box.scad", "vars": {"wall": "nope"}}), &params).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn module_names_are_valid_and_unique() {
        let mut t = BTreeSet::new();
        assert_eq!(module_name("lid-top", &mut t), "lid_top");
        assert_eq!(module_name("lid_top", &mut t), "lid_top_2");
        assert_eq!(module_name("cube", &mut t), "cube_part");
        assert_eq!(module_name("3d", &mut t), "obj_3d");
    }

    /// Every shape type writes out, and with OpenSCAD installed the file renders to the same
    /// volume only3d computes.
    #[test]
    fn objects_write_out_as_openscad_that_renders_the_same() {
        let dir = std::env::temp_dir().join(format!("o3d_scad_export_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shape: Node = serde_json::from_value(serde_json::json!({
            "type": "difference", "children": [
                {"type": "box", "size": [60, "w", 12], "corner_radius": 4, "fillet_bottom": 1.5, "chamfer_top": 1},
                {"type": "hole", "screw": "M3", "kind": "counterbore", "depth": 12, "at": [-20, 0, 12]},
                {"type": "cylinder", "d": "w / 4", "h": 30, "at": [20, 0, 2]},
                {"type": "polar", "count": 4, "child": {"type": "box", "size": [4, 4, 30], "at": [0, 10, 3]}},
                {"type": "text", "text": "OS", "size": 5, "h": 2, "at": [0, -12, 11]}
            ]
        }))
        .unwrap();
        let mut project = Project::default();
        project.params.insert("w".into(), 40.0);
        let mut o = Object::new("tray", shape);
        o.position = Some([100.0, 120.0]);
        o.rotation = [0.0, 0.0, 30.0];
        let mut b = Object::new("knob", serde_json::from_value(serde_json::json!({"type": "union", "children": [
            {"type": "cylinder", "d": 20, "h": 8, "fillet_top": 2},
            {"type": "slot", "length": 30, "width": 6, "h": 4},
            {"type": "polygon", "sides": 6, "flats": 10, "h": 12},
            {"type": "wedge", "size": [10, 12, 6], "at": [0, 14, 0]},
            {"type": "torus", "major": 12, "minor": 2},
            {"type": "sphere", "d": 8, "at": [0, 0, 10]},
            {"type": "array", "count": [3, 1, 1], "spacing": [5, 0, 0], "child": {"type": "box", "size": [2, 2, 14], "radius": 0.5}},
            {"type": "push_pull", "point": [0, 0, 4], "normal": [0, 0, 1], "distance": 3, "child": {"type": "box", "size": [6, 40, 4]}}
        ]})).unwrap());
        b.position = Some([220.0, 120.0]);
        project.objects = vec![o, b];
        let mut ev = Evaluator::new(&dir);
        let out = dir.join("export.scad");
        let objs: Vec<&Object> = project.objects.iter().collect();
        let x = export(&project, &objs, &mut ev, &Bed::h2d(), &out).unwrap();
        for (want, why) in [("w = 40;", "params become variables"), ("module tray()", "objects become modules"), ("o3d_slab([60, w, 12], 4, bottom = [2, 1.5], top = [1, 1]);", "a finished tray"), ("cylinder(h = 30, d = w / 4);", "expressions stay expressions"), ("for (ia = [0 : 3]) rotate([0, 0, ia * 90])", "polar copies"), ("o3d_turned(10, 10, 8, top = [2, 2]);", "numbers worked out"), ("// M3 counterbore hole, 12 mm deep\n        translate([-20, 0, 12]) union()", "notes above transforms"), ("translate([100", "placed as on the plate")] {
            assert!(x.source.contains(want), "{why}: no `{want}` in\n{}", x.source);
        }
        assert_eq!(x.meshed.len(), 1, "only the push/pull block is a mesh: {:?}", x.meshed);
        std::fs::write(&out, &x.source).unwrap();
        if Path::new(exe()).exists() || crate::proc::command(exe()).arg("--version").output().is_ok() {
            let m = render(&out, &[]).unwrap_or_else(|e| panic!("{e}\n{}", x.source));
            let theirs = m.analyze().volume_mm3;
            let ours: f64 = project.objects.iter().map(|o| project.evaluate_object(o, &mut ev, &Bed::h2d()).unwrap().report.volume_mm3).sum();
            eprintln!("OpenSCAD {theirs:.1} mm³, only3d {ours:.1} mm³");
            if let Ok(keep) = std::env::var("ONLY3D_KEEP_SCAD") {
                std::fs::write(keep, &x.source).unwrap();
            }
            // text fonts and facets differ a little
            assert!((theirs - ours).abs() / ours < 0.02, "OpenSCAD {theirs:.0} mm³ vs only3d {ours:.0} mm³\n{}", x.source);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
