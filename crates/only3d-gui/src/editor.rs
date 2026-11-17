//! The main area's tabs, like a code editor's.
//!
//! - The 3D plate is one tab that can't be closed; files open as tabs beside it.
//! - A single click in the Files panel opens a **preview tab** (its name in italics) that the
//!   next single click replaces; a double-click, dragging the tab or "Keep Open" keeps it.
//! - Tabs live in one group, or two side by side (⌘\ splits and joins). Drag a tab to reorder
//!   it, onto the other group, or to the right edge of the area to split.
//! - × or a middle click closes a tab (⌘W), ⇧⌘T reopens the last closed one, ⌃Tab cycles.
//! - Files dropped from Finder or the Files panel open as tabs where they land; models dropped
//!   on the plate import, as before.
//!
//! [`Editor`] is the plain model (tested below); `App::center` draws it.

use std::path::{Path, PathBuf};

use eframe::egui::{self, Rect, Sense};
use only3d_studio::workspace::FileKind;

use crate::app::App;
use crate::theme::{self, pal, Icon};

#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub id: u64,
    /// `None`: the 3D plate.
    pub file: Option<(PathBuf, FileKind)>,
    /// A kept tab stays; a preview tab (false) is replaced by the next preview.
    pub kept: bool,
}

impl Tab {
    pub fn is_plate(&self) -> bool {
        self.file.is_none()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub tabs: Vec<Tab>,
    pub active: usize,
}

#[derive(Debug)]
pub struct Editor {
    /// One or two groups, left to right.
    pub groups: Vec<Group>,
    /// The group that commands (⌘W, ⌃Tab, opening a file) act on.
    pub focus: usize,
    /// Side by side: the left group's share of the width.
    pub frac: f32,
    closed: Vec<(PathBuf, FileKind)>,
    next: u64,
}

impl Default for Editor {
    fn default() -> Self {
        Editor { groups: vec![Group { tabs: vec![Tab { id: 0, file: None, kept: true }], active: 0 }], focus: 0, frac: 0.5, closed: Vec::new(), next: 1 }
    }
}

/// What opening a file did.
#[derive(Debug, PartialEq)]
pub enum Opened {
    /// It was open already.
    Activated(u64),
    New(u64),
    /// It took the preview tab's place.
    Replaced { old: u64, new: u64 },
}

impl Editor {
    pub fn find(&self, id: u64) -> Option<(usize, usize)> {
        self.groups.iter().enumerate().find_map(|(g, gr)| gr.tabs.iter().position(|t| t.id == id).map(|t| (g, t)))
    }

    pub fn find_path(&self, path: &Path) -> Option<(usize, usize)> {
        self.groups.iter().enumerate().find_map(|(g, gr)| gr.tabs.iter().position(|t| t.file.as_ref().is_some_and(|(p, _)| p == path)).map(|t| (g, t)))
    }

    pub fn active(&self, g: usize) -> &Tab {
        let gr = &self.groups[g];
        &gr.tabs[gr.active.min(gr.tabs.len() - 1)]
    }

    pub fn plate_pos(&self) -> (usize, usize) {
        self.groups.iter().enumerate().find_map(|(g, gr)| gr.tabs.iter().position(Tab::is_plate).map(|t| (g, t))).unwrap_or((0, 0))
    }

    /// The 3D plate is on screen.
    pub fn plate_visible(&self) -> bool {
        let (g, t) = self.plate_pos();
        self.groups[g].active == t
    }

    /// Nothing but the plate: no tab strip at all.
    pub fn only_plate(&self) -> bool {
        self.groups.len() == 1 && self.groups[0].tabs.len() == 1
    }

    /// Files showing in a group right now.
    pub fn active_files(&self) -> Vec<PathBuf> {
        (0..self.groups.len()).filter_map(|g| self.active(g).file.as_ref().map(|(p, _)| p.clone())).collect()
    }

    pub fn can_reopen(&self) -> bool {
        !self.closed.is_empty()
    }

    pub fn activate(&mut self, id: u64) {
        if let Some((g, t)) = self.find(id) {
            self.groups[g].active = t;
            self.focus = g;
        }
    }

    pub fn keep(&mut self, id: u64) {
        if let Some((g, t)) = self.find(id) {
            self.groups[g].tabs[t].kept = true;
        }
    }

    pub fn show_plate(&mut self) {
        let (g, t) = self.plate_pos();
        self.groups[g].active = t;
        self.focus = g;
    }

    fn new_id(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    /// Open a file in `group` (default: the focused one), as a preview or kept.
    pub fn open(&mut self, path: &Path, kind: FileKind, kept: bool, group: Option<usize>) -> Opened {
        if let Some((g, t)) = self.find_path(path) {
            self.groups[g].active = t;
            self.focus = g;
            if kept {
                self.groups[g].tabs[t].kept = true;
            }
            return Opened::Activated(self.groups[g].tabs[t].id);
        }
        let g = group.unwrap_or(self.focus).min(self.groups.len() - 1);
        let id = self.new_id();
        let tab = Tab { id, file: Some((path.to_path_buf(), kind)), kept };
        self.focus = g;
        let gr = &mut self.groups[g];
        if !kept {
            if let Some(t) = gr.tabs.iter().position(|t| !t.kept && !t.is_plate()) {
                let old = std::mem::replace(&mut gr.tabs[t], tab).id;
                gr.active = t;
                return Opened::Replaced { old, new: id };
            }
        }
        let at = (gr.active + 1).min(gr.tabs.len());
        gr.tabs.insert(at, tab);
        gr.active = at;
        Opened::New(id)
    }

    /// Close tabs (the plate stays); returns the ids that went.
    pub fn close_many(&mut self, ids: &[u64]) -> Vec<u64> {
        let mut gone = Vec::new();
        for &id in ids {
            let Some((g, t)) = self.find(id) else { continue };
            let gr = &mut self.groups[g];
            if gr.tabs[t].is_plate() {
                continue;
            }
            let tab = gr.tabs.remove(t);
            if gr.active > t || gr.active >= gr.tabs.len() {
                gr.active = gr.active.saturating_sub(1);
            }
            if let Some(f) = tab.file {
                self.closed.retain(|c| c.0 != f.0);
                self.closed.push(f);
            }
            gone.push(id);
            self.tidy();
        }
        gone
    }

    /// The tabs of `id`'s group other than it (and not the plate).
    pub fn others(&self, id: u64) -> Vec<u64> {
        let Some((g, _)) = self.find(id) else { return Vec::new() };
        self.groups[g].tabs.iter().filter(|t| t.id != id && !t.is_plate()).map(|t| t.id).collect()
    }

    pub fn right_of(&self, id: u64) -> Vec<u64> {
        let Some((g, t)) = self.find(id) else { return Vec::new() };
        self.groups[g].tabs[t + 1..].iter().filter(|t| !t.is_plate()).map(|t| t.id).collect()
    }

    pub fn files_in(&self, g: usize) -> Vec<u64> {
        self.groups.get(g).map(|gr| gr.tabs.iter().filter(|t| !t.is_plate()).map(|t| t.id).collect()).unwrap_or_default()
    }

    /// The last closed file, to open again.
    pub fn reopen(&mut self) -> Option<(PathBuf, FileKind)> {
        self.closed.pop()
    }

    /// Move a tab to `index` in group `to` (`to` past the last group makes a new one on the
    /// right, at most two). A dragged tab is kept.
    pub fn move_tab(&mut self, id: u64, to: usize, index: usize) {
        let Some((g, t)) = self.find(id) else { return };
        let to = if to >= self.groups.len() {
            if self.groups.len() >= 2 || (self.groups[0].tabs.len() == 1) {
                return;
            }
            self.groups.push(Group { tabs: Vec::new(), active: 0 });
            1
        } else {
            to
        };
        let mut tab = self.groups[g].tabs.remove(t);
        tab.kept = true;
        {
            let gr = &mut self.groups[g];
            if gr.active > t || gr.active >= gr.tabs.len() {
                gr.active = gr.active.saturating_sub(1);
            }
        }
        let mut idx = index;
        if g == to && t < index {
            idx -= 1;
        }
        let gr = &mut self.groups[to];
        let idx = idx.min(gr.tabs.len());
        gr.tabs.insert(idx, tab);
        gr.active = idx;
        self.focus = to;
        self.tidy();
    }

    /// ⌘\: two groups join into one; one group splits, the file showing (else the last file)
    /// moving to a new group on the right.
    pub fn toggle_split(&mut self) {
        if self.groups.len() == 2 {
            let keep = self.active(self.focus).id;
            let right = self.groups.remove(1);
            self.groups[0].tabs.extend(right.tabs);
            self.focus = 0;
            self.activate(keep);
            return;
        }
        let pick = Some(self.active(0)).filter(|t| !t.is_plate()).or_else(|| self.groups[0].tabs.iter().rev().find(|t| !t.is_plate())).map(|t| t.id);
        if let Some(id) = pick {
            self.move_tab(id, 1, 0);
        }
    }

    /// ⌃Tab / ⌃⇧Tab in the focused group.
    pub fn cycle(&mut self, step: i32) {
        let g = self.focus.min(self.groups.len() - 1);
        let gr = &mut self.groups[g];
        let n = gr.tabs.len() as i32;
        gr.active = (gr.active as i32 + step).rem_euclid(n) as usize;
    }

    /// No empty groups (one always stays), actives and focus in range.
    fn tidy(&mut self) {
        if self.groups.len() > 1 {
            self.groups.retain(|g| !g.tabs.is_empty());
        }
        for g in &mut self.groups {
            g.active = g.active.min(g.tabs.len().saturating_sub(1));
        }
        self.focus = self.focus.min(self.groups.len() - 1);
    }
}

/// What a tab or a file dropped on the main area lands on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Spot {
    /// The 3D plate showing in a group (models import there).
    Plate,
    /// A group's tab strip or file pane: index to insert at.
    Group(usize, usize),
    /// The right edge of a single group: a new group.
    NewGroup,
}

/// Payload of a dragged tab.
#[derive(Clone, Copy, Debug)]
pub struct DragTab(pub u64);

/// The screen geometry of the tabs this frame, for drops.
#[derive(Default)]
pub struct Regions {
    /// (group, strip, body, the plate is showing in it)
    pub groups: Vec<(usize, Rect, Rect, bool)>,
    /// (group, index, rect) of each tab
    pub tabs: Vec<(usize, usize, Rect)>,
}

impl Regions {
    pub fn spot(&self, p: egui::Pos2) -> Option<Spot> {
        if let Some((g, i, r)) = self.tabs.iter().find(|(_, _, r)| r.contains(p)) {
            return Some(Spot::Group(*g, if p.x > r.center().x { i + 1 } else { *i }));
        }
        let single = self.groups.len() == 1;
        for (g, strip, body, plate) in &self.groups {
            let n = self.tabs.iter().filter(|t| t.0 == *g).count();
            if strip.contains(p) {
                return Some(Spot::Group(*g, n));
            }
            if body.contains(p) {
                if single && p.x > body.max.x - body.width() * 0.25 {
                    return Some(Spot::NewGroup);
                }
                return Some(if *plate { Spot::Plate } else { Spot::Group(*g, n) });
            }
        }
        None
    }
}

/// Where the pointer is, in window points, when a file from Finder is dropped: winit 0.30 drops
/// without a position on macOS, so ask AppKit for the mouse.
#[cfg(target_os = "macos")]
pub fn drop_point(ctx: &egui::Context) -> Option<egui::Pos2> {
    let mtm = objc2::MainThreadMarker::new()?;
    let m = objc2_app_kit::NSEvent::mouseLocation();
    let screens = objc2_app_kit::NSScreen::screens(mtm);
    let h = screens.firstObject()?.frame().size.height;
    let inner = ctx.input(|i| i.viewport().inner_rect)?;
    // desktop points (y up from the main screen's bottom) → egui points (zoomed, y down)
    let z = ctx.zoom_factor().max(0.01);
    Some(egui::pos2(m.x as f32 / z - inner.min.x, (h - m.y) as f32 / z - inner.min.y))
}

#[cfg(not(target_os = "macos"))]
pub fn drop_point(ctx: &egui::Context) -> Option<egui::Pos2> {
    ctx.input(|i| i.pointer.latest_pos())
}

/// What a tab's menu or its drag asked for, applied after drawing.
enum TabCmd {
    Activate(u64),
    Keep(u64),
    Close(Vec<u64>),
    Move(u64, usize, usize),
    OpenFiles(Vec<PathBuf>, Spot),
    Split,
    Reveal(PathBuf),
    OpenInApp(PathBuf),
    CopyPath(PathBuf),
    Import(PathBuf),
    OpenProject(PathBuf),
}

fn tab_icon(kind: Option<FileKind>) -> Icon {
    match kind {
        None | Some(FileKind::Model) | Some(FileKind::Project) => Icon::Cube,
        Some(FileKind::Print) => Icon::Printer,
        Some(FileKind::Image | FileKind::Vector | FileKind::Video) => Icon::FileImage,
        Some(FileKind::Data | FileKind::Code) => Icon::FileCode,
        _ => Icon::FileText,
    }
}

impl App {
    /// Open a file in a tab (`kept`: not a preview) and start loading it.
    pub(crate) fn open_file_tab(&mut self, ctx: &egui::Context, path: &Path, kind: FileKind, kept: bool, group: Option<usize>) {
        match self.editor.open(path, kind, kept, group) {
            Opened::Activated(_) => {}
            Opened::New(id) => {
                self.files_open.insert(id, crate::preview::FilePreview::open(ctx, path, kind));
            }
            Opened::Replaced { old, new } => {
                self.files_open.remove(&old);
                self.files_open.insert(new, crate::preview::FilePreview::open(ctx, path, kind));
            }
        }
    }

    pub(crate) fn close_tabs(&mut self, ids: &[u64]) {
        for id in self.editor.close_many(ids) {
            self.files_open.remove(&id);
        }
    }

    /// ⌘W: the focused group's tab (not the plate).
    pub(crate) fn close_active_tab(&mut self) {
        let id = self.editor.active(self.editor.focus).id;
        self.close_tabs(&[id]);
    }

    pub(crate) fn reopen_tab(&mut self) {
        if let Some((p, k)) = self.editor.reopen() {
            if p.exists() {
                let ctx = self.egui.clone();
                self.open_file_tab(&ctx, &p, k, true, None);
            }
        }
    }

    /// The main area: the plate alone, or tab groups (one or two side by side).
    pub(crate) fn center(&mut self, ui: &mut egui::Ui) {
        let full = ui.max_rect();
        self.regions = Regions::default();
        if self.editor.only_plate() {
            self.regions.groups.push((0, Rect::NOTHING, full, true));
            self.viewport(ui);
            self.drop_on_editor(ui);
            return;
        }
        let p = pal();
        let two = self.editor.groups.len() == 2;
        let x = full.min.x + full.width() * self.editor.frac.clamp(0.2, 0.8);
        let div = Rect::from_min_max(egui::pos2(x - 3.0, full.min.y), egui::pos2(x + 3.0, full.max.y));
        let rects: Vec<Rect> = if two { vec![Rect::from_min_max(full.min, egui::pos2(div.min.x, full.max.y)), Rect::from_min_max(egui::pos2(div.max.x, full.min.y), full.max)] } else { vec![full] };
        let mut cmds = Vec::new();
        for (g, r) in rects.iter().enumerate() {
            let strip = Rect::from_min_size(r.min, egui::vec2(r.width(), 30.0));
            let body = Rect::from_min_max(egui::pos2(r.min.x, strip.max.y), r.max);
            self.tab_strip(ui, g, strip, g + 1 == rects.len(), &mut cmds);
            let active = self.editor.active(g).clone();
            self.regions.groups.push((g, strip, body, active.is_plate()));
            if active.is_plate() {
                ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| self.viewport(ui));
            } else {
                ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| self.preview_body(ui, active.id));
            }
            // a click anywhere in a group gives it the focus
            if ui.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|q| r.contains(q))) {
                self.editor.focus = g;
            }
            if two && g == self.editor.focus {
                ui.painter().line_segment([strip.left_bottom() + egui::vec2(0.0, -1.0), strip.right_bottom() + egui::vec2(0.0, -1.0)], egui::Stroke::new(1.0, p.accent.gamma_multiply(0.5)));
            }
        }
        if two {
            let d = ui.interact(div, egui::Id::unique("o3d-editor-divider"), Sense::drag()).on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
            if d.dragged() {
                self.editor.frac = ((x + d.drag_delta().x - full.min.x) / full.width()).clamp(0.2, 0.8);
            }
            if d.double_clicked() {
                self.editor.frac = 0.5;
            }
            let line = if d.hovered() || d.dragged() { p.accent } else { p.border_variant };
            ui.painter().rect_filled(div, 0.0, p.bar);
            ui.painter().line_segment([egui::pos2(x, full.min.y), egui::pos2(x, full.max.y)], egui::Stroke::new(1.0, line));
        }
        self.drop_on_editor(ui);
        self.apply_tab_cmds(ui.ctx(), cmds);
    }

    /// Tabs and files dragged inside the window: where they would land, and the drop.
    fn drop_on_editor(&mut self, ui: &mut egui::Ui) {
        let p = pal();
        let ctx = ui.ctx().clone();
        let tab = egui::DragAndDrop::payload::<DragTab>(&ctx).map(|t| t.0);
        let files = egui::DragAndDrop::payload::<crate::app::DragFiles>(&ctx).map(|f| f.0.clone());
        if tab.is_none() && files.is_none() {
            return;
        }
        let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else { return };
        let Some(spot) = self.regions.spot(pos) else { return };
        // models over the plate: the viewport imports them, as before
        let models_only = egui::DragAndDrop::payload::<crate::app::DragFiles>(&ctx).is_some_and(|f| f.all_models());
        if tab.is_none() && spot == Spot::Plate && models_only {
            return;
        }
        // on the main area itself, over the 3D view and the file panes
        let painter = ui.painter().clone();
        match spot {
            Spot::NewGroup => {
                if let Some((_, _, body, _)) = self.regions.groups.first() {
                    let zone = Rect::from_min_max(egui::pos2(body.center().x, body.min.y), body.max).shrink(10.0);
                    painter.rect_filled(zone, 8.0, p.accent.gamma_multiply(0.12));
                    painter.rect_stroke(zone, 8.0, egui::Stroke::new(1.5, p.accent), egui::StrokeKind::Inside);
                    let what = if tab.is_some() { "Drop to move it beside the plate" } else { "Drop to open beside the plate" };
                    painter.text(zone.center(), egui::Align2::CENTER_CENTER, what, egui::FontId::proportional(15.0), p.accent);
                }
            }
            Spot::Group(g, i) => {
                let at = self.regions.tabs.iter().find(|t| t.0 == g && t.1 == i).map(|t| t.2.min.x).or_else(|| self.regions.tabs.iter().filter(|t| t.0 == g).map(|t| t.2.max.x).fold(None, |a: Option<f32>, x| Some(a.map_or(x, |a| a.max(x)))));
                if let Some((_, strip, _, _)) = self.regions.groups.iter().find(|r| r.0 == g) {
                    let x = at.unwrap_or(strip.min.x + 6.0);
                    painter.line_segment([egui::pos2(x, strip.min.y + 4.0), egui::pos2(x, strip.max.y - 4.0)], egui::Stroke::new(2.0, p.accent));
                }
            }
            Spot::Plate => {
                if let Some((_, _, body, _)) = self.regions.groups.iter().find(|r| r.3) {
                    painter.rect_filled(body.shrink(10.0), 8.0, p.accent.gamma_multiply(0.08));
                    painter.rect_stroke(body.shrink(10.0), 8.0, egui::Stroke::new(1.5, p.accent), egui::StrokeKind::Inside);
                }
                let what = if tab.is_some() { "Drop to move the tab here" } else { "Drop to open as a tab" };
                let g = painter.layout_no_wrap(what.into(), egui::FontId::proportional(13.0), p.accent);
                let r = Rect::from_min_size(pos + egui::vec2(14.0, 10.0), g.size() + egui::vec2(12.0, 8.0));
                painter.rect(r, 4.0, p.elevated, egui::Stroke::new(1.0, p.accent), egui::StrokeKind::Inside);
                painter.galley(r.min + egui::vec2(6.0, 4.0), g, p.accent);
            }
        }
        if ctx.input(|i| i.pointer.any_released()) {
            if let Some(id) = tab {
                egui::DragAndDrop::clear_payload(&ctx);
                match spot {
                    Spot::Group(g, i) => self.editor.move_tab(id, g, i),
                    Spot::NewGroup => self.editor.move_tab(id, 1, 0),
                    Spot::Plate => {
                        let (g, _) = self.editor.plate_pos();
                        let n = self.editor.groups[g].tabs.len();
                        self.editor.move_tab(id, g, n);
                    }
                }
            } else if let Some(paths) = files {
                egui::DragAndDrop::clear_payload(&ctx);
                let model = |p: &PathBuf| matches!(only3d_studio::workspace::kind_of(&p.to_string_lossy()), FileKind::Model);
                if spot == Spot::Plate {
                    // a mixed drag on the plate: its models import, the rest open as tabs
                    let (models, others): (Vec<PathBuf>, Vec<PathBuf>) = paths.into_iter().partition(model);
                    if !models.is_empty() {
                        self.import_paths(models);
                    }
                    let (g, _) = self.editor.plate_pos();
                    self.apply_tab_cmds(&ctx, vec![TabCmd::OpenFiles(others, Spot::Group(g, usize::MAX))]);
                } else {
                    self.apply_tab_cmds(&ctx, vec![TabCmd::OpenFiles(paths, spot)]);
                }
            }
        }
    }

    /// One group's tab strip; the last strip ends with the side-by-side switch.
    fn tab_strip(&mut self, ui: &mut egui::Ui, g: usize, strip: Rect, last: bool, cmds: &mut Vec<TabCmd>) {
        let p = pal();
        ui.painter().rect_filled(strip, 0.0, p.bar);
        ui.painter().line_segment([strip.left_bottom(), strip.right_bottom()], egui::Stroke::new(1.0, p.border_variant));
        let two = self.editor.groups.len() == 2;
        let chord = self.keymap.label_for(crate::keymap::Action::SideBySide).map(|k| format!(" ({k})")).unwrap_or_default();
        let project = self.project_name();
        let tabs = self.editor.groups[g].tabs.clone();
        let active = self.editor.groups[g].active;
        let switch_w = if last { 30.0 } else { 0.0 };
        let tabs_rect = Rect::from_min_max(strip.min + egui::vec2(4.0, 3.0), egui::pos2(strip.max.x - switch_w, strip.max.y - 3.0));
        let mut rects = Vec::new();
        ui.scope_builder(egui::UiBuilder::new().max_rect(tabs_rect).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
            egui::ScrollArea::horizontal().id_salt(("o3d-tabs", g)).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for (i, t) in tabs.iter().enumerate() {
                        let on = i == active;
                        let (name, kind) = match &t.file {
                            Some((path, k)) => (path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), Some(*k)),
                            None => (project.clone(), None),
                        };
                        let r = draw_tab(ui, tab_icon(kind), &name, on, !t.kept, !t.is_plate());
                        rects.push((g, i, r.rect));
                        let tip = match &t.file {
                            Some((path, _)) => format!("{}{}", crate::chrome::tilde(path), if t.kept { "" } else { "\nPreview: the next file you click replaces it (double-click to keep it)" }),
                            None => "The project's 3D plate".into(),
                        };
                        let resp = r.resp.on_hover_text(tip);
                        if r.close || resp.middle_clicked() {
                            cmds.push(TabCmd::Close(vec![t.id]));
                        } else if resp.double_clicked() {
                            cmds.push(TabCmd::Keep(t.id));
                        } else if resp.clicked() {
                            cmds.push(TabCmd::Activate(t.id));
                        }
                        if resp.drag_started() {
                            resp.dnd_set_drag_payload(DragTab(t.id));
                        }
                        let others = self.editor.others(t.id);
                        let right = self.editor.right_of(t.id);
                        let all = self.editor.files_in(g);
                        resp.context_menu(|ui| {
                            let file = t.file.clone();
                            if file.is_some() && ui.add(egui::Button::new("Close").shortcut_text(self.keymap.label_for(crate::keymap::Action::CloseTab).unwrap_or_default())).clicked() {
                                cmds.push(TabCmd::Close(vec![t.id]));
                                ui.close();
                            }
                            if ui.add_enabled(!others.is_empty(), egui::Button::new("Close Others")).clicked() {
                                cmds.push(TabCmd::Close(others.clone()));
                                ui.close();
                            }
                            if ui.add_enabled(!right.is_empty(), egui::Button::new("Close to the Right")).clicked() {
                                cmds.push(TabCmd::Close(right.clone()));
                                ui.close();
                            }
                            if ui.add_enabled(!all.is_empty(), egui::Button::new("Close All Files")).clicked() {
                                cmds.push(TabCmd::Close(all.clone()));
                                ui.close();
                            }
                            ui.separator();
                            if !t.kept && ui.button("Keep Open").clicked() {
                                cmds.push(TabCmd::Keep(t.id));
                                ui.close();
                            }
                            let move_label = if two { "Move to the Other Side" } else { "Move to the Right Side" };
                            if (two || !self.editor.only_plate()) && ui.button(move_label).clicked() {
                                cmds.push(TabCmd::Move(t.id, if g == 0 { 1 } else { 0 }, 0));
                                ui.close();
                            }
                            if let Some((path, kind)) = file {
                                ui.separator();
                                let main = match kind {
                                    FileKind::Project => Some(("Open Project", TabCmd::OpenProject(path.clone()))),
                                    FileKind::Model => Some(("Import onto the Plate", TabCmd::Import(path.clone()))),
                                    _ => None,
                                };
                                if let Some((label, cmd)) = main {
                                    if ui.button(label).clicked() {
                                        cmds.push(cmd);
                                        ui.close();
                                    }
                                }
                                if ui.button("Open in Its App").clicked() {
                                    cmds.push(TabCmd::OpenInApp(path.clone()));
                                    ui.close();
                                }
                                if ui.button(crate::os::show_in()).clicked() {
                                    cmds.push(TabCmd::Reveal(path.clone()));
                                    ui.close();
                                }
                                if ui.button("Copy Path").clicked() {
                                    cmds.push(TabCmd::CopyPath(path));
                                    ui.close();
                                }
                            }
                        });
                    }
                });
            });
        });
        self.regions.tabs.extend(rects);
        if last {
            let sw = Rect::from_center_size(egui::pos2(strip.max.x - 16.0, strip.center().y), egui::vec2(22.0, 22.0));
            ui.scope_builder(egui::UiBuilder::new().max_rect(sw), |ui| {
                let tip = if two { format!("Join into one group{chord}") } else { format!("Side by side: open the file in a group on the right{chord}") };
                if theme::icon_toggle(ui, Icon::DockRight, &tip, two).clicked() {
                    cmds.push(TabCmd::Split);
                }
            });
        }
    }

    fn apply_tab_cmds(&mut self, ctx: &egui::Context, cmds: Vec<TabCmd>) {
        for c in cmds {
            match c {
                TabCmd::Activate(id) => self.editor.activate(id),
                TabCmd::Keep(id) => {
                    self.editor.keep(id);
                    self.editor.activate(id);
                }
                TabCmd::Close(ids) => self.close_tabs(&ids),
                TabCmd::Move(id, g, i) => self.editor.move_tab(id, g, i),
                TabCmd::Split => {
                    self.editor.toggle_split();
                    self.cam_ready = false;
                }
                TabCmd::OpenFiles(paths, spot) => {
                    let group = match spot {
                        Spot::Group(g, _) => Some(g),
                        _ => None,
                    };
                    let mut opened = Vec::new();
                    for p in paths {
                        let kind = only3d_studio::workspace::kind_of(&p.to_string_lossy());
                        if kind == FileKind::Folder || p.is_dir() {
                            continue;
                        }
                        self.open_file_tab(ctx, &p, kind, true, group);
                        if let Some((g, t)) = self.editor.find_path(&p) {
                            opened.push(self.editor.groups[g].tabs[t].id);
                        }
                    }
                    if spot == Spot::NewGroup {
                        for id in opened {
                            self.editor.move_tab(id, 1, usize::MAX);
                        }
                        self.cam_ready = false;
                    }
                }
                TabCmd::Reveal(p) => {
                    crate::os::reveal(std::path::Path::new(&p));
                }
                TabCmd::OpenInApp(p) => {
                    crate::os::open(&p);
                }
                TabCmd::CopyPath(p) => ctx.copy_text(p.display().to_string()),
                TabCmd::Import(p) => {
                    self.worker.call("import_file", serde_json::json!({ "path": p.display().to_string() }), crate::worker::Kind::AddObject);
                    self.editor.show_plate();
                }
                TabCmd::OpenProject(p) => {
                    self.worker.call("open_project", serde_json::json!({ "path": p.display().to_string() }), crate::worker::Kind::Plain);
                    self.cam_ready = false;
                    self.editor.show_plate();
                }
            }
        }
    }

    /// Files dropped from Finder: tabs where they land; models on the plate (or outside the main
    /// area) import as before.
    pub(crate) fn drop_files(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>) {
        let spot = drop_point(ctx).and_then(|p| self.regions.spot(p));
        let model = |p: &PathBuf| matches!(only3d_studio::workspace::kind_of(&p.to_string_lossy()), FileKind::Model | FileKind::Project);
        match spot {
            Some(s @ (Spot::Group(..) | Spot::NewGroup)) => self.apply_tab_cmds(ctx, vec![TabCmd::OpenFiles(paths, s)]),
            _ => {
                let (models, others): (Vec<PathBuf>, Vec<PathBuf>) = paths.into_iter().partition(model);
                if !models.is_empty() {
                    self.import_paths(models);
                }
                if !others.is_empty() {
                    self.apply_tab_cmds(ctx, vec![TabCmd::OpenFiles(others, Spot::Group(self.editor.focus, usize::MAX))]);
                }
            }
        }
    }
}

struct DrawnTab {
    resp: egui::Response,
    rect: Rect,
    close: bool,
}

/// One tab: icon, name (italic while a preview), and × on the active or hovered tab.
fn draw_tab(ui: &mut egui::Ui, icon: Icon, label: &str, on: bool, preview: bool, closable: bool) -> DrawnTab {
    let p = pal();
    let font = egui::FontId::proportional(12.5);
    let text = |s: &str| {
        let mut job = egui::text::LayoutJob::single_section(s.to_string(), egui::TextFormat { font_id: font.clone(), color: if on { p.text } else { p.muted }, italics: preview, ..Default::default() });
        job.wrap = egui::text::TextWrapping::truncate_at_width(220.0);
        job
    };
    let g = ui.painter().layout_job(text(label));
    let w = 30.0 + g.size().x + if closable { 22.0 } else { 10.0 };
    let (r, resp) = ui.allocate_exact_size(egui::vec2(w, 24.0), Sense::click_and_drag());
    if on {
        ui.painter().rect_filled(r, 4.0, p.selected);
    } else if resp.hovered() {
        ui.painter().rect_filled(r, 4.0, p.hover);
    }
    theme::paint_icon(ui.painter(), Rect::from_center_size(egui::pos2(r.min.x + 14.0, r.center().y), egui::vec2(13.0, 13.0)), icon, if on { p.accent } else { p.placeholder });
    ui.painter().galley(egui::pos2(r.min.x + 26.0, r.center().y - g.size().y * 0.5), g, p.text);
    let mut close = false;
    if closable {
        let xr = Rect::from_center_size(egui::pos2(r.max.x - 12.0, r.center().y), egui::vec2(16.0, 16.0));
        let pointer_in = ui.input(|i| i.pointer.hover_pos()).is_some_and(|q| xr.contains(q));
        if on || resp.hovered() {
            if pointer_in {
                ui.painter().rect_filled(xr, 3.0, p.hover);
            }
            theme::paint_icon(ui.painter(), xr.shrink(3.0), Icon::Close, if pointer_in { p.text } else { p.placeholder });
        }
        close = resp.clicked() && pointer_in;
    }
    DrawnTab { resp, rect: r, close }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(format!("/w/{s}"))
    }

    fn names(e: &Editor, g: usize) -> Vec<String> {
        e.groups[g].tabs.iter().map(|t| t.file.as_ref().map(|f| f.0.file_name().unwrap().to_string_lossy().to_string()).unwrap_or_else(|| "3D".into())).collect()
    }

    #[test]
    fn previews_replace_each_other_and_kept_tabs_stay() {
        let mut e = Editor::default();
        assert!(e.only_plate());
        let a = e.open(&p("a.md"), FileKind::Text, false, None);
        assert!(matches!(a, Opened::New(_)));
        let b = e.open(&p("b.png"), FileKind::Image, false, None);
        assert!(matches!(b, Opened::Replaced { .. }), "a preview takes the previous preview's place");
        assert_eq!(names(&e, 0), ["3D", "b.png"]);
        e.open(&p("b.png"), FileKind::Image, true, None);
        e.open(&p("c.stl"), FileKind::Model, false, None);
        assert_eq!(names(&e, 0), ["3D", "b.png", "c.stl"], "kept b stays, c opens after it");
        assert!(e.open(&p("b.png"), FileKind::Image, false, None) == Opened::Activated(e.groups[0].tabs[1].id));
    }

    #[test]
    fn closing_moves_on_and_reopens_and_the_plate_stays() {
        let mut e = Editor::default();
        for f in ["a", "b", "c"] {
            e.open(&p(f), FileKind::Text, true, None);
        }
        let b = e.groups[0].tabs[2].id;
        e.activate(b);
        assert_eq!(e.close_many(&[b]), vec![b]);
        assert_eq!(names(&e, 0), ["3D", "a", "c"]);
        assert_eq!(e.active(0).file.as_ref().unwrap().0, p("c"), "the tab to the right takes over");
        assert_eq!(e.close_many(&[0]), Vec::<u64>::new(), "the plate can't be closed");
        assert_eq!(e.reopen().unwrap().0, p("b"));
        let c = e.groups[0].tabs[2].id;
        assert_eq!(e.right_of(e.groups[0].tabs[1].id), vec![c]);
        assert_eq!(e.others(c).len(), 1, "others leaves the plate alone");
    }

    #[test]
    fn tabs_reorder_split_and_join() {
        let mut e = Editor::default();
        for f in ["a", "b"] {
            e.open(&p(f), FileKind::Text, true, None);
        }
        let a = e.groups[0].tabs[1].id;
        e.move_tab(a, 0, 3);
        assert_eq!(names(&e, 0), ["3D", "b", "a"]);
        e.move_tab(a, 9, 0);
        assert_eq!(e.groups.len(), 2);
        assert_eq!(names(&e, 1), ["a"]);
        assert_eq!(e.focus, 1);
        e.cycle(1);
        e.toggle_split();
        assert_eq!(e.groups.len(), 1);
        assert_eq!(names(&e, 0), ["3D", "b", "a"]);
        assert_eq!(e.active(0).id, a, "the focused tab stays showing");
        // ⌘\ with the plate showing moves the last file over
        e.show_plate();
        e.toggle_split();
        assert_eq!((names(&e, 0), names(&e, 1)), (vec!["3D".to_string(), "b".to_string()], vec!["a".to_string()]));
        // emptying a group removes it
        let a = e.groups[1].tabs[0].id;
        e.close_many(&[a]);
        assert_eq!(e.groups.len(), 1);
        assert!(e.plate_visible() || e.active(0).file.is_some());
    }

    #[test]
    fn drops_land_where_they_are_aimed() {
        let mut r = Regions::default();
        let strip = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(400.0, 30.0));
        let body = Rect::from_min_max(egui::pos2(0.0, 30.0), egui::pos2(400.0, 300.0));
        r.groups.push((0, strip, body, true));
        r.tabs.push((0, 0, Rect::from_min_max(egui::pos2(4.0, 3.0), egui::pos2(100.0, 27.0))));
        assert_eq!(r.spot(egui::pos2(90.0, 15.0)), Some(Spot::Group(0, 1)));
        assert_eq!(r.spot(egui::pos2(200.0, 15.0)), Some(Spot::Group(0, 1)));
        assert_eq!(r.spot(egui::pos2(100.0, 150.0)), Some(Spot::Plate));
        assert_eq!(r.spot(egui::pos2(380.0, 150.0)), Some(Spot::NewGroup));
        assert_eq!(r.spot(egui::pos2(500.0, 150.0)), None);
    }
}
