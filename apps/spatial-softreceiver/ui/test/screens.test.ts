import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Api } from '../src/api';
import { mountApp } from '../src/app';
import type {
  EndpointUpmixView,
  EngineStatus,
  Intent,
  ProfileKind,
  Snapshot,
  SourceFormat,
  UpmixBypass,
  UpmixState,
} from '../src/bindings';
import { fixture, fixtureNames } from '../src/fixtures';
import { SPEAKER_LAYOUTS, assertIntent } from '../src/guard';
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
    for (const tab of ['status', 'profile', 'upmix', 'settings'] as const) {
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
    expect(text).toContain('이 채널 수에 맞는 스피커 위치가 정해져 있지 않습니다.');
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
    expect(textOf()).toContain(ko.profile.usingSpeakers);
    expect(textOf()).toContain(ko.profile.mapTitle);
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
    expect(buttons()).toContain('진단 기록 시작');
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
    expect(textOf()).toContain(ko.profile.usingHeadphones);
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
    expect(byKey('use-speakers')?.textContent).toBe(ko.profile.useSpeakers);
    expect(byKey('use-speakers')?.classList.contains('primary')).toBe(true);
    byKey('use-speakers')?.click();
    expect(headphones.sent).toEqual([{ kind: 'set_profile_kind', value: 'speakers' }]);

    const speakers = recording(fixture('ready-speakers'));
    expect(byKey('use-speakers')).toBeNull();
    expect(textOf()).toContain(ko.profile.usingSpeakers);
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
    expect(root.querySelector('#layout-hint')?.textContent).toBe(ko.profile.layoutByCorrection);
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
    expect(root.querySelector('.tab-footer #profile-running-hint')?.textContent).toBe(ko.profile.importRunning);
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
    expect(textOf()).toContain(ko.profile.usingHeadphones);
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
    expect(root.querySelector('#test-hint')?.textContent).toBe('켠 뒤에 시험용 신호를 재생할 수 있습니다.');
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
    expect(textOf()).toContain(`소리 위치 ${scene.length}곳`);
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
    expect(textOf()).toContain(`소리 위치 ${engine.scene.length}곳`);
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
    expect(textOf()).toContain('켜면 소리의 위치가 보입니다.');
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
      message: '출력 음량 조절 범위는 -20 dB에서 +10 dB까지입니다.',
    });
    expect((await api.snapshot()).profile.gain_db).toBe(-6);
  });

  it('plays a test tone only while running', async () => {
    const api = createMockApi(fixture('running'));
    await api.dispatch({ kind: 'test_direction', value: 3 });
    await expect(api.dispatch({ kind: 'test_direction', value: 99 })).rejects.toMatchObject({
      message: '선택한 방향을 찾을 수 없습니다.',
    });
    await api.dispatch({ kind: 'disable' });
    await expect(api.dispatch({ kind: 'test_direction', value: 0 })).rejects.toEqual({
      message: '켠 뒤에 시험용 신호를 재생할 수 있습니다.',
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

describe('the upmix screen', () => {
  const byKey = <T extends HTMLElement = HTMLButtonElement>(key: string) =>
    root.querySelector<T>(`[data-key="${key}"]`);
  const radio = (endpoint: number, choice: string) => byKey<HTMLInputElement>(`upmixer-${endpoint}-${choice}`);
  const cards = () => [...root.querySelectorAll<HTMLElement>('.upmix-card')];
  const card = (endpoint: number) => root.querySelector<HTMLElement>(`.upmix-card[data-endpoint="${endpoint}"]`);
  const stateOf = (endpoint: number) => card(endpoint)?.querySelector('.upmix-state');
  const describedBy = (element: Element | null) => element?.getAttribute('aria-describedby')?.split(' ') ?? [];
  const settle = () => new Promise((resolve) => setTimeout(resolve, 0));
  /** The screen over a bridge that only records what it is asked to send. */
  function recording(start: Snapshot, shell: Partial<Api['shell']> = {}, tab: Tab = 'upmix') {
    const sent: Intent[] = [];
    const inner = createMockApi(start);
    const api: Api = { ...inner, shell: { ...inner.shell, ...shell }, dispatch: async (intent) => void sent.push(intent) };
    const app = mountApp(root, api, { tab });
    app.draw(start);
    return { app, sent };
  }
  /** `name` with the music endpoint's upmixer changed. */
  function music(name: string, patch: Partial<EndpointUpmixView>): Snapshot {
    const snapshot = fixture(name);
    Object.assign(snapshot.upmix.endpoints[1]!, patch);
    return snapshot;
  }
  /** `name` with the PASS endpoint's upmixer changed. */
  function pass(name: string, patch: Partial<EndpointUpmixView>): Snapshot {
    const snapshot = fixture(name);
    Object.assign(snapshot.upmix.endpoints[2]!, patch);
    return snapshot;
  }

  it('sits between the profile and the settings in the navigation', () => {
    draw('ready');
    const items = [...root.querySelectorAll<HTMLButtonElement>('.nav-item')].map((item) => item.dataset.key);
    expect(items).toEqual(['nav-status', 'nav-profile', 'nav-upmix', 'nav-settings']);
    root.querySelector<HTMLButtonElement>('[data-key="nav-upmix"]')?.click();
    expect(root.querySelector('h1')?.textContent).toBe('업믹스');
    expect(root.querySelector('[aria-current="page"]')?.textContent).toBe('업믹스');
    // The intro, then its (i) with the rule of what is upmixed.
    expect(root.querySelector('.upmix-intro')?.textContent).toBe(`${ko.upmix.intro}\u00a0${ko.upmix.infoTip}`);
  });

  it('draws one card per endpoint, main, music and PASS, in order, by its name and purpose', () => {
    draw('ready', 'upmix');
    expect(cards().map((element) => element.dataset.endpoint)).toEqual(['0', '1', '2']);
    expect(cards().map((element) => element.querySelector('h2')?.textContent)).toEqual([
      '공간 소프트리시버',
      '공간 소프트리시버 음악',
      '공간 소프트리시버 PASS',
    ]);
    // The purposes the status screen gives the same endpoints.
    expect(cards().map((element) => element.querySelector('.upmix-card-head p')?.textContent)).toEqual(
      ko.endpoints.map((endpoint) => endpoint.purpose),
    );
    for (const element of cards()) {
      const title = element.querySelector('h2');
      expect(element.getAttribute('aria-labelledby')).toBe(title?.id);
      const group = element.querySelector('[role="radiogroup"]');
      expect(group?.getAttribute('aria-labelledby')?.split(' ')).toContain(title?.id);
      expect([...(group?.querySelectorAll('input[type="radio"]') ?? [])].map((input) => (input as HTMLInputElement).value)).toEqual([
        'first_party',
        'plugin',
        'off',
      ]);
    }
  });

  it('shows the first-party upmixer locked, tagged not built, and says why', () => {
    draw('ready', 'upmix');
    for (const endpoint of [0, 1, 2]) {
      const first = radio(endpoint, 'first_party');
      expect(first?.disabled).toBe(true);
      const label = first?.closest('label');
      expect(label?.querySelector('.tag')?.textContent).toBe('미구현');
      // Its name is the head of the row, the tag included; the line under it describes it.
      expect(root.querySelector(`#${first?.getAttribute('aria-labelledby')}`)?.textContent).toBe('앱 자체 업믹서미구현');
      const hint = root.querySelector(`#${describedBy(first)[0]}`);
      expect(hint?.textContent).toContain(ko.upmix.firstPartyHint);
      expect(radio(endpoint, 'plugin')?.disabled).toBe(false);
      expect(radio(endpoint, 'off')?.disabled).toBe(false);
    }
    // Once it is built, it is an upmixer like the others.
    const built = fixture('ready');
    built.upmix.first_party_available = true;
    recording(built);
    expect(radio(0, 'first_party')?.disabled).toBe(false);
    expect(root.querySelector('.upmix-option .tag')).toBeNull();
    expect(root.querySelector(`#${describedBy(radio(0, 'first_party'))[0]}`)?.textContent).toBe(ko.upmix.firstPartyBody);
  });

  it('checks what the choice resolves to and marks the default when nothing was chosen', () => {
    draw('ready', 'upmix');
    for (const endpoint of [0, 1, 2]) {
      expect(radio(endpoint, 'off')?.checked).toBe(true);
      expect(radio(endpoint, 'off')?.closest('label')?.querySelector('.upmix-default')?.textContent).toBe('기본값');
    }
    expect(root.querySelectorAll('.upmix-default')).toHaveLength(3);

    draw('ready-upmix', 'upmix');
    expect(radio(0, 'off')?.checked).toBe(true);
    expect(radio(1, 'plugin')?.checked).toBe(true);
    expect(radio(1, 'plugin')?.closest('label')?.classList.contains('is-checked')).toBe(true);
    // Both were chosen: neither follows the default.
    expect(root.querySelector('.upmix-default')).toBeNull();
  });

  it('sends set_upmixer with the endpoint and the upmixer of the radio chosen', () => {
    const choose = (start: Snapshot, endpoint: number, choice: string) => {
      const { sent } = recording(start);
      radio(endpoint, choice)?.click();
      return sent;
    };
    expect(choose(fixture('ready'), 1, 'plugin')).toEqual([{ kind: 'set_upmixer', value: { endpoint: 1, choice: 'plugin' } }]);
    expect(choose(fixture('ready'), 0, 'plugin')).toEqual([{ kind: 'set_upmixer', value: { endpoint: 0, choice: 'plugin' } }]);
    // PASS chooses as the others do: bitstream stereo is upmixed when it has an upmixer.
    expect(choose(fixture('ready'), 2, 'plugin')).toEqual([{ kind: 'set_upmixer', value: { endpoint: 2, choice: 'plugin' } }]);
    expect(choose(fixture('running-upmix-pass'), 2, 'off')).toEqual([{ kind: 'set_upmixer', value: { endpoint: 2, choice: 'off' } }]);
    expect(choose(fixture('running-upmix-pass'), 2, 'first_party')).toEqual([]);
    expect(choose(fixture('ready-upmix'), 1, 'off')).toEqual([{ kind: 'set_upmixer', value: { endpoint: 1, choice: 'off' } }]);
    // The option already checked sends nothing; the locked one cannot be chosen.
    expect(choose(fixture('ready-upmix'), 1, 'plugin')).toEqual([]);
    expect(choose(fixture('ready'), 0, 'first_party')).toEqual([]);
  });

  it('shows the plugin block only for a VST upmixer, with the plugin as Rust names it', () => {
    draw('ready', 'upmix');
    expect(root.querySelector('.upmix-plugin')).toBeNull();
    draw('ready-upmix', 'upmix');
    expect(card(0)?.querySelector('.upmix-plugin')).toBeNull();
    const block = card(1)?.querySelector('.upmix-plugin');
    const plugin = fixture('ready-upmix').upmix.endpoints[1]?.plugin;
    if (plugin === null || plugin === undefined) throw new Error('ready-upmix has a plugin on the music endpoint');
    const rows = [...(block?.querySelectorAll('.fact') ?? [])].map((row) => [
      row.querySelector('dt')?.textContent,
      row.querySelector('dd')?.textContent,
    ]);
    expect(rows).toEqual([
      ['이름', plugin.name],
      ['제작사', plugin.vendor],
      ['형식', 'VST 3'],
      ['입력', 'L R'],
      ['경로', plugin.path],
    ]);
    expect(block?.querySelector('dd .path')?.textContent).toBe(plugin.path);

    recording(music('ready-upmix', { plugin: { ...plugin, format: 'vst2', path: 'C:\\VST\\Upmixer.dll' } }));
    expect(card(1)?.textContent).toContain('VST 2');
  });

  it('asks for a plugin when the VST upmixer has none, and offers no editor', () => {
    recording(music('ready', { choice: 'plugin', effective: 'plugin' }));
    const block = card(1)?.querySelector('.upmix-plugin');
    expect(block?.querySelector('.upmix-no-plugin')?.textContent).toBe('고른 플러그인이 없습니다.');
    expect(block?.textContent).toContain('먼저 업믹스 플러그인을 고르십시오.');
    expect(block?.querySelector('dl')).toBeNull();
    expect(byKey('plugin-editor-1')).toBeNull();
    for (const key of ['plugin-folder-1', 'plugin-file-1']) {
      expect(byKey(key)?.disabled, key).toBe(false);
      expect(describedBy(byKey(key)), key).toContain('plugin-1-needed');
    }
    expect(stateOf(1)?.textContent).toBe('쓰지 않음');
  });

  it.each([
    ['plugin-folder-1', 'C:\\Program Files\\Common Files\\VST3\\Wide.vst3', ['VST 3 묶음 폴더']],
    [
      'plugin-file-1',
      'C:\\VSTPlugins\\Wide.dll',
      ['VST 3 또는 VST 2 플러그인 파일', { name: 'VST 플러그인', extensions: ['vst3', 'dll'] }],
    ],
  ] as const)('%s sends choose_plugin with the path picked', async (key, path, dialog) => {
    const picked: unknown[][] = [];
    const pick = async (...args: unknown[]) => {
      picked.push(args);
      return path;
    };
    const { sent } = recording(fixture('ready-upmix'), { pickFolder: pick, pickFile: pick });
    expect(byKey('plugin-folder-1')?.textContent).toBe('VST 3 묶음 폴더 고르기');
    expect(byKey('plugin-file-1')?.textContent).toBe('플러그인 파일 고르기');
    byKey(key)?.click();
    await settle();
    expect(picked).toEqual([dialog]);
    expect(sent).toEqual([{ kind: 'choose_plugin', value: { endpoint: 1, path } }]);
  });

  it('a cancelled picker sends nothing', async () => {
    const { sent } = recording(fixture('ready-upmix'), { pickFolder: async () => null, pickFile: async () => null });
    byKey('plugin-folder-1')?.click();
    byKey('plugin-file-1')?.click();
    await settle();
    expect(sent).toEqual([]);
  });

  it.each<[string, UpmixState['kind'], boolean]>([
    ['ready-upmix', 'stopped', false],
    ['running-upmix', 'running', true],
    ['running-upmix-fallback', 'fallback', true],
    ['ready-upmix-failed', 'failed', false],
  ])('%s: the editor opens only while the host is up (%s)', (name, kind, open) => {
    const { sent } = recording(fixture(name));
    expect(fixture(name).upmix.endpoints[1]?.state.kind).toBe(kind);
    const editor = byKey('plugin-editor-1');
    expect(editor?.textContent).toBe('편집기 열기');
    expect(editor?.disabled).toBe(!open);
    const hint = root.querySelector('#plugin-1-editor-hint');
    if (open) {
      expect(hint).toBeNull();
      editor?.click();
      expect(sent).toEqual([{ kind: 'open_plugin_editor', value: 1 }]);
    } else {
      expect(describedBy(editor)).toContain('plugin-1-editor-hint');
      expect(hint?.textContent).toBe(ko.upmix.editorHint);
    }
  });

  it('keeps the editor closed while the host starts', () => {
    recording(music('running-upmix', { state: { kind: 'starting' }, latency_ms: null }));
    expect(byKey('plugin-editor-1')?.disabled).toBe(true);
  });

  describe('the state line', () => {
    it('stopped: off, or starting when turned on, or not applied to the kind in use', () => {
      draw('ready', 'upmix');
      expect([stateOf(0)?.textContent, stateOf(1)?.textContent]).toEqual(['쓰지 않음', '쓰지 않음']);
      expect(stateOf(0)?.classList.contains('tone-off')).toBe(true);

      draw('ready-upmix', 'upmix');
      expect(stateOf(0)?.textContent).toBe('쓰지 않음');
      expect(stateOf(1)?.textContent).toBe('켜면 시작합니다');

      // Speakers are in use and do not apply upmixing: it still starts when
      // turned on, and the sound will go around it, as the line says.
      draw('ready-speakers-upmix-off', 'upmix');
      expect(stateOf(1)?.querySelector('.upmix-state-word')?.textContent).toBe('켜면 시작합니다');
      expect(stateOf(1)?.querySelector('.upmix-state-reason')?.textContent).toBe('지금 쓰는 출력에는 적용하지 않음');
      expect(stateOf(1)?.classList.contains('tone-off')).toBe(true);
      expect(stateOf(1)?.classList.contains('is-alert')).toBe(false);

      // Running, but stopped: whatever the switch says, it is just off.
      recording(music('running-upmix', { state: { kind: 'stopped' }, latency_ms: null }));
      expect(stateOf(1)?.textContent).toBe('쓰지 않음');
      const off = fixture('running-upmix');
      off.upmix.headphones = false;
      Object.assign(off.upmix.endpoints[1]!, { state: { kind: 'stopped' }, latency_ms: null });
      recording(off);
      expect(stateOf(1)?.textContent).toBe('쓰지 않음');
    });

    it('starting turns the spinner', () => {
      recording(music('running-upmix', { state: { kind: 'starting' }, latency_ms: null }));
      const line = stateOf(1);
      expect(line?.textContent).toBe('시작하는 중');
      expect(line?.querySelector('.spinner')?.getAttribute('aria-hidden')).toBe('true');
      expect(line?.querySelector('.lamp')).toBeNull();
    });

    it('running names the latency to one decimal and the outputs', () => {
      draw('running-upmix', 'upmix');
      const line = stateOf(1);
      expect(line?.classList.contains('tone-on')).toBe(true);
      expect(line?.querySelector('.upmix-state-word')?.textContent).toBe('실행 중');
      expect([...(line?.querySelectorAll('.upmix-state-detail') ?? [])].map((detail) => detail.textContent)).toEqual([
        '지연 23.3\u00a0ms',
        '출력 L R C LFE Ls Rs',
      ]);
      recording(music('running-upmix', { latency_ms: 21 }));
      expect(stateOf(1)?.textContent).toContain('지연 21.0\u00a0ms');
    });

    it('fallback says the original stereo plays, in the warning tone', () => {
      draw('running-upmix-fallback', 'upmix');
      const line = stateOf(1);
      expect(line?.textContent).toBe('플러그인이 응답하지 않아 원래 스테레오를 재생합니다.');
      expect(line?.classList.contains('tone-warning')).toBe(true);
      expect(line?.classList.contains('is-alert')).toBe(true);
    });

    it('failed shows the supervisor sentence as it is, in the error tone', () => {
      draw('ready-upmix-failed', 'upmix');
      const failed = fixture('ready-upmix-failed').upmix.endpoints[1]?.state;
      if (failed?.kind !== 'failed') throw new Error('ready-upmix-failed has a failed music endpoint');
      expect(stateOf(1)?.textContent).toBe(failed.value);
      expect(stateOf(1)?.classList.contains('tone-error')).toBe(true);
      const english = fixture('ready-upmix-failed', 'en');
      recording(english);
      const sentence = english.upmix.endpoints[1]?.state;
      expect(sentence?.kind === 'failed' && stateOf(1)?.textContent === sentence.value).toBe(true);
    });
  });

  it('locks every upmix control while an intent is in flight, and shows the choice it sends', () => {
    const start = fixture('ready-upmix');
    const api = createMockApi(start, { hold: true });
    mountApp(root, api, { tab: 'upmix' }).draw(start);
    radio(1, 'off')?.click();
    for (const choice of ['first_party', 'plugin', 'off']) {
      for (const endpoint of [0, 1]) expect(radio(endpoint, choice)?.disabled, `${endpoint} ${choice}`).toBe(true);
    }
    expect(radio(1, 'off')?.checked).toBe(true);
    const group = card(1)?.querySelector('[role="radiogroup"]');
    expect(group?.getAttribute('aria-busy')).toBe('true');
    expect(card(0)?.querySelector('[role="radiogroup"]')?.hasAttribute('aria-busy')).toBe(false);
    expect(root.querySelector('#upmixer-1-applying')?.textContent).toBe(ko.profile.applying);
    expect(describedBy(radio(1, 'plugin'))).toContain('upmixer-1-applying');
    // The plugin block stays until the snapshot says otherwise; its buttons wait too.
    for (const key of ['plugin-folder-1', 'plugin-file-1', 'plugin-editor-1']) expect(byKey(key)?.disabled, key).toBe(true);
  });

  it('says the plugin is being checked while choose_plugin is in flight', async () => {
    const start = fixture('ready-upmix');
    const api = createMockApi(start, { hold: true });
    mountApp(root, api, { tab: 'upmix' }).draw(start);
    byKey('plugin-file-1')?.click();
    await vi.waitFor(() => expect(root.querySelector('#plugin-1-scanning')?.textContent).toBe(ko.upmix.scanning));
    expect(byKey('plugin-file-1')?.disabled).toBe(true);
    expect(describedBy(byKey('plugin-folder-1'))).toContain('plugin-1-scanning');
  });

  it('says the editor is opening on its own button only', () => {
    const start = fixture('running-upmix');
    Object.assign(start.upmix.endpoints[0]!, structuredClone(start.upmix.endpoints[1]!), { endpoint: 0 });
    const api = createMockApi(start, { hold: true });
    mountApp(root, api, { tab: 'upmix' }).draw(start);
    byKey('plugin-editor-1')?.click();
    expect(byKey('plugin-editor-1')?.textContent).toBe('여는 중');
    expect(byKey('plugin-editor-1')?.getAttribute('aria-busy')).toBe('true');
    expect(byKey('plugin-editor-0')?.textContent).toBe('편집기 열기');
    expect(byKey('plugin-editor-0')?.hasAttribute('aria-busy')).toBe(false);
    expect(byKey('plugin-editor-0')?.disabled).toBe(true);
  });

  it('has no choice of how stereo arrives, on any card', () => {
    for (const name of ['ready-upmix', 'running-upmix', 'running-upmix-pass', 'running-upmix-native-surround']) {
      draw(name, 'upmix');
      expect(root.querySelectorAll('input[type="checkbox"]'), name).toHaveLength(0);
      expect(root.querySelector('[role="group"]'), name).toBeNull();
      expect(root.querySelector('[data-intent="set_upmix_sources"]'), name).toBeNull();
      expect(textOf(), name).not.toContain('비트스트림 스테레오');
    }
  });

  describe('the delay under the choices', () => {
    const delayOf = (endpoint: number) => root.querySelector(`#upmixer-${endpoint}-delay`);

    it('says what upmixing costs the input while an upmixer is in effect and its delay is known', () => {
      draw('running-upmix', 'upmix');
      expect(delayOf(1)?.textContent).toBe('업믹스하면 이 입력의 소리가 약 23.3\u00a0ms 늦어집니다.');
      expect(delayOf(1)?.closest('.upmix-section')?.querySelector('[role="radiogroup"]')).not.toBeNull();
      expect(card(1)?.querySelector('[role="radiogroup"]')?.getAttribute('aria-describedby')).toBe('upmixer-1-delay');
      expect([delayOf(0), delayOf(2)]).toEqual([null, null]);
      expect(root.querySelectorAll('.upmix-delay')).toHaveLength(1);

      draw('running-upmix-pass', 'upmix');
      expect(delayOf(2)?.textContent).toContain('약 23.3\u00a0ms');
      recording(pass('running-upmix-pass', { latency_ms: 21 }));
      expect(delayOf(2)?.textContent).toContain('약 21.0\u00a0ms');

      // Gone around for now, it still costs that much whenever mono or stereo comes.
      draw('running-upmix-native-surround', 'upmix');
      expect(delayOf(2)?.textContent).toContain('약 23.3\u00a0ms');

      // A first-party upmixer says it the same way once it is built.
      const built = music('running-upmix', { choice: 'first_party', effective: 'first_party', plugin: null });
      built.upmix.first_party_available = true;
      recording(built);
      expect(delayOf(1)?.textContent).toContain('약 23.3\u00a0ms');
    });

    it.each<[string, Snapshot]>([
      ['no upmixer', fixture('ready')],
      ['an upmixer that has not started', fixture('ready-upmix')],
      ['an upmixer starting', music('running-upmix', { state: { kind: 'starting' }, latency_ms: null })],
      ['upmixing off with a delay left over', music('running-upmix', { choice: 'off', effective: 'off' })],
    ])('says nothing with %s', (_what, start) => {
      recording(start);
      expect(root.querySelector('.upmix-delay')).toBeNull();
      for (const element of root.querySelectorAll('[role="radiogroup"]')) expect(element.hasAttribute('aria-describedby')).toBe(false);
    });
  });

  describe('the (i) beside the intro', () => {
    const info = () => byKey('upmix-info');
    const tip = () => root.querySelector<HTMLElement>('#upmix-info-tip');
    const isOpen = () => info()?.closest('.info-tip-wrap')?.classList.contains('is-open') ?? false;
    const key = (target: EventTarget, name: string) =>
      target.dispatchEvent(new KeyboardEvent('keydown', { key: name, bubbles: true }));

    afterEach(() => {
      // Leave no tip open, or closed by Escape, for the next test.
      info()?.blur();
      info()?.closest('.info-tip-wrap')?.dispatchEvent(new MouseEvent('mouseleave'));
    });

    it('is a named button described by the one line that says what is upmixed', () => {
      draw('ready', 'upmix');
      const button = info();
      expect(button?.tagName).toBe('BUTTON');
      expect(button?.getAttribute('type')).toBe('button');
      expect(button?.classList.contains('info-tip')).toBe(true);
      expect(button?.getAttribute('aria-label')).toBe('업믹스 대상');
      expect(button?.getAttribute('aria-describedby')).toBe('upmix-info-tip');
      expect(button?.textContent).toBe('');
      expect(button?.querySelector('svg')?.getAttribute('aria-hidden')).toBe('true');
      // In the DOM while closed, so a screen reader always has it.
      expect(tip()?.getAttribute('role')).toBe('tooltip');
      expect(tip()?.textContent).toBe('업믹스는 모노와 스테레오에만 적용합니다.');
      expect(isOpen()).toBe(false);
      expect(root.querySelector('.upmix-intro')?.contains(button ?? null)).toBe(true);
      // The rule is said there once: not in the intro, and nowhere about native surround.
      expect(ko.upmix.intro).not.toContain('모노');
      expect(textOf()).not.toContain('네이티브 서라운드');
    });

    it('opens on focus, and Escape closes it until the focus comes back', () => {
      draw('running-upmix-pass', 'upmix');
      info()?.focus();
      expect(document.activeElement).toBe(info());
      expect(isOpen()).toBe(true);
      key(info()!, 'Enter');
      expect(isOpen()).toBe(true);
      key(info()!, 'Escape');
      expect(isOpen()).toBe(false);
      expect(tip()?.textContent).toBe(ko.upmix.infoTip);
      info()?.blur();
      info()?.focus();
      expect(isOpen()).toBe(true);
      info()?.blur();
      expect(isOpen()).toBe(false);
    });

    it('opens on hover, stays while the pointer is on it, and Escape closes it there too', () => {
      draw('ready', 'upmix');
      const wrap = info()?.closest('.info-tip-wrap');
      wrap?.dispatchEvent(new MouseEvent('mouseenter'));
      expect(isOpen()).toBe(true);
      key(document, 'Escape');
      expect(isOpen()).toBe(false);
      wrap?.dispatchEvent(new MouseEvent('mouseleave'));
      wrap?.dispatchEvent(new MouseEvent('mouseenter'));
      expect(isOpen()).toBe(true);
      // Leaving with the focus still on it keeps it open.
      info()?.focus();
      wrap?.dispatchEvent(new MouseEvent('mouseleave'));
      expect(isOpen()).toBe(true);
    });

    it('stays open, or closed, across a redraw that gives the focus back', () => {
      const start = fixture('running-upmix-pass');
      const app = mountApp(root, createMockApi(start), { tab: 'upmix' });
      app.draw(start);
      info()?.focus();
      app.draw(fixture('running-upmix-pass'));
      expect(document.activeElement).toBe(info());
      expect(isOpen()).toBe(true);
      key(info()!, 'Escape');
      app.draw(fixture('running-upmix-pass'));
      expect(document.activeElement).toBe(info());
      expect(isOpen()).toBe(false);
    });
  });

  it('names the plugin\'s input speakers, and leaves the row out when it reports none', () => {
    const rows = () =>
      [...(card(1)?.querySelectorAll('.upmix-plugin .fact') ?? [])].map((row) => [
        row.querySelector('dt')?.textContent,
        row.querySelector('dd')?.textContent,
      ]);
    draw('running-upmix', 'upmix');
    expect(rows()).toContainEqual(['입력', 'L R']);
    recording(music('running-upmix', { inputs: ['L', 'R', 'C'] }));
    expect(rows()).toContainEqual(['입력', 'L R C']);
    recording(music('running-upmix', { inputs: [] }));
    expect(rows().map(([label]) => label)).toEqual(['이름', '제작사', '형식', '경로']);
    // The outputs stay on the state line.
    expect(stateOf(1)?.textContent).toContain('출력 L R C LFE Ls Rs');
  });

  describe('while the sound goes around a running upmixer', () => {
    const REASONS: [UpmixBypass, string][] = [
      ['native_surround', '모노·스테레오가 아니라 업믹스하지 않음'],
      ['kind_off', '지금 쓰는 출력에는 적용하지 않음'],
      ['rate_change', '새 표본율에 맞추는 중'],
    ];

    it.each(REASONS)('%s: the state line says it runs and why, quietly, without the delay', (bypass, reason) => {
      recording(music('running-upmix', { bypass }));
      const line = stateOf(1);
      expect(line?.querySelector('.upmix-state-word')?.textContent).toBe('실행 중');
      expect(line?.querySelector('.upmix-state-reason')?.textContent).toBe(reason);
      expect([...(line?.querySelectorAll('.upmix-state-detail') ?? [])].map((detail) => detail.textContent)).toEqual([
        reason,
        '출력 L R C LFE Ls Rs',
      ]);
      expect(line?.classList.contains('tone-on')).toBe(true);
      expect(line?.classList.contains('is-alert')).toBe(false);
      expect(line?.textContent).not.toContain('지연');
    });

    it('running-upmix-native-surround says why on the PASS card, and the 2.0 stream of running-upmix-pass goes through', () => {
      draw('running-upmix-native-surround', 'upmix');
      expect(stateOf(2)?.querySelector('.upmix-state-reason')?.textContent).toBe('모노·스테레오가 아니라 업믹스하지 않음');
      expect(stateOf(2)?.textContent).not.toContain('지연');
      draw('running-upmix-pass', 'upmix');
      expect(stateOf(2)?.querySelector('.upmix-state-reason')).toBeNull();
      expect([...(stateOf(2)?.querySelectorAll('.upmix-state-detail') ?? [])].map((detail) => detail.textContent)).toEqual([
        '지연 23.3\u00a0ms',
        '출력 L R C LFE Ls Rs',
      ]);
      expect([stateOf(0)?.textContent, stateOf(1)?.textContent]).toEqual(['쓰지 않음', '쓰지 않음']);
    });

    it.each(REASONS)('%s: the status card says it in words, the lamp unlit', (bypass, reason) => {
      const start = music('running-upmix', { bypass });
      mountApp(root, createMockApi(start)).draw(start);
      const line = root.querySelectorAll('.endpoint-card')[1]?.querySelector('.card-upmix');
      expect(line?.querySelector('.card-upmix-reason')?.textContent).toBe(reason);
      expect(line?.textContent).toBe(`업믹스: Example Upmixer${reason}`);
      expect(line?.classList.contains('tone-off')).toBe(true);
      expect(line?.classList.contains('tone-warning')).toBe(false);
    });

    it('the status card of a fallback stays a warning without a reason', () => {
      draw('running-upmix-fallback');
      const line = root.querySelectorAll('.endpoint-card')[1]?.querySelector('.card-upmix');
      expect(line?.querySelector('.card-upmix-reason')).toBeNull();
      expect(line?.classList.contains('tone-warning')).toBe(true);
    });
  });

  it('the status cards name the running upmixer, warning when the original stereo plays', () => {
    draw('running-upmix');
    const line = () => root.querySelectorAll('.endpoint-card')[1]?.querySelector('.card-upmix');
    expect(line()?.textContent).toBe('업믹스: Example Upmixer');
    expect(line()?.classList.contains('tone-on')).toBe(true);
    expect(root.querySelectorAll('.card-upmix')).toHaveLength(1);
    draw('running-upmix-fallback');
    expect(line()?.textContent).toBe('업믹스: Example Upmixer, 원래 스테레오를 재생');
    expect(line()?.classList.contains('tone-warning')).toBe(true);
    for (const name of ['running', 'ready-upmix', 'ready-upmix-failed']) {
      draw(name);
      expect(root.querySelector('.card-upmix'), name).toBeNull();
    }
  });
});

describe('the profile upmix switch', () => {
  const sw = (kind: ProfileKind) => root.querySelector<HTMLButtonElement>(`[data-key="upmix-apply-${kind}"]`);
  const tab = (kind: ProfileKind) => root.querySelector<HTMLButtonElement>(`[data-key="profile-tab-${kind}"]`);
  function recording(start: Snapshot) {
    const sent: Intent[] = [];
    const inner = createMockApi(start);
    const api: Api = { ...inner, dispatch: async (intent) => void sent.push(intent) };
    mountApp(root, api, { tab: 'profile' }).draw(start);
    return sent;
  }

  it('is not there while every upmixer is off', () => {
    for (const name of ['ready', 'running', 'ready-headphones', 'ready-speakers']) {
      draw(name, 'profile');
      for (const kind of ['headphones', 'speakers'] as const) {
        tab(kind)?.click();
        expect(sw(kind), `${name} ${kind}`).toBeNull();
      }
    }
  });

  it('is a switch on each tab once an upmixer is on, showing that kind\'s value', () => {
    draw('ready-speakers-upmix-off', 'profile');
    expect(sw('speakers')?.getAttribute('role')).toBe('switch');
    expect(sw('speakers')?.tagName).toBe('BUTTON');
    expect(sw('speakers')?.getAttribute('aria-checked')).toBe('false');
    expect(sw('speakers')?.textContent).toBe('업믹스 적용');
    const hint = root.querySelector(`#${sw('speakers')?.getAttribute('aria-describedby')}`);
    expect(hint?.textContent).toBe(ko.profile.upmixApplyHint);
    // Off: the sound as it came, without delay, the plugin still loaded.
    for (const phrase of ['끄면', '지연 없이', '그대로', '플러그인은 종료하지 않고 준비된 상태로']) expect(hint?.textContent).toContain(phrase);
    expect(sw('headphones')).toBeNull();
    tab('headphones')?.click();
    expect(sw('headphones')?.getAttribute('aria-checked')).toBe('true');
    expect(sw('speakers')).toBeNull();
  });

  it('sends set_upmix_apply with its kind and the other value', () => {
    let sent = recording(fixture('ready-speakers-upmix-off'));
    sw('speakers')?.click();
    expect(sent).toEqual([{ kind: 'set_upmix_apply', value: { kind: 'speakers', on: true } }]);
    sent = recording(fixture('ready-upmix'));
    tab('headphones')?.click();
    sw('headphones')?.click();
    expect(sent).toEqual([{ kind: 'set_upmix_apply', value: { kind: 'headphones', on: false } }]);
  });

  it('stands where it goes while the change is in flight, locked', () => {
    const start = fixture('ready-upmix');
    mountApp(root, createMockApi(start, { hold: true }), { tab: 'profile' }).draw(start);
    sw('speakers')?.click();
    expect(sw('speakers')?.getAttribute('aria-checked')).toBe('false');
    expect(sw('speakers')?.getAttribute('aria-busy')).toBe('true');
    expect(sw('speakers')?.disabled).toBe(true);
  });

  it('is not locked by profile.toml', () => {
    const start = fixture('ready-profile-file');
    start.upmix = structuredClone(fixture('ready-upmix').upmix);
    const sent = recording(start);
    const kind = start.profile.kind;
    expect(root.querySelector('.file-note')).not.toBeNull();
    expect(sw(kind)?.disabled).toBe(false);
    expect(sw(kind)?.getAttribute('aria-describedby')).not.toContain('profile-file-hint');
    sw(kind)?.click();
    expect(sent).toEqual([{ kind: 'set_upmix_apply', value: { kind, on: false } }]);
  });
});

describe('the mock supervisor follows the upmix rules (ADR 0035)', () => {
  const endpointOf = async (api: Api, endpoint: number) => {
    const view = (await api.snapshot()).upmix.endpoints[endpoint];
    if (view === undefined) throw new Error(`no endpoint ${endpoint}`);
    return view;
  };
  const music = (api: Api) => endpointOf(api, 1);
  const pass = (api: Api) => endpointOf(api, 2);

  it('lets main, music and PASS choose, and refuses endpoint 3 and the first-party upmixer', async () => {
    const start = fixture('ready');
    const api = createMockApi(start);
    expect(start.upmix.endpoints.map((endpoint) => endpoint.endpoint)).toEqual([0, 1, 2]);
    for (const endpoint of [3, 4]) {
      await expect(api.dispatch({ kind: 'set_upmixer', value: { endpoint, choice: 'plugin' } })).rejects.toEqual({
        message: '이 입력은 업믹스하지 않습니다.',
        next: 'none',
      });
    }
    await expect(api.dispatch({ kind: 'choose_plugin', value: { endpoint: 3, path: 'C:\\a.vst3' } })).rejects.toMatchObject({
      message: '이 입력은 업믹스하지 않습니다.',
    });
    await expect(api.dispatch({ kind: 'set_upmixer', value: { endpoint: 0, choice: 'first_party' } })).rejects.toEqual({
      message: '앱 자체 업믹서는 아직 제공되지 않습니다.',
      next: 'none',
    });
    expect((await api.snapshot()).upmix).toEqual(start.upmix);
    expect((await api.snapshot()).error).toBeNull();

    await api.dispatch({ kind: 'set_upmixer', value: { endpoint: 1, choice: 'plugin' } });
    expect(await music(api)).toMatchObject({ choice: 'plugin', effective: 'plugin', plugin: null, state: { kind: 'stopped' } });
    await api.dispatch({ kind: 'set_upmixer', value: { endpoint: 1, choice: 'off' } });
    expect(await music(api)).toMatchObject({ choice: 'off', effective: 'off' });
    expect((await api.snapshot()).upmix.endpoints[0]).toEqual(start.upmix.endpoints[0]);

    // PASS: its own upmixer and plugin, the others untouched.
    await api.dispatch({ kind: 'set_upmixer', value: { endpoint: 2, choice: 'plugin' } });
    await api.dispatch({ kind: 'choose_plugin', value: { endpoint: 2, path: 'C:\\VST3\\Wide.vst3' } });
    expect(await pass(api)).toMatchObject({ choice: 'plugin', effective: 'plugin', plugin: { name: 'Wide' } });
    const { upmix, error } = await api.snapshot();
    expect(upmix.endpoints.map((endpoint) => endpoint.endpoint)).toEqual([0, 1, 2]);
    expect(upmix.endpoints[1]).toMatchObject({ choice: 'off', plugin: null });
    expect(error).toBeNull();
  });

  it('starts the PASS upmixer with the engine and opens its editor while it runs', async () => {
    const api = createMockApi(fixture('ready'), { upmixStartMs: 5 });
    await api.dispatch({ kind: 'set_upmixer', value: { endpoint: 2, choice: 'plugin' } });
    await api.dispatch({ kind: 'choose_plugin', value: { endpoint: 2, path: 'C:\\VST3\\Wide.vst3' } });
    await api.dispatch({ kind: 'enable' });
    expect((await pass(api)).state).toEqual({ kind: 'starting' });
    await vi.waitFor(async () => expect((await pass(api)).state).toEqual({ kind: 'running' }));
    expect((await pass(api)).latency_ms).toBe(23.3);
    await api.dispatch({ kind: 'open_plugin_editor', value: 2 });
    await expect(api.dispatch({ kind: 'open_plugin_editor', value: 1 })).rejects.toMatchObject({
      message: '지금은 할 수 없는 동작입니다.',
    });
  });

  it('scans a .vst3 or .dll as the test plugin and refuses anything else', async () => {
    const api = createMockApi(fixture('ready'));
    await expect(api.dispatch({ kind: 'choose_plugin', value: { endpoint: 1, path: 'C:\\VST\\readme.txt' } })).rejects.toEqual({
      message: '업믹스 플러그인을 읽지 못했습니다: readme.txt',
      next: 'none',
    });
    expect((await api.snapshot()).error?.message).toBe('업믹스 플러그인을 읽지 못했습니다: readme.txt');
    expect((await music(api)).plugin).toBeNull();

    const bundle = 'C:\\Program Files\\Common Files\\VST3\\Wide Stage.vst3\\';
    await api.dispatch({ kind: 'choose_plugin', value: { endpoint: 1, path: bundle } });
    expect(await music(api)).toMatchObject({
      plugin: { path: bundle, format: 'vst3', name: 'Wide Stage', vendor: 'Example Audio' },
      inputs: ['L', 'R'],
      outputs: ['L', 'R', 'C', 'LFE', 'Ls', 'Rs'],
      state: { kind: 'stopped' },
      latency_ms: null,
    });
    expect((await api.snapshot()).error).toBeNull();
    await api.dispatch({ kind: 'choose_plugin', value: { endpoint: 1, path: 'C:\\VSTPlugins\\Old Wide.DLL' } });
    expect((await music(api)).plugin).toMatchObject({ format: 'vst2', name: 'Old Wide' });
    // Choosing a plugin does not choose the VST upmixer.
    expect((await music(api)).effective).toBe('off');
  });

  it('starts a chosen plugin while the engine runs, then runs it', async () => {
    const api = createMockApi(fixture('running'), { upmixStartMs: 5 });
    const seen: string[] = [];
    await api.onSnapshot((snapshot) => seen.push(snapshot.upmix.endpoints[1]?.state.kind ?? 'none'));
    await api.dispatch({ kind: 'set_upmixer', value: { endpoint: 1, choice: 'plugin' } });
    // No plugin yet: nothing to start.
    expect((await music(api)).state).toEqual({ kind: 'stopped' });
    await api.dispatch({ kind: 'choose_plugin', value: { endpoint: 1, path: 'C:\\VST3\\Wide.vst3' } });
    expect(await music(api)).toMatchObject({ state: { kind: 'starting' }, latency_ms: null });
    await vi.waitFor(async () => expect((await music(api)).state).toEqual({ kind: 'running' }));
    expect((await music(api)).latency_ms).toBe(23.3);
    await vi.waitFor(() => expect(seen.at(-1)).toBe('running'));
    expect(seen).toContain('starting');
  });

  it('holds a starting plugin when the start is frozen, as the gallery does', async () => {
    const api = createMockApi(fixture('ready-upmix'), { upmixStartMs: null });
    await api.dispatch({ kind: 'enable' });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect((await music(api)).state).toEqual({ kind: 'starting' });
  });

  it('starts the upmixer with the engine and stops it with the engine', async () => {
    const api = createMockApi(fixture('ready-upmix'), { upmixStartMs: 5 });
    await api.dispatch({ kind: 'enable' });
    expect((await music(api)).state).toEqual({ kind: 'starting' });
    await vi.waitFor(async () => expect((await music(api)).state).toEqual({ kind: 'running' }));
    await api.dispatch({ kind: 'disable' });
    expect(await music(api)).toMatchObject({ state: { kind: 'stopped' }, latency_ms: null });
  });

  it('opens the editor only while the host is up', async () => {
    await expect(createMockApi(fixture('ready-upmix')).dispatch({ kind: 'open_plugin_editor', value: 1 })).rejects.toEqual({
      message: '지금은 할 수 없는 동작입니다.',
      next: 'none',
    });
    await expect(createMockApi(fixture('ready-upmix-failed')).dispatch({ kind: 'open_plugin_editor', value: 1 })).rejects.toMatchObject({
      message: '지금은 할 수 없는 동작입니다.',
    });
    await expect(createMockApi(fixture('running-upmix')).dispatch({ kind: 'open_plugin_editor', value: 0 })).rejects.toMatchObject({
      message: '지금은 할 수 없는 동작입니다.',
    });
    await createMockApi(fixture('running-upmix')).dispatch({ kind: 'open_plugin_editor', value: 1 });
    await createMockApi(fixture('running-upmix-fallback')).dispatch({ kind: 'open_plugin_editor', value: 1 });
  });

  it('keeps each kind\'s switch, and the host stays up while it is off', async () => {
    const api = createMockApi(fixture('running-upmix'), { upmixStartMs: null });
    await api.dispatch({ kind: 'set_upmix_apply', value: { kind: 'speakers', on: false } });
    let snapshot = await api.snapshot();
    expect(snapshot.upmix).toMatchObject({ headphones: true, speakers: false });
    // Headphones are in use and still apply it.
    expect(snapshot.upmix.endpoints[1]).toMatchObject({ state: { kind: 'running' }, bypass: null });
    await api.dispatch({ kind: 'set_upmix_apply', value: { kind: 'headphones', on: false } });
    snapshot = await api.snapshot();
    // Off: the sound goes around it, and the host runs on.
    expect(snapshot.upmix.endpoints[1]).toMatchObject({ state: { kind: 'running' }, bypass: 'kind_off', latency_ms: 23.3 });
    // On again: straight back, without loading the plugin again.
    await api.dispatch({ kind: 'set_upmix_apply', value: { kind: 'headphones', on: true } });
    expect(await music(api)).toMatchObject({ state: { kind: 'running' }, bypass: null });
    // Switching to speakers, which do not apply it, goes around it too.
    await api.dispatch({ kind: 'set_profile_kind', value: 'speakers' });
    expect(await music(api)).toMatchObject({ state: { kind: 'running' }, bypass: 'kind_off' });
  });

  it('starts the host with the engine even when the kind in use does not apply it', async () => {
    const api = createMockApi(fixture('ready-speakers-upmix-off'), { upmixStartMs: 5 });
    await api.dispatch({ kind: 'enable' });
    expect(await music(api)).toMatchObject({ state: { kind: 'starting' }, bypass: null });
    await vi.waitFor(async () => expect(await music(api)).toMatchObject({ state: { kind: 'running' }, bypass: 'kind_off' }));
  });

  it('takes every upmix intent while profile.toml decides the profile', async () => {
    const api = createMockApi(fixture('ready-profile-file'));
    await api.dispatch({ kind: 'set_upmixer', value: { endpoint: 1, choice: 'plugin' } });
    await api.dispatch({ kind: 'choose_plugin', value: { endpoint: 1, path: 'C:\\VST3\\Wide.vst3' } });
    await api.dispatch({ kind: 'set_upmix_apply', value: { kind: 'headphones', on: false } });
    const { upmix, profile } = await api.snapshot();
    expect(profile.from_file).toBe(true);
    expect(upmix.headphones).toBe(false);
    expect(upmix.endpoints[1]).toMatchObject({ effective: 'plugin', plugin: { name: 'Wide' } });
  });

  it('refuses upmix intents before the wizard is done', async () => {
    const api = createMockApi(fixture('setup-choose-output'));
    await expect(api.dispatch({ kind: 'set_upmixer', value: { endpoint: 1, choice: 'plugin' } })).rejects.toMatchObject({
      message: '지금은 할 수 없는 동작입니다.',
    });
    await expect(api.dispatch({ kind: 'set_upmix_apply', value: { kind: 'speakers', on: false } })).rejects.toMatchObject({
      message: '지금은 할 수 없는 동작입니다.',
    });
  });

  it('keeps a failed plugin off for the session and says why in the language chosen', async () => {
    const api = createMockApi(fixture('ready-upmix-failed'), { upmixStartMs: null });
    await api.dispatch({ kind: 'enable' });
    expect((await music(api)).state).toEqual(fixture('ready-upmix-failed').upmix.endpoints[1]?.state);
    await api.dispatch({ kind: 'set_language', value: 'en' });
    expect((await music(api)).state).toEqual(fixture('ready-upmix-failed', 'en').upmix.endpoints[1]?.state);
    // A new plugin is a new start.
    await api.dispatch({ kind: 'choose_plugin', value: { endpoint: 1, path: 'C:\\VST3\\Other.vst3' } });
    expect((await music(api)).state).toEqual({ kind: 'starting' });
  });

  it('takes no choice of how stereo arrives', async () => {
    const api = createMockApi(fixture('running-upmix'));
    const old = { kind: 'set_upmix_sources', value: { endpoint: 1, sources: { pcm: false, bitstream: false } } };
    // Not an intent any more: the bridge's guard turns it away before the supervisor.
    expect(() => assertIntent(old)).toThrow('intent.kind');
    expect((await api.snapshot()).upmix.endpoints.every((endpoint) => !('sources' in endpoint))).toBe(true);
  });

  describe('goes around a running upmixer as ADR 0035 says', () => {
    const pcm = (channels: number, channel_mask: number): SourceFormat => ({
      kind: 'pcm',
      value: { channels, sample_rate: 48_000, bits_per_sample: 32, valid_bits: 32, sample_type: 'float', channel_mask },
    });
    const eac3 = (channels: number): SourceFormat => ({
      kind: 'bitstream',
      value: { codec: 'eac3', container_rate: 192_000, container_channels: 2, encoded_rate: 48_000, encoded_channels: channels },
    });
    /** The endpoint running the plugin while `format` plays: music in running-upmix, PASS in running-upmix-pass. */
    function playing(endpoint: 1 | 2, format: SourceFormat | null) {
      const start = fixture(endpoint === 1 ? 'running-upmix' : 'running-upmix-pass');
      start.engine!.endpoints[endpoint]!.format = format;
      return createMockApi(start, { upmixStartMs: null });
    }
    const apply = (on: boolean): Intent => ({ kind: 'set_upmix_apply', value: { kind: 'headphones', on } });

    it.each<[string, SourceFormat, boolean]>([
      ['mono PCM', pcm(1, 0x4), false],
      ['stereo PCM', pcm(2, 0x3), false],
      ['2.1 PCM', pcm(3, 0xb), false],
      ['3.0 PCM', pcm(3, 0x7), true],
      ['5.1 PCM', pcm(6, 0x3f), true],
      ['7.1.4 PCM', pcm(12, 0), true],
      ['a mono bitstream', eac3(1), false],
      ['a 2.0 bitstream', eac3(2), false],
      ['a 5.1 bitstream', eac3(6), true],
      ['a 7.1 bitstream', eac3(8), true],
    ])('%s', async (_name, format, surround) => {
      // However it arrives, on the music endpoint or on PASS, the same rule.
      for (const endpoint of [1, 2] as const) {
        const api = playing(endpoint, format);
        await api.dispatch(apply(true));
        expect((await endpointOf(api, endpoint)).bypass, `endpoint ${endpoint}`).toBe(surround ? 'native_surround' : null);
        // The kind in use not applying it comes first.
        await api.dispatch(apply(false));
        expect((await endpointOf(api, endpoint)).bypass, `endpoint ${endpoint}`).toBe('kind_off');
      }
    });

    it('nothing playing goes around nothing, and a stopped upmixer has no reason', async () => {
      const api = playing(2, null);
      await api.dispatch(apply(true));
      expect((await pass(api)).bypass).toBeNull();
      await api.dispatch({ kind: 'disable' });
      await api.dispatch(apply(false));
      expect(await pass(api)).toMatchObject({ state: { kind: 'stopped' }, bypass: null });
    });

    it.each(['running-upmix', 'running-upmix-fallback', 'running-upmix-pass', 'running-upmix-native-surround'])(
      'agrees with the %s fixture, all three endpoints',
      async (name) => {
        const start = fixture(name);
        const api = createMockApi(start, { upmixStartMs: null });
        // Any intent has the mock say its upmixers afresh; this one changes nothing.
        await api.dispatch({ kind: 'set_upmix_apply', value: { kind: start.profile.kind, on: start.upmix[start.profile.kind] } });
        expect((await api.snapshot()).upmix).toEqual(start.upmix);
      },
    );
  });

  it('draws the plugin it scanned, from the picker to the card', async () => {
    const start = fixture('ready');
    const api = createMockApi(start);
    const app = mountApp(root, api, { tab: 'upmix' });
    app.draw(start);
    await api.onSnapshot(app.draw);
    root.querySelector<HTMLInputElement>('[data-key="upmixer-1-plugin"]')?.click();
    await vi.waitFor(() => expect(root.querySelector('[data-key="plugin-folder-1"]')).not.toBeNull());
    await vi.waitFor(() => expect(root.querySelector<HTMLButtonElement>('[data-key="plugin-folder-1"]')?.disabled).toBe(false));
    root.querySelector<HTMLButtonElement>('[data-key="plugin-folder-1"]')?.click();
    await vi.waitFor(() => expect(root.querySelector('.upmix-plugin dl')?.textContent).toContain('Example Upmixer'));
    expect(root.querySelector('.upmix-plugin')?.textContent).toContain('C:\\Program Files\\Common Files\\VST3\\Example Upmixer.vst3');
  });
});
