//! only3d desktop GUI (egui + wgpu).
//!
//! The GUI is a client of the tool registry exactly like an AI agent: every edit is
//! `studio.call("<tool>", args, Actor::User)` (run on a background worker so the window never
//! waits on the session), while rendering and lists read the session state directly. With
//! [`GuiOptions::start_live_server`] the same session is served on the live socket, so an agent's
//! edits appear in the viewport and the activity feed as they happen, and the agent can see what
//! the user selected.
//!
//! Layout: a compact title bar, three docks (left / right / bottom) of tabbed, splittable panes
//! — drag a tab to another pane, dock edge or split zone, right-click it to move or close it,
//! click the active tab to collapse its dock (⌘B / ⌥⌘B / ⌘J), ⇧⌘↩ for zen mode — the 3D viewport,
//! and a status bar. The command palette (⇧⌘P / ⌘K) runs every UI action and every studio tool.
//! Theme, keymap (only3d or Bambu Studio), UI scale and layout persist in
//! `~/.only3d/ui.json`; key overrides in `~/.only3d/keymap.json` (see [`keymap`]).
//!
//! Developer knobs (screenshot self-test and performance checks): `ONLY3D_GUI_VIEW=toolpaths`
//! starts in the toolpath preview, `ONLY3D_GUI_LAYER=n` starts the preview at layer n,
//! `ONLY3D_GUI_THEME=dark|light|night|system`, `ONLY3D_GUI_KEYMAP=only3d|bambu`,
//! `ONLY3D_GUI_LAYOUT=settings:split-right,activity:hidden,bottom:collapsed` (see
//! [`dock::Layout::apply_script`]), `ONLY3D_GUI_OPEN=palette|shortcuts|agent|welcome_agent|preferences|close_prompt|rotate|
//! move|overhangs|spool_notice|timelapses|<panel>` (comma-separated) with `ONLY3D_GUI_PALETTE_QUERY=…` and
//! `ONLY3D_GUI_SETTINGS_QUERY=…`, `ONLY3D_GUI_KEYS=cmd-a,r` presses chords through the
//! real keyboard path once the session is loaded, `ONLY3D_GUI_THEN_CLICK=x,y;x,y` clicks after them, `ONLY3D_GUI_DRAG=x0,y0,x1,y1` drags (`_DRAG_HOLD=1` keeps the button down, `_DRAG_MODS=alt,shift,cmd` holds keys), `ONLY3D_GUI_RCLICK=x,y` right-clicks, `ONLY3D_GUI_HOVER=x,y` rests the pointer there, `ONLY3D_GUI_PREVIEW=<file>` opens a file in the preview tab, `ONLY3D_GUI_CAMERA=x,y` shows a camera position, `ONLY3D_GUI_STATS=1` shows triangle/segment counts and
//! frame times, `ONLY3D_GUI_SHOT_DELAY_MS` / `ONLY3D_GUI_SHOT_FRAMES` delay the
//! screenshot and `ONLY3D_GUI_SPIN=1` orbits the camera while waiting. `ONLY3D_GUI_MENU_LOG=1`
//! prints every menu-bar pick; `ONLY3D_GUI_MENU_PLAIN=1` gives plain keys (M, F) menu key
//! equivalents too (see `menubar.rs`). Screenshot runs never
//! read or write `ui.json`.

mod actions;
mod adapter;
mod approvals;
mod arrange_ops;
mod app;
mod app_menus;
mod os;
mod camera;
mod chrome;
mod config;
mod context_menu;
mod design_ui;
pub mod dock;
mod gpu;
pub mod keymap;
// the native menu bar is macOS-only; elsewhere only its shared spec (context menus) is used
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod menubar;
mod palette;
mod editor;
mod preview;
mod save_panel;
mod panels;
mod workspace_panel;
mod printer_live;
mod scene;
mod settings_ui;
mod theme;
mod timelapses;
mod update;
mod welcome;
mod worker;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use eframe::egui;
use only3d_studio::Studio;

/// How to open the window.
#[derive(Clone, Debug, Default)]
pub struct GuiOptions {
    /// Window title (defaults to the project file name).
    pub project_title: Option<String>,
    /// Self-test: render a few frames, save a PNG of the window here, then close.
    pub screenshot: Option<PathBuf>,
    /// Serve this session live and list it (`only3d_studio::live::host`) so agents can find and
    /// drive it.
    pub start_live_server: bool,
    /// With `screenshot`: open the print dialog on this preflight answer (`print`'s data without
    /// confirm), to picture it without a printer. Start never sends from it.
    pub print_preview: Option<serde_json::Value>,
}

/// The graphics adapters this machine offers, with the one the window draws with marked
/// (`only3d gpu`).
pub fn gpu_report() -> String {
    adapter::report()
}

/// `only3d gpu --bench`: frame times for a 1M-triangle mesh and 2M toolpath segments on that
/// adapter.
pub fn gpu_bench() -> String {
    adapter::bench_report()
}

/// Open the only3d window on a (possibly shared) session. Blocks until the window closes.
pub fn run(studio: Arc<Mutex<Studio>>, opts: GuiOptions) -> anyhow::Result<()> {
    let title = opts.project_title.clone().map(|t| format!("{t} — only3d")).unwrap_or_else(|| "only3d".into());
    let mut viewport = egui::ViewportBuilder::default().with_title(title).with_inner_size([1480.0, 920.0]).with_min_inner_size([960.0, 620.0]).with_drag_and_drop(true);
    // the taskbar / dock icon (the packaged apps carry their own too)
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../../../packaging/icons/only3d.png")) {
        viewport = viewport.with_icon(icon);
    }
    if cfg!(target_os = "macos") {
        // unified title bar: our own bar sits under the traffic lights
        viewport = viewport.with_fullsize_content_view(true).with_title_shown(false).with_titlebar_shown(false);
    }
    // our own adapter choice: a real GPU first, software last (see adapter.rs)
    let mut wgpu_options = egui_wgpu::WgpuConfiguration::default();
    if let egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
        setup.native_adapter_selector = Some(adapter::selector());
    }
    let native = eframe::NativeOptions { viewport, renderer: eframe::Renderer::Wgpu, multisampling: 0, depth_buffer: 0, wgpu_options, ..Default::default() };
    eframe::run_native("only3d", native, Box::new(move |cc| Ok(Box::new(app::App::new(cc, studio, opts)?) as Box<dyn eframe::App>))).map_err(|e| anyhow::anyhow!("GUI failed: {e}"))
}
