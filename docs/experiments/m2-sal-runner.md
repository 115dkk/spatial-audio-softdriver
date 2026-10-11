# M2 GitHub 러너 실험

2026-10-12. PR #6의 후속 실험. 사용자가 GitHub 러너에 VB-CABLE과 SAL을 설치하고 실행하도록 승인했다.

이전 실험에서는 windows-2022/2025 모두 시험서명 켜짐, Secure Boot/HVCI 꺼짐,
오디오 장치 0개를 확인했다. 재부팅하면 잡이 이어지지 않았다.
빌드는 windows-2022의 기존 WDK를 사용하고, 설치·실행은 windows-2025에서 한다.
SAL의 SysVAD INF는 빌드 22621 이상을 요구하므로 Server 2022에서는 설치되지 않는다.

`M2 SAL runner experiment`는 실험 브랜치 push 또는 수동 실행으로 시작한다.
설치 스크립트는 GitHub-hosted 러너인지 확인한 뒤 실행한다.

- VB-CABLE Pack45: 공식 배포 URL에서 받고 SHA-256을 검사한다.
- SAL: `5c689815680f3ea5b09fb35ea4dbe63a3dc5bfb7`을 고정한다. 공개 릴리스 바이너리가 없어
  upstream README의 명령으로 러너에서 빌드한다. 구현과 패치는 열람하지 않는다(ADR 0005).
  컴파일 로그는 소스 조각을 포함할 수 있어 아티팩트에서 제외하고 오류 코드만 보고한다.
  러너가 만든 임시 인증서로 카탈로그를 시험서명한다. 바이너리 패키지와 공개 인증서만
  `sal-test-package` 아티팩트로 다음 잡에 넘긴다. 개인 키와 SAL 구현 파일은 포함하지 않는다.
- BIO: 장치 시험, VB-CABLE 출력, SAL 링 녹화와 덤프 렌더를 실행한다.
  `sal_tone`은 SAL 문서의 48 kHz, 12채널, 32비트 PCM 형식과 마스크 `0x2D63F`로
  낮은 레벨의 채널별 시험 신호를 보낸다.
- 실시간 경로: 같은 신호를 SAL 링 → BIO 엔진 → VB-CABLE로 보내고 `sal-live.biodump`에 기록한다.
  `cable_capture`는 CABLE Output에서 10초 녹음해 `cable-output.wav`를 만들고,
  양쪽 귀에 유효한 신호가 있는지 확인한다. 녹음 중에는 메모리에 모으고 종료 후 WAV를 쓴다.
- 설치 실패에도 `always()` 단계에서 설치 로그, PnP 상태, 장치 시험 결과,
  생성된 덤프·WAV·보고서와 해시를 30일 보존한다. SAL 소스와 패치는 포함하지 않는다.

SAL이 없을 때도 통과하는 일반 장치 시험과 별개로, 이 실험은 SAL 링 열기 성공을 요구한다.
빈 녹화는 실패다. 합성 PCM의 성공 범위는 전송 경로이며, 게임/MAT와 Apple Music/JOC,
청취 및 구축기 대비 지연 비교는 각각 별도 증거가 필요하다.

## 러너에서 발견한 준비 조건

- VB-CABLE Pack45는 `-i -h` 설치 뒤 재부팅 없이 출력 장치 두 개를 제공했다.
  독점 모드 48 kHz/16비트/96프레임(2 ms)과 공유 모드 시험이 통과했다.
- SAL의 Microsoft SysVAD 하위 저장소와 그 아래 WIL까지 재귀적으로 받아야 한다.
- upstream WDK 설치기는 `winget.exe`를 요구한다. Server 2022 러너의 기존
  SDK/WDK 26100을 직접 사용한다.
- 개별 프로젝트를 MSBuild로 빌드할 때 `SolutionDir`를 명시한다.
  공통 `OutDir` 재정의는 SysVAD의 내부 라이브러리 참조를 깨뜨리므로 프로젝트 기본값을 유지한다.
- Server 2022의 설치 실패는 SetupAPI의 `0xe0000228`(호환 드라이버 없음)이었다.
  Microsoft SysVAD INF의 최소 빌드가 22621인 것을 확인하고 실행 잡을 2025로 옮겼다.
- 합성 덤프 재생의 종료 후 drain 구간에도 underrun 계수가 증가한다.
  정상 스트리밍 중 상태 줄과 종료 합계를 구분해서 읽어야 한다.

## 실행 기록

| 실행 | 관찰 및 수정 |
| --- | --- |
| [38161363285](https://github.com/115dkk/spatial-audio-softdriver/actions/runs/38161363285) | VB-CABLE 설치와 장치 시험 통과. SAL 초기화 실패. 덤프 재생 중 p50 13.2 ms, p99 14.0 ms. |
| [38161645775](https://github.com/115dkk/spatial-audio-softdriver/actions/runs/38161645775) | SAL 초기화 통과. WDK 설치기의 winget 부재 확인. |
| [38161904344](https://github.com/115dkk/spatial-audio-softdriver/actions/runs/38161904344) | 공통 드라이버 프로젝트 빌드 통과. 패키지의 C1083은 누락된 WIL 하위 저장소로 수정. |
| [38162190411](https://github.com/115dkk/spatial-audio-softdriver/actions/runs/38162190411) | 패키지 빌드 통과. 설치기의 예상 경로에 산출물이 없어 출력 경로 명시. |
| [38162894250](https://github.com/115dkk/spatial-audio-softdriver/actions/runs/38162894250) | 패키지 생성·시험서명 통과. Server 2022에서 OS 호환성으로 설치 거절, 실행 잡을 2025로 변경. |

참조: [이전 실험 PR #6](https://github.com/115dkk/spatial-audio-softdriver/pull/6),
[SAL 드라이버 설치 문서](https://github.com/barralutz/spatial-audio-lab/blob/5c689815680f3ea5b09fb35ea4dbe63a3dc5bfb7/driver/README.md),
[VB-CABLE 배포](https://vb-audio.com/Cable/).
