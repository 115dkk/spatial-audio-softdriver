# spatial-audio-softdriver (BIO Atmos)

게임과 Apple Music이 내보내는 Dolby Atmos를 가상 HDMI 장치로 받아, 개인 측정 BRIR로 헤드폰에 렌더링하는 Windows 앱입니다. 'BIO Atmos'는 작업용 이름입니다.

설계는 [`docs/BIO Atmos 아키텍처.html`](docs/BIO%20Atmos%20아키텍처.html)에, 용어는 [`CONTEXT.md`](CONTEXT.md)에, 결정은 [`docs/adr/`](docs/adr)에 있습니다.

## 진행 상황

| 단계 | 만드는 것 | 상태 |
| --- | --- | --- |
| M0 계약 | 저장소, `bio-types`, `bio-abi` 초안, 정책 게이트, CI와 릴리스 | 완료 (v0.1.0) |
| M1 리플레이 | `bitstream`, `decode`(PCM·MAT·DD+), `render`, `brir`, `convolver`, `bio-replay render` | v0.2.0. MAT 디코더와 블랙박스 골든만 남음(링 덤프 대기) |
| M2 실시간 | `engine`, `bio-sys`(SAL 개발용 링, WASAPI 출력), `bio-live` | 실시간 경로는 개발 PC에서 덤프로 확인. SAL PC에서 듣는 시험과 지연 비교가 남음 |
| M3 드라이버 | `bioatmos.sys`, ABI v1 동결 | 예정 |
| M4 앱 | `supervisor`, Tauri 화면, `bio-setup`, Velopack | 예정 |
| M5 서명·출시 | EV 인증서, 증명 서명 | 예정 |

기능별 상태는 [`features.toml`](features.toml)이 정본입니다.

## 크레이트

```
crates/
  bio-types   장면, 레이아웃, 피드, 소스 형식, 포트 트레이트, 의도와 스냅샷
  bio-abi     드라이버 계약: 기록, IOCTL, WAVEFORMATEX, 덤프, 생성된 C 헤더
  bitstream   IEC 61937 해체
  decode      형식별 어댑터 → 장면
  render      장면 → 방향 피드 (VBAP)
  convolver   피드 → 양이 신호
  brir        프로필 → PreparedBrir
  engine      실시간 스레드, 교차 링, 클럭 전략, 지연 계측
  supervisor  상태기계 (M4)
  bio-sys     unsafe와 Windows 전용 코드 전부
  bio-policy  워크스페이스 게이트
apps/
  bio-replay  덤프 리더 (inspect, M1부터 render)
  bio-live    SAL 개발 PC용 녹화와 실시간 재생 (M2, 출하하지 않음)
  bio-setup   관리자 단발 도우미 (M4)
```

## 빌드와 검사

Rust 툴체인은 `rust-toolchain.toml`(1.99.0)이 정합니다.

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p bio-policy          # unsafe, 기능 등록부, Windows 의존성 게이트
cargo deny check                  # 라이선스, 권고, 소스
./tools/check-c-header.ps1        # 드라이버 헤더를 MSVC로 C89, C11, C++17 컴파일 (Windows)
```

드라이버 계약을 바꾸면 C 헤더를 다시 생성합니다(v1 동결 뒤에는 ADR이 먼저입니다).

```powershell
$env:BIO_ABI_BLESS = '1'; cargo test -p bio-abi --test c_header; $env:BIO_ABI_BLESS = $null
```

## 릴리스

master에 push할 때마다 `release.yml`이 출하 파일의 변경 여부를 보고 `v0.MINOR.PATCH`를 냅니다. 0.x는 모두 사전 릴리스입니다([ADR 0017](docs/adr/0017-prerelease-on-every-master-push.md)).

## 라이선스

사용자 모드는 [MIT](LICENSE)입니다. 드라이버(M3)는 SysVAD에서 출발한 MS-PL로 둡니다.
