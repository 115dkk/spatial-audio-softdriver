// The settings screen: login autostart, diagnostic recording, and what the
// supervisor knows about the PC and the driver.

import { facts, h } from '../dom';
import { text } from '../text';
import { type Context, type Drawn, intentButton, panel } from './common';
import { driverRows, platformRows } from './facts';

const t = text.settings;

function autostart(ctx: Context): HTMLElement {
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

export function settings(ctx: Context): Drawn {
  const { platform, driver } = ctx.snapshot;
  const element = h(
    'div',
    { class: 'settings' },
    panel(t.startup, [autostart(ctx)]),
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
