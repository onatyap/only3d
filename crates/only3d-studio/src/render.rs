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
}

fn w() -> u32 {
    800
}
fn h() -> u32 {
    600
}

impl Default for RenderRequest {
    fn default() -> Self {
        RenderRequest { view: View::Iso, width: 800, height: 600, ids: None, toolpaths: false, layer: None, fit_plate: false }
    }
}

pub fn render(studio: &mut Studio, req: &RenderRequest) -> Result<Vec<u8>, ToolError> {
    let opts = RenderOptions {
        width: req.width.clamp(64, 2048),
        height: req.height.clamp(64, 2048),
        view: req.view,
        bed: Some(studio.bed.size),
        fit_bed: req.fit_plate,
        ..Default::default()
    };
    if req.toolpaths {
        if !studio.slice_is_current() {
            crate::tools::run_slice(studio)?;
        }
        let s = studio.last_slice.clone().ok_or_else(|| ToolError::new("not_sliced", "no slice yet", "call `slice` first"))?;
        let segs = only3d_slice::preview_segments(&s.result.layers, req.layer);
        let items: Vec<LineItem> = segs.iter().map(|(f, v)| LineItem { segments: v, color: f.color(), width: 1.2 }).collect();
        return Ok(raster::render(&[], &items, &opts).to_png());
    }
    let objs = studio.evaluated()?;
    let mut items = Vec::new();
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
    if items.is_empty() && req.ids.is_some() {
        return Err(ToolError::new("not_found", "none of the requested objects exist", "call `describe` to list object ids"));
    }
    let ri: Vec<RenderItem> = items.iter().map(|(m, c)| RenderItem { mesh: m, color: *c }).collect();
    Ok(raster::render(&ri, &[], &opts).to_png())
}

/// Plate thumbnail for the `.gcode.3mf` (Bambu shows it on the printer screen).
pub fn thumbnail(studio: &mut Studio, size: u32) -> Result<Vec<u8>, ToolError> {
    render(studio, &RenderRequest { view: View::Custom { azimuth: -60.0, elevation: 35.0 }, width: size, height: size, ..Default::default() })
}
