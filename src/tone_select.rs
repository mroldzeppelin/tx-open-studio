//! Model and cab choice in the app: the catalog and the per-preset cache
//! kept up to date from session events, the AMP model stepper, the CAB
//! cascading menu, the preset list's "amp · cab" lines and the library
//! status ("LIBRARY 57/192").

use crate::catalog::{amp_menu, amp_options, cab_menu, AmpOption, CabAction, Catalog, MenuNode, CATALOG_FILE};
use crate::preset_tones::{PresetCache, PRESETS_FILE};
use crate::protocol::{is_model_kind, LibraryItem, PresetSlots, SlotChoice, SlotInfo, SlotTarget, ToneInfo, KIND_AMP_MODEL, P_CAB_TYPE};
use crate::*;
use slint::ModelRc;
use slint::VecModel;
use std::sync::MutexGuard;

/// Catalog, preset cache and what the pickers show.
#[derive(Default)]
pub(crate) struct ToneState {
    pub(crate) catalog: Catalog,
    pub(crate) presets: PresetCache,
    amp_options: Vec<AmpOption>,
    cab_actions: Vec<CabAction>,
    /// Slot names of the current preset (live buffer or dump).
    current: Option<ToneInfo>,
    // Per session (reset on reconnect).
    list_known: bool,
    scan_requested: bool,
    scan_running: bool,
    /// This session's scan tried every unnamed item: items still without
    /// a name could not be read and are hidden from the pickers (retried
    /// on the next connect).
    scan_completed: bool,
}

impl ToneState {
    /// The session-scoped flags back to "nothing known yet".
    pub(crate) fn reset_session(&mut self) {
        self.list_known = false;
        self.scan_requested = false;
        self.scan_running = false;
        self.scan_completed = false;
    }
}

/// Slint menu model from the catalog's menu tree.
fn slint_menu(nodes: &[MenuNode]) -> ModelRc<MenuTop> {
    let id = |n: &MenuNode| n.action.map_or(-1, |a| a as i32);
    let tops: Vec<MenuTop> = nodes
        .iter()
        .map(|t| MenuTop {
            label: t.label.clone().into(),
            id: id(t),
            children: ModelRc::new(VecModel::from(
                t.children
                    .iter()
                    .map(|m| MenuMid {
                        label: m.label.clone().into(),
                        id: id(m),
                        children: ModelRc::new(VecModel::from(
                            m.children.iter().map(|l| MenuLeaf { label: l.label.clone().into(), id: id(l) }).collect::<Vec<_>>(),
                        )),
                    })
                    .collect::<Vec<_>>(),
            )),
        })
        .collect();
    ModelRc::new(VecModel::from(tops))
}

fn strings(items: impl Iterator<Item = String>) -> ModelRc<slint::SharedString> {
    ModelRc::new(VecModel::from(items.map(Into::into).collect::<Vec<slint::SharedString>>()))
}

impl SyncCtx {
    pub(crate) fn tone(&self) -> MutexGuard<'_, ToneState> {
        self.tone.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Loads the cached catalog and preset lines (startup): shown before
    /// the pedal answers.
    pub(crate) fn load_tone_cache(&self, ui: &AppWindow) {
        if let Some(dir) = store::data_dir() {
            let mut t = self.tone();
            t.catalog = Catalog::load(dir.join(CATALOG_FILE));
            t.presets = PresetCache::load(dir.join(PRESETS_FILE));
        }
        self.refresh_pickers(ui);
        self.refresh_tone_lines(ui);
        self.show_library(ui);
    }

    /// AMP options and CAB menu from the catalog.
    pub(crate) fn refresh_pickers(&self, ui: &AppWindow) {
        let mut t = self.tone();
        let current = t.current.as_ref().and_then(|i| Some((i.amp_id?, i.amp.as_ref()?.name.clone())));
        let placeholders = !t.scan_completed;
        let options = amp_options(&t.catalog, current.as_ref().map(|(id, name)| (id, name.as_str())), placeholders);
        let index = current.and_then(|(id, _)| options.iter().position(|o| o.id == id)).map_or(-1, |i| i as i32);
        ui.set_amp_model_options(strings(options.iter().map(|o| o.label.clone())));
        ui.set_amp_model_menu(slint_menu(&amp_menu(&options)));
        ui.set_amp_model_index(index);
        t.amp_options = options;
        // The model's own cab, by the current AMP model's name.
        let own_cab = t.current.as_ref().and_then(|i| i.amp.as_ref()).map_or_else(|| "Model cab".to_string(), |a| a.name.clone());
        let (menu, actions) = cab_menu(&t.catalog, placeholders, &own_cab);
        ui.set_cab_menu(slint_menu(&menu));
        t.cab_actions = actions;
    }

    /// Every preset's "amp · cab" line into the list.
    pub(crate) fn refresh_tone_lines(&self, ui: &AppWindow) {
        let (lines, search) = {
            let t = self.tone();
            (
                (0..PRESET_COUNT as u8).map(|i| t.presets.line(i, &t.catalog)).collect(),
                (0..PRESET_COUNT as u8).map(|i| t.presets.search_extra(i, &t.catalog)).collect(),
            )
        };
        self.names.set_tones(ui, lines, search);
    }

    pub(crate) fn show_library(&self, ui: &AppWindow) {
        let t = self.tone();
        let (known, total) = t.catalog.progress();
        ui.set_library_text(if total == 0 { "".into() } else { format!("LIBRARY {known}/{total}").into() });
        ui.set_library_scanning(t.scan_running);
    }

    /// Names of the current preset's slots: header lines, the AMP
    /// stepper's position, the CAB slot kind.
    pub(crate) fn show_tone_info(&self, ui: &AppWindow, info: Option<ToneInfo>) {
        let info = info.unwrap_or_default();
        ui.set_amp_model_text(info.amp.as_ref().map(amp_model_text).unwrap_or_default().into());
        ui.set_cab_source_text(info.cab.as_ref().map(|c| cab_source_text(c, info.amp.as_ref())).unwrap_or_default().into());
        ui.set_cab_slot_type(match info.cab {
            Some(protocol::CabSource::ModelCab) => 0,
            Some(protocol::CabSource::Ir(_)) => 3,
            Some(protocol::CabSource::Borrowed { .. }) => 4,
            None => -1,
        });
        self.tone().current = Some(info);
        self.refresh_pickers(ui);
    }

    pub(crate) fn on_library_list(&self, ui: &AppWindow, list: Result<Vec<LibraryItem>, String>) {
        match list {
            Ok(list) => {
                {
                    let mut t = self.tone();
                    if t.catalog.set_list(list) {
                        t.catalog.save();
                    }
                    t.list_known = true;
                }
                self.refresh_pickers(ui);
                self.refresh_tone_lines(ui);
                self.show_library(ui);
                self.maybe_start_scan();
            }
            Err(e) => eprintln!("TONEX library list read failed: {e}"),
        }
    }

    /// Names learned from any slot read or scan read: menus and lines
    /// update as they arrive.
    pub(crate) fn on_slots(&self, ui: &AppWindow, infos: &[SlotInfo]) {
        let changed = {
            let mut t = self.tone();
            let now = store::now_secs();
            let changed = infos.iter().fold(false, |acc, info| t.catalog.merge(info, now) | acc);
            if changed {
                t.catalog.save();
            }
            changed
        };
        if changed {
            self.refresh_pickers(ui);
            self.refresh_tone_lines(ui);
            self.show_library(ui);
        }
    }

    pub(crate) fn on_preset_slots(&self, ui: &AppWindow, preset_idx: u8, slots: &PresetSlots) {
        let changed = {
            let mut t = self.tone();
            let changed = t.presets.set(preset_idx, slots, store::now_secs());
            if changed {
                t.presets.save();
            }
            changed
        };
        if changed {
            self.refresh_tone_lines(ui);
        }
    }

    /// The live buffer after a load or on-pedal browsing: names, and the
    /// pedal's own dirty flag for EDITED.
    pub(crate) fn on_live(&self, ui: &AppWindow, preset_idx: u8, dirty: bool, info: Option<ToneInfo>) {
        if ui.get_preset() != i32::from(preset_idx) + 1 {
            return;
        }
        self.show_tone_info(ui, info);
        let mut amp = self.amp();
        amp.diff.set_pedal_dirty(dirty);
        update_dirty(ui, &amp, "model/cab load");
    }

    pub(crate) fn on_slot_applied(&self, ui: &AppWindow, target: SlotTarget, result: Result<(), String>) {
        ui.set_slot_busy(false);
        match result {
            Ok(()) if target == SlotTarget::Cab => {
                // The session set the cab type to 0 (slot / model cab).
                let mut amp = self.amp();
                amp.fx.wrote(P_CAB_TYPE, 0.0);
                record_value(ui, &mut amp, P_CAB_TYPE, 0.0, "cab load");
                if let Some(card) = fx::card_of(P_CAB_TYPE) {
                    fx::refresh(ui, &amp.fx, card);
                }
                ui.set_cab_type(0);
            }
            Ok(()) => {}
            Err(e) => {
                eprintln!("TONEX {target:?} load failed: {e}");
                ui.set_status_note("APPLY FAILED".into());
                // Back to what the pedal has.
                self.refresh_pickers(ui);
            }
        }
    }

    pub(crate) fn on_catalog_scan(&self, ui: &AppWindow, running: bool, completed: bool) {
        {
            let mut t = self.tone();
            t.scan_running = running;
            t.scan_completed |= completed;
        }
        self.show_library(ui);
        if completed {
            self.refresh_pickers(ui);
        }
    }

    /// Once per session, as soon as the library list is known: read the
    /// names of the items the cached catalog lacks.
    pub(crate) fn maybe_start_scan(&self) {
        let missing = {
            let mut t = self.tone();
            if !t.list_known || t.scan_requested {
                return;
            }
            t.scan_requested = true;
            let missing = t.catalog.missing();
            t.scan_completed = missing.is_empty();
            missing
        };
        if !missing.is_empty() {
            if let Err(e) = self.with_serial(|p| p.scan_catalog(missing)) {
                eprintln!("TONEX catalog scan request failed: {e}");
            }
        }
    }

    fn apply(&self, ui: &AppWindow, target: SlotTarget, choice: SlotChoice) {
        match self.with_serial(|p| p.apply_slot(target, choice)) {
            Ok(()) => ui.set_slot_busy(true),
            Err(e) => {
                eprintln!("TONEX {target:?} load failed: {e}");
                ui.set_status_note("APPLY FAILED".into());
                self.refresh_pickers(ui);
            }
        }
    }

    pub(crate) fn amp_model_picked(&self, ui: &AppWindow, index: i32) {
        let choice = {
            let t = self.tone();
            let Some(option) = usize::try_from(index).ok().and_then(|i| t.amp_options.get(i)) else { return };
            let kind = t.catalog.load_kind(&option.id);
            SlotChoice::Library { id: option.id, kind: if is_model_kind(kind) { kind } else { KIND_AMP_MODEL } }
        };
        self.apply(ui, SlotTarget::Amp, choice);
    }

    pub(crate) fn cab_menu_picked(&self, ui: &AppWindow, action: i32) {
        let Some(action) = usize::try_from(action).ok().and_then(|i| self.tone().cab_actions.get(i).cloned()) else { return };
        match action {
            CabAction::ModelCab => self.apply(ui, SlotTarget::Cab, SlotChoice::ModelCab),
            CabAction::Slot(choice) => self.apply(ui, SlotTarget::Cab, choice),
            CabAction::Vir(model) => {
                ui.set_cab_type(1);
                self.fx_changed(ui, fx::FxControl::CabType, 1.0);
                ui.set_cab_model(model as i32);
                self.fx_changed(ui, fx::FxControl::CabModel, model as f32);
            }
            CabAction::Off => {
                ui.set_cab_type(2);
                self.fx_changed(ui, fx::FxControl::CabType, 2.0);
            }
        }
    }

    /// Manual rescan: read every listed item's names again.
    pub(crate) fn library_rescan(&self) {
        let items = self.tone().catalog.list.clone();
        if let Err(e) = self.with_serial(|p| p.scan_catalog(items)) {
            eprintln!("TONEX catalog scan request failed: {e}");
        }
    }

    pub(crate) fn library_cancel(&self) {
        let _ = self.with_serial(|p| p.cancel_scan());
    }
}

/// The pickers' and library status' callbacks.
pub(crate) fn wire(ui: &AppWindow, ctx: &SyncCtx) {
    let (w, c) = (ui.as_weak(), ctx.clone());
    ui.on_amp_model_picked(move |i| {
        if let Some(ui) = w.upgrade() {
            c.amp_model_picked(&ui, i);
        }
    });
    let (w, c) = (ui.as_weak(), ctx.clone());
    ui.on_cab_menu_picked(move |i| {
        if let Some(ui) = w.upgrade() {
            c.cab_menu_picked(&ui, i);
        }
    });
    let c = ctx.clone();
    ui.on_library_rescan(move || c.library_rescan());
    let c = ctx.clone();
    ui.on_library_cancel(move || c.library_cancel());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::tests::{info, listed};
    use crate::test_util::headless_ui;
    use slint::Model;

    /// Before any names are known every listed item is pickable by
    /// placeholder; names replace placeholders as they arrive, in the
    /// pickers and in the preset list.
    #[test]
    fn placeholders_then_names_progressively() {
        let ui = headless_ui();
        let ctx = SyncCtx::default();
        crate::wiring::wire(&ui, &ctx);
        show_list(&ui, &ctx.names);
        ctx.on_library_list(&ui, Ok(listed(&[1, 2])));
        let rows: Vec<String> = ui.get_amp_model_options().iter().map(|s| s.to_string()).collect();
        assert_eq!(rows, ["Amp 1", "Amp 3"]);
        assert_eq!(ui.get_amp_model_menu().row_data(0).unwrap().label, "Unsorted");
        let menu = ui.get_cab_menu();
        let unsorted = menu.iter().find(|t| t.label == "Unsorted").unwrap();
        assert_eq!(unsorted.children.row_count(), 2);
        // Preset 1 uses item 1 with its own cab.
        let slots = PresetSlots { amp_id: Some([1; 16]), amp_kind: 4, cab_type: Some(0.0), ..Default::default() };
        ctx.on_preset_slots(&ui, 0, &slots);
        assert_eq!(ui.get_preset_names().row_data(0).unwrap().tone, "Amp 1");
        // A name arrives (scan read or dump).
        ctx.on_slots(&ui, &[info(1, 4, "Polite Twin", Some("Fender Twin Reverb"))]);
        let rows: Vec<String> = ui.get_amp_model_options().iter().map(|s| s.to_string()).collect();
        assert_eq!(rows, ["Polite Twin", "Amp 3"]);
        let menu = ui.get_amp_model_menu();
        let groups: Vec<String> = menu.iter().map(|g| g.label.to_string()).collect();
        assert_eq!(groups, ["Fender", "Unsorted"]);
        assert_eq!(menu.row_data(0).unwrap().children.row_data(0).unwrap().id, 0, "ids are option indices");
        assert_eq!(ui.get_preset_names().row_data(0).unwrap().tone, "Polite Twin");
        // Search matches the based-on too (not shown).
        ctx.names.set_query("fender");
        assert_eq!(ctx.names.list_model().row_count(), 1);
        assert_eq!(ui.get_library_text(), "LIBRARY 1/2");
        // Picking a placeholder is a load by id (no pedal here: reported failed).
        ctx.amp_model_picked(&ui, 1);
        assert_eq!(ui.get_status_note(), "APPLY FAILED");
        assert!(!ui.get_slot_busy());
        // A completed scan that left item 2 unnamed (the pedal refused it):
        // hidden from both pickers; shown again while the next session's
        // scan is pending.
        ctx.on_catalog_scan(&ui, false, true);
        let rows: Vec<String> = ui.get_amp_model_options().iter().map(|s| s.to_string()).collect();
        assert_eq!(rows, ["Polite Twin"]);
        assert!(!ui.get_cab_menu().iter().any(|t| t.label == "Unsorted"));
        ctx.tone().reset_session();
        ctx.refresh_pickers(&ui);
        assert_eq!(ui.get_amp_model_options().row_count(), 2);
    }

    /// The current AMP model selects its stepper entry; a VIR pick from
    /// the CAB menu switches the cab type and model in the UI.
    #[test]
    fn current_model_and_vir_pick() {
        let ui = headless_ui();
        let ctx = SyncCtx::default();
        crate::wiring::wire(&ui, &ctx);
        ctx.on_library_list(&ui, Ok(listed(&[1, 2])));
        ctx.on_slots(&ui, &[info(2, 4, "Breaking Thunder", Some("Orange Thunderverb 200"))]);
        let amp = protocol::AmpModel { name: "Breaking Thunder".into(), based_on: Some("Orange Thunderverb 200".into()) };
        ctx.show_tone_info(&ui, Some(ToneInfo { amp: Some(amp), cab: Some(protocol::CabSource::ModelCab), amp_id: Some([2; 16]), cab_id: None }));
        assert_eq!(ui.get_amp_model_index(), 0);
        assert_eq!(ui.get_amp_model_options().row_data(0).unwrap(), "Breaking Thunder");
        let (menu, actions) = cab_menu(&ctx.tone().catalog, true, "Breaking Thunder");
        assert_eq!(ui.get_cab_menu().row_data(0).unwrap().label, "Breaking Thunder", "own cab by the amp's name");
        let grn = menu[1].children.iter().flat_map(|s| s.children.iter()).find(|l| l.label == "412 GRN").unwrap().action.unwrap();
        assert_eq!(actions[grn], CabAction::Vir(33));
        // No pedal here: the writes fail, the UI shows what it knows.
        ctx.cab_menu_picked(&ui, grn as i32);
    }
}
