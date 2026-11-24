//! The AMS dialog, built around what the user is doing rather than `printer_ams`'s arguments:
//! - a slot: every slot of every unit as a strip to pick from, what the picked one holds, Load /
//!   Unload / Read tag (greyed out with the reason when the printer would refuse), and setting
//!   what it holds from a filament preset, a spool of the inventory or a bare material;
//! - drying a unit: filament, temperature and time, with the plan `printer_ams` makes for them
//!   shown as they change, then one button that starts it.
//!
//! Every button is a `printer_ams` call (or `spool_update`), so agents can do all of it too.

use eframe::egui::{self, Align, Color32, Layout, RichText, Sense, Ui};
use serde_json::{json, Map, Value};

use crate::app::{App, Dialog};
use crate::panels::muted;
use crate::theme::{self, pal};
use crate::worker::Kind;

/// Materials offered when a slot is set by hand (Bambu Studio's common types).
const MATERIALS: [&str; 14] = ["PLA", "PETG", "ABS", "ASA", "TPU", "PA", "PC", "PET-CF", "PLA-CF", "PA-CF", "PETG-CF", "PVA", "HIPS", "PPS"];

/// A nozzle range when a material is picked by hand (what Bambu's generic presets use).
fn material_temps(m: &str) -> (u32, u32) {
    match m.to_ascii_uppercase().as_str() {
        "PLA" | "PLA-CF" | "PVA" => (190, 230),
        "PETG" | "PETG-CF" | "PET-CF" => (220, 260),
        "ABS" | "ASA" | "HIPS" => (240, 270),
        "TPU" => (200, 240),
        "PA" | "PA-CF" | "PC" => (260, 290),
        "PPS" => (300, 320),
        _ => (190, 250),
    }
}

pub enum AmsDialog {
    Slot(SlotForm),
    Dry(DryForm),
}

/// Where "what it holds" comes from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Preset,
    Spool,
    Material,
}

pub struct SlotForm {
    /// The unit (254/255 = an external holder) and the slot in it.
    pub ams_id: u32,
    pub slot: u32,
    /// The editor was filled from this slot (it is again when another slot is picked).
    seeded: Option<(u32, u32)>,
    pub source: Source,
    pub preset: String,
    pub spool: String,
    pub material: String,
    pub color: Color32,
    pub temp_min: u32,
    pub temp_max: u32,
    /// The action waiting for its answer ("load", "set_slot"...).
    pub running: Option<String>,
    pub result: Option<Result<String, String>>,
    /// Sent once the running call succeeds (the spool's new place after `set_slot`).
    pub then: Option<(String, Value)>,
}

pub struct DryForm {
    pub ams_id: u32,
    /// The filament to dry for; "" = the loaded one that needs the lowest temperature.
    pub filament: String,
    /// `None` = what the filament's Bambu preset recommends (shown from the plan).
    pub temp: Option<u32>,
    pub hours: Option<u32>,
    pub rotate: bool,
    /// The arguments of the last plan asked for, and when they last changed.
    pub asked: Option<String>,
    changed_at: f64,
    pub plan: Option<Result<Value, String>>,
    pub running: Option<String>,
    pub result: Option<Result<String, String>>,
}

impl AmsDialog {
    pub fn slot(ams_id: u32, slot: u32) -> Self {
        AmsDialog::Slot(SlotForm {
            ams_id,
            slot,
            seeded: None,
            source: Source::Preset,
            preset: String::new(),
            spool: String::new(),
            material: "PLA".into(),
            color: Color32::WHITE,
            temp_min: 190,
            temp_max: 230,
            running: None,
            result: None,
            then: None,
        })
    }

    pub fn dry(ams_id: u32) -> Self {
        AmsDialog::Dry(DryForm { ams_id, filament: String::new(), temp: None, hours: None, rotate: false, asked: None, changed_at: 0.0, plan: None, running: None, result: None })
    }

    /// The answer to the call a button made.
    pub fn done(&mut self, r: Result<String, String>) -> Option<(String, Value)> {
        let (running, result, then) = match self {
            AmsDialog::Slot(f) => (&mut f.running, &mut f.result, f.then.take()),
            AmsDialog::Dry(f) => {
                // a failed start may have changed what the AMS will do: ask for the plan again
                if r.is_err() {
                    f.asked = None;
                }
                (&mut f.running, &mut f.result, None)
            }
        };
        *running = None;
        let ok = r.is_ok();
        *result = Some(r);
        then.filter(|_| ok)
    }
}

// ----------------------------------------------------------------------------------------------
// reading the printer's status

/// One slot as the status reports it.
struct SlotView {
    ams_id: u32,
    slot: u32,
    label: String,
    tray: Value,
}

fn units(d: &Value) -> Vec<Value> {
    d.get("ams").and_then(Value::as_array).cloned().unwrap_or_default()
}

fn unit(d: &Value, ams_id: u32) -> Option<Value> {
    units(d).into_iter().find(|u| u.get("id").and_then(Value::as_u64) == Some(u64::from(ams_id)))
}

fn dual(d: &Value) -> bool {
    d.get("nozzles").and_then(Value::as_array).is_some_and(|n| n.len() >= 2)
}

fn unit_name(u: &Value) -> String {
    u.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from).unwrap_or_else(|| u.get("id").and_then(Value::as_u64).unwrap_or(0).to_string())
}

/// Every slot: the AMS units' in order, then the external holders with a spool or a setting.
fn slots(d: &Value) -> Vec<(String, Vec<SlotView>)> {
    let mut out = Vec::new();
    for u in units(d) {
        let name = unit_name(&u);
        let id = u.get("id").and_then(Value::as_u64).unwrap_or(0) as u32;
        let row = u
            .get("trays")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|t| {
                let slot = t.get("slot_id").and_then(Value::as_u64).unwrap_or(0) as u32;
                SlotView { ams_id: id, slot, label: if name.starts_with("HT") { name.clone() } else { format!("{name}{}", slot + 1) }, tray: t.clone() }
            })
            .collect();
        out.push((name, row));
    }
    let ext: Vec<SlotView> = d
        .get("external_spools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|t| {
            let g = t.get("global_id").and_then(Value::as_u64).unwrap_or(255) as u32;
            let label = match g {
                254 => "ext L",
                255 if dual(d) => "ext R",
                _ => "ext",
            };
            SlotView { ams_id: g, slot: 0, label: label.into(), tray: t.clone() }
        })
        .collect();
    if !ext.is_empty() {
        out.push(("ext".into(), ext));
    }
    out
}

fn find_slot(d: &Value, ams_id: u32, slot: u32) -> Option<SlotView> {
    slots(d).into_iter().flat_map(|(_, r)| r).find(|s| s.ams_id == ams_id && s.slot == slot)
}

/// The nozzle a unit feeds, "left" / "right", on a two-nozzle printer.
fn feeds(d: &Value, ams_id: u32) -> Option<&'static str> {
    if !dual(d) {
        return None;
    }
    match ams_id {
        254 => Some("left"),
        255 => Some("right"),
        // firmware numbering: extruder 0 is the right nozzle, 1 the left
        _ => unit(d, ams_id).and_then(|u| u.get("extruder").and_then(Value::as_u64)).map(|e| if e == 0 { "right" } else { "left" }),
    }
}

/// A nozzle: its side, and the slot loaded into it (`None` when none is or it isn't reported).
struct Nozzle {
    side: Option<String>,
    holds: Option<(u32, u32)>,
}

fn nozzles(d: &Value) -> Vec<Nozzle> {
    d.get("nozzles")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|n| Nozzle {
            side: n.get("side").and_then(Value::as_str).map(String::from),
            holds: n.get("loaded_tray").filter(|t| !t.is_null()).map(|t| (t["ams_id"].as_u64().unwrap_or(0) as u32, t["slot_id"].as_u64().unwrap_or(0) as u32)),
        })
        .collect()
}

fn busy(d: &Value) -> bool {
    matches!(d.get("gcode_state").and_then(Value::as_str).map(str::to_ascii_uppercase).as_deref(), Some("RUNNING" | "PAUSE" | "PREPARE" | "SLICING"))
}

fn tray_color(t: &Value) -> Option<Color32> {
    let loaded = t.get("loaded").and_then(Value::as_bool).unwrap_or(false);
    t.get("color").and_then(Value::as_str).filter(|_| loaded).map(|c| {
        let c = crate::scene::parse_hex(c);
        Color32::from_rgb(c[0], c[1], c[2])
    })
}

/// "PETG Basic", the material, or "empty".
fn tray_name(t: &Value) -> String {
    let s = |k: &str| t.get(k).and_then(Value::as_str).filter(|v| !v.trim().is_empty());
    if t.get("reading").and_then(Value::as_bool) == Some(true) {
        return "reading the tag…".into();
    }
    if t.get("loaded").and_then(Value::as_bool) != Some(true) {
        return "empty".into();
    }
    s("sub_brand").or(s("filament_type")).unwrap_or("unknown filament").to_string()
}

fn hex(c: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
}

/// Dark text on a light swatch, light on a dark one.
fn ink(c: Color32) -> Color32 {
    let l = 0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32;
    if l > 150.0 {
        Color32::from_gray(20)
    } else {
        Color32::from_gray(240)
    }
}

/// A unit's temperature and humidity, "32 °C  31% RH".
fn climate(u: &Value) -> String {
    [
        u.get("temperature").and_then(Value::as_f64).map(|c| format!("{c:.0} °C")),
        u.get("humidity_percent").and_then(Value::as_u64).map(|h| format!("{h}% RH")).or_else(|| u.get("humidity_level").and_then(Value::as_u64).map(|l| format!("humidity {l}/5"))),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("  ")
}

/// A colour swatch; empty slots are an outline with a slash.
fn swatch(ui: &mut Ui, size: egui::Vec2, color: Option<Color32>, text: &str, selected: bool, sense: Sense) -> egui::Response {
    let p = pal();
    let (r, resp) = ui.allocate_exact_size(size, sense);
    let painter = ui.painter();
    match color {
        Some(c) => {
            painter.rect_filled(r, 4.0, c);
            painter.rect_stroke(r, 4.0, egui::Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
        }
        None => {
            painter.rect_stroke(r, 4.0, egui::Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
            painter.line_segment([r.left_bottom() + egui::vec2(5.0, -5.0), r.right_top() + egui::vec2(-5.0, 5.0)], egui::Stroke::new(1.0, p.border));
        }
    }
    if selected {
        painter.rect_stroke(r.expand(2.5), 6.0, egui::Stroke::new(2.0, p.accent), egui::StrokeKind::Outside);
    } else if resp.hovered() {
        painter.rect_stroke(r.expand(2.0), 5.0, egui::Stroke::new(1.0, p.muted), egui::StrokeKind::Outside);
    }
    if !text.is_empty() {
        let fg = color.map(ink).unwrap_or(p.muted);
        painter.text(r.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::monospace(11.0), fg);
    }
    resp
}

fn result_line(ui: &mut Ui, r: &Result<String, String>) {
    let p = pal();
    match r {
        Ok(t) => {
            ui.add(egui::Label::new(RichText::new(format!("✓ {}", t.lines().next().unwrap_or("Done"))).size(11.5).color(p.success)).wrap());
        }
        Err(e) => {
            let (what, hint) = e.split_once("\n→ ").unwrap_or((e.as_str(), ""));
            ui.add(egui::Label::new(RichText::new(what).size(11.5).color(p.error)).wrap());
            if !hint.is_empty() {
                ui.add(egui::Label::new(muted(format!("→ {hint}")).size(11.5)).wrap());
            }
        }
    }
}

/// A form label in a column of its own.
fn cell(ui: &mut Ui, w: f32, label: &str) {
    ui.allocate_ui_with_layout(egui::vec2(w, 22.0), Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_width(w);
        ui.label(RichText::new(label).size(12.5).color(pal().muted));
    });
}

/// A button that is greyed out with the reason as its tooltip.
fn action(ui: &mut Ui, label: &str, why_not: Option<String>, running: bool) -> bool {
    let b = egui::Button::new(RichText::new(if running { format!("{label}…") } else { label.to_string() }).size(12.5));
    let r = ui.add_enabled(why_not.is_none() && !running, b);
    match why_not {
        Some(w) => {
            r.on_disabled_hover_text(w);
            false
        }
        None => r.clicked(),
    }
}

impl App {
    /// Open the slot dialog on the slot a nozzle holds, else the first one with filament.
    pub fn open_ams_slot(&mut self) {
        let Some(d) = self.printer_data.clone() else {
            self.tool_form_with("printer_ams", json!({ "action": "load", "ams_id": 0, "slot": 0 }));
            return;
        };
        let held = nozzles(&d).into_iter().find_map(|n| n.holds);
        let all: Vec<SlotView> = slots(&d).into_iter().flat_map(|(_, r)| r).collect();
        let first = all.iter().find(|s| s.tray.get("loaded").and_then(Value::as_bool) == Some(true)).or(all.first()).map(|s| (s.ams_id, s.slot));
        match held.or(first) {
            Some((a, s)) => self.dialog = Some(Dialog::Ams(AmsDialog::slot(a, s))),
            None => self.tool_form_with("printer_ams", json!({ "action": "load", "ams_id": 0, "slot": 0 })),
        }
    }

    /// The dialog's contents; false closes it.
    pub fn ams_dialog_ui(&mut self, ui: &mut Ui, dlg: &mut AmsDialog) -> bool {
        let Some(d) = self.printer_data.clone() else {
            ui.label(muted("The printer's status hasn't been read yet."));
            return !ui.button("Close").clicked();
        };
        match dlg {
            AmsDialog::Slot(f) => self.slot_ui(ui, &d, f),
            AmsDialog::Dry(f) => self.dry_ui(ui, &d, f),
        }
    }

    fn slot_ui(&mut self, ui: &mut Ui, d: &Value, f: &mut SlotForm) -> bool {
        let p = pal();
        let mut keep = true;
        ui.set_width(500.0);
        ui.label(RichText::new("AMS").family(theme::medium()).size(14.0));
        ui.add_space(6.0);

        // the strip: every slot, to pick one
        let rows = slots(d);
        for (name, row) in &rows {
            ui.horizontal(|ui| {
                ui.add_sized([40.0, 26.0], egui::Label::new(RichText::new(name).monospace().size(11.5).color(p.muted)));
                for s in row {
                    let sel = s.ams_id == f.ams_id && s.slot == f.slot;
                    let short = if s.ams_id >= 254 { s.label.trim_start_matches("ext ").to_string() } else { (s.slot + 1).to_string() };
                    let short = if s.label.starts_with("HT") || s.label == "ext" { String::new() } else { short };
                    let r = swatch(ui, egui::vec2(44.0, 26.0), tray_color(&s.tray), &short, sel, Sense::click()).on_hover_text(format!("{} · {}", s.label, tray_name(&s.tray)));
                    if r.clicked() && !sel {
                        f.ams_id = s.ams_id;
                        f.slot = s.slot;
                        f.result = None;
                    }
                    ui.add_space(4.0);
                }
                if let Some(u) = unit(d, row.first().map(|s| s.ams_id).unwrap_or(u32::MAX)) {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(climate(&u)).monospace().size(11.0).color(p.muted));
                    });
                }
            });
            ui.add_space(2.0);
        }
        let Some(s) = find_slot(d, f.ams_id, f.slot) else {
            ui.add_space(6.0);
            ui.label(muted("The printer no longer reports this slot. Pick another above."));
            ui.add_space(6.0);
            return !ui.button("Close").clicked();
        };
        let t = &s.tray;
        let loaded = t.get("loaded").and_then(Value::as_bool).unwrap_or(false);
        let reading = t.get("reading").and_then(Value::as_bool).unwrap_or(false);
        let external = s.ams_id >= 254;
        let side = feeds(d, s.ams_id);
        let noz = nozzles(d);
        let holder = noz.iter().find(|n| n.holds == Some((s.ams_id, s.slot)));
        // seed the editor from what the slot reports, once per slot
        if f.seeded != Some((s.ams_id, s.slot)) {
            f.seeded = Some((s.ams_id, s.slot));
            if let Some(c) = tray_color(t) {
                f.color = c;
            }
            if let Some(m) = t.get("filament_type").and_then(Value::as_str).filter(|m| !m.is_empty()) {
                f.material = m.to_string();
            }
            let (lo, hi) = material_temps(&f.material);
            f.temp_min = t.get("nozzle_temp_min").and_then(Value::as_u64).map(|v| v as u32).filter(|v| *v > 0).unwrap_or(lo);
            f.temp_max = t.get("nozzle_temp_max").and_then(Value::as_u64).map(|v| v as u32).filter(|v| *v > 0).unwrap_or(hi);
            let loc = only3d_studio::spools::tray_location(s.ams_id, s.slot);
            match self.spools().into_iter().find(|sp| sp.location.as_deref() == Some(loc.as_str())) {
                Some(sp) => {
                    f.spool = sp.id;
                    f.source = Source::Spool;
                }
                None => f.spool.clear(),
            }
        }

        // the slot
        ui.add_space(8.0);
        egui::Frame::new().fill(p.bg).stroke(egui::Stroke::new(1.0, p.border_variant)).corner_radius(6).inner_margin(10).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                swatch(ui, egui::vec2(44.0, 44.0), tray_color(t), "", false, Sense::hover());
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&s.label).monospace().size(13.0).color(p.muted));
                        ui.label(RichText::new(tray_name(t)).family(theme::medium()).size(14.0));
                        if reading {
                            ui.add(egui::Spinner::new().size(12.0).color(p.accent));
                        }
                    });
                    let mut facts: Vec<String> = Vec::new();
                    if loaded {
                        let g = |k: &str| t.get(k).and_then(Value::as_str).filter(|v| !v.is_empty());
                        if let (Some(ty), true) = (g("filament_type"), g("sub_brand").is_some()) {
                            facts.push(ty.to_string());
                        }
                        if let Some(c) = t.get("color").and_then(Value::as_str) {
                            facts.push(format!("#{}", &c[..6.min(c.len())]));
                        }
                        if let (Some(lo), Some(hi)) = (t.get("nozzle_temp_min").and_then(Value::as_u64), t.get("nozzle_temp_max").and_then(Value::as_u64)) {
                            facts.push(format!("{lo}–{hi} °C"));
                        }
                        if let Some(r) = t.get("remain_percent").and_then(Value::as_u64) {
                            facts.push(format!("{r}% left"));
                        }
                    }
                    if let Some(sd) = side {
                        facts.push(format!("feeds the {sd} nozzle"));
                    }
                    ui.label(muted(facts.join(" · ")).size(11.5));
                    ui.horizontal(|ui| {
                        if let Some(Nozzle { side, .. }) = holder {
                            let at = side.as_deref().map(|s| format!("in the {s} nozzle")).unwrap_or_else(|| "in the nozzle".into());
                            theme::chip(ui, &at, p.accent, theme::soft(p.accent));
                        }
                        if t.get("tray_info_idx").and_then(Value::as_str).is_some_and(|i| i.starts_with("GF")) && t.get("sub_brand").and_then(Value::as_str).is_some_and(|b| !b.is_empty()) {
                            theme::chip(ui, "Bambu RFID", p.muted, p.hover);
                        }
                        let loc = only3d_studio::spools::tray_location(s.ams_id, s.slot);
                        if let Some(sp) = self.spools().into_iter().find(|sp| sp.location.as_deref() == Some(loc.as_str())) {
                            theme::chip(ui, &format!("spool {} · {:.0} g", sp.id, sp.remaining_g), p.muted, p.hover).on_hover_text(&sp.name);
                        }
                    });
                });
            });
        });

        // load, unload, read the tag: greyed out with the printer's reason
        ui.add_space(8.0);
        let printing = busy(d).then(|| "The printer is printing; this waits until it is idle.".to_string());
        let other_in_nozzle = noz.iter().find(|n| match side {
            Some(sd) => n.side.as_deref() == Some(sd),
            None => true,
        });
        let other_in_nozzle = other_in_nozzle.and_then(|n| n.holds).filter(|x| *x != (s.ams_id, s.slot));
        let label_of = |a: u32, sl: u32| find_slot(d, a, sl).map(|x| x.label).unwrap_or_else(|| "another slot".into());
        let mut call: Option<(String, Value)> = None;
        ui.horizontal(|ui| {
            let run = |what: &str| f.running.as_deref() == Some(what);
            let load_label = match side {
                Some(sd) => format!("Load into {sd} nozzle"),
                None => "Load".to_string(),
            };
            let why = printing.clone().or_else(|| (!loaded).then(|| "No filament in this slot.".into())).or_else(|| holder.map(|_| "Already loaded.".into()));
            if action(ui, &load_label, why, run("load")) {
                call = Some(("load".into(), json!({ "action": "load", "ams_id": s.ams_id, "slot": s.slot })));
            }
            // two-nozzle printers say which slot each nozzle holds; others may not, so they may try
            let knows = dual(d) || noz.iter().any(|n| n.holds.is_some());
            let why = printing.clone().or_else(|| match (holder, other_in_nozzle) {
                (Some(_), _) => None,
                (None, Some((a, sl))) => Some(format!("The nozzle holds {}: unload it from there.", label_of(a, sl))),
                (None, None) if knows => Some("This slot's filament isn't in a nozzle.".into()),
                (None, None) => None,
            });
            if action(ui, "Unload", why, run("unload")) {
                call = Some(("unload".into(), json!({ "action": "unload", "ams_id": s.ams_id, "slot": s.slot })));
            }
            let why = if external {
                Some("The external holders have no RFID reader.".to_string())
            } else {
                printing
                    .clone()
                    .or_else(|| (!loaded).then(|| "No filament at the slot's inlet, so the AMS won't turn the spool.".into()))
                    .or_else(|| noz.iter().find(|n| n.holds.is_some() && (side.is_none() || n.side.as_deref() == side)).map(|_| "Filament is in the nozzle this AMS feeds: unload it first.".into()))
                    .or_else(|| reading.then(|| "Reading it now.".into()))
            };
            if action(ui, "Read tag", why, run("read_rfid")) {
                call = Some(("read_rfid".into(), json!({ "action": "read_rfid", "ams_id": s.ams_id, "slot": s.slot })));
            }
        });

        // what it holds
        ui.add_space(10.0);
        theme::section_label(ui, "What it holds");
        if t.get("sub_brand").and_then(Value::as_str).is_some_and(|b| !b.is_empty()) && !external {
            ui.add(egui::Label::new(muted("The spool's RFID tag set this. A setting made here lasts until the tag is read again.").size(11.0)).wrap());
        }
        ui.horizontal(|ui| {
            for (src, text) in [(Source::Preset, "Filament preset"), (Source::Spool, "My spool"), (Source::Material, "Material")] {
                if ui.selectable_label(f.source == src, RichText::new(text).size(12.0)).clicked() {
                    f.source = src;
                }
            }
        });
        ui.add_space(4.0);
        const LW: f32 = 92.0;
        let row = |ui: &mut Ui, label: &str, add: &mut dyn FnMut(&mut Ui)| {
            ui.horizontal(|ui| {
                cell(ui, LW, label);
                add(ui);
            });
        };
        let spools = self.spools();
        match f.source {
            Source::Preset => {
                if !self.presets.contains_key("filament") {
                    self.refresh_presets("filament");
                }
                let mut picked = None;
                ui.horizontal(|ui| {
                    cell(ui, LW, "Preset");
                    let cur = if f.preset.is_empty() { "Pick a filament preset".to_string() } else { f.preset.clone() };
                    picked = self.preset_picker(ui, "ams_slot_preset", "filament", &cur, 300.0);
                });
                if let Some(pk) = picked {
                    f.preset = pk;
                }
                row(ui, "Colour", &mut |ui| {
                    egui::color_picker::color_edit_button_srgba(ui, &mut f.color, egui::color_picker::Alpha::Opaque);
                    ui.label(muted(hex(f.color)).monospace().size(11.0));
                });
                ui.add(egui::Label::new(muted("The preset gives the material, Bambu's filament id and the nozzle range.").size(11.0)).wrap());
            }
            Source::Spool => {
                row(ui, "Spool", &mut |ui| {
                    let cur = spools.iter().find(|sp| sp.id == f.spool);
                    let text = cur.map(|sp| format!("{} · {:.0} g", sp.name, sp.remaining_g)).unwrap_or_else(|| if spools.is_empty() { "No spools in the inventory".into() } else { "Pick a spool".into() });
                    egui::ComboBox::from_id_salt("ams_slot_spool").selected_text(RichText::new(text).size(12.5)).width(300.0).height(360.0).show_ui(ui, |ui| {
                        for sp in &spools {
                            ui.horizontal(|ui| {
                                let c = crate::scene::parse_hex(&sp.color);
                                swatch(ui, egui::vec2(14.0, 14.0), Some(Color32::from_rgb(c[0], c[1], c[2])), "", false, Sense::hover());
                                let at = sp.location.as_deref().map(|l| format!("  ({l})")).unwrap_or_default();
                                if ui.selectable_label(f.spool == sp.id, RichText::new(format!("{} · {:.0} g{at}", sp.name, sp.remaining_g)).size(12.0)).clicked() {
                                    f.spool = sp.id.clone();
                                    let c = crate::scene::parse_hex(&sp.color);
                                    f.color = Color32::from_rgb(c[0], c[1], c[2]);
                                    f.material = sp.material.clone();
                                    (f.temp_min, f.temp_max) = material_temps(&sp.material);
                                    ui.close();
                                }
                            });
                        }
                    });
                });
                ui.add(egui::Label::new(muted("Sets the slot from the spool's preset (or material) and colour, and records the spool as being in this slot, so prints are charged to it.").size(11.0)).wrap());
            }
            Source::Material => {
                row(ui, "Material", &mut |ui| {
                    egui::ComboBox::from_id_salt("ams_slot_material").selected_text(RichText::new(&f.material).size(12.5)).width(140.0).show_ui(ui, |ui| {
                        for m in MATERIALS {
                            if ui.selectable_label(f.material == m, m).clicked() {
                                f.material = m.into();
                                (f.temp_min, f.temp_max) = material_temps(m);
                            }
                        }
                    });
                });
                row(ui, "Colour", &mut |ui| {
                    egui::color_picker::color_edit_button_srgba(ui, &mut f.color, egui::color_picker::Alpha::Opaque);
                    ui.label(muted(hex(f.color)).monospace().size(11.0));
                });
                row(ui, "Nozzle", &mut |ui| {
                    ui.add(egui::DragValue::new(&mut f.temp_min).range(150..=350).suffix(" °C"));
                    ui.label(muted("to"));
                    ui.add(egui::DragValue::new(&mut f.temp_max).range(150..=350).suffix(" °C"));
                });
            }
        }
        ui.add_space(4.0);
        let set: Option<Value> = match f.source {
            Source::Preset if !f.preset.is_empty() => Some(json!({ "preset": f.preset, "color": hex(f.color) })),
            Source::Spool => spools.iter().find(|sp| sp.id == f.spool).map(|sp| {
                let mut a = Map::new();
                match sp.preset.as_deref().filter(|p| !p.is_empty()) {
                    Some(pr) => a.insert("preset".into(), json!(pr)),
                    None => a.insert("filament_type".into(), json!(sp.material)),
                };
                if let Some(id) = sp.filament_id.as_deref().filter(|i| !i.is_empty()) {
                    a.insert("tray_info_idx".into(), json!(id));
                }
                a.insert("color".into(), json!(hex(f.color)));
                Value::Object(a)
            }),
            Source::Material => Some(json!({ "filament_type": f.material, "color": hex(f.color), "min_temp": f.temp_min.min(f.temp_max), "max_temp": f.temp_max.max(f.temp_min), "tray_info_idx": "" })),
            _ => None,
        };
        let why = match (&set, f.source) {
            (None, Source::Preset) => Some("Pick a filament preset first."),
            (None, _) => Some("Pick a spool first."),
            _ => None,
        };
        ui.horizontal(|ui| {
            let running = f.running.as_deref() == Some("set_slot");
            let b = ui.add_enabled(why.is_none() && !running, theme::primary_button(if running { format!("Setting {}…", s.label) } else { format!("Set {}", s.label) }));
            if let Some(w) = why {
                b.on_disabled_hover_text(w);
            } else if b.clicked() {
                let mut args = set.clone().unwrap_or_default();
                args["action"] = json!("set_slot");
                args["ams_id"] = json!(s.ams_id);
                args["slot"] = json!(s.slot);
                if f.source == Source::Spool {
                    f.then = Some(("spool_update".into(), json!({ "id": f.spool, "location": only3d_studio::spools::tray_location(s.ams_id, s.slot) })));
                }
                call = Some(("set_slot".into(), args));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    keep = false;
                }
                if let Some(u) = unit(d, s.ams_id).filter(|u| u.get("drying").is_some()) {
                    if theme::bracket_button(ui, &format!("dry AMS {}…", unit_name(&u))).clicked() {
                        self.dialog = Some(Dialog::Ams(AmsDialog::dry(s.ams_id)));
                        keep = false;
                    }
                }
            });
        });
        if let Some(r) = &f.result {
            ui.add_space(4.0);
            result_line(ui, r);
        }
        if let Some((what, args)) = call {
            f.running = Some(what);
            f.result = None;
            if f.running.as_deref() != Some("set_slot") {
                f.then = None;
            }
            self.worker.call("printer_ams", args, Kind::Ams);
        }
        keep
    }

    fn dry_ui(&mut self, ui: &mut Ui, d: &Value, f: &mut DryForm) -> bool {
        let p = pal();
        let mut keep = true;
        ui.set_width(460.0);
        let Some(u) = unit(d, f.ams_id) else {
            ui.label(muted("The printer no longer reports this AMS."));
            return !ui.button("Close").clicked();
        };
        let name = unit_name(&u);
        let model = u.get("model").and_then(Value::as_str).unwrap_or("AMS");
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Dry filament · AMS {name}")).family(theme::medium()).size(14.0));
            ui.label(muted(model).size(12.0));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(climate(&u)).monospace().size(11.0).color(p.muted));
            });
        });
        ui.add_space(6.0);
        // what is in it
        let trays: Vec<Value> = u.get("trays").and_then(Value::as_array).cloned().unwrap_or_default();
        ui.horizontal_wrapped(|ui| {
            for t in &trays {
                swatch(ui, egui::vec2(14.0, 14.0), tray_color(t), "", false, Sense::hover());
                ui.label(RichText::new(tray_name(t)).size(11.5).color(if t.get("loaded").and_then(Value::as_bool) == Some(true) { p.text } else { p.placeholder }));
                ui.add_space(6.0);
            }
        });
        ui.add_space(6.0);

        let dr = u.get("drying").cloned().unwrap_or(Value::Null);
        let state = dr.get("state").and_then(Value::as_str).unwrap_or("off");
        if matches!(state, "checking" | "drying" | "stopping") {
            let summary = dr.get("summary").and_then(Value::as_str).unwrap_or("drying");
            let left = dr.get("remaining_minutes").and_then(Value::as_f64);
            let total = dr.get("duration_hours").and_then(Value::as_f64).map(|h| h * 60.0).filter(|t| *t > 0.0);
            match (left, total) {
                (Some(l), Some(t)) => {
                    ui.add(egui::ProgressBar::new((1.0 - l / t).clamp(0.0, 1.0) as f32).text(RichText::new(summary).size(11.5)).fill(p.accent).desired_height(20.0));
                }
                _ => {
                    ui.label(RichText::new(summary).size(12.0).color(p.accent));
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let running = f.running.is_some();
                if ui.add_enabled(state != "stopping" && !running, egui::Button::new(RichText::new(if running { "Stopping…" } else { "Stop drying" }).color(p.error))).clicked() {
                    f.running = Some("stop_drying".into());
                    f.result = None;
                    self.worker.call("printer_ams", json!({ "action": "stop_drying", "ams_id": f.ams_id }), Kind::Ams);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        keep = false;
                    }
                });
            });
            if let Some(r) = &f.result {
                result_line(ui, r);
            }
            return keep;
        }

        let plan = f.plan.as_ref().and_then(|r| r.as_ref().ok()).cloned();
        let (lo, hi) = if model == "AMS HT" { (45, 85) } else { (45, 65) };
        let mut types: Vec<String> = trays.iter().filter(|t| t.get("loaded").and_then(Value::as_bool) == Some(true)).filter_map(|t| t.get("filament_type").and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from)).collect();
        types.sort();
        types.dedup();
        let mut changed = false;
        egui::Grid::new("ams_dry_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label(RichText::new("Dry for").size(12.5).color(p.muted));
            let auto = match plan.as_ref().and_then(|p| p["filament"].as_str()) {
                Some(fl) if f.filament.is_empty() => format!("What's loaded ({fl})"),
                _ => "What's loaded".into(),
            };
            egui::ComboBox::from_id_salt("ams_dry_filament").selected_text(if f.filament.is_empty() { auto.clone() } else { f.filament.clone() }).width(220.0).show_ui(ui, |ui| {
                changed |= ui.selectable_value(&mut f.filament, String::new(), auto.as_str()).on_hover_text("The loaded filament that needs the lowest temperature").changed();
                ui.separator();
                for m in types.iter().map(String::as_str).chain(MATERIALS.iter().copied().filter(|m| !types.iter().any(|t| t == m))) {
                    changed |= ui.selectable_value(&mut f.filament, m.to_string(), m).changed();
                }
            });
            ui.end_row();

            ui.label(RichText::new("Temperature").size(12.5).color(p.muted));
            ui.horizontal(|ui| {
                let shown = f.temp.or_else(|| plan.as_ref().and_then(|p| p["temperature"].as_u64()).map(|v| v as u32)).unwrap_or(lo);
                let mut v = shown;
                if ui.add(egui::DragValue::new(&mut v).range(lo..=hi).suffix(" °C")).changed() {
                    f.temp = Some(v);
                    changed = true;
                }
                ui.label(muted(format!("{lo}–{hi} °C on the {model}")).size(11.0));
                if f.temp.is_some() && theme::bracket_button(ui, "Bambu's").on_hover_text("What the filament's Bambu preset recommends").clicked() {
                    f.temp = None;
                    changed = true;
                }
            });
            ui.end_row();

            ui.label(RichText::new("Time").size(12.5).color(p.muted));
            ui.horizontal(|ui| {
                let shown = f.hours.or_else(|| plan.as_ref().and_then(|p| p["hours"].as_u64()).map(|v| v as u32)).unwrap_or(8);
                let mut v = shown;
                if ui.add(egui::DragValue::new(&mut v).range(1..=72).suffix(" h")).changed() {
                    f.hours = Some(v);
                    changed = true;
                }
                ui.label(muted("1–72 h").size(11.0));
                if f.hours.is_some() && theme::bracket_button(ui, "Bambu's").on_hover_text("What the filament's Bambu preset recommends").clicked() {
                    f.hours = None;
                    changed = true;
                }
            });
            ui.end_row();

            ui.label("");
            changed |= theme::checkbox(ui, &mut f.rotate, RichText::new("Turn the spools while drying").size(12.5)).changed();
            ui.end_row();
        });

        // the plan for these values, asked again a moment after they stop changing
        let now = ui.input(|i| i.time);
        if changed {
            f.changed_at = now;
            f.result = None;
        }
        let mut args = json!({ "action": "dry", "ams_id": f.ams_id, "rotate_spools": f.rotate, "confirm": false });
        if !f.filament.is_empty() {
            args["filament_type"] = json!(f.filament);
        }
        if let Some(t) = f.temp {
            args["dry_temp"] = json!(t);
        }
        if let Some(h) = f.hours {
            args["dry_hours"] = json!(h);
        }
        let key = args.to_string();
        let fresh = f.asked.as_deref() == Some(key.as_str());
        if !fresh && f.running.is_none() {
            let wait = 0.35 - (now - f.changed_at);
            if wait <= 0.0 && !self.worker.busy_with(|k| matches!(k, Kind::AmsPlan(_))) {
                f.asked = Some(key.clone());
                self.read_tool("printer_ams", args.clone(), Kind::AmsPlan(key.clone()));
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(wait.max(0.05)));
            }
        }
        ui.add_space(8.0);
        let checking = !fresh;
        egui::Frame::new().fill(p.bg).stroke(egui::Stroke::new(1.0, p.border_variant)).corner_radius(6).inner_margin(10).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(44.0);
            match &f.plan {
                None => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(12.0).color(p.accent));
                        ui.label(muted("Working out the plan…").size(12.0));
                    });
                }
                Some(Ok(pl)) => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(pl["title"].as_str().unwrap_or("Ready")).family(theme::medium()).size(13.0));
                        if checking {
                            ui.add(egui::Spinner::new().size(11.0).color(p.muted));
                        }
                    });
                    for n in pl["notes"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                        ui.add(egui::Label::new(muted(format!("· {n}")).size(11.5)).wrap());
                    }
                }
                Some(Err(e)) => {
                    if checking {
                        ui.add(egui::Spinner::new().size(11.0).color(p.muted));
                    }
                    result_line(ui, &Err(e.clone()));
                }
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            // once started, the status shows the drying; changing a value allows another start
            let ready = fresh && matches!(f.plan, Some(Ok(_))) && !matches!(f.result, Some(Ok(_)));
            let running = f.running.is_some();
            let b = ui.add_enabled(ready && !running, theme::primary_button(if running { "Starting…" } else { "Start drying" }));
            if !ready && !running {
                b.on_disabled_hover_text("Waiting for a plan the AMS can run");
            } else if b.clicked() {
                let mut go = args.clone();
                go["confirm"] = json!(true);
                f.running = Some("dry".into());
                f.result = None;
                self.worker.call("printer_ams", go, Kind::Ams);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    keep = false;
                }
            });
        });
        if let Some(r) = &f.result {
            ui.add_space(4.0);
            result_line(ui, r);
        }
        keep
    }
}
