//! Machine provider backed by Bambu Studio's own presets (installed copy, or the embedded
//! fallback) and its G-code templates, rendered with the same variables Bambu computes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use only3d_model::{EvaluatedObject, Project};
use only3d_profiles::job::{self, LayerEvent, PrintSetup, TimelapseEvent};
use only3d_profiles::{BedType, Config, ConfigValue, Context, FilamentSummary, ListFilter, MachineSummary, PresetKind, ProcessSummary, ProfileStore, Template};
use only3d_slice::{FuzzySkin, InfillPattern, JobInfo, MachineHooks, SeamPosition, SliceSettings, SupportType, WallOrder};

use crate::machine::{apply_overrides, MachineProvider, MachineSetup};
use crate::ToolError;

static SYSTEM: OnceLock<Result<Arc<ProfileStore>, String>> = OnceLock::new();
static MERGED: Mutex<Option<Arc<Presets>>> = Mutex::new(None);

/// Where a preset comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetSource {
    /// Bambu Studio's bundled presets.
    System,
    /// The user's Bambu Studio presets (~/Library/Application Support/BambuStudio/user/*).
    BambuUser,
    /// only3d's own presets (~/.only3d/presets).
    Only3d,
}

/// System presets with the user's Bambu Studio presets and only3d's own presets layered on
/// top (later layers win). Rebuilt automatically when preset files change on disk.
pub struct Presets {
    pub store: Arc<ProfileStore>,
    pub sources: std::collections::HashMap<(PresetKind, String), PresetSource>,
    pub files: std::collections::HashMap<(PresetKind, String), PathBuf>,
    stamp: u64,
}

/// only3d's own preset folder (Bambu preset JSON format).
pub fn only3d_preset_dir() -> PathBuf {
    crate::config_dir().join("presets")
}

/// Bambu Studio user preset folders (one per account, plus "default").
pub fn bambu_user_dirs() -> Vec<PathBuf> {
    let base = only3d_bambu::bambu_studio_dir().join("user");
    let mut v: Vec<PathBuf> = std::fs::read_dir(&base).map(|r| r.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default();
    v.sort();
    v
}

fn preset_files() -> Vec<(PathBuf, PresetSource, &'static str)> {
    let mut out = Vec::new();
    let mut scan = |dir: &Path, src: PresetSource| {
        for kind in ["machine", "process", "filament"] {
            if let Ok(rd) = std::fs::read_dir(dir.join(kind)) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().is_some_and(|x| x == "json") {
                        out.push((p, src, kind));
                    }
                }
            }
        }
    };
    for d in bambu_user_dirs() {
        scan(&d, PresetSource::BambuUser);
    }
    scan(&only3d_preset_dir(), PresetSource::Only3d);
    out
}

fn files_stamp(files: &[(PathBuf, PresetSource, &'static str)]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for (p, _, _) in files {
        p.hash(&mut h);
        if let Ok(m) = std::fs::metadata(p) {
            m.len().hash(&mut h);
            m.modified().ok().hash(&mut h);
        }
    }
    h.finish()
}

pub fn presets() -> Result<Arc<Presets>, ToolError> {
    let system = SYSTEM
        .get_or_init(|| ProfileStore::system().map(Arc::new).map_err(|e| e.to_string()))
        .clone()
        .map_err(|e| ToolError::new("profiles_unavailable", e, "install Bambu Studio or set ONLY3D_BAMBU_PROFILES to a BBL profile folder"))?;
    let files = preset_files();
    let stamp = files_stamp(&files);
    let mut guard = MERGED.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(p) = guard.as_ref() {
        if p.stamp == stamp {
            return Ok(p.clone());
        }
    }
    let mut store = (*system).clone();
    let mut sources = std::collections::HashMap::new();
    let mut paths = std::collections::HashMap::new();
    for (path, src, kind) in &files {
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        let rel = format!("{kind}/{}", path.file_name().and_then(|n| n.to_str()).unwrap_or("x.json"));
        if let Ok(Some(mut raw)) = only3d_profiles::RawPreset::parse(&rel, &text) {
            raw.instantiation = Some("true".into());
            sources.insert((raw.kind, raw.name.clone()), *src);
            paths.insert((raw.kind, raw.name.clone()), path.clone());
            store.insert(raw);
        }
    }
    let p = Arc::new(Presets { store: Arc::new(store), sources, files: paths, stamp });
    *guard = Some(p.clone());
    Ok(p)
}

pub fn store() -> Result<Arc<ProfileStore>, ToolError> {
    presets().map(|p| p.store.clone())
}

pub fn kind_of(kind: &str) -> Result<PresetKind, ToolError> {
    match kind {
        "machine" | "printer" => Ok(PresetKind::Machine),
        "process" | "print" => Ok(PresetKind::Process),
        "filament" => Ok(PresetKind::Filament),
        other => Err(ToolError::new("bad_kind", format!("unknown preset kind '{other}'"), "process | filament | machine")),
    }
}

fn perr(e: only3d_profiles::ProfileError) -> ToolError {
    ToolError::new("profile_error", e.to_string(), "check the preset names with list_presets")
}

/// Where printer preset `machine`'s settings come from, for the slice summary.
fn source_name(s: &ProfileStore, machine: &str) -> String {
    let vendor = s.vendor_of(machine);
    if !vendor.is_empty() && vendor != only3d_profiles::BAMBU_VENDOR {
        return format!("OrcaSlicer's {vendor} presets (embedded)");
    }
    match s.source() {
        only3d_profiles::ProfileSource::Directory(p) => format!("Bambu Studio presets at {}", p.display()),
        only3d_profiles::ProfileSource::Embedded => "embedded Bambu presets".into(),
    }
}

/// Replace every element of a (per-variant) value with `v`, keeping the array length.
fn set_all(cfg: &mut Config, key: &str, v: f64) {
    let n = cfg.len_of(key).unwrap_or(1).max(1);
    let s = if v.fract() == 0.0 { format!("{}", v as i64) } else { format!("{v}") };
    let val: ConfigValue = if cfg.get(key).is_some_and(|x| x.is_array()) { vec![s; n].into() } else { s.into() };
    cfg.insert(key, val);
}

pub struct BambuProvider;

impl MachineProvider for BambuProvider {
    fn name(&self) -> String {
        store().map(|s| source_name(&s, "")).unwrap_or_else(|e| format!("Bambu presets unavailable: {}", e.message))
    }

    fn list_presets(&self, kind: &str, query: &str, machine: &str) -> Vec<String> {
        let Ok(p) = presets() else { return Vec::new() };
        let Ok(k) = kind_of(kind) else { return Vec::new() };
        let filter = ListFilter { compatible_printer: if k == PresetKind::Machine { None } else { Some(machine) }, ..Default::default() };
        let q = query.to_ascii_lowercase();
        // the user's own presets first (they are what people pick most), then the system ones;
        // both only where they fit the printer (a preset made for the H2D isn't one for a K2)
        let fits = p.store.names(k, &filter);
        let mut names: Vec<String> = fits.iter().filter(|n| p.sources.contains_key(&(k, (*n).clone()))).cloned().collect();
        names.extend(fits.into_iter().filter(|n| !p.sources.contains_key(&(k, n.clone()))));
        names.into_iter().filter(|n| q.is_empty() || n.to_ascii_lowercase().contains(&q)).collect()
    }

    fn system_presets(&self, kind: &str, machine: &str) -> Vec<String> {
        let (Ok(p), Ok(k)) = (presets(), kind_of(kind)) else { return Vec::new() };
        let filter = ListFilter { compatible_printer: if k == PresetKind::Machine { None } else { Some(machine) }, ..Default::default() };
        p.store.names(k, &filter).into_iter().filter(|n| !p.sources.contains_key(&(k, n.clone()))).collect()
    }

    fn machine_defaults(&self, machine: &str) -> Option<(String, String)> {
        let cfg = store().ok()?.resolve(PresetKind::Machine, machine).ok()?;
        let process = cfg.get_str("default_print_profile")?.to_string();
        let filament = cfg.get_strs("default_filament_profile").and_then(|f| f.first().cloned()).or_else(|| cfg.get_str("default_filament_profile").map(str::to_string))?;
        Some((process, filament))
    }

    fn printer_model(&self, machine: &str) -> Option<String> {
        let cfg = store().ok()?.resolve(PresetKind::Machine, machine).ok()?;
        cfg.get_str("printer_model").map(str::to_string)
    }

    fn bed(&self, machine: &str) -> Option<only3d_model::Bed> {
        let cfg = store().ok()?.resolve(PresetKind::Machine, machine).ok()?;
        let ms = MachineSummary::from_config(&cfg, &[]).ok()?;
        // only3d's plate starts at its corner; the printer's coordinates may not (deltas)
        let o = ms.bed_min;
        let full = [0.0, 0.0, ms.bed_size[0] as f32, ms.bed_size[1] as f32];
        let rect = |pts: &[[f64; 2]]| {
            let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
            for p in pts {
                lo = [lo[0].min(p[0] - o[0]), lo[1].min(p[1] - o[1])];
                hi = [hi[0].max(p[0] - o[0]), hi[1].max(p[1] - o[1])];
            }
            [lo[0] as f32, lo[1] as f32, hi[0] as f32, hi[1] as f32]
        };
        let mut nozzle_areas: Vec<[f32; 4]> = ms.extruders.iter().map(|e| if e.printable_area.is_empty() { full } else { rect(&e.printable_area) }).collect();
        if nozzle_areas.is_empty() {
            nozzle_areas.push(full);
        }
        let main_nozzle = ms.master_extruder.unwrap_or(0).min(nozzle_areas.len() - 1);
        // the corner it wipes the nozzle in (X1, P1): points like "18x28"
        let pts: Vec<[f64; 2]> = match cfg.get("bed_exclude_area") {
            Some(ConfigValue::Array(l)) => l.iter().filter_map(|p| p.split_once('x')).filter_map(|(x, y)| Some([x.trim().parse().ok()?, y.trim().parse().ok()?])).collect(),
            _ => Vec::new(),
        };
        let local = |p: &[f64; 2]| [(p[0] - o[0]) as f32, (p[1] - o[1]) as f32];
        let exclude = if pts.len() >= 3 { vec![pts.iter().map(local).collect::<Vec<_>>()] } else { Vec::new() };
        // a printable area that isn't its own bounding rectangle (a delta's circle)
        let area = cfg.get_points("printable_area").unwrap_or_default();
        let boxy = area.len() == 4 && area.iter().all(|p| (p[0] - o[0]).abs() < 1e-6 || (p[0] - o[0] - ms.bed_size[0]).abs() < 1e-6);
        let shape = if area.len() >= 3 && !boxy { area.iter().map(local).collect() } else { Vec::new() };
        Some(only3d_model::Bed { size: [full[2], full[3]], max_height: ms.max_print_height as f32, nozzle_areas, main_nozzle, exclude, origin: [o[0] as f32, o[1] as f32], shape })
    }

    fn setup(&self, project: &Project, objects: &[EvaluatedObject]) -> Result<MachineSetup, ToolError> {
        let st = store()?;
        let mut process = st.resolve(PresetKind::Process, &project.process.preset).map_err(perr)?;
        let mut used: Vec<u32> = objects.iter().filter(|o| !o.hidden).flat_map(|o| o.filaments()).collect();
        used.extend(interface_filaments(project, objects, &process));
        used.sort_unstable();
        used.dedup();
        if used.len() > 1 {
            return setup_multi(project, objects, &used);
        }
        let machine = st.resolve(PresetKind::Machine, &project.printer.machine).map_err(perr)?;
        let slot = objects.iter().find(|o| !o.hidden).map(|o| o.filament).unwrap_or(1);
        let fil_sel = project.filaments.get(slot as usize - 1).ok_or_else(|| ToolError::new("bad_filament_slot", format!("filament slot {slot} does not exist"), "set_filament to add it"))?;
        let mut fil = st.resolve(PresetKind::Filament, &fil_sel.preset).map_err(perr)?;
        // the linked spool's own calibration (flow, pressure advance, max volumetric speed)
        crate::spools::slot_calibration(fil_sel.spool.as_deref()).apply(&mut fil);
        let master = MachineSummary::from_config(&machine, &[]).map_err(perr)?.master_extruder.unwrap_or(0);
        let areas = self.bed(&project.printer.machine).map(|b| b.nozzle_areas).unwrap_or_default();
        let extruder = crate::machine::choose_extruder_on(project, objects, &areas, master)? as usize;
        let bed = BedType::from_name(&project.printer.bed_type).ok_or_else(|| ToolError::new("bad_bed_type", format!("unknown bed type '{}'", project.printer.bed_type), "set_printer bed_type"))?;

        let (s, fs) = base_settings(&machine, &process, &fil, extruder, bed, objects)?;
        let mut notes = Vec::new();
        let (g, per) = apply_overrides(&s, project, objects, &mut notes)?;
        // keep the templates consistent with overridden temperatures and heights
        set_all(&mut fil, "nozzle_temperature", g.nozzle_temp);
        set_all(&mut fil, "nozzle_temperature_initial_layer", g.first_layer_nozzle_temp);
        set_all(&mut fil, bed.temp_key(), g.bed_temp);
        set_all(&mut fil, bed.initial_layer_temp_key(), g.first_layer_bed_temp);
        set_all(&mut process, "layer_height", g.layer_height);
        set_all(&mut process, "initial_layer_print_height", g.first_layer_height);

        // The full config, as Bambu Studio builds it: code defaults, then machine, process (with
        // this project's overrides mapped to Bambu keys) and filament, then the per-job keys. It is
        // written to project_settings.config and, serialized like Bambu Studio, to the G-code
        // CONFIG block, which the printer reads (the H2D checks its nozzles against it).
        let mut project_settings = only3d_profiles::gcode_config::bambu_code_defaults();
        project_settings.merge(&machine);
        let mut process_cfg = process.clone();
        let mut fil_cfg = vec![fil.clone()];
        let _ = crate::bambu_export::map_overrides(&project.process.overrides, &mut process_cfg, &mut fil_cfg);
        project_settings.merge(&process_cfg);
        project_settings.merge(&fil_cfg[0]);
        project_settings.insert("curr_bed_type", project.printer.bed_type.clone());
        project_settings.insert("printer_settings_id", project.printer.machine.clone());
        project_settings.insert("print_settings_id", project.process.preset.clone());
        project_settings.insert("filament_settings_id", vec![fil_sel.preset.clone()]);
        project_settings.insert("filament_colour", vec![fil_sel.color.clone()]);
        job_keys(&mut project_settings, &machine, &process, &fil, extruder, fil_sel.nozzle == only3d_model::NozzleSel::Auto);
        let ext_variants: Vec<String> = (0..machine.len_of("nozzle_diameter").unwrap_or(1).max(1))
            .map(|e| format!("{} {}", machine.get_str_at("extruder_type", e).unwrap_or("Direct Drive"), machine.get_str_at("default_nozzle_volume_type", e).unwrap_or("Standard")))
            .collect();
        only3d_profiles::gcode_config::collapse_variants(&mut project_settings, &machine, &process_cfg, &fil_cfg[0], &ext_variants, extruder);

        Ok(MachineSetup {
            settings: g,
            object_settings: per,
            hooks: Box::new(BambuHooks { machine, process, filaments: vec![(fil, extruder)], colors: vec![fil_sel.color.clone()], bed, ctx: OnceLock::new(), full: project_settings.clone(), globals: Default::default() }),
            source: format!("{} ({}, {}, {})", source_name(&st, &project.printer.machine), project.printer.machine, project.process.preset, fil_sel.preset),
            filament_type: fs.filament_type.clone(),
            filament_color: fil_sel.color.clone(),
            filament_id: fs.filament_id.clone().unwrap_or_default(),
            notes,
            project_settings: Some(serde_json::to_string_pretty(&project_settings.to_json()).unwrap_or_default()),
            printer_safe: true,
            filament_settings: Vec::new(),
            tower: None,
            nozzles: None,
            filaments: vec![crate::machine::FilamentInfo { kind: fs.filament_type.clone(), color: fil_sel.color.clone(), id: fs.filament_id.clone().unwrap_or_default(), extruder }],
        })
    }
}

/// Which nozzle prints each project filament on a plate with several (H2D: 0 = left,
/// 1 = right). A filament set to a nozzle keeps it; otherwise its spool decides (the left
/// holder feeds the left nozzle; the right holder and the AMS the right one); the rest are
/// spread over both nozzles, so changes need no flushing. Each body must lie in its nozzle's
/// reach.
pub(crate) fn assign_extruders(project: &Project, objects: &[EvaluatedObject], used: &[u32]) -> Result<Vec<usize>, ToolError> {
    use only3d_model::NozzleSel;
    let nf = project.filaments.len();
    let mut ext: Vec<Option<usize>> = vec![None; nf];
    for (i, f) in project.filaments.iter().enumerate() {
        ext[i] = match f.nozzle {
            NozzleSel::Left => Some(0),
            NozzleSel::Right => Some(1),
            NozzleSel::Auto if !cfg!(test) => crate::spools::spool_for_slot(project, i).and_then(|sp| sp.location).and_then(|l| match l.as_str() {
                "ext:left" => Some(0),
                "ext:right" => Some(1),
                l if l.starts_with("ams") => Some(1),
                _ => None,
            }),
            NozzleSel::Auto => None,
        };
    }
    // the rest: alternate, starting with whichever nozzle has fewer of the plate's filaments
    for &u in used {
        let i = u as usize - 1;
        if i < nf && ext[i].is_none() {
            let on = |e: usize| used.iter().filter(|x| ext.get(**x as usize - 1).copied().flatten() == Some(e)).count();
            ext[i] = Some(if on(1) <= on(0) { 1 } else { 0 });
        }
    }
    let ext: Vec<usize> = ext.into_iter().map(|e| e.unwrap_or(1)).collect();
    // reach: the left nozzle prints x 0–325, the right x 25–350
    for o in objects.iter().filter(|o| !o.hidden) {
        let bodies: Vec<(u32, only3d_geom::Aabb)> = if o.parts.is_empty() { vec![(o.filament, o.report.bounds)] } else { o.parts.iter().map(|p| (p.filament, p.mesh.bounds())).collect() };
        for (f, b) in bodies {
            let e = ext.get(f as usize - 1).copied().unwrap_or(1);
            let (lo, hi) = if e == 0 { (0.0, 325.0) } else { (25.0, 350.0) };
            if b.min.x < lo - 0.01 || b.max.x > hi + 0.01 {
                return Err(ToolError::new(
                    "out_of_nozzle_reach",
                    format!("'{}' prints with filament {f} on the {} nozzle, which reaches x {lo:.0}–{hi:.0} mm; it spans x {:.1}–{:.1}", o.id, if e == 0 { "left" } else { "right" }, b.min.x, b.max.x),
                    "move it, or set that filament's nozzle (set_filament nozzle left/right)",
                ));
            }
        }
    }
    Ok(ext)
}

/// A plate with several filaments: each filament's settings on its nozzle, the full config
/// with every filament, the prime tower and the nozzle facts the G-code needs.
fn setup_multi(project: &Project, objects: &[EvaluatedObject], used: &[u32]) -> Result<MachineSetup, ToolError> {
    let st = store()?;
    let machine = st.resolve(PresetKind::Machine, &project.printer.machine).map_err(perr)?;
    let mut process = st.resolve(PresetKind::Process, &project.process.preset).map_err(perr)?;
    let bed = BedType::from_name(&project.printer.bed_type).ok_or_else(|| ToolError::new("bad_bed_type", format!("unknown bed type '{}'", project.printer.bed_type), "set_printer bed_type"))?;
    if let Some(u) = used.iter().find(|u| **u as usize > project.filaments.len() || **u == 0) {
        return Err(ToolError::new("bad_filament_slot", format!("filament slot {u} does not exist"), "set_filament to add it"));
    }
    let mut fils: Vec<Config> = Vec::new();
    for f in &project.filaments {
        let mut c = st.resolve(PresetKind::Filament, &f.preset).map_err(perr)?;
        crate::spools::slot_calibration(f.spool.as_deref()).apply(&mut c);
        fils.push(c);
    }
    let extruders = assign_extruders(project, objects, used)?;
    // the plate's own settings come with the first filament it prints
    let first = objects.iter().find(|o| !o.hidden).and_then(|o| o.filaments().first().copied()).unwrap_or(used[0]) as usize - 1;
    let (s0, fs0) = base_settings(&machine, &process, &fils[first], extruders[first], bed, objects)?;
    let mut notes = Vec::new();
    let (mut g, per) = apply_overrides(&s0, project, objects, &mut notes)?;
    let mut fsets = Vec::new();
    let mut infos = Vec::new();
    for (i, c) in fils.iter().enumerate() {
        let (sf, fsum) = base_settings(&machine, &process, c, extruders[i], bed, objects)?;
        let (gf, _) = apply_overrides(&sf, project, &[], &mut Vec::new())?;
        infos.push(crate::machine::FilamentInfo { kind: fsum.filament_type.clone(), color: project.filaments[i].color.clone(), id: fsum.filament_id.clone().unwrap_or_default(), extruder: extruders[i] });
        fsets.push(gf);
    }
    // the bed suits every filament on it: the hottest of their bed temperatures
    for u in used {
        let f = &fsets[*u as usize - 1];
        g.bed_temp = g.bed_temp.max(f.bed_temp);
        g.first_layer_bed_temp = g.first_layer_bed_temp.max(f.first_layer_bed_temp);
    }
    for f in fsets.iter_mut() {
        f.bed_temp = g.bed_temp;
        f.first_layer_bed_temp = g.first_layer_bed_temp;
    }
    g.extruder = extruders[first] as u32;
    for (i, c) in fils.iter_mut().enumerate() {
        set_all(c, "nozzle_temperature", fsets[i].nozzle_temp);
        set_all(c, "nozzle_temperature_initial_layer", fsets[i].first_layer_nozzle_temp);
        set_all(c, bed.temp_key(), g.bed_temp);
        set_all(c, bed.initial_layer_temp_key(), g.first_layer_bed_temp);
    }
    set_all(&mut process, "layer_height", g.layer_height);
    set_all(&mut process, "initial_layer_print_height", g.first_layer_height);

    // the full config with every filament (one value per filament in each filament option)
    let mut project_settings = only3d_profiles::gcode_config::bambu_code_defaults();
    project_settings.merge(&machine);
    let mut process_cfg = process.clone();
    let mut fil_cfgs = fils.clone();
    let _ = crate::bambu_export::map_overrides(&project.process.overrides, &mut process_cfg, &mut fil_cfgs);
    if !interface_filaments(project, objects, &process).is_empty() {
        // a roof in another filament touches the part with solid lines (the slicer's rule)
        set_all(&mut process_cfg, "support_top_z_distance", 0.0);
        set_all(&mut process_cfg, "support_interface_spacing", 0.0);
    }
    project_settings.merge(&process_cfg);
    project_settings.merge(&fil_cfgs[first]);
    let ext_variants: Vec<String> = (0..machine.len_of("nozzle_diameter").unwrap_or(1).max(1))
        .map(|e| format!("{} {}", machine.get_str_at("extruder_type", e).unwrap_or("Direct Drive"), machine.get_str_at("default_nozzle_volume_type", e).unwrap_or("Standard")))
        .collect();
    only3d_profiles::gcode_config::collapse_variants(&mut project_settings, &machine, &process_cfg, &fil_cfgs[first], &ext_variants, extruders[first]);
    let pairs: Vec<(&Config, usize)> = fil_cfgs.iter().zip(&extruders).map(|(c, e)| (c, *e)).collect();
    only3d_profiles::gcode_config::merge_filaments(&mut project_settings, &pairs, &ext_variants);
    project_settings.insert("curr_bed_type", project.printer.bed_type.clone());
    project_settings.insert("printer_settings_id", project.printer.machine.clone());
    project_settings.insert("print_settings_id", project.process.preset.clone());
    project_settings.insert("filament_settings_id", project.filaments.iter().map(|f| f.preset.clone()).collect::<Vec<_>>());
    project_settings.insert("filament_colour", project.filaments.iter().map(|f| f.color.clone()).collect::<Vec<_>>());
    let auto = project.filaments.iter().any(|f| f.nozzle == only3d_model::NozzleSel::Auto);
    job_keys_multi(&mut project_settings, &machine, &process, &fil_cfgs, &extruders, &project.filaments, auto);

    // the tower: front centre of the area both nozzles reach, out of the objects' way
    let w = machine.get_f64_at("nozzle_diameter", 0).unwrap_or(0.4) * 1.25;
    let tower = only3d_slice::TowerSpec {
        x: 175.0 - 35.0 / 2.0,
        y: 15.0,
        width: 35.0,
        line_width: w,
        prime_mm3: fils.iter().map(|c| c.get_f64_at("filament_prime_volume", 0).unwrap_or(30.0)).fold(15.0, f64::max),
        prime_nozzle_change_mm3: fils.iter().map(|c| c.get_f64_at("filament_prime_volume_nc", 0).unwrap_or(60.0)).fold(30.0, f64::max),
        area: [25.0, 0.0, 325.0, 320.0],
        speed: process.get_f64("prime_tower_max_speed").filter(|v| *v > 0.0).unwrap_or(90.0),
    };
    let heat = machine.get_f64_at("hotend_heating_rate", 0).unwrap_or(3.6);
    let cool = machine.get_f64_at("hotend_cooling_rate", 0).unwrap_or(2.0);
    let n_ext = machine.len_of("nozzle_diameter").unwrap_or(1).max(1);
    let physical: Vec<usize> = (0..n_ext).map(|e| machine.get_i64_at("physical_extruder_map", e).map(|v| v.max(0) as usize).unwrap_or(e)).collect();
    let nozzles = only3d_slice::NozzleSetup {
        extruder_of: extruders.clone(),
        physical,
        heat_rate: heat,
        cool_rate: cool,
        preheat_delta: fils.get(first).and_then(|c| c.get_f64_at("filament_preheat_temperature_delta", 0)).unwrap_or(10.0),
        switch_time_s: machine.get_f64("machine_switch_extruder_time").unwrap_or(5.6) + 14.0,
        change_time_s: machine.get_f64("machine_load_filament_time").unwrap_or(26.0) + machine.get_f64("machine_unload_filament_time").unwrap_or(26.0),
    };
    let names: Vec<String> = used.iter().map(|u| format!("{u}: {}", short_preset(&project.filaments[*u as usize - 1].preset))).collect();
    let colors: Vec<String> = project.filaments.iter().map(|f| f.color.clone()).collect();
    Ok(MachineSetup {
        settings: g,
        object_settings: per,
        hooks: Box::new(BambuHooks { machine, process, filaments: fils.into_iter().zip(extruders.iter().copied()).collect(), colors, bed, ctx: OnceLock::new(), full: project_settings.clone(), globals: Default::default() }),
        source: format!("{} ({}, {}, filaments {})", source_name(&st, &project.printer.machine), project.printer.machine, project.process.preset, names.join(", ")),
        filament_type: fs0.filament_type.clone(),
        filament_color: project.filaments[first].color.clone(),
        filament_id: fs0.filament_id.clone().unwrap_or_default(),
        notes,
        project_settings: Some(serde_json::to_string_pretty(&project_settings.to_json()).unwrap_or_default()),
        printer_safe: true,
        filament_settings: fsets,
        tower: Some(tower),
        nozzles: Some(nozzles),
        filaments: infos,
    })
}

/// Filament slots supports print their roofs in, where that isn't the part's own filament (from
/// the object's settings, the project's, or the process preset).
fn interface_filaments(project: &Project, objects: &[EvaluatedObject], process: &Config) -> Vec<u32> {
    let get = |o: &EvaluatedObject, k: &str| project.object(&o.id).and_then(|ob| ob.settings.get(k).cloned()).or_else(|| project.process.overrides.get(k).cloned());
    objects
        .iter()
        .filter(|o| !o.hidden)
        .filter_map(|o| {
            let on = get(o, "support").and_then(|v| v.as_bool()).unwrap_or_else(|| process.get_bool("enable_support").unwrap_or(false));
            let slot = get(o, "support_interface_filament").and_then(|v| v.as_u64()).unwrap_or_else(|| process.get_i64("support_interface_filament").unwrap_or(0).max(0) as u64) as u32;
            (on && slot > 0 && !o.filaments().contains(&slot)).then_some(slot)
        })
        .collect()
}

fn short_preset(p: &str) -> String {
    p.split(" @").next().unwrap_or(p).to_string()
}

/// `job_keys` for a plate with several filaments: the filament → nozzle map, per-filament
/// indices and ids, and the flush matrix for each nozzle.
fn job_keys_multi(cfg: &mut Config, machine: &Config, process: &Config, fils: &[Config], extruders: &[usize], slots: &[only3d_model::FilamentSlot], auto: bool) {
    let n = machine.len_of("nozzle_diameter").unwrap_or(1).max(1);
    let nf = fils.len();
    let diam: Vec<String> = (0..n).map(|i| machine.get_str_at("nozzle_diameter", i).unwrap_or("0.4").to_string()).collect();
    let flow: Vec<String> = (0..n).map(|i| machine.get_str_at("default_nozzle_volume_type", i).unwrap_or("Standard").to_string()).collect();
    cfg.insert("filament_map", extruders.iter().map(|e| (e + 1).to_string()).collect::<Vec<_>>());
    cfg.insert("filament_map_2", extruders.iter().map(|e| e.to_string()).collect::<Vec<_>>());
    cfg.insert("filament_nozzle_map", extruders.iter().map(|e| e.to_string()).collect::<Vec<_>>());
    cfg.insert("filament_map_mode", if auto { "Auto For Flush" } else { "Manual" });
    cfg.insert("filament_self_index", (1..=nf).map(|i| i.to_string()).collect::<Vec<_>>());
    cfg.insert("filament_ids", fils.iter().map(|f| f.get_str("filament_id").unwrap_or("").to_string()).collect::<Vec<_>>());
    cfg.insert("nozzle_volume_type", flow.clone());
    cfg.insert("extruder_nozzle_volume_type", flow.clone());
    cfg.insert("extruder_nozzle_stats", flow.iter().map(|f| format!("{f}#1")).collect::<Vec<_>>());
    cfg.insert("extruder_nozzle_stats_new", flow.iter().map(|f| format!("{f}#1")).collect::<Vec<_>>());
    cfg.insert("extruder_nozzle_count", vec!["1"; n]);
    cfg.insert("extruder_nozzle_diameter", diam);
    cfg.insert("extruder_nozzle_slot", (0..n).map(|i| i.to_string()).collect::<Vec<_>>());
    cfg.insert("multi_nozzle_diameter_mode", "0");
    cfg.insert("filament_multi_colour", slots.iter().map(|f| f.color.clone()).collect::<Vec<_>>());
    cfg.insert("filament_colour_type", vec!["0"; nf]);
    // one nf × nf matrix per nozzle: what to purge between two colours in it
    let mut flush = Vec::new();
    for _ in 0..n {
        for a in slots {
            for b in slots {
                flush.push(if std::ptr::eq(a, b) { "0".to_string() } else { format!("{:.0}", only3d_slice::multi::flush_volume(&a.color, &b.color)) });
            }
        }
    }
    cfg.insert("flush_volumes_matrix", flush);
    cfg.insert("enable_prime_tower", "1");
    cfg.insert("different_settings_to_system", vec![""; nf + 2]);
    cfg.insert("inherits_group", vec![""; nf + 2]);
    cfg.insert("compatible_machine_expression_group", vec![""; nf + 2]);
    if let Some(cp) = process.get_strs("compatible_printers") {
        cfg.insert("print_compatible_printers", cp.to_vec());
    }
}

/// Slicer settings for printing with filament preset `fil` on `extruder` (H2D: 0 = left,
/// 1 = right), from the machine and process presets. Also the filament's summary.
/// How the printer wants its G-code written, from its profile (OrcaSlicer's defaults where a
/// profile leaves a key out). Bambu Lab printers keep Bambu Studio's own form.
fn dialect(machine: &Config, process: &Config) -> only3d_slice::Dialect {
    let flavor = only3d_slice::Flavor::from_profile(machine.get_str("gcode_flavor").unwrap_or("marlin"));
    let bambu = machine.get_str("printer_model").is_some_and(|m| m.starts_with("Bambu Lab"));
    let klipper_ratio = flavor == only3d_slice::Flavor::Klipper && process.get_bool("accel_to_decel_enable").unwrap_or(true);
    only3d_slice::Dialect {
        flavor,
        bambu,
        relative_e: bambu || machine.get_bool("use_relative_e_distances").unwrap_or(true),
        firmware_retraction: !bambu && machine.get_bool("use_firmware_retraction").unwrap_or(false),
        accel_to_decel: (!bambu && klipper_ratio).then(|| process.get_percent("accel_to_decel_factor").unwrap_or(0.5)),
        fallback_accel: machine.get_f64_at("machine_max_acceleration_extruding", 0).filter(|a| *a > 0.0).unwrap_or(1000.0),
        origin: plate_origin(machine),
    }
}

/// The printer coordinates of the plate's corner (the printable area's lowest X and Y; a
/// delta's round bed is centred on 0,0).
fn plate_origin(machine: &Config) -> [f64; 2] {
    let pts = machine.get_points("printable_area").unwrap_or_default();
    if pts.is_empty() {
        return [0.0, 0.0];
    }
    [pts.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min), pts.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min)]
}

fn base_settings(machine: &Config, process: &Config, fil: &Config, extruder: usize, bed: BedType, objects: &[EvaluatedObject]) -> Result<(SliceSettings, FilamentSummary), ToolError> {
    let variants = ["Direct Drive Standard", "Direct Drive Standard"];
    let ms = MachineSummary::from_config(machine, &variants).map_err(perr)?;
    let ext = ms.extruders.get(extruder).ok_or_else(|| ToolError::new("bad_extruder", format!("machine has no extruder {extruder}"), ""))?;
    let ps = ProcessSummary::from_config(process, extruder, &ext.variant, ext.nozzle_diameter).map_err(perr)?;
    let fs = FilamentSummary::from_config(fil, &ext.variant).map_err(perr)?;
    let plate = fs.plate_temps(bed);

    let mrow = ext.variant_row;
    let m = |k: &str, d: f64| machine.get_f64_at(k, mrow).unwrap_or(d);
    // tree support keys (Bambu Studio's defaults where a preset leaves one out)
    let tree = |k: &str, d: f64| process.get_f64_at(k, ps.variant_row).or_else(|| process.get_f64(k)).filter(|v| *v > 0.0).unwrap_or(d);
    let lw = &ps.line_widths;
    let sp = &ps.speeds;
    let ac = &ps.accelerations;
    let mut s = SliceSettings {
        layer_height: ps.layer_height,
        first_layer_height: ps.initial_layer_height,
        nozzle_diameter: ext.nozzle_diameter,
        filament_diameter: fs.diameter,
        outer_wall_width: lw.outer_wall,
        inner_wall_width: lw.inner_wall,
        infill_width: lw.sparse_infill,
        solid_infill_width: lw.internal_solid_infill,
        top_surface_width: lw.top_surface,
        first_layer_width: lw.initial_layer,
        wall_loops: ps.wall_loops.max(1) as u32,
        top_layers: ps.top_shell_layers.max(0) as u32,
        bottom_layers: ps.bottom_shell_layers.max(0) as u32,
        infill_density: ps.sparse_infill_density / 100.0,
        infill_pattern: match ps.sparse_infill_pattern.as_str() {
            "concentric" => InfillPattern::Concentric,
            "line" | "zig-zag" | "monotonic" | "monotonicline" | "alignedrectilinear" => InfillPattern::Rectilinear,
            _ => InfillPattern::Grid,
        },
        infill_angle: ps.infill_direction,
        infill_wall_overlap: ps.infill_wall_overlap,
        seam: if ps.seam_position == "nearest" { SeamPosition::Nearest } else { SeamPosition::Rear },
        wall_order: if ps.wall_infill_order.contains("outer wall/inner wall") { WallOrder::OuterInner } else { WallOrder::InnerOuter },
        brim_width: if ps.brim.brim_type == "no_brim" { 0.0 } else if ps.brim.brim_type == "auto_brim" { auto_brim(objects) } else { ps.brim.width },
        brim_gap: ps.brim.object_gap,
        skirt_loops: ps.brim.skirt_loops.max(0) as u32,
        skirt_distance: if ps.brim.skirt_distance > 0.0 { ps.brim.skirt_distance } else { 3.0 },
        ironing: ps.ironing_type != "no ironing" && !ps.ironing_type.is_empty(),
        fuzzy_skin: match ps.fuzzy_skin.as_str() {
            "external" => FuzzySkin::External,
            "all" => FuzzySkin::All,
            "allwalls" => FuzzySkin::AllWalls,
            _ => FuzzySkin::None,
        },
        fuzzy_skin_thickness: ps.fuzzy_skin_thickness,
        fuzzy_skin_point_distance: if ps.fuzzy_skin_point_distance > 0.0 { ps.fuzzy_skin_point_distance } else { 0.8 },
        fuzzy_skin_first_layer: ps.fuzzy_skin_first_layer,
        support: ps.support.enabled,
        support_type: if ps.support.support_type.starts_with("tree") { SupportType::Tree } else { SupportType::Normal },
        support_angle: if ps.support.threshold_angle > 0.0 { ps.support.threshold_angle } else { 30.0 },
        support_spacing: if ps.support.base_pattern_spacing > 0.0 { ps.support.base_pattern_spacing + lw.default } else { 2.5 },
        support_interface_layers: ps.support.interface_top_layers.max(0) as u32,
        support_interface_spacing: if ps.support.interface_spacing > 0.0 { ps.support.interface_spacing + lw.default } else { 0.5 },
        support_xy_gap: if ps.support.object_xy_distance > 0.0 { ps.support.object_xy_distance } else { 0.35 },
        support_z_gap_layers: ((ps.support.top_z_distance / ps.layer_height.max(0.05)).round() as u32).max(1),
        support_on_plate_only: ps.support.on_build_plate_only,
        support_speed: if sp.support > 0.0 { sp.support } else { 150.0 },
        support_interface_speed: if sp.support_interface > 0.0 { sp.support_interface } else { 80.0 },
        support_interface_filament: ps.support.interface_filament.max(0) as u32,
        tree_branch_angle: tree("tree_support_branch_angle", 40.0),
        tree_branch_diameter: tree("tree_support_branch_diameter", 5.0),
        tree_branch_diameter_angle: tree("tree_support_branch_diameter_angle", 5.0),
        tree_tip_diameter: tree("tree_support_tip_diameter", 0.8),
        tree_branch_distance: tree("tree_support_branch_distance", 5.0),
        outer_wall_speed: sp.outer_wall,
        inner_wall_speed: sp.inner_wall,
        sparse_infill_speed: sp.sparse_infill,
        solid_infill_speed: sp.internal_solid_infill,
        top_surface_speed: sp.top_surface,
        bridge_speed: sp.bridge,
        first_layer_speed: sp.initial_layer,
        first_layer_infill_speed: sp.initial_layer_infill,
        travel_speed: sp.travel,
        default_accel: ac.default,
        outer_wall_accel: ac.outer_wall,
        top_surface_accel: ac.top_surface,
        first_layer_accel: ac.initial_layer,
        travel_accel: ac.travel,
        first_layer_travel_accel: if ac.initial_layer_travel > 0.0 { ac.initial_layer_travel } else { ac.travel },
        short_travel_accel: ac.travel_short_distance.max(0.0),
        filament_type: fs.filament_type.clone(),
        filament_density: fs.density,
        filament_cost_per_kg: fs.cost,
        flow_ratio: fs.flow_ratio,
        max_volumetric_speed: fs.max_volumetric_speed,
        nozzle_temp: fs.nozzle_temperature,
        first_layer_nozzle_temp: fs.nozzle_temperature_initial_layer,
        bed_temp: plate.other_layers,
        first_layer_bed_temp: plate.initial_layer,
        fan_min: fs.fan.min_speed / 100.0,
        fan_max: fs.fan.max_speed / 100.0,
        fan_full_below_layer_time: fs.fan.fan_cooling_layer_time,
        aux_fan: fs.fan.additional_cooling_fan_speed / 100.0,
        // Bambu's collect_aux_fan_slots: the side fan (P2) with `auxiliary_fan`, plus the
        // optional one (P10) when it is independent or synced to the second fan
        aux_fan_slots: if machine.get_bool("auxiliary_fan").unwrap_or(false) {
            let optional = machine.get_bool("aux_fan_independent_control").unwrap_or(false) || machine.get_bool("sync_auxiliary_fan_to_second_fan").unwrap_or(false);
            if optional { vec![2, 10] } else { vec![2] }
        } else {
            Vec::new()
        },
        start_part_fan: (fs.fan.close_fan_the_first_x_layers > 0).then(|| fil.get_f64_at("first_x_layer_part_fan_speed", 0).unwrap_or(0.0) / 100.0),
        fan_off_first_layers: fs.fan.close_fan_the_first_x_layers.max(0) as u32,
        min_layer_time: if fs.fan.slow_down_for_layer_cooling { fs.fan.slow_down_layer_time } else { 0.0 },
        min_print_speed: fs.fan.slow_down_min_speed,
        retract_length: fs.retraction.length.unwrap_or_else(|| m("retraction_length", 0.8)),
        retract_speed: fs.retraction.speed.unwrap_or_else(|| m("retraction_speed", 30.0)),
        deretract_speed: fs.retraction.deretraction_speed.unwrap_or_else(|| m("deretraction_speed", 30.0)),
        retract_min_travel: fs.retraction.minimum_travel.unwrap_or_else(|| m("retraction_minimum_travel", 1.0)),
        z_hop: fs.retraction.z_hop.unwrap_or_else(|| m("z_hop", 0.4)),
        wipe_distance: fs.retraction.wipe_distance.unwrap_or_else(|| m("wipe_distance", 1.0)),
        extruder: extruder as u32,
        dialect: dialect(machine, process),
        support_area: None,
    };
    if s.travel_speed <= 0.0 {
        s.travel_speed = 500.0;
    }
    // a zero means "the same as" here (Bambu Studio and OrcaSlicer)
    if s.deretract_speed <= 0.0 {
        s.deretract_speed = s.retract_speed;
    }
    if s.first_layer_speed <= 0.0 {
        s.first_layer_speed = 30.0;
    }
    if s.first_layer_infill_speed <= 0.0 {
        s.first_layer_infill_speed = s.first_layer_speed;
    }
    if !s.dialect.bambu {
        // OrcaSlicer's accelerations: with no default it never sets one; a feature's 0 means the
        // default; a travel's 0 leaves the current one; a short hop onto an outer wall uses the
        // outer wall's (Bambu has a separate value)
        if ac.default <= 0.0 {
            (s.default_accel, s.outer_wall_accel, s.top_surface_accel, s.first_layer_accel) = (0.0, 0.0, 0.0, 0.0);
            (s.travel_accel, s.first_layer_travel_accel, s.short_travel_accel) = (0.0, 0.0, 0.0);
        } else {
            for a in [&mut s.outer_wall_accel, &mut s.top_surface_accel, &mut s.first_layer_accel] {
                if *a <= 0.0 {
                    *a = ac.default;
                }
            }
            s.travel_accel = ac.travel.max(0.0);
            s.first_layer_travel_accel = ac.initial_layer_travel.max(0.0);
            s.short_travel_accel = ac.outer_wall.max(0.0);
        }
    }
    if !s.dialect.bambu {
        // OrcaSlicer's rule off Bambu printers: the side fan is P2 and nothing else, and when
        // the filament keeps the fans off for the first layers both start at 0
        let aux = machine.get_bool("auxiliary_fan").unwrap_or(false);
        s.aux_fan_slots = if aux { vec![2] } else { Vec::new() };
        s.start_part_fan = (aux && s.fan_off_first_layers > 0).then_some(0.0);
    }
    Ok((s, fs))
}

/// Bambu's "auto brim": a brim for tall parts on small footprints.
fn auto_brim(objects: &[EvaluatedObject]) -> f64 {
    let risky = objects.iter().filter(|o| !o.hidden).any(|o| {
        let contact = o.mesh.bed_contact_area().max(1.0);
        let h = o.report.size.z as f64;
        h > 4.0 * contact.sqrt() || contact < 100.0
    });
    if risky {
        5.0
    } else {
        0.0
    }
}

/// The keys Bambu Studio sets per job rather than from presets (one filament on `extruder`):
/// filament-to-extruder mapping, nozzle flow types and counts per extruder, and the like.
fn job_keys(cfg: &mut Config, machine: &Config, process: &Config, fil: &Config, extruder: usize, auto: bool) {
    let n = machine.len_of("nozzle_diameter").unwrap_or(1).max(1);
    let diam: Vec<String> = (0..n).map(|i| machine.get_str_at("nozzle_diameter", i).unwrap_or("0.4").to_string()).collect();
    let flow: Vec<String> = (0..n).map(|i| machine.get_str_at("default_nozzle_volume_type", i).unwrap_or("Standard").to_string()).collect();
    cfg.insert("filament_map", vec![(extruder + 1).to_string()]);
    // Bambu Studio writes 1 for these on a two-nozzle machine and 0 on a one-nozzle one
    let map = if n > 1 { "1" } else { "0" };
    cfg.insert("filament_map_2", vec![map]);
    cfg.insert("filament_nozzle_map", vec![map]);
    // per-nozzle values (code defaults captured on the two-nozzle H2D): one per nozzle
    for key in ["flush_multiplier", "flush_multiplier_fast"] {
        if let Some(v) = cfg.get_strs(key).map(|v| v.to_vec()) {
            let first = v.first().cloned().unwrap_or_else(|| "1".into());
            cfg.insert(key, (0..n).map(|i| v.get(i).cloned().unwrap_or_else(|| first.clone())).collect::<Vec<_>>());
        }
    }
    cfg.insert("filament_map_mode", if auto { "Auto For Flush" } else { "Manual" });
    cfg.insert("filament_self_index", vec!["1"]);
    if let Some(id) = fil.get_str("filament_id") {
        cfg.insert("filament_ids", vec![id.to_string()]);
    }
    cfg.insert("nozzle_volume_type", flow.clone());
    cfg.insert("extruder_nozzle_volume_type", flow.clone());
    cfg.insert("extruder_nozzle_stats", flow.iter().map(|f| format!("{f}#1")).collect::<Vec<_>>());
    cfg.insert("extruder_nozzle_count", vec!["1"; n]);
    cfg.insert("extruder_nozzle_diameter", diam);
    cfg.insert("extruder_nozzle_slot", (0..n).map(|i| i.to_string()).collect::<Vec<_>>());
    cfg.insert("flush_volumes_matrix", vec!["0"; n]);
    cfg.insert("different_settings_to_system", vec![""; 3]);
    cfg.insert("inherits_group", vec![""; 3]);
    cfg.insert("compatible_machine_expression_group", vec![""; 3]);
    if let Some(cp) = process.get_strs("compatible_printers") {
        cfg.insert("print_compatible_printers", cp.to_vec());
    }
}

struct BambuHooks {
    machine: Config,
    process: Config,
    /// Every filament of the config (index = filament id) with the nozzle it prints on.
    filaments: Vec<(Config, usize)>,
    colors: Vec<String>,
    bed: BedType,
    ctx: OnceLock<Result<Context, String>>,
    /// The full config for project_settings.config and the G-code CONFIG block.
    full: Config,
    /// The templates' `global` variables (PrusaSlicer and OrcaSlicer macros), for this job.
    globals: std::sync::Mutex<std::collections::HashMap<String, only3d_profiles::Value>>,
}

impl BambuHooks {
    fn setup(&self) -> PrintSetup<'_> {
        let mut s = PrintSetup::new(&self.machine, &self.process);
        for (c, e) in &self.filaments {
            s = s.with_filament(c, *e);
        }
        s
    }

    fn print_ctx(&self, job: &JobInfo) -> Result<&Context, String> {
        self.ctx
            .get_or_init(|| {
                // in the printer's coordinates
                let o = plate_origin(&self.machine);
                let min = [job.first_layer_min[0] + o[0], job.first_layer_min[1] + o[1]];
                let max = [min[0] + job.first_layer_size[0], min[1] + job.first_layer_size[1]];
                let mut pj = job::JobInfo::new(self.bed, if job.is_multi() { job.filament_sequence.clone() } else { vec![0] }, min, max, job.max_z, job.total_layers);
                if job.is_multi() {
                    pj.first_layer_filaments = job.first_layer_filaments.clone();
                    pj.total_toolchanges = job.toolchanges;
                    if let Some(t) = &job.tower {
                        pj.has_wipe_tower = true;
                        pj.wipe_tower_stop_pos = Some([(t.min[0] + t.max[0]) / 2.0, t.max[1] + 2.0]);
                    }
                }
                let mut c = job::build_print_context(&self.setup(), &pj).map_err(|e| e.to_string())?;
                orca_vars(&mut c, self, min, max);
                Ok(c)
            })
            .as_ref()
            .map_err(|e| e.clone())
    }

    /// The filament loaded when `layer` starts.
    fn loaded_at(&self, job: &JobInfo, layer: usize) -> usize {
        if !job.is_multi() {
            return 0;
        }
        (0..layer).rev().find_map(|l| job.layer_filaments.get(l).and_then(|o| o.last().copied())).or_else(|| job.filament_sequence.first().copied()).unwrap_or(0)
    }

    fn render(&self, key: &str, layers: &[&Context]) -> Result<String, String> {
        let src = self.machine.get_str(key).unwrap_or("");
        if src.trim().is_empty() {
            return Ok(String::new());
        }
        let t = Template::named(key, src).map_err(|e| e.to_string())?;
        // `global` variables carry from one of the printer's templates to the next
        let mut globals = self.globals.lock().unwrap_or_else(|p| p.into_inner());
        t.render_with_globals(layers, &mut globals).map_err(|e| format!("{key}: {e}"))
    }

    fn event(&self, job: &JobInfo, layer: usize, z: f64) -> LayerEvent {
        let f = self.loaded_at(job, layer);
        LayerEvent {
            layer_num: layer as i64,
            layer_z: z,
            max_layer_z: z,
            current_filament: f,
            most_used_extruder: self.filaments.get(f).map(|x| x.1).unwrap_or(0),
            curr_y_acceleration_limit: self.machine.get_f64_at("machine_max_acceleration_y", 0).unwrap_or(20000.0),
            ..Default::default()
        }
    }
}

impl MachineHooks for BambuHooks {
    fn name(&self) -> &str {
        "bambu-templates"
    }

    fn start(&self, job: &JobInfo) -> Result<String, String> {
        let ctx = self.print_ctx(job)?;
        self.render("machine_start_gcode", &[ctx])
    }

    fn filament_start(&self, job: &JobInfo) -> Result<String, String> {
        let ctx = self.print_ctx(job)?;
        let first = job.filament_sequence.first().copied().filter(|_| job.is_multi()).unwrap_or(0);
        let src = self.filaments.get(first).and_then(|f| f.0.get_str_at("filament_start_gcode", 0)).unwrap_or("").to_string();
        let mut out = if src.trim().is_empty() { String::new() } else { Template::named("filament_start_gcode", &src).map_err(|e| e.to_string())?.render(ctx).map_err(|e| e.to_string())? };
        if !out.ends_with('\n') && !out.is_empty() {
            out.push('\n');
        }
        // Bambu's mark of the first filament
        if job.settings.dialect.bambu {
            out.push_str(&format!(";VT{first} H-1\n"));
        }
        Ok(out)
    }

    fn end(&self, job: &JobInfo) -> Result<String, String> {
        self.end_at(job, [0.0, 0.0, job.max_z])
    }

    fn end_at(&self, job: &JobInfo, position: [f64; 3]) -> Result<String, String> {
        let ctx = self.print_ctx(job)?;
        let ev = self.event(job, job.total_layers, job.max_z);
        let used = if job.is_multi() { job.used_filaments.clone() } else { vec![0] };
        let ectx = self.setup().end_vars(&ev, &used);
        // PrusaSlicer's `position`: where the nozzle is now
        let mut pos = Context::new();
        pos.set("position", only3d_profiles::Value::Floats(position.to_vec()));
        self.render("machine_end_gcode", &[&ectx, &pos, ctx])
    }

    fn before_layer_change(&self, job: &JobInfo, layer: usize, z: f64) -> Result<String, String> {
        if self.machine.get_str("before_layer_change_gcode").is_none_or(|t| t.trim().is_empty()) {
            return Ok(String::new());
        }
        let ctx = self.print_ctx(job)?;
        let ev = self.event(job, layer, z);
        let lc = self.setup().layer_change_vars(&ev);
        let mut out = self.render("before_layer_change_gcode", &[&lc, ctx])?;
        out.push('\n');
        Ok(out)
    }

    fn layer_change(&self, job: &JobInfo, layer: usize, z: f64) -> Result<String, String> {
        let ctx = self.print_ctx(job)?;
        let ev = self.event(job, layer, z);
        let setup = self.setup();
        let lc = setup.layer_change_vars(&ev);
        let mut out = self.render("layer_change_gcode", &[&lc, ctx])?;
        out.push('\n');
        // Bambu GCode.cpp: the nozzle-clumping (wrapping) check runs only when the process
        // enables it, and only on the first `wrapping_detection_layers` layers.
        let wrap_on = self.process.get_bool("enable_wrapping_detection").unwrap_or(false);
        let wrap_layers = self.machine.get_f64_at("wrapping_detection_layers", 0).or_else(|| self.process.get_f64_at("wrapping_detection_layers", 0)).unwrap_or(20.0);
        if wrap_on && (layer as f64) < wrap_layers {
            let wd = setup.wrapping_detection_vars(&ev);
            let w = self.render("wrapping_detection_gcode", &[&wd, ctx])?;
            if !w.trim().is_empty() {
                out.push_str(&w);
                out.push('\n');
            }
        }
        Ok(out)
    }

    fn timelapse(&self, job: &JobInfo, layer: usize, z: f64) -> Result<String, String> {
        let ctx = self.print_ctx(job)?;
        let ev = self.event(job, layer, z);
        let tl = TimelapseEvent { safe_pos: None, inline_photo: true, farthest_point_enabled: self.machine.get_bool("farthest_point_timelapse").unwrap_or(false), clear_to_x0: false };
        let v = self.setup().timelapse_vars(&ev, &tl);
        let mut out = self.render("time_lapse_gcode", &[&v, ctx])?;
        if !out.is_empty() {
            out.push('\n');
        }
        Ok(out)
    }

    fn machine_limits(&self) -> String {
        let g = |k: &str| self.machine.get_f64_at(k, 0).unwrap_or(0.0);
        if !self.machine.get_str("printer_model").is_some_and(|m| m.starts_with("Bambu Lab")) {
            return orca_machine_limits(&self.machine);
        }
        // Bambu/PrusaSlicer `print_machine_envelope`: legacy Marlin ("marlin", which Bambu
        // printers use) gets the extruding acceleration as M204 T; Marlin 2 its own travel value.
        let legacy = self.machine.get_str("gcode_flavor").is_none_or(|f| f == "marlin");
        let travel = if legacy { g("machine_max_acceleration_extruding") } else { g("machine_max_acceleration_travel") };
        format!(
            "M201 X{} Y{} Z{} E{}\nM203 X{} Y{} Z{} E{}\nM204 P{} R{} T{}\nM205 X{:.2} Y{:.2} Z{:.2} E{:.2}\n",
            g("machine_max_acceleration_x"),
            g("machine_max_acceleration_y"),
            g("machine_max_acceleration_z"),
            g("machine_max_acceleration_e"),
            g("machine_max_speed_x"),
            g("machine_max_speed_y"),
            g("machine_max_speed_z"),
            g("machine_max_speed_e"),
            g("machine_max_acceleration_extruding"),
            g("machine_max_acceleration_retracting"),
            travel,
            g("machine_max_jerk_x"),
            g("machine_max_jerk_y"),
            g("machine_max_jerk_z"),
            g("machine_max_jerk_e"),
        )
    }

    fn config_lines(&self, job: &JobInfo) -> Vec<(String, String)> {
        match &job.tower {
            // where the tower actually went
            Some(t) => {
                let mut full = self.full.clone();
                full.insert("wipe_tower_x", vec![format!("{:.3}", t.min[0])]);
                full.insert("wipe_tower_y", vec![format!("{:.3}", t.min[1])]);
                full.insert("prime_tower_width", format!("{:.0}", t.max[0] - t.min[0]));
                only3d_profiles::gcode_config::config_block(&full)
            }
            None => only3d_profiles::gcode_config::config_block(&self.full),
        }
    }

    fn config_is_complete(&self) -> bool {
        true
    }

    fn change_filament(&self, job: &JobInfo, ev: &only3d_slice::FilamentChange) -> Result<String, String> {
        let ctx = self.print_ctx(job)?;
        let tc = job::ToolChange {
            old_filament: ev.from,
            new_filament: ev.to,
            layer_num: ev.layer as i64,
            layer_z: ev.z,
            max_layer_z: ev.z,
            toolchange_z: ev.z,
            toolchange_count: ev.count as i64,
            purge_volume: ev.purge_mm3,
            old_in_first_layer_phase: ev.layer == 0,
            new_in_first_layer_phase: ev.layer == 0,
            new_extruder_retracted_length: 0.0,
            position_after: ev.resume_at,
            travel_points: [[0.0; 2]; 3],
            wipe_avoid_perimeter: false,
            wipe_avoid_pos_x: 0.0,
            is_prime_tower_interface: false,
            wipe_tower_stop_pos: ev.tower_center,
            used_filaments: job.used_filaments.clone(),
        };
        let vars = self.setup().toolchange_vars(&tc).map_err(|e| e.to_string())?;
        self.render("change_filament_gcode", &[&vars, ctx])
    }

    fn flush_mm3(&self, _job: &JobInfo, from: usize, to: usize) -> f64 {
        match (self.colors.get(from), self.colors.get(to)) {
            (Some(a), Some(b)) => only3d_slice::multi::flush_volume(a, b),
            _ => 150.0,
        }
    }
}

/// OrcaSlicer's `print_machine_envelope` for printers other than Bambu's: Marlin 1 and 2 and
/// RepRapFirmware with `emit_machine_limits_to_gcode` get M201/M203/M204 and the jerk limits;
/// Klipper and the rest get nothing (their configuration holds the limits).
fn orca_machine_limits(m: &Config) -> String {
    use only3d_slice::Flavor;
    let flavor = Flavor::from_profile(m.get_str("gcode_flavor").unwrap_or("marlin"));
    if !matches!(flavor, Flavor::Marlin | Flavor::Marlin2 | Flavor::RepRapFirmware) || !m.get_bool("emit_machine_limits_to_gcode").unwrap_or(true) {
        return String::new();
    }
    let g = |k: &str| m.get_f64_at(k, 0).unwrap_or(0.0);
    let i = |v: f64| (v + 0.5) as i64;
    let factor = if flavor == Flavor::RepRapFirmware { 60.0 } else { 1.0 };
    let mut out = format!(
        "M201 X{} Y{} Z{} E{}\nM203 X{} Y{} Z{} E{}\n",
        i(g("machine_max_acceleration_x")),
        i(g("machine_max_acceleration_y")),
        i(g("machine_max_acceleration_z")),
        i(g("machine_max_acceleration_e")),
        i(g("machine_max_speed_x") * factor),
        i(g("machine_max_speed_y") * factor),
        i(g("machine_max_speed_z") * factor),
        i(g("machine_max_speed_e") * factor),
    );
    let travel = if flavor == Flavor::Marlin { g("machine_max_acceleration_extruding") } else { g("machine_max_acceleration_travel") };
    out.push_str(&match flavor {
        Flavor::RepRapFirmware => format!("M204 P{} T{} ; sets acceleration (P, T), mm/sec^2\n", i(g("machine_max_acceleration_extruding")), i(travel)),
        Flavor::Marlin2 => format!("M204 P{} R{} T{} ; sets acceleration (P, T) and retract acceleration (R), mm/sec^2\n", i(g("machine_max_acceleration_extruding")), i(g("machine_max_acceleration_retracting")), i(travel)),
        _ => format!("M204 P{} R{} T{}\n", i(g("machine_max_acceleration_extruding")), i(g("machine_max_acceleration_retracting")), i(travel)),
    });
    out.push_str(&format!(
        "{} X{:.2} Y{:.2} Z{:.2} E{:.2} ; sets the jerk limits, {}\n",
        if flavor == Flavor::RepRapFirmware { "M566" } else { "M205" },
        g("machine_max_jerk_x") * factor,
        g("machine_max_jerk_y") * factor,
        g("machine_max_jerk_z") * factor,
        g("machine_max_jerk_e") * factor,
        if flavor == Flavor::RepRapFirmware { "mm/min" } else { "mm/sec" },
    ));
    // Marlin 2's junction deviation, when the printer has one
    let jd = g("machine_max_junction_deviation");
    if flavor == Flavor::Marlin2 && jd > 0.0 {
        out.push_str(&format!("M205 J{jd} ; sets the junction deviation\n"));
    }
    out
}

/// The variables OrcaSlicer's placeholder parser adds while it slices, which other makers'
/// templates use: the preset names, the extruders in use, the adaptive bed mesh around the
/// first layer (`GCode::_do_export`), the date and time, and the old names PrusaSlicer gave
/// some settings. Only what the context doesn't hold already.
fn orca_vars(c: &mut Context, h: &BambuHooks, min: [f64; 2], max: [f64; 2]) {
    use only3d_profiles::Value;
    let mut put = |k: &str, v: Value| {
        if !c.contains(k) {
            c.set(k, v);
        }
    };
    let s = |k: &str| h.full.get_str(k).unwrap_or("").to_string();
    put("printer_preset", Value::Str(s("printer_settings_id")));
    put("print_preset", Value::Str(s("print_settings_id")));
    put("filament_preset", Value::Strs(h.full.get_strs("filament_settings_id").map(|v| v.to_vec()).unwrap_or_default()));
    put("input_filename_base", Value::Str("only3d".into()));
    // OrcaSlicer writes the computer's login name here; only3d keeps it out of the G-code
    put("user", Value::Str("only3d".into()));
    let extruders = h.machine.get_strs("nozzle_diameter").map(|v| v.len()).unwrap_or(1).max(1);
    put("is_extruder_used", Value::Bools((0..extruders).map(|e| h.filaments.iter().any(|f| f.1 == e)).collect()));
    // what OrcaSlicer counts while it writes the G-code; only3d's end templates get no totals yet
    let nf = h.filaments.len().max(1);
    put("extruded_volume", Value::Floats(vec![0.0; nf]));
    put("extruded_weight", Value::Floats(vec![0.0; nf]));
    put("extruded_volume_total", Value::Float(0.0));
    put("extruded_weight_total", Value::Float(0.0));
    put("num_extruders", Value::Int(extruders as i64));
    // per-job keys that only the full config carries (Snapmaker's templates read this one)
    if let Some(v) = h.full.get_strs("nozzle_volume_type") {
        put("nozzle_volume_type", Value::Strs(v.to_vec()));
    }
    // PrusaSlicer's old names for some settings, still used by makers' templates
    for (old, new) in [("retract_length", "retraction_length"), ("retract_speed", "retraction_speed"), ("deretract_speed", "deretraction_speed"), ("retract_lift", "z_hop"), ("zhop", "z_hop"), ("retract_before_travel", "retraction_minimum_travel")] {
        let vals: Vec<f64> = h.filaments.iter().map(|f| f.0.get_f64_at(new, 0).or_else(|| h.machine.get_f64_at(new, 0)).unwrap_or(0.0)).collect();
        put(old, Value::Floats(vals));
    }
    // legacy names: `temperature` is the nozzle temperature, `first_layer_height` the first
    // layer's
    put("first_layer_height", Value::Float(h.process.get_f64("initial_layer_print_height").unwrap_or(0.2)));
    let nozzle: Vec<f64> = h.filaments.iter().map(|f| f.0.get_f64_at("nozzle_temperature", 0).unwrap_or(0.0)).collect();
    put("temperature", Value::Floats(nozzle));
    // adaptive bed mesh: the first layer's bounds plus a margin, inside the mesh limits
    let point = |k: &str, d: [f64; 2]| h.machine.get_str(k).and_then(|p| p.split_once('x')).and_then(|(x, y)| Some([x.trim().parse().ok()?, y.trim().parse().ok()?])).unwrap_or(d);
    let (lo, hi) = (point("bed_mesh_min", [-99999.0, -99999.0]), point("bed_mesh_max", [99999.0, 99999.0]));
    let margin = h.machine.get_f64("adaptive_bed_mesh_margin").unwrap_or(0.0);
    let mmin = [lo[0].max(min[0] - margin), lo[1].max(min[1] - margin)];
    let mmax = [hi[0].min(max[0] + margin), hi[1].min(max[1] + margin)];
    put("adaptive_bed_mesh_min", Value::Floats(mmin.to_vec()));
    put("adaptive_bed_mesh_max", Value::Floats(mmax.to_vec()));
    let dist = point("bed_mesh_probe_distance", [50.0, 50.0]);
    let mut count = [0, 1].map(|i| (((mmax[i] - mmin[i]) / dist[i].max(1.0)).ceil() as i64 + 1).max(3));
    let algo = if count[0] * count[1] <= 6 {
        "lagrange"
    } else {
        if h.machine.get_str("gcode_flavor") == Some("klipper") {
            count = count.map(|n| n.max(4));
        }
        "bicubic"
    };
    put("bed_mesh_probe_count", Value::Ints(count.to_vec()));
    put("bed_mesh_algo", Value::Str(algo.into()));
    // the date and time (UTC)
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // civil date from days since 1970 (Howard Hinnant's algorithm)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let (hour, minute, second) = (rem / 3600, rem / 60 % 60, rem % 60);
    put("timestamp", Value::Str(format!("{year}{month:02}{day:02}-{hour:02}{minute:02}{second:02}")));
    for (k, v) in [("year", year), ("month", month), ("day", day), ("hour", hour), ("minute", minute), ("second", second)] {
        put(k, Value::Int(v));
    }
}
