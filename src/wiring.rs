//! The window's callbacks (UI → pedal).

use crate::*;
use std::time::Instant;

/// Registers the window's callbacks: rename/save, search, preset changes, knobs
/// and switches, globals, tuner, tap tempo, reconnect.
pub(crate) fn wire(ui: &AppWindow, ctx: &SyncCtx) {
    let engine = ctx.midi.clone();

    // Live filter for the rename field: printable ASCII, max 32 bytes.
    ui.on_name_filter(|text| protocol::filter_name(&text).into());

    {
        // Save the current preset (hold-to-save button, only while EDITED):
        // the pedal's live edit buffer is written back as the stored preset.
        let ui_weak = ui.as_weak();
        let ctx = ctx.clone();
        ui.on_save_preset(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            if ui.get_save_busy() || !ui.get_preset_dirty() {
                return;
            }
            let preset = ui.get_preset();
            match ctx.with_serial(|p| p.save_preset((clamp_preset(preset) - 1) as u8)) {
                Ok(()) => ui.set_save_busy(true),
                Err(e) => {
                    eprintln!("TONEX: save not started: {e}");
                    ui.set_status_note("SAVE FAILED".into());
                }
            }
        });
    }

    {
        // Preset rename (list context menu). Only one at a time; the row
        // shows "saving…" until Event::Renamed.
        let ui_weak = ui.as_weak();
        let ctx = ctx.clone();
        ui.on_rename_preset(move |preset, name| {
            let Some(ui) = ui_weak.upgrade() else { return };
            if ui.get_rename_busy() != 0 || !(MIN_PRESET..=MAX_PRESET).contains(&preset) {
                return;
            }
            let name = protocol::clean_name(&name);
            if name.is_empty() || name == ctx.names.name_of(preset) {
                return; // nothing to write: same as cancel
            }
            let idx = (preset - 1) as u8;
            match ctx.with_serial(|p| p.rename_preset(idx, &name)) {
                Ok(()) => ui.set_rename_busy(preset),
                Err(e) => {
                    eprintln!("TONEX: rename not started: {e}");
                    ui.set_status_note("RENAME FAILED".into());
                }
            }
        });
    }

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

    {
        let ui_weak = ui.as_weak();
        let ctx = ctx.clone();
        ui.on_preset_changed(move |requested| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let preset = clamp_preset(requested);

            show_preset(&ui, &ctx.names, preset);

            // The pedal loads the preset's own amp settings. Selected over
            // serial; the load reads below are served after the select.
            ctx.amp().clear();
            let idx = (preset - 1) as u8;
            match ctx.with_serial(|p| p.select_preset(idx)) {
                Ok(()) => {
                    *ctx.select_echo.lock().unwrap_or_else(|e| e.into_inner()) = Some((idx, Instant::now()));
                }
                Err(e) => eprintln!("TONEX preset select failed: {e}"),
            }
            ctx.request_amp_params(preset);
            ctx.request_tone_info(preset);
        });
    }

    {
        // Amp knobs: written over serial (0x0207 section 02) at their
        // parameter index, like every other knob.
        let ui_weak = ui.as_weak();
        let ctx = ctx.clone();
        ui.on_amp_param_changed(move |id, value| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let Some(knob) = knob_from_id(id) else {
                eprintln!("unknown amp param id {id}");
                return;
            };
            let v = amp_knob_value(value);
            let mut amp = ctx.amp();
            amp.knob_touched[knob as usize] = Some(Instant::now());
            match ctx.with_serial(|p| p.set_param(knob.param_index(), v)) {
                Ok(()) => record_value(&ui, &mut amp, knob.param_index(), v, "app amp knob write"),
                Err(e) => eprintln!("TONEX {knob:?} write failed: {e}"),
            }
        });
    }

    {
        // Tuner: the only MIDI message left (CC9, on the pedal's channel).
        // A failure only takes the tuner away; MIDI comes back via the poll.
        let ui_weak = ui.as_weak();
        let engine = engine.clone();
        let ctx = ctx.clone();
        ui.on_tuner_toggled(move |on| {
            let Some(ui) = ui_weak.upgrade() else { return };
            if let Err(e) = engine.send_cc(TUNER_CC, tuner_cc_value(on)) {
                eprintln!("MIDI tuner send failed: {e}");
                ui.set_tuner_active(!on);
                ctx.link().midi = false;
                engine.close();
                ctx.show_link(&ui);
            }
        });
    }

    {
        // Tap tempo: a live control, not a preset parameter, so it never
        // touches the EDITED state (no record_value). The BPM is written
        // over serial (global 18), once per burst.
        let ui_weak = ui.as_weak();
        let ctx = ctx.clone();
        let tap = std::cell::RefCell::new(TapTempo::default());
        let burst = std::rc::Rc::new(std::cell::RefCell::new(TapBurst::default()));
        let flush = slint::Timer::default();
        ui.on_tap_tempo(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let TapResult::Counted(bpm) = tap.borrow_mut().tap(Instant::now()) else { return };
            // Instant feedback from the taps; the pedal's own value (global
            // 18) replaces it with the re-read after the write.
            if let Some(bpm) = bpm {
                ui.set_tempo_bpm((bpm * 10.0).round() / 10.0);
                burst.borrow_mut().tap(bpm);
            }
            // Debounced: each tap restarts the timer, so one BPM write (the
            // latest estimate) and one globals read follow the burst.
            let ctx = ctx.clone();
            let ui_weak = ui.as_weak();
            let burst = burst.clone();
            flush.start(slint::TimerMode::SingleShot, TAP_REREAD_DELAY, move || {
                if let (Some(bpm), Some(ui)) = (burst.borrow_mut().flush(), ui_weak.upgrade()) {
                    ctx.write_global(&ui, GLOBAL_TEMPO_ID, bpm);
                }
                if let Err(e) = ctx.with_serial(|p| p.request_globals()) {
                    eprintln!("TONEX global config request failed: {e}");
                }
            });
        });
    }

    {
        let ui_weak = ui.as_weak();
        let ctx = ctx.clone();
        ui.on_amp_enable_changed(move |on| {
            let Some(ui) = ui_weak.upgrade() else { return };
            ctx.amp().enable_touched = Some(Instant::now());
            let value = if on { 1.0 } else { 0.0 };
            match ctx.with_serial(|p| p.set_param(protocol::AMP_ENABLE_PARAM, value)) {
                Ok(()) => record_value(&ui, &mut ctx.amp(), protocol::AMP_ENABLE_PARAM, value, "app amp enable write"),
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
        // Pedal-wide settings: never part of the EDITED state.
        let ui_weak = ui.as_weak();
        ui.on_global_changed(move |id, value| {
            let Some(ui) = ui_weak.upgrade() else { return };
            ctx.write_global(&ui, id, value);
        });
    }

    {
        let ui_weak = ui.as_weak();
        let ctx = ctx.clone();
        // Front-panel VOLUME: pedal-wide, not part of the EDITED state.
        ui.on_pedal_volume_changed(move |value| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let (min, max) = protocol::VOLUME_RANGE;
            let value = value.clamp(min, max);
            *ctx.volume_touched.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
            match ctx.with_serial(|p| p.set_volume(value)) {
                Ok(()) => ui.set_pedal_volume_known(true),
                Err(e) => eprintln!("TONEX VOLUME write failed: {e}"),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::headless_ui;

    #[test]
    fn frequency_labels() {
        // The formatter lives in app.slint; call it through a component on
        // the software renderer (no display needed).
        let ui = headless_ui();
        let hz = |v: f32| ui.global::<Format>().invoke_hz(v).to_string();
        assert_eq!(hz(75.0), "75");
        assert_eq!(hz(180.0), "180");
        assert_eq!(hz(750.0), "750");
        assert_eq!(hz(1000.0), "1K");
        assert_eq!(hz(1600.0), "1.6K");
        assert_eq!(hz(1650.0), "1.7K");
        assert_eq!(hz(4000.0), "4K");
        assert_eq!(hz(5000.0), "5K");
        // PC+Name ids: 0-based program number, as the pedal shows it.
        let pc = |p: i32| ui.global::<Format>().invoke_ordinal(p).to_string();
        assert_eq!(pc(84), "083");
        assert_eq!(pc(1), "000");
        assert_eq!(pc(11), "010");
        assert_eq!(pc(150), "149");
    }
}
