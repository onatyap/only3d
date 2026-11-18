//! The Efficient Elements-style arrange actions: each is an `arrange` (or `duplicate_object`)
//! tool call, an action id you can bind in keymap.json (`arrange.align_left`…), a palette entry
//! and a small drawn icon for the Inspector's Arrange section.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke};
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArrangeOp {
    AlignLeft,
    AlignCenterX,
    AlignRight,
    AlignFront,
    AlignMiddleY,
    AlignBack,
    DockLeft,
    DockRight,
    DockFront,
    DockBack,
    DistributeX,
    DistributeY,
    StackX,
    StackY,
    Grid,
    Circle,
    Swap,
    Snap,
    MatchWidth,
    MatchDepth,
    MatchHeight,
    MatchSize,
    StretchX,
    StretchY,
    FillX,
    FillY,
    DuplicateRight,
    DuplicateBack,
    /// Opens the Copies window (count, row, grid / matrix, fill).
    Copies,
    FillPlate,
    CopyTransform,
    PasteTransform,
    Assemble,
    Union,
    Subtract,
    Intersect,
    Explode,
}

use ArrangeOp::*;

impl ArrangeOp {
    pub const ALL: [ArrangeOp; 37] = [
        AlignLeft, AlignCenterX, AlignRight, AlignFront, AlignMiddleY, AlignBack, DockLeft, DockRight, DockFront, DockBack, DistributeX, DistributeY, StackX, StackY, Grid, Circle, Swap, Snap, MatchWidth, MatchDepth, MatchHeight, MatchSize, StretchX, StretchY, FillX, FillY, DuplicateRight, DuplicateBack, Copies, FillPlate, CopyTransform, PasteTransform, Assemble, Union, Subtract, Intersect, Explode,
    ];

    /// Combining objects (the `combine` tool), shown as their own row and menu.
    pub const COMBINE: [ArrangeOp; 5] = [Assemble, Union, Subtract, Intersect, Explode];

    /// The Inspector's rows of buttons, one kind of change per row.
    pub const ROWS: [(&'static str, &'static [ArrangeOp]); 8] = [
        ("Align", &[AlignLeft, AlignCenterX, AlignRight, AlignFront, AlignMiddleY, AlignBack]),
        ("Dock", &[DockLeft, DockRight, DockFront, DockBack]),
        ("Spread", &[DistributeX, DistributeY, StackX, StackY]),
        ("Layout", &[Grid, Circle, Swap, Snap]),
        ("Match", &[MatchWidth, MatchDepth, MatchHeight, MatchSize]),
        ("Stretch", &[StretchX, StretchY, FillX, FillY]),
        ("Copy", &[DuplicateRight, DuplicateBack, Copies, FillPlate, CopyTransform, PasteTransform]),
        ("Combine", &[Assemble, Union, Subtract, Intersect, Explode]),
    ];

    pub fn id(self) -> &'static str {
        match self {
            AlignLeft => "arrange.align_left",
            AlignCenterX => "arrange.align_center_x",
            AlignRight => "arrange.align_right",
            AlignFront => "arrange.align_front",
            AlignMiddleY => "arrange.align_middle_y",
            AlignBack => "arrange.align_back",
            DockLeft => "arrange.dock_left",
            DockRight => "arrange.dock_right",
            DockFront => "arrange.dock_front",
            DockBack => "arrange.dock_back",
            DistributeX => "arrange.distribute_x",
            DistributeY => "arrange.distribute_y",
            StackX => "arrange.stack_x",
            StackY => "arrange.stack_y",
            Grid => "arrange.grid",
            Circle => "arrange.circle",
            Swap => "arrange.swap",
            Snap => "arrange.snap_to_cells",
            MatchWidth => "arrange.match_width",
            MatchDepth => "arrange.match_depth",
            MatchHeight => "arrange.match_height",
            MatchSize => "arrange.match_size",
            StretchX => "arrange.stretch_x",
            StretchY => "arrange.stretch_y",
            FillX => "arrange.fill_x",
            FillY => "arrange.fill_y",
            DuplicateRight => "arrange.duplicate_right",
            DuplicateBack => "arrange.duplicate_back",
            Copies => "arrange.copies",
            FillPlate => "arrange.fill_plate",
            CopyTransform => "arrange.copy_transform",
            PasteTransform => "arrange.paste_transform",
            Assemble => "combine.assemble",
            Union => "combine.union",
            Subtract => "combine.subtract",
            Intersect => "combine.intersect",
            Explode => "combine.explode",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AlignLeft => "Align left edges",
            AlignCenterX => "Align centres left–right",
            AlignRight => "Align right edges",
            AlignFront => "Align front edges",
            AlignMiddleY => "Align centres front–back",
            AlignBack => "Align back edges",
            DockLeft => "Dock to the anchor's left",
            DockRight => "Dock to the anchor's right",
            DockFront => "Dock to the anchor's front",
            DockBack => "Dock to the anchor's back",
            DistributeX => "Distribute left–right (equal gaps)",
            DistributeY => "Distribute front–back (equal gaps)",
            StackX => "Stack left to right, touching",
            StackY => "Stack front to back, touching",
            Grid => "Arrange in a grid (selection order)",
            Circle => "Arrange on a circle around the anchor",
            Swap => "Swap the two objects' places",
            Snap => "Snap to 10 mm plate cells",
            MatchWidth => "Match the anchor's width",
            MatchDepth => "Match the anchor's depth",
            MatchHeight => "Match the anchor's height",
            MatchSize => "Match the anchor's size",
            StretchX => "Stretch left–right to the anchor's far side",
            StretchY => "Stretch front–back to the anchor's far side",
            FillX => "Extend left–right until touching the anchor",
            FillY => "Extend front–back until touching the anchor",
            DuplicateRight => "Duplicate to the right, touching",
            DuplicateBack => "Duplicate to the back, touching",
            Copies => "Copies… (a number, a row or a grid)",
            FillPlate => "Fill the plate with copies",
            CopyTransform => "Copy position, rotation and scale",
            PasteTransform => "Paste position, rotation and scale",
            Assemble => "Group into one object (each part keeps its filament)",
            Union => "Merge into one solid (union)",
            Subtract => "Subtract the others from the anchor (difference)",
            Intersect => "Keep only where they overlap (intersection)",
            Explode => "Ungroup into separate objects",
        }
    }

    /// Needs an object as anchor (the plate won't do).
    pub fn needs_anchor_object(self) -> bool {
        matches!(self, DockLeft | DockRight | DockFront | DockBack | MatchWidth | MatchDepth | MatchHeight | MatchSize | StretchX | StretchY | FillX | FillY)
    }

    /// The tool call for the selection (`plate`: use the plate as the anchor). Copy and paste
    /// transform are handled by the window (they keep the source object).
    pub fn call(self, sel: &[String], plate: bool) -> Option<(&'static str, Value)> {
        let mut a = match self {
            AlignLeft => json!({"op": "align", "x": "left"}),
            AlignCenterX => json!({"op": "align", "x": "center"}),
            AlignRight => json!({"op": "align", "x": "right"}),
            AlignFront => json!({"op": "align", "y": "front"}),
            AlignMiddleY => json!({"op": "align", "y": "middle"}),
            AlignBack => json!({"op": "align", "y": "back"}),
            DockLeft => json!({"op": "dock", "x": "left"}),
            DockRight => json!({"op": "dock", "x": "right"}),
            DockFront => json!({"op": "dock", "y": "front"}),
            DockBack => json!({"op": "dock", "y": "back"}),
            DistributeX => json!({"op": "distribute", "axis": "x"}),
            DistributeY => json!({"op": "distribute", "axis": "y"}),
            StackX => json!({"op": "distribute", "axis": "x", "gap": 0}),
            StackY => json!({"op": "distribute", "axis": "y", "gap": 0}),
            Grid => json!({"op": "grid"}),
            Circle => json!({"op": "circle"}),
            Swap => json!({"op": "swap"}),
            Snap => json!({"op": "snap"}),
            MatchWidth => json!({"op": "match_size", "axis": "x"}),
            MatchDepth => json!({"op": "match_size", "axis": "y"}),
            MatchHeight => json!({"op": "match_size", "axis": "z"}),
            MatchSize => json!({"op": "match_size", "axis": "xyz"}),
            StretchX => json!({"op": "match_size", "axis": "x", "mode": "stretch"}),
            StretchY => json!({"op": "match_size", "axis": "y", "mode": "stretch"}),
            FillX => json!({"op": "match_size", "axis": "x", "mode": "fill"}),
            FillY => json!({"op": "match_size", "axis": "y", "mode": "fill"}),
            DuplicateRight => return sel.last().map(|id| ("duplicate_object", json!({"id": id, "direction": "+x"}))),
            DuplicateBack => return sel.last().map(|id| ("duplicate_object", json!({"id": id, "direction": "+y"}))),
            FillPlate => return sel.last().map(|id| ("duplicate_object", json!({"id": id, "fill": true}))),
            Assemble | Union | Subtract | Intersect => {
                // the anchor (selected last, outlined) first: it is the main body, or what is cut
                let anchor = sel.last()?;
                let mut ids = vec![anchor.clone()];
                ids.extend(sel.iter().filter(|i| *i != anchor).cloned());
                let op = match self {
                    Assemble => "assemble",
                    Union => "union",
                    Subtract => "difference",
                    _ => "intersection",
                };
                return Some(("combine", json!({"op": op, "ids": ids})));
            }
            Explode => return sel.last().map(|id| ("combine", json!({"op": "explode", "ids": [id]}))),
            Copies | CopyTransform | PasteTransform => return None,
        };
        a["ids"] = json!(sel);
        if plate {
            a["anchor"] = json!("plate");
        }
        Some(("arrange", a))
    }

    /// A small schematic: the anchor as an outline, the moved objects filled.
    pub fn paint(self, p: &egui::Painter, r: Rect, col: Color32) {
        let c = r.center();
        let s = r.width().min(r.height()) / 16.0;
        let pt = |x: f32, y: f32| Pos2::new(c.x + x * s, c.y + y * s);
        let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(pt(x0, y0), pt(x1, y1));
        let st = Stroke::new(1.2, col);
        let fill = |x0, y0, x1, y1| p.rect_filled(rect(x0, y0, x1, y1), 1.0, col);
        let outline = |x0, y0, x1, y1| p.rect_stroke(rect(x0, y0, x1, y1), 1.0, st, egui::StrokeKind::Middle);
        let line = |a: (f32, f32), b: (f32, f32)| p.line_segment([pt(a.0, a.1), pt(b.0, b.1)], st);
        // screen up is the plate's back (−y on screen)
        match self {
            AlignLeft => {
                line((-6.0, -7.0), (-6.0, 7.0));
                fill(-6.0, -5.0, 4.0, -1.0);
                fill(-6.0, 1.0, 1.0, 5.0);
            }
            AlignCenterX => {
                line((0.0, -7.0), (0.0, 7.0));
                fill(-5.0, -5.0, 5.0, -1.0);
                fill(-3.0, 1.0, 3.0, 5.0);
            }
            AlignRight => {
                line((6.0, -7.0), (6.0, 7.0));
                fill(-4.0, -5.0, 6.0, -1.0);
                fill(-1.0, 1.0, 6.0, 5.0);
            }
            AlignBack => {
                line((-7.0, -6.0), (7.0, -6.0));
                fill(-5.0, -6.0, -1.0, 4.0);
                fill(1.0, -6.0, 5.0, 1.0);
            }
            AlignMiddleY => {
                line((-7.0, 0.0), (7.0, 0.0));
                fill(-5.0, -5.0, -1.0, 5.0);
                fill(1.0, -3.0, 5.0, 3.0);
            }
            AlignFront => {
                line((-7.0, 6.0), (7.0, 6.0));
                fill(-5.0, -4.0, -1.0, 6.0);
                fill(1.0, -1.0, 5.0, 6.0);
            }
            DockLeft => {
                outline(0.0, -4.0, 7.0, 4.0);
                fill(-7.0, -2.5, 0.0, 2.5);
            }
            DockRight => {
                outline(-7.0, -4.0, 0.0, 4.0);
                fill(0.0, -2.5, 7.0, 2.5);
            }
            DockBack => {
                outline(-4.0, 0.0, 4.0, 7.0);
                fill(-2.5, -7.0, 2.5, 0.0);
            }
            DockFront => {
                outline(-4.0, -7.0, 4.0, 0.0);
                fill(-2.5, 0.0, 2.5, 7.0);
            }
            DistributeX => {
                fill(-7.0, -3.0, -4.5, 3.0);
                fill(-1.25, -3.0, 1.25, 3.0);
                fill(4.5, -3.0, 7.0, 3.0);
            }
            DistributeY => {
                fill(-3.0, -7.0, 3.0, -4.5);
                fill(-3.0, -1.25, 3.0, 1.25);
                fill(-3.0, 4.5, 3.0, 7.0);
            }
            StackX => {
                fill(-7.0, -3.0, -2.5, 3.0);
                outline(-2.5, -3.0, 2.0, 3.0);
                fill(2.0, -3.0, 6.5, 3.0);
            }
            StackY => {
                fill(-3.0, -7.0, 3.0, -2.5);
                outline(-3.0, -2.5, 3.0, 2.0);
                fill(-3.0, 2.0, 3.0, 6.5);
            }
            Grid => {
                for (x, y) in [(-6.5, -6.5), (0.5, -6.5), (-6.5, 0.5), (0.5, 0.5)] {
                    fill(x, y, x + 6.0, y + 6.0);
                }
            }
            Circle => {
                p.circle_stroke(c, 5.0 * s, Stroke::new(1.0, col.gamma_multiply(0.5)));
                for k in 0..6 {
                    let a = k as f32 * std::f32::consts::TAU / 6.0;
                    p.circle_filled(pt(5.0 * a.cos(), 5.0 * a.sin()), 1.6 * s, col);
                }
            }
            Swap => {
                fill(-7.0, -2.5, -2.0, 2.5);
                outline(2.0, -2.5, 7.0, 2.5);
                line((-3.0, -5.5), (3.0, -5.5));
                line((1.5, -7.0), (3.0, -5.5));
                line((3.0, 5.5), (-3.0, 5.5));
                line((-1.5, 7.0), (-3.0, 5.5));
            }
            Snap => {
                for v in [-6.0f32, -2.0, 2.0, 6.0] {
                    p.line_segment([pt(v, -7.0), pt(v, 7.0)], Stroke::new(0.8, col.gamma_multiply(0.5)));
                    p.line_segment([pt(-7.0, v), pt(7.0, v)], Stroke::new(0.8, col.gamma_multiply(0.5)));
                }
                fill(-2.0, -2.0, 2.0, 2.0);
            }
            MatchWidth => {
                outline(-6.0, -6.0, 6.0, -1.0);
                fill(-6.0, 1.0, 6.0, 5.0);
            }
            MatchDepth => {
                outline(-6.0, -6.0, -1.0, 6.0);
                fill(1.0, -6.0, 5.0, 6.0);
            }
            MatchHeight => {
                line((-7.0, 6.0), (7.0, 6.0));
                outline(-6.0, -6.0, -1.0, 6.0);
                fill(1.0, -6.0, 6.0, 6.0);
            }
            MatchSize => {
                outline(-7.0, -7.0, 1.0, 1.0);
                fill(-1.0, -1.0, 7.0, 7.0);
            }
            StretchX => {
                outline(3.0, -6.0, 7.0, 6.0);
                fill(-7.0, -2.0, 7.0, 2.0);
            }
            StretchY => {
                outline(-6.0, -7.0, 6.0, -3.0);
                fill(-2.0, -7.0, 2.0, 7.0);
            }
            FillX => {
                outline(3.0, -6.0, 7.0, 6.0);
                fill(-7.0, -2.0, 3.0, 2.0);
            }
            FillY => {
                outline(-6.0, -7.0, 6.0, -3.0);
                fill(-2.0, -3.0, 2.0, 7.0);
            }
            DuplicateRight => {
                fill(-7.0, -3.0, -1.0, 3.0);
                outline(-1.0, -3.0, 5.0, 3.0);
                line((5.5, 0.0), (7.5, 0.0));
            }
            DuplicateBack => {
                fill(-3.0, 1.0, 3.0, 7.0);
                outline(-3.0, -5.0, 3.0, 1.0);
                line((0.0, -5.5), (0.0, -7.5));
            }
            Copies => {
                for (x, y) in [(-7.0, -7.0), (-1.5, -7.0), (4.0, -7.0), (-7.0, -1.5), (-1.5, -1.5), (4.0, -1.5)] {
                    if (x, y) == (-7.0, -7.0) {
                        fill(x, y, x + 4.0, y + 4.0);
                    } else {
                        outline(x, y, x + 4.0, y + 4.0);
                    }
                }
                line((-7.0, 5.5), (7.0, 5.5));
            }
            FillPlate => {
                outline(-7.5, -7.5, 7.5, 7.5);
                for (x, y) in [(-6.0, -6.0), (-1.5, -6.0), (3.0, -6.0), (-6.0, -1.5), (-1.5, -1.5), (3.0, -1.5), (-6.0, 3.0), (-1.5, 3.0), (3.0, 3.0)] {
                    fill(x, y, x + 3.0, y + 3.0);
                }
            }
            Assemble => {
                fill(-7.0, -3.0, -0.5, 3.5);
                p.rect_filled(rect(0.5, -3.0, 7.0, 3.5), 1.0, col.gamma_multiply(0.45));
                p.rect_stroke(rect(-7.5, -4.5, 7.5, 5.0), 2.0, Stroke::new(0.8, col.gamma_multiply(0.6)), egui::StrokeKind::Middle);
            }
            Union => {
                fill(-7.0, -5.0, 2.0, 3.0);
                fill(-2.0, -1.0, 7.0, 6.0);
            }
            Subtract => {
                // the anchor with a corner bitten out, the cutter outlined
                fill(-7.0, -5.0, 2.0, -1.0);
                fill(-7.0, -1.0, -2.0, 3.0);
                outline(-2.0, -1.0, 7.0, 6.0);
            }
            Intersect => {
                outline(-7.0, -5.0, 2.0, 3.0);
                outline(-2.0, -1.0, 7.0, 6.0);
                fill(-2.0, -1.0, 2.0, 3.0);
            }
            Explode => {
                fill(-7.0, -2.5, -2.5, 2.5);
                fill(2.5, -2.5, 7.0, 2.5);
                line((-1.0, -5.5), (-1.0, 5.5));
                line((1.0, -5.5), (1.0, 5.5));
            }
            CopyTransform => {
                outline(-6.0, -6.0, 2.0, 2.0);
                line((-2.0, -2.0), (5.0, 5.0));
                line((5.0, 5.0), (5.0, 1.5));
                line((5.0, 5.0), (1.5, 5.0));
            }
            PasteTransform => {
                fill(-2.0, -2.0, 6.0, 6.0);
                line((-7.0, -7.0), (-2.5, -2.5));
                line((-2.5, -2.5), (-2.5, -6.0));
                line((-2.5, -2.5), (-6.0, -2.5));
            }
        }
    }
}

impl crate::app::App {
    /// The Inspector's Arrange section: Efficient Elements-style buttons that act on the
    /// selection against the anchor (the last-selected object, outlined in the viewport; the
    /// plate when one object is selected, or with ⌥).
    pub(crate) fn arrange_section(&mut self, ui: &mut egui::Ui) {
        use crate::keymap::Action;
        use crate::theme::pal;
        let p = pal();
        let sel = self.snap.selection.clone();
        if sel.is_empty() || !self.section_header(ui, "inspector.arrange", "Arrange", true, false, |_| {}) {
            return;
        }
        let anchor_name = if sel.len() > 1 { sel.last().cloned() } else { None };
        let text = match &anchor_name {
            Some(a) => format!("Against {a} (selected last, outlined) · {}-click: the plate", crate::keymap::ALT),
            None => "Against the plate (the area both nozzles reach)".to_string(),
        };
        ui.label(egui::RichText::new(text).size(11.0).color(p.placeholder));
        ui.add_space(2.0);
        let alt = ui.input(|i| i.modifiers.alt);
        let mut fire: Option<(ArrangeOp, bool)> = None;
        for (row, ops) in ArrangeOp::ROWS {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.add_sized([44.0, 22.0], egui::Label::new(egui::RichText::new(row).size(10.5).color(p.placeholder)).selectable(false));
                for op in ops.iter().copied() {
                    let n = sel.len();
                    let ok = match op {
                        Swap => n == 2,
                        DistributeX | DistributeY => n >= 3,
                        StackX | StackY | Grid => n >= 2,
                        CopyTransform => n == 1,
                        Assemble | Union | Subtract | Intersect => n >= 2,
                        Explode => n == 1 && self.snap.project.object(&sel[0]).is_some_and(|o| !o.parts.is_empty()),
                        PasteTransform => self.transform_source.as_ref().is_some_and(|s| self.snap.project.object(s).is_some()),
                        o if o.needs_anchor_object() => n >= 2,
                        _ => true,
                    };
                    let (r, resp) = ui.allocate_exact_size(egui::vec2(26.0, 22.0), if ok { egui::Sense::click() } else { egui::Sense::hover() });
                    if resp.hovered() && ok {
                        ui.painter().rect_filled(r, 4.0, p.hover);
                    }
                    op.paint(ui.painter(), r.shrink(3.0), if ok { p.text.gamma_multiply(0.85) } else { p.placeholder.gamma_multiply(0.5) });
                    let key = self.keymap.label_for(Action::Op(op)).map(|k| format!("  {k}")).unwrap_or_default();
                    let why = if ok { "" } else { "\n(not for this selection)" };
                    if resp.on_hover_text(format!("{}{key}{why}", op.label())).clicked() {
                        fire = Some((op, alt && !op.needs_anchor_object()));
                    }
                }
            });
        }
        if let Some((op, plate)) = fire {
            if plate {
                if let Some(c) = op.call(&sel, true) {
                    self.run_calls(vec![(c.0.to_string(), c.1)], crate::worker::Kind::Plain);
                }
            } else {
                self.run(Action::Op(op));
            }
        }
    }

    /// Copy and paste transform: remember an object, then give the selection its place, turn and
    /// scale (e.g. put v2 of a part exactly where v1 was).
    pub(crate) fn transform_clip(&mut self, op: ArrangeOp) {
        let sel = self.snap.selection.clone();
        match op {
            Copies => {
                if let Some(id) = sel.last() {
                    self.dialog = Some(crate::app::Dialog::Copies(crate::design_ui::CopiesForm::new(id)));
                }
            }
            CopyTransform => {
                if let Some(id) = sel.last() {
                    self.transform_source = Some(id.clone());
                    self.toast(format!("Copied the place of {id}: select other objects and paste it"), false);
                }
            }
            PasteTransform => {
                let Some(src) = self.transform_source.clone() else { return };
                let ids: Vec<String> = sel.into_iter().filter(|i| *i != src).collect();
                if !ids.is_empty() {
                    self.run_calls(vec![("arrange".to_string(), json!({"op": "copy_transform", "anchor": src, "ids": ids}))], crate::worker::Kind::Plain);
                }
            }
            _ => {}
        }
    }
}
