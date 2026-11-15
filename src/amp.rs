//! Amp-tab bookkeeping: knob ids and values, echo guards, the EDITED updates.

use crate::*;
use std::time::Duration;
use std::time::Instant;

/// An amp knob value as written: the UI's 0.1 steps, 0..10.
pub(crate) fn amp_knob_value(v: f32) -> f32 {
    ((v * 10.0).round() / 10.0).clamp(0.0, 10.0)
}

/// UI knob id (`amp-param-changed` in app.slint) → knob.
pub(crate) fn knob_from_id(id: i32) -> Option<AmpKnob> {
    usize::try_from(id).ok().and_then(|i| AmpKnob::ALL.get(i).copied())
}

/// After we change a value, pedal reports for it are ignored this long, so
/// late echoes of our own changes don't yank the control back.
pub(crate) const ECHO_GUARD: Duration = Duration::from_millis(600);

/// Bookkeeping for two-way amp sync: when we last changed each control.
#[derive(Default)]
pub(crate) struct AmpSync {
    pub(crate) knob_touched: [Option<Instant>; 7],
    pub(crate) enable_touched: Option<Instant>,
    /// Delay/reverb parameter mirror (see fx.rs).
    pub(crate) fx: fx::FxMirror,
    /// Loaded vs current preset values, for the "Changed" badge.
    pub(crate) diff: fx::PresetDiff,
}

impl AmpSync {
    /// Forget what the pedal holds (preset change, reconnect).
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn knob_recent(&self, knob: AmpKnob) -> bool {
        self.knob_touched[knob as usize].is_some_and(|t| t.elapsed() < ECHO_GUARD)
    }

    pub(crate) fn enable_recent(&self) -> bool {
        self.enable_touched.is_some_and(|t| t.elapsed() < ECHO_GUARD)
    }
}

pub(crate) fn set_knob(ui: &AppWindow, knob: AmpKnob, v: f32) {
    match knob {
        AmpKnob::Gain => ui.set_amp_gain(v),
        AmpKnob::Volume => ui.set_amp_volume(v),
        AmpKnob::Presence => ui.set_amp_presence(v),
        AmpKnob::Depth => ui.set_amp_depth(v),
        AmpKnob::Bass => ui.set_amp_bass(v),
        AmpKnob::Mid => ui.set_amp_mid(v),
        AmpKnob::Treble => ui.set_amp_treble(v),
    }
}

/// Records a new value of preset parameter `index` and updates the
/// "Changed" badge.
///
/// The badge compares the current values with the ones the preset was
/// loaded with (`fx::PresetDiff`), so it goes off again when a control is
/// turned back. Current values come from our own writes (at send time) and
/// from every pedal report, echoes included. Globals are not part of the
/// array. The baseline is reset on preset load (`AmpSync::clear`, footswitch
/// push) and taken from the next read. Limit: edits made on the pedal before
/// that read (e.g. while the app wasn't running) become part of the baseline.
pub(crate) fn record_value(ui: &AppWindow, amp: &mut AmpSync, index: usize, value: f32, cause: &str) {
    amp.diff.set(index, value);
    update_dirty(ui, amp, cause);
}

/// Shows the EDITED state. With `TX_DEBUG_SERIAL=1`, logs what turned it on
/// (the event `cause` and the first differing value).
pub(crate) fn update_dirty(ui: &AppWindow, amp: &AmpSync, cause: &str) {
    let dirty = amp.diff.dirty();
    if dirty && !ui.get_preset_dirty() && protocol::debug_enabled() {
        let what = amp.diff.first_difference().unwrap_or_else(|| "?".into());
        eprintln!("TONEX: EDITED by {cause}: {what}");
    }
    ui.set_preset_dirty(dirty);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amp_knob_writes_go_to_their_param_indices() {
        // UI ids 0..6: gain, volume, presence, depth, bass, mid, treble.
        let indices: Vec<usize> = (0..7).filter_map(knob_from_id).map(|k| k.param_index()).collect();
        assert_eq!(indices, [20, 21, 34, 35, 11, 13, 16]);
        assert_eq!(knob_from_id(7), None);
        assert_eq!(knob_from_id(-1), None);
        // Written as the UI's 0.1 steps, clamped to 0..10.
        assert_eq!(amp_knob_value(6.94), 6.9);
        assert_eq!(amp_knob_value(-1.0), 0.0);
        assert_eq!(amp_knob_value(10.4), 10.0);
    }
}
