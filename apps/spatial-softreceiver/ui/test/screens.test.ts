import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { Api } from '../src/api';
import { mountApp } from '../src/app';
import type { EngineStatus, Intent, Snapshot } from '../src/bindings';
import { fixture, fixtureNames } from '../src/fixtures';
import { createMockApi } from '../src/mock';
import type { Tab } from '../src/screens/common';
import { MAP_POOL } from '../src/screens/objectmap';

let root: HTMLElement;

beforeEach(() => {
  root = document.createElement('div');
  document.body.replaceChildren(root);
});

function draw(name: string, tab?: Tab, api: Api = createMockApi(fixture(name))) {
  const app = mountApp(root, api, tab === undefined ? {} : { tab });
  app.draw(fixture(name));
  return app;
}

const textOf = () => root.textContent ?? '';
/** The sentence Rust wrote into a fixture's error. */
const errorOf = (name: string) => fixture(name).error?.message ?? 'no error in fixture';
const buttons = () => [...root.querySelectorAll('button')].map((button) => button.textContent?.trim());

describe('every fixture draws', () => {
  it.each(fixtureNames)('%s', (name) => {
    expect(() => draw(name)).not.toThrow();
    expect(root.dataset.ready).toBe('true');
    expect(textOf().length).toBeGreaterThan(0);
  });

  it.each(['ready', 'running', 'faulted'])('%s on every tab', (name) => {
    for (const tab of ['status', 'profile', 'settings'] as const) {
      expect(() => draw(name, tab)).not.toThrow();
    }
  });
});

describe('what each state says', () => {
  it('welcome offers 시작 and no navigation', () => {
    draw('setup-welcome');
    expect(buttons()).toContain('시작');
    expect(root.querySelector('nav')).toBeNull();
    expect(root.querySelector('[aria-current="step"]')?.textContent).toContain('환영');
  });

  it('unsupported shows the sentence and the platform facts', () => {
    draw('setup-unsupported');
    expect(textOf()).toContain(errorOf('setup-unsupported'));
    expect(textOf()).toContain('18363');
    expect(root.querySelectorAll('[data-intent]')).toHaveLength(0);
  });

  it('not_test_signing offers 다시 확인', () => {
    draw('setup-not-test-signing');
    expect(buttons()).toContain('다시 확인');
    expect(textOf()).toContain(errorOf('setup-not-test-signing'));
  });

  it('the driver step installs, or reinstalls after an error', () => {
    draw('setup-driver-not-installed');
    expect(buttons()).toContain('드라이버 설치');
    expect(textOf()).toContain('관리자 권한');
    draw('setup-driver-declined');
    expect(buttons()).toContain('다시 설치');
    expect(textOf()).toContain(errorOf('setup-driver-declined'));
  });

  it('choose_output lists the three outputs', () => {
    draw('setup-choose-output');
    for (const output of fixture('setup-choose-output').outputs) expect(textOf()).toContain(output.name);
    expect(buttons().filter((label) => label === '이 출력 쓰기')).toHaveLength(3);
  });

  it('choose_output with no outputs says so', () => {
    const snapshot = fixture('setup-choose-output');
    snapshot.outputs = [];
    mountApp(root, createMockApi(snapshot)).draw(snapshot);
    expect(textOf()).toContain('쓸 수 있는 출력 장치가 없습니다. 장치를 꽂고 다시 확인하십시오.');
    expect(buttons()).toContain('다시 확인');
  });

  it('ready is off, with 켜기 and the cards dimmed', () => {
    draw('ready');
    expect(root.querySelector('[data-intent="enable"]')?.textContent).toBe('켜기');
    expect(root.querySelector('#power-state')?.textContent).toContain('꺼짐');
    expect(root.querySelectorAll('.endpoint-card.is-off')).toHaveLength(3);
    expect(root.querySelector('.meters')).toBeNull();
    expect(textOf()).toContain('DENON AVR (NVIDIA High Definition Audio)');
  });

  it('running names the entrances, their formats and the latency', () => {
    draw('running');
    const text = textOf();
    expect(root.querySelector('[data-intent="disable"]')?.textContent).toBe('끄기');
    expect(root.querySelector('#power-state')?.textContent).toContain('켜짐');
    for (const name of ['공간 소프트리시버', '공간 소프트리시버 음악', '공간 소프트리시버 PASS']) {
      expect(text).toContain(name);
    }
    expect(text).toContain('PCM 8채널 48\u00a0kHz float32');
    expect(text).toContain('DD+ 6채널 48\u00a0kHz');
    expect(text).toContain('직접 출력 12채널');
    expect(text).toContain('이 채널 수에는 직접 배치가 없습니다.');
    expect(text).toContain('12.4');
    expect(text).toContain('18.9');
    expect(text).toContain('-37.5');
    expect(text).toContain('독점');
    // The fixture's note is the supervisor's Message::Reconnected.
    expect(text).toContain('다시 연결했습니다.');
  });

  it('running while reconnecting says so in the head', () => {
    draw('running-reconnecting');
    expect(root.querySelector('#power-state')?.textContent).toContain('다시 연결하는 중');
    expect(textOf()).toContain(errorOf('running-reconnecting'));
  });

  it('faulted shows the sentence and 켜기', () => {
    draw('faulted');
    expect(textOf()).toContain(errorOf('faulted'));
    expect(root.querySelector('[data-intent="enable"]')?.textContent).toBe('켜기');
    expect(root.querySelector('#power-state')?.textContent).toContain('멈춤');
  });

  it('the profile screen offers every profile intent', () => {
    draw('ready', 'profile');
    expect(textOf()).toContain('스테레오 기본');
    expect(textOf()).toContain('BRIR 세트를 가져오기 전에는 스테레오 스피커 패닝으로 냅니다.');
    expect(textOf()).toContain('객체 지도');
    expect(textOf()).not.toContain('다음 조각');
    expect(buttons()).toEqual(expect.arrayContaining(['폴더 고르기', 'hesuvi.wav / hrir.wav 고르기', 'FIR 파일 고르기']));
    expect(root.querySelectorAll('[data-intent="test_direction"]')).toHaveLength(2);
    const slider = root.querySelector<HTMLInputElement>('input[type="range"]');
    expect(slider?.min).toBe('-20');
    expect(slider?.max).toBe('10');
  });

  it('the settings screen shows facts and keeps autostart off outside Tauri', () => {
    draw('ready', 'settings');
    expect(root.querySelector<HTMLInputElement>('#autostart')?.disabled).toBe(true);
    expect(textOf()).toContain('26100');
    expect(textOf()).toContain('적용됨');
    expect(buttons()).toContain('진단 녹화 시작');
    // No updater outside Tauri: the panel says so and offers no button.
    expect(textOf()).toContain('앱 안에서만 확인합니다.');
    expect(root.querySelector('[data-key="update-apply"]')).toBeNull();
  });

  it('the settings screen offers the downloaded update with its version', async () => {
    const base = createMockApi(fixture('ready'));
    let applied = 0;
    const api: Api = {
      ...base,
      shell: {
        ...base.shell,
        native: true,
        updateState: async () => ({ current: '0.11.0', ready: '0.12.0' }),
        applyUpdate: async () => {
          applied += 1;
        },
        onUpdate: async () => () => {},
      },
    };
    const app = mountApp(root, api, { tab: 'settings' });
    app.draw(fixture('ready'));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(textOf()).toContain('지금 버전 0.11.0');
    expect(textOf()).toContain('새 버전 0.12.0을 받았습니다.');
    root.querySelector<HTMLButtonElement>('[data-key="update-apply"]')?.click();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(applied).toBe(1);
  });
});

describe('meters', () => {
  it('update text nodes in place', () => {
    const app = draw('running');
    const before = root.querySelector('.meters');
    const cards = [...root.querySelectorAll('.endpoint-card')];
    const engine = fixture('running').engine;
    if (engine === null) throw new Error('running has an engine');
    engine.latency.p50_ms = 7.25;
    const ep = engine.endpoints[0];
    ep.volume_db = -12;
    app.updateMeters(engine);
    expect(root.querySelector('.meters')).toBe(before);
    expect([...root.querySelectorAll('.endpoint-card')]).toEqual(cards);
    expect(root.querySelector('.meters')?.textContent).toContain('7.3');
    expect(cards[0]?.textContent).toContain('-12.0\u00a0dB');
  });

  it('are ignored outside running', () => {
    const app = draw('ready');
    const engine = fixture('running').engine;
    if (engine === null) throw new Error('running has an engine');
    app.updateMeters(engine);
    expect(root.querySelector('.meters')).toBeNull();
    expect(textOf()).not.toContain('12.4');
  });
});

describe('login autostart', () => {
  it('sends one change even when the screen redraws before it ends', async () => {
    const start = fixture('ready');
    const inner = createMockApi(start);
    let enabled = false;
    const calls: boolean[] = [];
    let finish: () => void = () => {};
    const api: Api = {
      ...inner,
      shell: {
        ...inner.shell,
        native: true,
        autostartEnabled: async () => enabled,
        setAutostart: (value) => {
          calls.push(value);
          return new Promise<void>((resolve) => {
            finish = () => {
              enabled = value;
              resolve();
            };
          });
        },
      },
    };
    const app = mountApp(root, api, { tab: 'settings' });
    app.draw(start);
    await new Promise((resolve) => setTimeout(resolve, 0));
    const box = () => root.querySelector<HTMLInputElement>('#autostart');
    expect(box()?.disabled).toBe(false);

    const first = box();
    if (first === null) throw new Error('no autostart checkbox');
    first.checked = true;
    first.dispatchEvent(new Event('change'));
    expect(calls).toEqual([true]);

    // A snapshot and a tab round trip while the request is open.
    app.draw(fixture('ready'));
    root.querySelector<HTMLButtonElement>('[data-key="nav-profile"]')?.click();
    root.querySelector<HTMLButtonElement>('[data-key="nav-settings"]')?.click();
    const redrawn = box();
    expect(redrawn).not.toBe(first);
    expect(redrawn?.disabled).toBe(true);
    expect(redrawn?.checked).toBe(true);

    // Neither the new checkbox nor the old one sends it again.
    for (const stale of [redrawn, first]) {
      if (stale === null) continue;
      stale.checked = !stale.checked;
      stale.dispatchEvent(new Event('change'));
    }
    expect(calls).toEqual([true]);

    finish();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(box()?.disabled).toBe(false);
    expect(box()?.checked).toBe(true);
    expect(calls).toEqual([true]);
  });
});

describe('sending intents', () => {
  /** A bridge whose dispatch waits until the test lets it go. */
  function heldApi(start: Snapshot, outcome: 'resolve' | 'reject') {
    const inner = createMockApi(start);
    const sent: Intent[] = [];
    let release: () => void = () => {};
    const api: Api = {
      ...inner,
      shell: inner.shell,
      dispatch: (intent) => {
        sent.push(intent);
        return new Promise<void>((resolve, reject) => {
          release = () =>
            outcome === 'resolve' ? resolve() : reject({ message: '아직 지원하지 않는 동작입니다.', next: 'none' });
        });
      },
    };
    return { api, sent, release: () => release() };
  }

  const tick = () => new Promise((resolve) => setTimeout(resolve, 0));
  const install = () => root.querySelector<HTMLButtonElement>('[data-intent="install_driver"]');

  it('locks the buttons until the snapshot after the reply lands', async () => {
    const start = fixture('setup-driver-not-installed');
    const held = heldApi(start, 'resolve');
    const app = mountApp(root, held.api);
    app.draw(start);
    install()?.click();
    expect(install()?.textContent).toBe('설치하는 중');
    expect(install()?.disabled).toBe(true);
    install()?.click();
    expect(held.sent).toHaveLength(1);

    held.release();
    await tick();
    expect(install()?.disabled).toBe(true);

    app.draw(fixture('setup-driver-not-installed'));
    await tick();
    expect(install()?.disabled).toBe(false);
    expect(install()?.textContent).toBe('드라이버 설치');
  });

  it('shows a refusal in the band until the state moves on', async () => {
    const start = fixture('ready');
    const held = heldApi(start, 'reject');
    const app = mountApp(root, held.api, { tab: 'settings' });
    app.draw(start);
    root.querySelector<HTMLButtonElement>('[data-intent="start_diagnostic_recording"]')?.click();
    held.release();
    await tick();
    expect(root.querySelector('.banner')?.textContent).toContain('아직 지원하지 않는 동작입니다.');

    // The snapshot the shell publishes after a refusal is unchanged.
    app.draw(fixture('ready'));
    await tick();
    expect(root.querySelector('.banner')?.textContent).toContain('아직 지원하지 않는 동작입니다.');
    expect(root.querySelector<HTMLButtonElement>('[data-intent="start_diagnostic_recording"]')?.disabled).toBe(false);

    app.draw(fixture('running'));
    expect(root.querySelector('.banner')).toBeNull();
  });

  it('holds the gain being sent until the snapshot after it lands', async () => {
    const start = fixture('ready');
    const held = heldApi(start, 'resolve');
    const app = mountApp(root, held.api, { tab: 'profile' });
    app.draw(start);
    const slider = () => root.querySelector<HTMLInputElement>('#gain');
    const first = slider();
    if (first === null) throw new Error('no gain slider');
    first.value = '2';
    first.dispatchEvent(new Event('change'));
    expect(held.sent).toEqual([{ kind: 'set_gain', value: 2 }]);
    expect(slider()?.value).toBe('2');
    expect(slider()?.disabled).toBe(true);

    held.release();
    await tick();
    expect(slider()?.value).toBe('2');

    const after = fixture('ready');
    after.profile.gain_db = 2;
    app.draw(after);
    await tick();
    expect(slider()?.disabled).toBe(false);
    expect(slider()?.value).toBe('2');
    expect(root.querySelector('.gain-readout')?.textContent).toBe('2.0 dB');
  });
});

describe('the profile screen', () => {
  const pickers = () => ({
    folder: root.querySelector<HTMLButtonElement>('[data-key="brir-folder"]'),
    file: root.querySelector<HTMLButtonElement>('[data-key="brir-file"]'),
    fir: root.querySelector<HTMLButtonElement>('[data-key="fir"]'),
  });
  const tones = () => [...root.querySelectorAll<HTMLButtonElement>('[data-intent="test_direction"]')];

  it('names the profile the supervisor resolved', () => {
    draw('running', 'profile');
    const { profile } = fixture('running');
    expect(textOf()).toContain('헤드폰');
    expect(textOf()).toContain(profile.name);
    expect(textOf()).toContain(profile.brir ?? 'no BRIR in fixture');
    expect(textOf()).toContain(profile.headphone_fir ?? 'no FIR in fixture');
    expect(textOf()).not.toContain('BRIR 세트를 가져오기 전에는');
  });

  it('offers one test tone per direction of the profile, by name', () => {
    draw('running', 'profile');
    const { directions } = fixture('running').profile;
    expect(directions.length).toBeGreaterThan(2);
    expect(tones().map((tone) => tone.textContent)).toEqual(directions.map((direction) => direction.name));
    expect(tones().every((tone) => !tone.disabled)).toBe(true);
  });

  it('the settings screen sends the chosen language, null for automatic', async () => {
    const start = fixture('ready');
    const sent: Intent[] = [];
    const api: Api = { ...createMockApi(start), dispatch: async (intent) => void sent.push(intent) };
    const app = mountApp(root, api, { tab: 'settings' });
    app.draw(start);
    const select = () => root.querySelector<HTMLSelectElement>('[data-key="language"]');
    // happy-dom's `select.value` misreports a selected last option; the
    // attribute the screen sets is what a browser selects.
    const chosen = () => select()?.querySelector('option[selected]')?.getAttribute('value') ?? null;
    const choose = async (value: string) => {
      const element = select();
      if (element === null) throw new Error('no language select');
      element.value = value;
      element.dispatchEvent(new Event('change'));
      // The intent's reply lands and the screen is drawn again.
      await new Promise((resolve) => setTimeout(resolve, 0));
      app.draw(start);
    };
    // The fixture follows the Windows language: the selector says so, not 'ko'.
    expect(start.language_choice).toBeNull();
    expect(chosen()).toBe('auto');
    await choose('en');
    await choose('auto');
    expect(sent).toEqual([
      { kind: 'set_language', value: 'en' },
      { kind: 'set_language', value: null },
    ]);
    // A saved choice equal to the resolved language still shows as that choice.
    app.draw({ ...start, language_choice: 'ko' });
    expect(chosen()).toBe('ko');
    app.draw({ ...start, language: 'en', language_choice: 'en' });
    expect(chosen()).toBe('en');
  });

  it('sends the direction id of the tone pressed', () => {
    const start = fixture('running');
    const sent: Intent[] = [];
    const api: Api = { ...createMockApi(start), dispatch: async (intent) => void sent.push(intent) };
    mountApp(root, api, { tab: 'profile' }).draw(start);
    const tfr = start.profile.directions.find((direction) => direction.name === 'TFR');
    tones()
      .find((tone) => tone.textContent === 'TFR')
      ?.click();
    expect(sent).toEqual([{ kind: 'test_direction', value: tfr?.id }]);
  });

  it('locks the test tones until the engine runs', () => {
    draw('ready', 'profile');
    expect(tones().map((tone) => tone.textContent)).toEqual(['FL', 'FR']);
    expect(tones().every((tone) => tone.disabled)).toBe(true);
    expect(root.querySelector('#test-hint')?.textContent).toBe('켠 뒤에 시험음을 낼 수 있습니다.');
    draw('running', 'profile');
    expect(root.querySelector('#test-hint')).toBeNull();
  });

  it('locks every import while profile.toml wins', () => {
    const snapshot = fixture('running');
    snapshot.profile.from_file = true;
    mountApp(root, createMockApi(snapshot), { tab: 'profile' }).draw(snapshot);
    for (const picker of Object.values(pickers())) {
      expect(picker?.disabled).toBe(true);
      expect(picker?.getAttribute('aria-describedby')).toBe('import-file-hint');
    }
    expect(root.querySelector('#import-file-hint')?.textContent).toBe(
      '상태 폴더의 profile.toml이 우선합니다. 그 파일을 지우거나 고치십시오.',
    );
    // Without the file the same profile imports freely.
    draw('running', 'profile');
    for (const picker of Object.values(pickers())) expect(picker?.disabled).toBe(false);
    expect(root.querySelector('#import-file-hint')).toBeNull();
  });

  it('locks the FIR import until a BRIR set is in', () => {
    draw('ready', 'profile');
    expect(pickers().folder?.disabled).toBe(false);
    expect(pickers().file?.disabled).toBe(false);
    expect(pickers().fir?.disabled).toBe(true);
    expect(root.querySelector('#fir-hint')?.textContent).toBe('먼저 BRIR 세트를 가져오십시오.');
  });

  it('imports a BRIR set from a folder or from hesuvi.wav, then a FIR', async () => {
    const start = fixture('ready');
    const api = createMockApi(start);
    const picked: string[] = [];
    const shell = {
      ...api.shell,
      pickFolder: async (title: string) => {
        picked.push(title);
        return 'C:\\BRIR\\living-room';
      },
      pickFile: async (title: string, filter: { name: string; extensions: string[] }) => {
        picked.push(`${title} (${filter.extensions.join(',')})`);
        return filter.name === 'BRIR WAV' ? 'C:\\BRIR\\studio\\hesuvi.wav' : 'C:\\EQ\\hd650.wav';
      },
    };
    const app = mountApp(root, { ...api, shell }, { tab: 'profile' });
    app.draw(start);
    await api.onSnapshot(app.draw);

    pickers().folder?.click();
    await vi.waitFor(() => expect(textOf()).toContain('C:\\BRIR\\living-room'));
    await vi.waitFor(() => expect(pickers().file?.disabled).toBe(false));
    pickers().file?.click();
    await vi.waitFor(() => expect(textOf()).toContain('C:\\BRIR\\studio\\hesuvi.wav'));
    expect(textOf()).toContain('studio');
    await vi.waitFor(() => expect(pickers().fir?.disabled).toBe(false));
    pickers().fir?.click();
    await vi.waitFor(() => expect(textOf()).toContain('C:\\EQ\\hd650.wav'));
    expect(picked).toEqual(['BRIR 세트 폴더', 'hesuvi.wav 또는 hrir.wav (wav)', '헤드폰 FIR 파일 (wav)']);
    expect(tones()).toHaveLength(fixture('running').profile.directions.length);
  });

  it('the gain slider shows the snapshot gain', () => {
    const app = draw('running', 'profile');
    const slider = () => root.querySelector<HTMLInputElement>('#gain');
    expect(slider()?.value).toBe('-3.5');
    expect(root.querySelector('.gain-readout')?.textContent).toBe('-3.5 dB');
    const next = fixture('running');
    next.profile.gain_db = 4;
    app.draw(next);
    expect(slider()?.value).toBe('4');
    expect(root.querySelector('.gain-readout')?.textContent).toBe('4.0 dB');
  });
});

describe('the object map', () => {
  const pool = () => [...root.querySelectorAll<SVGCircleElement>('.map-object')];
  const shown = () => pool().filter((circle) => circle.getAttribute('display') !== 'none');
  const at = (circle: Element | undefined, name: string) => Number(circle?.getAttribute(name));
  const runningEngine = (): EngineStatus => {
    const engine = fixture('running').engine;
    if (engine === null) throw new Error('running has an engine');
    return engine;
  };

  it('draws one circle per scene object while running', () => {
    draw('running', 'profile');
    const { scene } = runningEngine();
    expect(scene.length).toBeGreaterThan(0);
    expect(pool()).toHaveLength(MAP_POOL);
    expect(shown()).toHaveLength(scene.length);
    // Lane picks the colour; the fixture's lanes are 0, 1 and 2 (main, music, PASS).
    const lanes = shown().map((circle) => [...circle.classList].find((name) => name.startsWith('lane-')));
    expect(lanes).toEqual(scene.map((object) => `lane-${object.lane}`));
    // Height picks the outline: the fixture's objects are level or well above.
    expect(shown().map((circle) => circle.classList.contains('is-up'))).toEqual(scene.map((object) => object.z > 0.5));
    expect(textOf()).toContain(`소리 객체 ${scene.length}개`);
  });

  it('puts x to the right and the front at the top', () => {
    draw('running', 'profile');
    // The fixture's first object is front left (x -0.5, y 0.866).
    const [frontLeft] = shown();
    expect(at(frontLeft, 'cx')).toBeLessThan(150);
    expect(at(frontLeft, 'cy')).toBeLessThan(150);
  });

  it('draws louder objects larger and stronger', () => {
    draw('running', 'profile');
    const { scene } = runningEngine();
    const loud = scene.findIndex((object) => object.level_dbfs === -12);
    const quiet = scene.findIndex((object) => object.level_dbfs === -24);
    expect(at(shown()[loud], 'r')).toBeGreaterThan(at(shown()[quiet], 'r'));
    expect(at(shown()[loud], 'opacity')).toBeGreaterThan(at(shown()[quiet], 'opacity'));
  });

  it('names the directions of the layout the engine runs', () => {
    draw('running', 'profile');
    const names = [...root.querySelectorAll('.map-label')].map((label) => label.textContent);
    expect(names).toEqual(runningEngine().directions.map((direction) => direction.name));
  });

  it('moves the circles in place with the meters', () => {
    const app = draw('running', 'profile');
    const before = shown();
    const engine = runningEngine();
    const first = engine.scene[0];
    if (first === undefined) throw new Error('fixture scene is empty');
    first.x = 0.5;
    first.y = -0.866;
    engine.scene.pop();
    app.updateMeters(engine);

    const after = shown();
    expect(after).toHaveLength(before.length - 1);
    expect(after[0]).toBe(before[0]);
    expect(at(after[0], 'cx')).toBeGreaterThan(150);
    expect(at(after[0], 'cy')).toBeGreaterThan(150);
    expect(before.at(-1)?.getAttribute('display')).toBe('none');
    expect(pool()).toHaveLength(MAP_POOL);
    expect(textOf()).toContain(`소리 객체 ${engine.scene.length}개`);
  });

  it('follows a layout the engine changed', () => {
    const app = draw('running', 'profile');
    const engine = runningEngine();
    engine.directions = engine.directions.slice(0, 2);
    app.updateMeters(engine);
    const names = [...root.querySelectorAll('.map-label')].map((label) => label.textContent);
    expect(names).toEqual(['FL', 'FR']);
  });

  it('shows only the profile directions while off', () => {
    const app = draw('ready', 'profile');
    expect(shown()).toHaveLength(0);
    const names = [...root.querySelectorAll('.map-label')].map((label) => label.textContent);
    expect(names).toEqual(['FL', 'FR']);
    expect(textOf()).toContain('켜면 소리 객체가 보입니다.');
    app.updateMeters(runningEngine());
    expect(shown()).toHaveLength(0);
  });
});

describe('the mock supervisor follows the profile rules', () => {
  const runningScene = () => fixture('running').engine?.scene ?? [];

  it('needs a BRIR set before a headphone FIR', async () => {
    const api = createMockApi(fixture('ready'));
    await expect(api.dispatch({ kind: 'import_headphone_fir', value: 'C:\\hd650.wav' })).rejects.toMatchObject({
      message: '먼저 BRIR 세트를 가져오십시오.',
    });
    await api.dispatch({ kind: 'import_brir', value: 'C:\\BRIR\\living-room' });
    await api.dispatch({ kind: 'import_headphone_fir', value: 'C:\\hd650.wav' });
    const { profile } = await api.snapshot();
    expect(profile).toMatchObject({
      kind: 'headphones',
      name: 'living-room',
      brir: 'C:\\BRIR\\living-room',
      headphone_fir: 'C:\\hd650.wav',
      from_file: false,
    });
  });

  it('refuses imports while profile.toml wins', async () => {
    const start = fixture('ready');
    start.profile.from_file = true;
    const api = createMockApi(start);
    await expect(api.dispatch({ kind: 'import_brir', value: 'C:\\BRIR\\living-room' })).rejects.toMatchObject({
      message: '상태 폴더의 profile.toml이 우선합니다. 그 파일을 지우거나 고치십시오.',
    });
    expect((await api.snapshot()).profile.brir).toBeNull();
  });

  it('keeps the gain in range and in the snapshot', async () => {
    const api = createMockApi(fixture('ready'));
    await api.dispatch({ kind: 'set_gain', value: -6 });
    expect((await api.snapshot()).profile.gain_db).toBe(-6);
    await expect(api.dispatch({ kind: 'set_gain', value: 12 })).rejects.toMatchObject({
      message: '게인은 -20 dB에서 +10 dB 사이입니다.',
    });
    expect((await api.snapshot()).profile.gain_db).toBe(-6);
  });

  it('plays a test tone only while running', async () => {
    const api = createMockApi(fixture('running'));
    await api.dispatch({ kind: 'test_direction', value: 3 });
    await expect(api.dispatch({ kind: 'test_direction', value: 99 })).rejects.toMatchObject({
      message: '그 방향이 없습니다.',
    });
    await api.dispatch({ kind: 'disable' });
    await expect(api.dispatch({ kind: 'test_direction', value: 0 })).rejects.toEqual({
      message: '켠 뒤에 시험음을 낼 수 있습니다.',
      next: 'none',
    });
  });

  it('turns the scene slowly while running, for the gallery and npm run dev', async () => {
    const api = createMockApi(fixture('running'), { meters: true });
    const frames: EngineStatus[] = [];
    const stop = await api.onMeters((engine) => frames.push(engine));
    await vi.waitFor(() => expect(frames.length).toBeGreaterThan(2));
    stop();
    const first = frames[0]?.scene[0];
    const last = frames.at(-1)?.scene[0];
    expect(frames.at(-1)?.scene).toHaveLength(runningScene().length);
    expect(last?.x).not.toBe(first?.x);
  });

  it('runs the layout of the profile it enables', async () => {
    const api = createMockApi(fixture('ready'));
    await api.dispatch({ kind: 'enable' });
    const { engine, profile } = await api.snapshot();
    expect(engine?.directions).toEqual(profile.directions);
  });
});
