//! The preset list's "amp · cab" line: per-preset slot summaries (from
//! full dumps: startup pass, preset loads, our saves/renames/loads) cached
//! in `presets.json`, named through the catalog.

use crate::catalog::Catalog;
use crate::fx::cab_model_options;
use crate::protocol::{is_model_kind, LibraryId, PresetSlots, KIND_EMPTY, KIND_IR};
use crate::store;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub(crate) const PRESETS_VERSION: u32 = 1;
pub(crate) const PRESETS_FILE: &str = "presets.json";

/// One preset's slots and cab params (library ids as hex).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct PresetEntry {
    pub(crate) amp_id: Option<String>,
    pub(crate) amp_kind: u16,
    pub(crate) cab_id: Option<String>,
    pub(crate) cab_kind: u16,
    /// Contents `/2`.
    pub(crate) cab_flag: Option<u16>,
    /// Params 24 (cab type) and 25 (VIR cab model).
    pub(crate) p24: Option<f32>,
    pub(crate) p25: Option<f32>,
    /// Unix seconds.
    pub(crate) updated_at: u64,
}

impl PresetEntry {
    pub(crate) fn from_slots(s: &PresetSlots, now: u64) -> Self {
        Self {
            amp_id: s.amp_id.as_ref().map(store::hex_id),
            amp_kind: s.amp_kind,
            cab_id: s.cab_id.as_ref().map(store::hex_id),
            cab_kind: s.cab_kind,
            cab_flag: s.cab_flag,
            p24: s.cab_type,
            p25: s.cab_model,
            updated_at: now,
        }
    }

    fn same_content(&self, other: &Self) -> bool {
        Self { updated_at: 0, ..self.clone() } == Self { updated_at: 0, ..other.clone() }
    }
}

#[derive(Serialize, Deserialize)]
struct PresetsFile {
    version: u32,
    /// 0-based preset index → entry.
    presets: BTreeMap<u8, PresetEntry>,
}

#[derive(Default)]
pub(crate) struct PresetCache {
    pub(crate) entries: BTreeMap<u8, PresetEntry>,
    path: Option<PathBuf>,
}

impl PresetCache {
    pub(crate) fn load(path: PathBuf) -> Self {
        let entries = store::load_json::<PresetsFile>(&path)
            .filter(|f| f.version == PRESETS_VERSION)
            .map(|f| f.presets)
            .unwrap_or_default();
        Self { entries, path: Some(path) }
    }

    pub(crate) fn save(&self) {
        if let Some(path) = &self.path {
            store::save_json(path, &PresetsFile { version: PRESETS_VERSION, presets: self.entries.clone() });
        }
    }

    /// Stores preset `idx`'s summary; true if its content changed.
    pub(crate) fn set(&mut self, idx: u8, slots: &PresetSlots, now: u64) -> bool {
        let entry = PresetEntry::from_slots(slots, now);
        let changed = self.entries.get(&idx).is_none_or(|e| !e.same_content(&entry));
        self.entries.insert(idx, entry);
        changed
    }

    /// The list line of preset `idx` ("" when unknown).
    pub(crate) fn line(&self, idx: u8, catalog: &Catalog) -> String {
        self.entries.get(&idx).map(|e| tone_line(e, catalog)).unwrap_or_default()
    }

    /// Search-only words of preset `idx` (based-on strings).
    pub(crate) fn search_extra(&self, idx: u8, catalog: &Catalog) -> String {
        self.entries.get(&idx).map(|e| tone_search_extra(e, catalog)).unwrap_or_default()
    }
}

fn id_of(hex: &Option<String>) -> Option<LibraryId> {
    hex.as_deref().and_then(store::parse_hex_id)
}

/// "amp · cab" by name, as the pickers show them: amp = the AMP model's
/// name (else its placeholder); cab by param 24: 1 → the VIR cab (param
/// 25), 2 → "no cab", 0 → the CAB slot: a tone model → its name, an IR →
/// "IR <name>", empty (the model's own cab) → nothing, so the line is just
/// the amp name instead of repeating it.
pub(crate) fn tone_line(e: &PresetEntry, catalog: &Catalog) -> String {
    let amp_id = id_of(&e.amp_id).filter(|_| e.amp_kind != KIND_EMPTY);
    let amp_item = amp_id.as_ref().and_then(|id| catalog.item(id));
    let amp_placeholder = amp_id.as_ref().and_then(|id| catalog.placeholder(id, "Amp"));
    let amp = amp_item.map(|i| i.name.clone()).or_else(|| amp_placeholder.clone()).unwrap_or_default();
    let cab = match e.p24.map(|v| v.round() as i32) {
        Some(1) => {
            let model = e.p25.map_or(0, |v| v.round().max(0.0) as usize);
            cab_model_options(model).get(model).cloned().unwrap_or_default()
        }
        Some(2) => "no cab".into(),
        _ => match id_of(&e.cab_id).filter(|_| e.cab_kind != KIND_EMPTY) {
            None => String::new(),
            Some(id) => {
                let item = catalog.item(&id);
                let ir = item.map_or(e.cab_kind == KIND_IR, |i| i.is_ir() || !is_model_kind(i.kind));
                match (item, ir) {
                    (Some(i), true) => format!("IR {}", i.name),
                    (Some(i), false) => i.name.clone(),
                    (None, _) => catalog.placeholder(&id, "Cab").unwrap_or_default(),
                }
            }
        },
    };
    [amp, cab].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")
}

/// Extra search words for a preset: what its AMP and CAB slot items are
/// based on (not shown in the list line).
pub(crate) fn tone_search_extra(e: &PresetEntry, catalog: &Catalog) -> String {
    [&e.amp_id, &e.cab_id]
        .into_iter()
        .filter_map(|id| id_of(id).and_then(|id| catalog.item(&id)).and_then(|i| i.based_on.clone()))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::tests::{info, listed};

    fn slots(amp: u8, cab: Option<(u8, u16)>, p24: f32, p25: f32) -> PresetSlots {
        PresetSlots {
            amp_id: Some([amp; 16]),
            amp_kind: 4,
            cab_id: cab.map(|(id, _)| [id; 16]),
            cab_kind: cab.map_or(0, |(_, k)| k),
            cab_flag: Some(u16::from(cab.is_some())),
            cab_type: Some(p24),
            cab_model: Some(p25),
        }
    }

    #[test]
    fn line_by_cab_type() {
        let mut c = Catalog::default();
        c.set_list(listed(&[1, 2, 3, 4]));
        c.merge(&info(1, 4, "Polite Twin", Some("Fender Twin Reverb")), 0);
        c.merge(&info(2, 4, "Breaking Thunder", Some("Orange Thunderverb 200")), 0);
        c.merge(&info(3, 3, "Alnico 412", None), 0);
        let line = |s: PresetSlots| tone_line(&PresetEntry::from_slots(&s, 0), &c);
        assert_eq!(line(slots(1, None, 0.0, 0.0)), "Polite Twin", "own model cab: amp name only");
        assert_eq!(line(slots(1, Some((2, 4)), 0.0, 0.0)), "Polite Twin · Breaking Thunder");
        assert_eq!(line(slots(1, Some((3, 3)), 0.0, 0.0)), "Polite Twin · IR Alnico 412");
        assert_eq!(line(slots(1, Some((2, 4)), 1.0, 33.0)), "Polite Twin · 412 GRN", "VIR wins over the slot");
        assert_eq!(line(slots(1, None, 2.0, 0.0)), "Polite Twin · no cab");
        // Not scanned yet: placeholders by library position.
        assert_eq!(line(slots(4, Some((4, 4)), 0.0, 0.0)), "Amp 7 · Cab 7");
        assert_eq!(line(slots(4, None, 0.0, 0.0)), "Amp 7");
        // Based-on strings are search words only.
        let entry = PresetEntry::from_slots(&slots(1, Some((2, 4)), 0.0, 0.0), 0);
        assert_eq!(tone_search_extra(&entry, &c), "Fender Twin Reverb Orange Thunderverb 200");
        // Unknown preset: empty line.
        assert_eq!(PresetCache::default().line(3, &c), "");
    }

    #[test]
    fn cache_round_trip_and_change_detection() {
        let dir = std::env::temp_dir().join(format!("txos-presets-test-{}", std::process::id()));
        let path = dir.join(PRESETS_FILE);
        let mut cache = PresetCache::load(path.clone());
        assert!(cache.set(5, &slots(1, None, 0.0, 0.0), 10));
        assert!(!cache.set(5, &slots(1, None, 0.0, 0.0), 20), "same content, newer time");
        assert!(cache.set(5, &slots(1, None, 1.0, 3.0), 30));
        cache.save();
        let back = PresetCache::load(path.clone());
        assert_eq!(back.entries, cache.entries);
        std::fs::write(&path, "{\"version\":2,\"presets\":{}}").unwrap();
        assert!(PresetCache::load(path).entries.is_empty(), "unknown version: rebuilt");
        let _ = std::fs::remove_dir_all(dir);
    }
}
