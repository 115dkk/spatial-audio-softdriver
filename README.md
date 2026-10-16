# Spatial SoftReceiver · 공간 소프트리시버

AV 리시버가 하던 일을 PC 안에서 소프트웨어로 하는 Windows 앱입니다. 가상 HDMI 장치로 게임, 음악, 영화의 소리를 받아 디코드하고 공간에 배치한 뒤, 둘 가운데 하나로 내보냅니다.

- **실제 스피커:** 다채널 DAC나 오디오 인터페이스와 앰프에 연결한 스피커(2.0부터 7.1.4까지)로 내보냅니다. 스피커 위치에 맞춘 패닝, 저음 관리(크로스오버, 서브우퍼, LFE), 거리에 맞춘 지연과 레벨 정렬, 채널별 룸 보정 FIR을 리시버 대신 합니다.
- **헤드폰:** 개인 측정 BRIR로 스피커가 있는 방을 헤드폰 안에 재현합니다.

Dolby Atmos 콘텐츠(DD+ JOC, Windows 공간음향의 MAT)도 받아 객체 위치 그대로 렌더합니다.

Dolby, Dolby Atmos는 Dolby Laboratories의 상표입니다. 이 프로젝트는 Dolby Laboratories와 관계가 없고 보증받지 않았습니다.

설계는 [`docs/Spatial SoftReceiver 아키텍처.html`](docs/Spatial%20SoftReceiver%20아키텍처.html)에, 드라이버 동결 전의 확장(업믹스, 플러그인 호스트, 시스템 소리)은 [`docs/Spatial SoftReceiver 확장 설계.html`](docs/Spatial%20SoftReceiver%20확장%20설계.html)에, 용어는 [`CONTEXT.md`](CONTEXT.md)에, 결정은 [`docs/adr/`](docs/adr)에 있습니다.

## 진행 상황

| 단계 | 만드는 것 | 상태 |
| --- | --- | --- |
| M0 계약 | 저장소, `ssr-types`, `ssr-abi` 초안, 정책 게이트, CI와 릴리스 | 완료 (v0.1.0) |
| M1 리플레이 | `bitstream`, `decode`(PCM·MAT·DD+), `render`, `brir`, `convolver`, `ssr-replay render` | v0.2.0. MAT 디코더와 블랙박스 골든만 남음(링 덤프 대기) |
| M2 실시간 | `engine`, `speakers`, `ssr-sys`(SAL 개발용 링, WASAPI 출력), `ssr-live` | 헤드폰과 스피커(2~32채널, ADR 0030) 출력의 실시간 경로를 개발 PC와 러너의 가상 케이블에서 확인. SAL의 12채널 PCM도 러너에서 확인. SAL PC에서 게임과 음악을 듣는 시험, 지연 비교, 실제 스피커 청취가 남음 |
| M3 드라이버 | `spatialsoftreceiver.sys`, ABI v1 동결 | 진행 중. 호스트 러너에서 세 엔드포인트(주, 음악, 패스스루) 재생, 링 읽기, 볼륨, 독점 정책, Driver Verifier와 IOCTL 퍼저, 엔진까지의 종단 듣기를 확인. 시험서명 PC의 확인과 동결이 남음 |
| M4 앱 | `supervisor`, `ssr-setup`, `spatial-softreceiver`(Tauri 셸과 화면), `ssr-guard`, Velopack | 진행 중. 감독자 상태기계(ADR 0013), Windows 호스트 어댑터와 `ssr-setup`(ADR 0032), Tauri 셸과 화면 골격(ADR 0001), 감시 프로세스 `ssr-guard`(ADR 0033), 설치본과 Velopack 업데이트(ADR 0001 추가), 프로필 의도와 객체 지도(ADR 0013 추가), 한국어·영어 문구 카탈로그와 언어 설정(ADR 0001 추가), 모든 화면 상태를 두 언어와 두 팔레트로 찍어 리뷰 이슈에 모으는 화면 갤러리(`ui-gallery.yml`)가 들어옴. 시험서명 PC의 실기 확인이 남음 |
| M5 서명·출시 | EV 인증서, 증명 서명 | 예정 |

기능별 상태는 [`features.toml`](features.toml)이 정본입니다.

## 크레이트

```
crates/
  ssr-types   장면, 레이아웃, 피드, 소스 형식, 포트 트레이트, 의도와 스냅샷
  ssr-abi     드라이버 계약: 기록, IOCTL, WAVEFORMATEX, 덤프, 생성된 C 헤더
  bitstream   IEC 61937 해체
  decode      형식별 어댑터 → 장면
  render      장면 → 방향 피드 (VBAP)
  convolver   피드 → 양이 신호
  brir        프로필 → PreparedBrir
  engine      실시간 스레드, 교차 링, 클럭 전략, 지연 계측
  supervisor  상태기계 (M4)
  ssr-sys     unsafe와 Windows 전용 코드 (링, 출력, 호스트 어댑터)
  ssr-setup-sys  관리자 작업(설치, 삭제, 형식, 복구)만 담는 격리 크레이트, ssr-setup만 링크 (ADR 0032)
  ssr-policy  워크스페이스 게이트
apps/
  ssr-replay  덤프 리더 (inspect, M1부터 render)
  ssr-live    드라이버 링(또는 SAL 개발 링)의 녹화와 실시간 재생 (시험용, 앱은 M4)
  ssr-setup   관리자 단발 도우미 (M4)
  spatial-softreceiver  Tauri 2 셸과 TypeScript 화면 (M4, ADR 0001)
```

## 빌드와 검사

Rust 툴체인은 `rust-toolchain.toml`(1.99.0)이 정합니다.

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p ssr-policy          # unsafe, 기능 등록부, Windows 의존성 게이트
cargo deny check                  # 라이선스, 권고, 소스
cargo test -p ssr-types --features ts --test ui_contract   # 화면 계약 파일(bindings.ts, fixtures)이 타입과 같은지
cd apps/spatial-softreceiver/ui; npm ci; npm run check; npm test; npm run build   # 화면 (Node 22)
./tools/check-c-header.ps1        # 드라이버 헤더를 MSVC로 C89, C11, C++17 컴파일 (Windows)
```

드라이버 계약을 바꾸면 C 헤더를 다시 생성합니다(v1 동결 뒤에는 ADR이 먼저입니다).

```powershell
$env:SSR_ABI_BLESS = '1'; cargo test -p ssr-abi --test c_header; $env:SSR_ABI_BLESS = $null
```

## 릴리스

master에 push할 때마다 `release.yml`이 출하 파일의 변경 여부를 보고 `v0.MINOR.PATCH`를 냅니다. 0.x는 모두 사전 릴리스입니다([ADR 0017](docs/adr/0017-prerelease-on-every-master-push.md)).

## 라이선스

사용자 모드의 소스는 [MIT](LICENSE)입니다. ASIO SDK에 닿는 `asio-sys`의 파일만 GPL-3.0-or-later이고, ASIO 입구(`ssrasio.dll`)가 들어간 릴리스부터는 사용자 모드 프로그램 전체를 GPLv3 조건으로 냅니다([ADR 0025](docs/adr/0025-asio-entrance.md)). 드라이버(M3)는 SysVAD에서 출발한 MS-PL로 두고 별개 프로그램으로 함께 배포합니다.
