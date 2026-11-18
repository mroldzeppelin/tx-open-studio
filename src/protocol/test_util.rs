//! Synthetic payload builders shared by the protocol tests (no real dump bytes).

use super::*;

// Synthetic encoder for tree tests (the pedal's value encoding).
pub(crate) fn enc_int(n: u16) -> Vec<u8> {
    encode_int(n)
}

pub(crate) fn enc_list(tag: u8, items: Vec<Vec<u8>>) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend(enc_int(items.len() as u16));
    items.into_iter().for_each(|i| out.extend(i));
    out
}

pub(crate) fn enc_blob(bytes: &[u8]) -> Vec<u8> {
    let mut out = vec![0xBC];
    out.extend(enc_int(bytes.len() as u16));
    out.extend_from_slice(bytes);
    out
}

/// 33-byte name buffer: text, NUL, then garbage.
pub(crate) fn enc_name(text: &str) -> Vec<u8> {
    let mut buf = vec![0x5A; NAME_BUF_LEN];
    buf[..text.len()].copy_from_slice(text.as_bytes());
    buf[text.len()] = 0;
    enc_blob(&buf)
}

pub(crate) fn enc_slot(name: &str, kind: u16, data: &[u8]) -> Vec<u8> {
    enc_list(0xB9, vec![enc_blob(&[7; 16]), enc_list(0xB9, vec![enc_name(name), enc_name("")]), enc_int(kind), enc_blob(data), enc_int(0)])
}

/// `len` bytes: `prefix`, NUL, garbage.
pub(crate) fn slot_data(prefix: &[u8], len: usize) -> Vec<u8> {
    let mut d = vec![0xEE; len];
    d[..prefix.len()].copy_from_slice(prefix);
    d[prefix.len()] = 0;
    d
}

pub(crate) fn enc_body(idx: u16, amp: Vec<u8>, cab: Vec<u8>) -> Vec<u8> {
    enc_body_with(1, 0, idx, amp, cab)
}

/// Body `[mode, dirty, idx, contents]` (mode 1 stored, 0 live buffer).
pub(crate) fn enc_body_with(mode: u16, dirty: u16, idx: u16, amp: Vec<u8>, cab: Vec<u8>) -> Vec<u8> {
    let floats = enc_list(0xBA, (0..5).map(|i| { let mut f = vec![FLOAT_TAG]; f.extend((i as f32).to_le_bytes()); f }).collect());
    let details = enc_list(0xB9, vec![enc_name("PRESET"), enc_list(0xBA, vec![floats.clone(), floats.clone(), floats])]);
    let contents = enc_list(0xB9, vec![details, amp, enc_int(2), cab]);
    enc_list(0xB9, vec![enc_int(mode), enc_int(dirty), enc_int(idx), contents])
}

/// A type-0x0203 reply around `body`.
pub(crate) fn wrap_0203(body: &[u8]) -> Vec<u8> {
    let mut p = vec![0xB9, 0x03, 0x81, 0x03, 0x02, 0x81];
    p.extend((body.len() as u16).to_le_bytes());
    p.push(0x10);
    p.extend_from_slice(body);
    p
}

pub(crate) fn cab_slot() -> Vec<u8> {
    enc_slot("51st Hi", 4, &slot_data(b"\0\0\0\0Peavey 5150", 300))
}

pub(crate) fn amp_slot() -> Vec<u8> {
    enc_slot("Ratfish", 1, &slot_data(b"\x01\x02\x03\x04ProCo Rat", 300))
}

/// Synthetic full-dump reply for preset `idx` (no real dump bytes):
/// header, body `B9 04 01 00 <idx> B9 02 BC 21 <33-byte name> <filler>`.
pub(crate) fn synthetic_dump(idx: u8, name: &str, filler: usize) -> Vec<u8> {
    let mut body = vec![0xB9, 0x04, 0x01, 0x00];
    body.extend(index_bytes(idx));
    body.extend_from_slice(&[0xB9, 0x02, 0xBC, 0x21]);
    let mut buf = [0u8; NAME_BUF_LEN];
    buf[..name.len()].copy_from_slice(name.as_bytes());
    body.extend_from_slice(&buf);
    body.extend((0..filler).map(|i| (i % 251) as u8));
    let mut p = vec![0xB9, 0x03, 0x81, 0x03, 0x02, 0x81];
    p.extend_from_slice(&(body.len() as u16).to_le_bytes());
    p.push(0x10);
    p.extend(body);
    p
}

/// Synthetic summary payload: header + name + param array with the
/// given values (unset slots default to 0.0).
pub(crate) fn payload_with_params(set: &[(usize, f32)]) -> Vec<u8> {
    let mut values = [0.0f32; 109];
    values[P_EQ_BASS_FREQ] = 300.0;
    values[P_EQ_TREBLE_FREQ] = 1900.0;
    values[P_AMP_ENABLE] = 1.0;
    for &(i, v) in set {
        values[i] = v;
    }
    let mut p = vec![0xB9, 0x03, 0x81, 0x04, 0x02];
    p.extend_from_slice(&NAME_MARKER);
    p.extend_from_slice(&[b'X'; 33]);
    p.extend_from_slice(&[0xBA, 0x02, 0x88, 0, 0, 0, 0, 0x88, 0, 0, 0xF0, 0x42]);
    p.extend_from_slice(&PARAM_MARKER);
    for v in values {
        p.push(FLOAT_TAG);
        p.extend_from_slice(&v.to_le_bytes());
    }
    p
}

/// Synthetic global config reply: header, `B9 01 B9 02 BA 16`, 22 floats,
/// plus a trailing item standing in for the rest of the config.
pub(crate) fn globals_payload(trim: f32, vol: f32) -> Vec<u8> {
    let mut body = vec![0xB9, 0x01];
    body.extend_from_slice(&GLOBALS_MARKER);
    for i in 0..GLOBALS_COUNT {
        let v = match i {
            G_INPUT_TRIM => trim,
            G_MAIN_VOLUME => vol,
            _ => 1.0,
        };
        body.push(FLOAT_TAG);
        body.extend_from_slice(&v.to_le_bytes());
    }
    body.extend_from_slice(&[0xBC, 0x02, 0x41, 0x42]);
    let mut p = vec![0xB9, 0x03, 0x81, 0x06, 0x02, 0x82];
    p.extend_from_slice(&(body.len() as u16).to_le_bytes());
    p.extend_from_slice(&[0x80, 0x10, 0x03]);
    p.extend_from_slice(&body);
    p
}

pub(crate) fn meta_field(buf_size: u8, text: &[u8], garbage: &[u8]) -> Vec<u8> {
    let mut f = vec![0xB9, 0x02, 0xBC, buf_size];
    let mut buf = text.to_vec();
    buf.push(0);
    buf.extend_from_slice(garbage);
    buf.resize(usize::from(buf_size), 0xAA);
    f.extend_from_slice(&buf);
    f.push(text.len() as u8);
    f
}

/// `body` with parameter `index` of array 0 set to `value`.
pub(crate) fn patch_param(body: &[u8], index: usize, value: f32) -> Result<Vec<u8>, String> {
    let at = find(body, &PARAM_MARKER).ok_or("parameter array not found")? + PARAM_MARKER.len() + index * 5;
    if index >= PARAM_COUNT || body.get(at) != Some(&FLOAT_TAG) || body.len() < at + 5 {
        return Err("unexpected parameter array layout".into());
    }
    let mut out = body.to_vec();
    out[at + 1..at + 5].copy_from_slice(&value.to_le_bytes());
    Ok(out)
}
