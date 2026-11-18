//! UI preferences persisted in `~/.only3d/ui.json` (respects `ONLY3D_HOME`): theme,
//! keymap preset, UI scale, dock layout and collapsed sections.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::dock::Layout;
use crate::keymap::Preset;
use crate::theme::ThemeChoice;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub theme: ThemeChoice,
    pub keymap: Preset,
    pub ui_scale: f32,
    pub layout: Layout,
    /// Section id → open (only sections the user toggled).
    pub sections: BTreeMap<String, bool>,
    pub show_advanced: bool,
    /// Moving, turning and sizing in the viewport.
    pub gizmo: GizmoPrefs,
    /// Where the printer's built-in camera is, in plate mm from the front-left corner, as you
    /// measured it (shown as a vertical line when `show_camera`).
    pub camera_xy: Option<[f32; 2]>,
    pub show_camera: bool,
    /// A desktop notification when a print finishes or fails.
    pub notify_desktop: bool,
    /// Also POST the message here (an ntfy.sh topic URL, for your phone). Off when empty.
    pub notify_url: String,
    /// Look for a newer only3d on GitHub, at most once a day.
    pub check_updates: bool,
    /// When that last happened (seconds since the Unix epoch).
    pub update_checked: u64,
    /// Show every plate side by side (the active one in focus, the others dimmed).
    pub show_all_plates: bool,
    /// Projects opened or saved, most recent first (File > Open Recent).
    pub recent_projects: Vec<String>,
    /// The folder the Save panel used last.
    pub last_save_dir: Option<String>,
    /// The export format used last (`save_panel::Format::id`).
    pub export_format: Option<String>,
    /// Layout changes already applied to a saved layout (2: Files and Prints are panels; 3:
    /// panels closed before folding existed come back folded; 4: Prints sits with Files, and the
    /// left dock reads Files, Plates, Inspector).
    #[serde(default)]
    pub layout_version: u32,
}

/// The layout version a fresh layout is at.
pub const LAYOUT_VERSION: u32 = 4;

/// How the viewport handles feel (see docs/design/transforms.md). Every value is a default
/// you can change here or in `ui.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GizmoPrefs {
    /// "triad": move arrows, rotate ring and size handles together on a selection (CAD style).
    /// "tools": one tool at a time, M / R / S (slicer style).
    pub style: String,
    /// Line up with other objects' edges and centres while dragging, and show the gaps.
    pub smart_guides: bool,
    /// Move snaps to this many mm (hold ⌘/Ctrl while dragging for none).
    pub move_snap_mm: f32,
    /// Rotation snaps to this many degrees (hold ⌘/Ctrl for 1°).
    pub rotate_snap_deg: f32,
}

impl Default for GizmoPrefs {
    fn default() -> Self {
        GizmoPrefs { style: "triad".into(), smart_guides: true, move_snap_mm: 1.0, rotate_snap_deg: 15.0 }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig { theme: ThemeChoice::System, keymap: Preset::Only3d, ui_scale: 1.0, layout: Layout::default(), sections: BTreeMap::new(), show_advanced: false, gizmo: GizmoPrefs::default(), camera_xy: None, show_camera: false, notify_desktop: true, notify_url: String::new(), check_updates: true, update_checked: 0, show_all_plates: true, recent_projects: Vec::new(), last_save_dir: None, export_format: None, layout_version: LAYOUT_VERSION }
    }
}

pub fn path() -> PathBuf {
    only3d_studio::config_dir().join("ui.json")
}

impl UiConfig {
    pub fn load() -> (UiConfig, Option<String>) {
        let p = path();
        match std::fs::read_to_string(&p) {
            Ok(text) => match serde_json::from_str::<UiConfig>(&text) {
                Ok(mut c) => {
                    c.sanitize();
                    (c, None)
                }
                Err(e) => (UiConfig::default(), Some(format!("{}: {e} — using defaults", p.display()))),
            },
            Err(_) => (UiConfig::default(), None),
        }
    }

    pub fn sanitize(&mut self) {
        self.layout.sanitize();
        if self.layout_version < 2 {
            self.layout.add_split_workspace_panels();
        }
        if self.layout_version < 3 {
            self.layout.restore_closed_folded();
        }
        if self.layout_version < 4 {
            self.layout.prints_with_files();
        }
        self.layout_version = self.layout_version.max(LAYOUT_VERSION);
        if !(self.ui_scale.is_finite() && (0.6..=2.0).contains(&self.ui_scale)) {
            self.ui_scale = 1.0;
        }
        let g = &mut self.gizmo;
        if !(g.move_snap_mm.is_finite() && (0.0..=50.0).contains(&g.move_snap_mm)) {
            g.move_snap_mm = 1.0;
        }
        if !(g.rotate_snap_deg.is_finite() && (0.0..=90.0).contains(&g.rotate_snap_deg)) {
            g.rotate_snap_deg = 15.0;
        }
        if g.style != "triad" && g.style != "tools" {
            g.style = "triad".into();
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let p = path();
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        let text = serde_json::to_string_pretty(self).unwrap_or_default();
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, text + "\n")?;
        std::fs::rename(tmp, p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_tolerant_parse() {
        let mut c = UiConfig { theme: ThemeChoice::Light, keymap: Preset::Bambu, ui_scale: 1.1, ..Default::default() };
        c.sections.insert("print.process".into(), false);
        let text = serde_json::to_string(&c).unwrap();
        let back: UiConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(back, c);
        // missing fields fall back to defaults; junk scale is repaired
        let mut partial: UiConfig = serde_json::from_str(r#"{"theme":"dark","ui_scale":9}"#).unwrap();
        partial.sanitize();
        assert_eq!(partial.theme, ThemeChoice::Dark);
        assert_eq!(partial.keymap, Preset::Only3d);
        assert_eq!(partial.ui_scale, 1.0);
        assert_eq!(partial.layout, Layout::default());
    }

    #[test]
    fn gizmo_prefs_default_and_repair() {
        let c = UiConfig::default();
        assert_eq!((c.gizmo.style.as_str(), c.gizmo.smart_guides, c.gizmo.move_snap_mm, c.gizmo.rotate_snap_deg), ("triad", true, 1.0, 15.0));
        let mut bad: UiConfig = serde_json::from_str(r#"{"gizmo":{"style":"weird","move_snap_mm":-3,"rotate_snap_deg":400}}"#).unwrap();
        bad.sanitize();
        assert_eq!((bad.gizmo.style.as_str(), bad.gizmo.move_snap_mm, bad.gizmo.rotate_snap_deg), ("triad", 1.0, 15.0));
        let older: UiConfig = serde_json::from_str(r#"{"theme":"dark"}"#).unwrap();
        assert!(older.gizmo.smart_guides, "files from before keep the defaults");
    }
}
