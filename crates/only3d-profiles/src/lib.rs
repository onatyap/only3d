//! # only3d-profiles
//!
//! Bambu Studio presets and G-code templates for only3d.
//!
//! * [`ProfileStore`] loads Bambu Studio's printer (`machine`), process and filament presets
//!   (from the installed Bambu Studio, or the copy embedded in this crate) and resolves them
//!   exactly like Bambu Studio: `inherits` chain, then `include` templates, then own keys.
//! * [`Config`] is the resolved ordered key/value map with typed getters and per-extruder-variant
//!   lookup ([`Config::variant_index`]).
//! * [`template`] renders Bambu's G-code templates (`machine_start_gcode`, ...) with the
//!   PrusaSlicer/Bambu placeholder language, bit-for-bit compatible number formatting
//!   ([`format`]).
//! * [`job`] computes the print-wide template variables (`filament_map`,
//!   `initial_no_support_filament_id`, `first_layer_print_min`, ...) the way Bambu's `GCode.cpp`
//!   does, plus typed per-event variable sets (layer change, tool change, timelapse).
//! * [`summary`] offers typed views of machine/filament/process presets.
//!
//! ```no_run
//! use only3d_profiles::{job, PresetKind, ProfileStore};
//!
//! let store = ProfileStore::system()?; // installed Bambu Studio, else embedded copy
//! let machine = store.resolve(PresetKind::Machine, "Bambu Lab H2D 0.4 nozzle")?;
//! let start = machine.get_str("machine_start_gcode").unwrap();
//! let vars = only3d_profiles::template::required_variables(start)?;
//! # let _ = (vars, job::H2D_LEFT);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! See `H2D_VARIABLES.md` in this crate for every variable the H2D templates use.

pub mod config;
pub mod error;
pub mod format;
pub mod gcode_config;
pub mod job;
mod option_types;
pub mod store;
pub mod summary;
pub mod template;
pub mod variants;

pub use config::{Config, ConfigValue, VariantTable, DEFAULT_VARIANT};
pub use error::ProfileError;
pub use store::{ListFilter, PresetKind, ProfileSource, ProfileStore, RawPreset};
pub use summary::{BedType, FilamentSummary, MachineSummary, ProcessSummary};
pub use template::{render, required_variables, Context, Template, TemplateError, Value};

/// Names of the G-code template keys a machine preset can carry, in the order Bambu uses them.
pub const GCODE_TEMPLATE_KEYS: &[&str] = &[
    "machine_start_gcode",
    "machine_end_gcode",
    "before_layer_change_gcode",
    "layer_change_gcode",
    "time_lapse_gcode",
    "wrapping_detection_gcode",
    "change_filament_gcode",
    "machine_pause_gcode",
    "template_custom_gcode",
    "change_extrusion_role_gcode",
];
