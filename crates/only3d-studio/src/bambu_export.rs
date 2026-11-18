//! Hand a plate to Bambu Studio for what the native engine doesn't do yet (several filaments
//! on one plate, tree supports): writes a Bambu Studio project through Bambu Studio's own
//! command line, with presets flattened correctly (inherits *and* include, so the H2D keeps its
//! real start G-code), optionally sliced.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use only3d_profiles::{Config, ConfigValue, PresetKind};
use serde_json::{json, Value};

use crate::{bambu, Studio, ToolError, ToolOutput};

const EXE: &str = "/Applications/BambuStudio.app/Contents/MacOS/BambuStudio";

fn set_all(cfg: &mut Config, key: &str, v: &str) {
    let n = cfg.len_of(key).unwrap_or(1).max(1);
    let val: ConfigValue = if cfg.get(key).is_some_and(|x| x.is_array()) { vec![v.to_string(); n].into() } else { v.to_string().into() };
    cfg.insert(key, val);
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// Map only3d override keys onto Bambu preset keys. Returns keys that have no mapping.
pub(crate) fn map_overrides(ov: &std::collections::BTreeMap<String, Value>, process: &mut Config, filaments: &mut [Config]) -> Vec<String> {
    let mut unmapped = Vec::new();
    for (k, v) in ov {
        let f = num(v);
        match (k.as_str(), f) {
            ("layer_height", Some(x)) => set_all(process, "layer_height", &x.to_string()),
            ("first_layer_height", Some(x)) => set_all(process, "initial_layer_print_height", &x.to_string()),
            ("wall_loops", Some(x)) => set_all(process, "wall_loops", &(x as i64).to_string()),
            ("top_layers", Some(x)) => set_all(process, "top_shell_layers", &(x as i64).to_string()),
            ("bottom_layers", Some(x)) => set_all(process, "bottom_shell_layers", &(x as i64).to_string()),
            ("infill_density", Some(x)) => set_all(process, "sparse_infill_density", &format!("{}%", (x * 100.0).round())),
            ("brim_width", Some(x)) => {
                set_all(process, "brim_width", &x.to_string());
                set_all(process, "brim_type", if x > 0.0 { "outer_only" } else { "no_brim" });
            }
            ("support", _) => {
                let on = v.as_bool().unwrap_or(f.unwrap_or(0.0) != 0.0);
                set_all(process, "enable_support", if on { "1" } else { "0" });
                if on {
                    set_all(process, "support_type", "tree(auto)");
                }
            }
            ("support_angle", Some(x)) => set_all(process, "support_threshold_angle", &x.to_string()),
            ("support_on_plate_only", _) => set_all(process, "support_on_build_plate_only", if v.as_bool().unwrap_or(f.unwrap_or(0.0) != 0.0) { "1" } else { "0" }),
            ("support_interface_layers", Some(x)) => set_all(process, "support_interface_top_layers", &(x as i64).to_string()),
            ("skirt_loops", Some(x)) => set_all(process, "skirt_loops", &(x as i64).to_string()),
            ("skirt_distance", Some(x)) => set_all(process, "skirt_distance", &x.to_string()),
            ("outer_wall_speed", Some(x)) => set_all(process, "outer_wall_speed", &x.to_string()),
            ("inner_wall_speed", Some(x)) => set_all(process, "inner_wall_speed", &x.to_string()),
            ("sparse_infill_speed", Some(x)) => set_all(process, "sparse_infill_speed", &x.to_string()),
            ("top_surface_speed", Some(x)) => set_all(process, "top_surface_speed", &x.to_string()),
            ("fuzzy_skin", _) => set_all(process, "fuzzy_skin", v.as_str().unwrap_or("none")),
            ("fuzzy_skin_thickness", Some(x)) => set_all(process, "fuzzy_skin_thickness", &x.to_string()),
            ("fuzzy_skin_point_distance", Some(x)) => set_all(process, "fuzzy_skin_point_distance", &x.to_string()),
            ("fuzzy_skin_first_layer", _) => set_all(process, "fuzzy_skin_first_layer", if v.as_bool().unwrap_or(f.unwrap_or(0.0) != 0.0) { "1" } else { "0" }),
            ("ironing", _) => set_all(process, "ironing_type", if v.as_bool().unwrap_or(false) { "top" } else { "no ironing" }),
            ("bed_temp", Some(x)) => filaments.iter_mut().for_each(|c| set_all(c, "textured_plate_temp", &x.round().to_string())),
            ("flow_ratio", Some(x)) => filaments.iter_mut().for_each(|c| set_all(c, "filament_flow_ratio", &x.to_string())),
            ("z_hop", Some(x)) => set_all(process, "z_hop", &x.to_string()),
            ("nozzle_temp", Some(x)) => filaments.iter_mut().for_each(|c| set_all(c, "nozzle_temperature", &x.round().to_string())),
            ("first_layer_nozzle_temp", Some(x)) => filaments.iter_mut().for_each(|c| set_all(c, "nozzle_temperature_initial_layer", &x.round().to_string())),
            _ => unmapped.push(k.clone()),
        }
    }
    unmapped
}

fn write_json(dir: &Path, name: &str, cfg: &Config, kind: &str, preset: &str) -> Result<PathBuf, ToolError> {
    let p = dir.join(name);
    let mut v = cfg.to_json();
    if let Some(o) = v.as_object_mut() {
        o.remove("inherits");
        o.remove("include");
        o.insert("from".into(), json!("system"));
        o.insert("type".into(), json!(kind));
        o.insert("name".into(), json!(preset));
        o.entry("instantiation").or_insert(json!("true"));
    }
    std::fs::write(&p, serde_json::to_string(&v).unwrap_or_default()).map_err(|e| ToolError::new("io_error", e.to_string(), ""))?;
    Ok(p)
}

fn run_with_timeout(mut cmd: Command, limit: Duration) -> Result<(i32, String), ToolError> {
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| ToolError::new("bambu_studio_missing", format!("cannot run {EXE}: {e}"), "install Bambu Studio in /Applications"))?;
    let start = Instant::now();
    loop {
        if let Some(st) = child.try_wait().map_err(|e| ToolError::new("bambu_cli_failed", e.to_string(), ""))? {
            let mut out = String::new();
            if let Some(mut so) = child.stdout.take() {
                let _ = so.read_to_string(&mut out);
            }
            return Ok((st.code().unwrap_or(-1), out));
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            return Err(ToolError::new("bambu_cli_timeout", format!("Bambu Studio did not finish in {}s", limit.as_secs()), "check whether a Bambu Studio dialog is waiting on screen"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Open a project in the Bambu Studio app (macOS `open -a`), which takes over from here.
pub fn open_in_bambu_studio(path: &Path) -> Result<(), ToolError> {
    #[cfg(test)]
    {
        let _ = path;
        Ok(())
    }
    #[cfg(all(not(test), not(target_os = "macos")))]
    {
        Err(ToolError::new("open_failed", format!("opening Bambu Studio from only3d works on macOS only so far ({})", path.display()), "open the file in Bambu Studio yourself"))
    }
    #[cfg(all(not(test), target_os = "macos"))]
    {
        let app = Path::new(EXE).ancestors().nth(3).unwrap_or(Path::new("/Applications/BambuStudio.app"));
        let st = Command::new("open").arg("-a").arg(app).arg(path).status().map_err(|e| ToolError::new("open_failed", e.to_string(), "open the file in Bambu Studio yourself"))?;
        if !st.success() {
            return Err(ToolError::new("open_failed", format!("could not open {} in Bambu Studio", path.display()), "open the file in Bambu Studio yourself"));
        }
        Ok(())
    }
}

pub fn export_bambu_project(s: &mut Studio, out_path: &Path, slice: bool) -> Result<ToolOutput, ToolError> {
    if !Path::new(EXE).exists() {
        return Err(ToolError::new("bambu_studio_missing", "Bambu Studio is not installed", "install it from bambulab.com, or use the native `slice`"));
    }
    let store = bambu::store()?;
    // every plate of the project, in order (empty plates are left out)
    let objs = s.evaluated_all()?;
    let mut visible: Vec<_> = objs.iter().filter(|o| !o.hidden).cloned().collect();
    visible.sort_by_key(|o| o.plate);
    if visible.is_empty() {
        return Err(ToolError::new("empty_plate", "nothing to export", "add objects first"));
    }
    let p = &s.project;
    let perr = |e: only3d_profiles::ProfileError| ToolError::new("profile_error", e.to_string(), "check preset names with list_presets");
    let mut machine = store.resolve(PresetKind::Machine, &p.printer.machine).map_err(perr)?;
    let mut process = store.resolve(PresetKind::Process, &p.process.preset).map_err(perr)?;
    let mut fils: Vec<Config> = Vec::new();
    for f in &p.filaments {
        let mut c = store.resolve(PresetKind::Filament, &f.preset).map_err(perr)?;
        crate::spools::slot_calibration(f.spool.as_deref()).apply(&mut c);
        c.insert("filament_settings_id", vec![f.preset.clone()]);
        c.insert("compatible_printers", vec![p.printer.machine.clone()]);
        c.insert("compatible_printers_condition", "");
        c.insert("filament_colour", vec![f.color.clone()]);
        fils.push(c);
    }
    machine.insert("printer_settings_id", p.printer.machine.clone());
    machine.insert("curr_bed_type", p.printer.bed_type.clone());
    process.insert("print_settings_id", p.process.preset.clone());
    process.insert("compatible_printers", vec![p.printer.machine.clone()]);
    process.insert("compatible_printers_condition", "");
    process.insert("curr_bed_type", p.printer.bed_type.clone());
    let unmapped = map_overrides(&p.process.overrides, &mut process, &mut fils);

    let used: std::collections::BTreeSet<u32> = visible.iter().flat_map(|o| o.filaments()).collect();
    let multi = used.len() > 1;
    let tmp = std::env::temp_dir().join(format!("only3d_bambu_{}_{}", std::process::id(), crate::now_ms()));
    std::fs::create_dir_all(tmp.join("data")).map_err(|e| ToolError::new("io_error", e.to_string(), ""))?;
    let mpath = write_json(&tmp, "machine.json", &machine, "machine", &p.printer.machine)?;
    let ppath = write_json(&tmp, "process.json", &process, "process", &p.process.preset)?;
    let mut fpaths = Vec::new();
    for (i, f) in fils.iter().enumerate() {
        fpaths.push(write_json(&tmp, &format!("filament_{i}.json"), f, "filament", &p.filaments[i].preset)?.display().to_string());
    }
    let mut plates: Vec<(u32, Vec<Value>)> = Vec::new();
    for (i, o) in visible.iter().enumerate() {
        let b = o.report.bounds;
        let (cx, cy) = ((b.min.x + b.max.x) / 2.0, (b.min.y + b.max.y) / 2.0);
        // one entry per body; a multi-part object's bodies share its assemble_index, which
        // Bambu Studio merges into one object whose volumes keep their own filament
        let bodies: Vec<(std::sync::Arc<only3d_geom::Mesh>, u32)> = if o.parts.is_empty() { vec![(o.mesh.clone(), o.filament)] } else { o.parts.iter().map(|p| (p.mesh.clone(), p.filament)).collect() };
        for (j, (mesh, filament)) in bodies.into_iter().enumerate() {
            let mut m = (*mesh).clone();
            m.translate(glam::Vec3::new(-cx, -cy, -b.min.z));
            let stl = tmp.join(format!("part_{i}_{j}.stl"));
            std::fs::write(&stl, only3d_geom::io::stl::write_binary(&m)).map_err(|e| ToolError::new("io_error", e.to_string(), ""))?;
            let entry = json!({
                "path": stl, "count": 1, "filaments": [filament], "assemble_index": [i + 1],
                "pos_x": [cx], "pos_y": [cy], "pos_z": [0], "subtype": "normal_part", "object_name": o.name
            });
            match plates.last_mut() {
                Some((n, list)) if *n == o.plate => list.push(entry),
                _ => plates.push((o.plate, vec![entry])),
            }
        }
    }
    let n_plates = plates.len();
    let assemble = json!({"plates": plates.into_iter().map(|(n, objects)| {
        let name = if p.plate_count() > 1 { p.plate_name(n) } else if p.name.is_empty() { "only3d".to_string() } else { p.name.clone() };
        json!({"plate_name": name, "need_arrange": false, "objects": objects})
    }).collect::<Vec<_>>()});
    let apath = tmp.join("assemble.json");
    std::fs::write(&apath, assemble.to_string()).map_err(|e| ToolError::new("io_error", e.to_string(), ""))?;

    let name = "project.3mf";
    let mut cmd = Command::new(EXE);
    cmd.arg("--datadir").arg(tmp.join("data")).arg("--outputdir").arg(&tmp);
    cmd.arg("--load-settings").arg(format!("{};{}", mpath.display(), ppath.display()));
    cmd.arg("--load-filaments").arg(fpaths.join(";"));
    // Studio 02.09's CLI crashes slicing two-filament plates: export those unsliced
    let do_slice = slice && !multi;
    if do_slice {
        cmd.arg("--slice").arg("0");
    }
    cmd.arg("--export-3mf").arg(name).arg("--load-assemble-list").arg(&apath);
    let (rc, log) = run_with_timeout(cmd, Duration::from_secs(300))?;
    let produced = tmp.join(name);
    if !produced.exists() {
        return Err(ToolError::new("bambu_cli_failed", format!("Bambu Studio exited with {rc} and wrote no project: {}", log.lines().rev().take(4).collect::<Vec<_>>().join(" | ")), "try fewer objects, or slice natively"));
    }
    // name the objects (the CLI calls merged objects assemble_N)
    let bytes = std::fs::read(&produced).map_err(|e| ToolError::new("io_error", e.to_string(), ""))?;
    let mut zin = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| ToolError::new("bambu_cli_failed", e.to_string(), ""))?;
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..zin.len() {
        let mut f = zin.by_index(i).map_err(|e| ToolError::new("io_error", e.to_string(), ""))?;
        let mut d = Vec::new();
        f.read_to_end(&mut d).map_err(|e| ToolError::new("io_error", e.to_string(), ""))?;
        let n = f.name().to_string();
        if n == "Metadata/model_settings.config" {
            let mut text = String::from_utf8_lossy(&d).to_string();
            for (k, o) in visible.iter().enumerate().rev() {
                text = text.replace(&format!("value=\"assemble_{}\"", k + 1), &format!("value=\"{}\"", o.name.replace('"', "'")));
            }
            d = text.into_bytes();
        }
        files.push((n, d));
    }
    let gcode_head = files.iter().find(|(n, _)| n == "Metadata/plate_1.gcode").map(|(_, d)| String::from_utf8_lossy(&d[..d.len().min(20000)]).to_string());
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        use std::io::Write;
        let mut z = zip::ZipWriter::new(&mut buf);
        let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (n, d) in &files {
            z.start_file(n.as_str(), opt).map_err(|e| ToolError::new("io_error", e.to_string(), ""))?;
            z.write_all(d).map_err(|e| ToolError::new("io_error", e.to_string(), ""))?;
        }
        z.finish().map_err(|e| ToolError::new("io_error", e.to_string(), ""))?;
    }
    if let Some(dir) = out_path.parent().filter(|d| !d.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(out_path, buf.into_inner()).map_err(|e| ToolError::new("io_error", format!("{}: {e}", out_path.display()), ""))?;
    let _ = std::fs::remove_dir_all(&tmp);

    let mut text = format!(
        "Wrote Bambu Studio project {} ({} plate(s), {} object(s), {} filament(s){}).",
        out_path.display(),
        n_plates,
        visible.len(),
        used.len(),
        if do_slice { ", sliced by Bambu Studio" } else { ", not sliced — open it in Bambu Studio and press Slice" }
    );
    let mut data = json!({"path": out_path, "sliced": do_slice, "filaments": used.len(), "plates": n_plates});
    if let Some(h) = gcode_head {
        let grab = |pat: &str| h.lines().find(|l| l.contains(pat)).and_then(|l| l.split(':').next_back()).map(|x| x.trim().to_string());
        let (t, g) = (grab("total estimated time"), grab("total filament weight"));
        let genuine = h.contains("M993") || h.contains("H2D");
        text.push_str(&format!(" Bambu estimate: {} , {} g. Start G-code: {}.", t.clone().unwrap_or_default(), g.clone().unwrap_or_default(), if genuine { "H2D sequence" } else { "check it" }));
        data["bambu_time"] = json!(t);
        data["bambu_grams"] = json!(g);
    }
    if multi && slice {
        text.push_str(" (Bambu Studio's command line cannot slice multi-filament plates, so the project is unsliced.)");
    }
    if !unmapped.is_empty() {
        text.push_str(&format!(" Not carried over (set them in Studio): {}.", unmapped.join(", ")));
    }
    Ok(ToolOutput::text(text).with_data(data))
}
