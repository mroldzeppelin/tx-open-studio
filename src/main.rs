mod fx;
mod tonex_serial;

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use midir::{MidiOutput, MidiOutputConnection};
use slint::{ModelRc, VecModel};
use slint::Model;
use tonex_serial::{AmpKnob, Event, PresetMeta, PRESET_COUNT};

slint::include_modules!();

const MIN_PRESET: i32 = 1;
const MAX_PRESET: i32 = 150;
/// Shown only until the pedal's state reply tells us its current preset.
const DEFAULT_PRESET: i32 = 43;
const CLIENT_NAME: &str = "tx-open-studio";
const STATUS_UP: &str = "TONEX PEDAL CONNECTED";
const STATUS_RECONNECTING: &str = "RECONNECTING…";
/// Status bar text for `status`.
fn status_text(status: &LinkStatus) -> String {
    match status {
        LinkStatus::Connected => STATUS_UP.into(),
        LinkStatus::NoAccess(path) => format!("NO ACCESS TO {path} — INSTALL THE UDEV RULE (SEE README)"),
        LinkStatus::Reconnecting => STATUS_RECONNECTING.into(),
    }
}

/// GitHub repo checked for newer releases, and where the update chip leads.
const UPDATE_REPO: &str = "mroldzeppelin/tx-open-studio";
const RELEASES_URL: &str = "https://github.com/mroldzeppelin/tx-open-studio/releases/latest";
/// update-informer caches the answer on disk and asks GitHub at most this
/// often (the first run only creates the cache).
const UPDATE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Looks for a newer GitHub release in a background thread and, if there
/// is one, shows the update chip. Any failure (offline, no release yet,
/// rate limit) just means no chip; the UI thread never waits for it.
/// Disabled by setting TX_NO_UPDATE_CHECK.
fn start_update_check(ui: slint::Weak<AppWindow>) {
    // Dev hook: TX_FAKE_UPDATE=0.2.0 shows the chip without any network.
    if let Ok(fake) = std::env::var("TX_FAKE_UPDATE") {
        let _ = ui.upgrade_in_event_loop(move |ui| ui.set_update_version(format!("v{fake}").into()));
        return;
    }
    if std::env::var_os("TX_NO_UPDATE_CHECK").is_some() {
        return;
    }
    let spawned = std::thread::Builder::new().name("update-check".into()).spawn(move || {
        use update_informer::{registry, Check};
        let informer = update_informer::new(registry::GitHub, UPDATE_REPO, env!("CARGO_PKG_VERSION"))
            .interval(UPDATE_INTERVAL);
        if let Ok(Some(version)) = informer.check_version() {
            let label = version.to_string(); // "v1.2.3"
            let _ = ui.upgrade_in_event_loop(move |ui| ui.set_update_version(label.into()));
        }
    });
    if let Err(e) = spawned {
        eprintln!("update check not started: {e}");
    }
}

/// Opens the latest release page in the default browser.
fn open_releases_page() {
    match std::process::Command::new("xdg-open").arg(RELEASES_URL).spawn() {
        // Reap it in the background so it doesn't linger as a zombie.
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => eprintln!("xdg-open {RELEASES_URL} failed: {e}"),
    }
}

/// How often the reconnect poll looks for the pedal while a link is down.
const RECONNECT_POLL: Duration = Duration::from_secs(2);

/// Serial session as seen by the UI thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SerialLink {
    #[default]
    Down,
    /// Session thread started, handshake not done yet.
    Starting,
    Up,
}

/// What a serial session's `Disconnected` event means.
#[derive(Debug, PartialEq, Eq)]
enum SerialEnd {
    /// From a session that was already replaced or torn down: ignore.
    Stale,
    /// Open/handshake failed: serial stays down, MIDI is left alone.
    OpenFailed,
    /// A live session died (cable pulled, I/O error): the pedal is gone.
    Lost,
}

/// Both links to the pedal (one USB cable: MIDI + CDC serial). Serial
/// sessions are numbered so events of a replaced session are ignored.
#[derive(Debug, Default)]
struct LinkState {
    midi: bool,
    serial: SerialLink,
    session: u64,
    /// Last reported poll failure, to avoid logging it every 2 s.
    last_failure: String,
    /// Serial device node we may not open (udev rule missing), once the
    /// refusal persisted; cleared on connect or when the node is gone.
    no_access: Option<String>,
    /// Consecutive sessions refused access (see `serial_denied`), and the
    /// last refused session.
    denied_streak: u32,
    denied_session: u64,
}

/// Refused opens in a row before the "no access" hint shows (~4-6 s of
/// polling): a single refusal can be transient, e.g. right after another
/// instance released the port.
const NO_ACCESS_STREAK: u32 = 3;

/// What the status bar shows.
#[derive(Debug, PartialEq, Eq)]
enum LinkStatus {
    Connected,
    /// The pedal's serial port exists but we may not open it: the most
    /// actionable problem, shown even when MIDI works.
    NoAccess(String),
    Reconnecting,
}

impl LinkState {
    /// Fully connected (or the serial handshake is still running).
    fn healthy(&self) -> bool {
        self.midi && self.serial != SerialLink::Down
    }

    fn status(&self) -> LinkStatus {
        match &self.no_access {
            // Kept while a retry runs, so the 2 s polls don't flicker.
            Some(path) if self.serial != SerialLink::Up => LinkStatus::NoAccess(path.clone()),
            _ if self.healthy() => LinkStatus::Connected,
            _ => LinkStatus::Reconnecting,
        }
    }

    /// A new serial session starts; returns its id.
    fn serial_starting(&mut self) -> u64 {
        self.session += 1;
        self.serial = SerialLink::Starting;
        self.session
    }

    /// Handshake of session `id` done; false if that session is stale.
    fn serial_connected(&mut self, id: u64) -> bool {
        if id != self.session || self.serial == SerialLink::Down {
            return false;
        }
        self.serial = SerialLink::Up;
        self.clear_denied();
        true
    }

    /// Session `id` was refused access to `path`.
    fn serial_denied(&mut self, id: u64, path: String) {
        if id != self.session {
            return;
        }
        self.denied_session = id;
        self.denied_streak += 1;
        if self.denied_streak >= NO_ACCESS_STREAK {
            self.no_access = Some(path);
        }
    }

    fn clear_denied(&mut self) {
        self.no_access = None;
        self.denied_streak = 0;
    }

    fn serial_ended(&mut self, id: u64) -> SerialEnd {
        if id != self.session || self.serial == SerialLink::Down {
            return SerialEnd::Stale;
        }
        let was_up = self.serial == SerialLink::Up;
        self.serial = SerialLink::Down;
        if self.denied_session != id {
            // Failed for another reason: the refusals were not persistent.
            self.clear_denied();
        }
        if was_up {
            SerialEnd::Lost
        } else {
            SerialEnd::OpenFailed
        }
    }

    /// Both links dropped (pedal gone, or manual reconnect). The running
    /// session, if any, becomes stale.
    fn drop_all(&mut self) {
        self.midi = false;
        self.serial = SerialLink::Down;
        self.session += 1;
    }

    /// True when `failure` differs from the last one (worth logging).
    fn note_failure(&mut self, failure: &str) -> bool {
        if self.last_failure == failure {
            return false;
        }
        self.last_failure = failure.to_string();
        true
    }
}

/// Thread-safe holder for the MIDI output connection to the pedal.
#[derive(Clone, Default)]
struct MidiEngine {
    conn: Arc<Mutex<Option<MidiOutputConnection>>>,
}

impl MidiEngine {
    fn lock(&self) -> MutexGuard<'_, Option<MidiOutputConnection>> {
        // A poisoned lock only means another thread panicked mid-send; the
        // Option itself is still usable.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Finds the first output port containing "tonex" and connects to it.
    /// Returns the port name on success.
    fn try_connect(&self) -> Result<String, String> {
        let mut guard = self.lock();

        // Release any existing connection so the port is free to reopen.
        if let Some(old) = guard.take() {
            old.close();
        }

        // midir consumes MidiOutput on connect, so a fresh one is needed per attempt.
        let out = MidiOutput::new(CLIENT_NAME).map_err(|e| e.to_string())?;

        let (port, name) = out
            .ports()
            .into_iter()
            .find_map(|p| {
                let name = out.port_name(&p).ok()?;
                name.to_lowercase().contains("tonex").then_some((p, name))
            })
            .ok_or_else(|| "TONEX MIDI port not found".to_string())?;

        let conn = out
            .connect(&port, "tx-open-studio-out")
            .map_err(|e| e.to_string())?;

        *guard = Some(conn);
        Ok(name)
    }

    /// Sends the messages in order. On failure the connection is dropped so
    /// the UI can show "disconnected".
    fn send_all(&self, messages: &[&[u8]]) -> Result<(), String> {
        let mut guard = self.lock();

        let result = match guard.as_mut() {
            Some(conn) => messages
                .iter()
                .try_for_each(|m| conn.send(m))
                .map_err(|e| e.to_string()),
            None => Err("not connected".to_string()),
        };

        if result.is_err() {
            if let Some(conn) = guard.take() {
                conn.close();
            }
        }
        result
    }

    /// Drops the connection (pedal gone or reconnecting).
    fn close(&self) {
        if let Some(conn) = self.lock().take() {
            conn.close();
        }
    }

    /// Sends Bank Select (CC0) + Program Change on MIDI channel 1.
    fn send_preset(&self, preset_num: i32) -> Result<(), String> {
        let (bank, pc) = bank_and_pc(preset_num);
        self.send_all(&[&[0xB0, 0x00, bank], &[0xC0, pc]])
    }

    /// Sends a Control Change on MIDI channel 1.
    fn send_cc(&self, cc: u8, value: u8) -> Result<(), String> {
        self.send_all(&[&[0xB0, cc & 0x7F, value & 0x7F]])
    }
}

/// Tuner on/off: CC9 on channel 1, 127 = on, 0 = off (verified on the
/// pedal). The pedal sends no tuner pitch data and doesn't report tuner
/// toggles made on the pedal itself, so the UI shows the state we set.
const TUNER_CC: u8 = 9;

fn tuner_cc_value(on: bool) -> u8 {
    if on {
        127
    } else {
        0
    }
}

/// MIDI CC number of each amp knob on the TONEX Pedal (big floor unit).
///
/// Source: ThibaultDucray/TonexPedal-TouchOSC-Template v4.3
/// (`ToneX-controler-4-3-1.tosc`, controls radialGain=102,
/// radialModelVolume=103, radialPresence=106, radialDepth=107, fEqBass=23,
/// fEqMid=25, fEqTreble=28), which follows the pedal's MIDI implementation
/// chart (post Nov-2024 firmware). Every one of them sends the control's
/// 0..1 position scaled linearly to CC value 0..127; the pedal shows these
/// params as 0..10, so the mapping is linear: cc = round(v / 10 * 127).
/// Verified on hardware. Amp on/off has no CC; it is written over serial.
fn knob_cc(knob: AmpKnob) -> u8 {
    match knob {
        AmpKnob::Gain => 102,
        AmpKnob::Volume => 103,
        AmpKnob::Presence => 106,
        AmpKnob::Depth => 107,
        AmpKnob::Bass => 23,
        AmpKnob::Mid => 25,
        AmpKnob::Treble => 28,
    }
}

/// UI knob id (`amp-param-changed` in app.slint) → knob.
fn knob_from_id(id: i32) -> Option<AmpKnob> {
    usize::try_from(id).ok().and_then(|i| AmpKnob::ALL.get(i).copied())
}

/// UI knob value (0..10, 0.1 steps) → CC value 0..127.
fn knob_to_cc(v: f32) -> u8 {
    let v = (v * 10.0).round() / 10.0;
    (v.clamp(0.0, 10.0) / 10.0 * 127.0).round() as u8
}

/// After we change a value, pedal reports for it are ignored this long, so
/// late echoes of our own changes don't yank the control back.
const ECHO_GUARD: Duration = Duration::from_millis(600);

/// Bookkeeping for two-way amp sync: last CC sent per knob (to skip
/// redundant sends) and when we last changed each control.
#[derive(Default)]
struct AmpSync {
    last_cc: [Option<u8>; 7],
    knob_touched: [Option<Instant>; 7],
    enable_touched: Option<Instant>,
    /// When the user last changed the preset volume (echo guard).
    level_touched: Option<Instant>,
    /// Delay/reverb parameter mirror (see fx.rs).
    fx: fx::FxMirror,
    /// Loaded vs current preset values, for the "Changed" badge.
    diff: fx::PresetDiff,
}

impl AmpSync {
    /// Forget what the pedal holds (preset change, reconnect).
    fn clear(&mut self) {
        *self = Self::default();
    }

    fn knob_recent(&self, knob: AmpKnob) -> bool {
        self.knob_touched[knob as usize].is_some_and(|t| t.elapsed() < ECHO_GUARD)
    }

    fn enable_recent(&self) -> bool {
        self.enable_touched.is_some_and(|t| t.elapsed() < ECHO_GUARD)
    }
}

fn set_knob(ui: &AppWindow, knob: AmpKnob, v: f32) {
    match knob {
        AmpKnob::Gain => ui.set_amp_gain(v),
        AmpKnob::Volume => ui.set_amp_volume(v),
        AmpKnob::Presence => ui.set_amp_presence(v),
        AmpKnob::Depth => ui.set_amp_depth(v),
        AmpKnob::Bass => ui.set_amp_bass(v),
        AmpKnob::Mid => ui.set_amp_mid(v),
        AmpKnob::Treble => ui.set_amp_treble(v),
    }
}

fn clamp_preset(p: i32) -> i32 {
    p.clamp(MIN_PRESET, MAX_PRESET)
}

fn bank_and_pc(preset_num: i32) -> (u8, u8) {
    let idx = clamp_preset(preset_num) - 1;
    ((idx / 128) as u8, (idx % 128) as u8)
}

/// Preset names and characters read from the pedal over USB serial
/// (index 0 = preset 1).
#[derive(Clone, Default)]
struct PresetNames {
    names: Arc<Mutex<Vec<PresetMeta>>>,
    /// Current preset-list search query (as typed).
    query: Arc<Mutex<String>>,
}

fn bank_number(preset_num: i32) -> i32 {
    (clamp_preset(preset_num) - 1) / 3
}

fn slot_letter(preset_num: i32) -> char {
    ['A', 'B', 'C'][((clamp_preset(preset_num) - 1) % 3) as usize]
}

/// Short preset id as on the OLED: 0-based bank, zero-padded, plus slot
/// letter (preset 125 → "41B").
fn preset_id(preset_num: i32) -> String {
    format!("{:02}{}", bank_number(preset_num), slot_letter(preset_num))
}

/// Search: every whitespace-separated token of `query` (case-insensitive)
/// must occur in "id bank slot name character" (e.g. "46c", "dg solo",
/// "clean", "bank 14"). An empty query matches everything.
fn preset_matches(preset_num: i32, meta: &PresetMeta, query: &str) -> bool {
    let bank = bank_number(preset_num);
    let hay = format!(
        "{} bank {bank:02} {bank} slot {} {} {}",
        preset_id(preset_num),
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
fn character_kind(character: &str) -> i32 {
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
    fn lock(&self) -> MutexGuard<'_, Vec<PresetMeta>> {
        self.names.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn meta(&self, preset_num: i32) -> PresetMeta {
        let names = self.lock();
        usize::try_from(preset_num - 1)
            .ok()
            .and_then(|i| names.get(i))
            .cloned()
            .unwrap_or_default()
    }

    fn row(preset_num: i32, meta: &PresetMeta) -> PresetRow {
        PresetRow {
            preset: preset_num,
            id: preset_id(preset_num).into(),
            name: meta.name.clone().into(),
            character: meta.character.clone().into(),
            kind: character_kind(&meta.character),
        }
    }

    /// True once any preset name has been read (names survive reconnects).
    fn loaded(&self) -> bool {
        self.lock().iter().any(|m| !m.name.is_empty())
    }

    fn set_query(&self, query: &str) {
        *self.query.lock().unwrap_or_else(|e| e.into_inner()) = query.to_string();
    }

    /// Rows of the presets matching the current search query.
    fn list_model(&self) -> ModelRc<PresetRow> {
        let query = self.query.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let names = self.lock();
        let empty = PresetMeta::default();
        let rows: Vec<PresetRow> = (MIN_PRESET..=MAX_PRESET)
            .filter_map(|p| {
                let meta = names.get((p - 1) as usize).unwrap_or(&empty);
                preset_matches(p, meta, &query).then(|| Self::row(p, meta))
            })
            .collect();
        ModelRc::new(VecModel::from(rows))
    }

    /// Stores a preset's character (from a details read) and updates its
    /// list row in place if it is shown (the filter itself is re-applied
    /// on the next query change). Returns false if nothing changed.
    fn set_character(&self, ui: &AppWindow, preset_idx: u8, character: String) -> bool {
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
            model.set_row_data(row, Self::row(preset, &meta));
        }
        true
    }
}

/// OLED display: name ("" until known; bank/slot are derived in the UI)
/// and character chip of `preset`.
fn show_preset_meta(ui: &AppWindow, names: &PresetNames, preset: i32) {
    let meta = names.meta(preset);
    ui.set_preset_name(meta.name.into());
    let character = meta.character;
    ui.set_preset_character_kind(character_kind(&character));
    ui.set_preset_character(character.into());
}

/// Row of `preset` in the (filtered) list, -1 when filtered out.
fn list_row_of(ui: &AppWindow, preset: i32) -> i32 {
    ui.get_preset_names()
        .iter()
        .position(|r| r.preset == preset)
        .map_or(-1, |i| i as i32)
}

/// (Re)builds the preset list for the current query: rows, match count and
/// the current preset's row.
fn show_list(ui: &AppWindow, names: &PresetNames) {
    let model = names.list_model();
    ui.set_preset_count(model.row_count() as i32);
    ui.set_preset_names(model);
    ui.set_selected_row(list_row_of(ui, ui.get_preset()));
}

/// Shows `preset` as current in the UI. Does NOT send MIDI (used both by the
/// UI path, which sends separately, and for footswitch changes on the pedal).
fn show_preset(ui: &AppWindow, names: &PresetNames, preset: i32) {
    // Row first: the list scrolls to it when `preset` changes.
    ui.set_selected_row(list_row_of(ui, preset));
    ui.set_preset(preset);
    // Until the new preset's details arrive.
    ui.set_preset_level_known(false);
    ui.set_eq_freq_known(false);
    ui.set_amp_mix_known(false);
    show_preset_meta(ui, names, preset);
    // A (re)loaded preset starts clean.
    ui.set_preset_dirty(false);
}

/// Records a new value of preset parameter `index` and updates the
/// "Changed" badge.
///
/// The badge compares the current values with the ones the preset was
/// loaded with (`fx::PresetDiff`), so it goes off again when a control is
/// turned back. Current values come from our own writes (at send time) and
/// from every pedal report, echoes included. Globals are not part of the
/// array. The baseline is reset on preset load (`AmpSync::clear`, footswitch
/// push) and taken from the next read. Limit: edits made on the pedal before
/// that read (e.g. while the app wasn't running) become part of the baseline.
fn record_value(ui: &AppWindow, amp: &mut AmpSync, index: usize, value: f32) {
    amp.diff.set(index, value);
    ui.set_preset_dirty(amp.diff.dirty());
}

/// State shared between the UI callbacks and the serial event handler
/// (which runs in the UI thread, but must be `Send` to get there).
#[derive(Clone, Default)]
struct SyncCtx {
    /// MIDI output (program changes, amp CCs, tuner).
    midi: MidiEngine,
    /// State of both links; see `poll` / `handle_session_event`.
    link: Arc<Mutex<LinkState>>,
    names: PresetNames,
    amp: Arc<Mutex<AmpSync>>,
    /// The live serial session; replaced on reconnect.
    serial: Arc<Mutex<Option<tonex_serial::Pedal>>>,
    /// When the user last changed each global (0 trim, 1 master). Globals
    /// are pedal-wide, so this is NOT cleared on preset change.
    global_touched: Arc<Mutex<[Option<Instant>; 2]>>,
}

/// UI global id → (global index, valid range).
fn global_for_id(id: i32) -> Option<(usize, (f32, f32))> {
    match id {
        0 => Some((tonex_serial::G_INPUT_TRIM, tonex_serial::INPUT_TRIM_RANGE)),
        1 => Some((tonex_serial::G_MAIN_VOLUME, tonex_serial::MAIN_VOLUME_RANGE)),
        _ => None,
    }
}

impl SyncCtx {
    fn amp(&self) -> MutexGuard<'_, AmpSync> {
        self.amp.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn with_serial<T>(&self, f: impl FnOnce(&tonex_serial::Pedal) -> Result<T, String>) -> Result<T, String> {
        let guard = self.serial.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(pedal) => f(pedal),
            None => Err("serial session not started".into()),
        }
    }

    fn link(&self) -> MutexGuard<'_, LinkState> {
        self.link.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// (Re)starts the serial session; the previous one ends when its handle
    /// is dropped and releases the port before the new one opens it. Then
    /// reads the current preset's amp params, the globals and (once per app
    /// run: names can't change) all names.
    fn start_serial(&self, ui: slint::Weak<AppWindow>, preset: i32) {
        let id = self.link().serial_starting();
        let ctx = self.clone();
        let pedal = tonex_serial::Pedal::start(move |event| {
            let ctx = ctx.clone();
            let _ = ui.upgrade_in_event_loop(move |ui| ctx.handle_session_event(&ui, id, event));
        });
        // Params first: the knobs shouldn't wait for the ~10 s name scan.
        let _ = pedal.request_amp_params((clamp_preset(preset) - 1) as u8);
        let _ = pedal.request_globals();
        if !self.names.loaded() {
            let _ = pedal.request_names();
        }
        *self.serial.lock().unwrap_or_else(|e| e.into_inner()) = Some(pedal);
    }

    /// Forgets everything learned from the pedal except the preset names,
    /// so the next session reads it all again from scratch: the pedal may
    /// be changed while unplugged. Echo guards, dirty baseline, fx mirror
    /// and global guards are reset; the tuner is off after a power cycle.
    fn reset_pedal_state(&self, ui: &AppWindow) {
        self.amp().clear();
        *self.global_touched.lock().unwrap_or_else(|e| e.into_inner()) = [None; 2];
        ui.set_preset_dirty(false);
        ui.set_preset_level_known(false);
        ui.set_eq_freq_known(false);
    ui.set_amp_mix_known(false);
        ui.set_tuner_active(false);
    }

    /// Reconnect step (startup, every `RECONNECT_POLL` while not healthy,
    /// and the manual button): connects MIDI if it is down, and starts a
    /// serial session if serial is down and a candidate port exists.
    fn poll(&self, ui: &AppWindow) {
        let (midi, serial) = {
            let l = self.link();
            (l.midi, l.serial)
        };
        if !midi {
            match self.midi.try_connect() {
                Ok(name) => {
                    eprintln!("MIDI connected: {name}");
                    let mut l = self.link();
                    l.midi = true;
                    l.last_failure.clear();
                }
                Err(e) => {
                    if self.link().note_failure(&e) {
                        eprintln!("MIDI connect failed: {e} (retrying)");
                    }
                }
            }
        }
        if serial == SerialLink::Down {
            if tonex_serial::port_available() {
                self.start_serial(ui.as_weak(), ui.get_preset());
            } else {
                // Node gone (unplugged): the access problem is moot.
                self.link().clear_denied();
            }
        }
        self.show_link(ui);
    }

    /// The pedal is gone (or a manual reconnect): drops both links; the
    /// poll brings them back.
    fn lose_pedal(&self, ui: &AppWindow, why: &str) {
        let was_up = {
            let mut l = self.link();
            let was_up = l.midi || l.serial != SerialLink::Down;
            l.drop_all();
            was_up
        };
        if was_up {
            eprintln!("TONEX link lost: {why}; reconnecting");
        }
        self.midi.close();
        // Dropping the handle ends the old session thread (its events are
        // stale now) and frees the port for the next one.
        *self.serial.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.reset_pedal_state(ui);
        self.show_link(ui);
    }

    fn show_link(&self, ui: &AppWindow) {
        let status = self.link().status();
        ui.set_connected(status == LinkStatus::Connected);
        ui.set_reconnecting(status == LinkStatus::Reconnecting);
        ui.set_no_access(matches!(status, LinkStatus::NoAccess(_)));
        ui.set_status_text(status_text(&status).into());
    }

    /// Events of serial session `id`: connection changes update the link
    /// state; events of a stale session are dropped.
    fn handle_session_event(&self, ui: &AppWindow, id: u64, event: Event) {
        match event {
            Event::Connected(port) => {
                if self.link().serial_connected(id) {
                    eprintln!("TONEX serial session on {port}");
                    self.link().last_failure.clear();
                    // Starting already counted as connected, but a no-access
                    // hint (or RECONNECTING) may still be on screen.
                    self.show_link(ui);
                }
            }
            Event::NoAccess(path) => self.link().serial_denied(id, path),
            Event::Disconnected(reason) => {
                let end = self.link().serial_ended(id);
                match end {
                    SerialEnd::Stale => {}
                    SerialEnd::OpenFailed => {
                        *self.serial.lock().unwrap_or_else(|e| e.into_inner()) = None;
                        if self.link().note_failure(&reason) {
                            eprintln!("TONEX serial open failed: {reason} (retrying)");
                        }
                        self.show_link(ui);
                    }
                    SerialEnd::Lost => self.lose_pedal(ui, &format!("serial session ended: {reason}")),
                }
            }
            other => {
                if id == self.link().session {
                    self.handle_event(ui, other);
                }
            }
        }
    }

    fn request_amp_params(&self, preset: i32) {
        if let Err(e) = self.with_serial(|p| p.request_amp_params((clamp_preset(preset) - 1) as u8)) {
            eprintln!("TONEX amp params request failed: {e}");
        }
    }

    /// Applies a full set of amp values read from the pedal, unless the user
    /// is dragging a knob right now.
    fn apply_amp_params(&self, ui: &AppWindow, p: &tonex_serial::AmpParams) {
        if ui.get_knob_active() {
            return;
        }
        let mut amp = self.amp();
        let values = [p.gain, p.volume, p.presence, p.depth, p.bass, p.mid, p.treble];
        for (knob, v) in AmpKnob::ALL.into_iter().zip(values) {
            set_knob(ui, knob, v);
            amp.last_cc[knob as usize] = Some(knob_to_cc(v));
        }
        ui.set_amp_enabled(p.amp_enabled);
    }

    /// Runs in the UI thread for every event from the serial session.
    fn handle_event(&self, ui: &AppWindow, event: Event) {
        match event {
            // Link changes are handled in `handle_session_event`.
            Event::Connected(_) | Event::Disconnected(_) | Event::NoAccess(_) => {}
            Event::Names(Ok(names)) => {
                *self.names.lock() = names;
                show_list(ui, &self.names);
                show_preset_meta(ui, &self.names, ui.get_preset());
            }
            Event::Names(Err(e)) => eprintln!("TONEX preset names read failed: {e}"),
            Event::PresetLevel { preset_idx, value } => {
                // preset_idx: Some = details read, None = live change.
                if !preset_idx.is_none_or(|i| i32::from(i) + 1 == ui.get_preset()) {
                    return; // stale read of another preset
                }
                let mut amp = self.amp();
                match preset_idx {
                    Some(_) => amp.diff.load_level(value),
                    None => amp.diff.set_level(value),
                }
                ui.set_preset_dirty(amp.diff.dirty());
                let recent = amp.level_touched.is_some_and(|t| t.elapsed() < ECHO_GUARD);
                if !recent && !ui.get_knob_active() {
                    let (min, max) = tonex_serial::PRESET_LEVEL_RANGE;
                    ui.set_preset_level(((value * 10.0).round() / 10.0).clamp(min, max));
                    ui.set_preset_level_known(true);
                }
            }
            Event::Character { preset_idx, character } => {
                if self.names.set_character(ui, preset_idx, character) && ui.get_preset() == i32::from(preset_idx) + 1 {
                    show_preset_meta(ui, &self.names, ui.get_preset());
                }
            }
            Event::AmpParams { preset_idx, result, raw } => {
                let current = ui.get_preset() == i32::from(preset_idx) + 1;
                if current {
                    self.load_fx(ui, &raw);
                }
                match result {
                    Ok(p) if current => self.apply_amp_params(ui, &p),
                    Ok(_) => {} // stale: the UI moved on to another preset
                    Err(e) => eprintln!("TONEX amp params read failed (preset {}): {e}", i32::from(preset_idx) + 1),
                }
            }
            Event::ParamChanged { index, value } => self.apply_param_change(ui, index, value),
            Event::Globals(Ok(g)) => {
                let touched = *self.global_touched.lock().unwrap_or_else(|e| e.into_inner());
                let recent = |i: usize| touched[i].is_some_and(|t| t.elapsed() < ECHO_GUARD);
                let tenth = |v: f32| (v * 10.0).round() / 10.0;
                if !recent(0) {
                    ui.set_global_trim(tenth(g.input_trim));
                }
                if !recent(1) {
                    ui.set_global_volume(tenth(g.master_volume));
                }
            }
            Event::Globals(Err(e)) => eprintln!("TONEX global config read failed: {e}"),
            Event::CurrentPreset(preset_idx) => {
                // Show what the pedal is on, without sending MIDI back.
                let preset = i32::from(preset_idx) + 1;
                if preset != ui.get_preset() {
                    show_preset(ui, &self.names, preset);
                    self.amp().clear();
                    self.request_amp_params(preset);
                }
            }
            Event::PresetChanged { preset_idx, params, raw } => {
                let preset = i32::from(preset_idx) + 1;
                if preset != ui.get_preset() {
                    show_preset(ui, &self.names, preset);
                    self.amp().clear();
                } else {
                    // Same preset pushed again (re-selected on the pedal):
                    // it was reloaded, so its values are the new baseline.
                    self.amp().diff = fx::PresetDiff::default();
                }
                self.load_fx(ui, &raw);
                match params {
                    Some(p) => self.apply_amp_params(ui, &p),
                    None => self.request_amp_params(preset),
                }
            }
        }
    }

    /// Takes a freshly read parameter array: delay/reverb values for the
    /// cards, and the current (or, right after a preset load, baseline)
    /// values for the "Changed" badge.
    fn load_fx(&self, ui: &AppWindow, raw: &[f32]) {
        let mut amp = self.amp();
        for card in amp.fx.load(raw) {
            fx::refresh(ui, &amp.fx, card);
        }
        amp.diff.load(raw);
        ui.set_preset_dirty(amp.diff.dirty());
        fx::set_loaded_defaults(ui, &amp.diff);
    }

    /// User changed an effect-card control: write it over serial.
    fn fx_changed(&self, ui: &AppWindow, control: fx::FxControl, value: f32) {
        let Some(index) = fx::index_for(ui, control) else {
            return; // knob slot the selected modulation model doesn't have
        };
        let mut amp = self.amp();
        match self.with_serial(|p| p.set_param(index, value)) {
            Ok(()) => {
                amp.fx.wrote(index, value);
                record_value(ui, &mut amp, index, value);
                // A new model shows that model's own stored values.
                if control.is_model() {
                    fx::refresh(ui, &amp.fx, control.card());
                }
            }
            Err(e) => {
                eprintln!("TONEX {control:?} write failed: {e}");
                // Show what the pedal still has.
                fx::refresh(ui, &amp.fx, control.card());
            }
        }
    }

    /// A single parameter changed on the pedal (physical knob, or the echo
    /// of our own change).
    fn apply_param_change(&self, ui: &AppWindow, index: usize, value: f32) {
        {
            let mut amp = self.amp();
            // The badge tracks every real value, echoes and parameters the
            // app doesn't show (gate, comp, cab, ...) included.
            record_value(ui, &mut amp, index, value);
            if fx::card_of(index).is_some() {
                // None = echo of our own recent write: UI already shows it.
                if let Some(card) = amp.fx.pedal_changed(index, value) {
                    fx::refresh(ui, &amp.fx, card);
                }
                return;
            }
        }
        if index == tonex_serial::AMP_ENABLE_PARAM {
            if !self.amp().enable_recent() {
                ui.set_amp_enabled(value != 0.0);
            }
            return;
        }
        let Some(knob) = AmpKnob::from_param_index(index) else { return };
        let Some(v) = tonex_serial::knob_from_pedal(value) else {
            eprintln!("TONEX: param {index} value {value} out of range, ignored");
            return;
        };
        let mut amp = self.amp();
        if ui.get_knob_active() || amp.knob_recent(knob) {
            return; // our own edit in progress / its echo
        }
        set_knob(ui, knob, v);
        amp.last_cc[knob as usize] = Some(knob_to_cc(v));
    }
}

/// Desktop-entry id (`tx-open-studio.desktop`). GNOME on Wayland matches the
/// window's app_id against .desktop basenames to pick the dock/taskbar icon;
/// X11 shells use WM_CLASS the same way.
const APP_ID: &str = "tx-open-studio";

/// Selects the winit backend with a hook that names the window `APP_ID`
/// (Wayland app_id and X11 WM_CLASS). Must run before the first window.
fn select_backend() -> Result<(), slint::PlatformError> {
    slint::BackendSelector::new()
        .with_winit_window_attributes_hook(|attrs| {
            #[cfg(target_os = "linux")]
            let attrs = {
                use slint::winit_030::winit::platform::{wayland, x11};
                let attrs = wayland::WindowAttributesExtWayland::with_name(attrs, APP_ID, APP_ID);
                x11::WindowAttributesExtX11::with_name(attrs, APP_ID, APP_ID)
            };
            attrs
        })
        .select()
}

fn main() -> Result<(), slint::PlatformError> {
    select_backend()?;
    let ui = AppWindow::new()?;
    let ctx = SyncCtx::default();
    let engine = ctx.midi.clone();

    ui.set_preset(DEFAULT_PRESET);
    let cab_names: Vec<slint::SharedString> = (1..=fx::CAB_MODELS).map(|n| format!("Cab {n}").into()).collect();
    ui.set_cab_model_names(ModelRc::new(VecModel::from(cab_names)));
    show_list(&ui, &ctx.names);

    {
        let ui_weak = ui.as_weak();
        let ctx = ctx.clone();
        ui.on_search_changed(move |query| {
            let Some(ui) = ui_weak.upgrade() else { return };
            ctx.names.set_query(&query);
            show_list(&ui, &ctx.names);
        });
    }

    ui.on_open_releases(open_releases_page);
    start_update_check(ui.as_weak());

    // Connect now (pedal present: same as a normal start), then keep
    // polling while any link is down (no pedal: sits in RECONNECTING).
    ctx.poll(&ui);
    let reconnect_timer = slint::Timer::default();
    {
        let ui_weak = ui.as_weak();
        let ctx = ctx.clone();
        reconnect_timer.start(slint::TimerMode::Repeated, RECONNECT_POLL, move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            if !ctx.link().healthy() {
                ctx.poll(&ui);
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let engine = engine.clone();
        let ctx = ctx.clone();
        ui.on_preset_changed(move |requested| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let preset = clamp_preset(requested);

            show_preset(&ui, &ctx.names, preset);

            // The pedal loads the preset's own amp settings.
            ctx.amp().clear();
            if let Err(e) = engine.send_preset(preset) {
                ctx.lose_pedal(&ui, &format!("MIDI send failed: {e}"));
            }
            ctx.request_amp_params(preset);
        });
    }

    {
        let ui_weak = ui.as_weak();
        let engine = engine.clone();
        let ctx = ctx.clone();
        ui.on_amp_param_changed(move |id, value| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let Some(knob) = knob_from_id(id) else {
                eprintln!("unknown amp param id {id}");
                return;
            };
            let cc_value = knob_to_cc(value);
            let mut amp = ctx.amp();
            amp.knob_touched[knob as usize] = Some(Instant::now());
            if amp.last_cc[knob as usize] == Some(cc_value) {
                return;
            }
            match engine.send_cc(knob_cc(knob), cc_value) {
                Ok(()) => {
                    amp.last_cc[knob as usize] = Some(cc_value);
                    let v = (value * 10.0).round() / 10.0;
                    record_value(&ui, &mut amp, knob.param_index(), v);
                }
                Err(e) => {
                    drop(amp);
                    ctx.lose_pedal(&ui, &format!("MIDI send failed: {e}"));
                }
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let engine = engine.clone();
        let ctx = ctx.clone();
        ui.on_tuner_toggled(move |on| {
            let Some(ui) = ui_weak.upgrade() else { return };
            if let Err(e) = engine.send_cc(TUNER_CC, tuner_cc_value(on)) {
                ui.set_tuner_active(!on);
                ctx.lose_pedal(&ui, &format!("MIDI send failed: {e}"));
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let ctx = ctx.clone();
        ui.on_amp_enable_changed(move |on| {
            let Some(ui) = ui_weak.upgrade() else { return };
            ctx.amp().enable_touched = Some(Instant::now());
            let value = if on { 1.0 } else { 0.0 };
            match ctx.with_serial(|p| p.set_param(tonex_serial::AMP_ENABLE_PARAM, value)) {
                Ok(()) => record_value(&ui, &mut ctx.amp(), tonex_serial::AMP_ENABLE_PARAM, value),
                Err(e) => {
                    // No CC exists for amp on/off, so serial is the only way.
                    eprintln!("TONEX amp enable write failed: {e}");
                    ui.set_amp_enabled(!on);
                }
            }
        });
    }

    {
        let ctx = ctx.clone();
        ui.on_global_changed(move |id, value| {
            let Some((index, (min, max))) = global_for_id(id) else {
                eprintln!("unknown global id {id}");
                return;
            };
            ctx.global_touched.lock().unwrap_or_else(|e| e.into_inner())[id as usize] = Some(Instant::now());
            let result = ctx.with_serial(|p| p.set_global(index, value.clamp(min, max)));
            if let Err(e) = result {
                eprintln!("TONEX global write: {e}");
                // Show the pedal's actual value again.
                if let Err(e) = ctx.with_serial(|p| p.request_globals()) {
                    eprintln!("TONEX global config request failed: {e}");
                }
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let ctx = ctx.clone();
        ui.on_preset_level_changed(move |value| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let (min, max) = tonex_serial::PRESET_LEVEL_RANGE;
            let value = value.clamp(min, max);
            let mut amp = ctx.amp();
            amp.level_touched = Some(Instant::now());
            match ctx.with_serial(|p| p.set_preset_level(value)) {
                Ok(()) => {
                    amp.diff.set_level(value);
                    ui.set_preset_dirty(amp.diff.dirty());
                    ui.set_preset_level_known(true);
                }
                Err(e) => eprintln!("TONEX preset volume write failed: {e}"),
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let ctx = ctx.clone();
        ui.on_fx_changed(move |id, value| {
            let Some(ui) = ui_weak.upgrade() else { return };
            match fx::FxControl::from_id(id) {
                Some(control) => ctx.fx_changed(&ui, control, value),
                None => eprintln!("unknown fx control id {id}"),
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let ctx = ctx.clone();
        // Manual reconnect: drop both links and poll right away. No program
        // change: the pedal's current preset wins and arrives from the
        // serial state reply (CurrentPreset).
        ui.on_reconnect(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            ctx.lose_pedal(&ui, "reconnect requested");
            ctx.poll(&ui);
        });
    }

    ui.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tuner_cc_values() {
        assert_eq!(TUNER_CC, 9);
        assert_eq!(tuner_cc_value(true), 127);
        assert_eq!(tuner_cc_value(false), 0);
    }

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
    fn knob_maps_linearly_to_cc() {
        assert_eq!(knob_to_cc(0.0), 0);
        assert_eq!(knob_to_cc(5.0), 64); // 63.5 rounds up
        assert_eq!(knob_to_cc(10.0), 127);
        assert_eq!(knob_to_cc(6.9), 88);
        assert_eq!(knob_to_cc(-1.0), 0);
        assert_eq!(knob_to_cc(11.0), 127);
    }

    #[test]
    fn amp_param_ids_match_ui_order() {
        let ccs: Vec<u8> = (0..7).filter_map(knob_from_id).map(knob_cc).collect();
        assert_eq!(ccs, [102, 103, 106, 107, 23, 25, 28]);
        assert_eq!(knob_from_id(7), None);
        assert_eq!(knob_from_id(-1), None);
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
        assert!(preset_matches(141, &meta, ""));
        assert!(preset_matches(141, &meta, "46c"));
        assert!(preset_matches(141, &meta, "dg SOLO"));
        assert!(preset_matches(141, &meta, "  drive  bank 46 "));
        assert!(!preset_matches(141, &meta, "dg clean"));
        assert!(!preset_matches(141, &meta, "45c"));
    }

    #[test]
    fn link_state_normal_start() {
        let mut l = LinkState::default();
        assert!(!l.healthy());
        l.midi = true;
        let id = l.serial_starting();
        assert!(l.healthy(), "handshake running counts as connected");
        assert!(l.serial_connected(id));
        assert_eq!(l.serial, SerialLink::Up);
    }

    #[test]
    fn link_state_open_failure_keeps_midi() {
        let mut l = LinkState { midi: true, ..Default::default() };
        let id = l.serial_starting();
        assert_eq!(l.serial_ended(id), SerialEnd::OpenFailed);
        assert!(l.midi);
        assert!(!l.healthy());
    }

    #[test]
    fn link_state_lost_session_and_stale_events() {
        let mut l = LinkState { midi: true, ..Default::default() };
        let id = l.serial_starting();
        assert!(l.serial_connected(id));
        assert_eq!(l.serial_ended(id), SerialEnd::Lost);
        // A second end of the same session is not reported twice.
        assert_eq!(l.serial_ended(id), SerialEnd::Stale);

        // Torn down (or manual reconnect): the old session's events are stale.
        let old = l.serial_starting();
        l.drop_all();
        assert!(!l.midi);
        assert!(!l.serial_connected(old));
        assert_eq!(l.serial_ended(old), SerialEnd::Stale);

        // A replaced session can't take over the new one.
        let a = l.serial_starting();
        let b = l.serial_starting();
        assert_eq!(l.serial_ended(a), SerialEnd::Stale);
        assert!(l.serial_connected(b));
    }

    #[test]
    fn link_state_logs_each_failure_once() {
        let mut l = LinkState::default();
        assert!(l.note_failure("TONEX MIDI port not found"));
        assert!(!l.note_failure("TONEX MIDI port not found"));
        assert!(l.note_failure("other"));
    }

    #[test]
    fn link_status_prioritizes_missing_access() {
        let mut l = LinkState { midi: true, ..Default::default() };
        let path = || "/dev/ttyACM0".to_string();
        // Refused NO_ACCESS_STREAK times in a row: the hint shows.
        for _ in 0..NO_ACCESS_STREAK {
            let id = l.serial_starting();
            l.serial_denied(id, path());
            assert_eq!(l.serial_ended(id), SerialEnd::OpenFailed);
        }
        assert_eq!(l.status(), LinkStatus::NoAccess(path()));
        assert_eq!(
            status_text(&l.status()),
            "NO ACCESS TO /dev/ttyACM0 — INSTALL THE UDEV RULE (SEE README)"
        );
        // Retrying keeps the hint (no flicker); once the rule is installed
        // the next session connects and the hint goes away for good.
        let id = l.serial_starting();
        assert_eq!(l.status(), LinkStatus::NoAccess(path()));
        assert!(l.serial_connected(id));
        assert_eq!(l.status(), LinkStatus::Connected);
        assert_eq!(l.no_access, None);
        l.drop_all();
        assert_eq!(l.status(), LinkStatus::Reconnecting);
    }

    #[test]
    fn transient_access_refusal_never_shows_hint() {
        let mut l = LinkState { midi: true, ..Default::default() };
        // E.g. relaunch while the old instance still releases the port.
        for _ in 0..NO_ACCESS_STREAK - 1 {
            let id = l.serial_starting();
            l.serial_denied(id, "/dev/ttyACM0".into());
            assert_eq!(l.serial_ended(id), SerialEnd::OpenFailed);
            assert_eq!(l.status(), LinkStatus::Reconnecting);
        }
        let id = l.serial_starting();
        assert!(l.serial_connected(id));
        assert_eq!(l.status(), LinkStatus::Connected);

        // A different failure in between also restarts the count.
        l.drop_all();
        l.midi = true;
        for _ in 0..NO_ACCESS_STREAK - 1 {
            let id = l.serial_starting();
            l.serial_denied(id, "/dev/ttyACM0".into());
            l.serial_ended(id);
        }
        let id = l.serial_starting();
        assert_eq!(l.serial_ended(id), SerialEnd::OpenFailed); // e.g. no hello reply
        let id = l.serial_starting();
        l.serial_denied(id, "/dev/ttyACM0".into());
        l.serial_ended(id);
        assert_eq!(l.status(), LinkStatus::Reconnecting);
    }

    #[test]
    fn frequency_labels() {
        // The formatter lives in app.slint; call it through a component on
        // the software renderer (no display needed).
        use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
        use slint::platform::{Platform, WindowAdapter};
        use std::rc::Rc;
        struct Headless(Rc<MinimalSoftwareWindow>);
        impl Platform for Headless {
            fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
                Ok(self.0.clone())
            }
        }
        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        let _ = slint::platform::set_platform(Box::new(Headless(window)));
        let ui = AppWindow::new().unwrap();
        let hz = |v: f32| ui.global::<Format>().invoke_hz(v).to_string();
        assert_eq!(hz(75.0), "75");
        assert_eq!(hz(180.0), "180");
        assert_eq!(hz(750.0), "750");
        assert_eq!(hz(1000.0), "1K");
        assert_eq!(hz(1600.0), "1.6K");
        assert_eq!(hz(1650.0), "1.7K");
        assert_eq!(hz(4000.0), "4K");
        assert_eq!(hz(5000.0), "5K");
    }
}
