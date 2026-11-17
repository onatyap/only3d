//! The G-code CONFIG block, written the way Bambu Studio writes it.
//!
//! Bambu printers read this block. The H2D, for one, checks the nozzles declared there against
//! the ones installed (print_error 05FF_8053 "nozzle is not matched with slicing file"). Bambu
//! Studio's `GCode::append_full_config` writes every option of the full print config, sorted by
//! name, skipping a few banned keys and nil values, each serialized by its option type
//! (`ConfigOption*::serialize`): number/enum/bool/point vectors joined with `,`, string vectors
//! with `;` (C-style quoting), point groups with `#`, scalar strings C-escaped.

use crate::config::{Config, ConfigValue};
pub use crate::option_types::OptType;
use crate::option_types::OPTION_TYPES;

/// Keys Bambu Studio never writes into G-code.
const BANNED: &[&str] = &["compatible_printers", "compatible_prints", "print_host", "print_host_webui", "printhost_apikey", "printhost_cafile", "printhost_password", "printhost_port", "printhost_user"];

/// Bambu Studio's option type for `key`, if Bambu Studio knows the option.
pub fn option_type(key: &str) -> Option<OptType> {
    OPTION_TYPES.binary_search_by(|(k, _)| (*k).cmp(key)).ok().map(|i| OPTION_TYPES[i].1)
}

/// Values Bambu Studio fills in from code defaults (no system preset sets them). Captured from
/// Bambu Studio 02.09 by `tools/gen_bambu_options.py`; merge presets over them.
pub fn bambu_code_defaults() -> Config {
    static JSON: &str = include_str!("../data/bambu_code_defaults.json");
    match serde_json::from_str::<serde_json::Value>(JSON) {
        Ok(serde_json::Value::Object(m)) => Config::from_json_object(&m),
        _ => Config::new(),
    }
}

/// Bambu's `escape_string_cstyle`.
pub fn escape_string(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\r' => o.push_str("\\r"),
            '\n' => o.push_str("\\n"),
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c => o.push(c),
        }
    }
    o
}

/// Bambu's `escape_strings_cstyle`: `;`-separated, quoted when a string has blanks, quotes,
/// backslashes or line breaks (or is the only, empty, string).
pub fn escape_strings(v: &[String]) -> String {
    let mut o = String::new();
    for (i, s) in v.iter().enumerate() {
        if i > 0 {
            o.push(';');
        }
        let quote = (v.len() == 1 && s.is_empty()) || s.chars().any(|c| matches!(c, ' ' | '\t' | '\\' | '"' | '\r' | '\n'));
        if quote {
            o.push('"');
            for c in s.chars() {
                match c {
                    '\\' | '"' => {
                        o.push('\\');
                        o.push(c)
                    }
                    '\r' => o.push_str("\\r"),
                    '\n' => o.push_str("\\n"),
                    c => o.push(c),
                }
            }
            o.push('"');
        } else {
            o.push_str(s);
        }
    }
    o
}

fn is_nil(v: &ConfigValue) -> bool {
    match v {
        ConfigValue::Scalar(s) => s == "nil",
        ConfigValue::Array(a) => !a.is_empty() && a.iter().all(|s| s == "nil"),
    }
}

/// One option serialized like Bambu Studio's `opt_serialize`, or `None` when Bambu Studio would
/// not write it (unknown option, banned key or nil value).
pub fn serialize_option(key: &str, value: &ConfigValue) -> Option<String> {
    if BANNED.contains(&key) || is_nil(value) {
        return None;
    }
    let t = option_type(key)?;
    let items: Vec<String> = match value {
        ConfigValue::Scalar(s) => vec![s.clone()],
        ConfigValue::Array(a) => a.clone(),
    };
    let nums = |pct: bool| items.iter().map(|x| number(x, pct)).collect::<Vec<_>>().join(",");
    let first = items.first().cloned().unwrap_or_default();
    Some(match t {
        OptType::Strings => escape_strings(&items),
        OptType::PointsGroups => items.join("#"),
        OptType::Floats | OptType::FloatsOrPercents => nums(false),
        OptType::Percents => nums(true),
        OptType::Ints | OptType::Bools | OptType::Enums | OptType::Points => items.join(","),
        OptType::String => escape_string(&items.join(";")),
        // scalars: Bambu presets sometimes store them as one-element arrays
        OptType::Float | OptType::FloatOrPercent => number(&first, false),
        OptType::Percent => number(&first, true),
        // ConfigOptionPoint serializes as `x,y`; presets store `xXy`
        OptType::Point => first.replacen('x', ",", 1),
        _ => first,
    })
}

/// A number the way Bambu Studio prints it: shortest form (`12`, not `12.0`), keeping a `%`
/// suffix, and adding one for percent options. `nil` and non-numbers pass through.
fn number(s: &str, percent: bool) -> String {
    let (body, had_pct) = match s.strip_suffix('%') {
        Some(b) => (b, true),
        None => (s, false),
    };
    match body.trim().parse::<f64>() {
        Ok(x) if x.is_finite() => format!("{}{}", x, if had_pct || percent { "%" } else { "" }),
        _ => s.to_string(),
    }
}

/// Bambu's `update_values_to_printer_extruders`: per-variant arrays (one row per extruder
/// variant, see [`crate::variants`]) are reduced to the rows the job uses. Machine and process
/// keys keep one row per extruder, in extruder order; filament keys keep the row of the extruder
/// the filament prints with. `variants[e]` is extruder `e`'s variant, e.g. "Direct Drive Standard".
pub fn collapse_variants(cfg: &mut Config, machine: &Config, process: &Config, filament: &Config, variants: &[String], filament_extruder: usize) {
    use crate::config::VariantTable;
    use crate::variants::{FILAMENT_VARIANT_KEYS, PRINTER_VARIANT_KEYS, PRINTER_VARIANT_X2_KEYS, PROCESS_VARIANT_KEYS};
    let pick = |cfg: &mut Config, src: &Config, keys: &[&str], rows: &[usize], stride: usize| {
        for k in keys {
            let Some(ConfigValue::Array(all)) = src.get(k).cloned() else { continue };
            let mut out = Vec::with_capacity(rows.len() * stride);
            for &r in rows {
                for j in 0..stride {
                    out.push(all.get(r * stride + j).or_else(|| all.last()).cloned().unwrap_or_default());
                }
            }
            cfg.insert(*k, out);
        }
    };
    let rows = |src: &Config, table: VariantTable| -> Vec<usize> { variants.iter().enumerate().map(|(e, v)| src.variant_index_or_zero(table, e, v)).collect() };
    let mrows = rows(machine, VariantTable::Printer);
    pick(cfg, machine, PRINTER_VARIANT_KEYS, &mrows, 1);
    pick(cfg, machine, PRINTER_VARIANT_X2_KEYS, &mrows, 2);
    let prows = rows(process, VariantTable::Process);
    pick(cfg, process, PROCESS_VARIANT_KEYS, &prows, 1);
    let v = variants.get(filament_extruder).cloned().unwrap_or_default();
    let frow = filament.variant_index_or_zero(VariantTable::Filament, filament_extruder, &v);
    pick(cfg, filament, FILAMENT_VARIANT_KEYS, &[frow], 1);
    // other filament lists hold one value per filament in the full config: Bambu takes element
    // 0 of each filament preset's list (e.g. the per-AMS-model drying temperatures)
    for (k, val) in filament.iter() {
        if FILAMENT_VARIANT_KEYS.contains(&k) {
            continue;
        }
        if let (ConfigValue::Array(a), Some(t)) = (val, option_type(k)) {
            if a.len() > 1 && !matches!(t, OptType::String) && !is_scalar(t) {
                cfg.insert(k, vec![a[0].clone()]);
            }
        }
    }
}

/// The filament keys of the full config for a plate with several filaments, as Bambu Studio
/// builds it: every filament option holds one value per filament (index = filament id). A
/// per-variant key takes the row of the variant installed on that filament's extruder; other
/// lists take their first element (as `collapse_variants` does for one filament).
pub fn merge_filaments(cfg: &mut Config, filaments: &[(&Config, usize)], variants: &[String]) {
    use crate::config::VariantTable;
    use crate::variants::FILAMENT_VARIANT_KEYS;
    let mut keys: Vec<String> = Vec::new();
    for (f, _) in filaments {
        for (k, _) in f.iter() {
            if !keys.iter().any(|x| x == k) {
                keys.push(k.to_string());
            }
        }
    }
    for k in keys {
        // scalar options are not per filament (name, inherits, …)
        if option_type(&k).is_some_and(is_scalar) {
            continue;
        }
        let fallback = match cfg.get(&k) {
            Some(ConfigValue::Array(a)) => a.first().cloned().unwrap_or_default(),
            Some(ConfigValue::Scalar(x)) => x.clone(),
            None => String::new(),
        };
        let out: Vec<String> = filaments
            .iter()
            .map(|(f, e)| match f.get(&k) {
                Some(ConfigValue::Array(a)) if FILAMENT_VARIANT_KEYS.contains(&k.as_str()) => {
                    let v = variants.get(*e).cloned().unwrap_or_default();
                    let row = f.variant_index_or_zero(VariantTable::Filament, *e, &v);
                    a.get(row).or_else(|| a.last()).cloned().unwrap_or_default()
                }
                Some(ConfigValue::Array(a)) => a.first().cloned().unwrap_or_default(),
                Some(ConfigValue::Scalar(x)) => x.clone(),
                None => fallback.clone(),
            })
            .collect();
        cfg.insert(k, out);
    }
}

fn is_scalar(t: OptType) -> bool {
    matches!(t, OptType::Float | OptType::Int | OptType::String | OptType::Percent | OptType::FloatOrPercent | OptType::Point | OptType::Point3 | OptType::Bool | OptType::Enum)
}

/// The whole CONFIG block body (`; key = value` lines, sorted by key), like Bambu Studio.
pub fn config_block(cfg: &Config) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = cfg.iter().filter_map(|(k, val)| serialize_option(k, val).map(|s| (k.to_string(), s))).collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arr(v: &[&str]) -> ConfigValue {
        ConfigValue::Array(v.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn serializes_like_bambu_studio() {
        // the H2D's nozzle check needs `0.4,0.4`; only3d once wrote `0.4;0.4`
        assert_eq!(serialize_option("nozzle_diameter", &arr(&["0.4", "0.4"])).unwrap(), "0.4,0.4");
        assert_eq!(serialize_option("nozzle_volume_type", &arr(&["Standard", "Standard"])).unwrap(), "Standard,Standard");
        assert_eq!(serialize_option("extruder_nozzle_stats", &arr(&["Standard#1", "Standard#1"])).unwrap(), "Standard#1;Standard#1");
        assert_eq!(serialize_option("print_extruder_variant", &arr(&["Direct Drive Standard", "Direct Drive Standard"])).unwrap(), "\"Direct Drive Standard\";\"Direct Drive Standard\"");
        assert_eq!(serialize_option("extruder_printable_area", &arr(&["0x0,325x0", "25x0,350x0"])).unwrap(), "0x0,325x0#25x0,350x0");
        assert_eq!(serialize_option("machine_start_gcode", &ConfigValue::Scalar("G28\nM104 S\"x\"".into())).unwrap(), "G28\\nM104 S\\\"x\\\"");
        assert_eq!(serialize_option("default_filament_colour", &arr(&[""])).unwrap(), "\"\"");
        assert_eq!(serialize_option("layer_height", &arr(&["0.2"])).unwrap(), "0.2");
        assert_eq!(serialize_option("top_shell_thickness", &ConfigValue::Scalar("1.0".into())).unwrap(), "1");
        assert_eq!(serialize_option("top_surface_density", &ConfigValue::Scalar("100".into())).unwrap(), "100%");
        assert_eq!(serialize_option("best_object_pos", &ConfigValue::Scalar("0.3x0.5".into())).unwrap(), "0.3,0.5");
        assert_eq!(serialize_option("retract_before_wipe", &arr(&["0%", "0%"])).unwrap(), "0%,0%");
        // nil, banned and unknown keys are left out
        assert!(serialize_option("filament_z_hop", &arr(&["nil"])).is_none());
        assert!(serialize_option("compatible_printers", &arr(&["x"])).is_none());
        assert!(serialize_option("wall_infill_order", &ConfigValue::Scalar("x".into())).is_none());
    }

    #[test]
    fn code_defaults_load() {
        let d = bambu_code_defaults();
        assert!(d.len() > 100);
        assert_eq!(d.get_str("wall_sequence"), Some("inner wall/outer wall"));
    }
}
