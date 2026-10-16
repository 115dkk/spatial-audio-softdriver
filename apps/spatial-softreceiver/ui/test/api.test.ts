import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Language, Snapshot } from '../src/bindings';
import { fixture } from '../src/fixtures';
import { createMockApi } from '../src/mock';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(), isTauri: () => false }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }));

describe('the mock bridge', () => {
  it('walks the wizard from welcome to choosing an output', async () => {
    const api = createMockApi(fixture('setup-welcome'));
    const seen: Snapshot['phase'][] = [];
    await api.onSnapshot((snapshot) => seen.push(snapshot.phase));

    await api.dispatch({ kind: 'begin_setup' });
    expect((await api.snapshot()).phase).toEqual({ kind: 'setup', value: 'driver' });

    await api.dispatch({ kind: 'install_driver' });
    const chooser = await api.snapshot();
    expect(chooser.phase).toEqual({ kind: 'setup', value: 'choose_output' });
    expect(chooser.outputs).toHaveLength(3);
    // The snapshot event follows the reply, as the shell sends it.
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(seen).toEqual([
      { kind: 'setup', value: 'driver' },
      { kind: 'setup', value: 'choose_output' },
    ]);

    const target = chooser.outputs[1];
    if (target === undefined) throw new Error('no output');
    await api.dispatch({ kind: 'choose_output', value: target.id });
    const ready = await api.snapshot();
    expect(ready.phase).toEqual({ kind: 'ready' });
    expect(ready.chosen_output).toEqual(target);

    await api.dispatch({ kind: 'enable' });
    const running = await api.snapshot();
    expect(running.phase).toEqual({ kind: 'running' });
    expect(running.engine).not.toBeNull();
  });

  it('rejects a refused intent with its UserError and keeps it out of the snapshot', async () => {
    const api = createMockApi(fixture('ready'));
    await expect(api.dispatch({ kind: 'test_direction', value: 0 })).rejects.toEqual({
      message: '켠 뒤에 시험음을 낼 수 있습니다.',
      next: 'none',
    });
    await expect(api.dispatch({ kind: 'begin_setup' })).rejects.toMatchObject({ next: 'none' });
    expect((await api.snapshot()).error).toBeNull();
  });

  it('keeps a failed intent in the snapshot, as the supervisor does', async () => {
    const api = createMockApi(fixture('running'));
    await expect(api.dispatch({ kind: 'choose_output', value: 'x' })).rejects.toMatchObject({
      message: '먼저 끈 뒤 출력을 바꾸십시오.',
    });
    expect((await api.snapshot()).error?.message).toBe('먼저 끈 뒤 출력을 바꾸십시오.');
  });
});

describe('the mock speaks as the supervisor', () => {
  const languages: Language[] = ['ko', 'en'];

  it.each([
    ['ready-start-failed', 'ko', 'en'],
    ['running', 'ko', 'en'],
    ['setup-output-missing', 'en', 'ko'],
    ['ready-output-renamed', 'en', 'ko'],
  ] as const)('says the %s fixture sentences again in the other language (%s to %s)', async (name, from, to) => {
    // machine.rs renders its kept error and its notes in the new language.
    const api = createMockApi(fixture(name, from));
    await api.dispatch({ kind: 'set_language', value: to });
    const after = await api.snapshot();
    const expected = fixture(name, to);
    expect(after.language).toBe(to);
    expect(after.error).toEqual(expected.error);
    expect(after.notes).toEqual(expected.notes);
  });

  /** The welcome step on a PC whose platform or driver is a wizard fixture's. */
  function welcomeOn(language: Language, from: string): Snapshot {
    const start = fixture('setup-welcome', language);
    const pc = fixture(from, language);
    start.platform = pc.platform;
    start.driver = pc.driver;
    return start;
  }

  it.each(languages.flatMap((language) =>
    ['setup-unsupported', 'setup-not-test-signing', 'setup-driver-problem', 'setup-reboot-required'].map(
      (name) => [name, language] as const,
    ),
  ))('reaches %s (%s) with the sentence Rust wrote into it', async (name, language) => {
    const api = createMockApi(welcomeOn(language, name));
    const expected = fixture(name, language);
    await expect(api.dispatch({ kind: 'begin_setup' })).rejects.toEqual(expected.error);
    const snapshot = await api.snapshot();
    expect(snapshot.phase).toEqual(expected.phase);
    expect(snapshot.error).toEqual(expected.error);
    expect(snapshot.notes).toEqual([]);
  });

  it('says why another architecture is unsupported', async () => {
    const start = welcomeOn('en', 'setup-choose-output');
    start.platform = { os_build: 26100, arch: 'arm64', test_signing: true, secure_boot: 'disabled' };
    const api = createMockApi(start);
    await expect(api.dispatch({ kind: 'begin_setup' })).rejects.toEqual({
      message: 'This build is for x64, but this PC is Arm64.',
      next: 'none',
    });
    expect((await api.snapshot()).phase).toEqual({ kind: 'setup', value: 'unsupported' });
  });

  it('fails and refuses in English when it starts from an English fixture', async () => {
    const running = createMockApi(fixture('running', 'en'));
    await expect(running.dispatch({ kind: 'choose_output', value: 'x' })).rejects.toEqual({
      message: 'Turn it off before changing the output.',
      next: 'none',
    });
    expect((await running.snapshot()).error?.message).toBe('Turn it off before changing the output.');
    await expect(running.dispatch({ kind: 'test_direction', value: 99 })).rejects.toMatchObject({
      message: 'That direction does not exist.',
    });

    const ready = createMockApi(fixture('ready', 'en'));
    await expect(ready.dispatch({ kind: 'test_direction', value: 0 })).rejects.toEqual({
      message: 'Turn it on before playing a test tone.',
      next: 'none',
    });
    await expect(ready.dispatch({ kind: 'begin_setup' })).rejects.toEqual({
      message: 'That action is not available right now.',
      next: 'none',
    });
    await expect(ready.dispatch({ kind: 'start_diagnostic_recording' })).rejects.toMatchObject({
      message: 'Diagnostic recording is coming in a later update.',
    });
    await expect(ready.dispatch({ kind: 'set_profile_kind', value: 'headphones' })).rejects.toMatchObject({
      message: 'Import a BRIR set first.',
    });
    await expect(ready.dispatch({ kind: 'set_gain', value: 11 })).rejects.toMatchObject({
      message: 'Gain must be between -20 dB and +10 dB.',
    });
    expect((await ready.snapshot()).error?.message).toBe('Gain must be between -20 dB and +10 dB.');
  });

  it('adds no note of its own when it turns on, and clears the old ones', async () => {
    for (const language of languages) {
      const api = createMockApi(fixture('running', language));
      expect((await api.snapshot()).notes).toHaveLength(1);
      await api.dispatch({ kind: 'disable' });
      await api.dispatch({ kind: 'enable' });
      const snapshot = await api.snapshot();
      expect(snapshot.phase).toEqual({ kind: 'running' });
      expect(snapshot.notes).toEqual([]);
    }
  });

  it('finds a saved output again by its name, with the supervisor note', async () => {
    const start = fixture('ready', 'en');
    const saved = start.chosen_output;
    if (saved === null) throw new Error('ready has a chosen output');
    start.chosen_output = { id: 'an id from before a reboot', name: saved.name };
    const api = createMockApi(start);
    await api.dispatch({ kind: 'enable' });
    const snapshot = await api.snapshot();
    expect(snapshot.chosen_output).toEqual(saved);
    expect(snapshot.notes).toEqual([`The output device identifier changed, so it was found again by name: ${saved.name}`]);
  });

  it('sends a missing saved output back to the output step', async () => {
    const start = fixture('ready');
    start.chosen_output = { id: 'gone', name: '사라진 장치' };
    const api = createMockApi(start);
    await expect(api.dispatch({ kind: 'enable' })).rejects.toEqual({
      message: '저장된 출력 장치를 찾을 수 없습니다: 사라진 장치',
      next: 'open_settings',
    });
    expect((await api.snapshot()).phase).toEqual({ kind: 'setup', value: 'choose_output' });
  });

  it('says a kept error again in the language chosen, as the supervisor does', async () => {
    const api = createMockApi(fixture('ready'));
    await expect(api.dispatch({ kind: 'set_gain', value: 12 })).rejects.toMatchObject({
      message: '게인은 -20 dB에서 +10 dB 사이입니다.',
    });
    await api.dispatch({ kind: 'set_language', value: 'en' });
    const snapshot = await api.snapshot();
    expect(snapshot.language).toBe('en');
    expect(snapshot.error).toEqual({ message: 'Gain must be between -20 dB and +10 dB.', next: 'none' });
    // Any other success clears it.
    await api.dispatch({ kind: 'set_gain', value: -3 });
    expect((await api.snapshot()).error).toBeNull();
  });

  it('follows the starting language for automatic and names speakers by their layout or correction in any', async () => {
    const api = createMockApi(fixture('ready', 'en'));
    expect((await api.snapshot()).profile.name).toBe('2.0');
    await api.dispatch({ kind: 'set_language', value: 'ko' });
    let snapshot = await api.snapshot();
    expect(snapshot).toMatchObject({ language: 'ko', language_choice: 'ko' });
    expect(snapshot.profile.name).toBe('2.0');
    await api.dispatch({ kind: 'import_correction', value: 'C:\\SECS\\configs\\secs_mixed.yml' });
    await api.dispatch({ kind: 'set_language', value: null });
    snapshot = await api.snapshot();
    expect(snapshot).toMatchObject({ language: 'en', language_choice: null });
    expect(snapshot.profile.name).toBe(fixture('ready-speakers', 'en').profile.name);
  });
});

describe('the mock shell and held intents', () => {
  it('is the plain browser without a shell', async () => {
    const { shell } = createMockApi(fixture('ready'));
    expect(shell.native).toBe(false);
    expect(await shell.autostartEnabled()).toBe(false);
  });

  it('stands in for the Tauri shell with the values given', async () => {
    const { shell } = createMockApi(fixture('ready'), {
      shell: { autostart: true, update: { current: '0.15.0', ready: '0.16.0' } },
    });
    expect(shell.native).toBe(true);
    expect(await shell.autostartEnabled()).toBe(true);
    await shell.setAutostart(false);
    expect(await shell.autostartEnabled()).toBe(false);
    expect(await shell.updateState()).toEqual({ current: '0.15.0', ready: '0.16.0' });
    await expect(shell.applyUpdate()).resolves.toBeUndefined();
    const stop = await shell.onUpdate(() => {
      throw new Error('the stand-in announces no update');
    });
    stop();
  });

  it('keeps every intent in flight when held, and changes nothing', async () => {
    const api = createMockApi(fixture('ready'), { hold: true });
    let settled = false;
    void api.dispatch({ kind: 'enable' }).finally(() => {
      settled = true;
    });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(settled).toBe(false);
    expect((await api.snapshot()).phase).toEqual({ kind: 'ready' });
  });
});

describe('the Tauri bridge', () => {
  afterEach(() => vi.clearAllMocks());

  it('sends intents to the dispatch command and turns rejections into UserError', async () => {
    const core = await import('@tauri-apps/api/core');
    const invoke = vi.mocked(core.invoke);
    const { tauriApi } = await import('../src/api');

    invoke.mockResolvedValueOnce(null);
    await tauriApi.dispatch({ kind: 'choose_output', value: 'id-1' });
    expect(invoke).toHaveBeenCalledWith('dispatch', { intent: { kind: 'choose_output', value: 'id-1' } });

    invoke.mockRejectedValueOnce({ message: '지금은 할 수 없는 동작입니다.', next: 'none' });
    await expect(tauriApi.dispatch({ kind: 'begin_setup' })).rejects.toEqual({
      message: '지금은 할 수 없는 동작입니다.',
      next: 'none',
    });

    invoke.mockRejectedValueOnce('command dispatch not found');
    await expect(tauriApi.dispatch({ kind: 'quit' })).rejects.toEqual({
      message: '앱과 연결하지 못했습니다: command dispatch not found',
      next: 'none',
    });
  });

  it('checks the snapshot it receives', async () => {
    const core = await import('@tauri-apps/api/core');
    const invoke = vi.mocked(core.invoke);
    const { tauriApi } = await import('../src/api');

    invoke.mockResolvedValueOnce(fixture('ready'));
    expect((await tauriApi.snapshot()).phase).toEqual({ kind: 'ready' });
    expect(invoke).toHaveBeenCalledWith('snapshot');

    invoke.mockResolvedValueOnce({ phase: { kind: 'ready' } });
    await expect(tauriApi.snapshot()).rejects.toMatchObject({ next: 'none' });
  });

  it('listens to the snapshot and meters events', async () => {
    const event = await import('@tauri-apps/api/event');
    const listen = vi.mocked(event.listen);
    const handlers = new Map<string, (event: { payload: unknown }) => void>();
    listen.mockImplementation(async (name, handler) => {
      handlers.set(String(name), handler as (event: { payload: unknown }) => void);
      return () => {};
    });
    const { tauriApi } = await import('../src/api');

    const drawn: string[] = [];
    const problems: string[] = [];
    await tauriApi.onSnapshot(
      (snapshot) => drawn.push(snapshot.phase.kind),
      (error) => problems.push(error.message),
    );
    const meters: number[] = [];
    await tauriApi.onMeters((engine) => meters.push(engine.latency.p50_ms));

    handlers.get('snapshot')?.({ payload: fixture('running') });
    handlers.get('snapshot')?.({ payload: { phase: 'running' } });
    handlers.get('meters')?.({ payload: fixture('running').engine });
    handlers.get('meters')?.({ payload: null });

    expect(drawn).toEqual(['running']);
    expect(problems).toHaveLength(1);
    expect(meters).toEqual([12.4]);
  });
});
