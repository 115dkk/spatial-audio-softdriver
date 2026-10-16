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
        'import_brir',
        'import_headphone_fir',
        'set_gain',
        'test_direction',
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
  });
});
