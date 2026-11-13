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

// Amp EQ band frequencies (Hz), serial-only (no MIDI CC); the band gains
// (11, 13, 16) are amp knobs on MIDI CCs. Bass and treble ranges: Builty
// `tonex_params.c`, also the layout fingerprint in tonex_serial.rs. Mid:
// not established in this code base (PROTOCOL.md: idx 15 "suggests a
// frequency", 750 in a preset dump); 150..5000 Hz is an assumption.
const P_EQ_BASS_FREQ: usize = 12;
const P_EQ_MID_FREQ: usize = 15;
const P_EQ_TREBLE_FREQ: usize = 17;
const EQ_FREQS: [(usize, (f32, f32)); 3] =
    [(P_EQ_BASS_FREQ, (75.0, 600.0)), (P_EQ_MID_FREQ, (150.0, 5000.0)), (P_EQ_TREBLE_FREQ, (1000.0, 4000.0))];

/// Amp model dry/wet mix (%), serial-only (no MIDI CC). 100 in a preset
/// dump; range 0..100.
const P_AMP_MIX: usize = 22;
const AMP_MIX_RANGE: (f32, f32) = (0.0, 100.0);
/// EQ mid Q (PROTOCOL.md §5); range as on the pedal's own menu.
const P_EQ_MID_Q: usize = 14;
pub const EQ_MID_Q_RANGE: (f32, f32) = (0.2, 3.0);
/// EQ position, 0 pre / 1 post (hardware-verified).
const P_EQ_POST: usize = 10;

/// Single serial-only amp-tab params, each behind its own plausibility
/// gate (a range check of the one value): (card, index, range, step).
const SINGLES: [(Card, usize, (f32, f32), f32); 3] = [
    (Card::Mix, P_AMP_MIX, AMP_MIX_RANGE, 1.0),
    (Card::MidQ, P_EQ_MID_Q, EQ_MID_Q_RANGE, 0.05),
    (Card::EqPost, P_EQ_POST, (0.0, 1.0), 1.0),
];

/// Knob step for a frequency: 5 Hz below 1 kHz, 50 Hz from there (the
/// same rule as the UI knob's `step` binding).
pub fn freq_step(hz: f32) -> f32 {
    if hz >= 1000.0 {
        50.0
    } else {
        5.0
    }
}

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

// Noise gate (Builty enum 0..=4).
/// 0 pre / 1 post amp (Builty "NG POST", default 0).
const P_GATE_POST: usize = 0;
const P_GATE_ENABLE: usize = 1;
/// Threshold, -100..0 dB (default -64).
const P_GATE_THRESHOLD: usize = 2;
/// Release, 5..500 ms (default 20).
const P_GATE_RELEASE: usize = 3;
/// Depth, -100..-20 dB (default -60).
const P_GATE_DEPTH: usize = 4;

// Cabinet (inside the amp block; Builty enum 24..=33; 23 "CABINET_UNKNOWN"
// is left alone).
/// Cab type: 0 Tone Model (captured cab), 1 VIR, 2 Off. Order from Builty's
/// `TonexCabinetTypes` enum and its web UI; its own header comment (and
/// brunothesatellite, reading a different index) claim the reverse.
const P_CAB_TYPE: usize = 24;
const CAB_TYPES: usize = 3;
/// VIR cabinet model (select). FW 1.6.24 has the 40 models of
/// `VIR_CAB_NAMES` and clamps higher indices to the last one; reads are
/// still accepted as any whole number below `CAB_MODELS` (future firmware,
/// odd presets), shown as "Cab N" beyond the table.
const P_CAB_MODEL: usize = 25;
pub const CAB_MODELS: usize = 64;

/// VIR cab names in index order, as in the official TONEX software's cab
/// list; order verified on the pedal at indices 0, 25 and 39.
pub const VIR_CAB_NAMES: [&str; 40] = [
    "16 DK", "18 CHAMP", "110 TONE", "112 DLX", "112 MARK", "112 CALI", "112 MAZ", "112 TINY", "115 BMAN",
    "212 TWIN", "212 BLUE", "212 H70", "212 JAZZ", "212 OPEN", "212 PPC", "212 REHO", "212 DZ", "412 1960",
    "412 B30", "412 B8000", "412 B9000", "412 BS", "412 DBAG", "412 HW", "412 F1", "412 T1", "412 V1",
    "412 M1", "412 PPC", "412 RETS", "412 RKB", "412 RKV", "412 PIG", "412 GRN", "412 XXL", "412 STD",
    "410 SVX", "115 SVX", "810 SVX", "810 OBC",
];

/// Cab model selector options: the named models, extended with numbered
/// entries ("Cab 41", 1-based) up to `current` if that lies beyond the
/// table, so any index the pedal reports stays selectable.
pub fn cab_model_options(current: usize) -> Vec<String> {
    let len = VIR_CAB_NAMES.len().max(current + 1);
    (0..len)
        .map(|i| VIR_CAB_NAMES.get(i).map_or_else(|| format!("Cab {}", i + 1), |n| n.to_string()))
        .collect()
}
/// VIR resonance 0..10.
const P_CAB_RESO: usize = 26;
/// Mic 1 type: 0 Condenser, 1 Dynamic, 2 Ribbon (Builty MIDI map).
const P_CAB_MIC1: usize = 27;
/// Mic 1 position X / Z, 0..10.
const P_CAB_MIC1_X: usize = 28;
const P_CAB_MIC1_Z: usize = 29;
const P_CAB_MIC2: usize = 30;
/// Mic 2 X: Builty lists 0..2 (likely a copy slip; mic 1 X is 0..10), 0..10
/// used here.
const P_CAB_MIC2_X: usize = 31;
const P_CAB_MIC2_Z: usize = 32;
/// Mic blend, -100..100.
const P_CAB_BLEND: usize = 33;
const MIC_TYPES: usize = 3;

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
    GateEnable,
    GatePost,
    GateThreshold,
    GateRelease,
    GateDepth,
    CabType,
    CabModel,
    CabReso,
    CabMic1,
    CabMic1X,
    CabMic1Z,
    CabMic2,
    CabMic2X,
    CabMic2Z,
    CabBlend,
    EqBassFreq,
    EqMidFreq,
    EqTrebleFreq,
    AmpMix,
    EqMidQ,
    EqPost,
}

/// Which card a parameter belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Card {
    Delay,
    Reverb,
    Comp,
    Mod,
    Gate,
    Cab,
    /// EQ band frequencies of the amp tab.
    Eq,
    /// Amp mix knob of the amp tab (own plausibility gate: one 0..100 value
    /// says little about the layout, unlike the three EQ frequencies).
    Mix,
    /// EQ mid Q knob (own gate, see SINGLES).
    MidQ,
    /// EQ pre/post switch (own gate, see SINGLES).
    EqPost,
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
            40 => GateEnable,
            41 => GatePost,
            42 => GateThreshold,
            43 => GateRelease,
            44 => GateDepth,
            50 => CabType,
            51 => CabModel,
            52 => CabReso,
            53 => CabMic1,
            54 => CabMic1X,
            55 => CabMic1Z,
            56 => CabMic2,
            57 => CabMic2X,
            58 => CabMic2Z,
            59 => CabBlend,
            60 => EqBassFreq,
            61 => EqMidFreq,
            62 => EqTrebleFreq,
            63 => AmpMix,
            64 => EqMidQ,
            65 => EqPost,
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
            GatePost => P_GATE_POST,
            GateEnable => P_GATE_ENABLE,
            GateThreshold => P_GATE_THRESHOLD,
            GateRelease => P_GATE_RELEASE,
            GateDepth => P_GATE_DEPTH,
            CabType => P_CAB_TYPE,
            CabModel => P_CAB_MODEL,
            CabReso => P_CAB_RESO,
            CabMic1 => P_CAB_MIC1,
            CabMic1X => P_CAB_MIC1_X,
            CabMic1Z => P_CAB_MIC1_Z,
            CabMic2 => P_CAB_MIC2,
            CabMic2X => P_CAB_MIC2_X,
            CabMic2Z => P_CAB_MIC2_Z,
            CabBlend => P_CAB_BLEND,
            EqBassFreq => P_EQ_BASS_FREQ,
            EqMidFreq => P_EQ_MID_FREQ,
            EqTrebleFreq => P_EQ_TREBLE_FREQ,
            AmpMix => P_AMP_MIX,
            EqMidQ => P_EQ_MID_Q,
            EqPost => P_EQ_POST,
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
            GateEnable | GatePost | GateThreshold | GateRelease | GateDepth => Card::Gate,
            CabType | CabModel | CabReso | CabMic1 | CabMic1X | CabMic1Z | CabMic2 | CabMic2X | CabMic2Z
            | CabBlend => Card::Cab,
            EqBassFreq | EqMidFreq | EqTrebleFreq => Card::Eq,
            AmpMix => Card::Mix,
            EqMidQ => Card::MidQ,
            EqPost => Card::EqPost,
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

fn gate_range() -> std::ops::Range<usize> {
    P_GATE_POST..P_GATE_DEPTH + 1
}

fn cab_range() -> std::ops::Range<usize> {
    P_CAB_TYPE..P_CAB_BLEND + 1
}

fn mod_range() -> std::ops::Range<usize> {
    P_MOD_POST..MOD_END
}

/// Card owning parameter `index`, if any.
pub fn card_of(index: usize) -> Option<Card> {
    if EQ_FREQS.iter().any(|&(i, _)| i == index) {
        return Some(Card::Eq);
    }
    if let Some(&(card, ..)) = SINGLES.iter().find(|s| s.1 == index) {
        return Some(card);
    }
    [
        (Card::Delay, delay_range()),
        (Card::Reverb, reverb_range()),
        (Card::Comp, comp_range()),
        (Card::Mod, mod_range()),
        (Card::Gate, gate_range()),
        (Card::Cab, cab_range()),
    ]
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

fn gate_section_ok(raw: &[f32]) -> bool {
    raw.len() >= gate_range().end && is_switch(raw[P_GATE_POST]) && is_switch(raw[P_GATE_ENABLE])
}

fn cab_section_ok(raw: &[f32]) -> bool {
    raw.len() >= cab_range().end
        && is_index(raw[P_CAB_TYPE], CAB_TYPES)
        && is_index(raw[P_CAB_MODEL], CAB_MODELS)
        && is_index(raw[P_CAB_MIC1], MIC_TYPES)
        && is_index(raw[P_CAB_MIC2], MIC_TYPES)
}

/// EQ frequencies: each within its Hz range (unlike the switch-only checks
/// above, these values are the only evidence the layout is right).
fn eq_section_ok(raw: &[f32]) -> bool {
    EQ_FREQS.iter().all(|&(i, (min, max))| raw.get(i).is_some_and(|v| (min..=max).contains(v)))
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
        Card::Gate => {
            if let Some(v) = on(P_GATE_ENABLE) {
                ui.set_gate_enabled(v);
            }
            if let Some(v) = on(P_GATE_POST) {
                ui.set_gate_post(v);
            }
            if let Some(v) = m.get(P_GATE_THRESHOLD) {
                ui.set_gate_threshold(snap(v, (-100.0, 0.0, 1.0)));
            }
            if let Some(v) = m.get(P_GATE_RELEASE) {
                ui.set_gate_release(snap(v, (5.0, 500.0, 1.0)));
            }
            if let Some(v) = m.get(P_GATE_DEPTH) {
                ui.set_gate_depth(snap(v, (-100.0, -20.0, 1.0)));
            }
        }
        Card::Cab => {
            let pick = |i: usize, n: usize, set: fn(&AppWindow, i32)| {
                if let Some(x) = model_of(m.get(i), n) {
                    set(ui, x as i32);
                }
            };
            pick(P_CAB_TYPE, CAB_TYPES, AppWindow::set_cab_type);
            // Options first (extended past the named models if needed), so
            // the selector never shows an index without a label.
            if let Some(model) = model_of(m.get(P_CAB_MODEL), CAB_MODELS) {
                set_cab_model_options(ui, model);
                ui.set_cab_model(model as i32);
            }
            pick(P_CAB_MIC1, MIC_TYPES, AppWindow::set_cab_mic1);
            pick(P_CAB_MIC2, MIC_TYPES, AppWindow::set_cab_mic2);
            let knobs: [(usize, (f32, f32, f32), fn(&AppWindow, f32)); 6] = [
                (P_CAB_RESO, TEN, AppWindow::set_cab_reso),
                (P_CAB_MIC1_X, TEN, AppWindow::set_cab_mic1_x),
                (P_CAB_MIC1_Z, TEN, AppWindow::set_cab_mic1_z),
                (P_CAB_MIC2_X, TEN, AppWindow::set_cab_mic2_x),
                (P_CAB_MIC2_Z, TEN, AppWindow::set_cab_mic2_z),
                (P_CAB_BLEND, (-100.0, 100.0, 1.0), AppWindow::set_cab_blend),
            ];
            for (i, spec, set) in knobs {
                if let Some(v) = m.get(i) {
                    set(ui, snap(v, spec));
                }
            }
        }
        Card::Eq => {
            let setters: [fn(&AppWindow, f32); 3] =
                [AppWindow::set_eq_bass_freq, AppWindow::set_eq_mid_freq, AppWindow::set_eq_treble_freq];
            let mut known = true;
            for ((i, (min, max)), set) in EQ_FREQS.into_iter().zip(setters) {
                match m.get(i) {
                    Some(v) => set(ui, snap(v, (min, max, freq_step(v)))),
                    None => known = false,
                }
            }
            ui.set_eq_freq_known(known);
        }
        Card::Mix | Card::MidQ | Card::EqPost => {
            let Some(&(_, i, (min, max), step)) = SINGLES.iter().find(|s| s.0 == card) else { return };
            let v = m.get(i).map(|v| snap(v, (min, max, step)));
            match card {
                Card::Mix => {
                    if let Some(v) = v {
                        ui.set_amp_mix(v);
                    }
                    ui.set_amp_mix_known(v.is_some());
                }
                Card::MidQ => {
                    if let Some(v) = v {
                        ui.set_eq_mid_q(v);
                    }
                    ui.set_eq_mid_q_known(v.is_some());
                }
                _ => {
                    if let Some(v) = v {
                        ui.set_eq_post(v != 0.0);
                    }
                    ui.set_eq_post_known(v.is_some());
                }
            }
        }
    }
}

/// Points the double-click reset of the serial-only amp-tab knobs (EQ
/// frequencies, mix, mid Q) at the values the preset was loaded with (in
/// range only).
pub fn set_loaded_defaults(ui: &AppWindow, diff: &PresetDiff) {
    let setters: [fn(&AppWindow, f32); 3] = [
        AppWindow::set_eq_bass_freq_default,
        AppWindow::set_eq_mid_freq_default,
        AppWindow::set_eq_treble_freq_default,
    ];
    for ((i, (min, max)), set) in EQ_FREQS.into_iter().zip(setters) {
        if let Some(v) = diff.baseline(i).filter(|v| (min..=max).contains(v)) {
            set(ui, v);
        }
    }
    let (min, max) = AMP_MIX_RANGE;
    if let Some(v) = diff.baseline(P_AMP_MIX).filter(|v| (min..=max).contains(v)) {
        ui.set_amp_mix_default(v);
    }
    let (min, max) = EQ_MID_Q_RANGE;
    if let Some(v) = diff.baseline(P_EQ_MID_Q).filter(|v| (min..=max).contains(v)) {
        ui.set_eq_mid_q_default(v);
    }
}

/// Puts `cab_model_options(current)` into the UI unless the list already
/// has that length.
pub fn set_cab_model_options(ui: &AppWindow, current: usize) {
    use slint::Model;
    let options = cab_model_options(current);
    if ui.get_cab_model_names().row_count() != options.len() {
        let model: Vec<slint::SharedString> = options.into_iter().map(Into::into).collect();
        ui.set_cab_model_names(slint::ModelRc::new(slint::VecModel::from(model)));
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
        assert_eq!(card_of(10), Some(Card::EqPost));
        assert_eq!(card_of(20), None);
        // Gate 0..=4, cabinet 24..=33; amp knobs/enable stay outside.
        assert_eq!(GateThreshold.param_index(m(0, 0, 0)), Some(2));
        assert_eq!(CabType.param_index(m(0, 0, 0)), Some(24));
        assert_eq!(CabBlend.param_index(m(0, 0, 0)), Some(33));
        assert_eq!(card_of(0), Some(Card::Gate));
        assert_eq!(card_of(4), Some(Card::Gate));
        assert_eq!(card_of(24), Some(Card::Cab));
        assert_eq!(card_of(33), Some(Card::Cab));
        for amp in [11, 13, 16, 18, 20, 21, 23, 34, 35] {
            assert_eq!(card_of(amp), None, "index {amp}");
        }
        for eq in [12, 15, 17] {
            assert_eq!(card_of(eq), Some(Card::Eq), "index {eq}");
        }
        assert_eq!(card_of(22), Some(Card::Mix));
        assert_eq!(card_of(14), Some(Card::MidQ));
        assert_eq!(card_of(10), Some(Card::EqPost));
    }

    #[test]
    fn ids_roundtrip_and_cards() {
        for id in (0..=8).chain(10..=16).chain(20..=24).chain(30..=38).chain(40..=44).chain(50..=65) {
            let c = FxControl::from_id(id).unwrap();
            let card = match id {
                0..=8 => Card::Delay,
                10..=16 => Card::Reverb,
                20..=24 => Card::Comp,
                30..=38 => Card::Mod,
                40..=44 => Card::Gate,
                60..=62 => Card::Eq,
                63 => Card::Mix,
                64 => Card::MidQ,
                65 => Card::EqPost,
                _ => Card::Cab,
            };
            assert_eq!(c.card(), card);
        }
        assert_eq!(FxControl::EqMidFreq.param_index(Models::default()), Some(15));
        assert_eq!(FxControl::AmpMix.param_index(Models::default()), Some(22));
        assert_eq!(FxControl::EqMidQ.param_index(Models::default()), Some(14));
        assert_eq!(FxControl::EqPost.param_index(Models::default()), Some(10));
        for id in [9, 17, 25, 39, 45, 66] {
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
    fn vir_cab_names_and_fallback() {
        // Verified on the pedal: 0, 25 and 39.
        assert_eq!(VIR_CAB_NAMES[0], "16 DK");
        assert_eq!(VIR_CAB_NAMES[25], "412 T1");
        assert_eq!(VIR_CAB_NAMES[39], "810 OBC");
        let named = cab_model_options(0);
        assert_eq!(named.len(), 40);
        assert_eq!(named[25], "412 T1");
        assert_eq!(cab_model_options(39).len(), 40);
        // Beyond the table: numbered entries up to the current index.
        let extended = cab_model_options(45);
        assert_eq!(extended.len(), 46);
        assert_eq!(extended[39], "810 OBC");
        assert_eq!(extended[40], "Cab 41");
        assert_eq!(extended[45], "Cab 46");
        assert!(VIR_CAB_NAMES.len() < CAB_MODELS, "reads past the table stay accepted");
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
    /// Preset volume (section 03, not part of the array).
    level_base: Option<f32>,
    level_cur: Option<f32>,
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

    /// Value of parameter `index` as loaded with the preset (None before
    /// the first read); double-click resets of the EQ frequencies use it.
    pub fn baseline(&self, index: usize) -> Option<f32> {
        self.baseline.get(index).copied()
    }

    /// Preset volume read with the preset's details: baseline right after
    /// a preset load, else only the current value.
    pub fn load_level(&mut self, value: f32) {
        if self.level_base.is_none() {
            self.level_base = Some(value);
        }
        self.level_cur = Some(value);
    }

    /// Preset volume changed (our write or a pedal report).
    pub fn set_level(&mut self, value: f32) {
        self.level_cur = Some(value);
    }

    /// One parameter changed (our write or a pedal report).
    pub fn set(&mut self, index: usize, value: f32) {
        if let Some(v) = self.current.get_mut(index) {
            *v = value;
        }
    }

    pub fn dirty(&self) -> bool {
        // Half the knob's 0.1 step.
        let level_changed = matches!((self.level_base, self.level_cur), (Some(b), Some(c)) if (c - b).abs() > 0.05);
        level_changed
            || self
                .baseline
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
    fn preset_level_participates() {
        let mut d = PresetDiff::default();
        d.load(&base());
        d.load_level(8.0);
        assert!(!d.dirty());
        d.set_level(8.03);
        assert!(!d.dirty());
        d.set_level(7.0);
        assert!(d.dirty());
        d.load_level(7.0); // re-read keeps the 8.0 baseline
        assert!(d.dirty());
        d.set_level(8.0);
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
