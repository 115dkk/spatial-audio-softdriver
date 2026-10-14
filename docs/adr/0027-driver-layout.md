# 0027. 드라이버는 SysVAD의 HDMI 렌더 둘과 제어 장치 하나로 만들고, 커널 밖에서 시험할 코어를 떼어 낸다

상태: 채택, 2026-10-12. ADR 0004, 0011, 0014, 0024를 `spatialsoftreceiver.sys`의 구조로 옮깁니다.

## 배경

M3는 우리 드라이버를 만들고 ABI v1을 동결합니다. 동결한 뒤에는 EV의 1년(ADR 0004)이 지나면 고칠 수 없으므로, 서명 전에 결함을 최대한 많이 잡아야 합니다. 그런데 개발은 Linux 컨테이너와 GitHub 호스트 러너에서 하고, 호스트 러너는 재부팅할 수 없어 시험서명 모드를 켤 수 없습니다. 커널에 올리는 검사(Driver Verifier, IOCTL 퍼저, HVCI 로드, 형식 협상)는 시험서명 PC에서만 됩니다. 그래서 커널에 올리지 않고도 검증되는 부분을 가능한 크게 떼어 내는 구조로 정합니다.

## 결정

### 출발점과 범위

- 위치는 `driver/`, 언어는 C++17(예외와 RTTI 없음), 라이선스는 MS-PL입니다(ADR 0003).
- Microsoft Windows-driver-samples의 SysVAD에서 TabletAudioSample의 HDMI 렌더 경로만 가져옵니다(adapter, common, basetopo, kshelper, minwavert, minwavertstream, hdmitopo, hdmitoptable, hdmiwavtable, NewDelete). 마이크, 블루투스, USB, S/PDIF, 키워드 감지, APO, 오프로드, 오디오 엔진 노드, 저장 파일(savedata)은 들이지 않습니다. 가져온 파일의 머리에는 원본 경로와 커밋을 적습니다.
- SpatialAudioLab의 드라이버와 그 패치는 읽지 않습니다(ADR 0005). SysVAD와 Microsoft 문서만 봅니다.
- WDF(KMDF)는 쓰지 않습니다. SysVAD는 블루투스와 USB 사이드밴드의 작업 항목에만 WDF를 쓰므로, 그 장치를 빼면 순수 WDM과 PortCls로 충분합니다.

### 장치

- 루트 열거 장치 하나(`ROOT\SpatialSoftReceiver`)가 렌더 엔드포인트 셋을 냅니다(ADR 0028): `Spatial SoftReceiver`, `Spatial SoftReceiver Music`, `Spatial SoftReceiver PASS`(한국어 Windows에서는 `공간 소프트리시버`, `공간 소프트리시버 음악`, `공간 소프트리시버 PASS`). 엔드포인트마다 WaveRT 미니포트와 토폴로지 미니포트가 짝을 이루고, 잭은 HDMI입니다. 어느 엔드포인트를 낼지는 서비스 `Parameters` 키의 `EndpointMask` DWORD(기본 `0x7`)로 정하고, 장치를 다시 시작하면 반영합니다(ADR 0024).
- 형식 표는 SysVAD HDMI 표의 상위집합(PCM 2.0·5.1·7.1·7.1.4 × 44.1·48·88.2·96 kHz × 16비트·24-in-32·float32, AC-3, E-AC-3, E-AC-3 JOC, TrueHD, MAT 2.0, MAT 2.1, DTS, DTS:X)으로 컴파일하고, 엔드포인트마다 `FormatMask0`~`FormatMask3` QWORD로 켭니다. 비트는 `ssr-abi`의 `FORMAT_*`와 `format_pcm(layout, rate)`입니다(ADR 0024의 2026-10-13 고침). DTS:X E1과 E2(하위 형식 0x010b, 0x030b)는 DTS-HD 컨테이너이므로 `FORMAT_DTS_HD`로 묶고, 어느 쪽인지는 디코더가 스트림에서 읽습니다. 기본값(`DEFAULT_ENABLED_FORMATS`)은 PCM 2.0·5.1·7.1의 네 표본율, E-AC-3, E-AC-3 JOC, MAT 2.0, MAT 2.1입니다. 7.1.4는 공유 믹스가 12채널이면 장치를 직접 여는 게임 일부가 무음이었으므로 끄고, AC-3, TrueHD, DTS는 디코더가 생길 때 켭니다. 마스크는 `ssr-setup`만 씁니다(ADR 0011). (아래 2026-10-13 고침이 PCM 표와 `FormatMask{i}`를 코덱 마스크와 PCM 규칙으로 바꿨습니다.)

### 커널 밖에서 시험하는 코어

- 링, 기록 쓰기, 넘침 처리와 엔드포인트별 유실 계산, READ 완료 조건(최소 바이트, 형식 변경, 정지, 시간 초과), 트림 적용(±2000 ppm 자르기, 위치 계산), 형식 마스크 판정(아래 2026-10-13 고침이 PCM 규칙 판정 `PcmAllowed`로 바꿨습니다)은 `driver/core/`에 둡니다. 코어는 커널 API를 부르지 않고 할당하지 않으며, 바깥에서 준 메모리와 시각만 씁니다.
- 커널 쪽은 잠금(스핀 락), 타이머, IRP와 취소 안전 큐, MDL, 레지스트리만 감쌉니다.
- 코어는 같은 소스를 사용자 모드 시험 프로그램으로도 빌드합니다. Windows에서는 MSVC로, Linux에서는 clang과 gcc로 빌드하고 AddressSanitizer와 UBSan을 켜며, libFuzzer로 링과 READ 조건을 퍼징합니다(아래 2026-10-13 고침이 형식 판정기를 더했습니다).
- 코어 시험이 쓴 기록 스트림을 `ssr-abi`의 파서로 읽어 같은 값인지 확인합니다. 드라이버와 시험은 `ssr-abi`가 생성한 `ssr_abi.h` 하나만 include하므로 배치가 어긋나면 컴파일에서 멈춥니다.

### 제어 장치

- 제어 장치 `\\.\SpatialSoftReceiver`(`ssr_abi::CONTROL_DEVICE_PATH`)는 `IoCreateDeviceSecure`로 만들고 SDDL은 SYSTEM, 관리자, 대화형 사용자에게 읽기와 쓰기를 줍니다(ADR 0011). PortCls 초기화 뒤 원래 디스패치 표를 보존하고 모든 IRP를 장치별로 나눕니다. 제어 장치는 CREATE, CLOSE, CLEANUP, DEVICE_CONTROL만 처리하고 나머지는 거절하며, PortCls 장치는 보존한 처리기로 넘깁니다. 이름이 붙은 제어 장치 하위 경로 열기도 거절합니다.
- 핸들은 하나만 엽니다. 두 번째 열기는 `STATUS_SHARING_VIOLATION`입니다. CLEANUP에서 보류 중인 READ를 취소하고 트림을 0으로 돌립니다.
- READ는 `METHOD_OUT_DIRECT`이고 보류는 하나만 받습니다. 두 번째 보류는 `STATUS_DEVICE_BUSY`입니다. 완료 판단은 기록을 쓰는 타이머 DPC에서 하고, 시간 초과는 READ의 기한에 한 번 울리는 타이머가 셉니다.
- 링에서 출력 버퍼로 옮기는 복사는 스핀 락 아래에서 하므로 한 번에 `READ_MAX_BYTES`(256 KiB, MAT 2.1로 약 85 ms)까지만 옮깁니다. 데이터 기록의 페이로드는 `MAX_DATA_PAYLOAD`(64 KiB)까지이고 더 긴 구간은 나눠 씁니다. 두 값은 `ssr-abi`의 계약 상수이고, `record_size(MAX_DATA_PAYLOAD)` 이상인 버퍼는 항상 진행합니다.

### 링과 시각

- 링은 비페이지 메모리 8 MiB(MAT 2.1의 3.07 MB/s로 약 2.7초)이고 `AbiInfo::ring_bytes`로 알립니다.
- 사용자 모드가 늦어 링이 차면 가장 오래된 기록부터 통째로 덮어쓰고, 덮어쓴 데이터 바이트를 엔드포인트별로 셉니다. 기록을 잃은 엔드포인트는 다음 기록 앞에 넘침 기록과 지금 상태의 재진술(형식, 볼륨, 시작 또는 정지)을 받습니다. 정지해서 다음 기록이 없는 엔드포인트도 다음 READ에서 받습니다(ADR 0024의 2026-10-13 고침).
- 데이터 기록의 `qpc`는 가상 DMA가 그 기록의 마지막 바이트를 지난 시각입니다. 두 엔드포인트는 같은 타이머를 쓰므로 같은 클럭 도메인입니다.
- DMA 타이머(1 ms)는 어댑터가 시작됐고 D0 전원 상태이며 RUN 스트림이 하나라도 있을 때만 돕니다. 재생하지 않거나 D0을 떠난 PC에서 1 ms마다 깨어나는 일은 없습니다.
- 트림은 위치 계산에 `1 + ppm / 1e6`을 곱해 넣습니다. 엔드포인트 둘에 똑같이 걸립니다. 클럭은 64비트 연산만 쓰는 이식 가능한 128비트 유리수 누산으로 분수와 블록 정렬 나머지를 호출과 트림 변경 사이에 보존합니다.

### 쓰기 패킷 알림

SysVAD에는 `SetWritePacket` 경로가 있습니다. 동결 뒤에는 더할 수 없으므로 구현해 넣되 `Parameters`의 `WritePacket` DWORD(기본 0)로 켭니다. 켜면 `qpc_written`을 채우고 `FEATURE_WRITE_PACKET`을 알립니다. 쓸지는 ADR 0014대로 시험서명 PC에서 공간음향 엔진의 호출 주기를 재고 정합니다.

### 엔드포인트 빌더와 오디오 서비스에 하는 답

호스트 러너에서 두 엔드포인트가 NOTPRESENT로 남거나 재생이 `AUDCLNT_E_UNSUPPORTED_FORMAT`로 멈춘 원인은 모두 KS 응답 하나였고, 드라이버의 KS 요청 추적(`driver/spatialsoftreceiver/kstrace.cpp`, `tools/ci/ks-trace.ps1`)으로 찾았습니다. 아래는 지킵니다.

- 호스트 핀의 `KSPROPERTY_PIN_PROPOSEDATAFORMAT` SET에 `WAVEFORMATEX` 없이 `KSDATAFORMAT`(64바이트)만 오면 "이 형식 종류를 받는가"로 읽어, 형식 마스크에 그 서브포맷이 있으면 성공, 없으면 `STATUS_NO_MATCH`로 답합니다(PCM과 IEEE_FLOAT는 아래 2026-10-13 고침대로 규칙으로 답합니다). 엔드포인트 빌더는 PCM, AC-3, WMA Pro를 이렇게 묻고, 셋 모두 거절당하면 엔드포인트를 NOTPRESENT로 둡니다.
- 호스트 핀은 신호 처리 모드 RAW 하나를 알리고(`IMiniportAudioSignalProcessing::GetModes`), 데이터 범위마다 `KSDATARANGE_ATTRIBUTES`와 `KSATTRIBUTEID_AUDIOSIGNALPROCESSING_MODE` 속성 목록을 답니다. 오디오 서비스는 모드 속성을 단 형식으로 핀을 열고, KS는 그런 형식을 속성을 나열한 범위에만 맞춥니다.
- 잭이 아닌 핀에 잭 속성을, 스트림을 받지 않는 핀에 형식 제안을 물으면 `STATUS_INVALID_DEVICE_REQUEST`(지원하지 않음)로 답합니다.
- HDMI 엔드포인트의 이름은 잭의 싱크 설명(`KSJACK_SINK_INFORMATION.SinkDescription`)에서 옵니다. 엔드포인트마다 다른 이름을 돌려주고, INF의 이름과 같게 둡니다.

### Linux에서 하는 검사

`tools/kcheck.py`는 Microsoft가 NuGet으로 내는 WDK와 Windows SDK 패키지(SHA-256으로 고정)의 헤더를 받아, clang의 MSVC 모드로 드라이버의 모든 소스를 x64와 ARM64로 컴파일 검사하고 clang 정적 분석기를 돌립니다. Windows의 파일 이름은 대소문자를 가리지 않으므로 헤더가 쓰는 철자마다 심볼릭 링크를 둡니다. 링크하지 않으므로 빌드가 아니라 형식과 문법 검사이고, 진짜 빌드는 Windows CI가 합니다. 드라이버를 고친 사람이 컨테이너 안에서 CI 전에 돌리는 것이 목적입니다.

### CI에서 하는 검사

호스트 러너(windows-2022, WDK 10.0.26100)에서 다음을 모두 돌립니다. 패키지 설치 시험은 Windows 11 24H2와 오디오 스택을 같이 쓰는 windows-2025에서도 돌립니다.

- x64와 ARM64, Debug와 Release 빌드. `/W4 /WX`와 코드 분석(`/analyze`, 드라이버 규칙)을 켭니다.
- `InfVerif /w`, 시험 인증서로 `Inf2Cat`과 `signtool` 서명 및 검증.
- CodeQL의 Windows 드라이버 질의 묶음(WHCP 필수 규칙).
- 패키지 설치 시험: 시험 인증서를 신뢰 저장소에 넣고 `pnputil /add-driver`로 드라이버 저장소에 넣은 뒤 루트 장치를 만듭니다. 처음에는 시험서명 모드가 아니므로 커널 로드가 문제 코드 52로 멈출 것이라 보고 그 앞까지(INF 섹션, 서비스, `Parameters` 기본값, 장치 노드)만 확인하려 했습니다. 첫 실행에서 Windows Server 2022 러너의 커널은 시험서명 이미지를 그대로 올렸습니다(문제 코드 0, 서비스 실행 중). 그래서 두 결과를 모두 받아들이고, 로드되면 두 엔드포인트에 서로 다른 톤을 동시에 재생하며 제어 장치에서 기록을 읽어 엔드포인트마다 형식 변경과 시작, 바이트 연속, DMA 속도, 내용을 확인합니다(`tools/ci/driver_flow_check.py`). 부팅 항목의 testsigning 상태를 로그에 남깁니다. 두 렌더 엔드포인트는 서비스를 다시 시작하지 않고 장치가 도착한 대로 활성이어야 합니다. 설치와 재생 동안 드라이버가 받은 KS 요청을 추적해 로그에 풀어 둡니다.
- 대조군: 같은 커밋의 SysVAD를 windows-2025에 시험서명 설치하고 같은 도구로 봅니다(`continue-on-error`). 우리 엔드포인트가 다시 활성이 되지 않으면 러너 이미지의 변화인지 우리 드라이버의 회귀인지를 가립니다.
- 코어 시험(MSVC, clang, gcc, ASan, UBSan), libFuzzer, `ssr-abi` 교차 검사. libFuzzer 대상에 형식 판정기(`format_fuzz`)가 더해졌습니다(아래 2026-10-13 고침).

## 결과

- 커널에 올려야만 알 수 있는 것 가운데 러너에서 되는 것(링을 지나는 소리, 두 엔드포인트의 동시 재생, DMA 속도)은 CI가 맡습니다. Dolby Atmos for home theater가 열리는지, 트림을 오디오 엔진이 받아들이는 범위, 쓰기 패킷 주기, HVCI 로드는 여전히 시험서명 PC에서 확인합니다. Driver Verifier와 IOCTL 퍼저도 러너에서 돕니다(아래 2026-10-13 고침).
- 코어와 커널 감싸개의 경계가 생기므로, 링과 READ의 결함은 사용자 모드에서 재현됩니다.
- 커널 코드는 SysVAD에서 쓰지 않는 장치를 뺀 만큼 줄어듭니다.

## 2026-10-13 고침

- **엔드포인트 셋.** 셋째 짝(패스스루, `Spatial SoftReceiver PASS`)을 더했습니다.
- **볼륨·음소거 노드.** 엔드포인트마다 토폴로지 경로에 볼륨(−96~0 dB, 0.5 dB 단위)과 음소거 노드를 둡니다. 처리기는 SysVAD(`audio/sysvad/kshelper.cpp`, `sysvad.h`, 커밋 `2dc3fd3a`)를 따릅니다. 값은 코어가 보관하고 기록으로 엔진에 알립니다(ADR 0024).
- **기본 공유 형식.** INF가 엔드포인트마다 OEM 기본 형식(`PKEY_AudioEngine_OEMFormat`, `VT_BLOB`)을 둡니다. 0은 float32 7.1 48 kHz, 1과 2는 float32 2.0 48 kHz입니다. Windows가 이 값을 실제 장치 형식으로 쓰는지는 시험서명 PC에서 확인합니다.
- **독점 정책.** 엔드포인트의 독점 허용과 우선권은 INF로 줄 수 있다는 문서를 찾지 못했습니다. 그래서 시험 묶음의 `install.ps1`이 엔드포인트가 생긴 뒤 `IPropertyStore`로 씁니다: 0과 1은 끔, 2는 켬과 우선권. 쓴 뒤에는 실제 독점 열기로 확인합니다. 제거할 때 지웁니다. 나중에는 `ssr-setup`이 같은 일을 합니다.
- **러너의 커널 검사.**
  - **Driver Verifier:** 드라이버를 설치하기 전에 휘발성 설정(`verifier /volatile`)으로 켭니다. 재부팅이 필요 없습니다. 위반이 있으면 버그체크로 잡이 실패합니다. 호스트 이미지가 휘발성 검증을 받아들이는지는 첫 실행이 보여 주므로, 그때까지는 거절을 경고로 둡니다.
  - **IOCTL 퍼저:** `tools/ci/ioctl_fuzz.py`가 그 아래에서 60초 동안 제어 장치에 다음을 넣습니다. 잘못된 길이, 극단값, 모르는 코드, 대기 중인 READ와 겨루는 취소·닫기, 두 번째 열기. 그 뒤에도 장치가 바르게 답해야 하고, 모양이 틀린 요청이 성공하면 실패입니다.
  - 시험서명 PC에서는 같은 퍼저를 재부팅한 표준 검증 아래에서 돌립니다.
- **이름과 식별자.** 제품 이름은 `Spatial SoftReceiver`(한국어 `공간 소프트리시버`)입니다. 'BIO Atmos'는 작업용 이름이었고, 남의 상표('Atmos')와 사람 이름('BIO')을 제품에서 뺐습니다. 서명 패키지에 묶이는 식별자는 다음과 같습니다.
  - 드라이버 `spatialsoftreceiver.sys`, 카탈로그 `.cat`, INF `.inf`
  - 서비스 `SpatialSoftReceiver`
  - 하드웨어 ID `ROOT\SpatialSoftReceiver`
  - 제어 장치 `\\.\SpatialSoftReceiver`

  제거 스크립트는 옛 시험판의 식별자도 지웁니다(`tools/ci/package-identities.ps1`).

  2026-10-14 고침: 저장소 안의 이름에서도 작업용 이름을 뺐습니다. 크레이트와 앱은 `ssr-*`(`ssr-live.exe` 등), 드라이버 폴더는 `driver/spatialsoftreceiver/`, 계약의 C 이름은 `SSR_*`와 `IOCTL_SSR_*`(헤더 `ssr_abi.h`), 코어의 C++ 이름공간은 `ssr`, 풀 태그는 `'DrsS'`(poolmon에서 `SsrD`), 덤프 파일은 `.ssrdump`와 매직 `SSRDUMP\0`, ETW 공급자 이름은 `SpatialSoftReceiver.Driver.Ks`(GUID는 그대로)입니다. 덤프 리더는 옛 매직 `BIODUMP\0`도 받습니다. 형식 비트의 이름도 상표 대신 기술 이름을 써서 `FORMAT_EAC3_JOC`(E-AC-3 JOC)로 바꿨습니다. 비트 값과 IOCTL 코드, 구조체 배치는 그대로라 바이너리 계약은 달라지지 않았습니다.
- **엔드포인트 이름은 데이터.** INF는 UTF-16LE이고, 영어(`[Strings]`)와 한국어(`[Strings.0412]`) 이름을 둡니다. Windows가 설치할 때 언어에 맞는 쪽을 골라 서비스 `Parameters`의 `EndpointName0`~`EndpointName3`에 씁니다. 드라이버는 시작할 때 그 값을 읽어 싱크 설명(31자)에 씁니다. 값이 없거나 비었거나 너무 길면 컴파일된 영어 이름으로 돌아갑니다. 그래서 동결 뒤에도 `ssr-setup`이 레지스트리 값을 쓰고 장치를 다시 시작하면 서명을 다시 받지 않고 이름을 바꿀 수 있습니다. 도구와 앱은 이름이 아니라 엔드포인트 번호와 KS 필터 참조 문자열(`WaveHdmiN`, `TopologyHdmiN`)로 엔드포인트를 찾습니다(`tools/ci/endpoints.ps1`).
- **PCM 형식 규칙(드라이버).** 계약의 PCM 비트를 규칙으로 바꾼 일(ADR 0024의 2026-10-13 고침)에 맞춰 드라이버를 고쳤습니다. 자세한 것은 `driver/README.md`의 레지스트리 절에 있습니다.
  - 레지스트리: 서비스 `Parameters`의 엔드포인트 i(0~3)마다 `CodecMask{i}`(REG_QWORD, 기본 `0x6c`), `PcmRateMask{i}`(REG_DWORD, `0x3f`), `PcmShapeMask{i}`(`0x1f`), `PcmMaxMaskedChannels{i}`(18), `PcmDirectChannels{i}`(`0xffffffff`)를 읽습니다. 옛 `FormatMask{i}`는 읽지 않습니다. 비트 필드는 컴파일된 범위와 AND하고, `PcmMaxMaskedChannels`는 비트가 아니라 상한이므로 18과 작은 쪽을 씁니다. 값이 없거나 형식·크기가 틀리면 그 필드의 기본값을 씁니다.
  - 형식 판정: PCM과 IEEE_FLOAT의 판정은 `driver/core/ssr_format.cpp`의 `PcmAllowed`가 표본율, 표본 모양, 채널 마스크와 채널 수, 블록 정렬, 바이트율, valid bits, 서브포맷 GUID를 정확히 봅니다. NewStream, SetFormat, 전체 형식 제안이 모두 이 판정을 지납니다. 비확장 WAVEFORMATEX는 1채널을 마스크 `0x4`, 2채널을 `0x3`으로 읽고 3채널 이상은 거절합니다.
  - 데이터 범위: 호스트 핀은 IEEE_FLOAT와 PCM의 성긴 범위(`KSDATARANGE_AUDIO`)를 하나씩 알립니다. float 범위가 먼저입니다. 범위는 표본율, 비트 폭, 최대 채널의 바깥 경계만 담으므로, 중간의 받지 않는 값은 판정이 거절합니다. 비트스트림 형식 표와 브리지 범위는 그대로입니다.
  - 교집합: `DataRangeIntersection`은 범위와 설정이 허락하면 엔드포인트의 OEM 기본 형식(0은 float32 7.1 48 kHz, 1과 2는 float32 스테레오 48 kHz)을 먼저 고르고, 고른 후보도 `PcmAllowed`로 확인합니다. 공유 장치 형식을 안정적으로 두려는 드라이버의 선택이고, Windows가 요구하는 규칙은 아닙니다. 그다음 후보 순서는 커널 밖 코어의 `ChoosePcmIntersection`이 정합니다. OEM 배치를 다른 모양과 레이트로 먼저 찾고, 이어 표준 스피커 배치, 마지막으로 엔진이 기본 직접 배치를 가진 채널 수의 마스크 0을 찾습니다(감사에서 넓은 범위를 받으면 32채널 직접 출력을 고르던 것을 고침, 시험 `PcmIntersectionOrder`).
  - 형식 종류 질의: `KSDATAFORMAT`만 온 형식 제안에 PCM이나 IEEE_FLOAT를 물으면, 그 엔드포인트의 PCM 규칙이 해당 표본 모양(PCM은 정수 모양, IEEE_FLOAT는 Float32)과 표본율을 하나 이상 켜고 마스크 채널 상한이나 직접 출력 채널이 0이 아니면 성공으로 답합니다. 코덱 서브포맷은 코덱 마스크로 답합니다.
  - 볼륨 노드의 채널 수는 엔드포인트의 기본 장치 형식 채널 수(0은 8, 1과 2는 2)이고, 32채널 직접 출력 스트림을 열어도 바뀌지 않습니다.
- **READ는 한 시각의 스냅숏.** 오디오 엔진이 위치를 묻거나(`GetPosition`, `GetPacketCount`, `GetOutputStreamPresentationPosition`) 쓰기 패킷을 알리면, 예전에는 그 스트림 하나만 지금까지 진행시키고 READ를 펌프했습니다. 그래서 한 READ가 한 엔드포인트의 오디오는 지금까지, 다른 엔드포인트의 오디오는 마지막 타이머 틱까지 담을 수 있었습니다. 틱이 늦으면 몇 ms가 됩니다(windows-2022 러너에서 4 ms). 믹서는 도착 여유(2 ms)를 넘은 차이를 뒤처진 차선의 언더런으로 셌습니다(종단 시험에서 `lane_underrun_frames` 471).
  - 이제 모든 위치 갱신은 어댑터의 `UpdatePositions`로 갑니다. 제어 잠금을 한 번 잡고 그 안에서 실행 중인 모든 스트림을 같은 QPC까지 기록합니다. 그래서 READ 안에서 엔드포인트들의 마지막 데이터 기록은 한 프레임 이내로 같은 시각에서 끝납니다. 상태 전환과 버퍼 해제, 어댑터 정지도 먼저 모든 스트림을 그 시각까지 진행시킵니다.
  - `driver_flow_check.py`는 READ를 하나씩 보관하고, 둘 이상의 엔드포인트가 데이터를 낸 READ마다 마지막 QPC의 차이를 잽니다. 100 µs를 넘으면 실패입니다(`snapshot_skew_us`).
- **형식 판정 퍼저.** `driver.yml`의 코어 잡이 `format_fuzz`(형식 판정기를 계측해 함께 빌드한 libFuzzer 대상)를 120초 돌립니다. SetFormat과 형식 제안으로 들어오는 신뢰할 수 없는 WAVEFORMATEX 바이트를 대상으로 합니다.

## 2026-10-14 고침

동결 전에 커널을 얇고 시험 가능하게 만들었습니다. 동결 뒤에 커널을 고치면 서명을 다시 받아야 하므로, 이 일은 지금밖에 할 수 없습니다.

- **형식 규칙은 코어 하나에.** 무엇을 받을지 정하는 판정이 커널(`SsrValidateFormat`, `SsrFormatTypeAllowed`, `SsrPcmEnabled`, 데이터 범위 유도)과 코어(`PcmAllowed`, `ChoosePcmIntersection`)에 따로 있었습니다. 이제 코어의 `PcmCandidateAllowed`(PCM 규칙 하나), `PcmTypeEnabled`, `PcmRange`, `FormatAccepted`(KS SubFormat과 WAVEFORMATEX 바이트), `FormatTypeAccepted`(형식만 있는 KSDATAFORMAT), 반송파 표 `kCodecCarriers`가 정하고, 커널은 KS 구조체를 옮기기만 합니다. 커널의 `SsrFormats`는 이 표에서 컴파일 때 만들고, 각 GUID와 채널 마스크가 WDK `ksmedia.h`의 정의와 같은지 `static_assert`로 확인합니다. 그래서 판정 전부를 코어 시험과 `format_fuzz`가 덮습니다.
- **계약의 상수.** 표본 모양 비트(`SSR_PCM_SHAPE_*`, `SSR_PCM_SHAPES_INTEGER`), 표본율 개수(`SSR_PCM_RATE_COUNT`), 기본 엔드포인트 마스크(`SSR_DEFAULT_ENDPOINT_MASK`, INF와 시험이 비교), 디코더가 기본 직접 배치를 가진 채널 수(`SSR_DIRECT_DEFAULT_CHANNELS`, 디코더 시험이 비교)를 `wire.rs`에 두고 헤더로 생성합니다. 값과 배치는 바뀌지 않았습니다.
- **자원 수명은 RAII.** 커널의 풀 메모리, 레지스트리 핸들, 객체 참조, COM 참조, 고해상도 타이머, 제거 잠금, WaveRT DMA 페이지는 `ssr_kernel.h`의 소유 타입(`PoolPtr`, `KernelHandle`, `ObjectRef`, `ComRef`, `ExTimer`, `RemoveLockHold`, `DmaPages`)이 놓습니다. 할당하는 것은 `PoolPtr::Allocate`뿐입니다. 스핀 락은 정적 분석이 IRQL을 함수 단위로 추적하므로 가드 없이 손으로 둡니다. 규칙 두 가지를 지킵니다. IRP에 묶인 풀 블록은 그 IRP의 제거 잠금을 놓기 전에 해제합니다(놓는 순간 제거와 언로드가 끝날 수 있어, Pool Tracking이 그 블록을 누수로 봅니다). 기다릴 수 있는 소멸자(`ExTimer`)는 스핀 락 안에서 돌지 않게, 잠금 안에서는 지역 변수로 옮기기만 하고 잠금을 놓은 뒤 소멸시킵니다. 전역 변수는 CRT가 없어 소멸자를 둘 수 없으므로 `constinit`인 날 값으로 둡니다.
- **C++23.** 커널은 `stdcpp23`(MSVC 19.44의 `/std:c++23preview`), 코어는 clang과 gcc의 `-std=c++23`, kcheck도 C++23입니다. C++20 이상에서 WDK 헤더가 내는 경고(C5054, `/permissive-`)는 꺾쇠괄호로 넣는 헤더를 외부 헤더로 두어 끄고, 따옴표로 넣는 우리 헤더는 `/W4 /WX`를 그대로 받습니다. 쓸 수 있는 기능은 세 컴파일러(MSVC 19.44, clang 18, gcc 13)가 모두 가진 것뿐이고, C++26 언어 기능은 MSVC가 하나도 구현하지 않아 쓰지 않습니다. 계약 헤더는 C89, C11, C++17, C++23으로 모두 컴파일되는지 확인합니다.
- **러너의 Verifier.** 휘발성으로 켤 수 있는 옵션이 Windows 버전마다 다릅니다. 예전 플래그(`0x209BB`)는 Server 2022에서 통째로 거절되어(종료 코드 1, 플래그 0, "No drivers are currently verified") 2022의 설치를 한 번도 검사하지 않았고, 거절은 경고로만 남았습니다. 이제 `tools/ci/verifier.ps1`이 Server 2022에서는 드라이버가 처음 올라오기 전에 휘발성 플래그 `0x9BB`를, Server 2025에서는 드라이버가 돌고 난 뒤에 `/dif` 규칙 클래스와 `/now`를 씁니다. `/dif ... /now`는 돌고 있는 드라이버에만 걸리고, 아직 올라오지 않은 드라이버는 `0xC0000C7B`(STATUS_DIF_VOLATILE_DRIVER_IS_NOT_RUNNING)로 거절되기 때문입니다. 거절, 재부팅 요구, 켠 뒤 `/query`에 드라이버가 없는 경우는 단계를 실패시킵니다.
- **러너에서 Verifier가 하지 못하는 것.** 첫 실행들이 보여 준 사실은 이렇습니다. Server 2022의 휘발성 설정은 드라이버를 목록에 올리고 로드와 언로드를 세지만(31번 로드), 풀 할당·스핀 락·IRQL 올림 계수가 모두 0으로 남아 호출을 하나도 가로채지 않았습니다. Server 2025의 `/dif ... /now`는 도는 이미지의 풀 할당을 실제로 가로채(9,525번, 모두 특수 풀) 그 이미지가 언로드할 때까지 검사하지만, 다음 로드에는 붙지 않습니다. 그래서 러너의 Verifier는 2025에서 설치 뒤의 재생, 볼륨, 종단 재생, 퍼저와 첫 언로드까지를 덮고, DriverEntry, AddDevice, StartDevice는 덮지 못합니다. 저자원 시뮬레이션은 두 이미지 모두에서 아무것도 실패시키지 못했습니다. Server 2022의 `/volatile /faults`는 호출을 가로채지 않으니 당연하고, Server 2025의 체계적 시뮬레이션(클래스 19, `/faultssystematic enableruntime`)도 재생과 퍼저가 도는 동안 할당 480번을 보고 하나도 실패시키지 않았습니다. 그래서 `verifier.ps1`은 저자원 모드를 두지 않고 표준 검사만 켭니다. 재부팅이 필요한 진짜 Verifier는 시험서명 PC의 안내서 7-1단계가 맡습니다.
- **Server 2022에서 I/O 검증 아래의 설치(조사 중).** 휘발성 Verifier가 실제로 켜지자, I/O 검증(`0x10`)을 건 채 설치하면 2022 러너를 잃었습니다(단계 시간 제한이 듣지 않고 뒤 단계가 돌지 않으므로 버그체크로 봅니다). 처음에는 새 드라이버와 KS 추적이 함께 있을 때만으로 보였지만 그렇지 않았습니다. 이벤트를 쓰지 않는 빌드, 공급자를 등록하지 않는 빌드, 추적 세션이 없는 릴리스 빌드, master의 드라이버도 모두 같았습니다. 그러니 이 PR의 회귀가 아니고, 예전에는 Verifier가 꺼져 있어 드러나지 않았을 뿐입니다. 다른 검사 플래그는 하나씩 걸어도 지나갔습니다. `driver.yml`의 진단 잡은 설치를 단계로 나눠 러너를 잃은 단계로 원인을 좁힙니다. 셋째 갈래에서는 오디오 스트림을 열기 전, 장치를 올리고 시작하는 첫 단계에서 러너를 잃었습니다. 그 단계에는 끝점 빌더의 필터 탐색이 함께 있으므로, 넷째 갈래는 오디오 서비스를 끈 채 장치를 시작하고 서비스는 다음 단계에서 켭니다. 대조로 둔 SysVAD는 이 이미지에 설치되지 않았습니다. 넷째 갈래에서는 오디오 서비스를 끈 채로도 첫 단계에서 러너를 잃어, 끝점 빌더의 탐색이 필요 없다는 것이 드러났습니다. 그래서 I/O 검증이 따지는 규칙에 어긋나는 세 곳을 고쳤습니다. PortCls는 CLEANUP을 등록하지 않는데 `SsrDispatch`가 그 자리를 가로채 기본 처리기를 드라이버 안에서 불러, 닫히는 핸들마다 실패할 수 없는 CLEANUP을 실패시켰습니다. 이제 PortCls가 비워 둔 CLEANUP은 성공으로 끝냅니다. 제어 장치는 PnP 스택에 없으므로 그 장치에 온 PnP, 전원, WMI 요청은 보낸 쪽이 둔 상태를 바꾸지 않고 끝냅니다. 전원 능력 콜백은 시스템 몫인 `DeviceState[PowerSystemUnspecified]`와 버스 몫인 `DeviceD1`/`DeviceD2`를 고쳐 쓰던 것을 SysVAD처럼 그대로 둡니다. 셋을 고친 뒤에도 첫 단계에서 러너를 잃었으므로, 다섯째 갈래는 드라이버의 결함 스위치로 시작을 둘로 나눠 찾습니다. `FaultInjectAt=k`이면 시작이 k번째 지점에서 실패하고 되감기므로(Verifier 없이 훑으면 모든 되감기가 깨끗합니다), 러너를 잃는 가장 작은 k가 원인의 자리를 알려 줍니다. 시험서명 PC의 Verifier 단계(안내서 7-1)도 I/O 검증을 켜므로, 원인을 찾기 전에는 그 단계에 주의를 적어 둡니다.
- **드라이버 자체의 결함 스위치.** 그래서 오류 갈래는 드라이버가 스스로 실패시킵니다. 서비스 `Parameters\FaultInjectAt`(DWORD, 기본 없음)을 DriverEntry가 한 번 읽고, 이미지가 올라온 뒤 자원을 얻는 N번째 지점이 한 번 실패합니다. 지점은 모든 풀 할당(`ssr::km::PoolAllocate`, `PoolPtr`, `operator new`), `ExTimer::Allocate`, `DmaPages::Allocate`, 제어 장치의 `IoCreateDeviceSecure`, `PcAddAdapterDevice`, `PcNewPort`, `PcRegisterSubdevice`, `PcRegisterPhysicalConnection`, `PcRegisterAdapterPowerManagement`이고, 실패는 그 호출이 실제로 실패할 때와 같은 갈래를 탑니다. DMA 타이머와 위치·패킷 경로, 스핀 락 안에는 지점이 없습니다. 풀, 타이머, DMA 페이지는 얻을 때와 놓을 때 세고, 언로드는 서비스 키 아래 휘발성 `Diagnostics`에 `Unloads`, `FaultCalls`, `FaultInjectedAt`, `OutstandingPool`, `OutstandingTimers`, `OutstandingDma`를 씁니다. `faults` 잡(`tools/ci/fault-sweep.ps1`)은 두 이미지에서 두 번 훑습니다. 처음에는 깨끗한 로드가 지나는 지점 수만큼 N을 1부터 올리며 장치를 껐다 켜고, 다음에는 켠 뒤 세 엔드포인트에 2초씩 소리를 내며 링을 읽는 재생(`driver_flow_check.py`)이 로드 뒤에 더하는 지점(스트림, DMA 버퍼, 타이머, 제어 장치)을 하나씩 실패시킵니다. 매번 그 인스턴스가 언로드하며 남긴 수가 모두 0이어야 하고, 실패가 걸린 재생이 120초 안에 끝나지 않으면 오류 갈래의 멈춤으로 보고 그 호출 번호와 함께 단계를 실패시킵니다. 마지막에는 깨끗한 인스턴스가 세 엔드포인트를 되살리고 재생 시험을 통과해야 합니다. 장치를 다시 시작하지 않고 껐다 켜는 까닭은, 시작에 실패한 인스턴스가 곧바로 언로드하며 보고를 쓰므로 다시 시작하면 다음 인스턴스의 보고가 그것을 덮기 때문입니다. 스위치는 관리자만 쓸 수 있는 서비스 키에 있고 값이 없으면 동작이 이전과 같으므로 모든 빌드에 둡니다.
- **READ는 할당하지 않습니다.** `IOCTL_SSR_READ`는 READ마다 문맥(`SsrReadContext`)을 풀에서 할당했습니다. 리더는 초에 천 번쯤 READ하므로 자주 도는 경로의 할당이었고, 재생 몇 초 동안 결함 지점이 700~900개 생겨 재생 훑기가 상한을 넘었습니다. 장치는 열린 파일 하나와 기다리는 READ 하나만 받으므로, 이제 문맥은 제어 장치 상태의 슬롯 하나(`ReadSlot`)입니다. 바로 끝나는 READ는 지역 값만 쓰고, 기다리는 READ만 바쁨 검사와 같은 잠금 안에서 슬롯을 차지합니다. 슬롯은 `FinishRead`(데이터, 기한, 취소, 정리, 정지, CSQ 삽입 실패)나, 제거 잠금을 얻지 못한 디스패치가 같은 잠금 안에서 놓습니다. 예전에는 바쁨 검사와 CSQ 삽입 사이에 다른 READ가 검사를 지날 틈이 있었고(그 READ는 `CsqInsert`에서 BUSY로 끝났습니다), 이제 그 틈도 없습니다.
- **스트림을 열 때 할당은 둘뿐.** 재생 훑기에서 스트림의 알림 이벤트 등록(`RegisterNotificationEvent`의 풀 할당)을 실패시키면, 드라이버는 실패를 그대로 돌려주는데도 앱은 오류를 받지 않았습니다. 오디오 엔진이 다른 방식으로 계속 재생했고, 그 결과가 매번 달랐습니다. 소리가 −33 dB로 깨지기도 했고(Server 2022, 33번 지점), 1초 늦게 제대로 나오기도 했고(37번), 재생이 끝나지 않아 120초 제한에 걸리기도 했습니다(41번). 이 실패는 드라이버가 깨끗하게 처리해도 앱을 멈출 수 있으므로 일어나지 않게 했습니다. 패킷 표는 알림 수의 상한(2)만큼, 이벤트는 네 칸을 스트림 객체 안에 둡니다. 오디오 엔진은 스트림마다 이벤트 하나를 등록하고, 다섯째 등록만 `STATUS_INSUFFICIENT_RESOURCES`로 거절합니다. 이제 스트림을 열 때 실패할 수 있는 것은 스트림 객체와 DMA 페이지 둘이고, 둘 다 앱에 오류로 돌아갑니다(`AUDCLNT_E_DEVICE_IN_USE`, `ERROR_NO_SYSTEM_RESOURCES`). 재생 훑기의 재생이 시간 안에 끝나지 않으면, 프로세스를 끝내기 전에 `hang-dump.ps1`로 python과 audiodg의 스택과 커널 덤프를 뜹니다. 끝낸 뒤에는 오디오 엔진이 스트림을 닫아 덤프에 남는 것이 없기 때문입니다.
