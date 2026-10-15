// The fixtures Rust writes for each state the screen must draw
// (`cargo test -p ssr-types --features ts --test ui_contract`). The gallery,
// the browser mock and the tests read them through here.

import type { Snapshot } from './bindings';
import { assertSnapshot } from './guard';
import order from '../fixtures/index.json';

const files = import.meta.glob<unknown>('../fixtures/*.json', { eager: true, import: 'default' });

/** Fixture names in the order index.json gives. */
export const fixtureNames: readonly string[] = order;

/** The raw JSON of a fixture, as Rust wrote it. */
export function rawFixture(name: string): unknown {
  const value = files[`../fixtures/${name}.json`];
  if (value === undefined) throw new Error(`no fixture named ${name}`);
  return value;
}

/** A fixture checked against the bindings; a fresh copy every call. */
export function fixture(name: string): Snapshot {
  return assertSnapshot(structuredClone(rawFixture(name)));
}
