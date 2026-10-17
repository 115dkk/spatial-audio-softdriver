// Generated from i18n/*.toml. Do not edit by hand.
// Regenerate with `SSR_I18N_BLESS=1 cargo test -p ssr-i18n`.

use ssr_types::app::Language;

pub mod names {
    use super::Language;

    #[doc = "공간 소프트리시버 가상 오디오 장치"]
    pub fn driver_device(language: Language) -> &'static str {
        match language {
            Language::Ko => "공간 소프트리시버 가상 오디오 장치",
            Language::En => "Spatial SoftReceiver Virtual Audio Device",
        }
    }

    #[doc = "공간 소프트리시버 드라이버 디스크"]
    pub fn driver_disk(language: Language) -> &'static str {
        match language {
            Language::Ko => "공간 소프트리시버 드라이버 디스크",
            Language::En => "Spatial SoftReceiver Driver Disk",
        }
    }

    #[doc = "공간 소프트리시버 가상 오디오 드라이버"]
    pub fn driver_service(language: Language) -> &'static str {
        match language {
            Language::Ko => "공간 소프트리시버 가상 오디오 드라이버",
            Language::En => "Spatial SoftReceiver Virtual Audio Driver",
        }
    }

    #[doc = "공간 소프트리시버"]
    pub fn endpoint_main(language: Language) -> &'static str {
        match language {
            Language::Ko => "공간 소프트리시버",
            Language::En => "Spatial SoftReceiver",
        }
    }

    #[doc = "공간 소프트리시버 음악"]
    pub fn endpoint_music(language: Language) -> &'static str {
        match language {
            Language::Ko => "공간 소프트리시버 음악",
            Language::En => "Spatial SoftReceiver Music",
        }
    }

    #[doc = "공간 소프트리시버 PASS"]
    pub fn endpoint_pass(language: Language) -> &'static str {
        match language {
            Language::Ko => "공간 소프트리시버 PASS",
            Language::En => "Spatial SoftReceiver PASS",
        }
    }

    #[doc = "공간 소프트리시버"]
    pub fn product(language: Language) -> &'static str {
        match language {
            Language::Ko => "공간 소프트리시버",
            Language::En => "Spatial SoftReceiver",
        }
    }
}

pub mod common {
    use super::Language;

    #[doc = "상태 폴더의 profile.toml이 우선합니다. 그 파일을 지우거나 고치십시오."]
    pub fn profile_file_takes_precedence(language: Language) -> &'static str {
        match language {
            Language::Ko => "상태 폴더의 profile.toml이 우선합니다. 그 파일을 지우거나 고치십시오.",
            Language::En => {
                "profile.toml in the state folder takes precedence. Remove or edit that file."
            }
        }
    }

    #[doc = "켠 뒤에 시험용 신호를 재생할 수 있습니다."]
    pub fn test_after_enable(language: Language) -> &'static str {
        match language {
            Language::Ko => "켠 뒤에 시험용 신호를 재생할 수 있습니다.",
            Language::En => "Turn it on before playing a test signal.",
        }
    }

    #[doc = "끄기"]
    pub fn turn_off(language: Language) -> &'static str {
        match language {
            Language::Ko => "끄기",
            Language::En => "Turn off",
        }
    }

    #[doc = "켜기"]
    pub fn turn_on(language: Language) -> &'static str {
        match language {
            Language::Ko => "켜기",
            Language::En => "Turn on",
        }
    }
}

pub mod supervisor {
    use super::Language;

    #[doc = "관리자 승인을 취소했습니다."]
    pub fn admin_approval_cancelled(language: Language) -> &'static str {
        match language {
            Language::Ko => "관리자 승인을 취소했습니다.",
            Language::En => "Administrator approval was cancelled.",
        }
    }

    #[doc = "Arm64"]
    pub fn arch_arm64(language: Language) -> &'static str {
        match language {
            Language::Ko => "Arm64",
            Language::En => "Arm64",
        }
    }

    #[doc = "다른 종류의 PC"]
    pub fn arch_other(language: Language) -> &'static str {
        match language {
            Language::Ko => "다른 종류의 PC",
            Language::En => "a different type of PC",
        }
    }

    #[doc = "x64"]
    pub fn arch_x64(language: Language) -> &'static str {
        match language {
            Language::Ko => "x64",
            Language::En => "x64",
        }
    }

    #[doc = "이 버전은 {build} PC용이지만 이 PC는 {pc}입니다."]
    pub fn architecture_mismatch(
        language: Language,
        build: impl core::fmt::Display,
        pc: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!("이 버전은 {build} PC용이지만 이 PC는 {pc}입니다."),
            Language::En => format!("This version is for {build} PCs, but this PC is {pc}."),
        }
    }

    #[doc = "BRIR 세트를 읽지 못했습니다: {detail}"]
    pub fn brir_read_failed(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("BRIR 세트를 읽지 못했습니다: {detail}"),
            Language::En => format!("The BRIR set could not be read: {detail}"),
        }
    }

    #[doc = "캡처 장치와의 연결이 끊어졌습니다."]
    pub fn capture_disconnected(language: Language) -> &'static str {
        match language {
            Language::Ko => "캡처 장치와의 연결이 끊어졌습니다.",
            Language::En => "The capture device disconnected.",
        }
    }

    #[doc = "먼저 업믹스 플러그인을 고르십시오."]
    pub fn choose_plugin_first(language: Language) -> &'static str {
        match language {
            Language::Ko => "먼저 업믹스 플러그인을 고르십시오.",
            Language::En => "Choose an upmixer plugin first.",
        }
    }

    #[doc = "고른 출력 장치가 꺼져 있거나 연결되어 있지 않습니다."]
    pub fn chosen_output_inactive(language: Language) -> &'static str {
        match language {
            Language::Ko => "고른 출력 장치가 꺼져 있거나 연결되어 있지 않습니다.",
            Language::En => "The selected output device is disabled or disconnected.",
        }
    }

    #[doc = "고른 출력 장치를 찾을 수 없습니다."]
    pub fn chosen_output_missing(language: Language) -> &'static str {
        match language {
            Language::Ko => "고른 출력 장치를 찾을 수 없습니다.",
            Language::En => "The selected output device could not be found.",
        }
    }

    #[doc = "보정 파일을 읽지 못했습니다: {detail}"]
    pub fn correction_read_failed(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("보정 파일을 읽지 못했습니다: {detail}"),
            Language::En => format!("The correction file could not be read: {detail}"),
        }
    }

    #[doc = "고른 출력 장치가 독점 모드에서 보정 파일에 지정된 {channels}채널 출력을 지원하지 않습니다. 다른 출력 장치를 고르십시오."]
    pub fn correction_unsupported(language: Language, channels: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!(
                "고른 출력 장치가 독점 모드에서 보정 파일에 지정된 {channels}채널 출력을 지원하지 않습니다. 다른 출력 장치를 고르십시오."
            ),
            Language::En => format!(
                "The selected output device does not support the {channels}-channel output specified in the correction file in exclusive mode. Choose another output device."
            ),
        }
    }

    #[doc = "진단 기록 기능은 이후 업데이트에서 제공됩니다."]
    pub fn diagnostic_recording_unavailable(language: Language) -> &'static str {
        match language {
            Language::Ko => "진단 기록 기능은 이후 업데이트에서 제공됩니다.",
            Language::En => "Diagnostic recording will be available in a later update.",
        }
    }

    #[doc = "선택한 방향을 찾을 수 없습니다."]
    pub fn direction_missing(language: Language) -> &'static str {
        match language {
            Language::Ko => "선택한 방향을 찾을 수 없습니다.",
            Language::En => "The selected direction could not be found.",
        }
    }

    #[doc = "먼저 끈 뒤 출력을 바꾸십시오."]
    pub fn disable_before_changing_output(language: Language) -> &'static str {
        match language {
            Language::Ko => "먼저 끈 뒤 출력을 바꾸십시오.",
            Language::En => "Turn it off before changing the output.",
        }
    }

    #[doc = "드라이버 설치에 실패했습니다: {detail}"]
    pub fn driver_install_failed(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("드라이버 설치에 실패했습니다: {detail}"),
            Language::En => format!("Driver installation failed: {detail}"),
        }
    }

    #[doc = "드라이버가 올바르게 설치되지 않았습니다(문제 코드 {code}). 복구를 실행하십시오."]
    pub fn driver_problem(language: Language, code: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!(
                "드라이버가 올바르게 설치되지 않았습니다(문제 코드 {code}). 복구를 실행하십시오."
            ),
            Language::En => {
                format!("The driver is not installed correctly (problem code {code}). Run repair.")
            }
        }
    }

    #[doc = "드라이버 서비스가 실행되고 있지 않습니다. 복구를 실행하십시오."]
    pub fn driver_service_stopped(language: Language) -> &'static str {
        match language {
            Language::Ko => "드라이버 서비스가 실행되고 있지 않습니다. 복구를 실행하십시오.",
            Language::En => "The driver service is not running. Run repair.",
        }
    }

    #[doc = "Windows가 드라이버 서명을 확인하지 못했습니다(문제 코드 52). 시험서명 모드를 확인한 뒤 다시 설치하십시오."]
    pub fn driver_signature_rejected(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "Windows가 드라이버 서명을 확인하지 못했습니다(문제 코드 52). 시험서명 모드를 확인한 뒤 다시 설치하십시오."
            }
            Language::En => {
                "Windows could not verify the driver signature (problem code 52). Check test-signing mode, then reinstall the driver."
            }
        }
    }

    #[doc = "이 입력은 업믹스하지 않습니다."]
    pub fn endpoint_cannot_upmix(language: Language) -> &'static str {
        match language {
            Language::Ko => "이 입력은 업믹스하지 않습니다.",
            Language::En => "This input does not upmix.",
        }
    }

    #[doc = "엔진에 변경을 적용하지 못했습니다: {detail}"]
    pub fn engine_change_failed(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("엔진에 변경을 적용하지 못했습니다: {detail}"),
            Language::En => format!("The change could not be applied to the engine: {detail}"),
        }
    }

    #[doc = "엔진이 변경을 받지 못했습니다. 잠시 뒤 다시 시도하십시오."]
    pub fn engine_change_rejected(language: Language) -> &'static str {
        match language {
            Language::Ko => "엔진이 변경을 받지 못했습니다. 잠시 뒤 다시 시도하십시오.",
            Language::En => "The engine could not accept the change. Try again shortly.",
        }
    }

    #[doc = "엔진을 시작하지 못했습니다: {detail}"]
    pub fn engine_start_failed(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("엔진을 시작하지 못했습니다: {detail}"),
            Language::En => format!("The engine could not start: {detail}"),
        }
    }

    #[doc = "엔진을 멈추다가 오류가 났습니다: {detail}"]
    pub fn engine_stop_failed(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("엔진을 멈추다가 오류가 났습니다: {detail}"),
            Language::En => format!("An error occurred while stopping the engine: {detail}"),
        }
    }

    #[doc = "엔진이 멈췄습니다: {detail}"]
    pub fn engine_stopped(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("엔진이 멈췄습니다: {detail}"),
            Language::En => format!("The engine stopped: {detail}"),
        }
    }

    #[doc = "드라이버는 있지만 독점 정책이 적용되지 않았습니다. 복구를 실행하십시오."]
    pub fn exclusive_policy_missing(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "드라이버는 있지만 독점 정책이 적용되지 않았습니다. 복구를 실행하십시오."
            }
            Language::En => {
                "The driver is installed, but its exclusive-mode policy is missing. Run repair."
            }
        }
    }

    #[doc = "출력 음량 조절 범위는 -20 dB에서 +10 dB까지입니다."]
    pub fn gain_out_of_range(language: Language) -> &'static str {
        match language {
            Language::Ko => "출력 음량 조절 범위는 -20 dB에서 +10 dB까지입니다.",
            Language::En => "Output volume adjustment must be between -20 dB and +10 dB.",
        }
    }

    #[doc = "먼저 BRIR 세트를 가져오십시오."]
    pub fn import_brir_first(language: Language) -> &'static str {
        match language {
            Language::Ko => "먼저 BRIR 세트를 가져오십시오.",
            Language::En => "Import a BRIR set first.",
        }
    }

    #[doc = "hesuvi.wav, hrir.wav 또는 방향별 WAV 폴더를 고르십시오."]
    pub fn invalid_brir_source(language: Language) -> &'static str {
        match language {
            Language::Ko => "hesuvi.wav, hrir.wav 또는 방향별 WAV 폴더를 고르십시오.",
            Language::En => "Choose hesuvi.wav, hrir.wav, or a folder of per-direction WAV files.",
        }
    }

    #[doc = "고른 출력 장치가 독점 모드에서 {layout} 배치({channels}채널)를 지원하지 않습니다. 다른 배치나 출력 장치를 고르십시오."]
    pub fn layout_unsupported(
        language: Language,
        layout: impl core::fmt::Display,
        channels: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!(
                "고른 출력 장치가 독점 모드에서 {layout} 배치({channels}채널)를 지원하지 않습니다. 다른 배치나 출력 장치를 고르십시오."
            ),
            Language::En => format!(
                "The selected output device does not support the {layout} layout ({channels} channels) in exclusive mode. Choose another layout or output device."
            ),
        }
    }

    #[doc = "게임과 일반 앱의 소리를 받는 가상 오디오 장치를 찾을 수 없습니다. 드라이버 상태를 확인하십시오."]
    pub fn main_endpoint_missing(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "게임과 일반 앱의 소리를 받는 가상 오디오 장치를 찾을 수 없습니다. 드라이버 상태를 확인하십시오."
            }
            Language::En => {
                "The virtual audio device for games and other apps could not be found. Check the driver status."
            }
        }
    }

    #[doc = "지금은 할 수 없는 동작입니다."]
    pub fn not_allowed(language: Language) -> &'static str {
        match language {
            Language::Ko => "지금은 할 수 없는 동작입니다.",
            Language::En => "That action is not available right now.",
        }
    }

    #[doc = "Windows 빌드 {min_build} 이상이 필요합니다."]
    pub fn os_requirement(language: Language, min_build: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("Windows 빌드 {min_build} 이상이 필요합니다."),
            Language::En => format!("Windows build {min_build} or newer is required."),
        }
    }

    #[doc = "Windows 10 2004(빌드 19041) 이상이 필요합니다."]
    pub fn os_requirement_19041(language: Language) -> &'static str {
        match language {
            Language::Ko => "Windows 10 2004(빌드 19041) 이상이 필요합니다.",
            Language::En => "Windows 10 version 2004 (build 19041) or newer is required.",
        }
    }

    #[doc = "출력 장치와의 연결이 끊어졌습니다: {detail}"]
    pub fn output_disconnected(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("출력 장치와의 연결이 끊어졌습니다: {detail}"),
            Language::En => format!("The output device disconnected: {detail}"),
        }
    }

    #[doc = "출력 장치의 식별자가 바뀌어 이름으로 다시 찾았습니다: {name}"]
    pub fn output_id_changed(language: Language, name: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("출력 장치의 식별자가 바뀌어 이름으로 다시 찾았습니다: {name}"),
            Language::En => format!(
                "The output device identifier changed, so it was found again by name: {name}"
            ),
        }
    }

    #[doc = "Spatial SoftReceiver 자신을 출력으로 고를 수 없습니다."]
    pub fn own_output_chosen(language: Language) -> &'static str {
        match language {
            Language::Ko => "Spatial SoftReceiver 자신을 출력으로 고를 수 없습니다.",
            Language::En => "Spatial SoftReceiver cannot use itself as its output.",
        }
    }

    #[doc = "업믹스 플러그인이 10분 안에 세 번 멈춰 이번 실행에서는 껐습니다: {name}"]
    pub fn plugin_disabled(language: Language, name: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => {
                format!("업믹스 플러그인이 10분 안에 세 번 멈춰 이번 실행에서는 껐습니다: {name}")
            }
            Language::En => format!(
                "The upmixer plugin stopped three times within 10 minutes, so it is off until the app restarts: {name}"
            ),
        }
    }

    #[doc = "이 플러그인은 업믹서가 아닙니다: {name}"]
    pub fn plugin_not_upmixer(language: Language, name: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("이 플러그인은 업믹서가 아닙니다: {name}"),
            Language::En => format!("This plugin is not an upmixer: {name}"),
        }
    }

    #[doc = "업믹스 플러그인을 읽지 못했습니다: {detail}"]
    pub fn plugin_scan_failed(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("업믹스 플러그인을 읽지 못했습니다: {detail}"),
            Language::En => format!("The upmixer plugin could not be read: {detail}"),
        }
    }

    #[doc = "profile.toml을 읽지 못했습니다: {detail}"]
    pub fn profile_file_read_failed(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("profile.toml을 읽지 못했습니다: {detail}"),
            Language::En => format!("profile.toml could not be read: {detail}"),
        }
    }

    #[doc = "프로필을 준비하지 못했습니다: {detail}"]
    pub fn profile_prepare_failed(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("프로필을 준비하지 못했습니다: {detail}"),
            Language::En => format!("The profile could not be prepared: {detail}"),
        }
    }

    #[doc = "드라이버 설치를 마치려면 다시 시작해야 합니다."]
    pub fn reboot_required(language: Language) -> &'static str {
        match language {
            Language::Ko => "드라이버 설치를 마치려면 다시 시작해야 합니다.",
            Language::En => "Restart Windows to finish installing the driver.",
        }
    }

    #[doc = "엔진이 {seconds}초 동안 다시 연결하지 못했습니다. 캡처 장치와의 연결이 끊어졌습니다."]
    pub fn reconnect_timed_out_capture(
        language: Language,
        seconds: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!(
                "엔진이 {seconds}초 동안 다시 연결하지 못했습니다. 캡처 장치와의 연결이 끊어졌습니다."
            ),
            Language::En => format!(
                "The engine could not reconnect for {seconds} seconds. The capture device disconnected."
            ),
        }
    }

    #[doc = "엔진이 {seconds}초 동안 다시 연결하지 못했습니다. 출력 장치와의 연결이 끊어졌습니다: {detail}"]
    pub fn reconnect_timed_out_output(
        language: Language,
        seconds: impl core::fmt::Display,
        detail: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!(
                "엔진이 {seconds}초 동안 다시 연결하지 못했습니다. 출력 장치와의 연결이 끊어졌습니다: {detail}"
            ),
            Language::En => format!(
                "The engine could not reconnect for {seconds} seconds. The output device disconnected: {detail}"
            ),
        }
    }

    #[doc = "다시 연결했습니다."]
    pub fn reconnected(language: Language) -> &'static str {
        match language {
            Language::Ko => "다시 연결했습니다.",
            Language::En => "Reconnected.",
        }
    }

    #[doc = "이전 기본 장치({role})를 찾을 수 없어 고른 출력으로 되돌렸습니다: {name}"]
    pub fn restore_fallback(
        language: Language,
        role: impl core::fmt::Display,
        name: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => {
                format!("이전 기본 장치({role})를 찾을 수 없어 고른 출력으로 되돌렸습니다: {name}")
            }
            Language::En => format!(
                "The previous default {role} device could not be found, so the selected output was restored: {name}"
            ),
        }
    }

    #[doc = "이전 기본 장치({role})를 찾을 수 없어 Windows가 고르게 둡니다."]
    pub fn restore_left_to_windows(language: Language, role: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => {
                format!("이전 기본 장치({role})를 찾을 수 없어 Windows가 고르게 둡니다.")
            }
            Language::En => format!(
                "The previous default {role} device could not be found, so Windows will choose one."
            ),
        }
    }

    #[doc = "기본 장치를 되돌리기 위해 저장한 정보를 지우지 못했습니다: {detail}"]
    pub fn restore_point_remove_failed(
        language: Language,
        detail: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => {
                format!("기본 장치를 되돌리기 위해 저장한 정보를 지우지 못했습니다: {detail}")
            }
            Language::En => format!(
                "The saved information for restoring the default devices could not be removed: {detail}"
            ),
        }
    }

    #[doc = "기본 장치({role})를 되돌리지 못했습니다: {detail}"]
    pub fn restore_role_failed(
        language: Language,
        role: impl core::fmt::Display,
        detail: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!("기본 장치({role})를 되돌리지 못했습니다: {detail}"),
            Language::En => format!("The default {role} device could not be restored: {detail}"),
        }
    }

    #[doc = "기본 장치를 바꾸지 못했습니다: {detail}"]
    pub fn role_change_failed(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("기본 장치를 바꾸지 못했습니다: {detail}"),
            Language::En => format!("The default device could not be changed: {detail}"),
        }
    }

    #[doc = "통신"]
    pub fn role_communications(language: Language) -> &'static str {
        match language {
            Language::Ko => "통신",
            Language::En => "communications",
        }
    }

    #[doc = "콘솔"]
    pub fn role_console(language: Language) -> &'static str {
        match language {
            Language::Ko => "콘솔",
            Language::En => "console",
        }
    }

    #[doc = "멀티미디어"]
    pub fn role_multimedia(language: Language) -> &'static str {
        match language {
            Language::Ko => "멀티미디어",
            Language::En => "multimedia",
        }
    }

    #[doc = "설치 도우미: {detail}"]
    pub fn setup_helper(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("설치 도우미: {detail}"),
            Language::En => format!("Setup helper: {detail}"),
        }
    }

    #[doc = "저장된 출력 장치를 찾을 수 없습니다: {name}"]
    pub fn stored_output_missing(language: Language, name: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("저장된 출력 장치를 찾을 수 없습니다: {name}"),
            Language::En => format!("The saved output device could not be found: {name}"),
        }
    }

    #[doc = "시험서명 모드가 꺼져 있습니다. Secure Boot를 끈 뒤 관리자 명령으로 bcdedit /set testsigning on을 실행하고 다시 시작하십시오."]
    pub fn test_signing_disabled(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "시험서명 모드가 꺼져 있습니다. Secure Boot를 끈 뒤 관리자 명령으로 bcdedit /set testsigning on을 실행하고 다시 시작하십시오."
            }
            Language::En => {
                "Test-signing mode is off. Turn off Secure Boot, run bcdedit /set testsigning on as administrator, and restart Windows."
            }
        }
    }

    #[doc = " Secure Boot가 켜져 있어 먼저 꺼야 합니다."]
    pub fn test_signing_secure_boot(language: Language) -> &'static str {
        match language {
            Language::Ko => " Secure Boot가 켜져 있어 먼저 꺼야 합니다.",
            Language::En => " Secure Boot is on and must be turned off first.",
        }
    }

    #[doc = "앱 자체 업믹서는 아직 제공되지 않습니다."]
    pub fn upmixer_unavailable(language: Language) -> &'static str {
        match language {
            Language::Ko => "앱 자체 업믹서는 아직 제공되지 않습니다.",
            Language::En => "The app's own upmixer is not available yet.",
        }
    }
}

pub mod guard {
    use super::Language;

    #[doc = "지난번에 앱이 예기치 않게 종료되어 기본 오디오 장치를 되돌렸습니다."]
    pub fn marker_app_exited(language: Language) -> &'static str {
        match language {
            Language::Ko => "지난번에 앱이 예기치 않게 종료되어 기본 오디오 장치를 되돌렸습니다.",
            Language::En => {
                "The app exited unexpectedly. The default audio devices have been restored."
            }
        }
    }

    #[doc = "다시 시작한 뒤에도 소리 처리가 멈춰 기본 오디오 장치를 되돌렸습니다."]
    pub fn marker_restart_failed(language: Language) -> &'static str {
        match language {
            Language::Ko => "다시 시작한 뒤에도 소리 처리가 멈춰 기본 오디오 장치를 되돌렸습니다.",
            Language::En => {
                "Audio processing stopped responding after restarting. The default audio devices have been restored."
            }
        }
    }

    #[doc = "소리 처리가 멈춰 앱을 다시 시작했습니다."]
    pub fn marker_stall_restarted(language: Language) -> &'static str {
        match language {
            Language::Ko => "소리 처리가 멈춰 앱을 다시 시작했습니다.",
            Language::En => "Audio processing stopped responding, so the app was restarted.",
        }
    }

    #[doc = "앱을 기본 오디오 장치로 사용하던 중 응답이 없어 이전 장치로 되돌렸습니다."]
    pub fn marker_stuck(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "앱을 기본 오디오 장치로 사용하던 중 응답이 없어 이전 장치로 되돌렸습니다."
            }
            Language::En => {
                "The app stopped responding while it was the default audio device. The previous devices have been restored."
            }
        }
    }

    #[doc = "기본 장치를 되돌리기 위해 저장한 정보를 읽지 못했습니다: {error}"]
    pub fn restore_point_read_failed(language: Language, error: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => {
                format!("기본 장치를 되돌리기 위해 저장한 정보를 읽지 못했습니다: {error}")
            }
            Language::En => format!(
                "Could not read the saved information for restoring the default devices: {error}"
            ),
        }
    }
}

pub mod shell {
    use super::Language;

    #[doc = "앱이 종료되는 중입니다."]
    pub fn quitting(language: Language) -> &'static str {
        match language {
            Language::Ko => "앱이 종료되는 중입니다.",
            Language::En => "The app is shutting down.",
        }
    }

    #[doc = "지난 실행의 기본 장치를 되돌리지 못했습니다."]
    pub fn restore_failed(language: Language) -> &'static str {
        match language {
            Language::Ko => "지난 실행의 기본 장치를 되돌리지 못했습니다.",
            Language::En => "Could not restore the default audio devices from the previous run.",
        }
    }

    #[doc = "기본 장치를 되돌리기 위해 저장한 정보를 읽지 못했습니다: {detail}"]
    pub fn restore_point_read_failed(
        language: Language,
        detail: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => {
                format!("기본 장치를 되돌리기 위해 저장한 정보를 읽지 못했습니다: {detail}")
            }
            Language::En => format!(
                "Could not read the saved information for restoring the default devices: {detail}"
            ),
        }
    }

    #[doc = "열기"]
    pub fn tray_open(language: Language) -> &'static str {
        match language {
            Language::Ko => "열기",
            Language::En => "Open",
        }
    }

    #[doc = "종료"]
    pub fn tray_quit(language: Language) -> &'static str {
        match language {
            Language::Ko => "종료",
            Language::En => "Quit",
        }
    }

    #[doc = "업데이트를 적용하지 못했습니다: {detail}"]
    pub fn update_apply_failed(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("업데이트를 적용하지 못했습니다: {detail}"),
            Language::En => format!("Could not apply the update: {detail}"),
        }
    }

    #[doc = "받아 둔 업데이트가 없습니다."]
    pub fn update_none_downloaded(language: Language) -> &'static str {
        match language {
            Language::Ko => "받아 둔 업데이트가 없습니다.",
            Language::En => "There is no downloaded update.",
        }
    }

    #[doc = "이 버전에는 업데이트 기능이 없습니다."]
    pub fn update_not_built(language: Language) -> &'static str {
        match language {
            Language::Ko => "이 버전에는 업데이트 기능이 없습니다.",
            Language::En => "This version has no update feature.",
        }
    }
}

pub mod cli {
    use super::Language;

    #[doc = "인수:"]
    pub fn arguments_heading(language: Language) -> &'static str {
        match language {
            Language::Ko => "인수:",
            Language::En => "Arguments:",
        }
    }

    #[doc = "명령:"]
    pub fn commands_heading(language: Language) -> &'static str {
        match language {
            Language::Ko => "명령:",
            Language::En => "Commands:",
        }
    }

    #[doc = "[기본값: {values}]"]
    pub fn default_value(language: Language, values: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("[기본값: {values}]"),
            Language::En => format!("[default: {values}]"),
        }
    }

    #[doc = "도움말 출력"]
    pub fn help_description(language: Language) -> &'static str {
        match language {
            Language::Ko => "도움말 출력",
            Language::En => "Print help",
        }
    }

    #[doc = "도움말 출력('--help'로 자세히)"]
    pub fn help_description_more(language: Language) -> &'static str {
        match language {
            Language::Ko => "도움말 출력('--help'로 자세히)",
            Language::En => "Print help (see more with '--help')",
        }
    }

    #[doc = "도움말 출력('-h'로 요약)"]
    pub fn help_description_summary(language: Language) -> &'static str {
        match language {
            Language::Ko => "도움말 출력('-h'로 요약)",
            Language::En => "Print help (see a summary with '-h')",
        }
    }

    #[doc = "이 도움말이나 고른 명령의 도움말을 출력합니다."]
    pub fn help_subcommand_about(language: Language) -> &'static str {
        match language {
            Language::Ko => "이 도움말이나 고른 명령의 도움말을 출력합니다.",
            Language::En => "Print this message or the help of the given subcommand(s)",
        }
    }

    #[doc = "도움말을 볼 명령"]
    pub fn help_subcommand_arg(language: Language) -> &'static str {
        match language {
            Language::Ko => "도움말을 볼 명령",
            Language::En => "Print help for the subcommand(s)",
        }
    }

    #[doc = "옵션:"]
    pub fn options_heading(language: Language) -> &'static str {
        match language {
            Language::Ko => "옵션:",
            Language::En => "Options:",
        }
    }

    #[doc = "명령줄을 읽지 못했습니다. --help로 사용법을 보십시오."]
    pub fn parse_error(language: Language) -> &'static str {
        match language {
            Language::Ko => "명령줄을 읽지 못했습니다. --help로 사용법을 보십시오.",
            Language::En => "Could not read the command line. See --help for usage.",
        }
    }

    #[doc = "[가능한 값: {values}]"]
    pub fn possible_values(language: Language, values: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("[가능한 값: {values}]"),
            Language::En => format!("[possible values: {values}]"),
        }
    }

    #[doc = "가능한 값:"]
    pub fn possible_values_heading(language: Language) -> &'static str {
        match language {
            Language::Ko => "가능한 값:",
            Language::En => "Possible values:",
        }
    }

    #[doc = "사용법:"]
    pub fn usage_heading(language: Language) -> &'static str {
        match language {
            Language::Ko => "사용법:",
            Language::En => "Usage:",
        }
    }

    #[doc = "버전 출력"]
    pub fn version_description(language: Language) -> &'static str {
        match language {
            Language::Ko => "버전 출력",
            Language::En => "Print version",
        }
    }
}

pub mod live {
    use super::Language;

    #[doc = "SAL 개발 PC에서 쓰는 공간 소프트리시버 실시간 도구"]
    pub fn about(language: Language) -> &'static str {
        match language {
            Language::Ko => "SAL 개발 PC에서 쓰는 공간 소프트리시버 실시간 도구",
            Language::En => "Spatial SoftReceiver live tool for the SAL development PC",
        }
    }

    #[doc = "{name} 캡처 장치에 접근할 수 없습니다. 관리자 명령 프롬프트에서 실행하십시오"]
    pub fn capture_access_denied(language: Language, name: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!(
                "{name} 캡처 장치에 접근할 수 없습니다. 관리자 명령 프롬프트에서 실행하십시오"
            ),
            Language::En => format!(
                "access to the {name} capture device was denied; run from an administrator prompt"
            ),
        }
    }

    #[doc = "{name} 캡처 장치가 없습니다"]
    pub fn capture_not_present(language: Language, name: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("{name} 캡처 장치가 없습니다"),
            Language::En => format!("the {name} capture device is not present"),
        }
    }

    #[doc = "  체인 오류 {count}개"]
    pub fn chain_errors(language: Language, count: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("  체인 오류 {count}개"),
            Language::En => format!("  chain errors {count}"),
        }
    }

    #[doc = "    {channels}채널 마스크 {mask}: 독점 {exclusive}"]
    pub fn device_layout(
        language: Language,
        channels: impl core::fmt::Display,
        mask: impl core::fmt::Display,
        exclusive: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!("    {channels}채널 마스크 {mask}: 독점 {exclusive}"),
            Language::En => format!("    {channels} channels mask {mask}: exclusive {exclusive}"),
        }
    }

    #[doc = "    믹스: {rate} Hz, {channels}채널, 마스크 {mask}"]
    pub fn device_mix(
        language: Language,
        rate: impl core::fmt::Display,
        channels: impl core::fmt::Display,
        mask: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!("    믹스: {rate} Hz, {channels}채널, 마스크 {mask}"),
            Language::En => format!("    mix: {rate} Hz, {channels} channels, mask {mask}"),
        }
    }

    #[doc = "렌더 엔드포인트(이름, 기본 장치 여부, 식별자)를 나열합니다."]
    pub fn devices_about(language: Language) -> &'static str {
        match language {
            Language::Ko => "렌더 엔드포인트(이름, 기본 장치 여부, 식별자)를 나열합니다.",
            Language::En => "List render endpoints (name, default flag, id)",
        }
    }

    #[doc = "공유 믹스 형식과 표준 배치의 독점 모드 지원 여부를 함께 표시합니다."]
    pub fn devices_formats_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "공유 믹스 형식과 표준 배치의 독점 모드 지원 여부를 함께 표시합니다.",
            Language::En => "Include shared mix format and standard-layout exclusive support",
        }
    }

    #[doc = "기계가 읽는 JSON으로 출력합니다."]
    pub fn devices_json_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "기계가 읽는 JSON으로 출력합니다.",
            Language::En => "Emit machine-readable JSON",
        }
    }

    #[doc = "  엔드포인트 {endpoint}: 코덱 [{codecs}]"]
    pub fn driver_endpoint(
        language: Language,
        endpoint: impl core::fmt::Display,
        codecs: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!("  엔드포인트 {endpoint}: 코덱 [{codecs}]"),
            Language::En => format!("  endpoint {endpoint}: codecs [{codecs}]"),
        }
    }

    #[doc = "공간 소프트리시버 드라이버: {detail}"]
    pub fn driver_error(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("공간 소프트리시버 드라이버: {detail}"),
            Language::En => format!("Spatial SoftReceiver driver: {detail}"),
        }
    }

    #[doc = "공간 소프트리시버 드라이버: 엔드포인트 마스크 0x{mask}, QPC {qpc} Hz"]
    pub fn driver_info(
        language: Language,
        mask: impl core::fmt::Display,
        qpc: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => {
                format!("공간 소프트리시버 드라이버: 엔드포인트 마스크 0x{mask}, QPC {qpc} Hz")
            }
            Language::En => {
                format!("Spatial SoftReceiver driver: endpoint mask 0x{mask}, QPC {qpc} Hz")
            }
        }
    }

    #[doc = "공간 소프트리시버 드라이버: 없음"]
    pub fn driver_missing(language: Language) -> &'static str {
        match language {
            Language::Ko => "공간 소프트리시버 드라이버: 없음",
            Language::En => "Spatial SoftReceiver driver: not present",
        }
    }

    #[doc = "    PCM: 표본율 [{rates}] Hz; 모양 [{shapes}]; 마스크 채널 <= {masked}; 직접 출력 채널 [{direct}]"]
    pub fn driver_pcm(
        language: Language,
        rates: impl core::fmt::Display,
        shapes: impl core::fmt::Display,
        masked: impl core::fmt::Display,
        direct: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!(
                "    PCM: 표본율 [{rates}] Hz; 모양 [{shapes}]; 마스크 채널 <= {masked}; 직접 출력 채널 [{direct}]"
            ),
            Language::En => format!(
                "    PCM: rates [{rates}] Hz; shapes [{shapes}]; masked channels <= {masked}; direct-out channels [{direct}]"
            ),
        }
    }

    #[doc = "덤프가 끝났습니다"]
    pub fn dump_ended(language: Language) -> &'static str {
        match language {
            Language::Ko => "덤프가 끝났습니다",
            Language::En => "the dump has ended",
        }
    }

    #[doc = "엔드포인트 {endpoint}이 두 번 이상 나옵니다"]
    pub fn endpoint_duplicate(language: Language, endpoint: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("엔드포인트 {endpoint}이 두 번 이상 나옵니다"),
            Language::En => format!("endpoint {endpoint} is listed more than once"),
        }
    }

    #[doc = "--endpoint-mask는 set-formats에만 쓸 수 있습니다"]
    pub fn endpoint_mask_only(language: Language) -> &'static str {
        match language {
            Language::Ko => "--endpoint-mask는 set-formats에만 쓸 수 있습니다",
            Language::En => "--endpoint-mask belongs to set-formats only",
        }
    }

    #[doc = "set-formats에는 --endpoint-mask가 필요합니다"]
    pub fn endpoint_mask_required(language: Language) -> &'static str {
        match language {
            Language::Ko => "set-formats에는 --endpoint-mask가 필요합니다",
            Language::En => "set-formats needs --endpoint-mask",
        }
    }

    #[doc = "엔드포인트 {endpoint}은 0..{last} 범위 밖입니다"]
    pub fn endpoint_out_of_range(
        language: Language,
        endpoint: impl core::fmt::Display,
        last: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!("엔드포인트 {endpoint}은 0..{last} 범위 밖입니다"),
            Language::En => format!("endpoint {endpoint} is outside 0..{last}"),
        }
    }

    #[doc = "--endpoints에는 엔드포인트 번호가 하나 이상 있어야 합니다"]
    pub fn endpoints_empty(language: Language) -> &'static str {
        match language {
            Language::Ko => "--endpoints에는 엔드포인트 번호가 하나 이상 있어야 합니다",
            Language::En => "--endpoints requires at least one endpoint number",
        }
    }

    #[doc = "ssr-live: {detail}"]
    pub fn error_prefix(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("ssr-live: {detail}"),
            Language::En => format!("ssr-live: {detail}"),
        }
    }

    #[doc = "결함: {detail}"]
    pub fn fault(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("결함: {detail}"),
            Language::En => format!("fault: {detail}"),
        }
    }

    #[doc = "결함이 사라졌습니다"]
    pub fn fault_cleared(language: Language) -> &'static str {
        match language {
            Language::Ko => "결함이 사라졌습니다",
            Language::En => "fault cleared",
        }
    }

    #[doc = "  결함 {detail}"]
    pub fn fault_suffix(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("  결함 {detail}"),
            Language::En => format!("  FAULT {detail}"),
        }
    }

    #[doc = "엔드포인트 {endpoint} 에포크 {epoch}: {format}"]
    pub fn format_changed(
        language: Language,
        endpoint: impl core::fmt::Display,
        epoch: impl core::fmt::Display,
        format: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!("엔드포인트 {endpoint} 에포크 {epoch}: {format}"),
            Language::En => format!("endpoint {endpoint} epoch {epoch}: {format}"),
        }
    }

    #[doc = "엔드포인트 {endpoint} 에포크 {epoch}: {format} ({reason})"]
    pub fn format_unsupported(
        language: Language,
        endpoint: impl core::fmt::Display,
        epoch: impl core::fmt::Display,
        format: impl core::fmt::Display,
        reason: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!("엔드포인트 {endpoint} 에포크 {epoch}: {format} ({reason})"),
            Language::En => format!("endpoint {endpoint} epoch {epoch}: {format} ({reason})"),
        }
    }

    #[doc = "Windows 호스트 어댑터를 조회하고 제어하며 JSON 문서 하나를 출력합니다."]
    pub fn host_about(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "Windows 호스트 어댑터를 조회하고 제어하며 JSON 문서 하나를 출력합니다."
            }
            Language::En => "Query and drive the Windows host adapter; prints one JSON document",
        }
    }

    #[doc = "set-formats에서 쓰는 드라이버 EndpointMask입니다. n번 비트가 n번 엔드포인트를 켭니다(1~15)."]
    pub fn host_endpoint_mask_help(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "set-formats에서 쓰는 드라이버 EndpointMask입니다. n번 비트가 n번 엔드포인트를 켭니다(1~15)."
            }
            Language::En => {
                "With set-formats: the driver's EndpointMask, bit n enables endpoint n (1-15)"
            }
        }
    }

    #[doc = "`host show`가 출력한 엔드포인트 식별자입니다."]
    pub fn host_id_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "`host show`가 출력한 엔드포인트 식별자입니다.",
            Language::En => "The endpoint id, as `host show` prints it",
        }
    }

    #[doc = "드라이버 묶음 폴더입니다(spatialsoftreceiver.inf, 카탈로그와 드라이버, 시험 인증서, ssr-setup.exe)."]
    pub fn host_package_dir_help(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "드라이버 묶음 폴더입니다(spatialsoftreceiver.inf, 카탈로그와 드라이버, 시험 인증서, ssr-setup.exe)."
            }
            Language::En => {
                "The driver package directory (spatialsoftreceiver.inf, its catalog and driver, the test certificate, and ssr-setup.exe)"
            }
        }
    }

    #[doc = "기본 장치를 바꿀 역할입니다."]
    pub fn host_role_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "기본 장치를 바꿀 역할입니다.",
            Language::En => "The role whose default device changes",
        }
    }

    #[doc = "렌더 엔드포인트 하나를 역할 하나의 기본 장치로 정합니다."]
    pub fn host_set_default_about(language: Language) -> &'static str {
        match language {
            Language::Ko => "렌더 엔드포인트 하나를 역할 하나의 기본 장치로 정합니다.",
            Language::En => "Make a render endpoint the default device of one role",
        }
    }

    #[doc = "관리자 권한으로 ssr-setup 작업 하나를 실행합니다(UAC 확인 한 번)."]
    pub fn host_setup_about(language: Language) -> &'static str {
        match language {
            Language::Ko => "관리자 권한으로 ssr-setup 작업 하나를 실행합니다(UAC 확인 한 번).",
            Language::En => {
                "Run one administrator task through an elevated ssr-setup (one UAC prompt)"
            }
        }
    }

    #[doc = "관리자 권한으로 ssr-setup 작업 하나를 실행합니다(UAC 확인 한 번).\n\n--package-dir 폴더에 ssr-setup.exe가 있으면 그것을, 없으면 ssr-live.exe 옆의 것을 실행합니다. install과 repair는 실행하는 ssr-setup.exe와 같은 폴더의 spatialsoftreceiver.inf와 spatialsoftreceiver-test.cer를 씁니다. ssr-live는 아무것도 복사하지 않으므로 ssr-setup.exe를 먼저 묶음 폴더에 넣으십시오(CI 단계는 이 명령을 부르기 전에 실행 파일을 묶음 폴더에 복사합니다)."]
    pub fn host_setup_long_about(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "관리자 권한으로 ssr-setup 작업 하나를 실행합니다(UAC 확인 한 번).\n\n--package-dir 폴더에 ssr-setup.exe가 있으면 그것을, 없으면 ssr-live.exe 옆의 것을 실행합니다. install과 repair는 실행하는 ssr-setup.exe와 같은 폴더의 spatialsoftreceiver.inf와 spatialsoftreceiver-test.cer를 씁니다. ssr-live는 아무것도 복사하지 않으므로 ssr-setup.exe를 먼저 묶음 폴더에 넣으십시오(CI 단계는 이 명령을 부르기 전에 실행 파일을 묶음 폴더에 복사합니다)."
            }
            Language::En => {
                "Run one administrator task through an elevated ssr-setup (one UAC prompt).\n\nssr-setup.exe runs from --package-dir when that directory holds one, and otherwise from beside ssr-live.exe; install and repair take spatialsoftreceiver.inf and spatialsoftreceiver-test.cer from the directory of the ssr-setup.exe that runs. ssr-live copies nothing: put ssr-setup.exe into the package directory first (the CI step copies the binaries into the kit directory before it calls this)."
            }
        }
    }

    #[doc = "실행할 관리자 작업입니다."]
    pub fn host_setup_task_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "실행할 관리자 작업입니다.",
            Language::En => "Administrator task to run",
        }
    }

    #[doc = "묶음의 시험 인증서를 신뢰하고 루트 장치를 만든 뒤 드라이버를 설치하고 엔드포인트 독점 정책을 적용합니다."]
    pub fn host_setup_task_install(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "묶음의 시험 인증서를 신뢰하고 루트 장치를 만든 뒤 드라이버를 설치하고 엔드포인트 독점 정책을 적용합니다."
            }
            Language::En => {
                "Trust the package's test certificate, create the root device, install the driver and apply the endpoints' exclusive-mode policy"
            }
        }
    }

    #[doc = "드라이버 엔드포인트 마스크(--endpoint-mask)를 정하고 장치를 다시 시작한 뒤 켜진 엔드포인트에 독점 정책을 적용합니다."]
    pub fn host_setup_task_set_formats(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "드라이버 엔드포인트 마스크(--endpoint-mask)를 정하고 장치를 다시 시작한 뒤 켜진 엔드포인트에 독점 정책을 적용합니다."
            }
            Language::En => {
                "Set the driver's endpoint mask (--endpoint-mask), restart the device and apply the exclusive-mode policy to the endpoints it enables"
            }
        }
    }

    #[doc = "현재·이전 장치, 드라이버 묶음, 남은 서비스와 시험 인증서를 지웁니다."]
    pub fn host_setup_task_uninstall(language: Language) -> &'static str {
        match language {
            Language::Ko => "현재·이전 장치, 드라이버 묶음, 남은 서비스와 시험 인증서를 지웁니다.",
            Language::En => {
                "Remove the current and legacy devices, driver packages, orphan services and test certificates"
            }
        }
    }

    #[doc = "플랫폼, 드라이버 상태, 모든 렌더 엔드포인트(우리 것은 번호 포함), 각 역할의 기본 장치를 출력합니다."]
    pub fn host_show_about(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "플랫폼, 드라이버 상태, 모든 렌더 엔드포인트(우리 것은 번호 포함), 각 역할의 기본 장치를 출력합니다."
            }
            Language::En => {
                "Print the platform, the driver state, every render endpoint (ours with its endpoint number) and the default device of each role"
            }
        }
    }

    #[doc = "ssr-setup 결과 파일과 복원점을 둘 폴더입니다[기본값: %TEMP%\\ssr-live-host]."]
    pub fn host_state_dir_help(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "ssr-setup 결과 파일과 복원점을 둘 폴더입니다[기본값: %TEMP%\\ssr-live-host]."
            }
            Language::En => {
                "Directory for ssr-setup's result file and the restore point [default: %TEMP%\\ssr-live-host]"
            }
        }
    }

    #[doc = "엔드포인트 번호가 올바르지 않습니다: {value}"]
    pub fn invalid_endpoint(language: Language, value: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("엔드포인트 번호가 올바르지 않습니다: {value}"),
            Language::En => format!("invalid endpoint number {value}"),
        }
    }

    #[doc = "ssr-live.exe 위치를 찾지 못했습니다: {detail}"]
    pub fn locate_exe_failed(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("ssr-live.exe 위치를 찾지 못했습니다: {detail}"),
            Language::En => format!("cannot locate ssr-live.exe: {detail}"),
        }
    }

    #[doc = "아니요"]
    pub fn no(language: Language) -> &'static str {
        match language {
            Language::Ko => "아니요",
            Language::En => "no",
        }
    }

    #[doc = "데이터가 오지 않았습니다. 기록하는 동안 홈시어터용 Dolby Atmos 게임이나 Apple Music을 SAL 엔드포인트로 재생하십시오"]
    pub fn no_data(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "데이터가 오지 않았습니다. 기록하는 동안 홈시어터용 Dolby Atmos 게임이나 Apple Music을 SAL 엔드포인트로 재생하십시오"
            }
            Language::En => {
                "no data arrived: play a game with Dolby Atmos for home theater, or Apple Music, to the SAL endpoint while recording"
            }
        }
    }

    #[doc = "출력: {name}"]
    pub fn output(language: Language, name: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("출력: {name}"),
            Language::En => format!("output: {name}"),
        }
    }

    #[doc = "출력 주기 결함: {ratio}"]
    pub fn output_cadence_fault(language: Language, ratio: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("출력 주기 결함: {ratio}"),
            Language::En => format!("output cadence fault: {ratio}"),
        }
    }

    #[doc = "출력 주기로 다시 열기: {period} ms"]
    pub fn output_cadence_reopen(language: Language, period: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("출력 주기로 다시 열기: {period} ms"),
            Language::En => format!("output cadence reopen: {period} ms"),
        }
    }

    #[doc = "출력 넘침: 렌더 프레임 {frames}개 손실"]
    pub fn output_overflow(language: Language, frames: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("출력 넘침: 렌더 프레임 {frames}개 손실"),
            Language::En => format!("output overflow: {frames} rendered frames lost"),
        }
    }

    #[doc = "출력을 다시 연결했습니다"]
    pub fn output_reconnected(language: Language) -> &'static str {
        match language {
            Language::Ko => "출력을 다시 연결했습니다",
            Language::En => "output reconnected",
        }
    }

    #[doc = "출력을 다시 열었습니다: {channels}채널, 마스크 {mask}"]
    pub fn output_reopened(
        language: Language,
        channels: impl core::fmt::Display,
        mask: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!("출력을 다시 열었습니다: {channels}채널, 마스크 {mask}"),
            Language::En => format!("output reopened: {channels} channels, mask {mask}"),
        }
    }

    #[doc = "넘침: {lost}바이트 손실"]
    pub fn overrun(language: Language, lost: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("넘침: {lost}바이트 손실"),
            Language::En => format!("overrun: {lost} bytes lost"),
        }
    }

    #[doc = "ssr-live: {package_dir}에 ssr-setup.exe가 없어 패키지 파일 옆의 {setup}을 실행합니다"]
    pub fn package_setup_fallback(
        language: Language,
        package_dir: impl core::fmt::Display,
        setup: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!(
                "ssr-live: {package_dir}에 ssr-setup.exe가 없어 패키지 파일 옆의 {setup}을 실행합니다"
            ),
            Language::En => format!(
                "ssr-live: {package_dir} holds no ssr-setup.exe; running {setup} with the package files beside it"
            ),
        }
    }

    #[doc = "캡처 링을 BRIR 프로필로 재생합니다(관리자 권한 필요)."]
    pub fn play_about(language: Language) -> &'static str {
        match language {
            Language::Ko => "캡처 링을 BRIR 프로필로 재생합니다(관리자 권한 필요).",
            Language::En => "Play a capture ring through a BRIR profile (run as administrator)",
        }
    }

    #[doc = "독점 모드 요청이 실패하면 WASAPI 공유 모드로 대체하도록 허용합니다."]
    pub fn play_allow_shared_fallback_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "독점 모드 요청이 실패하면 WASAPI 공유 모드로 대체하도록 허용합니다.",
            Language::En => "Permit an exclusive request to fall back to WASAPI shared mode",
        }
    }

    #[doc = "클럭 제어 방식입니다."]
    pub fn play_clock_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "클럭 제어 방식입니다.",
            Language::En => "Clock-control mode",
        }
    }

    #[doc = "마스크가 0인 PCM 순서를 CHANNELS=PRESET 또는 CHANNELS=SPEAKER,SPEAKER,... 형식으로 덮어씁니다."]
    pub fn play_direct_layout_help(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "마스크가 0인 PCM 순서를 CHANNELS=PRESET 또는 CHANNELS=SPEAKER,SPEAKER,... 형식으로 덮어씁니다."
            }
            Language::En => {
                "Override a zero-mask PCM order, as CHANNELS=PRESET or CHANNELS=SPEAKER,SPEAKER,..."
            }
        }
    }

    #[doc = "쓸 엔드포인트입니다. 공개된 엔드포인트 전부 또는 쉼표로 나눈 일부를 고릅니다."]
    pub fn play_endpoints_help(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "쓸 엔드포인트입니다. 공개된 엔드포인트 전부 또는 쉼표로 나눈 일부를 고릅니다."
            }
            Language::En => {
                "Endpoints to consume: all exposed endpoints, or a comma-separated subset"
            }
        }
    }

    #[doc = "SAL 링 대신 덤프 파일을 기록된 속도로 재생하여 SAL이 없는 PC에서도 실시간 경로를 돌립니다. 덤프가 끝나면 멈춥니다."]
    pub fn play_from_dump_help(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "SAL 링 대신 덤프 파일을 기록된 속도로 재생하여 SAL이 없는 PC에서도 실시간 경로를 돌립니다. 덤프가 끝나면 멈춥니다."
            }
            Language::En => {
                "Play a dump file at its recorded pace instead of the SAL ring, to run the live path on a PC without SAL. Stops when the dump ends"
            }
        }
    }

    #[doc = "--from-dump 사용 시 덤프의 데이터 기록을 위치와 시간을 이어 반복합니다. --seconds가 지나거나 Enter를 누르면 멈춥니다."]
    pub fn play_loop_help(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "--from-dump 사용 시 덤프의 데이터 기록을 위치와 시간을 이어 반복합니다. --seconds가 지나거나 Enter를 누르면 멈춥니다."
            }
            Language::En => {
                "With --from-dump: repeat the dump's data records, with continuous positions and times, until --seconds or Enter stops the run"
            }
        }
    }

    #[doc = "모두 실행하되 장치에는 무음을 보냅니다."]
    pub fn play_mute_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "모두 실행하되 장치에는 무음을 보냅니다.",
            Language::En => "Run everything but send silence to the device",
        }
    }

    #[doc = "출력 장치 이름의 일부입니다. 생략하면 기본 장치를 씁니다."]
    pub fn play_output_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "출력 장치 이름의 일부입니다. 생략하면 기본 장치를 씁니다.",
            Language::En => "Part of the output device's name; the default device when omitted",
        }
    }

    #[doc = "장치 주기(ms)입니다. 생략하면 장치의 최솟값을 씁니다."]
    pub fn play_period_ms_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "장치 주기(ms)입니다. 생략하면 장치의 최솟값을 씁니다.",
            Language::En => "Device period in milliseconds; the device's minimum when omitted",
        }
    }

    #[doc = "프로필 TOML입니다(crates/brir 참고)."]
    pub fn play_profile_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "프로필 TOML입니다(crates/brir 참고).",
            Language::En => "Profile TOML (see crates/brir)",
        }
    }

    #[doc = "장치 표본율입니다."]
    pub fn play_rate_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "장치 표본율입니다.",
            Language::En => "Device sample rate",
        }
    }

    #[doc = "캡처한 기록 스트림을 이 덤프 파일에도 씁니다."]
    pub fn play_record_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "캡처한 기록 스트림을 이 덤프 파일에도 씁니다.",
            Language::En => "Also write the captured record stream to this dump file",
        }
    }

    #[doc = "멈출 때 실행 수치(끊김, 재시작, 결함, 지연, 링 최저 수위, 클럭)를 JSON 파일에 씁니다."]
    pub fn play_report_help(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "멈출 때 실행 수치(끊김, 재시작, 결함, 지연, 링 최저 수위, 클럭)를 JSON 파일에 씁니다."
            }
            Language::En => {
                "Write the run's figures (underruns, restarts, faults, latency, ring trough, clock) as JSON to this file when it stops"
            }
        }
    }

    #[doc = "이 시간(초)이 지나면 멈춥니다. 생략하면 Enter를 눌러 멈춥니다."]
    pub fn play_seconds_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "이 시간(초)이 지나면 멈춥니다. 생략하면 Enter를 눌러 멈춥니다.",
            Language::En => "Stop after this many seconds (otherwise press Enter to stop)",
        }
    }

    #[doc = "독점 모드 대신 WASAPI 공유 모드를 씁니다."]
    pub fn play_shared_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "독점 모드 대신 WASAPI 공유 모드를 씁니다.",
            Language::En => "Use WASAPI shared mode instead of exclusive",
        }
    }

    #[doc = "캡처 원본입니다. --from-dump를 쓰면 무시합니다."]
    pub fn play_source_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "캡처 원본입니다. --from-dump를 쓰면 무시합니다.",
            Language::En => "Capture source; ignored when --from-dump supplies the source",
        }
    }

    #[doc = "유지할 교차 링 수위(ms)입니다(게임 10, 음악 40)."]
    pub fn play_target_ms_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "유지할 교차 링 수위(ms)입니다(게임 10, 음악 40).",
            Language::En => {
                "Crossing-ring level to hold, in milliseconds (10 for games, 40 for music)"
            }
        }
    }

    #[doc = "재생 중입니다. 멈추려면 Enter를 누르십시오"]
    pub fn playing(language: Language) -> &'static str {
        match language {
            Language::Ko => "재생 중입니다. 멈추려면 Enter를 누르십시오",
            Language::En => "playing; press Enter to stop",
        }
    }

    #[doc = "변경 대기열이 가득 차 다시 시도합니다"]
    pub fn prepare_retry(language: Language) -> &'static str {
        match language {
            Language::Ko => "변경 대기열이 가득 차 다시 시도합니다",
            Language::En => "change queue full; retrying",
        }
    }

    #[doc = "{kind} {channels}채널 출력을 {rate} Hz로 {millis} ms 만에 준비했습니다"]
    pub fn prepared(
        language: Language,
        kind: impl core::fmt::Display,
        channels: impl core::fmt::Display,
        rate: impl core::fmt::Display,
        millis: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => {
                format!("{kind} {channels}채널 출력을 {rate} Hz로 {millis} ms 만에 준비했습니다")
            }
            Language::En => {
                format!("prepared {kind} {channels} output channels at {rate} Hz in {millis} ms")
            }
        }
    }

    #[doc = "캡처 링(공간 소프트리시버 드라이버 또는 SAL)을 덤프 파일에 기록합니다(관리자 권한 필요)."]
    pub fn record_about(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "캡처 링(공간 소프트리시버 드라이버 또는 SAL)을 덤프 파일에 기록합니다(관리자 권한 필요)."
            }
            Language::En => {
                "Record a capture ring (the Spatial SoftReceiver driver or SAL) to a dump file (run as administrator)"
            }
        }
    }

    #[doc = "{path}에 레코드 {bytes}바이트를 썼습니다(페이로드 {data}바이트, 기록기 손실 {recorder_lost}바이트)"]
    pub fn record_finished(
        language: Language,
        path: impl core::fmt::Display,
        bytes: impl core::fmt::Display,
        data: impl core::fmt::Display,
        recorder_lost: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!(
                "{path}에 레코드 {bytes}바이트를 썼습니다(페이로드 {data}바이트, 기록기 손실 {recorder_lost}바이트)"
            ),
            Language::En => format!(
                "wrote {bytes} bytes of records ({data} payload bytes, {recorder_lost} recorder bytes lost) to {path}"
            ),
        }
    }

    #[doc = "출력 파일(`.ssrdump`)입니다."]
    pub fn record_out_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "출력 파일(`.ssrdump`)입니다.",
            Language::En => "Output file (`.ssrdump`)",
        }
    }

    #[doc = "{seconds}초  엔드포인트 {endpoint}: 페이로드 {data}바이트, 형식 변경 {formats}회, 손실 {lost}바이트"]
    pub fn record_progress(
        language: Language,
        seconds: impl core::fmt::Display,
        endpoint: impl core::fmt::Display,
        data: impl core::fmt::Display,
        formats: impl core::fmt::Display,
        lost: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!(
                "{seconds}초  엔드포인트 {endpoint}: 페이로드 {data}바이트, 형식 변경 {formats}회, 손실 {lost}바이트"
            ),
            Language::En => format!(
                "{seconds} s  endpoint {endpoint}: {data} payload bytes, {formats} format changes, {lost} bytes lost"
            ),
        }
    }

    #[doc = "기록할 시간(초)입니다."]
    pub fn record_seconds_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "기록할 시간(초)입니다.",
            Language::En => "Seconds to record",
        }
    }

    #[doc = "캡처 원본입니다. 보통 공간 소프트리시버 드라이버를 씁니다."]
    pub fn record_source_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "캡처 원본입니다. 보통 공간 소프트리시버 드라이버를 씁니다.",
            Language::En => "Capture source; the Spatial SoftReceiver driver is the normal source",
        }
    }

    #[doc = "{source} 링을 {path}에 {seconds}초 동안 기록합니다. 지금 원본 재생을 시작하십시오"]
    pub fn recording(
        language: Language,
        source: impl core::fmt::Display,
        path: impl core::fmt::Display,
        seconds: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!(
                "{source} 링을 {path}에 {seconds}초 동안 기록합니다. 지금 원본 재생을 시작하십시오"
            ),
            Language::En => format!(
                "recording the {source} ring to {path} for {seconds} s; start the source now"
            ),
        }
    }

    #[doc = "공간 소프트리시버"]
    pub fn source_driver(language: Language) -> &'static str {
        match language {
            Language::Ko => "공간 소프트리시버",
            Language::En => "Spatial SoftReceiver",
        }
    }

    #[doc = "{second}초  지연 p50 {p50} ms p99 {p99} ms  링 최저 {ring} ms  출력 끊김 {underruns}회  입력 공백 +{gap} ms  장치 재시작 {restarts}회  클럭 {clock} ppm  손실 {lost} B  tee 손실 {dropped} B{suffix}{fault}"]
    #[allow(clippy::too_many_arguments)]
    pub fn status(
        language: Language,
        second: impl core::fmt::Display,
        p50: impl core::fmt::Display,
        p99: impl core::fmt::Display,
        ring: impl core::fmt::Display,
        underruns: impl core::fmt::Display,
        gap: impl core::fmt::Display,
        restarts: impl core::fmt::Display,
        clock: impl core::fmt::Display,
        lost: impl core::fmt::Display,
        dropped: impl core::fmt::Display,
        suffix: impl core::fmt::Display,
        fault: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!(
                "{second}초  지연 p50 {p50} ms p99 {p99} ms  링 최저 {ring} ms  출력 끊김 {underruns}회  입력 공백 +{gap} ms  장치 재시작 {restarts}회  클럭 {clock} ppm  손실 {lost} B  tee 손실 {dropped} B{suffix}{fault}"
            ),
            Language::En => format!(
                "{second} s  latency p50 {p50} ms p99 {p99} ms  ring trough {ring} ms  underruns {underruns}  input gap +{gap} ms  device restarts {restarts}  clock {clock} ppm  lost {lost} B  tee-dropped {dropped} B{suffix}{fault}"
            ),
        }
    }

    #[doc = "멈춤: 블록 {blocks}개, 끊김 {underruns}회, 장치 재시작 {restarts}회, 손실 {lost}바이트, 기록기 손실 {recorder_lost}바이트, 지연 p50 {p50} ms p99 {p99} ms"]
    #[allow(clippy::too_many_arguments)]
    pub fn stopped(
        language: Language,
        blocks: impl core::fmt::Display,
        underruns: impl core::fmt::Display,
        restarts: impl core::fmt::Display,
        lost: impl core::fmt::Display,
        recorder_lost: impl core::fmt::Display,
        p50: impl core::fmt::Display,
        p99: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!(
                "멈춤: 블록 {blocks}개, 끊김 {underruns}회, 장치 재시작 {restarts}회, 손실 {lost}바이트, 기록기 손실 {recorder_lost}바이트, 지연 p50 {p50} ms p99 {p99} ms"
            ),
            Language::En => format!(
                "stopped: {blocks} blocks, {underruns} underruns, {restarts} device restarts, {lost} bytes lost, {recorder_lost} recorder bytes lost, latency p50 {p50} ms p99 {p99} ms"
            ),
        }
    }

    #[doc = "      출력 끊김: 링 {ring}프레임, 표지 {markers}, 배치 {layout}, 필요 {needed}, 생성 {produced}, 마지막 블록 {ago} ms 전"]
    pub fn underrun(
        language: Language,
        ring: impl core::fmt::Display,
        markers: impl core::fmt::Display,
        layout: impl core::fmt::Display,
        needed: impl core::fmt::Display,
        produced: impl core::fmt::Display,
        ago: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!(
                "      출력 끊김: 링 {ring}프레임, 표지 {markers}, 배치 {layout}, 필요 {needed}, 생성 {produced}, 마지막 블록 {ago} ms 전"
            ),
            Language::En => format!(
                "      underrun: ring {ring} frames, markers {markers}, layout {layout}, needed {needed}, produced {produced}, last block {ago} ms ago"
            ),
        }
    }

    #[doc = "ssr-live는 Windows에서만 실행됩니다"]
    pub fn windows_only(language: Language) -> &'static str {
        match language {
            Language::Ko => "ssr-live는 Windows에서만 실행됩니다",
            Language::En => "ssr-live runs on Windows only",
        }
    }

    #[doc = "예"]
    pub fn yes(language: Language) -> &'static str {
        match language {
            Language::Ko => "예",
            Language::En => "yes",
        }
    }
}

pub mod replay {
    use super::Language;

    #[doc = "공간 소프트리시버 드라이버 기록 덤프를 읽는 오프라인 도구"]
    pub fn about(language: Language) -> &'static str {
        match language {
            Language::Ko => "공간 소프트리시버 드라이버 기록 덤프를 읽는 오프라인 도구",
            Language::En => "Offline reader for Spatial SoftReceiver driver record dumps",
        }
    }

    #[doc = "늦게 도착한 입구 데이터를 기다리며 끝난 구간을 보관할 시간입니다."]
    pub fn arrival_slack_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "늦게 도착한 입구 데이터를 기다리며 끝난 구간을 보관할 시간입니다.",
            Language::En => "Time to retain a completed interval for late entrance data",
        }
    }

    #[doc = "--arrival-slack-ms는 0 이상의 유한한 수여야 합니다"]
    pub fn arrival_slack_invalid(language: Language) -> &'static str {
        match language {
            Language::Ko => "--arrival-slack-ms는 0 이상의 유한한 수여야 합니다",
            Language::En => "--arrival-slack-ms must be a finite non-negative number",
        }
    }

    #[doc = "--arrival-slack-ms는 {max} ms를 넘을 수 없습니다"]
    pub fn arrival_slack_too_large(language: Language, max: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("--arrival-slack-ms는 {max} ms를 넘을 수 없습니다"),
            Language::En => format!("--arrival-slack-ms must not exceed {max} ms"),
        }
    }

    #[doc = "출력 블록 정렬 값이 WAVE 범위를 넘습니다"]
    pub fn block_align_overflow(language: Language) -> &'static str {
        match language {
            Language::Ko => "출력 블록 정렬 값이 WAVE 범위를 넘습니다",
            Language::En => "output block alignment overflows WAVE",
        }
    }

    #[doc = "출력 바이트율이 WAVE 범위를 넘습니다"]
    pub fn byte_rate_overflow(language: Language) -> &'static str {
        match language {
            Language::Ko => "출력 바이트율이 WAVE 범위를 넘습니다",
            Language::En => "output byte rate overflows WAVE",
        }
    }

    #[doc = "덤프 도중 출력 채널 수가 {old_value}개에서 {new_value}개로 바뀌었습니다"]
    pub fn channel_count_changed(
        language: Language,
        old_value: impl core::fmt::Display,
        new_value: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => {
                format!("덤프 도중 출력 채널 수가 {old_value}개에서 {new_value}개로 바뀌었습니다")
            }
            Language::En => format!(
                "output channel count changed from {old_value} to {new_value} during the dump"
            ),
        }
    }

    #[doc = "마스크가 0인 PCM 순서를 CHANNELS=PRESET 또는 CHANNELS=SPEAKER,SPEAKER,... 형식으로 덮어씁니다."]
    pub fn direct_layout_help(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "마스크가 0인 PCM 순서를 CHANNELS=PRESET 또는 CHANNELS=SPEAKER,SPEAKER,... 형식으로 덮어씁니다."
            }
            Language::En => {
                "Override a zero-mask PCM order, as CHANNELS=PRESET or CHANNELS=SPEAKER,SPEAKER,..."
            }
        }
    }

    #[doc = "진단 기록 덤프입니다."]
    pub fn dump_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "진단 기록 덤프입니다.",
            Language::En => "Diagnostic record dump",
        }
    }

    #[doc = "모든 엔드포인트 또는 0, 1, 2, 3번 중 하나를 렌더합니다."]
    pub fn endpoint_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "모든 엔드포인트 또는 0, 1, 2, 3번 중 하나를 렌더합니다.",
            Language::En => "Render all endpoints or only endpoint 0, 1, 2, or 3",
        }
    }

    #[doc = "ssr-replay: {detail}"]
    pub fn error_prefix(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("ssr-replay: {detail}"),
            Language::En => format!("ssr-replay: {detail}"),
        }
    }

    #[doc = "덤프의 엔드포인트·에포크별 형식, 기록 수와 손실을 JSON으로 요약합니다."]
    pub fn inspect_about(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "덤프의 엔드포인트·에포크별 형식, 기록 수와 손실을 JSON으로 요약합니다."
            }
            Language::En => {
                "Print a JSON summary of a dump: formats per endpoint and epoch, record counts, losses"
            }
        }
    }

    #[doc = "덤프에서 오디오 블록이 나오지 않았습니다"]
    pub fn no_audio_blocks(language: Language) -> &'static str {
        match language {
            Language::Ko => "덤프에서 오디오 블록이 나오지 않았습니다",
            Language::En => "the dump produced no audio blocks",
        }
    }

    #[doc = "출력 채널이 없습니다"]
    pub fn no_channels(language: Language) -> &'static str {
        match language {
            Language::Ko => "출력 채널이 없습니다",
            Language::En => "output has no channels",
        }
    }

    #[doc = "출력 WAV 파일입니다."]
    pub fn out_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "출력 WAV 파일입니다.",
            Language::En => "Output WAV file",
        }
    }

    #[doc = "출력 프로필 TOML입니다."]
    pub fn profile_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "출력 프로필 TOML입니다.",
            Language::En => "Output profile TOML",
        }
    }

    #[doc = "진단 덤프를 결정적인 32비트 부동소수점 WAV로 렌더합니다."]
    pub fn render_about(language: Language) -> &'static str {
        match language {
            Language::Ko => "진단 덤프를 결정적인 32비트 부동소수점 WAV로 렌더합니다.",
            Language::En => "Render a diagnostic dump to a deterministic 32-bit float WAV",
        }
    }

    #[doc = "기계가 읽는 JSON 렌더 보고서를 이 파일에 씁니다."]
    pub fn report_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "기계가 읽는 JSON 렌더 보고서를 이 파일에 씁니다.",
            Language::En => "Write a machine-readable JSON render report to this file",
        }
    }

    #[doc = "렌더 출력이 RIFF의 4 GiB 한도를 넘습니다"]
    pub fn riff_too_large(language: Language) -> &'static str {
        match language {
            Language::Ko => "렌더 출력이 RIFF의 4 GiB 한도를 넘습니다",
            Language::En => "rendered output exceeds the 4 GiB RIFF limit",
        }
    }

    #[doc = "덤프 도중 장면 표본율이 {old_value} Hz에서 {new_value} Hz로 바뀌었습니다"]
    pub fn scene_rate_changed(
        language: Language,
        old_value: impl core::fmt::Display,
        new_value: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => {
                format!("덤프 도중 장면 표본율이 {old_value} Hz에서 {new_value} Hz로 바뀌었습니다")
            }
            Language::En => {
                format!("scene rate changed from {old_value} Hz to {new_value} Hz during the dump")
            }
        }
    }

    #[doc = "출력 채널이 너무 많습니다: {channels}"]
    pub fn too_many_channels(language: Language, channels: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("출력 채널이 너무 많습니다: {channels}"),
            Language::En => format!("output has too many channels: {channels}"),
        }
    }

    #[doc = "ssr-replay: {blocks}개 블록, {frames}개 프레임을 {path}에 썼습니다 (sha256 {sha256})"]
    pub fn wrote(
        language: Language,
        blocks: impl core::fmt::Display,
        frames: impl core::fmt::Display,
        path: impl core::fmt::Display,
        sha256: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!(
                "ssr-replay: {blocks}개 블록, {frames}개 프레임을 {path}에 썼습니다 (sha256 {sha256})"
            ),
            Language::En => format!(
                "ssr-replay: wrote {blocks} blocks, {frames} frames to {path} (sha256 {sha256})"
            ),
        }
    }
}

pub mod setup {
    use super::Language;

    #[doc = "공간 소프트리시버 관리자 작업(앱이 관리자 권한으로 실행)"]
    pub fn about(language: Language) -> &'static str {
        match language {
            Language::Ko => "공간 소프트리시버 관리자 작업(앱이 관리자 권한으로 실행)",
            Language::En => "Spatial SoftReceiver administrator tasks (run elevated by the app)",
        }
    }

    #[doc = "시험서명 인증서입니다. 파일이 없으면 릴리스 묶음입니다."]
    pub fn cert_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "시험서명 인증서입니다. 파일이 없으면 릴리스 묶음입니다.",
            Language::En => "The test-signing certificate; a missing file means a release package",
        }
    }

    #[doc = "n번 비트가 n번 엔드포인트를 켭니다."]
    pub fn endpoint_mask_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "n번 비트가 n번 엔드포인트를 켭니다.",
            Language::En => "Bit n enables endpoint n",
        }
    }

    #[doc = "드라이버 묶음 INF입니다."]
    pub fn inf_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "드라이버 묶음 INF입니다.",
            Language::En => "The driver package INF",
        }
    }

    #[doc = "묶음의 시험서명 인증서를 신뢰하고 루트 장치를 만든 뒤 드라이버와 엔드포인트 독점 정책을 설치합니다."]
    pub fn install_about(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "묶음의 시험서명 인증서를 신뢰하고 루트 장치를 만든 뒤 드라이버와 엔드포인트 독점 정책을 설치합니다."
            }
            Language::En => {
                "Trust the package's test-signing certificate, create the root device, install the driver and apply the endpoints' exclusive-mode policy"
            }
        }
    }

    #[doc = "장치에 드라이버를 다시 설치하고 독점 정책을 적용합니다. 장치가 없으면 설치합니다."]
    pub fn repair_about(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "장치에 드라이버를 다시 설치하고 독점 정책을 적용합니다. 장치가 없으면 설치합니다."
            }
            Language::En => {
                "Reinstall the driver on the device and apply the exclusive-mode policy, or install when there is no device"
            }
        }
    }

    #[doc = "장치를 껐다 켭니다."]
    pub fn restart_device_about(language: Language) -> &'static str {
        match language {
            Language::Ko => "장치를 껐다 켭니다.",
            Language::En => "Disable and enable the device",
        }
    }

    #[doc = "결과 줄을 쓸 파일입니다."]
    pub fn result_help(language: Language) -> &'static str {
        match language {
            Language::Ko => "결과 줄을 쓸 파일입니다.",
            Language::En => "Where to write the result line",
        }
    }

    #[doc = "드라이버 엔드포인트 마스크를 정하고 장치를 다시 시작한 뒤 켜진 엔드포인트에 독점 정책을 적용합니다."]
    pub fn set_formats_about(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "드라이버 엔드포인트 마스크를 정하고 장치를 다시 시작한 뒤 켜진 엔드포인트에 독점 정책을 적용합니다."
            }
            Language::En => {
                "Set the driver's endpoint mask, restart the device and apply the exclusive-mode policy to the endpoints the mask enables"
            }
        }
    }

    #[doc = "현재·이전 장치, 드라이버 묶음, 남은 서비스와 시험서명 인증서를 지우고 남은 항목을 보고합니다."]
    pub fn uninstall_about(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "현재·이전 장치, 드라이버 묶음, 남은 서비스와 시험서명 인증서를 지우고 남은 항목을 보고합니다."
            }
            Language::En => {
                "Remove the current and legacy devices, driver packages, orphan services and test-signing certificates, and report what stayed"
            }
        }
    }

    #[doc = "ssr-setup: {path}: {detail}"]
    pub fn write_result_failed(
        language: Language,
        path: impl core::fmt::Display,
        detail: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!("ssr-setup: {path}: {detail}"),
            Language::En => format!("ssr-setup: {path}: {detail}"),
        }
    }
}

pub mod testkit {
    use super::Language;

    #[doc = "관리자 PowerShell에서 install.ps1을 다시 실행하십시오."]
    pub fn admin_required(language: Language) -> &'static str {
        match language {
            Language::Ko => "관리자 PowerShell에서 install.ps1을 다시 실행하십시오.",
            Language::En => "Run install.ps1 again in an administrator PowerShell.",
        }
    }

    #[doc = "이 묶음은 x64용이지만 현재 Windows 아키텍처는 {arch} 입니다."]
    pub fn architecture_mismatch(language: Language, arch: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("이 묶음은 x64용이지만 현재 Windows 아키텍처는 {arch} 입니다."),
            Language::En => {
                format!("This kit is for x64, but the current Windows architecture is {arch}.")
            }
        }
    }

    #[doc = "시험을 마치면 uninstall.ps1이 이 인증서를 삭제합니다."]
    pub fn certificate_cleanup(language: Language) -> &'static str {
        match language {
            Language::Ko => "시험을 마치면 uninstall.ps1이 이 인증서를 삭제합니다.",
            Language::En => "uninstall.ps1 removes this certificate when testing is complete.",
        }
    }

    #[doc = "시험 드라이버 서명을 신뢰하려고 인증서를 설치했습니다: {subject}, {thumbprint}"]
    pub fn certificate_installed(
        language: Language,
        subject: impl core::fmt::Display,
        thumbprint: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!(
                "시험 드라이버 서명을 신뢰하려고 인증서를 설치했습니다: {subject}, {thumbprint}"
            ),
            Language::En => format!(
                "Installed the certificate to trust the test driver signature: {subject}, {thumbprint}"
            ),
        }
    }

    #[doc = "collect.ps1: {path} 파일을 쓰지 못했습니다."]
    pub fn collect_failed(language: Language, path: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("collect.ps1: {path} 파일을 쓰지 못했습니다."),
            Language::En => format!("collect.ps1: {path} was not written"),
        }
    }

    #[doc = "수집한 항목:"]
    pub fn collected_heading(language: Language) -> &'static str {
        match language {
            Language::Ko => "수집한 항목:",
            Language::En => "Collected items:",
        }
    }

    #[doc = "{published} 패키지를 지우지 못했습니다({code})"]
    pub fn delete_driver_failed(
        language: Language,
        published: impl core::fmt::Display,
        code: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!("{published} 패키지를 지우지 못했습니다({code})"),
            Language::En => format!("could not delete {published} ({code})"),
        }
    }

    #[doc = "드라이버 패키지를 지웁니다: {published}(원본 {original})"]
    pub fn deleting_driver(
        language: Language,
        published: impl core::fmt::Display,
        original: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!("드라이버 패키지를 지웁니다: {published}(원본 {original})"),
            Language::En => format!("deleting driver package {published} (original {original})"),
        }
    }

    #[doc = "장치 {instance}: 상태 {status}, 문제 {problem}, 드라이버 {driver}, 서비스 {service}"]
    pub fn device_line(
        language: Language,
        instance: impl core::fmt::Display,
        status: impl core::fmt::Display,
        problem: impl core::fmt::Display,
        driver: impl core::fmt::Display,
        service: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!(
                "장치 {instance}: 상태 {status}, 문제 {problem}, 드라이버 {driver}, 서비스 {service}"
            ),
            Language::En => format!(
                "device {instance}: status {status}, problem {problem}, driver {driver}, service {service}"
            ),
        }
    }

    #[doc = "장치 업데이트: {detail}"]
    pub fn device_update_warning(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("장치 업데이트: {detail}"),
            Language::En => format!("device update: {detail}"),
        }
    }

    #[doc = "설치된 장치에 드라이버 INF가 연결되지 않았습니다."]
    pub fn driver_inf_missing(language: Language) -> &'static str {
        match language {
            Language::Ko => "설치된 장치에 드라이버 INF가 연결되지 않았습니다.",
            Language::En => "The installed device has no driver INF attached.",
        }
    }

    #[doc = ".ssrdump 파일 합계가 200 MB를 넘어 목록만 수집합니다."]
    pub fn dumps_too_large(language: Language) -> &'static str {
        match language {
            Language::Ko => ".ssrdump 파일 합계가 200 MB를 넘어 목록만 수집합니다.",
            Language::En => {
                "The .ssrdump files total more than 200 MB, so only the list is collected."
            }
        }
    }

    #[doc = "엔드포인트 {endpoint} ({state}): {name}"]
    pub fn endpoint_line(
        language: Language,
        endpoint: impl core::fmt::Display,
        state: impl core::fmt::Display,
        name: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!("엔드포인트 {endpoint} ({state}): {name}"),
            Language::En => format!("endpoint {endpoint} ({state}): {name}"),
        }
    }

    #[doc = "이 이름들이 Windows 설정 > 시스템 > 소리의 출력 장치 목록에 보여야 합니다."]
    pub fn endpoints_visible(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "이 이름들이 Windows 설정 > 시스템 > 소리의 출력 장치 목록에 보여야 합니다."
            }
            Language::En => {
                "These names should appear in the output device list under Windows Settings > System > Sound."
            }
        }
    }

    #[doc = "현재 또는 이전 시험판 장치가 이미 있습니다. 먼저 uninstall.ps1을 실행하십시오."]
    pub fn existing_device(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "현재 또는 이전 시험판 장치가 이미 있습니다. 먼저 uninstall.ps1을 실행하십시오."
            }
            Language::En => {
                "A current or earlier test device already exists. Run uninstall.ps1 first."
            }
        }
    }

    #[doc = "install.ps1: {detail}"]
    pub fn install_failed(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("install.ps1: {detail}"),
            Language::En => format!("install.ps1: {detail}"),
        }
    }

    #[doc = "녹음과 화면 파일 합계가 300 MB를 넘어 목록만 수집합니다."]
    pub fn media_too_large(language: Language) -> &'static str {
        match language {
            Language::Ko => "녹음과 화면 파일 합계가 300 MB를 넘어 목록만 수집합니다.",
            Language::En => {
                "The recordings and screenshots total more than 300 MB, so only the list is collected."
            }
        }
    }

    #[doc = "Windows 10 2004(빌드 19041) 이상에서 실행하십시오."]
    pub fn os_required(language: Language) -> &'static str {
        match language {
            Language::Ko => "Windows 10 2004(빌드 19041) 이상에서 실행하십시오.",
            Language::En => "Run on Windows 10 version 2004 (build 19041) or newer.",
        }
    }

    #[doc = "정책 정리: {detail}"]
    pub fn policy_cleanup(language: Language, detail: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("정책 정리: {detail}"),
            Language::En => format!("policy cleanup: {detail}"),
        }
    }

    #[doc = "남은 항목: 장치 {devices}, 드라이버 패키지 {drivers}, 인증서 {certificates}, 서비스 키 {services}, 엔드포인트 {endpoints}"]
    pub fn remaining(
        language: Language,
        devices: impl core::fmt::Display,
        drivers: impl core::fmt::Display,
        certificates: impl core::fmt::Display,
        services: impl core::fmt::Display,
        endpoints: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!(
                "남은 항목: 장치 {devices}, 드라이버 패키지 {drivers}, 인증서 {certificates}, 서비스 키 {services}, 엔드포인트 {endpoints}"
            ),
            Language::En => format!(
                "remaining: devices={devices}, driver packages={drivers}, certificates={certificates}, service keys={services}, endpoints={endpoints}"
            ),
        }
    }

    #[doc = "{instance} 장치를 지우지 못했습니다({code})"]
    pub fn remove_device_failed(
        language: Language,
        instance: impl core::fmt::Display,
        code: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!("{instance} 장치를 지우지 못했습니다({code})"),
            Language::En => format!("could not remove {instance} ({code})"),
        }
    }

    #[doc = "인증서를 지웁니다: {store} {thumbprint}"]
    pub fn removing_certificate(
        language: Language,
        store: impl core::fmt::Display,
        thumbprint: impl core::fmt::Display,
    ) -> String {
        match language {
            Language::Ko => format!("인증서를 지웁니다: {store} {thumbprint}"),
            Language::En => format!("removing certificate {store} {thumbprint}"),
        }
    }

    #[doc = "장치를 지웁니다: {instance}"]
    pub fn removing_device(language: Language, instance: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("장치를 지웁니다: {instance}"),
            Language::En => format!("removing device {instance}"),
        }
    }

    #[doc = "시험 보고서: {path}"]
    pub fn report_path(language: Language, path: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("시험 보고서: {path}"),
            Language::En => format!("Test report: {path}"),
        }
    }

    #[doc = "Root\\SpatialSoftReceiver 장치를 만들지 못했습니다."]
    pub fn root_device_failed(language: Language) -> &'static str {
        match language {
            Language::Ko => "Root\\SpatialSoftReceiver 장치를 만들지 못했습니다.",
            Language::En => "Could not create the Root\\SpatialSoftReceiver device.",
        }
    }

    #[doc = "루트 장치를 설치했습니다(다시 시작 요청: {reboot})"]
    pub fn root_device_installed(language: Language, reboot: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("루트 장치를 설치했습니다(다시 시작 요청: {reboot})"),
            Language::En => format!("root device installed (reboot requested: {reboot})"),
        }
    }

    #[doc = "Secure Boot가 켜져 있습니다. 시험서명 드라이버를 위해 Secure Boot를 꺼야 합니다. {guide}."]
    pub fn secure_boot_warning(language: Language, guide: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!(
                "Secure Boot가 켜져 있습니다. 시험서명 드라이버를 위해 Secure Boot를 꺼야 합니다. {guide}."
            ),
            Language::En => {
                format!("Secure Boot is enabled. Turn it off for the test-signed driver. {guide}.")
            }
        }
    }

    #[doc = "서비스 키는 장치와 패키지를 지운 뒤에도 재부팅 전까지 남을 수 있습니다. 재부팅 후 다시 확인하십시오."]
    pub fn service_keys_warning(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "서비스 키는 장치와 패키지를 지운 뒤에도 재부팅 전까지 남을 수 있습니다. 재부팅 후 다시 확인하십시오."
            }
            Language::En => {
                "Service keys can remain until restart after the devices and packages are removed. Restart, then check again."
            }
        }
    }

    #[doc = "Secure Boot를 끈 뒤 관리자 명령으로 `bcdedit /set testsigning on`을 실행하고 재부팅하십시오"]
    pub fn test_signing_guide(language: Language) -> &'static str {
        match language {
            Language::Ko => {
                "Secure Boot를 끈 뒤 관리자 명령으로 `bcdedit /set testsigning on`을 실행하고 재부팅하십시오"
            }
            Language::En => {
                "Turn off Secure Boot, run `bcdedit /set testsigning on` as administrator, and restart"
            }
        }
    }

    #[doc = "{guide}."]
    pub fn test_signing_required(language: Language, guide: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => format!("{guide}."),
            Language::En => format!("{guide}."),
        }
    }

    #[doc = "testsigning이 켜져 있지 않습니다. CI 허용 옵션으로 계속합니다. {guide}."]
    pub fn test_signing_warning(language: Language, guide: impl core::fmt::Display) -> String {
        match language {
            Language::Ko => {
                format!("testsigning이 켜져 있지 않습니다. CI 허용 옵션으로 계속합니다. {guide}.")
            }
            Language::En => {
                format!("Test-signing is not enabled. Continuing with the CI override. {guide}.")
            }
        }
    }
}
