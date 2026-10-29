import { existsSync, readdirSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

import { defineConfig } from '@playwright/test';

/**
 * A Chromium to drive: $PLAYWRIGHT_CHROMIUM_PATH, else the newest one in Playwright's cache
 * (so a browser that is already installed works without `playwright install`).
 */
function chromium(): string | undefined {
	if (process.env.PLAYWRIGHT_CHROMIUM_PATH) return process.env.PLAYWRIGHT_CHROMIUM_PATH;
	const cache = join(homedir(), '.cache', 'ms-playwright');
	if (!existsSync(cache)) return undefined;
	const builds = readdirSync(cache)
		.filter((d) => /^chromium-\d+$/.test(d))
		.sort()
		.reverse();
	return builds.map((d) => join(cache, d, 'chrome-linux64', 'chrome')).find(existsSync);
}

const port = 4173;

export default defineConfig({
	testDir: 'e2e',
	timeout: 90_000,
	expect: { timeout: 15_000 },
	retries: 0,
	webServer: {
		command: `bun run build && bun run preview --port ${port}`,
		url: `http://localhost:${port}`,
		reuseExistingServer: !process.env.CI,
		timeout: 240_000
	},
	use: {
		baseURL: `http://localhost:${port}`,
		launchOptions: { executablePath: chromium(), args: ['--no-sandbox'] },
		screenshot: 'only-on-failure'
	}
});
