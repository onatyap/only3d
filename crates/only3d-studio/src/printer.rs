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

pub(crate) fn bambu_err(e: only3d_bambu::Error) -> ToolError {
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

/// Connect to a printer; when its saved address doesn't answer, look for it (F-27: it moved to
/// another address and only3d lost it) by its serial or its pinned certificate, save the new
/// address and connect there.
fn connect_or_relocate(s: &Studio, mut cfg: PrinterConfig) -> Result<(PrinterConfig, PrinterClient), ToolError> {
    let err = match PrinterClient::connect(&cfg) {
        Ok(c) => return Ok((cfg, c)),
        Err(e) => e,
    };
    if !matches!(err.code(), "unreachable" | "timeout") || cfg!(test) || std::env::var_os("ONLY3D_NO_RELOCATE").is_some() {
        return Err(bambu_err(err));
    }
    let old = cfg.host.clone();
    let nets = only3d_bambu::nearby_subnets(old.parse().ok());
    let Some(ip) = only3d_bambu::relocate(&cfg.serial, cfg.cert_sha256.as_deref(), &nets, Duration::from_secs(4)).map(|ip| ip.to_string()).filter(|ip| *ip != old) else {
        let mut t = bambu_err(err);
        let looked = nets.iter().map(|n| format!("{n}/24")).collect::<Vec<_>>().join(", ");
        t.hint = format!("{} It isn't announcing itself, and no device in {looked} has its certificate. If it moved to another network, printer_add {{\"name\": \"{}\", \"host\": \"<new IP>\"}} changes only the address.", t.hint, cfg.name);
        return Err(t);
    };
    cfg.host = ip.clone();
    let client = PrinterClient::connect(&cfg).map_err(bambu_err)?;
    let mut book = load_book();
    if let Some(p) = book.printers.iter_mut().find(|p| p.serial == cfg.serial) {
        p.host = ip.clone();
        save_book(&book)?;
    }
    s.events.push("printer_connection", None, format!("{}: moved from {old} to {ip}; the new address is saved", cfg.name), json!({"printer": cfg.name, "from": old, "to": ip}));
    Ok((cfg, client))
}

/// Connect (or reuse the connection) to the named/default printer.
pub(crate) fn session<'a>(s: &'a mut Studio, name: Option<&str>) -> Result<&'a mut PrinterSession, ToolError> {
    let book = load_book();
    let cfg = pick(&book, name)?;
    let reuse = s.printer.as_ref().is_some_and(|p| p.cfg.serial == cfg.serial && p.client.is_connected());
    if !reuse {
        let (cfg, client) = connect_or_relocate(s, cfg)?;
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

/// Bambu Studio's local HMS database (`ecode` → text), from its data folder or the app bundle.
/// Empty when Bambu Studio isn't installed.
fn hms_db() -> &'static std::collections::HashMap<String, String> {
    static DB: std::sync::OnceLock<std::collections::HashMap<String, String>> = std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let mut m = std::collections::HashMap::new();
        let dirs = [only3d_bambu::bambu_studio_dir().join("hms"), PathBuf::from("/Applications/BambuStudio.app/Contents/Resources/hms")];
        for e in dirs.iter().flat_map(std::fs::read_dir).flatten().flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.starts_with("hms_en_") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(e.path()) else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
            fn walk(v: &Value, m: &mut std::collections::HashMap<String, String>) {
                match v {
                    Value::Object(o) => {
                        // some files list a code with an empty text (F-31): keep looking for one with text
                        if let (Some(c), Some(t)) = (o.get("ecode").and_then(|x| x.as_str()), o.get("intro").and_then(|x| x.as_str()).map(str::trim).filter(|t| !t.is_empty())) {
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
    })
}

/// Human description of an HMS or print-error code ("0500_0500_0001_0007", "0700_8011") from
/// Bambu Studio's local HMS database, when installed and when it has text for the code.
pub fn hms_text(id: &str) -> Option<String> {
    hms_db().get(&id.replace('_', "").to_ascii_uppercase()).cloned()
}

/// What to show for an alert code: its text, or why there is none (F-31: a code showed only
/// "common (mainboard)", as if that were its meaning).
pub fn alert_text(id: &str) -> String {
    describe_code(hms_db(), id)
}

fn describe_code(db: &std::collections::HashMap<String, String>, id: &str) -> String {
    match db.get(&id.replace('_', "").to_ascii_uppercase()) {
        Some(t) => t.clone(),
        None if db.is_empty() => "no description here (install Bambu Studio for the code texts, or look the code up on wiki.bambulab.com)".into(),
        None => "no description: Bambu's code list has no text for it (look it up on wiki.bambulab.com)".into(),
    }
}

/// "\n  Humidity 27%, 50 °C · drying PLA at 50 °C (dehumidifying), 10 h 37 min left of 12 h", or
/// "" when the unit reports none of it.
fn ams_unit_line(a: &only3d_bambu::AmsUnit) -> String {
    let climate: Vec<String> = [
        a.humidity_percent.map(|h| format!("humidity {h}%")).or_else(|| a.humidity_level.map(|l| format!("humidity level {l}/5"))),
        a.temperature.map(|c| format!("{c:.0} °C")),
    ]
    .into_iter()
    .flatten()
    .collect();
    let parts: Vec<String> = [(!climate.is_empty()).then(|| climate.join(", ")), a.drying.as_ref().map(|d| d.summary.clone()).filter(|s| !s.is_empty())].into_iter().flatten().collect();
    if parts.is_empty() {
        return String::new();
    }
    let line = parts.join(" · ");
    let mut c = line.chars();
    format!("\n  {}{}", c.next().map(|f| f.to_uppercase().to_string()).unwrap_or_default(), c.as_str())
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
            _ if tr.reading => "reading…".into(),
            Some(x) if x.loaded => [x.line.clone().or(x.material.clone()), x.color.clone(), x.percent.map(|p| format!("{p}%"))].into_iter().flatten().collect::<Vec<_>>().join(" "),
            _ => "empty".into(),
        };
        format!("{} {what}", crate::trays::label(st, tr))
    };
    for a in &st.ams {
        let side = a.trays.first().and_then(|tr| tray_extruder(st, tr.global_id as i32)).map(|e| format!(" → {} nozzle", side_name(e))).unwrap_or_default();
        let unit = Some(a.name.clone())
            .filter(|n| !n.is_empty())
            .or_else(|| a.trays.first().map(|tr| crate::trays::label(st, tr).trim_end_matches(char::is_numeric).to_string()))
            .unwrap_or_else(|| a.id.to_string());
        let model = a.model.as_deref().map(|m| format!(" ({m})")).unwrap_or_default();
        t.push_str(&format!("\nAMS {unit}{model}{side}: {}", a.trays.iter().map(holds).collect::<Vec<_>>().join(", ")));
        t.push_str(&ams_unit_line(a));
    }
    if let Some(s) = &st.ams_state {
        if let Some(act) = &s.activity {
            t.push_str(&format!("\nAMS is {act}"));
        }
        if s.read_rfid_on_insert == Some(false) {
            t.push_str("\nAMS setting \"Insertion update\" is off: a Bambu spool's RFID tag isn't read when it is inserted.");
        }
    }
    for e in &st.external_spools {
        if e.loaded {
            t.push_str(&format!("\nExternal spool: {}", holds(e)));
        }
    }
    if let Some(pe) = &st.print_error {
        t.push_str(&format!("\nPrint error {}: {}", pe.id, alert_text(&pe.id)));
    }
    if !st.hms.is_empty() {
        t.push_str(&format!(
            "\nHMS: {}",
            st.hms
                .iter()
                .map(|h| format!("{} {} ({}): {}", h.id, h.severity, h.module, alert_text(&h.id)))
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
    /// stop: keep the bed hot, so the part stays put.
    #[serde(default)]
    pub hold_bed: bool,
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
    /// The user's yes to a multi-filament print marked unverified.
    #[serde(default)]
    pub multi_filament_ok: bool,
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
    let mut b = load_book();
    // a printer saved before: what isn't given is kept, so a new address is all it takes (F-27)
    let saved = b.printers.iter().find(|p| p.name == i.name || i.serial.as_deref().is_some_and(|sn| p.serial == sn)).cloned();
    let serial = i.serial.clone().or_else(|| saved.as_ref().map(|p| p.serial.clone()));
    let code = i.access_code.clone().or_else(|| saved.as_ref().filter(|p| Some(&p.serial) == serial.as_ref()).map(|p| p.access_code.clone()));
    let (Some(serial), Some(code)) = (serial, code) else {
        return Err(ToolError::new("missing_field", "a Bambu printer needs its serial and LAN access code", "printer_discover finds the serial; the access code is on the printer's screen (Settings → Network)"));
    };
    let mut c = match saved.filter(|p| p.serial == serial && p.access_code == code) {
        // the same printer at a new address keeps its pinned certificate and model
        Some(mut p) => {
            p.host = i.host.clone();
            p.name = i.name.clone();
            p
        }
        None => PrinterConfig::new(i.host.clone(), serial, code),
    };
    c.name = i.name.clone();
    c.validate().map_err(bambu_err)?;
    let moved = b.printers.iter().find(|p| p.serial == c.serial && p.host != c.host).map(|p| p.host.clone());
    b.printers.retain(|p| p.name != c.name && p.serial != c.serial);
    b.printers.push(c);
    if i.make_default.unwrap_or(true) || b.default.is_none() {
        b.default = Some(i.name.clone());
    }
    save_book(&b)?;
    // a live connection to the old address is dropped, so the next call uses the new one
    if moved.is_some() {
        s.printer = None;
    }
    let what = moved.map(|old| format!("Moved printer '{}' from {old} to {}", i.name, i.host)).unwrap_or_else(|| format!("Saved printer '{}'", i.name));
    Ok(ToolOutput::text(format!("{what} in {} (readable only by you). Next: printer_status.", book_path().display())))
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

#[derive(Deserialize, JsonSchema)]
pub struct StatusIn {
    #[serde(default)]
    pub name: Option<String>,
    /// Key facts only (~0.5 KB, not ~8 KB): for polling.
    #[serde(default)]
    pub brief: bool,
}

fn t_status(s: &mut Studio, i: StatusIn) -> Result<ToolOutput, ToolError> {
    if i.brief && pick_host(&load_book(), i.name.as_deref()).is_none() {
        let ps = session(s, i.name.as_deref())?;
        let (text, data) = brief_status(&ps.cfg.name, &ps.client.status());
        return Ok(ToolOutput::text(text).with_data(data));
    }
    status(s, i.name.as_deref())
}

/// `printer_status {brief: true}` (F-35): the first line of the full status, temperatures, the
/// active nozzle's tray and the alerts, without the trays, fans, air and AMS detail.
pub fn brief_status(name: &str, st: &PrinterStatus) -> (String, Value) {
    let full = status_text(name, st);
    let mut t = full.lines().next().unwrap_or("").to_string();
    let nozzles: Vec<String> = st
        .nozzles
        .iter()
        .map(|n| {
            let tray = n.loaded_tray.map(|r| crate::spools::location_name(&crate::spools::tray_location(r.ams_id, r.slot_id)));
            format!("{} {}{}{}", n.side.clone().unwrap_or_else(|| format!("nozzle {}", n.index)), temp(&n.temperature), if n.active { " active" } else { "" }, tray.map(|x| format!(" ({x})")).unwrap_or_default())
        })
        .collect();
    t.push_str(&format!("
Bed {}; {}", temp(&st.bed), nozzles.join("; ")));
    if let Some(pe) = &st.print_error {
        t.push_str(&format!("
Print error {}: {}", pe.id, alert_text(&pe.id)));
    }
    for h in &st.hms {
        t.push_str(&format!("
HMS {} {}: {}", h.id, h.severity, alert_text(&h.id)));
    }
    let data = json!({
        "state": st.gcode_state.as_str(), "stage": st.stage_name, "progress_percent": st.progress_percent,
        "layer": st.layer, "total_layers": st.total_layers, "remaining_minutes": st.remaining_minutes, "job": st.subtask_name,
        "bed": st.bed, "nozzles": st.nozzles.iter().map(|n| json!({"index": n.index, "side": n.side, "temperature": n.temperature, "active": n.active, "loaded_tray": n.loaded_tray})).collect::<Vec<_>>(),
        "print_error": st.print_error, "hms": st.hms.iter().map(|h| json!({"id": h.id, "severity": h.severity})).collect::<Vec<_>>(),
    });
    (t, data)
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
    /// Timelapses, with N thumbnails.
    #[serde(default)]
    pub timelapses: Option<usize>,
}

fn t_files(s: &mut Studio, i: FilesIn) -> Result<ToolOutput, ToolError> {
    if let Some(thumbnails) = i.timelapses {
        return crate::timelapse::tool(i.name.as_deref(), thumbnails);
    }
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
    // F-43: what the bed was held at, and whether a part is left on it, before a stop
    let before = ps.client.status();
    let bed = before.bed.target.filter(|t| *t > 0.0).map(|t| t.round() as u16);
    let unfinished = before.gcode_state.is_busy() && before.progress_percent.unwrap_or(0) < 100;
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
    let mut bed_note = String::new();
    if let (Action::Stop, Some(t)) = (i.action, bed) {
        if i.hold_bed {
            // the stop turns the bed off; set it back so the part doesn't come loose
            ps.client.send(&Command::SetBedTemp(t)).map_err(bambu_err)?;
            bed_note = format!(" The bed stays at {t} °C, so the part stays put; turn it off (bed_temp 0) when you're done.");
        } else if unfinished {
            bed_note = format!(" The bed was at {t} °C and is cooling now. To keep the part on it (to continue it later), set the bed back to {t} °C soon: printer_set bed_temp {t}, or Printer controls.");
        }
    }
    let st = ps.client.status();
    Ok(ToolOutput::text(format!("Sent {:?}. {}{bed_note}", i.action, status_text(&ps.cfg.name, &st).lines().next().unwrap_or(""))).with_data(json!({"bed_was_c": bed, "bed_held": i.hold_bed && bed.is_some() && i.action == Action::Stop})))
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

/// The note for a plate with several filaments on `model` while that isn't verified there.
fn multi_note(multi: bool, model: Option<&str>) -> Option<String> {
    if !multi {
        return None;
    }
    crate::tested::multi_filament_note(model.unwrap_or("printer whose model isn't known"))
}

/// The preflight's lines for whoever approves: the untested printer and the unverified
/// multi-filament print, each on its own line.
fn approver_notes(untested: &Option<String>, multi: &Option<String>) -> String {
    let mut t = untested.as_ref().map(|n| format!("{n}\n")).unwrap_or_default();
    if let Some(n) = multi {
        t.push_str(&format!("[!] {n}\n"));
    }
    t
}

/// What the preflight asks for to start: confirm, and the extra yes for an unverified
/// multi-filament print.
fn start_args(multi: &Option<String>) -> &'static str {
    if multi.is_some() {
        "confirm: true and multi_filament_ok: true"
    } else {
        "confirm: true"
    }
}

fn multi_filament_unconfirmed(note: &str) -> ToolError {
    ToolError::new(
        "multi_filament_unverified",
        format!("not started: {note}"),
        "tell the user what this says; only if they still want to start it, call print again with confirm: true and multi_filament_ok: true",
    )
}

/// `print`'s preflight (without confirm) says the plate's multi-filament print isn't verified on
/// this printer, and `args` lacks the user's yes to it: the error, so the user isn't asked to
/// approve a start that would be refused.
pub fn multi_filament_unacked(pre: &ToolOutput, args: &Value) -> Option<ToolError> {
    let note = pre.data.get("multi_filament")?.get("note")?.as_str()?;
    (args.get("multi_filament_ok").and_then(Value::as_bool) != Some(true)).then(|| multi_filament_unconfirmed(note))
}

/// What this session last sent to a printer, so a repeated start of it is refused (F-32).
#[derive(Clone, Debug)]
pub struct Started {
    pub printer: String,
    /// [`crate::spools::gcode_id`] of the G-code sent.
    pub gcode: String,
    pub at: std::time::Instant,
    /// The printer's state and job just before it was sent: while they haven't changed, its
    /// status may simply not show the new job yet.
    pub before: (String, Option<String>),
    /// Its print record (P-0031), when it has one.
    pub record: Option<String>,
}

/// How long after a start the printer's status may still not show it.
const START_SETTLE: Duration = Duration::from_secs(300);

/// A print that would start a plate a second time (F-32): the answer to the first start can be
/// lost on its way back (the agent's connection closed), and repeating it must not print twice.
/// Refused when this session sent the same G-code to this printer a few minutes ago and the
/// printer is printing or hasn't moved on since, or when the printer is printing a job of this
/// plate's name. `now` is the printer's state and job; `job` the name this plate prints under.
fn already_started(last: Option<&Started>, printer: &str, gcode: &str, job: &str, now: (&str, Option<&str>), busy: bool) -> Option<ToolError> {
    let mine = last.filter(|l| l.printer == printer && l.gcode == gcode && l.at.elapsed() < START_SETTLE && (busy || (now.0, now.1) == (l.before.0.as_str(), l.before.1.as_deref())));
    let named = busy && now.1.is_some_and(|j| j == job || j.ends_with(&format!("/{job}")));
    let message = match (mine, named) {
        (Some(l), _) => {
            let secs = l.at.elapsed().as_secs();
            let ago = if secs < 60 { format!("{secs} s") } else { format!("{} min", secs / 60) };
            let record = l.record.as_ref().map(|r| format!(", recorded as {r}")).unwrap_or_default();
            let state = if busy { format!("the printer is {}", now.0) } else { "the printer may not show it yet".to_string() };
            format!("not started again: this plate was sent to '{printer}' {ago} ago{record}, and {state}")
        }
        (None, true) => format!("not started again: '{printer}' is {} '{}', the name this plate prints under", now.0, now.1.unwrap_or(job)),
        (None, false) => return None,
    };
    Some(ToolError::new(
        "already_started",
        message,
        "don't start it twice: follow it with printer_status (print_history has the record). Start another copy only after this one has ended, if the user asks for one",
    ))
}

/// `print` on another maker's printer: the same preflight (a current slice with the printer's
/// own start G-code, for this printer, a passing G-code check, printable parts, an idle printer)
/// and the same approval, then upload and start over its API.
fn host_print(s: &mut Studio, h: crate::hostprint::HostPrinter, i: PrintIn) -> Result<ToolOutput, ToolError> {
    if i.timelapse {
        return Err(ToolError::new("not_available", format!("'{}' is a {} printer: only3d records timelapses on Bambu printers only", h.name, h.kind.label()), "leave timelapse out, and turn one on from the printer's own web page"));
    }
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
    let used: std::collections::BTreeSet<u32> = objs.iter().filter(|o| !o.hidden).flat_map(|o| o.filaments()).collect();
    let file = format!("{}.gcode", sanitize(&s_name(s)));
    let gcode = crate::spools::gcode_id(&sl.result.gcode);
    let st = crate::hostprint::status(&h);
    match &st {
        Ok(st) => {
            // a start whose answer was lost, repeated (F-32)
            if let Some(e) = already_started(s.last_start.as_ref(), &h.name, &gcode, &file, (&st.state, st.file.as_deref()), !st.idle) {
                return Err(e);
            }
            checklist.push((st.idle, format!("printer state {}", st.state)))
        }
        Err(e) => checklist.push((false, format!("can't read the printer: {e}"))),
    }
    let model = s.machine.printer_model(&machine);
    let untested = model.as_deref().and_then(crate::tested::print_note);
    let multi = multi_note(used.len() > 1, model.as_deref());
    let st_s = &sl.result.stats;
    let summary = format!("{} layers, ~{:.0} min, {:.1} g", st_s.layers, st_s.print_time_s / 60.0, st_s.filament_g);
    let check_text = checklist.iter().map(|(ok, m)| format!("[{}] {m}", if *ok { "x" } else { " " })).collect::<Vec<_>>().join("\n");
    if !checklist.iter().all(|c| c.0) {
        return Err(ToolError::new("preflight_failed", format!("not ready to print ({summary}):\n{check_text}"), "fix the unchecked items, then call print again"));
    }
    if !i.confirm {
        return Ok(ToolOutput::text(format!(
            "Ready to print on '{}' ({summary}):\n{check_text}\n{}Make sure the plate is clear and has the filament the project expects ({}). Call print again with {} to upload and start — only if the user asked to start it.",
            h.name,
            approver_notes(&untested, &multi),
            s.project.filaments.first().map(|f| f.preset.as_str()).unwrap_or("?"),
            start_args(&multi),
        ))
        .with_data(json!({"ready": true, "summary": summary, "untested": untested, "multi_filament": multi.as_ref().map(|n| json!({"verified": false, "note": n})), "checklist": checklist.iter().map(|c| json!({"ok": c.0, "item": c.1})).collect::<Vec<_>>()})));
    }
    if let (Some(n), false) = (&multi, i.multi_filament_ok) {
        return Err(multi_filament_unconfirmed(n));
    }
    let before = st.map(|st| (st.state, st.file)).unwrap_or_default();
    let said = crate::hostprint::upload_and_start(&h, &file, sl.result.gcode.as_bytes()).map_err(|e| ToolError::new("upload_failed", e, "check the printer is on, and its URL and API key (printer_add)"))?;
    s.last_start = Some(Started { printer: h.name.clone(), gcode, at: std::time::Instant::now(), before, record: None });
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
    // a start whose answer was lost, repeated (F-32)
    let printer = s.printer.as_ref().map(|p| p.cfg.name.clone()).unwrap_or_default();
    let gcode = crate::spools::gcode_id(&sl.result.gcode);
    if let Some(e) = already_started(s.last_start.as_ref(), &printer, &gcode, &s_name(s), (st.gcode_state.as_str(), st.subtask_name.as_deref()), st.gcode_state.is_busy()) {
        return Err(e);
    }
    let safe = sl.engine.contains("bambu-templates");
    let machine = s.project.printer.machine.clone();
    checklist.push((safe, if safe { format!("G-code uses Bambu's own start/end sequence for the {machine}") } else { "G-code uses a GENERIC start sequence (Bambu profiles not loaded) — refusing to send".into() }));
    // the plate must be sliced for the printer it goes to: another model's start G-code homes,
    // probes and heats for a different machine
    let want_model = s.machine.printer_model(&machine);
    let model = have_model.map(str::to_string).or(want_model.clone());
    let untested = model.as_deref().and_then(crate::tested::print_note);
    // a plate with filament changes: not offered as if it were verified (F-21)
    let multi_unverified = multi_note(multi, model.as_deref());
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
        checklist.push((false, format!("printer error {} ({}, {}): {}. Fix it and clear it on the printer's screen, then call print again", h.id, h.severity, h.module, alert_text(&h.id).trim_end_matches('.'))));
    }
    let notices: Vec<String> = st.hms.iter().filter(|h| !blocking(h)).map(|h| format!("{} ({}): {}", h.id, h.severity, alert_text(&h.id))).collect();
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
    let multi_data = multi_unverified.as_ref().map(|n| json!({"verified": false, "note": n}));
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
            .with_data(json!({"ready": false, "needs_choice": true, "timelapse": i.timelapse, "question": question, "summary": summary, "trays": trays_arg, "mapping": mapping, "printer_trays": loaded, "notices": notices, "untested": untested, "multi_filament": multi_data, "checklist": items})));
    }
    if !i.confirm {
        // show the plate (when the LAN camera is on) so whoever confirms can see it is clear
        let cfg = s.printer.as_ref().map(|p| p.cfg.clone());
        let snap = cfg.as_ref().and_then(|c| t_camera(s, CameraIn { name: Some(c.name.clone()) }).ok());
        let ps = session(s, i.name.as_deref())?;
        let mut out = ToolOutput::text(format!(
            "Ready to print on '{}' ({summary}):\n{check_text}{on_printer}{notes}\n{}{}Make sure the plate is clear and is the {} the project expects. Call print again with {} to upload and start — only if the user asked to start it.",
            ps.cfg.name,
            approver_notes(&untested, &multi_unverified),
            if i.timelapse { "It records a timelapse.\n" } else { "" },
            s_bed(&sl),
            start_args(&multi_unverified),
        ))
        .with_data(json!({"ready": true, "timelapse": i.timelapse, "summary": summary, "trays": trays_arg, "mapping": mapping, "printer_trays": loaded, "notices": notices, "untested": untested, "multi_filament": multi_data, "checklist": items}));
        if let Some(snap) = snap {
            out.images.extend(snap.images);
            out.text.push_str("\nAttached: a camera picture of the plate — check it is empty.");
        }
        return Ok(out);
    }
    if let (Some(n), false) = (&multi_unverified, i.multi_filament_ok) {
        return Err(multi_filament_unconfirmed(n));
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
    let mut started = Started { printer, gcode, at: std::time::Instant::now(), before: (st.gcode_state.as_str().to_string(), st.subtask_name.clone()), record: None };
    s.last_start = Some(started.clone());
    // inventory: a traceable print record (project version, objects, spools) charging the spool
    let job_name = job.subtask_name.clone().unwrap_or_default();
    let printer_name = s.printer.as_ref().map(|p| p.cfg.name.clone());
    // this plate is printing: it leaves the queue
    let plate = s.plate;
    s.project.queue.retain(|k| *k != plate);
    let recorded = crate::spools::record_print(s, "RUNNING", &job_name, Some(&up.remote_name), printer_name.as_deref(), Some(&ack.task_id), &trays, true);
    if let Ok(pid) = &recorded {
        started.record = Some(pid.clone());
        s.last_start = Some(started);
    }
    let charged = match recorded {
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
        "Uploaded {} ({} KB) and sent the print command{}.{charged}{} Watch it with printer_status; don't call print again for it.",
        up.remote_name,
        up.bytes / 1024,
        if ack.acknowledged { " — printer acknowledged" } else { " (no acknowledgement yet; check printer_status)" },
        if i.timelapse { " It records a timelapse (printer_files with timelapses: 1, once it's done)." } else { "" }
    ))
    .with_data(json!({"remote_name": up.remote_name, "acknowledged": ack.acknowledged, "task_id": ack.task_id})))
}

#[derive(Deserialize, JsonSchema)]
pub struct WaitIn {
    #[serde(default)]
    pub name: Option<String>,
    /// Wait until the state is one of these.
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

/// The alerts already standing when `printer_wait` began. They don't end the wait (F-28: a
/// standing HMS made every wait return after 0 s); a new one, or a new print error, does.
#[derive(Default)]
pub struct WaitBaseline {
    hms: std::collections::HashSet<String>,
    print_error: Option<u32>,
}

impl WaitBaseline {
    pub fn of(st: &PrinterStatus) -> Self {
        WaitBaseline { hms: st.hms.iter().map(|h| h.id.clone()).collect(), print_error: st.print_error.as_ref().map(|e| e.code) }
    }
}

/// Whether `printer_wait`'s condition holds, or the printer needs attention: a print error or
/// a fatal or serious HMS that wasn't there when the wait began.
pub fn wait_met(i: &WaitIn, st: &PrinterStatus, base: &WaitBaseline) -> bool {
    i.states.iter().any(|x| st.gcode_state.as_str().eq_ignore_ascii_case(x))
        || i.layer.is_some_and(|l| st.layer.unwrap_or(0) >= l)
        || i.percent.is_some_and(|p| st.progress_percent.unwrap_or(0) >= p)
        || st.print_error.as_ref().is_some_and(|e| base.print_error != Some(e.code))
        || st.hms.iter().any(|h| (h.severity.eq_ignore_ascii_case("fatal") || h.severity.eq_ignore_ascii_case("serious")) && !base.hms.contains(&h.id))
}

/// `printer_wait` for a shared session (the live socket): never holds the session lock while
/// waiting, so the window and other callers keep working. Polls status briefly under the lock
/// and sleeps on the event log in between.
pub fn wait_unlocked(studio: &std::sync::Arc<std::sync::Mutex<Studio>>, args: Value, actor: crate::Actor) -> Result<ToolOutput, ToolError> {
    let i: WaitIn = crate::tools::parse_input("printer_wait", &crate::tools::schema_of::<WaitIn>(), crate::tools::object_args("printer_wait", args.clone())?)?;
    let timeout = Duration::from_secs(i.timeout_s.unwrap_or(600).clamp(1, 3600));
    let start = std::time::Instant::now();
    let log = studio.lock().map(|s| s.events.clone()).map_err(|_| ToolError::new("session_poisoned", "the live session crashed", "restart only3d"))?;
    let mut base = None;
    loop {
        let out = studio.lock().map_err(|_| ToolError::new("session_poisoned", "the live session crashed", "restart only3d"))?.call("printer_status", json!({"name": i.name}), actor)?;
        let st: PrinterStatus = serde_json::from_value(out.data.clone()).unwrap_or_default();
        let waited = start.elapsed().as_secs();
        let base = base.get_or_insert_with(|| WaitBaseline::of(&st));
        if wait_met(&i, &st, base) {
            return Ok(ToolOutput::text(format!("After {waited}s: {}", out.text)).with_data(out.data));
        }
        if start.elapsed() >= timeout {
            return Ok(ToolOutput::text(format!("Still waiting after {waited}s (condition not met). {}", out.text)).with_data(json!({"timed_out": true, "status": out.data})));
        }
        let seq = log.last_seq();
        let kinds = ["printer_state", "printer_stage", "printer_nozzle", "printer_alert", "print_progress", "print_finished", "print_failed", "printer_connection", "printer_stuck"].map(String::from);
        // a stage that runs far too long ends the wait too (F-42): someone should look
        if let Some(e) = log.wait(seq, &kinds, Duration::from_secs(5).min(timeout.saturating_sub(start.elapsed()))).into_iter().find(|e| e.kind == "printer_stuck") {
            return Ok(ToolOutput::text(format!("Stopped waiting after {}s: {}", start.elapsed().as_secs(), e.summary)).with_data(json!({"stuck": e.data})));
        }
    }
}

fn t_wait(s: &mut Studio, i: WaitIn) -> Result<ToolOutput, ToolError> {
    let ps = session(s, i.name.as_deref())?;
    let timeout = Duration::from_secs(i.timeout_s.unwrap_or(600).clamp(1, 3600));
    let start = std::time::Instant::now();
    let base = WaitBaseline::of(&ps.client.status());
    let r = ps.client.wait_for(|st| wait_met(&i, st, &base), timeout);
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
    /// Set what a slot holds.
    SetSlot,
    /// Load a slot's filament into the nozzle.
    Load,
    /// Unload the nozzle's filament.
    Unload,
    /// Re-read the RFID tag; the AMS turns the spool. Needs an empty nozzle.
    ReadRfid,
    Dry,
    StopDrying,
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
    /// set_slot: filament preset (type, id, nozzle range)
    #[serde(default)]
    pub preset: Option<String>,
    /// set_slot: "PLA", "PETG"...; dry: what to dry for
    #[serde(default)]
    pub filament_type: Option<String>,
    /// set_slot: "#RRGGBB"
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub min_temp: Option<u16>,
    #[serde(default)]
    pub max_temp: Option<u16>,
    /// Bambu filament id (GFA00); "" = generic
    #[serde(default)]
    pub tray_info_idx: Option<String>,
    /// dry: °C (default: Bambu's)
    #[serde(default)]
    pub dry_temp: Option<u32>,
    /// dry: 1-72
    #[serde(default)]
    pub dry_hours: Option<u32>,
    #[serde(default)]
    pub rotate_spools: bool,
    #[serde(default)]
    pub confirm: bool,
}

fn t_ams(s: &mut Studio, i: AmsIn) -> Result<ToolOutput, ToolError> {
    let ps = session(s, i.name.as_deref())?;
    match i.action {
        AmsAction::Dry => {
            let req = crate::drying::DryRequest { ams_id: i.ams_id, filament: i.filament_type, temperature: i.dry_temp, hours: i.dry_hours, rotate_spools: i.rotate_spools, confirm: i.confirm };
            return crate::drying::start(ps, &req);
        }
        AmsAction::StopDrying => return crate::drying::stop(ps, i.ams_id),
        _ => {}
    }
    let st = ps.client.status();
    // Payloads follow OpenBambuAPI mqtt.md (ams_filament_setting / ams_change_filament).
    let payload = match i.action {
        AmsAction::SetSlot => {
            let from = match i.preset.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
                Some(name) => Some(slot_preset(&*crate::bambu::store()?, name)?),
                None => None,
            };
            let ty = i.filament_type.clone().or_else(|| from.as_ref().map(|f| f.filament_type.clone())).ok_or_else(|| ToolError::new("bad_input", "set_slot needs filament_type or preset", "pass a filament type (\"PETG\") or a filament preset"))?.to_ascii_uppercase();
            let color = i.color.clone().unwrap_or_else(|| "#FFFFFF".into()).trim_start_matches('#').to_ascii_uppercase();
            let lo = i.min_temp.or(from.as_ref().and_then(|f| f.temp_min)).unwrap_or(190);
            let hi = i.max_temp.or(from.as_ref().and_then(|f| f.temp_max)).unwrap_or(250);
            let idx = i.tray_info_idx.clone().or_else(|| from.as_ref().map(|f| f.filament_id.clone())).unwrap_or_default();
            json!({"print": {"command": "ams_filament_setting", "ams_id": i.ams_id, "tray_id": i.slot, "slot_id": i.slot,
                "tray_info_idx": idx, "tray_color": format!("{color}FF"),
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
        AmsAction::ReadRfid => {
            read_rfid_ready(&st, i.ams_id, i.slot)?;
            // Bambu Studio's per-slot refresh on printers with the new AMS protocol (H2D):
            // MachineObject::command_ams_refresh_rfid2
            json!({"print": {"command": "ams_get_rfid", "ams_id": i.ams_id, "slot_id": i.slot, "mode": 0}})
        }
        AmsAction::Dry | AmsAction::StopDrying => unreachable!("drying is handled above"),
    };
    let reply = ps.client.request(&Command::Raw(payload.clone()), Duration::from_secs(8));
    let ack = match &reply {
        Ok(v) => format!("printer replied: {}", v.get("print").and_then(|p| p.get("result")).and_then(|r| r.as_str()).unwrap_or("ok")),
        Err(e) if e.code() == "timeout" => "sent (no explicit reply)".to_string(),
        Err(e) => return Err(ToolError::new(e.code(), e.to_string(), e.hint())),
    };
    if i.action == AmsAction::ReadRfid {
        let t = st.ams.iter().find(|u| u.id == i.ams_id).and_then(|u| u.trays.iter().find(|t| t.slot_id == i.slot));
        let label = t.map(|t| crate::trays::label(&st, t)).unwrap_or_else(|| format!("AMS {} slot {}", i.ams_id, i.slot + 1));
        let text = format!("Asked the AMS to read {label}'s RFID tag ({ack}). It turns the spool to find the tag and measures what's left (about 15 s); printer_status shows the slot as \"reading…\" until it's done, and an ams_slot event reports what it read.");
        return Ok(ToolOutput::text(text).with_data(json!({"payload": payload, "slot": label})));
    }
    Ok(ToolOutput::text(format!("AMS {:?} on {} slot {}: {ack}.", i.action, if i.ams_id >= 254 { "external spool".to_string() } else { format!("AMS {}", i.ams_id) }, i.slot + 1)).with_data(json!({"payload": payload})))
}

/// What a filament preset tells an AMS slot (Bambu Studio's AMS material setting dialog).
#[derive(Debug, PartialEq)]
struct SlotPreset {
    filament_type: String,
    /// "GFG00"; "" when the preset has none.
    filament_id: String,
    temp_min: Option<u16>,
    temp_max: Option<u16>,
}

fn slot_preset(store: &only3d_profiles::ProfileStore, name: &str) -> Result<SlotPreset, ToolError> {
    let cfg = store
        .resolve(only3d_profiles::PresetKind::Filament, name)
        .map_err(|e| ToolError::new("unknown_preset", format!("no filament preset '{name}': {e}"), "preset_list kind filament lists them"))?;
    let temp = |k: &str| cfg.get_f64_at(k, 0).filter(|t| *t > 0.0).map(|t| t.round() as u16);
    Ok(SlotPreset {
        filament_type: cfg.get_str("filament_type").filter(|t| !t.trim().is_empty()).unwrap_or("PLA").to_string(),
        filament_id: cfg.get_str("filament_id").unwrap_or("").to_string(),
        temp_min: temp("nozzle_temperature_range_low"),
        temp_max: temp("nozzle_temperature_range_high"),
    })
}

/// Whether the AMS can read a slot's RFID tag now, as Bambu Studio checks before its refresh
/// button: an AMS slot (the external holders have no reader), an idle printer, and no filament
/// in the nozzle that AMS feeds.
fn read_rfid_ready(st: &PrinterStatus, ams_id: u32, slot: u32) -> Result<(), ToolError> {
    if ams_id >= 254 {
        return Err(ToolError::new("bad_input", "the external spool holders have no RFID reader", "pass the ams_id and slot of an AMS slot"));
    }
    let Some(unit) = st.ams.iter().find(|u| u.id == ams_id) else {
        return Err(ToolError::new("no_such_ams", format!("the printer reports no AMS {ams_id}"), "printer_status lists the AMS units"));
    };
    let Some(tray) = unit.trays.iter().find(|t| t.slot_id == slot) else {
        return Err(ToolError::new("bad_input", format!("AMS {} has no slot {}", unit.name, slot + 1), "slots are 0..3 (0 on an AMS HT)"));
    };
    // the AMS answers "success" for an empty slot and then does nothing (tried on an H2D's AMS 2 Pro)
    if !tray.loaded {
        let at = crate::trays::label(st, tray);
        return Err(ToolError::new(
            "slot_empty",
            format!("the AMS reports {at} empty: no filament at its inlet, so it won't turn the spool to read it"),
            "push the filament end into the slot's inlet until the AMS pulls it in; with \"Insertion update\" on it then reads the tag by itself",
        ));
    }
    if st.gcode_state.is_busy() {
        return Err(ToolError::new("printer_busy", "the printer is printing; the AMS can't read tags now", "wait until it is idle (printer_wait)"));
    }
    // the nozzle this unit feeds (firmware numbering: 0 right, 1 left), or any on a single-nozzle printer
    let fed = st.nozzles.iter().filter(|n| unit.extruder.is_none_or(|e| u32::from(n.index) == u32::from(e)));
    if let Some(n) = fed.into_iter().find(|n| n.loaded_tray.is_some()) {
        let side = n.side.as_deref().map(|s| format!("{s} ")).unwrap_or_default();
        return Err(ToolError::new("filament_loaded", format!("filament is loaded in the {side}nozzle this AMS feeds, so it can't read tags"), "unload it first (printer_ams action unload), then read again"));
    }
    Ok(())
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
        let text = hms_text(&pe.id).or_else(|| print_error_text(&dev, &hex)).unwrap_or_else(|| alert_text(&pe.id));
        alerts.push(json!({"kind": "print_error", "code": pe.id, "text": text,
            "actions": offered.as_ref().map(|o| o.iter().map(|(a, label)| json!({"action": a.name(), "label": label})).collect::<Vec<_>>())}));
    }
    for h in &st.hms {
        alerts.push(json!({"kind": "hms", "code": h.id, "severity": h.severity, "module": h.module, "text": alert_text(&h.id)}));
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
                    format!("{} {}: {}{}", a["kind"].as_str().unwrap_or(""), a["code"].as_str().unwrap_or(""), a["text"].as_str().unwrap_or(""), acts.map(|x| format!("\n  actions: {x}")).unwrap_or_default())
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
    /// "left" or "right".
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
    /// download | delete
    pub action: String,
    /// e.g. /timelapse/video.mp4
    pub path: String,
    /// download: local destination file.
    #[serde(default)]
    pub save_to: Option<String>,
    /// delete: must be true.
    #[serde(default)]
    pub confirm: bool,
}

/// The last part of a printer path.
fn file_name(path: &str) -> &str {
    path.rsplit('/').find(|p| !p.is_empty()).unwrap_or("download")
}

/// Download `path` to `dest`, through a `.part` file so a broken transfer never leaves (or
/// replaces) a file that looks whole.
fn save_from(ftp: &mut only3d_bambu::FtpsClient, path: &str, dest: &std::path::Path, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<u64, ToolError> {
    let io = |e: std::io::Error| ToolError::new("io_error", format!("{}: {e}", dest.display()), "pass a save_to path in a folder that exists and can be written");
    let mut part = dest.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let got = (|| {
        let mut f = std::fs::File::create(&part).map_err(io)?;
        let n = ftp.retrieve_to(path, &mut f, progress).map_err(bambu_err)?;
        f.sync_all().map_err(io)?;
        drop(f);
        std::fs::rename(&part, dest).map_err(io)?;
        Ok(n)
    })();
    if got.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    got
}

/// Download one file from a printer to `dest`. Needs no session, so a long video never holds
/// it: the window downloads on a thread of its own.
pub fn download(name: Option<&str>, path: &str, dest: &std::path::Path, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<u64, ToolError> {
    let mut ftp = ftps(name)?.1;
    let got = save_from(&mut ftp, path, dest, progress);
    let _ = ftp.quit();
    got
}

/// A file session with a saved printer, and that printer's name.
pub(crate) fn ftps(name: Option<&str>) -> Result<(String, only3d_bambu::FtpsClient), ToolError> {
    let cfg = pick(&load_book(), name)?;
    let ftp = only3d_bambu::FtpsClient::connect(&cfg).map_err(bambu_err)?;
    Ok((cfg.name, ftp))
}

fn t_file_op(_s: &mut Studio, i: FileOpIn) -> Result<ToolOutput, ToolError> {
    let cfg = pick(&load_book(), i.name.as_deref())?;
    let mut ftp = only3d_bambu::FtpsClient::connect(&cfg).map_err(bambu_err)?;
    let out = match i.action.as_str() {
        "download" => {
            let mut dest = PathBuf::from(i.save_to.clone().unwrap_or_else(|| file_name(&i.path).to_string()));
            if dest.is_dir() {
                dest = dest.join(file_name(&i.path));
            }
            let bytes = save_from(&mut ftp, &i.path, &dest, &mut |_, _| {})?;
            ToolOutput::text(format!("Downloaded {} ({} KB) to {}", i.path, bytes / 1024, dest.display()))
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
    r.add(def::<StatusIn>("printer_status", "Printer status", "Live status: state, progress, layer, time left, temperatures, AMS trays, errors.", false, true, t_status));
    r.add(def::<FilesIn>("printer_files", "Printer files", "List files on the printer's storage (FTPS), optionally in a folder.", false, true, t_files));
    r.add(def::<ControlIn>("printer_control", "Printer control", "Pause, resume or stop the current print, or switch the chamber light. stop needs confirm: true.", false, true, t_control));
    r.add(def::<CameraIn>("printer_camera", "Camera snapshot", "A chamber camera picture (JPEG) to look at the print: first layer, spaghetti, finished parts.", false, true, t_camera));
    r.add(def::<RawIn>("printer_raw", "Raw printer state", "Everything the printer reports over MQTT, merged, optionally one section.", false, true, t_raw));
    r.add(def::<SetIn>("printer_set", "Printer controls", "Speed, light, bed/nozzle temperatures, part/aux/chamber fans. Heating, and temps/fans mid-print, ask the user.", false, true, t_set));
    r.add(def::<AmsIn>("printer_ams", "AMS", "AMS slots: set what a slot holds, read its RFID tag, or load/unload filament (idle printer only). dry: plan, then confirm.", false, true, t_ams));
    r.add(def::<FileOpIn>("printer_file", "Printer file", "Download (timelapses, jobs) or delete a file on the printer (delete needs confirm: true).", false, true, t_file_op));
    r.add(def::<SkipIn>("printer_skip_objects", "Skip objects", "Stop printing some objects of the running job (e.g. one that came loose). Needs confirm: true.", false, true, t_skip));
    r.add(def::<AlertIn>("printer_alert", "Printer alert", "The printer's alerts with the actions Bambu offers for each, or perform one (the user approves it in only3d).", false, true, t_alert));
    r.add(def::<NozzleIn>("printer_nozzle", "Switch nozzle", "Make the left or right nozzle active on a dual-nozzle printer (idle only). Approved by the user in only3d.", false, true, t_nozzle));
    r.add(def::<WaitIn>("printer_wait", "Wait for printer", "Block until the print reaches a state (FINISH, FAILED, PAUSE...), a layer or a percentage, or reports a new error; returns the status.", false, true, t_wait));
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

    /// F-35: the brief status is a few lines and a small object, with the alerts kept.
    #[test]
    fn brief_status_is_short_and_keeps_the_alerts() {
        use only3d_bambu::{AmsTray, AmsUnit, HmsError, PrintError, TrayRef};
        let mut st = h2d(0);
        st.gcode_state = only3d_bambu::GcodeState::parse("PAUSE");
        st.layer = Some(57);
        st.total_layers = Some(58);
        st.nozzles[0].active = true;
        st.nozzles[0].side = Some("right".into());
        st.nozzles[0].loaded_tray = Some(TrayRef { ams_id: 0, slot_id: 1 });
        st.print_error = Some(PrintError { code: 0x0700_8011, id: "0700_8011".into() });
        st.hms = vec![HmsError::new(0x0500_0200, 0x0003_000A)];
        st.ams = vec![AmsUnit { id: 0, trays: (0..4).map(|k| AmsTray { global_id: k, loaded: true, filament_type: Some("PLA".into()), ..Default::default() }).collect(), ..Default::default() }];
        let (text, data) = brief_status("h2d", &st);
        assert!(text.starts_with("h2d: PAUSE") && text.contains("layer 57/58"), "{text}");
        assert!(text.contains("right ") && text.contains("active (AMS 1 slot 2)"), "{text}");
        assert!(text.contains("Print error 0700_8011") && text.contains("HMS 0500_0200_0003_000A common"), "{text}");
        let full = serde_json::to_string(&st).unwrap().len();
        let brief = serde_json::to_string(&data).unwrap().len();
        assert!(brief * 3 < full, "brief {brief} B vs full {full} B");
        assert!(text.lines().count() <= 4, "{text}");
    }

    /// F-31: a code with no text says so; an empty text in Bambu's list isn't a description.
    #[test]
    fn alert_codes_without_text_say_so() {
        let mut db = std::collections::HashMap::new();
        assert!(describe_code(&db, "0500_0200_0003_000A").contains("install Bambu Studio"));
        db.insert("07008011".to_string(), "AMS filament ran out.".to_string());
        assert_eq!(describe_code(&db, "0700_8011"), "AMS filament ran out.");
        assert!(describe_code(&db, "0500_0200_0003_000A").starts_with("no description: Bambu's code list has no text"));
    }

    /// F-28: an alert already standing doesn't end a wait; a new one, or a new print error, does.
    #[test]
    fn wait_ignores_standing_alerts() {
        use only3d_bambu::{HmsError, PrintError};
        let i = WaitIn { name: None, states: vec!["FINISH".into()], layer: None, percent: None, timeout_s: None };
        let serious = HmsError::new(0x0C00_0400, 0x0002_0026);
        let standing = PrinterStatus { hms: vec![serious.clone()], print_error: Some(PrintError { code: 0x0500_4004, id: "0500_4004".into() }), ..Default::default() };
        let base = WaitBaseline::of(&standing);
        assert!(!wait_met(&i, &standing, &base), "a standing alert ended the wait");
        assert!(wait_met(&i, &standing, &WaitBaseline::default()), "a new alert must end it");
        let mut more = standing.clone();
        more.hms.push(HmsError::new(0x0300_0100, 0x0001_0007));
        assert!(wait_met(&i, &more, &base), "a new fatal alert must end it");
        let mut err = standing.clone();
        err.print_error = Some(PrintError { code: 0x0700_8011, id: "0700_8011".into() });
        assert!(wait_met(&i, &err, &base), "a new print error must end it");
        let mut info = standing.clone();
        info.hms.push(HmsError::new(0x0500_0200, 0x0004_000A));
        assert!(!wait_met(&i, &info, &base), "an info alert doesn't end it");
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
    fn status_shows_the_ams_model_climate_and_drying() {
        let mut st = h2d(0);
        st.ams[0].trays = (0..4).map(|k| AmsTray { slot_id: k, global_id: k, ..Default::default() }).collect();
        st.ams[0].trays[1].reading = true;
        st.ams[0].name = "A".into();
        st.ams[0].model = Some("AMS 2 Pro".into());
        st.ams[0].humidity_percent = Some(27);
        st.ams[0].temperature = Some(50.0);
        st.ams[0].drying = Some(only3d_bambu::AmsDrying { state: "drying".into(), summary: "drying PLA at 50 °C (dehumidifying), 10 h 37 min left of 12 h".into(), ..Default::default() });
        st.ams_state = Some(only3d_bambu::AmsState { activity: Some("reading RFID".into()), read_rfid_on_insert: Some(false), read_rfid_on_power_on: Some(true) });
        let t = status_text("h2d", &st);
        assert!(t.contains("AMS A (AMS 2 Pro) → right nozzle: A1 empty, A2 reading…, A3 empty, A4 empty"), "{t}");
        assert!(t.contains("\n  Humidity 27%, 50 °C · drying PLA at 50 °C (dehumidifying), 10 h 37 min left of 12 h"), "{t}");
        assert!(t.contains("\nAMS is reading RFID"), "{t}");
        assert!(t.contains("\"Insertion update\" is off"), "{t}");
    }

    #[test]
    fn a_slot_takes_its_type_id_and_nozzle_range_from_a_filament_preset() {
        let store = crate::bambu::store().unwrap();
        let p = slot_preset(&store, "Bambu PETG Basic @BBL H2D 0.4 nozzle").unwrap();
        assert_eq!((p.filament_type.as_str(), p.filament_id.as_str()), ("PETG", "GFG00"));
        assert!(matches!((p.temp_min, p.temp_max), (Some(lo), Some(hi)) if lo >= 200 && hi > lo), "{p:?}");
        assert_eq!(slot_preset(&store, "No Such Filament").unwrap_err().code, "unknown_preset");
    }

    #[test]
    fn reading_an_rfid_tag_needs_an_ams_slot_an_idle_printer_and_an_empty_nozzle() {
        use only3d_bambu::{GcodeState, TrayRef};
        let mut st = h2d(0);
        st.ams[0].trays = (0..4).map(|k| AmsTray { slot_id: k, global_id: k, loaded: k != 1, ..Default::default() }).collect();
        // the left nozzle holds the external spool; the AMS feeds the right one, which is empty
        let ext = TrayRef { ams_id: 254, slot_id: 0 };
        st.nozzles = vec![NozzleStatus { index: 0, side: Some("right".into()), ..Default::default() }, NozzleStatus { index: 1, side: Some("left".into()), loaded_tray: Some(ext), ..Default::default() }];
        assert!(read_rfid_ready(&st, 0, 2).is_ok());
        let code = |st: &PrinterStatus, ams: u32, slot: u32| read_rfid_ready(st, ams, slot).unwrap_err().code;
        assert_eq!(code(&st, 254, 0), "bad_input", "external holders have no reader");
        assert_eq!(code(&st, 1, 0), "no_such_ams");
        assert_eq!(code(&st, 0, 4), "bad_input");
        assert_eq!(code(&st, 0, 1), "slot_empty", "the AMS ignores an empty slot");
        let busy = PrinterStatus { gcode_state: GcodeState::Running, ..st.clone() };
        assert_eq!(code(&busy, 0, 0), "printer_busy");
        let mut loaded = st.clone();
        loaded.nozzles[0].loaded_tray = Some(TrayRef { ams_id: 0, slot_id: 3 });
        assert_eq!(code(&loaded, 0, 0), "filament_loaded");
        // an AMS wired to the left nozzle is blocked by the external spool there
        let left = PrinterStatus { ams: vec![AmsUnit { extruder: Some(1), ..st.ams[0].clone() }], ..st };
        assert_eq!(code(&left, 0, 0), "filament_loaded");
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

    /// F-21: a plate with two filaments, on a printer where that isn't verified (the H2D, whose
    /// filament change hung), says so and needs the user's extra yes before anything is sent.
    /// Driven through another maker's link to a stand-in server on 127.0.0.1, so nothing real
    /// is reached.
    #[test]
    fn an_unverified_multi_filament_print_needs_an_extra_yes() {
        use crate::hostprint::{HostKind, HostPrinter};
        use crate::Actor;
        let (url, seen) = crate::hostprint::tests::server(r#"{"result": {"status": {"print_stats": {"state": "standby", "filename": ""}, "virtual_sdcard": {"progress": 0}}, "print_started": true}}"#);
        let mut s = Studio::new();
        s.call("add_object", json!({"id": "a", "shape": {"type": "box", "size": [20, 20, 2]}, "position": [150, 160]}), Actor::User).unwrap();
        s.call("add_object", json!({"id": "b", "shape": {"type": "box", "size": [20, 20, 2]}, "position": [200, 160], "filament": 2}), Actor::User).unwrap();
        s.call("set_filament", json!({"slot": 2, "color": "#E05A2B"}), Actor::User).unwrap();
        let h = HostPrinter { name: "stand-in".into(), kind: HostKind::Moonraker, url, api_key: None, machine: Some(s.project.printer.machine.clone()) };
        let args = |v: Value| serde_json::from_value::<PrintIn>(v).unwrap();
        let uploads = || seen.lock().unwrap().iter().filter(|r| r.starts_with("POST")).count();

        let pre = host_print(&mut s, h.clone(), args(json!({}))).unwrap();
        let note = pre.data["multi_filament"]["note"].as_str().expect("the preflight says it isn't verified");
        assert!(note.contains("not verified") && note.contains("hung"), "{note}");
        assert!(pre.text.contains(&format!("[!] {note}")) && pre.text.contains("multi_filament_ok: true"), "{}", pre.text);
        // the approval would refuse it before asking the user
        assert_eq!(multi_filament_unacked(&pre, &json!({"confirm": true})).unwrap().code, "multi_filament_unverified");
        assert!(multi_filament_unacked(&pre, &json!({"confirm": true, "multi_filament_ok": true})).is_none());

        let e = host_print(&mut s, h.clone(), args(json!({"confirm": true}))).unwrap_err();
        assert_eq!(e.code, "multi_filament_unverified");
        assert!(e.hint.contains("multi_filament_ok: true"), "{e:?}");
        assert_eq!(uploads(), 0, "nothing was sent");

        host_print(&mut s, h.clone(), args(json!({"confirm": true, "multi_filament_ok": true}))).unwrap();
        assert_eq!(uploads(), 1);

        // one filament: no note, and confirm alone starts it
        s.call("remove_object", json!({"ids": ["b"]}), Actor::User).unwrap();
        let pre = host_print(&mut s, h.clone(), args(json!({}))).unwrap();
        assert!(pre.data["multi_filament"].is_null() && !pre.text.contains("multi_filament_ok"), "{}", pre.text);
        host_print(&mut s, h, args(json!({"confirm": true}))).unwrap();
        assert_eq!(uploads(), 2);
    }

    /// F-32: a print started, its answer was lost ("Connection closed"), and the call repeated
    /// would have printed the plate twice. Driven through another maker's link to stand-in
    /// servers on 127.0.0.1.
    #[test]
    fn a_repeated_start_of_the_same_plate_is_refused() {
        use crate::hostprint::{HostKind, HostPrinter};
        use crate::Actor;
        let (url, seen) = crate::hostprint::tests::server(r#"{"result": {"status": {"print_stats": {"state": "standby", "filename": ""}, "virtual_sdcard": {"progress": 0}}, "print_started": true}}"#);
        let mut s = Studio::new();
        s.call("add_object", json!({"id": "a", "shape": {"type": "box", "size": [20, 20, 2]}, "position": [150, 160]}), Actor::User).unwrap();
        let h = HostPrinter { name: "stand-in".into(), kind: HostKind::Moonraker, url, api_key: None, machine: Some(s.project.printer.machine.clone()) };
        let args = |v: Value| serde_json::from_value::<PrintIn>(v).unwrap();
        let uploads = || seen.lock().unwrap().iter().filter(|r| r.starts_with("POST")).count();

        host_print(&mut s, h.clone(), args(json!({"confirm": true}))).unwrap();
        assert_eq!(uploads(), 1);
        // the printer still reads standby (its status hasn't caught up): the repeat is refused,
        // and so is the dry run the approval makes, so the user isn't asked again
        for a in [json!({"confirm": true}), json!({})] {
            let e = host_print(&mut s, h.clone(), args(a)).unwrap_err();
            assert_eq!(e.code, "already_started", "{e:?}");
            assert!(e.message.contains("sent to 'stand-in'") && e.hint.contains("printer_status"), "{e:?}");
        }
        assert_eq!(uploads(), 1, "nothing was sent twice");

        // another plate is a new print
        s.call("update_object", json!({"id": "a", "shape": {"type": "box", "size": [20, 20, 3]}}), Actor::User).unwrap();
        host_print(&mut s, h.clone(), args(json!({"confirm": true}))).unwrap();
        assert_eq!(uploads(), 2);
        // and once the printer has had time to show it, the idle check decides alone
        let l = s.last_start.as_mut().unwrap();
        l.at = std::time::Instant::now().checked_sub(START_SETTLE + Duration::from_secs(1)).unwrap();
        host_print(&mut s, h.clone(), args(json!({"confirm": true}))).unwrap();
        assert_eq!(uploads(), 3);

        // a printer busy with a job of this plate's name (a window opened since the start)
        let file = format!("{}.gcode", sanitize(&s_name(&s)));
        let reply: &'static str = Box::leak(format!(r#"{{"result": {{"status": {{"print_stats": {{"state": "printing", "filename": "{file}"}}, "virtual_sdcard": {{"progress": 0.1}}}}}}}}"#).into_boxed_str());
        let (url, seen) = crate::hostprint::tests::server(reply);
        let busy = HostPrinter { url, ..h };
        s.last_start = None;
        let e = host_print(&mut s, busy, args(json!({"confirm": true}))).unwrap_err();
        assert_eq!(e.code, "already_started", "{e:?}");
        assert!(e.message.contains(&format!("is printing '{file}'")), "{e:?}");
        assert_eq!(seen.lock().unwrap().iter().filter(|r| r.starts_with("POST")).count(), 0);
    }

    #[test]
    fn a_start_is_taken_as_done_only_for_the_same_gcode_on_the_same_printer() {
        let before = ("FINISH".to_string(), Some("old".to_string()));
        let l = Started { printer: "h2d".into(), gcode: "g1".into(), at: std::time::Instant::now(), before: before.clone(), record: Some("P-0031".into()) };
        let refused = |printer: &str, gcode: &str, now: (&str, Option<&str>), busy: bool| already_started(Some(&l), printer, gcode, "plate", now, busy).map(|e| e.message);
        // status not caught up yet, or printing it
        assert!(refused("h2d", "g1", ("FINISH", Some("old")), false).unwrap().contains("recorded as P-0031, and the printer may not show it yet"));
        assert!(refused("h2d", "g1", ("RUNNING", Some("plate")), true).unwrap().contains("the printer is RUNNING"));
        // the printer moved on (it failed at once and is idle again): a new start may go
        assert!(refused("h2d", "g1", ("FAILED", Some("plate")), false).is_none());
        // other G-code, or another printer
        assert!(refused("h2d", "g2", ("FINISH", Some("old")), false).is_none());
        assert!(refused("p1s", "g1", ("FINISH", Some("old")), false).is_none());
        // nothing started here, but the printer is printing this plate's job (in a folder, too)
        assert!(already_started(None, "h2d", "g1", "plate", ("RUNNING", Some("jobs/plate")), true).unwrap().message.contains("is RUNNING 'jobs/plate'"));
        assert!(already_started(None, "h2d", "g1", "plate", ("RUNNING", Some("other")), true).is_none());
        assert!(already_started(None, "h2d", "g1", "plate", ("FINISH", Some("plate")), false).is_none());
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
