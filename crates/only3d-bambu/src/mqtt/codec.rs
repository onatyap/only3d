//! Minimal MQTT 3.1.1 packet codec (OASIS MQTT v3.1.1, section 2-3).
//!
//! Covers exactly what a Bambu LAN client needs, in both directions (the
//! server-side encoders are used by the in-process test broker):
//! CONNECT/CONNACK, PUBLISH (QoS 0/1), PUBACK, SUBSCRIBE/SUBACK,
//! PINGREQ/PINGRESP, DISCONNECT.

use std::fmt;

/// Largest value the 4-byte "remaining length" varint can carry (§2.2.3).
pub const MAX_REMAINING_LENGTH: usize = 268_435_455;

/// Decoding failure. `Incomplete` is not returned; incomplete input yields `Ok(None)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    /// Remaining length used more than 4 bytes.
    MalformedLength,
    /// Packet larger than the configured limit.
    TooLarge(usize),
    /// Packet type 0/15 or flags that 3.1.1 forbids.
    InvalidHeader(u8),
    /// Body shorter than its fields claim, or bad UTF-8.
    Malformed(&'static str),
    /// Value cannot be encoded (string > 65535 bytes, packet > 256 MiB, QoS 2).
    Unencodable(&'static str),
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CodecError::MalformedLength => write!(f, "malformed remaining length"),
            CodecError::TooLarge(n) => write!(f, "packet of {n} bytes exceeds limit"),
            CodecError::InvalidHeader(b) => write!(f, "invalid fixed header byte {b:#04x}"),
            CodecError::Malformed(what) => write!(f, "malformed packet: {what}"),
            CodecError::Unencodable(what) => write!(f, "cannot encode: {what}"),
        }
    }
}

impl std::error::Error for CodecError {}

/// Quality of service. QoS 2 is not supported (Bambu never uses it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum QoS {
    #[default]
    AtMostOnce = 0,
    AtLeastOnce = 1,
}

impl QoS {
    fn from_bits(b: u8) -> Option<QoS> {
        match b {
            0 => Some(QoS::AtMostOnce),
            1 => Some(QoS::AtLeastOnce),
            _ => None,
        }
    }
}

/// CONNACK return codes (§3.2.2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectReturnCode {
    Accepted,
    UnacceptableProtocolVersion,
    IdentifierRejected,
    ServerUnavailable,
    BadUserNameOrPassword,
    NotAuthorized,
    Other(u8),
}

impl ConnectReturnCode {
    pub fn from_u8(b: u8) -> Self {
        match b {
            0 => Self::Accepted,
            1 => Self::UnacceptableProtocolVersion,
            2 => Self::IdentifierRejected,
            3 => Self::ServerUnavailable,
            4 => Self::BadUserNameOrPassword,
            5 => Self::NotAuthorized,
            n => Self::Other(n),
        }
    }

    pub fn as_u8(self) -> u8 {
        match self {
            Self::Accepted => 0,
            Self::UnacceptableProtocolVersion => 1,
            Self::IdentifierRejected => 2,
            Self::ServerUnavailable => 3,
            Self::BadUserNameOrPassword => 4,
            Self::NotAuthorized => 5,
            Self::Other(n) => n,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connect {
    pub client_id: String,
    pub username: Option<String>,
    pub password: Option<Vec<u8>>,
    pub keep_alive_secs: u16,
    pub clean_session: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Publish {
    pub topic: String,
    pub payload: Vec<u8>,
    pub qos: QoS,
    /// Present iff `qos > 0`.
    pub packet_id: Option<u16>,
    pub retain: bool,
    pub dup: bool,
}

/// One MQTT control packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    Connect(Connect),
    ConnAck {
        session_present: bool,
        code: ConnectReturnCode,
    },
    Publish(Publish),
    PubAck {
        packet_id: u16,
    },
    Subscribe {
        packet_id: u16,
        topics: Vec<(String, QoS)>,
    },
    /// Return codes: 0x00/0x01 granted QoS, 0x80 failure.
    SubAck {
        packet_id: u16,
        return_codes: Vec<u8>,
    },
    PingReq,
    PingResp,
    Disconnect,
}

// ---------------------------------------------------------------- varint

/// Encode the remaining length (§2.2.3) into `out`.
pub fn encode_remaining_length(mut len: usize, out: &mut Vec<u8>) -> Result<(), CodecError> {
    if len > MAX_REMAINING_LENGTH {
        return Err(CodecError::Unencodable("remaining length > 268435455"));
    }
    loop {
        let mut byte = (len % 128) as u8;
        len /= 128;
        if len > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if len == 0 {
            return Ok(());
        }
    }
}

/// Decode a remaining length. `Ok(None)` if more bytes are needed;
/// otherwise `(value, bytes_used)`.
pub fn decode_remaining_length(buf: &[u8]) -> Result<Option<(usize, usize)>, CodecError> {
    let mut value = 0usize;
    let mut multiplier = 1usize;
    for (i, &byte) in buf.iter().enumerate() {
        if i >= 4 {
            return Err(CodecError::MalformedLength);
        }
        value += (byte & 0x7f) as usize * multiplier;
        if byte & 0x80 == 0 {
            return Ok(Some((value, i + 1)));
        }
        multiplier *= 128;
    }
    if buf.len() >= 4 {
        return Err(CodecError::MalformedLength);
    }
    Ok(None)
}

// ---------------------------------------------------------------- encode

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) -> Result<(), CodecError> {
    let len = u16::try_from(b.len()).map_err(|_| CodecError::Unencodable("field > 65535 bytes"))?;
    put_u16(out, len);
    out.extend_from_slice(b);
    Ok(())
}

fn frame(first: u8, body: Vec<u8>) -> Result<Vec<u8>, CodecError> {
    let mut out = Vec::with_capacity(body.len() + 5);
    out.push(first);
    encode_remaining_length(body.len(), &mut out)?;
    out.extend_from_slice(&body);
    Ok(out)
}

impl Packet {
    /// Serialise to wire bytes.
    pub fn encode(&self) -> Result<Vec<u8>, CodecError> {
        match self {
            Packet::Connect(c) => {
                let mut body = Vec::with_capacity(64);
                put_bytes(&mut body, b"MQTT")?;
                body.push(4); // protocol level 3.1.1
                let mut flags = 0u8;
                if c.clean_session {
                    flags |= 0x02;
                }
                if c.password.is_some() {
                    if c.username.is_none() {
                        return Err(CodecError::Unencodable("password without username"));
                    }
                    flags |= 0x40;
                }
                if c.username.is_some() {
                    flags |= 0x80;
                }
                body.push(flags);
                put_u16(&mut body, c.keep_alive_secs);
                put_bytes(&mut body, c.client_id.as_bytes())?;
                if let Some(u) = &c.username {
                    put_bytes(&mut body, u.as_bytes())?;
                }
                if let Some(p) = &c.password {
                    put_bytes(&mut body, p)?;
                }
                frame(0x10, body)
            }
            Packet::ConnAck {
                session_present,
                code,
            } => frame(0x20, vec![u8::from(*session_present), code.as_u8()]),
            Packet::Publish(p) => {
                let mut first = 0x30u8;
                if p.dup {
                    first |= 0x08;
                }
                first |= (p.qos as u8) << 1;
                if p.retain {
                    first |= 0x01;
                }
                let mut body = Vec::with_capacity(p.topic.len() + p.payload.len() + 4);
                put_bytes(&mut body, p.topic.as_bytes())?;
                match (p.qos, p.packet_id) {
                    (QoS::AtMostOnce, _) => {}
                    (QoS::AtLeastOnce, Some(id)) if id != 0 => put_u16(&mut body, id),
                    (QoS::AtLeastOnce, _) => {
                        return Err(CodecError::Unencodable("QoS 1 publish needs a non-zero packet id"))
                    }
                }
                body.extend_from_slice(&p.payload);
                frame(first, body)
            }
            Packet::PubAck { packet_id } => frame(0x40, packet_id.to_be_bytes().to_vec()),
            Packet::Subscribe { packet_id, topics } => {
                if topics.is_empty() {
                    return Err(CodecError::Unencodable("SUBSCRIBE without topics"));
                }
                let mut body = Vec::new();
                put_u16(&mut body, *packet_id);
                for (t, q) in topics {
                    put_bytes(&mut body, t.as_bytes())?;
                    body.push(*q as u8);
                }
                frame(0x82, body)
            }
            Packet::SubAck {
                packet_id,
                return_codes,
            } => {
                let mut body = packet_id.to_be_bytes().to_vec();
                body.extend_from_slice(return_codes);
                frame(0x90, body)
            }
            Packet::PingReq => Ok(vec![0xC0, 0]),
            Packet::PingResp => Ok(vec![0xD0, 0]),
            Packet::Disconnect => Ok(vec![0xE0, 0]),
        }
    }

    /// Decode one packet from the front of `buf`.
    ///
    /// Returns `Ok(None)` if `buf` does not yet hold a whole packet, or
    /// `Ok(Some((packet, consumed_bytes)))`.
    pub fn decode(buf: &[u8], max_size: usize) -> Result<Option<(Packet, usize)>, CodecError> {
        let Some(&first) = buf.first() else {
            return Ok(None);
        };
        let Some((len, len_bytes)) = decode_remaining_length(&buf[1..])? else {
            return Ok(None);
        };
        let total = 1 + len_bytes + len;
        if total > max_size {
            return Err(CodecError::TooLarge(total));
        }
        if buf.len() < total {
            return Ok(None);
        }
        let body = &buf[1 + len_bytes..total];
        let packet = decode_body(first, body)?;
        Ok(Some((packet, total)))
    }
}

struct Cursor<'a> {
    b: &'a [u8],
}

impl<'a> Cursor<'a> {
    fn u8(&mut self) -> Result<u8, CodecError> {
        let (&v, rest) = self.b.split_first().ok_or(CodecError::Malformed("truncated"))?;
        self.b = rest;
        Ok(v)
    }
    fn u16(&mut self) -> Result<u16, CodecError> {
        Ok(u16::from_be_bytes([self.u8()?, self.u8()?]))
    }
    fn bytes(&mut self) -> Result<&'a [u8], CodecError> {
        let n = self.u16()? as usize;
        if self.b.len() < n {
            return Err(CodecError::Malformed("length-prefixed field truncated"));
        }
        let (v, rest) = self.b.split_at(n);
        self.b = rest;
        Ok(v)
    }
    fn string(&mut self) -> Result<String, CodecError> {
        String::from_utf8(self.bytes()?.to_vec()).map_err(|_| CodecError::Malformed("invalid UTF-8"))
    }
}

fn decode_body(first: u8, body: &[u8]) -> Result<Packet, CodecError> {
    let kind = first >> 4;
    let flags = first & 0x0f;
    let mut c = Cursor { b: body };
    let expect_flags = |want: u8| {
        if flags == want {
            Ok(())
        } else {
            Err(CodecError::InvalidHeader(first))
        }
    };
    let packet = match kind {
        1 => {
            expect_flags(0)?;
            let name = c.bytes()?;
            let level = c.u8()?;
            if name != b"MQTT" || level != 4 {
                return Err(CodecError::Malformed("not MQTT 3.1.1"));
            }
            let cflags = c.u8()?;
            let keep_alive_secs = c.u16()?;
            let client_id = c.string()?;
            if cflags & 0x04 != 0 {
                // Will topic + message: skip (we never send them).
                c.bytes()?;
                c.bytes()?;
            }
            let username = if cflags & 0x80 != 0 { Some(c.string()?) } else { None };
            let password = if cflags & 0x40 != 0 {
                Some(c.bytes()?.to_vec())
            } else {
                None
            };
            Packet::Connect(Connect {
                client_id,
                username,
                password,
                keep_alive_secs,
                clean_session: cflags & 0x02 != 0,
            })
        }
        2 => {
            expect_flags(0)?;
            let sp = c.u8()?;
            let code = c.u8()?;
            Packet::ConnAck {
                session_present: sp & 1 == 1,
                code: ConnectReturnCode::from_u8(code),
            }
        }
        3 => {
            let qos = QoS::from_bits((flags >> 1) & 0x03).ok_or(CodecError::InvalidHeader(first))?;
            let topic = c.string()?;
            let packet_id = if qos == QoS::AtMostOnce { None } else { Some(c.u16()?) };
            Packet::Publish(Publish {
                topic,
                payload: c.b.to_vec(),
                qos,
                packet_id,
                retain: flags & 0x01 != 0,
                dup: flags & 0x08 != 0,
            })
        }
        4 => {
            expect_flags(0)?;
            Packet::PubAck { packet_id: c.u16()? }
        }
        8 => {
            expect_flags(2)?;
            let packet_id = c.u16()?;
            let mut topics = Vec::new();
            while !c.b.is_empty() {
                let t = c.string()?;
                let q = QoS::from_bits(c.u8()? & 0x03).unwrap_or(QoS::AtLeastOnce);
                topics.push((t, q));
            }
            if topics.is_empty() {
                return Err(CodecError::Malformed("SUBSCRIBE without topics"));
            }
            Packet::Subscribe { packet_id, topics }
        }
        9 => {
            expect_flags(0)?;
            let packet_id = c.u16()?;
            Packet::SubAck {
                packet_id,
                return_codes: c.b.to_vec(),
            }
        }
        12 => {
            expect_flags(0)?;
            Packet::PingReq
        }
        13 => {
            expect_flags(0)?;
            Packet::PingResp
        }
        14 => {
            expect_flags(0)?;
            Packet::Disconnect
        }
        _ => return Err(CodecError::InvalidHeader(first)),
    };
    Ok(packet)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc_len(n: usize) -> Vec<u8> {
        let mut v = Vec::new();
        encode_remaining_length(n, &mut v).unwrap();
        v
    }

    #[test]
    fn varint_boundaries() {
        // Table from MQTT 3.1.1 §2.2.3.
        let cases: &[(usize, &[u8])] = &[
            (0, &[0x00]),
            (127, &[0x7f]),
            (128, &[0x80, 0x01]),
            (16_383, &[0xff, 0x7f]),
            (16_384, &[0x80, 0x80, 0x01]),
            (2_097_151, &[0xff, 0xff, 0x7f]),
            (2_097_152, &[0x80, 0x80, 0x80, 0x01]),
            (268_435_455, &[0xff, 0xff, 0xff, 0x7f]),
        ];
        for &(n, bytes) in cases {
            assert_eq!(enc_len(n), bytes, "encode {n}");
            assert_eq!(decode_remaining_length(bytes).unwrap(), Some((n, bytes.len())), "decode {n}");
        }
        assert!(encode_remaining_length(268_435_456, &mut Vec::new()).is_err());
    }

    #[test]
    fn varint_incomplete_and_malformed() {
        assert_eq!(decode_remaining_length(&[]).unwrap(), None);
        assert_eq!(decode_remaining_length(&[0x80]).unwrap(), None);
        assert_eq!(decode_remaining_length(&[0xff, 0xff, 0xff]).unwrap(), None);
        assert_eq!(
            decode_remaining_length(&[0xff, 0xff, 0xff, 0xff]),
            Err(CodecError::MalformedLength)
        );
        assert_eq!(
            decode_remaining_length(&[0x80, 0x80, 0x80, 0x80, 0x01]),
            Err(CodecError::MalformedLength)
        );
        // Trailing bytes after a complete varint are ignored.
        assert_eq!(decode_remaining_length(&[0x05, 0xff]).unwrap(), Some((5, 1)));
    }

    fn roundtrip(p: Packet) {
        let bytes = p.encode().unwrap();
        let (back, used) = Packet::decode(&bytes, usize::MAX).unwrap().unwrap();
        assert_eq!(used, bytes.len());
        assert_eq!(back, p);
        // Every strict prefix is incomplete, never an error.
        for cut in 0..bytes.len() {
            assert_eq!(Packet::decode(&bytes[..cut], usize::MAX).unwrap(), None, "prefix {cut}");
        }
    }

    #[test]
    fn roundtrip_all_packets() {
        roundtrip(Packet::Connect(Connect {
            client_id: "only3d-1".into(),
            username: Some("bblp".into()),
            password: Some(b"12345678".to_vec()),
            keep_alive_secs: 30,
            clean_session: true,
        }));
        roundtrip(Packet::ConnAck {
            session_present: false,
            code: ConnectReturnCode::BadUserNameOrPassword,
        });
        roundtrip(Packet::Publish(Publish {
            topic: "device/X/request".into(),
            payload: br#"{"pushing":{"command":"pushall"}}"#.to_vec(),
            qos: QoS::AtLeastOnce,
            packet_id: Some(7),
            retain: false,
            dup: true,
        }));
        roundtrip(Packet::Publish(Publish {
            topic: "t".into(),
            payload: vec![0u8; 20_000],
            qos: QoS::AtMostOnce,
            packet_id: None,
            retain: true,
            dup: false,
        }));
        roundtrip(Packet::PubAck { packet_id: 65535 });
        roundtrip(Packet::Subscribe {
            packet_id: 1,
            topics: vec![("device/X/report".into(), QoS::AtMostOnce)],
        });
        roundtrip(Packet::SubAck {
            packet_id: 1,
            return_codes: vec![0x00, 0x80],
        });
        roundtrip(Packet::PingReq);
        roundtrip(Packet::PingResp);
        roundtrip(Packet::Disconnect);
    }

    #[test]
    fn known_wire_bytes() {
        assert_eq!(Packet::PingReq.encode().unwrap(), [0xC0, 0x00]);
        let sub = Packet::Subscribe {
            packet_id: 1,
            topics: vec![("a/b".into(), QoS::AtMostOnce)],
        };
        assert_eq!(sub.encode().unwrap(), [0x82, 0x08, 0x00, 0x01, 0x00, 0x03, b'a', b'/', b'b', 0x00]);
        let connect = Packet::Connect(Connect {
            client_id: "c".into(),
            username: Some("u".into()),
            password: Some(b"p".to_vec()),
            keep_alive_secs: 60,
            clean_session: true,
        });
        let b = connect.encode().unwrap();
        assert_eq!(&b[..12], &[0x10, 19, 0, 4, b'M', b'Q', b'T', b'T', 4, 0xC2, 0, 60]);
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(matches!(Packet::decode(&[0x00, 0x00], 1024), Err(CodecError::InvalidHeader(0))));
        assert!(matches!(Packet::decode(&[0xF0, 0x00], 1024), Err(CodecError::InvalidHeader(_))));
        // QoS 3 publish.
        assert!(matches!(Packet::decode(&[0x36, 0x00], 1024), Err(CodecError::InvalidHeader(_))));
        // SUBSCRIBE with wrong reserved flags.
        assert!(Packet::decode(&[0x80, 0x02, 0, 1], 1024).is_err());
        // Size limit enforced before the body arrives.
        assert_eq!(Packet::decode(&[0x30, 0xff, 0x7f], 100), Err(CodecError::TooLarge(16_386)));
        // QoS1 without id cannot be encoded.
        let p = Packet::Publish(Publish {
            topic: "t".into(),
            payload: vec![],
            qos: QoS::AtLeastOnce,
            packet_id: None,
            retain: false,
            dup: false,
        });
        assert!(p.encode().is_err());
    }

    #[test]
    fn two_packets_in_one_buffer() {
        let mut buf = Packet::PingResp.encode().unwrap();
        buf.extend(Packet::PubAck { packet_id: 3 }.encode().unwrap());
        let (a, n) = Packet::decode(&buf, 1024).unwrap().unwrap();
        assert_eq!(a, Packet::PingResp);
        let (b, m) = Packet::decode(&buf[n..], 1024).unwrap().unwrap();
        assert_eq!(b, Packet::PubAck { packet_id: 3 });
        assert_eq!(n + m, buf.len());
    }
}
