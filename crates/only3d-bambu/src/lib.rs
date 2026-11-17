//! # only3d-bambu
//!
//! Synchronous (std threads, no async runtime) LAN client for Bambu Lab
//! printers with first-class H2D support:
//!
//! * [`discover`] — passive SSDP discovery (UDP 2021/1990), no credentials.
//! * [`PrinterClient`] — MQTT 3.1.1 over TLS (port 8883, user `bblp`,
//!   password = LAN access code) with trust-on-first-use certificate pinning,
//!   a background I/O thread, keep-alive, auto-reconnect, a deep-merged state
//!   cache ([`PrinterClient::raw_state`]), a typed [`PrinterStatus`] and an
//!   event stream ([`PrinterClient::subscribe`]).
//! * [`Command`] — typed commands plus a raw JSON escape hatch.
//! * [`upload_file`] / [`list_files`] / [`FtpsClient`] — implicit FTPS
//!   (port 990) with the TLS session reuse vsftpd demands.
//! * [`start_print`] — `project_file` for an uploaded `.gcode.3mf`
//!   ([`PrintJob`]), with a busy check and acknowledgement handling.
//!
//! Every error has a stable [`Error::code`] and an actionable [`Error::hint`].
//!
//! ```no_run
//! use only3d_bambu::{PrinterClient, PrinterConfig};
//! # fn main() -> only3d_bambu::Result<()> {
//! let mut cfg = PrinterConfig::new("192.168.1.42", "0948AD4A1100123", "12345678");
//! let client = PrinterClient::connect(&cfg)?;
//! // Trust on first use: persist the fingerprint to pin it next time.
//! cfg.cert_sha256 = client.tls_fingerprint();
//! let st = client.status();
//! println!("{} {:?}% nozzles={:?}", st.gcode_state, st.progress_percent, st.nozzles);
//! # Ok(()) }
//! ```

pub mod recovery;

/// Bambu Studio's own data folder (HMS texts, user presets): `~/Library/Application Support`
/// on macOS, `%APPDATA%` on Windows, `~/.config` elsewhere.
pub fn bambu_studio_dir() -> std::path::PathBuf {
    let home = std::env::home_dir().unwrap_or_default();
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/BambuStudio")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(std::path::PathBuf::from).unwrap_or(home).join("BambuStudio")
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).unwrap_or_else(|| home.join(".config")).join("BambuStudio")
    }
}
pub mod camera;
pub mod client;
pub mod commands;
pub mod config;
pub mod discovery;
pub mod error;
pub mod ftps;
pub mod models;
pub mod mqtt;
pub mod print;
pub mod status;
pub mod tls;

pub use client::{ClientOptions, Event, PrinterClient};
pub use commands::{Calibration, Command, LightNode, PrintJob, SpeedLevel, UrlStyle};
pub use config::PrinterConfig;
pub use discovery::{discover, discover_detailed, parse_ssdp, DiscoveredPrinter, DiscoveryReport};
pub use error::{Error, Result};
pub use ftps::{list_files, list_files_with, parse_pasv, upload_file, upload_file_with, FtpOptions, FtpsClient, RemoteFile, UploadResult, UploadSource};
pub use models::{lookup as lookup_model, model_name, ModelInfo};
pub use print::{start_print, start_print_with, StartPrintAck};
pub use status::{
    deep_merge, stage_name, AirDuct, AirPart, AmsTray, AmsUnit, FanStatus, GcodeState, HmsError, LightStatus, NozzleStatus, PrintError,
    PrinterStatus, Temperature, TrayRef,
};
