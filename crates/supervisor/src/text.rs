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
    RoomFirReadFailed { detail: String },
    SpeakerNotInLayout { name: String },
    LayoutUnsupported { layout: String, channels: u16 },
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
    NotAllowed,
    RebootRequired,
    RoleChangeFailed { detail: String },
    OsRequirement { min_build: u32 },
    RestorePointRemoveFailed { detail: String },
    RestoreFallback { role: Role, name: String },
    RestoreLeftToWindows { role: Role },
    RestoreRoleFailed { role: Role, detail: String },
}

impl Message {
    pub fn render(&self, language: Language) -> String {
        match language {
            Language::Ko => self.render_ko(),
            Language::En => self.render_en(),
        }
    }

    fn render_ko(&self) -> String {
        self.render_in(Language::Ko)
    }

    fn render_en(&self) -> String {
        self.render_in(Language::En)
    }

    fn render_in(&self, language: Language) -> String {
        use ssr_i18n::supervisor as text;

        match self {
            Self::EngineStopped { detail } => text::engine_stopped(language, detail),
            Self::OutputDisconnected { detail } => text::output_disconnected(language, detail),
            Self::CaptureDisconnected => text::capture_disconnected(language).into(),
            Self::ReconnectTimedOutOutput { seconds, detail } => {
                text::reconnect_timed_out_output(language, seconds, detail)
            }
            Self::ReconnectTimedOutCapture { seconds } => {
                text::reconnect_timed_out_capture(language, seconds)
            }
            Self::TestAfterEnable => ssr_i18n::common::test_after_enable(language).into(),
            Self::DiagnosticRecordingUnavailable => {
                text::diagnostic_recording_unavailable(language).into()
            }
            Self::DisableBeforeChangingOutput => {
                text::disable_before_changing_output(language).into()
            }
            Self::ArchitectureMismatch { build, pc } => text::architecture_mismatch(
                language,
                arch_name(*build, language),
                arch_name(*pc, language),
            ),
            Self::TestSigningDisabled { secure_boot } => {
                let mut message = text::test_signing_disabled(language).to_owned();
                if *secure_boot == SecureBoot::Enabled {
                    message.push_str(text::test_signing_secure_boot(language));
                }
                message
            }
            Self::DriverSignatureRejected => text::driver_signature_rejected(language).into(),
            Self::DriverServiceStopped => text::driver_service_stopped(language).into(),
            Self::ExclusivePolicyMissing => text::exclusive_policy_missing(language).into(),
            Self::DriverProblem { code } => text::driver_problem(language, code),
            Self::AdminApprovalCancelled => text::admin_approval_cancelled(language).into(),
            Self::DriverInstallFailed { detail } => text::driver_install_failed(language, detail),
            Self::SetupHelper { detail } => text::setup_helper(language, detail),
            Self::ChosenOutputMissing => text::chosen_output_missing(language).into(),
            Self::OwnOutputChosen => text::own_output_chosen(language).into(),
            Self::ChosenOutputInactive => text::chosen_output_inactive(language).into(),
            Self::MainEndpointMissing => text::main_endpoint_missing(language).into(),
            Self::EngineStartFailed { detail } => text::engine_start_failed(language, detail),
            Self::Reconnected => text::reconnected(language).into(),
            Self::EngineStopFailed { detail } => text::engine_stop_failed(language, detail),
            Self::OutputIdChanged { name } => text::output_id_changed(language, name),
            Self::ImportBrirFirst => text::import_brir_first(language).into(),
            Self::RoomFirReadFailed { detail } => text::room_fir_read_failed(language, detail),
            Self::SpeakerNotInLayout { name } => text::speaker_not_in_layout(language, name),
            Self::LayoutUnsupported { layout, channels } => {
                text::layout_unsupported(language, layout, channels)
            }
            Self::GainOutOfRange => text::gain_out_of_range(language).into(),
            Self::DirectionMissing => text::direction_missing(language).into(),
            Self::StoredOutputMissing { name } => text::stored_output_missing(language, name),
            Self::ProfileFileReadFailed { detail } => {
                text::profile_file_read_failed(language, detail)
            }
            Self::ProfileFileTakesPrecedence => {
                ssr_i18n::common::profile_file_takes_precedence(language).into()
            }
            Self::InvalidBrirSource => text::invalid_brir_source(language).into(),
            Self::BrirReadFailed { detail } => text::brir_read_failed(language, detail),
            Self::ProfilePrepareFailed { detail } => text::profile_prepare_failed(language, detail),
            Self::EngineChangeRejected => text::engine_change_rejected(language).into(),
            Self::EngineChangeFailed { detail } => text::engine_change_failed(language, detail),
            Self::NotAllowed => text::not_allowed(language).into(),
            Self::RebootRequired => text::reboot_required(language).into(),
            Self::RoleChangeFailed { detail } => text::role_change_failed(language, detail),
            Self::OsRequirement { min_build } if *min_build == 19_041 => {
                text::os_requirement_19041(language).into()
            }
            Self::OsRequirement { min_build } => text::os_requirement(language, min_build),
            Self::RestorePointRemoveFailed { detail } => {
                text::restore_point_remove_failed(language, detail)
            }
            Self::RestoreFallback { role, name } => {
                text::restore_fallback(language, role_name(*role, language), name)
            }
            Self::RestoreLeftToWindows { role } => {
                text::restore_left_to_windows(language, role_name(*role, language))
            }
            Self::RestoreRoleFailed { role, detail } => {
                text::restore_role_failed(language, role_name(*role, language), detail)
            }
        }
    }
}

fn arch_name(arch: Arch, language: Language) -> &'static str {
    use ssr_i18n::supervisor as text;

    match arch {
        Arch::X64 => text::arch_x64(language),
        Arch::Arm64 => text::arch_arm64(language),
        Arch::Other => text::arch_other(language),
    }
}

fn role_name(role: Role, language: Language) -> &'static str {
    use ssr_i18n::supervisor as text;

    match role {
        Role::Console => text::role_console(language),
        Role::Multimedia => text::role_multimedia(language),
        Role::Communications => text::role_communications(language),
    }
}
