# 저장소 작업 규약

바꾸기 전에 [`CONTEXT.md`](CONTEXT.md)의 용어와 불변식, 관련 [ADR](docs/adr)을 읽습니다. 설계 전체는 `docs/BIO Atmos 아키텍처.html`입니다.

## 지킬 것

- unsafe는 `bio-sys`에만 둡니다. 새 크레이트 루트에는 `#![forbid(unsafe_code)]`를 걸고 Cargo.toml에 `[lints] workspace = true`를 둡니다. `unsafe-budget.toml`의 숫자를 올리려면 ADR이 먼저입니다.
- Windows API 의존성(`windows`, `windows-sys`, `wasapi`)은 `bio-sys`만 가집니다. `engine` 이하는 Linux에서 빌드되고 테스트되어야 합니다.
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

## 브랜치와 릴리스

- 기본 브랜치는 `master`입니다. 작업은 브랜치에서 하고 PR로 머지합니다.
- master에 머지하면 `release.yml`이 사전 릴리스를 냅니다(ADR 0017). 머지가 곧 릴리스 결정입니다.
- 커밋 제목은 `feat:`, `fix:`, `docs:`, `ci:`, `chore:` 같은 접두어 뒤에 한국어로 씁니다.
