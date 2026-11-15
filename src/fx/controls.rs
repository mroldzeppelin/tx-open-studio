//! Effect controls: the pedal's parameter indices and ranges, the `fx-changed` control ids and which card a parameter belongs to.

use super::*;

/// Number of time-signature options (delay and modulation `division`
/// 0..=17: 1/32, 1/32 D, 1/32 T, 1/16 … 1/1 T).
pub(crate) const DIVISIONS: usize = 18;

pub(crate) const DELAY_TIME_MAX: f32 = 1000.0;

// Amp EQ band frequencies (Hz), serial-only (no MIDI CC); the band gains
// (11, 13, 16) are amp knobs on MIDI CCs. Bass and treble ranges: Builty
// `tonex_params.c`, also the layout fingerprint in protocol.rs. Mid:
// not established in this code base (PROTOCOL.md: idx 15 "suggests a
// frequency", 750 in a preset dump); 150..5000 Hz is an assumption.
pub(crate) const P_EQ_BASS_FREQ: usize = 12;

pub(crate) const P_EQ_MID_FREQ: usize = 15;

pub(crate) const P_EQ_TREBLE_FREQ: usize = 17;

pub(crate) const EQ_FREQS: [(usize, (f32, f32)); 3] =
    [(P_EQ_BASS_FREQ, (75.0, 600.0)), (P_EQ_MID_FREQ, (150.0, 5000.0)), (P_EQ_TREBLE_FREQ, (1000.0, 4000.0))];

/// Amp model dry/wet mix (%), serial-only (no MIDI CC). 100 in a preset
/// dump; range 0..100.
pub(crate) const P_AMP_MIX: usize = 22;

pub(crate) const AMP_MIX_RANGE: (f32, f32) = (0.0, 100.0);

/// EQ mid Q (PROTOCOL.md §5); range as on the pedal's own menu.
pub(crate) const P_EQ_MID_Q: usize = 14;

pub const EQ_MID_Q_RANGE: (f32, f32) = (0.2, 3.0);

/// EQ position, 0 pre / 1 post (hardware-verified).
pub(crate) const P_EQ_POST: usize = 10;

/// Single serial-only amp-tab params, each behind its own plausibility
/// gate (a range check of the one value): (card, index, range, step).
pub(crate) const SINGLES: [(Card, usize, (f32, f32), f32); 3] = [
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
pub(crate) const P_COMP_POST: usize = 5;

pub(crate) const P_COMP_ENABLE: usize = 6;

/// Threshold, -40..0 dB (default -14).
pub(crate) const P_COMP_THRESHOLD: usize = 7;

/// Make-up gain, -30..10 dB (default -12).
pub(crate) const P_COMP_GAIN: usize = 8;

/// Attack, 1..51 ms (default 14).
pub(crate) const P_COMP_ATTACK: usize = 9;

// Noise gate (Builty enum 0..=4).
/// 0 pre / 1 post amp (Builty "NG POST", default 0).
pub(crate) const P_GATE_POST: usize = 0;

pub(crate) const P_GATE_ENABLE: usize = 1;

/// Threshold, -100..0 dB (default -64).
pub(crate) const P_GATE_THRESHOLD: usize = 2;

/// Release, 5..500 ms (default 20).
pub(crate) const P_GATE_RELEASE: usize = 3;

/// Depth, -100..-20 dB (default -60).
pub(crate) const P_GATE_DEPTH: usize = 4;

// Cabinet (inside the amp block; Builty enum 24..=33; 23 "CABINET_UNKNOWN"
// is left alone).
/// Cab type: 0 Tone Model (captured cab), 1 VIR, 2 Off. Order from Builty's
/// `TonexCabinetTypes` enum and its web UI; its own header comment (and
/// brunothesatellite, reading a different index) claim the reverse.
pub(crate) const P_CAB_TYPE: usize = 24;

pub(crate) const CAB_TYPES: usize = 3;

/// VIR cabinet model (select). FW 1.6.24 has the 40 models of
/// `VIR_CAB_NAMES` and clamps higher indices to the last one; reads are
/// still accepted as any whole number below `CAB_MODELS` (future firmware,
/// odd presets), shown as "Cab N" beyond the table.
pub(crate) const P_CAB_MODEL: usize = 25;

pub const CAB_MODELS: usize = 64;

/// VIR resonance 0..10.
pub(crate) const P_CAB_RESO: usize = 26;

/// Mic 1 type: 0 Condenser, 1 Dynamic, 2 Ribbon (Builty MIDI map).
pub(crate) const P_CAB_MIC1: usize = 27;

/// Mic 1 position X / Z, 0..10.
pub(crate) const P_CAB_MIC1_X: usize = 28;

pub(crate) const P_CAB_MIC1_Z: usize = 29;

pub(crate) const P_CAB_MIC2: usize = 30;

/// Mic 2 X: Builty lists 0..2 (likely a copy slip; mic 1 X is 0..10), 0..10
/// used here.
pub(crate) const P_CAB_MIC2_X: usize = 31;

pub(crate) const P_CAB_MIC2_Z: usize = 32;

/// Mic blend, -100..100.
pub(crate) const P_CAB_BLEND: usize = 33;

pub(crate) const MIC_TYPES: usize = 3;

// Modulation (Builty enum 63..=93).
pub(crate) const P_MOD_POST: usize = 63;

pub(crate) const P_MOD_ENABLE: usize = 64;

/// 0 Chorus, 1 Tremolo, 2 Phaser, 3 Flanger, 4 Rotary.
pub(crate) const P_MOD_MODEL: usize = 65;

pub(crate) const MOD_MODELS: usize = 5;

/// Per-model blocks (first index, length). Each starts with sync (0/1) and
/// time signature (0..=17), followed by the model's knobs (`MOD_KNOBS`).
pub(crate) const MOD_BLOCKS: [(usize, usize); MOD_MODELS] = [(66, 5), (71, 6), (77, 5), (82, 6), (88, 6)];

pub(crate) const MOD_END: usize = 94;

/// Knob range (min, max, step) per modulation model, in block order after
/// sync/time signature (tonex_params.c):
/// Chorus: rate 0.1..10, depth 0..100, level 0..10.
/// Tremolo: rate, shape 0..10, spread 0..100, level.
/// Phaser: rate, depth 0..100, level.
/// Flanger: rate, depth 0..100, feedback 0..100, level.
/// Rotary: speed 0..400, radius 0..300, spread 0..100, level.
/// The labels/units live in app.slint (`mod-*` knob definitions).
pub(crate) const RATE: (f32, f32, f32) = (0.1, 10.0, 0.1);

pub(crate) const PCT: (f32, f32, f32) = (0.0, 100.0, 1.0);

pub(crate) const TEN: (f32, f32, f32) = (0.0, 10.0, 0.1);

pub(crate) const MOD_KNOBS: [&[(f32, f32, f32)]; MOD_MODELS] = [
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

pub(crate) fn delay_range() -> std::ops::Range<usize> {
    P_DELAY_POST..P_DELAY_FIRST_BLOCK + DELAY_MODELS * DELAY_BLOCK_LEN
}

pub(crate) fn reverb_range() -> std::ops::Range<usize> {
    P_REVERB_POSITION..P_REVERB_FIRST_BLOCK + REVERB_MODELS * REVERB_BLOCK_LEN
}

pub(crate) fn comp_range() -> std::ops::Range<usize> {
    P_COMP_POST..P_COMP_ATTACK + 1
}

pub(crate) fn gate_range() -> std::ops::Range<usize> {
    P_GATE_POST..P_GATE_DEPTH + 1
}

pub(crate) fn cab_range() -> std::ops::Range<usize> {
    P_CAB_TYPE..P_CAB_BLEND + 1
}

pub(crate) fn mod_range() -> std::ops::Range<usize> {
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
}
