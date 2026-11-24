//! A single background thread that runs tool calls (and a few direct reads) against the shared
//! session, so the UI never waits on the studio mutex while a slice, a printer round-trip or an
//! agent's long call is in progress.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::egui;
use only3d_studio::{Actor, Studio, ToolError, ToolOutput};
use serde_json::Value;

/// What to do with a result when it comes back.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// Show the error as a toast; ignore success.
    Plain,
    /// Coalesced edit stream (drag values); the key frees the throttle slot.
    Throttled(String),
    AddObject,
    ShapeApply(String),
    Slice,
    PrintPreflight,
    /// The print dialog's preflight again, for the trays the user picked (`print`'s `trays`).
    PrintRecheck(Vec<i32>),
    PrintConfirm,
    PrinterStatus,
    Export,
    Save,
    Presets(String),
    /// A preset was created / imported / exported / deleted: refresh that kind ("" = all).
    PresetChanged(String),
    Effective(u64),
    /// `settings_catalog` for this project revision.
    Catalog(u64),
    /// Fallback `settings_schema` for this project revision.
    Schema(u64),
    /// A drag in the viewport was committed (errors drop the held preview).
    Commit,
    PrinterInfo,
    /// `printer_alert` (list) for the alert codes of the latest status.
    PrinterAlerts(String),
    /// The palette's run-tool form.
    ToolForm,
    /// Show the first line of the result as a toast.
    Report,
    /// A button of the AMS dialog: the answer goes to the dialog (a toast when it's closed).
    Ams,
    /// The drying plan for these `printer_ams` arguments (JSON), for the AMS dialog.
    AmsPlan(String),
    /// Spool inventory edits (the cached inventory is re-read).
    Spools,
    /// Ignore the result, errors included.
    Quiet,
    /// `print_detail` for the Prints section of the workspace.
    PrintDetail(String),
    /// A workspace change (root, import from the tree): refresh the file listing.
    Workspace,
    /// `fit show`, for the Spools panel.
    FitShown,
    /// A project version fetched to print it again.
    Reprint,
    /// Copies made by ⌥-drag or ⌘D: select them and remember them for the next ⌘D.
    Copied,
    /// `project_versions` for File > Revert to Version.
    Versions,
    /// A project version fetched to restore it.
    Restore(i64),
    /// First-launch printer setup: "discover", "add" or "status".
    Onboard(String),
    /// The export panel's preview render, for the choices with this hash.
    ExportPreview(u64),
    /// An export from the panel: toast, and show the file in Finder when true.
    Exported(bool),
}

pub type ReadFn = Box<dyn FnOnce(&mut Studio) -> Result<ToolOutput, ToolError> + Send>;

enum Job {
    Tool { tool: String, args: Value, kind: Kind },
    Read { f: ReadFn, kind: Kind, label: String },
}

pub struct Done {
    pub kind: Kind,
    pub tool: String,
    pub result: Result<ToolOutput, ToolError>,
}

pub struct Worker {
    tx: Sender<Job>,
    rx: Receiver<Done>,
    pub inflight: Vec<(String, Kind, Instant)>,
}

impl Worker {
    pub fn new(studio: Arc<Mutex<Studio>>, ctx: egui::Context) -> Self {
        let (tx, jobs) = channel::<Job>();
        let (done_tx, rx) = channel::<Done>();
        std::thread::Builder::new()
            .name("only3d-gui-worker".into())
            .spawn(move || {
                for job in jobs {
                    let (kind, tool, result) = match job {
                        Job::Tool { tool, args, kind } => {
                            let r = match studio.lock() {
                                Ok(mut s) => {
                                    let r = s.call(&tool, args, Actor::User);
                                    // warm the evaluation cache here, not on the UI thread
                                    let _ = s.evaluated();
                                    r
                                }
                                Err(_) => Err(ToolError::new("session_poisoned", "the session crashed", "restart only3d")),
                            };
                            (kind, tool, r)
                        }
                        Job::Read { f, kind, label } => {
                            let r = match studio.lock() {
                                Ok(mut s) => f(&mut s),
                                Err(_) => Err(ToolError::new("session_poisoned", "the session crashed", "restart only3d")),
                            };
                            (kind, label, r)
                        }
                    };
                    if done_tx.send(Done { kind, tool, result }).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            })
            .expect("spawn worker");
        Worker { tx, rx, inflight: Vec::new() }
    }

    pub fn call(&mut self, tool: &str, args: Value, kind: Kind) {
        self.inflight.push((tool.to_string(), kind.clone(), Instant::now()));
        let _ = self.tx.send(Job::Tool { tool: tool.to_string(), args, kind });
    }

    pub fn read(&mut self, label: &str, kind: Kind, f: ReadFn) {
        self.inflight.push((label.to_string(), kind.clone(), Instant::now()));
        let _ = self.tx.send(Job::Read { f, kind, label: label.to_string() });
    }

    pub fn poll(&mut self) -> Vec<Done> {
        let mut out = Vec::new();
        while let Ok(d) = self.rx.try_recv() {
            if let Some(i) = self.inflight.iter().position(|(t, k, _)| *t == d.tool && *k == d.kind) {
                self.inflight.remove(i);
            }
            out.push(d);
        }
        out
    }

    pub fn busy_with(&self, pred: impl Fn(&Kind) -> bool) -> bool {
        self.inflight.iter().any(|(_, k, _)| pred(k))
    }

    /// The longest-running job, if it has been running long enough to deserve a spinner.
    pub fn slow_job(&self) -> Option<(&str, f32)> {
        self.inflight.iter().map(|(t, _, at)| (t.as_str(), at.elapsed().as_secs_f32())).filter(|(_, s)| *s > 0.12).max_by(|a, b| a.1.total_cmp(&b.1))
    }
}
