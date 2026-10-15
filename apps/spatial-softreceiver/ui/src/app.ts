// The screen: draws a Snapshot, sends Intents, and keeps only view state
// (which tab is open, a closed band, an intent in flight). Everything else
// comes from Rust in the next snapshot (ADR 0001).

import { type Api, toUserError } from './api';
import type { EngineStatus, Intent, Snapshot, UserError } from './bindings';
import { h } from './dom';
import { setLanguage, text } from './text';
import { type Band, banners, notes } from './screens/banner';
import type { Context, Live, Tab, ViewState } from './screens/common';
import { nav } from './screens/nav';
import { profile } from './screens/profile';
import { settings } from './screens/settings';
import { status } from './screens/status';
import { quit, wizard } from './screens/wizard';

export interface App {
  /** Draws a snapshot (the shell's `snapshot` event). */
  draw(snapshot: Snapshot): void;
  /** Updates the meters in place (the shell's `meters` event). */
  updateMeters(engine: EngineStatus): void;
  /** Shows an error that did not come in a snapshot. */
  report(error: UserError): void;
  /** Sends an intent as a button would. */
  send(intent: Intent): Promise<void>;
}

export interface AppOptions {
  tab?: Tab;
}

/** The parts of a snapshot whose change ends a refused intent's band. */
function stateKey(snapshot: Snapshot): string {
  return JSON.stringify({
    ...snapshot,
    engine: snapshot.engine === null ? null : { fault: snapshot.engine.fault },
  });
}

/** How long the buttons wait for the snapshot that follows an intent. */
const SETTLE_MS = 1000;

const sameError = (a: UserError | null, b: UserError | null) =>
  a !== null && b !== null && a.message === b.message && a.next === b.next;

export function mountApp(root: HTMLElement, api: Api, options: AppOptions = {}): App {
  const view: ViewState = {
    tab: options.tab ?? 'status',
    outputsOpen: false,
    dismissed: null,
    gainSending: null,
    autostart: null,
    autostartWanted: null,
    update: null,
  };
  let snapshot: Snapshot | null = null;
  let lastKey = '';
  let transient: UserError | null = null;
  let pending: Intent['kind'] | null = null;
  let live: Live | null = null;
  let meters: EngineStatus | null = null;
  let focusKey: string | null = null;

  function setView(patch: Partial<ViewState>) {
    Object.assign(view, patch);
    render();
  }

  function report(error: UserError) {
    transient = error;
    render();
  }

  /** Resolves at the next drawn snapshot, or after `ms` without one. */
  let nextDraw: (() => void)[] = [];
  function snapshotLanded(ms: number): Promise<void> {
    return new Promise((resolve) => {
      const timer = setTimeout(resolve, ms);
      nextDraw.push(() => {
        clearTimeout(timer);
        resolve();
      });
    });
  }

  async function send(intent: Intent, after?: (ok: boolean) => void): Promise<void> {
    if (pending !== null) return;
    pending = intent.kind;
    transient = null;
    render();
    let ok = true;
    try {
      await api.dispatch(intent);
    } catch (reason) {
      ok = false;
      transient = toUserError(reason);
    }
    after?.(ok);
    // The shell replies first and publishes the snapshot right after
    // (apps/spatial-softreceiver/src/control.rs); the buttons stay locked
    // until it lands so they do not flash back to the old state.
    render();
    await snapshotLanded(SETTLE_MS);
    pending = null;
    render();
  }

  function chooseOutput() {
    if (snapshot?.phase.kind === 'setup') {
      root.querySelector<HTMLElement>('.output-list button')?.focus();
      return;
    }
    setView({ tab: 'status', outputsOpen: true });
  }

  function dismiss(error: UserError) {
    if (sameError(error, transient)) transient = null;
    if (sameError(error, snapshot?.error ?? null)) view.dismissed = error.message;
    render();
  }

  function render() {
    if (snapshot === null) return;
    setLanguage(snapshot.language);
    document.documentElement.lang = snapshot.language;
    document.title = text.product;
    const active = document.activeElement;
    const activeKey = active instanceof HTMLElement && root.contains(active) ? (active.dataset.key ?? null) : null;
    focusKey = activeKey ?? focusKey;

    const offered = new Set<Intent['kind']>();
    const ctx: Context = {
      snapshot,
      view,
      busy: pending !== null,
      pending,
      shell: api.shell,
      send: (intent, after) => void send(intent, after),
      setView,
      report,
      offer: (kind) => offered.add(kind),
    };

    const phase = snapshot.phase;
    const wizardMode = phase.kind === 'setup' || phase.kind === 'quit';
    live = null;
    let head: HTMLElement;
    let body: HTMLElement;
    if (phase.kind === 'setup') {
      ({ head, body } = wizard(ctx, phase.value));
    } else if (phase.kind === 'quit') {
      ({ head, body } = quit());
    } else {
      const titles: Record<Tab, string> = {
        status: text.status.title,
        profile: text.profile.title,
        settings: text.settings.title,
      };
      head = h('header', { class: 'main-head' }, h('h1', null, titles[view.tab]));
      const engineNow = phase.kind === 'running' ? (meters ?? snapshot.engine) : snapshot.engine;
      const drawn =
        view.tab === 'status'
          ? status(ctx, engineNow)
          : view.tab === 'profile'
            ? profile(ctx, engineNow)
            : settings(ctx);
      body = drawn.element;
      live = drawn.live ?? null;
    }

    const errors: Band[] = [];
    const kept = snapshot.error;
    if (transient !== null && !sameError(transient, kept)) errors.push({ error: transient, kept: false });
    if (kept !== null && !(kept.next === 'none' && view.dismissed === kept.message)) {
      errors.push({ error: kept, kept: true });
    }
    const alerts = h(
      'div',
      { class: 'alerts', 'aria-live': 'polite' },
      banners(ctx, { errors, offered, chooseOutput, dismiss }),
      notes(snapshot.notes),
    );

    const main = h('main', { class: 'main', id: 'main' }, head, alerts, h('div', { class: 'body' }, body));
    const frame = h(
      'div',
      { class: `app ${wizardMode ? 'mode-wizard' : 'mode-main'}`, 'data-phase': phase.kind },
      wizardMode ? null : nav(ctx),
      main,
    );
    root.replaceChildren(frame);
    root.dataset.ready = 'true';

    if (focusKey !== null) {
      const target = root.querySelector<HTMLElement>(`[data-key="${CSS.escape(focusKey)}"]`);
      if (target !== null && !(target as HTMLButtonElement).disabled) {
        // A redraw must not scroll the page back to the focused control.
        target.focus({ preventScroll: true });
        focusKey = null;
      } else if (target === null) {
        focusKey = null;
      }
    }
  }

  function draw(next: Snapshot) {
    const key = stateKey(next);
    if (key !== lastKey) {
      // A refused intent's band lasts until the state moves on.
      if (lastKey !== '') transient = null;
      lastKey = key;
    }
    if (view.dismissed !== null && next.error?.message !== view.dismissed) view.dismissed = null;
    if (next.phase.kind !== 'ready') view.outputsOpen = false;
    meters = null;
    snapshot = next;
    render();
    const waiting = nextDraw;
    nextDraw = [];
    for (const resolve of waiting) resolve();
  }

  function updateMeters(engine: EngineStatus) {
    if (snapshot?.phase.kind !== 'running') return;
    meters = engine;
    live?.update(engine);
  }

  if (api.shell.native) {
    api.shell.updateState().then(
      (update) => setView({ update }),
      () => setView({ update: null }),
    );
    void api.shell.onUpdate((update) => setView({ update }));
    api.shell.autostartEnabled().then(
      (enabled) => setView({ autostart: enabled }),
      () => setView({ autostart: null }),
    );
  }

  return { draw, updateMeters, report, send: (intent) => send(intent) };
}
