//! What the printer has loaded, as the print preflight shows it: every AMS and external tray under
//! the label the printer's screen and Bambu Studio use ("A1" is the first AMS's first slot, "ext
//! left" the left spool holder), its filament by name, what's left, and the nozzle it feeds. And
//! how well a tray fits a project filament: the material must match; another product line or
//! another colour is the user's call, never a silent pick.

use only3d_bambu::{AmsTray, PrinterStatus};
use serde::Serialize;

use crate::spools::Spool;

/// A new filament slot's display colour: a project that still has it never chose one.
pub const UNSET_COLOR: &str = "#D9CBB5";

/// Colours closer than this (CIE76 ΔE) are the same filament colour; past it, another shade.
const SAME_COLOUR_DE: f32 = 12.0;

/// A Bambu spool's filament weight, for turning the AMS's percentage into grams.
const BAMBU_SPOOL_G: f64 = 1000.0;

/// One tray, as people see it.
#[derive(Clone, Debug, Serialize)]
pub struct Tray {
    /// The id `print` takes in `tray` / `trays` (AMS unit × 4 + slot; 254/255 the spool holders).
    pub tray: i32,
    /// "A1".."D4", "HT-A", "ext left", "ext right", "ext".
    pub label: String,
    pub loaded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    /// Product line the spool's RFID reports ("PLA Wood").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<String>,
    /// "#RRGGBB".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// "Bambu PLA Wood Clay Brown": the inventory spool's name, else made from what the tray reports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The inventory spool sitting in this tray.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<u8>,
    /// Grams left: the inventory spool's, else estimated from `percent` on a 1 kg Bambu spool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grams_left: Option<f64>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub grams_estimated: bool,
    /// "left" or "right" on a two-nozzle printer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nozzle: Option<&'static str>,
}

impl Tray {
    /// "A1 (tray 0)": the label people know, and the id `print` takes.
    pub fn named(&self) -> String {
        format!("{} (tray {})", self.label, self.tray)
    }

    /// "Bambu PLA Wood Clay Brown #995F11, 73% (~730 g)", or "empty".
    pub fn holds(&self) -> String {
        if !self.loaded {
            return "empty".into();
        }
        let what = self.name.clone().or_else(|| self.material.clone()).unwrap_or_else(|| "unknown filament".into());
        let colour = self.color.as_deref().map(|c| format!(" {c}")).unwrap_or_default();
        let left = match (self.percent, self.grams_left) {
            (_, Some(g)) if !self.grams_estimated => format!(", {g:.0} g left"),
            (Some(p), Some(g)) => format!(", {p}% (~{g:.0} g)"),
            (Some(p), None) => format!(", {p}%"),
            _ => String::new(),
        };
        format!("{what}{colour}{left}")
    }
}

/// The label a tray has on the printer's screen.
pub fn label(st: &PrinterStatus, t: &AmsTray) -> String {
    let dual = st.nozzles.len() >= 2;
    match t.global_id {
        254 => "ext left".into(),
        255 if dual => "ext right".into(),
        255 => "ext".into(),
        _ => match t.ams_id {
            a @ 0..=25 => format!("{}{}", (b'A' + a as u8) as char, t.slot_id + 1),
            a @ 128..=153 => format!("HT-{}", (b'A' + (a - 128) as u8) as char),
            a => format!("AMS {a} slot {}", t.slot_id + 1),
        },
    }
}

/// "#995F11" from the printer's "995F11FF".
fn hex(c: &str) -> Option<String> {
    let h = c.trim().trim_start_matches('#');
    (h.len() >= 6 && h.is_char_boundary(6) && h[..6].chars().all(|c| c.is_ascii_hexdigit())).then(|| format!("#{}", h[..6].to_ascii_uppercase()))
}

/// Every tray the printer reports (AMS units, then the spool holders), with the inventory spool
/// each one holds.
pub fn trays(st: &PrinterStatus, spools: &[Spool]) -> Vec<Tray> {
    st.ams
        .iter()
        .flat_map(|a| a.trays.iter())
        .chain(st.external_spools.iter())
        .map(|t| {
            let at = crate::spools::tray_location(t.ams_id, t.slot_id);
            let spool = spools.iter().find(|s| !s.archived && s.location.as_deref() == Some(at.as_str())).filter(|_| t.loaded);
            let color = t.color.as_deref().and_then(hex);
            let bambu = t.tray_info_idx.as_deref().is_some_and(|i| i.starts_with("GF")) && t.sub_brand.is_some();
            let name = spool.map(|s| s.name.clone()).or_else(|| {
                let material = t.filament_type.as_deref().filter(|_| t.loaded)?;
                Some(crate::spool_names::auto_name(if bambu { "Bambu" } else { "" }, t.sub_brand.as_deref(), material, color.as_deref().unwrap_or(""), t.tray_info_idx.as_deref()))
            });
            let (grams_left, grams_estimated) = match (spool, t.remain_percent) {
                (Some(s), _) => (Some(s.remaining_g), false),
                (None, Some(p)) if bambu => (Some(p as f64 / 100.0 * BAMBU_SPOOL_G), true),
                _ => (None, false),
            };
            Tray {
                tray: t.global_id as i32,
                label: label(st, t),
                loaded: t.loaded,
                material: t.filament_type.clone().filter(|_| t.loaded),
                line: t.sub_brand.clone().filter(|_| t.loaded),
                color: color.filter(|_| t.loaded),
                name,
                spool: spool.map(|s| s.id.clone()),
                percent: t.remain_percent.filter(|_| t.loaded),
                grams_left: grams_left.filter(|_| t.loaded),
                grams_estimated,
                nozzle: crate::printer::tray_extruder(st, t.global_id as i32).map(|e| if e == 0 { "left" } else { "right" }),
            }
        })
        .collect()
}

/// The material a filament preset prints: "PLA" from "Bambu PLA Wood @BBL H2D".
pub fn material(preset: &str) -> String {
    preset.split_whitespace().nth(1).unwrap_or("").to_ascii_uppercase()
}

/// The Bambu product line a preset is for ("PLA Wood"); `None` for generic and other makers'
/// presets, which any spool of the material fits.
pub fn line(preset: &str) -> Option<String> {
    let name = preset.split(" @").next().unwrap_or(preset).trim();
    name.strip_prefix("Bambu ").map(|l| l.trim().to_string()).filter(|l| !l.is_empty())
}

/// Whether the tray holds the preset's material at all.
pub fn material_fits(preset: &str, t: &Tray) -> bool {
    let want = material(preset);
    let have = t.material.clone().unwrap_or_default().to_ascii_uppercase();
    t.loaded && (want.is_empty() || (!have.is_empty() && (have.contains(&want) || want.contains(&have))))
}

/// What differs between a project filament and a tray of the same material: another product
/// line, or another colour. `None` when it's the same filament as far as the printer can tell.
pub fn difference(preset: &str, color: &str, t: &Tray) -> Option<String> {
    let mut out = Vec::new();
    if let (Some(want), Some(have)) = (line(preset), t.line.as_deref()) {
        if !want.eq_ignore_ascii_case(have.trim()) {
            out.push(format!("the project uses {want}, the tray holds {have}"));
        }
    }
    let chosen = !color.trim().eq_ignore_ascii_case(UNSET_COLOR);
    if let (true, Some(have)) = (chosen, t.color.as_deref()) {
        if crate::spool_names::colour_distance(color, have).is_some_and(|d| d > SAME_COLOUR_DE) {
            out.push(format!("the colour differs: the project's is {}, the tray's {have}", hex(color).unwrap_or_else(|| color.to_string())));
        }
    }
    (!out.is_empty()).then(|| out.join("; "))
}

/// The loaded trays that hold a project filament's material, best first: the same product line,
/// then the nearest colour (a known same line before an unknown one, another line last).
pub fn ranked<'a>(trays: &'a [Tray], preset: &str, color: &str) -> Vec<&'a Tray> {
    let want_line = line(preset);
    let chosen = !color.trim().eq_ignore_ascii_case(UNSET_COLOR);
    let key = |t: &Tray| {
        let (other_line, unknown_line) = match (&want_line, &t.line) {
            (Some(w), Some(h)) => (!w.eq_ignore_ascii_case(h.trim()), false),
            (Some(_), None) => (false, true),
            _ => (false, false),
        };
        let d = if chosen { t.color.as_deref().and_then(|c| crate::spool_names::colour_distance(color, c)).unwrap_or(50.0) } else { 0.0 };
        (other_line, d, unknown_line)
    };
    let mut fit: Vec<&Tray> = trays.iter().filter(|t| material_fits(preset, t)).collect();
    fit.sort_by(|a, b| {
        let (ka, kb) = (key(a), key(b));
        ka.0.cmp(&kb.0).then(ka.1.total_cmp(&kb.1)).then(ka.2.cmp(&kb.2))
    });
    fit
}

/// The loaded tray that fits a project filament best (`ranked`'s first), not one already taken
/// by another filament.
pub fn best(trays: &[Tray], preset: &str, color: &str, taken: &[i32]) -> Option<i32> {
    ranked(trays, preset, color).into_iter().find(|t| !taken.contains(&t.tray)).map(|t| t.tray)
}

/// The printer's trays as lines for a listing.
pub fn listing(trays: &[Tray]) -> String {
    trays
        .iter()
        .map(|t| format!("  {}{}: {}", t.named(), t.nozzle.map(|n| format!(", {n} nozzle")).unwrap_or_default(), t.holds()))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use only3d_bambu::{AmsUnit, NozzleStatus};

    fn tray(ams: u32, slot: u32, ty: Option<&str>, sub: Option<&str>, color: &str, idx: Option<&str>, pct: Option<u8>) -> AmsTray {
        AmsTray {
            ams_id: ams,
            slot_id: slot,
            global_id: if ams >= 254 { ams } else { ams * 4 + slot },
            loaded: ty.is_some(),
            filament_type: ty.map(str::to_string),
            sub_brand: sub.map(str::to_string),
            color: Some(color.into()),
            tray_info_idx: idx.map(str::to_string),
            remain_percent: pct,
            ..Default::default()
        }
    }

    /// The printer the smart-frames session printed to: an H2D whose AMS feeds the right nozzle.
    fn h2d() -> PrinterStatus {
        PrinterStatus {
            nozzles: vec![NozzleStatus { index: 0, ..Default::default() }, NozzleStatus { index: 1, ..Default::default() }],
            ams: vec![AmsUnit {
                id: 0,
                extruder: Some(0),
                trays: vec![
                    tray(0, 0, Some("PLA"), Some("PLA Wood"), "995F11FF", Some("GFA16"), Some(73)),
                    tray(0, 1, None, None, "00000000", None, None),
                    tray(0, 2, Some("PETG"), Some("PETG Basic"), "7F7E83FF", Some("GFG00"), Some(37)),
                    tray(0, 3, Some("PETG"), Some("PETG Translucent"), "F9C1BDFF", Some("GFG01"), Some(87)),
                ],
                ..Default::default()
            }],
            external_spools: vec![tray(254, 0, Some("PETG"), None, "FFFFFFFF", None, None), tray(255, 0, None, None, "00000000", None, None)],
            ..Default::default()
        }
    }

    #[test]
    fn trays_read_as_on_the_printer_screen() {
        let t = trays(&h2d(), &[]);
        let labels: Vec<&str> = t.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(labels, ["A1", "A2", "A3", "A4", "ext left", "ext right"]);
        assert_eq!(t[0].name.as_deref(), Some("Bambu PLA Wood Clay Brown"));
        assert_eq!(t[0].color.as_deref(), Some("#995F11"));
        assert_eq!(t[0].nozzle, Some("right"));
        assert_eq!(t[4].nozzle, Some("left"));
        assert_eq!(t[0].holds(), "Bambu PLA Wood Clay Brown #995F11, 73% (~730 g)");
        assert_eq!(t[1].holds(), "empty");
        assert_eq!(t[0].named(), "A1 (tray 0)");
    }

    #[test]
    fn another_colour_or_line_is_a_difference_not_a_match() {
        let t = trays(&h2d(), &[]);
        let wood = "Bambu PLA Wood @BBL H2D";
        // the smart-frames plate: birch wanted, clay brown loaded
        assert!(material_fits(wood, &t[0]));
        assert!(difference(wood, "#B9A98A", &t[0]).unwrap().contains("colour differs"));
        // the same colour is no difference; a project that never chose a colour has none
        assert_eq!(difference(wood, "#995F11", &t[0]), None);
        assert_eq!(difference(wood, UNSET_COLOR, &t[0]), None);
        // PLA Basic wanted, PLA Wood loaded: another line
        assert!(difference("Bambu PLA Basic @BBL H2D", "#995F11", &t[0]).unwrap().contains("PLA Basic"));
        // generic presets take any line
        assert_eq!(difference("Generic PLA @BBL H2D", "#995F11", &t[0]), None);
        assert!(!material_fits(wood, &t[2]), "PETG is not PLA");
        assert!(!material_fits(wood, &t[1]), "an empty tray fits nothing");
    }

    #[test]
    fn the_best_tray_is_the_same_line_then_the_nearest_colour() {
        let t = trays(&h2d(), &[]);
        assert_eq!(best(&t, "Bambu PLA Wood @BBL H2D", "#B9A98A", &[]), Some(0), "the only PLA");
        assert_eq!(best(&t, "Bambu PETG Translucent @BBL H2D", "#F9C1BD", &[]), Some(3));
        assert_eq!(best(&t, "Bambu PETG Basic @BBL H2D", "#7F7E83", &[]), Some(2), "its line and colour");
        assert_eq!(best(&t, "Bambu PETG Basic @BBL H2D", "#FFFFFF", &[]), Some(254), "the white PETG of unknown line, not grey PETG Basic");
        assert_eq!(best(&t, "Bambu PETG Basic @BBL H2D", "#F9C1BD", &[254]), Some(2), "a known other line (Translucent) comes last, even in the right colour");
        assert_eq!(best(&t, "Generic PETG @BBL H2D", "#F9C1BD", &[]), Some(3), "any line: the pink one");
        assert_eq!(best(&t, "Generic PETG @BBL H2D", "#FFFFFF", &[254]), Some(3), "light pink is nearer white than grey");
        assert_eq!(best(&t, "Bambu ABS @BBL H2D", "#000000", &[]), None);
    }

    #[test]
    fn every_tray_of_the_material_is_a_choice_best_first() {
        let t = trays(&h2d(), &[]);
        let order = |preset: &str, color: &str| ranked(&t, preset, color).iter().map(|x| x.tray).collect::<Vec<_>>();
        // its line and colour, then the white PETG of unknown line, then another line
        assert_eq!(order("Bambu PETG Basic @BBL H2D", "#7F7E83"), [2, 254, 3]);
        assert_eq!(order("Bambu PLA Wood @BBL H2D", "#B9A98A"), [0]);
        // empty trays and other materials are never choices
        assert!(order("Bambu ABS @BBL H2D", "#000000").is_empty());
    }
}
