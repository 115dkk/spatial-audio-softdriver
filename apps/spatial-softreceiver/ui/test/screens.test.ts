import { beforeEach, describe, expect, it } from 'vitest';
import type { Api } from '../src/api';
import { mountApp } from '../src/app';
import type { Intent, Snapshot } from '../src/bindings';
import { fixture, fixtureNames } from '../src/fixtures';
import { createMockApi } from '../src/mock';
import type { Tab } from '../src/screens/common';

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
    expect(text).toContain('PCM 8채널 48 kHz float32');
    expect(text).toContain('DD+ 6채널 48 kHz');
    expect(text).toContain('직접 출력 12채널');
    expect(text).toContain('이 채널 수에는 직접 배치가 없습니다.');
    expect(text).toContain('12.4');
    expect(text).toContain('18.9');
    expect(text).toContain('-37.5');
    expect(text).toContain('독점');
    expect(text).toContain("이전 기본 장치 '스피커 (Realtek(R) Audio)'를 복원점에 적었습니다.");
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
    expect(textOf()).toContain('BRIR 세트를 가져오기 전에는 스테레오 스피커 패닝으로 냅니다.');
    expect(textOf()).toContain('객체 지도');
    expect(root.querySelectorAll('[data-intent="test_direction"]')).toHaveLength(8);
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
    expect(cards[0]?.textContent).toContain('-12.0 dB');
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
    const app = mountApp(root, held.api, { tab: 'profile' });
    app.draw(start);
    root.querySelector<HTMLButtonElement>('[data-intent="test_direction"]')?.click();
    held.release();
    await tick();
    expect(root.querySelector('.banner')?.textContent).toContain('아직 지원하지 않는 동작입니다.');

    // The snapshot the shell publishes after a refusal is unchanged.
    app.draw(fixture('ready'));
    await tick();
    expect(root.querySelector('.banner')?.textContent).toContain('아직 지원하지 않는 동작입니다.');
    expect(root.querySelector<HTMLButtonElement>('[data-intent="test_direction"]')?.disabled).toBe(false);

    app.draw(fixture('running'));
    expect(root.querySelector('.banner')).toBeNull();
  });
});
