use std::sync::Arc;
use std::sync::atomic::Ordering;

use render::Ramp;
use rtrb::Producer;
use ssr_types::{OutputError, OutputLayout, OutputSink, UnderrunSnapshot};

use super::clock::*;
use super::crossing::{CrossingConsumer, PeriodPlayback};
use super::{
    BOUNDARY_FADE_MS, EventWriter, LatencyWriter, MAX_OUTPUT_CHANNELS, MUTE_RAMP_MS, RtEvent,
    Shared,
};

pub(super) fn boundary_fade_frames(rate: u32) -> usize {
    ((f64::from(rate) * BOUNDARY_FADE_MS / 1_000.0).round() as usize).max(1)
}

/// Gain at `offset` of a fade that falls linearly from 1 to 0 over `total`
/// frames, reaching 0 on the last one. A fade of one frame or none is 0.
#[inline]
fn descending_boundary_gain(offset: usize, total: usize) -> f32 {
    if total <= 1 {
        0.0
    } else {
        (total - 1 - offset) as f32 / (total - 1) as f32
    }
}

/// Gain of drain frame `frame` of an ASRC tail. The old resampler's last
/// `real_frames` drained frames are real signal; the boundary fade ends at zero
/// on the last of them, over its last `fade_frames` (the rest of the fade,
/// `lead_frames`, lies in the last rendered period when the delay is shorter
/// than the fade). Real frames before the fade play at full level and frames
/// after the signal are silent.
pub(super) fn asrc_drain_gain(
    frame: usize,
    real_frames: usize,
    fade_frames: usize,
    lead_frames: usize,
) -> f32 {
    if frame >= real_frames {
        return 0.0;
    }
    let fade_start = real_frames - fade_frames.min(real_frames);
    if frame < fade_start {
        return 1.0;
    }
    let total = lead_frames + fade_frames;
    if total <= 1 {
        return 0.0;
    }
    let offset = lead_frames + (frame - fade_start);
    descending_boundary_gain(offset, total)
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct TailFade {
    /// Fade frames in the final rendered period, ending at `period_signal_end`.
    pub(super) lead_frames: usize,
    /// Frame of the final rendered period after the signal's last; frames
    /// from here to the period's end become zero. Unused without a period.
    pub(super) period_signal_end: usize,
    /// Real-signal frames in the drain, and the fade's frames among them.
    pub(super) drain_real_frames: usize,
    pub(super) drain_fade_frames: usize,
}

/// Plans the ASRC boundary fade across the last rendered period and its drain.
/// `rendered` is `(period_frames, real_output_frames)` of the final rendered
/// period when the fade may still touch it, or `None` when that period already
/// went to the sink.
pub(super) fn plan_asrc_tail_fade(
    delay: usize,
    fade: usize,
    rendered: Option<(usize, usize)>,
) -> TailFade {
    let Some((period_frames, real_output_frames)) = rendered else {
        return TailFade {
            lead_frames: 0,
            period_signal_end: 0,
            drain_real_frames: delay,
            drain_fade_frames: fade.min(delay),
        };
    };
    let signal_end = real_output_frames.saturating_add(delay);
    if signal_end >= period_frames {
        let drain_real_frames = signal_end - period_frames;
        let drain_fade_frames = fade.min(drain_real_frames);
        TailFade {
            lead_frames: fade.saturating_sub(drain_fade_frames).min(period_frames),
            period_signal_end: period_frames,
            drain_real_frames,
            drain_fade_frames,
        }
    } else {
        TailFade {
            lead_frames: fade.min(signal_end),
            period_signal_end: signal_end,
            drain_real_frames: 0,
            drain_fade_frames: 0,
        }
    }
}

pub(super) fn apply_fade_in(
    buffer: &mut [f32],
    channels: usize,
    rate: u32,
    audible_frames: usize,
    remaining: &mut usize,
) {
    if *remaining == 0 || audible_frames == 0 {
        return;
    }
    let total = boundary_fade_frames(rate);
    let used = total.saturating_sub(*remaining);
    let ramp = Ramp {
        from: 0.0,
        to: 1.0,
        frames: total,
    };
    for (offset, frame) in buffer
        .chunks_exact_mut(channels)
        .take(audible_frames)
        .enumerate()
    {
        if offset >= *remaining {
            break;
        }
        let gain = ramp.gain_with_start(used + offset);
        for sample in frame {
            *sample *= gain;
        }
    }
    *remaining = remaining.saturating_sub(audible_frames);
}

/// Why output must return to priming, and therefore which state is reset.
///
/// `Started` establishes the initial priming and fade. `SceneStarted` and
/// `ClockChanged` also restart the controller, ASRC, tail, and latency state.
/// `Underrun` restarts only priming and its fade. `SinkRestarted` additionally
/// clears latency and cadence measurements. `CrossingDiscarded` replaces the
/// ASRC, clears its tail, accounts for the discarded frames, and clears latency
/// and cadence measurements. `Reconfigured` resets all output-format-dependent
/// state after [`OutputState::reconfigure`] installs the new format.
///
/// Deliberate, unchanged subsets: an underrun keeps the controller, ASRC,
/// latency, and cadence; a sink restart keeps the controller and ASRC; and a
/// crossing discard keeps the controller. Keeping the controller keeps its
/// learned skew, and the capture thread's stall reserve is independent of
/// every reprime. The end of priming restarts only the moving reference
/// ([`TroughController::restart_reference`]).
#[derive(Clone, Copy, Debug)]
pub(super) enum Reprime {
    Started,
    SceneStarted,
    ClockChanged,
    Underrun,
    SinkRestarted,
    CrossingDiscarded { overflow_frames: u64 },
    Reconfigured,
}

pub(super) struct OutputState {
    pub(super) crossing: CrossingConsumer,
    pub(super) events: EventWriter,
    pub(super) shared: Arc<Shared>,
    pub(super) device_rate: u32,
    pub(super) target_ms: f64,
    pub(super) qpc_frequency: u64,
    pub(super) controller: TroughController,
    pub(super) direct: DirectClock,
    pub(super) asrc: AsrcClock,
    pub(super) strategy_mode: ClockMode,
    pub(super) layout: OutputLayout,
    pub(super) channels: usize,
    pub(super) period_frames: usize,
    pub(super) period_ring_level_frames: usize,
    /// Ring level left after this period's frames were taken.
    pub(super) period_trough_frames: usize,
    /// Set at start, after an underrun or a device restart, and when the
    /// strategy or scene rate changes: output silence until the ring holds
    /// target + one delivery + one period, then drop the exact-zero frames
    /// beyond that, so the trough starts at the target instead of at zero.
    pub(super) priming: bool,
    pub(super) period_stream_start: u64,
    pub(super) period_source_frames: usize,
    pub(super) period_had_audio: bool,
    pub(super) mute_gain: f32,
    pub(super) fade_in_remaining: usize,
    pub(super) last_output: [f32; MAX_OUTPUT_CHANNELS],
    pub(super) have_last_output: bool,
    pub(super) latency: LatencyWriter,
    pub(super) cadence: CadenceGuard,
    /// Periods of the resampler's tail still owed to the current sink, by a
    /// reopen or by the end of capture, whichever writes them.
    pub(super) tail_periods: usize,
    pub(super) drain_real_frames: usize,
    pub(super) drain_fade_frames: usize,
    pub(super) drain_fade_lead_frames: usize,
    pub(super) period_signal_end: usize,
    pub(super) drain_frames_written: usize,
    pub(super) drain_fade_prepared: bool,
    pub(super) asrc_tail_pending: bool,
    pub(super) scene_started: bool,
}

impl OutputState {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        crossing: CrossingConsumer,
        device_rate: u32,
        period_frames: usize,
        target_ms: f32,
        qpc_frequency: u64,
        event_producer: Producer<RtEvent>,
        shared: Arc<Shared>,
        layout: OutputLayout,
        _capacity_frames: usize,
    ) -> Self {
        let channels = usize::from(layout.channels);
        let events = EventWriter::new(event_producer, Arc::clone(&shared));
        let strategy_mode = ClockMode::from_u32(shared.clock_mode.load(Ordering::Acquire));
        let latency = LatencyWriter::new(&shared);
        let mut output = Self {
            crossing,
            events,
            shared,
            device_rate,
            target_ms: f64::from(target_ms),
            qpc_frequency,
            controller: TroughController::new(),
            direct: DirectClock,
            asrc: AsrcClock::new(period_frames, device_rate, channels),
            strategy_mode,
            layout,
            channels,
            period_frames,
            period_ring_level_frames: 0,
            period_trough_frames: 0,
            priming: false,
            period_stream_start: 0,
            period_source_frames: 0,
            period_had_audio: false,
            mute_gain: 1.0,
            fade_in_remaining: 0,
            last_output: [0.0; MAX_OUTPUT_CHANNELS],
            have_last_output: false,
            latency,
            cadence: CadenceGuard::new(),
            tail_periods: 0,
            drain_real_frames: 0,
            drain_fade_frames: 0,
            drain_fade_lead_frames: 0,
            period_signal_end: 0,
            drain_frames_written: 0,
            drain_fade_prepared: false,
            asrc_tail_pending: false,
            scene_started: false,
        };
        output.reprime(Reprime::Started);
        output
    }

    pub(super) fn reprime(&mut self, why: Reprime) {
        match why {
            Reprime::Started => {
                self.priming = true;
                self.fade_in_remaining = boundary_fade_frames(self.device_rate);
            }
            Reprime::SceneStarted | Reprime::ClockChanged => {
                self.controller.reset();
                self.asrc.reset(crate::MIX_RATE, self.device_rate);
                self.asrc_tail_pending = false;
                self.priming = true;
                self.fade_in_remaining = boundary_fade_frames(self.device_rate);
                self.latency.reset(&self.shared);
            }
            Reprime::Underrun => {
                self.priming = true;
                self.fade_in_remaining = boundary_fade_frames(self.device_rate);
            }
            Reprime::SinkRestarted => {
                self.priming = true;
                self.fade_in_remaining = boundary_fade_frames(self.device_rate);
                self.latency.reset(&self.shared);
                self.cadence.reset();
            }
            Reprime::CrossingDiscarded { overflow_frames } => {
                self.asrc = AsrcClock::new(self.period_frames, self.device_rate, self.channels);
                self.asrc.reset(crate::MIX_RATE, self.device_rate);
                self.asrc_tail_pending = false;
                self.tail_periods = 0;
                self.shared
                    .output_overflow_frames
                    .fetch_add(overflow_frames, Ordering::Relaxed);
                self.priming = true;
                self.fade_in_remaining = boundary_fade_frames(self.device_rate);
                self.latency.reset(&self.shared);
                self.cadence.reset();
            }
            Reprime::Reconfigured => {
                self.controller.reset();
                self.direct = DirectClock;
                self.asrc = AsrcClock::new(self.period_frames, self.device_rate, self.channels);
                self.asrc.reset(crate::MIX_RATE, self.device_rate);
                self.priming = true;
                self.period_source_frames = 0;
                self.period_had_audio = false;
                self.mute_gain = 1.0;
                self.fade_in_remaining = boundary_fade_frames(self.device_rate);
                self.last_output.fill(0.0);
                self.have_last_output = false;
                self.latency.reset(&self.shared);
                self.cadence.reset();
                self.drain_fade_prepared = false;
                self.tail_periods = 0;
                self.asrc_tail_pending = false;
            }
        }
    }

    pub(super) fn fill(&mut self, buffer: &mut [f32]) {
        // Read once, before anything about the ring: every block capture
        // published before it finished is then visible to this period, and
        // every decision about how the period ends uses this reading. Capture
        // finishing after it is a source that was late for this period.
        let capture_done = self.shared.capture_done.load(Ordering::Acquire);
        buffer.fill(0.0);
        self.crossing.refresh_markers();
        self.target_ms = f64::from(f32::from_bits(
            self.shared.target_ms_bits.load(Ordering::Acquire),
        ));
        self.period_ring_level_frames = self.crossing.queued_frames(self.channels);
        self.period_stream_start = self.crossing.stream_frame;
        self.period_source_frames = 0;
        self.crossing.discard_stale_markers();
        self.period_had_audio = false;

        let scene_rate = self.shared.scene_rate.load(Ordering::Acquire);
        let mode = ClockMode::from_u32(self.shared.clock_mode.load(Ordering::Acquire));
        if scene_rate == 0 {
            self.apply_mute(buffer);
            return;
        }
        if !self.scene_started {
            self.scene_started = true;
            self.strategy_mode = mode;
            self.reprime(Reprime::SceneStarted);
        } else if mode != self.strategy_mode {
            self.strategy_mode = mode;
            self.reprime(Reprime::ClockChanged);
        }
        if self.priming {
            // The source ended before the normal target was reached: drain
            // every queued frame rather than treating the first sound as
            // disposable priming excess.
            if !self.shared.capture_input_ended.load(Ordering::Acquire) {
                // The direct clock copies the period's frames from the ring as is.
                let take = match mode {
                    ClockMode::Trim => buffer.len() / self.channels,
                    ClockMode::Asrc => self.asrc.frames_needed(),
                };
                let Some(excess) = self.priming_excess(scene_rate, take) else {
                    self.apply_mute(buffer);
                    return;
                };
                self.discard(excess);
            }
            self.priming = false;
            // What priming kept above the setpoint (a burst, a backlog, a
            // delivery larger than a period) is latency, not clock skew.
            self.controller.restart_reference();
        }
        let frames = buffer.len() / self.channels;
        let layout_frames = self
            .crossing
            .current_layout_frames(self.layout, self.channels);
        let marker_frames = self.crossing.queued_frames(self.channels);
        let at_layout_boundary = layout_frames < marker_frames;
        self.asrc.input_limit = layout_frames;
        let seen = UnderrunSnapshot {
            ring_frames: (self.crossing.audio.slots() / self.channels) as u32,
            marker_frames: marker_frames as u32,
            layout_frames: layout_frames as u32,
            needed_frames: match mode {
                ClockMode::Trim => frames,
                ClockMode::Asrc => self.asrc.frames_needed(),
            } as u32,
            produced_frames: 0,
            since_block_ms: 0.0,
        };
        let produced = match mode {
            ClockMode::Trim => {
                let output_frames = frames.min(layout_frames);
                self.direct.render(
                    &mut self.crossing.audio,
                    &mut buffer[..output_frames * self.channels],
                    self.channels,
                    &mut self.crossing.stream_frame,
                )
            }
            ClockMode::Asrc => {
                self.asrc.ending = at_layout_boundary || capture_done;
                self.asrc.render(
                    &mut self.crossing.audio,
                    &mut buffer[..frames * self.channels],
                    scene_rate,
                    self.device_rate,
                    &mut self.crossing.stream_frame,
                )
            }
        };
        self.period_source_frames = produced.source_frames;
        self.period_had_audio = produced.output_frames != 0;
        if mode == ClockMode::Asrc && produced.source_frames != 0 {
            self.asrc_tail_pending = self.asrc.drain_periods() != 0;
        }
        // The stream ended here: nothing more is coming, so a short period is
        // its end, not a source falling behind.
        let stream_ended = capture_done && self.drained();
        let layout_ended = at_layout_boundary && produced.source_frames >= layout_frames;
        if (stream_ended || layout_ended)
            && mode == ClockMode::Asrc
            && produced.output_frames == 0
            && self.asrc_tail_pending
            && self.tail_periods == 0
        {
            // The take that emptied the stream or the old layout already went
            // out (the end was not visible yet), so the resampler's tail goes
            // in this period; a reopen writes whatever of it is left.
            self.plan_sent_period_tail();
            self.fill_tail_period(buffer);
            self.tail_periods = self.tail_periods.saturating_sub(1);
            return;
        }
        // Only a resampler that consumed input owes a tail.
        let drains_asrc = mode == ClockMode::Asrc
            && self.asrc_tail_pending
            && self.asrc.drain_periods() != 0
            && (layout_ended || stream_ended);
        if drains_asrc {
            self.prepare_asrc_drain_fade(Some((
                produced.output_frames,
                self.asrc.real_output_frames,
            )));
            self.tail_periods = self.asrc.drain_periods();
            self.asrc_tail_pending = false;
            self.apply_asrc_drain_lead_fade(buffer);
        }
        if produced.output_frames < frames {
            self.apply_fade_in(buffer, produced.output_frames);
            if !drains_asrc {
                if produced.output_frames == 0 {
                    self.apply_zero_frame_fade(buffer);
                } else {
                    self.apply_fade_out(buffer, produced.output_frames);
                }
            }
            if !layout_ended && !stream_ended {
                self.shared.underruns.fetch_add(1, Ordering::Relaxed);
                self.shared.publish_underrun(UnderrunSnapshot {
                    produced_frames: produced.output_frames as u32,
                    ..seen
                });
                self.reprime(Reprime::Underrun);
            }
        } else if layout_ended {
            self.apply_fade_in(buffer, produced.output_frames);
            if !drains_asrc {
                self.apply_fade_out(buffer, produced.output_frames);
            }
        } else {
            self.apply_fade_in(buffer, produced.output_frames);
        }
        self.crossing.refresh_markers();
        self.period_trough_frames = self.crossing.queued_frames(self.channels);
        self.apply_mute(buffer);
        if let Some(frame) = buffer.chunks_exact(self.channels).last() {
            self.last_output[..self.channels].copy_from_slice(frame);
            self.have_last_output = frame.iter().any(|sample| *sample != 0.0);
        }
    }

    fn prepare_asrc_drain_fade(&mut self, rendered: Option<(usize, usize)>) {
        if self.drain_fade_prepared {
            return;
        }
        let plan = plan_asrc_tail_fade(
            self.asrc.output_delay(),
            boundary_fade_frames(self.device_rate),
            rendered,
        );
        self.drain_real_frames = plan.drain_real_frames;
        self.drain_fade_frames = plan.drain_fade_frames;
        self.drain_fade_lead_frames = plan.lead_frames;
        self.period_signal_end = plan.period_signal_end;
        self.drain_frames_written = 0;
        self.drain_fade_prepared = true;
    }

    fn apply_asrc_drain_lead_fade(&self, buffer: &mut [f32]) {
        let total_fade_frames = self
            .drain_fade_lead_frames
            .saturating_add(self.drain_fade_frames);
        let fade_start = self
            .period_signal_end
            .saturating_sub(self.drain_fade_lead_frames);
        for (frame_index, frame) in buffer.chunks_exact_mut(self.channels).enumerate() {
            let gain = if frame_index >= self.period_signal_end {
                0.0
            } else if frame_index < fade_start {
                1.0
            } else if total_fade_frames <= 1 {
                0.0
            } else {
                let offset = frame_index - fade_start;
                descending_boundary_gain(offset, total_fade_frames)
            };
            for sample in frame {
                *sample *= gain;
            }
        }
    }

    pub(super) fn apply_asrc_drain_fade(&mut self, buffer: &mut [f32]) {
        let period_frames = buffer.len() / self.channels;
        for (offset, frame) in buffer.chunks_exact_mut(self.channels).enumerate() {
            let gain = asrc_drain_gain(
                self.drain_frames_written.saturating_add(offset),
                self.drain_real_frames,
                self.drain_fade_frames,
                self.drain_fade_lead_frames,
            );
            for sample in frame {
                *sample *= gain;
            }
        }
        self.drain_frames_written = self.drain_frames_written.saturating_add(period_frames);
    }

    /// `None` until the ring holds target + the next delivery + this period's
    /// take (capped one delivery below capacity, where the capture side would
    /// start dropping blocks instead); then the frames beyond target plus the
    /// delivery and the take. The next delivery is the larger of the head
    /// block and the largest recent capture step: a decoder hands over a
    /// whole sync frame (32 ms of E-AC-3) as several mixer blocks at once, and
    /// the ring must last until the next one. Only exact-zero frames may actually be discarded;
    /// audible excess remains as latency for the PI controller to reduce.
    /// The take is in scene frames and, for the ASRC, includes the filter's
    /// lookahead before its first render.
    fn priming_excess(&self, scene_rate: u32, take: usize) -> Option<usize> {
        let rate = f64::from(scene_rate);
        let block_frames = self.delivery_frames(scene_rate);
        let target_frames = self.target_ms * rate / 1_000.0;
        let take = take as f64;
        let threshold = (target_frames + block_frames + take)
            .min(self.crossing.capacity_frames as f64 - block_frames);
        let level = self.period_ring_level_frames as f64;
        if level < threshold {
            return None;
        }
        let keep = (target_frames + block_frames + take).min(threshold);
        Some((level - keep).max(0.0) as usize)
    }

    /// The next delivery in scene frames: the head block, or the largest
    /// recent capture step if that is larger (a DD+ sync frame lands as
    /// several mixer blocks at once).
    fn delivery_frames(&self, scene_rate: u32) -> f64 {
        self.crossing
            .pending_marker
            .map_or(0.0, |marker| {
                let marker_rate = crate::MIX_RATE;
                f64::from(marker.frames) * f64::from(scene_rate) / f64::from(marker_rate)
            })
            .max(self.shared.arrival_frames.load(Ordering::Acquire) as f64)
    }

    /// The trough the controller holds, in ms (see [`trough_setpoint_frames`]).
    fn trough_setpoint_ms(&self, scene_rate: u32, take_frames: f64) -> f64 {
        let rate = f64::from(scene_rate);
        trough_setpoint_frames(
            self.target_ms * rate / 1_000.0,
            take_frames,
            self.delivery_frames(scene_rate),
            self.shared.stall_reserve_frames.load(Ordering::Acquire) as f64,
            self.crossing.capacity_frames as f64,
        ) * 1_000.0
            / rate
    }

    /// Drops up to `frames` exact-zero frames from the head of the ring,
    /// keeping the stream position and latency markers in step. Inspection
    /// stops at the first audible frame.
    pub(super) fn discard(&mut self, frames: usize) {
        let discarded = self.crossing.discard(frames, self.channels);
        if discarded != 0 {
            self.period_stream_start = self.crossing.stream_frame;
        }
    }

    fn apply_fade_in(&mut self, buffer: &mut [f32], audible_frames: usize) {
        apply_fade_in(
            buffer,
            self.channels,
            self.device_rate,
            audible_frames,
            &mut self.fade_in_remaining,
        );
    }

    /// The drain continues the resampler's output, so a fade-in still running
    /// (the take before was the first after priming, or an underrun silenced
    /// the period before) continues into it rather than jumping to full gain.
    fn apply_drain_fade_in(&mut self, buffer: &mut [f32]) {
        let frames = buffer.len() / self.channels;
        self.apply_fade_in(buffer, frames);
    }

    fn apply_zero_frame_fade(&self, buffer: &mut [f32]) {
        if !self.have_last_output {
            return;
        }
        let frames = boundary_fade_frames(self.device_rate).min(buffer.len() / self.channels);
        for (offset, frame) in buffer
            .chunks_exact_mut(self.channels)
            .take(frames)
            .enumerate()
        {
            let gain = descending_boundary_gain(offset, frames);
            for (channel, sample) in frame.iter_mut().enumerate() {
                *sample = self.last_output[channel] * gain;
            }
        }
    }

    pub(super) fn apply_fade_out(&self, buffer: &mut [f32], audible_frames: usize) {
        let frames = boundary_fade_frames(self.device_rate).min(audible_frames);
        let start = audible_frames.saturating_sub(frames);
        for (offset, frame) in buffer
            .chunks_exact_mut(self.channels)
            .skip(start)
            .take(frames)
            .enumerate()
        {
            let gain = descending_boundary_gain(offset, frames);
            for sample in frame {
                *sample *= gain;
            }
        }
    }

    fn apply_mute(&mut self, buffer: &mut [f32]) {
        let target = if self.shared.mute.load(Ordering::Acquire) {
            0.0_f32
        } else {
            1.0_f32
        };
        let ramp_frames =
            ((f64::from(self.device_rate) * MUTE_RAMP_MS / 1_000.0).round() as usize).max(1);
        let step = 1.0 / ramp_frames as f32;
        for frame in buffer.chunks_exact_mut(self.channels) {
            if self.mute_gain < target {
                self.mute_gain = (self.mute_gain + step).min(target);
            } else if self.mute_gain > target {
                self.mute_gain = (self.mute_gain - step).max(target);
            }
            for sample in frame {
                *sample *= self.mute_gain;
            }
        }
    }

    pub(super) fn finish_period(&mut self, playback_qpc: u64) -> Option<(f64, f64)> {
        let scene_rate = self.shared.scene_rate.load(Ordering::Acquire);
        if scene_rate == 0 {
            return None;
        }
        let to_ms = |frames: usize| frames as f64 * 1_000.0 / f64::from(scene_rate);
        let period_seconds = self.period_frames as f64 / f64::from(self.device_rate);
        self.shared
            .output_elapsed_ns
            .fetch_add((period_seconds * 1.0e9).round() as u64, Ordering::Relaxed);
        if !self.period_had_audio {
            // Priming or starved: show the level that is building up.
            self.shared.ring_level_bits.store(
                (to_ms(self.period_ring_level_frames) as f32).to_bits(),
                Ordering::Release,
            );
            return None;
        }
        let setpoint_ms =
            self.trough_setpoint_ms(scene_rate, period_seconds * f64::from(scene_rate));
        let completed = self.controller.observe(
            to_ms(self.period_trough_frames),
            period_seconds,
            setpoint_ms,
            self.strategy_mode,
        );
        if let Some((trough_ms, ppm)) = completed {
            self.shared
                .ring_level_bits
                .store((trough_ms as f32).to_bits(), Ordering::Release);
            self.shared
                .clock_ppm_bits
                .store(ppm.to_bits(), Ordering::Release);
            match self.strategy_mode {
                ClockMode::Trim => self
                    .shared
                    .trim_ppm_bits
                    .store(ppm.to_bits(), Ordering::Release),
                // The ASRC ratio is output/input. A positive controller value
                // means the ring is high, so consume faster by reducing it.
                ClockMode::Asrc => {
                    self.shared
                        .trim_ppm_bits
                        .store(0.0_f64.to_bits(), Ordering::Release);
                    if !self.drain_fade_prepared {
                        self.asrc.set_ppm(ppm, scene_rate, self.device_rate);
                    }
                }
            }
        }
        self.consume_markers(playback_qpc);
        completed
    }

    pub(super) fn layout_at_boundary(&mut self) -> Option<OutputLayout> {
        self.crossing.layout_at_boundary()
    }

    pub(super) fn pending_layout_change(&mut self) -> Option<OutputLayout> {
        self.crossing.pending_layout_change(self.layout)
    }

    pub(super) fn discard_pending_layout(&mut self, layout: OutputLayout) -> u64 {
        let frames = self
            .crossing
            .discard_buffered_audio(|marker| marker.layout == layout);
        if frames != 0 {
            self.shared
                .output_overflow_frames
                .fetch_add(frames, Ordering::Relaxed);
            self.events.push(RtEvent::OutputOverflow { frames });
        }
        frames
    }

    pub(super) fn discard_crossing_audio(&mut self) {
        let overflow_frames = self.crossing.discard_buffered_audio(|_| true);
        self.reprime(Reprime::CrossingDiscarded { overflow_frames });
    }

    fn consume_markers(&mut self, playback_qpc: u64) {
        let period = PeriodPlayback {
            start: self.period_stream_start,
            frames: self.period_source_frames,
            qpc: playback_qpc,
            mode: self.strategy_mode,
            device_rate: self.device_rate,
            qpc_frequency: self.qpc_frequency,
        };
        let shared = &self.shared;
        let latency = &mut self.latency;
        self.crossing
            .consume_markers(period, |value| latency.push(value, shared));
    }

    pub(super) fn reconfigure(
        &mut self,
        mode: ClockMode,
        layout: OutputLayout,
        device_rate: u32,
        period_frames: usize,
    ) {
        self.strategy_mode = mode;
        self.layout = layout;
        self.channels = usize::from(layout.channels);
        self.device_rate = device_rate;
        self.period_frames = period_frames;
        self.reprime(Reprime::Reconfigured);
    }

    pub(super) fn drained(&self) -> bool {
        self.crossing.drained() && self.asrc.buffered_frames() == 0
    }

    /// Capture has ended and every frame it produced has been rendered.
    pub(super) fn ended(&self) -> bool {
        self.shared.capture_started.load(Ordering::Acquire)
            && self.shared.capture_done.load(Ordering::Acquire)
            && self.drained()
    }

    /// Plans the tail of a resampler whose last rendered period already went
    /// to the sink: no fade was planned in that period, so all of it lies in
    /// the tail, and the ratio stays as it is until the tail is out.
    fn plan_sent_period_tail(&mut self) {
        self.asrc.freeze_ratio();
        self.prepare_asrc_drain_fade(None);
        self.tail_periods = self.asrc.drain_periods();
        self.asrc_tail_pending = false;
    }

    /// One period of the resampler's tail: what it still holds, faded as
    /// planned, then silence.
    fn fill_tail_period(&mut self, buffer: &mut [f32]) {
        buffer.fill(0.0);
        self.asrc.flush(buffer);
        self.apply_drain_fade_in(buffer);
        self.apply_asrc_drain_fade(buffer);
        self.apply_mute(buffer);
    }

    /// Writes what the resampler still holds once capture has ended, then
    /// nothing more. A reopen drain the end interrupted continues where it
    /// was rather than starting over.
    pub(super) fn write_terminal_tail<S: OutputSink>(
        &mut self,
        sink: &mut S,
    ) -> Result<(), OutputError> {
        if self.strategy_mode != ClockMode::Asrc {
            return Ok(());
        }
        if self.asrc_tail_pending && self.tail_periods == 0 {
            self.plan_sent_period_tail();
        }
        for _ in 0..self.tail_periods {
            sink.next_period(&mut |buffer| self.fill_tail_period(buffer))?;
            self.shared.output_periods.fetch_add(1, Ordering::Relaxed);
            self.tail_periods -= 1;
        }
        Ok(())
    }

    pub(super) fn begin_reopen_tail(&mut self) -> bool {
        self.asrc.freeze_ratio();
        // Only input the resampler consumed and has not drained yet owes a
        // tail; one already written in full is not planned again.
        if self.strategy_mode == ClockMode::Asrc && self.tail_periods == 0 && self.asrc_tail_pending
        {
            self.plan_sent_period_tail();
        }
        self.tail_periods != 0
    }

    pub(super) fn write_reopen_tail_period<S: OutputSink>(
        &mut self,
        sink: &mut S,
    ) -> Result<bool, OutputError> {
        debug_assert!(self.tail_periods != 0);
        sink.next_period(&mut |buffer| self.fill_tail_period(buffer))?;
        self.shared.output_periods.fetch_add(1, Ordering::Relaxed);
        self.tail_periods = self.tail_periods.saturating_sub(1);
        Ok(self.tail_periods == 0)
    }
}
