//! Tap tempo: interval → BPM estimate, one BPM write per tap burst.

use std::time::Duration;
use std::time::Instant;

/// A longer gap starts a new tap sequence.
pub(crate) const TAP_RESET: Duration = Duration::from_millis(2500);

/// Taps closer than this are ignored (contact bounce, key repeat); 150 ms
/// would be 400 BPM, above anything the readout shows.
pub(crate) const TAP_MIN_INTERVAL: Duration = Duration::from_millis(150);

/// Intervals averaged for the estimate.
pub(crate) const TAP_AVERAGE: usize = 4;

pub(crate) const TAP_BPM_RANGE: (f32, f32) = (30.0, 300.0);

/// Outcome of one tap.
#[derive(Debug, PartialEq)]
pub(crate) enum TapResult {
    /// Too soon after the previous tap: not sent, not counted.
    Ignored,
    /// Counted; the new tempo estimate once there is an interval (None on
    /// the first tap of a sequence: keep showing the previous value).
    Counted(Option<f32>),
}

/// Local tempo estimate from tap times. The estimate is written as the
/// pedal's tempo (global 18, `G_TEMPO`), which reads back with the globals
/// (the re-read after the burst shows the value the pedal took).
#[derive(Debug, Default)]
pub(crate) struct TapTempo {
    pub(crate) last: Option<Instant>,
    pub(crate) intervals: Vec<Duration>,
}

impl TapTempo {
    pub(crate) fn tap(&mut self, now: Instant) -> TapResult {
        let Some(last) = self.last else {
            self.last = Some(now);
            return TapResult::Counted(None);
        };
        let interval = now.saturating_duration_since(last);
        if interval < TAP_MIN_INTERVAL {
            return TapResult::Ignored;
        }
        self.last = Some(now);
        if interval > TAP_RESET {
            self.intervals.clear();
            return TapResult::Counted(None);
        }
        self.intervals.push(interval);
        if self.intervals.len() > TAP_AVERAGE {
            self.intervals.remove(0);
        }
        let avg = self.intervals.iter().sum::<Duration>().as_secs_f32() / self.intervals.len() as f32;
        let (min, max) = TAP_BPM_RANGE;
        TapResult::Counted(Some((60.0 / avg).clamp(min, max)))
    }
}

/// After the last tap of a burst, the BPM is written once and the globals
/// read once more to show the tempo the pedal took.
pub(crate) const TAP_REREAD_DELAY: Duration = Duration::from_millis(500);

/// Global-control id of the tempo (see GLOBAL_CONTROLS).
pub(crate) const GLOBAL_TEMPO_ID: i32 = 5;

/// The tempo estimate of a tap burst, written once when the burst ends.
#[derive(Debug, Default)]
pub(crate) struct TapBurst {
    pub(crate) pending: Option<f32>,
}

impl TapBurst {
    /// A tap gave a new estimate.
    pub(crate) fn tap(&mut self, bpm: f32) {
        self.pending = Some(bpm);
    }

    /// The burst is over: its latest estimate, once.
    pub(crate) fn flush(&mut self) -> Option<f32> {
        self.pending.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    #[test]
    fn tap_burst_writes_once() {
        let mut b = TapBurst::default();
        assert_eq!(b.flush(), None, "no estimate, no write");
        for bpm in [118.0, 121.5, 120.0] {
            b.tap(bpm);
        }
        assert_eq!(b.flush(), Some(120.0), "the burst's latest estimate");
        assert_eq!(b.flush(), None, "written once");
        assert_eq!(global_control(GLOBAL_TEMPO_ID).unwrap().index, protocol::G_TEMPO);
    }

    #[test]
    fn tap_tempo_averages_last_intervals() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut t = TapTempo::default();
        assert_eq!(t.tap(at(0)), TapResult::Counted(None));
        assert_eq!(t.tap(at(500)), TapResult::Counted(Some(120.0)));
        // 500 + 1000 ms → 750 ms average → 80 BPM.
        assert_eq!(t.tap(at(1500)), TapResult::Counted(Some(80.0)));
        // Only the last 4 intervals count: 1000, 500, 500, 500 → 625 ms
        // → 96 BPM; one more 500 ms tap pushes the 1000 out → 120 BPM.
        t.tap(at(2000));
        t.tap(at(2500));
        assert_eq!(t.tap(at(3000)), TapResult::Counted(Some(96.0)));
        assert_eq!(t.intervals.len(), 4);
        assert_eq!(t.tap(at(3500)), TapResult::Counted(Some(120.0)));
    }

    #[test]
    fn tap_tempo_resets_ignores_bounces_and_clamps() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut t = TapTempo::default();
        t.tap(at(0));
        t.tap(at(400));
        // Bounce / key repeat: ignored, the next interval counts from 400.
        assert_eq!(t.tap(at(450)), TapResult::Ignored);
        assert_eq!(t.tap(at(800)), TapResult::Counted(Some(150.0)));
        // A pause over 2.5 s starts a new sequence.
        assert_eq!(t.tap(at(4000)), TapResult::Counted(None));
        assert_eq!(t.tap(at(5000)), TapResult::Counted(Some(60.0)));
        // Clamped to 30..300 BPM.
        let mut slow = TapTempo::default();
        slow.tap(at(0));
        assert_eq!(slow.tap(at(2400)), TapResult::Counted(Some(30.0)));
        let mut fast = TapTempo::default();
        fast.tap(at(0));
        assert_eq!(fast.tap(at(160)), TapResult::Counted(Some(300.0)));
    }
}
