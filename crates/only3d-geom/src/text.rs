//! Text as outlines: glyphs from the bundled fonts (Ubuntu Light for "sans", Hack for "mono",
//! both openly licensed) flattened into polygons, laid out left to right, lines with `\n`.

use glam::Vec2;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, MetadataProvider};

use crate::{GeomError, Result};

/// Flattens glyph curves into polylines (a few segments per curve, enough at print scale).
struct Flatten {
    contours: Vec<Vec<Vec2>>,
    cur: Vec<Vec2>,
    at: Vec2,
    offset: Vec2,
    tol_steps: usize,
}

impl Flatten {
    fn push(&mut self, p: Vec2) {
        let p = p + self.offset;
        if self.cur.last().is_none_or(|q| q.distance(p) > 1e-4) {
            self.cur.push(p);
        }
    }
}

impl OutlinePen for Flatten {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        self.at = Vec2::new(x, y);
        self.push(self.at);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.at = Vec2::new(x, y);
        self.push(self.at);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let (p0, c, p1) = (self.at, Vec2::new(cx, cy), Vec2::new(x, y));
        for k in 1..=self.tol_steps {
            let t = k as f32 / self.tol_steps as f32;
            self.push(p0 * (1.0 - t) * (1.0 - t) + c * 2.0 * t * (1.0 - t) + p1 * t * t);
        }
        self.at = p1;
    }
    fn curve_to(&mut self, c0x: f32, c0y: f32, c1x: f32, c1y: f32, x: f32, y: f32) {
        let (p0, c0, c1, p1) = (self.at, Vec2::new(c0x, c0y), Vec2::new(c1x, c1y), Vec2::new(x, y));
        for k in 1..=self.tol_steps {
            let t = k as f32 / self.tol_steps as f32;
            let u = 1.0 - t;
            self.push(p0 * u * u * u + c0 * 3.0 * u * u * t + c1 * 3.0 * u * t * t + p1 * t * t * t);
        }
        self.at = p1;
    }
    fn close(&mut self) {
        if self.cur.len() > 1 && self.cur[0].distance(*self.cur.last().unwrap()) < 1e-4 {
            self.cur.pop();
        }
        if self.cur.len() >= 3 {
            self.contours.push(std::mem::take(&mut self.cur));
        } else {
            self.cur.clear();
        }
    }
}

fn font_bytes(font: &str) -> Result<&'static [u8]> {
    match font {
        "" | "sans" => Ok(epaint_default_fonts::UBUNTU_LIGHT),
        "mono" => Ok(epaint_default_fonts::HACK_REGULAR),
        other => Err(GeomError::InvalidShape(format!("unknown font '{other}' (sans or mono)"))),
    }
}

/// Glyph contours for `text` with capital letters about `size` mm tall, centred on the origin.
/// Outer contours and holes come mixed; tell them apart by nesting (see [`classify`]).
pub fn outlines(text: &str, size: f32, font: &str) -> Result<Vec<Vec<Vec2>>> {
    if text.trim().is_empty() {
        return Err(GeomError::InvalidShape("text is empty".into()));
    }
    if size.is_nan() || size <= 0.1 {
        return Err(GeomError::InvalidShape("text size must be > 0.1 mm".into()));
    }
    let f = FontRef::new(font_bytes(font)?).map_err(|e| GeomError::InvalidShape(format!("font: {e}")))?;
    // scale so a capital H is `size` mm tall
    let probe = Size::new(100.0);
    let cap = {
        let mut pen = Flatten { contours: Vec::new(), cur: Vec::new(), at: Vec2::ZERO, offset: Vec2::ZERO, tol_steps: 4 };
        if let Some(g) = f.charmap().map('H').and_then(|id| f.outline_glyphs().get(id)) {
            let _ = g.draw(DrawSettings::unhinted(probe, LocationRef::default()), &mut pen);
            pen.close();
        }
        pen.contours.iter().flatten().map(|p| p.y).fold(0.0f32, f32::max).max(1.0)
    };
    let em = 100.0 * size / cap;
    let sz = Size::new(em);
    let glyphs = f.outline_glyphs();
    let charmap = f.charmap();
    let metrics = f.glyph_metrics(sz, LocationRef::default());
    let line_h = em * 1.25;
    let mut pen = Flatten { contours: Vec::new(), cur: Vec::new(), at: Vec2::ZERO, offset: Vec2::ZERO, tol_steps: 6 };
    let mut line_widths = Vec::new();
    let mut line_starts = Vec::new();
    for (li, line) in text.split('\n').enumerate() {
        line_starts.push(pen.contours.len());
        let mut x = 0.0;
        for ch in line.chars() {
            let gid = charmap.map(ch).or_else(|| charmap.map('?')).unwrap_or_default();
            pen.offset = Vec2::new(x, -(li as f32) * line_h);
            if let Some(g) = glyphs.get(gid) {
                let _ = g.draw(DrawSettings::unhinted(sz, LocationRef::default()), &mut pen);
                pen.close();
            }
            x += metrics.advance_width(gid).unwrap_or(em * 0.5);
        }
        line_widths.push(x);
    }
    line_starts.push(pen.contours.len());
    // centre each line, then the block
    let mut contours = pen.contours;
    for li in 0..line_widths.len() {
        let dx = -line_widths[li] / 2.0;
        for c in &mut contours[line_starts[li]..line_starts[li + 1]] {
            for p in c.iter_mut() {
                p.x += dx;
            }
        }
    }
    let (mut lo, mut hi) = (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY));
    for p in contours.iter().flatten() {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }
    if !lo.x.is_finite() {
        return Err(GeomError::InvalidShape("the text has no visible glyphs".into()));
    }
    let c = (lo + hi) * 0.5;
    for p in contours.iter_mut().flatten() {
        *p -= c;
    }
    Ok(contours)
}

/// Split contours into solids and holes: a contour inside an odd number of others is a hole.
/// Every contour comes back counter-clockwise; holes carry the index of their solid.
pub fn classify(contours: Vec<Vec<Vec2>>) -> (Vec<Vec<Vec2>>, Vec<(usize, Vec<Vec2>)>) {
    let inside = |p: Vec2, poly: &[Vec2]| {
        let mut c = false;
        let n = poly.len();
        for i in 0..n {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
                c = !c;
            }
        }
        c
    };
    let ccw = |mut c: Vec<Vec2>| {
        if crate::shapes::signed_area(&c) < 0.0 {
            c.reverse();
        }
        c
    };
    let depth: Vec<usize> = (0..contours.len()).map(|i| (0..contours.len()).filter(|j| *j != i && inside(contours[i][0], &contours[*j])).count()).collect();
    let mut solids = Vec::new();
    let mut solid_of = vec![usize::MAX; contours.len()];
    for (i, c) in contours.iter().enumerate() {
        if depth[i].is_multiple_of(2) {
            solid_of[i] = solids.len();
            solids.push(ccw(c.clone()));
        }
    }
    let mut holes = Vec::new();
    for (i, c) in contours.iter().enumerate() {
        if !depth[i].is_multiple_of(2) {
            // its solid: the smallest even-depth contour around it
            let owner = (0..contours.len())
                .filter(|j| depth[*j].is_multiple_of(2) && *j != i && inside(c[0], &contours[*j]))
                .min_by(|a, b| crate::shapes::signed_area(&contours[*a]).abs().total_cmp(&crate::shapes::signed_area(&contours[*b]).abs()));
            if let Some(o) = owner {
                holes.push((solid_of[o], ccw(c.clone())));
            }
        }
    }
    (solids, holes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_have_their_holes() {
        let c = outlines("OAK", 10.0, "sans").unwrap();
        let (solids, holes) = classify(c);
        assert_eq!(solids.len(), 3, "O, A and K");
        assert_eq!(holes.len(), 2, "the O's and the A's counters");
        let h = solids.iter().flatten().map(|p| p.y).fold(f32::MIN, f32::max) - solids.iter().flatten().map(|p| p.y).fold(f32::MAX, f32::min);
        assert!((9.5..11.5).contains(&h), "caps about 10 mm (O overshoots a little): {h}");
        assert!(outlines("hi\nthere", 5.0, "mono").is_ok());
        // every glyph of a typical label extrudes closed (flattened curves leave near-straight
        // runs that must not break the caps)
        for t in ["0123456789", "V2", "Ä é ß", "M3 x 12 (40%)"] {
            let (solids, _) = classify(outlines(t, 5.0, "sans").unwrap());
            for c in &solids {
                assert!(crate::shapes::extrude(c, 1.0).unwrap().analyze().watertight, "{t}");
            }
        }
        assert!(outlines("  ", 5.0, "sans").is_err());
    }
}

