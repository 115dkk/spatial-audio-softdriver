// The status screen (ready, running, faulted): the power switch, the chosen
// output, one card per entrance and, while running, the meter column. The
// cards and meters are built once per snapshot and then only their text nodes
// change, 30 times a second.

import type { EndpointStatus, EngineStatus } from '../bindings';
import { h, liveText } from '../dom';
import {
  chainErrorEntries,
  chainErrorTotal,
  count,
  describeCarrier,
  describeClock,
  describeFormat,
  describeOutputFormat,
  describeOutputMode,
  describeUnsupported,
  describeVolume,
  oneDecimal,
} from '../format';
import { text } from '../text';
import { type Context, type Drawn, type Live, button, intentButton } from './common';
import { powerState } from './nav';
import { outputList } from './wizard';

/** Endpoints the screen shows: 0, 1 and 2. Slot 3 is reserved in the contract. */
const SHOWN_ENDPOINTS = 3;

function header(ctx: Context): HTMLElement {
  const s = text.status;
  const { snapshot } = ctx;
  const running = snapshot.phase.kind === 'running';
  const toggle = intentButton(
    ctx,
    running ? s.powerOff : s.powerOn,
    running ? { kind: 'disable' } : { kind: 'enable' },
    {
      key: 'power',
      variant: running ? 'secondary' : 'primary',
      className: 'power',
      pendingLabel: running ? s.powerTurningOff : s.powerTurningOn,
    },
  );
  toggle.setAttribute('aria-describedby', 'power-state');

  const power = powerState(snapshot);
  const state = h(
    'div',
    { class: 'power-state' },
    h('span', { class: 'label' }, s.stateLabel),
    h(
      'strong',
      { id: 'power-state', class: `state-word tone-${power.tone}` },
      h('span', { class: 'lamp', 'aria-hidden': 'true' }),
      power.label,
    ),
    running && snapshot.roles_taken ? h('span', { class: 'muted small' }, s.rolesTaken) : null,
  );

  const canChange = snapshot.phase.kind === 'ready';
  const output = h(
    'div',
    { class: 'output-state' },
    h('span', { class: 'label' }, s.output),
    h(
      'div',
      { class: 'output-line' },
      h(
        'span',
        { class: `output-current${snapshot.chosen_output === null ? ' muted' : ''}` },
        snapshot.chosen_output?.name ?? s.noOutput,
      ),
      canChange
        ? button(
            ctx.view.outputsOpen ? s.closeOutputs : snapshot.chosen_output === null ? s.choose : s.change,
            () => ctx.setView({ outputsOpen: !ctx.view.outputsOpen }),
            { key: 'outputs-toggle', variant: 'quiet' },
          )
        : null,
    ),
  );

  const head = h('section', { class: 'panel status-head' }, h('div', { class: 'status-head-row' }, toggle, state, output));
  if (canChange && ctx.view.outputsOpen) head.append(h('div', { class: 'status-outputs' }, outputList(ctx, 'status-output')));
  return head;
}

interface Card {
  element: HTMLElement;
  update(endpoint: EndpointStatus | null): void;
}

function endpointCard(index: number): Card {
  const s = text.status;
  const names = text.endpoints[index] ?? { name: `${index}`, purpose: '' };
  const format = liveText();
  const carrier = liveText();
  const volume = liveText();
  const epoch = liveText();
  const unsupported = liveText();
  const errorTotal = liveText();

  const carrierLine = h('p', { class: 'card-carrier muted small', hidden: true }, carrier.node);
  const unsupportedLine = h('p', { class: 'card-warning', hidden: true }, unsupported.node);
  const errorList = h('ul', { class: 'error-kinds' });
  const errors = h(
    'details',
    { class: 'card-errors' },
    h('summary', null, h('span', { class: 'label' }, s.chainErrors), h('span', { class: 'num' }, errorTotal.node)),
    errorList,
  );
  const rows = h(
    'dl',
    { class: 'card-rows' },
    h('div', { class: 'fact' }, h('dt', null, s.volume), h('dd', { class: 'num' }, volume.node)),
    h('div', { class: 'fact', title: s.epochHint }, h('dt', null, s.epoch), h('dd', { class: 'num' }, epoch.node)),
  );
  const element = h(
    'article',
    { class: 'panel endpoint-card' },
    h('header', { class: 'card-head' }, h('h3', { class: 'card-title' }, names.name), h('p', { class: 'muted small' }, names.purpose)),
    h('div', null, h('p', { class: 'card-format' }, format.node), carrierLine),
    unsupportedLine,
    rows,
    errors,
  );

  let errorKey = '';
  return {
    element,
    update(endpoint) {
      element.classList.toggle('is-off', endpoint === null);
      rows.hidden = endpoint === null;
      errors.hidden = endpoint === null;
      if (endpoint === null) {
        format.set(s.engineOff);
        carrierLine.hidden = true;
        unsupportedLine.hidden = true;
        return;
      }
      format.set(describeFormat(endpoint.format));
      const container = describeCarrier(endpoint.format);
      carrierLine.hidden = container === null;
      if (container !== null) carrier.set(container);
      element.classList.toggle('is-idle', endpoint.format === null);
      volume.set(describeVolume(endpoint.volume_db, endpoint.muted));
      epoch.set(count(endpoint.epoch));
      unsupportedLine.hidden = endpoint.unsupported === null;
      if (endpoint.unsupported !== null) unsupported.set(describeUnsupported(endpoint.unsupported));

      const entries = chainErrorEntries(endpoint.chain_errors);
      const total = chainErrorTotal(endpoint.chain_errors);
      errorTotal.set(total === 0 ? s.chainErrorsNone : count(total));
      errors.classList.toggle('has-errors', total > 0);
      // The kinds change rarely; rebuild the small list only when they do.
      const key = entries.map((entry) => `${entry.kind}:${entry.count}`).join(',');
      if (key !== errorKey) {
        errorKey = key;
        errorList.replaceChildren(
          ...entries.map((entry) =>
            h('li', null, h('span', null, entry.label), h('span', { class: 'num' }, count(entry.count))),
          ),
        );
      }
      if (total === 0) errors.open = false;
    },
  };
}

type MeterRow = [label: string, unit: string, read: (engine: EngineStatus) => string];

function meterRows(): MeterRow[] {
  const m = text.meters;
  return [
    [m.latencyP50, m.ms, (e) => (e.latency.samples === 0 ? '-' : oneDecimal(e.latency.p50_ms))],
    [m.latencyP99, m.ms, (e) => (e.latency.samples === 0 ? '-' : oneDecimal(e.latency.p99_ms))],
    [m.ringLevel, m.ms, (e) => oneDecimal(e.ring_level_ms)],
    [m.underruns, m.times, (e) => count(e.underruns)],
    [m.outputRestarts, m.times, (e) => count(e.output_restarts)],
    [m.outputMode, '', describeOutputMode],
    [m.output, '', describeOutputFormat],
    [m.clock, '', describeClock],
    [m.decodeRefused, m.times, (e) => count(e.decode_refused)],
    [m.lostBytes, m.bytes, (e) => count(e.lost_bytes)],
  ];
}

function meterColumn(): { element: HTMLElement; update(engine: EngineStatus): void } {
  const s = text.status;
  const list = h('dl', { class: 'meters' });
  const setters = meterRows().map(([label, unit, read]) => {
    const value = liveText();
    list.append(
      h(
        'div',
        { class: 'meter' },
        h('dt', null, label),
        h('dd', null, h('span', { class: 'num' }, value.node), unit ? h('span', { class: 'unit' }, unit) : null),
      ),
    );
    return (engine: EngineStatus) => value.set(read(engine));
  });
  const element = h('section', { class: 'panel meter-panel' }, h('h2', { class: 'panel-title' }, s.meters), list);
  return {
    element,
    update(engine) {
      for (const set of setters) set(engine);
    },
  };
}

export function status(ctx: Context, engineNow: EngineStatus | null): Drawn {
  const s = text.status;
  const running = ctx.snapshot.phase.kind === 'running';
  const cards = Array.from({ length: SHOWN_ENDPOINTS }, (_, index) => endpointCard(index));
  const grid = h(
    'section',
    { class: 'entrances', 'aria-label': s.entrances },
    ...cards.map((card) => card.element),
  );
  const meters = running ? meterColumn() : null;
  const live: Live = {
    update(engine) {
      cards.forEach((card, index) => card.update(engine.endpoints[index] ?? null));
      meters?.update(engine);
    },
  };
  if (engineNow !== null) live.update(engineNow);
  else for (const card of cards) card.update(null);

  const element = h(
    'div',
    { class: 'status' },
    header(ctx),
    h('div', { class: `status-body${meters ? ' with-meters' : ''}` }, grid, meters?.element ?? null),
  );
  return engineNow === null ? { element } : { element, live };
}
