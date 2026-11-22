//! Timelapses in the Printer panel: the videos on the printer, newest first, each with its
//! thumbnail and the print it shows, to download, open or delete.
//!
//! Listing and downloading run on threads of their own (a video is tens of MB over the printer's
//! FTPS) through the functions behind `printer_files` and `printer_file`, which need no session.
//! Deleting goes through `printer_file`, after a confirmation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText, Ui};
use only3d_studio::timelapse::{Timelapse, MAX_THUMBNAILS};
use serde_json::json;

use crate::app::{App, Dialog};
use crate::panels::muted;
use crate::theme::{self, pal};

/// The section's id in the Printer panel.
pub const SECTION: &str = "printer.timelapses";
/// An open section lists again after this long.
const FRESH: Duration = Duration::from_secs(300);
/// Rows shown before "show all".
const FEW: usize = 8;
const THUMB: egui::Vec2 = egui::vec2(96.0, 54.0);

#[derive(Default)]
struct Shared {
    /// The printer being listed now.
    loading: Option<String>,
    listed: Option<Listed>,
    /// Thumbnails decoded since the last frame: video path → picture.
    pictures: Vec<(String, egui::ColorImage)>,
    /// Downloads under way: video path → fraction done.
    downloads: HashMap<String, f32>,
    /// Downloads that ended since the last frame.
    done: Vec<(String, Result<PathBuf, String>)>,
}

struct Listed {
    printer: String,
    at: Instant,
    videos: Result<Vec<Timelapse>, String>,
}

#[derive(Default)]
pub struct Timelapses {
    shared: Arc<Mutex<Shared>>,
    textures: HashMap<String, egui::TextureHandle>,
    /// Videos already in the downloads folder: video path → file.
    local: HashMap<String, PathBuf>,
    stale: bool,
    show_all: bool,
}

/// Where videos are saved.
fn downloads_dir() -> PathBuf {
    let home = std::env::home_dir().unwrap_or_default();
    let d = home.join("Downloads");
    if d.is_dir() {
        d
    } else {
        home
    }
}

/// The file a video is saved as in `dir`, and whether it is there already (same name and size).
/// Never an existing file that isn't it.
fn local_file(dir: &Path, v: &Timelapse) -> (PathBuf, bool) {
    // the printer's name for it, as a plain file name
    let name = Path::new(&v.name).file_name().and_then(|n| n.to_str()).unwrap_or("timelapse.mp4");
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, "mp4"));
    (1..)
        .map(|n| dir.join(if n == 1 { name.to_string() } else { format!("{stem} ({n}).{ext}") }))
        .find_map(|f| match std::fs::metadata(&f) {
            Err(_) => Some((f, false)),
            Ok(m) if v.bytes.is_some_and(|b| b > 0 && b == m.len()) => Some((f, true)),
            Ok(_) => None,
        })
        .expect("some name is free")
}

impl Timelapses {
    /// List again when the section is next shown.
    pub fn stale(&mut self) {
        self.stale = true;
    }

    /// Sample rows, to picture the section without a printer.
    pub fn demo(&mut self, printer: &str, videos: Vec<Timelapse>, picture: egui::ColorImage) {
        let mut g = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        g.pictures = videos.iter().filter(|v| v.thumbnail.is_some()).map(|v| (v.path.clone(), picture.clone())).collect();
        if let Some(v) = videos.get(1) {
            g.downloads.insert(v.path.clone(), 0.4);
        }
        g.listed = Some(Listed { printer: printer.into(), at: Instant::now(), videos: Ok(videos) });
    }

    fn list(&mut self, ctx: &egui::Context, printer: &str) {
        let mut g = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        if g.loading.is_some() {
            return;
        }
        g.loading = Some(printer.to_string());
        drop(g);
        self.stale = false;
        let (shared, ctx, printer) = (self.shared.clone(), ctx.clone(), printer.to_string());
        std::thread::spawn(move || {
            let r = only3d_studio::timelapse::list(Some(&printer), MAX_THUMBNAILS);
            let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
            g.loading = None;
            let videos = match r {
                Ok(l) => {
                    g.pictures = l.thumbnails.iter().filter_map(|(p, jpg)| Some((p.clone(), crate::printer_live::decode(jpg).ok()?))).collect();
                    Ok(l.videos)
                }
                Err(e) => Err(format!("{}\n→ {}", e.message, e.hint)),
            };
            g.listed = Some(Listed { printer, at: Instant::now(), videos });
            drop(g);
            ctx.request_repaint();
        });
    }

    fn download(&mut self, ctx: &egui::Context, printer: &str, v: &Timelapse) {
        let (dest, _) = local_file(&downloads_dir(), v);
        self.shared.lock().unwrap_or_else(|e| e.into_inner()).downloads.insert(v.path.clone(), 0.0);
        let (shared, ctx, printer, path) = (self.shared.clone(), ctx.clone(), printer.to_string(), v.path.clone());
        std::thread::spawn(move || {
            let mut shown = Instant::now();
            let r = only3d_studio::printer::download(Some(&printer), &path, &dest, &mut |got, total| {
                if shown.elapsed() > Duration::from_millis(150) {
                    shown = Instant::now();
                    let f = total.filter(|t| *t > 0).map_or(0.0, |t| got as f32 / t as f32);
                    shared.lock().unwrap_or_else(|e| e.into_inner()).downloads.insert(path.clone(), f);
                    ctx.request_repaint();
                }
            });
            let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
            g.downloads.remove(&path);
            g.done.push((path, r.map(|_| dest).map_err(|e| format!("{}\n→ {}", e.message, e.hint))));
            drop(g);
            ctx.request_repaint();
        });
    }
}

/// What a row asked for.
enum Ask {
    Download,
    Open(PathBuf),
    Reveal(PathBuf),
    Delete,
}

impl App {
    pub(crate) fn timelapses_section(&mut self, ui: &mut Ui, printer: &str) {
        let p = pal();
        let t = &mut self.timelapses;
        let (mut g, mut toasts) = (t.shared.lock().unwrap_or_else(|e| e.into_inner()), Vec::new());
        for (path, img) in g.pictures.drain(..) {
            t.textures.insert(path.clone(), ui.ctx().load_texture(format!("timelapse {path}"), img, egui::TextureOptions::LINEAR));
        }
        for (path, r) in g.done.drain(..) {
            match r {
                Ok(f) => {
                    toasts.push((format!("Saved {}", crate::chrome::tilde(&f)), false));
                    t.local.insert(path, f);
                }
                Err(e) => toasts.push((format!("The timelapse wasn't downloaded: {e}"), true)),
            }
        }
        let loading = g.loading.is_some();
        // another printer's videos are not this one's
        let listed = g.listed.as_ref().filter(|l| l.printer == printer);
        let due = listed.is_none_or(|l| l.at.elapsed() > FRESH) || t.stale;
        let videos = listed.map(|l| l.videos.clone());
        let downloads = g.downloads.clone();
        drop(g);
        for (text, err) in toasts {
            self.toast(text, err);
        }
        let count = match &videos {
            Some(Ok(v)) if !v.is_empty() => format!("Timelapses · {}", v.len()),
            _ => "Timelapses".into(),
        };
        let mut refresh = false;
        let open = self.section_header(ui, SECTION, &count, false, false, |ui| {
            if loading {
                ui.add(egui::Spinner::new().size(12.0).color(p.accent));
            } else if videos.is_some() && theme::bracket_button(ui, "refresh").on_hover_text("List the printer's timelapses again").clicked() {
                refresh = true;
            }
        });
        if !open {
            return;
        }
        if (due || refresh) && !loading {
            self.timelapses.list(ui.ctx(), printer);
        }
        let videos = match videos {
            None => {
                ui.label(muted("Reading the printer's timelapses…").size(11.5));
                return;
            }
            Some(Err(e)) => {
                let (what, hint) = e.split_once("\n→ ").unwrap_or((e.as_str(), ""));
                ui.add(egui::Label::new(RichText::new(what).size(11.5).color(p.error)).wrap());
                if !hint.is_empty() {
                    ui.add(egui::Label::new(muted(format!("→ {hint}")).size(11.0)).wrap());
                }
                return;
            }
            Some(Ok(v)) => v,
        };
        if videos.is_empty() {
            ui.add(egui::Label::new(muted("None yet. Tick \"Record a timelapse\" when you send a print. Videos the printer keeps on its internal storage can't be listed.").size(11.5)).wrap());
            return;
        }
        let shown = if self.timelapses.show_all { videos.len() } else { FEW.min(videos.len()) };
        for v in &videos[..shown] {
            let local = self.timelapses.local.entry(v.path.clone()).or_insert_with(|| {
                let (f, there) = local_file(&downloads_dir(), v);
                if there { f } else { PathBuf::new() }
            });
            let local = Some(local.clone()).filter(|f| !f.as_os_str().is_empty());
            let Some(ask) = row(ui, v, self.timelapses.textures.get(&v.path), local, downloads.get(&v.path).copied()) else { continue };
            match ask {
                Ask::Download => self.timelapses.download(ui.ctx(), printer, v),
                Ask::Open(f) | Ask::Reveal(f) if !f.exists() => {
                    // moved or deleted since: offer the download again
                    self.timelapses.local.remove(&v.path);
                    self.toast(format!("{} isn't there any more", crate::chrome::tilde(&f)), true);
                }
                Ask::Open(f) => crate::os::open(&f),
                Ask::Reveal(f) => crate::os::reveal(&f),
                Ask::Delete => {
                    self.dialog = Some(Dialog::Confirm {
                        title: "Delete this timelapse?".into(),
                        body: format!("{} is deleted from {printer}'s storage. This cannot be undone.", v.name),
                        tool: "printer_file".into(),
                        args: json!({ "name": printer, "action": "delete", "path": v.path, "confirm": true }),
                    })
                }
            }
        }
        if videos.len() > FEW && theme::bracket_button(ui, &if self.timelapses.show_all { "show fewer".to_string() } else { format!("show all {}", videos.len()) }).clicked() {
            self.timelapses.show_all = !self.timelapses.show_all;
        }
    }
}

fn row(ui: &mut Ui, v: &Timelapse, picture: Option<&egui::TextureHandle>, local: Option<PathBuf>, downloading: Option<f32>) -> Option<Ask> {
    let p = pal();
    let mut ask = None;
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(THUMB, egui::Sense::hover());
        ui.painter().rect_filled(rect, 3.0, p.hover);
        match picture {
            Some(t) => {
                // fill the box, cutting what doesn't fit
                let s = t.size_vec2();
                let k = (s / THUMB).min_elem();
                let part = THUMB * k / s;
                let uv = egui::Rect::from_center_size(egui::pos2(0.5, 0.5), part);
                egui::Image::new(t).uv(uv).corner_radius(3).paint_at(ui, rect);
            }
            None => {
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "no picture", egui::FontId::proportional(10.0), p.placeholder);
            }
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let title = v.job.as_deref().filter(|j| !j.is_empty()).unwrap_or(&v.name);
            let hover = match &v.print {
                Some(id) => format!("{}\n{id}: the print running when it was recorded", v.path),
                None => v.path.clone(),
            };
            ui.add(egui::Label::new(RichText::new(title).size(12.5)).truncate()).on_hover_text(hover);
            let mut facts: Vec<String> = Vec::new();
            facts.extend(v.recorded.as_deref().map(|t| t.get(..16).unwrap_or(t).to_string()).or(v.modified.clone()));
            facts.extend(v.bytes.map(|b| format!("{:.1} MB", b as f64 / 1e6)));
            facts.extend(v.print.clone());
            ui.add(egui::Label::new(muted(facts.join(" · ")).size(11.0)).truncate());
            ui.horizontal(|ui| match (downloading, local) {
                (Some(f), _) => {
                    ui.add(egui::ProgressBar::new(f).desired_width(ui.available_width().min(140.0)).desired_height(6.0)).on_hover_text(format!("Downloading: {:.0}%", f * 100.0));
                }
                (None, local) => {
                    match local {
                        Some(f) => {
                            if theme::bracket_button(ui, "open").on_hover_text(crate::chrome::tilde(&f)).clicked() {
                                ask = Some(Ask::Open(f.clone()));
                            }
                            if theme::bracket_button(ui, "show").on_hover_text(crate::os::show_in()).clicked() {
                                ask = Some(Ask::Reveal(f));
                            }
                        }
                        None => {
                            if theme::bracket_button(ui, "download").on_hover_text(format!("Save it to {}", crate::chrome::tilde(&downloads_dir()))).clicked() {
                                ask = Some(Ask::Download);
                            }
                        }
                    }
                    if theme::bracket_button(ui, "delete").on_hover_text("Delete it from the printer…").clicked() {
                        ask = Some(Ask::Delete);
                    }
                }
            });
        });
    });
    ui.add_space(2.0);
    ask
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_download_never_lands_on_another_file() {
        let dir = std::env::temp_dir().join(format!("only3d-timelapses-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let v = |name: &str, bytes| Timelapse { name: name.into(), path: format!("/timelapse/{name}"), bytes, recorded: None, modified: None, thumbnail: None, print: None, job: None };
        let a = v("a.mp4", Some(4));
        assert_eq!(local_file(&dir, &a), (dir.join("a.mp4"), false));
        std::fs::write(dir.join("a.mp4"), b"else!").unwrap();
        assert_eq!(local_file(&dir, &a), (dir.join("a (2).mp4"), false), "another file has the name");
        std::fs::write(dir.join("a (2).mp4"), b"done").unwrap();
        assert_eq!(local_file(&dir, &a), (dir.join("a (2).mp4"), true), "downloaded before");
        // a size the printer didn't say can't tell them apart
        assert_eq!(local_file(&dir, &v("a.mp4", None)), (dir.join("a (3).mp4"), false));
        // a name from the printer is never a path
        assert_eq!(local_file(&dir, &v("../../evil.mp4", Some(1))).0, dir.join("evil.mp4"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
