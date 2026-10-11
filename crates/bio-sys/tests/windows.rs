#![forbid(unsafe_code)]
#![cfg(windows)]

use bio_sys::{devices, output::WasapiOpener, qpc, sal::SalRing};
use bio_types::{CaptureError, OutputOpener, OutputSink};

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
        exclusive: true,
        period: None,
    }
    .open()
    .unwrap();
    println!("Opened {sink:?}");
    let format = sink.format();
    assert_eq!((format.rate, format.channels), (48_000, 2));
    for _ in 0..(format.rate as usize / 5).div_ceil(format.period_frames) {
        assert!(sink.next_period(&mut |samples| samples.fill(0.0)).unwrap() > 0);
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
        exclusive: false,
        period: None,
    }
    .open()
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
