//! Report merging and the typed, tolerant [`PrinterStatus`] view.
//!
//! Printers publish `{"print": {"command": "push_status", ...}}` on
//! `device/{serial}/report`: one full snapshot after `pushall`, then partial
//! deltas. [`deep_merge`] folds deltas into a cached JSON document;
//! [`PrinterStatus::from_state`] reads that document without ever panicking
//! on missing, null, stringly-typed or odd fields.
//!
//! Field knowledge comes from the H2D/X1C payloads documented in
//! Doridian/OpenBambuAPI (`mqtt.md`) and greghesp/ha-bambulab
//! (`pybambu/models.py`, mock data `MOCK-H2D.json`).

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

// ------------------------------------------------------------------ merge

/// Deep-merge `src` into `dst`.
///
/// * objects merge key by key, recursively;
/// * non-empty arrays whose elements are all objects with an `"id"` key are
///   merged element-wise by id (Bambu deltas send only the changed AMS unit /
///   tray / extruder, e.g. `{"ams":{"ams":[{"id":"1","tray":[{"id":"2","remain":40}]}]}}`);
///   unmatched ids are appended. `"0"` and `0` are the same id;
/// * everything else (scalars, other arrays, empty arrays such as `hms: []`)
///   replaces the old value.
pub fn deep_merge(dst: &mut Value, src: &Value) {
    match (dst, src) {
        (Value::Object(d), Value::Object(s)) => {
            for (k, v) in s {
                match d.get_mut(k) {
                    Some(existing) => deep_merge(existing, v),
                    None => {
                        d.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (Value::Array(d), Value::Array(s)) if is_id_array(s) && is_id_array(d) => {
            for item in s {
                let id = id_key(item);
                match d.iter_mut().find(|e| id_key(e) == id) {
                    Some(existing) => deep_merge(existing, item),
                    None => d.push(item.clone()),
                }
            }
        }
        (dst, src) => *dst = src.clone(),
    }
}

fn is_id_array(a: &[Value]) -> bool {
    !a.is_empty() && a.iter().all(|e| id_key(e).is_some())
}

fn id_key(v: &Value) -> Option<String> {
    match v.get("id")? {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

// ------------------------------------------------------------------ tolerant readers

fn path<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().try_fold(v, |acc, k| acc.get(*k))
}

/// Number or numeric string (`"27.0"`, `"-46dBm"` → -46).
fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let t = s.trim().trim_end_matches("dBm").trim_end_matches('%').trim();
            t.parse::<f64>().ok()
        }
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        _ => None,
    }
    .filter(|f| f.is_finite())
}

fn int(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.round() as i64)),
        Value::String(s) => {
            let t = s.trim();
            t.parse::<i64>()
                .ok()
                .or_else(|| t.parse::<f64>().ok().filter(|f| f.is_finite()).map(|f| f.round() as i64))
        }
        Value::Bool(b) => Some(i64::from(*b)),
        _ => None,
    }
}

/// Hex string (`"2103"`) or plain integer.
fn hex_int(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::String(s) => i64::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok(),
        other => int(Some(other)),
    }
}

fn text(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn boolish(v: Option<&Value>) -> Option<bool> {
    match v? {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_f64().map(|f| f != 0.0),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "on" | "1" | "yes" | "has_sdcard_normal" | "normal" => Some(true),
            "false" | "off" | "0" | "no" | "no_sdcard" | "none" => Some(false),
            other if other.starts_with("has_sdcard") => Some(true),
            _ => None,
        },
        _ => None,
    }
}

fn clamp_u8(v: Option<i64>) -> Option<u8> {
    v.map(|x| x.clamp(0, 255) as u8)
}

fn clamp_u32(v: Option<i64>) -> Option<u32> {
    v.filter(|x| *x >= 0).map(|x| x.min(u32::MAX as i64) as u32)
}

/// Newer firmware packs two 16-bit values into one integer:
/// low word = current, high word = target (ha-bambulab models.py, "low word
/// is current, high word is target"; e.g. 16056565 = 245 << 16 | 245).
pub fn unpack_temp(packed: i64) -> (f64, f64) {
    let p = packed as u64;
    ((p & 0xFFFF) as f64, ((p >> 16) & 0xFFFF) as f64)
}

/// Bambu fans report 0..=15; ha-bambulab rounds up to 10 % steps.
pub fn fan_percent(raw: i64) -> u8 {
    if raw <= 0 {
        return 0;
    }
    let pct = (raw.min(15) as f64 / 15.0) * 100.0;
    ((pct / 10.0).ceil() * 10.0).min(100.0) as u8
}

// ------------------------------------------------------------------ types

/// `print.gcode_state`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum GcodeState {
    /// Not reported yet.
    #[default]
    Unknown,
    Idle,
    Prepare,
    Slicing,
    Running,
    Pause,
    Finish,
    Failed,
    /// Any other value, verbatim.
    Other(String),
}

impl GcodeState {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_uppercase().as_str() {
            "" => GcodeState::Unknown,
            "IDLE" => GcodeState::Idle,
            "PREPARE" => GcodeState::Prepare,
            "SLICING" => GcodeState::Slicing,
            "RUNNING" => GcodeState::Running,
            "PAUSE" | "PAUSED" => GcodeState::Pause,
            "FINISH" | "FINISHED" => GcodeState::Finish,
            "FAILED" | "FAIL" => GcodeState::Failed,
            _ => GcodeState::Other(s.trim().to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            GcodeState::Unknown => "UNKNOWN",
            GcodeState::Idle => "IDLE",
            GcodeState::Prepare => "PREPARE",
            GcodeState::Slicing => "SLICING",
            GcodeState::Running => "RUNNING",
            GcodeState::Pause => "PAUSE",
            GcodeState::Finish => "FINISH",
            GcodeState::Failed => "FAILED",
            GcodeState::Other(s) => s,
        }
    }

    /// A job is preparing, running or paused: a new print would be refused.
    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            GcodeState::Prepare | GcodeState::Slicing | GcodeState::Running | GcodeState::Pause
        )
    }
}

impl fmt::Display for GcodeState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for GcodeState {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for GcodeState {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(if s.eq_ignore_ascii_case("UNKNOWN") {
            GcodeState::Unknown
        } else {
            GcodeState::parse(&s)
        })
    }
}

/// Current/target pair in °C.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Temperature {
    pub current: Option<f64>,
    pub target: Option<f64>,
}

/// An AMS slot reference. `ams_id` 0..=3 (AMS/AMS 2 Pro), 128.. (AMS HT),
/// 254/255 = external spool holders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrayRef {
    pub ams_id: u32,
    pub slot_id: u32,
}

impl TrayRef {
    /// Decode the `ams << 8 | slot` encoding used by `extruder.info[].snow`.
    /// `None` for "nothing loaded" (slot 0xFF or the 0xFFFF sentinel).
    pub fn from_snow(v: i64) -> Option<Self> {
        if !(0..=0xFFFF).contains(&v) || v == 0xFFFF {
            return None;
        }
        let ams_id = ((v >> 8) & 0xFF) as u32;
        let slot_id = (v & 0xFF) as u32;
        if slot_id == 0xFF {
            return None;
        }
        Some(TrayRef { ams_id, slot_id })
    }

    pub fn is_external(&self) -> bool {
        self.ams_id >= 254
    }

    /// This slot's bit in the AMS-wide `tray_exist_bits` / `tray_reading_bits` masks: `4 × unit
    /// + slot` for 4-slot units (ids 0-3), `16 + unit − 128` for an AMS HT (BambuStudio
    /// `DevAms::GetTrayId`). `None` for the external holders.
    pub fn mask_bit(&self) -> Option<u32> {
        match self.ams_id {
            0..=3 if self.slot_id < 4 => Some(self.ams_id * 4 + self.slot_id),
            128..=135 => Some(16 + self.ams_id - 128),
            _ => None,
        }
    }

    /// Flat "global tray id" used by `ams_mapping`: `ams*4 + slot` for 4-slot
    /// units, the unit id itself for AMS HT (128+) and external spools.
    pub fn global_id(&self) -> u32 {
        if self.ams_id >= 128 {
            self.ams_id
        } else {
            self.ams_id * 4 + self.slot_id
        }
    }
}

/// One nozzle / extruder.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NozzleStatus {
    /// Extruder index as reported (H2D: 0 = right/main, 1 = left/deputy).
    pub index: u8,
    /// `"right"`/`"left"` on dual-nozzle printers, `None` on single-nozzle ones.
    pub side: Option<String>,
    pub temperature: Temperature,
    pub diameter_mm: Option<f64>,
    /// Raw type code (`HS01`) or legacy name (`hardened_steel`).
    pub nozzle_type: Option<String>,
    /// Decoded material from the type code, e.g. `hardened_steel`, `high_flow_hardened_steel`.
    pub nozzle_material: Option<String>,
    /// Tray currently loaded into this extruder (H2D `extruder.info[].snow`).
    pub loaded_tray: Option<TrayRef>,
    pub active: bool,
}

/// Fan speeds in percent (rounded up to 10 % like the printer UI).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct FanStatus {
    pub part_cooling: Option<u8>,
    pub aux: Option<u8>,
    pub chamber: Option<u8>,
    pub heatbreak: Option<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LightStatus {
    pub node: String,
    pub mode: String,
}

/// One filament slot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AmsTray {
    pub ams_id: u32,
    pub slot_id: u32,
    /// Flat id as used by `ams_mapping`.
    pub global_id: u32,
    /// A spool is present (has a filament type, or the exist bit says so).
    pub loaded: bool,
    pub filament_type: Option<String>,
    pub sub_brand: Option<String>,
    /// `RRGGBBAA` hex.
    pub color: Option<String>,
    /// Bambu filament preset id (`GFA00`).
    pub tray_info_idx: Option<String>,
    /// Remaining filament %, `None` if unknown (printer reports -1).
    pub remain_percent: Option<u8>,
    pub nozzle_temp_min: Option<u32>,
    pub nozzle_temp_max: Option<u32>,
    /// The AMS is reading this slot (RFID, filament) right now (`tray_reading_bits`).
    #[serde(default)]
    pub reading: bool,
}

/// One AMS unit.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AmsUnit {
    pub id: u32,
    /// The name the printer's screen uses: "A".."D", "HT-A"...
    #[serde(default)]
    pub name: String,
    /// "AMS", "AMS Lite", "AMS 2 Pro", "AMS HT" (`info` bits 0-3).
    pub model: Option<String>,
    /// Filament drying, on units that dry (AMS 2 Pro, AMS HT).
    pub drying: Option<AmsDrying>,
    /// Humidity level 1..=5 as shown on the printer.
    pub humidity_level: Option<u8>,
    /// Relative humidity % (AMS 2 Pro / HT `humidity_raw`).
    pub humidity_percent: Option<u8>,
    pub temperature: Option<f64>,
    /// Which extruder this unit feeds on dual-nozzle printers
    /// (`(info >> 8) & 0xF`, 0 = right, 1 = left; ha-bambulab models.py).
    pub extruder: Option<u8>,
    pub trays: Vec<AmsTray>,
}

/// An AMS unit's filament drying, decoded as BambuStudio does (`DevFilaSystem.cpp`): the state
/// and phase from the unit's `info` bits 4-7 and 22-23, the job from `dry_setting`, the time
/// left from `dry_time` (minutes) and what stops it from starting from `dry_sf_reason`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AmsDrying {
    /// off, checking, drying, cooling, stopping, error, heater_fault, factory_test.
    pub state: String,
    /// While drying: "heating" or "dehumidifying".
    pub phase: Option<String>,
    /// The filament the job is set for ("PLA").
    pub filament: Option<String>,
    /// Target temperature (°C).
    pub temperature_c: Option<u32>,
    /// Length of the job (hours).
    pub duration_hours: Option<u32>,
    pub remaining_minutes: Option<u32>,
    /// Why drying can't start right now (empty when it can).
    #[serde(default)]
    pub blocked_by: Vec<String>,
    /// One line for people: "drying PLA at 50 °C (dehumidifying), 10 h 37 min left of 12 h".
    #[serde(default)]
    pub summary: String,
}

impl AmsDrying {
    /// Drying, or getting ready to (checking) or winding down (cooling, stopping).
    pub fn is_active(&self) -> bool {
        matches!(self.state.as_str(), "checking" | "drying" | "cooling" | "stopping")
    }

    fn describe(&self) -> String {
        let job = [self.filament.clone(), self.temperature_c.map(|c| format!("at {c} °C"))].into_iter().flatten().collect::<Vec<_>>().join(" ");
        let job = if job.is_empty() { String::new() } else { format!(" {job}") };
        let left = match (self.remaining_minutes, self.duration_hours) {
            (Some(m), Some(h)) => format!(", {} left of {h} h", minutes_text(m)),
            (Some(m), None) => format!(", {} left", minutes_text(m)),
            (None, Some(h)) => format!(", {h} h"),
            (None, None) => String::new(),
        };
        match self.state.as_str() {
            "drying" => format!("drying{job}{}{left}", self.phase.as_deref().map(|p| format!(" ({p})")).unwrap_or_default()),
            "checking" => format!("starting to dry{job}{left}"),
            "cooling" => "cooling down after drying".into(),
            "stopping" => "stopping drying".into(),
            "error" => "drying stopped with an error: see the printer's screen".into(),
            "heater_fault" => "drying heater fault: see the printer's screen".into(),
            "factory_test" => "running a factory drying test".into(),
            _ if !self.blocked_by.is_empty() => format!("can't dry now: {}", self.blocked_by.join("; ")),
            _ => "not drying".into(),
        }
    }
}

/// "10 h 37 min", "37 min".
pub fn minutes_text(m: u32) -> String {
    match (m / 60, m % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// What the AMS as a whole is doing, and its RFID settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AmsState {
    /// "changing filament", "reading RFID", "calibrating"... (`ams_status` high byte); `None`
    /// when idle.
    pub activity: Option<String>,
    /// The AMS setting "Insertion update": read a Bambu spool's RFID tag when it is inserted.
    pub read_rfid_on_insert: Option<bool>,
    /// The AMS setting "Power on update": read every spool when the AMS powers on.
    pub read_rfid_on_power_on: Option<bool>,
}

/// Health Management System entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HmsError {
    pub attr: u32,
    pub code: u32,
    /// `AAAA_AAAA_CCCC_CCCC`, the form used in Bambu's HMS wiki.
    pub id: String,
    /// fatal / serious / common / info / unknown.
    pub severity: String,
    /// mc / mainboard / ams / toolhead / xcam / unknown.
    pub module: String,
}

impl HmsError {
    pub fn new(attr: u32, code: u32) -> Self {
        let severity = match code >> 16 {
            1 => "fatal",
            2 => "serious",
            3 => "common",
            4 => "info",
            _ => "unknown",
        };
        let module = match attr >> 24 {
            0x03 => "mc",
            0x05 => "mainboard",
            0x07 => "ams",
            0x08 => "toolhead",
            0x0C => "xcam",
            _ => "unknown",
        };
        HmsError {
            attr,
            code,
            id: format!(
                "{:04X}_{:04X}_{:04X}_{:04X}",
                attr >> 16,
                attr & 0xFFFF,
                code >> 16,
                code & 0xFFFF
            ),
            severity: severity.into(),
            module: module.into(),
        }
    }
}

/// Non-zero `print_error`, e.g. `0x05004004` → `0500_4004` ("device busy").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrintError {
    pub code: u32,
    pub id: String,
}

/// Typed snapshot of the printer state. Every field is optional/defaulted.
/// Name of a `stg_cur` stage, from Bambu Studio's `DeviceManager` stage list (as tabulated by
/// bambuddy, AGPL-3.0). `None` for "no stage" (-1 / 255) and unknown ids.
pub fn stage_name(id: i64) -> Option<&'static str> {
    Some(match id {
        0 => "Printing",
        1 => "Auto bed leveling",
        2 => "Heatbed preheating",
        3 => "Vibration compensation",
        4 => "Changing filament",
        5 => "M400 pause",
        6 => "Paused (filament ran out)",
        7 => "Heating nozzle",
        8 => "Calibrating dynamic flow",
        9 => "Scanning bed surface",
        10 => "Inspecting first layer",
        11 => "Identifying build plate type",
        12 => "Calibrating Micro Lidar",
        13 => "Homing toolhead",
        14 => "Cleaning nozzle tip",
        15 => "Checking extruder temperature",
        16 => "Paused by the user",
        17 => "Paused (front cover fell off)",
        18 => "Calibrating the micro lidar",
        19 => "Calibrating flow ratio",
        20 => "Paused (nozzle temperature malfunction)",
        21 => "Paused (heatbed temperature malfunction)",
        22 => "Unloading filament",
        23 => "Paused (step loss)",
        24 => "Loading filament",
        25 => "Motor noise cancellation",
        26 => "Paused (AMS offline)",
        27 => "Paused (low heatbreak fan speed)",
        28 => "Paused (chamber temperature control problem)",
        29 => "Cooling chamber",
        30 => "Paused (G-code inserted by user)",
        31 => "Motor noise showoff",
        32 => "Paused (nozzle clumping)",
        33 => "Paused (cutter error)",
        34 => "Paused (first layer error)",
        35 => "Paused (nozzle clog)",
        36 => "Measuring motion precision",
        37 => "Enhancing motion precision",
        38 => "Measuring motion accuracy",
        39 => "Nozzle offset calibration",
        40 => "High-temperature auto bed leveling",
        41 => "Auto check: quick release lever",
        42 => "Auto check: door and upper cover",
        43 => "Laser calibration",
        44 => "Auto check: platform",
        45 => "Confirming BirdsEye camera location",
        46 => "Calibrating BirdsEye camera",
        47 => "Auto bed leveling, phase 1",
        48 => "Auto bed leveling, phase 2",
        49 => "Heating chamber",
        50 => "Cooling heatbed",
        51 => "Printing calibration lines",
        52 => "Auto check: material",
        53 => "Live view camera calibration",
        54 => "Waiting for heatbed temperature",
        55 => "Auto check: material position",
        56 => "Cutting module offset calibration",
        57 => "Measuring surface",
        58 => "Thermal preconditioning",
        59 => "Homing blade holder",
        60 => "Calibrating camera offset",
        61 => "Calibrating blade holder position",
        62 => "Hotend pick-and-place test",
        63 => "Waiting for chamber temperature",
        64 => "Preparing hotend",
        65 => "Calibrating nozzle clumping detection",
        66 => "Purifying the chamber air",
        74 => "Preparing",
        77 => "Preparing AMS",
        _ => return None,
    })
}

/// `device.airduct` (Bambu Studio `DevFan.cpp`): the duct mode and its fans and doors.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AirDuct {
    /// `modeCur`: 0 cooling + filter, 1 heating / internal filter, 2 exhaust, 3 full cooling.
    pub mode: Option<i64>,
    pub mode_name: Option<String>,
    pub parts: Vec<AirPart>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AirPart {
    /// Bambu `AIR_FUN` index (`id` bits 4..12).
    pub id: u32,
    /// "fan" or "door" (`id` bits 0..4).
    pub kind: String,
    pub name: String,
    /// Speed or opening, 0..100 (`state` bits 0..8).
    pub percent: u8,
}

impl AirDuct {
    fn from_json(v: &Value) -> Self {
        let mode = int(v.get("modeCur"));
        let parts = v
            .get("parts")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|p| {
                        let raw = int(p.get("id"))? as u32;
                        let (kind, id) = (raw & 0xF, (raw >> 4) & 0xFF);
                        let percent = (int(p.get("state")).unwrap_or(0) as u32 & 0xFF).min(100) as u8;
                        let name = match (kind, id) {
                            (1, 0) => "chamber air door",
                            (1, 1) => "inner-loop air door",
                            (1, 2) => "top air door",
                            (_, 0) => "heatbreak fan (right)",
                            (_, 1) => "part cooling fan",
                            (_, 2) => "auxiliary fan",
                            (_, 3) => "chamber exhaust fan",
                            (_, 4) => "heatbreak fan (left)",
                            (_, 5) => "mainboard fan",
                            (_, 6) => "air purifier (inner-loop fan)",
                            (_, 10) => "left auxiliary fan",
                            _ => "",
                        };
                        Some(AirPart { id, kind: if kind == 1 { "door" } else { "fan" }.into(), name: if name.is_empty() { format!("air part {id}") } else { name.to_string() }, percent })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mode_name = mode.and_then(|m| match m {
            0 => Some("cooling (filtered)"),
            1 => Some("heating / internal filtration"),
            2 => Some("exhaust"),
            3 => Some("full cooling"),
            _ => None,
        });
        AirDuct { mode, mode_name: mode_name.map(str::to_string), parts }
    }

    /// Fans and doors currently running or open.
    pub fn active(&self) -> impl Iterator<Item = &AirPart> {
        self.parts.iter().filter(|p| p.percent > 0)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PrinterStatus {
    pub gcode_state: GcodeState,
    pub progress_percent: Option<u8>,
    pub remaining_minutes: Option<u32>,
    pub layer: Option<u32>,
    pub total_layers: Option<u32>,
    pub subtask_name: Option<String>,
    pub gcode_file: Option<String>,
    pub print_error: Option<PrintError>,
    /// `stg_cur` stage id (see ha-bambulab `CURRENT_STAGE_IDS`).
    pub stage: Option<i64>,
    /// What the printer is doing in that stage, e.g. "Purifying the chamber air".
    pub stage_name: Option<String>,
    /// Air duct (H2/X2/P2 series): mode and the fans/doors it drives.
    pub air: Option<AirDuct>,
    pub bed: Temperature,
    pub chamber: Option<Temperature>,
    pub nozzles: Vec<NozzleStatus>,
    pub active_extruder: Option<u8>,
    pub fans: FanStatus,
    /// 1 silent, 2 standard, 3 sport, 4 ludicrous.
    pub speed_level: Option<u8>,
    pub speed_percent: Option<u32>,
    pub lights: Vec<LightStatus>,
    pub chamber_light: Option<bool>,
    pub ams: Vec<AmsUnit>,
    /// AMS-wide state, when an AMS is attached.
    pub ams_state: Option<AmsState>,
    /// External spool holder(s): one (`vt_tray`, id 255) or two on H2D (`vir_slot`, 254/255).
    pub external_spools: Vec<AmsTray>,
    pub hms: Vec<HmsError>,
    pub wifi_signal_dbm: Option<i32>,
    pub sdcard: Option<bool>,
    /// `info.module[name=ota].sw_ver` from `get_version`.
    pub firmware_version: Option<String>,
    /// `info.module[name=ota].product_name`, e.g. "Bambu Lab H2D".
    pub product_name: Option<String>,
}

impl PrinterStatus {
    /// Parse from the merged state document (`{"print": {...}, "info": {...}}`)
    /// or from a bare `print` object.
    pub fn from_state(state: &Value) -> Self {
        let print = state.get("print").unwrap_or(state);
        let mut st = PrinterStatus {
            gcode_state: print
                .get("gcode_state")
                .and_then(Value::as_str)
                .map(GcodeState::parse)
                .unwrap_or_default(),
            progress_percent: clamp_u8(int(print.get("mc_percent")).or_else(|| int(print.get("percent")))),
            remaining_minutes: clamp_u32(
                int(print.get("mc_remaining_time")).or_else(|| int(print.get("remain_time"))),
            ),
            layer: clamp_u32(int(print.get("layer_num")).or_else(|| int(path(print, &["3D", "layer_num"])))),
            total_layers: clamp_u32(
                int(print.get("total_layer_num")).or_else(|| int(path(print, &["3D", "total_layer_num"]))),
            ),
            subtask_name: text(print.get("subtask_name")),
            gcode_file: text(print.get("gcode_file")),
            print_error: int(print.get("print_error"))
                .filter(|c| *c != 0)
                .map(|c| {
                    let c = c as u32;
                    PrintError {
                        code: c,
                        id: format!("{:04X}_{:04X}", c >> 16, c & 0xFFFF),
                    }
                }),
            stage: int(print.get("stg_cur")),
            stage_name: int(print.get("stg_cur")).and_then(stage_name).map(str::to_string),
            air: print.get("device").and_then(|d| d.get("airduct")).map(AirDuct::from_json),
            speed_level: clamp_u8(int(print.get("spd_lvl"))),
            speed_percent: clamp_u32(int(print.get("spd_mag"))),
            wifi_signal_dbm: num(print.get("wifi_signal")).map(|f| f.round() as i32),
            sdcard: boolish(print.get("sdcard")),
            ..Default::default()
        };

        st.bed = parse_bed(print);
        st.chamber = parse_chamber(print);
        parse_nozzles(print, &mut st);
        st.fans = FanStatus {
            part_cooling: int(print.get("cooling_fan_speed")).map(fan_percent),
            aux: int(print.get("big_fan1_speed")).map(fan_percent),
            chamber: int(print.get("big_fan2_speed")).map(fan_percent),
            heatbreak: int(print.get("heatbreak_fan_speed")).map(fan_percent),
        };
        if let Some(Value::Array(lights)) = print.get("lights_report") {
            st.lights = lights
                .iter()
                .filter_map(|l| {
                    Some(LightStatus {
                        node: text(l.get("node"))?,
                        mode: text(l.get("mode")).unwrap_or_default(),
                    })
                })
                .collect();
            st.chamber_light = st
                .lights
                .iter()
                .find(|l| l.node == "chamber_light")
                .map(|l| l.mode != "off");
        }
        parse_ams(print, &mut st);
        if let Some(Value::Array(hms)) = print.get("hms") {
            st.hms = hms
                .iter()
                .filter_map(|h| {
                    let attr = int(h.get("attr"))?;
                    let code = int(h.get("code"))?;
                    Some(HmsError::new(attr as u32, code as u32))
                })
                .collect();
        }
        if let Some(Value::Array(modules)) = path(state, &["info", "module"]) {
            if let Some(ota) = modules.iter().find(|m| m.get("name").and_then(Value::as_str) == Some("ota")) {
                st.firmware_version = text(ota.get("sw_ver"));
                st.product_name = text(ota.get("product_name"));
            }
        }
        st
    }

    /// Nozzle by extruder index.
    pub fn nozzle(&self, index: u8) -> Option<&NozzleStatus> {
        self.nozzles.iter().find(|n| n.index == index)
    }

    /// All trays across AMS units and external spools.
    pub fn trays(&self) -> impl Iterator<Item = &AmsTray> {
        self.ams.iter().flat_map(|u| u.trays.iter()).chain(self.external_spools.iter())
    }
}

fn parse_bed(print: &Value) -> Temperature {
    // New firmware: device.bed.info.temp packed (also device.bed_temp).
    if let Some(packed) = int(path(print, &["device", "bed", "info", "temp"])).or_else(|| int(path(print, &["device", "bed_temp"]))) {
        let (c, t) = unpack_temp(packed);
        return Temperature {
            current: Some(c),
            target: Some(t),
        };
    }
    Temperature {
        current: num(print.get("bed_temper")),
        target: num(print.get("bed_target_temper")).map(f64::floor),
    }
}

fn parse_chamber(print: &Value) -> Option<Temperature> {
    if let Some(packed) = int(path(print, &["device", "ctc", "info", "temp"])) {
        let (c, t) = unpack_temp(packed);
        return Some(Temperature {
            current: Some(c),
            target: (t > 0.0).then_some(t),
        });
    }
    if let Some(c) = num(path(print, &["device", "cham_temp"])).or_else(|| num(print.get("chamber_temper"))) {
        return Some(Temperature {
            current: Some(c),
            target: None,
        });
    }
    None
}

/// Decode a nozzle type code like `HS01` (ha-bambulab `_nozzle_type_name`):
/// 2nd char H/E = high flow, U = TPU high flow; chars 3..4 = material.
pub fn nozzle_material(type_code: &str) -> Option<String> {
    let c = type_code.trim();
    if c.contains('_') {
        // Legacy firmware already reports a name ("hardened_steel").
        return Some(c.to_string());
    }
    let chars: Vec<char> = c.chars().collect();
    if chars.len() < 4 {
        return None;
    }
    if chars[1] == 'U' {
        return Some("tpu_high_flow".into());
    }
    let flow = if matches!(chars[1], 'H' | 'E') { "high_flow_" } else { "" };
    let material: String = chars[2..4].iter().collect();
    let material = match material.as_str() {
        "00" => "stainless_steel",
        "01" => "hardened_steel",
        "05" => "tungsten_carbide",
        _ => "unknown",
    };
    Some(format!("{flow}{material}"))
}

fn parse_nozzles(print: &Value, st: &mut PrinterStatus) {
    let ext_state = int(path(print, &["device", "extruder", "state"]));
    let ext_info = path(print, &["device", "extruder", "info"]).and_then(Value::as_array);
    let nozzle_info = path(print, &["device", "nozzle", "info"]).and_then(Value::as_array);

    let mut nozzles: Vec<NozzleStatus> = Vec::new();
    if let Some(entries) = ext_info.filter(|e| !e.is_empty()) {
        // H2D: device.extruder.info[] with packed temps (ha-bambulab models.py).
        for e in entries {
            let Some(idx) = int(e.get("id")).filter(|i| (0..=15).contains(i)) else {
                continue;
            };
            let temperature = match int(e.get("temp")) {
                Some(p) => {
                    let (c, t) = unpack_temp(p);
                    Temperature {
                        current: Some(c),
                        target: Some(t),
                    }
                }
                None => Temperature::default(),
            };
            nozzles.push(NozzleStatus {
                index: idx as u8,
                temperature,
                loaded_tray: int(e.get("snow")).and_then(TrayRef::from_snow),
                ..Default::default()
            });
        }
        nozzles.sort_by_key(|n| n.index);
    } else {
        nozzles.push(NozzleStatus {
            index: 0,
            temperature: Temperature {
                current: num(print.get("nozzle_temper")),
                target: num(print.get("nozzle_target_temper")),
            },
            ..Default::default()
        });
    }

    // Diameter / type: device.nozzle.info[] on new firmware, else top-level.
    if let Some(entries) = nozzle_info {
        for e in entries {
            let Some(idx) = int(e.get("id")) else { continue };
            if let Some(n) = nozzles.iter_mut().find(|n| i64::from(n.index) == idx) {
                n.diameter_mm = num(e.get("diameter"));
                n.nozzle_type = text(e.get("type"));
            }
        }
    }
    if let Some(n0) = nozzles.iter_mut().find(|n| n.index == 0) {
        if n0.diameter_mm.is_none() {
            n0.diameter_mm = num(print.get("nozzle_diameter"));
        }
        if n0.nozzle_type.is_none() {
            n0.nozzle_type = text(print.get("nozzle_type"));
        }
    }

    // device.extruder.state: low nibble = extruder count, next nibble = active
    // extruder (15 while switching) — ha-bambulab models.py `Extruder`.
    st.active_extruder = match ext_state {
        Some(s) => {
            let a = ((s >> 4) & 0xF) as u8;
            (a != 0xF).then_some(a)
        }
        None if nozzles.len() == 1 => Some(0),
        None => None,
    };
    let dual = nozzles.len() >= 2;
    for n in &mut nozzles {
        n.nozzle_material = n.nozzle_type.as_deref().and_then(nozzle_material);
        n.side = dual.then(|| if n.index == 0 { "right" } else { "left" }.to_string());
        n.active = st.active_extruder == Some(n.index);
    }
    st.nozzles = nozzles;
}

fn parse_tray(ams_id: u32, t: &Value, exist_bits: Option<i64>) -> Option<AmsTray> {
    let slot_id = clamp_u32(int(t.get("id")))?;
    let filament_type = text(t.get("tray_type"));
    let tref = TrayRef { ams_id, slot_id };
    let exists_bit = mask_has(exist_bits, tref);
    Some(AmsTray {
        ams_id,
        slot_id,
        global_id: tref.global_id(),
        // The AMS's presence sensor wins: tray fields merge across reports, so a slot emptied by
        // a partial report can still carry its old filament type.
        loaded: exists_bit.unwrap_or(filament_type.is_some()),
        filament_type,
        sub_brand: text(t.get("tray_sub_brands")),
        color: text(t.get("tray_color")).or_else(|| {
            t.get("cols")
                .and_then(Value::as_array)
                .and_then(|c| text(c.first()))
        }),
        tray_info_idx: text(t.get("tray_info_idx")),
        remain_percent: int(t.get("remain")).filter(|r| (0..=100).contains(r)).map(|r| r as u8),
        nozzle_temp_min: clamp_u32(int(t.get("nozzle_temp_min"))).filter(|v| *v > 0),
        nozzle_temp_max: clamp_u32(int(t.get("nozzle_temp_max"))).filter(|v| *v > 0),
        reading: false,
    })
}

/// Whether a slot's bit is set in an AMS-wide mask; `None` when the mask or the bit is unknown.
fn mask_has(mask: Option<i64>, t: TrayRef) -> Option<bool> {
    Some((mask? >> t.mask_bit()?) & 1 == 1)
}

/// "A".."Z" for AMS units 0..25, "HT-A".. for AMS HT units (128+), as on the printer's screen.
fn ams_unit_name(id: u32) -> String {
    match id {
        0..=25 => ((b'A' + id as u8) as char).to_string(),
        128..=153 => format!("HT-{}", (b'A' + (id - 128) as u8) as char),
        other => other.to_string(),
    }
}

/// AMS model from `info` bits 0-3 (BambuStudio `DevAmsType`).
fn ams_model(kind: i64) -> Option<&'static str> {
    match kind {
        1 => Some("AMS"),
        2 | 5 => Some("AMS Lite"),
        3 => Some("AMS 2 Pro"),
        4 => Some("AMS HT"),
        _ => None,
    }
}

/// What the AMS is doing, from `ams_status`'s high byte (BambuStudio `AmsStatusMain`). Idle and
/// "assist" (reported whenever filament is loaded) are `None`.
fn ams_activity(main: i64) -> Option<&'static str> {
    match main {
        0x01 => Some("changing filament"),
        0x02 => Some("reading RFID"),
        0x04 => Some("calibrating"),
        0x07 => Some("cold pull"),
        0x08 => Some("switching hotend"),
        0x10 => Some("self-check"),
        0x20 => Some("debug"),
        _ => None,
    }
}

/// Why an AMS can't start drying (`dry_sf_reason`, BambuStudio `CannotDryReason`).
fn dry_blocker(code: i64) -> Option<&'static str> {
    match code {
        0 => Some("the printer is busy"),
        1 => Some("too many AMS units are drying at once"),
        2 => Some("the AMS is busy (calibrating, reading RFID, loading or unloading)"),
        3 => Some("filament is in the AMS outlet: unload it first"),
        4 => Some("drying is starting"),
        5 => Some("not available in 2D mode"),
        // "drying in progress": BambuStudio ignores it on its own, and it is set while drying
        6 => None,
        7 => Some("a firmware update is in progress"),
        8 => Some("not enough power: plug in the AMS's power adapter"),
        9 => Some("drying is stopping"),
        10 => Some("filament is in the AMS outlet: unload it by hand"),
        _ => Some("the printer gave an unknown reason"),
    }
}

/// Drying on units that report it (AMS 2 Pro, AMS HT); see [`AmsDrying`].
fn parse_drying(u: &Value, info: Option<i64>) -> Option<AmsDrying> {
    let setting = u.get("dry_setting");
    if setting.is_none() && u.get("dry_time").is_none() {
        return None;
    }
    let status = info.map(|i| (i >> 4) & 0xF).unwrap_or(0);
    let active = (1..=4).contains(&status);
    let positive = |v: Option<&Value>| clamp_u32(int(v)).filter(|x| *x > 0);
    let job = |key: &str| setting.and_then(|s| s.get(key)).filter(|_| active);
    let mut d = AmsDrying {
        state: match status {
            0 => "off",
            1 => "checking",
            2 => "drying",
            3 => "cooling",
            4 => "stopping",
            5 => "error",
            6 => "heater_fault",
            7 => "factory_test",
            _ => "unknown",
        }
        .into(),
        phase: match info.map(|i| (i >> 22) & 0x3) {
            Some(1) if status == 2 => Some("heating".into()),
            Some(2) if status == 2 => Some("dehumidifying".into()),
            _ => None,
        },
        filament: text(job("dry_filament")),
        temperature_c: positive(job("dry_temperature")),
        duration_hours: positive(job("dry_duration")),
        // BambuStudio hides the time left once the unit is cooling down
        remaining_minutes: positive(u.get("dry_time")).filter(|_| matches!(status, 1 | 2 | 4)),
        blocked_by: if active {
            Vec::new()
        } else {
            u.get("dry_sf_reason")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|c| int(Some(c)).and_then(dry_blocker)).map(String::from).collect())
                .unwrap_or_default()
        },
        summary: String::new(),
    };
    d.summary = d.describe();
    Some(d)
}

fn parse_ams(print: &Value, st: &mut PrinterStatus) {
    let ams_root = print.get("ams");
    let exist_bits = hex_int(ams_root.and_then(|a| a.get("tray_exist_bits")));
    let reading_bits = hex_int(ams_root.and_then(|a| a.get("tray_reading_bits")));
    if let Some(units) = ams_root.and_then(|a| a.get("ams")).and_then(Value::as_array) {
        for u in units {
            let Some(id) = clamp_u32(int(u.get("id"))) else { continue };
            // `info` bits (BambuStudio DevFilaSystem.cpp ParseAmsInfo): 0-3 model, 4-7 drying
            // state, 8-11 the extruder it feeds, 22-23 the drying phase
            let info = hex_int(u.get("info"));
            let extruder = info.map(|info| ((info >> 8) & 0xF) as u8).filter(|e| *e < 0xE);
            let mut trays: Vec<AmsTray> = u
                .get("tray")
                .and_then(Value::as_array)
                .map(|ts| ts.iter().filter_map(|t| parse_tray(id, t, exist_bits)).collect())
                .unwrap_or_default();
            for t in &mut trays {
                t.reading = mask_has(reading_bits, TrayRef { ams_id: id, slot_id: t.slot_id }) == Some(true);
            }
            st.ams.push(AmsUnit {
                id,
                name: ams_unit_name(id),
                model: info.and_then(|i| ams_model(i & 0xF)).map(String::from),
                drying: parse_drying(u, info),
                humidity_level: clamp_u8(int(u.get("humidity"))).filter(|h| (1..=5).contains(h)),
                humidity_percent: clamp_u8(int(u.get("humidity_raw"))).filter(|h| (1..=100).contains(h)),
                temperature: num(u.get("temp")).filter(|t| *t > 0.0),
                extruder,
                trays,
            });
        }
        st.ams.sort_by_key(|u| u.id);
    }
    if !st.ams.is_empty() {
        st.ams_state = Some(AmsState {
            activity: int(print.get("ams_status")).and_then(|s| ams_activity((s >> 8) & 0xFF)).map(String::from),
            read_rfid_on_insert: ams_root.and_then(|a| boolish(a.get("insert_flag"))),
            read_rfid_on_power_on: ams_root.and_then(|a| boolish(a.get("power_on_flag"))),
        });
    }
    // External spools: H2D reports both holders in `vir_slot` (254 = left /
    // deputy, 255 = right / main), older printers one `vt_tray` (255).
    let ext: Vec<&Value> = match (print.get("vir_slot"), print.get("vt_tray")) {
        (Some(Value::Array(a)), _) if !a.is_empty() => a.iter().collect(),
        (_, Some(v @ Value::Object(_))) => vec![v],
        _ => Vec::new(),
    };
    for t in ext {
        let id = clamp_u32(int(t.get("id"))).unwrap_or(255);
        if let Some(mut tray) = parse_tray(id, t, None) {
            tray.slot_id = 0;
            tray.global_id = id;
            st.external_spools.push(tray);
        }
    }
}

/// Merge-able view used by [`crate::PrinterClient`]: holds the raw merged
/// document and decides which incoming messages belong in it.
#[derive(Debug, Default, Clone)]
pub struct StateCache {
    raw: Value,
}

impl StateCache {
    pub fn new() -> Self {
        StateCache {
            raw: Value::Object(Map::new()),
        }
    }

    pub fn raw(&self) -> &Value {
        &self.raw
    }

    /// Fold one report message in. Returns `true` if it was state (and the
    /// cache changed), `false` for command replies, which are not merged so
    /// that e.g. a `project_file` echo does not pollute `print.url`.
    pub fn apply(&mut self, msg: &Value) -> bool {
        let mut changed = false;
        if let Some(print) = msg.get("print") {
            let cmd = print.get("command").and_then(Value::as_str);
            if matches!(cmd, None | Some("push_status")) {
                let before = self.raw.get("print").cloned();
                let slot = self
                    .raw
                    .as_object_mut()
                    .map(|o| o.entry("print").or_insert_with(|| Value::Object(Map::new())));
                if let Some(slot) = slot {
                    deep_merge(slot, print);
                }
                changed |= before.as_ref() != self.raw.get("print");
            }
        }
        if let Some(info) = msg.get("info") {
            if info.get("command").and_then(Value::as_str) == Some("get_version") && info.get("module").is_some() {
                if let Some(o) = self.raw.as_object_mut() {
                    // The module list is always complete: replace, don't merge.
                    changed |= o.get("info") != Some(info);
                    o.insert("info".into(), info.clone());
                }
            }
        }
        changed
    }

    pub fn has_print_state(&self) -> bool {
        self.raw.get("print").is_some()
    }
}

#[cfg(test)]
mod tests {
    /// An H2D's AMS 2 Pro (captured 2026-11-20): `info`, drying fields and masks as reported.
    fn ams_report(info: &str, dry: serde_json::Value, extra: serde_json::Value) -> super::PrinterStatus {
        let mut unit = serde_json::json!({"id": "0", "info": info, "humidity": "1", "humidity_raw": "27", "temp": "50.0", "tray": [
            {"id": "0", "state": 10, "tray_type": "PLA", "tray_color": "995F11FF"},
            {"id": "1", "state": 0},
            {"id": "2", "state": 10},
            {"id": "3", "state": 11, "tray_type": "PETG", "tray_color": "F9C1BD80", "remain": 87}]});
        for (k, v) in dry.as_object().unwrap() {
            unit[k] = v.clone();
        }
        let mut print = serde_json::json!({"ams_status": 0, "ams": {"ams": [unit], "tray_exist_bits": "8", "tray_reading_bits": "4", "insert_flag": true, "power_on_flag": false}});
        for (k, v) in extra.as_object().unwrap() {
            print[k] = v.clone();
        }
        super::PrinterStatus::from_state(&serde_json::json!({ "print": print }))
    }

    #[test]
    fn decodes_ams_2_pro_drying() {
        let st = ams_report("50941023", serde_json::json!({"dry_setting": {"dry_duration": 12, "dry_filament": "PLA", "dry_temperature": 50}, "dry_sf_reason": [6], "dry_time": 637}), serde_json::json!({}));
        let a = &st.ams[0];
        assert_eq!((a.name.as_str(), a.model.as_deref(), a.extruder, a.humidity_percent), ("A", Some("AMS 2 Pro"), Some(0), Some(27)));
        let d = a.drying.as_ref().unwrap();
        assert_eq!((d.state.as_str(), d.phase.as_deref(), d.filament.as_deref()), ("drying", Some("dehumidifying"), Some("PLA")));
        assert_eq!((d.temperature_c, d.duration_hours, d.remaining_minutes), (Some(50), Some(12), Some(637)));
        assert!(d.blocked_by.is_empty(), "code 6 is set while drying and means nothing on its own");
        assert_eq!(d.summary, "drying PLA at 50 °C (dehumidifying), 10 h 37 min left of 12 h");
        assert!(d.is_active());
    }

    #[test]
    fn decodes_an_idle_ams_and_why_it_cant_dry() {
        // idle, with the settings the printer reports when no job is set
        let st = ams_report("50001003", serde_json::json!({"dry_setting": {"dry_duration": -1, "dry_filament": "", "dry_temperature": -1}, "dry_sf_reason": [], "dry_time": 0}), serde_json::json!({}));
        let d = st.ams[0].drying.as_ref().unwrap();
        assert_eq!((d.state.as_str(), d.phase.as_deref(), d.filament.as_deref(), d.remaining_minutes), ("off", None, None, None));
        assert_eq!(d.summary, "not drying");
        // filament left in the outlet blocks drying; a job's old settings don't show once it's off
        let st = ams_report("50001003", serde_json::json!({"dry_setting": {"dry_duration": 12, "dry_filament": "PLA", "dry_temperature": 50}, "dry_sf_reason": [3, 6], "dry_time": 0}), serde_json::json!({}));
        let d = st.ams[0].drying.as_ref().unwrap();
        assert_eq!(d.filament, None);
        assert_eq!(d.summary, "can't dry now: filament is in the AMS outlet: unload it first");
        // an AMS that doesn't dry reports no drying fields
        let st = ams_report("50001001", serde_json::json!({}), serde_json::json!({}));
        assert_eq!((st.ams[0].model.as_deref(), st.ams[0].drying.is_none()), (Some("AMS"), true));
    }

    #[test]
    fn ams_slots_follow_the_presence_and_reading_masks() {
        let st = ams_report("50001003", serde_json::json!({}), serde_json::json!({"ams_status": 0x0200}));
        let t = &st.ams[0].trays;
        // A1 still carries its old filament after a partial report, but the AMS says it's empty
        assert_eq!(t[0].filament_type.as_deref(), Some("PLA"));
        assert!(!t[0].loaded);
        assert!(t[2].reading && !t[0].reading && !t[3].reading);
        assert!(t[3].loaded);
        let s = st.ams_state.as_ref().unwrap();
        assert_eq!((s.activity.as_deref(), s.read_rfid_on_insert, s.read_rfid_on_power_on), (Some("reading RFID"), Some(true), Some(false)));
        // AMS HT units use bits 16 + (id − 128)
        assert_eq!(super::TrayRef { ams_id: 129, slot_id: 0 }.mask_bit(), Some(17));
        assert_eq!(super::TrayRef { ams_id: 1, slot_id: 2 }.mask_bit(), Some(6));
        assert_eq!(super::TrayRef { ams_id: 254, slot_id: 0 }.mask_bit(), None);
        // no AMS, no AMS state
        assert!(super::PrinterStatus::from_state(&serde_json::json!({"print": {"gcode_state": "IDLE"}})).ams_state.is_none());
    }

    #[test]
    fn decodes_h2d_air_purification_after_a_print() {
        // captured from an H2D at the end of a PETG print (2026-11-15)
        let v = serde_json::json!({"print": {"gcode_state": "RUNNING", "mc_percent": 100, "stg_cur": 66,
            "device": {"airduct": {"modeCur": 1, "parts": [
                {"func": 0, "id": 16, "range": 6553600, "state": 0, "tar_state": 0},
                {"func": 1, "id": 32, "range": 6553600, "state": 0, "tar_state": 0},
                {"func": 2, "id": 48, "range": 6553600, "state": 0, "tar_state": 0},
                {"func": 4, "id": 96, "range": 6553600, "state": 100, "tar_state": 100}]}}}});
        let st = PrinterStatus::from_state(&v);
        assert_eq!(st.stage_name.as_deref(), Some("Purifying the chamber air"));
        let air = st.air.unwrap();
        assert_eq!(air.mode_name.as_deref(), Some("heating / internal filtration"));
        let on: Vec<_> = air.active().map(|p| (p.name.as_str(), p.percent)).collect();
        assert_eq!(on, vec![("air purifier (inner-loop fan)", 100)]);
        assert_eq!(stage_name(-1), None);
    }

    use super::*;
    use serde_json::json;

    #[test]
    fn merge_objects_and_scalars() {
        let mut a = json!({"print": {"bed_temper": 20.0, "gcode_state": "IDLE", "nested": {"x": 1, "y": 2}}});
        deep_merge(&mut a, &json!({"print": {"bed_temper": 55.5, "nested": {"y": 3, "z": 4}}}));
        assert_eq!(
            a,
            json!({"print": {"bed_temper": 55.5, "gcode_state": "IDLE", "nested": {"x": 1, "y": 3, "z": 4}}})
        );
    }

    #[test]
    fn merge_id_arrays_and_replace_others() {
        let mut a = json!({
            "ams": [{"id": "0", "tray": [{"id": "0", "remain": 90, "tray_type": "PLA"}, {"id": "1", "remain": 50}]},
                    {"id": "1", "tray": []}],
            "hms": [{"attr": 1, "code": 2}],
            "s_obj": [1, 2, 3]
        });
        deep_merge(
            &mut a,
            &json!({
                "ams": [{"id": 0, "tray": [{"id": "1", "remain": 40}]}, {"id": "2", "tray": [{"id": "0"}]}],
                "hms": [],
                "s_obj": [9]
            }),
        );
        assert_eq!(a["ams"][0]["tray"][0]["remain"], 90);
        assert_eq!(a["ams"][0]["tray"][0]["tray_type"], "PLA");
        assert_eq!(a["ams"][0]["tray"][1]["remain"], 40);
        assert_eq!(a["ams"].as_array().unwrap().len(), 3);
        assert_eq!(a["hms"], json!([]));
        assert_eq!(a["s_obj"], json!([9]));
    }

    #[test]
    fn merge_type_change_replaces() {
        let mut a = json!({"x": {"a": 1}});
        deep_merge(&mut a, &json!({"x": 5}));
        assert_eq!(a, json!({"x": 5}));
        let mut b = json!({"x": 5});
        deep_merge(&mut b, &json!({"x": {"a": 1}}));
        assert_eq!(b, json!({"x": {"a": 1}}));
    }

    #[test]
    fn packed_temperatures() {
        assert_eq!(unpack_temp(16_056_565), (245.0, 245.0));
        assert_eq!(unpack_temp(4_587_590), (70.0, 70.0));
        assert_eq!(unpack_temp(47), (47.0, 0.0));
        assert_eq!(unpack_temp(2_621_480), (40.0, 40.0));
    }

    #[test]
    fn fan_and_snow() {
        assert_eq!(fan_percent(0), 0);
        assert_eq!(fan_percent(15), 100);
        assert_eq!(fan_percent(4), 30);
        assert_eq!(fan_percent(99), 100);
        assert_eq!(TrayRef::from_snow(259), Some(TrayRef { ams_id: 1, slot_id: 3 }));
        assert_eq!(TrayRef::from_snow(65279), None);
        assert_eq!(TrayRef::from_snow(65535), None);
        let ext = TrayRef::from_snow(65280).unwrap();
        assert!(ext.is_external());
        assert_eq!(ext.global_id(), 255);
        assert_eq!(TrayRef { ams_id: 1, slot_id: 2 }.global_id(), 6);
    }

    #[test]
    fn hms_and_error_ids() {
        let h = HmsError::new(0x0300_0100, 0x0001_0007);
        assert_eq!(h.id, "0300_0100_0001_0007");
        assert_eq!(h.severity, "fatal");
        assert_eq!(h.module, "mc");
        let st = PrinterStatus::from_state(&json!({"print_error": 0x0500_4004}));
        assert_eq!(st.print_error.unwrap().id, "0500_4004");
    }

    #[test]
    fn nozzle_type_codes() {
        assert_eq!(nozzle_material("HS01").as_deref(), Some("hardened_steel"));
        assert_eq!(nozzle_material("HH01").as_deref(), Some("high_flow_hardened_steel"));
        assert_eq!(nozzle_material("HS00").as_deref(), Some("stainless_steel"));
        assert_eq!(nozzle_material("stainless_steel").as_deref(), Some("stainless_steel"));
        assert_eq!(nozzle_material("x"), None);
    }

    #[test]
    fn garbage_never_panics() {
        for v in [
            json!(null),
            json!(42),
            json!("str"),
            json!([1, 2]),
            json!({"print": null}),
            json!({"print": {"device": {"extruder": {"info": [null, 3, {"id": "x"}, {"id": 99, "temp": "hot"}], "state": "?"}}}}),
            json!({"print": {"ams": {"ams": [{"id": -1}, {"id": "0", "tray": [{"id": null}, {}], "info": "zz"}]}}}),
            json!({"print": {"hms": [{"attr": "a"}], "lights_report": [1], "wifi_signal": [], "mc_percent": -5, "layer_num": 1e30}}),
            json!({"print": {"vt_tray": [], "vir_slot": {"id": 1}, "nozzle_temper": "NaN", "spd_lvl": 1000}}),
        ] {
            let st = PrinterStatus::from_state(&v);
            let _ = serde_json::to_string(&st).unwrap();
        }
    }

    #[test]
    fn state_cache_ignores_command_replies() {
        let mut c = StateCache::new();
        assert!(c.apply(&json!({"print": {"command": "push_status", "gcode_state": "IDLE"}})));
        assert!(!c.apply(&json!({"print": {"command": "project_file", "url": "ftp://x", "result": "SUCCESS"}})));
        assert!(c.raw()["print"].get("url").is_none());
        assert!(!c.apply(&json!({"print": {"command": "push_status", "gcode_state": "IDLE"}})));
        assert!(c.apply(&json!({"info": {"command": "get_version", "module": [{"name": "ota", "sw_ver": "01.02.03.04"}]}})));
        let st = PrinterStatus::from_state(c.raw());
        assert_eq!(st.firmware_version.as_deref(), Some("01.02.03.04"));
        assert_eq!(st.gcode_state, GcodeState::Idle);
    }

    #[test]
    fn gcode_state_serde() {
        let s = serde_json::to_string(&GcodeState::Running).unwrap();
        assert_eq!(s, "\"RUNNING\"");
        let back: GcodeState = serde_json::from_str("\"weird\"").unwrap();
        assert_eq!(back, GcodeState::Other("weird".into()));
        assert!(GcodeState::parse("pause").is_busy());
        assert!(!GcodeState::Finish.is_busy());
    }
}
