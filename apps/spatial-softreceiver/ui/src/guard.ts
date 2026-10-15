// Runtime checks that a value from Rust has the shape of bindings.ts.
//
// Every table below is typed against the generated bindings, so the compiler
// fails when bindings.ts gains or loses a field, a variant or a unit name and
// this file was not updated. The fixtures test runs every fixture through
// these guards, which is what ties fixtures, bindings and the screen together.

import type {
  Arch,
  BitstreamCodec,
  BitstreamFormat,
  ChainErrorMap,
  ClockStatus,
  DirectionView,
  DriverState,
  EndpointStatus,
  EngineFault,
  EngineStatus,
  Intent,
  LatencyStats,
  Language,
  NextAction,
  OutputChoice,
  OutputMode,
  PcmFormat,
  Phase,
  Platform,
  PolicyState,
  ProfileKind,
  ProfileView,
  SampleType,
  SceneObject,
  SecureBoot,
  SetupStep,
  Snapshot,
  SourceFormat,
  UnderrunSnapshot,
  UnsupportedReason,
  UserError,
} from './bindings';

/** A value that is not what the bindings say, with where it was found. */
export class ShapeError extends Error {
  constructor(
    readonly path: string,
    readonly expected: string,
  ) {
    super(`${path}: expected ${expected}`);
    this.name = 'ShapeError';
  }
}

type Guard<T> = (value: unknown, path: string) => T;

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value);

const number: Guard<number> = (value, path) => {
  if (typeof value !== 'number' || !Number.isFinite(value)) throw new ShapeError(path, 'a number');
  return value;
};

const string: Guard<string> = (value, path) => {
  if (typeof value !== 'string') throw new ShapeError(path, 'a string');
  return value;
};

const boolean: Guard<boolean> = (value, path) => {
  if (typeof value !== 'boolean') throw new ShapeError(path, 'a boolean');
  return value;
};

function nullable<T>(guard: Guard<T>): Guard<T | null> {
  return (value, path) => (value === null ? null : guard(value, path));
}

function array<T>(guard: Guard<T>): Guard<T[]> {
  return (value, path) => {
    if (!Array.isArray(value)) throw new ShapeError(path, 'an array');
    return value.map((item, index) => guard(item, `${path}[${index}]`));
  };
}

/** A string from a closed set; the record's keys must be the whole union. */
function oneOf<T extends string>(table: Record<T, true>): Guard<T> {
  const names = Object.keys(table);
  return (value, path) => {
    if (typeof value !== 'string' || !names.includes(value)) {
      throw new ShapeError(path, `one of ${names.join(', ')}`);
    }
    return value as T;
  };
}

/** An object with exactly these fields. */
function object<T>(fields: { [K in keyof T]-?: Guard<T[K]> }): Guard<T> {
  const entries = Object.entries(fields) as [string, Guard<unknown>][];
  return (value, path) => {
    if (!isRecord(value)) throw new ShapeError(path, 'an object');
    for (const key of Object.keys(value)) {
      if (!(key in fields)) throw new ShapeError(`${path}.${key}`, 'no such field');
    }
    const out: Record<string, unknown> = {};
    for (const [key, guard] of entries) {
      if (!(key in value)) throw new ShapeError(`${path}.${key}`, 'a field');
      out[key] = guard(value[key], `${path}.${key}`);
    }
    return out as T;
  };
}

type Kinds<T extends { kind: string }> = {
  [K in T['kind']]: Guard<Extract<T, { kind: K }>>;
};

/** A serde enum tagged `kind` (data under `value`); one guard per variant. */
function tagged<T extends { kind: string }>(cases: Kinds<T>): Guard<T> {
  const table = cases as Record<string, Guard<T>>;
  return (value, path) => {
    if (!isRecord(value) || typeof value.kind !== 'string' || !(value.kind in table)) {
      throw new ShapeError(`${path}.kind`, `one of ${Object.keys(table).join(', ')}`);
    }
    const guard = table[value.kind] as Guard<T>;
    return guard(value, path);
  };
}

function unit<K extends string>(kind: K): Guard<{ kind: K }> {
  return object<{ kind: K }>({ kind: oneOf({ [kind]: true } as Record<K, true>) });
}

function carrying<K extends string, V>(kind: K, inner: Guard<V>): Guard<{ kind: K; value: V }> {
  return object<{ kind: K; value: V }>({
    kind: oneOf({ [kind]: true } as Record<K, true>),
    value: inner,
  });
}

// Unit enums: the keys are checked against the bindings' unions.
export const LANGUAGES: Record<Language, true> = { ko: true, en: true };
export const ARCHES: Record<Arch, true> = { x64: true, arm64: true, other: true };
export const SECURE_BOOT: Record<SecureBoot, true> = { enabled: true, disabled: true, unsupported: true };
export const POLICY_STATES: Record<PolicyState, true> = { applied: true, missing: true, unknown: true };
export const SETUP_STEPS: Record<SetupStep, true> = {
  welcome: true,
  unsupported: true,
  not_test_signing: true,
  driver: true,
  reboot_required: true,
  choose_output: true,
};
export const NEXT_ACTIONS: Record<NextAction, true> = {
  retry: true,
  open_settings: true,
  reboot: true,
  none: true,
};
export const OUTPUT_MODES: Record<OutputMode, true> = { exclusive: true, shared: true };
export const PROFILE_KINDS: Record<ProfileKind, true> = { stereo: true, headphones: true, speakers: true };
export const CLOCKS: Record<ClockStatus, true> = { trim: true, asrc: true };
export const SAMPLE_TYPES: Record<SampleType, true> = { int: true, float: true };
export const UNSUPPORTED_REASONS: Record<UnsupportedReason, true> = {
  format: true,
  source_rate: true,
  direct_layout: true,
};
export const NAMED_CODECS: Record<Exclude<BitstreamCodec, object>, true> = {
  ac3: true,
  eac3: true,
  eac3_joc: true,
  true_hd: true,
  mat20: true,
  mat21: true,
  dts: true,
  dts_hd: true,
};

const language = oneOf(LANGUAGES);
const arch = oneOf(ARCHES);
const secureBoot = oneOf(SECURE_BOOT);
const policyState = oneOf(POLICY_STATES);
const setupStep = oneOf(SETUP_STEPS);
const nextAction = oneOf(NEXT_ACTIONS);
const outputMode = oneOf(OUTPUT_MODES);
const profileKind = oneOf(PROFILE_KINDS);
const clock = oneOf(CLOCKS);
const sampleType = oneOf(SAMPLE_TYPES);
const unsupportedReason = oneOf(UNSUPPORTED_REASONS);
const namedCodec = oneOf(NAMED_CODECS);

/** A codec by name, or `{ other: n }` for a subtype no adapter knows. */
const codec: Guard<BitstreamCodec> = (value, path) =>
  typeof value === 'string' ? namedCodec(value, path) : object<{ other: number }>({ other: number })(value, path);

const chainErrors: Guard<ChainErrorMap> = (value, path) => {
  if (!isRecord(value)) throw new ShapeError(path, 'a map of counts');
  const out: ChainErrorMap = {};
  for (const [key, count] of Object.entries(value)) out[key] = number(count, `${path}.${key}`);
  return out;
};

const pcmFormat = object<PcmFormat>({
  channels: number,
  sample_rate: number,
  bits_per_sample: number,
  valid_bits: number,
  sample_type: sampleType,
  channel_mask: number,
});

const bitstreamFormat = object<BitstreamFormat>({
  codec,
  container_rate: number,
  container_channels: number,
  encoded_rate: number,
  encoded_channels: number,
});

const sourceFormat = tagged<SourceFormat>({
  pcm: carrying('pcm', pcmFormat),
  bitstream: carrying('bitstream', bitstreamFormat),
});

const endpointStatus = object<EndpointStatus>({
  format: nullable(sourceFormat),
  epoch: number,
  volume_db: number,
  muted: boolean,
  unsupported: nullable(unsupportedReason),
  chain_errors: chainErrors,
});

type EndpointSlots = EngineStatus['endpoints'];

const endpoints: Guard<EndpointSlots> = (value, path) => {
  const list = array(endpointStatus)(value, path);
  if (list.length !== 4) throw new ShapeError(path, 'four endpoint slots');
  return list as EndpointSlots;
};

const latency = object<LatencyStats>({ p50_ms: number, p99_ms: number, samples: number });

const underrun = object<UnderrunSnapshot>({
  ring_frames: number,
  marker_frames: number,
  layout_frames: number,
  needed_frames: number,
  produced_frames: number,
  since_block_ms: number,
});

const engineFault = tagged<EngineFault>({
  capture_lost: carrying('capture_lost', string),
  output_lost: carrying('output_lost', string),
});

const directionView = object<DirectionView>({
  id: number,
  name: string,
  azimuth_deg: number,
  elevation_deg: number,
});

const sceneObject = object<SceneObject>({
  lane: number,
  index: number,
  x: number,
  y: number,
  z: number,
  level_dbfs: number,
});

const engineStatus = object<EngineStatus>({
  directions: array(directionView),
  scene: array(sceneObject),
  endpoints,
  unattributed_chain_errors: chainErrors,
  skipped_records: number,
  latency,
  ring_level_ms: number,
  underruns: number,
  last_underrun: nullable(underrun),
  output_periods: number,
  capture_reads: number,
  output_restarts: number,
  output_rate: number,
  trim_available: boolean,
  output_channels: number,
  output_channel_mask: number,
  output_reopens: number,
  output_reconnects: number,
  output_reconnect_attempts: number,
  output_mode: outputMode,
  output_fell_back: boolean,
  output_mix_channels: number,
  output_cadence_ratio: number,
  output_cadence_faults: number,
  lost_bytes: number,
  lane_underrun_frames: number,
  dropped_frames: number,
  decode_refused: number,
  volume_transitions_coalesced: number,
  output_overflow_frames: number,
  idle_blocks: number,
  dropped_events: number,
  clock,
  clock_ppm: number,
  fault: nullable(engineFault),
});

const phase = tagged<Phase>({
  setup: carrying('setup', setupStep),
  ready: unit('ready'),
  running: unit('running'),
  faulted: unit('faulted'),
  quit: unit('quit'),
});

const outputChoice = object<OutputChoice>({ id: string, name: string });

const driverState = tagged<DriverState>({
  not_installed: unit('not_installed'),
  installed: carrying(
    'installed',
    object<{ problem_code: number; service_running: boolean; exclusive_policy: PolicyState }>({
      problem_code: number,
      service_running: boolean,
      exclusive_policy: policyState,
    }),
  ),
  reboot_required: unit('reboot_required'),
});

const platform = object<Platform>({
  os_build: number,
  arch,
  test_signing: boolean,
  secure_boot: secureBoot,
});

const userError = object<UserError>({ message: string, next: nextAction });

const profileView = object<ProfileView>({
  kind: profileKind,
  name: string,
  brir: nullable(string),
  headphone_fir: nullable(string),
  gain_db: number,
  directions: array(directionView),
  from_file: boolean,
});

const snapshot = object<Snapshot>({
  language,
  language_choice: nullable(language),
  phase,
  profile: profileView,
  engine: nullable(engineStatus),
  outputs: array(outputChoice),
  chosen_output: nullable(outputChoice),
  driver: nullable(driverState),
  platform: nullable(platform),
  roles_taken: boolean,
  error: nullable(userError),
  notes: array(string),
});

const intent = tagged<Intent>({
  set_language: carrying('set_language', nullable(language)),
  begin_setup: unit('begin_setup'),
  install_driver: unit('install_driver'),
  recheck: unit('recheck'),
  choose_output: carrying('choose_output', string),
  import_brir: carrying('import_brir', string),
  import_headphone_fir: carrying('import_headphone_fir', string),
  set_gain: carrying('set_gain', number),
  test_direction: carrying('test_direction', number),
  enable: unit('enable'),
  disable: unit('disable'),
  start_diagnostic_recording: unit('start_diagnostic_recording'),
  quit: unit('quit'),
});

export const assertSnapshot = (value: unknown): Snapshot => snapshot(value, 'snapshot');
export const assertEngineStatus = (value: unknown): EngineStatus => engineStatus(value, 'engine');
export const assertUserError = (value: unknown): UserError => userError(value, 'error');
export const assertIntent = (value: unknown): Intent => intent(value, 'intent');
