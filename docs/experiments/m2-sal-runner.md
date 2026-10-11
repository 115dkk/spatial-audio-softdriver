# M2 GitHub 러너 실험

2026-10-12. PR #6의 후속 실험. 사용자가 GitHub 러너에 VB-CABLE과 SAL을 설치하고 실행하도록 승인했다.

이전 실험에서는 windows-2022/2025 모두 시험서명 켜짐, Secure Boot/HVCI 꺼짐,
오디오 장치 0개를 확인했다. 재부팅하면 잡이 이어지지 않았다.
이번에는 windows-2022의 기존 WDK와 현재 부팅 상태를 사용한다.

`M2 SAL runner experiment`는 실험 브랜치 push 또는 수동 실행으로 시작한다.
설치 스크립트는 GitHub-hosted 러너인지 확인한 뒤 실행한다.

- VB-CABLE Pack45: 공식 배포 URL에서 받고 SHA-256을 검사한다.
- SAL: `5c689815680f3ea5b09fb35ea4dbe63a3dc5bfb7`을 고정한다. 공개 릴리스 바이너리가 없어
  upstream README의 명령으로 러너에서 빌드한다. 구현과 패치는 열람하지 않는다(ADR 0005).
  컴파일 로그는 소스 조각을 포함할 수 있어 아티팩트에서 제외하고 오류 코드만 보고한다.
- BIO: 장치 시험, VB-CABLE 출력, SAL 링 녹화와 덤프 렌더를 실행한다.
  `sal_tone`은 SAL 문서의 48 kHz, 12채널, 32비트 PCM 형식과 마스크 `0x2D63F`로
  낮은 레벨의 채널별 시험 신호를 보낸다.
- 설치 실패에도 `always()` 단계에서 설치 로그, PnP 상태, 장치 시험 결과,
  생성된 덤프·WAV·보고서와 해시를 30일 보존한다. SAL 소스와 패치는 포함하지 않는다.

SAL이 없을 때도 통과하는 일반 장치 시험과 별개로, 이 실험은 SAL 링 열기 성공을 요구한다.
빈 녹화는 실패다. 합성 PCM의 성공 범위는 전송 경로이며, 게임/MAT와 Apple Music/JOC,
청취 및 구축기 대비 지연 비교는 각각 별도 증거가 필요하다.

참조: [이전 실험 PR #6](https://github.com/115dkk/spatial-audio-softdriver/pull/6),
[SAL 드라이버 설치 문서](https://github.com/barralutz/spatial-audio-lab/blob/5c689815680f3ea5b09fb35ea4dbe63a3dc5bfb7/driver/README.md),
[VB-CABLE 배포](https://vb-audio.com/Cable/).
