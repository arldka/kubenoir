import { describe, expect, it, vi } from 'vitest';

import { parseAppRef, pickCase, Registry, sourcesFromEnv, type Sources } from './registry';

/** A fake `fetch` over `url -> body`; anything else is a 404, and `down` hosts throw. */
function fakeFetch(files: Record<string, string | Uint8Array>, down: string[] = []) {
	return vi.fn(async (input: RequestInfo | URL) => {
		const url = String(input);
		if (down.some((host) => url.startsWith(host))) throw new Error('connection refused');
		const body = files[url];
		if (body === undefined) return new Response('not found', { status: 404 });
		return new Response(body as BodyInit);
	}) as unknown as typeof fetch & ReturnType<typeof vi.fn>;
}

const sources = (over: Partial<Sources> = {}): Sources => ({
	scenarios: ['https://remote', '/registry'],
	apps: ['https://remote', '/registry'],
	...over
});

describe('sourcesFromEnv', () => {
	it('defaults to the local registry', () => {
		expect(sourcesFromEnv({}, '/registry')).toEqual({
			scenarios: ['/registry'],
			apps: ['/registry'],
			progress: undefined,
			scores: undefined
		});
	});

	it('reads comma-separated lists and the reserved services', () => {
		const s = sourcesFromEnv(
			{
				VITE_SCENARIO_SOURCES: ' https://a/ , /registry',
				VITE_APP_REGISTRIES: 'https://apps',
				VITE_PROGRESS_URL: 'https://progress',
				VITE_SCORES_URL: ' '
			},
			'/base/registry/'
		);
		expect(s).toEqual({
			scenarios: ['https://a', '/registry'],
			apps: ['https://apps'],
			progress: 'https://progress',
			scores: undefined
		});
	});
});

describe('parseAppRef', () => {
	it('splits name@version', () => {
		expect(parseAppRef('db-client@0.1.0')).toEqual({ name: 'db-client', version: '0.1.0' });
	});

	it('rejects anything that is not a safe reference', () => {
		for (const bad of ['db-client', '@1', 'a@', 'a@b@c', '../x@1', 'a@../1', 'a b@1']) {
			expect(() => parseAppRef(bad), bad).toThrow(/Invalid app reference/);
		}
	});
});

describe('Registry', () => {
	it('fetches a scenario from the first source that has it', async () => {
		const fetch = fakeFetch({ '/registry/scenarios/case/scenario.json': '{"id":"case"}' });
		expect(await new Registry(sources(), fetch).fetchScenario('case')).toEqual({
			json: '{"id":"case"}',
			base: '/registry/scenarios/case/'
		});
		expect(fetch.mock.calls.map((c) => String(c[0]))).toEqual([
			'https://remote/scenarios/case/scenario.json',
			'/registry/scenarios/case/scenario.json'
		]);
	});

	it('fetches manifests from the folder of the source that served the scenario', async () => {
		const fetch = fakeFetch({
			'https://remote/scenarios/case/scenario.json': '{}',
			'https://remote/scenarios/case/manifests/a.yaml': 'A',
			'https://remote/scenarios/case/b.yaml': 'B',
			'/registry/scenarios/case/manifests/a.yaml': 'LOCAL'
		});
		const registry = new Registry(sources(), fetch);
		const source = await registry.fetchScenario('case');
		expect(await registry.fetchManifests(source, ['manifests/a.yaml', './b.yaml'])).toEqual({
			'manifests/a.yaml': 'A',
			'./b.yaml': 'B'
		});
		await expect(registry.fetchManifests(source, ['gone.yaml'])).rejects.toThrow(
			'Manifest gone.yaml could not be fetched; tried https://remote/scenarios/case/gone.yaml (404)'
		);
	});

	it('names every place it tried when a scenario is missing', async () => {
		const registry = new Registry(sources(), fakeFetch({}, ['https://remote']));
		await expect(registry.fetchScenario('nope')).rejects.toThrow(
			'Case "nope" could not be fetched; tried https://remote/scenarios/nope/scenario.json (connection refused), /registry/scenarios/nope/scenario.json (404)'
		);
		await expect(registry.fetchScenario('../x')).rejects.toThrow(/Invalid case id/);
	});

	it('merges scenario indexes, first source wins, and tolerates one being down', async () => {
		const fetch = fakeFetch({
			'https://remote/scenarios/index.json': JSON.stringify([{ id: 'b', title: 'Remote B', objective: 'o' }]),
			'/registry/scenarios/index.json': JSON.stringify([
				{ id: 'a', title: 'A', objective: 'o' },
				{ id: 'b', title: 'Local B', objective: 'o' },
				{ id: '../bad', title: 'x' }
			])
		});
		const list = await new Registry(sources(), fetch).listScenarios();
		expect(list.map((s) => [s.id, s.title])).toEqual([
			['b', 'Remote B'],
			['a', 'A']
		]);

		const half = await new Registry(sources(), fakeFetch({ '/registry/scenarios/index.json': '[]' }, ['https://remote'])).listScenarios();
		expect(half).toEqual([]);
		await expect(new Registry(sources(), fakeFetch({})).listScenarios()).rejects.toThrow(/No scenario index/);
	});

	it('fetches apps with fallback, in parallel, and caches them by reference', async () => {
		const fetch = fakeFetch({
			'https://remote/apps/db-client/0.1.0.wasm': new Uint8Array([1, 2]),
			'/registry/apps/nginx/0.1.0.wasm': new Uint8Array([3])
		});
		const registry = new Registry(sources(), fetch);
		const apps = await registry.fetchApps(['db-client@0.1.0', 'nginx@0.1.0']);
		expect([...apps.get('db-client@0.1.0')!]).toEqual([1, 2]);
		expect([...apps.get('nginx@0.1.0')!]).toEqual([3]);
		const calls = fetch.mock.calls.length;
		await registry.fetchApps(['db-client@0.1.0']);
		expect(fetch.mock.calls.length).toBe(calls);
	});

	it('does not cache a failed app fetch', async () => {
		const files: Record<string, Uint8Array> = {};
		const registry = new Registry(sources({ apps: ['/registry'] }), fakeFetch(files));
		await expect(registry.fetchApps(['db-client@9.9.9'])).rejects.toThrow(
			'App db-client@9.9.9 could not be fetched; tried /registry/apps/db-client/9.9.9.wasm (404)'
		);
		files['/registry/apps/db-client/9.9.9.wasm'] = new Uint8Array([7]);
		expect([...(await registry.fetchApps(['db-client@9.9.9'])).get('db-client@9.9.9')!]).toEqual([7]);
	});
});

describe('pickCase', () => {
	const files = {
		'/registry/scenarios/index.json': JSON.stringify([{ id: 'first' }, { id: 'second' }]),
		'/registry/scenarios/first/scenario.json': 'FIRST',
		'/registry/scenarios/second/scenario.json': 'SECOND',
		'http://dev/cases/mine.json': 'MINE'
	};
	const pick = (query: string, dev = false) => {
		const fetch = fakeFetch(files);
		return pickCase(new Registry(sources({ scenarios: ['/registry'] }), fetch), new URLSearchParams(query), {
			dev,
			fetchFn: fetch
		});
	};

	it('plays ?case=, else the first listed case', async () => {
		expect(await pick('')).toEqual({ json: 'FIRST', base: '/registry/scenarios/first/' });
		expect((await pick('case=second')).json).toBe('SECOND');
		await expect(pick('case=nope')).rejects.toThrow(/Case "nope" could not be fetched/);
	});

	it('plays ?scenarioUrl= only in dev builds', async () => {
		expect(await pick('scenarioUrl=http://dev/cases/mine.json', true)).toEqual({ json: 'MINE', base: 'http://dev/cases/' });
		expect((await pick('scenarioUrl=http://dev/cases/mine.json', false)).json).toBe('FIRST');
		await expect(pick('scenarioUrl=http://dev/gone.json', true)).rejects.toThrow(/gone.json could not be fetched \(404\)/);
	});

	it('says so when nothing is published', async () => {
		const empty = fakeFetch({ '/registry/scenarios/index.json': '[]' });
		await expect(
			pickCase(new Registry(sources({ scenarios: ['/registry'] }), empty), new URLSearchParams(), { dev: false })
		).rejects.toThrow('No cases are published yet');
	});
});
