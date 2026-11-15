//! Session side of model/cab choice: the library list, ID-only slot loads
//! (apply), the catalog scan (probe each library id in the CAB slot to
//! learn its name), the background preset pass and the browse refresh.

use super::*;
use std::collections::VecDeque;
use std::time::Duration;
use std::time::Instant;

/// After the pedal's last browse push, wait this long before reading the
/// live buffer (one read per pause in the browsing).
pub(crate) const BROWSE_DEBOUNCE: Duration = Duration::from_millis(300);

/// Live buffer polling while the pedal fills in an id-only slot.
pub(crate) const FILL_POLL: Duration = Duration::from_millis(120);
pub(crate) const FILL_TIMEOUT: Duration = Duration::from_millis(1500);

/// After the scan's restore, cab-type pushes (echoes of the scan's own
/// writes) stay hidden from the app this long.
pub(crate) const SCAN_ECHO_GUARD: Duration = Duration::from_millis(2000);

/// Parameter pushes the app must not see: the cab type while a scan has
/// the live buffer, and shortly after its restore. They echo the scan's
/// own temporary writes and must not touch EDITED or the baseline.
pub(crate) fn hides_scan_echo(index: usize, scanning: bool, echo_until: Option<Instant>, now: Instant) -> bool {
    index == P_CAB_TYPE && (scanning || echo_until.is_some_and(|t| now < t))
}

/// Background work (scan probes, preset pass) waits this long after the
/// last user request.
pub(crate) const BACKGROUND_QUIET: Duration = Duration::from_millis(1500);

/// The live buffer as it was before the scan's first probe (only what the
/// scan changes is restored: the CAB slot, contents `/2`, cab type).
pub(crate) struct SavedLive {
    pub(crate) idx: u8,
    pub(crate) cab_slot: Vec<u8>,
    pub(crate) cab_flag: Vec<u8>,
    pub(crate) cab_type: f32,
    /// Live body with the cab off: what each probe is built from. Pushed
    /// parameter changes are patched in, so probes don't undo them.
    pub(crate) probe_base: Vec<u8>,
    /// Something outside the scan changed the live buffer (browse push):
    /// restore now, resume later from a fresh read.
    pub(crate) external: bool,
}

#[derive(Default)]
pub(crate) struct CatalogScan {
    pub(crate) pending: VecDeque<LibraryId>,
    pub(crate) total: usize,
    pub(crate) done: usize,
    pub(crate) saved: Option<SavedLive>,
}

/// Outcome of polling for an id-only slot to be filled in.
pub(crate) enum Fill {
    /// `info`: None when the item's names don't read (loaded anyway).
    Filled { body: Vec<u8>, idx: u8, dirty: bool, info: Option<SlotInfo>, after: Duration },
    /// The active preset is no longer the expected one.
    PresetChanged(u8),
    Timeout(String),
}

pub(crate) fn hex_id(id: &LibraryId) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

impl<F: Fn(Event)> Session<'_, F> {
    /// Requests the library list and returns its used items.
    pub(crate) fn read_library_list(&mut self) -> Result<Result<Vec<LibraryItem>, String>, String> {
        self.link.send(&LIBRARY_LIST_REQUEST)?;
        let until = Instant::now() + LIBRARY_LIST_TIMEOUT;
        while let Some(payload) = self.link.read_frame(until.saturating_duration_since(Instant::now()), until)? {
            if frame_type(&payload) == Some(TYPE_LIBRARY_LIST) {
                return Ok(parse_library_list(&payload));
            }
            self.dispatch(&payload, false);
        }
        Ok(Err("no library list received".into()))
    }

    /// Reports what a dump or live body holds: catalog names and the
    /// preset's slot summary.
    pub(crate) fn report_body(&self, idx: u8, body: &[u8]) {
        let infos = slot_infos(body);
        if !infos.is_empty() {
            (self.on_event)(Event::Slots(infos));
        }
        if let Some(slots) = preset_slots(body) {
            (self.on_event)(Event::PresetSlots { preset_idx: idx, slots });
        }
    }

    /// `report_body` plus `Event::Live` for the live buffer.
    pub(crate) fn report_live(&self, body: &[u8], idx: u8, dirty: bool) {
        self.report_body(idx, body);
        (self.on_event)(Event::Live { preset_idx: idx, dirty, info: tone_info(body) });
    }

    /// Reads frames for `dur`, dispatching all but preset pushes (a live
    /// write may be answered with one; it is no footswitch change).
    pub(crate) fn pump(&mut self, dur: Duration) -> Result<(), String> {
        let until = Instant::now() + dur;
        while let Some(payload) = self.link.read_frame(until.saturating_duration_since(Instant::now()), until)? {
            if frame_type(&payload) != Some(TYPE_PRESET_DETAILS) {
                self.dispatch(&payload, false);
            }
        }
        Ok(())
    }

    /// The live buffer body: Ok((body, active preset, dirty)).
    pub(crate) fn fetch_live_body(&mut self) -> Result<Result<(Vec<u8>, u8, bool), String>, String> {
        Ok(self.fetch_live()?.and_then(|(payload, idx, dirty)| live_body(&payload).map(|(b, ..)| (b.to_vec(), idx, dirty))))
    }

    /// Sends `body`'s contents to the live edit buffer.
    pub(crate) fn write_live(&mut self, body: &[u8]) -> Result<Result<(), String>, String> {
        self.own_write = true;
        match live_write_body(body).and_then(|b| preset_write_request(&b)) {
            Ok(request) => self.link.send(&request).map(Ok),
            Err(e) => Ok(Err(e)),
        }
    }

    pub(crate) fn send_param(&mut self, index: usize, value: f32) -> Result<(), String> {
        self.link.send(&set_param_request(PARAM_SECTION, index as u8, value))
    }

    /// Polls the live buffer until `target` holds `id` filled in by the
    /// pedal (name or data no longer zero), up to `FILL_TIMEOUT`.
    pub(crate) fn await_fill(&mut self, target: SlotTarget, id: &LibraryId, idx: u8) -> Result<Fill, String> {
        let start = Instant::now();
        let mut problem = "the pedal did not load the item".to_string();
        loop {
            self.pump(FILL_POLL)?;
            match self.fetch_live_body()? {
                Ok((_, active, _)) if active != idx => return Ok(Fill::PresetChanged(active)),
                Ok((body, active, dirty)) => {
                    if let Some(info) = filled_slot(&body, target, id) {
                        return Ok(Fill::Filled { body, idx: active, dirty, info, after: start.elapsed() });
                    }
                }
                Err(e) => problem = format!("reading the live buffer: {e}"),
            }
            if start.elapsed() >= FILL_TIMEOUT {
                return Ok(Fill::Timeout(problem));
            }
        }
    }

    /// The apply operation (see `Pedal::apply_slot`). Inner Err: not
    /// loaded or not verified; outer Err: link failure.
    pub(crate) fn apply_slot(&mut self, target: SlotTarget, choice: &SlotChoice) -> Result<Result<(), String>, String> {
        let (body, idx, _) = match self.fetch_live_body()? {
            Ok(l) => l,
            Err(e) => return Ok(Err(format!("reading the live buffer: {e}"))),
        };
        let edited = match (target, choice) {
            (_, SlotChoice::Library { id, kind }) => with_library_slot(&body, target, id, *kind),
            (SlotTarget::Cab, SlotChoice::ModelCab) => model_cab_body(&body),
            (SlotTarget::Amp, SlotChoice::ModelCab) => Err("the AMP slot has no model cab".into()),
        };
        let edited = match edited {
            Ok(b) => b,
            Err(e) => return Ok(Err(e)),
        };
        if let Err(e) = self.write_live(&edited)? {
            return Ok(Err(e));
        }
        if target == SlotTarget::Cab {
            // The slot (or the model's own cab) plays.
            self.send_param(P_CAB_TYPE, CAB_TYPE_SLOT)?;
        }
        match choice {
            SlotChoice::Library { id, .. } => match self.await_fill(target, id, idx)? {
                Fill::Filled { body, idx, dirty, after, .. } => {
                    if self.debug {
                        eprintln!("TONEX serial: {} loaded into {target:?} after {} ms", hex_id(id), after.as_millis());
                    }
                    self.report_live(&body, idx, dirty);
                    Ok(Ok(()))
                }
                Fill::PresetChanged(active) => {
                    (self.on_event)(Event::CurrentPreset(active));
                    Ok(Err("the preset changed during the load".into()))
                }
                Fill::Timeout(e) => Ok(Err(e)),
            },
            SlotChoice::ModelCab => {
                self.pump(WRITE_SETTLE)?;
                match self.fetch_live_body()? {
                    Ok((body, active, dirty)) if active == idx && model_cab_active(&body) => {
                        self.report_live(&body, active, dirty);
                        Ok(Ok(()))
                    }
                    Ok(_) => Ok(Err("the pedal did not switch to the model cab".into())),
                    Err(e) => Ok(Err(format!("verifying: {e}"))),
                }
            }
        }
    }

    /// The browse refresh: the live buffer after the pedal's MODEL
    /// encoder moved.
    pub(crate) fn refresh_live(&mut self) -> Result<(), String> {
        self.browse_due = None;
        match self.fetch_live_body()? {
            Ok((body, idx, dirty)) => self.report_live(&body, idx, dirty),
            Err(e) => {
                if self.debug {
                    eprintln!("TONEX serial: live buffer read after browsing: {e}");
                }
            }
        }
        Ok(())
    }

    /// Starts (replaces) a catalog scan of `ids`.
    pub(crate) fn start_scan(&mut self, ids: Vec<LibraryId>) {
        let mut pending = VecDeque::new();
        for id in ids {
            if !pending.contains(&id) {
                pending.push_back(id);
            }
        }
        self.scan.total = pending.len();
        self.scan.done = 0;
        self.scan.pending = pending;
        (self.on_event)(Event::CatalogScan { running: self.scan.total > 0, completed: self.scan.total == 0 });
    }

    pub(crate) fn cancel_scan(&mut self) {
        if !self.scan.pending.is_empty() {
            self.scan.pending.clear();
            (self.on_event)(Event::CatalogScan { running: false, completed: false });
        }
    }

    /// Whether a scan probe (or the final restore) is due.
    pub(crate) fn scan_ready(&self, quiet: bool) -> bool {
        (quiet && !self.scan.pending.is_empty()) || (self.scan.pending.is_empty() && self.scan.saved.is_some())
    }

    /// Saves what the scan changes and switches the cab off (so probes
    /// are not heard).
    pub(crate) fn scan_begin(&mut self) -> Result<Result<(), String>, String> {
        let (body, idx, _) = match self.fetch_live_body()? {
            Ok(l) => l,
            Err(e) => return Ok(Err(format!("reading the live buffer: {e}"))),
        };
        let saved = (|| {
            Ok::<_, String>(SavedLive {
                idx,
                cab_slot: slot_bytes(&body, SlotTarget::Cab).ok_or("cab slot not found")?.to_vec(),
                cab_flag: span_at(&body, &CAB_FLAG_PATH).map(|r| body[r].to_vec()).ok_or("cab flag not found")?,
                cab_type: param_value(&body, P_CAB_TYPE).ok_or("cab type not found")?,
                probe_base: patch_param(&body, P_CAB_TYPE, CAB_TYPE_OFF)?,
                external: false,
            })
        })();
        match saved {
            Ok(saved) => {
                self.send_param(P_CAB_TYPE, CAB_TYPE_OFF)?;
                self.scan.saved = Some(saved);
                Ok(Ok(()))
            }
            Err(e) => Ok(Err(e)),
        }
    }

    /// One scan step: the next probe, or the restore after the last.
    pub(crate) fn scan_step(&mut self) -> Result<(), String> {
        let Some(&id) = self.scan.pending.front() else {
            return self.scan_restore();
        };
        if self.scan.saved.is_none() {
            if let Err(e) = self.scan_begin()? {
                eprintln!("TONEX catalog scan stopped: {e}");
                self.cancel_scan();
                return Ok(());
            }
        }
        let Some(saved) = self.scan.saved.as_ref() else { return Ok(()) };
        let idx = saved.idx;
        // Probed with the amp-model kind; the pedal's answer tells IRs apart.
        let probe = with_library_slot(&saved.probe_base, SlotTarget::Cab, &id, KIND_AMP_MODEL);
        let outcome = match probe {
            Ok(body) => match self.write_live(&body)? {
                Ok(()) => Some(self.await_fill(SlotTarget::Cab, &id, idx)?),
                Err(e) => {
                    eprintln!("TONEX catalog scan: {}: {e}", hex_id(&id));
                    None
                }
            },
            Err(e) => {
                eprintln!("TONEX catalog scan: {}: {e}", hex_id(&id));
                None
            }
        };
        match outcome {
            Some(Fill::Filled { body, info, after, .. }) => {
                // The AMP model changed under the probe (browsed on the
                // pedal): step aside, keeping the new model.
                let amp_of = |b: &[u8]| parse_tree(b).and_then(|t| t.at(&SlotTarget::Amp.path()).and_then(slot_id));
                if let Some(saved) = self.scan.saved.as_mut() {
                    if amp_of(&body) != amp_of(&saved.probe_base) {
                        saved.external = true;
                    }
                }
                if self.debug {
                    let raw_kind = parse_tree(&body).and_then(|t| t.at(&[3, 3, 2]).and_then(Node::int));
                    eprintln!(
                        "TONEX catalog scan: {} {:?} filled after {} ms (pedal kind {raw_kind:?}, taken as {:?})",
                        hex_id(&id),
                        info.as_ref().map(|i| i.name.as_str()),
                        after.as_millis(),
                        info.as_ref().map(|i| i.kind)
                    );
                }
                if let Some(info) = info {
                    (self.on_event)(Event::Slots(vec![info]));
                }
            }
            Some(Fill::PresetChanged(active)) => {
                // The pedal moved on: nothing to restore; retry this id later.
                self.scan.saved = None;
                (self.on_event)(Event::CurrentPreset(active));
                return Ok(());
            }
            Some(Fill::Timeout(e)) => eprintln!("TONEX catalog scan: {}: {e}", hex_id(&id)),
            None => {}
        }
        self.scan.pending.pop_front();
        self.scan.done += 1;
        let running = !self.scan.pending.is_empty();
        if !running {
            self.scan_restore()?;
        }
        if !running {
            (self.on_event)(Event::CatalogScan { running: false, completed: true });
        }
        Ok(())
    }

    /// Puts back what the scan changed into the current live buffer (the
    /// CAB slot, contents `/2`, the cab type), if the pedal is still on the
    /// same preset. Everything else in the live buffer stays as it is now.
    pub(crate) fn scan_restore(&mut self) -> Result<(), String> {
        let Some(saved) = self.scan.saved.take() else { return Ok(()) };
        self.scan_echo_until = Some(Instant::now() + SCAN_ECHO_GUARD);
        let (body, idx, _) = match self.fetch_live_body()? {
            Ok(l) => l,
            Err(e) => {
                eprintln!("TONEX catalog scan: restore skipped: {e}");
                return Ok(());
            }
        };
        if idx != saved.idx {
            (self.on_event)(Event::CurrentPreset(idx));
            return Ok(());
        }
        let restored = splice(&body, &SlotTarget::Cab.path(), &saved.cab_slot)
            .and_then(|b| splice(&b, &CAB_FLAG_PATH, &saved.cab_flag))
            .and_then(|b| patch_param(&b, P_CAB_TYPE, saved.cab_type));
        match restored {
            Ok(b) => {
                if let Err(e) = self.write_live(&b)? {
                    eprintln!("TONEX catalog scan: restore failed: {e}");
                }
            }
            Err(e) => eprintln!("TONEX catalog scan: restore failed: {e}"),
        }
        self.pump(WRITE_SETTLE)?;
        self.send_param(P_CAB_TYPE, saved.cab_type)?;
        self.scan_echo_until = Some(Instant::now() + SCAN_ECHO_GUARD);
        Ok(())
    }

    /// Bookkeeping for frames seen while a scan has the live buffer: a
    /// parameter pushed by the pedal goes into the probe base; a browse
    /// push makes the scan step aside.
    pub(crate) fn scan_note_frame(&mut self, payload: &[u8]) {
        let own_write = self.own_write;
        let Some(saved) = self.scan.saved.as_mut() else { return };
        if is_browse_push(payload) {
            if !own_write {
                saved.external = true;
            }
        } else if let Some((index, value)) = extract_param_change(payload) {
            if index != P_CAB_TYPE {
                if let Ok(b) = patch_param(&saved.probe_base, index, value) {
                    saved.probe_base = b;
                }
            }
        }
    }

    /// One preset of the background pass: its full dump.
    pub(crate) fn pass_step(&mut self) -> Result<(), String> {
        let Some(idx) = self.pass else { return Ok(()) };
        match self.fetch_dump(idx, false)? {
            Ok(dump) => match dump_body(&dump, idx) {
                Ok(body) => self.report_body(idx, body),
                Err(e) => eprintln!("TONEX preset pass: preset {idx}: {e}"),
            },
            Err(e) => eprintln!("TONEX preset pass: preset {idx}: {e}"),
        }
        let next = idx + 1;
        let running = usize::from(next) < PRESET_COUNT;
        self.pass = running.then_some(next);
        if !running {
            (self.on_event)(Event::PresetPass { running: false });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_cab_type_echoes_are_hidden() {
        let now = Instant::now();
        // While the scan has the live buffer: the cab type only.
        assert!(hides_scan_echo(P_CAB_TYPE, true, None, now));
        assert!(!hides_scan_echo(P_CAB_MODEL, true, None, now));
        // After the restore, for the guard time.
        let until = Some(now + SCAN_ECHO_GUARD);
        assert!(hides_scan_echo(P_CAB_TYPE, false, until, now + Duration::from_millis(500)));
        assert!(!hides_scan_echo(P_CAB_TYPE, false, until, now + SCAN_ECHO_GUARD));
        // No scan: every push reaches the app.
        assert!(!hides_scan_echo(P_CAB_TYPE, false, None, now));
    }
}
