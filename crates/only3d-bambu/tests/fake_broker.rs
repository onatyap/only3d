//! Integration tests against an in-process fake Bambu MQTT broker over TLS
//! (self-signed cert from rcgen). Nothing here touches the network beyond
//! 127.0.0.1, and nothing can start a print.

mod common;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use only3d_bambu::mqtt::{Connect, Packet, Publish, QoS};
use only3d_bambu::{ClientOptions, Command, Event, GcodeState, LightNode, PrinterClient, PrinterConfig};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use serde_json::{json, Value};

#[derive(Clone)]
struct Behavior {
    connack: u8,
    suback: u8,
    /// Answer pushall with reports.
    reply_pushall: bool,
    /// Close the TCP connection right after answering the first pushall.
    drop_first_session: bool,
}

impl Default for Behavior {
    fn default() -> Self {
        Behavior {
            connack: 0,
            suback: 0,
            reply_pushall: true,
            drop_first_session: false,
        }
    }
}

#[derive(Default)]
struct Log {
    connects: Vec<Connect>,
    subscriptions: Vec<String>,
    requests: Vec<(String, QoS, Value)>,
    pings: usize,
    disconnects: usize,
    sessions: usize,
}

struct Broker {
    addr: SocketAddr,
    log: Arc<Mutex<Log>>,
}

impl Broker {
    fn start(cfg: Arc<ServerConfig>, behavior: Behavior) -> Broker {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let log = Arc::new(Mutex::new(Log::default()));
        let log2 = log.clone();
        std::thread::spawn(move || {
            for sock in listener.incoming() {
                let Ok(sock) = sock else { break };
                let (cfg, log, behavior) = (cfg.clone(), log2.clone(), behavior.clone());
                std::thread::spawn(move || {
                    let session = {
                        let mut l = log.lock().unwrap();
                        l.sessions += 1;
                        l.sessions
                    };
                    let _ = serve(sock, cfg, &log, &behavior, session);
                });
            }
        });
        Broker { addr, log }
    }

    fn options(&self) -> ClientOptions {
        ClientOptions {
            port: self.addr.port(),
            connect_timeout: Duration::from_secs(5),
            initial_report_timeout: Some(Duration::from_secs(5)),
            command_timeout: Duration::from_secs(5),
            ..Default::default()
        }
    }
}

fn config() -> PrinterConfig {
    PrinterConfig::new("127.0.0.1", SERIAL, ACCESS_CODE)
}

type Tls = StreamOwned<ServerConnection, TcpStream>;

fn next_packet(tls: &mut Tls, buf: &mut Vec<u8>) -> Option<Packet> {
    loop {
        if let Some((p, n)) = Packet::decode(buf, 1 << 24).ok()? {
            buf.drain(..n);
            return Some(p);
        }
        let mut tmp = [0u8; 8192];
        match tls.read(&mut tmp) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }
    }
}

fn send(tls: &mut Tls, p: &Packet) -> std::io::Result<()> {
    tls.write_all(&p.encode().unwrap())?;
    tls.flush()
}

fn report(tls: &mut Tls, v: &Value) -> std::io::Result<()> {
    send(
        tls,
        &Packet::Publish(Publish {
            topic: format!("device/{SERIAL}/report"),
            payload: serde_json::to_vec(v).unwrap(),
            qos: QoS::AtMostOnce,
            packet_id: None,
            retain: false,
            dup: false,
        }),
    )
}

fn serve(sock: TcpStream, cfg: Arc<ServerConfig>, log: &Mutex<Log>, b: &Behavior, session: usize) -> std::io::Result<()> {
    sock.set_read_timeout(Some(Duration::from_secs(20)))?;
    let mut tls = StreamOwned::new(ServerConnection::new(cfg).unwrap(), sock);
    let mut buf = Vec::new();
    let Some(Packet::Connect(c)) = next_packet(&mut tls, &mut buf) else {
        return Ok(()); // e.g. client aborted the TLS handshake
    };
    let ok_password = c.password.as_deref() == Some(ACCESS_CODE.as_bytes());
    log.lock().unwrap().connects.push(c);
    let code = if ok_password { b.connack } else { 5 };
    send(&mut tls, &Packet::ConnAck { session_present: false, code: only3d_bambu::mqtt::ConnectReturnCode::from_u8(code) })?;
    if code != 0 {
        return Ok(());
    }
    while let Some(p) = next_packet(&mut tls, &mut buf) {
        match p {
            Packet::Subscribe { packet_id, topics } => {
                let wrong = topics.iter().any(|(t, _)| t != &format!("device/{SERIAL}/report"));
                log.lock().unwrap().subscriptions.extend(topics.into_iter().map(|t| t.0));
                let rc = if wrong { 0x80 } else { b.suback };
                send(&mut tls, &Packet::SubAck { packet_id, return_codes: vec![rc] })?;
            }
            Packet::Publish(p) => {
                if let (QoS::AtLeastOnce, Some(id)) = (p.qos, p.packet_id) {
                    send(&mut tls, &Packet::PubAck { packet_id: id })?;
                }
                let v: Value = serde_json::from_slice(&p.payload).unwrap();
                log.lock().unwrap().requests.push((p.topic.clone(), p.qos, v.clone()));
                if v["pushing"]["command"] == "pushall" && b.reply_pushall {
                    report(&mut tls, &sample("h2d_pushall.json"))?;
                    report(&mut tls, &sample("h2d_delta_temps.json"))?;
                    if b.drop_first_session && session == 1 {
                        // Give the test time to subscribe to events first.
                        std::thread::sleep(Duration::from_millis(400));
                        tls.sock.shutdown(std::net::Shutdown::Both)?;
                        return Ok(());
                    }
                } else if v["info"]["command"] == "get_version" {
                    let mut ver = sample("h2d_get_version.json");
                    ver["info"]["sequence_id"] = v["info"]["sequence_id"].clone();
                    report(&mut tls, &ver)?;
                } else if let Some((kind, body)) = v.as_object().and_then(|o| o.iter().next()) {
                    // Generic success reply echoing command + sequence_id.
                    let reply = json!({ kind.clone(): {
                        "command": body["command"], "sequence_id": body["sequence_id"], "result": "success"
                    }});
                    report(&mut tls, &reply)?;
                }
            }
            Packet::PingReq => {
                log.lock().unwrap().pings += 1;
                send(&mut tls, &Packet::PingResp)?;
            }
            Packet::Disconnect => {
                log.lock().unwrap().disconnects += 1;
                return Ok(());
            }
            _ => {}
        }
    }
    Ok(())
}

fn wait_log(log: &Mutex<Log>, timeout: Duration, pred: impl Fn(&Log) -> bool) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if pred(&log.lock().unwrap()) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn connect_subscribe_pushall_status_and_commands() {
    let cert = make_cert();
    let broker = Broker::start(server_config(&cert, rustls::ALL_VERSIONS), Behavior::default());
    let client = PrinterClient::connect_with(&config(), broker.options()).unwrap();

    // TOFU: unpinned connect reports the observed fingerprint.
    assert_eq!(client.tls_fingerprint().as_deref(), Some(cert.sha256.as_str()));

    // CONNECT / SUBSCRIBE / pushall as the printer expects them.
    {
        let log = broker.log.lock().unwrap();
        let c = &log.connects[0];
        assert_eq!(c.username.as_deref(), Some("bblp"));
        assert_eq!(c.password.as_deref(), Some(ACCESS_CODE.as_bytes()));
        assert!(c.client_id.starts_with("only3d-"));
        assert!(c.clean_session);
        assert_eq!(log.subscriptions, vec![format!("device/{SERIAL}/report")]);
        let (topic, _, first) = &log.requests[0];
        assert_eq!(topic, &format!("device/{SERIAL}/request"));
        assert_eq!(first["pushing"]["command"], "pushall");
        assert!(first["pushing"]["sequence_id"].is_string());
    }

    // Full report then delta, merged: left nozzle heats to 220 in the delta.
    let st = client
        .wait_for(|s| s.nozzle(1).and_then(|n| n.temperature.target) == Some(220.0), Duration::from_secs(5))
        .unwrap();
    assert_eq!(st.gcode_state, GcodeState::Running);
    assert_eq!(st.nozzle(0).unwrap().temperature.current, Some(245.0));
    assert_eq!(st.active_extruder, Some(1));
    assert_eq!(st.ams.len(), 2);
    assert_eq!(client.raw_state()["print"]["subtask_name"], "T-pose - slim H2D dual AMS riser");

    // get_version answered → product name → model.
    client
        .wait_for(|s| s.product_name.is_some(), Duration::from_secs(5))
        .unwrap();
    assert_eq!(client.model_info().unwrap().code, "O1D");
    assert_eq!(client.status().firmware_version.as_deref(), Some("01.02.00.00"));

    // QoS 1 command gets PUBACKed; the reply arrives as an event.
    let events = client.subscribe();
    let seq = client.send(&Command::Light { node: LightNode::ChamberLight, on: false }).unwrap();
    let reply = loop {
        match events.recv_timeout(Duration::from_secs(5)).unwrap() {
            Event::Message(m) if m.get("system").is_some() => break m,
            _ => {}
        }
    };
    assert_eq!(reply["system"]["sequence_id"], seq.as_str());
    {
        let log = broker.log.lock().unwrap();
        let (_, qos, last) = log.requests.last().unwrap();
        assert_eq!(*qos, QoS::AtLeastOnce);
        assert_eq!(last["system"]["command"], "ledctrl");
        assert_eq!(last["system"]["led_mode"], "off");
    }

    // request() correlates by sequence_id.
    let r = client.request(&Command::Pause, Duration::from_secs(5)).unwrap();
    assert_eq!(r["print"]["command"], "pause");
    assert_eq!(r["print"]["result"], "success");

    client.disconnect();
    assert!(wait_log(&broker.log, Duration::from_secs(3), |l| l.disconnects == 1));
}

#[test]
fn fingerprint_pinning_accepts_match_and_rejects_mismatch() {
    let cert = make_cert();
    let broker = Broker::start(server_config(&cert, rustls::ALL_VERSIONS), Behavior::default());

    let mut cfg = config();
    cfg.cert_sha256 = Some(pretty_fp(&cert.sha256));
    let client = PrinterClient::connect_with(&cfg, broker.options()).unwrap();
    assert_eq!(client.tls_fingerprint().as_deref(), Some(cert.sha256.as_str()));
    client.disconnect();
    let connects_before = broker.log.lock().unwrap().connects.len();

    cfg.cert_sha256 = Some("00".repeat(32));
    let err = PrinterClient::connect_with(&cfg, broker.options()).unwrap_err();
    assert_eq!(err.code(), "tls_fingerprint_mismatch", "{err}");
    assert!(!err.hint().is_empty());
    match &err {
        only3d_bambu::Error::TlsFingerprintMismatch { expected, actual } => {
            assert_eq!(expected, &"00".repeat(32));
            assert_eq!(actual, &cert.sha256);
        }
        other => panic!("unexpected {other:?}"),
    }
    // The access code never left the client.
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(broker.log.lock().unwrap().connects.len(), connects_before);
}

#[test]
fn tls12_only_broker_like_a_printer() {
    let cert = make_cert();
    let broker = Broker::start(server_config(&cert, &[&rustls::version::TLS12]), Behavior::default());
    let client = PrinterClient::connect_with(&config(), broker.options()).unwrap();
    assert_eq!(client.status().gcode_state, GcodeState::Running);
}

#[test]
fn bad_access_code_is_auth_failed() {
    let cert = make_cert();
    let broker = Broker::start(server_config(&cert, rustls::ALL_VERSIONS), Behavior::default());
    let mut cfg = config();
    cfg.access_code = "wrong123".into();
    let err = PrinterClient::connect_with(&cfg, broker.options()).unwrap_err();
    assert_eq!(err.code(), "auth_failed", "{err}");
    assert!(err.hint().contains("access code"));
}

#[test]
fn rejected_subscription_is_wrong_serial() {
    let cert = make_cert();
    let broker = Broker::start(
        server_config(&cert, rustls::ALL_VERSIONS),
        Behavior { suback: 0x80, ..Default::default() },
    );
    let err = PrinterClient::connect_with(&config(), broker.options()).unwrap_err();
    assert_eq!(err.code(), "wrong_serial", "{err}");
}

#[test]
fn silent_printer_times_out_with_serial_hint() {
    let cert = make_cert();
    let broker = Broker::start(
        server_config(&cert, rustls::ALL_VERSIONS),
        Behavior { reply_pushall: false, ..Default::default() },
    );
    let mut opts = broker.options();
    opts.initial_report_timeout = Some(Duration::from_millis(400));
    let err = PrinterClient::connect_with(&config(), opts).unwrap_err();
    assert_eq!(err.code(), "timeout");
    assert!(err.to_string().contains("serial"), "{err}");
}

#[test]
fn unreachable_port() {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let opts = ClientOptions { port, connect_timeout: Duration::from_secs(2), ..Default::default() };
    let err = PrinterClient::connect_with(&config(), opts).unwrap_err();
    assert_eq!(err.code(), "unreachable", "{err}");
}

#[test]
fn reconnects_after_drop_and_keeps_state() {
    let cert = make_cert();
    let broker = Broker::start(
        server_config(&cert, rustls::ALL_VERSIONS),
        Behavior { drop_first_session: true, ..Default::default() },
    );
    let client = PrinterClient::connect_with(&config(), broker.options()).unwrap();
    let events = client.subscribe();
    let mut saw_disconnect = false;
    let end = Instant::now() + Duration::from_secs(8);
    loop {
        let left = end.saturating_duration_since(Instant::now());
        match events.recv_timeout(left).expect("expected reconnect events") {
            Event::Disconnected { will_reconnect, .. } => {
                assert!(will_reconnect);
                saw_disconnect = true;
            }
            Event::Connected if saw_disconnect => break,
            _ => {}
        }
    }
    assert!(client.is_connected());
    // Second session re-subscribed and re-requested the full state.
    assert!(wait_log(&broker.log, Duration::from_secs(3), |l| {
        l.sessions == 2
            && l.subscriptions.len() == 2
            && l.requests.iter().filter(|r| r.2["pushing"]["command"] == "pushall").count() == 2
    }));
    assert_eq!(client.status().gcode_state, GcodeState::Running);
}

#[test]
fn keepalive_pings() {
    let cert = make_cert();
    let broker = Broker::start(server_config(&cert, rustls::ALL_VERSIONS), Behavior::default());
    let mut opts = broker.options();
    opts.keep_alive = Duration::from_secs(2);
    let client = PrinterClient::connect_with(&config(), opts).unwrap();
    assert!(wait_log(&broker.log, Duration::from_secs(4), |l| l.pings >= 2));
    assert!(client.is_connected());
}

#[test]
fn commands_fail_fast_when_closed() {
    let cert = make_cert();
    let broker = Broker::start(
        server_config(&cert, rustls::ALL_VERSIONS),
        Behavior { drop_first_session: true, ..Default::default() },
    );
    let mut opts = broker.options();
    opts.auto_reconnect = false;
    let client = PrinterClient::connect_with(&config(), opts).unwrap();
    let st = client.wait_for(|_| false, Duration::from_secs(3));
    assert_eq!(st.unwrap_err().code(), "connection_lost");
    let err = client.send(&Command::PushAll).unwrap_err();
    assert_eq!(err.code(), "not_connected");
}
