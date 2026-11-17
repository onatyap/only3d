//! One error type for the whole crate.
//!
//! Every variant has a stable machine-readable [`Error::code`] and a
//! [`Error::hint`] that tells a human (or an AI agent driving the app) what to
//! try next. The `Display` text carries the specifics (host, reply text, ...).

use std::io;

/// Crate-wide result alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can go wrong talking to a Bambu Lab printer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// TCP connect failed (refused, no route, DNS failure, ...).
    #[error("cannot reach {host}:{port}: {source}")]
    Unreachable {
        host: String,
        port: u16,
        #[source]
        source: io::Error,
    },

    /// An operation did not finish in time.
    #[error("timed out: {what}")]
    Timeout { what: String },

    /// The printer rejected the LAN access code (MQTT CONNACK 4/5 or FTP 530).
    #[error("authentication failed: {detail}")]
    AuthFailed { detail: String },

    /// The printer presented a certificate whose SHA-256 differs from the pinned one.
    #[error("TLS certificate fingerprint mismatch: expected {expected}, got {actual}")]
    TlsFingerprintMismatch { expected: String, actual: String },

    /// Any other TLS failure (handshake, unsupported cipher, bad record, ...).
    #[error("TLS error: {detail}")]
    Tls { detail: String },

    /// The peer violated MQTT/FTP framing or sent something we cannot parse.
    #[error("protocol error: {detail}")]
    Protocol { detail: String },

    /// The printer accepted the connection but refused the subscription, or
    /// dropped us right after subscribing (usually a wrong serial number).
    #[error("printer rejected subscription for serial {serial}: {detail}")]
    WrongSerial { serial: String, detail: String },

    /// An established connection dropped.
    #[error("connection lost: {detail}")]
    ConnectionLost { detail: String },

    /// A command was issued while the MQTT session is down (e.g. reconnecting).
    #[error("not connected to the printer")]
    NotConnected,

    /// Unexpected FTP reply.
    #[error("FTP {op} failed: {code} {message}")]
    Ftp {
        op: String,
        code: u16,
        message: String,
    },

    /// FTP 550 on a path that should exist.
    #[error("file not found on printer: {path}")]
    FileNotFound { path: String },

    /// FTP 452/552/553: storage full, missing or read-only.
    #[error("printer storage error during {op}: {code} {message}")]
    Storage {
        op: String,
        code: u16,
        message: String,
    },

    /// Refused locally: the printer is busy with another job.
    #[error("printer is busy (gcode_state = {state})")]
    PrinterBusy { state: String },

    /// The printer answered a `project_file` request with a failure.
    #[error("printer rejected the print: result={result} reason={reason}")]
    PrintRejected { result: String, reason: String },

    /// The printer answered a command with `result != success`.
    #[error("printer rejected command {command}: result={result} reason={reason}")]
    CommandRejected {
        command: String,
        result: String,
        reason: String,
    },

    /// The [`crate::PrinterConfig`] is unusable (empty host, bad characters, ...).
    #[error("invalid printer config: {detail}")]
    InvalidConfig { detail: String },

    /// A caller-supplied argument is invalid (bad file name, out-of-range value, ...).
    #[error("invalid input: {detail}")]
    InvalidInput { detail: String },

    /// The upload was cancelled by the progress callback.
    #[error("upload cancelled after {sent} bytes")]
    Cancelled { sent: u64 },

    /// Local socket setup for discovery failed.
    #[error("discovery unavailable: {detail}")]
    Discovery { detail: String },

    /// Any other I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// JSON (de)serialisation error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl Error {
    /// Stable, machine-readable error code. Never changes for a given variant.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Unreachable { .. } => "unreachable",
            Error::Timeout { .. } => "timeout",
            Error::AuthFailed { .. } => "auth_failed",
            Error::TlsFingerprintMismatch { .. } => "tls_fingerprint_mismatch",
            Error::Tls { .. } => "tls",
            Error::Protocol { .. } => "protocol",
            Error::WrongSerial { .. } => "wrong_serial",
            Error::ConnectionLost { .. } => "connection_lost",
            Error::NotConnected => "not_connected",
            Error::Ftp { .. } => "ftp",
            Error::FileNotFound { .. } => "file_not_found",
            Error::Storage { .. } => "storage",
            Error::PrinterBusy { .. } => "printer_busy",
            Error::PrintRejected { .. } => "print_rejected",
            Error::CommandRejected { .. } => "command_rejected",
            Error::InvalidConfig { .. } => "invalid_config",
            Error::InvalidInput { .. } => "invalid_input",
            Error::Cancelled { .. } => "cancelled",
            Error::Discovery { .. } => "discovery_unavailable",
            Error::Io(_) => "io",
            Error::Json(_) => "protocol",
        }
    }

    /// What to do next, phrased for a person or an agent.
    pub fn hint(&self) -> &'static str {
        match self {
            Error::Unreachable { .. } => {
                "Check the printer is powered on and on the same network, the IP is current \
                 (it may have changed via DHCP; run discovery), and LAN mode / LAN-only is \
                 enabled on the printer screen (Settings > Network). MQTT uses TCP 8883, FTPS TCP 990."
            }
            Error::Timeout { .. } => {
                "The printer did not answer in time. Check Wi-Fi signal and that LAN mode is \
                 enabled, then retry. If it was a status wait, the printer may simply not have \
                 reached that state yet."
            }
            Error::AuthFailed { .. } => {
                "The LAN access code was rejected. Read the current 8-character access code on the \
                 printer screen (Settings > Network / WLAN > Access Code; it changes when LAN mode is \
                 toggled) and update the printer config. Username is always 'bblp'."
            }
            Error::TlsFingerprintMismatch { .. } => {
                "The printer's TLS certificate changed since it was pinned. This happens after a \
                 mainboard replacement or a factory reset, but can also mean another device answers \
                 on this IP. Verify the printer (serial on the screen), then clear or update \
                 cert_sha256 in the printer config to re-pin."
            }
            Error::Tls { .. } => {
                "The TLS handshake failed. Make sure the IP really is a Bambu printer in LAN mode and \
                 that nothing (VPN, proxy, captive portal) intercepts the connection."
            }
            Error::Protocol { .. } => {
                "The printer sent something unexpected. Retry once; if it persists, update the printer \
                 firmware / this app and report the raw message."
            }
            Error::WrongSerial { .. } => {
                "Check the serial number in the printer config: it must match the printer exactly \
                 (Settings > Device on the printer screen, or the USN of discovery)."
            }
            Error::ConnectionLost { .. } => {
                "The connection dropped. The client reconnects automatically when enabled; otherwise \
                 reconnect. Frequent drops usually mean weak Wi-Fi or too many LAN clients (the \
                 printer allows only a few concurrent MQTT connections; close Bambu Studio/Handy LAN sessions)."
            }
            Error::NotConnected => {
                "The MQTT session is down (reconnecting or closed). Wait for a Connected event or \
                 reconnect, then resend. Commands are never queued across reconnects on purpose."
            }
            Error::Ftp { .. } => {
                "The printer's FTP server refused the operation. Retry once; make sure no other upload \
                 to the same printer is running (one FTPS session at a time is safest)."
            }
            Error::FileNotFound { .. } => {
                "The file is not on the printer storage. List files to see the exact name, or upload it first."
            }
            Error::Storage { .. } => {
                "The printer storage refused the write: insert / reseat the USB drive (H2D) or SD card (FTPS serves \
                 it), free space, or format it on the printer."
            }
            Error::PrinterBusy { .. } => {
                "A job is running, preparing or paused. Wait for it to finish (gcode_state FINISH / \
                 FAILED / IDLE) or stop it on the printer before starting another print."
            }
            Error::PrintRejected { .. } => {
                "The printer refused the job. Check the file was uploaded completely (file list, size), \
                 the plate number exists in the .gcode.3mf, the AMS mapping matches loaded filament, \
                 and the file was sliced for this printer model and nozzle."
            }
            Error::CommandRejected { .. } => {
                "The printer refused the command in its current state. Check status and HMS errors, then retry."
            }
            Error::InvalidConfig { .. } => {
                "Fix the printer config: host (IP), serial and the 8-character LAN access code are required."
            }
            Error::InvalidInput { .. } => "Fix the argument named in the error message and retry.",
            Error::Cancelled { .. } => {
                "The upload was cancelled; a partial file may remain on the printer. Re-upload before printing."
            }
            Error::Discovery { .. } => {
                "UDP 2021/1990 could not be opened (another app such as Bambu Studio may hold them, or \
                 the OS firewall blocks them). Close it or enter the printer IP manually."
            }
            Error::Io(_) => "A local I/O error occurred (file not readable, socket error). Check the path and retry.",
            Error::Json(_) => "Malformed JSON. Retry; if it persists, report the raw message.",
        }
    }

    /// Whether retrying the same operation unchanged may succeed.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Error::Unreachable { .. }
                | Error::Timeout { .. }
                | Error::ConnectionLost { .. }
                | Error::NotConnected
                | Error::Ftp { .. }
                | Error::Io(_)
        )
    }

    pub(crate) fn protocol(detail: impl Into<String>) -> Self {
        Error::Protocol {
            detail: detail.into(),
        }
    }

    pub(crate) fn timeout(what: impl Into<String>) -> Self {
        Error::Timeout { what: what.into() }
    }

    pub(crate) fn invalid_input(detail: impl Into<String>) -> Self {
        Error::InvalidInput {
            detail: detail.into(),
        }
    }
}

/// True for the error kinds a socket read with a timeout returns on expiry.
pub(crate) fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable_and_hints_non_empty() {
        let errors = [
            Error::timeout("x"),
            Error::AuthFailed { detail: "x".into() },
            Error::TlsFingerprintMismatch {
                expected: "a".into(),
                actual: "b".into(),
            },
            Error::protocol("x"),
            Error::NotConnected,
        ];
        let codes: Vec<_> = errors.iter().map(Error::code).collect();
        assert_eq!(
            codes,
            ["timeout", "auth_failed", "tls_fingerprint_mismatch", "protocol", "not_connected"]
        );
        for e in &errors {
            assert!(!e.hint().is_empty());
        }
    }
}
