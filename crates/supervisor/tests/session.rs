#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use engine::adapters::{DumpCapture, NullOpener};
use speakers::{SpeakerConfig, SpeakerProfile, SpeakerRange};
use ssr_abi::{RecordWriter, dump_header_bytes, wave_format_bytes};
use ssr_types::{
    OutputError, OutputFormat, OutputLayout, OutputOpener, OutputSink, PcmFormat, SampleType,
    SourceFormat,
};
use supervisor::{RequestFields, Session, SessionConfig};

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

fn profile() -> engine::OutputProfile {
    engine::OutputProfile::Speakers(SpeakerProfile {
        name: "test".into(),
        device_channels: 2,
        channel_mask: 3,
        gain_db: 0.0,
        limiter_ceiling_dbfs: 0.0,
        crossover_hz: 80.0,
        lfe_gain_db: 10.0,
        speakers: vec![
            SpeakerConfig {
                name: "FL".into(),
                azimuth_deg: 30.0,
                elevation_deg: 0.0,
                channel: 0,
                distance_m: None,
                delay_ms: None,
                trim_db: 0.0,
                range: SpeakerRange::Full,
                crossover_hz: None,
                fir: None,
            },
            SpeakerConfig {
                name: "FR".into(),
                azimuth_deg: -30.0,
                elevation_deg: 0.0,
                channel: 1,
                distance_m: None,
                delay_ms: None,
                trim_db: 0.0,
                range: SpeakerRange::Full,
                crossover_hz: None,
                fir: None,
            },
        ],
        subwoofers: Vec::new(),
        bass_to: Vec::new(),
    })
}

fn dump() -> Vec<u8> {
    let format = SourceFormat::Pcm(PcmFormat {
        channels: 2,
        sample_rate: 48_000,
        bits_per_sample: 32,
        valid_bits: 32,
        sample_type: SampleType::Float,
        channel_mask: 3,
    });
    let mut records = vec![0; 16_384];
    let mut writer = RecordWriter::new(&mut records);
    assert!(writer.format_change(1, 0, &wave_format_bytes(&format)));
    assert!(writer.stream_start(1, 0));
    let samples = 0.125_f32.to_le_bytes().repeat(480 * 2);
    assert!(writer.data(1, 0, 0, &samples));
    let len = writer.len();
    let mut dump = dump_header_bytes(1_000_000);
    dump.extend_from_slice(&records[..len]);
    dump
}

fn capture() -> DumpCapture {
    let now = Arc::new(AtomicU64::new(0));
    DumpCapture::from_dump(
        &dump(),
        1_000_000,
        move || now.fetch_add(20_000, Ordering::AcqRel),
        None,
        false,
    )
    .unwrap()
}

fn config() -> SessionConfig {
    SessionConfig {
        engine: engine::EngineConfig {
            chain: engine::ChainConfig {
                arrival_slack_frames: 0,
                ..Default::default()
            },
            read_wait: Duration::ZERO,
            ..Default::default()
        },
        profile: profile(),
        qpc_frequency: 1_000_000,
        mute: false,
        recorder: None,
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
    assert!(matches!(
        session.snapshot().phase,
        ssr_types::app::Phase::Running
    ));
    session.stop().unwrap();
}

#[test]
fn snapshot_is_faulted_after_permanent_output_failure() {
    let mut session = Session::start(config(), capture(), FailingOpener, None).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        session.poll().unwrap();
        if matches!(session.snapshot().phase, ssr_types::app::Phase::Faulted(_)) {
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
