//! Playback timestamps for the WASAPI sink (`output`), kept apart from it so
//! the arithmetic is tested on every OS.
//!
//! A reading is the audio clock's correlated (position, QPC) pair.
//! GetPosition's QPC is in 100 ns units, NOT raw performance-counter ticks.

#[derive(Clone, Copy, Debug)]
pub(crate) struct ClockReading {
    pub(crate) position: u64,
    pub(crate) qpc_100ns: u64,
}

/// Playback timestamps follow the frames written and lean on the device clock.
/// A clock reading sets the QPC outright only when it is the first, follows an
/// estimate, or disagrees with the frame count by more than
/// `RESYNC_PERIODS` periods (an underrun gap or a restart). Otherwise the
/// previous period plus one period predicts the QPC and an eighth of the
/// reading's disagreement corrects it. Shared-mode readings on a virtual cable
/// were seen to move by whole periods (the position updated every other
/// period), which put two periods at one QPC; contiguous frames cannot play at
/// one instant. The correction keeps a device clock that runs off nominal
/// from accumulating: its lag stays near seven times the per-period offset.
#[derive(Debug, Default)]
pub(crate) struct PlaybackClock {
    last_qpc: Option<u64>,
    measured: bool,
}

const RESYNC_PERIODS: i128 = 4;
const CORRECTION_SHIFT: u32 = 3;

impl PlaybackClock {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn next(
        &mut self,
        first_frame: u64,
        reading: Option<ClockReading>,
        started_qpc: u64,
        stream_rate: u32,
        clock_frequency: u64,
        qpc_frequency: u64,
        period_frames: usize,
    ) -> u64 {
        let period_ticks = scale(period_frames as u64, qpc_frequency, u64::from(stream_rate));
        let next = match (self.last_qpc, reading) {
            (None, None) => started_qpc.saturating_add(scale(
                first_frame,
                qpc_frequency,
                u64::from(stream_rate),
            )),
            (None, Some(reading)) => playback_qpc(
                reading.qpc_100ns,
                reading.position,
                clock_frequency,
                first_frame,
                stream_rate,
                qpc_frequency,
            ),
            (Some(last), None) => started_qpc
                .saturating_add(scale(first_frame, qpc_frequency, u64::from(stream_rate)))
                .max(last.saturating_add(period_ticks)),
            (Some(last), Some(reading)) => {
                let candidate = playback_qpc(
                    reading.qpc_100ns,
                    reading.position,
                    clock_frequency,
                    first_frame,
                    stream_rate,
                    qpc_frequency,
                );
                let predicted = last.saturating_add(period_ticks);
                let error = i128::from(candidate) - i128::from(predicted);
                if !self.measured || error.abs() > RESYNC_PERIODS * i128::from(period_ticks) {
                    candidate.max(last.saturating_add(1))
                } else {
                    (i128::from(predicted) + (error >> CORRECTION_SHIFT))
                        .clamp(i128::from(last) + 1, i128::from(u64::MAX))
                        as u64
                }
            }
        };
        self.measured = reading.is_some();
        self.last_qpc = Some(next);
        next
    }
}

pub(crate) fn scale(value: u64, numerator: u64, denominator: u64) -> u64 {
    (u128::from(value) * u128::from(numerator) / u128::from(denominator)).min(u128::from(u64::MAX))
        as u64
}

fn playback_qpc(
    clock_qpc: u64,
    position: u64,
    clock_frequency: u64,
    frame: u64,
    rate: u32,
    qpc_frequency: u64,
) -> u64 {
    let base = i128::from(scale(clock_qpc, qpc_frequency, 10_000_000));
    let queued = i128::from(scale(frame, qpc_frequency, u64::from(rate)))
        - i128::from(scale(position, qpc_frequency, clock_frequency));
    (base + queued).clamp(0, i128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_timestamp_uses_clock_units_and_queue_distance() {
        // A 24 MHz QPC, 192 kHz device clock, 48 kHz stream: 10 ms queued.
        assert_eq!(
            playback_qpc(20_000_000, 192_000, 192_000, 48_480, 48_000, 24_000_000),
            48_240_000
        );
    }

    #[test]
    fn playback_clock_is_monotonic_across_estimates_measurements_and_restarts() {
        #[derive(Clone, Copy, Debug)]
        struct Row {
            first_frame: u64,
            reading: Option<ClockReading>,
            started_qpc: u64,
        }

        for qpc_frequency in [10_000_000, 24_000_000] {
            let period = qpc_frequency / 100;
            let base = 7 * qpc_frequency;
            let rows = [
                // Startup estimate.
                Row {
                    first_frame: 480,
                    reading: None,
                    started_qpc: base,
                },
                // First correlation reports the hardware delay and moves ahead.
                Row {
                    first_frame: 960,
                    reading: Some(ClockReading {
                        position: 480,
                        qpc_100ns: 70_200_000,
                    }),
                    started_qpc: base,
                },
                // A regressing correlation is clamped by one whole period.
                Row {
                    first_frame: 1_440,
                    reading: Some(ClockReading {
                        position: 960,
                        qpc_100ns: 70_150_000,
                    }),
                    started_qpc: base,
                },
                // Correlation can disappear temporarily after it existed.
                Row {
                    first_frame: 1_920,
                    reading: None,
                    started_qpc: base,
                },
                // Reset rewinds submitted frames and changes the estimate base.
                Row {
                    first_frame: 480,
                    reading: None,
                    started_qpc: base - qpc_frequency,
                },
                // Large device positions must not overflow intermediate math.
                Row {
                    first_frame: u64::MAX - 480,
                    reading: Some(ClockReading {
                        position: u64::MAX - 960,
                        qpc_100ns: 80_000_000,
                    }),
                    started_qpc: base,
                },
            ];
            let mut clock = PlaybackClock::default();
            let mut actual = Vec::new();
            for row in rows {
                actual.push(clock.next(
                    row.first_frame,
                    row.reading,
                    row.started_qpc,
                    48_000,
                    48_000,
                    qpc_frequency,
                    480,
                ));
            }
            assert!(
                actual.windows(2).all(|pair| pair[1] >= pair[0]),
                "qpc_frequency={qpc_frequency}, rows={rows:#?}, actual={actual:?}"
            );
            assert!(
                actual[1] > base + 2 * period,
                "forward measurement was not accepted: qpc_frequency={qpc_frequency}, rows={rows:#?}, actual={actual:?}"
            );
        }
    }

    #[test]
    fn fast_dac_measurements_do_not_accumulate_nominal_period_error() {
        let qpc_frequency = 10_000_000;
        let rate = 48_000;
        let period_frames = 480;
        let periods = 360_000_u64;
        let period_100ns = 100_000_u64;
        let mut clock = PlaybackClock::default();
        let mut last = 0;
        for period in 0..periods {
            let first_frame = (period + 1) * period_frames as u64;
            let position = period * period_frames as u64;
            let qpc_100ns = 50_000_000 + period * period_100ns * 9_999 / 10_000;
            last = clock.next(
                first_frame,
                Some(ClockReading {
                    position,
                    qpc_100ns,
                }),
                5 * qpc_frequency,
                rate,
                u64::from(rate),
                qpc_frequency,
                period_frames,
            );
        }
        let expected =
            5 * qpc_frequency + (periods - 1) * period_100ns * 9_999 / 10_000 + qpc_frequency / 100;
        // The correction lags a clock 100 ppm off by about seven times its
        // per-period offset (10 ticks), and the lag does not grow: an hour of
        // accumulated nominal-period error would be 3.6 million ticks.
        assert!(
            last.abs_diff(expected) <= 8 * period_100ns / 10_000,
            "last {last}, expected {expected}"
        );
    }

    #[test]
    fn readings_that_move_by_whole_periods_still_space_periods_one_period_apart() {
        // A shared stream on a virtual cable: the position advanced every other
        // period while the QPC of each reading was the call's, so the raw
        // estimate alternated between one and two periods ahead. Before the
        // correction this put consecutive periods at the same QPC.
        let qpc_frequency = 10_000_000_u64;
        let rate = 48_000;
        let period_frames = 480_usize;
        let period = 100_000_u64;
        let mut clock = PlaybackClock::default();
        let mut stamps = Vec::new();
        for n in 0..2_000_u64 {
            let call_qpc = 70_000_000 + n * period + (n * 37) % 5_000;
            let position = (n - n % 2) * period_frames as u64;
            stamps.push(clock.next(
                n * period_frames as u64 + period_frames as u64,
                Some(ClockReading {
                    position,
                    qpc_100ns: call_qpc,
                }),
                0,
                rate,
                u64::from(rate),
                qpc_frequency,
                period_frames,
            ));
        }
        for pair in stamps.windows(2).skip(16) {
            let delta = pair[1] - pair[0];
            assert!(
                delta > period * 7 / 8 && delta < period * 9 / 8,
                "delta {delta} for a {period}-tick period"
            );
        }
        assert!(stamps.windows(2).all(|pair| pair[1] > pair[0]));
    }

    #[test]
    fn a_gap_of_more_than_four_periods_takes_the_reading() {
        let qpc_frequency = 10_000_000_u64;
        let mut clock = PlaybackClock::default();
        let reading = |position: u64, qpc_100ns: u64| {
            Some(ClockReading {
                position,
                qpc_100ns,
            })
        };
        let first = clock.next(
            480,
            reading(0, 70_000_000),
            0,
            48_000,
            48_000,
            qpc_frequency,
            480,
        );
        let second = clock.next(
            960,
            reading(480, 70_100_000),
            0,
            48_000,
            48_000,
            qpc_frequency,
            480,
        );
        assert_eq!(second - first, 100_000);
        // An underrun: the device played 50 ms of silence before this period.
        let after_gap = clock.next(
            3_840,
            reading(3_360, 70_700_000),
            0,
            48_000,
            48_000,
            qpc_frequency,
            480,
        );
        assert_eq!(after_gap, 70_800_000);
    }
}
