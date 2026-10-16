# 저장소 작업 규약

바꾸기 전에 [`CONTEXT.md`](CONTEXT.md)의 용어와 불변식, 관련 [ADR](docs/adr)을 읽습니다. 설계 전체는 `docs/Spatial SoftReceiver 아키텍처.html`이고, 드라이버 동결 전의 확장(업믹스, 플러그인 호스트, 시스템 소리)은 `docs/Spatial SoftReceiver 확장 설계.html`입니다.

## 지킬 것

- unsafe는 격리 크레이트(`unsafe-budget.toml`에 오른 `-sys` 크레이트, 지금은 `ssr-sys`와 `ssr-setup-sys`)에만 둡니다. 격리 크레이트를 뺀 새 크레이트 루트에는 `#![forbid(unsafe_code)]`를 걸고 Cargo.toml에 `[lints] workspace = true`를 둡니다. 격리 크레이트는 `ssr-sys`처럼 `#![deny(unsafe_code)]`와 자기 lint 표를 씁니다(ADR 0022). 숫자를 올리거나 격리 크레이트를 더하려면 ADR이 먼저입니다. `hostvst-sys`, `asio-sys`, `upmix-sys`는 ADR 0022가 허용했고, 처음 만드는 PR에서 숫자와 게이트를 함께 정합니다.
- Windows API 의존성(`windows`, `windows-sys`, `wasapi`)은 `ssr-sys`와 `ssr-setup-sys`만 가집니다(`hostvst-sys`와 `asio-sys`가 생기면 그 크레이트도). `ssr-setup-sys`는 관리자 작업만 담고 `apps/ssr-setup`만 링크합니다(ADR 0032). `engine` 이하는 Linux에서 빌드되고 테스트되어야 합니다.
- 외부 플러그인(VST 2, VST 3)은 엔진 프로세스에서 돌리지 않고 `ssr-plughost`에서만 돌립니다(ADR 0021). `streamring`과 hostvst 계열 크레이트는 `ssr-*`에 의존하지 않습니다.
- ASIO SDK 파일은 저장소에 넣지 않고 빌드 때 받아 SHA-256으로 고정합니다. EqualizerAPO-XT의 ASIO 래퍼에서 옮긴 커밋에는 원본 파일과 커밋을 적습니다(ADR 0025).
- 라이선스: 소스는 MIT입니다. ASIO SDK에 닿는 코드는 `asio-sys`에만 두고 파일 머리에 `// SPDX-License-Identifier: GPL-3.0-or-later`를 적습니다(SDK 정의를 손으로 옮긴 파일은 `GPL-3.0-only`). 다른 크레이트에는 GPL 코드를 들이지 않습니다(ADR 0003, 0025).
- 기능을 더하면 `features.toml`에 등록하고, `implemented`로 표시할 때는 실제로 있는 테스트를 가리킵니다.
- 실시간 스레드에서는 할당, 잠금, 파일 입출력을 하지 않습니다.
- 클린룸(ADR 0005): SpatialAudioLab의 소스와 그 소스에서 나온 패치는 읽지 않고, 작업자에게도 넘기지 않습니다. 구축기 공유 zip의 `sal-cli-bio-game.patch`가 여기에 해당합니다.
- 드라이버 계약(`crates/ssr-abi/src/wire.rs`)을 바꾸면 `SSR_ABI_BLESS=1`로 C 헤더를 다시 생성해 함께 커밋합니다.
- 사용자에게 보이는 문구는 모두 `i18n/ko.toml`과 `i18n/en.toml` 한 곳에만 씁니다(화면 `[screen]`, 감독자 `[supervisor]`, 감시 프로세스 `[guard]`, 앱 셸 `[shell]`, 명령줄 도구 `[cli]`(clap 공통)·`[live]`·`[replay]`·`[setup]`, 시험 묶음 스크립트 `[testkit]`, 여럿이 같이 쓰는 이름 `[names]`, Rust와 화면이 같이 쓰는 문장과 동작 이름 `[common]`). 코드에 사용자 문장을 직접 쓰지 않고, 같은 뜻의 문장은 한 키만 둡니다. 명령줄 도구(`ssr-setup`, `ssr-live`, `ssr-replay`)의 도움말과 사람이 읽는 출력, 시험 묶음 스크립트(`tools/testkit`)의 출력도 사용자 문구이므로 두 언어로 카탈로그에 둡니다. 도구의 언어는 `ssr_i18n::cli_language()`(환경 변수 `SSR_LANG`, 없으면 Windows 표시 언어)이고, 도움말은 clap derive의 doc 주석이 영어 원문이며 `ssr_i18n::clap_help::localize`가 카탈로그로 바꿉니다. 영어 도움말은 derive가 만든 것과 바이트까지 같아야 하므로(`clap_help::verify`가 도구마다 확인합니다) 도움말 문장을 고치면 doc 주석과 `en.toml`을 함께 고칩니다. CI가 읽는 영어 출력은 바꾸지 않습니다. 기계가 읽는 출력(JSON 보고서와 계약)의 키와 값, 로그 파일은 번역하지 않습니다. 원본을 고치면 `SSR_I18N_BLESS=1 cargo test -p ssr-i18n`으로 Rust 함수(`crates/ssr-i18n/src/catalog.rs`), 화면 카탈로그(`apps/spatial-softreceiver/ui/src/catalog.ts`), 스크립트 문구(`tools/testkit/strings.psd1`)를 다시 생성해 함께 커밋합니다. 두 언어의 키와 자리표시자(`{이름}`)는 같아야 하고 영어에는 한글이 없어야 하며, 드라이버 INF의 지역화 라벨은 Windows가 INF에서 읽으므로 INF에 남기되 `[names]`와 같아야 합니다(모두 그 시험이 확인합니다). 장치 이름, 파일 경로, 외부 라이브러리의 오류 상세 같은 데이터는 번역하지 않습니다.
- 사용자에게 보이는 이름(Windows 엔드포인트 이름이 되는 싱크 설명과 장치 설명, INF의 FriendlyName, 화면과 안내 문구)은 번호가 아니라 용도로 구분합니다. 이름만 보고 무엇에 쓰는지 알 수 있어야 합니다. 제품 이름은 `Spatial SoftReceiver`(한국어 `공간 소프트리시버`)이고, 엔드포인트는 `Spatial SoftReceiver`, `Spatial SoftReceiver Music`, `Spatial SoftReceiver PASS`(한국어 `공간 소프트리시버`, `공간 소프트리시버 음악`, `공간 소프트리시버 PASS`)입니다. 싱크 설명은 31자 안이어야 합니다.
- 엔드포인트 이름은 데이터입니다. INF가 설치할 때 언어에 맞는 이름(`[Strings]`, `[Strings.0412]`)을 서비스 `Parameters`의 `EndpointName0`~`EndpointName3`에 쓰고, 드라이버는 그 값을 읽습니다(없거나 31자를 넘으면 컴파일된 영어 이름). 그래서 도구와 앱은 이름으로 엔드포인트를 찾지 않습니다. 고정 식별자(엔드포인트 번호와 KS 필터 참조 문자열)로 찾는 공용 도우미 `tools/ci/endpoints.ps1`, `endpoints.py`를 씁니다. 코드와 계약 안의 엔드포인트 번호(0, 1, 2)는 식별자라 그대로 둡니다.
- 제품, 드라이버, 엔드포인트, 서비스, 파일, 앱의 이름에 남의 상표(Dolby, Atmos 등)나 사람 이름을 쓰지 않습니다. 호환되는 형식은 README와 안내서에 사실로만 적고, 상표 고지를 함께 둡니다.

## 검사

커밋 전에 아래를 통과시킵니다. CI(`ci.yml`)가 같은 것을 Linux와 Windows에서 돌리고, Windows 러너에서는 가상 오디오 케이블로 장치 시험, 루프백, 소크까지 돌립니다(ADR 0026). Windows 대상 clippy는 `rustup target add x86_64-pc-windows-msvc` 뒤 Linux에서도 됩니다.

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --target x86_64-pc-windows-msvc -- -D warnings
cargo test --workspace
cargo deny check
cargo test -p ssr-types --features ts --test ui_contract
cd apps/spatial-softreceiver/ui; npm ci; npm run check; npm test; npm run build
```

## 드라이버 (M3)

`driver/`는 MS-PL이고 구조는 ADR 0027입니다. SysVAD에서 가져온 파일의 머리에는 원본 경로와 커밋을 적고, `driver/core/`는 커널 API를 부르지 않고 할당하지 않습니다. 릴리스는 시험서명한 드라이버 시험 묶음(`tools/ci/driver-test-kit.ps1`, `tools/testkit/`)을 싣고, 시험서명 PC에서 시험하는 분은 [`docs/testing/test-signed-pc.md`](docs/testing/test-signed-pc.md)를 따릅니다. 드라이버, 묶음, 시험 도구의 동작이나 출력이 바뀌면 그 문서의 명령과 합격 기준도 함께 고칩니다. 드라이버를 고치면 커밋 전에 아래를 Linux에서 돌립니다. MSBuild, InfVerif, CodeQL, 시험서명 설치는 `driver.yml`이 Windows 러너에서 합니다.

```sh
python3 tools/kcheck.py --analyze
cmake -S driver/core -B target/core -G Ninja -DCMAKE_CXX_COMPILER=clang++ -DSSR_SANITIZE=ON
cmake --build target/core && target/core/core_tests
```

## 화면 (M4)

- 앱은 `apps/spatial-softreceiver`(Tauri 2 셸 크레이트)이고 화면은 그 아래 `ui/`(Vite + TypeScript, 프레임워크 없음)입니다. 화면은 `Intent`를 보내고 `Snapshot`을 그릴 뿐 상태와 판단을 갖지 않습니다(ADR 0001). 화면 문구는 `i18n/`의 `[screen]` 표에 있고 `ui/src/text.ts`는 생성된 카탈로그 위의 얇은 층입니다(영어 화면에 한글이 섞이면 시험이 막습니다). 감독자 문장은 `supervisor::text::Message`가 `ssr_i18n::supervisor`로 렌더합니다. 언어는 `Snapshot.language`가 정합니다(ADR 0001).
- `ssr-types`의 `Intent`, `Snapshot`과 그 아래 타입을 바꾸면 `SSR_UI_BLESS=1 cargo test -p ssr-types --features ts --test ui_contract`로 `ui/src/bindings.ts`와 `ui/fixtures/`를 다시 만들어 함께 커밋합니다. 자료를 가진 enum은 `kind`/`value`로 태그하고, serde와 specta의 snake_case가 다른 숫자 섞인 이름(`Eac3`, `Arm64`)은 `#[serde(rename)]`을 명시합니다.
- 화면 폰트는 UAC 원격 승인기와 같은 IBM Plex Sans KR(400, 500, 600, 700)뿐입니다. 파일은 저장소에 넣지 않고 `tools/ui-fonts.mjs`가 npm 패키지 `@ibm/plex-sans-kr`에서 받아 SHA-256으로 대조합니다(`npm run fonts`, 빌드가 먼저 돌립니다). 고정폭 글꼴이 꼭 필요하면 Sarasa Mono K를 씁니다. 다른 글꼴은 고르지 않습니다.
- 화면 갤러리: `ui/e2e/gallery.json`의 장면 표대로 `e2e/gallery.spec.ts`가 모든 화면 상태를 한국어와 영어, 밝은 팔레트와 어두운 팔레트로 찍습니다(`npm run shots`). `ci.yml`은 찍어서 검사하고(콘솔 오류, 글꼴, 가로 넘침, 영어 화면의 한글, `tools/ui-gallery-issue.mjs --check`로 장면 표와 대조), `ui-gallery.yml`은 그림을 `ui-gallery` 브랜치에 올려 리뷰 이슈(`화면 갤러리: PR #N`, `화면 갤러리: master`)에 모읍니다. 화면 상태를 더하면 장면 표에 장면을 더하고, Rust 쪽 상태라면 `ui_contract.rs`에 시험 자료를 두 언어로 더합니다. 자료의 문장은 감독자 문장 그대로여야 합니다(`cargo test -p supervisor --test fixture_sentences`). 화면을 바꾼 PR이면 그 이슈의 링크를 사용자에게 알립니다.
- `ui/dist`는 컴파일 때 앱에 들어가므로(`tauri::generate_context!`) Windows 대상의 cargo 검사(clippy, doc, test)보다 먼저 `npm ci && npm run build`를 돌립니다. CI는 `.github/actions/ui-dist`가 합니다.

## 브랜치와 릴리스

- 기본 브랜치는 `master`입니다. 작업은 브랜치에서 하고 PR로 머지합니다.
- master에 머지하면 `release.yml`이 릴리스를 냅니다(ADR 0017). 머지가 곧 릴리스 결정입니다. 0.x 버전은 모두 사전 릴리스이고, 태그에 `-pre.N` 같은 꼬리표를 붙이지 않습니다.
- 커밋 제목은 `feat:`, `fix:`, `docs:`, `ci:`, `chore:` 같은 접두어 뒤에 한국어로 씁니다. `feat`는 다음 릴리스에서 minor를, 나머지는 patch를 올립니다.
- 공개 저장소라 CI 분량을 아끼지 않습니다. 결함을 잡는 잡이면 더합니다.
