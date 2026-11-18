//! Arranging objects against an anchor, like Efficient Elements does for slides: align, dock,
//! distribute or stack, grid, circle, swap, snap to plate cells, match or stretch sizes, copy a
//! transform. The anchor is an object (by default the last of `ids` when there are several) or
//! the plate (the area both H2D nozzles reach). Locked objects are left where they are.
//!
//! Plate axes: left/right are −X/+X, front/back are −Y/+Y (the front edge is y = 0), whatever
//! the camera shows.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::tools::{ToolError, ToolOutput};
use crate::Studio;

#[derive(Deserialize, JsonSchema, Default)]
pub struct ArrangeIn {
    /// pack (default) | align | dock | distribute | grid | circle | swap | snap | match_size |
    /// copy_transform
    #[serde(default)]
    pub op: Option<String>,
    /// Default: the selection.
    #[serde(default)]
    pub ids: Option<Vec<String>>,
    /// Object id or "plate". Default: the last of several ids, else the plate.
    #[serde(default)]
    pub anchor: Option<String>,
    /// left | center | right
    #[serde(default)]
    pub x: Option<String>,
    /// front | middle | back
    #[serde(default)]
    pub y: Option<String>,
    /// mm. distribute without gap = equal gaps.
    #[serde(default)]
    pub gap: Option<f32>,
    #[serde(default)]
    pub spacing: Option<f32>,
    /// distribute: x | y. match_size: x | y | z | xy | xyz
    #[serde(default)]
    pub axis: Option<String>,
    #[serde(default)]
    pub cols: Option<u32>,
    #[serde(default)]
    pub radius: Option<f32>,
    #[serde(default)]
    pub start_deg: Option<f32>,
    #[serde(default)]
    pub face_out: bool,
    /// snap cell, mm (default 10)
    #[serde(default)]
    pub cell: Option<f32>,
    /// match_size: match | stretch | fill. copy_transform: position | rotation | scale | all
    #[serde(default)]
    pub mode: Option<String>,
}

/// An object's footprint on the plate (world XY bounds) and height.
#[derive(Clone, Copy, Debug)]
struct Fp {
    min: [f32; 2],
    max: [f32; 2],
    h: f32,
}

impl Fp {
    fn c(&self, a: usize) -> f32 {
        (self.min[a] + self.max[a]) / 2.0
    }
    fn size(&self, a: usize) -> f32 {
        self.max[a] - self.min[a]
    }
}

fn axis_index(s: &str) -> Result<usize, ToolError> {
    match s {
        "x" | "X" => Ok(0),
        "y" | "Y" => Ok(1),
        _ => Err(ToolError::new("bad_axis", format!("axis '{s}'"), "use \"x\" or \"y\"")),
    }
}

/// The area both nozzles reach (what "the plate" means for aligning).
fn plate_fp(s: &Studio) -> Fp {
    let b = &s.bed;
    let a = b.nozzle_areas.iter().fold([0.0f32, 0.0, b.size[0], b.size[1]], |a, n| [a[0].max(n[0]), a[1].max(n[1]), a[2].min(n[2]), a[3].min(n[3])]);
    Fp { min: [a[0], a[1]], max: [a[2], a[3]], h: b.max_height }
}

pub fn run(s: &mut Studio, i: ArrangeIn) -> Result<ToolOutput, ToolError> {
    let op = i.op.clone().unwrap_or_else(|| "pack".into());
    let mut ids = i.ids.clone().unwrap_or_else(|| s.selection.clone());
    if ids.is_empty() {
        return Err(ToolError::new("nothing_selected", format!("{op} needs objects"), "pass ids, or select objects first"));
    }
    for id in &ids {
        if s.project.object(id).is_none() {
            return Err(ToolError::new("not_found", format!("no object '{id}'"), "call `describe` to list ids"));
        }
    }
    let plates: std::collections::BTreeSet<u32> = ids.iter().filter_map(|id| s.project.object(id)).map(|o| o.plate).collect();
    if plates.len() > 1 {
        return Err(ToolError::new("several_plates", "those objects are on different plates", "arrange the objects of one plate at a time"));
    }
    if let Some(p) = plates.first() {
        s.set_plate(*p);
    }
    // the anchor: named, else the last of several, else the plate
    let anchor: Option<String> = match i.anchor.as_deref() {
        Some("plate") => None,
        Some(a) => {
            if s.project.object(a).is_none() {
                return Err(ToolError::new("not_found", format!("no anchor object '{a}'"), "pass an object id or \"plate\""));
            }
            Some(a.to_string())
        }
        None if ids.len() > 1 && op != "distribute" && op != "grid" && op != "swap" && op != "snap" => ids.last().cloned(),
        None => None,
    };
    if let Some(a) = &anchor {
        ids.retain(|x| x != a);
    }
    let objs = s.evaluated()?;
    let fp_of = |id: &str| -> Option<Fp> {
        objs.iter().find(|o| o.id == id).map(|o| {
            let b = o.report.bounds;
            Fp { min: [b.min.x, b.min.y], max: [b.max.x, b.max.y], h: b.max.z - b.min.z }
        })
    };
    let anchor_fp = match &anchor {
        Some(a) => fp_of(a).ok_or_else(|| ToolError::new("not_found", format!("anchor '{a}' is not on this plate"), ""))?,
        None => plate_fp(s),
    };
    let locked: Vec<String> = ids.iter().filter(|id| s.project.object(id).is_some_and(|o| o.locked)).cloned().collect();
    ids.retain(|id| !locked.contains(id));
    if ids.is_empty() {
        return Err(ToolError::new("locked", "every object to move is locked", "unlock them with update_object {\"locked\": false}"));
    }
    let fps: Vec<(String, Fp)> = ids.iter().filter_map(|id| fp_of(id).map(|f| (id.clone(), f))).collect();
    // new footprint centres (and rotations / scales) to write back
    let mut moves: Vec<(String, [f32; 2])> = Vec::new();
    let mut rotations: Vec<(String, [f32; 3])> = Vec::new();
    let mut scales: Vec<(String, [f32; 3], [f32; 2])> = Vec::new();
    let against = anchor.clone().unwrap_or_else(|| "the plate".into());
    let what: String;
    match op.as_str() {
        "align" | "dock" => {
            let dock = op == "dock";
            if dock && anchor.is_none() {
                return Err(ToolError::new("needs_anchor", "dock moves objects against another object", "pass anchor, or use align to line up with the plate"));
            }
            if i.x.is_none() && i.y.is_none() {
                return Err(ToolError::new("missing_side", format!("{op} needs x and/or y"), "x: left | center | right, y: front | middle | back"));
            }
            let gap = i.gap.unwrap_or(0.0);
            let a = anchor_fp;
            for (id, f) in &fps {
                let mut c = [f.c(0), f.c(1)];
                for (axis, side) in [(0usize, i.x.as_deref()), (1usize, i.y.as_deref())] {
                    let Some(side) = side else { continue };
                    let half = f.size(axis) / 2.0;
                    c[axis] = match (side, dock) {
                        ("left" | "front", false) => a.min[axis] + half,
                        ("center" | "middle", _) => a.c(axis),
                        ("right" | "back", false) => a.max[axis] - half,
                        ("left" | "front", true) => a.min[axis] - gap - half,
                        ("right" | "back", true) => a.max[axis] + gap + half,
                        (other, _) => return Err(ToolError::new("bad_side", format!("'{other}'"), "x: left | center | right, y: front | middle | back")),
                    };
                }
                moves.push((id.clone(), c));
            }
            what = format!("{} {} to {against}", if dock { "Docked" } else { "Aligned" }, [i.x.as_deref(), i.y.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" and "));
        }
        "distribute" => {
            let axis = axis_index(i.axis.as_deref().unwrap_or("x"))?;
            if fps.len() < 2 {
                return Err(ToolError::new("too_few", "distribute needs at least two objects", "pass more ids"));
            }
            match i.gap {
                // stack in the given order, each `gap` after the previous
                Some(gap) => {
                    let mut edge = fps[0].1.min[axis];
                    for (id, f) in &fps {
                        let mut c = [f.c(0), f.c(1)];
                        c[axis] = edge + f.size(axis) / 2.0;
                        edge += f.size(axis) + gap;
                        moves.push((id.clone(), c));
                    }
                    what = format!("Stacked {} along {} with {gap} mm gaps", fps.len(), if axis == 0 { "X" } else { "Y" });
                }
                // equal gaps; the outer two stay
                None => {
                    let mut sorted = fps.clone();
                    sorted.sort_by(|a, b| a.1.c(axis).total_cmp(&b.1.c(axis)));
                    let span = sorted.last().unwrap().1.max[axis] - sorted[0].1.min[axis];
                    let total: f32 = sorted.iter().map(|(_, f)| f.size(axis)).sum();
                    let gap = (span - total) / (sorted.len() - 1) as f32;
                    let mut edge = sorted[0].1.min[axis];
                    for (id, f) in &sorted {
                        let mut c = [f.c(0), f.c(1)];
                        c[axis] = edge + f.size(axis) / 2.0;
                        edge += f.size(axis) + gap;
                        moves.push((id.clone(), c));
                    }
                    what = format!("Distributed {} along {} with equal {:.1} mm gaps", sorted.len(), if axis == 0 { "X" } else { "Y" }, gap);
                }
            }
        }
        "grid" => {
            let n = fps.len();
            let cols = i.cols.unwrap_or_else(|| (n as f32).sqrt().ceil() as u32).max(1) as usize;
            let gap = i.gap.unwrap_or(5.0);
            let cw = fps.iter().map(|(_, f)| f.size(0)).fold(0.0, f32::max) + gap;
            let ch = fps.iter().map(|(_, f)| f.size(1)).fold(0.0, f32::max) + gap;
            let start = fps[0].1.min;
            for (k, (id, _)) in fps.iter().enumerate() {
                let (r, c) = (k / cols, k % cols);
                moves.push((id.clone(), [start[0] + (c as f32 + 0.5) * cw - gap / 2.0, start[1] + (r as f32 + 0.5) * ch - gap / 2.0]));
            }
            what = format!("Arranged {n} in a grid of {cols} column(s), {gap} mm apart, in the order given");
        }
        "circle" => {
            let n = fps.len() as f32;
            let centre = [anchor_fp.c(0), anchor_fp.c(1)];
            let biggest = fps.iter().map(|(_, f)| f.size(0).max(f.size(1))).fold(0.0, f32::max);
            let gap = i.gap.unwrap_or(4.0);
            // default: just big enough that neighbours don't touch, and clear of an anchor object
            let fit = (biggest + gap) / (2.0 * (std::f32::consts::PI / n.max(2.0)).sin());
            let clear = if anchor.is_some() { anchor_fp.size(0).max(anchor_fp.size(1)) / 2.0 + gap + biggest / 2.0 } else { 0.0 };
            let radius = i.radius.unwrap_or(fit.max(clear));
            let start = i.start_deg.unwrap_or(90.0);
            for (k, (id, _)) in fps.iter().enumerate() {
                let ang = start + k as f32 * 360.0 / n;
                let r = ang.to_radians();
                moves.push((id.clone(), [centre[0] + radius * r.cos(), centre[1] + radius * r.sin()]));
                if i.face_out {
                    let rot = s.project.object(id).map(|o| o.rotation).unwrap_or([0.0; 3]);
                    rotations.push((id.clone(), [rot[0], rot[1], (ang - 90.0).rem_euclid(360.0)]));
                }
            }
            what = format!("Arranged {} on a {radius:.0} mm circle around {against}", fps.len());
        }
        "swap" => {
            if fps.len() != 2 {
                return Err(ToolError::new("needs_two", "swap exchanges two objects", "pass exactly two ids"));
            }
            moves.push((fps[0].0.clone(), [fps[1].1.c(0), fps[1].1.c(1)]));
            moves.push((fps[1].0.clone(), [fps[0].1.c(0), fps[0].1.c(1)]));
            what = format!("Swapped {} and {}", fps[0].0, fps[1].0);
        }
        "snap" => {
            let cell = i.cell.unwrap_or(10.0).max(1.0);
            for (id, f) in &fps {
                let snap = |v: f32| ((v / cell).floor() + 0.5) * cell;
                moves.push((id.clone(), [snap(f.c(0)), snap(f.c(1))]));
            }
            what = format!("Snapped {} to the centres of {cell} mm plate cells", fps.len());
        }
        "match_size" => {
            let Some(a_id) = &anchor else {
                return Err(ToolError::new("needs_anchor", "match_size copies another object's size", "pass anchor (or several ids: the last is the anchor)"));
            };
            let mode = i.mode.as_deref().unwrap_or("match");
            let axes: Vec<usize> = match i.axis.as_deref().unwrap_or(if mode == "match" { "xy" } else { "x" }) {
                "x" => vec![0],
                "y" => vec![1],
                "z" => vec![2],
                "xy" => vec![0, 1],
                "xyz" => vec![0, 1, 2],
                other => return Err(ToolError::new("bad_axis", format!("axis '{other}'"), "x | y | z | xy | xyz")),
            };
            if mode != "match" && axes.contains(&2) {
                return Err(ToolError::new("bad_axis", "stretch and fill work along x or y", "objects always rest on the plate"));
            }
            let a = anchor_fp;
            for (id, f) in &fps {
                let o = s.project.object(id).cloned().expect("checked");
                // world axis → the object's own scale axis (turns in 90° steps about Z only)
                let quarter = (o.rotation[2] / 90.0).round();
                if o.rotation[0].abs() > 0.01 || o.rotation[1].abs() > 0.01 || (o.rotation[2] - quarter * 90.0).abs() > 0.01 {
                    return Err(ToolError::new("rotated", format!("'{id}' is turned at an angle"), "match_size works on objects turned in 90° steps about Z"));
                }
                let swap_xy = (quarter as i32).rem_euclid(2) == 1;
                let local = |w: usize| if w < 2 && swap_xy { 1 - w } else { w };
                let mut scale = o.scale;
                let mut c = [f.c(0), f.c(1)];
                for &w in &axes {
                    let cur = if w == 2 { f.h } else { f.size(w) };
                    if cur <= 1e-6 {
                        continue;
                    }
                    let target = match mode {
                        "match" => {
                            if w == 2 {
                                a.h
                            } else {
                                a.size(w)
                            }
                        }
                        "stretch" | "fill" => {
                            // grow toward the anchor, keeping the far edge where it is
                            let before = f.c(w) < a.c(w);
                            let (lo, hi) = match (mode, before) {
                                ("stretch", true) => (f.min[w], a.max[w]),
                                ("stretch", false) => (a.min[w], f.max[w]),
                                (_, true) => (f.min[w], a.min[w] - i.gap.unwrap_or(0.0)),
                                (_, false) => (a.max[w] + i.gap.unwrap_or(0.0), f.max[w]),
                            };
                            if hi - lo <= 0.1 {
                                return Err(ToolError::new("no_room", format!("'{id}' can't {mode} toward '{a_id}' along {}", if w == 0 { "X" } else { "Y" }), "they overlap along that axis already"));
                            }
                            c[w] = (lo + hi) / 2.0;
                            hi - lo
                        }
                        other => return Err(ToolError::new("bad_mode", format!("mode '{other}'"), "match | stretch | fill")),
                    };
                    scale[local(w)] *= target / cur;
                }
                scales.push((id.clone(), scale.map(|v| (v * 10000.0).round() / 10000.0), c));
            }
            what = format!("{} {} to {a_id} along {}", match mode { "match" => "Matched the size of", "stretch" => "Stretched", _ => "Filled" }, fps.iter().map(|(i, _)| i.as_str()).collect::<Vec<_>>().join(", "), i.axis.as_deref().unwrap_or(if mode == "match" { "xy" } else { "x" }));
        }
        "copy_transform" => {
            let Some(a_id) = &anchor else {
                return Err(ToolError::new("needs_anchor", "copy_transform copies from another object", "pass anchor: the object to copy from"));
            };
            let src = s.project.object(a_id).cloned().expect("checked");
            let mode = i.mode.as_deref().unwrap_or("all");
            for (id, _) in &fps {
                let o = s.project.object_mut(id).expect("checked");
                if matches!(mode, "position" | "all") {
                    o.position = src.position.or(Some([anchor_fp.c(0), anchor_fp.c(1)]));
                }
                if matches!(mode, "rotation" | "all") {
                    o.rotation = src.rotation;
                }
                if matches!(mode, "scale" | "all") {
                    o.scale = src.scale;
                }
            }
            what = format!("Copied {mode} from {a_id} to {}", fps.iter().map(|(i, _)| i.as_str()).collect::<Vec<_>>().join(", "));
        }
        other => return Err(ToolError::new("bad_op", format!("unknown op '{other}'"), "pack | align | dock | distribute | grid | circle | swap | snap | match_size | copy_transform")),
    }
    for (id, c) in &moves {
        if let Some(o) = s.project.object_mut(id) {
            o.position = Some([(c[0] * 100.0).round() / 100.0, (c[1] * 100.0).round() / 100.0]);
        }
    }
    for (id, r) in &rotations {
        if let Some(o) = s.project.object_mut(id) {
            o.rotation = *r;
        }
    }
    for (id, sc, c) in &scales {
        if let Some(o) = s.project.object_mut(id) {
            o.scale = *sc;
            o.position = Some([(c[0] * 100.0).round() / 100.0, (c[1] * 100.0).round() / 100.0]);
        }
    }
    s.changed("arrange");
    // report where everything ended up, and anything now off the plate
    let objs = s.evaluated()?;
    let bed = s.bed.size;
    let touched: Vec<&str> = moves.iter().map(|m| m.0.as_str()).chain(scales.iter().map(|m| m.0.as_str())).chain(rotations.iter().map(|m| m.0.as_str())).chain(fps.iter().map(|f| f.0.as_str())).collect();
    let mut lines = Vec::new();
    let mut off = Vec::new();
    for o in objs.iter().filter(|o| touched.contains(&o.id.as_str())) {
        let b = o.report.bounds;
        if lines.iter().any(|l: &String| l.starts_with(&format!("{} ", o.id))) {
            continue;
        }
        lines.push(format!("{} at ({:.1}, {:.1}), {:.1} x {:.1} x {:.1} mm", o.id, (b.min.x + b.max.x) / 2.0, (b.min.y + b.max.y) / 2.0, o.report.size.x, o.report.size.y, o.report.size.z));
        if b.min.x < -0.01 || b.min.y < -0.01 || b.max.x > bed[0] + 0.01 || b.max.y > bed[1] + 0.01 {
            off.push(o.id.clone());
        }
    }
    let mut text = format!("{what}.\n{}", lines.join("\n"));
    if !locked.is_empty() {
        text.push_str(&format!("\nLeft in place (locked): {}.", locked.join(", ")));
    }
    if !off.is_empty() {
        text.push_str(&format!("\nNow off the plate: {} (undo, or pick another anchor or gap).", off.join(", ")));
    }
    Ok(ToolOutput::text(text).with_data(json!({"op": op, "anchor": anchor, "moved": moves.iter().map(|m| &m.0).collect::<Vec<_>>(), "locked": locked, "off_plate": off})))
}

/// Centres for up to `n` copies of object `src` in the free space of its plate, nearest the
/// original first (to its right before behind, left or in front). Nothing on the plate moves:
/// copies only go where their footprint, `gap` mm from every other one, fits inside the area
/// both nozzles reach. Fewer than `n` come back when the plate is full.
pub fn free_spots(s: &mut Studio, src: &str, n: usize, gap: f32) -> Result<Vec<[f32; 2]>, ToolError> {
    let objs = s.evaluated()?;
    let me = objs.iter().find(|o| o.id == src).ok_or_else(|| ToolError::new("not_found", src.to_string(), ""))?;
    let b = me.report.bounds;
    let (w, d) = (b.max.x - b.min.x, b.max.y - b.min.y);
    let home = [(b.min.x + b.max.x) / 2.0, (b.min.y + b.max.y) / 2.0];
    let taken: Vec<[f32; 4]> = objs.iter().filter(|o| !o.hidden).map(|o| o.report.bounds).map(|b| [b.min.x, b.min.y, b.max.x, b.max.y]).collect();
    // nothing goes where the printer keeps the plate clear: ask for more spots, keep the good ones
    let spots = free_spots_in(reach(s), taken, [w, d], home, if s.bed.exclude.is_empty() { n } else { n * 4 + 16 }, gap);
    Ok(spots.into_iter().filter(|c| s.bed.excluded([c[0] - w / 2.0, c[1] - d / 2.0, c[0] + w / 2.0, c[1] + d / 2.0]).is_none()).take(n).collect())
}

/// The area both nozzles reach, as [min x, min y, max x, max y].
pub fn reach(s: &Studio) -> [f32; 4] {
    let a = plate_fp(s);
    [a.min[0], a.min[1], a.max[0], a.max[1]]
}

/// Up to `n` centres for a `size` footprint inside `area`, `gap` mm clear of every `taken`
/// rectangle and of each other, nearest `home` first (at equal distance: right, behind, left,
/// in front). `home` itself comes back first when it is free.
pub fn free_spots_in(area: [f32; 4], mut taken: Vec<[f32; 4]>, size: [f32; 2], home: [f32; 2], n: usize, gap: f32) -> Vec<[f32; 2]> {
    let [w, d] = size;
    let fits = |c: [f32; 2], taken: &[[f32; 4]]| -> bool {
        let r = [c[0] - w / 2.0, c[1] - d / 2.0, c[0] + w / 2.0, c[1] + d / 2.0];
        let eps = 1e-3;
        r[0] >= area[0] - eps
            && r[1] >= area[1] - eps
            && r[2] <= area[2] + eps
            && r[3] <= area[3] + eps
            && taken.iter().all(|t| r[0] >= t[2] + gap - eps || r[2] <= t[0] - gap + eps || r[1] >= t[3] + gap - eps || r[3] <= t[1] - gap + eps)
    };
    // spots touching a footprint (side by side, edges lined up) and a coarse scan of the plate
    let beside = |t: [f32; 4]| -> Vec<[f32; 2]> {
        let (cy, cx) = ((t[1] + t[3]) / 2.0, (t[0] + t[2]) / 2.0);
        let (ys, xs) = ([cy, t[1] + d / 2.0, t[3] - d / 2.0], [cx, t[0] + w / 2.0, t[2] - w / 2.0]);
        let mut v = Vec::new();
        for y in ys {
            v.push([t[2] + gap + w / 2.0, y]);
            v.push([t[0] - gap - w / 2.0, y]);
        }
        for x in xs {
            v.push([x, t[3] + gap + d / 2.0]);
            v.push([x, t[1] - gap - d / 2.0]);
        }
        v
    };
    let mut cands: Vec<[f32; 2]> = taken.iter().flat_map(|t| beside(*t)).collect();
    cands.push(home);
    let step = (w.min(d) / 2.0).clamp(2.0, 20.0);
    let mut y = area[1] + d / 2.0;
    while y <= area[3] - d / 2.0 + 1e-3 {
        let mut x = area[0] + w / 2.0;
        while x <= area[2] - w / 2.0 + 1e-3 {
            cands.push([x, y]);
            x += step;
        }
        y += step;
    }
    let score = |c: &[f32; 2]| -> f32 {
        let (dx, dy) = (c[0] - home[0], c[1] - home[1]);
        let turn = dy.atan2(dx).rem_euclid(std::f32::consts::TAU);
        (dx * dx + dy * dy).sqrt() * (1.0 + 1e-4 * turn)
    };
    let mut out = Vec::new();
    while out.len() < n {
        cands.retain(|c| fits(*c, &taken));
        let Some(best) = cands.iter().copied().min_by(|a, b| score(a).total_cmp(&score(b))) else { break };
        let r = [best[0] - w / 2.0, best[1] - d / 2.0, best[0] + w / 2.0, best[1] + d / 2.0];
        taken.push(r);
        cands.extend(beside(r));
        out.push([(best[0] * 100.0).round() / 100.0, (best[1] * 100.0).round() / 100.0]);
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::{Actor, Studio};
    use serde_json::json;

    fn bounds(s: &mut Studio, id: &str) -> [f32; 4] {
        let o = s.evaluated().unwrap().iter().find(|o| o.id == id).unwrap().report.bounds;
        [o.min.x, o.min.y, o.max.x, o.max.y]
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.02
    }

    fn setup() -> Studio {
        let mut s = Studio::new();
        for (id, size, at) in [("a", [20, 20], [100, 100]), ("b", [10, 10], [200, 60]), ("c", [30, 10], [60, 250]), ("d", [40, 40], [175, 160])] {
            s.call("add_object", json!({"id": id, "shape": {"type": "box", "size": [size[0], size[1], 5]}, "position": at}), Actor::User).unwrap();
        }
        s
    }

    #[test]
    fn align_and_dock_against_an_object_or_the_plate() {
        let mut s = setup();
        let call = |s: &mut Studio, a: serde_json::Value| s.call("arrange", a, Actor::Agent).unwrap();
        // the last id is the anchor
        call(&mut s, json!({"op": "align", "ids": ["a", "d"], "x": "left", "y": "back"}));
        let (a, d) = (bounds(&mut s, "a"), bounds(&mut s, "d"));
        assert!(near(a[0], d[0]) && near(a[3], d[3]), "{a:?} {d:?}");
        call(&mut s, json!({"op": "dock", "ids": ["a"], "anchor": "d", "x": "right", "gap": 2}));
        let a = bounds(&mut s, "a");
        assert!(near(a[0], d[2] + 2.0), "{a:?}");
        // one object, no anchor: the plate (the area both nozzles reach, x 25–325)
        call(&mut s, json!({"op": "align", "ids": ["b"], "x": "center", "y": "front"}));
        let b = bounds(&mut s, "b");
        assert!(near((b[0] + b[2]) / 2.0, 175.0) && near(b[1], 0.0), "{b:?}");
        assert_eq!(s.call("arrange", json!({"op": "dock", "ids": ["b"], "x": "left"}), Actor::User).unwrap_err().code, "needs_anchor");
        // locked objects stay
        s.call("update_object", json!({"id": "c", "locked": true}), Actor::User).unwrap();
        let c0 = bounds(&mut s, "c");
        let r = call(&mut s, json!({"op": "align", "ids": ["c", "b", "d"], "x": "right"}));
        assert_eq!(bounds(&mut s, "c"), c0);
        assert!(r.text.contains("locked"), "{}", r.text);
    }

    #[test]
    fn distribute_stack_grid_circle_swap_snap() {
        let mut s = setup();
        let call = |s: &mut Studio, a: serde_json::Value| s.call("arrange", a, Actor::Agent).unwrap();
        call(&mut s, json!({"op": "distribute", "ids": ["a", "b", "c"], "axis": "x"}));
        let (a, b, c) = (bounds(&mut s, "a"), bounds(&mut s, "b"), bounds(&mut s, "c"));
        // c (x 45–75) and b (x 195–205) stay at the ends; a sits in between with equal gaps
        assert!(near(c[0], 45.0) && near(b[2], 205.0), "{a:?} {b:?} {c:?}");
        assert!(near(a[0] - c[2], b[0] - a[2]), "{a:?} {b:?} {c:?}");
        // stack in the given order, touching
        call(&mut s, json!({"op": "distribute", "ids": ["b", "a", "c"], "axis": "y", "gap": 0}));
        let (a, b, c) = (bounds(&mut s, "a"), bounds(&mut s, "b"), bounds(&mut s, "c"));
        assert!(near(a[1], b[3]) && near(c[1], a[3]), "{a:?} {b:?} {c:?}");
        // grid in the order given, from the first object's corner
        call(&mut s, json!({"op": "grid", "ids": ["a", "b", "c", "d"], "cols": 2, "gap": 5}));
        let (a, b, c) = (bounds(&mut s, "a"), bounds(&mut s, "b"), bounds(&mut s, "c"));
        assert!(near((a[1] + a[3]) / 2.0, (b[1] + b[3]) / 2.0), "a and b share the first row");
        assert!(c[1] > a[3], "c starts the second row");
        // a circle around the plate centre
        call(&mut s, json!({"op": "circle", "ids": ["a", "b", "c"], "anchor": "plate", "radius": 80}));
        for id in ["a", "b", "c"] {
            let o = bounds(&mut s, id);
            let (x, y) = ((o[0] + o[2]) / 2.0 - 175.0, (o[1] + o[3]) / 2.0 - 160.0);
            assert!(near((x * x + y * y).sqrt(), 80.0), "{id} {o:?}");
        }
        let (a0, b0) = (bounds(&mut s, "a"), bounds(&mut s, "b"));
        call(&mut s, json!({"op": "swap", "ids": ["a", "b"]}));
        let (a1, b1) = (bounds(&mut s, "a"), bounds(&mut s, "b"));
        assert!(near((a1[0] + a1[2]) / 2.0, (b0[0] + b0[2]) / 2.0) && near((b1[1] + b1[3]) / 2.0, (a0[1] + a0[3]) / 2.0));
        call(&mut s, json!({"op": "snap", "ids": ["a"], "cell": 10}));
        let a = bounds(&mut s, "a");
        assert!(near(((a[0] + a[2]) / 2.0).rem_euclid(10.0), 5.0), "{a:?}");
    }

    #[test]
    fn match_stretch_fill_and_copy_transform() {
        let mut s = setup();
        let call = |s: &mut Studio, a: serde_json::Value| s.call("arrange", a, Actor::Agent).unwrap();
        call(&mut s, json!({"op": "match_size", "ids": ["a", "d"]}));
        let a = bounds(&mut s, "a");
        assert!(near(a[2] - a[0], 40.0) && near(a[3] - a[1], 40.0), "{a:?}");
        // b (x 195–205) stretches left to d's far side (x 155): 50 mm wide, right edge kept
        call(&mut s, json!({"op": "match_size", "ids": ["b"], "anchor": "d", "mode": "stretch", "axis": "x"}));
        let b = bounds(&mut s, "b");
        assert!(near(b[0], 155.0) && near(b[2], 205.0), "{b:?}");
        s.call("undo", json!({}), Actor::User).unwrap();
        // fill: b grows left until it touches d (x 195)
        call(&mut s, json!({"op": "match_size", "ids": ["b"], "anchor": "d", "mode": "fill", "axis": "x"}));
        let b = bounds(&mut s, "b");
        assert!(near(b[0], 195.0) && near(b[2], 205.0), "{b:?}");
        s.call("update_object", json!({"id": "d", "rotation": [0, 0, 30]}), Actor::User).unwrap();
        call(&mut s, json!({"op": "copy_transform", "ids": ["c"], "anchor": "d", "mode": "rotation"}));
        assert_eq!(s.project.object("c").unwrap().rotation, [0.0, 0.0, 30.0]);
        assert_eq!(s.call("arrange", json!({"op": "match_size", "ids": ["c"], "anchor": "a"}), Actor::User).unwrap_err().code, "rotated");
    }

    #[test]
    fn duplicates_in_a_direction_or_a_grid() {
        let mut s = setup();
        let r = s.call("duplicate_object", json!({"id": "a", "copies": 2, "direction": "+x", "gap": 2}), Actor::User).unwrap();
        let ids: Vec<String> = r.data["ids"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        let (a, c1, c2) = (bounds(&mut s, "a"), bounds(&mut s, &ids[0]), bounds(&mut s, &ids[1]));
        assert!(near(c1[0], a[2] + 2.0) && near(c2[0], c1[2] + 2.0), "{a:?} {c1:?} {c2:?}");
        let before = s.project.objects.len();
        s.call("duplicate_object", json!({"id": "b", "grid": [2, 3], "gap": 5}), Actor::User).unwrap();
        assert_eq!(s.project.objects.len(), before + 5);
    }

    #[test]
    fn a_plain_duplicate_moves_nothing_and_fill_packs_the_free_space() {
        let mut s = setup();
        let ids = ["a", "b", "c", "d"];
        let before: Vec<[f32; 4]> = ids.iter().map(|id| bounds(&mut s, id)).collect();
        let r = s.call("duplicate_object", json!({"id": "a"}), Actor::User).unwrap();
        let copy = r.data["ids"][0].as_str().unwrap().to_string();
        let after: Vec<[f32; 4]> = ids.iter().map(|id| bounds(&mut s, id)).collect();
        assert_eq!(before, after, "nothing else moved");
        // the copy sits right of the original, 5 mm away, lined up
        let (a, c) = (bounds(&mut s, "a"), bounds(&mut s, &copy));
        assert!(near(c[0], a[2] + 5.0) && near(c[1], a[1]), "{a:?} {c:?}");
        // fill: as many as fit, none overlapping, all on the plate, the others still in place
        let r = s.call("duplicate_object", json!({"id": "b", "fill": true}), Actor::User).unwrap();
        let n = r.data["ids"].as_array().unwrap().len();
        assert!(n > 100, "{n} copies of a 10 mm box");
        let objs = s.evaluated().unwrap();
        let boxes: Vec<[f32; 4]> = objs.iter().map(|o| [o.report.bounds.min.x, o.report.bounds.min.y, o.report.bounds.max.x, o.report.bounds.max.y]).collect();
        for (i, p) in boxes.iter().enumerate() {
            assert!(p[0] >= 24.99 && p[2] <= 325.01, "{p:?}");
            for q in &boxes[i + 1..] {
                assert!(p[0] >= q[2] - 0.01 || p[2] <= q[0] + 0.01 || p[1] >= q[3] - 0.01 || p[3] <= q[1] + 0.01, "{p:?} overlaps {q:?}");
            }
        }
        assert_eq!(before[1..], ids[1..].iter().map(|id| bounds(&mut s, id)).collect::<Vec<_>>()[..]);
    }
}
