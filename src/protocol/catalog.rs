//! Session side of model/cab choice: the library list, ID-only slot loads
//! (apply), the catalog scan (read each unnamed library item's names by
//! its list position, `0x0209`), the background preset pass and the
//! browse refresh.

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

/// Background work (catalog scan, preset pass) waits this long after the
/// last user request.
pub(crate) const BACKGROUND_QUIET: Duration = Duration::from_millis(1500);

/// Outcome of polling for an id-only slot to be filled in.
pub(crate) enum Fill {
    Filled { body: Vec<u8>, idx: u8, dirty: bool, after: Duration },
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
            self.dispatch(&payload);
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
                self.dispatch(&payload);
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
                    if filled_slot(&body, target, id).is_some() {
                        return Ok(Fill::Filled { body, idx: active, dirty, after: start.elapsed() });
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
                Fill::Filled { body, idx, dirty, after } => {
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

    /// Starts (replaces) a catalog scan of `items`.
    pub(crate) fn start_scan(&mut self, items: Vec<LibraryItem>) {
        let mut pending: VecDeque<LibraryItem> = VecDeque::new();
        for item in items {
            if !pending.contains(&item) {
                pending.push_back(item);
            }
        }
        let empty = pending.is_empty();
        self.scan = pending;
        (self.on_event)(Event::CatalogScan { running: !empty, completed: empty });
    }

    pub(crate) fn cancel_scan(&mut self) {
        if !self.scan.is_empty() {
            self.scan.clear();
            (self.on_event)(Event::CatalogScan { running: false, completed: false });
        }
    }

    /// Reads one library item's names (`0x0209` → `0x020A`), handling other
    /// frames meanwhile. Inner Err: no valid answer; outer Err: link failure.
    pub(crate) fn fetch_library_item(&mut self, item: &LibraryItem) -> Result<Result<SlotInfo, String>, String> {
        self.link.send(&library_item_request(item.position))?;
        let until = Instant::now() + LIBRARY_ITEM_TIMEOUT;
        let mut problem = "no answer".to_string();
        while let Some(payload) = self.link.read_frame(until.saturating_duration_since(Instant::now()), until)? {
            if frame_type(&payload) == Some(TYPE_LIBRARY_ITEM) {
                // A late answer to an earlier read fails the position/id check.
                match parse_library_item(&payload, item) {
                    Ok(info) => return Ok(Ok(info)),
                    Err(e) => problem = e,
                }
            } else {
                self.dispatch(&payload);
            }
        }
        Ok(Err(problem))
    }

    /// One scan step: the next item's names. A failed item keeps its
    /// placeholder; the scan goes on.
    pub(crate) fn scan_step(&mut self) -> Result<(), String> {
        let Some(item) = self.scan.pop_front() else { return Ok(()) };
        let start = Instant::now();
        match self.fetch_library_item(&item)? {
            Ok(info) => {
                if self.debug {
                    eprintln!(
                        "TONEX catalog scan: position {} {}: kind {} {:?} after {} ms",
                        item.position,
                        hex_id(&item.id),
                        info.kind,
                        info.name,
                        start.elapsed().as_millis()
                    );
                }
                (self.on_event)(Event::Slots(vec![info]));
            }
            Err(e) => {
                if self.debug {
                    eprintln!("TONEX catalog scan: position {} {}: {e}", item.position, hex_id(&item.id));
                }
            }
        }
        if self.scan.is_empty() {
            (self.on_event)(Event::CatalogScan { running: false, completed: true });
        }
        Ok(())
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
        self.pass = (usize::from(next) < PRESET_COUNT).then_some(next);
        Ok(())
    }
}
