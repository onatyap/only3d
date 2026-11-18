#![allow(clippy::too_many_arguments, clippy::type_complexity, clippy::needless_range_loop, clippy::unnecessary_sort_by)]
//! only3d document model.
//!
//! A project is a JSON document (`*.only3d.json`): named parameters, a printer/process/filament
//! selection and objects whose geometry is a parametric shape tree ([`shape::Node`]). Evaluation
//! is memoised and booleans run on a robust pure-Rust kernel, so agents can edit a number and
//! get a new watertight mesh back in milliseconds.

pub mod csg;
pub mod expr;
pub mod proc;
pub mod project;
pub mod scad;
pub mod shape;

pub use expr::{Num, Params};
pub use project::{best_orientation, check, new_project_id, Bed, EvaluatedObject, EvaluatedPart, FilamentSlot, Finding, NozzleSel, Object, Part, PlateInfo, Project, Severity, Spec};
pub use shape::{Evaluator, Node, Shape};

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("{path}: {source}")]
    At { path: String, #[source] source: Box<ModelError> },
    #[error("expression '{expr}': {message}")]
    Expr { expr: String, message: String },
    #[error("shape: {0}")]
    Shape(String),
    #[error("boolean failed: {message} (hint: {hint})")]
    Csg { message: String, hint: String },
    #[error("import {path}: {message}")]
    Import { path: String, message: String },
}

impl ModelError {
    /// Stable error code for agents.
    pub fn code(&self) -> &'static str {
        match self {
            ModelError::At { source, .. } => source.code(),
            ModelError::Expr { .. } => "bad_expression",
            ModelError::Shape(_) => "bad_shape",
            ModelError::Csg { .. } => "boolean_failed",
            ModelError::Import { .. } => "import_failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bracket_json() -> &'static str {
        r#"{
          "type": "difference",
          "children": [
            {"type": "box", "size": [60, 30, "wall"], "radius": 2},
            {"type": "array", "count": [2, 1, 1], "spacing": [40, 0, 0], "at": [-20, 0, -1],
             "child": {"type": "cylinder", "d": 5.4, "h": 20, "chamfer_top": 0.5}},
            {"type": "cylinder", "d": 12, "h": 20, "at": [0, 0, 2]}
          ]
        }"#
    }

    #[test]
    fn push_pull_moves_one_flat_face() {
        let mut ev = Evaluator::new(".");
        let p = Params::new();
        let pp = |at: [f32; 3], normal: [f32; 3], d: f64| -> Node {
            serde_json::from_value(serde_json::json!({"type": "push_pull", "child": {"type": "box", "size": [20, 10, 5]}, "point": at, "normal": normal, "distance": d})).unwrap()
        };
        let check = |ev: &mut Evaluator, n: Node, size: [f32; 3], vol: f64| {
            let r = ev.eval(&n, &p).unwrap().analyze();
            assert!(r.watertight, "{r:?}");
            assert!((r.size.x - size[0]).abs() < 0.02 && (r.size.y - size[1]).abs() < 0.02 && (r.size.z - size[2]).abs() < 0.02, "{:?}", r.size);
            assert!((r.volume_mm3 - vol).abs() < 1.0, "{} vs {vol}", r.volume_mm3);
            assert_eq!(r.components, 1);
        };
        // the top up 3, down 2; the +X side out 5
        check(&mut ev, pp([2.0, 1.0, 5.0], [0.0, 0.0, 1.0], 3.0), [20.0, 10.0, 8.0], 1600.0);
        check(&mut ev, pp([2.0, 1.0, 5.0], [0.0, 0.0, 1.0], -2.0), [20.0, 10.0, 3.0], 600.0);
        check(&mut ev, pp([10.0, 0.0, 2.0], [1.0, 0.0, 0.0], 5.0), [25.0, 10.0, 5.0], 1250.0);
        // a step: pulling the lower top of an L leaves the tall part alone
        let l: Node = serde_json::from_value(serde_json::json!({"type": "push_pull", "child": {"type": "union", "children": [{"type": "box", "size": [20, 10, 5]}, {"type": "box", "size": [10, 10, 15], "at": [5, 0, 0]}]}, "point": [-5.0, 0.0, 5.0], "normal": [0.0, 0.0, 1.0], "distance": 4})).unwrap();
        check(&mut ev, l, [20.0, 10.0, 15.0], 20.0 * 10.0 * 5.0 + 10.0 * 10.0 * 10.0 + 10.0 * 10.0 * 4.0);
        // the face is gone: a clear error
        let e = ev.eval(&pp([2.0, 1.0, 9.0], [0.0, 0.0, 1.0], 3.0), &p).unwrap_err();
        assert!(e.to_string().contains("no flat face"), "{e}");
    }

    #[test]
    fn evaluates_parametric_bracket() {
        let node: Node = serde_json::from_str(bracket_json()).unwrap();
        let mut p = Params::new();
        p.insert("wall".into(), 5.0);
        let mut ev = Evaluator::new(".");
        let t = std::time::Instant::now();
        let m = ev.eval(&node, &p).unwrap();
        let r = m.analyze();
        eprintln!("bracket: {} tris in {:?}, {:?}", r.triangles, t.elapsed(), ev.stats);
        assert!(r.watertight, "{r:?}");
        assert!((r.size.x - 60.0).abs() < 1e-3 && (r.size.z - 5.0).abs() < 1e-3);
        // volume: box minus two through holes, minus a 3 mm deep counterbore
        assert!(r.volume_mm3 < 60.0 * 30.0 * 5.0 && r.volume_mm3 > 60.0 * 30.0 * 5.0 - 1000.0, "{}", r.volume_mm3);
        // cache hit on re-eval
        let hits = ev.stats.cache_hits;
        ev.eval(&node, &p).unwrap();
        assert!(ev.stats.cache_hits > hits);
    }

    #[test]
    fn project_roundtrip_arrange_and_check() {
        let mut proj = Project::default();
        let n: Node = serde_json::from_str(bracket_json()).unwrap();
        proj.params.insert("wall".into(), 5.0);
        for i in 0..4 {
            let id = proj.unique_id("bracket");
            assert!(i == 0 || id.ends_with(&format!("_{}", i + 1)));
            proj.objects.push(Object::new(id, n.clone()));
        }
        let mut ev = Evaluator::new(".");
        let bed = Bed::h2d();
        let failed = proj.arrange(&mut ev, &bed, 6.0, 1).unwrap();
        assert!(failed.is_empty());
        let objs = proj.evaluate(&mut ev, &bed).unwrap();
        let findings = check(&proj, &objs, &bed);
        assert!(findings.iter().all(|f| f.severity != Severity::Error), "{findings:#?}");
        let s = serde_json::to_string_pretty(&proj).unwrap();
        let back: Project = serde_json::from_str(&s).unwrap();
        assert_eq!(back, proj);
    }

    #[test]
    fn filleted_knob_with_polar_holes_is_watertight() {
        let node: Node = serde_json::from_str(r#"{"type":"difference","children":[
            {"type":"cylinder","d":36,"h":18,"fillet_top":3,"chamfer_bottom":0.6},
            {"type":"polar","count":12,"child":{"type":"cylinder","d":5,"h":40,"at":[19,0,-1]}},
            {"type":"cylinder","d":6.2,"h":14,"at":[0,0,-1]}]}"#).unwrap();
        let m = Evaluator::new(".").eval(&node, &Params::new()).unwrap();
        let r = m.analyze();
        assert!(r.watertight, "{r:?}");
    }

    #[test]
    fn orientation_lays_a_plate_flat() {
        let node: Node = serde_json::from_str(r#"{"type":"box","size":[40,4,30]}"#).unwrap();
        let mut ev = Evaluator::new(".");
        let m = ev.eval(&node, &Params::new()).unwrap();
        // standing up has a small footprint; lying flat is better
        let (best, _) = best_orientation(&m, [0.0; 3]);
        let rotated = m.transformed(&glam::Mat4::from_quat(shape::euler_xyz(glam::Vec3::from(best.rotation))));
        assert!(rotated.bounds().size().z < 5.0, "{best:?}");
    }
}
