//! Agent requests waiting for the user's approval (see `only3d_studio::approval`).
//!
//! The live-socket thread that carries an agent's call blocks in [`ApprovalQueue::approver`]
//! until the user answers the dialog drawn by [`ApprovalQueue::ui`], or [`WAIT`] passes. It holds
//! no session lock meanwhile, so the window keeps working. A desktop notification announces each
//! request, and one that lapses, for a user who isn't looking at the window (F-29).

use std::collections::VecDeque;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Margin, RichText};
use only3d_studio::approval::{ApprovalRequest, Decision, WAIT};
use only3d_studio::live::Approver;

use crate::theme::{self, pal};

/// How long a request is on screen before its buttons work: when one request lapses and the next
/// takes its place, a click or Esc meant for the old one doesn't answer the new one.
const SETTLE: Duration = Duration::from_millis(800);

/// Whether a request shown since `shown_at` can be answered at `now`.
fn settled(shown_at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(shown_at) >= SETTLE
}

struct Pending {
    id: u64,
    req: ApprovalRequest,
    reply: SyncSender<Decision>,
    deadline: Instant,
}

#[derive(Default)]
pub struct ApprovalQueue {
    pending: Arc<Mutex<VecDeque<Pending>>>,
    next_id: Arc<Mutex<u64>>,
    /// The request on screen: its id, images and when it appeared.
    shown: Option<(u64, Vec<egui::TextureHandle>, Instant)>,
}

impl ApprovalQueue {
    /// The callback the live socket uses to ask the user. Blocks until they answer.
    pub fn approver(&self, ctx: egui::Context) -> Approver {
        let pending = self.pending.clone();
        let next_id = self.next_id.clone();
        Arc::new(move |req: ApprovalRequest| {
            let (tx, rx) = sync_channel(1);
            let (title, requester) = (req.title.clone(), req.requester.clone());
            let id = {
                let mut n = next_id.lock().unwrap();
                *n += 1;
                *n
            };
            pending.lock().unwrap().push_back(Pending { id, req, reply: tx, deadline: Instant::now() + WAIT });
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Critical));
            crate::os::notify("Approval needed", &format!("{requester} asks: {title}"));
            ctx.request_repaint();
            let d = rx.recv_timeout(WAIT).unwrap_or(Decision::TimedOut);
            if d == Decision::TimedOut {
                crate::os::notify("Approval request lapsed", &format!("{title}: not done, nobody answered in {}", only3d_studio::approval::wait_text()));
            }
            ctx.request_repaint();
            d
        })
    }

    /// Queue a request nobody waits for (screenshots of the dialog).
    pub fn demo(&self, req: ApprovalRequest) {
        let (tx, _rx) = sync_channel(1);
        self.pending.lock().unwrap().push_back(Pending { id: 0, req, reply: tx, deadline: Instant::now() + WAIT });
    }

    /// Draw the oldest waiting request as a modal; returns true while one is shown.
    pub fn ui(&mut self, ctx: &egui::Context) -> bool {
        let p = pal();
        let mut q = self.pending.lock().unwrap();
        let now = Instant::now();
        q.retain(|r| r.deadline > now);
        let Some(front) = q.front() else {
            self.shown = None;
            return false;
        };
        if self.shown.as_ref().map(|s| s.0) != Some(front.id) {
            let tex = front
                .req
                .images
                .iter()
                .enumerate()
                .filter_map(|(i, im)| decode(&im.png, &im.mime).ok().map(|img| ctx.load_texture(format!("approval-{}-{i}", front.id), img, egui::TextureOptions::LINEAR)))
                .collect();
            self.shown = Some((front.id, tex, now));
        }
        let ready = self.shown.as_ref().is_some_and(|s| settled(s.2, now));
        let left = front.deadline.saturating_duration_since(now).as_secs();
        let mut answer: Option<Decision> = None;
        let esc = ready && ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let frame = egui::Frame::new().fill(p.elevated).stroke(egui::Stroke::new(1.0, p.warning)).corner_radius(8).inner_margin(Margin::same(18));
        egui::Modal::new(egui::Id::unique("o3d_approval")).backdrop_color(Color32::from_black_alpha(if p.dark { 120 } else { 60 })).frame(frame).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.horizontal(|ui| {
                theme::chip(ui, "approval needed", p.warning, theme::soft(p.warning));
                ui.label(RichText::new(format!("asked by {}", front.req.requester)).size(12.0).color(p.muted));
            });
            ui.add_space(6.0);
            ui.label(RichText::new(&front.req.title).family(theme::medium()).size(16.0));
            ui.add_space(6.0);
            // pictures side by side (the model, then the plate), sized so the whole dialog fits
            if let Some((_, tex, _)) = &self.shown {
                let n = tex.len().clamp(1, 3) as f32;
                let gap = 8.0;
                let cell = (ui.available_width() - gap * (n - 1.0)) / n;
                let screen_h = ctx.content_rect().height();
                let max_h = (screen_h * 0.28).clamp(120.0, 300.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for t in tex.iter().take(3) {
                        let sz = t.size_vec2();
                        let mut s = egui::vec2(cell, cell * sz.y / sz.x.max(1.0));
                        if s.y > max_h {
                            s *= max_h / s.y;
                        }
                        ui.add(egui::Image::new(t).fit_to_exact_size(s).corner_radius(4));
                    }
                });
                ui.add_space(6.0);
            }
            let details_h = (ctx.content_rect().height() * 0.32).clamp(120.0, 320.0);
            egui::ScrollArea::vertical().max_height(details_h).show(ui, |ui| {
                ui.label(RichText::new(&front.req.details).monospace().size(11.5));
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                // Deny is the safe choice and the keyboard's: Esc denies, nothing approves by key
                if ui.add_enabled(ready, egui::Button::new(RichText::new("Deny").size(13.0)).min_size(egui::vec2(110.0, 30.0))).clicked() || esc {
                    answer = Some(Decision::Denied);
                }
                ui.add_space(8.0);
                if ui.add_enabled(ready, theme::primary_button("Approve").min_size(egui::vec2(140.0, 30.0))).clicked() {
                    answer = Some(Decision::Approved);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("denied automatically in {}:{:02}", left / 60, left % 60)).size(11.0).color(p.placeholder));
                });
            });
        });
        if let Some(d) = answer {
            let r = q.pop_front().unwrap();
            let _ = r.reply.try_send(d);
            self.shown = None;
        }
        ctx.request_repaint_after(if ready { Duration::from_secs(1) } else { SETTLE });
        true
    }
}

pub(crate) fn decode(bytes: &[u8], mime: &str) -> Result<egui::ColorImage, String> {
    if mime.contains("jpeg") {
        return crate::printer_live::decode(bytes);
    }
    let mut d = png::Decoder::new(std::io::Cursor::new(bytes));
    d.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = d.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let (w, h) = (info.width as usize, info.height as usize);
    let px = &buf[..info.buffer_size()];
    Ok(match info.color_type {
        png::ColorType::Rgba => egui::ColorImage::from_rgba_unmultiplied([w, h], px),
        png::ColorType::Rgb => egui::ColorImage::from_rgb([w, h], px),
        png::ColorType::GrayscaleAlpha => egui::ColorImage::from_rgba_unmultiplied([w, h], &px.chunks(2).flat_map(|c| [c[0], c[0], c[0], c[1]]).collect::<Vec<_>>()),
        _ => egui::ColorImage::from_rgb([w, h], &px.iter().flat_map(|&g| [g, g, g]).collect::<Vec<_>>()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request that takes the place of one that lapsed can't be answered by a click or Esc
    /// meant for the old one.
    #[test]
    fn a_new_request_takes_no_answer_in_its_first_moment() {
        let t = Instant::now();
        assert!(!settled(t, t));
        assert!(!settled(t, t + Duration::from_millis(300)));
        assert!(settled(t, t + SETTLE));
        assert!(WAIT >= Duration::from_secs(15 * 60), "a request waits long enough for a user away from the window (F-29)");
    }
}
