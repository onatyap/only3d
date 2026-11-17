# Bambu Lab H2D G-code template variables

Every variable referenced by the G-code templates of the **Bambu Lab H2D 0.4 nozzle** machine
preset (Bambu Studio profile bundle `02.09.00.75`), what it means, its type, and how Bambu Studio
computes it (`src/libslic3r/GCode.cpp`, BambuStudio `master`, Nov 2026). `only3d_profiles::job`
implements all of it:

* `job::build_print_context(&setup, &job)`: the print-wide variables (everything the start
  G-code needs, plus the per-filament/per-extruder arrays every template may use).
* `PrintSetup::{layer_change_vars, before_layer_change_vars, timelapse_vars,
  wrapping_detection_vars, end_vars, toolchange_vars}`: the per-event overrides.
* Render with `Template::render_layered(&[&event_vars, &print_vars])` (first layer wins, like
  Bambu's `config_override`).

`cargo run -p only3d-profiles --example list_variables` prints the variables per template for any
machine preset; `Template::missing_variables(&[..])` tells you what a context still lacks.

**Verified against Bambu Studio 02.09.01.70:** the start G-code (PLA cube and a tall PETG part),
layer-change, inline-photo timelapse and end G-code rendered by this crate are byte-identical to
what `BambuStudio --slice` writes, except for the `M73 P.. R..` progress lines Bambu's G-code
processor inserts afterwards (fixtures in `tests/snapshots/bambu_reference/`). The tool-change
variables are derived from source only (the Bambu CLI segfaults on two-filament slices here).

## Conventions you must get right

| concept | range | H2D meaning |
|---|---|---|
| **filament id** | 0-based index into the project filament list | `filament_type[f]`, `nozzle_temperature[f]`, ... are indexed by it |
| **logical extruder** | 0-based | `0` = **left** nozzle, `1` = **right** nozzle (the main one; `master_extruder_id` = 2) |
| `filament_map[f]` | **1-based** logical extruder of filament `f` | `1` = left, `2` = right |
| **physical (firmware) extruder** | `physical_extruder_map[logical]` = `[1, 0]` | right nozzle = firmware extruder **0** (`T0`, `M104 ... T0`), left = **1**. Templates compute it as `filament_map[f] % 2` |
| `first_filaments`, `first_non_support_filaments` | indexed by **physical** extruder | `[0]` = right nozzle, `[1]` = left nozzle; `-1` = nozzle unused |
| **nozzle id** | 0-based | one nozzle per extruder on the H2D, so nozzle id == logical extruder (`nozzle_diameter_at_nozzle_id[initial_nozzle_id]`) |
| **hotend** (`*_hotend`) | `-1` on the H2D | only meaningful on machines with a hotend rack; Bambu emits `H-1`, `B-1`, `D-1` for the H2D (`NOZZLE_ID_FOR_GCODE` returns -1 unless the print uses dynamic nozzle mapping) |
| **current filament** | filament id passed to the parser | selects the element printed by legacy `[vector]` expansions. Bambu passes the initial filament for start G-code, the **new** filament for change-filament, the active one for layer/timelapse/wrapping/end. `Context::set_current_filament_id` |

Per-variant arrays in the presets (7 rows on the H2D 0.4, `[L Std, L HF, L E3D-HF, R Std, R HF,
R TPU-HF, R E3D-HF]`; filament presets: `[Std, HF, E3D-HF]`) are collapsed **before** templates
see them: machine/process values become one entry per extruder, filament values one entry per
filament (the row of the variant of the extruder that filament is mapped to). An extruder's
variant is `"<extruder_type> <nozzle_volume_type>"`, e.g. `Direct Drive Standard` or
`Direct Drive High Flow`; Bambu's project config stores `nozzle_volume_type = Standard,Standard`.
In the Bambu CLI test a single filament was auto-mapped to the **left** nozzle (`filament_map = 1`).

## Rendering rules that bite

* **Types matter.** `{if x}` needs a bool, vector indices must be ints, `int / int` is integer
  division, `ceil()` returns an int. Use exactly the types below (the `job` module does).
* **Every branch is evaluated**, so every variable of every branch must exist (e.g. the
  `travel_point_*` inside `{if toolchange_count == 2}`), and `&&`/`||` don't short-circuit.
* Out-of-range or negative vector indices read element 0 (`hotend_cooling_rate[filament_map[f]-1]`
  relies on it never being out of range).
* `timelapse_type` is an enum: Bambu serializes it, so templates see the **string** `"0"`/`"1"`
  (`timelapse_type == 0` compares as strings and works; `M{timelapse_type}` prints `0`).
* Numbers print like C `%g` with 6 significant digits (`498.898`, `623.623`, `10.4167`, `0`,
  `-0.02`, `1e+06`); bools print `true`/`false` in `{}` but `1`/`0` in legacy `[]`.
* Bambu appends `"\n"` after `layer_change_gcode`, `time_lapse_gcode`,
  `wrapping_detection_gcode` and `before_layer_change_gcode`, writes `; FEATURE: Custom` before
  the start G-code and `; MACHINE_START_GCODE_END` after it, then the initial filament's
  `filament_start_gcode` and `;VT<initial filament> H<hotend>`. It skips its own `M140/M190`
  and `M104/M109` pre-heat lines because the H2D start G-code already contains them.

## Print-wide variables (start G-code)

| variable | type | meaning / how to compute |
|---|---|---|
| `filament_type` | string vector | per filament, `filament_type` of its preset (`PLA`, `PETG`, `TPU`, `PA`, `PLA-CF`, ...) |
| `filament_map` | int vector | per filament, **1-based** logical extruder (1 = left, 2 = right) |
| `nozzle_temperature` | int vector | per filament, `nozzle_temperature` at its variant row |
| `nozzle_temperature_initial_layer` | int vector | per filament, `nozzle_temperature_initial_layer` at its variant row |
| `filament_max_volumetric_speed` | float vector | per filament, at its variant row (mm³/s) |
| `flush_volumetric_speeds` | float vector | per filament `filament_flush_volumetric_speed` (variant row); if 0, `filament_max_volumetric_speed` |
| `flush_temperatures` | int vector | per filament `filament_flush_temp` (or `filament_flush_temp_fast` when the process `prime_volume_mode` is `Fast`); if 0, `nozzle_temperature_range_high` |
| `retraction_distances_when_cut` | float vector | per filament: the machine `retraction_distances_when_cut` at its extruder's variant row, overridden by the filament's `filament_retraction_distances_when_cut` unless `nil` (override only when the machine's `enable_long_retraction_when_cut` = 2, as on the H2D) |
| `retraction_distance_when_ec` | float | `retraction_distances_when_ec` of the **initial filament** (variant row); filament key |
| `long_retraction_when_ec` | bool | `long_retractions_when_ec` of the initial filament (variant row) |
| `initial_no_support_filament_id` | int | first non-support filament in print order (`filament_is_support` = 0); the initial filament when all are support |
| `initial_nozzle_id` | int | nozzle (= logical extruder) of the **initial filament** (the first filament printed, support or not) |
| `initial_no_support_hotend` | int | `-1` on the H2D |
| `first_filaments` | int vector | per **physical** extruder, the first filament printed with it (tool ordering of all layers in order), `-1` if unused |
| `first_non_support_filaments` | int vector | same, non-support filaments only (all `-1` if every filament is support) |
| `nozzle_diameter_at_nozzle_id` | float vector | nozzle diameter per nozzle id (= machine `nozzle_diameter`, `[0.4, 0.4]`) |
| `bed_temperature_initial_layer_single` | int | first-layer bed temperature for `curr_bed_type`: with the H2D's `bed_temperature_formula = by_highest_temp`, the max of `<plate>_temp_initial_layer` over the filaments printed on the first layer; otherwise the initial filament's value. Plate keys: Cool Plate `cool_plate_temp`, Engineering Plate `eng_plate_temp`, High Temp Plate `hot_plate_temp`, Textured PEI Plate `textured_plate_temp`, Supertack Plate `supertack_plate_temp` (+ `_initial_layer`) |
| `curr_bed_type` | string | `Textured PEI Plate`, `Cool Plate`, `Engineering Plate`, `High Temp Plate`, `Supertack Plate` (the H2D does not support Cool Plate) |
| `overall_chamber_temperature` | int | max `chamber_temperatures` over the used filaments (0 for PLA/PETG; >= 40 switches the airduct to heating and waits for the chamber) |
| `min_vitrification_temperature` | int | min `temperature_vitrification` over the used filaments (PLA 45, PETG 64...; <= 50 selects PLA chamber auto-cooling and the external toolhead fan) |
| `is_all_bbl_filament` | bool | every used filament has `filament_vendor` == `Bambu Lab` |
| `cooling_filter_enabled` | bool | machine `cooling_filter_enabled` (project setting; preset default 0) |
| `has_tpu_in_first_layer` | bool | any first-layer filament has type `TPU` |
| `first_layer_print_min` | float vector `[x, y]` | min corner (bed mm) of the first-layer bounding box: objects + support + brim + skirt + prime tower (Bambu `first_layer_projection`); drives the partial bed mesh `G29 A1 O X.. Y.. I.. J..` |
| `first_layer_print_size` | float vector `[w, h]` | size of that box |
| `max_print_z` | int | `ceil` of the highest object `print_z` (mm); >= 145 triggers the heatbed underside scan `G3811` |

Also provided (Bambu sets them; other machines or filament G-code use them): `first_layer_print_max`,
`first_layer_center_no_wipe_tower`, `first_layer_print_convex_hull` (not provided: point lists are
not supported), `print_bed_min/max/size`, `in_head_wrap_detect_zone`, `total_toolchanges`,
`has_wipe_tower`, `has_wipe_tower_this_layer`, `initial_tool`, `initial_extruder`,
`initial_filament_id`, `initial_extruder_id`, `initial_no_support_extruder(_id)`,
`initial_no_support_tool`, `initial_no_support_nozzle_id`, `first_tools`,
`first_non_support_tools`, `first_non_support_hotend`, `current_extruder`, `current_hotend`,
`bed_temperature`, `bed_temperature_initial_layer`, `first_layer_bed_temperature`,
`first_layer_temperature`, `chamber_temperature`, `max_print_height`, `z_offset`, `plate_name`,
`max_additional_fan`, `outer_wall_volumetric_speed`, `during_print_exhaust_fan_speed_num`,
`hold_chamber_temp_for_flat_print`, `first_layer_area_sum`, `wipe_tower_center_pos_x/y/valid`,
`retraction_distance_when_cut`, `long_retraction_when_cut`, `retraction_distances_when_ec`,
`long_retractions_when_ec`, `long_retractions_when_cut`, the per-filament effective machine
retraction settings (`retraction_length`, `z_hop`, `wipe`, ... with filament overrides applied) and
the per-extruder process speeds/accelerations listed below.

## Per-extruder arrays (from machine/process presets, collapsed to each extruder's variant)

| variable | type | used by | meaning |
|---|---|---|---|
| `hotend_cooling_rate` | float vector | change filament | °C/s, machine variant row per extruder; indexed `[filament_map[f]-1]` |
| `hotend_heating_rate` | float vector | change filament | °C/s, likewise |
| `initial_layer_acceleration` | float vector | change filament (`M204 S[...]` on layer 1) | process value per extruder (500 mm/s² for 0.20mm Standard); legacy `[]` prints element *current filament id*, element 0 when out of range |
| `default_acceleration` | float vector | change filament (`M204 S[...]` above layer 1) | process value per extruder (8000) |
| `initial_layer_print_height` | float | change filament | process first-layer height (0.2) |

## Layer change, timelapse, wrapping detection, end

| variable | type | template | meaning |
|---|---|---|---|
| `layer_num` | int | layer change, wrapping detection, (timelapse, end) | 0-based index of the layer being started (`m_layer_index`, already advanced). Layer change prints `M73 L{layer_num+1}` and `M991 S0 P{layer_num}`; wrapping detection runs `G39` on layers 3, 10 and 19 |
| `total_layer_count` | int | layer change | number of layers of the print |
| `layer_z` | float | timelapse, change filament | `print_z` of the current layer (change filament: of the tool-change layer) |
| `max_layer_z` | float | timelapse, end, change filament | highest Z printed so far; end G-code lifts to `max_layer_z + 0.4`, `+ 10`, then `+ 100 - max_layer_z/2` capped at Z320 |
| `spiral_mode` | bool | timelapse, wrapping detection | process `spiral_mode` (vase mode) |
| `timelapse_type` | string `"0"`/`"1"` | timelapse | 0 = traditional, 1 = smooth (project setting) |
| `timelapse_inline_photo` | bool | timelapse | Bambu `skip_pos_pick`: take the photo where the head is (`M971 S11 C10 O0 T0`, `M1004 S5 P1`) |
| `has_timelapse_safe_pos` | bool | timelapse | a safe camera position was found (`timelapse_pos` != (0,0)) |
| `timelapse_pos_x`, `timelapse_pos_y` | int | timelapse | that safe position in whole mm (0,0 when none / inline) |
| `most_used_physical_extruder_id` | int | timelapse (and layer change/wrapping) | firmware extruder (`physical_extruder_map[...]`) that prints most of this layer |
| `curr_physical_extruder_id` | int | timelapse, wrapping detection | firmware extruder of the active filament |
| `farthest_point_timelapse_enabled` | bool | timelapse | effective "farthest point" timelapse (machine `farthest_point_timelapse` = 1 on the H2D, folded with smooth/traditional) — photo at `layer_z` instead of `layer_z + 0.4` |

The layer-change context also carries `current_filament_id`, `current_extruder_id`,
`current_nozzle_id`, `curr_y_acceleration_limit`, `curr_accumulated_mass`, `curr_layer_mass`;
the end context `filament_extruder_id`, `current_*`, `nozzle_diameter_at_nozzle_id`,
`nozzle_volume_types`; the timelapse context `clear_to_x0`.

## Change filament (prime-tower tool change, `WipeTowerIntegration::append_tcr`)

The current filament id of this render is the **new** filament.

| variable | type | meaning / how to compute |
|---|---|---|
| `current_filament_id` | int | filament being unloaded (`-1` before the first extrusion) |
| `next_filament_id` | int | filament being loaded |
| `current_nozzle_id`, `next_nozzle_id` | int | their nozzle ids (= logical extruders) |
| `current_hotend`, `next_hotend` | int | `-1` on the H2D |
| `toolchange_count` | int | 1-based number of this tool change (`m_toolchange_count + 1`); `M620 Q{toolchange_count + 1}` |
| `flush_length` | float | purge **length** of filament (mm): `purge_volume / (π/4·d²)` where `purge_volume` = flush-matrix volume, raised to at least 100 mm³, 0 when < ε |
| `old_filament_temp` | int | old filament's `nozzle_temperature` (or `_initial_layer` while it is still in its first-layer phase), 210 when there is none |
| `new_filament_temp` | int | new filament's, same rule |
| `old_filament_e_feedrate`, `new_filament_e_feedrate` | int | `int(60 · max_volumetric_speed / filament_area)` (mm/min); 100 if that is 0; old: 200 when there is none |
| `new_extruder_retracted_length` | float | how far the new filament is currently retracted (writer state) |
| `filament_cooling_before_tower` | float vector | per filament `filament_cooling_before_tower` (variant row), **all 0** on layer 0 or when the tool change prints a tower interface |
| `is_prime_tower_interface` | bool | the tool change prints a prime-tower contact/interface (`tcr.is_contact`) and the process has `enable_tower_interface_features` = 1, else false |
| `filament_tower_interface_purge_volume` | float | new filament's `filament_tower_interface_purge_volume` when interface features are on, else 0 |
| `filament_tower_interface_print_temp` | int | new filament's `filament_tower_interface_print_temp` (-1 -> `nozzle_temperature_range_high`) when interface features are on, else -1 |
| `long_retraction_when_cut` | bool | effective `long_retractions_when_cut` of the filament being **unloaded** (still the value set at the previous tool change / start) |
| `retraction_distance_when_cut` | float | effective `retraction_distances_when_cut` of that filament |
| `retraction_distance_when_ec`, `long_retraction_when_ec` | float, bool | its `retraction_distances_when_ec` / `long_retractions_when_ec` |
| `nozzle_volume_types` | string vector | per nozzle id up to the highest used: `Standard`, `High Flow`, `TPU High Flow`, `E3D High Flow` (the extruder variant without `Direct Drive `); TPU High Flow on the right nozzle changes the TPU unload |
| `x_after_toolchange`, `y_after_toolchange` | float | prime-tower start position of this tool change |
| `z_after_toolchange` | float | current nozzle Z |
| `travel_point_1_x`, `travel_point_1_y`, `travel_point_2_x`, `travel_point_2_y`, `travel_point_3_x`, `travel_point_3_y` | float | the three travel points of the tool-change path (only in a commented-out branch, but must exist) |
| `wipe_avoid_perimeter` | bool | travel avoids perimeters to reach the tower |
| `wipe_avoid_pos_x` | float | X used for that avoidance move (`get_wipe_avoid_pos_x` of the tower box) |

Bambu also sets `previous_extruder`, `next_extruder`, `next_extruder_id`, `current_extruder_id`,
`layer_num`, `toolchange_z`, `relative_e_axis`, `fan_speed` (0), `old/new_retract_length`,
`old/new_retract_length_toolchange`, `filament_retract_length_nc`, `first/second_flush_volume`
(half of `flush_length` each), `old/new_extruder_variant`, `outer_wall_volumetric_speed`,
`wipe_tower_center_pos_x/y/valid`, `flush_length_1` .. `flush_length_4` (purge split into
`round(volume/135)` chunks, at most 4) — all provided by `PrintSetup::toolchange_vars`.
