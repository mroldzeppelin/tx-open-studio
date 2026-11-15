//! Full preset dump and live edit buffer: layout checks, the name buffer, rename/save bodies and their verification, amp/cab slot names.

use super::*;

/// The name is the 33-byte buffer (ASCII, NUL-terminated, zero-filled)
/// right after the first `BC 21` of a dump body, which must lie within
/// the first `NAME_TAG_WINDOW` bytes.
pub(crate) const NAME_TAG: [u8; 2] = [0xBC, 0x21];

pub(crate) const NAME_BUF_LEN: usize = 33;

pub(crate) const NAME_TAG_WINDOW: usize = 64;

/// Offset of the name buffer in a dump body.
pub(crate) fn name_buf_offset(body: &[u8]) -> Result<usize, String> {
    let window = &body[..body.len().min(NAME_TAG_WINDOW)];
    let start = find(window, &NAME_TAG).ok_or("name field not found")? + NAME_TAG.len();
    if body.len() < start + NAME_BUF_LEN {
        return Err("dump body too short".into());
    }
    Ok(start)
}

pub(crate) fn name_buffer(body: &[u8]) -> Result<&[u8], String> {
    let start = name_buf_offset(body)?;
    Ok(&body[start..start + NAME_BUF_LEN])
}

/// The body of a full dump reply for preset `idx`, after the layout checks
/// a write relies on: type 0x0203, header size == body length, body starts
/// `B9 04 01 00 <idx>`, name field near the start. Anything else: Err.
pub(crate) fn dump_body(payload: &[u8], idx: u8) -> Result<&[u8], String> {
    let body = reply_body(payload)?;
    let mut start = vec![0xB9, 0x04, 0x01, 0x00];
    start.extend(index_bytes(idx));
    if !body.starts_with(&start) {
        return Err(format!("dump body is not for preset index {idx}"));
    }
    name_buf_offset(body)?;
    Ok(body)
}

/// A name as typed, reduced to what the pedal stores: printable ASCII
/// (0x20..=0x7E) only, at most 32 bytes. Not trimmed (for live typing).
pub fn filter_name(input: &str) -> String {
    input.chars().filter(|c| (' '..='~').contains(c)).take(NAME_LEN).collect()
}

/// A name ready to write: `filter_name`, then trimmed.
pub fn clean_name(input: &str) -> String {
    filter_name(input).trim().to_string()
}

/// `body` with the name buffer set to `name` (cleaned), zero-filled.
pub(crate) fn patch_dump_name(body: &[u8], name: &str) -> Result<Vec<u8>, String> {
    let name = clean_name(name);
    if name.is_empty() {
        return Err("empty name".into());
    }
    let start = name_buf_offset(body)?;
    let mut out = body.to_vec();
    let buf = &mut out[start..start + NAME_BUF_LEN];
    buf.fill(0);
    buf[..name.len()].copy_from_slice(name.as_bytes());
    Ok(out)
}

/// Amp (tone model) slot: model name and what it's based on.
#[derive(Clone, Debug, PartialEq)]
pub struct AmpModel {
    pub name: String,
    pub based_on: Option<String>,
}

/// What the cab slot holds.
#[derive(Clone, Debug, PartialEq)]
pub enum CabSource {
    /// Empty cab slot: the amp slot's tone model plays its own cab.
    ModelCab,
    /// A custom IR (its name).
    Ir(String),
    /// The cab of another tone model.
    Borrowed { name: String, based_on: Option<String> },
}

/// A slot's content, by its content-kind int (same meaning in either
/// slot; hardware-checked on 6 presets): 0 empty (the slot keeps stale
/// data from earlier use, never read), 1 / 4 tone model (1 seen for a
/// stomp capture, 4 for amp captures; 2 treated alike, unseen), 3 custom
/// IR.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SlotContent {
    Empty,
    Model { name: String, based_on: Option<String> },
    Ir(String),
}

/// Amp and cab slot names of a preset, from its full dump.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToneInfo {
    pub amp: Option<AmpModel>,
    pub cab: Option<CabSource>,
    /// Library ids of the AMP model and of the CAB slot's content (None
    /// for an empty slot).
    pub amp_id: Option<LibraryId>,
    pub cab_id: Option<LibraryId>,
}

/// Longest slot string read, in characters (names are short; garbage
/// follows the NUL).
pub(crate) const SLOT_TEXT_MAX: usize = 64;

/// Bytes searched for the NUL (UTF-8: up to 4 bytes per character).
const SLOT_TEXT_SCAN: usize = 4 * SLOT_TEXT_MAX;

/// UTF-8 text up to the first NUL (lossy on invalid sequences, at most
/// `SLOT_TEXT_MAX` characters), trimmed; None if empty or containing a
/// control character. Library names use UTF-8 ("Plæxi Driver").
pub(crate) fn slot_text(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(SLOT_TEXT_SCAN)];
    let raw = &head[..head.iter().position(|&b| b == 0).unwrap_or(head.len())];
    let decoded = String::from_utf8_lossy(raw);
    let text: String = decoded.chars().take(SLOT_TEXT_MAX).collect();
    if text.chars().any(char::is_control) {
        return None;
    }
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// A slot `[id blob, [name, name], kind, data blob, …]` → its content.
/// Model: name buffer 0, based-on = data after 4 header bytes; IR: data
/// from byte 0. Unknown kind or unreadable name: None.
pub(crate) fn slot_content(slot: &Node<'_>) -> Option<SlotContent> {
    let items = slot.items()?;
    let kind = items.get(2)?.int()?;
    if kind == 0 {
        return Some(SlotContent::Empty);
    }
    let data = items.get(3)?.blob()?;
    match kind {
        1 | 2 | 4 => Some(SlotContent::Model {
            name: items.get(1)?.at(&[0])?.blob().and_then(slot_text)?,
            based_on: data.get(4..).and_then(slot_text),
        }),
        3 => slot_text(data).map(SlotContent::Ir),
        _ => None,
    }
}

/// Names from a full dump body: body = [mode, 0, idx, contents], contents =
/// [details, amp slot, int, cab slot]. The amp line shows a tone model
/// only; the cab slot maps empty → the model's own cab, IR → IR, tone model
/// → borrowed cab. Unknown or malformed parts: that part is None. A body
/// that doesn't parse: None.
pub fn tone_info(body: &[u8]) -> Option<ToneInfo> {
    let tree = parse_tree(body)?;
    let contents = tree.at(&[3])?;
    let amp = match contents.at(&[1]).and_then(slot_content) {
        Some(SlotContent::Model { name, based_on }) => Some(AmpModel { name, based_on }),
        _ => None,
    };
    let cab = contents.at(&[3]).and_then(slot_content).map(|c| match c {
        SlotContent::Empty => CabSource::ModelCab,
        SlotContent::Ir(name) => CabSource::Ir(name),
        SlotContent::Model { name, based_on } => CabSource::Borrowed { name, based_on },
    });
    let amp_id = amp.as_ref().and(contents.at(&[1])).and_then(slot_id);
    let cab_id = match cab {
        Some(CabSource::ModelCab) | None => None,
        Some(_) => contents.at(&[3]).and_then(slot_id),
    };
    Some(ToneInfo { amp, cab, amp_id, cab_id })
}

/// Body of a type-0x0203 reply whose header size matches its length.
pub(crate) fn reply_body(payload: &[u8]) -> Result<&[u8], String> {
    if frame_type(payload) != Some(TYPE_PRESET_DUMP) {
        return Err("not a preset dump".into());
    }
    let mut pos = 2;
    read_u16_value(payload, &mut pos).ok_or("bad dump header")?; // type
    let size = read_u16_value(payload, &mut pos).ok_or("bad dump header size")?;
    read_u16_value(payload, &mut pos).ok_or("bad dump header")?; // `10`
    let body = payload.get(pos..).ok_or("dump too short")?;
    if body.len() != usize::from(size) {
        return Err(format!("dump body is {} bytes, header says {size}", body.len()));
    }
    Ok(body)
}

/// The live edit buffer read: body `B9 04 00 <dirty> <idx> <contents>`
/// (field 1 = 00 live, 01 in stored dumps; field 2 = the pedal's dirty
/// flag). Returns the body, the active preset index and the dirty flag,
/// after checking the whole body parses as one tree.
pub(crate) fn live_body(payload: &[u8]) -> Result<(&[u8], u8, bool), String> {
    let body = reply_body(payload)?;
    let mut pos = 0;
    let tree = parse_node(body, &mut pos, 0).ok_or("live buffer does not parse")?;
    if pos != body.len() {
        return Err("live buffer has trailing bytes".into());
    }
    let field = |i: usize| tree.at(&[i]).and_then(|n| n.int());
    if field(0) != Some(0) {
        return Err("not the live edit buffer".into());
    }
    let dirty = match field(1) {
        Some(0) => false,
        Some(1) => true,
        _ => return Err("live buffer dirty flag unreadable".into()),
    };
    let idx = field(2).filter(|&i| usize::from(i) < PRESET_COUNT).ok_or("live buffer preset index unreadable")?;
    tree.at(&[3]).and_then(|n| n.items()).ok_or("live buffer contents missing")?;
    Ok((body, idx as u8, dirty))
}

/// Body to write for a save: the live buffer body with field 1 = 01
/// (stored) and field 2 = 00 (clean), as the official editor writes it.
/// Both are single-byte literals at body offsets 2 and 3; index and
/// contents stay untouched.
pub(crate) fn stored_body_from_live(live: &[u8]) -> Result<Vec<u8>, String> {
    match live {
        [0xB9, 0x04, 0x00, 0x00 | 0x01, ..] => {
            let mut out = live.to_vec();
            out[2] = 0x01;
            out[3] = 0x00;
            Ok(out)
        }
        _ => Err("unexpected live buffer header".into()),
    }
}

/// Byte range of the node at `path` (list indices) in `p`.
pub(crate) fn span_at(p: &[u8], path: &[usize]) -> Option<std::ops::Range<usize>> {
    let mut pos = 0;
    for &i in path {
        if !matches!(*p.get(pos)?, 0xB9 | 0xBA) {
            return None;
        }
        pos += 1;
        if usize::from(read_u16_value(p, &mut pos)?) <= i {
            return None;
        }
        for _ in 0..i {
            parse_node(p, &mut pos, 1)?;
        }
    }
    let start = pos;
    parse_node(p, &mut pos, 1)?;
    Some(start..pos)
}

/// A save is stored when the re-read stored body has, byte for byte, the
/// written details subtree (/3/0: name, the three parameter arrays,
/// metadata) and the same id blob, name buffers and content kind in the
/// AMP and CAB slots. The slots' data blobs are not compared: the active
/// preset's model data is legitimately re-encoded on write.
pub(crate) fn verify_saved(written: &[u8], stored: &[u8]) -> Result<(), String> {
    let same = |path: &[usize], what: &str| -> Result<(), String> {
        let w = span_at(written, path).ok_or_else(|| format!("{what}: not found in the written preset"))?;
        let s = span_at(stored, path).ok_or_else(|| format!("{what}: not found in the stored preset"))?;
        if written[w] == stored[s] {
            Ok(())
        } else {
            Err(format!("the pedal did not store the {what}"))
        }
    };
    same(&[3, 0], "preset details")?;
    for (slot, name) in [(1, "amp slot"), (3, "cab slot")] {
        for (part, what) in [(0, "id"), (1, "names"), (2, "kind")] {
            same(&[3, slot, part], &format!("{name} {what}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::test_util::*;

    #[test]
    fn live_buffer_request_and_header_rewrite() {
        assert_eq!(
            live_buffer_request(),
            [0xB9, 0x03, 0x81, 0x00, 0x02, 0x82, 0x06, 0x00, 0x80, 0x10, 0x03, 0xB9, 0x04, 0x10, 0x00, 0x00, 0x01]
        );
        for (idx, dirty) in [(5u16, 1u16), (140, 0)] {
            let live = enc_body_with(0, dirty, idx, amp_slot(), cab_slot());
            let payload = wrap_0203(&live);
            let (body, active, d) = live_body(&payload).unwrap();
            assert_eq!((active as u16, d), (idx, dirty == 1));
            // Rewrite: stored + clean, index (incl. `80 xx`) and contents untouched.
            let out = stored_body_from_live(body).unwrap();
            assert_eq!(&out[..4], &[0xB9, 0x04, 0x01, 0x00]);
            assert_eq!(&out[4..], &body[4..]);
            assert!(dump_body(&wrap_0203(&out), idx as u8).is_ok(), "reads as the stored preset");
        }
    }

    #[test]
    fn live_buffer_validation() {
        // A stored dump is not the live buffer.
        assert!(live_body(&wrap_0203(&enc_body(5, amp_slot(), cab_slot()))).is_err());
        // Length field mismatch.
        let mut p = wrap_0203(&enc_body_with(0, 1, 5, amp_slot(), cab_slot()));
        p.pop();
        assert!(live_body(&p).is_err());
        // Malformed tree (unknown tag inside) or trailing bytes.
        let mut body = enc_body_with(0, 1, 5, amp_slot(), cab_slot());
        let at = body.len() - 1;
        body[at] = 0x99;
        assert!(live_body(&wrap_0203(&body)).is_err());
        let mut trailing = enc_body_with(0, 1, 5, amp_slot(), cab_slot());
        trailing.push(0x00);
        assert!(live_body(&wrap_0203(&trailing)).is_err());
        // Dirty flag other than 0/1, preset index out of range.
        assert!(live_body(&wrap_0203(&enc_body_with(0, 2, 5, amp_slot(), cab_slot()))).is_err());
        assert!(live_body(&wrap_0203(&enc_body_with(0, 0, 150, amp_slot(), cab_slot()))).is_err());
        assert!(stored_body_from_live(&[0xB9, 0x04, 0x01, 0x00, 5]).is_err());
    }

    #[test]
    fn save_verification() {
        let live = enc_body_with(0, 1, 5, amp_slot(), cab_slot());
        let written = stored_body_from_live(&live).unwrap();
        // Stored as written.
        assert!(verify_saved(&written, &written).is_ok());
        // Model data re-encoded (inside the slots' data blobs): still fine.
        let mut reencoded = written.clone();
        let at = find(&reencoded, b"ProCo Rat").unwrap();
        reencoded[at] = b'X';
        assert!(verify_saved(&written, &reencoded).is_ok());
        // A parameter value not stored (details subtree differs).
        let mut param = written.clone();
        let at = find(&param, &3.0f32.to_le_bytes()).unwrap();
        param[at + 3] ^= 0x01;
        assert!(verify_saved(&written, &param).is_err());
        // A slot's kind or name differs.
        let other_kind = stored_body_from_live(&enc_body_with(0, 1, 5, amp_slot(), enc_slot("51st Hi", 1, &slot_data(b"\0\0\0\0Peavey 5150", 300)))).unwrap();
        assert!(verify_saved(&written, &other_kind).is_err());
        let other_name = stored_body_from_live(&enc_body_with(0, 1, 5, enc_slot("Ratfisx", 1, &slot_data(b"\x01\x02\x03\x04ProCo Rat", 300)), cab_slot())).unwrap();
        assert!(verify_saved(&written, &other_name).is_err());
        // Unparseable stored body.
        assert!(verify_saved(&written, &written[..20]).is_err());
    }

    #[test]
    fn tone_info_from_slots() {
        // Amp + own model cab (type 0).
        let body = enc_body(42, amp_slot(), enc_slot("", 0, &[0; 40]));
        assert!(dump_body(&{
            let mut p = vec![0xB9, 0x03, 0x81, 0x03, 0x02, 0x81];
            p.extend((body.len() as u16).to_le_bytes());
            p.push(0x10);
            p.extend(&body);
            p
        }, 42).is_ok(), "synthetic body passes the dump checks");
        let info = tone_info(&body).unwrap();
        assert_eq!(info.amp, Some(AmpModel { name: "Ratfish".into(), based_on: Some("ProCo Rat".into()) }));
        assert_eq!(info.cab, Some(CabSource::ModelCab));
        assert_eq!((info.amp_id, info.cab_id), (Some([7; 16]), None), "no id for an empty cab slot");
        // Custom IR (type 3): name from the data blob, garbage after NUL.
        let ir = enc_slot("", 3, &slot_data(b"B.Alnico 2 Liveplayrock", 200));
        assert_eq!(tone_info(&enc_body(130, amp_slot(), ir)).unwrap().cab, Some(CabSource::Ir("B.Alnico 2 Liveplayrock".into())));
        // Borrowed cab (type 4).
        let borrowed = enc_slot("51st Hi", 4, &slot_data(b"\0\0\0\0Peavey 5150", 300));
        assert_eq!(
            tone_info(&enc_body(1, amp_slot(), borrowed)).unwrap().cab,
            Some(CabSource::Borrowed { name: "51st Hi".into(), based_on: Some("Peavey 5150".into()) })
        );
        // Unknown kinds: nothing.
        let info = tone_info(&enc_body(1, enc_slot("X", 9, &[0; 8]), enc_slot("", 7, &[0; 8]))).unwrap();
        assert_eq!((info.amp, info.cab), (None, None));
    }

    #[test]
    fn slot_kinds_apply_to_either_slot() {
        // Amp captures use kind 4 in the AMP slot (2 treated alike).
        for kind in [4, 2] {
            let amp = enc_slot("Polite Twin", kind, &slot_data(b"\0\0\0\0Fender Twin Reverb", 300));
            let info = tone_info(&enc_body(9, amp, enc_slot("", 0, &[0; 8]))).unwrap();
            assert_eq!(info.amp, Some(AmpModel { name: "Polite Twin".into(), based_on: Some("Fender Twin Reverb".into()) }));
        }
        // An empty (kind 0) cab slot keeps stale data: never shown.
        let stale = enc_slot("51st Hi", 0, &slot_data(b"\0\0\0\0Peavey 5150", 300));
        assert_eq!(tone_info(&enc_body(0, amp_slot(), stale)).unwrap().cab, Some(CabSource::ModelCab));
        // Empty or IR content in the AMP slot: no amp line.
        let ir = enc_slot("", 3, &slot_data(b"Some IR", 50));
        for amp in [enc_slot("Old", 0, &slot_data(b"\0\0\0\0Old based", 50)), ir] {
            assert_eq!(tone_info(&enc_body(0, amp, enc_slot("", 0, &[0; 8]))).unwrap().amp, None);
        }
        // A model without a readable name: nothing (no guessing).
        let unnamed = enc_slot("", 4, &slot_data(b"\0\0\0\0Something", 50));
        assert_eq!(tone_info(&enc_body(0, unnamed, enc_slot("", 0, &[0; 8]))).unwrap().amp, None);
    }

    #[test]
    fn slot_text_rules() {
        assert_eq!(slot_text(b"Ratfish\0garbage"), Some("Ratfish".into()));
        assert_eq!(slot_text(b"  padded  \0"), Some("padded".into()));
        assert_eq!(slot_text(b"\0Ratfish"), None, "empty before the NUL");
        assert_eq!(slot_text(b"Bad\x01name\0"), None, "non-printable");
        assert_eq!(slot_text(&[b'A'; 100]).map(|t| t.len()), Some(SLOT_TEXT_MAX), "capped");
        // UTF-8 names (hardware: "Plæxi Driver"), capped in characters.
        assert_eq!(slot_text(b"Pl\xc3\xa6xi Driver\0junk").as_deref(), Some("Plæxi Driver"));
        assert_eq!(slot_text(&"æ".repeat(100).into_bytes()).map(|t| t.chars().count()), Some(SLOT_TEXT_MAX));
        // Invalid sequences are replaced, not refused; DEL is a control.
        assert_eq!(slot_text(b"Bad\xffname\0").as_deref(), Some("Bad\u{fffd}name"));
        assert_eq!(slot_text(b"Bad\x7fname\0"), None);
        // A UTF-8 name through tone info.
        let plaexi = enc_slot("", 4, &slot_data(b"\0\0\0\0Marshall Super Lead MKII", 300));
        let mut plaexi = plaexi;
        let at = find(&plaexi, &[0xBC, 0x21]).unwrap() + 2;
        plaexi[at..at + 14].copy_from_slice(b"Pl\xc3\xa6xi Driver\0");
        let info = tone_info(&enc_body(3, plaexi, enc_slot("", 0, &[0; 8]))).unwrap();
        assert_eq!(info.amp.unwrap().name, "Plæxi Driver");
        // A body that doesn't parse gives no names at all.
        let body = enc_body(1, amp_slot(), enc_slot("", 0, &[0; 8]));
        assert_eq!(tone_info(&body[..body.len() - 3]), None);
    }

    #[test]
    fn dump_layout_validation() {
        let good = synthetic_dump(42, "OLD NAME", 500);
        let body = dump_body(&good, 42).unwrap();
        assert!(body.starts_with(&[0xB9, 0x04, 0x01, 0x00, 42]));
        assert_eq!(name_buffer(body).unwrap()[..8], *b"OLD NAME");
        // Index >= 128.
        assert!(dump_body(&synthetic_dump(140, "X", 10), 140).is_ok());
        // Another preset's dump.
        assert!(dump_body(&good, 43).is_err());
        // Length field != body length.
        let mut short = good.clone();
        short.pop();
        assert!(dump_body(&short, 42).is_err());
        // Wrong type (a summary reply header).
        let mut wrong = good.clone();
        wrong[3] = 0x04;
        assert!(dump_body(&wrong, 42).is_err());
        // No name tag near the body start.
        let mut no_tag = good.clone();
        let tag = find(&no_tag, &NAME_TAG).unwrap();
        no_tag[tag] = 0x00;
        assert!(dump_body(&no_tag, 42).is_err());
        // Too short to hold the name buffer.
        let mut p = vec![0xB9, 0x03, 0x81, 0x03, 0x02, 0x81, 10, 0, 0x10];
        p.extend_from_slice(&[0xB9, 0x04, 0x01, 0x00, 42, 0xB9, 0x02, 0xBC, 0x21, 0x41]);
        assert!(dump_body(&p, 42).is_err());
    }

    #[test]
    fn name_patching() {
        let dump = synthetic_dump(7, "A MUCH LONGER OLD NAME", 100);
        let body = dump_body(&dump, 7).unwrap();
        let patched = patch_dump_name(body, "  New ").unwrap();
        let buf = name_buffer(&patched).unwrap();
        assert_eq!(&buf[..3], b"New");
        assert!(buf[3..].iter().all(|&b| b == 0), "zero-filled after the name");
        // Only the name buffer changes.
        let start = name_buf_offset(body).unwrap();
        assert_eq!(&patched[..start], &body[..start]);
        assert_eq!(&patched[start + NAME_BUF_LEN..], &body[start + NAME_BUF_LEN..]);
        // 32 bytes max (33rd stays NUL), printable ASCII only.
        let long = patch_dump_name(body, &"x".repeat(40)).unwrap();
        let buf = name_buffer(&long).unwrap();
        assert_eq!(&buf[..32], "x".repeat(32).as_bytes());
        assert_eq!(buf[32], 0);
        assert_eq!(filter_name("Caf\u{e9} \u{2713} ok\ttab"), "Caf  oktab");
        assert_eq!(clean_name("  Crunch 1  "), "Crunch 1");
        assert!(patch_dump_name(body, " \u{e9} ").is_err(), "empty after cleaning");
    }
}
