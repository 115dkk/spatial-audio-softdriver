// Screenshots for review: every fixture at 1280x800 in the light palette
// (<name>.png) and the dark one (<name>-dark.png), plus the profile screen
// (off and running), the settings screen and the smaller windows. The page
// must load without a console error and with IBM Plex Sans KR in place.

import { mkdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { expect, type Page, test } from '@playwright/test';

const fixtures: string[] = JSON.parse(readFileSync(new URL('../fixtures/index.json', import.meta.url), 'utf8'));

interface Shot {
  name: string;
  query: string;
  width: number;
  height: number;
  schemes: ('light' | 'dark')[];
}

const shots: Shot[] = [
  ...fixtures.map((name) => ({
    name,
    query: `fixture=${name}`,
    width: 1280,
    height: 800,
    schemes: ['light', 'dark'] as ('light' | 'dark')[],
  })),
  { name: 'ready-profile', query: 'fixture=ready&tab=profile', width: 1280, height: 800, schemes: ['light', 'dark'] },
  { name: 'running-profile', query: 'fixture=running&tab=profile', width: 1280, height: 800, schemes: ['light', 'dark'] },
  { name: 'running-profile-960x640', query: 'fixture=running&tab=profile', width: 960, height: 640, schemes: ['light'] },
  { name: 'running-profile-720x480', query: 'fixture=running&tab=profile', width: 720, height: 480, schemes: ['light'] },
  { name: 'ready-settings', query: 'fixture=ready&tab=settings', width: 1280, height: 800, schemes: ['light', 'dark'] },
  { name: 'running-960x640', query: 'fixture=running', width: 960, height: 640, schemes: ['light'] },
  { name: 'running-720x480', query: 'fixture=running', width: 720, height: 480, schemes: ['light'] },
  { name: 'setup-choose-output-720x480', query: 'fixture=setup-choose-output', width: 720, height: 480, schemes: ['light'] },
];

async function open(page: Page, shot: Shot, scheme: 'light' | 'dark', problems: string[]) {
  page.on('console', (message) => {
    if (message.type() === 'error' || message.type() === 'warning') problems.push(message.text());
  });
  page.on('pageerror', (error) => problems.push(error.message));
  await page.setViewportSize({ width: shot.width, height: shot.height });
  await page.emulateMedia({ colorScheme: scheme });
  await page.goto(`/?gallery=1&${shot.query}`);
  await page.locator('#app[data-ready="true"]').waitFor();
  await page.evaluate(() => document.fonts.ready);
  const fontLoaded = await page.evaluate(() => document.fonts.check('400 14px "IBM Plex Sans KR"', '공간'));
  expect(fontLoaded, 'IBM Plex Sans KR is loaded').toBe(true);
}

test.describe.configure({ mode: 'parallel' });

for (const shot of shots) {
  test(shot.name, async ({ page }, info) => {
    const dir = String(info.config.metadata.shotsDir);
    mkdirSync(dir, { recursive: true });
    for (const scheme of shot.schemes) {
      const problems: string[] = [];
      await open(page, shot, scheme, problems);
      const file = join(dir, scheme === 'dark' ? `${shot.name}-dark.png` : `${shot.name}.png`);
      await page.screenshot({ path: file, fullPage: true });
      expect(problems, 'console errors').toEqual([]);
    }
  });
}
