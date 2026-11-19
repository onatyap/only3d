//! Mesh booleans on top of `boolmesh` (a pure-Rust implementation of Manifold's robust
//! algorithm, the same family of kernel OpenSCAD now uses).

use std::sync::Arc;
use std::time::Instant;

use boolmesh::prelude::{compute_boolean, Manifold, OpType};
use glam::Vec3;
use only3d_geom::Mesh;

use crate::shape::EvalStats;
use crate::ModelError;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BoolOp {
    Union,
    Subtract,
    Intersect,
}

fn to_manifold(m: &Mesh, which: &str) -> Result<Manifold, ModelError> {
    let pos: Vec<f64> = m.positions.iter().flat_map(|p| [p.x as f64, p.y as f64, p.z as f64]).collect();
    let idx: Vec<usize> = m.indices.iter().flat_map(|t| [t[0] as usize, t[1] as usize, t[2] as usize]).collect();
    Manifold::new(&pos, &idx).map_err(|e| {
        let r = m.analyze();
        ModelError::Csg {
            message: format!("{which} operand is not a closed manifold ({e}); boundary edges: {}, non-manifold edges: {}", r.boundary_edges, r.non_manifold_edges),
            hint: "booleans need watertight solids: repair the imported mesh or rebuild it from primitives".into(),
        }
    })
}

fn from_manifold(m: &Manifold) -> Mesh {
    let positions: Vec<Vec3> = m.ps.iter().map(|p| Vec3::new(p.x as f32, p.y as f32, p.z as f32)).collect();
    let tails: Vec<u32> = m.hs.iter().map(|h| h.tail as u32).collect();
    let indices = tails.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
    let mut out = Mesh::from_parts(positions, indices);
    // Only drop triangles that repeat a vertex. Zero-area slivers with three distinct vertices
    // are part of the kernel's watertight topology: removing them opens holes.
    out.indices.retain(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2]);
    out.compact();
    out
}

pub fn boolean(a: &Mesh, b: &Mesh, op: BoolOp, stats: &mut EvalStats) -> Result<Mesh, ModelError> {
    // Fast paths that avoid the kernel entirely.
    let (ba, bb) = (a.bounds(), b.bounds());
    if !ba.overlaps(&bb) {
        return Ok(match op {
            BoolOp::Union => {
                let mut m = a.clone();
                m.append(b);
                m
            }
            BoolOp::Subtract => a.clone(),
            BoolOp::Intersect => Mesh::new(),
        });
    }
    if a.is_empty() || b.is_empty() {
        return Ok(match op {
            BoolOp::Union => if a.is_empty() { b.clone() } else { a.clone() },
            BoolOp::Subtract => a.clone(),
            BoolOp::Intersect => Mesh::new(),
        });
    }
    let t = Instant::now();
    let (ma, mb) = (to_manifold(a, "first")?, to_manifold(b, "second")?);
    let r = compute_boolean(
        &ma,
        &mb,
        match op {
            BoolOp::Union => OpType::Add,
            BoolOp::Subtract => OpType::Subtract,
            BoolOp::Intersect => OpType::Intersect,
        },
    );
    stats.booleans += 1;
    stats.boolean_ms += t.elapsed().as_secs_f64() * 1000.0;
    match r {
        Ok(r) => Ok(from_manifold(&r)),
        // the kernel reports an empty result (solids that only touch, or a cutter that removes
        // everything) as an error
        Err(e) if e.contains("empty pos matrix") => Ok(Mesh::new()),
        Err(e) => Err(ModelError::Csg { message: e, hint: "try nudging coincident faces apart by 0.01 mm".into() }),
    }
}

/// Union many meshes: disjoint groups are just appended; overlapping groups are merged with a
/// balanced reduction (fewer, smaller boolean calls).
pub fn union_all(meshes: Vec<Arc<Mesh>>, stats: &mut EvalStats) -> Result<Mesh, ModelError> {
    let n = meshes.len();
    if n == 0 {
        return Ok(Mesh::new());
    }
    if n == 1 {
        return Ok((*meshes[0]).clone());
    }
    let boxes: Vec<_> = meshes.iter().map(|m| m.bounds()).collect();
    // cluster by bbox overlap (union-find)
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    for i in 0..n {
        for j in i + 1..n {
            if boxes[i].overlaps(&boxes[j]) {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                if a != b {
                    parent[a] = b;
                }
            }
        }
    }
    let mut groups: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for i in 0..n {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let mut out = Mesh::new();
    for (_, g) in groups {
        let mut level: Vec<Mesh> = g.iter().map(|&i| (*meshes[i]).clone()).collect();
        while level.len() > 1 {
            let mut next = Vec::with_capacity(level.len().div_ceil(2));
            let mut it = level.into_iter();
            while let Some(a) = it.next() {
                match it.next() {
                    Some(b) => next.push(boolean(&a, &b, BoolOp::Union, stats)?),
                    None => next.push(a),
                }
            }
            level = next;
        }
        out.append(&level.pop().unwrap_or_default());
    }
    Ok(out)
}
