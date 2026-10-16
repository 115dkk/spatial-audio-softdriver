import { describe, expect, it } from 'vitest';
import intents from '../fixtures/intents.json';
import { fixture, fixtureNames, rawFixture } from '../src/fixtures';
import { ShapeError, assertIntent, assertSnapshot } from '../src/guard';

describe('fixtures match bindings.ts', () => {
  it('lists every fixture file', () => {
    expect(fixtureNames.length).toBeGreaterThan(0);
    for (const name of fixtureNames) expect(() => rawFixture(name)).not.toThrow();
  });

  it.each(fixtureNames)('%s passes assertSnapshot', (name) => {
    const raw = rawFixture(name);
    expect(assertSnapshot(raw)).toEqual(raw);
  });

  it('has the Korean fixtures in Korean', () => {
    for (const name of fixtureNames) expect(fixture(name).language, name).toBe('ko');
  });

  it('covers every intent', () => {
    const kinds = intents.map((intent) => assertIntent(intent).kind);
    expect(new Set(kinds)).toEqual(
      new Set([
        'set_language',
        'begin_setup',
        'install_driver',
        'recheck',
        'choose_output',
        'set_profile_kind',
        'import_brir',
        'set_speaker_layout',
        'import_correction',
        'clear_correction',
        'set_gain',
        'test_direction',
        'set_upmixer',
        'choose_plugin',
        'open_plugin_editor',
        'set_upmix_apply',
        'enable',
        'disable',
        'start_diagnostic_recording',
        'quit',
      ]),
    );
  });
});

describe('the English fixtures', () => {
  const englishFiles = Object.keys(import.meta.glob('../fixtures/en/*.json'))
    .map((path) => path.replace(/^.*\//, '').replace(/\.json$/, ''))
    .sort();

  it('are the same states as the Korean ones', () => {
    expect(englishFiles).toEqual([...fixtureNames].sort());
  });

  it.each(fixtureNames)('%s passes assertSnapshot', (name) => {
    const raw = rawFixture(name, 'en');
    expect(assertSnapshot(raw)).toEqual(raw);
  });

  it.each(fixtureNames)('%s is in English and in the same state as the Korean one', (name) => {
    const english = fixture(name, 'en');
    const korean = fixture(name);
    expect(english.language).toBe('en');
    expect(english.language_choice).toEqual(korean.language_choice);
    expect(english.phase).toEqual(korean.phase);
    expect(english.error?.next ?? null).toEqual(korean.error?.next ?? null);
    expect(english.notes).toHaveLength(korean.notes.length);
    expect(english.engine === null).toBe(korean.engine === null);
    expect(english.profile.kind).toBe(korean.profile.kind);
  });

  it('a fresh copy every call, apart from the Korean one', () => {
    const first = fixture('running', 'en');
    first.notes.push('changed');
    expect(fixture('running', 'en').notes).not.toContain('changed');
    expect(fixture('running', 'en')).not.toEqual(fixture('running'));
  });

  it('names a missing fixture with its language', () => {
    expect(() => rawFixture('no-such-state', 'en')).toThrow('no en fixture named no-such-state');
  });
});

describe('the guards reject what bindings.ts does not allow', () => {
  const running = () => structuredClone(rawFixture('running')) as Record<string, unknown> & {
    phase: { kind: string };
    engine: Record<string, unknown>;
  };

  it('an unknown phase kind', () => {
    const value = running();
    value.phase.kind = 'paused';
    expect(() => assertSnapshot(value)).toThrow(ShapeError);
  });

  it('a missing field, with its path', () => {
    const value = running();
    delete value.engine.decode_refused;
    expect(() => assertSnapshot(value)).toThrow('snapshot.engine.decode_refused');
  });

  it('a field bindings.ts does not have', () => {
    const value = running();
    value.volume = 3;
    expect(() => assertSnapshot(value)).toThrow('snapshot.volume');
  });

  it('a wrong unit enum name', () => {
    const value = running();
    value.engine.clock = 'pll';
    expect(() => assertSnapshot(value)).toThrow('snapshot.engine.clock');
  });

  it('a codec by name or by number', () => {
    const value = running();
    const endpoints = value.engine.endpoints as { format: { value: { codec: unknown } } }[];
    const slot = endpoints[1];
    if (slot === undefined) throw new Error('fixture has no endpoint 1');
    slot.format.value.codec = { other: 0x1234 };
    expect(() => assertSnapshot(value)).not.toThrow();
    slot.format.value.codec = 'atmos';
    expect(() => assertSnapshot(value)).toThrow('codec');
  });

  it('an intent with the wrong value', () => {
    expect(() => assertIntent({ kind: 'set_gain', value: '-3' })).toThrow('intent.value');
    expect(() => assertIntent({ kind: 'enable', value: 1 })).toThrow('intent.value');
    expect(() => assertIntent({ kind: 'set_upmixer', value: { endpoint: 1, choice: 'vst' } })).toThrow('intent.value.choice');
    expect(() => assertIntent({ kind: 'choose_plugin', value: { endpoint: 1 } })).toThrow('intent.value.path');
    expect(() => assertIntent({ kind: 'open_plugin_editor', value: '1' })).toThrow('intent.value');
    expect(() => assertIntent({ kind: 'set_upmix_apply', value: { kind: 'stereo', on: true } })).toThrow('intent.value.kind');
    // The choice of how stereo arrives is gone (ADR 0035, the 2026-10-16 correction).
    expect(() => assertIntent({ kind: 'set_upmix_sources', value: { endpoint: 1, sources: { pcm: true, bitstream: true } } })).toThrow(
      'intent.kind',
    );
    expect(() => assertIntent({ kind: 'set_upmixer', value: { endpoint: '2', choice: 'plugin' } })).toThrow('intent.value.endpoint');
    expect(assertIntent({ kind: 'set_upmixer', value: { endpoint: 2, choice: 'plugin' } })).toEqual({
      kind: 'set_upmixer',
      value: { endpoint: 2, choice: 'plugin' },
    });
  });

  it('an upmix state, plugin format or choice bindings.ts does not have', () => {
    const music = () => {
      const value = structuredClone(rawFixture('running-upmix')) as {
        upmix: { endpoints: { state: unknown; plugin: { format: unknown }; effective: unknown }[] };
      };
      const endpoint = value.upmix.endpoints[1];
      if (endpoint === undefined) throw new Error('running-upmix has a music endpoint');
      return { value, endpoint };
    };
    let { value, endpoint } = music();
    endpoint.state = { kind: 'paused' };
    expect(() => assertSnapshot(value)).toThrow('snapshot.upmix.endpoints[1].state.kind');
    ({ value, endpoint } = music());
    endpoint.state = { kind: 'failed' };
    expect(() => assertSnapshot(value)).toThrow('snapshot.upmix.endpoints[1].state.value');
    ({ value, endpoint } = music());
    endpoint.plugin.format = 'au';
    expect(() => assertSnapshot(value)).toThrow('snapshot.upmix.endpoints[1].plugin.format');
    ({ value, endpoint } = music());
    endpoint.effective = null;
    expect(() => assertSnapshot(value)).toThrow('snapshot.upmix.endpoints[1].effective');
  });

  it('a bypass, a choice of how stereo arrives or an input list bindings.ts does not have', () => {
    const pass = () => {
      const value = structuredClone(rawFixture('running-upmix-native-surround')) as {
        upmix: { endpoints: Record<string, unknown>[] };
      };
      const endpoint = value.upmix.endpoints[2];
      if (endpoint === undefined) throw new Error('running-upmix-native-surround has a PASS endpoint');
      return { value, endpoint };
    };
    expect(assertSnapshot(pass().value).upmix.endpoints.map((endpoint) => endpoint.endpoint)).toEqual([0, 1, 2]);
    expect(assertSnapshot(pass().value).upmix.endpoints[2]?.bypass).toBe('native_surround');
    for (const bypass of ['kind_off', 'rate_change', null]) {
      const { value, endpoint } = pass();
      endpoint.bypass = bypass;
      expect(assertSnapshot(value).upmix.endpoints[2]?.bypass).toBe(bypass);
    }
    for (const bypass of ['source_off', 'multichannel']) {
      const { value, endpoint } = pass();
      endpoint.bypass = bypass;
      expect(() => assertSnapshot(value), bypass).toThrow('snapshot.upmix.endpoints[2].bypass');
    }
    let { value, endpoint } = pass();
    delete endpoint.bypass;
    expect(() => assertSnapshot(value)).toThrow('snapshot.upmix.endpoints[2].bypass');
    ({ value, endpoint } = pass());
    endpoint.sources = { pcm: true, bitstream: true };
    expect(() => assertSnapshot(value)).toThrow('snapshot.upmix.endpoints[2].sources');
    ({ value, endpoint } = pass());
    endpoint.inputs = 'L R';
    expect(() => assertSnapshot(value)).toThrow('snapshot.upmix.endpoints[2].inputs');
    ({ value, endpoint } = pass());
    endpoint.inputs = ['L', 2];
    expect(() => assertSnapshot(value)).toThrow('snapshot.upmix.endpoints[2].inputs[1]');
  });
});
