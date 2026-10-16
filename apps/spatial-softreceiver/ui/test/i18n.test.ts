import { afterEach, describe, expect, expectTypeOf, it, vi } from 'vitest';
import type { Api } from '../src/api';
import { mountApp } from '../src/app';
import type { Language, Snapshot } from '../src/bindings';
import { fixture, fixtureNames } from '../src/fixtures';
import { assertIntent, assertSnapshot } from '../src/guard';
import { createMockApi } from '../src/mock';
import type { Tab } from '../src/screens/common';
import { en, ko, setLanguage, text } from '../src/text';

const HANGUL = /[\u1100-\u11ff\u3130-\u318f\ua960-\ua97f\uac00-\ud7af\ud7b0-\ud7ff\uffa0-\uffdc]/u;
const TABS: Tab[] = ['status', 'profile', 'settings'];

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
  if (clean.profile.brir !== null) clean.profile.brir = 'C:\\SOURCE_BRIR';
  if (clean.profile.headphone_fir !== null) clean.profile.headphone_fir = 'C:\\SOURCE_FIR.wav';
  clean.profile.directions.forEach((direction) => { direction.name = `SOURCE_DIRECTION_${direction.id}`; });
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
      'meters.fellBack': [[0], [2]],
      'format.channels': [[1], [12]],
      'format.otherCodec': [[0x1234]],
      'profile.angles': [[-30, 45]],
      'profile.mapObjects': [[0], [1], [8]],
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
    expect(en.format.otherCodec(0x1234)).toBe('Unknown codec 0x00001234');
    expect(en.profile.angles(-30, 45)).toContain('-30°');
    expect(en.profile.mapObjects(1)).toBe('1 sound object');
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
    snapshot.profile.directions = [];
    const { app, root } = mounted(snapshot, 'profile');
    expectEnglish(root);
    expect(root.textContent).toContain(en.profile.fromFile);
    expect(root.textContent).toContain(en.profile.noDirections);
    app.draw({ ...snapshot, phase: { kind: 'setup', value: 'choose_output' }, outputs: [] });
    expect(root.textContent).toContain(en.wizard.noOutputs);
    expectEnglish(root);
  });

  it('keeps Rust sentences and user device, file, profile and direction names verbatim', () => {
    const snapshot = fixture('running');
    snapshot.language = 'en';
    snapshot.profile.name = '내 프로필';
    snapshot.profile.brir = 'C:\\내 측정\\방';
    snapshot.profile.headphone_fir = 'C:\\보정\\헤드폰.wav';
    snapshot.profile.directions[0]!.name = '왼쪽';
    snapshot.engine!.directions[0]!.name = '왼쪽';
    snapshot.chosen_output!.name = '내 출력';
    snapshot.error = { message: 'Rust 오류 문장', next: 'none' };
    snapshot.notes = ['Rust 알림 문장'];
    const { root } = mounted(snapshot, 'profile');
    for (const value of ['내 프로필', 'C:\\내 측정\\방', 'C:\\보정\\헤드폰.wav', '왼쪽', 'Rust 오류 문장', 'Rust 알림 문장']) {
      expect(root.textContent).toContain(value);
    }
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
    root.querySelector<HTMLButtonElement>('[data-key="fir"]')?.click();
    expect(pickFile.mock.calls).toEqual([
      [en.profile.brirFileDialog, { name: en.profile.brirFilter, extensions: ['wav'] }],
      [en.profile.firDialog, { name: en.profile.firFilter, extensions: ['wav'] }],
    ]);
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
