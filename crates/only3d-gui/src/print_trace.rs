//! The print trace in the Prints panel: one print from `print_detail`'s data as a card (when it
//! ran, the estimate against what was charged, its objects, filament and project) with the
//! actions that fit: print it again, note the outcome (`print_update`), show the project file.

use eframe::egui::{self, Align, Color32, Layout, RichText, Ui};
use serde_json::{json, Value};

use crate::app::{fmt_duration, App};
use crate::dock::PanelId;
use crate::panels::{muted, swatch};
use crate::theme::{self, pal};
use crate::worker::Kind;

/// A print opened in the Prints panel.
pub struct PrintOpen {
    pub id: String,
    /// The trace as text, for the clipboard.
    pub text: String,
    /// `print_detail`'s data: `print`, `objects`, `filaments`, `ledger`.
    pub data: Value,
    /// The notes as typed here; saved when the field loses focus.
    pub notes: String,
}

impl PrintOpen {
    pub fn new(id: String, text: String, data: Value) -> Self {
        let notes = data["print"]["notes"].as_str().unwrap_or("").to_string();
        PrintOpen { id, text, data, notes }
    }
}

/// Seconds since the epoch of a UTC "YYYY-MM-DDTHH:MM:SS…" stamp (the inventory's format).
pub fn iso_secs(t: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| -> Option<i64> { t.get(r)?.parse().ok() };
    let (y, m, d, h, mi, s) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    // days_from_civil (Howard Hinnant)
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468) * 86400 + h * 3600 + mi * 60 + s)
}

/// "12 min ago", "5 h ago", "3 days ago".
fn ago(secs: i64) -> String {
    match secs {
        s if s < 90 => "just now".into(),
        s if s < 3600 => format!("{} min ago", s / 60),
        s if s < 86400 => format!("{} h ago", s / 3600),
        s => format!("{} day{} ago", s / 86400, if s / 86400 == 1 { "" } else { "s" }),
    }
}

/// A tray as the printer's screen names it: "ams0:3" is A3, "ams128:1" HT-A, "ext:left" ext left.
pub fn tray_label(loc: &str) -> String {
    let ams = loc.strip_prefix("ams").and_then(|r| r.split_once(':')).and_then(|(a, slot)| Some((a.parse::<u32>().ok()?, slot)));
    match (loc, ams) {
        ("ext:left", _) => "ext left".into(),
        ("ext:right", _) => "ext right".into(),
        (_, Some((a @ 0..=25, slot))) => format!("{}{slot}", (b'A' + a as u8) as char),
        (_, Some((a @ 128..=153, _))) => format!("HT-{}", (b'A' + (a - 128) as u8) as char),
        _ => loc.to_string(),
    }
}

/// A print's state in words, and whether the job is still on the printer.
fn state_label(state: &str) -> (String, bool) {
    match state {
        "FINISH" => ("finished".into(), false),
        "FAILED" => ("failed".into(), false),
        "CANCELLED" => ("cancelled".into(), false),
        "RUNNING" => ("printing".into(), true),
        "PAUSE" => ("paused".into(), true),
        "PREPARE" => ("preparing".into(), true),
        other => (other.to_lowercase(), false),
    }
}

/// A row with its value at the right edge and the rest cut short to fit beside it.
fn row(ui: &mut Ui, left: impl FnOnce(&mut Ui), right: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            right(ui);
            ui.with_layout(Layout::left_to_right(Align::Center), left);
        });
    });
}

fn mono(text: impl Into<String>, col: Color32) -> RichText {
    RichText::new(text).monospace().size(11.0).color(col)
}

impl App {
    /// The card under an open row of the Prints panel.
    pub(crate) fn print_trace_card(&mut self, ui: &mut Ui) {
        let p = pal();
        let Some(open) = self.ws.print_open.as_ref() else { return };
        let (id, text, d, mut notes) = (open.id.clone(), open.text.clone(), open.data.clone(), open.notes.clone());
        let pr = &d["print"];
        let spools = self.spools();
        let spool = |sid: &str| spools.iter().find(|s| s.id == sid);
        let colour = |sid: Option<&str>| {
            sid.and_then(spool).map(|s| {
                let c = crate::scene::parse_hex(&s.color);
                Color32::from_rgb(c[0], c[1], c[2])
            })
        };
        let filaments = d["filaments"].as_array().cloned().unwrap_or_default();
        let slot_spool = |slot: &Value| filaments.iter().find(|f| f["slot"] == *slot).and_then(|f| f["spool_id"].as_str());

        let state = pr["state"].as_str().unwrap_or("");
        let (label, live) = state_label(state);
        let state_col = match state {
            "FINISH" => p.success,
            "FAILED" | "CANCELLED" => p.error,
            _ if live => p.accent,
            _ => p.placeholder,
        };
        let started = pr["started_at"].as_str().and_then(iso_secs);
        let ended = pr["ended_at"].as_str().and_then(iso_secs);
        let now = (only3d_studio::now_ms() / 1000) as i64;
        let version = pr["project_version_id"].as_i64();
        let path = pr["project_path"].as_str().map(std::path::PathBuf::from).filter(|f| f.exists());
        let ledger = d["ledger"].as_array().cloned().unwrap_or_default();
        let used: f64 = ledger.iter().map(|e| -e["delta_g"].as_f64().unwrap_or(0.0)).sum::<f64>().max(0.0);
        let charged: std::collections::BTreeSet<&str> = ledger.iter().filter_map(|e| e["spool_id"].as_str()).collect();

        let mut mark: Option<&str> = None;
        let mut save_notes = false;
        let mut show_spools = false;
        egui::Frame::new().fill(p.bg).stroke(egui::Stroke::new(1.0, p.border_variant)).corner_radius(4).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 3.0;

            // when, and for how long
            ui.horizontal(|ui| {
                theme::chip(ui, &label, state_col, theme::soft(state_col));
                let mut when: Vec<String> = Vec::new();
                if let Some(s) = started {
                    when.push(ago(now - s));
                    match ended {
                        Some(e) if e >= s => when.push(format!("ran {}", fmt_duration((e - s) as f64))),
                        None if live => when.push(format!("running {}", fmt_duration((now - s) as f64))),
                        _ => {}
                    }
                }
                let stamps = format!("started {}{}", pr["started_at"].as_str().unwrap_or("?"), pr["ended_at"].as_str().map(|e| format!("\nended {e}")).unwrap_or_default());
                ui.add(egui::Label::new(RichText::new(when.join(" · ")).size(12.0).color(p.text)).truncate()).on_hover_text(stamps);
            });

            egui::Grid::new(("print_trace", &id)).num_columns(2).spacing([10.0, 3.0]).show(ui, |ui| {
                if let Some(printer) = pr["printer"].as_str() {
                    ui.label(muted("Printer").size(11.5));
                    let from = match pr["source"].as_str() {
                        Some("printer") => " · started at the printer",
                        Some("only3d") => " · sent from only3d",
                        _ => "",
                    };
                    ui.label(RichText::new(format!("{printer}{from}")).size(11.5));
                    ui.end_row();
                }
                if let Some(g) = pr["est_grams"].as_f64() {
                    ui.label(muted("Estimate").size(11.5));
                    ui.label(mono(format!("{} · {g:.1} g", fmt_duration(pr["est_time_s"].as_f64().unwrap_or(0.0))), p.text));
                    ui.end_row();
                }
                ui.label(muted("Charged").size(11.5));
                if !ledger.is_empty() {
                    let n = charged.len();
                    ui.label(mono(format!("{used:.1} g from {n} spool{}", if n == 1 { "" } else { "s" }), p.text)).on_hover_text(format!("{} ledger entr{} for this print", ledger.len(), if ledger.len() == 1 { "y" } else { "ies" }));
                } else if live {
                    ui.label(mono("—", p.placeholder));
                } else {
                    ui.label(RichText::new("nothing charged to a spool").size(11.5).color(p.warning))
                        .on_hover_text("No ledger entry belongs to this print, so no spool's weight changed for it. If filament was used, weigh the spool or set what's left in Spools.");
                }
                ui.end_row();
            });

            let objects = d["objects"].as_array().cloned().unwrap_or_default();
            if !objects.is_empty() {
                ui.add_space(3.0);
                theme::section_label(ui, "Objects");
                for o in &objects {
                    let name = o["object_name"].as_str().filter(|n| !n.is_empty()).or(o["object_id"].as_str()).unwrap_or("?");
                    let status = o["status"].as_str().unwrap_or("");
                    row(
                        ui,
                        |ui| {
                            swatch(ui, colour(slot_spool(&o["filament_slot"])).unwrap_or(p.border), 10.0);
                            ui.add(egui::Label::new(RichText::new(name).size(12.0)).truncate()).on_hover_text(format!("{} · filament {}", o["object_id"].as_str().unwrap_or(""), o["filament_slot"]));
                        },
                        |ui| {
                            // "planned" is every object's state until someone says otherwise
                            if !matches!(status, "" | "planned") {
                                let col = if matches!(status, "failed" | "skipped") { p.error } else { p.muted };
                                ui.label(RichText::new(status).size(11.0).color(col));
                            }
                            if let Some(g) = o["est_grams"].as_f64() {
                                ui.label(mono(format!("{g:.1} g"), p.muted));
                            }
                        },
                    );
                }
            }

            if !filaments.is_empty() {
                ui.add_space(3.0);
                theme::section_label(ui, "Filament");
                for f in &filaments {
                    let sid = f["spool_id"].as_str();
                    let name = match sid.and_then(spool) {
                        Some(s) if s.name.trim().is_empty() => s.auto_name(),
                        Some(s) => s.name.clone(),
                        None => sid.unwrap_or("unknown spool").to_string(),
                    };
                    row(
                        ui,
                        |ui| {
                            ui.label(mono(f["slot"].to_string(), p.muted));
                            swatch(ui, colour(sid).unwrap_or(p.border), 10.0);
                            let r = ui.add(egui::Label::new(RichText::new(name).size(12.0)).truncate().sense(egui::Sense::click()));
                            let tip = match (sid, f["est_grams"].as_f64()) {
                                (Some(s), Some(g)) => format!("{s} · {g:.1} g estimated · click for Spools"),
                                (Some(s), None) => format!("{s} · click for Spools"),
                                _ => "The spool in this tray wasn't in the inventory".to_string(),
                            };
                            if r.on_hover_text(tip).clicked() && sid.is_some() {
                                show_spools = true;
                            }
                        },
                        |ui| {
                            if let Some(t) = f["tray"].as_str() {
                                ui.label(mono(tray_label(t), p.muted)).on_hover_text(only3d_studio::spools::location_name(t));
                            }
                        },
                    );
                }
            }

            if let Some(name) = pr["project_name"].as_str() {
                ui.add_space(3.0);
                theme::section_label(ui, "Project");
                let tip = format!("{}\n{} · {}", pr["project_path"].as_str().unwrap_or("not saved to a file"), pr["project_id"].as_str().unwrap_or(""), pr["version_hash"].as_str().unwrap_or(""));
                row(
                    ui,
                    |ui| {
                        ui.add(egui::Label::new(RichText::new(name).size(12.0)).truncate()).on_hover_text(tip);
                    },
                    |ui| {
                        if let Some(v) = version {
                            ui.label(mono(format!("v{v}"), p.muted)).on_hover_text(format!("Project version {v}, saved {}", pr["version_created"].as_str().unwrap_or("?")));
                        }
                    },
                );
            }

            ui.add_space(3.0);
            let r = ui.add(egui::TextEdit::singleline(&mut notes).hint_text("Notes: how it came out, what to change").desired_width(f32::INFINITY));
            save_notes = r.lost_focus() && notes != pr["notes"].as_str().unwrap_or("");

            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if let Some(v) = version {
                    if theme::bracket_button(ui, "print again").on_hover_text("Opens the exact project version this was printed from (undo brings back what you had), then the print checklist").clicked() {
                        self.read_tool("project_versions", json!({ "version": v }), Kind::Reprint);
                    }
                }
                if let Some(f) = &path {
                    if theme::bracket_button(ui, "show file").on_hover_text(crate::chrome::tilde(f)).clicked() {
                        crate::os::reveal(f);
                    }
                }
                // a job on the printer keeps reporting its own state
                if !live {
                    let r = theme::bracket_button(ui, "mark as…").on_hover_text("Record how this print ended");
                    egui::Popup::menu(&r).show(|ui| {
                        for (to, what) in [("FINISH", "Finished"), ("FAILED", "Failed"), ("CANCELLED", "Cancelled")] {
                            if to != state && ui.button(what).clicked() {
                                mark = Some(to);
                            }
                        }
                    });
                }
                if theme::bracket_button(ui, "copy").on_hover_text("Copy the trace as text").clicked() {
                    ui.ctx().copy_text(text.clone());
                }
            });
        });

        if let Some(o) = self.ws.print_open.as_mut() {
            o.notes = notes.clone();
        }
        let mut update = serde_json::Map::new();
        if save_notes {
            update.insert("notes".into(), json!(notes));
        }
        if let Some(to) = mark {
            update.insert("state".into(), json!(to));
        }
        if !update.is_empty() {
            update.insert("id".into(), json!(id));
            // the worker runs these in order: the card re-reads what was just written
            self.worker.call("print_update", Value::Object(update), Kind::Report);
            self.read_tool("print_detail", json!({ "id": id }), Kind::PrintDetail(id));
        }
        if show_spools {
            self.cfg.layout.show(PanelId::Spools);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_inventory_stamps() {
        assert_eq!(iso_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(iso_secs("2026-11-21T21:11:03Z"), Some(1_795_295_463));
        // a leap day, and the day after it
        assert_eq!(iso_secs("2024-03-01T00:00:00Z").unwrap() - iso_secs("2024-02-28T00:00:00Z").unwrap(), 2 * 86400);
        assert_eq!(iso_secs("yesterday"), None);
    }

    #[test]
    fn names_trays_like_the_printer() {
        assert_eq!(tray_label("ams0:3"), "A3");
        assert_eq!(tray_label("ams1:1"), "B1");
        assert_eq!(tray_label("ams128:1"), "HT-A");
        assert_eq!(tray_label("ext:left"), "ext left");
        assert_eq!(tray_label("shelf"), "shelf");
    }

    #[test]
    fn ages_in_words() {
        assert_eq!(ago(30), "just now");
        assert_eq!(ago(600), "10 min ago");
        assert_eq!(ago(5 * 3600), "5 h ago");
        assert_eq!(ago(86400), "1 day ago");
        assert_eq!(ago(3 * 86400), "3 days ago");
    }
}
