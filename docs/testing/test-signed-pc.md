# 시험서명 PC에서 Spatial SoftReceiver 드라이버 시험하기

이 문서는 릴리스의 `spatial-softreceiver-driver-test-<버전>-x64.zip`에 `README.md`로 들어갑니다. 시험서명 모드를 켠 Windows PC에서, GitHub 호스트 러너로는 할 수 없는 드라이버 시험을 하는 방법을 적습니다. 시험하는 분이 AI 도우미와 함께 따라 할 수 있도록 명령, 합격 기준, 남길 파일을 단계마다 적습니다.

GitHub 러너가 PR마다 이미 하는 드라이버 시험(여기서 되풀이할 필요가 없는 것):
- 설치와 적재, 엔드포인트 마스크 `0x7`의 세 렌더 엔드포인트, 기본 형식 규칙(코덱 마스크 `0x6c`와 PCM 규칙: 표본율 `0x3f`, 표본 모양 `0x1f`, 마스크 채널 상한 18, 직접 출력 채널 `0xffffffff`)
- 엔드포인트마다 톤을 재생해 링으로 읽어 오기(공유 모드 기본값은 엔드포인트 0이 float32 7.1/48 kHz, 엔드포인트 1과 2가 float32 stereo/48 kHz. `collect.ps1`의 DeviceFormat 관찰값은 정보이고 합격 조건이 아님)
- 세 엔드포인트의 톤을 동시에 `ssr-live --source driver`로 받아 가상 케이블에서 듣기
- 독점 정책(0/1은 독점 거절, 2는 독점 허용)과 볼륨·음소거 기록
- 일시적 Driver Verifier를 켠 채 제어 장치 IOCTL 퍼저 60초
- 엔진: 헤드폰과 스피커 출력의 루프백(스피커는 가상 케이블이 받는 가장 넓은 레이아웃)

러너에는 실제 오디오 장치, 재부팅, Secure Boot와 HVCI, Windows 공간 음향, 재생기와 게임이 없습니다. 이 문서는 그것들이 필요한 시험을 다룹니다.

이 문서는 그 밖의 것을 두 부분으로 다룹니다.

- **M2 (SAL PC):** SpatialAudioLab(SAL)을 깐 PC에서만 할 수 있는 실시간 경로 시험입니다.
  - SAL 링 녹화
  - 게임과 음악 듣기
  - 지연을 구축기와 비교
  - USB DAC 재시작
  - 근거: [ADR 0015](../adr/0015-latency-as-numbers.md), [ADR 0018](../adr/0018-ssr-live-dev-app.md), [ADR 0019](../adr/0019-restart-a-stalled-exclusive-stream.md)
- **M3 (우리 드라이버):** 우리 커널 드라이버의 시험입니다.
  - Driver Verifier, 메모리 무결성(HVCI)
  - 공간 음향과 Dolby Atmos 협상, 비트스트림 형식
  - 쓰기 패킷 주기, 수명 경계
  - 근거: [ADR 0027](../adr/0027-driver-layout.md)과 [`driver/README.md`](../../driver/README.md)의 "시험서명 PC" 항목

SAL이 없는 PC라면 M2 부분은 건너뜁니다. 둘 다 할 수 있으면 **M2를 먼저** 합니다. 우리 드라이버를 깔기 전의, 익숙한 구축기 상태에서 기준을 잡기 위해서입니다.

> **이번 라운드는 드라이버와 직접 링 경로를 봅니다.** 장시간 게임과 안티치트 공존은 후속 시험으로 남기되, `ssr-live --source driver`로 헤드폰까지 듣는 시험은 아래 6-1단계에서 합니다.

## AI 도우미에게

- 이 문서에 적힌 명령만 실행하십시오. 레지스트리와 부팅 설정은 이 문서가 적은 값만 바꾸십시오.
- 단계마다 출력과 JSON 파일을 지우거나 고치지 말고 그대로 두십시오. 합격 여부는 각 단계의 "합격 기준"으로만 판단하고, 기준에 없는 일이 생기면 있는 그대로 적으십시오.
- 블루스크린(버그체크)이 나면 재부팅한 뒤 다른 일보다 먼저 [8단계의 수집](#8-결과-모으기와-보내기)을 실행하십시오.
- 결과를 공개된 곳(GitHub 이슈 등)에 올릴 때는 숫자 요약만 올리십시오. `.ssrdump`와 녹음 WAV에는 그때 재생한 소리가 그대로 들어 있으므로 저장소 소유자에게 따로 보냅니다.
- SAL과 구축기에서는 **출력만** 보냅니다. 덤프, 녹음, 화면, 숫자, 로그의 설정값이 여기에 해당합니다. SAL의 소스 코드, 그 소스에서 나온 패치(`sal-cli-bio-game.patch` 같은 것), SAL의 파일 자체는 보내지 마십시오. 이 저장소는 SAL을 보지 않고 만듭니다(클린룸, [ADR 0005](../adr/0005-clean-room.md)).

## 0. 준비

### 필요한 것

| 항목 | 내용 |
| --- | --- |
| PC | x64 Windows 11 22H2 이상 권장(24H2면 더 좋음). Windows 10 2004(빌드 19041) 이상이면 설치는 됩니다 |
| 권한 | 관리자 |
| Secure Boot | 꺼야 합니다. 켜져 있으면 시험서명 모드를 켤 수 없습니다 |
| 시험서명 모드 | `bcdedit /set testsigning on` 뒤 재부팅. 바탕 화면 오른쪽 아래에 "테스트 모드"가 보입니다 |
| Python | 3.10 이상([python.org](https://www.python.org/downloads/)). 설치할 때 "Add python.exe to PATH"를 켭니다 |
| 선택 | Dolby Access(Microsoft Store), 비트스트림 재생기(예: MPC-HC와 LAV Filters), Dolby Atmos·DTS 시험 영상, Atmos를 내는 게임 |
| M2 | 구축기 그대로의 SAL, 헤드폰과 DAC, 쓰던 BRIR(Impulcifer `hesuvi.wav` 또는 `hrir.wav`, 있으면 헤드폰 보정 FIR), Apple Music(Dolby Atmos 곡) |

**시작 전에 시스템 복원 지점을 만드십시오.** 커널 드라이버를 시험하고 Driver Verifier를 켜므로, 블루스크린과 재부팅이 날 수 있습니다.

### 시험서명 모드 켜기

1. UEFI(BIOS) 설정에서 Secure Boot를 끕니다. 메뉴 이름은 제조사마다 다릅니다.
2. 관리자 PowerShell에서:

   ```powershell
   bcdedit /set testsigning on
   ```

3. 재부팅합니다. 확인:

   ```powershell
   bcdedit /enum '{current}' | Select-String testsigning
   ```

   `testsigning Yes`가 보여야 합니다.

### 묶음 받기

1. 릴리스 페이지에서 다음을 받습니다.
   - `spatial-softreceiver-driver-test-<버전>-x64.zip` (M3)
   - `spatial-softreceiver-tools-<버전>-windows-x86_64.zip` (M2. `ssr-live.exe`, `ssr-replay.exe`)
   - `SHA256SUMS.txt`
2. 해시를 확인합니다:

   ```powershell
   Get-FileHash .\spatial-softreceiver-driver-test-*-x64.zip -Algorithm SHA256
   ```

   `SHA256SUMS.txt`의 값과 같아야 합니다. 도구 zip도 같은 방법으로 확인합니다.
3. 압축을 풀고, 인터넷에서 받은 표시를 지웁니다(관리자 PowerShell, 압축을 푼 폴더에서):

   ```powershell
   Get-ChildItem -Recurse | Unblock-File
   Set-ExecutionPolicy -Scope Process Bypass -Force
   ```

4. Python 패키지를 설치합니다:

   ```powershell
   py -m pip install -r requirements.txt soundcard==0.4.5
   ```

`kit.json`에 이 묶음의 버전, 커밋, 서명 인증서의 지문, 파일 해시가 있습니다.

### 시험 인증서에 대해

묶음의 드라이버는 릴리스를 만들 때 한 번 쓰고 버린 인증서(`CN=Spatial SoftReceiver test signing <버전>`)로 서명했습니다. 그 인증서의 개인 키는 빌드 러너 밖으로 나가지 않았고 지워졌습니다. 그래서 이 인증서를 신뢰해도, 이 인증서로 다른 무엇이 서명될 수는 없습니다.

`install.ps1`은 이 인증서를 이 PC의 "신뢰할 수 있는 루트 인증 기관"과 "신뢰할 수 있는 게시자"에 넣습니다. `uninstall.ps1`이 다시 지웁니다.

## M2. SAL PC에서 하는 시험

이 부분은 우리 드라이버가 필요 없습니다. 도구 zip을 푼 폴더에서, **관리자** PowerShell로 합니다. SAL 링은 관리자만 열 수 있습니다.

### M2-0. 프로필 만들기

`ssr-live play`와 `ssr-replay render`는 출력 프로필(TOML)을 읽습니다. 헤드폰이면 아래의 BRIR 프로필이고, 실제 스피커로 듣는 프로필은 6-2절에 있습니다. 도구 폴더에 `profile.toml`을 만들고, 경로를 쓰던 BRIR로 바꿉니다. Windows 경로는 작은따옴표 문자열로 씁니다.

```toml
name = "ssr-test"
gain_db = 0.0
limiter_ceiling_dbfs = -1.0

[brir]
hesuvi = 'C:\BRIR\hesuvi.wav'          # 또는: hrir = 'C:\BRIR\hrir.wav'
# headphone_fir = 'C:\BRIR\headphones.wav'

[lfe]
direction = "FC"
gain_db = 0.0
```

장치 목록을 확인합니다:

```powershell
.\ssr-live.exe devices
```

헤드폰 DAC의 이름 일부를 적어 둡니다. 아래 `--output`에 씁니다.

### M2-1. SAL 링 녹화 (MAT와 DD+ 덤프)

MAT 디코더와 블랙박스 골든 시험의 재료입니다. 구축기에서 하던 대로 SAL 쪽 공간 음향(Dolby Atmos for home theater)을 켠 상태에서, 아래를 재생하는 동안 녹화합니다.

| 덤프 이름 | 재생할 것 | 녹화 명령 |
| --- | --- | --- |
| `sal-game-joc.ssrdump` | Atmos 게임(예: 오버워치 2)의 한 판 일부 | `.\ssr-live.exe record --source sal --out sal-game-joc.ssrdump --seconds 60` |
| `sal-music-joc.ssrdump` | Apple Music의 Dolby Atmos 곡 하나, 처음부터 60초 | `.\ssr-live.exe record --source sal --out sal-music-joc.ssrdump --seconds 60` |
| `sal-music-stereo.ssrdump` | 같은 앱의 스테레오 곡 30초 | `--out sal-music-stereo.ssrdump --seconds 30` |
| `sal-ddp.ssrdump` | E-AC-3(DD+)을 SAL로 비트스트림하는 재생(가능하면 JOC) 30초 | `--out sal-ddp.ssrdump --seconds 30` |

녹화마다 확인합니다:

```powershell
.\ssr-replay.exe inspect sal-game-joc.ssrdump
```

**합격 기준**
- `inspect`의 형식이 재생한 것과 맞음(Atmos → MAT, DD+ → E-AC-3)
- 유실(losses)이 0

**골든 기준 녹음(선택).** `sal-music-joc`와 같은 곡의 같은 60초를 구축기로 재생하면서, 헤드폰 출력의 WASAPI 루프백을 32비트 WAV로 녹음해 `builder-music-joc.wav`로 남깁니다. Audacity라면 호스트 "Windows WASAPI", 녹음 장치는 헤드폰 출력의 "(loopback)"입니다. 쓴 BRIR과 구축기 설정을 메모에 적습니다.

### M2-2. 듣기 (게임과 음악)

구축기를 끈 상태에서 `ssr-live`가 SAL 링을 받아 헤드폰으로 내게 합니다. SAL 쪽 공간 음향은 켠 그대로 둡니다.

게임(독점 모드, 장치 최소 주기, 링 목표 10 ms):

```powershell
.\ssr-live.exe play --source sal --profile profile.toml --output "<DAC 이름 일부>" --target-ms 10 --report play-game.json
```

음악(링 목표 40 ms):

```powershell
.\ssr-live.exe play --source sal --profile profile.toml --output "<DAC 이름 일부>" --target-ms 40 --report play-music.json
```

같은 것을 공유 모드로 한 번 더: `--shared --report play-music-shared.json`.

Enter로 멈추면 `--report` 파일에 끊김, 장치 재시작, 지연, 링 바닥, 클럭 보정이 남습니다. 화면에도 1초마다 나옵니다.

들으며 적을 것:
- 끊김이나 잡음이 들렸는지
- 위치감과 음색이 구축기와 비교해 어떤지(같음/나음/못함과 한 줄 설명)
- 게임에서 소리가 화면보다 늦게 느껴지는지

**합격 기준**
- 재생 중 `underruns` 0. 언더런이 나면 ssr-live가 그 초의 줄 아래에 `underrun: ring … frames, markers …, layout …, needed …, produced …, last block … ms ago`를 찍고, `--report`의 JSON에도 `last_underrun`으로 남깁니다. 이 줄을 결과에 그대로 붙여 주십시오. 링이 비었는지(`ring`), 캡처가 멈췄는지(`last block`)를 가를 수 있습니다
- `output_restarts` 0. 아니면 그 DAC 이름과 함께 적습니다(ADR 0019)
- 들리는 문제 없음

### M2-3. 지연을 구축기와 비교 (M2의 끝나는 조건)

구축기는 체인 전체의 지연을 숫자로 잰 적이 없습니다(ADR 0015). 그래서 다음 셋을 함께 받습니다.

1. `play-game.json`의 지연 p50과 p99(ms). 소스 기록 시각부터 장치 재생 시각까지입니다.
2. 같은 게임, 같은 장면에서 구축기와 `ssr-live`를 번갈아 들은 비교. 발소리나 총소리처럼 화면과 맞춰 볼 수 있는 소리로 합니다. 결과는 "ssr-live가 더 늦음/같음/더 빠름"입니다.
3. 구축기가 로그나 설정에 남기는 단계별 버퍼와 지연 값이 있으면 그 값. 값만 적고 파일은 보내지 않습니다.

**합격 기준**
- 2에서 "ssr-live가 더 늦음"이 아님
- 1의 p50이 3의 합보다 크지 않음(3이 있을 때)
- 1의 p50과 p99가 모두 1초(1000 ms) 아래. 넘으면 계측 결함입니다. 수백 초처럼 PC가 켜져 있던 시간에 가까운 값이면 그 값을 그대로 적어 주십시오

## 1. 설치

관리자 PowerShell, 묶음 폴더에서:

```powershell
.\install.ps1
```

| 종료 코드 | 뜻 |
| --- | --- |
| 0 | 드라이버가 올라왔고 `EndpointMask`가 고른 렌더 엔드포인트가 모두 활성 |
| 2 | 사전 조건이 맞지 않음(관리자, OS 빌드, 시험서명 모드, 이미 설치됨). 출력된 안내를 따르십시오 |
| 3 | 설치는 됐지만 커널이 이미지를 거절(문제 코드 52). 시험서명 모드가 실제로 켜졌는지 다시 확인하십시오 |
| 4 | 올라왔지만 활성 엔드포인트 수나 고정 식별자이 `EndpointMask`와 맞지 않음 |
| 1 | 그 밖의 실패 |

**합격 기준**
- 종료 코드 0
- 설정 → 시스템 → 소리 → 출력 목록에 `Spatial SoftReceiver`, `Spatial SoftReceiver Music`, `Spatial SoftReceiver PASS`가 보임
- `install-result.json`이 묶음 폴더에 생김

영어 Windows에서는 위 영어 이름, 한국어 Windows에서는 아래 한국어 이름이 보여야 합니다. 사용자가 이름을 바꿔도 검사기는 이름 대신 고정 KS 필터 식별자로 번호를 알아냅니다.

| 번호 | 영어 | 한국어 |
| --- | --- | --- |
| 0 | Spatial SoftReceiver | 공간 소프트리시버 |
| 1 | Spatial SoftReceiver Music | 공간 소프트리시버 음악 |
| 2 | Spatial SoftReceiver PASS | 공간 소프트리시버 PASS |

패키지 파일은 `spatialsoftreceiver.sys`, `spatialsoftreceiver.inf`, `spatialsoftreceiver.cat`, 서비스는 `SpatialSoftReceiver`, 하드웨어 ID는 `ROOT\SpatialSoftReceiver`입니다. zip 파일 이름은 이번에 바꾸지 않았습니다. `uninstall.ps1`은 이전 시험판의 장치·서비스·패키지·시험 인증서도 함께 찾아 지웁니다. 옛 시험판이 있으면 먼저 이 제거기를 실행한 뒤 새 묶음을 설치하십시오.

`powershell -NoProfile -ExecutionPolicy Bypass -File .\endpoints.ps1`의 JSON에 `endpoint`, `id`, `name`, `state`가 나옵니다. 이름을 한국어 또는 다른 별칭으로 바꾼 뒤에도 0/1/2 대응과 기본 흐름·볼륨·독점 정책 시험이 그대로 통과해야 합니다.

**시험서명 PC에서 확인할 이름 계약:** 한국어 Windows의 새 설치에서 한국어 이름이 보이는지 확인하십시오. 서비스 `HKLM\SYSTEM\CurrentControlSet\Services\SpatialSoftReceiver\Parameters`의 `EndpointName0` REG_SZ를 1~31 UTF-16 코드 단위의 시험 이름으로 바꾼 뒤 해당 PnP 장치만 다시 시작하여 HDMI 싱크 설명이 그 이름을 쓰는지 확인합니다. 원래 레지스트리 값을 기록하고 시험 후 복원·장치 재시작하십시오. 누락·빈 값·32자·비 REG_SZ는 영어 기본값으로 돌아가야 합니다. Windows가 저장한 사용자 별칭과 싱크 설명은 구별해서 기록합니다. 서명 패키지 자체를 편집하지 마십시오.

기본 `EndpointMask`는 `0x7`입니다. 비트 0은 **Spatial SoftReceiver**(기본 장치·시스템 소리·공간음향), 비트 1은 **Spatial SoftReceiver Music**(공유 음악), 비트 2는 **Spatial SoftReceiver PASS**(독점 비트스트림·PCM)를 켭니다. 미니페어가 없는 비트 3은 무시합니다. 비트 2를 끈 0x3으로 장치를 다시 시작하면 활성 패스스루 엔드포인트가 없어야 하고 QUERY_INFO도 0x3이어야 합니다. 이때 검사 명령의 기대 마스크도 3으로 바꿉니다. 0은 '전부 끔'이 아니라 기본값 0x7로 돌아갑니다.

설치기는 엔드포인트 생성 후 0/1의 독점 허용·우선권을 0/0, 2를 1/1로 쓰고 아래 검사를 실행합니다. `install-result.json`의 `exclusive_policy`와 `exclusive-policy-summary.json`을 확인하십시오. 정책 쓰기나 동작 검사가 실패하면 설치 종료 코드 1이며, 장치는 진단·제거할 수 있도록 남습니다. 전역 오디오 서비스를 다시 시작하지 않습니다.

## 2. 기본 흐름

세 엔드포인트에 서로 다른 톤을 동시에 재생하면서, 제어 장치 `\\.\SpatialSoftReceiver`에서 기록을 읽어 맞춥니다. 러너에서 도는 것과 같은 시험입니다.

```powershell
py .\driver_flow_check.py --expect-mask 0x7 --summary driver-flow-basic.json
```

**합격 기준**
- 종료 코드 0
- `driver-flow-basic.json`에서 엔드포인트마다:
  - `gaps`가 0
  - `overruns`가 0
  - `tone_db`가 −6 dB 안팎
  - `other_tone_db`가 −60 dB 이하
  - `rate_error_ppm`의 절댓값이 100 이하
- `snapshot_skew_us`가 100 이하이고 `snapshot_reads_compared`가 1 이상. 한 번의 READ 안에서 엔드포인트들의 마지막 데이터가 같은 시각에서 끝나는지 봅니다. 드라이버가 모든 스트림을 함께 진행시키므로 차이는 한 프레임(수십 µs) 이하여야 합니다.

이 시험은 세 엔드포인트의 **공간 오디오가 꺼진 상태**에서 돌립니다. 켜져 있으면 공간 음향 엔진이 형식을 바꿔(예: MAT) 스테레오 톤 검사가 맞지 않습니다. 3단계 뒤에 이 시험을 다시 돌릴 때는 먼저 공간 오디오를 끕니다.

제어 장치는 핸들을 하나만 받습니다. 이 시험과 다음 단계의 녹화(`ring_record.py`)는 동시에 돌릴 수 없습니다.

## 2-0. 독점 정책

모든 플레이어를 종료한 뒤 64비트 관리자 PowerShell에서 실행합니다. 저장소에서 실행하면 스크립트 경로에 `tools/ci/`를 붙입니다.

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\exclusive_policy_check.ps1 -ExpectMask 7 -Summary exclusive-policy-summary.json
```

**합격 기준**
- 종료 코드 0, 요약의 `passed: true`
- 0/1의 `allow_exclusive`, `exclusive_priority`는 0/0, 2는 1/1
- 2는 `probe.supported: true`(독점 모드에서 지원하는 실제 PCM 형식)이고 `probe.initialize_hr`가 `0x00000000`
- 0/1은 독점 열기가 `0x8889000E`(`AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED`)로 거절됨. Windows는 형식 확인 단계(`probe.support_hr`)나 열기 단계(`probe.initialize_hr`) 가운데 한 곳에서 이 값을 돌려줍니다. 다른 오류 코드(예: 형식 미지원 `0x88890008`)는 정책의 증거가 아니므로 실패입니다.
- 장치 없음·중복·지원 형식 없음·사용 중은 건너뛰지 않고 실패

키는 공개 Windows SDK 계약이 아닌 private 속성입니다. 값만 맞고 동작이 다르면 합격이 아닙니다. 재부팅·장치 재시작·절전 복귀 후에도 재검사하십시오. 업데이트 뒤 검사가 실패하면 JSON과 수집 결과를 보내고, 임의로 서비스를 재시작하여 합격으로 바꾸지 마십시오.

패스스루의 독점 허용·우선권은 **공유 열기 금지가 아닙니다**. 기본 흐름 시험은 비어 있는 패스스루에도 공유 톤을 넣어 링에서 확인합니다. 일반 앱은 0/1로 보내십시오. 2에 잘못 보낸 공유 앱은 비트스트림이 열릴 때 끊길 수 있으며 supervisor의 공유 앱 감지·라우팅은 아직 없습니다.

## 2-1. 엔드포인트 볼륨과 음소거

`ssr-live`와 다른 링 판독기를 종료한 뒤 묶음 폴더에서 실행합니다. Python은 PATH에 있어야 합니다(`-Python py`로 런처 지정 가능).

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\volume_check.ps1 -Summary volume-summary.json
```

검사기는 QUERY_INFO에 나온 엔드포인트마다 하드웨어 지원 비트를 읽고 −20 dB와 음소거 켬/끔을 설정합니다. 별도 Python 프로세스가 그동안 링을 읽습니다. 원래 마스터·채널별 볼륨과 음소거는 성공 여부와 무관하게 복원을 시도합니다. `RESTORE FAILED`가 있으면 Windows 소리 설정에서 값을 직접 확인하십시오. `volume-summary.json`과 같은 폴더의 `volume-check-*` 증거를 보관합니다.

**합격 기준**
- 종료 코드 0, `passed: true`이며 모든 엔드포인트에 `QueryHardwareSupport`의 VOLUME(1)·MUTE(2) 비트가 있음
- 각 단계의 마지막 실제 변경 `VOLUME` 기록이 API 채널별 dB와 ±0.5 dB 이내이고 음소거 마스크가 일치함(재진술만으로는 합격하지 않음)
- 원래 마스터·채널별 볼륨과 음소거가 복원됨
- **엔진 적용 뒤:** 검사기를 종료하고 `ssr-live --source driver`로 재생할 때 소리 설정의 볼륨 슬라이더를 내리면 소리가 줄고, 음소거하면 조용해짐. PCM, MAT, DD+ 각각에서 이중 감쇠가 없는지 확인

## 3. 공간 음향과 Dolby Atmos 협상

우리 드라이버가 형식 목록만으로 Windows에 "Dolby Atmos for home theater"를 열어 주는지가 이 단계의 핵심입니다. 아키텍처 문서의 "확인하지 못한 것" 첫 항목입니다.

`Spatial SoftReceiver`(엔드포인트 0)와 `Spatial SoftReceiver Music`(엔드포인트 1)에 대해 각각:

1. 설정 → 시스템 → 소리 → 해당 출력 → **공간 오디오**의 목록에 무엇이 있는지 적고 화면을 캡처합니다. 예: 끔, Windows Sonic for Headphones, Dolby Atmos for Headphones, Dolby Atmos for home theater.
2. Dolby Access가 있으면 앱의 "홈 시어터" 설정에서 이 출력을 고를 수 있는지 적습니다.
3. "Dolby Atmos for home theater"를 고를 수 있으면 켭니다. 그다음 녹화를 시작하고, 녹화하는 동안 Dolby Access의 데모(또는 Atmos 게임)를 재생합니다:

   ```powershell
   py .\ring_record.py --seconds 30 --out joc-ep0.ssrdump
   ```

   `Spatial SoftReceiver Music`은 `--out joc-ep1.ssrdump`로 따로 녹화합니다.
4. `joc-ep0-summary.json`(화면에도 출력됨)에서 그 엔드포인트의 형식을 봅니다.

**합격 기준**
- "Dolby Atmos for home theater"가 목록에 있고 켤 수 있음
- 켠 뒤 재생 중의 형식(`formats[].name`)이 `IEC 61937 MAT 2.0` 또는 `IEC 61937 MAT 2.1` (8채널 192 kHz 컨테이너)
- 그 엔드포인트의 `byte_pos_discontinuities`, `overrun_records`, `lost_bytes`가 모두 0. 새 핸들은 알려진 FormatChange와 현재 StreamStart를 repeat로 재진술하고 다음 data를 discontinuity로 표시하며, 오버런 뒤에는 Overrun → 반복 FormatChange → 반복 현재 StreamStart/Stop 순서여야 합니다.

목록에 없으면 그 자체가 중요한 결과입니다. 스크린샷과 `collect.ps1` 결과를 보내 주십시오.

## 4. 비트스트림 패스스루 형식

재생기가 압축 오디오를 그대로(독점 모드로) 보낼 때 드라이버가 그 형식을 받아들이는지 봅니다. MPC-HC와 LAV Filters를 예로 듭니다.

기본 `CodecMask2=0x6c`에서는 DD+/MAT를 받지만 AC-3·TrueHD·DTS 계열은 꺼져 있습니다. PCM은 별도 규칙(`PcmRateMask2=0x3f`, `PcmShapeMask2=0x1f`, `PcmMaxMaskedChannels2=18`, `PcmDirectChannels2=0xffffffff`)으로 44.1~192 kHz의 여섯 표본율, 다섯 표본 모양, Windows 18개 스피커 비트의 배치 및 마스크 0의 1~32채널 직접 출력을 받습니다. 옛 `FormatMask2`는 읽지 않습니다. 기본으로 꺼진 코덱(AC-3, TrueHD, DTS 계열)의 협상은 지원 디코더에 맞춰 별도로 켠 시험 구성에서만 확인하며, 기본 구성에서 그 코덱이 거절되는 것은 드라이버 결함으로 보지 않습니다. 아래 재생과 함께 0으로 시스템 소리, 1로 공유 음악을 보내 둘이 끊기지 않는지도 확인합니다. PCM 형식의 범위는 4-1단계에서 봅니다.

1. LAV Audio Decoder 설정에서 비트스트리밍(AC-3, E-AC-3, TrueHD, DTS, DTS-HD)을 켭니다.
2. 오디오 렌더러를 MPC Audio Renderer로 하고, 독점 모드로 **`Spatial SoftReceiver PASS`** 출력을 고릅니다. `Spatial SoftReceiver`와 `Spatial SoftReceiver Music`은 독점을 거절합니다.
3. 형식마다 시험 영상을 재생하면서 녹화합니다:

   ```powershell
   py .\ring_record.py --seconds 20 --out passthrough-eac3.ssrdump
   ```

   AC-3, E-AC-3, TrueHD(MAT), DTS, DTS-HD MA, DTS:X 가운데 가진 파일로 합니다.

**합격 기준**
- 요약의 형식 이름(`formats[].name`)이 재생한 코덱과 맞음

  | 재생한 코덱 | 형식 이름 |
  | --- | --- |
  | AC-3 | `IEC 61937 AC-3` |
  | E-AC-3 | `IEC 61937 E-AC-3` (Atmos 표시가 있으면 `IEC 61937 E-AC-3 JOC`) |
  | TrueHD | `IEC 61937 TrueHD` |
  | DTS | `IEC 61937 DTS` |
  | DTS-HD MA | `IEC 61937 DTS-HD` |
  | DTS:X | `IEC 61937 DTS:X E1`, `E2`, 또는 `IEC 61937 DTS-HD` |

- `byte_pos_discontinuities`, `overrun_records`, `lost_bytes`가 0

재생기가 그 형식을 열지 못하고 PCM으로 떨어지면, 그 형식과 재생기 쪽 메시지를 적어 주십시오.

## 4-1. PCM 형식 범위

드라이버는 PCM을 표 대신 규칙으로 받습니다(ADR 0024의 2026-10-13 고침): 44.1~192 kHz의 여섯 표본율, 다섯 표본 모양(16비트, 24비트 packed, 24-in-32, 32비트 정수, float32), 그리고 채널 규칙(Windows 18개 스피커 비트의 아무 조합, 또는 마스크 0의 1~32채널 직접 출력)입니다. 이 단계는 높이 채널이 있는 PCM, 24비트 packed, 176.4·192 kHz, 직접 출력 16·24채널이 Windows와 재생기에서 실제로 열리는지 봅니다. 1과 2는 도구 zip의 `ssr-live.exe`로 합니다. 3과 4는 가능하면 합니다. 그런 형식을 내는 Windows 설정이나 재생기가 있어야 하기 때문입니다.

1. 드라이버가 알리는 규칙을 봅니다. 제어 장치를 여는 다른 프로그램(`ssr-live play`, `ring_record.py`)은 먼저 끕니다:

   ```powershell
   .\ssr-live.exe devices
   ```

   **합격 기준** (레지스트리 기본값일 때)
   - 노출된 엔드포인트마다 아래 두 줄이 나옴(엔드포인트 번호만 다름)

     ```text
       endpoint 0: codecs [E-AC-3, E-AC-3 JOC, MAT 2.0, MAT 2.1]
         PCM: rates [44100, 48000, 88200, 96000, 176400, 192000] Hz; shapes [int16, int24 packed, int24-in-32, int32, float32]; masked channels <= 18; direct-out channels [1-32]
     ```

2. Windows의 독점 형식 질의로 높이 채널 PCM을 봅니다:

   ```powershell
   .\ssr-live.exe devices --formats
   ```

   이 명령은 장치마다 48 kHz의 표준 배치(2, 4, 6, 8, 12채널)를 독점 모드로 받는지 묻습니다. 24비트 packed와 다른 표본율은 묻지 않습니다.

   **합격 기준**
   - `Spatial SoftReceiver PASS` 아래의 `12 channels mask 0x2d63f`(7.1.4)가 `exclusive yes`
   - 같은 장치의 2, 4, 6, 8채널 줄도 `exclusive yes`

3. **(가능하면) 공유 모드 기본 형식.** 제어판 소리(`mmsys.cpl`) → 재생 → `Spatial SoftReceiver Music` → 속성 → 고급의 기본 형식 목록을 적고 캡처합니다. 바꾸기 전의 값도 적어 둡니다. 목록에 24비트 176400 Hz나 192000 Hz 항목이 있으면 하나씩 골라 적용하고, 공간 오디오를 끈 채 2단계 명령을 다시 돌립니다:

   ```powershell
   py .\driver_flow_check.py --expect-mask 0x7 --summary driver-flow-pcm-rate.json
   ```

   **합격 기준**
   - 2단계의 합격 기준을 모두 만족
   - 엔드포인트 1의 `format.rate`가 고른 표본율이고 `format.sample`이 `int`. `format.bits`(24면 packed)는 그대로 적음
   - 끝나면 기본 형식을 적어 둔 원래 값으로 되돌림

4. **(가능하면) 재생기의 독점 PCM.** 독점 WASAPI 출력에서 형식을 고를 수 있는 재생기가 있으면, **`Spatial SoftReceiver PASS`** 로 아래 PCM을 하나씩 재생하면서 녹화합니다:

   ```powershell
   py .\ring_record.py --seconds 20 --out pcm-direct16.ssrdump
   ```

   | 시험 | 보낼 PCM | 요약(`<이름>-summary.json`) 엔드포인트 2의 `formats[]` |
   | --- | --- | --- |
   | 높이 채널 | 7.1.4(마스크 `0x2d63f`) 또는 다른 높이 배치, 48 kHz | `channel_layout`이 `speaker-mask`, `channels`와 `channel_mask`가 보낸 값 |
   | 24비트 packed | 24비트(3바이트) 정수 | `container_bits`가 24 |
   | 176.4·192 kHz | 176400 또는 192000 Hz | `container_rate`가 보낸 표본율 |
   | 직접 출력 16채널 | 채널 마스크 0, 16채널 | `channel_layout`이 `direct-out`, `channels`가 16 |
   | 직접 출력 24채널 | 채널 마스크 0, 24채널 | `channel_layout`이 `direct-out`, `channels`가 24 |

   `--out`의 이름은 시험마다 바꿉니다. 녹화를 끝낸 뒤 같은 재생을 6-1단계의 `ssr-live play`에 `--endpoints 2`를 붙여 듣습니다. 제어 장치는 핸들을 하나만 받으므로 녹화와 듣기는 따로 합니다.

   **합격 기준**
   - 요약의 `formats[]`가 위 표와 맞고, `byte_pos_discontinuities`, `overrun_records`, `lost_bytes`가 0
   - 높이 채널, 24비트 packed, 176.4·192 kHz, 직접 출력 16채널은 `ssr-live` 보고의 `endpoints[2].unsupported`가 `null`이고 소리가 들림. 16채널은 기본 배치 9.1.6으로 들림(6-1단계의 표)
   - 직접 출력 24채널은 `--direct-layout`이 없으면 `endpoints[2].unsupported`가 `PCM with channel mask 0 has no direct layout for its channel count`이고, 다른 엔드포인트의 소리는 끊기지 않음. 소스의 채널 순서대로 24개 이름을 `--direct-layout 24=<이름,...>`으로 주면 `unsupported`가 `null`이고 소리가 들림
   - 재생기가 그 형식을 열지 못하거나 다른 형식으로 바꿔 보냈으면, 고른 형식과 재생기 쪽 메시지를 그대로 적음

## 5. 쓰기 패킷 주기 (ADR 0014)

공간 음향 엔진이 드라이버의 쓰기 패킷 알림을 어떤 주기로 부르는지 잽니다. 기본은 꺼짐이고, 레지스트리로 켭니다.

1. 켜고 장치를 다시 시작합니다(관리자):

   ```powershell
   Set-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Services\SpatialSoftReceiver\Parameters' -Name WritePacket -Value 1 -Type DWord
   $id = (Get-PnpDevice -PresentOnly | Where-Object { $_.HardwareID -contains 'Root\SpatialSoftReceiver' }).InstanceId
   pnputil /restart-device "$id"
   ```

2. 3단계처럼 "Dolby Atmos for home theater"를 켜고 재생하면서 녹화합니다:

   ```powershell
   py .\ring_record.py --seconds 30 --out writepacket-joc.ssrdump
   ```

   PCM 스테레오 재생(일반 음악)으로도 한 번 더 합니다: `--out writepacket-pcm.ssrdump`.
3. 요약의 `qpc_written_intervals`(`min_ms`, `median_ms`, `p99_ms`, `max_ms`)를 적습니다.
4. 되돌립니다:

   ```powershell
   Set-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Services\SpatialSoftReceiver\Parameters' -Name WritePacket -Value 0 -Type DWord
   pnputil /restart-device "$id"
   ```

**합격 기준**
- 이 단계는 측정입니다. 숫자를 그대로 보내 주십시오.
- `qpc_written_intervals`가 `null`이면 엔진이 쓰기 패킷 경로를 쓰지 않은 것입니다. 그것도 결과입니다.

## 6. 수명 경계

각 항목 뒤에 2단계의 기본 흐름을 다시 돌려 소리가 다시 지나는지 확인합니다.

| 항목 | 하는 법 |
| --- | --- |
| 재생 중 장치 끄고 켜기 | 2단계를 돌리는 동안 다른 관리자 창에서 `Disable-PnpDevice -InstanceId $id -Confirm:$false`, 5초 뒤 `Enable-PnpDevice -InstanceId $id -Confirm:$false` |
| 장치 다시 시작 | `pnputil /restart-device "$id"` |
| 절전과 깨우기 | 음악을 Spatial SoftReceiver 출력으로 재생하는 동안 절전(`rundll32 powrprof.dll,SetSuspendState 0,1,0` 또는 시작 메뉴), 깨운 뒤 2단계 |
| 최대 절전 | 지원하면 같은 방법으로 |
| 재부팅 | 재부팅 뒤 장치 상태와 2단계 |
| `Spatial SoftReceiver`와 `Spatial SoftReceiver Music`의 다른 형식 | 한쪽은 공간 음향(Atmos), 다른 쪽은 PCM 음악을 동시에 재생하며 `ring_record.py --seconds 30` |

**합격 기준**
- 블루스크린 없음
- 장치가 문제 코드 없이 돌아옴
- 각 항목 뒤 2단계 합격

### 6-1. 드라이버 링 종단 듣기

시험서명 드라이버를 설치한 뒤, 도구 묶음의 `ssr-live.exe`와 자신의 BRIR `profile.toml`로 엔진이 드라이버 링을 직접 읽는 경로를 듣습니다. 먼저 `.\ssr-live.exe devices`에서 DAC 이름과 `Spatial SoftReceiver driver`의 엔드포인트 마스크·QPC 주파수, 그리고 엔드포인트마다 코덱 목록과 PCM 규칙(표본율, 표본 모양, 마스크 채널 상한, 직접 출력 채널)이 나오는지 확인합니다. 출력의 모양은 4-1단계에 있습니다.

```powershell
.\ssr-live.exe play --source driver --output "<자기 DAC 이름 일부>" --profile profile.toml --report driver-listen.json
```

1. Windows 기본 장치인 **Spatial SoftReceiver**로 게임과 시스템 소리를 보냅니다.
2. Apple Music 같은 음악 앱의 출력은 **Spatial SoftReceiver Music**으로 고릅니다.
3. 지원되는 DD+ 비트스트림 플레이어는 **Spatial SoftReceiver PASS**로 보내고 세 소리를 함께 1분 이상 듣습니다.
4. 한쪽 재생만 멈췄다가 다시 시작하고, 그동안 다른 쪽이 끊기지 않는지 듣습니다.
5. Enter로 끝낸 뒤 `driver-listen.json`을 결과 묶음 폴더에 둡니다.

`--endpoints`를 생략하면 드라이버가 노출한 엔드포인트를 모두 처리합니다. 한 엔드포인트만 진단하려면 `--endpoints 0`, `--endpoints 1` 또는 `--endpoints 2`를 쓰고, 임의 부분집합은 쉼표로 묶습니다(예: `--endpoints 0,2`).

출력은 기본이 독점입니다. DAC를 다른 프로그램이 독점으로 쥐고 있거나 DAC가 독점을 거절하면 `exclusive output is unavailable: ...`로 끝나고, 공유 모드로 몰래 내려가지 않습니다. 그 프로그램을 끄고 다시 하거나, 공유로 들으려면 `--shared`를 붙입니다. `--allow-shared-fallback`을 붙이면 독점이 안 될 때만 공유로 내려가고, 보고의 `output_fell_back`이 `true`가 됩니다.

보고의 `clock_strategy`는 명령행 요청을 되풀이한 값이 아니라 런타임이 시작할 때 실제로 고른 `trim` 또는 `asrc`입니다. `output_mode`(`mode`도 같음)도 실제로 연 모드입니다. `output_cadence_ratio`는 DAC가 실제로 재생한 속도를 명목 속도로 나눈 값이고 1에 가까워야 합니다. `endpoints`에는 네 슬롯의 `endpoint`, `format`, `epoch`, `unsupported`가 들어갑니다. 쓰지 않은 슬롯의 `format`이 `null`인 것은 정상이나 `unsupported`는 모든 슬롯에서 `null`이어야 합니다.

**합격 기준**
- `underruns_while_streaming`이 0
- `dropped_frames`, `lane_underrun_frames`, `output_overflow_frames`, `dropped_events`가 모두 0
- `endpoints`가 네 슬롯이고 `endpoints[*].unsupported`가 모두 `null`
- `driver.endpoints[*].lost_bytes`가 모두 0
- `output_mode`가 요청한 모드(기본 `exclusive`)이고 `output_fell_back`이 `false`
- `output_reconnects`, `output_cadence_faults`가 0이고, `output_cadence_ratio`가 0.98~1.02
- `clock_strategy`가 실제 시작 전략과 맞음(`--clock auto`에서는 48 kHz 출력과 트림 지원이 모두 있으면 `trim`, 아니면 `asrc`; 명시한 `--clock asrc`/`trim`은 그 전략)
- 세 소리가 함께 들림
- 한쪽을 멈추고 다시 틀어도 다른 쪽이 끊기지 않음

**(가능하면) 출력 장치를 뽑았다 꽂기.** USB DAC라면 위 명령으로 재생하는 동안 DAC를 뽑고, 10초 뒤 다시 꽂습니다. 끝낸 뒤 보고를 `driver-replug.json`으로 남깁니다(`--report driver-replug.json`).

**합격 기준**
- 뽑은 동안 `ssr-live`가 끝나지 않고 `fault: Output...`을 한 번 알림
- 다시 꽂으면 몇 초 안에 `output reconnected`가 나오고 소리가 돌아옴
- 끝난 보고의 `output_reconnects`가 1 이상. `faults`에는 지나간 장애도 남으므로, 뽑았을 때의 출력 장애(`Output`)만 있으면 정상
- 뽑을 때와 돌아올 때 큰 딸깍 소리가 없음(작은 끊김은 적어 둠)

**채널 마스크가 0인 PCM.** 앱이 채널 마스크 없이(직접 출력) PCM을 보내면, 엔진은 채널 수마다 정한 순서로 각 채널의 자리를 정합니다.

| 채널 수 | 기본 순서 | 근거 |
|---:|---|---|
| 1~6, 8 | Windows가 마스크 0에 가정하는 순서(예: 8은 L, R, C, LFE, Lrs, Rrs, Lss, Rss) | Windows 기본 마스크 |
| 10 | 7.1.2: 7.1 뒤에 Tfl, Tfr | ffmpeg |
| 12 | 7.1.4: 7.1.2 뒤에 Trl, Trr | ffmpeg |
| 14 | 9.1.4: L, R, C, LFE, Lrs, Rrs, **Lc, Rc**, Lss, Rss, Tfl, Tfr, Trl, Trr | ffmpeg |
| 16 | 9.1.6: 9.1.4 뒤에 Tsl, Tsr | ffmpeg |

- ffmpeg의 9.1.4와 9.1.6에서 7·8번 채널은 와이드(±60°)가 아니라 정면 가까이(±15°, FLC/FRC)입니다. 와이드를 쓰는 소스이거나 순서가 다른 소스라면 `play`에 `--direct-layout <채널 수>=<프리셋 또는 이름 목록>`을 붙입니다. 여러 채널 수를 정하려면 여러 번 붙입니다.

  ```powershell
  .\ssr-live.exe play --source driver --output "<DAC 이름 일부>" --profile profile.toml --direct-layout 16=FL,FR,FC,LFE,BL,BR,WL,WR,SL,SR,TFL,TFR,TBL,TBR,TSL,TSR
  ```

- 프리셋: `mono`, `stereo`, `3.0`, `quad`, `5.0`, `5.1`, `7.1`, `5.1.2`, `5.1.4`, `7.1.2`, `7.1.4`, `9.1.4`, `9.1.6`
- 이름(대소문자 무시): `L R C LFE Ls Rs Lrs Rrs Lss Rss Lw Rw Lc Rc Cs Tfl Tfr Tfc Tsl Tsr Trl Trr Trc Tc`(`LFE2`도 받음), 그리고 Impulcifer와 ffmpeg가 쓰는 `FL FR FC BL BR BC SL SR FLC FRC WL WR TFL TFR TFC TSL TSR TBL TBR TBC TC`
- `5.0`, `5.1`, `5.1.2`, `5.1.4` 프리셋의 서라운드는 Windows 5.1처럼 뒤쪽 쌍(Lrs, Rrs)입니다. 옆 채널(SL, SR)을 쓰는 소스(ffmpeg의 5.1, 5.1.4 등)라면 이름 목록으로 적습니다.
- 모르는 이름, 같은 자리 두 번, 채널 수와 이름 수가 다른 값은 재생을 시작하기 전에 거절합니다. `--report`의 `direct_layouts`에는 명령행으로 정한 배치만 남습니다.
- LFE(`LFE` 또는 `LFE2`)는 한 배치에 하나만 둡니다. 둘을 넣으면 재생을 시작하기 전에 거절합니다.
- 기본 순서가 없는 채널 수(7, 9, 11, 13, 15, 17 이상)는 배치를 정하지 않으면 재생하지 않습니다. 이때 보고의 `endpoints[*].unsupported`에 `no direct layout`이 남습니다. 자리 이름이 24개뿐이라(LFE 하나 포함) 직접 출력은 24채널까지 배치할 수 있습니다.

### 6-2. 스피커로 듣기 (선택, 다채널 장비가 있을 때)

AV 리시버(HDMI)나 다채널 DAC·오디오 인터페이스가 있으면, 헤드폰 대신 실제 스피커로 내보내는 출력(ADR 0030)을 시험합니다. 없으면 건너뜁니다.

1. 장치가 받는 레이아웃을 봅니다:

   ```powershell
   .\ssr-live.exe devices --formats
   ```

   쓸 장치의 공유 믹스 형식(채널 수와 마스크)과 독점으로 받는 표준 레이아웃(2.0, 4.0, 5.1, 7.1, 7.1.4)을 적습니다.
2. 도구 zip의 `profiles\speakers-5.1.toml` 또는 `speakers-7.1.4.toml`을 복사해 자기 방에 맞춥니다.
   - `device_channels`와 `channel_mask`: 1에서 본, 장치가 받는 레이아웃
   - `[[speaker]]`마다 `channel`(장치 채널 번호), `distance_m`(듣는 자리까지 거리), `trim_db`, `range`("small"이면 크로스오버 아래를 서브우퍼로)
   - `[[subwoofer]]`의 `channel`과 `distance_m`. 서브우퍼가 없으면 이 절을 지웁니다.
   - 마스크가 있으면 이름과 채널 순서가 Windows 순서와 맞아야 합니다(예: 7.1이면 0 L, 1 R, 2 C, 3 LFE, 4 Lrs, 5 Rrs, 6 Lss, 7 Rss). 어긋나면 프로필을 읽을 때 무엇이 틀렸는지 알려 줍니다.
3. 재생합니다(드라이버를 깔았으면 `--source driver`, SAL PC면 `--source sal`):

   ```powershell
   .\ssr-live.exe play --source driver --output "<장치 이름 일부>" --profile my-speakers.toml --report speakers-listen.json
   ```

4. Windows 기본 장치인 **Spatial SoftReceiver**로 채널 확인 영상이나 5.1/7.1 시험 음원을 재생해, 소리가 각 스피커에서 나오는지 확인합니다. Atmos 음원이 있으면 위층 스피커(7.1.4)에서 높이가 들리는지도 적습니다.
5. Enter로 끝내고 `speakers-listen.json`을 결과 묶음 폴더에 둡니다.

**합격 기준**
- 보고의 `output_kind`가 `speakers`, `output_channels`와 `output_channel_mask`가 프로필과 같음
- `underruns_while_streaming`, `dropped_frames`, `output_overflow_frames`가 0
- 채널 확인 음원의 각 채널이 그 스피커에서만 나옴(뒤바뀐 채널이 있으면 그 이름을 적음)
- 크로스오버를 쓴 경우 저음이 서브우퍼로 가고, 스피커 사이 거리 정렬 뒤 소리가 한 점에 모임

## 7. Driver Verifier와 메모리 무결성

### 7-1. Driver Verifier

**먼저 읽으십시오.** 검사기가 결함을 찾으면 블루스크린이 납니다. 그것이 이 시험의 목적입니다. 부팅할 때마다 블루스크린이 나면 아래 방법으로 끕니다.
- 고급 시작 옵션에서 **안전 모드**로 부팅합니다.
- 관리자 명령 프롬프트에서 `verifier /reset`을 실행하고 재부팅합니다.

1. 켭니다(관리자):

   ```powershell
   verifier /standard /driver spatialsoftreceiver.sys
   ```

   재부팅합니다.
2. 확인합니다:

   ```powershell
   verifier /query
   ```

   `spatialsoftreceiver.sys`가 목록에 있어야 합니다.
3. 검사기가 켜진 상태로 2, 3, 6단계를 다시 합니다. 3단계는 가능한 만큼만 합니다.
4. 검사기가 켜진 상태로 제어 장치 IOCTL 퍼저를 5분 돌립니다. 다른 링 판독기(`ssr-live`, `ring_record.py`)는 끕니다:

   ```powershell
   py .\ioctl_fuzz.py --seconds 300 --summary ioctl-fuzz.json
   ```

   러너는 일시적 검사기(`/volatile`)로 60초만 돌리므로, 표준 검사기와 긴 시간은 이 PC에서만 됩니다.

   링에 데이터가 오지 않으므로 퍼저가 보낸 READ는 대개 걸려 있습니다. 퍼저는 0.1초 뒤 그 READ를 취소하고, 드라이버는 취소를 5초 안에 끝내야 합니다. 퍼저가 지정한 시간(여기서는 5분)보다 1분 넘게 끝나지 않으면 불합격입니다. 창을 닫지 말고 결과 표에 적은 뒤 8단계를 실행하십시오.
5. 끕니다:

   ```powershell
   verifier /reset
   ```

   재부팅합니다.

**합격 기준**
- 블루스크린 없음
- 2단계 합격
- 퍼저가 지정한 시간에서 1분 안에 끝남
- 퍼저 종료 코드 0: `ioctl-fuzz.json`의 `checks`가 모두 `true`(취소한 READ를 드라이버가 끝냈는지 보는 `pending_requests_cancel` 포함)이고 `unexpected_successes`가 빈 목록. 화면 첫 줄의 `IOCTL fuzz seed`를 결과 표에 적습니다(같은 씨앗으로 다시 돌릴 수 있습니다)

블루스크린이 났다면 재부팅 뒤 바로 8단계를 실행하십시오. 덤프(`C:\Windows\Minidump`)가 함께 모입니다.

### 7-2. 메모리 무결성(HVCI)

1. Windows 보안 → 장치 보안 → 코어 격리 → **메모리 무결성**을 켜고 재부팅합니다. 이미 켜져 있으면 그대로 둡니다.
2. 장치 관리자나 다음 명령으로 장치 상태와 문제 코드를 봅니다:

   ```powershell
   Get-PnpDevice -PresentOnly | Where-Object { $_.HardwareID -contains 'Root\SpatialSoftReceiver' } | Format-List Status, Problem
   ```

3. 2단계를 돌립니다.
4. 원래 꺼져 있었다면 다시 끄고 재부팅합니다.

**합격 기준**
- 드라이버가 올라옴(문제 코드 없음)
- 2단계 합격

올라오지 않으면 8단계의 수집에 Code Integrity 로그가 들어가니 그것을 보내 주십시오.

## 8. 결과 모으기와 보내기

관리자 PowerShell, 묶음 폴더에서:

```powershell
.\collect.ps1
```

M2의 결과(`play-*.json`, `sal-*.ssrdump`, `builder-*.wav`)는 묶음 폴더로 옮긴 뒤 실행하면 함께 모입니다. M2만 하고 우리 드라이버를 깔지 않았어도 이 스크립트는 돕니다. 묶음 폴더에 `spatial-softreceiver-test-report-<무작위>-<날짜시각>.zip`이 생깁니다. 무엇이 들어갔는지는 화면과 zip 안 목록에 나옵니다.
- OS 빌드, 부팅 설정, Secure Boot와 HVCI 상태
- 장치와 서비스 상태, 드라이버 해시, 레지스트리 매개변수
- 엔드포인트 상태와 읽기 전용 `endpoint-policy.json`(독점 허용·우선권·오류)
- `setupapi.dev.log`의 우리 장치 부분
- 관련 이벤트 로그와 Code Integrity 로그
- `verifier /query`
- 미니덤프
- 이 폴더의 JSON과 덤프

보내는 방법:
1. **공개 이슈**에는 아래 표만 채워 올립니다. 이슈 제목은 `[시험] v<버전> <날짜>`로 합니다.
2. **zip과 `.ssrdump`**는 저장소 소유자에게 따로 보냅니다. 덤프에는 재생한 소리가 들어 있습니다.

| 단계 | 결과(합격/불합격/못 함) | 메모 |
| --- | --- | --- |
| M2-1 SAL 링 녹화(덤프별) | | 형식, 유실 |
| M2-2 듣기(게임/음악/공유) | | underruns, restarts, 들은 느낌 |
| M2-3 지연 비교 | | p50/p99 ms, 귀 비교 |
| 1 설치 | | 종료 코드 |
| 2 기본 흐름 | | |
| 2-0 독점 정책 | | 속성 값, 지원 형식, Initialize HRESULT |
| 3 공간 음향 목록(`Spatial SoftReceiver`/`Spatial SoftReceiver Music`) | | 보인 항목 |
| 3 Atmos 협상과 형식 | | 형식 이름 |
| 4 비트스트림(코덱별) | | |
| 4-1 PCM 형식 범위(1, 2, 그리고 한 경우 3과 4) | | `devices` 줄, 7.1.4 독점, 고른 형식과 요약의 `formats[]` |
| 5 쓰기 패킷 | | 중앙값/p99 ms |
| 6 수명 경계 | | |
| 6-1 드라이버 링 종단 듣기 | | underruns, drops, lost bytes, 세 소리 |
| 6-2 스피커로 듣기(선택) | | 장치, 레이아웃, 채널 확인, 저음과 정렬 |
| 7-1 Driver Verifier | | |
| 7-1 IOCTL 퍼저(검사기 켬, 5분) | | |
| 7-2 HVCI | | |

## 9. 지우기

관리자 PowerShell, 묶음 폴더에서:

```powershell
.\uninstall.ps1
```

장치 제거 전에 Spatial SoftReceiver 엔드포인트의 두 독점 속성을 지우고 읽어 확인한 다음, 장치·드라이버 저장소 항목·시험 인증서를 지우고 남은 것이 없는지 확인합니다(종료 코드 0). 정책 삭제 실패나 남은 엔드포인트는 종료 코드 1로 보고합니다. 다른 장치의 속성은 바꾸지 않습니다.

시험을 모두 마쳤으면 다음으로 되돌립니다.

```powershell
bcdedit /set testsigning off
```

재부팅한 뒤 UEFI에서 Secure Boot를 다시 켭니다.

## 이번 라운드에 없는 것

- 드라이버 링 종단 듣기는 6-1단계에 들어갔습니다. 다음 릴리스에서 더할 일:
  - 지연 숫자
  - 트림을 오디오 엔진이 받아들이는 범위(±2000 ppm)
  - 안티치트가 있는 게임과의 공존
  - 몇 시간 연속 재생
- 스피커 출력의 자동 측정(마이크로 거리·레벨·룸 보정을 재는 일)과 스테레오 스피커의 크로스토크 제거(XTC)는 아직 없습니다. 거리, 레벨, 룸 보정 FIR은 손으로 적습니다.
