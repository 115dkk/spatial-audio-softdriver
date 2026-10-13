# Spatial SoftReceiver 가상 HDMI 드라이버

`spatialsoftreceiver.sys`는 순수 WDM + PortCls 드라이버입니다. Windows에 HDMI 렌더 엔드포인트 셋을 내놓고, 받은 바이트와 시각을 `\\.\SpatialSoftReceiver`의 기록 스트림으로 넘깁니다. 코덱 해석과 출력 정책은 사용자 모드가 맡습니다. 구조와 동결 범위는 [ADR 0027](../docs/adr/0027-driver-layout.md), 계약은 [ADR 0011](../docs/adr/0011-frozen-driver-contract.md), 입구별 모드 정책은 [ADR 0028](../docs/adr/0028-no-sound-is-swallowed.md)에 있습니다.

이 디렉터리는 [MS-PL](LICENSE)입니다. Microsoft Windows-driver-samples의 `audio/sysvad`를 출발점으로 삼으며, 기준 커밋은 `2dc3fd3a0cc84a2933f2194e7ec0871584979071`입니다. 파생 파일 머리에 원본 경로와 커밋을 적습니다. SpatialAudioLab 소스와 패치는 사용하지 않습니다.

## 파일 지도

| 파일 | 역할 |
| --- | --- |
| `bioatmos/adapter.cpp` | DriverEntry, PortCls 어댑터 등록, 디스패치와 PnP 수명 |
| `bioatmos/common.cpp`, `common.h` | 어댑터 공용 상태, 세 스트림이 공유하는 DMA 타이머와 전원 상태 |
| `bioatmos/bioatmos.h`, `minipairs.h` | 공용 선언과 `WaveHdmi0`/`TopologyHdmi0`, `WaveHdmi1`/`TopologyHdmi1`, `WaveHdmi2`/`TopologyHdmi2` 짝 |
| `bioatmos/minwavert.cpp`, `minwavert.h` | WaveRT 미니포트, 엔드포인트별 형식 규칙(코덱 마스크와 PCM 규칙)에 따른 데이터 범위와 스트림 생성 |
| `bioatmos/minwavertstream.cpp`, `minwavertstream.h` | 가상 DMA, 버퍼, 알림과 쓰기 패킷, 기록 생산 |
| `bioatmos/hdmiwavtable.h` | HDMI 렌더 형식과 핀 표 |
| `bioatmos/basetopo.cpp`, `basetopo.h` | 토폴로지 미니포트 공용 부분 |
| `bioatmos/hdmitopo.cpp`, `hdmitopo.h`, `hdmitoptable.h` | HDMI 잭 속성과 토폴로지 표 |
| `bioatmos/kshelper.cpp`, `kshelper.h` | KS 형식·속성 보조 함수 |
| `bioatmos/NewDelete.cpp`, `NewDelete.h` | 커널 메모리 할당·해제 |
| `bioatmos/control.cpp`, `control.h` | 보안 제어 장치, IOCTL, 취소 안전 READ 큐, 링과 트림 |
| `core/bio_core.h`, `core/*.cpp` | 커널 API 없는 링, READ 조건, DMA 정수 클럭과 형식 판정 |
| `bioatmos/spatialsoftreceiver.inx` | DCH 설치 입력; 빌드에서 StampInf로 `.inf` 생성 |
| `bioatmos/bioatmos.vcxproj`, `bioatmos.sln` | x64/ARM64 WDM 빌드 |
| `packages.config`, `Directory.Build.props` | WDK/SDK NuGet 버전과 MSBuild 연결 |
| `../tools/build-driver.ps1` | NuGet 복원, MSBuild, 결과 수집, InfVerif |
| `../tools/kcheck.py` | Linux의 실제 WDK/SDK 헤더 기반 컴파일 검사와 정적 분석 |

ABI의 유일한 정의는 `crates/bio-abi/src/wire.rs`입니다. 커널은 그 파일에서 생성한 `crates/bio-abi/include/bioatmos_abi.h`를 읽습니다. 생성 헤더나 IOCTL 번호를 드라이버 쪽에서 따로 고치지 않습니다.

## 제어 장치와 기록

제어 장치는 `\Device\SpatialSoftReceiver`, DOS 링크는 `\DosDevices\SpatialSoftReceiver`입니다. SYSTEM, 관리자, 대화형 사용자가 읽고 쓸 수 있도록 `IoCreateDeviceSecure`에 `D:P(A;;GA;;;SY)(A;;GRGW;;;BA)(A;;GRGW;;;IU)`를 전달합니다. 동시에 열린 제어 핸들은 하나만 허용하고, 이름이 붙은 하위 경로 열기는 거절합니다. 드라이버 디스패치는 제어 장치의 CREATE, CLOSE, CLEANUP, DEVICE_CONTROL만 제어 처리기로 보내고 다른 IRP는 `STATUS_INVALID_DEVICE_REQUEST`로 끝냅니다. PortCls 장치는 초기화 때 보존한 원래 디스패치 표로 보냅니다.

- `QUERY_INFO`: ABI/드라이버 판, 링 크기, 컴파일된 형식, 기능, QPC 주파수, 엔드포인트 마스크와 엔드포인트별 형식(코덱 마스크와 PCM 규칙)을 돌려줍니다.
- `READ`: `METHOD_OUT_DIRECT`로 온 출력 MDL에 온전한 기록만 옮깁니다. 최소 바이트, 긴급 기록(형식 변경·볼륨·정지), 기한 중 하나가 충족되면 완료합니다. 보류 READ는 하나만 허용합니다. 큐에서 꺼내 완료하는 중인 READ도 소유권을 유지해, 다른 READ가 그 기록을 먼저 가져가지 못하게 합니다. 첫 기록보다 작은 출력 버퍼는 `STATUS_BUFFER_TOO_SMALL`입니다. 한 번에 옮기는 양은 `BIO_READ_MAX_BYTES`(256 KiB)까지이고, 최소 바이트도 이 값으로 자릅니다. 데이터 기록 하나의 페이로드는 `BIO_MAX_DATA_PAYLOAD`(64 KiB)까지라 그보다 긴 구간은 여러 기록으로 나뉘므로, `record_size(BIO_MAX_DATA_PAYLOAD)` 이상인 버퍼의 READ는 항상 진행합니다.
- `SET_RATE_TRIM`: ppm × 65536 정수로 받은 트림을 ±2000 ppm으로 자릅니다. 세 엔드포인트의 DMA 클럭에 같은 값을 적용합니다.
- `GET_STATS`: 엔드포인트별 바이트, 넘침, 유실과 현재 에포크·상태를 돌려줍니다.

링 저장소는 비페이지 메모리 8 MiB이며 시작 때 한 번 할당합니다. DMA 클럭은 64비트 연산만 쓰는 이식 가능한 128비트 유리수 누산으로 QPC 틱, 바이트율, Q16 ppm 트림을 정확히 계산하고 분수와 블록 정렬 나머지를 다음 호출과 트림 변경 뒤에도 이어 갑니다. 데이터 기록의 `qpc`는 해당 갱신에서 가상 DMA가 기록의 끝을 지난 QPC입니다. DMA 버퍼가 감기면 두 메모리 구간을 `RecordRing::Data`에 넘깁니다. 갱신이 한 바퀴 넘게 늦어 이미 덮인 구간은 반복해서 복사하지 않습니다. `RecordRing::SourceGap`으로 유실을 먼저 알리고 남아 있는 최신 한 버퍼만 기록합니다. 이때 위치·유실 통계와 넘침/형식 반복/불연속 기록을 코어가 함께 처리합니다. 쓰기 패킷 기능이 꺼져 있으면 `qpc_written`은 0입니다. 켜진 경우 `SetWritePacket`은 패킷별 작성 QPC를 저장하고, 실제 바이트는 명목 DMA가 지난 시점에 기록합니다. ADR 0014의 후보인 쓰기 직후 조기 복사는 하지 않습니다. 형식 변경, RUN 진입, RUN 이탈은 각각 독립 기록입니다.

CLEANUP은 해당 핸들의 보류 READ를 취소하고 트림을 0으로 되돌린 다음 읽지 않은 기록을 버립니다. 성공한 CREATE도 링이 시작된 상태라면 대기 기록을 버려 새 핸들이 이전 세션의 데이터를 읽지 않게 합니다. 코어는 버린 뒤 알려진 현재 형식 → 현재 `VOLUME` → 현재 `StreamStart/Stop`을 `REPEAT`로 다시 싣고 첫 후속 데이터를 `DISCONTINUITY`로 표시합니다. 세션 경계에서 버린 바이트는 유실 통계에 넣지 않습니다. CLOSE와 CLEANUP은 별개의 경로입니다.

## 엔드포인트 볼륨과 음소거

토폴로지 경로는 브리지 핀 → `KSNODETYPE_VOLUME` → `KSNODETYPE_MUTE` → HDMI 잭입니다. `KSPROPERTY_AUDIO_VOLUMELEVEL`과 `KSPROPERTY_AUDIO_MUTE`는 GET/SET/BASICSUPPORT를 지원하고, 채널 수는 엔드포인트의 기본 장치 형식과 같습니다(0은 8, 1과 2는 2). 32채널 직접 출력 스트림을 열어도 볼륨 노드의 채널 수는 바뀌지 않습니다. 볼륨은 −96~0 dB, 0.5 dB 단계이며 가장 가까운 단계로 반올림합니다(정중앙은 0에서 먼 쪽). 채널 −1의 GET은 SysVAD처럼 채널 0, SET은 모든 지원 채널입니다. 다른 음수와 범위 밖 채널은 거절합니다.

초기값은 채널마다 0 dB, 음소거 해제입니다. 링 코어에 엔드포인트별로 보관하며 스트림 시작·정지·형식 변경·핸들 재열기는 값을 초기화하지 않습니다. 장치를 다시 시작하면 기본값으로 초기화하고 Windows가 저장된 설정을 다시 적용합니다. KS 속성 처리기는 **제어 상태의 링 스핀 락 하나** 아래에서 GET/SET, 시각 읽기, 상태 변경과 기록을 함께 처리합니다. 스트림 락을 역으로 잡지 않고, 락을 놓은 뒤 `BioPumpRead`를 불러 재생 중이 아니어도 변경을 전달합니다. 잠금을 소유하거나 할당하는 코드는 코어에 없습니다.

기록 종류 6 `VOLUME`은 136바이트 리틀엔디언 페이로드(`u32 channel_count`, `u32 mute_mask`, `i32 level_q16[32]`)입니다. 쓰지 않는 채널과 비트는 0입니다. 실제 값이 변할 때만 기록하고 QPC는 변경 시각이며, 재진술의 QPC는 재진술 시각입니다. 새 핸들과 넘침 복구의 순서는 알려진 `FormatChange` → `VOLUME` → `StreamStart/Stop`입니다(손실이 있으면 앞에 `Overrun`). 한 번도 볼륨을 받지 않은 구성된 엔드포인트도 기본값으로 재진술합니다. PCM/비트스트림 바이트 자체에는 드라이버가 게인을 곱하지 않습니다.

엔진은 `bio_abi::volume_gain`의 채널별 선형 이득 전력 평균 제곱근을 엔드포인트 신호에 정확히 한 번 적용해야 합니다. 음소거 채널의 전력은 0이며 채널 0을 마스터로 취급하지 않습니다. **이 엔진 적용은 후속 작업이고 현재 드라이버 변경만으로 음량이 줄어든다고 보장하지 않습니다.** Windows가 하드웨어 제어로 인식하면 기존 소프트웨어 감쇠를 하지 않기 때문에, 엔진 적용 전에는 볼륨/음소거를 안전장치로 믿고 청취하지 마십시오.

실기 검사는 `powershell -NoProfile -ExecutionPolicy Bypass -File tools/ci/volume_check.ps1 -Summary volume-summary.json`입니다. 다른 링 판독기는 종료해야 합니다. 검사기는 지원 비트와 dB/음소거 변경 기록을 확인하고 `finally`에서 원래 마스터·채널별 균형·음소거를 복원합니다. 수집기는 `endpoint-volumes.json`에 읽기 전용 상태를 남깁니다.

## IRQL과 잠금 규칙

- 장치 생성, 레지스트리 읽기, 미니포트 준비와 메모리 준비는 PASSIVE_LEVEL에서 합니다.
- DMA 갱신과 타이머 콜백은 DISPATCH_LEVEL까지 실행됩니다. 이 경로는 비페이지 코드·메모리만 사용하며, 파일 I/O와 대기 가능한 잠금, 동적 할당을 하지 않습니다.
- 링, 트림, 열린 핸들과 보류 READ의 공유 상태는 제어 상태의 스핀 락 하나로 직렬화합니다. 코어는 잠금을 소유하지 않으므로 `RecordRing` 호출자는 이 락을 잡아야 합니다. 어댑터에는 스트림 목록과 DMA 상태의 수명을 지키는 별도 스핀 락이 있습니다. 둘 다 필요할 때 순서는 반드시 **스트림 락 → 제어 락**이며 역순으로 잡지 않습니다.
- `IoCsq`의 획득·해제 콜백도 같은 락을 사용합니다. 이미 잡은 상태에서 `IoCsqInsertIrpEx`나 `IoCsqRemoveNextIrp`를 불러 재귀 획득하지 않습니다.
- MDL은 `MmGetSystemAddressForMdlSafe`와 `NormalPagePriority | MdlMappingNoExecute`로 매핑하고 실패를 처리합니다. `IoCompleteRequest`는 항상 스핀 락 밖에서 합니다.
- 세 엔드포인트는 1 ms 주기의 고해상도 DMA 타이머 하나와 QPC를 공유합니다. 타이머는 어댑터가 시작됐고 D0 전원 상태이며 RUN 스트림이 하나 이상일 때만 돕니다. D0을 떠나거나 마지막 스트림이 RUN을 떠나거나 어댑터를 멈추면 끄고, D0으로 돌아오면 RUN 스트림이 있을 때만 다시 켭니다.
- 보류 READ의 기한은 별도의 고해상도 타이머가 그 기한에 한 번 울려 확인합니다(한 번에 최대 1분, 남으면 다시 겁니다). 데이터를 기다리는 동안 주기적으로 깨어나지 않고, 기록 생산 직후에 완료 조건을 확인합니다.
- 링 수명은 별도 `EX_RUNDOWN_REF` 대신 **제어 락 + 시작 상태 + READ 세대 번호**로 보호합니다. 모든 링 접근과 MDL로의 `Read` 복사는 제어 락 안에서 이루어지고, 같은 락 아래의 `Started` 검사로 유효 저장소가 있는지 확인합니다. `Init`도 같은 락 안에서 어댑터 START 때만 하며, 스트림 RUN은 링을 초기화하지 않습니다.
- STOP/제거는 제어 락 아래에서 `Started=false`로 바꾸고 저장소 포인터를 분리합니다. 락을 놓은 뒤에만 비페이지 저장소를 해제합니다. DMA 타이머는 어댑터 중지에서, READ 타이머는 제어 장치 중지에서 `ExDeleteTimer(..., TRUE, TRUE, ...)`로 취소·콜백 대기를 끝내며, 둘 다 링과 장치 확장이 사라지기 전에 종료됩니다. 대기는 PASSIVE_LEVEL 정리 경로에서만 합니다(API 자체의 상한은 APC_LEVEL입니다).
- 성공한 링 초기화마다 세대 번호가 증가합니다. READ는 최초 검사 때 세대를 저장하고 CSQ 삽입과 큐에서 꺼낸 뒤 복사 전에 다시 비교합니다. 이전 START의 READ가 재시작한 링을 소비하지 못하게 하며, 분리되어 완료 중인 READ도 완료 전까지 다른 READ를 막습니다. 이전 어댑터의 스트림은 `active=false`와 스트림 락으로 추가 기록을 차단합니다.
- REMOVE는 추가로 제거 락의 IRP 참조가 모두 반환될 때까지 기다립니다. 열린 파일 객체와 어댑터 참조는 장치 확장 수명을 유지하므로, 제거 후 들어오는 CLEANUP/CLOSE도 확장을 유효하게 사용할 수 있습니다.

컴파일 검사만으로 취소·제거 경합의 안전성을 입증할 수는 없습니다. 아래 실제 장치 검증을 생략하지 않습니다. 특히 READ의 링 복사는 스핀 락 아래에서 실행되므로(한 번에 256 KiB까지) 최악의 락 유지 시간과 DPC 지연을 측정해야 합니다. 엔드포인트당 렌더 스트림은 하나만 허용합니다. 한 엔드포인트의 형식·에포크·기록 위치가 단일 스트림에 대응한다는 코어 계약에 맞춘 제한입니다.

## 레지스트리

위치는 서비스 키 `HKLM\SYSTEM\CurrentControlSet\Services\SpatialSoftReceiver\Parameters`입니다. INF는 서비스 설치 섹션의 `HKR` 상대 경로만 씁니다. AddDevice와 매 START 때 값을 읽으며, 어댑터 인스턴스가 설정 사본을 가집니다. 변경은 장치를 다시 시작한 뒤 반영됩니다.

| 이름 | 형식 | 기본값 | 뜻 |
| --- | --- | --- | --- |
| `EndpointMask` | DWORD | `0x7` | 비트 i가 엔드포인트 i를 켬. 현재 미니페어가 있는 0, 1, 2만 반영 |
| `CodecMask0`~`CodecMask3` | QWORD | `0x6c` | 비트스트림 코덱 비트 1~8; 0과 9~63은 예약 |
| `PcmRateMask0`~`PcmRateMask3` | DWORD | `0x3f` | 비트 0~5: 44100, 48000, 88200, 96000, 176400, 192000 Hz |
| `PcmShapeMask0`~`PcmShapeMask3` | DWORD | `0x1f` | 비트 0~4: int16, packed int24, int24-in-32, int32, float32 |
| `PcmMaxMaskedChannels0`~`PcmMaxMaskedChannels3` | DWORD | `18` | 스피커 마스크를 가진 PCM의 최대 채널 수; 0은 끔 |
| `PcmDirectChannels0`~`PcmDirectChannels3` | DWORD | `0xffffffff` | 비트 c−1: 마스크 0인 직접 출력의 c채널 허용(1~32) |
| `EndpointName0`~`EndpointName2` | REG_SZ | 설치 언어의 아래 이름 | HDMI 싱크 설명. 31 UTF-16 코드 단위까지 |
| `EndpointName3` | REG_SZ | `Spatial SoftReceiver` | 예비 슬롯. 현재 미니페어 없음 |
| `WritePacket` | DWORD | `0` | 1일 때 `IMiniportWaveRTOutputStream`과 기능 비트를 노출 |

기본 코덱은 E-AC-3, E-AC-3 Atmos, MAT 2.0, MAT 2.1입니다. AC-3, TrueHD, DTS 계열은 기본으로 꺼져 있습니다. DTS:X E1/E2는 `BIO_FORMAT_DTS_HD`로 묶습니다. INF의 QWORD 플래그는 `0x000B0001`, DWORD는 `0x00010001`입니다. 값이 없거나 타입·크기가 틀리면 해당 필드의 기본값을 사용합니다. 코덱·레이트·모양·직접 출력 비트는 컴파일된 범위와 AND하고, `PcmMaxMaskedChannels`는 비트 마스크가 아닌 상한이므로 18과 최솟값을 취합니다. 옛 `FormatMask{i}`는 읽지 않습니다. 비노출 엔드포인트의 `AbiInfo.endpoints[i]` 전체는 0입니다. `EndpointMask`의 사용 가능한 비트를 가린 결과가 0이면 기본값 `0x7`을 씁니다. 슬롯 3은 예비입니다.

PCM은 레이아웃 표가 아니라 위 표의 레이트 × 모양 × 채널 규칙입니다. 0이 아닌 `dwChannelMask`는 Windows의 18 스피커 비트(`0x3ffff`)만 쓰고 켜진 비트 수가 채널 수와 같아야 합니다. 따라서 5.1 back/side, 6.1, 7.1 wide, 높이 채널 조합과 18채널 전체를 별도 표 없이 받습니다. Windows 비트로 표현할 수 없는 위치가 있는 9.1.4/9.1.6 등은 마스크 0의 직접 출력으로 받고, 채널 순서 해석은 엔진의 별도 계약입니다. 비확장 WAVEFORMATEX는 mono=0x4, stereo=0x3으로 해석하며 3채널 이상은 거절합니다. 블록 정렬, 바이트율, 길이, GUID와 valid bits도 `core/bio_format.cpp::PcmAllowed`에서 검사합니다. 링은 8 MiB 그대로입니다.

### 성긴 데이터 범위와 교집합

PCM과 IEEE_FLOAT에 각각 `KSDATARANGE_AUDIO` 하나를 만들며, 레이트·정수 폭·최대 채널의 외곽 범위만 알립니다. 중간의 지원하지 않는 값은 허용하지 않습니다. Microsoft의 [Data Ranges with Discrete Values](https://learn.microsoft.com/en-us/windows-hardware/drivers/audio/data-ranges-with-discrete-values)는 이 표현을 허용하되 잘못 고른 중간값은 NewStream에서 거절하도록 설명합니다. 여기서는 교집합 후보도 `PcmAllowed`로 확인하고, NewStream과 SetFormat 및 전체 형식 제안은 같은 `BioValidateFormat` → core 판정 경로를 씁니다. 비트스트림 carrier 표와 RAW 모드 속성, 브리지의 ANALOG 범위는 유지합니다.

`BioPcmIntersection`은 양쪽 범위와 설정이 허용하면 OEM 기본 형식(0: float32 7.1 48 kHz, 1/2: float32 stereo 48 kHz)을 우선합니다. 후보 순서는 `driver/core/bio_format.cpp`의 `ChoosePcmIntersection`이 정하고 `core_tests.cpp`의 `PcmIntersectionOrder`가 시험합니다. (1) OEM 기본 형식, (2) OEM 레이트·배치의 다른 표본 모양(float32, int32, int24-in-32, int24, int16 순), (3) OEM 배치의 다른 레이트(48, 44.1, 96, 88.2, 192, 176.4 kHz 순), (4) 표준 스피커 배치(7.1 서라운드 `0x63f`, 5.1 `0x3f`, 5.1 옆 `0x60f`, 7.1.4 `0x2d63f`, 7.1 `0xff`, 쿼드 `0x33`, 스테레오, 모노 순), (5) 엔진이 기본 직접 배치를 가진 채널 수의 마스크 0(2, 8, 6, 12, 16, 10, 14, 1, 3, 4, 5채널 순)입니다. 그 밖의 직접 출력 채널 수는 앱이 그 형식을 직접 제안하면 받지만 교집합이 먼저 고르지는 않습니다. 넓은 범위를 받아도 엔진이 재생하지 못하는 32채널 직접 출력을 고르지 않게 하려는 순서입니다. 고른 후보는 구체적인 WAVEFORMATEXTENSIBLE로 반환합니다. 이것은 공유 장치 형식을 안정적으로 유지하려는 드라이버의 선택 정책이지 Windows가 요구한 OEM 우선 규칙은 아닙니다. [IMiniport::DataRangeIntersection](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/portcls/nf-portcls-iminiport-datarangeintersection)은 구체 형식 반환과 0바이트 질의의 `STATUS_BUFFER_OVERFLOW`, 짧은 버퍼의 `STATUS_BUFFER_TOO_SMALL`, 교집합 없음의 `STATUS_NO_MATCH`를 규정합니다. SysVAD 기준 커밋의 `audio/sysvad/EndpointsCommon/minwavert.cpp::DataRangeIntersection`은 채널 검사 후 PortCls로 위임하지만, 여기서는 성긴 범위의 구멍을 고르지 않도록 직접 처리합니다. KS 범위에는 개별 채널 마스크/valid bits 제약이 없으므로 특정 배치를 원하는 앱은 전체 형식을 제안해야 합니다.

### QUERY_INFO 배치 (ABI_VERSION 1, 동결 전)

160바이트, 정렬 8바이트입니다. 예약 필드는 쓰는 쪽 0, 읽는 쪽 무시입니다.

| 오프셋 | 필드 | 바이트 |
| --- | --- | --- |
| 0 / 4 / 8 / 12 | abi_version / struct_size / driver_version / ring_bytes | 각각 4 |
| 16 | compiled: EndpointFormats | 24 |
| 40 / 48 | features / qpc_frequency | 각각 8 |
| 56 / 60 | endpoint_mask / reserved | 각각 4 |
| 64 / 88 / 112 / 136 | endpoints[0..3]: EndpointFormats | 각각 24 |

EndpointFormats 내부는 codecs +0(u64), pcm +8(16바이트)입니다. PCM 내부는 rates +0, shapes +4, max_masked_channels +8, direct_channels +12(모두 u32)입니다. `bio-live devices`는 각 엔드포인트의 코덱 이름과 PCM 규칙을 출력합니다.

### 이름은 설치 데이터

INF의 `[Strings]`는 영어, `[Strings.0412]`는 한국어 이름을 서비스 `Parameters\EndpointName0..2`와 인터페이스 `FriendlyName`에 씁니다. 원본 `spatialsoftreceiver.inx`는 한국어를 보존하도록 **UTF-16LE BOM**으로 저장하며 StampInf가 만든 INF도 이 인코딩을 사용합니다([Microsoft 지침](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/general-guidelines-for-inf-files)).

시작 시 PASSIVE_LEVEL에서 `EndpointName0..3`을 읽어 어댑터 설정에 복사하고, HDMI `KSJACK_SINK_INFORMATION.SinkDescription`에 씁니다. REG_SZ가 아니거나, 비었거나, NUL을 뺀 길이가 31 UTF-16 코드 단위를 넘거나, 잘못 종결된 값은 `core/bio_names.h`의 번호별 영어 기본값으로 돌아갑니다. 잘라서 쓰지 않습니다. 값 읽기는 스트리밍 경로에서 하지 않습니다.

나중에는 `bio-setup`(M4)이 이 레지스트리 값을 쓰고 해당 장치를 다시 시작합니다. 서명된 SYS·INF·CAT를 수정하지 않으므로 이름 변경에 재서명이 필요 없습니다. Windows 소리 설정에서 사용자가 바꾼 별칭은 싱크 설명과 다를 수 있습니다.

| 번호 | 영어 Windows | 한국어 Windows |
| --- | --- | --- |
| 0 | Spatial SoftReceiver | 공간 소프트리시버 |
| 1 | Spatial SoftReceiver Music | 공간 소프트리시버 음악 |
| 2 | Spatial SoftReceiver PASS | 공간 소프트리시버 PASS |

도구는 이름을 식별자로 쓰지 않습니다. `tools/ci/endpoints.ps1`은 `IMMDevice` → `IDeviceTopology` → 커넥터의 `GetDeviceIdConnectedTo`로 얻은 KS 필터 경로에서 `TopologyHdmi0..2`/`WaveHdmi0..2` 참조 문자열을 읽고 PnP 소유 인스턴스까지 맞춥니다. `endpoints.py`는 JSON `{endpoint,id,name,state}`를 검증하고 soundcard를 정확한 `id`로 엽니다. 중복 활성 번호는 실패이며, 이름은 사람이 읽는 출력에만 남깁니다. 분리·제거된 장치가 토폴로지를 내주지 않으면 이름으로 추측하지 않고 누락으로 다룹니다.

## 엔드포인트와 독점 정책

| 번호 | 이름 | 용도 | 독점 허용 / 우선권 | OEM 기본 형식 |
| --- | --- | --- | --- | --- |
| 0 | Spatial SoftReceiver | 기본 장치, 시스템 소리, 공간음향 | 0 / 0 | float32 7.1 48 kHz |
| 1 | Spatial SoftReceiver Music | 공유 음악 앱 | 0 / 0 | float32 2.0 48 kHz |
| 2 | Spatial SoftReceiver PASS | 비트스트림과 독점 PCM | 1 / 1 | float32 2.0 48 kHz |

`EndpointMask`의 비트 2를 끄고 장치를 다시 시작하면 패스스루 미니페어를 등록하지 않습니다. 예비 비트 3은 무시합니다. 마스크를 가린 결과가 0이면 기본값 0x7로 돌아갑니다. `CodecMask2`와 PCM 규칙의 기본은 엔드포인트 0과 같으며, **TrueHD·AC-3·DTS가 기본으로 켜졌다는 뜻은 아닙니다.** 디코더 지원에 맞춰 `CodecMask{i}`의 코덱 비트를 켜는 일은 별개입니다.

INF의 `EP\0`은 각 토폴로지 인터페이스 안의 엔드포인트입니다(셋째 장치도 `EP\0`, `EP\2`가 아님). OEMFormat은 [Microsoft 문서](https://learn.microsoft.com/en-us/windows-hardware/drivers/audio/pkey-audioengine-oemformat)의 직렬화된 VT_BLOB(48바이트)로 넣습니다.

독점 키는 공개 SDK PKEY가 아닌 `{b3f8fa53-0004-438e-9003-51a46e139bfc},3/4`의 DWORD입니다. 일반 EP 속성 전달은 문서화되어 있지만 이 키들의 INF 적용은 확인되지 않아, 설치 후 `tools/testkit/install.ps1`이 `tools/ci/exclusive-policy.ps1`의 관리자 `IMMDevice::OpenPropertyStore(STGM_READWRITE)` → `SetValue` → `Commit`으로 씁니다. 값 읽기만으로 성공으로 보지 않고 새 `IAudioClient`로 아래 검사를 실행합니다. 서비스 재시작이나 레지스트리 ACL 변경은 하지 않습니다. 이 키의 의미·즉시 반영은 Windows 버전마다 실기 검사로 확인해야 하며, 적용이나 검사 실패는 설치 종료 코드 1입니다.

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools/ci/exclusive_policy_check.ps1 -ExpectMask 7 -Summary exclusive-policy-summary.json
```

합격은 활성 장치 집합이 마스크와 같고, 속성 값이 표와 같으며, 장치가 `IsFormatSupported(EXCLUSIVE)`로 받는 PCM 형식의 `Initialize(EXCLUSIVE)`가 0/1에서 `0x8889000E` (`AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED`), 2에서 `S_OK`인 경우입니다. 플레이어를 먼저 종료하십시오. 지원 형식 없음·장치 사용 중·다른 HRESULT를 정책 성공으로 바꾸지 않습니다. 검사 중 소리를 재생하지는 않습니다. `collect.ps1`은 읽기 전용 `endpoint-policy.json`을 모으고, 제거기는 장치 제거 전에 두 속성을 VT_EMPTY로 지우고 확인합니다. 표시 이름과 무관하게 고정 KS 참조 문자열과 장치 소유 정보를 확인합니다.

**패스스루는 커널에서 공유 모드를 금지한 엔드포인트가 아닙니다.** [Windows 독점 정책](https://learn.microsoft.com/en-us/windows/win32/coreaudio/exclusive-mode-streams)은 독점 허용/선점만 제어하므로 비어 있을 때 공유 스트림도 열립니다. `driver_flow_check.py`는 엔드포인트 셋에 공유 톤을 보내 링 분리·연속성을 검사하고 실패하면 불합격으로 처리합니다. 잘못 들어온 공유 앱을 알리고 다른 입구로 보내는 supervisor 정책은 M4에 남습니다. 패스스루에서 독점이 공유를 선점하는 동작과 공유 입구의 공간음향은 시험서명 PC에서 확인합니다.

## Windows 빌드

Visual Studio C++ 빌드 도구와 대상 아키텍처용 컴파일러가 필요합니다. 드라이버는 `ExAllocatePool2`를 사용하므로 Windows 10 2004(빌드 19041) 이상이 대상입니다. WDK와 SDK는 사전 설치본이 아니라 NuGet의 다음 다섯 패키지를 `10.0.26100.6584`로 고정해서 씁니다.

- `Microsoft.Windows.WDK.x64`, `Microsoft.Windows.WDK.arm64`
- `Microsoft.Windows.SDK.CPP`, `Microsoft.Windows.SDK.CPP.x64`, `Microsoft.Windows.SDK.CPP.arm64`

저장소 루트에서 실행합니다.

```powershell
.\tools\build-driver.ps1 -Configuration Debug -Platform x64
.\tools\build-driver.ps1 -Configuration Release -Platform ARM64
```

스크립트는 `nuget restore driver/packages.config -PackagesDirectory driver/packages` 뒤에 VS 개발 환경을 찾아 MSBuild를 실행합니다. NuGet이 PATH에 없으면 도구를 내려받습니다. 결과 `.sys`, `.inf`, `.pdb`는 `target/driver/<Configuration>/<Platform>/`에 모으고 `InfVerif /w /v` 실패는 비영 종료로 전달합니다. 서명은 하지 않습니다. 복원한 `driver/packages/`는 버전 관리에서 제외됩니다.

## Linux 검사

```bash
python3 tools/kcheck.py --analyze
```

다른 캐시 경로가 필요하면 `BIO_KCHECK_CACHE`를 설정합니다. 도구는 SHA-256으로 고정한 NuGet 헤더에 대해 x64와 ARM64를 clang의 MSVC 모드로 컴파일 검사하고 x64 정적 분석을 실행합니다. 입력 파일을 지정하지 않으면 `driver/bioatmos/*.cpp` 전체를 검사합니다.

이는 링크나 MSVC/WDK 실제 빌드가 아닙니다. Windows CI의 x64·ARM64 Debug·Release 빌드, `/W4 /WX`, 코드 분석, InfVerif, CodeQL, 서명 검증은 별도 게이트입니다. 시험서명 PC에서는 Driver Verifier, HVCI 로드, IOCTL 퍼징, READ 취소·정리·제거 경합, 형식 협상, 세 엔드포인트의 공간음향 활성화, 쓰기 패킷/EOS와 트림을 검증해야 합니다.

### 커널에 올린 뒤의 필수 경계 시험

- READ: 빈 출력, 첫 기록보다 1바이트 작은 출력, 최소 바이트가 출력보다 큼, 기한 0, 무음 상태의 기한 만료, 두 번째 보류 요청, 이미 취소된 요청, 생산과 취소의 경합.
- 수명: 중복 CREATE, 보류 중 CLEANUP/CLOSE, 핸들을 연 채 STOP/제거, 스트림 RUN 중 surprise removal, START 실패 후 제거, 장치 중지 후 다시 시작.
- 형식: 각 마스크 비트를 따로 켜고 열거·NewStream·SetFormat 결과 일치 확인, 0 마스크, 세 엔드포인트의 서로 다른 마스크, 불완전한 WAVEFORMATEX와 큰 cbSize 거절.
- 시간: DMA 버퍼 감김, 한 바퀴 넘는 갱신 지연과 유실 기록, ±2000 ppm 경계 및 범위 밖 트림, 세 엔드포인트의 서로 다른 샘플 레이트, 전원 복귀.
- 패킷: WritePacket 기본 꺼짐, 켠 뒤 순서가 늦거나 빠른 패킷, 32비트 패킷 번호 감김, 0바이트 EOS와 부분 EOS, EOS 이후 추가 쓰기, 패킷별 `qpc_written` 대응.

## API 근거

- [PcAddAdapterDevice와 어댑터 전용 확장 슬롯](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/portcls/nf-portcls-pcaddadapterdevice)
- [IoCreateDeviceSecure와 고유 클래스 GUID](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdmsec/nf-wdmsec-wdmlibiocreatedevicesecure)
- [IoCsqInsertIrpEx의 삽입·취소 소유권](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdm/nf-wdm-iocsqinsertirpex)
- [ExDeleteTimer의 콜백 대기와 IRQL](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdm/nf-wdm-exdeletetimer)
- [SetWritePacket의 패킷·EOS 계약](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/portcls/nf-portcls-iminiportwavertoutputstream-setwritepacket)
- [AllocateBufferWithNotification](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/portcls/nf-portcls-iminiportwavertstreamnotification-allocatebufferwithnotification)
- [INF AddReg의 HKR 문맥과 QWORD 플래그](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/inf-addreg-directive)
