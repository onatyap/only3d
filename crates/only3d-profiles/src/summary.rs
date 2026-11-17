//! Typed views of resolved machine / filament / process presets.
//!
//! All per-variant arrays are read at the row of the requested extruder variant (see
//! [`Config::variant_index`]); per-extruder arrays (`nozzle_diameter`,
//! `extruder_printable_area`, ...) at the extruder index. Extruder indices are **0-based
//! logical** indices: on the H2D `0` is the left nozzle and `1` the right nozzle.

use serde::Serialize;

use crate::config::{Config, VariantTable, DEFAULT_VARIANT};
use crate::error::ProfileError;

/// Build plate types (`curr_bed_type` values).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum BedType {
    CoolPlate,
    EngineeringPlate,
    HighTempPlate,
    TexturedPeiPlate,
    SupertackPlate,
}

impl BedType {
    pub const ALL: [BedType; 5] = [
        BedType::CoolPlate,
        BedType::EngineeringPlate,
        BedType::HighTempPlate,
        BedType::TexturedPeiPlate,
        BedType::SupertackPlate,
    ];

    /// Bambu's name (the value of `curr_bed_type` templates compare against).
    pub fn name(self) -> &'static str {
        match self {
            BedType::CoolPlate => "Cool Plate",
            BedType::EngineeringPlate => "Engineering Plate",
            BedType::HighTempPlate => "High Temp Plate",
            BedType::TexturedPeiPlate => "Textured PEI Plate",
            BedType::SupertackPlate => "Supertack Plate",
        }
    }

    pub fn from_name(name: &str) -> Option<BedType> {
        Self::ALL.into_iter().find(|b| b.name() == name)
    }

    /// Filament key with the bed temperature for layers > 1 (e.g. `textured_plate_temp`).
    pub fn temp_key(self) -> &'static str {
        match self {
            BedType::CoolPlate => "cool_plate_temp",
            BedType::EngineeringPlate => "eng_plate_temp",
            BedType::HighTempPlate => "hot_plate_temp",
            BedType::TexturedPeiPlate => "textured_plate_temp",
            BedType::SupertackPlate => "supertack_plate_temp",
        }
    }

    /// Filament key with the first-layer bed temperature (e.g. `textured_plate_temp_initial_layer`).
    pub fn initial_layer_temp_key(self) -> &'static str {
        match self {
            BedType::CoolPlate => "cool_plate_temp_initial_layer",
            BedType::EngineeringPlate => "eng_plate_temp_initial_layer",
            BedType::HighTempPlate => "hot_plate_temp_initial_layer",
            BedType::TexturedPeiPlate => "textured_plate_temp_initial_layer",
            BedType::SupertackPlate => "supertack_plate_temp_initial_layer",
        }
    }
}

fn need_f64(cfg: &Config, key: &str, idx: usize, what: &str) -> Result<f64, ProfileError> {
    cfg.get_f64_at(key, idx).ok_or_else(|| ProfileError::bad_key(key, what))
}

/// `x/y/z/e` values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Axes {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub e: f64,
}

/// Firmware motion limits (`machine_max_*`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct MachineLimits {
    /// mm/s
    pub max_speed: Axes,
    /// mm/s²
    pub max_acceleration: Axes,
    /// mm/s
    pub max_jerk: Axes,
    pub max_acceleration_extruding: f64,
    pub max_acceleration_retracting: f64,
    pub max_acceleration_travel: f64,
}

impl MachineLimits {
    /// `row` is the machine variant row; `silent` selects the second entry of the pair.
    fn read(cfg: &Config, row: usize, silent: bool) -> MachineLimits {
        let i = row * 2 + usize::from(silent);
        // Pairs may be missing for older single-variant presets: fall back to the first pair.
        let g = |k: &str| {
            let n = cfg.len_of(k).unwrap_or(0);
            let idx = if i < n { i } else { usize::from(silent && n > 1) };
            cfg.get_f64_at(k, idx).unwrap_or(0.0)
        };
        let axes = |p: &str| Axes {
            x: g(&format!("{p}_x")),
            y: g(&format!("{p}_y")),
            z: g(&format!("{p}_z")),
            e: g(&format!("{p}_e")),
        };
        MachineLimits {
            max_speed: axes("machine_max_speed"),
            max_acceleration: axes("machine_max_acceleration"),
            max_jerk: axes("machine_max_jerk"),
            max_acceleration_extruding: g("machine_max_acceleration_extruding"),
            max_acceleration_retracting: g("machine_max_acceleration_retracting"),
            max_acceleration_travel: g("machine_max_acceleration_travel"),
        }
    }
}

/// Machine retraction / wipe / z-hop settings of one extruder variant.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Retraction {
    /// mm
    pub length: f64,
    /// mm/s
    pub speed: f64,
    /// mm/s (0 = same as `speed`)
    pub deretraction_speed: f64,
    /// mm
    pub minimum_travel: f64,
    pub restart_extra: f64,
    /// mm retracted before a tool change
    pub length_toolchange: f64,
    pub restart_extra_toolchange: f64,
    pub retract_when_changing_layer: bool,
    pub wipe: bool,
    /// mm
    pub wipe_distance: f64,
    /// Fraction (0..1) of the retraction done before wiping (`retract_before_wipe`, a percentage).
    pub retract_before_wipe: f64,
    /// mm
    pub z_hop: f64,
    /// `Auto Lift`, `Normal Lift`, `Slope Lift`, `Spiral Lift`
    pub z_hop_type: String,
    /// Only lift above / below these heights (mm).
    pub lift_above: f64,
    pub lift_below: f64,
    pub long_retraction_when_cut: bool,
    pub retraction_distance_when_cut: f64,
}

impl Retraction {
    fn read(cfg: &Config, row: usize) -> Retraction {
        let f = |k: &str| cfg.get_f64_at(k, row).unwrap_or(0.0);
        let b = |k: &str| cfg.get_bool_at(k, row).unwrap_or(false);
        Retraction {
            length: f("retraction_length"),
            speed: f("retraction_speed"),
            deretraction_speed: f("deretraction_speed"),
            minimum_travel: f("retraction_minimum_travel"),
            restart_extra: f("retract_restart_extra"),
            length_toolchange: f("retract_length_toolchange"),
            restart_extra_toolchange: f("retract_restart_extra_toolchange"),
            retract_when_changing_layer: b("retract_when_changing_layer"),
            wipe: b("wipe"),
            wipe_distance: f("wipe_distance"),
            retract_before_wipe: cfg.get_percent_or_abs_at("retract_before_wipe", row, 1.0).unwrap_or(0.0),
            z_hop: f("z_hop"),
            z_hop_type: cfg.get_str_at("z_hop_types", row).unwrap_or("Auto Lift").to_string(),
            lift_above: f("retract_lift_above"),
            lift_below: f("retract_lift_below"),
            long_retraction_when_cut: b("long_retractions_when_cut"),
            retraction_distance_when_cut: f("retraction_distances_when_cut"),
        }
    }
}

/// One physical extruder of a machine with its selected variant.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ExtruderSummary {
    /// 0-based logical index (H2D: 0 = left, 1 = right).
    pub index: usize,
    /// Firmware/physical index (`physical_extruder_map`; H2D: left = 1, right = 0).
    pub physical_index: usize,
    /// E.g. `Direct Drive Standard`.
    pub variant: String,
    /// Row of the per-variant machine arrays.
    pub variant_row: usize,
    /// Variants this extruder supports (`extruder_variant_list`).
    pub supported_variants: Vec<String>,
    pub extruder_type: String,
    pub nozzle_diameter: f64,
    /// Area this nozzle can reach (`extruder_printable_area`), bed coordinates.
    pub printable_area: Vec<[f64; 2]>,
    pub printable_height: Option<f64>,
    pub min_layer_height: Option<f64>,
    pub max_layer_height: Option<f64>,
    /// mm³ (`nozzle_volume`)
    pub nozzle_volume: Option<f64>,
    /// °C/s
    pub hotend_heating_rate: Option<f64>,
    pub hotend_cooling_rate: Option<f64>,
    pub limits: MachineLimits,
    pub limits_silent: MachineLimits,
    pub retraction: Retraction,
}

/// Extruder collision envelope (sequential printing).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct ExtruderClearance {
    pub radius: f64,
    pub max_radius: f64,
    pub height_to_rod: f64,
    pub height_to_lid: f64,
    pub dist_to_rod: f64,
}

/// Typed view of a resolved machine preset.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MachineSummary {
    pub printer_model: Option<String>,
    pub printer_variant: Option<String>,
    /// Bed polygon (`printable_area`), mm.
    pub printable_area: Vec<[f64; 2]>,
    /// Bounding box of `printable_area`.
    pub bed_min: [f64; 2],
    pub bed_size: [f64; 2],
    /// `printable_height`, mm.
    pub max_print_height: f64,
    pub extruders: Vec<ExtruderSummary>,
    /// 0-based logical index of the main extruder (`master_extruder_id` is 1-based; H2D: right).
    pub master_extruder: Option<usize>,
    pub clearance: ExtruderClearance,
    pub gcode_flavor: Option<String>,
    pub bed_exclude_area: Vec<[f64; 2]>,
    pub wrapping_exclude_area: Vec<[f64; 2]>,
    pub head_wrap_detect_zone: Vec<[f64; 2]>,
    /// `by_first_filament` or `by_highest_temp`.
    pub bed_temperature_formula: Option<String>,
    pub support_chamber_temp_control: bool,
    pub support_air_filtration: bool,
    pub scan_first_layer: bool,
}

impl MachineSummary {
    /// `extruder_variants[i]` is the variant installed on logical extruder `i`; missing entries
    /// default to `Direct Drive Standard`.
    pub fn from_config(cfg: &Config, extruder_variants: &[&str]) -> Result<Self, ProfileError> {
        let printable_area = cfg
            .get_points("printable_area")
            .filter(|p| !p.is_empty())
            .ok_or_else(|| ProfileError::bad_key("printable_area", "machine"))?;
        let (mut min, mut max) = ([f64::MAX; 2], [f64::MIN; 2]);
        for p in &printable_area {
            for a in 0..2 {
                min[a] = min[a].min(p[a]);
                max[a] = max[a].max(p[a]);
            }
        }
        let n = cfg.len_of("nozzle_diameter").ok_or_else(|| ProfileError::bad_key("nozzle_diameter", "machine"))?;
        let phys = cfg.get_strs("physical_extruder_map").map(|v| v.to_vec()).unwrap_or_default();
        let mut extruders = Vec::with_capacity(n);
        for e in 0..n {
            let variant = extruder_variants.get(e).copied().unwrap_or(DEFAULT_VARIANT).to_string();
            let row = cfg.variant_index_or_zero(VariantTable::Printer, e, &variant);
            extruders.push(ExtruderSummary {
                index: e,
                physical_index: phys.get(e).and_then(|s| s.trim().parse().ok()).unwrap_or(e),
                variant_row: row,
                supported_variants: cfg
                    .get_str_at("extruder_variant_list", e)
                    .map(|s| s.split(',').map(|v| v.trim().to_string()).collect())
                    .unwrap_or_default(),
                variant,
                extruder_type: cfg.get_str_at("extruder_type", e).unwrap_or("Direct Drive").to_string(),
                nozzle_diameter: need_f64(cfg, "nozzle_diameter", e, "machine")?,
                printable_area: cfg
                    .get_polygon_at("extruder_printable_area", e)
                    .unwrap_or_else(|| printable_area.clone()),
                printable_height: cfg.get_f64_at("extruder_printable_height", e),
                min_layer_height: cfg.get_f64_at("min_layer_height", e),
                max_layer_height: cfg.get_f64_at("max_layer_height", e),
                nozzle_volume: cfg.get_f64_at("nozzle_volume", row),
                hotend_heating_rate: cfg.get_f64_at("hotend_heating_rate", row),
                hotend_cooling_rate: cfg.get_f64_at("hotend_cooling_rate", row),
                limits: MachineLimits::read(cfg, row, false),
                limits_silent: MachineLimits::read(cfg, row, true),
                retraction: Retraction::read(cfg, row),
            });
        }
        let f = |k: &str| cfg.get_f64(k).unwrap_or(0.0);
        Ok(MachineSummary {
            printer_model: cfg.get_str("printer_model").map(str::to_string),
            printer_variant: cfg.get_str("printer_variant").map(str::to_string),
            bed_min: min,
            bed_size: [max[0] - min[0], max[1] - min[1]],
            printable_area,
            max_print_height: cfg
                .get_f64("printable_height")
                .ok_or_else(|| ProfileError::bad_key("printable_height", "machine"))?,
            extruders,
            master_extruder: cfg.get_i64("master_extruder_id").filter(|&i| i >= 1).map(|i| i as usize - 1),
            clearance: ExtruderClearance {
                radius: f("extruder_clearance_radius"),
                max_radius: f("extruder_clearance_max_radius"),
                height_to_rod: f("extruder_clearance_height_to_rod"),
                height_to_lid: f("extruder_clearance_height_to_lid"),
                dist_to_rod: f("extruder_clearance_dist_to_rod"),
            },
            gcode_flavor: cfg.get_str("gcode_flavor").map(str::to_string),
            bed_exclude_area: cfg.get_points("bed_exclude_area").unwrap_or_default(),
            wrapping_exclude_area: cfg.get_points("wrapping_exclude_area").unwrap_or_default(),
            head_wrap_detect_zone: cfg.get_points("head_wrap_detect_zone").unwrap_or_default(),
            bed_temperature_formula: cfg.get_str("bed_temperature_formula").map(str::to_string),
            support_chamber_temp_control: cfg.get_bool("support_chamber_temp_control").unwrap_or(false),
            support_air_filtration: cfg.get_bool("support_air_filtration").unwrap_or(false),
            scan_first_layer: cfg.get_bool("scan_first_layer").unwrap_or(false),
        })
    }
}

/// First-layer / other-layer bed temperature of one plate type (°C; 0 = plate not supported).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct PlateTemps {
    pub initial_layer: f64,
    pub other_layers: f64,
}

/// Part-cooling and exhaust fan settings (percentages unless noted).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct FanSettings {
    pub min_speed: f64,
    pub max_speed: f64,
    /// Layer time (s) below which the fan ramps from min to max.
    pub fan_cooling_layer_time: f64,
    /// Layer time (s) below which printing slows down.
    pub slow_down_layer_time: f64,
    /// mm/s
    pub slow_down_min_speed: f64,
    pub close_fan_the_first_x_layers: i64,
    pub full_fan_speed_layer: i64,
    pub overhang_fan_speed: f64,
    /// e.g. `50%` (overhang ratio threshold), kept verbatim.
    pub overhang_fan_threshold: String,
    /// Auxiliary (side) fan.
    pub additional_cooling_fan_speed: f64,
    pub during_print_exhaust_fan_speed: f64,
    pub complete_print_exhaust_fan_speed: f64,
    pub reduce_fan_stop_start_freq: bool,
    pub slow_down_for_layer_cooling: bool,
}

/// Filament overrides of the machine retraction settings; `None` = `nil` (use the machine's).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct RetractionOverrides {
    pub length: Option<f64>,
    pub speed: Option<f64>,
    pub deretraction_speed: Option<f64>,
    pub minimum_travel: Option<f64>,
    pub restart_extra: Option<f64>,
    pub length_toolchange: Option<f64>,
    pub retract_when_changing_layer: Option<bool>,
    pub wipe: Option<bool>,
    pub wipe_distance: Option<f64>,
    pub retract_before_wipe: Option<f64>,
    pub z_hop: Option<f64>,
    pub z_hop_type: Option<String>,
    pub long_retraction_when_cut: Option<bool>,
    pub retraction_distance_when_cut: Option<f64>,
}

/// Typed view of a resolved filament preset for one extruder variant.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FilamentSummary {
    pub filament_type: String,
    pub vendor: Option<String>,
    pub filament_id: Option<String>,
    pub variant: String,
    pub variant_row: usize,
    /// mm
    pub diameter: f64,
    /// g/cm³
    pub density: f64,
    /// money per kg
    pub cost: f64,
    pub flow_ratio: f64,
    /// mm³/s
    pub max_volumetric_speed: f64,
    pub nozzle_temperature: f64,
    pub nozzle_temperature_initial_layer: f64,
    /// Recommended range `[low, high]`.
    pub nozzle_temperature_range: [f64; 2],
    pub cool_plate_temp: PlateTemps,
    pub eng_plate_temp: PlateTemps,
    pub hot_plate_temp: PlateTemps,
    pub textured_plate_temp: PlateTemps,
    pub supertack_plate_temp: PlateTemps,
    pub chamber_temperature: f64,
    /// Softening temperature; the H2D start G-code picks chamber cooling by it.
    pub temperature_vitrification: f64,
    pub fan: FanSettings,
    pub retraction: RetractionOverrides,
    pub is_support: bool,
    pub soluble: bool,
    /// Flush (purge) temperature, 0 = use `nozzle_temperature_range` high.
    pub flush_temperature: f64,
    /// Flush volumetric speed, 0 = use `max_volumetric_speed`.
    pub flush_volumetric_speed: f64,
    pub pressure_advance: Option<f64>,
    pub compatible_printers: Vec<String>,
}

impl FilamentSummary {
    /// `variant` is the variant of the extruder the filament is printed with.
    pub fn from_config(cfg: &Config, variant: &str) -> Result<Self, ProfileError> {
        let row = cfg.variant_index_or_zero(VariantTable::Filament, 0, variant);
        let f = |k: &str| cfg.get_f64(k).unwrap_or(0.0);
        let fv = |k: &str| cfg.get_f64_at(k, row).unwrap_or(0.0);
        let plate = |b: BedType| PlateTemps {
            initial_layer: f(b.initial_layer_temp_key()),
            other_layers: f(b.temp_key()),
        };
        let opt_f = |k: &str| cfg.get_f64_at(k, row);
        let opt_b = |k: &str| cfg.get_bool_at(k, row);
        Ok(FilamentSummary {
            filament_type: cfg
                .get_str("filament_type")
                .ok_or_else(|| ProfileError::bad_key("filament_type", "filament"))?
                .to_string(),
            vendor: cfg.get_str("filament_vendor").map(str::to_string),
            filament_id: cfg.get_str("filament_id").map(str::to_string),
            variant: variant.to_string(),
            variant_row: row,
            diameter: cfg.get_f64("filament_diameter").unwrap_or(1.75),
            density: f("filament_density"),
            cost: f("filament_cost"),
            flow_ratio: cfg.get_f64_at("filament_flow_ratio", row).unwrap_or(1.0),
            max_volumetric_speed: fv("filament_max_volumetric_speed"),
            nozzle_temperature: need_f64(cfg, "nozzle_temperature", row, "filament")?,
            nozzle_temperature_initial_layer: need_f64(cfg, "nozzle_temperature_initial_layer", row, "filament")?,
            nozzle_temperature_range: [f("nozzle_temperature_range_low"), f("nozzle_temperature_range_high")],
            cool_plate_temp: plate(BedType::CoolPlate),
            eng_plate_temp: plate(BedType::EngineeringPlate),
            hot_plate_temp: plate(BedType::HighTempPlate),
            textured_plate_temp: plate(BedType::TexturedPeiPlate),
            supertack_plate_temp: plate(BedType::SupertackPlate),
            chamber_temperature: f("chamber_temperatures"),
            temperature_vitrification: f("temperature_vitrification"),
            fan: FanSettings {
                min_speed: f("fan_min_speed"),
                max_speed: f("fan_max_speed"),
                fan_cooling_layer_time: f("fan_cooling_layer_time"),
                slow_down_layer_time: f("slow_down_layer_time"),
                slow_down_min_speed: fv("slow_down_min_speed"),
                close_fan_the_first_x_layers: cfg.get_i64("close_fan_the_first_x_layers").unwrap_or(0),
                full_fan_speed_layer: cfg.get_i64("full_fan_speed_layer").unwrap_or(0),
                overhang_fan_speed: f("overhang_fan_speed"),
                overhang_fan_threshold: cfg.get_str("overhang_fan_threshold").unwrap_or("").to_string(),
                additional_cooling_fan_speed: f("additional_cooling_fan_speed"),
                during_print_exhaust_fan_speed: f("during_print_exhaust_fan_speed"),
                complete_print_exhaust_fan_speed: f("complete_print_exhaust_fan_speed"),
                reduce_fan_stop_start_freq: cfg.get_bool("reduce_fan_stop_start_freq").unwrap_or(false),
                slow_down_for_layer_cooling: cfg.get_bool("slow_down_for_layer_cooling").unwrap_or(true),
            },
            retraction: RetractionOverrides {
                length: opt_f("filament_retraction_length"),
                speed: opt_f("filament_retraction_speed"),
                deretraction_speed: opt_f("filament_deretraction_speed"),
                minimum_travel: opt_f("filament_retraction_minimum_travel"),
                restart_extra: opt_f("filament_retract_restart_extra"),
                length_toolchange: opt_f("filament_retract_length_toolchange"),
                retract_when_changing_layer: opt_b("filament_retract_when_changing_layer"),
                wipe: opt_b("filament_wipe"),
                wipe_distance: opt_f("filament_wipe_distance"),
                retract_before_wipe: if cfg.is_nil_at("filament_retract_before_wipe", row) {
                    None
                } else {
                    cfg.get_percent_or_abs_at("filament_retract_before_wipe", row, 1.0)
                },
                z_hop: opt_f("filament_z_hop"),
                z_hop_type: cfg
                    .get_str_at("filament_z_hop_types", row)
                    .filter(|s| *s != crate::config::NIL)
                    .map(str::to_string),
                long_retraction_when_cut: opt_b("filament_long_retractions_when_cut"),
                retraction_distance_when_cut: opt_f("filament_retraction_distances_when_cut"),
            },
            is_support: cfg.get_bool("filament_is_support").unwrap_or(false),
            soluble: cfg.get_bool("filament_soluble").unwrap_or(false),
            flush_temperature: fv("filament_flush_temp"),
            flush_volumetric_speed: fv("filament_flush_volumetric_speed"),
            pressure_advance: cfg.get_f64("pressure_advance"),
            compatible_printers: cfg.get_strs("compatible_printers").map(|v| v.to_vec()).unwrap_or_default(),
        })
    }

    /// Bed temperatures for a plate type.
    pub fn plate_temps(&self, bed: BedType) -> PlateTemps {
        match bed {
            BedType::CoolPlate => self.cool_plate_temp,
            BedType::EngineeringPlate => self.eng_plate_temp,
            BedType::HighTempPlate => self.hot_plate_temp,
            BedType::TexturedPeiPlate => self.textured_plate_temp,
            BedType::SupertackPlate => self.supertack_plate_temp,
        }
    }
}

/// Extrusion widths in mm (0 in the preset = use `line_width`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct LineWidths {
    pub default: f64,
    pub outer_wall: f64,
    pub inner_wall: f64,
    pub top_surface: f64,
    pub sparse_infill: f64,
    pub internal_solid_infill: f64,
    pub support: f64,
    pub initial_layer: f64,
    pub initial_layer_infill: f64,
}

/// Feature speeds in mm/s (percent-valued settings resolved).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Speeds {
    pub outer_wall: f64,
    pub inner_wall: f64,
    /// Resolved against `outer_wall` when given as a percentage.
    pub small_perimeter: f64,
    pub small_perimeter_threshold: f64,
    pub sparse_infill: f64,
    pub internal_solid_infill: f64,
    /// Resolved against `internal_solid_infill` when given as a percentage.
    pub vertical_shell: f64,
    pub top_surface: f64,
    pub gap_infill: f64,
    pub bridge: f64,
    pub support: f64,
    pub support_interface: f64,
    pub travel: f64,
    /// 0 = machine max
    pub travel_z: f64,
    pub initial_layer: f64,
    pub initial_layer_infill: f64,
    pub enable_overhang_speed: bool,
    /// Overhang speeds for 1/4, 2/4, 3/4, 4/4 overhang and fully floating (0 = no slowdown).
    pub overhang: [f64; 5],
}

/// Accelerations in mm/s² (0 = use `default`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Accelerations {
    pub default: f64,
    pub outer_wall: f64,
    pub inner_wall: f64,
    pub top_surface: f64,
    /// Resolved against `default` when given as a percentage.
    pub sparse_infill: f64,
    pub initial_layer: f64,
    pub travel: f64,
    pub initial_layer_travel: f64,
    pub travel_short_distance: f64,
}

/// Support settings.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SupportSettings {
    pub enabled: bool,
    /// `normal(auto)`, `tree(auto)`, `normal(manual)`, `tree(manual)`
    pub support_type: String,
    pub style: String,
    pub threshold_angle: f64,
    pub on_build_plate_only: bool,
    pub top_z_distance: f64,
    pub bottom_z_distance: f64,
    pub object_xy_distance: f64,
    pub interface_top_layers: i64,
    pub interface_bottom_layers: i64,
    pub interface_spacing: f64,
    pub interface_pattern: String,
    pub base_pattern: String,
    pub base_pattern_spacing: f64,
    /// 1-based filament number, 0 = default.
    pub filament: i64,
    pub interface_filament: i64,
}

/// Brim / skirt settings.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct BrimSettings {
    /// `auto_brim`, `outer_only`, `inner_only`, `outer_and_inner`, `no_brim`, ...
    pub brim_type: String,
    pub width: f64,
    pub object_gap: f64,
    pub skirt_loops: i64,
    pub skirt_distance: f64,
}

/// Typed view of a resolved process preset for one extruder variant.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProcessSummary {
    pub variant: String,
    pub variant_row: usize,
    pub layer_height: f64,
    pub initial_layer_height: f64,
    pub line_widths: LineWidths,
    pub wall_loops: i64,
    pub top_shell_layers: i64,
    pub bottom_shell_layers: i64,
    pub top_shell_thickness: f64,
    pub bottom_shell_thickness: f64,
    /// Percent (e.g. 15.0).
    pub sparse_infill_density: f64,
    pub sparse_infill_pattern: String,
    pub top_surface_pattern: String,
    pub bottom_surface_pattern: String,
    pub infill_direction: f64,
    /// Fraction of the inner wall width.
    pub infill_wall_overlap: f64,
    pub wall_generator: String,
    pub wall_infill_order: String,
    pub seam_position: String,
    pub speeds: Speeds,
    pub accelerations: Accelerations,
    pub support: SupportSettings,
    pub brim: BrimSettings,
    pub enable_prime_tower: bool,
    pub prime_tower_width: f64,
    pub spiral_mode: bool,
    pub print_sequence: String,
    pub ironing_type: String,
    pub top_solid_infill_flow_ratio: f64,
    pub bridge_flow: f64,
    pub elefant_foot_compensation: f64,
    pub xy_hole_compensation: f64,
    pub xy_contour_compensation: f64,
}

impl ProcessSummary {
    /// `extruder` is the 0-based logical extruder, `variant` its installed variant, and
    /// `nozzle_diameter` resolves percentage line widths.
    pub fn from_config(cfg: &Config, extruder: usize, variant: &str, nozzle_diameter: f64) -> Result<Self, ProfileError> {
        let row = cfg.variant_index_or_zero(VariantTable::Process, extruder, variant);
        let f = |k: &str| cfg.get_f64(k).unwrap_or(0.0);
        let fv = |k: &str| cfg.get_f64_at(k, row).unwrap_or(0.0);
        let s = |k: &str| cfg.get_str(k).unwrap_or("").to_string();
        let i = |k: &str| cfg.get_i64(k).unwrap_or(0);
        let b = |k: &str| cfg.get_bool(k).unwrap_or(false);
        let default_width = cfg.get_percent_or_abs("line_width", nozzle_diameter).unwrap_or(nozzle_diameter);
        let width = |k: &str| match cfg.get_percent_or_abs(k, nozzle_diameter) {
            Some(w) if w > 0.0 => w,
            _ => default_width,
        };
        let outer_wall = fv("outer_wall_speed");
        let internal_solid = fv("internal_solid_infill_speed");
        let default_acc = fv("default_acceleration");
        Ok(ProcessSummary {
            variant: variant.to_string(),
            variant_row: row,
            layer_height: cfg
                .get_f64("layer_height")
                .ok_or_else(|| ProfileError::bad_key("layer_height", "process"))?,
            initial_layer_height: cfg
                .get_f64("initial_layer_print_height")
                .ok_or_else(|| ProfileError::bad_key("initial_layer_print_height", "process"))?,
            line_widths: LineWidths {
                default: default_width,
                outer_wall: width("outer_wall_line_width"),
                inner_wall: width("inner_wall_line_width"),
                top_surface: width("top_surface_line_width"),
                sparse_infill: width("sparse_infill_line_width"),
                internal_solid_infill: width("internal_solid_infill_line_width"),
                support: width("support_line_width"),
                initial_layer: width("initial_layer_line_width"),
                initial_layer_infill: width("initial_layer_infill_line_width"),
            },
            wall_loops: i("wall_loops"),
            top_shell_layers: i("top_shell_layers"),
            bottom_shell_layers: i("bottom_shell_layers"),
            top_shell_thickness: f("top_shell_thickness"),
            bottom_shell_thickness: f("bottom_shell_thickness"),
            sparse_infill_density: cfg.get_percent_or_abs("sparse_infill_density", 100.0).unwrap_or(0.0),
            sparse_infill_pattern: s("sparse_infill_pattern"),
            top_surface_pattern: s("top_surface_pattern"),
            bottom_surface_pattern: s("bottom_surface_pattern"),
            infill_direction: f("infill_direction"),
            infill_wall_overlap: cfg.get_percent_or_abs("infill_wall_overlap", 1.0).unwrap_or(0.15),
            wall_generator: s("wall_generator"),
            wall_infill_order: s("wall_infill_order"),
            seam_position: s("seam_position"),
            speeds: Speeds {
                outer_wall,
                inner_wall: fv("inner_wall_speed"),
                small_perimeter: cfg.get_percent_or_abs_at("small_perimeter_speed", row, outer_wall).unwrap_or(outer_wall),
                small_perimeter_threshold: fv("small_perimeter_threshold"),
                sparse_infill: fv("sparse_infill_speed"),
                internal_solid_infill: internal_solid,
                vertical_shell: cfg
                    .get_percent_or_abs_at("vertical_shell_speed", row, internal_solid)
                    .unwrap_or(internal_solid),
                top_surface: fv("top_surface_speed"),
                gap_infill: fv("gap_infill_speed"),
                bridge: fv("bridge_speed"),
                support: fv("support_speed"),
                support_interface: fv("support_interface_speed"),
                travel: fv("travel_speed"),
                travel_z: fv("travel_speed_z"),
                initial_layer: fv("initial_layer_speed"),
                initial_layer_infill: fv("initial_layer_infill_speed"),
                enable_overhang_speed: cfg.get_bool_at("enable_overhang_speed", row).unwrap_or(true),
                overhang: [
                    fv("overhang_1_4_speed"),
                    fv("overhang_2_4_speed"),
                    fv("overhang_3_4_speed"),
                    fv("overhang_4_4_speed"),
                    fv("overhang_totally_speed"),
                ],
            },
            accelerations: Accelerations {
                default: default_acc,
                outer_wall: fv("outer_wall_acceleration"),
                inner_wall: fv("inner_wall_acceleration"),
                top_surface: fv("top_surface_acceleration"),
                sparse_infill: cfg
                    .get_percent_or_abs_at("sparse_infill_acceleration", row, default_acc)
                    .unwrap_or(default_acc),
                initial_layer: fv("initial_layer_acceleration"),
                travel: fv("travel_acceleration"),
                initial_layer_travel: fv("initial_layer_travel_acceleration"),
                travel_short_distance: fv("travel_short_distance_acceleration"),
            },
            support: SupportSettings {
                enabled: b("enable_support"),
                support_type: s("support_type"),
                style: s("support_style"),
                threshold_angle: f("support_threshold_angle"),
                on_build_plate_only: b("support_on_build_plate_only"),
                top_z_distance: f("support_top_z_distance"),
                bottom_z_distance: f("support_bottom_z_distance"),
                object_xy_distance: f("support_object_xy_distance"),
                interface_top_layers: i("support_interface_top_layers"),
                interface_bottom_layers: i("support_interface_bottom_layers"),
                interface_spacing: f("support_interface_spacing"),
                interface_pattern: s("support_interface_pattern"),
                base_pattern: s("support_base_pattern"),
                base_pattern_spacing: f("support_base_pattern_spacing"),
                filament: i("support_filament"),
                interface_filament: i("support_interface_filament"),
            },
            brim: BrimSettings {
                brim_type: cfg.get_str("brim_type").unwrap_or("auto_brim").to_string(),
                width: f("brim_width"),
                object_gap: f("brim_object_gap"),
                skirt_loops: i("skirt_loops"),
                skirt_distance: f("skirt_distance"),
            },
            enable_prime_tower: b("enable_prime_tower"),
            prime_tower_width: f("prime_tower_width"),
            spiral_mode: b("spiral_mode"),
            print_sequence: s("print_sequence"),
            ironing_type: s("ironing_type"),
            top_solid_infill_flow_ratio: cfg.get_f64_at("top_solid_infill_flow_ratio", row).unwrap_or(1.0),
            bridge_flow: cfg.get_f64("bridge_flow").unwrap_or(1.0),
            elefant_foot_compensation: f("elefant_foot_compensation"),
            xy_hole_compensation: f("xy_hole_compensation"),
            xy_contour_compensation: f("xy_contour_compensation"),
        })
    }
}
