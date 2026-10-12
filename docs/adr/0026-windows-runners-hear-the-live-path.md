# 0026. Windows 러너가 가상 오디오 케이블로 실시간 경로를 듣는다

상태: 채택, 2026-10-12

## 배경

개발은 Linux 컨테이너에서 하고, Windows에서만 도는 코드(`bio-sys`의 WASAPI, MMCSS, `bio-live`)는 CI의 Windows 러너에서만 실행됩니다. 그런데 GitHub의 Windows 이미지에는 소리 장치가 없어서, 장치가 필요한 시험은 모두 `#[ignore]`로 빠져 있었고 실시간 경로 전체를 돌리는 시험은 개발 PC에서만 돌았습니다. `cfg(windows)` 코드는 Windows에서 컴파일만 되고 clippy를 거치지 않았습니다.

## 결정

- **린트:** clippy와 rustdoc을 `x86_64-pc-windows-msvc`와 `aarch64-pc-windows-msvc` 대상으로도 돌립니다. 형식 검사에는 링커가 필요 없으므로 Linux 잡에서 합니다.
- **시험 행렬:** `cargo test`와 리플레이 결정성을 Windows Server 2025(windows-latest)와 2022, 그리고 Windows 11 ARM64에서도 돌립니다.
- **가상 케이블:** audio 잡은 Windows Server 2022와 2025 러너에 VB-CABLE을 설치합니다. 렌더 엔드포인트 'CABLE Input'에 쓴 소리가 캡처 엔드포인트 'CABLE Output'으로 돌아옵니다. 패키지는 CI가 돌 때 제작자의 사이트에서 받아 SHA-256으로 고정하고, 저장소에 넣거나 다시 배포하지 않습니다. 장치 노드는 devcon과 같은 SetupAPI 호출로 만듭니다(`tools/ci/install-virtual-cable.ps1`).
  - VB-CABLE은 Vincent Burel의 기부형 소프트웨어(donationware)이고, 라이선스는 사용과 원본 그대로의 복사를 허락하며 다른 소프트웨어의 설치 절차에 넣는 것만 막습니다. 우리는 CI에서 쓰기만 합니다.
  - Microsoft 증명 서명(attestation) 드라이버는 Windows Server에서 로드되지 않으므로, Server 러너에서 로드되는 서명을 가진 가상 장치는 VB-CABLE이 사실상 유일합니다.
- **장치 시험:** `bio-sys`가 `#[ignore]`로 둔 시험(독점과 공유 출력의 주기, 놓친 주기 뒤 회복, MMCSS, SAL 없음)을 이 장치로 돌립니다.
- **루프백:** `bio-live play --from-dump`로 픽스처 덤프를 'CABLE Input'에 재생하는 동안 'CABLE Output'을 녹음하고, 같은 덤프를 `bio-replay`로 오프라인 렌더링한 결과와 비교합니다. 실시간 경로는 케이블의 클럭에 맞춰 리샘플하므로 표본 단위로 같을 수 없습니다. 그래서 50 ms 창마다 기준 신호를 녹음에서 찾아(창마다 ±64 표본 안에서 위치를 따라감) 정규화 상관이 0.95 이상인 창이 90 % 이상이어야 통과입니다(`tools/ci/loopback_check.py`). 독점과 공유, PCM 7.1과 DD+ 5.1을 모두 봅니다.
- **소크:** `bio-live play --loop --report`로 독점 5분, 공유 2분, DD+ 2분, 모든 논리 프로세서를 보통 우선순위로 바쁘게 만든 채 독점 2분, 전체 페이지 힙(`gflags +hpa`와 같은 IFEO 설정) 아래 1분을 돌립니다. 소스가 흐르는 동안의 언더런, 장치 재시작, 유실 바이트, 넘침, 장애가 하나라도 있으면 실패입니다(`tools/ci/soak.ps1`). 결과는 잡 요약에 표로 남깁니다.

## 결과

- WASAPI 출력, 실시간 스레드, 클럭 보정, 지연 측정이 PR마다 실제 Windows 오디오 스택 위에서 돕니다. 소리가 장치까지 간다는 것도 녹음으로 확인합니다.
- 기준은 언더런 0입니다. CI 가상 머신의 시간 지터로 이 기준이 흔들리면, 기준을 느슨하게 하기 전에 원인(우선순위, 주기, 버퍼)을 먼저 찾습니다.
- 우리 드라이버는 여기서도 로드할 수 없습니다(시험서명 모드는 재부팅이 필요). 드라이버를 거치는 경로는 여전히 시험서명 PC에서 확인합니다(ADR 0027).
