# 0027. 드라이버는 SysVAD의 HDMI 렌더 둘과 제어 장치 하나로 만들고, 커널 밖에서 시험할 코어를 떼어 낸다

상태: 채택, 2026-10-12. ADR 0004, 0011, 0014, 0024를 `bioatmos.sys`의 구조로 옮깁니다.

## 배경

M3는 우리 드라이버를 만들고 ABI v1을 동결합니다. 동결한 뒤에는 EV의 1년(ADR 0004)이 지나면 고칠 수 없으므로, 서명 전에 결함을 최대한 많이 잡아야 합니다. 그런데 개발은 Linux 컨테이너와 GitHub 호스트 러너에서 하고, 호스트 러너는 재부팅할 수 없어 시험서명 모드를 켤 수 없습니다. 커널에 올리는 검사(Driver Verifier, IOCTL 퍼저, HVCI 로드, 형식 협상)는 시험서명 PC에서만 됩니다. 그래서 커널에 올리지 않고도 검증되는 부분을 가능한 크게 떼어 내는 구조로 정합니다.

## 결정

### 출발점과 범위

- 위치는 `driver/`, 언어는 C++17(예외와 RTTI 없음), 라이선스는 MS-PL입니다(ADR 0003).
- Microsoft Windows-driver-samples의 SysVAD에서 TabletAudioSample의 HDMI 렌더 경로만 가져옵니다(adapter, common, basetopo, kshelper, minwavert, minwavertstream, hdmitopo, hdmitoptable, hdmiwavtable, NewDelete). 마이크, 블루투스, USB, S/PDIF, 키워드 감지, APO, 오프로드, 오디오 엔진 노드, 저장 파일(savedata)은 들이지 않습니다. 가져온 파일의 머리에는 원본 경로와 커밋을 적습니다.
- SpatialAudioLab의 드라이버와 그 패치는 읽지 않습니다(ADR 0005). SysVAD와 Microsoft 문서만 봅니다.
- WDF(KMDF)는 쓰지 않습니다. SysVAD는 블루투스와 USB 사이드밴드의 작업 항목에만 WDF를 쓰므로, 그 장치를 빼면 순수 WDM과 PortCls로 충분합니다.

### 장치

- 루트 열거 장치 하나(`ROOT\BioAtmos`)가 렌더 엔드포인트 둘을 냅니다. 엔드포인트마다 WaveRT 미니포트와 토폴로지 미니포트가 짝을 이루고, 잭은 HDMI입니다. 둘째 엔드포인트는 서비스 `Parameters` 키의 `SecondEndpoint` DWORD(기본 1)로 켜고, 장치를 다시 시작하면 반영합니다(ADR 0024).
- 형식 표는 SysVAD HDMI 표의 상위집합(PCM, AC-3, E-AC-3, MAT 2.0, MAT 2.1, DTS, DTS:X)으로 컴파일하고, 엔드포인트마다 `FormatMask0`, `FormatMask1` QWORD로 켭니다. 비트는 `bio-abi`의 `FORMAT_*`입니다. DTS:X E1과 E2(하위 형식 0x010b, 0x030b)는 DTS-HD 컨테이너이므로 `FORMAT_DTS_HD`로 묶고, 어느 쪽인지는 디코더가 스트림에서 읽습니다. 기본값은 PCM, AC-3, E-AC-3, E-AC-3 Atmos, MAT 2.0, MAT 2.1이고 TrueHD와 DTS는 디코더가 생길 때 `bio-setup`이 켭니다. 마스크는 `bio-setup`만 씁니다(ADR 0011).

### 커널 밖에서 시험하는 코어

- 링, 기록 쓰기, 넘침 처리와 엔드포인트별 유실 계산, READ 완료 조건(최소 바이트, 형식 변경, 정지, 시간 초과), 트림 적용(±2000 ppm 자르기, 위치 계산), 형식 마스크 판정은 `driver/core/`에 둡니다. 코어는 커널 API를 부르지 않고 할당하지 않으며, 바깥에서 준 메모리와 시각만 씁니다.
- 커널 쪽은 잠금(스핀 락), 타이머, IRP와 취소 안전 큐, MDL, 레지스트리만 감쌉니다.
- 코어는 같은 소스를 사용자 모드 시험 프로그램으로도 빌드합니다. Windows에서는 MSVC로, Linux에서는 clang과 gcc로 빌드하고 AddressSanitizer와 UBSan을 켜며, libFuzzer로 링과 READ 조건을 퍼징합니다.
- 코어 시험이 쓴 기록 스트림을 `bio-abi`의 파서로 읽어 같은 값인지 확인합니다. 드라이버와 시험은 `bio-abi`가 생성한 `bioatmos_abi.h` 하나만 include하므로 배치가 어긋나면 컴파일에서 멈춥니다.

### 제어 장치

- `\\.\BioAtmos`는 `IoCreateDeviceSecure`로 만들고 SDDL은 SYSTEM, 관리자, 대화형 사용자에게 읽기와 쓰기를 줍니다(ADR 0011). PortCls가 쥔 디스패치 표 앞에서 CREATE, CLOSE, CLEANUP, DEVICE_CONTROL을 받아, 제어 장치로 온 것만 처리하고 나머지는 `PcDispatchIrp`로 넘깁니다.
- 핸들은 하나만 엽니다. 두 번째 열기는 `STATUS_SHARING_VIOLATION`입니다. CLEANUP에서 보류 중인 READ를 취소하고 트림을 0으로 돌립니다.
- READ는 `METHOD_OUT_DIRECT`이고 보류는 하나만 받습니다. 두 번째 보류는 `STATUS_DEVICE_BUSY`입니다. 완료 판단은 기록을 쓰는 타이머 DPC에서 하고, 시간 초과는 READ의 기한에 한 번 울리는 타이머가 셉니다.
- 링에서 출력 버퍼로 옮기는 복사는 스핀 락 아래에서 하므로 한 번에 `READ_MAX_BYTES`(256 KiB, MAT 2.1로 약 85 ms)까지만 옮깁니다. 데이터 기록의 페이로드는 `MAX_DATA_PAYLOAD`(64 KiB)까지이고 더 긴 구간은 나눠 씁니다. 두 값은 `bio-abi`의 계약 상수이고, `record_size(MAX_DATA_PAYLOAD)` 이상인 버퍼는 항상 진행합니다.

### 링과 시각

- 링은 비페이지 메모리 8 MiB(MAT 2.1의 3.07 MB/s로 약 2.7초)이고 `AbiInfo::ring_bytes`로 알립니다.
- 사용자 모드가 늦어 링이 차면 가장 오래된 기록부터 통째로 덮어쓰고, 덮어쓴 데이터 바이트를 엔드포인트별로 세어 다음 기록 앞에 엔드포인트마다 넘침 기록을 하나씩 넣습니다(ADR 0024).
- 데이터 기록의 `qpc`는 가상 DMA가 그 기록의 마지막 바이트를 지난 시각입니다. 두 엔드포인트는 같은 타이머를 쓰므로 같은 클럭 도메인입니다.
- DMA 타이머(1 ms)는 RUN 상태의 스트림이 하나라도 있을 때만 돕니다. 재생하지 않는 PC에서 1 ms마다 깨어나는 일은 없습니다.
- 트림은 위치 계산에 `1 + ppm / 1e6`을 곱해 넣습니다. 엔드포인트 둘에 똑같이 걸립니다.

### 쓰기 패킷 알림

SysVAD에는 `SetWritePacket` 경로가 있습니다. 동결 뒤에는 더할 수 없으므로 구현해 넣되 `Parameters`의 `WritePacket` DWORD(기본 0)로 켭니다. 켜면 `qpc_written`을 채우고 `FEATURE_WRITE_PACKET`을 알립니다. 쓸지는 ADR 0014대로 시험서명 PC에서 공간음향 엔진의 호출 주기를 재고 정합니다.

### Linux에서 하는 검사

`tools/kcheck.py`는 Microsoft가 NuGet으로 내는 WDK와 Windows SDK 패키지(SHA-256으로 고정)의 헤더를 받아, clang의 MSVC 모드로 드라이버의 모든 소스를 x64와 ARM64로 컴파일 검사하고 clang 정적 분석기를 돌립니다. Windows의 파일 이름은 대소문자를 가리지 않으므로 헤더가 쓰는 철자마다 심볼릭 링크를 둡니다. 링크하지 않으므로 빌드가 아니라 형식과 문법 검사이고, 진짜 빌드는 Windows CI가 합니다. 드라이버를 고친 사람이 컨테이너 안에서 CI 전에 돌리는 것이 목적입니다.

### CI에서 하는 검사

호스트 러너(windows-2022, WDK 10.0.26100)에서 다음을 모두 돌립니다.

- x64와 ARM64, Debug와 Release 빌드. `/W4 /WX`와 코드 분석(`/analyze`, 드라이버 규칙)을 켭니다.
- `InfVerif /w`, 시험 인증서로 `Inf2Cat`과 `signtool` 서명 및 검증.
- CodeQL의 Windows 드라이버 질의 묶음(WHCP 필수 규칙).
- 패키지 설치 시험: 시험 인증서를 신뢰 저장소에 넣고 `pnputil /add-driver`로 드라이버 저장소에 넣은 뒤 루트 장치를 만듭니다. 시험서명 모드가 아니므로 커널 로드는 문제 코드 52로 멈추는 것이 정상이고, 그 앞까지(INF 섹션, 서비스, `Parameters` 기본값, 장치 노드)를 확인합니다.
- 코어 시험(MSVC, clang, gcc, ASan, UBSan), libFuzzer, `bio-abi` 교차 검사.

## 결과

- 커널에 올려야만 알 수 있는 것(Dolby Atmos for home theater가 열리는지, 트림을 오디오 엔진이 받아들이는 범위, 쓰기 패킷 주기, Driver Verifier, HVCI 로드, IOCTL 퍼저)은 여전히 시험서명 PC에서 확인합니다. CI는 그 앞까지를 맡습니다.
- 코어와 커널 감싸개의 경계가 생기므로, 링과 READ의 결함은 사용자 모드에서 재현됩니다.
- 커널 코드는 SysVAD에서 쓰지 않는 장치를 뺀 만큼 줄어듭니다.
