//! Live chamber camera for the Printer panel.
//!
//! Frames come from `only3d_studio::printer::camera_jpeg` on a thread of their own (one RTSPS grab
//! takes a second or two), so slicing and the tool worker never wait on the camera. The thread
//! runs only while the panel keeps asking for frames and stops a few seconds after it is hidden.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

/// Time between the end of one grab and the start of the next.
const INTERVAL: Duration = Duration::from_secs(2);
/// Width the frames are scaled to (enough for a side panel, cheap to decode and upload).
const WIDTH: u32 = 960;

#[derive(Default)]
struct Shared {
    want_until: Option<Instant>,
    running: bool,
    frame: Option<(u64, egui::ColorImage)>,
    at: Option<Instant>,
    error: Option<String>,
    seq: u64,
}

#[derive(Default)]
pub struct CameraFeed {
    shared: Arc<Mutex<Shared>>,
    texture: Option<egui::TextureHandle>,
    shown: u64,
}

impl CameraFeed {
    /// Keep frames coming for the next few seconds (call it every frame the camera is shown).
    pub fn want(&mut self, ctx: &egui::Context, printer: Option<String>) {
        let mut g = self.shared.lock().unwrap();
        g.want_until = Some(Instant::now() + Duration::from_secs(4));
        if g.running {
            return;
        }
        g.running = true;
        drop(g);
        let shared = self.shared.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("printer-camera".into())
            .spawn(move || {
                let mut failures: u32 = 0;
                loop {
                let wanted = shared.lock().unwrap().want_until.is_some_and(|t| Instant::now() < t);
                if !wanted {
                    shared.lock().unwrap().running = false;
                    return;
                }
                let r = only3d_studio::printer::camera_jpeg(printer.as_deref(), Some(WIDTH)).map_err(|e| format!("{}\n→ {}", e.message, e.hint)).and_then(|(_, jpg)| decode(&jpg));
                let failed = r.is_err();
                {
                    let mut g = shared.lock().unwrap();
                    match r {
                        Ok(img) => {
                            g.seq += 1;
                            let seq = g.seq;
                            g.frame = Some((seq, img));
                            g.at = Some(Instant::now());
                            g.error = None;
                        }
                        Err(e) => g.error = Some(e),
                    }
                }
                ctx.request_repaint();
                // after failures, back off (10 s, 20 s, 40 s, then every 60 s): a camera that is
                // down or busy with another viewer should not be hammered with connections
                failures = if failed { failures + 1 } else { 0 };
                let wait = if failures == 0 { INTERVAL } else { (INTERVAL * 5 * 2u32.pow((failures - 1).min(3))).min(Duration::from_secs(60)) };
                std::thread::sleep(wait);
                }
            })
            .ok();
    }

    /// The newest frame as a texture, how old it is, and the last error (if the latest grab failed).
    pub fn frame(&mut self, ctx: &egui::Context) -> (Option<egui::TextureHandle>, Option<Duration>, Option<String>) {
        let mut g = self.shared.lock().unwrap();
        if let Some((seq, img)) = g.frame.take() {
            if seq != self.shown {
                self.shown = seq;
                match &mut self.texture {
                    Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                    None => self.texture = Some(ctx.load_texture("printer-camera", img, egui::TextureOptions::LINEAR)),
                }
            }
        }
        (self.texture.clone(), g.at.map(|t| t.elapsed()), g.error.clone())
    }
}

pub(crate) fn decode(jpg: &[u8]) -> Result<egui::ColorImage, String> {
    let mut d = zune_jpeg::JpegDecoder::new(zune_core::bytestream::ZCursor::new(jpg));
    let rgb = d.decode().map_err(|e| format!("camera frame: {e:?}"))?;
    let (w, h) = d.dimensions().ok_or("camera frame without a size")?;
    if rgb.len() != w * h * 3 {
        return Err(format!("camera frame: unexpected {} bytes for {w}x{h}", rgb.len()));
    }
    Ok(egui::ColorImage::from_rgb([w, h], &rgb))
}

#[cfg(test)]
mod tests {
    #[test]
    fn decodes_a_jpeg() {
        // 2x2 baseline JPEG written by ffmpeg
        let jpg = include_bytes!("../tests/data/tiny.jpg");
        let img = super::decode(jpg).unwrap();
        assert_eq!(img.size, [2, 2]);
    }
}
