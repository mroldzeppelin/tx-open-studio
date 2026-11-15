//! The GLOBAL tab's controls: global indices, ranges, rounding, UI setters and the per-index echo guard.

use crate::*;
use std::time::Instant;

/// A pedal-wide setting the GLOBAL tab edits: `id` is the `global-changed`
/// id in app.slint, `index` the global (PROTOCOL.md §4.5).
pub(crate) struct GlobalControl {
    pub(crate) id: i32,
    pub(crate) index: usize,
    pub(crate) range: (f32, f32),
    /// Rounded to tenths (levels in dB, tempo) instead of whole numbers
    /// (selectors, switches, Hz).
    pub(crate) tenths: bool,
}

impl GlobalControl {
    /// Value as written/shown: rounded and clamped to the range.
    pub(crate) fn sanitize(&self, v: f32) -> f32 {
        let v = if self.tenths { (v * 10.0).round() / 10.0 } else { v.round() };
        v.clamp(self.range.0, self.range.1)
    }
}

/// Interface (USB) volume range, -40 – +3 dB (pedal manual); default -10,
/// 1 dB steps.
pub(crate) const INTERFACE_VOLUME_RANGE: (f32, f32) = (-40.0, 3.0);

/// Tuner reference range as in IK's UIs (~427..453 Hz).
pub(crate) const TUNER_REF_RANGE: (f32, f32) = (427.0, 453.0);

pub(crate) const TEMPO_RANGE: (f32, f32) = (30.0, 300.0);

pub(crate) const GLOBAL_CONTROLS: [GlobalControl; 12] = {
    use protocol::*;
    pub(crate) const fn c(id: i32, index: usize, range: (f32, f32), tenths: bool) -> GlobalControl {
        GlobalControl { id, index, range, tenths }
    }
    [
        c(0, G_INPUT_TRIM, INPUT_TRIM_RANGE, true),
        c(1, G_MAIN_VOLUME, MAIN_VOLUME_RANGE, true),
        c(2, G_INTERFACE_VOLUME, INTERFACE_VOLUME_RANGE, false),
        c(3, G_TUNER_MODE, (0.0, 2.0), false),
        c(4, G_TUNER_REF, TUNER_REF_RANGE, false),
        c(5, G_TEMPO, TEMPO_RANGE, true),
        c(6, G_TEMPO_MODE, (0.0, 1.0), false),
        c(7, G_CAB_SIM, (0.0, 1.0), false),
        c(8, G_MIDI_CHANNEL, (1.0, 16.0), false),
        c(9, G_MIDI_THRU, (0.0, 1.0), false),
        c(10, G_MIDI_CLOCK, (0.0, 3.0), false),
        c(11, G_NAMING, (0.0, 2.0), false),
    ]
};

pub(crate) fn global_control(id: i32) -> Option<&'static GlobalControl> {
    GLOBAL_CONTROLS.iter().find(|c| c.id == id)
}

/// Shows global control `id`'s (sanitized) value in the GLOBAL tab.
pub(crate) fn show_global(ui: &AppWindow, id: i32, v: f32) {
    match id {
        0 => ui.set_global_trim(v),
        1 => ui.set_global_volume(v),
        2 => ui.set_global_interface(v),
        3 => ui.set_tuner_mode(v as i32),
        4 => ui.set_tuner_ref(v),
        5 => ui.set_tempo_bpm(v),
        6 => ui.set_tempo_mode(v as i32),
        7 => ui.set_cab_sim(v != 0.0),
        8 => ui.set_midi_channel_index(v as i32 - 1),
        9 => ui.set_midi_thru(v != 0.0),
        10 => ui.set_midi_clock(v as i32),
        11 => ui.set_naming(v as i32),
        _ => {}
    }
}

/// Echo guard per global index: after our own write, reads and pushes of
/// that global are ignored for `ECHO_GUARD`. Globals are pedal-wide, so
/// this is NOT cleared on preset change.
#[derive(Clone, Copy, Default)]
pub(crate) struct GlobalGuard([Option<Instant>; protocol::GLOBALS_COUNT]);

impl GlobalGuard {
    pub(crate) fn touch(&mut self, index: usize, now: Instant) {
        if let Some(t) = self.0.get_mut(index) {
            *t = Some(now);
        }
    }

    pub(crate) fn recent(&self, index: usize, now: Instant) -> bool {
        self.0.get(index).copied().flatten().is_some_and(|t| now.saturating_duration_since(t) < ECHO_GUARD)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn global_values_are_rounded_and_clamped() {
        let tempo = global_control(5).unwrap();
        assert_eq!(tempo.index, protocol::G_TEMPO);
        assert_eq!(tempo.sanitize(120.04), 120.0);
        assert_eq!(tempo.sanitize(162.25), 162.3);
        assert_eq!(tempo.sanitize(12.0), 30.0);
        assert_eq!(tempo.sanitize(450.0), 300.0);
        let channel = global_control(8).unwrap();
        assert_eq!(channel.sanitize(0.0), 1.0);
        assert_eq!(channel.sanitize(16.4), 16.0);
        assert_eq!(global_control(4).unwrap().sanitize(440.4), 440.0);
        assert_eq!(global_control(10).unwrap().sanitize(7.0), 3.0);
        assert_eq!(global_control(11).unwrap().index, protocol::G_NAMING);
        assert!(global_control(12).is_none());
        // Ids and indices are unique.
        for (i, a) in GLOBAL_CONTROLS.iter().enumerate() {
            for b in &GLOBAL_CONTROLS[i + 1..] {
                assert!(a.id != b.id && a.index != b.index);
            }
        }
    }

    #[test]
    fn global_guard_per_index() {
        let t0 = Instant::now();
        let mut g = GlobalGuard::default();
        assert!(!g.recent(protocol::G_TEMPO, t0));
        g.touch(protocol::G_TEMPO, t0);
        assert!(g.recent(protocol::G_TEMPO, t0 + Duration::from_millis(500)));
        assert!(!g.recent(protocol::G_TEMPO, t0 + ECHO_GUARD + Duration::from_millis(1)));
        assert!(!g.recent(protocol::G_MIDI_CHANNEL, t0));
        g.touch(99, t0); // out of range: ignored
        assert!(!g.recent(99, t0));
    }
}
