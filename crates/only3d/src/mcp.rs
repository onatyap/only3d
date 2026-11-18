//! Model Context Protocol server over stdio (JSON-RPC 2.0, one message per line).
//!
//! Every only3d tool becomes an MCP tool with its JSON Schema. If an only3d window is
//! open, calls are forwarded to it over the live socket so the user watches the agent work;
//! otherwise a headless session runs in-process.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use base64::Engine;
use only3d_studio::live::{self, LiveClient};
use only3d_studio::{Actor, Studio, ToolError, ToolOutput};
use serde_json::{json, Value};

pub const INSTRUCTIONS: &str = "only3d is a parametric 3D modelling + slicing + printing studio for Bambu Lab printers (default: H2D, 0.4 mm nozzles, 350x320 mm plate). \
Workflow: 1) `describe` to see the session (objects, params, what the user selected, checks). 2) Model with `add_object`/`update_object`: \
shapes are JSON trees (box/cylinder/sphere/torus/slot/tube/polygon/wedge/text/extrude/revolve/import, shell, hole (screws M2–M8: clearance/countersink/counterbore/insert/nut/teardrop) + union/difference/intersection/hull + array/polar), millimetres, Z up; \
primitives are centred on X/Y and sit on Z=0; numbers can be expressions over project params (`set_params`). Prefer params for anything the user may tweak. `combine` assembles or booleans objects in place; `update_object push_pull` pulls or pushes a flat face. \
3) Verify before claiming success: `check` (printability; `add` saves design checks — size, clearance, wall, overhang — that rerun on every change), `section`, `render` (look at it). Fits: `fit` calibrates hole clearances from a printed coupon. \
4) `set_filament`/`set_process` then `slice` for time/grams; `export` writes .stl/.3mf/.gcode.3mf. Several filaments slice natively (a nozzle each on the H2D, prime tower; `print` maps each to its tray); supports are native too, tree or normal (`support_type`). \
Settings: `settings_catalog` lists every setting with units/ranges; presets (yours from Bambu Studio included) are data: `preset_list`/`preset_get`/`preset_create`/`preset_update`/`preset_import`/`preset_export`. \
Filament: `spool_list`, `spool_sync` (AMS), `print_detail` traces any print to its project version and spools. \
Files: `workspace` lists the user's workspace folder (projects, models, print files); then `open_project` or `import_file` (STL/OBJ/3MF/STEP; .scad files take `set_params {object}` for their variables). \
Plates: a project has one or more build plates (`plates`, `plate` add/select/rename/duplicate/remove/move). Slice, check, print, arrange and render act on the active plate; naming an object switches to its plate; `slice` takes a `plate` number. \
Edits are undoable (`undo`). Errors carry a code and a hint: follow the hint. \
To react instead of polling, call `wait_for_event` (user edits/selection, slices, approvals, printer state, stages, alerts, layers, finish/fail); pass its `next_since` back in. \
Printer actions that start, stop or destroy something (print with confirm, stop, skip objects, delete files, load/unload filament, heat changes mid-print) are approved by the user: only3d asks them in its window (or through your app) and the call waits for their answer. \
Request a print only when the user wants one; on `denied_by_user` do not retry, ask them what they want instead.";

/// Live state an agent (or its user, e.g. via @-mentions in Claude Code) can attach as context:
/// (uri, name, mime type, tool that produces it, description).
const RESOURCES: &[(&str, &str, &str, &str, &str)] = &[
    ("only3d://project", "Project", "application/json", "describe", "The open project: objects with sizes and checks, params, printer, process and filaments, last slice."),
    ("only3d://selection", "Selection", "application/json", "get_selection", "What the user has selected in the only3d window, with full object definitions."),
    ("only3d://printer/status", "Printer status", "application/json", "printer_status", "Live printer state: job, progress, stage, temperatures, AMS trays, alerts."),
    ("only3d://printer/camera", "Printer camera", "image/jpeg", "printer_camera", "A fresh photo from the printer's chamber camera."),
    ("only3d://prints", "Print history", "application/json", "print_history", "Recent prints with state, grams, spools and project versions."),
    ("only3d://events", "Recent events", "application/json", "wait_for_event", "The latest 50 session events: edits, selection, slices, approvals, printer state, alerts, progress."),
];

/// Ready-made workflows for the main journeys (Claude Code shows them as slash commands,
/// `/mcp__only3d__<name>`). `{arg}` placeholders are filled from the prompt arguments.
struct Prompt {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    args: &'static [(&'static str, &'static str, bool)],
    text: &'static str,
}

const PROMPTS: &[Prompt] = &[
    Prompt {
        name: "design_part",
        title: "Design a part",
        description: "Model a printable part from a description, verify it and report time and grams.",
        args: &[("what", "What to make, with any sizes you know", true), ("material", "Filament, e.g. PETG (optional)", false)],
        text: "Design this part in only3d: {what}. Material: {material}.\n\
1. `describe` the session first; build on what is there if it fits.\n\
2. Put every size the user may want to change into project params (`set_params`) and use them in the shape tree.\n\
3. Model with `add_object` (primitives, booleans, fillets/chamfers). Think about the print: flat face down, no needless overhangs, walls at least 2 line widths.\n\
4. Verify before claiming success: `check`, `section` through critical heights (wall thickness, holes), `render` and look at the image.\n\
5. Pick the filament (`set_filament`) if a material was given, `slice`, and report time and grams.\n\
6. Summarise what you built, its params, and anything the user should decide. Do not print unless asked.",
    },
    Prompt {
        name: "print_plate",
        title: "Print the plate",
        description: "Preflight, ask the user to approve, print, then follow the job to the end.",
        args: &[],
        text: "Print the current plate on the user's printer with only3d.\n\
1. `print` without confirm: read the checklist and look at the plate photo. Fix what fails (slice, nozzle, filament, plate) or explain it.\n\
2. `print` with confirm: true. The user approves or denies it in only3d; on denied_by_user stop and ask what to change.\n\
3. Follow the job with `wait_for_event` (kinds: printer_stage, printer_alert, print_progress, print_finished, print_failed), passing next_since each time. Report stages briefly.\n\
4. On a printer_alert: `printer_alert` list, explain it, take a `printer_camera` photo, and propose the action Bambu offers; the user approves it.\n\
5. When finished: `spool_sync`, then `print_detail` for the record and grams used.",
    },
    Prompt {
        name: "diagnose_print",
        title: "Diagnose a print",
        description: "Work out what went wrong with a print (or what the printer is complaining about) and what to do.",
        args: &[("print_id", "A print id like P-0010 (optional: the latest)", false)],
        text: "Diagnose print {print_id} (if not given: the most recent) with only3d.\n\
1. `print_history` / `print_detail` for the job, its project version, settings and grams.\n\
2. `printer_status` and `printer_alert` for the printer's state and any alerts with Bambu's text and offered actions.\n\
3. `printer_camera` and look at the photo; `wait_for_event` with since: 0 and kinds printer_* / print_* for what happened.\n\
4. Explain the most likely cause in plain words and propose concrete fixes (settings, model, filament, plate). Change nothing without the user's go.",
    },
    Prompt {
        name: "watch_print",
        title: "Watch the running print",
        description: "Keep an eye on the running job and speak up when something needs attention.",
        args: &[],
        text: "Watch the running print with only3d until it finishes.\n\
Loop on `wait_for_event` (kinds: printer_stage, printer_alert, print_progress, print_finished, print_failed, printer_connection), passing next_since each time; report every 25% and every stage change in one line.\n\
At layer 3 and at the end take a `printer_camera` photo and check it (first layer stuck? spaghetti?).\n\
On an alert: `printer_alert` list, explain it, and propose Bambu's offered action; the user approves it in only3d.",
    },
];

enum Backend {
    Live(LiveClient),
    Local(Box<Studio>),
}

pub struct Server {
    backend: Option<Backend>,
    project: Option<PathBuf>,
    autosave: bool,
    allow_live: bool,
    /// The client's name from `initialize` (shown in approval requests).
    client: String,
    /// Whether the client can ask its user questions (MCP elicitation).
    can_elicit: bool,
    /// Incoming messages (read on a thread, so a tool call can wait for an elicitation reply).
    rx: Option<std::sync::mpsc::Receiver<String>>,
    backlog: std::collections::VecDeque<String>,
    next_out_id: u64,
}

impl Server {
    pub fn new(project: Option<PathBuf>, autosave: bool, allow_live: bool) -> Self {
        Server { backend: None, project, autosave, allow_live, client: "an agent".into(), can_elicit: false, rx: None, backlog: Default::default(), next_out_id: 1 }
    }

    fn backend(&mut self) -> &mut Backend {
        // re-check for a live window on every call while running locally with an empty session
        let want_live = self.allow_live
            && match &self.backend {
                None => true,
                Some(Backend::Local(s)) => s.project.objects.is_empty() && s.journal.is_empty(),
                Some(Backend::Live(_)) => false,
            };
        if want_live {
            if let Some(mut c) = LiveClient::connect(&live::socket_path()) {
                c.client = Some(self.client.clone());
                self.backend = Some(Backend::Live(c));
            }
        }
        if self.backend.is_none() {
            let mut s = match &self.project {
                Some(p) if p.exists() => Studio::open(p).unwrap_or_default(),
                _ => Studio::new(),
            };
            if let Some(p) = &self.project {
                s.project_path = Some(p.clone());
            }
            self.backend = Some(Backend::Local(Box::new(s)));
        }
        self.backend.as_mut().unwrap()
    }

    fn manifest(&mut self) -> Value {
        match self.backend() {
            Backend::Live(c) => match c.call("__manifest", json!({}), Actor::Agent) {
                Ok(o) => o.data,
                Err(_) => Studio::new().registry().manifest(),
            },
            Backend::Local(s) => s.registry().manifest(),
        }
    }

    fn call(&mut self, name: &str, args: Value) -> Result<ToolOutput, ToolError> {
        let autosave = self.autosave;
        match self.backend() {
            Backend::Live(c) => {
                let r = c.call(name, args.clone(), Actor::Agent);
                if let Err(e) = &r {
                    if e.code == "live_disconnected" && only3d_studio::approval::gate(name, &args).is_some() {
                        // the window closed while the user was being asked: don't quietly retry
                        self.backend = None;
                        return Err(ToolError::new("approval_interrupted", "the only3d window closed before the user answered the approval request", "nothing was done; ask the user to reopen only3d if they still want it"));
                    }
                    if e.code == "live_disconnected" {
                        // window closed: fall back to a local session and retry once
                        self.backend = None;
                        self.allow_live = false;
                        return self.call(name, args);
                    }
                }
                r
            }
            Backend::Local(_) if only3d_studio::approval::gate(name, &args).is_some() => self.call_gated_local(name, args),
            Backend::Local(s) => {
                let r = s.call(name, args, Actor::Agent);
                if autosave && r.is_ok() && s.project_path.is_some() {
                    let mutating = s.registry().get(name).map(|t| t.mutates).unwrap_or(false);
                    if mutating {
                        let _ = s.save(None);
                    }
                }
                r
            }
        }
    }

    fn handle(&mut self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let result: Result<Value, (i64, String)> = match method {
            "initialize" => {
                if let Some(n) = params.pointer("/clientInfo/name").and_then(|v| v.as_str()) {
                    self.client = n.to_string();
                }
                self.can_elicit = params.pointer("/capabilities/elicitation").is_some();
                let requested = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("2025-06-18");
                let supported = ["2024-11-05", "2025-03-26", "2025-06-18"];
                let version = if supported.contains(&requested) { requested } else { "2025-06-18" };
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}, "resources": {"subscribe": false, "listChanged": false}, "prompts": {"listChanged": false}},
                    "serverInfo": {"name": "only3d", "title": "only3d", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => {
                let m = self.manifest();
                let tools: Vec<Value> = m
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|t| {
                        let mutates = t.get("mutates").and_then(|x| x.as_bool()).unwrap_or(false);
                        let side = t.get("side_effects").and_then(|x| x.as_bool()).unwrap_or(false);
                        json!({
                            "name": t["name"], "title": t["title"], "description": t["description"], "inputSchema": t["inputSchema"],
                            "annotations": annotations(mutates, side)
                        })
                    })
                    .collect();
                Ok(json!({"tools": tools}))
            }
            "tools/call" => {
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                Ok(match self.call(&name, args) {
                    Ok(o) => tool_result(&name, o),
                    Err(e) => json!({
                        "content": [{"type": "text", "text": format!("Error {}: {}\nHint: {}", e.code, e.message, e.hint)}],
                        "structuredContent": {"error": e},
                        "isError": true
                    }),
                })
            }
            "resources/list" => Ok(json!({"resources": RESOURCES.iter().map(|(uri, name, mime, _, desc)| json!({"uri": uri, "name": name, "mimeType": mime, "description": desc})).collect::<Vec<_>>()})),
            "resources/templates/list" => Ok(json!({"resourceTemplates": []})),
            "resources/read" => {
                let uri = params.get("uri").and_then(|u| u.as_str()).unwrap_or("").to_string();
                match RESOURCES.iter().find(|r| r.0 == uri) {
                    None => Err((-32002, format!("unknown resource {uri}; see resources/list"))),
                    Some((_, _, mime, tool, _)) => {
                        let args = if *tool == "wait_for_event" { json!({"since": 0, "timeout_s": 0}) } else { json!({}) };
                        match self.call(tool, args) {
                            Err(e) => Err((-32603, format!("{}: {} ({})", e.code, e.message, e.hint))),
                            Ok(o) => {
                                if *mime == "image/jpeg" {
                                    let im = o.images.first().map(|i| base64::engine::general_purpose::STANDARD.encode(&i.png)).unwrap_or_default();
                                    Ok(json!({"contents": [{"uri": uri, "mimeType": mime, "blob": im}]}))
                                } else {
                                    let mut data = o.data;
                                    // recent events only: the newest 50
                                    if let Some(ev) = data.get_mut("events").and_then(|e| e.as_array_mut()) {
                                        let n = ev.len();
                                        ev.drain(..n.saturating_sub(50));
                                    }
                                    let body = json!({"summary": o.text, "data": data});
                                    Ok(json!({"contents": [{"uri": uri, "mimeType": mime, "text": serde_json::to_string_pretty(&body).unwrap_or_default()}]}))
                                }
                            }
                        }
                    }
                }
            }
            "prompts/list" => Ok(json!({"prompts": PROMPTS.iter().map(|p| json!({
                "name": p.name, "title": p.title, "description": p.description,
                "arguments": p.args.iter().map(|(n, d, req)| json!({"name": n, "description": d, "required": req})).collect::<Vec<_>>()
            })).collect::<Vec<_>>()})),
            "prompts/get" => {
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                match PROMPTS.iter().find(|p| p.name == name) {
                    None => Err((-32602, format!("unknown prompt {name}; see prompts/list"))),
                    Some(p) => {
                        let args = params.get("arguments").cloned().unwrap_or(json!({}));
                        let mut text = p.text.to_string();
                        for (n, _, req) in p.args {
                            let v = args.get(*n).and_then(|v| v.as_str()).unwrap_or("");
                            if *req && v.trim().is_empty() {
                                return Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": format!("prompt {name} needs `{n}`")}}));
                            }
                            text = text.replace(&format!("{{{n}}}"), if v.trim().is_empty() { "(not given)" } else { v });
                        }
                        Ok(json!({"description": p.description, "messages": [{"role": "user", "content": {"type": "text", "text": text}}]}))
                    }
                }
            }
            m if m.starts_with("notifications/") => return None,
            other => Err((-32601, format!("method not found: {other}"))),
        };
        let id = id?; // notifications get no reply
        Some(match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err((code, message)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
        })
    }

    pub fn run(mut self) -> anyhow::Result<()> {
        // read stdin on a thread so a tool call can wait for the client's answer to an
        // elicitation while other messages queue up
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        self.rx = Some(rx);
        loop {
            let line = match self.backlog.pop_front() {
                Some(l) => l,
                None => match self.rx.as_ref().unwrap().recv() {
                    Ok(l) => l,
                    Err(_) => break,
                },
            };
            if line.trim().is_empty() {
                continue;
            }
            let reply = match serde_json::from_str::<Value>(&line) {
                Ok(Value::Array(batch)) => {
                    let out: Vec<Value> = batch.iter().filter_map(|m| self.handle(m)).collect();
                    if out.is_empty() {
                        None
                    } else {
                        Some(Value::Array(out))
                    }
                }
                Ok(msg) => self.handle(&msg),
                Err(e) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}})),
            };
            if let Some(r) = reply {
                let mut stdout = std::io::stdout().lock();
                writeln!(stdout, "{r}")?;
                stdout.flush()?;
            }
        }
        Ok(())
    }

    /// A gated call in a headless session: ask the user through the client (elicitation).
    fn call_gated_local(&mut self, name: &str, args: Value) -> Result<ToolOutput, ToolError> {
        use only3d_studio::approval::{finish, prepare, Decision};
        let client = self.client.clone();
        let req = {
            let Backend::Local(s) = self.backend() else { unreachable!("only called for local sessions") };
            let mut call = |t: &str, a: Value| s.call(t, a, Actor::Agent);
            match prepare(name, &args, &client, &mut call)? {
                Some(r) => r,
                None => return call(name, args),
            }
        };
        let decision = if self.can_elicit { self.elicit(&req) } else { Decision::Unavailable };
        let Backend::Local(s) = self.backend() else { unreachable!() };
        let mut call = |t: &str, a: Value| s.call(t, a, Actor::Agent);
        finish(&req, args, decision, &mut call, &mut |what, ok| eprintln!("only3d: approval {}: {what}", if ok { "granted" } else { "refused" }))
    }

    /// Ask the client's user to approve `req` (MCP `elicitation/create`); waits up to 5 minutes.
    fn elicit(&mut self, req: &only3d_studio::approval::ApprovalRequest) -> only3d_studio::approval::Decision {
        use only3d_studio::approval::Decision;
        let id = format!("only3d-approval-{}", self.next_out_id);
        self.next_out_id += 1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": "elicitation/create", "params": {
            "message": format!("only3d: {} wants to: {}\n\n{}", req.requester, req.title, req.details),
            "requestedSchema": {"type": "object", "properties": {"approve": {"type": "boolean", "title": "Approve", "description": req.title}}, "required": ["approve"]}
        }});
        {
            let mut out = std::io::stdout().lock();
            if writeln!(out, "{msg}").and_then(|_| out.flush()).is_err() {
                return Decision::Unavailable;
            }
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        let Some(rx) = self.rx.as_ref() else { return Decision::Unavailable };
        while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
            let Ok(line) = rx.recv_timeout(left) else { break };
            match serde_json::from_str::<Value>(&line) {
                Ok(v) if v.get("id") == Some(&json!(id)) && v.get("method").is_none() => {
                    let accept = v.pointer("/result/action").and_then(Value::as_str) == Some("accept");
                    let yes = v.pointer("/result/content/approve").and_then(Value::as_bool) == Some(true);
                    return if accept && yes { Decision::Approved } else { Decision::Denied };
                }
                _ => self.backlog.push_back(line),
            }
        }
        Decision::TimedOut
    }
}

/// MCP tool annotations, sending only what differs from the spec's defaults (readOnly false,
/// destructive true, idempotent false, openWorld true): 68 tools x 4 hints otherwise cost ~1.6k
/// tokens of every agent's context.
fn annotations(mutates: bool, side_effects: bool) -> Value {
    let mut a = serde_json::Map::new();
    if !mutates && !side_effects {
        a.insert("readOnlyHint".into(), json!(true));
    } else {
        // edits are undoable and printer commands are gated (print/stop/delete need confirm)
        a.insert("destructiveHint".into(), json!(false));
    }
    if !mutates {
        a.insert("idempotentHint".into(), json!(true));
    }
    if !side_effects {
        a.insert("openWorldHint".into(), json!(false));
    }
    Value::Object(a)
}

/// `only3d mcp install [app…]`: register this binary as the "only3d" MCP server.
pub fn install(apps: &[String], print: bool) -> anyhow::Result<()> {
    use only3d_studio::agents;
    let exe = dunce::canonicalize(std::env::current_exe()?)?;
    let places = agents::Places::from_env();
    if apps.is_empty() {
        println!("{}", agents::snippet("other", &exe));
        if !print {
            eprintln!("\nAdd that to any MCP client, or let only3d do it: `only3d mcp install <app>`:");
            for a in agents::APPS.iter().filter(|a| a.id != "other") {
                let st = agents::status(&places, a.id);
                let state = match (a.id, st.added, st.found) {
                    ("skill", true, _) => "installed",
                    ("skill", false, _) => "not installed",
                    (_, true, _) => "has only3d",
                    (_, false, true) => "found",
                    _ => "not found",
                };
                eprintln!("  {:<15} {:<15} {}", a.id, state, a.models);
            }
        }
        return Ok(());
    }
    let mut failed = false;
    for id in apps {
        if print {
            println!("{}", agents::snippet(id, &exe));
            continue;
        }
        match agents::install(&places, id, &exe) {
            Ok(msg) => println!("{msg}"),
            Err(e) => {
                eprintln!("{id}: {e}");
                failed = true;
            }
        }
    }
    if failed {
        anyhow::bail!("not every app was set up");
    }
    Ok(())
}

/// A successful `tools/call` result. Clients such as Claude Code show the model
/// `structuredContent` *instead of* the text when both are present, so the human-readable
/// summary (confirmations, checklists, next-step hints) must live inside it too, and tools
/// without data send text only.
fn tool_result(tool: &str, o: ToolOutput) -> Value {
    let text = if o.text.trim().is_empty() { format!("{tool}: done.") } else { o.text };
    let mut content = vec![json!({"type": "text", "text": text})];
    for im in &o.images {
        content.push(json!({"type": "image", "data": base64::engine::general_purpose::STANDARD.encode(&im.png), "mimeType": im.mime}));
    }
    let structured = match o.data {
        Value::Null => None,
        Value::Object(mut m) => {
            if !m.contains_key("summary") {
                m.insert("summary".into(), json!(text));
            }
            Some(Value::Object(m))
        }
        other => Some(json!({"summary": text, "result": other})),
    };
    match structured {
        Some(st) => json!({"content": content, "structuredContent": st, "isError": false}),
        None => json!({"content": content, "isError": false}),
    }
}
