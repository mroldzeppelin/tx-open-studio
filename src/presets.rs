//! The preset list: names and characters, ids and search, the current preset in the UI.

use crate::*;
use std::sync::Arc;
use std::sync::MutexGuard;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;
use slint::ModelRc;
use slint::VecModel;
use slint::Model;

pub(crate) const MIN_PRESET: i32 = 1;

pub(crate) const MAX_PRESET: i32 = 150;

/// Shown only until the pedal's state reply tells us its current preset.
pub(crate) const DEFAULT_PRESET: i32 = 43;

pub(crate) fn clamp_preset(p: i32) -> i32 {
    p.clamp(MIN_PRESET, MAX_PRESET)
}

/// Preset names and characters read from the pedal over USB serial
/// (index 0 = preset 1).
#[derive(Clone, Default)]
pub(crate) struct PresetNames {
    pub(crate) names: Arc<Mutex<Vec<PresetMeta>>>,
    /// Current preset-list search query (as typed).
    pub(crate) query: Arc<Mutex<String>>,
    /// "amp · cab" line per preset (index 0 = preset 1; "" = unknown).
    pub(crate) tones: Arc<Mutex<Vec<String>>>,
    /// Search-only words per preset (what its models are based on).
    pub(crate) tone_search: Arc<Mutex<Vec<String>>>,
}

pub(crate) fn bank_number(preset_num: i32) -> i32 {
    (clamp_preset(preset_num) - 1) / 3
}

pub(crate) fn slot_letter(preset_num: i32) -> char {
    ['A', 'B', 'C'][((clamp_preset(preset_num) - 1) % 3) as usize]
}

/// Short preset id as on the OLED: 0-based bank, zero-padded, plus slot
/// letter (preset 125 → "41B").
pub(crate) fn preset_id(preset_num: i32) -> String {
    format!("{:02}{}", bank_number(preset_num), slot_letter(preset_num))
}

/// Search: every whitespace-separated token of `query` (case-insensitive)
/// must occur in "id program-number ordinal bank slot name character
/// amp·cab" (e.g. "46c", "140" (PC, as the pedal shows it), "141",
/// "dg solo", "clean", "bank 14", "twin"), whatever id style the list
/// shows (the Naming global). An empty query matches everything.
pub(crate) fn preset_matches(preset_num: i32, meta: &PresetMeta, tone: &str, query: &str) -> bool {
    let bank = bank_number(preset_num);
    let hay = format!(
        "{} {:03} {preset_num:03} bank {bank:02} {bank} slot {} {} {} {tone}",
        preset_id(preset_num),
        preset_num - 1,
        slot_letter(preset_num),
        meta.name,
        meta.character
    )
    .to_lowercase();
    query.to_lowercase().split_whitespace().all(|t| hay.contains(t))
}

/// Colour group of a preset character (`character-color` in app.slint):
/// 0 none (no badge), 1 clean, 2 crunch, 3 drive, 4 high gain / lead,
/// 5 stomp (any "STOMP - ..."), 6 bass, 7 acoustic. Unrecognized → 0.
pub(crate) fn character_kind(character: &str) -> i32 {
    let c = character.to_uppercase();
    let has = |w: &str| c.contains(w);
    if c.is_empty() {
        0
    } else if c.starts_with("STOMP") {
        5
    } else if has("ACOUSTIC") {
        7
    } else if has("BASS") {
        6
    } else if has("HIGH GAIN") || has("LEAD") || has("METAL") {
        4
    } else if has("CRUNCH") {
        2
    } else if has("DRIVE") {
        3
    } else if has("CLEAN") {
        1
    } else {
        0
    }
}

impl PresetNames {
    pub(crate) fn lock(&self) -> MutexGuard<'_, Vec<PresetMeta>> {
        self.names.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn meta(&self, preset_num: i32) -> PresetMeta {
        let names = self.lock();
        usize::try_from(preset_num - 1)
            .ok()
            .and_then(|i| names.get(i))
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn row(preset_num: i32, meta: &PresetMeta, tone: &str) -> PresetRow {
        PresetRow {
            preset: preset_num,
            id: preset_id(preset_num).into(),
            name: meta.name.clone().into(),
            character: meta.character.clone().into(),
            kind: character_kind(&meta.character),
            tone: tone.into(),
        }
    }

    pub(crate) fn tone(&self, preset_num: i32) -> String {
        let tones = self.tones.lock().unwrap_or_else(|e| e.into_inner());
        usize::try_from(preset_num - 1).ok().and_then(|i| tones.get(i)).cloned().unwrap_or_default()
    }

    /// Stores every preset's "amp · cab" line and updates the shown rows
    /// whose line changed, in place (the list keeps its scroll position).
    pub(crate) fn set_tones(&self, ui: &AppWindow, lines: Vec<String>, search: Vec<String>) {
        *self.tones.lock().unwrap_or_else(|e| e.into_inner()) = lines;
        *self.tone_search.lock().unwrap_or_else(|e| e.into_inner()) = search;
        let model = ui.get_preset_names();
        for row in 0..model.row_count() {
            let Some(data) = model.row_data(row) else { continue };
            let tone = self.tone(data.preset);
            if data.tone.as_str() != tone {
                model.set_row_data(row, PresetRow { tone: tone.into(), ..data });
            }
        }
    }

    /// True once any preset name has been read (names survive reconnects).
    pub(crate) fn loaded(&self) -> bool {
        self.lock().iter().any(|m| !m.name.is_empty())
    }

    pub(crate) fn set_query(&self, query: &str) {
        *self.query.lock().unwrap_or_else(|e| e.into_inner()) = query.to_string();
    }

    /// Rows of the presets matching the current search query.
    pub(crate) fn list_model(&self) -> ModelRc<PresetRow> {
        let query = self.query.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let names = self.lock();
        let tones = self.tones.lock().unwrap_or_else(|e| e.into_inner());
        let search = self.tone_search.lock().unwrap_or_else(|e| e.into_inner());
        let empty = PresetMeta::default();
        let rows: Vec<PresetRow> = (MIN_PRESET..=MAX_PRESET)
            .filter_map(|p| {
                let meta = names.get((p - 1) as usize).unwrap_or(&empty);
                let tone = tones.get((p - 1) as usize).map_or("", String::as_str);
                let extra = search.get((p - 1) as usize).map_or("", String::as_str);
                preset_matches(p, meta, &format!("{tone} {extra}"), &query).then(|| Self::row(p, meta, tone))
            })
            .collect();
        ModelRc::new(VecModel::from(rows))
    }

    /// Stores a preset's character (from a details read) and updates its
    /// list row in place if it is shown (the filter itself is re-applied
    /// on the next query change). Returns false if nothing changed.
    pub(crate) fn set_character(&self, ui: &AppWindow, preset_idx: u8, character: String) -> bool {
        let i = usize::from(preset_idx);
        let meta = {
            let mut names = self.lock();
            if names.len() < PRESET_COUNT {
                names.resize(PRESET_COUNT, PresetMeta::default());
            }
            if names[i].character == character {
                return false;
            }
            names[i].character = character;
            names[i].clone()
        };
        let model = ui.get_preset_names();
        let preset = i as i32 + 1;
        if let Some(row) = model.iter().position(|r| r.preset == preset) {
            model.set_row_data(row, Self::row(preset, &meta, &self.tone(preset)));
        }
        true
    }

    /// Stores a preset's new name (after a verified rename) and updates its
    /// list row in place if it is shown.
    pub(crate) fn set_name(&self, ui: &AppWindow, preset_idx: u8, name: String) {
        let i = usize::from(preset_idx);
        let meta = {
            let mut names = self.lock();
            if names.len() < PRESET_COUNT {
                names.resize(PRESET_COUNT, PresetMeta::default());
            }
            names[i].name = name;
            names[i].clone()
        };
        let model = ui.get_preset_names();
        let preset = i as i32 + 1;
        if let Some(row) = model.iter().position(|r| r.preset == preset) {
            model.set_row_data(row, Self::row(preset, &meta, &self.tone(preset)));
        }
    }

    pub(crate) fn name_of(&self, preset: i32) -> String {
        self.meta(preset).name
    }
}

/// AMP tab header line: the amp the model is based on ("Fender Twin
/// Reverb"), or the model's own name when that isn't stored.
pub(crate) fn amp_model_text(amp: &protocol::AmpModel) -> String {
    amp.based_on.clone().unwrap_or_else(|| amp.name.clone())
}

/// CAB selector text for the cab slot: "Polite Twin" (the AMP model's own
/// cab, "Model cab" while its name is unknown), "IR · <name>", or a borrowed cab's "<model> ·
/// <based on>".
pub(crate) fn cab_source_text(cab: &protocol::CabSource, amp: Option<&protocol::AmpModel>) -> String {
    use protocol::CabSource::*;
    match cab {
        ModelCab => amp.map_or_else(|| "Model cab".into(), |a| a.name.clone()),
        Ir(name) => format!("IR · {name}"),
        Borrowed { name, based_on: Some(b) } => format!("{name} · {b}"),
        Borrowed { name, based_on: None } => name.clone(),
    }
}

/// How long after our own preset select the pedal's summary push for that
/// preset is taken as its echo.
pub(crate) const SELECT_ECHO_GUARD: Duration = Duration::from_secs(2);

/// How long after a rename a pushed preset change for the renamed preset
/// is taken as an echo of the write, not as a footswitch change.
pub(crate) const RENAME_ECHO_GUARD: Duration = Duration::from_secs(3);

/// OLED display: name ("" until known; bank/slot are derived in the UI)
/// and character chip of `preset`.
pub(crate) fn show_preset_meta(ui: &AppWindow, names: &PresetNames, preset: i32) {
    let meta = names.meta(preset);
    ui.set_preset_name(meta.name.into());
    let character = meta.character;
    ui.set_preset_character_kind(character_kind(&character));
    ui.set_preset_character(character.into());
}

/// Row of `preset` in the (filtered) list, -1 when filtered out.
pub(crate) fn list_row_of(ui: &AppWindow, preset: i32) -> i32 {
    ui.get_preset_names()
        .iter()
        .position(|r| r.preset == preset)
        .map_or(-1, |i| i as i32)
}

/// (Re)builds the preset list for the current query: rows, match count and
/// the current preset's row.
pub(crate) fn show_list(ui: &AppWindow, names: &PresetNames) {
    let model = names.list_model();
    ui.set_preset_count(model.row_count() as i32);
    ui.set_preset_names(model);
    ui.set_selected_row(list_row_of(ui, ui.get_preset()));
}

/// Shows `preset` as current in the UI. Does NOT send MIDI (used both by the
/// UI path, which sends separately, and for footswitch changes on the pedal).
pub(crate) fn show_preset(ui: &AppWindow, names: &PresetNames, preset: i32) {
    // Row first: the list scrolls to it when `preset` changes.
    ui.set_selected_row(list_row_of(ui, preset));
    ui.set_preset(preset);
    // Slot names of the previous preset; the new ones follow its dump read.
    ui.set_amp_model_text("".into());
    ui.set_cab_source_text("".into());
    ui.set_cab_slot_type(-1);
    // Until the new preset's details arrive.
    ui.set_eq_freq_known(false);
    ui.set_amp_mix_known(false);
    ui.set_eq_mid_q_known(false);
    ui.set_eq_post_known(false);
    show_preset_meta(ui, names, preset);
    // A (re)loaded preset starts clean.
    ui.set_preset_dirty(false);
}

/// The pedal's summary push for `preset_idx` right after our own select of
/// it (see SELECT_ECHO_GUARD).
pub(crate) fn is_select_echo(echo: Option<(u8, Instant)>, preset_idx: u8, now: Instant) -> bool {
    echo.is_some_and(|(i, t)| i == preset_idx && now.saturating_duration_since(t) < SELECT_ECHO_GUARD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn character_kinds() {
        assert_eq!(character_kind("CLEAN"), 1);
        assert_eq!(character_kind("Crunch"), 2);
        assert_eq!(character_kind("DRIVE"), 3);
        assert_eq!(character_kind("HIGH GAIN"), 4);
        assert_eq!(character_kind("STOMP - OVERDRIVE"), 5);
        assert_eq!(character_kind("BASS"), 6);
        assert_eq!(character_kind("ACOUSTIC"), 7);
        assert_eq!(character_kind(""), 0);
        assert_eq!(character_kind("SOMETHING ELSE"), 0);
    }

    #[test]
    fn preset_id_uses_zero_based_bank_and_slot() {
        assert_eq!(preset_id(1), "00A");
        assert_eq!(preset_id(125), "41B");
        assert_eq!(preset_id(150), "49C");
    }

    #[test]
    fn search_matches_all_tokens_case_insensitively() {
        let meta = PresetMeta { name: "DG Comfortably solo LPR".into(), character: "DRIVE".into() };
        // Preset 141 = bank 46, slot C.
        assert!(preset_matches(141, &meta, "", ""));
        assert!(preset_matches(141, &meta, "", "46c"));
        assert!(preset_matches(141, &meta, "", "141"));
        assert!(preset_matches(7, &meta, "", "007"));
        // PC number as the pedal shows it in PC+Name mode (0-based).
        assert!(preset_matches(84, &meta, "", "083"));
        assert!(preset_matches(1, &meta, "", "000"));
        assert!(preset_matches(141, &meta, "", "dg SOLO"));
        assert!(preset_matches(141, &meta, "", "  drive  bank 46 "));
        assert!(!preset_matches(141, &meta, "", "dg clean"));
        assert!(!preset_matches(141, &meta, "", "45c"));
        // The "amp · cab" line is searchable too.
        assert!(preset_matches(141, &meta, "Fender Twin Reverb · Polite Twin", "twin dg"));
        assert!(!preset_matches(141, &meta, "", "twin"));
    }

    #[test]
    fn slot_texts() {
        use protocol::{AmpModel, CabSource};
        assert_eq!(
            amp_model_text(&AmpModel { name: "Ratfish".into(), based_on: Some("ProCo Rat".into()) }),
            "ProCo Rat"
        );
        assert_eq!(amp_model_text(&AmpModel { name: "Solo".into(), based_on: None }), "Solo");
        assert_eq!(cab_source_text(&CabSource::ModelCab, None), "Model cab");
        let twin = protocol::AmpModel { name: "Polite Twin".into(), based_on: Some("Fender Twin Reverb".into()) };
        assert_eq!(cab_source_text(&CabSource::ModelCab, Some(&twin)), "Polite Twin");
        assert_eq!(cab_source_text(&CabSource::Ir("B.Alnico 2".into()), None), "IR · B.Alnico 2");
        assert_eq!(
            cab_source_text(&CabSource::Borrowed { name: "51st Hi".into(), based_on: Some("Peavey 5150".into()) }, None),
            "51st Hi · Peavey 5150"
        );
    }

    #[test]
    fn select_echo_window() {
        let t0 = Instant::now();
        assert!(is_select_echo(Some((42, t0)), 42, t0 + Duration::from_millis(500)));
        assert!(!is_select_echo(Some((42, t0)), 43, t0), "another preset");
        assert!(!is_select_echo(Some((42, t0)), 42, t0 + SELECT_ECHO_GUARD), "too late");
        assert!(!is_select_echo(None, 42, t0));
    }
}
