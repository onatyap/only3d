//! Flat faces of a mesh: the connected coplanar region around a triangle, and a closed prism
//! swept from it along its normal (push/pull).

use std::collections::HashMap;

use glam::Vec3;

use crate::Mesh;

/// Normals within about 1.8° count as the same plane.
const SAME_NORMAL: f32 = 0.9995;

/// A flat face: coplanar triangles connected across shared edges.
#[derive(Clone, Debug)]
pub struct FlatFace {
    /// Triangle indices into the mesh.
    pub tris: Vec<usize>,
    /// Outward unit normal.
    pub normal: Vec3,
    /// Its boundary as directed edges (the face on the left, seen from outside), holes included.
    pub edges: Vec<[Vec3; 2]>,
    pub area: f64,
}

impl FlatFace {
    /// The centre of the face (area-weighted).
    pub fn centre(&self, m: &Mesh) -> Vec3 {
        let mut c = Vec3::ZERO;
        let mut w = 0.0f32;
        for &t in &self.tris {
            let [a, b, d] = m.triangle(t);
            let ar = (b - a).cross(d - a).length() * 0.5;
            c += (a + b + d) / 3.0 * ar;
            w += ar;
        }
        if w > 0.0 {
            c / w
        } else {
            m.triangle(self.tris[0])[0]
        }
    }
}

fn key(p: Vec3) -> (i64, i64, i64) {
    let q = 1e4f64;
    ((p.x as f64 * q).round() as i64, (p.y as f64 * q).round() as i64, (p.z as f64 * q).round() as i64)
}

/// The flat face triangle `seed` lies in: every triangle reachable from it across shared edges
/// whose normal matches and whose corners are within `tol` mm of its plane. Vertices are
/// matched by position, so meshes that repeat vertices per triangle work too.
pub fn flat_region(m: &Mesh, seed: usize, tol: f32) -> Option<FlatFace> {
    if seed >= m.triangle_count() {
        return None;
    }
    let n = m.face_normal(seed);
    if !n.is_finite() || n.length_squared() < 0.25 {
        return None;
    }
    let p0 = m.triangle(seed)[0];
    let on_plane = |t: usize| {
        let tn = m.face_normal(t);
        tn.dot(n) > SAME_NORMAL && m.triangle(t).iter().all(|v| (*v - p0).dot(n).abs() <= tol)
    };
    // only the coplanar triangles take part, so this stays cheap on big meshes
    let cands: Vec<usize> = (0..m.triangle_count()).filter(|t| *t == seed || on_plane(*t)).collect();
    let mut by_edge: HashMap<((i64, i64, i64), (i64, i64, i64)), Vec<usize>> = HashMap::new();
    for &t in &cands {
        let v = m.triangle(t);
        for k in 0..3 {
            let (a, b) = (key(v[k]), key(v[(k + 1) % 3]));
            by_edge.entry(if a < b { (a, b) } else { (b, a) }).or_default().push(t);
        }
    }
    let mut seen: HashMap<usize, ()> = HashMap::new();
    let mut stack = vec![seed];
    seen.insert(seed, ());
    while let Some(t) = stack.pop() {
        let v = m.triangle(t);
        for k in 0..3 {
            let (a, b) = (key(v[k]), key(v[(k + 1) % 3]));
            if let Some(ns) = by_edge.get(&if a < b { (a, b) } else { (b, a) }) {
                for &u in ns {
                    if seen.insert(u, ()).is_none() {
                        stack.push(u);
                    }
                }
            }
        }
    }
    let mut tris: Vec<usize> = seen.into_keys().collect();
    tris.sort_unstable();
    // boundary: edges used once within the face
    let mut count: HashMap<((i64, i64, i64), (i64, i64, i64)), u32> = HashMap::new();
    for &t in &tris {
        let v = m.triangle(t);
        for k in 0..3 {
            let (a, b) = (key(v[k]), key(v[(k + 1) % 3]));
            *count.entry(if a < b { (a, b) } else { (b, a) }).or_default() += 1;
        }
    }
    let mut edges = Vec::new();
    let mut area = 0.0f64;
    for &t in &tris {
        let v = m.triangle(t);
        area += ((v[1] - v[0]).cross(v[2] - v[0]).length() * 0.5) as f64;
        for k in 0..3 {
            let (a, b) = (key(v[k]), key(v[(k + 1) % 3]));
            if count[&if a < b { (a, b) } else { (b, a) }] == 1 {
                edges.push([v[k], v[(k + 1) % 3]]);
            }
        }
    }
    Some(FlatFace { tris, normal: n.normalize(), edges, area })
}

/// The flat face at `point` whose outward normal is close to `normal`: the triangle containing
/// the point (within `tol` mm), else the nearest coplanar one.
pub fn face_at(m: &Mesh, point: Vec3, normal: Vec3, tol: f32) -> Option<FlatFace> {
    let n = normal.normalize_or_zero();
    let mut best: Option<(f32, usize)> = None;
    for t in 0..m.triangle_count() {
        let tn = m.face_normal(t);
        if tn.dot(n) < 0.99 {
            continue;
        }
        let v = m.triangle(t);
        if (point - v[0]).dot(tn).abs() > tol.max(0.05) {
            continue;
        }
        let d = closest_on_triangle(point, v).distance(point);
        if best.is_none_or(|b| d < b.0) {
            best = Some((d, t));
        }
    }
    let (d, t) = best?;
    if d > 1.0 {
        return None;
    }
    flat_region(m, t, tol)
}

/// The flat face nearest `point` (within `max` mm), whatever way it faces.
pub fn face_near(m: &Mesh, point: Vec3, max: f32, tol: f32) -> Option<FlatFace> {
    let mut best: Option<(f32, usize)> = None;
    for t in 0..m.triangle_count() {
        let v = m.triangle(t);
        let d = closest_on_triangle(point, v).distance(point);
        // on an edge both faces are as near: the one the point is on the plane of wins
        let d = d + (point - v[0]).dot(m.face_normal(t)).abs() * 1e-3;
        if best.is_none_or(|b| d < b.0) {
            best = Some((d, t));
        }
    }
    let (d, t) = best?;
    if d > max {
        return None;
    }
    flat_region(m, t, tol)
}

fn closest_on_triangle(p: Vec3, [a, b, c]: [Vec3; 3]) -> Vec3 {
    // Ericson, Real-Time Collision Detection 5.1.5
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return a + ab * (d1 / (d1 - d3));
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return a + ac * (d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let denom = 1.0 / (va + vb + vc);
    a + ab * (vb * denom) + ac * (vc * denom)
}

/// A closed prism over `face`, from `lo` to `hi` mm along its normal (`lo` < `hi`): the face's
/// own triangles as the two caps, a wall on every boundary edge.
pub fn prism(m: &Mesh, face: &FlatFace, lo: f32, hi: f32) -> Mesh {
    let n = face.normal;
    let mut tris: Vec<[Vec3; 3]> = Vec::with_capacity(face.tris.len() * 2 + face.edges.len() * 2);
    for &t in &face.tris {
        let [a, b, c] = m.triangle(t);
        tris.push([a + n * hi, b + n * hi, c + n * hi]);
        tris.push([a + n * lo, c + n * lo, b + n * lo]);
    }
    for [a, b] in &face.edges {
        let (a0, b0, a1, b1) = (*a + n * lo, *b + n * lo, *a + n * hi, *b + n * hi);
        tris.push([a0, b0, b1]);
        tris.push([a0, b1, a1]);
    }
    Mesh::from_soup(&tris)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shapes;

    #[test]
    fn a_box_top_is_one_face_and_its_prism_is_closed() {
        let m = shapes::cuboid(Vec3::new(20.0, 10.0, 5.0)).unwrap();
        let top = face_at(&m, Vec3::new(3.0, 2.0, 5.0), Vec3::Z, 0.01).unwrap();
        assert!((top.area - 200.0).abs() < 1e-3, "{}", top.area);
        assert!((top.normal - Vec3::Z).length() < 1e-5);
        assert_eq!(top.edges.len(), 4, "a rectangle's outline");
        let p = prism(&m, &top, -0.01, 7.0);
        let r = p.analyze();
        assert!(r.watertight, "{r:?}");
        assert!((r.volume_mm3 - 200.0 * 7.01).abs() < 0.05, "{}", r.volume_mm3);
        // a side face
        let side = face_at(&m, Vec3::new(10.0, 0.0, 2.0), Vec3::X, 0.01).unwrap();
        assert!((side.area - 50.0).abs() < 1e-3);
        // nothing there
        assert!(face_at(&m, Vec3::new(3.0, 2.0, 9.0), Vec3::Z, 0.01).is_none());
    }

    #[test]
    fn a_face_with_a_hole_keeps_it() {
        // a frame: the top of a 20 mm square with a 10 mm square hole, as a soup
        let q = |x0: f32, y0: f32, x1: f32, y1: f32| [[Vec3::new(x0, y0, 0.0), Vec3::new(x1, y0, 0.0), Vec3::new(x1, y1, 0.0)], [Vec3::new(x0, y0, 0.0), Vec3::new(x1, y1, 0.0), Vec3::new(x0, y1, 0.0)]];
        let mut tris = Vec::new();
        let g = [-10.0, -5.0, 5.0, 10.0];
        for i in 0..3 {
            for j in 0..3 {
                if (i, j) != (1, 1) {
                    tris.extend(q(g[i], g[j], g[i + 1], g[j + 1]));
                }
            }
        }
        let m = Mesh::from_soup(&tris);
        let f = flat_region(&m, 0, 0.01).unwrap();
        assert_eq!(f.tris.len(), 16);
        assert_eq!(f.edges.len(), 16, "12 outside, 4 around the hole");
        assert!((f.area - 300.0).abs() < 1e-3);
        let p = prism(&m, &f, 0.0, 3.0);
        let r = p.analyze();
        assert!(r.watertight, "{r:?}");
        assert!((r.volume_mm3 - 900.0).abs() < 0.05, "{}", r.volume_mm3);
    }
}
