//! Window chrome: a compact title bar, three docks of tabbed panes (drag tabs
//! between them, split, collapse, resize), a status bar, and the overlays — command palette,
//! preferences, shortcut sheet, dialogs and the print confirmation.

use eframe::egui::{self, Align, Color32, Layout, Margin, Pos2, Rect, RichText, Sense, Stroke, Ui};
use serde_json::{json, Value};

use crate::app::{App, Dialog, LiveState, Mode, PrintChoice, PrintStage, ToolForm};
use crate::dock::{PanelId, Side, Target};
use crate::keymap::{Action, Preset, When};
use crate::palette::{self, Entry, Item};
use crate::panels::{self, muted};
use crate::theme::{self, pal, Icon, ThemeChoice};
use crate::worker::Kind;

/// Payload of a dragged dock tab.
#[derive(Clone, Copy, Debug)]
pub struct TabDrag(pub PanelId);

/// A place a dragged tab can be dropped this frame.
#[derive(Clone, Debug)]
pub struct DropZone {
    pub rect: Rect,
    pub kind: ZoneKind,
}

#[derive(Clone, Debug)]
pub enum ZoneKind {
    /// A pane's tab strip; the insert index comes from the tab centres.
    Strip { side: Side, pane: usize, centers: Vec<f32> },
    Split { side: Side, pane: usize, after: bool },
    Dock(Side),
}

impl ZoneKind {
    fn target(&self, pointer: Pos2) -> Target {
        match self {
            ZoneKind::Strip { side, pane, centers } => Target::Tab { side: *side, pane: *pane, index: centers.iter().filter(|c| **c < pointer.x).count() },
            ZoneKind::Split { side, pane, after } => Target::Split { side: *side, pane: *pane, after: *after },
            ZoneKind::Dock(s) => Target::Dock(*s),
        }
    }
}

const MAC: bool = cfg!(target_os = "macos");
const TAB_H: f32 = 30.0;
/// Width of a folded pane in the bottom dock (a column of tab icons).
const FOLD_W: f32 = 34.0;

impl App {
    pub fn chrome(&mut self, ui: &mut Ui) {
        let p = pal();
        self.drop_targets.clear();
        egui::Panel::top("o3d_titlebar").exact_size(36.0).resizable(false).frame(egui::Frame::new().fill(p.bar).inner_margin(Margin::symmetric(10, 0))).show(ui, |ui| self.title_bar(ui));
        egui::Panel::bottom("o3d_statusbar").exact_size(26.0).resizable(false).frame(egui::Frame::new().fill(p.bar).inner_margin(Margin::symmetric(6, 0))).show(ui, |ui| self.status_bar(ui));
        if self.cfg.layout.zen {
            return;
        }
        let total = ui.available_rect_before_wrap();
        for side in [Side::Left, Side::Right, Side::Bottom] {
            if !self.cfg.layout.dock(side).visible() {
                continue;
            }
            let size = self.cfg.layout.dock(side).size;
            let frame = egui::Frame::new().fill(p.surface);
            let id = ("o3d_dock", side as u8, self.layout_gen);
            let panel = match side {
                Side::Left => egui::Panel::left(id).size_range(200.0..=(total.width() * 0.45).max(260.0)),
                Side::Right => egui::Panel::right(id).size_range(220.0..=(total.width() * 0.45).max(280.0)),
                Side::Bottom => egui::Panel::bottom(id).size_range(80.0..=(total.height() * 0.6).max(120.0)),
            };
            let resp = panel.resizable(true).default_size(size).frame(frame).show(ui, |ui| self.dock_ui(ui, side));
            let r = resp.response.rect;
            let new = if side == Side::Bottom { r.height() } else { r.width() };
            if (new - size).abs() > 0.5 && new > 40.0 {
                self.cfg.layout.dock_mut(side).size = new.round();
            }
        }
    }

    // ------------------------------------------------------------------------------------------
    // title bar

    fn title_bar(&mut self, ui: &mut Ui) {
        let p = pal();
        let full = ui.max_rect();
        // empty title-bar space drags the window (the native title bar is hidden on macOS)
        let bg = ui.interact(full, egui::Id::unique("o3d_titlebar_drag"), Sense::click_and_drag());
        if bg.drag_started() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if bg.double_clicked() {
            let max = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
        }
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if MAC {
                ui.add_space(70.0);
            }
            theme::pixel_logo(ui, 4.0);
            ui.label(RichText::new("only3d").family(theme::medium()).size(13.0).color(p.text));
            ui.label(RichText::new("/").color(p.placeholder));
            let name = self.project_name();
            ui.label(RichText::new(name).size(13.0).color(p.muted)).on_hover_text(self.snap.project_path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "not saved yet".into()));
            if self.snap.unsaved {
                let save = self.keymap.label_for(Action::Save).map(|k| format!(" ({k})")).unwrap_or_default();
                ui.label(RichText::new("Edited").size(11.0).color(p.placeholder)).on_hover_text(format!("Unsaved changes. Save{save} keeps them; closing, New and Open ask first."));
            }
            if self.snap.revision != u64::MAX {
                ui.label(RichText::new(format!("rev {}", self.snap.revision)).monospace().size(10.5).color(p.placeholder));
            }
            ui.add_space(8.0);
            let (u, r) = self.snap.can_undo;
            let t = self.keymap.label_for(Action::Undo).map(|k| format!("Undo  {k}")).unwrap_or("Undo".into());
            if ui.add_enabled_ui(u, |ui| theme::icon_button(ui, Icon::Undo, &t)).inner.clicked() {
                self.run(Action::Undo);
            }
            let t = self.keymap.label_for(Action::Redo).map(|k| format!("Redo  {k}")).unwrap_or("Redo".into());
            if ui.add_enabled_ui(r, |ui| theme::icon_button(ui, Icon::Redo, &t)).inner.clicked() {
                self.run(Action::Redo);
            }
        });

        // Prepare / Preview, centred
        let seg_w = 172.0;
        let seg = Rect::from_center_size(full.center(), egui::vec2(seg_w, 24.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(seg), |ui| {
            ui.painter().rect(seg, 5.0, p.bg, Stroke::new(1.0, p.border_variant), egui::StrokeKind::Inside);
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.add_space(2.0);
                for (m, label, a) in [(Mode::Model, "Prepare", Action::Prepare), (Mode::Toolpaths, "Preview", Action::Preview)] {
                    let on = self.mode == m;
                    let (r, resp) = ui.allocate_exact_size(egui::vec2((seg_w - 4.0) / 2.0, 20.0), Sense::click());
                    if on {
                        ui.painter().rect_filled(r, 4.0, p.selected);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(r, 4.0, p.hover);
                    }
                    ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::new(12.5, if on { theme::medium() } else { egui::FontFamily::Proportional }), if on { p.text } else { p.muted });
                    let tip = self.keymap.label_for(Action::TogglePreview).map(|k| format!("{label}  ·  toggle with {k}")).unwrap_or_else(|| label.to_string());
                    if resp.on_hover_text(tip).clicked() {
                        self.run(a);
                    }
                }
            });
        });

        // right: command palette (Slice and Print live at the bottom right of the viewport)
        let right = Rect::from_min_max(egui::pos2(seg.max.x + 12.0, full.min.y), full.max);
        ui.scope_builder(egui::UiBuilder::new().max_rect(right).layout(Layout::right_to_left(Align::Center)), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            // a search field that opens the palette
            let chord = self.keymap.label_for(Action::Palette).unwrap_or_default();
            let w = (ui.available_width() - 8.0).clamp(120.0, 260.0);
            let (r, resp) = ui.allocate_exact_size(egui::vec2(w, 24.0), Sense::click());
            ui.painter().rect(r, 4.0, if resp.hovered() { p.hover } else { p.bg }, Stroke::new(1.0, p.border_variant), egui::StrokeKind::Inside);
            theme::paint_icon(ui.painter(), Rect::from_center_size(egui::pos2(r.min.x + 13.0, r.center().y), egui::vec2(14.0, 14.0)), Icon::Search, p.placeholder);
            ui.painter().text(egui::pos2(r.min.x + 24.0, r.center().y), egui::Align2::LEFT_CENTER, "Run a command or tool…", egui::FontId::proportional(12.0), p.placeholder);
            ui.painter().text(egui::pos2(r.max.x - 8.0, r.center().y), egui::Align2::RIGHT_CENTER, chord, egui::FontId::proportional(11.0), p.placeholder);
            if resp.clicked() {
                self.run(Action::Palette);
            }
            if let Some((label, secs)) = self.worker.slow_job().map(|(l, s)| (job_label(l), s)) {
                ui.add(egui::Spinner::new().size(12.0).color(p.accent));
                ui.label(RichText::new(format!("{label} {secs:.1}s")).size(11.5).color(p.muted));
            }
        });
    }

    // ------------------------------------------------------------------------------------------
    // status bar

    fn status_bar(&mut self, ui: &mut Ui) {
        let p = pal();
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let lay = &self.cfg.layout;
            let left_on = lay.left.visible() && !lay.zen;
            let t = self.keymap.label_for(Action::ToggleDock(Side::Left)).map(|k| format!("Toggle left dock  {k}")).unwrap_or_default();
            if theme::icon_toggle(ui, Icon::DockLeft, &t, left_on).clicked() {
                self.run(Action::ToggleDock(Side::Left));
            }
            ui.add_space(4.0);
            let n = self.snap.plate_objects().count();
            let s = self.snap.selection.len();
            let text = if s > 0 { format!("{s} of {n} selected") } else { format!("{n} object{}", if n == 1 { "" } else { "s" }) };
            if ui.add(theme::ghost_button(RichText::new(text).size(11.5).color(p.muted))).on_hover_text("Show the Plates panel").clicked() {
                self.cfg.layout.show(PanelId::Plates);
            }
            let f = &self.snap.findings;
            let e = f.iter().filter(|x| x.severity == only3d_model::Severity::Error).count();
            let w = f.iter().filter(|x| x.severity == only3d_model::Severity::Warning).count();
            let (dot, txt) = if e > 0 {
                (p.error, format!("{e} error{} · {w} warning{}", if e == 1 { "" } else { "s" }, if w == 1 { "" } else { "s" }))
            } else if w > 0 {
                (p.warning, format!("{w} warning{}", if w == 1 { "" } else { "s" }))
            } else {
                (p.success, if n == 0 { "No objects, ready to start".to_string() } else { "checks pass".to_string() })
            };
            if theme::dot_chip(ui, &txt, dot).on_hover_text("Printability checks — click to show").clicked() {
                self.cfg.layout.toggle_panel(PanelId::Checks);
            }
            if self.mode == Mode::Toolpaths {
                if let Some(sl) = &self.snap.last_slice {
                    let st = &sl.result.stats;
                    ui.label(RichText::new(format!("{} · {:.1} g · {} layers", crate::app::fmt_duration(st.print_time_s), st.filament_g, st.layers)).size(11.5).monospace().color(p.muted));
                }
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let lay = self.cfg.layout.clone();
                let t = self.keymap.label_for(Action::Zen).map(|k| format!("Zen mode  {k}")).unwrap_or_default();
                if theme::icon_toggle(ui, Icon::Zen, &t, lay.zen).clicked() {
                    self.run(Action::Zen);
                }
                for side in [Side::Right, Side::Bottom] {
                    let on = lay.dock(side).visible() && !lay.zen;
                    let t = self.keymap.label_for(Action::ToggleDock(side)).map(|k| format!("Toggle {}  {k}", side.label().to_lowercase())).unwrap_or_default();
                    if theme::icon_toggle(ui, side.icon(), &t, on).clicked() {
                        self.run(Action::ToggleDock(side));
                    }
                }
                ui.add(egui::Separator::default().vertical().spacing(6.0));
                let theme_icon = if self.pal.dark { Icon::Moon } else { Icon::Sun };
                if theme::icon_button(ui, theme_icon, &format!("Theme: {} — click to switch dark / light", self.pal.name)).clicked() {
                    self.run(Action::ToggleTheme);
                }
                let km = match self.cfg.keymap {
                    Preset::Only3d => "only3d keys",
                    Preset::Bambu => "Bambu keys",
                };
                if ui.add(theme::ghost_button(RichText::new(km).size(11.5).color(p.muted))).on_hover_text("Keymap — click for the shortcut sheet").clicked() {
                    self.show_shortcuts = true;
                }
                match &self.live {
                    LiveState::Running(h) => {
                        let agents = h.meta.agents();
                        let label = if agents.is_empty() { format!("agent link · {}", h.id()) } else { format!("{} · {}", h.id(), agents.join(", ")) };
                        let who = if agents.is_empty() { "No agent is attached right now.".to_string() } else { format!("Working here now: {}.", agents.join(", ")) };
                        theme::dot_chip(ui, &label, p.success).on_hover_text(format!(
                            "This window is only3d session {id}. Agents (MCP server, `only3d call`) drive it: their edits appear here and they see your selection. {who}\n\nWith several windows open, an agent asks which one: tell it {id}.\n\nSocket: {}",
                            tilde(&h.socket),
                            id = h.id()
                        ));
                    }
                    LiveState::Failed(e) => {
                        theme::dot_chip(ui, "agent link off", p.error).on_hover_text(e.clone());
                    }
                    LiveState::Off => {
                        theme::dot_chip(ui, "agent link off", p.placeholder);
                    }
                }
                let newer = self.update.as_ref().and_then(|s| s.lock().ok().and_then(|g| g.clone()));
                if let Some(n) = newer {
                    if theme::dot_chip(ui, &format!("only3d {} is out", n.version), p.accent).on_hover_text("A newer version: click to open its download page").clicked() {
                        crate::os::open(&n.url);
                    }
                }
                let (dot, txt) = self.printer_chip();
                if theme::dot_chip(ui, &txt, dot).on_hover_text("Printer — click to show the Printer panel").clicked() {
                    self.cfg.layout.show(PanelId::Printer);
                    if self.printer_status.is_none() && !self.printers.is_empty() {
                        self.worker.call("printer_status", json!({}), Kind::PrinterStatus);
                    }
                }
            });
        });
    }

    fn printer_chip(&self) -> (Color32, String) {
        let p = pal();
        if self.printers.is_empty() {
            return (p.placeholder, "no printer".into());
        }
        let name = self.default_printer.clone().unwrap_or_else(|| self.printers[0].clone());
        match (&self.printer_status, &self.printer_data) {
            (Some(Err(_)), _) => (p.error, format!("{name} · offline")),
            (Some(Ok(_)), Some(d)) => {
                let state = d.get("gcode_state").and_then(|s| s.as_str()).unwrap_or("?").to_string();
                let pct = d.get("progress_percent").and_then(|v| v.as_u64());
                let busy = matches!(state.to_ascii_uppercase().as_str(), "RUNNING" | "PREPARE" | "SLICING");
                let dot = if busy {
                    p.accent
                } else if state.eq_ignore_ascii_case("FAILED") {
                    p.error
                } else {
                    p.success
                };
                (dot, match pct.filter(|_| busy) {
                    Some(x) => format!("{name} · {} {x}%", state.to_lowercase()),
                    None => format!("{name} · {}", state.to_lowercase()),
                })
            }
            _ => (p.placeholder, name),
        }
    }

    // ------------------------------------------------------------------------------------------
    // docks

    fn dock_ui(&mut self, ui: &mut Ui, side: Side) {
        let p = pal();
        let area = ui.max_rect();
        // claim the whole panel, or a resizable egui panel shrinks to its content
        ui.expand_to_include_rect(area);
        let n = self.cfg.layout.dock(side).panes.len();
        if n == 0 {
            return;
        }
        let vertical = side != Side::Bottom;
        let sep = 5.0;
        let len = if vertical { area.height() } else { area.width() };
        // folded panes keep just their tabs; the open ones share the rest by weight
        let folded: Vec<bool> = self.cfg.layout.dock(side).panes.iter().map(|p| p.folded).collect();
        let fold_len = if vertical { TAB_H } else { FOLD_W };
        let nf = folded.iter().filter(|f| **f).count() as f32;
        let avail = (len - sep * (n as f32 - 1.0) - fold_len * nf).max(10.0);
        let weights: Vec<f32> = self.cfg.layout.dock(side).panes.iter().map(|p| p.weight.max(0.05)).collect();
        let total: f32 = weights.iter().zip(&folded).filter(|(_, f)| !**f).map(|(w, _)| *w).sum::<f32>().max(1e-6);
        // in a side dock a pane whose content is shorter than its share lends the rest to the
        // panes that scroll (until its splitter is dragged: then it keeps the size it was given)
        let open: Vec<usize> = (0..n).filter(|i| !folded[*i]).collect();
        let wants: Vec<(f32, Option<f32>)> = open
            .iter()
            .map(|i| {
                let pane = &self.cfg.layout.dock(side).panes[*i];
                let want = pane.active_panel().filter(|_| vertical && !pane.sized).and_then(|panel| self.pane_want.get(&panel).copied());
                (weights[*i], want.map(|h| h + TAB_H + 14.0))
            })
            .collect();
        let lengths = crate::dock::pane_lengths(avail, &wants);
        let mut at = if vertical { area.min.y } else { area.min.x };
        let mut rects = Vec::with_capacity(n);
        for (i, f) in folded.iter().enumerate() {
            let l = if *f { fold_len } else { open.iter().position(|o| *o == i).map(|k| lengths[k]).unwrap_or(avail * weights[i] / total) };
            let r = if vertical { Rect::from_min_max(egui::pos2(area.min.x, at), egui::pos2(area.max.x, at + l)) } else { Rect::from_min_max(egui::pos2(at, area.min.y), egui::pos2(at + l, area.max.y)) };
            rects.push(r);
            at += l + sep;
        }
        for (pi, r) in rects.iter().enumerate() {
            self.pane_ui(ui, side, pi, *r);
        }
        // splitters
        for pi in 0..n.saturating_sub(1) {
            let a = rects[pi];
            let sr = if vertical { Rect::from_min_max(egui::pos2(area.min.x, a.max.y), egui::pos2(area.max.x, a.max.y + sep)) } else { Rect::from_min_max(egui::pos2(a.max.x, area.min.y), egui::pos2(a.max.x + sep, area.max.y)) };
            let line = if vertical { Rect::from_center_size(sr.center(), egui::vec2(sr.width(), 1.0)) } else { Rect::from_center_size(sr.center(), egui::vec2(1.0, sr.height())) };
            if folded[pi] || folded[pi + 1] {
                // nothing to resize next to a folded pane
                ui.painter().rect_filled(line, 0.0, p.border_variant);
                continue;
            }
            let resp = ui.interact(sr, egui::Id::unique(("o3d_split", side as u8, pi, self.layout_gen)), Sense::drag());
            let hot = resp.hovered() || resp.dragged();
            ui.painter().rect_filled(line, 0.0, if hot { p.accent } else { p.border_variant });
            if hot {
                ui.ctx().set_cursor_icon(if vertical { egui::CursorIcon::ResizeVertical } else { egui::CursorIcon::ResizeHorizontal });
            }
            if resp.dragged() {
                let d = if vertical { resp.drag_delta().y } else { resp.drag_delta().x };
                let dw = d / avail * total;
                let min_w = 90.0 / avail * total;
                let panes = &mut self.cfg.layout.dock_mut(side).panes;
                // from here these two are sized by hand: start from what is on screen, so
                // nothing jumps, and stop lending or borrowing room for them
                if !(panes[pi].sized && panes[pi + 1].sized) {
                    for (i, pane) in panes.iter_mut().enumerate().filter(|(i, _)| !folded[*i]) {
                        pane.weight = (if vertical { rects[i].height() } else { rects[i].width() }) / avail * total;
                    }
                    panes[pi].sized = true;
                    panes[pi + 1].sized = true;
                }
                let (wa, wb) = (panes[pi].weight + dw, panes[pi + 1].weight - dw);
                if wa > min_w && wb > min_w {
                    panes[pi].weight = wa;
                    panes[pi + 1].weight = wb;
                }
            }
        }
    }

    fn pane_ui(&mut self, ui: &mut Ui, side: Side, pi: usize, rect: Rect) {
        let p = pal();
        let pane = self.cfg.layout.dock(side).panes[pi].clone();
        if pane.folded && side == Side::Bottom {
            self.folded_column_ui(ui, side, pi, rect);
            return;
        }
        let split = self.cfg.layout.dock(side).panes.len() > 1;
        let strip = Rect::from_min_size(rect.min, egui::vec2(rect.width(), TAB_H));
        let body = Rect::from_min_max(egui::pos2(rect.min.x, strip.max.y), rect.max);
        let painter = ui.painter().clone();
        painter.rect_filled(strip, 0.0, p.surface);
        painter.line_segment([egui::pos2(strip.min.x, strip.max.y - 0.5), egui::pos2(strip.max.x, strip.max.y - 0.5)], Stroke::new(1.0, p.border_variant));

        // tabs
        let mut x = strip.min.x + 4.0;
        let mut centers = Vec::new();
        let mut clicked: Option<usize> = None;
        let mut menu_for: Option<(PanelId, usize)> = None;
        let more_w = if split { 50.0 } else { 26.0 };
        let room = strip.width() - more_w - 8.0;
        let active_ti = pane.active.min(pane.tabs.len().saturating_sub(1));
        let mut natural: Vec<f32> = pane.tabs.iter().map(|t| ui.ctx().fonts_mut(|f| f.layout_no_wrap(t.label().to_string(), egui::FontId::proportional(12.5), p.text).size().x) + 36.0).collect();
        // too narrow: inactive tabs show only their icon (hover for the name)
        let mut icon_only = vec![false; natural.len()];
        if natural.iter().sum::<f32>() > room {
            for (i, w) in natural.iter_mut().enumerate() {
                if i != active_ti {
                    *w = 30.0;
                    icon_only[i] = true;
                }
            }
        }
        let sum: f32 = natural.iter().sum();
        let squeeze = if sum > room { room / sum } else { 1.0 };
        for (ti, panel) in pane.tabs.iter().enumerate() {
            let w = (natural[ti] * squeeze).max(30.0);
            let r = Rect::from_min_size(egui::pos2(x, strip.min.y), egui::vec2(w, TAB_H - 1.0));
            x += w;
            centers.push(r.center().x);
            let id = egui::Id::unique(("o3d_tab", *panel as u8));
            let resp = ui.interact(r, id, Sense::click_and_drag());
            let active = ti == active_ti && !pane.folded;
            if active {
                painter.rect_filled(r, 0.0, p.bg);
                painter.line_segment([egui::pos2(r.min.x + 6.0, r.max.y - 1.0), egui::pos2(r.max.x - 6.0, r.max.y - 1.0)], Stroke::new(2.0, p.accent));
            } else if resp.hovered() {
                painter.rect_filled(r, 0.0, p.hover);
            }
            let col = if active { p.text } else { p.muted };
            let show_icon = w > 46.0 || squeeze >= 1.0 || icon_only[ti];
            let mut tx = r.min.x + 10.0;
            if icon_only[ti] {
                theme::paint_icon(&painter, Rect::from_center_size(r.center(), egui::vec2(14.0, 14.0)), panel.icon(), col);
            } else if show_icon {
                theme::paint_icon(&painter, Rect::from_center_size(egui::pos2(r.min.x + 16.0, r.center().y), egui::vec2(14.0, 14.0)), panel.icon(), col);
                tx = r.min.x + 27.0;
            }
            let text_room = r.max.x - tx - 6.0;
            if text_room > 12.0 && !icon_only[ti] {
                let font = egui::FontId::new(12.5, if active { theme::medium() } else { egui::FontFamily::Proportional });
                let mut job = egui::text::LayoutJob::simple_singleline(panel.label().to_string(), font, col);
                job.wrap = egui::text::TextWrapping::truncate_at_width(text_room);
                let g = ui.ctx().fonts_mut(|f| f.layout_job(job));
                painter.galley(egui::pos2(tx, r.center().y - g.size().y * 0.5), g, col);
            }
            let tip = if pane.folded {
                format!("{} — click to open, drag to move, right-click for options", panel.label())
            } else if active && split {
                format!("{} — click to fold this pane, drag to move, right-click for options", panel.label())
            } else if active {
                format!("{} — click to collapse the dock, drag to move, right-click for options", panel.label())
            } else {
                format!("{} — drag to another dock, right-click for options", panel.label())
            };
            let resp = resp.on_hover_text_at_pointer(tip);
            if resp.drag_started() {
                egui::DragAndDrop::set_payload(ui.ctx(), TabDrag(*panel));
            }
            if resp.clicked() {
                clicked = Some(ti);
            }
            resp.context_menu(|ui| {
                self.tab_menu(ui, *panel, side);
            });
            if resp.secondary_clicked() {
                menu_for = Some((*panel, ti));
            }
        }
        let _ = menu_for;
        // the "⋯" menu for the active tab
        if let Some(active) = pane.active_panel() {
            let mr = Rect::from_min_size(egui::pos2(strip.max.x - 26.0 - 2.0, strip.min.y + (TAB_H - 22.0) * 0.5), egui::vec2(22.0, 22.0));
            ui.scope_builder(egui::UiBuilder::new().max_rect(mr), |ui| {
                let resp = theme::icon_button(ui, Icon::More, "Panel options");
                egui::Popup::menu(&resp).show(|ui| {
                    self.tab_menu(ui, active, side);
                });
            });
        }
        // fold this pane to its tabs (only where other panes can use the room)
        if split {
            let fr = Rect::from_min_size(egui::pos2(strip.max.x - 50.0 - 2.0, strip.min.y + (TAB_H - 22.0) * 0.5), egui::vec2(22.0, 22.0));
            let (icon, tip) = if pane.folded { (Icon::ChevronRight, "Open this pane") } else { (Icon::ChevronDown, "Fold this pane to its tabs") };
            let hit = ui.scope_builder(egui::UiBuilder::new().max_rect(fr), |ui| theme::icon_button(ui, icon, tip).clicked()).inner;
            if hit {
                self.cfg.layout.set_folded(side, pi, !pane.folded);
            }
        }
        if let Some(ti) = clicked {
            self.cfg.layout.click_tab(side, pi, ti);
        }
        self.drop_targets.push(DropZone { rect: strip, kind: ZoneKind::Strip { side, pane: pi, centers } });
        if pane.folded {
            return;
        }

        // body
        if let Some(panel) = pane.active_panel() {
            let inner = body.shrink2(egui::vec2(10.0, 6.0));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).id_salt(("o3d_pane", panel as u8)).layout(Layout::top_down(Align::Min)));
            child.set_clip_rect(body.intersect(ui.clip_rect()));
            let measure = egui::Id::unique(crate::panels::CONTENT_HEIGHT);
            ui.ctx().data_mut(|d| d.remove::<(f32, f32)>(measure));
            self.panel_ui(&mut child, panel);
            // what the panel would fill: down to the end of its scrolling content, or of what
            // it drew when nothing scrolls
            let want = match ui.ctx().data_mut(|d| d.get_temp::<(f32, f32)>(measure)) {
                Some((top, content)) => top - inner.min.y + content,
                None => child.min_rect().height(),
            };
            self.pane_want.insert(panel, want);
        }
        // drop zones on the body: split before / after, or join as a tab
        let vertical = side != Side::Bottom;
        let (before, after, middle) = if vertical {
            let h = body.height() * 0.3;
            (Rect::from_min_max(body.min, egui::pos2(body.max.x, body.min.y + h)), Rect::from_min_max(egui::pos2(body.min.x, body.max.y - h), body.max), Rect::from_min_max(egui::pos2(body.min.x, body.min.y + h), egui::pos2(body.max.x, body.max.y - h)))
        } else {
            let w = body.width() * 0.3;
            (Rect::from_min_max(body.min, egui::pos2(body.min.x + w, body.max.y)), Rect::from_min_max(egui::pos2(body.max.x - w, body.min.y), body.max), Rect::from_min_max(egui::pos2(body.min.x + w, body.min.y), egui::pos2(body.max.x - w, body.max.y)))
        };
        self.drop_targets.push(DropZone { rect: before, kind: ZoneKind::Split { side, pane: pi, after: false } });
        self.drop_targets.push(DropZone { rect: after, kind: ZoneKind::Split { side, pane: pi, after: true } });
        self.drop_targets.push(DropZone { rect: middle, kind: ZoneKind::Strip { side, pane: pi, centers: vec![f32::NEG_INFINITY; pane.tabs.len()] } });
    }

    /// A folded pane in the bottom dock: its tabs as a column of icons.
    fn folded_column_ui(&mut self, ui: &mut Ui, side: Side, pi: usize, rect: Rect) {
        let p = pal();
        let pane = self.cfg.layout.dock(side).panes[pi].clone();
        let painter = ui.painter().clone();
        painter.rect_filled(rect, 0.0, p.surface);
        let mut y = rect.min.y + 4.0;
        let ur = Rect::from_min_size(egui::pos2(rect.center().x - 11.0, y), egui::vec2(22.0, 22.0));
        if ui.scope_builder(egui::UiBuilder::new().max_rect(ur), |ui| theme::icon_button(ui, Icon::ChevronRight, "Open this pane").clicked()).inner {
            self.cfg.layout.set_folded(side, pi, false);
        }
        y += 28.0;
        let mut clicked = None;
        for (ti, panel) in pane.tabs.iter().enumerate() {
            let r = Rect::from_min_size(egui::pos2(rect.min.x + 2.0, y), egui::vec2(rect.width() - 4.0, 30.0));
            y += 30.0;
            let resp = ui.interact(r, egui::Id::unique(("o3d_tab", *panel as u8)), Sense::click_and_drag());
            if resp.hovered() {
                painter.rect_filled(r, 3.0, p.hover);
            }
            theme::paint_icon(&painter, Rect::from_center_size(r.center(), egui::vec2(15.0, 15.0)), panel.icon(), if resp.hovered() { p.text } else { p.muted });
            let resp = resp.on_hover_text_at_pointer(format!("{} — click to open, drag to move, right-click for options", panel.label()));
            if resp.drag_started() {
                egui::DragAndDrop::set_payload(ui.ctx(), TabDrag(*panel));
            }
            if resp.clicked() {
                clicked = Some(ti);
            }
            resp.context_menu(|ui| {
                self.tab_menu(ui, *panel, side);
            });
        }
        if let Some(ti) = clicked {
            self.cfg.layout.click_tab(side, pi, ti);
        }
        self.drop_targets.push(DropZone { rect, kind: ZoneKind::Strip { side, pane: pi, centers: vec![f32::NEG_INFINITY; pane.tabs.len()] } });
    }

    fn tab_menu(&mut self, ui: &mut Ui, panel: PanelId, side: Side) {
        for s in Side::ALL {
            if s == side {
                continue;
            }
            if ui.button(format!("Move to {}", s.label())).clicked() {
                let target = if self.cfg.layout.dock(s).panes.is_empty() { Target::Dock(s) } else { Target::Tab { side: s, pane: 0, index: usize::MAX } };
                self.cfg.layout.place(panel, target);
                ui.close();
            }
        }
        if ui.button("Split into its own pane").clicked() {
            self.cfg.layout.place(panel, Target::Dock(side));
            ui.close();
        }
        ui.separator();
        let at = self.cfg.layout.find(panel);
        if let Some((s, pi, _)) = at.filter(|(s, _, _)| self.cfg.layout.dock(*s).panes.len() > 1) {
            let folded = self.cfg.layout.dock(s).panes[pi].folded;
            if ui.button(if folded { "Open this pane" } else { "Fold this pane to its tabs" }).clicked() {
                self.cfg.layout.set_folded(s, pi, !folded);
                ui.close();
            }
        }
        if ui.button("Collapse dock").clicked() {
            self.cfg.layout.dock_mut(side).open = false;
            ui.close();
        }
        if ui.button(format!("Hide {}", panel.label())).on_hover_text("Take it out of the docks. Bring it back with Add a panel in any pane's menu.").clicked() {
            self.cfg.layout.close(panel);
            ui.close();
        }
        let hidden = self.cfg.layout.hidden_panels();
        if let Some((s, pi, _)) = at.filter(|_| !hidden.is_empty()) {
            ui.menu_button("Add a panel here", |ui| {
                for h in hidden {
                    if ui.button(h.label()).clicked() {
                        self.cfg.layout.place(h, Target::Tab { side: s, pane: pi, index: usize::MAX });
                        self.cfg.layout.show(h);
                        ui.close();
                    }
                }
            });
        }
        ui.separator();
        if ui.button("Reset layout").clicked() {
            self.reset_layout();
            ui.close();
        }
    }

    /// While a tab is dragged: highlight the zone under the pointer and drop it there on release.
    fn tab_drop(&mut self, ctx: &egui::Context) {
        let Some(payload) = egui::DragAndDrop::payload::<TabDrag>(ctx) else { return };
        let p = pal();
        let panel = payload.0;
        // edges for docks that are not on screen
        let vp = self.vp_rect;
        if vp.is_positive() {
            for side in Side::ALL {
                if self.cfg.layout.dock(side).visible() && !self.cfg.layout.zen {
                    continue;
                }
                let r = match side {
                    Side::Left => Rect::from_min_size(vp.min, egui::vec2(72.0, vp.height())),
                    Side::Right => Rect::from_min_max(egui::pos2(vp.max.x - 72.0, vp.min.y), vp.max),
                    Side::Bottom => Rect::from_min_max(egui::pos2(vp.min.x + 72.0, vp.max.y - 64.0), egui::pos2(vp.max.x - 72.0, vp.max.y)),
                };
                self.drop_targets.push(DropZone { rect: r, kind: ZoneKind::Dock(side) });
            }
        }
        let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("o3d_tab_drop"));
        let painter = ctx.layer_painter(layer);
        let pointer = ctx.pointer_latest_pos();
        // faint hint for the edge zones
        for z in &self.drop_targets {
            if let ZoneKind::Dock(side) = z.kind {
                painter.rect(z.rect.shrink(4.0), 6.0, p.accent.gamma_multiply(0.08), Stroke::new(1.0, p.accent.gamma_multiply(0.5)), egui::StrokeKind::Inside);
                painter.text(z.rect.center(), egui::Align2::CENTER_CENTER, side.label().replace(" Dock", ""), egui::FontId::monospace(10.5), p.accent);
            }
        }
        let hit = pointer.and_then(|pt| {
            let z = self.drop_targets.iter().rev().find(|z| z.rect.contains(pt)).cloned()?;
            Some((pt, z))
        });
        if let Some((pt, z)) = &hit {
            let r = match &z.kind {
                ZoneKind::Strip { centers, .. } if centers.iter().all(|c| c.is_finite()) => {
                    let idx = centers.iter().filter(|c| **c < pt.x).count();
                    let x = if centers.is_empty() {
                        z.rect.min.x + 4.0
                    } else if idx == 0 {
                        z.rect.min.x + 2.0
                    } else if idx >= centers.len() {
                        centers[centers.len() - 1] + (centers[centers.len() - 1] - z.rect.min.x).min(80.0)
                    } else {
                        (centers[idx - 1] + centers[idx]) * 0.5
                    };
                    Rect::from_min_size(egui::pos2(x - 1.5, z.rect.min.y + 4.0), egui::vec2(3.0, z.rect.height() - 8.0))
                }
                _ => z.rect.shrink(3.0),
            };
            painter.rect(r, 4.0, p.accent.gamma_multiply(0.18), Stroke::new(1.5, p.accent), egui::StrokeKind::Inside);
        }
        if let Some(pt) = pointer {
            let g = painter.layout_no_wrap(panel.label().to_string(), egui::FontId::proportional(12.0), p.text);
            let r = Rect::from_min_size(pt + egui::vec2(12.0, 8.0), g.size() + egui::vec2(16.0, 8.0));
            painter.rect(r, 4.0, p.elevated, Stroke::new(1.0, p.accent), egui::StrokeKind::Inside);
            painter.galley(r.min + egui::vec2(8.0, 4.0), g, p.text);
        }
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        if ctx.input(|i| i.pointer.any_released()) {
            egui::DragAndDrop::clear_payload(ctx);
            if let Some((pt, z)) = hit {
                let t = z.kind.target(pt);
                self.cfg.layout.place(panel, t);
            }
        }
    }

    // ------------------------------------------------------------------------------------------
    // collapsible section header (mono label)

    /// A panel's title row with its buttons on the right (no folding: the panel is the section).
    pub fn panel_bar(&mut self, ui: &mut Ui, label: &str, right: impl FnOnce(&mut Ui)) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), Sense::hover());
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(4.0, 0.0))).layout(Layout::right_to_left(Align::Center)));
        right(&mut child);
        bar_title(ui, rect, 6.0, label, &child);
    }

    pub fn section_header(&mut self, ui: &mut Ui, id: &str, label: &str, default_open: bool, force_open: bool, right: impl FnOnce(&mut Ui)) -> bool {
        let p = pal();
        let open = force_open || self.section_open(id, default_open);
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(rect, 3.0, p.hover);
        }
        let ic = Rect::from_center_size(egui::pos2(rect.min.x + 8.0, rect.center().y), egui::vec2(12.0, 12.0));
        theme::paint_icon(ui.painter(), ic, if open { Icon::ChevronDown } else { Icon::ChevronRight }, p.placeholder);
        // a child that does not allocate, so the parent's cursor stays below the header
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(4.0, 0.0))).layout(Layout::right_to_left(Align::Center)));
        right(&mut child);
        bar_title(ui, rect, 18.0, label, &child);
        if resp.clicked() && !force_open {
            self.set_section(id, !open);
        }
        open
    }

    // ------------------------------------------------------------------------------------------
    // overlays

    pub fn overlays(&mut self, ctx: &egui::Context) {
        self.tab_drop(ctx);
        self.print_modal_ui(ctx);
        if self.palette.open {
            self.palette_ui(ctx);
        }
        if self.show_prefs {
            self.prefs_ui(ctx);
        }
        if self.show_shortcuts {
            self.shortcuts_ui(ctx);
        }
        if self.dialog.is_some() {
            self.dialog_ui(ctx);
        }
        // last, so it sits above everything else
        self.welcome_ui(ctx);
        self.approvals.ui(ctx);
        self.leave_prompt_ui(ctx);
    }

    /// Closing, New or Open with unsaved changes (`App::leave`): Save / Don't Save / Cancel,
    /// laid out the platform's way. macOS keeps Don't Save apart on the left and Save last;
    /// Windows and Linux lead with Save. Enter saves, Esc cancels; a click outside does nothing.
    fn leave_prompt_ui(&mut self, ctx: &egui::Context) {
        #[derive(Clone, Copy)]
        enum Pick {
            Save,
            DontSave,
            Cancel,
        }
        let Some(what) = self.leave_prompt.clone() else { return };
        let p = pal();
        let name = self.project_name();
        let name = name.trim_end_matches(".only3d.json");
        let untitled = self.snap.project_path.is_none();
        // Save on a project with no file yet goes on to the Save panel
        let save_label = if untitled { "Save…" } else { "Save" };
        let mut pick = ctx.input(|i| {
            if i.key_pressed(egui::Key::Escape) {
                Some(Pick::Cancel)
            } else if i.key_pressed(egui::Key::Enter) {
                Some(Pick::Save)
            } else {
                None
            }
        });
        egui::Modal::new(egui::Id::unique("o3d_close_prompt")).backdrop_color(Self::backdrop()).frame(Self::modal_frame()).show(ctx, |ui| {
            ui.set_width(420.0);
            egui::Frame::new().inner_margin(Margin::same(18)).show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    theme::pixel_logo(ui, 6.0);
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.add(egui::Label::new(RichText::new(format!("Do you want to save the changes you made to “{name}”?")).family(theme::medium()).size(14.0)).wrap());
                        ui.add_space(2.0);
                        ui.add(egui::Label::new(RichText::new("Your changes will be lost if you don't save them.").size(12.5).color(p.muted)).wrap());
                    });
                });
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if MAC {
                        if ui.button("Don't Save").clicked() {
                            pick = Some(Pick::DontSave);
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.add(theme::primary_button(save_label)).clicked() {
                                pick = Some(Pick::Save);
                            }
                            if ui.button("Cancel").clicked() {
                                pick = Some(Pick::Cancel);
                            }
                        });
                    } else {
                        // right to left: the last added sits leftmost
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("Cancel").clicked() {
                                pick = Some(Pick::Cancel);
                            }
                            if ui.button("Don't Save").clicked() {
                                pick = Some(Pick::DontSave);
                            }
                            if ui.add(theme::primary_button(save_label)).clicked() {
                                pick = Some(Pick::Save);
                            }
                        });
                    }
                });
            });
        });
        let Some(pick) = pick else { return };
        self.leave_prompt = None;
        match pick {
            Pick::Save => {
                // goes on when the save lands (`Kind::Save`); a failed or cancelled save keeps
                // the project
                self.after_save = Some(what);
                self.save(false);
            }
            Pick::DontSave => self.leave_now(what),
            Pick::Cancel => {}
        }
    }

    pub(crate) fn modal_frame() -> egui::Frame {
        let p = pal();
        egui::Frame::new().fill(p.elevated).stroke(Stroke::new(1.0, p.border)).corner_radius(8).inner_margin(Margin::same(0)).shadow(egui::Shadow { offset: [0, 10], blur: 30, spread: 0, color: Color32::from_black_alpha(if p.dark { 110 } else { 40 }) })
    }

    pub(crate) fn backdrop() -> Color32 {
        Color32::from_black_alpha(if pal().dark { 70 } else { 30 })
    }

    fn palette_items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        for a in Action::all() {
            items.push(Item { entry: Entry::Action(a), label: format!("{}: {}", a.category(), a.label()), detail: a.id(), chord: self.keymap.label_for(a) });
        }
        for t in &self.tools {
            let first = t.description.split(". ").next().unwrap_or("").trim_end_matches('.');
            items.push(Item { entry: Entry::Tool(t.name.clone()), label: format!("Tool: {} — {}", t.name, t.title), detail: format!("{} {}", t.name, first), chord: None });
        }
        items
    }

    fn palette_ui(&mut self, ctx: &egui::Context) {
        let p = pal();
        let items = self.palette_items();
        let mut order = palette::filter(&items, &self.palette.query);
        if self.palette.query.trim().is_empty() {
            // recently used first
            let recent: Vec<usize> = self.palette.recent.iter().filter_map(|id| items.iter().position(|it| matches!(&it.entry, Entry::Action(a) if a.id() == *id))).collect();
            order.retain(|i| !recent.contains(i));
            order = recent.into_iter().chain(order).collect();
        }
        let n = order.len();
        self.palette.selected = self.palette.selected.min(n.saturating_sub(1));
        let (down, up, enter) = ctx.input_mut(|i| (i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown), i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp), i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)));
        if down && n > 0 {
            self.palette.selected = (self.palette.selected + 1) % n;
        }
        if up && n > 0 {
            self.palette.selected = (self.palette.selected + n - 1) % n;
        }
        let mut run: Option<usize> = if enter { order.get(self.palette.selected).copied() } else { None };
        let area = egui::Modal::default_area(egui::Id::unique("o3d_palette")).anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 64.0));
        let resp = egui::Modal::new(egui::Id::unique("o3d_palette")).area(area).backdrop_color(Color32::from_black_alpha(if p.dark { 40 } else { 14 })).frame(Self::modal_frame()).show(ctx, |ui| {
            ui.set_width(600.0);
            egui::Frame::new().inner_margin(Margin::symmetric(12, 10)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 22.0), Sense::hover());
                    theme::paint_icon(ui.painter(), r.shrink2(egui::vec2(0.0, 3.0)), Icon::Command, p.muted);
                    let te = egui::TextEdit::singleline(&mut self.palette.query).hint_text("Run a command, show a panel, or call a tool…").frame(egui::Frame::NONE).desired_width(f32::INFINITY).font(egui::FontId::proportional(14.0));
                    let r = ui.add(te);
                    if self.palette.focus {
                        r.request_focus();
                        self.palette.focus = false;
                    }
                    if r.changed() {
                        self.palette.selected = 0;
                    }
                });
            });
            let (sr, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), Sense::hover());
            ui.painter().rect_filled(sr, 0.0, p.border_variant);
            egui::ScrollArea::vertical().max_height(380.0).auto_shrink([false, true]).show(ui, |ui| {
                ui.add_space(4.0);
                if n == 0 {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        ui.label(RichText::new("No matches").color(p.muted));
                    });
                    ui.add_space(8.0);
                }
                for (row, &i) in order.iter().enumerate().take(200) {
                    let it = &items[i];
                    let selected = row == self.palette.selected;
                    let (r, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), Sense::click());
                    let r = r.shrink2(egui::vec2(6.0, 0.0));
                    if selected {
                        ui.painter().rect_filled(r, 4.0, p.selected);
                        if up || down {
                            ui.scroll_to_rect(r, None);
                        }
                    } else if resp.hovered() {
                        ui.painter().rect_filled(r, 4.0, p.hover);
                    }
                    let (cat, label) = it.label.split_once(": ").unwrap_or(("", &it.label));
                    let mut job = egui::text::LayoutJob::default();
                    if !cat.is_empty() {
                        job.append(&format!("{cat}: "), 0.0, egui::TextFormat { font_id: egui::FontId::proportional(13.0), color: p.muted, ..Default::default() });
                    }
                    job.append(label, 0.0, egui::TextFormat { font_id: egui::FontId::proportional(13.0), color: p.text, ..Default::default() });
                    job.wrap = egui::text::TextWrapping::truncate_at_width(r.width() - 140.0);
                    let g = ui.ctx().fonts_mut(|f| f.layout_job(job));
                    ui.painter().galley(egui::pos2(r.min.x + 10.0, r.center().y - g.size().y * 0.5), g, p.text);
                    if let Some(c) = &it.chord {
                        let g = ui.painter().layout_no_wrap(c.clone(), egui::FontId::proportional(11.5), p.muted);
                        let kr = Rect::from_min_size(egui::pos2(r.max.x - 10.0 - g.size().x - 10.0, r.center().y - 9.0), egui::vec2(g.size().x + 10.0, 18.0));
                        ui.painter().rect(kr, 3.0, p.bg, Stroke::new(1.0, p.border_variant), egui::StrokeKind::Inside);
                        ui.painter().galley(kr.min + egui::vec2(5.0, 9.0 - g.size().y * 0.5), g, p.muted);
                    } else if let Entry::Tool(name) = &it.entry {
                        let tag = self.tools.iter().find(|t| &t.name == name).map(|t| if t.side_effects { "tool · external" } else if t.mutates { "tool · edits" } else { "tool" }).unwrap_or("tool");
                        ui.painter().text(egui::pos2(r.max.x - 10.0, r.center().y), egui::Align2::RIGHT_CENTER, tag, egui::FontId::monospace(10.0), p.placeholder);
                    }
                    if resp.hovered() && ctx.input(|i| i.pointer.delta().length_sq() > 0.0) {
                        self.palette.selected = row;
                    }
                    if resp.clicked() {
                        run = Some(i);
                    }
                }
                ui.add_space(4.0);
            });
            let (sr, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), Sense::hover());
            ui.painter().rect_filled(sr, 0.0, p.border_variant);
            egui::Frame::new().inner_margin(Margin::symmetric(12, 6)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} commands · {} tools", Action::all().len(), self.tools.len())).size(11.0).color(p.placeholder));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new("↑↓ select · ⏎ run · esc close").size(11.0).color(p.placeholder));
                    });
                });
            });
        });
        if resp.should_close() {
            self.palette.open = false;
        }
        if let Some(i) = run {
            self.palette.open = false;
            match items[i].entry.clone() {
                Entry::Action(a) => self.run(a),
                Entry::Tool(name) => self.open_tool(&name),
            }
        }
    }

    /// Run a studio tool from the palette: directly when it takes no arguments, else via a form.
    pub fn open_tool(&mut self, name: &str) {
        if name == "print" {
            // printing always goes through the preflight dialog
            self.start_print();
            return;
        }
        let Some(t) = self.tools.iter().find(|t| t.name == name).cloned() else { return };
        if t.no_args() {
            self.worker.call(name, json!({}), Kind::ToolForm);
            let mut form = ToolForm::new(t, &json!({}));
            (form.running, form.focus) = (true, false);
            self.dialog = Some(Dialog::ToolForm(form));
        } else {
            // every required argument starts with a value of its type
            let args = serde_json::from_str(&palette::args_template(&t.schema)).unwrap_or_else(|_| json!({}));
            self.dialog = Some(Dialog::ToolForm(ToolForm::new(t, &args)));
        }
    }

    fn prefs_ui(&mut self, ctx: &egui::Context) {
        let p = pal();
        let mut close = false;
        let resp = egui::Modal::new(egui::Id::unique("o3d_prefs")).backdrop_color(Self::backdrop()).frame(Self::modal_frame()).show(ctx, |ui| {
            ui.set_width(520.0);
            egui::Frame::new().inner_margin(Margin::same(16)).show(ui, |ui| {
                ui.set_max_width(488.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Preferences").family(theme::medium()).size(15.0));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if theme::icon_button(ui, Icon::Close, "Close").clicked() {
                            close = true;
                        }
                    });
                });
                ui.add_space(8.0);
                theme::section_label(ui, "Theme");
                ui.horizontal_wrapped(|ui| {
                    for t in ThemeChoice::ALL {
                        let pal_t = t.palette(self.pal.dark);
                        let on = self.cfg.theme == t;
                        let (r, resp) = ui.allocate_exact_size(egui::vec2(116.0, 58.0), Sense::click());
                        ui.painter().rect(r, 6.0, pal_t.bg, Stroke::new(if on { 2.0 } else { 1.0 }, if on { p.accent } else { p.border }), egui::StrokeKind::Inside);
                        let bar = Rect::from_min_size(r.min + egui::vec2(1.0, 1.0), egui::vec2(r.width() - 2.0, 12.0));
                        ui.painter().rect_filled(bar, egui::CornerRadius { nw: 5, ne: 5, sw: 0, se: 0 }, pal_t.bar);
                        ui.painter().rect_filled(Rect::from_min_size(r.min + egui::vec2(1.0, 13.0), egui::vec2(28.0, r.height() - 14.0)), 0.0, pal_t.surface);
                        ui.painter().rect_filled(Rect::from_min_size(r.min + egui::vec2(36.0, 20.0), egui::vec2(44.0, 4.0)), 2.0, pal_t.accent);
                        ui.painter().rect_filled(Rect::from_min_size(r.min + egui::vec2(36.0, 28.0), egui::vec2(60.0, 3.0)), 2.0, pal_t.muted);
                        ui.painter().text(egui::pos2(r.min.x + 36.0, r.max.y - 9.0), egui::Align2::LEFT_CENTER, if t == ThemeChoice::System { "System".to_string() } else { t.label().to_string() }, egui::FontId::proportional(10.5), pal_t.text);
                        if resp.on_hover_text(t.label()).clicked() {
                            self.set_theme(t);
                        }
                    }
                });
                ui.add_space(10.0);
                theme::section_label(ui, "Keymap");
                for k in Preset::ALL {
                    if theme::radio(ui, self.cfg.keymap == k, k.label()).clicked() && self.cfg.keymap != k {
                        self.set_keymap(k);
                    }
                }
                let path = crate::keymap::keymap_path();
                ui.add(egui::Label::new(RichText::new(format!("Overrides: {}", tilde(&path))).size(11.5).color(p.muted)).truncate()).on_hover_text(path.display().to_string());
                ui.horizontal(|ui| {
                    if theme::bracket_button(ui, "edit").on_hover_text("Create keymap.json if needed and open it").clicked() {
                        if !path.exists() {
                            if let Some(d) = path.parent() {
                                let _ = std::fs::create_dir_all(d);
                            }
                            let _ = std::fs::write(&path, "{\n  \"bindings\": {},\n  \"prepare\": {},\n  \"preview\": {}\n}\n");
                        }
                        crate::os::open_text(&path);
                    }
                    if theme::bracket_button(ui, "reload").clicked() {
                        let k = self.cfg.keymap;
                        self.set_keymap(k);
                        self.toast("Keymap reloaded".into(), false);
                    }
                });
                ui.add(egui::Label::new(RichText::new("Format: {\"bindings\": {\"cmd-d\": \"edit.duplicate\", \"cmd-k\": null}, \"prepare\": {…}, \"preview\": {…}} — action ids are in the shortcut sheet (hover a row).").size(11.0).color(p.placeholder)).wrap());
                ui.add_space(10.0);
                theme::section_label(ui, "Viewport");
                let mut has_cam = self.cfg.camera_xy.is_some();
                ui.horizontal(|ui| {
                    if theme::checkbox(ui, &mut has_cam, "Printer camera at").changed() {
                        self.cfg.camera_xy = if has_cam { Some([175.0, 320.0]) } else { None };
                    }
                    if let Some(c) = self.cfg.camera_xy.as_mut() {
                        ui.add(egui::DragValue::new(&mut c[0]).range(-50.0..=400.0).speed(1.0).prefix("x ").suffix(" mm"));
                        ui.add(egui::DragValue::new(&mut c[1]).range(-50.0..=400.0).speed(1.0).prefix("y ").suffix(" mm"));
                    }
                });
                ui.label(RichText::new("Measured from the plate's front-left corner. The camera button in the view strip shows it as a vertical line.").size(11.0).color(p.placeholder));
                ui.add_space(10.0);
                theme::section_label(ui, "When a print ends");
                // Windows has no desktop notification yet (os::notify), so no checkbox there
                if !cfg!(windows) {
                    theme::checkbox(ui, &mut self.cfg.notify_desktop, if MAC { "Show a macOS notification" } else { "Show a desktop notification" });
                }
                ui.horizontal(|ui| {
                    ui.label("Also send to");
                    ui.add(egui::TextEdit::singleline(&mut self.cfg.notify_url).hint_text("https://ntfy.sh/your-private-topic").desired_width(240.0));
                });
                ui.label(RichText::new("Optional: an ntfy topic URL gets \"Print finished: <job>\" so your phone hears about it. Leave empty to send nothing.").size(11.0).color(p.placeholder));
                ui.add_space(10.0);
                theme::section_label(ui, "Updates");
                theme::checkbox(ui, &mut self.cfg.check_updates, "Check for a newer only3d once a day");
                ui.label(RichText::new("Asks GitHub for the latest release; nothing about you or your projects is sent. A newer version shows in the status bar.").size(11.0).color(p.placeholder));
                ui.add_space(10.0);
                theme::section_label(ui, "Moving and sizing");
                ui.horizontal(|ui| {
                    ui.label("Handles");
                    let g = &mut self.cfg.gizmo;
                    ui.selectable_value(&mut g.style, "triad".to_string(), "All together (CAD)").on_hover_text("Move arrows, rotate ring and size handles on every selection, like Fusion or Onshape");
                    ui.selectable_value(&mut g.style, "tools".to_string(), "One tool at a time").on_hover_text("M move, R rotate, S scale, like Bambu Studio");
                });
                theme::checkbox(ui, &mut self.cfg.gizmo.smart_guides, format!("Smart guides and gap labels while dragging (hold {} to skip)", crate::keymap::CMD));
                ui.horizontal(|ui| {
                    ui.label("Move snaps to");
                    ui.add(egui::DragValue::new(&mut self.cfg.gizmo.move_snap_mm).range(0.0..=50.0).speed(0.1).suffix(" mm"));
                    ui.label(RichText::new(format!("{} while dragging: no snapping", crate::keymap::CMD)).size(11.0).color(p.placeholder));
                });
                ui.horizontal(|ui| {
                    ui.label("Rotation snaps to");
                    ui.add(egui::DragValue::new(&mut self.cfg.gizmo.rotate_snap_deg).range(0.0..=90.0).speed(0.5).suffix("°"));
                    ui.label(RichText::new(format!("{} for 1°", crate::keymap::CMD)).size(11.0).color(p.placeholder));
                });
                ui.add_space(10.0);
                theme::section_label(ui, "Interface");
                ui.horizontal(|ui| {
                    ui.label("UI scale");
                    // applied when the pointer lets go: rescaling mid-drag moves the slider
                    // under the pointer and it jumps to another value
                    let mut s = self.ui_scale_pending.unwrap_or(self.cfg.ui_scale);
                    let r = ui.add(egui::Slider::new(&mut s, 0.8..=1.5).step_by(0.05).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)));
                    if r.changed() {
                        self.ui_scale_pending = Some(s);
                    }
                    if let Some(v) = self.ui_scale_pending {
                        if !r.is_pointer_button_down_on() && !r.has_focus() {
                            self.cfg.ui_scale = v;
                            self.ui_scale_pending = None;
                        } else {
                            ui.label(RichText::new("applies when you let go").size(11.0).color(p.placeholder));
                        }
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("Reset layout").clicked() {
                        self.reset_layout();
                    }
                    let mut zen = self.cfg.layout.zen;
                    if theme::checkbox(ui, &mut zen, "Zen mode").changed() {
                        self.cfg.layout.zen = zen;
                    }
                });
                let hidden = self.cfg.layout.hidden_panels();
                if !hidden.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Hidden panels:").size(12.0).color(p.muted));
                        for h in hidden {
                            if ui.small_button(format!("Show {}", h.label())).clicked() {
                                self.cfg.layout.show(h);
                            }
                        }
                    });
                }
                ui.add_space(10.0);
                theme::section_label(ui, "Graphics");
                let soft = self.gpu.device_type == egui_wgpu::wgpu::DeviceType::Cpu;
                // the summary already says "(software)"; name the kind of real GPU
                ui.label(if soft { format!("Drawing with {}", crate::adapter::summary(&self.gpu)) } else { format!("Drawing with {} ({})", crate::adapter::summary(&self.gpu), crate::adapter::kind_name(self.gpu.device_type)) });
                if soft {
                    ui.label(RichText::new("No GPU was found, so the 3D view is drawn in software and is slower. Updating the graphics driver may help.").size(11.0).color(p.warning));
                }
                ui.label(RichText::new("only3d picks a graphics card first, then a built-in GPU, and software only when there is neither.").size(11.0).color(p.placeholder));
                ui.add_space(6.0);
                ui.add(egui::Label::new(RichText::new(format!("Saved to {}", tilde(&crate::config::path()))).size(11.0).color(p.placeholder)).truncate());
            });
        });
        if resp.should_close() || close {
            self.show_prefs = false;
        }
    }

    fn shortcuts_ui(&mut self, ctx: &egui::Context) {
        let p = pal();
        let mut close = false;
        let h = (ctx.content_rect().height() * 0.78).max(300.0);
        let resp = egui::Modal::new(egui::Id::unique("o3d_shortcuts")).backdrop_color(Self::backdrop()).frame(Self::modal_frame()).show(ctx, |ui| {
            ui.set_width(700.0);
            egui::Frame::new().inner_margin(Margin::symmetric(16, 12)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Keyboard shortcuts").family(theme::medium()).size(15.0));
                    theme::chip(ui, self.cfg.keymap.label(), p.accent, theme::soft(p.accent));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if theme::icon_button(ui, Icon::Close, "Close").clicked() {
                            close = true;
                        }
                        if theme::bracket_button(ui, "preferences").clicked() {
                            close = true;
                            self.show_prefs = true;
                        }
                    });
                });
                ui.add(egui::TextEdit::singleline(&mut self.shortcut_query).hint_text("Filter…").desired_width(f32::INFINITY));
            });
            egui::ScrollArea::vertical().max_height(h - 90.0).min_scrolled_height(h - 90.0).auto_shrink([false, true]).show(ui, |ui| {
                egui::Frame::new().inner_margin(Margin::symmetric(16, 4)).show(ui, |ui| {
                    let q = self.shortcut_query.trim().to_string();
                    let mut cats: Vec<&'static str> = Vec::new();
                    for a in Action::all() {
                        if !cats.contains(&a.category()) {
                            cats.push(a.category());
                        }
                    }
                    ui.columns(2, |cols| {
                        let mut col = 0;
                        for cat in cats {
                            let rows: Vec<(Action, Vec<(When, crate::keymap::Chord)>)> = Action::all()
                                .into_iter()
                                .filter(|a| a.category() == cat)
                                .map(|a| (a, self.keymap.chords(a)))
                                .filter(|(_, c)| !c.is_empty())
                                .filter(|(a, _)| q.is_empty() || palette::fuzzy_score(&q, &a.label()).is_some() || a.id().contains(&q))
                                .collect();
                            if rows.is_empty() {
                                continue;
                            }
                            let ui = &mut cols[col];
                            col = 1 - col;
                            ui.add_space(6.0);
                            theme::section_label(ui, cat);
                            for (a, chords) in rows {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(a.label()).size(12.5)).on_hover_text(a.id());
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        ui.spacing_mut().item_spacing.x = 3.0;
                                        for (w, c) in chords.iter().rev().take(3) {
                                            let label = match w {
                                                When::Always => c.label(),
                                                When::Prepare => format!("{} ·prep", c.label()),
                                                When::Preview => format!("{} ·prev", c.label()),
                                            };
                                            theme::kbd(ui, &label);
                                        }
                                    });
                                });
                            }
                        }
                    });
                    ui.add_space(8.0);
                    ui.label(RichText::new("Mouse: drag empty space to orbit · right / middle / ⇧-drag to pan · scroll to zoom · drag an object to move it · double-click to frame. Arrow nudges follow the screen. ·prep / ·prev = only in Prepare / Preview.").size(11.0).color(p.muted));
                    ui.add_space(8.0);
                });
            });
        });
        if resp.should_close() || close {
            self.show_shortcuts = false;
        }
    }

    fn dialog_ui(&mut self, ctx: &egui::Context) {
        let p = pal();
        let Some(mut dialog) = self.dialog.take() else { return };
        let mut keep = true;
        let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
        let resp = egui::Modal::new(egui::Id::unique("o3d_dialog")).backdrop_color(Self::backdrop()).frame(Self::modal_frame()).show(ctx, |ui| {
            egui::Frame::new().inner_margin(Margin::same(16)).show(ui, |ui| match &mut dialog {
                Dialog::Rename { id, text, focus } => {
                    ui.set_width(380.0);
                    ui.label(RichText::new("Rename object").family(theme::medium()).size(14.0));
                    ui.label(RichText::new(format!("id: {id}")).size(11.0).monospace().color(p.muted));
                    let r = ui.add(egui::TextEdit::singleline(text).desired_width(f32::INFINITY));
                    if *focus {
                        r.request_focus();
                        *focus = false;
                    }
                    ui.add_space(6.0);
                    let ok = ui
                        .horizontal(|ui| {
                            let ok = ui.add(theme::primary_button("Rename")).clicked();
                            if ui.button("Cancel").clicked() {
                                keep = false;
                            }
                            ok
                        })
                        .inner;
                    if ok || (enter && r.lost_focus()) {
                        self.worker.call("update_object", json!({ "id": id, "name": text.trim() }), Kind::Plain);
                        keep = false;
                    }
                }
                Dialog::SavePreset { kind, name, focus } => {
                    ui.set_width(420.0);
                    ui.label(RichText::new(format!("Save {kind} preset")).family(theme::medium()).size(14.0));
                    ui.label(RichText::new("Saves the current preset plus your overrides as a new only3d preset (also usable from Bambu Studio's user folder when exported).").size(11.5).color(p.muted));
                    let r = ui.add(egui::TextEdit::singleline(name).hint_text("Preset name").desired_width(f32::INFINITY));
                    if *focus {
                        r.request_focus();
                        *focus = false;
                    }
                    ui.add_space(6.0);
                    let ok = ui
                        .horizontal(|ui| {
                            let ok = ui.add_enabled(!name.trim().is_empty(), theme::primary_button("Save preset")).clicked();
                            if ui.button("Cancel").clicked() {
                                keep = false;
                            }
                            ok
                        })
                        .inner;
                    if (ok || (enter && r.lost_focus())) && !name.trim().is_empty() {
                        let base = if kind == "process" { self.snap.project.process.preset.clone() } else { self.snap.project.filaments.first().map(|f| f.preset.clone()).unwrap_or_default() };
                        self.worker.call("preset_create", json!({ "kind": kind, "name": name.trim(), "base": base, "from_project": true, "apply": true }), Kind::PresetChanged(kind.clone()));
                        keep = false;
                    }
                }
                Dialog::Copies(f) => {
                    if self.copies_dialog(ui, f, enter) {
                        keep = false;
                    }
                }
                Dialog::Versions(rows) => {
                    if self.versions_dialog(ui, rows) {
                        keep = false;
                    }
                }
                Dialog::ConnectAgent(st) => {
                    if self.agent_dialog(ui, st) {
                        keep = false;
                    }
                }
                Dialog::Tools(filter) => {
                    if self.tools_dialog(ui, filter) {
                        keep = false;
                    }
                }
                Dialog::Save(d) => {
                    if self.save_dialog_ui(ui, d, enter) {
                        keep = false;
                    }
                }
                Dialog::Crashed { file, what } => {
                    ui.set_width(460.0);
                    ui.label(RichText::new("only3d closed unexpectedly last time").family(theme::medium()).size(14.0));
                    ui.add(egui::Label::new(RichText::new(format!("It stopped with: {what}")).size(12.5)).wrap());
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("A report is saved at {}. It holds the version, your system and where the error happened, and none of your projects or files. Sending it helps get this fixed.", tilde(file)))
                                .size(11.5)
                                .color(p.muted),
                        )
                        .wrap(),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.add(theme::primary_button("Report it on GitHub")).clicked() {
                            crate::os::open(crash_issue_url(what));
                            crate::os::open_text(file);
                            keep = false;
                        }
                        if ui.button("Open the report").clicked() {
                            crate::os::open_text(file);
                        }
                        if ui.button("Close").clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::Confirm { title, body, tool, args } => {
                    ui.set_width(400.0);
                    ui.label(RichText::new(title.as_str()).family(theme::medium()).size(14.0));
                    ui.label(RichText::new(body.as_str()).size(12.5).color(p.muted));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new(RichText::new("Confirm").color(p.on_accent)).fill(p.error)).clicked() {
                            let kind = if tool.starts_with("preset_") { Kind::PresetChanged(args.get("kind").and_then(|k| k.as_str()).unwrap_or("").to_string()) } else { Kind::Report };
                            self.worker.call(tool, args.clone(), kind);
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::ToolForm(f) => {
                    ui.set_width(620.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&f.tool.name).monospace().size(14.0).color(p.accent));
                        ui.label(RichText::new(&f.tool.title).family(theme::medium()).size(14.0));
                        if f.tool.side_effects {
                            theme::chip(ui, "external effects", p.warning, theme::soft(p.warning));
                        } else if f.tool.mutates {
                            theme::chip(ui, "edits the project · undoable", p.muted, p.hover);
                        }
                    });
                    ui.label(RichText::new(&f.tool.description).size(11.5).color(p.muted));
                    ui.add_space(6.0);
                    if !f.tool.no_args() {
                        crate::tool_form::args_ui(ui, f);
                        ui.add_space(4.0);
                    }
                    let run_key = ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter));
                    ui.horizontal(|ui| {
                        let go = ui.add_enabled(!f.running, theme::primary_button(if f.running { "Running…" } else { "Run" })).on_hover_text(if MAC { "⌘⏎" } else { "Ctrl+Enter" }).clicked() || (run_key && !f.running);
                        if ui.button("Close").clicked() {
                            keep = false;
                        }
                        if f.running {
                            ui.add(egui::Spinner::new().size(12.0).color(p.accent));
                        }
                        if go {
                            match f.args() {
                                Ok(args) => {
                                    f.running = true;
                                    f.result = None;
                                    self.worker.call(&f.tool.name, args, Kind::ToolForm);
                                }
                                Err(e) => f.result = Some(Err(e)),
                            }
                        }
                    });
                    if let Some(r) = &f.result {
                        crate::tool_form::result_ui(ui, r);
                    }
                }
            });
        });
        if resp.should_close() {
            keep = false;
        }
        if keep && self.dialog.is_none() {
            self.dialog = Some(dialog);
        }
    }

    // ------------------------------------------------------------------------------------------
    // print dialog: never confirms on its own

    pub fn print_modal_ui(&mut self, ctx: &egui::Context) {
        let preview = self.print_preview;
        let Some(stage) = self.print_modal.as_mut() else { return };
        let p = pal();
        let mut close = false;
        let mut start: Option<Value> = None;
        let mut recheck: Option<Vec<i32>> = None;
        let mut timelapse: Option<bool> = None;
        let busy = matches!(stage, PrintStage::Sending);
        let resp = egui::Modal::new(egui::Id::unique("o3d_print_modal")).backdrop_color(Self::backdrop()).frame(Self::modal_frame()).show(ctx, |ui| {
            ui.set_width(540.0);
            egui::Frame::new().inner_margin(Margin::same(16)).show(ui, |ui| {
                ui.label(RichText::new("Send to printer").family(theme::medium()).size(15.0));
                ui.add_space(6.0);
                match stage {
                    PrintStage::Checking => {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().color(p.accent));
                            ui.label("Checking the slice and the printer…");
                        });
                        ui.add_space(8.0);
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    }
                    PrintStage::Ready(c) => {
                        let before = c.timelapse;
                        let (go, cancel, changed) = print_ready_ui(ui, c);
                        close = cancel;
                        timelapse = (c.timelapse != before).then_some(c.timelapse);
                        if changed && c.complete() && c.checked != c.picks {
                            recheck = c.trays_arg();
                        }
                        if go && c.ready() && !preview {
                            start = Some(c.start_args());
                        }
                    }
                    PrintStage::Failed(text) => {
                        ui.label(RichText::new("Not ready to print").strong().color(p.error));
                        ui.label(RichText::new(text.as_str()).size(12.5));
                        ui.add_space(8.0);
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    }
                    PrintStage::Sending => {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().color(p.accent));
                            ui.label("Uploading and starting the print…");
                        });
                    }
                    PrintStage::Sent(text) => {
                        ui.label(RichText::new(text.as_str()).size(12.5));
                        ui.add_space(8.0);
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    }
                    PrintStage::SendFailed(text) => {
                        ui.label(RichText::new("The print was not started").strong().color(p.error));
                        ui.label(RichText::new(text.as_str()).size(12.5));
                        ui.add_space(8.0);
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    }
                }
            });
        });
        if resp.should_close() && !busy {
            close = true;
        }
        if let Some(on) = timelapse {
            // the next print starts with the same choice
            self.cfg.record_timelapse = on;
        }
        if let Some(t) = recheck {
            // the checklist for the picked trays (no confirm: nothing is sent)
            self.worker.call("print", json!({ "trays": t }), Kind::PrintRecheck(t));
        }
        if let Some(args) = start {
            // the only place `confirm: true` is ever sent: an explicit click on "Start print"
            self.print_modal = Some(PrintStage::Sending);
            self.worker.call("print", args, Kind::PrintConfirm);
        } else if close {
            self.print_modal = None;
        }
    }
}

/// The title of a panel bar or section header, `inset` from the bar's left edge: cut short
/// before the buttons `right` laid out at the bar's other end, with the whole title on hover
/// when it was cut.
fn bar_title(ui: &mut Ui, rect: Rect, inset: f32, label: &str, right: &Ui) {
    let p = pal();
    let used = right.min_rect();
    let end = if used.width() > 0.0 { used.min.x - 8.0 } else { rect.max.x - 4.0 };
    let mut job = theme::label_job(label, p.muted, 10.5);
    job.wrap = egui::text::TextWrapping::truncate_at_width((end - rect.min.x - inset).max(24.0));
    let g = ui.painter().layout_job(job);
    let (cut, area) = (g.elided, Rect::from_min_size(egui::pos2(rect.min.x + inset, rect.min.y), egui::vec2(g.size().x, rect.height())));
    ui.painter().galley(egui::pos2(rect.min.x + inset, rect.center().y - g.size().y * 0.5), g, p.muted);
    if cut {
        // hover only: a click still reaches the header under it
        ui.interact(area, ui.scope_id().with(("o3d_bar_title", label)), Sense::hover()).on_hover_text(label);
    }
}

/// "A1 · Bambu PLA Wood Clay Brown · 735 g" for a tray of the preflight's `printer_trays`.
fn tray_text(t: Option<&Value>, n: i32) -> String {
    let Some(t) = t else { return format!("tray {n}") };
    let label = t["label"].as_str().unwrap_or("?");
    if t["loaded"] != json!(true) {
        return format!("{label} · empty");
    }
    let what = t["name"].as_str().or(t["material"].as_str()).unwrap_or("unknown filament");
    let left = t["grams_left"].as_f64().map(|g| format!(" · {}{g:.0} g", if t["grams_estimated"] == json!(true) { "~" } else { "" })).unwrap_or_default();
    format!("{label} · {what}{left}")
}

fn colour_of(v: &Value) -> Option<Color32> {
    v.as_str().map(|h| {
        let c = crate::scene::parse_hex(h);
        Color32::from_rgb(c[0], c[1], c[2])
    })
}

/// The print dialog once the preflight answered: a tray picker per filament (a tray that isn't
/// the project's filament starts unpicked), the checklist for the picked trays, and Start, which
/// stays off until every filament has a tray and that exact choice passed the preflight.
/// Returns (start clicked, cancel clicked, a pick changed).
fn print_ready_ui(ui: &mut Ui, c: &mut PrintChoice) -> (bool, bool, bool) {
    let p = pal();
    let (mut go, mut cancel, mut changed) = (false, false, false);
    if let Some(s) = c.data["summary"].as_str() {
        ui.label(RichText::new(s).size(12.5).color(p.muted));
    }
    let rows = c.rows();
    let trays = c.data["printer_trays"].as_array().cloned().unwrap_or_default();
    let tray = |n: i32| trays.iter().find(|t| t["tray"].as_i64() == Some(n as i64));
    if !rows.is_empty() {
        ui.add_space(10.0);
        ui.label(RichText::new("Filament").family(theme::medium()).size(13.0));
        let unpicked = c.picks.iter().filter(|x| x.is_none()).count();
        if unpicked > 0 {
            ui.add_space(4.0);
            egui::Frame::new().fill(theme::soft(p.warning)).corner_radius(4).inner_margin(8).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let which = if rows.len() == 1 { "the tray to print it from" } else { "a tray for each filament marked below" };
                ui.label(RichText::new(format!("The loaded filament isn't the project's. Pick {which}: the printer uses exactly what you pick.")).size(12.0).color(p.text));
            });
        }
        ui.add_space(4.0);
        for (i, r) in rows.iter().enumerate() {
            let choices = r["choices"].as_array().cloned().unwrap_or_default();
            let pick = c.picks.get(i).copied().flatten();
            ui.horizontal(|ui| {
                panels::swatch(ui, colour_of(&r["color"]).unwrap_or(p.muted), 12.0);
                let name = r["filament"].as_str().unwrap_or("?");
                ui.add_sized([170.0, 18.0], egui::Label::new(RichText::new(format!("{} · {name}", r["slot"])).size(12.5)).truncate()).on_hover_text(format!("Project filament {}: {name} {}", r["slot"], r["color"].as_str().unwrap_or("")));
                ui.label(RichText::new("→").color(p.muted));
                if choices.is_empty() {
                    ui.label(RichText::new(tray_text(pick.and_then(&tray), pick.unwrap_or(0))).size(12.5));
                    return;
                }
                let shown = match pick {
                    Some(n) => RichText::new(tray_text(tray(n), n)).size(12.5),
                    None => RichText::new("Choose a tray…").size(12.5).color(p.warning),
                };
                egui::ComboBox::from_id_salt(("o3d_print_tray", i)).selected_text(shown).width(ui.available_width() - 4.0).show_ui(ui, |ui| {
                    for ch in &choices {
                        let n = ch["tray"].as_i64().unwrap_or(-1) as i32;
                        let t = tray(n);
                        let r = ui
                            .horizontal(|ui| {
                                panels::swatch(ui, t.and_then(|t| colour_of(&t["color"])).unwrap_or(p.muted), 10.0);
                                let differs = if ch["difference"].is_string() { " · not the project's" } else { "" };
                                ui.selectable_label(pick == Some(n), RichText::new(format!("{}{differs}", tray_text(t, n))).size(12.5))
                            })
                            .inner;
                        let r = match ch["difference"].as_str() {
                            Some(d) => r.on_hover_text(d),
                            None => r,
                        };
                        if r.clicked() && pick != Some(n) {
                            c.picks[i] = Some(n);
                            changed = true;
                        }
                    }
                });
            });
            // what differs from the project, under the row
            let note = match pick {
                None => r["difference"].as_str().map(|d| format!("Nearest is {}: {d}", r["label"].as_str().unwrap_or("?"))),
                Some(n) => choices.iter().find(|ch| ch["tray"].as_i64() == Some(n as i64)).and_then(|ch| ch["difference"].as_str()).map(|d| format!("Not the project's filament: {d}")),
            };
            if let Some(note) = note {
                ui.horizontal(|ui| {
                    ui.add_space(20.0);
                    ui.add(egui::Label::new(RichText::new(note).size(11.5).color(p.warning)).wrap());
                });
            }
        }
    }
    ui.add_space(10.0);
    if c.checking() {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().color(p.accent));
            ui.label(RichText::new("Checking with these trays…").size(12.5));
        });
    } else if let Some(f) = &c.failed {
        ui.label(RichText::new("Not ready with these trays").strong().color(p.error));
        ui.label(RichText::new(f.as_str()).size(12.0));
    } else {
        for item in c.data["checklist"].as_array().into_iter().flatten() {
            let ok = item["ok"] == json!(true);
            ui.horizontal_top(|ui| {
                ui.label(RichText::new(if ok { "✓" } else { "×" }).size(12.0).color(if ok { p.success } else { p.error }));
                ui.add(egui::Label::new(RichText::new(item["item"].as_str().unwrap_or("")).size(12.0)).wrap());
            });
        }
        if !c.complete() {
            ui.label(muted("The filament checks follow once every filament has a tray."));
        }
    }
    for note in [c.data["untested"].as_str().map(String::from), c.data["notices"].as_array().filter(|n| !n.is_empty()).map(|n| format!("Printer notices: {}", n.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; ")))].into_iter().flatten() {
        ui.label(RichText::new(note).size(12.0).color(p.muted));
    }
    if let Some(note) = c.multi_unverified().map(str::to_string) {
        ui.add_space(8.0);
        egui::Frame::new().fill(theme::soft(p.error)).corner_radius(4).inner_margin(8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(note).size(12.0).color(p.text));
            ui.add_space(4.0);
            theme::checkbox(ui, &mut c.multi_ok, RichText::new("Start it anyway: I'll watch the filament changes").size(12.5));
        });
    }
    if c.can_timelapse() {
        ui.add_space(8.0);
        theme::checkbox(ui, &mut c.timelapse, RichText::new("Record a timelapse").size(12.5)).on_hover_text("The printer takes a picture each layer and makes a video of them, kept on its USB drive or card.\nFind it under Timelapses in the Printer panel.");
    }
    ui.add_space(6.0);
    egui::Frame::new().fill(theme::soft(p.warning)).corner_radius(4).inner_margin(8).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("Starting uploads the file and begins printing immediately. Check that the plate is clear and is the right type.").size(12.0).color(p.text));
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let ok = c.ready();
        let b = ui.add_enabled(ok, theme::primary_button("Start print"));
        if b.clicked() {
            go = true;
        }
        if !ok {
            b.on_disabled_hover_text(if !c.complete() {
                "Pick a tray for each filament first"
            } else if c.checking() {
                "Checking these trays…"
            } else if c.multi_unverified().is_some() && !c.multi_ok {
                "Tick that you want this multi-filament print first"
            } else {
                "Not ready: see the checklist"
            });
        }
        if ui.button("Cancel").clicked() {
            cancel = true;
        }
    });
    (go, cancel, changed)
}

/// A new GitHub issue for a crash, titled with its cause; the body asks for the report, which
/// only3d opens beside it (it can be too long for a link).
fn crash_issue_url(what: &str) -> String {
    let enc = |t: &str| t.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>();
    let title: String = format!("Crash: {what}").chars().take(100).collect();
    let body = format!("only3d {} on {} {}.\n\nPaste the crash report here (only3d opened it for you), and say what you were doing.\n", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH);
    format!("https://github.com/onatyap/only3d/issues/new?title={}&body={}", enc(&title), enc(&body))
}

pub fn tilde(p: &std::path::Path) -> String {
    let s = p.display().to_string();
    match std::env::home_dir().map(|h| h.display().to_string()) {
        Some(h) if !h.is_empty() && s.starts_with(&h) => format!("~{}", &s[h.len()..]),
        _ => s,
    }
}

pub fn job_label(tool: &str) -> String {
    match tool {
        "slice" => "Slicing…".into(),
        "print" => "Talking to the printer…".into(),
        "printer_status" => "Reading printer status…".into(),
        "export" => "Exporting…".into(),
        "import_file" => "Importing…".into(),
        "orient" => "Orienting…".into(),
        "arrange" => "Arranging…".into(),
        "settings_catalog" | "effective settings" => "Reading settings…".into(),
        t => format!("{t}…"),
    }
}
