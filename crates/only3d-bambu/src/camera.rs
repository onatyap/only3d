//! Chamber camera snapshots over the printer's TLS JPEG stream on port 6000 (the LAN live view
//! protocol of P1/A1 and, as found on a real H2D, open there too). Read-only.
//!
//! Protocol (community-documented, e.g. ha-bambulab / OpenBambuAPI): after TLS, send an 80-byte
//! auth packet `u32 0x40, u32 0x3000, u32 0, u32 0, "bblp" padded to 32, access code padded
//! to 32` (little endian); the printer then streams frames of a 16-byte header (payload length in
//! the first 4 bytes, LE) followed by a JPEG.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use crate::config::PrinterConfig;
use crate::error::{Error, Result};
use crate::tls::{client_config, server_name, tcp_connect, TlsStream};

pub const CAMERA_PORT: u16 = 6000;

fn auth_packet(access_code: &str) -> Vec<u8> {
    let mut p = Vec::with_capacity(80);
    for v in [0x40u32, 0x3000, 0, 0] {
        p.extend_from_slice(&v.to_le_bytes());
    }
    let mut user = b"bblp".to_vec();
    user.resize(32, 0);
    let mut code = access_code.as_bytes().to_vec();
    code.resize(32, 0);
    p.extend_from_slice(&user);
    p.extend_from_slice(&code);
    p
}

fn read_exact_deadline(s: &mut TlsStream, buf: &mut [u8], deadline: Instant) -> Result<()> {
    let mut got = 0;
    while got < buf.len() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Error::timeout("camera frame"));
        }
        s.set_read_timeout(Some(left))?;
        match s.read(&mut buf[got..]) {
            Ok(0) => return Err(Error::Protocol { detail: "camera stream closed".into() }),
            Ok(n) => got += n,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                return Err(Error::timeout("camera frame"))
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Grab one JPEG frame from the chamber camera.
pub fn snapshot(cfg: &PrinterConfig, timeout: Duration) -> Result<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    let tls = client_config(cfg.cert_sha256.as_deref())?;
    let sock = tcp_connect(&cfg.host, CAMERA_PORT, timeout)?;
    let mut s = TlsStream::handshake(sock, tls.config.clone(), server_name(&cfg.host)?, timeout, &tls.verifier)?;
    s.set_write_timeout(Some(timeout))?;
    s.write_all(&auth_packet(&cfg.access_code))?;
    s.flush()?;
    // skip anything that isn't a JPEG (the first frame after connect is a full one)
    for _ in 0..5 {
        let mut hdr = [0u8; 16];
        read_exact_deadline(&mut s, &mut hdr, deadline)?;
        let len = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as usize;
        if len == 0 || len > 8 * 1024 * 1024 {
            return Err(Error::Protocol { detail: format!("implausible camera frame size {len} (wrong access code?)") });
        }
        let mut jpg = vec![0u8; len];
        read_exact_deadline(&mut s, &mut jpg, deadline)?;
        if jpg.starts_with(&[0xFF, 0xD8]) {
            s.close();
            return Ok(jpg);
        }
    }
    Err(Error::Protocol { detail: "no JPEG frame received".into() })
}

#[cfg(test)]
mod tests {
    #[test]
    fn auth_packet_layout() {
        let p = super::auth_packet("12345678");
        assert_eq!(p.len(), 80);
        assert_eq!(&p[0..4], &[0x40, 0, 0, 0]);
        assert_eq!(&p[4..8], &[0, 0x30, 0, 0]);
        assert_eq!(&p[16..20], b"bblp");
        assert_eq!(&p[48..56], b"12345678");
    }
}
