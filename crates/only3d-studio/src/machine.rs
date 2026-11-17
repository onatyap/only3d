//! Turns the project's printer/process/filament selection into concrete slicer settings and
//! machine G-code hooks. The default provider uses Bambu Studio's profiles (installed copy or
//! the embedded fallback) and renders Bambu's own H2D templates.

use only3d_model::{EvaluatedObject, NozzleSel, Project};
use only3d_slice::{GenericHooks, MachineHooks, SliceSettings};

use crate::ToolError;

pub struct MachineSetup {
    pub settings: SliceSettings,
    /// Per-object settings (same order as the objects passed in).
    pub object_settings: Vec<SliceSettings>,
    pub hooks: Box<dyn MachineHooks + Send>,
    /// Where the settings came from, for transparency.
    pub source: String,
    pub filament_type: String,
    pub filament_color: String,
    /// Bambu filament id ("GFA00"...), empty if unknown.
    pub filament_id: String,
    pub notes: Vec<String>,
    /// Flattened settings for Bambu Studio to reopen the exported project.
    pub project_settings: Option<String>,
    /// Safe to send to a real printer (genuine machine start/end G-code).
    pub printer_safe: bool,
    /// Plates with several filaments: each filament's settings (index = slot − 1). Empty for one.
    pub filament_settings: Vec<SliceSettings>,
    pub tower: Option<only3d_slice::TowerSpec>,
    pub nozzles: Option<only3d_slice::NozzleSetup>,
    /// Each filament of the config (index = filament id in the G-code): type, colour, Bambu id
    /// and the nozzle it prints on. One entry for a single-filament plate.
    pub filaments: Vec<FilamentInfo>,
}

#[derive(Clone, Debug)]
pub struct FilamentInfo {
    pub kind: String,
    pub color: String,
    /// Bambu filament id ("GFA00"), empty if unknown.
    pub id: String,
    /// H2D: 0 = left, 1 = right.
    pub extruder: usize,
}

pub trait MachineProvider: Send {
    fn name(&self) -> String;
    fn setup(&self, project: &Project, objects: &[EvaluatedObject]) -> Result<MachineSetup, ToolError>;
    /// Preset names of a kind ("machine" | "process" | "filament") containing `query`.
    fn list_presets(&self, kind: &str, query: &str) -> Vec<String>;
}

/// Apply project + object overrides to base settings. Unknown keys are reported, not ignored.
pub fn apply_overrides(base: &SliceSettings, project: &Project, objects: &[EvaluatedObject], notes: &mut Vec<String>) -> Result<(SliceSettings, Vec<SliceSettings>), ToolError> {
    let mut g = base.clone();
    let map: serde_json::Map<String, serde_json::Value> = project.process.overrides.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let unknown = g.apply_overrides(&map).map_err(|e| ToolError::new("invalid_settings", e, "see the slice settings schema (tool `settings_schema`)"))?;
    if !unknown.is_empty() {
        return Err(ToolError::new("unknown_setting", format!("unknown process override(s): {}", unknown.join(", ")), "call `settings_schema` to list valid keys"));
    }
    let mut per = Vec::new();
    for o in objects {
        let mut s = g.clone();
        if let Some(obj) = project.object(&o.id) {
            if !obj.settings.is_empty() {
                let m: serde_json::Map<String, serde_json::Value> = obj.settings.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                let unknown = s.apply_overrides(&m).map_err(|e| ToolError::new("invalid_settings", format!("object '{}': {e}", o.id), "fix the object's settings"))?;
                if !unknown.is_empty() {
                    return Err(ToolError::new("unknown_setting", format!("object '{}': unknown setting(s) {}", o.id, unknown.join(", ")), "call `settings_schema` to list valid keys"));
                }
                notes.push(format!("object '{}' overrides: {}", o.id, m.keys().cloned().collect::<Vec<_>>().join(", ")));
            }
        }
        per.push(s);
    }
    Ok((g, per))
}

/// Pick the H2D nozzle (0 = left, 1 = right) for a single-filament plate.
pub fn choose_extruder(project: &Project, objects: &[EvaluatedObject]) -> Result<u32, ToolError> {
    let slots: Vec<u32> = objects.iter().filter(|o| !o.hidden).flat_map(|o| o.filaments()).collect();
    let first = slots.first().copied().unwrap_or(1);
    if slots.iter().any(|&s| s != first) {
        return Err(ToolError::new(
            "multi_filament_unsupported",
            "the native slicer prints one filament per plate; objects use several filament slots",
            "the native slicer can't do this yet: call export_bambu_project with open: true to slice and print the plate in Bambu Studio, or put every object on one filament slot",
        ));
    }
    let want = project.filaments.get(first as usize - 1).map(|f| f.nozzle).unwrap_or_default();
    let (mut xmin, mut xmax) = (f32::INFINITY, f32::NEG_INFINITY);
    for o in objects.iter().filter(|o| !o.hidden) {
        xmin = xmin.min(o.report.bounds.min.x);
        xmax = xmax.max(o.report.bounds.max.x);
    }
    Ok(match want {
        NozzleSel::Left => 0,
        NozzleSel::Right => 1,
        NozzleSel::Auto => {
            if xmax > 325.0 {
                1
            } else if xmin < 25.0 {
                0
            } else {
                // Bambu Studio maps a single filament to the right (main) extruder, which the
                // first AMS feeds; `print` re-checks this against the tray it prints from.
                1
            }
        }
    })
}

/// Built-in defaults with a generic start sequence: for previews only.
pub struct DefaultProvider;

impl MachineProvider for DefaultProvider {
    fn name(&self) -> String {
        "built-in defaults (generic G-code)".into()
    }
    fn setup(&self, project: &Project, objects: &[EvaluatedObject]) -> Result<MachineSetup, ToolError> {
        let mut notes = vec!["Bambu profiles are not available: using built-in H2D/PLA defaults and a GENERIC start sequence. Do not send this G-code to an H2D.".to_string()];
        let (mut g, per) = apply_overrides(&SliceSettings::default(), project, objects, &mut notes)?;
        g.extruder = choose_extruder(project, objects)?;
        let fil = project.filaments.first();
        let color = fil.map(|f| f.color.clone()).unwrap_or_else(|| "#D9CBB5".into());
        let extruder = g.extruder as usize;
        Ok(MachineSetup {
            settings: g,
            object_settings: per,
            hooks: Box::new(GenericHooks),
            source: self.name(),
            filament_type: "PLA".into(),
            filament_color: color.clone(),
            filament_id: String::new(),
            notes,
            project_settings: None,
            printer_safe: false,
            filament_settings: Vec::new(),
            tower: None,
            nozzles: None,
            filaments: vec![FilamentInfo { kind: "PLA".into(), color, id: String::new(), extruder }],
        })
    }
    fn list_presets(&self, _kind: &str, _query: &str) -> Vec<String> {
        Vec::new()
    }
}

/// Bambu presets + templates when available (installed Bambu Studio or the embedded copy),
/// otherwise generic defaults.
pub fn default_provider() -> Box<dyn MachineProvider> {
    if crate::bambu::store().is_ok() {
        Box::new(crate::bambu::BambuProvider)
    } else {
        Box::new(DefaultProvider)
    }
}
