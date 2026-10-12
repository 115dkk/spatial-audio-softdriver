# BIO 가상 HDMI 드라이버

`bioatmos.sys`는 순수 WDM + PortCls 드라이버입니다. Windows에 HDMI 렌더 엔드포인트 둘을 내놓고, 받은 바이트와 시각을 `\\.\BioAtmos`의 기록 스트림으로 넘깁니다. 코덱 해석과 출력 정책은 사용자 모드가 맡습니다. 구조와 동결 범위는 [ADR 0027](../docs/adr/0027-driver-layout.md), 계약은 [ADR 0011](../docs/adr/0011-frozen-driver-contract.md), 두 엔드포인트의 규약은 [ADR 0024](../docs/adr/0024-second-render-endpoint.md)에 있습니다.

이 디렉터리는 [MS-PL](LICENSE)입니다. Microsoft Windows-driver-samples의 `audio/sysvad`를 출발점으로 삼으며, 기준 커밋은 `2dc3fd3a0cc84a2933f2194e7ec0871584979071`입니다. 파생 파일 머리에 원본 경로와 커밋을 적습니다. SpatialAudioLab 소스와 패치는 사용하지 않습니다.

## 파일 지도

| 파일 | 역할 |
| --- | --- |
| `bioatmos/adapter.cpp` | DriverEntry, PortCls 어댑터 등록, 디스패치와 PnP 수명 |
| `bioatmos/common.cpp`, `common.h` | 어댑터 공용 상태, 두 스트림이 공유하는 DMA 타이머와 전원 상태 |
| `bioatmos/bioatmos.h`, `minipairs.h` | 공용 선언과 `WaveHdmi0`/`TopologyHdmi0`, `WaveHdmi1`/`TopologyHdmi1` 짝 |
| `bioatmos/minwavert.cpp`, `minwavert.h` | WaveRT 미니포트, 엔드포인트별 형식 마스크와 스트림 생성 |
| `bioatmos/minwavertstream.cpp`, `minwavertstream.h` | 가상 DMA, 버퍼, 알림과 쓰기 패킷, 기록 생산 |
| `bioatmos/hdmiwavtable.h` | HDMI 렌더 형식과 핀 표 |
| `bioatmos/basetopo.cpp`, `basetopo.h` | 토폴로지 미니포트 공용 부분 |
| `bioatmos/hdmitopo.cpp`, `hdmitopo.h`, `hdmitoptable.h` | HDMI 잭 속성과 토폴로지 표 |
| `bioatmos/kshelper.cpp`, `kshelper.h` | KS 형식·속성 보조 함수 |
| `bioatmos/NewDelete.cpp`, `NewDelete.h` | 커널 메모리 할당·해제 |
| `bioatmos/control.cpp`, `control.h` | 보안 제어 장치, IOCTL, 취소 안전 READ 큐, 링과 트림 |
| `core/bio_core.h`, `core/*.cpp` | 커널 API 없는 링, READ 조건, DMA 정수 클럭과 형식 판정 |
| `bioatmos/bioatmos.inx` | DCH 설치 입력; 빌드에서 StampInf로 `.inf` 생성 |
| `bioatmos/bioatmos.vcxproj`, `bioatmos.sln` | x64/ARM64 WDM 빌드 |
| `packages.config`, `Directory.Build.props` | WDK/SDK NuGet 버전과 MSBuild 연결 |
| `../tools/build-driver.ps1` | NuGet 복원, MSBuild, 결과 수집, InfVerif |
| `../tools/kcheck.py` | Linux의 실제 WDK/SDK 헤더 기반 컴파일 검사와 정적 분석 |

ABI의 유일한 정의는 `crates/bio-abi/src/wire.rs`입니다. 커널은 그 파일에서 생성한 `crates/bio-abi/include/bioatmos_abi.h`를 읽습니다. 생성 헤더나 IOCTL 번호를 드라이버 쪽에서 따로 고치지 않습니다.

## 제어 장치와 기록

제어 장치는 `\Device\BioAtmos`, DOS 링크는 `\DosDevices\BioAtmos`입니다. SYSTEM, 관리자, 대화형 사용자가 읽고 쓸 수 있도록 `IoCreateDeviceSecure`에 `D:P(A;;GA;;;SY)(A;;GRGW;;;BA)(A;;GRGW;;;IU)`를 전달합니다. 동시에 열린 제어 핸들은 하나만 허용합니다.

- `QUERY_INFO`: ABI/드라이버 판, 링 크기, 컴파일된 형식, 기능, QPC 주파수와 엔드포인트별 마스크를 돌려줍니다.
- `READ`: `METHOD_OUT_DIRECT`로 온 출력 MDL에 온전한 기록만 옮깁니다. 최소 바이트, 긴급 기록(형식 변경·정지), 기한 중 하나가 충족되면 완료합니다. 보류 READ는 하나만 허용합니다. 큐에서 꺼내 완료하는 중인 READ도 소유권을 유지해, 다른 READ가 그 기록을 먼저 가져가지 못하게 합니다. 첫 기록보다 작은 출력 버퍼는 `STATUS_BUFFER_TOO_SMALL`입니다. 한 번에 옮기는 양은 `BIO_READ_MAX_BYTES`(256 KiB)까지이고, 최소 바이트도 이 값으로 자릅니다. 데이터 기록 하나의 페이로드는 `BIO_MAX_DATA_PAYLOAD`(64 KiB)까지라 그보다 긴 구간은 여러 기록으로 나뉘므로, `record_size(BIO_MAX_DATA_PAYLOAD)` 이상인 버퍼의 READ는 항상 진행합니다.
- `SET_RATE_TRIM`: ppm × 65536 정수로 받은 트림을 ±2000 ppm으로 자릅니다. 두 엔드포인트의 DMA 클럭에 같은 값을 적용합니다.
- `GET_STATS`: 엔드포인트별 바이트, 넘침, 유실과 현재 에포크·상태를 돌려줍니다.

링 저장소는 비페이지 메모리 8 MiB이며 시작 때 한 번 할당합니다. 데이터 기록의 `qpc`는 해당 갱신에서 가상 DMA가 기록의 끝을 지난 QPC입니다. DMA 버퍼가 감기면 두 메모리 구간을 `RecordRing::Data`에 넘깁니다. 갱신이 한 바퀴 넘게 늦어 이미 덮인 구간은 반복해서 복사하지 않습니다. `RecordRing::SourceGap`으로 유실을 먼저 알리고 남아 있는 최신 한 버퍼만 기록합니다. 이때 위치·유실 통계와 넘침/형식 반복/불연속 기록을 코어가 함께 처리합니다. 쓰기 패킷 기능이 꺼져 있으면 `qpc_written`은 0입니다. 켜진 경우 `SetWritePacket`은 패킷별 작성 QPC를 저장하고, 실제 바이트는 명목 DMA가 지난 시점에 기록합니다. ADR 0014의 후보인 쓰기 직후 조기 복사는 하지 않습니다. 형식 변경, RUN 진입, RUN 이탈은 각각 독립 기록입니다.

CLEANUP은 해당 핸들의 보류 READ를 취소하고 트림을 0으로 되돌린 다음 읽지 않은 기록을 버립니다. 성공한 CREATE도 링이 시작된 상태라면 대기 기록을 버려 새 핸들이 이전 세션의 데이터를 읽지 않게 합니다. 코어는 버린 뒤 다음 데이터 앞에 현재 형식을 `FORMAT_REPEAT`로 다시 싣고 데이터를 `DISCONTINUITY`로 표시합니다. 세션 경계에서 버린 바이트는 유실 통계에 넣지 않습니다. CLOSE와 CLEANUP은 별개의 경로입니다.

## IRQL과 잠금 규칙

- 장치 생성, 레지스트리 읽기, 미니포트 준비와 메모리 준비는 PASSIVE_LEVEL에서 합니다.
- DMA 갱신과 타이머 콜백은 DISPATCH_LEVEL까지 실행됩니다. 이 경로는 비페이지 코드·메모리만 사용하며, 파일 I/O와 대기 가능한 잠금, 동적 할당을 하지 않습니다.
- 링, 트림, 열린 핸들과 보류 READ의 공유 상태는 제어 상태의 스핀 락 하나로 직렬화합니다. 코어는 잠금을 소유하지 않으므로 `RecordRing` 호출자는 이 락을 잡아야 합니다. 어댑터에는 스트림 목록과 DMA 상태의 수명을 지키는 별도 스핀 락이 있습니다. 둘 다 필요할 때 순서는 반드시 **스트림 락 → 제어 락**이며 역순으로 잡지 않습니다.
- `IoCsq`의 획득·해제 콜백도 같은 락을 사용합니다. 이미 잡은 상태에서 `IoCsqInsertIrpEx`나 `IoCsqRemoveNextIrp`를 불러 재귀 획득하지 않습니다.
- MDL은 `MmGetSystemAddressForMdlSafe`와 `NormalPagePriority | MdlMappingNoExecute`로 매핑하고 실패를 처리합니다. `IoCompleteRequest`는 항상 스핀 락 밖에서 합니다.
- 두 엔드포인트는 1 ms 주기의 고해상도 DMA 타이머 하나와 QPC를 공유합니다. 타이머는 첫 스트림이 RUN에 들어갈 때 켜고 마지막 스트림이 RUN을 떠날 때 끕니다. 아무것도 재생하지 않는 동안에는 돌지 않습니다.
- 보류 READ의 기한은 별도의 고해상도 타이머가 그 기한에 한 번 울려 확인합니다(한 번에 최대 1분, 남으면 다시 겁니다). 데이터를 기다리는 동안 주기적으로 깨어나지 않고, 기록 생산 직후에 완료 조건을 확인합니다.
- 링 수명은 별도 `EX_RUNDOWN_REF` 대신 **제어 락 + 시작 상태 + READ 세대 번호**로 보호합니다. 모든 링 접근과 MDL로의 `Read` 복사는 제어 락 안에서 이루어지고, 같은 락 아래의 `Started` 검사로 유효 저장소가 있는지 확인합니다. `Init`도 같은 락 안에서 어댑터 START 때만 하며, 스트림 RUN은 링을 초기화하지 않습니다.
- STOP/제거는 제어 락 아래에서 `Started=false`로 바꾸고 저장소 포인터를 분리합니다. 락을 놓은 뒤에만 비페이지 저장소를 해제합니다. DMA 타이머는 어댑터 중지에서, READ 타이머는 제어 장치 중지에서 `ExDeleteTimer(..., TRUE, TRUE, ...)`로 취소·콜백 대기를 끝내며, 둘 다 링과 장치 확장이 사라지기 전에 종료됩니다. 대기는 PASSIVE_LEVEL 정리 경로에서만 합니다(API 자체의 상한은 APC_LEVEL입니다).
- 성공한 링 초기화마다 세대 번호가 증가합니다. READ는 최초 검사 때 세대를 저장하고 CSQ 삽입과 큐에서 꺼낸 뒤 복사 전에 다시 비교합니다. 이전 START의 READ가 재시작한 링을 소비하지 못하게 하며, 분리되어 완료 중인 READ도 완료 전까지 다른 READ를 막습니다. 이전 어댑터의 스트림은 `active=false`와 스트림 락으로 추가 기록을 차단합니다.
- REMOVE는 추가로 제거 락의 IRP 참조가 모두 반환될 때까지 기다립니다. 열린 파일 객체와 어댑터 참조는 장치 확장 수명을 유지하므로, 제거 후 들어오는 CLEANUP/CLOSE도 확장을 유효하게 사용할 수 있습니다.

컴파일 검사만으로 취소·제거 경합의 안전성을 입증할 수는 없습니다. 아래 실제 장치 검증을 생략하지 않습니다. 특히 READ의 링 복사는 스핀 락 아래에서 실행되므로(한 번에 256 KiB까지) 최악의 락 유지 시간과 DPC 지연을 측정해야 합니다. 엔드포인트당 렌더 스트림은 하나만 허용합니다. 한 엔드포인트의 형식·에포크·기록 위치가 단일 스트림에 대응한다는 코어 계약에 맞춘 제한입니다.

## 레지스트리

위치는 서비스 키 `HKLM\SYSTEM\CurrentControlSet\Services\bioatmos\Parameters`입니다. INF는 서비스 설치 섹션의 `HKR` 상대 경로만 씁니다. AddDevice와 매 START 때 값을 읽으며, 어댑터 인스턴스가 설정 사본을 가집니다. 변경은 장치를 다시 시작한 뒤 반영됩니다.

| 이름 | 형식 | 기본값 | 뜻 |
| --- | --- | --- | --- |
| `SecondEndpoint` | DWORD | `1` | 0이면 둘째 HDMI 미니페어를 등록하지 않음 |
| `FormatMask0` | QWORD | `0x6f` | 주 엔드포인트의 허용 형식 |
| `FormatMask1` | QWORD | `0x6f` | 둘째 엔드포인트의 허용 형식 |
| `WritePacket` | DWORD | `0` | 1일 때 `IMiniportWaveRTOutputStream`과 기능 비트를 노출 |

기본 마스크는 PCM, AC-3, E-AC-3, E-AC-3 Atmos, MAT 2.0, MAT 2.1입니다. TrueHD와 DTS 계열은 기본으로 꺼져 있습니다. SysVAD의 기존 HDMI 구체 형식 14개를 보존하고 E-AC-3, E-AC-3 Atmos, MAT 2.1의 `0x070c` 하위 형식을 더해 17개를 컴파일합니다. DTS:X E1/E2는 `BIO_FORMAT_DTS_HD` 마스크로 묶습니다. 스트리밍 데이터 범위는 이 구체 형식의 채널 수·레이트를 따라 엔드포인트별로 만듭니다. 비트 정의는 생성 ABI 헤더의 `BIO_FORMAT_*`를 따릅니다. INF의 QWORD 플래그는 `0x000B0001`입니다. 값이 없는 경우에도 드라이버가 같은 기본값을 사용합니다. 레지스트리 마스크에 컴파일되지 않은 비트가 있으면 버리고, 둘째 엔드포인트가 꺼져 있으면 `enabled_formats[1]`은 0입니다.

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

이는 링크나 MSVC/WDK 실제 빌드가 아닙니다. Windows CI의 x64·ARM64 Debug·Release 빌드, `/W4 /WX`, 코드 분석, InfVerif, CodeQL, 서명 검증은 별도 게이트입니다. 시험서명 PC에서는 Driver Verifier, HVCI 로드, IOCTL 퍼징, READ 취소·정리·제거 경합, 형식 협상, 두 엔드포인트의 공간음향 활성화, 쓰기 패킷/EOS와 트림을 검증해야 합니다.

### 커널에 올린 뒤의 필수 경계 시험

- READ: 빈 출력, 첫 기록보다 1바이트 작은 출력, 최소 바이트가 출력보다 큼, 기한 0, 무음 상태의 기한 만료, 두 번째 보류 요청, 이미 취소된 요청, 생산과 취소의 경합.
- 수명: 중복 CREATE, 보류 중 CLEANUP/CLOSE, 핸들을 연 채 STOP/제거, 스트림 RUN 중 surprise removal, START 실패 후 제거, 장치 중지 후 다시 시작.
- 형식: 각 마스크 비트를 따로 켜고 열거·NewStream·SetFormat 결과 일치 확인, 0 마스크, 두 엔드포인트의 서로 다른 마스크, 불완전한 WAVEFORMATEX와 큰 cbSize 거절.
- 시간: DMA 버퍼 감김, 한 바퀴 넘는 갱신 지연과 유실 기록, ±2000 ppm 경계 및 범위 밖 트림, 두 엔드포인트의 서로 다른 샘플 레이트, 전원 복귀.
- 패킷: WritePacket 기본 꺼짐, 켠 뒤 순서가 늦거나 빠른 패킷, 32비트 패킷 번호 감김, 0바이트 EOS와 부분 EOS, EOS 이후 추가 쓰기, 패킷별 `qpc_written` 대응.

## API 근거

- [PcAddAdapterDevice와 어댑터 전용 확장 슬롯](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/portcls/nf-portcls-pcaddadapterdevice)
- [IoCreateDeviceSecure와 고유 클래스 GUID](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdmsec/nf-wdmsec-wdmlibiocreatedevicesecure)
- [IoCsqInsertIrpEx의 삽입·취소 소유권](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdm/nf-wdm-iocsqinsertirpex)
- [ExDeleteTimer의 콜백 대기와 IRQL](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdm/nf-wdm-exdeletetimer)
- [SetWritePacket의 패킷·EOS 계약](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/portcls/nf-portcls-iminiportwavertoutputstream-setwritepacket)
- [AllocateBufferWithNotification](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/portcls/nf-portcls-iminiportwavertstreamnotification-allocatebufferwithnotification)
- [INF AddReg의 HKR 문맥과 QWORD 플래그](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/inf-addreg-directive)
