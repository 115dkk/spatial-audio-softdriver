# 0034. 앱에서 헤드폰(BRIR)과 스피커(SECS 2R 보정) 가운데 하나를 고르고, 헤드폰 FIR은 없앤다

상태: 채택, 2026-10-16. ADR 0016의 헤드폰 FIR 결정을 거둡니다. ADR 0030의 출력 종류를 앱에서 고르게 합니다. ADR 0013의 프로필 의도를 고칩니다.

## 배경

M4-f에서 프로필 화면에 "헤드폰 FIR" 가져오기를 두었습니다. 근거는 ADR 0016의 결정, 곧 헤드폰 FIR 하나를 모든 BRIR에 미리 접는다는 것이었습니다. 이 결정은 두 가지가 틀렸습니다.

- **헤드폰 보정은 BRIR의 몫입니다.** 헤드폰 보정은 BRIR을 만들 때(Impulcifer) 이미 들어갑니다. 앱이 따로 받으면 두 번 보정하거나 다른 헤드폰의 보정을 얹게 됩니다. 구축기의 별도 단계를 옮겨 온 것이 원인이었습니다.
- **FIR이 필요한 자리는 실제 스피커의 룸 보정입니다.** 이 자리에 쓸 결과물을 만드는 도구가 [SECS 2R](https://github.com/115dkk/Sound-Equalization-and-Correction-System-2R)(업스트림 SECS 2의 포크, GPL-3.0)입니다.
  - SECS 2R이 내보내는 것은 CamillaDSP v4 설정 파일(YAML) 하나와 그 설정이 가리키는 FIR WAV들입니다.
  - 그 설정은 하나의 신호 사슬입니다. 입력마다 PEQ와 FIR, 레벨과 지연, 크로스오버가 들어 있습니다. 메인을 서브우퍼 버스로 나누는 믹서, 서브우퍼마다의 게인과 극성과 올패스와 PEQ, LFE 처리, 스피커 사이의 MIMO 경로도 들어 있습니다.
  - 그래서 '스피커마다 FIR 하나'로는 이 보정을 재현할 수 없습니다. 이 사슬은 BRIR과 섞일 자리도 없습니다.

또 하나의 문제는, 지금까지 스피커 출력을 `profile.toml`로만 쓸 수 있었다는 점입니다. 앱에서 고를 수 있는 스피커는 BRIR이 없을 때의 기본 스테레오뿐이었습니다.

## 결정

### 헤드폰 FIR을 없앱니다

- **없애는 것**
  - 계약의 `ProfileView.headphone_fir`과 `Intent::ImportHeadphoneFir`
  - 감독자의 가져오기와 그 오류 문장
  - 화면의 패널
  - `brir::Profile.headphone_fir`과 `brir::check_headphone_fir`
- **`profile.toml`의 `[brir] headphone_fir`**: 조용히 무시하지 않고 거절합니다. 거절 문장에는 까닭과 대안을 적습니다.
- **앱 설정에 남은 `headphone_fir`**: 읽을 때 버립니다.

### 출력 종류는 둘이고, 한 번에 하나만 씁니다 (`ProfileKind`)

프로필은 두 종류의 설정을 따로 가집니다. 한쪽을 바꿔도 다른 쪽은 남습니다.

- **헤드폰:** BRIR 세트를 씁니다(지금과 같음). BRIR이 없으면 헤드폰으로 바꿀 수 없습니다(`ImportBrirFirst`).
- **스피커:** 아래 표의 배치 하나를 씁니다. SECS 2R 보정을 가져오면 그 보정이 스피커를 정합니다.
- **게인:** 종류마다 따로입니다. 헤드폰에 맞춘 +10 dB가 스피커로 넘어가지 않게 하기 위함입니다. `SetGain`은 쓰고 있는 종류의 게인을 바꿉니다.
- **실시간 교체:**
  - 다음 경우는 실시간 교체(`LiveChange::Prepared`)로 적용합니다. 채널 수가 바뀌면 출력 장치를 다시 엽니다(ADR 0030).
    - 종류를 바꿀 때
    - 쓰고 있는 종류의 BRIR, 배치, 보정을 바꿀 때
  - 쓰지 않는 종류의 설정은 검사한 뒤 저장만 합니다.
  - BRIR을 가져와도, 보정을 가져와도 종류는 저절로 바뀌지 않습니다.
- **기본값:** FIR이 없는 스피커 2.0입니다. 지금까지의 기본 스테레오(±30°, 장치의 처음 두 채널)와 같은 소리입니다.

### 앱의 스피커 배치 (보정이 없을 때)

Windows 채널 마스크가 있는 표준 배치 아홉 가지입니다(`ssr_types::app::SpeakerLayout`).

- **채널:** 장치 채널 n은 마스크의 n번째 비트입니다.
- **이름과 각도:** 이름과 공칭 각도는 ADR 0029의 `Speaker` 표를 따릅니다.
- **스피커 설정:** 모두 전대역입니다. LFE는 서브우퍼로 보냅니다(+10 dB). 정렬은 하지 않습니다.
- **그 밖의 설정:** 크로스오버, 거리, 레벨, 실제 각도, 다른 배치는 `profile.toml`(ADR 0030)로 정합니다.

| 배치 | 마스크 | 장치 채널 순서 |
| --- | --- | --- |
| 2.0 | `0x3` | L R |
| 2.1 | `0xb` | L R LFE |
| 4.0 | `0x33` | L R Ls Rs |
| 5.1 | `0x3f` | L R C LFE Ls Rs |
| 7.1 | `0x63f` | L R C LFE Lrs Rrs Lss Rss |
| 5.1.2 | `0x503f` | L R C LFE Ls Rs Tfl Tfr |
| 5.1.4 | `0x2d03f` | L R C LFE Ls Rs Tfl Tfr Trl Trr |
| 7.1.2 | `0x563f` | L R C LFE Lrs Rrs Lss Rss Tfl Tfr |
| 7.1.4 | `0x2d63f` | L R C LFE Lrs Rrs Lss Rss Tfl Tfr Trl Trr |

- **마스크와 순서:** `ssr_types::channel_layout_preset`과 같습니다.
- **옆 쌍이 없는 배치:** 서라운드는 뒤 비트(`0x30`)에 있지만 이름은 `Ls/Rs`(110°)입니다. 5.1 방의 공칭 각도를 따르기 위함입니다. `Ls/Rs`에는 Windows 비트가 없으므로 마스크 순서 검사는 이 둘을 건너뜁니다.

### SECS 2R 보정 (`ImportCorrection`, `ClearCorrection`)

스피커 종류는 SECS 2나 SECS 2R이 내보낸 CamillaDSP v4 설정 하나를 보정으로 가져올 수 있습니다. 보정을 가져오면 그 설정이 모든 것을 정합니다.
- 렌더 방향(입력)
- 장치 채널(출력)
- 그 사이의 처리

저장된 배치는 보정을 지우면 다시 쓰입니다.

- **읽는 범위.** 아래 범위가 SECS 2R의 내보내기가 쓰는 것 전부입니다. 이 밖의 것이 나오면 그 항목의 이름을 적어 거절합니다.
  - `devices`: `samplerate`, `capture.channels`, `playback.channels`만 읽고 나머지 장치 설정은 무시합니다.
  - `filters`:
    - `Biquad`: `Peaking`(freq, q, gain), `Allpass`(freq, q)
    - `BiquadCombo`: `LinkwitzRileyHighpass`/`Lowpass`(차수 2, 4, 8), `ButterworthHighpass`/`Lowpass`(차수 1~8)
    - `Conv`: `Wav`(filename, channel)
    - `Gain`: gain dB, inverted, mute, scale
    - `Delay`: `samples`와 `ms`. `subsample`은 false일 때만 받습니다.
  - `mixers`: `channels.in`/`out`, `mapping`의 `dest`와 `sources[channel, gain, inverted, mute, scale]`, `mute`
  - `pipeline`: `Mixer`(name)와 `Filter`(channels, names) 단계, 그리고 `bypassed`
  - 상대 경로는 설정 파일의 폴더를 기준으로 읽습니다.
- **입력의 이름.** SECS가 `description`에 적는 `inputs 0 L, 1 R, …` 목록이 입력의 이름입니다. 없으면 머리 주석 `# inputs N: L, R, …`을 씁니다. 둘 다 없으면 SECS 내보내기가 아니므로 거절합니다.
- **`route_in`은 건너뜁니다.** 파이프라인의 첫 단계가 믹서 `route_in`이면 렌더러가 SECS 입력을 이름으로 직접 채웁니다. `route_in`은 재생기의 채널 순서를 SECS 입력에 맞추는 단계인데, 렌더러는 그 순서를 거치지 않기 때문입니다.
- **`route_out`은 그대로 돌립니다.** 출력을 장치 채널에 꽂는 일이기 때문입니다. 장치 채널 수는 `playback.channels`입니다. 화면에 보이는 채널 이름은 `description`의 `outputs …` 목록이 `route_out`을 거쳐 닿는 장치 채널입니다.
- **렌더 방향.** LFE가 아닌 입력이 렌더 방향입니다. SECS의 역할 이름은 그대로 두고, 공칭 각도만 아래와 같이 붙입니다(방위는 왼쪽이 양수). 모르는 이름이면 거절합니다.

  | 역할 | 각도 |
  | --- | --- |
  | L, R | ±30° |
  | C | 0° |
  | Ls, Rs | ±90° (Lb/Rb가 있을 때), ±110° (없을 때) |
  | Lb, Rb | ±150° |
  | Lw, Rw | ±60° |
  | Tfl, Tfr | ±45°, 높이 45° |
  | Tml, Tmr | ±90°, 높이 45° |
  | Tbl, Tbr | ±135°, 높이 45° |
  | Hfl, Hfr | ±30°, 높이 30° |
  | Hbl, Hbr | ±150°, 높이 30° |

- **LFE.**
  - `LFE` 입력이 있으면 LFE 베드를 그대로(0 dB) 넣습니다. LFE 이득과 저역 통과는 SECS 사슬이 겁니다.
  - `LFE` 입력이 없으면 LFE를 +10 dB로 L과 R 입력에 나눠 넣습니다. 나누는 방법은 서브우퍼가 없을 때의 `bass_to` 규칙과 같습니다. L과 R이 없으면 모든 입력에 나눕니다.
- **표본율.** 처리는 믹스 표본율 48 kHz에서 합니다. 설정의 표본율이 다르면 다음과 같이 맞춥니다.
  - FIR WAV는 이득을 맞춰 리샘플합니다(ADR 0030).
  - 바이쿼드는 48 kHz에서 그 매개변수로 계산합니다.
  - `samples` 단위 지연은 표본율 비로 바꿔 반올림합니다.
- **채널 마스크.** 다음 조건을 둘 다 만족하면 그 배치의 마스크를 씁니다. 그렇지 않으면 마스크 0(직접 출력)을 씁니다.
  - 장치 채널의 이름이 위 배치표의 한 배치와 순서까지 같습니다. 이때 SECS의 `Lb/Rb`는 `Lrs/Rrs`, `Tbl/Tbr`은 `Trl/Trr`, `Ls/Rs`는 옆 쌍이나 서라운드로 맞춥니다.
  - 서브우퍼는 `SW1` 하나뿐이고, 그것이 `LFE` 자리에 있습니다.

  2채널은 언제나 `0x3`입니다.
- **순서.** SECS 사슬, 그다음 종류의 게인, 그다음 모든 채널을 묶은 리미터(ADR 0030)입니다. SECS 사슬이 가진 필터 지연(예: mixed 8 ms)은 출력 지연에 더해집니다. 이 지연은 소리의 일부이므로 측정 지연(ADR 0015, 전달과 버퍼)에는 넣지 않습니다. 그 값은 SECS가 제목에 적어 둔 것이 알려 줍니다.
- **한도.** 손으로 고친 파일이나 잘못된 파일이 실시간 스레드를 멈추거나 메모리를 다 쓰지 못하게, 준비하기 전에 펼친 그래프를 검사하고 넘으면 거절합니다.
  - 크기: 문서 4 MiB, 필터 4096개, 믹서 64개, 파이프라인 단계 4096개
  - 펼친 그래프: 필터 사용 16384회, 믹서 소스 65536개, 바이쿼드 단 16384개, 합성곱 사용 512회
  - 메모리: FIR 표본과 지연 표본의 합은 32채널 × 4초(지연은 2초)까지이고, 지연 하나는 2초까지입니다.
  - 값: 게인은 −150~+40 dB(선형 |g| ≤ 100), Peaking 게인은 ±40 dB, Q는 0.001~1000입니다. 계산한 계수와 FIR 표본은 모두 유한해야 합니다.
  - 안전망: 그래도 블록 출력에 비유한 값이 나오면 그 블록을 무음으로 내고 상태를 비웁니다.
  - 꼬리: 출력까지의 꼬리(지연, FIR 길이, 바이쿼드 여유)가 엔진의 스피커 꼬리 한도를 넘는 파일은 거절합니다. 끝낼 때 꼬리가 잘리지 않게 하기 위함입니다.
- **구현과 라이선스.**
  - 새 크레이트 `correction`(MIT)이 이 범위를 읽고 실시간으로 처리합니다. 무할당, 무잠금이고, 상태는 `f64` 바이쿼드입니다.
  - CamillaDSP의 공개 설정 문서, RBJ Audio EQ Cookbook, Linkwitz-Riley와 Butterworth의 정의로만 씁니다. CamillaDSP와 SECS의 소스는 둘 다 GPL-3.0이므로 옮기지 않습니다(ADR 0003).
  - 시험 자료는 시험이 직접 쓰는 SECS 모양의 합성 설정입니다. SECS가 만든 파일을 저장소에 넣지 않습니다.

### 장치가 받는지 먼저 묻습니다

다음 세 경우에 감독자는 고른 출력 장치가 그 배치를 독점 모드로 받는지 `Host::supports_layout`으로 묻습니다.
- 스피커로 바꿀 때
- 쓰고 있는 스피커의 배치나 보정을 바꿀 때
- 스피커로 켤 때

받지 않으면 거절합니다. 배치이면 `LayoutUnsupported`를, 보정이면 `CorrectionUnsupported`를 냅니다. 다음 경우에는 묻지 않습니다.
- 2채널 `0x3`
- 호스트가 답하지 못할 때(`HostError::Unsupported`). 이때는 그대로 진행합니다.

### `profile.toml`이 여전히 우선합니다

파일이 있으면 그 파일이 종류까지 정합니다. 앱의 종류, BRIR, 배치, 보정 의도는 거절합니다(`ProfileFileTakesPrecedence`). 게인은 파일 종류에 해당하는 저장 게인이 덮어씁니다. `profile.toml`의 스피커마다 `fir`(ADR 0030)는 그대로 둡니다.

## 계약 (ADR 0001)

- **`Intent`**
  - 더하는 것: `SetProfileKind(ProfileKind)`, `SetSpeakerLayout(SpeakerLayout)`, `ImportCorrection(PathBuf)`, `ClearCorrection`
  - 그대로 두는 것: `ImportBrir(PathBuf)`
  - 없애는 것: `ImportHeadphoneFir`
- **`ProfileKind`:** `headphones`와 `speakers` 둘입니다. `stereo`는 스피커 2.0이 되었습니다.
- **`ProfileView`**
  - 쓰고 있는 종류의 `kind`, `name`, `gain_db`, `directions`
  - `from_file`
  - `headphones { brir }`
  - `speakers { layout, correction { path, title }, channels[{ name, channel, subwoofer }] }`

  쓰지 않는 종류의 설정도 보이므로 화면이 두 종류를 나란히 보일 수 있습니다.

## 범위 밖

- **종류마다 다른 출력 장치**(헤드폰 DAC와 오디오 인터페이스): 지금은 두 종류가 한 출력 장치를 같이 씁니다. 다음 결정에서 다룹니다.
- **`profile.toml`로 SECS 보정 지정하기**, **명령줄 도구(`ssr-live`, `ssr-replay`)의 보정 옵션**
- **SECS 2R의 다른 내보내기**: REW mdat(측정과 비교용)와 `.secs` 프로젝트는 가져오지 않습니다.
- **앱에서 스피커의 거리, 레벨, 크로스오버, 각도 고치기**
- **헤드폰과 스피커에 동시에 내기**

## 결과

- **화면:** 프로필 화면이 헤드폰과 스피커 두 탭으로 나뉩니다.
  - 어느 탭을 보는지는 화면의 보기 상태입니다. 처음에는 쓰고 있는 종류의 탭을 엽니다.
  - 쓰지 않는 종류의 탭에는 그 종류로 바꾸는 단추가 있습니다.
  - 스피커 탭은 배치와 보정 파일을 고르고, 장치 채널마다 무엇이 나가는지 보입니다.
  - 화면 갤러리는 두 종류와 두 탭을 모두 찍습니다.
- **저장 설정:** 종류마다 나뉩니다. 이전 설정 파일은 이렇게 옮겨 읽습니다.
  - `brir`는 헤드폰 설정이 됩니다.
  - `gain_db`는 그때 쓰던 종류(BRIR이 있으면 헤드폰)의 게인이 됩니다.
  - `headphone_fir`는 버립니다.
- **시험:**
  - 배치표와 마스크 순서
  - 이전 설정 읽기
  - 종류, 배치, 보정 의도의 실시간 교체와 거절
  - 장치 확인
  - `headphone_fir` 키 거절
  - `correction`:
    - 필터마다 해석식과의 응답 비교
    - LR 합의 평탄도
    - 믹서
    - `route_in` 건너뛰기와 `route_out`
    - 합성 SECS 내보내기(MIMO 포함)의 끝에서 끝까지
    - 지원하지 않는 항목의 거절
    - 무할당
