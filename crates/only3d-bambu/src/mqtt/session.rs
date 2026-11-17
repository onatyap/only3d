//! One synchronous MQTT-over-TLS session: connect, CONNACK, SUBSCRIBE/SUBACK,
//! then packet-level send/poll for the client's I/O thread.

use std::io::Write;
use std::time::{Duration, Instant};

use crate::config::PrinterConfig;
use crate::error::{is_timeout, Error, Result};
use crate::mqtt::codec::{Connect, ConnectReturnCode, Packet, Publish, QoS};
use crate::tls::{map_tls_io_error, server_name, tcp_connect, TlsSetup, TlsStream};

pub(crate) struct SessionParams<'a> {
    pub cfg: &'a PrinterConfig,
    pub tls: &'a TlsSetup,
    pub port: u16,
    pub timeout: Duration,
    pub keep_alive: Duration,
    pub client_id: String,
    pub max_packet: usize,
}

pub(crate) struct MqttSession {
    stream: TlsStream,
    tls: TlsSetup,
    next_id: u16,
    max_packet: usize,
    read_timeout: Option<Duration>,
}

impl MqttSession {
    /// TCP + TLS + CONNECT/CONNACK + SUBSCRIBE/SUBACK on the report topic.
    /// Returns the session and any PUBLISH that raced ahead of the SUBACK.
    pub(crate) fn establish(p: &SessionParams<'_>) -> Result<(MqttSession, Vec<Publish>)> {
        let host = p.cfg.host_trimmed();
        let sock = tcp_connect(host, p.port, p.timeout)?;
        let stream = TlsStream::handshake(
            sock,
            p.tls.config.clone(),
            server_name(host)?,
            p.timeout,
            &p.tls.verifier,
        )?;
        stream.set_write_timeout(Some(p.timeout.max(Duration::from_secs(5))))?;
        let mut s = MqttSession {
            stream,
            tls: p.tls.clone(),
            next_id: 0,
            max_packet: p.max_packet,
            read_timeout: None,
        };
        let deadline = Instant::now() + p.timeout;

        s.send(&Packet::Connect(Connect {
            client_id: p.client_id.clone(),
            username: Some("bblp".into()),
            password: Some(p.cfg.access_code.as_bytes().to_vec()),
            keep_alive_secs: p.keep_alive.as_secs().clamp(5, 65_535) as u16,
            clean_session: true,
        }))?;
        match s.read_until(deadline, "CONNACK") {
            Ok(Packet::ConnAck { code: ConnectReturnCode::Accepted, .. }) => {}
            Ok(Packet::ConnAck {
                code: code @ (ConnectReturnCode::BadUserNameOrPassword | ConnectReturnCode::NotAuthorized),
                ..
            }) => {
                return Err(Error::AuthFailed {
                    detail: format!("MQTT CONNACK {code:?} from {host}"),
                })
            }
            Ok(Packet::ConnAck { code, .. }) => {
                return Err(Error::protocol(format!("MQTT broker refused connection: {code:?}")))
            }
            Ok(other) => return Err(Error::protocol(format!("expected CONNACK, got {other:?}"))),
            // Some brokers just drop the socket on bad credentials.
            Err(Error::ConnectionLost { detail }) => {
                return Err(Error::AuthFailed {
                    detail: format!("printer closed the connection before CONNACK ({detail})"),
                })
            }
            Err(e) => return Err(e),
        }

        let sub_id = s.next_packet_id();
        let topic = p.cfg.report_topic();
        s.send(&Packet::Subscribe {
            packet_id: sub_id,
            topics: vec![(topic.clone(), QoS::AtMostOnce)],
        })?;
        let mut early = Vec::new();
        loop {
            match s.read_until(deadline, "SUBACK") {
                Ok(Packet::SubAck {
                    packet_id,
                    return_codes,
                }) if packet_id == sub_id => {
                    if return_codes.first().is_none_or(|c| *c >= 0x80) {
                        return Err(Error::WrongSerial {
                            serial: p.cfg.serial.clone(),
                            detail: format!("SUBACK {return_codes:02x?} for {topic}"),
                        });
                    }
                    break;
                }
                Ok(Packet::Publish(publish)) => {
                    s.ack_if_needed(&publish)?;
                    early.push(publish);
                }
                Ok(_) => {}
                Err(Error::ConnectionLost { detail }) => {
                    return Err(Error::WrongSerial {
                        serial: p.cfg.serial.clone(),
                        detail: format!("printer closed the connection after SUBSCRIBE {topic} ({detail})"),
                    })
                }
                Err(e) => return Err(e),
            }
        }
        Ok((s, early))
    }

    pub(crate) fn next_packet_id(&mut self) -> u16 {
        self.next_id = self.next_id.wrapping_add(1);
        if self.next_id == 0 {
            self.next_id = 1;
        }
        self.next_id
    }

    pub(crate) fn send(&mut self, packet: &Packet) -> Result<()> {
        let bytes = packet.encode().map_err(|e| Error::protocol(e.to_string()))?;
        self.stream
            .write_all(&bytes)
            .and_then(|_| self.stream.flush())
            .map_err(|e| map_tls_io_error(e, &self.tls.verifier, "MQTT write"))
    }

    pub(crate) fn ack_if_needed(&mut self, p: &Publish) -> Result<()> {
        if let (QoS::AtLeastOnce, Some(id)) = (p.qos, p.packet_id) {
            self.send(&Packet::PubAck { packet_id: id })?;
        }
        Ok(())
    }

    fn try_decode(&mut self) -> Result<Option<Packet>> {
        match Packet::decode(self.stream.buffered(), self.max_packet) {
            Ok(Some((packet, used))) => {
                self.stream.consume(used);
                Ok(Some(packet))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(Error::protocol(format!("bad MQTT packet from printer: {e}"))),
        }
    }

    fn set_read_timeout(&mut self, t: Duration) -> Result<()> {
        let t = t.max(Duration::from_millis(1));
        if self.read_timeout != Some(t) {
            self.stream.set_read_timeout(Some(t))?;
            self.read_timeout = Some(t);
        }
        Ok(())
    }

    /// Wait up to `timeout` for one packet; `Ok(None)` on timeout.
    pub(crate) fn poll(&mut self, timeout: Duration) -> Result<Option<Packet>> {
        if let Some(p) = self.try_decode()? {
            return Ok(Some(p));
        }
        self.set_read_timeout(timeout)?;
        match self.stream.read_more() {
            Ok(0) => Err(Error::ConnectionLost {
                detail: "printer closed the MQTT connection".into(),
            }),
            Ok(_) => self.try_decode(),
            Err(e) if is_timeout(&e) => Ok(None),
            Err(e) => Err(map_tls_io_error(e, &self.tls.verifier, "MQTT read")),
        }
    }

    fn read_until(&mut self, deadline: Instant, what: &str) -> Result<Packet> {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::timeout(format!("waiting for MQTT {what}")));
            }
            if let Some(p) = self.poll(remaining)? {
                return Ok(p);
            }
        }
    }

    /// Best-effort DISCONNECT + close_notify + TCP shutdown.
    pub(crate) fn close(mut self) {
        let _ = self.send(&Packet::Disconnect);
        self.stream.close();
        self.stream.shutdown_tcp(std::net::Shutdown::Both);
    }
}
