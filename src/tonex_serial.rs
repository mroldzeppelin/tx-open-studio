//! Talks to an IK Multimedia TONEX Pedal (the big floor unit, USB ID
//! 1963:0068) over its USB CDC-ACM serial port: preset names, amp params,
//! live change notifications and parameter writes.
//!
//! # Protocol notes (unofficial)
//!
//! Sources:
//! - vit3k/tonex_controller `protocol.md` (framing, CRC, value encoding;
//!   written for TONEX ONE, uses 0x0B as the device byte in headers).
//! - brunothesatellite/tonexpedal `index.html` (`syncUSB()`), which targets
//!   the big TONEX Pedal and reads all 150 names.
//! - Builty/TonexOneController `source/main/usb_tonex.c` (big pedal driver):
//!   `usb_tonex_hello`, `usb_tonex_request_state`,
//!   `usb_tonex_request_preset_details`, `TonexPresetByteMarker`.
//! The byte sequences below are identical in the last two sources.
//!
//! ## Link
//! CDC-ACM, 115200 8N1, DTR+RTS asserted (Builty sets line coding to 115200
//! and control line state DTR=1 RTS=1).
//!
//! ## Framing (HDLC-like, async, no address/control fields)
//! `7E <stuffed payload> <stuffed CRC lo> <stuffed CRC hi> 7E`
//! - Stuffing: 0x7E / 0x7D -> `7D (b ^ 0x20)`; applied to payload AND CRC.
//! - CRC: CRC-16/X.25 (a.k.a. IBM-SDLC): reflected poly 0x8408, init 0xFFFF,
//!   final XOR 0xFFFF, computed over the *unstuffed* payload, sent
//!   little-endian.
//!
//! ## Value encoding
//! `B9 n` / `BA n` / `BC n` = collection of n items (BC n = n raw bytes);
//! `80/81/82 lo hi` = u16 LE; `88 f32le` = float; other bytes < 0x80 = literal.
//!
//! ## Requests (big pedal uses 0x10 where TONEX ONE uses 0x0B)
//! Hello:
//!   `B9 03 00 82 04 00 80 10 01 | B9 02 02 10`
//! Request state:
//!   `B9 03 00 82 06 00 80 10 03 | B9 02 81 01 02 10`
//! Request preset details (summary, ~2 KB response), index 0..=127:
//!   `B9 03 81 00 02 82 06 00 80 10 03 | B9 04 10 01 <idx> 00`
//! index 128..=149 (index encoded as u16 `80 <idx> 00`):
//!   `B9 03 81 00 02 82 06 00 80 10 03 | B9 04 10 01 80 <idx> 00`
//!   (both references keep the size field at 0x0006 here; copied verbatim)
//! The final byte is "full details": 0x00 = summary, 0x01 = ~30 KB dump.
//!
//! ## Preset details response
//! Header `B9 03 81 04 02 ...` (type 0x0204 on the big pedal; 0x0304 on ONE).
//! The name is the 32 bytes following the marker
//!   `B9 04 B9 02 BC 21`
//! (`BC 21` = 33-byte blob: 32 name bytes, NUL padded, + 1 trailing byte).
//! Names are decoded as UTF-8 (lossy), NULs and surrounding spaces trimmed.
//!
//! The pedal may also send unsolicited state frames (type 0x0201) at any
//! time; frames without the name marker are simply skipped.
//!
//! ## Preset parameters (amp knobs)
//! Sources:
//! - Builty/TonexOneController `source/main/usb_tonex.c`
//!   `usb_tonex_parse_preset_parameters()`: the big-pedal driver parses the
//!   *summary* (0x00) preset-details response of the current preset this way.
//! - Builty `source/main/tonex_params.h` `enum TonexParameters` ("defined in
//!   the same order as they are sent by the Pedal") and `tonex_params.c`
//!   (ranges).
//! - brunothesatellite/tonexpedal `index.html` uses the same marker and
//!   5-byte stride (it only reads AMP enable / CAB type, see caveat below).
//!
//! Rule: find the first `BA 03 BA 6D` (`BA 6D` = array of 109 items); right
//! after it come consecutive `88 <f32 LE>` items, parameter `i` at byte
//! offset `i * 5`. Indices used here (Builty enum):
//!   11 EQ bass (0..10)      12 EQ bass freq (75..600 Hz)
//!   13 EQ mid (0..10)       16 EQ treble (0..10)
//!   17 EQ treble freq (1000..4000 Hz)
//!   18 amp enable (0/1)     20 model gain (0..10)   21 model volume (0..10)
//!   34 model presence (0..10)   35 model depth (0..10)
//! Values are stored in UI units (0..10), no scaling needed.
//!
//! Caveat: brunothesatellite documents AMP enable at index 17 and CAB type
//! at 22, whereas Builty has them at 18 and 24. Because of this conflict the
//! parser checks a layout fingerprint before trusting the values: bass freq
//! (12) and treble freq (17) must lie in their Hz ranges and amp enable (18)
//! must be 0 or 1. A shifted layout fails this check instead of showing junk.
//!
//! ## Live session (unsolicited messages, parameter writes)
//! Source: Builty `usb_tonex.c` `usb_tonex_parse()` type switch (big pedal):
//! - `0x0207` PARAM_CHANGED: body `B9 04 02 00 <param idx> 88 <f32 LE>`
//!   (`usb_tonex_parse_param_changed`). Builty refreshes its UI on these,
//!   which is how it follows changes made on the pedal ("IK new parameter
//!   comms", firmware with Editor support).
//! - `0x0204` preset details pushed by the pedal when the preset is changed
//!   on it; they contain `B9 04 00 00 [80] <preset idx>`
//!   (`TonexPresetIndexByteMarker`: "when preset is changed on pedal, this
//!   byte sequence is in the preset details"; absent in sync responses).
//! - `0x0201` state update (current preset at a fixed offset): not used.
//! - Writing one parameter (`usb_tonex_send_single_parameter`):
//!   `B9 03 81 07 02 82 0A 00 80 10 03 | B9 04 02 00 <idx> 88 <f32 LE>`
//!   (type 0x0207, size 0x000A = body length).
//!
//! ## Global config (pedal-wide, not per preset)
//! Source: Builty `usb_tonex.c` (`usb_tonex_request_global_config`,
//! `usb_tonex_parse_global_config`, `usb_tonex_send_global_config`,
//! `GLOBAL_CONFIG_INDEX_*`), ranges from `tonex_params.c`.
//! - Request: `B9 03 00 82 06 00 80 10 03 | B9 02 81 06 02 10`.
//! - Reply (and, per Builty's handler, possibly pushed): type `0x0206`;
//!   22 `88 <f32>` items after `B9 02 BA 16`. Index 9 = input trim
//!   (-15..15 dB), 10 = main volume (-40..+3 dB); both default 0 dB.
//! - Write: there is no single-global message; Builty resends the whole
//!   config blob (type 0x0206 header + received body with values patched).
//!   Implemented in `global_write_request`; verified on hardware (no side
//!   effects observed) and enabled unconditionally in main.rs.
//!
//! A `Pedal` session thread owns the port for as long as the app is
//! connected: it answers our requests (names, params, writes) over the same
//! connection and reports unsolicited frames as `Event`s. Set
//! `TX_DEBUG_SERIAL=1` to log frames it doesn't handle (hex) to stderr.
//!
//! All port access is serialized by a process-wide lock: a new session
//! waits until the previous one has released the port.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::collections::VecDeque;
use std::sync::{mpsc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serial2::SerialPort;

pub const PRESET_COUNT: usize = 150;

const BAUD_RATE: u32 = 115_200;
const IK_VENDOR_ID: &str = "1963";
const FLAG: u8 = 0x7E;
const ESCAPE: u8 = 0x7D;
const ESCAPE_XOR: u8 = 0x20;
/// Received frames larger than this are discarded (summary is ~2 KB).
const MAX_FRAME_LEN: usize = 64 * 1024;

const OVERALL_DEADLINE: Duration = Duration::from_secs(10);
const READ_POLL: Duration = Duration::from_millis(50);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(1500);
const PRESET_TIMEOUT: Duration = Duration::from_millis(1000);

const HELLO_CMD: [u8; 13] = [
    0xB9, 0x03, 0x00, 0x82, 0x04, 0x00, 0x80, 0x10, 0x01, 0xB9, 0x02, 0x02, 0x10,
];
const REQUEST_STATE_CMD: [u8; 15] = [
    0xB9, 0x03, 0x00, 0x82, 0x06, 0x00, 0x80, 0x10, 0x03, 0xB9, 0x02, 0x81, 0x01, 0x02, 0x10,
];
const PRESET_DETAILS_HEADER: [u8; 11] = [
    0xB9, 0x03, 0x81, 0x00, 0x02, 0x82, 0x06, 0x00, 0x80, 0x10, 0x03,
];
const NAME_MARKER: [u8; 6] = [0xB9, 0x04, 0xB9, 0x02, 0xBC, 0x21];
const NAME_LEN: usize = 32;

/// Read poll while idle, waiting for unsolicited frames.
const IDLE_POLL: Duration = Duration::from_millis(100);
const PARAM_MARKER: [u8; 4] = [0xBA, 0x03, 0xBA, 0x6D];
const FLOAT_TAG: u8 = 0x88;
const PARAM_STRIDE: usize = 5;

// Parameter indices in the preset-details float array (see header).
const P_EQ_BASS: usize = 11;
const P_EQ_BASS_FREQ: usize = 12;
const P_EQ_MID: usize = 13;
const P_EQ_TREBLE: usize = 16;
const P_EQ_TREBLE_FREQ: usize = 17;
const P_AMP_ENABLE: usize = 18;
/// Amp on/off (0.0 / 1.0); no MIDI CC exists for it, written over serial.
pub const AMP_ENABLE_PARAM: usize = P_AMP_ENABLE;
const P_MODEL_GAIN: usize = 20;
const P_MODEL_VOLUME: usize = 21;
const P_MODEL_PRESENCE: usize = 34;
const P_MODEL_DEPTH: usize = 35;

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

// Global config (Builty `usb_tonex.c`: `usb_tonex_request_global_config`,
// `usb_tonex_parse_global_config`, `usb_tonex_send_global_config`,
// `GLOBAL_CONFIG_INDEX_*`; ranges from `tonex_params.c`).
const REQUEST_GLOBALS_CMD: [u8; 15] = [
    0xB9, 0x03, 0x00, 0x82, 0x06, 0x00, 0x80, 0x10, 0x03, 0xB9, 0x02, 0x81, 0x06, 0x02, 0x10,
];
const GLOBALS_TIMEOUT: Duration = Duration::from_millis(1500);
/// `BA 16` = array of 22 globals (`88 <f32>` each), inside `B9 02`.
const GLOBALS_MARKER: [u8; 4] = [0xB9, 0x02, 0xBA, 0x16];
const GLOBALS_COUNT: usize = 22;
/// Input trim, dB (-15..=15, default 0).
pub const G_INPUT_TRIM: usize = 9;
/// Main (master) volume, dB (-40..=3, default 0).
pub const G_MAIN_VOLUME: usize = 10;
pub const INPUT_TRIM_RANGE: (f32, f32) = (-15.0, 15.0);
pub const MAIN_VOLUME_RANGE: (f32, f32) = (-40.0, 3.0);
/// Global-config write header: type 0x0206, `82 <size LE>` patched in.
const GLOBAL_WRITE_HEADER: [u8; 11] = [
    0xB9, 0x03, 0x81, 0x06, 0x02, 0x82, 0x00, 0x00, 0x80, 0x10, 0x03,
];

// Message types (header value after `B9 03`), big pedal.
/// State (reply to request-state, also pushed): body starts with the
/// current preset index.
const TYPE_STATE: u16 = 0x0201;
const TYPE_PRESET_DETAILS: u16 = 0x0204;
const TYPE_GLOBAL_CONFIG: u16 = 0x0206;
const TYPE_PARAM_CHANGED: u16 = 0x0207;
/// Body of a 0x0207 message: `B9 04 02 00 <param idx> 88 <f32 LE>`.
const PARAM_CHANGED_MARKER: [u8; 4] = [0xB9, 0x04, 0x02, 0x00];
/// Header of a single-parameter write (type 0x0207, body size 0x000A).
const SET_PARAM_HEADER: [u8; 11] = [
    0xB9, 0x03, 0x81, 0x07, 0x02, 0x82, 0x0A, 0x00, 0x80, 0x10, 0x03,
];
/// In preset details pushed after a footswitch preset change:
/// `B9 04 00 00 [80] <preset idx>`.
const PRESET_INDEX_MARKER: [u8; 4] = [0xB9, 0x04, 0x00, 0x00];

/// Serializes all access to the pedal's serial port.
static PORT_LOCK: Mutex<()> = Mutex::new(());

fn port_lock() -> MutexGuard<'static, ()> {
    PORT_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

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

// ---------------------------------------------------------------- framing

/// CRC-16/X.25: reflected poly 0x8408, init 0xFFFF, xorout 0xFFFF.
fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= u16::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0x8408 } else { crc >> 1 };
        }
    }
    !crc
}

fn push_stuffed(out: &mut Vec<u8>, b: u8) {
    if b == FLAG || b == ESCAPE {
        out.push(ESCAPE);
        out.push(b ^ ESCAPE_XOR);
    } else {
        out.push(b);
    }
}

fn build_frame(payload: &[u8]) -> Vec<u8> {
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
fn decode_frame(body: &[u8]) -> Option<Vec<u8>> {
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

fn preset_request(index: u8) -> Vec<u8> {
    let mut req = PRESET_DETAILS_HEADER.to_vec();
    req.extend_from_slice(&[0xB9, 0x04, 0x10, 0x01]);
    if index >= 0x80 {
        req.push(0x80);
    }
    req.push(index);
    req.push(0x00); // summary, not full details
    req
}

/// Single-parameter write (Builty `usb_tonex_send_single_parameter`, big
/// pedal; needs the firmware that came with Editor support).
/// `section` 02 = the 109-param array, 03 = preset volume (index 0).
fn set_param_request(section: u8, index: u8, value: f32) -> Vec<u8> {
    let mut req = SET_PARAM_HEADER.to_vec();
    req.extend_from_slice(&[0xB9, 0x04, section, 0x00]);
    req.push(index);
    req.push(FLOAT_TAG);
    req.extend_from_slice(&value.to_le_bytes());
    req
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Result of one preset-details read.
struct DetailsRead {
    amp: Result<AmpParams, String>,
    raw: Vec<f32>,
    character: Option<String>,
    level: Option<f32>,
}

impl DetailsRead {
    fn failed(e: String) -> Self {
        Self { amp: Err(e), raw: Vec::new(), character: None, level: None }
    }
}

/// Byte length of the encoded value at `pos` (recursing into collections:
/// `B9 n` / `BA n` = n items, `BC n` = n raw bytes, `88` = f32, `80` = 1
/// extra byte, `81`/`82` = 2 extra bytes, < 0x80 = literal).
fn value_len(p: &[u8], pos: usize) -> Option<usize> {
    let b = *p.get(pos)?;
    Some(match b {
        0x00..=0x7F => 1,
        0x80 => 2,
        0x81 | 0x82 => 3,
        FLOAT_TAG => 5,
        0xBC => 2 + usize::from(*p.get(pos + 1)?),
        0xB9 | 0xBA => {
            let n = usize::from(*p.get(pos + 1)?);
            let mut at = pos + 2;
            for _ in 0..n {
                at += value_len(p, at)?;
            }
            at - pos
        }
        _ => return None,
    })
}

/// Start offsets of the items of the collection (`B9 n`/`BA n`) at `pos`.
fn collection_items(p: &[u8], pos: usize) -> Option<Vec<usize>> {
    if !matches!(p.get(pos)?, 0xB9 | 0xBA) {
        return None;
    }
    let n = usize::from(*p.get(pos + 1)?);
    let mut at = pos + 2;
    let mut items = Vec::with_capacity(n);
    for _ in 0..n {
        items.push(at);
        at += value_len(p, at)?;
    }
    Some(items)
}

fn float_at(p: &[u8], pos: usize) -> Option<f32> {
    let b = p.get(pos..pos + 5)?;
    if b[0] != FLOAT_TAG {
        return None;
    }
    let v = f32::from_le_bytes([b[1], b[2], b[3], b[4]]);
    v.is_finite().then_some(v)
}

/// Element 0 of float array `n` (0-based) inside the effects collection
/// `BA 03 [BA 6D <109 params>, BA .., BA ..]`.
fn effects_array_first(payload: &[u8], n: usize) -> Option<f32> {
    let ba03 = find(payload, &PARAM_MARKER)?;
    let arrays = collection_items(payload, ba03)?;
    let first = *collection_items(payload, *arrays.get(n)?)?.first()?;
    float_at(payload, first)
}

/// The float in the name group: `B9 02 [BC 21 <33 B> <strlen>] BA 01 [88 f32]`
/// (vit3k capture). Only logged for diagnostics: the preset-volume knob
/// sweep arrived as section 03, which isn't this array.
fn name_group_float(payload: &[u8]) -> Option<f32> {
    let mut pos = find(payload, &NAME_MARKER)? + NAME_MARKER.len() + (NAME_LEN + 1);
    read_u16_value(payload, &mut pos)?; // name strlen
    if payload.get(pos..pos + 2)? != [0xBA, 0x01] {
        return None;
    }
    float_at(payload, pos + 2)
}

/// Write section of the preset volume (`B9 04 03 00 00 88 <f32>`), from a
/// hardware capture of the pedal's PRESET VOLUME knob (values 9.2 … 6.9).
pub const LEVEL_SECTION: u8 = 0x03;
/// Section of the 109-entry parameter array (`B9 04 02 00 <idx> ...`).
const PARAM_SECTION: u8 = 0x02;
/// Preset volume range (as shown on the pedal; observed 6.9..9.2).
pub const PRESET_LEVEL_RANGE: (f32, f32) = (0.0, 10.0);

/// Preset volume from a details payload.
///
/// Writes address float arrays as `<section> 00 <index>`. Section 02 is the
/// 109-param array (`BA 03`'s first array, confirmed), and Builty's old
/// preset-tempo write `01 00 01` hit element 1 of the name group's float
/// array, i.e. sections count float arrays in document order. Section 03 is
/// then the second array inside `BA 03`, element 0. UNVERIFIED against a
/// details dump: `TX_DEBUG_SERIAL=1` logs every candidate
/// (`preset_level_candidates`) to settle it. Range-checked so a wrong guess
/// shows nothing rather than nonsense.
fn extract_preset_level(payload: &[u8]) -> Option<f32> {
    effects_array_first(payload, 1).filter(|v| (PRESET_LEVEL_RANGE.0..=PRESET_LEVEL_RANGE.1).contains(v))
}

/// All plausible locations of the preset volume, for a debug log.
fn preset_level_candidates(payload: &[u8]) -> String {
    format!(
        "name-group BA01[0]={:?}  BA03[0][0]={:?}  BA03[1][0]={:?}  BA03[2][0]={:?}",
        name_group_float(payload),
        effects_array_first(payload, 0),
        effects_array_first(payload, 1),
        effects_array_first(payload, 2),
    )
}

/// A live change of the preset volume: `0x0207` with body
/// `B9 04 03 00 00 88 <f32>` (captured on hardware while turning the knob:
/// `B9 03 81 07 02 0A 02 B9 04 03 00 00 88 <f32>`).
fn extract_level_change(payload: &[u8]) -> Option<f32> {
    let start = find(payload, &[0xB9, 0x04, LEVEL_SECTION, 0x00, 0x00, FLOAT_TAG])? + 5;
    float_at(payload, start)
}

/// Extracts the preset name from a preset-details payload, if present.
fn extract_name(payload: &[u8]) -> Option<String> {
    let start = find(payload, &NAME_MARKER)? + NAME_MARKER.len();
    let bytes = payload.get(start..start + NAME_LEN)?;
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    Some(String::from_utf8_lossy(&bytes[..end]).trim().to_string())
}

/// Reads parameter `index` (an `88 <f32 LE>` item) from the array that
/// starts at `params`.
fn param_at(params: &[u8], index: usize) -> Result<f32, String> {
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

fn check_range(name: &str, v: f32, min: f32, max: f32) -> Result<(), String> {
    if (min..=max).contains(&v) {
        Ok(())
    } else {
        Err(format!("unexpected layout: {name} = {v} outside {min}..={max}"))
    }
}

/// A 0..10 knob value, rounded to the UI's 0.1 step.
fn knob_value(params: &[u8], index: usize, name: &str) -> Result<f32, String> {
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
fn extract_state_preset(payload: &[u8]) -> Option<u8> {
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
const META_FIELDS: usize = 13;
/// Field holding the preset's character ("CLEAN", "DRIVE", ...).
const META_CHARACTER: usize = 2;

/// Parses one metadata string field at `*pos`:
/// `B9 02 BC <bufsize> <buf> <strlen>` (strlen is a normal value). The text
/// is `buf[..strlen]`, cut at the first NUL; bytes after that are garbage.
fn read_meta_field(p: &[u8], pos: &mut usize) -> Option<String> {
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
fn extract_character(payload: &[u8]) -> Option<String> {
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
fn report_state(state: Option<&[u8]>, on_event: &impl Fn(Event), debug: bool) {
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

/// Global levels, in dB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Globals {
    pub input_trim: f32,
    pub master_volume: f32,
}

/// Byte offset of global `index`'s `88 <f32>` item in a global config payload.
fn global_offset(payload: &[u8], index: usize) -> Result<usize, String> {
    let start = find(payload, &GLOBALS_MARKER).ok_or("global config marker not found")? + GLOBALS_MARKER.len();
    let off = start + index * PARAM_STRIDE;
    match payload.get(off..off + PARAM_STRIDE) {
        Some(item) if item[0] == FLOAT_TAG => Ok(off),
        Some(_) => Err(format!("global {index}: expected float tag")),
        None => Err(format!("global {index}: payload too short")),
    }
}

fn global_at(payload: &[u8], index: usize) -> Result<f32, String> {
    let off = global_offset(payload, index)?;
    Ok(f32::from_le_bytes([payload[off + 1], payload[off + 2], payload[off + 3], payload[off + 4]]))
}

/// Input trim and main volume from a global config payload (type 0x0206):
/// floats after `B9 02 BA 16`, indices 9 and 10. Range-checked.
fn extract_globals(payload: &[u8]) -> Result<Globals, String> {
    let trim = global_at(payload, G_INPUT_TRIM)?;
    let vol = global_at(payload, G_MAIN_VOLUME)?;
    check_range("input trim", trim, INPUT_TRIM_RANGE.0, INPUT_TRIM_RANGE.1)?;
    check_range("main volume", vol, MAIN_VOLUME_RANGE.0, MAIN_VOLUME_RANGE.1)?;
    Ok(Globals { input_trim: trim, master_volume: vol })
}

/// Replaces global `index` in a stored global config payload.
fn patch_global(payload: &mut [u8], index: usize, value: f32) -> Result<(), String> {
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
fn global_write_request(received: &[u8]) -> Result<Vec<u8>, String> {
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
fn extract_amp_params(payload: &[u8]) -> Result<AmpParams, String> {
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

// ---------------------------------------------------------------- transport

struct Link {
    port: SerialPort,
    /// Bytes of the frame being assembled (after an opening flag).
    pending: Vec<u8>,
    in_frame: bool,
    /// Bytes read from the port but not yet consumed.
    rx: Vec<u8>,
    rx_pos: usize,
}

impl Link {
    fn open(path: &Path) -> Result<Self, OpenFailure> {
        let mut port = SerialPort::open(path, BAUD_RATE).map_err(|e| {
            // Only a real EACCES (no rights on the node), not EPERM, which
            // Rust also files under PermissionDenied.
            if e.raw_os_error() == Some(EACCES) {
                OpenFailure::NoAccess(path.to_path_buf())
            } else {
                OpenFailure::Other(format!("open {}: {e}", path.display()))
            }
        })?;
        port.set_read_timeout(READ_POLL)
            .and_then(|_| port.set_write_timeout(Duration::from_secs(1)))
            .map_err(|e| format!("configure {}: {e}", path.display()))?;
        // Some CDC devices only talk once DTR is asserted; ignore failures.
        let _ = port.set_dtr(true);
        let _ = port.set_rts(true);
        let _ = port.discard_buffers();
        Ok(Self { port, pending: Vec::new(), in_frame: false, rx: Vec::with_capacity(4096), rx_pos: 0 })
    }

    fn send(&self, payload: &[u8]) -> Result<(), String> {
        self.port
            .write_all(&build_frame(payload))
            .map_err(|e| format!("write: {e}"))
    }

    /// Returns the next valid frame payload, or None when `timeout` (capped
    /// by `deadline`) elapses. Frames with a bad CRC are dropped.
    fn read_frame(&mut self, timeout: Duration, deadline: Instant) -> Result<Option<Vec<u8>>, String> {
        let until = (Instant::now() + timeout).min(deadline);
        let mut chunk = [0u8; 4096];
        loop {
            while self.rx_pos < self.rx.len() {
                let b = self.rx[self.rx_pos];
                self.rx_pos += 1;
                if b == FLAG {
                    // A flag both closes the current frame and may open the
                    // next one; empty bodies (back-to-back flags) are skipped.
                    if self.in_frame && !self.pending.is_empty() {
                        let body = std::mem::take(&mut self.pending);
                        if let Some(payload) = decode_frame(&body) {
                            return Ok(Some(payload));
                        }
                    }
                    self.in_frame = true;
                } else if self.in_frame {
                    if self.pending.len() >= MAX_FRAME_LEN {
                        self.pending.clear();
                        self.in_frame = false;
                    } else {
                        self.pending.push(b);
                    }
                }
            }
            self.rx.clear();
            self.rx_pos = 0;

            if Instant::now() >= until {
                return Ok(None);
            }
            match self.port.read(&mut chunk) {
                // poll() said readable but nothing came: hang-up (unplugged).
                Ok(0) => return Err("device disconnected".into()),
                Ok(n) => self.rx.extend_from_slice(&chunk[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
                Err(e) => return Err(format!("read: {e}")),
            }
        }
    }
}

// ---------------------------------------------------------------- discovery

/// USB vendor ID of the device behind /dev/ttyACMx, read from sysfs.
fn usb_vendor_id(dev_name: &str) -> Option<String> {
    let iface = fs::canonicalize(format!("/sys/class/tty/{dev_name}/device")).ok()?;
    let vid = fs::read_to_string(iface.parent()?.join("idVendor")).ok()?;
    Some(vid.trim().to_lowercase())
}

/// /dev/ttyACM* ports, IK Multimedia devices first. Ports whose vendor is
/// known to be something else are skipped; unknown ones are kept as a
/// fallback (e.g. when sysfs is unavailable).
fn candidate_ports() -> Vec<PathBuf> {
    let mut ik = Vec::new();
    let mut unknown = Vec::new();
    if let Ok(entries) = fs::read_dir("/dev") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("ttyACM") {
                continue;
            }
            match usb_vendor_id(&name).as_deref() {
                Some(IK_VENDOR_ID) => ik.push(entry.path()),
                Some(_) => {}
                None => unknown.push(entry.path()),
            }
        }
    }
    ik.sort();
    unknown.sort();
    ik.extend(unknown);
    ik
}

/// Cheap presence check for the reconnect poll: is there a port a session
/// could try (same discovery as `Pedal::start`)? Opens nothing.
pub fn port_available() -> bool {
    !candidate_ports().is_empty()
}

// ---------------------------------------------------------------- frame classification

/// Reads one value at `*pos` in the `B9/BA/80/81/82/literal` encoding,
/// returning it as u16 (collections are not values; returns None).
fn read_u16_value(p: &[u8], pos: &mut usize) -> Option<u16> {
    let b = *p.get(*pos)?;
    match b {
        // One extra byte: u8 values 128..=255 (seen on the wire as
        // `80 8C` for preset index 140; the next byte after it is the
        // following value, not a high byte).
        0x80 => {
            let v = *p.get(*pos + 1)?;
            *pos += 2;
            Some(u16::from(v))
        }
        // Two extra bytes: u16 LE.
        0x81 | 0x82 => {
            let lo = *p.get(*pos + 1)?;
            let hi = *p.get(*pos + 2)?;
            *pos += 3;
            Some(u16::from(lo) | (u16::from(hi) << 8))
        }
        0x00..=0x7F => {
            *pos += 1;
            Some(u16::from(b))
        }
        _ => None,
    }
}

/// Message type from the `B9 03 <type> <size> <unknown>` header.
fn frame_type(payload: &[u8]) -> Option<u16> {
    if payload.get(..2)? != [0xB9, 0x03] {
        return None;
    }
    let mut pos = 2;
    read_u16_value(payload, &mut pos)
}

/// `0x0207` param-changed body: `B9 04 02 00 <param idx> 88 <f32 LE>`.
/// Returns (param index, value).
fn extract_param_change(payload: &[u8]) -> Option<(usize, f32)> {
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
fn extract_current_preset(payload: &[u8]) -> Option<u8> {
    let mut pos = find(payload, &PRESET_INDEX_MARKER)? + PRESET_INDEX_MARKER.len();
    if *payload.get(pos)? == 0x80 {
        pos += 1;
    }
    let idx = *payload.get(pos)?;
    (usize::from(idx) < PRESET_COUNT).then_some(idx)
}

fn debug_enabled() -> bool {
    std::env::var_os("TX_DEBUG_SERIAL").is_some()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------- session

/// Something the pedal told us. Delivered from the session thread.
#[derive(Debug)]
pub enum Event {
    /// Handshake done; the session is live.
    Connected(String),
    /// The session ended (open failed, unplugged, I/O error).
    Disconnected(String),
    /// Opening the port (path) failed with "permission denied"; always
    /// followed by `Disconnected`.
    NoAccess(String),
    /// Result of `Pedal::request_names`.
    Names(Result<Vec<PresetMeta>, String>),
    /// The lone per-preset float after the name (`BA 01 88 <f32>`), assumed
    /// to be the preset volume; see `extract_preset_level`. `preset_idx` is
    /// None for a live change (applies to the current preset).
    PresetLevel { preset_idx: Option<u8>, value: f32 },
    /// Character of one preset (0-based), from a details read or push.
    Character { preset_idx: u8, character: String },
    /// Result of `Pedal::request_amp_params` (0-based preset index).
    /// `raw` is the whole parameter array as far as it parsed (see
    /// `extract_param_array`), empty if the details didn't arrive.
    AmpParams { preset_idx: u8, result: Result<AmpParams, String>, raw: Vec<f32> },
    /// A parameter changed on the pedal (e.g. a physical knob was turned).
    /// `index` is the preset-parameter index (see `AmpKnob::param_index`).
    ParamChanged { index: usize, value: f32 },
    /// The pedal's current preset (0-based), from a state message: the
    /// handshake's request-state reply, or a pushed state update.
    CurrentPreset(u8),
    /// Global config (reply to `request_globals`, or pushed by the pedal).
    Globals(Result<Globals, String>),
    /// The pedal switched preset by itself (footswitch); carries the new
    /// preset's amp params when they parsed, and its raw parameter array.
    PresetChanged { preset_idx: u8, params: Option<AmpParams>, raw: Vec<f32> },
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

enum Request {
    Names,
    AmpParams(u8),
    /// (section, index, value).
    SetParam(u8, u8, f32),
    Globals,
    SetGlobal(usize, f32),
}

/// Handle to the session thread that owns the serial port. Dropping it ends
/// the session (the thread notices within one read poll, or after the
/// current preset during a name scan).
pub struct Pedal {
    tx: mpsc::Sender<Request>,
}

impl Pedal {
    /// Starts a session thread: it waits for any previous session to release
    /// the port, opens + handshakes, then serves requests and reports
    /// unsolicited pedal messages through `on_event` until the port fails or
    /// the handle is dropped. Returns immediately.
    pub fn start(on_event: impl Fn(Event) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _guard = port_lock();
            let link = match open_any() {
                Ok((link, path, state)) => {
                    on_event(Event::Connected(path.display().to_string()));
                    report_state(state.as_deref(), &on_event, debug_enabled());
                    link
                }
                Err(failure) => {
                    if let OpenFailure::NoAccess(path) = &failure {
                        on_event(Event::NoAccess(path.display().to_string()));
                    }
                    on_event(Event::Disconnected(failure.message()));
                    return;
                }
            };
            let mut session = Session { link, rx, queue: VecDeque::new(), on_event: &on_event,
                debug: debug_enabled(),
                globals: None,
                globals_seen: 0,
            };
            let reason = session.run();
            on_event(Event::Disconnected(reason));
        });
        Self { tx }
    }

    fn send(&self, r: Request) -> Result<(), String> {
        self.tx.send(r).map_err(|_| "serial session not running".to_string())
    }

    pub fn request_names(&self) -> Result<(), String> {
        self.send(Request::Names)
    }

    /// Writes one preset parameter (index < 0x80) on the pedal.
    pub fn set_param(&self, index: usize, value: f32) -> Result<(), String> {
        match u8::try_from(index) {
            Ok(i) if i < 0x80 => self.send(Request::SetParam(PARAM_SECTION, i, value)),
            _ => Err(format!("param index {index} not supported for writes")),
        }
    }

    /// Reads the amp params of `preset_idx` (0-based).
    /// Writes the current preset's volume (section 03, index 0).
    pub fn set_preset_level(&self, value: f32) -> Result<(), String> {
        self.send(Request::SetParam(LEVEL_SECTION, 0, value))
    }

    pub fn request_amp_params(&self, preset_idx: u8) -> Result<(), String> {
        self.send(Request::AmpParams(preset_idx))
    }

    /// Reads the global config; answered with `Event::Globals`.
    pub fn request_globals(&self) -> Result<(), String> {
        self.send(Request::Globals)
    }

    /// Writes one global (`G_INPUT_TRIM` / `G_MAIN_VOLUME`) by sending the
    /// last global config read back with that value changed. Verified on
    /// hardware.
    pub fn set_global(&self, index: usize, value: f32) -> Result<(), String> {
        if index >= GLOBALS_COUNT {
            return Err(format!("global index {index} out of range"));
        }
        self.send(Request::SetGlobal(index, value))
    }
}

/// Linux errno for "permission denied" on open (no libc dependency).
const EACCES: i32 = 13;

/// Why no session could be opened.
#[derive(Debug, PartialEq, Eq)]
enum OpenFailure {
    /// The device node exists but may not be opened (EACCES): typically
    /// the udev rule granting access is not installed.
    NoAccess(PathBuf),
    Other(String),
}

impl From<String> for OpenFailure {
    fn from(e: String) -> Self {
        OpenFailure::Other(e)
    }
}

impl OpenFailure {
    fn message(&self) -> String {
        match self {
            OpenFailure::NoAccess(path) => format!("open {}: permission denied", path.display()),
            OpenFailure::Other(e) => e.clone(),
        }
    }
}

/// Combines the per-port failures of one open attempt: if none of the
/// ports could be used and one of them refused access, that is the
/// actionable problem to report; otherwise all messages.
fn combine_failures(failures: Vec<OpenFailure>) -> OpenFailure {
    if let Some(i) = failures.iter().position(|f| matches!(f, OpenFailure::NoAccess(_))) {
        return failures.into_iter().nth(i).expect("index from position");
    }
    OpenFailure::Other(failures.iter().map(OpenFailure::message).collect::<Vec<_>>().join("; "))
}

/// Opens the first candidate port that answers the handshake. Also returns
/// the state reply (if one arrived) for the current preset.
fn open_any() -> Result<(Link, PathBuf, Option<Vec<u8>>), OpenFailure> {
    let ports = candidate_ports();
    if ports.is_empty() {
        return Err(OpenFailure::Other("no /dev/ttyACM* port found".into()));
    }
    let mut failures = Vec::new();
    for path in ports {
        match open_pedal(&path, Instant::now() + HANDSHAKE_TIMEOUT * 2) {
            Ok((link, state)) => return Ok((link, path, state)),
            Err(OpenFailure::Other(e)) => failures.push(OpenFailure::Other(format!("{}: {e}", path.display()))),
            Err(denied) => failures.push(denied),
        }
    }
    Err(combine_failures(failures))
}

fn open_pedal(path: &Path, deadline: Instant) -> Result<(Link, Option<Vec<u8>>), OpenFailure> {
    let mut link = Link::open(path)?;

    // Handshake. Replies are not strictly required, but the pedal expects
    // hello + state before serving preset details.
    link.send(&HELLO_CMD)?;
    let hello = link.read_frame(HANDSHAKE_TIMEOUT, deadline)?;
    if hello.is_none() {
        return Err(OpenFailure::Other("no reply to hello (not a TONEX Pedal?)".into()));
    }
    link.send(&REQUEST_STATE_CMD)?;
    // Keep the state reply (type 0x0201): it carries the current preset.
    let until = (Instant::now() + HANDSHAKE_TIMEOUT).min(deadline);
    let mut state = None;
    while let Some(payload) = link.read_frame(until.saturating_duration_since(Instant::now()), until)? {
        if frame_type(&payload) == Some(TYPE_STATE) {
            state = Some(payload);
            break;
        }
    }
    Ok((link, state))
}

struct Session<'a, F: Fn(Event)> {
    link: Link,
    rx: mpsc::Receiver<Request>,
    queue: VecDeque<Request>,
    on_event: &'a F,
    debug: bool,
    /// Last global config payload received (needed to write a global).
    globals: Option<Vec<u8>>,
    /// Counts received global config frames (to detect our reply).
    globals_seen: u32,
}

impl<F: Fn(Event)> Session<'_, F> {
    /// Main loop; returns why the session ended.
    fn run(&mut self) -> String {
        loop {
            if !self.pull_requests() {
                return "session closed".into();
            }
            let result = match self.serve_quick() {
                Ok(true) => Ok(()),
                Ok(false) if self.take_names_request() => {
                    self.read_names().map(|r| (self.on_event)(Event::Names(r)))
                }
                Ok(false) => self.idle(),
                Err(e) => Err(e),
            };
            if let Err(e) = result {
                return e;
            }
        }
    }

    /// Serves queued quick requests: all writes (in order), then only the
    /// newest amp-params read. Returns whether anything was served.
    fn serve_quick(&mut self) -> Result<bool, String> {
        let mut writes: Vec<(u8, u8, f32)> = Vec::new();
        let mut global_writes: Vec<(usize, f32)> = Vec::new();
        let mut read_globals = false;
        let mut latest_read = None;
        let mut rest = VecDeque::new();
        for r in std::mem::take(&mut self.queue) {
            match r {
                // A drag queues many writes per param: keep the latest value,
                // in first-seen order across params.
                Request::SetParam(section, index, value) => {
                    match writes.iter_mut().find(|(s, i, _)| (*s, *i) == (section, index)) {
                        Some(w) => w.2 = value,
                        None => writes.push((section, index, value)),
                    }
                }
                Request::SetGlobal(index, value) => match global_writes.iter_mut().find(|(i, _)| *i == index) {
                    Some(w) => w.1 = value,
                    None => global_writes.push((index, value)),
                },
                Request::Globals => read_globals = true,
                Request::AmpParams(idx) => latest_read = Some(idx),
                other => rest.push_back(other),
            }
        }
        self.queue = rest;
        for &(section, index, value) in &writes {
            self.link.send(&set_param_request(section, index, value))?;
        }
        let mut served = !writes.is_empty() || !global_writes.is_empty();
        if !global_writes.is_empty() {
            self.write_globals(&global_writes)?;
        }
        if read_globals {
            self.read_globals()?;
            served = true;
        }
        if let Some(idx) = latest_read {
            let read = self.read_amp_params(idx)?;
            (self.on_event)(Event::AmpParams { preset_idx: idx, result: read.amp, raw: read.raw });
            if let Some(character) = read.character {
                (self.on_event)(Event::Character { preset_idx: idx, character });
            }
            if let Some(value) = read.level {
                (self.on_event)(Event::PresetLevel { preset_idx: Some(idx), value });
            }
            served = true;
        }
        Ok(served)
    }

    /// Removes all queued names requests; true if there was one.
    fn take_names_request(&mut self) -> bool {
        let before = self.queue.len();
        self.queue.retain(|r| !matches!(r, Request::Names));
        self.queue.len() != before
    }

    /// Moves pending requests into the local queue; false once the handle
    /// is gone (the session should end).
    fn pull_requests(&mut self) -> bool {
        loop {
            match self.rx.try_recv() {
                Ok(r) => self.queue.push_back(r),
                Err(mpsc::TryRecvError::Empty) => return true,
                Err(mpsc::TryRecvError::Disconnected) => return false,
            }
        }
    }

    /// Requests the global config; the reply is reported from `dispatch`.
    fn read_globals(&mut self) -> Result<(), String> {
        let seen = self.globals_seen;
        self.link.send(&REQUEST_GLOBALS_CMD)?;
        let until = Instant::now() + GLOBALS_TIMEOUT;
        while self.globals_seen == seen {
            match self.link.read_frame(until.saturating_duration_since(Instant::now()), until)? {
                Some(payload) => {
                    self.dispatch(&payload, false);
                }
                None => {
                    (self.on_event)(Event::Globals(Err("no global config received".into())));
                    break;
                }
            }
        }
        Ok(())
    }

    /// Sends the stored global config back with `changes` applied.
    fn write_globals(&mut self, changes: &[(usize, f32)]) -> Result<(), String> {
        let Some(stored) = self.globals.as_mut() else {
            eprintln!("TONEX: global write skipped: global config not read yet");
            return Ok(());
        };
        for &(index, value) in changes {
            if let Err(e) = patch_global(stored, index, value) {
                eprintln!("TONEX: global write skipped: {e}");
                return Ok(());
            }
        }
        match global_write_request(stored) {
            Ok(req) => self.link.send(&req),
            Err(e) => {
                eprintln!("TONEX: global write skipped: {e}");
                Ok(())
            }
        }
    }

    /// Waits a short while for unsolicited frames.
    fn idle(&mut self) -> Result<(), String> {
        let far = Instant::now() + Duration::from_secs(3600);
        if let Some(payload) = self.link.read_frame(IDLE_POLL, far)? {
            self.dispatch(&payload, false);
        }
        Ok(())
    }

    /// Handles one incoming frame. Returns true when `awaiting` a details
    /// response and this frame is it.
    ///
    /// While a request is pending, the first frame carrying a preset name is
    /// taken as its response (the behaviour verified on hardware for the
    /// name scan). Only while idle is a details frame with the preset-index
    /// marker treated as a footswitch push, so a push that lands during a
    /// request is missed (rare; the next preset change re-syncs).
    fn dispatch(&mut self, payload: &[u8], awaiting: bool) -> bool {
        let ty = frame_type(payload);
        if ty == Some(TYPE_GLOBAL_CONFIG) {
            let result = extract_globals(payload);
            if result.is_ok() {
                self.globals = Some(payload.to_vec());
            }
            self.globals_seen = self.globals_seen.wrapping_add(1);
            (self.on_event)(Event::Globals(result));
            return false;
        }
        if ty == Some(TYPE_STATE) {
            // Pushed state update (e.g. preset changed on the pedal).
            report_state(Some(payload), self.on_event, self.debug);
            return false;
        }
        if ty == Some(TYPE_PARAM_CHANGED) {
            if let Some((index, value)) = extract_param_change(payload) {
                (self.on_event)(Event::ParamChanged { index, value });
            } else if let Some(value) = extract_level_change(payload) {
                (self.on_event)(Event::PresetLevel { preset_idx: None, value });
            } else if self.debug {
                eprintln!("TONEX serial: unparsed 0x0207: {}", hex(payload));
            }
            return false;
        }
        if awaiting && find(payload, &NAME_MARKER).is_some() {
            return true;
        }
        if !awaiting && ty == Some(TYPE_PRESET_DETAILS) {
            if let Some(idx) = extract_current_preset(payload) {
                let params = extract_amp_params(payload)
                    .map_err(|e| eprintln!("TONEX: pushed preset {idx} params: {e}"))
                    .ok();
                let raw = extract_param_array(payload);
                (self.on_event)(Event::PresetChanged { preset_idx: idx, params, raw });
                if let Some(character) = extract_character(payload) {
                    (self.on_event)(Event::Character { preset_idx: idx, character });
                }
                if let Some(value) = extract_preset_level(payload) {
                    (self.on_event)(Event::PresetLevel { preset_idx: Some(idx), value });
                }
                return false;
            }
        }
        if self.debug {
            let shown = &payload[..payload.len().min(48)];
            eprintln!("TONEX serial: unhandled frame type {ty:04X?} ({} B): {}", payload.len(), hex(shown));
        }
        false
    }

    /// Requests details of preset `idx` and returns the response payload
    /// (None on timeout), dispatching unrelated frames meanwhile.
    fn fetch_details(&mut self, idx: u8, timeout: Duration) -> Result<Option<Vec<u8>>, String> {
        self.link.send(&preset_request(idx))?;
        let until = Instant::now() + timeout;
        while let Some(payload) = self.link.read_frame(until.saturating_duration_since(Instant::now()), until)? {
            if self.dispatch(&payload, true) {
                return Ok(Some(payload));
            }
        }
        Ok(None)
    }

    /// Reads preset `idx`'s details. Ok(read) for the events; Err only for
    /// link failures (session ends).
    fn read_amp_params(&mut self, idx: u8) -> Result<DetailsRead, String> {
        if usize::from(idx) >= PRESET_COUNT {
            return Ok(DetailsRead::failed(format!("preset index {idx} out of range")));
        }
        let details = self.fetch_details(idx, PRESET_TIMEOUT)?;
        if let (true, Some(payload)) = (self.debug, details.as_deref()) {
            eprintln!("TONEX serial: preset {idx} level candidates: {}", preset_level_candidates(payload));
        }
        Ok(match details {
            Some(payload) => DetailsRead {
                amp: extract_amp_params(&payload),
                raw: extract_param_array(&payload),
                character: extract_character(&payload),
                level: extract_preset_level(&payload),
            },
            None => DetailsRead::failed("no preset details received".into()),
        })
    }

    /// Reads all 150 names. Unreadable slots are empty strings; the inner
    /// result fails only if no name at all could be read.
    fn read_names(&mut self) -> Result<Result<Vec<PresetMeta>, String>, String> {
        let deadline = Instant::now() + OVERALL_DEADLINE;
        let mut names = vec![PresetMeta::default(); PRESET_COUNT];
        let mut read = 0usize;
        for i in 0..PRESET_COUNT {
            if Instant::now() >= deadline {
                break;
            }
            // Stay responsive during the ~10 s scan: knob writes and the
            // current preset's params go in between names.
            if !self.pull_requests() {
                return Err("session closed".into());
            }
            self.serve_quick()?;
            let timeout = PRESET_TIMEOUT.min(deadline.saturating_duration_since(Instant::now()));
            if let Some(payload) = self.fetch_details(i as u8, timeout)? {
                if let Some(name) = extract_name(&payload) {
                    names[i] = PresetMeta { name, character: extract_character(&payload).unwrap_or_default() };
                    read += 1;
                }
            }
        }

        if read == 0 {
            return Ok(Err("pedal answered but no preset names were received".into()));
        }
        if read < PRESET_COUNT {
            eprintln!("TONEX: read {read}/{PRESET_COUNT} preset names");
        }
        Ok(Ok(names))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_failures_report_permission_problem_first() {
        let denied = PathBuf::from("/dev/ttyACM1");
        let f = combine_failures(vec![
            OpenFailure::Other("/dev/ttyACM0: no reply to hello".into()),
            OpenFailure::NoAccess(denied.clone()),
        ]);
        assert_eq!(f, OpenFailure::NoAccess(denied));
        assert_eq!(f.message(), "open /dev/ttyACM1: permission denied");

        let f = combine_failures(vec![OpenFailure::Other("a".into()), OpenFailure::Other("b".into())]);
        assert_eq!(f, OpenFailure::Other("a; b".into()));
    }

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

    #[test]
    fn extracts_name() {
        let mut p = vec![0xB9, 0x03, 0x81, 0x04, 0x02];
        p.extend_from_slice(&NAME_MARKER);
        p.extend_from_slice(b"BazPlexi\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0");
        p.push(0x14);
        assert_eq!(extract_name(&p).as_deref(), Some("BazPlexi"));
    }

    /// Synthetic summary payload: header + name + param array with the
    /// given values (unset slots default to 0.0).
    fn payload_with_params(set: &[(usize, f32)]) -> Vec<u8> {
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

    /// Synthetic global config reply: header, `B9 01 B9 02 BA 16`, 22 floats,
    /// plus a trailing item standing in for the rest of the config.
    fn globals_payload(trim: f32, vol: f32) -> Vec<u8> {
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

    fn meta_field(buf_size: u8, text: &[u8], garbage: &[u8]) -> Vec<u8> {
        let mut f = vec![0xB9, 0x02, 0xBC, buf_size];
        let mut buf = text.to_vec();
        buf.push(0);
        buf.extend_from_slice(garbage);
        buf.resize(usize::from(buf_size), 0xAA);
        f.extend_from_slice(&buf);
        f.push(text.len() as u8);
        f
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
    fn parses_captured_preset_volume_frames() {
        // Exact frames pushed while turning PRESET VOLUME (9.2, then 6.9).
        for v in [9.2f32, 6.9] {
            let mut f = vec![0xB9, 0x03, 0x81, 0x07, 0x02, 0x0A, 0x02, 0xB9, 0x04, 0x03, 0x00, 0x00, 0x88];
            f.extend_from_slice(&v.to_le_bytes());
            assert_eq!(frame_type(&f), Some(TYPE_PARAM_CHANGED));
            assert_eq!(extract_level_change(&f), Some(v));
            assert_eq!(extract_param_change(&f), None);
        }
    }

    #[test]
    fn preset_level_write_bytes() {
        let req = set_param_request(LEVEL_SECTION, 0, 7.5);
        let mut want = SET_PARAM_HEADER.to_vec();
        want.extend_from_slice(&[0xB9, 0x04, 0x03, 0x00, 0x00, 0x88]);
        want.extend_from_slice(&7.5f32.to_le_bytes());
        assert_eq!(req, want);
        // Body length matches the header's size field (0x0A).
        assert_eq!(req.len() - SET_PARAM_HEADER.len(), 0x0A);
        // The pedal's own push format parses back.
        assert_eq!(extract_level_change(&req), Some(7.5));
    }

    #[test]
    fn walks_effects_arrays() {
        // name group with BA 01 float, then BA 03 [BA 03 params, BA 02, BA 01].
        let mut p = vec![0xB9, 0x03, 0x81, 0x04, 0x02];
        p.extend_from_slice(&NAME_MARKER);
        p.extend_from_slice(&[0u8; NAME_LEN + 1]);
        p.push(0x00); // strlen
        p.extend_from_slice(&[0xBA, 0x01, 0x88]);
        p.extend_from_slice(&1.0f32.to_le_bytes());
        let ba03 = p.len();
        p.extend_from_slice(&[0xBA, 0x03, 0xBA, 0x03]);
        for v in [0.0f32, 2.0, 3.0] {
            p.push(0x88);
            p.extend_from_slice(&v.to_le_bytes());
        }
        p.extend_from_slice(&[0xBA, 0x02]);
        for v in [8.3f32, 4.0] {
            p.push(0x88);
            p.extend_from_slice(&v.to_le_bytes());
        }
        p.extend_from_slice(&[0xBA, 0x01, 0x88]);
        p.extend_from_slice(&5.5f32.to_le_bytes());
        // Real data is found via the `BA 03 BA 6D` marker; here the walker
        // is tested directly from the known offset.
        let arrays = collection_items(&p, ba03).unwrap();
        assert_eq!(arrays.len(), 3);
        assert_eq!(float_at(&p, collection_items(&p, arrays[1]).unwrap()[0]), Some(8.3));
        assert_eq!(float_at(&p, collection_items(&p, arrays[2]).unwrap()[0]), Some(5.5));
        assert_eq!(name_group_float(&p), Some(1.0));
        assert_eq!(value_len(&p, ba03), Some(p.len() - ba03));
    }

    #[test]
    fn extracts_globals() {
        let p = globals_payload(-3.5, -12.0);
        assert_eq!(frame_type(&p), Some(TYPE_GLOBAL_CONFIG));
        assert_eq!(extract_globals(&p).unwrap(), Globals { input_trim: -3.5, master_volume: -12.0 });
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
        assert_eq!(extract_globals(&req).unwrap().master_volume, -6.0);
        assert!(global_write_request(&[0xB9, 0x03, 0x81, 0x04, 0x02]).is_err());
    }

    #[test]
    fn reads_amp_disabled() {
        let p = payload_with_params(&[(P_AMP_ENABLE, 0.0)]);
        assert!(!extract_amp_params(&p).unwrap().amp_enabled);
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
    fn extracts_pushed_preset_index() {
        let mut p = vec![0xB9, 0x03, 0x81, 0x04, 0x02];
        p.extend_from_slice(&[0xB9, 0x04, 0x00, 0x00, 42]);
        assert_eq!(extract_current_preset(&p), Some(42));
        let mut p = vec![0xB9, 0x03, 0x81, 0x04, 0x02];
        p.extend_from_slice(&[0xB9, 0x04, 0x00, 0x00, 0x80, 140]);
        assert_eq!(extract_current_preset(&p), Some(140));
        assert_eq!(extract_current_preset(&[0xB9, 0x04, 0x00, 0x00, 0x80, 200]), None);
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
