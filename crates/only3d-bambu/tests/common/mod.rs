//! Shared helpers for the in-process fake printer servers.
#![allow(dead_code)]

use std::io::{self, Read};
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, SupportedProtocolVersion};
use serde_json::Value;

pub const SERIAL: &str = "0948AD4A1100123";
pub const ACCESS_CODE: &str = "a1b2c3d4";

pub struct TestCert {
    pub cert: CertificateDer<'static>,
    pub key: PrivatePkcs8KeyDer<'static>,
    pub sha256: String,
}

/// Self-signed cert shaped like a printer's (CN = serial).
pub fn make_cert() -> TestCert {
    let mut params = rcgen::CertificateParams::new(vec!["127.0.0.1".to_string()]).unwrap();
    params.distinguished_name = rcgen::DistinguishedName::new();
    params.distinguished_name.push(rcgen::DnType::CommonName, SERIAL);
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap();
    let der = cert.der().clone();
    TestCert {
        sha256: only3d_bambu::tls::sha256_hex(der.as_ref()),
        cert: der,
        key: PrivatePkcs8KeyDer::from(key.serialize_der()),
    }
}

pub fn server_config(c: &TestCert, versions: &[&'static SupportedProtocolVersion]) -> Arc<ServerConfig> {
    let cfg = ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(versions)
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![c.cert.clone()], PrivateKeyDer::Pkcs8(c.key.clone_key()))
        .unwrap();
    Arc::new(cfg)
}

pub fn sample(name: &str) -> Value {
    let path = format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Fingerprint formatted the way a user might paste it: upper case with colons.
pub fn pretty_fp(hex: &str) -> String {
    hex.as_bytes()
        .chunks(2)
        .map(|c| std::str::from_utf8(c).unwrap().to_ascii_uppercase())
        .collect::<Vec<_>>()
        .join(":")
}

/// Read until EOF, treating a missing close_notify as EOF.
pub fn read_to_end_lenient(r: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        match r.read(&mut buf) {
            Ok(0) => return Ok(out),
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(out),
            Err(e) => return Err(e),
        }
    }
}
