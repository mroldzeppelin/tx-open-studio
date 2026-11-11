//! Effect cards (Compressor, Modulation, Delay, Reverb): control ids, the
//! pedal's parameter indices, and a mirror of the current preset's parameter
//! array that the cards display.
//!
//! Every effect control is written over USB serial (`0x0207`
//! single-parameter write, confirmed on hardware) rather than MIDI CC:
//! serial addresses the exact per-model parameter with its native value
//! (dB, ms, %, index), while the CC numbers/scaling for most of these
//! controls are undocumented. Indices and ranges: Builty/TonexOneController
//! `tonex_params.h` / `tonex_params.c`; option lists: its `web/index.html`.
//!
//! The pedal stores per-model values for delay (Digital/Tape), reverb
//! (6 models) and modulation (5 models; blocks of different lengths), so the
//! index of those controls depends on the selected model. Switching model
//! re-displays that model's stored values from the mirror.

use std::time::Instant;

use crate::tonex_serial::{
    DELAY_BLOCK_LEN, DELAY_MODELS, PARAM_COUNT, P_DELAY_ENABLE, P_DELAY_FIRST_BLOCK, P_DELAY_MODEL, P_DELAY_POST,
    P_REVERB_ENABLE, P_REVERB_FIRST_BLOCK, P_REVERB_MODEL, P_REVERB_POSITION, REVERB_BLOCK_LEN, REVERB_MODELS,
};
use crate::{AppWindow, ECHO_GUARD};

/// Number of time-signature options (delay and modulation `division`
/// 0..=17: 1/32, 1/32 D, 1/32 T, 1/16 … 1/1 T).
const DIVISIONS: usize = 18;
const DELAY_TIME_MAX: f32 = 1000.0;

// Compressor (Builty enum 5..=9).
/// 0 pre / 1 post amp (Builty "COMP POST", default 1).
const P_COMP_POST: usize = 5;
const P_COMP_ENABLE: usize = 6;
/// Threshold, -40..0 dB (default -14).
const P_COMP_THRESHOLD: usize = 7;
/// Make-up gain, -30..10 dB (default -12).
const P_COMP_GAIN: usize = 8;
/// Attack, 1..51 ms (default 14).
const P_COMP_ATTACK: usize = 9;

// Modulation (Builty enum 63..=93).
const P_MOD_POST: usize = 63;
const P_MOD_ENABLE: usize = 64;
/// 0 Chorus, 1 Tremolo, 2 Phaser, 3 Flanger, 4 Rotary.
const P_MOD_MODEL: usize = 65;
const MOD_MODELS: usize = 5;
/// Per-model blocks (first index, length). Each starts with sync (0/1) and
/// time signature (0..=17), followed by the model's knobs (`MOD_KNOBS`).
const MOD_BLOCKS: [(usize, usize); MOD_MODELS] = [(66, 5), (71, 6), (77, 5), (82, 6), (88, 6)];
const MOD_END: usize = 94;

/// Knob range (min, max, step) per modulation model, in block order after
/// sync/time signature (tonex_params.c):
/// Chorus: rate 0.1..10, depth 0..100, level 0..10.
/// Tremolo: rate, shape 0..10, spread 0..100, level.
/// Phaser: rate, depth 0..100, level.
/// Flanger: rate, depth 0..100, feedback 0..100, level.
/// Rotary: speed 0..400, radius 0..300, spread 0..100, level.
/// The labels/units live in app.slint (`mod-*` knob definitions).
const RATE: (f32, f32, f32) = (0.1, 10.0, 0.1);
const PCT: (f32, f32, f32) = (0.0, 100.0, 1.0);
const TEN: (f32, f32, f32) = (0.0, 10.0, 0.1);
const MOD_KNOBS: [&[(f32, f32, f32)]; MOD_MODELS] = [
    &[RATE, PCT, TEN],
    &[RATE, TEN, PCT, TEN],
    &[RATE, PCT, TEN],
    &[RATE, PCT, PCT, TEN],
    &[(0.0, 400.0, 1.0), (0.0, 300.0, 1.0), PCT, TEN],
];

/// An effect UI control; the id is the `fx-changed` id in app.slint.
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
    CompEnable,
    CompPost,
    CompThreshold,
    CompGain,
    CompAttack,
    ModEnable,
    ModPost,
    ModModel,
    ModSync,
    ModDivision,
    /// n-th knob (0..=3) of the selected modulation model.
    ModKnob(u8),
}

/// Which card a parameter belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Card {
    Delay,
    Reverb,
    Comp,
    Mod,
}

/// Selected (0-based) model per card, for per-model parameter indices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Models {
    pub delay: usize,
    pub reverb: usize,
    pub modulation: usize,
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
            20 => CompEnable,
            21 => CompPost,
            22 => CompThreshold,
            23 => CompGain,
            24 => CompAttack,
            30 => ModEnable,
            31 => ModPost,
            32 => ModModel,
            33 => ModSync,
            34 => ModDivision,
            35..=38 => ModKnob((id - 35) as u8),
            _ => return None,
        })
    }

    /// Parameter index on the pedal; per-model controls use `m`. None for a
    /// modulation knob slot the selected model doesn't have.
    pub fn param_index(self, m: Models) -> Option<usize> {
        use FxControl::*;
        let dly = P_DELAY_FIRST_BLOCK + m.delay.min(DELAY_MODELS - 1) * DELAY_BLOCK_LEN;
        let rvb = P_REVERB_FIRST_BLOCK + m.reverb.min(REVERB_MODELS - 1) * REVERB_BLOCK_LEN;
        let (mod_block, mod_len) = MOD_BLOCKS[m.modulation.min(MOD_MODELS - 1)];
        Some(match self {
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
            CompPost => P_COMP_POST,
            CompEnable => P_COMP_ENABLE,
            CompThreshold => P_COMP_THRESHOLD,
            CompGain => P_COMP_GAIN,
            CompAttack => P_COMP_ATTACK,
            ModPost => P_MOD_POST,
            ModEnable => P_MOD_ENABLE,
            ModModel => P_MOD_MODEL,
            ModSync => mod_block,
            ModDivision => mod_block + 1,
            ModKnob(n) => {
                let i = mod_block + 2 + usize::from(n);
                if i >= mod_block + mod_len {
                    return None;
                }
                i
            }
        })
    }

    pub fn card(self) -> Card {
        use FxControl::*;
        match self {
            DelayEnable | DelayPost | DelayModel | DelaySync | DelayDivision | DelayTime | DelayFeedback
            | DelayPingPong | DelayMix => Card::Delay,
            ReverbEnable | ReverbPost | ReverbModel | ReverbTime | ReverbPredelay | ReverbColor | ReverbMix => {
                Card::Reverb
            }
            CompEnable | CompPost | CompThreshold | CompGain | CompAttack => Card::Comp,
            ModEnable | ModPost | ModModel | ModSync | ModDivision | ModKnob(_) => Card::Mod,
        }
    }

    /// Selecting a model changes which per-model values are shown.
    pub fn is_model(self) -> bool {
        matches!(self, FxControl::DelayModel | FxControl::ReverbModel | FxControl::ModModel)
    }
}

fn delay_range() -> std::ops::Range<usize> {
    P_DELAY_POST..P_DELAY_FIRST_BLOCK + DELAY_MODELS * DELAY_BLOCK_LEN
}

fn reverb_range() -> std::ops::Range<usize> {
    P_REVERB_POSITION..P_REVERB_FIRST_BLOCK + REVERB_MODELS * REVERB_BLOCK_LEN
}

fn comp_range() -> std::ops::Range<usize> {
    P_COMP_POST..P_COMP_ATTACK + 1
}

fn mod_range() -> std::ops::Range<usize> {
    P_MOD_POST..MOD_END
}

/// Card owning parameter `index`, if any.
pub fn card_of(index: usize) -> Option<Card> {
    [(Card::Delay, delay_range()), (Card::Reverb, reverb_range()), (Card::Comp, comp_range()), (Card::Mod, mod_range())]
        .into_iter()
        .find(|(_, r)| r.contains(&index))
        .map(|(c, _)| c)
}

fn is_switch(v: f32) -> bool {
    v == 0.0 || v == 1.0
}

fn is_index(v: f32, count: usize) -> bool {
    v.fract() == 0.0 && (0.0..count as f32).contains(&v)
}

/// Loose plausibility checks per card section of a raw parameter array.
/// Only switches and selectors are checked (continuous values are clamped
/// for display), so valid presets aren't rejected over range details.
fn delay_section_ok(raw: &[f32]) -> bool {
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

fn reverb_section_ok(raw: &[f32]) -> bool {
    raw.len() >= reverb_range().end
        && is_switch(raw[P_REVERB_POSITION])
        && is_switch(raw[P_REVERB_ENABLE])
        && is_index(raw[P_REVERB_MODEL], REVERB_MODELS)
}

fn comp_section_ok(raw: &[f32]) -> bool {
    raw.len() >= comp_range().end && is_switch(raw[P_COMP_POST]) && is_switch(raw[P_COMP_ENABLE])
}

fn mod_section_ok(raw: &[f32]) -> bool {
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
    fn get(&self, index: usize) -> Option<f32> {
        self.values.get(index).copied().flatten()
    }

    fn recent(&self, index: usize) -> bool {
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

/// Rounds to `step` and clamps to `min..=max`.
fn snap(v: f32, (min, max, step): (f32, f32, f32)) -> f32 {
    ((v / step).round() * step).clamp(min, max)
}

/// Models the UI currently shows.
fn ui_models(ui: &AppWindow) -> Models {
    let m = |v: i32| usize::try_from(v).unwrap_or(0);
    Models { delay: m(ui.get_dly_model()), reverb: m(ui.get_rvb_model()), modulation: m(ui.get_mod_model()) }
}

/// Pushes the mirror's values for `card` into the UI (unknown ones are left
/// as they are).
pub fn refresh(ui: &AppWindow, m: &FxMirror, card: Card) {
    let on = |i: usize| m.get(i).map(|v| v != 0.0);
    match card {
        Card::Delay => {
            if let Some(v) = on(P_DELAY_ENABLE) {
                ui.set_dly_enabled(v);
            }
            if let Some(v) = on(P_DELAY_POST) {
                ui.set_dly_post(v);
            }
            if let Some(model) = model_of(m.get(P_DELAY_MODEL), DELAY_MODELS) {
                ui.set_dly_model(model as i32);
            }
            let models = ui_models(ui);
            let get = |c: FxControl| c.param_index(models).and_then(|i| m.get(i));
            if let Some(v) = get(FxControl::DelaySync) {
                ui.set_dly_sync(v != 0.0);
            }
            if let Some(d) = model_of(get(FxControl::DelayDivision), DIVISIONS) {
                ui.set_dly_division(d as i32);
            }
            if let Some(v) = get(FxControl::DelayTime) {
                ui.set_dly_time(snap(v, (0.0, DELAY_TIME_MAX, 1.0)));
            }
            if let Some(v) = get(FxControl::DelayFeedback) {
                ui.set_dly_feedback(snap(v, PCT));
            }
            if let Some(v) = get(FxControl::DelayPingPong) {
                ui.set_dly_pingpong(v != 0.0);
            }
            if let Some(v) = get(FxControl::DelayMix) {
                ui.set_dly_mix(snap(v, PCT));
            }
        }
        Card::Reverb => {
            if let Some(v) = on(P_REVERB_ENABLE) {
                ui.set_rvb_enabled(v);
            }
            if let Some(v) = on(P_REVERB_POSITION) {
                ui.set_rvb_post(v);
            }
            if let Some(model) = model_of(m.get(P_REVERB_MODEL), REVERB_MODELS) {
                ui.set_rvb_model(model as i32);
            }
            let models = ui_models(ui);
            let get = |c: FxControl| c.param_index(models).and_then(|i| m.get(i));
            if let Some(v) = get(FxControl::ReverbTime) {
                ui.set_rvb_time(snap(v, TEN));
            }
            if let Some(v) = get(FxControl::ReverbPredelay) {
                ui.set_rvb_predelay(snap(v, (0.0, 500.0, 1.0)));
            }
            if let Some(v) = get(FxControl::ReverbColor) {
                ui.set_rvb_color(snap(v, (-10.0, 10.0, 0.1)));
            }
            if let Some(v) = get(FxControl::ReverbMix) {
                ui.set_rvb_mix(snap(v, PCT));
            }
        }
        Card::Comp => {
            if let Some(v) = on(P_COMP_ENABLE) {
                ui.set_comp_enabled(v);
            }
            if let Some(v) = on(P_COMP_POST) {
                ui.set_comp_post(v);
            }
            if let Some(v) = m.get(P_COMP_THRESHOLD) {
                ui.set_comp_threshold(snap(v, (-40.0, 0.0, 0.1)));
            }
            if let Some(v) = m.get(P_COMP_GAIN) {
                ui.set_comp_gain(snap(v, (-30.0, 10.0, 0.1)));
            }
            if let Some(v) = m.get(P_COMP_ATTACK) {
                ui.set_comp_attack(snap(v, (1.0, 51.0, 1.0)));
            }
        }
        Card::Mod => {
            if let Some(v) = on(P_MOD_ENABLE) {
                ui.set_mod_enabled(v);
            }
            if let Some(v) = on(P_MOD_POST) {
                ui.set_mod_post(v);
            }
            if let Some(model) = model_of(m.get(P_MOD_MODEL), MOD_MODELS) {
                ui.set_mod_model(model as i32);
            }
            let models = ui_models(ui);
            let get = |c: FxControl| c.param_index(models).and_then(|i| m.get(i));
            if let Some(v) = get(FxControl::ModSync) {
                ui.set_mod_sync(v != 0.0);
            }
            if let Some(d) = model_of(get(FxControl::ModDivision), DIVISIONS) {
                ui.set_mod_division(d as i32);
            }
            let specs = MOD_KNOBS[models.modulation.min(MOD_MODELS - 1)];
            let setters: [fn(&AppWindow, f32); 4] =
                [AppWindow::set_mod_k1, AppWindow::set_mod_k2, AppWindow::set_mod_k3, AppWindow::set_mod_k4];
            for (n, (spec, set)) in specs.iter().zip(setters).enumerate() {
                if let Some(v) = get(FxControl::ModKnob(n as u8)) {
                    set(ui, snap(v, *spec));
                }
            }
        }
    }
}

/// Parameter index for a UI change, using the models the UI shows.
pub fn index_for(ui: &AppWindow, control: FxControl) -> Option<usize> {
    control.param_index(ui_models(ui))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indices_follow_builty_enum() {
        use FxControl::*;
        let m = |delay, reverb, modulation| Models { delay, reverb, modulation };
        // Digital block 97..=102, Tape block 103..=108.
        assert_eq!(DelayTime.param_index(m(0, 0, 0)), Some(99));
        assert_eq!(DelayMix.param_index(m(1, 0, 0)), Some(108));
        assert_eq!(DelaySync.param_index(m(1, 0, 0)), Some(103));
        // Spring 1 block 39..=42, Plate (model 5) block 59..=62.
        assert_eq!(ReverbTime.param_index(m(0, 0, 0)), Some(39));
        assert_eq!(ReverbMix.param_index(m(0, 5, 0)), Some(62));
        assert_eq!(ReverbColor.param_index(m(0, 4, 0)), Some(57));
        // Compressor 5..=9.
        assert_eq!(CompThreshold.param_index(m(0, 0, 0)), Some(7));
        assert_eq!(CompAttack.param_index(m(0, 0, 0)), Some(9));
        // Modulation: chorus 66..=70 (3 knobs), tremolo 71..=76 (4 knobs),
        // rotary 88..=93.
        assert_eq!(ModSync.param_index(m(0, 0, 0)), Some(66));
        assert_eq!(ModKnob(0).param_index(m(0, 0, 0)), Some(68));
        assert_eq!(ModKnob(2).param_index(m(0, 0, 0)), Some(70));
        assert_eq!(ModKnob(3).param_index(m(0, 0, 0)), None);
        assert_eq!(ModKnob(3).param_index(m(0, 0, 1)), Some(76));
        assert_eq!(ModDivision.param_index(m(0, 0, 3)), Some(83));
        assert_eq!(ModKnob(3).param_index(m(0, 0, 4)), Some(93));
        assert_eq!(card_of(108), Some(Card::Delay));
        assert_eq!(card_of(36), Some(Card::Reverb));
        assert_eq!(card_of(63), Some(Card::Mod));
        assert_eq!(card_of(93), Some(Card::Mod));
        assert_eq!(card_of(5), Some(Card::Comp));
        assert_eq!(card_of(10), None);
        assert_eq!(card_of(20), None);
    }

    #[test]
    fn ids_roundtrip_and_cards() {
        for id in (0..=8).chain(10..=16).chain(20..=24).chain(30..=38) {
            let c = FxControl::from_id(id).unwrap();
            let card = match id {
                0..=8 => Card::Delay,
                10..=16 => Card::Reverb,
                20..=24 => Card::Comp,
                _ => Card::Mod,
            };
            assert_eq!(c.card(), card);
        }
        for id in [9, 17, 25, 39] {
            assert_eq!(FxControl::from_id(id), None);
        }
    }

    fn plausible_raw() -> Vec<f32> {
        let mut raw = vec![0.0f32; PARAM_COUNT];
        raw[P_REVERB_MODEL] = 5.0;
        raw[P_DELAY_MODEL] = 1.0;
        raw[P_DELAY_FIRST_BLOCK + 1] = 9.0; // 1/4
        raw[P_DELAY_FIRST_BLOCK + DELAY_BLOCK_LEN + 2] = 320.0;
        raw[P_COMP_POST] = 1.0;
        raw[P_MOD_MODEL] = 4.0;
        raw[MOD_BLOCKS[4].0 + 1] = 17.0;
        raw
    }

    #[test]
    fn loads_plausible_sections() {
        let mut m = FxMirror::default();
        assert_eq!(m.load(&plausible_raw()), [Card::Delay, Card::Reverb, Card::Comp, Card::Mod]);
        let idx = FxControl::DelayTime.param_index(Models { delay: 1, ..Models::default() }).unwrap();
        assert_eq!(m.get(idx), Some(320.0));
    }

    #[test]
    fn rejects_implausible_or_short_sections() {
        let mut raw = plausible_raw();
        raw[P_DELAY_MODEL] = 2.5;
        let mut m = FxMirror::default();
        raw[MOD_BLOCKS[2].0] = 0.5;
        assert_eq!(m.load(&raw), [Card::Reverb, Card::Comp]);
        assert_eq!(m.load(&raw[..80]), [Card::Reverb, Card::Comp]);
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
        raw[FxControl::DelayTime.param_index(Models::default()).unwrap()] = 320.0;
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
