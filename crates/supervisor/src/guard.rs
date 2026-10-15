//! The decision core of `ssr-guard` (ADR 0023 decision 4).
//!
//! The guard runs beside the app with user rights. It watches two things: that
//! the app process is alive, and that the engine makes progress (the output
//! thread writes a period, silent or not, on every exclusive period, and the
//! capture thread reads). Counters that stand still for `stall_after` mean the
//! engine stalled even though the app is alive. The first stall restarts the
//! app; a second stall, or the app exiting, gives the default roles back
//! (`restore_point::restore`) and tells the user.
//!
//! This module only decides. Reading the counters from shared memory, watching
//! the process and restarting it belong to the Windows process, which feeds
//! the clock and the observations in, so the decisions are tested here.
//!
//! On [`Action::RestoreAndNotify`] the Windows process reads the restore point
//! first and does nothing (no restore, no notification) when there is none:
//! that is the normal quit, where the supervisor already gave the roles back
//! and removed the restore point before the app exited.

use std::time::{Duration, Instant};

/// The engine's progress counters as the guard last read them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub output_periods: u64,
    pub capture_reads: u64,
}

/// One look at the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observation {
    pub app_alive: bool,
    /// `None` when the counters cannot be read.
    pub progress: Option<Progress>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// Restart the app, then call [`Guard::app_restarted`].
    RestartApp,
    /// Give the roles back from the restore point and tell the user why.
    RestoreAndNotify {
        reason: Reason,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    AppExited,
    /// The engine stalled again after the guard restarted the app.
    RestartFailed,
}

#[derive(Clone, Debug)]
pub struct Guard {
    stall_after: Duration,
    /// The guard already restarted the app once.
    restarted: bool,
    /// The counters last read.
    last_progress: Option<Progress>,
    /// The start of the current still period: when the counters last moved,
    /// or the first observation since the clock was reset.
    still_since: Option<Instant>,
    /// The guard asked for the restore; nothing is left to decide.
    finished: bool,
}

impl Guard {
    /// The stall threshold ADR 0023 names.
    pub const STALL_AFTER: Duration = Duration::from_secs(2);

    pub fn new(stall_after: Duration) -> Self {
        Self {
            stall_after,
            restarted: false,
            last_progress: None,
            still_since: None,
            finished: false,
        }
    }

    /// Decides what to do about one observation made at `now`.
    ///
    /// A dead app is restored once. Either counter moving restarts the stall
    /// clock; counters that stand still, or cannot be read, for `stall_after`
    /// are a stall. The first stall asks for a restart, the second for the
    /// restore. After a restore every observation is [`Action::Nothing`].
    pub fn observe(&mut self, now: Instant, seen: Observation) -> Action {
        if self.finished {
            return Action::Nothing;
        }
        if !seen.app_alive {
            return self.finish(Reason::AppExited);
        }
        let still_since = *self.still_since.get_or_insert(now);
        if let Some(progress) = seen.progress
            && self.last_progress != Some(progress)
        {
            self.last_progress = Some(progress);
            self.still_since = Some(now);
            return Action::Nothing;
        }
        if now.saturating_duration_since(still_since) < self.stall_after {
            return Action::Nothing;
        }
        if self.restarted {
            return self.finish(Reason::RestartFailed);
        }
        self.restarted = true;
        self.reset_clock();
        Action::RestartApp
    }

    /// After the guard asked for a restart: the new app's first observation
    /// resets the stall clock, so a slow start is not a stall.
    pub fn app_restarted(&mut self) {
        self.reset_clock();
    }

    /// The app says it is not Running (off, in setup, or quitting): the
    /// counters are not expected to move, so the stall clock stops. The
    /// restart already spent stays spent: a second stall after the app is
    /// enabled again still restores.
    pub fn app_idle(&mut self) {
        self.reset_clock();
    }

    fn reset_clock(&mut self) {
        self.last_progress = None;
        self.still_since = None;
    }

    fn finish(&mut self, reason: Reason) -> Action {
        self.finished = true;
        Action::RestoreAndNotify { reason }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alive(output_periods: u64, capture_reads: u64) -> Observation {
        Observation {
            app_alive: true,
            progress: Some(Progress {
                output_periods,
                capture_reads,
            }),
        }
    }

    fn at(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    #[test]
    fn a_dead_app_restores_once() {
        let mut guard = Guard::new(Guard::STALL_AFTER);
        let start = Instant::now();
        assert_eq!(guard.observe(start, alive(1, 1)), Action::Nothing);
        let dead = Observation {
            app_alive: false,
            progress: None,
        };
        assert_eq!(
            guard.observe(at(start, 100), dead),
            Action::RestoreAndNotify {
                reason: Reason::AppExited
            }
        );
        assert_eq!(guard.observe(at(start, 200), dead), Action::Nothing);
        assert_eq!(
            guard.observe(at(start, 5_000), alive(1, 1)),
            Action::Nothing
        );
    }

    #[test]
    fn a_stall_restarts_then_restores() {
        let mut guard = Guard::new(Guard::STALL_AFTER);
        let start = Instant::now();
        assert_eq!(guard.observe(start, alive(10, 5)), Action::Nothing);
        assert_eq!(
            guard.observe(at(start, 1_000), alive(10, 5)),
            Action::Nothing
        );
        assert_eq!(
            guard.observe(at(start, 1_999), alive(10, 5)),
            Action::Nothing
        );
        assert_eq!(
            guard.observe(at(start, 2_000), alive(10, 5)),
            Action::RestartApp
        );

        // The new app takes a while to start; its first observation starts the
        // clock again, and its counters start over.
        guard.app_restarted();
        assert_eq!(
            guard.observe(at(start, 3_500), alive(0, 0)),
            Action::Nothing
        );
        assert_eq!(
            guard.observe(at(start, 4_000), alive(3, 2)),
            Action::Nothing
        );
        assert_eq!(
            guard.observe(at(start, 5_999), alive(3, 2)),
            Action::Nothing
        );
        assert_eq!(
            guard.observe(at(start, 6_000), alive(3, 2)),
            Action::RestoreAndNotify {
                reason: Reason::RestartFailed
            }
        );
        assert_eq!(
            guard.observe(at(start, 9_000), alive(3, 2)),
            Action::Nothing
        );
    }

    #[test]
    fn moving_counters_are_not_a_stall() {
        let mut guard = Guard::new(Guard::STALL_AFTER);
        let start = Instant::now();
        // Output periods alone, then capture reads alone, then both: any
        // counter moving is progress (the output writes silence without input).
        for step in 0..10_u64 {
            assert_eq!(
                guard.observe(at(start, step * 1_500), alive(step, 0)),
                Action::Nothing
            );
        }
        for step in 10..20_u64 {
            assert_eq!(
                guard.observe(at(start, step * 1_500), alive(9, step)),
                Action::Nothing
            );
        }
        for step in 20..30_u64 {
            assert_eq!(
                guard.observe(at(start, step * 1_500), alive(step, step)),
                Action::Nothing
            );
        }
    }

    #[test]
    fn unreadable_counters_count_as_a_stall() {
        let mut guard = Guard::new(Guard::STALL_AFTER);
        let start = Instant::now();
        let unreadable = Observation {
            app_alive: true,
            progress: None,
        };
        assert_eq!(guard.observe(start, unreadable), Action::Nothing);
        assert_eq!(guard.observe(at(start, 1_000), unreadable), Action::Nothing);
        assert_eq!(
            guard.observe(at(start, 2_000), unreadable),
            Action::RestartApp
        );

        // Counters that were moving and then cannot be read: the clock runs
        // from the last movement.
        guard.app_restarted();
        assert_eq!(
            guard.observe(at(start, 3_000), alive(1, 1)),
            Action::Nothing
        );
        assert_eq!(guard.observe(at(start, 4_000), unreadable), Action::Nothing);
        assert_eq!(
            guard.observe(at(start, 5_000), unreadable),
            Action::RestoreAndNotify {
                reason: Reason::RestartFailed
            }
        );
    }

    #[test]
    fn idling_stops_the_clock_but_keeps_the_restart_spent() {
        let mut guard = Guard::new(Guard::STALL_AFTER);
        let start = Instant::now();
        assert_eq!(guard.observe(start, alive(1, 1)), Action::Nothing);
        assert_eq!(
            guard.observe(at(start, 2_000), alive(1, 1)),
            Action::RestartApp
        );
        guard.app_restarted();
        assert_eq!(
            guard.observe(at(start, 3_000), alive(1, 1)),
            Action::Nothing
        );

        // Off for a while: no stall, however long.
        guard.app_idle();
        assert_eq!(
            guard.observe(at(start, 10_000), alive(1, 1)),
            Action::Nothing
        );
        assert_eq!(
            guard.observe(at(start, 11_000), alive(2, 2)),
            Action::Nothing
        );
        // Enabled again and stalled again: the one restart is spent.
        assert_eq!(
            guard.observe(at(start, 13_000), alive(2, 2)),
            Action::RestoreAndNotify {
                reason: Reason::RestartFailed
            }
        );
    }
}
