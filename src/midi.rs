//! MIDI output: only the tuner toggle (CC9) on the pedal's MIDI channel.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::sync::MutexGuard;
use std::sync::Mutex;
use midir::MidiOutputConnection;
use midir::MidiOutput;

pub(crate) const CLIENT_NAME: &str = "tx-open-studio";

/// MIDI channel 1..=16 (the pedal's global, as read or chosen) → the
/// status byte's low nibble 0..=15; None for anything else.
pub(crate) fn channel_nibble(channel: f32) -> Option<u8> {
    (channel.fract() == 0.0 && (1.0..=16.0).contains(&channel)).then(|| channel as u8 - 1)
}

/// Channel-voice status byte: message kind (0xB0 CC, 0xC0 PC) + channel.
pub(crate) fn status_byte(kind: u8, nibble: u8) -> u8 {
    (kind & 0xF0) | (nibble & 0x0F)
}

/// Thread-safe holder for the MIDI output connection to the pedal.
#[derive(Clone, Default)]
pub(crate) struct MidiEngine {
    pub(crate) conn: Arc<Mutex<Option<MidiOutputConnection>>>,
    /// Channel every message goes out on, as a status nibble (0 = channel
    /// 1, the default until the pedal's MIDI channel global is read).
    pub(crate) channel: Arc<AtomicU8>,
}

impl MidiEngine {
    pub(crate) fn lock(&self) -> MutexGuard<'_, Option<MidiOutputConnection>> {
        // A poisoned lock only means another thread panicked mid-send; the
        // Option itself is still usable.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Finds the first output port containing "tonex" and connects to it.
    /// Returns the port name on success.
    pub(crate) fn try_connect(&self) -> Result<String, String> {
        let mut guard = self.lock();

        // Release any existing connection so the port is free to reopen.
        if let Some(old) = guard.take() {
            old.close();
        }

        // midir consumes MidiOutput on connect, so a fresh one is needed per attempt.
        let out = MidiOutput::new(CLIENT_NAME).map_err(|e| e.to_string())?;

        let (port, name) = out
            .ports()
            .into_iter()
            .find_map(|p| {
                let name = out.port_name(&p).ok()?;
                name.to_lowercase().contains("tonex").then_some((p, name))
            })
            .ok_or_else(|| "TONEX MIDI port not found".to_string())?;

        let conn = out
            .connect(&port, "tx-open-studio-out")
            .map_err(|e| e.to_string())?;

        *guard = Some(conn);
        Ok(name)
    }

    /// Sends the messages in order. On failure the connection is dropped so
    /// the UI can show "disconnected".
    pub(crate) fn send_all(&self, messages: &[&[u8]]) -> Result<(), String> {
        let mut guard = self.lock();

        let result = match guard.as_mut() {
            Some(conn) => messages
                .iter()
                .try_for_each(|m| conn.send(m))
                .map_err(|e| e.to_string()),
            None => Err("not connected".to_string()),
        };

        if result.is_err() {
            if let Some(conn) = guard.take() {
                conn.close();
            }
        }
        result
    }

    /// Drops the connection (pedal gone or reconnecting).
    pub(crate) fn close(&self) {
        if let Some(conn) = self.lock().take() {
            conn.close();
        }
    }

    /// Follows the pedal's MIDI channel (status nibble, see `channel_nibble`).
    pub(crate) fn set_channel(&self, nibble: u8) {
        self.channel.store(nibble & 0x0F, Ordering::Relaxed);
    }

    pub(crate) fn status(&self, kind: u8) -> u8 {
        status_byte(kind, self.channel.load(Ordering::Relaxed))
    }

    /// Sends a Control Change on the pedal's channel.
    pub(crate) fn send_cc(&self, cc: u8, value: u8) -> Result<(), String> {
        self.send_all(&[&[self.status(0xB0), cc & 0x7F, value & 0x7F]])
    }
}

/// Tuner on/off: CC9, 127 = on, 0 = off (verified on the
/// pedal). The pedal sends no tuner pitch data and doesn't report tuner
/// toggles made on the pedal itself, so the UI shows the state we set.
pub(crate) const TUNER_CC: u8 = 9;

pub(crate) fn tuner_cc_value(on: bool) -> u8 {
    if on {
        127
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tuner_cc_values() {
        assert_eq!(TUNER_CC, 9);
        assert_eq!(tuner_cc_value(true), 127);
        assert_eq!(tuner_cc_value(false), 0);
    }

    #[test]
    fn midi_channel_nibble_and_status() {
        assert_eq!(channel_nibble(1.0), Some(0));
        assert_eq!(channel_nibble(10.0), Some(9));
        assert_eq!(channel_nibble(16.0), Some(15));
        for bad in [0.0, 17.0, 1.5, -1.0, f32::NAN] {
            assert_eq!(channel_nibble(bad), None, "{bad}");
        }
        assert_eq!(status_byte(0xB0, 0), 0xB0);
        assert_eq!(status_byte(0xB0, 15), 0xBF);
        assert_eq!(status_byte(0xC0, 9), 0xC9);
        let engine = MidiEngine::default();
        assert_eq!(engine.status(0xC0), 0xC0, "channel 1 until globals are read");
        engine.set_channel(channel_nibble(16.0).unwrap());
        assert_eq!((engine.status(0xB0), engine.status(0xC0)), (0xBF, 0xCF));
    }
}
