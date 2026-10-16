// A stand-in for the supervisor, for the gallery and `npm run dev` in a plain
// browser. It follows the same rules as crates/supervisor/src/machine.rs for
// the paths the screen can take, starting from a fixture, and emits a
// snapshot after every intent the way the shell does. Its sentences are the
// supervisor's own (crates/supervisor/src/text.rs), in the snapshot's
// language. It decides nothing the real app relies on.

import type { Api, Shell, Unlisten, UpdateState } from './api';
import type {
  Arch,
  DirectionView,
  DriverState,
  EngineStatus,
  Intent,
  Language,
  NextAction,
  OutputChoice,
  Platform,
  ProfileView,
  SceneObject,
  SecureBoot,
  Snapshot,
  UserError,
} from './bindings';
import { fixture, fixtureNames } from './fixtures';
import { text } from './text';

/** What the app shell reads from the OS: login autostart and the updater. */
export interface GalleryShell {
  autostart: boolean;
  update: UpdateState;
}

export interface MockOptions {
  /** Emit 30 Hz meters while running (off for the gallery and tests). */
  meters?: boolean;
  /** How long each intent takes, so the pending states can be seen. */
  latencyMs?: number;
  /**
   * Stand in for the Tauri shell (`native: true`) with these values; without
   * it the shell is the plain browser's, which has no autostart or updater.
   */
  shell?: GalleryShell;
  /** Every intent stays in flight for good, so a pending button can be seen. */
  hold?: boolean;
}

// ---- The supervisor's sentences --------------------------------------------

/** A `supervisor::text::Message`, as `render_ko` and `render_en` write it. */
type Message = Readonly<Record<Language, string>>;

const message = (ko: string, en: string): Message => ({ ko, en });

/** text.rs arch_name. */
const archName = (arch: Arch): Message =>
  arch === 'x64'
    ? message('x64', 'x64')
    : arch === 'arm64'
      ? message('Arm64', 'Arm64')
      : message('다른 아키텍처', 'another architecture');

/** The Messages the mock can meet, by their names in text.rs. */
const say = {
  notAllowed: message('지금은 할 수 없는 동작입니다.', 'That action is not available right now.'),
  testAfterEnable: message('켠 뒤에 시험음을 낼 수 있습니다.', 'Turn it on before playing a test tone.'),
  diagnosticRecordingUnavailable: message(
    '진단 녹음은 다음 조각에서 옵니다.',
    'Diagnostic recording is coming in a later update.',
  ),
  disableBeforeChangingOutput: message(
    '먼저 끈 뒤 출력을 바꾸십시오.',
    'Turn it off before changing the output.',
  ),
  architectureMismatch: (build: Arch, pc: Arch): Message =>
    message(
      `이 빌드는 ${archName(build).ko}용이지만 이 PC는 ${archName(pc).ko}입니다.`,
      `This build is for ${archName(build).en}, but this PC is ${archName(pc).en}.`,
    ),
  testSigningDisabled: (secureBoot: SecureBoot): Message => {
    const on = secureBoot === 'enabled';
    return message(
      '시험서명 모드가 꺼져 있습니다. Secure Boot를 끈 뒤 관리자 명령으로 bcdedit /set testsigning on을 실행하고 다시 시작하십시오.' +
        (on ? ' Secure Boot가 켜져 있어 먼저 꺼야 합니다.' : ''),
      'Test-signing mode is off. Turn off Secure Boot, run bcdedit /set testsigning on as administrator, and restart Windows.' +
        (on ? ' Secure Boot is on and must be turned off first.' : ''),
    );
  },
  driverSignatureRejected: message(
    '드라이버 서명을 받아들이지 않았습니다(문제 코드 52). 시험서명 모드를 확인한 뒤 다시 설치하십시오.',
    'Windows rejected the driver signature (problem code 52). Check test-signing mode, then reinstall the driver.',
  ),
  driverServiceStopped: message(
    '드라이버 서비스가 돌고 있지 않습니다. 복구를 실행하십시오.',
    'The driver service is not running. Run repair.',
  ),
  exclusivePolicyMissing: message(
    '드라이버는 있지만 독점 정책이 적용되지 않았습니다. 복구를 실행하십시오.',
    'The driver is installed, but its exclusive-mode policy is missing. Run repair.',
  ),
  driverProblem: (code: number): Message =>
    message(
      `드라이버가 올바르게 설치되지 않았습니다(문제 코드 ${code}). 복구를 실행하십시오.`,
      `The driver is not installed correctly (problem code ${code}). Run repair.`,
    ),
  chosenOutputMissing: message(
    '고른 출력 장치를 찾을 수 없습니다.',
    'The selected output device could not be found.',
  ),
  outputIdChanged: (name: string): Message =>
    message(
      `출력 장치의 식별자가 바뀌어 이름으로 다시 찾았습니다: ${name}`,
      `The output device identifier changed, so it was found again by name: ${name}`,
    ),
  importBrirFirst: message('먼저 BRIR 세트를 가져오십시오.', 'Import a BRIR set first.'),
  headphoneFirReadFailed: (detail: string): Message =>
    message(`헤드폰 FIR을 읽지 못했습니다: ${detail}`, `The headphone FIR could not be read: ${detail}`),
  gainOutOfRange: message('게인은 -20 dB에서 +10 dB 사이입니다.', 'Gain must be between -20 dB and +10 dB.'),
  directionMissing: message('그 방향이 없습니다.', 'That direction does not exist.'),
  storedOutputMissing: (name: string): Message =>
    message(`저장된 출력 장치를 찾을 수 없습니다: ${name}`, `The saved output device could not be found: ${name}`),
  profileFileTakesPrecedence: message(
    '상태 폴더의 profile.toml이 우선합니다. 그 파일을 지우거나 고치십시오.',
    'profile.toml in the state folder takes precedence. Remove or edit that file.',
  ),
  invalidBrirSource: message(
    'hesuvi.wav, hrir.wav 또는 방향별 WAV 폴더를 고르십시오.',
    'Choose hesuvi.wav, hrir.wav, or a folder of per-direction WAV files.',
  ),
  rebootRequired: message(
    '드라이버 설치를 마치려면 다시 시작해야 합니다.',
    'Restart Windows to finish installing the driver.',
  ),
  osRequirement: (minBuild: number): Message =>
    minBuild === 19041
      ? message('Windows 10 2004(빌드 19041) 이상이 필요합니다.', 'Windows 10 version 2004 (build 19041) or newer is required.')
      : message(`Windows 빌드 ${minBuild} 이상이 필요합니다.`, `Windows build ${minBuild} or newer is required.`),
  defaultProfileName: message('Stereo', 'Default stereo'),
} as const;

/**
 * The supervisor's sentences the fixtures carry (errors and notes), each
 * paired with the same sentence in the other language: the Korean and the
 * English fixture of a state say the same Messages in the same places.
 */
let fixturePairs: Map<string, Message> | undefined;
function fixtureSentence(sentence: string): Message {
  if (fixturePairs === undefined) {
    fixturePairs = new Map();
    const pair = (ko: string | undefined, en: string | undefined) => {
      if (ko === undefined || en === undefined) return;
      const both = message(ko, en);
      fixturePairs?.set(ko, both);
      fixturePairs?.set(en, both);
    };
    for (const name of fixtureNames) {
      const ko = fixture(name, 'ko');
      const en = fixture(name, 'en');
      pair(ko.error?.message, en.error?.message);
      ko.notes.forEach((note, index) => pair(note, en.notes[index]));
    }
  }
  // A sentence no fixture has (a snapshot made by hand) stays as written.
  return fixturePairs.get(sentence) ?? message(sentence, sentence);
}

/** A sentence and the action it offers, kept so a language change re-renders it. */
interface Said {
  message: Message;
  next: NextAction;
}

type Outcome = { kind: 'done' } | ({ kind: 'failed' | 'refused' } & Said);

const done: Outcome = { kind: 'done' };
const failed = (message: Message, next: NextAction): Outcome => ({ kind: 'failed', message, next });
const refused = (message: Message = say.notAllowed): Outcome => ({ kind: 'refused', message, next: 'none' });

// ---- The PC and the shell ---------------------------------------------------

/** machine.rs Config: Windows 10 2004 or newer, an x64 build, test signed. */
const MIN_OS_BUILD = 19041;
const BUILD_ARCH: Arch = 'x64';
const GOOD_PLATFORM: Platform = { os_build: 26100, arch: 'x64', test_signing: true, secure_boot: 'disabled' };
const GOOD_DRIVER: DriverState = {
  kind: 'installed',
  value: { problem_code: 0, service_running: true, exclusive_policy: 'applied' },
};

const pickFolder: Shell['pickFolder'] = async () => 'C:\\Users\\me\\BRIR\\living-room';
// The BRIR picker asks for hesuvi.wav or hrir.wav; the other for an FIR.
const pickFile: Shell['pickFile'] = async (_title, filter) =>
  filter.name === text.profile.brirFilter ? 'C:\\Users\\me\\BRIR\\studio\\hesuvi.wav' : 'C:\\Users\\me\\hd650.wav';

/** A plain browser's shell: no autostart, no updater. */
const browserShell: Shell = {
  native: false,
  pickFolder,
  pickFile,
  autostartEnabled: async () => false,
  setAutostart: async () => {},
  updateState: async () => ({ current: '0.0.0', ready: null }),
  applyUpdate: async () => {},
  onUpdate: async () => () => {},
};

/** The Tauri shell as the gallery stands it in: what it reads from the OS is given. */
function standInShell(given: GalleryShell): Shell {
  let autostart = given.autostart;
  const update = structuredClone(given.update);
  return {
    native: true,
    pickFolder,
    pickFile,
    autostartEnabled: async () => autostart,
    setAutostart: async (enabled) => {
      autostart = enabled;
    },
    updateState: async () => structuredClone(update),
    // Applying would quit the app; the stand-in has nothing to quit.
    applyUpdate: async () => {},
    onUpdate: async () => () => {},
  };
}

/** The supervisor's gain range (machine.rs GAIN_MIN_DB, GAIN_MAX_DB). */
const GAIN_MIN_DB = -20;
const GAIN_MAX_DB = 10;

/** What the supervisor keeps in Settings.profile. */
interface ProfileSettings {
  brir: string | null;
  headphoneFir: string | null;
  gainDb: number | null;
}

const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;

/** A folder of direction WAVs, hesuvi.wav or hrir.wav, as machine.rs brir_source. */
function brirSource(path: string): 'folder' | 'file' | null {
  const name = fileName(path).toLowerCase();
  if (name === 'hesuvi.wav' || name === 'hrir.wav') return 'file';
  // The mock cannot look at the disk: a name without an extension is a folder.
  return name.includes('.') ? null : 'folder';
}

/** The folder's name, or the name of the folder holding the file. */
function brirName(path: string): string {
  const parts = path.split(/[\\/]/).filter((part) => part !== '');
  const named = brirSource(path) === 'file' ? parts[parts.length - 2] : parts[parts.length - 1];
  return named ?? 'BRIR';
}

export function createMockApi(start: Snapshot, options: MockOptions = {}): Api {
  let state: Snapshot = structuredClone(start);
  // The fixtures the mock borrows from are read in the language it starts in,
  // and that language stands for the Windows display language (what
  // `set_language` with null, automatic, follows).
  const systemLanguage = start.language;
  const borrowed = (name: string) => fixture(name, systemLanguage);
  // The PC the mock pretends to look at.
  const host = {
    platform: start.platform ?? GOOD_PLATFORM,
    driver: start.driver ?? ({ kind: 'not_installed' } as DriverState),
    outputs: start.outputs.length > 0 ? start.outputs : borrowed('ready').outputs,
  };
  const engineTemplate: EngineStatus = start.engine ?? (borrowed('running').engine as EngineStatus);
  // The scene the meters move around the listener, from the fixture.
  const sceneTemplate: SceneObject[] = structuredClone(engineTemplate.scene);
  const settings: ProfileSettings = {
    brir: start.profile.from_file ? null : start.profile.brir,
    headphoneFir: start.profile.from_file ? null : start.profile.headphone_fir,
    gainDb: start.profile.gain_db,
  };
  // The kept error and the notes as Messages, so a language change renders
  // them again as the supervisor does (machine.rs re-renders its kept error
  // and its notes). The fixture's own sentences start them.
  let kept: Said | null =
    start.error === null ? null : { message: fixtureSentence(start.error.message), next: start.error.next };
  let notes: Message[] | null = start.notes.length === 0 ? null : start.notes.map(fixtureSentence);
  const snapshotListeners = new Set<(snapshot: Snapshot) => void>();
  const meterListeners = new Set<(engine: EngineStatus) => void>();
  let meterTimer: ReturnType<typeof setInterval> | undefined;
  let tick = 0;

  const render = (said: Said): UserError => ({ message: said.message[state.language], next: said.next });

  /** Sends this moment's snapshot to the listeners a tick later. */
  const emitLater = () => {
    const copy = structuredClone(state);
    setTimeout(() => {
      for (const listener of snapshotListeners) listener(structuredClone(copy));
      syncMeters();
    }, 0);
  };

  function syncMeters() {
    const want = options.meters === true && state.phase.kind === 'running' && meterListeners.size > 0;
    if (want && meterTimer === undefined) {
      meterTimer = setInterval(sendMeters, 1000 / 30);
    } else if (!want && meterTimer !== undefined) {
      clearInterval(meterTimer);
      meterTimer = undefined;
    }
  }

  function sendMeters() {
    if (state.engine === null) return;
    tick += 1;
    const engine = state.engine;
    const wobble = Math.sin(tick / 9);
    engine.latency.p50_ms = 12.4 + 0.3 * wobble;
    engine.latency.p99_ms = 18.9 + 0.8 * Math.sin(tick / 23);
    engine.latency.samples += 1;
    engine.ring_level_ms = 9.6 + 0.4 * Math.cos(tick / 11);
    engine.clock_ppm = -37.5 + 0.6 * Math.sin(tick / 41);
    engine.idle_blocks += 1;
    engine.scene = movingScene(tick);
    const copy = structuredClone(engine);
    for (const listener of meterListeners) listener(copy);
  }

  /**
   * The fixture's objects turning slowly around the listener, each at its
   * own pace and height, with a level that breathes.
   */
  function movingScene(at: number): SceneObject[] {
    return sceneTemplate.map((object, slot) => {
      const seconds = at / 30;
      const turn = ((slot % 2 === 0 ? 1 : -1) * 2 * Math.PI * seconds) / (18 + 7 * slot);
      const cos = Math.cos(turn);
      const sin = Math.sin(turn);
      return {
        ...object,
        x: object.x * cos - object.y * sin,
        y: object.x * sin + object.y * cos,
        level_dbfs: Math.min(0, object.level_dbfs + 6 * Math.sin(seconds * 0.9 + slot)),
      };
    });
  }

  /** The layout an imported set reads as: the running fixture's 7.1.4 directions. */
  function brirDirections(): DirectionView[] {
    return borrowed('running').profile.directions;
  }

  /** What resolve() in machine.rs makes of the settings (no profile.toml). */
  function resolveProfile(next: ProfileSettings): ProfileView {
    if (next.brir === null) {
      return {
        kind: 'stereo',
        name: say.defaultProfileName[state.language],
        brir: null,
        headphone_fir: null,
        gain_db: next.gainDb ?? 0,
        directions: borrowed('ready').profile.directions,
        from_file: false,
      };
    }
    return {
      kind: 'headphones',
      name: brirName(next.brir),
      brir: next.brir,
      headphone_fir: next.headphoneFir,
      gain_db: next.gainDb ?? 0,
      directions: brirDirections(),
      from_file: false,
    };
  }

  /** change_profile: a running engine takes the new layout with it. */
  function changeProfile(next: ProfileSettings): Outcome {
    Object.assign(settings, next);
    state.profile = resolveProfile(settings);
    if (state.engine !== null) state.engine.directions = structuredClone(state.profile.directions);
    return done;
  }

  function importBrir(path: string): Outcome {
    if (state.profile.from_file) return failed(say.profileFileTakesPrecedence, 'none');
    if (brirSource(path) === null) return failed(say.invalidBrirSource, 'none');
    return changeProfile({ ...settings, brir: path });
  }

  function importHeadphoneFir(path: string): Outcome {
    if (state.profile.from_file) return failed(say.profileFileTakesPrecedence, 'none');
    if (settings.brir === null) return failed(say.importBrirFirst, 'none');
    // The detail is the brir crate's; the mock can only name the file.
    if (!fileName(path).toLowerCase().endsWith('.wav')) return failed(say.headphoneFirReadFailed(fileName(path)), 'none');
    return changeProfile({ ...settings, headphoneFir: path });
  }

  function setGain(db: number): Outcome {
    if (!Number.isFinite(db) || db < GAIN_MIN_DB || db > GAIN_MAX_DB) return failed(say.gainOutOfRange, 'none');
    settings.gainDb = db;
    state.profile.gain_db = db;
    return done;
  }

  function setupStep(step: Extract<Snapshot['phase'], { kind: 'setup' }>['value']) {
    state.phase = { kind: 'setup', value: step };
  }

  /** The wizard's checks in order (machine.rs evaluate). */
  function evaluate(): Outcome {
    state.platform = structuredClone(host.platform);
    if (host.platform.os_build < MIN_OS_BUILD) {
      setupStep('unsupported');
      return failed(say.osRequirement(MIN_OS_BUILD), 'none');
    }
    if (host.platform.arch !== BUILD_ARCH) {
      setupStep('unsupported');
      return failed(say.architectureMismatch(BUILD_ARCH, host.platform.arch), 'none');
    }
    if (!host.platform.test_signing) {
      setupStep('not_test_signing');
      return failed(say.testSigningDisabled(host.platform.secure_boot), 'reboot');
    }
    return evaluateDriver();
  }

  /** machine.rs evaluate_driver. */
  function evaluateDriver(): Outcome {
    const driver = host.driver;
    state.driver = structuredClone(driver);
    if (driver.kind === 'not_installed') {
      setupStep('driver');
      return done;
    }
    if (driver.kind === 'reboot_required') {
      setupStep('reboot_required');
      return failed(say.rebootRequired, 'reboot');
    }
    const { problem_code, service_running, exclusive_policy } = driver.value;
    if (problem_code === 52) {
      setupStep('driver');
      return failed(say.driverSignatureRejected, 'retry');
    }
    if (problem_code !== 0) {
      setupStep('driver');
      return failed(say.driverProblem(problem_code), 'retry');
    }
    if (!service_running) {
      setupStep('driver');
      return failed(say.driverServiceStopped, 'retry');
    }
    if (exclusive_policy === 'missing') {
      setupStep('driver');
      return failed(say.exclusivePolicyMissing, 'retry');
    }
    setupStep('choose_output');
    return resumeOutput();
  }

  /**
   * machine.rs find_output: the chosen output by id, else by its name when
   * exactly one output has it (remembering the new id, with a note).
   */
  function findOutput(): OutputChoice | null {
    const chosen = state.chosen_output;
    if (chosen === null) return null;
    const byId = host.outputs.find((output) => output.id === chosen.id);
    if (byId !== undefined) {
      state.chosen_output = { ...byId };
      return byId;
    }
    const named = host.outputs.filter((output) => output.name === chosen.name);
    const [only] = named;
    if (named.length !== 1 || only === undefined) return null;
    state.chosen_output = { ...only };
    notes = [...(notes ?? []), say.outputIdChanged(only.name)];
    return only;
  }

  /** machine.rs resume_output: on to Ready when the saved output is there. */
  function resumeOutput(): Outcome {
    if (state.chosen_output === null) return done;
    if (findOutput() === null) return failed(say.storedOutputMissing(state.chosen_output.name), 'open_settings');
    state.phase = { kind: 'ready' };
    return done;
  }

  function chooseOutput(id: string): Outcome {
    const found = host.outputs.find((output) => output.id === id);
    if (found === undefined) return failed(say.chosenOutputMissing, 'none');
    state.chosen_output = { ...found };
    state.phase = { kind: 'ready' };
    return done;
  }

  /** machine.rs enable: the notes start over; the engine runs the profile's layout. */
  function enable(): Outcome {
    notes = [];
    if (findOutput() === null) {
      setupStep('choose_output');
      return failed(say.storedOutputMissing(state.chosen_output?.name ?? ''), 'open_settings');
    }
    state.engine = structuredClone(engineTemplate);
    state.engine.fault = null;
    state.engine.directions = structuredClone(state.profile.directions);
    state.roles_taken = true;
    state.phase = { kind: 'running' };
    return done;
  }

  function stop() {
    state.engine = null;
    state.roles_taken = false;
  }

  function apply(intent: Intent): Outcome {
    const phase = state.phase;
    if (phase.kind === 'quit') return refused();
    // The profile can change whenever the wizard is done.
    const profileOpen = phase.kind === 'ready' || phase.kind === 'running' || phase.kind === 'faulted';
    switch (intent.kind) {
      case 'set_language':
        // Automatic follows the language the mock started in.
        state.language_choice = intent.value;
        state.language = intent.value ?? systemLanguage;
        if (state.profile.kind === 'stereo' && !state.profile.from_file) {
          state.profile.name = say.defaultProfileName[state.language];
        }
        return done;
      case 'import_brir':
        return profileOpen ? importBrir(intent.value) : refused();
      case 'import_headphone_fir':
        return profileOpen ? importHeadphoneFir(intent.value) : refused();
      case 'set_gain':
        return profileOpen ? setGain(intent.value) : refused();
      case 'test_direction':
        if (phase.kind === 'running') {
          return intent.value < state.profile.directions.length ? done : failed(say.directionMissing, 'none');
        }
        return profileOpen ? refused(say.testAfterEnable) : refused();
      case 'start_diagnostic_recording':
        return refused(say.diagnosticRecordingUnavailable);
      case 'begin_setup':
        if (phase.kind !== 'setup' || phase.value !== 'welcome') return refused();
        notes = [];
        return evaluate();
      case 'recheck':
        if (phase.kind === 'faulted') {
          state.phase = { kind: 'ready' };
          return done;
        }
        if (phase.kind !== 'setup' || phase.value === 'welcome') return refused();
        // Pretend the user did what the step asked (rebooted, repaired).
        if (phase.value === 'reboot_required') host.driver = GOOD_DRIVER;
        return evaluate();
      case 'install_driver':
        if (phase.kind !== 'setup' || phase.value !== 'driver') return refused();
        host.driver = GOOD_DRIVER;
        return evaluateDriver();
      case 'choose_output':
        if (phase.kind === 'running') return failed(say.disableBeforeChangingOutput, 'none');
        if (phase.kind === 'ready' || (phase.kind === 'setup' && phase.value === 'choose_output')) {
          return chooseOutput(intent.value);
        }
        return refused();
      case 'enable':
        if (phase.kind !== 'ready' && phase.kind !== 'faulted') return refused();
        return enable();
      case 'disable':
        if (phase.kind !== 'running') return refused();
        stop();
        state.phase = { kind: 'ready' };
        return done;
      case 'quit':
        stop();
        state.phase = { kind: 'quit' };
        return done;
    }
  }

  const wait = () =>
    new Promise<void>((resolve) => {
      const ms = options.latencyMs ?? 0;
      if (ms > 0) setTimeout(resolve, ms);
      else queueMicrotask(resolve);
    });

  return {
    async dispatch(intent) {
      // In flight for good: the button that sent it stays pending.
      if (options.hold === true) return new Promise<void>(() => {});
      await wait();
      const outcome = apply(structuredClone(intent));
      // Like the supervisor, a snapshot lists the host's outputs afresh; a
      // refusal changed nothing, so its snapshot stays the same.
      if (outcome.kind !== 'refused') state.outputs = structuredClone(host.outputs);
      if (outcome.kind === 'done' && intent.kind !== 'set_language') {
        kept = null;
        state.error = null;
      }
      if (outcome.kind === 'failed') kept = { message: outcome.message, next: outcome.next };
      // A language change keeps the error and the notes, said again.
      if (kept !== null) state.error = render(kept);
      if (notes !== null) state.notes = notes.map((note) => note[state.language]);
      // The shell replies first and publishes the snapshot after.
      emitLater();
      if (outcome.kind !== 'done') throw render(outcome);
    },
    async snapshot() {
      return structuredClone(state);
    },
    async onSnapshot(draw) {
      snapshotListeners.add(draw);
      return () => snapshotListeners.delete(draw);
    },
    async onMeters(update) {
      meterListeners.add(update);
      syncMeters();
      const unlisten: Unlisten = () => {
        meterListeners.delete(update);
        syncMeters();
      };
      return unlisten;
    },
    shell: options.shell === undefined ? browserShell : standInShell(options.shell),
  };
}
