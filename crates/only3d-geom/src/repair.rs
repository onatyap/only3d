//! Repair downloaded meshes so they slice and combine: weld near-duplicate vertices, drop
//! degenerate and duplicate faces, turn every face of a piece the same way (outward), and close
//! small holes with fans. What can't be fixed safely (edges shared by three or more faces) is
//! reported, not guessed at.

use std::collections::{HashMap, HashSet, VecDeque};

use glam::Vec3;
use serde::Serialize;

use crate::Mesh;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct RepairReport {
    pub vertices_merged: usize,
    pub faces_removed: usize,
    pub faces_flipped: usize,
    pub holes_filled: usize,
    /// Holes left open (too large or not a clean loop).
    pub holes_left: usize,
    pub watertight: bool,
}

impl RepairReport {
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.vertices_merged > 0 {
            parts.push(format!("merged {} duplicate vertices", self.vertices_merged));
        }
        if self.faces_removed > 0 {
            parts.push(format!("removed {} degenerate or duplicate faces", self.faces_removed));
        }
        if self.faces_flipped > 0 {
            parts.push(format!("turned {} faces outward", self.faces_flipped));
        }
        if self.holes_filled > 0 {
            parts.push(format!("closed {} hole(s)", self.holes_filled));
        }
        if self.holes_left > 0 {
            parts.push(format!("{} hole(s) too big to close safely", self.holes_left));
        }
        let what = if parts.is_empty() { "nothing to fix".to_string() } else { parts.join(", ") };
        format!("{what}; {}", if self.watertight { "now watertight" } else { "still not watertight" })
    }
}

/// Repair in place. Holes with more than `max_hole_edges` edges are left open.
pub fn repair(m: &mut Mesh, max_hole_edges: usize) -> RepairReport {
    let mut rep = RepairReport::default();
    // 1. weld and drop degenerate / duplicate faces
    let before_v = m.positions.len();
    m.weld(1e-4);
    let before_f = m.indices.len();
    m.remove_degenerate();
    let mut seen = HashSet::new();
    m.indices.retain(|t| {
        let mut k = *t;
        k.sort_unstable();
        seen.insert(k)
    });
    m.compact();
    rep.vertices_merged = before_v.saturating_sub(m.positions.len());
    rep.faces_removed = before_f - m.indices.len();

    // 2. orient: walk each piece face by face; a shared edge must run opposite ways
    let mut by_edge: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for (fi, t) in m.indices.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            by_edge.entry((a.min(b), a.max(b))).or_default().push(fi);
        }
    }
    let has_dir = |t: &[u32; 3], a: u32, b: u32| (0..3).any(|k| t[k] == a && t[(k + 1) % 3] == b);
    let n = m.indices.len();
    let mut done = vec![false; n];
    let mut pieces: Vec<Vec<usize>> = Vec::new();
    for start in 0..n {
        if done[start] {
            continue;
        }
        done[start] = true;
        let mut piece = vec![start];
        let mut q = VecDeque::from([start]);
        while let Some(f) = q.pop_front() {
            let t = m.indices[f];
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                let Some(nb) = by_edge.get(&(a.min(b), a.max(b))) else { continue };
                if nb.len() != 2 {
                    continue; // boundary or non-manifold: don't propagate across it
                }
                for &g in nb {
                    if g == f || done[g] {
                        continue;
                    }
                    // g must traverse a→b the other way (b→a)
                    if has_dir(&m.indices[g], a, b) {
                        m.indices[g].swap(1, 2);
                        rep.faces_flipped += 1;
                    }
                    done[g] = true;
                    piece.push(g);
                    q.push_back(g);
                }
            }
        }
        pieces.push(piece);
    }
    // each closed-ish piece points outward (positive volume)
    for piece in &pieces {
        let vol: f64 = piece
            .iter()
            .map(|&f| {
                let [a, b, c] = m.indices[f].map(|i| m.positions[i as usize].as_dvec3());
                a.dot(b.cross(c)) / 6.0
            })
            .sum();
        if vol < 0.0 {
            for &f in piece {
                m.indices[f].swap(1, 2);
            }
            rep.faces_flipped += piece.len();
        }
    }
    rep.faces_flipped = rep.faces_flipped.min(n);

    // 3. close holes: boundary edges chained into loops, each filled with a fan to its centre
    let mut boundary: HashMap<u32, Vec<u32>> = HashMap::new(); // from → to (the hole runs the other way)
    let mut counts: HashMap<(u32, u32), usize> = HashMap::new();
    for t in &m.indices {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *counts.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    for t in &m.indices {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            if counts[&(a.min(b), a.max(b))] == 1 {
                // the face uses a→b, so the hole's rim, seen from outside, runs b→a
                boundary.entry(b).or_default().push(a);
            }
        }
    }
    let mut used: HashSet<(u32, u32)> = HashSet::new();
    let starts: Vec<u32> = boundary.keys().copied().collect();
    for s0 in starts {
        let Some(nexts) = boundary.get(&s0).cloned() else { continue };
        for first in nexts {
            if used.contains(&(s0, first)) {
                continue;
            }
            // follow the rim until it comes back to s0
            let mut lp = vec![s0];
            let mut cur = first;
            let mut prev = s0;
            used.insert((s0, first));
            let mut ok = true;
            while cur != s0 {
                lp.push(cur);
                if lp.len() > max_hole_edges.max(3) * 4 {
                    ok = false;
                    break;
                }
                let Some(cands) = boundary.get(&cur) else {
                    ok = false;
                    break;
                };
                let next = cands.iter().copied().find(|c| !used.contains(&(cur, *c)));
                let Some(nx) = next else {
                    ok = false;
                    break;
                };
                used.insert((cur, nx));
                prev = cur;
                cur = nx;
            }
            let _ = prev;
            if !ok || lp.len() < 3 {
                rep.holes_left += 1;
                continue;
            }
            if lp.len() > max_hole_edges {
                rep.holes_left += 1;
                continue;
            }
            let c = lp.iter().map(|&i| m.positions[i as usize]).sum::<Vec3>() / lp.len() as f32;
            let ci = m.positions.len() as u32;
            m.positions.push(c);
            for k in 0..lp.len() {
                let (a, b) = (lp[k], lp[(k + 1) % lp.len()]);
                m.indices.push([a, b, ci]);
            }
            rep.holes_filled += 1;
        }
    }
    rep.watertight = m.analyze().watertight;
    rep
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shapes;

    #[test]
    fn closes_holes_and_turns_faces_outward() {
        let mut cube = shapes::cuboid(Vec3::new(10.0, 10.0, 10.0)).unwrap();
        let good = cube.analyze().volume_mm3;
        // break it: drop the two triangles of one face, flip one more, add a duplicate
        let top: Vec<usize> = (0..cube.indices.len()).filter(|&i| cube.triangle(i).iter().all(|p| p.z > 9.99)).collect();
        assert_eq!(top.len(), 2);
        for &i in top.iter().rev() {
            cube.indices.remove(i);
        }
        cube.indices[0].swap(1, 2);
        let dup = cube.indices[3];
        cube.indices.push(dup);
        let r = cube.analyze();
        assert!(!r.watertight);
        let rep = repair(&mut cube, 200);
        assert!(rep.watertight, "{rep:?}");
        assert_eq!(rep.holes_filled, 1);
        assert_eq!(rep.faces_removed, 1);
        assert!(rep.faces_flipped >= 1);
        let after = cube.analyze();
        assert!(after.watertight && !after.inverted);
        assert!((after.volume_mm3 - good).abs() < 1.0, "{} vs {good}", after.volume_mm3);
        assert!(rep.summary().contains("now watertight"));
        // an inside-out mesh comes back right side out
        let mut inv = shapes::cuboid(Vec3::new(5.0, 5.0, 5.0)).unwrap();
        inv.flip();
        repair(&mut inv, 200);
        assert!(!inv.analyze().inverted && inv.volume() > 0.0);
    }
}
