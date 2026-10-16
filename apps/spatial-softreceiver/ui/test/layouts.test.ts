// The mock's speaker layout table against the contract it stands in for:
// `SpeakerLayout` and its `speakers()` in crates/ssr-types/src/app.rs, and
// `Speaker::name()` and `Speaker::angles()` in crates/ssr-types/src/position.rs.
// The Rust source is read as text, so a change there fails here until the
// table follows.

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import type { SpeakerLayout } from '../src/bindings';
import { fixture } from '../src/fixtures';
import { SPEAKER_LAYOUTS } from '../src/guard';
import { LAYOUT_SPEAKERS, SPEAKER_ANGLES, speakerChannels, speakerDirections } from '../src/mock';

const typesSource = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..', '..', 'crates', 'ssr-types', 'src');
const rust = (file: string) => readFileSync(join(typesSource, file), 'utf8');

/** The text between `from` and the next `until` after it. */
function section(source: string, from: string, until: string): string {
  const start = source.indexOf(from);
  if (start < 0) throw new Error(`no ${from} in the Rust source`);
  const end = source.indexOf(until, start + from.length);
  return source.slice(start, end < 0 ? undefined : end);
}

const app = rust('app.rs');
const position = rust('position.rs');

/** Variant → serialized name, from the serde renames of `enum SpeakerLayout`. */
const serialized = new Map(
  [...section(app, 'pub enum SpeakerLayout {', '\n}').matchAll(/#\[serde\(rename = "([^"]+)"\)\]\s*(\w+),/g)].map(
    ([, name, variant]) => [variant!, name!] as const,
  ),
);

/** The serialized names in `SpeakerLayout::ALL` order. */
const order = [...section(app, 'pub const ALL', '];').matchAll(/Self::(\w+)/g)].map(([, variant]) => {
  const name = serialized.get(variant!);
  if (name === undefined) throw new Error(`SpeakerLayout::${variant} has no serde rename`);
  return name;
});

/** `Speaker` variant → its `name()`. */
const speakerName = new Map(
  [...section(position, 'pub const fn name(self)', 'pub ').matchAll(/(\w+) => "([^"]+)",/g)].map(
    ([, variant, name]) => [variant!, name!] as const,
  ),
);

/** `SpeakerLayout::speakers()`: serialized layout name → speaker names in channel order. */
const contractSpeakers = new Map(
  [...section(app, 'pub const fn speakers(self)', 'pub const fn').matchAll(/Self::(\w+) => &\[([^\]]*)\],/g)].map(
    ([, variant, list]) => [
      serialized.get(variant!),
      list!.split(',').map((speaker) => {
        const name = speakerName.get(speaker.trim());
        if (name === undefined) throw new Error(`Speaker::${speaker.trim()} has no name`);
        return name;
      }),
    ],
  ),
);

/** `Speaker::angles()`: speaker name → [azimuth, elevation]. */
const contractAngles = new Map(
  [...section(position, 'pub const fn angles(self)', 'pub fn').matchAll(/(\w+) => \((-?[\d.]+), (-?[\d.]+)\),/g)].map(
    ([, variant, azimuth, elevation]) => [speakerName.get(variant!), [Number(azimuth), Number(elevation)]] as const,
  ),
);

describe("the mock's speaker layouts", () => {
  it('reads nine layouts from the contract', () => {
    expect(order).toEqual(['2.0', '2.1', '4.0', '5.1', '7.1', '5.1.2', '5.1.4', '7.1.2', '7.1.4']);
    expect([...contractSpeakers.keys()].sort()).toEqual([...order].sort());
  });

  it('lists the layouts in the order of SpeakerLayout::ALL, in the guard and the mock', () => {
    expect(Object.keys(SPEAKER_LAYOUTS)).toEqual(order);
    expect(Object.keys(LAYOUT_SPEAKERS)).toEqual(order);
  });

  it.each(order)('%s has the speakers of SpeakerLayout::speakers()', (layout) => {
    expect(LAYOUT_SPEAKERS[layout as SpeakerLayout]).toEqual(contractSpeakers.get(layout));
  });

  it('places every speaker but the subwoofer at its nominal angles', () => {
    const named = new Set(Object.values(LAYOUT_SPEAKERS).flat().filter((name) => name !== 'LFE'));
    expect(Object.keys(SPEAKER_ANGLES).sort()).toEqual([...named].sort());
    for (const name of named) expect(SPEAKER_ANGLES[name], name).toEqual(contractAngles.get(name));
    expect(contractAngles.has('LFE')).toBe(false);
  });

  it('draws what the supervisor wrote into the fixtures for a layout without correction', () => {
    const cases: [SpeakerLayout, string][] = [
      ['2.0', 'ready'],
      ['7.1', 'ready-headphones'],
      ['7.1.4', 'ready-layout-unsupported'],
    ];
    for (const [layout, name] of cases) {
      const { speakers, kind, directions } = fixture(name).profile;
      expect(speakers.layout, name).toBe(layout);
      expect(speakers.correction, name).toBeNull();
      expect(speakerChannels(layout), name).toEqual(speakers.channels);
      if (kind === 'speakers') expect(speakerDirections(layout), name).toEqual(directions);
    }
  });
});
