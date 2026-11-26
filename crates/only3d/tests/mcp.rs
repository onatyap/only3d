//! End-to-end MCP test: drives `only3d mcp --no-live` over stdio the way an agent does.
//! Runs with a throwaway ONLY3D_HOME, so it never touches a printer or the user's data.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
    home: std::path::PathBuf,
}

impl Session {
    fn start() -> Session {
        // tests run in parallel: each session gets its own home
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let k = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let home = std::env::temp_dir().join(format!("only3d-mcp-test-{}-{k}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_only3d"))
            .args(["mcp", "--no-live"])
            .env("ONLY3D_HOME", &home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start only3d mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Session { child, stdin, stdout, next: 1, home }
    }

    fn notify(&mut self, method: &str) {
        writeln!(self.stdin, "{}", json!({"jsonrpc": "2.0", "method": method})).unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        writeln!(self.stdin, "{}", json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).unwrap();
        self.stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "server closed stdout");
            let v: Value = serde_json::from_str(&line).expect("one JSON-RPC message per line");
            if v["id"] == json!(id) {
                return v;
            }
        }
    }

    fn call(&mut self, tool: &str, args: Value) -> Value {
        let r = self.request("tools/call", json!({"name": tool, "arguments": args}));
        r["result"].clone()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

#[test]
fn an_agent_can_model_check_slice_and_look() {
    let mut s = Session::start();

    let init = s.request("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}));
    assert_eq!(init["result"]["serverInfo"]["name"], "only3d");
    assert!(init["result"]["instructions"].as_str().unwrap().contains("describe"));
    s.notify("notifications/initialized");

    // the tool list: every tool has a schema, read-only tools say so, and it stays affordable
    let list = s.request("tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    assert!(tools.len() > 50);
    let size = serde_json::to_string(&list["result"]).unwrap().len();
    // ~14k tokens. Raised from 52k for Phase 2's shape grammar (slot, tube, polygon, wedge,
    // text, shell, holes), the core of what agents model with; Claude Code defers big tool sets
    // anyway. Grow existing tools (ops, fields) rather than adding new ones. 56.5k: `session`,
    // which keeps agents out of the wrong window (or none). 58k: add_object's shape grammar as
    // text too, since Codex shows descriptions but not `$ref` schemas (it got `shape: unknown`).
    // 60k: `assembly` (parts fitted together with hardware that isn't printed), kept to ~1k by
    // describing its lists in words; views and cutaways stay in `render`. 60.5k: AMS drying, as
    // `printer_ams` actions (`dry`, `stop_drying`) rather than a tool of its own (~340 chars).
    assert!(size < 60_500, "tools/list grew to {size} chars (~{} tokens)", size / 4);
    let find = |n: &str| tools.iter().find(|t| t["name"] == n).unwrap_or_else(|| panic!("tool {n}"));
    assert_eq!(find("describe")["annotations"]["readOnlyHint"], true);
    assert!(find("add_object")["inputSchema"]["$defs"].get("Node").is_some());
    assert!(find("update_object")["inputSchema"].get("$defs").is_none(), "the shape grammar lives in add_object only");
    assert_eq!(find("update_object")["inputSchema"]["properties"]["parts"]["type"], "array");
    assert!(find("add_object")["inputSchema"]["properties"].get("parts").is_some());

    // model
    let add = s.call("add_object", json!({"id": "plate", "shape": {"type": "box", "size": [40, 30, 4], "corner_radius": 3}}));
    assert_eq!(add["isError"], false, "{add}");
    let add = s.call("add_object", json!({"id": "peg", "shape": {"type": "cylinder", "d": "8", "h": 10}, "position": [120, 100]}));
    assert_eq!(add["isError"], false, "{add}");

    // every result says what happened, in the part clients actually show the model
    // (Claude Code shows structuredContent instead of the text when both are present)
    let p = s.call("set_params", json!({"params": {"wall": 2}}));
    assert!(p.get("structuredContent").is_none(), "data-less tools send text only: {p}");
    assert!(p["content"][0]["text"].as_str().unwrap().contains("wall=2"), "{p}");
    let c = s.call("check", json!({}));
    assert!(c["structuredContent"]["summary"].as_str().is_some_and(|t| !t.is_empty()), "{c}");

    // look at the session the way an agent does: text for the model, structure for code
    let d = s.call("describe", json!({}));
    let text = d["structuredContent"]["summary"].as_str().unwrap();
    assert!(text.contains("plate") && text.contains("peg"), "{text}");
    assert_eq!(d["structuredContent"]["objects"].as_array().unwrap().len(), 2);

    // verify before claiming success
    let c = s.call("check", json!({}));
    assert_eq!(c["isError"], false, "{c}");

    let sl = s.call("slice", json!({}));
    assert_eq!(sl["isError"], false, "{sl}");
    assert!(sl["structuredContent"]["filament_g"].as_f64().unwrap_or(0.0) > 0.0, "{sl}");
    assert!(sl["structuredContent"]["summary"].as_str().unwrap().contains("layers"), "{sl}");

    // images come back as MCP image content
    let r = s.call("render", json!({}));
    assert_eq!(r["isError"], false, "{r}");
    assert!(r["content"].as_array().unwrap().iter().any(|c| c["type"] == "image" && c["mimeType"] == "image/png"), "no PNG in {}", r["content"]);

    // errors are tool results with a code and a hint, not protocol failures
    let bad = s.call("update_object", json!({"id": "nope", "name": "x"}));
    assert_eq!(bad["isError"], true);
    assert!(bad["structuredContent"]["error"]["hint"].as_str().is_some_and(|h| !h.is_empty()), "{bad}");

    // live state as resources, and the event log
    let res = s.request("resources/list", json!({}));
    let uris: Vec<&str> = res["result"]["resources"].as_array().unwrap().iter().filter_map(|r| r["uri"].as_str()).collect();
    assert!(uris.contains(&"only3d://project") && uris.contains(&"only3d://events"), "{uris:?}");
    let proj = s.request("resources/read", json!({"uri": "only3d://project"}));
    let text = proj["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(text.contains("\"peg\"") || text.contains("peg"), "{text}");
    let ev = s.call("wait_for_event", json!({"since": 0, "kinds": ["project_changed", "sliced"], "timeout_s": 1}));
    let kinds: Vec<&str> = ev["structuredContent"]["events"].as_array().unwrap().iter().filter_map(|e| e["kind"].as_str()).collect();
    assert!(kinds.contains(&"project_changed") && kinds.contains(&"sliced"), "{kinds:?}");

    // ready-made workflows
    let pl = s.request("prompts/list", json!({}));
    let names: Vec<&str> = pl["result"]["prompts"].as_array().unwrap().iter().filter_map(|p| p["name"].as_str()).collect();
    assert!(names.contains(&"design_part") && names.contains(&"print_plate"), "{names:?}");
    let pg = s.request("prompts/get", json!({"name": "design_part", "arguments": {"what": "a 30 mm hex pen cup"}}));
    let msg = pg["result"]["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(msg.contains("a 30 mm hex pen cup") && msg.contains("set_params"), "{msg}");
    let missing = s.request("prompts/get", json!({"name": "design_part", "arguments": {}}));
    assert!(missing.get("error").is_some(), "{missing}");

    // undo is a tool too: first the param change, then the second object
    let u = s.call("undo", json!({}));
    assert_eq!(u["isError"], false, "{u}");
    assert!(u["content"][0]["text"].as_str().unwrap().starts_with("Undone"), "{u}");
    let d = s.call("describe", json!({}));
    assert!(d["structuredContent"]["params"].get("wall").is_none(), "{d}");
    s.call("undo", json!({}));
    let d = s.call("describe", json!({}));
    assert_eq!(d["structuredContent"]["objects"].as_array().unwrap().len(), 1);
}

#[test]
fn an_agent_can_lay_out_several_plates() {
    let mut s = Session::start();
    s.request("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}));
    s.notify("notifications/initialized");
    let list = s.request("tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    let find = |n: &str| tools.iter().find(|t| t["name"] == n).unwrap_or_else(|| panic!("tool {n}"));
    assert_eq!(find("plates")["annotations"]["readOnlyHint"], true);
    assert!(find("plate")["description"].as_str().unwrap().contains("move objects"));

    for k in 0..4 {
        let r = s.call("add_object", json!({"id": format!("ghost_{k}"), "shape": {"type": "cylinder", "d": 30, "h": 8}}));
        assert_eq!(r["isError"], false, "{r}");
    }
    let r = s.call("plate", json!({"action": "add", "name": "Pumpkins"}));
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["active"], 2, "{r}");
    for k in 0..3 {
        s.call("add_object", json!({"id": format!("pumpkin_{k}"), "shape": {"type": "cylinder", "d": 25, "h": 20}}));
    }
    // the active plate is what slicing sees
    let sl = s.call("slice", json!({}));
    assert_eq!(sl["isError"], false, "{sl}");
    let d = s.call("describe", json!({}));
    let objs = d["structuredContent"]["objects"].as_array().unwrap();
    assert_eq!(objs.len(), 3, "describe details the active plate: {d}");
    assert_eq!(d["structuredContent"]["plates"][0]["objects"].as_array().unwrap().len(), 4);
    assert_eq!(d["structuredContent"]["plates"][1]["name"], "Pumpkins");
    // slicing plate 1 by number switches to it
    let sl = s.call("slice", json!({"plate": 1}));
    assert_eq!(sl["isError"], false, "{sl}");
    let p = s.call("plates", json!({}));
    assert_eq!(p["structuredContent"]["active"], 1, "{p}");
    // errors say what to do
    let bad = s.call("plate", json!({"action": "select", "plate": 7}));
    assert_eq!(bad["isError"], true);
    assert!(bad["content"][0]["text"].as_str().unwrap().contains("no_such_plate"), "{bad}");
    // events tell a watching agent which plate the user is on
    let ev = s.call("wait_for_event", json!({"since": 0, "kinds": ["plate_changed"], "timeout_s": 1}));
    assert!(ev["content"][0]["text"].as_str().unwrap().contains("Pumpkins"), "{ev}");
}

#[test]
fn an_agent_can_find_models_in_the_workspace_and_lock_them() {
    let mut s = Session::start();
    s.request("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}));
    s.notify("notifications/initialized");
    let folder = s.home.join("designs");
    std::fs::create_dir_all(folder.join("hooks")).unwrap();
    // make a model file to find: export a box as STL into the folder
    s.call("add_object", json!({"id": "hook", "shape": {"type": "box", "size": [30, 12, 6]}}));
    let ex = s.call("export", json!({"path": folder.join("hooks/hook.stl")}));
    assert_eq!(ex["isError"], false, "{ex}");
    s.call("remove_object", json!({"ids": ["hook"]}));

    let w = s.call("workspace", json!({"set_root": folder}));
    assert_eq!(w["isError"], false, "{w}");
    assert_eq!(w["structuredContent"]["entries"][0]["kind"], "folder", "{w}");
    let w = s.call("workspace", json!({"path": "hooks"}));
    let e = &w["structuredContent"]["entries"][0];
    assert_eq!((e["name"].as_str(), e["kind"].as_str()), (Some("hook.stl"), Some("model")), "{w}");
    // the root is remembered for the next call
    let again = s.call("workspace", json!({}));
    assert!(again["content"][0]["text"].as_str().unwrap().contains("designs"), "{again}");

    // bring it in on its own plate and lock it
    s.call("plate", json!({"action": "add", "name": "Hooks"}));
    let im = s.call("import_file", json!({"path": e["path"]}));
    assert_eq!(im["isError"], false, "{im}");
    let id = im["structuredContent"]["id"].as_str().unwrap_or("hook").to_string();
    let lk = s.call("update_object", json!({"id": id, "locked": true}));
    assert_eq!(lk["isError"], false, "{lk}");
    let p = s.call("get_project", json!({}));
    let obj = &p["structuredContent"]["objects"][0];
    assert_eq!((obj["plate"].as_u64(), obj["locked"].as_bool()), (Some(2), Some(true)), "{p}");
}

#[test]
fn an_agent_can_make_a_two_colour_part() {
    let mut s = Session::start();
    s.request("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}));
    s.notify("notifications/initialized");
    s.call("set_filament", json!({"slot": 2, "preset": "Bambu PLA Basic @BBL H2D", "color": "#C0392B"}));
    let r = s.call("add_object", json!({"id": "coaster", "shape": {"type": "cylinder", "d": 90, "h": 4},
        "parts": [{"name": "ring", "shape": {"type": "difference", "children": [{"type": "cylinder", "d": 80, "h": 1}, {"type": "cylinder", "d": 70, "h": 3, "at": [0, 0, -1]}], "at": [0, 0, 4]}, "filament": 2}]}));
    assert_eq!(r["isError"], false, "{r}");
    let d = s.call("describe", json!({}));
    assert_eq!(d["structuredContent"]["objects"][0]["parts"][1]["name"], "ring", "{d}");
    // the native slicer prints both: a nozzle each, changes where the ring is, a prime tower
    let sl = s.call("slice", json!({}));
    assert_eq!(sl["isError"], false, "{sl}");
    let sc = &sl["structuredContent"];
    assert_eq!(sc["filaments"].as_array().map(|a| a.len()), Some(2), "{sl}");
    assert!(sc["filament_changes"].as_u64().unwrap_or(0) >= 1 && sc["tower_g"].as_f64().unwrap_or(0.0) > 0.0, "{sl}");
    assert_eq!(sc["lint"]["ok"], true, "{sl}");
}

#[test]
fn an_agent_can_combine_copy_and_use_spools() {
    let mut s = Session::start();
    s.request("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}));
    s.notify("notifications/initialized");
    s.call("add_object", json!({"id": "base", "shape": {"type": "box", "size": [40, 30, 4]}, "position": [100, 100]}));
    s.call("add_object", json!({"id": "peg", "shape": {"type": "cylinder", "d": 8, "h": 20}, "position": [100, 100]}));
    // merge in place: one object, where the two were
    let c = s.call("combine", json!({"op": "union", "ids": ["base", "peg"]}));
    assert_eq!(c["isError"], false, "{c}");
    assert!(c["content"][0]["text"].as_str().unwrap().contains("at (100.0, 100.0)"), "{c}");
    // copies go next to it and nothing else moves; fill packs the plate
    let d = s.call("duplicate_object", json!({"id": "base", "copies": 2}));
    assert_eq!(d["structuredContent"]["ids"].as_array().map(|a| a.len()), Some(2), "{d}");
    assert!(d["content"][0]["text"].as_str().unwrap().contains("Nothing else moved"), "{d}");
    let f = s.call("duplicate_object", json!({"id": "base", "fill": true, "gap": 10}));
    assert!(f["structuredContent"]["ids"].as_array().map(|a| a.len()).unwrap_or(0) > 10, "{f}");
    // an inventory spool prints an object: its slot is added and linked
    let sp = s.call("spool_add", json!({"material": "PETG", "name": "Pink", "color": "#E58FB0", "location": "ext:left"}));
    let sid = sp["structuredContent"]["id"].as_str().unwrap_or("").to_string();
    assert!(!sid.is_empty(), "{sp}");
    let u = s.call("update_object", json!({"id": "base", "spool": sid}));
    assert_eq!(u["isError"], false, "{u}");
    let p = s.call("get_project", json!({}));
    let fils = p["structuredContent"]["filaments"].as_array().unwrap();
    assert_eq!(fils.len(), 2, "{p}");
    assert_eq!(fils[1]["spool"], sid.as_str());
    let base = p["structuredContent"]["objects"].as_array().unwrap().iter().find(|o| o["id"] == "base").unwrap().clone();
    assert_eq!(base["filament"], 2);
}

#[test]
fn an_agent_can_build_a_functional_part_with_checks_and_fits() {
    let mut s = Session::start();
    s.request("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}));
    s.notify("notifications/initialized");
    // a bracket: rounded plate, two M3 countersunk holes, an engraved label, a hollow boss
    let r = s.call("add_object", json!({"id": "bracket", "shape": {"type": "difference", "children": [
        {"type": "union", "children": [
            {"type": "extrude", "points": [[-30, -15], [30, -15], [30, 15], [-30, 15]], "height": 5, "corner_radius": 4},
            {"type": "shell", "child": {"type": "cylinder", "d": 14, "h": 20}, "wall": 2.4}
        ]},
        {"type": "hole", "screw": "M3", "kind": "countersink", "depth": 6, "at": [-22, 0, 5]},
        {"type": "hole", "screw": "M3", "kind": "countersink", "depth": 6, "at": [22, 0, 5]},
        {"type": "text", "text": "V2", "size": 5, "h": 1, "at": [0, -10, 4.4]}
    ]}}));
    assert_eq!(r["isError"], false, "{r}");
    let c = s.call("check", json!({"add": {"kind": "size", "object": "bracket", "axis": "x", "max": 62, "note": "fits the rail"}}));
    assert!(c["content"][0]["text"].as_str().unwrap().contains("Saved check c1"), "{c}");
    s.call("check", json!({"add": {"kind": "wall", "object": "bracket", "z": 12, "min": 2}}));
    let c = s.call("check", json!({}));
    let ok = c["content"][0]["text"].as_str().unwrap().matches("spec_ok").count();
    assert_eq!(ok, 2, "{c}");
    // checks travel with the project
    let p = s.call("get_project", json!({}));
    assert_eq!(p["structuredContent"]["specs"].as_array().unwrap().len(), 2);
    // fit calibration: a coupon to print, then what was found
    let f = s.call("fit", json!({"action": "coupon"}));
    assert_eq!(f["isError"], false, "{f}");
    let f = s.call("fit", json!({"action": "record", "slip": 0.2, "press": 0.1}));
    assert!(f["content"][0]["text"].as_str().unwrap().contains("0.2 mm extra"), "{f}");
    let f = s.call("fit", json!({"action": "show"}));
    assert!(f["content"][0]["text"].as_str().unwrap().contains("slip +0.2"), "{f}");
    // split a long bar in two
    s.call("add_object", json!({"id": "rail", "shape": {"type": "slot", "length": 200, "width": 20, "h": 8}}));
    let sp = s.call("split_object", json!({"id": "rail", "axis": "x"}));
    assert_eq!(sp["structuredContent"]["ids"].as_array().unwrap().len(), 2, "{sp}");
}

#[test]
fn spools_carry_their_calibration_into_slicing() {
    let mut s = Session::start();
    s.request("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}));
    s.notify("notifications/initialized");
    let a = s.call("spool_add", json!({"material": "PETG", "color": "#3F8E43", "net_weight_g": 1000, "preset": "Bambu PETG Basic @BBL H2D 0.4 nozzle", "location": "ext:right"}));
    assert_eq!(a["isError"], false, "{a}");
    let id = a["structuredContent"]["id"].as_str().unwrap_or("petg-3f8e43-1").to_string();
    // the name is brand + line + colour until it is renamed, and "" makes it automatic again
    let text = |v: &Value| v["content"][0]["text"].as_str().unwrap_or("").to_string();
    assert!(text(&a).contains("Generic PETG Green"), "{a}");
    let u = s.call("spool_update", json!({"id": id, "name": "Lab green"}));
    assert!(text(&u).contains("Lab green"), "{u}");
    let u = s.call("spool_update", json!({"id": id, "brand": "Sunlu", "sub_brand": "PETG HS"}));
    assert!(text(&u).contains("Lab green"), "a typed name stays: {u}");
    let u = s.call("spool_update", json!({"id": id, "name": ""}));
    assert!(text(&u).contains("Sunlu PETG HS Green"), "{u}");
    let u = s.call("spool_update", json!({"id": id, "color": "#111111"}));
    assert!(text(&u).contains("Sunlu PETG HS Black"), "{u}");
    let u = s.call("spool_update", json!({"id": id, "calibration": {"flow_ratio": 0.95, "pressure_advance": 0.03}}));
    assert_eq!(u["isError"], false, "{u}");
    assert!(u["content"][0]["text"].as_str().unwrap().contains("calibrated: flow 0.95, K 0.03"), "{u}");
    // a slot using that spool slices with its flow ratio
    s.call("set_filament", json!({"slot": 1, "preset": "Bambu PETG Basic @BBL H2D 0.4 nozzle", "spool": id}));
    s.call("add_object", json!({"id": "cube", "shape": {"type": "box", "size": [20, 20, 5]}}));
    let sl = s.call("slice", json!({}));
    assert_eq!(sl["isError"], false, "{sl}");
    let g = s.call("export", json!({"path": s.home.join("cube.gcode")}));
    assert_eq!(g["isError"], false, "{g}");
    let gcode = std::fs::read_to_string(s.home.join("cube.gcode")).unwrap();
    assert!(gcode.contains("filament_flow_ratio = 0.95"), "the spool's flow ratio is in the G-code's config");
    // F-33: exporting the same file again replaces its record, and exports aren't listed as prints
    let g2 = s.call("export", json!({"path": s.home.join("cube.gcode")}));
    assert_eq!(g2["isError"], false, "{g2}");
    assert_ne!(g["structuredContent"]["print_id"], g2["structuredContent"]["print_id"], "{g2}");
    let h = s.call("print_history", json!({}));
    let ht = text(&h);
    assert!(!ht.contains("EXPORTED") && ht.contains("1 exported file(s) not printed yet"), "{h}");
    let old = s.call("print_detail", json!({"id": g["structuredContent"]["print_id"]}));
    assert_eq!(old["isError"], true, "the first export's record is gone: {old}");
}

#[test]
fn an_agent_can_repair_a_downloaded_mesh() {
    let mut s = Session::start();
    s.request("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}));
    s.notify("notifications/initialized");
    // a 10 mm cube with its top missing, as downloads sometimes are
    let v = |x: f32, y: f32, z: f32| format!("vertex {x} {y} {z}");
    let tri = |a: (f32, f32, f32), b: (f32, f32, f32), c: (f32, f32, f32)| format!("facet normal 0 0 0\nouter loop\n{}\n{}\n{}\nendloop\nendfacet\n", v(a.0, a.1, a.2), v(b.0, b.1, b.2), v(c.0, c.1, c.2));
    let (o, x, y, xy) = ((0.0, 0.0), (10.0, 0.0), (0.0, 10.0), (10.0, 10.0));
    let p = |q: (f32, f32), z: f32| (q.0, q.1, z);
    let mut stl = String::from("solid open\n");
    for (a, b, c) in [
        (p(o, 0.0), p(xy, 0.0), p(x, 0.0)), (p(o, 0.0), p(y, 0.0), p(xy, 0.0)),
        (p(o, 0.0), p(x, 0.0), p(x, 10.0)), (p(o, 0.0), p(x, 10.0), p(o, 10.0)),
        (p(x, 0.0), p(xy, 0.0), p(xy, 10.0)), (p(x, 0.0), p(xy, 10.0), p(x, 10.0)),
        (p(xy, 0.0), p(y, 0.0), p(y, 10.0)), (p(xy, 0.0), p(y, 10.0), p(xy, 10.0)),
        (p(y, 0.0), p(o, 0.0), p(o, 10.0)), (p(y, 0.0), p(o, 10.0), p(y, 10.0)),
    ] {
        stl.push_str(&tri(a, b, c));
    }
    stl.push_str("endsolid open\n");
    let file = s.home.join("open_box.stl");
    std::fs::write(&file, stl).unwrap();
    let a = s.call("import_file", json!({"path": file, "id": "raw"}));
    assert!(a["content"][0]["text"].as_str().unwrap().contains("NOT watertight"), "{a}");
    let b = s.call("import_file", json!({"path": file, "id": "fixed", "repair": true}));
    let t = b["content"][0]["text"].as_str().unwrap();
    assert!(t.contains("watertight") && !t.contains("NOT watertight"), "{b}");
}
