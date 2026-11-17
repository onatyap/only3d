//! TLS plumbing shared by MQTT (8883) and FTPS (990).
//!
//! Bambu printers serve a per-device certificate (CN = serial) issued by a
//! private Bambu Lab CA, and we connect by IP. Instead of a CA + hostname
//! check we use **trust on first use** pinning of the leaf certificate's
//! SHA-256:
//!
//! * a fingerprint is configured → the presented leaf must match it exactly;
//! * none configured → accept, and record the observed fingerprint so the
//!   caller can persist it ([`crate::PrinterClient::tls_fingerprint`]).
//!
//! Either way the handshake signature is still verified against the leaf's
//! public key, so a pinned fingerprint really proves possession of the key.

use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Lower-case hex SHA-256 of a DER certificate (64 chars, no separators).
pub fn sha256_hex(der: &[u8]) -> String {
    hex(&Sha256::digest(der))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Normalise a user-supplied fingerprint: accepts an optional `sha256:`
/// prefix, `:`/space separators and any case. `None` if not 32 bytes of hex.
pub fn normalize_fingerprint(s: &str) -> Option<String> {
    let s = s.trim();
    let s = s
        .strip_prefix("sha256:")
        .or_else(|| s.strip_prefix("SHA256:"))
        .unwrap_or(s);
    let cleaned: String = s
        .chars()
        .filter(|c| !matches!(c, ':' | ' ' | '-'))
        .map(|c| c.to_ascii_lowercase())
        .collect();
    (cleaned.len() == 64 && cleaned.chars().all(|c| c.is_ascii_hexdigit())).then_some(cleaned)
}

/// rustls certificate verifier implementing TOFU fingerprint pinning.
#[derive(Debug)]
pub struct PinningVerifier {
    expected: Option<String>,
    observed: Mutex<Option<String>>,
    provider: Arc<CryptoProvider>,
}

impl PinningVerifier {
    /// `expected`: pinned fingerprint (any format accepted by
    /// [`normalize_fingerprint`]) or `None` for trust-on-first-use.
    pub fn new(expected: Option<&str>, provider: Arc<CryptoProvider>) -> Result<Self> {
        let expected = match expected {
            None => None,
            Some(fp) => Some(normalize_fingerprint(fp).ok_or_else(|| Error::InvalidConfig {
                detail: format!("cert_sha256 {fp:?} is not a SHA-256 hex fingerprint"),
            })?),
        };
        Ok(PinningVerifier {
            expected,
            observed: Mutex::new(None),
            provider,
        })
    }

    /// Fingerprint of the most recently presented leaf certificate.
    pub fn observed(&self) -> Option<String> {
        self.observed.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The pinned fingerprint, normalised.
    pub fn expected(&self) -> Option<&str> {
        self.expected.as_deref()
    }

    /// True when a pin is configured and the last observed cert differs.
    pub fn mismatch(&self) -> Option<(String, String)> {
        match (&self.expected, self.observed()) {
            (Some(e), Some(o)) if *e != o => Some((e.clone(), o)),
            _ => None,
        }
    }
}

impl ServerCertVerifier for PinningVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let fp = sha256_hex(end_entity.as_ref());
        *self.observed.lock().unwrap_or_else(|p| p.into_inner()) = Some(fp.clone());
        match &self.expected {
            Some(expected) if *expected != fp => Err(rustls::Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            )),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_signature_raw(message, cert, dss, &self.provider, false)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_signature_raw(message, cert, dss, &self.provider, true)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// One DER TLV: (tag, header length, content).
fn der_tlv(buf: &[u8]) -> Option<(u8, usize, &[u8])> {
    let tag = *buf.first()?;
    let first = *buf.get(1)? as usize;
    let (len, hdr) = if first < 0x80 {
        (first, 2)
    } else {
        let n = first & 0x7f;
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for i in 0..n {
            len = (len << 8) | *buf.get(2 + i)? as usize;
        }
        (len, 2 + n)
    };
    let content = buf.get(hdr..hdr + len)?;
    Some((tag, hdr + len, content))
}

/// (AlgorithmIdentifier contents, subjectPublicKey bits) of an X.509 certificate of *any*
/// version. Bambu printers (the H2D at least) present a certificate that webpki rejects with
/// `UnsupportedCertVersion`, so the key is extracted here instead.
pub(crate) fn spki_of(cert: &[u8]) -> Option<(&[u8], &[u8])> {
    let (t, _, cert_body) = der_tlv(cert)?;
    if t != 0x30 {
        return None;
    }
    let (t, _, mut tbs) = der_tlv(cert_body)?;
    if t != 0x30 {
        return None;
    }
    // [0] EXPLICIT version (absent in v1 certificates)
    if tbs.first() == Some(&0xa0) {
        let (_, n, _) = der_tlv(tbs)?;
        tbs = &tbs[n..];
    }
    // serialNumber, signature, issuer, validity, subject
    for _ in 0..5 {
        let (_, n, _) = der_tlv(tbs)?;
        tbs = &tbs[n..];
    }
    let (t, _, spki) = der_tlv(tbs)?;
    if t != 0x30 {
        return None;
    }
    let (t, n, alg) = der_tlv(spki)?;
    if t != 0x30 {
        return None;
    }
    let (t, _, bits) = der_tlv(&spki[n..])?;
    if t != 0x03 || bits.is_empty() {
        return None;
    }
    Some((alg, &bits[1..]))
}

/// Verify a handshake signature with the leaf's public key, using the provider's own
/// algorithms (ring), without asking webpki to parse the certificate.
fn verify_signature_raw(
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
    provider: &CryptoProvider,
    tls13: bool,
) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
    let tls13_ok = matches!(
        dss.scheme,
        SignatureScheme::ECDSA_NISTP256_SHA256
            | SignatureScheme::ECDSA_NISTP384_SHA384
            | SignatureScheme::ECDSA_NISTP521_SHA512
            | SignatureScheme::RSA_PSS_SHA256
            | SignatureScheme::RSA_PSS_SHA384
            | SignatureScheme::RSA_PSS_SHA512
            | SignatureScheme::ED25519
            | SignatureScheme::ED448
    );
    if tls13 && !tls13_ok {
        return Err(rustls::Error::PeerMisbehaved(rustls::PeerMisbehaved::SignedHandshakeWithUnadvertisedSigScheme));
    }
    let (alg_id, public_key) = spki_of(cert.as_ref()).ok_or(rustls::Error::InvalidCertificate(CertificateError::BadEncoding))?;
    let algs = &provider.signature_verification_algorithms;
    let candidates = algs
        .mapping
        .iter()
        .find(|(scheme, _)| *scheme == dss.scheme)
        .map(|(_, a)| *a)
        .ok_or(rustls::Error::PeerMisbehaved(rustls::PeerMisbehaved::SignedHandshakeWithUnadvertisedSigScheme))?;
    let mut matched = false;
    for alg in candidates {
        let id = alg.public_key_alg_id();
        let id: &[u8] = id.as_ref();
        // AlgorithmIdentifier contents; some keys encode NULL parameters, some omit them
        if id != alg_id && !(alg_id.starts_with(id) && alg_id[id.len()..] == [0x05, 0x00]) && !(id.starts_with(alg_id) && id[alg_id.len()..] == [0x05, 0x00]) {
            continue;
        }
        matched = true;
        if alg.verify_signature(public_key, message, dss.signature()).is_ok() {
            return Ok(HandshakeSignatureValid::assertion());
        }
    }
    Err(rustls::Error::InvalidCertificate(if matched { CertificateError::BadSignature } else { CertificateError::UnknownIssuer }))
}

/// A client config plus a handle on its verifier (to read the fingerprint).
#[derive(Debug, Clone)]
pub struct TlsSetup {
    pub config: Arc<ClientConfig>,
    pub verifier: Arc<PinningVerifier>,
}

/// Build a rustls client config (TLS 1.3 + 1.2, ring) with pinning.
///
/// The config carries rustls' default in-memory session store, which is what
/// lets the FTPS data channel resume the control channel's session.
pub fn client_config(expected_fingerprint: Option<&str>) -> Result<TlsSetup> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(PinningVerifier::new(expected_fingerprint, provider.clone())?);
    let config = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(|e| Error::Tls {
            detail: e.to_string(),
        })?
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone())
        .with_no_client_auth();
    Ok(TlsSetup {
        config: Arc::new(config),
        verifier,
    })
}

/// rustls server name for a host: IP literal → `IpAddress` (no SNI is sent),
/// otherwise a DNS name.
pub fn server_name(host: &str) -> Result<ServerName<'static>> {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(ServerName::IpAddress(ip.into()));
    }
    ServerName::try_from(host.to_string()).map_err(|_| Error::InvalidConfig {
        detail: format!("host {host:?} is neither an IP address nor a valid DNS name"),
    })
}

/// Resolve and connect with a timeout, mapping failures to [`Error::Unreachable`].
pub(crate) fn tcp_connect(host: &str, port: u16, timeout: Duration) -> Result<TcpStream> {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    let unreachable = |source: io::Error| Error::Unreachable {
        host: host.to_string(),
        port,
        source,
    };
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(unreachable)?
        .collect();
    let mut last = io::Error::new(io::ErrorKind::NotFound, "host resolved to no addresses");
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                return Ok(s);
            }
            Err(e) => last = e,
        }
    }
    if crate::error::is_timeout(&last) {
        return Err(Error::Unreachable {
            host: host.to_string(),
            port,
            source: io::Error::new(io::ErrorKind::TimedOut, "connect timed out"),
        });
    }
    Err(unreachable(last))
}

/// A blocking TLS client stream over a `TcpStream`, driven manually so that
/// socket read timeouts (used for polling) never corrupt TLS state.
pub(crate) struct TlsStream {
    sock: TcpStream,
    conn: ClientConnection,
    buf: Vec<u8>,
    pos: usize,
}

impl TlsStream {
    /// Run the TLS handshake on an already-connected socket.
    pub(crate) fn handshake(
        sock: TcpStream,
        config: Arc<ClientConfig>,
        name: ServerName<'static>,
        timeout: Duration,
        verifier: &PinningVerifier,
    ) -> Result<Self> {
        let conn = ClientConnection::new(config, name).map_err(|e| Error::Tls {
            detail: e.to_string(),
        })?;
        let mut s = TlsStream {
            sock,
            conn,
            buf: Vec::new(),
            pos: 0,
        };
        let deadline = Instant::now() + timeout;
        while s.conn.is_handshaking() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::timeout("TLS handshake"));
            }
            s.sock.set_read_timeout(Some(remaining))?;
            s.sock.set_write_timeout(Some(remaining))?;
            if let Err(e) = s.conn.complete_io(&mut s.sock) {
                return Err(map_tls_io_error(e, verifier, "TLS handshake"));
            }
        }
        s.flush_tls()
            .map_err(|e| map_tls_io_error(e, verifier, "TLS handshake"))?;
        Ok(s)
    }

    pub(crate) fn set_read_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        self.sock.set_read_timeout(t)
    }

    pub(crate) fn set_write_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        self.sock.set_write_timeout(t)
    }

    /// Whether this connection resumed a cached session (FTPS data channel).
    pub(crate) fn resumed(&self) -> bool {
        matches!(self.conn.handshake_kind(), Some(rustls::HandshakeKind::Resumed))
    }

    pub(crate) fn protocol_version(&self) -> Option<rustls::ProtocolVersion> {
        self.conn.protocol_version()
    }

    fn flush_tls(&mut self) -> io::Result<()> {
        while self.conn.wants_write() {
            self.conn.write_tls(&mut self.sock)?;
        }
        Ok(())
    }

    /// Decrypt more data into the internal buffer. `Ok(0)` = peer closed.
    /// Socket timeouts surface as `WouldBlock`/`TimedOut` errors.
    fn fill(&mut self) -> io::Result<usize> {
        let mut tmp = [0u8; 16 * 1024];
        loop {
            match self.conn.reader().read(&mut tmp) {
                Ok(0) => return Ok(0),
                Ok(n) => {
                    if self.pos == self.buf.len() {
                        self.buf.clear();
                        self.pos = 0;
                    }
                    self.buf.extend_from_slice(&tmp[..n]);
                    return Ok(n);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
            }
            self.flush_tls()?;
            let n = self.conn.read_tls(&mut self.sock)?;
            if let Err(e) = self.conn.process_new_packets() {
                // Try to deliver the alert before failing.
                let _ = self.flush_tls();
                return Err(io::Error::new(io::ErrorKind::InvalidData, e));
            }
            // Post-handshake messages (TLS 1.3 tickets, key updates) may need a reply.
            self.flush_tls()?;
            if n == 0 {
                // TCP EOF: the reader now reports Ok(0) (close_notify seen)
                // or UnexpectedEof on the next iteration.
                continue;
            }
        }
    }

    /// Bytes already decrypted but not consumed.
    pub(crate) fn buffered(&self) -> &[u8] {
        &self.buf[self.pos..]
    }

    pub(crate) fn consume(&mut self, n: usize) {
        self.pos = (self.pos + n).min(self.buf.len());
        if self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        }
    }

    /// Read more plaintext into the buffer (see [`Self::fill`]).
    pub(crate) fn read_more(&mut self) -> io::Result<usize> {
        self.fill()
    }

    /// Send `close_notify` and flush; errors ignored (best effort).
    pub(crate) fn close(&mut self) {
        self.conn.send_close_notify();
        let _ = self.flush_tls();
    }

    pub(crate) fn shutdown_tcp(&self, how: std::net::Shutdown) {
        let _ = self.sock.shutdown(how);
    }
}

impl Read for TlsStream {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos == self.buf.len() && self.fill()? == 0 {
            return Ok(0);
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.consume(n);
        Ok(n)
    }
}

impl Write for TlsStream {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut rest = data;
        while !rest.is_empty() {
            let n = self.conn.writer().write(rest)?;
            rest = &rest[n..];
            self.flush_tls()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_tls()
    }
}

/// Map an I/O error from a TLS operation into the crate error, recognising
/// fingerprint mismatches and timeouts.
pub(crate) fn map_tls_io_error(e: io::Error, verifier: &PinningVerifier, what: &str) -> Error {
    if let Some((expected, actual)) = verifier.mismatch() {
        return Error::TlsFingerprintMismatch { expected, actual };
    }
    if crate::error::is_timeout(&e) {
        return Error::timeout(what);
    }
    let is_tls = e
        .get_ref()
        .map(|inner| inner.is::<rustls::Error>())
        .unwrap_or(false);
    if is_tls {
        return Error::Tls {
            detail: format!("{what}: {e}"),
        };
    }
    match e.kind() {
        io::ErrorKind::UnexpectedEof
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe => Error::ConnectionLost {
            detail: format!("{what}: {e}"),
        },
        _ => Error::Io(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_normalisation() {
        let raw = "AB:".repeat(31) + "AB";
        assert_eq!(normalize_fingerprint(&raw).unwrap(), "ab".repeat(32));
        assert_eq!(
            normalize_fingerprint(&format!("sha256:{}", "0F".repeat(32))).unwrap(),
            "0f".repeat(32)
        );
        assert!(normalize_fingerprint("abc").is_none());
        assert!(normalize_fingerprint(&"zz".repeat(32)).is_none());
    }

    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn server_name_for_ip_and_dns() {
        assert!(matches!(server_name("192.168.1.5").unwrap(), ServerName::IpAddress(_)));
        assert!(matches!(server_name("printer.local").unwrap(), ServerName::DnsName(_)));
        assert!(server_name("bad host!").is_err());
    }
}
