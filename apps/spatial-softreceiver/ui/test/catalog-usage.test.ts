// @vitest-environment node
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath, URL } from 'node:url';
import { describe, expect, it } from 'vitest';
import { en, ko } from '../src/text';

function leafPaths(value: unknown, path = ''): string[] {
  if (Array.isArray(value)) {
    return value.flatMap((child, index) => leafPaths(child, `${path}[${index}]`));
  }
  if (value !== null && typeof value === 'object') {
    return Object.entries(value).flatMap(([key, child]) => leafPaths(child, path ? `${path}.${key}` : key));
  }
  return [path];
}

function consumerSources(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return consumerSources(path);
    // Neither generated declarations nor the projection itself prove a use.
    if (!entry.name.endsWith('.ts') || /\.(test|spec)\.ts$/.test(entry.name) ||
      ['catalog.ts', 'bindings.ts', 'text.ts'].includes(entry.name)) return [];
    return [readFileSync(path, 'utf8')];
  });
}

// format.ts reads these labels by the engine's runtime ChainErrorKind, not a
// literal member access. Keep the exact paths so additions need an audit.
const dynamicLeaves = [
  'chainErrorKinds.unsupported_codec',
  'chainErrorKinds.unsupported_format',
  'chainErrorKinds.corrupt_payload',
  'chainErrorKinds.missing_format',
  'chainErrorKinds.wrong_route',
  'chainErrorKinds.unsupported_scene_rate',
  'chainErrorKinds.truncated_record',
  'chainErrorKinds.bad_record_length',
  'chainErrorKinds.bad_format_record',
  'chainErrorKinds.bad_volume_record',
  'chainErrorKinds.missing_direct_layout',
  'chainErrorKinds.decoder_behind',
];

function lastKey(path: string): string {
  // Array entries are used through their containing key (welcomeBody, steps).
  return path.split('.').at(-1)!.replace(/\[\d+\]/g, '');
}

function hasKey(source: string, path: string): boolean {
  const key = lastKey(path).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return new RegExp(`\\b${key}\\b`).test(source);
}

describe('screen catalog usage', () => {
  it('every projected text leaf has a non-generated consumer or an explicit dynamic use', () => {
    const source = consumerSources(fileURLToPath(new URL('../src', import.meta.url))).join('\n');
    for (const catalog of [ko, en]) {
      const paths = leafPaths(catalog);
      expect(paths.length).toBeGreaterThan(0);
      expect(dynamicLeaves.filter((path) => !paths.includes(path)), 'stale dynamic paths').toEqual([]);
      expect(dynamicLeaves.filter((path) => hasKey(source, path)), 'unnecessary exemptions').toEqual([]);
      expect(paths.filter((path) => !dynamicLeaves.includes(path) && !hasKey(source, path)), 'unused text leaves').toEqual([]);
    }
  });

  it('detects unused scalar and array keys without exempting whole tables', () => {
    const sample = leafPaths({ shown: ['one', 'two'], deadLabel: 'unused', enumLabels: { deadKind: 'unused' } });
    expect(sample.filter((path) => !hasKey('text.shown.map(render)', path))).toEqual(['deadLabel', 'enumLabels.deadKind']);
  });
});
