//! The mirror of the current preset's parameter array the cards display: plausibility checks per section, echo handling of our own writes.

use super::*;
use std::time::Instant;
use crate::ECHO_GUARD;

pub(crate) fn is_switch(v: f32) -> bool {
    v == 0.0 || v == 1.0
}

pub(crate) fn is_index(v: f32, count: usize) -> bool {
    v.fract() == 0.0 && (0.0..count as f32).contains(&v)
}

/// Loose plausibility checks per card section of a raw parameter array.
/// Only switches and selectors are checked (continuous values are clamped
/// for display), so valid presets aren't rejected over range details.
pub(crate) fn delay_section_ok(raw: &[f32]) -> bool {
    if raw.len() < delay_range().end {
        return false;
    }
    is_switch(raw[P_DELAY_POST])
        && is_switch(raw[P_DELAY_ENABLE])
        && is_index(raw[P_DELAY_MODEL], DELAY_MODELS)
        && (0..DELAY_MODELS).all(|m| {
            let b = P_DELAY_FIRST_BLOCK + m * DELAY_BLOCK_LEN;
            is_switch(raw[b]) && is_index(raw[b + 1], DIVISIONS) && is_switch(raw[b + 4])
        })
}

pub(crate) fn reverb_section_ok(raw: &[f32]) -> bool {
    raw.len() >= reverb_range().end
        && is_switch(raw[P_REVERB_POSITION])
        && is_switch(raw[P_REVERB_ENABLE])
        && is_index(raw[P_REVERB_MODEL], REVERB_MODELS)
}

pub(crate) fn comp_section_ok(raw: &[f32]) -> bool {
    raw.len() >= comp_range().end && is_switch(raw[P_COMP_POST]) && is_switch(raw[P_COMP_ENABLE])
}

pub(crate) fn gate_section_ok(raw: &[f32]) -> bool {
    raw.len() >= gate_range().end && is_switch(raw[P_GATE_POST]) && is_switch(raw[P_GATE_ENABLE])
}

pub(crate) fn cab_section_ok(raw: &[f32]) -> bool {
    raw.len() >= cab_range().end
        && is_index(raw[P_CAB_TYPE], CAB_TYPES)
        && is_index(raw[P_CAB_MODEL], CAB_MODELS)
        && is_index(raw[P_CAB_MIC1], MIC_TYPES)
        && is_index(raw[P_CAB_MIC2], MIC_TYPES)
}

/// EQ frequencies: each within its Hz range (unlike the switch-only checks
/// above, these values are the only evidence the layout is right).
pub(crate) fn eq_section_ok(raw: &[f32]) -> bool {
    EQ_FREQS.iter().all(|&(i, (min, max))| raw.get(i).is_some_and(|v| (min..=max).contains(v)))
}

pub(crate) fn mod_section_ok(raw: &[f32]) -> bool {
    raw.len() >= MOD_END
        && is_switch(raw[P_MOD_POST])
        && is_switch(raw[P_MOD_ENABLE])
        && is_index(raw[P_MOD_MODEL], MOD_MODELS)
        && MOD_BLOCKS.iter().all(|&(b, _)| is_switch(raw[b]) && is_index(raw[b + 1], DIVISIONS))
}

/// The current preset's parameter values as last known (read, pushed by the
/// pedal, or written by us), and when we last wrote each one.
pub struct FxMirror {
    values: [Option<f32>; PARAM_COUNT],
    touched: [Option<Instant>; PARAM_COUNT],
}

impl Default for FxMirror {
    fn default() -> Self {
        Self { values: [None; PARAM_COUNT], touched: [None; PARAM_COUNT] }
    }
}

impl FxMirror {
    pub(crate) fn get(&self, index: usize) -> Option<f32> {
        self.values.get(index).copied().flatten()
    }

    pub(crate) fn recent(&self, index: usize) -> bool {
        self.touched.get(index).copied().flatten().is_some_and(|t| t.elapsed() < ECHO_GUARD)
    }

    /// Takes the effect sections of a freshly read parameter array, each
    /// only if it passes its plausibility check. Values we changed within
    /// the echo guard are kept. Returns which cards changed.
    pub fn load(&mut self, raw: &[f32]) -> Vec<Card> {
        let mut cards = Vec::new();
        let sections = [
            (Card::Delay, delay_section_ok(raw), delay_range()),
            (Card::Reverb, reverb_section_ok(raw), reverb_range()),
            (Card::Comp, comp_section_ok(raw), comp_range()),
            (Card::Mod, mod_section_ok(raw), mod_range()),
            (Card::Gate, gate_section_ok(raw), gate_range()),
            (Card::Cab, cab_section_ok(raw), cab_range()),
        ];
        for (card, ok, range) in sections {
            if !ok {
                if !raw.is_empty() {
                    eprintln!("TONEX: {card:?} params look implausible, not shown");
                }
                continue;
            }
            for i in range {
                if !self.recent(i) {
                    self.values[i] = Some(raw[i]);
                }
            }
            cards.push(card);
        }
        if eq_section_ok(raw) {
            for (i, _) in EQ_FREQS {
                if !self.recent(i) {
                    self.values[i] = Some(raw[i]);
                }
            }
            cards.push(Card::Eq);
        } else if !raw.is_empty() {
            eprintln!("TONEX: EQ frequencies look implausible, not shown");
        }
        for (card, i, (min, max), _) in SINGLES {
            match raw.get(i) {
                // A switch must be exactly 0 or 1.
                Some(&v) if (min..=max).contains(&v) && (card != Card::EqPost || is_switch(v)) => {
                    if !self.recent(i) {
                        self.values[i] = Some(v);
                    }
                    cards.push(card);
                }
                Some(v) => eprintln!("TONEX: {card:?} value {v} outside {min}..{max}, not shown"),
                None => {}
            }
        }
        cards
    }

    /// A single change reported by the pedal. Returns the card to refresh,
    /// or None if it isn't ours or is an echo of our own recent write.
    pub fn pedal_changed(&mut self, index: usize, value: f32) -> Option<Card> {
        let card = card_of(index)?;
        if self.recent(index) || !value.is_finite() {
            return None;
        }
        self.values[index] = Some(value);
        Some(card)
    }

    /// Records our own write.
    pub fn wrote(&mut self, index: usize, value: f32) {
        if index < PARAM_COUNT {
            self.values[index] = Some(value);
            self.touched[index] = Some(Instant::now());
        }
    }
}

pub(crate) fn model_of(v: Option<f32>, count: usize) -> Option<usize> {
    v.filter(|&v| is_index(v, count)).map(|v| v as usize)
}

/// Rounds to `step` and clamps to `min..=max`.
pub(crate) fn snap(v: f32, (min, max, step): (f32, f32, f32)) -> f32 {
    ((v / step).round() * step).clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plausible_raw() -> Vec<f32> {
        let mut raw = vec![0.0f32; PARAM_COUNT];
        raw[P_REVERB_MODEL] = 5.0;
        raw[P_DELAY_MODEL] = 1.0;
        raw[P_DELAY_FIRST_BLOCK + 1] = 9.0; // 1/4
        raw[P_DELAY_FIRST_BLOCK + DELAY_BLOCK_LEN + 2] = 320.0;
        raw[P_COMP_POST] = 1.0;
        raw[P_MOD_MODEL] = 4.0;
        raw[MOD_BLOCKS[4].0 + 1] = 17.0;
        raw[P_GATE_ENABLE] = 1.0;
        raw[P_CAB_TYPE] = 1.0;
        raw[P_CAB_MODEL] = 25.0; // seen on hardware
        raw[P_CAB_MIC2] = 2.0;
        raw
    }

    #[test]
    fn loads_plausible_sections() {
        let mut m = FxMirror::default();
        assert_eq!(
            m.load(&plausible_raw()),
            [Card::Delay, Card::Reverb, Card::Comp, Card::Mod, Card::Gate, Card::Cab, Card::Mix, Card::EqPost]
        );
        let idx = FxControl::DelayTime.param_index(Models { delay: 1, ..Models::default() }).unwrap();
        assert_eq!(m.get(idx), Some(320.0));
    }

    #[test]
    fn rejects_implausible_or_short_sections() {
        let mut raw = plausible_raw();
        raw[P_DELAY_MODEL] = 2.5;
        let mut m = FxMirror::default();
        raw[MOD_BLOCKS[2].0] = 0.5;
        raw[P_CAB_MODEL] = 2.5; // not a whole number
        assert_eq!(m.load(&raw), [Card::Reverb, Card::Comp, Card::Gate, Card::Mix, Card::EqPost]);
        assert_eq!(m.load(&raw[..80]), [Card::Reverb, Card::Comp, Card::Gate, Card::Mix, Card::EqPost]);
        assert!(m.load(&[]).is_empty());
    }

    #[test]
    fn eq_frequencies_load_only_when_plausible() {
        let mut raw = plausible_raw();
        raw[P_EQ_BASS_FREQ] = 300.0;
        raw[P_EQ_MID_FREQ] = 750.0;
        raw[P_EQ_TREBLE_FREQ] = 2000.0;
        let mut m = FxMirror::default();
        assert!(m.load(&raw).contains(&Card::Eq));
        assert_eq!(m.get(P_EQ_MID_FREQ), Some(750.0));
        // Our write wins over a stale read within the echo guard.
        m.wrote(P_EQ_MID_FREQ, 900.0);
        m.load(&raw);
        assert_eq!(m.get(P_EQ_MID_FREQ), Some(900.0));

        raw[P_EQ_TREBLE_FREQ] = 1.0; // shifted layout / junk
        assert!(!FxMirror::default().load(&raw).contains(&Card::Eq));
        assert!(!FxMirror::default().load(&raw[..16]).contains(&Card::Eq));
    }

    #[test]
    fn amp_mix_has_its_own_gate() {
        let mut raw = plausible_raw();
        raw[P_AMP_MIX] = 100.0; // preset 148
        let mut m = FxMirror::default();
        let cards = m.load(&raw);
        // EQ frequencies are 0 here (implausible), mix still loads.
        assert!(cards.contains(&Card::Mix) && !cards.contains(&Card::Eq));
        assert_eq!(m.get(P_AMP_MIX), Some(100.0));
        for bad in [-1.0, 100.5, f32::NAN] {
            raw[P_AMP_MIX] = bad;
            assert!(!FxMirror::default().load(&raw).contains(&Card::Mix), "{bad}");
        }
        assert!(!FxMirror::default().load(&raw[..22]).contains(&Card::Mix));
    }

    #[test]
    fn mid_q_and_eq_position_gates() {
        let mut raw = plausible_raw();
        raw[P_EQ_MID_Q] = 0.7; // preset 148
        raw[P_EQ_POST] = 1.0;
        let mut m = FxMirror::default();
        let cards = m.load(&raw);
        assert!(cards.contains(&Card::MidQ) && cards.contains(&Card::EqPost));
        assert_eq!((m.get(P_EQ_MID_Q), m.get(P_EQ_POST)), (Some(0.7), Some(1.0)));
        // Outside the pedal's Q range, or a non-switch position: not
        // shown, so the controls stay locked.
        for (i, bad) in [(P_EQ_MID_Q, 0.1), (P_EQ_MID_Q, 3.5), (P_EQ_POST, 0.5), (P_EQ_POST, 2.0)] {
            let mut r = raw.clone();
            r[i] = bad;
            assert!(!FxMirror::default().load(&r).contains(&card_of(i).unwrap()), "{i}={bad}");
        }
        assert_eq!(snap(0.72, (EQ_MID_Q_RANGE.0, EQ_MID_Q_RANGE.1, 0.05)), 0.7);
    }

    #[test]
    fn freq_steps() {
        assert_eq!(freq_step(75.0), 5.0);
        assert_eq!(freq_step(995.0), 5.0);
        assert_eq!(freq_step(1000.0), 50.0);
        assert_eq!(snap(1623.0, (1000.0, 4000.0, freq_step(1623.0))), 1600.0);
        assert_eq!(snap(312.4, (75.0, 600.0, freq_step(312.4))), 310.0);
    }

    /// params[0..40] of preset 148 dumped from the user's pedal.
    #[test]
    fn accepts_hardware_dump_preset_148() {
        let dump = [
            0.0, 0.0, -100.0, 20.0, -60.0, 1.0, 0.0, 0.0, -8.0, 5.0, 1.0, 4.3, 300.0, 8.1, 0.7, 750.0, 2.6, 2000.0,
            1.0, 0.0, 5.5, 4.2, 100.0, 1.0, 0.0, 25.0, 5.0, 0.0, 1.5, 0.0, 1.0, 1.5, 0.0, 0.0, 0.0, 8.4, 1.0, 1.0,
            5.0, 5.0,
        ];
        let mut raw = plausible_raw();
        raw[..dump.len()].copy_from_slice(&dump);
        let mut m = FxMirror::default();
        let cards = m.load(&raw);
        assert!(cards.contains(&Card::Gate) && cards.contains(&Card::Cab) && cards.contains(&Card::Comp));
        let at = |c: FxControl| m.get(c.param_index(Models::default()).unwrap());
        assert_eq!(at(FxControl::CabModel), Some(25.0));
        assert_eq!(at(FxControl::GateThreshold), Some(-100.0));
        // Comp: 7 threshold, 8 gain, 9 attack.
        assert_eq!(at(FxControl::CompThreshold), Some(0.0));
        assert_eq!(at(FxControl::CompGain), Some(-8.0));
        assert_eq!(at(FxControl::CompAttack), Some(5.0));
    }

    #[test]
    fn own_writes_mask_echoes() {
        let mut m = FxMirror::default();
        m.wrote(99, 300.0);
        assert_eq!(m.pedal_changed(99, 250.0), None);
        assert_eq!(m.get(99), Some(300.0));
        assert_eq!(m.pedal_changed(100, 40.0), Some(Card::Delay));
        assert_eq!(m.pedal_changed(20, 5.0), None); // amp param: not ours
    }
}
