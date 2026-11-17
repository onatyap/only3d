//! End-to-end tests against the embedded Bambu Lab H2D presets.

use std::path::PathBuf;

use only3d_profiles::job::{self, JobInfo, LayerEvent, PrintSetup, TimelapseEvent, ToolChange, H2D_LEFT, H2D_RIGHT};
use only3d_profiles::template::{Context, Template};
use only3d_profiles::{
    BedType, Config, FilamentSummary, ListFilter, MachineSummary, PresetKind, ProcessSummary, ProfileStore,
    VariantTable, GCODE_TEMPLATE_KEYS,
};

const MACHINE: &str = "Bambu Lab H2D 0.4 nozzle";
const PROCESS: &str = "0.20mm Standard @BBL H2D";
const PLA: &str = "Bambu PLA Basic @BBL H2D";
const PLA_MATTE: &str = "Bambu PLA Matte @BBL H2D";
const PETG: &str = "Bambu PETG Basic @BBL H2D 0.4 nozzle";
const ASA: &str = "Bambu ASA @BBL H2D 0.4 nozzle";

/// Templates the H2D 0.4 machine preset pulls in through `include`.
const H2D_TEMPLATES: &[&str] = &[
    "machine_start_gcode",
    "machine_end_gcode",
    "layer_change_gcode",
    "time_lapse_gcode",
    "wrapping_detection_gcode",
    "change_filament_gcode",
];

fn store() -> ProfileStore {
    ProfileStore::embedded().expect("embedded store")
}

struct Presets {
    machine: Config,
    process: Config,
    filaments: Vec<Config>,
}

fn presets(store: &ProfileStore, filaments: &[&str]) -> Presets {
    Presets {
        machine: store.resolve(PresetKind::Machine, MACHINE).unwrap(),
        process: store.resolve(PresetKind::Process, PROCESS).unwrap(),
        filaments: filaments.iter().map(|f| store.resolve(PresetKind::Filament, f).unwrap()).collect(),
    }
}

/// Single PLA Basic job on the right nozzle, Textured PEI Plate.
fn single_pla_job() -> JobInfo {
    JobInfo::new(BedType::TexturedPeiPlate, vec![0], [155.0, 140.0], [195.0, 180.0], 20.0, 100)
}

fn snapshot_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("snapshots").join(name)
}

/// Compares against a checked-in snapshot; writes it when missing or when `UPDATE_SNAPSHOTS=1`.
fn assert_snapshot(name: &str, actual: &str) {
    let path = snapshot_path(name);
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap();
    if expected != actual {
        let line = expected
            .lines()
            .zip(actual.lines())
            .position(|(a, b)| a != b)
            .map_or_else(|| "length differs".to_string(), |i| format!("first difference at line {}", i + 1));
        panic!("snapshot {name} differs ({line}); rerun with UPDATE_SNAPSHOTS=1 to accept");
    }
}

#[test]
fn embedded_store_contains_h2d_presets() {
    let s = store();
    for (kind, name) in [
        (PresetKind::Machine, MACHINE),
        (PresetKind::Machine, "fdm_bbl_3dp_002_common"),
        (PresetKind::Machine, "Bambu Lab H2D 0.4 nozzle template machine_start_gcode"),
        (PresetKind::MachineModel, "Bambu Lab H2D"),
        (PresetKind::Process, PROCESS),
        (PresetKind::Filament, PLA),
        (PresetKind::Filament, PLA_MATTE),
        (PresetKind::Filament, PETG),
        (PresetKind::Filament, ASA),
        (PresetKind::Filament, "fdm_filament_template_direct_dual_e3d"),
    ] {
        assert!(s.contains(kind, name), "missing {kind} {name}");
    }
    assert_eq!(s.source(), &only3d_profiles::ProfileSource::Embedded);
}

#[test]
fn include_templates_override_inherited_gcode() {
    let s = store();
    let m = s.resolve(PresetKind::Machine, MACHINE).unwrap();
    let parent = s.resolve(PresetKind::Machine, "fdm_bbl_3dp_002_common").unwrap();
    for key in H2D_TEMPLATES {
        let tpl = s
            .raw(PresetKind::Machine, &format!("{MACHINE} template {key}"))
            .unwrap()
            .config
            .get_str(key)
            .unwrap()
            .to_string();
        assert_eq!(m.get_str(key), Some(tpl.as_str()), "{key} must come from the include template");
    }
    let start = m.get_str("machine_start_gcode").unwrap();
    assert!(start.contains("machine: H2D"));
    assert!(start.contains("M993 A0 B0 C0"));
    assert_ne!(Some(start), parent.get_str("machine_start_gcode"));
    // Keys that only the parents define are still inherited.
    assert_eq!(m.get_str("machine_pause_gcode"), parent.get_str("machine_pause_gcode"));
    assert_eq!(m.get_strs("physical_extruder_map").unwrap(), ["1", "0"]);
    // Meta keys are not part of the resolved config.
    assert!(!m.contains_key("inherits") && !m.contains_key("include") && !m.contains_key("name"));

    let chain = s.resolution_chain(PresetKind::Machine, MACHINE).unwrap();
    assert_eq!(chain[0], MACHINE);
    assert!(chain.contains(&"fdm_machine_common".to_string()));
    assert!(chain.iter().any(|n| n.ends_with("template machine_start_gcode")));
}

#[test]
fn include_precedence_matches_bambu() {
    let mut s = ProfileStore::empty(only3d_profiles::ProfileSource::Embedded);
    s.insert_json("machine/base.json", r#"{"name":"base","instantiation":"false","a":"base","b":"base","c":"base","d":"base"}"#)
        .unwrap();
    s.insert_json("machine/inc1.json", r#"{"name":"inc1","instantiation":"false","b":"inc1","c":"inc1"}"#).unwrap();
    s.insert_json("machine/inc2.json", r#"{"name":"inc2","instantiation":"false","c":"inc2","d":"inc2"}"#).unwrap();
    s.insert_json(
        "machine/child.json",
        r#"{"name":"child","inherits":"base","include":["inc1","inc2"],"instantiation":"true","d":"own"}"#,
    )
    .unwrap();
    s.insert_json("machine/grandchild.json", r#"{"name":"grandchild","inherits":"child","instantiation":"true"}"#)
        .unwrap();
    let c = s.resolve(PresetKind::Machine, "child").unwrap();
    assert_eq!(c.get_str("a"), Some("base")); // inherited
    assert_eq!(c.get_str("b"), Some("inc1")); // include over inherits
    assert_eq!(c.get_str("c"), Some("inc2")); // later include wins
    assert_eq!(c.get_str("d"), Some("own")); // own keys win
    // Includes of a parent propagate to children.
    assert_eq!(s.resolve(PresetKind::Machine, "grandchild").unwrap(), c);
    assert_eq!(s.effective_value(PresetKind::Machine, "grandchild", "c").unwrap().first(), Some("inc2"));

    s.insert_json("machine/broken.json", r#"{"name":"broken","include":["nope"]}"#).unwrap();
    assert!(matches!(
        s.resolve(PresetKind::Machine, "broken"),
        Err(only3d_profiles::ProfileError::MissingInclude { .. })
    ));
    s.insert_json("machine/loop1.json", r#"{"name":"loop1","inherits":"loop2"}"#).unwrap();
    s.insert_json("machine/loop2.json", r#"{"name":"loop2","inherits":"loop1"}"#).unwrap();
    assert!(matches!(s.resolve(PresetKind::Machine, "loop1"), Err(only3d_profiles::ProfileError::Cycle { .. })));
}

#[test]
fn filament_include_template_is_applied() {
    let s = store();
    let pla = s.resolve(PresetKind::Filament, PLA).unwrap();
    // From fdm_filament_template_direct_dual_e3d (include) ...
    assert_eq!(
        pla.get_strs("filament_extruder_variant").unwrap(),
        ["Direct Drive Standard", "Direct Drive High Flow", "Direct Drive E3D High Flow"]
    );
    assert!(pla.is_nil_at("filament_deretraction_speed", 0));
    assert_eq!(pla.get_f64_at("retraction_distances_when_ec", 2), Some(10.0));
    // ... but the preset's own keys win over the include.
    assert_eq!(pla.get_f64_at("filament_retraction_length", 2), Some(1.0));
    assert_eq!(pla.get_f64_at("filament_flow_ratio", 0), Some(0.98));
    assert_eq!(pla.get_f64_at("filament_max_volumetric_speed", 1), Some(40.0));
    // Inherited from Bambu PLA Basic @base -> fdm_filament_pla -> fdm_filament_common.
    assert_eq!(pla.get_str("filament_id"), Some("GFA00"));
    assert_eq!(pla.get_str("filament_vendor"), Some("Bambu Lab"));
    assert_eq!(pla.get_f64("textured_plate_temp_initial_layer"), Some(55.0));
    assert_eq!(pla.get_f64("nozzle_temperature_range_high"), Some(240.0));
}

#[test]
fn list_filters() {
    let s = store();
    let filaments = s.names(PresetKind::Filament, &ListFilter::compatible_with(MACHINE));
    assert_eq!(filaments, [ASA, PETG, PLA, PLA_MATTE]);
    assert!(s.names(PresetKind::Filament, &ListFilter::compatible_with("Bambu Lab X1 Carbon 0.4 nozzle")).is_empty());
    assert_eq!(s.names(PresetKind::Process, &ListFilter::compatible_with(MACHINE)), [PROCESS]);
    assert_eq!(s.names(PresetKind::Machine, &ListFilter::default()), [MACHINE]);
    let all = s.names(PresetKind::Filament, &ListFilter { include_abstract: true, ..Default::default() });
    assert!(all.contains(&"fdm_filament_common".to_string()));
    let pla_only = s.names(PresetKind::Filament, &ListFilter { name_contains: Some("PLA"), ..Default::default() });
    assert_eq!(pla_only, [PLA, PLA_MATTE]);
}

#[test]
fn variant_rows() {
    let p = presets(&store(), &[PLA]);
    let m = &p.machine;
    assert_eq!(m.variant_index(VariantTable::Printer, H2D_LEFT, "Direct Drive Standard"), Some(0));
    assert_eq!(m.variant_index(VariantTable::Printer, H2D_RIGHT, "Direct Drive Standard"), Some(3));
    assert_eq!(m.variant_index(VariantTable::Printer, H2D_RIGHT, "Direct Drive TPU High Flow"), Some(5));
    assert_eq!(p.process.variant_index(VariantTable::Process, H2D_RIGHT, "Direct Drive High Flow"), Some(4));
    assert_eq!(p.process.get_f64_at("outer_wall_speed", 3), Some(200.0));
    assert_eq!(p.process.get_f64_at("outer_wall_speed", 4), Some(500.0));
    assert_eq!(p.filaments[0].variant_index(VariantTable::Filament, H2D_RIGHT, "Direct Drive High Flow"), Some(1));
    // Every per-variant array of the resolved H2D presets has one entry per variant row.
    for (cfg, kind, rows) in [(&p.machine, PresetKind::Machine, 7), (&p.process, PresetKind::Process, 7)] {
        for (key, value) in cfg.iter() {
            if only3d_profiles::variants::is_variant_key(kind, key) && value.is_array() {
                let want = if only3d_profiles::variants::PRINTER_VARIANT_X2_KEYS.contains(&key) { rows * 2 } else { rows };
                assert_eq!(value.len(), want, "{kind} key {key}");
            }
        }
    }
}

#[test]
fn summaries() {
    let p = presets(&store(), &[PLA, PETG]);
    let ms = MachineSummary::from_config(&p.machine, &["Direct Drive Standard", "Direct Drive High Flow"]).unwrap();
    assert_eq!(ms.bed_size, [350.0, 320.0]);
    assert_eq!(ms.max_print_height, 325.0);
    assert_eq!(ms.extruders.len(), 2);
    assert_eq!(ms.master_extruder, Some(H2D_RIGHT));
    let left = &ms.extruders[H2D_LEFT];
    let right = &ms.extruders[H2D_RIGHT];
    assert_eq!(left.physical_index, 1);
    assert_eq!(right.physical_index, 0);
    assert_eq!(right.variant_row, 4);
    assert_eq!(left.printable_area, vec![[0.0, 0.0], [325.0, 0.0], [325.0, 320.0], [0.0, 320.0]]);
    assert_eq!(right.printable_area[0], [25.0, 0.0]);
    assert_eq!(left.nozzle_diameter, 0.4);
    assert_eq!(left.limits.max_speed.x, 1000.0);
    assert_eq!(left.limits.max_acceleration.x, 20000.0);
    assert_eq!(left.retraction.length, 0.8);
    assert_eq!(left.retraction.z_hop, 0.4);
    assert_eq!(left.retraction.z_hop_type, "Auto Lift");
    assert!(right.supported_variants.contains(&"Direct Drive TPU High Flow".to_string()));
    assert_eq!(ms.clearance.radius, 49.0);

    let pla = FilamentSummary::from_config(&p.filaments[0], "Direct Drive High Flow").unwrap();
    assert_eq!(pla.filament_type, "PLA");
    assert_eq!(pla.max_volumetric_speed, 40.0);
    assert_eq!(pla.nozzle_temperature, 220.0);
    assert_eq!(pla.textured_plate_temp.initial_layer, 55.0);
    assert_eq!(pla.plate_temps(BedType::TexturedPeiPlate), pla.textured_plate_temp);
    assert_eq!(pla.retraction.length, Some(0.4));
    assert_eq!(pla.retraction.deretraction_speed, None); // nil: use the machine's
    assert_eq!(pla.retraction.z_hop_type.as_deref(), Some("Spiral Lift"));
    assert!(pla.density > 1.2 && pla.cost > 0.0);
    let petg = FilamentSummary::from_config(&p.filaments[1], "Direct Drive Standard").unwrap();
    assert_eq!(petg.filament_type, "PETG");

    let ps = ProcessSummary::from_config(&p.process, H2D_RIGHT, "Direct Drive Standard", 0.4).unwrap();
    assert_eq!(ps.layer_height, 0.2);
    assert_eq!(ps.speeds.outer_wall, 200.0);
    assert_eq!(ps.speeds.small_perimeter, 100.0); // 50% of the outer wall speed
    assert_eq!(ps.accelerations.initial_layer, 500.0);
    assert_eq!(ps.sparse_infill_density, 15.0);
    assert!(ps.line_widths.outer_wall > 0.3);
    let hf = ProcessSummary::from_config(&p.process, H2D_RIGHT, "Direct Drive High Flow", 0.4).unwrap();
    assert_eq!(hf.speeds.outer_wall, 500.0);
}

/// A realistic two-filament context: PLA Basic on the right nozzle, PETG Basic on the left.
fn two_filament_setup(p: &Presets) -> PrintSetup<'_> {
    PrintSetup::new(&p.machine, &p.process)
        .with_filament(&p.filaments[0], H2D_RIGHT)
        .with_filament(&p.filaments[1], H2D_LEFT)
        .with_extruder_variants(&["Direct Drive Standard", "Direct Drive Standard"])
}

fn two_filament_job() -> JobInfo {
    let mut job = JobInfo::new(BedType::TexturedPeiPlate, vec![0, 1, 0, 1], [120.0, 100.0], [230.0, 200.0], 160.4, 801);
    job.first_layer_filaments = vec![0, 1];
    job.has_wipe_tower = true;
    job.total_toolchanges = 3;
    job.wipe_tower_stop_pos = Some([262.0, 250.0]);
    job
}

/// Per-template event variables for the two-filament job.
fn event_vars(setup: &PrintSetup<'_>, template: &str) -> Context {
    let layer = LayerEvent {
        layer_num: 10,
        layer_z: 2.2,
        max_layer_z: 2.2,
        current_filament: 0,
        most_used_extruder: H2D_RIGHT,
        ..Default::default()
    };
    match template {
        "machine_start_gcode" => Context::new(),
        "layer_change_gcode" => setup.layer_change_vars(&layer),
        "before_layer_change_gcode" => setup.before_layer_change_vars(&layer),
        "wrapping_detection_gcode" => setup.wrapping_detection_vars(&layer),
        "time_lapse_gcode" => setup.timelapse_vars(
            &layer,
            &TimelapseEvent { safe_pos: Some([300, 310]), farthest_point_enabled: true, ..Default::default() },
        ),
        "machine_end_gcode" => setup.end_vars(&LayerEvent { layer_num: 800, layer_z: 160.4, max_layer_z: 160.4, ..layer }, &[0, 1]),
        "change_filament_gcode" => setup
            .toolchange_vars(&ToolChange {
                old_filament: Some(0),
                new_filament: 1,
                layer_num: 10,
                layer_z: 2.2,
                max_layer_z: 2.2,
                toolchange_z: 2.2,
                toolchange_count: 1,
                purge_volume: 250.0,
                new_extruder_retracted_length: 0.8,
                position_after: [262.0, 250.0, 2.6],
                travel_points: [[300.0, 290.0], [320.0, 300.0], [330.0, 310.0]],
                wipe_tower_stop_pos: [262.0, 250.0],
                used_filaments: vec![0, 1],
                ..Default::default()
            })
            .unwrap(),
        other => panic!("no event variables for {other}"),
    }
}

#[test]
fn renders_every_h2d_template_without_errors() {
    let s = store();
    let p = presets(&s, &[PLA, PETG]);
    let setup = two_filament_setup(&p);
    let print_vars = job::build_print_context(&setup, &two_filament_job()).unwrap();
    let mut rendered = 0;
    for key in GCODE_TEMPLATE_KEYS {
        let Some(src) = p.machine.get_str(key) else { continue };
        if !H2D_TEMPLATES.contains(key) && *key != "before_layer_change_gcode" {
            continue;
        }
        if src.trim().is_empty() {
            continue;
        }
        let t = Template::named(*key, src).unwrap();
        let ev = event_vars(&setup, key);
        let missing = t.missing_variables(&[&ev, &print_vars]);
        assert!(missing.is_empty(), "{key}: context lacks {missing:?}");
        let out = t.render_layered(&[&ev, &print_vars]).unwrap_or_else(|e| panic!("{e}"));
        assert!(!out.contains('{') && !out.contains('}'), "{key}: unexpanded macro in\n{out}");
        assert!(!out.contains("[initial") && !out.contains("[next_") && !out.contains("[current_"), "{key}: unexpanded [var]");
        rendered += 1;
    }
    assert_eq!(rendered, H2D_TEMPLATES.len());

    // Also every template of every *other* machine preset known to Bambu? Only the H2D is embedded;
    // the filament start/end G-code must render too.
    for (f, cfg) in p.filaments.iter().enumerate() {
        for key in ["filament_start_gcode", "filament_end_gcode"] {
            let src = cfg.get_str(key).unwrap();
            let mut ev = Context::new();
            ev.set("filament_extruder_id", f as i64).set_current_filament_id(f);
            Template::parse(src).unwrap().render_layered(&[&ev, &print_vars]).unwrap();
        }
    }
}

#[test]
fn h2d_template_variables_are_documented() {
    let doc = std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("H2D_VARIABLES.md")).unwrap();
    let p = presets(&store(), &[]);
    let mut all = std::collections::BTreeSet::new();
    for key in H2D_TEMPLATES {
        all.extend(only3d_profiles::required_variables(p.machine.get_str(key).unwrap()).unwrap());
    }
    let undocumented: Vec<_> = all.iter().filter(|v| !doc.contains(&format!("`{v}`"))).collect();
    assert!(undocumented.is_empty(), "H2D_VARIABLES.md lacks {undocumented:?}");
}

#[test]
fn start_gcode_snapshot_single_pla_right_nozzle() {
    let s = store();
    let p = presets(&s, &[PLA]);
    let setup = PrintSetup::new(&p.machine, &p.process).with_filament(&p.filaments[0], H2D_RIGHT);
    let ctx = job::build_print_context(&setup, &single_pla_job()).unwrap();
    let t = Template::named("machine_start_gcode", p.machine.get_str("machine_start_gcode").unwrap()).unwrap();
    let out = t.render(&ctx).unwrap();

    // Sanity checks on the genuine H2D sequence.
    assert!(out.starts_with(";===== machine: H2D"));
    assert!(out.contains("M993 A0 B0 C0 ; nozzle cam detection not allowed."));
    assert!(out.contains("G28 X T300"), "first homing");
    assert!(out.contains("G28 Z P0 T250"));
    assert!(out.contains("M140 S55\n"), "bed to 55 °C");
    assert!(out.contains("M190 S55; ensure bed temp"));
    assert!(out.contains("M104 S220 T0 ; rise temp in advance"), "right nozzle is firmware T0");
    assert!(out.contains("G151 P0 M"));
    assert!(out.contains("M109 S220"));
    assert!(out.contains("M620 S0A H-1"));
    assert!(out.contains("T0 H-1"));
    assert!(out.contains("M1002 set_filament_type:PLA"));
    assert!(out.contains("M620.10 A0 F498.898 H0.4 T240 P220 S1"), "flush speed/temp fallbacks + %g formatting");
    assert!(out.contains("M142 P1 R30 S40 T45 U0.3 V0.5 W0.8 O45"), "PLA chamber auto-cooling");
    assert!(out.contains("M972 S26 P0 C0"), "Textured PEI branch");
    assert!(out.contains("G29 A1 O X155 Y140 I40 J40 R"), "bed mesh limited to the first layer");
    assert!(out.contains("G29.1 Z-0.02 ; for Textured PEI Plate"));
    assert!(out.contains("G29.1 Z0 ; clear z-trim value first"));
    assert!(out.contains("M620.17 T0 S220 L0"));
    assert!(out.contains("M620.17 T1 S220 L0"), "unused left nozzle falls back to filament 0");
    assert!(out.contains("G1 E45 F623.623"), "0.4 nozzle purge");
    assert!(out.contains("M1015.4 S1 K1 H0.4"));
    assert!(!out.contains("G3811"), "short print: no underside detection");
    assert!(!out.contains('{') && !out.contains('}') && !out.contains('['), "leftover macro");
    // Not the generic Bambu start code.
    assert!(!out.contains("G1 X10"));

    assert_snapshot("h2d_start_single_pla_right_textured_pei.gcode.snap", &out);
}

#[test]
fn other_h2d_templates_render_sensibly() {
    let s = store();
    let p = presets(&s, &[PLA, PETG]);
    let setup = two_filament_setup(&p);
    let print_vars = job::build_print_context(&setup, &two_filament_job()).unwrap();
    let render = |key: &str| {
        Template::named(key, p.machine.get_str(key).unwrap())
            .unwrap()
            .render_layered(&[&event_vars(&setup, key), &print_vars])
            .unwrap()
    };
    let layer = render("layer_change_gcode");
    assert!(layer.contains("; layer num/total_layer_count: 11/801"));
    assert!(layer.contains("M73 L11\nM991 S0 P10"));

    let end = render("machine_end_gcode");
    assert!(end.contains("G1 Z160.8 F900"));
    assert!(end.contains("G1 Z170.4 F900"));
    assert!(end.contains("G1 Z180.2 F600\n        G1 Z178.2"), "end lift: 160.4 + 100 - 80.2");

    let tc = render("change_filament_gcode");
    assert!(tc.contains("M620 S1A H-1"));
    assert!(tc.contains("T1 H-1"));
    assert!(tc.contains("M1002 set_filament_type:PETG"));
    assert!(tc.contains("G1 Z5.2 F1200"));
    // PETG Basic is on the left nozzle (filament_map 1): firmware T1.
    let tl = render("time_lapse_gcode");
    assert!(tl.contains("M9711 M0 E0 X300 Y310 Z2.2 S11 C10 O0 T3000"));
    let wrap = render("wrapping_detection_gcode");
    assert!(wrap.contains("M622.1 S0"));
    assert!(wrap.contains("\n            G39\n"), "layer 10 is a detection layer");
}

#[test]
fn print_context_values() {
    let p = presets(&store(), &[PLA, PETG]);
    let setup = two_filament_setup(&p);
    let c = job::build_print_context(&setup, &two_filament_job()).unwrap();
    use only3d_profiles::Value::*;
    assert_eq!(c.get("filament_map"), Some(&Ints(vec![2, 1])));
    assert_eq!(c.get("first_filaments"), Some(&Ints(vec![0, 1])));
    assert_eq!(c.get("first_non_support_filaments"), Some(&Ints(vec![0, 1])));
    assert_eq!(c.get("initial_no_support_filament_id"), Some(&Int(0)));
    assert_eq!(c.get("initial_nozzle_id"), Some(&Int(1)));
    assert_eq!(c.get("nozzle_diameter_at_nozzle_id"), Some(&Floats(vec![0.4, 0.4])));
    assert_eq!(c.get("nozzle_volume_types"), Some(&Strs(vec!["Standard".into(), "Standard".into()])));
    assert_eq!(c.get("max_print_z"), Some(&Int(161)));
    assert_eq!(c.get("bed_temperature_initial_layer_single"), Some(&Int(70)), "highest of PLA 55 / PETG 70");
    assert_eq!(c.get("min_vitrification_temperature"), Some(&Int(45)));
    assert_eq!(c.get("is_all_bbl_filament"), Some(&Bool(true)));
    assert_eq!(c.get("timelapse_type"), Some(&Str("0".into())));
    assert_eq!(c.get("curr_bed_type"), Some(&Str("Textured PEI Plate".into())));
    assert_eq!(c.get("retraction_distances_when_cut"), Some(&Floats(vec![10.0, 10.0])));
    assert_eq!(c.get("long_retraction_when_ec"), Some(&Bool(true)));
    assert_eq!(c.get("retraction_distance_when_ec"), Some(&Float(10.0)));
    assert_eq!(c.get("hotend_cooling_rate"), Some(&Floats(vec![2.0, 2.0])));
    assert_eq!(c.get("initial_layer_acceleration"), Some(&Floats(vec![500.0, 500.0])));
    assert_eq!(c.current_filament_id(), Some(0));
}

/// Drops the `M73 P<percent> R<minutes>` progress lines Bambu's G-code processor inserts *after*
/// template rendering (they are not template output).
fn strip_progress(s: &str) -> String {
    s.lines()
        .filter(|l| {
            let rest = l.strip_prefix("M73 P").unwrap_or("");
            !(rest.contains(" R") && rest.chars().all(|c| c.is_ascii_digit() || c == ' ' || c == 'R'))
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

fn reference(name: &str) -> String {
    std::fs::read_to_string(snapshot_path("bambu_reference").join(name)).unwrap()
}

/// The `tests/snapshots/bambu_reference/*.snap` files were cut out of G-code written by Bambu
/// Studio 02.09.01.70 itself (`BambuStudio --slice 0`, presets flattened by this crate,
/// Textured PEI Plate; Bambu's automatic filament mapping put the single filament on the
/// **left** nozzle). Our rendering must be byte-identical, apart from Bambu's post-processed
/// `M73 P.. R..` progress lines.
#[test]
fn matches_bambu_studio_reference_output() {
    let s = store();
    let p = presets(&s, &[PLA, PETG]);
    let machine = &p.machine;
    let render = |setup: &PrintSetup<'_>, job: &JobInfo, key: &str, ev: &Context| {
        let print = job::build_print_context(setup, job).unwrap();
        Template::named(key, machine.get_str(key).unwrap()).unwrap().render_layered(&[ev, &print]).unwrap()
    };

    // 40x40x20 mm cube, PLA Basic, arranged at the bed centre.
    let pla_left = PrintSetup::new(machine, &p.process).with_filament(&p.filaments[0], H2D_LEFT);
    let cube = JobInfo::new(BedType::TexturedPeiPlate, vec![0], [155.0, 140.0], [195.0, 180.0], 20.0, 100);
    let start = render(&pla_left, &cube, "machine_start_gcode", &Context::new());
    assert_eq!(strip_progress(&start), strip_progress(&reference("start_pla_basic_left_cube40.snap")));
    assert!(start.contains("M104 S220 T1 ; rise temp in advance"), "left nozzle is firmware T1");

    let layer = LayerEvent { layer_num: 1, layer_z: 0.4, max_layer_z: 0.4, ..Default::default() };
    let lc = render(&pla_left, &cube, "layer_change_gcode", &pla_left.layer_change_vars(&layer));
    assert_eq!(strip_progress(&lc), strip_progress(&reference("layer_change_layer_index1.snap")));

    let tl_vars = pla_left.timelapse_vars(&layer, &TimelapseEvent { inline_photo: true, ..Default::default() });
    let tl = render(&pla_left, &cube, "time_lapse_gcode", &tl_vars);
    assert_eq!(strip_progress(&tl), strip_progress(&reference("time_lapse_inline_photo.snap")));

    let last = LayerEvent { layer_num: 99, layer_z: 20.4, max_layer_z: 20.0, ..Default::default() };
    let end = render(&pla_left, &cube, "machine_end_gcode", &pla_left.end_vars(&last, &[0]));
    assert_eq!(strip_progress(&end), strip_progress(&reference("end_pla_left_maxz20.snap")));

    // 20x30x150.3 mm block in PETG Basic: auto brim enlarges the first-layer box, the part is
    // tall enough for the heatbed-underside scan (G3811) and PETG selects other chamber cooling.
    let petg_left = PrintSetup::new(machine, &p.process).with_filament(&p.filaments[1], H2D_LEFT);
    let tall = JobInfo::new(BedType::TexturedPeiPlate, vec![0], [156.673, 136.673], [193.3275, 183.3275], 150.4, 752);
    let start = render(&petg_left, &tall, "machine_start_gcode", &Context::new());
    assert_eq!(strip_progress(&start), strip_progress(&reference("start_petg_basic_left_tall150.snap")));
    assert!(start.contains("G3811 Z151"));
}

#[test]
fn system_profiles_resolve_like_embedded() {
    // Uses the locally installed Bambu Studio when present; otherwise this is the embedded copy.
    let sys = ProfileStore::system().unwrap();
    let m = sys.resolve(PresetKind::Machine, MACHINE).unwrap();
    let start = m.get_str("machine_start_gcode").unwrap();
    assert!(start.contains("machine: H2D") && start.contains("M993"));
    let t = Template::parse(start).unwrap();
    let embedded = presets(&store(), &[PLA]);
    let setup = PrintSetup::new(&embedded.machine, &embedded.process).with_filament(&embedded.filaments[0], H2D_RIGHT);
    let ctx = job::build_print_context(&setup, &single_pla_job()).unwrap();
    t.render(&ctx).unwrap();
}

/// Every preset of the installed Bambu Studio (or the embedded subset) resolves, and every G-code
/// template of every printer and filament parses.
#[test]
fn all_system_presets_resolve_and_templates_parse() {
    let sys = ProfileStore::system().unwrap();
    let mut parsed = 0;
    for kind in [PresetKind::Machine, PresetKind::Process, PresetKind::Filament] {
        for p in sys.list(kind, &ListFilter::default()) {
            let cfg = sys.resolve(kind, &p.name).unwrap_or_else(|e| panic!("{e}"));
            let keys: &[&str] = match kind {
                PresetKind::Machine => GCODE_TEMPLATE_KEYS,
                PresetKind::Filament => &["filament_start_gcode", "filament_end_gcode"],
                _ => &[],
            };
            for key in keys {
                for src in cfg.get_strs(key).unwrap_or_default() {
                    Template::named(*key, src.as_str()).unwrap_or_else(|e| panic!("{} {}: {e}", p.name, key));
                    parsed += 1;
                }
            }
        }
    }
    assert!(parsed >= 6);
}
