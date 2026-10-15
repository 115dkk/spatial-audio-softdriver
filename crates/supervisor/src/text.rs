use ssr_types::app::Language;
use ssr_types::{Arch, Role, SecureBoot};

/// Every sentence and built-in profile name produced by the supervisor.
///
/// Data supplied by Windows, the engine, profile readers, or setup helpers is
/// kept verbatim and appended to the translated supervisor text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    EngineStopped { detail: String },
    OutputDisconnected { detail: String },
    CaptureDisconnected,
    ReconnectTimedOutOutput { seconds: u64, detail: String },
    ReconnectTimedOutCapture { seconds: u64 },
    TestAfterEnable,
    DiagnosticRecordingUnavailable,
    DisableBeforeChangingOutput,
    ArchitectureMismatch { build: Arch, pc: Arch },
    TestSigningDisabled { secure_boot: SecureBoot },
    DriverSignatureRejected,
    DriverServiceStopped,
    ExclusivePolicyMissing,
    DriverProblem { code: u32 },
    AdminApprovalCancelled,
    DriverInstallFailed { detail: String },
    SetupHelper { detail: String },
    ChosenOutputMissing,
    OwnOutputChosen,
    ChosenOutputInactive,
    MainEndpointMissing,
    EngineStartFailed { detail: String },
    Reconnected,
    EngineStopFailed { detail: String },
    OutputIdChanged { name: String },
    ImportBrirFirst,
    HeadphoneFirReadFailed { detail: String },
    GainOutOfRange,
    DirectionMissing,
    StoredOutputMissing { name: String },
    ProfileFileReadFailed { detail: String },
    ProfileFileTakesPrecedence,
    InvalidBrirSource,
    BrirReadFailed { detail: String },
    ProfilePrepareFailed { detail: String },
    EngineChangeRejected,
    EngineChangeFailed { detail: String },
    ReconnectingOutput { detail: String },
    ReconnectingCapture,
    NotAllowed,
    RebootRequired,
    RoleChangeFailed { detail: String },
    OsRequirement { min_build: u32 },
    RestorePointRemoveFailed { detail: String },
    RestoreFallback { role: Role, name: String },
    RestoreLeftToWindows { role: Role },
    RestoreRoleFailed { role: Role, detail: String },
    DefaultProfileName,
}

impl Message {
    pub fn render(&self, language: Language) -> String {
        match language {
            Language::Ko => self.render_ko(),
            Language::En => self.render_en(),
        }
    }

    fn render_ko(&self) -> String {
        match self {
            Self::EngineStopped { detail } => format!("엔진이 멈췄습니다: {detail}"),
            Self::OutputDisconnected { detail } => {
                format!("출력 장치와의 연결이 끊어졌습니다: {detail}")
            }
            Self::CaptureDisconnected => "캡처 장치와의 연결이 끊어졌습니다.".into(),
            Self::ReconnectTimedOutOutput { seconds, detail } => format!(
                "엔진이 {seconds}초 동안 다시 연결하지 못했습니다. 출력 장치와의 연결이 끊어졌습니다: {detail}"
            ),
            Self::ReconnectTimedOutCapture { seconds } => format!(
                "엔진이 {seconds}초 동안 다시 연결하지 못했습니다. 캡처 장치와의 연결이 끊어졌습니다."
            ),
            Self::TestAfterEnable => "켠 뒤에 시험음을 낼 수 있습니다.".into(),
            Self::DiagnosticRecordingUnavailable => "진단 녹음은 다음 조각에서 옵니다.".into(),
            Self::DisableBeforeChangingOutput => "먼저 끈 뒤 출력을 바꾸십시오.".into(),
            Self::ArchitectureMismatch { build, pc } => format!(
                "이 빌드는 {}용이지만 이 PC는 {}입니다.",
                arch_name(*build, Language::Ko),
                arch_name(*pc, Language::Ko)
            ),
            Self::TestSigningDisabled { secure_boot } => {
                let mut message = "시험서명 모드가 꺼져 있습니다. Secure Boot를 끈 뒤 관리자 명령으로 bcdedit /set testsigning on을 실행하고 다시 시작하십시오.".to_owned();
                if *secure_boot == SecureBoot::Enabled {
                    message.push_str(" Secure Boot가 켜져 있어 먼저 꺼야 합니다.");
                }
                message
            }
            Self::DriverSignatureRejected => "드라이버 서명을 받아들이지 않았습니다(문제 코드 52). 시험서명 모드를 확인한 뒤 다시 설치하십시오.".into(),
            Self::DriverServiceStopped => "드라이버 서비스가 돌고 있지 않습니다. 복구를 실행하십시오.".into(),
            Self::ExclusivePolicyMissing => "드라이버는 있지만 독점 정책이 적용되지 않았습니다. 복구를 실행하십시오.".into(),
            Self::DriverProblem { code } => format!("드라이버가 올바르게 설치되지 않았습니다(문제 코드 {code}). 복구를 실행하십시오."),
            Self::AdminApprovalCancelled => "관리자 승인을 취소했습니다.".into(),
            Self::DriverInstallFailed { detail } => format!("드라이버 설치에 실패했습니다: {detail}"),
            Self::SetupHelper { detail } => format!("설치 도우미: {detail}"),
            Self::ChosenOutputMissing => "고른 출력 장치를 찾을 수 없습니다.".into(),
            Self::OwnOutputChosen => "Spatial SoftReceiver 자신을 출력으로 고를 수 없습니다.".into(),
            Self::ChosenOutputInactive => "고른 출력 장치가 꺼져 있거나 연결되어 있지 않습니다.".into(),
            Self::MainEndpointMissing => "주 엔드포인트가 보이지 않습니다. 드라이버 상태를 확인하십시오.".into(),
            Self::EngineStartFailed { detail } => format!("엔진을 시작하지 못했습니다: {detail}"),
            Self::Reconnected => "다시 연결했습니다.".into(),
            Self::EngineStopFailed { detail } => format!("엔진을 멈추다가 오류가 났습니다: {detail}"),
            Self::OutputIdChanged { name } => format!("출력 장치의 식별자가 바뀌어 이름으로 다시 찾았습니다: {name}"),
            Self::ImportBrirFirst => "먼저 BRIR 세트를 가져오십시오.".into(),
            Self::HeadphoneFirReadFailed { detail } => format!("헤드폰 FIR을 읽지 못했습니다: {detail}"),
            Self::GainOutOfRange => "게인은 -20 dB에서 +10 dB 사이입니다.".into(),
            Self::DirectionMissing => "그 방향이 없습니다.".into(),
            Self::StoredOutputMissing { name } => format!("저장된 출력 장치를 찾을 수 없습니다: {name}"),
            Self::ProfileFileReadFailed { detail } => format!("profile.toml을 읽지 못했습니다: {detail}"),
            Self::ProfileFileTakesPrecedence => "상태 폴더의 profile.toml이 우선합니다. 그 파일을 지우거나 고치십시오.".into(),
            Self::InvalidBrirSource => "hesuvi.wav, hrir.wav 또는 방향별 WAV 폴더를 고르십시오.".into(),
            Self::BrirReadFailed { detail } => format!("BRIR 세트를 읽지 못했습니다: {detail}"),
            Self::ProfilePrepareFailed { detail } => format!("프로필을 준비하지 못했습니다: {detail}"),
            Self::EngineChangeRejected => "엔진이 변경을 받지 못했습니다. 잠시 뒤 다시 시도하십시오.".into(),
            Self::EngineChangeFailed { detail } => format!("엔진에 변경을 적용하지 못했습니다: {detail}"),
            Self::ReconnectingOutput { detail } => format!("출력 장치와의 연결이 끊어져 다시 연결하는 중입니다: {detail}"),
            Self::ReconnectingCapture => "캡처 장치와의 연결이 끊어져 다시 연결하는 중입니다.".into(),
            Self::NotAllowed => "지금은 할 수 없는 동작입니다.".into(),
            Self::RebootRequired => "드라이버 설치를 마치려면 다시 시작해야 합니다.".into(),
            Self::RoleChangeFailed { detail } => format!("기본 장치를 바꾸지 못했습니다: {detail}"),
            Self::OsRequirement { min_build } if *min_build == 19_041 => "Windows 10 2004(빌드 19041) 이상이 필요합니다.".into(),
            Self::OsRequirement { min_build } => format!("Windows 빌드 {min_build} 이상이 필요합니다."),
            Self::RestorePointRemoveFailed { detail } => format!("복원점을 지우지 못했습니다: {detail}"),
            Self::RestoreFallback { role, name } => format!("이전 기본 장치({})를 찾을 수 없어 고른 출력으로 되돌렸습니다: {name}", role_name(*role, Language::Ko)),
            Self::RestoreLeftToWindows { role } => format!("이전 기본 장치({})를 찾을 수 없어 Windows가 고르게 둡니다.", role_name(*role, Language::Ko)),
            Self::RestoreRoleFailed { role, detail } => format!("기본 장치({})를 되돌리지 못했습니다: {detail}", role_name(*role, Language::Ko)),
            Self::DefaultProfileName => "Stereo".into(),
        }
    }

    fn render_en(&self) -> String {
        match self {
            Self::EngineStopped { detail } => format!("The engine stopped: {detail}"),
            Self::OutputDisconnected { detail } => format!("The output device disconnected: {detail}"),
            Self::CaptureDisconnected => "The capture device disconnected.".into(),
            Self::ReconnectTimedOutOutput { seconds, detail } => format!("The engine could not reconnect for {seconds} seconds. The output device disconnected: {detail}"),
            Self::ReconnectTimedOutCapture { seconds } => format!("The engine could not reconnect for {seconds} seconds. The capture device disconnected."),
            Self::TestAfterEnable => "Turn it on before playing a test tone.".into(),
            Self::DiagnosticRecordingUnavailable => "Diagnostic recording is coming in a later update.".into(),
            Self::DisableBeforeChangingOutput => "Turn it off before changing the output.".into(),
            Self::ArchitectureMismatch { build, pc } => format!("This build is for {}, but this PC is {}.", arch_name(*build, Language::En), arch_name(*pc, Language::En)),
            Self::TestSigningDisabled { secure_boot } => {
                let mut message = "Test-signing mode is off. Turn off Secure Boot, run bcdedit /set testsigning on as administrator, and restart Windows.".to_owned();
                if *secure_boot == SecureBoot::Enabled {
                    message.push_str(" Secure Boot is on and must be turned off first.");
                }
                message
            }
            Self::DriverSignatureRejected => "Windows rejected the driver signature (problem code 52). Check test-signing mode, then reinstall the driver.".into(),
            Self::DriverServiceStopped => "The driver service is not running. Run repair.".into(),
            Self::ExclusivePolicyMissing => "The driver is installed, but its exclusive-mode policy is missing. Run repair.".into(),
            Self::DriverProblem { code } => format!("The driver is not installed correctly (problem code {code}). Run repair."),
            Self::AdminApprovalCancelled => "Administrator approval was cancelled.".into(),
            Self::DriverInstallFailed { detail } => format!("Driver installation failed: {detail}"),
            Self::SetupHelper { detail } => format!("Setup helper: {detail}"),
            Self::ChosenOutputMissing => "The selected output device could not be found.".into(),
            Self::OwnOutputChosen => "Spatial SoftReceiver cannot use itself as its output.".into(),
            Self::ChosenOutputInactive => "The selected output device is disabled or disconnected.".into(),
            Self::MainEndpointMissing => "The main endpoint is not available. Check the driver status.".into(),
            Self::EngineStartFailed { detail } => format!("The engine could not start: {detail}"),
            Self::Reconnected => "Reconnected.".into(),
            Self::EngineStopFailed { detail } => format!("An error occurred while stopping the engine: {detail}"),
            Self::OutputIdChanged { name } => format!("The output device identifier changed, so it was found again by name: {name}"),
            Self::ImportBrirFirst => "Import a BRIR set first.".into(),
            Self::HeadphoneFirReadFailed { detail } => format!("The headphone FIR could not be read: {detail}"),
            Self::GainOutOfRange => "Gain must be between -20 dB and +10 dB.".into(),
            Self::DirectionMissing => "That direction does not exist.".into(),
            Self::StoredOutputMissing { name } => format!("The saved output device could not be found: {name}"),
            Self::ProfileFileReadFailed { detail } => format!("profile.toml could not be read: {detail}"),
            Self::ProfileFileTakesPrecedence => "profile.toml in the state folder takes precedence. Remove or edit that file.".into(),
            Self::InvalidBrirSource => "Choose hesuvi.wav, hrir.wav, or a folder of per-direction WAV files.".into(),
            Self::BrirReadFailed { detail } => format!("The BRIR set could not be read: {detail}"),
            Self::ProfilePrepareFailed { detail } => format!("The profile could not be prepared: {detail}"),
            Self::EngineChangeRejected => "The engine could not accept the change. Try again shortly.".into(),
            Self::EngineChangeFailed { detail } => format!("The change could not be applied to the engine: {detail}"),
            Self::ReconnectingOutput { detail } => format!("The output device disconnected. Reconnecting: {detail}"),
            Self::ReconnectingCapture => "The capture device disconnected. Reconnecting.".into(),
            Self::NotAllowed => "That action is not available right now.".into(),
            Self::RebootRequired => "Restart Windows to finish installing the driver.".into(),
            Self::RoleChangeFailed { detail } => format!("The default device could not be changed: {detail}"),
            Self::OsRequirement { min_build } if *min_build == 19_041 => "Windows 10 version 2004 (build 19041) or newer is required.".into(),
            Self::OsRequirement { min_build } => format!("Windows build {min_build} or newer is required."),
            Self::RestorePointRemoveFailed { detail } => format!("The restore point could not be removed: {detail}"),
            Self::RestoreFallback { role, name } => format!("The previous default {} device could not be found, so the selected output was restored: {name}", role_name(*role, Language::En)),
            Self::RestoreLeftToWindows { role } => format!("The previous default {} device could not be found, so Windows will choose one.", role_name(*role, Language::En)),
            Self::RestoreRoleFailed { role, detail } => format!("The default {} device could not be restored: {detail}", role_name(*role, Language::En)),
            Self::DefaultProfileName => "Default stereo".into(),
        }
    }
}

fn arch_name(arch: Arch, language: Language) -> &'static str {
    match (arch, language) {
        (Arch::X64, _) => "x64",
        (Arch::Arm64, _) => "Arm64",
        (Arch::Other, Language::Ko) => "다른 아키텍처",
        (Arch::Other, Language::En) => "another architecture",
    }
}

fn role_name(role: Role, language: Language) -> &'static str {
    match (role, language) {
        (Role::Console, Language::Ko) => "콘솔",
        (Role::Multimedia, Language::Ko) => "멀티미디어",
        (Role::Communications, Language::Ko) => "통신",
        (Role::Console, Language::En) => "console",
        (Role::Multimedia, Language::En) => "multimedia",
        (Role::Communications, Language::En) => "communications",
    }
}
