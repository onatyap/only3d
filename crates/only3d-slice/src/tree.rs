//! Tree supports, after Bambu Studio's default ("organic") trees, Thomas Rahm's take on Cura's.
//!
//! Every overhang gets a roof of interface layers, carried by tips spaced a few millimetres
//! apart. Going down, each tip leans towards its nearest neighbour (at most `tree_branch_angle`
//! from vertical, 25° when it only moves to merge), merges with it, thickens, and stands on the
//! plate on a wide foot. A branch keeps out of the "avoidance": the places from which it could
//! no longer reach the plate without running into the model. Where it cannot, it rests on the
//! model (on a floor of interface lines, below a Z gap), or is removed with its tips when
//! `support_on_plate_only` is set. Each layer's branches are the union of circles minus the
//! model grown by the XY gap, so supports never pass through the part. The planner prints
//! branches as hollow loops (a second loop for thick trunks) and the first layer solid.

use std::collections::HashMap;

use rayon::prelude::*;

use crate::poly::{self, dist, Contour, Region, P};
use crate::settings::SliceSettings;
use crate::slicer::LayerStack;

/// Radius of a branch's foot on the first layer (Bambu's `support_tree_bp_diameter` / 2).
const FOOT_RADIUS: f64 = 3.75;
/// Above the first layer a foot flares out from the trunk by at most this much (mm), at a
/// mean 12° from vertical (Bambu Studio 2.09's plate feet).
const FLARE: f64 = 1.2;
const FLARE_ANGLE: f64 = 12.0;
/// Steepest lean (degrees from vertical) for branches that only move to merge.
const SLOW_ANGLE: f64 = 25.0;
/// Bambu's `support_tree_top_rate` (%): how densely the tips sit under an overhang.
const TOP_RATE: f64 = 30.0;
/// Smallest roof worth printing (mm²).
const MIN_ROOF_AREA: f64 = 1.0;
/// Branches resting on the model are at least this tall (mm) unless they carry a roof.
const MIN_HEIGHT_TO_MODEL: f64 = 1.0;
/// Branches this big (mm² of a layer) get a second loop: Bambu's area for PLA, π/4 · 13.8².
pub const DOUBLE_WALL_AREA: f64 = 150.0;
/// Branches further apart than this (mm) never lean towards each other.
const MAX_REACH: f64 = 40.0;

struct Params {
    gap: f64,
    gap_min: f64,
    tip_r: f64,
    branch_r: f64,
    tip_layers: f64,
    grow: f64,
    /// How much a foot widens per layer down.
    flare: f64,
    tan_max: f64,
    tan_slow: f64,
    roof_layers: usize,
    z_gap: usize,
    connect: f64,
    spacing: f64,
    rest_on_model: bool,
}

impl Params {
    fn new(s: &SliceSettings) -> Params {
        let w = s.outer_wall_width;
        let h = s.layer_height.max(0.01);
        let tip_r = (s.tree_tip_diameter.max(w) / 2.0).min(s.tree_branch_diameter.max(w) / 2.0);
        let branch_r = (s.tree_branch_diameter / 2.0).max(tip_r);
        let angle = s.tree_branch_angle.clamp(0.0, 89.0);
        let connect = w * 100.0 / TOP_RATE + (2.0 * tip_r - w).max(0.0);
        Params {
            gap: s.support_xy_gap.max(0.01),
            gap_min: s.support_xy_gap.max(0.01).min(0.5 * w),
            tip_r,
            branch_r,
            tip_layers: ((branch_r - tip_r) / (w / 3.0)).max(branch_r / h).max(1.0),
            grow: s.tree_branch_diameter_angle.clamp(0.0, 89.0).to_radians().tan() * h,
            flare: FLARE_ANGLE.to_radians().tan() * h,
            tan_max: angle.to_radians().tan(),
            tan_slow: SLOW_ANGLE.min(angle).to_radians().tan(),
            roof_layers: s.support_interface_layers as usize,
            z_gap: s.support_z_gap_layers as usize,
            connect,
            // Bambu puts tips along the roof's lines every `connect`; under no roof, along lines
            // `tree_branch_distance` apart. A square grid of about the same density:
            spacing: if s.support_interface_layers > 0 { 1.4 * connect } else { (s.tree_branch_distance.max(1.0) * connect).sqrt() },
            rest_on_model: !s.support_on_plate_only,
        }
    }

    /// Radius of a branch `dtt` layers below its tip.
    fn radius(&self, dtt: f64) -> f64 {
        if dtt <= self.tip_layers {
            self.tip_r + (self.branch_r - self.tip_r) * dtt / self.tip_layers
        } else {
            self.branch_r + (dtt - self.tip_layers) * self.grow
        }
    }
}

/// A region's edges, for inside tests and nearest-boundary queries.
struct Edges {
    segs: Vec<(P, P)>,
    conts: Vec<([f64; 4], std::ops::Range<usize>)>,
}

impl Edges {
    fn new(r: &Region) -> Edges {
        let mut segs = Vec::new();
        let mut conts = Vec::new();
        for c in r.iter().flatten() {
            if c.len() < 3 {
                continue;
            }
            let s0 = segs.len();
            let mut bb = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
            for i in 0..c.len() {
                let a = c[i];
                segs.push((a, c[(i + 1) % c.len()]));
                bb = [bb[0].min(a[0]), bb[1].min(a[1]), bb[2].max(a[0]), bb[3].max(a[1])];
            }
            conts.push((bb, s0..segs.len()));
        }
        Edges { segs, conts }
    }

    fn inside(&self, p: P) -> bool {
        let mut inside = false;
        for (bb, rg) in &self.conts {
            if p[1] < bb[1] || p[1] > bb[3] || p[0] > bb[2] {
                continue;
            }
            for &(a, b) in &self.segs[rg.clone()] {
                if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
                    inside = !inside;
                }
            }
        }
        inside
    }

    /// The nearest boundary point closer than `max`: (distance, point, outward normal there).
    fn nearest(&self, p: P, max: f64) -> Option<(f64, P, P)> {
        let mut best = None;
        let mut lim = max;
        for (bb, rg) in &self.conts {
            let dx = (bb[0] - p[0]).max(p[0] - bb[2]).max(0.0);
            let dy = (bb[1] - p[1]).max(p[1] - bb[3]).max(0.0);
            if dx * dx + dy * dy >= lim * lim {
                continue;
            }
            for &(a, b) in &self.segs[rg.clone()] {
                let (ex, ey) = (b[0] - a[0], b[1] - a[1]);
                let l2 = ex * ex + ey * ey;
                let t = if l2 > 0.0 { (((p[0] - a[0]) * ex + (p[1] - a[1]) * ey) / l2).clamp(0.0, 1.0) } else { 0.0 };
                let q = [a[0] + t * ex, a[1] + t * ey];
                let d = dist(p, q);
                if d < lim {
                    lim = d;
                    let l = l2.sqrt().max(1e-12);
                    // outer contours run CCW and holes CW: the material is on the left
                    best = Some((d, q, [ey / l, -ex / l]));
                }
            }
        }
        best
    }

    /// At least `c` outside the region.
    fn clear(&self, p: P, c: f64) -> bool {
        !self.inside(p) && self.nearest(p, c).is_none()
    }

    /// The point nearest `p` that is at least `c` outside the region (a few steps along the
    /// boundary normal; check the result with `clear`).
    fn push_out(&self, mut q: P, c: f64) -> P {
        for _ in 0..4 {
            let inside = self.inside(q);
            let Some((d, b, n)) = self.nearest(q, if inside { f64::INFINITY } else { c }) else {
                return q;
            };
            let dir = if d > 1e-9 {
                let s = if inside { 1.0 } else { -1.0 };
                [s * (b[0] - q[0]) / d, s * (b[1] - q[1]) / d]
            } else {
                n
            };
            q = [b[0] + dir[0] * (c + 0.02), b[1] + dir[1] * (c + 0.02)];
        }
        q
    }
}

/// Points hashed into square cells, for neighbour searches.
struct Grid {
    cs: f64,
    cells: HashMap<(i64, i64), Vec<usize>>,
}

impl Grid {
    fn new(pts: &[P], cs: f64) -> Grid {
        let mut cells: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for (i, p) in pts.iter().enumerate() {
            cells.entry(Self::key(*p, cs)).or_default().push(i);
        }
        Grid { cs, cells }
    }

    fn key(p: P, cs: f64) -> (i64, i64) {
        ((p[0] / cs).floor() as i64, (p[1] / cs).floor() as i64)
    }

    /// The nearest other point within `max` (ties: the lower index).
    fn nearest(&self, pts: &[P], i: usize, max: f64) -> Option<(usize, f64)> {
        let (cx, cy) = Self::key(pts[i], self.cs);
        let rings = (max / self.cs).ceil() as i64 + 1;
        let mut best: Option<(usize, f64)> = None;
        for r in 0..=rings {
            if best.is_some_and(|b| b.1 <= (r - 1).max(0) as f64 * self.cs) {
                break;
            }
            for x in cx - r..=cx + r {
                for y in cy - r..=cy + r {
                    if (x - cx).abs() != r && (y - cy).abs() != r {
                        continue;
                    }
                    for &j in self.cells.get(&(x, y)).into_iter().flatten() {
                        let d = dist(pts[i], pts[j]);
                        if j != i && d <= max && best.is_none_or(|b| d < b.1 || (d == b.1 && j < b.0)) {
                            best = Some((j, d));
                        }
                    }
                }
            }
        }
        best
    }

    /// Every other point within `r`.
    fn within(&self, pts: &[P], i: usize, r: f64) -> Vec<usize> {
        let (cx, cy) = Self::key(pts[i], self.cs);
        let k = (r / self.cs).ceil() as i64;
        let mut out = Vec::new();
        for x in cx - k..=cx + k {
            for y in cy - k..=cy + k {
                for &j in self.cells.get(&(x, y)).into_iter().flatten() {
                    if j != i && dist(pts[i], pts[j]) <= r {
                        out.push(j);
                    }
                }
            }
        }
        out
    }
}

#[derive(Clone)]
struct Node {
    p: P,
    /// Effective layers below the tip: drives the radius (stops growing where it can't fit).
    dtt: f64,
    /// Layers below the tip.
    age: f64,
    to_plate: bool,
    /// Carries a roof (then it may rest on the model however short it is).
    roof: bool,
    parents: Vec<usize>,
    child: Option<usize>,
    dead: bool,
}

/// A branch on its way down one layer.
struct Step {
    p: P,
    dtt: f64,
    age: f64,
    to_plate: bool,
    roof: bool,
    /// The nodes above it.
    from: Vec<usize>,
}

struct Roof {
    layers: Vec<(usize, Region)>,
    tips: Vec<usize>,
    /// No tips: the roof reaches the plate or sits on the model.
    rests: bool,
}

/// One layer's overhang islands: each roof's layers, the layer of its tips (`usize::MAX`: the
/// roof reaches the plate) and the tips.
struct Planned {
    roofs: Vec<(Vec<(usize, Region)>, usize, Vec<P>)>,
}

fn sample_tips(area: &Region, spacing: f64, connect: f64) -> Vec<P> {
    let mut out = Vec::new();
    let mut bb = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for p in area.iter().flatten().flatten() {
        bb = [bb[0].min(p[0]), bb[1].min(p[1]), bb[2].max(p[0]), bb[3].max(p[1])];
    }
    if !bb[0].is_finite() {
        return out;
    }
    let idx = Edges::new(area);
    // a grid aligned to the plate, so overhangs stacked over each other share their tips
    let (i0, i1) = ((bb[0] / spacing).ceil() as i64, (bb[2] / spacing).floor() as i64);
    let (j0, j1) = ((bb[1] / spacing).ceil() as i64, (bb[3] / spacing).floor() as i64);
    for i in i0..=i1 {
        for j in j0..=j1 {
            let p = [i as f64 * spacing, j as f64 * spacing];
            if idx.inside(p) {
                out.push(p);
            }
        }
    }
    let perimeter: f64 = area.iter().flatten().map(|c| (0..c.len()).map(|i| dist(c[i], c[(i + 1) % c.len()])).sum::<f64>()).sum();
    let min_pts = (perimeter / connect).clamp(1.0, 3.0) as usize;
    if out.len() < min_pts {
        // too narrow for the grid (a sliver on a slope): tips along its outline instead
        out.clear();
        for c in area.iter().flatten() {
            out.extend(along(c, connect));
        }
    }
    out
}

/// Points along a closed contour, at most `step` apart (at least one).
fn along(c: &Contour, step: f64) -> Vec<P> {
    let n = c.len();
    let len: f64 = (0..n).map(|i| dist(c[i], c[(i + 1) % n])).sum();
    let k = (len / step).ceil().max(1.0) as usize;
    let gap = len / k as f64;
    let mut out = Vec::with_capacity(k);
    let (mut acc, mut next) = (0.0, gap / 2.0);
    for i in 0..n {
        let (a, b) = (c[i], c[(i + 1) % n]);
        let l = dist(a, b);
        while next <= acc + l && out.len() < k {
            let t = if l > 0.0 { (next - acc) / l } else { 0.0 };
            out.push([a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])]);
            next += gap;
        }
        acc += l;
    }
    out
}

fn circle(c: P, r: f64) -> Contour {
    let n = ((std::f64::consts::TAU * r / 0.4).ceil() as usize).clamp(8, 48);
    (0..n)
        .map(|i| {
            let a = i as f64 / n as f64 * std::f64::consts::TAU;
            [c[0] + r * a.cos(), c[1] + r * a.sin()]
        })
        .collect()
}

/// What tree supports hold up on each layer, as Bambu Studio finds it: the extrudable part of
/// a layer beyond the one below grown by the threshold slope (+1°), without overhang clusters
/// (connected from layer to layer) too small to matter, unless they start in mid-air.
pub fn overhangs(regions: &[Region], stack: &LayerStack, s: &SliceSettings) -> Vec<Region> {
    let n = regions.len();
    let w = s.outer_wall_width;
    let angle = (s.support_angle + 1.0).clamp(5.0, 89.0).to_radians();
    let extrudable: Vec<Region> = regions.par_iter().map(|r| poly::intersection(r, &poly::offset(&poly::offset(r, -w / 2.0), w))).collect();
    // each layer's pieces, and whether a piece floats (nothing of the layer below under it)
    let pieces: Vec<Vec<(Region, bool)>> = (0..n)
        .into_par_iter()
        .map(|l| {
            if l == 0 {
                return Vec::new();
            }
            let o = poly::difference(&extrudable[l], &poly::offset(&extrudable[l - 1], stack.heights[l - 1] / angle.tan()));
            let below = poly::offset(&regions[l - 1], w);
            poly::drop_small(o, 0.01).into_iter().map(|sh| {
                let r: Region = vec![sh];
                let floating = poly::intersection(&r, &below).is_empty();
                (r, floating)
            }).collect()
        })
        .collect();
    // clusters: a piece joins the cluster of a piece on the layer below that it overlaps
    let mut cluster_of: Vec<Vec<usize>> = Vec::with_capacity(n);
    let mut parent: Vec<usize> = Vec::new();
    fn find(p: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while p[r] != r {
            r = p[r];
        }
        p[i] = r;
        r
    }
    let bbox = |r: &Region| {
        let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for p in r.iter().flatten().flatten() {
            b = [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])];
        }
        b
    };
    let boxes: Vec<Vec<[f64; 4]>> = pieces.iter().map(|ps| ps.iter().map(|(r, _)| bbox(r)).collect()).collect();
    for l in 0..n {
        let mut ids = Vec::with_capacity(pieces[l].len());
        for (i, (piece, _)) in pieces[l].iter().enumerate() {
            let id = parent.len();
            parent.push(id);
            if l > 0 && !pieces[l - 1].is_empty() {
                let grown = poly::offset(piece, w);
                let a = boxes[l][i];
                for (k, (other, _)) in pieces[l - 1].iter().enumerate() {
                    let b = boxes[l - 1][k];
                    if a[0] - w > b[2] || b[0] > a[2] + w || a[1] - w > b[3] || b[1] > a[3] + w {
                        continue;
                    }
                    if !poly::intersection(&grown, other).is_empty() {
                        let (a, b) = (find(&mut parent, id), find(&mut parent, cluster_of[l - 1][k]));
                        parent[a.max(b)] = a.min(b);
                    }
                }
            }
            ids.push(id);
        }
        cluster_of.push(ids);
    }
    // per cluster: footprint, layers, narrowest width, and whether any piece floats
    struct Cluster {
        merged: Vec<Contour>,
        layers: Vec<usize>,
        narrow: Vec<f64>,
        area: f64,
        floating: bool,
    }
    let mut clusters: HashMap<usize, Cluster> = HashMap::new();
    for l in 0..n {
        for (k, (piece, floating)) in pieces[l].iter().enumerate() {
            let c = find(&mut parent, cluster_of[l][k]);
            let e = clusters.entry(c).or_insert(Cluster { merged: Vec::new(), layers: Vec::new(), narrow: Vec::new(), area: 0.0, floating: false });
            e.merged.extend(poly::offset(piece, w).into_iter().flatten());
            if e.layers.last() != Some(&l) {
                e.layers.push(l);
                e.narrow.push(0.0);
            }
            let mut narrow: f64 = 0.0;
            for sh in poly::offset(piece, -w / 2.0) {
                let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
                for p in &sh[0] {
                    lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                    hi = [hi[0].max(p[0]), hi[1].max(p[1])];
                }
                narrow = narrow.max((hi[0] - lo[0]).min(hi[1] - lo[1]));
            }
            *e.narrow.last_mut().expect("pushed above") = e.narrow.last().copied().unwrap_or(0.0).max(narrow);
            e.area += poly::region_area(piece);
            e.floating |= floating;
        }
    }
    let small: std::collections::HashSet<usize> = clusters
        .into_par_iter()
        .filter(|(_, c)| {
            if c.floating {
                return false;
            }
            let nl = c.layers.len() as f64;
            let avg_w = c.narrow.iter().sum::<f64>() / nl;
            if c.narrow.iter().all(|x| *x <= w) && avg_w < w / 2.0 && c.area / nl < 4.0 && c.layers.len() < 3 {
                return true;
            }
            let eroded = poly::offset(&poly::from_loops(&c.merged), -2.5 * w);
            let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
            for p in eroded.iter().flatten().flatten() {
                lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                hi = [hi[0].max(p[0]), hi[1].max(p[1])];
            }
            eroded.is_empty() || (hi[0] - lo[0] < 2.0 && hi[1] - lo[1] < 2.0)
        })
        .map(|(id, _)| id)
        .collect();
    let roots: Vec<usize> = (0..parent.len()).map(|i| find(&mut parent, i)).collect();
    (0..n).map(|l| pieces[l].iter().enumerate().filter(|(k, _)| !small.contains(&roots[cluster_of[l][*k]])).flat_map(|(_, (r, _))| r.iter().cloned()).collect()).collect()
}

/// Tree support areas per layer for one object: (branches, interface). `overhang` is what needs
/// support on each layer, `obstacles` every object's layers on the plate (what the branches
/// must keep clear of).
pub fn support_regions(overhang: &[Region], obstacles: &[Region], stack: &LayerStack, s: &SliceSettings) -> Vec<(Region, Region)> {
    let n = overhang.len();
    let mut out = vec![(Vec::new(), Vec::new()); n];
    let pr = Params::new(s);
    let gap = pr.z_gap + 1;
    let w = s.outer_wall_width;

    // 1. roofs and tips under every overhang, clear of the model on their layers
    let mut wanted = vec![false; n];
    for l in (gap..n).filter(|&l| !overhang[l].is_empty()) {
        for k in 0..=pr.roof_layers.min(l - gap) {
            wanted[l - gap - k] = true;
        }
    }
    let clearance: Vec<Option<(Region, Region)>> = (0..n)
        .into_par_iter()
        .map(|l| wanted[l].then(|| (poly::offset(&obstacles[l], pr.gap_min), poly::offset(&obstacles[l], pr.gap_min + pr.tip_r * 0.5))))
        .collect();
    let near = |l: usize| &clearance[l].as_ref().expect("wanted above").0;
    let planned: Vec<Planned> = (1..n)
        .into_par_iter()
        .map(|l| {
            let mut roofs = Vec::new();
            if overhang[l].is_empty() || l < gap {
                return Planned { roofs };
            }
            let t = l - gap;
            for shape in &overhang[l] {
                let island: Region = vec![shape.clone()];
                let mut layers = Vec::new();
                let mut last = island.clone();
                for k in 0..pr.roof_layers.min(t + 1) {
                    // a roof narrower than two lines is no roof (a sliver on a slope): tips hold it
                    let r = poly::difference(&island, near(t - k));
                    let r = poly::offset(&poly::offset(&r, -w), w);
                    if poly::region_area(&r) < MIN_ROOF_AREA {
                        break;
                    }
                    last = r.clone();
                    layers.push((t - k, r));
                }
                let k = layers.len();
                if k > t {
                    // the roof reaches the plate
                    roofs.push((layers, usize::MAX, Vec::new()));
                    continue;
                }
                let tl = t - k;
                let free = poly::difference(&last, &clearance[tl].as_ref().expect("wanted above").1);
                let tips = sample_tips(&free, pr.spacing, pr.connect);
                roofs.push((layers, tl, tips));
            }
            Planned { roofs }
        })
        .collect();
    let mut roofs: Vec<Roof> = Vec::new();
    let mut tips_at: Vec<Vec<(usize, P)>> = vec![Vec::new(); n];
    for p in planned {
        for (layers, tl, tips) in p.roofs {
            let id = roofs.len();
            let rests = tl == usize::MAX || tips.is_empty();
            if !layers.is_empty() || !tips.is_empty() {
                roofs.push(Roof { layers, tips: Vec::new(), rests });
                if tl != usize::MAX {
                    tips_at[tl].extend(tips.into_iter().map(|p| (id, p)));
                }
            }
        }
    }
    let tip_top = (0..n).rev().find(|&l| !tips_at[l].is_empty());
    let roof_top = roofs.iter().flat_map(|r| r.layers.iter().map(|x| x.0)).max();
    let Some(top) = tip_top.max(roof_top) else {
        return out;
    };

    // 2. where a branch could still reach the plate: the avoidance, grown from the plate up
    let max_move: Vec<f64> = stack.heights.iter().map(|h| pr.tan_max * h).collect();
    let r0 = pr.branch_r;
    let grown: Vec<Region> = (0..=top).into_par_iter().map(|l| poly::offset(&obstacles[l], r0 + pr.gap)).collect();
    let mut avoid: Vec<Region> = Vec::with_capacity(top + 1);
    for l in 0..=top {
        let a = if l == 0 { grown[0].clone() } else { poly::union(&grown[l], &poly::offset(&avoid[l - 1], -max_move[l])) };
        avoid.push(a);
    }
    drop(grown);
    let av: Vec<Edges> = avoid.par_iter().map(Edges::new).collect();
    let model: Vec<Edges> = obstacles[..=top].par_iter().map(Edges::new).collect();

    // a node at q with radius r on layer l is fine where it can still reach the plate, or
    // (resting on the model) where it just clears the model
    let clear = |l: usize, q: P, r: f64, to_plate: bool| if to_plate { av[l].clear(q, (r - r0).max(0.0)) } else { model[l].clear(q, r + pr.gap) };
    let push = |l: usize, q: P, r: f64, to_plate: bool| if to_plate { av[l].push_out(q, (r - r0).max(0.0)) } else { model[l].push_out(q, r + pr.gap) };

    // 3. grow the branches down, layer by layer
    let mut nodes: Vec<Node> = Vec::new();
    let mut at: Vec<Vec<usize>> = vec![Vec::new(); top + 1];
    let mut active: Vec<usize> = Vec::new();
    let kill = |nodes: &mut Vec<Node>, i: usize| {
        let mut stack = vec![i];
        while let Some(k) = stack.pop() {
            if !nodes[k].dead {
                nodes[k].dead = true;
                stack.extend(nodes[k].parents.iter().copied());
            }
        }
    };
    for l in (0..=top).rev() {
        if l < top && !active.is_empty() {
            let (slow, fast) = (pr.tan_slow * stack.heights[l], max_move[l]);
            let pts: Vec<P> = active.iter().map(|&i| nodes[i].p).collect();
            let grid = Grid::new(&pts, pr.spacing.max(1.0));
            let reach = (2.0 * slow * (l + 1) as f64).min(MAX_REACH);
            // move each towards its nearest neighbour, within what the model allows
            let mut next: Vec<Step> = Vec::with_capacity(active.len());
            for (k, &i) in active.iter().enumerate() {
                let nd = &nodes[i];
                let r = pr.radius(nd.dtt);
                let mut q = nd.p;
                if let Some((j, d)) = grid.nearest(&pts, k, reach) {
                    // a branch that thickens may lean that much further (Rahm's extra speed): tips
                    // merge quickly while they grow to a branch
                    let extra = if nd.age < pr.tip_layers { (pr.branch_r - pr.tip_r) / pr.tip_layers } else { pr.grow };
                    let step = (d / 2.0).min(slow + extra);
                    q = [q[0] + (pts[j][0] - q[0]) / d * step, q[1] + (pts[j][1] - q[1]) / d * step];
                }
                let place = |to_plate: bool| -> Option<P> {
                    if clear(l, q, r, to_plate) {
                        return Some(q);
                    }
                    [push(l, q, r, to_plate), push(l, nd.p, r, to_plate)].into_iter().find(|c| dist(*c, nd.p) <= fast + 1e-9 && clear(l, *c, r, to_plate))
                };
                let mut to_plate = nd.to_plate || clear(l, nd.p, r, true);
                let mut got = place(to_plate);
                if got.is_none() && to_plate && pr.rest_on_model {
                    to_plate = false;
                    got = place(false);
                }
                match got {
                    Some(q) => {
                        // thicken while there is room
                        let dtt = if clear(l, q, pr.radius(nd.dtt + 1.0), to_plate) { nd.dtt + 1.0 } else { nd.dtt };
                        next.push(Step { p: q, dtt, age: nd.age + 1.0, to_plate, roof: nd.roof, from: vec![i] });
                    }
                    // it can't go on: it rests on the model here, or goes with its tips
                    None => {
                        if !pr.rest_on_model || !(nd.age * stack.heights[l] >= MIN_HEIGHT_TO_MODEL || nd.roof) {
                            kill(&mut nodes, i);
                        }
                    }
                }
            }
            // merge branches that met
            let pts: Vec<P> = next.iter().map(|x| x.p).collect();
            let merge = 2.0 * fast;
            let grid = Grid::new(&pts, merge.max(0.05));
            let mut parent: Vec<usize> = (0..next.len()).collect();
            fn find(p: &mut [usize], i: usize) -> usize {
                let mut r = i;
                while p[r] != r {
                    r = p[r];
                }
                p[i] = r;
                r
            }
            for i in 0..next.len() {
                for j in grid.within(&pts, i, merge) {
                    let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                    if a != b {
                        parent[a.max(b)] = a.min(b);
                    }
                }
            }
            let mut groups: Vec<Vec<usize>> = vec![Vec::new(); next.len()];
            for i in 0..next.len() {
                let r = find(&mut parent, i);
                groups[r].push(i);
            }
            active.clear();
            for g in groups.into_iter().filter(|g| !g.is_empty()) {
                // at the centre of the merged branches, weighted by their cross-sections
                let (mut sw, mut c) = (0.0, [0.0, 0.0]);
                for &m in &g {
                    let w = pr.radius(next[m].dtt).powi(2);
                    sw += w;
                    c = [c[0] + next[m].p[0] * w, c[1] + next[m].p[1] * w];
                }
                c = [c[0] / sw, c[1] / sw];
                let dtt = g.iter().map(|&m| next[m].dtt).fold(0.0, f64::max);
                let age = g.iter().map(|&m| next[m].age).fold(0.0, f64::max);
                let big = *g.iter().max_by(|&&a, &&b| next[a].dtt.total_cmp(&next[b].dtt).then(b.cmp(&a))).expect("groups are not empty");
                let mut to_plate = g.iter().any(|&m| next[m].to_plate);
                let r = pr.radius(dtt);
                let p = if g.len() == 1 {
                    next[big].p
                } else if clear(l, c, r, to_plate) {
                    c
                } else if to_plate && pr.rest_on_model && clear(l, c, r, false) {
                    to_plate = false;
                    c
                } else {
                    to_plate = next[big].to_plate;
                    next[big].p
                };
                let id = nodes.len();
                let parents: Vec<usize> = g.iter().flat_map(|&m| next[m].from.iter().copied()).collect();
                for &q in &parents {
                    nodes[q].child = Some(id);
                }
                nodes.push(Node { p, dtt, age, to_plate, roof: g.iter().any(|&m| next[m].roof), parents, child: None, dead: false });
                active.push(id);
            }
        }
        // new tips: where a branch already passes, it carries the roof too
        let cs = (pr.spacing * 0.5).max(0.1);
        let mut taken: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for &i in &active {
            taken.entry(Grid::key(nodes[i].p, cs)).or_default().push(i);
        }
        for &(roof, p) in &tips_at[l] {
            let (kx, ky) = Grid::key(p, cs);
            let near = (kx - 1..=kx + 1).flat_map(|x| (ky - 1..=ky + 1).map(move |y| (x, y))).filter_map(|k| taken.get(&k)).flatten().copied().find(|&i| dist(nodes[i].p, p) < cs);
            if let Some(i) = near {
                nodes[i].roof = true;
                roofs[roof].tips.push(i);
                continue;
            }
            // under a roof a tip starts as thick as a branch (as Bambu's do)
            let under_roof = !roofs[roof].layers.is_empty();
            let dtt = if under_roof { pr.tip_layers } else { 0.0 };
            let to_plate = clear(l, p, pr.radius(dtt), true);
            if !to_plate && !pr.rest_on_model {
                continue;
            }
            let id = nodes.len();
            nodes.push(Node { p, dtt, age: 0.0, to_plate, roof: under_roof, parents: Vec::new(), child: None, dead: false });
            roofs[roof].tips.push(id);
            active.push(id);
            taken.entry((kx, ky)).or_default().push(id);
        }
        at[l] = active.clone();
    }

    // 4. draw: circles per node, feet on the plate, roofs, floors where branches rest
    // feet: each trunk on the plate flares out over its lowest layers (eased: steepest at the
    // plate), on a wide disc on the first layer
    let mut foot = vec![0.0f64; nodes.len()];
    for root in at.first().into_iter().flatten().copied().filter(|&i| !nodes[i].dead) {
        let r_root = pr.radius(nodes[root].dtt);
        let need = ((FLARE.min(FOOT_RADIUS - r_root) / pr.flare).ceil().max(0.0)) as usize;
        let mut path = vec![root];
        while path.len() <= need {
            let last = *path.last().expect("not empty");
            let Some(&up) = nodes[last].parents.iter().filter(|&&q| !nodes[q].dead).max_by(|&&a, &&b| nodes[a].dtt.total_cmp(&nodes[b].dtt).then(b.cmp(&a))) else {
                break;
            };
            path.push(up);
        }
        let top = need.min(path.len() - 1);
        if top > 0 {
            let r_trunk = pr.radius(nodes[path[top]].dtt);
            let r_plate = FOOT_RADIUS.min(r_trunk + FLARE).min(r_trunk + top as f64 * pr.flare);
            for (k, &i) in path.iter().enumerate().take(top + 1) {
                let t = k as f64 / top as f64;
                let e = 1.0 - (1.0 - t) * (1.0 - t);
                foot[i] = foot[i].max(r_plate + e * (r_trunk - r_plate));
            }
        }
        foot[root] = FOOT_RADIUS;
    }
    let mut roof_at: Vec<Region> = vec![Vec::new(); top + 1];
    for r in &roofs {
        let alive = r.tips.iter().any(|&t| !nodes[t].dead);
        let rests_ok = r.rests && (pr.rest_on_model || r.layers.iter().any(|x| x.0 == 0));
        if !(alive || rests_ok) {
            continue;
        }
        for (l, reg) in &r.layers {
            roof_at[*l].extend(reg.iter().cloned());
        }
    }
    let floor_layers = pr.roof_layers;
    let drawn: Vec<(Region, Region)> = (0..=top)
        .into_par_iter()
        .map(|l| {
            let mut circles: Vec<Contour> = Vec::new();
            for &i in &at[l] {
                let nd = &nodes[i];
                if nd.dead {
                    continue;
                }
                circles.push(circle(nd.p, pr.radius(nd.dtt).max(foot[i])));
            }
            let roof = poly::from_loops(&roof_at[l].iter().flatten().cloned().collect::<Vec<_>>());
            if circles.is_empty() && roof.is_empty() {
                return (Vec::new(), Vec::new());
            }
            let mut body = poly::difference(&poly::from_loops(&circles), &poly::offset(&obstacles[l], pr.gap));
            // keep the Z gap above the model, and print the layers above that as a floor
            let below = |from: usize, to: usize| -> Region {
                let mut r: Region = Vec::new();
                for k in from..=to {
                    if l >= k {
                        r = poly::union(&r, &obstacles[l - k]);
                    }
                }
                r
            };
            if !body.is_empty() && l > 0 {
                body = poly::difference(&body, &below(1, pr.z_gap));
            }
            let floor = if floor_layers > 0 && !body.is_empty() && l > pr.z_gap { poly::intersection(&body, &poly::offset(&below(pr.z_gap + 1, pr.z_gap + floor_layers), 0.5 * s.outer_wall_width)) } else { Vec::new() };
            let iface = poly::union(&poly::difference(&roof, &poly::offset(&obstacles[l], pr.gap_min)), &floor);
            let body = poly::drop_small(poly::difference(&body, &iface), 0.05);
            (body, poly::drop_small(iface, 0.2))
        })
        .collect();
    for (l, d) in drawn.into_iter().enumerate() {
        out[l] = d;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::{plan, Feature, PlannerObject};
    use crate::slicer::slice_mesh;
    use glam::Vec3;
    use only3d_geom::{shapes, Mesh};

    fn cuboid(size: [f32; 3], at: [f32; 3]) -> Mesh {
        let mut m = shapes::cuboid(Vec3::from(size)).unwrap();
        m.translate(Vec3::from(at) + Vec3::new(100.0, 100.0, 0.0));
        m
    }

    fn union(parts: &[Mesh]) -> Mesh {
        let mut m = parts[0].clone();
        for p in &parts[1..] {
            m.append(p);
        }
        m
    }

    /// The test parts of tools/supports_verify.py.
    fn part(name: &str) -> Mesh {
        match name {
            "t" => union(&[cuboid([4.0, 4.0, 10.0], [0.0, 0.0, 0.0]), cuboid([30.0, 10.0, 2.0], [0.0, 0.0, 10.0])]),
            "arch" => union(&[cuboid([10.0, 10.0, 15.0], [-20.0, 0.0, 0.0]), cuboid([10.0, 10.0, 15.0], [20.0, 0.0, 0.0]), cuboid([50.0, 10.0, 5.0], [0.0, 0.0, 15.0])]),
            "hook" => union(&[cuboid([6.0, 10.0, 30.0], [-12.0, 0.0, 0.0]), cuboid([30.0, 10.0, 4.0], [0.0, 0.0, 26.0]), cuboid([4.0, 10.0, 10.0], [13.0, 0.0, 16.0])]),
            "sphere" => {
                let mut m = shapes::sphere(15.0, None).unwrap();
                m.translate(Vec3::new(100.0, 100.0, 0.0));
                m
            }
            // a J: a post, an arm at the top and a hook curling back under the arm, so branches
            // under the arm stand on the hook (or must go round it)
            "j" => union(&[cuboid([6.0, 10.0, 30.0], [-15.0, 0.0, 0.0]), cuboid([36.0, 10.0, 4.0], [0.0, 0.0, 26.0]), cuboid([4.0, 10.0, 16.0], [16.0, 0.0, 10.0]), cuboid([14.0, 10.0, 3.0], [11.0, 0.0, 10.0])]),
            _ => unreachable!(),
        }
    }

    struct Run {
        regions: Vec<Region>,
        sup: Vec<(Region, Region)>,
        s: SliceSettings,
        stack: LayerStack,
    }

    fn run(name: &str, s: SliceSettings) -> Run {
        let m = part(name);
        let stack = LayerStack::new(m.bounds().max.z as f64, 0.2, 0.2);
        let (regions, _) = slice_mesh(&m, &stack.cut_planes());
        let sup = support_regions(&overhangs(&regions, &stack, &s), &regions, &stack, &s);
        Run { regions, sup, s, stack }
    }

    fn tree() -> SliceSettings {
        SliceSettings { support: true, ..Default::default() }
    }

    fn area(r: &Region) -> f64 {
        poly::region_area(r)
    }

    fn all(r: &(Region, Region)) -> Region {
        poly::union(&r.0, &r.1)
    }

    /// Supports keep the XY gap to the model on every layer, and the Z gap above it.
    fn assert_clear(t: &Run) {
        for (l, sup) in t.sup.iter().enumerate() {
            let sup = all(sup);
            if sup.is_empty() {
                continue;
            }
            let near = poly::intersection(&sup, &poly::offset(&t.regions[l], t.s.support_xy_gap.min(0.21) - 0.02));
            assert!(area(&near) < 1e-3, "layer {l}: {:.4} mm² of support inside the model's XY gap", area(&near));
            if l > 0 {
                let on = poly::intersection(&sup, &poly::offset(&t.regions[l - 1], -0.05));
                assert!(area(&on) < 1e-3, "layer {l}: {:.4} mm² of support right on the model below (no Z gap)", area(&on));
            }
        }
    }

    /// Branch islands (connected areas) on a layer.
    fn islands(r: &Region) -> usize {
        r.len()
    }

    #[test]
    fn t_slab_stands_on_merging_branches() {
        let t = run("t", tree());
        assert_clear(&t);
        let slab = 50; // the slab's first layer (z 10.0..10.2)
        // the Z gap right under the slab, then two layers of roof over the whole overhang
        assert!(all(&t.sup[slab - 1]).is_empty());
        for l in [slab - 2, slab - 3] {
            let roof = area(&t.sup[l].1);
            assert!(roof > 220.0 && roof < 300.0, "layer {l}: roof {roof:.0} mm²");
        }
        // many thin tips under the roof merge into fewer, thicker branches on their way down
        let tips = islands(&t.sup[slab - 4].0);
        let mid = islands(&t.sup[25].0);
        assert!(tips >= 25, "{tips} tips");
        assert!(mid < tips * 2 / 3 && mid >= 4, "{tips} tips, {mid} branches half way down");
        // hollow-able branches, far less than the grid's solid columns
        let tree_area: f64 = t.sup[1..slab - 4].iter().map(|s| area(&s.0)).sum();
        let gs = SliceSettings { support_type: crate::SupportType::Normal, ..tree() };
        let grid = crate::planner::support_regions(&t.regions, &t.regions, &t.stack, &gs);
        let grid_area: f64 = grid[1..slab - 4].iter().map(|s| area(&s.0)).sum();
        assert!(tree_area < grid_area * 0.5, "tree {tree_area:.0} mm² vs grid {grid_area:.0} mm²");
        // and every branch stands on the plate, on a wide foot
        assert!(area(&t.sup[0].0) > area(&t.sup[5].0) * 1.5);
    }

    #[test]
    fn arch_bridge_is_held_from_the_plate() {
        let t = run("arch", tree());
        assert_clear(&t);
        let deck = 75; // z 15.0
        let roof = area(&t.sup[deck - 2].1);
        assert!(roof > 250.0 && roof < 320.0, "roof {roof:.0} mm² under the 30 x 10 mm span");
        // between the pillars all the way down
        for l in [0, 10, 40, 70] {
            assert!(!t.sup[l].0.is_empty(), "layer {l}");
        }
    }

    #[test]
    fn hook_branches_avoid_the_lip() {
        let t = run("hook", tree());
        assert_clear(&t);
        // the arm's underside (z 26) and the lip's (z 16) both get a roof
        assert!(area(&t.sup[128].1) > 100.0, "{}", area(&t.sup[128].1));
        assert!(area(&t.sup[78].1) > 20.0, "{}", area(&t.sup[78].1));
    }

    #[test]
    fn branches_rest_on_the_model_or_go_round_it() {
        // J: tips under the arm, above the hook's tip. By default branches stand on the hook (on
        // a floor of interface); on the plate only, they must not
        let t = run("j", tree());
        assert_clear(&t);
        let hook_top = 65; // z 13.0: the hook's top surface
        let above: f64 = (hook_top + 1..hook_top + 6).map(|l| area(&poly::intersection(&all(&t.sup[l]), &t.regions[hook_top - 1]))).sum();
        assert!(above > 5.0, "branches stand on the hook: {above:.1} mm²");
        let floor: f64 = (hook_top + 1..hook_top + 5).map(|l| area(&poly::intersection(&t.sup[l].1, &t.regions[hook_top - 1]))).sum();
        assert!(floor > 1.0, "a floor of interface on the hook: {floor:.1} mm²");

        let p = run("j", SliceSettings { support_on_plate_only: true, ..tree() });
        assert_clear(&p);
        // nothing stands on the model: every branch reaches the plate (it may lean over the
        // model higher up, on its way round)
        for l in 2..p.sup.len() {
            let on = poly::intersection(&all(&p.sup[l]), &poly::offset(&p.regions[l - 2], -0.05));
            assert!(area(&on) < 1e-3, "layer {l}: {:.3} mm² of support standing on the model", area(&on));
        }
        // but the arm is still held where branches can reach the plate past the hook
        assert!(area(&p.sup[128].1) > 30.0, "{}", area(&p.sup[128].1));
    }

    #[test]
    fn sphere_bottom_gets_a_ring_of_branches() {
        let t = run("sphere", tree());
        assert_clear(&t);
        // overhangs flatter than 30° end about 2 mm up: supports stop there
        let top = t.sup.iter().rposition(|s| !all(s).is_empty()).unwrap();
        assert!((6..=11).contains(&top), "support up to layer {top}");
        assert!(area(&t.sup[0].0) > 50.0, "a foothold on the plate around the contact point");
    }

    #[test]
    fn branches_keep_clear_of_other_objects() {
        // a block (another object) under one wing of the T: the T's branches go round it or
        // stand on it, never through it
        let t = part("t");
        let block = cuboid([6.0, 12.0, 6.0], [10.0, 0.0, 0.0]);
        let stack = LayerStack::new(12.0, 0.2, 0.2);
        let (rt, _) = slice_mesh(&t, &stack.cut_planes());
        let (rb, _) = slice_mesh(&block, &stack.cut_planes());
        let s = tree();
        let objs = [PlannerObject { regions: &rt, settings: &s, cover: None, support: true }, PlannerObject { regions: &rb, settings: &s, cover: None, support: true }];
        let layers = plan(&objs, &stack, &s);
        let mut supported = 0;
        for (l, layer) in layers.iter().enumerate() {
            let keep_out = poly::offset(&rb[l], s.support_xy_gap - s.outer_wall_width / 2.0 - 0.02);
            for p in layer.paths.iter().filter(|p| p.object == 0 && matches!(p.feature, Feature::Support | Feature::SupportInterface)) {
                supported += 1;
                assert!(p.pts.iter().all(|q| !Edges::new(&keep_out).inside(*q)), "layer {l}: support of the T inside the block");
            }
        }
        assert!(supported > 100);
    }

    #[test]
    fn planner_prints_branches_as_loops() {
        let t = run("t", tree());
        let layers = plan(&[PlannerObject { regions: &t.regions, settings: &t.s, cover: None, support: true }], &t.stack, &t.s);
        let sup = |l: usize| layers[l].paths.iter().filter(|p| p.feature == Feature::Support).collect::<Vec<_>>();
        assert!(sup(25).iter().all(|p| p.closed), "branches are loops");
        assert!(sup(0).iter().any(|p| !p.closed), "the first layer is filled");
        assert!(layers[48].paths.iter().any(|p| p.feature == Feature::SupportInterface));
    }
}
