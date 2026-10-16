// What every screen gets from the app, and the pieces they share.

import type { Shell, UpdateState } from '../api';
import type { EngineStatus, Intent, ProfileKind, Snapshot, UserError } from '../bindings';
import { type Child, h, svg } from '../dom';

export type Tab = 'status' | 'profile' | 'upmix' | 'settings';

/** View-only state: which tab is open and the like. Nothing Rust decides. */
export interface ViewState {
  tab: Tab;
  /** The output list under the status header is open. */
  outputsOpen: boolean;
  /**
   * The output kind whose settings the profile screen shows. `null` follows
   * `snapshot.profile.kind`, so a reopened window shows the kind in use.
   * Opening a tab sends nothing.
   */
  profileTab: ProfileKind | null;
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
  /**
   * The intent in flight, for controls that show what they are sending
   * (which endpoint's upmixer, which way a switch goes).
   */
  readonly sending: Intent | null;
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

/** The ids that describe a control, or null when none does. */
export function described(...ids: (string | false | null)[]): string | null {
  const present = ids.filter((id): id is string => typeof id === 'string');
  return present.length === 0 ? null : present.join(' ');
}

/**
 * A button that asks the shell for a file or a folder and sends the path
 * picked; disabled while any intent is in flight or when `locked`.
 */
export function picker(
  ctx: Context,
  label: string,
  key: string,
  locked: boolean,
  describedBy: string | null,
  pick: () => Promise<string | null>,
  send: (path: string) => void,
): HTMLButtonElement {
  const element = button(
    label,
    () => {
      pick().then(
        (path) => {
          if (path !== null) send(path);
        },
        (reason: unknown) => ctx.report({ message: String(reason), next: 'none' }),
      );
    },
    { key, disabled: ctx.busy || locked },
  );
  if (describedBy !== null) element.setAttribute('aria-describedby', describedBy);
  return element;
}

export interface SwitchOptions {
  /** Stable key so focus survives a redraw; also the element id. */
  key: string;
  /** Visible label; it is the switch's name. */
  label: string;
  /** On or off, as the switch shows it. */
  on: boolean;
  disabled: boolean;
  /** The change is in flight. */
  busy: boolean;
  describedBy: string | null;
  onToggle: (on: boolean) => void;
}

/**
 * An on/off switch (`role="switch"`): a button, so Space and Enter toggle
 * it, with the track and thumb drawn by the stylesheet and the label beside
 * them inside the button, so the whole row is the target.
 */
export function switchControl(options: SwitchOptions): HTMLButtonElement {
  return h(
    'button',
    {
      type: 'button',
      role: 'switch',
      id: options.key,
      class: 'switch',
      'data-key': options.key,
      'aria-checked': options.on ? 'true' : 'false',
      'aria-busy': options.busy ? 'true' : null,
      'aria-describedby': options.describedBy,
      disabled: options.disabled,
      onClick: () => options.onToggle(!options.on),
    },
    h('span', { class: 'switch-track', 'aria-hidden': 'true' }, h('span', { class: 'switch-thumb' })),
    h('span', { class: 'switch-label' }, options.label),
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

/** Tips closed with Escape, by key, so a redraw that gives the focus back does not open them again. */
const closedTips = new Set<string>();

/** The (i) drawn as a ring, a dot and a stem in the text colour. */
function infoGlyph(): SVGSVGElement {
  return svg(
    'svg',
    { class: 'info-tip-glyph', viewBox: '0 0 16 16', width: 16, height: 16, 'aria-hidden': 'true', focusable: 'false' },
    svg('circle', { cx: 8, cy: 8, r: 6.75, fill: 'none', stroke: 'currentColor', 'stroke-width': 1.5 }),
    svg('circle', { cx: 8, cy: 5, r: 1, fill: 'currentColor' }),
    svg('path', { d: 'M8 7.5v4', fill: 'none', stroke: 'currentColor', 'stroke-width': 1.5, 'stroke-linecap': 'round' }),
  );
}

/**
 * An (i) button with one line of help. The line opens under it on hover and
 * on focus, stays while the pointer moves onto it, and Escape closes it
 * (WCAG 1.4.13). It is always in the DOM as the button's description
 * (`role="tooltip"`), hidden from sight while closed, so a screen reader
 * reads it either way. `key` keeps the focus across a redraw.
 */
export function infoTip(key: string, label: string, tip: string): HTMLElement {
  const tipId = `${key}-tip`;
  const trigger = h(
    'button',
    { type: 'button', class: 'info-tip', 'data-key': key, 'aria-label': label, 'aria-describedby': tipId },
    infoGlyph(),
  );
  const wrap = h('span', { class: 'info-tip-wrap' }, trigger, h('span', { id: tipId, role: 'tooltip', class: 'info-tip-text' }, tip));
  let hovered = false;
  let focused = false;

  const onKey = (event: KeyboardEvent) => {
    if (event.key !== 'Escape') return;
    document.removeEventListener('keydown', onKey);
    // A tip a redraw took away has nothing left to close.
    if (!wrap.isConnected) return;
    closedTips.add(key);
    update();
  };
  function update() {
    // Pointer and focus both gone: the next hover or focus opens it again.
    if (!hovered && !focused) closedTips.delete(key);
    const open = (hovered || focused) && !closedTips.has(key);
    wrap.classList.toggle('is-open', open);
    if (open) document.addEventListener('keydown', onKey);
    else document.removeEventListener('keydown', onKey);
  }

  trigger.addEventListener('focus', () => {
    focused = true;
    update();
  });
  trigger.addEventListener('blur', () => {
    // Taken away by a redraw: the new button takes the focus over.
    if (!trigger.isConnected) return;
    focused = false;
    update();
  });
  wrap.addEventListener('mouseenter', () => {
    hovered = true;
    update();
  });
  wrap.addEventListener('mouseleave', () => {
    hovered = false;
    update();
  });
  return wrap;
}
