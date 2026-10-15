// The one bridge between the screen and Rust (ADR 0001): the screen sends an
// Intent and draws the whole Snapshot it gets back. Inside Tauri this is the
// shell's `dispatch` and `snapshot` commands and its `snapshot` and `meters`
// events; in a plain browser (gallery, `npm run dev`) it is the mock in
// mock.ts, which starts from the fixtures.

import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import * as autostart from '@tauri-apps/plugin-autostart';
import { open } from '@tauri-apps/plugin-dialog';
import type { EngineStatus, Intent, Snapshot, UserError } from './bindings';
import { assertEngineStatus, assertSnapshot, assertUserError } from './guard';
import { text } from './text';

export type Unlisten = () => void;

/** What the screen asks of the OS rather than of the supervisor. */
/** What the shell's updater holds (the `update_state` command and event). */
export interface UpdateState {
  /** The running version. */
  current: string;
  /** A version downloaded and waiting for a restart. */
  ready: string | null;
}

export interface Shell {
  /** False outside Tauri: the dialogs, autostart and updates are not there. */
  readonly native: boolean;
  pickFolder(title: string): Promise<string | null>;
  pickFile(title: string, filter: { name: string; extensions: string[] }): Promise<string | null>;
  autostartEnabled(): Promise<boolean>;
  setAutostart(enabled: boolean): Promise<void>;
  updateState(): Promise<UpdateState>;
  /** Quits the app and applies the downloaded update; rejects with a UserError. */
  applyUpdate(): Promise<void>;
  /** The shell says an update was downloaded. */
  onUpdate(update: (state: UpdateState) => void): Promise<Unlisten>;
}

export interface Api {
  /** Applies an intent; a refusal or failure rejects with its UserError. */
  dispatch(intent: Intent): Promise<void>;
  snapshot(): Promise<Snapshot>;
  /** Every snapshot Rust emits; `problem` gets the ones that do not parse. */
  onSnapshot(draw: (snapshot: Snapshot) => void, problem?: (error: UserError) => void): Promise<Unlisten>;
  /** The running engine's status, about 30 times a second. */
  onMeters(update: (engine: EngineStatus) => void): Promise<Unlisten>;
  readonly shell: Shell;
}

/** Whether the page runs inside the Tauri shell. */
export function inTauri(): boolean {
  return typeof window !== 'undefined' && ('__TAURI_INTERNALS__' in window || isTauri());
}

/** Whatever a rejected command carried, as a sentence the banner can show. */
export function toUserError(reason: unknown): UserError {
  try {
    return assertUserError(reason);
  } catch {
    const detail = reason instanceof Error ? reason.message : String(reason);
    return { message: text.bridge.unreachable(detail), next: 'none' };
  }
}

function parseSnapshot(value: unknown): Snapshot {
  try {
    return assertSnapshot(value);
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    throw { message: text.bridge.badSnapshot(detail), next: 'none' } satisfies UserError;
  }
}

const tauriShell: Shell = {
  native: true,
  async pickFolder(title) {
    return open({ directory: true, multiple: false, title });
  },
  async pickFile(title, filter) {
    return open({ directory: false, multiple: false, title, filters: [filter] });
  },
  autostartEnabled: () => autostart.isEnabled(),
  setAutostart: (enabled) => (enabled ? autostart.enable() : autostart.disable()),
  updateState: () => invoke<UpdateState>('update_state'),
  async applyUpdate() {
    try {
      await invoke<null>('apply_update');
    } catch (reason) {
      throw toUserError(reason);
    }
  },
  onUpdate: (update) => listen<UpdateState>('update', (event) => update(event.payload)),
};

/** The Tauri shell's commands and events. */
export const tauriApi: Api = {
  async dispatch(intent) {
    try {
      await invoke<null>('dispatch', { intent });
    } catch (reason) {
      throw toUserError(reason);
    }
  },
  async snapshot() {
    let value: unknown;
    try {
      value = await invoke<unknown>('snapshot');
    } catch (reason) {
      throw toUserError(reason);
    }
    return parseSnapshot(value);
  },
  onSnapshot(draw, problem) {
    return listen<unknown>('snapshot', (event) => {
      let snapshot: Snapshot;
      try {
        snapshot = parseSnapshot(event.payload);
      } catch (error) {
        problem?.(toUserError(error));
        return;
      }
      draw(snapshot);
    });
  },
  onMeters(update) {
    let warned = false;
    return listen<unknown>('meters', (event) => {
      // Outside Running the shell may send nothing or null; the snapshot
      // already says the engine is off.
      if (event.payload === null || event.payload === undefined) return;
      try {
        update(assertEngineStatus(event.payload));
      } catch (error) {
        if (!warned) console.warn('meters payload does not match EngineStatus', error);
        warned = true;
      }
    });
  },
  shell: tauriShell,
};

/**
 * The bridge for this page: Tauri's, or the mock seeded with `fixtureName`
 * (default: the welcome step) when the page runs in a plain browser.
 */
export async function connect(fixtureName?: string): Promise<Api> {
  if (inTauri()) return tauriApi;
  const { createMockApi } = await import('./mock');
  const { fixture } = await import('./fixtures');
  return createMockApi(fixture(fixtureName ?? 'setup-welcome'), { meters: true });
}
