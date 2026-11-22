//! Integration tests against an in-process implicit-FTPS server that, like
//! the printer's vsftpd (`require_ssl_reuse=YES`), refuses any data channel
//! whose TLS session was not resumed from the control channel (522).

mod common;

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use md5::{Digest, Md5};
use only3d_bambu::{FtpOptions, FtpsClient, PrinterConfig};
use rustls::{HandshakeKind, ServerConfig, ServerConnection, StreamOwned};

#[derive(Default)]
struct FtpState {
    files: BTreeMap<String, Vec<u8>>,
    commands: Vec<String>,
    data_channels: Vec<bool>,
    rejected_522: usize,
    /// RETR sends only this many bytes, and still answers 226.
    cut_short: Option<usize>,
}

struct FtpServer {
    addr: SocketAddr,
    state: Arc<Mutex<FtpState>>,
}

type Tls = StreamOwned<ServerConnection, TcpStream>;

impl FtpServer {
    fn start(cfg: Arc<ServerConfig>) -> FtpServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(FtpState::default()));
        let st = state.clone();
        std::thread::spawn(move || {
            for sock in listener.incoming() {
                let Ok(sock) = sock else { break };
                let (cfg, st) = (cfg.clone(), st.clone());
                std::thread::spawn(move || {
                    let _ = control(sock, cfg, &st);
                });
            }
        });
        FtpServer { addr, state }
    }

    fn opts(&self) -> FtpOptions {
        FtpOptions {
            port: self.addr.port(),
            timeout: Duration::from_secs(5),
            final_reply_timeout: Duration::from_secs(5),
            chunk_size: 4096,
            ..Default::default()
        }
    }
}

fn reply(tls: &mut Tls, line: &str) -> std::io::Result<()> {
    tls.write_all(format!("{line}\r\n").as_bytes())?;
    tls.flush()
}

fn read_line(tls: &mut Tls, buf: &mut Vec<u8>) -> Option<String> {
    loop {
        if let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let line = String::from_utf8_lossy(&buf[..pos]).trim_end_matches('\r').to_string();
            buf.drain(..=pos);
            return Some(line);
        }
        let mut tmp = [0u8; 4096];
        match tls.read(&mut tmp) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }
    }
}

/// Accept the data connection and require TLS session resumption.
fn accept_data(listener: &TcpListener, cfg: &Arc<ServerConfig>, st: &Mutex<FtpState>) -> Option<Tls> {
    let (sock, _) = listener.accept().ok()?;
    sock.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    let mut tls = StreamOwned::new(ServerConnection::new(cfg.clone()).unwrap(), sock);
    while tls.conn.is_handshaking() {
        tls.conn.complete_io(&mut tls.sock).ok()?;
    }
    let resumed = tls.conn.handshake_kind() == Some(HandshakeKind::Resumed);
    st.lock().unwrap().data_channels.push(resumed);
    resumed.then_some(tls)
}

fn control(sock: TcpStream, cfg: Arc<ServerConfig>, st: &Mutex<FtpState>) -> std::io::Result<()> {
    sock.set_read_timeout(Some(Duration::from_secs(20)))?;
    let mut tls = StreamOwned::new(ServerConnection::new(cfg.clone()).unwrap(), sock);
    reply(&mut tls, "220 (vsFTPd 3.0.3)")?;
    let mut buf = Vec::new();
    let mut authed = false;
    let mut pasv: Option<TcpListener> = None;
    while let Some(line) = read_line(&mut tls, &mut buf) {
        st.lock().unwrap().commands.push(line.clone());
        let (verb, arg) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        let arg = arg.to_string();
        match verb {
            "USER" => reply(&mut tls, "331 Please specify the password.")?,
            "PASS" => {
                if arg == ACCESS_CODE {
                    authed = true;
                    reply(&mut tls, "230 Login successful.")?;
                } else {
                    reply(&mut tls, "530 Login incorrect.")?;
                }
            }
            _ if !authed => reply(&mut tls, "530 Please login with USER and PASS.")?,
            "PBSZ" => reply(&mut tls, "200 PBSZ set to 0.")?,
            "PROT" => reply(&mut tls, "200 PROT now Private.")?,
            "TYPE" => reply(&mut tls, "200 Switching to Binary mode.")?,
            "PASV" => {
                let l = TcpListener::bind("127.0.0.1:0")?;
                let port = l.local_addr()?.port();
                pasv = Some(l);
                // Like some printers: advertise 0.0.0.0, client must use the control host.
                reply(&mut tls, &format!("227 Entering Passive Mode (0,0,0,0,{},{}).", port >> 8, port & 0xff))?;
            }
            "STOR" | "LIST" | "NLST" | "RETR" => {
                let Some(l) = pasv.take() else {
                    reply(&mut tls, "425 Use PORT or PASV first.")?;
                    continue;
                };
                reply(&mut tls, "150 Ok to send data.")?;
                let Some(mut data) = accept_data(&l, &cfg, st) else {
                    st.lock().unwrap().rejected_522 += 1;
                    reply(&mut tls, "522 SSL connection failed: session reuse required")?;
                    continue;
                };
                if verb == "STOR" {
                    let bytes = read_to_end_lenient(&mut data)?;
                    st.lock().unwrap().files.insert(arg.clone(), bytes);
                    reply(&mut tls, "226 Transfer complete.")?;
                } else if verb == "RETR" {
                    let mut bytes = st.lock().unwrap().files.get(&arg).cloned().unwrap_or_default();
                    bytes.truncate(st.lock().unwrap().cut_short.unwrap_or(usize::MAX));
                    data.write_all(&bytes)?;
                    data.conn.send_close_notify();
                    data.flush()?;
                    drop(data);
                    reply(&mut tls, "226 Transfer complete.")?;
                } else {
                    let listing: String = st
                        .lock()
                        .unwrap()
                        .files
                        .iter()
                        .map(|(name, b)| {
                            let name = name.trim_start_matches('/');
                            if verb == "LIST" {
                                format!("-rw-r--r--    1 0        0        {:>8} Nov 14 12:00 {name}\r\n", b.len())
                            } else {
                                format!("{name}\r\n")
                            }
                        })
                        .collect();
                    data.write_all(listing.as_bytes())?;
                    data.conn.send_close_notify();
                    data.flush()?;
                    drop(data);
                    reply(&mut tls, "226 Directory send OK.")?;
                }
            }
            "SIZE" => {
                let size = st.lock().unwrap().files.get(&arg).map(Vec::len);
                match size {
                    Some(n) => reply(&mut tls, &format!("213 {n}"))?,
                    None => reply(&mut tls, "550 Could not get file size.")?,
                }
            }
            "DELE" => {
                let removed = st.lock().unwrap().files.remove(&arg).is_some();
                reply(&mut tls, if removed { "250 Delete operation successful." } else { "550 Delete operation failed." })?;
            }
            "QUIT" => {
                reply(&mut tls, "221 Goodbye.")?;
                return Ok(());
            }
            _ => reply(&mut tls, "502 Command not implemented.")?,
        }
    }
    Ok(())
}

fn config() -> PrinterConfig {
    PrinterConfig::new("127.0.0.1", SERIAL, ACCESS_CODE)
}

fn payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 31 % 251) as u8).collect()
}

fn roundtrip(versions: &[&'static rustls::SupportedProtocolVersion]) {
    let cert = make_cert();
    let server = FtpServer::start(server_config(&cert, versions));
    let data = payload(200_000);
    let mut progress = Vec::new();
    let mut cb = |sent: u64, total: Option<u64>| {
        progress.push((sent, total));
        true
    };
    let res = only3d_bambu::upload_file_with(&config(), &data, "part v2.gcode.3mf", Some(&mut cb), server.opts()).unwrap();
    assert_eq!(res.remote_name, "part v2.gcode.3mf");
    assert_eq!(res.bytes, data.len() as u64);
    assert_eq!(res.md5, format!("{:x}", Md5::digest(&data)));
    assert_eq!(res.cert_sha256.as_deref(), Some(cert.sha256.as_str()));
    assert_eq!(progress.last(), Some(&(data.len() as u64, Some(data.len() as u64))));
    assert!(progress.windows(2).all(|w| w[0].0 < w[1].0));
    {
        let st = server.state.lock().unwrap();
        assert_eq!(st.files.get("/part v2.gcode.3mf"), Some(&data));
        assert_eq!(st.rejected_522, 0);
        assert!(st.data_channels.iter().all(|r| *r), "every data channel resumed");
        assert!(st.commands.iter().any(|c| c == "PBSZ 0"));
        assert!(st.commands.iter().any(|c| c == "PROT P"));
        assert!(st.commands.iter().any(|c| c == "TYPE I"));
    }

    let files = only3d_bambu::list_files_with(&config(), server.opts()).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].name, "part v2.gcode.3mf");
    assert_eq!(files[0].size, Some(data.len() as u64));

    let mut ftp = FtpsClient::connect_with(&config(), server.opts()).unwrap();
    assert_eq!(ftp.nlst("/").unwrap(), vec!["part v2.gcode.3mf".to_string()]);
    assert_eq!(ftp.last_data_channel_resumed(), Some(true));
    assert_eq!(ftp.size("/part v2.gcode.3mf").unwrap(), data.len() as u64);
    assert_eq!(ftp.retrieve("/part v2.gcode.3mf").unwrap(), data);
    ftp.delete("/part v2.gcode.3mf").unwrap();
    let err = ftp.size("/part v2.gcode.3mf").unwrap_err();
    assert_eq!(err.code(), "file_not_found");
    ftp.quit().unwrap();
    // Four data channels (STOR, LIST, NLST, RETR), all resumed.
    let st = server.state.lock().unwrap();
    assert_eq!(st.data_channels, vec![true, true, true, true]);
}

#[test]
fn upload_list_delete_tls12_session_reuse() {
    // Printers' vsftpd only negotiates TLS 1.2 on :990.
    roundtrip(&[&rustls::version::TLS12]);
}

#[test]
fn upload_list_delete_tls13_session_reuse() {
    roundtrip(&[&rustls::version::TLS13]);
}

#[test]
fn upload_from_path_and_cancel() {
    let cert = make_cert();
    let server = FtpServer::start(server_config(&cert, rustls::ALL_VERSIONS));
    let dir = std::env::temp_dir().join(format!("only3d-bambu-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cube.gcode.3mf");
    let data = payload(50_000);
    std::fs::write(&path, &data).unwrap();
    let res = only3d_bambu::upload_file_with(&config(), &path, "cube.gcode.3mf", None, server.opts()).unwrap();
    assert_eq!(res.bytes, 50_000);
    assert_eq!(server.state.lock().unwrap().files["/cube.gcode.3mf"], data);

    let mut stop = |sent: u64, _: Option<u64>| sent < 8192;
    let err = only3d_bambu::upload_file_with(&config(), &path, "partial.3mf", Some(&mut stop), server.opts()).unwrap_err();
    assert_eq!(err.code(), "cancelled");
    std::fs::remove_dir_all(&dir).unwrap();

    let err = only3d_bambu::upload_file_with(&config(), &data, "../escape.3mf", None, server.opts()).unwrap_err();
    assert_eq!(err.code(), "invalid_input");
}

#[test]
fn ftps_bad_password_and_pin_mismatch() {
    let cert = make_cert();
    let server = FtpServer::start(server_config(&cert, &[&rustls::version::TLS12]));
    let mut cfg = config();
    cfg.access_code = "nope0000".into();
    let err = FtpsClient::connect_with(&cfg, server.opts()).unwrap_err();
    assert_eq!(err.code(), "auth_failed", "{err}");

    let mut cfg = config();
    cfg.cert_sha256 = Some("ab".repeat(32));
    let before = server.state.lock().unwrap().commands.len();
    let err = only3d_bambu::list_files_with(&cfg, server.opts()).unwrap_err();
    assert_eq!(err.code(), "tls_fingerprint_mismatch", "{err}");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(server.state.lock().unwrap().commands.len(), before, "no command (no PASS) sent");

    cfg.cert_sha256 = Some(pretty_fp(&cert.sha256));
    assert!(only3d_bambu::list_files_with(&cfg, server.opts()).unwrap().is_empty());
}

#[test]
fn a_download_streams_with_progress_and_notices_a_short_file() {
    let cert = make_cert();
    let server = FtpServer::start(server_config(&cert, &[&rustls::version::TLS12]));
    let (path, data) = ("/timelapse/video_2026-11-15_12-00-07.mp4", payload(300_000));
    server.state.lock().unwrap().files.insert(path.into(), data.clone());
    let mut ftp = FtpsClient::connect_with(&config(), server.opts()).unwrap();
    let (mut out, mut seen) = (Vec::new(), Vec::new());
    assert_eq!(ftp.retrieve_to(path, &mut out, &mut |got, total| seen.push((got, total))).unwrap(), 300_000);
    assert_eq!(out, data);
    assert_eq!(seen.last(), Some(&(300_000, Some(300_000))));
    assert!(seen.windows(2).all(|w| w[0].0 < w[1].0));

    // the printer closed the data channel early and called it done
    server.state.lock().unwrap().cut_short = Some(100_000);
    let err = ftp.retrieve_to(path, &mut Vec::new(), &mut |_, _| {}).unwrap_err();
    assert!(err.to_string().contains("300000") && err.to_string().contains("100000"), "{err}");
    // the session still works
    assert_eq!(ftp.size(path).unwrap(), 300_000);
    ftp.quit().unwrap();
}
