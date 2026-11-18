//! Template variables computed the way Bambu Studio's `src/libslic3r/GCode.cpp` computes them.
//!
//! * [`build_print_context`] produces the print-wide variables used by `machine_start_gcode`
//!   (and available to every other template): per-filament arrays collapsed to each filament's
//!   extruder variant, `filament_map`, `first_non_support_filaments`, bed/chamber temperatures,
//!   first-layer bounding box, ... (`GCode::_do_export` + `update_placeholder_parser_with_variant_params`).
//! * [`PrintSetup::layer_change_vars`], [`PrintSetup::toolchange_vars`],
//!   [`PrintSetup::timelapse_vars`], [`PrintSetup::wrapping_detection_vars`],
//!   [`PrintSetup::end_vars`] produce the per-event overrides Bambu passes as `config_override`.
//!   Render with `template.render_layered(&[&event_vars, &print_vars])`.
//!
//! Index conventions (H2D):
//! * **filament id**: 0-based index into the project filament list (`filament_type[i]` ...).
//! * **logical extruder**: 0-based, `0` = left nozzle, `1` = right nozzle. `filament_map[f]` is
//!   the 1-based logical extruder (`1` = left, `2` = right).
//! * **physical extruder** (firmware): `physical_extruder_map[logical]` = `[1, 0]`, i.e. the
//!   right nozzle is physical/firmware extruder `0` (`T0`), the left one `1`. Start G-code uses
//!   `filament_map[f] % 2` to get it. `first_filaments` / `first_non_support_filaments` are
//!   indexed by physical extruder.
//! * **nozzle id**: on the H2D one nozzle per extruder, nozzle id == logical extruder index.
//!   `*_hotend` variables are `-1` (they are only meaningful on machines with a hotend rack).

use crate::config::{parse_f64, volume_type_of_variant, Config, VariantTable, DEFAULT_VARIANT, NIL};
use crate::error::ProfileError;
use crate::summary::BedType;
use crate::template::{Context, Value};

/// Logical index of the H2D left nozzle.
pub const H2D_LEFT: usize = 0;
/// Logical index of the H2D right nozzle (the main extruder, firmware `T0`).
pub const H2D_RIGHT: usize = 1;

/// Bambu `g_min_purge_volume` (mm³).
pub const MIN_PURGE_VOLUME: f64 = 100.0;
/// Bambu `g_purge_volume_one_time` (mm³).
pub const PURGE_VOLUME_ONE_TIME: f64 = 135.0;
/// Bambu `g_max_flush_count`.
pub const MAX_FLUSH_COUNT: usize = 4;

/// One project filament and the extruder it is assigned to.
#[derive(Clone, Copy, Debug)]
pub struct FilamentSlot<'a> {
    /// Resolved filament preset.
    pub config: &'a Config,
    /// 0-based logical extruder (H2D: 0 = left, 1 = right).
    pub extruder: usize,
}

/// The presets of a print: machine, process, filaments (index = filament id) and the variant
/// installed on each extruder.
#[derive(Clone, Debug)]
pub struct PrintSetup<'a> {
    pub machine: &'a Config,
    pub process: &'a Config,
    pub filaments: Vec<FilamentSlot<'a>>,
    /// Variant per logical extruder (`"Direct Drive Standard"`, `"Direct Drive High Flow"`, ...);
    /// missing entries default to `Direct Drive Standard`.
    pub extruder_variants: Vec<String>,
}

fn filament_area(diameter: f64) -> f64 {
    std::f64::consts::PI / 4.0 * diameter * diameter
}

impl<'a> PrintSetup<'a> {
    pub fn new(machine: &'a Config, process: &'a Config) -> Self {
        PrintSetup { machine, process, filaments: Vec::new(), extruder_variants: Vec::new() }
    }

    /// Adds the next filament (its id is the number of filaments added before).
    pub fn with_filament(mut self, config: &'a Config, extruder: usize) -> Self {
        self.filaments.push(FilamentSlot { config, extruder });
        self
    }

    pub fn with_extruder_variants(mut self, variants: &[&str]) -> Self {
        self.extruder_variants = variants.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Number of physical extruders (`nozzle_diameter` entries).
    pub fn extruder_count(&self) -> usize {
        self.machine.len_of("nozzle_diameter").unwrap_or(1).max(1)
    }

    pub fn extruder_variant(&self, extruder: usize) -> &str {
        self.extruder_variants.get(extruder).map_or(DEFAULT_VARIANT, String::as_str)
    }

    /// Firmware extruder index (`physical_extruder_map`).
    pub fn physical_extruder(&self, extruder: usize) -> usize {
        self.machine
            .get_i64_at("physical_extruder_map", extruder)
            .filter(|_| self.machine.len_of("physical_extruder_map").unwrap_or(0) > extruder)
            .map_or(extruder, |v| v.max(0) as usize)
    }

    /// Row of the machine per-variant arrays for `extruder`.
    pub fn machine_row(&self, extruder: usize) -> usize {
        self.machine.variant_index_or_zero(VariantTable::Printer, extruder, self.extruder_variant(extruder))
    }

    /// Row of the process per-variant arrays for `extruder`.
    pub fn process_row(&self, extruder: usize) -> usize {
        self.process.variant_index_or_zero(VariantTable::Process, extruder, self.extruder_variant(extruder))
    }

    /// Logical extruder of filament `f`.
    pub fn filament_extruder(&self, f: usize) -> usize {
        self.filaments[f].extruder
    }

    /// Row of filament `f`'s per-variant arrays (variant of the extruder it prints with).
    pub fn filament_row(&self, f: usize) -> usize {
        let e = self.filament_extruder(f);
        self.filaments[f].config.variant_index_or_zero(VariantTable::Filament, e, self.extruder_variant(e))
    }

    fn fcfg(&self, f: usize) -> &'a Config {
        self.filaments[f].config
    }

    fn validate(&self) -> Result<(), ProfileError> {
        if self.filaments.is_empty() {
            return Err(ProfileError::InvalidJob("no filaments".into()));
        }
        let n = self.extruder_count();
        if let Some((i, s)) = self.filaments.iter().enumerate().find(|(_, s)| s.extruder >= n) {
            return Err(ProfileError::InvalidJob(format!(
                "filament {i} is assigned to extruder {} but the machine has {n}",
                s.extruder
            )));
        }
        Ok(())
    }

    /// Effective value of a machine per-variant retraction key (`retraction_length`, `z_hop`,
    /// `wipe`, `retraction_distances_when_cut`, ...) for filament `f`: the machine value of the
    /// filament's extruder variant, overridden by the filament's `filament_<key>` unless that is
    /// `nil` (Bambu `compute_filament_override_value`). `long_retractions_when_cut` and
    /// `retraction_distances_when_cut` are only overridden when the machine's
    /// `enable_long_retraction_when_cut` is 2.
    pub fn effective_machine_value(&self, f: usize, key: &str) -> Option<String> {
        let machine_val = self.machine.get_str_at(key, self.machine_row(self.filament_extruder(f)));
        let allow = !matches!(key, "long_retractions_when_cut" | "retraction_distances_when_cut")
            || self.machine.get_i64("enable_long_retraction_when_cut") == Some(2);
        let fkey = match key {
            "long_retractions_when_cut" => "filament_long_retractions_when_cut".to_string(),
            "retraction_distances_when_cut" => "filament_retraction_distances_when_cut".to_string(),
            k => format!("filament_{k}"),
        };
        let fil = self.fcfg(f).get_str_at(&fkey, self.filament_row(f)).filter(|s| s.trim() != NIL);
        match (allow, fil) {
            (true, Some(v)) => Some(v.to_string()),
            _ => machine_val.map(str::to_string),
        }
    }

    fn eff_f64(&self, f: usize, key: &str) -> f64 {
        self.effective_machine_value(f, key)
            .and_then(|s| crate::config::parse_percent_or_abs(&s, 100.0))
            .unwrap_or(0.0)
    }

    fn eff_bool(&self, f: usize, key: &str) -> bool {
        self.effective_machine_value(f, key).and_then(|s| crate::config::parse_bool(&s)).unwrap_or(false)
    }

    fn f_str(&self, f: usize, key: &str) -> String {
        self.fcfg(f).get_str(key).unwrap_or("").to_string()
    }
    fn f_f64(&self, f: usize, key: &str) -> f64 {
        self.fcfg(f).get_f64(key).unwrap_or(0.0)
    }
    fn f_f64_row(&self, f: usize, key: &str) -> f64 {
        self.fcfg(f).get_f64_at(key, self.filament_row(f)).unwrap_or(0.0)
    }
    fn f_i64(&self, f: usize, key: &str) -> i64 {
        self.fcfg(f).get_i64(key).unwrap_or(0)
    }
    fn f_i64_row(&self, f: usize, key: &str) -> i64 {
        self.fcfg(f).get_i64_at(key, self.filament_row(f)).unwrap_or(0)
    }
    fn f_bool(&self, f: usize, key: &str) -> bool {
        self.fcfg(f).get_bool(key).unwrap_or(false)
    }
    fn f_bool_row(&self, f: usize, key: &str) -> bool {
        self.fcfg(f).get_bool_at(key, self.filament_row(f)).unwrap_or(false)
    }

    fn per_filament<T>(&self, g: impl Fn(usize) -> T) -> Vec<T> {
        (0..self.filaments.len()).map(g).collect()
    }

    fn per_extruder<T>(&self, g: impl Fn(usize) -> T) -> Vec<T> {
        (0..self.extruder_count()).map(g).collect()
    }

    fn nozzle_diameters(&self) -> Vec<f64> {
        self.per_extruder(|e| self.machine.get_f64_at("nozzle_diameter", e).unwrap_or(0.4))
    }

    /// `nozzle_volume_types`: one entry per nozzle up to the highest nozzle used by `used`.
    fn nozzle_volume_types(&self, used: &[usize]) -> Vec<String> {
        let max = used.iter().map(|&f| self.filament_extruder(f)).max().unwrap_or(0);
        (0..=max).map(|e| volume_type_of_variant(self.extruder_variant(e)).to_string()).collect()
    }

    /// `flush_volumetric_speeds` / `flush_temperatures` (Bambu falls back to
    /// `filament_max_volumetric_speed` / `nozzle_temperature_range_high` when 0).
    fn flush_params(&self, fast: bool) -> (Vec<f64>, Vec<i64>) {
        let speeds = self.per_filament(|f| {
            let v = self.f_f64_row(f, "filament_flush_volumetric_speed");
            if v == 0.0 {
                self.f_f64_row(f, "filament_max_volumetric_speed")
            } else {
                v
            }
        });
        let temps = self.per_filament(|f| {
            let t = self.f_i64_row(f, if fast { "filament_flush_temp_fast" } else { "filament_flush_temp" });
            if t == 0 {
                self.f_i64(f, "nozzle_temperature_range_high")
            } else {
                t
            }
        });
        (speeds, temps)
    }

    /// Bambu `get_outer_wall_volumetric_speed` for filament `f`.
    pub fn outer_wall_volumetric_speed(&self, f: usize) -> f64 {
        let e = self.filament_extruder(f);
        let nozzle = self.machine.get_f64_at("nozzle_diameter", e).unwrap_or(0.4);
        let mut width = self.process.get_percent_or_abs("outer_wall_line_width", nozzle).unwrap_or(0.0);
        if width == 0.0 {
            width = self.process.get_percent_or_abs("line_width", nozzle).unwrap_or(0.0);
            if width == 0.0 {
                width = self.f_f64(f, "filament_diameter");
            }
        }
        let h = self.process.get_f64("layer_height").unwrap_or(0.2);
        // Flow::mm3_per_mm for a non-bridge extrusion (rectangle with semicircular ends).
        let mm3_per_mm = h * (width - h * (1.0 - std::f64::consts::FRAC_PI_4));
        let speed = self.process.get_f64_at("outer_wall_speed", self.process_row(e)).unwrap_or(0.0);
        (speed * mm3_per_mm).min(self.f_f64_row(f, "filament_max_volumetric_speed"))
    }

    fn nozzle_temp(&self, f: usize, first_layer: bool) -> i64 {
        self.f_i64_row(f, if first_layer { "nozzle_temperature_initial_layer" } else { "nozzle_temperature" })
    }

    /// Variables for `layer_change_gcode` (Bambu sets them after the layer index was advanced).
    pub fn layer_change_vars(&self, ev: &LayerEvent) -> Context {
        let mut c = Context::new();
        let e = self.filament_extruder(ev.current_filament);
        c.set("most_used_physical_extruder_id", self.physical_extruder(ev.most_used_extruder) as i64)
            .set("layer_num", ev.layer_num)
            .set("layer_z", ev.layer_z)
            .set("max_layer_z", ev.max_layer_z)
            .set("curr_y_acceleration_limit", ev.curr_y_acceleration_limit)
            .set("curr_accumulated_mass", ev.curr_accumulated_mass)
            .set("curr_layer_mass", ev.curr_layer_mass)
            .set("current_filament_id", ev.current_filament as i64)
            .set("current_extruder_id", e as i64)
            .set("current_nozzle_id", e as i64)
            .set_current_filament_id(ev.current_filament);
        c
    }

    /// Variables for `before_layer_change_gcode` (`layer_num` is the *next* layer index).
    pub fn before_layer_change_vars(&self, ev: &LayerEvent) -> Context {
        let mut c = Context::new();
        c.set("layer_num", ev.layer_num + 1)
            .set("layer_z", ev.layer_z)
            .set("max_layer_z", ev.max_layer_z)
            .set_current_filament_id(ev.current_filament);
        c
    }

    /// Variables for `wrapping_detection_gcode`.
    pub fn wrapping_detection_vars(&self, ev: &LayerEvent) -> Context {
        let mut c = Context::new();
        c.set("layer_num", ev.layer_num)
            .set("layer_z", ev.layer_z)
            .set("max_layer_z", ev.max_layer_z)
            .set("most_used_physical_extruder_id", self.physical_extruder(ev.most_used_extruder) as i64)
            .set(
                "curr_physical_extruder_id",
                self.physical_extruder(self.filament_extruder(ev.current_filament)) as i64,
            )
            .set_current_filament_id(ev.current_filament);
        c
    }

    /// Variables for `time_lapse_gcode`.
    pub fn timelapse_vars(&self, ev: &LayerEvent, tl: &TimelapseEvent) -> Context {
        let mut c = self.wrapping_detection_vars(ev);
        let pos = if tl.inline_photo { None } else { tl.safe_pos };
        let [x, y] = pos.unwrap_or([0, 0]);
        c.set("timelapse_pos_x", x)
            .set("timelapse_pos_y", y)
            .set("has_timelapse_safe_pos", pos.is_some_and(|p| p != [0, 0]))
            .set("timelapse_inline_photo", tl.inline_photo)
            .set("farthest_point_timelapse_enabled", tl.farthest_point_enabled)
            .set("clear_to_x0", tl.clear_to_x0);
        c
    }

    /// Variables for `machine_end_gcode` (and `filament_end_gcode`). `ev.layer_z` should be the
    /// current nozzle Z.
    pub fn end_vars(&self, ev: &LayerEvent, used_filaments: &[usize]) -> Context {
        let mut c = Context::new();
        let f = ev.current_filament;
        let e = self.filament_extruder(f);
        c.set("layer_num", ev.layer_num)
            .set("layer_z", ev.layer_z)
            .set("max_layer_z", ev.max_layer_z)
            .set("nozzle_diameter_at_nozzle_id", self.nozzle_diameters())
            .set("nozzle_volume_types", self.nozzle_volume_types(used_filaments))
            .set("filament_extruder_id", f as i64)
            .set("current_filament_id", f as i64)
            .set("current_extruder_id", e as i64)
            .set("current_nozzle_id", e as i64)
            .set_current_filament_id(f);
        c
    }

    /// Variables for `change_filament_gcode` on the wipe-tower path (`WipeTowerIntegration::append_tcr`).
    pub fn toolchange_vars(&self, tc: &ToolChange) -> Result<Context, ProfileError> {
        let nf = self.filaments.len();
        if tc.new_filament >= nf || tc.old_filament.is_some_and(|o| o >= nf) {
            return Err(ProfileError::InvalidJob("tool change references an unknown filament".into()));
        }
        let new = tc.new_filament;
        let new_e = self.filament_extruder(new);
        let mut c = Context::new();
        let old_i = tc.old_filament.map_or(-1, |o| o as i64);
        let old_e = tc.old_filament.map_or(-1, |o| self.filament_extruder(o) as i64);

        let old_retract = tc.old_filament.map_or(0.0, |o| self.eff_f64(o, "retraction_length"));
        let new_retract = self.eff_f64(new, "retraction_length");
        let old_retract_tc = tc.old_filament.map_or(0.0, |o| self.eff_f64(o, "retract_length_toolchange"));
        let new_retract_tc = self.eff_f64(new, "retract_length_toolchange");
        // Bambu reads filament_retract_length_nc at the *old* filament's config row; 0 without one.
        let old_nc = tc.old_filament.map_or(0.0, |o| self.f_f64_row(o, "filament_retract_length_nc"));
        let old_temp = tc.old_filament.map_or(210, |o| self.nozzle_temp(o, tc.old_in_first_layer_phase));
        let new_temp = self.nozzle_temp(new, tc.new_in_first_layer_phase);

        let purge_volume = if tc.purge_volume < 1e-6 { 0.0 } else { tc.purge_volume.max(MIN_PURGE_VOLUME) };
        let new_area = filament_area(self.f_f64(new, "filament_diameter"));
        let purge_length = purge_volume / new_area;
        let old_area = tc.old_filament.map_or(new_area, |o| filament_area(self.f_f64(o, "filament_diameter")));
        let mut old_feed = tc
            .old_filament
            .map_or(200, |o| (60.0 * self.f_f64_row(o, "filament_max_volumetric_speed") / old_area) as i64);
        if old_feed == 0 {
            old_feed = 100;
        }
        let mut new_feed = (60.0 * self.f_f64_row(new, "filament_max_volumetric_speed") / new_area) as i64;
        if new_feed == 0 {
            new_feed = 100;
        }

        let fast = self.process.get_str("prime_volume_mode") == Some("Fast");
        let (flush_speeds, flush_temps) = self.flush_params(fast);
        let zero_cooling = tc.is_prime_tower_interface || tc.layer_num == 0;
        let cooling = self.per_filament(|f| if zero_cooling { 0.0 } else { self.f_f64_row(f, "filament_cooling_before_tower") });

        let tower_features = self.process.get_bool("enable_tower_interface_features").unwrap_or(false);
        let (is_iface, iface_volume, iface_temp) = if tower_features {
            let mut t = self.fcfg(new).get_i64("filament_tower_interface_print_temp").unwrap_or(-1);
            if t == -1 {
                t = self.f_i64(new, "nozzle_temperature_range_high");
            }
            (tc.is_prime_tower_interface, self.f_f64(new, "filament_tower_interface_purge_volume"), t)
        } else {
            (false, 0.0, -1)
        };

        // The scalar `*_when_cut` / `*_when_ec` variables still hold the values of the filament
        // that is being unloaded while change_filament_gcode runs.
        let cur = tc.old_filament.unwrap_or(new);
        self.set_retraction_scalars(&mut c, cur);

        c.set("previous_extruder", old_i)
            .set("next_extruder", new as i64)
            .set("next_hotend", -1i64)
            .set("current_hotend", -1i64)
            .set("layer_num", tc.layer_num)
            .set("layer_z", tc.layer_z)
            .set("toolchange_z", tc.toolchange_z)
            .set("outer_wall_volumetric_speed", self.outer_wall_volumetric_speed(new))
            .set("current_extruder_id", old_e)
            .set("next_extruder_id", new_e as i64)
            .set("current_nozzle_id", old_e)
            .set("next_nozzle_id", new_e as i64)
            .set("current_filament_id", old_i)
            .set("next_filament_id", new as i64)
            .set("nozzle_diameter_at_nozzle_id", self.nozzle_diameters())
            .set("nozzle_volume_types", self.nozzle_volume_types(&tc.used_filaments))
            .set("max_layer_z", tc.max_layer_z)
            .set("relative_e_axis", self.machine.get_bool("use_relative_e_distances").unwrap_or(true))
            .set("toolchange_count", tc.toolchange_count)
            .set("fan_speed", 0i64)
            .set("old_retract_length", old_retract)
            .set("new_retract_length", new_retract)
            .set("filament_retract_length_nc", old_nc)
            .set("old_retract_length_toolchange", old_retract_tc)
            .set("new_retract_length_toolchange", new_retract_tc)
            .set("new_extruder_retracted_length", tc.new_extruder_retracted_length)
            .set("old_filament_temp", old_temp)
            .set("new_filament_temp", new_temp)
            .set("x_after_toolchange", tc.position_after[0])
            .set("y_after_toolchange", tc.position_after[1])
            .set("z_after_toolchange", tc.position_after[2])
            .set("first_flush_volume", purge_length / 2.0)
            .set("second_flush_volume", purge_length / 2.0)
            .set("old_filament_e_feedrate", old_feed)
            .set("new_filament_e_feedrate", new_feed)
            .set(
                "old_extruder_variant",
                if old_e >= 0 { self.extruder_variant(old_e as usize).to_string() } else { String::new() },
            )
            .set("new_extruder_variant", self.extruder_variant(new_e).to_string())
            .set("flush_volumetric_speeds", flush_speeds)
            .set("flush_temperatures", flush_temps)
            .set("filament_cooling_before_tower", cooling)
            .set("flush_length", purge_length)
            .set("wipe_avoid_perimeter", tc.wipe_avoid_perimeter)
            .set("wipe_avoid_pos_x", tc.wipe_avoid_pos_x)
            .set("is_prime_tower_interface", is_iface)
            .set("filament_tower_interface_purge_volume", iface_volume)
            .set("filament_tower_interface_print_temp", iface_temp)
            .set("wipe_tower_center_pos_x", tc.wipe_tower_stop_pos[0])
            .set("wipe_tower_center_pos_y", tc.wipe_tower_stop_pos[1])
            .set("wipe_tower_center_pos_valid", true);
        for (i, p) in tc.travel_points.iter().enumerate() {
            c.set(format!("travel_point_{}_x", i + 1), p[0]);
            c.set(format!("travel_point_{}_y", i + 1), p[1]);
        }
        let mut count = (purge_volume / PURGE_VOLUME_ONE_TIME).round() as usize;
        count = count.min(MAX_FLUSH_COUNT);
        if count == 0 && purge_volume > 0.0 {
            count = 1;
        }
        for i in 0..MAX_FLUSH_COUNT {
            let v = if i < count { purge_length / count as f64 } else { 0.0 };
            c.set(format!("flush_length_{}", i + 1), v);
        }
        c.set_current_filament_id(new);
        Ok(c)
    }

    fn set_retraction_scalars(&self, c: &mut Context, f: usize) {
        c.set("retraction_distance_when_cut", self.eff_f64(f, "retraction_distances_when_cut"))
            .set("long_retraction_when_cut", self.eff_bool(f, "long_retractions_when_cut"))
            .set("retraction_distance_when_ec", self.f_f64_row(f, "retraction_distances_when_ec"))
            .set("long_retraction_when_ec", self.f_bool_row(f, "long_retractions_when_ec"));
    }
}

/// Facts about the sliced job needed by the start G-code.
#[derive(Clone, Debug)]
pub struct JobInfo {
    pub bed_type: BedType,
    /// Filament ids in print order (all layers' tool ordering flattened; repeats allowed).
    /// The first entry is the first filament printed.
    pub filament_sequence: Vec<usize>,
    /// Filaments extruded on the first layer (objects, supports, brim, prime tower).
    pub first_layer_filaments: Vec<usize>,
    /// First-layer bounding box (objects + support + brim + skirt + prime tower), bed mm.
    pub first_layer_min: [f64; 2],
    pub first_layer_max: [f64; 2],
    /// Center of the first-layer bbox of objects only (no prime tower); default: bbox center.
    pub first_layer_center_no_wipe_tower: Option<[f64; 2]>,
    /// Highest `print_z` of any object, mm (the template gets `ceil` of it).
    pub max_print_z: f64,
    pub total_layer_count: usize,
    pub has_wipe_tower: bool,
    pub total_toolchanges: usize,
    /// Where the nozzle waits beside the prime tower (`wipe_tower_center_pos_*`).
    pub wipe_tower_stop_pos: Option<[f64; 2]>,
    /// Area (mm²) of the first layer incl. support, brim and prime tower (for
    /// `hold_chamber_temp_for_flat_print`).
    pub first_layer_area: f64,
    /// Override for `in_head_wrap_detect_zone`; default: first-layer bbox overlaps the machine's
    /// `head_wrap_detect_zone` bbox.
    pub in_head_wrap_detect_zone: Option<bool>,
    /// `timelapse_type`: 0 = traditional, 1 = smooth.
    pub timelapse_type: i64,
    /// Override the process `spiral_mode`.
    pub spiral_mode: Option<bool>,
    /// Override the machine `cooling_filter_enabled`.
    pub cooling_filter_enabled: Option<bool>,
    pub plate_name: String,
}

impl JobInfo {
    /// A job with the given essentials; everything else defaults (no prime tower, traditional
    /// timelapse, the first filament of the sequence is the only first-layer filament).
    pub fn new(
        bed_type: BedType,
        filament_sequence: Vec<usize>,
        first_layer_min: [f64; 2],
        first_layer_max: [f64; 2],
        max_print_z: f64,
        total_layer_count: usize,
    ) -> Self {
        let first_layer_filaments = filament_sequence.first().copied().into_iter().collect();
        JobInfo {
            bed_type,
            filament_sequence,
            first_layer_filaments,
            first_layer_min,
            first_layer_max,
            first_layer_center_no_wipe_tower: None,
            max_print_z,
            total_layer_count,
            has_wipe_tower: false,
            total_toolchanges: 0,
            wipe_tower_stop_pos: None,
            first_layer_area: 0.0,
            in_head_wrap_detect_zone: None,
            timelapse_type: 0,
            spiral_mode: None,
            cooling_filter_enabled: None,
            plate_name: String::new(),
        }
    }
}

/// Per-layer facts for layer-change / timelapse / wrapping-detection / end templates.
#[derive(Clone, Debug, Default)]
pub struct LayerEvent {
    /// 0-based layer index (Bambu `m_layer_index`).
    pub layer_num: i64,
    /// Z of the layer being started (`print_z`), mm.
    pub layer_z: f64,
    /// Highest Z printed so far, mm.
    pub max_layer_z: f64,
    /// Filament currently loaded.
    pub current_filament: usize,
    /// Logical extruder printing most of this layer.
    pub most_used_extruder: usize,
    pub curr_y_acceleration_limit: f64,
    pub curr_accumulated_mass: f64,
    pub curr_layer_mass: f64,
}

/// Timelapse facts (Bambu `TimelapsePosPicker`).
#[derive(Clone, Debug, Default)]
pub struct TimelapseEvent {
    /// Safe camera position in whole mm (`None` = no safe position found).
    pub safe_pos: Option<[i64; 2]>,
    /// Take the photo in place (no safe-position move).
    pub inline_photo: bool,
    pub farthest_point_enabled: bool,
    pub clear_to_x0: bool,
}

/// One tool change on the prime-tower path.
#[derive(Clone, Debug, Default)]
pub struct ToolChange {
    /// Filament being unloaded (`None` before the first extrusion).
    pub old_filament: Option<usize>,
    pub new_filament: usize,
    /// 0-based layer index.
    pub layer_num: i64,
    /// `print_z` of the tool-change layer.
    pub layer_z: f64,
    pub max_layer_z: f64,
    pub toolchange_z: f64,
    /// 1-based count of this tool change.
    pub toolchange_count: i64,
    /// Purge volume from the flush matrix, mm³ (Bambu raises it to at least 100 mm³).
    pub purge_volume: f64,
    /// Bambu `filament_in_first_layer_phase` for the old / new filament (selects the
    /// initial-layer nozzle temperature).
    pub old_in_first_layer_phase: bool,
    pub new_in_first_layer_phase: bool,
    /// Length the new filament is currently retracted by (`get_extruder_retracted_length`).
    pub new_extruder_retracted_length: f64,
    /// `x/y_after_toolchange` = prime-tower start position, `z_after_toolchange` = nozzle Z.
    pub position_after: [f64; 3],
    pub travel_points: [[f64; 2]; 3],
    pub wipe_avoid_perimeter: bool,
    pub wipe_avoid_pos_x: f64,
    /// The tool change prints a prime-tower interface (`tcr.is_contact`).
    pub is_prime_tower_interface: bool,
    pub wipe_tower_stop_pos: [f64; 2],
    /// Filaments used by the print (sizes `nozzle_volume_types`).
    pub used_filaments: Vec<usize>,
}

fn dedup(seq: &[usize]) -> Vec<usize> {
    let mut out = Vec::new();
    for &f in seq {
        if !out.contains(&f) {
            out.push(f);
        }
    }
    out
}

/// Print-wide template variables (Bambu's placeholder-parser state when it renders
/// `machine_start_gcode`). The returned context's current filament is the initial filament.
pub fn build_print_context(setup: &PrintSetup<'_>, job: &JobInfo) -> Result<Context, ProfileError> {
    setup.validate()?;
    let nf = setup.filaments.len();
    let ne = setup.extruder_count();
    if job.filament_sequence.is_empty() {
        return Err(ProfileError::InvalidJob("empty filament sequence".into()));
    }
    if let Some(f) = job.filament_sequence.iter().chain(&job.first_layer_filaments).find(|&&f| f >= nf) {
        return Err(ProfileError::InvalidJob(format!("filament {f} does not exist ({nf} filaments)")));
    }
    let m = setup.machine;
    let p = setup.process;
    let mut c = Context::new();

    // ---------------------------------------------------------------- per-filament settings
    c.set("filament_type", setup.per_filament(|f| setup.f_str(f, "filament_type")))
        .set("filament_vendor", setup.per_filament(|f| setup.f_str(f, "filament_vendor")))
        .set("filament_is_support", setup.per_filament(|f| setup.f_bool(f, "filament_is_support")))
        .set("filament_soluble", setup.per_filament(|f| setup.f_bool(f, "filament_soluble")))
        .set("filament_diameter", setup.per_filament(|f| setup.f_f64(f, "filament_diameter")))
        .set("filament_density", setup.per_filament(|f| setup.f_f64(f, "filament_density")))
        .set("filament_cost", setup.per_filament(|f| setup.f_f64(f, "filament_cost")))
        .set("filament_map", setup.per_filament(|f| setup.filament_extruder(f) as i64 + 1))
        .set("physical_extruder_map", setup.per_extruder(|e| setup.physical_extruder(e) as i64));
    let nozzle_temp = setup.per_filament(|f| setup.nozzle_temp(f, false));
    let nozzle_temp_first = setup.per_filament(|f| setup.nozzle_temp(f, true));
    c.set("nozzle_temperature", nozzle_temp)
        .set("nozzle_temperature_initial_layer", nozzle_temp_first.clone())
        .set("first_layer_temperature", nozzle_temp_first)
        .set("nozzle_temperature_range_low", setup.per_filament(|f| setup.f_i64(f, "nozzle_temperature_range_low")))
        .set("nozzle_temperature_range_high", setup.per_filament(|f| setup.f_i64(f, "nozzle_temperature_range_high")));
    for key in ["filament_max_volumetric_speed", "filament_flow_ratio", "filament_cooling_before_tower"] {
        c.set(key, setup.per_filament(|f| setup.f_f64_row(f, key)));
    }
    for key in ["filament_pre_cooling_temperature", "filament_pre_cooling_temperature_nc"] {
        c.set(key, setup.per_filament(|f| setup.f_i64_row(f, key)));
    }
    let fast = p.get_str("prime_volume_mode") == Some("Fast");
    let (flush_speeds, flush_temps) = setup.flush_params(fast);
    c.set("flush_volumetric_speeds", flush_speeds).set("flush_temperatures", flush_temps);
    for key in [
        "chamber_temperatures",
        "temperature_vitrification",
        "additional_cooling_fan_speed",
        "during_print_exhaust_fan_speed",
        "complete_print_exhaust_fan_speed",
        "fan_min_speed",
        "fan_max_speed",
        "close_fan_the_first_x_layers",
    ] {
        c.set(key, setup.per_filament(|f| setup.f_i64(f, key)));
    }
    c.set("activate_air_filtration", setup.per_filament(|f| setup.f_bool(f, "activate_air_filtration")));
    c.set(
        "during_print_exhaust_fan_speed_num",
        setup.per_filament(|f| (setup.f_f64(f, "during_print_exhaust_fan_speed") / 100.0 * 255.0) as i64),
    );
    for bed in BedType::ALL {
        for key in [bed.temp_key(), bed.initial_layer_temp_key()] {
            c.set(key, setup.per_filament(|f| setup.f_i64(f, key)));
        }
    }
    // Machine per-variant retraction settings, merged with filament overrides, per filament.
    for key in [
        "retraction_length",
        "retraction_speed",
        "deretraction_speed",
        "retraction_minimum_travel",
        "retract_restart_extra",
        "retract_length_toolchange",
        "retract_restart_extra_toolchange",
        "z_hop",
        "retract_lift_above",
        "retract_lift_below",
        "wipe_distance",
        "retract_before_wipe",
        "retraction_distances_when_cut",
    ] {
        c.set(key, setup.per_filament(|f| setup.eff_f64(f, key)));
    }
    for key in ["wipe", "retract_when_changing_layer", "long_retractions_when_cut"] {
        c.set(key, setup.per_filament(|f| setup.eff_bool(f, key)));
    }
    c.set(
        "z_hop_types",
        setup.per_filament(|f| setup.effective_machine_value(f, "z_hop_types").unwrap_or_default()),
    );
    c.set("retraction_distances_when_ec", setup.per_filament(|f| setup.f_f64_row(f, "retraction_distances_when_ec")))
        .set("long_retractions_when_ec", setup.per_filament(|f| setup.f_bool_row(f, "long_retractions_when_ec")));

    // ---------------------------------------------------------------- per-extruder settings
    let nozzle_diameters = setup.nozzle_diameters();
    c.set("nozzle_diameter", nozzle_diameters.clone());
    for key in ["hotend_cooling_rate", "hotend_heating_rate", "nozzle_volume", "hotend_cooling_rate_on_rack"] {
        c.set(key, setup.per_extruder(|e| m.get_f64_at(key, setup.machine_row(e)).unwrap_or(0.0)));
    }
    for key in [
        "initial_layer_acceleration",
        "default_acceleration",
        "travel_acceleration",
        "outer_wall_acceleration",
        "inner_wall_acceleration",
        "top_surface_acceleration",
        "initial_layer_travel_acceleration",
        "travel_short_distance_acceleration",
        "initial_layer_speed",
        "initial_layer_infill_speed",
        "outer_wall_speed",
        "inner_wall_speed",
        "sparse_infill_speed",
        "internal_solid_infill_speed",
        "top_surface_speed",
        "gap_infill_speed",
        "bridge_speed",
        "support_speed",
        "support_interface_speed",
        "travel_speed",
        "travel_speed_z",
    ] {
        c.set(key, setup.per_extruder(|e| p.get_f64_at(key, setup.process_row(e)).unwrap_or(0.0)));
    }

    // ---------------------------------------------------------------- scalars
    let pf = |k: &str, d: f64| p.get_f64(k).unwrap_or(d);
    c.set("layer_height", pf("layer_height", 0.2))
        .set("initial_layer_print_height", pf("initial_layer_print_height", 0.2))
        .set("spiral_mode", job.spiral_mode.unwrap_or(p.get_bool("spiral_mode").unwrap_or(false)))
        .set("enable_prime_tower", p.get_bool("enable_prime_tower").unwrap_or(false))
        .set("print_sequence", p.get_str("print_sequence").unwrap_or("by layer"))
        // An enum: Bambu serializes it to "0"/"1", so templates see a string.
        .set("timelapse_type", job.timelapse_type.to_string())
        .set("curr_bed_type", job.bed_type.name())
        .set(
            "cooling_filter_enabled",
            job.cooling_filter_enabled.unwrap_or(m.get_bool("cooling_filter_enabled").unwrap_or(false)),
        )
        .set("printable_height", m.get_f64("printable_height").unwrap_or(0.0))
        .set("printer_model", m.get_str("printer_model").unwrap_or(""))
        .set("scan_first_layer", m.get_bool("scan_first_layer").unwrap_or(false))
        .set("support_chamber_temp_control", m.get_bool("support_chamber_temp_control").unwrap_or(false))
        .set("support_air_filtration", m.get_bool("support_air_filtration").unwrap_or(false))
        .set("bed_temperature_formula", m.get_str("bed_temperature_formula").unwrap_or("by_first_filament"));

    // ---------------------------------------------------------------- initial filaments
    let seq = &job.filament_sequence;
    let initial = seq[0];
    let used = dedup(seq);
    let is_support = |f: usize| setup.f_bool(f, "filament_is_support");
    let has_non_support = used.iter().any(|&f| !is_support(f));
    let mut first_filaments = vec![-1i64; ne];
    let mut first_non_support = vec![-1i64; ne];
    let mut initial_ns: Option<usize> = None;
    for &f in seq {
        let e = setup.filament_extruder(f);
        if first_filaments[e] == -1 {
            first_filaments[e] = f as i64;
        }
        if has_non_support && !is_support(f) {
            initial_ns.get_or_insert(f);
            if first_non_support[e] == -1 {
                first_non_support[e] = f as i64;
            }
        }
    }
    let initial_ns = initial_ns.unwrap_or(initial);
    // Re-index by physical (firmware) extruder.
    let to_physical = |v: &[i64]| {
        let mut out = vec![-1i64; ne];
        for (e, &f) in v.iter().enumerate() {
            let pe = setup.physical_extruder(e);
            if pe < ne {
                out[pe] = f;
            }
        }
        out
    };
    let first_filaments = to_physical(&first_filaments);
    let first_non_support = to_physical(&first_non_support);
    let init_e = setup.filament_extruder(initial) as i64;
    let init_ns_e = setup.filament_extruder(initial_ns) as i64;
    c.set("first_tools", first_filaments.clone())
        .set("first_filaments", first_filaments)
        .set("initial_tool", initial as i64)
        .set("initial_extruder", initial as i64)
        .set("first_non_support_tools", first_non_support.clone())
        .set("first_non_support_hotend", vec![-1i64; ne])
        .set("first_non_support_filaments", first_non_support)
        .set("initial_no_support_tool", initial_ns as i64)
        .set("initial_no_support_extruder", initial_ns as i64)
        .set("initial_no_support_hotend", -1i64)
        .set("current_extruder", initial as i64)
        .set("current_hotend", -1i64)
        .set("initial_filament_id", initial as i64)
        .set("initial_extruder_id", init_e)
        .set("initial_nozzle_id", init_e)
        .set("initial_no_support_filament_id", initial_ns as i64)
        .set("initial_no_support_extruder_id", init_ns_e)
        .set("initial_no_support_nozzle_id", init_ns_e)
        .set("nozzle_diameter_at_nozzle_id", nozzle_diameters)
        .set("nozzle_volume_types", setup.nozzle_volume_types(&used));
    setup.set_retraction_scalars(&mut c, initial);

    let max_fan = used.iter().map(|&f| setup.f_f64(f, "additional_cooling_fan_speed")).fold(0.0, f64::max);
    c.set("max_additional_fan", max_fan)
        .set("total_layer_count", job.total_layer_count as i64)
        .set("current_object_idx", 0i64)
        .set("has_wipe_tower", job.has_wipe_tower)
        .set("has_wipe_tower_this_layer", job.has_wipe_tower)
        .set("total_toolchanges", job.total_toolchanges as i64);

    // ---------------------------------------------------------------- geometry
    let bed = m.get_points("printable_area").unwrap_or_default();
    let (mut bmin, mut bmax) = ([0.0f64; 2], [0.0f64; 2]);
    if let Some(first) = bed.first() {
        bmin = *first;
        bmax = *first;
        for q in &bed {
            for a in 0..2 {
                bmin[a] = bmin[a].min(q[a]);
                bmax[a] = bmax[a].max(q[a]);
            }
        }
    }
    let fl_min = job.first_layer_min;
    let fl_max = job.first_layer_max;
    let fl_size = [fl_max[0] - fl_min[0], fl_max[1] - fl_min[1]];
    let center = job
        .first_layer_center_no_wipe_tower
        .unwrap_or([(fl_min[0] + fl_max[0]) / 2.0, (fl_min[1] + fl_max[1]) / 2.0]);
    let in_wrap_zone = job.in_head_wrap_detect_zone.unwrap_or_else(|| {
        let zone = m.get_points("head_wrap_detect_zone").unwrap_or_default();
        if zone.is_empty() {
            return false;
        }
        let zx = zone.iter().map(|q| q[0]);
        let zy = zone.iter().map(|q| q[1]);
        let (zx0, zx1) = (zx.clone().fold(f64::MAX, f64::min), zx.fold(f64::MIN, f64::max));
        let (zy0, zy1) = (zy.clone().fold(f64::MAX, f64::min), zy.fold(f64::MIN, f64::max));
        fl_min[0] <= zx1 && fl_max[0] >= zx0 && fl_min[1] <= zy1 && fl_max[1] >= zy0
    });
    c.set("print_bed_min", vec![bmin[0], bmin[1]])
        .set("print_bed_max", vec![bmax[0], bmax[1]])
        .set("print_bed_size", vec![bmax[0] - bmin[0], bmax[1] - bmin[1]])
        .set("first_layer_print_min", vec![fl_min[0], fl_min[1]])
        .set("first_layer_print_max", vec![fl_max[0], fl_max[1]])
        .set("first_layer_print_size", vec![fl_size[0], fl_size[1]])
        .set("first_layer_center_no_wipe_tower", vec![center[0], center[1]])
        .set("in_head_wrap_detect_zone", in_wrap_zone)
        .set("max_print_z", job.max_print_z.ceil() as i64);

    // ---------------------------------------------------------------- temperatures
    let bed_first: Vec<i64> = setup.per_filament(|f| setup.f_i64(f, job.bed_type.initial_layer_temp_key()));
    let bed_other: Vec<i64> = setup.per_filament(|f| setup.f_i64(f, job.bed_type.temp_key()));
    let target_bed = if m.get_str("bed_temperature_formula") == Some("by_highest_temp") {
        job.first_layer_filaments.iter().map(|&f| bed_first[f]).max().unwrap_or(0).max(0)
    } else {
        bed_first[initial]
    };
    let chamber = used.iter().map(|&f| setup.f_i64(f, "chamber_temperatures")).max().unwrap_or(0).max(0);
    let vitrification = used.iter().map(|&f| setup.f_i64(f, "temperature_vitrification")).min().unwrap_or(i32::MAX as i64);
    let all_bbl = used.iter().all(|&f| setup.f_str(f, "filament_vendor") == "Bambu Lab");
    let tpu_first = job.first_layer_filaments.iter().any(|&f| setup.f_str(f, "filament_type") == "TPU");
    c.set("bbl_bed_temperature_gcode", false)
        .set("bed_temperature_initial_layer", bed_first.clone())
        .set("bed_temperature", bed_other)
        .set("bed_temperature_initial_layer_single", target_bed)
        .set("bed_temperature_initial_layer_vector", "")
        .set("first_layer_bed_temperature", bed_first)
        .set("chamber_temperature", vec![chamber])
        .set("overall_chamber_temperature", chamber)
        .set("enable_high_low_temp_mix", true)
        .set("min_vitrification_temperature", vitrification)
        .set("max_print_height", m.get_f64("printable_height").unwrap_or(0.0) as i64)
        .set("z_offset", 0.0)
        .set("plate_name", job.plate_name.clone())
        .set("is_all_bbl_filament", all_bbl)
        .set("has_tpu_in_first_layer", tpu_first);
    let stop = job.wipe_tower_stop_pos;
    c.set("wipe_tower_center_pos_x", stop.map_or(0.0, |s| s[0]))
        .set("wipe_tower_center_pos_y", stop.map_or(0.0, |s| s[1]))
        .set("wipe_tower_center_pos_valid", job.has_wipe_tower && stop.is_some())
        .set("outer_wall_volumetric_speed", setup.outer_wall_volumetric_speed(initial_ns));

    // hold_chamber_temp_for_flat_print
    let first_h = p.get_f64("initial_layer_print_height").unwrap_or(0.2);
    let bed_w = bmax[0] - bmin[0];
    let bed_d = bmax[1] - bmin[1];
    let threshold = if bed_w > 260.0 && bed_d > 260.0 {
        let l = bed_w.max(bed_d) - 70.0;
        l * l
    } else {
        40000.0
    };
    let flat = job.max_print_z > 0.0 && job.max_print_z < first_h + 1e-4 && job.first_layer_area > threshold;
    c.set("hold_chamber_temp_for_flat_print", flat).set("first_layer_area_sum", job.first_layer_area);

    c.set("filament_ids", setup.per_filament(|f| setup.f_str(f, "filament_id")));
    // every other setting too, as Bambu's PlaceholderParser exposes the whole config: other
    // printers' templates read more than the H2D's do
    add_remaining(&mut c, setup);
    add_orca_defaults(&mut c);

    c.set_current_filament_id(initial);
    Ok(c)
}

/// The machine, process and filament settings the context doesn't have yet, typed as Bambu
/// types them: machine and process values as they are, filament values one per filament (each
/// at its own variant row). Keys already set keep their computed values.
/// OrcaSlicer's option type (`coBools`, ...) of a key, from data/orca_code_defaults.json.
pub(crate) fn orca_type(k: &str) -> Option<&'static str> {
    static TYPES: std::sync::OnceLock<std::collections::HashMap<String, String>> = std::sync::OnceLock::new();
    TYPES
        .get_or_init(|| {
            let json: serde_json::Value = serde_json::from_str(include_str!("../data/orca_code_defaults.json")).unwrap_or_default();
            json.as_object().map(|m| m.iter().filter_map(|(k, v)| Some((k.clone(), v.get("type")?.as_str()?.to_string()))).collect()).unwrap_or_default()
        })
        .get(k)
        .map(String::as_str)
}

/// Values of key `k` as a list typed by OrcaSlicer's option type, if it knows the key.
fn orca_typed(k: &str, vals: &[String]) -> Option<Value> {
    let num = |s: &String| parse_f64(s.trim_end_matches('%')).unwrap_or(0.0);
    Some(match orca_type(k)? {
        "coBool" | "coBools" => Value::Bools(vals.iter().map(|s| crate::config::parse_bool(s).unwrap_or(false)).collect()),
        "coInt" | "coInts" => Value::Ints(vals.iter().map(|s| num(s) as i64).collect()),
        "coFloat" | "coFloats" | "coPercent" | "coPercents" | "coFloatOrPercent" | "coFloatsOrPercents" => Value::Floats(vals.iter().map(num).collect()),
        "coString" | "coStrings" | "coPoint" | "coPoints" => Value::Strs(vals.to_vec()),
        _ => return None,
    })
}

/// OrcaSlicer's code defaults for the settings no preset gives (data/orca_code_defaults.json,
/// from tools/gen_orca_defaults.py): other makers' templates read some of them.
fn add_orca_defaults(c: &mut Context) {
    static JSON: &str = include_str!("../data/orca_code_defaults.json");
    let Ok(serde_json::Value::Object(all)) = serde_json::from_str::<serde_json::Value>(JSON) else { return };
    for (k, d) in &all {
        if c.contains(k) {
            continue;
        }
        let (Some(t), Some(v)) = (d.get("type").and_then(|t| t.as_str()), d.get("value")) else { continue };
        let strs: Vec<String> = match v {
            serde_json::Value::Array(a) => a.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect(),
            serde_json::Value::String(s) => vec![s.clone()],
            _ => continue,
        };
        let one = strs.first().cloned().unwrap_or_default();
        let num = |s: &str| parse_f64(s.trim_end_matches('%')).unwrap_or(0.0);
        let value = match t {
            "coBool" => Value::Bool(one == "1"),
            "coBools" => Value::Bools(strs.iter().map(|s| s == "1").collect()),
            "coInt" => Value::Int(num(&one) as i64),
            "coInts" => Value::Ints(strs.iter().map(|s| num(s) as i64).collect()),
            "coFloat" | "coPercent" | "coFloatOrPercent" => Value::Float(num(&one)),
            "coFloats" | "coPercents" | "coFloatsOrPercents" => Value::Floats(strs.iter().map(|s| num(s)).collect()),
            "coString" | "coPoint" => Value::Str(one),
            "coStrings" | "coPoints" => Value::Strs(strs),
            _ => continue,
        };
        c.set(k, value);
    }
}

/// A value's template type from how it looks: whole numbers are ints, other numbers floats,
/// everything else strings (lists likewise).
fn guess_type(v: &crate::config::ConfigValue) -> Value {
    fn list(items: &[String]) -> Value {
        if !items.is_empty() && items.iter().all(|s| s.trim().parse::<i64>().is_ok()) {
            Value::Ints(items.iter().map(|s| s.trim().parse().unwrap_or(0)).collect())
        } else if !items.is_empty() && items.iter().all(|s| parse_f64(s).is_some()) {
            Value::Floats(items.iter().map(|s| parse_f64(s).unwrap_or(0.0)).collect())
        } else {
            Value::Strs(items.to_vec())
        }
    }
    match v {
        crate::config::ConfigValue::Array(items) => list(items),
        crate::config::ConfigValue::Scalar(x) => match list(std::slice::from_ref(x)) {
            Value::Ints(i) => Value::Int(i[0]),
            Value::Floats(f) => Value::Float(f[0]),
            _ => Value::Str(x.clone()),
        },
    }
}

fn add_remaining(c: &mut Context, setup: &PrintSetup) {
    use crate::gcode_config::option_type;
    use crate::option_types::OptType;
    fn scalar(t: OptType, v: &str) -> Option<Value> {
        Some(match t {
            OptType::Bool => Value::Bool(crate::config::parse_bool(v).unwrap_or(false)),
            OptType::Int => Value::Int(parse_f64(v).unwrap_or(0.0) as i64),
            OptType::Float | OptType::Percent | OptType::FloatOrPercent => Value::Float(parse_f64(v.trim_end_matches('%')).unwrap_or(0.0)),
            OptType::String | OptType::Enum => Value::Str(v.to_string()),
            _ => return None,
        })
    }
    fn list(t: OptType, vs: &[String]) -> Option<Value> {
        Some(match t {
            OptType::Bools => Value::Bools(vs.iter().map(|v| crate::config::parse_bool(v).unwrap_or(false)).collect()),
            OptType::Ints => Value::Ints(vs.iter().map(|v| parse_f64(v).unwrap_or(0.0) as i64).collect()),
            OptType::Floats | OptType::Percents | OptType::FloatsOrPercents => Value::Floats(vs.iter().map(|v| if v == NIL { f64::NAN } else { parse_f64(v.trim_end_matches('%')).unwrap_or(0.0) }).collect()),
            OptType::Strings | OptType::Enums => Value::Strs(vs.to_vec()),
            _ => return None,
        })
    }
    // the filaments' own keys (OrcaSlicer's makers read `filament_notes`, pressure advance...),
    // one value per filament
    let mut keys: Vec<&str> = setup.filaments.iter().flat_map(|f| f.config.iter().map(|(k, _)| k)).collect();
    keys.sort_unstable();
    keys.dedup();
    for k in keys {
        if c.contains(k) {
            continue;
        }
        let vals: Vec<String> = setup.filaments.iter().map(|f| f.config.get(k).and_then(|v| v.first()).unwrap_or("").to_string()).collect();
        let v = match option_type(k) {
            Some(t) => list(t, &vals),
            None => orca_typed(k, &vals),
        };
        c.set(k, v.unwrap_or_else(|| guess_type(&crate::config::ConfigValue::Array(vals))));
    }
    for cfg in [setup.process, setup.machine] {
        for (k, v) in cfg.iter() {
            if c.contains(k) {
                continue;
            }
            let Some(t) = option_type(k) else {
                // a key Bambu Studio doesn't know: OrcaSlicer's type for it, else its look
                let typed = orca_typed(k, v.as_slice()).map(|x| match (x, v) {
                    (Value::Bools(b), crate::config::ConfigValue::Scalar(_)) if orca_type(k).is_some_and(|t| !t.ends_with('s')) => Value::Bool(b[0]),
                    (Value::Ints(i), crate::config::ConfigValue::Scalar(_)) if orca_type(k).is_some_and(|t| !t.ends_with('s')) => Value::Int(i[0]),
                    (Value::Floats(f), crate::config::ConfigValue::Scalar(_)) if orca_type(k).is_some_and(|t| !t.ends_with('s')) => Value::Float(f[0]),
                    (Value::Strs(s), crate::config::ConfigValue::Scalar(_)) if orca_type(k).is_some_and(|t| !t.ends_with('s')) => Value::Str(s[0].clone()),
                    (x, _) => x,
                });
                c.set(k, typed.unwrap_or_else(|| guess_type(v)));
                continue;
            };
            let value = match v {
                crate::config::ConfigValue::Scalar(x) => scalar(t, x).or_else(|| list(t, std::slice::from_ref(x))),
                crate::config::ConfigValue::Array(xs) => list(t, xs).or_else(|| xs.first().and_then(|x| scalar(t, x))),
            };
            if let Some(value) = value {
                c.set(k, value);
            }
        }
    }
    let mut keys: Vec<&str> = Vec::new();
    for f in 0..setup.filaments.len() {
        keys.extend(setup.fcfg(f).keys());
    }
    keys.sort_unstable();
    keys.dedup();
    for k in keys {
        if c.contains(k) {
            continue;
        }
        let Some(t) = option_type(k) else { continue };
        let row: Vec<String> = setup.per_filament(|f| {
            let cfg = setup.fcfg(f);
            cfg.get_str_at(k, setup.filament_row(f)).or_else(|| cfg.get_str_at(k, 0)).or_else(|| cfg.get_str(k)).unwrap_or("").to_string()
        });
        if let Some(value) = list(t, &row) {
            c.set(k, value);
        }
    }
}

/// Converts a raw preset value to a template [`Value`] of the requested kind.
/// `kind` is one of `"bool"`, `"int"`, `"float"`, `"string"`, and the vector forms
/// `"bools"`, `"ints"`, `"floats"`, `"strings"`. `nil` floats become `NaN`.
pub fn config_value_as(config: &Config, key: &str, kind: &str) -> Option<Value> {
    let v = config.get(key)?;
    let items = v.as_slice();
    let f = |s: &String| parse_f64(s).unwrap_or(f64::NAN);
    let i = |s: &String| s.trim().parse::<i64>().ok().or_else(|| parse_f64(s).map(|x| x as i64)).unwrap_or(0);
    let b = |s: &String| crate::config::parse_bool(s).unwrap_or(false);
    Some(match kind {
        "bool" => Value::Bool(b(items.first()?)),
        "int" => Value::Int(i(items.first()?)),
        "float" => Value::Float(f(items.first()?)),
        "string" => Value::Str(items.first()?.clone()),
        "bools" => Value::Bools(items.iter().map(b).collect()),
        "ints" => Value::Ints(items.iter().map(i).collect()),
        "floats" => Value::Floats(items.iter().map(f).collect()),
        "strings" => Value::Strs(items.to_vec()),
        _ => return None,
    })
}
