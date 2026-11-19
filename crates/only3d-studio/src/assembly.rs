//! The assembly: how printed parts go together, with the bought hardware they hold (a
//! parachute, a motor, bearings). It lives beside the plates, in the project: placing a part
//! here doesn't move it on its plate, and reference hardware is never placed on a plate, sliced
//! or checked for printing. `render {"assembly": true}` draws it; this tool sets it and reports
//! where bodies run into each other.

use std::sync::Arc;

use glam::{Mat4, Vec3};
use only3d_geom::Mesh;
use only3d_model::{AssemblyPart, Num, ReferencePart};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::render::RenderRequest;
use crate::tools::{def, Registry, ToolError, ToolOutput};
use crate::Studio;

/// Reference hardware's colour when it has none: steel grey, apart from filament colours.
const REFERENCE_GREY: [u8; 3] = [138, 150, 166];

/// One body of the assembled scene, in assembly coordinates.
pub struct Body {
    /// The printed object it comes from ("" for reference hardware).
    pub object: String,
    /// Object name (or object · part) for printed parts; the name for reference hardware.
    pub name: String,
    pub mesh: Arc<Mesh>,
    pub color: [u8; 3],
    pub reference: bool,
    /// Index of the assembly entry it belongs to (parts first, then references): bodies of one
    /// entry are one piece.
    pub entry: usize,
}

fn nums(v: &Option<[Num; 3]>, params: &only3d_model::Params, what: &str) -> Result<Vec3, ToolError> {
    let Some(v) = v else { return Ok(Vec3::ZERO) };
    let mut out = [0.0f32; 3];
    for (k, n) in v.iter().enumerate() {
        out[k] = n.eval(params).map_err(|e| ToolError::new("bad_expression", format!("{what}: {e}"), "check parameter names with `describe`"))? as f32;
    }
    Ok(Vec3::from(out))
}

/// Every body of the assembly, placed. Notes say what was left out.
pub fn bodies(s: &mut Studio) -> Result<(Vec<Body>, Vec<String>), ToolError> {
    let Some(a) = s.project.assembly.clone() else { return Ok((Vec::new(), Vec::new())) };
    let params = s.project.params.clone();
    let mut out = Vec::new();
    let mut notes = Vec::new();
    for (k, p) in a.parts.iter().enumerate() {
        let Some(o) = s.project.object(&p.object).cloned() else {
            notes.push(format!("'{}' is in the assembly but no longer exists", p.object));
            continue;
        };
        let at = nums(&p.at, &params, &format!("parts[{k}].at"))?;
        let rot = nums(&p.rotate, &params, &format!("parts[{k}].rotate"))?;
        // the object in its own modelling frame (its shape's coordinates, with its scale but
        // not the turn it prints with), turned, then moved
        let xf = Mat4::from_translation(at) * Mat4::from_quat(only3d_model::shape::euler_xyz(rot)) * Mat4::from_scale(Vec3::from(o.scale));
        let mut pieces = vec![(o.name.clone(), o.filament, o.shape.clone())];
        for (j, q) in o.parts.iter().enumerate() {
            pieces.push((format!("{} · {}", o.name, if q.name.is_empty() { format!("part {}", j + 1) } else { q.name.clone() }), q.filament, q.shape.clone()));
        }
        for (name, filament, shape) in pieces {
            let mesh = s.evaluator.eval(&shape, &params).map_err(ToolError::from)?;
            let color = s.project.filaments.get(filament as usize - 1).map(|f| crate::render::parse_hex(&f.color)).unwrap_or([200, 190, 170]);
            out.push(Body { object: o.id.clone(), name, mesh: Arc::new(mesh.transformed(&xf)), color, reference: false, entry: k });
        }
    }
    // parts printed in the same colour get a slightly different shade each, so they read apart
    let mut seen: Vec<([u8; 3], usize)> = Vec::new();
    let mut last_entry = usize::MAX;
    for b in out.iter_mut() {
        let n = match seen.iter_mut().find(|(c, _)| *c == b.color) {
            Some((_, n)) => {
                if b.entry != last_entry {
                    *n += 1;
                }
                *n
            }
            None => {
                seen.push((b.color, 0));
                0
            }
        };
        last_entry = b.entry;
        let f = 1.0 - 0.13 * (n % 3) as f32;
        b.color = b.color.map(|c| (c as f32 * f) as u8);
    }
    for (k, r) in a.reference.iter().enumerate() {
        let mesh = s.evaluator.eval(&r.shape, &params).map_err(ToolError::from)?;
        let color = r.color.as_deref().map(crate::render::parse_hex).unwrap_or(REFERENCE_GREY);
        out.push(Body { object: String::new(), name: r.name.clone(), mesh, color, reference: true, entry: a.parts.len() + k });
    }
    Ok((out, notes))
}

#[derive(Deserialize, JsonSchema)]
pub struct AssemblyIn {
    /// Replace the placed parts: [{"object": id, "at": [x, y, z], "rotate": [x, y, z]}].
    #[serde(default)]
    pub parts: Option<Vec<AssemblyPart>>,
    /// Replace the hardware that is never printed: [{"name", "shape", "color"}].
    #[serde(default)]
    pub reference: Option<Vec<ReferencePart>>,
    /// Remove the assembly.
    #[serde(default)]
    pub clear: bool,
    /// An iso picture (default true).
    #[serde(default)]
    pub render: Option<bool>,
}

const ASSEMBLY_DOC: &str = "Show how printed parts go together, with bought hardware that is never printed (a parachute, a motor). Saved with the project; nothing moves on the plates. Reports bodies that overlap. Other views and cutaways: render {\"assembly\": true, \"cut\": {\"axis\": \"y\"}}.";

fn t_assembly(s: &mut Studio, i: AssemblyIn) -> Result<ToolOutput, ToolError> {
    let mut changed = false;
    if i.clear {
        s.project.assembly = None;
        changed = true;
    }
    if i.parts.is_some() || i.reference.is_some() {
        let mut a = s.project.assembly.clone().unwrap_or_default();
        if let Some(p) = i.parts {
            // checked now, not when drawn: every object exists
            let ids: Vec<String> = s.project.objects.iter().map(|o| o.id.clone()).collect();
            if let Some((k, bad)) = p.iter().enumerate().find(|(_, x)| !ids.contains(&x.object)) {
                return Err(ToolError::new("not_found", format!("assembly parts[{k}]: no object '{}'", bad.object), format!("objects: {}", ids.join(", "))));
            }
            a.parts = p;
        }
        if let Some(r) = i.reference {
            if let Some((k, _)) = r.iter().enumerate().find(|(_, x)| x.name.trim().is_empty()) {
                return Err(ToolError::new("bad_input", format!("assembly reference[{k}] has no name"), "name it (\"parachute\", \"motor 18x70\")"));
            }
            a.reference = r;
        }
        s.project.assembly = Some(a);
        changed = true;
    }
    if changed {
        s.changed("assembly");
    }
    // builds every body: bad expressions and shapes fail here, and the change is undone
    let (bodies, mut notes) = bodies(s)?;
    let Some(a) = s.project.assembly.clone().filter(|a| !a.parts.is_empty() || !a.reference.is_empty()) else {
        return Ok(ToolOutput::text(if i.clear { "Removed the assembly." } else { "No assembly yet: place parts with {\"parts\": [{\"object\": id, \"at\": [x, y, z], \"rotate\": [x, y, z]}]} and add bought hardware with {\"reference\": [{\"name\", \"shape\"}]}." }).with_data(json!({"parts": [], "reference": []})));
    };
    let mut lines = Vec::new();
    let mut rows = Vec::new();
    for b in &bodies {
        let bb = b.mesh.bounds();
        let r = |v: f32| (v * 10.0).round() / 10.0;
        let span = [[r(bb.min.x), r(bb.max.x)], [r(bb.min.y), r(bb.max.y)], [r(bb.min.z), r(bb.max.z)]];
        lines.push(format!("  {}{}: x {}..{}, y {}..{}, z {}..{}", b.name, if b.reference { " (reference, not printed)" } else { "" }, span[0][0], span[0][1], span[1][0], span[1][1], span[2][0], span[2][1]));
        rows.push(json!({"name": b.name, "object": if b.reference { json!(null) } else { json!(b.object) }, "reference": b.reference, "bounds": span}));
    }
    let clashes = interference(&bodies, &mut notes);
    let mut text = format!("Assembly: {} placed part(s), {} reference(s), {} bodies:\n{}", a.parts.len(), a.reference.len(), bodies.len(), lines.join("\n"));
    if clashes.is_empty() {
        text.push_str("\nNo bodies overlap.");
    } else {
        for c in &clashes {
            text.push_str(&format!("\nOVERLAP: '{}' and '{}' share {:.0} mm³ (x {:.1}..{:.1}, y {:.1}..{:.1}, z {:.1}..{:.1})", c.a, c.b, c.volume, c.at[0], c.at[3], c.at[1], c.at[4], c.at[2], c.at[5]));
        }
        text.push_str("\nMove a part (parts[].at) or change the design so they fit; `section` and `clearance` measure the gaps on the plate.");
    }
    for n in &notes {
        text.push_str(&format!("\nNote: {n}"));
    }
    let data = json!({
        "parts": a.parts, "reference": a.reference, "bodies": rows,
        "overlaps": clashes.iter().map(|c| json!({"a": c.a, "b": c.b, "volume_mm3": (c.volume * 10.0).round() / 10.0, "bounds": c.at})).collect::<Vec<_>>(),
        "notes": notes,
    });
    let mut out = ToolOutput::text(text).with_data(data);
    if i.render.unwrap_or(true) {
        let (png, _) = crate::render::render_noted(s, &RenderRequest { assembly: true, ..Default::default() })?;
        out = out.with_image(png, "assembly");
    }
    Ok(out)
}

struct Clash {
    a: String,
    b: String,
    volume: f64,
    /// [x0, y0, z0, x1, y1, z1] of the shared volume.
    at: [f32; 6],
}

/// Pairs of bodies from different entries whose solids overlap by more than 1 mm³.
fn interference(bodies: &[Body], notes: &mut Vec<String>) -> Vec<Clash> {
    use only3d_model::csg::{boolean, BoolOp};
    let mut out = Vec::new();
    let mut stats = Default::default();
    let mut tried = 0;
    let mut skipped = 0;
    for i in 0..bodies.len() {
        for j in i + 1..bodies.len() {
            let (a, b) = (&bodies[i], &bodies[j]);
            if a.entry == b.entry || !a.mesh.bounds().overlaps(&b.mesh.bounds()) {
                continue;
            }
            tried += 1;
            if tried > 60 {
                skipped += 1;
                continue;
            }
            match boolean(&a.mesh, &b.mesh, BoolOp::Intersect, &mut stats) {
                Ok(m) if !m.is_empty() => {
                    let v = m.analyze().volume_mm3.abs();
                    if v > 1.0 {
                        let bb = m.bounds();
                        out.push(Clash { a: a.name.clone(), b: b.name.clone(), volume: v, at: [bb.min.x, bb.min.y, bb.min.z, bb.max.x, bb.max.y, bb.max.z] });
                    }
                }
                Ok(_) => {}
                Err(_) => notes.push(format!("couldn't test '{}' against '{}' (not closed solids)", a.name, b.name)),
            }
        }
    }
    if skipped > 0 {
        notes.push(format!("{skipped} more pair(s) of touching bodies weren't tested"));
    }
    out
}

pub fn register(r: &mut Registry) {
    r.add(def::<AssemblyIn>("assembly", "Assembly", ASSEMBLY_DOC, true, false, t_assembly));
}
