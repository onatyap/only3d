//! 3D convex hull (incremental, f64). OpenSCAD's `hull()` is one of the most used modelling
//! tools for soft, organic-but-printable forms, so it is a first-class operation here.

use std::collections::HashMap;

use glam::{DVec3, Vec3};

use crate::Mesh;

struct Face {
    v: [usize; 3],
    n: DVec3,
    d: f64,
    alive: bool,
}

fn make_face(p: &[DVec3], a: usize, b: usize, c: usize) -> Face {
    let n = (p[b] - p[a]).cross(p[c] - p[a]).normalize_or_zero();
    Face { v: [a, b, c], n, d: n.dot(p[a]), alive: true }
}

/// Convex hull of a point cloud. Returns `None` when the points are (nearly) coplanar.
///
/// The result is always a closed 2-manifold (every edge shared by exactly two triangles), so it
/// can feed booleans directly. Points within a small tolerance of the hull are treated as on it;
/// the tolerance suits single-precision input (mesh vertices), which is what the modeller hulls.
pub fn convex_hull(points: &[Vec3]) -> Option<Mesh> {
    // a looser tolerance only if the strict one could not produce a manifold (pathological input)
    for rel in [1e-7, 1e-5, 1e-4] {
        if let Some(m) = hull_with(points, rel) {
            return Some(m);
        }
    }
    None
}

fn hull_with(points: &[Vec3], rel_eps: f64) -> Option<Mesh> {
    // dedupe
    let mut seen = HashMap::new();
    let mut p: Vec<DVec3> = Vec::new();
    for q in points {
        let key = ((q.x * 1e4).round() as i64, (q.y * 1e4).round() as i64, (q.z * 1e4).round() as i64);
        seen.entry(key).or_insert_with(|| {
            p.push(q.as_dvec3());
        });
    }
    if p.len() < 4 {
        return None;
    }
    let scale = p.iter().fold(0.0f64, |m, q| m.max(q.abs().max_element())).max(1.0);
    let eps = rel_eps * scale;

    // initial simplex: extreme x pair, farthest from line, farthest from plane
    let (mut i0, mut i1) = (0, 0);
    for (i, q) in p.iter().enumerate() {
        if q.x < p[i0].x {
            i0 = i;
        }
        if q.x > p[i1].x {
            i1 = i;
        }
    }
    if i0 == i1 {
        i1 = (0..p.len()).max_by(|&a, &b| p[a].distance(p[i0]).total_cmp(&p[b].distance(p[i0])))?;
    }
    let dir = (p[i1] - p[i0]).normalize_or_zero();
    let i2 = (0..p.len()).max_by(|&a, &b| {
        let da = (p[a] - p[i0]).cross(dir).length();
        let db = (p[b] - p[i0]).cross(dir).length();
        da.total_cmp(&db)
    })?;
    let n = (p[i1] - p[i0]).cross(p[i2] - p[i0]).normalize_or_zero();
    if n.length_squared() < 0.5 {
        return None;
    }
    let i3 = (0..p.len()).max_by(|&a, &b| n.dot(p[a] - p[i0]).abs().total_cmp(&n.dot(p[b] - p[i0]).abs()))?;
    if n.dot(p[i3] - p[i0]).abs() < eps * 10.0 {
        return None;
    }

    let mut faces: Vec<Face> = Vec::new();
    // directed edge (a, b) -> the live face that has it
    let mut edges: HashMap<(usize, usize), usize> = HashMap::new();
    let add = |faces: &mut Vec<Face>, edges: &mut HashMap<(usize, usize), usize>, a: usize, b: usize, c: usize| {
        let f = faces.len();
        faces.push(make_face(&p, a, b, c));
        edges.insert((a, b), f);
        edges.insert((b, c), f);
        edges.insert((c, a), f);
    };
    let centroid = (p[i0] + p[i1] + p[i2] + p[i3]) / 4.0;
    for (a, b, c) in [(i0, i1, i2), (i0, i3, i1), (i1, i3, i2), (i2, i3, i0)] {
        let f = make_face(&p, a, b, c);
        if f.n.dot(centroid) - f.d > 0.0 {
            add(&mut faces, &mut edges, a, c, b);
        } else {
            add(&mut faces, &mut edges, a, b, c);
        }
    }

    let mut order: Vec<usize> = (0..p.len()).filter(|&i| ![i0, i1, i2, i3].contains(&i)).collect();
    // deterministic shuffle for average-case performance
    let mut s: u64 = 0x9E3779B97F4A7C15;
    for i in (1..order.len()).rev() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        order.swap(i, (s % (i as u64 + 1)) as usize);
    }

    let dist = |f: &Face, q: DVec3| f.n.dot(q) - f.d;
    for &pi in &order {
        let q = p[pi];
        // the face the point is farthest outside of seeds the visible region
        let Some((seed, d)) = faces.iter().enumerate().filter(|(_, f)| f.alive).map(|(i, f)| (i, dist(f, q))).max_by(|a, b| a.1.total_cmp(&b.1)) else { break };
        if d <= eps {
            continue; // inside or on the hull
        }
        // grow it across edges, so the region is connected and its boundary is one loop
        let mut visible = vec![seed];
        let mut is_visible: HashMap<usize, bool> = HashMap::from([(seed, true)]);
        let mut k = 0;
        while k < visible.len() {
            let f = visible[k];
            k += 1;
            let v = faces[f].v;
            for e in 0..3 {
                let (a, b) = (v[e], v[(e + 1) % 3]);
                let Some(&g) = edges.get(&(b, a)) else { continue };
                if is_visible.contains_key(&g) {
                    continue;
                }
                let vis = dist(&faces[g], q) > eps;
                is_visible.insert(g, vis);
                if vis {
                    visible.push(g);
                }
            }
        }
        // horizon: edges of the region whose neighbour is not visible
        let mut horizon: Vec<(usize, usize)> = Vec::new();
        for &f in &visible {
            let v = faces[f].v;
            for e in 0..3 {
                let (a, b) = (v[e], v[(e + 1) % 3]);
                let g = edges.get(&(b, a)).copied();
                if g.is_none_or(|g| !is_visible.get(&g).copied().unwrap_or(false)) {
                    horizon.push((a, b));
                }
            }
        }
        // the horizon must be one simple loop; if rounding made it anything else, leave this
        // point out (it lies within tolerance of the hull anyway)
        let mut starts: HashMap<usize, usize> = HashMap::new();
        if horizon.iter().any(|&(a, _)| {
            let c = starts.entry(a).or_insert(0);
            *c += 1;
            *c > 1
        }) || horizon.len() < 3
        {
            continue;
        }
        for &f in &visible {
            faces[f].alive = false;
            let v = faces[f].v;
            for e in 0..3 {
                edges.remove(&(v[e], v[(e + 1) % 3]));
            }
        }
        for (a, b) in horizon {
            add(&mut faces, &mut edges, a, b, pi);
        }
    }

    // every live directed edge must have its twin: a closed 2-manifold
    if faces.iter().filter(|f| f.alive).any(|f| (0..3).any(|e| !edges.contains_key(&(f.v[(e + 1) % 3], f.v[e])))) {
        return None;
    }
    let mut used: HashMap<usize, u32> = HashMap::new();
    let mut pos = Vec::new();
    let mut idx = Vec::new();
    for f in faces.iter().filter(|f| f.alive) {
        let mut t = [0u32; 3];
        for k in 0..3 {
            t[k] = *used.entry(f.v[k]).or_insert_with(|| {
                pos.push(p[f.v[k]].as_vec3());
                (pos.len() - 1) as u32
            });
        }
        idx.push(t);
    }
    // sliver triangles are kept: dropping them would open holes in the surface
    Some(Mesh::from_parts(pos, idx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shapes;

    fn assert_closed(m: &Mesh, what: &str) {
        let r = m.analyze();
        assert!(r.watertight && r.boundary_edges == 0 && r.non_manifold_edges == 0, "{what}: {r:?}");
    }

    #[test]
    fn teardrop_hull_from_an_agent_is_manifold() {
        // the wall-hook eval: a teardrop screw hole = hull(cylinder along X, thin box at its top).
        // The old hull produced 214 non-manifold edges here and the boolean failed.
        for (box_w, box_z) in [(0.1f32, 4.0 * 0.4358 - 0.05), (0.4, 4.0 * 0.8717 - 0.3)] {
            let mut cyl = shapes::cylinder(2.0, 2.0, 7.0, None, shapes::EdgeFinish::Sharp, shapes::EdgeFinish::Sharp).unwrap();
            cyl.translate(Vec3::new(0.0, 0.0, -3.5));
            cyl.transform(&glam::Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2));
            let mut b = shapes::cuboid(Vec3::new(7.0, box_w, box_w)).unwrap();
            b.translate(Vec3::new(0.0, 0.0, box_z - box_w / 2.0));
            cyl.append(&b);
            cyl.translate(Vec3::new(2.5, 42.0, 10.0));
            let h = convex_hull(&cyl.positions).unwrap();
            assert_closed(&h, &format!("teardrop box {box_w}"));
        }
    }

    #[test]
    fn hulls_of_awkward_point_sets_are_manifold() {
        // many coplanar points (cylinder caps), boxes touching, a cloud with near-duplicates
        let c = shapes::cylinder(10.0, 10.0, 20.0, Some(96), shapes::EdgeFinish::Sharp, shapes::EdgeFinish::Sharp).unwrap();
        assert_closed(&convex_hull(&c.positions).unwrap(), "cylinder");
        let mut a = shapes::cuboid(Vec3::splat(10.0)).unwrap();
        let mut b = a.clone();
        b.translate(Vec3::new(10.0, 0.0, 0.0));
        a.append(&b);
        assert_closed(&convex_hull(&a.positions).unwrap(), "touching boxes");
        let mut pts = Vec::new();
        let mut s: u64 = 7;
        for _ in 0..3000 {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let f = |k: u64| ((s >> k) & 0xffff) as f32 / 65535.0;
            let v = Vec3::new(f(0), f(16), f(32)) * 40.0;
            pts.push(v);
            pts.push(v + Vec3::splat(1e-6)); // near-duplicate
        }
        assert_closed(&convex_hull(&pts).unwrap(), "random cloud");
    }

    #[test]
    fn hull_of_two_spheres_is_capsule() {
        let mut a = shapes::sphere(5.0, Some(32)).unwrap();
        let mut b = a.clone();
        b.translate(Vec3::new(20.0, 0.0, 0.0));
        a.append(&b);
        let h = convex_hull(&a.positions).unwrap();
        let r = h.analyze();
        assert!(r.watertight, "{r:?}");
        let expected = std::f64::consts::PI * 25.0 * 20.0 + 4.0 / 3.0 * std::f64::consts::PI * 125.0;
        assert!((r.volume_mm3 - expected).abs() / expected < 0.03, "{} vs {}", r.volume_mm3, expected);
    }
}
