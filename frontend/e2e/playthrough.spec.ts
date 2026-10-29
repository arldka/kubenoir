import { expect, test, type Page } from '@playwright/test';

/** The real database URL: the value the misspelled variable already holds. */
const CORRECT_URL = 'postgres://payments@postgres.default.svc.cluster.local:5432/payments';

const screen = (page: Page) => page.locator('.xterm-rows');

/** Type a command at the prompt and press Enter. */
async function run(page: Page, command: string) {
	await page.keyboard.type(command);
	await page.keyboard.press('Enter');
}

async function openCase(page: Page) {
	const problems: string[] = [];
	page.on('pageerror', (e) => problems.push(`pageerror: ${e.message}`));
	page.on('console', (m) => m.type() === 'error' && problems.push(`console: ${m.text()}`));
	await page.goto('/');
	await expect(screen(page)).toContainText('detective@cluster:~$');
	await page.locator('.xterm').click();
	return problems;
}

test('the whole case can be solved in the browser', async ({ page }) => {
	const problems = await openCase(page);
	await expect(page.getByText("The Payments Pod That Wouldn't Stay Up").first()).toBeVisible();

	// Investigate.
	await run(page, 'kubectl get pods');
	await expect(screen(page)).toContainText('CrashLoopBackOff');

	// Tab completes the live pod name (the only pod starting with "pay").
	await page.keyboard.type('kubectl logs pay');
	await page.keyboard.press('Tab');
	await expect(screen(page)).toContainText(/kubectl logs payments-api-\w+-\w+/);
	await page.keyboard.press('Enter');
	await expect(screen(page)).toContainText('required environment variable DATABASE_URL is not set');

	// History: the previous command comes back with Up.
	await page.keyboard.press('ArrowUp');
	await expect(screen(page)).toContainText(/\$ kubectl logs payments-api-\w+-\w+\s*$/m);
	await page.keyboard.press('Control+C');

	// Fix it the real way and let the cluster clock run while the rollout completes.
	await run(page, `kubectl set env deployment/payments-api DATABASE_URL=${CORRECT_URL} DATABSE_URL-`);
	await expect(screen(page)).toContainText('deployment.apps/payments-api env updated');
	await expect
		.poll(
			async () => {
				await run(page, 'clear');
				await run(page, 'kubectl get pods');
				const text = await screen(page).innerText();
				return /payments-api-\S+\s+1\/1\s+Running/.test(text) && !text.includes('CrashLoopBackOff');
			},
			{ timeout: 30_000, intervals: [1000] }
		)
		.toBe(true);

	// The sidebar clock has been running too.
	await expect(page.getByText('00:00:00').first()).toBeHidden();

	// Once the fix has held for the stability window the judge closes the case.
	await expect(page.getByTestId('report-outcome')).toHaveText('Case closed', { timeout: 40_000 });
	const report = page.getByRole('dialog');
	await expect(report).toContainText('Findings: 1 of 3 uncovered');
	await expect(report).toContainText('payments-api is available again');
	await expect(report).toContainText('the web deployment was left alone');
	await expect(report).toContainText('Hints used');
	await report.getByRole('button', { name: 'Back to the terminal' }).click();
	await expect(page.getByTestId('objective-state')).toHaveText('Closed');
	await expect(page.getByRole('button', { name: 'Open the case report' })).toBeVisible();
	expect(problems).toEqual([]);
});

test('findings are uncovered by investigating, and counted in the sidebar', async ({ page }) => {
	await openCase(page);
	const counter = page.getByTestId('findings-counter');
	await expect(counter).toContainText('0 / 3');
	await expect(page.getByText('???')).toHaveCount(3);
	// The objective is shown in the briefing's wording and nothing else is given away.
	await expect(page.getByTestId('objective-state')).toHaveText('Open');
	await expect(page.getByText(/last words/)).toBeHidden();

	await run(page, 'kubectl get pods');
	await expect(counter).toContainText('0 / 3');

	await run(page, 'kubectl logs deploy/payments-api');
	await expect(counter).toContainText('1 / 3');
	await expect(page.getByText(/The app's last words/)).toBeVisible();

	await run(page, 'kubectl describe deployment payments-api');
	await expect(counter).toContainText('2 / 3');
	await expect(page.getByText(/misspelled/)).toBeVisible();

	await run(page, 'kubectl logs payments-api-does-not-exist');
	await expect(counter).toContainText('2 / 3');

	await run(page, 'kubectl get events');
	await expect(counter).toContainText('3 / 3');
	await expect(page.getByText('???')).toHaveCount(0);
});

test('breaking the rest of the cluster turns the result into a cost', async ({ page }) => {
	await page.clock.install();
	await openCase(page);

	await run(page, 'kubectl rollout restart deployment/web');
	await expect(screen(page)).toContainText('deployment.apps/web restarted');
	await run(page, `kubectl set env deployment/payments-api DATABASE_URL=${CORRECT_URL} DATABSE_URL-`);
	await expect(screen(page)).toContainText('env updated');

	// Let the cluster run for a minute of its own time instead of waiting for it.
	await page.clock.fastForward(60_000);
	await expect(page.getByTestId('report-outcome')).toHaveText('Closed at a cost', { timeout: 15_000 });
	const report = page.getByRole('dialog');
	await expect(report).toContainText('the web deployment was left alone');
	await expect(report.locator('li', { hasText: 'the web deployment was left alone' })).toContainText('Broken:');
	await expect(report.locator('li', { hasText: 'no web pod was disturbed' })).not.toContainText('Broken:');
});

test('asking for hints is part of the report', async ({ page }) => {
	await page.clock.install();
	await openCase(page);
	await page.getByRole('button', { name: /Request a hint/ }).click();
	await page.getByRole('button', { name: /Request a hint/ }).click();
	await page.locator('.xterm').click();

	await run(page, `kubectl set env deployment/payments-api DATABASE_URL=${CORRECT_URL} DATABSE_URL-`);
	await expect(screen(page)).toContainText('env updated');
	await page.clock.fastForward(60_000);

	const report = page.getByRole('dialog');
	await expect(report).toContainText('Case closed', { timeout: 15_000 });
	await expect(report.getByText('Hints used').locator('..')).toContainText('2');
	await expect(report).toContainText('Findings: 0 of 3 uncovered');

	// Play again resets everything, including the sidebar and the verdict.
	await report.getByRole('button', { name: 'Play again' }).click();
	await expect(page.getByRole('dialog')).toBeHidden();
	await expect(page.getByTestId('objective-state')).toHaveText('Open');
	await expect(page.getByTestId('findings-counter')).toContainText('0 / 3');
	await expect(page.getByText('Hint 1.')).toBeHidden();
});

test('hints are revealed one at a time and the case can be restarted', async ({ page }) => {
	await openCase(page);

	await page.getByRole('button', { name: /Request a hint/ }).click();
	await expect(page.getByText('Hint 1.')).toBeVisible();
	await expect(page.getByRole('button', { name: /1\/3 used/ })).toBeVisible();

	// Clicking a button in the sidebar moves keyboard focus there; click back into the terminal.
	await page.locator('.xterm').click();
	await run(page, `kubectl set env deployment/payments-api DATABASE_URL=${CORRECT_URL} DATABSE_URL-`);
	await expect(screen(page)).toContainText('env updated');

	await page.getByRole('button', { name: 'Restart case' }).click();
	await page.getByRole('button', { name: 'Restart', exact: true }).click();

	await expect(page.getByText('Hint 1.')).toBeHidden();
	await expect(screen(page)).toContainText('CASE FILE:');
	await expect(screen(page)).not.toContainText('env updated');
	await page.locator('.xterm').click();
	await run(page, 'kubectl get pods');
	await expect(screen(page)).toContainText('CrashLoopBackOff');
});

test('a wrong database URL does not bring the service back; the right one does', async ({ page }) => {
	await page.clock.install();
	await openCase(page);

	await run(page, 'kubectl set env deployment/payments-api DATABASE_URL=postgres://nope');
	await expect(screen(page)).toContainText('env updated');
	await page.clock.fastForward(120_000);

	await run(page, 'clear');
	await run(page, 'kubectl get pods');
	await expect(screen(page)).toContainText('CrashLoopBackOff');
	await expect(screen(page)).not.toContainText(/payments-api-\S+\s+1\/1\s+Running/);
	await expect(page.getByRole('dialog')).toBeHidden();
	await expect(page.getByTestId('objective-state')).toHaveText('Open');

	// Setting the real value afterwards still solves the case.
	await run(page, 'clear');
	await run(page, `kubectl set env deployment/payments-api DATABASE_URL=${CORRECT_URL}`);
	await expect(screen(page)).toContainText('env updated');
	await page.clock.fastForward(90_000);
	await expect(page.getByTestId('report-outcome')).toHaveText('Case closed', { timeout: 15_000 });
	await expect(page.getByRole('dialog')).toContainText('payments-api is configured with the real database URL');
});

test('the terminal built-ins and error output work', async ({ page }) => {
	await openCase(page);
	await run(page, 'help');
	await expect(screen(page)).toContainText('kubectl logs POD|deploy/NAME');
	await run(page, 'kubectl get nodes');
	await expect(screen(page)).toContainText('the server doesn\'t have a resource type "nodes"');
	await run(page, 'kubectl get pods -n kube-system');
	await expect(screen(page)).toContainText('etcd-control-plane');
});

test('the layout fits a phone screen', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 800 });
	await openCase(page);

	// The case file starts collapsed; the terminal is usable right away.
	await expect(page.getByRole('button', { name: 'Show case file' })).toBeVisible();
	await run(page, 'kubectl get pods');
	await expect(screen(page)).toContainText('CrashLoopBackOff');

	await page.getByRole('button', { name: 'Show case file' }).click();
	await expect(page.getByText('Case file No.')).toBeVisible();

	const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
	expect(overflow).toBeLessThanOrEqual(0);
});

test('the case and its apps are fetched from the local registry', async ({ page }) => {
	const fetched: string[] = [];
	page.on('request', (r) => fetched.push(new URL(r.url()).pathname));
	await openCase(page);
	expect(fetched).toEqual(
		expect.arrayContaining([
			'/registry/scenarios/index.json',
			'/registry/scenarios/crashloop-env-typo.json',
			'/registry/apps/db-client/0.1.0.wasm',
			'/registry/apps/nginx/0.1.0.wasm'
		])
	);
	await page.goto('/?case=crashloop-env-typo');
	await expect(screen(page)).toContainText('detective@cluster:~$');
});

test('an unknown case says what could not be found', async ({ page }) => {
	await page.goto('/?case=no-such-case');
	await expect(page.getByRole('alert')).toContainText('Case "no-such-case" could not be fetched');
	await expect(page.getByRole('alert')).toContainText('/registry/scenarios/no-such-case.json (404)');
});
