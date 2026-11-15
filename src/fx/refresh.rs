//! UI glue: pushing mirrored values into the window, loaded-value defaults, cab model options.

use super::*;
use crate::AppWindow;

/// Models the UI currently shows.
pub(crate) fn ui_models(ui: &AppWindow) -> Models {
    let m = |v: i32| usize::try_from(v).unwrap_or(0);
    Models { delay: m(ui.get_dly_model()), reverb: m(ui.get_rvb_model()), modulation: m(ui.get_mod_model()) }
}

/// Pushes the mirror's values for `card` into the UI (unknown ones are left
/// as they are).
pub fn refresh(ui: &AppWindow, m: &FxMirror, card: Card) {
    let on = |i: usize| m.get(i).map(|v| v != 0.0);
    match card {
        Card::Delay => {
            if let Some(v) = on(P_DELAY_ENABLE) {
                ui.set_dly_enabled(v);
            }
            if let Some(v) = on(P_DELAY_POST) {
                ui.set_dly_post(v);
            }
            if let Some(model) = model_of(m.get(P_DELAY_MODEL), DELAY_MODELS) {
                ui.set_dly_model(model as i32);
            }
            let models = ui_models(ui);
            let get = |c: FxControl| c.param_index(models).and_then(|i| m.get(i));
            if let Some(v) = get(FxControl::DelaySync) {
                ui.set_dly_sync(v != 0.0);
            }
            if let Some(d) = model_of(get(FxControl::DelayDivision), DIVISIONS) {
                ui.set_dly_division(d as i32);
            }
            if let Some(v) = get(FxControl::DelayTime) {
                ui.set_dly_time(snap(v, (0.0, DELAY_TIME_MAX, 1.0)));
            }
            if let Some(v) = get(FxControl::DelayFeedback) {
                ui.set_dly_feedback(snap(v, PCT));
            }
            if let Some(v) = get(FxControl::DelayPingPong) {
                ui.set_dly_pingpong(v != 0.0);
            }
            if let Some(v) = get(FxControl::DelayMix) {
                ui.set_dly_mix(snap(v, PCT));
            }
        }
        Card::Reverb => {
            if let Some(v) = on(P_REVERB_ENABLE) {
                ui.set_rvb_enabled(v);
            }
            if let Some(v) = on(P_REVERB_POSITION) {
                ui.set_rvb_post(v);
            }
            if let Some(model) = model_of(m.get(P_REVERB_MODEL), REVERB_MODELS) {
                ui.set_rvb_model(model as i32);
            }
            let models = ui_models(ui);
            let get = |c: FxControl| c.param_index(models).and_then(|i| m.get(i));
            if let Some(v) = get(FxControl::ReverbTime) {
                ui.set_rvb_time(snap(v, TEN));
            }
            if let Some(v) = get(FxControl::ReverbPredelay) {
                ui.set_rvb_predelay(snap(v, (0.0, 500.0, 1.0)));
            }
            if let Some(v) = get(FxControl::ReverbColor) {
                ui.set_rvb_color(snap(v, (-10.0, 10.0, 0.1)));
            }
            if let Some(v) = get(FxControl::ReverbMix) {
                ui.set_rvb_mix(snap(v, PCT));
            }
        }
        Card::Comp => {
            if let Some(v) = on(P_COMP_ENABLE) {
                ui.set_comp_enabled(v);
            }
            if let Some(v) = on(P_COMP_POST) {
                ui.set_comp_post(v);
            }
            if let Some(v) = m.get(P_COMP_THRESHOLD) {
                ui.set_comp_threshold(snap(v, (-40.0, 0.0, 0.1)));
            }
            if let Some(v) = m.get(P_COMP_GAIN) {
                ui.set_comp_gain(snap(v, (-30.0, 10.0, 0.1)));
            }
            if let Some(v) = m.get(P_COMP_ATTACK) {
                ui.set_comp_attack(snap(v, (1.0, 51.0, 1.0)));
            }
        }
        Card::Mod => {
            if let Some(v) = on(P_MOD_ENABLE) {
                ui.set_mod_enabled(v);
            }
            if let Some(v) = on(P_MOD_POST) {
                ui.set_mod_post(v);
            }
            if let Some(model) = model_of(m.get(P_MOD_MODEL), MOD_MODELS) {
                ui.set_mod_model(model as i32);
            }
            let models = ui_models(ui);
            let get = |c: FxControl| c.param_index(models).and_then(|i| m.get(i));
            if let Some(v) = get(FxControl::ModSync) {
                ui.set_mod_sync(v != 0.0);
            }
            if let Some(d) = model_of(get(FxControl::ModDivision), DIVISIONS) {
                ui.set_mod_division(d as i32);
            }
            let specs = MOD_KNOBS[models.modulation.min(MOD_MODELS - 1)];
            let setters: [fn(&AppWindow, f32); 4] =
                [AppWindow::set_mod_k1, AppWindow::set_mod_k2, AppWindow::set_mod_k3, AppWindow::set_mod_k4];
            for (n, (spec, set)) in specs.iter().zip(setters).enumerate() {
                if let Some(v) = get(FxControl::ModKnob(n as u8)) {
                    set(ui, snap(v, *spec));
                }
            }
        }
        Card::Gate => {
            if let Some(v) = on(P_GATE_ENABLE) {
                ui.set_gate_enabled(v);
            }
            if let Some(v) = on(P_GATE_POST) {
                ui.set_gate_post(v);
            }
            if let Some(v) = m.get(P_GATE_THRESHOLD) {
                ui.set_gate_threshold(snap(v, (-100.0, 0.0, 1.0)));
            }
            if let Some(v) = m.get(P_GATE_RELEASE) {
                ui.set_gate_release(snap(v, (5.0, 500.0, 1.0)));
            }
            if let Some(v) = m.get(P_GATE_DEPTH) {
                ui.set_gate_depth(snap(v, (-100.0, -20.0, 1.0)));
            }
        }
        Card::Cab => {
            let pick = |i: usize, n: usize, set: fn(&AppWindow, i32)| {
                if let Some(x) = model_of(m.get(i), n) {
                    set(ui, x as i32);
                }
            };
            pick(P_CAB_TYPE, CAB_TYPES, AppWindow::set_cab_type);
            // Options first (extended past the named models if needed), so
            // the selector never shows an index without a label.
            if let Some(model) = model_of(m.get(P_CAB_MODEL), CAB_MODELS) {
                set_cab_model_options(ui, model);
                ui.set_cab_model(model as i32);
            }
            pick(P_CAB_MIC1, MIC_TYPES, AppWindow::set_cab_mic1);
            pick(P_CAB_MIC2, MIC_TYPES, AppWindow::set_cab_mic2);
            let knobs: [(usize, (f32, f32, f32), fn(&AppWindow, f32)); 6] = [
                (P_CAB_RESO, TEN, AppWindow::set_cab_reso),
                (P_CAB_MIC1_X, TEN, AppWindow::set_cab_mic1_x),
                (P_CAB_MIC1_Z, TEN, AppWindow::set_cab_mic1_z),
                (P_CAB_MIC2_X, TEN, AppWindow::set_cab_mic2_x),
                (P_CAB_MIC2_Z, TEN, AppWindow::set_cab_mic2_z),
                (P_CAB_BLEND, (-100.0, 100.0, 1.0), AppWindow::set_cab_blend),
            ];
            for (i, spec, set) in knobs {
                if let Some(v) = m.get(i) {
                    set(ui, snap(v, spec));
                }
            }
        }
        Card::Eq => {
            let setters: [fn(&AppWindow, f32); 3] =
                [AppWindow::set_eq_bass_freq, AppWindow::set_eq_mid_freq, AppWindow::set_eq_treble_freq];
            let mut known = true;
            for ((i, (min, max)), set) in EQ_FREQS.into_iter().zip(setters) {
                match m.get(i) {
                    Some(v) => set(ui, snap(v, (min, max, freq_step(v)))),
                    None => known = false,
                }
            }
            ui.set_eq_freq_known(known);
        }
        Card::Mix | Card::MidQ | Card::EqPost => {
            let Some(&(_, i, (min, max), step)) = SINGLES.iter().find(|s| s.0 == card) else { return };
            let v = m.get(i).map(|v| snap(v, (min, max, step)));
            match card {
                Card::Mix => {
                    if let Some(v) = v {
                        ui.set_amp_mix(v);
                    }
                    ui.set_amp_mix_known(v.is_some());
                }
                Card::MidQ => {
                    if let Some(v) = v {
                        ui.set_eq_mid_q(v);
                    }
                    ui.set_eq_mid_q_known(v.is_some());
                }
                _ => {
                    if let Some(v) = v {
                        ui.set_eq_post(v != 0.0);
                    }
                    ui.set_eq_post_known(v.is_some());
                }
            }
        }
    }
}

/// Points the double-click reset of the serial-only amp-tab knobs (EQ
/// frequencies, mix, mid Q) and the cab mic pad at the values the preset
/// was loaded with (in range only).
pub fn set_loaded_defaults(ui: &AppWindow, diff: &PresetDiff) {
    let setters: [fn(&AppWindow, f32); 3] = [
        AppWindow::set_eq_bass_freq_default,
        AppWindow::set_eq_mid_freq_default,
        AppWindow::set_eq_treble_freq_default,
    ];
    for ((i, (min, max)), set) in EQ_FREQS.into_iter().zip(setters) {
        if let Some(v) = diff.baseline(i).filter(|v| (min..=max).contains(v)) {
            set(ui, v);
        }
    }
    let (min, max) = AMP_MIX_RANGE;
    if let Some(v) = diff.baseline(P_AMP_MIX).filter(|v| (min..=max).contains(v)) {
        ui.set_amp_mix_default(v);
    }
    let (min, max) = EQ_MID_Q_RANGE;
    if let Some(v) = diff.baseline(P_EQ_MID_Q).filter(|v| (min..=max).contains(v)) {
        ui.set_eq_mid_q_default(v);
    }
    // Cab mic pad: double-click returns a mic to its loaded position.
    let mics: [(usize, fn(&AppWindow, f32)); 4] = [
        (P_CAB_MIC1_X, AppWindow::set_cab_mic1_x_default),
        (P_CAB_MIC1_Z, AppWindow::set_cab_mic1_z_default),
        (P_CAB_MIC2_X, AppWindow::set_cab_mic2_x_default),
        (P_CAB_MIC2_Z, AppWindow::set_cab_mic2_z_default),
    ];
    for (i, set) in mics {
        if let Some(v) = diff.baseline(i).filter(|v| (0.0..=10.0).contains(v)) {
            set(ui, snap(v, TEN));
        }
    }
}

/// Puts `cab_model_options(current)` into the UI unless the list already
/// has that length.
pub fn set_cab_model_options(ui: &AppWindow, current: usize) {
    use slint::Model;
    let options = cab_model_options(current);
    if ui.get_cab_model_names().row_count() != options.len() {
        let model: Vec<slint::SharedString> = options.into_iter().map(Into::into).collect();
        ui.set_cab_model_names(slint::ModelRc::new(slint::VecModel::from(model)));
    }
}

/// Parameter index for a UI change, using the models the UI shows.
pub fn index_for(ui: &AppWindow, control: FxControl) -> Option<usize> {
    control.param_index(ui_models(ui))
}
