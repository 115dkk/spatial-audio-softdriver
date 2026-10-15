// Every sentence and label the screen shows, in one place, so another
// language can be added as a second catalogue with the same keys. Sentences
// from Rust (UserError, notes) are shown as they come and are not here.
//
// Names follow CLAUDE.md: the product is 공간 소프트리시버, and endpoints are
// told apart by what they are for, never by number.

export const text = {
  product: '공간 소프트리시버',
  productLatin: 'Spatial SoftReceiver',

  nav: {
    label: '화면 이동',
    status: '상태',
    profile: '프로필',
    settings: '설정',
  },

  banner: {
    retry: '다시 시도',
    recheck: '다시 확인',
    rebootHint: 'Windows를 다시 시작한 뒤 다시 확인하십시오.',
    chooseOutput: '출력 고르기',
    dismiss: '닫기',
    errorLabel: '오류',
    notesLabel: '알림',
  },

  /** Sentences for failures on the screen's side of the bridge. */
  bridge: {
    unreachable: (detail: string) => `앱과 연결하지 못했습니다: ${detail}`,
    badSnapshot: (detail: string) => `앱이 보낸 상태를 화면이 읽지 못했습니다. 앱을 다시 시작해 보십시오. (${detail})`,
  },

  wizard: {
    heading: '처음 설정',
    stepsLabel: '설정 단계',
    steps: ['환영', '확인', '드라이버', '출력'] as const,
    stepDone: '마침',
    stepCurrent: '지금 단계',

    welcomeTitle: '공간 소프트리시버를 설정합니다',
    welcomeBody: [
      '공간 소프트리시버는 AV 리시버가 하던 비트스트림 디코드와 공간 렌더를 이 PC에서 소프트웨어로 처리해, 게임과 음악의 공간음향을 헤드폰이나 스피커로 냅니다.',
      '소리를 받으려면 가상 오디오 장치 드라이버를 설치해야 하고, 설치할 때 Windows가 관리자 권한을 한 번 묻습니다.',
      '지금 드라이버는 시험서명 묶음이라 Windows의 시험서명 모드가 켜져 있어야 합니다.',
    ],
    begin: '시작',

    unsupportedTitle: '이 PC에서는 쓸 수 없습니다',
    unsupportedBody: '이 PC에는 공간 소프트리시버를 설치할 수 없습니다. 아래는 이 PC에서 읽은 값입니다.',

    testSigningTitle: '시험서명 모드를 켜야 합니다',
    testSigningBody:
      '시험서명 드라이버는 Windows의 시험서명 모드에서만 설치됩니다. 모드를 켜고 Windows를 다시 시작한 뒤 이 앱을 다시 열어 확인하십시오.',

    driverTitle: '드라이버를 설치합니다',
    driverBody:
      '게임과 음악 앱이 소리를 보낼 가상 오디오 장치 셋을 만듭니다. 설치가 끝나면 Windows 소리 설정에 아래 이름으로 나타납니다.',
    driverInstall: '드라이버 설치',
    driverReinstall: '다시 설치',
    driverInstalling: '설치하는 중',
    driverAdminHint: '누르면 Windows가 관리자 권한을 묻는 창을 띄웁니다. 승인해야 설치가 진행됩니다.',

    rebootTitle: 'Windows를 다시 시작해야 합니다',
    rebootBody:
      '열려 있는 작업을 저장한 뒤 Windows를 다시 시작하십시오. 다시 시작한 뒤 이 앱을 열어 설정을 이어 가십시오. 이미 다시 시작했다면 다시 확인을 누르십시오.',

    outputTitle: '소리를 낼 장치를 고르십시오',
    outputBody:
      '공간 소프트리시버가 처리한 소리를 이 장치로 냅니다. 헤드폰을 꽂은 장치, 스피커, AV 리시버 가운데 하나를 고르십시오.',
    outputsLabel: '출력 장치',
    useOutput: '이 출력 쓰기',
    noOutputs: '쓸 수 있는 출력 장치가 없습니다. 장치를 꽂고 다시 확인하십시오.',

    quitTitle: '공간 소프트리시버를 끝냈습니다',
    quitBody: '엔진을 멈췄고, 기본 장치를 넘겨받았다면 원래 장치로 되돌렸습니다.',
  },

  endpoints: [
    { name: '공간 소프트리시버', purpose: '게임과 일반 앱' },
    { name: '공간 소프트리시버 음악', purpose: '음악 앱' },
    { name: '공간 소프트리시버 PASS', purpose: '비트스트림 통과' },
  ] as const,

  status: {
    title: '상태',
    powerOn: '켜기',
    powerOff: '끄기',
    powerTurningOn: '켜는 중',
    powerTurningOff: '끄는 중',
    stateLabel: '상태',
    off: '꺼짐',
    on: '켜짐',
    stopped: '멈춤',
    reconnecting: '다시 연결하는 중',
    output: '출력',
    noOutput: '고른 출력이 없습니다',
    change: '바꾸기',
    choose: '고르기',
    closeOutputs: '닫기',
    currentOutput: '지금 쓰는 출력',
    rolesTaken: 'Windows 기본 장치가 공간 소프트리시버를 가리킵니다.',
    entrances: '입구',
    meters: '측정값',

    noInput: '입력 없음',
    engineOff: '꺼짐',
    epoch: '에포크',
    epochHint: '형식이 바뀔 때마다 1씩 오르는 번호',
    volume: '볼륨',
    muted: '음소거',
    chainErrors: '체인 오류',
    chainErrorsNone: '없음',
  },

  /** Why an entrance's format is not played, as a sentence. */
  unsupported: {
    format: '지원하지 않는 형식입니다.',
    source_rate: '48 kHz로 바꿀 수 없는 표본율입니다.',
    direct_layout: '이 채널 수에는 직접 배치가 없습니다.',
  },

  chainErrorKinds: {
    unsupported_codec: '지원하지 않는 코덱',
    unsupported_format: '지원하지 않는 형식',
    corrupt_payload: '깨진 데이터',
    missing_format: '형식을 모르는 데이터',
    wrong_route: '경로가 맞지 않는 데이터',
    unsupported_scene_rate: '바꿀 수 없는 표본율',
    truncated_record: '잘린 기록',
    bad_record_length: '길이가 틀린 기록',
    bad_format_record: '잘못된 형식 기록',
    bad_volume_record: '잘못된 볼륨 기록',
    missing_direct_layout: '직접 배치 없음',
    decoder_behind: '디코더가 밀려 버린 버스트',
  } as Record<string, string>,

  meters: {
    latencyP50: '지연 p50',
    latencyP99: '지연 p99',
    ringLevel: '교차 링 수위',
    underruns: '끊김',
    outputRestarts: '출력 재시작',
    outputMode: '출력 모드',
    output: '출력 형식',
    clock: '클럭',
    decodeRefused: '디코더 지연 거부',
    lostBytes: '손실 바이트',
    exclusive: '독점',
    shared: '공유',
    fellBack: (mix: number) => (mix > 0 ? `공유로 대체됨, 믹스 ${mix}채널` : '공유로 대체됨'),
    trim: '트림',
    asrc: 'ASRC',
    ms: 'ms',
    times: '회',
    bytes: '바이트',
    ppm: 'ppm',
  },

  format: {
    pcm: 'PCM',
    direct: '직접 출력',
    channels: (n: number) => `${n}채널`,
    container: '전송',
    otherCodec: (code: number) => `알 수 없는 코덱 0x${code.toString(16).padStart(8, '0')}`,
  },

  codecs: {
    ac3: 'AC-3',
    eac3: 'DD+',
    eac3_joc: 'DD+ JOC',
    true_hd: 'TrueHD',
    mat20: 'MAT 2.0',
    mat21: 'MAT 2.1',
    dts: 'DTS',
    dts_hd: 'DTS-HD',
  },

  profile: {
    title: '프로필',
    defaultNote: 'BRIR 세트를 가져오기 전에는 스테레오 스피커 패닝으로 냅니다.',
    brirTitle: 'BRIR 세트',
    brirBody: '측정한 BRIR이 든 폴더를 고르면 헤드폰 출력이 그 세트로 소리를 배치합니다.',
    brirImport: 'BRIR 폴더 가져오기',
    firTitle: '헤드폰 FIR',
    firBody: '헤드폰의 주파수 특성을 고르게 펴는 FIR 파일(WAV)을 고릅니다.',
    firImport: 'FIR 파일 가져오기',
    firFilter: 'WAV 파일',
    gainTitle: '게인',
    gainBody: '출력 전체의 게인입니다. 슬라이더를 놓으면 적용합니다.',
    gainLabel: '게인',
    gainUnit: 'dB',
    testTitle: '방향별 시험음',
    testBody: '한 방향에서만 시험음을 내어 소리가 제자리에서 들리는지 확인합니다.',
    testLabel: '시험음을 낼 방향',
    direction: (id: number) => `방향 ${id}`,
    mapTitle: '객체 지도',
    mapBody: '객체 지도는 다음 조각에서 채웁니다. 실행 중에 소리 객체가 어디에 놓였는지 위에서 내려다본 그림으로 보여 줄 자리입니다.',
    pickerUnavailable: '파일 선택 창은 앱 안에서만 열립니다.',
  },

  gallery: {
    title: '고정물',
  },

  settings: {
    title: '설정',
    startup: '시작',
    autostart: '로그인할 때 시작',
    autostartHint: 'Windows에 로그인하면 공간 소프트리시버를 함께 엽니다.',
    autostartUnavailable: '앱 안에서만 바꿀 수 있습니다.',
    diagnostics: '진단',
    record: '진단 녹화 시작',
    recordHint: '드라이버가 보낸 기록을 파일로 남깁니다. 문제를 알릴 때 이 파일을 첨부하십시오.',
    platform: '이 PC',
    driver: '드라이버',
    unknown: '아직 확인하지 않음',
  },

  facts: {
    osBuild: 'Windows 빌드',
    arch: '아키텍처',
    archNames: { x64: 'x64', arm64: 'ARM64', other: '기타' },
    testSigning: '시험서명 모드',
    secureBoot: 'Secure Boot',
    secureBootNames: { enabled: '켜짐', disabled: '꺼짐', unsupported: '없음(레거시 BIOS)' },
    on: '켜짐',
    off: '꺼짐',
    driverState: '상태',
    driverNotInstalled: '설치 안 됨',
    driverInstalled: '설치됨',
    driverRebootRequired: '다시 시작해야 설치가 끝남',
    problemCode: '문제 코드',
    problemNone: '없음',
    service: '서비스',
    serviceRunning: '돌고 있음',
    serviceStopped: '멈춤',
    exclusivePolicy: '독점 정책',
    policyNames: { applied: '적용됨', missing: '빠짐', unknown: '아직 모름' },
  },
} as const;
