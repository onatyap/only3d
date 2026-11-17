//! Tolerant status parsing + delta merging against realistic H2D / X1C
//! report samples in `tests/data/`.

use only3d_bambu::status::StateCache;
use only3d_bambu::{GcodeState, PrinterStatus, TrayRef};
use serde_json::Value;

fn sample(name: &str) -> Value {
    let path = format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

#[test]
fn h2d_full_report() {
    let st = PrinterStatus::from_state(&sample("h2d_pushall.json"));
    assert_eq!(st.gcode_state, GcodeState::Running);
    assert_eq!(st.progress_percent, Some(6));
    assert_eq!(st.remaining_minutes, Some(549));
    assert_eq!((st.layer, st.total_layers), (Some(12), Some(200)));
    assert_eq!(st.subtask_name.as_deref(), Some("T-pose - slim H2D dual AMS riser"));
    assert_eq!(st.gcode_file.as_deref(), Some("/data/Metadata/plate_1.gcode"));
    assert_eq!(st.print_error, None);
    assert_eq!((st.bed.current, st.bed.target), (Some(70.0), Some(70.0)));
    let chamber = st.chamber.unwrap();
    assert_eq!((chamber.current, chamber.target), (Some(35.0), None));

    // Both nozzles from device.extruder.info (packed temps), not the
    // top-level nozzle_temper which only mirrors the active one.
    assert_eq!(st.nozzles.len(), 2);
    let right = st.nozzle(0).unwrap();
    assert_eq!(right.side.as_deref(), Some("right"));
    assert_eq!((right.temperature.current, right.temperature.target), (Some(245.0), Some(245.0)));
    assert_eq!(right.diameter_mm, Some(0.4));
    assert_eq!(right.nozzle_type.as_deref(), Some("HS01"));
    assert_eq!(right.nozzle_material.as_deref(), Some("hardened_steel"));
    assert_eq!(right.loaded_tray, Some(TrayRef { ams_id: 1, slot_id: 3 }));
    assert!(right.active);
    let left = st.nozzle(1).unwrap();
    assert_eq!(left.side.as_deref(), Some("left"));
    assert_eq!((left.temperature.current, left.temperature.target), (Some(47.0), Some(0.0)));
    assert_eq!(left.diameter_mm, Some(0.6));
    assert_eq!(left.nozzle_material.as_deref(), Some("high_flow_hardened_steel"));
    assert_eq!(left.loaded_tray, None);
    assert!(!left.active);
    assert_eq!(st.active_extruder, Some(0));

    assert_eq!(st.fans.part_cooling, Some(100));
    assert_eq!(st.fans.aux, Some(0));
    assert_eq!(st.fans.chamber, Some(30));
    assert_eq!(st.fans.heatbreak, Some(100));
    assert_eq!(st.speed_level, Some(2));
    assert_eq!(st.chamber_light, Some(true));
    assert_eq!(st.lights.len(), 3);
    assert_eq!(st.wifi_signal_dbm, Some(-46));
    assert_eq!(st.sdcard, Some(true));

    // AMS: unit 0 feeds the left extruder, unit 1 the right one.
    assert_eq!(st.ams.len(), 2);
    let a0 = &st.ams[0];
    assert_eq!(a0.extruder, Some(1));
    assert_eq!((a0.humidity_level, a0.humidity_percent), (Some(5), Some(21)));
    assert_eq!(a0.temperature, Some(27.0));
    assert_eq!(a0.trays.len(), 4);
    assert_eq!(a0.trays[0].filament_type.as_deref(), Some("PLA-S"));
    assert_eq!(a0.trays[0].remain_percent, Some(1));
    assert!(!a0.trays[3].loaded, "slot without filament");
    assert_eq!(st.ams[1].extruder, Some(0));
    assert_eq!(st.ams[1].trays[1].color.as_deref(), Some("F72323FF"));
    assert_eq!(st.ams[1].trays[1].global_id, 5);
    assert!(st.ams[1].trays.iter().all(|t| t.loaded));

    // Two external spool holders on H2D.
    assert_eq!(st.external_spools.len(), 2);
    let e255 = st.external_spools.iter().find(|t| t.ams_id == 255).unwrap();
    assert!(e255.loaded);
    assert_eq!(e255.filament_type.as_deref(), Some("PETG"));
    assert_eq!(e255.remain_percent, None);
    assert!(!st.external_spools.iter().find(|t| t.ams_id == 254).unwrap().loaded);

    assert_eq!(st.hms.len(), 1);
    assert_eq!(st.hms[0].id, "0701_8000_0003_0002");
    assert_eq!(st.hms[0].module, "ams");
    assert_eq!(st.hms[0].severity, "common");
}

#[test]
fn h2d_deltas_merge_into_cache() {
    let mut cache = StateCache::new();
    assert!(cache.apply(&sample("h2d_pushall.json")));
    assert!(cache.apply(&sample("h2d_delta_temps.json")));
    let st = PrinterStatus::from_state(cache.raw());
    // Only extruder 1 was in the delta; extruder 0 keeps its values.
    let left = st.nozzle(1).unwrap();
    assert_eq!((left.temperature.current, left.temperature.target), (Some(220.0), Some(220.0)));
    assert_eq!(left.loaded_tray, Some(TrayRef { ams_id: 0, slot_id: 2 }));
    assert_eq!(st.nozzle(0).unwrap().temperature.current, Some(245.0));
    assert_eq!(st.nozzle(0).unwrap().loaded_tray, Some(TrayRef { ams_id: 1, slot_id: 3 }));
    assert_eq!(st.active_extruder, Some(1));
    assert!(left.active);
    assert_eq!(st.progress_percent, Some(7));
    assert_eq!(st.layer, Some(13));
    assert_eq!(st.wifi_signal_dbm, Some(-51));
    // Untouched fields survive.
    assert_eq!(st.subtask_name.as_deref(), Some("T-pose - slim H2D dual AMS riser"));
    assert_eq!(cache.raw()["print"]["device"]["extruder"]["info"][1]["filam_bak"], serde_json::json!([10]));

    assert!(cache.apply(&sample("h2d_delta_ams.json")));
    let st = PrinterStatus::from_state(cache.raw());
    assert_eq!(st.ams.len(), 2, "partial AMS delta must not drop unit 0");
    assert_eq!(st.ams[1].trays.len(), 4);
    assert_eq!(st.ams[1].trays[0].remain_percent, Some(42));
    assert_eq!(st.ams[1].trays[0].filament_type.as_deref(), Some("PLA"));
    assert!(st.hms.is_empty(), "empty hms array clears errors");
    assert_eq!(st.chamber_light, Some(false));

    assert!(cache.apply(&sample("h2d_get_version.json")));
    let st = PrinterStatus::from_state(cache.raw());
    assert_eq!(st.firmware_version.as_deref(), Some("01.02.00.00"));
    assert_eq!(st.product_name.as_deref(), Some("Bambu Lab H2D"));
    assert_eq!(only3d_bambu::lookup_model("Bambu Lab H2D").unwrap().code, "O1D");

    // A command reply is not state.
    assert!(!cache.apply(&sample("h2d_project_file_ack.json")));
    assert!(cache.raw()["print"].get("result").is_none());
}

#[test]
fn x1c_classic_fields() {
    let st = PrinterStatus::from_state(&sample("x1c_pushall.json"));
    assert_eq!(st.gcode_state, GcodeState::Idle);
    assert_eq!(st.nozzles.len(), 1);
    let n = &st.nozzles[0];
    assert_eq!((n.temperature.current, n.temperature.target), (Some(26.0), Some(0.0)));
    assert_eq!(n.side, None);
    assert_eq!(n.diameter_mm, Some(0.4));
    assert_eq!(n.nozzle_material.as_deref(), Some("hardened_steel"));
    assert!(n.active);
    assert_eq!(st.active_extruder, Some(0));
    assert_eq!((st.bed.current, st.bed.target), (Some(23.4), Some(0.0)));
    assert_eq!(st.chamber.unwrap().current, Some(25.0));
    assert_eq!(st.print_error.as_ref().unwrap().id, "0500_4004");
    assert_eq!(st.hms[0].id, "0300_0100_0001_0007");
    assert_eq!(st.hms[0].severity, "fatal");
    assert_eq!(st.ams.len(), 1);
    assert!(st.ams[0].trays[0].loaded);
    assert!(!st.ams[0].trays[1].loaded);
    assert_eq!(st.external_spools.len(), 1);
    assert!(!st.external_spools[0].loaded);
}

#[test]
fn every_prefix_of_a_report_parses() {
    // Tolerance check: drop each top-level print key in turn; parsing never panics
    // and serialises to JSON.
    let full = sample("h2d_pushall.json");
    let keys: Vec<String> = full["print"].as_object().unwrap().keys().cloned().collect();
    for k in keys {
        let mut v = full.clone();
        v["print"].as_object_mut().unwrap().remove(&k);
        let st = PrinterStatus::from_state(&v);
        serde_json::to_string(&st).unwrap();
    }
}
