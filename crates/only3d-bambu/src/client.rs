//! [`PrinterClient`]: a live MQTT connection with a background I/O thread,
//! a merged state cache, typed status, events and commands.

use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::commands::Command;
use crate::config::PrinterConfig;
use crate::error::{Error, Result};
use crate::models::ModelInfo;
use crate::mqtt::codec::{Packet, Publish, QoS};
use crate::mqtt::session::{MqttSession, SessionParams};
use crate::status::{PrinterStatus, StateCache};
use crate::tls::{client_config, TlsSetup};

/// Tunables for [`PrinterClient::connect_with`].
#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// MQTT TLS port (8883 on every Bambu printer).
    pub port: u16,
    /// TCP connect + TLS handshake + CONNACK/SUBACK budget.
    pub connect_timeout: Duration,
    /// MQTT keep-alive; a PINGREQ is sent every half interval.
    pub keep_alive: Duration,
    /// Reconnect with exponential backoff (1 s .. 30 s) after a drop.
    /// Commands are never replayed across reconnects.
    pub auto_reconnect: bool,
    /// Send `pushall` + `get_version` after every (re)connect.
    pub request_full_state: bool,
    /// Wait this long in `connect` for the first status report
    /// (`None` = return as soon as subscribed).
    pub initial_report_timeout: Option<Duration>,
    /// How long `send` waits for the PUBACK / socket flush.
    pub command_timeout: Duration,
    /// Largest accepted MQTT packet.
    pub max_packet_size: usize,
}

impl Default for ClientOptions {
    fn default() -> Self {
        ClientOptions {
            port: 8883,
            connect_timeout: Duration::from_secs(10),
            keep_alive: Duration::from_secs(30),
            auto_reconnect: true,
            request_full_state: true,
            initial_report_timeout: Some(Duration::from_secs(10)),
            command_timeout: Duration::from_secs(10),
            max_packet_size: 8 * 1024 * 1024,
        }
    }
}

/// Something that happened on the connection.
#[derive(Debug, Clone)]
pub enum Event {
    /// (Re)connected and subscribed.
    Connected,
    /// Every JSON message received on the report topic (replies included).
    Message(Arc<Value>),
    /// The typed status changed (after merging a report).
    StateChanged(Box<PrinterStatus>),
    /// The connection dropped. `code` is an [`Error::code`].
    Disconnected {
        code: &'static str,
        message: String,
        will_reconnect: bool,
    },
    /// The client was shut down or gave up; no further events.
    Closed,
}

enum Outbound {
    Publish {
        payload: Vec<u8>,
        qos: QoS,
        reply: SyncSender<Result<()>>,
    },
    Shutdown,
}

struct Shared {
    cache: StateCache,
    status: PrinterStatus,
    connected: bool,
    closed: bool,
    last_error: Option<String>,
    messages: u64,
}

struct Inner {
    cfg: PrinterConfig,
    state: Mutex<Shared>,
    cond: Condvar,
    subscribers: Mutex<Vec<Sender<Event>>>,
    seq: AtomicU64,
    tls: TlsSetup,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn emit(&self, ev: Event) {
        let mut subs = self.subscribers.lock().unwrap_or_else(|p| p.into_inner());
        subs.retain(|s| s.send(ev.clone()).is_ok());
    }

    fn set_connected(&self, connected: bool, error: Option<String>) {
        let mut st = self.lock();
        st.connected = connected;
        if error.is_some() {
            st.last_error = error;
        }
        drop(st);
        self.cond.notify_all();
    }

    fn set_closed(&self) {
        let mut st = self.lock();
        st.connected = false;
        st.closed = true;
        drop(st);
        self.cond.notify_all();
        self.emit(Event::Closed);
    }

    fn handle_publish(&self, p: &Publish) {
        if p.topic != self.cfg.report_topic() {
            return;
        }
        // Printers occasionally put invalid UTF-8 (e.g. truncated file names)
        // in reports: decode lossily rather than dropping the whole message.
        let text = String::from_utf8_lossy(&p.payload);
        let Ok(msg) = serde_json::from_str::<Value>(text.trim_end_matches('\0')) else {
            return;
        };
        let msg = Arc::new(msg);
        let mut changed_status = None;
        {
            let mut st = self.lock();
            st.messages += 1;
            if st.cache.apply(&msg) {
                let status = PrinterStatus::from_state(st.cache.raw());
                if status != st.status {
                    st.status = status.clone();
                    changed_status = Some(status);
                }
            }
        }
        self.cond.notify_all();
        self.emit(Event::Message(msg));
        if let Some(s) = changed_status {
            self.emit(Event::StateChanged(Box::new(s)));
        }
    }
}

/// A connected printer. Cheap to share by reference across threads
/// (`Send + Sync`); dropping it disconnects.
pub struct PrinterClient {
    inner: Arc<Inner>,
    tx: Sender<Outbound>,
    thread: Option<JoinHandle<()>>,
    opts: ClientOptions,
}

impl std::fmt::Debug for PrinterClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrinterClient")
            .field("cfg", &self.inner.cfg)
            .field("connected", &self.is_connected())
            .finish()
    }
}

fn random_hex() -> String {
    let mut h = RandomState::new().build_hasher();
    h.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.write_u32(std::process::id());
    format!("{:016x}", h.finish())
}

impl PrinterClient {
    /// Connect with default [`ClientOptions`].
    pub fn connect(cfg: &PrinterConfig) -> Result<PrinterClient> {
        Self::connect_with(cfg, ClientOptions::default())
    }

    /// Connect: TLS (with pinning), MQTT CONNECT as `bblp`, subscribe to
    /// `device/{serial}/report`, request a full snapshot, start the I/O thread
    /// and (by default) wait for the first status report.
    pub fn connect_with(cfg: &PrinterConfig, opts: ClientOptions) -> Result<PrinterClient> {
        cfg.validate()?;
        let tls = client_config(cfg.cert_sha256.as_deref())?;
        let client_id = format!("only3d-{}", random_hex());
        let (session, early) = MqttSession::establish(&SessionParams {
            cfg,
            tls: &tls,
            port: opts.port,
            timeout: opts.connect_timeout,
            keep_alive: opts.keep_alive,
            client_id: client_id.clone(),
            max_packet: opts.max_packet_size,
        })?;

        let inner = Arc::new(Inner {
            cfg: cfg.clone(),
            state: Mutex::new(Shared {
                cache: StateCache::new(),
                status: PrinterStatus::default(),
                connected: true,
                closed: false,
                last_error: None,
                messages: 0,
            }),
            cond: Condvar::new(),
            subscribers: Mutex::new(Vec::new()),
            seq: AtomicU64::new(1),
            tls,
        });
        for p in &early {
            inner.handle_publish(p);
        }
        let (tx, rx) = mpsc::channel();
        let io = IoLoop {
            inner: inner.clone(),
            rx,
            opts: opts.clone(),
            client_id,
        };
        let thread = std::thread::Builder::new()
            .name(format!("bambu-mqtt-{}", cfg.serial))
            .spawn(move || io.run(session))
            .map_err(Error::Io)?;
        let client = PrinterClient {
            inner,
            tx,
            thread: Some(thread),
            opts: opts.clone(),
        };
        if let Some(t) = opts.initial_report_timeout {
            let got = client.wait_until(t, |st| st.cache.has_print_state());
            if !got {
                let e = client.inner.lock().last_error.clone();
                return Err(match e {
                    Some(detail) if client.inner.lock().closed => Error::ConnectionLost { detail },
                    _ => Error::timeout(format!(
                        "first status report from {} (connected and subscribed to device/{}/report, but \
                         nothing arrived; check that the serial number matches this printer)",
                        cfg.host, cfg.serial
                    )),
                });
            }
        }
        Ok(client)
    }

    pub fn config(&self) -> &PrinterConfig {
        &self.inner.cfg
    }

    /// SHA-256 of the printer's TLS certificate as seen during the handshake.
    /// Save it into [`PrinterConfig::cert_sha256`] to pin it.
    pub fn tls_fingerprint(&self) -> Option<String> {
        self.inner.tls.verifier.observed()
    }

    pub fn is_connected(&self) -> bool {
        self.inner.lock().connected
    }

    /// Typed snapshot of the merged state.
    pub fn status(&self) -> PrinterStatus {
        self.inner.lock().status.clone()
    }

    /// The raw merged state: `{"print": {...push_status...}, "info": {...get_version...}}`.
    pub fn raw_state(&self) -> Value {
        self.inner.lock().cache.raw().clone()
    }

    /// Model from the config, else from the `get_version` product name.
    pub fn model_info(&self) -> Option<&'static ModelInfo> {
        self.inner.cfg.model_info().or_else(|| {
            let product = self.inner.lock().status.product_name.clone()?;
            crate::models::lookup(product.trim_start_matches("Bambu Lab ").trim())
                .or_else(|| crate::models::lookup(&product))
        })
    }

    /// New event receiver. Events are unbounded: drain it or drop it.
    pub fn subscribe(&self) -> Receiver<Event> {
        let (tx, rx) = mpsc::channel();
        self.inner
            .subscribers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(tx);
        rx
    }

    fn wait_until(&self, timeout: Duration, mut pred: impl FnMut(&Shared) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        let mut st = self.inner.lock();
        loop {
            if pred(&st) {
                return true;
            }
            if st.closed {
                return false;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            st = self
                .inner
                .cond
                .wait_timeout(st, remaining)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    /// Block until `predicate(status)` holds; returns that status.
    pub fn wait_for(
        &self,
        mut predicate: impl FnMut(&PrinterStatus) -> bool,
        timeout: Duration,
    ) -> Result<PrinterStatus> {
        if self.wait_until(timeout, |st| predicate(&st.status)) {
            return Ok(self.status());
        }
        let st = self.inner.lock();
        if st.closed {
            return Err(Error::ConnectionLost {
                detail: st.last_error.clone().unwrap_or_else(|| "client closed".into()),
            });
        }
        Err(Error::timeout("waiting for printer state condition"))
    }

    pub(crate) fn next_sequence_id(&self) -> String {
        self.inner.seq.fetch_add(1, Ordering::Relaxed).to_string()
    }

    /// Publish a raw JSON payload to `device/{serial}/request` and wait until
    /// the broker acknowledged it (QoS 1) or it was written (QoS 0).
    pub fn publish_json(&self, payload: &Value, qos: QoS) -> Result<()> {
        if self.inner.lock().closed {
            return Err(Error::NotConnected);
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(Outbound::Publish {
                payload: serde_json::to_vec(payload)?,
                qos,
                reply: reply_tx,
            })
            .map_err(|_| Error::NotConnected)?;
        match reply_rx.recv_timeout(self.opts.command_timeout) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => Err(Error::timeout("MQTT PUBACK from printer")),
            Err(RecvTimeoutError::Disconnected) => Err(Error::NotConnected),
        }
    }

    /// Send a command; returns its `sequence_id`.
    ///
    /// `Command::GcodeLine` and the temperature commands execute G-code on the
    /// printer immediately — see [`Command::GcodeLine`].
    pub fn send(&self, cmd: &Command) -> Result<String> {
        let seq = self.next_sequence_id();
        let payload = cmd.to_payload(&seq)?;
        self.publish_json(&payload, cmd.qos())?;
        Ok(seq)
    }

    /// Send and wait for the printer's reply carrying the same `sequence_id`
    /// (not every command is answered; `pushall` is not).
    pub fn request(&self, cmd: &Command, timeout: Duration) -> Result<Value> {
        let seq = self.next_sequence_id();
        let payload = cmd.to_payload(&seq)?;
        self.request_payload(payload, cmd.qos(), timeout)
    }

    pub(crate) fn request_payload(&self, payload: Value, qos: QoS, timeout: Duration) -> Result<Value> {
        let (kind, body) = payload
            .as_object()
            .and_then(|o| o.iter().next())
            .map(|(k, v)| (k.clone(), v.clone()))
            .ok_or_else(|| Error::invalid_input("payload must be {\"<type>\": {...}}"))?;
        let seq = body.get("sequence_id").cloned().unwrap_or(Value::Null);
        let command = body.get("command").cloned().unwrap_or(Value::Null);
        let rx = self.subscribe();
        self.publish_json(&payload, qos)?;
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(remaining) {
                Ok(Event::Message(m)) => {
                    if let Some(reply) = m.get(&kind) {
                        if reply.get("command") == Some(&command)
                            && seq_matches(reply.get("sequence_id"), &seq)
                        {
                            return Ok((*m).clone());
                        }
                    }
                }
                Ok(Event::Closed) => return Err(Error::NotConnected),
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => {
                    return Err(Error::timeout(format!(
                        "reply to {kind}.{} (sequence_id {seq})",
                        command.as_str().unwrap_or("?")
                    )))
                }
                Err(RecvTimeoutError::Disconnected) => return Err(Error::NotConnected),
            }
        }
    }

    /// Stop the I/O thread and close the connection (also done on drop).
    pub fn disconnect(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let _ = self.tx.send(Outbound::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn seq_matches(got: Option<&Value>, want: &Value) -> bool {
    let norm = |v: &Value| match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    got.is_some_and(|g| norm(g) == norm(want))
}

impl Drop for PrinterClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ------------------------------------------------------------------ I/O thread

struct IoLoop {
    inner: Arc<Inner>,
    rx: Receiver<Outbound>,
    opts: ClientOptions,
    client_id: String,
}

enum Flow {
    Shutdown,
}

const POLL: Duration = Duration::from_millis(50);

impl IoLoop {
    fn run(self, first: MqttSession) {
        let mut session = Some(first);
        let mut backoff = Duration::from_secs(1);
        if self.opts.request_full_state {
            if let Some(s) = session.as_mut() {
                let _ = self.request_full_state(s);
            }
        }
        self.inner.emit(Event::Connected);
        loop {
            if let Some(mut s) = session.take() {
                match self.pump(&mut s) {
                    Ok(Flow::Shutdown) => {
                        s.close();
                        break;
                    }
                    Err(e) => {
                        let will_reconnect = self.opts.auto_reconnect;
                        self.inner.set_connected(false, Some(e.to_string()));
                        self.inner.emit(Event::Disconnected {
                            code: e.code(),
                            message: e.to_string(),
                            will_reconnect,
                        });
                        drop(s);
                        if !will_reconnect {
                            break;
                        }
                        backoff = Duration::from_secs(1);
                    }
                }
            } else {
                // Disconnected: refuse commands while waiting out the backoff.
                let deadline = Instant::now() + backoff;
                loop {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    match self.rx.recv_timeout(remaining) {
                        Ok(Outbound::Publish { reply, .. }) => {
                            let _ = reply.send(Err(Error::NotConnected));
                        }
                        Ok(Outbound::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                            self.inner.set_closed();
                            return;
                        }
                        Err(RecvTimeoutError::Timeout) => break,
                    }
                }
                match self.reconnect() {
                    Ok(mut s) => {
                        if self.opts.request_full_state {
                            let _ = self.request_full_state(&mut s);
                        }
                        self.inner.set_connected(true, None);
                        self.inner.emit(Event::Connected);
                        session = Some(s);
                    }
                    Err(e) => {
                        self.inner.lock().last_error = Some(e.to_string());
                        // Credentials / pin problems will not fix themselves.
                        if matches!(e.code(), "auth_failed" | "tls_fingerprint_mismatch" | "wrong_serial") {
                            self.inner.emit(Event::Disconnected {
                                code: e.code(),
                                message: e.to_string(),
                                will_reconnect: false,
                            });
                            break;
                        }
                        backoff = (backoff * 2).min(Duration::from_secs(30));
                    }
                }
            }
        }
        // Fail anything still queued.
        while let Ok(o) = self.rx.try_recv() {
            if let Outbound::Publish { reply, .. } = o {
                let _ = reply.send(Err(Error::NotConnected));
            }
        }
        self.inner.set_closed();
    }

    fn reconnect(&self) -> Result<MqttSession> {
        let (s, early) = MqttSession::establish(&SessionParams {
            cfg: &self.inner.cfg,
            tls: &self.inner.tls,
            port: self.opts.port,
            timeout: self.opts.connect_timeout,
            keep_alive: self.opts.keep_alive,
            client_id: self.client_id.clone(),
            max_packet: self.opts.max_packet_size,
        })?;
        for p in &early {
            self.inner.handle_publish(p);
        }
        Ok(s)
    }

    fn request_full_state(&self, s: &mut MqttSession) -> Result<()> {
        for cmd in [Command::PushAll, Command::GetVersion] {
            let seq = self.inner.seq.fetch_add(1, Ordering::Relaxed).to_string();
            let payload = serde_json::to_vec(&cmd.to_payload(&seq)?)?;
            s.send(&Packet::Publish(Publish {
                topic: self.inner.cfg.request_topic(),
                payload,
                qos: QoS::AtMostOnce,
                packet_id: None,
                retain: false,
                dup: false,
            }))?;
        }
        Ok(())
    }

    fn pump(&self, s: &mut MqttSession) -> Result<Flow> {
        let keep_alive = self.opts.keep_alive.max(Duration::from_secs(2));
        let ping_every = keep_alive / 2;
        let ping_timeout = (keep_alive / 2).max(Duration::from_secs(5));
        let mut last_sent = Instant::now();
        let mut ping_sent: Option<Instant> = None;
        let mut pending: HashMap<u16, (SyncSender<Result<()>>, Instant)> = HashMap::new();
        let topic = self.inner.cfg.request_topic();

        let result = (|| -> Result<Flow> {
            loop {
                // 1. Outbound commands.
                loop {
                    match self.rx.try_recv() {
                        Ok(Outbound::Publish { payload, qos, reply }) => {
                            let packet_id = (qos == QoS::AtLeastOnce).then(|| s.next_packet_id());
                            let sent = s.send(&Packet::Publish(Publish {
                                topic: topic.clone(),
                                payload,
                                qos,
                                packet_id,
                                retain: false,
                                dup: false,
                            }));
                            match (sent, packet_id) {
                                (Ok(()), Some(id)) => {
                                    pending.insert(id, (reply, Instant::now() + self.opts.command_timeout));
                                }
                                (Ok(()), None) => {
                                    let _ = reply.send(Ok(()));
                                }
                                (Err(e), _) => {
                                    let _ = reply.send(Err(Error::ConnectionLost {
                                        detail: e.to_string(),
                                    }));
                                    return Err(e);
                                }
                            }
                            last_sent = Instant::now();
                        }
                        Ok(Outbound::Shutdown) | Err(TryRecvError::Disconnected) => return Ok(Flow::Shutdown),
                        Err(TryRecvError::Empty) => break,
                    }
                }

                // 2. Keep-alive.
                let now = Instant::now();
                if now.duration_since(last_sent) >= ping_every && ping_sent.is_none() {
                    s.send(&Packet::PingReq)?;
                    last_sent = now;
                    ping_sent = Some(now);
                }
                if let Some(t) = ping_sent {
                    if now.duration_since(t) > ping_timeout {
                        return Err(Error::ConnectionLost {
                            detail: format!("no PINGRESP within {ping_timeout:?}"),
                        });
                    }
                }
                pending.retain(|_, (reply, deadline)| {
                    if now > *deadline {
                        let _ = reply.send(Err(Error::timeout("MQTT PUBACK from printer")));
                        false
                    } else {
                        true
                    }
                });

                // 3. Inbound.
                match s.poll(POLL)? {
                    Some(Packet::Publish(p)) => {
                        s.ack_if_needed(&p)?;
                        self.inner.handle_publish(&p);
                    }
                    Some(Packet::PubAck { packet_id }) => {
                        if let Some((reply, _)) = pending.remove(&packet_id) {
                            let _ = reply.send(Ok(()));
                        }
                    }
                    Some(Packet::PingResp) => ping_sent = None,
                    Some(Packet::Disconnect) => {
                        return Err(Error::ConnectionLost {
                            detail: "printer sent DISCONNECT".into(),
                        })
                    }
                    Some(_) | None => {}
                }
            }
        })();

        for (_, (reply, _)) in pending.drain() {
            let _ = reply.send(Err(Error::ConnectionLost {
                detail: "connection closed before PUBACK".into(),
            }));
        }
        result
    }
}
