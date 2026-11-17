//! What an [`Action`] does to the project: a pure mapping from (action, current state) to tool
//! calls, so the dispatch table is testable without a window. Actions that only change the UI
//! (views, docks, dialogs) return [`Plan::Ui`] and are handled by the app.

use std::collections::{BTreeSet, HashMap};

use only3d_model::{Object, Project};
use serde_json::{json, Value};

use crate::keymap::Action;

/// The viewport's manipulation tool (Bambu's M / R / S gizmos).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Gizmo {
    #[default]
    Select,
    Move,
    Rotate,
    Scale,
    /// Pull or push a flat face (SketchUp's Push/Pull, Fusion's Press Pull).
    PushPull,
}

impl Gizmo {
    pub fn label(self) -> &'static str {
        match self {
            Gizmo::Select => "Select",
            Gizmo::Move => "Move",
            Gizmo::Rotate => "Rotate",
            Gizmo::Scale => "Scale",
            Gizmo::PushPull => "Push/Pull a flat face",
        }
    }
}

/// The state an action is resolved against.
pub struct Ctx<'a> {
    pub project: &'a Project,
    /// Footprint centre of each evaluated object (used when an object has no explicit position).
    pub centers: &'a HashMap<String, [f32; 2]>,
    pub selection: &'a [String],
    /// The active plate (a selection lives on one plate).
    pub plate: u32,
    /// Toolpath preview is showing.
    pub preview: bool,
    pub gizmo: Gizmo,
    /// Plate axes closest to the screen's right and up directions (for arrow-key nudges).
    pub screen_right: [f32; 2],
    pub screen_up: [f32; 2],
    pub can_undo: (bool, bool),
    /// Objects on the in-app clipboard.
    pub clipboard: &'a [Object],
}

#[derive(Debug, PartialEq)]
pub enum Plan {
    /// Tool calls; more than one is sent as a single `batch` (one undo step).
    Calls(Vec<(String, Value)>),
    /// Change the selection.
    Select(Vec<String>),
    /// Handled by the UI itself.
    Ui,
    /// Does nothing right now (the key press is left to other widgets).
    NotApplicable,
}

fn calls(v: Vec<(&str, Value)>) -> Plan {
    if v.is_empty() {
        Plan::NotApplicable
    } else {
        Plan::Calls(v.into_iter().map(|(t, a)| (t.to_string(), a)).collect())
    }
}

fn round2(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

/// f32 → f64 rounded to `places` decimals (so JSON shows 2.1, not 2.0999999046325684).
pub fn jf(v: f32, places: i32) -> f64 {
    let k = 10f64.powi(places);
    ((v as f64) * k).round() / k
}

/// Wrap degrees to (-180, 180].
pub fn wrap_deg(a: f32) -> f32 {
    let mut a = a % 360.0;
    if a <= -180.0 {
        a += 360.0;
    } else if a > 180.0 {
        a -= 360.0;
    }
    round2(a)
}

impl Ctx<'_> {
    fn selected(&self) -> Vec<&Object> {
        self.project.objects.iter().filter(|o| self.selection.contains(&o.id)).collect()
    }

    pub fn position_of(&self, o: &Object) -> [f32; 2] {
        o.position.or_else(|| self.centers.get(&o.id).copied()).unwrap_or([0.0, 0.0])
    }

    fn nudge(&self, dx: f32, dy: f32) -> Plan {
        calls(
            self.selected()
                .into_iter()
                .filter(|o| !o.locked)
                .map(|o| {
                    let p = self.position_of(o);
                    ("update_object", json!({ "id": o.id, "position": [jf(p[0] + dx, 2), jf(p[1] + dy, 2)] }))
                })
                .collect(),
        )
    }

    fn rotate(&self, deg: f32) -> Plan {
        calls(
            self.selected()
                .into_iter()
                .map(|o| {
                    let r = o.rotation;
                    ("update_object", json!({ "id": o.id, "rotation": [jf(r[0], 2), jf(r[1], 2), jf(wrap_deg(r[2] + deg), 2)] }))
                })
                .collect(),
        )
    }

    fn scale(&self, f: f32) -> Plan {
        calls(
            self.selected()
                .into_iter()
                .map(|o| {
                    let s = o.scale.map(|x| jf(x * f, 4));
                    ("update_object", json!({ "id": o.id, "scale": s }))
                })
                .collect(),
        )
    }

    /// Arrow keys: translate in screen-aligned plate axes, or rotate / scale in those tools.
    fn arrow(&self, right: f32, up: f32, big: bool) -> Plan {
        if self.preview || self.selection.is_empty() {
            return Plan::NotApplicable;
        }
        match self.gizmo {
            Gizmo::Rotate => {
                if right == 0.0 {
                    return Plan::NotApplicable;
                }
                self.rotate(-right * if big { 90.0 } else { 15.0 })
            }
            Gizmo::Scale => {
                if up == 0.0 {
                    return Plan::NotApplicable;
                }
                let f = if big { 1.25 } else { 1.05 };
                self.scale(if up > 0.0 { f } else { 1.0 / f })
            }
            Gizmo::Select | Gizmo::Move | Gizmo::PushPull => {
                let step = if big { 10.0 } else { 1.0 };
                let d = [(self.screen_right[0] * right + self.screen_up[0] * up) * step, (self.screen_right[1] * right + self.screen_up[1] * up) * step];
                self.nudge(d[0], d[1])
            }
        }
    }

    /// add_object (+ update_object for scale/settings/visibility) calls that recreate the
    /// clipboard objects with fresh ids, offset by 10 mm so the copies are visible.
    pub fn paste_calls(&self, objects: &[Object]) -> Vec<(String, Value)> {
        let mut taken: BTreeSet<String> = self.project.objects.iter().map(|o| o.id.clone()).collect();
        let mut out = Vec::new();
        for o in objects {
            let id = unique_id(&o.id, &taken);
            taken.insert(id.clone());
            let mut add = json!({ "id": id, "shape": o.shape, "rotation": o.rotation, "filament": o.filament.clamp(1, self.project.filaments.len().max(1) as u32) });
            if !o.name.is_empty() {
                add["name"] = json!(o.name);
            }
            if let Some(p) = o.position {
                add["position"] = json!([jf(p[0] + 10.0, 2), jf(p[1] + 10.0, 2)]);
            }
            out.push(("add_object".to_string(), add));
            if o.scale != [1.0; 3] || !o.settings.is_empty() || o.hidden {
                let mut up = json!({ "id": id });
                if o.scale != [1.0; 3] {
                    up["scale"] = json!(o.scale);
                }
                if !o.settings.is_empty() {
                    up["settings"] = json!(o.settings);
                }
                if o.hidden {
                    up["hidden"] = json!(true);
                }
                out.push(("update_object".to_string(), up));
            }
        }
        out
    }
}

/// `base`, `base_2`, `base_3`… (strips an existing `_n` suffix first).
pub fn unique_id(base: &str, taken: &BTreeSet<String>) -> String {
    let stem = match base.rsplit_once('_') {
        Some((s, n)) if !s.is_empty() && n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty() => s,
        _ => base,
    };
    let stem: String = stem.chars().filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-').collect();
    let stem = if stem.is_empty() { "object".to_string() } else { stem };
    if !taken.contains(&stem) {
        return stem;
    }
    (2..).map(|n| format!("{stem}_{n}")).find(|c| !taken.contains(c)).expect("ids")
}

pub fn plan(action: Action, cx: &Ctx) -> Plan {
    use Action::*;
    let sel: Vec<String> = cx.selection.iter().filter(|id| cx.project.object(id).is_some()).cloned().collect();
    let all: Vec<String> = cx.project.objects.iter().map(|o| o.id.clone()).collect();
    match action {
        Undo => {
            if cx.can_undo.0 {
                calls(vec![("undo", json!({}))])
            } else {
                Plan::NotApplicable
            }
        }
        Redo => {
            if cx.can_undo.1 {
                calls(vec![("redo", json!({}))])
            } else {
                Plan::NotApplicable
            }
        }
        New => calls(vec![("new_project", json!({}))]),
        // everything on the active plate (as in Bambu Studio); Delete all covers every plate
        SelectAll => {
            let here: Vec<String> = cx.project.objects.iter().filter(|o| o.plate == cx.plate && !o.hidden).map(|o| o.id.clone()).collect();
            if here.is_empty() {
                Plan::NotApplicable
            } else {
                Plan::Select(here)
            }
        }
        Deselect => {
            if cx.gizmo != Gizmo::Select {
                Plan::Ui
            } else if sel.is_empty() {
                Plan::NotApplicable
            } else {
                Plan::Select(Vec::new())
            }
        }
        Delete => {
            if sel.is_empty() {
                Plan::NotApplicable
            } else {
                calls(vec![("remove_object", json!({ "ids": sel }))])
            }
        }
        DeleteAll => {
            if all.is_empty() {
                Plan::NotApplicable
            } else {
                calls(vec![("remove_object", json!({ "ids": all }))])
            }
        }
        Duplicate => calls(sel.iter().map(|id| ("duplicate_object", json!({ "id": id }))).collect()),
        ToggleHidden => calls(cx.selected().into_iter().map(|o| ("update_object", json!({ "id": o.id, "hidden": !o.hidden }))).collect()),
        SetFilament(n) => {
            if n == 0 || n as usize > cx.project.filaments.len() || sel.is_empty() {
                Plan::NotApplicable
            } else {
                calls(cx.selected().into_iter().filter(|o| o.filament != n as u32).map(|o| ("update_object", json!({ "id": o.id, "filament": n }))).collect())
            }
        }
        Op(o) => match o {
            crate::arrange_ops::ArrangeOp::CopyTransform | crate::arrange_ops::ArrangeOp::PasteTransform | crate::arrange_ops::ArrangeOp::Copies => {
                if sel.is_empty() {
                    Plan::NotApplicable
                } else {
                    Plan::Ui
                }
            }
            crate::arrange_ops::ArrangeOp::Assemble | crate::arrange_ops::ArrangeOp::Union | crate::arrange_ops::ArrangeOp::Subtract | crate::arrange_ops::ArrangeOp::Intersect if sel.len() < 2 => Plan::NotApplicable,
            crate::arrange_ops::ArrangeOp::Explode if !(sel.len() == 1 && cx.selected().first().is_some_and(|o| !o.parts.is_empty())) => Plan::NotApplicable,
            _ => match o.call(&sel, false) {
                Some(c) if !sel.is_empty() => calls(vec![c]),
                _ => Plan::NotApplicable,
            },
        },
        Arrange => {
            if all.is_empty() {
                Plan::NotApplicable
            } else {
                calls(vec![("arrange", json!({}))])
            }
        }
        Orient => {
            if all.is_empty() {
                Plan::NotApplicable
            } else if sel.is_empty() {
                calls(vec![("orient", json!({}))])
            } else {
                calls(vec![("orient", json!({ "ids": sel }))])
            }
        }
        PlaceOnFace => {
            if sel.is_empty() {
                Plan::NotApplicable
            } else {
                calls(vec![("orient", json!({ "ids": sel }))])
            }
        }
        MoveLeft => cx.arrow(-1.0, 0.0, false),
        MoveRight => cx.arrow(1.0, 0.0, false),
        MoveUp => cx.arrow(0.0, 1.0, false),
        MoveDown => cx.arrow(0.0, -1.0, false),
        MoveLeftBig => cx.arrow(-1.0, 0.0, true),
        MoveRightBig => cx.arrow(1.0, 0.0, true),
        MoveUpBig => cx.arrow(0.0, 1.0, true),
        MoveDownBig => cx.arrow(0.0, -1.0, true),
        RotateCcw => {
            if cx.preview {
                Plan::NotApplicable
            } else {
                cx.rotate(15.0)
            }
        }
        RotateCw => {
            if cx.preview {
                Plan::NotApplicable
            } else {
                cx.rotate(-15.0)
            }
        }
        ScaleUp => cx.scale(1.05),
        ScaleDown => cx.scale(1.0 / 1.05),
        Paste => {
            if cx.clipboard.is_empty() {
                Plan::NotApplicable
            } else {
                Plan::Calls(cx.paste_calls(cx.clipboard))
            }
        }
        Copy | Cut | Rename => {
            if sel.is_empty() {
                Plan::NotApplicable
            } else {
                Plan::Ui
            }
        }
        ToolMove | ToolRotate | ToolScale | ToolSelect | ToolPushPull => {
            if cx.preview {
                Plan::NotApplicable
            } else {
                Plan::Ui
            }
        }
        LayerUp | LayerDown | LayerUpFast | LayerDownFast | OneLayer => {
            if cx.preview {
                Plan::Ui
            } else {
                Plan::NotApplicable
            }
        }
        ToggleLock => {
            let objs = cx.selected();
            let lock = !objs.iter().all(|o| o.locked);
            calls(objs.iter().map(|o| ("update_object", json!({ "id": o.id, "locked": lock }))).collect())
        }
        RepairMesh => calls(
            cx.selected()
                .into_iter()
                .filter(|o| crate::context_menu::has_import(&o.shape))
                .map(|o| ("update_object", json!({ "id": o.id, "shape": crate::context_menu::with_repair(&o.shape) })))
                .collect(),
        ),
        ObjectSettings => {
            if sel.len() == 1 {
                Plan::Ui
            } else {
                Plan::NotApplicable
            }
        }
        PlateNew => calls(vec![("plate", json!({ "action": "add" })), ("plate", json!({ "action": "select", "plate": cx.project.plate_count() + 1 }))]),
        PlateDuplicate => calls(vec![("plate", json!({ "action": "duplicate", "plate": cx.plate }))]),
        PlateNext if cx.plate < cx.project.plate_count() => calls(vec![("plate", json!({ "action": "select", "plate": cx.plate + 1 }))]),
        PlatePrev if cx.plate > 1 => calls(vec![("plate", json!({ "action": "select", "plate": cx.plate - 1 }))]),
        PlateNext | PlatePrev => Plan::NotApplicable,
        PlateDelete => {
            if cx.project.plate_count() > 1 {
                Plan::Ui
            } else {
                Plan::NotApplicable
            }
        }
        _ => Plan::Ui,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> Project {
        let mut p = Project::default();
        let shape: only3d_model::Node = serde_json::from_value(json!({"type": "box", "size": [10, 10, 10]})).unwrap();
        let mut a = Object::new("a", shape.clone());
        a.position = Some([100.0, 100.0]);
        let mut b = Object::new("b", shape.clone());
        b.rotation = [0.0, 0.0, 170.0];
        b.scale = [2.0, 2.0, 2.0];
        let c = Object::new("box_3", shape);
        p.objects = vec![a, b, c];
        if p.filaments.len() < 2 {
            p.filaments.push(p.filaments[0].clone());
        }
        p
    }

    struct Fx {
        p: Project,
        centers: HashMap<String, [f32; 2]>,
        sel: Vec<String>,
        clip: Vec<Object>,
    }

    impl Fx {
        fn new(sel: &[&str]) -> Fx {
            let mut centers = HashMap::new();
            centers.insert("b".to_string(), [50.0, 60.0]);
            Fx { p: project(), centers, sel: sel.iter().map(|s| s.to_string()).collect(), clip: Vec::new() }
        }
        fn cx(&self, gizmo: Gizmo, preview: bool) -> Ctx<'_> {
            Ctx { project: &self.p, centers: &self.centers, selection: &self.sel, plate: 1, preview, gizmo, screen_right: [1.0, 0.0], screen_up: [0.0, 1.0], can_undo: (true, false), clipboard: &self.clip }
        }
    }

    fn one(p: Plan) -> (String, Value) {
        match p {
            Plan::Calls(mut v) if v.len() == 1 => v.remove(0),
            other => panic!("expected one call, got {other:?}"),
        }
    }

    #[test]
    fn selection_actions() {
        let f = Fx::new(&["a"]);
        assert_eq!(plan(Action::SelectAll, &f.cx(Gizmo::Select, false)), Plan::Select(vec!["a".into(), "b".into(), "box_3".into()]));
        // only the active plate's objects: the session keeps a selection on one plate
        let mut g = Fx::new(&["a"]);
        g.p.objects[1].plate = 2;
        assert_eq!(plan(Action::SelectAll, &g.cx(Gizmo::Select, false)), Plan::Select(vec!["a".into(), "box_3".into()]));
        assert_eq!(plan(Action::Deselect, &f.cx(Gizmo::Select, false)), Plan::Select(vec![]));
        // Esc in a tool first leaves the tool
        assert_eq!(plan(Action::Deselect, &f.cx(Gizmo::Rotate, false)), Plan::Ui);
        let none = Fx::new(&[]);
        assert_eq!(plan(Action::Deselect, &none.cx(Gizmo::Select, false)), Plan::NotApplicable);
        assert_eq!(plan(Action::Delete, &none.cx(Gizmo::Select, false)), Plan::NotApplicable);
    }

    #[test]
    fn delete_duplicate_filament() {
        let f = Fx::new(&["a", "b", "ghost"]);
        let cx = f.cx(Gizmo::Select, false);
        assert_eq!(one(plan(Action::Delete, &cx)), ("remove_object".into(), json!({"ids": ["a", "b"]})));
        assert_eq!(one(plan(Action::DeleteAll, &cx)), ("remove_object".into(), json!({"ids": ["a", "b", "box_3"]})));
        match plan(Action::Duplicate, &cx) {
            Plan::Calls(v) => assert_eq!(v.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(), ["duplicate_object", "duplicate_object"]),
            p => panic!("{p:?}"),
        }
        match plan(Action::SetFilament(2), &cx) {
            Plan::Calls(v) => assert_eq!(v, vec![("update_object".to_string(), json!({"id": "a", "filament": 2})), ("update_object".to_string(), json!({"id": "b", "filament": 2}))]),
            p => panic!("{p:?}"),
        }
        assert_eq!(plan(Action::SetFilament(7), &cx), Plan::NotApplicable);
        assert_eq!(one(plan(Action::Orient, &cx)), ("orient".into(), json!({"ids": ["a", "b"]})));
        assert_eq!(one(plan(Action::Orient, &Fx::new(&[]).cx(Gizmo::Select, false))), ("orient".into(), json!({})));
        assert_eq!(one(plan(Action::Undo, &cx)), ("undo".into(), json!({})));
        assert_eq!(plan(Action::Redo, &cx), Plan::NotApplicable);
    }

    #[test]
    fn arrows_nudge_rotate_scale() {
        let f = Fx::new(&["a", "b"]);
        // move: a has an explicit position, b uses its evaluated centre
        match plan(Action::MoveRight, &f.cx(Gizmo::Select, false)) {
            Plan::Calls(v) => {
                assert_eq!(v[0].1, json!({"id": "a", "position": [101.0, 100.0]}));
                assert_eq!(v[1].1, json!({"id": "b", "position": [51.0, 60.0]}));
            }
            p => panic!("{p:?}"),
        }
        match plan(Action::MoveDownBig, &f.cx(Gizmo::Move, false)) {
            Plan::Calls(v) => assert_eq!(v[0].1, json!({"id": "a", "position": [100.0, 90.0]})),
            p => panic!("{p:?}"),
        }
        // rotate tool: left = counter-clockwise 15°, wraps past 180
        match plan(Action::MoveLeft, &f.cx(Gizmo::Rotate, false)) {
            Plan::Calls(v) => {
                assert_eq!(v[0].1, json!({"id": "a", "rotation": [0.0, 0.0, 15.0]}));
                assert_eq!(v[1].1, json!({"id": "b", "rotation": [0.0, 0.0, -175.0]}));
            }
            p => panic!("{p:?}"),
        }
        assert_eq!(plan(Action::MoveUp, &f.cx(Gizmo::Rotate, false)), Plan::NotApplicable);
        // scale tool: up = ×1.05
        match plan(Action::MoveUp, &f.cx(Gizmo::Scale, false)) {
            Plan::Calls(v) => assert_eq!(v[1].1, json!({"id": "b", "scale": [2.1, 2.1, 2.1]})),
            p => panic!("{p:?}"),
        }
        // preview: arrows are for the layer slider
        assert_eq!(plan(Action::MoveLeft, &f.cx(Gizmo::Select, true)), Plan::NotApplicable);
        assert_eq!(plan(Action::LayerUp, &f.cx(Gizmo::Select, true)), Plan::Ui);
        assert_eq!(plan(Action::LayerUp, &f.cx(Gizmo::Select, false)), Plan::NotApplicable);
    }

    #[test]
    fn paste_makes_unique_ids() {
        let mut f = Fx::new(&[]);
        f.clip = vec![f.p.objects[0].clone(), f.p.objects[1].clone(), f.p.objects[2].clone()];
        match plan(Action::Paste, &f.cx(Gizmo::Select, false)) {
            Plan::Calls(v) => {
                let adds: Vec<&Value> = v.iter().filter(|c| c.0 == "add_object").map(|c| &c.1).collect();
                assert_eq!(adds.iter().map(|a| a["id"].as_str().unwrap()).collect::<Vec<_>>(), ["a_2", "b_2", "box"]);
                assert_eq!(adds[0]["position"], json!([110.0, 110.0]));
                assert!(adds[1].get("position").is_none());
                // b was scaled: a follow-up update_object carries the scale
                assert!(v.iter().any(|c| c.0 == "update_object" && c.1["id"] == "b_2" && c.1["scale"] == json!([2.0, 2.0, 2.0])));
            }
            p => panic!("{p:?}"),
        }
        assert_eq!(plan(Action::Paste, &Fx::new(&[]).cx(Gizmo::Select, false)), Plan::NotApplicable);
    }

    #[test]
    fn every_action_has_a_plan() {
        let f = Fx::new(&["a"]);
        for a in Action::all() {
            // must not panic, and object-editing actions with a selection must produce calls
            let p = plan(a, &f.cx(Gizmo::Select, false));
            if matches!(a, Action::Delete | Action::Duplicate | Action::Arrange | Action::Orient | Action::PlaceOnFace | Action::MoveLeft | Action::RotateCw | Action::ScaleUp | Action::ToggleHidden | Action::SetFilament(2)) {
                assert!(matches!(p, Plan::Calls(_)), "{a:?} → {p:?}");
            }
        }
    }

    #[test]
    fn ids_and_angles() {
        let taken: BTreeSet<String> = ["box", "box_2", "a"].iter().map(|s| s.to_string()).collect();
        assert_eq!(unique_id("box", &taken), "box_3");
        assert_eq!(unique_id("box_2", &taken), "box_3");
        assert_eq!(unique_id("cup", &taken), "cup");
        assert_eq!(unique_id("my part!", &taken), "mypart");
        assert_eq!(wrap_deg(190.0), -170.0);
        assert_eq!(wrap_deg(-180.0), 180.0);
        assert_eq!(wrap_deg(360.0 + 15.0), 15.0);
    }
}
