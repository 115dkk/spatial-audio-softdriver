//! When an exclusive WASAPI stream counts as stalled (`output`), kept apart
//! from it so the rule is tested on every OS.
//!
//! After one missed deadline an exclusive-mode USB endpoint (a TOPPING DAC on
//! usbaudio2) was seen to signal only every ~33 ms from then on, at every
//! period size, so the stream fell behind for good; a restart brings back the
//! normal cadence. A single long wait is not that: the first wait after a
//! start includes the device's start-up, and under CPU load the thread can
//! wake late once (a 2-minute soak with a CPU hog restarted a healthy virtual
//! cable in its first second). A restart drops what the device had queued, so
//! only a cadence that stays slow for `STALLED_WAITS` waits in a row counts.

/// Consecutive long waits that make a stall.
pub(crate) const STALLED_WAITS: u32 = 3;

#[derive(Debug, Default)]
pub(crate) struct StallGuard {
    slow_waits: u32,
    waits_since_start: u32,
}

impl StallGuard {
    /// A new start: the next wait includes the device's start-up.
    pub(crate) fn started(&mut self) {
        self.slow_waits = 0;
        self.waits_since_start = 0;
    }

    /// Records one wait for the device; true when the stream should restart.
    pub(crate) fn stalled(&mut self, waited_ticks: u64, stall_ticks: u64) -> bool {
        let first = self.waits_since_start == 0;
        self.waits_since_start = self.waits_since_start.saturating_add(1);
        if first || waited_ticks <= stall_ticks {
            self.slow_waits = 0;
            return false;
        }
        self.slow_waits += 1;
        self.slow_waits >= STALLED_WAITS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STALL: u64 = 400_000;
    const SLOW: u64 = 330_000 * 2;
    const NORMAL: u64 = 200_000;

    fn run(waits: &[u64]) -> Vec<bool> {
        let mut guard = StallGuard::default();
        guard.started();
        waits
            .iter()
            .map(|&waited| guard.stalled(waited, STALL))
            .collect()
    }

    #[test]
    fn one_late_wake_does_not_restart_the_stream() {
        assert!(!run(&[NORMAL, SLOW, NORMAL, NORMAL]).contains(&true));
        assert!(!run(&[NORMAL, SLOW, SLOW, NORMAL, SLOW, SLOW, NORMAL]).contains(&true));
    }

    #[test]
    fn the_first_wait_after_a_start_is_the_device_starting() {
        assert!(!run(&[SLOW, SLOW, SLOW, NORMAL]).contains(&true));
        assert_eq!(run(&[SLOW, SLOW, SLOW, SLOW]), [false, false, false, true]);
    }

    #[test]
    fn a_cadence_that_stays_slow_restarts_and_counts_again_after_the_start() {
        let mut guard = StallGuard::default();
        guard.started();
        let waits = [NORMAL, SLOW, SLOW, SLOW];
        let decisions: Vec<bool> = waits
            .iter()
            .map(|&waited| guard.stalled(waited, STALL))
            .collect();
        assert_eq!(decisions, [false, false, false, true]);
        guard.started();
        assert!(!guard.stalled(SLOW, STALL));
        assert!(!guard.stalled(SLOW, STALL));
        assert!(!guard.stalled(SLOW, STALL));
        assert!(guard.stalled(SLOW, STALL));
    }
}
