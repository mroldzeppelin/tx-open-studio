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

use crate::protocol::{
    DELAY_BLOCK_LEN, DELAY_MODELS, PARAM_COUNT, P_DELAY_ENABLE, P_DELAY_FIRST_BLOCK, P_DELAY_MODEL, P_DELAY_POST,
    P_REVERB_ENABLE, P_REVERB_FIRST_BLOCK, P_REVERB_MODEL, P_REVERB_POSITION, REVERB_BLOCK_LEN, REVERB_MODELS,
};

mod controls;
mod names;
mod mirror;
mod refresh;
mod diff;

pub(crate) use controls::*;
pub(crate) use names::*;
pub(crate) use mirror::*;
pub(crate) use refresh::*;
pub(crate) use diff::*;
