//! Ordered key -> value configuration map with Bambu-style typed getters.
//!
//! Bambu Studio preset JSON stores every value as a string, or as an array of strings
//! (`"0.4"`, `["220", "220", "220"]`, `"15%"`, `"nil"`). [`Config`] keeps them verbatim and
//! offers typed getters on top.
//!
//! # Per-variant arrays
//!
//! Dual-nozzle machines (H2D) store many settings as arrays with one entry per *extruder
//! variant* rather than per extruder. Which row belongs to which extruder is described by a
//! pair of keys in the same preset:
//!
//! | preset   | id key (1-based extruder) | variant key                 | rows for H2D 0.4 |
//! |----------|---------------------------|-----------------------------|------------------|
//! | machine  | `printer_extruder_id`     | `printer_extruder_variant`  | 7 |
//! | process  | `print_extruder_id`       | `print_extruder_variant`    | 7 |
//! | filament | (none)                    | `filament_extruder_variant` | 3 (one per variant, extruder-agnostic) |
//!
//! For the H2D 0.4 machine/process the 7 rows are
//! `[1 Standard, 1 High Flow, 1 E3D High Flow, 2 Standard, 2 High Flow, 2 TPU High Flow, 2 E3D High Flow]`
//! (all `Direct Drive ...`); extruder id `1` is the **left** nozzle, `2` the **right** nozzle.
//! Filament arrays have one row per variant (`Standard`, `High Flow`, `E3D High Flow`), and the
//! row is picked by the variant of the extruder the filament is assigned to.
//! Machine-limit keys (`machine_max_*`) have **two** rows per machine variant
//! (`[normal, silent]`), i.e. 14 entries for the H2D.
//!
//! Use [`Config::variant_index`] to find the row, then `get_*_at(key, row)`. Getters fall back to
//! element 0 when an array is shorter than the requested index (this matches Bambu's
//! `ConfigOptionVector::get_at`). Which keys are per-variant is listed in
//! [`crate::variants`].

use std::collections::HashMap;
use std::fmt;

use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};

/// The literal Bambu uses for "unset, inherit from the machine/process" in nullable arrays.
pub const NIL: &str = "nil";

/// A raw preset value: a scalar string or an array of strings.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ConfigValue {
    Scalar(String),
    Array(Vec<String>),
}

impl ConfigValue {
    /// The scalar value, or element 0 of an array (Bambu sometimes stores scalars as
    /// one-element arrays, e.g. `"enable_long_retraction_when_cut": ["2"]`).
    pub fn first(&self) -> Option<&str> {
        match self {
            ConfigValue::Scalar(s) => Some(s),
            ConfigValue::Array(v) => v.first().map(String::as_str),
        }
    }

    /// Element `idx`, falling back to element 0 when the array is shorter; scalars ignore `idx`.
    pub fn at(&self, idx: usize) -> Option<&str> {
        match self {
            ConfigValue::Scalar(s) => Some(s),
            ConfigValue::Array(v) => v.get(idx).or_else(|| v.first()).map(String::as_str),
        }
    }

    /// The value as a slice (a scalar is a one-element slice).
    pub fn as_slice(&self) -> &[String] {
        match self {
            ConfigValue::Scalar(s) => std::slice::from_ref(s),
            ConfigValue::Array(v) => v,
        }
    }

    /// Number of entries (1 for a scalar).
    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    /// True for an empty array.
    pub fn is_empty(&self) -> bool {
        self.as_slice().is_empty()
    }

    pub fn is_array(&self) -> bool {
        matches!(self, ConfigValue::Array(_))
    }

    /// Converts a JSON value from a preset file. Strings and arrays of strings are kept verbatim;
    /// numbers/bools (never present in Bambu presets) are stringified.
    pub fn from_json(value: &serde_json::Value) -> ConfigValue {
        fn scalar(v: &serde_json::Value) -> String {
            match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Bool(b) => if *b { "1" } else { "0" }.to_string(),
                serde_json::Value::Null => String::new(),
                serde_json::Value::Array(items) => {
                    items.iter().map(scalar).collect::<Vec<_>>().join(",")
                }
                other => other.to_string(),
            }
        }
        match value {
            serde_json::Value::Array(items) => ConfigValue::Array(items.iter().map(scalar).collect()),
            other => ConfigValue::Scalar(scalar(other)),
        }
    }

    /// The JSON representation (string or array of strings).
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            ConfigValue::Scalar(s) => serde_json::Value::String(s.clone()),
            ConfigValue::Array(v) => {
                serde_json::Value::Array(v.iter().cloned().map(serde_json::Value::String).collect())
            }
        }
    }
}

impl From<&str> for ConfigValue {
    fn from(s: &str) -> Self {
        ConfigValue::Scalar(s.to_string())
    }
}
impl From<String> for ConfigValue {
    fn from(s: String) -> Self {
        ConfigValue::Scalar(s)
    }
}
impl From<Vec<String>> for ConfigValue {
    fn from(v: Vec<String>) -> Self {
        ConfigValue::Array(v)
    }
}
impl From<Vec<&str>> for ConfigValue {
    fn from(v: Vec<&str>) -> Self {
        ConfigValue::Array(v.into_iter().map(str::to_string).collect())
    }
}

impl Serialize for ConfigValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            ConfigValue::Scalar(s) => serializer.serialize_str(s),
            ConfigValue::Array(v) => {
                let mut seq = serializer.serialize_seq(Some(v.len()))?;
                for item in v {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
        }
    }
}

impl fmt::Display for ConfigValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigValue::Scalar(s) => f.write_str(s),
            ConfigValue::Array(v) => f.write_str(&v.join(";")),
        }
    }
}

/// Parses a Bambu number. Accepts plain floats/ints; returns `None` for `nil`, empty strings,
/// percentages and garbage.
pub fn parse_f64(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() || t == NIL || t.ends_with('%') {
        return None;
    }
    t.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Parses a Bambu bool (`"1"`, `"0"`, `"true"`, `"false"`).
pub fn parse_bool(s: &str) -> Option<bool> {
    match s.trim() {
        "1" | "true" | "True" | "TRUE" => Some(true),
        "0" | "false" | "False" | "FALSE" => Some(false),
        _ => None,
    }
}

/// Parses `"15%"` as a ratio of `base` or `"0.4"` as an absolute value.
pub fn parse_percent_or_abs(s: &str, base: f64) -> Option<f64> {
    let t = s.trim();
    if let Some(p) = t.strip_suffix('%') {
        p.trim().parse::<f64>().ok().map(|p| base * p / 100.0)
    } else {
        parse_f64(t)
    }
}

/// Parses `"12x34"` into `[12.0, 34.0]`.
pub fn parse_point(s: &str) -> Option<[f64; 2]> {
    let (x, y) = s.trim().split_once(['x', 'X'])?;
    Some([x.trim().parse().ok()?, y.trim().parse().ok()?])
}

/// An insertion-ordered map of preset keys to raw values.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Config {
    entries: Vec<(String, ConfigValue)>,
    index: HashMap<String, usize>,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.entries.iter().map(|(k, v)| (k, v))).finish()
    }
}

impl Serialize for Config {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.entries.len()))?;
        for (k, v) in &self.entries {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

impl<K: Into<String>, V: Into<ConfigValue>> FromIterator<(K, V)> for Config {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut c = Config::new();
        for (k, v) in iter {
            c.insert(k, v);
        }
        c
    }
}

impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a config from a JSON object (every member becomes a key).
    pub fn from_json_object(obj: &serde_json::Map<String, serde_json::Value>) -> Self {
        obj.iter().map(|(k, v)| (k.clone(), ConfigValue::from_json(v))).collect()
    }

    /// JSON object representation (key order preserved).
    pub fn to_json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        for (k, v) in &self.entries {
            map.insert(k.clone(), v.to_json());
        }
        serde_json::Value::Object(map)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.index.contains_key(key)
    }

    /// Inserts or replaces a value. A replaced key keeps its position.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<ConfigValue>) {
        let key = key.into();
        let value = value.into();
        if let Some(&i) = self.index.get(&key) {
            self.entries[i].1 = value;
        } else {
            self.index.insert(key.clone(), self.entries.len());
            self.entries.push((key, value));
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<ConfigValue> {
        let i = self.index.remove(key)?;
        let (_, v) = self.entries.remove(i);
        for (j, (k, _)) in self.entries.iter().enumerate().skip(i) {
            self.index.insert(k.clone(), j);
        }
        Some(v)
    }

    /// Applies `other` on top of `self` (Bambu's `DynamicConfig::apply`): every key of `other`
    /// overrides the same key here; new keys are appended.
    pub fn merge(&mut self, other: &Config) {
        for (k, v) in &other.entries {
            self.insert(k.clone(), v.clone());
        }
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(k, _)| k.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &ConfigValue)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn get(&self, key: &str) -> Option<&ConfigValue> {
        self.index.get(key).map(|&i| &self.entries[i].1)
    }

    /// Number of entries of `key` (1 for scalars), or `None` if absent.
    pub fn len_of(&self, key: &str) -> Option<usize> {
        self.get(key).map(ConfigValue::len)
    }

    /// Scalar string, or element 0 of an array.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key)?.first()
    }

    /// Element `idx` (falls back to element 0 when the array is shorter).
    pub fn get_str_at(&self, key: &str, idx: usize) -> Option<&str> {
        self.get(key)?.at(idx)
    }

    /// All entries as strings (a scalar is a one-element slice).
    pub fn get_strs(&self, key: &str) -> Option<&[String]> {
        self.get(key).map(ConfigValue::as_slice)
    }

    /// True when the entry at `idx` (with element-0 fallback) is `nil`.
    pub fn is_nil_at(&self, key: &str, idx: usize) -> bool {
        self.get_str_at(key, idx).is_some_and(|s| s.trim() == NIL)
    }

    /// Scalar (or element 0) as a number. `None` for missing keys, `nil`, percentages.
    pub fn get_f64(&self, key: &str) -> Option<f64> {
        parse_f64(self.get_str(key)?)
    }

    /// Element `idx` as a number (element-0 fallback). `None` for missing keys and `nil`.
    pub fn get_f64_at(&self, key: &str, idx: usize) -> Option<f64> {
        let s = self.get_str_at(key, idx)?;
        // some makers' profiles join a list into one element ("0.4,0.4")
        parse_f64(s).or_else(|| s.contains(',').then(|| s.split(',').nth(idx).or(s.split(',').next()).and_then(parse_f64)).flatten())
    }

    /// All entries as numbers; `nil`/unparsable entries become `None`.
    pub fn get_f64s(&self, key: &str) -> Option<Vec<Option<f64>>> {
        Some(self.get_strs(key)?.iter().map(|s| parse_f64(s)).collect())
    }

    /// Scalar (or element 0) as an integer (accepts `"220"` and `"220.0"`).
    pub fn get_i64(&self, key: &str) -> Option<i64> {
        self.get_i64_at(key, 0)
    }

    /// Element `idx` as an integer (element-0 fallback).
    pub fn get_i64_at(&self, key: &str, idx: usize) -> Option<i64> {
        let s = self.get_str_at(key, idx)?.trim();
        s.parse::<i64>().ok().or_else(|| parse_f64(s).map(|f| f.round() as i64))
    }

    /// Scalar (or element 0) as a bool (`"1"`/`"0"`/`"true"`/`"false"`).
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        parse_bool(self.get_str(key)?)
    }

    /// Element `idx` as a bool (element-0 fallback). `None` for `nil`.
    pub fn get_bool_at(&self, key: &str, idx: usize) -> Option<bool> {
        parse_bool(self.get_str_at(key, idx)?)
    }

    /// `"15%"` -> `base * 0.15`, `"0.4"` -> `0.4`.
    pub fn get_percent_or_abs(&self, key: &str, base: f64) -> Option<f64> {
        parse_percent_or_abs(self.get_str(key)?, base)
    }

    /// A percent option (`coPercent`) as a fraction: `"30%"` and `"30"` are both 0.30 (Bambu and
    /// Orca store either form).
    pub fn get_percent(&self, key: &str) -> Option<f64> {
        parse_f64(self.get_str(key)?.trim().trim_end_matches('%')).map(|v| v / 100.0)
    }

    /// Per-index variant of [`Config::get_percent_or_abs`].
    pub fn get_percent_or_abs_at(&self, key: &str, idx: usize, base: f64) -> Option<f64> {
        parse_percent_or_abs(self.get_str_at(key, idx)?, base)
    }

    /// A list of `"XxY"` points, e.g. `printable_area`.
    pub fn get_points(&self, key: &str) -> Option<Vec<[f64; 2]>> {
        // a list of points, or (some makers' profiles) one comma-separated string of them
        self.get_strs(key)?.iter().flat_map(|s| s.split(',')).filter(|s| !s.trim().is_empty()).map(parse_point).collect()
    }

    /// A polygon stored as one comma-separated string per entry, e.g. `extruder_printable_area`
    /// (`"0x0,325x0,325x320,0x320"`). Element-0 fallback.
    pub fn get_polygon_at(&self, key: &str, idx: usize) -> Option<Vec<[f64; 2]>> {
        self.get_str_at(key, idx)?
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .map(parse_point)
            .collect()
    }

    /// Finds the row of a per-variant array for logical extruder `extruder` (**0-based**:
    /// H2D `0` = left, `1` = right) and variant name (e.g. `"Direct Drive Standard"`).
    ///
    /// Mirrors Bambu's `get_config_index_base`: an exact (extruder, variant) match wins; otherwise
    /// the first row belonging to that extruder (normally its Standard row). For
    /// [`VariantTable::Filament`] the extruder is ignored (filament rows are per variant only)
    /// and the fallback is row 0. Returns `None` only when the preset has no variant table at all
    /// (single-extruder presets): callers should then use index 0.
    pub fn variant_index(&self, table: VariantTable, extruder: usize, variant: &str) -> Option<usize> {
        let variants = self.get_strs(table.variant_key())?;
        let ids: Option<Vec<i64>> = table.id_key().map(|k| {
            (0..variants.len()).map(|i| self.get_i64_at(k, i).unwrap_or(1)).collect()
        });
        let want_id = extruder as i64 + 1;
        let matches_id = |i: usize| ids.as_ref().is_none_or(|ids| ids[i] == want_id);
        if let Some(i) = (0..variants.len()).find(|&i| variants[i] == variant && matches_id(i)) {
            return Some(i);
        }
        if let Some(i) = (0..variants.len()).find(|&i| matches_id(i)) {
            return Some(i);
        }
        Some(0)
    }

    /// [`Config::variant_index`] with `0` as the default for presets without a variant table.
    pub fn variant_index_or_zero(&self, table: VariantTable, extruder: usize, variant: &str) -> usize {
        self.variant_index(table, extruder, variant).unwrap_or(0)
    }
}

/// Which variant table of a preset to consult; see the module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VariantTable {
    /// Machine preset: `printer_extruder_id` + `printer_extruder_variant`.
    Printer,
    /// Process preset: `print_extruder_id` + `print_extruder_variant`.
    Process,
    /// Filament preset: `filament_extruder_variant` (no extruder ids).
    Filament,
}

impl VariantTable {
    pub fn id_key(self) -> Option<&'static str> {
        match self {
            VariantTable::Printer => Some("printer_extruder_id"),
            VariantTable::Process => Some("print_extruder_id"),
            VariantTable::Filament => None,
        }
    }

    pub fn variant_key(self) -> &'static str {
        match self {
            VariantTable::Printer => "printer_extruder_variant",
            VariantTable::Process => "print_extruder_variant",
            VariantTable::Filament => "filament_extruder_variant",
        }
    }
}

/// The default extruder variant of Bambu printers.
pub const DEFAULT_VARIANT: &str = "Direct Drive Standard";

/// `"Direct Drive High Flow"` -> `"High Flow"` (the nozzle *volume type* Bambu exposes to G-code
/// templates as `nozzle_volume_types`).
pub fn volume_type_of_variant(variant: &str) -> &str {
    variant
        .strip_prefix("Direct Drive ")
        .or_else(|| variant.strip_prefix("Bowden "))
        .unwrap_or(variant)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Config {
        let mut c = Config::new();
        c.insert("printer_extruder_id", vec!["1", "1", "1", "2", "2", "2", "2"]);
        c.insert(
            "printer_extruder_variant",
            vec![
                "Direct Drive Standard",
                "Direct Drive High Flow",
                "Direct Drive E3D High Flow",
                "Direct Drive Standard",
                "Direct Drive High Flow",
                "Direct Drive TPU High Flow",
                "Direct Drive E3D High Flow",
            ],
        );
        c.insert("retraction_length", vec!["0.8", "0.8", "0.8", "0.6", "0.8", "0.8", "0.8"]);
        c.insert("sparse_infill_density", "15%");
        c.insert("filament_z_hop", vec!["nil", "0.4"]);
        c.insert("enable", "1");
        c
    }

    #[test]
    fn typed_getters() {
        let c = sample();
        assert_eq!(c.get_f64("retraction_length"), Some(0.8));
        assert_eq!(c.get_f64_at("retraction_length", 3), Some(0.6));
        assert_eq!(c.get_f64_at("retraction_length", 99), Some(0.8));
        assert_eq!(c.get_f64("sparse_infill_density"), None);
        assert_eq!(c.get_percent_or_abs("sparse_infill_density", 100.0), Some(15.0));
        assert_eq!(c.get_percent_or_abs_at("retraction_length", 3, 5.0), Some(0.6));
        assert_eq!(c.get_f64_at("filament_z_hop", 0), None);
        assert!(c.is_nil_at("filament_z_hop", 0));
        assert_eq!(c.get_f64_at("filament_z_hop", 1), Some(0.4));
        assert_eq!(c.get_bool("enable"), Some(true));
        assert_eq!(c.get_i64_at("printer_extruder_id", 4), Some(2));
    }

    #[test]
    fn variant_lookup() {
        let c = sample();
        assert_eq!(c.variant_index(VariantTable::Printer, 0, "Direct Drive Standard"), Some(0));
        assert_eq!(c.variant_index(VariantTable::Printer, 1, "Direct Drive Standard"), Some(3));
        assert_eq!(c.variant_index(VariantTable::Printer, 1, "Direct Drive TPU High Flow"), Some(5));
        // Left extruder has no TPU High Flow row: falls back to its first row.
        assert_eq!(c.variant_index(VariantTable::Printer, 0, "Direct Drive TPU High Flow"), Some(0));
        assert_eq!(c.variant_index(VariantTable::Process, 0, "x"), None);
    }

    #[test]
    fn merge_keeps_order() {
        let mut a: Config = [("a", "1"), ("b", "2")].into_iter().collect();
        let b: Config = [("b", "3"), ("c", "4")].into_iter().collect();
        a.merge(&b);
        assert_eq!(a.keys().collect::<Vec<_>>(), vec!["a", "b", "c"]);
        assert_eq!(a.get_str("b"), Some("3"));
        a.remove("a");
        assert_eq!(a.get_str("c"), Some("4"));
        assert_eq!(a.keys().collect::<Vec<_>>(), vec!["b", "c"]);
    }
}
