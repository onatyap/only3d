//! Printer tools: discover, register, status, files, control and print — over the LAN with
//! `only3d-bambu`. Starting a print is gated: it needs a current slice made with the machine's
//! genuine start G-code, an idle printer, and an explicit `confirm: true`.

use std::path::PathBuf;
use std::time::Duration;

use only3d_bambu::{Calibration, Command, LightNode, PrintJob, PrinterClient, PrinterConfig, PrinterStatus};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::tools::{def, run_slice, Registry, ToolError, ToolOutput};
use crate::Studio;

pub struct PrinterSession {
    pub cfg: PrinterConfig,
    pub client: PrinterClient,
}

#[derive(Default, Serialize, Deserialize)]
pub struct PrinterBook {
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub printers: Vec<PrinterConfig>,
    /// Other makers' printers, on Moonraker, OctoPrint or PrusaLink.
    #[serde(default)]
    pub hosts: Vec<crate::hostprint::HostPrinter>,
}

impl PrinterBook {
    /// The default printer's name, else the first one's.
    pub fn default_name(&self) -> Option<String> {
        self.default.clone().or_else(|| self.printers.first().map(|p| p.name.clone()))
    }
}

fn book_path() -> PathBuf {
    crate::config_dir().join("printers.json")
}

pub fn load_book() -> PrinterBook {
    let mut b: PrinterBook = std::fs::read_to_string(book_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    // environment override (CI, scripts)
    if let (Ok(h), Ok(s), Ok(a)) = (std::env::var("ONLY3D_PRINTER_HOST"), std::env::var("ONLY3D_PRINTER_SERIAL"), std::env::var("ONLY3D_PRINTER_ACCESS_CODE")) {
        let mut c = PrinterConfig::new(h, s, a);
        c.name = "env".into();
        b.printers.retain(|p| p.name != "env");
        b.printers.insert(0, c);
        b.default.get_or_insert_with(|| "env".into());
    }
    b
}

fn save_book(b: &PrinterBook) -> Result<(), ToolError> {
    let p = book_path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| ToolError::new("io_error", e.to_string(), ""))?;
    }
    let printers: Vec<&PrinterConfig> = b.printers.iter().filter(|c| c.name != "env").collect();
    let text = serde_json::to_string_pretty(&json!({"default": b.default, "printers": printers, "hosts": b.hosts})).unwrap_or_default();
    std::fs::write(&p, text).map_err(|e| ToolError::new("io_error", format!("{}: {e}", p.display()), ""))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn bambu_err(e: only3d_bambu::Error) -> ToolError {
    ToolError::new(e.code(), e.to_string(), e.hint())
}

/// Bambu's 2025 firmware answers unsigned control commands with this in cloud mode.
pub const SIGNED_COMMANDS_CODE: &str = "84033543";
pub const SIGNED_COMMANDS_HINT: &str = "The printer only accepts signed print commands in cloud mode (Bambu's authorization system): lights and status work, but G-code, speed, pause/stop and print start are rejected. For full control from only3d, switch the printer to LAN-only mode and turn on Developer Mode (printer Settings → Network). Bambu Studio and Handy keep working over LAN, but cloud features (remote access) stop.";

/// The other maker's printer `name` names (or the default is), if it is one.
fn pick_host(book: &PrinterBook, name: Option<&str>) -> Option<crate::hostprint::HostPrinter> {
    let want = name.map(String::from).or_else(|| book.default.clone());
    match want {
        Some(n) => book.hosts.iter().find(|h| h.name == n).cloned(),
        None if book.printers.is_empty() => book.hosts.first().cloned(),
        None => None,
    }
}

fn pick(book: &PrinterBook, name: Option<&str>) -> Result<PrinterConfig, ToolError> {
    if let Some(h) = pick_host(book, name) {
        return Err(ToolError::new(
            "not_available",
            format!("'{}' is a {} printer: only3d prints on it and reads its status, but this needs a Bambu printer", h.name, h.kind.label()),
            "use print or printer_status with it, or its own web page for the rest",
        ));
    }
    let want = name.map(String::from).or_else(|| book.default.clone());
    let found = match &want {
        Some(n) => book.printers.iter().find(|p| &p.name == n || &p.serial == n),
        None => book.printers.first(),
    };
    found.cloned().ok_or_else(|| {
        ToolError::new(
            "no_printer",
            match want {
                Some(n) => format!("no printer named '{n}'"),
                None => "no printer configured".into(),
            },
            "run printer_discover to find the printer's IP and serial, then printer_add with the 8-digit LAN access code from the printer screen (Settings → LAN only / Network)",
        )
    })
}

/// Connect (or reuse the connection) to the named/default printer.
pub(crate) fn session<'a>(s: &'a mut Studio, name: Option<&str>) -> Result<&'a mut PrinterSession, ToolError> {
    let book = load_book();
    let cfg = pick(&book, name)?;
    let reuse = s.printer.as_ref().is_some_and(|p| p.cfg.serial == cfg.serial && p.client.is_connected());
    if !reuse {
        let client = PrinterClient::connect(&cfg).map_err(bambu_err)?;
        // trust on first use: pin the certificate we saw
        if cfg.cert_sha256.is_none() {
            if let Some(fp) = client.tls_fingerprint() {
                let mut book = load_book();
                if let Some(p) = book.printers.iter_mut().find(|p| p.serial == cfg.serial) {
                    p.cert_sha256 = Some(fp);
                    let _ = save_book(&book);
                }
            }
        }
        // log state changes, stages, alerts and progress for agents (wait_for_event)
        crate::events::watch_printer(s.events.clone(), cfg.name.clone(), client.subscribe());
        let _ = client.wait_for(|st| st.gcode_state != only3d_bambu::GcodeState::Unknown, Duration::from_secs(8));
        s.printer = Some(PrinterSession { cfg, client });
    }
    Ok(s.printer.as_mut().unwrap())
}

fn temp(t: &only3d_bambu::Temperature) -> String {
    match (t.current, t.target) {
        (Some(c), Some(tg)) if tg > 0.0 => format!("{c:.0}/{tg:.0}°C"),
        (Some(c), _) => format!("{c:.0}°C"),
        _ => "?".into(),
    }
}

/// Human description of an HMS code ("0500_0500_0001_0007") from Bambu Studio's local HMS
/// database, when installed.
pub fn hms_text(id: &str) -> Option<String> {
    static DB: std::sync::OnceLock<std::collections::HashMap<String, String>> = std::sync::OnceLock::new();
    let db = DB.get_or_init(|| {
        let mut m = std::collections::HashMap::new();
        let dir = only3d_bambu::bambu_studio_dir().join("hms");
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.starts_with("hms_en_") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(e.path()) else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
            fn walk(v: &Value, m: &mut std::collections::HashMap<String, String>) {
                match v {
                    Value::Object(o) => {
                        if let (Some(c), Some(t)) = (o.get("ecode").and_then(|x| x.as_str()), o.get("intro").and_then(|x| x.as_str())) {
                            m.entry(c.to_ascii_uppercase()).or_insert_with(|| t.to_string());
                        }
                        o.values().for_each(|x| walk(x, m));
                    }
                    Value::Array(a) => a.iter().for_each(|x| walk(x, m)),
                    _ => {}
                }
            }
            walk(&v, &mut m);
        }
        m
    });
    db.get(&id.replace('_', "").to_ascii_uppercase()).cloned()
}

pub fn status_text(name: &str, st: &PrinterStatus) -> String {
    let mut t = format!("{name}: {}", st.gcode_state);
    if st.gcode_state.is_busy() {
        if let Some(p) = st.progress_percent {
            t.push_str(&format!(" {p}%"));
        }
        if let (Some(l), Some(n)) = (st.layer, st.total_layers) {
            t.push_str(&format!(", layer {l}/{n}"));
        }
        if let Some(m) = st.remaining_minutes {
            t.push_str(&format!(", {}h{:02}m left", m / 60, m % 60));
        }
        if let Some(n) = &st.subtask_name {
            t.push_str(&format!(" ('{n}')"));
        }
    }
    // what the machine is doing right now (heating, leveling, purifying the air after a job...)
    if let Some(sn) = st.stage_name.as_deref().filter(|n| *n != "Printing") {
        t.push_str(&format!(" — {sn}"));
    }
    t.push_str(&format!("\nBed {}", temp(&st.bed)));
    if let Some(c) = &st.chamber {
        t.push_str(&format!(", chamber {}", temp(c)));
    }
    for n in &st.nozzles {
        t.push_str(&format!(
            "\nNozzle {} ({}): {}{}{}",
            n.index,
            n.side.clone().unwrap_or_default(),
            temp(&n.temperature),
            n.diameter_mm.map(|d| format!(", {d} mm")).unwrap_or_default(),
            if n.active { ", active" } else { "" }
        ));
    }
    if let Some(air) = &st.air {
        let on: Vec<String> = air.active().map(|p| format!("{} {}%", p.name, p.percent)).collect();
        if !on.is_empty() {
            t.push_str(&format!("\nAir{}: {}", air.mode_name.as_ref().map(|m| format!(" ({m})")).unwrap_or_default(), on.join(", ")));
        }
    }
    // trays as the printer's screen and the print preflight name them: A1..A4, ext left/right
    let shown = crate::trays::trays(st, &[]);
    let holds = |tr: &only3d_bambu::AmsTray| {
        let x = shown.iter().find(|x| x.tray == tr.global_id as i32);
        let what = match x {
            Some(x) if x.loaded => [x.line.clone().or(x.material.clone()), x.color.clone(), x.percent.map(|p| format!("{p}%"))].into_iter().flatten().collect::<Vec<_>>().join(" "),
            _ => "empty".into(),
        };
        format!("{} {what}", crate::trays::label(st, tr))
    };
    for a in &st.ams {
        let side = a.trays.first().and_then(|tr| tray_extruder(st, tr.global_id as i32)).map(|e| format!(" → {} nozzle", side_name(e))).unwrap_or_default();
        let unit = a.trays.first().map(|tr| crate::trays::label(st, tr).trim_end_matches(char::is_numeric).to_string()).unwrap_or_else(|| a.id.to_string());
        t.push_str(&format!("\nAMS {unit}{side}: {}", a.trays.iter().map(holds).collect::<Vec<_>>().join(", ")));
    }
    for e in &st.external_spools {
        if e.loaded {
            t.push_str(&format!("\nExternal spool: {}", holds(e)));
        }
    }
    if let Some(pe) = &st.print_error {
        t.push_str(&format!("\nPrint error {}", pe.id));
    }
    if !st.hms.is_empty() {
        t.push_str(&format!(
            "\nHMS: {}",
            st.hms
                .iter()
                .map(|h| format!("{} {} ({}){}", h.id, h.severity, h.module, hms_text(&h.id).map(|d| format!(": {d}")).unwrap_or_default()))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    if st.sdcard == Some(false) {
        t.push_str("\nNo removable storage (a USB flash drive in the H2D's top port; microSD on X1/P1/A1): only3d can read status, but LAN uploads (and so LAN printing) need it.");
    }
    t
}

// ------------------------------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub struct DiscoverIn {
    /// Seconds to listen (printers announce every few seconds; default 6).
    #[serde(default)]
    pub seconds: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct AddPrinterIn {
    /// Your name for it, e.g. "h2d".
    pub name: String,
    /// IP on the LAN; another maker's: its URL (http://ip:port).
    pub host: String,
    #[serde(default)]
    pub serial: Option<String>,
    /// Bambu: 8-digit LAN access code from its screen. OctoPrint, PrusaLink: API key.
    #[serde(default)]
    pub access_code: Option<String>,
    /// bambu (default), moonraker (Klipper), octoprint, prusalink.
    #[serde(default)]
    pub kind: Option<String>,
    /// Another maker's: its printer preset.
    #[serde(default)]
    pub machine: Option<String>,
    #[serde(default)]
    pub make_default: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct NameIn {
    /// Printer name or serial (default printer when omitted).
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Pause,
    Resume,
    Stop,
    LightOn,
    LightOff,
}

#[derive(Deserialize, JsonSchema)]
pub struct ControlIn {
    pub action: Action,
    #[serde(default)]
    pub name: Option<String>,
    /// Required for `stop` (cancels the print).
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct PrintIn {
    #[serde(default)]
    pub name: Option<String>,
    /// Must be true to actually upload and start. Without it you get the preflight checklist.
    /// Only set it when the user explicitly asked to start this print.
    #[serde(default)]
    pub confirm: bool,
    /// Feed from the AMS (default true). false = external spool.
    #[serde(default)]
    pub use_ams: Option<bool>,
    /// The tray to print from, by its number in the preflight's list (A1 = 0, B1 = 4; 254/255
    /// the left/right spool holders).
    #[serde(default)]
    pub tray: Option<i32>,
    /// The tray for each filament slot (index = slot − 1, −1 = none). Default: each slot's spool,
    /// else the loaded tray that matches; another line or colour is asked about.
    #[serde(default)]
    pub trays: Option<Vec<i32>>,
    #[serde(default)]
    pub timelapse: bool,
    /// Auto bed levelling (default on).
    #[serde(default)]
    pub bed_leveling: Option<bool>,
}

fn t_discover(_s: &mut Studio, i: DiscoverIn) -> Result<ToolOutput, ToolError> {
    let rep = only3d_bambu::discover_detailed(Duration::from_secs(i.seconds.unwrap_or(6).clamp(1, 30))).map_err(bambu_err)?;
    let mut t = if rep.printers.is_empty() { "No printers answered.".to_string() } else { format!("Found {} printer(s):", rep.printers.len()) };
    for p in &rep.printers {
        t.push_str(&format!(
            "\n- {} {} at {} (serial {}, {} mode, firmware {})",
            p.name.clone().unwrap_or_default(),
            p.model_name.clone().or(p.model_code.clone()).unwrap_or_default(),
            p.ip,
            p.serial,
            p.connect_mode.clone().unwrap_or("?".into()),
            p.firmware.clone().unwrap_or("?".into())
        ));
    }
    for w in &rep.warnings {
        t.push_str(&format!("\nNote: {w}"));
    }
    Ok(ToolOutput::text(t).with_data(json!({"printers": rep.printers.iter().map(|p| json!({"serial": p.serial, "ip": p.ip, "model": p.model_name, "model_code": p.model_code, "name": p.name, "connect_mode": p.connect_mode})).collect::<Vec<_>>(), "warnings": rep.warnings})))
}

fn t_add(s: &mut Studio, i: AddPrinterIn) -> Result<ToolOutput, ToolError> {
    let kind = i.kind.clone().unwrap_or_else(|| "bambu".into());
    if !kind.eq_ignore_ascii_case("bambu") {
        return add_host(s, i, &kind);
    }
    let (Some(serial), Some(code)) = (i.serial.clone(), i.access_code.clone()) else {
        return Err(ToolError::new("missing_field", "a Bambu printer needs its serial and LAN access code", "printer_discover finds the serial; the access code is on the printer's screen (Settings → Network)"));
    };
    let mut c = PrinterConfig::new(i.host, serial, code);
    c.name = i.name.clone();
    c.validate().map_err(bambu_err)?;
    let mut b = load_book();
    b.printers.retain(|p| p.name != c.name && p.serial != c.serial);
    b.printers.push(c);
    if i.make_default.unwrap_or(true) || b.default.is_none() {
        b.default = Some(i.name.clone());
    }
    save_book(&b)?;
    Ok(ToolOutput::text(format!("Saved printer '{}' to {} (readable only by you). Next: printer_status.", i.name, book_path().display())))
}

/// `printer_add` for another maker's printer.
fn add_host(s: &mut Studio, i: AddPrinterIn, kind: &str) -> Result<ToolOutput, ToolError> {
    use crate::hostprint::{HostKind, HostPrinter};
    let k = HostKind::parse(kind).ok_or_else(|| ToolError::new("bad_kind", format!("unknown printer kind '{kind}'"), "bambu, moonraker (Klipper), octoprint or prusalink"))?;
    let Some(machine) = i.machine.clone() else {
        return Err(ToolError::new("missing_field", "say which printer it is (machine: its printer preset)", "list_presets {\"kind\": \"machine\", \"query\": \"<model>\"} finds it"));
    };
    if !s.presets("machine", &machine).contains(&machine) {
        return Err(ToolError::new("unknown_preset", format!("no printer preset '{machine}'"), "list_presets {\"kind\": \"machine\"} lists them"));
    }
    let h = HostPrinter { name: i.name.clone(), kind: k, url: i.host.clone(), api_key: i.access_code.clone().filter(|k| !k.trim().is_empty()), machine: Some(machine.clone()) };
    let mut b = load_book();
    b.printers.retain(|p| p.name != h.name);
    b.hosts.retain(|x| x.name != h.name);
    b.hosts.push(h.clone());
    if i.make_default.unwrap_or(true) || b.default.is_none() {
        b.default = Some(i.name.clone());
    }
    save_book(&b)?;
    let answer = match crate::hostprint::status(&h) {
        Ok(st) => format!("It answers: {}.", st.state),
        Err(e) => format!("It doesn't answer yet ({e}): check the URL and API key."),
    };
    Ok(ToolOutput::text(format!("Saved '{}', a {} on {} ({machine}), to {} (readable only by you). {answer}", i.name, k.label(), h.url, book_path().display())))
}

fn t_list(_s: &mut Studio, _: crate::tools::Empty) -> Result<ToolOutput, ToolError> {
    let b = load_book();
    let mut lines: Vec<String> = b
        .printers
        .iter()
        .map(|p| format!("- {}{}: {} serial {} {}", p.name, if b.default.as_deref() == Some(&p.name) { " (default)" } else { "" }, p.host, p.serial, if p.cert_sha256.is_some() { "cert pinned" } else { "cert not pinned yet" }))
        .collect();
    lines.extend(b.hosts.iter().map(|h| format!("- {}{}: {} on {}, {}", h.name, if b.default.as_deref() == Some(&h.name) { " (default)" } else { "" }, h.kind.label(), h.url, h.machine.as_deref().unwrap_or("printer preset not set"))));
    Ok(ToolOutput::text(if lines.is_empty() { "No printers configured. Use printer_discover then printer_add.".into() } else { lines.join("\n") })
        .with_data(json!(b.printers.iter().map(|p| json!({"name": p.name, "host": p.host, "serial": p.serial, "pinned": p.cert_sha256.is_some()})).collect::<Vec<_>>())))
}

fn t_status(s: &mut Studio, i: NameIn) -> Result<ToolOutput, ToolError> {
    status(s, i.name.as_deref())
}

/// What `printer_status` returns, without a journal entry: for UIs that poll it.
pub fn status(s: &mut Studio, name: Option<&str>) -> Result<ToolOutput, ToolError> {
    if let Some(h) = pick_host(&load_book(), name) {
        let st = crate::hostprint::status(&h).map_err(|e| ToolError::new("printer_unreachable", e, "check the printer is on, and its URL and API key (printer_add)"))?;
        let progress = st.progress.map(|p| format!(", {:.0}% done", p * 100.0)).unwrap_or_default();
        let file = st.file.as_deref().map(|f| format!(" ({f})")).unwrap_or_default();
        return Ok(ToolOutput::text(format!("{} ({} on {}): {}{progress}{file}{}", h.name, h.kind.label(), h.url, st.state, if st.idle { ", idle" } else { "" }))
            .with_data(json!({"name": h.name, "kind": h.kind, "state": st.state, "idle": st.idle, "progress": st.progress})));
    }
    let ps = session(s, name)?;
    let st = ps.client.status();
    let text = status_text(&ps.cfg.name, &st);
    Ok(ToolOutput::text(text).with_data(serde_json::to_value(&st).unwrap_or(Value::Null)))
}

#[derive(Deserialize, JsonSchema)]
pub struct FilesIn {
    #[serde(default)]
    pub name: Option<String>,
    /// Folder on the printer (default "/").
    #[serde(default)]
    pub path: Option<String>,
}

fn t_files(s: &mut Studio, i: FilesIn) -> Result<ToolOutput, ToolError> {
    let cfg = pick(&load_book(), i.name.as_deref())?;
    let path = i.path.unwrap_or_else(|| "/".into());
    let mut ftp = only3d_bambu::FtpsClient::connect(&cfg).map_err(bambu_err)?;
    let raw = ftp.list_raw(&path).map_err(bambu_err)?;
    let files = ftp.list(&path).unwrap_or_default();
    let pwd = ftp.cmd("PWD").map(|r| r.text).unwrap_or_default();
    let _ = ftp.quit();
    let names: Vec<String> = files.iter().map(|f| format!("{}{} ({} KB)", f.name, if f.is_dir { "/" } else { "" }, f.size.unwrap_or(0) / 1024)).collect();
    let _ = s;
    Ok(ToolOutput::text(format!("{} entr(ies) in {path} (FTP home: {}):\n{}", files.len(), pwd.trim(), names.join("\n"))).with_data(json!({"path": path, "files": files, "raw": raw})))
}

fn t_control(s: &mut Studio, i: ControlIn) -> Result<ToolOutput, ToolError> {
    if i.action == Action::Stop && !i.confirm {
        return Err(ToolError::new("confirm_required", "stop cancels the running print and cannot be undone", "ask the user, then call again with confirm: true"));
    }
    let ps = session(s, i.name.as_deref())?;
    let cmd = match i.action {
        Action::Pause => Command::Pause,
        Action::Resume => Command::Resume,
        Action::Stop => Command::Stop,
        Action::LightOn => Command::Light { node: LightNode::ChamberLight, on: true },
        Action::LightOff => Command::Light { node: LightNode::ChamberLight, on: false },
    };
    match ps.client.request(&cmd, Duration::from_secs(4)) {
        Ok(reply) => {
            let body = reply.as_object().and_then(|o| o.values().next()).cloned().unwrap_or(Value::Null);
            let reason = body.get("reason").and_then(|r| r.as_str()).unwrap_or("");
            let failed = body.get("result").and_then(|r| r.as_str()).is_some_and(|r| r.eq_ignore_ascii_case("fail") || r.eq_ignore_ascii_case("failed"));
            if failed || body.get("err_code").and_then(|e| e.as_i64()).is_some_and(|c| c != 0) {
                let code = body.get("err_code").and_then(|e| e.as_i64()).unwrap_or(0);
                return Err(if code.to_string() == SIGNED_COMMANDS_CODE {
                    ToolError::new("printer_auth_required", format!("{:?} rejected: {reason}", i.action), SIGNED_COMMANDS_HINT)
                } else {
                    ToolError::new("command_rejected", format!("{:?} rejected: {reason} (code {code})", i.action), "check the printer screen")
                });
            }
        }
        Err(e) if e.code() == "timeout" => {}
        Err(e) => return Err(bambu_err(e)),
    }
    if matches!(i.action, Action::LightOn | Action::LightOff) && ps.client.status().lights.iter().any(|l| l.node == "chamber_light2") {
        ps.client.send(&Command::Light { node: LightNode::ChamberLight2, on: i.action == Action::LightOn }).map_err(bambu_err)?;
    }
    std::thread::sleep(Duration::from_millis(800));
    let st = ps.client.status();
    Ok(ToolOutput::text(format!("Sent {:?}. {}", i.action, status_text(&ps.cfg.name, &st).lines().next().unwrap_or(""))))
}

/// Logical extruder (0 = left, 1 = right) that `tray` is wired to on a dual-nozzle printer.
/// AMS units report the nozzle they feed (firmware numbering: 0 = right); the external holders
/// are 255 = right (main) and 254 = left (deputy). `None` on single-nozzle printers or when the
/// tray is unknown.
pub(crate) fn tray_extruder(st: &PrinterStatus, tray: i32) -> Option<u32> {
    if st.nozzles.len() < 2 {
        return None;
    }
    let firmware = match tray {
        255 => 0,
        254 => 1,
        t if t >= 0 => st.ams.iter().find(|a| a.trays.iter().any(|tr| tr.global_id as i32 == t))?.extruder? as u32,
        _ => return None,
    };
    Some(1 - firmware.min(1))
}

/// The checklist line comparing the printer model a plate is sliced for with the one the
/// printer reports: a mismatch fails (another model's start G-code homes, probes and heats for a
/// different machine).
fn model_check(want: Option<&str>, have: Option<&str>) -> Option<(bool, String)> {
    Some(match (want?, have) {
        (want, Some(have)) if want.eq_ignore_ascii_case(have) => (true, format!("sliced for this printer ({have})")),
        (want, Some(have)) => (false, format!("this plate is sliced for a {want}, but the printer is a {have}: set_printer to its preset (list_presets machine \"{}\") and slice again", have.trim_start_matches("Bambu Lab "))),
        (want, None) => (true, format!("sliced for a {want} (the printer didn't say its model)")),
    })
}

fn side_name(logical: u32) -> &'static str {
    if logical == 0 {
        "left"
    } else {
        "right"
    }
}

/// The refusal for a print whose filament is waiting on the user's choice: nothing is sent, and
/// nobody is asked to approve a print that can't start as asked.
fn filament_choice(question: &str, details: &str) -> ToolError {
    ToolError::new("filament_choice_needed", format!("nothing was sent: the loaded filament isn't the project's\n{details}"), question)
}

/// `print`'s preflight (without confirm) waiting on the user's filament choice, as the error the
/// print itself would give: approval returns it to the agent instead of asking the user to approve.
pub fn choice_needed(pre: &ToolOutput) -> Option<ToolError> {
    (pre.data.get("needs_choice") == Some(&json!(true))).then(|| filament_choice(pre.data.get("question").and_then(Value::as_str).unwrap_or(""), &pre.text))
}

/// `print` on another maker's printer: the same preflight (a current slice with the printer's
/// own start G-code, for this printer, a passing G-code check, printable parts, an idle printer)
/// and the same approval, then upload and start over its API.
fn host_print(s: &mut Studio, h: crate::hostprint::HostPrinter, i: PrintIn) -> Result<ToolOutput, ToolError> {
    let mut checklist: Vec<(bool, String)> = Vec::new();
    let machine = h.machine.clone().unwrap_or_default();
    checklist.push(if machine == s.project.printer.machine {
        (true, format!("sliced for this printer ({machine})"))
    } else {
        (false, format!("this plate is sliced for {}, but '{}' is a {machine}: set_printer {{\"machine\": \"{machine}\"}} and slice again", s.project.printer.machine, h.name))
    });
    if !s.slice_is_current() {
        run_slice(s)?;
    }
    let sl = s.last_slice.clone().expect("sliced");
    let safe = sl.engine.contains("bambu-templates");
    checklist.push((safe, if safe { format!("G-code uses the printer's own start/end sequence ({machine})") } else { "G-code uses a GENERIC start sequence (no printer profile) — refusing to send".into() }));
    checklist.push((sl.lint.ok, if sl.lint.ok { format!("G-code check: {} extrusions inside the plate, max {:.0} mm/s", sl.lint.extrusion_moves, sl.lint.max_feed_mm_s) } else { format!("G-code check failed: {}", sl.lint.problems.join("; ")) }));
    let objs = s.evaluated()?;
    let errors: Vec<String> = only3d_model::check(&s.project, &objs, &s.bed).into_iter().filter(|f| f.severity == only3d_model::Severity::Error).map(|f| f.message).collect();
    checklist.push((errors.is_empty(), if errors.is_empty() { "printability checks pass".into() } else { format!("printability errors: {}", errors.join("; ")) }));
    match crate::hostprint::status(&h) {
        Ok(st) => checklist.push((st.idle, format!("printer state {}", st.state))),
        Err(e) => checklist.push((false, format!("can't read the printer: {e}"))),
    }
    let untested = s.machine.printer_model(&machine).and_then(|m| crate::tested::print_note(&m));
    let st_s = &sl.result.stats;
    let summary = format!("{} layers, ~{:.0} min, {:.1} g", st_s.layers, st_s.print_time_s / 60.0, st_s.filament_g);
    let check_text = checklist.iter().map(|(ok, m)| format!("[{}] {m}", if *ok { "x" } else { " " })).collect::<Vec<_>>().join("\n");
    if !checklist.iter().all(|c| c.0) {
        return Err(ToolError::new("preflight_failed", format!("not ready to print ({summary}):\n{check_text}"), "fix the unchecked items, then call print again"));
    }
    if !i.confirm {
        return Ok(ToolOutput::text(format!(
            "Ready to print on '{}' ({summary}):\n{check_text}\n{}Make sure the plate is clear and has the filament the project expects ({}). Call print again with confirm: true to upload and start — only if the user asked to start it.",
            h.name,
            untested.as_ref().map(|n| format!("{n}\n")).unwrap_or_default(),
            s.project.filaments.first().map(|f| f.preset.as_str()).unwrap_or("?"),
        ))
        .with_data(json!({"ready": true, "summary": summary, "untested": untested, "checklist": checklist.iter().map(|c| json!({"ok": c.0, "item": c.1})).collect::<Vec<_>>()})));
    }
    let file = format!("{}.gcode", sanitize(&s_name(s)));
    let said = crate::hostprint::upload_and_start(&h, &file, sl.result.gcode.as_bytes()).map_err(|e| ToolError::new("upload_failed", e, "check the printer is on, and its URL and API key (printer_add)"))?;
    Ok(ToolOutput::text(format!("Printing on '{}': {said} ({summary}). printer_status follows it.", h.name)))
}

fn t_print(s: &mut Studio, i: PrintIn) -> Result<ToolOutput, ToolError> {
    if let Some(h) = pick_host(&load_book(), i.name.as_deref()) {
        return host_print(s, h, i);
    }
    // 0. which tray feeds each filament (given, its inventory spool's place, or for several
    //    filaments the loaded tray that matches), and which nozzle each tray is wired to
    let ps = session(s, i.name.as_deref())?;
    let (st, have_model) = (ps.client.status(), ps.client.model_info().map(|m| m.name));
    let objs = s.evaluated()?;
    let mut used: Vec<u32> = objs.iter().filter(|o| !o.hidden).flat_map(|o| o.filaments()).collect();
    used.sort_unstable();
    used.dedup();
    if used.is_empty() {
        used.push(1);
    }
    let multi = used.len() > 1;
    if multi && i.tray.is_some() && i.trays.is_none() {
        return Err(ToolError::new(
            "bad_input",
            format!("print.tray: this plate prints {} filaments ({}), so one tray isn't enough", used.len(), used.iter().map(|k| k.to_string()).collect::<Vec<_>>().join(", ")),
            "pass trays instead: one tray per filament slot, in slot order ([tray for slot 1, slot 2, ...], −1 = unused); print without confirm lists the trays",
        ));
    }
    // every tray the printer has, named as on its screen, with the inventory spool in it
    let spools = crate::inventory::Db::open().and_then(|d| d.spools(false)).unwrap_or_default();
    let loaded = crate::trays::trays(&st, &spools);
    let mut checklist: Vec<(bool, String)> = Vec::new();
    let mut trays: Vec<(u32, i32)> = Vec::new();
    // how each tray was picked: the user's (tray/trays or a linked spool's place) or matched here
    let mut picked_by_user: Vec<bool> = Vec::new();
    for &k in &used {
        let idx = k as usize - 1;
        let given = i.trays.as_ref().and_then(|t| t.get(idx)).copied().filter(|t| *t >= 0);
        let given = if multi { given } else { i.tray.or(given) };
        let from_spool = crate::spools::spool_for_slot(&s.project, idx).and_then(|sp| sp.location).and_then(|l| crate::spools::location_tray(&l));
        let taken: Vec<i32> = trays.iter().map(|t| t.1).collect();
        let f = &s.project.filaments[idx];
        let matched = crate::trays::best(&loaded, &f.preset, &f.color, &taken);
        match (given.or(from_spool), matched) {
            (Some(t), _) => {
                trays.push((k, t));
                picked_by_user.push(true);
            }
            (None, Some(t)) => {
                trays.push((k, t));
                picked_by_user.push(false);
            }
            // the printer reports no trays at all: the AMS's first, as before
            (None, None) if loaded.is_empty() => {
                trays.push((k, 0));
                picked_by_user.push(false);
            }
            (None, None) => checklist.push((
                false,
                format!("filament {k} ({} {}): no loaded tray holds {} — load it (the trays are listed below), link its spool (set_filament spool), or pass {}", f.preset, f.color, crate::trays::material(&f.preset), if multi { "trays" } else { "tray" }),
            )),
        }
    }
    // "A1 (tray 0)" where the printer reports that tray
    let tray_name = |t: i32| loaded.iter().find(|x| x.tray == t).map(|x| x.named()).unwrap_or_else(|| format!("tray {t}"));
    // "auto" nozzles: print each filament with the nozzle its tray is wired to
    let mut resolved = String::new();
    let current: Vec<usize> = if multi { crate::bambu::assign_extruders(&s.project, &objs, &used).unwrap_or_default() } else { vec![crate::machine::choose_extruder(&s.project, &objs, &s.bed).map(|e| e as usize).unwrap_or(s.bed.main_nozzle)] };
    for (n, (k, t)) in trays.iter().enumerate() {
        let idx = *k as usize - 1;
        let Some(need) = tray_extruder(&st, *t) else { continue };
        let have = if multi { current.get(idx).copied() } else { current.first().copied() };
        if s.project.filaments.get(idx).is_some_and(|f| f.nozzle == only3d_model::NozzleSel::Auto) && have != Some(need as usize) {
            s.project.filaments[idx].nozzle = if need == 0 { only3d_model::NozzleSel::Left } else { only3d_model::NozzleSel::Right };
            s.changed("print");
            resolved.push_str(&format!("{}filament {k} set from auto to {}", if n == 0 && resolved.is_empty() { " (" } else { ", " }, side_name(need)));
        }
    }
    if !resolved.is_empty() {
        resolved.push(')');
    }
    let tray = trays.first().map(|t| t.1).unwrap_or(0);
    let slot_idx = used[0] as usize - 1;
    let spool = crate::spools::spool_for_slot(&s.project, slot_idx);
    let feeds = tray_extruder(&st, tray);
    // 1. a current, printer-safe slice
    if !s.slice_is_current() {
        run_slice(s)?;
    }
    let sl = s.last_slice.clone().expect("sliced");
    let safe = sl.engine.contains("bambu-templates");
    let machine = s.project.printer.machine.clone();
    checklist.push((safe, if safe { format!("G-code uses Bambu's own start/end sequence for the {machine}") } else { "G-code uses a GENERIC start sequence (Bambu profiles not loaded) — refusing to send".into() }));
    // the plate must be sliced for the printer it goes to: another model's start G-code homes,
    // probes and heats for a different machine
    let want_model = s.machine.printer_model(&machine);
    let untested = have_model.map(str::to_string).or(want_model.clone()).and_then(|m| crate::tested::print_note(&m));
    checklist.extend(model_check(want_model.as_deref(), have_model));
    checklist.push((sl.lint.ok, if sl.lint.ok { format!("G-code check: {} extrusions inside the nozzle's reach, max {:.0} mm/s", sl.lint.extrusion_moves, sl.lint.max_feed_mm_s) } else { format!("G-code check failed: {}", sl.lint.problems.join("; ")) }));
    let objs = s.evaluated()?;
    let errors: Vec<String> = only3d_model::check(&s.project, &objs, &s.bed).into_iter().filter(|f| f.severity == only3d_model::Severity::Error).map(|f| f.message).collect();
    checklist.push((errors.is_empty(), if errors.is_empty() { "printability checks pass".into() } else { format!("printability errors: {}", errors.join("; ")) }));
    // 2. printer state: idle, with storage, and no error of its own (a fatal or serious HMS
    //    stops a print, even one left over from the last job)
    let idle = !st.gcode_state.is_busy();
    checklist.push((idle, format!("printer state {}", st.gcode_state)));
    if st.sdcard == Some(false) {
        checklist.push((false, "no storage in the printer: plug a USB flash drive into the H2D's top port (microSD on X1/P1/A1); LAN uploads need it, cloud jobs use internal storage".into()));
    }
    let blocking = |h: &&only3d_bambu::HmsError| h.severity.eq_ignore_ascii_case("fatal") || h.severity.eq_ignore_ascii_case("serious");
    for h in st.hms.iter().filter(blocking) {
        checklist.push((false, format!("printer error {} ({}, {}){}: fix it and clear it on the printer's screen, then call print again", h.id, h.severity, h.module, hms_text(&h.id).map(|d| format!(": {d}")).unwrap_or_default())));
    }
    let notices: Vec<String> = st.hms.iter().filter(|h| !blocking(h)).map(|h| format!("{} ({}){}", h.id, h.severity, hms_text(&h.id).map(|d| format!(": {d}")).unwrap_or_default())).collect();
    // 3. filament: each project filament's tray, what is on it, the nozzle it feeds and enough of
    //    it. Another line or colour than the project's is the user's call: a tray matched here
    //    with one is asked about, never ticked.
    let mut questions: Vec<String> = Vec::new();
    for (n, (k, t)) in trays.iter().enumerate() {
        let idx = *k as usize - 1;
        let f = &s.project.filaments[idx];
        let name = tray_name(*t);
        if multi {
            let have = sl.result.job.nozzles.as_ref().and_then(|n| n.extruder_of.get(idx).copied()).unwrap_or(1) as u32;
            if let Some(need) = tray_extruder(&st, *t) {
                checklist.push(if have == need {
                    (true, format!("filament {k}: the {} nozzle, fed by {name}", side_name(need)))
                } else {
                    (false, format!("filament {k}: the G-code prints it with the {} nozzle, but {name} feeds the {} one — set_filament {{\"slot\": {k}, \"nozzle\": \"auto\"}}, or load it on the {} side", side_name(have), side_name(need), side_name(have)))
                });
            }
        }
        let want = format!("{} {}", f.preset.split(" @").next().unwrap_or(&f.preset), f.color);
        let material = crate::trays::material(&f.preset);
        match loaded.iter().find(|x| x.tray == *t) {
            Some(x) if !x.loaded => checklist.push((false, format!("filament {k} ({want}): {name} is empty — load it there, or pass another {}", if multi { "tray in trays" } else { "tray" }))),
            Some(x) if !crate::trays::material_fits(&f.preset, x) => checklist.push((false, format!("filament {k} is {material} ({want}), but {name} holds {} — load {material} there, or pick another tray", x.holds()))),
            Some(x) => match (crate::trays::difference(&f.preset, &f.color, x), picked_by_user[n]) {
                (None, _) => checklist.push((true, format!("filament {k} ({want}) → {name}: {}", x.holds()))),
                (Some(d), true) => checklist.push((true, format!("filament {k} ({want}) → {name}, as chosen: {} ({d})", x.holds()))),
                (Some(d), false) => questions.push(format!("filament {k} ({want}) → {name}, the nearest loaded {material}: {} ({d})", x.holds())),
            },
            None if !loaded.is_empty() => checklist.push((false, format!("filament {k}: the printer has no {name} (its trays are listed below)"))),
            None => {}
        }
        // enough of it: the linked spool's ledger, else the spool in that tray, else the AMS reading
        let need = if multi { sl.result.stats.filaments.iter().find(|x| x.0 == idx).map(|x| x.2).unwrap_or(0.0) } else { sl.result.stats.filament_g };
        let left = crate::spools::spool_for_slot(&s.project, idx).map(|sp| (sp.remaining_g, format!("spool {}", sp.id))).or_else(|| {
            let x = loaded.iter().find(|x| x.tray == *t && x.loaded)?;
            let from = match (&x.spool, x.percent) {
                (Some(id), _) => format!("spool {id} in {}", x.label),
                (None, Some(p)) => format!("{} reads {p}% of a 1 kg spool", x.label),
                _ => x.label.clone(),
            };
            Some((x.grams_left?, from))
        });
        if let Some((g, from)) = left {
            checklist.push((g >= need * 1.05, format!("filament {k}: {}{g:.0} g left ({from}), the job needs {need:.1} g", if from.contains('%') { "about " } else { "" })));
        }
    }
    // nozzle: the G-code must heat, prime and print with the nozzle this tray is wired to
    if let (false, Some(need)) = (multi, feeds) {
        let have = sl.result.job.settings.extruder;
        let name = tray_name(tray);
        checklist.push(if have == need {
            (true, format!("nozzle: prints with the {} nozzle, which {name} feeds{resolved}", side_name(need)))
        } else {
            (false, format!(
                "nozzle: the G-code prints with the {} nozzle, but {name} feeds the {} one — set_filament {{\"slot\": {}, \"nozzle\": \"auto\"}} (or \"{}\"), or print from a tray on the {} side",
                side_name(have), side_name(need), slot_idx + 1, side_name(need), side_name(have)
            ))
        });
    }
    let st_s = &sl.result.stats;
    let summary = format!("{} layers, ~{:.0} min, {:.1} g", st_s.layers, st_s.print_time_s / 60.0, st_s.filament_g);
    let all_ok = checklist.iter().all(|c| c.0);
    let mut check_text = checklist.iter().map(|(ok, m)| format!("[{}] {m}", if *ok { "x" } else { " " })).collect::<Vec<_>>().join("\n");
    for q in &questions {
        check_text.push_str(&format!("\n[?] {q}"));
    }
    let on_printer = if loaded.is_empty() { String::new() } else { format!("\nOn the printer (print takes the tray number):\n{}", crate::trays::listing(&loaded)) };
    let notes = if notices.is_empty() { String::new() } else { format!("\nPrinter notices: {}", notices.join("; ")) };
    // the trays `print` would use, as its `trays` argument: one per project filament, −1 unused
    let trays_arg: Vec<i32> = (1..=s.project.filaments.len() as u32).map(|k| trays.iter().find(|t| t.0 == k).map(|t| t.1).unwrap_or(-1)).collect();
    // each filament's tray, and every tray that holds its material (best first) to pick from
    let mapping: Vec<Value> = trays
        .iter()
        .zip(&picked_by_user)
        .map(|((k, t), by_user)| {
            let f = &s.project.filaments[*k as usize - 1];
            let here = loaded.iter().find(|x| x.tray == *t);
            let choices: Vec<Value> = crate::trays::ranked(&loaded, &f.preset, &f.color).into_iter().map(|x| json!({"tray": x.tray, "label": x.label, "difference": crate::trays::difference(&f.preset, &f.color, x)})).collect();
            json!({
                "slot": k,
                "filament": f.preset.split(" @").next().unwrap_or(&f.preset),
                "color": f.color,
                "tray": t,
                "label": here.map(|x| x.label.clone()),
                "chosen": by_user,
                "difference": here.and_then(|x| crate::trays::difference(&f.preset, &f.color, x)),
                "choices": choices,
            })
        })
        .collect();
    let items = checklist.iter().map(|c| json!({"ok": c.0, "item": c.1})).collect::<Vec<_>>();
    if !all_ok {
        return Err(ToolError::new("preflight_failed", format!("not ready to print ({summary}):\n{check_text}{on_printer}{notes}"), "fix the unchecked items, then call print again"));
    }
    if !questions.is_empty() {
        let arg = if multi { format!("{{\"trays\": {}}}", serde_json::to_string(&trays_arg).unwrap_or_default()) } else { format!("{{\"tray\": {tray}}}") };
        let question = format!(
            "The loaded filament isn't the project's: {}. Ask the user which to print with: {} anyway (print {arg}), another tray from the list (its number in {}), or load the project's filament and call print again.",
            questions.join("; "),
            if multi { "these trays".to_string() } else { tray_name(tray) },
            if multi { "trays" } else { "tray" }
        );
        if i.confirm {
            return Err(filament_choice(&question, &format!("{check_text}{on_printer}")));
        }
        return Ok(ToolOutput::text(format!("Not started: the filament needs the user's choice ({summary}):\n{check_text}{on_printer}{notes}\n\n{question}"))
            .with_data(json!({"ready": false, "needs_choice": true, "question": question, "summary": summary, "trays": trays_arg, "mapping": mapping, "printer_trays": loaded, "notices": notices, "untested": untested, "checklist": items})));
    }
    if !i.confirm {
        // show the plate (when the LAN camera is on) so whoever confirms can see it is clear
        let cfg = s.printer.as_ref().map(|p| p.cfg.clone());
        let snap = cfg.as_ref().and_then(|c| t_camera(s, CameraIn { name: Some(c.name.clone()) }).ok());
        let ps = session(s, i.name.as_deref())?;
        let mut out = ToolOutput::text(format!(
            "Ready to print on '{}' ({summary}):\n{check_text}{on_printer}{notes}\n{}Make sure the plate is clear and is the {} the project expects. Call print again with confirm: true to upload and start — only if the user asked to start it.",
            ps.cfg.name,
            untested.as_ref().map(|n| format!("{n}\n")).unwrap_or_default(),
            s_bed(&sl)
        ))
        .with_data(json!({"ready": true, "summary": summary, "trays": trays_arg, "mapping": mapping, "printer_trays": loaded, "notices": notices, "untested": untested, "checklist": items}));
        if let Some(snap) = snap {
            out.images.extend(snap.images);
            out.text.push_str("\nAttached: a camera picture of the plate — check it is empty.");
        }
        return Ok(out);
    }
    // 3. upload + start
    let name = format!("{}.gcode.3mf", sanitize(&s_name(s)));
    let cfg = s.printer.as_ref().unwrap().cfg.clone();
    let up = only3d_bambu::upload_file(&cfg, only3d_bambu::UploadSource::Bytes(&sl.package), &name, None).map_err(bambu_err)?;
    let mut job = PrintJob::new(&up.remote_name);
    if multi {
        // one entry per filament of the G-code's config (every project slot), −1 = unused
        job.use_ams = i.use_ams.unwrap_or(trays.iter().any(|t| t.1 < 254));
        job.ams_mapping = (1..=s.project.filaments.len() as u32).map(|k| trays.iter().find(|t| t.0 == k).map(|t| t.1).unwrap_or(-1)).collect();
    } else {
        job.use_ams = i.use_ams.unwrap_or(tray < 254);
        job.ams_mapping = vec![tray];
    }
    job.timelapse = i.timelapse;
    job.bed_leveling = if i.bed_leveling.unwrap_or(true) { Calibration::On } else { Calibration::Off };
    job.subtask_name = Some(s_name(s));
    let ps = s.printer.as_ref().unwrap();
    let ack = only3d_bambu::start_print(&ps.client, &job).map_err(|e| {
        let mut t = bambu_err(e);
        if t.code == "print_rejected" || t.code == "auth_failed" {
            t.hint = format!("{} On current H2D firmware, third-party print starts need LAN-only mode with Developer Mode enabled (printer Settings → Network). Until then, open the exported .gcode.3mf in Bambu Studio and print from there.", t.hint);
        }
        t
    })?;
    // inventory: a traceable print record (project version, objects, spools) charging the spool
    let job_name = job.subtask_name.clone().unwrap_or_default();
    let printer_name = s.printer.as_ref().map(|p| p.cfg.name.clone());
    // this plate is printing: it leaves the queue
    let plate = s.plate;
    s.project.queue.retain(|k| *k != plate);
    let charged = match crate::spools::record_print(s, "RUNNING", &job_name, Some(&up.remote_name), printer_name.as_deref(), Some(&ack.task_id), &trays, true) {
        Ok(pid) => format!(
            " Recorded as {pid}{}.",
            spool
                .as_ref()
                .map(|sp| if sp.rfid_uuid.is_some() {
                    format!("; {} (RFID) is charged from the AMS readings as it prints (est. {:.1} g)", sp.id, sl.result.stats.filament_g)
                } else {
                    format!(", {:.1} g (estimate) charged to {}", sl.result.stats.filament_g, sp.id)
                })
                .unwrap_or_default()
        ),
        Err(e) => format!(" (inventory not updated: {})", e.message),
    };
    Ok(ToolOutput::text(format!(
        "Uploaded {} ({} KB) and sent the print command{}.{charged} Watch it with printer_status.",
        up.remote_name,
        up.bytes / 1024,
        if ack.acknowledged { " — printer acknowledged" } else { " (no acknowledgement yet; check printer_status)" }
    ))
    .with_data(json!({"remote_name": up.remote_name, "acknowledged": ack.acknowledged, "task_id": ack.task_id})))
}

#[derive(Deserialize, JsonSchema)]
pub struct WaitIn {
    #[serde(default)]
    pub name: Option<String>,
    /// Wait until the print state is one of these (e.g. ["FINISH", "FAILED", "IDLE", "PAUSE"]).
    #[serde(default)]
    pub states: Vec<String>,
    /// Or until this layer number is reached.
    #[serde(default)]
    pub layer: Option<u32>,
    /// Or until progress reaches this percentage.
    #[serde(default)]
    pub percent: Option<u8>,
    /// Give up after this many seconds (default 600, max 3600).
    #[serde(default)]
    pub timeout_s: Option<u64>,
}

/// Whether `printer_wait`'s condition holds (or the printer needs attention).
pub fn wait_met(i: &WaitIn, st: &PrinterStatus) -> bool {
    i.states.iter().any(|x| st.gcode_state.as_str().eq_ignore_ascii_case(x))
        || i.layer.is_some_and(|l| st.layer.unwrap_or(0) >= l)
        || i.percent.is_some_and(|p| st.progress_percent.unwrap_or(0) >= p)
        || st.print_error.is_some()
        || st.hms.iter().any(|h| h.severity.eq_ignore_ascii_case("fatal") || h.severity.eq_ignore_ascii_case("serious"))
}

/// `printer_wait` for a shared session (the live socket): never holds the session lock while
/// waiting, so the window and other callers keep working. Polls status briefly under the lock
/// and sleeps on the event log in between.
pub fn wait_unlocked(studio: &std::sync::Arc<std::sync::Mutex<Studio>>, args: Value, actor: crate::Actor) -> Result<ToolOutput, ToolError> {
    let i: WaitIn = crate::tools::parse_input("printer_wait", &crate::tools::schema_of::<WaitIn>(), crate::tools::object_args("printer_wait", args.clone())?)?;
    let timeout = Duration::from_secs(i.timeout_s.unwrap_or(600).clamp(1, 3600));
    let start = std::time::Instant::now();
    let log = studio.lock().map(|s| s.events.clone()).map_err(|_| ToolError::new("session_poisoned", "the live session crashed", "restart only3d"))?;
    loop {
        let out = studio.lock().map_err(|_| ToolError::new("session_poisoned", "the live session crashed", "restart only3d"))?.call("printer_status", json!({"name": i.name}), actor)?;
        let st: PrinterStatus = serde_json::from_value(out.data.clone()).unwrap_or_default();
        let waited = start.elapsed().as_secs();
        if wait_met(&i, &st) {
            return Ok(ToolOutput::text(format!("After {waited}s: {}", out.text)).with_data(out.data));
        }
        if start.elapsed() >= timeout {
            return Ok(ToolOutput::text(format!("Still waiting after {waited}s (condition not met). {}", out.text)).with_data(json!({"timed_out": true, "status": out.data})));
        }
        let seq = log.last_seq();
        let kinds = ["printer_state", "printer_stage", "printer_nozzle", "printer_alert", "print_progress", "print_finished", "print_failed", "printer_connection"].map(String::from);
        log.wait(seq, &kinds, Duration::from_secs(5).min(timeout.saturating_sub(start.elapsed())));
    }
}

fn t_wait(s: &mut Studio, i: WaitIn) -> Result<ToolOutput, ToolError> {
    let ps = session(s, i.name.as_deref())?;
    let timeout = Duration::from_secs(i.timeout_s.unwrap_or(600).clamp(1, 3600));
    let start = std::time::Instant::now();
    let r = ps.client.wait_for(|st| wait_met(&i, st), timeout);
    let st = ps.client.status();
    let waited = start.elapsed().as_secs();
    match r {
        Ok(_) => Ok(ToolOutput::text(format!("After {waited}s: {}", status_text(&ps.cfg.name, &st))).with_data(serde_json::to_value(&st).unwrap_or(Value::Null))),
        Err(e) if e.code() == "timeout" => Ok(ToolOutput::text(format!("Still waiting after {waited}s (condition not met). {}", status_text(&ps.cfg.name, &st))).with_data(json!({"timed_out": true, "status": st}))),
        Err(e) => Err(bambu_err(e)),
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct CameraIn {
    #[serde(default)]
    pub name: Option<String>,
}

fn t_camera(_s: &mut Studio, i: CameraIn) -> Result<ToolOutput, ToolError> {
    let (name, jpg) = camera_jpeg(i.name.as_deref(), None)?;
    Ok(ToolOutput::text(format!("Chamber camera snapshot from '{name}' ({} KB JPEG).", jpg.len() / 1024)).with_data(json!({"bytes": jpg.len()})).with_jpeg(jpg, "chamber camera"))
}

/// One JPEG frame from a printer's chamber camera: `(printer name, jpeg bytes)`. `width`
/// scales the frame down (RTSPS cameras only), e.g. for a live preview. Needs no session, so
/// a GUI can poll it from its own thread.
pub fn camera_jpeg(name: Option<&str>, width: Option<u32>) -> Result<(String, Vec<u8>), ToolError> {
    let cfg = pick(&load_book(), name)?;
    // 1) X1/H2D: RTSPS on 322 (needs "LAN Only Liveview" on the printer), one frame via ffmpeg
    let rtsp_open = std::net::TcpStream::connect_timeout(&format!("{}:322", cfg.host).parse().map_err(|_| ToolError::new("bad_host", "printer host is not an IP", ""))?, Duration::from_secs(2)).is_ok();
    let jpg = if rtsp_open {
        let exe = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
        let on_path = || std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(exe)).find(|f| f.is_file()));
        let ffmpeg = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"].into_iter().map(PathBuf::from).find(|p| p.exists()).or_else(on_path).ok_or_else(|| ToolError::new("ffmpeg_missing", "the H2D camera is an RTSPS H.264 stream and decoding it needs ffmpeg", "brew install ffmpeg"))?;
        // The printer serves one RTSPS viewer at a time: grabs from every only3d process
        // (GUI live view, CLI, MCP) take turns through a lock file.
        let lock_path = crate::config_dir().join("camera.lock");
        let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&lock_path).map_err(|e| ToolError::new("camera_failed", format!("camera lock {}: {e}", lock_path.display()), ""))?;
        let waited = std::time::Instant::now();
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if waited.elapsed() < Duration::from_secs(20) => std::thread::sleep(Duration::from_millis(100)),
                Err(_) => return Err(ToolError::new("camera_busy", "another only3d process has been using the printer camera for 20 s", "try again in a moment")),
            }
        }
        let mut child = only3d_model::proc::command(ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-rtsp_transport", "tcp", "-tls_verify", "0", "-timeout", "8000000", "-i"])
            .arg(format!("rtsps://bblp:{}@{}:322/streaming/live/1", cfg.access_code, cfg.host))
            .args(width.map(|w| vec!["-vf".to_string(), format!("scale={w}:-2")]).unwrap_or_default())
            .args(["-frames:v", "1", "-f", "image2", "-c:v", "mjpeg", "-q:v", "3", "pipe:1"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| ToolError::new("ffmpeg_failed", e.to_string(), ""))?;
        let (mut out, mut err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
        let reader = std::thread::spawn(move || {
            let (mut o, mut e) = (Vec::new(), Vec::new());
            let _ = std::io::Read::read_to_end(&mut out, &mut o);
            let _ = std::io::Read::read_to_end(&mut err, &mut e);
            (o, e)
        });
        // a hard deadline: a stalled stream must never hang the caller (or the GUI's live view)
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let status = loop {
            match child.try_wait() {
                Ok(Some(st)) => break Some(st),
                Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let (stdout, stderr) = reader.join().unwrap_or_default();
        drop(lock);
        let Some(status) = status else {
            return Err(ToolError::new("camera_timeout", "the chamber camera sent no frame within 15 s", "check that the camera is on and LAN Only Liveview is enabled; the printer may be busy with another viewer (Bambu Studio, Handy)"));
        };
        if !status.success() || stdout.len() < 100 {
            return Err(ToolError::new("camera_failed", String::from_utf8_lossy(&stderr).replace(&cfg.access_code, "********").lines().take(3).collect::<Vec<_>>().join(" | "), "check that the chamber camera is on"));
        }
        stdout
    } else {
        // 2) P1/A1-style JPEG stream on 6000
        only3d_bambu::camera::snapshot(&cfg, Duration::from_secs(12)).map_err(|e| {
            let mut t = bambu_err(e);
            t.code = "camera_unavailable".into();
            t.hint = "On the H2D, turn on Settings → Network → \"LAN Only Liveview\" (opens the RTSPS camera stream on port 322), and keep the chamber camera enabled.".into();
            t
        })?
    };
    Ok((cfg.name.clone(), jpg))
}

#[derive(Deserialize, JsonSchema)]
pub struct RawIn {
    #[serde(default)]
    pub name: Option<String>,
    /// Only this top-level section of the report, e.g. "print" (default: everything).
    #[serde(default)]
    pub section: Option<String>,
}

fn t_raw(s: &mut Studio, i: RawIn) -> Result<ToolOutput, ToolError> {
    let ps = session(s, i.name.as_deref())?;
    let raw = ps.client.raw_state();
    let v = match &i.section {
        Some(sec) => raw.get(sec).cloned().unwrap_or(Value::Null),
        None => raw,
    };
    let text = serde_json::to_string(&v).unwrap_or_default();
    let shown: String = text.chars().take(20000).collect();
    Ok(ToolOutput::text(format!("Raw printer state ({} bytes){}:\n{shown}", text.len(), if text.len() > 20000 { ", truncated in text; full JSON in data" } else { "" })).with_data(v))
}

#[derive(Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Speed {
    Silent,
    Standard,
    Sport,
    Ludicrous,
}

#[derive(Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Left,
    Right,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetIn {
    #[serde(default)]
    pub name: Option<String>,
    /// Speed profile (fine mid-print).
    #[serde(default)]
    pub speed: Option<Speed>,
    /// Chamber light.
    #[serde(default)]
    pub light: Option<bool>,
    /// Bed target °C (0 = off).
    #[serde(default)]
    pub bed_temp: Option<u16>,
    /// Nozzle target °C (0 = off) for `nozzle` (default: the active one).
    #[serde(default)]
    pub nozzle_temp: Option<u16>,
    #[serde(default)]
    pub nozzle: Option<Side>,
    /// Fan speeds in percent.
    #[serde(default)]
    pub part_fan: Option<u8>,
    #[serde(default)]
    pub aux_fan: Option<u8>,
    #[serde(default)]
    pub chamber_fan: Option<u8>,
    /// Needed for temperatures or fans mid-print.
    #[serde(default)]
    pub confirm: bool,
}

fn t_set(s: &mut Studio, i: SetIn) -> Result<ToolOutput, ToolError> {
    let ps = session(s, i.name.as_deref())?;
    let busy = ps.client.status().gcode_state.is_busy();
    let thermal = i.bed_temp.is_some() || i.nozzle_temp.is_some() || i.part_fan.is_some() || i.aux_fan.is_some() || i.chamber_fan.is_some();
    if busy && thermal && !i.confirm {
        return Err(ToolError::new("confirm_required", "a print is running: changing temperatures or fans now affects it", "ask the user, then repeat with confirm: true"));
    }
    let mut done = Vec::new();
    // wait briefly for the printer's answer so rejections are reported, not silently lost
    let send = |cmd: Command, what: String, done: &mut Vec<String>| -> Result<(), ToolError> {
        let verdict = match ps.client.request(&cmd, Duration::from_secs(4)) {
            Ok(reply) => {
                let body = reply.as_object().and_then(|o| o.values().next()).cloned().unwrap_or(Value::Null);
                let result = body.get("result").and_then(|r| r.as_str()).unwrap_or("").to_string();
                let reason = body.get("reason").and_then(|r| r.as_str()).or_else(|| body.get("err_code").and_then(|r| r.as_str())).unwrap_or("").to_string();
                let err = body.get("err_code").and_then(|e| e.as_i64()).filter(|c| *c != 0);
                if result.eq_ignore_ascii_case("fail") || result.eq_ignore_ascii_case("failed") || err.is_some() {
                    format!("REJECTED by the printer ({}{})", if reason.is_empty() { result } else { reason }, err.map(|c| format!(", code {c}")).unwrap_or_default())
                } else if result.is_empty() {
                    "acknowledged".into()
                } else {
                    result.to_ascii_lowercase()
                }
            }
            Err(e) if e.code() == "timeout" => "sent (no reply)".into(),
            Err(e) => return Err(bambu_err(e)),
        };
        done.push(format!("{what}: {verdict}"));
        Ok(())
    };
    if let Some(sp) = i.speed {
        let lvl = match sp {
            Speed::Silent => only3d_bambu::SpeedLevel::Silent,
            Speed::Standard => only3d_bambu::SpeedLevel::Standard,
            Speed::Sport => only3d_bambu::SpeedLevel::Sport,
            Speed::Ludicrous => only3d_bambu::SpeedLevel::Ludicrous,
        };
        send(Command::PrintSpeed(lvl), format!("speed {sp:?}"), &mut done)?;
    }
    if let Some(on) = i.light {
        send(Command::Light { node: LightNode::ChamberLight, on }, format!("light {}", if on { "on" } else { "off" }), &mut done)?;
        // the H2D family has a second chamber light; switch it together, like Bambu's apps
        if ps.client.status().lights.iter().any(|l| l.node == "chamber_light2") {
            ps.client.send(&Command::Light { node: LightNode::ChamberLight2, on }).map_err(bambu_err)?;
        }
    }
    if let Some(t) = i.bed_temp {
        send(Command::SetBedTemp(t.min(only3d_bambu::commands::MAX_BED_C)), format!("bed {t}°C"), &mut done)?;
    }
    if let Some(t) = i.nozzle_temp {
        // firmware numbering on the H2D: T0 = right, T1 = left
        let ext = i.nozzle.map(|n| if n == Side::Right { 0u8 } else { 1u8 });
        send(Command::SetNozzleTemp { celsius: t.min(only3d_bambu::commands::MAX_NOZZLE_C), extruder: ext }, format!("nozzle {t}°C{}", i.nozzle.map(|n| format!(" ({n:?})")).unwrap_or_default()), &mut done)?;
    }
    for (pct, fan, label) in [(i.part_fan, 1, "part fan"), (i.aux_fan, 2, "aux fan"), (i.chamber_fan, 3, "chamber fan")] {
        if let Some(pct) = pct {
            let pwm = (pct.min(100) as u32 * 255 / 100) as u16;
            send(Command::GcodeLine(format!("M106 P{fan} S{pwm}\n")), format!("{label} {pct}%"), &mut done)?;
        }
    }
    if done.is_empty() {
        return Err(ToolError::new("nothing_to_set", "no setting given", "pass speed, light, bed_temp, nozzle_temp or a fan"));
    }
    if done.iter().all(|d| d.contains(SIGNED_COMMANDS_CODE)) {
        return Err(ToolError::new("printer_auth_required", format!("the printer rejected: {}", done.join(", ")), SIGNED_COMMANDS_HINT));
    }
    std::thread::sleep(Duration::from_millis(1500));
    let st = ps.client.status();
    let mut text = format!("Sent: {}.\n{}", done.join(", "), status_text(&ps.cfg.name, &st));
    if done.iter().any(|d| d.contains(SIGNED_COMMANDS_CODE)) {
        text.push_str(&format!("\nNote: {SIGNED_COMMANDS_HINT}"));
    }
    Ok(ToolOutput::text(text))
}

#[derive(Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AmsAction {
    /// Tell the printer what is in a slot (type, colour, temperature range), like editing a slot in Bambu Studio.
    SetSlot,
    /// Load the filament of a slot into the nozzle (idle printer only).
    Load,
    /// Unload the current filament (idle printer only).
    Unload,
}

#[derive(Deserialize, JsonSchema)]
pub struct AmsIn {
    #[serde(default)]
    pub name: Option<String>,
    pub action: AmsAction,
    /// AMS unit (0 = first). 254/255 = external spool (left/right on H2D).
    #[serde(default)]
    pub ams_id: u32,
    /// Slot 0..3 within the unit.
    #[serde(default)]
    pub slot: u32,
    /// set_slot: "PLA", "PETG", "ASA"...
    #[serde(default)]
    pub filament_type: Option<String>,
    /// set_slot: "#RRGGBB"
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub min_temp: Option<u16>,
    #[serde(default)]
    pub max_temp: Option<u16>,
    /// Bambu filament id (e.g. GFA00 PLA Basic, GFG02 PETG HF); "" for generic.
    #[serde(default)]
    pub tray_info_idx: Option<String>,
}

fn t_ams(s: &mut Studio, i: AmsIn) -> Result<ToolOutput, ToolError> {
    let ps = session(s, i.name.as_deref())?;
    let st = ps.client.status();
    // Payloads follow OpenBambuAPI mqtt.md (ams_filament_setting / ams_change_filament).
    let payload = match i.action {
        AmsAction::SetSlot => {
            let ty = i.filament_type.clone().ok_or_else(|| ToolError::new("bad_input", "set_slot needs filament_type", ""))?.to_ascii_uppercase();
            let color = i.color.clone().unwrap_or_else(|| "#FFFFFF".into()).trim_start_matches('#').to_ascii_uppercase();
            let (lo, hi) = (i.min_temp.unwrap_or(190), i.max_temp.unwrap_or(250));
            json!({"print": {"command": "ams_filament_setting", "ams_id": i.ams_id, "tray_id": i.slot, "slot_id": i.slot,
                "tray_info_idx": i.tray_info_idx.clone().unwrap_or_default(), "tray_color": format!("{color}FF"),
                "nozzle_temp_min": lo, "nozzle_temp_max": hi, "tray_type": ty}})
        }
        AmsAction::Load | AmsAction::Unload => {
            if st.gcode_state.is_busy() {
                return Err(ToolError::new("printer_busy", "the printer is printing; loading/unloading would interrupt it", "wait until it is idle (printer_wait)"));
            }
            // addressed the way Bambu Studio does (only3d_bambu::recovery, checked against bambuddy)
            let tray = if i.ams_id >= 128 { i.ams_id } else { i.ams_id * 4 + i.slot };
            if i.action == AmsAction::Load {
                let right = st.nozzles.iter().find(|n| n.side.as_deref() == Some("right")).and_then(|n| n.temperature.current);
                only3d_bambu::recovery::ams_load_payload(tray, right)
            } else {
                let active = st.nozzles.iter().find(|n| n.active).and_then(|n| n.temperature.current);
                only3d_bambu::recovery::ams_unload_payload(Some(tray), active)
            }
        }
    };
    let reply = ps.client.request(&Command::Raw(payload.clone()), Duration::from_secs(8));
    let ack = match &reply {
        Ok(v) => format!("printer replied: {}", v.get("print").and_then(|p| p.get("result")).and_then(|r| r.as_str()).unwrap_or("ok")),
        Err(e) if e.code() == "timeout" => "sent (no explicit reply)".to_string(),
        Err(e) => return Err(ToolError::new(e.code(), e.to_string(), e.hint())),
    };
    Ok(ToolOutput::text(format!("AMS {:?} on {} slot {}: {ack}.", i.action, if i.ams_id >= 254 { "external spool".to_string() } else { format!("AMS {}", i.ams_id) }, i.slot + 1)).with_data(json!({"payload": payload})))
}

#[derive(Deserialize, JsonSchema)]
pub struct AlertIn {
    #[serde(default)]
    pub name: Option<String>,
    /// list (default) | resume | stop | ignore | dont_remind | ok | ams_done | ams_retry | ams_abort
    #[serde(default)]
    pub action: Option<String>,
}

fn t_alert(s: &mut Studio, i: AlertIn) -> Result<ToolOutput, ToolError> {
    let ps = session(s, i.name.as_deref())?;
    let st = ps.client.status();
    let raw = ps.client.raw_state();
    let dev: String = ps.cfg.serial.chars().take(3).collect();
    let job_id = raw.pointer("/print/job_id").map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).unwrap_or_default();
    // the alert that pauses the job is `print_error`; HMS entries are listed with it
    let mut alerts: Vec<Value> = Vec::new();
    if let Some(pe) = &st.print_error {
        let hex = pe.id.replace('_', "");
        let offered = only3d_bambu::recovery::offered_actions(&dev, &hex);
        let text = hms_text(&pe.id).or_else(|| print_error_text(&dev, &hex));
        alerts.push(json!({"kind": "print_error", "code": pe.id, "text": text,
            "actions": offered.as_ref().map(|o| o.iter().map(|(a, label)| json!({"action": a.name(), "label": label})).collect::<Vec<_>>())}));
    }
    for h in &st.hms {
        alerts.push(json!({"kind": "hms", "code": h.id, "severity": h.severity, "module": h.module, "text": hms_text(&h.id)}));
    }
    let action = i.action.as_deref().unwrap_or("list");
    if action == "list" {
        let text = if alerts.is_empty() {
            format!("{}: no alerts ({}).", ps.cfg.name, st.gcode_state)
        } else {
            alerts
                .iter()
                .map(|a| {
                    let acts = a["actions"].as_array().map(|x| x.iter().map(|y| format!("{} (\"{}\")", y["action"].as_str().unwrap_or(""), y["label"].as_str().unwrap_or(""))).collect::<Vec<_>>().join(", "));
                    format!("{} {}: {}{}", a["kind"].as_str().unwrap_or(""), a["code"].as_str().unwrap_or(""), a["text"].as_str().unwrap_or("(no description)"), acts.map(|x| format!("\n  actions: {x}")).unwrap_or_default())
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        return Ok(ToolOutput::text(text).with_data(json!({"alerts": alerts, "state": st.gcode_state.as_str()})));
    }
    let a = only3d_bambu::recovery::AlertAction::parse(action).ok_or_else(|| ToolError::new("bad_input", format!("unknown alert action '{action}'"), "use list to see the actions the current alert offers"))?;
    let Some(pe) = &st.print_error else {
        return Err(ToolError::new("no_alert", format!("{} has no active alert to answer", ps.cfg.name), "printer_alert list shows alerts; to resume an ordinary pause use printer_control resume"));
    };
    let hex = pe.id.replace('_', "");
    if let Some(offered) = only3d_bambu::recovery::offered_actions(&dev, &hex) {
        if !offered.iter().any(|(x, _)| *x == a) {
            let names: Vec<&str> = offered.iter().map(|(x, _)| x.name()).collect();
            return Err(ToolError::new("action_not_offered", format!("Bambu offers {} for alert {}, not {action}", if names.is_empty() { "no printer action".to_string() } else { names.join(", ") }, pe.id), "pick one of those, or ask the user"));
        }
    }
    let payload = only3d_bambu::recovery::alert_payload(a, &hex, &job_id);
    let reply = ps.client.request(&Command::Raw(payload.clone()), Duration::from_secs(6));
    let ack = match &reply {
        Ok(v) => v.pointer("/print/result").and_then(Value::as_str).unwrap_or("ok").to_string(),
        Err(e) if e.code() == "timeout" => "sent (no explicit reply)".into(),
        Err(e) => return Err(ToolError::new(e.code(), e.to_string(), e.hint())),
    };
    Ok(ToolOutput::text(format!("{}: {action} on alert {}: {ack}.", ps.cfg.name, pe.id)).with_data(json!({"payload": payload, "reply": ack})))
}

/// Text of a `print_error` code from Bambu Studio's per-model HMS file (device_error list).
fn print_error_text(dev: &str, hex: &str) -> Option<String> {
    for p in [only3d_bambu::bambu_studio_dir().join(format!("hms/hms_en_{dev}.json")), PathBuf::from(format!("/Applications/BambuStudio.app/Contents/Resources/hms/hms_en_{dev}.json"))] {
        let Ok(t) = std::fs::read_to_string(p) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
        if let Some(list) = v.pointer("/data/device_error/en").and_then(Value::as_array) {
            if let Some(e) = list.iter().find(|e| e.get("ecode").and_then(Value::as_str).is_some_and(|c| c.eq_ignore_ascii_case(hex))) {
                return e.get("intro").and_then(Value::as_str).map(str::to_string);
            }
        }
    }
    None
}

#[derive(Deserialize, JsonSchema)]
pub struct NozzleIn {
    #[serde(default)]
    pub name: Option<String>,
    /// "left" or "right": the nozzle to make active (dual-nozzle printers, idle only).
    pub side: String,
}

fn t_nozzle(s: &mut Studio, i: NozzleIn) -> Result<ToolOutput, ToolError> {
    let ps = session(s, i.name.as_deref())?;
    let st = ps.client.status();
    if st.nozzles.len() < 2 {
        return Err(ToolError::new("not_dual_nozzle", "this printer has one nozzle", ""));
    }
    if st.gcode_state.is_busy() {
        return Err(ToolError::new("printer_busy", "switching nozzles mid-print would ruin the job", "wait until it is idle (printer_wait)"));
    }
    let right = match i.side.to_ascii_lowercase().as_str() {
        "right" => true,
        "left" => false,
        other => return Err(ToolError::new("bad_input", format!("side must be left or right, not {other}"), "")),
    };
    let payload = only3d_bambu::recovery::select_extruder_payload(right);
    let reply = ps.client.request(&Command::Raw(payload.clone()), Duration::from_secs(8));
    let ack = match &reply {
        Ok(v) => v.pointer("/print/result").and_then(Value::as_str).unwrap_or("ok").to_string(),
        Err(e) if e.code() == "timeout" => "sent (no explicit reply)".into(),
        Err(e) => return Err(ToolError::new(e.code(), e.to_string(), e.hint())),
    };
    Ok(ToolOutput::text(format!("{}: switching to the {} nozzle: {ack}.", ps.cfg.name, i.side)).with_data(json!({"payload": payload})))
}

#[derive(Deserialize, JsonSchema)]
pub struct FileOpIn {
    #[serde(default)]
    pub name: Option<String>,
    /// "download" or "delete"
    pub action: String,
    /// Path on the printer, e.g. "/timelapse/video_2026-11-15.mp4".
    pub path: String,
    /// download: local destination file.
    #[serde(default)]
    pub save_to: Option<String>,
    /// delete: must be true.
    #[serde(default)]
    pub confirm: bool,
}

fn t_file_op(_s: &mut Studio, i: FileOpIn) -> Result<ToolOutput, ToolError> {
    let cfg = pick(&load_book(), i.name.as_deref())?;
    let mut ftp = only3d_bambu::FtpsClient::connect(&cfg).map_err(bambu_err)?;
    let out = match i.action.as_str() {
        "download" => {
            let bytes = ftp.retrieve(&i.path).map_err(bambu_err)?;
            let dest = PathBuf::from(i.save_to.clone().unwrap_or_else(|| i.path.rsplit('/').next().unwrap_or("download").to_string()));
            std::fs::write(&dest, &bytes).map_err(|e| ToolError::new("io_error", e.to_string(), ""))?;
            ToolOutput::text(format!("Downloaded {} ({} KB) to {}", i.path, bytes.len() / 1024, dest.display()))
        }
        "delete" => {
            if !i.confirm {
                return Err(ToolError::new("confirm_required", format!("deleting {} cannot be undone", i.path), "ask the user, then repeat with confirm: true"));
            }
            ftp.delete(&i.path).map_err(bambu_err)?;
            ToolOutput::text(format!("Deleted {}", i.path))
        }
        other => return Err(ToolError::new("bad_action", format!("unknown action '{other}'"), "download | delete")),
    };
    let _ = ftp.quit();
    Ok(out)
}

#[derive(Deserialize, JsonSchema)]
pub struct SkipIn {
    #[serde(default)]
    pub name: Option<String>,
    /// Object ids of the running job to stop printing (the `identify_id`s in its slice_info).
    pub object_ids: Vec<i64>,
    pub confirm: bool,
}

fn t_skip(s: &mut Studio, i: SkipIn) -> Result<ToolOutput, ToolError> {
    if !i.confirm {
        return Err(ToolError::new("confirm_required", "skipping objects of a running print cannot be undone", "ask the user, then repeat with confirm: true"));
    }
    let ps = session(s, i.name.as_deref())?;
    let payload = json!({"print": {"command": "skip_objects", "obj_list": i.object_ids}});
    ps.client.send(&Command::Raw(payload)).map_err(bambu_err)?;
    Ok(ToolOutput::text(format!("Asked the printer to skip objects {:?}.", i.object_ids)))
}

fn s_name(s: &Studio) -> String {
    if s.project.name.is_empty() || s.project.name == "untitled" {
        s.project.objects.first().map(|o| o.name.clone()).unwrap_or_else(|| "only3d".into())
    } else {
        s.project.name.clone()
    }
}

fn s_bed(sl: &crate::SliceOutput) -> String {
    sl.result.job.bed_type.clone()
}

fn sanitize(n: &str) -> String {
    let s: String = n.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    s.chars().take(48).collect()
}

pub fn register(r: &mut Registry) {
    r.add(def::<DiscoverIn>("printer_discover", "Discover printers", "Find Bambu printers on the LAN (no credentials): IP, serial, model, LAN/cloud mode.", false, true, t_discover));
    r.add(def::<AddPrinterIn>("printer_add", "Add printer", "Save a printer (~/.only3d/printers.json, mode 600): a Bambu's IP, serial and access code, or another maker's URL, API kind and key.", false, true, t_add));
    r.add(def::<crate::tools::Empty>("printer_list", "List printers", "Printers configured in only3d.", false, false, t_list));
    r.add(def::<NameIn>("printer_status", "Printer status", "Live status: state, progress, layer, time left, temperatures, AMS trays, errors.", false, true, t_status));
    r.add(def::<FilesIn>("printer_files", "Printer files", "List files on the printer's storage (FTPS), optionally in a folder.", false, true, t_files));
    r.add(def::<ControlIn>("printer_control", "Printer control", "Pause, resume or stop the current print, or switch the chamber light. stop needs confirm: true.", false, true, t_control));
    r.add(def::<CameraIn>("printer_camera", "Camera snapshot", "A chamber camera picture (JPEG) to look at the print: first layer, spaghetti, finished parts.", false, true, t_camera));
    r.add(def::<RawIn>("printer_raw", "Raw printer state", "Everything the printer reports over MQTT, merged, optionally one section.", false, true, t_raw));
    r.add(def::<SetIn>("printer_set", "Printer controls", "Speed, light, bed/nozzle temperatures, part/aux/chamber fans. Heating, and temps/fans mid-print, ask the user.", false, true, t_set));
    r.add(def::<AmsIn>("printer_ams", "AMS", "AMS slots: tell the printer what filament is in a slot (set_slot), or load/unload filament when the printer is idle.", false, true, t_ams));
    r.add(def::<FileOpIn>("printer_file", "Printer file", "Download a file from the printer (timelapses, jobs) or delete one (delete needs confirm: true).", false, true, t_file_op));
    r.add(def::<SkipIn>("printer_skip_objects", "Skip objects", "Stop printing some objects of the running job (e.g. one that came loose). Needs confirm: true.", false, true, t_skip));
    r.add(def::<AlertIn>("printer_alert", "Printer alert", "The printer's alerts with the actions Bambu offers for each, or perform one (the user approves it in only3d).", false, true, t_alert));
    r.add(def::<NozzleIn>("printer_nozzle", "Switch nozzle", "Make the left or right nozzle active on a dual-nozzle printer (idle only). Approved by the user in only3d.", false, true, t_nozzle));
    r.add(def::<WaitIn>("printer_wait", "Wait for printer", "Block until the print reaches a state (FINISH, FAILED, PAUSE...), a layer or a percentage, or reports an error; returns the status.", false, true, t_wait));
    r.add(def::<PrintIn>("print", "Print", "Preflight, upload and start the current slice. Without confirm: the checklist and a plate photo. With confirm: true the user approves in only3d first. Only when the user wants to print.", false, true, t_print));
}

#[cfg(test)]
mod tests {
    use super::*;
    use only3d_bambu::{AmsTray, AmsUnit, NozzleStatus};

    fn h2d(ams_nozzle: u8) -> PrinterStatus {
        PrinterStatus {
            nozzles: vec![NozzleStatus::default(), NozzleStatus::default()],
            ams: vec![AmsUnit { id: 0, extruder: Some(ams_nozzle), trays: (0..4).map(|k| AmsTray { global_id: k, ..Default::default() }).collect(), ..Default::default() }],
            ..Default::default()
        }
    }

    #[test]
    fn each_filament_finds_its_loaded_tray() {
        let tray = |slot: u32, ty: &str, color: &str| AmsTray { slot_id: slot, global_id: slot, loaded: true, filament_type: Some(ty.into()), color: Some(color.into()), ..Default::default() };
        let mut st = h2d(0);
        st.ams[0].trays = vec![tray(0, "PLA", "000000FF"), tray(1, "PETG", "FFFFFFFF"), tray(2, "PETG", "F9C1BDFF")];
        st.external_spools = vec![AmsTray { ams_id: 254, global_id: 254, ..tray(0, "PETG", "F2F0EAFF") }];
        let trays = crate::trays::trays(&st, &[]);
        let (pink, white) = (("Bambu PETG Translucent @BBL H2D 0.4 nozzle", "#F9C1BD"), ("Bambu PETG Basic @BBL H2D 0.4 nozzle", "#FFFFFF"));
        // same material, nearest colour
        assert_eq!(crate::trays::best(&trays, pink.0, pink.1, &[]), Some(2));
        assert_eq!(crate::trays::best(&trays, white.0, white.1, &[]), Some(1));
        // a tray already given to another filament is skipped
        assert_eq!(crate::trays::best(&trays, white.0, white.1, &[1]), Some(254));
        // no PETG at all: nothing
        assert_eq!(crate::trays::best(&trays[..1], white.0, white.1, &[]), None);
    }

    #[test]
    fn status_names_trays_as_the_preflight_does() {
        let mut st = h2d(0);
        st.ams[0].trays = (0..4).map(|k| AmsTray { slot_id: k, global_id: k, ..Default::default() }).collect();
        st.ams[0].trays[0] = AmsTray { loaded: true, filament_type: Some("PLA".into()), sub_brand: Some("PLA Wood".into()), color: Some("995F11FF".into()), remain_percent: Some(73), ..st.ams[0].trays[0].clone() };
        st.external_spools = vec![AmsTray { ams_id: 254, global_id: 254, loaded: true, filament_type: Some("PETG".into()), color: Some("FFFFFFFF".into()), ..Default::default() }];
        let t = status_text("h2d", &st);
        assert!(t.contains("AMS A → right nozzle: A1 PLA Wood #995F11 73%, A2 empty, A3 empty, A4 empty"), "{t}");
        assert!(t.contains("External spool: ext left PETG #FFFFFF"), "{t}");
    }

    #[test]
    fn trays_map_to_the_nozzle_they_feed() {
        // AMS on firmware nozzle 0 (right) -> logical 1; on firmware 1 (left) -> logical 0
        assert_eq!(tray_extruder(&h2d(0), 2), Some(1));
        assert_eq!(tray_extruder(&h2d(1), 2), Some(0));
        // external holders: 255 main (right), 254 deputy (left)
        assert_eq!(tray_extruder(&h2d(0), 255), Some(1));
        assert_eq!(tray_extruder(&h2d(0), 254), Some(0));
        assert_eq!(tray_extruder(&h2d(0), 9), None);
        // single-nozzle printers have nothing to choose
        let single = PrinterStatus { nozzles: vec![NozzleStatus::default()], ..h2d(0) };
        assert_eq!(tray_extruder(&single, 2), None);
    }

    #[test]
    fn a_plate_sliced_for_another_model_is_refused() {
        assert!(model_check(Some("Bambu Lab H2D"), Some("Bambu Lab H2D")).unwrap().0);
        let (ok, why) = model_check(Some("Bambu Lab H2D"), Some("Bambu Lab P1S")).unwrap();
        assert!(!ok && why.contains("sliced for a Bambu Lab H2D") && why.contains("\"P1S\""), "{why}");
        assert!(model_check(Some("Bambu Lab A1"), None).unwrap().0, "a printer that doesn't say its model isn't refused");
        assert!(model_check(None, Some("Bambu Lab A1")).is_none());
        assert!(only3d_bambu::models::MODELS.iter().all(|m| crate::tested::find(m.name).is_some()), "every model the LAN link knows is in profiles/printers.json");
    }
}
