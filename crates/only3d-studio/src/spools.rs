//! Filament inventory: register spools, track what is left, and keep a usage history.
//!
//! Remaining filament is kept up to date from three sources:
//! * **AMS readings** (`spool_sync`): Bambu RFID spools report a unique `tray_uuid` and
//!   `remain_g`, so they are matched and updated automatically (new RFID spools are registered on
//!   first sight);
//! * **prints**: only3d charges the sliced grams to each filament slot's spool when a print
//!   starts, and records jobs it sees on the printer;
//! * **weigh-ins / manual entries** (`spool_weigh`, `spool_use`) for spools without RFID.
//!
//! Stored in the inventory database (see [`crate::inventory`]): every change is a ledger event,
//! and print-caused events point at the print (→ project version → objects).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::tools::{def, Registry, ToolError, ToolOutput};
use crate::Studio;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct UsageEvent {
    /// ISO date-time (UTC).
    pub at: String,
    /// Change in grams (negative = consumed).
    pub delta_g: f64,
    pub remaining_g: f64,
    /// "add" | "print" | "ams" | "weigh" | "manual"
    pub source: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Spool {
    /// Short id, e.g. "petg-pink-1".
    pub id: String,
    pub name: String,
    /// The name follows brand, line and colour (see [`crate::spool_names`]) until renamed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub name_auto: bool,
    /// "PLA", "PETG", "ASA", "TPU"...
    pub material: String,
    #[serde(default)]
    pub brand: String,
    /// Product line, e.g. "PETG Translucent".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_brand: Option<String>,
    /// "#RRGGBB"
    pub color: String,
    /// Bambu Studio filament preset to slice with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// Bambu filament id (GFA00, GFG01...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filament_id: Option<String>,
    #[serde(default = "d175")]
    pub diameter_mm: f64,
    /// Filament weight of a full spool (g).
    pub net_weight_g: f64,
    pub remaining_g: f64,
    /// Empty-spool weight (g), for weigh-ins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tare_g: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_kg: Option<f64>,
    /// RFID tray_uuid of Bambu spools (auto-matched by spool_sync).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rfid_uuid: Option<String>,
    /// Where it is: "ams0:1".."ams0:4", "ext:left", "ext:right", or free text ("shelf", "dry box").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    pub added: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    /// What this spool was calibrated to; used when it is linked to a filament slot.
    #[serde(default, skip_serializing_if = "Calibration::is_empty")]
    pub calibration: Calibration,
    #[serde(default)]
    pub history: Vec<UsageEvent>,
}

/// A spool's calibration, used when it is in a filament slot.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Calibration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_ratio: Option<f64>,
    /// K
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressure_advance: Option<f64>,
    /// mm³/s
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_volumetric_speed: Option<f64>,
}

impl Calibration {
    pub fn is_empty(&self) -> bool {
        self.flow_ratio.is_none() && self.pressure_advance.is_none() && self.max_volumetric_speed.is_none()
    }

    /// Put these values into a resolved Bambu filament config.
    pub fn apply(&self, cfg: &mut only3d_profiles::Config) {
        let set = |cfg: &mut only3d_profiles::Config, k: &str, v: String| {
            let n = cfg.len_of(k).unwrap_or(1).max(1);
            let val: only3d_profiles::ConfigValue = if cfg.get(k).is_some_and(|x| x.is_array()) { vec![v; n].into() } else { v.into() };
            cfg.insert(k, val);
        };
        if let Some(f) = self.flow_ratio {
            set(cfg, "filament_flow_ratio", f.to_string());
        }
        if let Some(v) = self.max_volumetric_speed {
            set(cfg, "filament_max_volumetric_speed", v.to_string());
        }
        if let Some(k) = self.pressure_advance {
            set(cfg, "pressure_advance", k.to_string());
            set(cfg, "enable_pressure_advance", "1".into());
        }
    }
}

/// The calibration of the spool linked to a filament slot (none in unit tests).
pub fn slot_calibration(spool: Option<&str>) -> Calibration {
    if cfg!(test) {
        return Calibration::default();
    }
    spool.and_then(|id| Db::open().ok()?.spool(id).ok().flatten()).map(|s| s.calibration).unwrap_or_default()
}

fn d175() -> f64 {
    1.75
}

/// Spools for UI lists (reads the inventory database).
pub struct Inventory {
    pub spools: Vec<Spool>,
}

impl Inventory {
    pub fn load() -> Inventory {
        Inventory { spools: crate::inventory::Db::open().and_then(|d| d.spools(true)).unwrap_or_default() }
    }

    pub fn get(&self, id: &str) -> Option<&Spool> {
        self.spools.iter().find(|s| s.id == id)
    }
}

impl Spool {
    pub fn blank(id: &str, material: &str, color: &str, net_weight_g: f64) -> Spool {
        Spool {
            id: id.into(),
            name: String::new(),
            name_auto: false,
            material: material.into(),
            brand: String::new(),
            sub_brand: None,
            color: color.into(),
            preset: None,
            filament_id: None,
            diameter_mm: 1.75,
            net_weight_g,
            remaining_g: 0.0,
            tare_g: None,
            cost_per_kg: None,
            rfid_uuid: None,
            location: None,
            added: now_iso(),
            last_used: None,
            archived: false,
            notes: String::new(),
            calibration: Calibration::default(),
            history: Vec::new(),
        }
    }

    pub fn percent(&self) -> f64 {
        if self.net_weight_g > 0.0 {
            (self.remaining_g / self.net_weight_g * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        }
    }

    pub fn low(&self) -> bool {
        self.remaining_g < 100.0 || self.percent() < 10.0
    }

    /// Brand + line + colour name, what the name is while it is automatic.
    pub fn auto_name(&self) -> String {
        crate::spool_names::auto_name(&self.brand, self.sub_brand.as_deref(), &self.material, &self.color, self.filament_id.as_deref())
    }

    /// Make the name automatic and fill it in.
    pub fn set_auto_name(&mut self) {
        self.name_auto = true;
        self.name = self.auto_name();
    }

    pub fn line(&self) -> String {
        let name = if self.name.trim().is_empty() { self.auto_name() } else { self.name.clone() };
        format!(
            "{}: {} {} — {:.0} g left of {:.0} g ({:.0}%){}{}{}{}",
            self.id,
            name,
            self.color,
            self.remaining_g,
            self.net_weight_g,
            self.percent(),
            self.location.as_ref().map(|l| format!(" @ {l}")).unwrap_or_default(),
            if self.rfid_uuid.is_some() { " [RFID]" } else { "" },
            if self.low() { " LOW" } else { "" },
            if self.calibration.is_empty() {
                String::new()
            } else {
                let c = &self.calibration;
                let parts: Vec<String> = [c.flow_ratio.map(|v| format!("flow {v}")), c.pressure_advance.map(|v| format!("K {v}")), c.max_volumetric_speed.map(|v| format!("{v} mm³/s"))].into_iter().flatten().collect();
                format!(" · calibrated: {}", parts.join(", "))
            }
        )
    }
}

/// UTC "YYYY-MM-DDTHH:MM:SSZ" without a date crate.
pub fn now_iso() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // civil_from_days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, (rem / 60) % 60, rem % 60)
}

fn norm_color(c: &str) -> Result<String, ToolError> {
    let h = c.trim().trim_start_matches('#');
    let h = if h.len() == 8 { &h[..6] } else { h };
    if h.len() == 6 && h.chars().all(|x| x.is_ascii_hexdigit()) {
        Ok(format!("#{}", h.to_ascii_uppercase()))
    } else {
        Err(ToolError::new("bad_color", format!("'{c}' is not #RRGGBB"), "e.g. #F9C1BD"))
    }
}

/// Location string for an AMS tray / external spool id.
/// Something about a loaded spool that needs the user (it isn't registered, or another spool is
/// registered in its place). Unlike a change, it stays true until fixed.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SpoolNotice {
    /// Inventory location ("ext:left", "ams0:3").
    pub location: String,
    pub material: String,
    pub color: String,
    /// The spool registered in that location, when it is the wrong one.
    pub spool: Option<String>,
    pub message: String,
}

/// A location the way people say it: "the left external holder", "AMS 1 slot 3".
pub fn location_name(loc: &str) -> String {
    match loc {
        "ext:left" => "the left external holder".into(),
        "ext:right" => "the right external holder".into(),
        l => match l.strip_prefix("ams").and_then(|r| r.split_once(':')) {
            Some((a, slot)) => format!("AMS {} slot {slot}", a.parse::<u32>().map(|n| n + 1).unwrap_or(1)),
            None => l.to_string(),
        },
    }
}

pub fn tray_location(ams_id: u32, slot: u32) -> String {
    match ams_id {
        254 => "ext:left".into(),
        255 => "ext:right".into(),
        a => format!("ams{a}:{}", slot + 1),
    }
}

/// Global tray id used in print mappings for a location ("ams0:3" → 2, "ext:left" → 254).
pub fn location_tray(loc: &str) -> Option<i32> {
    match loc {
        "ext:left" => Some(254),
        "ext:right" => Some(255),
        _ => {
            let rest = loc.strip_prefix("ams")?;
            let (a, s) = rest.split_once(':')?;
            let (a, s): (i32, i32) = (a.parse().ok()?, s.parse().ok()?);
            (1..=4).contains(&s).then_some(a * 4 + s - 1)
        }
    }
}

/// Bambu preset name for an RFID spool's product, if the profile store has it.
fn guess_preset(s: &Studio, sub_brand: &str) -> Option<String> {
    let cands = s.machine.list_presets("filament", sub_brand);
    let want = format!("Bambu {sub_brand} @BBL H2D");
    cands.iter().find(|n| **n == want).or_else(|| cands.iter().find(|n| n.starts_with(&want))).cloned()
}

// ------------------------------------------------------------------------------------------
// tools

use crate::inventory::Db;
use rusqlite::params;

fn dbe(e: rusqlite::Error) -> ToolError {
    ToolError::new("database_error", e.to_string(), "")
}

fn need_spool(db: &Db, id: &str) -> Result<Spool, ToolError> {
    db.spool(id)?.ok_or_else(|| {
        let ids: Vec<String> = db.spools(false).map(|v| v.into_iter().map(|s| s.id).collect()).unwrap_or_default();
        ToolError::new("not_found", format!("no spool '{id}'"), format!("spools: {}", ids.join(", ")))
    })
}

#[derive(Deserialize, JsonSchema)]
pub struct AddIn {
    /// "PLA", "PETG", "ASA"...
    pub material: String,
    /// "#RRGGBB"
    pub color: String,
    /// Default: brand + line + colour.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub brand: Option<String>,
    #[serde(default)]
    pub sub_brand: Option<String>,
    /// Bambu filament preset (default: guessed).
    #[serde(default)]
    pub preset: Option<String>,
    /// Full-spool filament weight in g (default 1000).
    #[serde(default)]
    pub net_weight_g: Option<f64>,
    /// What is left now (default: full).
    #[serde(default)]
    pub remaining_g: Option<f64>,
    /// Empty spool weight (Bambu ≈ 250 g, cardboard ≈ 150–200 g).
    #[serde(default)]
    pub tare_g: Option<f64>,
    #[serde(default)]
    pub cost_per_kg: Option<f64>,
    /// "ams0:1".."ams0:4", "ext:left", "ext:right", or free text.
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

fn t_add(s: &mut Studio, i: AddIn) -> Result<ToolOutput, ToolError> {
    let db = Db::open()?;
    let color = norm_color(&i.color)?;
    let material = i.material.trim().to_ascii_uppercase();
    let brand = i.brand.clone().unwrap_or_else(|| "Generic".into());
    let net = i.net_weight_g.unwrap_or(1000.0);
    if net <= 0.0 {
        return Err(ToolError::new("bad_weight", "net_weight_g must be > 0", ""));
    }
    let preset = i.preset.clone().or_else(|| {
        let line = i.sub_brand.clone().unwrap_or_else(|| format!("{material} Basic"));
        if brand.to_ascii_lowercase().contains("bambu") {
            guess_preset(s, &line)
        } else {
            s.machine.list_presets("filament", &format!("Generic {material}")).into_iter().next()
        }
    });
    let custom = i.name.clone().map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    let id = db.unique_spool_id(&format!("{material}-{}", custom.clone().unwrap_or_else(|| color.trim_start_matches('#').to_string())))?;
    let mut sp = Spool::blank(&id, &material, &color, net);
    sp.brand = brand;
    sp.sub_brand = i.sub_brand.filter(|x| !x.trim().is_empty());
    match custom {
        Some(n) => sp.name = n,
        None => sp.set_auto_name(),
    }
    sp.preset = preset;
    sp.tare_g = i.tare_g;
    sp.cost_per_kg = i.cost_per_kg;
    sp.notes = i.notes.unwrap_or_default();
    db.insert_spool(&sp)?;
    db.event(&id, "add", i.remaining_g.unwrap_or(net).clamp(0.0, net), None, "registered")?;
    if let Some(l) = &i.location {
        db.place(&id, l)?;
        s.spool_notices.retain(|n| n.location != *l);
    }
    Ok(ToolOutput::text(format!("Registered {}", need_spool(&db, &id)?.line())).with_data(json!({"id": id})))
}

#[derive(Deserialize, JsonSchema)]
pub struct ListIn {
    /// Filter by material ("PETG").
    #[serde(default)]
    pub material: Option<String>,
    #[serde(default)]
    pub include_archived: bool,
}

fn t_list(_s: &mut Studio, i: ListIn) -> Result<ToolOutput, ToolError> {
    let db = Db::open()?;
    let list: Vec<Spool> = db.spools(i.include_archived)?.into_iter().filter(|s| i.material.as_ref().is_none_or(|m| s.material.eq_ignore_ascii_case(m))).collect();
    if list.is_empty() {
        return Ok(ToolOutput::text("No spools registered. Use spool_sync to pick up the AMS's RFID spools, or spool_add.").with_data(json!([])));
    }
    let total: f64 = list.iter().map(|s| s.remaining_g).sum();
    let mut text = format!("{} spool(s), {:.2} kg in stock:\n", list.len(), total / 1000.0);
    for sp in &list {
        text.push_str(&format!("- {}\n", sp.line()));
    }
    let low: Vec<&str> = list.iter().filter(|s| s.low()).map(|s| s.id.as_str()).collect();
    if !low.is_empty() {
        text.push_str(&format!("Running low: {}\n", low.join(", ")));
    }
    let data: Vec<Value> = list
        .iter()
        .map(|s| {
            let mut v = serde_json::to_value(s).unwrap_or(Value::Null);
            if let Some(o) = v.as_object_mut() {
                o.remove("history");
            }
            v["percent"] = json!(s.percent());
            v
        })
        .collect();
    Ok(ToolOutput::text(text.trim_end()).with_data(Value::Array(data)))
}

#[derive(Deserialize, JsonSchema)]
pub struct UpdateIn {
    pub id: String,
    /// "" = automatic (brand + line + colour).
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub brand: Option<String>,
    #[serde(default)]
    pub sub_brand: Option<String>,
    #[serde(default)]
    pub material: Option<String>,
    #[serde(default)]
    pub net_weight_g: Option<f64>,
    /// Set what is left (logged as a manual correction).
    #[serde(default)]
    pub remaining_g: Option<f64>,
    #[serde(default)]
    pub tare_g: Option<f64>,
    #[serde(default)]
    pub cost_per_kg: Option<f64>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    /// Hide a used-up spool (history kept).
    #[serde(default)]
    pub archived: Option<bool>,
    /// Given values replace, 0 clears.
    #[serde(default)]
    pub calibration: Option<Calibration>,
}

fn t_update(s: &mut Studio, i: UpdateIn) -> Result<ToolOutput, ToolError> {
    let db = Db::open()?;
    let sp = need_spool(&db, &i.id)?;
    let color = i.color.as_deref().map(norm_color).transpose()?;
    let material = i.material.as_deref().map(|m| m.trim().to_ascii_uppercase()).filter(|m| !m.is_empty());
    db.conn
        .execute(
            "UPDATE spools SET color=COALESCE(?2,color), preset=COALESCE(?3,preset), brand=COALESCE(?4,brand),
             net_weight_g=COALESCE(?5,net_weight_g), tare_g=COALESCE(?6,tare_g), cost_per_kg=COALESCE(?7,cost_per_kg), notes=COALESCE(?8,notes),
             archived=COALESCE(?9,archived), material=COALESCE(?10,material) WHERE id=?1",
            params![i.id, color, i.preset, i.brand, i.net_weight_g, i.tare_g, i.cost_per_kg, i.notes, i.archived.map(|a| a as i64), material],
        )
        .map_err(dbe)?;
    if let Some(sb) = &i.sub_brand {
        let sb = sb.trim();
        db.conn.execute("UPDATE spools SET sub_brand=?2 WHERE id=?1", params![i.id, (!sb.is_empty()).then_some(sb)]).map_err(dbe)?;
    }
    // a typed name is kept; "" goes back to the automatic name
    if let Some(n) = &i.name {
        let n = n.trim();
        db.conn.execute("UPDATE spools SET name=?2, name_auto=?3 WHERE id=?1", params![i.id, n, n.is_empty() as i64]).map_err(dbe)?;
    }
    db.refresh_auto_name(&i.id)?;
    if i.archived == Some(true) {
        db.conn.execute("UPDATE spools SET location=NULL WHERE id=?1", [&i.id]).map_err(dbe)?;
    }
    if let Some(c) = &i.calibration {
        for (col, v) in [("flow_ratio", c.flow_ratio), ("pressure_advance", c.pressure_advance), ("max_vol_speed", c.max_volumetric_speed)] {
            if let Some(v) = v {
                let v = (v > 0.0).then_some(v);
                db.conn.execute(&format!("UPDATE spools SET {col}=?2 WHERE id=?1"), params![i.id, v]).map_err(dbe)?;
            }
        }
    }
    if let Some(r) = i.remaining_g {
        db.event(&i.id, "manual", r.max(0.0) - sp.remaining_g, None, "set remaining")?;
    }
    if let Some(l) = &i.location {
        db.place(&i.id, l)?;
        // the next sync re-checks; until then, a spool placed where a notice was answers it
        s.spool_notices.retain(|n| n.location != *l && n.spool.as_deref() != Some(i.id.as_str()));
    }
    Ok(ToolOutput::text(format!("Updated {}", need_spool(&db, &i.id)?.line())))
}

#[derive(Deserialize, JsonSchema)]
pub struct WeighIn {
    pub id: String,
    /// Scale reading of the spool with filament (g).
    pub gross_g: f64,
    /// Empty-spool weight if not already stored on the spool.
    #[serde(default)]
    pub tare_g: Option<f64>,
}

fn t_weigh(_s: &mut Studio, i: WeighIn) -> Result<ToolOutput, ToolError> {
    let db = Db::open()?;
    let sp = need_spool(&db, &i.id)?;
    if let Some(t) = i.tare_g {
        db.conn.execute("UPDATE spools SET tare_g=?2 WHERE id=?1", params![i.id, t]).map_err(dbe)?;
    }
    let tare = i.tare_g.or(sp.tare_g).ok_or_else(|| ToolError::new("tare_unknown", format!("'{}' has no empty-spool weight", i.id), "pass tare_g (weigh an empty spool of the same kind; Bambu reusable ≈ 250 g)"))?;
    let net = (i.gross_g - tare).max(0.0);
    let d = net - sp.remaining_g;
    db.event(&i.id, "weigh", d, None, &format!("scale {:.0} g − tare {:.0} g", i.gross_g, tare))?;
    Ok(ToolOutput::text(format!("Weighed: {} (change {d:+.0} g)", need_spool(&db, &i.id)?.line())))
}

#[derive(Deserialize, JsonSchema)]
pub struct UseIn {
    pub id: String,
    /// Grams consumed (positive), e.g. a print sliced elsewhere, a failed print, purging.
    pub grams: f64,
    /// Attribute it to a print (P-0007) so it shows in that print's trace.
    #[serde(default)]
    pub print_id: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

fn t_use(_s: &mut Studio, i: UseIn) -> Result<ToolOutput, ToolError> {
    let db = Db::open()?;
    need_spool(&db, &i.id)?;
    if let Some(p) = &i.print_id {
        let n: i64 = db.conn.query_row("SELECT COUNT(*) FROM prints WHERE id=?1", [p], |r| r.get(0)).map_err(dbe)?;
        if n == 0 {
            return Err(ToolError::new("not_found", format!("no print '{p}'"), "see print_history"));
        }
    }
    db.event(&i.id, if i.print_id.is_some() { "print" } else { "manual" }, -i.grams.abs(), i.print_id.as_deref(), i.note.as_deref().unwrap_or(""))?;
    Ok(ToolOutput::text(format!("Used {:.1} g: {}", i.grams.abs(), need_spool(&db, &i.id)?.line())))
}

#[derive(Deserialize, JsonSchema)]
pub struct IdIn {
    pub id: String,
}

fn t_spool_detail(_s: &mut Studio, i: IdIn) -> Result<ToolOutput, ToolError> {
    let db = Db::open()?;
    let sp = need_spool(&db, &i.id)?;
    let events = db.rows(
        "SELECT e.at, e.kind, e.delta_g, SUM(e.delta_g) OVER (ORDER BY e.id) AS remaining_g, e.print_id, p.job_name, e.note
         FROM spool_events e LEFT JOIN prints p ON p.id = e.print_id WHERE e.spool_id=?1 ORDER BY e.id",
        &[&i.id],
    )?;
    let mut text = format!("{}\nLedger ({} entries):", sp.line(), events.len());
    for e in events.iter().rev().take(40) {
        text.push_str(&format!(
            "\n  {} {:<6} {:+8.1} g → {:7.1} g{}{}",
            e["at"].as_str().unwrap_or("").get(..16).unwrap_or(""),
            e["kind"].as_str().unwrap_or(""),
            e["delta_g"].as_f64().unwrap_or(0.0),
            e["remaining_g"].as_f64().unwrap_or(0.0),
            e["print_id"].as_str().map(|p| format!("  [{p} {}]", e["job_name"].as_str().unwrap_or(""))).unwrap_or_default(),
            e["note"].as_str().filter(|n| !n.is_empty()).map(|n| format!("  {n}")).unwrap_or_default()
        ));
    }
    Ok(ToolOutput::text(text).with_data(json!({"spool": sp, "ledger": events})))
}

/// One AMS / external tray as reported by the printer.
#[derive(Clone, Debug)]
pub struct TrayReading {
    pub ams_id: u32,
    pub slot: u32,
    pub loaded: bool,
    pub material: String,
    pub sub_brand: String,
    pub color: String,
    pub filament_id: String,
    pub uuid: Option<String>,
    pub remain_g: Option<f64>,
    pub weight_g: Option<f64>,
}

/// Parse trays from the printer's raw report (`print.ams.ams[].tray[]`, `print.vir_slot[]` /
/// `print.vt_tray`).
pub fn read_trays(raw: &Value) -> Vec<TrayReading> {
    let p = raw.get("print").unwrap_or(raw);
    let num = |v: Option<&Value>| v.and_then(|x| x.as_f64().or_else(|| x.as_str().and_then(|s| s.parse().ok())));
    let parse = |ams_id: u32, t: &Value| -> Option<TrayReading> {
        let slot = num(t.get("id"))? as u32;
        let material = t.get("tray_type").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let uuid = t.get("tray_uuid").and_then(|v| v.as_str()).filter(|u| !u.is_empty() && u.chars().any(|c| c != '0')).map(String::from);
        let remain_g = num(t.get("remain_g")).filter(|_| num(t.get("remain")).unwrap_or(-1.0) >= 0.0);
        let (ams_id, slot) = if ams_id == u32::MAX { (slot, 0) } else { (ams_id, slot) };
        Some(TrayReading {
            ams_id,
            slot,
            loaded: !material.is_empty(),
            material,
            sub_brand: t.get("tray_sub_brands").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            color: t.get("tray_color").and_then(|v| v.as_str()).map(|c| format!("#{}", &c[..c.len().min(6)])).unwrap_or_default(),
            filament_id: t.get("tray_info_idx").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            uuid: uuid.clone(),
            remain_g: if uuid.is_some() { remain_g } else { None },
            weight_g: num(t.get("tray_weight")).filter(|w| *w > 0.0),
        })
    };
    let mut out = Vec::new();
    if let Some(units) = p.get("ams").and_then(|a| a.get("ams")).and_then(|a| a.as_array()) {
        for u in units {
            let aid = num(u.get("id")).unwrap_or(0.0) as u32;
            for t in u.get("tray").and_then(|t| t.as_array()).into_iter().flatten() {
                out.extend(parse(aid, t));
            }
        }
    }
    let ext: Vec<Value> = p.get("vir_slot").and_then(|v| v.as_array()).cloned().or_else(|| p.get("vt_tray").map(|v| vec![v.clone()])).unwrap_or_default();
    for t in &ext {
        out.extend(parse(u32::MAX, t));
    }
    out
}

/// Map a print's `mapping` entries ((ams_id << 8) | slot, or plain tray ids) to locations.
pub fn mapping_locations(raw: &Value) -> Vec<String> {
    raw.get("print")
        .and_then(|p| p.get("mapping"))
        .and_then(|m| m.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_u64())
                .filter_map(|v| match v {
                    0xFE00..=0xFEFF => Some("ext:left".to_string()),
                    0xFF00..=0xFFFE => Some("ext:right".to_string()),
                    0xFFFF => None,
                    v if v >= 0x100 => Some(tray_location((v >> 8) as u32, (v & 0xff) as u32)),
                    v => Some(tray_location((v / 4) as u32, (v % 4) as u32)),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Reconcile AMS readings with the inventory. AMS weight changes are attributed to the print
/// running on `printer` (if any). Returns human-readable changes, and what needs the user.
pub fn sync_from_trays(s: &Studio, db: &Db, trays: &[TrayReading], running_print: Option<&str>) -> Result<(Vec<String>, Vec<SpoolNotice>), ToolError> {
    let mut changes = Vec::new();
    let mut notices = Vec::new();
    for t in trays.iter().filter(|t| t.loaded) {
        let loc = tray_location(t.ams_id, t.slot);
        if let Some(uuid) = &t.uuid {
            let existing: Option<String> = db.conn.query_row("SELECT id FROM spools WHERE rfid_uuid=?1", [uuid], |r| r.get(0)).ok();
            let id = match existing {
                Some(id) => id,
                None => {
                    let line = if t.sub_brand.is_empty() { format!("{} Basic", t.material) } else { t.sub_brand.clone() };
                    let net = t.weight_g.unwrap_or(1000.0);
                    let id = db.unique_spool_id(&format!("{}-{}", t.material, t.color.trim_start_matches('#')))?;
                    let mut sp = Spool::blank(&id, &t.material.to_ascii_uppercase(), &t.color, net);
                    sp.brand = "Bambu Lab".into();
                    sp.sub_brand = Some(line.clone());
                    sp.preset = guess_preset(s, &line);
                    sp.filament_id = (!t.filament_id.is_empty()).then(|| t.filament_id.clone());
                    sp.set_auto_name();
                    sp.tare_g = Some(250.0);
                    sp.rfid_uuid = Some(uuid.clone());
                    db.insert_spool(&sp)?;
                    let g = t.remain_g.unwrap_or(net);
                    db.event(&id, "add", g, None, &format!("RFID spool found in {loc}"))?;
                    changes.push(format!("new RFID spool {id} in {loc}: {g:.0} g"));
                    id
                }
            };
            if let Some(rg) = t.remain_g {
                let d = rg - db.remaining(&id)?;
                if d.abs() >= 1.0 {
                    // a drop during a print belongs to that print; rises (refills, corrections) don't
                    let pid = if d < 0.0 { running_print } else { None };
                    db.event(&id, "ams", d, pid, &format!("AMS reading in {loc}"))?;
                    changes.push(format!("{id}: {d:+.0} g → {rg:.0} g (AMS){}", pid.map(|p| format!(" during {p}")).unwrap_or_default()));
                }
            }
            if db.spool(&id)?.and_then(|sp| sp.location).as_deref() != Some(loc.as_str()) {
                db.place(&id, &loc)?;
                changes.push(format!("{id} is now in {loc}"));
            }
        } else {
            let at: Option<(String, String)> = db.conn.query_row("SELECT id, material FROM spools WHERE location=?1 AND archived=0", [&loc], |r| Ok((r.get(0)?, r.get(1)?))).ok();
            match at {
                Some((_, m)) if m.eq_ignore_ascii_case(&t.material) => {}
                Some((id, m)) => notices.push(SpoolNotice {
                    location: loc.clone(),
                    material: t.material.clone(),
                    color: t.color.clone(),
                    spool: Some(id.clone()),
                    message: format!("{} holds {} {}, but spool '{id}' ({m}) is registered there", location_name(&loc), t.material, t.color),
                }),
                None => notices.push(SpoolNotice {
                    location: loc.clone(),
                    material: t.material.clone(),
                    color: t.color.clone(),
                    spool: None,
                    message: format!("{} {} in {} has no RFID and isn't registered", t.material, t.color, location_name(&loc)),
                }),
            }
        }
    }
    Ok((changes, notices))
}

#[derive(Deserialize, JsonSchema)]
pub struct SyncIn {
    /// Printer name (default printer when omitted).
    #[serde(default)]
    pub name: Option<String>,
}

fn t_sync(s: &mut Studio, i: SyncIn) -> Result<ToolOutput, ToolError> {
    let (text, changes) = sync_now(s, i.name.as_deref())?;
    Ok(ToolOutput::text(text).with_data(json!({"changes": changes, "notices": s.spool_notices})))
}

/// Keep the inventory in step with the printer while a live session runs: every `every`, read
/// the AMS and job state and record changes (no-op when no printer is configured).
pub fn spawn_tracker(shared: std::sync::Arc<std::sync::Mutex<Studio>>, every: std::time::Duration) {
    if crate::printer::load_book().printers.is_empty() {
        return;
    }
    let _ = std::thread::Builder::new().name("only3d-spool-tracker".into()).spawn(move || loop {
        std::thread::sleep(every);
        if let Ok(mut s) = shared.try_lock() {
            let before = s.spool_notices.clone();
            if let Ok((_, mut changes)) = sync_now(&mut s, None) {
                // standing issues are logged once, when they appear, not on every pass
                changes.extend(s.spool_notices.iter().filter(|n| !before.contains(n)).map(|n| n.message.clone()));
                if !changes.is_empty() {
                    let rev = s.revision;
                    let mut e = crate::JournalEntry::new(rev, crate::Actor::Script, "spool_sync", changes.join("; ").chars().take(160).collect(), true);
                    e.detail = changes.join("\n");
                    s.journal.push(e);
                }
            }
        }
    });
}

/// Read the printer and reconcile the inventory and print records. Returns (summary, changes).
pub fn sync_now(s: &mut Studio, name: Option<&str>) -> Result<(String, Vec<String>), ToolError> {
    let (raw, state, job, printer) = {
        let ps = crate::printer::session(s, name)?;
        let st = ps.client.status();
        (ps.client.raw_state(), st.gcode_state.as_str().to_string(), st.subtask_name.clone(), ps.cfg.name.clone())
    };
    let db = Db::open()?;
    let p = raw.get("print").cloned().unwrap_or(Value::Null);
    let task_id = p.get("task_id").and_then(|v| v.as_str().map(String::from).or_else(|| v.as_i64().map(|n| n.to_string()))).filter(|t| !t.is_empty() && t != "0");
    let busy = matches!(state.as_str(), "RUNNING" | "PREPARE" | "PAUSE");
    let mut changes = Vec::new();

    // 1. the job on the printer → a print record (linked to an only3d export when it is one)
    let mut running: Option<String> = None;
    if let Some(name) = job.filter(|n| !n.is_empty()) {
        let existing: Option<(String, String)> = match &task_id {
            Some(t) => db.conn.query_row("SELECT id, state FROM prints WHERE printer=?1 AND printer_task_id=?2", params![printer, t], |r| Ok((r.get(0)?, r.get(1)?))).ok(),
            None => None,
        }
        .or_else(|| db.conn.query_row("SELECT id, state FROM prints WHERE printer=?1 AND job_name=?2 AND ended_at IS NULL ORDER BY started_at DESC LIMIT 1", params![printer, name], |r| Ok((r.get(0)?, r.get(1)?))).ok());
        match existing {
            Some((pid, old)) => {
                // weight drops seen while it runs, or in the sync that sees it end, belong to it
                if busy || matches!(old.as_str(), "RUNNING" | "PREPARE" | "PAUSE") {
                    running = Some(pid.clone());
                }
                if let Some(t) = &task_id {
                    db.conn.execute("UPDATE prints SET printer_task_id=COALESCE(printer_task_id, ?2) WHERE id=?1", params![pid, t]).map_err(dbe)?;
                }
                if old != state {
                    let ended = (!busy).then(now_iso);
                    db.conn.execute("UPDATE prints SET state=?2, ended_at=COALESCE(ended_at, ?3) WHERE id=?1", params![pid, state, ended]).map_err(dbe)?;
                    changes.push(format!("{pid} '{name}': {old} → {state}"));
                }
            }
            None if busy => {
                // an only3d export started from Bambu Studio/Handy? (job names start with the file stem)
                let exported: Option<String> = db
                    .conn
                    .query_row("SELECT id FROM prints WHERE state='EXPORTED' AND ?1 LIKE job_name || '%' ORDER BY started_at DESC LIMIT 1", [&name], |r| r.get(0))
                    .ok();
                let pid = match exported {
                    Some(pid) => {
                        db.conn.execute("UPDATE prints SET state=?2, printer=?3, printer_task_id=?4, started_at=?5 WHERE id=?1", params![pid, state, printer, task_id, now_iso()]).map_err(dbe)?;
                        changes.push(format!("{pid}: only3d export '{name}' is now printing on {printer}"));
                        changes.extend(charge_estimate(&db, &pid)?);
                        pid
                    }
                    None => {
                        let pid = db.new_print_id()?;
                        db.conn
                            .execute("INSERT INTO prints (id, printer, printer_task_id, job_name, source, started_at, state) VALUES (?1,?2,?3,?4,'printer',?5,?6)", params![pid, printer, task_id, name, now_iso(), state])
                            .map_err(dbe)?;
                        changes.push(format!("{pid}: job '{name}' seen on {printer} ({state})"));
                        pid
                    }
                };
                // which spools feed it (from the printer's filament mapping)
                for (k, loc) in mapping_locations(&raw).into_iter().enumerate() {
                    let sp: Option<String> = db.conn.query_row("SELECT id FROM spools WHERE location=?1 AND archived=0", [&loc], |r| r.get(0)).ok();
                    db.conn.execute("INSERT OR IGNORE INTO print_filaments (print_id, slot, spool_id, tray) VALUES (?1,?2,?3,?4)", params![pid, k as i64 + 1, sp, loc]).map_err(dbe)?;
                }
                running = Some(pid);
            }
            None => {}
        }
    }
    // 2. trays (weight drops during a print are attributed to it)
    let trays = read_trays(&raw);
    let (tray_changes, notices) = sync_from_trays(s, &db, &trays, running.as_deref())?;
    changes.extend(tray_changes);
    let mut text = if changes.is_empty() { "Inventory already matches the printer.".to_string() } else { format!("Synced with {printer}:\n- {}", changes.join("\n- ")) };
    for n in &notices {
        let fix = match &n.spool {
            None => format!("register it with spool_add location=\"{}\" (then spool_weigh)", n.location),
            Some(id) => format!("move '{id}' with spool_update, or register the loaded spool"),
        };
        text.push_str(&format!("\nNeeds you: {}. To fix, {fix}.", n.message));
    }
    s.spool_notices = notices;
    let loaded: Vec<String> = trays.iter().filter(|t| t.loaded).map(|t| format!("{} {} {}{}", tray_location(t.ams_id, t.slot), t.material, t.color, t.remain_g.map(|g| format!(" {g:.0} g")).unwrap_or_else(|| " (no RFID)".into()))).collect();
    text.push_str(&format!("\nLoaded now: {}", loaded.join("; ")));
    Ok((text, changes))
}

#[allow(clippy::too_many_arguments)]
/// Record a print (or an export that may be printed elsewhere) with full traceability:
/// project version, per-object grams, slot → spool, and ledger entries charging the spools.
/// Record a print (or an export) of the last slice: its project version, objects and, per
/// filament it uses, the spool and tray (`trays`: 1-based slot → tray) with the grams expected.
pub fn record_print(s: &Studio, state: &str, job_name: &str, remote_file: Option<&str>, printer: Option<&str>, task_id: Option<&str>, trays: &[(u32, i32)], charge: bool) -> Result<String, ToolError> {
    let sl = s.last_slice.clone().ok_or_else(|| ToolError::new("not_sliced", "nothing sliced", "call slice first"))?;
    let db = Db::open()?;
    let path = s.project_path.as_deref().map(|p| std::fs::canonicalize(p).unwrap_or(p.to_path_buf()));
    let version = db.project_version(&s.project, path.as_deref(), if state == "EXPORTED" { "export" } else { "print" })?;
    let pid = db.new_print_id()?;
    let st = &sl.result.stats;
    let md5: String = {
        use sha2::Digest;
        sha2::Sha256::digest(sl.result.gcode.as_bytes()).iter().take(16).map(|b| format!("{b:02x}")).collect()
    };
    let settings = serde_json::to_string(&json!({"process": s.project.process, "printer": s.project.printer, "layer_height": sl.result.job.settings.layer_height, "infill": sl.result.job.settings.infill_density, "walls": sl.result.job.settings.wall_loops})).unwrap_or_default();
    db.conn
        .execute(
            "INSERT INTO prints (id, project_version_id, printer, printer_task_id, job_name, remote_file, gcode_md5, source, engine, settings, started_at, state, est_time_s, est_grams)
             VALUES (?1,?2,?3,?4,?5,?6,?7,'only3d',?8,?9,?10,?11,?12,?13)",
            params![pid, version, printer, task_id, job_name, remote_file, md5, sl.engine, settings, now_iso(), state, st.print_time_s, st.filament_g],
        )
        .map_err(dbe)?;
    let visible: Vec<&only3d_model::Object> = s.project.objects.iter().filter(|o| !o.hidden && o.plate == sl.plate).collect();
    for (k, o) in visible.iter().enumerate() {
        db.conn
            .execute(
                "INSERT INTO print_objects (print_id, object_id, object_name, filament_slot, identify_id, est_grams) VALUES (?1,?2,?3,?4,?5,?6)",
                params![pid, o.id, o.name, o.filament, 15 + k as i64, st.object_filament_g.get(k).copied().unwrap_or(0.0)],
            )
            .map_err(dbe)?;
    }
    // one row per filament: its slot, spool, tray and grams (the prime tower included)
    let per: Vec<(u32, f64)> = if st.filaments.len() > 1 { st.filaments.iter().map(|(f, _, g)| (*f as u32 + 1, *g)).collect() } else { vec![(visible.first().map(|o| o.filament).unwrap_or(1), st.filament_g)] };
    for (slot, grams) in per {
        let spool = s.project.filaments.get(slot as usize - 1).and_then(|f| f.spool.clone());
        let tray = trays.iter().find(|t| t.0 == slot).map(|t| t.1);
        let tray_loc = tray.map(|t| if t >= 254 { tray_location(t as u32, 0) } else { tray_location((t / 4) as u32, (t % 4) as u32) });
        db.conn.execute("INSERT INTO print_filaments (print_id, slot, spool_id, tray, est_grams) VALUES (?1,?2,?3,?4,?5)", params![pid, slot, spool, tray_loc, grams]).map_err(dbe)?;
    }
    if charge {
        charge_estimate(&db, &pid)?;
    }
    Ok(pid)
}

/// Charge a print's estimated grams to its spools that have no RFID (RFID spools are charged by
/// what the AMS measures while the print runs, so nothing is counted twice).
pub fn charge_estimate(db: &Db, print_id: &str) -> Result<Vec<String>, ToolError> {
    let rows = db.rows(
        "SELECT f.spool_id, f.est_grams, p.job_name FROM print_filaments f JOIN prints p ON p.id=f.print_id JOIN spools s ON s.id=f.spool_id
         WHERE f.print_id=?1 AND s.rfid_uuid IS NULL AND f.est_grams > 0",
        &[&print_id],
    )?;
    let mut out = Vec::new();
    for r in rows {
        let (sp, g) = (r["spool_id"].as_str().unwrap_or(""), r["est_grams"].as_f64().unwrap_or(0.0));
        db.event(sp, "print", -g, Some(print_id), &format!("estimate for '{}'", r["job_name"].as_str().unwrap_or("")))?;
        out.push(format!("{sp} −{g:.1} g (estimate)"));
    }
    Ok(out)
}

#[derive(Deserialize, JsonSchema)]
pub struct HistoryIn {
    #[serde(default)]
    pub limit: Option<usize>,
    /// Only prints that used this spool.
    #[serde(default)]
    pub spool: Option<String>,
    /// Only prints of this project id.
    #[serde(default)]
    pub project: Option<String>,
}

fn t_history(_s: &mut Studio, i: HistoryIn) -> Result<ToolOutput, ToolError> {
    let db = Db::open()?;
    let rows = db.rows(
        "SELECT p.id, p.started_at, p.job_name, p.state, p.source, p.printer, p.est_grams,
                (SELECT ROUND(SUM(grams),1) FROM print_usage u WHERE u.print_id=p.id) AS used_g,
                (SELECT GROUP_CONCAT(spool_id) FROM print_filaments f WHERE f.print_id=p.id) AS spools,
                v.project_id, v.id AS version
         FROM prints p LEFT JOIN project_versions v ON v.id = p.project_version_id
         WHERE (?1 IS NULL OR EXISTS (SELECT 1 FROM print_filaments f WHERE f.print_id=p.id AND f.spool_id=?1) OR EXISTS (SELECT 1 FROM spool_events e WHERE e.print_id=p.id AND e.spool_id=?1))
           AND (?2 IS NULL OR v.project_id=?2)
         ORDER BY p.started_at DESC LIMIT ?3",
        &[&i.spool, &i.project, &(i.limit.unwrap_or(30) as i64)],
    )?;
    if rows.is_empty() {
        return Ok(ToolOutput::text("No prints recorded yet.").with_data(json!([])));
    }
    let lines: Vec<String> = rows
        .iter()
        .map(|r| {
            format!(
                "{} {} {} — {} [{}]{}{}{}",
                r["id"].as_str().unwrap_or(""),
                r["started_at"].as_str().unwrap_or("").get(..16).unwrap_or(""),
                r["job_name"].as_str().unwrap_or(""),
                r["state"].as_str().unwrap_or(""),
                r["source"].as_str().unwrap_or(""),
                r["used_g"].as_f64().map(|g| format!(", used {g:.1} g")).or_else(|| r["est_grams"].as_f64().map(|g| format!(", est {g:.1} g"))).unwrap_or_default(),
                r["spools"].as_str().map(|s| format!(", spools {s}")).unwrap_or_default(),
                r["project_id"].as_str().map(|p| format!(", {p} v{}", r["version"])).unwrap_or_default()
            )
        })
        .collect();
    Ok(ToolOutput::text(lines.join("\n")).with_data(Value::Array(rows)))
}

fn t_print_detail(_s: &mut Studio, i: IdIn) -> Result<ToolOutput, ToolError> {
    let db = Db::open()?;
    let p = db.rows("SELECT p.*, v.project_id, v.hash AS version_hash, v.created_at AS version_created, pr.name AS project_name, pr.path AS project_path FROM prints p LEFT JOIN project_versions v ON v.id=p.project_version_id LEFT JOIN projects pr ON pr.id=v.project_id WHERE p.id=?1", &[&i.id])?;
    let p = p.into_iter().next().ok_or_else(|| ToolError::new("not_found", format!("no print '{}'", i.id), "see print_history"))?;
    let objects = db.rows("SELECT object_id, object_name, filament_slot, identify_id, ROUND(est_grams,2) AS est_grams, status FROM print_objects WHERE print_id=?1", &[&i.id])?;
    let filaments = db.rows("SELECT slot, spool_id, tray, ROUND(est_grams,2) AS est_grams FROM print_filaments WHERE print_id=?1 ORDER BY slot", &[&i.id])?;
    let events = db.rows("SELECT at, spool_id, kind, delta_g, note FROM spool_events WHERE print_id=?1 ORDER BY id", &[&i.id])?;
    let mut text = format!(
        "{} '{}' — {} [{}] started {}{}\n",
        i.id,
        p["job_name"].as_str().unwrap_or(""),
        p["state"].as_str().unwrap_or(""),
        p["source"].as_str().unwrap_or(""),
        p["started_at"].as_str().unwrap_or(""),
        p["ended_at"].as_str().map(|e| format!(", ended {e}")).unwrap_or_default()
    );
    if let Some(pid) = p["project_id"].as_str() {
        text.push_str(&format!("Project {} '{}' version {} ({}, {}){}\n", pid, p["project_name"].as_str().unwrap_or(""), p["project_version_id"], p["version_hash"].as_str().unwrap_or(""), p["version_created"].as_str().unwrap_or(""), p["project_path"].as_str().map(|x| format!(" at {x}")).unwrap_or_default()));
    }
    if let Some(g) = p["est_grams"].as_f64() {
        text.push_str(&format!("Estimate: {g:.1} g, {:.0} min\n", p["est_time_s"].as_f64().unwrap_or(0.0) / 60.0));
    }
    for o in &objects {
        text.push_str(&format!("  object {} '{}': {:.2} g (slot {}, Bambu id {}) {}\n", o["object_id"].as_str().unwrap_or(""), o["object_name"].as_str().unwrap_or(""), o["est_grams"].as_f64().unwrap_or(0.0), o["filament_slot"], o["identify_id"], o["status"].as_str().unwrap_or("")));
    }
    for f in &filaments {
        text.push_str(&format!("  slot {} ← {} ({})\n", f["slot"], f["spool_id"].as_str().unwrap_or("unknown spool"), f["tray"].as_str().unwrap_or("?")));
    }
    let used: f64 = events.iter().map(|e| -e["delta_g"].as_f64().unwrap_or(0.0)).sum::<f64>().max(0.0) + 0.0;
    text.push_str(&format!("Filament charged: {used:.1} g in {} ledger entr(ies)", events.len()));
    Ok(ToolOutput::text(text).with_data(json!({"print": p, "objects": objects, "filaments": filaments, "ledger": events})))
}

#[derive(Deserialize, JsonSchema)]
pub struct PrintUpdateIn {
    pub id: String,
    /// e.g. "FINISH", "FAILED", "CANCELLED".
    #[serde(default)]
    pub state: Option<String>,
    /// Outcome notes ("warped corner", "stringing on stems").
    #[serde(default)]
    pub notes: Option<String>,
    /// Per-object outcome, e.g. {"stem_3": "failed", "pin_2": "skipped"}.
    #[serde(default)]
    pub objects: Option<std::collections::BTreeMap<String, String>>,
}

fn t_print_update(_s: &mut Studio, i: PrintUpdateIn) -> Result<ToolOutput, ToolError> {
    let db = Db::open()?;
    let n = db.conn.execute("UPDATE prints SET state=COALESCE(?2,state), notes=COALESCE(?3,notes), ended_at=CASE WHEN ?2 IS NOT NULL AND ended_at IS NULL THEN ?4 ELSE ended_at END WHERE id=?1", params![i.id, i.state, i.notes, now_iso()]).map_err(dbe)?;
    if n == 0 {
        return Err(ToolError::new("not_found", format!("no print '{}'", i.id), "see print_history"));
    }
    for (obj, st) in i.objects.unwrap_or_default() {
        db.conn.execute("UPDATE print_objects SET status=?3 WHERE print_id=?1 AND object_id=?2", params![i.id, obj, st]).map_err(dbe)?;
    }
    Ok(ToolOutput::text(format!("Updated {}.", i.id)))
}

fn t_print_delete(_s: &mut Studio, i: IdIn) -> Result<ToolOutput, ToolError> {
    let state = Db::open()?.delete_print(&i.id)?;
    Ok(ToolOutput::text(format!("Deleted print record {} ({state}). Its number is not reused.", i.id)))
}

#[derive(Deserialize, JsonSchema)]
pub struct ReportIn {
    /// "spool" | "material" | "project" | "month" (default spool).
    #[serde(default)]
    pub group_by: Option<String>,
    /// Only usage on/after this ISO date, e.g. "2026-11-01".
    #[serde(default)]
    pub since: Option<String>,
}

fn t_report(_s: &mut Studio, i: ReportIn) -> Result<ToolOutput, ToolError> {
    let db = Db::open()?;
    let (key, join) = match i.group_by.as_deref().unwrap_or("spool") {
        "spool" => ("e.spool_id", ""),
        "material" => ("s.material", ""),
        "project" => ("COALESCE(pr.name || ' (' || pr.id || ')', 'outside only3d')", "LEFT JOIN prints p ON p.id=e.print_id LEFT JOIN project_versions v ON v.id=p.project_version_id LEFT JOIN projects pr ON pr.id=v.project_id"),
        "month" => ("SUBSTR(e.at,1,7)", ""),
        other => return Err(ToolError::new("bad_group", format!("unknown group_by '{other}'"), "spool | material | project | month")),
    };
    let sql = format!(
        "SELECT {key} AS grp, ROUND(-SUM(e.delta_g),1) AS grams, COUNT(DISTINCT e.print_id) AS prints,
                ROUND(-SUM(e.delta_g * COALESCE(s.cost_per_kg, 0)) / 1000.0, 2) AS cost
         FROM spool_events e JOIN spools s ON s.id=e.spool_id {join}
         WHERE e.delta_g < 0 AND e.kind IN ('print','ams','manual','weigh') AND (?1 IS NULL OR e.at >= ?1)
         GROUP BY grp ORDER BY grams DESC"
    );
    let rows = db.rows(&sql, &[&i.since])?;
    let total: f64 = rows.iter().map(|r| r["grams"].as_f64().unwrap_or(0.0)).sum();
    let mut text = format!("Filament used{}: {:.1} g total\n", i.since.as_ref().map(|d| format!(" since {d}")).unwrap_or_default(), total);
    for r in &rows {
        text.push_str(&format!("  {:<40} {:>8.1} g  {} print(s){}\n", r["grp"].as_str().unwrap_or("?"), r["grams"].as_f64().unwrap_or(0.0), r["prints"], r["cost"].as_f64().filter(|c| *c > 0.0).map(|c| format!("  ~{c:.2}")).unwrap_or_default()));
    }
    Ok(ToolOutput::text(text.trim_end()).with_data(Value::Array(rows)))
}

#[derive(Deserialize, JsonSchema)]
pub struct VersionsIn {
    /// Project id (default: the open project).
    #[serde(default)]
    pub project: Option<String>,
    /// Return the full document of this version id (to inspect or restore with set_project).
    #[serde(default)]
    pub version: Option<i64>,
}

fn t_versions(s: &mut Studio, i: VersionsIn) -> Result<ToolOutput, ToolError> {
    let db = Db::open()?;
    if let Some(v) = i.version {
        let doc: String = db.conn.query_row("SELECT document FROM project_versions WHERE id=?1", [v], |r| r.get(0)).map_err(|_| ToolError::new("not_found", format!("no version {v}"), "see project_versions"))?;
        let json: Value = serde_json::from_str(&doc).unwrap_or(Value::Null);
        return Ok(ToolOutput::text(format!("Version {v} (restore it with set_project {{\"project\": ...}}):\n{doc}")).with_data(json));
    }
    let pid = i.project.unwrap_or_else(|| s.project.id.clone());
    let rows = db.rows(
        "SELECT v.id, v.created_at, v.reason, v.hash, (SELECT COUNT(*) FROM prints p WHERE p.project_version_id=v.id) AS prints FROM project_versions v WHERE v.project_id=?1 ORDER BY v.id DESC",
        &[&pid],
    )?;
    let text = if rows.is_empty() {
        format!("No saved versions of {pid} yet (versions are recorded on save, export and print).")
    } else {
        rows.iter().map(|r| format!("v{} {} {} ({}), {} print(s)", r["id"], r["created_at"].as_str().unwrap_or("").get(..16).unwrap_or(""), r["reason"].as_str().unwrap_or(""), r["hash"].as_str().unwrap_or(""), r["prints"])).collect::<Vec<_>>().join("\n")
    };
    Ok(ToolOutput::text(text).with_data(Value::Array(rows)))
}

#[derive(Deserialize, JsonSchema)]
pub struct QueryIn {
    /// One SELECT / WITH. Tables: spools, spool_events(spool_id, at, kind, delta_g, print_id),
    /// prints(id, project_version_id, printer, job_name, state, started_at, est_grams),
    /// print_objects, print_filaments(print_id, slot, spool_id, tray), project_versions,
    /// projects; views spool_stock (+ remaining_g), print_usage. Columns:
    /// pragma_table_info('t').
    pub sql: String,
}

fn t_query(_s: &mut Studio, i: QueryIn) -> Result<ToolOutput, ToolError> {
    let q = i.sql.trim().trim_end_matches(';');
    let lower = q.to_ascii_lowercase();
    if !(lower.starts_with("select") || lower.starts_with("with")) || q.contains(';') {
        return Err(ToolError::new("read_only", "only a single SELECT/WITH statement is allowed", "use the spool_* and print_* tools to change data"));
    }
    let conn = Db::open_read_only()?;
    let rows = crate::inventory::query_json(&conn, q, &[])?;
    let shown: Vec<&Value> = rows.iter().take(200).collect();
    Ok(ToolOutput::text(format!("{} row(s){}:\n{}", rows.len(), if rows.len() > 200 { " (first 200)" } else { "" }, shown.iter().map(|r| r.to_string()).collect::<Vec<_>>().join("\n"))).with_data(Value::Array(rows)))
}

/// Spool assigned to a project filament slot (by id), with its current stock.
pub fn spool_for_slot(project: &only3d_model::Project, slot: usize) -> Option<Spool> {
    let id = project.filaments.get(slot)?.spool.clone()?;
    Db::open().ok()?.spool(&id).ok()?
}

pub fn register(r: &mut Registry) {
    r.add(def::<AddIn>("spool_add", "Add spool", "Register a spool (material, colour, weight, location). spool_sync registers Bambu RFID spools in the AMS.", false, true, t_add));
    r.add(def::<ListIn>("spool_list", "Spools", "Every spool with grams left (from its ledger), location (AMS slot / external / shelf), RFID and low-stock flags.", false, false, t_list));
    r.add(def::<IdIn>("spool_detail", "Spool ledger", "One spool's full ledger: every gram change with its source (print, AMS reading, weigh-in, manual) and the print it belongs to.", false, false, t_spool_detail));
    r.add(def::<UpdateIn>("spool_update", "Update spool", "Edit a spool (colour, preset, weights, cost, location, notes), set what is left, or archive it.", false, true, t_update));
    r.add(def::<WeighIn>("spool_weigh", "Weigh spool", "Update what is left from a kitchen-scale reading (gross − empty-spool weight). Best for spools without RFID.", false, true, t_weigh));
    r.add(def::<UseIn>("spool_use", "Use filament", "Record filament used outside only3d (a failed print, purging, a print sliced elsewhere), optionally for a print id.", false, true, t_use));
    r.add(def::<SyncIn>("spool_sync", "Sync with printer", "Read the printer: register RFID spools, update grams (charged to the running print), follow AMS locations.", false, true, t_sync));
    r.add(def::<HistoryIn>("print_history", "Print history", "Prints with id, job, state, grams used, spools and project version; filter by spool or project.", false, false, t_history));
    r.add(def::<IdIn>("print_detail", "Print trace", "One print (P-0007): its project version, objects with grams, slot → spool/tray, and ledger entries.", false, false, t_print_detail));
    r.add(def::<PrintUpdateIn>("print_update", "Print outcome", "Record how a print went: state, notes, per-object outcome (failed / skipped).", false, true, t_print_update));
    r.add(def::<IdIn>("print_delete", "Delete print record", "Delete a print record that never charged filament (test exports, duplicates). Prints that used filament stay.", false, true, t_print_delete));
    r.add(def::<ReportIn>("usage_report", "Usage report", "Filament used, grouped by spool, material, project or month (with cost when spools have a price).", false, false, t_report));
    r.add(def::<VersionsIn>("project_versions", "Project versions", "Snapshots of a project recorded at save/export/print; fetch one to inspect or restore it.", false, false, t_versions));
    r.add(def::<QueryIn>("inventory_query", "Query inventory", "Read-only SQL over the inventory database (spools, ledger, prints, objects, project versions) for questions the other tools don't answer.", false, false, t_query));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unregistered_spool_is_a_standing_notice_not_a_change() {
        let dir = std::env::temp_dir().join(format!("o3d_notice_test_{}_{}", std::process::id(), crate::now_ms()));
        let db = Db::open_at(&dir.join("t.db")).unwrap();
        let s = Studio::new();
        let white = TrayReading { ams_id: 254, slot: 0, loaded: true, material: "PETG".into(), sub_brand: String::new(), color: "#FFFFFF".into(), filament_id: String::new(), uuid: None, remain_g: None, weight_g: None };
        let (changes, notices) = sync_from_trays(&s, &db, std::slice::from_ref(&white), None).unwrap();
        assert!(changes.is_empty(), "nothing changed: {changes:?}");
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].location, "ext:left");
        assert!(notices[0].message.contains("left external holder"), "{}", notices[0].message);
        // the same reading again is the same notice (so the tracker logs it only once)
        assert_eq!(sync_from_trays(&s, &db, std::slice::from_ref(&white), None).unwrap().1, notices);
        // once a PETG spool is registered there, it's answered
        let mut sp = Spool::blank("petg-white-1", "PETG", "#FFFFFF", 1000.0);
        sp.name = "white".into();
        db.insert_spool(&sp).unwrap();
        db.place("petg-white-1", "ext:left").unwrap();
        assert!(sync_from_trays(&s, &db, &[white], None).unwrap().1.is_empty());
        assert_eq!(location_name("ams0:3"), "AMS 1 slot 3");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rfid_spools_get_automatic_names() {
        let dir = std::env::temp_dir().join(format!("o3d_rfid_name_test_{}_{}", std::process::id(), crate::now_ms()));
        let db = Db::open_at(&dir.join("t.db")).unwrap();
        let s = Studio::new();
        let pink = TrayReading { ams_id: 0, slot: 3, loaded: true, material: "PETG".into(), sub_brand: "PETG Translucent".into(), color: "#F9C1BD".into(), filament_id: "GFG01".into(), uuid: Some("ABC".into()), remain_g: Some(876.0), weight_g: Some(1000.0) };
        sync_from_trays(&s, &db, &[pink], None).unwrap();
        let sp = db.spools(false).unwrap().pop().unwrap();
        assert_eq!((sp.name.as_str(), sp.name_auto), ("Bambu PETG Translucent Pink", true));
        assert!(sp.line().contains("Bambu PETG Translucent Pink #F9C1BD"), "{}", sp.line());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parses_h2d_trays_mapping_and_locations() {
        let raw = json!({"print": {"mapping": [65024, 3], "ams": {"ams": [{"id": "0", "tray": [
            {"id": "0", "state": 10},
            {"id": "2", "tray_type": "PETG", "tray_color": "7F7E83FF", "tray_sub_brands": "PETG Basic", "tray_info_idx": "GFG00",
             "tray_uuid": "22B51C48C2744FBDB84258EF068CC7E3", "remain": 38, "remain_g": 381, "tray_weight": "1000"}]}]},
            "vir_slot": [{"id": "254", "tray_type": "PETG", "tray_color": "FFFFFFFF", "tray_uuid": "00000000000000000000000000000000", "remain": 0, "remain_g": 0, "tray_weight": "0"}]}});
        let t = read_trays(&raw);
        assert_eq!(t.len(), 3);
        let grey = t.iter().find(|x| x.uuid.is_some()).unwrap();
        assert_eq!(tray_location(grey.ams_id, grey.slot), "ams0:3");
        assert_eq!(grey.remain_g, Some(381.0));
        assert_eq!(grey.color, "#7F7E83");
        let ext = t.iter().find(|x| x.ams_id == 254).unwrap();
        assert!(ext.uuid.is_none() && ext.remain_g.is_none());
        assert_eq!(mapping_locations(&raw), vec!["ext:left".to_string(), "ams0:4".to_string()]);
        assert_eq!(location_tray("ams0:3"), Some(2));
        assert_eq!(location_tray("ext:left"), Some(254));
        assert!(now_iso().len() == 20 && now_iso().starts_with("20"));
    }
}
