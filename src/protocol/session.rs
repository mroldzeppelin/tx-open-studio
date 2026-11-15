//! The session thread: serves requests, reads replies and pushes, reports events.

use super::*;
use std::collections::VecDeque;
use std::sync::mpsc;
use std::time::Duration;
use std::time::Instant;

/// Result of one preset-details read.
pub(crate) struct DetailsRead {
    amp: Result<AmpParams, String>,
    raw: Vec<f32>,
    character: Option<String>,
}

impl DetailsRead {
    pub(crate) fn failed(e: String) -> Self {
        Self { amp: Err(e), raw: Vec::new(), character: None }
    }
}

pub(crate) struct Session<'a, F: Fn(Event)> {
    pub(crate) link: Link,
    pub(crate) rx: mpsc::Receiver<Request>,
    pub(crate) queue: VecDeque<Request>,
    pub(crate) on_event: &'a F,
    pub(crate) debug: bool,
    /// Last global config payload received (needed to write a global).
    pub(crate) globals: Option<Vec<u8>>,
    /// Counts received global config frames (to detect our reply).
    pub(crate) globals_seen: u32,
    /// When to read the live buffer after the pedal's browse pushes.
    pub(crate) browse_due: Option<Instant>,
    pub(crate) scan: CatalogScan,
    /// Next preset of the background preset pass.
    pub(crate) pass: Option<u8>,
    /// Background work waits until then (set by user requests).
    pub(crate) quiet_until: Option<Instant>,
    /// We wrote the live buffer in this loop round: browse pushes now may
    /// answer our own write, not the MODEL encoder.
    pub(crate) own_write: bool,
    /// Cab-type pushes are hidden from the app until then (scan echoes).
    pub(crate) scan_echo_until: Option<Instant>,
}

impl<F: Fn(Event)> Session<'_, F> {
    /// Main loop; returns why the session ended.
    pub(crate) fn run(&mut self) -> String {
        loop {
            self.own_write = false;
            if !self.pull_requests() {
                // Best effort: leave the live buffer as the scan found it.
                let _ = self.scan_restore();
                return "session closed".into();
            }
            // A scan probe never stays in the live buffer while anything
            // else is served, or after the pedal browsed.
            let step_aside = self.scan.saved.as_ref().is_some_and(|s| s.external || !self.queue.is_empty());
            if step_aside {
                if let Err(e) = self.scan_restore() {
                    return e;
                }
            }
            let now = Instant::now();
            let quiet = self.quiet_until.is_none_or(|t| now >= t);
            let result = match self.serve_quick() {
                Ok(true) => {
                    self.quiet_until = Some(Instant::now() + BACKGROUND_QUIET);
                    Ok(())
                }
                Ok(false) if self.take_names_request() => {
                    self.read_names().map(|r| (self.on_event)(Event::Names(r)))
                }
                Ok(false) if self.browse_due.is_some_and(|t| now >= t) => {
                    self.quiet_until = Some(now + BACKGROUND_QUIET);
                    self.refresh_live()
                }
                Ok(false) if quiet && self.pass.is_some() => self.pass_step(),
                Ok(false) if self.scan_ready(quiet) => self.scan_step(),
                Ok(false) => self.idle(),
                Err(e) => Err(e),
            };
            if let Err(e) = result {
                return e;
            }
        }
    }

    /// Serves queued quick requests: all writes (in order), then only the
    /// newest amp-params read. Returns whether anything was served.
    pub(crate) fn serve_quick(&mut self) -> Result<bool, String> {
        let mut writes: Vec<(u8, u8, f32)> = Vec::new();
        let mut global_writes: Vec<(usize, f32)> = Vec::new();
        let mut read_globals = false;
        let mut latest_read = None;
        let mut renames = Vec::new();
        let mut saves = Vec::new();
        let mut latest_tone = None;
        let mut read_volume = false;
        let mut read_library = false;
        let mut applies: Vec<(SlotTarget, SlotChoice)> = Vec::new();
        let mut select = None;
        let mut rest = VecDeque::new();
        let mut held_back = false;
        for r in std::mem::take(&mut self.queue) {
            if held_back {
                rest.push_back(r);
                continue;
            }
            match r {
                // A select goes out first, unless something queued before it
                // (for the old preset) was collected: then it waits, with
                // everything after it, for the next round.
                Request::Select(idx) => {
                    let collected = !writes.is_empty()
                        || !global_writes.is_empty()
                        || read_globals
                        || latest_read.is_some()
                        || !renames.is_empty()
                        || !saves.is_empty()
                        || latest_tone.is_some()
                        || read_volume
                        || read_library
                        || !applies.is_empty();
                    if collected {
                        held_back = true;
                        rest.push_back(Request::Select(idx));
                    } else {
                        select = Some(idx);
                    }
                }
                // A drag queues many writes per param: keep the latest value,
                // in first-seen order across params.
                Request::SetParam(section, index, value) => {
                    match writes.iter_mut().find(|(s, i, _)| (*s, *i) == (section, index)) {
                        Some(w) => w.2 = value,
                        None => writes.push((section, index, value)),
                    }
                }
                Request::SetGlobal(index, value) => match global_writes.iter_mut().find(|(i, _)| *i == index) {
                    Some(w) => w.1 = value,
                    None => global_writes.push((index, value)),
                },
                Request::Globals => read_globals = true,
                Request::AmpParams(idx) => latest_read = Some(idx),
                Request::Rename(idx, name) => renames.push((idx, name)),
                Request::Save(idx) => saves.push(idx),
                Request::ToneInfo(idx) => latest_tone = Some(idx),
                Request::ReadVolume => read_volume = true,
                Request::LibraryList => read_library = true,
                Request::ApplySlot(target, choice) => applies.push((target, choice)),
                Request::ScanCatalog(ids) => self.start_scan(ids),
                Request::CancelScan => self.cancel_scan(),
                Request::PresetPass => {
                    self.pass = Some(0);
                    (self.on_event)(Event::PresetPass { running: true });
                }
                other => rest.push_back(other),
            }
        }
        self.queue = rest;
        if let Some(idx) = select {
            self.link.send(&select_request(idx))?;
            // The reads that follow are for the new preset: wait until the
            // pedal has switched (its summary push), or a read of live
            // values could still answer for the previous preset.
            self.await_select(idx)?;
        }
        for &(section, index, value) in &writes {
            self.link.send(&set_param_request(section, index, value))?;
        }
        let mut served = select.is_some() || !writes.is_empty() || !global_writes.is_empty();
        if !global_writes.is_empty() {
            self.write_globals(&global_writes)?;
        }
        if read_globals {
            self.read_globals()?;
            served = true;
        }
        if let Some(idx) = latest_read {
            let read = self.read_amp_params(idx)?;
            (self.on_event)(Event::AmpParams { preset_idx: idx, result: read.amp, raw: read.raw });
            if let Some(character) = read.character {
                (self.on_event)(Event::Character { preset_idx: idx, character });
            }
            served = true;
        }
        // Front-panel VOLUME (pedal-wide, read at connect): the answer is
        // a 0x0207 frame taken here.
        if read_volume {
            if let Some(value) = self.read_volume()? {
                (self.on_event)(Event::Volume(value));
            } else if self.debug {
                eprintln!("TONEX serial: no answer to the VOLUME read");
            }
            served = true;
        }
        // Names after the params: only the newest request, footswitch
        // pushes still handled meanwhile.
        if let Some(idx) = latest_tone {
            let info = match self.fetch_dump(idx, false)? {
                Ok(dump) => dump_body(&dump, idx).ok().and_then(|body| {
                    self.report_body(idx, body);
                    tone_info(body)
                }),
                Err(e) => {
                    if self.debug {
                        eprintln!("TONEX serial: tone info for preset {idx}: {e}");
                    }
                    None
                }
            };
            (self.on_event)(Event::ToneInfo { preset_idx: idx, info });
            served = true;
        }
        for (target, choice) in applies {
            match self.apply_slot(target, &choice) {
                Ok(result) => (self.on_event)(Event::SlotApplied { target, result }),
                Err(e) => {
                    (self.on_event)(Event::SlotApplied { target, result: Err(e.clone()) });
                    return Err(e);
                }
            }
            served = true;
        }
        if read_library {
            let list = self.read_library_list()?;
            (self.on_event)(Event::LibraryList(list));
            served = true;
        }
        for (idx, name) in renames {
            match self.rename_preset(idx, &name) {
                Ok(result) => (self.on_event)(Event::Renamed { preset_idx: idx, result }),
                Err(e) => {
                    // Link failure: report it, then end the session.
                    (self.on_event)(Event::Renamed { preset_idx: idx, result: Err(e.clone()) });
                    return Err(e);
                }
            }
            served = true;
        }
        for idx in saves {
            match self.save_preset(idx) {
                Ok(result) => (self.on_event)(Event::Saved { preset_idx: idx, result }),
                Err(e) => {
                    (self.on_event)(Event::Saved { preset_idx: idx, result: Err(e.clone()) });
                    return Err(e);
                }
            }
            served = true;
        }
        Ok(served)
    }

    /// Requests a full dump of preset `idx` and returns its reply payload,
    /// handling other frames meanwhile. With `drop_pushes` (rename/save),
    /// preset-details pushes are dropped: one may echo our own write and
    /// must not look like a footswitch change; read-only callers keep them.
    /// Inner Err: no valid dump (with the last layout problem seen); outer
    /// Err: link failure.
    pub(crate) fn fetch_dump(&mut self, idx: u8, drop_pushes: bool) -> Result<Result<Vec<u8>, String>, String> {
        self.fetch_0203(&full_dump_request(idx), |p| dump_body(p, idx).map(|_| ()), drop_pushes)
    }

    /// Sends `request` and returns the first type-0x0203 reply `accept`
    /// takes (other 0x0203 frames: their problem is kept for the error),
    /// handling other frames as `fetch_dump` describes.
    pub(crate) fn fetch_0203(
        &mut self,
        request: &[u8],
        accept: impl Fn(&[u8]) -> Result<(), String>,
        drop_pushes: bool,
    ) -> Result<Result<Vec<u8>, String>, String> {
        self.link.send(request)?;
        let until = Instant::now() + DUMP_TIMEOUT;
        let mut problem = "no preset dump received".to_string();
        while let Some(payload) = self.link.read_frame(until.saturating_duration_since(Instant::now()), until)? {
            match frame_type(&payload) {
                Some(TYPE_PRESET_DUMP) => match accept(&payload) {
                    Ok(()) => return Ok(Ok(payload)),
                    Err(e) => problem = e,
                },
                Some(TYPE_PRESET_DETAILS) if drop_pushes => {
                    if self.debug {
                        eprintln!("TONEX serial: preset push during a dump read dropped");
                    }
                }
                _ => {
                    self.dispatch(&payload, false);
                }
            }
        }
        Ok(Err(problem))
    }

    /// The rename operation (see `Pedal::rename_preset`). Inner Err: not
    /// written or not verified; outer Err: link failure.
    pub(crate) fn rename_preset(&mut self, idx: u8, name: &str) -> Result<Result<String, String>, String> {
        // 1. A fresh dump, validated.
        let dump = match self.fetch_dump(idx, true)? {
            Ok(d) => d,
            Err(e) => return Ok(Err(format!("reading the preset: {e}"))),
        };
        let body = match dump_body(&dump, idx) {
            Ok(b) => b.to_vec(),
            Err(e) => return Ok(Err(e)),
        };
        // 2. Patch the name (validates the name field again).
        let patched = match patch_dump_name(&body, name) {
            Ok(p) => p,
            Err(e) => return Ok(Err(e)),
        };
        let request = match preset_write_request(&patched) {
            Ok(r) => r,
            Err(e) => return Ok(Err(e)),
        };
        // 3. Write, re-read and compare the name buffer only (the active
        // preset's model data is stored in another, benign form, so the
        // rest may differ).
        let after = match self.write_and_reread(idx, &request)? {
            Ok(d) => d,
            Err(e) => return Ok(Err(e)),
        };
        if let Ok(body) = dump_body(&after, idx) {
            self.report_body(idx, body);
        }
        let stored = dump_body(&after, idx).and_then(name_buffer);
        match (name_buffer(&patched), stored) {
            (Ok(want), Ok(got)) if want == got => Ok(Ok(clean_name(name))),
            (Ok(_), Ok(_)) => Ok(Err("the pedal did not store the new name".into())),
            (Err(e), _) | (_, Err(e)) => Ok(Err(e)),
        }
    }

    /// Sends a preset write, lets the pedal store it (handling other frames
    /// meanwhile, dropping preset pushes), then reads the preset back.
    pub(crate) fn write_and_reread(&mut self, idx: u8, request: &[u8]) -> Result<Result<Vec<u8>, String>, String> {
        self.link.send(request)?;
        let settle = Instant::now() + WRITE_SETTLE;
        while let Some(payload) = self.link.read_frame(settle.saturating_duration_since(Instant::now()), settle)? {
            if frame_type(&payload) != Some(TYPE_PRESET_DETAILS) {
                self.dispatch(&payload, false);
            }
        }
        Ok(self.fetch_dump(idx, true)?.map_err(|e| format!("verifying the write: {e}")))
    }

    /// Waits up to `SELECT_SETTLE` for the pedal's confirmation of our
    /// select of `idx` (consumed: the new preset's reads follow anyway),
    /// handling other frames meanwhile. On timeout the reads go out regardless.
    pub(crate) fn await_select(&mut self, idx: u8) -> Result<(), String> {
        let until = Instant::now() + SELECT_SETTLE;
        while let Some(payload) = self.link.read_frame(until.saturating_duration_since(Instant::now()), until)? {
            if is_select_ack(&payload, idx) {
                return Ok(());
            }
            self.dispatch(&payload, false);
        }
        if self.debug {
            eprintln!("TONEX serial: no confirmation of the select of preset {idx}");
        }
        Ok(())
    }

    /// Sends the VOLUME get-parameter request and returns the value
    /// from its `0x0207` answer (None on timeout), handling other frames.
    pub(crate) fn read_volume(&mut self) -> Result<Option<f32>, String> {
        self.link.send(&get_param_request(VOLUME_SECTION, 0))?;
        let until = Instant::now() + GET_PARAM_TIMEOUT;
        while let Some(payload) = self.link.read_frame(until.saturating_duration_since(Instant::now()), until)? {
            if frame_type(&payload) == Some(TYPE_PARAM_CHANGED) {
                if let Some(value) = extract_volume_change(&payload) {
                    return Ok(Some(value));
                }
            }
            self.dispatch(&payload, false);
        }
        Ok(None)
    }

    /// Reads the live edit buffer: (payload, active preset index, dirty).
    pub(crate) fn fetch_live(&mut self) -> Result<Result<(Vec<u8>, u8, bool), String>, String> {
        let payload = match self.fetch_0203(&live_buffer_request(), |p| live_body(p).map(|_| ()), true)? {
            Ok(p) => p,
            Err(e) => return Ok(Err(e)),
        };
        Ok(live_body(&payload).map(|(_, idx, dirty)| (payload.clone(), idx, dirty)))
    }

    /// The save operation (see `Pedal::save_preset`). Inner Err: not
    /// written or not verified; outer Err: link failure.
    pub(crate) fn save_preset(&mut self, idx: u8) -> Result<Result<(), String>, String> {
        // 1. The live edit buffer, validated, for the preset being saved.
        let (live, active, _) = match self.fetch_live()? {
            Ok(l) => l,
            Err(e) => return Ok(Err(format!("reading the live buffer: {e}"))),
        };
        if active != idx {
            return Ok(Err(format!("the pedal's active preset ({active}) is not the one being saved ({idx})")));
        }
        // 2. Written back as the stored preset (01 stored, 00 clean).
        let written = match live_body(&live).and_then(|(body, ..)| stored_body_from_live(body)) {
            Ok(b) => b,
            Err(e) => return Ok(Err(e)),
        };
        let request = match preset_write_request(&written) {
            Ok(r) => r,
            Err(e) => return Ok(Err(e)),
        };
        // 3. Write, re-read the stored dump, verify.
        let after = match self.write_and_reread(idx, &request)? {
            Ok(d) => d,
            Err(e) => return Ok(Err(e)),
        };
        let stored = match dump_body(&after, idx) {
            Ok(b) => b,
            Err(e) => return Ok(Err(e)),
        };
        if let Err(e) = verify_saved(&written, stored) {
            return Ok(Err(e));
        }
        self.report_body(idx, stored);
        // 4. The pedal's own EDITED flag should be clear now (log only).
        match self.fetch_live()? {
            Ok((_, _, true)) => eprintln!("TONEX: saved, but the pedal still reports unsaved edits"),
            Ok(_) => {}
            Err(e) => eprintln!("TONEX: saved; live buffer re-read failed: {e}"),
        }
        Ok(Ok(()))
    }

    /// Removes all queued names requests; true if there was one.
    pub(crate) fn take_names_request(&mut self) -> bool {
        let before = self.queue.len();
        self.queue.retain(|r| !matches!(r, Request::Names));
        self.queue.len() != before
    }

    /// Moves pending requests into the local queue; false once the handle
    /// is gone (the session should end).
    pub(crate) fn pull_requests(&mut self) -> bool {
        loop {
            match self.rx.try_recv() {
                Ok(r) => self.queue.push_back(r),
                Err(mpsc::TryRecvError::Empty) => return true,
                Err(mpsc::TryRecvError::Disconnected) => return false,
            }
        }
    }

    /// Requests the global config; the reply is reported from `dispatch`.
    pub(crate) fn read_globals(&mut self) -> Result<(), String> {
        let seen = self.globals_seen;
        self.link.send(&REQUEST_GLOBALS_CMD)?;
        let until = Instant::now() + GLOBALS_TIMEOUT;
        while self.globals_seen == seen {
            match self.link.read_frame(until.saturating_duration_since(Instant::now()), until)? {
                Some(payload) => {
                    self.dispatch(&payload, false);
                }
                None => {
                    (self.on_event)(Event::Globals(Err("no global config received".into())));
                    break;
                }
            }
        }
        Ok(())
    }

    /// Sends the stored global config back with `changes` applied.
    pub(crate) fn write_globals(&mut self, changes: &[(usize, f32)]) -> Result<(), String> {
        let Some(stored) = self.globals.as_mut() else {
            eprintln!("TONEX: global write skipped: global config not read yet");
            return Ok(());
        };
        for &(index, value) in changes {
            if let Err(e) = patch_global(stored, index, value) {
                eprintln!("TONEX: global write skipped: {e}");
                return Ok(());
            }
        }
        match global_write_request(stored) {
            Ok(req) => self.link.send(&req),
            Err(e) => {
                eprintln!("TONEX: global write skipped: {e}");
                Ok(())
            }
        }
    }

    /// Waits a short while for unsolicited frames.
    pub(crate) fn idle(&mut self) -> Result<(), String> {
        let far = Instant::now() + Duration::from_secs(3600);
        if let Some(payload) = self.link.read_frame(IDLE_POLL, far)? {
            self.dispatch(&payload, false);
        }
        Ok(())
    }

    /// Handles one incoming frame. Returns true when `awaiting` a details
    /// response and this frame is it.
    ///
    /// While a request is pending, the first frame carrying a preset name is
    /// taken as its response (the behaviour verified on hardware for the
    /// name scan). Only while idle is a details frame with the preset-index
    /// marker treated as a footswitch push, so a push that lands during a
    /// request is missed (rare; the next preset change re-syncs).
    pub(crate) fn dispatch(&mut self, payload: &[u8], awaiting: bool) -> bool {
        let ty = frame_type(payload);
        self.scan_note_frame(payload);
        if ty == Some(TYPE_LIVE_WRITE_ACK) {
            return false;
        }
        if ty == Some(TYPE_BROWSE) {
            if self.own_write {
                if self.debug {
                    eprintln!("TONEX serial: browse push after our live write ignored");
                }
            } else {
                self.browse_due = Some(Instant::now() + BROWSE_DEBOUNCE);
            }
            return false;
        }
        if ty == Some(TYPE_GLOBAL_CONFIG) {
            if self.debug {
                eprintln!("TONEX serial: globals: {}", globals_dump(payload));
            }
            let result = extract_globals(payload);
            if result.is_ok() {
                self.globals = Some(payload.to_vec());
            }
            self.globals_seen = self.globals_seen.wrapping_add(1);
            (self.on_event)(Event::Globals(result));
            return false;
        }
        if ty == Some(TYPE_STATE) {
            // Pushed state update (e.g. preset changed on the pedal).
            report_state(Some(payload), self.on_event, self.debug);
            return false;
        }
        if ty == Some(TYPE_PARAM_CHANGED) {
            if let Some((index, value)) = extract_param_change(payload) {
                if hides_scan_echo(index, self.scan.saved.is_some(), self.scan_echo_until, Instant::now()) {
                    if self.debug {
                        eprintln!("TONEX serial: catalog scan echo hidden: param {index} = {value}");
                    }
                } else {
                    (self.on_event)(Event::ParamChanged { index, value });
                }
            } else if let Some(value) = extract_volume_change(payload) {
                (self.on_event)(Event::Volume(value));
            } else if self.debug {
                eprintln!("TONEX serial: unparsed 0x0207: {}", hex(payload));
            }
            return false;
        }
        if awaiting && find(payload, &NAME_MARKER).is_some() {
            return true;
        }
        if !awaiting && ty == Some(TYPE_PRESET_DETAILS) {
            if let Some(idx) = extract_current_preset(payload) {
                for event in pushed_preset_events(idx, payload) {
                    (self.on_event)(event);
                }
                return false;
            }
        }
        if self.debug {
            let shown = &payload[..payload.len().min(48)];
            eprintln!("TONEX serial: unhandled frame type {ty:04X?} ({} B): {}", payload.len(), hex(shown));
        }
        false
    }

    /// Requests details of preset `idx` and returns the response payload
    /// (None on timeout), dispatching unrelated frames meanwhile.
    pub(crate) fn fetch_details(&mut self, idx: u8, timeout: Duration) -> Result<Option<Vec<u8>>, String> {
        self.link.send(&preset_request(idx))?;
        let until = Instant::now() + timeout;
        while let Some(payload) = self.link.read_frame(until.saturating_duration_since(Instant::now()), until)? {
            if self.dispatch(&payload, true) {
                return Ok(Some(payload));
            }
        }
        Ok(None)
    }

    /// Reads preset `idx`'s details. Ok(read) for the events; Err only for
    /// link failures (session ends).
    pub(crate) fn read_amp_params(&mut self, idx: u8) -> Result<DetailsRead, String> {
        if usize::from(idx) >= PRESET_COUNT {
            return Ok(DetailsRead::failed(format!("preset index {idx} out of range")));
        }
        let details = self.fetch_details(idx, PRESET_TIMEOUT)?;
        Ok(match details {
            Some(payload) => DetailsRead {
                amp: extract_amp_params(&payload),
                raw: extract_param_array(&payload),
                character: extract_character(&payload),
            },
            None => DetailsRead::failed("no preset details received".into()),
        })
    }

    /// Reads all 150 names. Unreadable slots are empty strings; the inner
    /// result fails only if no name at all could be read.
    pub(crate) fn read_names(&mut self) -> Result<Result<Vec<PresetMeta>, String>, String> {
        let deadline = Instant::now() + OVERALL_DEADLINE;
        let mut names = vec![PresetMeta::default(); PRESET_COUNT];
        let mut read = 0usize;
        for i in 0..PRESET_COUNT {
            if Instant::now() >= deadline {
                break;
            }
            // Stay responsive during the ~10 s scan: knob writes and the
            // current preset's params go in between names.
            if !self.pull_requests() {
                return Err("session closed".into());
            }
            self.serve_quick()?;
            let timeout = PRESET_TIMEOUT.min(deadline.saturating_duration_since(Instant::now()));
            if let Some(payload) = self.fetch_details(i as u8, timeout)? {
                if let Some(name) = extract_name(&payload) {
                    names[i] = PresetMeta { name, character: extract_character(&payload).unwrap_or_default() };
                    read += 1;
                }
            }
        }

        if read == 0 {
            return Ok(Err("pedal answered but no preset names were received".into()));
        }
        if read < PRESET_COUNT {
            eprintln!("TONEX: read {read}/{PRESET_COUNT} preset names");
        }
        Ok(Ok(names))
    }
}
