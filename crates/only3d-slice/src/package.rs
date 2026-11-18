//! Bambu-compatible `.gcode.3mf` packaging (what the printer downloads over FTPS and what
//! Bambu Studio/Handy show): G-code + md5, plate thumbnails, slice_info, model_settings, plate json.

use std::fmt::Write as _;
use std::io::{Cursor, Write};

use md5::{Digest, Md5};
use only3d_geom::Mesh;

use crate::gcode::PrintStats;

/// Above this many triangles the package stores bounding boxes instead of meshes.
pub const MAX_EMBEDDED_TRIANGLES: usize = 300_000;

pub struct PackageObject<'a> {
    pub name: &'a str,
    pub mesh: &'a Mesh,
}

/// One filament of the print file's config (index = filament id in the G-code).
pub struct PackageFilament<'a> {
    pub kind: &'a str,
    /// "#RRGGBB"
    pub color: &'a str,
    /// Bambu filament id (e.g. "GFA00" = Bambu PLA Basic), empty if unknown.
    pub tray_info_idx: &'a str,
    /// 0 = left, 1 = right (H2D).
    pub extruder: usize,
    pub used_m: f64,
    pub used_g: f64,
}

pub struct PackageInfo<'a> {
    pub gcode: &'a str,
    pub stats: &'a PrintStats,
    pub objects: Vec<PackageObject<'a>>,
    /// 512x512 PNG.
    pub thumbnail_png: Vec<u8>,
    /// 128x128 PNG.
    pub thumbnail_small_png: Vec<u8>,
    /// Every filament of the config; unused ones print 0 g.
    pub filaments: Vec<PackageFilament<'a>>,
    /// Filament ids in print order: the first loaded, then each change's.
    pub sequence: Vec<usize>,
    /// The filaments of each layer.
    pub layer_filaments: Vec<Vec<usize>>,
    pub nozzle_diameter: f64,
    /// Nozzles on the printer (2 on the H2D, 1 on an X1C): slice_info lists one value per nozzle.
    pub nozzle_count: usize,
    /// e.g. "textured_plate"
    pub bed_type_key: &'a str,
    pub layer_height: f64,
    /// Full flattened settings JSON so Bambu Studio can reopen the project (optional).
    pub project_settings: Option<String>,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Bambu's plate-type key for the plate json from a Studio bed type name.
pub fn bed_type_key(bed_type: &str) -> &'static str {
    match bed_type {
        "Cool Plate" | "Cool Plate (SuperTack)" => "cool_plate",
        "Engineering Plate" => "eng_plate",
        "High Temp Plate" | "Smooth PEI Plate" => "hot_plate",
        _ => "textured_plate",
    }
}

pub fn write_gcode_3mf(info: &PackageInfo) -> Result<Vec<u8>, String> {
    let md5 = {
        let mut h = Md5::new();
        h.update(info.gcode.as_bytes());
        h.finalize().iter().map(|b| format!("{b:02X}")).collect::<String>()
    };
    let st = info.stats;
    let maps = info.filaments.iter().map(|f| (f.extruder + 1).to_string()).collect::<Vec<_>>().join(" ");
    let first = info.sequence.first().copied().unwrap_or(0);
    let first_ext = info.filaments.get(first).map(|f| f.extruder).unwrap_or(0);
    let base_id = 15u32; // Bambu numbers "identify_id"s from an arbitrary base

    // 3D model: one object per mesh, already in plate coordinates.
    let mut model = String::new();
    model.push_str(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<model unit="millimeter" xml:lang="en-US" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02" xmlns:BambuStudio="http://schemas.bambulab.com/package/2021">
 <metadata name="Application">only3d</metadata>
 <metadata name="BambuStudio:3mfVersion">1</metadata>
 <resources>
"#,
    );
    // The printer only reads the G-code; full meshes are kept for small plates (so Bambu Studio
    // can reopen them) but replaced by bounding boxes on huge plates to keep uploads small.
    let total_tris: usize = info.objects.iter().map(|o| o.mesh.triangle_count()).sum();
    let boxes: Vec<Mesh> = if total_tris > MAX_EMBEDDED_TRIANGLES {
        info.objects
            .iter()
            .map(|o| {
                let b = o.mesh.bounds();
                let mut m = only3d_geom::shapes::cuboid((b.size()).max(glam::Vec3::splat(0.01))).unwrap_or_default();
                m.translate(glam::Vec3::new(b.center().x, b.center().y, b.min.z));
                m
            })
            .collect()
    } else {
        Vec::new()
    };
    if !boxes.is_empty() {
        let _ = writeln!(model, " <metadata name=\"only3d:meshes\">bounding boxes only ({total_tris} triangles omitted)</metadata>");
    }
    for (i, o) in info.objects.iter().enumerate() {
        let mesh = boxes.get(i).unwrap_or(o.mesh);
        let _ = writeln!(model, "  <object id=\"{}\" type=\"model\" name=\"{}\">\n   <mesh>\n    <vertices>", i + 1, esc(o.name));
        for p in &mesh.positions {
            let _ = writeln!(model, "     <vertex x=\"{}\" y=\"{}\" z=\"{}\"/>", p.x, p.y, p.z);
        }
        model.push_str("    </vertices>\n    <triangles>\n");
        for t in &mesh.indices {
            let _ = writeln!(model, "     <triangle v1=\"{}\" v2=\"{}\" v3=\"{}\"/>", t[0], t[1], t[2]);
        }
        model.push_str("    </triangles>\n   </mesh>\n  </object>\n");
    }
    model.push_str(" </resources>\n <build>\n");
    for i in 0..info.objects.len() {
        let _ = writeln!(model, "  <item objectid=\"{}\" printable=\"1\"/>", i + 1);
    }
    model.push_str(" </build>\n</model>\n");

    // slice_info.config
    let mut slice_info = String::new();
    let _ = write!(
        slice_info,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<config>
  <header>
    <header_item key="X-BBL-Client-Type" value="slicer"/>
    <header_item key="X-BBL-Client-Version" value="02.09.01.70"/>
  </header>
  <plate>
    <metadata key="index" value="1"/>
    <metadata key="extruder_type" value="{per_nozzle}"/>
    <metadata key="nozzle_volume_type" value="{per_nozzle}"/>
    <metadata key="printer_model_id" value=""/>
    <metadata key="nozzle_diameters" value="{diameters}"/>
    <metadata key="timelapse_type" value="0"/>
    <metadata key="prediction" value="{pred}"/>
    <metadata key="weight" value="{w:.2}"/>
    <metadata key="pause_count" value="0"/>
    <metadata key="first_layer_time" value="{flt:.6}"/>
    <metadata key="outside" value="false"/>
    <metadata key="support_used" value="false"/>
    <metadata key="label_object_enabled" value="false"/>
    <metadata key="support_material_on_wipe_tower" value="false"/>
    <metadata key="enable_filament_dynamic_map" value="false"/>
    <metadata key="has_filament_switcher" value="false"/>
    <metadata key="multi_nozzle_diameter_mode" value="0"/>
    <metadata key="filament_maps" value="{maps}"/>
    <metadata key="limit_filament_maps" value="{limits}"/>
"#,
        per_nozzle = vec!["0"; info.nozzle_count.max(1)].join(" "),
        diameters = vec![info.nozzle_diameter.to_string(); info.nozzle_count.max(1)].join(","),
        pred = st.print_time_s.round() as i64,
        w = st.filament_g,
        flt = st.layer_times_s.first().copied().unwrap_or(0.0),
        limits = vec!["0"; info.filaments.len().max(1)].join(" "),
    );
    for (i, o) in info.objects.iter().enumerate() {
        let _ = writeln!(slice_info, "    <object identify_id=\"{}\" name=\"{}\" skipped=\"false\" />", base_id + i as u32, esc(o.name));
    }
    let mut nozzles: Vec<usize> = Vec::new();
    for (i, f) in info.filaments.iter().enumerate() {
        if f.used_g <= 0.0 && info.filaments.len() > 1 {
            continue;
        }
        let _ = writeln!(
            slice_info,
            "    <filament id=\"{}\" tray_info_idx=\"{}\" type=\"{}\" color=\"{}\" used_m=\"{:.2}\" used_g=\"{:.2}\" group_id=\"{}\" nozzle_diameter=\"{:.2}\" volume_type=\"Standard\" used_for_object=\"true\" used_for_support=\"false\" directly_used=\"true\" total_load_time=\"0.00\" total_unload_time=\"0.00\"/>",
            i + 1,
            esc(f.tray_info_idx),
            esc(f.kind),
            esc(f.color),
            f.used_m,
            f.used_g,
            f.extruder,
            info.nozzle_diameter
        );
        if !nozzles.contains(&f.extruder) {
            nozzles.push(f.extruder);
        }
    }
    nozzles.sort_unstable();
    for e in &nozzles {
        let _ = writeln!(slice_info, "    <nozzle id=\"{e}\" extruder_id=\"{}\" nozzle_diameter=\"{}\" volume_type=\"Standard\"/>", e + 1, info.nozzle_diameter);
    }
    // which filaments print on which layers, as runs of layers
    slice_info.push_str("    <layer_filament_lists>\n");
    let n = st.layers;
    let lists: Vec<String> = (0..n).map(|l| info.layer_filaments.get(l).map(|v| { let mut v = v.clone(); v.sort_unstable(); v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(" ") }).unwrap_or_else(|| first.to_string())).collect();
    let mut runs: Vec<(String, usize, usize)> = Vec::new();
    for (l, key) in lists.iter().enumerate() {
        match runs.last_mut() {
            Some(r) if r.0 == *key && r.2 + 1 == l => r.2 = l,
            _ => runs.push((key.clone(), l, l)),
        }
    }
    // Bambu Studio lists each filament set once, with all its layer ranges
    let mut keys: Vec<String> = Vec::new();
    for r in &runs {
        if !keys.contains(&r.0) {
            keys.push(r.0.clone());
        }
    }
    for k in keys.into_iter().filter(|k| !k.is_empty()) {
        let ranges: Vec<String> = runs.iter().filter(|r| r.0 == k).map(|r| format!("{} {}", r.1, r.2)).collect();
        let _ = writeln!(slice_info, "      <layer_filament_list filament_list=\"{k}\" layer_ranges=\"{}\" />", ranges.join(","));
    }
    slice_info.push_str("    </layer_filament_lists>\n  </plate>\n</config>\n");

    // model_settings.config
    let mut ms = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<config>\n");
    for (i, o) in info.objects.iter().enumerate() {
        let _ = writeln!(
            ms,
            "  <object id=\"{id}\">\n    <metadata key=\"name\" value=\"{n}\"/>\n    <metadata key=\"extruder\" value=\"1\"/>\n    <metadata face_count=\"{fc}\"/>\n    <part id=\"1\" subtype=\"normal_part\">\n      <metadata key=\"name\" value=\"{n}\"/>\n      <metadata key=\"matrix\" value=\"1 0 0 0 0 1 0 0 0 0 1 0 0 0 0 1\"/>\n      <mesh_stat face_count=\"{fc}\" edges_fixed=\"0\" degenerate_facets=\"0\" facets_removed=\"0\" facets_reversed=\"0\" backwards_edges=\"0\"/>\n    </part>\n  </object>",
            id = i + 1,
            n = esc(o.name),
            fc = o.mesh.triangle_count()
        );
    }
    let _ = writeln!(
        ms,
        "  <plate>\n    <metadata key=\"plater_id\" value=\"1\"/>\n    <metadata key=\"plater_name\" value=\"\"/>\n    <metadata key=\"locked\" value=\"false\"/>\n    <metadata key=\"filament_map_mode\" value=\"Manual\"/>\n    <metadata key=\"filament_maps\" value=\"{maps}\"/>\n    <metadata key=\"gcode_file\" value=\"Metadata/plate_1.gcode\"/>\n    <metadata key=\"thumbnail_file\" value=\"Metadata/plate_1.png\"/>\n    <metadata key=\"thumbnail_no_light_file\" value=\"Metadata/plate_no_light_1.png\"/>\n    <metadata key=\"top_file\" value=\"Metadata/top_1.png\"/>\n    <metadata key=\"pick_file\" value=\"Metadata/pick_1.png\"/>"
    );
    for i in 0..info.objects.len() {
        let _ = writeln!(
            ms,
            "    <model_instance>\n      <metadata key=\"object_id\" value=\"{}\"/>\n      <metadata key=\"instance_id\" value=\"0\"/>\n      <metadata key=\"identify_id\" value=\"{}\"/>\n    </model_instance>",
            i + 1,
            base_id + i as u32
        );
    }
    ms.push_str("  </plate>\n  <assemble>\n  </assemble>\n</config>\n");

    // plate_1.json
    let mut all = only3d_geom::Aabb::EMPTY;
    let objs: Vec<serde_json::Value> = info
        .objects
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let b = o.mesh.bounds();
            all = all.union(&b);
            serde_json::json!({"area": (b.size().x * b.size().y) as f64, "bbox": [b.min.x, b.min.y, b.max.x, b.max.y], "id": base_id + i as u32, "layer_height": info.layer_height, "name": o.name})
        })
        .collect();
    let plate_json = serde_json::json!({
        "bbox_all": [all.min.x, all.min.y, all.max.x, all.max.y],
        "bbox_objects": objs,
        "bed_type": info.bed_type_key,
        "filament_colors": info.filaments.iter().map(|f| f.color).collect::<Vec<_>>(),
        "filament_ids": (0..info.filaments.len()).collect::<Vec<_>>(),
        "first_extruder": first_ext,
        "first_layer_time": st.layer_times_s.first().copied().unwrap_or(0.0),
        "is_seq_print": false,
        "nozzle_diameter": info.nozzle_diameter,
        "version": 2
    })
    .to_string();
    let list = |v: Vec<String>| v.join(",");
    let seq = format!(
        "{{\"plate_1\":{{\"nozzle_sequence\":[{}],\"optimal_assignment\":[{}],\"sequence\":[{}]}}}}",
        list(info.sequence.iter().map(|f| info.filaments.get(*f).map(|x| x.extruder).unwrap_or(0).to_string()).collect()),
        list(info.filaments.iter().map(|f| f.extruder.to_string()).collect()),
        list(info.sequence.iter().map(|f| (f + 1).to_string()).collect())
    );

    let rels = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
 <Relationship Target="/3D/3dmodel.model" Id="rel-1" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/>
 <Relationship Target="/Metadata/plate_1.png" Id="rel-2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail"/>
 <Relationship Target="/Metadata/plate_1.png" Id="rel-4" Type="http://schemas.bambulab.com/package/2021/cover-thumbnail-middle"/>
 <Relationship Target="/Metadata/plate_1_small.png" Id="rel-5" Type="http://schemas.bambulab.com/package/2021/cover-thumbnail-small"/>
</Relationships>
"#;
    let ms_rels = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
 <Relationship Target="/Metadata/plate_1.gcode" Id="rel-1" Type="http://schemas.bambulab.com/package/2021/gcode"/>
</Relationships>
"#;

    let mut buf = Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let mut put = |name: &str, data: &[u8]| -> Result<(), String> {
            z.start_file(name, opt).map_err(|e| e.to_string())?;
            z.write_all(data).map_err(|e| e.to_string())
        };
        put("[Content_Types].xml", only3d_geom::io::threemf::CONTENT_TYPES.as_bytes())?;
        put("_rels/.rels", rels.as_bytes())?;
        put("3D/3dmodel.model", model.as_bytes())?;
        put("Metadata/plate_1.png", &info.thumbnail_png)?;
        put("Metadata/plate_1_small.png", &info.thumbnail_small_png)?;
        put("Metadata/plate_no_light_1.png", &info.thumbnail_png)?;
        put("Metadata/top_1.png", &info.thumbnail_png)?;
        put("Metadata/pick_1.png", &info.thumbnail_png)?;
        put("Metadata/plate_1.json", plate_json.as_bytes())?;
        if let Some(ps) = &info.project_settings {
            put("Metadata/project_settings.config", ps.as_bytes())?;
        }
        put("Metadata/plate_1.gcode.md5", md5.as_bytes())?;
        put("Metadata/plate_1.gcode", info.gcode.as_bytes())?;
        put("Metadata/_rels/model_settings.config.rels", ms_rels.as_bytes())?;
        put("Metadata/model_settings.config", ms.as_bytes())?;
        put("Metadata/slice_info.config", slice_info.as_bytes())?;
        put("Metadata/filament_sequence.json", seq.as_bytes())?;
        z.finish().map_err(|e| e.to_string())?;
    }
    Ok(buf.into_inner())
}
