//! CPU-side scene helpers: plate geometry, toolpath buffers, colours and ray picking.

use glam::Vec3;
use only3d_geom::Mesh;
use only3d_model::{Bed, EvaluatedObject};
use only3d_slice::Feature;
use rayon::prelude::*;

use crate::gpu::{ObjInstance, PlateData, Seg, ToolpathData};
use crate::theme;

pub fn parse_hex(c: &str) -> [u8; 3] {
    let s = c.trim_start_matches('#');
    let h = |i: usize| s.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok()).unwrap_or(200);
    [h(0), h(2), h(4)]
}

pub fn srgb_to_linear(c: [u8; 3]) -> [f32; 4] {
    let f = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    [f(c[0]), f(c[1]), f(c[2]), 1.0]
}

fn quad(m: &mut Mesh, x0: f32, y0: f32, x1: f32, y1: f32, z: f32) {
    let b = m.positions.len() as u32;
    m.positions.extend([Vec3::new(x0, y0, z), Vec3::new(x1, y0, z), Vec3::new(x1, y1, z), Vec3::new(x0, y1, z)]);
    m.indices.extend([[b, b + 1, b + 2], [b, b + 2, b + 3]]);
}

fn slab(m: &mut Mesh, x0: f32, y0: f32, x1: f32, y1: f32, z0: f32, z1: f32) {
    let b = m.positions.len() as u32;
    let p = [
        Vec3::new(x0, y0, z0),
        Vec3::new(x1, y0, z0),
        Vec3::new(x1, y1, z0),
        Vec3::new(x0, y1, z0),
        Vec3::new(x0, y0, z1),
        Vec3::new(x1, y0, z1),
        Vec3::new(x1, y1, z1),
        Vec3::new(x0, y1, z1),
    ];
    m.positions.extend(p);
    for [a, c, d, e] in [[0, 1, 5, 4], [1, 2, 6, 5], [2, 3, 7, 6], [3, 0, 4, 7], [3, 2, 1, 0]] {
        m.indices.push([b + a, b + c, b + d]);
        m.indices.push([b + a, b + d, b + e]);
    }
}

/// The two strips of an H2D-style dual-nozzle plate that only one nozzle reaches: (x0, x1, label).
pub fn nozzle_strips(bed: &Bed) -> Vec<(f32, f32, &'static str)> {
    if bed.nozzle_areas.len() < 2 {
        return Vec::new();
    }
    let x0 = bed.nozzle_areas.iter().map(|a| a[0]).fold(0.0f32, f32::max);
    let x1 = bed.nozzle_areas.iter().map(|a| a[2]).fold(bed.size[0], f32::min);
    let mut v = Vec::new();
    if x0 > 0.0 {
        v.push((0.0, x0, "left nozzle only"));
    }
    if x1 < bed.size[0] {
        v.push((x1, bed.size[0], "right nozzle only"));
    }
    v
}

pub fn build_plate(bed: &Bed, ppp: f32, pal: &theme::Palette) -> PlateData {
    let (w, h) = (bed.size[0], bed.size[1]);
    let mut body = Mesh::new();
    slab(&mut body, -4.0, -4.0, w + 4.0, h + 4.0, -5.0, -0.6);
    let mut top = Mesh::new();
    quad(&mut top, 0.0, 0.0, w, h, -0.3);
    let mut strips = Mesh::new();
    let mut lines = Vec::new();
    let line = |a: [f32; 2], b: [f32; 2], z: f32, c: [u8; 3], px: f32| Seg { a: [a[0], a[1], z], b: [b[0], b[1], z], color: [c[0], c[1], c[2], 255], width: -px * ppp };
    for (sx0, sx1, _) in nozzle_strips(bed) {
        quad(&mut strips, sx0, 0.0, sx1, h, -0.2);
        // diagonal hatching
        let step = 7.0;
        let mut c = sx0 - h;
        while c < sx1 {
            let xa = sx0.max(c);
            let xb = sx1.min(c + h);
            if xb > xa {
                lines.push(line([xa, xa - c], [xb, xb - c], -0.12, pal.plate_hatch, 1.0));
            }
            c += step;
        }
        let edge = if sx0 <= 0.0 { sx1 } else { sx0 };
        lines.push(line([edge, 0.0], [edge, h], -0.08, pal.plate_strip_edge, 1.5));
    }
    let mut x = 10.0;
    while x < w - 0.01 {
        let major = (x / 50.0).fract().abs() < 1e-3;
        lines.push(line([x, 0.0], [x, h], -0.1, if major { pal.plate_grid_major } else { pal.plate_grid }, if major { 1.3 } else { 0.9 }));
        x += 10.0;
    }
    let mut y = 10.0;
    while y < h - 0.01 {
        let major = (y / 50.0).fract().abs() < 1e-3;
        lines.push(line([0.0, y], [w, y], -0.1, if major { pal.plate_grid_major } else { pal.plate_grid }, if major { 1.3 } else { 0.9 }));
        y += 10.0;
    }
    for (a, b) in [([0.0, 0.0], [w, 0.0]), ([w, 0.0], [w, h]), ([w, h], [0.0, h]), ([0.0, h], [0.0, 0.0])] {
        lines.push(line(a, b, -0.05, pal.plate_border, 1.6));
    }
    // which way the plate faces, written on it like Bambu Studio does (both read from the front)
    // thin screen-space lines in the grid's colour, so it reads as printed on the plate
    for (text, y) in [("FRONT", 3.0), ("BACK", h - 13.0)] {
        lines.extend(plate_text(text, w * 0.5, y, 10.0).into_iter().map(|(a, b)| line(a, b, -0.09, pal.plate_grid_major, 1.0)));
    }
    let tint = pal.plate_top.iter().chain(pal.plate_grid.iter()).fold(0u64, |a, c| a.wrapping_mul(131).wrapping_add(*c as u64));
    let key = (w.to_bits() as u64) << 32 ^ (h.to_bits() as u64) ^ ((ppp * 100.0) as u64) << 8 ^ (bed.nozzle_areas.len() as u64) ^ tint << 20;
    PlateData {
        key,
        meshes: vec![
            (body, ObjInstance::plain(srgb_to_linear(pal.plate_body), 0)),
            (top, ObjInstance::plain(srgb_to_linear(pal.plate_top), 1)),
            (strips, ObjInstance::plain(srgb_to_linear(pal.plate_strip), 1)),
        ],
        lines,
    }
}

/// Where plate `k` (1-based) of `n` sits in the all-plates view, from plate 1's corner: a grid,
/// left to right, then rows towards the front (like reading a page in the top view).
pub fn plate_origin(k: u32, n: u32, bed: &Bed) -> [f32; 2] {
    let cols = (n.max(1) as f32).sqrt().ceil().max(1.0) as u32;
    let i = k.max(1) - 1;
    let gap = (bed.size[0].max(bed.size[1]) * 0.25).round();
    [(i % cols) as f32 * (bed.size[0] + gap), -((i / cols) as f32) * (bed.size[1] + gap)]
}

/// Plate `k`'s corner relative to the active plate's (the active plate is always drawn at the
/// origin, so every edit, pick and handle works in its own coordinates).
pub fn plate_offset(k: u32, active: u32, n: u32, bed: &Bed) -> [f32; 2] {
    let (a, b) = (plate_origin(k, n, bed), plate_origin(active, n, bed));
    [a[0] - b[0], a[1] - b[1]]
}

/// The active plate plus dimmed copies at `others` (offsets from the active plate's corner).
pub fn build_plates(bed: &Bed, ppp: f32, pal: &theme::Palette, others: &[[f32; 2]]) -> PlateData {
    let mut d = build_plate(bed, ppp, pal);
    if others.is_empty() {
        return d;
    }
    let base_meshes: Vec<(Mesh, ObjInstance)> = d.meshes.clone();
    let base_lines = d.lines.clone();
    let dim = |c: [f32; 4], k: f32| [c[0] * k, c[1] * k, c[2] * k, c[3]];
    for off in others {
        for (m, inst) in &base_meshes {
            let mut m = m.clone();
            m.translate(glam::Vec3::new(off[0], off[1], 0.0));
            let mut i = *inst;
            i.color = dim(i.color, 0.72);
            d.meshes.push((m, i));
        }
        for l in &base_lines {
            let mut l = *l;
            l.a = [l.a[0] + off[0], l.a[1] + off[1], l.a[2]];
            l.b = [l.b[0] + off[0], l.b[1] + off[1], l.b[2]];
            l.color = [(l.color[0] as f32 * 0.7) as u8, (l.color[1] as f32 * 0.7) as u8, (l.color[2] as f32 * 0.7) as u8, l.color[3]];
            d.lines.push(l);
        }
        d.key = d.key.wrapping_mul(1_000_003) ^ ((off[0].to_bits() as u64) << 32 | off[1].to_bits() as u64);
    }
    d
}

/// Strokes of a tiny single-line font (only the letters the plate needs), centred on `cx` with
/// the baseline at `y`, `height` mm tall. Glyphs are drawn on a 4 × 6 grid.
fn plate_text(text: &str, cx: f32, y: f32, height: f32) -> Vec<([f32; 2], [f32; 2])> {
    fn glyph(ch: char) -> &'static [&'static [(f32, f32)]] {
        match ch {
            'F' => &[&[(0.0, 0.0), (0.0, 6.0), (4.0, 6.0)], &[(0.0, 3.0), (3.0, 3.0)]],
            'R' => &[&[(0.0, 0.0), (0.0, 6.0), (3.0, 6.0), (4.0, 5.0), (4.0, 4.0), (3.0, 3.0), (0.0, 3.0)], &[(2.0, 3.0), (4.0, 0.0)]],
            'O' => &[&[(1.0, 0.0), (3.0, 0.0), (4.0, 1.0), (4.0, 5.0), (3.0, 6.0), (1.0, 6.0), (0.0, 5.0), (0.0, 1.0), (1.0, 0.0)]],
            'N' => &[&[(0.0, 0.0), (0.0, 6.0), (4.0, 0.0), (4.0, 6.0)]],
            'T' => &[&[(0.0, 6.0), (4.0, 6.0)], &[(2.0, 6.0), (2.0, 0.0)]],
            'B' => &[&[(0.0, 0.0), (0.0, 6.0), (3.0, 6.0), (4.0, 5.0), (4.0, 4.0), (3.0, 3.0), (0.0, 3.0)], &[(3.0, 3.0), (4.0, 2.0), (4.0, 1.0), (3.0, 0.0), (0.0, 0.0)]],
            'A' => &[&[(0.0, 0.0), (0.0, 4.0), (2.0, 6.0), (4.0, 4.0), (4.0, 0.0)], &[(0.0, 3.0), (4.0, 3.0)]],
            'C' => &[&[(4.0, 5.0), (3.0, 6.0), (1.0, 6.0), (0.0, 5.0), (0.0, 1.0), (1.0, 0.0), (3.0, 0.0), (4.0, 1.0)]],
            'K' => &[&[(0.0, 0.0), (0.0, 6.0)], &[(4.0, 6.0), (0.0, 2.0)], &[(1.3, 3.3), (4.0, 0.0)]],
            _ => &[],
        }
    }
    let u = height / 6.0;
    let n = text.chars().count() as f32;
    let x0 = cx - (n * 6.0 - 2.0) * u * 0.5;
    let mut out = Vec::new();
    for (i, ch) in text.chars().enumerate() {
        let ox = x0 + i as f32 * 6.0 * u;
        for stroke in glyph(ch) {
            for w in stroke.windows(2) {
                out.push(([ox + w[0].0 * u, y + w[0].1 * u], [ox + w[1].0 * u, y + w[1].1 * u]));
            }
        }
    }
    out
}

pub fn feature_index(f: Feature) -> usize {
    Feature::ALL.iter().position(|x| *x == f).unwrap_or(31)
}

/// Flatten a slice into GPU ribbon segments (same data as `only3d_slice::preview_segments`, but kept
/// per layer, top layer first, so a layer range is one contiguous draw).
/// Toolpaths for the preview, coloured by feature, or with `by_filament` (each path's object →
/// filament, and the filaments' colours) in the colour of the filament that prints them. The
/// feature stays in the alpha byte either way, so features can still be hidden.
pub fn build_toolpaths(layers: &[only3d_slice::LayerPlan], by_filament: Option<(&[usize], &[[u8; 3]])>) -> ToolpathData {
    let total: usize = layers.iter().flat_map(|l| l.paths.iter()).map(|p| p.pts.len() + 1).sum();
    let mut segs = Vec::with_capacity(total);
    let mut stored_starts = Vec::with_capacity(layers.len() + 1);
    let layer_z = layers.iter().map(|l| l.z as f32).collect();
    for l in layers.iter().rev() {
        stored_starts.push(segs.len() as u32);
        let z = (l.z - l.height * 0.5) as f32;
        for p in &l.paths {
            if p.pts.len() < 2 {
                continue;
            }
            let c = match by_filament {
                Some((of, colors)) => colors.get(of.get(p.object).copied().unwrap_or(0)).copied().unwrap_or([200, 190, 170]),
                None => p.feature.color(),
            };
            let color = [c[0], c[1], c[2], feature_index(p.feature) as u8];
            let width = (p.width as f32).max(0.1);
            let n = p.pts.len();
            let count = if p.closed { n } else { n - 1 };
            for i in 0..count {
                let a = p.pts[i];
                let b = p.pts[(i + 1) % n];
                segs.push(Seg { a: [a[0] as f32, a[1] as f32, z], b: [b[0] as f32, b[1] as f32, z], color, width });
            }
        }
    }
    stored_starts.push(segs.len() as u32);
    ToolpathData { segs, stored_starts, layer_z }
}

fn ray_aabb(o: Vec3, d: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let inv = Vec3::ONE / d;
    let t0 = (min - o) * inv;
    let t1 = (max - o) * inv;
    let tmin = t0.min(t1).max_element();
    let tmax = t0.max(t1).min_element();
    (tmax >= tmin.max(0.0)).then_some(tmin.max(0.0))
}

#[inline]
fn ray_tri(o: Vec3, d: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
    let e1 = b - a;
    let e2 = c - a;
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = o - a;
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t > 0.0).then_some(t)
}

/// Nearest ray hit against one mesh (parallel brute force; ~ms for 1M triangles).
pub fn ray_mesh(m: &Mesh, o: Vec3, d: Vec3) -> Option<f32> {
    let pos = &m.positions;
    let best = m
        .indices
        .par_chunks(8192)
        .map(|chunk| {
            let mut best = f32::INFINITY;
            for t in chunk {
                if let Some(h) = ray_tri(o, d, pos[t[0] as usize], pos[t[1] as usize], pos[t[2] as usize]) {
                    best = best.min(h);
                }
            }
            best
        })
        .reduce(|| f32::INFINITY, f32::min);
    best.is_finite().then_some(best)
}

/// The nearest triangle a ray hits: (distance, triangle index).
pub fn ray_mesh_tri(m: &Mesh, o: Vec3, d: Vec3) -> Option<(f32, usize)> {
    let pos = &m.positions;
    let best = m
        .indices
        .par_chunks(8192)
        .enumerate()
        .map(|(ci, chunk)| {
            let mut best = (f32::INFINITY, 0usize);
            for (k, t) in chunk.iter().enumerate() {
                if let Some(h) = ray_tri(o, d, pos[t[0] as usize], pos[t[1] as usize], pos[t[2] as usize]) {
                    if h < best.0 {
                        best = (h, ci * 8192 + k);
                    }
                }
            }
            best
        })
        .reduce(|| (f32::INFINITY, 0), |a, b| if a.0 <= b.0 { a } else { b });
    best.0.is_finite().then_some(best)
}

/// Pick the nearest visible object along a ray. Returns (index into `objs`, distance).
pub fn pick(objs: &[EvaluatedObject], o: Vec3, d: Vec3) -> Option<(usize, f32)> {
    let mut cands: Vec<(usize, f32)> = objs
        .iter()
        .enumerate()
        .filter(|(_, ob)| !ob.hidden)
        .filter_map(|(i, ob)| ray_aabb(o, d, ob.report.bounds.min - Vec3::splat(0.01), ob.report.bounds.max + Vec3::splat(0.01)).map(|t| (i, t)))
        .collect();
    cands.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut best: Option<(usize, f32)> = None;
    for (i, tbox) in cands {
        if best.is_some_and(|b| b.1 < tbox) {
            break;
        }
        if let Some(t) = ray_mesh(&objs[i].mesh, o, d) {
            if best.is_none_or(|b| t < b.1) {
                best = Some((i, t));
            }
        }
    }
    best
}

/// Intersect a ray with the plane z = `z`.
pub fn ray_plane_z(o: Vec3, d: Vec3, z: f32) -> Option<Vec3> {
    if d.z.abs() < 1e-6 {
        return None;
    }
    let t = (z - o.z) / d.z;
    (t > 0.0).then(|| o + d * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plates_lay_out_in_a_grid_around_the_active_one() {
        let bed = Bed::h2d();
        // one plate: nothing moves
        assert_eq!(plate_origin(1, 1, &bed), [0.0, 0.0]);
        // four plates: two columns, the second row in front (−Y)
        let (w, d, gap) = (350.0, 320.0, 88.0);
        assert_eq!(plate_origin(2, 4, &bed), [w + gap, 0.0]);
        assert_eq!(plate_origin(3, 4, &bed), [0.0, -(d + gap)]);
        assert_eq!(plate_origin(4, 4, &bed), [w + gap, -(d + gap)]);
        // offsets are from the active plate, which stays at the origin
        assert_eq!(plate_offset(3, 3, 4, &bed), [0.0, 0.0]);
        assert_eq!(plate_offset(1, 3, 4, &bed), [0.0, d + gap]);
        // copies of the plate geometry go where the other plates are
        let pal = theme::DARK;
        let one = build_plates(&bed, 2.0, &pal, &[]);
        let three = build_plates(&bed, 2.0, &pal, &[[w + gap, 0.0], [0.0, -(d + gap)]]);
        assert_eq!(three.meshes.len(), one.meshes.len() * 3);
        assert_eq!(three.lines.len(), one.lines.len() * 3);
        assert_ne!(one.key, three.key);
        let b = three.meshes[one.meshes.len()].0.bounds();
        assert!(b.min.x > w, "{b:?}");
    }
}
