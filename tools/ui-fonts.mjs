#!/usr/bin/env node
// Fetches the screen font, IBM Plex Sans KR (OFL-1.1), into
// apps/spatial-softreceiver/ui/public/fonts/ibm-plex-sans-kr/.
//
// The font files stay out of the repository: this script takes them from the
// npm tarball of @ibm/plex-sans-kr, checks the tarball and every file it keeps
// against the SHA-256 values below, and writes nothing that does not match.
// Files already present with the right hash are left alone. Node built-ins
// only, so `npm ci` is not needed to run it.

import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { gunzipSync } from 'node:zlib';

const TARBALL = {
  url: 'https://registry.npmjs.org/@ibm/plex-sans-kr/-/plex-sans-kr-1.1.0.tgz',
  sha256: 'd351afb104446fe1b877144a36f10b3c05ea675de187b573c35a73023c5f9ae5',
};

const SOURCE_DIR = 'package/fonts/complete/woff2/hinted/';

/** File name in the tarball's hinted woff2 directory, and its SHA-256. */
const FILES = {
  'IBMPlexSansKR-Regular.woff2': '055a35664c3c3965161c92292504c5633ba9604ec904edc1ed799bf2a436276d',
  'IBMPlexSansKR-Medium.woff2': 'c00dcd8a9c32a6b6ab8c6b3119e68b3d7aa6eba6f4441d5493019cc019dcdd1e',
  'IBMPlexSansKR-SemiBold.woff2': '5ad7db28ba74d59fe14c260205c62ddb701320f4f098d8a45ef2757bebc29666',
  'IBMPlexSansKR-Bold.woff2': 'cf874a368dc2c2c0e4d1933c35611b849e48942023bfdd1fa11128475dbc4860',
  'license.txt': '91c25c350d3cac39da2736d74f7ba37ef648f5237a4e330a240615bc8d8c4360',
};

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const target = join(root, 'apps', 'spatial-softreceiver', 'ui', 'public', 'fonts', 'ibm-plex-sans-kr');

const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');

function fail(message) {
  console.error(`ui-fonts: ${message}`);
  process.exit(1);
}

/** Names whose file is missing or differs; a differing file is removed. */
function stale() {
  const names = [];
  for (const [name, expected] of Object.entries(FILES)) {
    const path = join(target, name);
    if (!existsSync(path)) {
      names.push(name);
    } else if (sha256(readFileSync(path)) !== expected) {
      console.error(`ui-fonts: ${name} has the wrong SHA-256; removing it`);
      rmSync(path);
      names.push(name);
    }
  }
  return names;
}

/** Reads a NUL-terminated ASCII field of a tar header. */
function field(header, start, length) {
  const bytes = header.subarray(start, start + length);
  const end = bytes.indexOf(0);
  return bytes.subarray(0, end === -1 ? length : end).toString('utf8');
}

/**
 * The regular files of a POSIX (ustar) tar archive, by path. Understands the
 * ustar name prefix, pax `path` records and GNU long names, which is all an
 * npm tarball uses.
 */
function untar(archive) {
  const files = new Map();
  let offset = 0;
  let longName = null;
  while (offset + 512 <= archive.length) {
    const header = archive.subarray(offset, offset + 512);
    if (header.every((byte) => byte === 0)) break;
    const size = Number.parseInt(field(header, 124, 12).trim() || '0', 8);
    if (!Number.isFinite(size) || size < 0) throw new Error(`bad size at offset ${offset}`);
    const type = String.fromCharCode(header[156] || 0x30);
    const body = archive.subarray(offset + 512, offset + 512 + size);
    if (body.length !== size) throw new Error('truncated archive');
    offset += 512 + Math.ceil(size / 512) * 512;

    if (type === 'L') {
      longName = field(body, 0, body.length);
      continue;
    }
    if (type === 'x') {
      const match = /(?:^|\n)\d+ path=([^\n]*)\n/.exec(body.toString('utf8'));
      if (match) longName = match[1];
      continue;
    }
    if (type === 'g') continue;

    let name = field(header, 0, 100);
    const prefix = field(header, 345, 155);
    if (field(header, 257, 6).startsWith('ustar') && prefix) name = `${prefix}/${name}`;
    if (longName !== null) {
      name = longName;
      longName = null;
    }
    if (type === '0' || type === '\0' || type === '7') files.set(name, Buffer.from(body));
  }
  return files;
}

/** The body of `url`, with up to three tries for a dropped connection. */
async function download(url) {
  let lastError;
  for (let attempt = 1; attempt <= 3; attempt += 1) {
    try {
      const response = await fetch(url, { redirect: 'follow' });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      return Buffer.from(await response.arrayBuffer());
    } catch (error) {
      lastError = error;
      console.error(`ui-fonts: try ${attempt} failed: ${error instanceof Error ? error.message : error}`);
    }
  }
  throw lastError;
}

async function main() {
  const missing = stale();
  if (missing.length === 0) {
    console.log(`ui-fonts: up to date in ${target}`);
    return;
  }

  console.log(`ui-fonts: fetching ${TARBALL.url}`);
  let tarball;
  try {
    tarball = await download(TARBALL.url);
  } catch (error) {
    fail(`download failed: ${error instanceof Error ? error.message : error}`);
  }
  const tarballHash = sha256(tarball);
  if (tarballHash !== TARBALL.sha256) {
    fail(`tarball SHA-256 is ${tarballHash}, expected ${TARBALL.sha256}`);
  }

  const files = untar(gunzipSync(tarball));
  mkdirSync(target, { recursive: true });
  const written = [];
  for (const name of missing) {
    const bytes = files.get(SOURCE_DIR + name);
    if (!bytes) {
      for (const path of written) rmSync(path, { force: true });
      fail(`${SOURCE_DIR}${name} is not in the tarball`);
    }
    const hash = sha256(bytes);
    if (hash !== FILES[name]) {
      for (const path of written) rmSync(path, { force: true });
      fail(`${name} SHA-256 is ${hash}, expected ${FILES[name]}`);
    }
    const path = join(target, name);
    writeFileSync(path, bytes);
    written.push(path);
    console.log(`ui-fonts: ${name} ${hash}`);
  }

  const left = stale();
  if (left.length > 0) fail(`still wrong after writing: ${left.join(', ')}`);
}

await main();
