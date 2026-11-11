//! Reads preset names from an IK Multimedia TONEX Pedal (the big floor unit,
//! USB ID 1963:0068) over its USB CDC-ACM serial port.
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
//! All port access is serialized by a process-wide lock so the name scan and
//! a params read never talk to the pedal at the same time.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
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

const PARAMS_DEADLINE: Duration = Duration::from_secs(4);
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
const P_MODEL_GAIN: usize = 20;
const P_MODEL_VOLUME: usize = 21;
const P_MODEL_PRESENCE: usize = 34;
const P_MODEL_DEPTH: usize = 35;

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
    // Tiny tolerance for float noise at the ends of the range.
    check_range(name, v, -0.01, 10.01)?;
    Ok(((v * 10.0).round() / 10.0).clamp(0.0, 10.0))
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
                Ok(0) => {}
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

// ---------------------------------------------------------------- public API

/// Reads all 150 preset names from the pedal. Unreadable slots are returned
/// as empty strings; fails only if no name at all could be read.
/// Bounded by an overall deadline so it never hangs.
pub fn read_preset_names() -> Result<Vec<String>, String> {
    with_pedal(OVERALL_DEADLINE, read_names)
}

/// Reads the amp knob values of preset `preset_idx` (0-based) from the
/// summary preset-details response.
pub fn read_amp_params(preset_idx: u8) -> Result<AmpParams, String> {
    if usize::from(preset_idx) >= PRESET_COUNT {
        return Err(format!("preset index {preset_idx} out of range"));
    }
    with_pedal(PARAMS_DEADLINE, |link, deadline| {
        link.send(&preset_request(preset_idx))?;
        let until = (Instant::now() + PRESET_TIMEOUT).min(deadline);
        // Skip unrelated frames (e.g. unsolicited state updates); the first
        // frame carrying a preset name is the details response.
        while let Some(payload) = link.read_frame(until.saturating_duration_since(Instant::now()), deadline)? {
            if find(&payload, &NAME_MARKER).is_some() {
                return extract_amp_params(&payload);
            }
        }
        Err("no preset details received".into())
    })
}

/// Takes the port lock, opens the first port that answers the handshake and
/// runs `op` on it. `timeout` bounds the port work; it starts once the lock
/// is held, so waiting for another serial user doesn't eat into it.
fn with_pedal<T>(
    timeout: Duration,
    mut op: impl FnMut(&mut Link, Instant) -> Result<T, String>,
) -> Result<T, String> {
    let _guard = port_lock();
    let deadline = Instant::now() + timeout;
    let ports = candidate_ports();
    if ports.is_empty() {
        return Err("no /dev/ttyACM* port found".into());
    }

    let mut errors = Vec::new();
    for path in ports {
        if Instant::now() >= deadline {
            errors.push("deadline reached".into());
            break;
        }
        let result = open_pedal(&path, deadline).and_then(|mut link| op(&mut link, deadline));
        match result {
            Ok(v) => return Ok(v),
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

fn read_names(link: &mut Link, deadline: Instant) -> Result<Vec<String>, String> {
    let mut names = vec![String::new(); PRESET_COUNT];
    let mut read = 0usize;
    for (i, slot) in names.iter_mut().enumerate() {
        if Instant::now() >= deadline {
            break;
        }
        link.send(&preset_request(i as u8))?;
        let until = (Instant::now() + PRESET_TIMEOUT).min(deadline);
        // Skip unrelated frames (e.g. unsolicited state updates).
        while let Some(payload) = link.read_frame(until.saturating_duration_since(Instant::now()), deadline)? {
            if let Some(name) = extract_name(&payload) {
                *slot = name;
                read += 1;
                break;
            }
        }
    }

    if read == 0 {
        return Err("pedal answered but no preset names were received".into());
    }
    if read < PRESET_COUNT {
        eprintln!("TONEX: read {read}/{PRESET_COUNT} preset names");
    }
    Ok(names)
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
            AmpParams { gain: 6.9, volume: 5.2, presence: 5.0, depth: 10.0, bass: 5.4, mid: 4.8, treble: 6.2 }
        );
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
