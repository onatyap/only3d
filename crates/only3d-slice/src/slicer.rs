//! Mesh → per-layer polygon regions. Topological segment chaining (by mesh edge identity) makes
//! this exact on watertight meshes and tolerant of vertices lying on the cutting plane.

use std::collections::HashMap;

use only3d_geom::Mesh;
use rayon::prelude::*;

use crate::poly::{self, dist, Contour, Region, P};

/// Layer stack shared by every object on a plate.
#[derive(Clone, Debug, serde::Serialize)]
pub struct LayerStack {
    /// Top Z of each layer.
    pub tops: Vec<f64>,
    /// Thickness of each layer.
    pub heights: Vec<f64>,
}

impl LayerStack {
    pub fn new(max_z: f64, first: f64, h: f64) -> Self {
        let mut tops = Vec::new();
        let mut heights = Vec::new();
        if max_z <= 0.0 {
            return LayerStack { tops, heights };
        }
        let mut z = first;
        tops.push(z);
        heights.push(first);
        while z + 1e-6 < max_z {
            // last layer may be slightly thinner to land exactly on the top
            let step = if max_z - z < h * 0.5 { (max_z - z).max(h * 0.5) } else { h };
            z += step;
            tops.push((z * 1e4).round() / 1e4);
            heights.push(step);
        }
        LayerStack { tops, heights }
    }

    pub fn len(&self) -> usize {
        self.tops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tops.is_empty()
    }

    /// Z at which each layer is cut (the middle of the layer).
    pub fn cut_planes(&self) -> Vec<f64> {
        self.tops.iter().zip(&self.heights).map(|(t, h)| t - h / 2.0).collect()
    }
}

/// Slice a mesh at the given planes. Returns one region per plane plus the number of open
/// (unclosable) chains encountered, which signals a broken mesh.
pub fn slice_mesh(mesh: &Mesh, planes: &[f64]) -> (Vec<Region>, usize) {
    let pos: Vec<[f64; 3]> = mesh.positions.iter().map(|p| [p.x as f64, p.y as f64, p.z as f64]).collect();
    // bucket triangles by the planes they cross: zmin < z <= zmax
    let mut buckets: Vec<Vec<u32>> = vec![Vec::new(); planes.len()];
    for (ti, t) in mesh.indices.iter().enumerate() {
        let zs = [pos[t[0] as usize][2], pos[t[1] as usize][2], pos[t[2] as usize][2]];
        let zmin = zs[0].min(zs[1]).min(zs[2]);
        let zmax = zs[0].max(zs[1]).max(zs[2]);
        let start = planes.partition_point(|&z| z <= zmin);
        let end = planes.partition_point(|&z| z <= zmax);
        for b in &mut buckets[start..end] {
            b.push(ti as u32);
        }
    }
    let results: Vec<(Region, usize)> = buckets
        .par_iter()
        .zip(planes.par_iter())
        .map(|(tris, &z)| {
            let loops = slice_layer(&pos, &mesh.indices, tris, z);
            let open = loops.1;
            let region = poly::from_loops(&loops.0.into_iter().map(|c| simplify_closed(&c, 0.01)).collect::<Vec<_>>());
            (region, open)
        })
        .collect();
    let open = results.iter().map(|r| r.1).sum();
    (results.into_iter().map(|r| r.0).collect(), open)
}

type Key = (u32, u32);

fn edge_point(pos: &[[f64; 3]], a: u32, b: u32, z: f64) -> P {
    let (a, b) = if a < b { (a, b) } else { (b, a) };
    let (pa, pb) = (pos[a as usize], pos[b as usize]);
    let t = (z - pa[2]) / (pb[2] - pa[2]);
    [pa[0] + (pb[0] - pa[0]) * t, pa[1] + (pb[1] - pa[1]) * t]
}

fn key(a: u32, b: u32) -> Key {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

fn slice_layer(pos: &[[f64; 3]], idx: &[[u32; 3]], tris: &[u32], z: f64) -> (Vec<Contour>, usize) {
    struct Seg {
        from: Key,
        to: Key,
        a: P,
    }
    let mut segs: Vec<Seg> = Vec::with_capacity(tris.len());
    for &ti in tris {
        let t = idx[ti as usize];
        let above = [pos[t[0] as usize][2] >= z, pos[t[1] as usize][2] >= z, pos[t[2] as usize][2] >= z];
        let n_above = above.iter().filter(|&&a| a).count();
        if n_above == 0 || n_above == 3 {
            continue;
        }
        // the lone vertex is the one on its own side
        let lone = if n_above == 1 { above.iter().position(|&a| a).unwrap() } else { above.iter().position(|&a| !a).unwrap() };
        let l = t[lone];
        let nx = t[(lone + 1) % 3];
        let pv = t[(lone + 2) % 3];
        let (from, to) = if above[lone] { ((l, nx), (pv, l)) } else { ((pv, l), (l, nx)) };
        segs.push(Seg { from: key(from.0, from.1), to: key(to.0, to.1), a: edge_point(pos, from.0, from.1, z) });
    }
    // Non-manifold edges (bodies touching along an edge, common in OpenSCAD output) start
    // several segments at the same key, so keep every candidate.
    let mut start: HashMap<Key, Vec<usize>> = HashMap::with_capacity(segs.len());
    for (i, s) in segs.iter().enumerate() {
        start.entry(s.from).or_default().push(i);
    }
    let mut used = vec![false; segs.len()];
    let mut loops = Vec::new();
    let mut open_chains: Vec<Contour> = Vec::new();
    for s0 in 0..segs.len() {
        if used[s0] {
            continue;
        }
        let mut chain = Vec::new();
        let mut cur = s0;
        let closed = loop {
            used[cur] = true;
            chain.push(segs[cur].a);
            let cands = start.get(&segs[cur].to).map(|v| v.as_slice()).unwrap_or(&[]);
            let next = if cands.contains(&s0) && (chain.len() >= 3 || cands.iter().all(|&c| used[c])) {
                None
            } else {
                // prefer the candidate that turns least sharply away (keeps touching bodies apart)
                cands.iter().copied().filter(|&c| !used[c]).min_by(|&a, &b| dist(segs[a].a, segs[cur].a).total_cmp(&dist(segs[b].a, segs[cur].a)))
            };
            match next {
                None if cands.contains(&s0) => break true,
                Some(nxt) => cur = nxt,
                _ => {
                    // keep the end point of an open chain
                    let s = &segs[cur];
                    chain.push(edge_point(pos, s.to.0, s.to.1, z));
                    break false;
                }
            }
        };
        if closed {
            if chain.len() >= 3 {
                loops.push(chain);
            }
        } else {
            open_chains.push(chain);
        }
    }
    let n_open = open_chains.len();
    loops.extend(close_open_chains(open_chains, 1.0));
    (loops, n_open)
}

/// Join open polylines end-to-start when the gap is small; close a chain when its ends meet.
fn close_open_chains(mut chains: Vec<Contour>, tol: f64) -> Vec<Contour> {
    let mut out = Vec::new();
    while let Some(mut c) = chains.pop() {
        loop {
            let end = *c.last().unwrap();
            if c.len() >= 3 && dist(end, c[0]) <= tol {
                out.push(c);
                break;
            }
            let best = chains.iter().enumerate().map(|(i, o)| (i, dist(end, o[0]))).min_by(|a, b| a.1.total_cmp(&b.1));
            match best {
                Some((i, d)) if d <= tol => {
                    let o = chains.swap_remove(i);
                    c.extend(o);
                }
                _ => break, // give up on this chain
            }
        }
    }
    out
}

/// Remove points closer than `tol` to the line through their neighbours (closed contour).
pub fn simplify_closed(c: &Contour, tol: f64) -> Contour {
    if c.len() <= 3 {
        return c.clone();
    }
    let mut out: Vec<P> = Vec::with_capacity(c.len());
    for &p in c {
        if out.last().is_none_or(|q| dist(*q, p) > 1e-6) {
            out.push(p);
        }
    }
    let mut changed = true;
    while changed && out.len() > 3 {
        changed = false;
        let mut i = 0;
        while i < out.len() && out.len() > 3 {
            let n = out.len();
            let (a, p, b) = (out[(i + n - 1) % n], out[i], out[(i + 1) % n]);
            let ab = [b[0] - a[0], b[1] - a[1]];
            let len = (ab[0] * ab[0] + ab[1] * ab[1]).sqrt();
            let d = if len < 1e-12 { dist(a, p) } else { ((p[0] - a[0]) * ab[1] - (p[1] - a[1]) * ab[0]).abs() / len };
            if d < tol {
                out.remove(i);
                changed = true;
            } else {
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;
    use only3d_geom::shapes;

    #[test]
    fn slices_a_box_with_a_hole() {
        let b = shapes::cuboid(Vec3::new(20.0, 20.0, 10.0)).unwrap();
        let stack = LayerStack::new(10.0, 0.2, 0.2);
        assert_eq!(stack.len(), 50);
        let (regions, open) = slice_mesh(&b, &stack.cut_planes());
        assert_eq!(open, 0);
        for r in &regions {
            assert!((poly::region_area(r) - 400.0).abs() < 1e-3);
        }
        // plane exactly through vertices (z = 0 / 10 boundaries) must not break
        let (r2, open2) = slice_mesh(&b, &[0.0, 10.0, 5.0]);
        assert_eq!(open2, 0);
        assert!((poly::region_area(&r2[2]) - 400.0).abs() < 1e-3);
    }

    #[test]
    fn slices_a_cylinder_tube() {
        let mut outer = shapes::cylinder(10.0, 10.0, 5.0, Some(64), Default::default(), Default::default()).unwrap();
        let mut inner = shapes::cylinder(5.0, 5.0, 5.0, Some(64), Default::default(), Default::default()).unwrap();
        inner.flip(); // inverted inner surface == hole
        outer.append(&inner);
        outer.weld(1e-5);
        let (regions, _) = slice_mesh(&outer, &[2.5]);
        let a = poly::region_area(&regions[0]);
        let expect = std::f64::consts::PI * (100.0 - 25.0);
        assert!((a - expect).abs() / expect < 0.01, "{a} vs {expect}");
        assert_eq!(regions[0].len(), 1);
        assert_eq!(regions[0][0].len(), 2);
    }
}
