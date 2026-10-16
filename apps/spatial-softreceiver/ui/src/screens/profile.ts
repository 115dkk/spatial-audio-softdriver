// The profile screen: the two outputs side by side as tabs (headphones with
// their BRIR set, speakers with a layout or a SECS 2R correction), the gain
// of the output in use, a test tone per direction of its layout, and the
// object map. Every value comes from `Snapshot.profile`; each tab's upmix
// switch comes from `Snapshot.upmix` (ADR 0035). Which tab is open is view
// state; Rust decides whether a switch, an import or a tone is taken
// (ADR 0034).

import type { EngineStatus, ProfileKind, SpeakerLayout } from '../bindings';
import { type Child, facts, h } from '../dom';
import { oneDecimal } from '../format';
import { SPEAKER_LAYOUTS } from '../guard';
import { text } from '../text';
import { type Context, type Drawn, described, intentButton, panel, picker, switchControl } from './common';
import { objectMap } from './objectmap';

export const GAIN_MIN = -20;
export const GAIN_MAX = 10;

/** The tabs in the order they stand. */
const KINDS: readonly ProfileKind[] = ['headphones', 'speakers'];
/** The app's speaker layouts in the contract's order. */
const LAYOUTS = Object.keys(SPEAKER_LAYOUTS) as SpeakerLayout[];

/** The file note's sentence: profile.toml wins over every change here. */
const FILE_HINT = 'profile-file-hint';
/** While running, a change re-prepares the engine. */
const RUNNING_HINT = 'profile-running-hint';

/** A path as data, or 없음. */
const path = (value: string | null) => h('span', { class: value === null ? 'muted' : 'path' }, value ?? text.profile.none);

/** The ids that describe a change control: the file that locks it, a change that takes seconds. */
function changeHints(ctx: Context): (string | false)[] {
  const { profile, phase } = ctx.snapshot;
  return [profile.from_file && FILE_HINT, !profile.from_file && phase.kind === 'running' && RUNNING_HINT];
}

/** profile.toml decides the output: its tag and name, and the sentence. */
function fileNote(ctx: Context): HTMLElement | null {
  const profile = ctx.snapshot.profile;
  if (!profile.from_file) return null;
  return h(
    'div',
    { class: 'file-note' },
    h(
      'p',
      { class: 'file-note-head' },
      h('span', { class: 'tag' }, text.profile.fromFileTag),
      h('span', { class: 'file-note-name' }, profile.name),
    ),
    h('p', { id: FILE_HINT }, text.profile.fromFile),
  );
}

/** The tab bar: one tab per kind, the kind in use badged. Opening one only changes view state. */
function tabs(ctx: Context, open: ProfileKind): HTMLElement {
  const inUse = ctx.snapshot.profile.kind;
  const choose = (kind: ProfileKind, focus: boolean) => {
    if (focus) list.querySelector<HTMLElement>(`[data-key="profile-tab-${kind}"]`)?.focus();
    ctx.setView({ profileTab: kind });
  };
  const list = h('div', {
    class: 'output-tabs',
    role: 'tablist',
    'aria-label': text.profile.outputTitle,
    onKeydown: (event: Event) => {
      const key = (event as KeyboardEvent).key;
      const at = KINDS.indexOf(open);
      const to =
        key === 'ArrowRight' ? (at + 1) % KINDS.length
        : key === 'ArrowLeft' ? (at - 1 + KINDS.length) % KINDS.length
        : key === 'Home' ? 0
        : key === 'End' ? KINDS.length - 1
        : null;
      const kind = to === null ? undefined : KINDS[to];
      if (kind === undefined) return;
      event.preventDefault();
      // Focus first, so the redraw keeps it on the tab now open.
      choose(kind, true);
    },
  });
  for (const kind of KINDS) {
    const selected = kind === open;
    list.append(
      h(
        'button',
        {
          type: 'button',
          role: 'tab',
          id: `profile-tab-${kind}`,
          class: 'output-tab',
          'data-key': `profile-tab-${kind}`,
          'aria-selected': selected ? 'true' : 'false',
          'aria-controls': selected ? `profile-panel-${kind}` : null,
          tabindex: selected ? 0 : -1,
          onClick: () => choose(kind, false),
        },
        h('span', null, text.profile.kindNames[kind]),
        kind === inUse ? h('span', { class: 'in-use' }, text.profile.inUse) : null,
      ),
    );
  }
  return list;
}

/** The end of a tab: the kind in use says so; the other offers the switch. */
function footer(ctx: Context, kind: ProfileKind): HTMLElement {
  const { profile, phase } = ctx.snapshot;
  const children: Child[] = [];
  if (profile.kind === kind) {
    children.push(
      h(
        'p',
        { class: 'in-use-line' },
        h('span', { class: 'lamp', 'aria-hidden': 'true' }),
        kind === 'headphones' ? text.profile.usingHeadphones : text.profile.usingSpeakers,
      ),
    );
  } else {
    // Headphones need a BRIR set; the supervisor refuses them without one.
    const needsBrir = kind === 'headphones' && profile.headphones.brir === null && !profile.from_file;
    const use = intentButton(
      ctx,
      kind === 'headphones' ? text.profile.useHeadphones : text.profile.useSpeakers,
      { kind: 'set_profile_kind', value: kind },
      { key: `use-${kind}`, variant: 'primary', pendingLabel: text.profile.switching },
    );
    if (profile.from_file || needsBrir) use.disabled = true;
    const ids = described(needsBrir && 'use-headphones-hint', ...changeHints(ctx));
    if (ids !== null) use.setAttribute('aria-describedby', ids);
    children.push(
      h(
        'div',
        { class: 'tab-footer-row' },
        use,
        needsBrir ? h('p', { id: 'use-headphones-hint', class: 'hint' }, text.profile.needsBrir) : null,
      ),
    );
  }
  if (phase.kind === 'running' && !profile.from_file) {
    children.push(h('p', { id: RUNNING_HINT, class: 'hint' }, text.profile.importRunning));
  }
  return h('div', { class: 'tab-footer' }, ...children);
}

/**
 * Whether this kind applies the upmixers (ADR 0035). Shown only while some
 * endpoint's upmixer resolves to something other than off; profile.toml
 * does not lock it.
 */
function upmixApply(ctx: Context, kind: ProfileKind): HTMLElement | null {
  const { upmix } = ctx.snapshot;
  if (upmix.endpoints.every((endpoint) => endpoint.effective === 'off')) return null;
  const sending = ctx.sending?.kind === 'set_upmix_apply' && ctx.sending.value.kind === kind ? ctx.sending.value : null;
  const hint = `upmix-apply-${kind}-hint`;
  return h(
    'div',
    { class: 'upmix-apply' },
    switchControl({
      key: `upmix-apply-${kind}`,
      label: text.profile.upmixApply,
      // While the change is in flight the switch already stands where it goes.
      on: sending?.on ?? upmix[kind],
      disabled: ctx.busy,
      busy: sending !== null,
      describedBy: hint,
      onToggle: (on) => ctx.send({ kind: 'set_upmix_apply', value: { kind, on } }),
    }),
    h('p', { id: hint, class: 'hint' }, text.profile.upmixApplyHint),
  );
}

function headphonesTab(ctx: Context): Child[] {
  const { profile } = ctx.snapshot;
  const shell = ctx.shell;
  const locked = profile.from_file;
  const importing = ctx.pending === 'import_brir';
  const ids = described(importing && 'brir-hint', ...changeHints(ctx));
  return [
    facts([[text.profile.brir, path(profile.headphones.brir)]], 'facts tab-facts'),
    h('p', { class: 'body-text tab-body' }, text.profile.brirBody),
    h(
      'div',
      { class: 'actions' },
      picker(
        ctx,
        text.profile.brirFolder,
        'brir-folder',
        locked,
        ids,
        () => shell.pickFolder(text.profile.brirFolderDialog),
        (value) => ctx.send({ kind: 'import_brir', value }),
      ),
      picker(
        ctx,
        text.profile.brirFile,
        'brir-file',
        locked,
        ids,
        () => shell.pickFile(text.profile.brirFileDialog, { name: text.profile.brirFilter, extensions: ['wav'] }),
        (value) => ctx.send({ kind: 'import_brir', value }),
      ),
    ),
    importing ? h('p', { id: 'brir-hint', class: 'hint import-hint' }, text.profile.importing) : null,
    upmixApply(ctx, 'headphones'),
    footer(ctx, 'headphones'),
  ];
}

/** The layout speakers use while no correction is imported. */
function layoutField(ctx: Context): HTMLElement {
  const { profile } = ctx.snapshot;
  const { layout, correction } = profile.speakers;
  const changing = ctx.pending === 'set_speaker_layout';
  // A correction decides the speakers; the stored layout waits behind it.
  const byCorrection = correction !== null && !profile.from_file;
  const hint = changing ? text.profile.applying : byCorrection ? text.profile.layoutByCorrection : null;
  const select = h('select', {
    id: 'speaker-layout',
    'data-key': 'speaker-layout',
    disabled: ctx.busy || profile.from_file || layout === null || correction !== null,
    'aria-busy': changing ? 'true' : null,
    'aria-describedby': described(hint !== null && 'layout-hint', ...changeHints(ctx)),
  });
  // A speaker profile.toml lays the speakers out its own way.
  if (layout === null) select.append(h('option', { value: '', selected: true }, text.profile.layoutFromFile));
  for (const value of LAYOUTS) select.append(h('option', { value, selected: value === layout }, value));
  select.addEventListener('change', () => {
    const value = LAYOUTS.find((candidate) => candidate === select.value);
    if (value !== undefined) ctx.send({ kind: 'set_speaker_layout', value });
  });
  return h(
    'div',
    { class: 'field layout-field' },
    h('label', { for: 'speaker-layout' }, text.profile.layout),
    select,
    hint === null ? null : h('p', { id: 'layout-hint', class: 'hint' }, hint),
  );
}

/** The SECS 2R correction: the file, its title, and the buttons that set and clear it. */
function correctionSection(ctx: Context): HTMLElement {
  const { profile } = ctx.snapshot;
  const correction = profile.speakers.correction;
  const locked = profile.from_file;
  const hint =
    ctx.pending === 'import_correction' ? text.profile.importing
    : ctx.pending === 'clear_correction' ? text.profile.applying
    : null;
  const ids = described(hint !== null && 'correction-hint', ...changeHints(ctx));
  const rows: [string, Child][] = [[text.profile.correctionFile, path(correction?.path ?? null)]];
  if (correction !== null) rows.push([text.profile.correctionName, correction.title]);
  let clear: HTMLButtonElement | null = null;
  if (correction !== null) {
    clear = intentButton(ctx, text.profile.correctionClear, { kind: 'clear_correction' }, { key: 'correction-clear' });
    if (locked) clear.disabled = true;
    if (ids !== null) clear.setAttribute('aria-describedby', ids);
  }
  return h(
    'section',
    { class: 'tab-section', 'aria-labelledby': 'correction-title' },
    h('h3', { id: 'correction-title', class: 'tab-section-title' }, text.profile.correctionTitle),
    h('p', { class: 'body-text' }, text.profile.correctionBody),
    facts(rows, 'facts tab-facts'),
    h(
      'div',
      { class: 'actions' },
      picker(
        ctx,
        text.profile.correctionImport,
        'correction-import',
        locked,
        ids,
        () =>
          ctx.shell.pickFile(text.profile.correctionDialog, {
            name: text.profile.correctionFilter,
            extensions: ['yml', 'yaml'],
          }),
        (value) => ctx.send({ kind: 'import_correction', value }),
      ),
      clear,
    ),
    hint === null ? null : h('p', { id: 'correction-hint', class: 'hint import-hint' }, hint),
  );
}

/** What each device channel plays: the correction's outputs, else the layout's speakers. */
function channelTable(ctx: Context): HTMLElement {
  const { channels } = ctx.snapshot.profile.speakers;
  const rows = channels.map((channel) =>
    h(
      'tr',
      null,
      h('td', { class: 'channel-number' }, channel.channel + 1),
      h(
        'th',
        { scope: 'row', class: 'channel-name' },
        h('span', null, channel.name),
        channel.subwoofer ? h('span', { class: 'tag' }, text.profile.subwoofer) : null,
      ),
    ),
  );
  return h(
    'section',
    { class: 'tab-section', 'aria-labelledby': 'channels-title' },
    h('h3', { id: 'channels-title', class: 'tab-section-title' }, text.profile.channelsTitle),
    h(
      'table',
      { class: 'channel-table', 'aria-labelledby': 'channels-title' },
      h(
        'thead',
        null,
        h(
          'tr',
          null,
          h('th', { scope: 'col', class: 'channel-number' }, text.profile.channel),
          h('th', { scope: 'col' }, text.profile.speaker),
        ),
      ),
      h('tbody', null, ...rows),
    ),
  );
}

function speakersTab(ctx: Context): Child[] {
  return [layoutField(ctx), correctionSection(ctx), channelTable(ctx), upmixApply(ctx, 'speakers'), footer(ctx, 'speakers')];
}

/** The output panel: the file note, the tabs, and the open tab's settings. */
function output(ctx: Context): HTMLElement {
  const open = ctx.view.profileTab ?? ctx.snapshot.profile.kind;
  return panel(
    text.profile.outputTitle,
    [
      fileNote(ctx),
      tabs(ctx, open),
      h(
        'div',
        {
          role: 'tabpanel',
          id: `profile-panel-${open}`,
          class: 'output-panel',
          'aria-labelledby': `profile-tab-${open}`,
          tabindex: 0,
        },
        ...(open === 'headphones' ? headphonesTab(ctx) : speakersTab(ctx)),
      ),
    ],
    'output',
  );
}

function gain(ctx: Context): HTMLElement {
  // The snapshot's gain, except while a set_gain is in flight: then the
  // value being sent, so the slider does not jump back until Rust answers.
  const value =
    ctx.pending === 'set_gain' && ctx.view.gainSending !== null ? ctx.view.gainSending : ctx.snapshot.profile.gain_db;
  const readout = h('output', { class: 'num gain-readout', for: 'gain' }, `${oneDecimal(value)} ${text.profile.gainUnit}`);
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
    readout.textContent = `${oneDecimal(moved)} ${text.profile.gainUnit}`;
    slider.setAttribute('aria-valuetext', `${oneDecimal(moved)} dB`);
  });
  // `change` fires when the slider is let go (or a key step ends).
  slider.addEventListener('change', () => {
    const wanted = Number(slider.value);
    ctx.setView({ gainSending: wanted });
    ctx.send({ kind: 'set_gain', value: wanted });
  });
  return panel(text.profile.gainTitle, [
    h('p', { class: 'body-text' }, text.profile.gainBody),
    h(
      'div',
      { class: 'gain' },
      h('label', { for: 'gain', class: 'visually-hidden' }, text.profile.gainLabel),
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
  const grid = h('div', { class: 'direction-grid', role: 'group', 'aria-label': text.profile.testLabel });
  for (const direction of profile.directions) {
    const tone = intentButton(
      ctx,
      direction.name,
      { kind: 'test_direction', value: direction.id },
      { key: `direction-${direction.id}` },
    );
    tone.title = text.profile.angles(direction.azimuth_deg, direction.elevation_deg);
    if (!running) {
      tone.disabled = true;
      tone.setAttribute('aria-describedby', 'test-hint');
    }
    grid.append(tone);
  }
  const empty = profile.directions.length === 0;
  return panel(text.profile.testTitle, [
    h('p', { class: 'body-text' }, text.profile.testBody),
    empty ? h('p', { class: 'hint import-hint' }, text.profile.noDirections) : grid,
    running || empty ? null : h('p', { id: 'test-hint', class: 'hint import-hint' }, text.profile.testOff),
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
      h('div', { class: 'profile-controls' }, output(ctx), gain(ctx), tones(ctx)),
      map.element,
    ),
  );
  return running ? { element, live: { update: map.update } } : { element };
}
