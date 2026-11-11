use std::sync::{Arc, Mutex, MutexGuard};

use midir::{MidiOutput, MidiOutputConnection};

slint::include_modules!();

const MIN_PRESET: i32 = 1;
const MAX_PRESET: i32 = 150;
const DEFAULT_PRESET: i32 = 43;
const CLIENT_NAME: &str = "tx-open-studio";
const STATUS_DOWN: &str = "Педаль не подключена";

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

    /// Sends Bank Select (CC0) + Program Change on MIDI channel 1.
    /// On failure the connection is dropped so the UI can show "disconnected".
    fn send_preset(&self, preset_num: i32) -> Result<(), String> {
        let (bank, pc) = bank_and_pc(preset_num);
        let mut guard = self.lock();

        let result = match guard.as_mut() {
            Some(conn) => conn
                .send(&[0xB0, 0x00, bank])
                .and_then(|_| conn.send(&[0xC0, pc]))
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
}

fn clamp_preset(p: i32) -> i32 {
    p.clamp(MIN_PRESET, MAX_PRESET)
}

fn bank_and_pc(preset_num: i32) -> (u8, u8) {
    let idx = clamp_preset(preset_num) - 1;
    ((idx / 128) as u8, (idx % 128) as u8)
}

fn bank_info(preset_num: i32) -> String {
    let (bank, pc) = bank_and_pc(preset_num);
    format!("BANK {} · PC {}", bank, pc)
}

fn show_connected(ui: &AppWindow, port_name: &str) {
    ui.set_connected(true);
    ui.set_status_text(format!("Подключено: {}", port_name).into());
}

fn show_disconnected(ui: &AppWindow) {
    ui.set_connected(false);
    ui.set_status_text(STATUS_DOWN.into());
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    let engine = MidiEngine::default();

    ui.set_preset(DEFAULT_PRESET);
    ui.set_bank_info(bank_info(DEFAULT_PRESET).into());

    match engine.try_connect() {
        Ok(name) => show_connected(&ui, &name),
        Err(e) => {
            eprintln!("MIDI connect failed: {e}");
            show_disconnected(&ui);
        }
    }

    {
        let ui_weak = ui.as_weak();
        let engine = engine.clone();
        ui.on_preset_changed(move |requested| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let preset = clamp_preset(requested);

            ui.set_preset(preset);
            ui.set_bank_info(bank_info(preset).into());

            if let Err(e) = engine.send_preset(preset) {
                eprintln!("MIDI send failed: {e}");
                show_disconnected(&ui);
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let engine = engine.clone();
        ui.on_reconnect(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
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
