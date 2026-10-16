// A stand-in for the supervisor, for the gallery and `npm run dev` in a plain
// browser. It follows the same rules as crates/supervisor/src/machine.rs for
// the paths the screen can take, starting from a fixture, and emits a
// snapshot after every intent the way the shell does. Its sentences are the
// supervisor's own (crates/supervisor/src/text.rs), in the snapshot's
// language. It decides nothing the real app relies on.

import type { Api, Shell, Unlisten, UpdateState } from './api';
import type {
  Arch,
  CorrectionView,
  DirectionView,
  DriverState,
  EngineStatus,
  Intent,
  Language,
  NextAction,
  OutputChoice,
  Platform,
  PluginFormat,
  PluginView,
  ProfileKind,
  ProfileView,
  SceneObject,
  SecureBoot,
  Snapshot,
  SourceFormat,
  SpeakerChannelView,
  SpeakerLayout,
  UpmixBypass,
  UpmixState,
  UpmixView,
  UpmixerChoice,
  UpmixerSetting,
  UserError,
} from './bindings';
import { en as catalogEn, ko as catalogKo } from './catalog';
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
  /**
   * How long a plugin host takes from starting to running, in ms (default
   * 600). `null` keeps it starting, so a picture holds still.
   */
  upmixStartMs?: number | null;
}

// ---- The supervisor's sentences --------------------------------------------

/** A `supervisor::text::Message`, as `render_ko` and `render_en` write it. */
type Message = Readonly<Record<Language, string>>;

const message = (ko: string, en: string): Message => ({ ko, en });

/** text.rs arch_name. */
const archName = (arch: Arch): Message =>
  arch === 'x64'
    ? message(catalogKo.supervisor.arch_x64, catalogEn.supervisor.arch_x64)
    : arch === 'arm64'
      ? message(catalogKo.supervisor.arch_arm64, catalogEn.supervisor.arch_arm64)
      : message(catalogKo.supervisor.arch_other, catalogEn.supervisor.arch_other);

/** The Messages the mock can meet, by their names in text.rs. */
const say = {
  notAllowed: message(catalogKo.supervisor.not_allowed, catalogEn.supervisor.not_allowed),
  testAfterEnable: message(catalogKo.common.test_after_enable, catalogEn.common.test_after_enable),
  diagnosticRecordingUnavailable: message(
    catalogKo.supervisor.diagnostic_recording_unavailable,
    catalogEn.supervisor.diagnostic_recording_unavailable,
  ),
  disableBeforeChangingOutput: message(
    catalogKo.supervisor.disable_before_changing_output,
    catalogEn.supervisor.disable_before_changing_output,
  ),
  architectureMismatch: (build: Arch, pc: Arch): Message =>
    message(
      catalogKo.supervisor.architecture_mismatch(archName(build).ko, archName(pc).ko),
      catalogEn.supervisor.architecture_mismatch(archName(build).en, archName(pc).en),
    ),
  testSigningDisabled: (secureBoot: SecureBoot): Message => {
    const on = secureBoot === 'enabled';
    return message(
      catalogKo.supervisor.test_signing_disabled +
        (on ? catalogKo.supervisor.test_signing_secure_boot : ''),
      catalogEn.supervisor.test_signing_disabled +
        (on ? catalogEn.supervisor.test_signing_secure_boot : ''),
    );
  },
  driverSignatureRejected: message(catalogKo.supervisor.driver_signature_rejected, catalogEn.supervisor.driver_signature_rejected),
  driverServiceStopped: message(catalogKo.supervisor.driver_service_stopped, catalogEn.supervisor.driver_service_stopped),
  exclusivePolicyMissing: message(catalogKo.supervisor.exclusive_policy_missing, catalogEn.supervisor.exclusive_policy_missing),
  driverProblem: (code: number): Message =>
    message(catalogKo.supervisor.driver_problem(code), catalogEn.supervisor.driver_problem(code)),
  chosenOutputMissing: message(catalogKo.supervisor.chosen_output_missing, catalogEn.supervisor.chosen_output_missing),
  outputIdChanged: (name: string): Message =>
    message(catalogKo.supervisor.output_id_changed(name), catalogEn.supervisor.output_id_changed(name)),
  importBrirFirst: message(catalogKo.supervisor.import_brir_first, catalogEn.supervisor.import_brir_first),
  endpointCannotUpmix: message(catalogKo.supervisor.endpoint_cannot_upmix, catalogEn.supervisor.endpoint_cannot_upmix),
  upmixerUnavailable: message(catalogKo.supervisor.upmixer_unavailable, catalogEn.supervisor.upmixer_unavailable),
  pluginScanFailed: (detail: string): Message =>
    message(catalogKo.supervisor.plugin_scan_failed(detail), catalogEn.supervisor.plugin_scan_failed(detail)),
  correctionReadFailed: (detail: string): Message =>
    message(catalogKo.supervisor.correction_read_failed(detail), catalogEn.supervisor.correction_read_failed(detail)),
  gainOutOfRange: message(catalogKo.supervisor.gain_out_of_range, catalogEn.supervisor.gain_out_of_range),
  directionMissing: message(catalogKo.supervisor.direction_missing, catalogEn.supervisor.direction_missing),
  storedOutputMissing: (name: string): Message =>
    message(catalogKo.supervisor.stored_output_missing(name), catalogEn.supervisor.stored_output_missing(name)),
  profileFileTakesPrecedence: message(
    catalogKo.common.profile_file_takes_precedence,
    catalogEn.common.profile_file_takes_precedence,
  ),
  invalidBrirSource: message(catalogKo.supervisor.invalid_brir_source, catalogEn.supervisor.invalid_brir_source),
  rebootRequired: message(catalogKo.supervisor.reboot_required, catalogEn.supervisor.reboot_required),
  osRequirement: (minBuild: number): Message =>
    minBuild === 19041
      ? message(catalogKo.supervisor.os_requirement_19041, catalogEn.supervisor.os_requirement_19041)
      : message(catalogKo.supervisor.os_requirement(minBuild), catalogEn.supervisor.os_requirement(minBuild)),
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
      ko.upmix.endpoints.forEach((endpoint, index) => {
        const english = en.upmix.endpoints[index]?.state;
        if (endpoint.state.kind === 'failed' && english?.kind === 'failed') pair(endpoint.state.value, english.value);
      });
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

/** Intents that can turn the engine on or off or change the kind in use. */
const PHASE_OR_KIND: ReadonlySet<Intent['kind']> = new Set<Intent['kind']>([
  'enable',
  'disable',
  'quit',
  'recheck',
  'set_profile_kind',
]);
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

// The upmix screen asks for a VST 3 bundle; the profile screen for a BRIR folder.
const pickFolder: Shell['pickFolder'] = async (title) =>
  title === text.upmix.folderDialog
    ? 'C:\\Program Files\\Common Files\\VST3\\Example Upmixer.vst3'
    : 'C:\\Users\\me\\BRIR\\living-room';
// The BRIR picker asks for hesuvi.wav or hrir.wav, the plugin picker for a
// .vst3 or .dll, and the other for a SECS correction.
const pickFile: Shell['pickFile'] = async (_title, filter) =>
  filter.name === text.profile.brirFilter
    ? 'C:\\Users\\me\\BRIR\\studio\\hesuvi.wav'
    : filter.name === text.upmix.fileFilter
      ? 'C:\\Program Files\\VSTPlugins\\Example Upmixer.dll'
      : 'C:\\Users\\me\\Documents\\SECS_export\\study\\configs\\secs_mixed.yml';

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

/**
 * `SpeakerLayout::speakers()` in crates/ssr-types/src/app.rs: each app layout's
 * speakers in device-channel order, by `Speaker::name()`. `LFE` is the
 * subwoofer. test/layouts.test.ts holds this to the Rust source.
 */
export const LAYOUT_SPEAKERS: Readonly<Record<SpeakerLayout, readonly string[]>> = {
  '2.0': ['L', 'R'],
  '2.1': ['L', 'R', 'LFE'],
  '4.0': ['L', 'R', 'Ls', 'Rs'],
  '5.1': ['L', 'R', 'C', 'LFE', 'Ls', 'Rs'],
  '7.1': ['L', 'R', 'C', 'LFE', 'Lrs', 'Rrs', 'Lss', 'Rss'],
  '5.1.2': ['L', 'R', 'C', 'LFE', 'Ls', 'Rs', 'Tfl', 'Tfr'],
  '5.1.4': ['L', 'R', 'C', 'LFE', 'Ls', 'Rs', 'Tfl', 'Tfr', 'Trl', 'Trr'],
  '7.1.2': ['L', 'R', 'C', 'LFE', 'Lrs', 'Rrs', 'Lss', 'Rss', 'Tfl', 'Tfr'],
  '7.1.4': ['L', 'R', 'C', 'LFE', 'Lrs', 'Rrs', 'Lss', 'Rss', 'Tfl', 'Tfr', 'Trl', 'Trr'],
};

/** The subwoofer's name; it is no direction (`Speaker::angles()` is None). */
const SUBWOOFER = 'LFE';

/**
 * `Speaker::angles()` in crates/ssr-types/src/position.rs for the speakers the
 * layouts use: nominal azimuth (positive left) and elevation, in degrees.
 */
export const SPEAKER_ANGLES: Readonly<Record<string, readonly [azimuth: number, elevation: number]>> = {
  L: [30, 0],
  R: [-30, 0],
  C: [0, 0],
  Ls: [110, 0],
  Rs: [-110, 0],
  Lrs: [150, 0],
  Rrs: [-150, 0],
  Lss: [90, 0],
  Rss: [-90, 0],
  Tfl: [45, 45],
  Tfr: [-45, 45],
  Trl: [135, 45],
  Trr: [-135, 45],
};

/** The layout's device channels: one speaker each, in mask order. */
export function speakerChannels(layout: SpeakerLayout): SpeakerChannelView[] {
  return LAYOUT_SPEAKERS[layout].map((name, channel) => ({ name, channel, subwoofer: name === SUBWOOFER }));
}

/** The layout's directions: every speaker but the subwoofer, in channel order. */
export function speakerDirections(layout: SpeakerLayout): DirectionView[] {
  return LAYOUT_SPEAKERS[layout]
    .filter((name) => name !== SUBWOOFER)
    .map((name, id) => {
      const [azimuth, elevation] = SPEAKER_ANGLES[name] ?? [0, 0];
      return { id, name, azimuth_deg: azimuth, elevation_deg: elevation };
    });
}

/** What the supervisor keeps in Settings.profile: each kind's own settings (ADR 0034). */
interface ProfileSettings {
  kind: ProfileKind;
  brir: string | null;
  /** The layout speakers use while no correction is imported. */
  layout: SpeakerLayout;
  correction: CorrectionView | null;
  gainDb: Record<ProfileKind, number | null>;
}

/**
 * machine.rs DEFAULT_UPMIXER: what an endpoint that never chose resolves to.
 * Off until the first-party upmixer exists (ADR 0035).
 */
const DEFAULT_UPMIXER: UpmixerChoice = 'off';

/** The mock's scan finds every plugin to be the test plugin: stereo in, 5.1 out (ADR 0035). */
const PLUGIN_INPUTS: readonly string[] = ['L', 'R'];
const PLUGIN_OUTPUTS: readonly string[] = ['L', 'R', 'C', 'LFE', 'Ls', 'Rs'];
const PLUGIN_VENDOR = 'Example Audio';
/** Two 10 ms blocks, the plugin's own 1.3 ms and the 2 ms concealment (ADR 0035). */
const PLUGIN_LATENCY_MS = 23.3;
/** SPEAKER_LOW_FREQUENCY in a PCM channel mask. */
const LFE_BIT = 0x8;

/** The endpoints that choose an upmixer: main (0), music (1) and PASS (2) (ADR 0035, the 2026-10-16 correction). */
const UPMIX_ENDPOINTS = 3;

/**
 * Whether the sound an endpoint plays now is native surround, which is never
 * upmixed (ADR 0035): PCM with more than two channels besides LFE, or a
 * bitstream that carries more than two. Mono and stereo are upmixed however
 * they arrive. The supervisor judges by the decoded scene; the mock judges by
 * the format the engine reports.
 */
function nativeSurround(format: SourceFormat): boolean {
  if (format.kind === 'pcm') {
    const { channels, channel_mask } = format.value;
    const lfe = (channel_mask & LFE_BIT) !== 0 ? 1 : 0;
    return channels - lfe > 2;
  }
  return format.value.encoded_channels > 2;
}
/** How long the mock's plugin host takes to start unless told otherwise. */
const UPMIX_START_MS = 600;

/** A `.vst3` bundle or file is VST 3, a `.dll` VST 2; anything else does not scan. */
function pluginFormat(name: string): PluginFormat | null {
  if (/\.vst3$/i.test(name)) return 'vst3';
  if (/\.dll$/i.test(name)) return 'vst2';
  return null;
}

/** What the supervisor keeps of an endpoint's upmixer, and what its host is doing. */
interface UpmixSlot {
  endpoint: number;
  choice: UpmixerChoice | null;
  plugin: PluginView | null;
  inputs: string[];
  outputs: string[];
  state: Exclude<UpmixState['kind'], 'failed'>;
  /** Off for this session, and why (said again in the language chosen). */
  failed: Message | null;
  latencyMs: number;
  /** The host has the plugin loaded; it stays loaded while the kind in use does not apply upmixing (ADR 0035). */
  loaded: boolean;
  timer: ReturnType<typeof setTimeout> | undefined;
}

/** A file name the mock takes for a CamillaDSP configuration. */
const isYaml = (path: string) => /\.ya?ml$/i.test(path);

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
  const firstProfile = start.profile;
  const settings: ProfileSettings = {
    kind: firstProfile.kind,
    brir: firstProfile.headphones.brir,
    layout: firstProfile.speakers.layout ?? '2.0',
    correction: firstProfile.speakers.correction,
    gainDb: {
      headphones: firstProfile.kind === 'headphones' ? firstProfile.gain_db : null,
      speakers: firstProfile.kind === 'speakers' ? firstProfile.gain_db : null,
    },
  };
  // The name the starting snapshot gives its BRIR set, kept while that set stays.
  const brirNames = new Map<string, string>();
  if (firstProfile.kind === 'headphones' && firstProfile.headphones.brir !== null && !firstProfile.from_file) {
    brirNames.set(firstProfile.headphones.brir, firstProfile.name);
  }
  // Each endpoint's upmixer, as the starting snapshot has it (an endpoint it
  // leaves out never chose), and whether each kind applies them (ADR 0035).
  const upmixSlots: UpmixSlot[] = Array.from({ length: UPMIX_ENDPOINTS }, (_, endpoint) => {
    const view = start.upmix.endpoints.find((candidate) => candidate.endpoint === endpoint);
    if (view === undefined) {
      return {
        endpoint,
        choice: null,
        plugin: null,
        inputs: [],
        outputs: [],
        state: 'stopped',
        failed: null,
        latencyMs: PLUGIN_LATENCY_MS,
        loaded: false,
        timer: undefined,
      };
    }
    return {
      endpoint,
      choice: view.choice,
      plugin: view.plugin === null ? null : { ...view.plugin },
      inputs: [...view.inputs],
      outputs: [...view.outputs],
      state: view.state.kind === 'failed' ? 'stopped' : view.state.kind,
      failed: view.state.kind === 'failed' ? fixtureSentence(view.state.value) : null,
      latencyMs: view.latency_ms ?? PLUGIN_LATENCY_MS,
      loaded: view.state.kind === 'running' || view.state.kind === 'fallback',
      timer: undefined,
    };
  });
  const upmixApplies: Record<ProfileKind, boolean> = {
    headphones: start.upmix.headphones,
    speakers: start.upmix.speakers,
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

  /**
   * What an imported correction reads as. The mock cannot read the YAML, so
   * every file reads as the SECS export of the ready-speakers fixture: its
   * title, its inputs as directions and its outputs as device channels.
   */
  function correctionShape(): { title: string; channels: SpeakerChannelView[]; directions: DirectionView[] } {
    const { name, directions, speakers } = borrowed('ready-speakers').profile;
    return { title: speakers.correction?.title ?? name, channels: speakers.channels, directions };
  }

  /**
   * What resolve() in machine.rs makes of the settings (no profile.toml): the
   * kind in use names the profile, its gain and its directions, and both
   * kinds' settings are in the snapshot. A correction decides the speakers;
   * without one the stored layout does.
   */
  function resolveProfile(): ProfileView {
    const correction = settings.correction === null ? null : correctionShape();
    const shared = {
      gain_db: settings.gainDb[settings.kind] ?? 0,
      from_file: false,
      headphones: { brir: settings.brir },
      speakers: {
        layout: settings.layout,
        correction: settings.correction,
        channels: correction?.channels ?? speakerChannels(settings.layout),
      },
    };
    if (settings.kind === 'headphones') {
      // set_profile_kind keeps headphones from being chosen without a set.
      const brir = settings.brir ?? '';
      return { kind: 'headphones', name: brirNames.get(brir) ?? brirName(brir), directions: brirDirections(), ...shared };
    }
    if (settings.correction !== null && correction !== null) {
      return { kind: 'speakers', name: settings.correction.title, directions: correction.directions, ...shared };
    }
    return { kind: 'speakers', name: settings.layout, directions: speakerDirections(settings.layout), ...shared };
  }

  /**
   * The snapshot after a settings change. A change to the kind in use goes
   * live (a running engine takes the new layout); a change to the other kind
   * is only kept, so the engine's directions stay as they are.
   */
  function changeProfile(): Outcome {
    state.profile = resolveProfile();
    if (state.engine !== null) state.engine.directions = structuredClone(state.profile.directions);
    return done;
  }

  /** profile.toml decides the output: no kind, BRIR, layout or correction intent is taken. */
  const fileWins = (): Outcome | null =>
    state.profile.from_file ? failed(say.profileFileTakesPrecedence, 'none') : null;

  function setProfileKind(kind: ProfileKind): Outcome {
    const refusal = fileWins();
    if (refusal !== null) return refusal;
    if (kind === 'headphones' && settings.brir === null) return failed(say.importBrirFirst, 'none');
    settings.kind = kind;
    return changeProfile();
  }

  /** Keeps the set for headphones; it never switches the kind. */
  function importBrir(path: string): Outcome {
    const refusal = fileWins();
    if (refusal !== null) return refusal;
    if (brirSource(path) === null) return failed(say.invalidBrirSource, 'none');
    settings.brir = path;
    return changeProfile();
  }

  function setSpeakerLayout(layout: SpeakerLayout): Outcome {
    const refusal = fileWins();
    if (refusal !== null) return refusal;
    settings.layout = layout;
    return changeProfile();
  }

  /** Keeps the correction for speakers; it never switches the kind. */
  function importCorrection(path: string): Outcome {
    const refusal = fileWins();
    if (refusal !== null) return refusal;
    // The detail is the correction crate's; the mock can only name the file.
    if (!isYaml(path)) return failed(say.correctionReadFailed(fileName(path)), 'none');
    settings.correction = { path, title: correctionShape().title };
    return changeProfile();
  }

  /** Back to the stored layout. */
  function clearCorrection(): Outcome {
    const refusal = fileWins();
    if (refusal !== null) return refusal;
    settings.correction = null;
    return changeProfile();
  }

  /** The gain of the kind in use; the other kind keeps its own. */
  function setGain(db: number): Outcome {
    if (!Number.isFinite(db) || db < GAIN_MIN_DB || db > GAIN_MAX_DB) return failed(say.gainOutOfRange, 'none');
    settings.gainDb[state.profile.kind] = db;
    state.profile.gain_db = db;
    return done;
  }

  const effective = (slot: UpmixSlot): UpmixerChoice => slot.choice ?? DEFAULT_UPMIXER;
  const slotOf = (endpoint: number) => upmixSlots.find((slot) => slot.endpoint === endpoint);

  /** Stops the slot's host: nothing loaded, no start under way. */
  function unload(slot: UpmixSlot) {
    clearTimeout(slot.timer);
    slot.timer = undefined;
    slot.loaded = false;
  }

  /** The host comes up after a moment, as `ssr-plughost` loads the plugin. */
  function startHost(slot: UpmixSlot) {
    slot.state = 'starting';
    const ms = options.upmixStartMs === undefined ? UPMIX_START_MS : options.upmixStartMs;
    if (ms === null || slot.timer !== undefined) return;
    slot.timer = setTimeout(() => {
      slot.timer = undefined;
      if (slot.state !== 'starting') return;
      slot.loaded = true;
      slot.state = 'running';
      slot.latencyMs = PLUGIN_LATENCY_MS;
      publishUpmix();
      emitLater();
    }, ms);
  }

  /**
   * What each upmixer does now: a VST upmixer with its plugin runs while the
   * engine runs, whatever the kind in use's switch says. With the switch off
   * the sound goes around it, without delay, and the host stays loaded, so
   * turning it on again comes straight back (ADR 0035). A plugin that
   * stopped three times stays off for the session.
   */
  function refreshUpmix() {
    const running = state.phase.kind === 'running';
    for (const slot of upmixSlots) {
      if (effective(slot) !== 'plugin' || slot.plugin === null || slot.failed !== null || !running) {
        unload(slot);
        slot.state = 'stopped';
      } else if (slot.loaded) {
        if (slot.state !== 'fallback') slot.state = 'running';
      } else {
        startHost(slot);
      }
    }
  }

  /**
   * Why the sound the endpoint plays now goes around its running upmixer:
   * the kind in use does not apply it, before anything else, or the sound is
   * native surround (ADR 0035). Null while it goes through, and while nothing
   * plays. The mock's rate never changes.
   */
  function bypassOf(slot: UpmixSlot): UpmixBypass | null {
    if (slot.state !== 'running' || slot.failed !== null) return null;
    if (!upmixApplies[state.profile.kind]) return 'kind_off';
    const format = state.engine?.endpoints[slot.endpoint]?.format ?? null;
    return format !== null && nativeSurround(format) ? 'native_surround' : null;
  }

  /** Snapshot.upmix from the slots, in the snapshot's language. */
  function publishUpmix() {
    state.upmix = {
      endpoints: upmixSlots.map((slot) => {
        const live = slot.state === 'running' || slot.state === 'fallback';
        const shown: UpmixState =
          slot.failed !== null && effective(slot) === 'plugin'
            ? { kind: 'failed', value: slot.failed[state.language] }
            : { kind: slot.state };
        return {
          endpoint: slot.endpoint,
          choice: slot.choice,
          effective: effective(slot),
          plugin: slot.plugin === null ? null : { ...slot.plugin },
          state: shown,
          bypass: shown.kind === 'failed' ? null : bypassOf(slot),
          latency_ms: live && shown.kind !== 'failed' ? slot.latencyMs : null,
          inputs: slot.plugin === null ? [] : [...slot.inputs],
          outputs: slot.plugin === null ? [] : [...slot.outputs],
        };
      }),
      headphones: upmixApplies.headphones,
      speakers: upmixApplies.speakers,
      first_party_available: state.upmix.first_party_available,
    } satisfies UpmixView;
  }

  /** Main, music and PASS choose; the first-party upmixer is refused until it exists. */
  function setUpmixer({ endpoint, choice }: UpmixerSetting): Outcome {
    const slot = slotOf(endpoint);
    if (slot === undefined) return refused(say.endpointCannotUpmix);
    if (choice === 'first_party' && !state.upmix.first_party_available) return refused(say.upmixerUnavailable);
    slot.choice = choice;
    refreshUpmix();
    return done;
  }

  /**
   * Scans the plugin and keeps it for the endpoint. The mock cannot load a
   * plugin: a .vst3 or .dll reads as the test plugin under its file's name.
   */
  function choosePlugin(endpoint: number, path: string): Outcome {
    const slot = slotOf(endpoint);
    if (slot === undefined) return refused(say.endpointCannotUpmix);
    const name = path.replace(/[\\/]+$/, '').split(/[\\/]/).pop() ?? path;
    const format = pluginFormat(name);
    if (format === null) return failed(say.pluginScanFailed(name), 'none');
    // A new plugin is a new host, and no earlier failure carries over.
    unload(slot);
    slot.plugin = { path, format, name: name.replace(/\.(vst3|dll)$/i, ''), vendor: PLUGIN_VENDOR };
    slot.inputs = [...PLUGIN_INPUTS];
    slot.outputs = [...PLUGIN_OUTPUTS];
    slot.failed = null;
    slot.state = 'stopped';
    refreshUpmix();
    return done;
  }

  /** The editor is the host's own window, so the host must be up. */
  function openPluginEditor(endpoint: number): Outcome {
    const slot = slotOf(endpoint);
    const up = slot !== undefined && slot.failed === null && (slot.state === 'running' || slot.state === 'fallback');
    return up ? done : refused();
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
        return done;
      case 'set_profile_kind':
        return profileOpen ? setProfileKind(intent.value) : refused();
      case 'import_brir':
        return profileOpen ? importBrir(intent.value) : refused();
      case 'set_speaker_layout':
        return profileOpen ? setSpeakerLayout(intent.value) : refused();
      case 'import_correction':
        return profileOpen ? importCorrection(intent.value) : refused();
      case 'clear_correction':
        return profileOpen ? clearCorrection() : refused();
      case 'set_gain':
        return profileOpen ? setGain(intent.value) : refused();
      case 'set_upmixer':
        return profileOpen ? setUpmixer(intent.value) : refused();
      case 'choose_plugin':
        return profileOpen ? choosePlugin(intent.value.endpoint, intent.value.path) : refused();
      case 'open_plugin_editor':
        return profileOpen ? openPluginEditor(intent.value) : refused();
      case 'set_upmix_apply':
        if (!profileOpen) return refused();
        upmixApplies[intent.value.kind] = intent.value.on;
        refreshUpmix();
        return done;
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
      // A change of phase or of the kind in use starts or stops the upmixers.
      if (outcome.kind === 'done' && PHASE_OR_KIND.has(intent.kind)) refreshUpmix();
      publishUpmix();
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
