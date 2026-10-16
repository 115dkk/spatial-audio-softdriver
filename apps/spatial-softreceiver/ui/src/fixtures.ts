// The fixtures Rust writes for each state the screen must draw
// (`cargo test -p ssr-types --features ts --test ui_contract`): one snapshot
// per state in Korean (fixtures/<name>.json) and in English
// (fixtures/en/<name>.json), with the supervisor's sentences in that
// language. The gallery, the browser mock and the tests read them through
// here.

import type { Language, Snapshot } from './bindings';
import { assertSnapshot } from './guard';
import order from '../fixtures/index.json';

const files: Record<Language, Record<string, unknown>> = {
  ko: import.meta.glob<unknown>('../fixtures/*.json', { eager: true, import: 'default' }),
  en: import.meta.glob<unknown>('../fixtures/en/*.json', { eager: true, import: 'default' }),
};

const folders: Record<Language, string> = { ko: '../fixtures', en: '../fixtures/en' };

/** Fixture names in the order index.json gives; the same in both languages. */
export const fixtureNames: readonly string[] = order;

/** The raw JSON of a fixture in `language`, as Rust wrote it. */
export function rawFixture(name: string, language: Language = 'ko'): unknown {
  const value = files[language][`${folders[language]}/${name}.json`];
  if (value === undefined) throw new Error(`no ${language} fixture named ${name}`);
  return value;
}

/** A fixture checked against the bindings; a fresh copy every call. */
export function fixture(name: string, language: Language = 'ko'): Snapshot {
  return assertSnapshot(structuredClone(rawFixture(name, language)));
}
