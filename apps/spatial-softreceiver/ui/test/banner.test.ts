import { describe, expect, it } from 'vitest';
import type { NextAction, Phase, Snapshot } from '../src/bindings';
import { mountApp } from '../src/app';
import { createMockApi } from '../src/mock';
import { fixture } from '../src/fixtures';
import { bannerAction } from '../src/screens/banner';

const setup = (value: Extract<Phase, { kind: 'setup' }>['value']): Phase => ({ kind: 'setup', value });

/** The intent kind a band's button sends, or what else it does. */
function sends(phase: Phase, next: NextAction): string {
  const action = bannerAction(phase, next);
  return action.kind === 'intent' ? action.intent.kind : action.kind;
}

describe('bannerAction', () => {
  it('retry depends on the phase', () => {
    expect(sends(setup('driver'), 'retry')).toBe('install_driver');
    for (const step of ['welcome', 'unsupported', 'not_test_signing', 'reboot_required', 'choose_output'] as const) {
      expect(sends(setup(step), 'retry')).toBe('recheck');
    }
    expect(sends({ kind: 'ready' }, 'retry')).toBe('enable');
    expect(sends({ kind: 'faulted' }, 'retry')).toBe('recheck');
    expect(sends({ kind: 'running' }, 'retry')).toBe('nothing');
    expect(sends({ kind: 'quit' }, 'retry')).toBe('nothing');
  });

  it('reboot always rechecks', () => {
    const phases: Phase[] = [setup('not_test_signing'), setup('reboot_required'), { kind: 'ready' }, { kind: 'faulted' }];
    for (const phase of phases) {
      expect(sends(phase, 'reboot')).toBe('recheck');
      expect(bannerAction(phase, 'reboot')).toMatchObject({ label: '다시 확인' });
    }
  });

  it('open_settings opens the output list', () => {
    expect(sends({ kind: 'ready' }, 'open_settings')).toBe('choose_output');
    expect(bannerAction({ kind: 'ready' }, 'open_settings')).toMatchObject({ label: '출력 고르기' });
    expect(sends(setup('choose_output'), 'open_settings')).toBe('choose_output');
  });

  it('none only closes, and in the wizard not even that', () => {
    for (const phase of [{ kind: 'ready' }, { kind: 'running' }, { kind: 'faulted' }] as Phase[]) {
      expect(sends(phase, 'none')).toBe('dismiss');
    }
    expect(sends(setup('unsupported'), 'none')).toBe('nothing');
    expect(sends(setup('choose_output'), 'none')).toBe('nothing');
  });
});

function mount(snapshot: Snapshot) {
  const root = document.createElement('div');
  document.body.replaceChildren(root);
  const api = createMockApi(snapshot);
  const app = mountApp(root, api);
  app.draw(snapshot);
  return { root, app, api };
}

const bandButtons = (root: HTMLElement) =>
  [...root.querySelectorAll<HTMLButtonElement>('.banner button')].map((button) => button.textContent);

describe('the band on screen', () => {
  it('shows the reboot hint and a recheck button only once', () => {
    const { root } = mount(fixture('setup-reboot-required'));
    expect(root.querySelector('.banner')?.textContent).toContain('Windows를 다시 시작한 뒤 다시 확인하십시오.');
    const rechecks = [...root.querySelectorAll('button[data-intent="recheck"]')];
    expect(rechecks).toHaveLength(1);
  });

  it('leaves the retry to the body when the body offers the same intent', () => {
    const { root } = mount(fixture('setup-driver-problem'));
    expect(bandButtons(root)).toEqual([]);
    expect(root.querySelectorAll('button[data-intent="install_driver"]')).toHaveLength(1);
  });

  it('offers recheck beside the power switch when faulted', () => {
    const { root } = mount(fixture('faulted'));
    expect(bandButtons(root)).toEqual(['다시 확인']);
    expect(root.querySelector('.banner button')?.getAttribute('data-intent')).toBe('recheck');
  });

  it('opens the output list from 출력 고르기', () => {
    const { root } = mount(fixture('ready-output-renamed'));
    expect(root.querySelector('.status-outputs')).toBeNull();
    root.querySelector<HTMLButtonElement>('.banner button')?.click();
    expect(root.querySelectorAll('.status-outputs .output-row')).toHaveLength(3);
  });

  it('closes a none band until the sentence changes', () => {
    const snapshot = fixture('running-reconnecting');
    const { root, app } = mount(snapshot);
    expect(bandButtons(root)).toEqual(['닫기']);
    root.querySelector<HTMLButtonElement>('.banner button')?.click();
    expect(root.querySelector('.banner')).toBeNull();
    app.draw(structuredClone(snapshot));
    expect(root.querySelector('.banner')).toBeNull();
    const changed = structuredClone(snapshot);
    changed.error = { message: '캡처 장치와의 연결이 끊어져 다시 연결하는 중입니다.', next: 'none' };
    app.draw(changed);
    expect(root.querySelector('.banner')?.textContent).toContain('캡처 장치');
    app.draw(structuredClone(snapshot));
    expect(root.querySelector('.banner')?.textContent).toContain(snapshot.error?.message ?? 'no error');
  });
});

describe('a refusal in the wizard', () => {
  it('can be closed although the step band cannot', async () => {
    const start = fixture('setup-unsupported');
    const root = document.createElement('div');
    document.body.replaceChildren(root);
    const api = createMockApi(start);
    const app = mountApp(root, api);
    app.draw(start);
    await api.onSnapshot(app.draw);
    expect(bandButtons(root)).toEqual([]);
    await app.send({ kind: 'begin_setup' });
    expect(root.querySelectorAll('.banner')).toHaveLength(2);
    expect(bandButtons(root)).toEqual(['닫기']);
    root.querySelector<HTMLButtonElement>('.banner button')?.click();
    expect(root.querySelectorAll('.banner')).toHaveLength(1);
    expect(root.textContent).toContain(start.error?.message ?? 'no error');
  });
});
