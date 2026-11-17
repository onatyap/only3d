//! What happened in the session, in order, for agents to wait on.
//!
//! Each event has a sequence number. An agent calls `wait_for_event` with the last number it
//! saw and gets everything newer, or waits until something new arrives. Events cover:
//! - the user's and agents' edits and selection;
//! - finished slices;
//! - approval decisions;
//! - the printer: state changes, stages (heating, leveling, purifying the air...), alerts,
//!   layer progress, finish/failure and connection loss.
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
    /// printer_connection.
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

#[derive(Default)]
pub struct EventLog {
    inner: Mutex<Inner>,
    cv: Condvar,
}

impl EventLog {
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
