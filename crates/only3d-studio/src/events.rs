//! What happened in the session, in order, for agents to wait on.
//!
//! Each event has a sequence number. An agent calls `wait_for_event` with the last number it
//! saw and gets everything newer, or waits until something new arrives. Events cover:
//! - the user's and agents' edits and selection;
//! - finished slices;
//! - approval decisions;
//! - the printer: state changes, stages (heating, leveling, purifying the air...), alerts,
//!   layer progress, finish/failure and connection loss;
//! - the AMS: drying starting, finishing or stopping, and spools going in, out or being read.
//!
//! The log is shared (`Arc`), so the printer watcher thread and waiting callers never need the
//! session lock.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};

use crate::tools::Actor;

const KEEP: usize = 1000;

#[derive(Clone, Debug, Serialize)]
pub struct StudioEvent {
    pub seq: u64,
    pub at_ms: u128,
    /// project_changed, selection_changed, sliced, approval, printer_state, printer_stage,
    /// printer_nozzle, printer_alert, print_progress, print_finished, print_failed,
    /// printer_connection, ams_drying, ams_slot.
    pub kind: String,
    /// Who caused it, when known (the user or an agent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<Actor>,
    pub summary: String,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub data: Value,
}

#[derive(Default)]
struct Inner {
    next: u64,
    items: VecDeque<StudioEvent>,
}

/// Called when something happened, from whichever thread caused it.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
pub struct EventLog {
    inner: Mutex<Inner>,
    cv: Condvar,
    waker: Mutex<Option<Waker>>,
}

impl EventLog {
    /// Run `f` after every event and every finished tool call. The window uses it to redraw only
    /// when something changed, instead of polling.
    pub fn set_waker(&self, f: Waker) {
        *self.waker.lock().unwrap_or_else(|p| p.into_inner()) = Some(f);
    }

    /// Tell the waker that the session changed (an event, a tool call, a journal entry).
    pub fn wake(&self) {
        let w = self.waker.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(w) = w {
            w();
        }
    }

    pub fn push(&self, kind: &str, by: Option<Actor>, summary: impl Into<String>, data: Value) -> u64 {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.next += 1;
        let seq = g.next;
        g.items.push_back(StudioEvent { seq, at_ms: crate::now_ms(), kind: kind.to_string(), by, summary: summary.into(), data });
        while g.items.len() > KEEP {
            g.items.pop_front();
        }
        drop(g);
        self.cv.notify_all();
        self.wake();
        seq
    }

    /// Sequence number of the newest event (0 when there is none).
    pub fn last_seq(&self) -> u64 {
        self.inner.lock().unwrap_or_else(|p| p.into_inner()).next
    }

    /// Events after `since` whose kind is in `kinds` (all kinds when empty).
    pub fn since(&self, since: u64, kinds: &[String]) -> Vec<StudioEvent> {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.items.iter().filter(|e| e.seq > since && (kinds.is_empty() || kinds.contains(&e.kind))).cloned().collect()
    }

    /// [`EventLog::since`], waiting up to `timeout` for at least one event.
    pub fn wait(&self, since: u64, kinds: &[String], timeout: Duration) -> Vec<StudioEvent> {
        let deadline = Instant::now() + timeout;
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            let found: Vec<StudioEvent> = g.items.iter().filter(|e| e.seq > since && (kinds.is_empty() || kinds.contains(&e.kind))).cloned().collect();
            if !found.is_empty() {
                return found;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else { return Vec::new() };
            g = self.cv.wait_timeout(g, left).unwrap_or_else(|p| p.into_inner()).0;
        }
    }
}

/// `wait_for_event`: events after `since` (default: from now on), of `kinds` (default: all),
/// waiting up to `timeout_s` (default 120 s, max 1800 s) for the first one.
pub fn wait_tool(log: &EventLog, args: &Value) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
    let since = args.get("since").and_then(Value::as_u64).unwrap_or_else(|| log.last_seq());
    let kinds: Vec<String> = args.get("kinds").and_then(Value::as_array).map(|a| a.iter().filter_map(|k| k.as_str().map(str::to_string)).collect()).unwrap_or_default();
    let timeout = Duration::from_secs(args.get("timeout_s").and_then(Value::as_u64).unwrap_or(120).min(1800));
    let t0 = Instant::now();
    let events = log.wait(since, &kinds, timeout);
    let next = events.last().map(|e| e.seq).unwrap_or(since);
    let text = if events.is_empty() {
        format!("No new events in {} s (since #{since}). Call again with since: {next} to keep waiting.", t0.elapsed().as_secs())
    } else {
        let mut t = events.iter().map(|e| format!("#{} {}{}: {}", e.seq, e.kind, e.by.map(|b| format!(" by {b:?}").to_lowercase()).unwrap_or_default(), e.summary)).collect::<Vec<_>>().join("\n");
        t.push_str(&format!("\nNext: wait_for_event with since: {next}."));
        t
    };
    Ok(crate::tools::ToolOutput::text(text).with_data(json!({"events": events, "next_since": next, "timed_out": events.is_empty()})))
}

/// Watch a printer connection and log what changes, until the client goes away.
pub fn watch_printer(log: Arc<EventLog>, printer: String, rx: std::sync::mpsc::Receiver<only3d_bambu::Event>) {
    let _ = std::thread::Builder::new().name(format!("only3d-watch-{printer}")).spawn(move || {
        let mut prev: Option<only3d_bambu::PrinterStatus> = None;
        let mut timing = crate::timing::Tracker::default();
        for ev in rx {
            match ev {
                only3d_bambu::Event::StateChanged(st) => {
                    if let Some(p) = &prev {
                        diff(&log, &printer, p, &st);
                        timing.observe(&printer, p, &st);
                    }
                    prev = Some(*st);
                }
                only3d_bambu::Event::Disconnected { message, will_reconnect, .. } => {
                    log.push("printer_connection", None, format!("{printer}: connection lost ({message}){}", if will_reconnect { ", reconnecting" } else { "" }), json!({"connected": false}));
                }
                only3d_bambu::Event::Connected if prev.is_some() => {
                    log.push("printer_connection", None, format!("{printer}: connected again"), json!({"connected": true}));
                }
                only3d_bambu::Event::Closed => break,
                _ => {}
            }
        }
    });
}

fn diff(log: &EventLog, name: &str, a: &only3d_bambu::PrinterStatus, b: &only3d_bambu::PrinterStatus) {
    let (sa, sb) = (a.gcode_state.as_str(), b.gcode_state.as_str());
    let job = b.subtask_name.clone().unwrap_or_default();
    if sa != sb {
        log.push("printer_state", None, format!("{name}: {sa} -> {sb}{}", if job.is_empty() { String::new() } else { format!(" ('{job}')") }), json!({"from": sa, "to": sb, "job": job}));
        match sb {
            "FINISH" => {
                log.push("print_finished", None, format!("{name}: '{job}' finished"), json!({"job": job}));
            }
            "FAILED" => {
                log.push("print_failed", None, format!("{name}: '{job}' stopped or failed"), json!({"job": job, "print_error": b.print_error}));
            }
            _ => {}
        }
    }
    if a.active_extruder != b.active_extruder && b.nozzles.len() > 1 {
        if let Some(i) = b.active_extruder {
            let side = b.nozzles.iter().find(|n| n.index == i).and_then(|n| n.side.clone()).unwrap_or_else(|| format!("nozzle {i}"));
            log.push("printer_nozzle", None, format!("{name}: {side} nozzle active"), json!({"active_extruder": i, "side": side}));
        }
    }
    if a.stage_name != b.stage_name {
        if let Some(stage) = b.stage_name.as_deref().filter(|s| *s != "Printing") {
            log.push("printer_stage", None, format!("{name}: {stage}"), json!({"stage": b.stage, "name": stage}));
        }
    }
    // new alerts: HMS entries or a print error that were not there before
    let had: Vec<(u32, u32)> = a.hms.iter().map(|h| (h.attr, h.code)).collect();
    for h in b.hms.iter().filter(|h| !had.contains(&(h.attr, h.code))) {
        let text = crate::printer::hms_text(&h.id).unwrap_or_default();
        log.push("printer_alert", None, format!("{name}: HMS {} ({}) {}", h.id, h.severity, text), json!({"hms": h, "text": text}));
    }
    if let Some(pe) = b.print_error.as_ref().filter(|_| a.print_error != b.print_error) {
        log.push("printer_alert", None, format!("{name}: print error {}", pe.id), json!({"print_error": pe}));
    }
    if b.layer != a.layer && b.gcode_state.is_busy() {
        if let Some(l) = b.layer.filter(|l| *l > 0) {
            log.push("print_progress", None, format!("{name}: layer {l}/{} ({}%)", b.total_layers.unwrap_or(0), b.progress_percent.unwrap_or(0)), json!({"layer": l, "total_layers": b.total_layers, "percent": b.progress_percent, "remaining_minutes": b.remaining_minutes}));
        }
    }
    for ub in &b.ams {
        let Some(ua) = a.ams.iter().find(|u| u.id == ub.id) else { continue };
        let unit = if ub.name.is_empty() { ub.id.to_string() } else { ub.name.clone() };
        ams_drying(log, name, &unit, ua, ub);
        for tb in &ub.trays {
            if let Some(ta) = ua.trays.iter().find(|t| t.slot_id == tb.slot_id) {
                ams_slot(log, name, b, ta, tb);
            }
        }
    }
}

/// `ams_drying`: a unit started drying, finished, was stopped or hit an error. Only state
/// changes count; the minutes ticking down and the heating/dehumidifying phases don't.
fn ams_drying(log: &EventLog, name: &str, unit: &str, ua: &only3d_bambu::AmsUnit, ub: &only3d_bambu::AmsUnit) {
    let Some(d) = ub.drying.as_ref() else { return };
    let was = ua.drying.as_ref();
    let (from, to) = (was.map_or("off", |w| w.state.as_str()), d.state.as_str());
    if from == to {
        return;
    }
    // leaving "drying": it ran its time (finished) or was cut short (stopped, error)
    let left = was.and_then(|w| w.remaining_minutes).filter(|m| *m > 2);
    let finished = (from == "drying").then_some(left.is_none());
    let cut = |what: &str| match left {
        Some(m) => format!("drying stopped with {} left{what}", only3d_bambu::minutes_text(m)),
        None => format!("drying finished{what}"),
    };
    let what = match to {
        "cooling" => cut(", cooling down"),
        "stopping" if from == "drying" => cut(""),
        "off" if from == "drying" => cut(""),
        "off" if from == "cooling" => "done cooling down; drying is over".into(),
        "off" if from == "stopping" => "drying stopped".into(),
        "off" => "not drying".into(),
        _ => d.summary.clone(),
    };
    log.push(
        "ams_drying",
        None,
        format!("{name}: AMS {unit}: {what}"),
        json!({"unit": unit, "ams_id": ub.id, "from": from, "to": to, "finished": finished, "drying": d, "humidity_percent": ub.humidity_percent}),
    );
}

/// `ams_slot`: a spool went in or came out of an AMS slot, the AMS finished reading one, or the
/// filament set for a slot changed. Remaining-% updates during a print don't count.
fn ams_slot(log: &EventLog, name: &str, st: &only3d_bambu::PrinterStatus, ta: &only3d_bambu::AmsTray, tb: &only3d_bambu::AmsTray) {
    let filament = |t: &only3d_bambu::AmsTray| (t.filament_type.clone(), t.sub_brand.clone(), t.color.clone(), t.tray_info_idx.clone());
    let new_filament = filament(ta) != filament(tb);
    let holds = || {
        let what = tb.sub_brand.clone().or_else(|| tb.filament_type.clone()).unwrap_or_else(|| "unknown filament".into());
        let colour = tb.color.as_deref().and_then(crate::trays::hex).map(|c| format!(" {c}")).unwrap_or_default();
        let left = tb.remain_percent.map(|p| format!(", {p}% left")).unwrap_or_default();
        format!("{what}{colour}{left}")
    };
    let (change, what) = match (ta.loaded, tb.loaded) {
        (false, true) if tb.reading => ("inserted", "spool inserted, reading it".to_string()),
        // the slot's old fields can outlive its spool: name the filament only when it changed
        (false, true) if new_filament => ("inserted", format!("spool inserted: {}", holds())),
        (false, true) => ("inserted", "spool inserted".to_string()),
        (true, false) => ("removed", "spool removed".to_string()),
        (true, true) if ta.reading && !tb.reading => ("read", format!("read {}", holds())),
        (true, true) if !tb.reading && new_filament => ("changed", format!("now {}", holds())),
        _ => return,
    };
    let at = crate::trays::label(st, tb);
    log.push(
        "ams_slot",
        None,
        format!("{name}: AMS {at}: {what}"),
        json!({"slot": at, "ams_id": tb.ams_id, "slot_id": tb.slot_id, "location": crate::spools::tray_location(tb.ams_id, tb.slot_id), "change": change, "tray": tb}),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nozzle_switch_is_an_event() {
        use only3d_bambu::{NozzleStatus, PrinterStatus};
        let nozzles = vec![NozzleStatus { index: 0, side: Some("right".into()), ..Default::default() }, NozzleStatus { index: 1, side: Some("left".into()), ..Default::default() }];
        let a = PrinterStatus { nozzles: nozzles.clone(), active_extruder: Some(0), ..Default::default() };
        let b = PrinterStatus { nozzles, active_extruder: Some(1), ..Default::default() };
        let log = EventLog::default();
        diff(&log, "h2d", &a, &b);
        let ev = log.since(0, &[]);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].kind, "printer_nozzle");
        assert_eq!(ev[0].summary, "h2d: left nozzle active");
    }

    /// A printer with one AMS 2 Pro ("A") whose drying and first slot are as given.
    fn with_ams(drying: Option<only3d_bambu::AmsDrying>, a1: only3d_bambu::AmsTray) -> only3d_bambu::PrinterStatus {
        let unit = only3d_bambu::AmsUnit { id: 0, name: "A".into(), model: Some("AMS 2 Pro".into()), drying, trays: vec![a1], ..Default::default() };
        only3d_bambu::PrinterStatus { ams: vec![unit], ..Default::default() }
    }

    /// The summaries `diff` logs, one status to the next.
    fn summaries(steps: &[only3d_bambu::PrinterStatus]) -> Vec<String> {
        let log = EventLog::default();
        for w in steps.windows(2) {
            diff(&log, "h2d", &w[0], &w[1]);
        }
        log.since(0, &[]).into_iter().map(|e| e.summary).collect()
    }

    #[test]
    fn ams_drying_starting_and_finishing_are_events() {
        let dry = |state: &str, phase: Option<&str>, left: Option<u32>| {
            let mut d = only3d_bambu::AmsDrying { state: state.into(), phase: phase.map(String::from), ..Default::default() };
            if matches!(state, "checking" | "drying") {
                (d.filament, d.temperature_c, d.duration_hours, d.remaining_minutes) = (Some("PLA".into()), Some(50), Some(12), left);
            }
            d.summary = match state {
                "checking" => "starting to dry PLA at 50 °C, 12 h".into(),
                "drying" => format!("drying PLA at 50 °C ({}), …", phase.unwrap_or("")),
                s => s.into(),
            };
            Some(d)
        };
        let a1 = only3d_bambu::AmsTray::default;
        let steps = [
            with_ams(dry("off", None, None), a1()),
            with_ams(dry("checking", None, Some(720)), a1()),
            with_ams(dry("drying", Some("heating"), Some(719)), a1()),
            // minutes ticking down and the phase changing are not events
            with_ams(dry("drying", Some("dehumidifying"), Some(400)), a1()),
            with_ams(dry("drying", Some("dehumidifying"), Some(1)), a1()),
            with_ams(dry("cooling", None, None), a1()),
            with_ams(dry("off", None, None), a1()),
            // a second job, stopped from the printer's screen with hours to go
            with_ams(dry("drying", Some("heating"), Some(300)), a1()),
            with_ams(dry("off", None, None), a1()),
        ];
        assert_eq!(
            summaries(&steps),
            [
                "h2d: AMS A: starting to dry PLA at 50 °C, 12 h",
                "h2d: AMS A: drying PLA at 50 °C (heating), …",
                "h2d: AMS A: drying finished, cooling down",
                "h2d: AMS A: done cooling down; drying is over",
                "h2d: AMS A: drying PLA at 50 °C (heating), …",
                "h2d: AMS A: drying stopped with 5 h left",
            ]
        );
        let log = EventLog::default();
        diff(&log, "h2d", &steps[4], &steps[5]);
        diff(&log, "h2d", &steps[7], &steps[8]);
        let finished: Vec<Value> = log.since(0, &["ams_drying".into()]).iter().map(|e| e.data["finished"].clone()).collect();
        assert_eq!(finished, [json!(true), json!(false)]);
    }

    #[test]
    fn ams_slots_report_spools_going_in_being_read_and_coming_out() {
        use only3d_bambu::AmsTray;
        let empty = AmsTray { filament_type: Some("PLA".into()), color: Some("995F11FF".into()), ..Default::default() };
        let pla_basic = AmsTray { loaded: true, filament_type: Some("PLA".into()), sub_brand: Some("PLA Basic".into()), color: Some("000000FF".into()), tray_info_idx: Some("GFA00".into()), remain_percent: Some(95), ..Default::default() };
        let steps = [
            with_ams(None, empty.clone()),
            with_ams(None, AmsTray { loaded: true, reading: true, ..empty.clone() }),
            with_ams(None, pla_basic.clone()),
            // what's left going down while printing is not an event
            with_ams(None, AmsTray { remain_percent: Some(90), ..pla_basic.clone() }),
            with_ams(None, AmsTray { loaded: false, ..pla_basic.clone() }),
            // a spool without a tag: the slot still shows the old spool's filament
            with_ams(None, AmsTray { loaded: true, ..pla_basic.clone() }),
            // the user sets its filament on the printer's screen
            with_ams(None, AmsTray { loaded: true, filament_type: Some("PETG".into()), sub_brand: None, color: Some("FFFFFFFF".into()), tray_info_idx: Some("GFG99".into()), remain_percent: None, ..Default::default() }),
        ];
        assert_eq!(
            summaries(&steps),
            [
                "h2d: AMS A1: spool inserted, reading it",
                "h2d: AMS A1: read PLA Basic #000000, 95% left",
                "h2d: AMS A1: spool removed",
                "h2d: AMS A1: spool inserted",
                "h2d: AMS A1: now PETG #FFFFFF",
            ]
        );
        let log = EventLog::default();
        diff(&log, "h2d", &steps[1], &steps[2]);
        let e = &log.since(0, &["ams_slot".into()])[0];
        assert_eq!((e.data["change"].as_str(), e.data["location"].as_str()), (Some("read"), Some("ams0:1")));
    }

    #[test]
    fn the_waker_hears_every_event_and_every_tool_call() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let woke = Arc::new(AtomicUsize::new(0));
        let mut s = crate::Studio::new();
        let w = woke.clone();
        s.events.set_waker(Arc::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        }));
        s.events.push("print_progress", None, "layer 3", Value::Null);
        assert_eq!(woke.load(Ordering::SeqCst), 1, "an event");
        s.call("add_object", serde_json::json!({"id": "a", "shape": {"type": "box", "size": [10, 10, 10]}}), Actor::Agent).unwrap();
        assert_eq!(woke.load(Ordering::SeqCst), 2, "an edit (its project_changed event)");
        s.call("describe", serde_json::json!({}), Actor::Agent).unwrap();
        assert_eq!(woke.load(Ordering::SeqCst), 3, "a read: the journal the window shows grew");
        assert!(s.call("remove_object", serde_json::json!({"id": "nope"}), Actor::Agent).is_err());
        assert_eq!(woke.load(Ordering::SeqCst), 4, "a failed call is journaled too");
    }

    #[test]
    fn waits_for_new_events_of_the_right_kind() {
        let log = Arc::new(EventLog::default());
        log.push("project_changed", Some(Actor::User), "edit", Value::Null);
        let since = log.last_seq();
        let l2 = log.clone();
        let t = std::thread::spawn(move || l2.wait(since, &["print_finished".to_string()], Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(50));
        log.push("print_progress", None, "layer 3", Value::Null);
        log.push("print_finished", None, "done", Value::Null);
        let got = t.join().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, "print_finished");
        assert!(log.wait(log.last_seq(), &[], Duration::from_millis(30)).is_empty(), "times out when nothing new arrives");
    }
}
