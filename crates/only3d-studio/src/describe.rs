//! A compact, LLM-friendly description of the session: what an agent needs in its context to
//! act, in a few hundred tokens, plus the same facts as structured JSON.

use std::fmt::Write as _;

use only3d_model::Severity;
use serde_json::json;

use crate::{Studio, ToolError};

pub fn describe(studio: &mut Studio) -> Result<(String, serde_json::Value), ToolError> {
    let objs = studio.evaluated()?;
    let findings = studio.findings()?;
    let p = &studio.project;
    let mut t = String::new();
    let _ = writeln!(
        t,
        "Project \"{}\" (rev {}){} — {} · {} · {}",
        p.name,
        studio.revision,
        studio.project_path.as_ref().map(|x| format!(" at {}", x.display())).unwrap_or_default(),
        p.printer.machine,
        p.printer.bed_type,
        p.process.preset
    );
    if !p.process.overrides.is_empty() {
        let _ = writeln!(t, "Process overrides: {}", serde_json::to_string(&p.process.overrides).unwrap_or_default());
    }
    let inv = crate::spools::Inventory::load();
    for (i, f) in p.filaments.iter().enumerate() {
        let sp = f.spool.as_deref().and_then(|id| inv.get(id));
        let _ = writeln!(
            t,
            "Filament {}: {} {} (nozzle: {:?}){}",
            i + 1,
            f.preset,
            f.color,
            f.nozzle,
            sp.map(|sp| format!(" — spool {}: {:.0} g left{}", sp.id, sp.remaining_g, sp.location.as_ref().map(|l| format!(" in {l}")).unwrap_or_default())).unwrap_or_default()
        );
    }
    if !p.params.is_empty() {
        let _ = writeln!(t, "Params: {}", p.params.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(", "));
    }
    let density = 1.24; // PLA-ish estimate for quick mass hints
    let n_plates = p.plate_count();
    if n_plates > 1 {
        let list: Vec<String> = (1..=n_plates)
            .map(|n| {
                let k = p.objects.iter().filter(|o| o.plate == n).count();
                format!("{}{n} '{}' ({k} object{})", if n == studio.plate { "▸" } else { "" }, p.plate_name(n), if k == 1 { "" } else { "s" })
            })
            .collect();
        let _ = writeln!(t, "Plates: {}. Below is the active plate; tools act on it (switch with plate {{\"action\": \"select\"}}).", list.join(", "));
    }
    let _ = writeln!(
        t,
        "{} — bed {}x{} mm, max height {} mm. Objects ({}):",
        if n_plates > 1 { format!("Plate {} '{}'", studio.plate, p.plate_name(studio.plate)) } else { "Plate".to_string() },
        studio.bed.size[0],
        studio.bed.size[1],
        studio.bed.max_height,
        objs.len()
    );
    let mut jobjs = Vec::new();
    for o in objs.iter() {
        let r = &o.report;
        let b = r.bounds;
        let sel = if studio.selection.contains(&o.id) { " [SELECTED]" } else { "" };
        let _ = writeln!(
            t,
            "- {}{}{}: {:.1} x {:.1} x {:.1} mm at ({:.1}, {:.1}), {:.2} cm³ (~{:.1} g solid), {} tris, {}{}{}",
            o.id,
            if o.name != o.id && !o.name.is_empty() { format!(" \"{}\"", o.name) } else { String::new() },
            sel,
            r.size.x,
            r.size.y,
            r.size.z,
            (b.min.x + b.max.x) / 2.0,
            (b.min.y + b.max.y) / 2.0,
            r.volume_mm3 / 1000.0,
            r.volume_mm3 / 1000.0 * density,
            r.triangles,
            if r.watertight { "watertight" } else { "NOT watertight" },
            if o.filament != 1 { format!(", filament {}", o.filament) } else { String::new() },
            if o.hidden { ", hidden" } else { "" },
        );
        if studio.project.object(&o.id).is_some_and(|x| x.locked) {
            t.pop();
            let _ = writeln!(t, ", locked");
        }
        if !o.parts.is_empty() {
            let _ = writeln!(t, "  parts: {}", o.parts.iter().map(|p| format!("{} (filament {})", p.name, p.filament)).collect::<Vec<_>>().join(", "));
        }
        jobjs.push(json!({
            "id": o.id, "name": o.name, "selected": studio.selection.contains(&o.id), "hidden": o.hidden,
            "locked": studio.project.object(&o.id).is_some_and(|x| x.locked),
            "parts": o.parts.iter().map(|p| json!({"name": p.name, "filament": p.filament})).collect::<Vec<_>>(),
            "filament": o.filament, "bounds": {"min": b.min, "max": b.max}, "size": r.size,
            "volume_mm3": r.volume_mm3, "watertight": r.watertight, "triangles": r.triangles, "components": r.components
        }));
    }
    if let Some(a) = p.assembly.as_ref().filter(|a| !a.parts.is_empty() || !a.reference.is_empty()) {
        let _ = writeln!(
            t,
            "Assembly: {}{} (render {{\"assembly\": true}})",
            a.parts.iter().map(|x| x.object.as_str()).collect::<Vec<_>>().join(", "),
            if a.reference.is_empty() { String::new() } else { format!(" + reference, not printed: {}", a.reference.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(", ")) }
        );
    }
    let errors = findings.iter().filter(|f| f.severity == Severity::Error).count();
    let warnings = findings.iter().filter(|f| f.severity == Severity::Warning).count();
    let _ = writeln!(t, "Checks: {errors} error(s), {warnings} warning(s)");
    let ticked = findings.iter().filter(|f| f.code == "spec_ok").count();
    if ticked > 0 {
        let _ = writeln!(t, "Project checks passing: {ticked}");
    }
    for f in findings.iter().filter(|f| f.severity != Severity::Info).take(8) {
        let _ = writeln!(t, "  {:?} {}: {} → {}", f.severity, f.code, f.message, f.hint);
    }
    let slice = studio.last_slice.as_ref().map(|s| {
        let st = &s.result.stats;
        let current = s.revision == studio.revision && s.plate == studio.plate;
        let _ = writeln!(
            t,
            "Last slice ({}{}): {} layers, {:.0} min, {:.1} g, {} warning(s)",
            s.engine,
            if current { ", current" } else { ", STALE — re-slice" },
            st.layers,
            st.print_time_s / 60.0,
            st.filament_g,
            s.result.warnings.len()
        );
        json!({"current": current, "layers": st.layers, "print_time_s": st.print_time_s, "filament_g": st.filament_g, "engine": s.engine})
    });
    if slice.is_none() {
        let _ = writeln!(t, "Not sliced yet.");
    }
    let data = json!({
        "name": p.name, "revision": studio.revision, "path": studio.project_path,
        "printer": p.printer, "process": p.process, "filaments": p.filaments, "params": p.params,
        "bed": studio.bed, "objects": jobjs, "selection": studio.selection,
        "active_plate": studio.plate,
        "plates": (1..=n_plates).map(|n| json!({"plate": n, "name": p.plate_name(n), "objects": p.objects.iter().filter(|o| o.plate == n).map(|o| o.id.clone()).collect::<Vec<_>>()})).collect::<Vec<_>>(),
        "findings": findings, "slice": slice, "assembly": p.assembly,
    });
    Ok((t, data))
}
