// The screen gallery (`?gallery=1`): every screen state drawn by the real
// screen code over the mock supervisor, from the scene table e2e/gallery.json.
//
// - `?gallery=1&scene=<name>&lang=<ko|en>` draws one scene as the whole
//   window: its fixture in that language, its tab and shell, then its steps.
//   When everything is drawn and the fonts are in, <html> gets
//   `data-gallery-ready="true"`; that is what e2e/gallery.spec.ts waits for
//   before it takes the picture. A scene that cannot be drawn throws.
// - `?gallery=1&fixture=<name>[&lang=…][&tab=…]` draws one fixture alone.
// - `?gallery=1` alone is the page a person reads: every scene, group by
//   group, in each language side by side.
//
// In a person's browser the running fixtures' meters move (the object map
// turns); under automation (`navigator.webdriver`, the screenshots) they hold
// still so every shot is the fixture as written.

import { mountApp } from './app';
import type { Language } from './bindings';
import { h } from './dom';
import { fixture } from './fixtures';
import { type GalleryShell, createMockApi } from './mock';
import { text } from './text';
import type { Tab } from './screens/common';
import table from '../e2e/gallery.json';

type Scheme = 'light' | 'dark';
type Step = { click: string } | { wait: string };

/** A scene as the table writes it; what it leaves out comes from `defaults`. */
interface SceneEntry {
  name: string;
  group: string;
  title: string;
  fixture: string;
  tab: Tab | null;
  shell?: GalleryShell;
  hold?: boolean;
  steps?: Step[];
  size?: [number, number];
  schemes?: Scheme[];
}

interface Table {
  languages: Language[];
  languageNames: Record<Language, string>;
  defaults: { size: [number, number]; schemes: Scheme[]; shell: GalleryShell };
  groups: { id: string; title: string }[];
  scenarios: SceneEntry[];
}

/** A scene with the table's defaults applied. */
export interface Scene {
  name: string;
  group: string;
  title: string;
  fixture: string;
  tab: Tab | null;
  shell: GalleryShell;
  hold: boolean;
  steps: Step[];
  size: [width: number, height: number];
  schemes: Scheme[];
}

const scenesTable = table as unknown as Table;

/** Every scene in table order, defaults applied. */
export const scenes: readonly Scene[] = scenesTable.scenarios.map((entry) => ({
  name: entry.name,
  group: entry.group,
  title: entry.title,
  fixture: entry.fixture,
  tab: entry.tab,
  shell: entry.shell ?? scenesTable.defaults.shell,
  hold: entry.hold ?? false,
  steps: entry.steps ?? [],
  size: entry.size ?? scenesTable.defaults.size,
  schemes: entry.schemes ?? scenesTable.defaults.schemes,
}));

/** How long a `wait` step looks for its selector. */
const WAIT_MS = 5000;

const nextFrame = () => new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));

function language(value: string | null): Language {
  if (value === null) return 'ko';
  if (value === 'ko' || value === 'en') return value;
  throw new Error(`gallery: unknown language ${value}`);
}

function visible(element: Element): boolean {
  return element.getClientRects().length > 0 && getComputedStyle(element).visibility !== 'hidden';
}

async function waitFor(selector: string, scene: string): Promise<void> {
  const until = performance.now() + WAIT_MS;
  for (;;) {
    const found = document.querySelector(selector);
    if (found !== null && visible(found)) return;
    if (performance.now() > until) {
      throw new Error(`gallery scene ${scene}: ${selector} did not show within ${WAIT_MS} ms`);
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
}

async function run(step: Step, scene: string): Promise<void> {
  if ('click' in step) {
    const target = document.querySelector<HTMLElement>(step.click);
    if (target === null) throw new Error(`gallery scene ${scene}: nothing to click at ${step.click}`);
    target.click();
  } else {
    await waitFor(step.wait, scene);
  }
}

/** Marks the page ready for its picture once the fonts are in and the frame is painted. */
async function ready(): Promise<void> {
  await document.fonts.ready;
  await nextFrame();
  await nextFrame();
  document.documentElement.dataset.galleryReady = 'true';
}

/** Draws the fixture `name` in `lang` as the whole window over a mock, as the app would. */
async function drawWindow(
  root: HTMLElement,
  name: string,
  lang: Language,
  tab: Tab | null | undefined,
  options: { shell?: GalleryShell; hold?: boolean } = {},
) {
  const api = createMockApi(fixture(name, lang), { meters: !navigator.webdriver, latencyMs: 400, ...options });
  const app = mountApp(root, api, tab === null || tab === undefined ? {} : { tab });
  app.draw(await api.snapshot());
  await api.onSnapshot(app.draw);
  await api.onMeters(app.updateMeters);
}

async function scenePage(root: HTMLElement, name: string, lang: Language): Promise<void> {
  const scene = scenes.find((candidate) => candidate.name === name);
  if (scene === undefined) throw new Error(`gallery: no scene named ${name} in e2e/gallery.json`);
  await drawWindow(root, scene.fixture, lang, scene.tab, { shell: scene.shell, hold: scene.hold });
  // The shell's autostart and updater answers land before the first step.
  await new Promise((resolve) => setTimeout(resolve, 0));
  for (const step of scene.steps) await run(step, scene.name);
  await ready();
}

function sceneUrl(scene: Scene, lang: Language): string {
  return `?gallery=1&scene=${encodeURIComponent(scene.name)}&lang=${lang}`;
}

function frame(scene: Scene, lang: Language): HTMLElement {
  const [width, height] = scene.size;
  const url = sceneUrl(scene, lang);
  const label = scenesTable.languageNames[lang];
  const viewport = h(
    'div',
    { class: 'gallery-viewport' },
    h('iframe', { src: url, width, height, title: `${scene.title} · ${label}`, loading: 'lazy' }),
  );
  viewport.style.setProperty('--frame-width', `${width}px`);
  viewport.style.setProperty('--frame-height', `${height}px`);
  return h(
    'figure',
    { class: 'gallery-frame' },
    h('figcaption', null, h('a', { href: url, target: '_blank', rel: 'noopener' }, label)),
    viewport,
  );
}

function listPage(root: HTMLElement): void {
  document.documentElement.classList.add('gallery-page');
  const page = h('div', { class: 'gallery' }, h('h1', null, text.gallery.title));
  for (const group of scenesTable.groups) {
    const section = h('section', { class: 'gallery-group' }, h('h2', null, group.title));
    for (const scene of scenes.filter((candidate) => candidate.group === group.id)) {
      const [width, height] = scene.size;
      section.append(
        h(
          'article',
          { class: 'gallery-scene' },
          h(
            'header',
            { class: 'gallery-scene-head' },
            h('h3', null, scene.title),
            h('code', null, scene.name),
            h('span', { class: 'muted small num' }, `${width}×${height}`),
          ),
          h('div', { class: 'gallery-frames' }, ...scenesTable.languages.map((lang) => frame(scene, lang))),
        ),
      );
    }
    page.append(section);
  }
  root.replaceChildren(page);
}

export async function gallery(root: HTMLElement, params: URLSearchParams, tab: Tab | undefined): Promise<void> {
  const scene = params.get('scene');
  const name = params.get('fixture');
  if (scene !== null) {
    await scenePage(root, scene, language(params.get('lang')));
  } else if (name !== null) {
    await drawWindow(root, name, language(params.get('lang')), tab);
    await ready();
  } else {
    listPage(root);
  }
}
