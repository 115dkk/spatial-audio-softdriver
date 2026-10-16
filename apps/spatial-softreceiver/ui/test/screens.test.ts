import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Api } from '../src/api';
import { mountApp } from '../src/app';
import type { EngineStatus, Intent, ProfileKind, Snapshot } from '../src/bindings';
import { fixture, fixtureNames } from '../src/fixtures';
import { SPEAKER_LAYOUTS } from '../src/guard';
import { createMockApi } from '../src/mock';
import type { Tab } from '../src/screens/common';
import { MAP_POOL } from '../src/screens/objectmap';
import { en, ko } from '../src/text';

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

  it('running while reconnecting says so in the head, and only there', () => {
    draw('running-reconnecting');
    expect(fixture('running-reconnecting').error).toBeNull();
    const state = root.querySelector('#power-state');
    expect(state?.textContent).toContain('다시 연결하는 중');
    expect(state?.className).toContain('tone-warning');
    // The head already says it: no band repeats it.
    expect(root.querySelector('.banner')).toBeNull();
  });

  it('faulted shows the sentence and 켜기', () => {
    draw('faulted');
    expect(textOf()).toContain(errorOf('faulted'));
    expect(root.querySelector('[data-intent="enable"]')?.textContent).toBe('켜기');
    expect(root.querySelector('#power-state')?.textContent).toContain('멈춤');
  });

  it('the profile screen opens on the speakers of the first day', () => {
    draw('ready', 'profile');
    expect(root.querySelector('[role="tab"][aria-selected="true"]')?.textContent).toBe('스피커사용 중');
    expect(textOf()).toContain('지금 스피커로 듣고 있습니다.');
    expect(textOf()).toContain('객체 지도');
    expect(textOf()).not.toContain('다음 조각');
    expect(buttons()).toEqual(expect.arrayContaining(['헤드폰', '스피커사용 중', '보정 파일 고르기']));
    expect(root.querySelector<HTMLSelectElement>('[data-key="speaker-layout"]')?.disabled).toBe(false);
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
    // Only the power switch turns a spinner; this button says what it does.
    expect(install()?.textContent).toBe('설치하는 중');
    expect(install()?.querySelector('.spinner')).toBeNull();
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

  describe('the power switch', () => {
    // The stylesheet decides what is seen, so the document gets it. (Vite
    // rewrites `new URL(path, import.meta.url)` into an asset URL here.)
    const styles = readFileSync(join(dirname(fileURLToPath(import.meta.url)), '../src/styles.css'), 'utf8');
    let sheet: HTMLStyleElement | null = null;
    beforeEach(() => {
      sheet = document.createElement('style');
      sheet.textContent = styles;
      document.head.append(sheet);
    });
    afterEach(() => sheet?.remove());

    const power = () => root.querySelector<HTMLButtonElement>('[data-key="power"]');
    const head = () => root.querySelector('#power-state');
    const rail = () => root.querySelector('.nav-power');

    /** The text a sighted user sees in `element`, by the stylesheet. */
    function seenText(element: Element | null): string {
      const hidden = (at: Element | null): boolean => {
        for (; at !== null && at !== element?.parentElement; at = at.parentElement) {
          const style = getComputedStyle(at);
          if (style.display === 'none' || style.visibility === 'hidden' || style.clipPath === 'inset(50%)') return true;
        }
        return false;
      };
      const seen = (node: Node): string =>
        node.nodeType === Node.TEXT_NODE
          ? hidden(node.parentElement)
            ? ''
            : (node.textContent ?? '')
          : [...node.childNodes].map(seen).join('');
      return element === null ? '' : seen(element).trim();
    }

    /** What a screen reader calls `element`: its text outside aria-hidden parts. */
    function spokenName(element: Element | null): string {
      const spoken = (node: Node): string =>
        node instanceof Element && node.getAttribute('aria-hidden') === 'true'
          ? ''
          : node.nodeType === Node.TEXT_NODE
            ? (node.textContent ?? '')
            : [...node.childNodes].map(spoken).join('');
      return element === null ? '' : (element.getAttribute('aria-label') ?? spoken(element).trim());
    }

    /** The state word in the status head and in the rail, with its tone. */
    const states = () =>
      [head(), rail()].map((state) => [
        state?.textContent,
        [...(state?.classList ?? [])].find((name) => name.startsWith('tone-')),
      ]);

    describe.each(['ko', 'en'] as const)('in %s', (language) => {
      const t = language === 'ko' ? ko : en;

      it('turning on spins in place of the label and says turning on', async () => {
        const start = fixture('ready', language);
        const held = heldApi(start, 'resolve');
        const app = mountApp(root, held.api);
        app.draw(start);
        expect(seenText(power())).toBe(t.status.powerOn);
        expect(power()?.querySelector('.spinner')).toBeNull();

        power()?.click();
        expect(held.sent).toEqual([{ kind: 'enable' }]);
        const button = power();
        expect(button?.getAttribute('aria-busy')).toBe('true');
        expect(button?.disabled).toBe(true);
        expect(button?.querySelector('.spinner')?.getAttribute('aria-hidden')).toBe('true');
        expect(seenText(button)).toBe('');
        expect(spokenName(button)).toBe(t.status.powerTurningOn);
        // Its name already says turning on, so the state word is not read twice.
        expect(button?.hasAttribute('aria-describedby')).toBe(false);
        // The hidden label still holds the button's width.
        expect(button?.querySelector('.button-label')?.textContent).toBe(t.status.powerOn);
        expect(states()).toEqual([
          [t.status.powerTurningOn, 'tone-warning'],
          [t.status.powerTurningOn, 'tone-warning'],
        ]);

        // The reply is in; the switch still waits for the snapshot after it.
        held.release();
        await tick();
        expect(power()?.querySelector('.spinner')).not.toBeNull();

        app.draw(fixture('running', language));
        await tick();
        expect(power()?.getAttribute('aria-busy')).toBeNull();
        expect(power()?.querySelector('.spinner')).toBeNull();
        expect(seenText(power())).toBe(t.status.powerOff);
        expect(spokenName(power())).toBe(t.status.powerOff);
        expect(power()?.getAttribute('aria-describedby')).toBe('power-state');
        expect(states()).toEqual([
          [t.status.on, 'tone-on'],
          [t.status.on, 'tone-on'],
        ]);
      });

      it('turning off spins too, says turning off, and leaves the state on', async () => {
        const start = fixture('running', language);
        const held = heldApi(start, 'resolve');
        const app = mountApp(root, held.api);
        app.draw(start);

        power()?.click();
        expect(held.sent).toEqual([{ kind: 'disable' }]);
        expect(power()?.getAttribute('aria-busy')).toBe('true');
        expect(power()?.querySelector('.spinner')).not.toBeNull();
        expect(seenText(power())).toBe('');
        expect(spokenName(power())).toBe(t.status.powerTurningOff);
        expect(power()?.querySelector('.button-label')?.textContent).toBe(t.status.powerOff);
        expect(states()).toEqual([
          [t.status.on, 'tone-on'],
          [t.status.on, 'tone-on'],
        ]);

        held.release();
        await tick();
        app.draw(fixture('ready', language));
        await tick();
        expect(power()?.getAttribute('aria-busy')).toBeNull();
        expect(power()?.querySelector('.spinner')).toBeNull();
        expect(seenText(power())).toBe(t.status.powerOn);
        expect(states()).toEqual([
          [t.status.off, 'tone-off'],
          [t.status.off, 'tone-off'],
        ]);
      });

      it('a refused enable brings the label and the off state back', async () => {
        const start = fixture('ready', language);
        const held = heldApi(start, 'reject');
        const app = mountApp(root, held.api);
        app.draw(start);
        power()?.click();
        expect(power()?.querySelector('.spinner')).not.toBeNull();

        held.release();
        await tick();
        app.draw(fixture('ready', language));
        await tick();
        expect(power()?.getAttribute('aria-busy')).toBeNull();
        expect(power()?.querySelector('.spinner')).toBeNull();
        expect(power()?.disabled).toBe(false);
        expect(seenText(power())).toBe(t.status.powerOn);
        expect(states()).toEqual([
          [t.status.off, 'tone-off'],
          [t.status.off, 'tone-off'],
        ]);
      });
    });

    it('the spinner turns once in 0.8 to 1 s and stands still when motion is reduced', () => {
      const rules = [...(sheet?.sheet?.cssRules ?? [])];
      const spinnerRule = (list: CSSRule[]) =>
        list.find((rule) => 'selectorText' in rule && rule.selectorText === '.spinner')?.cssText ?? '';
      const turn = /animation:\s*spin\s+([\d.]+)s\s+linear\s+infinite/.exec(spinnerRule(rules));
      expect(Number(turn?.[1])).toBeGreaterThanOrEqual(0.8);
      expect(Number(turn?.[1])).toBeLessThanOrEqual(1);
      const reduced = rules.find(
        (rule): rule is CSSMediaRule =>
          'conditionText' in rule && rule.conditionText === '(prefers-reduced-motion: reduce)',
      );
      expect(spinnerRule([...(reduced?.cssRules ?? [])])).toMatch(/animation:\s*none/);
    });
  });
});

describe('the profile screen', () => {
  const byKey = <T extends HTMLElement = HTMLButtonElement>(key: string) =>
    root.querySelector<T>(`[data-key="${key}"]`);
  const tab = (kind: ProfileKind) => byKey(`profile-tab-${kind}`);
  const openTab = () => root.querySelector<HTMLElement>('[role="tab"][aria-selected="true"]')?.dataset.key;
  const badged = () => root.querySelector('.in-use')?.closest('[role="tab"]')?.getAttribute('data-key');
  const tones = () => [...root.querySelectorAll<HTMLButtonElement>('[data-intent="test_direction"]')];
  const describedBy = (element: Element | null) => element?.getAttribute('aria-describedby')?.split(' ') ?? [];
  /** The screen over a bridge that only records what it is asked to send. */
  function recording(start: Snapshot, shell: Partial<Api['shell']> = {}) {
    const sent: Intent[] = [];
    const inner = createMockApi(start);
    const api: Api = { ...inner, shell: { ...inner.shell, ...shell }, dispatch: async (intent) => void sent.push(intent) };
    const app = mountApp(root, api, { tab: 'profile' });
    app.draw(start);
    return { app, sent };
  }
  const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

  it('opens the tab of the kind in use and badges that kind', () => {
    draw('ready', 'profile');
    expect(openTab()).toBe('profile-tab-speakers');
    expect(badged()).toBe('profile-tab-speakers');
    expect(root.querySelector('#profile-panel-speakers')?.getAttribute('aria-labelledby')).toBe('profile-tab-speakers');
    expect(root.querySelector('#profile-panel-headphones')).toBeNull();
    expect(tab('speakers')?.getAttribute('aria-controls')).toBe('profile-panel-speakers');
    expect(tab('speakers')?.tabIndex).toBe(0);
    expect(tab('headphones')?.tabIndex).toBe(-1);

    draw('ready-headphones', 'profile');
    expect(openTab()).toBe('profile-tab-headphones');
    expect(badged()).toBe('profile-tab-headphones');
    expect(root.querySelectorAll('.in-use')).toHaveLength(1);
    expect(textOf()).toContain('지금 헤드폰으로 듣고 있습니다.');
    expect(textOf()).toContain(fixture('ready-headphones').profile.headphones.brir ?? 'no BRIR in fixture');
  });

  it('switches tabs on a click without sending anything, and keeps the tab across snapshots', () => {
    const { app, sent } = recording(fixture('ready'));
    tab('headphones')?.click();
    expect(openTab()).toBe('profile-tab-headphones');
    expect(root.querySelector('#profile-panel-headphones')).not.toBeNull();
    // The badge stays on the kind in use; only the view moved.
    expect(badged()).toBe('profile-tab-speakers');
    app.draw(fixture('ready'));
    expect(openTab()).toBe('profile-tab-headphones');
    tab('speakers')?.click();
    expect(openTab()).toBe('profile-tab-speakers');
    expect(sent).toEqual([]);
  });

  it('moves between the tabs with the arrow, Home and End keys', () => {
    const { sent } = recording(fixture('ready'));
    const press = (key: string) =>
      document.activeElement?.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }));
    tab('speakers')?.focus();
    press('ArrowRight');
    expect(openTab()).toBe('profile-tab-headphones');
    expect((document.activeElement as HTMLElement | null)?.dataset.key).toBe('profile-tab-headphones');
    press('ArrowRight');
    expect(openTab()).toBe('profile-tab-speakers');
    press('ArrowLeft');
    expect(openTab()).toBe('profile-tab-headphones');
    press('End');
    expect(openTab()).toBe('profile-tab-speakers');
    press('Home');
    expect(openTab()).toBe('profile-tab-headphones');
    expect((document.activeElement as HTMLElement | null)?.dataset.key).toBe('profile-tab-headphones');
    expect(sent).toEqual([]);
  });

  it('the switch buttons send the kind of their tab', () => {
    const headphones = recording(fixture('ready-headphones'));
    expect(byKey('use-headphones')).toBeNull();
    tab('speakers')?.click();
    expect(byKey('use-speakers')?.textContent).toBe('스피커로 듣기');
    expect(byKey('use-speakers')?.classList.contains('primary')).toBe(true);
    byKey('use-speakers')?.click();
    expect(headphones.sent).toEqual([{ kind: 'set_profile_kind', value: 'speakers' }]);

    const speakers = recording(fixture('ready-speakers'));
    expect(byKey('use-speakers')).toBeNull();
    expect(textOf()).toContain('지금 스피커로 듣고 있습니다.');
    tab('headphones')?.click();
    expect(byKey('use-headphones')?.disabled).toBe(false);
    byKey('use-headphones')?.click();
    expect(speakers.sent).toEqual([{ kind: 'set_profile_kind', value: 'headphones' }]);
  });

  it('says it is switching while the kind is in flight', () => {
    const start = fixture('ready-headphones');
    const api = createMockApi(start, { hold: true });
    mountApp(root, api, { tab: 'profile' }).draw(start);
    tab('speakers')?.click();
    byKey('use-speakers')?.click();
    expect(byKey('use-speakers')?.textContent).toBe('바꾸는 중');
    expect(byKey('use-speakers')?.getAttribute('aria-busy')).toBe('true');
    expect(byKey('use-speakers')?.disabled).toBe(true);
    expect(byKey('speaker-layout')?.disabled).toBe(true);
    expect(byKey('correction-import')?.disabled).toBe(true);
    // A tab is view state; it still opens.
    expect(tab('headphones')?.disabled).toBe(false);
  });

  it('keeps headphones off until a BRIR set is in, and says why', () => {
    draw('ready', 'profile');
    tab('headphones')?.click();
    const use = byKey('use-headphones');
    expect(use?.disabled).toBe(true);
    expect(describedBy(use)).toContain('use-headphones-hint');
    expect(root.querySelector('#use-headphones-hint')?.textContent).toBe('먼저 BRIR 세트를 가져오십시오.');
    expect(root.querySelector('.tab-facts')?.textContent).toBe('BRIR 세트없음');
    expect(byKey('brir-folder')?.disabled).toBe(false);
    expect(byKey('brir-file')?.disabled).toBe(false);
  });

  it('lists the nine layouts in contract order and sends the one chosen', () => {
    const { sent } = recording(fixture('ready'));
    const select = byKey<HTMLSelectElement>('speaker-layout');
    if (select === null) throw new Error('no layout select');
    expect(root.querySelector(`label[for="${select.id}"]`)?.textContent).toBe('스피커 배치');
    expect([...select.options].map((option) => option.value)).toEqual(Object.keys(SPEAKER_LAYOUTS));
    expect(select.querySelector('option[selected]')?.getAttribute('value')).toBe('2.0');
    select.value = '5.1.4';
    select.dispatchEvent(new Event('change'));
    expect(sent).toEqual([{ kind: 'set_speaker_layout', value: '5.1.4' }]);
  });

  it('shows the stored layout of the kind not in use', () => {
    draw('ready-headphones', 'profile');
    tab('speakers')?.click();
    const select = byKey<HTMLSelectElement>('speaker-layout');
    expect(select?.querySelector('option[selected]')?.getAttribute('value')).toBe('7.1');
    expect(select?.disabled).toBe(false);
    const rows = [...root.querySelectorAll('.channel-table tbody tr')].map((row) => row.textContent);
    expect(rows).toEqual(['1L', '2R', '3C', '4LFE서브우퍼', '5Lrs', '6Rrs', '7Lss', '8Rss']);
    expect(byKey('correction-clear')).toBeNull();
    expect(textOf()).toContain('파일없음');
  });

  it('picks a CamillaDSP configuration and sends import_correction', async () => {
    const picked: unknown[][] = [];
    const path = 'C:\\SECS\\configs\\secs_mixed.yml';
    const { sent } = recording(fixture('ready'), {
      pickFile: async (...args) => {
        picked.push(args);
        return path;
      },
    });
    byKey('correction-import')?.click();
    await settle();
    expect(picked).toEqual([['SECS 2R 보정 파일', { name: 'CamillaDSP 설정', extensions: ['yml', 'yaml'] }]]);
    expect(sent).toEqual([{ kind: 'import_correction', value: path }]);
  });

  it('lets a correction decide the speakers, locks the layout and clears it', () => {
    const start = fixture('ready-speakers');
    const correction = start.profile.speakers.correction;
    if (correction === null) throw new Error('ready-speakers has a correction');
    const { sent } = recording(start);
    const select = byKey<HTMLSelectElement>('speaker-layout');
    expect(select?.disabled).toBe(true);
    expect(describedBy(select)).toContain('layout-hint');
    expect(root.querySelector('#layout-hint')?.textContent).toBe(
      '보정 파일이 스피커를 정합니다. 보정을 지우면 이 배치로 돌아갑니다.',
    );
    expect(textOf()).toContain(correction.path);
    expect(textOf()).toContain(correction.title);
    const numbers = [...root.querySelectorAll('.channel-table tbody td')].map((cell) => cell.textContent);
    expect(numbers).toEqual(start.profile.speakers.channels.map((channel) => String(channel.channel + 1)));
    const subwoofers = [...root.querySelectorAll('.channel-table tbody th')]
      .filter((cell) => cell.querySelector('.tag') !== null)
      .map((cell) => cell.firstChild?.textContent);
    expect(subwoofers).toEqual(['SW1', 'SW2']);
    byKey('correction-clear')?.click();
    expect(sent).toEqual([{ kind: 'clear_correction' }]);
  });

  it('locks every kind, import, layout and correction control while profile.toml wins', () => {
    const start = fixture('ready-profile-file');
    recording(start);
    const note = root.querySelector('.file-note');
    expect(root.querySelectorAll('.file-note')).toHaveLength(1);
    expect(note?.querySelector('.tag')?.textContent).toBe('profile.toml');
    expect(note?.textContent).toContain(start.profile.name);
    expect(root.querySelector('#profile-file-hint')?.textContent).toBe(
      '상태 폴더의 profile.toml이 우선합니다. 그 파일을 지우거나 고치십시오.',
    );
    // The badge marks the file's kind.
    expect(badged()).toBe('profile-tab-headphones');
    for (const key of ['brir-folder', 'brir-file']) {
      expect(byKey(key)?.disabled, key).toBe(true);
      expect(describedBy(byKey(key)), key).toContain('profile-file-hint');
    }
    tab('speakers')?.click();
    for (const key of ['use-speakers', 'speaker-layout', 'correction-import']) {
      expect(byKey(key)?.disabled, key).toBe(true);
      expect(describedBy(byKey(key)), key).toContain('profile-file-hint');
    }
    // The gain is still the listener's.
    expect(byKey<HTMLInputElement>('gain')?.disabled).toBe(false);

    // A speaker profile.toml lays the speakers out its own way.
    const speakerFile = fixture('ready-profile-file');
    speakerFile.profile.kind = 'speakers';
    speakerFile.profile.speakers.layout = null;
    speakerFile.profile.speakers.correction = { path: 'C:\\SECS\\a.yml', title: 'A' };
    recording(speakerFile);
    expect(badged()).toBe('profile-tab-speakers');
    expect(byKey<HTMLSelectElement>('speaker-layout')?.querySelector('option[selected]')?.textContent).toBe(
      'profile.toml의 배치',
    );
    expect(byKey('speaker-layout')?.disabled).toBe(true);
    expect(byKey('correction-clear')?.disabled).toBe(true);
    tab('headphones')?.click();
    expect(byKey('use-headphones')?.disabled).toBe(true);
    expect(root.querySelector('#use-headphones-hint')).toBeNull();

    // Without the file the same controls are free.
    draw('ready-headphones', 'profile');
    expect(root.querySelector('.file-note')).toBeNull();
    expect(byKey('brir-folder')?.disabled).toBe(false);
    tab('speakers')?.click();
    for (const key of ['use-speakers', 'speaker-layout', 'correction-import']) expect(byKey(key)?.disabled, key).toBe(false);
  });

  it('says once, near the footer, that a change while on takes seconds', () => {
    draw('running', 'profile');
    expect(root.querySelectorAll('#profile-running-hint')).toHaveLength(1);
    expect(root.querySelector('.tab-footer #profile-running-hint')?.textContent).toBe(
      '켜져 있을 때 바꾸면 엔진을 다시 준비하므로 몇 초 걸립니다.',
    );
    expect(describedBy(byKey('brir-folder'))).toContain('profile-running-hint');
    tab('speakers')?.click();
    expect(root.querySelectorAll('#profile-running-hint')).toHaveLength(1);
    expect(describedBy(byKey('use-speakers'))).toContain('profile-running-hint');
    draw('ready', 'profile');
    expect(root.querySelector('#profile-running-hint')).toBeNull();
  });

  it('imports a BRIR set without switching, then switches to headphones', async () => {
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
        return 'C:\\BRIR\\studio\\hesuvi.wav';
      },
    };
    const app = mountApp(root, { ...api, shell }, { tab: 'profile' });
    app.draw(start);
    await api.onSnapshot(app.draw);
    tab('headphones')?.click();

    byKey('brir-folder')?.click();
    await vi.waitFor(() => expect(textOf()).toContain('C:\\BRIR\\living-room'));
    await vi.waitFor(() => expect(byKey('brir-file')?.disabled).toBe(false));
    byKey('brir-file')?.click();
    await vi.waitFor(() => expect(textOf()).toContain('C:\\BRIR\\studio\\hesuvi.wav'));
    expect(picked).toEqual(['BRIR 세트 폴더', 'hesuvi.wav 또는 hrir.wav (wav)']);
    // Still the speakers: an import never switches the kind.
    expect(badged()).toBe('profile-tab-speakers');
    expect(tones()).toHaveLength(2);

    await vi.waitFor(() => expect(byKey('use-headphones')?.disabled).toBe(false));
    byKey('use-headphones')?.click();
    await vi.waitFor(() => expect(badged()).toBe('profile-tab-headphones'));
    expect(textOf()).toContain('지금 헤드폰으로 듣고 있습니다.');
    expect(tones()).toHaveLength(fixture('running').profile.directions.length);
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
    expect(tones().map((tone) => tone.textContent)).toEqual(['L', 'R']);
    expect(tones().every((tone) => tone.disabled)).toBe(true);
    expect(root.querySelector('#test-hint')?.textContent).toBe('켠 뒤에 시험음을 낼 수 있습니다.');
    draw('running', 'profile');
    expect(root.querySelector('#test-hint')).toBeNull();
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
    expect(names).toEqual(['L', 'R']);
    expect(textOf()).toContain('켜면 소리 객체가 보입니다.');
    app.updateMeters(runningEngine());
    expect(shown()).toHaveLength(0);
  });
});

describe('the mock supervisor follows the profile rules', () => {
  const runningScene = () => fixture('running').engine?.scene ?? [];

  const fromFile = '상태 폴더의 profile.toml이 우선합니다. 그 파일을 지우거나 고치십시오.';

  it('needs a BRIR set before headphones, and an import never switches the kind', async () => {
    const api = createMockApi(fixture('ready'));
    await expect(api.dispatch({ kind: 'set_profile_kind', value: 'headphones' })).rejects.toMatchObject({
      message: '먼저 BRIR 세트를 가져오십시오.',
    });
    expect((await api.snapshot()).profile.kind).toBe('speakers');
    await api.dispatch({ kind: 'import_brir', value: 'C:\\BRIR\\living-room' });
    let { profile } = await api.snapshot();
    expect(profile).toMatchObject({ kind: 'speakers', name: '2.0', headphones: { brir: 'C:\\BRIR\\living-room' } });
    await api.dispatch({ kind: 'set_profile_kind', value: 'headphones' });
    ({ profile } = await api.snapshot());
    expect(profile).toMatchObject({ kind: 'headphones', name: 'living-room', from_file: false });
    expect(profile.directions).toEqual(fixture('running').profile.directions);
    // The speakers keep their settings behind the headphones.
    expect(profile.speakers).toEqual(fixture('ready').profile.speakers);
  });

  it('keeps a gain for each kind', async () => {
    const api = createMockApi(fixture('ready'));
    await api.dispatch({ kind: 'set_gain', value: -6 });
    await api.dispatch({ kind: 'import_brir', value: 'C:\\BRIR\\living-room' });
    await api.dispatch({ kind: 'set_profile_kind', value: 'headphones' });
    expect((await api.snapshot()).profile.gain_db).toBe(0);
    await api.dispatch({ kind: 'set_gain', value: 4 });
    await api.dispatch({ kind: 'set_profile_kind', value: 'speakers' });
    expect((await api.snapshot()).profile.gain_db).toBe(-6);
    await api.dispatch({ kind: 'set_profile_kind', value: 'headphones' });
    expect((await api.snapshot()).profile.gain_db).toBe(4);
  });

  it('changes the stored layout live only while speakers are in use', async () => {
    const start = fixture('running');
    const api = createMockApi(start);
    await api.dispatch({ kind: 'set_speaker_layout', value: '5.1' });
    let snapshot = await api.snapshot();
    expect(snapshot.profile.kind).toBe('headphones');
    expect(snapshot.profile.speakers.layout).toBe('5.1');
    expect(snapshot.profile.speakers.channels.map((channel) => channel.name)).toEqual(['L', 'R', 'C', 'LFE', 'Ls', 'Rs']);
    expect(snapshot.profile.directions).toEqual(start.profile.directions);
    expect(snapshot.engine?.directions).toEqual(start.engine?.directions);

    await api.dispatch({ kind: 'set_profile_kind', value: 'speakers' });
    snapshot = await api.snapshot();
    expect(snapshot.profile.name).toBe('5.1');
    expect(snapshot.profile.directions.map((direction) => [direction.name, direction.azimuth_deg])).toEqual([
      ['L', 30],
      ['R', -30],
      ['C', 0],
      ['Ls', 110],
      ['Rs', -110],
    ]);
    expect(snapshot.engine?.directions).toEqual(snapshot.profile.directions);
  });

  it('imports a correction that decides the speakers, and clears it back to the layout', async () => {
    const secs = fixture('ready-speakers').profile;
    const api = createMockApi(fixture('ready'));
    const path = 'C:\\SECS\\configs\\secs_mixed.yml';
    await api.dispatch({ kind: 'import_correction', value: path });
    let { profile } = await api.snapshot();
    expect(profile.speakers).toEqual({ layout: '2.0', correction: { path, title: secs.name }, channels: secs.speakers.channels });
    expect(profile.name).toBe(secs.name);
    expect(profile.directions).toEqual(secs.directions);

    await api.dispatch({ kind: 'clear_correction' });
    ({ profile } = await api.snapshot());
    expect(profile).toMatchObject({ name: '2.0', speakers: fixture('ready').profile.speakers });

    await expect(api.dispatch({ kind: 'import_correction', value: 'C:\\SECS\\filters\\L.wav' })).rejects.toMatchObject({
      message: '보정 파일을 읽지 못했습니다: L.wav',
    });
  });

  it('keeps a correction for speakers while headphones play on', async () => {
    const start = fixture('running');
    const api = createMockApi(start);
    await api.dispatch({ kind: 'import_correction', value: 'C:\\SECS\\configs\\secs_mixed.yml' });
    const snapshot = await api.snapshot();
    expect(snapshot.profile).toMatchObject({ kind: 'headphones', name: start.profile.name });
    expect(snapshot.profile.speakers.correction?.path).toBe('C:\\SECS\\configs\\secs_mixed.yml');
    expect(snapshot.engine?.directions).toEqual(start.engine?.directions);
  });

  it('refuses every kind, import, layout and correction intent while profile.toml wins', async () => {
    const start = fixture('ready-profile-file');
    const api = createMockApi(start);
    const intents: Intent[] = [
      { kind: 'set_profile_kind', value: 'speakers' },
      { kind: 'import_brir', value: 'C:\\BRIR\\living-room' },
      { kind: 'set_speaker_layout', value: '5.1' },
      { kind: 'import_correction', value: 'C:\\SECS\\configs\\secs_mixed.yml' },
      { kind: 'clear_correction' },
    ];
    for (const intent of intents) {
      await expect(api.dispatch(intent), intent.kind).rejects.toMatchObject({ message: fromFile });
    }
    expect((await api.snapshot()).profile).toEqual(start.profile);
    await api.dispatch({ kind: 'set_gain', value: -2 });
    expect((await api.snapshot()).profile.gain_db).toBe(-2);
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
