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

// Message types (header value after `B9 03`), big pedal.
const TYPE_PRESET_DETAILS: u16 = 0x0204;
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
fn set_param_request(index: u8, value: f32) -> Vec<u8> {
    let mut req = SET_PARAM_HEADER.to_vec();
    req.extend_from_slice(&PARAM_CHANGED_MARKER);
    req.push(index);
    req.push(FLOAT_TAG);
    req.extend_from_slice(&value.to_le_bytes());
    req
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
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
    fn open(path: &Path) -> Result<Self, String> {
        let mut port = SerialPort::open(path, BAUD_RATE)
            .map_err(|e| format!("open {}: {e}", path.display()))?;
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

// ---------------------------------------------------------------- frame classification

/// Reads one value at `*pos` in the `B9/BA/80/81/82/literal` encoding,
/// returning it as u16 (collections are not values; returns None).
fn read_u16_value(p: &[u8], pos: &mut usize) -> Option<u16> {
    let b = *p.get(*pos)?;
    match b {
        0x80..=0x82 => {
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
    /// Result of `Pedal::request_names`.
    Names(Result<Vec<String>, String>),
    /// Result of `Pedal::request_amp_params` (0-based preset index).
    AmpParams { preset_idx: u8, result: Result<AmpParams, String> },
    /// A parameter changed on the pedal (e.g. a physical knob was turned).
    /// `index` is the preset-parameter index (see `AmpKnob::param_index`).
    ParamChanged { index: usize, value: f32 },
    /// The pedal switched preset by itself (footswitch); carries the new
    /// preset's amp params when they parsed.
    PresetChanged { preset_idx: u8, params: Option<AmpParams> },
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
    SetParam(u8, f32),
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
                Ok((link, path)) => {
                    on_event(Event::Connected(path.display().to_string()));
                    link
                }
                Err(e) => {
                    on_event(Event::Disconnected(e));
                    return;
                }
            };
            let mut session = Session { link, rx, queue: VecDeque::new(), on_event: &on_event, debug: debug_enabled() };
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
            Ok(i) if i < 0x80 => self.send(Request::SetParam(i, value)),
            _ => Err(format!("param index {index} not supported for writes")),
        }
    }

    /// Reads the amp params of `preset_idx` (0-based).
    pub fn request_amp_params(&self, preset_idx: u8) -> Result<(), String> {
        self.send(Request::AmpParams(preset_idx))
    }
}

/// Opens the first candidate port that answers the handshake.
fn open_any() -> Result<(Link, PathBuf), String> {
    let ports = candidate_ports();
    if ports.is_empty() {
        return Err("no /dev/ttyACM* port found".into());
    }
    let mut errors = Vec::new();
    for path in ports {
        match open_pedal(&path, Instant::now() + HANDSHAKE_TIMEOUT * 2) {
            Ok(link) => return Ok((link, path)),
            Err(e) => errors.push(format!("{}: {e}", path.display())),
        }
    }
    Err(errors.join("; "))
}

fn open_pedal(path: &Path, deadline: Instant) -> Result<Link, String> {
    let mut link = Link::open(path)?;

    // Handshake. Replies are not strictly required, but the pedal expects
    // hello + state before serving preset details.
    link.send(&HELLO_CMD)?;
    let hello = link.read_frame(HANDSHAKE_TIMEOUT, deadline)?;
    if hello.is_none() {
        return Err("no reply to hello (not a TONEX Pedal?)".into());
    }
    link.send(&REQUEST_STATE_CMD)?;
    let _ = link.read_frame(HANDSHAKE_TIMEOUT, deadline)?;
    Ok(link)
}

struct Session<'a, F: Fn(Event)> {
    link: Link,
    rx: mpsc::Receiver<Request>,
    queue: VecDeque<Request>,
    on_event: &'a F,
    debug: bool,
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
        let mut served = false;
        let mut latest_read = None;
        let mut rest = VecDeque::new();
        for r in std::mem::take(&mut self.queue) {
            match r {
                Request::SetParam(index, value) => {
                    self.link.send(&set_param_request(index, value))?;
                    served = true;
                }
                Request::AmpParams(idx) => latest_read = Some(idx),
                other => rest.push_back(other),
            }
        }
        self.queue = rest;
        if let Some(idx) = latest_read {
            let result = self.read_amp_params(idx)?;
            (self.on_event)(Event::AmpParams { preset_idx: idx, result });
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
        if ty == Some(TYPE_PARAM_CHANGED) {
            match extract_param_change(payload) {
                Some((index, value)) => (self.on_event)(Event::ParamChanged { index, value }),
                None if self.debug => eprintln!("TONEX serial: unparsed 0x0207: {}", hex(payload)),
                None => {}
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
                (self.on_event)(Event::PresetChanged { preset_idx: idx, params });
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

    /// Ok(result) for the event; Err only for link failures (session ends).
    fn read_amp_params(&mut self, idx: u8) -> Result<Result<AmpParams, String>, String> {
        if usize::from(idx) >= PRESET_COUNT {
            return Ok(Err(format!("preset index {idx} out of range")));
        }
        Ok(match self.fetch_details(idx, PRESET_TIMEOUT)? {
            Some(payload) => extract_amp_params(&payload),
            None => Err("no preset details received".into()),
        })
    }

    /// Reads all 150 names. Unreadable slots are empty strings; the inner
    /// result fails only if no name at all could be read.
    fn read_names(&mut self) -> Result<Result<Vec<String>, String>, String> {
        let deadline = Instant::now() + OVERALL_DEADLINE;
        let mut names = vec![String::new(); PRESET_COUNT];
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
            if let Some(name) = self.fetch_details(i as u8, timeout)?.as_deref().and_then(extract_name) {
                names[i] = name;
                read += 1;
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
    fn reads_amp_disabled() {
        let p = payload_with_params(&[(P_AMP_ENABLE, 0.0)]);
        assert!(!extract_amp_params(&p).unwrap().amp_enabled);
    }

    #[test]
    fn set_param_matches_builty_bytes() {
        let req = set_param_request(18, 1.0);
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
        let frame = set_param_request(P_MODEL_GAIN as u8, 7.5);
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
