# 0001. 화면은 Tauri 2, 판단은 Rust

상태: 채택, 2026-10-11

## 배경

앱에는 첫 실행 마법사, 상태 화면, 프로필 화면이 필요하고, 실시간 오디오와 장치 상태는 Rust 쪽에 있습니다. 화면과 엔진이 각자 상태를 가지면 둘이 어긋났을 때 어느 쪽이 맞는지 정할 기준이 없습니다.

## 결정

화면은 Tauri 2와 TypeScript로 그립니다. 상태와 판단은 모두 Rust의 `supervisor` 하나가 가집니다. 화면은 `Intent`만 보내고, `Snapshot` 전체를 받아 그립니다. 미터와 객체 위치는 상태 변경 이벤트에 묻히지 않도록 30 Hz 이벤트로 따로 보냅니다.

## 결과

- 화면에는 상태가 없으므로 창을 다시 열어도 같은 그림이 나옵니다.
- 오류 문장과 다음 행동도 Rust가 정해 `UserError`로 보냅니다. 화면은 오류 코드를 해석하지 않습니다.
- Tauri 앱(`apps/spatial-softreceiver`)은 M4에서 만듭니다. M0부터 M2까지는 개발 도구만 있습니다.

## 2026-10-15 보탬 (M4 셸 골격)

앱 `apps/spatial-softreceiver`를 만들면서 정한 것입니다.

- **배치.** 크레이트 루트가 곧 Tauri 쪽입니다(`Cargo.toml`, `build.rs`, `tauri.conf.json`, `capabilities/`, `icons/`, `src/`). 설계서의 `src-tauri/`는 이 디렉터리 자체이고, 화면은 그 아래 `ui/`(Vite + TypeScript, 프레임워크 없음)입니다. 워크스페이스의 `apps/*` 글롭이 크레이트를 찾으려면 `Cargo.toml`이 디렉터리 바로 아래 있어야 하기 때문입니다. tauri 계열 의존성은 `[target.'cfg(windows)'.dependencies]`에만 있어 Linux에서는 셸이 컴파일되지 않고, 제어 스레드와 설정 파일과 경로는 모든 OS에서 컴파일되어 Linux에서 시험합니다. Windows 대상 clippy와 doc은 Linux에서 돌고, 그때 `tauri::generate_context!`가 `ui/dist`와 아이콘을 읽으므로 화면 빌드가 먼저입니다(`.github/actions/ui-dist`).
- **계약은 파일로 둡니다.** `ssr-types`의 `Intent`, `Snapshot`, `UserError`, `EngineStatus`와 그 아래 타입에 serde가 붙고, feature `ts`에서 `specta::Type`도 붙습니다. `tests/ui_contract.rs`가 `ui/src/bindings.ts`(TypeScript 타입)와 `ui/fixtures/*.json`(화면 상태마다 하나씩, serde가 실제로 내보내는 스냅샷과 모든 의도)을 만들고 CI에서 대조합니다. `SSR_UI_BLESS=1`로 다시 만듭니다. 드라이버의 C 헤더(`SSR_ABI_BLESS`)와 같은 규율입니다. 자료를 가진 enum은 `kind`/`value`로 태그하고(`{"kind":"choose_output","value":"..."}`), 단위 enum은 snake_case 문자열입니다. serde와 specta의 snake_case는 숫자가 섞인 이름(`Eac3`, `Arm64`)에서 다르므로 그런 변종은 `#[serde(rename)]`을 명시합니다. 체인 오류 계수는 종류 이름을 키로 한 맵으로 내보냅니다. specta는 u64를 JavaScript 숫자로 내보내는 마지막 판(`2.0.0-rc.22`, `specta-typescript 0.0.9`)에 고정했습니다. 생성 결과가 커밋되어 있으므로 생성기는 언제든 바꿀 수 있습니다.
- **명령 둘, 이벤트 둘.** `dispatch(intent)`는 `Result<(), UserError>`를 돌려주고, `snapshot()`은 화면이 뜰 때 한 번 현재 상태를 받는 명령입니다(설계서의 명령 하나에 이것을 더했습니다. 화면이 처음 그릴 상태를 이벤트로만 받으면 창을 다시 열 때 기다려야 하기 때문입니다). 이벤트 `snapshot`은 상태가 바뀔 때, `meters`는 `Running`인 동안 틱마다(33 ms, 30 Hz) `EngineStatus`를 실어 보냅니다.
- **제어 스레드.** 감독자는 UI 스레드가 아닌 스레드 하나가 가집니다. Windows 호스트의 COM 호출이 다중 스레드 아파트먼트에 들어가야 해서입니다(ADR 0032). 의도는 채널로 받아 `handle`하고, 성공이면 설정을 저장하며, 결과와 상관없이 스냅샷을 다시 떠 보냅니다. 틱마다 `tick`을 부르고 알림이 있거나 위상이 바뀌면 스냅샷을 보내며, 1초마다 스냅샷을 떠서 마지막 것과 다를 때만 보냅니다(출력 목록의 변화를 줍는 길이고, 엔진 상태는 비교에서 뺍니다). `Quit`에 들어가면 셸에 알리고 끝납니다. 셸 쪽 채널이 모두 닫혀도 스스로 `Quit`를 처리해 역할을 되돌립니다.
- **설정 파일.** `%LOCALAPPDATA%\Spatial SoftReceiver\settings.json`(`version`과 `supervisor::Settings`). 임시 파일에 쓰고 바꿔 넣습니다. 같은 디렉터리에 복원점, `app.log`, 그리고 있으면 `profile.toml`이 삽니다.
- **프로필 계약.** 감독자가 `Settings.profile`과 상태 폴더의 `profile.toml`을 해석해 `OutputProfile`을 런처에 빌려 줍니다(`Launcher::launch(&mut self, output: &OutputDevice, profile: &OutputProfile)`). 런처는 프로필 파일을 읽지 않고, 해석한 프로필은 감독자가 쥐고 있다가 다시 준비할 때 씁니다. 존재하는 `profile.toml`이 가져온 BRIR보다 우선하고, `SetGain`으로 저장한 게인이 있으면 파일 프로필의 게인을 덮어씁니다(없으면 파일의 게인 그대로). BRIR을 가져오면 헤드폰 프로필이 되며, 아무 설정도 없으면 FL/FR ±30°의 스테레오 스피커 프로필입니다. `Snapshot.profile`은 감독자가 캐시한 종류, 이름, 경로, 게인, 방향 목록과 파일 우선 여부를 담으므로 화면은 파일이나 BRIR을 직접 읽지 않습니다. `SetGain`과 `TestDirection`은 `Running`에서 엔진의 실시간 변경 큐로 전달되고, 다른 위상에서는 저장하거나 명시적으로 거부됩니다.
- **객체 지도 계약.** `EngineStatus.directions`는 현재 준비된 출력 레이아웃을, `EngineStatus.scene`은 가장 최근 믹스 블록에서 신호를 낸 객체의 차선·객체 번호·방향 벡터·RMS dBFS를 담습니다. 화면은 30 Hz `meters` 이벤트에 실린 이 값만 그립니다.
- **셸의 몫.** 트레이(열기, 켜기/끄기, 종료), 창 닫기는 숨기기(소리는 계속), 단일 인스턴스(두 번째 실행은 창을 앞으로), 로그인 시 자동 시작(`--minimized`), 로그오프 같은 바깥의 종료 요청에도 `Quit`를 먼저 보내 역할을 되돌리기. `Quit`가 3초 안에 끝나지 않거나 제어 스레드가 패닉으로 끝나면 셸이 복원점 파일을 읽어 `restore_roles`로 역할을 되돌립니다(`ssr-guard`가 할 일을 프로세스 안에서 마지막 수단으로 하는 것). 설치본과 업데이트는 아래 '설치본' 항목입니다(Tauri의 `bundle.active`는 꺼 둠).
- **연기 시험.** `--smoke [초]`로 띄우면 화면이 `snapshot` 명령을 부른 순간 `Quit`를 거쳐 0으로 끝나고, 시간 안에 안 오면 2로 끝납니다. CI의 Windows 러너가 디버그와 릴리스 빌드로 돌려, WebView2가 우리 번들을 그리고 IPC가 통하는지 확인합니다. 드라이버가 없는 러너에서는 마법사의 첫 단계가 그 스냅샷입니다.
- **글꼴.** IBM Plex Sans KR은 저장소에 넣지 않고 `tools/ui-fonts.mjs`가 npm 패키지 `@ibm/plex-sans-kr` 1.1.0의 tarball과 파일을 SHA-256으로 대조해 받습니다(ASIO SDK와 같은 방식). 화면 문구는 한국어이고 `ui/src/text.ts`에 모아 영어 카탈로그를 붙일 자리를 둡니다.

## 설치본과 업데이트 (2026-10-15 추가)

- **Velopack.** `release.yml`의 `app-installer` 잡이 앱(`spatial-softreceiver.exe`), 감시 프로세스(`ssr-guard.exe`), 설치 도구(`ssr-setup.exe`), 같은 릴리스의 시험서명 드라이버 패키지(`spatialsoftreceiver.sys/.inf/.cat`, `spatialsoftreceiver-test.cer`), 스피커 프로필 예시를 한 폴더에 모아 `vpk pack`으로 묶습니다. `Setup.exe`는 묻지 않고 `%LOCALAPPDATA%\SpatialSoftReceiver`에 풀고 시작 메뉴 바로가기를 만듭니다. 바로가기와 프로세스의 AUMID는 `io.github.115dkk.spatial-softreceiver`(`tauri.conf.json`의 identifier)이고, 토스트가 이것을 씁니다. Tauri의 번들러를 쓰지 않는 까닭은 드라이버 패키지와 두 도구를 앱 옆에 두고 업데이트까지 한 틀로 다루기 위해서입니다.
- **업데이트의 출처.** 앱은 `velopack` 크레이트의 `GithubSource`(사전 릴리스 포함)로 이 저장소의 릴리스에서 `releases.win.json`과 `.nupkg`를 읽습니다. 0.x는 모두 사전 릴리스라 `latest` 링크를 쓰지 않습니다. 시작 20초 뒤에 한 번, 그 뒤 여섯 시간마다 확인하고, 있으면 바로 내려받아 `update` 이벤트로 화면에 알립니다(`apps/spatial-softreceiver/src/update.rs`).
- **적용 시점.** 실행 중인 세션 뒤에서 적용하지 않습니다. 설정 화면의 '다시 시작해 적용'이 Velopack의 업데이터를 띄워 종료를 기다리게 한 뒤 `Quit`를 보내고(역할이 돌아가고 감시 프로세스는 EOF로 끝남), 업데이터가 적용하고 앱을 인자 없이 다시 띄우므로 새 인스턴스가 제 감시 프로세스를 띄웁니다. 받아 두고 누르지 않았으면 다음 보통 실행(`--guarded`가 아닌 실행)이 감시 프로세스를 띄우기 전에 적용합니다. Velopack의 시작 시 자동 적용은 끕니다. 켜 두면 `--guarded`로 뜬 앱이 그 인자 그대로 감시 프로세스 없이 다시 뜨기 때문입니다.
- **훅.** `VelopackApp::run()`이 `main`의 첫 줄입니다. 제거 훅(`--veloapp-uninstall`)은 복원점이 남아 있으면 기본 장치 역할을 되돌립니다. 드라이버는 지우지 않습니다(관리자 권한과 시간이 들어 15초 안의 빠른 훅에 맞지 않음). 드라이버는 시험 묶음의 `uninstall.ps1`이나 다음 조각의 설정 화면이 지웁니다.
- **IPC.** 셸 명령 `update_state`, `apply_update`와 이벤트 `update`가 더해졌습니다. 업데이트 상태는 감독자의 스냅샷이 아니라 셸의 것입니다. 감독자는 PC 안의 소리만 다루고, 셸이 바깥(릴리스 페이지)을 봅니다.
