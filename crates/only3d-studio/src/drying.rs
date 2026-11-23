//! `printer_ams` `dry` / `stop_drying`: dry the filament in an AMS 2 Pro or AMS HT, or stop.
//!
//! It follows Bambu Studio's drying dialog (`AMSDryControl.cpp`, `DevFilaSystemCtrl.cpp`):
//! - the temperature and time default to what the filament's Bambu preset recommends for that
//!   AMS model (`filament_dev_ams_drying_temperature` / `_time`), for the loaded filament that
//!   needs the lowest temperature;
//! - the AMS 2 Pro dries at 45–65 °C and the AMS HT at 45–85 °C, for 1–72 h;
//! - while a print runs, the temperature is capped at what the loaded filaments tolerate then;
//! - a loaded filament whose end would soften at that temperature (its heat distortion
//!   temperature) blocks the start: its end could clog the inlet;
//! - the printer's own reasons it can't dry now (`dry_sf_reason`) are passed on.
//!
//! Without `confirm` it only says what it would do. Starting heats the AMS for hours, so an
//! agent's start is approved by the user ([`crate::approval`]); stopping needs no approval.

use std::time::Duration;

use only3d_bambu::recovery::{ams_drying_payload, ams_drying_stop_payload, DryingJob};
use only3d_bambu::{AmsTray, AmsUnit, Command, PrinterStatus};
use only3d_profiles::{Config, ListFilter, PresetKind, ProfileStore, BAMBU_VENDOR};
use serde_json::{json, Value};

use crate::printer::{PrinterSession, SIGNED_COMMANDS_CODE, SIGNED_COMMANDS_HINT};
use crate::tools::{ToolError, ToolOutput};

/// Drying time Bambu Studio accepts (`AMS_DRY_MIN_HOUR`, `AMS_DRY_MAX_HOUR`).
pub const HOURS: (u32, u32) = (1, 72);
/// The filament Bambu Studio assumes when a slot's filament is unknown (PLA Basic).
const FALLBACK_FILAMENT_ID: &str = "GFA00";

/// A request to dry the filament in one AMS unit (`printer_ams` action `dry`).
#[derive(Clone, Debug, Default)]
pub struct DryRequest {
    pub ams_id: u32,
    /// The filament type to dry for ("PLA", "PETG"...); `None` = the loaded filament that needs
    /// the lowest temperature.
    pub filament: Option<String>,
    /// °C; `None` = what the filament's Bambu preset recommends for this AMS.
    pub temperature: Option<u32>,
    /// `None` = what the preset recommends.
    pub hours: Option<u32>,
    pub rotate_spools: bool,
    /// Send it; otherwise only the plan.
    pub confirm: bool,
}

/// An AMS model that dries, with Bambu Studio's limits and where its values sit in the filament
/// presets' drying lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dryer {
    /// AMS 2 Pro (Bambu's N3F).
    Ams2Pro,
    /// AMS HT (Bambu's N3S).
    AmsHt,
}

impl Dryer {
    pub fn of(unit: &AmsUnit) -> Option<Dryer> {
        match unit.model.as_deref() {
            Some("AMS 2 Pro") => Some(Dryer::Ams2Pro),
            Some("AMS HT") => Some(Dryer::AmsHt),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Dryer::Ams2Pro => "AMS 2 Pro",
            Dryer::AmsHt => "AMS HT",
        }
    }

    /// The temperatures it accepts (AMSDryControl `ams_limits`).
    pub fn temp_range(self) -> (u32, u32) {
        match self {
            Dryer::Ams2Pro => (45, 65),
            Dryer::AmsHt => (45, 85),
        }
    }

    /// Index in `filament_dev_ams_drying_ams_limitations` codes and the per-model lists
    /// (softening, heat distortion): 0 = AMS 2 Pro, 1 = AMS HT.
    fn model_index(self) -> usize {
        match self {
            Dryer::Ams2Pro => 0,
            Dryer::AmsHt => 1,
        }
    }

    /// Index in `filament_dev_ams_drying_temperature` / `_time`:
    /// [AMS 2 Pro idle, AMS HT idle, AMS 2 Pro printing, AMS HT printing, ...].
    fn job_index(self, printing: bool) -> usize {
        self.model_index() + if printing { 2 } else { 0 }
    }
}

/// A filament's drying values on one AMS model, from its Bambu preset
/// (`DevUtilBackend::FillDryingPresetFromConfig`).
#[derive(Clone, Debug, PartialEq)]
pub struct DryingPreset {
    /// The preset they came from.
    pub preset: String,
    pub temp_idle: f64,
    pub hours_idle: f64,
    pub temp_printing: f64,
    pub hours_printing: f64,
    /// The filament softens above this; Bambu Studio sends it as the cooling temperature.
    pub softening: Option<f64>,
    /// A filament end left in the inlet deforms above this.
    pub heat_distortion: Option<f64>,
    /// Bambu lists this AMS model as able to dry it completely.
    pub dries_fully: bool,
}

impl DryingPreset {
    /// The values for `dryer`, or `None` when the preset has no usable drying values (Bambu
    /// Studio's `HasUsableAmsDryingValues`: a temperature and a time above 0).
    pub fn from_config(preset: &str, cfg: &Config, dryer: Dryer) -> Option<DryingPreset> {
        let (t, h) = ("filament_dev_ams_drying_temperature", "filament_dev_ams_drying_time");
        if cfg.get_f64_at(t, 0)? <= 0.0 || cfg.get_f64_at(h, 0)? <= 0.0 {
            return None;
        }
        let at = |key: &str, idx: usize| cfg.get_f64_at(key, idx).filter(|v| *v > 0.0);
        let (idle, printing) = (dryer.job_index(false), dryer.job_index(true));
        let m = dryer.model_index();
        Some(DryingPreset {
            preset: preset.to_string(),
            temp_idle: at(t, idle)?,
            hours_idle: at(h, idle)?,
            temp_printing: at(t, printing)?,
            hours_printing: at(h, printing)?,
            softening: at("filament_dev_drying_softening_temperature", m),
            heat_distortion: at("filament_dev_ams_drying_heat_distortion_temperature", m),
            dries_fully: cfg.get_strs("filament_dev_ams_drying_ams_limitations").is_some_and(|l| l.iter().any(|c| c.trim() == m.to_string())),
        })
    }

    /// Bambu Studio's recommended temperature: the preset's, and while printing no hotter than
    /// the filament softens or deforms.
    pub fn recommended_temp(&self, printing: bool) -> f64 {
        if printing {
            [Some(self.temp_printing), self.softening, self.heat_distortion].into_iter().flatten().fold(f64::INFINITY, f64::min)
        } else {
            self.temp_idle
        }
    }

    pub fn recommended_hours(&self, printing: bool) -> f64 {
        if printing {
            self.hours_printing
        } else {
            self.hours_idle
        }
    }
}

/// Finds a filament's drying values in Bambu's presets: by its Bambu filament id (a slot's
/// `tray_info_idx`, e.g. GFG02), else by type through the generic preset ("Generic PETG-CF",
/// then "Generic PETG", then the `fdm_filament_*` base).
pub fn drying_preset(store: &ProfileStore, filament_id: Option<&str>, filament_type: Option<&str>, dryer: Dryer) -> Option<DryingPreset> {
    let from = |name: &str| store.resolve(PresetKind::Filament, name).ok().and_then(|cfg| DryingPreset::from_config(name, &cfg, dryer));
    if let Some(id) = filament_id.map(str::trim).filter(|i| !i.is_empty()) {
        let all = store.list(PresetKind::Filament, &ListFilter { include_abstract: true, ..Default::default() });
        // Bambu's own presets first, as Bambu Studio prefers system presets
        let mut owners: Vec<_> = all.into_iter().filter(|p| p.config.get_str("filament_id") == Some(id)).collect();
        owners.sort_by_key(|p| p.vendor != BAMBU_VENDOR);
        if let Some(found) = owners.iter().find_map(|p| from(&p.name)) {
            return Some(found);
        }
    }
    let ty = filament_type.map(str::trim).filter(|t| !t.is_empty())?;
    let base = ty.split('-').next().unwrap_or(ty);
    let lower = base.to_ascii_lowercase();
    let file = if lower == "petg" { "pet".to_string() } else { lower };
    [format!("Generic {ty} @base"), format!("Generic {base} @base"), format!("fdm_filament_{file}")].iter().find_map(|n| from(n))
}

/// The filament type Bambu Studio sends for a slot: support materials by their id, others by
/// type (`get_drying_filament_type_for_command`).
fn command_filament(tray: &AmsTray) -> String {
    let id = tray.tray_info_idx.as_deref().unwrap_or("");
    for (prefix, name) in [("GFS02", "Support for PLA"), ("GFS03", "Support for PA/PET"), ("GFS05", "Support for PLA/PETG"), ("GFS06", "Support for ABS")] {
        if id.starts_with(prefix) {
            return name.to_string();
        }
    }
    tray.filament_type.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| "PLA".into())
}

/// What a `dry` request will send, and why.
#[derive(Clone, Debug, PartialEq)]
pub struct DryPlan {
    pub ams_id: u32,
    /// "AMS A (AMS 2 Pro)".
    pub unit: String,
    pub job: DryingJob,
    /// "Dry PETG in AMS A at 65 °C for 12 h".
    pub title: String,
    /// Where the values came from, and warnings.
    pub notes: Vec<String>,
}

/// Looks up a filament's drying values: `(filament id, filament type, AMS model)`.
pub type PresetLookup<'a> = &'a dyn Fn(Option<&str>, Option<&str>, Dryer) -> Option<DryingPreset>;

/// The unit `ams_id` and its drying, or why it has none.
fn dryer_unit(st: &PrinterStatus, ams_id: u32) -> Result<(&AmsUnit, Dryer, &only3d_bambu::AmsDrying), ToolError> {
    let Some(unit) = st.ams.iter().find(|u| u.id == ams_id) else {
        let have: Vec<String> = st.ams.iter().map(|u| format!("{} = {} ({})", u.id, u.name, u.model.as_deref().unwrap_or("AMS"))).collect();
        return Err(ToolError::new(
            "no_such_ams",
            format!("the printer reports no AMS {ams_id}"),
            if have.is_empty() { "the printer reports no AMS at all".to_string() } else { format!("ams_id is one of: {}", have.join(", ")) },
        ));
    };
    let model = unit.model.as_deref().unwrap_or("AMS");
    let Some(dryer) = Dryer::of(unit) else {
        return Err(ToolError::new("cannot_dry", format!("AMS {} is an {model}, which has no dryer", unit.name), "only the AMS 2 Pro and AMS HT dry filament; use a filament dryer for the others"));
    };
    let Some(drying) = unit.drying.as_ref() else {
        return Err(ToolError::new("cannot_dry", format!("the printer reports no drying for AMS {} ({model})", unit.name), "update the printer's and the AMS's firmware, then try again"));
    };
    Ok((unit, dryer, drying))
}

/// Checks a start request against the printer's state and Bambu Studio's rules and works out
/// what to send.
pub fn plan_start(st: &PrinterStatus, i: &DryRequest, lookup: PresetLookup<'_>) -> Result<DryPlan, ToolError> {
    let (unit, dryer, drying) = dryer_unit(st, i.ams_id)?;
    let unit_name = format!("AMS {} ({})", unit.name, dryer.name());
    match drying.state.as_str() {
        "checking" | "drying" | "stopping" => {
            return Err(ToolError::new("already_drying", format!("{unit_name} is already {}", drying.summary), "stop it first (printer_ams action stop_drying) to change it, or let it finish"));
        }
        "error" | "heater_fault" | "factory_test" => {
            return Err(ToolError::new("cannot_dry", format!("{unit_name}: {}", drying.summary), "look at the printer's screen (printer_alert lists its alerts)"));
        }
        _ => {}
    }
    if !drying.blocked_by.is_empty() {
        let outlet = drying.blocked_by.iter().any(|r| r.contains("outlet"));
        return Err(ToolError::new(
            "cannot_dry_now",
            format!("{unit_name} can't dry now: {}", drying.blocked_by.join("; ")),
            if outlet { "unload the filament first (printer_ams action unload), then try again" } else { "printer_status shows when that has cleared; then try again" },
        ));
    }
    let printing = st.gcode_state.is_busy();
    let loaded: Vec<&AmsTray> = unit.trays.iter().filter(|t| t.loaded).collect();
    let fallback = || lookup(Some(FALLBACK_FILAMENT_ID), Some("PLA"), dryer);
    // Bambu Studio counts a slot it has no preset for as PLA
    let preset_of = |t: &AmsTray| lookup(t.tray_info_idx.as_deref(), t.filament_type.as_deref(), dryer).or_else(fallback);
    let mut notes = Vec::new();
    let (filament, preset) = match i.filament.as_deref().map(str::trim).filter(|f| !f.is_empty()) {
        Some(want) => match loaded.iter().find(|t| command_filament(t).eq_ignore_ascii_case(want) || t.filament_type.as_deref().is_some_and(|ty| ty.eq_ignore_ascii_case(want))) {
            Some(t) => (command_filament(t), preset_of(t)),
            None => (want.to_ascii_uppercase(), lookup(None, Some(&want.to_ascii_uppercase()), dryer)),
        },
        // the loaded filament that needs the lowest temperature, as Bambu Studio picks
        None => {
            let lowest = loaded.iter().filter_map(|t| preset_of(t).map(|p| (*t, p))).min_by(|(_, a), (_, b)| a.recommended_temp(printing).total_cmp(&b.recommended_temp(printing)));
            match lowest {
                Some((t, p)) => {
                    notes.push(format!("{} is the loaded filament that needs the lowest temperature", command_filament(t)));
                    (command_filament(t), Some(p))
                }
                None => {
                    notes.push("no filament is loaded, so it dries as for PLA".into());
                    ("PLA".to_string(), fallback())
                }
            }
        }
    };
    let rounded = |v: f64| v.round().max(0.0) as u32;
    let (temp, hours) = match (&preset, i.temperature, i.hours) {
        (_, Some(t), Some(h)) => (t, h),
        (Some(p), t, h) => {
            let source = format!("the {} preset", p.preset.trim_end_matches(" @base"));
            if t.is_none() || h.is_none() {
                notes.push(format!("{} recommends {} °C for {} h on the {}{}", source, rounded(p.recommended_temp(printing)), rounded(p.recommended_hours(printing)), dryer.name(), if printing { " during a print" } else { "" }));
            }
            (t.unwrap_or(rounded(p.recommended_temp(printing))), h.unwrap_or(rounded(p.recommended_hours(printing))))
        }
        (None, _, _) => {
            return Err(ToolError::new("bad_input", format!("Bambu has no drying values for {filament}"), "pass dry_temp and dry_hours (see the spool's label)"));
        }
    };
    let (lo, hi) = dryer.temp_range();
    if !(lo..=hi).contains(&temp) {
        return Err(ToolError::new("bad_input", format!("the {} dries at {lo}–{hi} °C, not {temp} °C", dryer.name()), format!("pass a dry_temp from {lo} to {hi}")));
    }
    if !(HOURS.0..=HOURS.1).contains(&hours) {
        return Err(ToolError::new("bad_input", format!("drying runs {}–{} h, not {hours} h", HOURS.0, HOURS.1), format!("pass dry_hours from {} to {}", HOURS.0, HOURS.1)));
    }
    if printing {
        // no hotter than every loaded filament tolerates during a print (Bambu Studio's
        // recommended temperature while the AMS is in use)
        let cap = loaded.iter().filter_map(|t| preset_of(t)).map(|p| p.recommended_temp(true)).fold(f64::INFINITY, f64::min);
        let cap = if cap.is_finite() { rounded(cap) } else { fallback().map(|p| rounded(p.recommended_temp(true))).unwrap_or(lo) };
        if temp > cap {
            return Err(ToolError::new(
                "too_hot_while_printing",
                format!("a print is running: drying {unit_name} above {cap} °C now could affect it and the filament it feeds"),
                format!("dry at {cap} °C or lower now, or wait for the print to finish"),
            ));
        }
    }
    // a filament end left in an inlet deforms above its heat distortion temperature and can
    // clog the inlet; Bambu Studio won't start then
    let soft: Vec<(String, u32)> = loaded
        .iter()
        .filter_map(|t| {
            let d = preset_of(t)?.heat_distortion?;
            (d < temp as f64).then(|| (format!("{} {}", crate::trays::label(st, t), command_filament(t)), rounded(d)))
        })
        .collect();
    if !soft.is_empty() {
        let lowest = soft.iter().map(|(_, d)| *d).min().unwrap_or(lo);
        let list: Vec<String> = soft.iter().map(|(what, d)| format!("{what} (above {d} °C)")).collect();
        return Err(ToolError::new(
            "filament_would_soften",
            format!("at {temp} °C the filament ends in these inlets would soften and could clog them: {}", list.join(", ")),
            if lowest >= lo {
                format!("dry at {lowest} °C or lower, or ask the user to pull those filaments' ends out of the inlets first (the slots then read empty)")
            } else {
                "ask the user to pull those filaments' ends out of the inlets first (the slots then read empty)".to_string()
            },
        ));
    }
    if let Some(p) = &preset {
        if !p.dries_fully {
            notes.push(format!("Bambu says the {} may not dry {filament} completely", dryer.name()));
        }
    }
    if i.rotate_spools {
        notes.push("the spools turn while drying; their filament ends should be out of the inlets".into());
    }
    let job = DryingJob { filament: filament.clone(), temp_c: temp, hours, rotate_spools: i.rotate_spools, cooling_temp_c: preset.as_ref().and_then(|p| p.softening).map(rounded).unwrap_or(50) };
    let title = format!("Dry {filament} in AMS {} at {temp} °C for {hours} h", unit.name);
    Ok(DryPlan { ams_id: i.ams_id, unit: unit_name, job, title, notes })
}

/// The printer's answer to a command: `Ok(what it said)`, or `Err` when it refused.
fn verdict(reply: Result<Value, only3d_bambu::Error>) -> Result<String, ToolError> {
    let v = match reply {
        Ok(v) => v,
        Err(e) if e.code() == "timeout" => return Ok("sent (no reply yet)".into()),
        Err(e) => return Err(ToolError::new(e.code(), e.to_string(), e.hint())),
    };
    let body = v.get("print").cloned().unwrap_or(Value::Null);
    let result = body.get("result").and_then(Value::as_str).unwrap_or("").to_ascii_lowercase();
    let reason = body.get("reason").and_then(Value::as_str).unwrap_or("");
    let code = body.get("err_code").and_then(|c| c.as_i64().map(|n| n.to_string()).or_else(|| c.as_str().map(str::to_string))).filter(|c| c != "0" && !c.is_empty());
    if code.as_deref() == Some(SIGNED_COMMANDS_CODE) {
        return Err(ToolError::new("printer_auth_required", "the printer refused the command: it only accepts signed commands in cloud mode", SIGNED_COMMANDS_HINT));
    }
    if result == "fail" || result == "failed" || code.is_some() {
        let why = [Some(reason.to_string()).filter(|r| !r.is_empty()), code.map(|c| format!("code {c}"))].into_iter().flatten().collect::<Vec<_>>().join(", ");
        return Err(ToolError::new("printer_rejected", format!("the printer refused it{}", if why.is_empty() { String::new() } else { format!(" ({why})") }), "look at the printer's screen; printer_status shows why the AMS can't dry"));
    }
    Ok(if result.is_empty() { "acknowledged".into() } else { result })
}

/// The drying of unit `ams_id` once `done` holds for it, or after 10 s.
fn drying_after(ps: &PrinterSession, ams_id: u32, done: impl Fn(&only3d_bambu::AmsDrying) -> bool) -> Option<only3d_bambu::AmsDrying> {
    let of = |st: &PrinterStatus| st.ams.iter().find(|u| u.id == ams_id).and_then(|u| u.drying.clone());
    let now = ps.client.wait_for(|st| of(st).is_some_and(|d| done(&d)), Duration::from_secs(10)).unwrap_or_else(|_| ps.client.status());
    of(&now)
}

/// `printer_ams` action `dry`: the plan, or with `confirm` the job sent to the AMS.
pub fn start(ps: &PrinterSession, i: &DryRequest) -> Result<ToolOutput, ToolError> {
    let st = ps.client.status();
    let printer = &ps.cfg.name;
    let store = crate::bambu::store().ok();
    let lookup = |id: Option<&str>, ty: Option<&str>, dryer: Dryer| store.as_deref().and_then(|s| drying_preset(s, id, ty, dryer));
    let plan = plan_start(&st, i, &lookup)?;
    let notes = if plan.notes.is_empty() { String::new() } else { format!("\n- {}", plan.notes.join("\n- ")) };
    let payload = ams_drying_payload(plan.ams_id, &plan.job);
    // what the user is shown when an agent asks to start it (approval::prepare)
    let summary = format!("{}{} on {printer}.{notes}", plan.title, if plan.job.rotate_spools { ", turning the spools" } else { "" });
    let data = |sent: bool, drying: Option<&only3d_bambu::AmsDrying>| {
        json!({"ready": true, "sent": sent, "title": plan.title, "plan": summary, "unit": plan.unit, "filament": plan.job.filament, "temperature": plan.job.temp_c,
            "hours": plan.job.hours, "rotate_spools": plan.job.rotate_spools, "notes": plan.notes, "payload": payload, "drying": drying})
    };
    if !i.confirm {
        let text = format!("Ready on {printer}: {}.{notes}\nNothing sent yet. To start it, call again with confirm: true; an agent's request is approved by the user in only3d.", plan.title);
        return Ok(ToolOutput::text(text).with_data(data(false, None)));
    }
    let ack = verdict(ps.client.request(&Command::Raw(payload.clone()), Duration::from_secs(8)))?;
    // the AMS checks first (a few seconds), then heats
    let after = drying_after(ps, plan.ams_id, |d| d.is_active() || !d.blocked_by.is_empty());
    let state = match &after {
        Some(d) if d.is_active() => format!("Now: {}.", d.summary),
        Some(d) if !d.blocked_by.is_empty() => format!("The AMS didn't start: {}.", d.blocked_by.join("; ")),
        _ => "It hasn't reported drying yet; printer_status shows it once it does.".into(),
    };
    let text = format!("{printer}: {} ({ack}). {state}{notes}\nAn ams_drying event reports when it finishes or stops.", plan.title);
    Ok(ToolOutput::text(text).with_data(data(true, after.as_ref())))
}

/// `printer_ams` action `stop_drying`.
pub fn stop(ps: &PrinterSession, ams_id: u32) -> Result<ToolOutput, ToolError> {
    let st = ps.client.status();
    let printer = &ps.cfg.name;
    let (unit, dryer, drying) = dryer_unit(&st, ams_id)?;
    let unit_name = format!("AMS {} ({})", unit.name, dryer.name());
    if !matches!(drying.state.as_str(), "checking" | "drying") {
        return Ok(ToolOutput::text(format!("{printer}: {unit_name} isn't drying ({}); nothing to stop.", drying.summary)).with_data(json!({"sent": false, "drying": drying})));
    }
    let payload = ams_drying_stop_payload(ams_id);
    let ack = verdict(ps.client.request(&Command::Raw(payload.clone()), Duration::from_secs(8)))?;
    let after = drying_after(ps, ams_id, |d| !matches!(d.state.as_str(), "checking" | "drying"));
    let summary = after.as_ref().map(|d| d.summary.clone()).unwrap_or_default();
    Ok(ToolOutput::text(format!("{printer}: asked {unit_name} to stop drying ({ack}). Now: {summary}.")).with_data(json!({"sent": true, "payload": payload, "drying": after})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use only3d_bambu::{AmsDrying, GcodeState};

    fn tray(slot: u32, ty: &str, idx: &str) -> AmsTray {
        AmsTray { slot_id: slot, global_id: slot, loaded: true, filament_type: Some(ty.into()), tray_info_idx: Some(idx.into()), ..Default::default() }
    }

    fn printer(model: &str, trays: Vec<AmsTray>, drying: AmsDrying) -> PrinterStatus {
        PrinterStatus {
            gcode_state: GcodeState::Idle,
            ams: vec![AmsUnit { id: 0, name: "A".into(), model: Some(model.into()), drying: Some(drying), trays, ..Default::default() }],
            ..Default::default()
        }
    }

    fn off() -> AmsDrying {
        AmsDrying { state: "off".into(), summary: "not drying".into(), ..Default::default() }
    }

    fn start() -> DryRequest {
        DryRequest::default()
    }

    /// PLA and PETG as Bambu's presets have them for the AMS 2 Pro.
    fn presets(_id: Option<&str>, ty: Option<&str>, _d: Dryer) -> Option<DryingPreset> {
        let p = |name: &str, temp: f64, printing: f64, soft: f64, distortion: f64| DryingPreset {
            preset: name.into(),
            temp_idle: temp,
            hours_idle: 12.0,
            temp_printing: printing,
            hours_printing: 12.0,
            softening: Some(soft),
            heat_distortion: Some(distortion),
            dries_fully: true,
        };
        match ty? {
            "PLA" => Some(p("Bambu PLA Basic @base", 45.0, 45.0, 50.0, 45.0)),
            "PETG" => Some(p("Bambu PETG Basic @base", 65.0, 55.0, 70.0, 70.0)),
            _ => None,
        }
    }

    #[test]
    fn defaults_come_from_the_loaded_filament_that_needs_the_lowest_temperature() {
        let st = printer("AMS 2 Pro", vec![tray(0, "PETG", "GFG00"), tray(1, "PLA", "GFA00")], off());
        let plan = plan_start(&st, &start(), &presets).unwrap();
        assert_eq!(plan.title, "Dry PLA in AMS A at 45 °C for 12 h");
        assert_eq!(plan.job, DryingJob { filament: "PLA".into(), temp_c: 45, hours: 12, rotate_spools: false, cooling_temp_c: 50 });
        // only PETG loaded: its own values
        let st = printer("AMS 2 Pro", vec![tray(0, "PETG", "GFG00")], off());
        let plan = plan_start(&st, &start(), &presets).unwrap();
        assert_eq!((plan.job.filament.as_str(), plan.job.temp_c, plan.job.cooling_temp_c), ("PETG", 65, 70));
        assert!(plan.notes.iter().any(|n| n.contains("Bambu PETG Basic preset recommends 65 °C for 12 h")), "{:?}", plan.notes);
        // nothing loaded: as for PLA
        let st = printer("AMS 2 Pro", vec![], off());
        assert_eq!(plan_start(&st, &start(), &presets).unwrap().job.filament, "PLA");
    }

    #[test]
    fn bambu_studios_limits_hold() {
        let st = printer("AMS 2 Pro", vec![tray(0, "PETG", "GFG00")], off());
        let code = |st: &PrinterStatus, i: DryRequest| plan_start(st, &i, &presets).unwrap_err().code;
        assert_eq!(code(&st, DryRequest { temperature: Some(70), ..start() }), "bad_input", "the AMS 2 Pro tops out at 65 °C");
        assert_eq!(code(&st, DryRequest { temperature: Some(40), ..start() }), "bad_input");
        assert_eq!(code(&st, DryRequest { hours: Some(0), ..start() }), "bad_input");
        assert_eq!(code(&st, DryRequest { hours: Some(73), ..start() }), "bad_input");
        assert!(plan_start(&st, &DryRequest { temperature: Some(60), hours: Some(72), ..start() }, &presets).is_ok());
        // the AMS HT goes to 85 °C
        let ht = PrinterStatus { ams: vec![AmsUnit { id: 128, name: "HT-A".into(), ..st.ams[0].clone() }], ..st.clone() };
        let ht = PrinterStatus { ams: vec![AmsUnit { model: Some("AMS HT".into()), ..ht.ams[0].clone() }], ..ht };
        assert!(plan_start(&ht, &DryRequest { ams_id: 128, filament: Some("PETG".into()), temperature: Some(70), ..start() }, &presets).is_ok());
        assert_eq!(code(&ht, DryRequest { ams_id: 128, temperature: Some(90), ..start() }), "bad_input");
    }

    #[test]
    fn units_that_cant_dry_now_are_refused() {
        let code = |st: &PrinterStatus| plan_start(st, &start(), &presets).unwrap_err().code;
        assert_eq!(code(&printer("AMS", vec![], off())), "cannot_dry", "the first AMS has no dryer");
        assert_eq!(code(&PrinterStatus { ams: vec![], ..printer("AMS 2 Pro", vec![], off()) }), "no_such_ams");
        let drying = AmsDrying { state: "drying".into(), summary: "drying PLA at 45 °C".into(), ..Default::default() };
        assert_eq!(code(&printer("AMS 2 Pro", vec![], drying)), "already_drying");
        let blocked = AmsDrying { blocked_by: vec!["filament is in the AMS outlet: unload it first".into()], ..off() };
        let e = plan_start(&printer("AMS 2 Pro", vec![], blocked), &start(), &presets).unwrap_err();
        assert_eq!(e.code, "cannot_dry_now");
        assert!(e.hint.contains("printer_ams action unload"), "{e:?}");
        // cooling down after a job counts as idle, as in Bambu Studio
        assert!(plan_start(&printer("AMS 2 Pro", vec![], AmsDrying { state: "cooling".into(), ..off() }), &start(), &presets).is_ok());
    }

    #[test]
    fn a_loaded_filament_that_would_soften_blocks_a_hotter_job() {
        // PLA deforms above 45 °C: drying the PETG beside it at 65 °C would clog PLA's inlet
        let st = printer("AMS 2 Pro", vec![tray(0, "PETG", "GFG00"), tray(1, "PLA", "GFA00")], off());
        let e = plan_start(&st, &DryRequest { filament: Some("petg".into()), ..start() }, &presets).unwrap_err();
        assert_eq!(e.code, "filament_would_soften");
        assert!(e.message.contains("A2 PLA (above 45 °C)") && e.hint.contains("dry at 45 °C or lower"), "{e:?}");
    }

    #[test]
    fn during_a_print_the_temperature_is_capped() {
        let st = PrinterStatus { gcode_state: GcodeState::Running, ..printer("AMS 2 Pro", vec![tray(0, "PETG", "GFG00")], off()) };
        // PETG dries at 55 °C during a print (65 when idle)
        assert_eq!(plan_start(&st, &start(), &presets).unwrap().job.temp_c, 55);
        assert_eq!(plan_start(&st, &DryRequest { temperature: Some(65), ..start() }, &presets).unwrap_err().code, "too_hot_while_printing");
    }

    #[test]
    fn support_filaments_are_named_as_bambu_studio_names_them() {
        assert_eq!(command_filament(&tray(0, "PLA-S", "GFS02")), "Support for PLA");
        assert_eq!(command_filament(&tray(0, "PETG", "GFG02")), "PETG");
        assert_eq!(command_filament(&AmsTray { loaded: true, ..Default::default() }), "PLA");
    }

    #[test]
    fn drying_values_come_from_bambus_presets() {
        let store = crate::bambu::store().unwrap();
        // by the slot's filament id, then by type through the generic presets
        let pla = drying_preset(&store, Some("GFA00"), Some("PLA"), Dryer::Ams2Pro).unwrap();
        assert_eq!((pla.temp_idle, pla.hours_idle, pla.heat_distortion, pla.softening), (45.0, 12.0, Some(45.0), Some(50.0)));
        assert!(pla.dries_fully);
        let petg = drying_preset(&store, None, Some("PETG"), Dryer::Ams2Pro).unwrap();
        assert!(petg.temp_idle > pla.temp_idle, "{petg:?}");
        assert!(drying_preset(&store, None, Some("PETG-CF"), Dryer::AmsHt).is_some());
        assert!(drying_preset(&store, Some("nonsense"), None, Dryer::Ams2Pro).is_none());
    }
}
