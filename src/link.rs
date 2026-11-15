//! Link state of both connections (serial, MIDI for the tuner), the status bar text and the reconnect logic's pure parts.

use std::time::Duration;

pub(crate) const STATUS_UP: &str = "TONEX PEDAL CONNECTED";

pub(crate) const STATUS_RECONNECTING: &str = "RECONNECTING…";

/// Status bar text for `status`.
pub(crate) fn status_text(status: &LinkStatus) -> String {
    match status {
        LinkStatus::Connected => STATUS_UP.into(),
        LinkStatus::NoAccess(path) => format!("NO ACCESS TO {path} — INSTALL THE UDEV RULE (SEE README)"),
        LinkStatus::Reconnecting => STATUS_RECONNECTING.into(),
    }
}

/// How often the reconnect poll looks for the pedal while a link is down.
pub(crate) const RECONNECT_POLL: Duration = Duration::from_secs(2);

/// Serial session as seen by the UI thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SerialLink {
    #[default]
    Down,
    /// Session thread started, handshake not done yet.
    Starting,
    Up,
}

/// What a serial session's `Disconnected` event means.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SerialEnd {
    /// From a session that was already replaced or torn down: ignore.
    Stale,
    /// Open/handshake failed: serial stays down, MIDI is left alone.
    OpenFailed,
    /// A live session died (cable pulled, I/O error): the pedal is gone.
    Lost,
}

/// Both links to the pedal (one USB cable: MIDI + CDC serial). Serial
/// sessions are numbered so events of a replaced session are ignored.
#[derive(Debug, Default)]
pub(crate) struct LinkState {
    pub(crate) midi: bool,
    pub(crate) serial: SerialLink,
    pub(crate) session: u64,
    /// Last reported poll failure, to avoid logging it every 2 s.
    pub(crate) last_failure: String,
    /// Serial device node we may not open (udev rule missing), once the
    /// refusal persisted; cleared on connect or when the node is gone.
    pub(crate) no_access: Option<String>,
    /// Consecutive sessions refused access (see `serial_denied`), and the
    /// last refused session.
    pub(crate) denied_streak: u32,
    pub(crate) denied_session: u64,
}

/// Refused opens in a row before the "no access" hint shows (~4-6 s of
/// polling): a single refusal can be transient, e.g. right after another
/// instance released the port.
pub(crate) const NO_ACCESS_STREAK: u32 = 3;

/// What the status bar shows.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LinkStatus {
    Connected,
    /// The pedal's serial port exists but we may not open it: the most
    /// actionable problem, shown even when MIDI works.
    NoAccess(String),
    Reconnecting,
}

impl LinkState {
    /// Connected = the serial link is up (or its handshake is running).
    /// MIDI is only used for the tuner and doesn't count.
    pub(crate) fn healthy(&self) -> bool {
        self.serial != SerialLink::Down
    }

    /// Anything to reconnect: serial, or MIDI for the tuner.
    pub(crate) fn wants_poll(&self) -> bool {
        !self.healthy() || !self.midi
    }

    pub(crate) fn status(&self) -> LinkStatus {
        match &self.no_access {
            // Kept while a retry runs, so the 2 s polls don't flicker.
            Some(path) if self.serial != SerialLink::Up => LinkStatus::NoAccess(path.clone()),
            _ if self.healthy() => LinkStatus::Connected,
            _ => LinkStatus::Reconnecting,
        }
    }

    /// A new serial session starts; returns its id.
    pub(crate) fn serial_starting(&mut self) -> u64 {
        self.session += 1;
        self.serial = SerialLink::Starting;
        self.session
    }

    /// Handshake of session `id` done; false if that session is stale.
    pub(crate) fn serial_connected(&mut self, id: u64) -> bool {
        if id != self.session || self.serial == SerialLink::Down {
            return false;
        }
        self.serial = SerialLink::Up;
        self.clear_denied();
        true
    }

    /// Session `id` was refused access to `path`.
    pub(crate) fn serial_denied(&mut self, id: u64, path: String) {
        if id != self.session {
            return;
        }
        self.denied_session = id;
        self.denied_streak += 1;
        if self.denied_streak >= NO_ACCESS_STREAK {
            self.no_access = Some(path);
        }
    }

    pub(crate) fn clear_denied(&mut self) {
        self.no_access = None;
        self.denied_streak = 0;
    }

    pub(crate) fn serial_ended(&mut self, id: u64) -> SerialEnd {
        if id != self.session || self.serial == SerialLink::Down {
            return SerialEnd::Stale;
        }
        let was_up = self.serial == SerialLink::Up;
        self.serial = SerialLink::Down;
        if self.denied_session != id {
            // Failed for another reason: the refusals were not persistent.
            self.clear_denied();
        }
        if was_up {
            SerialEnd::Lost
        } else {
            SerialEnd::OpenFailed
        }
    }

    /// Both links dropped (pedal gone, or manual reconnect). The running
    /// session, if any, becomes stale.
    pub(crate) fn drop_all(&mut self) {
        self.midi = false;
        self.serial = SerialLink::Down;
        self.session += 1;
    }

    /// True when `failure` differs from the last one (worth logging).
    pub(crate) fn note_failure(&mut self, failure: &str) -> bool {
        if self.last_failure == failure {
            return false;
        }
        self.last_failure = failure.to_string();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_state_with_and_without_midi() {
        // Connected = serial; MIDI (tuner only) doesn't count.
        let mut l = LinkState::default();
        let id = l.serial_starting();
        assert!(l.serial_connected(id));
        assert!(l.healthy());
        assert_eq!(l.status(), LinkStatus::Connected);
        assert!(l.wants_poll(), "still polling for MIDI (tuner)");
        l.midi = true;
        assert!(!l.wants_poll());
        // MIDI gone again: still connected.
        l.midi = false;
        assert_eq!(l.status(), LinkStatus::Connected);
        // Serial gone: reconnecting, whatever MIDI does.
        l.midi = true;
        l.serial_ended(id);
        assert_eq!(l.status(), LinkStatus::Reconnecting);
        assert!(l.wants_poll());
    }

    #[test]
    fn link_state_normal_start() {
        let mut l = LinkState::default();
        assert!(!l.healthy());
        l.midi = true;
        let id = l.serial_starting();
        assert!(l.healthy(), "handshake running counts as connected");
        assert!(l.serial_connected(id));
        assert_eq!(l.serial, SerialLink::Up);
    }

    #[test]
    fn link_state_open_failure_keeps_midi() {
        let mut l = LinkState { midi: true, ..Default::default() };
        let id = l.serial_starting();
        assert_eq!(l.serial_ended(id), SerialEnd::OpenFailed);
        assert!(l.midi);
        assert!(!l.healthy());
    }

    #[test]
    fn link_state_lost_session_and_stale_events() {
        let mut l = LinkState { midi: true, ..Default::default() };
        let id = l.serial_starting();
        assert!(l.serial_connected(id));
        assert_eq!(l.serial_ended(id), SerialEnd::Lost);
        // A second end of the same session is not reported twice.
        assert_eq!(l.serial_ended(id), SerialEnd::Stale);

        // Torn down (or manual reconnect): the old session's events are stale.
        let old = l.serial_starting();
        l.drop_all();
        assert!(!l.midi);
        assert!(!l.serial_connected(old));
        assert_eq!(l.serial_ended(old), SerialEnd::Stale);

        // A replaced session can't take over the new one.
        let a = l.serial_starting();
        let b = l.serial_starting();
        assert_eq!(l.serial_ended(a), SerialEnd::Stale);
        assert!(l.serial_connected(b));
    }

    #[test]
    fn link_state_logs_each_failure_once() {
        let mut l = LinkState::default();
        assert!(l.note_failure("TONEX MIDI port not found"));
        assert!(!l.note_failure("TONEX MIDI port not found"));
        assert!(l.note_failure("other"));
    }

    #[test]
    fn link_status_prioritizes_missing_access() {
        let mut l = LinkState { midi: true, ..Default::default() };
        let path = || "/dev/ttyACM0".to_string();
        // Refused NO_ACCESS_STREAK times in a row: the hint shows.
        for _ in 0..NO_ACCESS_STREAK {
            let id = l.serial_starting();
            l.serial_denied(id, path());
            assert_eq!(l.serial_ended(id), SerialEnd::OpenFailed);
        }
        assert_eq!(l.status(), LinkStatus::NoAccess(path()));
        assert_eq!(
            status_text(&l.status()),
            "NO ACCESS TO /dev/ttyACM0 — INSTALL THE UDEV RULE (SEE README)"
        );
        // Retrying keeps the hint (no flicker); once the rule is installed
        // the next session connects and the hint goes away for good.
        let id = l.serial_starting();
        assert_eq!(l.status(), LinkStatus::NoAccess(path()));
        assert!(l.serial_connected(id));
        assert_eq!(l.status(), LinkStatus::Connected);
        assert_eq!(l.no_access, None);
        l.drop_all();
        assert_eq!(l.status(), LinkStatus::Reconnecting);
    }

    #[test]
    fn transient_access_refusal_never_shows_hint() {
        let mut l = LinkState { midi: true, ..Default::default() };
        // E.g. relaunch while the old instance still releases the port.
        for _ in 0..NO_ACCESS_STREAK - 1 {
            let id = l.serial_starting();
            l.serial_denied(id, "/dev/ttyACM0".into());
            assert_eq!(l.serial_ended(id), SerialEnd::OpenFailed);
            assert_eq!(l.status(), LinkStatus::Reconnecting);
        }
        let id = l.serial_starting();
        assert!(l.serial_connected(id));
        assert_eq!(l.status(), LinkStatus::Connected);

        // A different failure in between also restarts the count.
        l.drop_all();
        l.midi = true;
        for _ in 0..NO_ACCESS_STREAK - 1 {
            let id = l.serial_starting();
            l.serial_denied(id, "/dev/ttyACM0".into());
            l.serial_ended(id);
        }
        let id = l.serial_starting();
        assert_eq!(l.serial_ended(id), SerialEnd::OpenFailed); // e.g. no hello reply
        let id = l.serial_starting();
        l.serial_denied(id, "/dev/ttyACM0".into());
        l.serial_ended(id);
        assert_eq!(l.status(), LinkStatus::Reconnecting);
    }
}
