//! Preset slots by library id: slot identities, ID-only slots (the pedal
//! fills in names and model data from its internal library), live-buffer
//! edits, per-preset slot summaries, the library list (`0x0208`) and the
//! browse push (`0x020B`).

use super::*;
use std::time::Duration;

/// A 16-byte library id (slot `/0`).
pub type LibraryId = [u8; 16];

pub(crate) const TYPE_LIBRARY_LIST: u16 = 0x0208;

/// The pedal's short reaction to a live write (`B9 03 05 00 10`, also in
/// the official editor's captures): no content, handled silently.
pub(crate) const TYPE_LIVE_WRITE_ACK: u16 = 0x0005;

/// Pushed per step of the pedal's MODEL encoder (`B9 03 81 0B 02 03 02 |
/// B9 01 01`, no index); afterwards the live buffer holds the new item.
pub(crate) const TYPE_BROWSE: u16 = 0x020B;

/// Library list request (the globals request with type `08`).
pub(crate) const LIBRARY_LIST_REQUEST: [u8; 15] =
    [0xB9, 0x03, 0x00, 0x82, 0x06, 0x00, 0x80, 0x10, 0x03, 0xB9, 0x02, 0x81, 0x08, 0x02, 0x10];

/// The 300-entry id array in the library list reply.
pub(crate) const LIBRARY_LIST_MARKER: [u8; 4] = [0xBA, 0x81, 0x2C, 0x01];

pub(crate) const LIBRARY_LIST_TIMEOUT: Duration = Duration::from_millis(2000);

/// Slot content kinds (slot `/2`): 0 empty, 1/2/4 tone model, 3 IR.
pub const KIND_EMPTY: u16 = 0;
pub const KIND_IR: u16 = 3;
pub const KIND_AMP_MODEL: u16 = 4;

pub fn is_model_kind(kind: u16) -> bool {
    matches!(kind, 1 | 2 | 4)
}

/// Cab type (param 24): 0 the cab slot / model cab, 1 VIR, 2 off.
pub const CAB_TYPE_SLOT: f32 = 0.0;
pub const CAB_TYPE_OFF: f32 = 2.0;
/// Param indices of the cab type and the VIR cab model.
pub const P_CAB_TYPE: usize = 24;
pub const P_CAB_MODEL: usize = 25;

/// One used entry of the pedal's library list: its position in the
/// 300-entry array (0-based) and its id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LibraryItem {
    pub position: u16,
    pub id: LibraryId,
}

/// Used ids of a library list reply, in list order (unused entries are
/// all `00` or all `FF`; duplicates dropped).
pub(crate) fn parse_library_list(payload: &[u8]) -> Result<Vec<LibraryItem>, String> {
    if frame_type(payload) != Some(TYPE_LIBRARY_LIST) {
        return Err("not a library list".into());
    }
    let mut pos = find(payload, &LIBRARY_LIST_MARKER).ok_or("library id array not found")?;
    let node = parse_node(payload, &mut pos, 0).ok_or("library id array does not parse")?;
    let items = node.items().ok_or("library id array is not a list")?;
    let mut out: Vec<LibraryItem> = Vec::new();
    for (position, item) in items.iter().enumerate() {
        let Some(id) = first_id(item) else { continue };
        let unused = id.iter().all(|&b| b == 0) || id.iter().all(|&b| b == 0xFF);
        if !unused && !out.iter().any(|i| i.id == id) {
            out.push(LibraryItem { position: position as u16, id });
        }
    }
    Ok(out)
}

/// The first 16-byte blob in `node` (the item itself or nested in it).
fn first_id(node: &Node<'_>) -> Option<LibraryId> {
    match node {
        Node::Blob(b) => (*b).try_into().ok(),
        Node::List(items) => items.iter().find_map(first_id),
        _ => None,
    }
}

pub(crate) fn is_browse_push(payload: &[u8]) -> bool {
    frame_type(payload) == Some(TYPE_BROWSE)
}

/// The AMP or the CAB slot of a preset body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotTarget {
    Amp,
    Cab,
}

impl SlotTarget {
    /// Path in a dump / live body: `/3/1` AMP, `/3/3` CAB.
    pub(crate) fn path(self) -> [usize; 2] {
        match self {
            Self::Amp => [3, 1],
            Self::Cab => [3, 3],
        }
    }
}

/// Contents `/2`: 1 while the CAB slot is in use (the official editor
/// sets it with a cab slot), 0 for the model's own cab.
pub(crate) const CAB_FLAG_PATH: [usize; 2] = [3, 2];

/// What to load into a slot.
#[derive(Clone, Debug, PartialEq)]
pub enum SlotChoice {
    /// A library item by id, written ID-only with this content kind.
    Library { id: LibraryId, kind: u16 },
    /// CAB slot only: empty, the AMP model plays its own cab.
    ModelCab,
}

/// A filled slot's library identity and names (never its model data).
#[derive(Clone, Debug, PartialEq)]
pub struct SlotInfo {
    pub id: LibraryId,
    pub kind: u16,
    pub name: String,
    pub based_on: Option<String>,
}

pub(crate) fn slot_id(slot: &Node<'_>) -> Option<LibraryId> {
    slot.at(&[0])?.blob()?.try_into().ok()
}

/// Identity and names of a filled slot (kind != 0, data not all zero).
/// Model: name buffer 0 + based-on after the data's 4 header bytes. IR:
/// name from the data's start. A model-kind slot with an empty name
/// buffer but a name at the data's start is an IR (as when an IR id is
/// loaded with a model kind).
pub(crate) fn slot_info(slot: &Node<'_>) -> Option<SlotInfo> {
    let items = slot.items()?;
    let kind = items.get(2)?.int()?;
    if kind == KIND_EMPTY {
        return None;
    }
    let id = slot_id(slot)?;
    let data = items.get(3)?.blob()?;
    if data.iter().all(|&b| b == 0) {
        return None;
    }
    let name = items.get(1)?.at(&[0])?.blob().and_then(slot_text);
    match (kind, name) {
        (1 | 2 | 4, Some(name)) => Some(SlotInfo { id, kind, name, based_on: data.get(4..).and_then(slot_text) }),
        (1..=4, _) => slot_text(data).map(|name| SlotInfo { id, kind: KIND_IR, name, based_on: None }),
        _ => None,
    }
}

/// Filled AMP and CAB slots of a body (for the catalog).
pub(crate) fn slot_infos(body: &[u8]) -> Vec<SlotInfo> {
    let Some(tree) = parse_tree(body) else { return Vec::new() };
    [SlotTarget::Amp, SlotTarget::Cab].iter().filter_map(|t| tree.at(&t.path()).and_then(slot_info)).collect()
}

/// `target` holds `id` and the pedal has filled it in (content kind set,
/// data no longer all zero). Some(its info, None if the names don't
/// read); None while not loaded. A name is not required for success.
pub(crate) fn filled_slot(body: &[u8], target: SlotTarget, id: &LibraryId) -> Option<Option<SlotInfo>> {
    let tree = parse_tree(body)?;
    let slot = tree.at(&target.path())?;
    let items = slot.items()?;
    let loaded = slot_id(slot) == Some(*id)
        && items.get(2).and_then(Node::int).is_some_and(|k| k != KIND_EMPTY)
        && items.get(3).and_then(Node::blob).is_some_and(|d| d.iter().any(|&b| b != 0));
    loaded.then(|| slot_info(slot))
}

/// What a preset's slots and cab params hold, for the preset list line
/// (ids only; names come from the catalog).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PresetSlots {
    pub amp_id: Option<LibraryId>,
    pub amp_kind: u16,
    /// None when the CAB slot is empty (kind 0: its id is stale).
    pub cab_id: Option<LibraryId>,
    pub cab_kind: u16,
    pub cab_flag: Option<u16>,
    pub cab_type: Option<f32>,
    pub cab_model: Option<f32>,
}

/// Slot summary of a dump or live body.
pub(crate) fn preset_slots(body: &[u8]) -> Option<PresetSlots> {
    let tree = parse_tree(body)?;
    let kind = |t: SlotTarget| tree.at(&t.path()).and_then(|s| s.at(&[2])).and_then(Node::int).unwrap_or(KIND_EMPTY);
    let id = |t: SlotTarget| (kind(t) != KIND_EMPTY).then(|| tree.at(&t.path()).and_then(slot_id)).flatten();
    let params = extract_param_array(body);
    Some(PresetSlots {
        amp_id: id(SlotTarget::Amp),
        amp_kind: kind(SlotTarget::Amp),
        cab_id: id(SlotTarget::Cab),
        cab_kind: kind(SlotTarget::Cab),
        cab_flag: tree.at(&CAB_FLAG_PATH).and_then(Node::int),
        cab_type: params.get(P_CAB_TYPE).copied(),
        cab_model: params.get(P_CAB_MODEL).copied(),
    })
}

/// Raw bytes of `target`'s slot in `body`.
pub(crate) fn slot_bytes(body: &[u8], target: SlotTarget) -> Option<&[u8]> {
    span_at(body, &target.path()).map(|r| &body[r])
}

/// `p` with the node at `path` replaced by `new` (lists count items, not
/// bytes, so nothing else changes).
pub(crate) fn splice(p: &[u8], path: &[usize], new: &[u8]) -> Result<Vec<u8>, String> {
    let span = span_at(p, path).ok_or_else(|| format!("node {path:?} not found"))?;
    let mut out = Vec::with_capacity(p.len() - span.len() + new.len());
    out.extend_from_slice(&p[..span.start]);
    out.extend_from_slice(new);
    out.extend_from_slice(&p[span.end..]);
    Ok(out)
}

/// `slot` reduced to an id-only slot for `id`: id blob set, the name
/// list's blobs and the data blob zeroed (tags and lengths kept), content
/// kind `kind`. Everything else stays byte-identical: non-blob entries of
/// the name list (on hardware the second entry is often an int, e.g. `16`)
/// and anything after the data (`/4`…). The pedal fills in the rest from
/// its library.
pub(crate) fn id_only_slot(slot: &[u8], id: &LibraryId, kind: u16) -> Result<Vec<u8>, String> {
    let tree = parse_tree(slot).ok_or("slot does not parse")?;
    let items = tree.items().ok_or("slot is not a list")?;
    if items.len() < 4 || items[2].int().is_none() || items[3].blob().is_none() {
        return Err("unexpected slot layout".into());
    }
    if items[0].blob().map(<[u8]>::len) != Some(16) {
        return Err("slot id is not 16 bytes".into());
    }
    let name_blobs: Vec<usize> = items[1]
        .items()
        .ok_or("slot names missing")?
        .iter()
        .enumerate()
        .filter(|(_, n)| n.blob().is_some())
        .map(|(i, _)| i)
        .collect();
    let mut out = slot.to_vec();
    let mut fill = |path: &[usize], with: Option<&[u8]>| -> Result<(), String> {
        let span = span_at(slot, path).ok_or("slot part not found")?;
        let len = parse_tree(&slot[span.clone()]).and_then(|n| n.blob().map(<[u8]>::len)).ok_or("slot part is not a blob")?;
        let content = &mut out[span.end - len..span.end];
        match with {
            Some(bytes) => content.copy_from_slice(bytes),
            None => content.fill(0),
        }
        Ok(())
    };
    fill(&[0], Some(id))?;
    for i in name_blobs {
        fill(&[1, i], None)?;
    }
    fill(&[3], None)?;
    // Last: the kind's encoding may change the length.
    splice(&out, &[2], &encode_int(kind))
}

/// `body` with `target` replaced by an id-only slot for `id`; for the CAB
/// slot also contents `/2` = 1 (slot in use).
pub(crate) fn with_library_slot(body: &[u8], target: SlotTarget, id: &LibraryId, kind: u16) -> Result<Vec<u8>, String> {
    let slot = slot_bytes(body, target).ok_or("slot not found")?;
    let out = splice(body, &target.path(), &id_only_slot(slot, id, kind)?)?;
    match target {
        SlotTarget::Cab => splice(&out, &CAB_FLAG_PATH, &encode_int(1)),
        SlotTarget::Amp => Ok(out),
    }
}

/// `body` with the CAB slot emptied (kind 0) and contents `/2` = 0: the
/// AMP model plays its own cab.
pub(crate) fn model_cab_body(body: &[u8]) -> Result<Vec<u8>, String> {
    let out = splice(body, &CAB_FLAG_PATH, &encode_int(0))?;
    splice(&out, &[3, 3, 2], &encode_int(KIND_EMPTY))
}

/// The CAB slot is empty and unused (after `model_cab_body`).
pub(crate) fn model_cab_active(body: &[u8]) -> bool {
    parse_tree(body).is_some_and(|t| {
        t.at(&CAB_FLAG_PATH).and_then(Node::int) == Some(0) && t.at(&[3, 3, 2]).and_then(Node::int) == Some(KIND_EMPTY)
    })
}

/// Body of a write to the live edit buffer: `B9 04 00 00 00` + the
/// contents of `body` (as the official editor sends previews).
pub(crate) fn live_write_body(body: &[u8]) -> Result<Vec<u8>, String> {
    let contents = span_at(body, &[3]).ok_or("body contents not found")?;
    let mut out = vec![0xB9, 0x04, 0x00, 0x00, 0x00];
    out.extend_from_slice(&body[contents]);
    Ok(out)
}

/// Value of parameter `index` (array 0) in a dump or live body.
pub(crate) fn param_value(body: &[u8], index: usize) -> Option<f32> {
    extract_param_array(body).get(index).copied()
}

/// `body` with parameter `index` of array 0 set to `value`.
pub(crate) fn patch_param(body: &[u8], index: usize, value: f32) -> Result<Vec<u8>, String> {
    if index >= PARAM_COUNT {
        return Err(format!("param index {index} out of range"));
    }
    let at = find(body, &PARAM_MARKER).ok_or("parameter array not found")? + PARAM_MARKER.len() + index * 5;
    if body.get(at) != Some(&FLOAT_TAG) || body.len() < at + 5 {
        return Err("unexpected parameter array layout".into());
    }
    let mut out = body.to_vec();
    out[at + 1..at + 5].copy_from_slice(&value.to_le_bytes());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::test_util::*;

    const ID: LibraryId = [0x42; 16];

    /// A live body with a real 109-param array 0 (so param patches work).
    fn live_with_params() -> Vec<u8> {
        let floats = |n: usize| {
            enc_list(0xBA, (0..n).map(|i| { let mut f = vec![FLOAT_TAG]; f.extend((i as f32).to_le_bytes()); f }).collect())
        };
        let details = enc_list(0xB9, vec![enc_name("PRESET"), enc_list(0xBA, vec![floats(PARAM_COUNT), floats(3), floats(3)])]);
        let contents = enc_list(0xB9, vec![details, amp_slot(), enc_int(0), cab_slot()]);
        enc_list(0xB9, vec![enc_int(0), enc_int(0), enc_int(5), contents])
    }

    #[test]
    fn id_only_slot_zeroes_names_and_data() {
        let slot = cab_slot();
        let out = id_only_slot(&slot, &ID, KIND_IR).unwrap();
        assert_eq!(out.len(), slot.len(), "same length (kind literal stays one byte)");
        let tree = parse_tree(&out).unwrap();
        assert_eq!(slot_id(&tree), Some(ID));
        assert!(tree.at(&[1, 0]).unwrap().blob().unwrap().iter().all(|&b| b == 0));
        assert!(tree.at(&[1, 1]).unwrap().blob().unwrap().iter().all(|&b| b == 0));
        assert_eq!(tree.at(&[2]).unwrap().int(), Some(KIND_IR));
        let data = tree.at(&[3]).unwrap().blob().unwrap();
        assert_eq!(data.len(), 300);
        assert!(data.iter().all(|&b| b == 0));
        let original = parse_tree(&slot).unwrap();
        assert_eq!(tree.at(&[4]), original.at(&[4]), "trailing items kept");
        // Not a slot: refused.
        assert!(id_only_slot(&enc_list(0xB9, vec![enc_int(1)]), &ID, 4).is_err());
        assert!(id_only_slot(&enc_list(0xB9, vec![enc_blob(&[1; 8]), enc_list(0xB9, vec![]), enc_int(4), enc_blob(&[0; 4])]), &ID, 4).is_err());
    }

    #[test]
    fn slot_splice_keeps_everything_else_byte_identical() {
        let body = live_with_params();
        let cab = span_at(&body, &[3, 3]).unwrap();
        let flag = span_at(&body, &CAB_FLAG_PATH).unwrap();
        let out = with_library_slot(&body, SlotTarget::Cab, &ID, KIND_AMP_MODEL).unwrap();
        // Everything before the flag, and between the flag and the cab slot, unchanged.
        assert_eq!(&out[..flag.start], &body[..flag.start]);
        assert_eq!(out[flag.start], 1, "cab slot in use");
        assert_eq!(&out[flag.end..cab.start], &body[flag.end..cab.start]);
        assert_eq!(out.len(), body.len());
        let tree = parse_tree(&out).unwrap();
        assert_eq!(slot_id(tree.at(&[3, 3]).unwrap()), Some(ID));
        assert_eq!(tree.at(&[3, 1]), parse_tree(&body).unwrap().at(&[3, 1]), "amp slot untouched");
        // AMP: only the amp slot changes; the flag stays.
        let out = with_library_slot(&body, SlotTarget::Amp, &ID, 1).unwrap();
        let amp = span_at(&body, &[3, 1]).unwrap();
        assert_eq!(&out[..amp.start], &body[..amp.start]);
        assert_eq!(&out[amp.end..], &body[amp.end..]);
        // Model cab: flag 0, cab kind 0, nothing else.
        let model = model_cab_body(&with_library_slot(&body, SlotTarget::Cab, &ID, 4).unwrap()).unwrap();
        assert!(model_cab_active(&model));
        assert!(!model_cab_active(&with_library_slot(&body, SlotTarget::Cab, &ID, 4).unwrap()));
    }

    /// Slot as on hardware (preset 20A's AMP slot): the name list's second
    /// entry is an int (16), not a name buffer.
    fn slot_20a() -> Vec<u8> {
        enc_list(0xB9, vec![
            enc_blob(&[9; 16]),
            enc_list(0xB9, vec![enc_name("Breaking Thunder"), enc_int(16)]),
            enc_int(4),
            enc_blob(&slot_data(b"Cb\x01\x02Orange Thunderverb 200", 13768)),
            enc_int(0),
        ])
    }

    #[test]
    fn int_name_entry_is_kept_byte_identical() {
        let slot = slot_20a();
        let out = id_only_slot(&slot, &ID, KIND_AMP_MODEL).unwrap();
        assert_eq!(out.len(), slot.len());
        let tree = parse_tree(&out).unwrap();
        assert_eq!(slot_id(&tree), Some(ID));
        assert!(tree.at(&[1, 0]).unwrap().blob().unwrap().iter().all(|&b| b == 0), "name blob zeroed");
        assert_eq!(tree.at(&[1, 1]).unwrap().int(), Some(16), "int entry kept");
        let ints = |p: &[u8]| span_at(p, &[1, 1]).map(|r| p[r].to_vec());
        assert_eq!(ints(&out), ints(&slot));
        let data_span = span_at(&out, &[3]).unwrap();
        assert_eq!(&out[data_span.start..data_span.start + 4], &[0xBC, 0x81, 0xC8, 0x35], "data blob header kept");
        assert!(out[data_span.start + 4..data_span.end].iter().all(|&b| b == 0));
        // Reading such slots: names, ids, summaries, apply.
        let info = slot_info(&parse_tree(&slot).unwrap()).unwrap();
        assert_eq!((info.name.as_str(), info.based_on.as_deref()), ("Breaking Thunder", Some("Orange Thunderverb 200")));
        let body = enc_body_with(0, 0, 58, slot_20a(), slot_20a());
        assert_eq!(slot_infos(&body).len(), 2);
        let tone = tone_info(&body).unwrap();
        assert_eq!(tone.amp.unwrap().name, "Breaking Thunder");
        assert_eq!(tone.amp_id, Some([9; 16]));
        assert!(matches!(tone.cab, Some(CabSource::Borrowed { .. })));
        assert_eq!(preset_slots(&body).unwrap().cab_id, Some([9; 16]));
        for target in [SlotTarget::Amp, SlotTarget::Cab] {
            let applied = with_library_slot(&body, target, &ID, 4).unwrap();
            assert_eq!(parse_tree(&applied).unwrap().at(&target.path()).and_then(slot_id), Some(ID));
        }
        assert!(model_cab_active(&model_cab_body(&body).unwrap()));
    }

    #[test]
    fn live_write_body_is_the_editor_header_plus_contents() {
        let body = enc_body_with(0, 1, 140, amp_slot(), cab_slot());
        let out = live_write_body(&body).unwrap();
        assert_eq!(&out[..5], &[0xB9, 0x04, 0x00, 0x00, 0x00]);
        let contents = span_at(&body, &[3]).unwrap();
        assert_eq!(&out[5..], &body[contents]);
        assert!(parse_tree(&out).is_some());
    }

    #[test]
    fn slot_info_kinds() {
        let info = |slot: Vec<u8>| slot_info(&parse_tree(&slot).unwrap());
        let model = info(cab_slot()).unwrap();
        assert_eq!((model.kind, model.name.as_str(), model.based_on.as_deref()), (4, "51st Hi", Some("Peavey 5150")));
        assert_eq!(model.id, [7; 16]);
        let ir = info(enc_slot("", 3, &slot_data(b"Alnico 412", 50))).unwrap();
        assert_eq!((ir.kind, ir.name.as_str(), ir.based_on), (KIND_IR, "Alnico 412", None));
        // An IR loaded with a model kind: no name buffer, name in the data.
        let ir4 = info(enc_slot("", 4, &slot_data(b"Alnico 412", 50))).unwrap();
        assert_eq!((ir4.kind, ir4.name.as_str()), (KIND_IR, "Alnico 412"));
        // Empty slot, or not filled in yet (data all zero): nothing.
        assert_eq!(info(enc_slot("Stale", 0, &slot_data(b"\0\0\0\0Old", 50))), None);
        assert_eq!(info(enc_slot("", 4, &[0; 50])), None);
        // Filled only when the id matches.
        let body = enc_body_with(0, 0, 3, amp_slot(), cab_slot());
        assert!(filled_slot(&body, SlotTarget::Cab, &[7; 16]).unwrap().is_some());
        assert!(filled_slot(&body, SlotTarget::Cab, &ID).is_none());
        // Loaded without a readable name (binary data, empty name buffer):
        // still loaded.
        let unnamed = enc_body_with(0, 0, 3, amp_slot(), enc_slot("", 4, &[0x01, 0x02, 0x03, 0x04, 0x05]));
        assert_eq!(filled_slot(&unnamed, SlotTarget::Cab, &[7; 16]), Some(None));
        // Not filled in yet (data zero): not loaded.
        let pending = enc_body_with(0, 0, 3, amp_slot(), enc_slot("", 4, &[0; 50]));
        assert_eq!(filled_slot(&pending, SlotTarget::Cab, &[7; 16]), None);
        // A UTF-8 name reads.
        let utf8 = enc_body_with(0, 0, 3, amp_slot(), enc_slot("Plæxi Driver", 4, &slot_data(b"\0\0\0\0Marshall", 50)));
        let info = filled_slot(&utf8, SlotTarget::Cab, &[7; 16]).unwrap().unwrap();
        assert_eq!(info.name, "Plæxi Driver");
        assert_eq!(slot_infos(&body).len(), 2);
    }

    #[test]
    fn preset_slot_summary() {
        let mut body = live_with_params();
        body = patch_param(&body, P_CAB_TYPE, 1.0).unwrap();
        body = patch_param(&body, P_CAB_MODEL, 33.0).unwrap();
        assert_eq!(param_value(&body, P_CAB_MODEL), Some(33.0));
        let s = preset_slots(&body).unwrap();
        assert_eq!((s.amp_id, s.amp_kind), (Some([7; 16]), 1));
        assert_eq!((s.cab_id, s.cab_kind, s.cab_flag), (Some([7; 16]), 4, Some(0)));
        assert_eq!((s.cab_type, s.cab_model), (Some(1.0), Some(33.0)));
        // Empty cab slot: its stale id is not reported.
        let s = preset_slots(&model_cab_body(&body).unwrap()).unwrap();
        assert_eq!((s.cab_id, s.cab_kind), (None, 0));
        assert!(patch_param(&body, PARAM_COUNT, 0.0).is_err());
        assert!(patch_param(&[0xB9, 0x00], 3, 0.0).is_err());
    }

    #[test]
    fn library_list_parsing() {
        let mut ids: Vec<Vec<u8>> = (0..300u16).map(|_| enc_blob(&[0; 16])).collect();
        ids[0] = enc_blob(&[1; 16]);
        ids[7] = enc_blob(&[2; 16]);
        ids[8] = enc_blob(&[0xFF; 16]);
        ids[9] = enc_blob(&[1; 16]); // duplicate
        let array = enc_list(0xBA, ids);
        assert_eq!(&array[..4], &LIBRARY_LIST_MARKER);
        let mut p = vec![0xB9, 0x03, 0x81, 0x08, 0x02, 0x82, 0x00, 0x00, 0x80, 0x10, 0x03, 0xB9, 0x02, 0x10];
        p.extend(array);
        let list = parse_library_list(&p).unwrap();
        assert_eq!(list, vec![LibraryItem { position: 0, id: [1; 16] }, LibraryItem { position: 7, id: [2; 16] }]);
        // Another frame type, or no array: refused.
        let mut other = p.clone();
        other[3] = 0x06;
        assert!(parse_library_list(&other).is_err());
        assert!(parse_library_list(&p[..11]).is_err());
        assert!(is_browse_push(&[0xB9, 0x03, 0x81, 0x0B, 0x02, 0x03, 0x02, 0xB9, 0x01, 0x01]));
        assert!(!is_browse_push(&p));
    }
}
