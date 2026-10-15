import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Snapshot } from '../src/bindings';
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
    await expect(api.dispatch({ kind: 'set_gain', value: -3 })).rejects.toEqual({
      message: '아직 지원하지 않는 동작입니다.',
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
