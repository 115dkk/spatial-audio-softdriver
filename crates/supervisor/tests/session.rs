#![forbid(unsafe_code)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use common::{capture, config};
use engine::adapters::NullOpener;
use ssr_types::{OutputError, OutputFormat, OutputLayout, OutputOpener, OutputSink};
use supervisor::{DriverStatsSource, RequestFields, Session, SessionError};

struct FailingOpener;

impl OutputOpener for FailingOpener {
    type Sink = FailingSink;

    fn open(&mut self, layout: OutputLayout) -> Result<Self::Sink, OutputError> {
        Ok(FailingSink {
            format: OutputFormat::exclusive(48_000, layout.channels, layout.channel_mask, 480),
        })
    }
}

struct FailingSink {
    format: OutputFormat,
}

impl OutputSink for FailingSink {
    fn format(&self) -> OutputFormat {
        self.format
    }

    fn next_period(&mut self, _fill: &mut dyn FnMut(&mut [f32])) -> Result<u64, OutputError> {
        Err(OutputError::Removed)
    }
}

#[test]
fn dump_session_report_keeps_the_live_cli_key_contract() {
    let mut session = Session::start(
        config(),
        capture(),
        NullOpener {
            rate: 48_000,
            period_frames: 480,
        },
        None,
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(20));
    session.poll().unwrap();
    let report = session.stop().unwrap().to_json(&RequestFields {
        source: "dump".into(),
        clock_requested: "auto".into(),
        output_mode_requested: "exclusive".into(),
        direct_layouts: Vec::new(),
    });
    let actual = report
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    let expected = [
        "output_mode",
        "output_fell_back",
        "output_kind",
        "output_channels",
        "output_channel_mask",
        "direct_layouts",
        "clock_strategy",
        "seconds",
        "blocks",
        "underruns",
        "underruns_while_streaming",
        "last_underrun",
        "output_restarts",
        "output_reopens",
        "output_reconnects",
        "output_reconnect_attempts",
        "output_mix_channels",
        "output_cadence_ratio",
        "output_cadence_faults",
        "lost_bytes",
        "lane_underrun_frames",
        "dropped_frames",
        "decode_refused",
        "volume_transitions_coalesced",
        "output_overflow_frames",
        "idle_blocks",
        "dropped_events",
        "skipped_records",
        "endpoints",
        "chain_errors",
        "overruns",
        "faults",
        "latency_p50_ms",
        "latency_p99_ms",
        "ring_trough_min_ms",
        "clock_ppm",
        "clock_ppm_max_abs",
        "tee_dropped_bytes",
        "recorder_lost_bytes",
        "driver",
        "driver_stats_error",
        "source",
        "clock_requested",
        "output_mode_requested",
        "output_sample_rate",
        "trim_available",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(actual, expected);
    let endpoints = report["endpoints"].as_array().unwrap();
    assert_eq!(endpoints.len(), ssr_abi::MAX_ENDPOINTS);
    assert_eq!(
        endpoints[0]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        [
            "endpoint",
            "format",
            "epoch",
            "unsupported",
            "unsupported_reason",
            "chain_errors",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
    assert!(report["seconds"].is_number());
    assert!(report["blocks"].is_number());
    assert!(report["faults"].is_array());
    assert!(report["direct_layouts"].is_array());
    assert!(report["chain_errors"]["unattributed"].is_object());
    assert!(report["driver"].is_null());
    assert!(report["driver_stats_error"].is_null());
    assert!(report["source"].is_string());
    assert!(report["trim_available"].is_boolean());
}

#[test]
fn snapshot_is_running_without_a_fault() {
    let session = Session::start(
        config(),
        capture(),
        NullOpener {
            rate: 48_000,
            period_frames: 480,
        },
        None,
    )
    .unwrap();
    let snapshot = session.snapshot();
    assert_eq!(snapshot.phase, ssr_types::app::Phase::Running);
    assert_eq!(snapshot.error, None);
    session.stop().unwrap();
}

#[test]
fn snapshot_is_faulted_after_permanent_output_failure() {
    let mut session = Session::start(config(), capture(), FailingOpener, None).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        session.poll().unwrap();
        let snapshot = session.snapshot();
        if snapshot.phase == ssr_types::app::Phase::Faulted {
            let error = snapshot.error.expect("a fault carries its error");
            assert!(error.message.starts_with("엔진이 멈췄습니다: OutputLost"));
            assert_eq!(error.next, ssr_types::app::NextAction::Retry);
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "output failure did not reach the session snapshot"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    session.stop().unwrap();
}

struct CountingStats(Arc<AtomicU64>);

impl DriverStatsSource for CountingStats {
    fn sample(&self) -> Result<(u64, ssr_abi::DriverStats), String> {
        self.0.fetch_add(1, Ordering::AcqRel);
        Ok((
            0,
            ssr_abi::DriverStats {
                struct_size: std::mem::size_of::<ssr_abi::DriverStats>() as u32,
                open_count: 1,
                max_timer_lateness_us: 0,
                reserved: 0,
                endpoints: [ssr_abi::EndpointStats {
                    total_bytes: 0,
                    overruns: 0,
                    lost_bytes: 0,
                    epoch: 1,
                    state: 1,
                }; ssr_abi::MAX_ENDPOINTS],
            },
        ))
    }
}

#[test]
fn the_driver_baseline_is_taken_before_the_output_is_prepared() {
    // A driver ring that overflows while a large profile prepares is this
    // run's loss, so the baseline sample comes first; a profile that cannot
    // prepare shows the order.
    let samples = Arc::new(AtomicU64::new(0));
    let mut config = config();
    let engine::OutputProfile::Speakers(speakers) = &mut config.profile else {
        unreachable!()
    };
    speakers.speakers[1].channel = 5;
    let started = Session::start(
        config,
        capture(),
        NullOpener {
            rate: 48_000,
            period_frames: 480,
        },
        Some(Box::new(CountingStats(Arc::clone(&samples)))),
    );
    assert!(matches!(started, Err(SessionError::Prepare(_))));
    assert_eq!(samples.load(Ordering::Acquire), 1);
}
