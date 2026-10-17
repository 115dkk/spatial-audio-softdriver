use rtrb::Consumer;
use rubato::audioadapter::AdapterMut;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::audioadapter_buffers::owned::InterleavedOwned;
use rubato::{
    Adjustable, Async, FixedAsync, Resampler, SincInterpolationParameters, WindowFunction,
};
use ssr_types::{OutputFormat, OutputLayout};

use super::{ClockPreference, MAX_ASRC_INPUT_FRAMES, MAX_OUTPUT_PERIOD_FRAMES, MIN_DEVICE_RATE};

const ASRC_MAX_RELATIVE_RATIO: f64 = 1.002_1;
const CADENCE_WINDOW_SECONDS: f64 = 2.0;
const CADENCE_FAULT_SECONDS: f64 = 3.0;
pub(super) const PI_LIMIT_PPM: f64 = 2_000.0;
pub(super) const PI_KP: f64 = 60.0;
pub(super) const PI_KI: f64 = 3.0;
const PI_INTEGRAL_BAND_MS: f64 = 12.0;
pub(super) const CONTROL_WINDOW_SECONDS: f64 = 0.2;
/// How fast the trough the PI follows moves to the setpoint, in milliseconds
/// of ring level per second; one millisecond per second is a 1,000 ppm
/// correction. Half the limit leaves the other half for clock skew.
const REFERENCE_SLEW_MS_PER_SECOND: f64 = PI_LIMIT_PPM / 2.0 / 1_000.0;
/// The reference moves to the lowest setpoint of this many recent windows,
/// so a raised setpoint must hold that long before it is followed. A late
/// capture step that delivers two blocks at once raises the largest recent
/// delivery (see `ArrivalPeak`) for eight steps, up to a quarter second of
/// E-AC-3; followed with the feedforward, that flicker added 1,000 ppm to
/// the dip the late step itself left and reached the limit. The lowest, not
/// one that held: with such steps every second no setpoint holds five
/// windows in a row, and a live target change would never be followed.
pub(super) const SETPOINT_SETTLE_WINDOWS: usize = 5;
const ARRIVAL_WINDOW: usize = 8;
/// A learned source stall remains relevant for 30 seconds after the last
/// catch-up delivery, long enough to cover stalls that recur every few seconds.
pub(super) const STALL_HOLD_SECONDS: f64 = 30.0;
/// At most 150 ms at 48 kHz is kept for a recurring source stall, less than
/// half the default 320 ms crossing ring so catch-up deliveries retain room.
pub(super) const MAX_STALL_RESERVE_FRAMES: u64 = 7_200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClockMode {
    Asrc = 1,
    Trim = 2,
}

impl ClockMode {
    pub(super) fn as_u32(self) -> u32 {
        self as u32
    }
    pub(super) fn from_u32(value: u32) -> Self {
        if value == 2 { Self::Trim } else { Self::Asrc }
    }
}

pub(super) fn select_output_clock(
    preference: ClockPreference,
    trim_available: bool,
    format: OutputFormat,
    layout: OutputLayout,
) -> Result<ClockMode, &'static str> {
    if format.channels != layout.channels {
        return Err("the output sink channel count does not match the requested layout");
    }
    if format.rate < MIN_DEVICE_RATE || format.period_frames == 0 {
        return Err("the output format rate is below 8 kHz or its period is zero");
    }
    if format.period_frames > MAX_OUTPUT_PERIOD_FRAMES {
        return Err("the output period exceeds the runtime's 16,384-frame limit");
    }
    match preference {
        ClockPreference::Trim if format.rate != crate::MIX_RATE => {
            Err("Trim requires a 48 kHz output device for the common mix rate")
        }
        ClockPreference::Trim => Ok(ClockMode::Trim),
        ClockPreference::Auto if trim_available && format.rate == crate::MIX_RATE => {
            Ok(ClockMode::Trim)
        }
        ClockPreference::Auto | ClockPreference::Asrc => Ok(ClockMode::Asrc),
    }
}

#[derive(Clone, Copy)]
pub(super) struct Rendered {
    pub(super) source_frames: usize,
    pub(super) output_frames: usize,
}

pub(super) struct DirectClock;

impl DirectClock {
    pub(super) fn render(
        &mut self,
        audio: &mut Consumer<f32>,
        output: &mut [f32],
        channels: usize,
        stream_frame: &mut u64,
    ) -> Rendered {
        let available_samples = audio.slots().min(output.len());
        let samples = available_samples - available_samples % channels;
        if samples != 0 {
            let (popped, _) = audio.pop_partial_slice(&mut output[..samples]);
            debug_assert_eq!(popped.len(), samples);
        }
        let frames = samples / channels;
        *stream_frame = stream_frame.saturating_add(frames as u64);
        Rendered {
            source_frames: frames,
            output_frames: frames,
        }
    }
}

pub(super) struct AsrcClock {
    pub(super) resampler: Option<Async<f32>>,
    pub(super) input: InterleavedOwned<f32>,
    pub(super) nominal_ratio: f64,
    pub(super) scene_rate: u32,
    pub(super) device_rate: u32,
    pub(super) period_output_frames: usize,
    pub(super) channels: usize,
    pub(super) input_limit: usize,
    pub(super) real_output_frames: usize,
    /// The source has ended: a last take shorter than the resampler asks for
    /// is completed with silence instead of waiting for frames that will not
    /// come.
    pub(super) ending: bool,
    pub(super) ratio_frozen: bool,
}

impl AsrcClock {
    pub(super) fn new(period_frames: usize, device_rate: u32, channels: usize) -> Self {
        // A 256-tap Blackman-Harris sinc with cubic interpolation is rubato's
        // documented high-quality starting point. Stereo uses the combined-sinc
        // optimization, making cubic suitable for a 48 kHz real-time stream.
        let parameters = SincInterpolationParameters::new(256, WindowFunction::BlackmanHarris2);
        let ratio = f64::from(device_rate) / f64::from(crate::MIX_RATE);
        let resampler = Async::new_sinc(
            ratio,
            ASRC_MAX_RELATIVE_RATIO,
            &parameters,
            period_frames,
            channels,
            FixedAsync::Output,
        )
        .ok();
        Self {
            resampler,
            input: InterleavedOwned::new(0.0, channels, MAX_ASRC_INPUT_FRAMES),
            nominal_ratio: ratio,
            scene_rate: 0,
            device_rate,
            period_output_frames: period_frames,
            channels,
            input_limit: usize::MAX,
            real_output_frames: 0,
            ending: false,
            ratio_frozen: false,
        }
    }

    pub(super) fn reset(&mut self, scene_rate: u32, device_rate: u32) {
        self.ratio_frozen = false;
        debug_assert!(scene_rate == 0 || scene_rate == crate::MIX_RATE);
        if scene_rate == 0 || device_rate != self.device_rate {
            self.scene_rate = 0;
            return;
        }
        if let Some(resampler) = &mut self.resampler {
            resampler.reset();
        }
        self.nominal_ratio = f64::from(device_rate) / f64::from(crate::MIX_RATE);
        self.scene_rate = crate::MIX_RATE;
    }

    pub(super) fn set_ppm(&mut self, ppm: f64, scene_rate: u32, device_rate: u32) {
        if self.ratio_frozen {
            return;
        }
        if self.scene_rate != scene_rate || self.resampler.is_none() {
            self.reset(scene_rate, device_rate);
        }
        if let Some(resampler) = &mut self.resampler {
            let ratio = self.nominal_ratio * (1.0 - ppm * 1.0e-6);
            let _ = resampler.set_resample_ratio(ratio, true);
        }
    }

    pub(super) fn freeze_ratio(&mut self) {
        let Some(resampler) = &mut self.resampler else {
            self.ratio_frozen = true;
            return;
        };
        let ratio = resampler.resample_ratio();
        let _ = resampler.set_resample_ratio(ratio, false);
        self.ratio_frozen = true;
    }

    pub(super) fn buffered_frames(&self) -> usize {
        0
    }

    /// Scene frames the next render takes from the ring: one period at the
    /// current ratio, plus the filter's lookahead before the first render.
    pub(super) fn frames_needed(&self) -> usize {
        self.resampler
            .as_ref()
            .map_or(self.period_output_frames, |resampler| {
                resampler.input_frames_next()
            })
    }

    pub(super) fn output_delay(&self) -> usize {
        self.resampler
            .as_ref()
            .map_or(0, rubato::Resampler::output_delay)
    }

    pub(super) fn drain_periods(&self) -> usize {
        self.output_delay().div_ceil(self.period_output_frames)
    }

    pub(super) fn flush(&mut self, output: &mut [f32]) {
        let Some(resampler) = &mut self.resampler else {
            return;
        };
        let input_frames = resampler.input_frames_next();
        if input_frames > MAX_ASRC_INPUT_FRAMES {
            return;
        }
        for frame in 0..input_frames {
            for channel in 0..self.channels {
                let _ = self.input.write_sample(channel, frame, &0.0);
            }
        }
        let output_frames_capacity = output.len() / self.channels;
        let Ok(mut output_adapter) =
            InterleavedSlice::new_mut(output, self.channels, output_frames_capacity)
        else {
            return;
        };
        let _ = resampler.process_into_buffer(&self.input, &mut output_adapter, None);
    }
}

impl AsrcClock {
    pub(super) fn render(
        &mut self,
        audio: &mut Consumer<f32>,
        output: &mut [f32],
        scene_rate: u32,
        device_rate: u32,
        stream_frame: &mut u64,
    ) -> Rendered {
        self.real_output_frames = 0;
        if self.scene_rate != scene_rate || self.resampler.is_none() {
            self.reset(scene_rate, device_rate);
        }
        let Some(resampler) = &mut self.resampler else {
            return Rendered {
                source_frames: 0,
                output_frames: 0,
            };
        };
        let input_frames = resampler.input_frames_next();
        let available = (audio.slots() / self.channels).min(self.input_limit);
        // Before the end the whole take must be there; at the end the frames
        // left over go out followed by silence, so the ring drains.
        let short = available < input_frames && (!self.ending || available == 0);
        if input_frames > MAX_ASRC_INPUT_FRAMES || short {
            return Rendered {
                source_frames: 0,
                output_frames: 0,
            };
        }
        let real = available.min(input_frames);
        for frame in 0..input_frames {
            for channel in 0..self.channels {
                let sample = if frame < real {
                    audio.pop().expect("availability was checked")
                } else {
                    0.0
                };
                let _ = self.input.write_sample(channel, frame, &sample);
            }
        }
        let output_frames_capacity = output.len() / self.channels;
        let Ok(mut output_adapter) =
            InterleavedSlice::new_mut(output, self.channels, output_frames_capacity)
        else {
            return Rendered {
                source_frames: 0,
                output_frames: 0,
            };
        };
        let processed = resampler.process_into_buffer(&self.input, &mut output_adapter, None);
        let Ok((consumed, produced)) = processed else {
            return Rendered {
                source_frames: 0,
                output_frames: 0,
            };
        };
        let output_frames = output_frames_capacity.min(produced);
        self.real_output_frames = if real == input_frames {
            output_frames
        } else {
            output_frames
                .saturating_mul(real)
                .saturating_add(input_frames / 2)
                / input_frames
        };
        let consumed = consumed.min(real);
        *stream_frame = stream_frame.saturating_add(consumed as u64);
        Rendered {
            source_frames: consumed,
            output_frames,
        }
    }
}

pub(super) struct PiController {
    pub(super) integral: f64,
}

impl PiController {
    pub(super) fn new() -> Self {
        Self { integral: 0.0 }
    }

    /// One window's correction in ppm: `feedforward_ppm` (what the
    /// reference's own motion needs, see [`TroughController`]) plus P and I
    /// on the error from that reference.
    pub(super) fn update(&mut self, error_ms: f64, seconds: f64, feedforward_ppm: f64) -> f64 {
        let proportional = PI_KP * error_ms;
        // The integral is the clock skew. An error beyond the band is a
        // one-off (a late delivery, a phase slip) for P to correct, and
        // integrating it, or integrating further into a saturated output,
        // winds the integral up so it keeps correcting past the reference
        // until the output runs dry. While the reference moves (feedforward),
        // the error is how far the trough lags it: with a period as long as a
        // delivery the trough moves in whole deliveries, half a block behind
        // a ramp on average, and the integral learned that lag as skew.
        let unclamped = feedforward_ppm + proportional + PI_KI * self.integral;
        let saturated_this_way =
            unclamped.abs() >= PI_LIMIT_PPM && unclamped.signum() == error_ms.signum();
        if feedforward_ppm == 0.0 && error_ms.abs() <= PI_INTEGRAL_BAND_MS && !saturated_this_way {
            self.integral = (self.integral + error_ms * seconds)
                .clamp(-PI_LIMIT_PPM / PI_KI, PI_LIMIT_PPM / PI_KI);
        }
        (feedforward_ppm + proportional + PI_KI * self.integral).clamp(-PI_LIMIT_PPM, PI_LIMIT_PPM)
    }
}

/// The largest number of frames one capture step put into the crossing ring,
/// over the last [`ARRIVAL_WINDOW`] steps that put any. The mixer cuts its
/// output into `MIX_FRAMES` blocks, but a decoder delivers a whole sync frame
/// in one step, so the ring waits that long, not one block, for the next.
pub(super) struct ArrivalPeak {
    pub(super) recent: [u64; ARRIVAL_WINDOW],
    pub(super) next: usize,
}

impl ArrivalPeak {
    pub(super) fn new() -> Self {
        Self {
            recent: [0; ARRIVAL_WINDOW],
            next: 0,
        }
    }

    pub(super) fn peak(&self) -> u64 {
        self.recent.iter().copied().max().unwrap_or(0)
    }

    /// Records one step's delivery and returns the window's largest.
    pub(super) fn record(&mut self, frames: u64) -> u64 {
        self.recent[self.next] = frames;
        self.next = (self.next + 1) % ARRIVAL_WINDOW;
        self.peak()
    }
}

/// Extra trough margin learned from a catch-up delivery after a source stall.
/// It is fixed-size state suitable for the real-time capture thread.
pub(super) struct StallReserve {
    frames: u64,
    age_seconds: f64,
}

impl StallReserve {
    pub(super) fn new() -> Self {
        Self {
            frames: 0,
            age_seconds: 0.0,
        }
    }

    /// Observes one non-empty capture-step delivery against the peak from
    /// before that delivery. A delivery is a catch-up only when its excess is
    /// larger than both 20 ms and the prior peak, which excludes ordinary
    /// one-step PCM and DD+ lateness; an empty prior window cannot establish a
    /// normal delivery size. A catch-up keeps the largest excess and restarts
    /// the hold, so stalls that keep coming keep the reserve even when they
    /// are shorter than the one it was learned from. Other deliveries age it
    /// by elapsed output time.
    pub(super) fn record(&mut self, delivery: u64, prior_peak: u64, elapsed_seconds: f64) -> u64 {
        let excess = delivery.saturating_sub(prior_peak);
        let threshold = (2 * crate::MIX_FRAMES as u64).max(prior_peak);
        if prior_peak != 0 && excess > threshold {
            self.frames = self.frames.max(excess.min(MAX_STALL_RESERVE_FRAMES));
            self.age_seconds = 0.0;
        } else {
            self.age_seconds += elapsed_seconds;
            if self.age_seconds > STALL_HOLD_SECONDS {
                self.frames = 0;
                self.age_seconds = 0.0;
            }
        }
        self.frames
    }
}

/// Where the controller holds the trough, in frames: the target beyond what
/// one delivery landing just after a take instead of just before it costs
/// that take, which is the smaller of the take and the delivery, plus the
/// learned reserve for a recurring source stall. The trough alone cannot show
/// that cost while every period gets its delivery: with a
/// period at least as long as a delivery (10 ms shared on 10 ms PCM, 20 ms
/// periods) it is flat until the arrival phase crosses the period boundary,
/// so a controller holding it at the target kept the ring one late delivery
/// from empty, and the period that missed its delivery underran. Priming
/// fills to target + delivery + take, so the trough starts here. Capped where
/// the ring still takes a delivery after the take.
pub(super) fn trough_setpoint_frames(
    target: f64,
    take: f64,
    delivery: f64,
    reserve: f64,
    capacity: f64,
) -> f64 {
    (target + take.min(delivery) + reserve).min((capacity - delivery - take).max(0.0))
}

/// The PI controller fed with the ring's trough: the lowest level left after
/// an output period, taken over a [`CONTROL_WINDOW_SECONDS`] window.
///
/// The PI does not hold the trough at the setpoint directly but at a
/// reference that starts at the first window's trough after priming and
/// moves to the setpoint at [`REFERENCE_SLEW_MS_PER_SECOND`], with the
/// correction that motion needs fed forward. Priming keeps every audible
/// frame (ADR 0028) and waits for a whole delivery, so the trough can start a
/// block or a backlog above the setpoint, and the setpoint itself steps when
/// the largest recent delivery changes. That is latency to move, not clock
/// skew. Held at the setpoint directly, the PI drained such an excess with P
/// while the integral, inside its band for the last 10 ms block of it,
/// learned a false skew of hundreds of ppm, and the trough then fell to the
/// bare target once per block slip for a minute or more. Against the
/// reference the error is only what the clocks add, and the integral learns
/// only once the reference is still, so it learns only skew.
pub(super) struct TroughController {
    pub(super) pi: PiController,
    pub(super) trough_ms: f64,
    pub(super) seconds: f64,
    /// Where the trough of the window being measured should end, in ms;
    /// `None` until the first window after (re)priming.
    pub(super) reference_ms: Option<f64>,
    /// The setpoints of the last [`SETPOINT_SETTLE_WINDOWS`] windows, in ms
    /// (infinite until seen), and where the next one goes.
    pub(super) recent_setpoints_ms: [f64; SETPOINT_SETTLE_WINDOWS],
    pub(super) next_setpoint: usize,
}

impl TroughController {
    pub(super) fn new() -> Self {
        Self {
            pi: PiController::new(),
            trough_ms: f64::INFINITY,
            seconds: 0.0,
            reference_ms: None,
            recent_setpoints_ms: [f64::INFINITY; SETPOINT_SETTLE_WINDOWS],
            next_setpoint: 0,
        }
    }

    pub(super) fn reset(&mut self) {
        *self = Self::new();
    }

    /// Priming ended: the next window, measured from here, is where the
    /// reference starts. A window begun before the ring ran dry or was
    /// refilled would start it at a level the ring no longer has. The learned
    /// skew stays.
    pub(super) fn restart_reference(&mut self) {
        self.reference_ms = None;
        self.trough_ms = f64::INFINITY;
        self.seconds = 0.0;
    }

    /// Records the level left after one period; when a window completes,
    /// returns its trough and the new correction in ppm (positive: the source
    /// is fast, so ASRC consumes faster; trim slows the source with negative).
    pub(super) fn observe(
        &mut self,
        level_ms: f64,
        period_seconds: f64,
        setpoint_ms: f64,
        mode: ClockMode,
    ) -> Option<(f64, f64)> {
        self.trough_ms = self.trough_ms.min(level_ms);
        self.seconds += period_seconds;
        if self.seconds < CONTROL_WINDOW_SECONDS {
            return None;
        }
        let trough_ms = self.trough_ms;
        let seconds = self.seconds;
        self.recent_setpoints_ms[self.next_setpoint] = setpoint_ms;
        self.next_setpoint = (self.next_setpoint + 1) % SETPOINT_SETTLE_WINDOWS;
        let goal_ms = self
            .recent_setpoints_ms
            .iter()
            .copied()
            .fold(f64::INFINITY, f64::min);
        let reference_ms = self.reference_ms.unwrap_or(trough_ms);
        // Where the next window should end, and the drain (positive) or fill
        // that takes over a window as long as this one.
        let slew_ms = REFERENCE_SLEW_MS_PER_SECOND * seconds;
        let next_ms = reference_ms + (goal_ms - reference_ms).clamp(-slew_ms, slew_ms);
        let drain_ppm = (reference_ms - next_ms) / seconds * 1_000.0;
        self.reference_ms = Some(next_ms);
        let (error_ms, feedforward_ppm) = match mode {
            ClockMode::Trim => (reference_ms - trough_ms, -drain_ppm),
            ClockMode::Asrc => (trough_ms - reference_ms, drain_ppm),
        };
        let ppm = self.pi.update(error_ms, seconds, feedforward_ppm);
        self.trough_ms = f64::INFINITY;
        self.seconds = 0.0;
        Some((trough_ms, ppm))
    }
}

pub(super) struct CadenceGuard {
    pub(super) start_qpc: Option<u64>,
    pub(super) start_position: Option<u64>,
    pub(super) bad_seconds: f64,
    pub(super) latched: bool,
    pub(super) newly_faulted: bool,
}

impl CadenceGuard {
    pub(super) fn new() -> Self {
        Self {
            start_qpc: None,
            start_position: None,
            bad_seconds: 0.0,
            latched: false,
            newly_faulted: false,
        }
    }

    pub(super) fn reset(&mut self) {
        *self = Self::new();
    }

    pub(super) fn observe(
        &mut self,
        qpc: u64,
        position: Option<u64>,
        qpc_frequency: u64,
        rate: u32,
    ) -> Option<f32> {
        self.newly_faulted = false;
        let position = position?;
        let Some(start_qpc) = self.start_qpc else {
            self.start_qpc = Some(qpc);
            self.start_position = Some(position);
            return None;
        };
        let seconds = qpc.saturating_sub(start_qpc) as f64 / qpc_frequency as f64;
        if seconds < CADENCE_WINDOW_SECONDS {
            return None;
        }
        let frames = position.saturating_sub(self.start_position.unwrap_or(position));
        let ratio = frames as f64 / (seconds * f64::from(rate));
        self.start_qpc = Some(qpc);
        self.start_position = Some(position);
        if !(0.9..=1.1).contains(&ratio) {
            self.bad_seconds += seconds;
            if self.bad_seconds >= CADENCE_FAULT_SECONDS && !self.latched {
                self.latched = true;
                self.newly_faulted = true;
            }
        } else {
            self.bad_seconds = 0.0;
            self.latched = false;
        }
        Some(ratio as f32)
    }

    pub(super) fn faulted(&self) -> bool {
        self.newly_faulted
    }
}
