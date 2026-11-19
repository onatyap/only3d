//! Scene and toolpath rendering to PNG (headless), used by the `render` tool, thumbnails and
//! the CLI.

use only3d_geom::raster::{self, LineItem, RenderItem, RenderOptions, View};
use serde::{Deserialize, Serialize};

use crate::{Studio, ToolError};

pub fn parse_hex(c: &str) -> [u8; 3] {
    let s = c.trim_start_matches('#');
    let h = |i: usize| s.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok()).unwrap_or(200);
    [h(0), h(2), h(4)]
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RenderRequest {
    /// iso (default), front, back, left, right, top, bottom, or {"custom": {"azimuth": deg, "elevation": deg}}.
    #[serde(default)]
    pub view: View,
    #[serde(default = "w")]
    pub width: u32,
    #[serde(default = "h")]
    pub height: u32,
    /// Default: all visible.
    #[serde(default)]
    pub ids: Option<Vec<String>>,
    /// The sliced toolpaths instead (slices if needed).
    #[serde(default)]
    pub toolpaths: bool,
    /// Toolpaths up to this layer (0-based).
    #[serde(default)]
    pub layer: Option<usize>,
    /// Frame the whole plate.
    #[serde(default)]
    pub fit_plate: bool,
    /// Draw the assembly (tool `assembly`) instead of the plate.
    #[serde(default)]
    pub assembly: bool,
    /// Cutaway: drop the half facing the camera, e.g. {"axis": "y"} or {"axis": "x", "at": 12}.
    #[serde(default)]
    pub cut: Option<Cut>,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Cut {
    /// x | y | z
    pub axis: String,
    /// mm (default: the middle)
    #[serde(default)]
    pub at: Option<f32>,
    /// Drop the far half instead.
    #[serde(default)]
    pub flip: bool,
}

fn w() -> u32 {
    800
}
fn h() -> u32 {
    600
}

impl Default for RenderRequest {
    fn default() -> Self {
        RenderRequest { view: View::Iso, width: 800, height: 600, ids: None, toolpaths: false, layer: None, fit_plate: false, assembly: false, cut: None }
    }
}

/// A mesh to draw and its colour.
type Item = (std::sync::Arc<only3d_geom::Mesh>, [u8; 3]);

pub fn render(studio: &mut Studio, req: &RenderRequest) -> Result<Vec<u8>, ToolError> {
    render_noted(studio, req).map(|(png, _)| png)
}

/// Render, with notes on anything left out (a part the assembly names that no longer exists,
/// a body the cutaway couldn't cut).
pub fn render_noted(studio: &mut Studio, req: &RenderRequest) -> Result<(Vec<u8>, Vec<String>), ToolError> {
    let opts = RenderOptions {
        width: req.width.clamp(64, 2048),
        height: req.height.clamp(64, 2048),
        view: req.view,
        bed: if req.assembly { None } else { Some(studio.bed.size) },
        fit_bed: req.fit_plate && !req.assembly,
        ..Default::default()
    };
    if req.toolpaths {
        if req.assembly || req.cut.is_some() {
            return Err(ToolError::new("bad_input", "toolpaths show the sliced plate, not the assembly or a cutaway", "drop toolpaths, or drop assembly/cut"));
        }
        if !studio.slice_is_current() {
            crate::tools::run_slice(studio)?;
        }
        let s = studio.last_slice.clone().ok_or_else(|| ToolError::new("not_sliced", "no slice yet", "call `slice` first"))?;
        let segs = only3d_slice::preview_segments(&s.result.layers, req.layer);
        let items: Vec<LineItem> = segs.iter().map(|(f, v)| LineItem { segments: v, color: f.color(), width: 1.2 }).collect();
        return Ok((raster::render(&[], &items, &opts).to_png(), Vec::new()));
    }
    let mut notes = Vec::new();
    let mut items: Vec<Item> = Vec::new();
    if req.assembly {
        let (bodies, n) = crate::assembly::bodies(studio)?;
        notes.extend(n);
        if bodies.is_empty() {
            return Err(ToolError::new("no_assembly", "the project has no assembly yet", "place the printed parts with `assembly` {\"parts\": [{\"object\": id, \"at\": [x, y, z], \"rotate\": [x, y, z]}]}"));
        }
        for b in bodies {
            if req.ids.as_ref().is_none_or(|ids| ids.contains(&b.object) || ids.contains(&b.name)) {
                items.push((b.mesh, b.color));
            }
        }
    } else {
        let objs = studio.evaluated()?;
        for o in objs.iter().filter(|o| !o.hidden) {
            if let Some(ids) = &req.ids {
                if !ids.contains(&o.id) {
                    continue;
                }
            }
            let bodies: Vec<(std::sync::Arc<only3d_geom::Mesh>, u32)> = if o.parts.is_empty() { vec![(o.mesh.clone(), o.filament)] } else { o.parts.iter().map(|p| (p.mesh.clone(), p.filament)).collect() };
            for (mesh, filament) in bodies {
                let mut c = studio.project.filaments.get(filament as usize - 1).map(|f| parse_hex(&f.color)).unwrap_or([200, 190, 170]);
                if studio.selection.contains(&o.id) {
                    c = [(c[0] as u16 * 3 / 5 + 40) as u8, (c[1] as u16 * 3 / 5 + 80) as u8, (c[2] as u16 * 3 / 5 + 102).min(255) as u8];
                }
                items.push((mesh, c));
            }
        }
    }
    if items.is_empty() && req.ids.is_some() {
        return Err(ToolError::new("not_found", "none of the requested objects exist", if req.assembly { "ids name assembly parts (object ids) or references (names)" } else { "call `describe` to list object ids" }));
    }
    if let Some(cut) = &req.cut {
        items = cutaway(items, cut, req.view, &mut notes)?;
    }
    let ri: Vec<RenderItem> = items.iter().map(|(m, c)| RenderItem { mesh: m, color: *c }).collect();
    Ok((raster::render(&ri, &[], &opts).to_png(), notes))
}

/// Cut every body along `cut` and drop the half on the camera's side (or the other, with
/// `flip`), so the inside shows. The cut faces are closed, so walls read as solid.
fn cutaway(items: Vec<Item>, cut: &Cut, view: View, notes: &mut Vec<String>) -> Result<Vec<Item>, ToolError> {
    use only3d_model::csg::{boolean, BoolOp};
    let axis = match cut.axis.as_str() {
        "x" => 0,
        "y" => 1,
        "z" => 2,
        other => return Err(ToolError::new("bad_axis", format!("cut axis '{other}'"), "x, y or z")),
    };
    let mut bb = only3d_geom::Aabb::EMPTY;
    for (m, _) in &items {
        let b = m.bounds();
        if b.min.x.is_finite() {
            bb.extend(b.min);
            bb.extend(b.max);
        }
    }
    if !bb.min.x.is_finite() {
        return Ok(items);
    }
    let at = cut.at.unwrap_or((bb.min[axis] + bb.max[axis]) / 2.0);
    // the camera's side of the plane
    let (az, el) = view.angles();
    let (az, el) = (az.to_radians(), el.to_radians());
    let cam = [az.cos() * el.cos(), az.sin() * el.cos(), el.sin()];
    let mut drop_positive = cam[axis] > 1e-3 || (cam[axis].abs() <= 1e-3 && axis == 2);
    if cut.flip {
        drop_positive = !drop_positive;
    }
    let pad = 1.0 + (bb.max - bb.min).max_element();
    let (mut lo, mut hi) = (bb.min - glam::Vec3::splat(pad), bb.max + glam::Vec3::splat(pad));
    if drop_positive {
        lo[axis] = at;
    } else {
        hi[axis] = at;
    }
    let mut cutter = only3d_geom::shapes::cuboid(hi - lo).map_err(|e| ToolError::new("cut_failed", e.to_string(), "try another plane"))?;
    let c = cutter.bounds();
    cutter.translate(lo - c.min);
    let mut stats = Default::default();
    let mut out = Vec::new();
    let mut uncut = 0;
    for (m, col) in items {
        match boolean(&m, &cutter, BoolOp::Subtract, &mut stats) {
            Ok(r) if !r.is_empty() => out.push((std::sync::Arc::new(r), col)),
            Ok(_) => {}
            Err(_) => {
                uncut += 1;
                out.push((m, col));
            }
        }
    }
    if uncut > 0 {
        notes.push(format!("{uncut} body(ies) are not closed solids and are drawn whole"));
    }
    Ok(out)
}

/// Plate thumbnail for the `.gcode.3mf` (Bambu shows it on the printer screen).
pub fn thumbnail(studio: &mut Studio, size: u32) -> Result<Vec<u8>, ToolError> {
    render(studio, &RenderRequest { view: View::Custom { azimuth: -60.0, elevation: 35.0 }, width: size, height: size, ..Default::default() })
}
