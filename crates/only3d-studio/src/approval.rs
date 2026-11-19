//! The user approves what an agent does to the printer.
//!
//! Some calls start or stop machine motion or destroy data on the printer: starting a print,
//! stopping one, skipping objects, deleting printer files, loading/unloading filament, heating
//! the nozzle or bed (even on an idle printer), changing heat or fans mid-print. Turning a
//! heater off needs no approval. When an *agent* makes one of these calls (over MCP or the live socket),
//! the user is asked: in the only3d window when one is open, otherwise through the agent's
//! own app (MCP elicitation). An agent passing `confirm: true` is not enough on its own: it only
//! asks for the action; the user's answer decides it.
//!
//! [`gated_call`] holds no lock while it waits, so the window stays responsive.

use serde_json::{json, Value};

use crate::tools::{ImageOut, ToolError, ToolOutput};

/// What the user is asked to approve.
#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    /// Who is asking, e.g. "claude-code" (the MCP client name) or "an agent".
    pub requester: String,
    pub tool: String,
    /// One line, e.g. "Start printing 'first print tile' on h2d".
    pub title: String,
    /// The preflight checklist or the printer's current state, plus the exact request.
    pub details: String,
    /// The camera picture of the plate, when the preflight took one.
    pub images: Vec<ImageOut>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Approved,
    Denied,
    /// Nobody answered in time.
    TimedOut,
    /// There is no way to ask the user here (no window, client cannot elicit).
    Unavailable,
}

/// The title of the approval a call needs, or `None` when it needs none.
///
/// It reads the arguments by name. That is sound only because every tool refuses arguments that
/// aren't an object before parsing them (`tools::object_args`), so a call the gate lets through
/// can't carry its fields some other way, such as a positional array.
pub fn gate(tool: &str, args: &Value) -> Option<String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").to_ascii_lowercase();
    let confirm = args.get("confirm").and_then(Value::as_bool).unwrap_or(false);
    let printer = args.get("name").and_then(Value::as_str).map(|n| format!(" on {n}")).unwrap_or_default();
    match tool {
        "print" if confirm => Some(format!("Start a print{printer}")),
        "printer_control" if s("action") == "stop" => Some(format!("Stop the running print{printer} (cannot be undone)")),
        "printer_skip_objects" if confirm => Some(format!("Skip objects of the running print{printer} (cannot be undone)")),
        "printer_file" if s("action") == "delete" => Some(format!("Delete '{}' from the printer{printer}", args.get("path").and_then(Value::as_str).unwrap_or("?"))),
        "printer_ams" if matches!(s("action").as_str(), "load" | "unload") => Some(format!("{} filament{printer}", if s("action") == "load" { "Load" } else { "Unload" })),
        "printer_set" => match heats(args) {
            Some(what) => Some(format!("Heat {what}{printer}")),
            None if confirm => Some(format!("Change temperatures or fans during a running print{printer}")),
            None => None,
        },
        "printer_alert" if !matches!(s("action").as_str(), "" | "list") => Some(format!("Answer the printer's alert: {}{printer}", s("action"))),
        "printer_nozzle" => Some(format!("Switch to the {} nozzle{printer} (the toolhead moves)", s("side"))),
        _ => None,
    }
}

/// What a `printer_set` call heats, e.g. "the left nozzle to 220 °C and the bed to 60 °C", or
/// `None` when it heats nothing (no temperature given, or 0 = off). Heating an idle printer
/// needs the user too: nobody may be there to watch it. A value that isn't plainly a number
/// counts as heating, so a malformed request is asked about rather than let through.
fn heats(args: &Value) -> Option<String> {
    let nozzle = match args.get("nozzle").and_then(Value::as_str) {
        Some(side) => format!("the {} nozzle", side.to_ascii_lowercase()),
        None => "the nozzle".to_string(),
    };
    let parts: Vec<String> = [("nozzle_temp", nozzle), ("bed_temp", "the bed".to_string())]
        .into_iter()
        .filter_map(|(key, what)| {
            let v = args.get(key).filter(|v| !v.is_null())?;
            match v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())) {
                Some(c) if c <= 0.0 => None,
                Some(c) => Some(format!("{what} to {c} °C")),
                None => Some(format!("{what} ({v})")),
            }
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(" and "))
}

/// Run `tool` for an agent, asking the user first when [`gate`] says so.
///
/// `call` runs a tool (taking whatever lock it needs only for that call); `approve` asks the user
/// and blocks until they answer; `note` records the decision in the session journal.
pub fn gated_call(
    tool: &str,
    args: Value,
    requester: &str,
    call: &mut dyn FnMut(&str, Value) -> Result<ToolOutput, ToolError>,
    approve: &mut dyn FnMut(ApprovalRequest) -> Decision,
    note: &mut dyn FnMut(&str, bool),
) -> Result<ToolOutput, ToolError> {
    match prepare(tool, &args, requester, call)? {
        None => call(tool, args),
        Some(req) => {
            let decision = approve(req.clone());
            finish(&req, args, decision, call, note)
        }
    }
}

/// The approval request for a gated call (`None` when the call needs none). Shows the user what
/// they are approving: a print gets its full preflight (checklist + plate photo), and a failing
/// preflight is returned to the agent as an error without bothering the user.
pub fn prepare(tool: &str, args: &Value, requester: &str, call: &mut dyn FnMut(&str, Value) -> Result<ToolOutput, ToolError>) -> Result<Option<ApprovalRequest>, ToolError> {
    let Some(mut title) = gate(tool, args) else { return Ok(None) };
    let (details, images) = if tool == "print" {
        let mut dry = args.clone();
        dry["confirm"] = json!(false);
        let pre = call("print", dry)?;
        // the filament isn't the project's and nobody has chosen a tray: the agent asks the user
        // which first, rather than the user approving a print that would then refuse to start
        if let Some(e) = crate::printer::choice_needed(&pre) {
            return Err(e);
        }
        // Say *what* will be printed before anything else: project, objects and material, and a
        // picture of the model; then the plate photo and the checklist.
        let (what, name) = describe_job(call);
        if let Some(n) = name {
            title = format!("Print '{n}'{}", args.get("name").and_then(Value::as_str).map(|p| format!(" on {p}")).unwrap_or_default());
        }
        let mut images = call("render", json!({"view": "iso", "width": 720, "height": 420})).map(|o| o.images).unwrap_or_default();
        images.extend(pre.images);
        (format!("{what}{}", pre.text), images)
    } else {
        let status = call("printer_status", json!({"name": args.get("name")})).map(|o| o.text).unwrap_or_else(|e| format!("(printer status unavailable: {})", e.message));
        (format!("{status}\n\nRequest: {tool} {}", serde_json::to_string(args).unwrap_or_default()), Vec::new())
    };
    Ok(Some(ApprovalRequest { requester: requester.to_string(), tool: tool.to_string(), title, details, images }))
}

/// A few lines on what a print job contains, from `describe`: the visible objects with their
/// sizes and each filament they use with its spool. Also returns the project name.
pub fn describe_job(call: &mut dyn FnMut(&str, Value) -> Result<ToolOutput, ToolError>) -> (String, Option<String>) {
    let Ok(d) = call("describe", json!({})) else { return (String::new(), None) };
    let data = &d.data;
    let name = data.get("name").and_then(Value::as_str).map(str::to_string);
    let objects: Vec<&Value> = data.get("objects").and_then(Value::as_array).map(|a| a.iter().filter(|o| o.get("hidden") != Some(&json!(true))).collect()).unwrap_or_default();
    if objects.is_empty() {
        return (String::new(), name);
    }
    let size = |o: &Value| -> String {
        let s: Vec<f64> = o.get("size").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
        if s.len() == 3 {
            format!("{:.1} x {:.1} x {:.1} mm", s[0], s[1], s[2])
        } else {
            String::new()
        }
    };
    let mut t = format!("What: {} object{}", objects.len(), if objects.len() == 1 { "" } else { "s" });
    for o in objects.iter().take(6) {
        let n = o.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()).or_else(|| o.get("id").and_then(Value::as_str)).unwrap_or("?");
        t.push_str(&format!("\n  - {n}  {}", size(o)));
    }
    if objects.len() > 6 {
        t.push_str(&format!("\n  - and {} more", objects.len() - 6));
    }
    let used: std::collections::BTreeSet<u64> = objects.iter().filter_map(|o| o.get("filament").and_then(Value::as_u64)).collect();
    for slot in used {
        if let Some(f) = data.get("filaments").and_then(|f| f.get(slot.saturating_sub(1) as usize)) {
            let preset = f.get("preset").and_then(Value::as_str).unwrap_or("?");
            let preset = preset.split(" @").next().unwrap_or(preset);
            let spool = f.get("spool").and_then(Value::as_str).map(|s| format!(", spool {s}")).unwrap_or_default();
            t.push_str(&format!("\nFilament {slot}: {preset} {}{spool}", f.get("color").and_then(Value::as_str).unwrap_or("")));
        }
    }
    t.push_str("\n\n");
    (t, name)
}

/// Act on the user's answer to `req`: run the call (once, confirmed) or explain the refusal.
pub fn finish(
    req: &ApprovalRequest,
    args: Value,
    decision: Decision,
    call: &mut dyn FnMut(&str, Value) -> Result<ToolOutput, ToolError>,
    note: &mut dyn FnMut(&str, bool),
) -> Result<ToolOutput, ToolError> {
    let (tool, title, requester) = (req.tool.as_str(), req.title.as_str(), req.requester.as_str());
    match decision {
        Decision::Approved => {
            note(&format!("approved for {requester}: {title}"), true);
            let mut a = args;
            if matches!(tool, "print" | "printer_control" | "printer_skip_objects" | "printer_file" | "printer_set") {
                a["confirm"] = json!(true);
            }
            let mut out = call(tool, a)?;
            out.text = format!("The user approved this in only3d. {}", out.text);
            Ok(out)
        }
        Decision::Denied => {
            note(&format!("denied for {requester}: {title}"), false);
            Err(ToolError::new("denied_by_user", format!("the user declined: {title}"), "don't retry it; ask the user what they would like instead"))
        }
        Decision::TimedOut => {
            note(&format!("unanswered request from {requester}: {title}"), false);
            Err(ToolError::new("approval_timeout", format!("nobody answered the approval request ({title}) within 5 minutes"), "ask the user to look at the only3d window, then try again"))
        }
        Decision::Unavailable => Err(ToolError::new(
            "approval_required",
            format!("{title}: this needs the user's approval, and there is no way to ask them from here"),
            "ask the user to open the only3d window (run `only3d`) so they can approve it there, or to do it themselves",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(t: &str) -> ToolOutput {
        ToolOutput::text(t)
    }

    #[test]
    fn only_risky_printer_actions_are_gated() {
        assert!(gate("print", &json!({"confirm": true})).is_some());
        assert!(gate("print", &json!({})).is_none(), "the preflight alone is read-only");
        assert!(gate("printer_control", &json!({"action": "stop"})).is_some());
        assert!(gate("printer_control", &json!({"action": "pause"})).is_none());
        assert!(gate("printer_file", &json!({"action": "delete", "path": "/a.3mf"})).is_some());
        assert!(gate("printer_file", &json!({"action": "download", "path": "/a.3mf"})).is_none());
        assert!(gate("printer_ams", &json!({"action": "load"})).is_some());
        assert!(gate("printer_ams", &json!({"action": "set_slot"})).is_none());
        assert!(gate("add_object", &json!({})).is_none());
        assert!(gate("printer_alert", &json!({})).is_none(), "listing alerts is read-only");
        assert!(gate("printer_alert", &json!({"action": "resume"})).is_some());
        assert!(gate("printer_nozzle", &json!({"side": "left"})).is_some());
    }

    #[test]
    fn heating_needs_the_user_even_when_idle_and_cooling_does_not() {
        let g = |a: Value| gate("printer_set", &a);
        assert_eq!(g(json!({"nozzle_temp": 220})).as_deref(), Some("Heat the nozzle to 220 °C"));
        assert_eq!(g(json!({"nozzle_temp": 220, "nozzle": "left", "bed_temp": 60, "name": "h2d"})).as_deref(), Some("Heat the left nozzle to 220 °C and the bed to 60 °C on h2d"));
        assert_eq!(g(json!({"bed_temp": 60, "nozzle_temp": 0})).as_deref(), Some("Heat the bed to 60 °C"));
        assert!(g(json!({"nozzle_temp": "220"})).is_some(), "a number sent as text still heats");
        assert!(g(json!({"nozzle_temp": "hot"})).is_some(), "an unreadable value is asked about, not let through");
        assert!(g(json!({"nozzle_temp": 0, "bed_temp": 0})).is_none(), "turning heaters off is always allowed");
        assert!(g(json!({"nozzle_temp": null})).is_none());
        assert!(g(json!({"light": true, "speed": "silent"})).is_none());
        assert!(g(json!({"part_fan": 50})).is_none(), "fans on an idle printer are harmless");
        assert_eq!(g(json!({"part_fan": 50, "confirm": true})).as_deref(), Some("Change temperatures or fans during a running print"));
        assert_eq!(g(json!({"nozzle_temp": 0, "confirm": true})).as_deref(), Some("Change temperatures or fans during a running print"));
    }

    #[test]
    fn approved_heating_runs_once_confirmed() {
        let mut sent = Vec::new();
        let mut call = |t: &str, a: Value| {
            sent.push((t.to_string(), a.clone()));
            Ok(out("ok"))
        };
        let mut title = String::new();
        let r = gated_call("printer_set", json!({"bed_temp": 60}), "x", &mut call, &mut |r| {
            title = r.title;
            Decision::Approved
        }, &mut |_, _| {});
        assert!(r.unwrap().text.starts_with("The user approved"));
        assert_eq!(title, "Heat the bed to 60 °C");
        // asked once, then sent once with confirm, so a running print doesn't ask a second time
        assert_eq!(sent.iter().filter(|(t, _)| t == "printer_set").count(), 1);
        assert_eq!(sent.last().unwrap().1, json!({"bed_temp": 60, "confirm": true}));
    }

    #[test]
    fn approval_decides_not_the_agents_confirm() {
        let mut calls: Vec<(String, Value)> = Vec::new();
        let mut call = |t: &str, a: Value| {
            calls.push((t.to_string(), a.clone()));
            Ok(out(if a["confirm"] == json!(true) { "started" } else { "ready: [x] all good" }))
        };
        let mut asked = None;
        let mut approve = |r: ApprovalRequest| {
            asked = Some(r.details.clone());
            Decision::Denied
        };
        let mut notes = Vec::new();
        let mut note = |s: &str, ok: bool| notes.push((s.to_string(), ok));
        let r = gated_call("print", json!({"confirm": true}), "claude-code", &mut call, &mut approve, &mut note);
        assert_eq!(r.unwrap_err().code, "denied_by_user");
        // the user saw the preflight, and the print itself was never sent
        assert!(asked.as_deref().unwrap().contains("ready: [x] all good"));
        assert!(calls.iter().all(|(_, a)| a["confirm"] != json!(true)), "{calls:?}");
        assert!(calls.iter().any(|(t, a)| t == "print" && a["confirm"] == json!(false)));
        assert_eq!(notes, vec![("denied for claude-code: Start a print".to_string(), false)]);
    }

    #[test]
    fn a_print_whose_filament_needs_a_choice_goes_back_to_the_agent() {
        let mut calls: Vec<(String, Value)> = Vec::new();
        let mut call = |t: &str, a: Value| {
            calls.push((t.to_string(), a.clone()));
            Ok(ToolOutput::text("Not started: the filament needs the user's choice\n[?] filament 1 → A1 (tray 0)").with_data(json!({"ready": false, "needs_choice": true, "question": "Ask the user which to print with: A1 (tray 0) anyway (print {\"tray\": 0})"})))
        };
        let mut asked = false;
        let mut approve = |_: ApprovalRequest| {
            asked = true;
            Decision::Approved
        };
        let mut note = |_: &str, _: bool| {};
        let e = gated_call("print", json!({"confirm": true}), "claude-code", &mut call, &mut approve, &mut note).unwrap_err();
        assert_eq!(e.code, "filament_choice_needed");
        assert!(e.hint.contains("{\"tray\": 0}") && e.message.contains("A1 (tray 0)"), "{e:?}");
        // nobody was asked to approve it, and nothing was sent
        assert!(!asked);
        assert!(calls.iter().all(|(_, a)| a["confirm"] != json!(true)), "{calls:?}");
    }

    #[test]
    fn a_print_request_says_what_will_be_printed() {
        let mut call = |t: &str, a: Value| -> Result<ToolOutput, ToolError> {
            Ok(match t {
                "describe" => ToolOutput::text("").with_data(json!({"name": "first print tile",
                    "objects": [{"id": "tile", "name": "tile", "size": [40.0, 40.0, 3.0], "filament": 1, "hidden": false}],
                    "filaments": [{"preset": "Bambu PETG Basic @BBL H2D 0.4 nozzle", "color": "#7F7E83", "spool": "petg-7f7e83-1"}]})),
                "print" if a["confirm"] == json!(false) => out("Ready to print on 'h2d' (15 layers, ~6 min, 3.6 g):"),
                _ => out(""),
            })
        };
        let req = prepare("print", &json!({"confirm": true, "name": "h2d"}), "claude-code", &mut call).unwrap().unwrap();
        assert_eq!(req.title, "Print 'first print tile' on h2d");
        assert!(req.details.starts_with("What: 1 object\n  - tile  40.0 x 40.0 x 3.0 mm\nFilament 1: Bambu PETG Basic #7F7E83, spool petg-7f7e83-1"), "{}", req.details);
        assert!(req.details.contains("15 layers, ~6 min, 3.6 g"));
    }

    #[test]
    fn approved_runs_once_with_confirm_and_says_so() {
        let mut sent = Vec::new();
        let mut call = |t: &str, a: Value| {
            sent.push((t.to_string(), a["confirm"].clone()));
            Ok(out("ok"))
        };
        let r = gated_call("printer_control", json!({"action": "stop"}), "x", &mut call, &mut |_| Decision::Approved, &mut |_, _| {}).unwrap();
        assert!(r.text.starts_with("The user approved"));
        assert_eq!(sent, vec![("printer_status".to_string(), Value::Null), ("printer_control".to_string(), json!(true))]);
    }

    #[test]
    fn no_way_to_ask_means_no() {
        let mut call = |_: &str, _: Value| Ok(out("ok"));
        let r = gated_call("printer_file", json!({"action": "delete", "path": "/x"}), "x", &mut call, &mut |_| Decision::Unavailable, &mut |_, _| {});
        assert_eq!(r.unwrap_err().code, "approval_required");
    }
}
