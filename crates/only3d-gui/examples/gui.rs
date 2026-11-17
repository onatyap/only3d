//! Open the only3d window.
//!
//! ```text
//! cargo run -p only3d-gui --release --example gui -- [project.only3d.json] [--demo] [--live] [--screenshot out.png]
//!                                                  [--select id] [--triangles N]
//! ```
//!
//! `--demo` builds a small parametric plate through the tool registry (some calls attributed to
//! an agent so the activity feed shows both) and slices it. `--triangles N` adds an imported-style
//! mesh object with about N triangles for viewport performance checks. `--agent-demo` (implies
//! `--live`) connects to the window's live socket from another thread after a moment and edits the
//! session as an agent would, to exercise the live path end to end.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use only3d_gui::{run, GuiOptions};
use only3d_studio::{Actor, Studio};
use serde_json::json;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut project: Option<PathBuf> = None;
    let mut opts = GuiOptions::default();
    let mut demo = false;
    let mut select: Option<String> = None;
    let mut triangles: Option<usize> = None;
    let mut agent_demo = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--demo" => demo = true,
            "--live" => opts.start_live_server = true,
            "--agent-demo" => {
                agent_demo = true;
                opts.start_live_server = true;
            }
            "--screenshot" => opts.screenshot = args.next().map(PathBuf::from),
            "--select" => select = args.next(),
            "--triangles" => triangles = args.next().and_then(|n| n.parse().ok()),
            "--title" => opts.project_title = args.next(),
            p => project = Some(PathBuf::from(p)),
        }
    }
    let mut studio = match &project {
        Some(p) => Studio::open(p).map_err(|e| anyhow::anyhow!("{e}"))?,
        None => Studio::new(),
    };
    if demo {
        build_demo(&mut studio)?;
    }
    if let Some(n) = triangles {
        add_dense_mesh(&mut studio, n)?;
    }
    if let Some(id) = select {
        studio.call("select", json!({ "ids": [id] }), Actor::Agent).map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    if agent_demo {
        std::thread::spawn(agent_session);
    }
    run(Arc::new(Mutex::new(studio)), opts)
}

/// Pretend to be an agent on the live socket: read the user's selection, then edit.
fn agent_session() {
    let path = only3d_studio::live::socket_path();
    let mut client = None;
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Some(c) = only3d_studio::live::LiveClient::connect(&path) {
            client = Some(c);
            break;
        }
    }
    let Some(mut c) = client else {
        eprintln!("agent-demo: no live session at {}", path.display());
        return;
    };
    std::thread::sleep(std::time::Duration::from_millis(400));
    let sel = c.call("get_selection", json!({}), Actor::Agent);
    eprintln!("agent-demo: get_selection → {}", sel.as_ref().map(|o| o.text.lines().next().unwrap_or("").to_string()).unwrap_or_else(|e| e.to_string()));
    let r = c.call(
        "add_object",
        json!({ "id": "hook", "name": "wall hook", "position": [80, 230], "shape": { "type": "union", "children": [
            { "type": "box", "size": [16, 6, 40], "radius": 1.5 },
            { "type": "box", "size": [16, 22, 6], "radius": 1.5, "at": [0, 8, 0] }
        ] } }),
        Actor::Agent,
    );
    eprintln!("agent-demo: add_object → {}", r.map(|o| o.text).unwrap_or_else(|e| e.to_string()));
    let _ = c.call("select", json!({ "ids": ["hook"] }), Actor::Agent);
}

fn call(s: &mut Studio, tool: &str, args: serde_json::Value, actor: Actor) -> anyhow::Result<()> {
    s.call(tool, args, actor).map(|_| ()).map_err(|e| anyhow::anyhow!("{tool}: {e}"))
}

fn build_demo(s: &mut Studio) -> anyhow::Result<()> {
    call(s, "new_project", json!({ "name": "desk-set" }), Actor::User)?;
    call(s, "set_params", json!({ "params": { "wall": 4, "hole_d": 5.4 } }), Actor::User)?;
    call(s, "set_filament", json!({ "slot": 1, "color": "#C9B79C" }), Actor::User)?;
    call(
        s,
        "add_object",
        json!({ "id": "bracket", "position": [120, 170], "shape": { "type": "difference", "children": [
            { "type": "box", "size": [70, 34, "wall*2"], "radius": 2.5 },
            { "type": "array", "count": [2, 1, 1], "spacing": [48, 0, 0], "at": [-24, 0, -1],
              "child": { "type": "cylinder", "d": "hole_d", "h": 30, "chamfer_top": 0.6 } },
            { "type": "cylinder", "d": 14, "h": 30, "at": [0, 0, 4] }
        ] } }),
        Actor::Agent,
    )?;
    call(
        s,
        "add_object",
        json!({ "id": "knob", "position": [210, 175], "shape": { "type": "difference", "children": [
            { "type": "cylinder", "d": 36, "h": 18, "fillet_top": 3, "chamfer_bottom": 0.6 },
            { "type": "polar", "count": 12, "child": { "type": "cylinder", "d": 5, "h": 40, "at": [19, 0, -1] } },
            { "type": "cylinder", "d": 6.2, "h": 14, "at": [0, 0, -1] }
        ] } }),
        Actor::Agent,
    )?;
    call(
        s,
        "add_object",
        json!({ "id": "cup", "position": [165, 110], "shape": { "type": "difference", "children": [
            { "type": "cylinder", "d": 46, "h": 40, "fillet_top": 1.2 },
            { "type": "cylinder", "d": "46 - wall*2", "h": 50, "at": [0, 0, 2.4], "fillet_bottom": 4 }
        ] } }),
        Actor::User,
    )?;
    call(
        s,
        "add_object",
        json!({ "id": "tray", "position": [245, 105], "shape": { "type": "difference", "children": [
            { "type": "box", "size": [56, 40, 12], "radius": 3 },
            { "type": "box", "size": [48, 32, 20], "radius": 2.5, "at": [0, 0, 2] }
        ] } }),
        Actor::User,
    )?;
    call(s, "set_process", json!({ "overrides": { "wall_loops": 3, "infill_density": 0.2, "brim_width": 3 } }), Actor::Agent)?;
    call(s, "slice", json!({}), Actor::Agent)?;
    Ok(())
}

/// A dense UV sphere-ish blob with about `n` triangles, added as a plain mesh file import.
fn add_dense_mesh(s: &mut Studio, n: usize) -> anyhow::Result<()> {
    let seg = ((n as f64 / 2.0).sqrt() as usize).max(8);
    let (rings, sectors) = (seg, seg);
    let mut tris: Vec<[glam::Vec3; 3]> = Vec::with_capacity(rings * sectors * 2);
    let p = |i: usize, j: usize| {
        let th = std::f32::consts::PI * i as f32 / rings as f32;
        let ph = std::f32::consts::TAU * j as f32 / sectors as f32;
        let r = 30.0 + 2.5 * (6.0 * ph).sin() * th.sin() + 1.5 * (9.0 * th).cos();
        glam::Vec3::new(r * th.sin() * ph.cos(), r * th.sin() * ph.sin(), r * th.cos())
    };
    for i in 0..rings {
        for j in 0..sectors {
            let (a, b, c, d) = (p(i, j), p(i + 1, j), p(i + 1, j + 1), p(i, j + 1));
            if i != 0 {
                tris.push([a, b, d]);
            }
            if i != rings - 1 {
                tris.push([b, c, d]);
            }
        }
    }
    let mesh = only3d_geom::Mesh::from_soup(&tris);
    let dir = std::env::temp_dir().join("only3d-gui-demo");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("dense_{n}.stl"));
    only3d_geom::io::save(&path, &[only3d_geom::io::ImportedObject::new("dense", mesh)]).map_err(|e| anyhow::anyhow!("{e}"))?;
    call(s, "import_file", json!({ "path": path.display().to_string(), "id": "dense", "position": [80, 80] }), Actor::User)
}
