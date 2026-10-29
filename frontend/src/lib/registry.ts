/**
 * Where scenarios and apps come from. The local registry (`/registry`, built by `make registry`)
 * and any remote backend serve the same layout (docs/registry.md), so one client handles both:
 *
 *   {scenarioSource}/scenarios/index.json            [{ id, title, objective }]
 *   {scenarioSource}/scenarios/{id}/scenario.json
 *   {scenarioSource}/scenarios/{id}/{manifest}       the files its cluster.manifests lists
 *   {appRegistry}/apps/{name}/{version}.wasm
 *
 * Each list of sources is tried in order; the first that has the item wins.
 */

export interface Sources {
	/** Base URLs serving `scenarios/`. */
	scenarios: string[];
	/** Base URLs serving `apps/`. */
	apps: string[];
	/** Reserved for the progress service (not used yet). */
	progress?: string;
	/** Reserved for the scores/leaderboard service (not used yet). */
	scores?: string;
}

export interface ScenarioSummary {
	id: string;
	title: string;
	objective: string;
}

/** A scenario file and the folder its manifest references are relative to. */
export interface ScenarioSource {
	json: string;
	/** URL of the scenario's folder, ending in `/`. */
	base: string;
}

/** The folder of a file URL: everything up to and including the last `/`. */
function folderOf(url: string): string {
	return url.slice(0, url.lastIndexOf('/') + 1);
}

type Env = Record<string, string | boolean | undefined>;

function list(value: unknown): string[] {
	return typeof value === 'string'
		? value
				.split(',')
				.map((s) => s.trim().replace(/\/+$/, ''))
				.filter(Boolean)
		: [];
}

/**
 * Sources from build-time env (`VITE_SCENARIO_SOURCES`, `VITE_APP_REGISTRIES`: comma-separated
 * base URLs; `VITE_PROGRESS_URL`, `VITE_SCORES_URL`). Unset lists default to the local registry
 * under `localBase` (the app's base path + `/registry`).
 */
export function sourcesFromEnv(env: Env, localBase: string): Sources {
	const local = [localBase.replace(/\/+$/, '')];
	const scenarios = list(env.VITE_SCENARIO_SOURCES);
	const apps = list(env.VITE_APP_REGISTRIES);
	const optional = (value: unknown) => (typeof value === 'string' && value.trim() ? value.trim() : undefined);
	return {
		scenarios: scenarios.length ? scenarios : local,
		apps: apps.length ? apps : local,
		progress: optional(env.VITE_PROGRESS_URL),
		scores: optional(env.VITE_SCORES_URL)
	};
}

/** Ids, app names and versions become URL path segments: keep them to safe characters. */
const SEGMENT = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;

/** Split `name@version`, or throw if it is not a safe app reference. */
export function parseAppRef(ref: string): { name: string; version: string } {
	const [name, version, ...rest] = ref.split('@');
	if (!name || !version || rest.length || !SEGMENT.test(name) || !SEGMENT.test(version)) {
		throw new Error(`Invalid app reference "${ref}" (expected name@version)`);
	}
	return { name, version };
}

export class Registry {
	readonly #sources: Sources;
	readonly #fetch: typeof fetch;
	/** App bytes by reference. References are immutable, so a fetched app never goes stale. */
	readonly #apps = new Map<string, Promise<Uint8Array>>();

	constructor(sources: Sources, fetchFn: typeof fetch = fetch) {
		this.#sources = sources;
		this.#fetch = fetchFn;
	}

	/**
	 * GET `path` from each base in turn; the first OK response wins. A 404 moves on to the next
	 * base, as does an unreachable one. Throws naming every place tried.
	 */
	async #first(bases: string[], path: string, what: string): Promise<{ response: Response; url: string }> {
		const tried: string[] = [];
		for (const base of bases) {
			const url = `${base}/${path}`;
			try {
				const response = await this.#fetch(url);
				if (response.ok) return { response, url };
				tried.push(`${url} (${response.status})`);
			} catch (e) {
				tried.push(`${url} (${e instanceof Error ? e.message : String(e)})`);
			}
		}
		throw new Error(`${what} could not be fetched; tried ${tried.join(', ')}`);
	}

	/** Every listed scenario across the sources; an id listed twice is taken from the first. */
	async listScenarios(): Promise<ScenarioSummary[]> {
		const seen = new Map<string, ScenarioSummary>();
		const failures: string[] = [];
		for (const base of this.#sources.scenarios) {
			try {
				const response = await this.#fetch(`${base}/scenarios/index.json`);
				if (!response.ok) throw new Error(String(response.status));
				const entries: unknown = await response.json();
				if (!Array.isArray(entries)) throw new Error('index is not a list');
				for (const entry of entries as Partial<ScenarioSummary>[]) {
					if (typeof entry?.id === 'string' && SEGMENT.test(entry.id) && !seen.has(entry.id)) {
						seen.set(entry.id, { id: entry.id, title: String(entry.title ?? entry.id), objective: String(entry.objective ?? '') });
					}
				}
			} catch (e) {
				failures.push(`${base} (${e instanceof Error ? e.message : String(e)})`);
			}
		}
		if (failures.length === this.#sources.scenarios.length) {
			throw new Error(`No scenario index could be fetched; tried ${failures.join(', ')}`);
		}
		return [...seen.values()];
	}

	/** The scenario JSON for `id`, and where its manifests live (the source that served it). */
	async fetchScenario(id: string): Promise<ScenarioSource> {
		if (!SEGMENT.test(id)) throw new Error(`Invalid case id "${id}"`);
		const { response, url } = await this.#first(
			this.#sources.scenarios,
			`scenarios/${id}/scenario.json`,
			`Case "${id}"`
		);
		return { json: await response.text(), base: folderOf(url) };
	}

	/**
	 * The text of each manifest a scenario references (`refs`, already checked by the engine's
	 * `requiredFiles` to be safe relative paths), fetched in parallel from the scenario's folder.
	 */
	async fetchManifests(source: ScenarioSource, refs: string[]): Promise<Record<string, string>> {
		const entries = await Promise.all(
			refs.map(async (ref) => {
				const url = source.base + ref.replace(/^(\.\/)+/, '');
				const response = await this.#fetch(url);
				if (!response.ok) throw new Error(`Manifest ${ref} could not be fetched; tried ${url} (${response.status})`);
				return [ref, await response.text()] as const;
			})
		);
		return Object.fromEntries(entries);
	}

	/** The bytes of each app reference, fetched in parallel and cached. */
	async fetchApps(refs: string[]): Promise<Map<string, Uint8Array>> {
		const entries = await Promise.all(refs.map(async (ref) => [ref, await this.#app(ref)] as const));
		return new Map(entries);
	}

	#app(ref: string): Promise<Uint8Array> {
		let pending = this.#apps.get(ref);
		if (!pending) {
			const { name, version } = parseAppRef(ref);
			pending = this.#first(this.#sources.apps, `apps/${name}/${version}.wasm`, `App ${ref}`).then(
				async ({ response }) => new Uint8Array(await response.arrayBuffer())
			);
			// A failure is not cached, so a later attempt (e.g. after a backend recovers) retries.
			pending.catch(() => this.#apps.delete(ref));
			this.#apps.set(ref, pending);
		}
		return pending;
	}
}

/**
 * The scenario the page should play: `?scenarioUrl=` (any URL; dev builds only, for authors
 * trying a file), else `?case={id}`, else the first listed case.
 */
export async function pickCase(
	registry: Registry,
	params: URLSearchParams,
	options: { dev: boolean; fetchFn?: typeof fetch }
): Promise<ScenarioSource> {
	const url = params.get('scenarioUrl');
	if (url && options.dev) {
		const response = await (options.fetchFn ?? fetch)(url);
		if (!response.ok) throw new Error(`${url} could not be fetched (${response.status})`);
		return { json: await response.text(), base: folderOf(url) };
	}
	const id = params.get('case') ?? (await registry.listScenarios())[0]?.id;
	if (!id) throw new Error('No cases are published yet');
	return registry.fetchScenario(id);
}
