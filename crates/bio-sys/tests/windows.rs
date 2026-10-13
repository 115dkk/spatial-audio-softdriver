#![forbid(unsafe_code)]
#![cfg(windows)]

use bio_sys::{devices, output::WasapiOpener, qpc, sal::SalRing};
use bio_types::{CaptureError, OutputOpener, OutputSink};

/// The exclusive period the cadence tests ask for: the device's minimum
/// unless `BIO_TEST_EXCLUSIVE_PERIOD_MS` names one. CI sets 20 ms because
/// VB-CABLE, a WaveCyclic driver, signals no faster than the 15.6 ms system
/// timer (`exclusive_cadence_at_each_period` measures it; ADR 0026).
fn exclusive_period() -> Option<std::time::Duration> {
    std::env::var("BIO_TEST_EXCLUSIVE_PERIOD_MS")
        .ok()
        .and_then(|ms| ms.parse::<u64>().ok())
        .filter(|&ms| ms > 0)
        .map(std::time::Duration::from_millis)
}

#[test]
#[ignore = "requires at least one active Windows render endpoint"]
fn render_endpoints_include_a_default() {
    let endpoints = devices::render_endpoints().unwrap();
    assert!(!endpoints.is_empty());
    assert!(endpoints.iter().any(|endpoint| endpoint.is_default));
    assert!(
        endpoints
            .iter()
            .all(|endpoint| !endpoint.id.is_empty() && !endpoint.name.is_empty())
    );
    let default = devices::find(None).unwrap();
    assert!(default.is_default);
    let found = devices::find(Some(&default.name.to_uppercase())).unwrap();
    assert!(
        found
            .name
            .to_lowercase()
            .contains(&default.name.to_lowercase())
    );
    assert_eq!(
        devices::find(Some("bio-sys nonexistent endpoint 83c20195")),
        Err(bio_types::OutputError::NotFound)
    );
    println!("{} render endpoints", endpoints.len());
}

#[test]
#[ignore = "requires VB-CABLE with a supported multichannel render format; plays silence"]
fn wasapi_opens_multichannel_layouts_on_the_cable() {
    let Some(cable) = devices::render_endpoint_formats()
        .unwrap()
        .into_iter()
        .find(|formats| formats.endpoint.name.to_lowercase().contains("cable input"))
    else {
        eprintln!("skipped: CABLE Input render endpoint is absent");
        return;
    };
    let mut tested = 0;
    for layout in [
        bio_types::OutputLayout {
            channels: 6,
            channel_mask: 0x3f,
        },
        bio_types::OutputLayout {
            channels: 8,
            channel_mask: 0x63f,
        },
    ] {
        let supported = cable
            .layouts
            .iter()
            .any(|candidate| candidate.channels == layout.channels && candidate.exclusive);
        if !supported {
            eprintln!("skipped: {} channels are unsupported", layout.channels);
            continue;
        }
        let mut sink = WasapiOpener {
            endpoint: Some(cable.endpoint.id.clone()),
            rate: 48_000,
            shared_fallback: true,
            exclusive: true,
            period: exclusive_period(),
        }
        .open(layout)
        .unwrap();
        let format = sink.format();
        assert_eq!(format.channels, layout.channels);
        assert_eq!(format.channel_mask, layout.channel_mask);
        for _ in 0..4 {
            sink.next_period(&mut |samples| samples.fill(0.0)).unwrap();
        }
        tested += 1;
    }
    if tested == 0 {
        eprintln!("skipped: CABLE Input supports neither tested layout");
    }
}

#[test]
#[ignore = "requires access to Windows devices; opens and resets SAL if installed"]
fn sal_open_reports_present_or_not_present() {
    match SalRing::open() {
        Ok(_) => println!("SAL open: success"),
        Err(CaptureError::NotPresent) => println!("SAL open: NotPresent"),
        Err(error) => panic!("SAL open returned {error:?}"),
    }
}

#[test]
#[ignore = "registers this thread with the Windows MMCSS Pro Audio task"]
fn mmcss_pro_audio_registration_reverts_on_drop() {
    let guard = bio_sys::mmcss::promote("Pro Audio").unwrap();
    drop(guard);
    let guard = bio_sys::mmcss::promote("Pro Audio").unwrap();
    drop(guard);
}

#[test]
#[ignore = "requires a render endpoint; requests exclusive mode and plays 200 ms of silence"]
fn exclusive_or_shared_fallback_plays_silence() {
    let mut sink = WasapiOpener {
        endpoint: None,
        rate: 48_000,
        shared_fallback: true,
        exclusive: true,
        period: None,
    }
    .open(bio_types::OutputLayout::STEREO)
    .unwrap();
    println!("Opened {sink:?}");
    let format = sink.format();
    assert_eq!((format.rate, format.channels), (48_000, 2));
    for _ in 0..(format.rate as usize / 5).div_ceil(format.period_frames) {
        assert!(sink.next_period(&mut |samples| samples.fill(0.0)).unwrap() > 0);
    }
}

/// The sink must take frames at the device rate: a sink that skips device
/// events consumes too slowly and the crossing ring fills without bound.
fn assert_real_time_cadence(exclusive: bool) {
    let mut sink = WasapiOpener {
        endpoint: None,
        rate: 48_000,
        exclusive,
        shared_fallback: true,
        period: if exclusive { exclusive_period() } else { None },
    }
    .open(bio_types::OutputLayout::STEREO)
    .unwrap();
    println!("exclusive requested: {exclusive}; opened {sink:?}");
    let format = sink.format();
    for _ in 0..20 {
        sink.next_period(&mut |samples| samples.fill(0.0)).unwrap();
    }
    let started = qpc::now();
    let mut frames = 0_usize;
    while frames < format.rate as usize * 2 {
        sink.next_period(&mut |samples| samples.fill(0.0)).unwrap();
        frames += format.period_frames;
    }
    let seconds = (qpc::now() - started) as f64 / qpc::frequency() as f64;
    let rate = frames as f64 / seconds;
    println!(
        "{} periods of {} frames: {rate:.0} frames/s",
        frames / format.period_frames,
        format.period_frames
    );
    assert!(
        (rate / f64::from(format.rate) - 1.0).abs() < 0.01,
        "the sink took {rate:.0} frames/s at {} Hz",
        format.rate
    );
}

#[test]
#[ignore = "requires a render endpoint; requests exclusive mode and plays 2 s of silence"]
fn exclusive_sink_takes_frames_at_the_device_rate() {
    assert_real_time_cadence(true);
}

#[test]
#[ignore = "requires a render endpoint; plays 2 s of silence in shared mode"]
fn shared_sink_takes_frames_at_the_device_rate() {
    assert_real_time_cadence(false);
}

/// A thread that misses a deadline by 30 ms must not leave the device slow.
/// A TOPPING USB DAC in exclusive mode then signalled only every ~33 ms for
/// good, until the sink learned to restart its stream.
#[test]
#[ignore = "requires a render endpoint; requests exclusive mode and plays 3 s of silence"]
fn exclusive_sink_recovers_its_cadence_after_a_missed_deadline() {
    let mut sink = WasapiOpener {
        endpoint: None,
        rate: 48_000,
        shared_fallback: true,
        exclusive: true,
        period: exclusive_period(),
    }
    .open(bio_types::OutputLayout::STEREO)
    .unwrap();
    let format = sink.format();
    for _ in 0..200 {
        sink.next_period(&mut |samples| samples.fill(0.0)).unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(30));
    // Let a stall show itself and the restart happen.
    let settle = format.rate as usize / 2;
    let mut frames = 0;
    while frames < settle {
        sink.next_period(&mut |samples| samples.fill(0.0)).unwrap();
        frames += format.period_frames;
    }
    let started = qpc::now();
    let mut frames = 0_usize;
    while frames < format.rate as usize * 2 {
        sink.next_period(&mut |samples| samples.fill(0.0)).unwrap();
        frames += format.period_frames;
    }
    let seconds = (qpc::now() - started) as f64 / qpc::frequency() as f64;
    let rate = frames as f64 / seconds;
    println!(
        "{rate:.0} frames/s after the miss, {} restarts",
        sink.restarts()
    );
    assert!(
        (rate / f64::from(format.rate) - 1.0).abs() < 0.01,
        "the sink took {rate:.0} frames/s at {} Hz after the miss",
        format.rate
    );
}

/// Diagnostic for endpoints whose events cannot keep a short exclusive
/// period (WaveCyclic drivers such as VB-CABLE signal at their own timer's
/// rate): prints the rate the sink achieves at each period. It asserts only
/// that every period opens and plays; the cadence numbers decide whether the
/// sink needs to move to a longer period on its own.
#[test]
#[ignore = "requires a render endpoint; plays about 5 s of silence in exclusive mode"]
fn exclusive_cadence_at_each_period() {
    for period_ms in [0_u64, 5, 10, 20, 40] {
        let mut sink = WasapiOpener {
            endpoint: None,
            rate: 48_000,
            shared_fallback: true,
            exclusive: true,
            period: (period_ms > 0).then(|| std::time::Duration::from_millis(period_ms)),
        }
        .open(bio_types::OutputLayout::STEREO)
        .unwrap();
        let format = sink.format();
        let started = qpc::now();
        let mut frames = 0_usize;
        while frames < format.rate as usize {
            sink.next_period(&mut |samples| samples.fill(0.0)).unwrap();
            frames += format.period_frames;
        }
        let seconds = (qpc::now() - started) as f64 / qpc::frequency() as f64;
        println!(
            "period request {period_ms} ms: {sink:?}, {:.0} frames/s, {} restarts",
            frames as f64 / seconds,
            sink.restarts()
        );
    }
}

#[test]
fn qpc_ticks_increase_and_frequency_is_nonzero() {
    assert!(qpc::frequency() > 0);
    let before = qpc::now();
    std::thread::sleep(std::time::Duration::from_millis(2));
    assert!(qpc::now() > before);
}

#[test]
#[ignore = "requires an active default render endpoint and plays 200 ms of silence"]
fn shared_silence_has_period_spaced_playback_qpcs() {
    let mut sink = WasapiOpener {
        endpoint: None,
        rate: 48_000,
        shared_fallback: true,
        exclusive: false,
        period: None,
    }
    .open(bio_types::OutputLayout::STEREO)
    .unwrap();
    let format = sink.format();
    assert_eq!((format.rate, format.channels), (48_000, 2));
    assert!(format.period_frames > 0);
    let count = (format.rate as usize / 5).div_ceil(format.period_frames);
    let expected = qpc::frequency() as f64 * format.period_frames as f64 / f64::from(format.rate);
    // The device clock can remain at zero while the primed silence reaches
    // the endpoint. Check spacing after that documented startup interval.
    for _ in 0..3 {
        sink.next_period(&mut |samples| samples.fill(0.0)).unwrap();
    }
    let mut previous = None;
    for _ in 0..count {
        let timestamp = sink
            .next_period(&mut |samples| {
                assert_eq!(samples.len(), format.period_frames * 2);
                samples.fill(0.0);
            })
            .unwrap();
        if let Some(previous) = previous {
            assert!(timestamp > previous);
            let delta = (timestamp - previous) as f64;
            assert!(
                (delta - expected).abs() < expected * 0.5,
                "QPC delta {delta}, expected {expected}"
            );
        }
        previous = Some(timestamp);
    }
}
