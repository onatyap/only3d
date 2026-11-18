//! Layer regions → ordered toolpaths (walls, infill, surfaces, brim).

use rayon::prelude::*;
use serde::Serialize;

use crate::poly::{self, dist, Contour, Region, Shape, P};
use crate::settings::{FuzzySkin, InfillPattern, SeamPosition, SliceSettings, SupportType, WallOrder};
use crate::slicer::LayerStack;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    Brim,
    InnerWall,
    OuterWall,
    SparseInfill,
    SolidInfill,
    Bridge,
    BottomSurface,
    TopSurface,
    Ironing,
    Support,
    SupportInterface,
    PrimeTower,
}

impl Feature {
    /// Name Bambu Studio writes in `; FEATURE:` comments.
    pub fn bambu_name(self) -> &'static str {
        match self {
            Feature::Brim => "Brim",
            Feature::InnerWall => "Inner wall",
            Feature::OuterWall => "Outer wall",
            Feature::SparseInfill => "Sparse infill",
            Feature::SolidInfill => "Internal solid infill",
            Feature::Bridge => "Bridge",
            Feature::BottomSurface => "Bottom surface",
            Feature::TopSurface => "Top surface",
            Feature::Ironing => "Ironing",
            Feature::Support => "Support",
            Feature::SupportInterface => "Support interface",
            Feature::PrimeTower => "Prime tower",
        }
    }

    pub const ALL: [Feature; 12] = [
        Feature::Brim,
        Feature::InnerWall,
        Feature::OuterWall,
        Feature::SparseInfill,
        Feature::SolidInfill,
        Feature::Bridge,
        Feature::BottomSurface,
        Feature::TopSurface,
        Feature::Ironing,
        Feature::Support,
        Feature::SupportInterface,
        Feature::PrimeTower,
    ];

    /// Display colour used by previews (close to Bambu's palette).
    pub fn color(self) -> [u8; 3] {
        match self {
            Feature::Brim => [0, 140, 120],
            Feature::InnerWall => [255, 200, 60],
            Feature::OuterWall => [255, 120, 30],
            Feature::SparseInfill => [180, 40, 40],
            Feature::SolidInfill => [160, 60, 200],
            Feature::Bridge => [70, 120, 200],
            Feature::BottomSurface => [110, 170, 110],
            Feature::TopSurface => [230, 60, 60],
            Feature::Ironing => [255, 160, 160],
            Feature::Support => [120, 200, 120],
            Feature::SupportInterface => [60, 160, 80],
            Feature::PrimeTower => [140, 140, 150],
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Path {
    pub feature: Feature,
    pub pts: Vec<P>,
    pub closed: bool,
    pub width: f64,
    /// Flow multiplier on top of the filament flow ratio.
    pub flow: f64,
    /// Target speed (mm/s), already capped by the volumetric limit.
    pub speed: f64,
    /// Index into the job's objects.
    pub object: usize,
}

impl Path {
    pub fn length(&self) -> f64 {
        let mut l: f64 = self.pts.windows(2).map(|w| dist(w[0], w[1])).sum();
        if self.closed && self.pts.len() > 1 {
            l += dist(*self.pts.last().unwrap(), self.pts[0]);
        }
        l
    }
    pub fn start(&self) -> P {
        self.pts[0]
    }
    pub fn end(&self) -> P {
        if self.closed {
            self.pts[0]
        } else {
            *self.pts.last().unwrap()
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct LayerPlan {
    pub index: usize,
    pub z: f64,
    pub height: f64,
    pub paths: Vec<Path>,
    /// Objects that have material on this layer.
    pub objects: Vec<usize>,
}

/// Per-object input to the planner.
pub struct PlannerObject<'a> {
    pub regions: &'a [Region],
    pub settings: &'a SliceSettings,
    /// For a body of a multi-part object: the whole object's regions per layer, so a surface
    /// covered by another body (another colour) is not a top or bottom skin. None = `regions`.
    pub cover: Option<&'a [Region]>,
    /// Plan supports for this object (only the first body of a multi-part object does, under
    /// the whole object).
    pub support: bool,
}

struct Island {
    object: usize,
    walls: Vec<Path>,
    infill_area: Region,
}

/// Infill areas narrower than (1 - this) solid-line spacings are dropped (PrusaSlicer's value).
const INSET_OVERLAP_TOLERANCE: f64 = 0.4;

fn cap_speed(s: &SliceSettings, v: f64, w: f64, h: f64) -> f64 {
    let a = SliceSettings::bead_area(w, h);
    v.min(s.max_volumetric_speed / a).max(1.0)
}

/// Rotate a closed contour so it starts at index `i`.
fn rotate_to(c: &Contour, i: usize) -> Contour {
    let mut v = c[i..].to_vec();
    v.extend_from_slice(&c[..i]);
    v
}

fn seam_index(c: &Contour, seam: SeamPosition, near: P) -> usize {
    match seam {
        SeamPosition::Nearest => (0..c.len()).min_by(|&a, &b| dist(c[a], near).total_cmp(&dist(c[b], near))).unwrap_or(0),
        SeamPosition::Rear => {
            let ymax = c.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max);
            // among rear-most points (within 0.5 mm), prefer the sharpest corner (hides the seam)
            let n = c.len();
            (0..n)
                .filter(|&i| c[i][1] >= ymax - 0.5)
                .max_by(|&a, &b| {
                    let sharp = |i: usize| {
                        let (p, q, r) = (c[(i + n - 1) % n], c[i], c[(i + 1) % n]);
                        let (u, v) = ([q[0] - p[0], q[1] - p[1]], [r[0] - q[0], r[1] - q[1]]);
                        let lu = (u[0] * u[0] + u[1] * u[1]).sqrt().max(1e-9);
                        let lv = (v[0] * v[0] + v[1] * v[1]).sqrt().max(1e-9);
                        1.0 - (u[0] * v[0] + u[1] * v[1]) / (lu * lv)
                    };
                    sharp(a).total_cmp(&sharp(b)).then(c[a][1].total_cmp(&c[b][1]))
                })
                .unwrap_or(0)
        }
    }
}

/// Small deterministic generator (SplitMix64), so the same plate slices to the same G-code.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    /// Uniform in [0, 1).
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Bambu Studio's `should_fuzzify`: which wall loops get fuzzy skin.
fn should_fuzzify(s: &SliceSettings, layer: usize, loop_index: u32, is_contour: bool) -> bool {
    if s.fuzzy_skin == FuzzySkin::None || (layer == 0 && !s.fuzzy_skin_first_layer) {
        return false;
    }
    let contours = loop_index == 0 || s.fuzzy_skin == FuzzySkin::AllWalls;
    let holes = contours && matches!(s.fuzzy_skin, FuzzySkin::All | FuzzySkin::AllWalls);
    if is_contour { contours } else { holes }
}

/// Bambu Studio's classic fuzzy skin (`fuzzy_polyline`, closed): new points every 0.75 to 1.25
/// point distances along the loop, each pushed sideways by a uniform random amount within
/// ±thickness. The loop keeps its start (the seam) as the first new point lies on its first edge.
fn fuzzy_loop(pts: &[P], thickness: f64, point_distance: f64, rng: &mut Rng) -> Vec<P> {
    let n = pts.len();
    if n < 3 || point_distance <= 0.0 {
        return pts.to_vec();
    }
    let min_gap = point_distance * 0.75;
    let range = point_distance * 0.5;
    let mut left_over = rng.next() * min_gap / 2.0;
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let (p0, p1) = (pts[i], pts[(i + 1) % n]);
        let (dx, dy) = (p1[0] - p0[0], p1[1] - p0[1]);
        let len = (dx * dx + dy * dy).sqrt();
        let mut d = left_over;
        while d < len {
            let r = (rng.next() * 2.0 - 1.0) * thickness;
            let t = d / len;
            out.push([p0[0] + dx * t - dy / len * r, p0[1] + dy * t + dx / len * r]);
            d += min_gap + rng.next() * range;
        }
        left_over = d - len;
    }
    if out.len() >= 3 { out } else { pts.to_vec() }
}

fn walls_for_shape(shape: &Shape, layer: usize, h: f64, s: &SliceSettings, object: usize) -> Island {
    let first = layer == 0;
    let w_o = if first { s.first_layer_width } else { s.outer_wall_width };
    let w_i = if first { s.first_layer_width } else { s.inner_wall_width };
    let (sp_o, sp_i) = (SliceSettings::spacing(w_o, h), SliceSettings::spacing(w_i, h));
    let v_o = cap_speed(s, if first { s.first_layer_speed } else { s.outer_wall_speed }, w_o, h);
    let v_i = cap_speed(s, if first { s.first_layer_speed } else { s.inner_wall_speed }, w_i, h);

    let mut walls: Vec<Path> = Vec::new();
    let mut last_inset = 0.0;
    let mut last_width = w_o;
    let mut outer_seam: Option<P> = None;
    let mut inner_loops: Vec<Vec<Path>> = Vec::new();
    for k in 0..s.wall_loops.max(1) {
        let d = if k == 0 { w_o / 2.0 } else { w_o / 2.0 + (sp_o + sp_i) / 2.0 + (k - 1) as f64 * sp_i };
        let ring = poly::offset_shape(shape, -d);
        if ring.is_empty() {
            break;
        }
        last_inset = d;
        last_width = if k == 0 { w_o } else { w_i };
        let mut loops = Vec::new();
        for (si, sh) in ring.iter().enumerate() {
            for (ci, c) in sh.iter().enumerate() {
                if c.len() < 3 {
                    continue;
                }
                let start = if k == 0 {
                    let i = seam_index(c, s.seam, [0.0, 0.0]);
                    if outer_seam.is_none() {
                        outer_seam = Some(c[i]);
                    }
                    i
                } else {
                    seam_index(c, SeamPosition::Nearest, outer_seam.unwrap_or(c[0]))
                };
                let mut pts = rotate_to(c, start);
                if should_fuzzify(s, layer, k, ci == 0) {
                    let seed = ((object as u64) << 48) ^ ((layer as u64) << 24) ^ ((k as u64) << 16) ^ ((si as u64) << 8) ^ ci as u64;
                    pts = fuzzy_loop(&pts, s.fuzzy_skin_thickness, s.fuzzy_skin_point_distance, &mut Rng::new(seed));
                }
                loops.push(Path {
                    feature: if k == 0 { Feature::OuterWall } else { Feature::InnerWall },
                    pts,
                    closed: true,
                    width: if k == 0 { w_o } else { w_i },
                    flow: 1.0,
                    speed: if k == 0 { v_o } else { v_i },
                    object,
                });
            }
        }
        inner_loops.push(loops);
    }
    match s.wall_order {
        WallOrder::InnerOuter => {
            for l in inner_loops.iter().rev() {
                walls.extend(l.iter().cloned());
            }
        }
        WallOrder::OuterInner => {
            for l in &inner_loops {
                walls.extend(l.iter().cloned());
            }
        }
    }
    let infill_area = if inner_loops.is_empty() {
        Vec::new()
    } else {
        // The infill area reaches `infill_wall_overlap` into the last wall. Open it (shrink, then
        // grow back) by most of a solid line so strips too narrow for a line vanish, as
        // PrusaSlicer and Bambu Studio do. Otherwise, where the loops from two sides already
        // meet, the overlap alone leaves a sliver and the fill lays a full line in it.
        let inset = last_inset + SliceSettings::spacing(last_width, h) / 2.0 - s.infill_wall_overlap * w_i;
        let w_fill = if first { s.first_layer_width } else { s.solid_infill_width };
        let open = SliceSettings::spacing(w_fill, h) * (1.0 - INSET_OVERLAP_TOLERANCE) / 2.0;
        let core = poly::offset_shape(shape, -(inset + open));
        poly::drop_small(poly::offset(&core, open), 0.05)
    };
    Island { object, walls, infill_area }
}

fn point_in_region(r: &Region, p: P) -> bool {
    let mut inside = false;
    for s in r {
        for c in s {
            let n = c.len();
            let mut j = n - 1;
            for i in 0..n {
                let (a, b) = (c[i], c[j]);
                if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
                    inside = !inside;
                }
                j = i;
            }
        }
    }
    inside
}

/// Distance from `p` to the nearest edge of `r`.
fn dist_to_boundary(r: &Region, p: P) -> f64 {
    let mut best = f64::INFINITY;
    for c in r.iter().flatten() {
        let n = c.len();
        for i in 0..n {
            let (a, b) = (c[i], c[(i + 1) % n]);
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let l2 = dx * dx + dy * dy;
            let t = if l2 > 0.0 { (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
            best = best.min(dist(p, [a[0] + t * dx, a[1] + t * dy]));
        }
    }
    best
}

/// Turn hatch segments into as few polylines as possible by joining neighbouring ends with a
/// short extruded connector. Hatch ends lie on the area's edge, so a connector between them
/// runs along that edge: accept it when its midpoint is inside the area or within a quarter
/// line of the edge (a strict inside test fails at random on straight edges).
fn link_segments(segs: Vec<[P; 2]>, area: &Region, max_link: f64, width: f64) -> Vec<Vec<P>> {
    // Several chains stay open at once, so lines split by a hole (or any notch) grow one zig-zag
    // per side instead of jumping across the gap after every line.
    let ok = |a: P, b: P| {
        let mid = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
        dist(a, b) <= max_link && (point_in_region(area, mid) || dist_to_boundary(area, mid) <= width * 0.25)
    };
    let mut out: Vec<Vec<P>> = Vec::new();
    for seg in segs {
        let mut best: Option<(f64, usize, bool)> = None;
        for (ci, cur) in out.iter().enumerate() {
            let end = *cur.last().unwrap();
            for flip in [false, true] {
                let start = if flip { seg[1] } else { seg[0] };
                let d = dist(end, start);
                if best.is_none_or(|b| d < b.0) && ok(end, start) {
                    best = Some((d, ci, flip));
                }
            }
        }
        match best {
            Some((_, ci, flip)) => {
                let (a, b) = if flip { (seg[1], seg[0]) } else { (seg[0], seg[1]) };
                out[ci].push(a);
                out[ci].push(b);
            }
            None => out.push(vec![seg[0], seg[1]]),
        }
    }
    out
}

fn fill_paths(area: &Region, feature: Feature, angle: f64, spacing: f64, width: f64, speed: f64, flow: f64, object: usize, phase: f64) -> Vec<Path> {
    let segs = poly::hatch(area, angle, spacing, phase);
    link_segments(segs, area, spacing * 2.5, width)
        .into_iter()
        .map(|pts| Path { feature, pts, closed: false, width, flow, speed, object })
        .collect()
}

/// Pick the bridge direction that gives the shortest unsupported spans.
fn bridge_angle(area: &Region, spacing: f64) -> f64 {
    let mut best = (f64::INFINITY, 45.0);
    for a in [0.0, 45.0, 90.0, 135.0] {
        let segs = poly::hatch(area, a, spacing.max(0.5), 0.0);
        if segs.is_empty() {
            continue;
        }
        let maxlen = segs.iter().map(|s| dist(s[0], s[1])).fold(0.0, f64::max);
        if maxlen < best.0 {
            best = (maxlen, a);
        }
    }
    best.1
}

/// One layer of an object's supports.
#[derive(Clone, Debug, Default)]
pub struct SupportLayer {
    /// Grid supports: filled with lines. Tree supports: branch cross-sections, a loop each.
    pub body: Region,
    /// Dense interface lines: roofs under the part, floors where branches stand on it.
    pub iface: Region,
    /// Tree supports above the first layer: the loop round each branch.
    pub loops: Vec<Contour>,
    /// Tree supports: second loops, in thick trunks and the lowest millimetres of each branch.
    pub walls: Vec<Contour>,
    /// Tree supports: short lines under walls of the layer above that would otherwise hang over
    /// a trunk's hollow inside (where branches merge).
    pub ribs: Vec<Vec<P>>,
}

/// Parts of each layer that need support: not resting on the layer below, allowing overhangs
/// up to `support_angle` from horizontal, without slivers thinner than a line.
pub fn overhangs(regions: &[Region], stack: &LayerStack, s: &SliceSettings) -> Vec<Region> {
    let angle = s.support_angle.clamp(5.0, 85.0).to_radians();
    let w = s.outer_wall_width;
    (0..regions.len())
        .into_par_iter()
        .map(|l| {
            if l == 0 {
                return Vec::new();
            }
            let allow = stack.heights[l] / angle.tan();
            let o = poly::difference(&regions[l], &poly::offset(&regions[l - 1], allow));
            poly::drop_small(poly::offset(&poly::offset(&o, -w / 2.0), w / 2.0), 1.0)
        })
        .collect()
}

/// Support areas per layer for one object: (body, interface). `obstacles` are every object's
/// layers on the plate (tree branches keep clear of them all). Grid supports ("normal"):
/// overhangs are carried straight down until they reach the plate (or the model, unless
/// `support_on_plate_only`), keeping an XY and a Z gap to the part. Tree supports: see
/// [`crate::tree`].
pub fn support_regions(regions: &[Region], obstacles: &[Region], stack: &LayerStack, s: &SliceSettings) -> Vec<SupportLayer> {
    let n = regions.len();
    let mut out = vec![(Vec::new(), Vec::new()); n];
    if !s.support || n < 3 {
        return vec![SupportLayer::default(); n];
    }
    if s.support_type == SupportType::Tree {
        return crate::tree::support_regions(&crate::tree::overhangs(regions, stack, s), obstacles, stack, s);
    }
    let overhang = overhangs(regions, stack, s);
    let gap = s.support_z_gap_layers as usize + 1;
    let blocked: Vec<Region> = regions.par_iter().map(|r| poly::offset(r, s.support_xy_gap)).collect();
    // sweep down from the top, carrying the column outline
    let mut carry: Region = Vec::new();
    let mut fresh: Vec<Region> = vec![Vec::new(); n];
    for l in (0..n).rev() {
        let mut add: Region = Vec::new();
        if l + gap < n {
            add = overhang[l + gap].clone();
        }
        carry = poly::union(&carry, &add);
        if carry.is_empty() {
            continue;
        }
        if s.support_on_plate_only {
            // a column that hits the model is dropped entirely below that point
            let hit = poly::intersection(&carry, &regions[l]);
            if !hit.is_empty() {
                carry = poly::difference(&carry, &poly::offset(&hit, s.support_spacing));
            }
        }
        let body = poly::drop_small(poly::difference(&carry, &blocked[l]), 2.0);
        if !s.support_on_plate_only {
            // columns stop where they land on the model
            carry = poly::difference(&carry, &regions[l]);
        }
        fresh[l] = add;
        out[l].0 = body;
    }
    // interface: the top layers of each column
    for l in 0..n {
        let mut top: Region = Vec::new();
        for k in 0..s.support_interface_layers as usize {
            if l + gap + k < n {
                top = poly::union(&top, &overhang[l + gap + k]);
            }
        }
        if !top.is_empty() && !out[l].0.is_empty() {
            let iface = poly::intersection(&out[l].0, &poly::offset(&top, 0.5));
            out[l].0 = poly::drop_small(poly::difference(&out[l].0, &iface), 1.0);
            out[l].1 = poly::drop_small(iface, 1.0);
        }
    }
    let _ = fresh;
    out.into_iter().map(|(body, iface)| SupportLayer { body, iface, ..Default::default() }).collect()
}

/// Tree branches: a loop around each, hollow like Bambu Studio's organic trees, with the
/// second loops and ribs the tree planned. The first layer is filled for grip on the plate.
fn tree_paths(sup: &SupportLayer, first: bool, w: f64, h: f64, v: f64, object: usize) -> Vec<Path> {
    let path = |pts: Vec<P>, closed: bool| Path { feature: Feature::Support, pts, closed, width: w, flow: 1.0, speed: v, object };
    let mut out: Vec<Path> = if first { poly::offset(&sup.body, -w / 2.0).into_iter().flatten().filter(|c| c.len() >= 3).map(|c| path(c, true)).collect() } else { sup.loops.iter().map(|c| path(c.clone(), true)).collect() };
    out.extend(sup.walls.iter().filter(|c| c.len() >= 3).map(|c| path(c.clone(), true)));
    out.extend(sup.ribs.iter().filter(|r| r.len() >= 2).map(|r| path(r.clone(), false)));
    if first {
        let sp = SliceSettings::spacing(w, h);
        let inner = poly::offset(&sup.body, -w / 2.0 - sp * 0.5);
        out.extend(fill_paths(&inner, Feature::Support, 0.0, sp, w, v, 1.0, object, 0.0));
    }
    out
}

/// Plan every layer. `regions[obj][layer]` are the sliced regions.
pub fn plan(objects: &[PlannerObject], stack: &LayerStack, global: &SliceSettings) -> Vec<LayerPlan> {
    let n = stack.len();
    // Pass 1: walls and infill areas, per layer in parallel.
    let islands: Vec<Vec<Island>> = (0..n)
        .into_par_iter()
        .map(|l| {
            let h = stack.heights[l];
            let mut out = Vec::new();
            for (oi, o) in objects.iter().enumerate() {
                for shape in &o.regions[l] {
                    out.push(walls_for_shape(shape, l, h, o.settings, oi));
                }
            }
            out
        })
        .collect();

    // supports stay where the nozzle reaches: everything outside is in the way
    let outside: Option<Region> = global.support_area.map(|a| {
        let rect = |x0: f64, y0: f64, x1: f64, y1: f64| -> Region { vec![vec![vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]]] };
        let (m, e) = (100.0, global.outer_wall_width);
        poly::difference(&rect(a[0] - m, a[1] - m, a[2] + m, a[3] + m), &rect(a[0] + e, a[1] + e, a[2] - e, a[3] - e))
    });
    // tree branches keep clear of every object on the plate, not just their own
    let tree = objects.iter().any(|o| o.support && o.settings.support && o.settings.support_type == SupportType::Tree);
    let obstacles: Vec<Region> = if tree && (objects.len() > 1 || outside.is_some()) {
        (0..n).into_par_iter().map(|l| objects.iter().fold(outside.clone().unwrap_or_default(), |acc, o| poly::union(&acc, &o.cover.unwrap_or(o.regions)[l]))).collect()
    } else {
        Vec::new()
    };
    let mut supports: Vec<Vec<SupportLayer>> = objects
        .iter()
        .map(|o| {
            let own = o.cover.unwrap_or(o.regions);
            if o.support { support_regions(own, if obstacles.is_empty() { own } else { &obstacles }, stack, o.settings) } else { vec![SupportLayer::default(); n] }
        })
        .collect();
    if let Some(out) = &outside {
        for sup in supports.iter_mut().flatten() {
            sup.body = poly::difference(&sup.body, out);
            sup.iface = poly::difference(&sup.iface, out);
        }
    }
    // the brim has the first layer around the parts to itself
    if global.brim_width > 0.0 && n > 0 && supports.iter().any(|s| !s[0].body.is_empty() || !s[0].iface.is_empty()) {
        let first: Region = objects.iter().fold(Vec::new(), |acc, o| poly::union(&acc, &o.regions[0]));
        let outer: Region = first.iter().map(|s| vec![s[0].clone()]).collect();
        let zone = poly::difference(&poly::offset(&outer, global.brim_gap + global.brim_width + global.first_layer_width / 2.0), &outer);
        for sup in &mut supports {
            sup[0].body = poly::difference(&sup[0].body, &zone);
            sup[0].iface = poly::difference(&sup[0].iface, &zone);
        }
    }

    // Pass 2: classify infill (solid shells, surfaces, bridges, sparse) and order everything.
    let mut layers: Vec<LayerPlan> = (0..n)
        .into_par_iter()
        .map(|l| {
            let h = stack.heights[l];
            let first = l == 0;
            let mut by_island: Vec<(P, Vec<Path>)> = Vec::new();
            let mut present = Vec::new();
            for isl in &islands[l] {
                let o = &objects[isl.object];
                let s = o.settings;
                if !present.contains(&isl.object) {
                    present.push(isl.object);
                }
                let mut paths = isl.walls.clone();
                let infill = &isl.infill_area;
                // what covers this body above and below: the whole object, all its bodies
                let cov = o.cover.unwrap_or(o.regions);
                if !infill.is_empty() {
                    let empty: Region = Vec::new();
                    let above = if l + 1 < n { &cov[l + 1] } else { &empty };
                    let below = if l > 0 { &cov[l - 1] } else { &empty };
                    let mut cover_above: Option<Region> = None;
                    for j in 1..=s.top_layers as usize {
                        let r = if l + j < n { &cov[l + j] } else { &empty };
                        cover_above = Some(match cover_above {
                            None => r.clone(),
                            Some(c) => poly::intersection(&c, r),
                        });
                    }
                    let mut cover_below: Option<Region> = None;
                    for j in 1..=s.bottom_layers as usize {
                        let r = if l >= j { &cov[l - j] } else { &empty };
                        cover_below = Some(match cover_below {
                            None => r.clone(),
                            Some(c) => poly::intersection(&c, r),
                        });
                    }
                    let min_area = 4.0 * s.solid_infill_width * s.solid_infill_width;
                    let top = poly::drop_small(poly::difference(infill, above), min_area);
                    let bottom = if first { infill.clone() } else { poly::drop_small(poly::difference(infill, below), min_area) };
                    let shell_top = cover_above.map(|c| poly::difference(infill, &c)).unwrap_or_default();
                    let shell_bot = cover_below.map(|c| poly::difference(infill, &c)).unwrap_or_default();
                    let shell = poly::union(&shell_top, &shell_bot);
                    let internal_solid = poly::drop_small(poly::difference(&poly::difference(&shell, &top), &bottom), min_area);
                    let sparse = poly::drop_small(poly::difference(infill, &poly::union(&shell, &poly::union(&top, &bottom))), min_area);

                    let w_s = if first { s.first_layer_width } else { s.solid_infill_width };
                    let w_t = s.top_surface_width;
                    let w_sp = if first { s.first_layer_width } else { s.infill_width };
                    let solid_angle = if l % 2 == 0 { s.infill_angle } else { s.infill_angle + 90.0 };
                    let v_first = s.first_layer_infill_speed;
                    // sparse
                    if s.infill_density > 0.0 && !sparse.is_empty() {
                        let sp = SliceSettings::spacing(w_sp, h);
                        let v = cap_speed(s, if first { v_first } else { s.sparse_infill_speed }, w_sp, h);
                        match s.infill_pattern {
                            InfillPattern::Grid => {
                                let spacing = 2.0 * sp / s.infill_density;
                                paths.extend(fill_paths(&sparse, Feature::SparseInfill, s.infill_angle, spacing, w_sp, v, 1.0, isl.object, 0.0));
                                paths.extend(fill_paths(&sparse, Feature::SparseInfill, s.infill_angle + 90.0, spacing, w_sp, v, 1.0, isl.object, 0.0));
                            }
                            InfillPattern::Rectilinear => {
                                let spacing = sp / s.infill_density;
                                paths.extend(fill_paths(&sparse, Feature::SparseInfill, solid_angle, spacing, w_sp, v, 1.0, isl.object, 0.0));
                            }
                            InfillPattern::Concentric => {
                                let spacing = sp / s.infill_density;
                                let mut ring = poly::offset(&sparse, -w_sp / 2.0);
                                let mut guard = 0;
                                while !ring.is_empty() && guard < 500 {
                                    for sh in &ring {
                                        for c in sh {
                                            paths.push(Path { feature: Feature::SparseInfill, pts: c.clone(), closed: true, width: w_sp, flow: 1.0, speed: v, object: isl.object });
                                        }
                                    }
                                    ring = poly::offset(&ring, -spacing);
                                    guard += 1;
                                }
                            }
                        }
                    }
                    if !internal_solid.is_empty() {
                        let v = cap_speed(s, if first { v_first } else { s.solid_infill_speed }, w_s, h);
                        paths.extend(fill_paths(&internal_solid, Feature::SolidInfill, solid_angle, SliceSettings::spacing(w_s, h), w_s, v, 1.0, isl.object, 0.0));
                    }
                    if !bottom.is_empty() {
                        if first {
                            let v = cap_speed(s, v_first, w_s, h);
                            paths.extend(fill_paths(&bottom, Feature::BottomSurface, solid_angle, SliceSettings::spacing(w_s, h), w_s, v, 1.0, isl.object, 0.0));
                        } else {
                            let a = bridge_angle(&bottom, 1.0);
                            let v = cap_speed(s, s.bridge_speed, w_s, h);
                            paths.extend(fill_paths(&bottom, Feature::Bridge, a, SliceSettings::spacing(w_s, h), w_s, v, 1.0, isl.object, 0.0));
                        }
                    }
                    if !top.is_empty() {
                        let v = cap_speed(s, if first { v_first } else { s.top_surface_speed }, w_t, h);
                        paths.extend(fill_paths(&top, Feature::TopSurface, solid_angle, SliceSettings::spacing(w_t, h), w_t, v, 1.0, isl.object, 0.0));
                        if s.ironing && !first {
                            let iron = poly::offset(&top, -w_t / 2.0);
                            paths.extend(fill_paths(&iron, Feature::Ironing, solid_angle + 90.0, 0.1, w_t, 30.0, 0.1, isl.object, 0.0));
                        }
                    }
                }
                if let Some(p) = paths.first() {
                    by_island.push((p.start(), paths));
                }
            }
            // supports are islands of their own
            for (oi, sup) in supports.iter().enumerate() {
                let (body, iface) = (&sup[l].body, &sup[l].iface);
                let s = objects[oi].settings;
                let w = if first { s.first_layer_width } else { s.outer_wall_width };
                let mut paths = Vec::new();
                if !body.is_empty() {
                    let v = cap_speed(s, if first { s.first_layer_speed } else { s.support_speed }, w, h);
                    let a = if l % 2 == 0 { 0.0 } else { 90.0 };
                    if s.support_type == SupportType::Tree {
                        paths.extend(tree_paths(&sup[l], first, w, h, v, oi));
                    } else {
                        paths.extend(fill_paths(body, Feature::Support, a, s.support_spacing, w, v, 1.0, oi, 0.0));
                    }
                }
                if !iface.is_empty() {
                    let v = cap_speed(s, if first { s.first_layer_speed } else { s.support_interface_speed }, w, h);
                    let a = if l % 2 == 0 { 45.0 } else { 135.0 };
                    paths.extend(fill_paths(iface, Feature::SupportInterface, a, s.support_interface_spacing, w, v, 1.0, oi, 0.0));
                }
                if let Some(p) = paths.first() {
                    by_island.push((p.start(), paths));
                }
            }
            // order paths inside each island (islands themselves are ordered sequentially below,
            // continuing from where the previous layer ended)
            let mut islands_out: Vec<Vec<Path>> = Vec::new();
            for (_, mut paths) in by_island.drain(..) {
                let mut ordered: Vec<Path> = Vec::new();
                let mut cur = paths[0].start();
                // inside an island: walls in planned order, infill groups by feature, each
                // group nearest-neighbour ordered (open paths may be reversed)
                let walls: Vec<Path> = paths.iter().filter(|p| matches!(p.feature, Feature::InnerWall | Feature::OuterWall)).cloned().collect();
                paths.retain(|p| !matches!(p.feature, Feature::InnerWall | Feature::OuterWall));
                for w in walls {
                    cur = w.end();
                    ordered.push(w);
                }
                for f in [Feature::Support, Feature::SupportInterface, Feature::SparseInfill, Feature::SolidInfill, Feature::Bridge, Feature::BottomSurface, Feature::TopSurface, Feature::Ironing] {
                    let mut group: Vec<Path> = paths.iter().filter(|p| p.feature == f).cloned().collect();
                    while !group.is_empty() {
                        let (gi, rev) = (0..group.len())
                            .flat_map(|g| [(g, false), (g, true)])
                            .filter(|(g, r)| !*r || !group[*g].closed)
                            .min_by(|a, b| {
                                let pa = if a.1 { group[a.0].end() } else { group[a.0].start() };
                                let pb = if b.1 { group[b.0].end() } else { group[b.0].start() };
                                dist(pa, cur).total_cmp(&dist(pb, cur))
                            })
                            .unwrap();
                        let mut p = group.swap_remove(gi);
                        if rev {
                            p.pts.reverse();
                        }
                        cur = p.end();
                        ordered.push(p);
                    }
                }
                islands_out.push(ordered);
            }
            (LayerPlan { index: l, z: stack.tops[l], height: h, paths: Vec::new(), objects: present }, islands_out)
        })
        .collect::<Vec<_>>()
        .into_iter()
        .scan([0.0f64, 0.0f64], |cur, (mut plan, mut islands)| {
            // nearest-neighbour island order, carrying the nozzle position across layers
            while !islands.is_empty() {
                let i = (0..islands.len()).min_by(|&a, &b| dist(islands[a][0].start(), *cur).total_cmp(&dist(islands[b][0].start(), *cur))).unwrap();
                let isl = islands.swap_remove(i);
                if let Some(last) = isl.last() {
                    *cur = last.end();
                }
                plan.paths.extend(isl);
            }
            Some(plan)
        })
        .collect();

    // Brim on the first layer, printed before anything else.
    if global.brim_width > 0.0 && n > 0 {
        let w = global.first_layer_width;
        let sp = SliceSettings::spacing(w, stack.heights[0]);
        let mut first_region: Region = Vec::new();
        for o in objects {
            first_region = poly::union(&first_region, &o.regions[0]);
        }
        // outer contours only
        let outer: Region = first_region.iter().map(|s| vec![s[0].clone()]).collect();
        let loops = (global.brim_width / sp).ceil() as usize;
        let mut brim = Vec::new();
        for k in 0..loops {
            let ring = poly::offset(&outer, global.brim_gap + w / 2.0 + k as f64 * sp);
            for s in ring {
                if let Some(c) = s.first() {
                    brim.push(Path { feature: Feature::Brim, pts: c.clone(), closed: true, width: w, flow: 1.0, speed: cap_speed(global, global.first_layer_speed, w, stack.heights[0]), object: 0 });
                }
            }
        }
        brim.reverse(); // outermost first, finishing next to the part
        let mut l0 = std::mem::take(&mut layers[0].paths);
        brim.append(&mut l0);
        layers[0].paths = brim;
    }
    // Skirt: loops around everything on the first layer, printed first.
    if global.skirt_loops > 0 && n > 0 {
        let w = global.first_layer_width;
        let sp = SliceSettings::spacing(w, stack.heights[0]);
        let mut first_region: Region = Vec::new();
        for o in objects {
            first_region = poly::union(&first_region, &o.regions[0]);
        }
        let outer: Region = first_region.iter().map(|s| vec![s[0].clone()]).collect();
        let base = global.skirt_distance.max(0.5) + if global.brim_width > 0.0 { global.brim_width + global.brim_gap } else { 0.0 };
        let mut skirt = Vec::new();
        for k in (0..global.skirt_loops as usize).rev() {
            // merge nearby islands so the skirt is one loop around close parts
            let ring = poly::offset(&poly::offset(&outer, base + w / 2.0 + k as f64 * sp + 3.0), -3.0);
            for s in ring {
                if let Some(c) = s.first() {
                    skirt.push(Path { feature: Feature::Brim, pts: c.clone(), closed: true, width: w, flow: 1.0, speed: cap_speed(global, global.first_layer_speed, w, stack.heights[0]), object: 0 });
                }
            }
        }
        let mut l0 = std::mem::take(&mut layers[0].paths);
        skirt.append(&mut l0);
        layers[0].paths = skirt;
    }
    layers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slicer::{slice_mesh, LayerStack};

    #[test]
    fn supports_a_floating_shelf() {
        // a T: thin post with a wide slab on top -> the slab overhang needs support
        let mut post = only3d_geom::shapes::cuboid(glam::Vec3::new(4.0, 4.0, 10.0)).unwrap();
        let mut slab = only3d_geom::shapes::cuboid(glam::Vec3::new(30.0, 10.0, 2.0)).unwrap();
        slab.translate(glam::Vec3::new(0.0, 0.0, 10.0));
        post.append(&slab);
        post.translate(glam::Vec3::new(100.0, 100.0, 0.0));
        let stack = LayerStack::new(12.0, 0.2, 0.2);
        let (regions, _) = slice_mesh(&post, &stack.cut_planes());
        let s = SliceSettings { support: true, support_type: SupportType::Normal, ..Default::default() };
        let sup = support_regions(&regions, &regions, &stack, &s);
        let area0 = poly::region_area(&sup[0].body) + poly::region_area(&sup[0].iface);
        // 30x10 slab minus the 4x4 post and gaps: roughly 280 mm² of columns at the bed
        assert!(area0 > 200.0 && area0 < 300.0, "{area0}");
        // the layer right under the slab (z gap 1) is empty, the one below has interface
        let top = 10.0 / 0.2;
        assert!(sup[top as usize - 1].body.is_empty() && sup[top as usize - 1].iface.is_empty());
        assert!(!sup[top as usize - 2].iface.is_empty());
        let layers = plan(&[PlannerObject { regions: &regions, settings: &s, cover: None, support: true }], &stack, &s);
        assert!(layers[5].paths.iter().any(|p| p.feature == Feature::Support));
    }
    use glam::Vec3;
    use only3d_geom::shapes;

    #[test]
    fn solid_fill_around_a_hole_is_two_zigzags() {
        // 40x40 square with an 8 mm hole: every 45° hatch line through the hole splits in two,
        // and the pieces must still chain into one zig-zag per side (the first H2D test tile
        // printed 61 separate lines with a travel between each)
        let sq: Vec<P> = vec![[0.0, 0.0], [40.0, 0.0], [40.0, 40.0], [0.0, 40.0]];
        let hole: Vec<P> = (0..48).rev().map(|i| {
            let a = i as f64 / 48.0 * std::f64::consts::TAU;
            [20.0 + 4.0 * a.cos(), 20.0 + 4.0 * a.sin()]
        }).collect();
        let area: Region = vec![vec![sq, hole]];
        let paths = fill_paths(&area, Feature::BottomSurface, 45.0, 0.457, 0.5, 50.0, 1.0, 0, 0.0);
        assert!(paths.len() <= 3, "{} separate fill paths", paths.len());
        let n: usize = paths.iter().map(|p| p.pts.len()).sum();
        assert!(n > 150, "{n} points");
    }

    /// Plan a 40 mm square tube with walls `wall` thick and return the paths of a mid layer.
    fn tube_mid_layer(wall: f64) -> Vec<Path> {
        let sq = |x0: f64, s: f64| -> Contour { vec![[x0, x0], [x0 + s, x0], [x0 + s, x0 + s], [x0, x0 + s]] };
        let mut hole = sq(100.0 + wall, 40.0 - 2.0 * wall);
        hole.reverse();
        let ring = poly::from_loops(&[sq(100.0, 40.0), hole]);
        let stack = LayerStack::new(10.0, 0.2, 0.2);
        let regions: Vec<Region> = vec![ring; stack.len()];
        let s = SliceSettings::default();
        plan(&[PlannerObject { regions: &regions, settings: &s, cover: None, support: true }], &stack, &s).swap_remove(25).paths
    }

    #[test]
    fn thin_wall_whose_loops_meet_gets_no_infill() {
        // two loops a side need ~1.61 mm; at 1.6 mm the loops meet and the only area left is
        // the infill/wall overlap, which used to get a full-width extruded line every layer
        // (hex pen cup: +7 g and over-extruded walls)
        let paths = tube_mid_layer(1.6);
        let infill: Vec<&Path> = paths.iter().filter(|p| !matches!(p.feature, Feature::InnerWall | Feature::OuterWall)).collect();
        assert!(infill.is_empty(), "{} infill paths, {:.1} mm", infill.len(), infill.iter().map(|p| p.length()).sum::<f64>());
        assert_eq!(paths.len(), 4, "two loops on each side");
    }

    #[test]
    fn real_gap_between_wall_loops_is_still_filled() {
        // 2.4 mm leaves a ~0.8 mm gap between the outer and hole loops: that must not become a void
        let paths = tube_mid_layer(2.4);
        assert!(paths.iter().any(|p| p.feature == Feature::SparseInfill));
    }

    #[test]
    fn plans_a_cube() {
        let mut m = shapes::cuboid(Vec3::new(20.0, 20.0, 10.0)).unwrap();
        m.translate(Vec3::new(100.0, 100.0, 0.0));
        let stack = LayerStack::new(10.0, 0.2, 0.2);
        let (regions, _) = slice_mesh(&m, &stack.cut_planes());
        let s = SliceSettings::default();
        let layers = plan(&[PlannerObject { regions: &regions, settings: &s, cover: None, support: true }], &stack, &s);
        assert_eq!(layers.len(), 50);
        let feats = |l: &LayerPlan| l.paths.iter().map(|p| p.feature).collect::<std::collections::BTreeSet<_>>();
        assert!(feats(&layers[0]).contains(&Feature::BottomSurface));
        assert!(feats(&layers[25]).contains(&Feature::SparseInfill));
        assert!(!feats(&layers[25]).contains(&Feature::TopSurface));
        assert!(feats(&layers[49]).contains(&Feature::TopSurface));
        assert!(feats(&layers[46]).contains(&Feature::SolidInfill));
        // outer wall is the last wall and inset by half a line width
        let outer: Vec<&Path> = layers[10].paths.iter().filter(|p| p.feature == Feature::OuterWall).collect();
        assert_eq!(outer.len(), 1);
        let xs: Vec<f64> = outer[0].pts.iter().map(|p| p[0]).collect();
        let xmin = xs.iter().cloned().fold(f64::INFINITY, f64::min);
        assert!((xmin - (90.0 + 0.21)).abs() < 1e-3, "{xmin}");
    }

    fn tube_layers(fuzzy: FuzzySkin) -> Vec<LayerPlan> {
        let sq = |x0: f64, s: f64| -> Contour { vec![[x0, x0], [x0 + s, x0], [x0 + s, x0 + s], [x0, x0 + s]] };
        let mut hole = sq(110.0, 20.0);
        hole.reverse();
        let ring = poly::from_loops(&[sq(100.0, 40.0), hole]);
        let stack = LayerStack::new(10.0, 0.2, 0.2);
        let regions: Vec<Region> = vec![ring; stack.len()];
        let s = SliceSettings { fuzzy_skin: fuzzy, wall_loops: 3, ..Default::default() };
        plan(&[PlannerObject { regions: &regions, settings: &s, cover: None, support: true }], &stack, &s)
    }

    fn worst_offset(p: &Path, plain: &Path) -> f64 {
        let r: Region = vec![vec![plain.pts.clone()]];
        p.pts.iter().map(|&q| dist_to_boundary(&r, q)).fold(0.0, f64::max)
    }

    #[test]
    fn fuzzy_skin_roughens_only_the_outside_outer_wall() {
        let plain = tube_layers(FuzzySkin::None);
        let fuzzy = tube_layers(FuzzySkin::External);
        let outer = |l: &LayerPlan, outside: bool| -> Path {
            l.paths.iter().find(|p| p.feature == Feature::OuterWall && p.pts.iter().any(|q| q[0] < 105.0) == outside).unwrap().clone()
        };
        let (f, p) = (outer(&fuzzy[25], true), outer(&plain[25], true));
        // 160 mm around at 0.6..1.0 mm per point
        assert!(f.pts.len() > 150 && f.pts.len() < 270, "{}", f.pts.len());
        let w = worst_offset(&f, &p);
        assert!(w > 0.2 && w <= 0.3 + 1e-9, "jitter {w}");
        // the hole's outer wall, every inner wall and the first layer stay as they were
        assert_eq!(outer(&fuzzy[25], false).pts, outer(&plain[25], false).pts);
        let inner = |l: &LayerPlan| l.paths.iter().filter(|p| p.feature == Feature::InnerWall).map(|p| p.pts.clone()).collect::<Vec<_>>();
        assert_eq!(inner(&fuzzy[25]), inner(&plain[25]));
        assert_eq!(outer(&fuzzy[0], true).pts, outer(&plain[0], true).pts);
        // the same plate slices to the same G-code
        assert_eq!(tube_layers(FuzzySkin::External)[25].paths[0].pts, fuzzy[25].paths[0].pts);
    }

    #[test]
    fn fuzzy_skin_modes_pick_their_loops() {
        let plain = tube_layers(FuzzySkin::None);
        let count = |l: &LayerPlan, f: Feature| l.paths.iter().filter(|p| p.feature == f).map(|p| p.pts.len()).sum::<usize>();
        let all = tube_layers(FuzzySkin::All);
        // contour and hole outer walls both roughened, inner walls not
        assert!(count(&all[25], Feature::OuterWall) > 200);
        assert_eq!(count(&all[25], Feature::InnerWall), count(&plain[25], Feature::InnerWall));
        let every = tube_layers(FuzzySkin::AllWalls);
        assert!(count(&every[25], Feature::InnerWall) > 4 * count(&plain[25], Feature::InnerWall));
    }
}
