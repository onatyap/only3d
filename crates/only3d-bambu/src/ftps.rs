//! Implicit FTPS client (TLS from the first byte, port 990) for the printer's
//! storage, plus the [`upload_file`] / [`list_files`] helpers.
//!
//! Login is `bblp` / LAN access code (OpenBambuAPI `ftp.md`). FTPS serves the
//! printer's removable storage (SD card); Bambu Studio and bambuddy put
//! print files in its root and start them as `ftp://<name>`. The H2D's
//! internal eMMC (`brtc://emmc/...` URLs) is not reachable over FTPS.
//!
//! ## TLS session reuse on the data channel
//!
//! The printer runs vsftpd with `require_ssl_reuse=YES`: every data
//! connection (LIST, STOR, ...) must *resume* the TLS session of the control
//! connection, otherwise vsftpd answers `522 SSL connection failed: session
//! reuse required` and drops the transfer. rustls resumes automatically when
//! two connections share the same `Arc<ClientConfig>` (whose default
//! `Resumption` holds an in-memory session store) **and** the same
//! `ServerName` (the store's cache key). So the data channel is built from
//! `self.tls.config.clone()` and `self.server_name.clone()` — never a fresh
//! config. Bambu FTPS has been observed to negotiate TLS 1.2 only (bambuddy
//! `ftp_profiles.py`: X1C, H2D and P2S refuse 1.3 on :990), where rustls
//! stores the session ID/ticket at the end of the control handshake and can
//! offer it any number of times; with TLS 1.3 each data channel consumes one
//! of the tickets received on the control channel.

use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use md5::{Digest, Md5};
use rustls::pki_types::ServerName;
use serde::{Deserialize, Serialize};

use crate::commands::validate_remote_name;
use crate::config::PrinterConfig;
use crate::error::{is_timeout, Error, Result};
use crate::tls::{client_config, hex, map_tls_io_error, server_name, tcp_connect, TlsSetup, TlsStream};

/// FTPS tunables.
#[derive(Debug, Clone)]
pub struct FtpOptions {
    pub port: u16,
    /// Connect / handshake / ordinary reply timeout.
    pub timeout: Duration,
    /// Inactivity timeout on the data socket.
    pub transfer_timeout: Duration,
    /// How long to wait for `226` after the upload data was sent. The H2D can
    /// take 30+ s to flush and confirm (bambuddy `bambu_ftp.py`).
    pub final_reply_timeout: Duration,
    /// Upload chunk size (progress granularity).
    pub chunk_size: usize,
}

impl Default for FtpOptions {
    fn default() -> Self {
        FtpOptions {
            port: 990,
            timeout: Duration::from_secs(15),
            transfer_timeout: Duration::from_secs(60),
            final_reply_timeout: Duration::from_secs(120),
            chunk_size: 64 * 1024,
        }
    }
}

/// One FTP reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FtpReply {
    pub code: u16,
    /// All reply lines joined with `\n`, including the code prefixes.
    pub text: String,
}

/// A directory entry parsed from `LIST`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteFile {
    pub name: String,
    pub size: Option<u64>,
    pub is_dir: bool,
    /// Modification time as printed by the server (`Nov 14 12:00`).
    pub modified: Option<String>,
}

/// Parse a `227 Entering Passive Mode (h1,h2,h3,h4,p1,p2)` reply.
/// Parentheses are optional; the first run of six comma-separated numbers wins.
pub fn parse_pasv(reply: &str) -> Result<(Ipv4Addr, u16)> {
    let bytes = reply.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() && (i == 0 || !bytes[i - 1].is_ascii_digit()) {
            let end = bytes[i..]
                .iter()
                .position(|b| !(b.is_ascii_digit() || *b == b',' || *b == b' '))
                .map_or(bytes.len(), |p| i + p);
            let nums: Vec<&str> = reply[i..end].split(',').map(str::trim).collect();
            if nums.len() >= 6 {
                let parsed: Option<Vec<u8>> = nums[..6].iter().map(|n| n.parse::<u8>().ok()).collect();
                if let Some(n) = parsed {
                    let ip = Ipv4Addr::new(n[0], n[1], n[2], n[3]);
                    let port = u16::from(n[4]) << 8 | u16::from(n[5]);
                    if port == 0 {
                        return Err(Error::protocol(format!("PASV reply with port 0: {reply:?}")));
                    }
                    return Ok((ip, port));
                }
            }
            i = end.max(i + 1);
        } else {
            i += 1;
        }
    }
    Err(Error::protocol(format!("cannot parse PASV reply {reply:?}")))
}

/// Parse one Unix-style `LIST` line (`-rw-r--r-- 1 user group 1234 Nov 14 12:00 a b.3mf`).
pub fn parse_list_line(line: &str) -> Option<RemoteFile> {
    let line = line.trim_end_matches(['\r', '\n']);
    let first = line.chars().next()?;
    if !matches!(first, '-' | 'd' | 'l') {
        return None;
    }
    // Find the start of the 9th whitespace-separated field; the name may contain spaces.
    let mut fields = Vec::with_capacity(8);
    let mut rest = line;
    for _ in 0..8 {
        let t = rest.trim_start();
        let end = t.find(char::is_whitespace)?;
        fields.push(&t[..end]);
        rest = &t[end..];
    }
    let mut name = rest.trim_start().to_string();
    if first == 'l' {
        if let Some(pos) = name.find(" -> ") {
            name.truncate(pos);
        }
    }
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    Some(RemoteFile {
        name,
        size: fields[4].parse().ok(),
        is_dir: first == 'd',
        modified: Some(format!("{} {} {}", fields[5], fields[6], fields[7])),
    })
}

fn check_path(path: &str) -> Result<()> {
    if path.chars().any(|c| c == '\r' || c == '\n' || c == '\0') {
        return Err(Error::invalid_input(format!("FTP path {path:?} contains CR/LF/NUL")));
    }
    Ok(())
}

/// A logged-in FTPS session.
pub struct FtpsClient {
    ctrl: TlsStream,
    tls: TlsSetup,
    server_name: ServerName<'static>,
    peer_ip: IpAddr,
    opts: FtpOptions,
    last_data_resumed: Option<bool>,
}

impl std::fmt::Debug for FtpsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FtpsClient")
            .field("peer", &self.peer_ip)
            .field("last_data_resumed", &self.last_data_resumed)
            .finish()
    }
}

impl FtpsClient {
    pub fn connect(cfg: &PrinterConfig) -> Result<Self> {
        Self::connect_with(cfg, FtpOptions::default())
    }

    /// TLS handshake, `USER bblp`, `PASS <code>`, `PBSZ 0`, `PROT P`, `TYPE I`.
    pub fn connect_with(cfg: &PrinterConfig, opts: FtpOptions) -> Result<Self> {
        cfg.validate()?;
        let tls = client_config(cfg.cert_sha256.as_deref())?;
        let host = cfg.host_trimmed();
        let name = server_name(host)?;
        let sock = tcp_connect(host, opts.port, opts.timeout)?;
        let peer_ip = sock.peer_addr()?.ip();
        let ctrl = TlsStream::handshake(sock, tls.config.clone(), name.clone(), opts.timeout, &tls.verifier)?;
        ctrl.set_write_timeout(Some(opts.timeout))?;
        let mut c = FtpsClient {
            ctrl,
            tls,
            server_name: name,
            peer_ip,
            opts,
            last_data_resumed: None,
        };
        let greeting = c.read_reply(c.opts.timeout)?;
        if greeting.code != 220 {
            return Err(c.reply_error("greeting", "", greeting));
        }
        let user = c.cmd("USER bblp")?;
        match user.code {
            230 => {}
            331 => {
                check_path(&cfg.access_code)?;
                let pass = c.cmd(&format!("PASS {}", cfg.access_code))?;
                if pass.code != 230 {
                    return Err(c.reply_error("PASS", "", pass));
                }
            }
            _ => return Err(c.reply_error("USER", "", user)),
        }
        c.expect("PBSZ 0", &[200], "PBSZ")?;
        c.expect("PROT P", &[200], "PROT")?;
        c.expect("TYPE I", &[200], "TYPE")?;
        Ok(c)
    }

    /// SHA-256 of the printer certificate seen on the control channel.
    pub fn peer_fingerprint(&self) -> Option<String> {
        self.tls.verifier.observed()
    }

    /// Whether the most recent data channel resumed the control session.
    pub fn last_data_channel_resumed(&self) -> Option<bool> {
        self.last_data_resumed
    }

    /// Negotiated TLS version of the control channel.
    pub fn tls_version(&self) -> Option<String> {
        self.ctrl.protocol_version().map(|v| format!("{v:?}"))
    }

    fn reply_error(&self, op: &str, path: &str, r: FtpReply) -> Error {
        let message = r.text.lines().last().unwrap_or("").get(4..).unwrap_or("").trim().to_string();
        match r.code {
            530 => Error::AuthFailed {
                detail: format!("FTP {op}: {}", r.text.trim()),
            },
            550 if !path.is_empty() => Error::FileNotFound { path: path.to_string() },
            452 | 552 | 553 => Error::Storage {
                op: op.to_string(),
                code: r.code,
                message,
            },
            522 => Error::Tls {
                detail: format!(
                    "FTP {op}: {} (data-channel TLS session was not resumed)",
                    r.text.trim()
                ),
            },
            _ => Error::Ftp {
                op: op.to_string(),
                code: r.code,
                message,
            },
        }
    }

    fn read_line(&mut self, deadline: Instant) -> Result<String> {
        loop {
            if let Some(pos) = self.ctrl.buffered().iter().position(|b| *b == b'\n') {
                let line = String::from_utf8_lossy(&self.ctrl.buffered()[..pos]).trim_end_matches('\r').to_string();
                self.ctrl.consume(pos + 1);
                return Ok(line);
            }
            if self.ctrl.buffered().len() > 64 * 1024 {
                return Err(Error::protocol("FTP reply line longer than 64 KiB"));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::timeout("FTP reply"));
            }
            self.ctrl.set_read_timeout(Some(remaining.max(Duration::from_millis(1))))?;
            match self.ctrl.read_more() {
                Ok(0) => {
                    return Err(Error::ConnectionLost {
                        detail: "printer closed the FTP control connection".into(),
                    })
                }
                Ok(_) => {}
                Err(e) if is_timeout(&e) => {}
                Err(e) => return Err(map_tls_io_error(e, &self.tls.verifier, "FTP control read")),
            }
        }
    }

    /// Read one (possibly multi-line) reply.
    pub fn read_reply(&mut self, timeout: Duration) -> Result<FtpReply> {
        let deadline = Instant::now() + timeout;
        let first = self.read_line(deadline)?;
        let code = first
            .get(..3)
            .and_then(|c| c.parse::<u16>().ok())
            .ok_or_else(|| Error::protocol(format!("bad FTP reply line {first:?}")))?;
        let mut text = first.clone();
        if first.as_bytes().get(3) == Some(&b'-') {
            let end = format!("{code} ");
            loop {
                let line = self.read_line(deadline)?;
                text.push('\n');
                text.push_str(&line);
                if line.starts_with(&end) || line == code.to_string() {
                    break;
                }
            }
        }
        Ok(FtpReply { code, text })
    }

    /// Send a raw command line (no CR/LF) and read the reply.
    pub fn cmd(&mut self, line: &str) -> Result<FtpReply> {
        check_path(line)?;
        let mut bytes = line.as_bytes().to_vec();
        bytes.extend_from_slice(b"\r\n");
        self.ctrl
            .write_all(&bytes)
            .and_then(|_| self.ctrl.flush())
            .map_err(|e| map_tls_io_error(e, &self.tls.verifier, "FTP control write"))?;
        self.read_reply(self.opts.timeout)
    }

    fn expect(&mut self, line: &str, ok: &[u16], op: &str) -> Result<FtpReply> {
        let r = self.cmd(line)?;
        if ok.contains(&r.code) {
            Ok(r)
        } else {
            Err(self.reply_error(op, "", r))
        }
    }

    /// `PASV`, then connect to the **control connection's peer** on the
    /// returned port. The address in the 227 reply is ignored on purpose:
    /// printers have been seen returning `0.0.0.0` or an internal address,
    /// and dialing only the control peer also rules out FTP bounce tricks
    /// (curl does the same by default, `--ftp-skip-pasv-ip`).
    fn open_data_socket(&mut self) -> Result<TcpStream> {
        let r = self.cmd("PASV")?;
        if r.code != 227 {
            return Err(self.reply_error("PASV", "", r));
        }
        let (_advertised, port) = parse_pasv(&r.text)?;
        let addr = SocketAddr::new(self.peer_ip, port);
        let sock = TcpStream::connect_timeout(&addr, self.opts.timeout).map_err(|source| Error::Unreachable {
            host: self.peer_ip.to_string(),
            port,
            source,
        })?;
        let _ = sock.set_nodelay(true);
        sock.set_read_timeout(Some(self.opts.transfer_timeout))?;
        sock.set_write_timeout(Some(self.opts.transfer_timeout))?;
        Ok(sock)
    }

    /// Open a data connection for `command`, expecting a 125/150 preliminary
    /// reply, then run the TLS handshake on it *resuming the control session*.
    fn start_transfer(&mut self, command: &str, op: &str, path: &str) -> Result<TlsStream> {
        let sock = self.open_data_socket()?;
        let r = self.cmd(command)?;
        if r.code != 125 && r.code != 150 {
            return Err(self.reply_error(op, path, r));
        }
        // Same Arc<ClientConfig> (shared session store) + same ServerName
        // (cache key) as the control channel ⇒ rustls offers the cached
        // session and vsftpd's require_ssl_reuse is satisfied.
        match TlsStream::handshake(
            sock,
            self.tls.config.clone(),
            self.server_name.clone(),
            self.opts.timeout,
            &self.tls.verifier,
        ) {
            Ok(data) => {
                self.last_data_resumed = Some(data.resumed());
                data.set_read_timeout(Some(self.opts.transfer_timeout))?;
                data.set_write_timeout(Some(self.opts.transfer_timeout))?;
                Ok(data)
            }
            Err(e) => {
                // vsftpd explains a refused data handshake on the control channel.
                if let Ok(r) = self.read_reply(Duration::from_secs(2)) {
                    if r.code >= 400 {
                        return Err(self.reply_error(op, path, r));
                    }
                }
                Err(e)
            }
        }
    }

    fn read_all(data: &mut TlsStream) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        Self::copy_all(data, &mut out, None, &mut |_, _| {})?;
        Ok(out)
    }

    fn finish_transfer(&mut self, op: &str, path: &str, timeout: Duration) -> Result<FtpReply> {
        let r = self.read_reply(timeout)?;
        if r.code == 226 || r.code == 250 {
            Ok(r)
        } else {
            Err(self.reply_error(op, path, r))
        }
    }

    fn listing(&mut self, verb: &str, path: &str) -> Result<Vec<String>> {
        check_path(path)?;
        let command = if path.is_empty() { verb.to_string() } else { format!("{verb} {path}") };
        let mut data = self.start_transfer(&command, verb, path)?;
        let bytes = Self::read_all(&mut data);
        data.close();
        drop(data);
        let bytes = bytes?;
        self.finish_transfer(verb, path, self.opts.timeout)?;
        Ok(String::from_utf8_lossy(&bytes)
            .lines()
            .map(|l| l.trim_end_matches('\r').to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }

    /// Raw `LIST` lines.
    pub fn list_raw(&mut self, path: &str) -> Result<Vec<String>> {
        self.listing("LIST", path)
    }

    /// Parsed `LIST`.
    pub fn list(&mut self, path: &str) -> Result<Vec<RemoteFile>> {
        Ok(self.list_raw(path)?.iter().filter_map(|l| parse_list_line(l)).collect())
    }

    /// `NLST`: bare names.
    pub fn nlst(&mut self, path: &str) -> Result<Vec<String>> {
        self.listing("NLST", path)
    }

    /// `SIZE` in bytes.
    pub fn size(&mut self, path: &str) -> Result<u64> {
        check_path(path)?;
        let r = self.cmd(&format!("SIZE {path}"))?;
        if r.code != 213 {
            return Err(self.reply_error("SIZE", path, r));
        }
        r.text
            .get(4..)
            .and_then(|s| s.trim().parse().ok())
            .ok_or_else(|| Error::protocol(format!("bad SIZE reply {:?}", r.text)))
    }

    /// `RETR`: download a small file (thumbnails, logs) into memory.
    pub fn retrieve(&mut self, path: &str) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.retrieve_to(path, &mut out, &mut |_, _| {})?;
        Ok(out)
    }

    /// `RETR` into a writer, so a long video is never held in memory. `progress(got, total)` is
    /// called after every chunk. A transfer shorter than the file's `SIZE` is an error: some
    /// firmware drops the data channel without saying so.
    pub fn retrieve_to(&mut self, path: &str, out: &mut dyn Write, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<u64> {
        check_path(path)?;
        let _ = self.cmd("TYPE I")?;
        // not every server answers SIZE; without it only the 226 says the file is whole
        let total = self.size(path).ok();
        let mut data = self.start_transfer(&format!("RETR {path}"), "RETR", path)?;
        let got = Self::copy_all(&mut data, out, total, progress);
        data.close();
        drop(data);
        let got = got?;
        self.finish_transfer("RETR", path, self.opts.timeout)?;
        match total {
            Some(n) if n != got => Err(Error::protocol(format!("{path} is {n} bytes but the printer sent {got}"))),
            _ => Ok(got),
        }
    }

    fn copy_all(data: &mut TlsStream, out: &mut dyn Write, total: Option<u64>, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<u64> {
        let (mut got, mut buf) = (0u64, vec![0u8; 64 * 1024]);
        loop {
            match data.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    out.write_all(&buf[..n])?;
                    got += n as u64;
                    progress(got, total);
                }
                // vsftpd may close the data socket without close_notify.
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) if is_timeout(&e) => return Err(Error::timeout("FTP data transfer")),
                Err(e) => return Err(Error::Io(e)),
            }
        }
        Ok(got)
    }

    /// `DELE`.
    pub fn delete(&mut self, path: &str) -> Result<()> {
        check_path(path)?;
        let r = self.cmd(&format!("DELE {path}"))?;
        if r.code != 250 {
            return Err(self.reply_error("DELE", path, r));
        }
        Ok(())
    }

    /// `MKD`.
    pub fn mkdir(&mut self, path: &str) -> Result<()> {
        check_path(path)?;
        let r = self.cmd(&format!("MKD {path}"))?;
        if r.code != 257 {
            return Err(self.reply_error("MKD", path, r));
        }
        Ok(())
    }

    /// `STOR` from a reader. `progress(sent, total)` is called after every
    /// chunk; returning `false` cancels (a partial file may remain).
    ///
    /// The upload counts as successful on `226`, or — because Bambu firmware
    /// sometimes answers `426`/times out even though the file landed — when a
    /// follow-up `SIZE` equals the bytes sent.
    pub fn store(
        &mut self,
        remote_path: &str,
        reader: &mut dyn Read,
        total: Option<u64>,
        mut progress: Option<&mut dyn FnMut(u64, Option<u64>) -> bool>,
    ) -> Result<u64> {
        check_path(remote_path)?;
        let mut data = self.start_transfer(&format!("STOR {remote_path}"), "STOR", "")?;
        let mut buf = vec![0u8; self.opts.chunk_size.max(1024)];
        let mut sent: u64 = 0;
        let mut cancelled = false;
        let write_result: Result<()> = (|| {
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) => return Ok(()),
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(Error::Io(e)),
                };
                data.write_all(&buf[..n]).map_err(|e| {
                    if is_timeout(&e) {
                        Error::timeout("FTP upload (printer stopped reading)")
                    } else {
                        map_tls_io_error(e, &self.tls.verifier, "FTP upload")
                    }
                })?;
                sent += n as u64;
                if let Some(cb) = progress.as_mut() {
                    if !cb(sent, total) {
                        cancelled = true;
                        return Ok(());
                    }
                }
            }
        })();
        // close_notify, then half-close so vsftpd sees EOF and finalises the file.
        data.close();
        data.shutdown_tcp(std::net::Shutdown::Write);
        if let Err(e) = write_result {
            let _ = self.read_reply(Duration::from_secs(5));
            return Err(e);
        }
        let final_reply = self.read_reply(self.opts.final_reply_timeout);
        drop(data);
        if cancelled {
            return Err(Error::Cancelled { sent });
        }
        match final_reply {
            Ok(r) if r.code == 226 || r.code == 250 => Ok(sent),
            Ok(r) if matches!(r.code, 452 | 552 | 553 | 530 | 522) => Err(self.reply_error("STOR", "", r)),
            other => {
                // 426 / timeout: trust SIZE if it matches exactly.
                match self.size(remote_path) {
                    Ok(sz) if sz == sent => Ok(sent),
                    _ => match other {
                        Ok(r) => Err(self.reply_error("STOR", "", r)),
                        Err(e) => Err(e),
                    },
                }
            }
        }
    }

    /// `QUIT` and close.
    pub fn quit(mut self) -> Result<()> {
        let r = self.cmd("QUIT");
        self.ctrl.close();
        self.ctrl.shutdown_tcp(std::net::Shutdown::Both);
        r.map(|_| ())
    }
}

/// What to upload.
#[derive(Debug, Clone, Copy)]
pub enum UploadSource<'a> {
    Path(&'a Path),
    Bytes(&'a [u8]),
}

impl<'a> From<&'a Path> for UploadSource<'a> {
    fn from(p: &'a Path) -> Self {
        UploadSource::Path(p)
    }
}
impl<'a> From<&'a PathBuf> for UploadSource<'a> {
    fn from(p: &'a PathBuf) -> Self {
        UploadSource::Path(p.as_path())
    }
}
impl<'a> From<&'a [u8]> for UploadSource<'a> {
    fn from(b: &'a [u8]) -> Self {
        UploadSource::Bytes(b)
    }
}
impl<'a> From<&'a Vec<u8>> for UploadSource<'a> {
    fn from(b: &'a Vec<u8>) -> Self {
        UploadSource::Bytes(b.as_slice())
    }
}

/// Result of [`upload_file`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadResult {
    pub remote_name: String,
    pub bytes: u64,
    /// Lower-case hex MD5 of the uploaded bytes.
    pub md5: String,
    /// Printer certificate fingerprint seen during the upload.
    pub cert_sha256: Option<String>,
}

struct Md5Reader<R> {
    inner: R,
    hasher: Md5,
}

impl<R: Read> Read for Md5Reader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
}

/// Upload a file (or bytes) into the root of the printer storage as
/// `remote_name` (a plain file name such as `part.gcode.3mf`), then verify the
/// size. Opens and closes its own FTPS session.
pub fn upload_file<'a>(
    cfg: &PrinterConfig,
    source: impl Into<UploadSource<'a>>,
    remote_name: &str,
    progress: Option<&mut dyn FnMut(u64, Option<u64>) -> bool>,
) -> Result<UploadResult> {
    upload_file_with(cfg, source, remote_name, progress, FtpOptions::default())
}

/// [`upload_file`] with explicit [`FtpOptions`].
pub fn upload_file_with<'a>(
    cfg: &PrinterConfig,
    source: impl Into<UploadSource<'a>>,
    remote_name: &str,
    progress: Option<&mut dyn FnMut(u64, Option<u64>) -> bool>,
    opts: FtpOptions,
) -> Result<UploadResult> {
    validate_remote_name(remote_name)?;
    let source = source.into();
    let (reader, total): (Box<dyn Read + '_>, u64) = match source {
        UploadSource::Path(p) => {
            let f = File::open(p)?;
            let len = f.metadata()?.len();
            (Box::new(BufReader::with_capacity(256 * 1024, f)), len)
        }
        UploadSource::Bytes(b) => (Box::new(b), b.len() as u64),
    };
    let mut reader = Md5Reader {
        inner: reader,
        hasher: Md5::new(),
    };
    let mut ftp = FtpsClient::connect_with(cfg, opts)?;
    let remote_path = format!("/{remote_name}");
    let sent = ftp.store(&remote_path, &mut reader, Some(total), progress)?;
    if sent != total {
        return Err(Error::protocol(format!(
            "local file changed during upload: sent {sent} of {total} bytes"
        )));
    }
    let cert_sha256 = ftp.peer_fingerprint();
    let _ = ftp.quit();
    Ok(UploadResult {
        remote_name: remote_name.to_string(),
        bytes: sent,
        md5: hex(&reader.hasher.finalize()),
        cert_sha256,
    })
}

/// List the storage root.
pub fn list_files(cfg: &PrinterConfig) -> Result<Vec<RemoteFile>> {
    list_files_with(cfg, FtpOptions::default())
}

/// [`list_files`] with explicit [`FtpOptions`].
pub fn list_files_with(cfg: &PrinterConfig, opts: FtpOptions) -> Result<Vec<RemoteFile>> {
    let mut ftp = FtpsClient::connect_with(cfg, opts)?;
    let files = ftp.list("/")?;
    let _ = ftp.quit();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasv_variants() {
        assert_eq!(
            parse_pasv("227 Entering Passive Mode (192,168,1,50,195,80).").unwrap(),
            (Ipv4Addr::new(192, 168, 1, 50), 195 * 256 + 80)
        );
        assert_eq!(
            parse_pasv("227 Entering Passive Mode (0,0,0,0,4,1)").unwrap(),
            (Ipv4Addr::UNSPECIFIED, 1025)
        );
        assert_eq!(
            parse_pasv("227 Entering Passive Mode 10,0,0,7,200,10").unwrap(),
            (Ipv4Addr::new(10, 0, 0, 7), 200 * 256 + 10)
        );
        assert_eq!(
            parse_pasv("227 =127,0,0,1,0,21").unwrap(),
            (Ipv4Addr::LOCALHOST, 21)
        );
        assert!(parse_pasv("227 Entering Passive Mode (1,2,3,4,5)").is_err());
        assert!(parse_pasv("227 Entering Passive Mode (1,2,3,400,5,6)").is_err());
        assert!(parse_pasv("227 Entering Passive Mode (1,2,3,4,0,0)").is_err());
        assert!(parse_pasv("").is_err());
    }

    #[test]
    fn list_lines() {
        let f = parse_list_line("-rw-rw-rw-   1 root  root   1234567 Nov 14 12:00 my part v2.gcode.3mf").unwrap();
        assert_eq!(f.name, "my part v2.gcode.3mf");
        assert_eq!(f.size, Some(1_234_567));
        assert!(!f.is_dir);
        assert_eq!(f.modified.as_deref(), Some("Nov 14 12:00"));
        let d = parse_list_line("drwxr-xr-x 2 0 0 4096 Jan 01 2024 timelapse\r").unwrap();
        assert!(d.is_dir);
        assert_eq!(d.name, "timelapse");
        let l = parse_list_line("lrwxrwxrwx 1 0 0 4 Jan 01 2024 link -> target").unwrap();
        assert_eq!(l.name, "link");
        assert!(parse_list_line("total 12").is_none());
        assert!(parse_list_line("drwxr-xr-x 2 0 0 4096 Jan 01 2024 .").is_none());
        assert!(parse_list_line("-rw short").is_none());
    }

    #[test]
    fn rejects_crlf_paths() {
        assert!(check_path("a\r\nDELE b").is_err());
        assert!(check_path("/ok name.3mf").is_ok());
    }
}
