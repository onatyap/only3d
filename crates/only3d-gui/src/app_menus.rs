//! The app's side of the macOS menu bar ([`crate::menubar`]): running what a menu picks, the
//! state each entry shows (enabled, ticked, shortcut, label), File > Open Recent, and the
//! windows only the menus open (Revert to Version, Connect an Agent, Agent Tools).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use serde_json::{json, Value};

use crate::actions::Gizmo;
use crate::app::{App, Dialog, Mode, PRIMITIVES};
use crate::dock::PanelId;
use crate::keymap::Action;
use crate::menubar::{Cmd, ItemState};
use crate::theme::{self, pal};
use crate::worker::Kind;

const GUIDE_URL: &str = "https://github.com/onatyap/only3d/blob/main/docs/guide.md";
const ISSUES_URL: &str = "https://github.com/onatyap/only3d/issues/new";

/// The last install: which app (its index) and what it said.
type InstallResult = Arc<Mutex<Option<(usize, Result<String, String>)>>>;

/// Connect an Agent: the app picked, each app's status (read on a thread), and what the last
/// install said.
#[derive(Default)]
pub struct AgentSetup {
    pub pick: Option<usize>,
    pub status: Arc<Mutex<Option<Vec<only3d_studio::agents::Status>>>>,
    pub out: InstallResult,
    pub running: bool,
    pub asked: bool,
}

impl AgentSetup {
    /// Read every app's status in the background (they read config files, one of them large).
    fn refresh(&mut self, ctx: &egui::Context) {
        self.asked = true;
        let slot = self.status.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let places = only3d_studio::agents::Places::from_env();
            let v = only3d_studio::agents::APPS.iter().map(|a| only3d_studio::agents::status(&places, a.id)).collect();
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(v);
            ctx.request_repaint();
        });
    }
}

impl App {
    /// Once per frame: run menu picks, keep Open Recent current, and push enabled / ticked /
    /// shortcut state to the menus (at most every 120 ms, at once when typing starts or stops).
    pub(crate) fn menu_frame(&mut self, ctx: &egui::Context) {
        if !self.menubar.installed() {
            return;
        }
        let typing = ctx.text_edit_focused();
        let _ = self.workspace_root();
        let picks = self.menubar.take_commands();
        let picked = !picks.is_empty();
        for cmd in picks {
            self.menu_command(ctx, cmd, typing);
        }
        if let Some(p) = self.snap.project_path.as_ref().map(|p| dunce::canonicalize(p).unwrap_or_else(|_| p.clone()).display().to_string()) {
            if self.cfg.recent_projects.first() != Some(&p) {
                self.cfg.recent_projects.retain(|r| *r != p);
                self.cfg.recent_projects.insert(0, p);
                self.cfg.recent_projects.truncate(10);
                self.save_ui_now();
            }
        }
        // shortcuts text fields need come off the menu while one has focus (and over overlays,
        // where the keymap only lets the palette toggle through)
        let bypass = typing || self.overlay_open();
        let now = Instant::now();
        let due = picked || bypass != self.menu_bypass || self.menu_synced.is_none_or(|t| now.duration_since(t) >= Duration::from_millis(120));
        if !due {
            ctx.request_repaint_after(Duration::from_millis(130));
            return;
        }
        self.menu_synced = Some(now);
        self.menu_bypass = bypass;
        let mut bar = std::mem::take(&mut self.menubar);
        bar.sync(|a| self.menu_item_state(a, bypass));
        bar.set_recent(&self.cfg.recent_projects);
        let plates: Vec<(u32, String)> = (1..=self.snap.project.plate_count()).filter(|k| *k != self.snap.plate).map(|k| (k, self.snap.project.plate_name(k))).collect();
        bar.set_plates(&plates, !self.snap.selection.is_empty());
        bar.set_panels(|p| self.cfg.layout.is_showing(p));
        self.menubar = bar;
    }

    fn menu_item_state(&self, a: Action, bypass: bool) -> ItemState {
        let chord = self.keymap.menu_chord(a).map(|(c, _)| c).filter(|_| !bypass || a.while_typing());
        let text = match a {
            Action::SetFilament(k) => self.snap.project.filaments.get(k as usize - 1).map(|f| format!("{k}  {}", crate::settings_ui::short(&f.preset))),
            _ => None,
        };
        ItemState { enabled: self.menu_enabled(a), checked: self.menu_checked(a), chord, text }
    }

    pub(crate) fn menu_enabled(&self, a: Action) -> bool {
        use Action::*;
        let printer = !self.printers.is_empty();
        match a {
            ShowInFinder => self.snap.project_path.is_some(),
            ExportSelection => !self.snap.selection.is_empty(),
            CloseTab => !self.editor.active(self.editor.focus).is_plate(),
            ReopenTab => self.editor.can_reopen(),
            NextTab | PrevTab => self.editor.groups[self.editor.focus].tabs.len() > 1,
            SideBySide => self.editor.groups.len() == 2 || self.editor.groups[0].tabs.len() > 1,
            ExportAny => !self.snap.project.objects.is_empty(),
            CloseFolder => self.ws.root.as_ref().is_some_and(|(_, r)| r.is_some()),
            Slice => !self.slicing() && !self.snap.plate_is_empty(),
            Print => self.print_modal.is_none() && !self.snap.plate_is_empty(),
            ExportPrint | ExportStl | Export3mf | ExportScad | ExportBambu => !self.snap.plate_is_empty(),
            PrinterCamera | PrinterPause | PrinterResume | PrinterStop | SkipObjects | LoadFilament | UnloadFilament | SyncSpools | PrinterFiles | PrinterTimelapses => printer,
            ToolsReference => !self.tools.is_empty(),
            // ticked when it is already the selection's filament, and still pickable
            SetFilament(k) => !self.snap.selection.is_empty() && (k as usize) <= self.snap.project.filaments.len(),
            _ => self.applicable(a),
        }
    }

    pub(crate) fn menu_checked(&self, a: Action) -> bool {
        use Action::*;
        let model = self.mode == Mode::Model;
        match a {
            ToolSelect => model && self.gizmo == Gizmo::Select,
            ToolMove => model && self.gizmo == Gizmo::Move,
            ToolRotate => model && self.gizmo == Gizmo::Rotate,
            ToolScale => model && self.gizmo == Gizmo::Scale,
            ToolPushPull => model && self.gizmo == Gizmo::PushPull,
            Prepare => model,
            Preview => !model,
            ToggleOverhangs => self.overhangs,
            ToggleAllPlates => self.cfg.show_all_plates,
            SideBySide => self.editor.groups.len() == 2,
            OneLayer => self.one_layer,
            Theme(t) => self.cfg.theme == t,
            ToggleDock(side) => !self.cfg.layout.zen && self.cfg.layout.dock(side).visible(),
            Zen => self.cfg.layout.zen,
            SetFilament(k) => {
                let sel: Vec<u32> = self.snap.selection.iter().filter_map(|id| self.snap.project.object(id)).map(|o| o.filament).collect();
                !sel.is_empty() && sel.iter().all(|f| *f == k as u32)
            }
            _ => false,
        }
    }

    fn menu_command(&mut self, ctx: &egui::Context, cmd: Cmd, typing: bool) {
        match cmd {
            Cmd::Run(a) => self.run_from_menu(ctx, a, typing),
            Cmd::OpenRecent(p) => {
                if std::path::Path::new(&p).exists() {
                    self.open_project(p);
                } else {
                    self.cfg.recent_projects.retain(|r| *r != p);
                    self.save_ui_now();
                    self.toast(format!("{} was moved or deleted", crate::chrome::tilde(std::path::Path::new(&p))), true);
                }
            }
            Cmd::ClearRecent => {
                self.cfg.recent_projects.clear();
                self.save_ui_now();
            }
            Cmd::MoveToPlate(k) => {
                let sel = self.snap.selection.clone();
                if !sel.is_empty() {
                    self.worker.call("plate", json!({ "action": "move", "plate": k, "ids": sel }), Kind::Plain);
                }
            }
            Cmd::MoveToNewPlate => {
                let sel = self.snap.selection.clone();
                if !sel.is_empty() {
                    let n = self.snap.project.plate_count();
                    self.worker.call("plate", json!({ "action": "add" }), Kind::Plain);
                    self.worker.call("plate", json!({ "action": "move", "plate": n + 1, "ids": sel }), Kind::Plain);
                }
            }
            Cmd::TogglePanel(p) => self.cfg.layout.toggle_panel(p),
        }
    }

    /// A menu pick. While a text field has focus, the edit commands act on its text (the way
    /// their keys would); Paste asks for the system clipboard so objects copied in another
    /// only3d window paste here too.
    fn run_from_menu(&mut self, ctx: &egui::Context, a: Action, typing: bool) {
        use egui::{Event, Key, Modifiers, ViewportCommand};
        if typing {
            let key = |key: Key, modifiers: Modifiers| [true, false].map(|pressed| Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers });
            match a {
                Action::Copy => return ctx.send_viewport_cmd(ViewportCommand::RequestCopy),
                Action::Cut => return ctx.send_viewport_cmd(ViewportCommand::RequestCut),
                Action::Paste => return ctx.send_viewport_cmd(ViewportCommand::RequestPaste),
                Action::Undo => return self.menu_keys.extend(key(Key::Z, Modifiers::COMMAND)),
                Action::Redo => return self.menu_keys.extend(key(Key::Z, Modifiers::COMMAND | Modifiers::SHIFT)),
                Action::SelectAll => return self.menu_keys.extend(key(Key::A, Modifiers::COMMAND)),
                _ => {}
            }
        }
        if a == Action::Paste {
            // comes back as a paste event, through the same path as ⌘V
            return ctx.send_viewport_cmd(ViewportCommand::RequestPaste);
        }
        if self.palette.open && a != Action::Palette {
            self.palette.open = false;
        }
        self.run(a);
    }

    /// The UI side of the actions that only the menus (and the palette) list.
    pub(crate) fn menu_ui_action(&mut self, a: Action) {
        use Action::*;
        match a {
            AddShape(i) => {
                self.editor.show_plate();
                if let Some((name, shape)) = PRIMITIVES.get(i as usize) {
                    let shape: Value = serde_json::from_str(shape).unwrap_or_default();
                    self.worker.call("add_object", json!({ "shape": shape, "name": name.to_ascii_lowercase().replace(' ', "_") }), Kind::AddObject);
                }
            }
            ExportBambu => self.save_panel(crate::save_panel::SaveWhat::BambuProject),
            ShowInFinder => {
                if let Some(p) = &self.snap.project_path {
                    crate::os::reveal(std::path::Path::new(&p));
                }
            }
            Versions => self.read_tool("project_versions", json!({}), Kind::Versions),
            OpenFolder => self.open_folder_dialog(),
            ExportAny => self.save_panel(crate::save_panel::SaveWhat::ExportLast),
            ExportSelection => self.save_panel(crate::save_panel::SaveWhat::Selection),
            SideBySide => {
                self.editor.toggle_split();
                self.cam_ready = false;
            }
            CloseTab => self.close_active_tab(),
            ReopenTab => self.reopen_tab(),
            NextTab => self.editor.cycle(1),
            PrevTab => self.editor.cycle(-1),
            // one window per process: quitting is closing it, so both ask about unsaved changes
            CloseWindow | Quit => self.egui.send_viewport_cmd(egui::ViewportCommand::Close),
            CloseFolder => self.worker.call("workspace", json!({ "set_root": "" }), Kind::Workspace),
            ObjectSettings => {
                self.settings_object = true;
                self.cfg.layout.show(PanelId::Settings);
            }
            PlateRename => {
                self.cfg.layout.show(PanelId::Plates);
                self.plate_rename_start(self.snap.plate);
            }
            PlateDelete => {
                let k = self.snap.plate;
                let n = self.snap.project.objects.iter().filter(|o| o.plate == k).count();
                let args = json!({ "action": "remove", "plate": k });
                if n == 0 {
                    self.worker.call("plate", args, Kind::Plain);
                } else {
                    let name = self.snap.project.plate_name(k);
                    self.dialog = Some(Dialog::Confirm {
                        title: format!("Delete {name}?"),
                        body: format!("Its {n} object{} go with it. To keep them, move them to another plate first (Object > Move to Plate). Undo brings the plate back.", if n == 1 { "" } else { "s" }),
                        tool: "plate".into(),
                        args,
                    });
                }
            }
            CheckPlate => {
                self.worker.call("check", json!({}), Kind::Report);
                self.cfg.layout.show(PanelId::Checks);
            }
            FitCoupon => self.worker.call("fit", json!({ "action": "coupon" }), Kind::Report),
            Theme(t) => {
                self.set_theme(t);
                self.save_ui_now();
            }
            PrinterCamera => self.cfg.layout.show(PanelId::Printer),
            PrinterPause => self.worker.call("printer_control", json!({ "action": "pause" }), Kind::Report),
            PrinterResume => self.worker.call("printer_control", json!({ "action": "resume" }), Kind::Report),
            PrinterStop => {
                self.dialog = Some(Dialog::Confirm {
                    title: "Stop the print?".into(),
                    body: "The running print is cancelled. This cannot be undone.".into(),
                    tool: "printer_control".into(),
                    args: json!({ "action": "stop", "confirm": true }),
                })
            }
            SkipObjects => self.tool_form_with("printer_skip_objects", json!({ "object_ids": [] })),
            LoadFilament => self.tool_form_with("printer_ams", json!({ "action": "load", "ams_id": 0, "slot": 0 })),
            UnloadFilament => self.tool_form_with("printer_ams", json!({ "action": "unload" })),
            SyncSpools => self.worker.call("spool_sync", json!({}), Kind::Spools),
            PrinterFiles => self.open_tool("printer_files"),
            PrinterTimelapses => {
                self.set_section(crate::timelapses::SECTION, true);
                self.cfg.layout.show(PanelId::Printer);
            }
            AddPrinter => self.tool_form_with("printer_add", json!({ "name": "", "host": "", "serial": "", "access_code": "" })),
            ConnectAgent => self.dialog = Some(Dialog::ConnectAgent(AgentSetup::default())),
            OpenGuide => open(GUIDE_URL),
            ToolsReference => self.dialog = Some(Dialog::Tools(String::new())),
            ShowDataFolder => {
                let dir = only3d_studio::config_dir();
                let _ = std::fs::create_dir_all(&dir);
                open(&dir.display().to_string());
            }
            ReportIssue => open(ISSUES_URL),
            _ => {}
        }
    }

    /// File > Revert to Version: the saved versions of this project, newest first.
    pub(crate) fn versions_dialog(&mut self, ui: &mut egui::Ui, rows: &[Value]) -> bool {
        let p = pal();
        let mut close = false;
        ui.set_width(460.0);
        ui.label(RichText::new("Revert to a saved version").family(theme::medium()).size(14.0));
        ui.label(RichText::new("only3d keeps a version each time you save, export or print. Restoring one replaces the project; Undo brings back what you have now.").size(11.5).color(p.muted));
        ui.add_space(6.0);
        if rows.is_empty() {
            ui.label(RichText::new("No saved versions of this project yet.").size(12.5));
        }
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            for r in rows {
                let id = r["id"].as_i64().unwrap_or(0);
                let at = r["created_at"].as_str().unwrap_or("").replace('T', " ");
                let at = at.get(..16).unwrap_or(&at).to_string();
                let reason = r["reason"].as_str().unwrap_or("");
                let prints = r["prints"].as_i64().unwrap_or(0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("v{id}")).monospace().size(11.5).color(p.muted));
                    ui.label(RichText::new(format!("{at} · {reason}{}", if prints > 0 { format!(" · printed {prints}×") } else { String::new() })).size(12.5));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Restore").clicked() {
                            self.read_tool("project_versions", json!({ "version": id }), Kind::Restore(id));
                            close = true;
                        }
                    });
                });
            }
        });
        ui.add_space(6.0);
        if ui.button("Close").clicked() {
            close = true;
        }
        close
    }

    /// only3d > Connect an Agent: add only3d to the app you run agents in, whichever model runs
    /// there (`only3d mcp install <app>`), or paste its entry into any other.
    pub(crate) fn agent_dialog(&mut self, ui: &mut egui::Ui, st: &mut AgentSetup) -> bool {
        let p = pal();
        let mut close = false;
        ui.set_width(700.0);
        ui.label(RichText::new("Connect an agent").family(theme::medium()).size(15.0));
        ui.label(RichText::new("only3d is an MCP server, so any agent app can model, check, slice and (once you approve it here) print with it, whatever model runs inside. You see each step in this window.").size(12.0).color(p.muted));
        ui.add_space(12.0);
        self.agent_picker(ui, st, false);
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("With this window open, agents work on what you see.").size(11.5).color(p.placeholder));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
        close
    }

    /// The agent apps on the left (with what this Mac has), the picked one on the right: what
    /// adding only3d changes, the button, and the entry to paste. Shared by the Connect an Agent
    /// window and the last step of the first-launch setup (`compact`).
    pub(crate) fn agent_picker(&mut self, ui: &mut egui::Ui, st: &mut AgentSetup, compact: bool) {
        use only3d_studio::agents::{self, APPS};
        let p = pal();
        if !st.asked {
            st.refresh(ui.ctx());
        }
        let status = st.status.lock().unwrap_or_else(|e| e.into_inner()).clone();
        // start on the first app this Mac has that lacks only3d
        if st.pick.is_none() {
            if let Some(s) = &status {
                st.pick = Some(s.iter().position(|x| x.found && !x.added).or_else(|| s.iter().position(|x| x.found)).unwrap_or(0));
            }
        }
        let pick = st.pick.unwrap_or(0).min(APPS.len() - 1);
        let app = &APPS[pick];
        let exe = std::env::current_exe().and_then(dunce::canonicalize).unwrap_or_default();
        let places = agents::Places::from_env();

        let (list_w, row_h) = if compact { (190.0, 26.0) } else { (200.0, 30.0) };
        ui.horizontal_top(|ui| {
            // the apps
            ui.vertical(|ui| {
                ui.set_width(list_w);
                for (k, a) in APPS.iter().enumerate() {
                    if a.id == "skill" {
                        ui.add_space(4.0);
                        ui.label(RichText::new("WITHOUT MCP · ANY OTHER APP").size(10.0).color(p.placeholder));
                    }
                    let s = status.as_ref().and_then(|v| v.get(k)).copied();
                    let (r, resp) = ui.allocate_exact_size(egui::vec2(list_w, row_h), egui::Sense::click());
                    let on = k == pick;
                    if on {
                        ui.painter().rect_filled(r, 6.0, p.elevated);
                        ui.painter().rect_filled(egui::Rect::from_min_size(r.min + egui::vec2(0.0, row_h * 0.5 - 8.0), egui::vec2(2.5, 16.0)), 1.0, p.accent);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(r, 6.0, p.hover);
                    }
                    let dim = s.is_some_and(|s| !s.found);
                    ui.painter().text(r.left_center() + egui::vec2(12.0, 0.0), egui::Align2::LEFT_CENTER, a.name, egui::FontId::proportional(13.0), if dim { p.placeholder } else { p.text });
                    let (tag, col) = match s {
                        Some(s) if s.added && a.id != "other" => ("added", p.success),
                        Some(s) if !s.found => ("not found", p.placeholder),
                        _ => ("", p.muted),
                    };
                    if !tag.is_empty() {
                        ui.painter().text(r.right_center() - egui::vec2(10.0, 0.0), egui::Align2::RIGHT_CENTER, tag, egui::FontId::proportional(11.0), col);
                    }
                    if resp.clicked() {
                        st.pick = Some(k);
                        *st.out.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    }
                }
            });
            ui.add_space(16.0);
            // the picked app
            ui.vertical(|ui| {
                ui.set_min_height(if compact { 0.0 } else { 380.0 });
                let s = status.as_ref().and_then(|v| v.get(pick)).copied().unwrap_or_default();
                ui.label(RichText::new(app.name).family(theme::medium()).size(14.0));
                ui.label(RichText::new(app.models).size(12.0).color(p.muted));
                ui.add_space(10.0);
                ui.label(RichText::new(agents::plan(&places, app.id)).size(12.5));
                ui.add_space(8.0);
                if app.id != "other" {
                    let label = match (app.id, s.added) {
                        ("skill", false) => "Install the skill".to_string(),
                        ("skill", true) => "Update the skill".to_string(),
                        (_, false) => format!("Add to {}", app.name),
                        (_, true) => format!("Update in {}", app.name),
                    };
                    ui.horizontal(|ui| {
                        let go = ui.add_enabled(!st.running, theme::primary_button(label)).on_hover_text(if s.added { "only3d is already there: this points it at this copy of only3d" } else { "" });
                        if go.clicked() {
                            st.running = true;
                            *st.out.lock().unwrap_or_else(|e| e.into_inner()) = None;
                            let (out, ctx, id, exe, places) = (st.out.clone(), ui.ctx().clone(), app.id, exe.clone(), places.clone());
                            std::thread::spawn(move || {
                                let r = agents::install(&places, id, &exe);
                                *out.lock().unwrap_or_else(|e| e.into_inner()) = Some((pick, r));
                                ctx.request_repaint();
                            });
                        }
                        if st.running {
                            ui.add(egui::Spinner::new().size(12.0).color(p.accent));
                        } else if s.added {
                            ui.label(RichText::new("only3d is there").size(12.0).color(p.success));
                        } else if !s.found {
                            ui.label(RichText::new(format!("{} isn't on this Mac yet", app.name)).size(12.0).color(p.muted));
                        }
                    });
                }
                let result = st.out.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if let Some((k, r)) = result {
                    if st.running {
                        st.running = false;
                        st.refresh(ui.ctx());
                    }
                    if k == pick {
                        let (text, err) = match &r {
                            Ok(t) => (t.as_str(), false),
                            Err(t) => (t.as_str(), true),
                        };
                        ui.add_space(6.0);
                        ui.label(RichText::new(text).size(12.0).color(if err { p.error } else { p.success }));
                    }
                }
                ui.add_space(12.0);
                let manual = agents::snippet(app.id, &exe);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(if app.id == "other" { "Server entry" } else if app.id == "skill" { "The skill" } else { "Or by hand" }).size(12.0).color(p.muted));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::bracket_button(ui, "copy").clicked() {
                            ui.ctx().copy_text(manual.clone());
                            self.toast("Copied".into(), false);
                        }
                    });
                });
                egui::Frame::new().fill(p.bg).stroke(egui::Stroke::new(1.0, p.border_variant)).corner_radius(6).inner_margin(10).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::both().max_height(if compact { 96.0 } else { 170.0 }).id_salt("o3d_agent_snippet").show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(manual.as_str()).monospace().size(11.0)).selectable(true).wrap_mode(egui::TextWrapMode::Extend));
                    });
                });
            });
        });
        ui.add_space(8.0);
        ui.label(RichText::new("Using DeepSeek, Qwen, GLM or a local model? Connect the app you run it in: opencode takes any provider, and Claude Code can use DeepSeek's and Kimi's Anthropic-compatible APIs.").size(11.5).color(p.muted));
    }

    /// Help > Agent Tools Reference: every tool agents (and the palette) can call.
    pub(crate) fn tools_dialog(&mut self, ui: &mut egui::Ui, filter: &mut String) -> bool {
        let p = pal();
        let mut close = false;
        ui.set_width(640.0);
        ui.label(RichText::new("Agent tools").family(theme::medium()).size(14.0));
        ui.label(RichText::new(format!("The {} tools agents call over MCP. The app, the CLI (`only3d call <tool>`) and the palette use the same ones.", self.tools.len())).size(11.5).color(p.muted));
        ui.add_space(4.0);
        ui.add(egui::TextEdit::singleline(filter).hint_text("Filter").desired_width(f32::INFINITY));
        ui.add_space(4.0);
        let q = filter.to_lowercase();
        let mut run: Option<String> = None;
        egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            for t in self.tools.iter().filter(|t| q.is_empty() || t.name.contains(&q) || t.title.to_lowercase().contains(&q) || t.description.to_lowercase().contains(&q)) {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&t.name).monospace().size(12.0).color(p.accent));
                    ui.label(RichText::new(&t.title).size(12.5));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::bracket_button(ui, "run").clicked() {
                            run = Some(t.name.clone());
                        }
                    });
                });
                let first = t.description.split(". ").next().unwrap_or("").trim_end_matches('.');
                ui.label(RichText::new(first).size(11.5).color(p.muted));
                ui.add_space(4.0);
            }
        });
        if let Some(name) = run {
            // the run form replaces this window
            self.open_tool(&name);
            return false;
        }
        ui.add_space(4.0);
        if ui.button("Close").clicked() {
            close = true;
        }
        close
    }
}

/// Open a URL or a folder with the system (the file manager, the browser).
fn open(target: &str) {
    crate::os::open(target);
}
