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
//!   effects observed) and used unconditionally (`SyncCtx::write_global`).
//!
//! A `Pedal` session thread owns the port for as long as the app is
//! connected: it answers our requests (names, params, writes) over the same
//! connection and reports unsolicited frames as `Event`s. Set
//! `TX_DEBUG_SERIAL=1` to log frames it doesn't handle (hex) to stderr.
//!
//! All port access is serialized by a process-wide lock: a new session
//! waits until the previous one has released the port.

mod framing;
mod port;
mod value;
mod messages;
mod params;
mod preset;
mod globals;
mod pedal;
mod session;
mod slots;
mod catalog;
#[cfg(test)]
mod test_util;

pub(crate) use framing::*;
pub(crate) use port::*;
pub(crate) use value::*;
pub(crate) use messages::*;
pub(crate) use params::*;
pub(crate) use preset::*;
pub(crate) use globals::*;
pub(crate) use pedal::*;
pub(crate) use session::*;
pub(crate) use slots::*;
pub(crate) use catalog::*;
