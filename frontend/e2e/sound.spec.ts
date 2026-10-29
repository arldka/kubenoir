import { expect, test, type Page } from '@playwright/test';

/** Record what the page does with Web Audio, so the tests can see it without listening. */
async function spyOnAudio(page: Page) {
	await page.addInitScript(() => {
		/* eslint-disable @typescript-eslint/no-explicit-any */
		const Original = (window as any).AudioContext;
		const log = { created: 0, resumed: 0, starts: 0, gains: [] as any[] };
		(window as any).__audio = log;
		(window as any).AudioContext = class extends Original {
			constructor(...args: unknown[]) {
				super(...args);
				log.created++;
			}
			createGain() {
				const gain = super.createGain();
				log.gains.push(gain);
				return gain;
			}
			createBufferSource() {
				const source = super.createBufferSource();
				const start = source.start.bind(source);
				source.start = (...args: unknown[]) => {
					log.starts++;
					return start(...args);
				};
				return source;
			}
			resume() {
				log.resumed++;
				return super.resume();
			}
		};
	});
}

const audio = (page: Page) =>
	page.evaluate(() => {
		const log = (window as any).__audio;
		return { created: log.created, resumed: log.resumed, starts: log.starts, master: log.gains[0]?.gain.value ?? null };
	});

const masterGain = async (page: Page) => (await audio(page)).master as number | null;

async function openGame(page: Page) {
	const problems: string[] = [];
	page.on('pageerror', (e) => problems.push(`pageerror: ${e.message}`));
	page.on('console', (m) => m.type() === 'error' && problems.push(`console: ${m.text()}`));
	page.on('requestfailed', (r) => problems.push(`request failed: ${r.url()}`));
	await spyOnAudio(page);
	await page.goto('/');
	await expect(page.locator('.xterm-rows')).toContainText('detective@cluster:~$');
	return problems;
}

test('nothing plays until the first click or key, then the sound starts', async ({ page }) => {
	const problems = await openGame(page);
	expect((await audio(page)).created).toBe(0);

	await page.locator('.xterm').click();
	await expect.poll(async () => (await audio(page)).created).toBe(1);
	expect((await audio(page)).resumed).toBeGreaterThan(0);
	await expect.poll(() => masterGain(page)).toBeCloseTo(0.36, 1); // the default volume, 60%, squared
	expect(problems).toEqual([]);
});

test('a key press alone also starts the sound', async ({ page }) => {
	await openGame(page);
	await page.keyboard.press('Shift');
	await expect.poll(async () => (await audio(page)).created).toBe(1);
});

test('the volume slider and the mute button drive the master volume', async ({ page }) => {
	await openGame(page);
	await page.locator('.xterm').click();
	await expect.poll(() => masterGain(page)).toBeCloseTo(0.36, 1);

	const slider = page.getByTestId('volume-slider');
	await slider.focus();
	await page.keyboard.press('End');
	await expect(slider).toHaveValue('100');
	await expect(slider).toHaveAttribute('aria-valuetext', '100 percent');
	await expect.poll(() => masterGain(page)).toBeCloseTo(1, 1);

	await page.keyboard.press('Home');
	await expect(slider).toHaveValue('0');
	await expect.poll(() => masterGain(page)).toBeCloseTo(0, 1);

	await slider.fill('50');
	await expect.poll(() => masterGain(page)).toBeCloseTo(0.25, 1);

	const mute = page.getByTestId('mute-toggle');
	await mute.click();
	await expect(mute).toHaveAttribute('aria-pressed', 'true');
	await expect(mute).toHaveAccessibleName('Unmute');
	await expect.poll(() => masterGain(page)).toBeCloseTo(0, 1);

	// Moving the slider while muted brings the sound back.
	await slider.fill('80');
	await expect(mute).toHaveAttribute('aria-pressed', 'false');
	await expect.poll(() => masterGain(page)).toBeCloseTo(0.64, 1);
});

test('volume and mute are remembered across reloads', async ({ page }) => {
	await openGame(page);
	await page.getByTestId('volume-slider').fill('25');
	await page.getByTestId('mute-toggle').click();

	await page.reload();
	await expect(page.locator('.xterm-rows')).toContainText('detective@cluster:~$');
	await expect(page.getByTestId('volume-slider')).toHaveValue('25');
	await expect(page.getByTestId('mute-toggle')).toHaveAttribute('aria-pressed', 'true');

	await page.locator('.xterm').click();
	await expect.poll(() => masterGain(page)).toBeCloseTo(0, 1); // muted from the first moment
});

test('typing makes typewriter sounds, and not while muted', async ({ page }) => {
	const problems = await openGame(page);
	await page.locator('.xterm').click();
	await expect.poll(() => masterGain(page)).toBeCloseTo(0.36, 1);

	const before = (await audio(page)).starts;
	await page.keyboard.type('kubectl');
	await page.keyboard.press('Enter');
	await expect.poll(async () => (await audio(page)).starts - before).toBeGreaterThanOrEqual(3);

	// Cursor movement stays silent.
	const quiet = (await audio(page)).starts;
	await page.keyboard.press('ArrowLeft');
	await page.keyboard.press('ArrowRight');
	await page.waitForTimeout(200);
	expect((await audio(page)).starts).toBe(quiet);

	await page.getByTestId('mute-toggle').click();
	await page.locator('.xterm').click();
	const muted = (await audio(page)).starts;
	await page.keyboard.type('abc');
	await page.waitForTimeout(300);
	expect((await audio(page)).starts).toBe(muted);
	expect(problems).toEqual([]);
});

test('the sound panel is reachable and labelled', async ({ page }) => {
	await openGame(page);
	await expect(page.getByRole('heading', { name: 'Sound' })).toBeVisible();
	await expect(page.getByRole('slider', { name: 'Volume' })).toBeVisible();
	await expect(page.getByRole('button', { name: 'Mute' })).toBeVisible();
});
