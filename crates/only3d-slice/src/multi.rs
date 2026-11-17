//! Several filaments on one plate: the order filaments print in on each layer (as few changes
//! as possible, carrying the loaded filament across layers, like Bambu Studio's tool ordering)
//! and the prime tower, where the freshly loaded filament primes after every change.
//!
//! The tower here is simpler than Bambu Studio's (no rib walls or ironing): a rectangle in the
//! area both nozzles reach, two walls, a dense first layer with a brim, after each change a
//! block of rows the new filament primes into, and sparse rows on the other layers so the
//! tower stays whole up to the last change. On the H2D the purge itself goes down the chute
//! (the firmware flushes as part of the change); the tower only takes the priming.

use serde::Serialize;

use crate::planner::{Feature, LayerPlan, Path};
use crate::settings::SliceSettings;

/// Where the tower goes and how much each change primes on it.
#[derive(Clone, Debug, Serialize)]
pub struct TowerSpec {
    /// Preferred front-left corner (it moves if objects are in the way).
    pub x: f64,
    pub y: f64,
    /// Along X (mm).
    pub width: f64,
    pub line_width: f64,
    /// Primed after a change on the same nozzle / after a nozzle switch (mm³).
    pub prime_mm3: f64,
    pub prime_nozzle_change_mm3: f64,
    /// Area the tower must stay in [x0, y0, x1, y1] (both nozzles reach it).
    pub area: [f64; 4],
    pub speed: f64,
}

/// The tower as placed.
#[derive(Clone, Debug, Serialize)]
pub struct TowerInfo {
    pub min: [f64; 2],
    pub max: [f64; 2],
    /// Layers the tower is printed on (0..=last change layer).
    pub layers: usize,
}

/// One filament change: on `layer`, before path `at` of that layer's (reordered) paths.
#[derive(Clone, Debug, Serialize)]
pub struct Change {
    pub layer: usize,
    pub at: usize,
    pub from: Option<usize>,
    pub to: usize,
}

/// Group each layer's paths by filament and order the groups so the filament that is loaded
/// keeps printing first. Returns the filament order per layer (the filament of path `i` is
/// `filament_of(path.object)`). `start` is the filament loaded when printing begins (None =
/// whichever the first layer prints first).
pub fn order_layers(layers: &mut [LayerPlan], filament_of: &dyn Fn(usize) -> usize, start: Option<usize>) -> Vec<Vec<usize>> {
    let mut cur = start;
    let mut orders = Vec::with_capacity(layers.len());
    for l in layers.iter_mut() {
        let mut used: Vec<usize> = Vec::new();
        for p in &l.paths {
            let f = filament_of(p.object);
            if !used.contains(&f) {
                used.push(f);
            }
        }
        // the loaded filament first, the rest in the order they first appear (the first
        // layer starts with whatever prints its first path, usually the brim's)
        let mut order: Vec<usize> = Vec::new();
        if let Some(c) = cur.filter(|c| used.contains(c)) {
            order.push(c);
        }
        for f in used {
            if !order.contains(&f) {
                order.push(f);
            }
        }
        if order.len() > 1 {
            let mut paths = std::mem::take(&mut l.paths);
            for f in &order {
                let (mine, rest): (Vec<Path>, Vec<Path>) = paths.into_iter().partition(|p| filament_of(p.object) == *f);
                l.paths.extend(mine);
                paths = rest;
            }
        }
        if let Some(last) = order.last() {
            cur = Some(*last);
        }
        orders.push(order);
    }
    orders
}

/// The changes the orders imply (including one at the very start when `start` differs).
pub fn changes(layers: &[LayerPlan], filament_of: &dyn Fn(usize) -> usize, start: Option<usize>) -> Vec<Change> {
    let mut cur = start;
    let mut out = Vec::new();
    for (li, l) in layers.iter().enumerate() {
        for (pi, p) in l.paths.iter().enumerate() {
            let f = filament_of(p.object);
            if Some(f) != cur {
                if cur.is_some() {
                    out.push(Change { layer: li, at: pi, from: cur, to: f });
                }
                cur = Some(f);
            }
        }
    }
    out
}

/// Rows `rows` (indices from the front, ascending) of the tower as one zig-zag line, each row
/// joined to the next along the tower's side, as Bambu Studio prints its tower.
fn rows_paths(rows: &[usize], min: [f64; 2], max: [f64; 2], w: f64, speed: f64, object: usize) -> Vec<Path> {
    if rows.is_empty() {
        return Vec::new();
    }
    let mut pts = Vec::with_capacity(rows.len() * 2);
    for (k, r) in rows.iter().enumerate() {
        let y = min[1] + w * (*r as f64 + 0.5);
        if k % 2 == 0 {
            pts.push([min[0], y]);
            pts.push([max[0], y]);
        } else {
            pts.push([max[0], y]);
            pts.push([min[0], y]);
        }
    }
    vec![Path { feature: Feature::PrimeTower, pts, closed: false, width: w, flow: 1.0, speed, object }]
}

/// Rows the old nozzle wipes on before a switch (Bambu Studio's NOZZLE_CHANGE block).
const WIPE_ROWS: usize = 2;

fn loop_path(min: [f64; 2], max: [f64; 2], w: f64, speed: f64, object: usize) -> Path {
    Path { feature: Feature::PrimeTower, pts: vec![[min[0], min[1]], [max[0], min[1]], [max[0], max[1]], [min[0], max[1]]], closed: true, width: w, flow: 1.0, speed, object }
}

/// Place the tower and add its paths to the layers, after the changes in `orders`. Tower paths
/// get object index `tower_object(filament)`. `obstacles` are the objects' footprints
/// [x0, y0, x1, y1] (brim included). `nozzle_switch(a, b)` says whether going from filament a
/// to b switches nozzle (primes more). Returns None when no layer changes filament.
#[allow(clippy::too_many_arguments)]
pub fn build_tower(
    layers: &mut [LayerPlan],
    filament_of: &dyn Fn(usize) -> usize,
    tower_object: &dyn Fn(usize) -> usize,
    start: Option<usize>,
    spec: &TowerSpec,
    obstacles: &[[f64; 4]],
    nozzle_switch: &dyn Fn(usize, usize) -> bool,
    first: &SliceSettings,
) -> Option<TowerInfo> {
    let ch = changes(layers, filament_of, start);
    let last = ch.iter().map(|c| c.layer).max()?;
    let w = spec.line_width;
    let walls = 2.0;
    let inner_w = spec.width - 2.0 * walls * w;
    // rows a change needs: the new filament's priming, and before a nozzle switch a few rows
    // the old nozzle wipes itself clean on
    let switch = |c: &Change| c.from.is_some_and(|a| nozzle_switch(a, c.to));
    let prime_rows = |c: &Change, h: f64| -> usize {
        let v = if switch(c) { spec.prime_nozzle_change_mm3 } else { spec.prime_mm3 };
        (v / (inner_w * w * h)).ceil().max(2.0) as usize
    };
    let rows_for = |c: &Change, h: f64| -> usize { prime_rows(c, h) + if switch(c) { WIPE_ROWS } else { 0 } };
    let mut need = 8usize;
    for li in 0..=last {
        let h = layers[li].height;
        let n: usize = ch.iter().filter(|c| c.layer == li).map(|c| rows_for(c, h)).sum();
        need = need.max(n);
    }
    let depth = need as f64 * w + 2.0 * walls * w;
    // place it: the preferred spot, else the nearest free one inside the area
    let brim = 3.0;
    let fits = |x: f64, y: f64| -> bool {
        let r = [x - brim, y - brim, x + spec.width + brim, y + depth + brim];
        r[0] >= spec.area[0] && r[1] >= spec.area[1] && r[2] <= spec.area[2] && r[3] <= spec.area[3] && obstacles.iter().all(|o| r[0] >= o[2] + 3.0 || r[2] <= o[0] - 3.0 || r[1] >= o[3] + 3.0 || r[3] <= o[1] - 3.0)
    };
    let mut best: Option<(f64, [f64; 2])> = None;
    let mut y = spec.area[1] + brim;
    while y + depth + brim <= spec.area[3] {
        let mut x = spec.area[0] + brim;
        while x + spec.width + brim <= spec.area[2] {
            if fits(x, y) {
                let d = (x - spec.x).hypot(y - spec.y);
                if best.is_none_or(|b| d < b.0) {
                    best = Some((d, [x, y]));
                }
            }
            x += 5.0;
        }
        y += 5.0;
    }
    let (_, at) = best?;
    let min = [at[0], at[1]];
    let max = [at[0] + spec.width, at[1] + depth];
    let inner_min = [min[0] + walls * w, min[1] + walls * w];
    let inner_max = [max[0] - walls * w, max[1] - walls * w];
    let rows_total = need;

    for li in 0..=last {
        let h = layers[li].height;
        let speed = if li == 0 { first.first_layer_speed.min(spec.speed) } else { spec.speed };
        let speed = speed.min(first.max_volumetric_speed / SliceSettings::bead_area(w, h)).max(5.0);
        let mut used_rows = 0usize;
        // (insert position, paths), applied back to front so positions stay valid
        let mut inserts: Vec<(usize, Vec<Path>)> = Vec::new();
        let layer_changes: Vec<&Change> = ch.iter().filter(|c| c.layer == li).collect();
        for c in &layer_changes {
            let mut paths = Vec::new();
            if let (true, Some(from)) = (switch(c), c.from) {
                let n = WIPE_ROWS.min(rows_total.saturating_sub(used_rows));
                let rows: Vec<usize> = (used_rows..used_rows + n).collect();
                used_rows += n;
                paths.extend(rows_paths(&rows, inner_min, inner_max, w, speed, tower_object(from)));
            }
            let n = prime_rows(c, h).min(rows_total.saturating_sub(used_rows));
            let rows: Vec<usize> = (used_rows..used_rows + n).collect();
            used_rows += n;
            paths.extend(rows_paths(&rows, inner_min, inner_max, w, speed, tower_object(c.to)));
            inserts.push((c.at, paths));
        }
        // the rest of the layer: dense on the first layer, sparse above, and the walls, by the
        // last filament to change here (or, on a layer without changes, the one loaded)
        let rest: Vec<usize> = (used_rows..rows_total).filter(|r| li == 0 || (r - used_rows) % 3 == 1).collect();
        let (at, f) = match layer_changes.last() {
            Some(c) => (c.at, c.to),
            None => (layers[li].paths.len(), layers[li].paths.last().map(|p| filament_of(p.object)).unwrap_or(start.unwrap_or(0))),
        };
        let o = tower_object(f);
        let mut finish = rows_paths(&rest, inner_min, inner_max, w, speed, o);
        for k in 0..walls as usize {
            let d = (k as f64 + 0.5) * w;
            finish.push(loop_path([min[0] + d, min[1] + d], [max[0] - d, max[1] - d], w, speed, o));
        }
        if li == 0 {
            // a brim around the tower, outermost first
            let loops = (brim / w).floor() as usize;
            let mut b: Vec<Path> = (0..loops).rev().map(|k| {
                let d = (k as f64 + 0.5) * w;
                let mut p = loop_path([min[0] - d, min[1] - d], [max[0] + d, max[1] + d], w, speed, o);
                p.feature = Feature::Brim;
                p
            }).collect();
            b.append(&mut finish);
            finish = b;
        }
        match inserts.iter_mut().find(|(a, _)| *a == at) {
            Some((_, v)) if layer_changes.last().is_some() => v.extend(finish),
            _ => inserts.push((at, finish)),
        }
        inserts.sort_by_key(|(a, _)| std::cmp::Reverse(*a));
        for (a, paths) in inserts {
            let a = a.min(layers[li].paths.len());
            layers[li].paths.splice(a..a, paths);
        }
    }
    Some(TowerInfo { min, max, layers: last + 1 })
}

/// Purge volume (mm³) for changing from colour `a` to `b` in the same nozzle, after Bambu
/// Studio's flush-volume estimate: more for light after dark and across hues, never less
/// than the minimum. "#RRGGBB" (alpha ignored).
pub fn flush_volume(a: &str, b: &str) -> f64 {
    let rgb = |c: &str| -> [f64; 3] {
        let h = c.trim_start_matches('#');
        let p = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("80"), 16).unwrap_or(128) as f64 / 255.0;
        [p(0), p(2), p(4)]
    };
    let (s, d) = (rgb(a), rgb(b));
    let lum = |c: [f64; 3]| 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2];
    let hsv = |c: [f64; 3]| -> (f64, f64, f64) {
        let mx = c[0].max(c[1]).max(c[2]);
        let mn = c[0].min(c[1]).min(c[2]);
        let d = mx - mn;
        let h = if d < 1e-9 {
            0.0
        } else if mx == c[0] {
            60.0 * (((c[1] - c[2]) / d).rem_euclid(6.0))
        } else if mx == c[1] {
            60.0 * ((c[2] - c[0]) / d + 2.0)
        } else {
            60.0 * ((c[0] - c[1]) / d + 4.0)
        };
        (h, if mx < 1e-9 { 0.0 } else { d / mx }, mx)
    };
    let (h1, s1, v1) = hsv(s);
    let (h2, s2, v2) = hsv(d);
    // hue/saturation distance on the colour disc
    let (x1, y1) = (s1 * h1.to_radians().cos(), s1 * h1.to_radians().sin());
    let (x2, y2) = (s2 * h2.to_radians().cos(), s2 * h2.to_radians().sin());
    let hs = ((x1 - x2).powi(2) + (y1 - y2).powi(2)).sqrt().min(0.67 * v2 + 0.33 * v1);
    let (l1, l2) = (lum(s), lum(d));
    let lumi = if l2 >= l1 { (l2 - l1).powf(0.7) * 560.0 } else { (l1 - l2) * 80.0 };
    let hsf = 230.0 * hs;
    // third side of a triangle with a 120° angle between the two contributions
    let v = (hsf * hsf + lumi * lumi + hsf * lumi).sqrt().max(60.0);
    // × 1.1 matches Bambu Studio 2.09's H2D values (white ↔ pink PETG: 192 / 330 mm³)
    ((v + 107.0) * 1.1).min(800.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(object: usize, x: f64) -> Path {
        Path { feature: Feature::OuterWall, pts: vec![[x, 10.0], [x + 5.0, 10.0]], closed: false, width: 0.45, flow: 1.0, speed: 100.0, object }
    }

    fn layer(i: usize, paths: Vec<Path>) -> LayerPlan {
        LayerPlan { index: i, z: 0.2 * (i + 1) as f64, height: 0.2, paths, objects: vec![] }
    }

    #[test]
    fn layers_keep_the_loaded_filament_first() {
        // bodies 0 and 1 in filament 0 and 1, both on every layer
        let fil = |o: usize| o;
        let mut layers: Vec<LayerPlan> = (0..4).map(|i| layer(i, vec![p(0, 10.0), p(1, 50.0), p(0, 20.0)])).collect();
        let orders = order_layers(&mut layers, &fil, None);
        assert_eq!(orders, vec![vec![0, 1], vec![1, 0], vec![0, 1], vec![1, 0]]);
        // one change per layer, none between layers
        let ch = changes(&layers, &fil, None);
        assert_eq!(ch.len(), 4);
        assert_eq!((ch[0].layer, ch[0].at, ch[0].from, ch[0].to), (0, 2, Some(0), 1));
        assert_eq!((ch[1].layer, ch[1].at, ch[1].to), (1, 1, 0));
        // grouped: both filament-0 paths of layer 0 come before the filament-1 path
        assert_eq!(layers[0].paths.iter().map(|p| p.object).collect::<Vec<_>>(), vec![0, 0, 1]);
    }

    #[test]
    fn the_tower_takes_each_change_and_stays_whole_up_to_the_last() {
        let fil = |o: usize| if o >= 10 { o - 10 } else { o };
        let tower = |f: usize| 10 + f;
        // filament 1 only on the first three layers; five layers in all
        let mut layers: Vec<LayerPlan> = (0..5).map(|i| layer(i, if i < 3 { vec![p(0, 10.0), p(1, 50.0)] } else { vec![p(0, 10.0)] })).collect();
        order_layers(&mut layers, &fil, None);
        let spec = TowerSpec { x: 160.0, y: 20.0, width: 35.0, line_width: 0.5, prime_mm3: 30.0, prime_nozzle_change_mm3: 60.0, area: [25.0, 0.0, 325.0, 320.0], speed: 90.0 };
        let s = SliceSettings::default();
        let t = build_tower(&mut layers, &fil, &tower, None, &spec, &[[0.0, 0.0, 70.0, 70.0]], &|_, _| true, &s).unwrap();
        // changes on layers 0, 1, 2 (to 1, back to 0, to 1) and one more on layer 3 (back to 0)
        assert_eq!(t.layers, 4);
        assert!(t.min[0] >= 25.0 && (t.max[0] - t.min[0] - 35.0).abs() < 1e-9, "{t:?}");
        let mut loaded: Option<usize> = None;
        for (li, l) in layers.iter().enumerate() {
            let tower_paths = l.paths.iter().filter(|p| p.feature == Feature::PrimeTower).count();
            if li < 4 {
                assert!(tower_paths > 0, "layer {li} has a tower");
                // a tower path belongs to the filament printing just before it (the old nozzle
                // wiping before a switch, or the end of a layer without a change) or just after
                // it (the new filament priming)
                for (i, p) in l.paths.iter().enumerate().filter(|(_, p)| p.object >= 10) {
                    let next = l.paths[i..].iter().find(|q| q.object < 10).map(|q| fil(q.object));
                    let prev = l.paths[..i].iter().rev().find(|q| q.object < 10).map(|q| fil(q.object)).or(loaded);
                    assert!(Some(fil(p.object)) == next || Some(fil(p.object)) == prev, "layer {li}: tower path {i}");
                }
                // and the whole layer, tower included, changes filament as few times as needed
                let mut seq: Vec<usize> = l.paths.iter().map(|p| fil(p.object)).collect();
                seq.dedup();
                let mut uniq = seq.clone();
                uniq.sort_unstable();
                uniq.dedup();
                assert_eq!(seq.len(), uniq.len(), "layer {li}: {seq:?}");
                // the object paths are still grouped: one run per filament
                let mut runs: Vec<usize> = Vec::new();
                for p in l.paths.iter().filter(|p| p.object < 10) {
                    if runs.last() != Some(&fil(p.object)) {
                        runs.push(fil(p.object));
                    }
                }
                let mut d = runs.clone();
                d.dedup();
                assert_eq!(runs.len(), d.len());
            } else {
                assert_eq!(tower_paths, 0, "no tower above the last change");
            }
            loaded = l.paths.last().map(|p| fil(p.object));
        }
        // the first layer has a brim around the tower and is dense
        assert!(layers[0].paths.iter().any(|p| p.feature == Feature::Brim));
        // nothing overlaps the object footprint
        assert!(t.min[0] > 73.0 || t.min[1] > 73.0);
    }

    #[test]
    fn flush_volumes_look_like_bambu_studios() {
        // Bambu Studio 2.09 for white ↔ pink PETG on the H2D: 192 and 330 mm³
        let a = flush_volume("#FFFFFF", "#F9C1BD");
        let b = flush_volume("#F9C1BD", "#FFFFFF");
        assert!(a > 140.0 && a < 260.0, "white→pink {a}");
        assert!(b > a, "light after dark needs more: {b} vs {a}");
        assert!(flush_volume("#000000", "#FFFFFF") > flush_volume("#FFFFFF", "#000000"));
        assert!((flush_volume("#123456", "#123456") - 183.7).abs() < 1.0);
    }
}
