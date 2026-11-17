//! Combining objects, like Bambu Studio's assemble and boolean tools:
//! - `assemble` keeps every body, each in its own filament, as the parts of one object (they move
//!   and print together);
//! - `union`, `difference` (the first minus the rest) and `intersection` make one solid;
//! - `explode` turns an assembled object's parts back into objects.
//!
//! Nothing moves: each source's turn, scale and place on the plate are baked into the new shape
//! tree, so the result sits exactly where its sources were and stays editable.

use glam::Vec3;
use only3d_model::{Node, Object, Part, Shape};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::tools::{def, Registry, ToolError, ToolOutput};
use crate::Studio;

#[derive(Deserialize, JsonSchema)]
pub struct CombineIn {
    /// assemble | union | difference (first id minus the rest) | intersection | explode (one id)
    pub op: String,
    pub ids: Vec<String>,
    /// New object's id (default: the first id)
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// One body of an object: its name, filament and shape in plate coordinates.
struct Body {
    name: String,
    filament: u32,
    node: Node,
}

/// Every body of `o` with the object's turn, scale and place on the plate baked in.
fn baked(s: &mut Studio, o: &Object) -> Result<Vec<Body>, ToolError> {
    let objs = s.evaluated()?;
    let world = objs.iter().find(|e| e.id == o.id).ok_or_else(|| ToolError::new("not_found", o.id.clone(), ""))?.report.bounds;
    let xf = o.local_transform();
    let mut bodies = vec![(if o.name.is_empty() { o.id.clone() } else { o.name.clone() }, o.filament, o.shape.clone())];
    for (k, p) in o.parts.iter().enumerate() {
        bodies.push((if p.name.is_empty() { format!("{} part {}", o.id, k + 1) } else { p.name.clone() }, p.filament, p.shape.clone()));
    }
    let mut lb = only3d_geom::Aabb::EMPTY;
    for (_, _, shape) in &bodies {
        let b = s.evaluator.eval(shape, &s.project.params)?.transformed(&xf).bounds();
        if b.min.x.is_finite() {
            lb.extend(b.min);
            lb.extend(b.max);
        }
    }
    let shift: Vec3 = world.min - lb.min;
    let turned = o.rotation.iter().any(|v| *v != 0.0);
    let scaled = o.scale.iter().any(|v| *v != 1.0);
    Ok(bodies
        .into_iter()
        .map(|(name, filament, shape)| Body {
            node: Node {
                shape: Shape::Union { children: vec![shape] },
                at: Some([shift.x, shift.y, shift.z].map(|v| ((v as f64 * 1e4).round() / 1e4).into())),
                rotate: turned.then(|| o.rotation.map(|v| (v as f64).into())),
                scale: scaled.then(|| o.scale.map(|v| (v as f64).into())),
                mirror: None,
                label: Some(name.clone()),
            },
            name,
            filament,
        })
        .collect())
}

fn one(nodes: Vec<Node>) -> Node {
    if nodes.len() == 1 {
        nodes.into_iter().next().unwrap()
    } else {
        Node::from(Shape::Union { children: nodes })
    }
}

/// The plate position that leaves a plate-coordinate shape where it is (its footprint centre).
fn centre_of(s: &mut Studio, n: &Node) -> Result<[f32; 2], ToolError> {
    let b = s.evaluator.eval(n, &s.project.params)?.bounds();
    if !b.min.x.is_finite() {
        return Err(ToolError::new("empty_result", "the result has no volume", "the objects don't overlap: for `intersection` and `difference` they must"));
    }
    Ok([((b.min.x + b.max.x) / 2.0 * 100.0).round() / 100.0, ((b.min.y + b.max.y) / 2.0 * 100.0).round() / 100.0])
}

fn t_combine(s: &mut Studio, i: CombineIn) -> Result<ToolOutput, ToolError> {
    let srcs: Vec<Object> = i.ids.iter().map(|id| s.project.object(id).cloned().ok_or_else(|| ToolError::new("not_found", format!("no object '{id}'"), "call `describe` to list ids"))).collect::<Result<_, _>>()?;
    let Some(first) = srcs.first().cloned() else {
        return Err(ToolError::new("missing_ids", "no ids", "name the objects to combine"));
    };
    if srcs.iter().any(|o| o.plate != first.plate) {
        return Err(ToolError::new("different_plates", "the objects are on different plates", "move them onto one plate first (plate {\"action\": \"move\"})"));
    }
    s.set_plate(first.plate);
    let at = s.project.objects.iter().position(|o| o.id == first.id).unwrap_or(s.project.objects.len());
    if i.op == "explode" {
        return explode(s, &first, at);
    }
    if srcs.len() < 2 {
        return Err(ToolError::new("missing_ids", format!("{} needs two or more objects", i.op), "select the objects to combine"));
    }
    let mut each: Vec<Vec<Body>> = Vec::new();
    for o in &srcs {
        each.push(baked(s, o)?);
    }
    let mut obj = first.clone();
    obj.id = i.id.clone().unwrap_or_else(|| first.id.clone());
    if obj.id != first.id && s.project.object(&obj.id).is_some() && !i.ids.contains(&obj.id) {
        return Err(ToolError::new("duplicate_id", format!("object '{}' already exists", obj.id), "pick another id"));
    }
    obj.rotation = [0.0; 3];
    obj.scale = [1.0; 3];
    obj.locked = false;
    obj.hidden = false;
    let what = match i.op.as_str() {
        "assemble" => {
            let mut bodies = each.into_iter().flatten();
            let main = bodies.next().unwrap();
            obj.filament = main.filament;
            obj.shape = main.node;
            obj.parts = bodies.map(|b| Part { name: b.name, shape: b.node, filament: b.filament }).collect();
            format!("Assembled {} into '{}' ({} bodies, each keeps its filament)", i.ids.join(", "), obj.id, obj.parts.len() + 1)
        }
        "union" | "difference" | "intersection" => {
            let groups: Vec<Node> = each.into_iter().map(|bs| one(bs.into_iter().map(|b| b.node).collect())).collect();
            obj.shape = Node::from(match i.op.as_str() {
                "union" => Shape::Union { children: groups },
                "difference" => Shape::Difference { children: groups },
                _ => Shape::Intersection { children: groups },
            });
            obj.parts.clear();
            match i.op.as_str() {
                "union" => format!("Merged {} into '{}'", i.ids.join(", "), obj.id),
                "difference" => format!("Cut {} out of '{}'", i.ids[1..].join(", "), obj.id),
                _ => format!("Kept where {} overlap, as '{}'", i.ids.join(", "), obj.id),
            }
        }
        other => return Err(ToolError::new("bad_op", format!("'{other}'"), "assemble | union | difference | intersection | explode")),
    };
    if let Some(n) = i.name {
        obj.name = n;
    } else if i.op == "assemble" {
        obj.name = next_group_name(&s.project);
    } else if obj.name.is_empty() || obj.name == first.id {
        obj.name = obj.id.clone();
    }
    let whole = if obj.parts.is_empty() { obj.shape.clone() } else { one(std::iter::once(obj.shape.clone()).chain(obj.parts.iter().map(|p| p.shape.clone())).collect()) };
    obj.position = Some(centre_of(s, &whole)?);
    s.project.objects.retain(|o| !i.ids.contains(&o.id));
    let at = at.min(s.project.objects.len());
    let new_id = obj.id.clone();
    s.project.objects.insert(at, obj);
    s.selection = vec![new_id.clone()];
    s.changed("combine");
    let objs = s.evaluated()?;
    let line = objs.iter().find(|o| o.id == new_id).map(crate::tools::obj_line).unwrap_or_default();
    Ok(ToolOutput::text(format!("{what}: {line}")).with_data(json!({"id": new_id})))
}

/// An assembled object back into one object per body, each where it was.
fn explode(s: &mut Studio, o: &Object, at: usize) -> Result<ToolOutput, ToolError> {
    if o.parts.is_empty() {
        return Err(ToolError::new("no_parts", format!("'{}' is one body", o.id), "explode works on assembled (multi-part) objects; to cut one, use split_object"));
    }
    let bodies = baked(s, o)?;
    let mut out = Vec::new();
    for (k, b) in bodies.into_iter().enumerate() {
        let mut c = o.clone();
        c.id = if k == 0 { o.id.clone() } else { s.project.unique_id(&format!("{}_{}", o.id, slug(&b.name))) };
        // the first body keeps the group's id; a "Group N" name goes with the group
        c.name = if k > 0 {
            b.name.clone()
        } else if group_number(&o.name).is_some() {
            String::new()
        } else {
            o.name.clone()
        };
        c.parts.clear();
        c.rotation = [0.0; 3];
        c.scale = [1.0; 3];
        c.filament = b.filament;
        c.position = Some(centre_of(s, &b.node)?);
        c.shape = b.node;
        out.push(c);
    }
    s.project.objects.retain(|x| x.id != o.id);
    let ids: Vec<String> = out.iter().map(|c| c.id.clone()).collect();
    for (k, c) in out.into_iter().enumerate() {
        let at = (at + k).min(s.project.objects.len());
        s.project.objects.insert(at, c);
    }
    s.selection = ids.clone();
    s.changed("combine");
    Ok(ToolOutput::text(format!("Split '{}' into {} objects, each where it was: {}", o.id, ids.len(), ids.join(", "))).with_data(json!({"ids": ids})))
}

/// `Group 3` → 3.
fn group_number(name: &str) -> Option<u32> {
    name.strip_prefix("Group ").and_then(|n| n.parse().ok())
}

/// The next free group name: one more than the highest "Group N" on any plate.
fn next_group_name(p: &only3d_model::Project) -> String {
    let n = p.objects.iter().filter_map(|o| group_number(&o.name)).max().unwrap_or(0);
    format!("Group {}", n + 1)
}

fn slug(name: &str) -> String {
    let s: String = name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let s = s.trim_matches('_').to_string();
    if s.is_empty() {
        "part".into()
    } else {
        s
    }
}

pub fn register(r: &mut Registry) {
    r.add(def::<CombineIn>(
        "combine",
        "Combine objects",
        "Combine objects in place: assemble (one object, bodies keep their filaments), union, difference (first minus the rest), intersection; explode splits an assembly.",
        true,
        false,
        t_combine,
    ));
}

#[cfg(test)]
mod tests {
    use crate::{Actor, Studio};
    use serde_json::json;

    fn bounds(s: &mut Studio, id: &str) -> [f32; 6] {
        let b = s.evaluated().unwrap().iter().find(|o| o.id == id).unwrap().report.bounds;
        [b.min.x, b.min.y, b.min.z, b.max.x, b.max.y, b.max.z]
    }

    fn near(a: [f32; 6], b: [f32; 6]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.02)
    }

    fn setup() -> Studio {
        let mut s = Studio::new();
        s.call("add_object", json!({"id": "plate", "shape": {"type": "box", "size": [40, 30, 4]}, "position": [100, 100], "rotation": [0, 0, 90]}), Actor::User).unwrap();
        s.call("add_object", json!({"id": "peg", "shape": {"type": "cylinder", "d": 8, "h": 20}, "position": [100, 110], "filament": 2}), Actor::User).unwrap();
        s.call("set_filament", json!({"slot": 2, "color": "#E05A2B"}), Actor::User).unwrap();
        s
    }

    #[test]
    fn assemble_keeps_places_and_filaments_and_explodes_back() {
        let mut s = setup();
        let (a, b) = (bounds(&mut s, "plate"), bounds(&mut s, "peg"));
        let r = s.call("combine", json!({"op": "assemble", "ids": ["plate", "peg"]}), Actor::User).unwrap();
        assert_eq!(r.data["id"], "plate");
        assert_eq!(s.project.object("plate").unwrap().name, "Group 1");
        assert_eq!(s.project.objects.len(), 1);
        let objs = s.evaluated().unwrap();
        let o = &objs[0];
        assert_eq!(o.filaments(), vec![1, 2]);
        let pb = o.parts[1].mesh.bounds();
        assert!(near([pb.min.x, pb.min.y, pb.min.z, pb.max.x, pb.max.y, pb.max.z], b), "peg stayed: {pb:?} vs {b:?}");
        let ab = o.parts[0].mesh.bounds();
        assert!(near([ab.min.x, ab.min.y, ab.min.z, ab.max.x, ab.max.y, ab.max.z], a), "plate stayed (turned 90°): {ab:?} vs {a:?}");
        let r = s.call("combine", json!({"op": "explode", "ids": ["plate"]}), Actor::User).unwrap();
        let ids: Vec<String> = r.data["ids"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert_eq!(ids.len(), 2);
        assert!(near(bounds(&mut s, &ids[0]), a) && near(bounds(&mut s, &ids[1]), b));
        assert_eq!(s.project.object(&ids[1]).unwrap().filament, 2);
        // the first piece gets its own name back; the next group counts on
        assert_eq!(s.project.object("plate").unwrap().name, "");
        s.project.objects[0].name = "Group 4".into();
        assert_eq!(super::next_group_name(&s.project), "Group 5");
    }

    #[test]
    fn booleans_make_one_solid_in_place() {
        let mut s = setup();
        let a = bounds(&mut s, "plate");
        s.call("combine", json!({"op": "difference", "ids": ["plate", "peg"]}), Actor::User).unwrap();
        let o = &s.evaluated().unwrap()[0];
        assert!(o.report.watertight, "{:?}", o.report);
        let full = 40.0 * 30.0 * 4.0;
        let hole = std::f32::consts::PI * 16.0 * 4.0;
        assert!((o.report.volume_mm3 as f32 - (full - hole)).abs() < 15.0, "{}", o.report.volume_mm3);
        assert!(near(bounds(&mut s, "plate"), a));
        s.call("undo", json!({}), Actor::User).unwrap();
        s.call("combine", json!({"op": "union", "ids": ["plate", "peg"], "id": "base"}), Actor::User).unwrap();
        assert_eq!(s.project.objects.len(), 1);
        assert!((bounds(&mut s, "base")[5] - 20.0).abs() < 0.01);
        s.call("undo", json!({}), Actor::User).unwrap();
        s.call("combine", json!({"op": "intersection", "ids": ["plate", "peg"]}), Actor::User).unwrap();
        let b = bounds(&mut s, "plate");
        assert!((b[3] - b[0] - 8.0).abs() < 0.05 && (b[5] - 4.0).abs() < 0.01, "{b:?}");
        let e = s.call("combine", json!({"op": "explode", "ids": ["plate"]}), Actor::User).unwrap_err();
        assert_eq!(e.code, "no_parts");
    }
}
