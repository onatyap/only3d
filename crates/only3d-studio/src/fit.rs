//! Fit calibration from real prints: print a coupon (a bar of holes from nominal to +0.5 mm,
//! each labelled, and a pin), try the pin in each hole, and record which one slides (slip) and
//! which one holds (press). The slip offset is saved per printer and filament and added to every
//! `hole` from then on, so fits come out right the first time.

use rusqlite::params;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::inventory::Db;
use crate::tools::{def, Registry, ToolError, ToolOutput};
use crate::{Actor, Studio};

#[derive(Deserialize, JsonSchema)]
pub struct FitIn {
    /// coupon | record | show
    pub action: String,
    /// coupon: hole/pin mm (default 5)
    #[serde(default)]
    pub nominal: Option<f64>,
    /// record: offset of the hole the pin slides in
    #[serde(default)]
    pub slip: Option<f64>,
    /// record: offset of the hole the pin holds in
    #[serde(default)]
    pub press: Option<f64>,
    /// default: filament 1's preset
    #[serde(default)]
    pub filament: Option<String>,
}

pub const OFFSETS: [f64; 6] = [0.0, 0.1, 0.2, 0.3, 0.4, 0.5];

fn t_fit(s: &mut Studio, i: FitIn) -> Result<ToolOutput, ToolError> {
    let printer = s.project.printer.machine.clone();
    let filament = i.filament.clone().or_else(|| s.project.filaments.first().map(|f| f.preset.clone())).unwrap_or_default();
    match i.action.as_str() {
        "coupon" => coupon(s, i.nominal.unwrap_or(5.0)),
        "record" => {
            if i.slip.is_none() && i.press.is_none() {
                return Err(ToolError::new("missing_fit", "record needs slip and/or press", "the offset printed next to the hole, e.g. {\"slip\": 0.2, \"press\": 0.1}"));
            }
            let db = Db::open()?;
            let now = crate::spools::now_iso();
            for (fit, v) in [("slip", i.slip), ("press", i.press)] {
                if let Some(v) = v {
                    if !(-0.5..=1.0).contains(&v) {
                        return Err(ToolError::new("bad_offset", format!("{fit} {v} mm is outside −0.5…1.0"), "use the offset printed by the hole"));
                    }
                    db.conn
                        .execute(
                            "INSERT INTO fits (printer, filament, fit, offset, nominal, at) VALUES (?1,?2,?3,?4,?5,?6)
                             ON CONFLICT(printer, filament, fit) DO UPDATE SET offset=?4, nominal=?5, at=?6",
                            params![printer, filament, fit, v, i.nominal, now],
                        )
                        .map_err(|e| ToolError::new("database_error", e.to_string(), ""))?;
                }
            }
            // re-evaluate with the new fit
            s.fit_cache = None;
            s.changed("fit");
            Ok(ToolOutput::text(format!(
                "Saved for {filament} on {printer}: {}{}. Holes on this filament now get {} mm extra.",
                i.slip.map(|v| format!("slip +{v} mm")).unwrap_or_default(),
                i.press.map(|v| format!("{}press +{v} mm", if i.slip.is_some() { ", " } else { "" })).unwrap_or_default(),
                s.calibrated_fit()
            ))
            .with_data(json!({"printer": printer, "filament": filament, "slip": i.slip, "press": i.press})))
        }
        "show" => {
            let db = Db::open()?;
            let rows = db.rows("SELECT printer, filament, fit, offset, nominal, at FROM fits ORDER BY printer, filament, fit", &[])?;
            let now = s.calibrated_fit();
            let text = if rows.is_empty() {
                "No fits calibrated yet. Print a coupon (fit {\"action\": \"coupon\"}), try the pin in each hole and record what you find.".to_string()
            } else {
                let lines: Vec<String> = rows.iter().map(|r| format!("{} · {}: {} +{} mm ({})", r["printer"].as_str().unwrap_or(""), r["filament"].as_str().unwrap_or(""), r["fit"].as_str().unwrap_or(""), r["offset"], r["at"].as_str().unwrap_or("").get(..10).unwrap_or(""))).collect();
                format!("{}\nThis project's holes get +{now} mm ({filament}).", lines.join("\n"))
            };
            Ok(ToolOutput::text(text).with_data(json!({"fits": rows, "current": now})))
        }
        other => Err(ToolError::new("bad_action", format!("'{other}'"), "coupon | record | show")),
    }
}

/// The coupon: a bar with one hole per offset (nominal + offset, the offset engraved beside it)
/// and a pin of the nominal size, on the active plate. Holes here carry an explicit `fit` so the
/// coupon tests raw sizes whatever is calibrated.
fn coupon(s: &mut Studio, nominal: f64) -> Result<ToolOutput, ToolError> {
    if !(1.0..=20.0).contains(&nominal) {
        return Err(ToolError::new("bad_nominal", format!("{nominal} mm"), "pick 1–20 mm"));
    }
    let pitch = nominal + 7.0;
    let n = OFFSETS.len() as f64;
    let (len, width, thick) = (n * pitch + 4.0, nominal + 14.0, 6.0);
    let mut cuts = Vec::new();
    for (k, off) in OFFSETS.iter().enumerate() {
        let x = -len / 2.0 + 2.0 + pitch * (k as f64 + 0.5);
        cuts.push(json!({"type": "hole", "d": nominal + off, "depth": thick + 1.0, "fit": 0, "at": [x, 3.0, thick]}));
        let label = if *off == 0.0 { "0".to_string() } else { format!(".{}", (off * 10.0).round() as i64) };
        cuts.push(json!({"type": "text", "text": label, "size": 3.2, "h": 1.0, "at": [x, -width / 2.0 + 3.5, thick - 0.6]}));
    }
    let mut children = vec![json!({"type": "box", "size": [len, width, thick], "corner_radius": 2})];
    children.extend(cuts);
    let bar = json!({"id": "fit_coupon", "name": format!("fit coupon {nominal} mm"), "shape": {"type": "difference", "children": children}});
    let pin = json!({"id": "fit_pin", "name": format!("fit pin {nominal} mm"), "shape": {"type": "cylinder", "d": nominal, "h": 14, "chamfer_top": 0.6}});
    let mut ids = Vec::new();
    for o in [bar, pin] {
        let out = s.call("add_object", o, Actor::Script)?;
        if let Some(id) = out.data.get("id").and_then(|v| v.as_str()) {
            ids.push(id.to_string());
        }
    }
    Ok(ToolOutput::text(format!(
        "Added {} on plate {}: holes of {nominal} mm + 0, .1, .2, .3, .4, .5 (engraved by each) and a {nominal} mm pin. Print them in the filament to calibrate, push the pin into each hole, then fit {{\"action\": \"record\", \"slip\": <slides freely>, \"press\": <holds firmly>}}.",
        ids.join(" and "),
        s.plate
    ))
    .with_data(json!({"ids": ids, "offsets": OFFSETS})))
}

pub fn register(r: &mut Registry) {
    r.add(def::<FitIn>(
        "fit",
        "Fit calibration",
        "Fit calibration: `coupon` adds a labelled hole bar (+0…+0.5 mm) and a pin to print; `record` the offsets that slide (slip) and hold (press). Slip is added to every `hole`.",
        true,
        true,
        t_fit,
    ));
}
