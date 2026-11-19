//! File preview in the main area, like a code editor's preview tab. A click on a file in the
//! Files panel opens it in a tab beside the plate:
//!
//! - images (PNG and JPEG decoded here; HEIC, TIFF, GIF, WebP and BMP through `sips`);
//! - text, Markdown, data and code, read-only;
//! - models (STL, 3MF, OBJ, STEP) and only3d projects, drawn by only3d's own renderer, so you
//!   can look before importing;
//! - print files: the picture Bambu stores inside the 3MF;
//! - PDF, SVG, Office documents and video through Quick Look (`qlmanage -t`).
//!
//! Everything loads on a thread of its own; the tab shows a spinner meanwhile.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};
use only3d_studio::workspace::FileKind;
use serde_json::json;

use crate::app::App;
use crate::theme::{self, pal};
use crate::worker::Kind;

/// What a preview shows.
pub enum Content {
    Image { img: egui::ColorImage, note: String },
    Text { text: String, markdown: bool, truncated: bool },
    None(String),
}

type Slot = Arc<Mutex<Option<Content>>>;

/// The file in the preview tab.
pub struct FilePreview {
    pub path: PathBuf,
    pub kind: FileKind,
    slot: Slot,
    content: Option<Content>,
    texture: Option<egui::TextureHandle>,
}

/// Text files are shown up to this many bytes.
const TEXT_MAX: usize = 256 * 1024;

impl FilePreview {
    pub fn open(ctx: &egui::Context, path: &Path, kind: FileKind) -> FilePreview {
        let slot: Slot = Arc::default();
        let (out, p, ctx2) = (slot.clone(), path.to_path_buf(), ctx.clone());
        let bg = pal().bg;
        std::thread::spawn(move || {
            let c = load(&p, kind, [bg.r(), bg.g(), bg.b()]);
            *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(c);
            ctx2.request_repaint();
        });
        FilePreview { path: path.to_path_buf(), kind, slot, content: None, texture: None }
    }
}

fn ext(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

fn load(path: &Path, kind: FileKind, bg: [u8; 3]) -> Content {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let e = ext(path);
    let r: Result<Content, String> = match kind {
        FileKind::Image => image(path, &e).map(|img| {
            let note = format!("{} × {} px", img.size[0], img.size[1]);
            Content::Image { img: crate::workspace_panel::shrink(img, 2400), note }
        }),
        FileKind::Model if e == "scad" => text(path),
        FileKind::Model => model(path, bg),
        FileKind::Project => project(path).or_else(|_| text(path)),
        FileKind::Print if e == "gcode" => text(path),
        FileKind::Print => bambu_thumbnail(path).or_else(|_| quick_look(path)),
        FileKind::Text | FileKind::Data | FileKind::Code | FileKind::Other => text(path).or_else(|_| quick_look(path)),
        FileKind::Document | FileKind::Vector | FileKind::Video | FileKind::Archive => quick_look(path),
        FileKind::Folder => Err("a folder".into()),
    };
    r.unwrap_or_else(|why| Content::None(if size > 0 { why } else { "The file is empty.".into() }))
}

/// PNG and JPEG here; other formats through macOS `sips` (elsewhere they show no picture).
fn image(path: &Path, e: &str) -> Result<egui::ColorImage, String> {
    match e {
        "png" => crate::approvals::decode(&std::fs::read(path).map_err(|e| e.to_string())?, "image/png"),
        "jpg" | "jpeg" => crate::approvals::decode(&std::fs::read(path).map_err(|e| e.to_string())?, "image/jpeg"),
        #[cfg(not(target_os = "macos"))]
        _ => Err("No preview for this kind of picture here yet; open it to see it.".into()),
        #[cfg(target_os = "macos")]
        _ => {
            let out = scratch().join(format!("{}.png", std::process::id()));
            let ok = std::process::Command::new("sips").args(["-s", "format", "png", "-Z", "2400"]).arg(path).arg("--out").arg(&out).output().map(|o| o.status.success()).unwrap_or(false);
            if !ok {
                return quick_look_image(path);
            }
            let r = crate::approvals::decode(&std::fs::read(&out).map_err(|e| e.to_string())?, "image/png");
            let _ = std::fs::remove_file(&out);
            r
        }
    }
}

fn text(path: &Path) -> Result<Content, String> {
    let mut bytes = Vec::new();
    use std::io::Read;
    std::fs::File::open(path).map_err(|e| e.to_string())?.take(TEXT_MAX as u64 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.iter().take(8000).any(|b| *b == 0) {
        return Err("not a text file".into());
    }
    let truncated = bytes.len() > TEXT_MAX;
    bytes.truncate(TEXT_MAX);
    let e = ext(path);
    Ok(Content::Text { text: String::from_utf8_lossy(&bytes).replace('\t', "    "), markdown: e == "md" || e == "markdown", truncated })
}

fn render_note(mesh: &only3d_geom::Mesh) -> String {
    let b = mesh.bounds();
    let s = b.max - b.min;
    format!("{:.1} × {:.1} × {:.1} mm · {} triangles · drawn by only3d", s.x, s.y, s.z, mesh.indices.len())
}

fn model(path: &Path, bg: [u8; 3]) -> Result<Content, String> {
    let mesh = only3d_geom::io::load_merged(path).map_err(|e| format!("couldn't read the model: {e}"))?;
    if mesh.indices.is_empty() {
        return Err("the model has no triangles".into());
    }
    let opts = only3d_geom::raster::RenderOptions { width: 1400, height: 1000, background: bg, ssaa: 2, ..Default::default() };
    let img = only3d_geom::raster::render(&[only3d_geom::raster::RenderItem { mesh: &mesh, color: [214, 200, 178] }], &[], &opts);
    Ok(Content::Image { img: egui::ColorImage::from_rgba_unmultiplied([img.width as usize, img.height as usize], &img.rgba), note: render_note(&mesh) })
}

fn project(path: &Path) -> Result<Content, String> {
    let mut s = only3d_studio::Studio::open(path).map_err(|e| e.message)?;
    let n = s.project.objects.len();
    let plates = s.project.plate_count();
    let req = only3d_studio::render::RenderRequest { width: 1400, height: 1000, ..Default::default() };
    let png = only3d_studio::render::render(&mut s, &req).map_err(|e| e.message)?;
    let img = crate::approvals::decode(&png, "image/png")?;
    Ok(Content::Image { img, note: format!("only3d project · {n} object{} · {plates} plate{} · plate 1 shown", if n == 1 { "" } else { "s" }, if plates == 1 { "" } else { "s" }) })
}

/// The plate picture Bambu Studio (and only3d) store inside a 3MF.
fn bambu_thumbnail(path: &Path) -> Result<Content, String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
    for name in ["Metadata/plate_1.png", "Metadata/thumbnail.png", "Metadata/plate_1_small.png"] {
        if let Ok(mut file) = z.by_name(name) {
            let mut b = Vec::new();
            use std::io::Read;
            file.read_to_end(&mut b).map_err(|e| e.to_string())?;
            let img = crate::approvals::decode(&b, "image/png")?;
            return Ok(Content::Image { img, note: "the plate picture stored in the file".into() });
        }
    }
    Err("no picture inside".into())
}

#[cfg(target_os = "macos")]
fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join("only3d-preview");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// A Quick Look picture of the file (PDF first page, SVG, Office, a video frame…). macOS only.
#[cfg(not(target_os = "macos"))]
fn quick_look_image(_path: &Path) -> Result<egui::ColorImage, String> {
    Err("No preview for this kind of file here yet; open it to see it.".into())
}

/// A Quick Look picture of the file (PDF first page, SVG, Office, a video frame…).
#[cfg(target_os = "macos")]
fn quick_look_image(path: &Path) -> Result<egui::ColorImage, String> {
    let dir = scratch().join(format!("ql-{}-{}", std::process::id(), only3d_studio::now_ms()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut child = std::process::Command::new("qlmanage").args(["-t", "-s", "1600", "-o"]).arg(&dir).arg(path).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().map_err(|e| e.to_string())?;
    // Quick Look can stall on odd files: give it a few seconds
    for _ in 0..80 {
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = child.kill();
    let png = std::fs::read_dir(&dir).ok().and_then(|mut d| d.find_map(|e| e.ok().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "png"))));
    let r = match png {
        Some(p) => crate::approvals::decode(&std::fs::read(&p).map_err(|e| e.to_string())?, "image/png"),
        None => Err("no preview for this kind of file".into()),
    };
    let _ = std::fs::remove_dir_all(&dir);
    r
}

fn quick_look(path: &Path) -> Result<Content, String> {
    let img = quick_look_image(path)?;
    Ok(Content::Image { img, note: "Quick Look preview".into() })
}

impl App {
    /// The file in tab `id`: a header (name, size, what to do with it) over its preview.
    pub(crate) fn preview_body(&mut self, ui: &mut egui::Ui, id: u64) {
        let p = pal();
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, p.bg);
        let Some(pv) = self.files_open.get_mut(&id) else { return };
        if pv.content.is_none() {
            pv.content = pv.slot.lock().unwrap_or_else(|e| e.into_inner()).take();
        }
        if let (Some(Content::Image { img, .. }), None) = (&pv.content, &pv.texture) {
            pv.texture = Some(ui.ctx().load_texture(format!("preview {}", pv.path.display()), img.clone(), egui::TextureOptions::LINEAR));
        }
        // header: what it is, and what you can do with it
        let (kind, path) = (pv.kind, pv.path.clone());
        let md = std::fs::metadata(&path).ok();
        let size = md.as_ref().map(|m| only3d_studio::workspace::human_size(m.len())).unwrap_or_default();
        let note = match &pv.content {
            Some(Content::Image { note, .. }) => note.clone(),
            Some(Content::Text { truncated: true, .. }) => format!("first {} shown", only3d_studio::workspace::human_size(TEXT_MAX as u64)),
            _ => String::new(),
        };
        let mut action: Option<&'static str> = None;
        let head = egui::Rect::from_min_size(rect.min + egui::vec2(16.0, 10.0), egui::vec2(rect.width() - 32.0, 26.0));
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let info = format!("{size}{}", if note.is_empty() { String::new() } else { format!(" · {note}") });
        // buttons first (right), then the name and details in what is left, cut short to fit
        ui.scope_builder(egui::UiBuilder::new().max_rect(head).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| {
            if ui.button(crate::os::show_in()).clicked() {
                action = Some("reveal");
            }
            let main = match kind {
                FileKind::Project => "Open project",
                FileKind::Model => "Import onto the plate",
                FileKind::Print => "",
                _ => "Open in its app",
            };
            if !main.is_empty() && ui.add(theme::primary_button(main)).clicked() {
                action = Some("open");
            }
            ui.add_space(12.0);
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(RichText::new(&name).size(13.0).family(theme::medium())).truncate()).on_hover_text(crate::chrome::tilde(&path));
                ui.add(egui::Label::new(RichText::new(&info).size(11.5).color(p.placeholder)).truncate());
            });
        });
        let body = egui::Rect::from_min_max(egui::pos2(rect.min.x + 16.0, head.max.y + 8.0), rect.max - egui::vec2(16.0, 16.0));
        let Some(pv) = self.files_open.get(&id) else { return };
        match &pv.content {
            None => {
                ui.scope_builder(egui::UiBuilder::new().max_rect(body).layout(egui::Layout::top_down(egui::Align::Center)), |ui| {
                    ui.add_space(body.height() * 0.4);
                    ui.add(egui::Spinner::new().size(18.0).color(p.accent));
                    ui.label(RichText::new("Loading the preview…").size(12.0).color(p.muted));
                });
            }
            Some(Content::Image { .. }) => {
                if let Some(t) = &pv.texture {
                    let s = t.size_vec2() / ui.ctx().pixels_per_point().max(1.0);
                    // fit the area, at most twice the picture's own size
                    let k = (body.width() / s.x).min(body.height() / s.y).min(2.0);
                    let r = egui::Rect::from_center_size(body.center(), s * k);
                    ui.painter().image(t.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                }
            }
            Some(Content::Text { text, markdown, .. }) => {
                let (text, markdown) = (text.clone(), *markdown);
                let frame = egui::Frame::new().fill(p.elevated).stroke(egui::Stroke::new(1.0, p.border_variant)).corner_radius(6).inner_margin(egui::Margin::symmetric(18, 14));
                ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| {
                    frame.show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                            if markdown {
                                markdown_ui(ui, &text);
                            } else {
                                ui.add(egui::Label::new(RichText::new(text).monospace().size(12.0).color(p.text)).selectable(true).extend());
                            }
                        });
                    });
                });
            }
            Some(Content::None(why)) => {
                ui.scope_builder(egui::UiBuilder::new().max_rect(body).layout(egui::Layout::top_down(egui::Align::Center)), |ui| {
                    ui.add_space(body.height() * 0.38);
                    ui.label(RichText::new("No preview").size(15.0).family(theme::medium()));
                    ui.label(RichText::new(format!("{why}. Use the buttons above to open it.")).size(12.0).color(p.muted));
                });
            }
        }
        match action {
            Some("reveal") => {
                crate::os::reveal(std::path::Path::new(&path));
            }
            Some(_) => match kind {
                FileKind::Project => {
                    self.open_project(path.clone());
                    self.editor.show_plate();
                }
                FileKind::Model => {
                    self.worker.call("import_file", json!({ "path": path.display().to_string() }), Kind::AddObject);
                    self.editor.show_plate();
                }
                _ => {
                    crate::os::open(&path);
                }
            },
            None => {}
        }
    }
}

/// A small Markdown view: headings, bullets, quotes and code blocks; the rest as text.
fn markdown_ui(ui: &mut egui::Ui, text: &str) {
    let p = pal();
    let mut code = false;
    for line in text.lines().take(6000) {
        let t = line.trim_end();
        if t.trim_start().starts_with("```") {
            code = !code;
            continue;
        }
        if code {
            ui.add(egui::Label::new(RichText::new(t).monospace().size(12.0).color(p.text).background_color(p.bg)).selectable(true).extend());
            continue;
        }
        let (size, body, medium) = if let Some(h) = t.strip_prefix("### ") {
            (15.0, h.to_string(), true)
        } else if let Some(h) = t.strip_prefix("## ") {
            (17.0, h.to_string(), true)
        } else if let Some(h) = t.strip_prefix("# ") {
            (21.0, h.to_string(), true)
        } else if let Some(b) = t.trim_start().strip_prefix("- ").or_else(|| t.trim_start().strip_prefix("* ")) {
            let indent = (t.len() - t.trim_start().len()) / 2;
            (13.0, format!("{}•  {b}", "    ".repeat(indent)), false)
        } else if let Some(q) = t.strip_prefix("> ") {
            (13.0, format!("│ {q}"), false)
        } else {
            (13.0, t.to_string(), false)
        };
        if medium {
            ui.add_space(6.0);
        }
        let plain = body.replace("**", "").replace('`', "");
        let mut rt = RichText::new(plain).size(size).color(if medium { p.text } else { p.text.gamma_multiply(0.9) });
        if medium {
            rt = rt.family(theme::medium());
        }
        ui.add(egui::Label::new(rt).selectable(true).wrap());
        if medium {
            ui.add_space(2.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_text_and_refuses_binary() {
        let dir = std::env::temp_dir().join(format!("o3d_preview_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let md = dir.join("notes.md");
        std::fs::write(&md, "# Title\n- a\n").unwrap();
        assert!(matches!(text(&md), Ok(Content::Text { markdown: true, truncated: false, .. })));
        let bin = dir.join("blob.bin");
        std::fs::write(&bin, [0u8, 1, 2, 3]).unwrap();
        assert!(text(&bin).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
