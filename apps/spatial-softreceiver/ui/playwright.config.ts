import { existsSync, readdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium, defineConfig } from '@playwright/test';

// `npm run shots`: builds the screen, serves it with `vite preview` and has
// e2e/shots.spec.ts photograph every fixture in the light and dark palettes.
// Shots go to $SSR_UI_SHOTS_DIR (CI uploads it), else ui/shots/.

const here = dirname(fileURLToPath(import.meta.url));
const port = 4173;

/**
 * The Chromium to drive: $SSR_UI_CHROMIUM if given; else Playwright's own
 * build (what `npx playwright install chromium` puts in place, as CI does);
 * else the newest Chromium build under $PLAYWRIGHT_BROWSERS_PATH, for a
 * machine whose preinstalled browsers are another revision.
 */
function chromiumPath(): string | undefined {
  const explicit = process.env.SSR_UI_CHROMIUM;
  if (explicit) return explicit;
  if (existsSync(chromium.executablePath())) return undefined;
  const root = process.env.PLAYWRIGHT_BROWSERS_PATH;
  if (!root || !existsSync(root)) return undefined;
  const builds = readdirSync(root)
    .filter((name) => /^chromium-\d+$/.test(name))
    .sort((a, b) => Number(b.slice('chromium-'.length)) - Number(a.slice('chromium-'.length)));
  const binaries = [
    'chrome-linux64/chrome',
    'chrome-linux/chrome',
    'chrome-win64/chrome.exe',
    'chrome-win/chrome.exe',
    'chrome-mac-arm64/Chromium.app/Contents/MacOS/Chromium',
    'chrome-mac/Chromium.app/Contents/MacOS/Chromium',
  ];
  for (const build of builds) {
    for (const binary of binaries) {
      const path = join(root, build, binary);
      if (existsSync(path)) return path;
    }
  }
  return undefined;
}

const executablePath = chromiumPath();

export default defineConfig({
  testDir: join(here, 'e2e'),
  testMatch: '*.spec.ts',
  outputDir: join(here, 'shots', '.results'),
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: 0,
  reporter: 'list',
  metadata: { shotsDir: resolve(process.env.SSR_UI_SHOTS_DIR ?? join(here, 'shots')) },
  use: {
    baseURL: `http://127.0.0.1:${port}/`,
    browserName: 'chromium',
    deviceScaleFactor: 1,
    locale: 'ko-KR',
    launchOptions: executablePath ? { executablePath } : {},
  },
  webServer: {
    command: `npx vite build --logLevel warn && npx vite preview --host 127.0.0.1 --port ${port} --strictPort`,
    cwd: here,
    url: `http://127.0.0.1:${port}/`,
    reuseExistingServer: false,
    timeout: 120_000,
  },
});
