//! Loaded vs current preset values: the EDITED state.

use super::*;

/// Compares the current preset's parameter values with the values it was
/// loaded with, for the "Changed" badge (strict: turning a control back to
/// its loaded value clears it).
///
/// The baseline is taken from the first full read after a preset load
/// (startup, preset change, footswitch push, Reconnect); later reads of the
/// same preset only refresh `current`.
#[derive(Default)]
pub struct PresetDiff {
    baseline: Vec<f32>,
    current: Vec<f32>,
    /// The pedal reports unsaved edits after a model/cab load or on-pedal
    /// browsing (its own dirty flag; slots are not in the arrays).
    slot_edited: bool,
}

/// Largest difference treated as "unchanged" for parameter `index`: half the
/// UI step for the per-model delay/reverb controls, and a tiny relative
/// tolerance for everything else (amp knobs too: written over serial as
/// exact floats, they come back exactly).
pub(crate) fn tolerance(index: usize, baseline: f32) -> f32 {
    use FxControl::*;
    let per_model = |c: FxControl| {
        (0..DELAY_MODELS).any(|delay| {
            (0..REVERB_MODELS).any(|reverb| c.param_index(Models { delay, reverb, modulation: 0 }) == Some(index))
        })
    };
    if [ReverbTime, ReverbColor].into_iter().any(per_model) {
        0.05
    } else if [DelayTime, DelayFeedback, DelayMix, ReverbPredelay, ReverbMix].into_iter().any(per_model) {
        0.5
    } else {
        1e-3 * baseline.abs().max(1.0)
    }
}

impl PresetDiff {
    /// A full parameter array read from the pedal. Becomes the baseline if
    /// none is set yet (preset just loaded), else only updates `current`.
    pub fn load(&mut self, raw: &[f32]) {
        if raw.is_empty() {
            return;
        }
        if self.baseline.is_empty() {
            self.baseline = raw.to_vec();
        }
        self.current = raw.to_vec();
    }

    /// After a verified save the current values are the preset's stored
    /// state: they become the baseline, as after a preset load.
    pub fn rebaseline(&mut self) {
        self.baseline = self.current.clone();
        self.slot_edited = false;
    }

    /// The pedal's dirty flag read after a model/cab load or browsing.
    pub fn set_slot_edited(&mut self, edited: bool) {
        self.slot_edited = edited;
    }

    /// Value of parameter `index` as loaded with the preset (None before
    /// the first read); double-click resets of the EQ frequencies use it.
    pub fn baseline(&self, index: usize) -> Option<f32> {
        self.baseline.get(index).copied()
    }

    /// What makes the preset dirty, for the debug log: the first parameter
    /// whose current value differs from the baseline.
    pub fn first_difference(&self) -> Option<String> {
        if self.slot_edited {
            return Some("model/cab slot (pedal dirty flag)".into());
        }
        self.baseline
            .iter()
            .zip(&self.current)
            .enumerate()
            .find(|&(i, (&b, &c))| (c - b).abs() > tolerance(i, b))
            .map(|(i, (b, c))| format!("param {i}: loaded {b}, now {c}"))
    }

    /// One parameter changed (our write or a pedal report).
    pub fn set(&mut self, index: usize, value: f32) {
        if let Some(v) = self.current.get_mut(index) {
            *v = value;
        }
    }

    pub fn dirty(&self) -> bool {
        self.slot_edited
            || self
                .baseline
            .iter()
            .zip(&self.current)
            .enumerate()
            .any(|(i, (&b, &c))| (c - b).abs() > tolerance(i, b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::AmpKnob;

    fn base() -> Vec<f32> {
        let mut raw = vec![0.0f32; PARAM_COUNT];
        raw[AmpKnob::Gain.param_index()] = 6.9;
        raw[FxControl::DelayTime.param_index(Models::default()).unwrap()] = 320.0;
        raw[12] = 300.0; // EQ bass freq, not shown in the app
        raw
    }

    #[test]
    fn rebaseline_after_save() {
        let mut d = PresetDiff::default();
        d.load(&base());
        let gain = AmpKnob::Gain.param_index();
        d.set(gain, 8.0);
        assert!(d.dirty());
        // A save makes the current values the new baseline.
        d.rebaseline();
        assert!(!d.dirty());
        assert_eq!(d.baseline(gain), Some(8.0));
    }

    #[test]
    fn clean_after_load_and_after_return_to_baseline() {
        let mut d = PresetDiff::default();
        assert!(!d.dirty());
        d.load(&base());
        assert!(!d.dirty());
        let gain = AmpKnob::Gain.param_index();
        d.set(gain, 7.5);
        assert!(d.dirty());
        assert_eq!(d.first_difference().unwrap(), "param 20: loaded 6.9, now 7.5");
        // Back to exactly 6.9 (serial writes are exact floats).
        d.set(gain, 6.9);
        assert!(!d.dirty());
        // No CC-sized allowance any more: 6.93 is a change.
        d.set(gain, 6.93);
        assert!(d.dirty());
    }

    #[test]
    fn epsilon_per_param() {
        let mut d = PresetDiff::default();
        d.load(&base());
        let time = FxControl::DelayTime.param_index(Models::default()).unwrap();
        d.set(time, 320.3); // within half a ms step
        assert!(!d.dirty());
        d.set(time, 321.0);
        assert!(d.dirty());
        d.set(time, 320.0);
        d.set(12, 300.0001); // float noise on a hidden param
        assert!(!d.dirty());
        d.set(12, 310.0);
        assert!(d.dirty());
    }

    #[test]
    fn slot_edit_is_dirty_until_saved() {
        let mut d = PresetDiff::default();
        d.load(&base());
        d.set_slot_edited(true);
        assert!(d.dirty());
        assert!(d.first_difference().unwrap().contains("slot"));
        d.rebaseline();
        assert!(!d.dirty());
    }

    #[test]
    fn reread_keeps_baseline() {
        let mut d = PresetDiff::default();
        d.load(&base());
        let mut edited = base();
        edited[FxControl::DelayMix.param_index(Models::default()).unwrap()] = 40.0;
        d.load(&edited);
        assert!(d.dirty());
        d.load(&base());
        assert!(!d.dirty());
    }
}
