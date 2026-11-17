//! First launch, in three steps over the real window (so the interface is already there behind
//! it):
//!
//! 1. **Which app are you used to?** Picks the shortcuts and the handle style that will feel
//!    familiar, says exactly what it sets, and everything stays changeable in Settings (and in
//!    ~/.only3d/ui.json and keymap.json).
//! 2. **Set up your 3D printer now?** "Later" ends the setup. "Set it up now" is a short guide:
//!    find the printer on the network (`printer_discover`), copy the access code from its
//!    screen, save it (`printer_add`) and test the connection (`printer_status`).
//! 3. **Connect an AI agent?** The agent apps, the same picker as only3d ▸ Connect an Agent:
//!    which ones this Mac has, and one click to add only3d to one (`agents::install`).
//!
//! Every step can be skipped; with the defaults it is three clicks to the plate.

use eframe::egui::{self, RichText};
use serde_json::{json, Value};

use crate::app::App;
use crate::keymap::Preset;
use crate::theme::{self, pal};
use crate::worker::Kind;

/// One choice on the welcome screen.
pub struct Profile {
    pub name: &'static str,
    pub apps: &'static str,
    pub keymap: Preset,
    /// "triad" or "tools" (see GizmoPrefs::style).
    pub handles: &'static str,
    pub why: &'static str,
}

pub const PROFILES: [Profile; 5] = [
    Profile { name: "only3d", apps: "the default", keymap: Preset::Only3d, handles: "triad", why: "Move arrows, rotate ring and size handles together on a selection; macOS-style keys (⌘K palette)." },
    Profile { name: "Bambu Studio", apps: "Bambu Studio, OrcaSlicer, PrusaSlicer", keymap: Preset::Bambu, handles: "tools", why: "Bambu Studio's shortcuts, and one tool at a time: M move, R rotate, S scale." },
    Profile { name: "CAD", apps: "Fusion, Onshape, SolidWorks, Shapr3D, FreeCAD", keymap: Preset::Only3d, handles: "triad", why: "Handles together like a CAD triad, exact values typed while dragging, smart guides." },
    Profile { name: "Blender", apps: "Blender", keymap: Preset::Only3d, handles: "triad", why: "Type a number while dragging to set it exactly; handles together. (G/R/S keys aren't mapped yet.)" },
    Profile { name: "Tinkercad", apps: "Tinkercad", keymap: Preset::Only3d, handles: "triad", why: "Drag objects on the plate, size them with the box handles, add shapes with right-click." },
];

/// Where the first-launch setup is, the printer form of step 2 and the agent picker of step 3.
pub struct Onboarding {
    /// 1, 2 or 3.
    pub step: u8,
    /// The profile picked in step 1.
    pub choice: usize,
    /// Step 2: "Set it up now" was picked.
    pub setup: bool,
    pub searching: bool,
    /// What `printer_discover` found (serial, ip, model, name, connect_mode), or why not.
    pub found: Option<Result<Vec<Value>, String>>,
    pub name: String,
    pub host: String,
    pub serial: String,
    pub code: String,
    /// Saving and testing the printer.
    pub busy: bool,
    pub outcome: Option<Result<String, String>>,
    pub agents: crate::app_menus::AgentSetup,
}

impl Default for Onboarding {
    fn default() -> Self {
        Onboarding { step: 1, choice: 0, setup: false, searching: false, found: None, name: "H2D".into(), host: String::new(), serial: String::new(), code: String::new(), busy: false, outcome: None, agents: Default::default() }
    }
}

impl Onboarding {
    fn connected(&self) -> bool {
        matches!(self.outcome, Some(Ok(_))) && !self.busy
    }
}

/// "Step 1 of 3" with a short bar per step.
fn steps(ui: &mut egui::Ui, step: u8) {
    let p = pal();
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        for k in [3u8, 2, 1] {
            let (r, _) = ui.allocate_exact_size(egui::vec2(28.0, 4.0), egui::Sense::hover());
            ui.painter().rect_filled(r, 2.0, if k <= step { p.accent } else { p.border_variant });
        }
        ui.label(RichText::new(format!("Step {step} of 3")).size(12.0).color(p.muted));
    });
}

impl App {
    pub(crate) fn welcome_ui(&mut self, ctx: &egui::Context) {
        if !self.show_welcome {
            return;
        }
        let mut finish = false;
        egui::Modal::new(egui::Id::unique("o3d_welcome")).backdrop_color(Self::backdrop()).frame(Self::modal_frame()).show(ctx, |ui| {
            // roomy margins: this is the first thing anyone sees
            egui::Frame::new().inner_margin(egui::Margin::symmetric(36, 30)).show(ui, |ui| {
                ui.set_width(640.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Welcome to only3d").size(18.0).family(theme::medium()));
                    steps(ui, self.onboard.step);
                });
                ui.add_space(14.0);
                finish = match self.onboard.step {
                    1 => self.welcome_interface(ui),
                    2 => {
                        self.welcome_printer(ui);
                        false
                    }
                    _ => self.welcome_agent(ui),
                };
            });
        });
        if finish {
            self.show_welcome = false;
            self.onboard.code.clear();
            self.save_ui_now();
        }
    }

    /// Step 1: which app are you used to? Returns true to end the setup.
    fn welcome_interface(&mut self, ui: &mut egui::Ui) -> bool {
        let p = pal();
        ui.label(RichText::new("Which app are you used to?").size(15.0).family(theme::medium()));
        ui.label(RichText::new("only3d borrows its shortcuts and handles so it feels familiar. You can change this any time in Settings.").size(12.0).color(p.muted));
        ui.add_space(8.0);
        for (k, prof) in PROFILES.iter().enumerate() {
            let sel = self.onboard.choice == k;
            let r = egui::Frame::new()
                .fill(if sel { p.selected } else { p.bg })
                .stroke(egui::Stroke::new(1.0, if sel { p.accent } else { p.border_variant }))
                .corner_radius(6)
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(prof.name).size(13.5).family(theme::medium()));
                        ui.label(RichText::new(prof.apps).size(11.5).color(p.placeholder));
                    });
                    ui.label(RichText::new(prof.why).size(12.0).color(p.muted));
                })
                .response
                .interact(egui::Sense::click());
            if r.clicked() {
                self.onboard.choice = k;
            }
            ui.add_space(4.0);
        }
        // exactly what the choice sets
        let prof = &PROFILES[self.onboard.choice];
        ui.add_space(4.0);
        egui::Frame::new().fill(p.elevated).corner_radius(4).inner_margin(8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("This sets").size(11.0).color(p.placeholder));
            for (k, v) in [
                ("Shortcuts", prof.keymap.label().to_string()),
                ("Handles", if prof.handles == "triad" { "all together (CAD style)".into() } else { "one tool at a time (M / R / S)".into() }),
                ("Snapping", format!("move {} mm (⌥ finer), rotate {}° (⌥ 1°), smart guides on", self.cfg.gizmo.move_snap_mm, self.cfg.gizmo.rotate_snap_deg)),
                ("Mouse", "left-drag orbits, right-drag pans, scroll zooms (the same for every choice for now)".to_string()),
            ] {
                ui.horizontal(|ui| {
                    ui.add_sized([80.0, 16.0], egui::Label::new(RichText::new(k).size(12.0).color(p.muted)));
                    ui.label(RichText::new(v).size(12.0));
                });
            }
        });
        ui.add_space(18.0);
        let mut done = false;
        ui.horizontal(|ui| {
            if ui.add(theme::primary_button(format!("Use {} and continue", prof.name))).clicked() {
                let prof = &PROFILES[self.onboard.choice];
                self.set_keymap(prof.keymap);
                self.cfg.gizmo.style = prof.handles.to_string();
                self.onboard.step = 2;
                self.save_ui_now();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Skip setup").on_hover_text("Keep the defaults and go straight to the plate").clicked() {
                    done = true;
                }
            });
        });
        done
    }

    /// Step 2: set up a printer now (a short guide) or later; both go on to step 3.
    fn welcome_printer(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        ui.label(RichText::new("Set up your 3D printer now?").size(15.0).family(theme::medium()));
        ui.label(
            RichText::new("only3d sends prints to your Bambu Lab printer over your home network and shows how they're going. It takes about two minutes, or do it later from the Printer menu.")
                .size(12.0)
                .color(p.muted),
        );
        ui.add_space(18.0);
        if !self.onboard.setup {
            ui.horizontal(|ui| {
                if ui.add(theme::primary_button("Set it up now")).clicked() {
                    self.onboard.setup = true;
                }
                if ui.button("Later").on_hover_text("Add a printer any time from the Printer menu").clicked() {
                    self.onboard.step = 3;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Back").clicked() {
                        self.onboard.step = 1;
                    }
                });
            });
            return;
        }

        let num = |ui: &mut egui::Ui, n: &str, text: &str| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(n).size(12.0).family(theme::medium()).color(p.accent));
                ui.label(RichText::new(text).size(12.5).family(theme::medium()));
            });
        };

        // 1: find it
        num(ui, "1", "Find the printer on your network");
        ui.label(RichText::new("Your Mac and the printer need to be on the same Wi-Fi or network.").size(11.5).color(p.muted));
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.onboard.searching, egui::Button::new(if self.onboard.searching { "Looking…" } else { "Find printers" })).clicked() {
                self.onboard.searching = true;
                self.onboard.found = None;
                self.worker.call("printer_discover", json!({ "seconds": 6 }), Kind::Onboard("discover".into()));
            }
            if self.onboard.searching {
                ui.add(egui::Spinner::new().size(12.0).color(p.accent));
                ui.label(RichText::new("printers announce themselves every few seconds").size(11.5).color(p.placeholder));
            }
        });
        let found = self.onboard.found.clone();
        match &found {
            Some(Ok(list)) if list.is_empty() => {
                ui.label(RichText::new("None answered. Check the printer is on, or type its IP address below (Settings ▸ Network on its screen).").size(11.5).color(p.warning));
            }
            Some(Ok(list)) => {
                for f in list {
                    let ip = f["ip"].as_str().unwrap_or("");
                    let serial = f["serial"].as_str().unwrap_or("");
                    let model = f["model"].as_str().or(f["model_code"].as_str()).unwrap_or("Bambu Lab printer");
                    let name = f["name"].as_str().unwrap_or("");
                    let mode = f["connect_mode"].as_str().unwrap_or("");
                    let on = self.onboard.serial == serial && !serial.is_empty();
                    let label = format!("{}{model} · {ip}{}", if name.is_empty() { String::new() } else { format!("{name} · ") }, if mode.is_empty() { String::new() } else { format!(" · {mode} mode") });
                    if ui.selectable_label(on, RichText::new(label).size(12.0)).clicked() {
                        self.onboard.host = ip.to_string();
                        self.onboard.serial = serial.to_string();
                        if !name.is_empty() {
                            self.onboard.name = name.to_string();
                        }
                    }
                }
            }
            Some(Err(e)) => {
                ui.label(RichText::new(e).size(11.5).color(p.error));
            }
            None => {}
        }
        ui.add_space(8.0);

        // 2: the access code
        num(ui, "2", "Copy the access code from the printer's screen");
        ui.label(RichText::new("On the printer: Settings ▸ Network shows the IP address and an 8-character Access Code. The serial number is under Settings ▸ Device (finding the printer fills it in).").size(11.5).color(p.muted));
        ui.add_space(4.0);
        egui::Grid::new("o3d_onboard_printer").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            for (label, field, hint, secret) in [("Name", &mut self.onboard.name, "H2D", false), ("IP address", &mut self.onboard.host, "like 192.168.1.20", false), ("Serial", &mut self.onboard.serial, "from Settings ▸ Device", false), ("Access code", &mut self.onboard.code, "8 characters", true)] {
                ui.label(RichText::new(label).size(12.0).color(p.muted));
                ui.add(egui::TextEdit::singleline(field).hint_text(hint).password(secret).desired_width(300.0));
                ui.end_row();
            }
        });
        ui.add_space(8.0);

        // 3: what printing from only3d needs
        num(ui, "3", "To start prints from only3d");
        egui::Frame::new().fill(p.elevated).corner_radius(4).inner_margin(8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new("Turn on LAN Only mode and Developer Mode in the printer's network settings. Without them only3d can still show the printer's status, but Bambu's firmware refuses prints sent from other apps. With LAN Only on, the Bambu Handy app and cloud printing stop working.")
                    .size(11.5)
                    .color(p.muted),
            );
        });
        ui.add_space(8.0);

        if let Some(r) = &self.onboard.outcome {
            let (text, err) = match r {
                Ok(t) => (t.as_str(), false),
                Err(t) => (t.as_str(), true),
            };
            egui::Frame::new().fill(if err { theme::soft(p.error) } else { theme::soft(p.success) }).corner_radius(4).inner_margin(8).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(text).size(12.0).color(if err { p.error } else { p.text }));
            });
            ui.add_space(6.0);
        }

        let ready = !self.onboard.host.trim().is_empty() && !self.onboard.serial.trim().is_empty() && !self.onboard.code.trim().is_empty() && !self.onboard.name.trim().is_empty();
        ui.horizontal(|ui| {
            if self.onboard.connected() {
                if ui.add(theme::primary_button("Continue")).clicked() {
                    self.onboard.step = 3;
                }
            } else {
                let label = if self.onboard.busy { "Connecting…" } else { "Add printer" };
                if ui.add_enabled(ready && !self.onboard.busy, theme::primary_button(label)).clicked() {
                    self.onboard.busy = true;
                    self.onboard.outcome = None;
                    let o = &self.onboard;
                    let args = json!({ "name": o.name.trim(), "host": o.host.trim(), "serial": o.serial.trim(), "access_code": o.code.trim() });
                    self.worker.call("printer_add", args, Kind::Onboard("add".into()));
                }
                if self.onboard.busy {
                    ui.add(egui::Spinner::new().size(12.0).color(p.accent));
                }
                if ui.button("Later").on_hover_text("Add a printer any time from the Printer menu").clicked() {
                    self.onboard.step = 3;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Back").clicked() {
                    self.onboard.setup = false;
                }
            });
        });
    }

    /// Step 3: connect an AI agent now or later. Returns true to end the setup.
    fn welcome_agent(&mut self, ui: &mut egui::Ui) -> bool {
        let p = pal();
        let mut done = false;
        ui.label(RichText::new("Connect an AI agent?").size(15.0).family(theme::medium()));
        ui.label(
            RichText::new("Add only3d to the agent app you already use, whatever model runs in it. The agent can then design, check and slice parts, and print once you approve it here. Or do it later from only3d ▸ Connect an Agent.")
                .size(12.0)
                .color(p.muted),
        );
        ui.add_space(14.0);
        let mut st = std::mem::take(&mut self.onboard.agents);
        self.agent_picker(ui, &mut st, true);
        self.onboard.agents = st;
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            if ui.add(theme::primary_button("Start using only3d")).on_hover_text("Connect agents any time from only3d ▸ Connect an Agent").clicked() {
                done = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Back").clicked() {
                    self.onboard.step = 2;
                }
            });
        });
        done
    }

    /// Results of the step-2 tool calls.
    pub(crate) fn onboard_done(&mut self, what: &str, r: Result<only3d_studio::ToolOutput, only3d_studio::ToolError>) {
        let err = |e: only3d_studio::ToolError| format!("{}\n→ {}", e.message, e.hint);
        match what {
            "discover" => {
                self.onboard.searching = false;
                self.onboard.found = Some(match r {
                    Ok(o) => {
                        let list = o.data["printers"].as_array().cloned().unwrap_or_default();
                        // one printer: fill the form in
                        if let [only] = list.as_slice() {
                            self.onboard.host = only["ip"].as_str().unwrap_or("").to_string();
                            self.onboard.serial = only["serial"].as_str().unwrap_or("").to_string();
                            if let Some(n) = only["name"].as_str().filter(|n| !n.is_empty()) {
                                self.onboard.name = n.to_string();
                            }
                        }
                        Ok(list)
                    }
                    Err(e) => Err(err(e)),
                });
            }
            "add" => match r {
                Ok(_) => {
                    let book = only3d_studio::printer::load_book();
                    self.printers = book.printers.iter().map(|p| p.name.clone()).collect();
                    self.default_printer = book.default.clone();
                    self.onboard.code.clear();
                    let name = self.onboard.name.trim().to_string();
                    self.worker.call("printer_status", json!({ "name": name }), Kind::Onboard("status".into()));
                }
                Err(e) => {
                    self.onboard.busy = false;
                    self.onboard.outcome = Some(Err(err(e)));
                }
            },
            "status" => {
                self.onboard.busy = false;
                self.onboard.outcome = Some(match r {
                    Ok(o) => {
                        self.printer_data = Some(o.data.clone());
                        let first = o.text.lines().next().unwrap_or("").to_string();
                        self.printer_status = Some(Ok(o.text));
                        Ok(format!("Connected. {first}"))
                    }
                    Err(e) => Err(format!("Saved, but only3d couldn't reach it yet: {}", err(e))),
                });
            }
            _ => {}
        }
    }
}
