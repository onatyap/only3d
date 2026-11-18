#![allow(clippy::too_many_arguments, clippy::type_complexity, clippy::needless_range_loop, clippy::unnecessary_sort_by)]
//! only3d native slicer.
//!
//! Mesh → layer regions ([`slicer`]) → toolpaths ([`planner`]) → G-code ([`gcode`]) →
//! Bambu `.gcode.3mf` ([`package`]). Layers are processed in parallel; a 20 mm cube slices in a
//! few milliseconds. Machine-specific start/end G-code comes from a [`gcode::MachineHooks`]
//! implementation (the H2D one renders Bambu Studio's own templates).

pub mod gcode;
pub mod multi;
pub mod package;
pub mod planner;
pub mod poly;
pub mod settings;
pub mod slicer;

use std::sync::Arc;
use std::time::Instant;

use glam::Vec3;
use only3d_geom::Mesh;
use serde::Serialize;

pub use gcode::{FilamentChange, GenericHooks, JobInfo, MachineHooks, NozzleSetup, PrintStats};
pub use multi::{TowerInfo, TowerSpec};
pub use planner::{Feature, LayerPlan, Path};
pub use settings::{Dialect, Flavor, FuzzySkin, InfillPattern, SeamPosition, SliceSettings, WallOrder};
pub use slicer::LayerStack;

#[derive(Debug, thiserror::Error)]
pub enum SliceError {
    #[error("nothing to slice: {0}")]
    Empty(String),
    #[error("invalid settings: {0}")]
    Settings(String),
    #[error("G-code generation failed: {0}")]
    Gcode(String),
}

impl SliceError {
    pub fn code(&self) -> &'static str {
        match self {
            SliceError::Empty(_) => "nothing_to_slice",
            SliceError::Settings(_) => "invalid_settings",
            SliceError::Gcode(_) => "gcode_failed",
        }
    }
}

/// One object on the plate, already in plate coordinates (resting on Z=0).
#[derive(Clone)]
pub struct PlateObject {
    pub id: String,
    pub name: String,
    pub mesh: Arc<Mesh>,
    pub settings: SliceSettings,
    /// 0-based filament (project slot − 1) this body prints with.
    pub filament: usize,
    /// The job object this body belongs to: the bodies of a multi-part object share it (one
    /// object for labels, skipping and grams). Later bodies of a group win where they overlap.
    pub group: usize,
}

impl PlateObject {
    /// A single-body object in filament 0.
    pub fn new(id: impl Into<String>, mesh: Arc<Mesh>, settings: SliceSettings, group: usize) -> Self {
        let id = id.into();
        PlateObject { name: id.clone(), id, mesh, settings, filament: 0, group }
    }
}

pub struct SliceJob {
    pub objects: Vec<PlateObject>,
    /// Plate-wide settings (layer heights, temperatures, fans, extruder).
    pub settings: SliceSettings,
    pub bed_type: String,
    /// Each filament's settings (index = filament id: flow, temperatures, retraction, cooling,
    /// density), for plates with several filaments. Empty: every body uses `settings`.
    pub filament_settings: Vec<SliceSettings>,
    /// The prime tower, used when the plate changes filament.
    pub tower: Option<TowerSpec>,
    /// Which nozzle each filament prints with, and how the nozzles heat (multi-nozzle machines).
    pub nozzles: Option<NozzleSetup>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Timings {
    pub slice_ms: f64,
    pub plan_ms: f64,
    pub gcode_ms: f64,
}

pub struct SliceResult {
    pub stack: LayerStack,
    pub layers: Vec<LayerPlan>,
    pub gcode: String,
    pub stats: PrintStats,
    pub warnings: Vec<String>,
    pub timings: Timings,
    pub job: JobInfo,
}

pub fn slice(job: &SliceJob, hooks: &dyn MachineHooks) -> Result<SliceResult, SliceError> {
    job.settings.validate().map_err(SliceError::Settings)?;
    let objs: Vec<&PlateObject> = job.objects.iter().filter(|o| !o.mesh.is_empty()).collect();
    if objs.is_empty() {
        return Err(SliceError::Empty("no objects with geometry on the plate".into()));
    }
    let mut warnings = Vec::new();
    let max_z = objs.iter().map(|o| o.mesh.bounds().max.z as f64).fold(0.0, f64::max);
    let g = &job.settings;
    let stack = LayerStack::new(max_z, g.first_layer_height, g.layer_height);

    let t0 = Instant::now();
    let planes = stack.cut_planes();
    // Copies that differ only by an XY translation (20 keychains on a plate) are sliced once.
    let keys: Vec<u64> = objs.iter().map(|o| geometry_key(&o.mesh)).collect();
    let mut regions: Vec<Vec<poly::Region>> = Vec::with_capacity(objs.len());
    let mut first_of: std::collections::HashMap<u64, usize> = Default::default();
    let mut reused = 0;
    for (i, o) in objs.iter().enumerate() {
        if let Some(&j) = first_of.get(&keys[i]) {
            let (a, b) = (objs[j].mesh.bounds().min, o.mesh.bounds().min);
            let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
            let shifted = regions[j].iter().map(|r| r.iter().map(|sh| sh.iter().map(|c| c.iter().map(|p| [p[0] + dx, p[1] + dy]).collect()).collect()).collect()).collect();
            regions.push(shifted);
            reused += 1;
            continue;
        }
        let (r, open) = slicer::slice_mesh(&o.mesh, &planes);
        if open > 0 {
            warnings.push(format!("'{}': {open} open contour(s) while slicing — the mesh is not watertight; walls may be missing", o.id));
        }
        first_of.insert(keys[i], i);
        regions.push(r);
    }
    let _ = reused;
    // Bodies of one object (a multi-colour print): where they overlap, the later body wins, and
    // the object as a whole decides what is a top or bottom surface and where supports go.
    let mut covers: std::collections::HashMap<usize, Vec<poly::Region>> = Default::default();
    {
        let mut by_group: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
        for (i, o) in objs.iter().enumerate() {
            by_group.entry(o.group).or_default().push(i);
        }
        for (gid, bodies) in by_group.into_iter().filter(|(_, b)| b.len() > 1) {
            let n = stack.len();
            let mut cover: Vec<poly::Region> = vec![Vec::new(); n];
            for l in 0..n {
                let mut later: poly::Region = Vec::new();
                for &bi in bodies.iter().rev() {
                    let r = std::mem::take(&mut regions[bi][l]);
                    let clipped = if later.is_empty() { r } else { poly::difference(&r, &later) };
                    later = poly::union(&later, &clipped);
                    regions[bi][l] = clipped;
                }
                cover[l] = later;
            }
            covers.insert(gid, cover);
        }
    }
    let slice_ms = t0.elapsed().as_secs_f64() * 1e3;

    let t1 = Instant::now();
    let multi = !job.filament_settings.is_empty() && {
        let mut f: Vec<usize> = objs.iter().map(|o| o.filament).collect();
        f.sort_unstable();
        f.dedup();
        f.len() > 1
    };
    // per-object settings share the plate's layer heights and machine settings; with several
    // filaments each body takes its filament's flow limits
    let obj_settings: Vec<SliceSettings> = objs
        .iter()
        .map(|o| {
            let mut s = o.settings.clone();
            s.layer_height = g.layer_height;
            s.first_layer_height = g.first_layer_height;
            s.max_volumetric_speed = if multi { job.filament_settings.get(o.filament).map(|f| f.max_volumetric_speed).unwrap_or(g.max_volumetric_speed) } else { g.max_volumetric_speed };
            s
        })
        .collect();
    let mut seen_group: std::collections::HashSet<usize> = Default::default();
    let pobjs: Vec<planner::PlannerObject> = regions
        .iter()
        .zip(&obj_settings)
        .zip(&objs)
        .map(|((r, s), o)| planner::PlannerObject { regions: r, settings: s, cover: covers.get(&o.group).map(|c| c.as_slice()), support: seen_group.insert(o.group) })
        .collect();
    let mut layers = planner::plan(&pobjs, &stack, g);
    // the top layer can come out empty (geometry thinner than a line): don't print it
    while layers.len() > 1 && layers.last().is_some_and(|l| l.paths.is_empty()) {
        layers.pop();
    }

    // several filaments: print each layer's filaments in an order with few changes, and prime
    // the new filament on a tower after each change
    let nbodies = objs.len();
    let mut object_filament: Vec<usize> = objs.iter().map(|o| if multi { o.filament } else { 0 }).collect();
    let mut tower = None;
    let mut filament_sequence = vec![if multi { objs[0].filament } else { 0 }];
    let mut first_layer_filaments = filament_sequence.clone();
    let mut toolchanges = 0;
    let mut used_filaments: Vec<usize> = Vec::new();
    let mut layer_filaments: Vec<Vec<usize>> = Vec::new();
    if multi {
        let nf = job.filament_settings.len();
        object_filament.extend(0..nf); // tower paths: object nbodies + filament
        let fil_of = |o: usize| object_filament.get(o).copied().unwrap_or(0);
        let orders = multi::order_layers(&mut layers, &fil_of, None);
        let start = orders.iter().find_map(|o| o.first().copied());
        if let Some(spec) = &job.tower {
            let obstacles: Vec<[f64; 4]> = objs
                .iter()
                .map(|o| {
                    let b = o.mesh.bounds();
                    let m = g.brim_width.max(0.0) + g.brim_gap + 1.0;
                    [b.min.x as f64 - m, b.min.y as f64 - m, b.max.x as f64 + m, b.max.y as f64 + m]
                })
                .collect();
            let switch = |a: usize, b: usize| job.nozzles.as_ref().is_some_and(|n| n.extruder_of.get(a) != n.extruder_of.get(b));
            let first_fs = start.and_then(|f| job.filament_settings.get(f)).unwrap_or(g);
            tower = multi::build_tower(&mut layers, &fil_of, &|f| nbodies + f, start, spec, &obstacles, &switch, first_fs);
            if tower.is_none() && orders.iter().any(|o| o.len() > 1) {
                warnings.push("no room for a prime tower on the plate: the new filament primes only in the chute after each change (move objects to make room)".into());
            }
        }
        let ch = multi::changes(&layers, &fil_of, start);
        toolchanges = ch.len();
        filament_sequence = start.into_iter().chain(ch.iter().map(|c| c.to)).collect();
        first_layer_filaments = orders.first().cloned().unwrap_or_default();
        layer_filaments = orders.clone();
        for o in &orders {
            for f in o {
                if !used_filaments.contains(f) {
                    used_filaments.push(*f);
                }
            }
        }
        used_filaments.sort_unstable();
    } else {
        used_filaments.push(0);
    }
    let plan_ms = t1.elapsed().as_secs_f64() * 1e3;
    // geometry thinner than a line: one warning per object, or one for many
    let mut thin: Vec<(String, usize)> = Vec::new();
    for (oi, o) in objs.iter().enumerate() {
        let empty = layers.iter().filter(|l| !regions[oi][l.index].is_empty() && !l.paths.iter().any(|p| p.object == oi)).count();
        if empty > 0 {
            thin.push((o.name.clone(), empty));
        }
    }
    if thin.len() > 3 {
        let most = thin.iter().map(|t| t.1).max().unwrap_or(0);
        warnings.push(format!("{} bodies (e.g. '{}') have up to {most} layer(s) thinner than one line width that print nothing", thin.len(), thin[0].0));
    } else {
        for (name, n) in thin {
            warnings.push(format!("'{name}': {n} layer(s) have geometry thinner than one line width and print nothing"));
        }
    }

    let t2 = Instant::now();
    // First-layer footprint for the partial bed mesh (`G29 A1 O X Y I J`): like Bambu Studio,
    // the XY bounds of every object standing on the plate, plus every first-layer path widened
    // by half its width (brim, skirt, supports), so probing covers the whole contact area.
    let mut fb = only3d_geom::Aabb::EMPTY;
    for o in &objs {
        let b = o.mesh.bounds();
        if (b.min.z as f64) <= g.first_layer_height + 1e-3 {
            fb.extend(Vec3::new(b.min.x, b.min.y, 0.0));
            fb.extend(Vec3::new(b.max.x, b.max.y, 0.0));
        }
    }
    if let Some(l) = layers.first() {
        for path in &l.paths {
            let h = (path.width / 2.0) as f32;
            for p in &path.pts {
                fb.extend(Vec3::new(p[0] as f32 - h, p[1] as f32 - h, 0.0));
                fb.extend(Vec3::new(p[0] as f32 + h, p[1] as f32 + h, 0.0));
            }
        }
    }
    let fb = if fb.min.x <= fb.max.x { fb } else { only3d_geom::Aabb::default() };
    let info = JobInfo {
        total_layers: layers.len(),
        max_z,
        first_layer_min: [fb.min.x as f64, fb.min.y as f64],
        first_layer_size: [fb.size().x as f64, fb.size().y as f64],
        settings: g.clone(),
        bed_type: job.bed_type.clone(),
        filament_settings: if multi { job.filament_settings.clone() } else { Vec::new() },
        object_filament,
        object_group: objs.iter().map(|o| o.group).collect(),
        filament_sequence,
        first_layer_filaments,
        layer_filaments,
        used_filaments,
        toolchanges,
        tower,
        nozzles: if multi { job.nozzles.clone() } else { None },
    };
    let (gcode, stats) = gcode::emit(&layers, &info, hooks).map_err(SliceError::Gcode)?;
    let gcode_ms = t2.elapsed().as_secs_f64() * 1e3;
    Ok(SliceResult { stack, layers, gcode, stats, warnings, timings: Timings { slice_ms, plan_ms, gcode_ms }, job: info })
}

/// Hash of a mesh's shape independent of its XY position (positions relative to the bbox min,
/// quantised to 0.1 µm). Equal keys mean "same part, shifted".
pub fn geometry_key(m: &Mesh) -> u64 {
    use std::hash::{Hash, Hasher};
    let b = m.bounds().min;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    m.indices.len().hash(&mut h);
    m.positions.len().hash(&mut h);
    ((b.z * 1e4).round() as i64).hash(&mut h);
    for p in &m.positions {
        (((p.x - b.x) * 1e4).round() as i64).hash(&mut h);
        (((p.y - b.y) * 1e4).round() as i64).hash(&mut h);
        (((p.z - b.z) * 1e4).round() as i64).hash(&mut h);
    }
    m.indices.hash(&mut h);
    h.finish()
}

/// Toolpath segments for previews, grouped by feature. `upto` limits to layers 0..=upto.
pub fn preview_segments(layers: &[LayerPlan], upto: Option<usize>) -> Vec<(Feature, Vec<[Vec3; 2]>)> {
    let mut by: std::collections::BTreeMap<Feature, Vec<[Vec3; 2]>> = Default::default();
    let last = upto.unwrap_or(usize::MAX);
    for l in layers.iter().take_while(|l| l.index <= last) {
        let z = l.z as f32;
        for p in &l.paths {
            let v = by.entry(p.feature).or_default();
            let mut pts: Vec<Vec3> = p.pts.iter().map(|q| Vec3::new(q[0] as f32, q[1] as f32, z)).collect();
            if p.closed {
                pts.push(pts[0]);
            }
            for w in pts.windows(2) {
                v.push([w[0], w[1]]);
            }
        }
    }
    by.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use only3d_geom::shapes;

    /// A Klipper printer with absolute extrusion, firmware retraction and a plate centred on 0,0
    /// gets its own commands and coordinates, and nothing only Bambu printers understand.
    #[test]
    fn other_firmware_get_their_own_g_code() {
        let mut m = shapes::cuboid(Vec3::new(20.0, 20.0, 4.0)).unwrap();
        m.translate(Vec3::new(100.0, 100.0, 0.0));
        let s = SliceSettings {
            dialect: Dialect { flavor: Flavor::Klipper, bambu: false, relative_e: false, firmware_retraction: true, accel_to_decel: Some(0.5), fallback_accel: 1000.0, origin: [-100.0, -100.0] },
            ..Default::default()
        };
        let job = SliceJob { objects: vec![PlateObject::new("c", Arc::new(m), s.clone(), 0)], settings: s, bed_type: "Textured PEI Plate".into(), filament_settings: Vec::new(), tower: None, nozzles: None };
        let r = slice(&job, &GenericHooks).unwrap();
        let body: Vec<&str> = r.gcode.lines().skip_while(|l| !l.starts_with("; MACHINE_START_GCODE_END")).collect();
        let has = |p: &str| body.iter().any(|l| l.starts_with(p));
        assert!(has(";LAYER_CHANGE") && has(";Z:0.2") && has(";TYPE:Outer wall") && !has("; CHANGE_LAYER"));
        assert!(has("SET_VELOCITY_LIMIT ACCEL=") && !has("M204 S"), "Klipper sets accelerations its own way");
        assert!(has("M82 ") && has("G10 ; retract") && has("G11 ; unretract") && !has("M981"));
        // absolute E only grows while printing, and the cube sits around the printer's 0,0
        let mut last = 0.0;
        for l in body.iter().filter(|l| l.starts_with("G1 X") && l.contains(" E")) {
            let w = |k: char| l.split_whitespace().find(|t| t.starts_with(k)).unwrap()[1..].parse::<f64>().unwrap();
            assert!(w('E') >= last - 1e-9, "{l}");
            last = w('E');
            assert!(w('X').abs() <= 12.0 && w('Y').abs() <= 12.0, "{l}");
        }
        assert!(gcode::lint(&r.gcode, [-110.0, -110.0, 110.0, 110.0], 1000.0, 0.15).ok);
    }

    #[test]
    fn slices_cube_to_gcode() {
        let mut m = shapes::cuboid(Vec3::new(20.0, 20.0, 20.0)).unwrap();
        m.translate(Vec3::new(175.0, 160.0, 0.0));
        let s = SliceSettings::default();
        let job = SliceJob { objects: vec![PlateObject::new("cube", Arc::new(m), s.clone(), 0)], settings: s, bed_type: "Textured PEI Plate".into(), filament_settings: Vec::new(), tower: None, nozzles: None };
        let t = Instant::now();
        let r = slice(&job, &GenericHooks).unwrap();
        eprintln!("cube: {:?} total {:?}; {:?}", r.timings, t.elapsed(), (r.stats.print_time_s, r.stats.filament_g, r.stats.layers));
        assert_eq!(r.stats.layers, 100);
        // Bambu Studio: 3.75 g, 8m52s for the same cube/profile; we should be in the same ballpark
        assert!(r.stats.filament_g > 3.0 && r.stats.filament_g < 4.6, "{}", r.stats.filament_g);
        assert!(r.stats.print_time_s > 300.0 && r.stats.print_time_s < 900.0, "{}", r.stats.print_time_s);
        assert!(r.gcode.contains("; CHANGE_LAYER") && r.gcode.contains("; FEATURE: Outer wall") && r.gcode.contains("M73 P50"));
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        let per_object: f64 = r.stats.object_filament_g.iter().sum();
        assert!((per_object - r.stats.filament_g).abs() < 1e-6, "per-object grams add up");
        // every extrusion stays inside the cube footprint
        for line in r.gcode.lines().filter(|l| l.starts_with("G1 X") && l.contains(" E") && !l.contains("E-")) {
            let x: f64 = line.split_whitespace().find(|t| t.starts_with('X')).unwrap()[1..].parse().unwrap();
            if x > 100.0 {
                assert!((165.0..=185.0).contains(&x), "{line}");
            }
        }
    }
}
