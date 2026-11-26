//! Passive LAN discovery.
//!
//! Bambu printers periodically send SSDP-style `NOTIFY * HTTP/1.1` datagrams
//! to UDP 2021 (broadcast) and 1990 (multicast 239.255.255.250), with
//! `NT: urn:bambulab-com:device:3dprinter:1`, `Location: <ip>`,
//! `USN: <serial>` and vendor headers `DevModel.bambu.com` (model id, `O1D` =
//! H2D), `DevName.bambu.com`, `DevConnect.bambu.com` (`lan`/`cloud`),
//! `DevBind.bambu.com` (`free`/`occupied`), `DevSignal.bambu.com`,
//! `DevVersion.bambu.com`, `Devseclink.bambu.com`, `DevCap.bambu.com`. They
//! also answer an `M-SEARCH` for that search target (bambuddy `discovery.py`).
//! Nothing here needs credentials.

use std::collections::BTreeMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};

use crate::error::{Error, Result};

/// Bambu's SSDP search target / notification type.
pub const BAMBU_SEARCH_TARGET: &str = "urn:bambulab-com:device:3dprinter:1";
/// SSDP multicast group.
pub const SSDP_MULTICAST: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
/// Ports printers announce on.
pub const SSDP_PORTS: [u16; 2] = [2021, 1990];

/// One printer seen on the LAN.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredPrinter {
    /// `USN` header.
    pub serial: String,
    /// `Location` header (the printer's IP), falling back to the sender address.
    pub ip: String,
    /// `DevModel.bambu.com`, e.g. `O1D`.
    pub model_code: Option<String>,
    /// Marketing name for `model_code`, e.g. `Bambu Lab H2D`.
    pub model_name: Option<String>,
    /// `DevName.bambu.com` (user-set name).
    pub name: Option<String>,
    /// `DevConnect.bambu.com`: `lan` (LAN-only mode) or `cloud`.
    pub connect_mode: Option<String>,
    /// `DevBind.bambu.com`: `free` or `occupied` (bound to a cloud account).
    pub bind_state: Option<String>,
    /// `DevSignal.bambu.com` (Wi-Fi dBm).
    pub signal: Option<String>,
    /// `DevVersion.bambu.com` firmware.
    pub firmware: Option<String>,
    /// All headers, lower-cased names.
    pub headers: BTreeMap<String, String>,
}

/// Parse one SSDP datagram. Returns `None` for anything that is not a Bambu
/// printer announcement or search response (e.g. other M-SEARCH requests).
pub fn parse_ssdp(msg: &str, from: Option<IpAddr>) -> Option<DiscoveredPrinter> {
    let mut lines = msg.split('\n').map(|l| l.trim_end_matches('\r'));
    let start = lines.next()?.trim();
    let upper = start.to_ascii_uppercase();
    if !(upper.starts_with("NOTIFY ") || upper.starts_with("HTTP/1.1 200") || upper.starts_with("HTTP/1.0 200")) {
        return None;
    }
    let mut headers = BTreeMap::new();
    for line in lines {
        if line.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let target = headers
        .get("nt")
        .or_else(|| headers.get("st"))
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let has_bambu_header = headers.keys().any(|k| k.ends_with(".bambu.com"));
    if !target.contains("bambulab") && !has_bambu_header {
        return None;
    }
    let serial = headers
        .get("usn")?
        .trim()
        .trim_start_matches("uuid:")
        .split("::")
        .next()?
        .trim()
        .to_string();
    if serial.is_empty() || !serial.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let ip = headers
        .get("location")
        .and_then(|l| location_ip(l))
        .or(from)
        .map(|ip| ip.to_string())?;
    let get = |k: &str| headers.get(k).filter(|v| !v.is_empty()).cloned();
    let model_code = get("devmodel.bambu.com");
    Some(DiscoveredPrinter {
        model_name: model_code
            .as_deref()
            .and_then(crate::models::model_name)
            .map(str::to_string),
        model_code,
        name: get("devname.bambu.com"),
        connect_mode: get("devconnect.bambu.com"),
        bind_state: get("devbind.bambu.com"),
        signal: get("devsignal.bambu.com"),
        firmware: get("devversion.bambu.com"),
        serial,
        ip,
        headers,
    })
}

/// `Location` is a bare IP on Bambu printers; accept a URL too.
fn location_ip(loc: &str) -> Option<IpAddr> {
    let s = loc.trim();
    if let Ok(ip) = s.parse() {
        return Some(ip);
    }
    let s = s.split("://").nth(1).unwrap_or(s);
    let host = s.split(['/', '?']).next()?;
    let host = host.rsplit_once(':').map_or(host, |(h, p)| if p.chars().all(|c| c.is_ascii_digit()) { h } else { host });
    host.trim_start_matches('[').trim_end_matches(']').parse().ok()
}

fn msearch(port: u16) -> String {
    format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: {SSDP_MULTICAST}:{port}\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {BAMBU_SEARCH_TARGET}\r\n\r\n"
    )
}

fn listener(port: u16) -> io::Result<UdpSocket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos", target_os = "cygwin"))))]
    let _ = s.set_reuse_port(true);
    s.set_broadcast(true)?;
    s.bind(&SockAddr::from(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port))))?;
    let _ = s.join_multicast_v4(&SSDP_MULTICAST, &Ipv4Addr::UNSPECIFIED);
    s.set_nonblocking(true)?;
    Ok(s.into())
}

fn merge(into: &mut DiscoveredPrinter, new: DiscoveredPrinter) {
    let keep = |old: &mut Option<String>, new: Option<String>| {
        if new.is_some() {
            *old = new;
        }
    };
    into.ip = new.ip;
    keep(&mut into.model_code, new.model_code);
    keep(&mut into.model_name, new.model_name);
    keep(&mut into.name, new.name);
    keep(&mut into.connect_mode, new.connect_mode);
    keep(&mut into.bind_state, new.bind_state);
    keep(&mut into.signal, new.signal);
    keep(&mut into.firmware, new.firmware);
    into.headers.extend(new.headers);
}

/// Outcome of [`discover_detailed`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryReport {
    pub printers: Vec<DiscoveredPrinter>,
    /// Announcement ports we managed to listen on (subset of 2021, 1990).
    pub listening_ports: Vec<u16>,
    /// Human-readable problems, e.g. UDP 2021 held by Bambu Studio.
    pub warnings: Vec<String>,
}

/// Listen for printers for `timeout` (5–10 s recommended; printers announce
/// every few seconds) while sending `M-SEARCH` every 2 s. Results are
/// deduplicated by serial and sorted by serial.
pub fn discover(timeout: Duration) -> Result<Vec<DiscoveredPrinter>> {
    discover_detailed(timeout).map(|r| r.printers)
}

/// [`discover`] plus which ports could be opened and why others could not.
///
/// Most announcements arrive on UDP 2021. Bambu Studio / OrcaSlicer bind that
/// port exclusively while running, in which case only port 1990 and M-SEARCH
/// replies remain and printers may not show up.
pub fn discover_detailed(timeout: Duration) -> Result<DiscoveryReport> {
    let mut report = DiscoveryReport::default();
    let mut sockets: Vec<UdpSocket> = Vec::new();
    for port in SSDP_PORTS {
        match listener(port) {
            Ok(s) => {
                sockets.push(s);
                report.listening_ports.push(port);
            }
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => report.warnings.push(format!(
                "UDP {port} is held exclusively by another program (usually Bambu Studio or OrcaSlicer); \
                 announcements on that port go to it. Quit it to discover, or enter the printer IP manually."
            )),
            Err(e) => report.warnings.push(format!("cannot listen on UDP {port}: {e}")),
        }
    }
    // Ephemeral socket for M-SEARCH; unicast replies come back to it.
    let sender = match listener(0) {
        Ok(s) => Some(s),
        Err(e) => {
            report.warnings.push(format!("cannot open a UDP socket for M-SEARCH: {e}"));
            None
        }
    };
    if sockets.is_empty() && sender.is_none() {
        return Err(Error::Discovery {
            detail: report.warnings.join("; "),
        });
    }

    let send_search = |s: &UdpSocket| {
        for port in SSDP_PORTS {
            let msg = msearch(port);
            let _ = s.send_to(msg.as_bytes(), (SSDP_MULTICAST, port));
            let _ = s.send_to(msg.as_bytes(), (Ipv4Addr::BROADCAST, port));
        }
    };

    let deadline = Instant::now() + timeout;
    let mut next_search = Instant::now();
    let mut found: BTreeMap<String, DiscoveredPrinter> = BTreeMap::new();
    let mut buf = [0u8; 4096];
    while Instant::now() < deadline {
        if Instant::now() >= next_search {
            if let Some(s) = &sender {
                send_search(s);
            }
            next_search = Instant::now() + Duration::from_secs(2);
        }
        let mut got_any = false;
        for s in sockets.iter().chain(sender.iter()) {
            loop {
                match s.recv_from(&mut buf) {
                    Ok((n, from)) => {
                        got_any = true;
                        let text = String::from_utf8_lossy(&buf[..n]);
                        if let Some(p) = parse_ssdp(&text, Some(from.ip())) {
                            match found.get_mut(&p.serial) {
                                Some(existing) => merge(existing, p),
                                None => {
                                    found.insert(p.serial.clone(), p);
                                }
                            }
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(_) => break,
                }
            }
        }
        if !got_any {
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    report.printers = found.into_values().collect();
    Ok(report)
}

/// The SHA-256 fingerprint of the certificate a host presents on the MQTT port (8883), if it
/// answers within `timeout`. A Bambu printer's certificate is its own, so a pinned fingerprint
/// recognises it at any address.
pub fn cert_fingerprint(host: IpAddr, timeout: Duration) -> Option<String> {
    let tls = crate::tls::client_config(None).ok()?;
    let sock = crate::tls::tcp_connect(&host.to_string(), 8883, timeout).ok()?;
    let name = crate::tls::server_name(&host.to_string()).ok()?;
    let _ = crate::tls::TlsStream::handshake(sock, tls.config.clone(), name, timeout * 4, &tls.verifier);
    tls.verifier.observed()
}

/// The /24 networks to look in for a printer that moved: this computer's own (the interface the
/// default route uses) and the one around the address it had.
pub fn nearby_subnets(old: Option<IpAddr>) -> Vec<Ipv4Addr> {
    let own = UdpSocket::bind("0.0.0.0:0").and_then(|u| u.connect("192.0.2.1:9").and(u.local_addr())).ok().map(|a| a.ip());
    let mut nets: Vec<Ipv4Addr> = Vec::new();
    for ip in [own, old].into_iter().flatten() {
        if let IpAddr::V4(v) = ip {
            let o = v.octets();
            let net = Ipv4Addr::new(o[0], o[1], o[2], 0);
            if !v.is_loopback() && !v.is_unspecified() && !nets.contains(&net) {
                nets.push(net);
            }
        }
    }
    nets
}

/// Find a printer whose address changed (F-27): first by its serial in SSDP announcements
/// (`listen`), then by its pinned certificate on port 8883 in `subnets` (each a /24).
pub fn relocate(serial: &str, pinned: Option<&str>, subnets: &[Ipv4Addr], listen: Duration) -> Option<IpAddr> {
    if let Ok(found) = discover(listen) {
        if let Some(ip) = found.iter().find(|p| p.serial.eq_ignore_ascii_case(serial)).and_then(|p| p.ip.parse().ok()) {
            return Some(ip);
        }
    }
    let pin = crate::tls::normalize_fingerprint(pinned?)?;
    let hosts: Vec<IpAddr> = subnets.iter().flat_map(|n| (1..=254u8).map(move |h| IpAddr::V4(Ipv4Addr::new(n.octets()[0], n.octets()[1], n.octets()[2], h)))).collect();
    let found = std::sync::Mutex::new(None);
    std::thread::scope(|sc| {
        for chunk in hosts.chunks(hosts.len().div_ceil(48).max(1)) {
            let (found, pin) = (&found, &pin);
            sc.spawn(move || {
                for ip in chunk {
                    if found.lock().map(|f| f.is_some()).unwrap_or(true) {
                        return;
                    }
                    if cert_fingerprint(*ip, Duration::from_millis(300)).as_deref() == Some(pin.as_str()) {
                        if let Ok(mut f) = found.lock() {
                            *f = Some(*ip);
                        }
                        return;
                    }
                }
            });
        }
    });
    found.into_inner().ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    const H2D_NOTIFY: &str = "NOTIFY * HTTP/1.1\r\n\
HOST: 239.255.255.250:1900\r\n\
Server: UPnP/1.0\r\n\
Location: 192.168.1.42\r\n\
NT: urn:bambulab-com:device:3dprinter:1\r\n\
NTS: ssdp:alive\r\n\
USN: 0948AD4A1100123\r\n\
Cache-Control: max-age=1800\r\n\
DevModel.bambu.com: O1D\r\n\
DevName.bambu.com: Workshop H2D\r\n\
DevConnect.bambu.com: lan\r\n\
DevBind.bambu.com: free\r\n\
Devseclink.bambu.com: secure\r\n\
DevVersion.bambu.com: 01.02.00.00\r\n\
DevSignal.bambu.com: -48\r\n\
DevCap.bambu.com: 1\r\n\r\n";

    #[test]
    fn parses_h2d_notify() {
        let p = parse_ssdp(H2D_NOTIFY, Some("192.168.1.99".parse().unwrap())).unwrap();
        assert_eq!(p.serial, "0948AD4A1100123");
        assert_eq!(p.ip, "192.168.1.42");
        assert_eq!(p.model_code.as_deref(), Some("O1D"));
        assert_eq!(p.model_name.as_deref(), Some("Bambu Lab H2D"));
        assert_eq!(p.name.as_deref(), Some("Workshop H2D"));
        assert_eq!(p.connect_mode.as_deref(), Some("lan"));
        assert_eq!(p.bind_state.as_deref(), Some("free"));
        assert_eq!(p.firmware.as_deref(), Some("01.02.00.00"));
        assert_eq!(p.signal.as_deref(), Some("-48"));
        assert_eq!(p.headers.get("devseclink.bambu.com").map(String::as_str), Some("secure"));
    }

    #[test]
    fn parses_search_response_and_lf_only() {
        let msg = "HTTP/1.1 200 OK\nST: urn:bambulab-com:device:3dprinter:1\nUSN: 01S00C123456789\nDevModel.bambu.com: C12\n\n";
        let p = parse_ssdp(msg, Some("10.0.0.8".parse().unwrap())).unwrap();
        assert_eq!(p.ip, "10.0.0.8", "falls back to sender IP");
        assert_eq!(p.model_name.as_deref(), Some("Bambu Lab P1S"));
    }

    #[test]
    fn ignores_non_bambu_and_requests() {
        assert!(parse_ssdp(&msearch(2021), None).is_none());
        let sonos = "NOTIFY * HTTP/1.1\r\nNT: upnp:rootdevice\r\nUSN: uuid:RINCON_1::upnp:rootdevice\r\nLOCATION: http://10.0.0.3:1400/xml\r\n\r\n";
        assert!(parse_ssdp(sonos, None).is_none());
        assert!(parse_ssdp("", None).is_none());
        assert!(parse_ssdp("NOTIFY * HTTP/1.1\r\nNT: urn:bambulab-com:device:3dprinter:1\r\n\r\n", None).is_none());
        // Serial with injection characters is rejected.
        let evil = "NOTIFY * HTTP/1.1\r\nNT: urn:bambulab-com:device:3dprinter:1\r\nUSN: abc/#\r\nLocation: 1.2.3.4\r\n\r\n";
        assert!(parse_ssdp(evil, None).is_none());
    }

    #[test]
    fn location_forms() {
        assert_eq!(location_ip("192.168.0.5"), Some("192.168.0.5".parse().unwrap()));
        assert_eq!(location_ip("http://192.168.0.5:80/desc.xml"), Some("192.168.0.5".parse().unwrap()));
        assert_eq!(location_ip("garbage"), None);
    }
}
