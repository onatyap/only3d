//! STEP (ISO 10303-21: AP203, AP214, AP242) solids, read and tessellated with monstertruck.
//! Each solid becomes one object (assembly instances placed where the assembly puts them),
//! scaled from the file's length unit to millimetres.

use std::collections::HashMap;

use glam::{DMat4, Vec3};
use monstertruck_assembly::assy::{EdgeEntity, NodeEntity};
use monstertruck_io::step::load::convert::{AssembleEntity, ProductEntity};
use monstertruck_io::step::load::Table;
use monstertruck_meshing::prelude::*;
use monstertruck_topology::compress::CompressedShell;

use crate::io::ImportedObject;
use crate::{GeomError, Mesh, Result};

/// Chord error of the tessellation (mm), like the only3d primitives.
const CHORD_MM: f64 = 0.05;

type Meshed = CompressedShell<Point3, PolylineCurve<Point3>, Option<PolygonMesh>>;

fn perr(message: impl Into<String>) -> GeomError {
    GeomError::Parse { format: "STEP", message: message.into() }
}

/// Every solid in a STEP file, in millimetres.
pub fn read(bytes: &[u8]) -> Result<Vec<ImportedObject>> {
    let table = Table::from_step_bytes(bytes).map_err(|e| perr(e.to_string()))?;
    let unit = length_unit_mm(&String::from_utf8_lossy(bytes));
    let tol = CHORD_MM / unit;

    // one mesh per solid (or surface model), tessellated once even when placed many times
    let mut meshes: HashMap<u64, Option<Mesh>> = HashMap::new();
    let mut tessellate = |idx: u64| -> Option<Mesh> {
        meshes
            .entry(idx)
            .or_insert_with(|| {
                let shells = if let Some(s) = table.manifold_solid_brep.get(&idx) {
                    table.to_compressed_solid(s).ok()?.boundaries
                } else {
                    table.to_compressed_shells(table.shell_based_surface_model.get(&idx)?).ok()?
                };
                let m = to_mesh(shells.into_iter().map(|s| s.robust_triangulation(tol)), unit);
                (!m.is_empty()).then_some(m)
            })
            .clone()
    };

    let mut out: Vec<ImportedObject> = Vec::new();
    let mut placed: std::collections::HashSet<u64> = Default::default();
    // assemblies: every path from a top product down to a part with a shape is one placed copy
    if let Ok(assy) = table.step_assy() {
        let flat = assy.map(
            |p: &ProductEntity| NodeEntity { shape: p.shape.clone(), attrs: if p.attrs.name.trim().is_empty() { p.attrs.id.clone() } else { p.attrs.name.clone() } },
            |e: &AssembleEntity| EdgeEntity { matrix: Matrix4::try_from(&e.matrix).unwrap_or(Matrix4::identity()), attrs: () },
        );
        for top in flat.top_nodes() {
            for path in flat.paths_iter(top.index()) {
                let node = path.terminal_node();
                let (shapes, name) = (node.shape(), node.attributes());
                let m = path.matrix();
                let cols: [[f64; 4]; 4] = m.into();
                let xf = DMat4::from_cols_array_2d(&cols).as_mat4();
                for &idx in shapes {
                    if let Some(mut mesh) = tessellate(idx) {
                        placed.insert(idx);
                        if xf != glam::Mat4::IDENTITY {
                            mesh.transform(&xf);
                        }
                        out.push(ImportedObject::new(name.clone(), mesh));
                    }
                }
            }
        }
    }
    // solids no product refers to (files without product structure)
    let mut loose: Vec<(u64, String)> = table.manifold_solid_brep.iter().map(|(k, s)| (*k, s.label.clone())).chain(table.shell_based_surface_model.keys().map(|k| (*k, String::new()))).filter(|(k, _)| !placed.contains(k)).collect();
    loose.sort_by_key(|(k, _)| *k);
    for (idx, label) in loose {
        if let Some(mesh) = tessellate(idx) {
            let name = if label.trim().is_empty() { format!("solid {}", out.len() + 1) } else { label };
            out.push(ImportedObject::new(name, mesh));
        }
    }
    if out.is_empty() {
        return Err(perr(if table.manifold_solid_brep.is_empty() && table.shell_based_surface_model.is_empty() { "no solids in the file (only curves or a drawing?)" } else { "none of its solids could be meshed" }));
    }
    if (unit - 1.0).abs() > 1e-9 {
        for o in &mut out {
            o.mesh.transform(&glam::Mat4::from_scale(Vec3::splat(unit as f32)));
        }
    }
    Ok(out)
}

/// The tessellated shells as one welded mesh, without the zero-area slivers tessellation leaves
/// along some seams (they read as open and non-manifold edges).
fn to_mesh(shells: impl Iterator<Item = Meshed>, unit_mm: f64) -> Mesh {
    let mut tris: Vec<[Vec3; 3]> = Vec::new();
    for s in shells {
        let poly = s.to_polygon();
        let pos = poly.positions();
        let v = |i: usize| Vec3::new(pos[i].x as f32, pos[i].y as f32, pos[i].z as f32);
        for t in poly.faces().triangle_iter() {
            tris.push([v(t[0].pos), v(t[1].pos), v(t[2].pos)]);
        }
    }
    let mut m = Mesh::from_soup(&tris);
    m.weld((1e-4 / unit_mm) as f32);
    m.remove_degenerate();
    m
}

/// Millimetres per length unit of the file: the unit its global context names (else the first
/// length unit declared; mm when it declares none).
pub fn length_unit_mm(text: &str) -> f64 {
    let ents = unit_entities(text);
    let refs = |body: &str| -> Vec<u64> { body.split('#').skip(1).filter_map(|s| s.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok()).collect() };
    let is_length = |id: &u64| ents.get(id).is_some_and(|b| b.contains("LENGTH_UNIT("));
    let mut ids: Vec<u64> = ents.keys().copied().collect();
    ids.sort_unstable();
    // the length unit the global context assigns, else the first one declared
    let global = ids.iter().filter_map(|id| ents[id].split("GLOBAL_UNIT_ASSIGNED_CONTEXT(").nth(1)).flat_map(|rest| refs(rest.split(')').next().unwrap_or(""))).find(is_length);
    let id = global.or_else(|| ids.iter().copied().find(is_length));
    id.and_then(|id| unit_factor(&ents, id, 0)).filter(|f| f.is_finite() && *f > 0.0).unwrap_or(1.0)
}

fn unit_factor(ents: &HashMap<u64, String>, id: u64, depth: u32) -> Option<f64> {
    let b = ents.get(&id)?;
    if let Some(si) = b.split("SI_UNIT(").nth(1) {
        let args = si.split(')').next()?;
        if !args.contains(".METRE.") {
            return None;
        }
        let prefix = args.split(',').next()?;
        return Some(match prefix {
            ".MILLI." => 1.0,
            ".CENTI." => 10.0,
            ".DECI." => 100.0,
            "$" => 1000.0,
            ".KILO." => 1e6,
            ".MICRO." => 1e-3,
            ".NANO." => 1e-6,
            _ => return None,
        });
    }
    let conv = b.split("CONVERSION_BASED_UNIT(").nth(1)?;
    let name = conv.split('\'').nth(1).unwrap_or("").to_ascii_uppercase();
    match name.as_str() {
        "INCH" | "IN" => return Some(25.4),
        "FOOT" | "FT" => return Some(304.8),
        "MIL" | "THOU" => return Some(0.0254),
        _ => {}
    }
    // a named unit defined by a measure: LENGTH_MEASURE_WITH_UNIT(LENGTH_MEASURE(25.4),#unit)
    if depth > 3 {
        return None;
    }
    let m = conv.split('#').nth(1)?.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok()?;
    let mb = ents.get(&m)?;
    let v: f64 = mb.split("LENGTH_MEASURE(").nth(1)?.split(')').next()?.parse().ok()?;
    let base = mb.rsplit('#').next()?.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok()?;
    Some(v * unit_factor(ents, base, depth + 1)?)
}

/// The DATA entities that say something about units, whitespace removed, by id.
fn unit_entities(text: &str) -> HashMap<u64, String> {
    let mut out = HashMap::new();
    let data = match text.find("DATA;") {
        Some(i) => &text[i + 5..],
        None => text,
    };
    let mut cur = String::new();
    let (mut in_str, mut in_comment) = (false, false);
    let mut chars = data.chars().peekable();
    while let Some(c) = chars.next() {
        if in_comment {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_comment = false;
            }
            continue;
        }
        if in_str {
            cur.push(c);
            if c == '\'' {
                in_str = false;
            }
            continue;
        }
        match c {
            '\'' => {
                in_str = true;
                cur.push(c);
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                in_comment = true;
            }
            ';' => {
                if cur.contains("UNIT") || cur.contains("MEASURE") {
                    if let Some((id, body)) = cur.trim_start_matches('#').split_once('=') {
                        if let Ok(id) = id.parse::<u64>() {
                            out.insert(id, body.to_string());
                        }
                    }
                }
                cur.clear();
            }
            c if c.is_whitespace() => {}
            c => cur.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLATE: &[u8] = include_bytes!("../../tests/data/plate_hole.step");

    #[test]
    fn a_plate_with_a_hole_is_one_closed_solid() {
        let objs = read(PLATE).unwrap();
        assert_eq!(objs.len(), 1);
        let r = objs[0].mesh.analyze();
        assert!(r.watertight, "{r:?}");
        // 20 × 10 × 5 minus a 4 mm hole: 1000 − 62.8 mm³ (the faceted hole is a hair smaller)
        assert!((r.volume_mm3 - 937.2).abs() < 1.5, "{}", r.volume_mm3);
        assert!((r.size - Vec3::new(20.0, 10.0, 5.0)).length() < 1e-3, "{:?}", r.size);
    }

    #[test]
    fn inch_files_come_in_as_millimetres() {
        let text = String::from_utf8_lossy(PLATE).replace("( LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT(.MILLI.,.METRE.) )", "( CONVERSION_BASED_UNIT('INCH',#900) LENGTH_UNIT() NAMED_UNIT(#901) )");
        assert!(text.contains("'INCH'"), "the fixture's unit line changed");
        assert_eq!(length_unit_mm(&text), 25.4);
        let objs = read(text.as_bytes()).unwrap();
        assert!((objs[0].mesh.analyze().size.x - 508.0).abs() < 0.01);
        assert_eq!(length_unit_mm("DATA;\n#5=(LENGTH_UNIT()NAMED_UNIT(*)SI_UNIT($,.METRE.));\nENDSEC;"), 1000.0);
        assert_eq!(length_unit_mm("no units here"), 1.0);
    }

    /// A real export (Fusion 360, via Bambu Studio's calibration models) when it is installed.
    #[test]
    fn a_fusion_360_export_is_watertight() {
        let Ok(bytes) = std::fs::read("/Applications/BambuStudio.app/Contents/Resources/calib/volumetric_speed/SpeedTestStructure.step") else { return };
        let objs = read(&bytes).unwrap();
        let r = objs[0].mesh.analyze();
        assert!(r.watertight && r.non_manifold_edges == 0, "{r:?}");
        assert!((r.size - Vec3::new(180.0, 100.0, 280.0)).length() < 0.01, "{:?}", r.size);
    }

    #[test]
    fn junk_is_an_error_not_a_panic() {
        assert!(read(b"ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\nENDSEC;\nEND-ISO-10303-21;\n").is_err());
        assert!(read(b"not a step file").is_err());
    }
}
