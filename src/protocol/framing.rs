//! Frame layer: CRC-16/X.25, byte stuffing, frame encode/decode.

pub(crate) const FLAG: u8 = 0x7E;

pub(crate) const ESCAPE: u8 = 0x7D;

pub(crate) const ESCAPE_XOR: u8 = 0x20;

/// Received frames larger than this are discarded (summary is ~2 KB).
/// Upper bound for one frame as received (still byte-stuffed): a full
/// preset dump is ~30 KB and may grow when stuffed.
pub(crate) const MAX_FRAME_LEN: usize = 128 * 1024;

/// CRC-16/X.25: reflected poly 0x8408, init 0xFFFF, xorout 0xFFFF.
pub(crate) fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= u16::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0x8408 } else { crc >> 1 };
        }
    }
    !crc
}

pub(crate) fn push_stuffed(out: &mut Vec<u8>, b: u8) {
    if b == FLAG || b == ESCAPE {
        out.push(ESCAPE);
        out.push(b ^ ESCAPE_XOR);
    } else {
        out.push(b);
    }
}

pub(crate) fn build_frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() * 2 + 4);
    out.push(FLAG);
    for &b in payload {
        push_stuffed(&mut out, b);
    }
    let crc = crc16(payload);
    push_stuffed(&mut out, (crc & 0xFF) as u8);
    push_stuffed(&mut out, (crc >> 8) as u8);
    out.push(FLAG);
    out
}

/// Takes the bytes between two flags (flags excluded), unstuffs them and
/// verifies the CRC. Returns the payload without CRC.
pub(crate) fn decode_frame(body: &[u8]) -> Option<Vec<u8>> {
    let mut raw = Vec::with_capacity(body.len());
    let mut iter = body.iter();
    while let Some(&b) = iter.next() {
        if b == ESCAPE {
            raw.push(*iter.next()? ^ ESCAPE_XOR);
        } else {
            raw.push(b);
        }
    }
    if raw.len() < 3 {
        return None;
    }
    let (payload, crc_bytes) = raw.split_at(raw.len() - 2);
    let received = u16::from(crc_bytes[0]) | (u16::from(crc_bytes[1]) << 8);
    (received == crc16(payload)).then(|| payload.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_protocol_md_hello() {
        // TONEX ONE hello from protocol.md: CRC bytes on the wire are 17 8C.
        let hello_one = [0xB9, 0x03, 0x00, 0x82, 0x04, 0x00, 0x80, 0x0B, 0x01, 0xB9, 0x02, 0x02, 0x0B];
        let frame = build_frame(&hello_one);
        assert_eq!(&frame[frame.len() - 3..], &[0x17, 0x8C, 0x7E]);
    }

    #[test]
    fn frame_roundtrip_with_escapes() {
        let payload = [0x01, 0x7E, 0x7D, 0x02];
        let frame = build_frame(&payload);
        assert_eq!(decode_frame(&frame[1..frame.len() - 1]).unwrap(), payload);
    }
}
