# 0022. 손으로 쓰는 unsafe를 격리 크레이트 몇 개에 나눠 둔다

상태: 채택, 2026-10-12. ADR 0002의 'unsafe는 `bio-sys` 한 크레이트에만'과 'Windows 의존성은 `bio-sys`만'을 고칩니다.

## 배경

ADR 0002는 손으로 쓰는 unsafe를 `bio-sys` 하나에 모았습니다. ADR 0020과 0021로 unsafe가 필요한 곳이 둘 생깁니다. 플러그인 호스팅(VST 2 ABI, VST 3 COM, DLL 적재)은 반드시 unsafe를 쓰고, 퍼스트파티 업믹서는 가속이 필요할 때만 씁니다.

이것을 `bio-sys`에 넣으면 장치와 드라이버를 다루는 크레이트가 플러그인 ABI와 업믹서 가속까지 떠안아 검토할 범위가 섞입니다. 그리고 `bio-sys`는 `bio-types`와 `bio-abi`에 의존하므로, 그 안에 넣은 호스팅 코드는 HostVST로 따로 낼 수 없습니다.

## 결정

- 손으로 쓰는 unsafe를 가질 수 있는 크레이트를 격리 크레이트라고 부르고, 이름은 `-sys`로 끝냅니다. 지금은 `bio-sys` 하나이고, 이 ADR로 `hostvst-sys`와 `upmix-sys`가 자격을 얻습니다. 둘 다 아직 만들지 않았고, `upmix-sys`는 ADR 0020의 조건(벤치로 보인 필요)을 채울 때만 만듭니다.
- 격리 크레이트는 `unsafe-budget.toml`에 올라야 합니다. 처음 숫자는 크레이트를 만드는 PR에서 실제 개수로 정하고 이 ADR 끝에 덧붙입니다. 그 뒤 숫자를 올리거나 새 격리 크레이트를 더하려면 ADR이 먼저입니다.
- 격리 크레이트는 `bio-sys`와 같은 lint 표를 씁니다. 루트는 `#![deny(unsafe_code)]`이고 unsafe를 쓰는 모듈만 허용하며, `undocumented_unsafe_blocks`, `multiple_unsafe_ops_per_block`, `missing_safety_doc`을 deny로 둡니다.
- 격리 크레이트 바로 위에는 그것을 감싸는 안전한 크레이트를 둡니다(`hostvst-sys` 위에 `hostvst`, `upmix-sys` 위에 `upmix`). 그 위의 코드는 unsafe 없이 씁니다.
- Windows API 의존성(`windows`, `windows-sys`, `windows-core`, `wasapi`)은 `bio-sys`와 `hostvst-sys`만 가집니다. `upmix-sys`는 가지지 않습니다. `upmix`는 `engine`이 쓰고, `engine` 이하는 Linux에서 빌드되어야 하기 때문입니다.
- 링크 규칙은 다음과 같습니다.
  - `bio-sys`는 지금처럼 `apps/` 아래의 앱만 의존합니다(`bio-replay` 제외).
  - `hostvst-sys`는 `hostvst`만, `[target.'cfg(windows)'.dependencies]`로 의존합니다. `hostvst`는 앱만 의존합니다(`bio-plughost`, M4의 `bio-atmos`).
  - `upmix-sys`는 `upmix`만 의존합니다.
  - `engine`은 `hostvst-proto`와 `upmix`를 의존할 수 있습니다.
  - `hostvst`, `hostvst-sys`, `hostvst-proto`는 `bio-*` 크레이트에 의존하지 않습니다.
- `bio-policy`의 Windows 의존성 게이트는 허용 목록을 갖도록 바꾸고, 위 링크 규칙과 hostvst 계열의 독립성을 검사하는 게이트를 더합니다. 게이트는 첫 새 격리 크레이트를 더하는 PR에서 함께 바꿉니다. 그전까지는 지금 게이트가 `bio-sys`만 허용합니다.

## 결과

- unsafe를 검토할 크레이트가 하나에서 최대 셋으로 늘지만, 크레이트마다 다루는 대상(장치와 드라이버, 플러그인 ABI, 업믹서 가속)이 하나씩이라 검토 범위가 섞이지 않습니다.
- `CLAUDE.md`와 `CONTEXT.md`의 'unsafe는 `bio-sys`에만'을 '격리 크레이트에만'으로 바꿉니다.
- HostVST를 떼어 낼 때 unsafe와 Windows 의존성이 `hostvst-sys` 한 곳에 있으므로 크레이트 세 개를 그대로 옮기면 됩니다.
