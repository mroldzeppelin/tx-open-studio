//! Global config `0x0206`: the 22 values, reading and the whole-blob write.

use super::*;
use std::time::Duration;

// Global config (Builty `usb_tonex.c`: `usb_tonex_request_global_config`,
// `usb_tonex_parse_global_config`, `usb_tonex_send_global_config`,
// `GLOBAL_CONFIG_INDEX_*`; ranges from `tonex_params.c`).
pub(crate) const REQUEST_GLOBALS_CMD: [u8; 15] = [
    0xB9, 0x03, 0x00, 0x82, 0x06, 0x00, 0x80, 0x10, 0x03, 0xB9, 0x02, 0x81, 0x06, 0x02, 0x10,
];

pub(crate) const GLOBALS_TIMEOUT: Duration = Duration::from_millis(1500);

/// `BA 16` = array of 22 globals (`88 <f32>` each), inside `B9 02`.
pub(crate) const GLOBALS_MARKER: [u8; 4] = [0xB9, 0x02, 0xBA, 0x16];

pub const GLOBALS_COUNT: usize = 22;

// Global indices used by the app (full map: PROTOCOL.md §4.5, from a
// hardware mapping session, FW 1.6.24).
/// Naming (how the pedal's display titles presets): 0 Name / 1 PC+Name /
/// 2 Bank+Name.
pub const G_NAMING: usize = 2;

/// MIDI channel, 1..=16.
pub const G_MIDI_CHANNEL: usize = 7;

/// MIDI Thru, 0 off / 1 thru.
pub const G_MIDI_THRU: usize = 8;

/// Input trim, dB (-15..=15, default 0).
pub const G_INPUT_TRIM: usize = 9;

/// Main (master) volume, dB (-40..=3, default 0).
pub const G_MAIN_VOLUME: usize = 10;

/// Interface (USB) volume, dB (default -10).
pub const G_INTERFACE_VOLUME: usize = 11;

/// Global cab sim, 1 active / 0 bypass.
pub const G_CAB_SIM: usize = 15;

/// Tuner mode, 0 off / 1 through / 2 muted.
pub const G_TUNER_MODE: usize = 16;

/// Tuner reference, Hz.
pub const G_TUNER_REF: usize = 17;

/// Tempo, BPM (one decimal).
pub const G_TEMPO: usize = 18;

/// Tempo mode, 0 global / 1 preset.
pub const G_TEMPO_MODE: usize = 19;

/// MIDI clock, 0 off / 1 master / 2 slave mix / 3 slave USB.
pub const G_MIDI_CLOCK: usize = 20;

pub const INPUT_TRIM_RANGE: (f32, f32) = (-15.0, 15.0);

pub const MAIN_VOLUME_RANGE: (f32, f32) = (-40.0, 3.0);

/// Global-config write header: type 0x0206, `82 <size LE>` patched in.
pub(crate) const GLOBAL_WRITE_HEADER: [u8; 11] = [
    0xB9, 0x03, 0x81, 0x06, 0x02, 0x82, 0x00, 0x00, 0x80, 0x10, 0x03,
];

/// The 22 global config values (index map: `G_*`, PROTOCOL.md §4.5).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Globals(pub [f32; GLOBALS_COUNT]);

impl Globals {
    pub fn get(&self, index: usize) -> f32 {
        self.0[index]
    }
}

/// Byte offset of global `index`'s `88 <f32>` item in a global config payload.
pub(crate) fn global_offset(payload: &[u8], index: usize) -> Result<usize, String> {
    let start = find(payload, &GLOBALS_MARKER).ok_or("global config marker not found")? + GLOBALS_MARKER.len();
    let off = start + index * PARAM_STRIDE;
    match payload.get(off..off + PARAM_STRIDE) {
        Some(item) if item[0] == FLOAT_TAG => Ok(off),
        Some(_) => Err(format!("global {index}: expected float tag")),
        None => Err(format!("global {index}: payload too short")),
    }
}

pub(crate) fn global_at(payload: &[u8], index: usize) -> Result<f32, String> {
    let off = global_offset(payload, index)?;
    Ok(f32::from_le_bytes([payload[off + 1], payload[off + 2], payload[off + 3], payload[off + 4]]))
}

/// All 22 globals from a global config payload (type 0x0206): floats
/// after `B9 02 BA 16`. Input trim and main volume are range-checked as a
/// layout fingerprint; the caller validates the others it uses.
pub(crate) fn extract_globals(payload: &[u8]) -> Result<Globals, String> {
    let mut values = [0.0; GLOBALS_COUNT];
    for (i, v) in values.iter_mut().enumerate() {
        *v = global_at(payload, i)?;
    }
    check_range("input trim", values[G_INPUT_TRIM], INPUT_TRIM_RANGE.0, INPUT_TRIM_RANGE.1)?;
    check_range("main volume", values[G_MAIN_VOLUME], MAIN_VOLUME_RANGE.0, MAIN_VOLUME_RANGE.1)?;
    Ok(Globals(values))
}

/// All 22 globals as one debug line ("[0]=1 [1]=0.5 ..."; "?" where an
/// item isn't a float). For mapping the still unknown indices (e.g. BPM).
pub(crate) fn globals_dump(payload: &[u8]) -> String {
    (0..GLOBALS_COUNT)
        .map(|i| match global_at(payload, i) {
            Ok(v) => format!("[{i}]={v}"),
            Err(_) => format!("[{i}]=?"),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Replaces global `index` in a stored global config payload.
pub(crate) fn patch_global(payload: &mut [u8], index: usize, value: f32) -> Result<(), String> {
    let off = global_offset(payload, index)?;
    payload[off + 1..off + 5].copy_from_slice(&value.to_le_bytes());
    Ok(())
}

/// Builds a global-config write from a received global config payload:
/// Builty's write header (`B9 03 81 06 02 82 <size> 80 10 03`) followed by
/// the received body (everything after the received header, i.e. from
/// `B9 01 B9 02 BA 16 ...`), with the size field set to the body length.
///
/// Builty sends a literal 0x7E before the header, a hard-coded size 0x0DF0,
/// and includes the 2 received CRC bytes at the end of its body (its unframe
/// keeps them); none of that is reproduced here.
pub(crate) fn global_write_request(received: &[u8]) -> Result<Vec<u8>, String> {
    if frame_type(received) != Some(TYPE_GLOBAL_CONFIG) {
        return Err("stored global config has an unexpected header".into());
    }
    // The body always starts at `B9 01 B9 02 BA 16`; locating it by marker
    // is robust against header-encoding variations (literal vs 80-prefixed).
    let start = find(received, &[0xB9, 0x01, 0xB9, 0x02, 0xBA, 0x16])
        .ok_or("global config body marker not found")?;
    let body = &received[start..];
    let len = u16::try_from(body.len()).map_err(|_| "global config too large")?;
    let mut req = GLOBAL_WRITE_HEADER.to_vec();
    req[6..8].copy_from_slice(&len.to_le_bytes());
    req.extend_from_slice(body);
    Ok(req)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::test_util::*;

    #[test]
    fn globals_dump_lists_all_values() {
        let line = globals_dump(&globals_payload(-2.5, -6.0));
        assert!(line.starts_with("[0]=1 [1]=1 "), "{line}");
        assert!(line.contains(" [9]=-2.5 [10]=-6 "), "{line}");
        assert!(line.ends_with(" [21]=1"), "{line}");
        assert_eq!(globals_dump(&[0xB9, 0x03]).matches("=?").count(), GLOBALS_COUNT);
    }

    #[test]
    fn extracts_globals() {
        let p = globals_payload(-3.5, -12.0);
        assert_eq!(frame_type(&p), Some(TYPE_GLOBAL_CONFIG));
        let g = extract_globals(&p).unwrap();
        assert_eq!((g.get(G_INPUT_TRIM), g.get(G_MAIN_VOLUME)), (-3.5, -12.0));
        assert_eq!(g.get(G_MIDI_CHANNEL), 1.0);
        assert!(extract_globals(&globals_payload(40.0, 0.0)).is_err());
        assert!(extract_globals(&[0xB9, 0x03]).is_err());
    }

    #[test]
    fn global_write_resends_patched_body() {
        let mut stored = globals_payload(0.0, 0.0);
        patch_global(&mut stored, G_MAIN_VOLUME, -6.0).unwrap();
        let req = global_write_request(&stored).unwrap();
        assert_eq!(&req[..6], &[0xB9, 0x03, 0x81, 0x06, 0x02, 0x82]);
        assert_eq!(&req[8..11], &[0x80, 0x10, 0x03]);
        let size = usize::from(u16::from_le_bytes([req[6], req[7]]));
        assert_eq!(size, req.len() - GLOBAL_WRITE_HEADER.len());
        assert_eq!(&req[11..17], &[0xB9, 0x01, 0xB9, 0x02, 0xBA, 0x16]);
        assert_eq!(extract_globals(&req).unwrap().get(G_MAIN_VOLUME), -6.0);
        assert!(global_write_request(&[0xB9, 0x03, 0x81, 0x04, 0x02]).is_err());
    }
}
