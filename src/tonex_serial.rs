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

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
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
    let deadline = Instant::now() + OVERALL_DEADLINE;
    let ports = candidate_ports();
    if ports.is_empty() {
        return Err("no /dev/ttyACM* port found".into());
    }

    let mut errors = Vec::new();
    for path in ports {
        if Instant::now() >= deadline {
            break;
        }
        match read_from_port(&path, deadline) {
            Ok(names) => return Ok(names),
            Err(e) => errors.push(format!("{}: {e}", path.display())),
        }
    }
    Err(errors.join("; "))
}

fn read_from_port(path: &Path, deadline: Instant) -> Result<Vec<String>, String> {
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
}
