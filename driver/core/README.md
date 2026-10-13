<!-- SPDX-License-Identifier: MS-PL -->
<!-- 사용자 모드에서 검증하는 드라이버 코어의 경계와 빌드 방법. -->
# 드라이버 코어

`bio_core`는 커널 API 없이 고정 메모리 기록 링, READ 완료 판정, 가상 DMA
시계와 형식 판정(코덱 마스크, 그리고 PCM 규칙을 보는 `PcmAllowed`)을
구현합니다. 호출자가 직렬화하며 코어는 할당하지 않습니다. 커널 쪽은 스핀
락, 타이머, IRP와 레지스트리만 감쌉니다.

Linux에서는 다음처럼 사용자 모드 시험을 빌드합니다.

```sh
cmake -S driver/core -B build/core -G Ninja -DBIO_SANITIZE=ON
cmake --build build/core
build/core/core_tests
build/core/write_dump build/core/core.biodump
```

Clang 빌드는 `ring_fuzz`, `clock_fuzz`, `format_fuzz`(형식 판정기)도 만들고,
CI는 각각 `-max_total_time=120`으로 실행합니다.
Windows에서는 MSVC와 같은 CMake 명령을 사용합니다.
