#![forbid(unsafe_code)]

use ssr_types::app::Language;
use ssr_types::{Arch, Role, SecureBoot};
use supervisor::text::Message;

#[test]
fn every_message_renders_nonempty_and_distinct_in_both_languages() {
    let messages = vec![
        Message::EngineStopped {
            detail: "detail".into(),
        },
        Message::OutputDisconnected {
            detail: "detail".into(),
        },
        Message::CaptureDisconnected,
        Message::ReconnectTimedOutOutput {
            seconds: 15,
            detail: "detail".into(),
        },
        Message::ReconnectTimedOutCapture { seconds: 15 },
        Message::TestAfterEnable,
        Message::DiagnosticRecordingUnavailable,
        Message::DisableBeforeChangingOutput,
        Message::ArchitectureMismatch {
            build: Arch::X64,
            pc: Arch::Arm64,
        },
        Message::TestSigningDisabled {
            secure_boot: SecureBoot::Enabled,
        },
        Message::DriverSignatureRejected,
        Message::DriverServiceStopped,
        Message::ExclusivePolicyMissing,
        Message::DriverProblem { code: 10 },
        Message::AdminApprovalCancelled,
        Message::DriverInstallFailed {
            detail: "detail".into(),
        },
        Message::SetupHelper {
            detail: "detail".into(),
        },
        Message::ChosenOutputMissing,
        Message::OwnOutputChosen,
        Message::ChosenOutputInactive,
        Message::MainEndpointMissing,
        Message::EngineStartFailed {
            detail: "detail".into(),
        },
        Message::Reconnected,
        Message::EngineStopFailed {
            detail: "detail".into(),
        },
        Message::OutputIdChanged { name: "DAC".into() },
        Message::ImportBrirFirst,
        Message::CorrectionReadFailed {
            detail: "detail".into(),
        },
        Message::CorrectionUnsupported { channels: 12 },
        Message::LayoutUnsupported {
            layout: "7.1.4".into(),
            channels: 12,
        },
        Message::GainOutOfRange,
        Message::DirectionMissing,
        Message::StoredOutputMissing { name: "DAC".into() },
        Message::ProfileFileReadFailed {
            detail: "detail".into(),
        },
        Message::ProfileFileTakesPrecedence,
        Message::InvalidBrirSource,
        Message::BrirReadFailed {
            detail: "detail".into(),
        },
        Message::ProfilePrepareFailed {
            detail: "detail".into(),
        },
        Message::EngineChangeRejected,
        Message::EngineChangeFailed {
            detail: "detail".into(),
        },
        Message::NotAllowed,
        Message::RebootRequired,
        Message::RoleChangeFailed {
            detail: "detail".into(),
        },
        Message::OsRequirement { min_build: 19_041 },
        Message::RestorePointRemoveFailed {
            detail: "detail".into(),
        },
        Message::RestoreFallback {
            role: Role::Console,
            name: "DAC".into(),
        },
        Message::RestoreLeftToWindows {
            role: Role::Multimedia,
        },
        Message::RestoreRoleFailed {
            role: Role::Communications,
            detail: "detail".into(),
        },
    ];

    for message in messages {
        let ko = message.render(Language::Ko);
        let en = message.render(Language::En);
        assert!(!ko.is_empty(), "empty Korean: {message:?}");
        assert!(!en.is_empty(), "empty English: {message:?}");
        assert_ne!(ko, en, "same rendering: {message:?}");
    }
}
