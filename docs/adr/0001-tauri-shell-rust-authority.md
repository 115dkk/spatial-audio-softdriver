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
- **기본 프로필.** BRIR 가져오기 의도는 아직 거부되므로, 런처는 `profile.toml`이 없으면 FL/FR ±30°의 스테레오 스피커 프로필로 엔진을 띄웁니다. 앱은 첫날부터 소리를 내고, 헤드폰 프로필은 다음 조각이 붙입니다.
- **셸의 몫.** 트레이(열기, 켜기/끄기, 종료), 창 닫기는 숨기기(소리는 계속), 단일 인스턴스(두 번째 실행은 창을 앞으로), 로그인 시 자동 시작(`--minimized`), 로그오프 같은 바깥의 종료 요청에도 `Quit`를 먼저 보내 역할을 되돌리기. `Quit`가 3초 안에 끝나지 않거나 제어 스레드가 패닉으로 끝나면 셸이 복원점 파일을 읽어 `restore_roles`로 역할을 되돌립니다(`ssr-guard`가 할 일을 프로세스 안에서 마지막 수단으로 하는 것). 설치본과 Velopack 업데이트는 다음 조각입니다(`bundle.active`는 꺼 둠).
- **연기 시험.** `--smoke [초]`로 띄우면 화면이 `snapshot` 명령을 부른 순간 `Quit`를 거쳐 0으로 끝나고, 시간 안에 안 오면 2로 끝납니다. CI의 Windows 러너가 디버그와 릴리스 빌드로 돌려, WebView2가 우리 번들을 그리고 IPC가 통하는지 확인합니다. 드라이버가 없는 러너에서는 마법사의 첫 단계가 그 스냅샷입니다.
- **글꼴.** IBM Plex Sans KR은 저장소에 넣지 않고 `tools/ui-fonts.mjs`가 npm 패키지 `@ibm/plex-sans-kr` 1.1.0의 tarball과 파일을 SHA-256으로 대조해 받습니다(ASIO SDK와 같은 방식). 화면 문구는 한국어이고 `ui/src/text.ts`에 모아 영어 카탈로그를 붙일 자리를 둡니다.
