//! The little of STUN (RFC 8489) that internet connections need: asking a STUN server which public
//! address a router gave our UDP socket, and "punch" packets that open the router towards a peer.
//!
//! Punches are Binding Indications: STUN, so the session socket can tell them from QUIC (RFC 7983:
//! STUN's first byte is 0–3, QUIC's is 64 or more) and drop them before QUIC sees them.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

const MAGIC: u32 = 0x2112_A442;
const HEADER: usize = 20;

const BINDING_REQUEST: u16 = 0x0001;
const BINDING_SUCCESS: u16 = 0x0101;
const BINDING_INDICATION: u16 = 0x0011;

const ATTR_MAPPED_ADDRESS: u16 = 0x0001;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;

pub type TransactionId = [u8; 12];

/// A STUN message, by its fixed header: first two bits zero, the magic cookie, and a length that
/// matches the datagram.
pub fn is_stun(packet: &[u8]) -> bool {
    packet.len() >= HEADER
        && packet[0] & 0xC0 == 0
        && packet[4..8] == MAGIC.to_be_bytes()
        && usize::from(u16::from_be_bytes([packet[2], packet[3]])) + HEADER == packet.len()
}

fn header(kind: u16, len: u16, id: &TransactionId) -> Vec<u8> {
    let mut m = Vec::with_capacity(HEADER + usize::from(len));
    m.extend_from_slice(&kind.to_be_bytes());
    m.extend_from_slice(&len.to_be_bytes());
    m.extend_from_slice(&MAGIC.to_be_bytes());
    m.extend_from_slice(id);
    m
}

pub fn binding_request(id: &TransactionId) -> Vec<u8> {
    header(BINDING_REQUEST, 0, id)
}

pub fn binding_indication(id: &TransactionId) -> Vec<u8> {
    header(BINDING_INDICATION, 0, id)
}

pub fn transaction_id(packet: &[u8]) -> Option<TransactionId> {
    is_stun(packet).then(|| packet[8..20].try_into().expect("12 bytes"))
}

pub fn is_binding_request(packet: &[u8]) -> bool {
    is_stun(packet) && u16::from_be_bytes([packet[0], packet[1]]) == BINDING_REQUEST
}

/// The address in a Binding Success response (XOR-MAPPED-ADDRESS, or the older MAPPED-ADDRESS).
pub fn mapped_address(packet: &[u8]) -> Option<SocketAddr> {
    if !is_stun(packet) || u16::from_be_bytes([packet[0], packet[1]]) != BINDING_SUCCESS {
        return None;
    }
    let mut plain = None;
    let mut at = HEADER;
    while at + 4 <= packet.len() {
        let kind = u16::from_be_bytes([packet[at], packet[at + 1]]);
        let len = usize::from(u16::from_be_bytes([packet[at + 2], packet[at + 3]]));
        let value = packet.get(at + 4..at + 4 + len)?;
        match kind {
            ATTR_XOR_MAPPED_ADDRESS => return decode_address(value, Some(&packet[4..20])),
            ATTR_MAPPED_ADDRESS => plain = decode_address(value, None),
            _ => {}
        }
        at += 4 + len.next_multiple_of(4);
    }
    plain
}

/// `xor`: the magic cookie and transaction id (bytes 4..20 of the message) for XOR-MAPPED-ADDRESS.
fn decode_address(v: &[u8], xor: Option<&[u8]>) -> Option<SocketAddr> {
    let key = |i: usize| xor.map_or(0, |k| k[i]);
    let port = u16::from_be_bytes([v.get(2)? ^ key(0), v.get(3)? ^ key(1)]);
    let ip = match v.get(1)? {
        0x01 => IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(v.get(4..8)?).ok()?.map_index(key))),
        0x02 => IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(v.get(4..20)?).ok()?.map_index(key))),
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

trait MapIndex<const N: usize> {
    fn map_index(self, key: impl Fn(usize) -> u8) -> Self;
}

impl<const N: usize> MapIndex<N> for [u8; N] {
    fn map_index(mut self, key: impl Fn(usize) -> u8) -> Self {
        for (i, b) in self.iter_mut().enumerate() {
            *b ^= key(i);
        }
        self
    }
}

/// A Binding Success response reporting `addr` (what a STUN server sends; tests use it to stand in
/// for one).
pub fn binding_success(id: &TransactionId, addr: SocketAddr) -> Vec<u8> {
    Message::new(BINDING_SUCCESS, *id).attr(ATTR_XOR_MAPPED_ADDRESS, xor_address(id, addr)).encode()
}

/// XOR-MAPPED-ADDRESS / XOR-PEER-ADDRESS / XOR-RELAYED-ADDRESS value.
fn xor_address(id: &TransactionId, addr: SocketAddr) -> Vec<u8> {
    let (family, ip): (u8, Vec<u8>) = match addr.ip().to_canonical() {
        IpAddr::V4(ip) => (0x01, ip.octets().to_vec()),
        IpAddr::V6(ip) => (0x02, ip.octets().to_vec()),
    };
    let mut key = MAGIC.to_be_bytes().to_vec();
    key.extend_from_slice(id);
    let mut value = vec![0, family];
    value.extend(addr.port().to_be_bytes().iter().zip(&key).map(|(b, k)| b ^ k));
    value.extend(ip.iter().zip(&key).map(|(b, k)| b ^ k));
    value
}

// ---- TURN (RFC 8656): the little a relay client needs ------------------------------------------

pub const ALLOCATE_REQUEST: u16 = 0x0003;
pub const ALLOCATE_SUCCESS: u16 = 0x0103;
pub const REFRESH_REQUEST: u16 = 0x0004;
pub const CREATE_PERMISSION_REQUEST: u16 = 0x0008;
pub const SEND_INDICATION: u16 = 0x0016;
pub const DATA_INDICATION: u16 = 0x0017;
/// The error-response class bits: a request's type | 0x0110.
pub const ERROR_CLASS: u16 = 0x0110;

pub const ATTR_USERNAME: u16 = 0x0006;
pub const ATTR_MESSAGE_INTEGRITY: u16 = 0x0008;
pub const ATTR_ERROR_CODE: u16 = 0x0009;
pub const ATTR_LIFETIME: u16 = 0x000D;
pub const ATTR_XOR_PEER_ADDRESS: u16 = 0x0012;
pub const ATTR_DATA: u16 = 0x0013;
pub const ATTR_REALM: u16 = 0x0014;
pub const ATTR_NONCE: u16 = 0x0015;
pub const ATTR_XOR_RELAYED_ADDRESS: u16 = 0x0016;
pub const ATTR_REQUESTED_TRANSPORT: u16 = 0x0019;

/// A STUN/TURN message being built.
pub struct Message {
    kind: u16,
    id: TransactionId,
    attrs: Vec<u8>,
}

impl Message {
    pub fn new(kind: u16, id: TransactionId) -> Self {
        Self { kind, id, attrs: Vec::new() }
    }

    pub fn attr(mut self, kind: u16, value: impl AsRef<[u8]>) -> Self {
        let value = value.as_ref();
        self.attrs.extend_from_slice(&kind.to_be_bytes());
        self.attrs.extend_from_slice(&(value.len() as u16).to_be_bytes());
        self.attrs.extend_from_slice(value);
        self.attrs.resize(self.attrs.len().next_multiple_of(4), 0);
        self
    }

    pub fn peer(self, kind: u16, addr: SocketAddr) -> Self {
        let id = self.id;
        self.attr(kind, xor_address(&id, addr))
    }

    pub fn encode(self) -> Vec<u8> {
        let mut m = header(self.kind, self.attrs.len() as u16, &self.id);
        m.extend_from_slice(&self.attrs);
        m
    }

    /// Ends with MESSAGE-INTEGRITY: HMAC-SHA1 under `key`, over the message up to that attribute
    /// with the length already counting it (RFC 8489 §14.5).
    pub fn encode_signed(self, key: &[u8; 16]) -> Vec<u8> {
        let mut m = header(self.kind, (self.attrs.len() + 24) as u16, &self.id);
        m.extend_from_slice(&self.attrs);
        let tag = ring::hmac::sign(&ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, key), &m);
        m.extend_from_slice(&ATTR_MESSAGE_INTEGRITY.to_be_bytes());
        m.extend_from_slice(&20u16.to_be_bytes());
        m.extend_from_slice(tag.as_ref());
        m
    }
}

/// The long-term credential key: MD5(username ":" realm ":" password). MD5 is what TURN
/// requires (RFC 8489 §9.2.2); relays reject anything else. The key only signs messages to the
/// relay and isn't stored; our relay's passwords are short-lived ones the server hands out.
pub fn long_term_key(username: &str, realm: &str, password: &str) -> [u8; 16] {
    use md5::Digest;
    md5::Md5::digest(format!("{username}:{realm}:{password}")).into()
}

/// Whether `packet` carries a MESSAGE-INTEGRITY that checks out under `key`.
pub fn integrity_ok(packet: &[u8], key: &[u8; 16]) -> bool {
    let Some(at) = attr_offset(packet, ATTR_MESSAGE_INTEGRITY) else { return false };
    let mut signed = packet[..at].to_vec();
    signed[2..4].copy_from_slice(&((at + 24 - HEADER) as u16).to_be_bytes());
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, key);
    ring::hmac::verify(&key, &signed, &packet[at + 4..at + 24]).is_ok()
}

pub fn message_type(packet: &[u8]) -> Option<u16> {
    is_stun(packet).then(|| u16::from_be_bytes([packet[0], packet[1]]))
}

fn attr_offset(packet: &[u8], want: u16) -> Option<usize> {
    if !is_stun(packet) {
        return None;
    }
    let mut at = HEADER;
    while at + 4 <= packet.len() {
        let kind = u16::from_be_bytes([packet[at], packet[at + 1]]);
        let len = usize::from(u16::from_be_bytes([packet[at + 2], packet[at + 3]]));
        packet.get(at + 4..at + 4 + len)?;
        if kind == want {
            return Some(at);
        }
        at += 4 + len.next_multiple_of(4);
    }
    None
}

/// An attribute's value.
pub fn attribute(packet: &[u8], want: u16) -> Option<&[u8]> {
    let at = attr_offset(packet, want)?;
    let len = usize::from(u16::from_be_bytes([packet[at + 2], packet[at + 3]]));
    packet.get(at + 4..at + 4 + len)
}

/// An XOR-…-ADDRESS attribute.
pub fn xor_address_attr(packet: &[u8], want: u16) -> Option<SocketAddr> {
    let a = decode_address(attribute(packet, want)?, Some(&packet[4..20]))?;
    Some(SocketAddr::new(a.ip().to_canonical(), a.port()))
}

/// An error response's code (e.g. 401, 438).
pub fn error_code(packet: &[u8]) -> Option<u16> {
    let v = attribute(packet, ATTR_ERROR_CODE)?;
    Some(u16::from(*v.get(2)? & 0x07) * 100 + u16::from(*v.get(3)?))
}

pub fn lifetime(packet: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(attribute(packet, ATTR_LIFETIME)?.try_into().ok()?))
}

/// A Data indication from the relay: who sent it, and what.
pub fn data_indication(packet: &[u8]) -> Option<(SocketAddr, &[u8])> {
    (message_type(packet)? == DATA_INDICATION)
        .then(|| Some((xor_address_attr(packet, ATTR_XOR_PEER_ADDRESS)?, attribute(packet, ATTR_DATA)?)))?
}

/// A Send indication: `data` for the relay to pass to `peer`.
pub fn send_indication(id: TransactionId, peer: SocketAddr, data: &[u8]) -> Vec<u8> {
    Message::new(SEND_INDICATION, id).peer(ATTR_XOR_PEER_ADDRESS, peer).attr(ATTR_DATA, data).encode()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: TransactionId = [0xb7, 0xe7, 0xa7, 0x01, 0xbc, 0x34, 0xd6, 0x86, 0xfa, 0x87, 0xdf, 0xae];

    #[test]
    fn rfc5769_ipv4_response() {
        // RFC 5769 §2.2 (sample IPv4 response), with its SOFTWARE, MESSAGE-INTEGRITY and
        // FINGERPRINT attributes kept: they must be skipped.
        let hex = "0101003c2112a442b7e7a701bc34d686fa87dfae8022000b7465737420766563746f7220\
                   002000080001a147e112a643000800142b91f599fd9e90c38c7489f92af9ba53f06be7d7\
                   80280004c07d4c96";
        let msg: Vec<u8> = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
        assert!(is_stun(&msg));
        assert_eq!(transaction_id(&msg), Some(ID));
        assert_eq!(mapped_address(&msg), Some("192.0.2.1:32853".parse().unwrap()));
    }

    #[test]
    fn round_trips_both_families() {
        for addr in ["203.0.113.9:47800", "[2001:db8::42]:51000"] {
            let addr: SocketAddr = addr.parse().unwrap();
            assert_eq!(mapped_address(&binding_success(&ID, addr)), Some(addr));
        }
        // A v4 address seen on a dual-stack socket is reported as plain IPv4.
        let mapped: SocketAddr = "[::ffff:198.51.100.7]:9".parse().unwrap();
        assert_eq!(mapped_address(&binding_success(&ID, mapped)), Some("198.51.100.7:9".parse().unwrap()));
    }

    fn unhex(hex: &str) -> Vec<u8> {
        let hex: String = hex.split_whitespace().collect();
        (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn rfc5769_long_term_integrity() {
        // RFC 5769 §2.4: a request with long-term authentication. Username "マトリックス",
        // password "TheMatrIX" (after SASLprep), realm "example.org".
        let msg = unhex(
            "00010060 2112a442 78ad3433 c6ad72c0 29da412e
             00060012 e3839ee3 8388e383 aae38383 e382afe3 82b90000
             0015001c 662f2f34 39396b39 35346436 4f4c3334 6f4c3946 53547679 36347341
             0014000b 6578616d 706c652e 6f726700
             00080014 f6702465 6dd64a3e 02b8e071 2e85c9a2 8ca89666",
        );
        let key = long_term_key("マトリックス", "example.org", "TheMatrIX");
        assert!(integrity_ok(&msg, &key));
        assert!(!integrity_ok(&msg, &long_term_key("マトリックス", "example.org", "wrong")));
        assert_eq!(attribute(&msg, ATTR_REALM), Some(&b"example.org"[..]));
        // Rebuilding the same request gives the same bytes, signature included.
        let id: TransactionId = msg[8..20].try_into().unwrap();
        let rebuilt = Message::new(0x0001, id)
            .attr(ATTR_USERNAME, "マトリックス")
            .attr(ATTR_NONCE, "f//499k954d6OL34oL9FSTvy64sA")
            .attr(ATTR_REALM, "example.org")
            .encode_signed(&key);
        assert_eq!(rebuilt, msg);
    }

    #[test]
    fn turn_messages_round_trip() {
        let peer: SocketAddr = "198.51.100.20:51234".parse().unwrap();
        let send = send_indication(ID, peer, b"quic bytes");
        assert_eq!(message_type(&send), Some(SEND_INDICATION));
        assert_eq!(xor_address_attr(&send, ATTR_XOR_PEER_ADDRESS), Some(peer));
        assert_eq!(attribute(&send, ATTR_DATA), Some(&b"quic bytes"[..]));
        // The relay's Data indication carries the same attributes.
        let mut data = send.clone();
        data[1] = 0x17;
        assert_eq!(data_indication(&data), Some((peer, &b"quic bytes"[..])));
        assert_eq!(data_indication(&send), None);
        let err = Message::new(ALLOCATE_REQUEST | ERROR_CLASS, ID).attr(ATTR_ERROR_CODE, [0, 0, 4, 1]).encode();
        assert_eq!(error_code(&err), Some(401));
        let ok = Message::new(ALLOCATE_SUCCESS, ID)
            .peer(ATTR_XOR_RELAYED_ADDRESS, "[2001:db8::7]:3000".parse().unwrap())
            .attr(ATTR_LIFETIME, 600u32.to_be_bytes())
            .encode();
        assert_eq!(xor_address_attr(&ok, ATTR_XOR_RELAYED_ADDRESS), Some("[2001:db8::7]:3000".parse().unwrap()));
        assert_eq!(lifetime(&ok), Some(600));
    }

    #[test]
    fn tells_stun_from_quic_and_junk() {
        assert!(is_binding_request(&binding_request(&ID)));
        assert!(is_stun(&binding_indication(&ID)) && !is_binding_request(&binding_indication(&ID)));
        assert_eq!(mapped_address(&binding_request(&ID)), None);
        // A QUIC long-header Initial (first byte 0xc0..) and a short header (0x40..) are not STUN.
        let mut quic = binding_request(&ID);
        quic[0] = 0xc3;
        assert!(!is_stun(&quic));
        quic[0] = 0x41;
        assert!(!is_stun(&quic));
        // A truncated message, or one whose length field disagrees.
        assert!(!is_stun(&binding_request(&ID)[..19]));
        let mut long = binding_request(&ID);
        long.extend_from_slice(&[0; 4]);
        assert!(!is_stun(&long));
        assert_eq!(mapped_address(&binding_success(&ID, "1.2.3.4:5".parse().unwrap())[..24]), None);
    }
}
