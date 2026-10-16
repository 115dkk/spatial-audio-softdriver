import './styles.css';
import { connect, toUserError } from './api';
import { mountApp } from './app';
import { h } from './dom';
import { text } from './text';
import type { Tab } from './screens/common';

const root = document.getElementById('app');
if (root === null) throw new Error('#app is missing from index.html');

const params = new URLSearchParams(location.search);

function tabParam(): Tab | undefined {
  const tab = params.get('tab');
  return tab === 'status' || tab === 'profile' || tab === 'upmix' || tab === 'settings' ? tab : undefined;
}

async function start(host: HTMLElement) {
  if (params.has('gallery')) {
    const { gallery } = await import('./gallery');
    await gallery(host, params, tabParam());
    return;
  }
  const api = await connect(params.get('fixture') ?? undefined);
  const tab = tabParam();
  const app = mountApp(host, api, tab === undefined ? {} : { tab });
  let seen = false;
  await api.onSnapshot((snapshot) => {
    seen = true;
    app.draw(snapshot);
  }, app.report);
  const first = await api.snapshot();
  if (!seen) app.draw(first);
  await api.onMeters(app.updateMeters);
}

start(root).catch((reason: unknown) => {
  // The gallery's pictures must fail on this (as a page error), not show it.
  if (params.has('gallery')) {
    setTimeout(() => {
      throw reason;
    }, 0);
  }
  // Nothing to draw without a snapshot: say why and offer a reload.
  const error = toUserError(reason);
  root.replaceChildren(
    h(
      'div',
      { class: 'fatal', role: 'alert' },
      h('p', null, error.message),
      h('button', { type: 'button', class: 'button secondary', onClick: () => location.reload() }, text.banner.retry),
    ),
  );
});
