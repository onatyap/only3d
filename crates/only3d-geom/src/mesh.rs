use std::collections::HashMap;

use glam::{DVec3, Mat4, Vec3};
use serde::{Deserialize, Serialize};

use crate::Aabb;

/// Indexed triangle mesh. Units: millimetres, Z up, counter-clockwise = outward.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mesh {
    pub positions: Vec<Vec3>,
    pub indices: Vec<[u32; 3]>,
}

/// Health report of a mesh: what a printer (and an agent) needs to know.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MeshReport {
    pub triangles: usize,
    pub vertices: usize,
    pub bounds: Aabb,
    pub size: Vec3,
    pub volume_mm3: f64,
    pub area_mm2: f64,
    /// Every edge is shared by exactly two consistently oriented triangles.
    pub watertight: bool,
    pub boundary_edges: usize,
    pub non_manifold_edges: usize,
    pub flipped_edges: usize,
    pub degenerate_triangles: usize,
    pub components: usize,
    /// Signed volume is negative: normals point inwards.
    pub inverted: bool,
}

impl Mesh {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_parts(positions: Vec<Vec3>, indices: Vec<[u32; 3]>) -> Self {
        Self { positions, indices }
    }

    /// Build from a triangle soup and weld identical vertices.
    pub fn from_soup(tris: &[[Vec3; 3]]) -> Self {
        let mut m = Mesh {
            positions: tris.iter().flat_map(|t| t.iter().copied()).collect(),
            indices: (0..tris.len() as u32).map(|i| [3 * i, 3 * i + 1, 3 * i + 2]).collect(),
        };
        m.weld(1e-5);
        m
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len()
    }

    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    #[inline]
    pub fn triangle(&self, i: usize) -> [Vec3; 3] {
        let [a, b, c] = self.indices[i];
        [self.positions[a as usize], self.positions[b as usize], self.positions[c as usize]]
    }

    pub fn triangles(&self) -> impl Iterator<Item = [Vec3; 3]> + '_ {
        (0..self.indices.len()).map(|i| self.triangle(i))
    }

    pub fn bounds(&self) -> Aabb {
        // Only count referenced vertices would be more exact; meshes here are kept compact.
        Aabb::from_points(self.positions.iter())
    }

    /// Merge vertices closer than `eps` (grid hashing) and drop unused ones.
    pub fn weld(&mut self, eps: f32) {
        let inv = 1.0 / eps as f64;
        let mut map: HashMap<(i64, i64, i64), u32> = HashMap::with_capacity(self.positions.len());
        let mut remap = Vec::with_capacity(self.positions.len());
        let mut out = Vec::with_capacity(self.positions.len() / 2);
        for p in &self.positions {
            let key = (
                (p.x as f64 * inv).round() as i64,
                (p.y as f64 * inv).round() as i64,
                (p.z as f64 * inv).round() as i64,
            );
            let idx = *map.entry(key).or_insert_with(|| {
                out.push(*p);
                (out.len() - 1) as u32
            });
            remap.push(idx);
        }
        for t in &mut self.indices {
            for v in t.iter_mut() {
                *v = remap[*v as usize];
            }
        }
        self.positions = out;
        self.compact();
    }

    /// Remove triangles that repeat a vertex or have (near) zero area. Returns how many were removed.
    pub fn remove_degenerate(&mut self) -> usize {
        let before = self.indices.len();
        let pos = &self.positions;
        self.indices.retain(|t| {
            if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] {
                return false;
            }
            let (a, b, c) = (pos[t[0] as usize], pos[t[1] as usize], pos[t[2] as usize]);
            (b - a).cross(c - a).length_squared() > 1e-14
        });
        before - self.indices.len()
    }

    /// Drop unreferenced vertices.
    pub fn compact(&mut self) {
        let mut used = vec![u32::MAX; self.positions.len()];
        let mut out = Vec::with_capacity(self.positions.len());
        for t in &mut self.indices {
            for v in t.iter_mut() {
                let u = &mut used[*v as usize];
                if *u == u32::MAX {
                    *u = out.len() as u32;
                    out.push(self.positions[*v as usize]);
                }
                *v = *u;
            }
        }
        self.positions = out;
    }

    /// Weld, drop degenerate faces and orient outward. Cheap; call after generating geometry.
    pub fn clean(&mut self) {
        self.weld(1e-5);
        self.remove_degenerate();
        self.compact();
        if self.volume() < 0.0 {
            self.flip();
        }
    }

    pub fn transform(&mut self, m: &Mat4) {
        for p in &mut self.positions {
            *p = m.transform_point3(*p);
        }
        if m.determinant() < 0.0 {
            self.flip();
        }
    }

    pub fn transformed(&self, m: &Mat4) -> Mesh {
        let mut c = self.clone();
        c.transform(m);
        c
    }

    pub fn translate(&mut self, v: Vec3) {
        for p in &mut self.positions {
            *p += v;
        }
    }

    pub fn append(&mut self, other: &Mesh) {
        let off = self.positions.len() as u32;
        self.positions.extend_from_slice(&other.positions);
        self.indices.extend(other.indices.iter().map(|t| [t[0] + off, t[1] + off, t[2] + off]));
    }

    pub fn flip(&mut self) {
        for t in &mut self.indices {
            t.swap(1, 2);
        }
    }

    /// Signed volume (mm³) via the divergence theorem. Accumulated in f64.
    pub fn volume(&self) -> f64 {
        let mut v = 0.0f64;
        for [a, b, c] in self.triangles() {
            let (a, b, c) = (a.as_dvec3(), b.as_dvec3(), c.as_dvec3());
            v += a.dot(b.cross(c));
        }
        v / 6.0
    }

    pub fn area(&self) -> f64 {
        self.triangles().map(|[a, b, c]| 0.5 * (b - a).as_dvec3().cross((c - a).as_dvec3()).length()).sum()
    }

    /// Unit face normal (zero for degenerate triangles).
    pub fn face_normal(&self, i: usize) -> Vec3 {
        let [a, b, c] = self.triangle(i);
        (b - a).cross(c - a).normalize_or_zero()
    }

    /// Area-weighted vertex normals (for smooth display).
    pub fn vertex_normals(&self) -> Vec<Vec3> {
        let mut n = vec![Vec3::ZERO; self.positions.len()];
        for t in &self.indices {
            let [a, b, c] = [self.positions[t[0] as usize], self.positions[t[1] as usize], self.positions[t[2] as usize]];
            let fnorm = (b - a).cross(c - a);
            for &v in t {
                n[v as usize] += fnorm;
            }
        }
        n.iter_mut().for_each(|v| *v = v.normalize_or_zero());
        n
    }

    /// Area (mm²) of downward-facing surface steeper than `max_angle_deg` from vertical that is
    /// not resting on the lowest Z (i.e. needs support).
    pub fn overhang_area(&self, max_angle_deg: f32) -> f64 {
        let zmin = self.bounds().min.z;
        let limit = -(max_angle_deg.to_radians()).sin();
        let mut area = 0.0;
        for i in 0..self.indices.len() {
            let [a, b, c] = self.triangle(i);
            let cr = (b - a).cross(c - a);
            let len = cr.length();
            if len <= 0.0 {
                continue;
            }
            let nz = cr.z / len;
            // nz < -sin(angle): facing down more steeply than allowed (angle measured from vertical wall)
            if nz < limit && a.z.min(b.z).min(c.z) > zmin + 0.05 {
                area += 0.5 * len as f64;
            }
        }
        area
    }

    /// Area (mm²) of faces lying on the lowest Z plane: the first-layer contact patch.
    pub fn bed_contact_area(&self) -> f64 {
        let zmin = self.bounds().min.z;
        let mut area = 0.0;
        for [a, b, c] in self.triangles() {
            if (a.z - zmin).abs() < 0.01 && (b.z - zmin).abs() < 0.01 && (c.z - zmin).abs() < 0.01 {
                area += 0.5 * (b - a).cross(c - a).length() as f64;
            }
        }
        area
    }

    pub fn analyze(&self) -> MeshReport {
        use rayon::prelude::*;
        // Edge statistics via a parallel sort of packed (min,max) vertex keys: much faster and
        // leaner than a hash map on multi-million-triangle meshes.
        let mut edges: Vec<(u64, i8)> = Vec::with_capacity(self.indices.len() * 3);
        for t in &self.indices {
            for k in 0..3 {
                let (u, v) = (t[k], t[(k + 1) % 3]);
                let (a, b, d) = if u < v { (u, v, 1i8) } else { (v, u, -1i8) };
                edges.push((((a as u64) << 32) | b as u64, d));
            }
        }
        edges.par_sort_unstable_by_key(|e| e.0);
        let (mut boundary, mut nonmanifold, mut flipped) = (0, 0, 0);
        let mut i = 0;
        while i < edges.len() {
            let mut j = i;
            let mut dir = 0i32;
            while j < edges.len() && edges[j].0 == edges[i].0 {
                dir += edges[j].1 as i32;
                j += 1;
            }
            match j - i {
                1 => boundary += 1,
                2 if dir != 0 => flipped += 1,
                2 => {}
                _ => nonmanifold += 1,
            }
            i = j;
        }
        drop(edges);
        let degenerate = (0..self.indices.len())
            .into_par_iter()
            .filter(|&i| {
                let [a, b, c] = self.triangle(i);
                (b - a).cross(c - a).length_squared() <= 1e-14
            })
            .count();
        let bounds = self.bounds();
        let volume = self.volume();
        MeshReport {
            triangles: self.indices.len(),
            vertices: self.positions.len(),
            bounds,
            size: bounds.size(),
            volume_mm3: volume.abs(),
            area_mm2: self.area(),
            watertight: boundary == 0 && nonmanifold == 0 && flipped == 0 && !self.indices.is_empty(),
            boundary_edges: boundary,
            non_manifold_edges: nonmanifold,
            flipped_edges: flipped,
            degenerate_triangles: degenerate,
            components: self.component_labels().1,
            inverted: volume < 0.0,
        }
    }

    /// Label each triangle with its connected component (shared vertices). Returns (labels, count).
    pub fn component_labels(&self) -> (Vec<u32>, usize) {
        let mut parent: Vec<u32> = (0..self.positions.len() as u32).collect();
        fn find(p: &mut [u32], mut x: u32) -> u32 {
            while p[x as usize] != x {
                p[x as usize] = p[p[x as usize] as usize];
                x = p[x as usize];
            }
            x
        }
        for t in &self.indices {
            let r0 = find(&mut parent, t[0]);
            for &v in &t[1..] {
                let r = find(&mut parent, v);
                if r != r0 {
                    parent[r as usize] = r0;
                }
            }
            // re-root to keep r0 valid
            let _ = find(&mut parent, t[0]);
        }
        let mut ids: HashMap<u32, u32> = HashMap::new();
        let labels = self
            .indices
            .iter()
            .map(|t| {
                let r = find(&mut parent, t[0]);
                let n = ids.len() as u32;
                *ids.entry(r).or_insert(n)
            })
            .collect();
        (labels, ids.len())
    }

    /// Split into connected components (each compacted).
    pub fn split_components(&self) -> Vec<Mesh> {
        let (labels, n) = self.component_labels();
        let mut parts = vec![Mesh { positions: self.positions.clone(), indices: Vec::new() }; n];
        for (t, l) in self.indices.iter().zip(labels) {
            parts[l as usize].indices.push(*t);
        }
        for p in &mut parts {
            p.compact();
        }
        parts
    }

    /// Mesh centroid of the solid (volume-weighted), falls back to bbox centre.
    pub fn centroid(&self) -> Vec3 {
        let mut c = DVec3::ZERO;
        let mut v = 0.0;
        for [a, b, cc] in self.triangles() {
            let (a, b, cc) = (a.as_dvec3(), b.as_dvec3(), cc.as_dvec3());
            let w = a.dot(b.cross(cc));
            c += (a + b + cc) * w;
            v += w;
        }
        if v.abs() < 1e-12 {
            self.bounds().center()
        } else {
            (c / (4.0 * v)).as_vec3()
        }
    }
}
