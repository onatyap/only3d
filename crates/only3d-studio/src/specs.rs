//! Design checks saved with the project (`Project::specs`): sizes, clearances between parts,
//! wall thickness at a height, overhang area. They run with the printability checks after
//! every change and show in the Checks panel; agents add them with `check {"add": …}`.

use only3d_model::{EvaluatedObject, Finding, Severity, Spec};
use only3d_slice::poly;

use crate::Studio;

/// The gap between two objects as placed (negative: they overlap by that much), sampled at
/// heights across their shared Z range.
pub fn gap(a: &EvaluatedObject, b: &EvaluatedObject, zs: Option<&[f64]>) -> Option<f64> {
    let (ba, bb) = (a.report.bounds, b.report.bounds);
    let (z0, z1) = (ba.min.z.max(bb.min.z) as f64, ba.max.z.min(bb.max.z) as f64);
    if z1 <= z0 {
        return None;
    }
    let heights: Vec<f64> = zs.map(|z| z.to_vec()).unwrap_or_else(|| (0..9).map(|k| z0 + (z1 - z0) * (k as f64 + 0.5) / 9.0).collect());
    let mut worst: Option<f64> = None;
    for z in heights {
        let (ra, _) = only3d_slice::slicer::slice_mesh(&a.mesh, &[z]);
        let (rb, _) = only3d_slice::slicer::slice_mesh(&b.mesh, &[z]);
        let (ra, rb) = (&ra[0], &rb[0]);
        if ra.is_empty() || rb.is_empty() {
            continue;
        }
        let touch = |d: f64| poly::region_area(&poly::intersection(&poly::offset(ra, d), rb)) > 1e-6;
        let g = if touch(0.0) {
            let (mut lo, mut hi) = (0.0f64, 5.0f64);
            for _ in 0..20 {
                let mid = (lo + hi) / 2.0;
                if touch(-mid) {
                    lo = mid
                } else {
                    hi = mid
                }
            }
            -hi
        } else if !touch(50.0) {
            continue;
        } else {
            let (mut lo, mut hi) = (0.0f64, 50.0f64);
            for _ in 0..22 {
                let mid = (lo + hi) / 2.0;
                if touch(mid) {
                    hi = mid
                } else {
                    lo = mid
                }
            }
            hi
        };
        worst = Some(worst.map_or(g, |w: f64| w.min(g)));
    }
    worst
}

/// The thinnest wall of `o` at height `z`: the widest opening (shrink then grow back) that
/// still keeps almost all of the cross-section.
pub fn thinnest_wall(o: &EvaluatedObject, z: f64) -> Option<f64> {
    let (r, _) = only3d_slice::slicer::slice_mesh(&o.mesh, &[z]);
    let r = &r[0];
    let area = poly::region_area(r);
    if area < 1e-6 {
        return None;
    }
    let keeps = |w: f64| poly::region_area(&poly::offset(&poly::offset(r, -w / 2.0), w / 2.0)) >= area * 0.995;
    let (mut lo, mut hi) = (0.0f64, 40.0f64);
    if keeps(hi) {
        return Some(hi);
    }
    for _ in 0..18 {
        let mid = (lo + hi) / 2.0;
        if keeps(mid) {
            lo = mid
        } else {
            hi = mid
        }
    }
    Some(lo)
}

fn fmt_limits(sp: &Spec, unit: &str) -> String {
    match (sp.min, sp.max) {
        (Some(a), Some(b)) => format!("{a}–{b}{unit}"),
        (Some(a), None) => format!("≥ {a}{unit}"),
        (None, Some(b)) => format!("≤ {b}{unit}"),
        (None, None) => "no limit set".into(),
    }
}

/// Check one spec: (passed, what was measured), or an error about the spec itself.
pub fn evaluate(sp: &Spec, objs: &[EvaluatedObject]) -> Result<(bool, String), String> {
    let find = |id: &Option<String>| -> Result<&EvaluatedObject, String> {
        let id = id.as_deref().ok_or("needs `object`")?;
        objs.iter().find(|o| o.id == id).ok_or_else(|| format!("no object '{id}'"))
    };
    let within = |v: f64| sp.min.is_none_or(|m| v >= m - 1e-6) && sp.max.is_none_or(|m| v <= m + 1e-6);
    match sp.kind.as_str() {
        "size" => {
            let o = find(&sp.object)?;
            let k = match sp.axis.as_deref().unwrap_or("x") {
                "x" => 0,
                "y" => 1,
                "z" => 2,
                other => return Err(format!("axis '{other}'")),
            };
            let v = o.report.size[k] as f64;
            Ok((within(v), format!("{} {} is {v:.2} mm (wants {})", o.id, ["width", "depth", "height"][k], fmt_limits(sp, " mm"))))
        }
        "clearance" => {
            let (a, b) = (find(&sp.object)?, find(&sp.other)?);
            match gap(a, b, sp.z.map(|z| vec![z]).as_deref()) {
                Some(g) if g < 0.0 => Ok((within(g), format!("{} overlaps {} by {:.2} mm (wants a gap {})", a.id, b.id, -g, fmt_limits(sp, " mm")))),
                Some(g) => Ok((within(g), format!("{} to {}: {g:.2} mm gap (wants {})", a.id, b.id, fmt_limits(sp, " mm")))),
                None => Err(format!("{} and {} don't share a height (place them assembled)", a.id, b.id)),
            }
        }
        "wall" => {
            let o = find(&sp.object)?;
            let z = sp.z.unwrap_or(o.report.bounds.min.z as f64 + o.report.size.z as f64 / 2.0);
            match thinnest_wall(o, z) {
                Some(w) => Ok((within(w), format!("{} thinnest wall at z {z:.1}: {w:.2} mm (wants {})", o.id, fmt_limits(sp, " mm")))),
                None => Err(format!("{} has nothing at z {z:.1}", o.id)),
            }
        }
        "overhang" => {
            let o = find(&sp.object)?;
            let a = o.mesh.overhang_area(45.0);
            Ok((within(a), format!("{} overhangs {a:.0} mm² past 45° (wants {})", o.id, fmt_limits(sp, " mm²"))))
        }
        other => Err(format!("unknown kind '{other}' (size, clearance, wall, overhang)")),
    }
}

/// Every saved check as a finding: passing ones as info (so they show as ticked), failing
/// ones as errors.
pub fn run(s: &mut Studio) -> Vec<Finding> {
    if s.project.specs.is_empty() {
        return Vec::new();
    }
    let Ok(objs) = s.evaluated_all() else { return Vec::new() };
    s.project
        .specs
        .iter()
        .map(|sp| {
            let label = if sp.note.is_empty() { sp.id.clone() } else { format!("{} ({})", sp.id, sp.note) };
            match evaluate(sp, &objs) {
                Ok((true, m)) => Finding { severity: Severity::Info, code: "spec_ok".into(), object: sp.object.clone(), message: format!("✓ {label}: {m}"), hint: String::new(), mitigation: None },
                Ok((false, m)) => Finding { severity: Severity::Error, code: "spec_failed".into(), object: sp.object.clone(), message: format!("✗ {label}: {m}"), hint: "change the design (or the check: check {\"remove\": id})".into(), mitigation: None },
                Err(e) => Finding { severity: Severity::Warning, code: "spec_broken".into(), object: sp.object.clone(), message: format!("{label}: {e}"), hint: "fix or remove this check".into(), mitigation: None },
            }
        })
        .collect()
}
