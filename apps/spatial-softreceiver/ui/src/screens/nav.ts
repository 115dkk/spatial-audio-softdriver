// The narrow column on the left: the mark, the three screens and a one-word
// power state so it is visible from every screen.

import type { Snapshot } from '../bindings';
import { h } from '../dom';
import { text } from '../text';
import type { Context, Tab } from './common';

const TABS: Tab[] = ['status', 'profile', 'settings'];

/** The power state in one word, and its tone. */
export function powerState(snapshot: Snapshot): { label: string; tone: 'off' | 'on' | 'warning' | 'error' } {
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
  const power = powerState(ctx.snapshot);
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
