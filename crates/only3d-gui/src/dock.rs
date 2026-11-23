//! The dock layout: three docks (left, right, bottom), each a stack of panes, each pane a set of
//! panel tabs with one active. Pure data + operations (rendering lives in `chrome.rs`), persisted
//! as JSON in `~/.only3d/ui.json`.

use serde::{Deserialize, Serialize};

use crate::theme::Icon;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelId {
    /// The plates and their objects (was "Workspace": old layouts still load).
    #[serde(alias = "objects")]
    Plates,
    /// The workspace folder as a file tree.
    Files,
    /// Recent prints.
    Prints,
    Parameters,
    Inspector,
    Print,
    Settings,
    Spools,
    Printer,
    Checks,
    Activity,
}

impl PanelId {
    pub const ALL: [PanelId; 11] = [PanelId::Plates, PanelId::Files, PanelId::Prints, PanelId::Parameters, PanelId::Inspector, PanelId::Print, PanelId::Settings, PanelId::Spools, PanelId::Printer, PanelId::Checks, PanelId::Activity];

    pub fn label(self) -> &'static str {
        match self {
            PanelId::Plates => "Plates",
            PanelId::Files => "Files",
            PanelId::Prints => "Prints",
            PanelId::Parameters => "Parameters",
            PanelId::Inspector => "Inspector",
            PanelId::Print => "Print",
            PanelId::Settings => "Settings",
            PanelId::Spools => "Spools",
            PanelId::Printer => "Printer",
            PanelId::Checks => "Checks",
            PanelId::Activity => "Activity",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            PanelId::Plates => "plates",
            PanelId::Files => "files",
            PanelId::Prints => "prints",
            PanelId::Parameters => "parameters",
            PanelId::Inspector => "inspector",
            PanelId::Print => "print",
            PanelId::Settings => "settings",
            PanelId::Spools => "spools",
            PanelId::Printer => "printer",
            PanelId::Checks => "checks",
            PanelId::Activity => "activity",
        }
    }

    pub fn parse(s: &str) -> Option<PanelId> {
        let s = s.trim().to_ascii_lowercase();
        if s == "objects" || s == "workspace" {
            return Some(PanelId::Plates);
        }
        PanelId::ALL.into_iter().find(|p| p.id() == s)
    }

    pub fn icon(self) -> Icon {
        match self {
            PanelId::Plates => Icon::Grid,
            PanelId::Files => Icon::Folder,
            PanelId::Prints => Icon::Printer,
            PanelId::Parameters => Icon::Hash,
            PanelId::Inspector => Icon::Info,
            PanelId::Print => Icon::Layers,
            PanelId::Settings => Icon::Sliders,
            PanelId::Spools => Icon::Spool,
            PanelId::Printer => Icon::Printer,
            PanelId::Checks => Icon::Check,
            PanelId::Activity => Icon::Activity,
        }
    }

    /// Where the panel goes when it is reopened and its old place is gone.
    pub fn home(self) -> Side {
        match self {
            PanelId::Plates | PanelId::Files | PanelId::Prints | PanelId::Parameters | PanelId::Inspector => Side::Left,
            PanelId::Print | PanelId::Settings | PanelId::Spools | PanelId::Printer => Side::Right,
            PanelId::Checks | PanelId::Activity => Side::Bottom,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Left,
    Right,
    Bottom,
}

impl Side {
    pub const ALL: [Side; 3] = [Side::Left, Side::Right, Side::Bottom];
    pub fn label(self) -> &'static str {
        match self {
            Side::Left => "Left Dock",
            Side::Right => "Right Dock",
            Side::Bottom => "Bottom Dock",
        }
    }
    pub fn icon(self) -> Icon {
        match self {
            Side::Left => Icon::DockLeft,
            Side::Right => Icon::DockRight,
            Side::Bottom => Icon::DockBottom,
        }
    }
    pub fn parse(s: &str) -> Option<Side> {
        match s.trim().to_ascii_lowercase().as_str() {
            "left" => Some(Side::Left),
            "right" => Some(Side::Right),
            "bottom" => Some(Side::Bottom),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pane {
    pub tabs: Vec<PanelId>,
    #[serde(default)]
    pub active: usize,
    /// Share of the dock's length (normalised when drawn).
    #[serde(default = "one")]
    pub weight: f32,
    /// Folded down to its tab strip (a column of icons in the bottom dock).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub folded: bool,
    /// Sized by hand (its splitter was dragged): it keeps its share whatever it holds. Other
    /// panes lend the room their content doesn't use to the ones that scroll.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sized: bool,
}

fn one() -> f32 {
    1.0
}

impl Pane {
    pub fn new(tabs: Vec<PanelId>, weight: f32) -> Self {
        Pane { tabs, active: 0, weight, folded: false, sized: false }
    }
    pub fn active_panel(&self) -> Option<PanelId> {
        self.tabs.get(self.active.min(self.tabs.len().saturating_sub(1))).copied()
    }
}

/// The least a pane that lends room keeps (its tab strip and a few rows).
pub const PANE_MIN: f32 = 96.0;

/// Lengths of a dock's open panes along it. Each starts with its share of `avail` by weight;
/// a pane that `wants` less than its share (its content is shorter) lends the difference, down
/// to [`PANE_MIN`], to the panes that want more, by weight and never past what they want. What
/// nobody wants goes back to the lenders. `None` for a want keeps the share: a pane sized by
/// hand, or one whose content hasn't been measured.
pub fn pane_lengths(avail: f32, panes: &[(f32, Option<f32>)]) -> Vec<f32> {
    let total: f32 = panes.iter().map(|(w, _)| *w).sum::<f32>().max(1e-6);
    let mut len: Vec<f32> = panes.iter().map(|(w, _)| avail * w / total).collect();
    let mut lent = vec![0.0f32; panes.len()];
    for (i, (_, want)) in panes.iter().enumerate() {
        if let Some(want) = want.filter(|x| *x < len[i]) {
            let keep = want.max(PANE_MIN).min(len[i]);
            lent[i] = len[i] - keep;
            len[i] = keep;
        }
    }
    let mut spare: f32 = lent.iter().sum();
    // by weight, capped at each pane's need; what a capped pane leaves goes round again
    for _ in 0..panes.len() {
        let needy: Vec<usize> = (0..panes.len()).filter(|i| panes[*i].1.is_some_and(|want| want > len[*i] + 0.5)).collect();
        let weights: f32 = needy.iter().map(|i| panes[*i].0).sum();
        if spare < 0.5 || needy.is_empty() || weights <= 0.0 {
            break;
        }
        let round = spare;
        for i in needy {
            let give = (round * panes[i].0 / weights).min(panes[i].1.unwrap_or(0.0) - len[i]);
            len[i] += give;
            spare -= give;
        }
    }
    let lent_total: f32 = lent.iter().sum();
    if spare > 0.5 && lent_total > 0.0 {
        for i in 0..panes.len() {
            len[i] += spare * lent[i] / lent_total;
        }
    }
    len
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Dock {
    #[serde(default)]
    pub panes: Vec<Pane>,
    #[serde(default = "yes")]
    pub open: bool,
    /// Width (side docks) or height (bottom dock) in points.
    #[serde(default = "default_size")]
    pub size: f32,
}

fn yes() -> bool {
    true
}
fn default_size() -> f32 {
    300.0
}

impl Dock {
    pub fn is_empty(&self) -> bool {
        self.panes.iter().all(|p| p.tabs.is_empty())
    }
    /// Shown on screen: open and holding something.
    pub fn visible(&self) -> bool {
        self.open && !self.is_empty()
    }
}

/// Where a dragged tab (or a "move to" command) puts a panel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    /// Into an existing pane's tab strip at `index`.
    Tab { side: Side, pane: usize, index: usize },
    /// A new pane before (`after == false`) or after pane `pane`.
    Split { side: Side, pane: usize, after: bool },
    /// A new pane at the end of the dock.
    Dock(Side),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub left: Dock,
    pub right: Dock,
    pub bottom: Dock,
    /// Zen / focus mode: every dock hidden, the viewport fills the window.
    #[serde(default)]
    pub zen: bool,
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            left: Dock { panes: vec![Pane::new(vec![PanelId::Files, PanelId::Prints], 0.25), Pane::new(vec![PanelId::Plates, PanelId::Parameters], 0.40), Pane::new(vec![PanelId::Inspector], 0.35)], open: true, size: 290.0 },
            right: Dock { panes: vec![Pane::new(vec![PanelId::Print, PanelId::Settings, PanelId::Spools, PanelId::Printer], 1.0)], open: true, size: 330.0 },
            bottom: Dock { panes: vec![Pane::new(vec![PanelId::Checks], 1.0), Pane::new(vec![PanelId::Activity], 1.2)], open: true, size: 170.0 },
            zen: false,
        }
    }
}

impl Layout {
    pub fn dock(&self, side: Side) -> &Dock {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
            Side::Bottom => &self.bottom,
        }
    }

    pub fn dock_mut(&mut self, side: Side) -> &mut Dock {
        match side {
            Side::Left => &mut self.left,
            Side::Right => &mut self.right,
            Side::Bottom => &mut self.bottom,
        }
    }

    /// (side, pane, tab) of a panel, if it is docked anywhere.
    pub fn find(&self, panel: PanelId) -> Option<(Side, usize, usize)> {
        for side in Side::ALL {
            for (pi, pane) in self.dock(side).panes.iter().enumerate() {
                if let Some(ti) = pane.tabs.iter().position(|t| *t == panel) {
                    return Some((side, pi, ti));
                }
            }
        }
        None
    }

    /// Is the panel on screen right now (docked, active in its pane, dock visible, not zen)?
    pub fn is_showing(&self, panel: PanelId) -> bool {
        match self.find(panel) {
            Some((side, pi, ti)) => !self.zen && self.dock(side).open && self.dock(side).panes[pi].active == ti && !self.dock(side).panes[pi].folded,
            None => false,
        }
    }

    pub fn hidden_panels(&self) -> Vec<PanelId> {
        PanelId::ALL.into_iter().filter(|p| self.find(*p).is_none()).collect()
    }

    fn remove_at(&mut self, side: Side, pi: usize, ti: usize) {
        let dock = self.dock_mut(side);
        let pane = &mut dock.panes[pi];
        pane.tabs.remove(ti);
        if pane.active > ti || pane.active >= pane.tabs.len() {
            pane.active = pane.active.saturating_sub(1);
        }
        if pane.tabs.is_empty() {
            dock.panes.remove(pi);
        }
    }

    /// Take a panel out of the docks. It is listed under "Add a panel" in every pane's menu, in
    /// the command palette and in preferences.
    pub fn close(&mut self, panel: PanelId) {
        if let Some((s, p, t)) = self.find(panel) {
            self.remove_at(s, p, t);
        }
    }

    /// Put a panel at `target`, moving it if it is docked elsewhere.
    pub fn place(&mut self, panel: PanelId, target: Target) {
        let src = self.find(panel);
        // normalise the target against the current state
        let target = match target {
            Target::Tab { side, pane, index } if pane < self.dock(side).panes.len() => Target::Tab { side, pane, index: index.min(self.dock(side).panes[pane].tabs.len()) },
            Target::Tab { side, .. } => Target::Dock(side),
            Target::Split { side, pane, after } => Target::Split { side, pane: pane.min(self.dock(side).panes.len()), after },
            t => t,
        };
        // dropping a tab onto its own position, or splitting a single-tab pane next to itself
        if let (Some((ss, sp, st)), Target::Tab { side, pane, index }) = (src, target) {
            if ss == side && sp == pane && (index == st || index == st + 1) {
                self.dock_mut(side).panes[pane].active = st;
                self.dock_mut(side).open = true;
                return;
            }
        }
        if let (Some((ss, sp, _)), Target::Split { side, pane, .. }) = (src, target) {
            if ss == side && sp == pane && self.dock(side).panes[sp].tabs.len() == 1 {
                self.dock_mut(side).open = true;
                return;
            }
        }
        // insert first, then remove the original (adjusting its indices for the insertion)
        let (tside, tpane) = match target {
            Target::Tab { side, pane, index } => {
                let p = &mut self.dock_mut(side).panes[pane];
                p.tabs.insert(index, panel);
                p.active = index;
                (side, pane)
            }
            Target::Split { side, pane, after } => {
                let at = if after { (pane + 1).min(self.dock(side).panes.len()) } else { pane };
                let weight = self.dock(side).panes.get(pane).map(|p| p.weight).unwrap_or(1.0);
                self.dock_mut(side).panes.insert(at, Pane::new(vec![panel], weight));
                (side, at)
            }
            Target::Dock(side) => {
                let w = self.dock(side).panes.iter().map(|p| p.weight).sum::<f32>() / self.dock(side).panes.len().max(1) as f32;
                self.dock_mut(side).panes.push(Pane::new(vec![panel], if w > 0.0 { w } else { 1.0 }));
                (side, self.dock(side).panes.len() - 1)
            }
        };
        let mut tpane = tpane;
        if let Some((ss, mut sp, mut st)) = src {
            match target {
                Target::Tab { side, pane, index } if side == ss && pane == sp && st >= index => st += 1,
                Target::Split { side, .. } if side == ss && sp >= tpane => sp += 1,
                _ => {}
            }
            let removes_pane = self.dock(ss).panes[sp].tabs.len() == 1;
            self.remove_at(ss, sp, st);
            if removes_pane && ss == tside && sp < tpane {
                tpane -= 1;
            }
        }
        if let Some(p) = self.dock_mut(tside).panes.get_mut(tpane) {
            if let Some(i) = p.tabs.iter().position(|t| *t == panel) {
                p.active = i;
            }
        }
        self.dock_mut(tside).open = true;
    }

    /// Bring a panel on screen: activate its tab and open its dock, or re-dock it at home.
    pub fn show(&mut self, panel: PanelId) {
        self.zen = false;
        match self.find(panel) {
            Some((side, pi, ti)) => {
                let d = self.dock_mut(side);
                d.panes[pi].active = ti;
                d.panes[pi].folded = false;
                d.open = true;
            }
            None => {
                let side = panel.home();
                if self.dock(side).panes.is_empty() {
                    self.place(panel, Target::Dock(side));
                } else {
                    let n = self.dock(side).panes[0].tabs.len();
                    self.place(panel, Target::Tab { side, pane: 0, index: n });
                }
            }
        }
    }

    /// Show the panel, or collapse its dock if it is already the one on screen.
    pub fn toggle_panel(&mut self, panel: PanelId) {
        if self.is_showing(panel) {
            if let Some((side, _, _)) = self.find(panel) {
                self.dock_mut(side).open = false;
            }
        } else {
            self.show(panel);
        }
    }

    pub fn toggle_dock(&mut self, side: Side) {
        if self.zen {
            self.zen = false;
            self.dock_mut(side).open = true;
            return;
        }
        let d = self.dock_mut(side);
        d.open = !d.open;
    }

    /// Click on a tab: activate it, or collapse the dock when it already is the active tab.
    pub fn click_tab(&mut self, side: Side, pane: usize, tab: usize) {
        let split = self.dock(side).panes.len() > 1;
        let d = self.dock_mut(side);
        if let Some(p) = d.panes.get_mut(pane) {
            if p.folded {
                p.folded = false;
                p.active = tab.min(p.tabs.len().saturating_sub(1));
                d.open = true;
            } else if p.active == tab {
                // the active tab folds its pane; a dock with one pane collapses instead
                if split {
                    p.folded = true;
                } else {
                    d.open = !d.open;
                }
            } else {
                p.active = tab.min(p.tabs.len().saturating_sub(1));
                d.open = true;
            }
        }
    }

    /// Closing used to take a panel out of the docks: bring every closed panel back as a folded
    /// pane at the end of its home dock, so it shows as a tab again.
    pub fn restore_closed_folded(&mut self) {
        for panel in self.hidden_panels() {
            let side = panel.home();
            self.place(panel, Target::Dock(side));
            if let Some((s, pi, _)) = self.find(panel) {
                let d = self.dock_mut(s);
                d.panes[pi].folded = !d.panes.iter().enumerate().all(|(i, p)| i == pi || p.folded);
                d.panes[pi].weight = 0.4;
            }
        }
    }

    /// Fold a pane down to its tabs, or open it again.
    pub fn set_folded(&mut self, side: Side, pane: usize, folded: bool) {
        if let Some(p) = self.dock_mut(side).panes.get_mut(pane) {
            p.folded = folded;
        }
    }

    /// Layouts saved before Files and Prints were panels of their own (they were sections of
    /// the Workspace panel): give them their default places once.
    pub fn add_split_workspace_panels(&mut self) {
        if self.find(PanelId::Files).is_none() {
            match self.find(PanelId::Plates) {
                Some((side, pi, _)) => {
                    // a pane of its own right below the plates
                    let w = self.dock(side).panes[pi].weight;
                    self.dock_mut(side).panes[pi].weight = w * 0.62;
                    self.place(PanelId::Files, Target::Split { side, pane: pi, after: true });
                    if let Some((s2, p2, _)) = self.find(PanelId::Files) {
                        self.dock_mut(s2).panes[p2].weight = w * 0.38;
                    }
                }
                None => self.show(PanelId::Files),
            }
        }
        if self.find(PanelId::Prints).is_none() {
            match self.find(PanelId::Activity) {
                Some((side, pi, _)) => {
                    let n = self.dock(side).panes[pi].tabs.len();
                    self.place(PanelId::Prints, Target::Tab { side, pane: pi, index: n });
                }
                None => self.show(PanelId::Prints),
            }
        }
    }

    /// Layouts from before Prints sat with Files (`layout_version` 4): Prints joins the Files
    /// pane, and the left dock reads Files + Prints, Plates + Parameters, Inspector. A panel the
    /// user moved to another dock, or closed, stays where it is.
    pub fn prints_with_files(&mut self) {
        if let (Some((Side::Left, fp, ft)), Some((Side::Bottom, _, _))) = (self.find(PanelId::Files), self.find(PanelId::Prints)) {
            self.place(PanelId::Prints, Target::Tab { side: Side::Left, pane: fp, index: ft + 1 });
            // Files stays the tab you see
            if let Some((s, pi, ti)) = self.find(PanelId::Files) {
                self.dock_mut(s).panes[pi].active = ti;
            }
        }
        let rank = |p: &Pane| {
            p.tabs
                .iter()
                .map(|t| match t {
                    PanelId::Files | PanelId::Prints => 0,
                    PanelId::Plates | PanelId::Parameters => 1,
                    PanelId::Inspector => 2,
                    _ => 3,
                })
                .min()
                .unwrap_or(3)
        };
        self.left.panes.sort_by_key(rank);
    }

    /// Repair a layout read from disk: every panel at most once, indices in range, sane sizes.
    pub fn sanitize(&mut self) {
        let mut seen = std::collections::HashSet::new();
        for side in Side::ALL {
            let d = self.dock_mut(side);
            for p in d.panes.iter_mut() {
                p.tabs.retain(|t| seen.insert(*t));
                p.active = p.active.min(p.tabs.len().saturating_sub(1));
                if !(p.weight.is_finite() && p.weight > 0.05) {
                    p.weight = 1.0;
                }
            }
            d.panes.retain(|p| !p.tabs.is_empty());
            let (lo, hi) = if side == Side::Bottom { (80.0, 900.0) } else { (200.0, 900.0) };
            if !d.size.is_finite() {
                d.size = default_size();
            }
            d.size = d.size.clamp(lo, hi);
        }
    }

    /// Apply a compact layout script, e.g. `settings:right,activity:hidden,bottom:collapsed` —
    /// `<panel>:<left|right|bottom|hidden|folded|split-left|…>`, `<dock>:<open|collapsed>`, `zen`.
    /// Used by the screenshot self-test (`ONLY3D_GUI_LAYOUT`).
    pub fn apply_script(&mut self, script: &str) -> Result<(), String> {
        for op in script.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if op == "zen" {
                self.zen = true;
                continue;
            }
            if op == "reset" {
                *self = Layout::default();
                continue;
            }
            let (a, b) = op.split_once(':').ok_or_else(|| format!("bad layout op '{op}'"))?;
            if let Some(side) = Side::parse(a) {
                match b {
                    "open" => self.dock_mut(side).open = true,
                    "collapsed" | "closed" => self.dock_mut(side).open = false,
                    _ => return Err(format!("bad dock state '{b}'")),
                }
                continue;
            }
            let panel = PanelId::parse(a).ok_or_else(|| format!("unknown panel '{a}'"))?;
            if b == "hidden" {
                self.close(panel);
            } else if b == "folded" {
                if let Some((s, pi, _)) = self.find(panel) {
                    self.set_folded(s, pi, true);
                }
            } else if let Some(side) = b.strip_prefix("split-").and_then(Side::parse) {
                self.place(panel, Target::Dock(side));
            } else if let Some(side) = Side::parse(b) {
                let n = self.dock(side).panes.len();
                if n == 0 {
                    self.place(panel, Target::Dock(side));
                } else {
                    let len = self.dock(side).panes[0].tabs.len();
                    self.place(panel, Target::Tab { side, pane: 0, index: len });
                }
            } else {
                return Err(format!("bad panel place '{b}'"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panes_lend_the_room_they_do_not_use() {
        let near = |a: &[f32], b: &[f32]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.6);
        // nothing measured: plain shares
        assert!(near(&pane_lengths(800.0, &[(0.25, None), (0.40, None), (0.35, None)]), &[200.0, 320.0, 280.0]));
        // the middle pane holds two rows; the other two scroll: they share what it lends, by weight
        let l = pane_lengths(800.0, &[(0.25, Some(900.0)), (0.40, Some(120.0)), (0.35, Some(900.0))]);
        assert!(near(&l, &[200.0 + 200.0 * 0.25 / 0.6, 120.0, 280.0 + 200.0 * 0.35 / 0.6]), "{l:?}");
        // a pane takes no more than it wants; the rest goes to the other one that scrolls
        let l = pane_lengths(800.0, &[(0.25, Some(230.0)), (0.40, Some(120.0)), (0.35, Some(900.0))]);
        assert!(near(&l, &[230.0, 120.0, 450.0]), "{l:?}");
        // nobody wants it: back to the lender
        let l = pane_lengths(800.0, &[(0.25, Some(200.0)), (0.40, Some(120.0)), (0.35, None)]);
        assert!(near(&l, &[200.0, 320.0, 280.0]), "{l:?}");
        // a pane never lends below the minimum, and one sized by hand keeps its share
        let l = pane_lengths(800.0, &[(0.5, Some(10.0)), (0.5, None)]);
        assert!(near(&l, &[400.0, 400.0]), "{l:?}");
        let l = pane_lengths(800.0, &[(0.5, Some(10.0)), (0.25, None), (0.25, Some(900.0))]);
        assert!(near(&l, &[PANE_MIN, 200.0, 800.0 - PANE_MIN - 200.0]), "{l:?}");
        for l in [pane_lengths(800.0, &[(0.3, Some(50.0)), (0.3, Some(2000.0)), (0.4, Some(300.0))]), pane_lengths(500.0, &[(1.0, Some(900.0)), (1.0, Some(900.0))])] {
            assert!((l.iter().sum::<f32>() - if l.len() == 3 { 800.0 } else { 500.0 }).abs() < 1.0, "{l:?}");
        }
    }

    fn all_once(l: &Layout) {
        let mut n = 0;
        for side in Side::ALL {
            for p in &l.dock(side).panes {
                assert!(!p.tabs.is_empty(), "empty pane left behind");
                assert!(p.active < p.tabs.len());
                n += p.tabs.len();
            }
        }
        let hidden = l.hidden_panels().len();
        assert_eq!(n + hidden, PanelId::ALL.len());
    }

    #[test]
    fn default_layout_has_every_panel_once() {
        let l = Layout::default();
        all_once(&l);
        assert!(l.hidden_panels().is_empty());
    }

    #[test]
    fn move_between_docks() {
        let mut l = Layout::default();
        l.place(PanelId::Settings, Target::Dock(Side::Left));
        assert_eq!(l.find(PanelId::Settings).map(|f| f.0), Some(Side::Left));
        assert_eq!(l.left.panes.len(), 4);
        all_once(&l);
        // Inspector (alone in its pane) into the right dock's tab strip: its pane disappears
        l.place(PanelId::Inspector, Target::Tab { side: Side::Right, pane: 0, index: 0 });
        assert_eq!(l.find(PanelId::Inspector), Some((Side::Right, 0, 0)));
        assert_eq!(l.right.panes[0].active, 0);
        assert_eq!(l.left.panes.len(), 3);
        all_once(&l);
    }

    #[test]
    fn reorder_within_a_pane() {
        let mut l = Layout::default();
        // right pane: Print, Settings, Spools, Printer → move Printer to the front
        l.place(PanelId::Printer, Target::Tab { side: Side::Right, pane: 0, index: 0 });
        assert_eq!(l.right.panes[0].tabs, vec![PanelId::Printer, PanelId::Print, PanelId::Settings, PanelId::Spools]);
        // move Print (now index 1) to the end
        l.place(PanelId::Print, Target::Tab { side: Side::Right, pane: 0, index: 4 });
        assert_eq!(l.right.panes[0].tabs, vec![PanelId::Printer, PanelId::Settings, PanelId::Spools, PanelId::Print]);
        assert_eq!(l.right.panes[0].active, 3);
        all_once(&l);
    }

    #[test]
    fn split_and_merge() {
        let mut l = Layout::default();
        l.place(PanelId::Settings, Target::Split { side: Side::Right, pane: 0, after: true });
        assert_eq!(l.right.panes.len(), 2);
        assert_eq!(l.right.panes[1].tabs, vec![PanelId::Settings]);
        // split before the first pane of the left dock, from the bottom dock
        l.place(PanelId::Activity, Target::Split { side: Side::Left, pane: 0, after: false });
        assert_eq!(l.left.panes[0].tabs, vec![PanelId::Activity]);
        // Activity was alone in its pane: the pane goes, Checks stays
        assert_eq!(l.bottom.panes.len(), 1);
        assert_eq!(l.bottom.panes[0].tabs, vec![PanelId::Checks]);
        // Files stays where Prints was
        l.place(PanelId::Prints, Target::Tab { side: Side::Bottom, pane: 0, index: 1 });
        assert_eq!(l.left.panes[1].tabs, vec![PanelId::Files]);
        assert_eq!(l.bottom.panes[0].tabs, vec![PanelId::Checks, PanelId::Prints]);
        // the last pane of the bottom dock goes away: dock becomes empty
        l.place(PanelId::Checks, Target::Tab { side: Side::Left, pane: 1, index: 0 });
        l.place(PanelId::Prints, Target::Tab { side: Side::Left, pane: 1, index: 0 });
        assert!(l.bottom.is_empty());
        assert!(!l.bottom.visible());
        all_once(&l);
        // splitting a single-tab pane next to itself is a no-op
        let before = l.clone();
        l.place(PanelId::Activity, Target::Split { side: Side::Left, pane: 0, after: true });
        assert_eq!(l, before);
    }

    #[test]
    fn split_within_same_dock_adjusts_indices() {
        let mut l = Layout::default();
        // left: [Files, Prints] [Plates, Parameters] [Inspector]; move Inspector before pane 0
        l.place(PanelId::Inspector, Target::Split { side: Side::Left, pane: 0, after: false });
        assert_eq!(l.left.panes.len(), 3);
        assert_eq!(l.left.panes[0].tabs, vec![PanelId::Inspector]);
        assert_eq!(l.left.panes[1].tabs, vec![PanelId::Files, PanelId::Prints]);
        // move Prints out of pane 1 into a new pane after it
        l.place(PanelId::Prints, Target::Split { side: Side::Left, pane: 1, after: true });
        assert_eq!(l.left.panes.len(), 4);
        assert_eq!(l.left.panes[2].tabs, vec![PanelId::Prints]);
        all_once(&l);
    }

    #[test]
    fn close_and_reopen() {
        let mut l = Layout::default();
        l.close(PanelId::Activity);
        assert_eq!(l.hidden_panels(), vec![PanelId::Activity]);
        assert!(!l.is_showing(PanelId::Activity));
        l.bottom.open = false;
        l.show(PanelId::Activity);
        assert!(l.is_showing(PanelId::Activity));
        assert!(l.bottom.open);
        all_once(&l);
        // closing everything in a dock then reopening re-creates a pane at home
        l.close(PanelId::Checks);
        l.close(PanelId::Activity);
        l.close(PanelId::Prints);
        assert!(l.bottom.panes.is_empty());
        l.show(PanelId::Checks);
        assert_eq!(l.find(PanelId::Checks), Some((Side::Bottom, 0, 0)));
    }

    #[test]
    fn folded_panes_keep_their_tabs() {
        let mut l = Layout::default();
        let (side, pi, ti) = l.find(PanelId::Inspector).unwrap();
        // clicking the active tab of a split dock folds that pane, not the dock
        l.click_tab(side, pi, ti);
        assert!(l.dock(side).panes[pi].folded);
        assert!(l.dock(side).open);
        assert!(!l.is_showing(PanelId::Inspector));
        assert_eq!(l.find(PanelId::Inspector), Some((side, pi, ti)));
        assert!(l.hidden_panels().is_empty());
        // clicking it again opens it
        l.click_tab(side, pi, ti);
        assert!(l.is_showing(PanelId::Inspector));
        // showing a panel unfolds its pane
        l.set_folded(side, pi, true);
        l.show(PanelId::Inspector);
        assert!(l.is_showing(PanelId::Inspector));
        // a dock with a single pane still collapses as a whole
        let (rs, rp, rt) = l.find(PanelId::Print).unwrap();
        l.click_tab(rs, rp, rt);
        assert!(!l.dock(rs).open);
        assert!(!l.dock(rs).panes[rp].folded);
        // the flag round-trips, and old layouts without it load unfolded
        l.set_folded(side, pi, true);
        let back: Layout = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
        assert!(back.dock(side).panes[pi].folded);
        all_once(&back);
    }

    #[test]
    fn old_layouts_get_files_and_prints_once() {
        // a ui.json from before the split: "objects" is the Plates panel now
        let old = r#"{"left":{"panes":[{"tabs":["objects","parameters"],"weight":0.4},{"tabs":["inspector"],"weight":0.6}]},"right":{"panes":[{"tabs":["print","settings","spools","printer"]}]},"bottom":{"panes":[{"tabs":["checks"]},{"tabs":["activity"]}]}}"#;
        let mut c: crate::config::UiConfig = serde_json::from_str(&format!(r#"{{"layout": {old}}}"#)).unwrap();
        assert_eq!(c.layout_version, 0);
        c.sanitize();
        let l = &c.layout;
        assert_eq!(l.left.panes[0].tabs, vec![PanelId::Files, PanelId::Prints]);
        assert_eq!(l.left.panes[1].tabs, vec![PanelId::Plates, PanelId::Parameters]);
        assert_eq!(l.left.panes[0].active, 0);
        all_once(l);
        // a panel the user closes afterwards stays closed
        c.layout.close(PanelId::Files);
        c.sanitize();
        assert!(!c.layout.is_showing(PanelId::Files));
    }

    #[test]
    fn closed_panels_come_back_folded_once() {
        // the user's layout: the Inspector closed before folding existed
        let old = r#"{"left":{"panes":[{"tabs":["plates","parameters"]},{"tabs":["files"]}]},"right":{"panes":[{"tabs":["print","settings","spools","printer"]}]},"bottom":{"panes":[{"tabs":["activity","prints"]},{"tabs":["checks"]}]}}"#;
        let mut c: crate::config::UiConfig = serde_json::from_str(&format!(r#"{{"layout": {old}, "layout_version": 2}}"#)).unwrap();
        c.sanitize();
        let (side, pi, _) = c.layout.find(PanelId::Inspector).unwrap();
        assert_eq!((side, pi), (Side::Left, 2));
        assert!(c.layout.left.panes[2].folded);
        assert!(!c.layout.left.panes[0].folded);
        all_once(&c.layout);
        // hidden again by the user: it stays hidden
        c.layout.close(PanelId::Inspector);
        c.sanitize();
        assert!(c.layout.find(PanelId::Inspector).is_none());
    }

    #[test]
    fn prints_join_files_once() {
        // the user's layout on 2026-11-16 (Files already moved to the top)
        let old = r#"{"left":{"panes":[{"tabs":["files"],"weight":0.25},{"tabs":["plates","parameters"],"weight":0.46},{"tabs":["inspector"],"weight":0.26}],"size":403.0},"right":{"panes":[{"tabs":["print","settings","spools","printer"]}]},"bottom":{"panes":[{"tabs":["checks"]},{"tabs":["activity","prints"],"active":1}]}}"#;
        let mut c: crate::config::UiConfig = serde_json::from_str(&format!(r#"{{"layout": {old}, "layout_version": 3}}"#)).unwrap();
        c.sanitize();
        let left: Vec<Vec<PanelId>> = c.layout.left.panes.iter().map(|p| p.tabs.clone()).collect();
        assert_eq!(left, vec![vec![PanelId::Files, PanelId::Prints], vec![PanelId::Plates, PanelId::Parameters], vec![PanelId::Inspector]]);
        assert_eq!(c.layout.left.panes[0].active, 0);
        assert_eq!(c.layout.left.size, 403.0);
        assert_eq!(c.layout.bottom.panes[1].tabs, vec![PanelId::Activity]);
        all_once(&c.layout);
        // moved back by the user afterwards: it stays
        c.layout.place(PanelId::Prints, Target::Dock(Side::Bottom));
        c.sanitize();
        assert_eq!(c.layout.find(PanelId::Prints).map(|f| f.0), Some(Side::Bottom));
        // the old order with Plates on top is put in the new order too
        let old = r#"{"left":{"panes":[{"tabs":["plates","parameters"]},{"tabs":["files"]},{"tabs":["inspector"]}]},"bottom":{"panes":[{"tabs":["checks"]},{"tabs":["activity","prints"]}]}}"#;
        let mut c: crate::config::UiConfig = serde_json::from_str(&format!(r#"{{"layout": {old}, "layout_version": 3}}"#)).unwrap();
        c.sanitize();
        assert_eq!(c.layout.left.panes[0].tabs, vec![PanelId::Files, PanelId::Prints]);
        assert_eq!(c.layout.left.panes[2].tabs, vec![PanelId::Inspector]);
    }

    #[test]
    fn tab_click_collapses_active() {
        let mut l = Layout::default();
        l.click_tab(Side::Right, 0, 1);
        assert_eq!(l.right.panes[0].active, 1);
        assert!(l.right.open);
        l.click_tab(Side::Right, 0, 1);
        assert!(!l.right.open);
        l.toggle_panel(PanelId::Settings);
        assert!(l.right.open);
        l.toggle_panel(PanelId::Settings);
        assert!(!l.right.open);
    }

    #[test]
    fn sanitize_repairs_bad_json() {
        let text = r#"{"left":{"panes":[{"tabs":["objects","objects","inspector"],"active":9}],"size":-5},
                       "right":{"panes":[{"tabs":[]}]},"bottom":{"panes":[{"tabs":["inspector","checks"],"weight":0}]}}"#;
        let mut l: Layout = serde_json::from_str(text).unwrap();
        l.sanitize();
        assert_eq!(l.left.panes[0].tabs, vec![PanelId::Plates, PanelId::Inspector]);
        assert_eq!(l.left.panes[0].active, 1);
        assert!(l.right.panes.is_empty());
        assert_eq!(l.bottom.panes[0].tabs, vec![PanelId::Checks]);
        assert_eq!(l.bottom.panes[0].weight, 1.0);
        assert!(l.left.size >= 200.0);
        all_once(&l);
        let back: Layout = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
        assert_eq!(back, l);
    }

    #[test]
    fn layout_script() {
        let mut l = Layout::default();
        l.apply_script("settings:split-right, activity:hidden, bottom:collapsed, objects:right").unwrap();
        assert_eq!(l.right.panes.len(), 2);
        assert_eq!(l.right.panes[1].tabs, vec![PanelId::Settings]);
        assert!(l.find(PanelId::Activity).is_none());
        assert!(!l.bottom.open);
        assert_eq!(l.find(PanelId::Plates).map(|f| f.0), Some(Side::Right));
        assert!(l.apply_script("nonsense:left").is_err());
        all_once(&l);
    }
}
