//! Starting a print (`print.project_file`).

use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::{Event, PrinterClient};
use crate::commands::{submission_id, PrintJob};
use crate::error::{Error, Result};
use crate::mqtt::QoS;
use crate::status::GcodeState;

/// What happened after a `project_file` request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StartPrintAck {
    pub sequence_id: String,
    pub task_id: String,
    /// The printer answered with `result: SUCCESS`. `false` means no answer
    /// arrived in time (some firmware does not echo); watch
    /// `status().gcode_state` for `PREPARE`/`RUNNING`.
    pub acknowledged: bool,
    pub result: Option<String>,
    pub reason: Option<String>,
    /// The exact payload that was published.
    pub payload: Value,
}

/// Start printing a `.gcode.3mf` that is already on the printer
/// (see [`crate::upload_file`]). Waits up to 15 s for the printer's answer.
pub fn start_print(client: &PrinterClient, job: &PrintJob) -> Result<StartPrintAck> {
    start_print_with(client, job, Duration::from_secs(15))
}

/// [`start_print`] with an explicit acknowledgement timeout.
///
/// Refuses (without sending anything) when the printer state is unknown or a
/// job is preparing/running/paused: firmware answers a busy printer with
/// `0500_4004` and on some models that cancels the running job (bambuddy #2598).
pub fn start_print_with(client: &PrinterClient, job: &PrintJob, ack_timeout: Duration) -> Result<StartPrintAck> {
    // Unit tests in this crate can never start a print, whatever they call.
    if cfg!(test) {
        return Err(Error::invalid_input("start_print is disabled under cfg(test)"));
    }
    let status = client
        .wait_for(|s| s.gcode_state != GcodeState::Unknown, Duration::from_secs(5))
        .map_err(|_| Error::invalid_input("printer state unknown (no status report yet); refusing to start a print"))?;
    if status.gcode_state.is_busy() {
        return Err(Error::PrinterBusy {
            state: status.gcode_state.to_string(),
        });
    }
    // The model decides URL form and dual-nozzle fields; give get_version a moment.
    let mut model = client.model_info();
    if model.is_none() {
        let _ = client.wait_for(|s| s.product_name.is_some(), Duration::from_secs(3));
        model = client.model_info();
    }

    let sequence_id = client.next_sequence_id();
    let task_id = submission_id();
    let payload = job.to_payload(&sequence_id, model, &task_id)?;
    let url = payload["print"]["url"].clone();

    let rx = client.subscribe();
    client.publish_json(&payload, QoS::AtLeastOnce)?;

    let deadline = Instant::now() + ack_timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(Event::Message(m)) => {
                let Some(p) = m.get("print") else { continue };
                if p.get("command").and_then(Value::as_str) != Some("project_file") {
                    continue;
                }
                let same_seq = p.get("sequence_id").and_then(Value::as_str) == Some(sequence_id.as_str());
                let same_url = p.get("url") == Some(&url);
                if !(same_seq || same_url) {
                    continue;
                }
                let result = p.get("result").and_then(Value::as_str).map(str::to_string);
                let reason = p.get("reason").and_then(Value::as_str).map(str::to_string);
                let Some(r) = result.clone() else { continue };
                if !r.eq_ignore_ascii_case("success") {
                    return Err(Error::PrintRejected {
                        result: r,
                        reason: reason.unwrap_or_default(),
                    });
                }
                return Ok(StartPrintAck {
                    sequence_id,
                    task_id,
                    acknowledged: true,
                    result,
                    reason,
                    payload,
                });
            }
            Ok(Event::Closed) | Err(RecvTimeoutError::Disconnected) => return Err(Error::NotConnected),
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => {
                return Ok(StartPrintAck {
                    sequence_id,
                    task_id,
                    acknowledged: false,
                    result: None,
                    reason: None,
                    payload,
                })
            }
        }
    }
}
