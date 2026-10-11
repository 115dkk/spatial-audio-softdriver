# 저장소 작업 규약

바꾸기 전에 [`CONTEXT.md`](CONTEXT.md)의 용어와 불변식, 관련 [ADR](docs/adr)을 읽습니다. 설계 전체는 `docs/BIO Atmos 아키텍처.html`이고, 드라이버 동결 전의 확장(업믹스, 플러그인 호스트, 시스템 소리)은 `docs/BIO Atmos 확장 설계.html`입니다.

## 지킬 것

- unsafe는 격리 크레이트(`unsafe-budget.toml`에 오른 `-sys` 크레이트, 지금은 `bio-sys`)에만 둡니다. 격리 크레이트를 뺀 새 크레이트 루트에는 `#![forbid(unsafe_code)]`를 걸고 Cargo.toml에 `[lints] workspace = true`를 둡니다. 격리 크레이트는 `bio-sys`처럼 `#![deny(unsafe_code)]`와 자기 lint 표를 씁니다(ADR 0022). 숫자를 올리거나 격리 크레이트를 더하려면 ADR이 먼저입니다. `hostvst-sys`, `asio-sys`, `upmix-sys`는 ADR 0022가 허용했고, 처음 만드는 PR에서 숫자와 게이트를 함께 정합니다.
- Windows API 의존성(`windows`, `windows-sys`, `wasapi`)은 `bio-sys`만 가집니다(`hostvst-sys`와 `asio-sys`가 생기면 그 크레이트도). `engine` 이하는 Linux에서 빌드되고 테스트되어야 합니다.
- 외부 플러그인(VST 2, VST 3)은 엔진 프로세스에서 돌리지 않고 `bio-plughost`에서만 돌립니다(ADR 0021). `streamring`과 hostvst 계열 크레이트는 `bio-*`에 의존하지 않습니다.
- ASIO SDK 파일은 저장소에 넣지 않고 빌드 때 받아 SHA-256으로 고정합니다. EqualizerAPO-XT의 ASIO 래퍼에서 옮긴 커밋에는 원본 파일과 커밋을 적습니다(ADR 0025).
- 기능을 더하면 `features.toml`에 등록하고, `implemented`로 표시할 때는 실제로 있는 테스트를 가리킵니다.
- 실시간 스레드에서는 할당, 잠금, 파일 입출력을 하지 않습니다.
- 클린룸(ADR 0005): SpatialAudioLab의 소스와 그 소스에서 나온 패치는 읽지 않고, 작업자에게도 넘기지 않습니다. 구축기 공유 zip의 `sal-cli-bio-game.patch`가 여기에 해당합니다.
- 드라이버 계약(`crates/bio-abi/src/wire.rs`)을 바꾸면 `BIO_ABI_BLESS=1`로 C 헤더를 다시 생성해 함께 커밋합니다.

## 검사

커밋 전에 아래를 통과시킵니다. CI(`ci.yml`)가 같은 것을 Linux와 Windows에서 돌립니다.

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
```

## 화면 (M4)

- 화면 폰트는 UAC 원격 승인기와 같은 IBM Plex Sans KR(400, 500, 600, 700)입니다. 파일과 라이선스는 `E:\Windows_UAC_Remote_Controller\ui\public\fonts\ibm-plex-sans-kr\`에 있고, 그 저장소의 `tools/ui-fonts.mjs`처럼 SHA-256으로 고정해 들입니다.
- 고정폭 글꼴이 꼭 필요하면 Sarasa Mono K를 씁니다. 다른 글꼴은 고르지 않습니다.

## 브랜치와 릴리스

- 기본 브랜치는 `master`입니다. 작업은 브랜치에서 하고 PR로 머지합니다.
- master에 머지하면 `release.yml`이 릴리스를 냅니다(ADR 0017). 머지가 곧 릴리스 결정입니다. 0.x 버전은 모두 사전 릴리스이고, 태그에 `-pre.N` 같은 꼬리표를 붙이지 않습니다.
- 커밋 제목은 `feat:`, `fix:`, `docs:`, `ci:`, `chore:` 같은 접두어 뒤에 한국어로 씁니다. `feat`는 다음 릴리스에서 minor를, 나머지는 patch를 올립니다.
- 공개 저장소라 CI 분량을 아끼지 않습니다. 결함을 잡는 잡이면 더합니다.
