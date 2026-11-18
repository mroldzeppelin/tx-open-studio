//! The app-facing handle: `Pedal` (requests into the session thread) and `Event` (what the pedal told us).

use super::*;
use std::collections::VecDeque;
use std::sync::mpsc;

pub(crate) fn debug_enabled() -> bool {
    std::env::var_os("TX_DEBUG_SERIAL").is_some()
}

/// Something the pedal told us. Delivered from the session thread.
#[derive(Debug)]
pub enum Event {
    /// Handshake done; the session is live.
    Connected(String),
    /// The session ended (open failed, unplugged, I/O error).
    Disconnected(String),
    /// Opening the port (path) failed with "permission denied"; always
    /// followed by `Disconnected`.
    NoAccess(String),
    /// Result of `Pedal::request_names`.
    Names(Result<Vec<PresetMeta>, String>),
    /// The pedal's front-panel VOLUME (0..10, section 03 index 0): the
    /// master volume, pedal-wide and persistent across presets (not preset
    /// data). From `Pedal::request_volume` or a live change push.
    Volume(f32),
    /// Character of one preset (0-based), from a details read or push.
    Character { preset_idx: u8, character: String },
    /// Result of `Pedal::request_amp_params` (0-based preset index).
    /// `raw` is the whole parameter array as far as it parsed (see
    /// `extract_param_array`), empty if the details didn't arrive.
    AmpParams { preset_idx: u8, result: Result<AmpParams, String>, raw: Vec<f32> },
    /// A parameter changed on the pedal (e.g. a physical knob was turned).
    /// `index` is the preset-parameter index (see `AmpKnob::param_index`).
    ParamChanged { index: usize, value: f32 },
    /// The pedal's current preset (0-based), from a state message: the
    /// handshake's request-state reply, or a pushed state update.
    CurrentPreset(u8),
    /// Global config (reply to `request_globals`, or pushed by the pedal).
    Globals(Result<Globals, String>),
    /// Result of `Pedal::rename_preset` (0-based preset index): the name
    /// as stored and verified, or why nothing (or nothing verified) was
    /// written. Always sent, also when the session ends mid-operation.
    Renamed { preset_idx: u8, result: Result<String, String> },
    /// Result of `Pedal::save_preset` (0-based preset index); always sent,
    /// also when the session ends mid-operation.
    Saved { preset_idx: u8, result: Result<(), String> },
    /// Result of `Pedal::request_tone_info` (0-based preset index); None
    /// when the dump couldn't be read or parsed.
    ToneInfo { preset_idx: u8, info: Option<ToneInfo> },
    /// The pedal switched preset by itself (footswitch); carries the new
    /// preset's amp params when they parsed, and its raw parameter array.
    PresetChanged { preset_idx: u8, params: Option<AmpParams>, raw: Vec<f32> },
    /// The pedal's library list (`0x0208`): used items in list order.
    LibraryList(Result<Vec<LibraryItem>, String>),
    /// Library items' names (any dump or live read with filled slots, a
    /// catalog scan read): names for the catalog.
    Slots(Vec<SlotInfo>),
    /// Slot summary of preset `preset_idx` (stored dump, or the live
    /// buffer of the active preset).
    PresetSlots { preset_idx: u8, slots: PresetSlots },
    /// The live buffer after an apply or an on-pedal browse: the active
    /// preset, the pedal's dirty flag and the slot names.
    Live { preset_idx: u8, dirty: bool, info: Option<ToneInfo> },
    /// Result of `Pedal::apply_slot`; always sent.
    SlotApplied { target: SlotTarget, result: Result<(), String> },
    /// A catalog scan started or ended (`running` false once finished or
    /// cancelled). `completed`: every item was read or tried (not
    /// cancelled).
    CatalogScan { running: bool, completed: bool },
}

pub(crate) enum Request {
    Names,
    AmpParams(u8),
    /// (section, index, value).
    SetParam(u8, u8, f32),
    Globals,
    SetGlobal(usize, f32),
    /// Rename a preset (index, new name).
    Rename(u8, String),
    /// Save the pedal's live edit buffer into its preset (index).
    Save(u8),
    /// Read a preset's amp/cab slot names (index).
    ToneInfo(u8),
    /// Make preset `idx` the active one.
    Select(u8),
    /// Read the front-panel VOLUME.
    ReadVolume,
    /// Read the library list (`0x0208`).
    LibraryList,
    /// Load a library item (or the model cab) into a slot of the live buffer.
    ApplySlot(SlotTarget, SlotChoice),
    /// Read these library items' names by list position (background).
    ScanCatalog(Vec<LibraryItem>),
    CancelScan,
    /// Read every preset's full dump for its slot summary (background).
    PresetPass,
}

/// Handle to the session thread that owns the serial port. Dropping it ends
/// the session (the thread notices within one read poll, or after the
/// current preset during a name scan).
pub struct Pedal {
    tx: mpsc::Sender<Request>,
}

impl Pedal {
    /// Starts a session thread: it waits for any previous session to release
    /// the port, opens + handshakes, then serves requests and reports
    /// unsolicited pedal messages through `on_event` until the port fails or
    /// the handle is dropped. Returns immediately.
    pub fn start(on_event: impl Fn(Event) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _guard = port_lock();
            let link = match open_any() {
                Ok((link, path, state)) => {
                    on_event(Event::Connected(path.display().to_string()));
                    report_state(state.as_deref(), &on_event, debug_enabled());
                    link
                }
                Err(failure) => {
                    if let OpenFailure::NoAccess(path) = &failure {
                        on_event(Event::NoAccess(path.display().to_string()));
                    }
                    on_event(Event::Disconnected(failure.message()));
                    return;
                }
            };
            let mut session = Session { link, rx, queue: VecDeque::new(), on_event: &on_event,
                debug: debug_enabled(),
                globals: None,
                globals_seen: 0,
                browse_due: None,
                scan: VecDeque::new(),
                pass: None,
                quiet_until: None,
                own_write: false,
            };
            let reason = session.run();
            on_event(Event::Disconnected(reason));
        });
        Self { tx }
    }

    pub(crate) fn send(&self, r: Request) -> Result<(), String> {
        self.tx.send(r).map_err(|_| "serial session not running".to_string())
    }

    pub fn request_names(&self) -> Result<(), String> {
        self.send(Request::Names)
    }

    /// Writes one preset parameter (index < 0x80) on the pedal.
    pub fn set_param(&self, index: usize, value: f32) -> Result<(), String> {
        match u8::try_from(index) {
            Ok(i) if i < 0x80 => self.send(Request::SetParam(PARAM_SECTION, i, value)),
            _ => Err(format!("param index {index} not supported for writes")),
        }
    }

    /// Writes the front-panel VOLUME (section 03, index 0; pedal-wide).
    pub fn set_volume(&self, value: f32) -> Result<(), String> {
        self.send(Request::SetParam(VOLUME_SECTION, 0, value))
    }

    /// Reads the amp params of `preset_idx` (0-based).
    pub fn request_amp_params(&self, preset_idx: u8) -> Result<(), String> {
        self.send(Request::AmpParams(preset_idx))
    }

    /// Renames preset `idx` (0-based): reads a fresh full dump, validates
    /// it, writes it back with the new name and verifies by re-reading.
    /// Answered with `Event::Renamed`.
    pub fn rename_preset(&self, idx: u8, name: &str) -> Result<(), String> {
        if usize::from(idx) >= PRESET_COUNT {
            return Err(format!("preset index {idx} out of range"));
        }
        self.send(Request::Rename(idx, name.to_string()))
    }

    /// Saves preset `idx` (0-based, the active preset): reads the pedal's
    /// live edit buffer, writes it back as the stored preset (as the
    /// official editor does) and verifies by re-reading the stored dump.
    /// Answered with `Event::Saved`.
    pub fn save_preset(&self, idx: u8) -> Result<(), String> {
        if usize::from(idx) >= PRESET_COUNT {
            return Err(format!("preset index {idx} out of range"));
        }
        self.send(Request::Save(idx))
    }

    /// Selects preset `idx` (0-based) on the pedal. Requests queued after
    /// it (the new preset's reads) are served after the select; writes
    /// queued before it go to the old preset first.
    pub fn select_preset(&self, idx: u8) -> Result<(), String> {
        if usize::from(idx) >= PRESET_COUNT {
            return Err(format!("preset index {idx} out of range"));
        }
        self.send(Request::Select(idx))
    }

    /// Reads the front-panel VOLUME (get-parameter, section 03 index 0);
    /// answered with `Event::Volume`. Pedal-wide: read once per session
    /// (connect / reconnect), later changes arrive as pushes.
    pub fn request_volume(&self) -> Result<(), String> {
        self.send(Request::ReadVolume)
    }

    /// Reads the amp model / cab source names of preset `idx` (0-based)
    /// from its full dump; answered with `Event::ToneInfo`. Read-only.
    pub fn request_tone_info(&self, idx: u8) -> Result<(), String> {
        self.send(Request::ToneInfo(idx))
    }

    /// Reads the library list; answered with `Event::LibraryList`.
    pub fn request_library_list(&self) -> Result<(), String> {
        self.send(Request::LibraryList)
    }

    /// Loads a library item by id (the pedal fills in names and data) or,
    /// for the CAB slot, the model's own cab into the live edit buffer of
    /// the active preset; a CAB load also sets the cab type (param 24) to
    /// 0. Verified by re-reading the live buffer; answered with
    /// `Event::SlotApplied` (and `Event::Live` on success).
    pub fn apply_slot(&self, target: SlotTarget, choice: SlotChoice) -> Result<(), String> {
        self.send(Request::ApplySlot(target, choice))
    }

    /// Reads the names of library `items` in the background (one
    /// read-only `0x0209` request each, by list position; user requests
    /// first); replaces a running scan. Reports `Event::Slots` per item
    /// read and `Event::CatalogScan` progress.
    pub fn scan_catalog(&self, items: Vec<LibraryItem>) -> Result<(), String> {
        self.send(Request::ScanCatalog(items))
    }

    /// Stops a catalog scan.
    pub fn cancel_scan(&self) -> Result<(), String> {
        self.send(Request::CancelScan)
    }

    /// Reads every preset's full dump in the background (user requests
    /// first; read-only, no preset select): `Event::PresetSlots` +
    /// `Event::Slots` per preset.
    pub fn preset_pass(&self) -> Result<(), String> {
        self.send(Request::PresetPass)
    }

    /// Reads the global config; answered with `Event::Globals`.
    pub fn request_globals(&self) -> Result<(), String> {
        self.send(Request::Globals)
    }

    /// Writes one global (any `G_*` index) by sending the
    /// last global config read back with that value changed. Verified on
    /// hardware.
    pub fn set_global(&self, index: usize, value: f32) -> Result<(), String> {
        if index >= GLOBALS_COUNT {
            return Err(format!("global index {index} out of range"));
        }
        self.send(Request::SetGlobal(index, value))
    }
}
