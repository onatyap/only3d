//! Printer connection settings.

use std::fmt;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::models::ModelInfo;

/// Everything needed to talk to one printer over the LAN.
///
/// Serialisable so the app can persist it. `access_code` is a secret: it is
/// redacted from `Debug` output, but **is** serialised, so store the file with
/// user-only permissions.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrinterConfig {
    /// Display name chosen by the user (free text).
    #[serde(default)]
    pub name: String,
    /// IP address (preferred) or host name.
    pub host: String,
    /// Printer serial number (MQTT topics are `device/{serial}/...`).
    pub serial: String,
    /// 8-character LAN access code from the printer screen.
    pub access_code: String,
    /// Model code from discovery (`O1D` = H2D) or a name; optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Pinned SHA-256 of the printer's TLS leaf certificate (hex, any case,
    /// colons allowed). `None` = trust on first use: accept and report the
    /// observed fingerprint so the caller can save it here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert_sha256: Option<String>,
}

impl fmt::Debug for PrinterConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrinterConfig")
            .field("name", &self.name)
            .field("host", &self.host)
            .field("serial", &self.serial)
            .field("access_code", &"<redacted>")
            .field("model", &self.model)
            .field("cert_sha256", &self.cert_sha256)
            .finish()
    }
}

impl PrinterConfig {
    /// Convenience constructor; `name` defaults to the serial.
    pub fn new(
        host: impl Into<String>,
        serial: impl Into<String>,
        access_code: impl Into<String>,
    ) -> Self {
        let serial = serial.into();
        PrinterConfig {
            name: serial.clone(),
            host: host.into(),
            serial,
            access_code: access_code.into(),
            model: None,
            cert_sha256: None,
        }
    }

    /// Check for values that can never work or could inject protocol commands
    /// (CR/LF in FTP, `/`/`#`/`+` in MQTT topics).
    pub fn validate(&self) -> Result<()> {
        let bad = |detail: String| Err(Error::InvalidConfig { detail });
        let host = self.host.trim();
        if host.is_empty() {
            return bad("host is empty".into());
        }
        if host.chars().any(|c| c.is_whitespace() || c.is_control() || c == '/') {
            return bad(format!("host {host:?} contains invalid characters"));
        }
        if self.serial.is_empty() || !self.serial.chars().all(|c| c.is_ascii_alphanumeric()) {
            return bad(format!(
                "serial {:?} must be non-empty ASCII letters/digits",
                self.serial
            ));
        }
        if self.access_code.is_empty() {
            return bad("access_code is empty".into());
        }
        if self
            .access_code
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
        {
            return bad("access_code contains whitespace or control characters".into());
        }
        if let Some(fp) = &self.cert_sha256 {
            if crate::tls::normalize_fingerprint(fp).is_none() {
                return bad(format!(
                    "cert_sha256 {fp:?} is not a 64-hex-digit SHA-256 fingerprint"
                ));
            }
        }
        Ok(())
    }

    /// Host with surrounding whitespace / IPv6 brackets removed.
    pub(crate) fn host_trimmed(&self) -> &str {
        self.host
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
    }

    /// Parsed IP address, if `host` is an IP literal.
    pub fn ip(&self) -> Option<IpAddr> {
        self.host_trimmed().parse().ok()
    }

    /// Model information resolved from `model`, if known.
    pub fn model_info(&self) -> Option<&'static ModelInfo> {
        self.model.as_deref().and_then(crate::models::lookup)
    }

    /// MQTT topic the printer publishes reports on.
    pub fn report_topic(&self) -> String {
        format!("device/{}/report", self.serial)
    }

    /// MQTT topic the printer accepts commands on.
    pub fn request_topic(&self) -> String {
        format!("device/{}/request", self.serial)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_access_code() {
        let cfg = PrinterConfig::new("192.168.1.20", "0948AB123456789", "acc0de99");
        let s = format!("{cfg:?}");
        assert!(!s.contains("acc0de99"));
        assert!(s.contains("redacted"));
    }

    #[test]
    fn validate_rejects_injection() {
        let mut cfg = PrinterConfig::new("192.168.1.20", "0948AB123456789", "1234\r\nDELE x");
        assert_eq!(cfg.validate().unwrap_err().code(), "invalid_config");
        cfg.access_code = "12345678".into();
        cfg.validate().unwrap();
        cfg.serial = "abc/#".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn serde_roundtrip_with_defaults() {
        let cfg: PrinterConfig = serde_json::from_str(
            r#"{"host":"10.0.0.5","serial":"0948AB123456789","access_code":"abcd1234"}"#,
        )
        .unwrap();
        assert_eq!(cfg.name, "");
        assert!(cfg.model.is_none() && cfg.cert_sha256.is_none());
        let back = serde_json::to_value(&cfg).unwrap();
        assert!(back.get("model").is_none());
        assert_eq!(cfg.ip(), Some("10.0.0.5".parse().unwrap()));
    }
}
