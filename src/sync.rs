//! State shared by the UI callbacks and the serial event handler: the pedal session, link state, preset sync.

use crate::*;
use std::sync::Arc;
use std::sync::MutexGuard;
use std::sync::Mutex;
use std::time::Instant;

/// State shared between the UI callbacks and the serial event handler
/// (which runs in the UI thread, but must be `Send` to get there).
#[derive(Clone, Default)]
pub(crate) struct SyncCtx {
    /// MIDI output (program changes, amp CCs, tuner).
    pub(crate) midi: MidiEngine,
    /// State of both links; see `poll` / `handle_session_event`.
    pub(crate) link: Arc<Mutex<LinkState>>,
    pub(crate) names: PresetNames,
    pub(crate) amp: Arc<Mutex<AmpSync>>,
    /// The live serial session; replaced on reconnect.
    pub(crate) serial: Arc<Mutex<Option<protocol::Pedal>>>,
    /// Our own recent global writes (see `GlobalGuard`).
    pub(crate) global_guard: Arc<Mutex<GlobalGuard>>,
    /// Last rename (0-based preset index, when it finished), see
    /// `RENAME_ECHO_GUARD`.
    pub(crate) rename_echo: Arc<Mutex<Option<(u8, Instant)>>>,
    /// Last preset we selected (0-based index, when): the pedal pushes a
    /// summary after a select, which must not count as a footswitch load.
    pub(crate) select_echo: Arc<Mutex<Option<(u8, Instant)>>>,
    /// When the user last turned the front-panel VOLUME knob (echo guard).
    pub(crate) volume_touched: Arc<Mutex<Option<Instant>>>,
    /// Catalog, preset lines and the model/cab pickers.
    pub(crate) tone: Arc<Mutex<tone_select::ToneState>>,
}

impl SyncCtx {
    pub(crate) fn amp(&self) -> MutexGuard<'_, AmpSync> {
        self.amp.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn global_guard(&self) -> MutexGuard<'_, GlobalGuard> {
        self.global_guard.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Writes global control `id` (clamped/rounded): blob patch over
    /// serial, echo guard; the MIDI channel also switches the tuner's
    /// channel. On failure the globals are re-read to show the pedal's value.
    pub(crate) fn write_global(&self, _ui: &AppWindow, id: i32, value: f32) {
        let Some(c) = global_control(id) else {
            eprintln!("unknown global id {id}");
            return;
        };
        let value = c.sanitize(value);
        match self.with_serial(|p| p.set_global(c.index, value)) {
            Ok(()) => {
                self.global_guard().touch(c.index, Instant::now());
                if c.index == protocol::G_MIDI_CHANNEL {
                    if let Some(n) = channel_nibble(value) {
                        self.midi.set_channel(n);
                    }
                }
            }
            Err(e) => {
                eprintln!("TONEX global write: {e}");
                if let Err(e) = self.with_serial(|p| p.request_globals()) {
                    eprintln!("TONEX global config request failed: {e}");
                }
            }
        }
    }

    /// The one place a globals read reaches the UI (startup, reconnect,
    /// pushes, re-reads after taps): every GLOBAL tab control, unless we
    /// just wrote it; the MIDI channel all sends use; unlocks the tab.
    pub(crate) fn apply_globals(&self, ui: &AppWindow, g: &protocol::Globals) {
        let guard = *self.global_guard();
        let now = Instant::now();
        for c in &GLOBAL_CONTROLS {
            if !guard.recent(c.index, now) {
                show_global(ui, c.id, c.sanitize(g.get(c.index)));
            }
        }
        if !guard.recent(protocol::G_MIDI_CHANNEL, now) {
            match channel_nibble(g.get(protocol::G_MIDI_CHANNEL)) {
                Some(n) => self.midi.set_channel(n),
                None => eprintln!("TONEX: MIDI channel global {} invalid, keeping the current one", g.get(protocol::G_MIDI_CHANNEL)),
            }
        }
        ui.set_globals_known(true);
    }

    pub(crate) fn with_serial<T>(&self, f: impl FnOnce(&protocol::Pedal) -> Result<T, String>) -> Result<T, String> {
        let guard = self.serial.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(pedal) => f(pedal),
            None => Err("serial session not started".into()),
        }
    }

    pub(crate) fn link(&self) -> MutexGuard<'_, LinkState> {
        self.link.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// (Re)starts the serial session; the previous one ends when its handle
    /// is dropped and releases the port before the new one opens it. Then
    /// reads the current preset's amp params, the globals and (once per app
    /// run: names can't change) all names.
    pub(crate) fn start_serial(&self, ui: slint::Weak<AppWindow>, preset: i32) {
        let id = self.link().serial_starting();
        let ctx = self.clone();
        let pedal = protocol::Pedal::start(move |event| {
            let ctx = ctx.clone();
            let _ = ui.upgrade_in_event_loop(move |ui| ctx.handle_session_event(&ui, id, event));
        });
        // Params first: the knobs shouldn't wait for the ~10 s name scan.
        let _ = pedal.request_amp_params((clamp_preset(preset) - 1) as u8);
        let _ = pedal.request_tone_info((clamp_preset(preset) - 1) as u8);
        let _ = pedal.request_globals();
        // Pedal-wide: once per session; later changes arrive as pushes.
        let _ = pedal.request_volume();
        let _ = pedal.request_library_list();
        // Background: every preset's slots for the list lines (after the
        // name scan; user requests first). TX_NO_LIBRARY_SCAN also skips it
        // (testing without any learned names).
        if std::env::var_os("TX_NO_LIBRARY_SCAN").is_none() {
            let _ = pedal.preset_pass();
        }
        if !self.names.loaded() {
            let _ = pedal.request_names();
        }
        *self.serial.lock().unwrap_or_else(|e| e.into_inner()) = Some(pedal);
    }

    /// Forgets everything learned from the pedal except the preset names,
    /// so the next session reads it all again from scratch: the pedal may
    /// be changed while unplugged. Echo guards, dirty baseline, fx mirror
    /// and global guards are reset; the tuner is off after a power cycle.
    pub(crate) fn reset_pedal_state(&self, ui: &AppWindow) {
        self.amp().clear();
        *self.global_guard() = GlobalGuard::default();
        *self.volume_touched.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.tone().reset_session();
        ui.set_slot_busy(false);
        ui.set_library_scanning(false);
        ui.set_globals_known(false);
        // A rename/save in flight on the lost session never reports back.
        ui.set_rename_busy(0);
        ui.set_save_busy(false);
        ui.set_preset_dirty(false);
        ui.set_pedal_volume_known(false);
        ui.set_eq_freq_known(false);
    ui.set_amp_mix_known(false);
    ui.set_eq_mid_q_known(false);
    ui.set_eq_post_known(false);
        ui.set_tuner_active(false);
    }

    /// Reconnect step (startup, every `RECONNECT_POLL` while not healthy,
    /// and the manual button): connects MIDI if it is down, and starts a
    /// serial session if serial is down and a candidate port exists.
    pub(crate) fn poll(&self, ui: &AppWindow) {
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
            if protocol::port_available() {
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
    pub(crate) fn lose_pedal(&self, ui: &AppWindow, why: &str) {
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

    pub(crate) fn show_link(&self, ui: &AppWindow) {
        ui.set_midi_available(self.link().midi);
        let status = self.link().status();
        ui.set_connected(status == LinkStatus::Connected);
        ui.set_reconnecting(status == LinkStatus::Reconnecting);
        ui.set_no_access(matches!(status, LinkStatus::NoAccess(_)));
        ui.set_status_text(status_text(&status).into());
    }

    /// Events of serial session `id`: connection changes update the link
    /// state; events of a stale session are dropped.
    pub(crate) fn handle_session_event(&self, ui: &AppWindow, id: u64, event: Event) {
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

    /// Amp model / cab source names of `preset` (read after its params).
    pub(crate) fn request_tone_info(&self, preset: i32) {
        if let Err(e) = self.with_serial(|p| p.request_tone_info((clamp_preset(preset) - 1) as u8)) {
            eprintln!("TONEX tone info request failed: {e}");
        }
    }

    pub(crate) fn request_amp_params(&self, preset: i32) {
        if let Err(e) = self.with_serial(|p| p.request_amp_params((clamp_preset(preset) - 1) as u8)) {
            eprintln!("TONEX amp params request failed: {e}");
        }
    }

    /// Applies a full set of amp values read from the pedal, unless the user
    /// is dragging a knob right now.
    pub(crate) fn apply_amp_params(&self, ui: &AppWindow, p: &protocol::AmpParams) {
        if ui.get_knob_active() {
            return;
        }
        let values = [p.gain, p.volume, p.presence, p.depth, p.bass, p.mid, p.treble];
        for (knob, v) in AmpKnob::ALL.into_iter().zip(values) {
            set_knob(ui, knob, v);
        }
        ui.set_amp_enabled(p.amp_enabled);
    }

    /// Runs in the UI thread for every event from the serial session.
    pub(crate) fn handle_event(&self, ui: &AppWindow, event: Event) {
        match event {
            // Link changes are handled in `handle_session_event`.
            Event::Connected(_) | Event::Disconnected(_) | Event::NoAccess(_) => {}
            Event::Names(Ok(names)) => {
                *self.names.lock() = names;
                show_list(ui, &self.names);
                show_preset_meta(ui, &self.names, ui.get_preset());
            }
            Event::Names(Err(e)) => eprintln!("TONEX preset names read failed: {e}"),
            Event::Volume(value) => {
                // Front-panel VOLUME (read at connect, or a push): pedal-wide,
                // not preset data, so never part of the EDITED state.
                let touched = *self.volume_touched.lock().unwrap_or_else(|e| e.into_inner());
                let recent = touched.is_some_and(|t| t.elapsed() < ECHO_GUARD);
                if !recent && !ui.get_knob_active() {
                    let (min, max) = protocol::VOLUME_RANGE;
                    ui.set_pedal_volume(((value * 10.0).round() / 10.0).clamp(min, max));
                    ui.set_pedal_volume_known(true);
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
            Event::Globals(Ok(g)) => self.apply_globals(ui, &g),
            Event::Globals(Err(e)) => eprintln!("TONEX global config read failed: {e}"),
            Event::CurrentPreset(preset_idx) => {
                // Show what the pedal is on, without sending MIDI back.
                let preset = i32::from(preset_idx) + 1;
                if preset != ui.get_preset() {
                    show_preset(ui, &self.names, preset);
                    self.amp().clear();
                    self.request_amp_params(preset);
                    self.request_tone_info(preset);
                }
            }
            Event::Renamed { preset_idx, result } => {
                ui.set_rename_busy(0);
                *self.rename_echo.lock().unwrap_or_else(|e| e.into_inner()) = Some((preset_idx, Instant::now()));
                match result {
                    Ok(name) => {
                        eprintln!("TONEX: preset {} renamed to \"{name}\"", preset_id(i32::from(preset_idx) + 1));
                        self.names.set_name(ui, preset_idx, name);
                        if ui.get_preset() == i32::from(preset_idx) + 1 {
                            show_preset_meta(ui, &self.names, ui.get_preset());
                        }
                    }
                    Err(e) => {
                        eprintln!("TONEX: rename failed: {e}");
                        ui.set_status_note("RENAME FAILED".into());
                    }
                }
            }
            Event::Saved { preset_idx, result } => {
                ui.set_save_busy(false);
                match result {
                    // Saved: the current values are now the preset's stored
                    // state (EDITED clears, double-click resets follow).
                    Ok(()) if ui.get_preset() == i32::from(preset_idx) + 1 => {
                        {
                            let mut amp = self.amp();
                            amp.diff.rebaseline();
                            update_dirty(ui, &amp, "save");
                            fx::set_loaded_defaults(ui, &amp.diff);
                        }
                        self.request_tone_info(ui.get_preset());
                    }
                    Ok(()) => {} // the UI moved on to another preset meanwhile
                    Err(e) => {
                        eprintln!("TONEX: save failed: {e}");
                        ui.set_status_note("SAVE FAILED".into());
                    }
                }
            }
            Event::PresetChanged { preset_idx, params, raw } => {
                let preset = i32::from(preset_idx) + 1;
                let echo = *self.rename_echo.lock().unwrap_or_else(|e| e.into_inner());
                if preset != ui.get_preset() && echo.is_some_and(|(i, t)| i == preset_idx && t.elapsed() < RENAME_ECHO_GUARD) {
                    return; // echo of our rename write, not a footswitch change
                }
                // The summary the pedal pushes after our own select: the
                // load reads requested with the select cover it.
                {
                    let mut sel = self.select_echo.lock().unwrap_or_else(|e| e.into_inner());
                    if preset == ui.get_preset() && is_select_echo(*sel, preset_idx, Instant::now()) {
                        *sel = None;
                        return;
                    }
                }
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
                self.request_tone_info(preset);
            }
            Event::ToneInfo { preset_idx, info } => {
                if ui.get_preset() != i32::from(preset_idx) + 1 {
                    return; // stale: the UI moved on to another preset
                }
                self.show_tone_info(ui, info);
            }
            Event::LibraryList(list) => self.on_library_list(ui, list),
            Event::Slots(infos) => self.on_slots(ui, &infos),
            Event::PresetSlots { preset_idx, slots } => self.on_preset_slots(ui, preset_idx, &slots),
            Event::Live { preset_idx, dirty, info } => self.on_live(ui, preset_idx, dirty, info),
            Event::SlotApplied { target, result } => self.on_slot_applied(ui, target, result),
            Event::CatalogScan { running, completed } => self.on_catalog_scan(ui, running, completed),
            Event::PresetPass { running, .. } => self.on_preset_pass(running),
        }
    }

    /// Takes a freshly read parameter array: delay/reverb values for the
    /// cards, and the current (or, right after a preset load, baseline)
    /// values for the "Changed" badge.
    pub(crate) fn load_fx(&self, ui: &AppWindow, raw: &[f32]) {
        let mut amp = self.amp();
        for card in amp.fx.load(raw) {
            fx::refresh(ui, &amp.fx, card);
        }
        amp.diff.load(raw);
        update_dirty(ui, &amp, "parameter read");
        fx::set_loaded_defaults(ui, &amp.diff);
    }

    /// User changed an effect-card control: write it over serial.
    pub(crate) fn fx_changed(&self, ui: &AppWindow, control: fx::FxControl, value: f32) {
        let Some(index) = fx::index_for(ui, control) else {
            return; // knob slot the selected modulation model doesn't have
        };
        let mut amp = self.amp();
        match self.with_serial(|p| p.set_param(index, value)) {
            Ok(()) => {
                amp.fx.wrote(index, value);
                record_value(ui, &mut amp, index, value, "app write");
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
    pub(crate) fn apply_param_change(&self, ui: &AppWindow, index: usize, value: f32) {
        {
            let mut amp = self.amp();
            // The badge tracks every real value, echoes and parameters the
            // app doesn't show (gate, comp, cab, ...) included.
            record_value(ui, &mut amp, index, value, "pedal push");
            if fx::card_of(index).is_some() {
                // None = echo of our own recent write: UI already shows it.
                if let Some(card) = amp.fx.pedal_changed(index, value) {
                    fx::refresh(ui, &amp.fx, card);
                }
                return;
            }
        }
        if index == protocol::AMP_ENABLE_PARAM {
            if !self.amp().enable_recent() {
                ui.set_amp_enabled(value != 0.0);
            }
            return;
        }
        let Some(knob) = AmpKnob::from_param_index(index) else { return };
        let Some(v) = protocol::knob_from_pedal(value) else {
            eprintln!("TONEX: param {index} value {value} out of range, ignored");
            return;
        };
        let amp = self.amp();
        if ui.get_knob_active() || amp.knob_recent(knob) {
            return; // our own edit in progress / its echo
        }
        set_knob(ui, knob, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::headless_ui;

    /// Regression: every preset showed EDITED right after loading. A load
    /// through the real event handlers (parameter read, the pedal's push
    /// echoing our select) must not be dirty; a later live change must.
    #[test]
    fn load_preset_is_not_dirty() {
        let ui = headless_ui();
        let ctx = SyncCtx::default();
        let preset = 43;
        let idx = (preset - 1) as u8;
        ui.set_preset(preset);
        let mut raw = vec![0.0f32; protocol::PARAM_COUNT];
        raw[12] = 300.0;
        raw[15] = 750.0;
        raw[17] = 2000.0;
        raw[18] = 1.0;
        raw[20] = 5.5;
        raw[22] = 100.0;
        let params = protocol::AmpParams {
            gain: 5.5, volume: 4.2, presence: 0.0, depth: 8.4, bass: 4.3, mid: 8.1, treble: 2.6, amp_enabled: true,
        };
        ctx.handle_event(&ui, Event::AmpParams { preset_idx: idx, result: Ok(params), raw: raw.clone() });
        assert!(!ui.get_preset_dirty(), "after the parameter read");
        // The pedal's summary push right after our own select: ignored.
        *ctx.select_echo.lock().unwrap() = Some((idx, Instant::now()));
        let mut pushed = raw.clone();
        pushed[20] = 9.0;
        ctx.handle_event(&ui, Event::PresetChanged { preset_idx: idx, params: Some(params), raw: pushed });
        assert!(!ui.get_preset_dirty(), "after the select echo");
        // A later live change (pedal push) is an edit.
        ctx.handle_event(&ui, Event::ParamChanged { index: 20, value: 7.0 });
        assert!(ui.get_preset_dirty(), "a live change marks EDITED");
    }

    /// The front-panel VOLUME is the pedal's master volume, persistent
    /// across presets: its read and pushes set the GLOBAL knob and never
    /// the EDITED state.
    #[test]
    fn volume_is_pedal_wide_and_never_edited() {
        let ui = headless_ui();
        let ctx = SyncCtx::default();
        crate::wiring::wire(&ui, &ctx);
        ui.set_preset(10);
        ctx.handle_event(&ui, Event::AmpParams {
            preset_idx: 9,
            result: Err("no params".into()),
            raw: vec![0.0; protocol::PARAM_COUNT],
        });
        assert!(!ui.get_pedal_volume_known());
        ctx.handle_event(&ui, Event::Volume(6.5)); // the read at connect
        assert!(ui.get_pedal_volume_known());
        assert_eq!(ui.get_pedal_volume(), 6.5);
        ctx.handle_event(&ui, Event::Volume(3.04)); // turned on the pedal
        assert_eq!(ui.get_pedal_volume(), 3.0);
        assert!(!ui.get_preset_dirty(), "VOLUME is not preset data");
    }

    /// Switching presets (app select, rapid scroll, footswitch push)
    /// leaves the VOLUME knob as it is and doesn't mark EDITED.
    #[test]
    fn preset_switch_keeps_volume_and_edited() {
        let ui = headless_ui();
        let ctx = SyncCtx::default();
        crate::wiring::wire(&ui, &ctx);
        ui.set_preset(10);
        ctx.handle_event(&ui, Event::Volume(4.0));
        ui.invoke_preset_changed(44);
        for preset in 45..=49 {
            ui.invoke_preset_changed(preset);
        }
        assert_eq!(ui.get_preset(), 49);
        let raw = vec![0.0f32; protocol::PARAM_COUNT];
        ctx.handle_event(&ui, Event::PresetChanged { preset_idx: 2, params: None, raw });
        assert_eq!(ui.get_preset(), 3);
        assert!(ui.get_pedal_volume_known(), "still known after switching");
        assert_eq!(ui.get_pedal_volume(), 4.0);
        assert!(!ui.get_preset_dirty());
    }
}
