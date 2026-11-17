//! `only3d` — one binary for people, scripts and agents.

mod mcp;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use only3d_studio::live::{self, LiveClient};
use only3d_studio::{Actor, Studio, ToolOutput};
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "only3d", version, about = "Fast, structured, agent-first 3D modelling, slicing and printing for Bambu Lab printers")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the desktop app (optionally with a project or model file). Agents attach live.
    Gui {
        input: Option<PathBuf>,
        /// Don't open the live agent socket.
        #[arg(long)]
        no_live: bool,
        /// Self-test: save a PNG of the window after a few frames, then exit.
        #[arg(long)]
        screenshot: Option<PathBuf>,
    },
    /// Run the Model Context Protocol server on stdio (for Claude and other agents).
    Mcp {
        /// Project file to open (and autosave to with --autosave).
        #[arg(long)]
        project: Option<PathBuf>,
        /// Save the project after every change.
        #[arg(long)]
        autosave: bool,
        /// Never attach to a running only3d window.
        #[arg(long)]
        no_live: bool,
        #[command(subcommand)]
        action: Option<McpAction>,
    },
    /// List every tool with its JSON Schema (the complete API).
    Tools {
        /// Only names and one-line descriptions.
        #[arg(long)]
        brief: bool,
    },
    /// Call one tool: `only3d call add_object '{"shape":{"type":"box","size":[20,20,10]}}' -p part.only3d.json`
    Call {
        tool: String,
        /// JSON arguments (default {}). Use @file.json to read from a file, - for stdin.
        args: Option<String>,
        /// Project file to load and (if changed) save back.
        #[arg(short, long)]
        project: Option<PathBuf>,
        /// Print structured JSON instead of text.
        #[arg(long)]
        json: bool,
        /// Where to write returned images (default: current directory).
        #[arg(long)]
        image_dir: Option<PathBuf>,
        /// Use the live window session if one is open (default: yes unless --project is given).
        #[arg(long)]
        live: bool,
    },
    /// Slice model files (STL/3MF/OBJ/SCAD) or a project straight to a print file.
    Slice {
        inputs: Vec<PathBuf>,
        /// Output (.gcode.3mf or .gcode). Default: <first input>.gcode.3mf
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Process overrides, e.g. --set infill_density=0.25 --set wall_loops=3
        #[arg(long = "set", value_name = "KEY=VALUE")]
        set: Vec<String>,
        /// Filament preset, e.g. "Bambu PETG Basic @BBL H2D 0.4 nozzle"
        #[arg(long)]
        filament: Option<String>,
        /// Auto-orient each part first.
        #[arg(long)]
        orient: bool,
        #[arg(long)]
        json: bool,
    },
    /// Render a project or model file to PNG.
    Render {
        input: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// iso, front, back, left, right, top, bottom
        #[arg(long, default_value = "iso")]
        view: String,
        #[arg(long, default_value_t = 1000)]
        width: u32,
        #[arg(long, default_value_t = 750)]
        height: u32,
        /// Show sliced toolpaths.
        #[arg(long)]
        toolpaths: bool,
    },
    /// Mesh report for a model file (size, volume, watertightness, overhangs).
    Info { input: PathBuf },
    /// Host a shared live session without a window (agents and CLI attach to it).
    Serve { project: Option<PathBuf> },
}

fn read_args(a: Option<String>) -> Result<Value> {
    let Some(a) = a else { return Ok(json!({})) };
    let text = if a == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
        s
    } else if let Some(f) = a.strip_prefix('@') {
        std::fs::read_to_string(f).with_context(|| format!("reading {f}"))?
    } else {
        a
    };
    serde_json::from_str(&text).context("arguments must be JSON")
}

fn print_output(o: &ToolOutput, as_json: bool, image_dir: &Path, tool: &str) -> Result<()> {
    let mut paths = Vec::new();
    for (i, im) in o.images.iter().enumerate() {
        std::fs::create_dir_all(image_dir)?;
        let ext = if im.mime == "image/jpeg" { "jpg" } else { "png" };
        let p = image_dir.join(if o.images.len() == 1 { format!("{tool}.{ext}") } else { format!("{tool}_{i}.{ext}") });
        std::fs::write(&p, &im.png)?;
        paths.push(p);
    }
    if as_json {
        println!("{}", serde_json::to_string_pretty(&json!({"ok": true, "text": o.text, "data": o.data, "images": paths}))?);
    } else {
        println!("{}", o.text);
        for p in paths {
            println!("image: {}", p.display());
        }
    }
    Ok(())
}

fn studio_for_files(inputs: &[PathBuf]) -> Result<Studio> {
    if inputs.len() == 1 && inputs[0].to_string_lossy().ends_with(".only3d.json") {
        return Studio::open(&inputs[0]).map_err(|e| anyhow::anyhow!("{e}"));
    }
    let mut s = Studio::new();
    for p in inputs {
        let abs = std::fs::canonicalize(p).with_context(|| format!("{} not found", p.display()))?;
        s.call("import_file", json!({"path": abs}), Actor::Script).map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    s.call("arrange", json!({}), Actor::Script).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(s)
}

#[derive(clap::Subcommand)]
enum McpAction {
    /// Register only3d with agent apps: claude-code, claude-desktop, codex, gemini, antigravity,
    /// opencode, kimi, cursor, or `skill` for agents that use a shell instead of MCP. With no
    /// app, lists them (and whether only3d is there) and prints the entry for any other client.
    Install {
        /// Apps to add only3d to (config files are merged; the old file is kept as .bak).
        apps: Vec<String>,
        /// Same as `claude-code`.
        #[arg(long)]
        claude_code: bool,
        /// Same as `claude-desktop`.
        #[arg(long)]
        claude_desktop: bool,
        /// Only print the server entry (or each named app's), change nothing.
        #[arg(long)]
        print: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cmd = cli.cmd.unwrap_or(Cmd::Gui { input: None, no_live: false, screenshot: None });
    match cmd {
        Cmd::Gui { input, no_live, screenshot } => {
            #[cfg(feature = "gui")]
            {
                let title = input.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string());
                let s = match &input {
                    Some(p) => studio_for_files(std::slice::from_ref(p))?,
                    None => Studio::new(),
                };
                let shared = std::sync::Arc::new(std::sync::Mutex::new(s));
                if screenshot.is_none() {
                    only3d_studio::spools::spawn_tracker(shared.clone(), std::time::Duration::from_secs(120));
                }
                only3d_gui::run(shared, only3d_gui::GuiOptions { project_title: title, screenshot, start_live_server: !no_live })
            }
            #[cfg(not(feature = "gui"))]
            {
                let _ = (input, no_live, screenshot);
                bail!("this build has no GUI (rebuild with --features gui)")
            }
        }
        Cmd::Mcp { action: Some(McpAction::Install { mut apps, claude_code, claude_desktop, print }), .. } => {
            if claude_code {
                apps.push("claude-code".into());
            }
            if claude_desktop {
                apps.push("claude-desktop".into());
            }
            mcp::install(&apps, print)
        }
        Cmd::Mcp { project, autosave, no_live, action: None } => mcp::Server::new(project, autosave, !no_live).run(),
        Cmd::Tools { brief } => {
            let m = Studio::new().registry().manifest();
            // write, don't println!: a reader that stops early (`| head`) just ends the listing
            use std::io::Write;
            let mut out = std::io::stdout().lock();
            if brief {
                for t in m.as_array().unwrap() {
                    if writeln!(out, "{:<18} {}", t["name"].as_str().unwrap_or(""), t["description"].as_str().unwrap_or("").split(". ").next().unwrap_or("")).is_err() {
                        break;
                    }
                }
            } else {
                let _ = writeln!(out, "{}", serde_json::to_string_pretty(&m)?);
            }
            Ok(())
        }
        Cmd::Call { tool, args, project, json: as_json, image_dir, live: use_live } => {
            let args = read_args(args)?;
            let image_dir = image_dir.unwrap_or_else(|| PathBuf::from("."));
            let try_live = use_live || project.is_none();
            if try_live {
                if let Some(mut c) = LiveClient::connect(&live::socket_path()) {
                    let r = c.call(&tool, args, Actor::Agent);
                    return match r {
                        Ok(o) => print_output(&o, as_json, &image_dir, &tool),
                        Err(e) => {
                            eprintln!("{}", serde_json::to_string_pretty(&json!({"ok": false, "error": e}))?);
                            std::process::exit(2)
                        }
                    };
                }
            }
            let mut s = match &project {
                Some(p) if p.exists() => Studio::open(p).map_err(|e| anyhow::anyhow!("{e}"))?,
                Some(p) => {
                    let mut s = Studio::new();
                    s.project_path = Some(p.clone());
                    s
                }
                None => Studio::new(),
            };
            let before = s.revision;
            match s.call(&tool, args, Actor::Agent) {
                Ok(o) => {
                    if let Some(p) = &project {
                        if s.revision != before || !p.exists() {
                            s.save(Some(p)).map_err(|e| anyhow::anyhow!("{e}"))?;
                        }
                    }
                    print_output(&o, as_json, &image_dir, &tool)
                }
                Err(e) => {
                    eprintln!("{}", serde_json::to_string_pretty(&json!({"ok": false, "error": e}))?);
                    std::process::exit(2)
                }
            }
        }
        Cmd::Slice { inputs, output, set, filament, orient, json: as_json } => {
            if inputs.is_empty() {
                bail!("give at least one model or project file");
            }
            let mut s = studio_for_files(&inputs)?;
            if orient {
                s.call("orient", json!({}), Actor::Script).map_err(|e| anyhow::anyhow!("{e}"))?;
                s.call("arrange", json!({}), Actor::Script).map_err(|e| anyhow::anyhow!("{e}"))?;
            }
            if let Some(f) = filament {
                s.call("set_filament", json!({"slot": 1, "preset": f}), Actor::Script).map_err(|e| anyhow::anyhow!("{e}"))?;
            }
            let mut overrides = serde_json::Map::new();
            for kv in set {
                let (k, v) = kv.split_once('=').with_context(|| format!("--set {kv}: expected KEY=VALUE"))?;
                let val = serde_json::from_str(v).unwrap_or(Value::String(v.to_string()));
                overrides.insert(k.to_string(), val);
            }
            if !overrides.is_empty() {
                s.call("set_process", json!({"overrides": overrides}), Actor::Script).map_err(|e| anyhow::anyhow!("{e}"))?;
            }
            let out = output.unwrap_or_else(|| {
                let p = &inputs[0];
                let stem = p.file_name().and_then(|n| n.to_str()).unwrap_or("plate").split('.').next().unwrap_or("plate").to_string();
                p.with_file_name(format!("{stem}.gcode.3mf"))
            });
            let o = s.call("export", json!({"path": out}), Actor::Script).map_err(|e| anyhow::anyhow!("{e}"))?;
            if as_json {
                let sl = s.last_slice.as_ref().map(|x| serde_json::to_value(&x.result.stats).unwrap_or(Value::Null));
                println!("{}", serde_json::to_string_pretty(&json!({"output": out, "stats": sl, "warnings": s.last_slice.as_ref().map(|x| x.result.warnings.clone())}))?);
            } else {
                println!("{}", o.text);
                if let Some(x) = &s.last_slice {
                    for w in &x.result.warnings {
                        println!("warning: {w}");
                    }
                }
            }
            Ok(())
        }
        Cmd::Render { input, output, view, width, height, toolpaths } => {
            let mut s = studio_for_files(std::slice::from_ref(&input))?;
            if toolpaths {
                s.call("slice", json!({}), Actor::Script).map_err(|e| anyhow::anyhow!("{e}"))?;
            }
            let o = s.call("render", json!({"view": view, "width": width, "height": height, "toolpaths": toolpaths}), Actor::Script).map_err(|e| anyhow::anyhow!("{e}"))?;
            let out = output.unwrap_or_else(|| input.with_extension("png"));
            std::fs::write(&out, &o.images[0].png)?;
            println!("wrote {}", out.display());
            Ok(())
        }
        Cmd::Info { input } => {
            let objs = only3d_geom::io::load(&input).map_err(|e| anyhow::anyhow!("{e}"))?;
            for o in objs {
                let r = o.mesh.analyze();
                println!(
                    "{}: {:.2} x {:.2} x {:.2} mm, {:.2} cm³, {} triangles, {}{}, overhang>45° {:.0} mm², {} bodies",
                    o.name,
                    r.size.x,
                    r.size.y,
                    r.size.z,
                    r.volume_mm3 / 1000.0,
                    r.triangles,
                    if r.watertight { "watertight" } else { "NOT watertight" },
                    if r.inverted { " (inverted)" } else { "" },
                    o.mesh.overhang_area(45.0),
                    r.components
                );
            }
            Ok(())
        }
        Cmd::Serve { project } => {
            let s = match project {
                Some(p) => Studio::open(&p).map_err(|e| anyhow::anyhow!("{e}"))?,
                None => Studio::new(),
            };
            let shared = std::sync::Arc::new(std::sync::Mutex::new(s));
            let path = live::socket_path();
            live::serve(shared.clone(), &path)?;
            only3d_studio::spools::spawn_tracker(shared, std::time::Duration::from_secs(120));
            eprintln!("only3d live session at {} (Ctrl-C to stop)", path.display());
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            }
        }
    }
}
