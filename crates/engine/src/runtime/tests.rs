use rtrb::RingBuffer;

use super::crossing::{CrossingConsumer, TimingMarker};
use super::*;

fn output_format(rate: u32) -> OutputFormat {
    OutputFormat {
        rate,
        channels: 2,
        channel_mask: 0x3,
        period_frames: 480,
        mode: OutputMode::Exclusive,
        fell_back: false,
        mix_channels: 2,
    }
}

#[test]
fn the_asrc_drain_fade_ends_on_the_last_real_frame() {
    // 44.1 kHz output: 117 real frames in the drain, an 88-frame fade.
    let (real, fade) = (117, 88);
    let gains: Vec<f32> = (0..160)
        .map(|frame| asrc_drain_gain(frame, real, fade, 0))
        .collect();
    assert!(
        gains[..real - fade].iter().all(|gain| *gain == 1.0),
        "{gains:?}"
    );
    assert!(
        gains[real - fade..real]
            .windows(2)
            .all(|pair| pair[1] < pair[0])
    );
    assert_eq!(gains[real - 1], 0.0);
    assert!(gains[real..].iter().all(|gain| *gain == 0.0));
    // A delay shorter than the fade: the drain continues the ramp the last
    // rendered period began (lead 40 of 88 frames).
    let (real, fade, lead) = (48, 48, 40);
    assert_eq!(asrc_drain_gain(0, real, fade, lead), 47.0 / 87.0);
    assert_eq!(asrc_drain_gain(real - 1, real, fade, lead), 0.0);
}

#[test]
fn the_asrc_tail_fade_plan_places_the_ramp_at_the_real_signal_end() {
    assert_eq!(
        plan_asrc_tail_fade(117, 88, None),
        TailFade {
            lead_frames: 0,
            period_signal_end: 0,
            drain_real_frames: 117,
            drain_fade_frames: 88,
        }
    );
    assert_eq!(
        plan_asrc_tail_fade(59, 88, None),
        TailFade {
            lead_frames: 0,
            period_signal_end: 0,
            drain_real_frames: 59,
            drain_fade_frames: 59,
        }
    );
    assert_eq!(
        plan_asrc_tail_fade(117, 88, Some((441, 400))),
        TailFade {
            lead_frames: 12,
            period_signal_end: 441,
            drain_real_frames: 76,
            drain_fade_frames: 76,
        }
    );
    assert_eq!(
        plan_asrc_tail_fade(117, 88, Some((441, 200))),
        TailFade {
            lead_frames: 88,
            period_signal_end: 317,
            drain_real_frames: 0,
            drain_fade_frames: 0,
        }
    );
}

#[test]
fn a_planned_asrc_tail_freezes_the_pending_ratio_through_drain() {
    let scene_rate = 48_000;
    let device_rate = 44_100;
    let period_frames = 441;
    let source_frames = 960;
    let (mut audio_in, mut audio) = RingBuffer::<f32>::new(source_frames * 2);
    for frame in 0..source_frames {
        let sample = (2.0 * std::f32::consts::PI * 997.0 * frame as f32 / scene_rate as f32).sin();
        audio_in.push(sample).unwrap();
        audio_in.push(sample).unwrap();
    }
    let mut asrc = AsrcClock::new(period_frames, device_rate, 2);
    asrc.reset(scene_rate, device_rate);
    let mut rendered = vec![0.0; period_frames * 2];
    let mut stream_frame = 0;
    let result = asrc.render(
        &mut audio,
        &mut rendered,
        scene_rate,
        device_rate,
        &mut stream_frame,
    );
    assert_eq!(result.output_frames, period_frames);
    let planned_end = period_frames + asrc.output_delay();

    asrc.freeze_ratio();
    asrc.set_ppm(400_000.0, scene_rate, device_rate);
    let frozen_end = period_frames + asrc.output_delay();
    let mut drain = vec![0.0; period_frames * 2];
    asrc.flush(&mut drain);
    let planned_delay = planned_end - period_frames;
    for (frame_index, frame) in drain.as_chunks_mut::<2>().0.iter_mut().enumerate() {
        let gain = asrc_drain_gain(
            frame_index,
            planned_delay,
            boundary_fade_frames(device_rate),
            0,
        );
        frame[0] *= gain;
        frame[1] *= gain;
    }
    let last_nonzero = drain
        .as_chunks::<2>()
        .0
        .iter()
        .rposition(|frame| frame[0] != 0.0 || frame[1] != 0.0)
        .map(|frame| period_frames + frame + 1)
        .expect("the frozen ASRC emitted no delayed signal");

    assert_eq!(frozen_end, planned_end);
    assert!(
        last_nonzero.abs_diff(planned_end) <= 1,
        "drained end {last_nonzero} moved away from frozen plan {planned_end}"
    );
    assert!(
        drain[planned_delay * 2..]
            .iter()
            .all(|sample| *sample == 0.0),
        "frozen drain continued past planned end {planned_end}"
    );
}

#[test]
fn leaving_trim_clears_a_sub_deadband_applied_request_exactly() {
    struct TrimCapture(Vec<f64>);

    impl CaptureSource for TrimCapture {
        fn read(&mut self, _buffer: &mut [u8], _wait: Duration) -> Result<usize, CaptureError> {
            Ok(0)
        }

        fn set_rate_trim(&mut self, ppm: f64) -> Result<(), ssr_types::TrimUnsupported> {
            self.0.push(ppm);
            Ok(())
        }
    }

    let shared = Shared::new();
    shared
        .clock_mode
        .store(ClockMode::Trim.as_u32(), Ordering::Release);
    shared
        .trim_ppm_bits
        .store(0.06_f64.to_bits(), Ordering::Release);
    let mut capture = TrimCapture(Vec::new());
    let mut applied = f64::NAN;
    apply_requested_trim(&mut capture, true, &shared, &mut applied);
    shared
        .clock_mode
        .store(ClockMode::Asrc.as_u32(), Ordering::Release);
    apply_requested_trim(&mut capture, true, &shared, &mut applied);

    assert_eq!(capture.0, [0.06, 0.0]);
    assert_eq!(applied.to_bits(), 0.0_f64.to_bits());
}

#[test]
fn output_clock_selection_rechecks_every_opened_format() {
    assert_eq!(
        select_output_clock(
            ClockPreference::Auto,
            true,
            output_format(crate::MIX_RATE),
            OutputLayout::STEREO,
        ),
        Ok(ClockMode::Trim)
    );
    assert_eq!(
        select_output_clock(
            ClockPreference::Auto,
            true,
            output_format(44_100),
            OutputLayout::STEREO,
        ),
        Ok(ClockMode::Asrc)
    );
    assert_eq!(
        select_output_clock(
            ClockPreference::Auto,
            true,
            output_format(crate::MIX_RATE),
            OutputLayout::STEREO,
        ),
        Ok(ClockMode::Trim)
    );
    assert!(
        select_output_clock(
            ClockPreference::Trim,
            true,
            output_format(44_100),
            OutputLayout::STEREO,
        )
        .is_err()
    );
    assert!(
        select_output_clock(
            ClockPreference::Asrc,
            false,
            output_format(MIN_DEVICE_RATE - 1),
            OutputLayout::STEREO,
        )
        .is_err()
    );
    assert_eq!(
        select_output_clock(
            ClockPreference::Asrc,
            false,
            output_format(MIN_DEVICE_RATE),
            OutputLayout::STEREO,
        ),
        Ok(ClockMode::Asrc)
    );
}

#[test]
fn only_errors_without_an_endpoint_use_unattributed_counters() {
    let shared = Shared::new();
    shared.note_chain_error(None, ChainErrorKind::TruncatedRecord);
    shared.note_chain_error(Some(1), ChainErrorKind::CorruptPayload);
    shared.note_chain_error(
        Some(ENGINE_ENDPOINTS as u16),
        ChainErrorKind::BadRecordLength,
    );

    assert_eq!(
        shared
            .read_unattributed_chain_errors()
            .get(ChainErrorKind::TruncatedRecord),
        1
    );
    assert_eq!(shared.read_unattributed_chain_errors().total(), 1);
    assert_eq!(
        shared.endpoints[1]
            .read_chain_errors()
            .get(ChainErrorKind::CorruptPayload),
        1
    );
    assert_eq!(
        shared
            .endpoints
            .iter()
            .map(|endpoint| endpoint.read_chain_errors().total())
            .sum::<u64>(),
        1
    );
}

#[test]
fn cadence_guard_accepts_normal_rejects_sustained_slow_and_ignores_a_blip() {
    fn feed(ratios: &[f64]) -> (Vec<f32>, usize) {
        let mut guard = CadenceGuard::new();
        let mut qpc = 0_u64;
        let mut position = 0_u64;
        let mut observed = Vec::new();
        let mut faults = 0;
        let _ = guard.observe(qpc, Some(position), 1_000, 1_000);
        for ratio in ratios {
            qpc += 2_000;
            position += (2_000.0 * ratio).round() as u64;
            if let Some(value) = guard.observe(qpc, Some(position), 1_000, 1_000) {
                observed.push(value);
                faults += usize::from(guard.faulted());
            }
        }
        (observed, faults)
    }

    assert_eq!(feed(&[1.0, 1.0, 1.0]).1, 0);
    let (slow, faults) = feed(&[0.65, 0.65, 0.65]);
    assert_eq!(slow, [0.65, 0.65, 0.65]);
    assert_eq!(faults, 1);
    assert_eq!(feed(&[0.65, 1.0, 1.0]).1, 0);
}

struct ClockRun {
    underruns: u32,
    clamped: u32,
    /// Windows at the ppm limit over the second half.
    late_clamped: u32,
    /// Lowest and highest window trough over the second half, in ms.
    trough: (f64, f64),
    /// Lowest and highest correction over the second half, in ppm.
    ppm: (f64, f64),
    final_ppm: f64,
}

fn simulate_clock(
    skew_ppm: f64,
    block: usize,
    period: usize,
    trim: bool,
    seconds: f64,
) -> ClockRun {
    simulate_clock_from(skew_ppm, block, period, trim, seconds, 0.0)
}

fn simulate_clock_from(
    skew_ppm: f64,
    block: usize,
    period: usize,
    trim: bool,
    seconds: f64,
    backlog_ms: f64,
) -> ClockRun {
    simulate_clock_with(skew_ppm, block, period, trim, seconds, backlog_ms, 0.0)
}

fn simulated_setpoint_ms(block: usize, period: usize) -> f64 {
    trough_setpoint_frames(480.0, period as f64, block as f64, f64::INFINITY) / 48.0
}

/// Drives the production crossing producer and output state. Source time is
/// continuous, but a complete mixer block becomes visible atomically; optional
/// late deliveries are held for exactly one output call. A starting backlog
/// lands as one capture step, as when capture opens on a full driver ring.
fn simulate_clock_with(
    skew_ppm: f64,
    block: usize,
    period: usize,
    trim: bool,
    seconds: f64,
    backlog_ms: f64,
    late: f64,
) -> ClockRun {
    let mode = if trim {
        ClockMode::Trim
    } else {
        ClockMode::Asrc
    };
    let layout = OutputLayout {
        channels: 1,
        channel_mask: 1,
    };
    let capacity = capacity_frames(320.0, crate::MIX_RATE);
    let shared = Arc::new(Shared::new());
    shared.scene_rate.store(crate::MIX_RATE, Ordering::Release);
    shared.clock_mode.store(mode.as_u32(), Ordering::Release);
    shared.capture_started.store(true, Ordering::Release);
    let (mut crossing, consumer) = crossing::channel(capacity, 1);
    let (event_producer, _event_consumer) = RingBuffer::new(EVENT_QUEUE_CAPACITY);
    let mut output = OutputState::new(
        consumer,
        crate::MIX_RATE,
        period,
        10.0,
        DEFAULT_QPC_FREQUENCY,
        event_producer,
        Arc::clone(&shared),
        layout,
        capacity,
    );
    let (capture_events, _capture_event_consumer) = RingBuffer::new(EVENT_QUEUE_CAPACITY);
    let mut events = EventWriter::new(capture_events, Arc::clone(&shared));
    let samples = vec![0.25; block];
    let push_delivery = |crossing: &mut CrossingProducer, events: &mut EventWriter| {
        crossing.push(
            crate::chain::BlockOut {
                epoch: 1,
                rate: crate::MIX_RATE,
                source_qpc: 0,
                channels: 1,
                channel_mask: 1,
                frames: block,
                samples: &samples,
                silent: false,
                idle: false,
            },
            events,
            &shared,
        );
        shared.arrival_frames.store(block as u64, Ordering::Release);
    };
    let initial_frames = (backlog_ms * f64::from(crate::MIX_RATE) / 1_000.0).round() as usize;
    let initial_deliveries = initial_frames.div_ceil(block);
    for _ in 0..initial_deliveries {
        push_delivery(&mut crossing, &mut events);
    }
    let mut arrivals = ArrivalPeak::new();
    if initial_deliveries != 0 {
        shared.arrival_frames.store(
            arrivals.record((initial_deliveries * block) as u64),
            Ordering::Release,
        );
    }

    let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
    let mut draw = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1_u64 << 53) as f64
    };
    let period_seconds = period as f64 / f64::from(crate::MIX_RATE);
    let periods = (seconds / period_seconds) as usize;
    let mut source_phase = 0.0;
    let mut held_deliveries = 0_usize;
    let mut period_buffer = vec![0.0; period];
    let mut run = ClockRun {
        underruns: 0,
        clamped: 0,
        late_clamped: 0,
        trough: (f64::INFINITY, f64::NEG_INFINITY),
        ppm: (f64::INFINITY, f64::NEG_INFINITY),
        final_ppm: 0.0,
    };
    for index in 0..periods {
        // Held deliveries land together, as one capture step.
        for _ in 0..held_deliveries {
            push_delivery(&mut crossing, &mut events);
        }
        if held_deliveries != 0 {
            shared.arrival_frames.store(
                arrivals.record((held_deliveries * block) as u64),
                Ordering::Release,
            );
        }
        held_deliveries = 0;
        let trim_ppm = if trim { requested_trim(&shared) } else { 0.0 };
        source_phase += period as f64 * (1.0 + (skew_ppm + trim_ppm) * 1.0e-6);
        while source_phase >= block as f64 {
            source_phase -= block as f64;
            if late > 0.0 && draw() < late {
                held_deliveries += 1;
            } else {
                // On time, each delivery is its own capture step, however
                // many land within one output period.
                push_delivery(&mut crossing, &mut events);
                shared
                    .arrival_frames
                    .store(arrivals.record(block as u64), Ordering::Release);
            }
        }
        output.fill(&mut period_buffer);
        let playback_qpc =
            ((index + 1) as f64 * period_seconds * DEFAULT_QPC_FREQUENCY as f64).round() as u64;
        if let Some((trough, ppm)) = output.finish_period(playback_qpc) {
            if ppm.abs() >= PI_LIMIT_PPM {
                run.clamped += 1;
                if index * 2 >= periods {
                    run.late_clamped += 1;
                }
            }
            if index * 2 >= periods {
                run.trough = (run.trough.0.min(trough), run.trough.1.max(trough));
                run.ppm = (run.ppm.0.min(ppm), run.ppm.1.max(ppm));
            }
        }
    }
    run.underruns = shared.underruns.load(Ordering::Acquire) as u32;
    run.final_ppm = f64::from_bits(shared.clock_ppm_bits.load(Ordering::Acquire));
    run
}

#[test]
fn target_change_is_observed_at_the_next_output_period() {
    let shared = Arc::new(Shared::new());
    shared.scene_rate.store(48_000, Ordering::Release);
    shared
        .target_ms_bits
        .store(10.0_f32.to_bits(), Ordering::Release);
    let (_audio_in, audio) = RingBuffer::<f32>::new(2);
    let (_timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
    let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
    let mut output = OutputState::new(
        CrossingConsumer::from_parts(audio, timing, 80 * 48),
        48_000,
        48,
        10.0,
        DEFAULT_QPC_FREQUENCY,
        events,
        Arc::clone(&shared),
        OutputLayout::STEREO,
        80 * 48,
    );
    let mut period = vec![0.0; 48 * 2];
    output.fill(&mut period);
    assert_eq!(output.target_ms, 10.0);
    shared
        .target_ms_bits
        .store(25.0_f32.to_bits(), Ordering::Release);
    assert_eq!(output.target_ms, 10.0);
    output.fill(&mut period);
    assert_eq!(output.target_ms, 25.0);
}

#[test]
fn target_change_rejects_values_outside_crossing_capacity() {
    let (changes, _consumer) = RingBuffer::new(8);
    let (_capture_events_in, capture_events) = RingBuffer::new(1);
    let (_output_events_in, output_events) = RingBuffer::new(1);
    let (_retired_in, retired) = RingBuffer::new(1);
    let shared = Arc::new(Shared::new());
    shared
        .max_target_ms_bits
        .store(50.0_f32.to_bits(), Ordering::Release);
    let engine = Engine {
        shared,
        changes: RefCell::new(changes),
        capture_events,
        output_events,
        retired,
        capture_thread: None,
        output_thread: None,
    };

    assert!(engine.apply(LiveChange::TargetMs(0.0)).is_ok());
    assert!(engine.apply(LiveChange::TargetMs(49.999)).is_ok());
    for invalid in [-0.1, 50.0, f32::NEG_INFINITY, f32::INFINITY, f32::NAN] {
        assert_eq!(engine.apply(LiveChange::TargetMs(invalid)), Err(Rejected));
    }
}

#[test]
fn lfe_gain_change_rejects_values_outside_the_supported_range() {
    let (changes, _consumer) = RingBuffer::new(4);
    let (_capture_events_in, capture_events) = RingBuffer::new(1);
    let (_output_events_in, output_events) = RingBuffer::new(1);
    let (_retired_in, retired) = RingBuffer::new(1);
    let engine = Engine {
        shared: Arc::new(Shared::new()),
        changes: RefCell::new(changes),
        capture_events,
        output_events,
        retired,
        capture_thread: None,
        output_thread: None,
    };

    assert!(engine.apply(LiveChange::LfeGain(-40.0)).is_ok());
    assert!(engine.apply(LiveChange::LfeGain(20.0)).is_ok());
    for invalid in [-40.1, 20.1, f32::NEG_INFINITY, f32::INFINITY, f32::NAN] {
        assert_eq!(engine.apply(LiveChange::LfeGain(invalid)), Err(Rejected));
    }
}

#[test]
fn pi_controller_clamps_and_integrates() {
    let mut controller = PiController::new();
    assert_eq!(controller.update(100.0, 0.01, 0.0), PI_LIMIT_PPM);
    let mut controller = PiController::new();
    let first = controller.update(0.5, 0.01, 0.0);
    let later = (0..100).fold(first, |_, _| controller.update(0.5, 0.01, 0.0));
    assert!(later > first);
    // With feedforward the reference moves, and the integral waits.
    let mut controller = PiController::new();
    assert_eq!(controller.update(0.5, 0.01, 1_000.0), 1_000.0 + PI_KP * 0.5);
    assert_eq!(controller.integral, 0.0);
}

#[test]
fn a_primed_excess_moves_the_reference_without_teaching_the_integral() {
    // Priming left the trough 20 ms above a 20 ms setpoint. A trough that
    // follows the reference down is no error, so the integral learns nothing
    // and the correction is the slew alone; inside the integral band (12 ms)
    // the old controller learned the last block of such an excess as skew.
    for mode in [ClockMode::Trim, ClockMode::Asrc] {
        let drain = match mode {
            ClockMode::Trim => -PI_LIMIT_PPM / 2.0,
            ClockMode::Asrc => PI_LIMIT_PPM / 2.0,
        };
        let mut controller = TroughController::new();
        let mut windows = 0;
        let mut trough = 40.0;
        while trough > 20.0 {
            let (_, ppm) = controller.observe(trough, 0.25, 20.0, mode).unwrap();
            assert_eq!(ppm, drain, "{mode:?} at {trough} ms");
            assert_eq!(controller.pi.integral, 0.0, "{mode:?} at {trough} ms");
            trough = controller.reference_ms.unwrap();
            windows += 1;
        }
        // 20 ms at 1 ms per second in 0.25 s windows.
        assert_eq!(windows, 80, "{mode:?}");
        assert_eq!(
            controller.observe(20.0, 0.25, 20.0, mode),
            Some((20.0, 0.0))
        );
        // At the setpoint the controller is a plain PI: an error is skew.
        let (_, ppm) = controller.observe(21.0, 0.25, 20.0, mode).unwrap();
        assert!(
            ppm.abs() > 0.0 && ppm.abs() < PI_LIMIT_PPM / 2.0,
            "{mode:?}: {ppm} ppm"
        );
        assert_ne!(controller.pi.integral, 0.0, "{mode:?}");
        // A new priming restarts the reference but keeps the learned skew.
        let integral = controller.pi.integral;
        controller.restart_reference();
        let (_, ppm) = controller.observe(30.0, 0.25, 20.0, mode).unwrap();
        assert_eq!(controller.pi.integral, integral, "{mode:?}");
        assert_eq!(ppm, drain + PI_KI * integral, "{mode:?}");
        // A trough that lags the moving reference (it moves in whole blocks
        // when a period is as long as one) gets P, and the integral waits.
        let (_, ppm) = controller.observe(30.0, 0.25, 20.0, mode).unwrap();
        assert_eq!(controller.pi.integral, integral, "{mode:?}");
        let lag = match mode {
            ClockMode::Trim => -0.25,
            ClockMode::Asrc => 0.25,
        };
        assert_eq!(ppm, drain + PI_KP * lag + PI_KI * integral, "{mode:?}");
    }
}

#[test]
fn the_reference_follows_a_setpoint_only_once_it_holds() {
    for mode in [ClockMode::Trim, ClockMode::Asrc] {
        let fill = match mode {
            ClockMode::Trim => PI_LIMIT_PPM / 2.0,
            ClockMode::Asrc => -PI_LIMIT_PPM / 2.0,
        };
        let mut controller = TroughController::new();
        assert_eq!(
            controller.observe(20.0, 0.25, 20.0, mode),
            Some((20.0, 0.0))
        );
        // A late double delivery raises the setpoint for a window or two.
        for _ in 0..SETPOINT_SETTLE_WINDOWS - 1 {
            assert_eq!(
                controller.observe(20.0, 0.25, 30.0, mode),
                Some((20.0, 0.0))
            );
        }
        assert_eq!(
            controller.observe(20.0, 0.25, 20.0, mode),
            Some((20.0, 0.0))
        );
        assert_eq!(controller.reference_ms, Some(20.0));
        // One that holds is followed, at the slew rate.
        for _ in 0..SETPOINT_SETTLE_WINDOWS - 1 {
            assert_eq!(
                controller.observe(20.0, 0.25, 30.0, mode),
                Some((20.0, 0.0))
            );
        }
        let reference = controller.reference_ms.unwrap();
        let (_, ppm) = controller.observe(reference, 0.25, 30.0, mode).unwrap();
        assert_eq!(ppm, fill, "{mode:?}");
        assert_eq!(controller.reference_ms, Some(20.25), "{mode:?}");
    }
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "drives the real resampler for minutes of audio; run with --release (CI's release test jobs do)"
)]
fn clock_holds_the_trough_without_chasing_the_block_sawtooth() {
    // (block, period): PCM or MAT at a 3 ms exclusive period, E-AC-3 at
    // 3 ms, E-AC-3 at a 10 ms shared period, PCM and E-AC-3 at a 20 ms
    // exclusive period.
    for (block, period) in [
        (480, 144),
        (1_536, 144),
        (1_536, 480),
        (480, 960),
        (1_536, 960),
    ] {
        for trim in [false, true] {
            let run = simulate_clock(0.0, block, period, trim, 120.0);
            let label = format!("block {block} period {period} trim {trim}");
            assert_eq!(run.underruns, 0, "{label}");
            assert_eq!(run.clamped, 0, "{label}");
            // The trough read at 10 ms periods steps by up to 2 ms as the
            // 32 ms blocks fall into different periods, hence 150 ppm.
            assert!(
                run.ppm.0 >= -150.0 && run.ppm.1 <= 150.0,
                "{label}: correction {:?} ppm with no skew",
                run.ppm
            );
        }
    }
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "drives the real resampler for minutes of audio; run with --release (CI's release test jobs do)"
)]
fn clock_follows_three_hundred_ppm_of_skew_for_asrc_and_trim() {
    for (block, period) in [(480, 144), (1_536, 144), (1_536, 480)] {
        for trim in [false, true] {
            for skew in [-300.0, 300.0] {
                let run = simulate_clock(skew, block, period, trim, 300.0);
                let label = format!("block {block} period {period} trim {trim} skew {skew}");
                let setpoint = simulated_setpoint_ms(block, period);
                assert_eq!(run.underruns, 0, "{label}");
                assert_eq!(run.clamped, 0, "{label}");
                assert!(
                    run.trough.0 >= setpoint - 2.0 && run.trough.1 <= setpoint + 2.0,
                    "{label}: trough {:?} ms",
                    run.trough
                );
                let expected = if trim { -skew } else { skew };
                assert!(
                    (run.final_ppm - expected).abs() <= 100.0,
                    "{label}: correction {} ppm",
                    run.final_ppm
                );
            }
        }
    }
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "drives the real resampler for minutes of audio; run with --release (CI's release test jobs do)"
)]
fn audible_priming_excess_drains_without_winding_up_the_clock() {
    // Priming keeps every audible frame (ADR 0028), so a source that
    // starts with a backlog leaves the trough tens of milliseconds above
    // target. The controller drains that latency at up to its limit; if
    // the integral learned it as clock skew, it kept draining past the
    // target, the output ran dry, primed again with the excess, and the
    // cycle repeated every ~20 s (the CI soaks' underruns).
    // Periods shorter than a block, at any skew the simulation covers; and
    // every period at no skew (the CI soaks: a QPC-paced dump into a
    // QPC-clocked virtual cable). Longer periods under skew starve now
    // and then for another reason (clock_with_a_long_period_stays_bounded).
    let short = [(480, 144), (1_536, 144), (1_536, 480)].map(|case| (case, [-300.0, 0.0, 300.0]));
    let long = [(480, 480), (480, 960), (1_536, 960)].map(|case| (case, [0.0; 3]));
    for ((block, period), skews) in short.into_iter().chain(long) {
        for skew in skews {
            for backlog in [30.0, 60.0] {
                for trim in [false, true] {
                    let run = simulate_clock_from(skew, block, period, trim, 300.0, backlog);
                    let label = format!(
                        "block {block} period {period} skew {skew} backlog {backlog} trim {trim}"
                    );
                    let setpoint = simulated_setpoint_ms(block, period);
                    assert_eq!(run.underruns, 0, "{label}");
                    assert_eq!(run.late_clamped, 0, "{label}: still at the limit");
                    assert!(
                        run.trough.0 >= setpoint - 3.0 && run.trough.1 <= setpoint + 3.0,
                        "{label}: trough {:?} ms",
                        run.trough
                    );
                }
            }
        }
    }
}

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "drives the real resampler for minutes of audio; run with --release (CI's release test jobs do)"
)]
fn a_period_as_long_as_a_block_survives_phase_slips_and_late_blocks() {
    // A 10 ms shared period with 10 ms PCM blocks, and a 20 ms exclusive
    // period with PCM or E-AC-3. While every period gets its block the
    // trough is flat; as the arrival phase drifts past the period boundary
    // one period gets none, and so does a period whose block lands late.
    // Held at the bare target the ring was one block from empty there and
    // starved about once per phase slip (33 s at 300 ppm for 10 ms
    // blocks); the setpoint keeps that block as margin.
    for (block, period) in [(480, 480), (480, 960), (1_536, 960)] {
        for skew in [-300.0, -100.0, 0.0, 100.0, 300.0] {
            for late in [0.0, 0.02] {
                for trim in [false, true] {
                    let run = simulate_clock_with(skew, block, period, trim, 300.0, 0.0, late);
                    let label = format!(
                        "block {block} period {period} skew {skew} late {late} trim {trim}"
                    );
                    assert_eq!(run.underruns, 0, "{label}");
                    assert_eq!(run.late_clamped, 0, "{label}: still at the limit");
                }
            }
        }
    }
}

#[test]
fn trough_setpoint_keeps_one_late_delivery_beyond_the_target() {
    // The smaller of the take and the delivery: a 3 ms period loses one
    // take to a late 10 ms block, a 20 ms period loses one 10 ms block.
    assert_eq!(trough_setpoint_frames(480.0, 144.0, 480.0, 9_600.0), 624.0);
    assert_eq!(trough_setpoint_frames(480.0, 960.0, 480.0, 9_600.0), 960.0);
    assert_eq!(
        trough_setpoint_frames(480.0, 480.0, 1_536.0, 9_600.0),
        960.0
    );
    // Never above what leaves room for a delivery after the take.
    assert_eq!(
        trough_setpoint_frames(480.0, 960.0, 1_536.0, 2_976.0),
        480.0
    );
    assert_eq!(trough_setpoint_frames(480.0, 960.0, 1_536.0, 1_000.0), 0.0);
}

#[test]
fn the_default_ring_never_caps_the_setpoint_for_a_dd_plus_delivery() {
    // A DD+ sync frame lands as up to four mixer blocks (40 ms). The
    // setpoint keeps one late delivery's worth above the target only while
    // the ring has room for that delivery after the take; at 80 ms the
    // 20 ms period was capped to a 20 ms trough and underran.
    let config = EngineConfig::default();
    let capacity = capacity_frames(config.capacity_ms, crate::MIX_RATE) as f64;
    let target = f64::from(config.target_ms) * 48.0;
    let delivery = 4.0 * crate::MIX_FRAMES as f64;
    for take in [144.0, 480.0, 960.0, 1_920.0, 4_096.0] {
        assert_eq!(
            trough_setpoint_frames(target, take, delivery, capacity),
            target + f64::min(take, delivery),
            "{take}-frame period"
        );
    }
}

/// Drives the real output state (ring, priming, ASRC) with whole blocks
/// landing in step with the periods, as a paced source does, and returns
/// the underruns and the periods that carried audio.
fn play_output(block: usize, period: usize, periods: usize) -> (u64, usize) {
    let rate = 48_000;
    let shared = Arc::new(Shared::new());
    shared.scene_rate.store(rate, Ordering::Release);
    shared
        .clock_mode
        .store(ClockMode::Asrc.as_u32(), Ordering::Release);
    shared.capture_started.store(true, Ordering::Release);
    let (mut audio_in, audio) = RingBuffer::<f32>::new(80 * 48 * 2);
    let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
    let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
    let mut output = OutputState::new(
        CrossingConsumer::from_parts(audio, timing, 80 * 48),
        rate,
        period,
        10.0,
        DEFAULT_QPC_FREQUENCY,
        events,
        Arc::clone(&shared),
        OutputLayout::STEREO,
        80 * 48,
    );
    let mut buffer = vec![0.0; period * 2];
    let (mut arriving, mut stream_frame, mut played) = (0, 0_u64, 0);
    for index in 0..periods {
        arriving += period;
        while arriving >= block {
            arriving -= block;
            for _ in 0..block * 2 {
                audio_in.push(0.25).expect("the ring has room");
            }
            let _ = timing_in.push(TimingMarker {
                stream_frame,
                frames: block as u32,
                source_qpc: 0,
                layout: OutputLayout::STEREO,
            });
            stream_frame += block as u64;
        }
        output.fill(&mut buffer);
        played += usize::from(output.period_had_audio);
        output.finish_period(index as u64 * 200_000);
    }
    (shared.underruns.load(Ordering::Acquire), played)
}

#[test]
fn the_end_of_a_source_drains_without_underruns() {
    // DD+ blocks against a 10 ms shared period: when the source ends, the
    // frames left in the ring are fewer than the ASRC's next take. They
    // must still go out, the ring must drain, and no period after the end
    // may count as an underrun.
    let rate = 48_000;
    let (block, period) = (1_536, 480);
    let shared = Arc::new(Shared::new());
    shared.scene_rate.store(rate, Ordering::Release);
    shared
        .clock_mode
        .store(ClockMode::Asrc.as_u32(), Ordering::Release);
    shared.capture_started.store(true, Ordering::Release);
    let (mut audio_in, audio) = RingBuffer::<f32>::new(80 * 48 * 2);
    let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
    let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
    let mut output = OutputState::new(
        CrossingConsumer::from_parts(audio, timing, 80 * 48),
        rate,
        period,
        10.0,
        DEFAULT_QPC_FREQUENCY,
        events,
        Arc::clone(&shared),
        OutputLayout::STEREO,
        80 * 48,
    );
    let mut buffer = vec![0.0; period * 2];
    let (mut arriving, mut stream_frame) = (0, 0_u64);
    for index in 0..103 {
        arriving += period;
        while arriving >= block {
            arriving -= block;
            for _ in 0..block * 2 {
                audio_in.push(0.25).expect("the ring has room");
            }
            let _ = timing_in.push(TimingMarker {
                stream_frame,
                frames: block as u32,
                source_qpc: 0,
                layout: OutputLayout::STEREO,
            });
            stream_frame += block as u64;
        }
        output.fill(&mut buffer);
        output.finish_period(index as u64 * 100_000);
    }
    assert_eq!(shared.underruns.load(Ordering::Acquire), 0);
    assert!(
        output.crossing.audio.slots() > 0,
        "the ring holds the last frames when the source ends"
    );
    shared.capture_done.store(true, Ordering::Release);
    let mut periods = 0;
    while !output.drained() {
        periods += 1;
        assert!(periods <= 20, "the ring did not drain");
        output.fill(&mut buffer);
        output.finish_period(0);
    }
    assert_eq!(shared.underruns.load(Ordering::Acquire), 0);
}

#[test]
fn a_period_longer_than_a_block_plays_after_priming() {
    // 20 and 40 ms exclusive periods against 10 ms PCM blocks, and 20 ms
    // against 32 ms E-AC-3 blocks: priming must leave target plus the
    // ASRC's whole first take (the period and the filter's lookahead), or
    // the first render comes up short and primes again forever.
    for (block, period) in [(480, 960), (480, 1_920), (1_536, 960), (480, 144)] {
        let (underruns, played) = play_output(block, period, 400);
        assert_eq!(underruns, 0, "block {block} period {period}");
        assert!(
            played >= 390,
            "block {block} period {period}: {played} of 400 periods carried audio"
        );
    }
}

#[test]
fn asrc_keeps_channels_apart_with_twelve_channels() {
    let channels = 12;
    let frames = 960;
    let (mut producer, mut audio) = RingBuffer::<f32>::new(frames * channels);
    for _ in 0..frames {
        for channel in 0..channels {
            producer.push((channel + 1) as f32).unwrap();
        }
    }
    let mut asrc = AsrcClock::new(441, 44_100, channels);
    asrc.reset(48_000, 44_100);
    asrc.input_limit = frames;
    let mut output = vec![0.0; 441 * channels];
    let mut stream_frame = 0;
    let rendered = asrc.render(&mut audio, &mut output, 48_000, 44_100, &mut stream_frame);
    assert_eq!(rendered.output_frames, 441);
    for frame in output.chunks_exact(channels) {
        let reference = frame[0];
        for (channel, sample) in frame.iter().enumerate() {
            assert!(
                (*sample - reference * (channel + 1) as f32).abs() < 1.0e-4,
                "channel {channel} contained {sample} for reference {reference}"
            );
        }
    }
}

#[test]
fn multichannel_blocks_enter_the_crossing_ring_with_their_layout() {
    let shared = Arc::new(Shared::new());
    shared.dropped_frames.store(7, Ordering::Release);
    let (mut crossing, mut output) = crossing::channel(16, 4);
    crossing.stream_frame = 11;
    let (event_producer, _event_out) = RingBuffer::<RtEvent>::new(4);
    let mut events = EventWriter::new(event_producer, Arc::clone(&shared));
    assert_eq!(shared.scene_rate.load(Ordering::Acquire), 0);
    let samples = [
        1.0, 2.0, 3.0, 11.0, 12.0, 13.0, 21.0, 22.0, 23.0, 31.0, 32.0, 33.0,
    ];
    handle_chain_event(
        ChainEvent::Block(crate::BlockOut {
            epoch: 1,
            rate: 48_000,
            source_qpc: 23,
            channels: 4,
            channel_mask: 0x33,
            frames: 3,
            samples: &samples,
            silent: false,
            idle: false,
        }),
        &mut crossing,
        &mut events,
        &shared,
    );
    let interleaved = (0..12)
        .map(|_| output.audio.pop().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        interleaved,
        vec![
            1.0, 11.0, 21.0, 31.0, 2.0, 12.0, 22.0, 32.0, 3.0, 13.0, 23.0, 33.0
        ]
    );
    let marker = output.next_marker().unwrap();
    assert_eq!(marker.stream_frame, 11);
    assert_eq!(marker.frames, 3);
    assert_eq!(marker.layout.channels, 4);
    assert_eq!(marker.layout.channel_mask, 0x33);
    assert_eq!(crossing.stream_frame(), 14);
    assert_eq!(shared.scene_rate.load(Ordering::Acquire), crate::MIX_RATE);
    assert_eq!(shared.blocks.load(Ordering::Acquire), 1);
    assert_eq!(shared.status().dropped_frames, 7);
}

#[test]
fn idle_blocks_are_dropped_silently_when_the_ring_is_full() {
    let shared = Shared::new();
    let (mut crossing, _output) = crossing::channel(0, 2);
    let (events, _events_out) = RingBuffer::<RtEvent>::new(1);
    let mut events = EventWriter::new(events, Arc::new(Shared::new()));
    crossing.push(
        crate::chain::BlockOut {
            epoch: 1,
            rate: 48_000,
            source_qpc: 1,
            channels: 2,
            channel_mask: 3,
            frames: 1,
            samples: &[0.0, 0.0],
            silent: true,
            idle: true,
        },
        &mut events,
        &shared,
    );
    assert_eq!(shared.output_overflow_frames.load(Ordering::Acquire), 0);
    assert_eq!(shared.idle_blocks.load(Ordering::Acquire), 0);
    assert_eq!(crossing.stream_frame(), 0);
}

#[test]
fn a_full_ring_counts_overflowed_frames() {
    let shared = Shared::new();
    let (mut crossing, _output) = crossing::channel(0, 2);
    let (events, mut events_out) = RingBuffer::<RtEvent>::new(1);
    let mut events = EventWriter::new(events, Arc::new(Shared::new()));
    crossing.push(
        crate::chain::BlockOut {
            epoch: 1,
            rate: 48_000,
            source_qpc: 1,
            channels: 2,
            channel_mask: 3,
            frames: 2,
            samples: &[0.25; 4],
            silent: false,
            idle: false,
        },
        &mut events,
        &shared,
    );
    assert_eq!(shared.output_overflow_frames.load(Ordering::Acquire), 2);
    assert!(matches!(
        events_out.pop(),
        Ok(RtEvent::OutputOverflow { frames: 2 })
    ));
    assert_eq!(crossing.stream_frame(), 0);
}

#[test]
fn an_overflow_burst_reports_its_frames_without_flooding_the_event_queue() {
    let shared = Shared::new();
    let (mut crossing, mut output) = crossing::channel(2, 2);
    let (events, mut events_out) = RingBuffer::<RtEvent>::new(8);
    let event_shared = Arc::new(Shared::new());
    let mut events = EventWriter::new(events, Arc::clone(&event_shared));
    let block = || crate::chain::BlockOut {
        epoch: 1,
        rate: 48_000,
        source_qpc: 1,
        channels: 2,
        channel_mask: 3,
        frames: 2,
        samples: &[0.25; 4],
        silent: false,
        idle: false,
    };
    crossing.push(block(), &mut events, &shared);
    for _ in 0..1_000 {
        crossing.push(block(), &mut events, &shared);
    }
    for _ in 0..4 {
        output.audio.pop().unwrap();
    }
    let _ = output.next_marker();
    crossing.push(block(), &mut events, &shared);
    let mut reported = Vec::new();
    while let Ok(RtEvent::OutputOverflow { frames }) = events_out.pop() {
        reported.push(frames);
    }
    assert_eq!(event_shared.dropped_events.load(Ordering::Acquire), 0);
    assert_eq!(reported.first(), Some(&2), "{reported:?}");
    assert!(reported.len() <= 3, "{reported:?}");
    assert_eq!(reported.iter().sum::<u64>(), 2_000);
    assert_eq!(shared.output_overflow_frames.load(Ordering::Acquire), 2_000);
}

#[test]
fn priming_never_discards_sound() {
    fn discarded(samples: &[f32], frames: usize) -> (u64, Vec<f32>) {
        let shared = Arc::new(Shared::new());
        shared.scene_rate.store(48_000, Ordering::Release);
        let (mut producer, audio) = RingBuffer::<f32>::new(samples.len().max(2));
        for &sample in samples {
            producer.push(sample).unwrap();
        }
        let (_timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
        let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
        let mut output = OutputState::new(
            CrossingConsumer::from_parts(audio, timing, samples.len() / 2),
            48_000,
            48,
            10.0,
            DEFAULT_QPC_FREQUENCY,
            events,
            shared,
            OutputLayout::STEREO,
            samples.len() / 2,
        );
        output.discard(frames);
        let remaining = output.crossing.audio.slots();
        let mut samples = vec![0.0; remaining];
        let _ = output.crossing.audio.pop_partial_slice(&mut samples);
        (output.crossing.stream_frame, samples)
    }

    let sound = [0.25, -0.25, 0.5, -0.5];
    let (frames, remaining) = discarded(&sound, 2);
    assert_eq!(frames, 0);
    assert_eq!(remaining, sound);

    let samples = [0.0, 0.0, 0.0, 0.0, 0.25, -0.25, 0.5, -0.5];
    let (frames, remaining) = discarded(&samples, 4);
    assert_eq!(frames, 2);
    assert_eq!(remaining, samples[4..]);
    assert!(
        samples[..frames as usize * 2]
            .iter()
            .all(|sample| *sample == 0.0)
    );
}

#[test]
fn latency_writer_matches_previous_percentiles_and_resets_by_generation() {
    let shared = Shared::new();
    let mut writer = LatencyWriter::new(&shared);
    for value in 1..=100 {
        writer.push(value as f32, &shared);
    }
    let status = shared.status();
    assert_eq!(status.latency.samples, 100);
    assert_eq!(status.latency.p50_ms, 51.0);
    assert_eq!(status.latency.p99_ms, 99.0);

    writer.reset(&shared);
    writer.push(7.0, &shared);
    assert_eq!(
        shared.status().latency,
        LatencyStats {
            p50_ms: 7.0,
            p99_ms: 7.0,
            samples: 1,
        }
    );
}

#[test]
fn latency_slot_sequences_do_not_alias_across_the_u32_wrap() {
    let shared = Shared::new();
    let mut writer = LatencyWriter::new(&shared);
    writer.next = u64::from(u32::MAX) - 511;
    shared.latency_next.store(writer.next, Ordering::Release);
    for _ in 0..LATENCY_SAMPLES {
        writer.push(7.0, &shared);
    }

    assert_eq!(shared.latency().samples, LATENCY_SAMPLES as u32);
    assert_eq!(shared.latency().p50_ms, 7.0);
    assert_eq!(shared.latency().p99_ms, 7.0);
    let sequence = u64::from(u32::MAX) + 511;
    let index = sequence as usize % LATENCY_SAMPLES;
    assert_eq!(
        shared.latency_sequences[index].load(Ordering::Acquire),
        sequence.wrapping_add(1)
    );
}

#[test]
fn latency_status_never_observes_a_slot_while_its_value_is_being_published() {
    let shared = Arc::new(Shared::new());
    let writer_shared = Arc::clone(&shared);
    let writer = std::thread::spawn(move || {
        let mut writer = LatencyWriter::new(&writer_shared);
        for value in 1..=100_000_u32 {
            writer.push(value as f32, &writer_shared);
        }
    });
    while !writer.is_finished() {
        let latency = shared.latency();
        if latency.samples != 0 {
            assert!(latency.p50_ms >= 1.0, "{latency:?}");
            assert!(latency.p99_ms >= latency.p50_ms, "{latency:?}");
        }
    }
    writer.join().unwrap();
    assert_eq!(shared.latency().samples, LATENCY_SAMPLES as u32);
}

#[test]
fn latency_status_never_mixes_samples_across_concurrent_resets() {
    let shared = Arc::new(Shared::new());
    let writer_shared = Arc::clone(&shared);
    let writer = std::thread::spawn(move || {
        let mut writer = LatencyWriter::new(&writer_shared);
        for generation in 1..=2_000_u32 {
            writer.reset(&writer_shared);
            let value = generation as f32;
            for _ in 0..32 {
                writer.push(value, &writer_shared);
            }
        }
    });
    while !writer.is_finished() {
        let latency = shared.latency();
        if latency.samples != 0 {
            assert_eq!(latency.p50_ms, latency.p99_ms, "{latency:?}");
        }
    }
    writer.join().unwrap();
}

#[test]
fn a_short_terminal_asrc_take_fades_inside_the_rendered_period_and_drains_silently() {
    let shared = Arc::new(Shared::new());
    shared.scene_rate.store(48_000, Ordering::Release);
    shared
        .clock_mode
        .store(ClockMode::Asrc.as_u32(), Ordering::Release);
    shared.capture_started.store(true, Ordering::Release);
    let period_frames = 441;
    let real_frames = 8;
    let new_layout = OutputLayout {
        channels: 12,
        channel_mask: 0,
    };
    let (mut audio_in, audio) = RingBuffer::<f32>::new(real_frames * 2 + 12);
    for frame in 0..real_frames {
        let sample = (2.0 * std::f32::consts::PI * 997.0 * frame as f32 / 48_000.0).sin();
        audio_in.push(sample).unwrap();
        audio_in.push(sample).unwrap();
    }
    for _ in 0..12 {
        audio_in.push(0.0).unwrap();
    }
    let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
    timing_in
        .push(TimingMarker {
            stream_frame: 0,
            frames: real_frames as u32,
            source_qpc: 0,
            layout: OutputLayout::STEREO,
        })
        .unwrap();
    timing_in
        .push(TimingMarker {
            stream_frame: real_frames as u64,
            frames: 1,
            source_qpc: real_frames as u64,
            layout: new_layout,
        })
        .unwrap();
    let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
    let mut output = OutputState::new(
        CrossingConsumer::from_parts(audio, timing, 80 * 48),
        44_100,
        period_frames,
        0.0,
        DEFAULT_QPC_FREQUENCY,
        events,
        shared,
        OutputLayout::STEREO,
        period_frames,
    );
    output.asrc.reset(48_000, 44_100);
    output.priming = false;
    output.scene_started = true;
    output.fade_in_remaining = 0;
    let mut period = vec![0.0; period_frames * 2];
    output.fill(&mut period);

    let predicted_signal_end = 7 + output.asrc.output_delay();
    assert!(predicted_signal_end < period_frames);
    assert_eq!(output.period_signal_end, predicted_signal_end);
    let left: Vec<_> = period
        .as_chunks::<2>()
        .0
        .iter()
        .map(|frame| frame[0])
        .collect();
    let first_zero = left[..predicted_signal_end]
        .iter()
        .rposition(|sample| *sample == 0.0)
        .expect("the in-period fade did not reach exact zero");
    assert!(first_zero >= predicted_signal_end.saturating_sub(2));
    assert!(
        left[predicted_signal_end..]
            .iter()
            .all(|sample| *sample == 0.0)
    );
    let tone_step = 2.0 * (std::f32::consts::PI * 997.0 / 44_100.0).sin();
    let max_step = left[..predicted_signal_end]
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0_f32, f32::max);
    assert!(
        max_step <= tone_step + 0.05,
        "short terminal fade step {max_step} exceeded tone bound {}",
        tone_step + 0.05
    );
    assert_eq!(output.pending_layout_change(), Some(new_layout));

    let mut drain = vec![1.0; period_frames * 2];
    output.asrc.flush(&mut drain);
    output.apply_asrc_drain_fade(&mut drain);
    assert!(drain.iter().all(|sample| *sample == 0.0));
}

/// Records every period the output writes after the test's own renders.
struct TailSink {
    periods: Vec<Vec<f32>>,
    period_frames: usize,
}

impl OutputSink for TailSink {
    fn format(&self) -> ssr_types::OutputFormat {
        ssr_types::OutputFormat::exclusive(44_100, 2, 0, self.period_frames)
    }

    fn next_period(&mut self, fill: &mut dyn FnMut(&mut [f32])) -> Result<u64, OutputError> {
        let mut period = vec![0.0; self.period_frames * 2];
        fill(&mut period);
        self.periods.push(period);
        Ok(0)
    }
}

/// A 48 kHz scene into a 44.1 kHz ASRC whose ring holds exactly one take
/// of a 997 Hz tone, rendered with capture still running.
fn asrc_after_one_whole_take() -> (OutputState, Arc<Shared>, Vec<f32>) {
    asrc_after_one_whole_take_with_period(441)
}

fn asrc_after_one_whole_take_with_period(
    period_frames: usize,
) -> (OutputState, Arc<Shared>, Vec<f32>) {
    let (output, shared, period, _, _) = asrc_after_one_whole_take_keeping_input(period_frames);
    (output, shared, period)
}

/// As above, with the ring's producers kept so more input can follow.
fn asrc_after_one_whole_take_keeping_input(
    period_frames: usize,
) -> (
    OutputState,
    Arc<Shared>,
    Vec<f32>,
    rtrb::Producer<f32>,
    rtrb::Producer<TimingMarker>,
) {
    let shared = Arc::new(Shared::new());
    shared.scene_rate.store(48_000, Ordering::Release);
    shared
        .clock_mode
        .store(ClockMode::Asrc.as_u32(), Ordering::Release);
    shared.capture_started.store(true, Ordering::Release);
    let (mut audio_in, audio) = RingBuffer::<f32>::new(4_096 * 2);
    let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
    let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
    let mut output = OutputState::new(
        CrossingConsumer::from_parts(audio, timing, 80 * 48),
        44_100,
        period_frames,
        0.0,
        DEFAULT_QPC_FREQUENCY,
        events,
        Arc::clone(&shared),
        OutputLayout::STEREO,
        4_096,
    );
    output.asrc.reset(48_000, 44_100);
    output.priming = false;
    output.scene_started = true;
    output.fade_in_remaining = 0;
    let take = output.asrc.frames_needed();
    for frame in 0..take {
        let sample = (2.0 * std::f32::consts::PI * 997.0 * frame as f32 / 48_000.0).sin();
        audio_in.push(sample).unwrap();
        audio_in.push(sample).unwrap();
    }
    timing_in
        .push(TimingMarker {
            stream_frame: 0,
            frames: take as u32,
            source_qpc: 0,
            layout: OutputLayout::STEREO,
        })
        .unwrap();
    let mut period = vec![0.0; period_frames * 2];
    output.fill(&mut period);
    assert_eq!(
        output.crossing.audio.slots(),
        0,
        "the take did not empty the ring"
    );
    assert_eq!(shared.underruns.load(Ordering::Acquire), 0);
    assert_eq!(output.tail_periods, 0, "capture had not ended yet");
    (output, shared, period, audio_in, timing_in)
}

fn left(samples: &[f32]) -> Vec<f32> {
    samples
        .as_chunks::<2>()
        .0
        .iter()
        .map(|frame| frame[0])
        .collect()
}

fn max_step(frames: &[f32]) -> f32 {
    frames
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0_f32, f32::max)
}

#[test]
fn the_end_of_capture_seen_after_the_last_take_drains_the_tail_right_after_it() {
    let (mut output, shared, period) = asrc_after_one_whole_take();
    let delay = output.asrc.output_delay();
    assert_eq!(delay, 117);
    assert_eq!(boundary_fade_frames(44_100), 88);

    // The end of capture becomes visible only now, after the take that
    // emptied the ring went out as an ordinary period.
    shared.capture_done.store(true, Ordering::Release);
    assert!(output.ended());
    let mut sink = TailSink {
        periods: Vec::new(),
        period_frames: 441,
    };
    output.write_terminal_tail(&mut sink).unwrap();

    // One drain period follows the rendered one: no empty period, no
    // underrun, and the tail is written once.
    assert_eq!(sink.periods.len(), 1);
    assert_eq!(shared.underruns.load(Ordering::Acquire), 0);
    assert_eq!(output.tail_periods, 0);
    assert!(output.write_terminal_tail(&mut sink).is_ok());
    assert_eq!(sink.periods.len(), 1, "the tail was written twice");

    // The whole take was real input, so the real signal runs `delay`
    // frames into the drain: full level up to the fade, which takes the
    // last 88 of them to exact zero.
    let mut frames = left(&period);
    frames.extend(left(&sink.periods[0]));
    let signal_end = 441 + delay;
    let fade_start = signal_end - 88;
    let pre_fade_peak = frames[441..fade_start]
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    assert!(
        pre_fade_peak > 0.5,
        "the drain lost the tail before its fade: peak {pre_fade_peak}"
    );
    assert_eq!(frames[signal_end - 1], 0.0);
    assert!(frames[signal_end - 2] != 0.0);
    assert!(frames[signal_end..].iter().all(|sample| *sample == 0.0));
    let tone_step = 2.0 * (std::f32::consts::PI * 997.0 / 44_100.0).sin();
    let step = max_step(&frames[220..signal_end]);
    assert!(
        step <= tone_step + 0.05,
        "the tail step {step} exceeded the tone bound {}",
        tone_step + 0.05
    );
}

#[test]
fn the_end_of_capture_seen_while_the_device_asks_for_a_period_puts_the_tail_in_it() {
    let (mut output, shared, period) = asrc_after_one_whole_take();
    let delay = output.asrc.output_delay();

    // The output thread is waiting inside the sink when capture ends, so
    // the end is first seen by the period the device asks for next.
    shared.capture_done.store(true, Ordering::Release);
    let mut next = vec![0.0; 441 * 2];
    output.fill(&mut next);

    assert_eq!(shared.underruns.load(Ordering::Acquire), 0);
    assert_eq!(output.tail_periods, 0);
    assert!(output.ended());
    let mut sink = TailSink {
        periods: Vec::new(),
        period_frames: 441,
    };
    output.write_terminal_tail(&mut sink).unwrap();
    assert!(sink.periods.is_empty(), "the tail was written twice");

    let mut frames = left(&period);
    frames.extend(left(&next));
    let signal_end = 441 + delay;
    let pre_fade_peak = frames[441..signal_end - 88]
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    assert!(
        pre_fade_peak > 0.5,
        "the period lost the tail before its fade: peak {pre_fade_peak}"
    );
    assert_eq!(frames[signal_end - 1], 0.0);
    assert!(frames[signal_end..].iter().all(|sample| *sample == 0.0));
    let tone_step = 2.0 * (std::f32::consts::PI * 997.0 / 44_100.0).sin();
    let step = max_step(&frames[220..signal_end]);
    assert!(
        step <= tone_step + 0.05,
        "the tail step {step} exceeded the tone bound {}",
        tone_step + 0.05
    );
}

#[test]
fn a_layout_change_seen_while_the_device_asks_for_a_period_puts_the_old_tail_in_it() {
    let (mut output, shared, period, mut audio_in, mut timing_in) =
        asrc_after_one_whole_take_keeping_input(441);
    let delay = output.asrc.output_delay();
    let new_layout = OutputLayout {
        channels: 12,
        channel_mask: 0,
    };
    // While the device waits, capture delivers a block in a new layout.
    timing_in
        .push(TimingMarker {
            stream_frame: output.crossing.stream_frame,
            frames: 1,
            source_qpc: output.crossing.stream_frame,
            layout: new_layout,
        })
        .unwrap();
    for _ in 0..12 {
        audio_in.push(0.25).unwrap();
    }
    let mut next = vec![0.0; 441 * 2];
    output.fill(&mut next);

    assert_eq!(shared.underruns.load(Ordering::Acquire), 0);
    assert_eq!(output.pending_layout_change(), Some(new_layout));
    assert_eq!(output.tail_periods, 0);
    assert!(!output.begin_reopen_tail(), "the reopen owes a second tail");
    let mut frames = left(&period);
    frames.extend(left(&next));
    let signal_end = 441 + delay;
    let pre_fade_peak = frames[441..signal_end - 88]
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    assert!(
        pre_fade_peak > 0.5,
        "the old layout's tail was not in the period: peak {pre_fade_peak}"
    );
    assert_eq!(frames[signal_end - 1], 0.0);
    assert!(frames[signal_end..].iter().all(|sample| *sample == 0.0));
    let tone_step = 2.0 * (std::f32::consts::PI * 997.0 / 44_100.0).sin();
    let step = max_step(&frames[220..signal_end]);
    assert!(
        step <= tone_step + 0.05,
        "the tail step {step} exceeded the tone bound {}",
        tone_step + 0.05
    );
}

#[test]
fn the_end_of_capture_during_a_reopen_drain_writes_only_the_periods_left() {
    let (mut output, shared, period) = asrc_after_one_whole_take_with_period(44);
    let delay = output.asrc.output_delay();
    assert_eq!(output.asrc.drain_periods(), 3);
    let mut sink = TailSink {
        periods: Vec::new(),
        period_frames: 44,
    };
    assert!(output.begin_reopen_tail());
    assert!(!output.write_reopen_tail_period(&mut sink).unwrap());

    shared.capture_done.store(true, Ordering::Release);
    assert!(output.ended());
    output.write_terminal_tail(&mut sink).unwrap();
    assert_eq!(sink.periods.len(), 3, "the end restarted the reopen drain");

    let mut frames = left(&period);
    for tail in &sink.periods {
        frames.extend(left(tail));
    }
    let signal_end = 44 + delay;
    assert_eq!(frames[signal_end - 1], 0.0);
    assert!(frames[signal_end - 2] != 0.0);
    assert!(frames[signal_end..].iter().all(|sample| *sample == 0.0));
    let tone_step = 2.0 * (std::f32::consts::PI * 997.0 / 44_100.0).sin();
    let step = max_step(&frames[..signal_end]);
    assert!(
        step <= tone_step + 0.05,
        "the interrupted tail stepped {step}, above the tone bound {}",
        tone_step + 0.05
    );
}

fn ended_output(mode: ClockMode, frames: usize) -> (OutputState, Arc<Shared>) {
    let shared = Arc::new(Shared::new());
    shared.scene_rate.store(48_000, Ordering::Release);
    shared.clock_mode.store(mode.as_u32(), Ordering::Release);
    shared.capture_started.store(true, Ordering::Release);
    shared.capture_input_ended.store(true, Ordering::Release);
    shared.capture_done.store(true, Ordering::Release);
    let (mut audio_in, audio) = RingBuffer::<f32>::new(4_096 * 2);
    let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
    let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
    let device_rate = if mode == ClockMode::Trim {
        48_000
    } else {
        44_100
    };
    let mut output = OutputState::new(
        CrossingConsumer::from_parts(audio, timing, 80 * 48),
        device_rate,
        480,
        0.0,
        DEFAULT_QPC_FREQUENCY,
        events,
        Arc::clone(&shared),
        OutputLayout::STEREO,
        4_096,
    );
    output.asrc.reset(48_000, device_rate);
    output.scene_started = true;
    output.fade_in_remaining = 0;
    for frame in 0..frames {
        let sample = (2.0 * std::f32::consts::PI * 997.0 * frame as f32 / 48_000.0).sin();
        audio_in.push(sample).unwrap();
        audio_in.push(sample).unwrap();
    }
    if frames != 0 {
        timing_in
            .push(TimingMarker {
                stream_frame: 0,
                frames: frames as u32,
                source_qpc: 0,
                layout: OutputLayout::STEREO,
            })
            .unwrap();
    }
    (output, shared)
}

#[test]
fn a_stream_that_ends_before_any_audio_owes_no_tail_and_no_underrun() {
    let (mut output, shared) = ended_output(ClockMode::Asrc, 0);
    assert!(output.asrc.drain_periods() != 0);
    let mut period = vec![1.0; 480 * 2];
    output.fill(&mut period);
    assert!(period.iter().all(|sample| *sample == 0.0));
    assert_eq!(shared.underruns.load(Ordering::Acquire), 0);
    assert_eq!(output.tail_periods, 0);
    assert!(!output.begin_reopen_tail(), "a reopen invented a tail");
    let mut sink = TailSink {
        periods: Vec::new(),
        period_frames: 480,
    };
    output.write_terminal_tail(&mut sink).unwrap();
    assert!(sink.periods.is_empty(), "the end invented a tail");
}

#[test]
fn a_trim_stream_that_ends_inside_a_period_plays_its_last_frames_without_an_underrun() {
    let (mut output, shared) = ended_output(ClockMode::Trim, 100);
    let mut period = vec![0.0; 480 * 2];
    output.fill(&mut period);
    assert_eq!(shared.underruns.load(Ordering::Acquire), 0);
    assert!(output.drained());
    // Every one of the 100 frames plays, the last 88 under the end fade.
    let fade = boundary_fade_frames(48_000);
    for (frame, played) in left(&period[..100 * 2]).into_iter().enumerate() {
        let source = (2.0 * std::f32::consts::PI * 997.0 * frame as f32 / 48_000.0).sin();
        let gain = match frame.checked_sub(100 - fade) {
            None => 1.0,
            Some(offset) => (fade - 1 - offset) as f32 / (fade - 1) as f32,
        };
        assert!(
            (played - source * gain).abs() <= 1.0e-6,
            "frame {frame} played {played}, expected {}",
            source * gain
        );
    }
    assert!(period[100 * 2..].iter().all(|sample| *sample == 0.0));
}

#[test]
fn a_reopen_gives_way_to_finishing_once_everything_is_rendered() {
    let layout = OutputLayout {
        channels: 12,
        channel_mask: 0,
    };
    assert!(matches!(
        end_before_reopen(OutputStep::ChangeLayout(layout), true),
        OutputStep::Continue
    ));
    assert!(matches!(
        end_before_reopen(OutputStep::GrowCadence(Duration::from_millis(20)), true),
        OutputStep::Continue
    ));
    assert!(matches!(
        end_before_reopen(OutputStep::Lost(OutputError::Removed), true),
        OutputStep::Lost(_)
    ));
    assert!(matches!(
        end_before_reopen(OutputStep::ChangeLayout(layout), false),
        OutputStep::ChangeLayout(_)
    ));
    assert!(matches!(
        end_before_reopen(OutputStep::GrowCadence(Duration::from_millis(20)), false),
        OutputStep::GrowCadence(_)
    ));
}

#[test]
fn an_underrun_before_the_end_of_capture_fades_the_tail_back_in() {
    let (mut output, shared, _) = asrc_after_one_whole_take();
    let delay = output.asrc.output_delay();
    let mut underrun = vec![0.0; 441 * 2];
    output.fill(&mut underrun);
    assert_eq!(shared.underruns.load(Ordering::Acquire), 1);
    let underrun = left(&underrun);
    assert_eq!(*underrun.last().unwrap(), 0.0);

    shared.capture_done.store(true, Ordering::Release);
    assert!(output.ended());
    let mut sink = TailSink {
        periods: Vec::new(),
        period_frames: 441,
    };
    output.write_terminal_tail(&mut sink).unwrap();
    assert_eq!(sink.periods.len(), 1);
    assert_eq!(shared.underruns.load(Ordering::Acquire), 1);

    // The tail comes back after silence, so it starts from zero and ramps
    // up instead of jumping to the level the take ended at.
    let drain = left(&sink.periods[0]);
    assert_eq!(drain[0], 0.0);
    let peak = drain
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    assert!(peak > 0.1, "the tail after the underrun was dropped");
    assert!(drain[delay..].iter().all(|sample| *sample == 0.0));
    let mut frames = underrun;
    frames.extend(drain);
    let tone_step = 2.0 * (std::f32::consts::PI * 997.0 / 44_100.0).sin();
    let step = max_step(&frames);
    assert!(
        step <= tone_step + 0.05,
        "the tail after the underrun stepped {step}, above the tone bound {}",
        tone_step + 0.05
    );
}

#[test]
fn layout_boundary_fades_out_the_old_sink_without_dropping_its_remainder() {
    let shared = Arc::new(Shared::new());
    shared.scene_rate.store(48_000, Ordering::Release);
    shared
        .clock_mode
        .store(ClockMode::Trim.as_u32(), Ordering::Release);
    shared.capture_started.store(true, Ordering::Release);
    let period_frames = 480;
    let old_frames = 48;
    let old_samples = old_frames * 2;
    let new_layout = OutputLayout {
        channels: 12,
        channel_mask: 0,
    };
    let (mut audio_in, audio) = RingBuffer::<f32>::new(old_samples + 12);
    for _ in 0..old_samples {
        audio_in.push(1.0).unwrap();
    }
    for _ in 0..12 {
        audio_in.push(0.5).unwrap();
    }
    let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
    timing_in
        .push(TimingMarker {
            stream_frame: 0,
            frames: old_frames as u32,
            source_qpc: 0,
            layout: OutputLayout::STEREO,
        })
        .unwrap();
    timing_in
        .push(TimingMarker {
            stream_frame: old_frames as u64,
            frames: 1,
            source_qpc: old_frames as u64,
            layout: new_layout,
        })
        .unwrap();
    let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
    let mut output = OutputState::new(
        CrossingConsumer::from_parts(audio, timing, 80 * 48),
        48_000,
        period_frames,
        0.0,
        DEFAULT_QPC_FREQUENCY,
        events,
        shared,
        OutputLayout::STEREO,
        period_frames,
    );
    output.asrc.reset(48_000, 48_000);
    output.priming = false;
    output.scene_started = true;
    output.fade_in_remaining = 0;
    let mut period = vec![0.0; period_frames * 2];
    output.fill(&mut period);

    assert!(
        period[old_samples..period_frames * 2]
            .iter()
            .all(|sample| *sample == 0.0)
    );
    let audible = &period[..old_samples];
    assert_eq!(audible[0], 1.0);
    assert_eq!(audible[1], 1.0);
    assert_eq!(audible[old_samples - 2], 0.0);
    assert_eq!(audible[old_samples - 1], 0.0);
    assert_eq!(output.pending_layout_change(), Some(new_layout));
    assert_eq!(output.crossing.audio.slots(), 12);
}

#[test]
fn retry_observes_a_return_to_the_current_layout() {
    let shared = Arc::new(Shared::new());
    let rejected = OutputLayout {
        channels: 6,
        channel_mask: 0x3f,
    };
    let (mut audio_in, audio) = RingBuffer::<f32>::new(32);
    for _ in 0..16 {
        audio_in.push(1.0).unwrap();
    }
    let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
    timing_in
        .push(TimingMarker {
            stream_frame: 0,
            frames: 2,
            source_qpc: 1,
            layout: rejected,
        })
        .unwrap();
    timing_in
        .push(TimingMarker {
            stream_frame: 2,
            frames: 2,
            source_qpc: 2,
            layout: OutputLayout::STEREO,
        })
        .unwrap();
    let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
    let mut output = OutputState::new(
        CrossingConsumer::from_parts(audio, timing, 80 * 48),
        48_000,
        480,
        0.0,
        DEFAULT_QPC_FREQUENCY,
        events,
        shared,
        OutputLayout::STEREO,
        480,
    );

    assert_eq!(output.pending_layout_change(), Some(rejected));
    assert_eq!(output.discard_pending_layout(rejected), 2);
    assert_eq!(output.layout_at_boundary(), Some(OutputLayout::STEREO));
    assert_eq!(output.pending_layout_change(), None);
}

#[test]
fn partially_consumed_marker_is_discarded_from_its_remaining_frame() {
    let shared = Arc::new(Shared::new());
    shared.scene_rate.store(48_000, Ordering::Release);
    shared
        .clock_mode
        .store(ClockMode::Trim.as_u32(), Ordering::Release);
    let rejected = OutputLayout::STEREO;
    let newer = OutputLayout {
        channels: 6,
        channel_mask: 0x3f,
    };
    let (mut audio_in, audio) = RingBuffer::<f32>::new(2_000);
    for _ in 0..(480 * 2 + 6) {
        audio_in.push(1.0).unwrap();
    }
    let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
    timing_in
        .push(TimingMarker {
            stream_frame: 0,
            frames: 480,
            source_qpc: 1,
            layout: rejected,
        })
        .unwrap();
    timing_in
        .push(TimingMarker {
            stream_frame: 480,
            frames: 1,
            source_qpc: 2,
            layout: newer,
        })
        .unwrap();
    let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
    let mut output = OutputState::new(
        CrossingConsumer::from_parts(audio, timing, 80 * 48),
        48_000,
        144,
        0.0,
        DEFAULT_QPC_FREQUENCY,
        events,
        shared,
        rejected,
        480,
    );
    output.strategy_mode = ClockMode::Trim;
    output.asrc.reset(48_000, 48_000);
    output.priming = false;
    output.scene_started = true;
    output.fade_in_remaining = 0;
    let mut period = vec![0.0; 144 * 2];
    output.fill(&mut period);
    assert_eq!(output.crossing.stream_frame, 144);
    output.finish_period(0);

    assert_eq!(output.discard_pending_layout(rejected), 336);
    assert_eq!(output.crossing.stream_frame, 480);
    assert_eq!(output.pending_layout_change(), Some(newer));
    assert_eq!(output.crossing.audio.slots(), 6);
}

#[test]
fn rejected_layout_discards_only_that_layout_and_counts_overflow() {
    let shared = Arc::new(Shared::new());
    shared.scene_rate.store(48_000, Ordering::Release);
    let rejected = OutputLayout {
        channels: 2,
        channel_mask: 0x3,
    };
    let newer = OutputLayout {
        channels: 2,
        channel_mask: 0,
    };
    let (mut audio_in, audio) = RingBuffer::<f32>::new(16);
    for sample in 0..12 {
        audio_in.push(sample as f32).unwrap();
    }
    let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
    timing_in
        .push(TimingMarker {
            stream_frame: 0,
            frames: 4,
            source_qpc: 1,
            layout: rejected,
        })
        .unwrap();
    timing_in
        .push(TimingMarker {
            stream_frame: 4,
            frames: 2,
            source_qpc: 2,
            layout: newer,
        })
        .unwrap();
    let (events, mut events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
    let mut output = OutputState::new(
        CrossingConsumer::from_parts(audio, timing, 80 * 48),
        48_000,
        480,
        0.0,
        DEFAULT_QPC_FREQUENCY,
        events,
        Arc::clone(&shared),
        OutputLayout {
            channels: 6,
            channel_mask: 0x3f,
        },
        480,
    );

    assert_eq!(output.pending_layout_change(), Some(rejected));
    assert_eq!(output.discard_pending_layout(rejected), 4);
    assert_eq!(output.crossing.stream_frame, 4);
    assert_eq!(output.crossing.audio.slots(), 4);
    assert_eq!(output.pending_layout_change(), Some(newer));
    assert_eq!(shared.output_overflow_frames.load(Ordering::Acquire), 4);
    assert!(matches!(
        events_out.pop(),
        Ok(RtEvent::OutputOverflow { frames: 4 })
    ));
}

#[test]
fn zero_frame_starvation_fades_from_the_previous_output_frame() {
    let shared = Arc::new(Shared::new());
    shared.scene_rate.store(48_000, Ordering::Release);
    shared
        .clock_mode
        .store(ClockMode::Trim.as_u32(), Ordering::Release);
    let (mut audio_in, audio) = RingBuffer::<f32>::new(960);
    for _ in 0..960 {
        audio_in.push(0.75).unwrap();
    }
    let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
    timing_in
        .push(TimingMarker {
            stream_frame: 0,
            frames: 480,
            source_qpc: 1,
            layout: OutputLayout::STEREO,
        })
        .unwrap();
    let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
    let mut output = OutputState::new(
        CrossingConsumer::from_parts(audio, timing, 80 * 48),
        48_000,
        480,
        0.0,
        DEFAULT_QPC_FREQUENCY,
        events,
        shared,
        OutputLayout::STEREO,
        480,
    );
    output.strategy_mode = ClockMode::Trim;
    output.asrc.reset(48_000, 48_000);
    output.priming = false;
    output.scene_started = true;
    output.fade_in_remaining = 0;
    let mut period = vec![0.0; 960];
    output.fill(&mut period);
    assert_eq!(&period[958..], &[0.75, 0.75]);

    output.fill(&mut period);

    let fade_frames = boundary_fade_frames(48_000);
    assert_eq!(&period[..2], &[0.75, 0.75]);
    assert_eq!(&period[(fade_frames - 1) * 2..fade_frames * 2], &[0.0, 0.0]);
    assert!(
        period[fade_frames * 2..]
            .iter()
            .all(|sample| *sample == 0.0)
    );
}

#[test]
fn trough_is_the_ring_left_after_the_period() {
    // A period that takes several deliveries reads past markers that stay
    // queued until finish_period. The trough sampled after the render must
    // be what the ring holds, not those markers counted whole: the soaks
    // ran at 20 ms periods on 10 ms deliveries, saw "20 ms" while the ring
    // held 0-10 ms, and underran.
    for (period_frames, block_frames) in [(480, 480), (960, 480), (1_440, 480), (960, 1_536)] {
        let label = format!("period {period_frames}, block {block_frames}");
        let shared = Arc::new(Shared::new());
        shared.scene_rate.store(48_000, Ordering::Release);
        shared
            .clock_mode
            .store(ClockMode::Asrc.as_u32(), Ordering::Release);
        let (mut audio_in, audio) = RingBuffer::<f32>::new(48_000 * 2);
        let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
        let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
        let mut output = OutputState::new(
            CrossingConsumer::from_parts(audio, timing, 48_000),
            48_000,
            period_frames,
            10.0,
            DEFAULT_QPC_FREQUENCY,
            events,
            Arc::clone(&shared),
            OutputLayout::STEREO,
            48_000,
        );
        let mut stream = 0_u64;
        let mut delivered = 0_usize;
        let mut deliver = |audio_in: &mut Producer<f32>,
                           timing_in: &mut Producer<TimingMarker>,
                           delivered: &mut usize| {
            timing_in
                .push(TimingMarker {
                    stream_frame: stream,
                    frames: block_frames as u32,
                    source_qpc: 1,
                    layout: OutputLayout::STEREO,
                })
                .unwrap();
            for _ in 0..block_frames * 2 {
                audio_in.push(0.25).unwrap();
            }
            stream += block_frames as u64;
            *delivered += block_frames;
        };
        let mut period = vec![0.0; period_frames * 2];
        let mut taken = 0_usize;
        for n in 0..150_u64 {
            // Deliver at the period's average rate, with the source a
            // little fast so the controller acts on a moving level.
            while delivered < taken + period_frames * 4 + period_frames * n as usize / 1_000 {
                deliver(&mut audio_in, &mut timing_in, &mut delivered);
            }
            output.fill(&mut period);
            let ring = output.crossing.audio.slots() / 2;
            assert_eq!(output.period_trough_frames, ring, "{label}, period {n}");
            taken += output.period_source_frames;
            output.finish_period(n * period_frames as u64);
        }
        assert_eq!(shared.underruns.load(Ordering::Acquire), 0, "{label}");
    }
}

#[test]
fn starvation_counts_an_underrun_and_installs_a_resume_fade() {
    let shared = Arc::new(Shared::new());
    shared.scene_rate.store(48_000, Ordering::Release);
    shared
        .clock_mode
        .store(ClockMode::Trim.as_u32(), Ordering::Release);
    let (_audio_in, audio) = RingBuffer::<f32>::new(960);
    let (_timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
    let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
    let mut output = OutputState::new(
        CrossingConsumer::from_parts(audio, timing, 80 * 48),
        48_000,
        480,
        0.0,
        DEFAULT_QPC_FREQUENCY,
        events,
        Arc::clone(&shared),
        OutputLayout::STEREO,
        480,
    );
    output.strategy_mode = ClockMode::Trim;
    output.asrc.reset(48_000, 48_000);
    output.priming = false;
    output.scene_started = true;
    output.fade_in_remaining = 0;
    let mut period = vec![1.0; 960];

    assert_eq!(shared.status().last_underrun, None);
    output.fill(&mut period);

    assert_eq!(shared.underruns.load(Ordering::Acquire), 1);
    assert!(output.priming);
    assert_eq!(output.fade_in_remaining, boundary_fade_frames(48_000));
    assert!(period.iter().all(|sample| *sample == 0.0));
    // The status says what the period saw: an empty ring, not a limit.
    let seen = shared
        .status()
        .last_underrun
        .expect("the underrun is published");
    assert_eq!(
        (
            seen.ring_frames,
            seen.marker_frames,
            seen.layout_frames,
            seen.needed_frames,
            seen.produced_frames
        ),
        (0, 0, 0, 480, 0)
    );
    assert!(seen.since_block_ms >= 0.0);
}

#[test]
fn partial_period_consumes_existing_fade_before_installing_resume_fade() {
    let shared = Arc::new(Shared::new());
    shared.scene_rate.store(48_000, Ordering::Release);
    shared
        .clock_mode
        .store(ClockMode::Trim.as_u32(), Ordering::Release);
    let (mut audio_in, audio) = RingBuffer::<f32>::new(200);
    for _ in 0..200 {
        audio_in.push(1.0).unwrap();
    }
    let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
    timing_in
        .push(TimingMarker {
            stream_frame: 0,
            frames: 100,
            source_qpc: 1,
            layout: OutputLayout::STEREO,
        })
        .unwrap();
    let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
    let mut output = OutputState::new(
        CrossingConsumer::from_parts(audio, timing, 80 * 48),
        48_000,
        480,
        0.0,
        DEFAULT_QPC_FREQUENCY,
        events,
        Arc::clone(&shared),
        OutputLayout::STEREO,
        480,
    );
    output.strategy_mode = ClockMode::Trim;
    output.asrc.reset(48_000, 48_000);
    output.priming = false;
    output.scene_started = true;
    let mut period = vec![0.0; 960];

    output.fill(&mut period);

    assert_eq!(shared.underruns.load(Ordering::Acquire), 1);
    assert!(period[0].abs() <= f32::EPSILON);
    assert!(period[2] > period[0]);
    assert_eq!(period[198], 0.0);
    assert_eq!(period[199], 0.0);
    assert_eq!(output.fade_in_remaining, boundary_fade_frames(48_000));
}

#[test]
fn boundary_fades_are_frame_common_end_at_zero_and_leave_normal_flow_exact() {
    let mut stereo = vec![1.0; 100 * 2];
    let mut remaining = boundary_fade_frames(48_000);
    apply_fade_in(&mut stereo, 2, 48_000, 100, &mut remaining);
    assert_eq!(stereo[0..2], [0.0, 0.0]);
    assert_eq!(stereo[2], stereo[3]);
    assert_eq!(remaining, 0);
    assert_eq!(stereo[96 * 2], 1.0);
    assert_eq!(stereo[99 * 2], 1.0);

    let shared = Arc::new(Shared::new());
    shared.scene_rate.store(48_000, Ordering::Release);
    let (_audio_in, audio) = RingBuffer::<f32>::new(2);
    let (_timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
    let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
    let output = OutputState::new(
        CrossingConsumer::from_parts(audio, timing, 80 * 48),
        48_000,
        100,
        10.0,
        DEFAULT_QPC_FREQUENCY,
        events,
        shared,
        OutputLayout::STEREO,
        2,
    );
    let mut suffix = vec![1.0; 100 * 2];
    output.apply_fade_out(&mut suffix, 100);
    assert_eq!(suffix[(100 - 96) * 2 - 1], 1.0);
    assert_eq!(suffix[99 * 2], 0.0);
    assert_eq!(suffix[99 * 2 + 1], 0.0);

    let original = vec![0.25; 100 * 2];
    let mut unchanged = original.clone();
    let mut done = 0;
    apply_fade_in(&mut unchanged, 2, 48_000, 100, &mut done);
    assert_eq!(unchanged, original);
}

#[test]
fn fault_transitions_emit_once_and_clear_only_the_matching_fault() {
    let shared = Arc::new(Shared::new());
    let (producer, mut consumer) = RingBuffer::new(8);
    let mut events = EventWriter::new(producer, Arc::clone(&shared));

    events.fault(FAULT_OUTPUT, RtEvent::OutputFault);
    events.fault(FAULT_OUTPUT, RtEvent::OutputFault);
    events.fault(FAULT_CAPTURE, RtEvent::CaptureFault);
    assert_eq!(
        shared.fault.load(Ordering::Acquire),
        FAULT_OUTPUT | FAULT_CAPTURE
    );
    assert!(matches!(consumer.pop(), Ok(RtEvent::OutputFault)));
    assert!(matches!(consumer.pop(), Ok(RtEvent::CaptureFault)));
    assert!(consumer.pop().is_err());

    events.clear_fault(FAULT_CAPTURE);
    assert_eq!(shared.fault.load(Ordering::Acquire), FAULT_OUTPUT);
    assert!(consumer.pop().is_err());
    events.clear_fault(FAULT_OUTPUT);
    assert_eq!(shared.fault.load(Ordering::Acquire), FAULT_NONE);
    assert!(matches!(consumer.pop(), Ok(RtEvent::FaultCleared)));
    assert!(consumer.pop().is_err());
}

#[test]
fn zero_qpc_tail_marker_does_not_enter_latency_statistics() {
    let shared = Arc::new(Shared::new());
    shared.scene_rate.store(48_000, Ordering::Release);
    shared
        .clock_mode
        .store(ClockMode::Trim.as_u32(), Ordering::Release);
    let (mut audio_in, audio) = RingBuffer::<f32>::new(2);
    audio_in.push(0.5).unwrap();
    audio_in.push(0.5).unwrap();
    let (mut timing_in, timing) = RingBuffer::<TimingMarker>::new(TIMING_QUEUE_CAPACITY);
    timing_in
        .push(TimingMarker {
            stream_frame: 0,
            frames: 1,
            source_qpc: 0,
            layout: OutputLayout::STEREO,
        })
        .unwrap();
    let (events, _events_out) = RingBuffer::<RtEvent>::new(EVENT_QUEUE_CAPACITY);
    let mut output = OutputState::new(
        CrossingConsumer::from_parts(audio, timing, 80 * 48),
        48_000,
        1,
        0.0,
        DEFAULT_QPC_FREQUENCY,
        events,
        Arc::clone(&shared),
        OutputLayout::STEREO,
        1,
    );
    output.strategy_mode = ClockMode::Trim;
    output.priming = false;
    output.scene_started = true;
    output.fade_in_remaining = 0;
    let mut period = [0.0; 2];
    output.fill(&mut period);
    output.finish_period(DEFAULT_QPC_FREQUENCY);

    assert_eq!(shared.latency().samples, 0);
}

#[test]
fn any_endpoint_epoch_requests_an_output_owned_latency_reset() {
    let shared = Arc::new(Shared::new());
    let (mut crossing, _output) = crossing::channel(1, 2);
    let (event_producer, _event_consumer) = RingBuffer::<RtEvent>::new(8);
    let mut events = EventWriter::new(event_producer, Arc::clone(&shared));
    let format = SourceFormat::Pcm(PcmFormat {
        channels: 2,
        sample_rate: 48_000,
        bits_per_sample: 32,
        valid_bits: 32,
        sample_type: SampleType::Float,
        channel_mask: 3,
    });
    let mut latency = LatencyWriter::new(&shared);
    latency.push(12.0, &shared);

    handle_chain_event(
        ChainEvent::FormatChanged {
            endpoint: 1,
            epoch: 1,
            format,
            unsupported: None,
        },
        &mut crossing,
        &mut events,
        &shared,
    );
    assert_eq!(shared.latency().samples, 1);
    assert_eq!(shared.latency_reset_requested.load(Ordering::Acquire), 1);
    latency.apply_requested_reset(&shared);
    assert_eq!(shared.latency().samples, 0);

    latency.push(13.0, &shared);
    handle_chain_event(
        ChainEvent::FormatChanged {
            endpoint: 0,
            epoch: 1,
            format,
            unsupported: None,
        },
        &mut crossing,
        &mut events,
        &shared,
    );
    assert_eq!(shared.latency().samples, 1);
    assert_eq!(shared.latency_reset_requested.load(Ordering::Acquire), 2);
    latency.apply_requested_reset(&shared);
    assert_eq!(shared.latency().samples, 0);
}

#[test]
fn requested_reset_racing_with_output_reset_never_republishes_old_samples() {
    let shared = Shared::new();
    let mut writer = LatencyWriter::new(&shared);
    writer.push(91.0, &shared);
    shared.request_latency_reset();
    writer.reset(&shared);
    shared.request_latency_reset();
    writer.apply_requested_reset(&shared);
    writer.push(7.0, &shared);

    assert_eq!(
        shared.latency(),
        LatencyStats {
            p50_ms: 7.0,
            p99_ms: 7.0,
            samples: 1,
        }
    );
}

#[test]
fn format_seqlock_round_trips_formats() {
    let shared = Shared::new();
    let formats = [
        SourceFormat::Pcm(PcmFormat {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 32,
            valid_bits: 24,
            sample_type: SampleType::Float,
            channel_mask: 3,
        }),
        SourceFormat::Bitstream(BitstreamFormat {
            codec: BitstreamCodec::Eac3Joc,
            container_rate: 192_000,
            container_channels: 2,
            encoded_rate: 48_000,
            encoded_channels: u32::MAX,
        }),
        SourceFormat::Bitstream(BitstreamFormat {
            codec: BitstreamCodec::Other(0xdead_beef),
            container_rate: 192_000,
            container_channels: 8,
            encoded_rate: 96_000,
            encoded_channels: 0xfeed_beef,
        }),
    ];
    for (epoch, format) in formats.into_iter().enumerate() {
        let endpoint = epoch % ENGINE_ENDPOINTS;
        shared.publish_format(endpoint as u16, epoch as u32, format, None);
        assert_eq!(
            shared.endpoints[endpoint].read(),
            (Some(format), epoch as u32, None, 0.0, false)
        );
        assert_eq!(shared.status().endpoints[endpoint].format, Some(format));
    }
}

#[test]
fn endpoint_format_support_and_volume_share_one_seqlock_generation() {
    let shared = Arc::new(Shared::new());
    let formats = [
        SourceFormat::Pcm(PcmFormat {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 32,
            valid_bits: 24,
            sample_type: SampleType::Float,
            channel_mask: 3,
        }),
        SourceFormat::Bitstream(BitstreamFormat {
            codec: BitstreamCodec::Other(0x9a7b_6c5d),
            container_rate: 192_000,
            container_channels: 8,
            encoded_rate: 96_000,
            encoded_channels: 0xfedc_ba98,
        }),
    ];
    shared.publish_format(2, 0, formats[0], None);
    let writer = Arc::clone(&shared);
    let handle = std::thread::spawn(move || {
        let mut state = 0x1234_5678_u32;
        for _ in 0..100_000 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let endpoint = &writer.endpoints[2];
            endpoint.format_seq.fetch_add(1, Ordering::AcqRel);
            let index = (state & 1) as usize;
            let (tag, a, b, c) = encode_format(formats[index]);
            endpoint.format_tag.store(tag, Ordering::Relaxed);
            endpoint.format_a.store(a, Ordering::Relaxed);
            endpoint.format_b.store(b, Ordering::Relaxed);
            endpoint.format_c.store(c, Ordering::Relaxed);
            endpoint.epoch.store(index as u32, Ordering::Relaxed);
            endpoint.unsupported.store(
                if index == 0 {
                    0
                } else {
                    UnsupportedReason::Format.code()
                },
                Ordering::Relaxed,
            );
            endpoint.volume_bits.store(
                if index == 0 { 0.0_f32 } else { -6.0_f32 }.to_bits(),
                Ordering::Relaxed,
            );
            endpoint.muted.store(index != 0, Ordering::Relaxed);
            endpoint.format_seq.fetch_add(1, Ordering::Release);
        }
    });
    let mut state = 0x8765_4321_u32;
    for _ in 0..100_000 {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        if state & 7 == 0 {
            std::hint::spin_loop();
        }
        let observed = shared.endpoints[2].read();
        assert!(
            observed == (Some(formats[0]), 0, None, 0.0, false)
                || observed
                    == (
                        Some(formats[1]),
                        1,
                        Some(UnsupportedReason::Format),
                        -6.0,
                        true,
                    ),
            "torn endpoint state: {observed:?}"
        );
    }
    handle.join().unwrap();
}
