//! Slice settings: the ~40 knobs that decide 95% of print outcomes. Defaults mirror Bambu
//! Studio's "0.20mm Standard @BBL H2D" process with "Bambu PLA Basic @BBL H2D" on a 0.4 nozzle.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum InfillPattern {
    /// Two crossing line directions every layer (Bambu default).
    #[default]
    Grid,
    /// One direction per layer, alternating 90°.
    Rectilinear,
    /// Concentric rings following the walls.
    Concentric,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum SeamPosition {
    /// Line the seam up at the back of the part (cleanest look).
    #[default]
    Rear,
    /// Start each loop where the nozzle already is (fastest).
    Nearest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum WallOrder {
    /// Inner walls first, then the outer wall (best dimensional accuracy on overhangs; Bambu default).
    #[default]
    InnerOuter,
    OuterInner,
}

/// Every value is in mm, mm/s, mm/s², °C or a 0..1 fraction.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SliceSettings {
    // geometry
    pub layer_height: f64,
    pub first_layer_height: f64,
    pub nozzle_diameter: f64,
    pub filament_diameter: f64,
    pub outer_wall_width: f64,
    pub inner_wall_width: f64,
    pub infill_width: f64,
    pub solid_infill_width: f64,
    pub top_surface_width: f64,
    pub first_layer_width: f64,
    pub wall_loops: u32,
    pub top_layers: u32,
    pub bottom_layers: u32,
    /// Sparse infill density 0..1.
    pub infill_density: f64,
    pub infill_pattern: InfillPattern,
    /// Infill angle in degrees.
    pub infill_angle: f64,
    /// Overlap of infill into the innermost wall, fraction of its width.
    pub infill_wall_overlap: f64,
    pub seam: SeamPosition,
    pub wall_order: WallOrder,
    /// Brim width (mm) around the first layer; 0 = none.
    pub brim_width: f64,
    pub brim_gap: f64,
    /// Skirt loops around the first layer (primes the nozzle); 0 = none.
    pub skirt_loops: u32,
    /// Gap between skirt and objects (mm).
    pub skirt_distance: f64,
    /// Ironing pass over top surfaces.
    pub ironing: bool,

    // supports (grid style)
    pub support: bool,
    /// Overhangs flatter than this angle from horizontal (degrees) get support.
    pub support_angle: f64,
    /// Distance between support lines (mm).
    pub support_spacing: f64,
    /// Dense layers at the top of each support column.
    pub support_interface_layers: u32,
    pub support_interface_spacing: f64,
    /// Horizontal clearance between support and model (mm).
    pub support_xy_gap: f64,
    /// Empty layers between the support top and the model.
    pub support_z_gap_layers: u32,
    /// Only build support that stands on the plate.
    pub support_on_plate_only: bool,
    pub support_speed: f64,
    pub support_interface_speed: f64,

    // speeds (mm/s)
    pub outer_wall_speed: f64,
    pub inner_wall_speed: f64,
    pub sparse_infill_speed: f64,
    pub solid_infill_speed: f64,
    pub top_surface_speed: f64,
    pub bridge_speed: f64,
    pub first_layer_speed: f64,
    pub first_layer_infill_speed: f64,
    pub travel_speed: f64,

    // accelerations (mm/s²)
    pub default_accel: f64,
    pub outer_wall_accel: f64,
    pub top_surface_accel: f64,
    pub first_layer_accel: f64,
    pub travel_accel: f64,

    // filament
    pub filament_type: String,
    pub filament_density: f64,
    pub filament_cost_per_kg: f64,
    pub flow_ratio: f64,
    pub max_volumetric_speed: f64,
    pub nozzle_temp: f64,
    pub first_layer_nozzle_temp: f64,
    pub bed_temp: f64,
    pub first_layer_bed_temp: f64,
    /// Part fan range (0..1) and the layer time (s) below which it runs at max.
    pub fan_min: f64,
    pub fan_max: f64,
    pub fan_full_below_layer_time: f64,
    pub aux_fan: f64,
    pub fan_off_first_layers: u32,
    /// Slow down so each layer takes at least this long (s), never below `min_print_speed`.
    pub min_layer_time: f64,
    pub min_print_speed: f64,

    // retraction / travel
    pub retract_length: f64,
    pub retract_speed: f64,
    pub deretract_speed: f64,
    pub retract_min_travel: f64,
    pub z_hop: f64,
    pub wipe_distance: f64,

    /// 0 = left nozzle, 1 = right nozzle (H2D).
    pub extruder: u32,
}

impl Default for SliceSettings {
    fn default() -> Self {
        SliceSettings {
            layer_height: 0.2,
            first_layer_height: 0.2,
            nozzle_diameter: 0.4,
            filament_diameter: 1.75,
            outer_wall_width: 0.42,
            inner_wall_width: 0.45,
            infill_width: 0.45,
            solid_infill_width: 0.42,
            top_surface_width: 0.42,
            first_layer_width: 0.5,
            wall_loops: 2,
            top_layers: 5,
            bottom_layers: 3,
            infill_density: 0.15,
            infill_pattern: InfillPattern::Grid,
            infill_angle: 45.0,
            infill_wall_overlap: 0.15,
            seam: SeamPosition::Rear,
            wall_order: WallOrder::InnerOuter,
            brim_width: 0.0,
            brim_gap: 0.1,
            skirt_loops: 0,
            skirt_distance: 3.0,
            ironing: false,
            support: false,
            support_angle: 30.0,
            support_spacing: 2.5,
            support_interface_layers: 2,
            support_interface_spacing: 0.5,
            support_xy_gap: 0.35,
            support_z_gap_layers: 1,
            support_on_plate_only: false,
            support_speed: 150.0,
            support_interface_speed: 80.0,
            outer_wall_speed: 200.0,
            inner_wall_speed: 300.0,
            sparse_infill_speed: 350.0,
            solid_infill_speed: 250.0,
            top_surface_speed: 200.0,
            bridge_speed: 50.0,
            first_layer_speed: 50.0,
            first_layer_infill_speed: 105.0,
            travel_speed: 1000.0,
            default_accel: 8000.0,
            outer_wall_accel: 5000.0,
            top_surface_accel: 2000.0,
            first_layer_accel: 500.0,
            travel_accel: 10000.0,
            filament_type: "PLA".into(),
            filament_density: 1.26,
            filament_cost_per_kg: 15.99,
            flow_ratio: 0.98,
            max_volumetric_speed: 25.0,
            nozzle_temp: 220.0,
            first_layer_nozzle_temp: 220.0,
            bed_temp: 55.0,
            first_layer_bed_temp: 55.0,
            fan_min: 0.6,
            fan_max: 0.8,
            fan_full_below_layer_time: 100.0,
            aux_fan: 0.75,
            fan_off_first_layers: 1,
            min_layer_time: 4.0,
            min_print_speed: 20.0,
            retract_length: 0.4,
            retract_speed: 30.0,
            deretract_speed: 30.0,
            retract_min_travel: 1.0,
            z_hop: 0.4,
            wipe_distance: 1.0,
            extruder: 1,
        }
    }
}

impl SliceSettings {
    /// Apply a JSON object of overrides (keys are field names). Returns the keys that were unknown.
    pub fn apply_overrides(&mut self, overrides: &serde_json::Map<String, serde_json::Value>) -> Result<Vec<String>, String> {
        let mut v = serde_json::to_value(&*self).map_err(|e| e.to_string())?;
        let obj = v.as_object_mut().expect("settings serialise to an object");
        let mut unknown = Vec::new();
        for (k, val) in overrides {
            if obj.contains_key(k) {
                obj.insert(k.clone(), val.clone());
            } else {
                unknown.push(k.clone());
            }
        }
        *self = serde_json::from_value(v).map_err(|e| format!("invalid override value: {e}"))?;
        self.validate()?;
        Ok(unknown)
    }

    pub fn validate(&self) -> Result<(), String> {
        let n = self.nozzle_diameter;
        let checks: [(bool, String); 7] = [
            (self.layer_height > 0.0 && self.layer_height <= n * 0.8, format!("layer_height {} must be in (0, {:.2}] for a {n} mm nozzle", self.layer_height, n * 0.8)),
            (self.first_layer_height > 0.0 && self.first_layer_height <= n * 0.8, format!("first_layer_height {} out of range", self.first_layer_height)),
            (self.infill_density >= 0.0 && self.infill_density <= 1.0, format!("infill_density {} must be a fraction 0..1 (e.g. 0.15 for 15%)", self.infill_density)),
            (self.outer_wall_width >= n * 0.6 && self.outer_wall_width <= n * 2.0, format!("outer_wall_width {} out of range for a {n} mm nozzle", self.outer_wall_width)),
            (self.nozzle_temp >= 150.0 && self.nozzle_temp <= 350.0, format!("nozzle_temp {} out of range", self.nozzle_temp)),
            (self.bed_temp >= 0.0 && self.bed_temp <= 120.0, format!("bed_temp {} out of range", self.bed_temp)),
            (self.extruder <= 1, format!("extruder must be 0 (left) or 1 (right), got {}", self.extruder)),
        ];
        for (ok, msg) in checks {
            if !ok {
                return Err(msg);
            }
        }
        Ok(())
    }

    /// Cross-section area of an extrusion (Slic3r rounded-rectangle model), mm².
    pub fn bead_area(width: f64, height: f64) -> f64 {
        (width - height) * height + std::f64::consts::PI * (height / 2.0).powi(2)
    }

    /// Distance between neighbouring bead centre lines.
    pub fn spacing(width: f64, height: f64) -> f64 {
        width - height * (1.0 - std::f64::consts::FRAC_PI_4)
    }

    pub fn filament_area(&self) -> f64 {
        std::f64::consts::PI * (self.filament_diameter / 2.0).powi(2)
    }
}

/// UI/agent metadata for one setting (drives the settings editor and `settings_catalog`).
#[derive(Clone, Debug, Serialize)]
pub struct SettingInfo {
    pub key: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    /// "number" | "integer" | "bool" | "enum"
    pub kind: &'static str,
    pub unit: &'static str,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub options: Vec<&'static str>,
    pub description: &'static str,
    pub advanced: bool,
    /// Stored as a 0..1 fraction, shown as a percentage.
    pub percent: bool,
}

pub const GROUPS: [(&str, &str); 11] = [
    ("quality", "Quality"),
    ("walls", "Walls & shells"),
    ("infill", "Infill"),
    ("support", "Support"),
    ("adhesion", "Adhesion (brim & skirt)"),
    ("speed", "Speed"),
    ("acceleration", "Acceleration"),
    ("temperature", "Temperature"),
    ("cooling", "Cooling"),
    ("retraction", "Retraction & travel"),
    ("filament", "Filament & machine"),
];

#[allow(clippy::too_many_arguments)]
fn s(key: &'static str, label: &'static str, group: &'static str, kind: &'static str, unit: &'static str, min: f64, max: f64, step: f64, description: &'static str, advanced: bool) -> SettingInfo {
    SettingInfo { key, label, group, kind, unit, min: Some(min), max: Some(max), step: Some(step), options: Vec::new(), description, advanced, percent: false }
}

/// Every editable setting with its group, unit, range and explanation.
pub fn catalog() -> Vec<SettingInfo> {
    let pct = |mut i: SettingInfo| {
        i.percent = true;
        i
    };
    let opts = |mut i: SettingInfo, o: Vec<&'static str>| {
        i.kind = "enum";
        i.options = o;
        i.min = None;
        i.max = None;
        i.step = None;
        i
    };
    let b = |key, label, group, description, advanced| SettingInfo { key, label, group, kind: "bool", unit: "", min: None, max: None, step: None, options: Vec::new(), description, advanced, percent: false };
    vec![
        s("layer_height", "Layer height", "quality", "number", "mm", 0.04, 0.32, 0.02, "Thinner layers look smoother but take longer (0.2 mm is the standard for a 0.4 mm nozzle).", false),
        s("first_layer_height", "First layer height", "quality", "number", "mm", 0.1, 0.32, 0.02, "A thicker first layer forgives an uneven bed.", true),
        opts(s("seam", "Seam position", "quality", "enum", "", 0.0, 0.0, 0.0, "Where each wall loop starts and ends: rear hides it at the back, nearest is fastest.", false), vec!["rear", "nearest"]),
        b("ironing", "Ironing", "quality", "A slow, nearly dry pass over top surfaces for a smooth finish.", false),
        s("outer_wall_width", "Outer wall line width", "quality", "number", "mm", 0.3, 0.8, 0.01, "Width of the visible outer wall.", true),
        s("inner_wall_width", "Inner wall line width", "quality", "number", "mm", 0.3, 0.8, 0.01, "", true),
        s("first_layer_width", "First layer line width", "quality", "number", "mm", 0.3, 0.8, 0.01, "Wider first-layer lines stick better.", true),
        s("wall_loops", "Wall loops", "walls", "integer", "", 1.0, 12.0, 1.0, "Number of perimeters. More walls = stronger and more watertight.", false),
        opts(s("wall_order", "Wall order", "walls", "enum", "", 0.0, 0.0, 0.0, "inner_outer gives cleaner overhangs; outer_inner gives better dimensional accuracy.", true), vec!["inner_outer", "outer_inner"]),
        s("top_layers", "Top solid layers", "walls", "integer", "", 0.0, 20.0, 1.0, "Solid layers closing the top. Raise for smoother tops over sparse infill.", false),
        s("bottom_layers", "Bottom solid layers", "walls", "integer", "", 0.0, 20.0, 1.0, "Solid layers at the bottom.", false),
        pct(s("infill_density", "Sparse infill density", "infill", "number", "%", 0.0, 1.0, 0.01, "How much of the inside is filled (15% is standard; 25–40% for functional parts).", false)),
        opts(s("infill_pattern", "Infill pattern", "infill", "enum", "", 0.0, 0.0, 0.0, "grid: strong in two directions; rectilinear: fast; concentric: follows the walls.", false), vec!["grid", "rectilinear", "concentric"]),
        s("infill_angle", "Infill angle", "infill", "number", "°", 0.0, 180.0, 5.0, "Direction of infill lines.", true),
        pct(s("infill_wall_overlap", "Infill/wall overlap", "infill", "number", "%", 0.0, 0.5, 0.01, "How far infill reaches into the walls (bonding).", true)),
        s("infill_width", "Infill line width", "infill", "number", "mm", 0.3, 0.8, 0.01, "", true),
        s("solid_infill_width", "Solid infill line width", "infill", "number", "mm", 0.3, 0.8, 0.01, "", true),
        s("top_surface_width", "Top surface line width", "infill", "number", "mm", 0.3, 0.8, 0.01, "", true),
        b("support", "Enable supports", "support", "Grid supports under overhangs flatter than the threshold angle. Try auto-orient first.", false),
        s("support_angle", "Threshold angle", "support", "number", "°", 5.0, 85.0, 1.0, "Overhangs flatter than this angle (from horizontal) get support.", false),
        b("support_on_plate_only", "On build plate only", "support", "Only build supports that stand on the plate (never on the model).", false),
        s("support_spacing", "Support line spacing", "support", "number", "mm", 0.5, 10.0, 0.1, "Distance between support lines: wider = less material, easier removal.", true),
        s("support_interface_layers", "Interface layers", "support", "integer", "", 0.0, 6.0, 1.0, "Dense layers on top of the support for a cleaner underside.", false),
        s("support_interface_spacing", "Interface line spacing", "support", "number", "mm", 0.2, 3.0, 0.05, "", true),
        s("support_xy_gap", "XY distance to model", "support", "number", "mm", 0.0, 3.0, 0.05, "Horizontal gap between support and the part.", true),
        s("support_z_gap_layers", "Top Z distance (layers)", "support", "integer", "", 0.0, 5.0, 1.0, "Empty layers between support and the part: more = easier removal, rougher underside.", false),
        s("support_speed", "Support speed", "support", "number", "mm/s", 10.0, 500.0, 5.0, "", true),
        s("support_interface_speed", "Support interface speed", "support", "number", "mm/s", 10.0, 300.0, 5.0, "", true),
        s("brim_width", "Brim width", "adhesion", "number", "mm", 0.0, 20.0, 0.5, "Extra loops around the first layer to stop warping and tipping (0 = none).", false),
        s("brim_gap", "Brim-object gap", "adhesion", "number", "mm", 0.0, 1.0, 0.05, "Small gap makes the brim easy to peel off.", true),
        s("skirt_loops", "Skirt loops", "adhesion", "integer", "", 0.0, 10.0, 1.0, "Loops drawn around the parts before printing them (primes the nozzle).", false),
        s("skirt_distance", "Skirt distance", "adhesion", "number", "mm", 0.5, 20.0, 0.5, "Gap between skirt and parts.", true),
        s("outer_wall_speed", "Outer wall", "speed", "number", "mm/s", 10.0, 600.0, 5.0, "Slower outer walls look better.", false),
        s("inner_wall_speed", "Inner wall", "speed", "number", "mm/s", 10.0, 600.0, 5.0, "", false),
        s("sparse_infill_speed", "Sparse infill", "speed", "number", "mm/s", 10.0, 600.0, 5.0, "", false),
        s("solid_infill_speed", "Internal solid infill", "speed", "number", "mm/s", 10.0, 600.0, 5.0, "", true),
        s("top_surface_speed", "Top surface", "speed", "number", "mm/s", 10.0, 600.0, 5.0, "", false),
        s("bridge_speed", "Bridge", "speed", "number", "mm/s", 5.0, 200.0, 5.0, "", true),
        s("first_layer_speed", "First layer", "speed", "number", "mm/s", 5.0, 200.0, 5.0, "", false),
        s("first_layer_infill_speed", "First layer infill", "speed", "number", "mm/s", 5.0, 300.0, 5.0, "", true),
        s("travel_speed", "Travel", "speed", "number", "mm/s", 50.0, 1000.0, 10.0, "", true),
        s("min_print_speed", "Minimum print speed", "speed", "number", "mm/s", 5.0, 100.0, 1.0, "Floor for cooling slowdown.", true),
        s("default_accel", "Default", "acceleration", "number", "mm/s²", 500.0, 20000.0, 500.0, "", true),
        s("outer_wall_accel", "Outer wall", "acceleration", "number", "mm/s²", 500.0, 20000.0, 500.0, "", true),
        s("top_surface_accel", "Top surface", "acceleration", "number", "mm/s²", 500.0, 20000.0, 500.0, "", true),
        s("first_layer_accel", "First layer", "acceleration", "number", "mm/s²", 100.0, 5000.0, 100.0, "", true),
        s("travel_accel", "Travel", "acceleration", "number", "mm/s²", 500.0, 20000.0, 500.0, "", true),
        s("nozzle_temp", "Nozzle", "temperature", "number", "°C", 150.0, 320.0, 5.0, "From the filament preset; override for tests.", false),
        s("first_layer_nozzle_temp", "Nozzle (first layer)", "temperature", "number", "°C", 150.0, 320.0, 5.0, "", false),
        s("bed_temp", "Bed", "temperature", "number", "°C", 0.0, 120.0, 5.0, "Plate temperature for the chosen plate type.", false),
        s("first_layer_bed_temp", "Bed (first layer)", "temperature", "number", "°C", 0.0, 120.0, 5.0, "", false),
        pct(s("fan_min", "Part fan minimum", "cooling", "number", "%", 0.0, 1.0, 0.05, "", false)),
        pct(s("fan_max", "Part fan maximum", "cooling", "number", "%", 0.0, 1.0, 0.05, "", false)),
        pct(s("aux_fan", "Auxiliary fan", "cooling", "number", "%", 0.0, 1.0, 0.05, "Side fan (PLA likes it, PETG/ASA less).", false)),
        s("fan_off_first_layers", "No cooling for first layers", "cooling", "integer", "", 0.0, 10.0, 1.0, "", true),
        s("min_layer_time", "Minimum layer time", "cooling", "number", "s", 0.0, 60.0, 1.0, "Slow small layers down so they can cool.", true),
        s("fan_full_below_layer_time", "Full fan below layer time", "cooling", "number", "s", 0.0, 300.0, 5.0, "", true),
        s("retract_length", "Retraction length", "retraction", "number", "mm", 0.0, 5.0, 0.05, "Pulls filament back on travel to stop stringing.", false),
        s("retract_speed", "Retraction speed", "retraction", "number", "mm/s", 5.0, 100.0, 5.0, "", true),
        s("deretract_speed", "Deretraction speed", "retraction", "number", "mm/s", 5.0, 100.0, 5.0, "", true),
        s("retract_min_travel", "Minimum travel for retraction", "retraction", "number", "mm", 0.0, 10.0, 0.5, "", true),
        s("z_hop", "Z hop", "retraction", "number", "mm", 0.0, 2.0, 0.05, "Lift the nozzle on travel to avoid scuffing tops.", false),
        s("wipe_distance", "Wipe distance", "retraction", "number", "mm", 0.0, 5.0, 0.1, "", true),
        s("flow_ratio", "Flow ratio", "filament", "number", "", 0.8, 1.2, 0.005, "Extrusion multiplier from flow calibration.", false),
        s("max_volumetric_speed", "Max volumetric speed", "filament", "number", "mm³/s", 1.0, 60.0, 0.5, "Caps speed so the hotend can melt enough filament.", true),
        s("filament_density", "Density", "filament", "number", "g/cm³", 0.8, 2.0, 0.01, "Used for grams and cost.", true),
        s("filament_cost_per_kg", "Cost per kg", "filament", "number", "", 0.0, 200.0, 0.5, "", true),
        opts(s("extruder", "Nozzle", "filament", "enum", "", 0.0, 0.0, 0.0, "H2D nozzle (normally chosen from the filament slot): 0 = left, 1 = right.", true), vec!["0", "1"]),
    ]
}
