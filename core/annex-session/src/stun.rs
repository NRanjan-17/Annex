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
    let (family, ip): (u8, Vec<u8>) = match addr.ip().to_canonical() {
        IpAddr::V4(ip) => (0x01, ip.octets().to_vec()),
        IpAddr::V6(ip) => (0x02, ip.octets().to_vec()),
    };
    let mut key = MAGIC.to_be_bytes().to_vec();
    key.extend_from_slice(id);
    let mut value = vec![0, family];
    value.extend(addr.port().to_be_bytes().iter().zip(&key).map(|(b, k)| b ^ k));
    value.extend(ip.iter().zip(&key).map(|(b, k)| b ^ k));
    let mut m = header(BINDING_SUCCESS, 4 + value.len() as u16, id);
    m.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
    m.extend_from_slice(&(value.len() as u16).to_be_bytes());
    m.extend_from_slice(&value);
    m
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
