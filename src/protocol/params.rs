//! Preset summary/details: name, the 109-parameter array, metadata, amp knobs.

use super::*;

pub const PRESET_COUNT: usize = 150;

pub(crate) const NAME_MARKER: [u8; 6] = [0xB9, 0x04, 0xB9, 0x02, 0xBC, 0x21];

pub(crate) const NAME_LEN: usize = 32;

pub(crate) const PARAM_MARKER: [u8; 4] = [0xBA, 0x03, 0xBA, 0x6D];

pub(crate) const PARAM_STRIDE: usize = 5;

// Parameter indices in the preset-details float array (see header).
pub(crate) const P_EQ_BASS: usize = 11;

pub(crate) const P_EQ_BASS_FREQ: usize = 12;

pub(crate) const P_EQ_MID: usize = 13;

pub(crate) const P_EQ_TREBLE: usize = 16;

pub(crate) const P_EQ_TREBLE_FREQ: usize = 17;

pub(crate) const P_AMP_ENABLE: usize = 18;

/// Amp on/off (0.0 / 1.0); no MIDI CC exists for it, written over serial.
pub const AMP_ENABLE_PARAM: usize = P_AMP_ENABLE;

pub(crate) const P_MODEL_GAIN: usize = 20;

pub(crate) const P_MODEL_VOLUME: usize = 21;

pub(crate) const P_MODEL_PRESENCE: usize = 34;

pub(crate) const P_MODEL_DEPTH: usize = 35;

/// Number of items in the preset parameter array (`BA 6D` = 109;
/// Builty `TONEX_PARAM_LAST`).
pub const PARAM_COUNT: usize = 109;

// Delay / reverb parameter indices (Builty `enum TonexParameters`; ranges
// from `tonex_params.c`, units as the pedal stores them).
/// Reverb: 0 pre / 1 post amp (TouchOSC calls the CC84 switch "post").
pub const P_REVERB_POSITION: usize = 36;

/// Reverb on/off (0/1).
pub const P_REVERB_ENABLE: usize = 37;

/// Reverb model 0..=5: Spring 1-4, Room, Plate (`TonexReverbModels`).
pub const P_REVERB_MODEL: usize = 38;

/// First per-model reverb block (Spring 1); each model has 4 params in the
/// order time (0..10), predelay (0..500 ms), color (-10..10), mix (0..100 %).
pub const P_REVERB_FIRST_BLOCK: usize = 39;

pub const REVERB_BLOCK_LEN: usize = 4;

pub const REVERB_MODELS: usize = 6;

/// Delay: 0 pre / 1 post amp.
pub const P_DELAY_POST: usize = 94;

/// Delay on/off (0/1).
pub const P_DELAY_ENABLE: usize = 95;

/// Delay model 0 Digital / 1 Tape (`TonexDelayModels`).
pub const P_DELAY_MODEL: usize = 96;

/// First per-model delay block (Digital); each model has 6 params in the
/// order sync (0/1), time signature (0..=17), time (0..1000 ms),
/// feedback (0..100 %), mode (0 normal / 1 ping-pong), mix (0..100 %).
pub const P_DELAY_FIRST_BLOCK: usize = 97;

pub const DELAY_BLOCK_LEN: usize = 6;

pub const DELAY_MODELS: usize = 2;

/// Amp knob values of one preset, in UI units (0.0..=10.0).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AmpParams {
    pub gain: f32,
    pub volume: f32,
    pub presence: f32,
    pub depth: f32,
    pub bass: f32,
    pub mid: f32,
    pub treble: f32,
    pub amp_enabled: bool,
}

/// Events for a footswitch push of preset `idx` (a `0x0204` with the
/// pushed-index marker): the preset change and its character.
pub(crate) fn pushed_preset_events(idx: u8, payload: &[u8]) -> Vec<Event> {
    let params = extract_amp_params(payload)
        .map_err(|e| eprintln!("TONEX: pushed preset {idx} params: {e}"))
        .ok();
    let mut events = vec![Event::PresetChanged { preset_idx: idx, params, raw: extract_param_array(payload) }];
    if let Some(character) = extract_character(payload) {
        events.push(Event::Character { preset_idx: idx, character });
    }
    events
}

/// Extracts the preset name from a preset-details payload, if present.
pub(crate) fn extract_name(payload: &[u8]) -> Option<String> {
    let start = find(payload, &NAME_MARKER)? + NAME_MARKER.len();
    let bytes = payload.get(start..start + NAME_LEN)?;
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    Some(String::from_utf8_lossy(&bytes[..end]).trim().to_string())
}

/// Reads parameter `index` (an `88 <f32 LE>` item) from the array that
/// starts at `params`.
pub(crate) fn param_at(params: &[u8], index: usize) -> Result<f32, String> {
    let off = index * PARAM_STRIDE;
    let item = params
        .get(off..off + PARAM_STRIDE)
        .ok_or_else(|| format!("param {index}: payload too short"))?;
    if item[0] != FLOAT_TAG {
        return Err(format!("param {index}: expected float tag, got {:#04x}", item[0]));
    }
    let v = f32::from_le_bytes([item[1], item[2], item[3], item[4]]);
    if v.is_finite() {
        Ok(v)
    } else {
        Err(format!("param {index}: not a finite float"))
    }
}

pub(crate) fn check_range(name: &str, v: f32, min: f32, max: f32) -> Result<(), String> {
    if (min..=max).contains(&v) {
        Ok(())
    } else {
        Err(format!("unexpected layout: {name} = {v} outside {min}..={max}"))
    }
}

/// A 0..10 knob value, rounded to the UI's 0.1 step.
pub(crate) fn knob_value(params: &[u8], index: usize, name: &str) -> Result<f32, String> {
    let v = param_at(params, index)?;
    knob_from_pedal(v).ok_or_else(|| format!("unexpected layout: {name} = {v} outside 0..=10"))
}

/// Current preset index (0-based) from a state message (type 0x0201).
///
/// Layout seen on the user's big pedal (hardware capture):
///   `B9 03 | 81 01 02 | 04 | 10 | B9 02 <preset> <?>`
/// i.e. type, a 1-byte size, a 1-byte `10`, then a 2-item collection whose
/// first value is the preset index (second, `00` so far, unknown).
/// Builty's layout (`usb_tonex_parse_state`, `unframed[9]`) has the index
/// as the first body value after a header ending `80 10 03`, with no
/// collection. Both are handled: the header is walked value by value, an
/// optional `B9 n`/`BA n` collection header is skipped, and the first value
/// is decoded (literal, or `80 lo hi` etc. for presets >= 128).
pub(crate) fn extract_state_preset(payload: &[u8]) -> Option<u8> {
    if frame_type(payload)? != TYPE_STATE {
        return None;
    }
    let mut pos = 2;
    for _ in 0..3 {
        read_u16_value(payload, &mut pos)?;
    }
    if matches!(payload.get(pos), Some(0xB9 | 0xBA)) {
        pos += 2;
    }
    let idx = read_u16_value(payload, &mut pos)?;
    u8::try_from(idx).ok().filter(|&i| usize::from(i) < PRESET_COUNT)
}

/// Number of string fields in the preset metadata block (`B9 0D`).
pub(crate) const META_FIELDS: usize = 13;

/// Field holding the preset's character ("CLEAN", "DRIVE", ...).
pub(crate) const META_CHARACTER: usize = 2;

/// Parses one metadata string field at `*pos`:
/// `B9 02 BC <bufsize> <buf> <strlen>` (strlen is a normal value). The text
/// is `buf[..strlen]`, cut at the first NUL; bytes after that are garbage.
pub(crate) fn read_meta_field(p: &[u8], pos: &mut usize) -> Option<String> {
    if p.get(*pos..*pos + 3)? != [0xB9, 0x02, 0xBC] {
        return None;
    }
    let size = usize::from(*p.get(*pos + 3)?);
    let buf = p.get(*pos + 4..*pos + 4 + size)?;
    let mut end = *pos + 4 + size;
    let len = usize::from(read_u16_value(p, &mut end)?);
    *pos = end;
    let text = &buf[..len.min(buf.len())];
    let text = &text[..text.iter().position(|&b| b == 0).unwrap_or(text.len())];
    Some(String::from_utf8_lossy(text).trim().to_string())
}

/// The preset's character from the metadata block in a preset-details
/// payload: `B9 0D` followed by 13 string fields ([0] author, [1] date,
/// [2] character, [3] instrument, ..., [12] description), as captured on
/// the user's pedal. Every `B9 0D` candidate is walked field by field (field
/// sizes vary); the first one where all 13 fields parse wins.
pub(crate) fn extract_character(payload: &[u8]) -> Option<String> {
    let mut from = 0;
    while let Some(off) = find(&payload[from..], &[0xB9, 0x0D, 0xB9, 0x02, 0xBC]) {
        let start = from + off;
        let mut pos = start + 2;
        let fields: Option<Vec<String>> = (0..META_FIELDS).map(|_| read_meta_field(payload, &mut pos)).collect();
        if let Some(mut fields) = fields {
            return Some(fields.swap_remove(META_CHARACTER));
        }
        from = start + 1;
    }
    None
}

/// Name and character of one preset ("" when unknown).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PresetMeta {
    pub name: String,
    pub character: String,
}

/// Emits `CurrentPreset` for a state payload, or logs why it couldn't.
pub(crate) fn report_state(state: Option<&[u8]>, on_event: &impl Fn(Event), debug: bool) {
    match state {
        Some(p) => match extract_state_preset(p) {
            Some(idx) => on_event(Event::CurrentPreset(idx)),
            None => eprintln!("TONEX: state message without a readable preset index: {}", hex(&p[..p.len().min(48)])),
        },
        None => eprintln!("TONEX: no state reply in handshake; current preset unknown"),
    }
    if debug {
        if let Some(p) = state {
            eprintln!("TONEX serial: state ({} B): {}", p.len(), hex(&p[..p.len().min(48)]));
        }
    }
}

/// The whole preset parameter array (`88 <f32>` items after the marker),
/// up to `PARAM_COUNT` items; stops early at the first non-float item, so a
/// shorter result means the tail is unknown. Empty if there's no marker.
pub fn extract_param_array(payload: &[u8]) -> Vec<f32> {
    let Some(start) = find(payload, &PARAM_MARKER) else { return Vec::new() };
    payload[start + PARAM_MARKER.len()..]
        .chunks_exact(PARAM_STRIDE)
        .take(PARAM_COUNT)
        .map_while(|c| (c[0] == FLOAT_TAG).then(|| f32::from_le_bytes([c[1], c[2], c[3], c[4]])))
        .collect()
}

/// Extracts amp knob values from a preset-details payload (see the
/// "Preset parameters" section in the module docs for the rule).
pub(crate) fn extract_amp_params(payload: &[u8]) -> Result<AmpParams, String> {
    let start = find(payload, &PARAM_MARKER).ok_or("parameter marker not found")? + PARAM_MARKER.len();
    let params = &payload[start..];

    // Layout fingerprint: guards against a shifted index table.
    check_range("EQ bass freq", param_at(params, P_EQ_BASS_FREQ)?, 75.0, 600.0)?;
    check_range("EQ treble freq", param_at(params, P_EQ_TREBLE_FREQ)?, 1000.0, 4000.0)?;
    let amp_enable = param_at(params, P_AMP_ENABLE)?;
    if amp_enable != 0.0 && amp_enable != 1.0 {
        return Err(format!("unexpected layout: amp enable = {amp_enable}"));
    }

    Ok(AmpParams {
        gain: knob_value(params, P_MODEL_GAIN, "gain")?,
        volume: knob_value(params, P_MODEL_VOLUME, "volume")?,
        presence: knob_value(params, P_MODEL_PRESENCE, "presence")?,
        depth: knob_value(params, P_MODEL_DEPTH, "depth")?,
        bass: knob_value(params, P_EQ_BASS, "bass")?,
        mid: knob_value(params, P_EQ_MID, "mid")?,
        treble: knob_value(params, P_EQ_TREBLE, "treble")?,
        amp_enabled: amp_enable != 0.0,
    })
}

/// The 7 amp knobs, in the UI's id order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AmpKnob {
    Gain,
    Volume,
    Presence,
    Depth,
    Bass,
    Mid,
    Treble,
}

impl AmpKnob {
    pub const ALL: [AmpKnob; 7] = [
        AmpKnob::Gain,
        AmpKnob::Volume,
        AmpKnob::Presence,
        AmpKnob::Depth,
        AmpKnob::Bass,
        AmpKnob::Mid,
        AmpKnob::Treble,
    ];

    /// Index in the preset parameter array / `0x0207` messages.
    pub fn param_index(self) -> usize {
        match self {
            AmpKnob::Gain => P_MODEL_GAIN,
            AmpKnob::Volume => P_MODEL_VOLUME,
            AmpKnob::Presence => P_MODEL_PRESENCE,
            AmpKnob::Depth => P_MODEL_DEPTH,
            AmpKnob::Bass => P_EQ_BASS,
            AmpKnob::Mid => P_EQ_MID,
            AmpKnob::Treble => P_EQ_TREBLE,
        }
    }

    pub fn from_param_index(index: usize) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.param_index() == index)
    }
}

/// A 0..10 knob value from the pedal, rounded to the UI's 0.1 step;
/// None when out of range.
pub fn knob_from_pedal(v: f32) -> Option<f32> {
    (-0.01..=10.01)
        .contains(&v)
        .then(|| ((v * 10.0).round() / 10.0).clamp(0.0, 10.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::test_util::*;

    #[test]
    fn extracts_name() {
        let mut p = vec![0xB9, 0x03, 0x81, 0x04, 0x02];
        p.extend_from_slice(&NAME_MARKER);
        p.extend_from_slice(b"BazPlexi\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0");
        p.push(0x14);
        assert_eq!(extract_name(&p).as_deref(), Some("BazPlexi"));
    }

    #[test]
    fn extracts_amp_params() {
        let p = payload_with_params(&[
            (P_MODEL_GAIN, 6.9),
            (P_MODEL_VOLUME, 5.2),
            (P_MODEL_PRESENCE, 5.0),
            (P_MODEL_DEPTH, 10.0),
            (P_EQ_BASS, 5.4),
            (P_EQ_MID, 4.8),
            (P_EQ_TREBLE, 6.23),
        ]);
        let a = extract_amp_params(&p).unwrap();
        assert_eq!(
            a,
            AmpParams {
                gain: 6.9,
                volume: 5.2,
                presence: 5.0,
                depth: 10.0,
                bass: 5.4,
                mid: 4.8,
                treble: 6.2,
                amp_enabled: true
            }
        );
    }

    #[test]
    fn extracts_full_param_array() {
        let p = payload_with_params(&[(P_DELAY_ENABLE, 1.0), (PARAM_COUNT - 1, 42.0)]);
        let raw = extract_param_array(&p);
        assert_eq!(raw.len(), PARAM_COUNT);
        assert_eq!(raw[P_DELAY_ENABLE], 1.0);
        assert_eq!(raw[PARAM_COUNT - 1], 42.0);
        // Truncated: only the complete items before the cut.
        let cut = find(&p, &PARAM_MARKER).unwrap() + PARAM_MARKER.len() + 50 * PARAM_STRIDE + 2;
        assert_eq!(extract_param_array(&p[..cut]).len(), 50);
        assert!(extract_param_array(&[0xB9, 0x03]).is_empty());
    }

    #[test]
    fn extracts_state_preset_captured_layout() {
        // Exact frame from the user's pedal (on preset 10 = index 9).
        let p = [0xB9, 0x03, 0x81, 0x01, 0x02, 0x04, 0x10, 0xB9, 0x02, 0x09, 0x00];
        assert_eq!(extract_state_preset(&p), Some(9));
        // Exact frame from the user's pedal on preset 141: `80 <u8>` is a
        // ONE-byte extended value; the 0x01 after it is the next list item.
        let p = [0xB9, 0x03, 0x81, 0x01, 0x02, 0x05, 0x10, 0xB9, 0x02, 0x80, 0x8C, 0x01];
        assert_eq!(extract_state_preset(&p), Some(140));
    }

    #[test]
    fn extracts_character_from_metadata_block() {
        let mut p = vec![0xB9, 0x03, 0x81, 0x04, 0x02];
        p.extend_from_slice(&NAME_MARKER);
        p.extend_from_slice(&[b'N'; 33]);
        // A decoy `B9 0D` that doesn't parse as 13 fields.
        p.extend_from_slice(&[0xB9, 0x0D, 0xB9, 0x02, 0xBC, 0x05, 1, 2]);
        p.extend_from_slice(&[0xB9, 0x0D]);
        p.extend(meta_field(0x21, b"someone", b"JUNK"));
        p.extend(meta_field(0x0B, b"2025-04-01", b""));
        p.extend(meta_field(0x21, b"STOMP - OVERDRIVE", b"leftover text"));
        p.extend(meta_field(0x21, b"GUITAR", b""));
        for _ in 4..12 {
            p.extend(meta_field(0x21, b"", b"xx"));
        }
        p.extend(meta_field(0x41, b"A long description", b"zz"));
        p.extend_from_slice(&PARAM_MARKER);
        assert_eq!(extract_character(&p).as_deref(), Some("STOMP - OVERDRIVE"));
        // strlen shorter than the text before the NUL wins.
        let mut q = vec![0xB9, 0x0D];
        let mut f = meta_field(0x21, b"CLEANX", b"");
        let last = f.len() - 1;
        f[last] = 5;
        q.extend(meta_field(0x21, b"a", b""));
        q.extend(meta_field(0x0B, b"d", b""));
        q.extend(f);
        for _ in 3..13 {
            q.extend(meta_field(0x21, b"", b""));
        }
        assert_eq!(extract_character(&q).as_deref(), Some("CLEAN"));
        assert_eq!(extract_character(&[0xB9, 0x0D, 0xB9, 0x02, 0xBC]), None);
    }

    #[test]
    fn extracts_state_preset() {
        // Index as a bare value right after the header (no list wrapper).
        let p = [0xB9, 0x03, 0x81, 0x01, 0x02, 0x01, 0x10, 63];
        assert_eq!(extract_state_preset(&p), Some(63));
        // 2-byte size field; index >= 128 as one-byte-extended `80 <u8>`.
        let p = [0xB9, 0x03, 0x81, 0x01, 0x02, 0x82, 0x03, 0x00, 0x10, 0x80, 140];
        assert_eq!(extract_state_preset(&p), Some(140));
        // Out of range / wrong type.
        let p = [0xB9, 0x03, 0x81, 0x01, 0x02, 0x03, 0x10, 0x80, 200];
        assert_eq!(extract_state_preset(&p), None);
        let p = [0xB9, 0x03, 0x81, 0x04, 0x02, 0x01, 0x10, 5];
        assert_eq!(extract_state_preset(&p), None);
    }

    #[test]
    fn reads_amp_disabled() {
        let p = payload_with_params(&[(P_AMP_ENABLE, 0.0)]);
        assert!(!extract_amp_params(&p).unwrap().amp_enabled);
    }

    #[test]
    fn rejects_shifted_layout() {
        // Treble freq in the amp-enable slot, as a one-slot shift would give.
        let p = payload_with_params(&[(P_AMP_ENABLE, 1900.0)]);
        assert!(extract_amp_params(&p).is_err());
    }

    #[test]
    fn rejects_out_of_range_knob() {
        let p = payload_with_params(&[(P_MODEL_GAIN, 100.0)]);
        assert!(extract_amp_params(&p).is_err());
    }

    #[test]
    fn rejects_missing_marker_or_truncated() {
        assert!(extract_amp_params(&[0xB9, 0x03]).is_err());
        let p = payload_with_params(&[]);
        let cut = find(&p, &PARAM_MARKER).unwrap() + PARAM_MARKER.len() + 20 * PARAM_STRIDE;
        assert!(extract_amp_params(&p[..cut]).is_err());
    }
}
