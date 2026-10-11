# 0002. unsafe는 bio-sys에만 두고, 규칙은 게이트로 지킨다

상태: 채택, 2026-10-11. unsafe와 Windows 의존성을 가질 수 있는 크레이트의 범위는 ADR 0022가 고쳤습니다.

## 배경

드라이버 IOCTL, MMCSS, QPC를 다루려면 손으로 쓰는 unsafe가 필요합니다. unsafe가 여러 크레이트에 흩어지면 검토할 곳이 늘고, 리뷰만으로는 새 unsafe가 들어오는 것을 막지 못합니다. 등록하지 않은 기능이나 테스트 없이 '구현됨'으로 표시한 기능도 같은 이유로 리뷰에서 놓칩니다.

## 결정

- 손으로 쓰는 unsafe는 `bio-sys` 한 크레이트에만 둡니다. 다른 크레이트 루트에는 모두 `#![forbid(unsafe_code)]`를 겁니다.
- 크레이트별 허용량은 `unsafe-budget.toml`에 적습니다. 0이 아닌 크레이트는 `bio-sys`(32)뿐이고, 숫자를 올리려면 먼저 ADR을 씁니다.
- `bio-sys`는 루트에 `#![deny(unsafe_code)]`를 걸고 자기 lint 표를 가집니다. 워크스페이스의 `forbid`는 모듈 단위로 풀 수 없어서입니다. unsafe를 쓰는 모듈만 따로 허용합니다.
- 기능은 `features.toml`에 등록합니다. 구현됐다고 표시한 기능은 존재하는 검증 테스트를 하나 이상 가리켜야 합니다.
- Windows API를 부르는 의존성(`windows`, `windows-sys`, `wasapi`)은 `bio-sys`만 가집니다. `bio-sys`를 링크하는 것은 `apps/` 아래의 앱뿐이고, `bio-replay`는 링크하지 않습니다.
- `crates/bio-policy`의 테스트가 소스 트리를 직접 읽어 위 규칙을 검사하고, CI(`ci.yml`)가 모든 PR과 master push에서 돌립니다. Impulcifer 3.x의 `impulcifer-policy`에서 가져온 관례입니다.

## 결과

- 새 unsafe, 등록하지 않은 기능, 테스트 없는 '구현됨' 표시, Windows 의존성의 누수는 리뷰보다 먼저 CI에서 걸립니다.
- `engine` 이하가 Windows 없이 빌드되므로 덤프 재생을 Linux CI에서 검증합니다.
- 게이트는 빌드 설정을 거치지 않고 파일을 읽으므로, cfg나 빠진 `[lints] workspace = true`로 규칙을 피할 수 없습니다.
