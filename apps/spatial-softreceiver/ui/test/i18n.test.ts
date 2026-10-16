import { afterEach, describe, expect, expectTypeOf, it, vi } from 'vitest';
import type { Api } from '../src/api';
import { mountApp } from '../src/app';
import type { Language, PluginView, Snapshot, UpmixBypass, UpmixState } from '../src/bindings';
import { en as catalogEn, ko as catalogKo } from '../src/catalog';
import { codecName } from '../src/format';
import { fixture, fixtureNames } from '../src/fixtures';
import { assertIntent, assertSnapshot } from '../src/guard';
import { createMockApi } from '../src/mock';
import type { Tab } from '../src/screens/common';
import { en, ko, setLanguage, text } from '../src/text';

const HANGUL = /[\u1100-\u11ff\u3130-\u318f\ua960-\ua97f\uac00-\ud7af\ud7b0-\ud7ff\uffa0-\uffdc]/u;
const TABS: Tab[] = ['status', 'profile', 'upmix', 'settings'];

afterEach(() => {
  setLanguage('ko');
  document.documentElement.lang = 'ko';
  document.body.replaceChildren();
});

function leaves(value: unknown, path = ''): [string, unknown][] {
  if (typeof value !== 'object' || value === null) return [[path, value]];
  return Object.entries(value).flatMap(([key, child]) => leaves(child, path ? `${path}.${key}` : key));
}

/** Replace only Rust/user-owned fields, not any text the UI produces. */
function uiOnly(snapshot: Snapshot): Snapshot {
  const clean = structuredClone(snapshot);
  if (clean.error !== null) clean.error.message = 'SOURCE_ERROR';
  clean.notes = clean.notes.map((_, index) => `SOURCE_NOTE_${index}`);
  clean.outputs.forEach((output, index) => { output.name = `SOURCE_OUTPUT_${index}`; });
  if (clean.chosen_output !== null) clean.chosen_output.name = 'SOURCE_CHOSEN_OUTPUT';
  clean.profile.name = 'SOURCE_PROFILE';
  if (clean.profile.headphones.brir !== null) clean.profile.headphones.brir = 'C:\\SOURCE_BRIR';
  const { speakers } = clean.profile;
  if (speakers.correction !== null) speakers.correction = { path: 'C:\\SOURCE_CORRECTION.yml', title: 'SOURCE_TITLE' };
  speakers.channels.forEach((channel) => { channel.name = `SOURCE_CHANNEL_${channel.channel}`; });
  clean.profile.directions.forEach((direction) => { direction.name = `SOURCE_DIRECTION_${direction.id}`; });
  for (const endpoint of clean.upmix.endpoints) {
    if (endpoint.plugin !== null) {
      endpoint.plugin = { ...endpoint.plugin, path: `C:\\SOURCE_PLUGIN_${endpoint.endpoint}.vst3`, name: 'SOURCE_PLUGIN', vendor: 'SOURCE_VENDOR' };
    }
    if (endpoint.state.kind === 'failed') endpoint.state.value = 'SOURCE_UPMIX_FAILURE';
  }
  if (clean.engine !== null) {
    if (clean.engine.fault !== null) clean.engine.fault.value = 'SOURCE_FAULT';
    clean.engine.directions.forEach((direction) => { direction.name = `SOURCE_DIRECTION_${direction.id}`; });
  }
  return clean;
}

function expectEnglish(root: HTMLElement) {
  expect(root.textContent).not.toMatch(HANGUL);
  for (const element of root.querySelectorAll('*')) {
    for (const attr of element.attributes) expect(attr.value, `${element.tagName}[${attr.name}]`).not.toMatch(HANGUL);
  }
  expect(document.documentElement.lang).toBe('en');
  expect(document.title).toBe(en.product);
}

function mounted(snapshot: Snapshot, tab: Tab = 'status') {
  const root = document.createElement('div');
  document.body.replaceChildren(root);
  const app = mountApp(root, createMockApi(snapshot), { tab });
  app.draw(snapshot);
  return { app, root };
}

describe('i18n catalog contract', () => {
  it('has identical recursive keys, arrays and leaf kinds in ko and en', () => {
    const shape = (catalog: typeof ko) => leaves(catalog).map(([path, value]) => [path, typeof value]);
    expect(shape(en)).toEqual(shape(ko));
    for (const [path, value] of leaves(en)) {
      if (typeof value === 'string') {
        expect(value, path).not.toMatch(HANGUL);
        expect(value.length, path).toBeGreaterThan(0);
      }
    }
  });

  it('keeps every function signature and exercises every function leaf', () => {
    expectTypeOf(en).toEqualTypeOf<typeof ko>();
    type FunctionPaths<T> = {
      [K in keyof T & string]: T[K] extends (...args: never[]) => unknown
        ? K
        : T[K] extends readonly unknown[] ? never
        : T[K] extends object ? `${K}.${FunctionPaths<T[K]>}` : never;
    }[keyof T & string];
    const cases = {
      'bridge.unreachable': [['detail']],
      'bridge.badSnapshot': [['detail']],
      'meters.fellBackMix': [[0], [2]],
      'format.channels': [[1], [12]],
      'format.otherCodec': [['0x00001234']],
      'profile.angles': [[-30, 45]],
      'profile.mapObjectsMany': [[0], [1], [8]],
      'status.upmix': [['Example Upmixer']],
      'status.upmixFallback': [['Example Upmixer']],
      'upmix.latency': [['12.5']],
      'upmix.outputs': [['L R C LFE Ls Rs']],
      'upmix.delayHint': [['23.3']],
      'settings.updateCurrent': [['0.13.0']],
      'settings.updateReady': [['0.14.0']],
    } satisfies Record<FunctionPaths<typeof ko>, unknown[][]>;
    const functions = leaves(ko).filter(([, value]) => typeof value === 'function');
    expect(functions.map(([path]) => path).sort()).toEqual(Object.keys(cases).sort());
    const english = new Map(leaves(en));
    for (const [path, korean] of functions) {
      const translated = english.get(path) as (...args: unknown[]) => string;
      expect(translated.length, path).toBe((korean as Function).length);
      for (const args of cases[path as keyof typeof cases]) {
        expect(translated(...args), path).not.toMatch(HANGUL);
        expect(translated(...args).length, path).toBeGreaterThan(0);
      }
    }
    setLanguage('en');
    expect(codecName({ other: 0x1234 })).toBe('Unknown codec 0x00001234');
    expect(en.profile.angles(-30, 45)).toContain('-30°');
    expect(en.profile.mapObjectsOne).toBe('1 sound position');
  });

  it.each([
    [ko, catalogKo],
    [en, catalogEn],
  ])('projects generated screen leaves and sources shared names only from names', (projected, catalog) => {
    const { endpoints, status, profile, upmix, ...screen } = catalog.screen;
    const {
      product,
      endpoints: named,
      status: projectedStatus,
      profile: projectedProfile,
      upmix: projectedUpmix,
      ...rest
    } = projected;
    expect(rest).toEqual(screen);
    for (const key of Object.keys(screen) as (keyof typeof screen)[]) {
      expect(rest[key], key).toBe(screen[key]);
    }
    // Sentences the supervisor also says come from [common], not a second copy.
    const { common } = catalog;
    expect(projectedStatus).toEqual({ ...status, powerOn: common.turn_on, powerOff: common.turn_off });
    expect(projectedProfile).toEqual({
      ...profile,
      fromFile: common.profile_file_takes_precedence,
      testOff: common.test_after_enable,
      needsBrir: catalog.supervisor.import_brir_first,
    });
    expect(projectedUpmix).toEqual({ ...upmix, needsPlugin: catalog.supervisor.choose_plugin_first });
    expect(upmix).not.toHaveProperty('needsPlugin');
    for (const key of ['powerOn', 'powerOff']) expect(status).not.toHaveProperty(key);
    for (const key of ['fromFile', 'testOff', 'needsBrir']) expect(profile).not.toHaveProperty(key);
    expect(product).toBe(catalog.names.product);
    expect(projected).not.toHaveProperty('productLatin');
    expect(named).toEqual([
      { ...endpoints[0], name: catalog.names.endpoint_main },
      { ...endpoints[1], name: catalog.names.endpoint_music },
      { ...endpoints[2], name: catalog.names.endpoint_pass },
    ]);
    expect(catalog.screen).not.toHaveProperty('product');
    expect(catalog.screen).not.toHaveProperty('productLatin');
    for (const endpoint of endpoints) expect(Object.keys(endpoint)).toEqual(['purpose']);
    expect(projected.chainErrorKinds['future_kind']).toBeUndefined();
  });

  it('keeps the text.foo calling convention live across catalog changes', () => {
    setLanguage('ko');
    expect(text).toBe(ko);
    setLanguage('en');
    expect(text).toBe(en);
    expect(text.format.channels(8)).toBe('8\u00a0channels');
    setLanguage('ko');
    expect(text.format.channels(8)).toBe('8채널');
  });
});

describe('English rendering', () => {
  it.each(fixtureNames)('%s has no Hangul in UI-owned text or attributes on any tab', (name) => {
    const snapshot = uiOnly(fixture(name));
    snapshot.language = 'en';
    for (const tab of TABS) {
      const { app, root } = mounted(snapshot, tab);
      expectEnglish(root);
      if (snapshot.engine !== null) {
        app.updateMeters(snapshot.engine);
        expectEnglish(root);
      }
    }
  });

  it.each<UpmixBypass>(['native_surround', 'kind_off', 'rate_change'])(
    'says why the sound goes around the upmixer (%s) in English on the upmix and status tabs',
    (bypass) => {
      const snapshot = uiOnly(fixture('running-upmix', 'en'));
      snapshot.upmix.endpoints[1]!.bypass = bypass;
      for (const tab of ['upmix', 'status'] as const) {
        const { root } = mounted(snapshot, tab);
        expectEnglish(root);
        const reason = root.querySelector(tab === 'upmix' ? '.upmix-state-reason' : '.card-upmix-reason');
        expect(reason?.textContent?.length, tab).toBeGreaterThan(0);
      }
    },
  );

  it('names the three cards, the (i), the delay, the reason and the plugin inputs in English', () => {
    const snapshot = uiOnly(fixture('running-upmix-native-surround', 'en'));
    const { root } = mounted(snapshot, 'upmix');
    expectEnglish(root);
    expect([...root.querySelectorAll('.upmix-card h2')].map((title) => title.textContent)).toEqual([
      'Spatial SoftReceiver',
      'Spatial SoftReceiver Music',
      'Spatial SoftReceiver PASS',
    ]);
    const info = root.querySelector<HTMLButtonElement>('[data-key="upmix-info"]');
    expect(info?.getAttribute('aria-label')).toBe('What is upmixed');
    expect(root.querySelector('#upmix-info-tip')?.textContent).toBe('Upmixing applies to mono and stereo only.');
    info?.focus();
    expect(info?.closest('.info-tip-wrap')?.classList.contains('is-open')).toBe(true);
    expectEnglish(root);
    info?.blur();
    expect(root.querySelector('#upmixer-2-delay')?.textContent).toBe('Upmixing delays this input by about 23.3\u00a0ms.');
    expect(root.querySelector('.upmix-state-reason')?.textContent).toBe('Not mono or stereo, so not upmixed');
    expect(root.textContent).toContain('InputsL R');
    expect(root.textContent).not.toMatch(/native surround/i);
  });

  it.each(TABS)('switches the same mounted %s screen ko → en → ko, including live meters', (tab) => {
    const snapshot = uiOnly(fixture('running'));
    snapshot.language = 'ko';
    const { app, root } = mounted(snapshot, tab);
    const korean = root.innerHTML;
    expect(document.title).toBe(ko.product);
    for (const language of ['en', 'ko'] as const) {
      app.draw({ ...snapshot, language });
      if (snapshot.engine !== null) app.updateMeters(snapshot.engine);
      expect(document.documentElement.lang).toBe(language);
      expect(document.title).toBe(language === 'en' ? en.product : ko.product);
      if (language === 'en') expectEnglish(root);
      else expect(root.innerHTML).toBe(korean);
    }
  });

  it.each(fixtureNames.filter((name) => name.startsWith('setup-') || name === 'quit'))(
    'switches the same mounted wizard %s ko → en → ko', (name) => {
      const snapshot = uiOnly(fixture(name));
      snapshot.language = 'ko';
      const { app, root } = mounted(snapshot);
      const korean = root.innerHTML;
      app.draw({ ...snapshot, language: 'en' });
      expectEnglish(root);
      app.draw(snapshot);
      expect(root.innerHTML).toBe(korean);
      expect(document.documentElement.lang).toBe('ko');
      expect(document.title).toBe(ko.product);
    },
  );

  it.each(fixtureNames.filter((name) => ['ready', 'running', 'faulted'].includes(fixture(name).phase.kind)))(
    '%s has no Hangul on either output tab of the profile screen', (name) => {
      const snapshot = uiOnly(fixture(name));
      snapshot.language = 'en';
      const { root } = mounted(snapshot, 'profile');
      for (const kind of ['headphones', 'speakers'] as const) {
        root.querySelector<HTMLButtonElement>(`[data-key="profile-tab-${kind}"]`)?.click();
        expect(root.querySelector(`#profile-panel-${kind}`)).not.toBeNull();
        expectEnglish(root);
      }
    },
  );

  /** The snapshot, then the music endpoint in every upmix state and the main one as a VST upmixer without a plugin. */
  function upmixVariants(snapshot: Snapshot): Snapshot[] {
    const states: UpmixState[] = [
      { kind: 'stopped' },
      { kind: 'starting' },
      { kind: 'running' },
      { kind: 'fallback' },
      { kind: 'failed', value: 'SOURCE_UPMIX_FAILURE' },
    ];
    const plugin: PluginView = { path: 'C:\\SOURCE_PLUGIN.dll', format: 'vst2', name: 'SOURCE_PLUGIN', vendor: 'SOURCE_VENDOR' };
    const variants = [snapshot];
    for (const state of states) {
      const next = structuredClone(snapshot);
      Object.assign(next.upmix.endpoints[1]!, {
        choice: 'plugin',
        effective: 'plugin',
        plugin,
        state,
        latency_ms: 12.5,
        outputs: ['L', 'R', 'C', 'LFE', 'Ls', 'Rs'],
      });
      variants.push(next);
    }
    const bare = structuredClone(snapshot);
    Object.assign(bare.upmix.endpoints[0]!, { choice: null, effective: 'plugin', plugin: null, state: { kind: 'stopped' }, latency_ms: null, outputs: [] });
    bare.upmix.first_party_available = true;
    bare.upmix.headphones = false;
    bare.upmix.speakers = false;
    variants.push(bare);
    return variants;
  }

  it.each(fixtureNames.filter((name) => ['ready', 'running', 'faulted'].includes(fixture(name).phase.kind)))(
    '%s has no Hangul on the upmix tab in any upmix state, nor on the status cards and profile switches', (name) => {
      const start = uiOnly(fixture(name));
      start.language = 'en';
      for (const snapshot of upmixVariants(start)) {
        const upmix = mounted(snapshot, 'upmix').root;
        expect(upmix.querySelectorAll('.upmix-card')).toHaveLength(3);
        expectEnglish(upmix);
        expectEnglish(mounted(snapshot, 'status').root);
        const { root } = mounted(snapshot, 'profile');
        for (const kind of ['headphones', 'speakers'] as const) {
          root.querySelector<HTMLButtonElement>(`[data-key="profile-tab-${kind}"]`)?.click();
          expectEnglish(root);
        }
      }
    },
  );

  it('uses English for the plugin pickers and sends the path as picked', async () => {
    const snapshot = fixture('ready-upmix');
    snapshot.language = 'en';
    const api = createMockApi(snapshot);
    const path = 'C:\\플러그인\\업믹서.vst3';
    const pickFolder = vi.fn(async () => path);
    const pickFile = vi.fn(async () => null);
    const dispatch = vi.fn(async () => {});
    const bridge: Api = { ...api, dispatch, shell: { ...api.shell, pickFolder, pickFile } };
    const root = document.createElement('div');
    document.body.replaceChildren(root);
    mountApp(root, bridge, { tab: 'upmix' }).draw(snapshot);
    root.querySelector<HTMLButtonElement>('[data-key="plugin-file-1"]')?.click();
    root.querySelector<HTMLButtonElement>('[data-key="plugin-folder-1"]')?.click();
    await Promise.resolve();
    expect(pickFile).toHaveBeenCalledWith(en.upmix.fileDialog, { name: en.upmix.fileFilter, extensions: ['vst3', 'dll'] });
    expect(pickFolder).toHaveBeenCalledWith(en.upmix.folderDialog);
    expect(dispatch).toHaveBeenCalledWith({ kind: 'choose_plugin', value: { endpoint: 1, path } });
  });

  it('reads translated navigation labels after switching tabs and opening the output list', () => {
    const snapshot = uiOnly(fixture('ready'));
    snapshot.language = 'en';
    const { root } = mounted(snapshot);
    for (const tab of TABS) {
      root.querySelector<HTMLButtonElement>(`[data-key="nav-${tab}"]`)?.click();
      expect(root.querySelector('h1')?.textContent).toBe(en[tab].title);
      expectEnglish(root);
    }
    root.querySelector<HTMLButtonElement>('[data-key="nav-status"]')?.click();
    root.querySelector<HTMLButtonElement>('[data-key="outputs-toggle"]')?.click();
    expect(root.querySelector('.output-list')).not.toBeNull();
    expectEnglish(root);
  });

  it('translates empty and file-profile branches without translating source data', () => {
    const snapshot = uiOnly(fixture('ready'));
    snapshot.language = 'en';
    snapshot.profile.from_file = true;
    snapshot.profile.kind = 'speakers';
    snapshot.profile.speakers.layout = null;
    snapshot.profile.directions = [];
    const { app, root } = mounted(snapshot, 'profile');
    expectEnglish(root);
    expect(root.textContent).toContain(en.profile.fromFile);
    expect(root.textContent).toContain(en.profile.layoutFromFile);
    expect(root.textContent).toContain(en.profile.noDirections);
    root.querySelector<HTMLButtonElement>('[data-key="profile-tab-headphones"]')?.click();
    expect(root.textContent).toContain(en.profile.useHeadphones);
    expectEnglish(root);
    app.draw({ ...snapshot, phase: { kind: 'setup', value: 'choose_output' }, outputs: [] });
    expect(root.textContent).toContain(en.wizard.noOutputs);
    expectEnglish(root);
  });

  it('keeps Rust sentences and user device, file, profile and direction names verbatim', () => {
    const snapshot = fixture('running');
    snapshot.language = 'en';
    snapshot.profile.from_file = true;
    snapshot.profile.name = '내 프로필';
    snapshot.profile.headphones.brir = 'C:\\내 측정\\방';
    snapshot.profile.speakers.correction = { path: 'C:\\보정\\거실.yml', title: '거실 보정' };
    snapshot.profile.speakers.channels[0]!.name = '앞 왼쪽';
    snapshot.profile.directions[0]!.name = '왼쪽';
    snapshot.engine!.directions[0]!.name = '왼쪽';
    snapshot.chosen_output!.name = '내 출력';
    snapshot.error = { message: 'Rust 오류 문장', next: 'none' };
    snapshot.notes = ['Rust 알림 문장'];
    const { root } = mounted(snapshot, 'profile');
    for (const value of ['내 프로필', 'C:\\내 측정\\방', '왼쪽', 'Rust 오류 문장', 'Rust 알림 문장']) {
      expect(root.textContent).toContain(value);
    }
    root.querySelector<HTMLButtonElement>('[data-key="profile-tab-speakers"]')?.click();
    for (const value of ['C:\\보정\\거실.yml', '거실 보정', '앞 왼쪽']) expect(root.textContent).toContain(value);
    root.querySelector<HTMLButtonElement>('[data-key="nav-status"]')?.click();
    expect(root.textContent).toContain('내 출력');
  });

  it('uses English for native picker titles and filters without changing the selected path', async () => {
    const snapshot = fixture('running');
    snapshot.language = 'en';
    const api = createMockApi(snapshot);
    const path = 'C:\\내 측정\\방';
    const pickFolder = vi.fn(async () => path);
    const pickFile = vi.fn(async () => null);
    const dispatch = vi.fn(async () => {});
    const bridge: Api = { ...api, dispatch, shell: { ...api.shell, pickFolder, pickFile } };
    const root = document.createElement('div');
    document.body.replaceChildren(root);
    const app = mountApp(root, bridge, { tab: 'profile' });
    app.draw(snapshot);
    root.querySelector<HTMLButtonElement>('[data-key="brir-file"]')?.click();
    root.querySelector<HTMLButtonElement>('[data-key="profile-tab-speakers"]')?.click();
    root.querySelector<HTMLButtonElement>('[data-key="correction-import"]')?.click();
    expect(pickFile.mock.calls).toEqual([
      [en.profile.brirFileDialog, { name: en.profile.brirFilter, extensions: ['wav'] }],
      [en.profile.correctionDialog, { name: en.profile.correctionFilter, extensions: ['yml', 'yaml'] }],
    ]);
    root.querySelector<HTMLButtonElement>('[data-key="profile-tab-headphones"]')?.click();
    root.querySelector<HTMLButtonElement>('[data-key="brir-folder"]')?.click();
    await Promise.resolve();
    expect(pickFolder).toHaveBeenCalledWith(en.profile.brirFolderDialog);
    expect(dispatch).toHaveBeenCalledWith({ kind: 'import_brir', value: path });
    await Promise.resolve();
    app.draw(snapshot);
  });
});

describe('language bridge contract', () => {
  it.each<Language>(['ko', 'en'])('accepts the resolved snapshot language %s', (language) => {
    expect(assertSnapshot({ ...fixture('ready'), language }).language).toBe(language);
  });

  it.each(['ko', 'en', null])('accepts the language override %s', (value) => {
    expect(assertIntent({ kind: 'set_language', value })).toEqual({ kind: 'set_language', value });
  });

  it.each(['fr', '', undefined, null, 1])('rejects invalid resolved language %s', (language) => {
    expect(() => assertSnapshot({ ...fixture('ready'), language })).toThrow('snapshot.language');
  });

  it.each(['fr', '', undefined, 1])('rejects invalid language override %s', (value) => {
    expect(() => assertIntent({ kind: 'set_language', value })).toThrow('intent.value');
  });

  it('switches through dispatch and the next snapshot, with null restoring the mock Korean baseline', async () => {
    const snapshot = uiOnly(fixture('ready'));
    snapshot.language = 'ko';
    const api = createMockApi(snapshot);
    const root = document.createElement('div');
    document.body.replaceChildren(root);
    const app = mountApp(root, api);
    app.draw(snapshot);
    const stop = await api.onSnapshot(app.draw);
    await app.send({ kind: 'set_language', value: 'en' });
    expectEnglish(root);
    await app.send({ kind: 'set_language', value: null });
    expect(document.documentElement.lang).toBe('ko');
    expect(root.querySelector('h1')?.textContent).toBe(ko.status.title);
    stop();
  });
});
