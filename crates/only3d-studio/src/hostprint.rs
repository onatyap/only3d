//! Printers of other makers, on their network APIs: Klipper through Moonraker, OctoPrint, and
//! PrusaLink. only3d reads whether the printer is idle, and uploads the G-code and starts it
//! (after the same preflight and the same approval as a Bambu print). HTTP goes through `curl`,
//! which every supported OS has; the API key reaches it on stdin, never on its command line.

use std::fmt;
use std::io::Write as _;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Which API the printer speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum HostKind {
    /// Klipper's Moonraker (Mainsail, Fluidd; Creality K1/K2, Voron, Sovol, Elegoo...).
    Moonraker,
    Octoprint,
    /// Prusa MK4, MINI, XL, CORE One.
    Prusalink,
}

impl HostKind {
    pub fn parse(s: &str) -> Option<HostKind> {
        match s.to_ascii_lowercase().as_str() {
            "moonraker" | "klipper" | "mainsail" | "fluidd" => Some(HostKind::Moonraker),
            "octoprint" => Some(HostKind::Octoprint),
            "prusalink" | "prusa" => Some(HostKind::Prusalink),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            HostKind::Moonraker => "Klipper (Moonraker)",
            HostKind::Octoprint => "OctoPrint",
            HostKind::Prusalink => "PrusaLink",
        }
    }
}

/// A network printer of another maker, kept in the printer book next to the Bambu ones.
#[derive(Clone, Serialize, Deserialize)]
pub struct HostPrinter {
    pub name: String,
    pub kind: HostKind,
    /// Base URL, e.g. `http://192.168.1.20` or `http://voron.local:7125`.
    pub url: String,
    /// OctoPrint's and PrusaLink's API key (Moonraker's, when it asks for one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// The printer preset it is (only3d's machine name, e.g. `Creality K1C 0.4 nozzle`): a plate
    /// sliced for another one is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
}

impl fmt::Debug for HostPrinter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostPrinter").field("name", &self.name).field("kind", &self.kind).field("url", &self.url).field("api_key", &self.api_key.as_ref().map(|_| "<hidden>")).field("machine", &self.machine).finish()
    }
}

/// What the printer is doing.
#[derive(Clone, Debug, PartialEq)]
pub struct HostStatus {
    /// The API's own word for it (`standby`, `Printing`, `IDLE`...).
    pub state: String,
    /// Free to start a print.
    pub idle: bool,
    /// 0..1 while printing.
    pub progress: Option<f64>,
    pub file: Option<String>,
}

enum Body<'a> {
    None,
    /// multipart form: fields, then the file field (name, path, file name)
    Form(Vec<(&'static str, &'static str)>, (&'static str, &'a Path, String)),
    /// PUT the file as the body
    File(&'a Path),
}

/// A string for curl's config file (`-K`), quoted.
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"))
}

/// One HTTP request through curl. Returns the status code and body.
fn request(method: &str, url: &str, key: Option<&str>, headers: &[(&str, &str)], body: Body<'_>, timeout_s: u32) -> Result<(u16, String), String> {
    let mut cfg = format!("url = {}\nrequest = {}\n", quote(url), quote(method));
    if let Some(k) = key.filter(|k| !k.is_empty()) {
        cfg.push_str(&format!("header = {}\n", quote(&format!("X-Api-Key: {k}"))));
    }
    for (h, v) in headers {
        cfg.push_str(&format!("header = {}\n", quote(&format!("{h}: {v}"))));
    }
    if !matches!(body, Body::None) {
        // send the file straight away (no `Expect: 100-continue` round trip)
        cfg.push_str("header = \"Expect:\"\n");
    }
    match &body {
        Body::None => {}
        Body::Form(fields, (field, path, file_name)) => {
            for (k, v) in fields {
                cfg.push_str(&format!("form = {}\n", quote(&format!("{k}={v}"))));
            }
            cfg.push_str(&format!("form = {}\n", quote(&format!("{field}=@{};filename={file_name}", path.display()))));
        }
        Body::File(path) => cfg.push_str(&format!("upload-file = {}\n", quote(&path.display().to_string()))),
    }
    let mut child = only3d_model::proc::command("curl")
        .args(["-sS", "-m", &timeout_s.to_string(), "-w", "\n%{http_code}", "-K", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl isn't available ({e}); only3d sends to network printers through it"))?;
    child.stdin.take().expect("piped").write_all(cfg.as_bytes()).map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("can't reach {url}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let (body, code) = text.rsplit_once('\n').unwrap_or(("", &text));
    Ok((code.trim().parse().unwrap_or(0), body.to_string()))
}

fn base(h: &HostPrinter) -> String {
    let u = h.url.trim().trim_end_matches('/');
    if u.contains("://") {
        u.to_string()
    } else {
        format!("http://{u}")
    }
}

fn json(code: u16, body: &str, what: &str) -> Result<Value, String> {
    match code {
        200..=299 => serde_json::from_str(body).map_err(|e| format!("{what}: unexpected answer ({e})")),
        401 | 403 => Err(format!("{what}: the printer refused the API key (HTTP {code})")),
        _ => Err(format!("{what}: HTTP {code} {}", body.chars().take(200).collect::<String>())),
    }
}

/// Whether the printer is idle, and what it is doing.
pub fn status(h: &HostPrinter) -> Result<HostStatus, String> {
    let b = base(h);
    let key = h.api_key.as_deref();
    Ok(match h.kind {
        HostKind::Moonraker => {
            let (c, body) = request("GET", &format!("{b}/printer/objects/query?print_stats&virtual_sdcard"), key, &[], Body::None, 10)?;
            let v = json(c, &body, "Moonraker status")?;
            let st = &v["result"]["status"];
            let state = st["print_stats"]["state"].as_str().unwrap_or("unknown").to_string();
            HostStatus {
                idle: matches!(state.as_str(), "standby" | "complete" | "cancelled"),
                progress: st["virtual_sdcard"]["progress"].as_f64().filter(|_| state == "printing" || state == "paused"),
                file: st["print_stats"]["filename"].as_str().filter(|f| !f.is_empty()).map(str::to_string),
                state,
            }
        }
        HostKind::Octoprint => {
            let (c, body) = request("GET", &format!("{b}/api/job"), key, &[], Body::None, 10)?;
            let v = json(c, &body, "OctoPrint status")?;
            let state = v["state"].as_str().unwrap_or("unknown").to_string();
            HostStatus {
                idle: state == "Operational",
                progress: v["progress"]["completion"].as_f64().map(|p| p / 100.0).filter(|_| state.starts_with("Printing") || state.starts_with("Paus")),
                file: v["job"]["file"]["name"].as_str().map(str::to_string),
                state,
            }
        }
        HostKind::Prusalink => {
            let (c, body) = request("GET", &format!("{b}/api/v1/status"), key, &[], Body::None, 10)?;
            let v = json(c, &body, "PrusaLink status")?;
            let state = v["printer"]["state"].as_str().unwrap_or("UNKNOWN").to_string();
            HostStatus {
                idle: matches!(state.as_str(), "IDLE" | "READY" | "FINISHED" | "STOPPED"),
                progress: v["job"]["progress"].as_f64().map(|p| p / 100.0),
                file: None,
                state,
            }
        }
    })
}

/// Upload `gcode` as `file_name` and start printing it. Returns what the printer said.
pub fn upload_and_start(h: &HostPrinter, file_name: &str, gcode: &[u8]) -> Result<String, String> {
    let b = base(h);
    // tests never start a print: under `cargo test` only a server on this machine is reachable
    #[cfg(test)]
    {
        let host = b.split("://").nth(1).unwrap_or("").split(['/', ':']).next().unwrap_or("");
        if !matches!(host, "127.0.0.1" | "localhost" | "[") {
            return Err(format!("tests don't start prints (refusing {b})"));
        }
    }
    let dir = tempfile_dir()?;
    let path = dir.join(file_name);
    std::fs::write(&path, gcode).map_err(|e| e.to_string())?;
    let key = h.api_key.as_deref();
    let res = match h.kind {
        HostKind::Moonraker => request("POST", &format!("{b}/server/files/upload"), key, &[], Body::Form(vec![("print", "true")], ("file", &path, file_name.to_string())), 600)
            .and_then(|(c, body)| json(c, &body, "Moonraker upload"))
            .and_then(|v| match v["result"]["print_started"].as_bool() {
                Some(true) => Ok(format!("uploaded {file_name} and started it")),
                _ => Err(format!("uploaded {file_name} but Moonraker didn't start it: {}", v["result"])),
            }),
        HostKind::Octoprint => request("POST", &format!("{b}/api/files/local"), key, &[], Body::Form(vec![("select", "true"), ("print", "true")], ("file", &path, file_name.to_string())), 600)
            .and_then(|(c, body)| json(c, &body, "OctoPrint upload"))
            .map(|v| format!("uploaded {} and started it", v["files"]["local"]["name"].as_str().unwrap_or(file_name))),
        HostKind::Prusalink => {
            let url = format!("{b}/api/v1/files/usb/{}", percent(file_name));
            request("PUT", &url, key, &[("Content-Type", "text/x.gcode"), ("Print-After-Upload", "?1"), ("Overwrite", "?1")], Body::File(&path), 600).and_then(|(c, body)| match c {
                200..=299 => Ok(format!("uploaded {file_name} to the printer's USB drive and started it")),
                401 | 403 => Err("PrusaLink refused the API key".into()),
                _ => Err(format!("PrusaLink upload: HTTP {c} {}", body.chars().take(200).collect::<String>())),
            })
        }
    };
    let _ = std::fs::remove_dir_all(&dir);
    res
}

fn tempfile_dir() -> Result<std::path::PathBuf, String> {
    let d = std::env::temp_dir().join(format!("only3d-upload-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)));
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    Ok(d)
}

/// A file name for a URL path.
fn percent(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// A one-file HTTP server on 127.0.0.1 answering every request with `reply`, recording
    /// the requests it got (head and body).
    fn server(reply: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut head = String::new();
                let mut len = 0usize;
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                    head.push_str(&line);
                }
                let mut body = vec![0; len];
                let _ = r.read_exact(&mut body);
                log.lock().unwrap().push(format!("{head}\n{}", String::from_utf8_lossy(&body)));
                let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len());
                let _ = (&s).write_all(resp.as_bytes());
            }
        });
        (url, seen)
    }

    #[test]
    fn moonraker_status_and_upload() {
        let (url, seen) = server(r#"{"result": {"status": {"print_stats": {"state": "standby", "filename": ""}, "virtual_sdcard": {"progress": 0}}, "print_started": true}}"#);
        let h = HostPrinter { name: "k1c".into(), kind: HostKind::Moonraker, url: url.clone(), api_key: Some("secret-key".into()), machine: None };
        let st = status(&h).unwrap();
        assert!(st.idle && st.state == "standby", "{st:?}");
        let said = upload_and_start(&h, "box.gcode", b"G28\nG1 X10\n").unwrap();
        assert!(said.contains("started"), "{said}");
        let reqs = seen.lock().unwrap();
        assert!(reqs[0].starts_with("GET /printer/objects/query?print_stats"), "{}", reqs[0]);
        let up = &reqs[1];
        assert!(up.starts_with("POST /server/files/upload") && up.contains("X-Api-Key: secret-key"), "{up}");
        assert!(up.contains("name=\"print\"") && up.contains("filename=\"box.gcode\"") && up.contains("G1 X10"), "{up}");
        // the key never went on curl's command line (it's on stdin): nothing to check there,
        // but Debug output hides it
        assert!(!format!("{h:?}").contains("secret-key"));
    }

    #[test]
    fn octoprint_and_prusalink_requests() {
        let (url, seen) = server(r#"{"state": "Operational", "files": {"local": {"name": "box.gcode"}}, "printer": {"state": "IDLE"}}"#);
        let o = HostPrinter { name: "o".into(), kind: HostKind::Octoprint, url: url.clone(), api_key: Some("k".into()), machine: None };
        assert!(status(&o).unwrap().idle);
        upload_and_start(&o, "box.gcode", b"G28\n").unwrap();
        let p = HostPrinter { name: "p".into(), kind: HostKind::Prusalink, url: url.trim_start_matches("http://").to_string(), api_key: Some("k".into()), machine: None };
        assert!(status(&p).unwrap().idle);
        upload_and_start(&p, "my box.gcode", b"G28\n").unwrap();
        let reqs = seen.lock().unwrap();
        assert!(reqs[1].starts_with("POST /api/files/local") && reqs[1].contains("name=\"print\""), "{}", reqs[1]);
        assert!(reqs[2].starts_with("GET /api/v1/status"), "{}", reqs[2]);
        assert!(reqs[3].starts_with("PUT /api/v1/files/usb/my%20box.gcode") && reqs[3].contains("Print-After-Upload: ?1"), "{}", reqs[3]);
    }

    #[test]
    fn tests_never_reach_a_real_printer() {
        let h = HostPrinter { name: "x".into(), kind: HostKind::Moonraker, url: "http://192.0.2.10".into(), api_key: None, machine: None };
        assert!(upload_and_start(&h, "a.gcode", b"").unwrap_err().contains("don't start prints"));
    }
}
