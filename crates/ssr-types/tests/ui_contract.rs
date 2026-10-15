#![cfg(feature = "ts")]
#![forbid(unsafe_code)]
//! The screen contract as files (ADR 0001): the TypeScript bindings specta
//! generates from `Intent` and `Snapshot`, and JSON fixtures of representative
//! snapshots and intents, serialized by serde exactly as the shell sends and
//! receives them, which the screen's tests render.
//!
//! The files live under `apps/spatial-softreceiver/ui`. `SSR_UI_BLESS=1`
//! rewrites them; without it the test fails when they differ, so a change to
//! the types comes with its regenerated bindings and fixtures in the same
//! commit (the same discipline as the driver's C header, `SSR_ABI_BLESS`).

use std::path::{Path, PathBuf};

use specta::TypeCollection;
use specta_typescript::{BigIntExportBehavior, Typescript};
use ssr_types::app::{
    Intent, Language, NextAction, OutputChoice, Phase, ProfileKind, ProfileView, SetupStep,
    Snapshot, UserError,
};
use ssr_types::ports::PolicyState;
use ssr_types::status::{
    ChainErrorCounts, ChainErrorKind, ClockStatus, EndpointStatus, LatencyStats, UnderrunSnapshot,
    UnsupportedReason,
};
use ssr_types::{
    Arch, BitstreamCodec, BitstreamFormat, DeviceId, DirectionId, DirectionView, DriverState,
    EngineFault, EngineStatus, OutputMode, PcmFormat, Platform, SampleType, SceneObject,
    SecureBoot, SourceFormat,
};

const BLESS: &str = "SSR_UI_BLESS";

fn ui_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/spatial-softreceiver/ui")
}

/// Compares `content` with the file, or writes it when blessing.
fn check_or_bless(path: &Path, content: &str) {
    if std::env::var_os(BLESS).is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
        return;
    }
    let existing = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{}: {error}; run with {BLESS}=1", path.display()));
    assert!(
        existing == content,
        "{} is out of date; run `{BLESS}=1 cargo test -p ssr-types --features ts --test ui_contract`",
        path.display()
    );
}

#[test]
fn typescript_bindings_match_the_types() {
    let mut types = TypeCollection::default();
    types
        .register::<Intent>()
        .register::<Snapshot>()
        .register::<UserError>()
        .register::<EngineStatus>();
    let header = "// Generated from crates/ssr-types by `SSR_UI_BLESS=1 cargo test -p ssr-types \
                  --features ts --test ui_contract`. Do not edit.\n\
                  // The shape is serde's: enums with data are tagged `kind` with the data \
                  under `value`; 64-bit counters are JavaScript numbers.";
    let bindings = Typescript::default()
        .header(header)
        .bigint(BigIntExportBehavior::Number)
        .export(&types)
        .unwrap();
    let bindings = bindings
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    check_or_bless(&ui_dir().join("src/bindings.ts"), &bindings);
}

fn choice(id: &str, name: &str) -> OutputChoice {
    OutputChoice {
        id: DeviceId(id.to_owned()),
        name: name.to_owned(),
    }
}

fn outputs() -> Vec<OutputChoice> {
    vec![
        choice(
            "{0.0.0.00000000}.{3f2504e0-4f89-11d3-9a0c-0305e82c3301}",
            "스피커 (Realtek(R) Audio)",
        ),
        choice(
            "{0.0.0.00000000}.{7c1c3e55-3a8c-4b1f-9d33-5a2a7b0d7e12}",
            "DENON AVR (NVIDIA High Definition Audio)",
        ),
        choice(
            "{0.0.0.00000000}.{a1b2c3d4-e5f6-4a5b-8c9d-0e1f2a3b4c5d}",
            "헤드폰 (USB Audio DAC)",
        ),
    ]
}

fn platform(test_signing: bool, secure_boot: SecureBoot) -> Platform {
    Platform {
        os_build: 26_100,
        arch: Arch::X64,
        test_signing,
        secure_boot,
    }
}

fn installed(policy: PolicyState) -> DriverState {
    DriverState::Installed {
        problem_code: 0,
        service_running: true,
        exclusive_policy: policy,
    }
}

fn error(message: &str, next: NextAction) -> UserError {
    UserError {
        message: message.to_owned(),
        next,
    }
}

fn direction(id: u16, name: &str, azimuth_deg: f32, elevation_deg: f32) -> DirectionView {
    DirectionView {
        id: DirectionId(id),
        name: name.to_owned(),
        azimuth_deg,
        elevation_deg,
    }
}

fn stereo_profile() -> ProfileView {
    ProfileView {
        kind: ProfileKind::Stereo,
        name: "Stereo".to_owned(),
        brir: None,
        headphone_fir: None,
        gain_db: 0.0,
        directions: vec![
            direction(0, "FL", 30.0, 0.0),
            direction(1, "FR", -30.0, 0.0),
        ],
        from_file: false,
    }
}

fn headphone_profile() -> ProfileView {
    ProfileView {
        kind: ProfileKind::Headphones,
        name: "거실 7.1.4 헤드폰".to_owned(),
        brir: Some(r"C:\Users\me\BRIR\living-room".to_owned()),
        headphone_fir: Some(r"C:\Users\me\hd650.wav".to_owned()),
        gain_db: -3.5,
        directions: vec![
            // 7.1.4 has eleven render directions: LFE is routed separately and
            // therefore is not a BRIR direction or a test-tone target.
            direction(0, "FL", 30.0, 0.0),
            direction(1, "FR", -30.0, 0.0),
            direction(2, "FC", 0.0, 0.0),
            direction(3, "SL", 90.0, 0.0),
            direction(4, "SR", -90.0, 0.0),
            direction(5, "BL", 150.0, 0.0),
            direction(6, "BR", -150.0, 0.0),
            direction(7, "TFL", 30.0, 45.0),
            direction(8, "TFR", -30.0, 45.0),
            direction(9, "TBL", 150.0, 45.0),
            direction(10, "TBR", -150.0, 45.0),
        ],
        from_file: false,
    }
}

fn blank(phase: Phase) -> Snapshot {
    Snapshot {
        language: Language::Ko,
        language_choice: None,
        phase,
        profile: stereo_profile(),
        engine: None,
        outputs: Vec::new(),
        chosen_output: None,
        driver: None,
        platform: None,
        roles_taken: false,
        error: None,
        notes: Vec::new(),
    }
}

fn with_phase(mut snapshot: Snapshot, phase: Phase) -> Snapshot {
    snapshot.phase = phase;
    snapshot
}

fn engine_status() -> EngineStatus {
    let mut endpoints: [EndpointStatus; 4] = Default::default();
    endpoints[0] = EndpointStatus {
        format: Some(SourceFormat::Pcm(PcmFormat {
            channels: 8,
            sample_rate: 48_000,
            bits_per_sample: 32,
            valid_bits: 32,
            sample_type: SampleType::Float,
            channel_mask: 0x63f,
        })),
        epoch: 3,
        volume_db: -6.0,
        muted: false,
        unsupported: None,
        chain_errors: ChainErrorCounts::default(),
    };
    endpoints[1] = EndpointStatus {
        format: Some(SourceFormat::Bitstream(BitstreamFormat {
            codec: BitstreamCodec::Eac3,
            container_rate: 192_000,
            container_channels: 2,
            encoded_rate: 48_000,
            encoded_channels: 6,
        })),
        epoch: 1,
        volume_db: 0.0,
        muted: false,
        unsupported: None,
        chain_errors: ChainErrorCounts::from_fn(|kind| match kind {
            ChainErrorKind::DecoderBehind => 2,
            _ => 0,
        }),
    };
    endpoints[2] = EndpointStatus {
        format: Some(SourceFormat::Pcm(PcmFormat {
            channels: 12,
            sample_rate: 96_000,
            bits_per_sample: 24,
            valid_bits: 24,
            sample_type: SampleType::Int,
            channel_mask: 0,
        })),
        epoch: 2,
        volume_db: 0.0,
        muted: true,
        unsupported: Some(UnsupportedReason::DirectLayout),
        chain_errors: ChainErrorCounts::from_fn(|kind| match kind {
            ChainErrorKind::MissingDirectLayout => 41,
            _ => 0,
        }),
    };
    EngineStatus {
        directions: headphone_profile().directions,
        scene: vec![
            SceneObject {
                lane: 0,
                index: 0,
                x: -0.5,
                y: 0.866,
                z: 0.0,
                level_dbfs: -12.0,
            },
            SceneObject {
                lane: 0,
                index: 1,
                x: 0.5,
                y: 0.866,
                z: 0.0,
                level_dbfs: -18.0,
            },
            SceneObject {
                lane: 1,
                index: 0,
                x: 0.0,
                y: -1.0,
                z: 0.0,
                level_dbfs: -24.0,
            },
            SceneObject {
                lane: 2,
                index: 3,
                x: 0.25,
                y: 0.433,
                z: 0.866,
                level_dbfs: -30.0,
            },
        ],
        endpoints,
        unattributed_chain_errors: ChainErrorCounts::default(),
        skipped_records: 0,
        latency: LatencyStats {
            p50_ms: 12.4,
            p99_ms: 18.9,
            samples: 300,
        },
        ring_level_ms: 9.6,
        underruns: 1,
        last_underrun: Some(UnderrunSnapshot {
            ring_frames: 96,
            marker_frames: 96,
            layout_frames: 480,
            needed_frames: 480,
            produced_frames: 96,
            since_block_ms: 31.5,
        }),
        output_periods: 12_345,
        capture_reads: 6_789,
        output_restarts: 0,
        output_rate: 48_000,
        trim_available: true,
        output_channels: 2,
        output_channel_mask: 3,
        output_reopens: 1,
        output_reconnects: 0,
        output_reconnect_attempts: 0,
        output_mode: OutputMode::Exclusive,
        output_fell_back: false,
        output_mix_channels: 0,
        output_cadence_ratio: 1.001,
        output_cadence_faults: 0,
        lost_bytes: 0,
        lane_underrun_frames: 1_920,
        dropped_frames: 0,
        decode_refused: 2,
        volume_transitions_coalesced: 0,
        output_overflow_frames: 0,
        idle_blocks: 1_204,
        dropped_events: 0,
        clock: ClockStatus::Asrc,
        clock_ppm: -37.5,
        fault: None,
    }
}

/// One snapshot per screen state the wizard, the status screen and the error
/// banner must draw. The messages are the supervisor's own sentences.
fn snapshots() -> Vec<(&'static str, Snapshot)> {
    let ready = Snapshot {
        outputs: outputs(),
        chosen_output: Some(outputs()[1].clone()),
        driver: Some(installed(PolicyState::Applied)),
        platform: Some(platform(true, SecureBoot::Disabled)),
        ..blank(Phase::Ready)
    };
    vec![
        ("setup-welcome", blank(Phase::Setup(SetupStep::Welcome))),
        (
            "setup-unsupported",
            Snapshot {
                platform: Some(Platform {
                    os_build: 18_363,
                    ..platform(false, SecureBoot::Enabled)
                }),
                error: Some(error(
                    "Windows 10 2004(빌드 19041) 이상이 필요합니다. 이 PC는 빌드 18363입니다.",
                    NextAction::None,
                )),
                ..blank(Phase::Setup(SetupStep::Unsupported))
            },
        ),
        (
            "setup-not-test-signing",
            Snapshot {
                platform: Some(platform(false, SecureBoot::Enabled)),
                error: Some(error(
                    "시험서명 모드가 꺼져 있습니다. Secure Boot를 끈 뒤 관리자 명령으로 bcdedit \
                     /set testsigning on을 실행하고 다시 시작하십시오. Secure Boot가 켜져 있어 \
                     먼저 꺼야 합니다.",
                    NextAction::Reboot,
                )),
                ..blank(Phase::Setup(SetupStep::NotTestSigning))
            },
        ),
        (
            "setup-driver-not-installed",
            Snapshot {
                platform: Some(platform(true, SecureBoot::Disabled)),
                driver: Some(DriverState::NotInstalled),
                ..blank(Phase::Setup(SetupStep::Driver))
            },
        ),
        (
            "setup-driver-problem",
            Snapshot {
                platform: Some(platform(true, SecureBoot::Disabled)),
                driver: Some(DriverState::Installed {
                    problem_code: 52,
                    service_running: false,
                    exclusive_policy: PolicyState::Unknown,
                }),
                error: Some(error(
                    "드라이버 서명을 받아들이지 않았습니다(문제 코드 52). 시험서명 모드를 확인한 뒤 \
                     다시 설치하십시오.",
                    NextAction::Retry,
                )),
                ..blank(Phase::Setup(SetupStep::Driver))
            },
        ),
        (
            "setup-driver-declined",
            Snapshot {
                platform: Some(platform(true, SecureBoot::Disabled)),
                driver: Some(DriverState::NotInstalled),
                error: Some(error("관리자 승인을 취소했습니다.", NextAction::Retry)),
                ..blank(Phase::Setup(SetupStep::Driver))
            },
        ),
        (
            "setup-reboot-required",
            Snapshot {
                platform: Some(platform(true, SecureBoot::Disabled)),
                driver: Some(DriverState::RebootRequired),
                error: Some(error(
                    "드라이버 설치를 마치려면 다시 시작해야 합니다.",
                    NextAction::Reboot,
                )),
                ..blank(Phase::Setup(SetupStep::RebootRequired))
            },
        ),
        (
            "setup-choose-output",
            Snapshot {
                outputs: outputs(),
                driver: Some(installed(PolicyState::Applied)),
                platform: Some(platform(true, SecureBoot::Disabled)),
                ..blank(Phase::Setup(SetupStep::ChooseOutput))
            },
        ),
        ("ready", ready.clone()),
        (
            "ready-output-renamed",
            Snapshot {
                notes: vec!["고른 출력의 식별자가 바뀌어 이름으로 다시 찾았습니다.".to_owned()],
                error: Some(error(
                    "저장된 출력 장치를 찾을 수 없습니다: DENON AVR (NVIDIA High Definition Audio)",
                    NextAction::OpenSettings,
                )),
                chosen_output: None,
                ..ready.clone()
            },
        ),
        (
            "running",
            Snapshot {
                profile: headphone_profile(),
                engine: Some(engine_status()),
                roles_taken: true,
                notes: vec![
                    "이전 기본 장치 '스피커 (Realtek(R) Audio)'를 복원점에 적었습니다.".to_owned(),
                ],
                ..with_phase(ready.clone(), Phase::Running)
            },
        ),
        (
            "running-reconnecting",
            Snapshot {
                profile: headphone_profile(),
                engine: Some(EngineStatus {
                    fault: Some(EngineFault::OutputLost(
                        "AUDCLNT_E_DEVICE_INVALIDATED".to_owned(),
                    )),
                    ..engine_status()
                }),
                roles_taken: true,
                error: Some(error(
                    "출력 장치와의 연결이 끊어져 다시 연결하는 중입니다: AUDCLNT_E_DEVICE_INVALIDATED",
                    NextAction::None,
                )),
                ..with_phase(ready.clone(), Phase::Running)
            },
        ),
        (
            "faulted",
            Snapshot {
                error: Some(error(
                    "엔진이 15초 동안 다시 연결하지 못했습니다. 출력 장치와의 연결이 \
                     끊어졌습니다: AUDCLNT_E_DEVICE_INVALIDATED",
                    NextAction::Retry,
                )),
                ..with_phase(ready.clone(), Phase::Faulted)
            },
        ),
        ("quit", with_phase(ready, Phase::Quit)),
    ]
}

/// Every intent, as the screen must send it.
fn intents() -> Vec<Intent> {
    vec![
        Intent::SetLanguage(None),
        Intent::SetLanguage(Some(Language::Ko)),
        Intent::SetLanguage(Some(Language::En)),
        Intent::BeginSetup,
        Intent::InstallDriver,
        Intent::Recheck,
        Intent::ChooseOutput(outputs()[1].id.clone()),
        Intent::ImportBrir(PathBuf::from(r"C:\Users\me\BRIR\living-room")),
        Intent::ImportHeadphoneFir(PathBuf::from(r"C:\Users\me\hd650.wav")),
        Intent::SetGain(-3.5),
        Intent::TestDirection(DirectionId(7)),
        Intent::Enable,
        Intent::Disable,
        Intent::StartDiagnosticRecording,
        Intent::Quit,
    ]
}

fn pretty<T: serde::Serialize>(value: &T) -> String {
    let mut json = serde_json::to_string_pretty(value).unwrap();
    json.push('\n');
    json
}

#[test]
fn snapshot_fixtures_match_the_types() {
    let dir = ui_dir().join("fixtures");
    let mut names = Vec::new();
    for (name, snapshot) in snapshots() {
        check_or_bless(&dir.join(format!("{name}.json")), &pretty(&snapshot));
        let back: Snapshot = serde_json::from_str(&pretty(&snapshot)).unwrap();
        assert_eq!(back, snapshot, "{name} does not round-trip");
        names.push(name);
    }
    check_or_bless(&dir.join("intents.json"), &pretty(&intents()));
    let back: Vec<Intent> = serde_json::from_str(&pretty(&intents())).unwrap();
    assert_eq!(back, intents());
    check_or_bless(&dir.join("index.json"), &pretty(&names));
}
