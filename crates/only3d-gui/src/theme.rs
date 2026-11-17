//! The look: Dark, Light and Night palettes, flat surfaces, 1 px
//! borders, 4 px radii, dense 13 px text. The palette is a process-wide value read with [`pal`],
//! swapped by [`apply`] when the user changes the theme (or the OS flips dark/light mode).

use std::sync::RwLock;

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Margin, Pos2, Rect, Stroke, TextStyle, Vec2};
use serde::{Deserialize, Serialize};

/// Which theme the user picked. `System` follows the OS dark/light mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    #[default]
    System,
    Dark,
    Light,
    Night,
}

impl ThemeChoice {
    pub const ALL: [ThemeChoice; 4] = [ThemeChoice::System, ThemeChoice::Dark, ThemeChoice::Light, ThemeChoice::Night];
    pub fn label(self) -> &'static str {
        match self {
            ThemeChoice::System => "System (follow macOS)",
            ThemeChoice::Dark => "Dark",
            ThemeChoice::Light => "Light",
            ThemeChoice::Night => "Night",
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            ThemeChoice::System => "system",
            ThemeChoice::Dark => "dark",
            ThemeChoice::Light => "light",
            ThemeChoice::Night => "night",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().replace(['-', ' '], "_").as_str() {
            "system" | "auto" => ThemeChoice::System,
            "dark" => ThemeChoice::Dark,
            "light" => ThemeChoice::Light,
            "night" => ThemeChoice::Night,
            _ => return None,
        })
    }
    /// The concrete palette, given whether the OS is in dark mode.
    pub fn palette(self, os_dark: bool) -> Palette {
        match self {
            ThemeChoice::System => {
                if os_dark {
                    DARK
                } else {
                    LIGHT
                }
            }
            ThemeChoice::Dark => DARK,
            ThemeChoice::Light => LIGHT,
            ThemeChoice::Night => NIGHT,
        }
    }
}

/// Every colour the UI and the viewport use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub name: &'static str,
    pub dark: bool,
    /// Editor / viewport / text-input background.
    pub bg: Color32,
    /// Panels.
    pub surface: Color32,
    /// Popups, menus, modals.
    pub elevated: Color32,
    /// Title bar and status bar.
    pub bar: Color32,
    /// Buttons and other filled elements.
    pub element: Color32,
    pub border: Color32,
    pub border_variant: Color32,
    pub border_focused: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub placeholder: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    pub hover: Color32,
    pub active: Color32,
    pub selected: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub error: Color32,
    // logo colours
    pub coral: Color32,
    pub blue: Color32,
    pub amber: Color32,
    // 3D scene, sRGB
    pub view_top: [u8; 3],
    pub view_bottom: [u8; 3],
    pub dots: [u8; 3],
    pub plate_top: [u8; 3],
    pub plate_body: [u8; 3],
    pub plate_grid: [u8; 3],
    pub plate_grid_major: [u8; 3],
    pub plate_border: [u8; 3],
    pub plate_strip: [u8; 3],
    pub plate_hatch: [u8; 3],
    pub plate_strip_edge: [u8; 3],
    pub accent_3d: [u8; 3],
    pub outline_3d: [u8; 3],
    pub overhang_3d: [u8; 3],
}

const fn rgb(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}
const fn rgb3(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

pub const DARK: Palette = Palette {
    name: "Dark",
    dark: true,
    bg: rgb(0x282c33),
    surface: rgb(0x2f343e),
    elevated: rgb(0x2f343e),
    bar: rgb(0x3b414d),
    element: rgb(0x363c46),
    border: rgb(0x464b57),
    border_variant: rgb(0x363c46),
    border_focused: rgb(0x47679e),
    text: rgb(0xdce0e5),
    muted: rgb(0xa9afbc),
    placeholder: rgb(0x878a98),
    accent: rgb(0x74ade8),
    on_accent: rgb(0x1b1f25),
    hover: rgb(0x363c46),
    active: rgb(0x454a56),
    selected: rgb(0x454a56),
    success: rgb(0xa1c181),
    warning: rgb(0xdec184),
    error: rgb(0xd07277),
    coral: rgb(0xf09082),
    blue: rgb(0x4d9abf),
    amber: rgb(0xf1be58),
    view_top: rgb3(0x2b3038),
    view_bottom: rgb3(0x22262c),
    dots: rgb3(0x3a404b),
    plate_top: rgb3(0x343944),
    plate_body: rgb3(0x1d2026),
    plate_grid: rgb3(0x40464f),
    plate_grid_major: rgb3(0x535a66),
    plate_border: rgb3(0x7b8494),
    plate_strip: rgb3(0x3d3a3a),
    plate_hatch: rgb3(0x5e5248),
    plate_strip_edge: rgb3(0xb39a6a),
    accent_3d: rgb3(0x74ade8),
    outline_3d: rgb3(0x8cc0f5),
    overhang_3d: rgb3(0xe0626b),
};

pub const LIGHT: Palette = Palette {
    name: "Light",
    dark: false,
    bg: rgb(0xfafafa),
    surface: rgb(0xebebec),
    elevated: rgb(0xf3f3f4),
    bar: rgb(0xdcdcdd),
    element: rgb(0xe2e2e4),
    border: rgb(0xc9c9ca),
    border_variant: rgb(0xdfdfe0),
    border_focused: rgb(0x7d82e8),
    text: rgb(0x242529),
    muted: rgb(0x58585a),
    placeholder: rgb(0x7e8086),
    accent: rgb(0x5c78e2),
    on_accent: rgb(0xfafafa),
    hover: rgb(0xdfdfe0),
    active: rgb(0xcacaca),
    selected: rgb(0xd5d6dc),
    success: rgb(0x669f59),
    warning: rgb(0xa48819),
    error: rgb(0xd36151),
    coral: rgb(0xe0705f),
    blue: rgb(0x3c86ad),
    amber: rgb(0xd9a32e),
    view_top: rgb3(0xfafafa),
    view_bottom: rgb3(0xe6e6e8),
    dots: rgb3(0xd2d2d6),
    plate_top: rgb3(0x4a4e57),
    plate_body: rgb3(0x2f3238),
    plate_grid: rgb3(0x575c66),
    plate_grid_major: rgb3(0x6b717d),
    plate_border: rgb3(0x8f96a3),
    plate_strip: rgb3(0x544f4b),
    plate_hatch: rgb3(0x776a5c),
    plate_strip_edge: rgb3(0xc9a253),
    accent_3d: rgb3(0x5c78e2),
    outline_3d: rgb3(0x6f8cff),
    overhang_3d: rgb3(0xd94a3d),
};

pub const NIGHT: Palette = Palette {
    name: "Night",
    dark: true,
    bg: rgb(0x161d27),
    surface: rgb(0x1b2330),
    elevated: rgb(0x1f2836),
    bar: rgb(0x121820),
    element: rgb(0x232d3b),
    border: rgb(0x2c3747),
    border_variant: rgb(0x222b38),
    border_focused: rgb(0x3f6a91),
    text: rgb(0xd5d8db),
    muted: rgb(0x8e98a5),
    placeholder: rgb(0x6b7684),
    accent: rgb(0x6a9fcc),
    on_accent: rgb(0x10161e),
    hover: rgb(0x232d3b),
    active: rgb(0x2c3a4c),
    selected: rgb(0x2a3749),
    success: rgb(0x8fc28a),
    warning: rgb(0xf1be58),
    error: rgb(0xf09082),
    coral: rgb(0xf09082),
    blue: rgb(0x4d9abf),
    amber: rgb(0xf1be58),
    view_top: rgb3(0x182130),
    view_bottom: rgb3(0x121822),
    dots: rgb3(0x263142),
    plate_top: rgb3(0x232c3a),
    plate_body: rgb3(0x0e131a),
    plate_grid: rgb3(0x2e3949),
    plate_grid_major: rgb3(0x3d4b5e),
    plate_border: rgb3(0x6a7f97),
    plate_strip: rgb3(0x2d2a2f),
    plate_hatch: rgb3(0x574a45),
    plate_strip_edge: rgb3(0xf09082),
    accent_3d: rgb3(0x6a9fcc),
    outline_3d: rgb3(0x86b9e6),
    overhang_3d: rgb3(0xf07a6a),
};

static PALETTE: RwLock<Palette> = RwLock::new(DARK);

/// The active palette.
pub fn pal() -> Palette {
    PALETTE.read().map(|p| *p).unwrap_or(DARK)
}

/// Named font family for medium-weight text (titles, active tabs).
pub fn medium() -> FontFamily {
    FontFamily::Name("medium".into())
}

/// Load the macOS system UI fonts when present (SF Pro, SF Mono / Menlo); otherwise keep egui's
/// bundled fonts. No network, no bundled font files.
pub fn install_fonts(ctx: &egui::Context) {
    let mut defs = egui::FontDefinitions::default();
    let read = |p: &str| std::fs::read(p).ok().filter(|b| b.len() > 1024);
    let ui = read("/System/Library/Fonts/SFNS.ttf");
    let mono = read("/System/Library/Fonts/SFNSMono.ttf").map(|b| (b, 0u32)).or_else(|| read("/System/Library/Fonts/Menlo.ttc").map(|b| (b, 0)));
    let fallback_prop: Vec<String> = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let mut medium_list = Vec::new();
    if let Some(bytes) = ui {
        let blob: egui::epaint::text::Blob = std::sync::Arc::new(bytes);
        let regular = egui::FontData::from_blob(blob.clone(), 0).tweak(egui::FontTweak { coords: egui::epaint::text::VariationCoords::new([(b"wght", 420.0)]), ..Default::default() });
        let med = egui::FontData::from_blob(blob, 0).tweak(egui::FontTweak { coords: egui::epaint::text::VariationCoords::new([(b"wght", 590.0)]), ..Default::default() });
        defs.font_data.insert("sf-ui".into(), regular.into());
        defs.font_data.insert("sf-ui-medium".into(), med.into());
        if let Some(f) = defs.families.get_mut(&FontFamily::Proportional) {
            f.insert(0, "sf-ui".into());
        }
        medium_list.push("sf-ui-medium".to_string());
    }
    medium_list.extend(fallback_prop);
    defs.families.insert(medium(), medium_list);
    if let Some((bytes, index)) = mono {
        let blob: egui::epaint::text::Blob = std::sync::Arc::new(bytes);
        defs.font_data.insert("sf-mono".into(), egui::FontData::from_blob(blob, index).into());
        if let Some(f) = defs.families.get_mut(&FontFamily::Monospace) {
            f.insert(0, "sf-mono".into());
        }
    }
    ctx.set_fonts(defs);
}

/// Install `p` as the active palette and restyle egui for it.
pub fn apply(ctx: &egui::Context, p: Palette) {
    if let Ok(mut g) = PALETTE.write() {
        *g = p;
    }
    let base = if p.dark { egui::Theme::Dark } else { egui::Theme::Light };
    let mut style = (*ctx.style_of(base)).clone();
    style.text_styles = [
        (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(13.0, FontFamily::Proportional)),
        (TextStyle::Heading, FontId::new(15.0, medium())),
        (TextStyle::Monospace, FontId::new(12.0, FontFamily::Monospace)),
    ]
    .into();
    let s = &mut style.spacing;
    s.item_spacing = Vec2::new(6.0, 5.0);
    s.button_padding = Vec2::new(8.0, 2.0);
    s.interact_size = Vec2::new(28.0, 22.0);
    s.window_margin = Margin::same(12);
    s.menu_margin = Margin::same(4);
    s.menu_spacing = 2.0;
    s.slider_width = 120.0;
    s.combo_width = 140.0;
    s.indent = 12.0;
    s.icon_width = 13.0;
    s.icon_width_inner = 7.0;
    s.scroll = egui::style::ScrollStyle::thin();
    style.interaction.selectable_labels = false;

    let v = &mut style.visuals;
    *v = if p.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.override_text_color = Some(p.text);
    v.weak_text_color = Some(p.muted);
    v.panel_fill = p.surface;
    v.window_fill = p.elevated;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(6);
    v.menu_corner_radius = CornerRadius::same(6);
    let shadow_a = if p.dark { 90 } else { 28 };
    v.window_shadow = egui::Shadow { offset: [0, 6], blur: 22, spread: 0, color: Color32::from_black_alpha(shadow_a) };
    v.popup_shadow = egui::Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha(shadow_a) };
    v.faint_bg_color = p.hover;
    v.extreme_bg_color = p.bg;
    v.text_edit_bg_color = Some(p.bg);
    v.code_bg_color = p.bg;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.error;
    v.selection.bg_fill = p.accent.gamma_multiply(if p.dark { 0.35 } else { 0.30 });
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.text_cursor.stroke = Stroke::new(1.5, p.accent);
    v.slider_trailing_fill = true;
    v.striped = false;
    v.indent_has_left_vline = false;
    v.collapsing_header_frame = false;
    v.button_frame = true;
    v.handle_shape = egui::style::HandleShape::Rect { aspect_ratio: 0.55 };

    let r = CornerRadius::same(4);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border_variant);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r;
    w.inactive.bg_fill = p.element;
    w.inactive.weak_bg_fill = p.element;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border_variant);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = r;
    w.inactive.expansion = 0.0;
    w.hovered.bg_fill = p.active;
    w.hovered.weak_bg_fill = p.active;
    w.hovered.bg_stroke = Stroke::new(1.0, p.border);
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;
    w.active.bg_fill = p.selected;
    w.active.weak_bg_fill = p.selected;
    w.active.bg_stroke = Stroke::new(1.0, p.border_focused);
    w.active.fg_stroke = Stroke::new(1.0, p.text);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;
    w.open.bg_fill = p.selected;
    w.open.weak_bg_fill = p.selected;
    w.open.bg_stroke = Stroke::new(1.0, p.border);
    w.open.fg_stroke = Stroke::new(1.0, p.text);
    w.open.corner_radius = r;
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
    ctx.set_theme(base);
}

// ---------------------------------------------------------------------------------------------
// small widgets

/// Section label: monospace, uppercase, letter-spaced, muted.
pub fn label_job(text: &str, color: Color32, size: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.append(&text.to_uppercase(), 0.0, egui::TextFormat { font_id: FontId::monospace(size), color, extra_letter_spacing: 1.1, ..Default::default() });
    job
}

pub fn section_label(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.label(label_job(text, pal().muted, 10.5))
}

/// A bracketed text button, used sparingly for secondary actions: `[ EXPORT ]`.
pub fn bracket_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let p = pal();
    let galley = ui.painter().layout_job(label_job(&format!("[ {text} ]"), p.muted, 10.5));
    let (r, resp) = ui.allocate_exact_size(galley.size() + egui::vec2(4.0, 6.0), egui::Sense::click());
    let col = if resp.hovered() { p.accent } else { p.muted };
    let galley = ui.painter().layout_job(label_job(&format!("[ {text} ]"), col, 10.5));
    ui.painter().galley(r.min + egui::vec2(2.0, 3.0), galley, col);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// An accent-filled button.
pub fn primary_button(text: impl Into<String>) -> egui::Button<'static> {
    let p = pal();
    egui::Button::new(egui::RichText::new(text.into()).color(p.on_accent).family(medium())).fill(p.accent).stroke(Stroke::NONE).corner_radius(CornerRadius::same(4)).min_size(Vec2::new(0.0, 24.0))
}

/// A borderless button that only shows a background on hover (a "ghost" button).
pub fn ghost_button(text: impl Into<egui::WidgetText>) -> egui::Button<'static> {
    egui::Button::new(text).fill(Color32::TRANSPARENT).stroke(Stroke::NONE)
}

pub fn chip(ui: &mut egui::Ui, text: &str, fg: Color32, bg: Color32) -> egui::Response {
    egui::Frame::new().fill(bg).corner_radius(CornerRadius::same(4)).inner_margin(Margin::symmetric(6, 1)).show(ui, |ui| ui.label(egui::RichText::new(text).size(11.0).color(fg))).response
}

/// A soft background tint of a status colour.
pub fn soft(c: Color32) -> Color32 {
    let p = pal();
    blend(p.surface, c, if p.dark { 0.16 } else { 0.14 })
}

pub fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// A status-bar chip: a coloured dot and text, no background unless hovered.
pub fn dot_chip(ui: &mut egui::Ui, text: &str, dot: Color32) -> egui::Response {
    let p = pal();
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(11.5), p.muted);
    let size = egui::vec2(galley.size().x + 22.0, 20.0);
    let (r, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let painter = ui.painter();
    if resp.hovered() {
        painter.rect_filled(r, 4.0, p.hover);
    }
    painter.circle_filled(egui::pos2(r.min.x + 9.0, r.center().y), 3.0, dot);
    painter.galley(egui::pos2(r.min.x + 17.0, r.center().y - galley.size().y * 0.5), galley, p.muted);
    resp
}

/// A key-cap rendering of a shortcut ("⌘⇧P").
pub fn kbd(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let p = pal();
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(11.0), p.muted);
    let (r, resp) = ui.allocate_exact_size(galley.size() + egui::vec2(10.0, 4.0), egui::Sense::hover());
    ui.painter().rect(r, 3.0, p.bg, Stroke::new(1.0, p.border_variant), egui::StrokeKind::Inside);
    ui.painter().galley(r.min + egui::vec2(5.0, 2.0), galley, p.muted);
    resp
}

/// The only3d mark: a tiny pixel block in coral / blue / amber.
pub fn pixel_logo(ui: &mut egui::Ui, cell: f32) -> egui::Response {
    let p = pal();
    let (r, resp) = ui.allocate_exact_size(egui::vec2(cell * 3.0 + 2.0, cell * 3.0 + 2.0), egui::Sense::hover());
    // a stylised "o" with a corner pixel: coral / blue / amber
    let cells: [(usize, usize, Color32); 7] = [(0, 0, p.coral), (1, 0, p.coral), (2, 0, p.blue), (0, 1, p.coral), (2, 1, p.blue), (0, 2, p.amber), (1, 2, p.amber)];
    for (x, y, c) in cells {
        let min = r.min + egui::vec2(1.0 + x as f32 * cell, 1.0 + y as f32 * cell);
        ui.painter().rect_filled(Rect::from_min_size(min, egui::vec2(cell - 1.0, cell - 1.0)), 0.0, c);
    }
    resp
}

// ---------------------------------------------------------------------------------------------
// line icons (painted, no icon font)

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    DockLeft,
    DockRight,
    DockBottom,
    Zen,
    Sun,
    Moon,
    Search,
    Gear,
    Plus,
    Eye,
    EyeOff,
    Copy,
    Trash,
    Close,
    ChevronRight,
    ChevronDown,
    Reset,
    Cube,
    Hash,
    Info,
    Printer,
    Spool,
    Check,
    Activity,
    Sliders,
    Layers,
    Command,
    Keyboard,
    More,
    Move,
    Rotate,
    Scale,
    Pointer,
    Link,
    Undo,
    Redo,
    Import,
    Grid,
    LayFlat,
    Overhang,
    Lock,
    Folder,
    File,
    /// A file with lines of text.
    FileText,
    /// A file with a picture (mountain and sun).
    FileImage,
    /// A file with code brackets.
    FileCode,
    Camera,
    PushPull,
}

pub fn paint_icon(painter: &egui::Painter, r: Rect, icon: Icon, col: Color32) {
    let st = Stroke::new(1.25, col);
    let c = r.center();
    let s = r.width().min(r.height()) / 16.0;
    let pt = |x: f32, y: f32| Pos2::new(c.x + x * s, c.y + y * s);
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(pt(x0, y0), pt(x1, y1));
    let line = |pts: &[(f32, f32)]| egui::Shape::line(pts.iter().map(|(x, y)| pt(*x, *y)).collect(), st);
    match icon {
        Icon::DockLeft | Icon::DockRight | Icon::DockBottom => {
            painter.rect_stroke(rect(-6.0, -5.0, 6.0, 5.0), 1.5, st, egui::StrokeKind::Middle);
            let fill = match icon {
                Icon::DockLeft => rect(-6.0, -5.0, -2.0, 5.0),
                Icon::DockRight => rect(2.0, -5.0, 6.0, 5.0),
                _ => rect(-6.0, 1.5, 6.0, 5.0),
            };
            painter.rect_filled(fill, 1.0, col.gamma_multiply(0.7));
        }
        Icon::Zen => {
            for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                painter.add(line(&[(sx * 6.0, sy * 2.5), (sx * 6.0, sy * 6.0), (sx * 2.5, sy * 6.0)]));
            }
        }
        Icon::Sun => {
            painter.circle_stroke(c, 3.0 * s, st);
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                painter.line_segment([pt(a.cos() * 5.0, a.sin() * 5.0), pt(a.cos() * 6.6, a.sin() * 6.6)], st);
            }
        }
        Icon::Moon => {
            let mut pts = Vec::new();
            for k in 0..=24 {
                let a = 0.9 + k as f32 / 24.0 * 4.5;
                pts.push((a.cos() * 5.8, a.sin() * 5.8));
            }
            for k in 0..=24 {
                let a = 0.9 + 4.5 - k as f32 / 24.0 * 4.5;
                pts.push((2.2 + a.cos() * 4.4, -1.6 + a.sin() * 4.4));
            }
            painter.add(line(&pts));
        }
        Icon::Search => {
            painter.circle_stroke(pt(-1.0, -1.0), 4.2 * s, st);
            painter.line_segment([pt(2.2, 2.2), pt(5.8, 5.8)], st);
        }
        Icon::Gear => {
            painter.circle_stroke(c, 2.2 * s, st);
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                painter.line_segment([pt(a.cos() * 4.2, a.sin() * 4.2), pt(a.cos() * 6.4, a.sin() * 6.4)], Stroke::new(2.0, col));
            }
            painter.circle_stroke(c, 4.6 * s, st);
        }
        Icon::Plus => {
            painter.line_segment([pt(-5.0, 0.0), pt(5.0, 0.0)], st);
            painter.line_segment([pt(0.0, -5.0), pt(0.0, 5.0)], st);
        }
        Icon::Eye | Icon::EyeOff => {
            let mut top = Vec::new();
            let mut bot = Vec::new();
            for i in 0..=16 {
                let x = -6.5 + 13.0 * i as f32 / 16.0;
                let h = 4.2 * (1.0 - (x / 6.5).powi(2));
                top.push((x, -h));
                bot.push((x, h));
            }
            painter.add(line(&top));
            painter.add(line(&bot));
            painter.circle_stroke(c, 1.8 * s, st);
            if icon == Icon::EyeOff {
                painter.line_segment([pt(-5.5, 5.5), pt(5.5, -5.5)], st);
            }
        }
        Icon::Copy => {
            painter.rect_stroke(rect(-5.0, -2.5, 2.5, 5.0), 1.5, st, egui::StrokeKind::Middle);
            painter.add(line(&[(-2.5, -4.0), (-2.5, -5.0), (5.0, -5.0), (5.0, 2.5), (4.0, 2.5)]));
        }
        Icon::Trash => {
            painter.line_segment([pt(-5.5, -3.5), pt(5.5, -3.5)], st);
            painter.add(line(&[(-2.0, -3.5), (-2.0, -5.5), (2.0, -5.5), (2.0, -3.5)]));
            painter.add(line(&[(-4.2, -3.5), (-3.6, 5.5), (3.6, 5.5), (4.2, -3.5)]));
        }
        Icon::Close => {
            painter.line_segment([pt(-4.0, -4.0), pt(4.0, 4.0)], st);
            painter.line_segment([pt(-4.0, 4.0), pt(4.0, -4.0)], st);
        }
        Icon::ChevronRight => {
            painter.add(line(&[(-1.5, -4.0), (2.5, 0.0), (-1.5, 4.0)]));
        }
        Icon::ChevronDown => {
            painter.add(line(&[(-4.0, -1.5), (0.0, 2.5), (4.0, -1.5)]));
        }
        Icon::Reset => {
            let mut pts = Vec::new();
            for k in 0..=20 {
                let a = -2.6 + k as f32 / 20.0 * 4.9;
                pts.push((a.cos() * 4.8, a.sin() * 4.8));
            }
            painter.add(line(&pts));
            let a = -2.6f32;
            let (x, y) = (a.cos() * 4.8, a.sin() * 4.8);
            painter.add(line(&[(x - 0.5, y - 3.0), (x, y), (x + 3.0, y + 0.3)]));
        }
        Icon::Camera => {
            painter.rect_stroke(rect(-6.5, -3.5, 4.0, 4.5), 1.5, st, egui::StrokeKind::Middle);
            painter.add(egui::Shape::closed_line(vec![pt(4.0, -1.0), pt(7.0, -3.0), pt(7.0, 4.0), pt(4.0, 2.0)], st));
        }
        Icon::Lock => {
            painter.rect_stroke(rect(-4.5, -0.5, 4.5, 6.0), 1.0, st, egui::StrokeKind::Middle);
            painter.add(line(&[(-3.0, -0.5), (-3.0, -3.0), (-2.0, -5.0), (0.0, -5.8), (2.0, -5.0), (3.0, -3.0), (3.0, -0.5)]));
        }
        Icon::Folder => {
            painter.add(egui::Shape::closed_line(vec![pt(-6.5, -4.5), pt(-1.5, -4.5), pt(0.0, -3.0), pt(6.5, -3.0), pt(6.5, 5.0), pt(-6.5, 5.0)], st));
        }
        Icon::File => {
            painter.add(egui::Shape::closed_line(vec![pt(-4.5, -6.0), pt(2.0, -6.0), pt(4.5, -3.5), pt(4.5, 6.0), pt(-4.5, 6.0)], st));
            painter.add(line(&[(2.0, -6.0), (2.0, -3.5), (4.5, -3.5)]));
        }
        Icon::FileText | Icon::FileImage | Icon::FileCode => {
            painter.add(egui::Shape::closed_line(vec![pt(-4.5, -6.0), pt(2.0, -6.0), pt(4.5, -3.5), pt(4.5, 6.0), pt(-4.5, 6.0)], st));
            painter.add(line(&[(2.0, -6.0), (2.0, -3.5), (4.5, -3.5)]));
            match icon {
                Icon::FileText => {
                    for y in [-1.0, 1.5, 4.0] {
                        painter.line_segment([pt(-2.5, y), pt(2.5, y)], st);
                    }
                }
                Icon::FileImage => {
                    painter.add(line(&[(-3.0, 4.0), (-0.8, 1.0), (0.8, 2.8), (1.8, 1.8), (3.0, 4.0)]));
                    painter.circle_stroke(pt(-1.5, -2.0), 1.1 * s, st);
                }
                _ => {
                    painter.add(line(&[(-1.0, -1.0), (-2.8, 1.2), (-1.0, 3.4)]));
                    painter.add(line(&[(1.0, -1.0), (2.8, 1.2), (1.0, 3.4)]));
                }
            }
        }
        Icon::Cube => {
            painter.add(egui::Shape::closed_line(vec![pt(0.0, -6.0), pt(5.5, -3.0), pt(5.5, 3.5), pt(0.0, 6.5), pt(-5.5, 3.5), pt(-5.5, -3.0)], st));
            painter.add(line(&[(-5.5, -3.0), (0.0, 0.0), (5.5, -3.0)]));
            painter.line_segment([pt(0.0, 0.0), pt(0.0, 6.5)], st);
        }
        Icon::Hash => {
            painter.line_segment([pt(-2.0, -6.0), pt(-3.0, 6.0)], st);
            painter.line_segment([pt(3.0, -6.0), pt(2.0, 6.0)], st);
            painter.line_segment([pt(-5.5, -2.0), pt(5.5, -2.0)], st);
            painter.line_segment([pt(-6.0, 2.5), pt(5.0, 2.5)], st);
        }
        Icon::Info => {
            painter.circle_stroke(c, 6.0 * s, st);
            painter.line_segment([pt(0.0, -0.5), pt(0.0, 3.5)], st);
            painter.circle_filled(pt(0.0, -3.0), 0.9 * s, col);
        }
        Icon::Printer => {
            painter.rect_stroke(rect(-6.0, -6.0, 6.0, 6.0), 1.5, st, egui::StrokeKind::Middle);
            painter.line_segment([pt(-6.0, -2.5), pt(6.0, -2.5)], st);
            painter.add(line(&[(-1.5, -2.5), (0.0, -0.5), (1.5, -2.5)]));
            painter.line_segment([pt(-3.5, 4.0), pt(3.5, 4.0)], st);
        }
        Icon::Spool => {
            painter.circle_stroke(c, 6.0 * s, st);
            painter.circle_stroke(c, 2.0 * s, st);
            painter.circle_stroke(c, 4.0 * s, Stroke::new(1.0, col.gamma_multiply(0.6)));
        }
        Icon::Check => {
            painter.add(line(&[(-5.0, 0.5), (-1.5, 4.0), (5.0, -3.5)]));
        }
        Icon::Activity => {
            painter.add(line(&[(-6.5, 0.0), (-3.5, 0.0), (-1.5, -5.0), (1.5, 5.0), (3.5, 0.0), (6.5, 0.0)]));
        }
        Icon::Sliders => {
            for (y, x) in [(-4.0, 2.0), (0.0, -2.5), (4.0, 1.0)] {
                painter.line_segment([pt(-6.0, y), pt(6.0, y)], st);
                painter.circle_filled(pt(x, y), 1.7 * s, col);
            }
        }
        Icon::Layers => {
            painter.add(egui::Shape::closed_line(vec![pt(0.0, -6.0), pt(6.0, -3.0), pt(0.0, 0.0), pt(-6.0, -3.0)], st));
            painter.add(line(&[(-6.0, 0.5), (0.0, 3.5), (6.0, 0.5)]));
            painter.add(line(&[(-6.0, 3.5), (0.0, 6.5), (6.0, 3.5)]));
        }
        Icon::Command => {
            painter.rect_stroke(rect(-2.0, -2.0, 2.0, 2.0), 0.0, st, egui::StrokeKind::Middle);
            for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                painter.circle_stroke(pt(sx * 4.0, sy * 4.0), 2.0 * s, st);
            }
        }
        Icon::Keyboard => {
            painter.rect_stroke(rect(-7.0, -4.5, 7.0, 4.5), 1.5, st, egui::StrokeKind::Middle);
            for x in [-4.0, -1.3, 1.3, 4.0] {
                painter.circle_filled(pt(x, -1.5), 0.7 * s, col);
            }
            painter.line_segment([pt(-3.5, 2.0), pt(3.5, 2.0)], st);
        }
        Icon::More => {
            for x in [-4.5, 0.0, 4.5] {
                painter.circle_filled(pt(x, 0.0), 1.1 * s, col);
            }
        }
        Icon::Move => {
            painter.line_segment([pt(-6.0, 0.0), pt(6.0, 0.0)], st);
            painter.line_segment([pt(0.0, -6.0), pt(0.0, 6.0)], st);
            for (dx, dy) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
                let (tx, ty) = (dx * 6.0, dy * 6.0);
                painter.add(line(&[(tx - dx * 2.0 - dy * 2.0, ty - dy * 2.0 - dx * 2.0), (tx, ty), (tx - dx * 2.0 + dy * 2.0, ty - dy * 2.0 + dx * 2.0)]));
            }
        }
        Icon::Rotate => {
            let mut pts = Vec::new();
            for k in 0..=20 {
                let a = -0.4 + k as f32 / 20.0 * 5.2;
                pts.push((a.cos() * 5.2, a.sin() * 5.2));
            }
            painter.add(line(&pts));
            let a = 4.8f32;
            let (x, y) = (a.cos() * 5.2, a.sin() * 5.2);
            painter.add(line(&[(x - 2.5, y - 1.0), (x, y), (x + 0.5, y + 2.6)]));
        }
        Icon::Scale => {
            painter.rect_stroke(rect(-6.0, -1.0, 1.0, 6.0), 1.0, st, egui::StrokeKind::Middle);
            painter.add(line(&[(-1.0, 1.0), (6.0, -6.0)]));
            painter.add(line(&[(1.5, -6.0), (6.0, -6.0), (6.0, -1.5)]));
        }
        Icon::PushPull => {
            // a box seen from above the front-right corner, its top face lit and pulled up
            painter.add(egui::Shape::convex_polygon(vec![pt(-6.5, 0.0), pt(-3.0, -3.0), pt(4.5, -3.0), pt(1.0, 0.0)], col.gamma_multiply(0.45), Stroke::NONE));
            painter.add(egui::Shape::closed_line(vec![pt(-6.5, 0.0), pt(1.0, 0.0), pt(1.0, 7.0), pt(-6.5, 7.0)], st));
            painter.add(line(&[(-6.5, 0.0), (-3.0, -3.0), (4.5, -3.0), (1.0, 0.0)]));
            painter.add(line(&[(4.5, -3.0), (4.5, 4.0), (1.0, 7.0)]));
            painter.add(line(&[(-1.0, -1.5), (-1.0, -7.5)]));
            painter.add(line(&[(-3.0, -5.5), (-1.0, -7.5), (1.0, -5.5)]));
        }
        Icon::Pointer => {
            painter.add(egui::Shape::closed_line(vec![pt(-4.0, -6.0), pt(4.5, 1.5), pt(0.5, 1.8), pt(-1.5, 6.0)], st));
        }
        Icon::Link => {
            painter.circle_stroke(pt(-2.5, 0.0), 3.3 * s, st);
            painter.circle_stroke(pt(2.5, 0.0), 3.3 * s, st);
        }
        Icon::Undo | Icon::Redo => {
            let f = if icon == Icon::Undo { 1.0 } else { -1.0 };
            let mut pts = Vec::new();
            for k in 0..=16 {
                let a = -std::f32::consts::FRAC_PI_2 + k as f32 / 16.0 * std::f32::consts::PI;
                pts.push((f * (a.cos() * 4.0) + f * 1.0, a.sin() * 4.0 + 0.5));
            }
            pts.push((f * -4.5, 4.5));
            painter.add(line(&pts));
            painter.add(line(&[(f * -1.5, -6.0), (f * -4.5, -3.5), (f * -1.5, -1.0)]));
        }
        Icon::Grid => {
            for (x, y) in [(-5.5, -5.5), (0.8, -5.5), (-5.5, 0.8), (0.8, 0.8)] {
                painter.rect_stroke(rect(x, y, x + 4.7, y + 4.7), 1.0, st, egui::StrokeKind::Middle);
            }
        }
        Icon::LayFlat => {
            painter.line_segment([pt(-6.5, 6.0), pt(6.5, 6.0)], st);
            painter.rect_stroke(rect(-4.5, 0.5, 4.5, 4.0), 1.0, st, egui::StrokeKind::Middle);
            painter.line_segment([pt(0.0, -6.5), pt(0.0, -1.5)], st);
            painter.add(line(&[(-2.2, -3.6), (0.0, -1.5), (2.2, -3.6)]));
        }
        Icon::Overhang => {
            painter.add(line(&[(-6.0, 6.0), (-6.0, -4.0), (6.0, -4.0), (6.0, -1.0), (-2.5, -1.0), (-2.5, 6.0)]));
            for x in [-1.0, 2.0, 5.0] {
                painter.line_segment([pt(x, 0.5), pt(x - 2.0, 3.5)], Stroke::new(1.0, col.gamma_multiply(0.7)));
            }
        }
        Icon::Import => {
            painter.line_segment([pt(0.0, -6.0), pt(0.0, 2.5)], st);
            painter.add(line(&[(-3.0, -0.5), (0.0, 2.5), (3.0, -0.5)]));
            painter.add(line(&[(-6.0, 2.0), (-6.0, 6.0), (6.0, 6.0), (6.0, 2.0)]));
        }
    }
}

/// A square ghost button with a painted icon.
pub fn icon_button(ui: &mut egui::Ui, icon: Icon, tip: &str) -> egui::Response {
    icon_toggle(ui, icon, tip, false)
}

/// An icon button that shows an "on" state.
pub fn icon_toggle(ui: &mut egui::Ui, icon: Icon, tip: &str, on: bool) -> egui::Response {
    let p = pal();
    let (r, resp) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::click());
    let resp = if tip.is_empty() { resp } else { resp.on_hover_text(tip) };
    if on {
        ui.painter().rect_filled(r, 4.0, p.selected);
    } else if resp.hovered() {
        ui.painter().rect_filled(r, 4.0, p.hover);
    }
    let col = if on {
        p.accent
    } else if resp.hovered() {
        p.text
    } else {
        p.muted
    };
    paint_icon(ui.painter(), r.shrink(3.0), icon, col);
    resp
}

/// What a [`slider`] did this frame.
pub struct SliderOut {
    pub changed: bool,
    /// Being dragged or typed into (edits are held back until it settles).
    pub active: bool,
    /// The preset's tick was clicked: go back to the preset value.
    pub reset: bool,
}

/// A thin slider: a 3 px track filled up to the value, a round knob with the value above it
/// while dragging, a tick at the preset's own value (click it to go back), and a field to type
/// an exact number.
#[allow(clippy::too_many_arguments)]
pub fn slider(ui: &mut egui::Ui, value: &mut f64, range: std::ops::RangeInclusive<f64>, step: f64, suffix: &str, decimals: usize, preset: Option<f64>, overridden: bool) -> SliderOut {
    let p = pal();
    let (lo, hi) = (*range.start(), *range.end());
    let mut out = SliderOut { changed: false, active: false, reset: false };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(128.0, 20.0), egui::Sense::click_and_drag());
        let (x0, x1, y) = (rect.left() + 7.0, rect.right() - 7.0, rect.center().y);
        let at = |v: f64| x0 + (x1 - x0) * (((v - lo) / (hi - lo)).clamp(0.0, 1.0) as f32);
        // the preset tick: clicking near it resets
        let tick = preset.filter(|_| overridden).map(at);
        let near_tick = |px: f32| tick.is_some_and(|t| (px - t).abs() < 5.0);
        if let Some(pp) = resp.interact_pointer_pos() {
            if resp.clicked() && near_tick(pp.x) {
                out.reset = true;
            } else if resp.dragged() || resp.clicked() {
                let t = ((pp.x - x0) / (x1 - x0)).clamp(0.0, 1.0) as f64;
                let v = ((lo + (hi - lo) * t) / step).round() * step;
                let v = (v.clamp(lo, hi) * 1e6).round() / 1e6;
                if (v - *value).abs() > 1e-9 {
                    *value = v;
                    out.changed = true;
                }
            }
        }
        out.active |= resp.dragged();
        let painter = ui.painter();
        let xv = at(*value);
        painter.line_segment([egui::pos2(x0, y), egui::pos2(x1, y)], Stroke::new(3.0, p.border));
        painter.line_segment([egui::pos2(x0, y), egui::pos2(xv, y)], Stroke::new(3.0, if overridden { p.accent } else { p.accent.gamma_multiply(0.6) }));
        if let Some(t) = tick {
            painter.line_segment([egui::pos2(t, y - 6.0), egui::pos2(t, y + 6.0)], Stroke::new(1.5, p.muted));
        }
        let hot = resp.hovered() || resp.dragged();
        painter.circle(egui::pos2(xv, y), if hot { 7.0 } else { 6.0 }, p.elevated, Stroke::new(2.0, p.accent));
        if resp.dragged() {
            let t = format!("{:.*}{suffix}", decimals, *value);
            let g = painter.layout_no_wrap(t, egui::FontId::monospace(11.0), p.on_accent);
            let r = Rect::from_center_size(egui::pos2(xv, y - 18.0), g.size() + Vec2::new(8.0, 3.0));
            painter.rect_filled(r, 3.0, p.accent);
            painter.galley(r.min + Vec2::new(4.0, 1.5), g, p.on_accent);
        }
        let tip = match preset {
            Some(v) if overridden => format!("Your value · the preset has {v:.*}{suffix} (click the tick to go back)", decimals),
            Some(_) => "The preset's value · drag or type to change it".to_string(),
            None => String::new(),
        };
        if !tip.is_empty() {
            resp.on_hover_text(tip);
        }
        let dv = ui.add(egui::DragValue::new(value).range(range.clone()).speed(step).suffix(suffix).max_decimals(decimals).min_decimals(decimals.min(2)));
        if dv.changed() {
            *value = ((*value / step).round() * step).clamp(lo, hi);
            out.changed = true;
        }
        out.active |= dv.dragged() || dv.has_focus();
    });
    out
}
