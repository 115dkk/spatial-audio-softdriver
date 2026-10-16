// The upmix screen (ADR 0035, the 2026-10-16 correction): one short
// paragraph on what upmixing does, with an (i) whose line says what is
// upmixed, then one card per endpoint (main, music and PASS), each with its
// upmixer choice, the delay upmixing adds, its VST plugin and what the
// upmixer is doing now. Every value comes from `Snapshot.upmix`; Rust decides
// whether a choice, a plugin or an editor is taken, and why the sound playing
// now goes around a running upmixer. Whether the output kind in use applies
// upmixing is the profile screen's switch.

import type { EndpointUpmixView, PluginFormat, UpmixBypass, UpmixerChoice } from '../bindings';
import { type Child, facts, h } from '../dom';
import { oneDecimal } from '../format';
import { text } from '../text';
import { type ButtonOptions, type Context, type Drawn, described, infoTip, intentButton, picker } from './common';

/** The choices in the order they stand. */
const CHOICES: readonly UpmixerChoice[] = ['first_party', 'plugin', 'off'];

/** The endpoint an upmix intent in flight is for, or null. */
function sendingFor(ctx: Context): number | null {
  const intent = ctx.sending;
  if (intent === null) return null;
  switch (intent.kind) {
    case 'set_upmixer':
    case 'choose_plugin':
      return intent.value.endpoint;
    case 'open_plugin_editor':
      return intent.value;
    default:
      return null;
  }
}

function choiceName(choice: UpmixerChoice): string {
  switch (choice) {
    case 'first_party':
      return text.upmix.firstParty;
    case 'plugin':
      return text.upmix.plugin;
    case 'off':
      return text.upmix.off;
  }
}

function choiceBody(choice: UpmixerChoice, firstPartyBuilt: boolean): string {
  switch (choice) {
    case 'first_party':
      return firstPartyBuilt ? text.upmix.firstPartyBody : `${text.upmix.firstPartyBody} ${text.upmix.firstPartyHint}`;
    case 'plugin':
      return text.upmix.pluginBody;
    case 'off':
      return text.upmix.offBody;
  }
}

/**
 * Why the sound playing now goes around a running upmixer, as a quiet
 * phrase; the status screen says it too.
 */
export function bypassReason(bypass: UpmixBypass): string {
  switch (bypass) {
    case 'native_surround':
      return text.upmix.bypassNativeSurround;
    case 'kind_off':
      return text.upmix.bypassKindOff;
    case 'rate_change':
      return text.upmix.bypassRateChange;
  }
}

function formatName(format: PluginFormat): string {
  switch (format) {
    case 'vst3':
      return text.upmix.formatNames.vst3;
    case 'vst2':
      return text.upmix.formatNames.vst2;
  }
}

/** The editor opens only while the plugin's host process is up. */
const editorOpen = (view: EndpointUpmixView) => view.state.kind === 'running' || view.state.kind === 'fallback';

type Tone = 'off' | 'busy' | 'on' | 'warning' | 'error';

function stateLine(tone: Tone, children: Child[]): HTMLElement {
  const alert = tone === 'warning' || tone === 'error';
  return h(
    'p',
    { class: `upmix-state tone-${tone}${alert ? ' is-alert' : ''}` },
    tone === 'busy'
      ? h('span', { class: 'spinner', 'aria-hidden': 'true' })
      : h('span', { class: 'lamp', 'aria-hidden': 'true' }),
    // Its own box, so a line it wraps onto starts under the word, not the lamp.
    h('span', { class: 'upmix-state-text' }, ...children),
  );
}

/** What the endpoint's upmixer is doing, in one line. */
function state(ctx: Context, view: EndpointUpmixView): HTMLElement {
  const { snapshot } = ctx;
  const word = (value: string) => h('span', { class: 'upmix-state-word' }, value);
  const detail = (value: string, className = '') =>
    h('span', { class: `upmix-state-detail ${className}`.trim() }, value);
  switch (view.state.kind) {
    case 'stopped': {
      // Off, or a VST upmixer still without its plugin: nothing would start.
      const idle = view.effective === 'off' || (view.effective === 'plugin' && view.plugin === null);
      if (idle || snapshot.phase.kind === 'running') return stateLine('off', [word(text.upmix.stateOff)]);
      // It starts with the engine whatever the switch says; with the switch
      // off it will run as the bypass below says, so say that already.
      const notApplied = !snapshot.upmix[snapshot.profile.kind];
      return stateLine('off', [
        word(text.upmix.stateAfterEnable),
        notApplied ? detail(text.upmix.bypassKindOff, 'upmix-state-reason') : null,
      ]);
    }
    case 'starting':
      return stateLine('busy', [word(text.upmix.stateStarting)]);
    case 'running': {
      const details: Child[] = [];
      if (view.bypass !== null) {
        // The upmixer is up, but this sound plays as it came: no delay to name.
        details.push(detail(bypassReason(view.bypass), 'upmix-state-reason'));
      } else if (view.latency_ms !== null) {
        details.push(detail(text.upmix.latency(oneDecimal(view.latency_ms)), 'num'));
      }
      if (view.outputs.length > 0) details.push(detail(text.upmix.outputs(view.outputs.join(' '))));
      return stateLine('on', [word(text.upmix.stateRunning), ...details]);
    }
    case 'fallback':
      return stateLine('warning', [word(text.upmix.stateFallback)]);
    case 'failed':
      // The supervisor's sentence, as it wrote it.
      return stateLine('error', [word(view.state.value)]);
  }
}

/**
 * The three upmixers as radios; the checked one is what the choice resolves
 * to. Under them, while an upmixer is in effect and its delay is known, the
 * one line that says what upmixing costs this input.
 */
function choices(ctx: Context, view: EndpointUpmixView, titleId: string): HTMLElement {
  const endpoint = view.endpoint;
  const built = ctx.snapshot.upmix.first_party_available;
  const sending = ctx.sending?.kind === 'set_upmixer' && ctx.sending.value.endpoint === endpoint ? ctx.sending.value : null;
  // While the choice is in flight the radios already show where it goes.
  const shown = sending?.choice ?? view.effective;
  const labelId = `upmixer-${endpoint}-label`;
  const applyingId = `upmixer-${endpoint}-applying`;
  const delayId = `upmixer-${endpoint}-delay`;
  const delay =
    view.effective !== 'off' && view.latency_ms !== null ? text.upmix.delayHint(oneDecimal(view.latency_ms)) : null;
  const group = h('div', {
    class: 'upmix-choices',
    role: 'radiogroup',
    'aria-labelledby': `${titleId} ${labelId}`,
    'aria-describedby': delay === null ? null : delayId,
    'aria-busy': sending !== null ? 'true' : null,
  });
  for (const choice of CHOICES) {
    const key = `upmixer-${endpoint}-${choice}`;
    const nameId = `${key}-name`;
    const bodyId = `${key}-hint`;
    const unbuilt = choice === 'first_party' && !built;
    const checked = choice === shown;
    const input = h('input', {
      type: 'radio',
      id: key,
      name: `upmixer-${endpoint}`,
      value: choice,
      'data-key': key,
      'data-intent': 'set_upmixer',
      checked,
      disabled: ctx.busy || unbuilt,
      // The whole row is the label to click; the name is the head alone and
      // the line under it describes it.
      'aria-labelledby': nameId,
      'aria-describedby': described(bodyId, sending !== null && applyingId),
    });
    input.addEventListener('change', () => {
      if (input.checked) ctx.send({ kind: 'set_upmixer', value: { endpoint, choice } });
    });
    group.append(
      h(
        'label',
        {
          class: ['upmix-option', checked ? 'is-checked' : null, unbuilt ? 'is-locked' : null].filter(Boolean).join(' '),
        },
        input,
        h(
          'span',
          { class: 'upmix-option-text' },
          h(
            'span',
            { id: nameId, class: 'upmix-option-head' },
            h('span', { class: 'upmix-option-name' }, choiceName(choice)),
            unbuilt ? h('span', { class: 'tag tag-quiet' }, text.upmix.notBuilt) : null,
            // Nothing chosen yet: the option the default resolves to says so.
            view.choice === null && choice === view.effective
              ? h('span', { class: 'upmix-default' }, text.upmix.defaultNote)
              : null,
          ),
          h('span', { id: bodyId, class: 'upmix-option-body' }, choiceBody(choice, built)),
        ),
      ),
    );
  }
  return h(
    'div',
    { class: 'upmix-section' },
    h('h3', { id: labelId, class: 'upmix-section-title' }, text.upmix.choiceLabel),
    group,
    delay === null ? null : h('p', { id: delayId, class: 'hint upmix-delay num' }, delay),
    sending === null ? null : h('p', { id: applyingId, class: 'hint import-hint' }, text.profile.applying),
  );
}

/** The VST upmixer's plugin: what it is, where it is, and the buttons that choose and open it. */
function pluginBlock(ctx: Context, view: EndpointUpmixView): HTMLElement {
  const endpoint = view.endpoint;
  const plugin = view.plugin;
  const mine = sendingFor(ctx) === endpoint;
  const scanning = mine && ctx.pending === 'choose_plugin';
  const scanId = `plugin-${endpoint}-scanning`;
  const neededId = `plugin-${endpoint}-needed`;
  const editorId = `plugin-${endpoint}-editor-hint`;
  const pickIds = described(plugin === null && neededId, scanning && scanId);
  const send = (path: string) => ctx.send({ kind: 'choose_plugin', value: { endpoint, path } });

  let editor: HTMLButtonElement | null = null;
  if (plugin !== null) {
    const options: ButtonOptions = { key: `plugin-editor-${endpoint}` };
    if (mine) options.pendingLabel = text.upmix.opening;
    editor = intentButton(ctx, text.upmix.openEditor, { kind: 'open_plugin_editor', value: endpoint }, options);
    // Another endpoint's editor in flight is not this one's.
    if (!mine) editor.removeAttribute('aria-busy');
    if (!editorOpen(view)) {
      editor.disabled = true;
      editor.setAttribute('aria-describedby', editorId);
    }
  }

  const about: Child[] =
    plugin === null
      ? [
          h('p', { class: 'upmix-no-plugin' }, text.upmix.noPlugin),
          h('p', { id: neededId, class: 'hint' }, text.upmix.needsPlugin),
        ]
      : [
          facts(
            [
              [text.upmix.pluginName, plugin.name],
              [text.upmix.pluginVendor, plugin.vendor],
              [text.upmix.pluginFormat, formatName(plugin.format)],
              // The speakers the stereo is placed on before the plugin.
              ...(view.inputs.length > 0 ? [[text.upmix.pluginInputs, view.inputs.join(' ')] as [string, Child]] : []),
              [text.upmix.pluginPath, h('span', { class: 'path' }, plugin.path)],
            ],
            'facts upmix-facts',
          ),
        ];

  return h(
    'section',
    { class: 'upmix-section upmix-plugin', 'aria-labelledby': `plugin-${endpoint}-title` },
    h('h3', { id: `plugin-${endpoint}-title`, class: 'upmix-section-title' }, text.upmix.pluginTitle),
    ...about,
    h(
      'div',
      { class: 'actions' },
      picker(
        ctx,
        text.upmix.pickFolder,
        `plugin-folder-${endpoint}`,
        false,
        pickIds,
        () => ctx.shell.pickFolder(text.upmix.folderDialog),
        send,
      ),
      picker(
        ctx,
        text.upmix.pickFile,
        `plugin-file-${endpoint}`,
        false,
        pickIds,
        () => ctx.shell.pickFile(text.upmix.fileDialog, { name: text.upmix.fileFilter, extensions: ['vst3', 'dll'] }),
        send,
      ),
      editor,
    ),
    scanning ? h('p', { id: scanId, class: 'hint import-hint' }, text.upmix.scanning) : null,
    editor !== null && !editorOpen(view) ? h('p', { id: editorId, class: 'hint import-hint' }, text.upmix.editorHint) : null,
  );
}

function card(ctx: Context, view: EndpointUpmixView): HTMLElement {
  const names = text.endpoints[view.endpoint] ?? { name: String(view.endpoint), purpose: '' };
  const titleId = `upmix-title-${view.endpoint}`;
  return h(
    'section',
    { class: 'panel upmix-card', 'aria-labelledby': titleId, 'data-endpoint': view.endpoint },
    h(
      'header',
      { class: 'upmix-card-head' },
      h('h2', { id: titleId, class: 'upmix-card-title' }, names.name),
      h('p', { class: 'muted small' }, names.purpose),
    ),
    state(ctx, view),
    choices(ctx, view, titleId),
    view.effective === 'plugin' ? pluginBlock(ctx, view) : null,
  );
}

export function upmix(ctx: Context): Drawn {
  const element = h(
    'div',
    { class: 'upmix' },
    // The rule of what is upmixed is said once, in the (i) at the intro's
    // end; the no-break space keeps the (i) with the last word.
    h(
      'p',
      { class: 'body-text upmix-intro' },
      text.upmix.intro,
      '\u00a0',
      infoTip('upmix-info', text.upmix.infoLabel, text.upmix.infoTip),
    ),
    h('div', { class: 'upmix-cards' }, ...ctx.snapshot.upmix.endpoints.map((view) => card(ctx, view))),
  );
  return { element };
}
