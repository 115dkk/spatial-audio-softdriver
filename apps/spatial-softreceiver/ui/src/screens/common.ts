// What every screen gets from the app, and the pieces they share.

import type { Shell } from '../api';
import type { EngineStatus, Intent, Snapshot, UserError } from '../bindings';
import { type Child, h } from '../dom';

export type Tab = 'status' | 'profile' | 'settings';

/** View-only state: which tab is open and the like. Nothing Rust decides. */
export interface ViewState {
  tab: Tab;
  /** The output list under the status header is open. */
  outputsOpen: boolean;
  /** The error sentence the user closed; shown again once it changes. */
  dismissed: string | null;
  /** The last gain Rust accepted, so a redraw does not move the slider. */
  gain: number;
  /** Login autostart as last read from the OS; null until read. */
  autostart: boolean | null;
  /** The autostart value being set; null when no change is in flight. */
  autostartWanted: boolean | null;
}

export interface Context {
  readonly snapshot: Snapshot;
  readonly view: Readonly<ViewState>;
  /** An intent is in flight; every intent button is disabled. */
  readonly busy: boolean;
  /** The kind of the intent in flight. */
  readonly pending: Intent['kind'] | null;
  readonly shell: Shell;
  /** Sends an intent unless one is already in flight. */
  send(intent: Intent, after?: (ok: boolean) => void): void;
  /** Changes view state and redraws. */
  setView(patch: Partial<ViewState>): void;
  /** Shows an error that is not in the snapshot (a refused intent). */
  report(error: UserError): void;
  /** Records that the body offers this intent, so the banner need not. */
  offer(kind: Intent['kind']): void;
}

/** The status screen's live part, updated by the 30 Hz meters. */
export interface Live {
  update(engine: EngineStatus): void;
}

export interface Drawn {
  element: HTMLElement;
  live?: Live;
}

export interface ButtonOptions {
  /** Stable key so focus survives a redraw. */
  key: string;
  variant?: 'primary' | 'secondary' | 'quiet';
  /** Label while this intent is in flight. */
  pendingLabel?: string;
  /** Extra classes. */
  className?: string;
}

/** A button that sends one intent; disabled while any intent is in flight. */
export function intentButton(ctx: Context, label: string, intent: Intent, options: ButtonOptions): HTMLButtonElement {
  ctx.offer(intent.kind);
  const pending = ctx.pending === intent.kind;
  const button = h(
    'button',
    {
      type: 'button',
      class: ['button', options.variant ?? 'secondary', options.className].filter(Boolean).join(' '),
      'data-key': options.key,
      'data-intent': intent.kind,
      disabled: ctx.busy,
      'aria-busy': pending ? 'true' : null,
      onClick: () => ctx.send(intent),
    },
    pending && options.pendingLabel ? options.pendingLabel : label,
  );
  return button;
}

/** A plain button for view changes and OS calls. */
export function button(
  label: string,
  onClick: () => void,
  options: { key: string; variant?: 'primary' | 'secondary' | 'quiet'; disabled?: boolean; className?: string },
): HTMLButtonElement {
  return h(
    'button',
    {
      type: 'button',
      class: ['button', options.variant ?? 'secondary', options.className].filter(Boolean).join(' '),
      'data-key': options.key,
      disabled: options.disabled ?? false,
      onClick,
    },
    label,
  );
}

/** A titled panel. */
export function panel(title: string | null, children: Child[], className = ''): HTMLElement {
  return h(
    'section',
    { class: `panel ${className}`.trim() },
    title === null ? null : h('h2', { class: 'panel-title' }, title),
    ...children,
  );
}
