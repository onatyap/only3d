//! Recovering from printer alerts, switching nozzles and moving filament, the way Bambu Studio
//! does it.
//!
//! Sources:
//! - `DeviceErrorDialog.{hpp,cpp}` (Bambu Studio): which button sends which command, and the
//!   numeric action ids used in the per-model `hms_action_<dev>.json` tables.
//! - bambuddy's `bambu_mqtt.py` (AGPL-3.0): the exact MQTT payloads for those commands, the
//!   `ams_change_filament` addressing and `select_extruder`.
//!
//! The action tables themselves are read from the user's Bambu Studio installation (like the
//! HMS texts), not copied.

use serde::Serialize;
use serde_json::{json, Value};

/// What the user (or an agent, with the user's approval) can do about an alert.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertAction {
    /// Resume the paused print (Bambu: Resume / Resume (defects acceptable) / Problem solved, resume).
    Resume,
    /// Stop the print.
    Stop,
    /// Ignore this alert and resume.
    Ignore,
    /// Don't show this alert again (until the next job).
    DontRemind,
    /// Acknowledge (clears the print error).
    Ok,
    /// AMS: filament was extruded / done.
    AmsDone,
    /// AMS: retry.
    AmsRetry,
    /// AMS: abort.
    AmsAbort,
}

impl AlertAction {
    pub fn parse(s: &str) -> Option<AlertAction> {
        Some(match s.to_ascii_lowercase().as_str() {
            "resume" => AlertAction::Resume,
            "stop" => AlertAction::Stop,
            "ignore" => AlertAction::Ignore,
            "dont_remind" => AlertAction::DontRemind,
            "ok" => AlertAction::Ok,
            "ams_done" => AlertAction::AmsDone,
            "ams_retry" => AlertAction::AmsRetry,
            "ams_abort" => AlertAction::AmsAbort,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            AlertAction::Resume => "resume",
            AlertAction::Stop => "stop",
            AlertAction::Ignore => "ignore",
            AlertAction::DontRemind => "dont_remind",
            AlertAction::Ok => "ok",
            AlertAction::AmsDone => "ams_done",
            AlertAction::AmsRetry => "ams_retry",
            AlertAction::AmsAbort => "ams_abort",
        }
    }

    /// Bambu Studio's `DeviceErrorDialog::ActionButton` id -> (action, button label).
    pub fn from_bambu_id(id: i64) -> Option<(AlertAction, &'static str)> {
        Some(match id {
            2 => (AlertAction::Resume, "Resume printing"),
            3 => (AlertAction::Resume, "Resume (defects acceptable)"),
            4 => (AlertAction::Resume, "Resume (problem solved)"),
            5 => (AlertAction::Stop, "Stop printing"),
            7 => (AlertAction::AmsDone, "Filament extruded, continue"),
            8 => (AlertAction::AmsRetry, "Not extruded yet, retry"),
            9 => (AlertAction::AmsRetry, "Continue"),
            11 => (AlertAction::Ok, "OK"),
            12 => (AlertAction::Resume, "Filament loaded, resume"),
            23 => (AlertAction::DontRemind, "Don't remind me next time"),
            25 | 27 => (AlertAction::Ignore, "Ignore this and resume"),
            28 => (AlertAction::Resume, "Problem solved and resume"),
            34 => (AlertAction::AmsRetry, "Problem solved, retry"),
            41 => (AlertAction::Resume, "Proceed"),
            51 => (AlertAction::AmsAbort, "Abort"),
            _ => return None,
        })
    }
}

/// The MQTT payload for `action` on the alert `err` ("05FF_8053" or "05FF8053"), for the job
/// `job_id` (the report's `job_id`).
pub fn alert_payload(action: AlertAction, err: &str, job_id: &str) -> Value {
    let hex: String = err.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let err_dec = u64::from_str_radix(&hex, 16).map(|v| v.to_string()).unwrap_or(hex);
    match action {
        AlertAction::Resume => json!({"print": {"command": "resume", "param": ""}}),
        AlertAction::Stop => json!({"print": {"command": "stop", "param": ""}}),
        AlertAction::Ignore => json!({"print": {"command": "ignore", "err": err_dec, "param": "reserve", "job_id": job_id}}),
        AlertAction::DontRemind => json!({"print": {"command": "idle_ignore", "err": err_dec, "type": 0}}),
        AlertAction::Ok => json!({"print": {"command": "clean_print_error"}}),
        AlertAction::AmsDone => json!({"print": {"command": "ams_control", "param": "done"}}),
        AlertAction::AmsRetry => json!({"print": {"command": "ams_control", "param": "resume"}}),
        AlertAction::AmsAbort => json!({"print": {"command": "ams_control", "param": "abort"}}),
    }
}

/// Actions Bambu Studio offers for alert `err` on a printer whose serial starts with `dev`
/// (e.g. "094" for an H2D), from the installed Bambu Studio's `hms_action_<dev>.json`.
/// `None` when the table is not installed or has no entry for this alert.
pub fn offered_actions(dev: &str, err: &str) -> Option<Vec<(AlertAction, &'static str)>> {
    let hex: String = err.chars().filter(|c| c.is_ascii_hexdigit()).collect::<String>().to_ascii_uppercase();
    let candidates = [
        crate::bambu_studio_dir().join(format!("hms/hms_action_{dev}.json")),
        std::path::PathBuf::from(format!("/Applications/BambuStudio.app/Contents/Resources/hms/hms_action_{dev}.json")),
    ];
    for path in candidates {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        let entries = v.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
        if let Some(e) = entries.iter().find(|e| e.get("ecode").and_then(Value::as_str).is_some_and(|c| c.eq_ignore_ascii_case(&hex))) {
            let mut out: Vec<(AlertAction, &'static str)> = Vec::new();
            for id in e.get("actions").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_i64) {
                if let Some(a) = AlertAction::from_bambu_id(id) {
                    if !out.iter().any(|(x, _)| *x == a.0) {
                        out.push(a);
                    }
                }
            }
            return Some(out);
        }
    }
    None
}

/// `select_extruder`: make a nozzle the active one on a dual-nozzle printer.
pub fn select_extruder_payload(right: bool) -> Value {
    // firmware numbering: 0 = right (main), 1 = left (deputy)
    json!({"print": {"command": "select_extruder", "extruder_index": if right { 0 } else { 1 }}})
}

/// `ams_change_filament` to load the filament of global tray `tray` (AMS slots 0..15, AMS-HT
/// units 128..135, external holders 254 = left / 255 = right on the H2D). `right_temp` is the
/// right nozzle's current temperature (used only for the right external spool).
pub fn ams_load_payload(tray: u32, right_temp: Option<f64>) -> Value {
    let (ams_id, slot_id, target, temp) = match tray {
        255 => (255, 0, 255, right_temp.filter(|t| *t >= 180.0).unwrap_or(215.0).round() as i64),
        254 => (255, 254, 254, -1),
        128..=135 => (tray, 0, tray, -1),
        t => (t / 4, t % 4, t, -1),
    };
    json!({"print": {"command": "ams_change_filament", "ams_id": ams_id, "slot_id": slot_id, "target": target, "curr_temp": temp, "tar_temp": temp}})
}

/// `ams_change_filament` to unload. `source_tray` is the tray being unloaded (its AMS is
/// addressed, as Bambu Studio does); `nozzle_temp` is the active nozzle's temperature.
pub fn ams_unload_payload(source_tray: Option<u32>, nozzle_temp: Option<f64>) -> Value {
    let ams_id = match source_tray {
        None | Some(254) | Some(255) => 255,
        Some(t @ 128..=135) => t,
        Some(t) => t / 4,
    };
    let temp = nozzle_temp.filter(|t| *t >= 180.0).unwrap_or(210.0).round() as i64;
    json!({"print": {"command": "ams_change_filament", "ams_id": ams_id, "slot_id": 255, "target": 255, "curr_temp": temp, "tar_temp": temp}})
}

/// What an AMS that dries (AMS 2 Pro, AMS HT) is asked to do by [`ams_drying_payload`].
#[derive(Clone, Debug, PartialEq)]
pub struct DryingJob {
    /// The filament type the job is set for, as Bambu Studio names it: "PLA", "PETG",
    /// "Support for PLA"...
    pub filament: String,
    pub temp_c: u32,
    pub hours: u32,
    /// Turn the spools while drying, so they dry evenly.
    pub rotate_spools: bool,
    /// The filament's softening temperature on this AMS (Bambu Studio sends the preset's
    /// `filament_dev_drying_softening_temperature` here, 50 when it has none).
    pub cooling_temp_c: u32,
}

/// `ams_filament_drying` to start drying the filament in AMS unit `ams_id` for a set time, as
/// Bambu Studio's drying dialog sends it (`DevFilaSystem::CtrlAmsStartDryingHour`).
pub fn ams_drying_payload(ams_id: u32, job: &DryingJob) -> Value {
    // mode 1 = dry for a time (0 off, 2 until a humidity, which Bambu Studio doesn't offer);
    // zone 0 = the whole unit on the AMS 2 Pro and AMS HT
    json!({"print": {"command": "ams_filament_drying", "ams_id": ams_id, "zone_id": 0, "mode": 1,
        "filament": job.filament, "temp": job.temp_c, "duration": job.hours, "humidity": 0,
        "rotate_tray": job.rotate_spools, "cooling_temp": job.cooling_temp_c, "close_power_conflict": false}})
}

/// `ams_filament_drying` with mode 0: stop drying AMS unit `ams_id`
/// (`DevFilaSystem::CtrlAmsStopDrying`).
pub fn ams_drying_stop_payload(ams_id: u32) -> Value {
    json!({"print": {"command": "ams_filament_drying", "ams_id": ams_id, "mode": 0, "filament": "", "temp": 0, "duration": 0,
        "humidity": 0, "rotate_tray": false, "cooling_temp": 0, "close_power_conflict": false}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alert_payloads_match_bambu_studio() {
        // the nozzle-mismatch pause we hit: "ignore and resume" sends the code in decimal
        assert_eq!(alert_payload(AlertAction::Ignore, "05FF_8053", "123"), json!({"print": {"command": "ignore", "err": "100630611", "param": "reserve", "job_id": "123"}}));
        assert_eq!(alert_payload(AlertAction::Resume, "0C00_8042", ""), json!({"print": {"command": "resume", "param": ""}}));
        assert_eq!(alert_payload(AlertAction::Ok, "x", ""), json!({"print": {"command": "clean_print_error"}}));
        assert_eq!(AlertAction::from_bambu_id(28).unwrap().0, AlertAction::Resume);
        assert_eq!(AlertAction::from_bambu_id(27).unwrap().0, AlertAction::Ignore);
        assert_eq!(AlertAction::from_bambu_id(5).unwrap().0, AlertAction::Stop);
        assert!(AlertAction::from_bambu_id(6).is_none(), "\"check the assistant\" opens a web page, not a printer action");
    }

    #[test]
    fn reads_the_installed_bambu_studio_action_table() {
        // only where Bambu Studio is installed (the table ships with it, keyed by serial prefix)
        let Some(acts) = offered_actions("094", "05FF8053") else { return };
        let names: Vec<_> = acts.iter().map(|(a, _)| *a).collect();
        assert_eq!(names, vec![AlertAction::Resume, AlertAction::Ignore], "H2D nozzle-mismatch alert");
        if let Some(acts) = offered_actions("094", "0C00_8042") {
            assert!(acts.iter().any(|(a, _)| *a == AlertAction::Stop) && acts.iter().any(|(a, _)| *a == AlertAction::Resume), "spaghetti alert: {acts:?}");
        }
    }

    #[test]
    fn ams_addressing_matches_bambu_studio() {
        let p = |v: Value| v["print"].clone();
        // AMS 0 slot 3 (global tray 2... 0-based): unit, local slot, global target, printer picks temperatures
        assert_eq!(p(ams_load_payload(2, None)), json!({"command": "ams_change_filament", "ams_id": 0, "slot_id": 2, "target": 2, "curr_temp": -1, "tar_temp": -1}));
        assert_eq!(p(ams_load_payload(6, None))["ams_id"], 1);
        // external holders on the H2D
        assert_eq!(p(ams_load_payload(254, None)), json!({"command": "ams_change_filament", "ams_id": 255, "slot_id": 254, "target": 254, "curr_temp": -1, "tar_temp": -1}));
        assert_eq!(p(ams_load_payload(255, Some(40.0))), json!({"command": "ams_change_filament", "ams_id": 255, "slot_id": 0, "target": 255, "curr_temp": 215, "tar_temp": 215}));
        // AMS-HT: its unit id is its tray
        assert_eq!(p(ams_load_payload(129, None))["target"], 129);
        // unload addresses the source tray's AMS
        assert_eq!(p(ams_unload_payload(Some(2), Some(250.0))), json!({"command": "ams_change_filament", "ams_id": 0, "slot_id": 255, "target": 255, "curr_temp": 250, "tar_temp": 250}));
        assert_eq!(p(ams_unload_payload(None, Some(30.0)))["curr_temp"], 210);
        assert_eq!(select_extruder_payload(false), json!({"print": {"command": "select_extruder", "extruder_index": 1}}));
    }

    #[test]
    fn drying_payloads_match_bambu_studio() {
        let job = DryingJob { filament: "PETG".into(), temp_c: 65, hours: 12, rotate_spools: true, cooling_temp_c: 70 };
        assert_eq!(
            ams_drying_payload(128, &job),
            json!({"print": {"command": "ams_filament_drying", "ams_id": 128, "zone_id": 0, "mode": 1, "filament": "PETG", "temp": 65, "duration": 12,
                "humidity": 0, "rotate_tray": true, "cooling_temp": 70, "close_power_conflict": false}})
        );
        assert_eq!(
            ams_drying_stop_payload(1),
            json!({"print": {"command": "ams_filament_drying", "ams_id": 1, "mode": 0, "filament": "", "temp": 0, "duration": 0,
                "humidity": 0, "rotate_tray": false, "cooling_temp": 0, "close_power_conflict": false}})
        );
    }
}
