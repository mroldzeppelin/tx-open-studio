//! The tone catalog: what each item of the pedal's internal library is
//! (name, based-on, kind), learned from slot reads and catalog-scan
//! reads (`0x0209`), cached in `catalog.json`. Never any model data: the pedal loads
//! items by id. Items without names yet show placeholders by their library
//! position ("Amp 7", "Cab 7") and stay selectable.

mod menu;

pub(crate) use menu::*;

use crate::protocol::{is_model_kind, LibraryId, LibraryItem, SlotInfo, KIND_IR};
use crate::store;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub(crate) const CATALOG_VERSION: u32 = 1;
pub(crate) const CATALOG_FILE: &str = "catalog.json";

/// What one library item is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct CatalogItem {
    pub(crate) name: String,
    pub(crate) based_on: Option<String>,
    pub(crate) kind: u16,
    /// Unix seconds.
    pub(crate) first_seen: u64,
}

impl CatalogItem {
    pub(crate) fn is_ir(&self) -> bool {
        self.kind == KIND_IR
    }
}

#[derive(Serialize, Deserialize)]
struct ListEntry {
    position: u16,
    id: String,
}

#[derive(Serialize, Deserialize)]
struct CatalogFile {
    version: u32,
    /// The pedal's library list (`0x0208`) the items were learned against.
    list: Vec<ListEntry>,
    /// Library id (hex) → item.
    items: BTreeMap<String, CatalogItem>,
}

#[derive(Default)]
pub(crate) struct Catalog {
    pub(crate) list: Vec<LibraryItem>,
    pub(crate) items: BTreeMap<LibraryId, CatalogItem>,
    /// None: in memory only (tests).
    path: Option<PathBuf>,
}

impl Catalog {
    /// The cached catalog at `path`, or an empty one (missing, malformed or
    /// unknown version: rebuilt).
    pub(crate) fn load(path: PathBuf) -> Self {
        let mut catalog = store::load_json::<CatalogFile>(&path).and_then(Self::from_file).unwrap_or_default();
        catalog.path = Some(path);
        catalog
    }

    fn from_file(file: CatalogFile) -> Option<Self> {
        if file.version != CATALOG_VERSION {
            return None;
        }
        let list = file.list.iter().filter_map(|e| Some(LibraryItem { position: e.position, id: store::parse_hex_id(&e.id)? })).collect();
        let items = file.items.into_iter().filter_map(|(k, v)| Some((store::parse_hex_id(&k)?, v))).collect();
        Some(Self { list, items, path: None })
    }

    fn to_file(&self) -> CatalogFile {
        CatalogFile {
            version: CATALOG_VERSION,
            list: self.list.iter().map(|i| ListEntry { position: i.position, id: store::hex_id(&i.id) }).collect(),
            items: self.items.iter().map(|(k, v)| (store::hex_id(k), v.clone())).collect(),
        }
    }

    pub(crate) fn save(&self) {
        if let Some(path) = &self.path {
            store::save_json(path, &self.to_file());
        }
    }

    /// The pedal's current library list; true if it changed.
    pub(crate) fn set_list(&mut self, list: Vec<LibraryItem>) -> bool {
        let changed = self.list != list;
        self.list = list;
        changed
    }

    /// Learns (or corrects) an item from a filled slot; true if changed.
    pub(crate) fn merge(&mut self, info: &SlotInfo, now: u64) -> bool {
        let first_seen = self.items.get(&info.id).map_or(now, |i| i.first_seen);
        let item = CatalogItem { name: info.name.clone(), based_on: info.based_on.clone(), kind: info.kind, first_seen };
        if self.items.get(&info.id) == Some(&item) {
            return false;
        }
        self.items.insert(info.id, item);
        true
    }

    pub(crate) fn item(&self, id: &LibraryId) -> Option<&CatalogItem> {
        self.items.get(id)
    }

    /// 1-based library position of `id` (placeholders, ordering).
    pub(crate) fn number(&self, id: &LibraryId) -> Option<u16> {
        self.list.iter().find(|i| i.id == *id).map(|i| i.position + 1)
    }

    /// "<prefix> <library position>" for an item without a name.
    pub(crate) fn placeholder(&self, id: &LibraryId, prefix: &str) -> Option<String> {
        self.number(id).map(|n| format!("{prefix} {n}"))
    }

    /// Listed items without a catalog entry, in list order.
    pub(crate) fn missing(&self) -> Vec<LibraryItem> {
        self.list.iter().filter(|i| !self.items.contains_key(&i.id)).copied().collect()
    }

    /// (listed items with a catalog entry, listed items).
    pub(crate) fn progress(&self) -> (usize, usize) {
        (self.list.len() - self.missing().len(), self.list.len())
    }

    /// Kind to load `id` with: the catalog's, else a tone model (the
    /// pedal fills in the rest either way).
    pub(crate) fn load_kind(&self, id: &LibraryId) -> u16 {
        self.item(id).map_or(crate::protocol::KIND_AMP_MODEL, |i| i.kind)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn info(id: u8, kind: u16, name: &str, based_on: Option<&str>) -> SlotInfo {
        SlotInfo { id: [id; 16], kind, name: name.into(), based_on: based_on.map(Into::into) }
    }

    pub(crate) fn listed(ids: &[u8]) -> Vec<LibraryItem> {
        ids.iter().enumerate().map(|(i, &id)| LibraryItem { position: i as u16 * 2, id: [id; 16] }).collect()
    }

    #[test]
    fn merge_dedupes_and_keeps_first_seen() {
        let mut c = Catalog::default();
        assert!(c.merge(&info(1, 4, "Polite Twin", Some("Fender Twin Reverb")), 100));
        assert!(!c.merge(&info(1, 4, "Polite Twin", Some("Fender Twin Reverb")), 200), "same item again");
        assert!(c.merge(&info(1, 4, "Polite Twin 2", Some("Fender Twin Reverb")), 300), "renamed");
        assert_eq!(c.items.len(), 1);
        assert_eq!(c.item(&[1; 16]).unwrap().first_seen, 100);
    }

    #[test]
    fn file_round_trip_and_rebuild() {
        let dir = std::env::temp_dir().join(format!("txos-catalog-test-{}", std::process::id()));
        let path = dir.join(CATALOG_FILE);
        let mut c = Catalog::load(path.clone());
        assert!(c.items.is_empty(), "no file yet");
        c.set_list(listed(&[1, 2, 3, 4]));
        c.merge(&info(2, 3, "Alnico 412", None), 5);
        c.merge(&info(4, 4, "Plæxi Driver", Some("Marshall Super Lead MKII")), 6);
        c.save();
        let back = Catalog::load(path.clone());
        assert_eq!(back.list, c.list);
        assert_eq!(back.items, c.items);
        assert_eq!(back.missing().iter().map(|i| i.id).collect::<Vec<_>>(), vec![[1; 16], [3; 16]]);
        assert_eq!(back.progress(), (2, 4));
        assert_eq!(back.item(&[4; 16]).unwrap().name, "Plæxi Driver", "UTF-8 survives the file");
        assert_eq!(back.placeholder(&[3; 16], "Amp").as_deref(), Some("Amp 5"));
        // Unknown version or garbage: ignored, rebuilt from scratch.
        let text = std::fs::read_to_string(&path).unwrap().replace("\"version\": 1", "\"version\": 99");
        std::fs::write(&path, text).unwrap();
        assert!(Catalog::load(path.clone()).items.is_empty());
        std::fs::write(&path, "garbage").unwrap();
        assert!(Catalog::load(path).list.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
