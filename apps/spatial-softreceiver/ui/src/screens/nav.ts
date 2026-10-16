// The narrow column on the left: the mark, the four screens and a one-word
// power state so it is visible from every screen.

import type { Intent, Snapshot } from '../bindings';
import { h } from '../dom';
import { text } from '../text';
import type { Context, Tab } from './common';

const TABS: Tab[] = ['status', 'profile', 'upmix', 'settings'];

/**
 * The power state in one word, and its tone. `pending` is the intent in
 * flight (`Context.pending`): while an `enable` is on its way the snapshot
 * still says off, but the screen knows it is turning on.
 */
export function powerState(
  snapshot: Snapshot,
  pending: Intent['kind'] | null,
): { label: string; tone: 'off' | 'on' | 'warning' | 'error' } {
  if (pending === 'enable') return { label: text.status.powerTurningOn, tone: 'warning' };
  switch (snapshot.phase.kind) {
    case 'running':
      return snapshot.engine?.fault
        ? { label: text.status.reconnecting, tone: 'warning' }
        : { label: text.status.on, tone: 'on' };
    case 'faulted':
      return { label: text.status.stopped, tone: 'error' };
    default:
      return { label: text.status.off, tone: 'off' };
  }
}

export function brand(): HTMLElement {
  return h(
    'div',
    { class: 'brand' },
    h('img', { class: 'brand-mark', src: '/mark.svg', alt: '', width: 28, height: 28 }),
    h('span', { class: 'brand-name' }, text.product),
  );
}

export function nav(ctx: Context): HTMLElement {
  const list = h('ul', { class: 'nav-list' });
  for (const tab of TABS) {
    const label = text.nav[tab];
    const current = ctx.view.tab === tab;
    list.append(
      h(
        'li',
        null,
        h(
          'button',
          {
            type: 'button',
            class: 'nav-item',
            'data-key': `nav-${tab}`,
            'aria-current': current ? 'page' : null,
            onClick: () => ctx.setView({ tab }),
          },
          label,
        ),
      ),
    );
  }
  const power = powerState(ctx.snapshot, ctx.pending);
  // The rail paints the column the whole page down; the nav inside it stays
  // in view while the page scrolls.
  return h(
    'aside',
    { class: 'nav-rail' },
    h(
      'nav',
      { class: 'nav', 'aria-label': text.nav.label },
      brand(),
      list,
      h(
        'p',
        { class: `nav-power tone-${power.tone}` },
        h('span', { class: 'lamp', 'aria-hidden': 'true' }),
        power.label,
      ),
    ),
  );
}
