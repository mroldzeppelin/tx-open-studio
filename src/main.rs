mod tonex_serial;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};

use midir::{MidiOutput, MidiOutputConnection};
use slint::{ModelRc, SharedString, VecModel};

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

/// Amp knobs and their TONEX Pedal (big floor unit) MIDI CC numbers.
///
/// Source: ThibaultDucray/TonexPedal-TouchOSC-Template v4.3
/// (`ToneX-controler-4-3-1.tosc`, controls radialGain=102,
/// radialModelVolume=103, radialPresence=106, radialDepth=107, fEqBass=23,
/// fEqMid=25, fEqTreble=28), which follows the pedal's MIDI implementation
/// chart (post Nov-2024 firmware). Every one of them sends the control's
/// 0..1 position scaled linearly to CC value 0..127; the pedal shows these
/// params as 0..10, so the mapping is linear: cc = round(v / 10 * 127).
/// The order matches the `amp-param-changed` ids in app.slint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AmpParam {
    Gain,
    Volume,
    Presence,
    Depth,
    Bass,
    Mid,
    Treble,
}

impl AmpParam {
    const ALL: [AmpParam; 7] = [
        AmpParam::Gain,
        AmpParam::Volume,
        AmpParam::Presence,
        AmpParam::Depth,
        AmpParam::Bass,
        AmpParam::Mid,
        AmpParam::Treble,
    ];

    fn from_id(id: i32) -> Option<Self> {
        usize::try_from(id).ok().and_then(|i| Self::ALL.get(i).copied())
    }

    fn cc(self) -> u8 {
        match self {
            AmpParam::Gain => 102,
            AmpParam::Volume => 103,
            AmpParam::Presence => 106,
            AmpParam::Depth => 107,
            AmpParam::Bass => 23,
            AmpParam::Mid => 25,
            AmpParam::Treble => 28,
        }
    }
}

/// UI knob value (0..10, 0.1 steps) → CC value 0..127.
fn knob_to_cc(v: f32) -> u8 {
    let v = (v * 10.0).round() / 10.0;
    (v.clamp(0.0, 10.0) / 10.0 * 127.0).round() as u8
}

/// Last CC value sent per amp param, to skip redundant sends. Cleared when
/// the pedal's state changes under us (preset change, reconnect).
#[derive(Clone, Default)]
struct AmpCcCache {
    last: Rc<RefCell<[Option<u8>; 7]>>,
}

impl AmpCcCache {
    /// Returns true when `value` differs from the last value sent for `param`.
    fn changed(&self, param: AmpParam, value: u8) -> bool {
        self.last.borrow()[param as usize] != Some(value)
    }

    fn record(&self, param: AmpParam, value: u8) {
        self.last.borrow_mut()[param as usize] = Some(value);
    }

    fn clear(&self) {
        *self.last.borrow_mut() = [None; 7];
    }
}

fn set_amp_knobs(ui: &AppWindow, p: &tonex_serial::AmpParams) {
    ui.set_amp_gain(p.gain);
    ui.set_amp_volume(p.volume);
    ui.set_amp_presence(p.presence);
    ui.set_amp_depth(p.depth);
    ui.set_amp_bass(p.bass);
    ui.set_amp_mid(p.mid);
    ui.set_amp_treble(p.treble);
}

/// Reads amp params over serial on a single worker thread. Requests that
/// pile up while a read is running are coalesced to the latest one, and a
/// result is applied only if the UI still shows that preset.
#[derive(Clone)]
struct AmpParamsReader {
    tx: mpsc::Sender<i32>,
}

impl AmpParamsReader {
    fn spawn(ui: slint::Weak<AppWindow>) -> Self {
        let (tx, rx) = mpsc::channel::<i32>();
        std::thread::spawn(move || {
            while let Ok(mut preset) = rx.recv() {
                // Coalesce: only the newest pending request matters.
                while let Ok(newer) = rx.try_recv() {
                    preset = newer;
                }
                let idx = (clamp_preset(preset) - 1) as u8;
                match tonex_serial::read_amp_params(idx) {
                    Ok(params) => {
                        let _ = ui.upgrade_in_event_loop(move |ui| {
                            if ui.get_preset() == preset {
                                set_amp_knobs(&ui, &params);
                            }
                        });
                    }
                    Err(e) => eprintln!("TONEX amp params read failed (preset {preset}): {e}"),
                }
            }
        });
        Self { tx }
    }

    /// Queues a read of `preset`'s amp params (1-based).
    fn request(&self, preset: i32) {
        // Only fails if the worker is gone, which leaves the knobs as-is.
        let _ = self.tx.send(preset);
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
    loading: Arc<AtomicBool>,
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

    /// Reads names in a background thread and pushes the current preset's
    /// name into the UI. A no-op while a previous read is still running.
    fn refresh(&self, ui: slint::Weak<AppWindow>) {
        if self.loading.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.clone();
        std::thread::spawn(move || {
            let result = tonex_serial::read_preset_names();
            this.loading.store(false, Ordering::SeqCst);
            let names = match result {
                Ok(names) => names,
                Err(e) => {
                    eprintln!("TONEX preset names read failed: {e}");
                    Vec::new()
                }
            };
            *this.lock() = names;
            let _ = ui.upgrade_in_event_loop(move |ui| {
                ui.set_preset_name(this.name_for(ui.get_preset()).into());
                ui.set_preset_names(this.list_model());
            });
        });
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
    let names = PresetNames::default();
    let amp_reader = AmpParamsReader::spawn(ui.as_weak());
    let cc_cache = AmpCcCache::default();

    ui.set_preset(DEFAULT_PRESET);
    ui.set_preset_names(names.list_model());

    match engine.try_connect() {
        Ok(name) => show_connected(&ui, &name),
        Err(e) => {
            eprintln!("MIDI connect failed: {e}");
            show_disconnected(&ui);
        }
    }
    // Queue the (short) params read first so the knobs don't wait for the
    // full name scan; serial access is serialized inside tonex_serial.
    amp_reader.request(ui.get_preset());
    names.refresh(ui.as_weak());

    {
        let ui_weak = ui.as_weak();
        let engine = engine.clone();
        let names = names.clone();
        let amp_reader = amp_reader.clone();
        let cc_cache = cc_cache.clone();
        ui.on_preset_changed(move |requested| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let preset = clamp_preset(requested);

            ui.set_preset(preset);
            ui.set_preset_name(names.name_for(preset).into());

            // The pedal loads the preset's own amp settings.
            cc_cache.clear();
            if let Err(e) = engine.send_preset(preset) {
                eprintln!("MIDI send failed: {e}");
                show_disconnected(&ui);
            }
            amp_reader.request(preset);
        });
    }

    {
        let ui_weak = ui.as_weak();
        let engine = engine.clone();
        let cc_cache = cc_cache.clone();
        ui.on_amp_param_changed(move |id, value| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let Some(param) = AmpParam::from_id(id) else {
                eprintln!("unknown amp param id {id}");
                return;
            };
            let cc_value = knob_to_cc(value);
            if !cc_cache.changed(param, cc_value) {
                return;
            }
            match engine.send_cc(param.cc(), cc_value) {
                Ok(()) => cc_cache.record(param, cc_value),
                Err(e) => {
                    eprintln!("MIDI send failed: {e}");
                    show_disconnected(&ui);
                }
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let engine = engine.clone();
        let names = names.clone();
        let amp_reader = amp_reader.clone();
        let cc_cache = cc_cache.clone();
        ui.on_reconnect(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            cc_cache.clear();
            amp_reader.request(ui.get_preset());
            names.refresh(ui_weak.clone());
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
        let ccs: Vec<u8> = (0..7).filter_map(AmpParam::from_id).map(AmpParam::cc).collect();
        assert_eq!(ccs, [102, 103, 106, 107, 23, 25, 28]);
        assert_eq!(AmpParam::from_id(7), None);
        assert_eq!(AmpParam::from_id(-1), None);
    }
}
