// The profile screen: what the supervisor resolved as the profile, the BRIR
// set and headphone FIR imports, the gain, a test tone per direction of the
// layout, and the object map. Every value comes from `Snapshot.profile`;
// Rust decides whether an import or a tone is taken.

import type { EngineStatus } from '../bindings';
import { facts, h } from '../dom';
import { oneDecimal } from '../format';
import { text } from '../text';
import { type Context, type Drawn, button, intentButton, panel } from './common';
import { objectMap } from './objectmap';

const p = text.profile;

export const GAIN_MIN = -20;
export const GAIN_MAX = 10;

function picker(
  ctx: Context,
  label: string,
  key: string,
  locked: boolean,
  describedBy: string | null,
  pick: () => Promise<string | null>,
  send: (path: string) => void,
) {
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

function current(ctx: Context): HTMLElement {
  const profile = ctx.snapshot.profile;
  const kind = h(
    'span',
    { class: 'profile-kind' },
    p.kindNames[profile.kind],
    profile.from_file ? h('span', { class: 'tag' }, p.fromFileTag) : null,
  );
  const path = (value: string | null) => h('span', { class: value === null ? 'muted' : 'path' }, value ?? p.none);
  return panel(p.currentTitle, [
    facts([
      [p.kind, kind],
      [p.name, profile.name],
      [p.brir, path(profile.brir)],
      [p.fir, path(profile.headphone_fir)],
    ]),
    profile.kind === 'stereo' && !profile.from_file ? h('p', { class: 'hint panel-note' }, p.stereoNote) : null,
  ]);
}

function imports(ctx: Context): HTMLElement {
  const { profile, phase } = ctx.snapshot;
  const shell = ctx.shell;
  // profile.toml wins over every import: one sentence for all three buttons.
  const fromFile = profile.from_file;
  const brirHint = fromFile
    ? null
    : ctx.pending === 'import_brir'
      ? p.importing
      : phase.kind === 'running'
        ? p.importRunning
        : null;
  const firHint = fromFile
    ? null
    : profile.brir === null
      ? p.firNeedsBrir
      : ctx.pending === 'import_headphone_fir'
        ? p.importing
        : null;
  const brirDescribed = fromFile ? 'import-file-hint' : brirHint === null ? null : 'brir-hint';
  const firDescribed = fromFile ? 'import-file-hint' : firHint === null ? null : 'fir-hint';

  const brir = h(
    'div',
    { class: 'import' },
    h('h3', { class: 'import-title' }, p.brir),
    h('p', { class: 'body-text' }, p.brirBody),
    h(
      'div',
      { class: 'actions' },
      picker(
        ctx,
        p.brirFolder,
        'brir-folder',
        fromFile,
        brirDescribed,
        () => shell.pickFolder(p.brirFolderDialog),
        (path) => ctx.send({ kind: 'import_brir', value: path }),
      ),
      picker(
        ctx,
        p.brirFile,
        'brir-file',
        fromFile,
        brirDescribed,
        () => shell.pickFile(p.brirFileDialog, { name: p.brirFilter, extensions: ['wav'] }),
        (path) => ctx.send({ kind: 'import_brir', value: path }),
      ),
    ),
    brirHint === null ? null : h('p', { id: 'brir-hint', class: 'hint import-hint' }, brirHint),
  );
  const fir = h(
    'div',
    { class: 'import' },
    h('h3', { class: 'import-title' }, p.fir),
    h('p', { class: 'body-text' }, p.firBody),
    h(
      'div',
      { class: 'actions' },
      picker(
        ctx,
        p.firImport,
        'fir',
        fromFile || profile.brir === null,
        firDescribed,
        () => shell.pickFile(p.firDialog, { name: p.firFilter, extensions: ['wav'] }),
        (path) => ctx.send({ kind: 'import_headphone_fir', value: path }),
      ),
    ),
    firHint === null ? null : h('p', { id: 'fir-hint', class: 'hint import-hint' }, firHint),
  );
  return panel(
    p.importTitle,
    [fromFile ? h('p', { id: 'import-file-hint', class: 'file-note' }, p.fromFile) : null, brir, fir],
    'imports',
  );
}

function gain(ctx: Context): HTMLElement {
  // The snapshot's gain, except while a set_gain is in flight: then the
  // value being sent, so the slider does not jump back until Rust answers.
  const value =
    ctx.pending === 'set_gain' && ctx.view.gainSending !== null ? ctx.view.gainSending : ctx.snapshot.profile.gain_db;
  const readout = h('output', { class: 'num gain-readout', for: 'gain' }, `${oneDecimal(value)} ${p.gainUnit}`);
  const slider = h('input', {
    id: 'gain',
    type: 'range',
    min: GAIN_MIN,
    max: GAIN_MAX,
    step: 0.5,
    value,
    'data-key': 'gain',
    disabled: ctx.busy,
    'aria-valuetext': `${oneDecimal(value)} dB`,
  });
  slider.addEventListener('input', () => {
    const moved = Number(slider.value);
    readout.textContent = `${oneDecimal(moved)} ${p.gainUnit}`;
    slider.setAttribute('aria-valuetext', `${oneDecimal(moved)} dB`);
  });
  // `change` fires when the slider is let go (or a key step ends).
  slider.addEventListener('change', () => {
    const wanted = Number(slider.value);
    ctx.setView({ gainSending: wanted });
    ctx.send({ kind: 'set_gain', value: wanted });
  });
  return panel(p.gainTitle, [
    h('p', { class: 'body-text' }, p.gainBody),
    h(
      'div',
      { class: 'gain' },
      h('label', { for: 'gain', class: 'visually-hidden' }, p.gainLabel),
      h('span', { class: 'gain-scale muted small' }, `${GAIN_MIN}`),
      slider,
      h('span', { class: 'gain-scale muted small' }, `+${GAIN_MAX}`),
      readout,
    ),
  ]);
}

function tones(ctx: Context): HTMLElement {
  const { profile, phase } = ctx.snapshot;
  const running = phase.kind === 'running';
  const grid = h('div', { class: 'direction-grid', role: 'group', 'aria-label': p.testLabel });
  for (const direction of profile.directions) {
    const tone = intentButton(
      ctx,
      direction.name,
      { kind: 'test_direction', value: direction.id },
      { key: `direction-${direction.id}` },
    );
    tone.title = p.angles(direction.azimuth_deg, direction.elevation_deg);
    if (!running) {
      tone.disabled = true;
      tone.setAttribute('aria-describedby', 'test-hint');
    }
    grid.append(tone);
  }
  const empty = profile.directions.length === 0;
  return panel(p.testTitle, [
    h('p', { class: 'body-text' }, p.testBody),
    empty ? h('p', { class: 'hint import-hint' }, p.noDirections) : grid,
    running || empty ? null : h('p', { id: 'test-hint', class: 'hint import-hint' }, p.testOff),
  ]);
}

export function profile(ctx: Context, engineNow: EngineStatus | null): Drawn {
  const running = ctx.snapshot.phase.kind === 'running';
  // Running, the map follows the engine's layout; otherwise the profile's.
  const engine = running ? engineNow : null;
  const map = objectMap(engine?.directions ?? ctx.snapshot.profile.directions, running);
  if (engine !== null) map.update(engine);

  const element = h(
    'div',
    { class: 'profile' },
    h(
      'div',
      { class: 'profile-layout' },
      h('div', { class: 'profile-controls' }, current(ctx), imports(ctx), gain(ctx), tones(ctx)),
      map.element,
    ),
  );
  return running ? { element, live: { update: map.update } } : { element };
}
