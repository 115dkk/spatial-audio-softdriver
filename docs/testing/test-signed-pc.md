# 시험서명 PC에서 BIO Atmos 드라이버 시험하기

이 문서는 릴리스의 `bio-atmos-driver-test-<버전>-x64.zip`에 `README.md`로 들어갑니다. 시험서명 모드를 켠 Windows PC에서, GitHub 호스트 러너로는 할 수 없는 드라이버 시험을 하는 방법을 적습니다. 시험하는 분이 AI 도우미와 함께 따라 할 수 있도록 명령, 합격 기준, 남길 파일을 단계마다 적습니다.

러너가 이미 확인하는 것:
- 설치와 적재
- 두 렌더 엔드포인트가 활성이 되는지
- 엔드포인트마다 톤을 재생해 링으로 읽어 오는지

이 문서는 그 밖의 것을 다룹니다: Driver Verifier, 메모리 무결성(HVCI), 공간 음향과 Dolby Atmos 협상, 비트스트림 형식, 쓰기 패킷 주기, 수명 경계. 근거는 [ADR 0027](../adr/0027-driver-layout.md)과 [`driver/README.md`](../../driver/README.md)의 "시험서명 PC" 항목입니다.

> **이번 라운드는 드라이버만 봅니다.** 우리 드라이버의 소리를 헤드폰으로 끝까지 듣는 시험, 지연 숫자, 트림, 장시간 게임 시험은 엔진이 이 드라이버를 읽게 된 뒤(`sys.driver_ring`)의 릴리스에서 이 문서에 더합니다.

## AI 도우미에게

- 이 문서에 적힌 명령만 실행하십시오. 레지스트리와 부팅 설정은 이 문서가 적은 값만 바꾸십시오.
- 단계마다 출력과 JSON 파일을 지우거나 고치지 말고 그대로 두십시오. 합격 여부는 각 단계의 "합격 기준"으로만 판단하고, 기준에 없는 일이 생기면 있는 그대로 적으십시오.
- 블루스크린(버그체크)이 나면 재부팅한 뒤 다른 일보다 먼저 [8단계의 수집](#8-결과-모으기와-보내기)을 실행하십시오.
- 결과를 공개된 곳(GitHub 이슈 등)에 올릴 때는 숫자 요약만 올리십시오. `.biodump`에는 그때 재생한 소리가 그대로 들어 있으므로 저장소 소유자에게 따로 보냅니다.

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

1. 릴리스 페이지에서 `bio-atmos-driver-test-<버전>-x64.zip`과 `SHA256SUMS.txt`를 받습니다.
2. 해시를 확인합니다:

   ```powershell
   Get-FileHash .\bio-atmos-driver-test-*-x64.zip -Algorithm SHA256
   ```

   `SHA256SUMS.txt`의 값과 같아야 합니다.
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

묶음의 드라이버는 릴리스를 만들 때 한 번 쓰고 버린 인증서(`CN=BIO Atmos test signing <버전>`)로 서명했습니다. 그 인증서의 개인 키는 빌드 러너 밖으로 나가지 않았고 지워졌습니다. 그래서 이 인증서를 신뢰해도, 이 인증서로 다른 무엇이 서명될 수는 없습니다.

`install.ps1`은 이 인증서를 이 PC의 "신뢰할 수 있는 루트 인증 기관"과 "신뢰할 수 있는 게시자"에 넣습니다. `uninstall.ps1`이 다시 지웁니다.

## 1. 설치

관리자 PowerShell, 묶음 폴더에서:

```powershell
.\install.ps1
```

| 종료 코드 | 뜻 |
| --- | --- |
| 0 | 드라이버가 올라왔고 렌더 엔드포인트 둘이 활성 |
| 2 | 사전 조건이 맞지 않음(관리자, OS 빌드, 시험서명 모드, 이미 설치됨). 출력된 안내를 따르십시오 |
| 3 | 설치는 됐지만 커널이 이미지를 거절(문제 코드 52). 시험서명 모드가 실제로 켜졌는지 다시 확인하십시오 |
| 4 | 올라왔지만 활성 엔드포인트가 둘이 아님 |
| 1 | 그 밖의 실패 |

**합격 기준**
- 종료 코드 0
- 설정 → 시스템 → 소리 → 출력 목록에 `BIO Atmos Virtual HDMI`와 `BIO Atmos Virtual HDMI 2`가 보임
- `install-result.json`이 묶음 폴더에 생김

엔드포인트 이름의 "2"는 시험판 이름입니다. 릴리스 전에 용도를 드러내는 이름으로 바뀝니다.

## 2. 기본 흐름

두 엔드포인트에 서로 다른 톤을 동시에 재생하면서, 제어 장치 `\\.\BioAtmos`에서 기록을 읽어 맞춥니다. 러너에서 도는 것과 같은 시험입니다.

```powershell
py .\driver_flow_check.py --summary driver-flow-basic.json
```

**합격 기준**
- 종료 코드 0
- `driver-flow-basic.json`에서 엔드포인트마다:
  - `gaps`가 0
  - `overruns`가 0
  - `tone_db`가 −6 dB 안팎
  - `other_tone_db`가 −60 dB 이하
  - `rate_error_ppm`의 절댓값이 100 이하

이 시험은 두 엔드포인트의 **공간 오디오가 꺼진 상태**에서 돌립니다. 켜져 있으면 공간 음향 엔진이 형식을 바꿔(예: MAT) 스테레오 톤 검사가 맞지 않습니다. 3단계 뒤에 이 시험을 다시 돌릴 때는 먼저 공간 오디오를 끕니다.

제어 장치는 핸들을 하나만 받습니다. 이 시험과 다음 단계의 녹화(`ring_record.py`)는 동시에 돌릴 수 없습니다.

## 3. 공간 음향과 Dolby Atmos 협상

우리 드라이버가 형식 목록만으로 Windows에 "Dolby Atmos for home theater"를 열어 주는지가 이 단계의 핵심입니다. 아키텍처 문서의 "확인하지 못한 것" 첫 항목입니다.

엔드포인트 둘에 대해 각각:

1. 설정 → 시스템 → 소리 → 해당 출력 → **공간 오디오**의 목록에 무엇이 있는지 적고 화면을 캡처합니다. 예: 끔, Windows Sonic for Headphones, Dolby Atmos for Headphones, Dolby Atmos for home theater.
2. Dolby Access가 있으면 앱의 "홈 시어터" 설정에서 이 출력을 고를 수 있는지 적습니다.
3. "Dolby Atmos for home theater"를 고를 수 있으면 켭니다. 그다음 녹화를 시작하고, 녹화하는 동안 Dolby Access의 데모(또는 Atmos 게임)를 재생합니다:

   ```powershell
   py .\ring_record.py --seconds 30 --out atmos-ep0.biodump
   ```

   두 번째 엔드포인트는 `--out atmos-ep1.biodump`로 따로 녹화합니다.
4. `atmos-ep0-summary.json`(화면에도 출력됨)에서 그 엔드포인트의 형식을 봅니다.

**합격 기준**
- "Dolby Atmos for home theater"가 목록에 있고 켤 수 있음
- 켠 뒤 재생 중의 형식이 `MAT 2.0` 또는 `MAT 2.1`(IEC 61937, 8채널 192 kHz 컨테이너)
- 불연속(`discontinuities`)과 넘침(`overruns`)이 0

목록에 없으면 그 자체가 중요한 결과입니다. 스크린샷과 `collect.ps1` 결과를 보내 주십시오.

## 4. 비트스트림 패스스루 형식

재생기가 압축 오디오를 그대로(독점 모드로) 보낼 때 드라이버가 그 형식을 받아들이는지 봅니다. MPC-HC와 LAV Filters를 예로 듭니다.

1. LAV Audio Decoder 설정에서 비트스트리밍(AC-3, E-AC-3, TrueHD, DTS, DTS-HD)을 켭니다.
2. 오디오 렌더러를 MPC Audio Renderer로 하고, 독점 모드로 BIO Atmos 출력을 고릅니다.
3. 형식마다 시험 영상을 재생하면서 녹화합니다:

   ```powershell
   py .\ring_record.py --seconds 20 --out passthrough-eac3.biodump
   ```

   AC-3, E-AC-3, TrueHD(MAT), DTS, DTS-HD MA, DTS:X 가운데 가진 파일로 합니다.

**합격 기준**
- 요약의 형식 이름이 재생한 코덱과 맞음
  - E-AC-3 → `E-AC-3`
  - TrueHD → `MAT`
  - DTS-HD MA → `DTS-HD`
- 불연속과 넘침이 0

재생기가 그 형식을 열지 못하고 PCM으로 떨어지면, 그 형식과 재생기 쪽 메시지를 적어 주십시오.

## 5. 쓰기 패킷 주기 (ADR 0014)

공간 음향 엔진이 드라이버의 쓰기 패킷 알림을 어떤 주기로 부르는지 잽니다. 기본은 꺼짐이고, 레지스트리로 켭니다.

1. 켜고 장치를 다시 시작합니다(관리자):

   ```powershell
   Set-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Services\bioatmos\Parameters' -Name WritePacket -Value 1 -Type DWord
   $id = (Get-PnpDevice -PresentOnly | Where-Object { $_.HardwareID -contains 'Root\BioAtmos' }).InstanceId
   pnputil /restart-device "$id"
   ```

2. 3단계처럼 "Dolby Atmos for home theater"를 켜고 재생하면서 녹화합니다:

   ```powershell
   py .\ring_record.py --seconds 30 --out writepacket-atmos.biodump
   ```

   PCM 스테레오 재생(일반 음악)으로도 한 번 더 합니다: `--out writepacket-pcm.biodump`.
3. 요약의 `qpc_written` 간격(최소, 중앙값, p99, 최대, ms)을 적습니다.
4. 되돌립니다:

   ```powershell
   Set-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Services\bioatmos\Parameters' -Name WritePacket -Value 0 -Type DWord
   pnputil /restart-device "$id"
   ```

**합격 기준**
- 이 단계는 측정입니다. 숫자를 그대로 보내 주십시오.
- `qpc_written`이 모두 0이면 엔진이 쓰기 패킷 경로를 쓰지 않은 것입니다. 그것도 결과입니다.

## 6. 수명 경계

각 항목 뒤에 2단계의 기본 흐름을 다시 돌려 소리가 다시 지나는지 확인합니다.

| 항목 | 하는 법 |
| --- | --- |
| 재생 중 장치 끄고 켜기 | 2단계를 돌리는 동안 다른 관리자 창에서 `Disable-PnpDevice -InstanceId $id -Confirm:$false`, 5초 뒤 `Enable-PnpDevice -InstanceId $id -Confirm:$false` |
| 장치 다시 시작 | `pnputil /restart-device "$id"` |
| 절전과 깨우기 | 음악을 BIO 출력으로 재생하는 동안 절전(`rundll32 powrprof.dll,SetSuspendState 0,1,0` 또는 시작 메뉴), 깨운 뒤 2단계 |
| 최대 절전 | 지원하면 같은 방법으로 |
| 재부팅 | 재부팅 뒤 장치 상태와 2단계 |
| 두 엔드포인트의 다른 형식 | 한쪽은 공간 음향(Atmos), 다른 쪽은 PCM 음악을 동시에 재생하며 `ring_record.py --seconds 30` |

**합격 기준**
- 블루스크린 없음
- 장치가 문제 코드 없이 돌아옴
- 각 항목 뒤 2단계 합격

## 7. Driver Verifier와 메모리 무결성

### 7-1. Driver Verifier

**먼저 읽으십시오.** 검사기가 결함을 찾으면 블루스크린이 납니다. 그것이 이 시험의 목적입니다. 부팅할 때마다 블루스크린이 나면 아래 방법으로 끕니다.
- 고급 시작 옵션에서 **안전 모드**로 부팅합니다.
- 관리자 명령 프롬프트에서 `verifier /reset`을 실행하고 재부팅합니다.

1. 켭니다(관리자):

   ```powershell
   verifier /standard /driver bioatmos.sys
   ```

   재부팅합니다.
2. 확인합니다:

   ```powershell
   verifier /query
   ```

   `bioatmos.sys`가 목록에 있어야 합니다.
3. 검사기가 켜진 상태로 2, 3, 6단계를 다시 합니다. 3단계는 가능한 만큼만 합니다.
4. 끕니다:

   ```powershell
   verifier /reset
   ```

   재부팅합니다.

**합격 기준**
- 블루스크린 없음
- 2단계 합격

블루스크린이 났다면 재부팅 뒤 바로 8단계를 실행하십시오. 덤프(`C:\Windows\Minidump`)가 함께 모입니다.

### 7-2. 메모리 무결성(HVCI)

1. Windows 보안 → 장치 보안 → 코어 격리 → **메모리 무결성**을 켜고 재부팅합니다. 이미 켜져 있으면 그대로 둡니다.
2. 장치 관리자나 다음 명령으로 장치 상태와 문제 코드를 봅니다:

   ```powershell
   Get-PnpDevice -PresentOnly | Where-Object { $_.HardwareID -contains 'Root\BioAtmos' } | Format-List Status, Problem
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

묶음 폴더에 `bio-atmos-test-report-<무작위>-<날짜시각>.zip`이 생깁니다. 무엇이 들어갔는지는 화면과 zip 안 목록에 나옵니다.
- OS 빌드, 부팅 설정, Secure Boot와 HVCI 상태
- 장치와 서비스 상태, 드라이버 해시, 레지스트리 매개변수
- 엔드포인트 상태
- `setupapi.dev.log`의 우리 장치 부분
- 관련 이벤트 로그와 Code Integrity 로그
- `verifier /query`
- 미니덤프
- 이 폴더의 JSON과 덤프

보내는 방법:
1. **공개 이슈**에는 아래 표만 채워 올립니다. 이슈 제목은 `[시험] v<버전> <날짜>`로 합니다.
2. **zip과 `.biodump`**는 저장소 소유자에게 따로 보냅니다. 덤프에는 재생한 소리가 들어 있습니다.

| 단계 | 결과(합격/불합격/못 함) | 메모 |
| --- | --- | --- |
| 1 설치 | | 종료 코드 |
| 2 기본 흐름 | | |
| 3 공간 음향 목록(엔드포인트 0/1) | | 보인 항목 |
| 3 Atmos 협상과 형식 | | 형식 이름 |
| 4 비트스트림(코덱별) | | |
| 5 쓰기 패킷 | | 중앙값/p99 ms |
| 6 수명 경계 | | |
| 7-1 Driver Verifier | | |
| 7-2 HVCI | | |

## 9. 지우기

관리자 PowerShell, 묶음 폴더에서:

```powershell
.\uninstall.ps1
```

장치, 드라이버 저장소 항목, 시험 인증서를 지우고 남은 것이 없는지 확인합니다(종료 코드 0).

시험을 모두 마쳤으면 다음으로 되돌립니다.

```powershell
bcdedit /set testsigning off
```

재부팅한 뒤 UEFI에서 Secure Boot를 다시 켭니다.

## 이번 라운드에 없는 것

- 우리 드라이버를 읽는 엔진(`sys.driver_ring`)과 두 엔드포인트 믹서가 들어간 뒤의 일은 다음 릴리스에서 이 문서에 단계로 더합니다.
  - 헤드폰으로 끝까지 듣기
  - 지연 숫자
  - 트림을 오디오 엔진이 받아들이는 범위(±2000 ppm)
  - 안티치트가 있는 게임과의 공존
  - 몇 시간 연속 재생
- 제어 장치 IOCTL 퍼징은 도구가 준비되면 더합니다.
