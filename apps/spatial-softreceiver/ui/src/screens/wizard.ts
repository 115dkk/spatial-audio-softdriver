// The first-run wizard (phase `setup`): a step strip and one card per step.
// The supervisor decides the step; the card only explains it and offers the
// intent that moves it on.

import type { SetupStep, Snapshot } from '../bindings';
import { facts, h } from '../dom';
import { text } from '../text';
import { brand } from './nav';
import { type Context, intentButton } from './common';
import { driverRows, platformRows } from './facts';

const w = text.wizard;

/** Which of the four steps (환영, 확인, 드라이버, 출력) a setup step belongs to. */
export function stepIndex(step: SetupStep): number {
  switch (step) {
    case 'welcome':
      return 0;
    case 'unsupported':
    case 'not_test_signing':
      return 1;
    case 'driver':
    case 'reboot_required':
      return 2;
    case 'choose_output':
      return 3;
  }
}

function steps(current: number): HTMLElement {
  const list = h('ol', { class: 'steps', 'aria-label': w.stepsLabel });
  w.steps.forEach((label, index) => {
    const state = index < current ? 'done' : index === current ? 'current' : 'upcoming';
    list.append(
      h(
        'li',
        { class: `step ${state}`, 'aria-current': state === 'current' ? 'step' : null },
        h('span', { class: 'step-number', 'aria-hidden': 'true' }, String(index + 1)),
        h('span', { class: 'step-label' }, label),
        state === 'done' ? h('span', { class: 'visually-hidden' }, ` (${w.stepDone})`) : null,
      ),
    );
  });
  return list;
}

function card(title: string, ...children: (Node | null)[]): HTMLElement {
  return h('section', { class: 'panel wizard-card' }, h('h1', { class: 'wizard-title' }, title), ...children);
}

const para = (sentence: string) => h('p', { class: 'body-text' }, sentence);

function welcome(ctx: Context): HTMLElement {
  return card(
    w.welcomeTitle,
    ...w.welcomeBody.map(para),
    h(
      'div',
      { class: 'actions' },
      intentButton(ctx, w.begin, { kind: 'begin_setup' }, { key: 'begin', variant: 'primary' }),
    ),
  );
}

function platformFacts(snapshot: Snapshot): HTMLElement | null {
  return snapshot.platform === null ? null : facts(platformRows(snapshot.platform));
}

function unsupported(ctx: Context): HTMLElement {
  return card(w.unsupportedTitle, para(w.unsupportedBody), platformFacts(ctx.snapshot));
}

function notTestSigning(ctx: Context): HTMLElement {
  return card(
    w.testSigningTitle,
    para(w.testSigningBody),
    platformFacts(ctx.snapshot),
    h(
      'div',
      { class: 'actions' },
      intentButton(ctx, text.banner.recheck, { kind: 'recheck' }, { key: 'recheck', variant: 'primary' }),
    ),
  );
}

function endpointList(): HTMLElement {
  return h(
    'ul',
    { class: 'endpoint-names' },
    ...text.endpoints.map((endpoint) =>
      h('li', null, h('span', { class: 'endpoint-name' }, endpoint.name), h('span', { class: 'muted' }, endpoint.purpose)),
    ),
  );
}

function driver(ctx: Context): HTMLElement {
  const again = ctx.snapshot.error !== null || ctx.snapshot.driver?.kind === 'installed';
  return card(
    w.driverTitle,
    para(w.driverBody),
    endpointList(),
    ctx.snapshot.driver === null ? null : facts(driverRows(ctx.snapshot.driver)),
    h(
      'div',
      { class: 'actions' },
      intentButton(
        ctx,
        again ? w.driverReinstall : w.driverInstall,
        { kind: 'install_driver' },
        { key: 'install', variant: 'primary', pendingLabel: w.driverInstalling },
      ),
      h('p', { class: 'hint' }, w.driverAdminHint),
    ),
  );
}

function rebootRequired(ctx: Context): HTMLElement {
  return card(
    w.rebootTitle,
    para(w.rebootBody),
    h(
      'div',
      { class: 'actions' },
      intentButton(ctx, text.banner.recheck, { kind: 'recheck' }, { key: 'recheck', variant: 'primary' }),
    ),
  );
}

/** The outputs to choose from; shared with the status screen's list. */
export function outputList(ctx: Context, keyPrefix: string): HTMLElement {
  const { outputs, chosen_output } = ctx.snapshot;
  if (outputs.length === 0) {
    return h(
      'div',
      { class: 'empty' },
      para(w.noOutputs),
      h('div', { class: 'actions' }, intentButton(ctx, text.banner.recheck, { kind: 'recheck' }, { key: `${keyPrefix}-recheck` })),
    );
  }
  const list = h('ul', { class: 'output-list', 'aria-label': w.outputsLabel });
  outputs.forEach((output, index) => {
    const current = chosen_output?.id === output.id;
    list.append(
      h(
        'li',
        { class: `output-row${current ? ' current' : ''}` },
        h('span', { class: 'output-name' }, output.name),
        current
          ? h('span', { class: 'tag' }, text.status.currentOutput)
          : intentButton(
              ctx,
              w.useOutput,
              { kind: 'choose_output', value: output.id },
              { key: `${keyPrefix}-${index}`, variant: 'secondary' },
            ),
      ),
    );
  });
  return list;
}

function chooseOutput(ctx: Context): HTMLElement {
  return card(w.outputTitle, para(w.outputBody), outputList(ctx, 'output'));
}

function body(ctx: Context, step: SetupStep): HTMLElement {
  switch (step) {
    case 'welcome':
      return welcome(ctx);
    case 'unsupported':
      return unsupported(ctx);
    case 'not_test_signing':
      return notTestSigning(ctx);
    case 'driver':
      return driver(ctx);
    case 'reboot_required':
      return rebootRequired(ctx);
    case 'choose_output':
      return chooseOutput(ctx);
  }
}

export interface WizardParts {
  head: HTMLElement;
  body: HTMLElement;
}

/** The wizard's head (brand and steps) and the current step's card. */
export function wizard(ctx: Context, step: SetupStep): WizardParts {
  const head = h(
    'header',
    { class: 'wizard-head' },
    h('div', { class: 'wizard-brand' }, brand(), h('span', { class: 'muted' }, w.heading)),
    steps(stepIndex(step)),
  );
  return { head, body: body(ctx, step) };
}

/** The screen after `quit`. */
export function quit(): WizardParts {
  return {
    head: h('header', { class: 'wizard-head' }, h('div', { class: 'wizard-brand' }, brand())),
    body: card(w.quitTitle, para(w.quitBody)),
  };
}
