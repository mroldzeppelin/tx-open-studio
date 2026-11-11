mod fx;
mod tonex_serial;

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use midir::{MidiOutput, MidiOutputConnection};
use slint::{ModelRc, SharedString, VecModel};
use tonex_serial::{AmpKnob, Event};

slint::include_modules!();

const MIN_PRESET: i32 = 1;
const MAX_PRESET: i32 = 150;
const DEFAULT_PRESET: i32 = 43;
const CLIENT_NAME: &str = "tx-open-studio";
const STATUS_DOWN: &str = "Pedal not connected";

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
    /// Delay/reverb parameter mirror (see fx.rs).
    fx: fx::FxMirror,
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

/// Preset names read from the pedal over USB serial (index 0 = preset 1).
#[derive(Clone, Default)]
struct PresetNames {
    names: Arc<Mutex<Vec<String>>>,
}

/// Display label: the pedal groups presets in banks of 3 (A/B/C), so
/// preset 125 with name "FULDRV" is shown as "41 FULDRV". With no name the
/// label is just the preset number ("43").
fn display_label(preset_num: i32, name: &str) -> String {
    if name.is_empty() {
        preset_num.to_string()
    } else {
        format!("{} {}", (clamp_preset(preset_num) - 1) / 3, name)
    }
}

impl PresetNames {
    fn lock(&self) -> MutexGuard<'_, Vec<String>> {
        self.names.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Label for the current-preset card, or "" when the name is unknown
    /// (the UI then falls back to the bare number).
    fn name_for(&self, preset_num: i32) -> String {
        let names = self.lock();
        usize::try_from(preset_num - 1)
            .ok()
            .and_then(|i| names.get(i))
            .filter(|n| !n.is_empty())
            .map(|n| display_label(preset_num, n))
            .unwrap_or_default()
    }

    /// Labels for all presets, for the list view.
    fn list_model(&self) -> ModelRc<SharedString> {
        let names = self.lock();
        let labels: Vec<SharedString> = (MIN_PRESET..=MAX_PRESET)
            .map(|p| {
                let name = names.get((p - 1) as usize).map(String::as_str).unwrap_or("");
                display_label(p, name).into()
            })
            .collect();
        ModelRc::new(VecModel::from(labels))
    }
}

/// Shows `preset` as current in the UI. Does NOT send MIDI (used both by the
/// UI path, which sends separately, and for footswitch changes on the pedal).
fn show_preset(ui: &AppWindow, names: &PresetNames, preset: i32) {
    ui.set_preset(preset);
    ui.set_preset_name(names.name_for(preset).into());
}

/// State shared between the UI callbacks and the serial event handler
/// (which runs in the UI thread, but must be `Send` to get there).
#[derive(Clone, Default)]
struct SyncCtx {
    names: PresetNames,
    amp: Arc<Mutex<AmpSync>>,
    /// The live serial session; replaced on reconnect.
    serial: Arc<Mutex<Option<tonex_serial::Pedal>>>,
    /// When the user last changed each global (0 trim, 1 master). Globals
    /// are pedal-wide, so this is NOT cleared on preset change.
    global_touched: Arc<Mutex<[Option<Instant>; 2]>>,
}

/// Global writes resend the whole global config blob (format from Builty,
/// unverified on hardware; a bad write could alter other pedal-wide
/// settings such as the MIDI channel), so they are opt-in for now.
const GLOBAL_WRITE_ENV: &str = "TX_ENABLE_GLOBAL_WRITE";

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

    /// (Re)starts the serial session; the previous one ends when its handle
    /// is dropped and releases the port before the new one opens it. Then
    /// reads the current preset's amp params and all names.
    fn start_serial(&self, ui: slint::Weak<AppWindow>, preset: i32) {
        let ctx = self.clone();
        let pedal = tonex_serial::Pedal::start(move |event| {
            let ctx = ctx.clone();
            let _ = ui.upgrade_in_event_loop(move |ui| ctx.handle_event(&ui, event));
        });
        // Params first: the knobs shouldn't wait for the ~10 s name scan.
        let _ = pedal.request_amp_params((clamp_preset(preset) - 1) as u8);
        let _ = pedal.request_globals();
        let _ = pedal.request_names();
        *self.serial.lock().unwrap_or_else(|e| e.into_inner()) = Some(pedal);
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
            Event::Connected(port) => eprintln!("TONEX serial session on {port}"),
            Event::Disconnected(reason) => {
                eprintln!("TONEX serial session ended: {reason} (press Reconnect to retry)")
            }
            Event::Names(Ok(names)) => {
                *self.names.lock() = names;
                ui.set_preset_name(self.names.name_for(ui.get_preset()).into());
                ui.set_preset_names(self.names.list_model());
            }
            Event::Names(Err(e)) => eprintln!("TONEX preset names read failed: {e}"),
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
            Event::PresetChanged { preset_idx, params, raw } => {
                let preset = i32::from(preset_idx) + 1;
                if preset != ui.get_preset() {
                    show_preset(ui, &self.names, preset);
                    self.amp().clear();
                }
                self.load_fx(ui, &raw);
                match params {
                    Some(p) => self.apply_amp_params(ui, &p),
                    None => self.request_amp_params(preset),
                }
            }
        }
    }

    /// A single parameter changed on the pedal (physical knob, or the echo
    /// of our own change).
    /// Takes delay/reverb values from a freshly read parameter array.
    fn load_fx(&self, ui: &AppWindow, raw: &[f32]) {
        let mut amp = self.amp();
        for card in amp.fx.load(raw) {
            fx::refresh(ui, &amp.fx, card);
        }
    }

    /// User changed a delay/reverb control: write it over serial.
    fn fx_changed(&self, ui: &AppWindow, control: fx::FxControl, value: f32) {
        let index = fx::index_for(ui, control);
        let mut amp = self.amp();
        match self.with_serial(|p| p.set_param(index, value)) {
            Ok(()) => {
                amp.fx.wrote(index, value);
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

    fn apply_param_change(&self, ui: &AppWindow, index: usize, value: f32) {
        {
            let mut amp = self.amp();
            if fx::card_of(index).is_some() {
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
            return;
        }
        set_knob(ui, knob, v);
        amp.last_cc[knob as usize] = Some(knob_to_cc(v));
    }
}

fn show_connected(ui: &AppWindow, port_name: &str) {
    ui.set_connected(true);
    ui.set_status_text(format!("Connected: {}", port_name).into());
}

fn show_disconnected(ui: &AppWindow) {
    ui.set_connected(false);
    ui.set_status_text(STATUS_DOWN.into());
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    let engine = MidiEngine::default();
    let ctx = SyncCtx::default();

    ui.set_preset(DEFAULT_PRESET);
    ui.set_preset_names(ctx.names.list_model());

    match engine.try_connect() {
        Ok(name) => show_connected(&ui, &name),
        Err(e) => {
            eprintln!("MIDI connect failed: {e}");
            show_disconnected(&ui);
        }
    }
    ctx.start_serial(ui.as_weak(), ui.get_preset());

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
                eprintln!("MIDI send failed: {e}");
                show_disconnected(&ui);
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
                Ok(()) => amp.last_cc[knob as usize] = Some(cc_value),
                Err(e) => {
                    eprintln!("MIDI send failed: {e}");
                    show_disconnected(&ui);
                }
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
            if let Err(e) = ctx.with_serial(|p| p.set_param(tonex_serial::AMP_ENABLE_PARAM, value)) {
                // No CC exists for amp on/off, so serial is the only way.
                eprintln!("TONEX amp enable write failed: {e}");
                ui.set_amp_enabled(!on);
            }
        });
    }

    {
        let ctx = ctx.clone();
        let write_enabled = std::env::var_os(GLOBAL_WRITE_ENV).is_some();
        ui.on_global_changed(move |id, value| {
            let Some((index, (min, max))) = global_for_id(id) else {
                eprintln!("unknown global id {id}");
                return;
            };
            let result = if write_enabled {
                ctx.global_touched.lock().unwrap_or_else(|e| e.into_inner())[id as usize] = Some(Instant::now());
                ctx.with_serial(|p| p.set_global(index, value.clamp(min, max)))
            } else {
                Err(format!("global writes are disabled (unverified format); set {GLOBAL_WRITE_ENV}=1 to enable"))
            };
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
        let engine = engine.clone();
        let ctx = ctx.clone();
        ui.on_reconnect(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            ctx.amp().clear();
            ctx.start_serial(ui_weak.clone(), ui.get_preset());
            match engine.try_connect() {
                Ok(name) => {
                    show_connected(&ui, &name);
                    // Re-sync the pedal with what the UI shows.
                    if let Err(e) = engine.send_preset(ui.get_preset()) {
                        eprintln!("MIDI send failed: {e}");
                        show_disconnected(&ui);
                    }
                }
                Err(e) => {
                    eprintln!("MIDI connect failed: {e}");
                    show_disconnected(&ui);
                }
            }
        });
    }

    ui.run()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
