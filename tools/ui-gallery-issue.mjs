#!/usr/bin/env node
// Checks a rendered screen gallery against its scene table and writes the
// review issue's body (.github/workflows/ui-gallery.yml).
//
//   node tools/ui-gallery-issue.mjs --shots DIR --check
//   node tools/ui-gallery-issue.mjs --shots DIR --base URL --tree URL \
//        --run URL --run-number N --commit SHA --source REF > body.md
//
// The scene table is apps/spatial-softreceiver/ui/e2e/gallery.json; the
// gallery spec takes <name>.<language>.<scheme>.png for every scene. The
// check fails when a picture is missing, empty or not in the table, when a
// fixture in fixtures/index.json is in no scene, or when the table itself is
// inconsistent. Node built-ins only, so `npm ci` is not needed to run it.

import { readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const ui = join(root, 'apps', 'spatial-softreceiver', 'ui');
const TABS = new Set(['status', 'profile', 'upmix', 'settings']);
const SCHEMES = new Set(['light', 'dark']);
/** The app speaks exactly these; the gallery shows every screen in both. */
const LANGUAGES = ['ko', 'en'];
/** GitHub refuses an issue body over 65536 characters. */
const BODY_LIMIT = 65_000;

function fail(message) {
  console.error(`ui-gallery-issue: ${message}`);
  process.exit(1);
}

function args(argv) {
  const out = { check: false };
  for (let i = 0; i < argv.length; i += 1) {
    const key = argv[i];
    if (key === '--check') {
      out.check = true;
      continue;
    }
    if (!key.startsWith('--') || i + 1 >= argv.length) fail(`unexpected argument ${key}`);
    out[key.slice(2)] = argv[i + 1];
    i += 1;
  }
  if (out.shots === undefined) fail('--shots DIR is required');
  return out;
}

const readJson = (path) => JSON.parse(readFileSync(path, 'utf8'));

/** The table with defaults applied; throws on an inconsistent table. */
function scenes(table, fixtures) {
  const problems = [];
  const languages = [...table.languages].sort();
  if (languages.join() !== [...LANGUAGES].sort().join()) {
    problems.push(`languages must be exactly ${LANGUAGES.join(', ')}, not ${table.languages.join(', ')}`);
  }
  for (const language of LANGUAGES) {
    if (typeof table.languageNames?.[language] !== 'string') problems.push(`languageNames has no ${language}`);
  }
  for (const scheme of SCHEMES) {
    if (typeof table.schemeNames?.[scheme] !== 'string') problems.push(`schemeNames has no ${scheme}`);
  }
  const groups = new Set(table.groups.map((group) => group.id));
  const names = new Set();
  const used = new Set();
  const list = table.scenarios.map((scene) => {
    const full = {
      ...scene,
      size: scene.size ?? table.defaults.size,
      schemes: scene.schemes ?? table.defaults.schemes,
    };
    if (names.has(full.name)) problems.push(`scene ${full.name} is listed twice`);
    names.add(full.name);
    if (!/^[a-z0-9][a-z0-9x-]*$/.test(full.name)) problems.push(`scene name ${full.name} is not a plain file name`);
    if (!groups.has(full.group)) problems.push(`scene ${full.name} has no group ${full.group}`);
    if (!fixtures.includes(full.fixture)) problems.push(`scene ${full.name} uses unknown fixture ${full.fixture}`);
    if (full.tab !== null && !TABS.has(full.tab)) problems.push(`scene ${full.name} has unknown tab ${full.tab}`);
    if (full.schemes.length === 0) problems.push(`scene ${full.name} has no palette`);
    if (new Set(full.schemes).size !== full.schemes.length) problems.push(`scene ${full.name} lists a palette twice`);
    for (const scheme of full.schemes) if (!SCHEMES.has(scheme)) problems.push(`scene ${full.name}: scheme ${scheme}`);
    used.add(full.fixture);
    return full;
  });
  for (const fixture of fixtures) if (!used.has(fixture)) problems.push(`fixture ${fixture} is in no scene`);
  for (const group of table.groups) {
    if (!list.some((scene) => scene.group === group.id)) problems.push(`group ${group.id} has no scene`);
  }
  if (problems.length > 0) fail(`gallery.json:\n  ${problems.join('\n  ')}`);
  return list;
}

const file = (scene, language, scheme) => `${scene.name}.${language}.${scheme}.png`;

/** Text from outside (a branch name) shown as code: escaped, so it cannot close the element or mention anyone. */
const code = (value) =>
  `<code>${String(value).replace(/[&<>"'`@]/g, (character) => `&#${character.codePointAt(0)};`)}</code>`;

/** Fails unless DIR holds exactly the table's pictures, none empty. */
function checkShots(dir, table, list) {
  const expected = new Set();
  for (const scene of list) {
    for (const language of table.languages) for (const scheme of scene.schemes) expected.add(file(scene, language, scheme));
  }
  let present;
  try {
    present = readdirSync(dir).filter((name) => name.endsWith('.png'));
  } catch (error) {
    fail(`cannot read ${dir}: ${error.message}`);
  }
  const problems = [];
  for (const name of expected) {
    if (!present.includes(name)) problems.push(`missing ${name}`);
    else if (statSync(join(dir, name)).size === 0) problems.push(`empty ${name}`);
  }
  for (const name of present) if (!expected.has(name)) problems.push(`not in gallery.json: ${name}`);
  if (problems.length > 0) fail(`${dir}:\n  ${problems.join('\n  ')}`);
  return expected.size;
}

function body(options, table, list, count) {
  const need = ['base', 'tree', 'run', 'run-number', 'commit', 'source'];
  for (const key of need) if (options[key] === undefined) fail(`--${key} is required for the issue body`);
  const base = options.base.replace(/\/$/, '');
  const short = options.commit.slice(0, 7);
  const lines = [
    '<!-- ui-gallery -->',
    `**화면 갤러리** · 실행 [#${options['run-number']}](${options.run}) · 커밋 ${code(short)} (${code(options.source)})`,
    '',
    '앱의 모든 화면 상태를 한국어와 영어, 밝은 팔레트와 어두운 팔레트로 찍었습니다. 앱을 띄우지 않고, 실제 화면 코드를 Rust가 만든 시험 자료와 모의 감독자 위에서 그린 것입니다. 장면을 펼쳐 보고, 그림을 누르면 원래 크기로 열립니다.',
    '',
    `장면 ${list.length}개, 그림 ${count}장 · [원본 PNG](${options.tree}) · 실행의 \`ui-gallery\` 아티팩트에도 같은 그림이 있습니다.`,
    '',
    '장면 표는 `apps/spatial-softreceiver/ui/e2e/gallery.json`입니다. 새 화면 상태가 생기면 그 표에 장면을 더하고, Rust 쪽 상태라면 `crates/ssr-types/tests/ui_contract.rs`에 시험 자료를 더합니다.',
  ];
  for (const group of table.groups) {
    const scenesInGroup = list.filter((scene) => scene.group === group.id);
    lines.push('', `### ${group.title}`, '');
    for (const scene of scenesInGroup) {
      const [width, height] = scene.size;
      const where = scene.tab === null ? scene.fixture : `${scene.fixture}, ${scene.tab}`;
      lines.push(
        `<details><summary><b>${scene.title}</b> · <code>${scene.name}</code> · ${width}×${height} · ${where}</summary>`,
        '',
        `| | ${table.languages.map((language) => table.languageNames[language]).join(' | ')} |`,
        `|:-:|${table.languages.map(() => ':-:').join('|')}|`,
      );
      for (const scheme of scene.schemes) {
        const cells = table.languages.map((language) => {
          const name = file(scene, language, scheme);
          return `![${scene.name} ${language} ${scheme}](${base}/${name})`;
        });
        lines.push(`| ${table.schemeNames[scheme]} | ${cells.join(' | ')} |`);
      }
      lines.push('', '</details>');
    }
  }
  const text = `${lines.join('\n')}\n`;
  if (text.length > BODY_LIMIT) fail(`the body is ${text.length} characters; GitHub takes at most 65536`);
  return text;
}

const options = args(process.argv.slice(2));
const table = readJson(join(ui, 'e2e', 'gallery.json'));
const fixtures = readJson(join(ui, 'fixtures', 'index.json'));
const list = scenes(table, fixtures);
const count = checkShots(options.shots, table, list);
if (options.check) {
  console.log(`ui-gallery-issue: ${list.length} scenes, ${count} pictures, all present`);
} else {
  process.stdout.write(body(options, table, list, count));
}
