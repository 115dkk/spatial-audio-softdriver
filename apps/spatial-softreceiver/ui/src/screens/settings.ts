// The settings screen: login autostart, the language, the app's update,
// diagnostic recording, and what the supervisor knows about the PC and
// the driver.

import { toUserError } from '../api';
import type { Language } from '../bindings';
import { type Child, facts, h } from '../dom';
import { text } from '../text';
import { type Context, type Drawn, button, intentButton, panel } from './common';
import { driverRows, platformRows } from './facts';

function autostart(ctx: Context): HTMLElement {
  const t = text.settings;
  const native = ctx.shell.native;
  // A change in flight lives in view state, not in this checkbox: a redraw
  // (a snapshot, a tab switch) builds a new checkbox that must stay locked.
  const inFlight = ctx.view.autostartWanted !== null;
  const checkbox = h('input', {
    type: 'checkbox',
    id: 'autostart',
    'data-key': 'autostart',
    checked: (ctx.view.autostartWanted ?? ctx.view.autostart) === true,
    disabled: !native || ctx.view.autostart === null || inFlight,
    'aria-busy': inFlight ? 'true' : null,
    'aria-describedby': 'autostart-hint',
  });
  checkbox.addEventListener('change', () => {
    // `ctx.view` is the app's live view state, so a checkbox from an older
    // drawing also sees a change that is already in flight.
    if (ctx.view.autostartWanted !== null || ctx.view.autostart === null) {
      checkbox.checked = !checkbox.checked;
      return;
    }
    const wanted = checkbox.checked;
    const before = ctx.view.autostart;
    ctx.setView({ autostartWanted: wanted });
    ctx.shell
      .setAutostart(wanted)
      .then(() => ctx.shell.autostartEnabled())
      .then(
        (enabled) => ctx.setView({ autostart: enabled, autostartWanted: null }),
        (reason: unknown) => {
          ctx.report({ message: String(reason), next: 'none' });
          ctx.setView({ autostart: before, autostartWanted: null });
        },
      );
  });
  return h(
    'div',
    { class: 'check' },
    checkbox,
    h('label', { for: 'autostart' }, t.autostart),
    h('p', { id: 'autostart-hint', class: 'hint' }, native ? t.autostartHint : t.autostartUnavailable),
  );
}

/** The running version, and the downloaded one with the button that applies it. */
function update(ctx: Context): HTMLElement {
  const t = text.settings;
  const state = ctx.view.update;
  const children: Child[] = [];
  if (!ctx.shell.native) {
    children.push(h('p', { class: 'hint' }, t.updateUnavailable));
  } else if (state === null) {
    children.push(h('p', { class: 'hint' }, t.updateUnknown));
  } else {
    children.push(h('p', { class: 'body-text' }, t.updateCurrent(state.current)));
    if (state.ready === null) {
      children.push(h('p', { class: 'hint' }, t.updateNone));
    } else {
      children.push(h('p', { class: 'body-text' }, t.updateReady(state.ready)));
      children.push(
        h(
          'div',
          { class: 'actions' },
          button(
            t.updateApply,
            () => {
              ctx.shell.applyUpdate().catch((reason: unknown) => ctx.report(toUserError(reason)));
            },
            { key: 'update-apply', variant: 'primary', disabled: ctx.busy },
          ),
        ),
      );
    }
  }
  return h('div', { class: 'update' }, ...children);
}

/** The language: automatic (the Windows display language) or one of the two. */
function language(ctx: Context): HTMLElement {
  const t = text.settings;
  const current = ctx.snapshot.language_choice ?? 'auto';
  const options: Array<[value: 'auto' | Language, label: string]> = [
    ['auto', t.languageAuto],
    ['ko', t.languageKo],
    ['en', t.languageEn],
  ];
  const select = h('select', {
    id: 'language',
    'data-key': 'language',
    disabled: ctx.busy,
    'aria-describedby': 'language-hint',
  });
  for (const [value, label] of options) {
    select.append(h('option', { value, selected: value === current }, label));
  }
  select.addEventListener('change', () => {
    const value = select.value;
    ctx.send({ kind: 'set_language', value: value === 'auto' ? null : (value as Language) });
  });
  return h(
    'div',
    { class: 'field' },
    h('label', { for: 'language' }, t.language),
    select,
    h('p', { id: 'language-hint', class: 'hint' }, t.languageHint),
  );
}

export function settings(ctx: Context): Drawn {
  const t = text.settings;
  const { platform, driver } = ctx.snapshot;
  const element = h(
    'div',
    { class: 'settings' },
    panel(t.startup, [autostart(ctx)]),
    panel(t.language, [language(ctx)]),
    panel(t.update, [update(ctx)]),
    panel(t.diagnostics, [
      h('p', { class: 'body-text' }, t.recordHint),
      h(
        'div',
        { class: 'actions' },
        intentButton(ctx, t.record, { kind: 'start_diagnostic_recording' }, { key: 'record' }),
      ),
    ]),
    h(
      'div',
      { class: 'settings-facts' },
      panel(t.platform, [platform === null ? h('p', { class: 'muted' }, t.unknown) : facts(platformRows(platform))]),
      panel(t.driver, [driver === null ? h('p', { class: 'muted' }, t.unknown) : facts(driverRows(driver))]),
    ),
  );
  return { element };
}
