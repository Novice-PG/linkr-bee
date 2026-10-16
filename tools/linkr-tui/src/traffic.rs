//! Traffic sampling: the session's byte counters read on a clock, turned into
//! a rate per second and a short history for the top bar's graph.
//!
//! The counters are totals, and a total says nothing about what the link is
//! doing *now*: ten megabytes read once looks the same as ten megabytes
//! trickling all afternoon. The rate is the difference between two readings
//! divided by the time between them, and the history is the last few of those
//! — shape and scale, which is what a sparkline and the number beside it need.
//!
//! Sampling is gated on wall-clock time, not on being called: the pump runs
//! for every key and every frame, and a history that was a count of loop turns
//! would scroll at whatever speed the user types.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The least wall-clock time between two samples. The pump is called far more
/// often than this — every key it handles is one — so the gate is what keeps
/// the history a clock rather than a count of calls.
pub const SAMPLE_PERIOD: Duration = Duration::from_millis(250);

/// How many samples the history holds: twelve seconds at [`SAMPLE_PERIOD`].
pub const WINDOW: usize = 48;

/// What the link has done lately, in both directions together.
#[derive(Debug, Clone, Default)]
pub struct Traffic {
    /// The recent rates, bytes a second, oldest first. The graph shows the
    /// newest at its right edge and drops the oldest that do not fit.
    pub window: VecDeque<u64>,
    /// The newest reading, bytes a second. The graph alone is shape; this
    /// number is what makes it scale.
    pub rate: u64,
    /// The counters and the clock time of the last sample taken: the state the
    /// next rate is measured against. `None` until the first sample, which has
    /// nothing behind it to measure.
    previous: Option<(u64, u64, Instant)>,
}

impl Traffic {
    /// Fold the session's counters in, at wall-clock `now`.
    ///
    /// A call inside [`SAMPLE_PERIOD`] of the last one is ignored; one after
    /// it records the average rate over the whole stretch. The counters are
    /// totals for the life of a link, so a reconnect starting them at zero
    /// reads as a delta of zero rather than a negative flood.
    pub fn sample(&mut self, rx: u64, tx: u64, now: Instant) {
        let Some((prev_rx, prev_tx, prev_at)) = self.previous else {
            self.previous = Some((rx, tx, now));
            return;
        };
        let elapsed = now.saturating_duration_since(prev_at);
        if elapsed < SAMPLE_PERIOD {
            return;
        }
        let millis = elapsed.as_millis().max(1) as u64;
        let bytes = rx.saturating_sub(prev_rx) + tx.saturating_sub(prev_tx);
        self.rate = bytes.saturating_mul(1000) / millis;
        if self.window.len() == WINDOW {
            self.window.pop_front();
        }
        self.window.push_back(self.rate);
        self.previous = Some((rx, tx, now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(millis: u64) -> Instant {
        Instant::now() + Duration::from_millis(millis)
    }

    /// The first sample has nothing to measure against: it sets the baseline.
    #[test]
    fn the_first_sample_only_sets_the_baseline() {
        let mut traffic = Traffic::default();
        traffic.sample(10_000, 500, at(0));
        assert_eq!(traffic.rate, 0);
        assert!(traffic.window.is_empty());
    }

    /// A rate is bytes over *time*, not over calls: the pump may run a
    /// thousand times a second, and none of those turns is a sample.
    #[test]
    fn the_rate_is_per_second_however_often_the_pump_runs() {
        let mut traffic = Traffic::default();
        traffic.sample(0, 0, at(0));
        // 100 bytes each way in half a second: 200 + 200 bytes a second.
        traffic.sample(100, 100, at(500));
        assert_eq!(traffic.rate, 400);
        assert_eq!(traffic.window.iter().copied().collect::<Vec<_>>(), vec![400]);
        // A call inside the period changes nothing.
        traffic.sample(500, 500, at(600));
        assert_eq!(traffic.rate, 400);
        assert_eq!(traffic.window.len(), 1);
    }

    /// A stretch longer than one period is the *average* over the stretch: a
    /// graph that spiked on a pause would be lying about the link.
    #[test]
    fn a_long_stretch_averages_over_the_whole_stretch() {
        let mut traffic = Traffic::default();
        traffic.sample(0, 0, at(0));
        // 1000 bytes over two seconds is 500 a second, not a 1000 spike.
        traffic.sample(1000, 0, at(2000));
        assert_eq!(traffic.rate, 500);
    }

    /// The counters reset with the link: a reconnect drops them to zero, which
    /// is a fresh baseline and not a negative rate.
    #[test]
    fn a_counter_reset_is_not_a_negative_rate() {
        let mut traffic = Traffic::default();
        traffic.sample(10_000, 10_000, at(0));
        traffic.sample(0, 0, at(500));
        assert_eq!(traffic.rate, 0);
    }

    /// The history is bounded: old samples leave from the front, so the newest
    /// is always the last — the end the graph draws at its right edge.
    #[test]
    fn the_history_keeps_the_newest_samples() {
        let mut traffic = Traffic::default();
        traffic.sample(0, 0, at(0));
        for step in 0..(WINDOW as u64 + 10) {
            traffic.sample(step * 1_000, 0, at((step + 1) * 1_000));
        }
        assert_eq!(traffic.window.len(), WINDOW);
        assert_eq!(
            traffic.window.back().copied(),
            Some(1000),
            "the newest sample is the last one"
        );
        assert_eq!(traffic.window.front().copied(), Some(1000), "old left");
    }

    /// The exactly-on-time call counts: the period is a floor, the boundary
    /// its own case.
    #[test]
    fn a_call_on_the_period_is_a_sample() {
        let mut traffic = Traffic::default();
        traffic.sample(0, 0, at(0));
        traffic.sample(250, 0, at(250));
        assert_eq!(traffic.rate, 1000);
        assert_eq!(traffic.window.len(), 1);
    }
}
