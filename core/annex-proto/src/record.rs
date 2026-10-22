//! Record framing over a reliable byte stream (TLS over TCP in M1).
//!
//! ```text
//! +--------+----------------+----------------------+
//! | type:1 | length:4 (BE)  | payload: length bytes |
//! +--------+----------------+----------------------+
//! ```

use crate::ProtoError;

pub const RECORD_HEADER_LEN: usize = 5;
/// Upper bound for any record. A 4K keyframe at high bitrate fits comfortably.
pub const MAX_RECORD_LEN: usize = 16 * 1024 * 1024;
/// Upper bound for control records; they are small by design.
pub const MAX_CONTROL_LEN: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RecordType {
    Control = 0x01,
    Video = 0x02,
}

impl TryFrom<u8> for RecordType {
    type Error = ProtoError;
    fn try_from(v: u8) -> Result<Self, ProtoError> {
        match v {
            0x01 => Ok(RecordType::Control),
            0x02 => Ok(RecordType::Video),
            other => Err(ProtoError::UnknownRecordType(other)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordHeader {
    pub kind: RecordType,
    pub len: usize,
}

impl RecordHeader {
    pub fn parse(bytes: &[u8; RECORD_HEADER_LEN]) -> Result<Self, ProtoError> {
        let kind = RecordType::try_from(bytes[0])?;
        let len = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) as usize;
        let max = match kind {
            RecordType::Control => MAX_CONTROL_LEN,
            RecordType::Video => MAX_RECORD_LEN,
        };
        if len > max {
            return Err(ProtoError::RecordTooLarge(len));
        }
        Ok(RecordHeader { kind, len })
    }

    pub fn to_bytes(self) -> [u8; RECORD_HEADER_LEN] {
        let l = (self.len as u32).to_be_bytes();
        [self.kind as u8, l[0], l[1], l[2], l[3]]
    }
}

/// Appends one framed record to `out`.
pub fn encode_record(kind: RecordType, payload: &[u8], out: &mut Vec<u8>) -> Result<(), ProtoError> {
    if payload.len() > MAX_RECORD_LEN {
        return Err(ProtoError::RecordTooLarge(payload.len()));
    }
    out.extend_from_slice(&RecordHeader { kind, len: payload.len() }.to_bytes());
    out.extend_from_slice(payload);
    Ok(())
}

/// A decoded record: its type, payload, and the number of bytes consumed from the input.
pub type DecodedRecord<'a> = (RecordType, &'a [u8], usize);

/// Decodes one record from the front of `buf`.
/// Returns `Ok(None)` if more bytes are needed.
pub fn decode_record(buf: &[u8]) -> Result<Option<DecodedRecord<'_>>, ProtoError> {
    let Some(header) = buf.first_chunk::<RECORD_HEADER_LEN>() else {
        return Ok(None);
    };
    let header = RecordHeader::parse(header)?;
    let total = RECORD_HEADER_LEN + header.len;
    if buf.len() < total {
        return Ok(None);
    }
    Ok(Some((header.kind, &buf[RECORD_HEADER_LEN..total], total)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_partial() {
        let mut out = Vec::new();
        encode_record(RecordType::Control, b"abc", &mut out).unwrap();
        encode_record(RecordType::Video, &[9; 10], &mut out).unwrap();
        let (k, p, n) = decode_record(&out).unwrap().unwrap();
        assert_eq!((k, p), (RecordType::Control, &b"abc"[..]));
        let (k, p, m) = decode_record(&out[n..]).unwrap().unwrap();
        assert_eq!((k, p.len()), (RecordType::Video, 10));
        assert_eq!(n + m, out.len());
        assert_eq!(decode_record(&out[..4]).unwrap(), None);
        assert_eq!(decode_record(&out[..n - 1]).unwrap(), None);
    }

    #[test]
    fn rejects_oversized_control() {
        let h = RecordHeader { kind: RecordType::Control, len: MAX_CONTROL_LEN + 1 }.to_bytes();
        assert_eq!(RecordHeader::parse(&h), Err(ProtoError::RecordTooLarge(MAX_CONTROL_LEN + 1)));
    }

    #[test]
    fn rejects_unknown_type() {
        assert_eq!(decode_record(&[0x7f, 0, 0, 0, 0]), Err(ProtoError::UnknownRecordType(0x7f)));
    }
}
