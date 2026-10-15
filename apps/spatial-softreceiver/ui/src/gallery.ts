// Review page (`?gallery=1`): every fixture drawn by the real screen code with
// the mock bridge. `&fixture=<name>` draws one fixture as the whole window
// (what the screenshots take); without it, each fixture is a 960x640 frame.
// In a person's browser the running fixtures' meters move (the object map
// turns); under automation (`navigator.webdriver`, the screenshots) they hold
// still so every shot is the fixture as written.

import { mountApp } from './app';
import { h } from './dom';
import { fixture, fixtureNames } from './fixtures';
import { createMockApi } from './mock';
import { text } from './text';
import type { Tab } from './screens/common';

export async function gallery(root: HTMLElement, name: string | null, tab: Tab | undefined): Promise<void> {
  if (name !== null) {
    const api = createMockApi(fixture(name), { meters: !navigator.webdriver, latencyMs: 400 });
    const app = mountApp(root, api, tab === undefined ? {} : { tab });
    app.draw(await api.snapshot());
    await api.onSnapshot(app.draw);
    await api.onMeters(app.updateMeters);
    return;
  }
  document.documentElement.classList.add('gallery-page');
  const list = h('div', { class: 'gallery' }, h('h1', null, text.gallery.title));
  for (const fixtureName of fixtureNames) {
    list.append(
      h(
        'section',
        { class: 'gallery-item' },
        h('h2', null, fixtureName),
        h('iframe', {
          src: `?gallery=1&fixture=${encodeURIComponent(fixtureName)}`,
          width: 960,
          height: 640,
          title: fixtureName,
          loading: 'lazy',
        }),
      ),
    );
  }
  root.replaceChildren(list);
}
