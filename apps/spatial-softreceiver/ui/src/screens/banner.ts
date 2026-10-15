// The error band above the body and the notes under it. The sentence and the
// next action come from Rust; the screen only maps the action to a button.

import type { Intent, NextAction, Phase, UserError } from '../bindings';
import { h } from '../dom';
import { text } from '../text';
import { type Context, intentButton } from './common';

export type BannerAction =
  /** A button that sends this intent. */
  | { kind: 'intent'; intent: Intent; label: string }
  /** A button that opens the output list. */
  | { kind: 'choose_output'; label: string }
  /** No action; the band can be closed (outside the wizard). */
  | { kind: 'dismiss'; label: string }
  /** No button at all. */
  | { kind: 'nothing' };

/** What the band offers for `next` in `phase`. */
export function bannerAction(phase: Phase, next: NextAction): BannerAction {
  switch (next) {
    case 'retry':
      switch (phase.kind) {
        case 'setup':
          return phase.value === 'driver'
            ? { kind: 'intent', intent: { kind: 'install_driver' }, label: text.banner.retry }
            : { kind: 'intent', intent: { kind: 'recheck' }, label: text.banner.recheck };
        case 'ready':
          return { kind: 'intent', intent: { kind: 'enable' }, label: text.banner.retry };
        case 'faulted':
          return { kind: 'intent', intent: { kind: 'recheck' }, label: text.banner.recheck };
        case 'running':
        case 'quit':
          return { kind: 'nothing' };
      }
      break;
    case 'reboot':
      return { kind: 'intent', intent: { kind: 'recheck' }, label: text.banner.recheck };
    case 'open_settings':
      return { kind: 'choose_output', label: text.banner.chooseOutput };
    case 'none':
      // In the wizard the band is the step's explanation (an unsupported PC
      // has nothing else to read), so it stays.
      return phase.kind === 'setup' ? { kind: 'nothing' } : { kind: 'dismiss', label: text.banner.dismiss };
  }
}

export interface Band {
  error: UserError;
  /** From the snapshot (true) or a refused intent's reply (false). */
  kept: boolean;
}

export interface BannerInput {
  /** A refused intent's error first, then the snapshot's. */
  errors: Band[];
  /** Intents the body already offers; the band does not repeat them. */
  offered: ReadonlySet<Intent['kind']>;
  /** Opens the output list (status tab, list open). */
  chooseOutput(): void;
  /** Closes a band whose action is none. */
  dismiss(error: UserError): void;
}

function band(ctx: Context, { error, kept }: Band, input: BannerInput, index: number): HTMLElement {
  let action = bannerAction(ctx.snapshot.phase, error.next);
  // A refusal is not the step's explanation; it can always be closed.
  if (!kept && action.kind === 'nothing' && error.next === 'none') {
    action = { kind: 'dismiss', label: text.banner.dismiss };
  }
  const message = h('div', { class: 'banner-text' }, h('p', { class: 'banner-message' }, error.message));
  if (error.next === 'reboot') message.append(h('p', { class: 'banner-hint' }, text.banner.rebootHint));

  let control: HTMLElement | null = null;
  switch (action.kind) {
    case 'intent':
      if (!input.offered.has(action.intent.kind)) {
        control = intentButton(ctx, action.label, action.intent, { key: `banner-${index}`, variant: 'secondary' });
      }
      break;
    case 'choose_output':
      control = h(
        'button',
        { type: 'button', class: 'button secondary', 'data-key': `banner-${index}`, onClick: input.chooseOutput },
        action.label,
      );
      break;
    case 'dismiss':
      control = h(
        'button',
        {
          type: 'button',
          class: 'button quiet banner-close',
          'data-key': `banner-${index}`,
          onClick: () => input.dismiss(error),
        },
        action.label,
      );
      break;
    case 'nothing':
      break;
  }
  const severity = error.next === 'none' && ctx.snapshot.phase.kind === 'running' ? 'warning' : 'error';
  return h(
    'div',
    { class: `banner ${severity}`, role: 'alert' },
    h('span', { class: 'banner-mark', 'aria-hidden': 'true' }),
    message,
    control === null ? null : h('div', { class: 'banner-action' }, control),
  );
}

/** The bands for `errors`, or null when there are none. */
export function banners(ctx: Context, input: BannerInput): HTMLElement | null {
  if (input.errors.length === 0) return null;
  const list = h('div', { class: 'banners', 'aria-label': text.banner.errorLabel });
  input.errors.forEach((entry, index) => list.append(band(ctx, entry, input, index)));
  return list;
}

/** The snapshot's notes, as quiet lines. */
export function notes(lines: string[]): HTMLElement | null {
  if (lines.length === 0) return null;
  return h(
    'ul',
    { class: 'notes', 'aria-label': text.banner.notesLabel },
    ...lines.map((line) => h('li', null, line)),
  );
}
