//! 3MF core-spec reader/writer, including the production extension (components that live in
//! other model parts, as Bambu Studio and Orca write them).

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};

use glam::{Mat4, Vec3, Vec4};
use quick_xml::events::Event;
use quick_xml::Reader;

use super::{ImportedObject, ImportedPart};
use crate::{GeomError, Mesh, Result};

fn perr(m: impl Into<String>) -> GeomError {
    GeomError::Parse { format: "3mf", message: m.into() }
}

/// Parse a 3MF transform "m00 m01 m02 m10 m11 m12 m20 m21 m22 m30 m31 m32" (row-vector convention).
pub fn parse_transform(s: &str) -> Option<Mat4> {
    let v: Vec<f32> = s.split_whitespace().filter_map(|t| t.parse().ok()).collect();
    if v.len() != 12 {
        return None;
    }
    Some(Mat4::from_cols(
        Vec4::new(v[0], v[1], v[2], 0.0),
        Vec4::new(v[3], v[4], v[5], 0.0),
        Vec4::new(v[6], v[7], v[8], 0.0),
        Vec4::new(v[9], v[10], v[11], 1.0),
    ))
}

pub fn format_transform(m: &Mat4) -> String {
    let c = m.to_cols_array();
    format!(
        "{} {} {} {} {} {} {} {} {} {} {} {}",
        c[0], c[1], c[2], c[4], c[5], c[6], c[8], c[9], c[10], c[12], c[13], c[14]
    )
}

#[derive(Default, Clone)]
struct ObjDef {
    name: String,
    mesh: Mesh,
    components: Vec<(String, u32, Mat4)>, // (path, objectid, transform)
    color: Option<[u8; 3]>,
}

fn parse_color(s: &str) -> Option<[u8; 3]> {
    let s = s.trim_start_matches('#');
    if s.len() < 6 {
        return None;
    }
    let h = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
    Some([h(0)?, h(2)?, h(4)?])
}

fn parse_model(xml: &str, path: &str, objects: &mut HashMap<(String, u32), ObjDef>, build: &mut Vec<(String, u32, Mat4)>) -> Result<()> {
    let mut r = Reader::from_str(xml);
    r.config_mut().trim_text(true);
    let mut cur: Option<(u32, ObjDef)> = None;
    let mut materials: HashMap<u32, Vec<[u8; 3]>> = HashMap::new();
    let mut cur_mat: Option<u32> = None;
    let mut buf = Vec::new();
    loop {
        let ev = r.read_event_into(&mut buf).map_err(|e| perr(format!("{path}: {e}")))?;
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let mut attrs: HashMap<String, String> = HashMap::new();
                for a in e.attributes().flatten() {
                    let k = String::from_utf8_lossy(a.key.as_ref()).to_string();
                    let v = a.unescape_value().map(|c| c.into_owned()).unwrap_or_default();
                    attrs.insert(k, v);
                }
                let get = |k: &str| attrs.get(k).map(|s| s.as_str());
                match e.local_name().as_ref() {
                    b"basematerials" => cur_mat = get("id").and_then(|s| s.parse().ok()),
                    b"base" => {
                        if let (Some(id), Some(c)) = (cur_mat, get("displaycolor").and_then(parse_color)) {
                            materials.entry(id).or_default().push(c);
                        }
                    }
                    b"object" => {
                        let id: u32 = get("id").and_then(|s| s.parse().ok()).ok_or_else(|| perr("object without id"))?;
                        let mut d = ObjDef { name: get("name").unwrap_or("").to_string(), ..Default::default() };
                        if let (Some(pid), Some(pi)) = (get("pid").and_then(|s| s.parse::<u32>().ok()), get("pindex").and_then(|s| s.parse::<usize>().ok())) {
                            d.color = materials.get(&pid).and_then(|m| m.get(pi)).copied();
                        }
                        cur = Some((id, d));
                    }
                    b"vertex" => {
                        if let Some((_, d)) = cur.as_mut() {
                            let f = |k: &str| get(k).and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
                            d.mesh.positions.push(Vec3::new(f("x"), f("y"), f("z")));
                        }
                    }
                    b"triangle" => {
                        if let Some((_, d)) = cur.as_mut() {
                            let f = |k: &str| get(k).and_then(|s| s.parse::<u32>().ok());
                            if let (Some(a), Some(b), Some(c)) = (f("v1"), f("v2"), f("v3")) {
                                d.mesh.indices.push([a, b, c]);
                            }
                        }
                    }
                    b"component" => {
                        if let Some((_, d)) = cur.as_mut() {
                            let oid: u32 = get("objectid").and_then(|s| s.parse().ok()).ok_or_else(|| perr("component without objectid"))?;
                            let p = attrs.iter().find(|(k, _)| k.ends_with(":path") || *k == "path").map(|(_, v)| v.clone()).unwrap_or_else(|| path.to_string());
                            let t = get("transform").and_then(parse_transform).unwrap_or(Mat4::IDENTITY);
                            d.components.push((p, oid, t));
                        }
                    }
                    b"item" => {
                        let oid: u32 = get("objectid").and_then(|s| s.parse().ok()).ok_or_else(|| perr("build item without objectid"))?;
                        let p = attrs.iter().find(|(k, _)| k.ends_with(":path")).map(|(_, v)| v.clone()).unwrap_or_else(|| path.to_string());
                        let t = get("transform").and_then(parse_transform).unwrap_or(Mat4::IDENTITY);
                        build.push((p, oid, t));
                    }
                    _ => {}
                }
                if matches!(ev, Event::Empty(_)) && e.local_name().as_ref() == b"object" {
                    if let Some((id, d)) = cur.take() {
                        objects.insert((path.to_string(), id), d);
                    }
                }
            }
            Event::End(ref e) => {
                if e.local_name().as_ref() == b"object" {
                    if let Some((id, d)) = cur.take() {
                        let n = d.mesh.positions.len() as u32;
                        if d.mesh.indices.iter().any(|t| t.iter().any(|&v| v >= n)) {
                            return Err(perr(format!("{path}: object {id} has a triangle index out of range")));
                        }
                        objects.insert((path.to_string(), id), d);
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

fn flatten(objects: &HashMap<(String, u32), ObjDef>, key: &(String, u32), t: Mat4, out: &mut Mesh, depth: u32) -> Result<()> {
    if depth > 16 {
        return Err(perr("component nesting too deep (cycle?)"));
    }
    let d = objects.get(key).ok_or_else(|| perr(format!("missing object {} in {}", key.1, key.0)))?;
    if !d.mesh.indices.is_empty() {
        out.append(&d.mesh.transformed(&t));
    }
    for (p, id, ct) in &d.components {
        flatten(objects, &(p.clone(), *id), t * *ct, out, depth + 1)?;
    }
    Ok(())
}

/// One filament slot of a Bambu Studio / Orca project.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FilamentInfo {
    /// "#RRGGBB"
    pub color: String,
    /// Slicer filament preset ("Bambu PLA Basic @BBL H2D").
    pub preset: Option<String>,
    /// "PLA", "PETG"...
    pub material: Option<String>,
}

/// Slicer metadata of a 3MF project (Bambu Studio / Orca): filament slots and which slot each
/// imported object uses (1-based, parallel to the objects `read` returns).
#[derive(Clone, Debug, Default)]
pub struct ThreeMfMeta {
    pub filaments: Vec<FilamentInfo>,
    pub object_filaments: Vec<Option<u32>>,
    /// Other filaments used by parts of each object (multi-part, multi-colour objects).
    pub object_extra_filaments: Vec<Vec<u32>>,
    /// Plate (1-based) of each object in multi-plate projects.
    pub object_plates: Vec<Option<u32>>,
    pub plates: u32,
    /// Triangles carry per-face colour painting (`paint_color`), which is imported as the
    /// object's main filament only.
    pub painted: bool,
}

fn parse_hex_color(s: &str) -> Option<[u8; 3]> {
    parse_color(s)
}

/// Objects plus Bambu/Orca project metadata (filament colours and slots).
pub fn read_with_meta(bytes: &[u8]) -> Result<(Vec<ImportedObject>, ThreeMfMeta)> {
    let mut objects = read_objects(bytes)?;
    let mut meta = ThreeMfMeta::default();
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| perr(e.to_string()))?;
    let mut text = |name: &str| -> Option<String> {
        let mut f = zip.by_name(name).ok()?;
        let mut s = String::new();
        f.read_to_string(&mut s).ok()?;
        Some(s)
    };
    if let Some(ps) = text("Metadata/project_settings.config") {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&ps) {
            let arr = |k: &str| -> Vec<String> { v.get(k).and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() };
            let (colors, presets, types) = (arr("filament_colour"), arr("filament_settings_id"), arr("filament_type"));
            meta.filaments = colors
                .iter()
                .enumerate()
                .map(|(i, c)| FilamentInfo { color: c.chars().take(7).collect::<String>().to_ascii_uppercase(), preset: presets.get(i).cloned(), material: types.get(i).cloned() })
                .collect();
        }
    }
    // model_settings.config: object id → extruder (object level, else its parts by size), and
    // plate membership; mapped to build order (root model's build items)
    let mut extruder: HashMap<u32, u32> = HashMap::new();
    let mut parts: HashMap<u32, Vec<(u32, u64)>> = HashMap::new(); // (extruder, faces)
    // every part by object: (part id, extruder, subtype, name)
    let mut part_info: HashMap<u32, Vec<(u32, Option<u32>, String, String)>> = HashMap::new();
    let mut plate_of: HashMap<u32, u32> = HashMap::new();
    if let Some(ms) = text("Metadata/model_settings.config") {
        let mut r = Reader::from_str(&ms);
        r.config_mut().trim_text(true);
        let mut buf = Vec::new();
        let mut obj: Option<u32> = None;
        let mut part: Option<(Option<u32>, u64)> = None;
        let mut part_meta: Option<(u32, String, String)> = None; // (id, subtype, name)
        let mut plate: Option<u32> = None;
        let mut in_plate = false;
        loop {
            match r.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                    let attrs: HashMap<String, String> = e.attributes().flatten().map(|a| (String::from_utf8_lossy(a.key.as_ref()).to_string(), a.unescape_value().map(|c| c.into_owned()).unwrap_or_default())).collect();
                    let key = attrs.get("key").map(String::as_str);
                    let val = || attrs.get("value").and_then(|x| x.parse::<u32>().ok());
                    match e.local_name().as_ref() {
                        b"object" if !in_plate => obj = attrs.get("id").and_then(|x| x.parse().ok()),
                        b"part" => {
                            part = Some((None, 0));
                            part_meta = Some((attrs.get("id").and_then(|x| x.parse().ok()).unwrap_or(0), attrs.get("subtype").cloned().unwrap_or_else(|| "normal_part".into()), String::new()));
                        }
                        b"metadata" if !in_plate && key == Some("name") && part_meta.is_some() => {
                            if let Some(pm) = part_meta.as_mut() {
                                pm.2 = attrs.get("value").cloned().unwrap_or_default();
                            }
                        }
                        b"plate" => {
                            in_plate = true;
                            plate = None;
                        }
                        b"mesh_stat" => {
                            if let Some(p) = part.as_mut() {
                                p.1 = attrs.get("face_count").and_then(|x| x.parse().ok()).unwrap_or(0);
                            }
                        }
                        b"metadata" if in_plate && key == Some("plater_id") => plate = val(),
                        b"metadata" if in_plate && key == Some("object_id") => {
                            if let (Some(pl), Some(id)) = (plate, val()) {
                                plate_of.insert(id, pl);
                            }
                        }
                        b"metadata" if key == Some("extruder") && !in_plate => match (part.as_mut(), obj) {
                            (Some(p), _) => p.0 = val(),
                            (None, Some(id)) => {
                                if let Some(v) = val() {
                                    extruder.insert(id, v);
                                }
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                }
                Ok(Event::End(ref e)) => match e.local_name().as_ref() {
                    b"object" if !in_plate => obj = None,
                    b"part" => {
                        let ext = part.as_ref().and_then(|p| p.0);
                        if let (Some((pid, sub, name)), Some(id)) = (part_meta.take(), obj) {
                            part_info.entry(id).or_default().push((pid, ext, sub.clone(), name));
                            // only solid parts count towards an object's filaments
                            if sub != "normal_part" {
                                part = None;
                            }
                        }
                        if let (Some((Some(x), faces)), Some(id)) = (part.take(), obj) {
                            parts.entry(id).or_default().push((x, faces));
                        }
                    }
                    b"plate" => in_plate = false,
                    _ => {}
                },
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
    }
    let ids = build_object_ids(bytes)?;
    for id in &ids {
        let ps = parts.get(id).cloned().unwrap_or_default();
        let main = extruder.get(id).copied().or_else(|| ps.iter().max_by_key(|p| p.1).map(|p| p.0));
        let mut extra: Vec<u32> = ps.iter().map(|p| p.0).filter(|x| Some(*x) != main).collect();
        extra.sort();
        extra.dedup();
        meta.object_filaments.push(main);
        meta.object_extra_filaments.push(extra);
        meta.object_plates.push(plate_of.get(id).copied());
    }
    // label each part with its filament; modifiers and negative volumes are not solid parts
    for (k, o) in objects.iter_mut().enumerate() {
        let Some(info) = ids.get(k).and_then(|id| part_info.get(id)) else { continue };
        let main = meta.object_filaments.get(k).copied().flatten();
        let before = o.parts.len();
        o.parts.retain(|p| info.iter().find(|i| i.0 == p.id).is_none_or(|i| i.2 == "normal_part"));
        for p in &mut o.parts {
            if let Some(i) = info.iter().find(|i| i.0 == p.id) {
                p.filament = i.1.or(main);
                if !i.3.is_empty() {
                    p.name = i.3.clone();
                }
            } else {
                p.filament = main;
            }
        }
        if o.parts.len() != before && !o.parts.is_empty() {
            let mut m = Mesh::new();
            for p in &o.parts {
                m.append(&p.mesh);
            }
            o.mesh = m;
        }
        if o.parts.len() == 1 {
            o.parts.clear();
        }
    }
    meta.object_filaments.resize(objects.len(), None);
    meta.object_extra_filaments.resize(objects.len(), Vec::new());
    meta.object_plates.resize(objects.len(), None);
    meta.plates = meta.object_plates.iter().flatten().copied().max().unwrap_or(1);
    meta.painted = bytes_contain_paint(bytes);
    for (o, f) in objects.iter_mut().zip(&meta.object_filaments) {
        if o.color.is_none() {
            if let Some(fi) = f.and_then(|k| meta.filaments.get(k as usize - 1)) {
                o.color = parse_hex_color(&fi.color);
            }
        }
    }
    Ok((objects, meta))
}

fn build_object_ids(bytes: &[u8]) -> Result<Vec<u32>> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| perr(e.to_string()))?;
    let mut xml = String::new();
    if let Ok(mut f) = zip.by_name("3D/3dmodel.model") {
        let _ = f.read_to_string(&mut xml);
    }
    let mut r = Reader::from_str(&xml);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) if e.local_name().as_ref() == b"item" => {
                if let Some(id) = e.attributes().flatten().find(|a| a.key.as_ref() == b"objectid").and_then(|a| a.unescape_value().ok()).and_then(|v| v.parse().ok()) {
                    out.push(id);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

fn bytes_contain_paint(bytes: &[u8]) -> bool {
    let Ok(mut zip) = zip::ZipArchive::new(Cursor::new(bytes)) else { return false };
    for i in 0..zip.len() {
        let Ok(mut f) = zip.by_index(i) else { continue };
        if !f.name().ends_with(".model") {
            continue;
        }
        let mut s = String::new();
        if f.read_to_string(&mut s).is_ok() && s.contains("paint_color=") {
            return true;
        }
    }
    false
}

pub fn read(bytes: &[u8]) -> Result<Vec<ImportedObject>> {
    read_with_meta(bytes).map(|(o, _)| o)
}

fn read_objects(bytes: &[u8]) -> Result<Vec<ImportedObject>> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| perr(e.to_string()))?;
    let mut models: Vec<(String, String)> = Vec::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| perr(e.to_string()))?;
        let name = f.name().to_string();
        if name.to_ascii_lowercase().ends_with(".model") {
            let mut s = String::new();
            f.read_to_string(&mut s).map_err(|e| perr(format!("{name}: {e}")))?;
            models.push((format!("/{}", name.trim_start_matches('/')), s));
        }
    }
    if models.is_empty() {
        return Err(perr("no .model part in archive"));
    }
    let mut objects = HashMap::new();
    let mut build = Vec::new();
    let mut root_build = Vec::new();
    // Bambu/Orca write one model part per object: parse them in parallel
    use rayon::prelude::*;
    type Parsed = (String, HashMap<(String, u32), ObjDef>, Vec<(String, u32, Mat4)>);
    let parsed: Vec<Result<Parsed>> = models
        .par_iter()
        .map(|(p, xml)| {
            let mut o = HashMap::new();
            let mut b = Vec::new();
            parse_model(xml, p, &mut o, &mut b)?;
            Ok((p.clone(), o, b))
        })
        .collect();
    for r in parsed {
        let (p, o, b) = r?;
        objects.extend(o);
        if p.eq_ignore_ascii_case("/3D/3dmodel.model") {
            root_build = b;
        } else {
            build.extend(b);
        }
    }
    if !root_build.is_empty() {
        build = root_build;
    }
    build
        .par_iter()
        .enumerate()
        .map(|(i, (p, id, t))| {
            let mut m = Mesh::new();
            flatten(&objects, &(p.clone(), *id), *t, &mut m, 0)?;
            m.weld(1e-5);
            let d = &objects[&(p.clone(), *id)];
            let name = if d.name.is_empty() { format!("object_{}", i + 1) } else { d.name.clone() };
            // an object made of several components: each is a part (slicers give each its own
            // filament in their metadata)
            let mut parts = Vec::new();
            if d.mesh.indices.is_empty() && d.components.len() > 1 {
                for (k, (cp, cid, ct)) in d.components.iter().enumerate() {
                    let mut pm = Mesh::new();
                    flatten(&objects, &(cp.clone(), *cid), *t * *ct, &mut pm, 1)?;
                    pm.weld(1e-5);
                    let cn = objects.get(&(cp.clone(), *cid)).map(|c| c.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| format!("part {}", k + 1));
                    parts.push(ImportedPart { id: *cid, name: cn, mesh: pm, filament: None });
                }
            }
            Ok(ImportedObject { name, mesh: m, color: d.color, parts })
        })
        .collect()
}

pub fn write(objects: &[ImportedObject]) -> Result<Vec<u8>> {
    let mut model = String::with_capacity(1 << 16);
    model.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>
<model unit="millimeter" xml:lang="en-US" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">
 <metadata name="Application">only3d</metadata>
 <resources>
"#);
    let colored: Vec<_> = objects.iter().filter_map(|o| o.color).collect();
    if !colored.is_empty() {
        model.push_str("  <basematerials id=\"1000\">\n");
        for c in &colored {
            model.push_str(&format!("   <base name=\"c\" displaycolor=\"#{:02X}{:02X}{:02X}FF\"/>\n", c[0], c[1], c[2]));
        }
        model.push_str("  </basematerials>\n");
    }
    let mut ci = 0;
    for (i, o) in objects.iter().enumerate() {
        let name = o.name.replace('&', "&amp;").replace('<', "&lt;").replace('"', "&quot;");
        let mat = if o.color.is_some() {
            ci += 1;
            format!(" pid=\"1000\" pindex=\"{}\"", ci - 1)
        } else {
            String::new()
        };
        model.push_str(&format!("  <object id=\"{}\" type=\"model\" name=\"{}\"{}>\n   <mesh>\n    <vertices>\n", i + 1, name, mat));
        for p in &o.mesh.positions {
            model.push_str(&format!("     <vertex x=\"{}\" y=\"{}\" z=\"{}\"/>\n", p.x, p.y, p.z));
        }
        model.push_str("    </vertices>\n    <triangles>\n");
        for t in &o.mesh.indices {
            model.push_str(&format!("     <triangle v1=\"{}\" v2=\"{}\" v3=\"{}\"/>\n", t[0], t[1], t[2]));
        }
        model.push_str("    </triangles>\n   </mesh>\n  </object>\n");
    }
    model.push_str(" </resources>\n <build>\n");
    for i in 0..objects.len() {
        model.push_str(&format!("  <item objectid=\"{}\"/>\n", i + 1));
    }
    model.push_str(" </build>\n</model>\n");

    let mut buf = Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let w = |z: &mut zip::ZipWriter<&mut Cursor<Vec<u8>>>, name: &str, data: &[u8]| -> Result<()> {
            z.start_file(name, opt).map_err(|e| perr(e.to_string()))?;
            z.write_all(data).map_err(|e| perr(e.to_string()))
        };
        w(&mut z, "[Content_Types].xml", CONTENT_TYPES.as_bytes())?;
        w(&mut z, "_rels/.rels", RELS.as_bytes())?;
        w(&mut z, "3D/3dmodel.model", model.as_bytes())?;
        z.finish().map_err(|e| perr(e.to_string()))?;
    }
    Ok(buf.into_inner())
}

pub const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
 <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
 <Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/>
 <Default Extension="png" ContentType="image/png"/>
 <Default Extension="gcode" ContentType="text/x.gcode"/>
</Types>
"#;

pub const RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
 <Relationship Target="/3D/3dmodel.model" Id="rel-1" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/>
</Relationships>
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shapes;

    /// A Bambu Studio-style project: an object made of two components (parts), each with its
    /// own extruder, plus a modifier volume that must not become solid.
    #[test]
    fn reads_the_parts_of_a_slicer_project() {
        let tet = |id: u32, x: f32| {
            format!(
                r#"<object id="{id}" type="model"><mesh><vertices><vertex x="{x}" y="0" z="0"/><vertex x="{}" y="0" z="0"/><vertex x="{x}" y="10" z="0"/><vertex x="{x}" y="0" z="10"/></vertices><triangles><triangle v1="0" v2="2" v3="1"/><triangle v1="0" v2="1" v3="3"/><triangle v1="0" v2="3" v3="2"/><triangle v1="1" v2="2" v3="3"/></triangles></mesh></object>"#,
                x + 10.0
            )
        };
        let model = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02"><resources>{}{}{}<object id="4" type="model"><components><component objectid="1"/><component objectid="2"/><component objectid="3"/></components></object></resources><build><item objectid="4"/></build></model>"#,
            tet(1, 0.0),
            tet(2, 20.0),
            tet(3, 40.0)
        );
        let settings = r#"<?xml version="1.0" encoding="UTF-8"?><config><object id="4"><metadata key="name" value="duo"/><metadata key="extruder" value="1"/><part id="1" subtype="normal_part"><metadata key="name" value="base"/><metadata key="extruder" value="1"/></part><part id="2" subtype="normal_part"><metadata key="name" value="inlay"/><metadata key="extruder" value="2"/></part><part id="3" subtype="modifier_part"><metadata key="name" value="infill zone"/></part></object></config>"#;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let opt = zip::write::SimpleFileOptions::default();
            z.start_file("3D/3dmodel.model", opt).unwrap();
            z.write_all(model.as_bytes()).unwrap();
            z.start_file("Metadata/model_settings.config", opt).unwrap();
            z.write_all(settings.as_bytes()).unwrap();
            z.finish().unwrap();
        }
        let (objs, meta) = read_with_meta(&buf.into_inner()).unwrap();
        assert_eq!(objs.len(), 1);
        let parts: Vec<(&str, Option<u32>)> = objs[0].parts.iter().map(|p| (p.name.as_str(), p.filament)).collect();
        assert_eq!(parts, vec![("base", Some(1)), ("inlay", Some(2))], "the modifier is not a part");
        assert_eq!(objs[0].mesh.indices.len(), 2 * 4, "nor part of the solid (two tetrahedra)");
        assert_eq!(meta.object_extra_filaments[0], vec![2]);
    }

    #[test]
    fn roundtrip() {
        let a = shapes::cuboid(Vec3::new(10.0, 10.0, 10.0)).unwrap();
        let objs = vec![ImportedObject { name: "cube & co".into(), mesh: a, color: Some([10, 200, 30]), parts: Vec::new() }];
        let bytes = write(&objs).unwrap();
        let back = read(&bytes).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].name, "cube & co");
        assert_eq!(back[0].color, Some([10, 200, 30]));
        assert!((back[0].mesh.volume() - 1000.0).abs() < 1e-3);
    }

    #[test]
    fn transform_roundtrip() {
        let m = Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0)) * Mat4::from_rotation_z(0.5);
        let back = parse_transform(&format_transform(&m)).unwrap();
        assert!(back.abs_diff_eq(m, 1e-5));
    }
}
