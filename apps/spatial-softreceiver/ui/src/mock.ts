// A stand-in for the supervisor, for the gallery and `npm run dev` in a plain
// browser. It follows the same rules as crates/supervisor/src/machine.rs for
// the paths the screen can take, starting from a fixture, and emits a
// snapshot after every intent the way the shell does. It decides nothing the
// real app relies on.

import type { Api, Shell, Unlisten } from './api';
import type {
  DirectionView,
  DriverState,
  EngineStatus,
  Intent,
  NextAction,
  OutputChoice,
  Platform,
  ProfileView,
  SceneObject,
  Snapshot,
  UserError,
} from './bindings';
import { fixture } from './fixtures';
import { text } from './text';

export interface MockOptions {
  /** Emit 30 Hz meters while running (off for the gallery and tests). */
  meters?: boolean;
  /** How long each intent takes, so the pending states can be seen. */
  latencyMs?: number;
}

type Outcome = { kind: 'done' } | { kind: 'failed'; error: UserError } | { kind: 'refused'; error: UserError };

const done: Outcome = { kind: 'done' };
const failed = (message: string, next: NextAction): Outcome => ({ kind: 'failed', error: { message, next } });
const refused = (message = '지금은 할 수 없는 동작입니다.'): Outcome => ({
  kind: 'refused',
  error: { message, next: 'none' },
});

const MIN_OS_BUILD = 19041;
const GOOD_PLATFORM: Platform = { os_build: 26100, arch: 'x64', test_signing: true, secure_boot: 'disabled' };
const GOOD_DRIVER: DriverState = {
  kind: 'installed',
  value: { problem_code: 0, service_running: true, exclusive_policy: 'applied' },
};

const mockShell: Shell = {
  native: false,
  pickFolder: async () => 'C:\\Users\\me\\BRIR\\living-room',
  // The BRIR picker asks for hesuvi.wav or hrir.wav; the other for an FIR.
  pickFile: async (_title, filter) =>
    filter.name === text.profile.brirFilter ? 'C:\\Users\\me\\BRIR\\studio\\hesuvi.wav' : 'C:\\Users\\me\\hd650.wav',
  autostartEnabled: async () => false,
  setAutostart: async () => {},
  updateState: async () => ({ current: '0.0.0', ready: null }),
  applyUpdate: async () => {},
  onUpdate: async () => () => {},
};

/** The supervisor's gain range (machine.rs GAIN_MIN_DB, GAIN_MAX_DB). */
const GAIN_MIN_DB = -20;
const GAIN_MAX_DB = 10;

const PROFILE_FILE_WINS = '상태 폴더의 profile.toml이 우선합니다. 그 파일을 지우거나 고치십시오.';

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

/** The layout an imported set reads as: the running fixture's 7.1.4 directions. */
function brirDirections(): DirectionView[] {
  return fixture('running').profile.directions;
}

export function createMockApi(start: Snapshot, options: MockOptions = {}): Api {
  let state: Snapshot = structuredClone(start);
  // The PC the mock pretends to look at.
  const host = {
    platform: start.platform ?? GOOD_PLATFORM,
    driver: start.driver ?? ({ kind: 'not_installed' } as DriverState),
    outputs: start.outputs.length > 0 ? start.outputs : fixture('ready').outputs,
  };
  const engineTemplate: EngineStatus = start.engine ?? (fixture('running').engine as EngineStatus);
  // The scene the meters move around the listener, from the fixture.
  const sceneTemplate: SceneObject[] = structuredClone(engineTemplate.scene);
  const settings: ProfileSettings = {
    brir: start.profile.from_file ? null : start.profile.brir,
    headphoneFir: start.profile.from_file ? null : start.profile.headphone_fir,
    gainDb: start.profile.gain_db,
  };
  const snapshotListeners = new Set<(snapshot: Snapshot) => void>();
  const meterListeners = new Set<(engine: EngineStatus) => void>();
  let meterTimer: ReturnType<typeof setInterval> | undefined;
  let tick = 0;

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

  /** What resolve() in machine.rs makes of the settings (no profile.toml). */
  function resolveProfile(next: ProfileSettings): ProfileView {
    if (next.brir === null) {
      return {
        kind: 'stereo',
        name: 'Stereo',
        brir: null,
        headphone_fir: null,
        gain_db: next.gainDb ?? 0,
        directions: fixture('ready').profile.directions,
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
    if (state.profile.from_file) return failed(PROFILE_FILE_WINS, 'none');
    if (brirSource(path) === null) return failed('hesuvi.wav, hrir.wav 또는 방향별 WAV 폴더를 고르십시오.', 'none');
    return changeProfile({ ...settings, brir: path });
  }

  function importHeadphoneFir(path: string): Outcome {
    if (state.profile.from_file) return failed(PROFILE_FILE_WINS, 'none');
    if (settings.brir === null) return failed('먼저 BRIR 세트를 가져오십시오.', 'none');
    if (!fileName(path).toLowerCase().endsWith('.wav')) {
      return failed(`헤드폰 FIR을 읽지 못했습니다: ${fileName(path)}`, 'none');
    }
    return changeProfile({ ...settings, headphoneFir: path });
  }

  function setGain(db: number): Outcome {
    if (!Number.isFinite(db) || db < GAIN_MIN_DB || db > GAIN_MAX_DB) {
      return failed('게인은 -20 dB에서 +10 dB 사이입니다.', 'none');
    }
    settings.gainDb = db;
    state.profile.gain_db = db;
    return done;
  }

  function setupStep(step: Extract<Snapshot['phase'], { kind: 'setup' }>['value']) {
    state.phase = { kind: 'setup', value: step };
  }

  function evaluate(): Outcome {
    state.platform = structuredClone(host.platform);
    if (host.platform.os_build < MIN_OS_BUILD) {
      setupStep('unsupported');
      return failed(
        `Windows 10 2004(빌드 ${MIN_OS_BUILD}) 이상이 필요합니다. 이 PC는 빌드 ${host.platform.os_build}입니다.`,
        'none',
      );
    }
    if (!host.platform.test_signing) {
      setupStep('not_test_signing');
      let message =
        '시험서명 모드가 꺼져 있습니다. Secure Boot를 끈 뒤 관리자 명령으로 bcdedit /set testsigning on을 실행하고 다시 시작하십시오.';
      if (host.platform.secure_boot === 'enabled') message += ' Secure Boot가 켜져 있어 먼저 꺼야 합니다.';
      return failed(message, 'reboot');
    }
    return evaluateDriver();
  }

  function evaluateDriver(): Outcome {
    const driver = host.driver;
    state.driver = structuredClone(driver);
    if (driver.kind === 'not_installed') {
      setupStep('driver');
      return done;
    }
    if (driver.kind === 'reboot_required') {
      setupStep('reboot_required');
      return failed('드라이버 설치를 마치려면 다시 시작해야 합니다.', 'reboot');
    }
    const { problem_code, service_running, exclusive_policy } = driver.value;
    if (problem_code === 52) {
      setupStep('driver');
      return failed(
        '드라이버 서명을 받아들이지 않았습니다(문제 코드 52). 시험서명 모드를 확인한 뒤 다시 설치하십시오.',
        'retry',
      );
    }
    if (problem_code !== 0) {
      setupStep('driver');
      return failed(`드라이버가 올바르게 설치되지 않았습니다(문제 코드 ${problem_code}). 복구를 실행하십시오.`, 'retry');
    }
    if (!service_running) {
      setupStep('driver');
      return failed('드라이버 서비스가 돌고 있지 않습니다. 복구를 실행하십시오.', 'retry');
    }
    if (exclusive_policy === 'missing') {
      setupStep('driver');
      return failed('드라이버는 있지만 독점 정책이 적용되지 않았습니다. 복구를 실행하십시오.', 'retry');
    }
    setupStep('choose_output');
    if (state.chosen_output !== null) state.phase = { kind: 'ready' };
    return done;
  }

  function chooseOutput(id: string): Outcome {
    const found = host.outputs.find((output) => output.id === id);
    if (found === undefined) return failed('고른 출력 장치를 찾을 수 없습니다.', 'none');
    state.chosen_output = { ...found };
    state.phase = { kind: 'ready' };
    return done;
  }

  function enable(): Outcome {
    state.notes = [];
    const chosen: OutputChoice | null = state.chosen_output;
    if (chosen === null || !host.outputs.some((output) => output.id === chosen.id)) {
      setupStep('choose_output');
      return failed(`저장된 출력 장치를 찾을 수 없습니다: ${chosen?.name ?? ''}`, 'open_settings');
    }
    const previous = host.outputs.find((output) => output.id !== chosen.id) ?? chosen;
    state.engine = structuredClone(engineTemplate);
    state.engine.fault = null;
    state.engine.directions = structuredClone(state.profile.directions);
    state.roles_taken = true;
    state.notes = [`이전 기본 장치 '${previous.name}'를 복원점에 적었습니다.`];
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
        // The browser stand-in has a fixed Korean system-language baseline.
        state.language_choice = intent.value;
        state.language = intent.value ?? 'ko';
        return done;
      case 'import_brir':
        return profileOpen ? importBrir(intent.value) : refused();
      case 'import_headphone_fir':
        return profileOpen ? importHeadphoneFir(intent.value) : refused();
      case 'set_gain':
        return profileOpen ? setGain(intent.value) : refused();
      case 'test_direction':
        if (phase.kind === 'running') {
          return intent.value < state.profile.directions.length ? done : failed('그 방향이 없습니다.', 'none');
        }
        return profileOpen ? refused('켠 뒤에 시험음을 낼 수 있습니다.') : refused();
      case 'start_diagnostic_recording':
        return refused('진단 녹음은 다음 조각에서 옵니다.');
      case 'begin_setup':
        if (phase.kind !== 'setup' || phase.value !== 'welcome') return refused();
        state.notes = [];
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
        if (phase.kind === 'running') return failed('먼저 끈 뒤 출력을 바꾸십시오.', 'none');
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
      await wait();
      const outcome = apply(structuredClone(intent));
      // Like the supervisor, a snapshot lists the host's outputs afresh; a
      // refusal changed nothing, so its snapshot stays the same.
      if (outcome.kind !== 'refused') state.outputs = structuredClone(host.outputs);
      if (outcome.kind === 'done') state.error = null;
      if (outcome.kind === 'failed') state.error = outcome.error;
      // The shell replies first and publishes the snapshot after.
      emitLater();
      if (outcome.kind !== 'done') throw { ...outcome.error };
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
    shell: mockShell,
  };
}
