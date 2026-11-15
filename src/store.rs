//! The app's data folder (`$XDG_DATA_HOME` or `~/.local/share`, then
//! `tx-open-studio/`): JSON caches only, written atomically. Any file that
//! doesn't parse (or has an unknown version) is ignored and rebuilt.

use crate::protocol::LibraryId;
use serde::de::DeserializeOwned;
use std::path::Path;
use std::path::PathBuf;

pub(crate) fn data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("tx-open-studio"))
}

/// Writes `bytes` to a temporary file next to `path`, then renames it over
/// `path` (a reader never sees a half-written file).
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Serializes `value` and writes it atomically; failures are logged only
/// (the file is a cache).
pub(crate) fn save_json(path: &Path, value: &impl serde::Serialize) {
    let result = serde_json::to_vec_pretty(value).map_err(|e| e.to_string()).and_then(|b| write_atomic(path, &b).map_err(|e| e.to_string()));
    if let Err(e) = result {
        eprintln!("TX Open Studio: writing {} failed: {e}", path.display());
    }
}

/// The parsed file, or None (missing, unreadable, malformed).
pub(crate) fn load_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn hex_id(id: &LibraryId) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn parse_hex_id(s: &str) -> Option<LibraryId> {
    if s.len() != 32 || !s.is_ascii() {
        return None;
    }
    let mut id = [0u8; 16];
    for (i, b) in id.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(id)
}

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_ids_round_trip() {
        let id: LibraryId = core::array::from_fn(|i| (i * 17) as u8);
        assert_eq!(parse_hex_id(&hex_id(&id)), Some(id));
        assert_eq!(parse_hex_id("00"), None);
        assert_eq!(parse_hex_id(&"zz".repeat(16)), None);
    }

    #[test]
    fn atomic_write_replaces_the_file() {
        let dir = std::env::temp_dir().join(format!("txos-store-test-{}", std::process::id()));
        let path = dir.join("x.json");
        save_json(&path, &vec![1, 2, 3]);
        save_json(&path, &vec![4]);
        assert_eq!(load_json::<Vec<u8>>(&path), Some(vec![4]));
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(load_json::<Vec<u8>>(&path), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
