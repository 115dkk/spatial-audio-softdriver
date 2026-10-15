//! Fakes shared by the session and state machine tests: a two-speaker profile
//! and a short PCM dump that plays through the null output.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use engine::adapters::DumpCapture;
use speakers::{SpeakerConfig, SpeakerProfile, SpeakerRange};
use ssr_abi::{RecordWriter, dump_header_bytes, wave_format_bytes};
use ssr_types::{PcmFormat, SampleType, SourceFormat};
use supervisor::SessionConfig;

pub fn profile() -> engine::OutputProfile {
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

pub fn dump() -> Vec<u8> {
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

pub fn capture() -> DumpCapture {
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

pub fn config() -> SessionConfig {
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
