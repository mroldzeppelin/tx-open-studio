//! Delay and Reverb cards: control ids, the pedal's parameter indices, and a
//! mirror of the current preset's parameter array that the cards display.
//!
//! Every delay/reverb control is written over USB serial (`0x0207`
//! single-parameter write, confirmed on hardware for amp on/off) rather than
//! MIDI CC: serial addresses the exact per-model parameter with its native
//! value (ms, %, index), while the CC numbers/scaling for most of these
//! controls are undocumented. Indices and ranges: Builty/TonexOneController
//! `tonex_params.h` / `tonex_params.c`; option lists: its `web/index.html`.
//!
//! The pedal stores time/feedback/mode/mix *per model* (Digital and Tape
//! each keep their own), and reverb time/predelay/color/mix per model too,
//! so the index of those controls depends on the selected model. Switching
//! model re-displays that model's stored values from the mirror.

use std::time::Instant;

use crate::tonex_serial::{
    DELAY_BLOCK_LEN, DELAY_MODELS, PARAM_COUNT, P_DELAY_ENABLE, P_DELAY_FIRST_BLOCK, P_DELAY_MODEL, P_DELAY_POST,
    P_REVERB_ENABLE, P_REVERB_FIRST_BLOCK, P_REVERB_MODEL, P_REVERB_POSITION, REVERB_BLOCK_LEN, REVERB_MODELS,
};
use crate::{AppWindow, ECHO_GUARD};

/// Number of delay time-signature options (`dly-division` 0..=17:
/// 1/32, 1/32 D, 1/32 T, 1/16 … 1/1 T).
const DELAY_DIVISIONS: usize = 18;
const DELAY_TIME_MAX: f32 = 1000.0;

/// A delay/reverb UI control; the id is the `fx-changed` id in app.slint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FxControl {
    DelayEnable,
    DelayPost,
    DelayModel,
    DelaySync,
    DelayDivision,
    DelayTime,
    DelayFeedback,
    DelayPingPong,
    DelayMix,
    ReverbEnable,
    ReverbPost,
    ReverbModel,
    ReverbTime,
    ReverbPredelay,
    ReverbColor,
    ReverbMix,
}

/// Which card a parameter belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Card {
    Delay,
    Reverb,
}

impl FxControl {
    pub fn from_id(id: i32) -> Option<Self> {
        use FxControl::*;
        Some(match id {
            0 => DelayEnable,
            1 => DelayPost,
            2 => DelayModel,
            3 => DelaySync,
            4 => DelayDivision,
            5 => DelayTime,
            6 => DelayFeedback,
            7 => DelayPingPong,
            8 => DelayMix,
            10 => ReverbEnable,
            11 => ReverbPost,
            12 => ReverbModel,
            13 => ReverbTime,
            14 => ReverbPredelay,
            15 => ReverbColor,
            16 => ReverbMix,
            _ => return None,
        })
    }

    /// Parameter index on the pedal; per-model controls use the given
    /// (0-based) delay / reverb model.
    pub fn param_index(self, delay_model: usize, reverb_model: usize) -> usize {
        use FxControl::*;
        let dly = P_DELAY_FIRST_BLOCK + delay_model.min(DELAY_MODELS - 1) * DELAY_BLOCK_LEN;
        let rvb = P_REVERB_FIRST_BLOCK + reverb_model.min(REVERB_MODELS - 1) * REVERB_BLOCK_LEN;
        match self {
            DelayPost => P_DELAY_POST,
            DelayEnable => P_DELAY_ENABLE,
            DelayModel => P_DELAY_MODEL,
            DelaySync => dly,
            DelayDivision => dly + 1,
            DelayTime => dly + 2,
            DelayFeedback => dly + 3,
            DelayPingPong => dly + 4,
            DelayMix => dly + 5,
            ReverbPost => P_REVERB_POSITION,
            ReverbEnable => P_REVERB_ENABLE,
            ReverbModel => P_REVERB_MODEL,
            ReverbTime => rvb,
            ReverbPredelay => rvb + 1,
            ReverbColor => rvb + 2,
            ReverbMix => rvb + 3,
        }
    }

    pub fn card(self) -> Card {
        if (self as usize) <= FxControl::DelayMix as usize {
            Card::Delay
        } else {
            Card::Reverb
        }
    }

    /// Selecting a model changes which per-model values are shown.
    pub fn is_model(self) -> bool {
        matches!(self, FxControl::DelayModel | FxControl::ReverbModel)
    }
}

/// Card owning parameter `index`, if any.
pub fn card_of(index: usize) -> Option<Card> {
    let delay = P_DELAY_POST..P_DELAY_FIRST_BLOCK + DELAY_MODELS * DELAY_BLOCK_LEN;
    let reverb = P_REVERB_POSITION..P_REVERB_FIRST_BLOCK + REVERB_MODELS * REVERB_BLOCK_LEN;
    if delay.contains(&index) {
        Some(Card::Delay)
    } else if reverb.contains(&index) {
        Some(Card::Reverb)
    } else {
        None
    }
}

fn is_switch(v: f32) -> bool {
    v == 0.0 || v == 1.0
}

fn is_index(v: f32, count: usize) -> bool {
    v.fract() == 0.0 && (0.0..count as f32).contains(&v)
}

/// Loose plausibility check of the delay section of a raw parameter array.
/// Only switches and selectors are checked (continuous values are clamped
/// for display), so valid presets aren't rejected over range details.
fn delay_section_ok(raw: &[f32]) -> bool {
    let end = P_DELAY_FIRST_BLOCK + DELAY_MODELS * DELAY_BLOCK_LEN;
    if raw.len() < end {
        return false;
    }
    is_switch(raw[P_DELAY_POST])
        && is_switch(raw[P_DELAY_ENABLE])
        && is_index(raw[P_DELAY_MODEL], DELAY_MODELS)
        && (0..DELAY_MODELS).all(|m| {
            let b = P_DELAY_FIRST_BLOCK + m * DELAY_BLOCK_LEN;
            is_switch(raw[b]) && is_index(raw[b + 1], DELAY_DIVISIONS) && is_switch(raw[b + 4])
        })
}

fn reverb_section_ok(raw: &[f32]) -> bool {
    let end = P_REVERB_FIRST_BLOCK + REVERB_MODELS * REVERB_BLOCK_LEN;
    raw.len() >= end
        && is_switch(raw[P_REVERB_POSITION])
        && is_switch(raw[P_REVERB_ENABLE])
        && is_index(raw[P_REVERB_MODEL], REVERB_MODELS)
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
    fn get(&self, index: usize) -> Option<f32> {
        self.values.get(index).copied().flatten()
    }

    fn recent(&self, index: usize) -> bool {
        self.touched.get(index).copied().flatten().is_some_and(|t| t.elapsed() < ECHO_GUARD)
    }

    /// Takes the delay/reverb sections of a freshly read parameter array,
    /// each only if it passes its plausibility check. Values we changed
    /// within the echo guard are kept. Returns which cards changed.
    pub fn load(&mut self, raw: &[f32]) -> Vec<Card> {
        let mut cards = Vec::new();
        let sections = [
            (Card::Delay, delay_section_ok(raw), P_DELAY_POST..P_DELAY_FIRST_BLOCK + DELAY_MODELS * DELAY_BLOCK_LEN),
            (
                Card::Reverb,
                reverb_section_ok(raw),
                P_REVERB_POSITION..P_REVERB_FIRST_BLOCK + REVERB_MODELS * REVERB_BLOCK_LEN,
            ),
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

fn model_of(v: Option<f32>, count: usize) -> Option<usize> {
    v.filter(|&v| is_index(v, count)).map(|v| v as usize)
}

/// Pushes the mirror's values for `card` into the UI (unknown ones are left
/// as they are).
pub fn refresh(ui: &AppWindow, m: &FxMirror, card: Card) {
    match card {
        Card::Delay => {
            if let Some(v) = m.get(P_DELAY_ENABLE) {
                ui.set_dly_enabled(v != 0.0);
            }
            if let Some(v) = m.get(P_DELAY_POST) {
                ui.set_dly_post(v != 0.0);
            }
            if let Some(model) = model_of(m.get(P_DELAY_MODEL), DELAY_MODELS) {
                ui.set_dly_model(model as i32);
            }
            let model = usize::try_from(ui.get_dly_model()).unwrap_or(0);
            let idx = |c: FxControl| c.param_index(model, 0);
            if let Some(v) = m.get(idx(FxControl::DelaySync)) {
                ui.set_dly_sync(v != 0.0);
            }
            if let Some(d) = model_of(m.get(idx(FxControl::DelayDivision)), DELAY_DIVISIONS) {
                ui.set_dly_division(d as i32);
            }
            if let Some(v) = m.get(idx(FxControl::DelayTime)) {
                ui.set_dly_time(v.round().clamp(0.0, DELAY_TIME_MAX));
            }
            if let Some(v) = m.get(idx(FxControl::DelayFeedback)) {
                ui.set_dly_feedback(v.round().clamp(0.0, 100.0));
            }
            if let Some(v) = m.get(idx(FxControl::DelayPingPong)) {
                ui.set_dly_pingpong(v != 0.0);
            }
            if let Some(v) = m.get(idx(FxControl::DelayMix)) {
                ui.set_dly_mix(v.round().clamp(0.0, 100.0));
            }
        }
        Card::Reverb => {
            if let Some(v) = m.get(P_REVERB_ENABLE) {
                ui.set_rvb_enabled(v != 0.0);
            }
            if let Some(v) = m.get(P_REVERB_POSITION) {
                ui.set_rvb_post(v != 0.0);
            }
            if let Some(model) = model_of(m.get(P_REVERB_MODEL), REVERB_MODELS) {
                ui.set_rvb_model(model as i32);
            }
            let model = usize::try_from(ui.get_rvb_model()).unwrap_or(0);
            let idx = |c: FxControl| c.param_index(0, model);
            let tenth = |v: f32| (v * 10.0).round() / 10.0;
            if let Some(v) = m.get(idx(FxControl::ReverbTime)) {
                ui.set_rvb_time(tenth(v).clamp(0.0, 10.0));
            }
            if let Some(v) = m.get(idx(FxControl::ReverbPredelay)) {
                ui.set_rvb_predelay(v.round().clamp(0.0, 500.0));
            }
            if let Some(v) = m.get(idx(FxControl::ReverbColor)) {
                ui.set_rvb_color(tenth(v).clamp(-10.0, 10.0));
            }
            if let Some(v) = m.get(idx(FxControl::ReverbMix)) {
                ui.set_rvb_mix(v.round().clamp(0.0, 100.0));
            }
        }
    }
}

/// Parameter index for a UI change, using the models the UI shows.
pub fn index_for(ui: &AppWindow, control: FxControl) -> usize {
    let dly = usize::try_from(ui.get_dly_model()).unwrap_or(0);
    let rvb = usize::try_from(ui.get_rvb_model()).unwrap_or(0);
    control.param_index(dly, rvb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indices_follow_builty_enum() {
        use FxControl::*;
        // Digital block 97..=102, Tape block 103..=108.
        assert_eq!(DelayTime.param_index(0, 0), 99);
        assert_eq!(DelayMix.param_index(1, 0), 108);
        assert_eq!(DelaySync.param_index(1, 0), 103);
        // Spring 1 block 39..=42, Plate (model 5) block 59..=62.
        assert_eq!(ReverbTime.param_index(0, 0), 39);
        assert_eq!(ReverbMix.param_index(0, 5), 62);
        assert_eq!(ReverbColor.param_index(0, 4), 57);
        assert_eq!(card_of(108), Some(Card::Delay));
        assert_eq!(card_of(36), Some(Card::Reverb));
        assert_eq!(card_of(63), None);
        assert_eq!(card_of(20), None);
    }

    #[test]
    fn ids_roundtrip_and_cards() {
        for id in (0..=8).chain(10..=16) {
            let c = FxControl::from_id(id).unwrap();
            assert_eq!(c.card(), if id <= 8 { Card::Delay } else { Card::Reverb });
        }
        assert_eq!(FxControl::from_id(9), None);
        assert_eq!(FxControl::from_id(17), None);
    }

    fn plausible_raw() -> Vec<f32> {
        let mut raw = vec![0.0f32; PARAM_COUNT];
        raw[P_REVERB_MODEL] = 5.0;
        raw[P_DELAY_MODEL] = 1.0;
        raw[P_DELAY_FIRST_BLOCK + 1] = 9.0; // 1/4
        raw[P_DELAY_FIRST_BLOCK + DELAY_BLOCK_LEN + 2] = 320.0;
        raw
    }

    #[test]
    fn loads_plausible_sections() {
        let mut m = FxMirror::default();
        assert_eq!(m.load(&plausible_raw()), [Card::Delay, Card::Reverb]);
        assert_eq!(m.get(FxControl::DelayTime.param_index(1, 0)), Some(320.0));
    }

    #[test]
    fn rejects_implausible_or_short_sections() {
        let mut raw = plausible_raw();
        raw[P_DELAY_MODEL] = 2.5;
        let mut m = FxMirror::default();
        assert_eq!(m.load(&raw), [Card::Reverb]);
        assert_eq!(m.load(&raw[..80]), [Card::Reverb]);
        assert!(m.load(&[]).is_empty());
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
}

/// Largest difference treated as "unchanged" for parameter `index`: half the
/// UI step for displayed controls (amp knobs also absorb the 1/127 CC
/// quantization: a value sent as CC comes back as e.g. 6.929 for 6.9), and a
/// tiny relative tolerance for everything else (switches, selectors and
/// parameters the app doesn't show).
fn tolerance(index: usize, baseline: f32) -> f32 {
    use crate::tonex_serial::AmpKnob;
    use FxControl::*;
    if AmpKnob::from_param_index(index).is_some() {
        return 0.05;
    }
    let per_model = |c: FxControl| {
        (0..DELAY_MODELS).any(|d| (0..REVERB_MODELS).any(|r| c.param_index(d, r) == index))
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

    /// One parameter changed (our write or a pedal report).
    pub fn set(&mut self, index: usize, value: f32) {
        if let Some(v) = self.current.get_mut(index) {
            *v = value;
        }
    }

    pub fn dirty(&self) -> bool {
        self.baseline
            .iter()
            .zip(&self.current)
            .enumerate()
            .any(|(i, (&b, &c))| (c - b).abs() > tolerance(i, b))
    }
}

#[cfg(test)]
mod diff_tests {
    use super::*;
    use crate::tonex_serial::AmpKnob;

    fn base() -> Vec<f32> {
        let mut raw = vec![0.0f32; PARAM_COUNT];
        raw[AmpKnob::Gain.param_index()] = 6.9;
        raw[FxControl::DelayTime.param_index(0, 0)] = 320.0;
        raw[12] = 300.0; // EQ bass freq, not shown in the app
        raw
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
        // Back to 6.9 via CC: the pedal reports 88/127*10 = 6.929.
        d.set(gain, 88.0 / 127.0 * 10.0);
        assert!(!d.dirty());
    }

    #[test]
    fn epsilon_per_param() {
        let mut d = PresetDiff::default();
        d.load(&base());
        let time = FxControl::DelayTime.param_index(0, 0);
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
    fn reread_keeps_baseline() {
        let mut d = PresetDiff::default();
        d.load(&base());
        let mut edited = base();
        edited[FxControl::DelayMix.param_index(0, 0)] = 40.0;
        d.load(&edited);
        assert!(d.dirty());
        d.load(&base());
        assert!(!d.dirty());
    }
}
