//! Watertight primitive generators. All shapes sit on Z=0 and are centred on X/Y unless noted.

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use glam::{Vec2, Vec3};
use serde::{Deserialize, Serialize};

use crate::{GeomError, Mesh, Result};

/// How an edge of a cylinder end is finished.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeFinish {
    #[default]
    Sharp,
    /// 45° chamfer with the given leg length (mm).
    Chamfer(f32),
    /// Quarter-round fillet with the given radius (mm).
    Fillet(f32),
}

/// Choose a segment count from a radius: at most ~0.01 mm chord error, so small holes and pins
/// come out true to size for fits (a 3.5 mm hole with 14 facets is ~0.06 mm undersize).
pub fn auto_segments(radius: f32) -> u32 {
    if radius <= 0.0 {
        return 8;
    }
    let tol = 0.01f32.min(radius * 0.5);
    let n = (PI / (1.0 - tol / radius).clamp(-1.0, 1.0).acos()).ceil() as u32;
    (n.div_ceil(4) * 4).clamp(24, 360)
}

/// Axis-aligned box from (0,0,0) to size, centred on X/Y.
pub fn cuboid(size: Vec3) -> Result<Mesh> {
    if size.min_element() <= 0.0 {
        return Err(GeomError::InvalidShape(format!("box size must be positive, got {size:?}")));
    }
    let h = Vec3::new(size.x / 2.0, size.y / 2.0, 0.0);
    let (x0, y0, x1, y1, z1) = (-h.x, -h.y, h.x, h.y, size.z);
    let p = vec![
        Vec3::new(x0, y0, 0.0),
        Vec3::new(x1, y0, 0.0),
        Vec3::new(x1, y1, 0.0),
        Vec3::new(x0, y1, 0.0),
        Vec3::new(x0, y0, z1),
        Vec3::new(x1, y0, z1),
        Vec3::new(x1, y1, z1),
        Vec3::new(x0, y1, z1),
    ];
    let i = vec![
        [0, 2, 1],
        [0, 3, 2],
        [4, 5, 6],
        [4, 6, 7],
        [0, 1, 5],
        [0, 5, 4],
        [1, 2, 6],
        [1, 6, 5],
        [2, 3, 7],
        [2, 7, 6],
        [3, 0, 4],
        [3, 4, 7],
    ];
    Ok(Mesh::from_parts(p, i))
}

/// Box with every edge and corner rounded (`segments` per quarter circle) — or chamfered when
/// `segments == 1`. Radius is clamped to half the smallest side.
pub fn rounded_box(size: Vec3, radius: f32, segments: u32) -> Result<Mesh> {
    if radius <= 1e-4 {
        return cuboid(size);
    }
    if size.min_element() <= 0.0 {
        return Err(GeomError::InvalidShape(format!("box size must be positive, got {size:?}")));
    }
    let half = size * 0.5;
    let r = radius.min(half.min_element());
    let n = segments.max(1);
    let core = half - Vec3::splat(r);
    let signs = [(1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)];
    let mut rings = Vec::new();
    for (zsign, phis) in [(-1.0f32, (-FRAC_PI_2, 0.0)), (1.0, (0.0, FRAC_PI_2))] {
        for j in 0..=n {
            let phi = phis.0 + (phis.1 - phis.0) * j as f32 / n as f32;
            let mut ring = Vec::with_capacity(4 * (n as usize + 1));
            for (q, (sx, sy)) in signs.iter().enumerate() {
                for k in 0..=n {
                    let th = (q as f32 + k as f32 / n as f32) * FRAC_PI_2;
                    ring.push(Vec3::new(
                        r * phi.cos() * th.cos() + sx * core.x,
                        r * phi.cos() * th.sin() + sy * core.y,
                        r * phi.sin() + zsign * core.z + half.z,
                    ));
                }
            }
            rings.push(ring);
        }
    }
    let mut m = loft_rings(&rings, true, true);
    m.clean();
    Ok(m)
}

/// Box whose vertical edges are rounded with `corner_radius` (a rounded rectangle extruded),
/// with optional chamfer/fillet on the bottom and top outer edges. Flat top and bottom.
pub fn rounded_slab(size: Vec3, corner_radius: f32, bottom: EdgeFinish, top: EdgeFinish) -> Result<Mesh> {
    if size.min_element() <= 0.0 {
        return Err(GeomError::InvalidShape(format!("box size must be positive, got {size:?}")));
    }
    let (hx, hy, h) = (size.x / 2.0, size.y / 2.0, size.z);
    let cr = corner_radius.clamp(0.0, hx.min(hy));
    let n = (auto_segments(cr.max(0.5)) / 4).max(4);
    let lim = |e: EdgeFinish| match e {
        EdgeFinish::Sharp => EdgeFinish::Sharp,
        EdgeFinish::Chamfer(c) => EdgeFinish::Chamfer(c.clamp(0.0, (hx.min(hy) * 0.99).min(h * 0.49))),
        EdgeFinish::Fillet(c) => EdgeFinish::Fillet(c.clamp(0.0, (hx.min(hy) * 0.99).min(h * 0.49))),
    };
    let (bottom, top) = (lim(bottom), lim(top));
    // ring at height z, inset from the outline by `inset`
    let ring = |z: f32, inset: f32| -> Vec<Vec3> {
        let r = (cr - inset).max(0.0);
        let (cx, cy) = (hx - inset - r, hy - inset - r);
        let mut v = Vec::with_capacity(4 * (n as usize + 1));
        for (q, (sx, sy)) in [(1.0f32, 1.0f32), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)].iter().enumerate() {
            for k in 0..=n {
                let a = (q as f32 + k as f32 / n as f32) * FRAC_PI_2;
                v.push(Vec3::new(sx * cx.max(0.0) + r * a.cos(), sy * cy.max(0.0) + r * a.sin(), z));
            }
        }
        v
    };
    let mut profile: Vec<(f32, f32)> = Vec::new(); // (z, inset) bottom → top
    match bottom {
        EdgeFinish::Sharp => profile.push((0.0, 0.0)),
        EdgeFinish::Chamfer(c) => {
            profile.push((0.0, c));
            profile.push((c, 0.0));
        }
        EdgeFinish::Fillet(c) => {
            let k = (auto_segments(c) / 4).max(3);
            for i in 0..=k {
                let a = -FRAC_PI_2 + FRAC_PI_2 * i as f32 / k as f32;
                profile.push((c + c * a.sin(), c - c * a.cos()));
            }
        }
    }
    match top {
        EdgeFinish::Sharp => profile.push((h, 0.0)),
        EdgeFinish::Chamfer(c) => {
            profile.push((h - c, 0.0));
            profile.push((h, c));
        }
        EdgeFinish::Fillet(c) => {
            let k = (auto_segments(c) / 4).max(3);
            for i in 0..=k {
                let a = FRAC_PI_2 * i as f32 / k as f32;
                profile.push((h - c + c * a.sin(), c - c * a.cos()));
            }
        }
    }
    let rings: Vec<Vec<Vec3>> = profile.iter().map(|(z, i)| ring(*z, *i)).collect();
    let mut m = loft_rings(&rings, true, true);
    m.clean();
    Ok(m)
}

/// Cylinder or cone frustum sitting on Z=0, with optional chamfer/fillet on each end edge.
pub fn cylinder(r_bottom: f32, r_top: f32, height: f32, segments: Option<u32>, bottom: EdgeFinish, top: EdgeFinish) -> Result<Mesh> {
    if height <= 0.0 || r_bottom < 0.0 || r_top < 0.0 || (r_bottom == 0.0 && r_top == 0.0) {
        return Err(GeomError::InvalidShape(format!(
            "cylinder needs height > 0 and a positive radius (r1={r_bottom}, r2={r_top}, h={height})"
        )));
    }
    let seg = segments.unwrap_or_else(|| auto_segments(r_bottom.max(r_top)));
    let mut prof = vec![Vec2::new(0.0, 0.0)];
    let limit = |e: EdgeFinish, r: f32| match e {
        EdgeFinish::Sharp => EdgeFinish::Sharp,
        EdgeFinish::Chamfer(c) => EdgeFinish::Chamfer(c.min(r * 0.999).min(height * 0.499).max(0.0)),
        EdgeFinish::Fillet(c) => EdgeFinish::Fillet(c.min(r * 0.999).min(height * 0.499).max(0.0)),
    };
    let (bottom, top) = (limit(bottom, r_bottom), limit(top, r_top));
    // bottom edge: from (r_bottom - c, 0) to (r_bottom, c)
    match bottom {
        EdgeFinish::Sharp => prof.push(Vec2::new(r_bottom, 0.0)),
        EdgeFinish::Chamfer(c) => {
            prof.push(Vec2::new(r_bottom - c, 0.0));
            prof.push(Vec2::new(r_bottom, c));
        }
        EdgeFinish::Fillet(c) => {
            let n = (auto_segments(c) / 4).max(3);
            for i in 0..=n {
                let a = -FRAC_PI_2 + FRAC_PI_2 * i as f32 / n as f32;
                prof.push(Vec2::new(r_bottom - c + c * a.cos(), c + c * a.sin()));
            }
        }
    }
    match top {
        EdgeFinish::Sharp => prof.push(Vec2::new(r_top, height)),
        EdgeFinish::Chamfer(c) => {
            prof.push(Vec2::new(r_top, height - c));
            prof.push(Vec2::new(r_top - c, height));
        }
        EdgeFinish::Fillet(c) => {
            let n = (auto_segments(c) / 4).max(3);
            for i in 0..=n {
                let a = FRAC_PI_2 * i as f32 / n as f32;
                prof.push(Vec2::new(r_top - c + c * a.cos(), height - c + c * a.sin()));
            }
        }
    }
    prof.push(Vec2::new(0.0, height));
    revolve(&prof, seg)
}

/// UV sphere of radius r, sitting on Z=0 (centre at z=r).
pub fn sphere(r: f32, segments: Option<u32>) -> Result<Mesh> {
    if r <= 0.0 {
        return Err(GeomError::InvalidShape(format!("sphere radius must be positive, got {r}")));
    }
    let seg = segments.unwrap_or_else(|| auto_segments(r));
    let n = (seg / 2).max(4);
    let prof: Vec<Vec2> = (0..=n)
        .map(|i| {
            let a = -FRAC_PI_2 + PI * i as f32 / n as f32;
            Vec2::new((r * a.cos()).max(0.0), r + r * a.sin())
        })
        .collect();
    revolve(&prof, seg)
}

/// Torus around Z: ring radius `major`, tube radius `minor`, sitting on Z=0.
pub fn torus(major: f32, minor: f32, segments: Option<u32>) -> Result<Mesh> {
    if minor <= 0.0 || major <= minor {
        return Err(GeomError::InvalidShape(format!("torus needs major > minor > 0 (major={major}, minor={minor})")));
    }
    let seg = segments.unwrap_or_else(|| auto_segments(major + minor));
    let n = auto_segments(minor).min(64);
    let prof: Vec<Vec2> = (0..n)
        .map(|i| {
            let a = TAU * i as f32 / n as f32;
            Vec2::new(major + minor * a.cos(), minor + minor * a.sin())
        })
        .collect();
    revolve_closed(&prof, seg)
}

/// Revolve an open profile (r, z) whose first and last points lie on the axis (r = 0).
pub fn revolve(profile: &[Vec2], segments: u32) -> Result<Mesh> {
    revolve_closed(profile, segments)
}

/// Revolve a closed profile polygon (implicitly closed last→first) in the r>=0 half-plane around Z.
pub fn revolve_closed(profile: &[Vec2], segments: u32) -> Result<Mesh> {
    if profile.len() < 2 {
        return Err(GeomError::InvalidShape("revolve profile needs at least 2 points".into()));
    }
    if profile.iter().any(|p| p.x < -1e-3) {
        return Err(GeomError::InvalidShape("revolve profile must have r >= 0 (x is the radius)".into()));
    }
    // snap near-axis points onto the axis (float noise at poles would leave micro-rings)
    let profile: Vec<Vec2> = profile.iter().map(|p| Vec2::new(if p.x.abs() < 1e-4 { 0.0 } else { p.x.max(0.0) }, p.y)).collect();
    let profile = profile.as_slice();
    let seg = segments.max(3) as usize;
    let np = profile.len();
    let mut pos = Vec::with_capacity(np * seg);
    for k in 0..seg {
        let th = TAU * k as f32 / seg as f32;
        let (s, c) = th.sin_cos();
        for p in profile {
            pos.push(Vec3::new(p.x * c, p.x * s, p.y));
        }
    }
    let id = |j: usize, k: usize| ((k % seg) * np + (j % np)) as u32;
    let mut idx = Vec::with_capacity(np * seg * 2);
    for k in 0..seg {
        for j in 0..np {
            let (a, b, c, d) = (id(j, k), id(j, k + 1), id(j + 1, k + 1), id(j + 1, k));
            idx.push([a, b, c]);
            idx.push([a, c, d]);
        }
    }
    let mut m = Mesh::from_parts(pos, idx);
    m.clean();
    Ok(m)
}

/// Extrude a simple polygon (any winding, no self-intersections) from z=0 to `height`.
pub fn extrude(polygon: &[Vec2], height: f32) -> Result<Mesh> {
    if polygon.len() < 3 || height <= 0.0 {
        return Err(GeomError::InvalidShape("extrude needs >= 3 points and height > 0".into()));
    }
    // points on a straight run (flattened font curves, arcs meeting lines) add nothing and
    // would leave the caps short of the walls: drop them, and near-duplicates
    let mut poly = simplify(polygon);
    if poly.len() < 3 {
        return Err(GeomError::InvalidShape("extrude needs an outline with area".into()));
    }
    if signed_area(&poly) < 0.0 {
        poly.reverse();
    }
    let tris = triangulate(&poly).ok_or_else(|| GeomError::InvalidShape("polygon is self-intersecting or degenerate".into()))?;
    let n = poly.len() as u32;
    let mut pos: Vec<Vec3> = poly.iter().map(|p| p.extend(0.0)).collect();
    pos.extend(poly.iter().map(|p| p.extend(height)));
    let mut idx = Vec::new();
    for t in &tris {
        idx.push([t[0], t[2], t[1]]);
        idx.push([t[0] + n, t[1] + n, t[2] + n]);
    }
    for i in 0..n {
        let j = (i + 1) % n;
        idx.push([i, j, j + n]);
        idx.push([i, j + n, i + n]);
    }
    let mut m = Mesh::from_parts(pos, idx);
    m.clean();
    Ok(m)
}

/// Connect closed rings (each CCW seen from +Z, same length) bottom→top; optionally cap the ends
/// with a fan (rings must be convex there).
pub fn loft_rings(rings: &[Vec<Vec3>], cap_bottom: bool, cap_top: bool) -> Mesh {
    let n = rings[0].len();
    let mut pos = Vec::with_capacity(rings.len() * n);
    for r in rings {
        pos.extend_from_slice(r);
    }
    let id = |ring: usize, k: usize| (ring * n + (k % n)) as u32;
    let mut idx = Vec::new();
    for r in 0..rings.len() - 1 {
        for k in 0..n {
            let (a0, a1, b0, b1) = (id(r, k), id(r, k + 1), id(r + 1, k), id(r + 1, k + 1));
            idx.push([a0, a1, b1]);
            idx.push([a0, b1, b0]);
        }
    }
    if cap_bottom {
        for k in 1..n - 1 {
            idx.push([id(0, 0), id(0, k + 1), id(0, k)]);
        }
    }
    if cap_top {
        let t = rings.len() - 1;
        for k in 1..n - 1 {
            idx.push([id(t, 0), id(t, k), id(t, k + 1)]);
        }
    }
    Mesh::from_parts(pos, idx)
}

/// The outline without repeated points and points on a straight line between their neighbours.
pub fn simplify(poly: &[Vec2]) -> Vec<Vec2> {
    let mut v: Vec<Vec2> = Vec::with_capacity(poly.len());
    for p in poly {
        if v.last().is_none_or(|q: &Vec2| q.distance(*p) > 1e-4) {
            v.push(*p);
        }
    }
    while v.len() > 3 && v[0].distance(*v.last().unwrap()) <= 1e-4 {
        v.pop();
    }
    loop {
        let n = v.len();
        if n <= 3 {
            return v;
        }
        let drop = (0..n).find(|&i| {
            let (a, b, c) = (v[(i + n - 1) % n], v[i], v[(i + 1) % n]);
            let (ab, bc, ac) = (b - a, c - b, c - a);
            // b within 0.001 mm of the line a–c (far below print resolution) and between them:
            // such points only make sliver triangles that cleaning would drop from the caps
            ac.length() > 1e-9 && (ac.perp_dot(ab)).abs() / ac.length() < 1e-3 && ab.dot(bc) > 0.0
        });
        match drop {
            Some(i) => {
                v.remove(i);
            }
            None => return v,
        }
    }
}

pub fn signed_area(poly: &[Vec2]) -> f32 {
    let mut a = 0.0;
    for i in 0..poly.len() {
        let (p, q) = (poly[i], poly[(i + 1) % poly.len()]);
        a += p.x * q.y - q.x * p.y;
    }
    a * 0.5
}

/// Ear-clipping triangulation of a simple CCW polygon. Returns index triples.
pub fn triangulate(poly: &[Vec2]) -> Option<Vec<[u32; 3]>> {
    let n = poly.len();
    if n < 3 {
        return None;
    }
    let mut v: Vec<usize> = (0..n).collect();
    let mut out = Vec::with_capacity(n - 2);
    let cross = |o: Vec2, a: Vec2, b: Vec2| (a - o).perp_dot(b - o);
    let mut guard = 0;
    while v.len() > 3 {
        let m = v.len();
        let mut clipped = false;
        for i in 0..m {
            let (ia, ib, ic) = (v[(i + m - 1) % m], v[i], v[(i + 1) % m]);
            let (a, b, c) = (poly[ia], poly[ib], poly[ic]);
            if cross(a, b, c) <= 1e-12 {
                continue; // reflex or collinear
            }
            let inside = v.iter().any(|&j| {
                if j == ia || j == ib || j == ic {
                    return false;
                }
                let p = poly[j];
                cross(a, b, p) >= 0.0 && cross(b, c, p) >= 0.0 && cross(c, a, p) >= 0.0
            });
            if inside {
                continue;
            }
            out.push([ia as u32, ib as u32, ic as u32]);
            v.remove(i);
            clipped = true;
            break;
        }
        if !clipped {
            // Collinear leftovers: drop one vertex to make progress, fail if it keeps happening.
            guard += 1;
            if guard > n {
                return None;
            }
            let i = (0..v.len())
                .find(|&i| {
                    let m = v.len();
                    cross(poly[v[(i + m - 1) % m]], poly[v[i]], poly[v[(i + 1) % m]]).abs() <= 1e-9
                })
                .ok_or(())
                .ok()?;
            v.remove(i);
        }
    }
    out.push([v[0] as u32, v[1] as u32, v[2] as u32]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(m: &Mesh, vol: f64, tol: f64) {
        let r = m.analyze();
        assert!(r.watertight, "not watertight: {r:?}");
        assert!(!r.inverted);
        assert_eq!(r.components, 1);
        assert!((r.volume_mm3 - vol).abs() / vol < tol, "volume {} vs {}", r.volume_mm3, vol);
    }

    #[test]
    fn primitives_are_watertight() {
        check(&cuboid(Vec3::new(10.0, 20.0, 30.0)).unwrap(), 6000.0, 1e-6);
        let rb = rounded_box(Vec3::new(20.0, 20.0, 20.0), 2.0, 6).unwrap();
        // Minkowski sum of a 16³ core with a sphere r=2
        let pi = PI as f64;
        check(&rb, 4096.0 + 3072.0 + pi * 4.0 * 48.0 + 4.0 / 3.0 * pi * 8.0, 0.01);
        // Chamfer = Minkowski sum of an 18x18x8 core with an octahedron r=1
        let ch = rounded_box(Vec3::new(20.0, 20.0, 10.0), 1.0, 1).unwrap();
        check(&ch, 2592.0 + 1224.0 + 88.0 + 4.0 / 3.0, 1e-4);
        let c = cylinder(10.0, 10.0, 10.0, Some(128), EdgeFinish::Sharp, EdgeFinish::Sharp).unwrap();
        check(&c, PI as f64 * 1000.0, 0.01);
        let cf = cylinder(10.0, 10.0, 10.0, Some(128), EdgeFinish::Chamfer(1.0), EdgeFinish::Fillet(2.0)).unwrap();
        check(&cf, PI as f64 * 1000.0, 0.05);
        let cone = cylinder(10.0, 0.0, 10.0, Some(128), EdgeFinish::Sharp, EdgeFinish::Sharp).unwrap();
        check(&cone, PI as f64 * 1000.0 / 3.0, 0.01);
        let s = sphere(10.0, Some(96)).unwrap();
        check(&s, 4.0 / 3.0 * PI as f64 * 1000.0, 0.01);
        let sl = rounded_slab(Vec3::new(40.0, 30.0, 10.0), 5.0, EdgeFinish::Chamfer(1.0), EdgeFinish::Fillet(2.0)).unwrap();
        check(&sl, (40.0 * 30.0 - (4.0 - PI as f64) * 25.0) * 10.0, 0.05);
        let sl2 = rounded_slab(Vec3::new(40.0, 30.0, 10.0), 5.0, EdgeFinish::Sharp, EdgeFinish::Sharp).unwrap();
        check(&sl2, (40.0 * 30.0 - (4.0 - PI as f64) * 25.0) * 10.0, 0.005);
        let big = sphere(35.0, None).unwrap();
        check(&big, 4.0 / 3.0 * PI as f64 * 35f64.powi(3), 0.01);
        let t = torus(20.0, 5.0, None).unwrap();
        check(&t, 2.0 * (PI as f64).powi(2) * 20.0 * 25.0, 0.02);
        let l = extrude(&[Vec2::new(0., 0.), Vec2::new(20., 0.), Vec2::new(20., 5.), Vec2::new(5., 5.), Vec2::new(5., 20.), Vec2::new(0., 20.)], 4.0).unwrap();
        check(&l, (20.0 * 5.0 + 5.0 * 15.0) * 4.0, 1e-5);
    }
}

// -------------------------------------------------------------------------------------------
// 2D outlines (counter-clockwise, centred on the origin) for the profile primitives

/// A slot (obround): `length` end to end along X, `width` across, round ends.
pub fn slot_outline(length: f32, width: f32) -> Result<Vec<Vec2>> {
    if !(width > 0.0 && length >= width) {
        return Err(GeomError::InvalidShape("slot needs length ≥ width > 0".into()));
    }
    let r = width / 2.0;
    let half = (length - width) / 2.0;
    let seg = (auto_segments(r) / 2).max(6);
    let mut v = Vec::new();
    for k in 0..=seg {
        let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * k as f32 / seg as f32;
        v.push(Vec2::new(half + r * a.cos(), r * a.sin()));
    }
    for k in 0..=seg {
        let a = std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * k as f32 / seg as f32;
        v.push(Vec2::new(-half + r * a.cos(), r * a.sin()));
    }
    if half <= 1e-6 {
        v.dedup_by(|a, b| a.distance(*b) < 1e-5);
    }
    Ok(v)
}

/// A regular polygon with `sides`, corners on a circle of radius `r`; a flat side faces −Y.
pub fn regular_polygon(sides: u32, r: f32) -> Result<Vec<Vec2>> {
    if sides < 3 || r.is_nan() || r <= 0.0 {
        return Err(GeomError::InvalidShape("polygon needs ≥ 3 sides and a size > 0".into()));
    }
    let start = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI / sides as f32;
    Ok((0..sides).map(|k| {
        let a = start + std::f32::consts::TAU * k as f32 / sides as f32;
        Vec2::new(r * a.cos(), r * a.sin())
    })
    .collect())
}

/// Round every corner of a simple polygon with radius `r` (capped where edges are short).
pub fn round_corners(poly: &[Vec2], r: f32) -> Vec<Vec2> {
    if r <= 0.0 || poly.len() < 3 {
        return poly.to_vec();
    }
    let n = poly.len();
    let mut out = Vec::new();
    for i in 0..n {
        let (p0, p1, p2) = (poly[(i + n - 1) % n], poly[i], poly[(i + 1) % n]);
        let (d0, d1) = ((p0 - p1).normalize_or_zero(), (p2 - p1).normalize_or_zero());
        let cos = d0.dot(d1).clamp(-1.0, 1.0);
        let half = cos.acos() / 2.0;
        if half < 1e-3 || (std::f32::consts::FRAC_PI_2 - half) < 1e-3 {
            out.push(p1);
            continue;
        }
        // distance from the corner to the tangent points, limited to half of each edge
        let t = (r / half.tan()).min(p0.distance(p1) * 0.5).min(p2.distance(p1) * 0.5);
        let rr = t * half.tan();
        let (a, b) = (p1 + d0 * t, p1 + d1 * t);
        let c = p1 + (d0 + d1).normalize_or_zero() * (rr / half.sin());
        let (a0, a1) = ((a - c).to_angle(), (b - c).to_angle());
        let mut sweep = a1 - a0;
        // the short way round
        while sweep > std::f32::consts::PI {
            sweep -= std::f32::consts::TAU;
        }
        while sweep < -std::f32::consts::PI {
            sweep += std::f32::consts::TAU;
        }
        let seg = ((sweep.abs() / std::f32::consts::FRAC_PI_2) * (auto_segments(rr) as f32 / 4.0)).ceil().max(2.0) as u32;
        for k in 0..=seg {
            let ang = a0 + sweep * k as f32 / seg as f32;
            out.push(c + Vec2::new(ang.cos(), ang.sin()) * rr);
        }
    }
    out
}

/// Move every edge of a counter-clockwise polygon inward by `d` (mitred corners). Good for
/// convex and gently concave outlines; returns None when the result folds over itself.
pub fn inset(poly: &[Vec2], d: f32) -> Option<Vec<Vec2>> {
    let n = poly.len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let (p0, p1, p2) = (poly[(i + n - 1) % n], poly[i], poly[(i + 1) % n]);
        let n0 = { let e = (p1 - p0).normalize_or_zero(); Vec2::new(-e.y, e.x) };
        let n1 = { let e = (p2 - p1).normalize_or_zero(); Vec2::new(-e.y, e.x) };
        let bis = (n0 + n1).normalize_or_zero();
        let cos = bis.dot(n0).max(0.2);
        out.push(p1 + bis * (d / cos));
    }
    let a0 = signed_area(poly);
    let a1 = signed_area(&out);
    // every edge keeps its direction (a square inset past its middle flips twice and would
    // otherwise look fine)
    let kept = (0..n).all(|i| (out[(i + 1) % n] - out[i]).dot(poly[(i + 1) % n] - poly[i]) > 0.0);
    (kept && a1 > 0.0 && a1 < a0 && triangulate(&out).is_some()).then_some(out)
}

#[cfg(test)]
mod outline_tests {
    use super::*;

    #[test]
    fn slots_polygons_rounding_and_insets() {
        let s = slot_outline(40.0, 10.0).unwrap();
        let (lo, hi) = s.iter().fold((Vec2::splat(f32::MAX), Vec2::splat(f32::MIN)), |(l, h), p| (l.min(*p), h.max(*p)));
        assert!((hi.x - lo.x - 40.0).abs() < 0.01 && (hi.y - lo.y - 10.0).abs() < 0.01);
        assert!(signed_area(&s) > 0.0);
        let hex = regular_polygon(6, 10.0).unwrap();
        let flats = hex.iter().map(|p| p.y).fold(f32::MIN, f32::max) - hex.iter().map(|p| p.y).fold(f32::MAX, f32::min);
        assert!((flats - 10.0 * 3f32.sqrt()).abs() < 0.01, "flat to flat {flats}");
        let sq = vec![Vec2::new(-10.0, -10.0), Vec2::new(10.0, -10.0), Vec2::new(10.0, 10.0), Vec2::new(-10.0, 10.0)];
        let r = round_corners(&sq, 3.0);
        let area = signed_area(&r);
        let want = 400.0 - (4.0 - std::f32::consts::PI) * 9.0;
        assert!((area - want).abs() < 1.0, "{area} vs {want}");
        let i = inset(&sq, 2.0).unwrap();
        assert!((signed_area(&i) - 256.0).abs() < 0.1);
        assert!(inset(&sq, 11.0).is_none());
    }
}
