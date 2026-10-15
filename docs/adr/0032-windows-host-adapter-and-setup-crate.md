# 0032. Windows 호스트 어댑터는 `ssr-sys`에, 설치는 `ssr-setup-sys`에 둔다

상태: 채택, 2026-10-15. ADR 0022의 격리 크레이트 목록에 `ssr-setup-sys`를 더하고 `ssr-sys`의 unsafe 예산을 32에서 40으로 올립니다.

## 배경

ADR 0013의 감독자는 OS를 `Host` 포트로만 부릅니다(M4 첫 조각, PR #25). 그 Windows 어댑터는 렌더 엔드포인트를 상태와 KS 참조(우리 엔드포인트 번호)까지 열거하고, 기본 장치 역할 셋을 읽고 바꾸며, 시험서명과 Secure Boot와 OS 빌드를 읽고, 관리자 작업을 `ssr-setup`으로 승격해 실행합니다. 기본 장치를 바꾸는 `IPolicyConfig`는 Windows가 문서화하지 않은 COM 인터페이스이고, 엔드포인트 토폴로지와 `ShellExecuteEx`와 레지스트리와 `NtQuerySystemInformation`은 모두 `windows` 크레이트의 unsafe 호출입니다. 지금 `ssr-sys`는 unsafe 토큰 11개를 쓰고 예산은 32인데, 어댑터에 약 20개가 더 듭니다.

관리자 작업(루트 장치 만들기, 드라이버 패키지 설치와 삭제, 인증서, 레지스트리 매개변수, 장치 재시작, 독점 정책)은 SetupAPI와 cfgmgr32와 인증서 저장소의 unsafe 호출이 약 15개 더 듭니다. ADR 0013은 설치 모듈을 `ssr-setup`만 링크한다고 정했습니다.

## 결정

- **`ssr-sys::host`.** `ssr_types::Host`의 Windows 어댑터 `WindowsHost`를 `ssr-sys`에 둡니다. 앱 본체가 사용자 권한으로 하는 일(열거, 역할 읽기와 쓰기, 플랫폼 사실, 승격 실행, 복원점 파일)만 있습니다. `ssr-sys`의 예산을 40으로 올립니다.
- **`IPolicyConfig`는 vtable 구조체로 부릅니다.** 문서화되지 않은 인터페이스(CLSID `870af99c-171d-4f9e-af0d-e63df40c2bc9`, IID `f8679f50-850a-41cf-9c72-430f290290c8`)의 `SetDefaultEndpoint` 자리(IUnknown 셋 뒤 열한 번째)만 함수 포인터로 선언합니다. 메서드 열셋을 `unsafe fn`으로 다 적는 것보다 unsafe 토큰이 적고, 쓰지 않는 자리를 부를 길이 없습니다. Windows가 이 인터페이스를 바꾸면 기본 장치 바꾸기만 실패하고 그 사실이 `HostError`로 올라옵니다. 그때는 `ssr-guard`와 되돌리기도 같은 길을 쓰므로 함께 실패하며, 감독자는 역할을 넘겨받지 않고 안내합니다.
- **`ssr-setup-sys`.** 관리자 작업을 새 격리 크레이트 `ssr-setup-sys`에 둡니다. `ssr-setup`만 의존하고, 앱 본체는 의존하지 않습니다. 기능 플래그가 아니라 크레이트로 가르는 까닭은, 링크 그래프가 곧 보장이라 `ssr-policy` 게이트가 Cargo.toml만 보고 검사할 수 있기 때문입니다. 예산은 만드는 PR에서 실제 개수로 정합니다(ADR 0022).
- **`ssr-setup`의 계약.** `ssr-setup <task> [options] --result <path>`로 부르고, 결과는 JSON 한 줄입니다(`ok`, `reboot_required`, `message`, 실패하면 `error`와 `code`). 승격된 프로세스의 stdout은 부모가 받을 수 없으므로 같은 줄을 `--result` 파일에도 씁니다. 작업은 `install`, `uninstall`, `set-formats`, `restart-device`, `repair`이고 정해진 이름만 받습니다. 패키지 식별자(하드웨어 ID, 서비스, INF, 인증서 주체, 옛 패키지)는 `tools/ci/package-identities.ps1`과 같아야 하고 시험이 그것을 확인합니다.
- **설치 순서는 시험 묶음과 같습니다.** `tools/ci/RootDevice.cs`의 SetupAPI 순서(`SetupDiGetINFClass`, `SetupDiCreateDeviceInfo`, `SPDRP_HARDWAREID`, `DIF_REGISTERDEVICE`, `UpdateDriverForPlugAndPlayDevices`)와 `install.ps1`의 인증서 설치, `exclusive-policy.ps1`의 독점 정책을 Rust로 옮깁니다. 시험서명 인증서는 묶음이 시험서명일 때만 설치하고 삭제합니다.
- **게이트.** `ssr-policy`의 Windows 의존성 게이트는 허용 목록(`ssr-sys`, `ssr-setup-sys`)을 갖고, `ssr-setup-sys`를 `ssr-setup`만 의존한다는 링크 규칙을 검사합니다.

## 결과

- 앱 본체가 관리자 권한 없이 돈다는 사실은 `ssr-setup-sys`가 `apps/ssr-setup`에만 링크된다는 것으로 보장되고, 게이트가 지킵니다.
- 호스트 어댑터와 설치 모듈은 Windows에서만 컴파일되고, Linux CI는 `--target x86_64-pc-windows-msvc`의 clippy로 타입을 검사합니다. 순수한 부분(KS 참조 경로 해석, JSON 계약, 인자 해석, 식별자 일치)은 모든 OS에서 시험합니다. 실제 장치에서의 동작은 Windows 러너의 드라이버 시험 단계와 시험서명 PC에서 확인하며, 그 CI 변경은 별도 PR입니다.
- unsafe 검토 범위가 셋이 됩니다(장치와 드라이버와 호스트, 설치, 그리고 앞으로의 플러그인과 ASIO).
