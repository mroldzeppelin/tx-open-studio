//! Message types, request builders and frame classification.

use super::*;
use std::time::Duration;

pub(crate) const OVERALL_DEADLINE: Duration = Duration::from_secs(10);

pub(crate) const PRESET_TIMEOUT: Duration = Duration::from_millis(1000);

pub(crate) const HELLO_CMD: [u8; 13] = [
    0xB9, 0x03, 0x00, 0x82, 0x04, 0x00, 0x80, 0x10, 0x01, 0xB9, 0x02, 0x02, 0x10,
];

pub(crate) const REQUEST_STATE_CMD: [u8; 15] = [
    0xB9, 0x03, 0x00, 0x82, 0x06, 0x00, 0x80, 0x10, 0x03, 0xB9, 0x02, 0x81, 0x01, 0x02, 0x10,
];

pub(crate) const PRESET_DETAILS_HEADER: [u8; 11] = [
    0xB9, 0x03, 0x81, 0x00, 0x02, 0x82, 0x06, 0x00, 0x80, 0x10, 0x03,
];

/// Read poll while idle, waiting for unsolicited frames.
pub(crate) const IDLE_POLL: Duration = Duration::from_millis(100);

// Message types (header value after `B9 03`), big pedal.
/// State (reply to request-state, also pushed): body starts with the
/// current preset index.
pub(crate) const TYPE_STATE: u16 = 0x0201;

pub(crate) const TYPE_PRESET_DETAILS: u16 = 0x0204;

pub(crate) const TYPE_GLOBAL_CONFIG: u16 = 0x0206;

pub(crate) const TYPE_PARAM_CHANGED: u16 = 0x0207;

/// Body of a 0x0207 message: `B9 04 02 00 <param idx> 88 <f32 LE>`.
pub(crate) const PARAM_CHANGED_MARKER: [u8; 4] = [0xB9, 0x04, 0x02, 0x00];

/// Header of a single-parameter write (type 0x0207, body size 0x000A).
pub(crate) const SET_PARAM_HEADER: [u8; 11] = [
    0xB9, 0x03, 0x81, 0x07, 0x02, 0x82, 0x0A, 0x00, 0x80, 0x10, 0x03,
];

/// In preset details pushed after a footswitch preset change:
/// `B9 04 00 00 [80] <preset idx>`.
pub(crate) const PRESET_INDEX_MARKER: [u8; 4] = [0xB9, 0x04, 0x00, 0x00];

pub(crate) fn preset_request(index: u8) -> Vec<u8> {
    let mut req = PRESET_DETAILS_HEADER.to_vec();
    req.extend_from_slice(&[0xB9, 0x04, 0x10, 0x01]);
    if index >= 0x80 {
        req.push(0x80);
    }
    req.push(index);
    req.push(0x00); // summary, not full details
    req
}

/// Full preset dump reply, and the preset write (type 0x0203).
pub(crate) const TYPE_PRESET_DUMP: u16 = 0x0203;

pub(crate) const DUMP_TIMEOUT: Duration = Duration::from_millis(4000);

/// Time the pedal gets to store a written preset before it is re-read.
pub(crate) const WRITE_SETTLE: Duration = Duration::from_millis(300);

/// Full preset dump request, as the official editor sends it: like the
/// details request but with the last byte 01 (full) and the header size
/// field = body length.
pub(crate) fn full_dump_request(idx: u8) -> Vec<u8> {
    let mut body = vec![0xB9, 0x04, 0x10, 0x01];
    body.extend(index_bytes(idx));
    body.push(0x01);
    let mut req = vec![0xB9, 0x03, 0x81, 0x00, 0x02, 0x82];
    req.extend_from_slice(&(body.len() as u16).to_le_bytes());
    req.extend_from_slice(&[0x80, 0x10, 0x03]);
    req.extend(body);
    req
}

/// Preset write: `B9 03 81 03 02 82 <len LE> 80 10 03` + a (patched) dump
/// body.
pub(crate) fn preset_write_request(body: &[u8]) -> Result<Vec<u8>, String> {
    let len = u16::try_from(body.len()).map_err(|_| "preset body too large".to_string())?;
    let mut req = vec![0xB9, 0x03, 0x81, 0x03, 0x02, 0x82];
    req.extend_from_slice(&len.to_le_bytes());
    req.extend_from_slice(&[0x80, 0x10, 0x03]);
    req.extend_from_slice(body);
    Ok(req)
}

/// Read of the pedal's live edit buffer (the active preset with unsaved
/// edits): like a full dump request, but `10 00 00 01`.
pub(crate) fn live_buffer_request() -> Vec<u8> {
    let body = [0xB9, 0x04, 0x10, 0x00, 0x00, 0x01];
    let mut req = vec![0xB9, 0x03, 0x81, 0x00, 0x02, 0x82];
    req.extend_from_slice(&(body.len() as u16).to_le_bytes());
    req.extend_from_slice(&[0x80, 0x10, 0x03]);
    req.extend_from_slice(&body);
    req
}

/// Single-parameter write (Builty `usb_tonex_send_single_parameter`, big
/// pedal; needs the firmware that came with Editor support).
/// `section` 02 = the 109-param array, 03 = front-panel VOLUME (index 0).
pub(crate) fn set_param_request(section: u8, index: u8, value: f32) -> Vec<u8> {
    let mut req = SET_PARAM_HEADER.to_vec();
    req.extend_from_slice(&[0xB9, 0x04, section, 0x00]);
    req.push(index);
    req.push(FLOAT_TAG);
    req.extend_from_slice(&value.to_le_bytes());
    req
}

/// Section of the front-panel VOLUME knob (`B9 04 03 00 00 88 <f32>`), from
/// a hardware capture of turning it (values 9.2 … 6.9). The pedal's master
/// volume: pedal-wide, persistent across presets, not in the preset data.
pub const VOLUME_SECTION: u8 = 0x03;

/// Front-panel VOLUME range (pedal manual: 0 – 10).
pub const VOLUME_RANGE: (f32, f32) = (0.0, 10.0);

/// Section of the 109-entry parameter array (`B9 04 02 00 <idx> ...`).
pub(crate) const PARAM_SECTION: u8 = 0x02;

/// Preset select (type 0x0201 header, from the official editor, verified
/// on hardware): `B9 03 81 01 02 82 <len LE> 80 10 03 | B9 02 <idx> 00`,
/// size field = body length (4, or 5 with `80 xx` for idx >= 128).
pub(crate) fn select_request(idx: u8) -> Vec<u8> {
    let mut body = vec![0xB9, 0x02];
    body.extend(index_bytes(idx));
    body.push(0x00);
    let mut req = vec![0xB9, 0x03, 0x81, 0x01, 0x02, 0x82];
    req.extend_from_slice(&(body.len() as u16).to_le_bytes());
    req.extend_from_slice(&[0x80, 0x10, 0x03]);
    req.extend(body);
    req
}

/// "Get parameter" request (type 0x020C, from the official editor):
/// `B9 03 81 0C 02 82 <len LE> 80 10 03 | B9 03 <section> 00 <idx>`, size
/// field = body length. The pedal answers with an ordinary `0x0207`
/// parameter frame holding the LIVE value. Not for verifying our own
/// writes: right after a write it may still return the previous value.
pub(crate) fn get_param_request(section: u8, index: u8) -> Vec<u8> {
    let mut body = vec![0xB9, 0x03, section, 0x00];
    body.extend(index_bytes(index));
    let mut req = vec![0xB9, 0x03, 0x81, 0x0C, 0x02, 0x82];
    req.extend_from_slice(&(body.len() as u16).to_le_bytes());
    req.extend_from_slice(&[0x80, 0x10, 0x03]);
    req.extend(body);
    req
}

/// How long to wait for the answer to a get-parameter request.
pub(crate) const GET_PARAM_TIMEOUT: Duration = Duration::from_millis(1000);

/// How long to wait, after a preset select, for the pedal to confirm the
/// switch (see `is_select_ack`) before the new preset's reads go out.
pub(crate) const SELECT_SETTLE: Duration = Duration::from_millis(1000);

/// A live change of the front-panel VOLUME: `0x0207` with body
/// `B9 04 03 00 00 88 <f32>` (captured on hardware while turning the knob:
/// `B9 03 81 07 02 0A 02 B9 04 03 00 00 88 <f32>`).
pub(crate) fn extract_volume_change(payload: &[u8]) -> Option<f32> {
    let start = find(payload, &[0xB9, 0x04, VOLUME_SECTION, 0x00, 0x00, FLOAT_TAG])? + 5;
    float_at(payload, start)
}

/// The pedal's reply to hello (`B9 03 02 <size> 10 | <body>`).
pub(crate) const TYPE_HELLO_REPLY: u16 = 0x0002;

/// What the hello reply says about the pedal. Each part is None when it
/// didn't read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Serial number, 9 digits zero-padded (as the official editor shows it).
    pub serial: Option<String>,
    /// Firmware version, e.g. "1.6.24".
    pub firmware: Option<String>,
}

/// Serial number and firmware from the hello reply. Body (FW 1.6.24, from
/// a capture of the official editor): `B9 07` + 7 items: `00`, `80 81`,
/// list `[0C 02 00]` (unknown), list `[01 06 18]` = firmware 1.6.24, a
/// 20-byte blob (unknown), `82` + 4 bytes = the serial number (u32 LE),
/// `00`. Items 0..4 follow the value encoding; the serial's `82` is
/// followed by 4 bytes here (not 2 as in size fields), which is what makes
/// the 7 items fill the header's size exactly.
pub(crate) fn parse_hello(payload: &[u8]) -> DeviceInfo {
    let mut info = DeviceInfo::default();
    if frame_type(payload) != Some(TYPE_HELLO_REPLY) {
        return info;
    }
    let Ok(body) = sized_body(payload, "hello reply") else { return info };
    if body.get(..2) != Some(&[0xB9, 0x07]) {
        return info;
    }
    let mut pos = 2;
    let mut items = Vec::new();
    for _ in 0..5 {
        match parse_node(body, &mut pos, 1) {
            Some(node) => items.push(node),
            None => return info,
        }
    }
    info.firmware = items[3].items().and_then(|parts| {
        let numbers: Option<Vec<String>> = parts.iter().map(|n| n.int().map(|v| v.to_string())).collect();
        numbers.filter(|n| n.len() == 3).map(|n| n.join("."))
    });
    if body.get(pos) == Some(&0x82) {
        if let Some(&[a, b, c, d]) = body.get(pos + 1..pos + 5) {
            info.serial = Some(format!("{:09}", u32::from_le_bytes([a, b, c, d])));
        }
    }
    info
}

/// Message type from the `B9 03 <type> <size> <unknown>` header.
pub(crate) fn frame_type(payload: &[u8]) -> Option<u16> {
    if payload.get(..2)? != [0xB9, 0x03] {
        return None;
    }
    let mut pos = 2;
    read_u16_value(payload, &mut pos)
}

/// `0x0207` param-changed body: `B9 04 02 00 <param idx> 88 <f32 LE>`.
/// Returns (param index, value).
pub(crate) fn extract_param_change(payload: &[u8]) -> Option<(usize, f32)> {
    let start = find(payload, &PARAM_CHANGED_MARKER)? + PARAM_CHANGED_MARKER.len();
    let item = payload.get(start..start + 6)?;
    // Indices >= 0x80 would be u16-encoded; none of ours are, skip them.
    if item[0] >= 0x80 || item[1] != FLOAT_TAG {
        return None;
    }
    let v = f32::from_le_bytes([item[2], item[3], item[4], item[5]]);
    v.is_finite().then_some((usize::from(item[0]), v))
}

/// Preset index from an unsolicited preset-details frame
/// (`B9 04 00 00 [80] <idx>`; absent in responses to our own requests).
pub(crate) fn extract_current_preset(payload: &[u8]) -> Option<u8> {
    let mut pos = find(payload, &PRESET_INDEX_MARKER)? + PRESET_INDEX_MARKER.len();
    if *payload.get(pos)? == 0x80 {
        pos += 1;
    }
    let idx = *payload.get(pos)?;
    (usize::from(idx) < PRESET_COUNT).then_some(idx)
}

/// Preset index of a reply to our summary request for a stored preset:
/// type `0x0204`, size field = body length, body head `B9 04 01 00 <idx>`
/// (hardware-verified for indices below and above 128 and for the active
/// preset). Read with the value decoder, not by searching the payload.
pub(crate) fn summary_reply_index(payload: &[u8]) -> Option<u8> {
    if frame_type(payload) != Some(TYPE_PRESET_DETAILS) {
        return None;
    }
    let body = sized_body(payload, "preset details").ok()?;
    if body.get(..2)? != [0xB9, 0x04] {
        return None;
    }
    let mut pos = 2;
    let mode = read_u16_value(body, &mut pos)?;
    let dirty = read_u16_value(body, &mut pos)?;
    let idx = read_u16_value(body, &mut pos)?;
    if mode != 1 || dirty != 0 || usize::from(idx) >= PRESET_COUNT {
        return None;
    }
    u8::try_from(idx).ok()
}

/// What a `0x0204` preset-details frame is.
#[derive(Debug, PartialEq)]
pub(crate) enum DetailsFrame {
    /// The reply to our request for the `requested` index.
    Reply,
    /// The pedal's push after a preset change (footswitch, our select).
    Push(u8),
    /// A reply for another index (late, or not ours), or unrecognised.
    Other,
}

/// Classifies a frame while a summary request for `requested` (if any)
/// is pending: only a stored-preset reply for exactly that index is the
/// reply; a push stays a push.
pub(crate) fn classify_details(payload: &[u8], requested: Option<u8>) -> DetailsFrame {
    if frame_type(payload) != Some(TYPE_PRESET_DETAILS) {
        return DetailsFrame::Other;
    }
    match summary_reply_index(payload) {
        Some(idx) if Some(idx) == requested => DetailsFrame::Reply,
        Some(_) => DetailsFrame::Other,
        None => extract_current_preset(payload).map_or(DetailsFrame::Other, DetailsFrame::Push),
    }
}

/// The pedal's confirmation of our select of preset `idx`: the `0x0204`
/// summary it pushes once the preset is loaded (with the pushed-index
/// marker). Reads of live values issued before it may still see the
/// previous preset.
pub(crate) fn is_select_ack(payload: &[u8], idx: u8) -> bool {
    classify_details(payload, None) == DetailsFrame::Push(idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_request_bytes() {
        assert_eq!(
            select_request(5),
            [0xB9, 0x03, 0x81, 0x01, 0x02, 0x82, 0x04, 0x00, 0x80, 0x10, 0x03, 0xB9, 0x02, 0x05, 0x00]
        );
        // As the editor sent it for 0x8E: `80 8E`, size 5.
        assert_eq!(
            select_request(0x8E),
            [0xB9, 0x03, 0x81, 0x01, 0x02, 0x82, 0x05, 0x00, 0x80, 0x10, 0x03, 0xB9, 0x02, 0x80, 0x8E, 0x00]
        );
    }

    #[test]
    fn get_param_request_bytes() {
        // Front-panel VOLUME: section 03, index 0; size field = body length.
        assert_eq!(
            get_param_request(VOLUME_SECTION, 0),
            [0xB9, 0x03, 0x81, 0x0C, 0x02, 0x82, 0x05, 0x00, 0x80, 0x10, 0x03, 0xB9, 0x03, 0x03, 0x00, 0x00]
        );
        let long = get_param_request(0x02, 0x90);
        assert_eq!(&long[6..8], &[0x06, 0x00]);
        assert_eq!(&long[11..], &[0xB9, 0x03, 0x02, 0x00, 0x80, 0x90]);
        // The answer is an ordinary 0x0207 level frame.
        let mut answer = vec![0xB9, 0x03, 0x81, 0x07, 0x02, 0x0A, 0x02, 0xB9, 0x04, 0x03, 0x00, 0x00, FLOAT_TAG];
        answer.extend(9.0f32.to_le_bytes());
        assert_eq!(frame_type(&answer), Some(TYPE_PARAM_CHANGED));
        assert_eq!(extract_volume_change(&answer), Some(9.0));
    }

    #[test]
    fn dump_and_write_requests() {
        assert_eq!(
            full_dump_request(5),
            [0xB9, 0x03, 0x81, 0x00, 0x02, 0x82, 0x06, 0x00, 0x80, 0x10, 0x03, 0xB9, 0x04, 0x10, 0x01, 0x05, 0x01]
        );
        // Index >= 128: `80 xx`, size field = body length (7).
        assert_eq!(
            full_dump_request(131),
            [0xB9, 0x03, 0x81, 0x00, 0x02, 0x82, 0x07, 0x00, 0x80, 0x10, 0x03, 0xB9, 0x04, 0x10, 0x01, 0x80, 0x83, 0x01]
        );
        let body = vec![0xAA; 300];
        let w = preset_write_request(&body).unwrap();
        assert_eq!(&w[..11], &[0xB9, 0x03, 0x81, 0x03, 0x02, 0x82, 0x2C, 0x01, 0x80, 0x10, 0x03]);
        assert_eq!(&w[11..], body.as_slice());
        assert_eq!(frame_type(&w), Some(TYPE_PRESET_DUMP));
    }

    #[test]
    fn parses_captured_volume_frames() {
        // Exact frames pushed while turning VOLUME (9.2, then 6.9).
        for v in [9.2f32, 6.9] {
            let mut f = vec![0xB9, 0x03, 0x81, 0x07, 0x02, 0x0A, 0x02, 0xB9, 0x04, 0x03, 0x00, 0x00, 0x88];
            f.extend_from_slice(&v.to_le_bytes());
            assert_eq!(frame_type(&f), Some(TYPE_PARAM_CHANGED));
            assert_eq!(extract_volume_change(&f), Some(v));
            assert_eq!(extract_param_change(&f), None);
        }
    }

    #[test]
    fn volume_write_bytes() {
        let req = set_param_request(VOLUME_SECTION, 0, 7.5);
        let mut want = SET_PARAM_HEADER.to_vec();
        want.extend_from_slice(&[0xB9, 0x04, 0x03, 0x00, 0x00, 0x88]);
        want.extend_from_slice(&7.5f32.to_le_bytes());
        assert_eq!(req, want);
        // Body length matches the header's size field (0x0A).
        assert_eq!(req.len() - SET_PARAM_HEADER.len(), 0x0A);
        // The pedal's own push format parses back.
        assert_eq!(extract_volume_change(&req), Some(7.5));
    }

    #[test]
    fn set_param_matches_builty_bytes() {
        let req = set_param_request(PARAM_SECTION, 18, 1.0);
        assert_eq!(
            req,
            [
                0xB9, 0x03, 0x81, 0x07, 0x02, 0x82, 0x0A, 0x00, 0x80, 0x10, 0x03, // header, size 10
                0xB9, 0x04, 0x02, 0x00, 18, 0x88, 0x00, 0x00, 0x80, 0x3F, // idx 18 = 1.0f
            ]
        );
        // Size field equals the body length.
        assert_eq!(req.len() - SET_PARAM_HEADER.len(), 0x0A);
        assert_eq!(frame_type(&req), Some(TYPE_PARAM_CHANGED));
    }

    #[test]
    fn parses_param_changed_frame() {
        // Same layout as our write; the pedal reports changes this way.
        let frame = set_param_request(PARAM_SECTION, P_MODEL_GAIN as u8, 7.5);
        assert_eq!(extract_param_change(&frame), Some((P_MODEL_GAIN, 7.5)));
        assert_eq!(AmpKnob::from_param_index(P_MODEL_GAIN), Some(AmpKnob::Gain));
        assert_eq!(AmpKnob::from_param_index(P_AMP_ENABLE), None);
    }

    #[test]
    fn frame_type_of_details_response() {
        assert_eq!(frame_type(&[0xB9, 0x03, 0x81, 0x04, 0x02, 0x82, 0x00, 0x08]), Some(TYPE_PRESET_DETAILS));
        assert_eq!(frame_type(&[0xB9, 0x03, 0x02]), Some(0x02));
        assert_eq!(frame_type(&[0xB9, 0x04, 0x02]), None);
    }

    #[test]
    fn select_ack_is_the_push_for_the_selected_preset() {
        let push = |idx: &[u8]| {
            let mut p = vec![0xB9, 0x03, 0x81, 0x04, 0x02];
            p.extend_from_slice(&[0xB9, 0x04, 0x00, 0x00]);
            p.extend_from_slice(idx);
            p
        };
        assert!(is_select_ack(&push(&[42]), 42));
        assert!(!is_select_ack(&push(&[42]), 43), "another preset");
        assert!(is_select_ack(&push(&[0x80, 140]), 140), "index >= 128");
        // A parameter frame or a reply without the pushed-index marker.
        let mut level = vec![0xB9, 0x03, 0x81, 0x07, 0x02, 0x0A, 0x02, 0xB9, 0x04, 0x03, 0x00, 0x00, 0x88];
        level.extend(5.0f32.to_le_bytes());
        assert!(!is_select_ack(&level, 0));
        assert!(!is_select_ack(&[0xB9, 0x03, 0x81, 0x04, 0x02, 0xB9, 0x04, 0x10, 0x01, 42], 42));
    }

    /// Synthetic hello reply: fake serial 1234567, firmware 1.6.24.
    fn hello_reply(serial: u32) -> Vec<u8> {
        let mut body = vec![0xB9, 0x07, 0x00, 0x80, 0x81, 0xB9, 0x03, 0x0C, 0x02, 0x00, 0xB9, 0x03, 0x01, 0x06, 0x18, 0xBC, 0x14];
        body.extend([0xA5; 20]);
        body.push(0x82);
        body.extend(serial.to_le_bytes());
        body.push(0x00);
        let mut p = vec![0xB9, 0x03, 0x02, body.len() as u8, 0x10];
        p.extend(body);
        p
    }

    #[test]
    fn hello_reply_serial_and_firmware() {
        let p = hello_reply(1_234_567);
        assert_eq!(p.len(), 48, "same length as the pedal's reply");
        let info = parse_hello(&p);
        assert_eq!(info.serial.as_deref(), Some("001234567"));
        assert_eq!(info.firmware.as_deref(), Some("1.6.24"));
        // Serial cut off: firmware only (size field adjusted).
        let mut short = p[..p.len() - 3].to_vec();
        short[3] -= 3;
        assert_eq!(parse_hello(&short), DeviceInfo { serial: None, firmware: Some("1.6.24".into()) });
        // Another frame type, a bad size field, or no body: nothing.
        let mut other = p.clone();
        other[2] = 0x05;
        assert_eq!(parse_hello(&other), DeviceInfo::default());
        let mut sized = p.clone();
        sized[3] += 1;
        assert_eq!(parse_hello(&sized), DeviceInfo::default());
        assert_eq!(parse_hello(&HELLO_CMD), DeviceInfo::default());
    }

    /// Synthetic summary frame: header with a matching size field, body
    /// head `B9 04 <mode> 00 <idx>`, then name and filler.
    fn details_frame(mode: u8, idx: u8) -> Vec<u8> {
        let mut body = vec![0xB9, 0x04, mode, 0x00];
        body.extend(index_bytes(idx));
        body.extend_from_slice(&NAME_MARKER);
        body.extend([b'X'; 33]);
        body.extend([0x5A; 40]);
        let mut p = vec![0xB9, 0x03, 0x81, 0x04, 0x02, 0x81];
        p.extend((body.len() as u16).to_le_bytes());
        p.push(0x10);
        p.extend(body);
        p
    }

    #[test]
    fn summary_reply_is_only_the_requested_index() {
        for idx in [5u8, 140] {
            let reply = details_frame(0x01, idx);
            assert_eq!(summary_reply_index(&reply), Some(idx));
            assert_eq!(classify_details(&reply, Some(idx)), DetailsFrame::Reply);
            // A reply for another index (late, or not ours): neither the
            // reply nor a push.
            assert_eq!(classify_details(&reply, Some(idx.wrapping_add(1))), DetailsFrame::Other);
            assert_eq!(classify_details(&reply, None), DetailsFrame::Other);
            // A push landing while `idx` is requested stays a push.
            let push = details_frame(0x00, idx);
            assert_eq!(summary_reply_index(&push), None);
            assert_eq!(classify_details(&push, Some(idx)), DetailsFrame::Push(idx));
            assert_eq!(classify_details(&push, Some(3)), DetailsFrame::Push(idx));
            assert!(is_select_ack(&push, idx));
            assert!(!is_select_ack(&reply, idx), "a reply is no select confirmation");
        }
        // Size field off, index out of range, or another frame type: no reply.
        let mut sized = details_frame(0x01, 5);
        sized[6] ^= 1;
        assert_eq!(summary_reply_index(&sized), None);
        assert_eq!(summary_reply_index(&details_frame(0x01, 200)), None);
        assert_eq!(classify_details(&preset_request(5), Some(5)), DetailsFrame::Other);
    }

    #[test]
    fn extracts_pushed_preset_index() {
        let mut p = vec![0xB9, 0x03, 0x81, 0x04, 0x02];
        p.extend_from_slice(&[0xB9, 0x04, 0x00, 0x00, 42]);
        assert_eq!(extract_current_preset(&p), Some(42));
        let mut p = vec![0xB9, 0x03, 0x81, 0x04, 0x02];
        p.extend_from_slice(&[0xB9, 0x04, 0x00, 0x00, 0x80, 140]);
        assert_eq!(extract_current_preset(&p), Some(140));
        assert_eq!(extract_current_preset(&[0xB9, 0x04, 0x00, 0x00, 0x80, 200]), None);
    }
}
