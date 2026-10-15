// The profile screen: BRIR set, headphone FIR, gain, per-direction test
// tones, and the place the object map will take. Rust refuses these intents
// for now; the refusal shows in the band like any other.

import { h } from '../dom';
import { oneDecimal } from '../format';
import { text } from '../text';
import { type Context, type Drawn, button, intentButton, panel } from './common';

const p = text.profile;

/** Direction ids offered for the test tone until layouts carry names. */
const TEST_DIRECTIONS = [0, 1, 2, 3, 4, 5, 6, 7];

export const GAIN_MIN = -20;
export const GAIN_MAX = 10;

function picker(ctx: Context, label: string, key: string, pick: () => Promise<string | null>, send: (path: string) => void) {
  return button(
    label,
    () => {
      pick().then(
        (path) => {
          if (path !== null) send(path);
        },
        (reason: unknown) => ctx.report({ message: String(reason), next: 'none' }),
      );
    },
    { key, disabled: ctx.busy },
  );
}

function gain(ctx: Context): HTMLElement {
  const readout = h('output', { class: 'num gain-readout', for: 'gain' }, `${oneDecimal(ctx.view.gain)} ${p.gainUnit}`);
  const slider = h('input', {
    id: 'gain',
    type: 'range',
    min: GAIN_MIN,
    max: GAIN_MAX,
    step: 0.5,
    value: ctx.view.gain,
    'data-key': 'gain',
    disabled: ctx.busy,
    'aria-valuetext': `${oneDecimal(ctx.view.gain)} dB`,
  });
  slider.addEventListener('input', () => {
    const value = Number(slider.value);
    readout.textContent = `${oneDecimal(value)} ${p.gainUnit}`;
    slider.setAttribute('aria-valuetext', `${oneDecimal(value)} dB`);
  });
  // `change` fires when the slider is let go (or a key step ends).
  slider.addEventListener('change', () => {
    const value = Number(slider.value);
    ctx.send({ kind: 'set_gain', value }, (ok) => {
      if (ok) ctx.setView({ gain: value });
    });
  });
  return h(
    'div',
    { class: 'gain' },
    h('label', { for: 'gain', class: 'visually-hidden' }, p.gainLabel),
    h('span', { class: 'gain-scale muted small' }, `${GAIN_MIN}`),
    slider,
    h('span', { class: 'gain-scale muted small' }, `+${GAIN_MAX}`),
    readout,
  );
}

export function profile(ctx: Context): Drawn {
  const shell = ctx.shell;
  const brir = panel(p.brirTitle, [
    h('p', { class: 'body-text' }, p.brirBody),
    h(
      'div',
      { class: 'actions' },
      picker(ctx, p.brirImport, 'brir', () => shell.pickFolder(p.brirImport), (path) =>
        ctx.send({ kind: 'import_brir', value: path }),
      ),
    ),
  ]);
  const fir = panel(p.firTitle, [
    h('p', { class: 'body-text' }, p.firBody),
    h(
      'div',
      { class: 'actions' },
      picker(
        ctx,
        p.firImport,
        'fir',
        () => shell.pickFile(p.firImport, { name: p.firFilter, extensions: ['wav'] }),
        (path) => ctx.send({ kind: 'import_headphone_fir', value: path }),
      ),
    ),
  ]);
  const gainPanel = panel(p.gainTitle, [h('p', { class: 'body-text' }, p.gainBody), gain(ctx)]);
  const directions = h('div', { class: 'direction-grid', role: 'group', 'aria-label': p.testLabel });
  for (const id of TEST_DIRECTIONS) {
    directions.append(intentButton(ctx, p.direction(id), { kind: 'test_direction', value: id }, { key: `direction-${id}` }));
  }
  const test = panel(p.testTitle, [h('p', { class: 'body-text' }, p.testBody), directions]);
  const map = panel(p.mapTitle, [h('div', { class: 'placeholder' }, h('p', null, p.mapBody))], 'object-map');

  const element = h(
    'div',
    { class: 'profile' },
    h('p', { class: 'lead' }, p.defaultNote),
    h('div', { class: 'profile-grid' }, brir, fir, gainPanel, test),
    map,
  );
  return { element };
}
