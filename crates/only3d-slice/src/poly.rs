//! 2D polygon helpers on top of `i_overlay` (pure Rust, robust booleans + offsets).

use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::simplify::SimplifyShape;
use i_overlay::float::single::SingleFloatOverlay;
use i_overlay::mesh::outline::offset::OutlineOffset;
use i_overlay::mesh::style::{LineJoin, OutlineStyle};

pub type P = [f64; 2];
pub type Contour = Vec<P>;
/// Outer contour (CCW) followed by holes (CW).
pub type Shape = Vec<Contour>;
pub type Region = Vec<Shape>;

/// Clean arbitrary (oriented, possibly overlapping) loops into a region using the non-zero rule.
pub fn from_loops(loops: &[Contour]) -> Region {
    if loops.is_empty() {
        return Vec::new();
    }
    let loops: Vec<Contour> = loops.iter().filter(|l| l.len() >= 3).cloned().collect();
    if loops.is_empty() {
        return Vec::new();
    }
    loops.simplify_shape(FillRule::NonZero)
}

/// Grow (d > 0) or shrink (d < 0) a region with mitred corners.
pub fn offset(r: &Region, d: f64) -> Region {
    if r.is_empty() {
        return Vec::new();
    }
    if d.abs() < 1e-9 {
        return r.clone();
    }
    let style = OutlineStyle::new(d).line_join(LineJoin::Miter(0.3));
    r.outline(&style)
}

pub fn offset_shape(s: &Shape, d: f64) -> Region {
    if d.abs() < 1e-9 {
        return vec![s.clone()];
    }
    let style = OutlineStyle::new(d).line_join(LineJoin::Miter(0.3));
    s.outline(&style)
}

pub fn difference(a: &Region, b: &Region) -> Region {
    if a.is_empty() || b.is_empty() {
        return a.clone();
    }
    a.overlay(b, OverlayRule::Difference, FillRule::NonZero)
}

/// The parts of closed loops `loops` outside `keepout`, as open polylines.
pub fn clip_loops_outside(loops: &[Contour], keepout: &Region) -> Vec<Contour> {
    use i_overlay::float::clip::FloatClip;
    use i_overlay::string::clip::ClipRule;
    let rule = ClipRule { invert: true, boundary_included: false };
    let mut out = Vec::new();
    for l in loops.iter().filter(|l| l.len() >= 2) {
        let mut open = l.clone();
        open.push(l[0]);
        out.extend(open.clip_by(keepout, FillRule::NonZero, rule).into_iter().filter(|p| p.len() >= 2));
    }
    out
}

pub fn intersection(a: &Region, b: &Region) -> Region {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    a.overlay(b, OverlayRule::Intersect, FillRule::NonZero)
}

pub fn union(a: &Region, b: &Region) -> Region {
    if a.is_empty() {
        return b.clone();
    }
    if b.is_empty() {
        return a.clone();
    }
    a.overlay(b, OverlayRule::Union, FillRule::NonZero)
}

pub fn contour_area(c: &Contour) -> f64 {
    let mut a = 0.0;
    for i in 0..c.len() {
        let (p, q) = (c[i], c[(i + 1) % c.len()]);
        a += p[0] * q[1] - q[0] * p[1];
    }
    a * 0.5
}

/// Material area of a shape: its outer contour minus its holes.
pub fn shape_area(s: &Shape) -> f64 {
    let mut areas = s.iter().map(|c| contour_area(c).abs());
    let outer = areas.next().unwrap_or(0.0);
    outer - areas.sum::<f64>()
}

pub fn region_area(r: &Region) -> f64 {
    r.iter().flat_map(|s| s.iter()).map(contour_area).sum()
}

/// Drop shapes with less than `min_area` mm² of material (slivers that would only make noise).
/// Holes count: a thin ring around a large outline is small.
pub fn drop_small(r: Region, min_area: f64) -> Region {
    r.into_iter().filter(|s| shape_area(s) >= min_area).collect()
}

#[inline]
pub fn dist(a: P, b: P) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// Straight-line hatch of a region: lines at `angle_deg`, `spacing` apart, aligned to a global
/// grid (so sparse infill stacks layer to layer). Returns segments ordered boustrophedon-style.
pub fn hatch(r: &Region, angle_deg: f64, spacing: f64, phase: f64) -> Vec<[P; 2]> {
    if r.is_empty() || spacing <= 0.0 {
        return Vec::new();
    }
    let (s, c) = angle_deg.to_radians().sin_cos();
    // rotate by -angle so hatch lines become horizontal
    let rot = |p: P| -> P { [p[0] * c + p[1] * s, -p[0] * s + p[1] * c] };
    let unrot = |p: P| -> P { [p[0] * c - p[1] * s, p[0] * s + p[1] * c] };
    let mut edges: Vec<(P, P)> = Vec::new();
    let (mut ymin, mut ymax) = (f64::INFINITY, f64::NEG_INFINITY);
    for shape in r {
        for cont in shape {
            for i in 0..cont.len() {
                let (a, b) = (rot(cont[i]), rot(cont[(i + 1) % cont.len()]));
                ymin = ymin.min(a[1]);
                ymax = ymax.max(a[1]);
                if (a[1] - b[1]).abs() > 1e-12 {
                    edges.push((a, b));
                }
            }
        }
    }
    let k0 = ((ymin - phase) / spacing).ceil() as i64;
    let k1 = ((ymax - phase) / spacing).floor() as i64;
    let mut rows: Vec<Vec<[P; 2]>> = Vec::new();
    let mut xs: Vec<f64> = Vec::new();
    for k in k0..=k1 {
        let y = phase + k as f64 * spacing;
        xs.clear();
        for (a, b) in &edges {
            let (lo, hi) = if a[1] < b[1] { (a, b) } else { (b, a) };
            if y >= lo[1] && y < hi[1] {
                let t = (y - lo[1]) / (hi[1] - lo[1]);
                xs.push(lo[0] + t * (hi[0] - lo[0]));
            }
        }
        xs.sort_by(|a, b| a.total_cmp(b));
        let mut row = Vec::new();
        for pair in xs.chunks_exact(2) {
            if pair[1] - pair[0] > 1e-3 {
                row.push([unrot([pair[0], y]), unrot([pair[1], y])]);
            }
        }
        if !row.is_empty() {
            rows.push(row);
        }
    }
    // boustrophedon: reverse every other row
    let mut out = Vec::new();
    for (i, mut row) in rows.into_iter().enumerate() {
        if i % 2 == 1 {
            row.reverse();
            for seg in &mut row {
                seg.swap(0, 1);
            }
        }
        out.extend(row);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x0: f64, y0: f64, s: f64) -> Contour {
        vec![[x0, y0], [x0 + s, y0], [x0 + s, y0 + s], [x0, y0 + s]]
    }

    #[test]
    fn offsets_and_booleans() {
        let r = from_loops(&[square(0.0, 0.0, 10.0)]);
        assert!((region_area(&r) - 100.0).abs() < 1e-6);
        let shrunk = offset(&r, -1.0);
        assert!((region_area(&shrunk) - 64.0).abs() < 1e-3, "{}", region_area(&shrunk));
        let grown = offset(&r, 1.0);
        assert!(region_area(&grown) > 100.0);
        let d = difference(&r, &from_loops(&[square(2.0, 2.0, 2.0)]));
        assert!((region_area(&d) - 96.0).abs() < 1e-6);
        // hole loop given clockwise is subtracted by non-zero filling
        let mut hole = square(4.0, 4.0, 2.0);
        hole.reverse();
        let with_hole = from_loops(&[square(0.0, 0.0, 10.0), hole]);
        assert!((region_area(&with_hole) - 96.0).abs() < 1e-6);
        let lines = hatch(&r, 0.0, 1.0, 0.5);
        assert_eq!(lines.len(), 10);
    }

    #[test]
    fn drop_small_counts_holes() {
        // a 0.1 mm ring around a 10 mm square: big outline, ~4 mm² of material
        let mut hole = square(0.1, 0.1, 9.8);
        hole.reverse();
        let ring = from_loops(&[square(0.0, 0.0, 10.0), hole]);
        assert!((shape_area(&ring[0]) - 3.96).abs() < 1e-6);
        assert!(drop_small(ring.clone(), 5.0).is_empty());
        assert_eq!(drop_small(ring, 3.0).len(), 1);
    }
}
