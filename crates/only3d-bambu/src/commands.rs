//! Typed MQTT commands and the `project_file` (start print) payload.
//!
//! Payload shapes follow Doridian/OpenBambuAPI `mqtt.md`, greghesp/ha-bambulab
//! `pybambu/commands.py` + `utils.py`, and maziggy/bambuddy
//! `backend/app/services/bambu_mqtt.py` (`start_print`, validated against
//! Bambu Studio request-topic captures from H2D / H2D Pro / H2S / H2C).

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::models::ModelInfo;
use crate::mqtt::QoS;

/// Light nodes known to exist (`system.ledctrl.led_node`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LightNode {
    ChamberLight,
    /// Second chamber light (H2D family).
    ChamberLight2,
    WorkLight,
    /// Bed light (H2 family).
    HeatbedLight,
}

impl LightNode {
    pub fn as_str(self) -> &'static str {
        match self {
            LightNode::ChamberLight => "chamber_light",
            LightNode::ChamberLight2 => "chamber_light2",
            LightNode::WorkLight => "work_light",
            LightNode::HeatbedLight => "heatbed_light",
        }
    }
}

/// `print.print_speed` levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeedLevel {
    Silent = 1,
    Standard = 2,
    Sport = 3,
    Ludicrous = 4,
}

impl SpeedLevel {
    pub fn from_level(n: u8) -> Option<Self> {
        Some(match n {
            1 => SpeedLevel::Silent,
            2 => SpeedLevel::Standard,
            3 => SpeedLevel::Sport,
            4 => SpeedLevel::Ludicrous,
            _ => return None,
        })
    }
}

/// A command for `device/{serial}/request`.
///
/// Start-print is deliberately *not* a variant: use [`crate::start_print`]
/// so the busy-check and ack handling always run.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// `pushing.pushall`: ask for a full state snapshot.
    PushAll,
    /// `info.get_version`: firmware/module versions.
    GetVersion,
    Pause,
    Resume,
    Stop,
    /// `system.ledctrl` on/off.
    Light { node: LightNode, on: bool },
    PrintSpeed(SpeedLevel),
    /// **Dangerous**: raw G-code, executed immediately by the motion
    /// controller with no validation. Can move axes into obstacles, heat
    /// hotends or crash a running print. Multiple lines separated by `\n`.
    GcodeLine(String),
    /// Bed target °C via `M140 S<t>` (G-code under the hood).
    SetBedTemp(u16),
    /// Nozzle target °C via `M104 S<t> [T<extruder>]`; `extruder` selects the
    /// H2D nozzle (0 = right, 1 = left), `None` = active nozzle.
    SetNozzleTemp { celsius: u16, extruder: Option<u8> },
    /// Escape hatch: any JSON object `{"<type>": {"command": ..., ...}}`.
    /// `sequence_id` is filled in if absent.
    Raw(Value),
}

/// Max nozzle temperature accepted by the typed command (H2D range is 0..=350).
pub const MAX_NOZZLE_C: u16 = 350;
/// Max bed temperature accepted by the typed command (H2D range is 0..=120).
pub const MAX_BED_C: u16 = 120;

impl Command {
    /// QoS used on the wire: control commands use QoS 1 like Bambu Studio
    /// ("sent with QoS of 1 for higher priority", OpenBambuAPI mqtt.md).
    pub fn qos(&self) -> QoS {
        match self {
            Command::PushAll | Command::GetVersion => QoS::AtMostOnce,
            _ => QoS::AtLeastOnce,
        }
    }

    /// Whether this command makes the printer run G-code.
    pub fn is_gcode(&self) -> bool {
        matches!(
            self,
            Command::GcodeLine(_) | Command::SetBedTemp(_) | Command::SetNozzleTemp { .. }
        )
    }

    /// Build the JSON payload with the given sequence id.
    pub fn to_payload(&self, sequence_id: &str) -> Result<Value> {
        let seq = sequence_id;
        let gcode = |param: String| json!({"print": {"sequence_id": seq, "command": "gcode_line", "param": param}});
        Ok(match self {
            Command::PushAll => json!({"pushing": {"sequence_id": seq, "command": "pushall"}}),
            Command::GetVersion => json!({"info": {"sequence_id": seq, "command": "get_version"}}),
            Command::Pause => json!({"print": {"sequence_id": seq, "command": "pause", "param": ""}}),
            Command::Resume => json!({"print": {"sequence_id": seq, "command": "resume", "param": ""}}),
            Command::Stop => json!({"print": {"sequence_id": seq, "command": "stop", "param": ""}}),
            Command::Light { node, on } => json!({"system": {
                "sequence_id": seq,
                "command": "ledctrl",
                "led_node": node.as_str(),
                "led_mode": if *on { "on" } else { "off" },
                // Required even for on/off (OpenBambuAPI mqtt.md system.ledctrl).
                "led_on_time": 500,
                "led_off_time": 500,
                "loop_times": 0,
                "interval_time": 0
            }}),
            Command::PrintSpeed(level) => json!({"print": {
                "sequence_id": seq, "command": "print_speed", "param": (*level as u8).to_string()
            }}),
            Command::GcodeLine(g) => {
                if g.trim().is_empty() {
                    return Err(Error::invalid_input("empty G-code"));
                }
                let mut g = g.clone();
                if !g.ends_with('\n') {
                    g.push('\n');
                }
                gcode(g)
            }
            Command::SetBedTemp(t) => {
                if *t > MAX_BED_C {
                    return Err(Error::invalid_input(format!("bed temperature {t} > {MAX_BED_C}")));
                }
                gcode(format!("M140 S{t}\n"))
            }
            Command::SetNozzleTemp { celsius, extruder } => {
                if *celsius > MAX_NOZZLE_C {
                    return Err(Error::invalid_input(format!(
                        "nozzle temperature {celsius} > {MAX_NOZZLE_C}"
                    )));
                }
                match extruder {
                    // "M104 S220 T1" — tool index selects the extruder
                    // (synman/bambu-printer-manager mqtt-protocol-reference).
                    Some(e) if *e > 1 => {
                        return Err(Error::invalid_input(format!("extruder {e} (H2D has 0 and 1)")))
                    }
                    Some(e) => gcode(format!("M104 S{celsius} T{e}\n")),
                    None => gcode(format!("M104 S{celsius}\n")),
                }
            }
            Command::Raw(v) => {
                let mut v = v.clone();
                let obj = v
                    .as_object_mut()
                    .filter(|o| o.len() == 1)
                    .ok_or_else(|| Error::invalid_input("raw command must be an object with exactly one key, e.g. {\"print\": {...}}"))?;
                let inner = obj
                    .values_mut()
                    .next()
                    .and_then(Value::as_object_mut)
                    .ok_or_else(|| Error::invalid_input("raw command body must be an object"))?;
                if !inner.contains_key("command") {
                    return Err(Error::invalid_input("raw command body needs a \"command\" field"));
                }
                if inner.get("command").and_then(Value::as_str) == Some("project_file") {
                    return Err(Error::invalid_input(
                        "use start_print() for project_file so busy checks and acks run",
                    ));
                }
                inner
                    .entry("sequence_id")
                    .or_insert_with(|| Value::String(seq.to_string()));
                v
            }
        })
    }
}

/// Off / force / auto, encoded like Bambu Studio's `getValueInt`
/// (0 = never, 1 = every print, 2 = only if needed). Source: bambuddy
/// `start_print` (`_tristate_wire`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Calibration {
    Off,
    On,
    #[default]
    Auto,
}

impl Calibration {
    fn wire(self) -> i64 {
        match self {
            Calibration::Off => 0,
            Calibration::On => 1,
            Calibration::Auto => 2,
        }
    }
}

impl From<bool> for Calibration {
    fn from(b: bool) -> Self {
        if b {
            Calibration::On
        } else {
            Calibration::Off
        }
    }
}

/// How `project_file.url` names the uploaded file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UrlStyle {
    /// `file:///sdcard/<name>` for legacy models (X1/P1/A1), `ftp://<name>`
    /// otherwise, including H2D and unknown models.
    #[default]
    Auto,
    /// `ftp://<name>` (bambuddy, H2D-tested).
    Ftp,
    /// `file:///sdcard/<name>` (ha-bambulab `LEGACY_SDCARD_PRINTERS`).
    SdCard,
}

/// A print job for a `.gcode.3mf` already uploaded to the printer root.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrintJob {
    /// File name on the printer (as passed to `upload_file`), e.g. `part.gcode.3mf`.
    pub remote_name: String,
    /// 1-based plate number inside the 3MF (`Metadata/plate_<n>.gcode`).
    pub plate: u32,
    /// Display name; defaults to the file stem.
    pub subtask_name: Option<String>,
    /// Feed filament from the AMS. Always sent as a JSON bool: H2D Pro
    /// firmware reads an integer `use_ams` as a nozzle index (bambuddy #1386).
    pub use_ams: bool,
    /// Flat AMS mapping, one entry per filament in the 3MF (in order):
    /// global tray id = `ams_id * 4 + slot` (AMS HT: its unit id 128+;
    /// external spool: 254 = left/deputy, 255 = right/main on H2D; -1 = unused).
    pub ams_mapping: Vec<i32>,
    /// Bed type; `"auto"` lets the printer use the plate it detects.
    pub bed_type: String,
    pub bed_leveling: Calibration,
    /// Flow dynamics (pressure advance) calibration.
    pub flow_cali: Calibration,
    /// Nozzle offset calibration; only sent non-zero on dual-nozzle printers.
    pub nozzle_offset_cali: Calibration,
    pub vibration_cali: bool,
    pub layer_inspect: bool,
    pub timelapse: bool,
    /// MD5 of the uploaded file (hex). Sent as `""` when `None`, which every
    /// firmware accepts as "skip check" (bambuddy).
    pub md5: Option<String>,
    pub url_style: UrlStyle,
    /// Optional per-filament nozzle mapping (H2C rack); omitted when `None`.
    pub nozzle_mapping: Option<Vec<i32>>,
    /// Extra fields merged into the payload last (escape hatch).
    pub extra: Map<String, Value>,
}

impl Default for PrintJob {
    fn default() -> Self {
        PrintJob {
            remote_name: String::new(),
            plate: 1,
            subtask_name: None,
            use_ams: true,
            ams_mapping: vec![0],
            bed_type: "auto".into(),
            bed_leveling: Calibration::On,
            flow_cali: Calibration::Off,
            nozzle_offset_cali: Calibration::Auto,
            vibration_cali: false,
            layer_inspect: false,
            timelapse: false,
            md5: None,
            url_style: UrlStyle::Auto,
            nozzle_mapping: None,
            extra: Map::new(),
        }
    }
}

/// Reject names that could break the URL/FTP command or escape the root.
pub(crate) fn validate_remote_name(name: &str) -> Result<()> {
    let bad = |why: &str| Err(Error::invalid_input(format!("remote file name {name:?}: {why}")));
    if name.trim().is_empty() {
        return bad("empty");
    }
    if name.len() > 200 {
        return bad("longer than 200 bytes");
    }
    if name.chars().any(|c| c.is_control()) {
        return bad("contains control characters");
    }
    if name.contains('/') || name.contains('\\') {
        return bad("must be a plain file name in the storage root (no '/' or '\\')");
    }
    if name == "." || name == ".." {
        return bad("reserved name");
    }
    if name.contains('#') || name.contains('?') {
        return bad("'#' and '?' break the ftp:// URL");
    }
    Ok(())
}

fn file_stem(name: &str) -> &str {
    name.strip_suffix(".gcode.3mf")
        .or_else(|| name.strip_suffix(".3mf"))
        .or_else(|| name.strip_suffix(".gcode"))
        .unwrap_or(name)
}

/// Per-submission id: Bambu Studio mints fresh task ids; P1S firmware clamps
/// them to i32, so stay below 2^31-1 and never 0 (bambuddy #1011/#1042).
pub(crate) fn submission_id() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(1);
    let id = (ms % 2_147_483_647) as u64;
    (if id == 0 { 1 } else { id }).to_string()
}

impl PrintJob {
    /// Job for `remote_name` with the defaults (plate 1, AMS slot 0, bed
    /// leveling on, flow calibration off, no timelapse, bed type auto).
    pub fn new(remote_name: impl Into<String>) -> Self {
        PrintJob {
            remote_name: remote_name.into(),
            ..Default::default()
        }
    }

    pub fn url(&self, model: Option<&ModelInfo>) -> String {
        let name = &self.remote_name;
        let sdcard = match self.url_style {
            UrlStyle::Ftp => false,
            UrlStyle::SdCard => true,
            UrlStyle::Auto => model.is_some_and(|m| m.legacy_sdcard_url),
        };
        if sdcard {
            format!("file:///sdcard/{name}")
        } else {
            format!("ftp://{name}")
        }
    }

    /// Build the full `project_file` request.
    ///
    /// `model` decides the URL form and the dual-nozzle fields; `None` is
    /// treated as an unknown single-nozzle model with the `ftp://` URL.
    /// `task_id` is a fresh per-submission id (what `start_print` generates).
    pub fn to_payload(&self, sequence_id: &str, model: Option<&ModelInfo>, task_id: &str) -> Result<Value> {
        validate_remote_name(&self.remote_name)?;
        if self.plate == 0 || self.plate > 999 {
            return Err(Error::invalid_input(format!("plate {} (must be 1..=999)", self.plate)));
        }
        if let Some(md5) = &self.md5 {
            if md5.len() != 32 || !md5.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(Error::invalid_input(format!("md5 {md5:?} is not 32 hex digits")));
            }
        }
        let dual = model.is_some_and(|m| m.is_dual_nozzle());

        // Flat mapping + `ams_mapping2` [{ams_id, slot_id}] exactly as
        // bambuddy builds it from Bambu Studio captures:
        //  * -1           → flat -1, {255, 255}
        //  * 254/255 ext. → flat -1 (raw 254/255 makes H2D fail with
        //                   0700_8012 "Failed to get AMS mapping table"),
        //                   {ams_id: 254|255 on dual-nozzle, else 255, slot 0}
        //  * 128..=253    → AMS HT: flat id, {id, 0}
        //  * 0..=127      → flat id, {id / 4, id % 4}
        let mut flat = Vec::with_capacity(self.ams_mapping.len());
        let mut detail = Vec::with_capacity(self.ams_mapping.len());
        for &t in &self.ams_mapping {
            match t {
                -1 => {
                    flat.push(-1);
                    detail.push(json!({"ams_id": 255, "slot_id": 255}));
                }
                254 | 255 => {
                    flat.push(-1);
                    detail.push(json!({"ams_id": if dual { t } else { 255 }, "slot_id": 0}));
                }
                128..=253 => {
                    flat.push(t);
                    detail.push(json!({"ams_id": t, "slot_id": 0}));
                }
                0..=127 => {
                    flat.push(t);
                    detail.push(json!({"ams_id": t / 4, "slot_id": t % 4}));
                }
                other => {
                    return Err(Error::invalid_input(format!(
                        "ams_mapping entry {other} (use -1, 0..=253, 254 or 255)"
                    )))
                }
            }
        }
        // Single-nozzle printers without AMS reject use_ams=true for an
        // external-spool-only job (07FF_8012); dual-nozzle firmware routes by
        // ams_mapping2 instead, so leave it alone there (bambuddy).
        let only_external = !dual
            && self.ams_mapping.iter().any(|t| *t >= 254)
            && self.ams_mapping.iter().all(|t| *t >= 254 || *t == -1);
        let use_ams = self.use_ams && !only_external;

        let subtask = self
            .subtask_name
            .clone()
            .unwrap_or_else(|| file_stem(&self.remote_name).to_string());
        let mut p = json!({
            "sequence_id": sequence_id,
            "command": "project_file",
            "param": format!("Metadata/plate_{}.gcode", self.plate),
            "url": self.url(model),
            // bambuddy sends the name here as well; OpenBambuAPI notes `file`
            // is unused when `url` is set.
            "file": self.remote_name,
            "md5": self.md5.as_deref().unwrap_or(""),
            "bed_type": self.bed_type,
            "timelapse": self.timelapse,
            // Spelled `bed_leveling` by ha-bambulab, bambuddy and Bambu Studio
            // (OpenBambuAPI's `bed_levelling` is a doc typo). Bool is true only
            // for "on"; the int carries the tri-state.
            "bed_leveling": self.bed_leveling == Calibration::On,
            "auto_bed_leveling": self.bed_leveling.wire(),
            "flow_cali": self.flow_cali == Calibration::On,
            "extrude_cali_flag": self.flow_cali.wire(),
            "extrude_cali_manual_mode": 0,
            "nozzle_offset_cali": if dual { self.nozzle_offset_cali.wire() } else { 0 },
            "vibration_cali": self.vibration_cali,
            "layer_inspect": self.layer_inspect,
            "use_ams": use_ams,
            "ams_mapping": flat,
            "ams_mapping2": detail,
            "subtask_name": subtask,
            "profile_id": "0",
            "project_id": task_id,
            "subtask_id": task_id,
            "task_id": task_id,
        });
        let obj = p.as_object_mut().expect("object literal");
        if let Some(nm) = &self.nozzle_mapping {
            obj.insert("nozzle_mapping".into(), json!(nm));
        }
        for (k, v) in &self.extra {
            obj.insert(k.clone(), v.clone());
        }
        Ok(json!({ "print": p }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::lookup;

    #[test]
    fn simple_commands() {
        assert_eq!(
            Command::PushAll.to_payload("0").unwrap(),
            json!({"pushing": {"sequence_id": "0", "command": "pushall"}})
        );
        assert_eq!(Command::Stop.to_payload("5").unwrap()["print"]["command"], "stop");
        let l = Command::Light { node: LightNode::ChamberLight, on: false }.to_payload("1").unwrap();
        assert_eq!(l["system"]["led_mode"], "off");
        assert_eq!(l["system"]["led_node"], "chamber_light");
        assert_eq!(Command::PrintSpeed(SpeedLevel::Sport).to_payload("1").unwrap()["print"]["param"], "3");
        assert_eq!(Command::PushAll.qos(), QoS::AtMostOnce);
        assert_eq!(Command::Pause.qos(), QoS::AtLeastOnce);
    }

    #[test]
    fn gcode_commands_serialize_only() {
        let p = Command::SetNozzleTemp { celsius: 220, extruder: Some(1) }.to_payload("2").unwrap();
        assert_eq!(p["print"]["param"], "M104 S220 T1\n");
        assert_eq!(Command::SetBedTemp(60).to_payload("2").unwrap()["print"]["param"], "M140 S60\n");
        assert!(Command::SetBedTemp(500).to_payload("2").is_err());
        assert!(Command::SetNozzleTemp { celsius: 200, extruder: Some(2) }.to_payload("2").is_err());
        assert!(Command::GcodeLine("  ".into()).to_payload("1").is_err());
        assert!(Command::GcodeLine("G28".into()).is_gcode());
    }

    #[test]
    fn raw_escape_hatch() {
        let p = Command::Raw(json!({"print": {"command": "print_option", "sound_enable": true}}))
            .to_payload("9")
            .unwrap();
        assert_eq!(p["print"]["sequence_id"], "9");
        assert!(Command::Raw(json!({"print": {}})).to_payload("1").is_err());
        assert!(Command::Raw(json!([1])).to_payload("1").is_err());
        assert!(Command::Raw(json!({"print": {"command": "project_file"}})).to_payload("1").is_err());
    }

    #[test]
    fn h2d_project_file_payload() {
        let h2d = lookup("O1D");
        let mut job = PrintJob::new("bracket v2.gcode.3mf");
        job.ams_mapping = vec![0, 5, 255, -1, 254];
        let p = job.to_payload("42", h2d, "777").unwrap();
        let pr = &p["print"];
        assert_eq!(pr["command"], "project_file");
        assert_eq!(pr["param"], "Metadata/plate_1.gcode");
        assert_eq!(pr["url"], "ftp://bracket v2.gcode.3mf");
        assert_eq!(pr["subtask_name"], "bracket v2");
        assert_eq!(pr["use_ams"], json!(true));
        assert_eq!(pr["bed_leveling"], json!(true));
        assert_eq!(pr["auto_bed_leveling"], 1);
        assert_eq!(pr["flow_cali"], json!(false));
        assert_eq!(pr["extrude_cali_flag"], 0);
        assert_eq!(pr["nozzle_offset_cali"], 2);
        assert_eq!(pr["ams_mapping"], json!([0, 5, -1, -1, -1]));
        assert_eq!(
            pr["ams_mapping2"],
            json!([
                {"ams_id": 0, "slot_id": 0},
                {"ams_id": 1, "slot_id": 1},
                {"ams_id": 255, "slot_id": 0},
                {"ams_id": 255, "slot_id": 255},
                {"ams_id": 254, "slot_id": 0}
            ])
        );
        assert_eq!(pr["task_id"], "777");
        assert_eq!(pr["md5"], "");
    }

    #[test]
    fn legacy_and_single_nozzle_payload() {
        let mut job = PrintJob::new("a.3mf");
        job.plate = 3;
        job.ams_mapping = vec![254];
        let p = job.to_payload("1", lookup("C12"), "5").unwrap();
        assert_eq!(p["print"]["url"], "file:///sdcard/a.3mf");
        assert_eq!(p["print"]["param"], "Metadata/plate_3.gcode");
        assert_eq!(p["print"]["nozzle_offset_cali"], 0);
        // External spool only on a single-nozzle printer → ams_id 255, use_ams false.
        assert_eq!(p["print"]["ams_mapping2"], json!([{"ams_id": 255, "slot_id": 0}]));
        assert_eq!(p["print"]["use_ams"], json!(false));
        let p = job.to_payload("1", None, "5").unwrap();
        assert_eq!(p["print"]["url"], "ftp://a.3mf");
    }

    #[test]
    fn rejects_bad_jobs() {
        for name in ["", "../x.3mf", "a/b.3mf", "x\r\nDELE y", "a#b.3mf"] {
            assert!(PrintJob::new(name).to_payload("1", None, "1").is_err(), "{name:?}");
        }
        let mut j = PrintJob::new("ok.3mf");
        j.plate = 0;
        assert!(j.to_payload("1", None, "1").is_err());
        let mut j = PrintJob::new("ok.3mf");
        j.ams_mapping = vec![300];
        assert!(j.to_payload("1", None, "1").is_err());
        let mut j = PrintJob::new("ok.3mf");
        j.md5 = Some("xyz".into());
        assert!(j.to_payload("1", None, "1").is_err());
    }

    #[test]
    fn submission_ids_fit_i32() {
        let id: i64 = submission_id().parse().unwrap();
        assert!(id > 0 && id < i64::from(i32::MAX));
    }
}
