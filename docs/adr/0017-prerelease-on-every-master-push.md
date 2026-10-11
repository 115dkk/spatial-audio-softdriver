# 0017. master에 push할 때마다 사전 릴리스를 낸다

상태: 채택, 2026-10-11

## 배경

릴리스를 매번 손으로 dispatch하면 빌드를 받아 볼 때마다 한 단계가 더 들고, 잊으면 확인할 빌드가 없습니다. 반면 M5 전에는 사용자에게 내놓을 앱이 없으므로, 어떤 빌드도 'latest' 릴리스가 되어서는 안 됩니다.

## 결정

- `release.yml`은 master push마다 돕니다. 게이트(`.github/scripts/release_gate.py`)는 가장 최근 `v*` 태그 이후 출하 파일(`crates/`, `apps/`, `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`에서 테스트, 벤치, 퍼즈, 문서, `bio-policy`를 뺀 것)이 바뀌었을 때만 릴리스합니다. 손으로 dispatch할 때는 `force`로 강제할 수 있습니다.
- 태그는 `v<워크스페이스 버전>-pre.<높이>`입니다. 높이는 그 커밋에서 닿는 커밋 수라서, 같은 커밋은 언제나 같은 태그가 되고 봇이 master에 커밋할 일이 없습니다.
- 모든 릴리스는 GitHub prerelease이고 'latest'를 차지하지 않습니다. 같은 태그로 다시 돌면 자산만 바꿉니다.
- 지금 내는 것은 개발 도구(`bio-replay`)의 Windows zip과 Linux tar.gz, `SHA256SUMS.txt`입니다. 앱은 M4부터 같은 워크플로에 더합니다.

## 결과

- 머지가 곧 릴리스 결정입니다. 문서나 CI만 바꾼 머지는 릴리스를 만들지 않습니다.
- 안정 릴리스로 넘어갈 때(M5)는 게이트를 바꿔 prerelease 표시를 빼고, 이 ADR을 대체하는 새 ADR을 씁니다.
- 저장소의 기본 브랜치는 `master`이므로 워크플로도 `master`를 봅니다.
