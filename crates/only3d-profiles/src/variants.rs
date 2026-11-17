//! Which preset keys are indexed per extruder *variant* (copied from Bambu Studio
//! `src/libslic3r/PrintConfig.cpp`: `print_options_with_variant`, `filament_options_with_variant`,
//! `printer_options_with_variant_1`, `printer_options_with_variant_2`, `printer_extruder_options`).
//!
//! Before G-code generation Bambu collapses these arrays to the values of the variant each
//! extruder (machine/process keys) or each filament (filament keys) actually uses. See
//! [`crate::Config::variant_index`].

/// Process keys with one entry per `print_extruder_variant` row.
pub const PROCESS_VARIANT_KEYS: &[&str] = &[
    "initial_layer_speed",
    "initial_layer_infill_speed",
    "outer_wall_speed",
    "inner_wall_speed",
    "small_perimeter_speed",
    "small_perimeter_threshold",
    "sparse_infill_speed",
    "internal_solid_infill_speed",
    "vertical_shell_speed",
    "top_surface_speed",
    "enable_overhang_speed",
    "overhang_1_4_speed",
    "overhang_2_4_speed",
    "overhang_3_4_speed",
    "overhang_4_4_speed",
    "overhang_totally_speed",
    "enable_height_slowdown",
    "slowdown_start_height",
    "slowdown_start_speed",
    "slowdown_start_acc",
    "slowdown_end_height",
    "slowdown_end_speed",
    "slowdown_end_acc",
    "bridge_speed",
    "gap_infill_speed",
    "support_speed",
    "support_interface_speed",
    "travel_speed",
    "travel_speed_z",
    "default_acceleration",
    "travel_acceleration",
    "travel_short_distance_acceleration",
    "initial_layer_travel_acceleration",
    "initial_layer_acceleration",
    "outer_wall_acceleration",
    "inner_wall_acceleration",
    "sparse_infill_acceleration",
    "top_surface_acceleration",
    "print_extruder_id",
    "print_extruder_variant",
    "top_solid_infill_flow_ratio",
];

/// Filament keys with one entry per `filament_extruder_variant` row.
pub const FILAMENT_VARIANT_KEYS: &[&str] = &[
    "filament_flow_ratio",
    "filament_max_volumetric_speed",
    "filament_ramming_volumetric_speed",
    "filament_pre_cooling_temperature",
    "filament_ramming_travel_time",
    "filament_ramming_volumetric_speed_nc",
    "filament_pre_cooling_temperature_nc",
    "filament_ramming_travel_time_nc",
    "filament_extruder_variant",
    "filament_retraction_length",
    "filament_retract_length_nc",
    "filament_retract_length_toolchange",
    "filament_retract_restart_extra_toolchange",
    "filament_z_hop",
    "filament_z_hop_types",
    "filament_retract_restart_extra",
    "filament_retraction_speed",
    "filament_deretraction_speed",
    "filament_retraction_minimum_travel",
    "filament_retract_when_changing_layer",
    "filament_wipe",
    "filament_wipe_distance",
    "filament_retract_before_wipe",
    "filament_long_retractions_when_cut",
    "filament_retraction_distances_when_cut",
    "long_retractions_when_ec",
    "retraction_distances_when_ec",
    "nozzle_temperature_initial_layer",
    "nozzle_temperature",
    "filament_flush_volumetric_speed",
    "filament_flush_temp",
    "filament_flush_temp_fast",
    "filament_enable_overhang_speed",
    "filament_bridge_speed",
    "filament_overhang_1_4_speed",
    "filament_overhang_2_4_speed",
    "filament_overhang_3_4_speed",
    "filament_overhang_4_4_speed",
    "filament_overhang_totally_speed",
    "override_process_overhang_speed",
    "volumetric_speed_coefficients",
    "filament_adaptive_volumetric_speed",
    "filament_preheat_temperature_delta",
    "filament_cooling_before_tower",
    "slow_down_min_speed",
];

/// Machine keys with one entry per `printer_extruder_variant` row.
pub const PRINTER_VARIANT_KEYS: &[&str] = &[
    "nozzle_volume",
    "retraction_length",
    "z_hop",
    "retract_lift_above",
    "retract_lift_below",
    "z_hop_types",
    "retraction_speed",
    "deretraction_speed",
    "retraction_minimum_travel",
    "retract_when_changing_layer",
    "wipe",
    "wipe_distance",
    "retract_before_wipe",
    "retract_length_toolchange",
    "retract_restart_extra",
    "retract_restart_extra_toolchange",
    "long_retractions_when_cut",
    "retraction_distances_when_cut",
    "printer_extruder_id",
    "printer_extruder_variant",
    "hotend_cooling_rate",
    "hotend_cooling_rate_on_rack",
    "hotend_heating_rate",
    "nozzle_flush_dataset",
];

/// Machine keys with **two** entries per `printer_extruder_variant` row: `[normal, silent]`.
pub const PRINTER_VARIANT_X2_KEYS: &[&str] = &[
    "machine_max_acceleration_x",
    "machine_max_acceleration_y",
    "machine_max_acceleration_z",
    "machine_max_acceleration_e",
    "machine_max_acceleration_extruding",
    "machine_max_acceleration_retracting",
    "machine_max_acceleration_travel",
    "machine_max_speed_x",
    "machine_max_speed_y",
    "machine_max_speed_z",
    "machine_max_speed_e",
    "machine_max_jerk_x",
    "machine_max_jerk_y",
    "machine_max_jerk_z",
    "machine_max_jerk_e",
];

/// Machine keys with one entry per physical extruder (index 0 = left on H2D).
pub const PRINTER_EXTRUDER_KEYS: &[&str] = &[
    "extruder_type",
    "nozzle_diameter",
    "extruder_printable_area",
    "extruder_printable_height",
    "min_layer_height",
    "max_layer_height",
    "extruder_max_nozzle_count",
];

/// Filament keys that, when not `nil`, override the machine key without the `filament_` prefix
/// for the extruder the filament is printed with (Bambu `filament_extruder_override_keys`).
pub const FILAMENT_OVERRIDE_KEYS: &[&str] = &[
    "filament_retraction_length",
    "filament_retract_length_toolchange",
    "filament_retract_restart_extra_toolchange",
    "filament_z_hop",
    "filament_z_hop_types",
    "filament_retract_lift_above",
    "filament_retract_lift_below",
    "filament_retraction_speed",
    "filament_deretraction_speed",
    "filament_retract_restart_extra",
    "filament_retraction_minimum_travel",
    "filament_wipe_distance",
    "filament_retract_when_changing_layer",
    "filament_wipe",
    "filament_retract_before_wipe",
    "filament_long_retractions_when_cut",
    "filament_retraction_distances_when_cut",
];

/// Is `key` indexed per variant in a preset of the given kind?
pub fn is_variant_key(kind: crate::PresetKind, key: &str) -> bool {
    match kind {
        crate::PresetKind::Machine => {
            PRINTER_VARIANT_KEYS.contains(&key) || PRINTER_VARIANT_X2_KEYS.contains(&key)
        }
        crate::PresetKind::Process => PROCESS_VARIANT_KEYS.contains(&key),
        crate::PresetKind::Filament => FILAMENT_VARIANT_KEYS.contains(&key),
        crate::PresetKind::MachineModel => false,
    }
}
