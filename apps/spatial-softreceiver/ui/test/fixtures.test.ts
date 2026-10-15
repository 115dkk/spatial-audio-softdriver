import { describe, expect, it } from 'vitest';
import intents from '../fixtures/intents.json';
import { fixtureNames, rawFixture } from '../src/fixtures';
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

  it('covers every intent', () => {
    const kinds = intents.map((intent) => assertIntent(intent).kind);
    expect(new Set(kinds)).toEqual(
      new Set([
        'begin_setup',
        'install_driver',
        'recheck',
        'choose_output',
        'import_brir',
        'import_headphone_fir',
        'switch_profile',
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
