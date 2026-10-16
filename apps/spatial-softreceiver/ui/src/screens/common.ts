// What every screen gets from the app, and the pieces they share.

import type { Shell, UpdateState } from '../api';
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
  /**
   * The gain a `set_gain` in flight carries, so the slider holds it until
   * the snapshot that follows lands. The slider otherwise shows the
   * snapshot's `profile.gain_db`.
   */
  gainSending: number | null;
  /** Login autostart as last read from the OS; null until read. */
  autostart: boolean | null;
  /** The autostart value being set; null when no change is in flight. */
  autostartWanted: boolean | null;
  /** The shell's updater state; null until read. */
  update: UpdateState | null;
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

/** A screen's live part, updated in place by the 30 Hz meters. */
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
  /**
   * How the button shows its intent in flight. `'label'` (the default)
   * writes `pendingLabel` in place of the label. `'spinner'` hides the label
   * where it stands, so the button keeps its width, turns a spinner over it
   * and gives `pendingLabel` to screen readers only.
   */
  busy?: 'label' | 'spinner';
  /** Extra classes. */
  className?: string;
}

/** A button that sends one intent; disabled while any intent is in flight. */
export function intentButton(ctx: Context, label: string, intent: Intent, options: ButtonOptions): HTMLButtonElement {
  ctx.offer(intent.kind);
  const pending = ctx.pending === intent.kind;
  const spinning = pending && options.busy === 'spinner';
  const button = h(
    'button',
    {
      type: 'button',
      class: ['button', options.variant ?? 'secondary', options.className, spinning ? 'is-spinning' : null]
        .filter(Boolean)
        .join(' '),
      'data-key': options.key,
      'data-intent': intent.kind,
      disabled: ctx.busy,
      'aria-busy': pending ? 'true' : null,
      onClick: () => ctx.send(intent),
    },
    ...(spinning
      ? spinnerLabel(label, options.pendingLabel ?? label)
      : [pending && options.pendingLabel ? options.pendingLabel : label]),
  );
  return button;
}

/**
 * A busy button's content: the label, kept only for its width (neither seen
 * nor read), the spinner over it, and what a screen reader says instead.
 */
function spinnerLabel(label: string, spoken: string): Child[] {
  return [
    h('span', { class: 'button-label', 'aria-hidden': 'true' }, label),
    h('span', { class: 'spinner', 'aria-hidden': 'true' }),
    h('span', { class: 'visually-hidden' }, spoken),
  ];
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
