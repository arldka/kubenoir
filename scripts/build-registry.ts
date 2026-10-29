// Builds the local registry into frontend/static/registry, in the same layout a remote registry
// serves (docs/registry.md):
//
//   apps/index.json                 {"db-client": ["0.1.0"], ...}
//   apps/{name}/{version}.wasm      from apps/target/wasm32-wasip1/release (run `make apps` first)
//   scenarios/index.json            [{"id", "title", "objective", "chapter"?}], in campaign order
//   scenarios/{id}/scenario.json    from scenarios/{id}/scenario.json
//   scenarios/{id}/{file}           every file its cluster.manifests and files list, at the same path
//
// Scenarios without a `briefing` are engine test fixtures and are skipped. Anything else that is
// wrong (bad meta, an id that does not match its folder, an app not in the registry) fails.
//
// Usage: bun scripts/build-registry.ts

import { cpSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';

import { parseScenarioMeta } from '../frontend/src/lib/scenario.ts';

const root = resolve(import.meta.dir, '..');
const out = join(root, 'frontend/static/registry');
const built = join(root, 'apps/target/wasm32-wasip1/release');

function fail(message: string): never {
	console.error(`build-registry: ${message}`);
	process.exit(1);
}

/** A `key = "value"` line from a Cargo manifest (the first one, i.e. in `[package]`). */
function manifestField(manifest: string, key: string): string | undefined {
	const line = manifest.split('\n').find((l) => l.split('=')[0]?.trim() === key);
	return line?.split('=').slice(1).join('=').trim().replace(/^"|"$/g, '');
}

rmSync(out, { recursive: true, force: true });

// Apps: every workspace member with a src/main.rs.
const apps: Record<string, string[]> = {};
for (const dir of readdirSync(join(root, 'apps')).sort()) {
	const crate = join(root, 'apps', dir);
	if (!existsSync(join(crate, 'src/main.rs'))) continue;
	const manifest = readFileSync(join(crate, 'Cargo.toml'), 'utf8');
	const name = manifestField(manifest, 'name');
	const version = manifestField(manifest, 'version');
	if (!name || !version) fail(`apps/${dir}/Cargo.toml needs a name and a version`);
	const wasm = join(built, `${name}.wasm`);
	if (!existsSync(wasm)) fail(`${wasm} is missing; run \`make apps\` first`);
	mkdirSync(join(out, 'apps', name), { recursive: true });
	cpSync(wasm, join(out, 'apps', name, `${version}.wasm`));
	apps[name] = [version];
}
mkdirSync(join(out, 'apps'), { recursive: true });
writeFileSync(join(out, 'apps/index.json'), JSON.stringify(apps, null, '\t') + '\n');

// Scenarios.
const index: { id: string; title: string; objective: string; chapter?: number }[] = [];
mkdirSync(join(out, 'scenarios'), { recursive: true });
for (const dir of readdirSync(join(root, 'scenarios')).sort()) {
	const file = join(root, 'scenarios', dir, 'scenario.json');
	if (!existsSync(file)) continue;
	const json = readFileSync(file, 'utf8');
	const data = JSON.parse(json) as {
		briefing?: unknown;
		chapter?: unknown;
		images?: Record<string, { app?: string }>;
		cluster?: { manifests?: unknown };
		files?: unknown;
	};
	if (data.briefing === undefined) {
		console.log(`build-registry: skipped scenarios/${dir} (no briefing: a test fixture, not a case)`);
		continue;
	}
	let meta;
	try {
		meta = parseScenarioMeta(json);
	} catch (e) {
		fail(`scenarios/${dir}: ${e instanceof Error ? e.message : String(e)}`);
	}
	if (meta.id !== dir) fail(`scenarios/${dir}: its id is "${meta.id}"; the folder must be named after the id`);
	for (const [image, spec] of Object.entries(data.images ?? {})) {
		if (!spec.app) continue;
		const [name, version] = spec.app.split('@');
		if (!version || !apps[name!]?.includes(version)) {
			fail(`scenarios/${dir}: image ${image} runs ${spec.app}, which is not in the registry`);
		}
	}
	const manifests = data.cluster?.manifests ?? [];
	if (!Array.isArray(manifests)) fail(`scenarios/${dir}: cluster.manifests must be a list of paths`);
	const extraFiles = data.files ?? [];
	if (!Array.isArray(extraFiles)) fail(`scenarios/${dir}: files must be a list of paths`);
	const published = join(out, 'scenarios', meta.id);
	mkdirSync(published, { recursive: true });
	cpSync(file, join(published, 'scenario.json'));
	for (const ref of [...manifests, ...extraFiles]) {
		// The engine's scenario-check has already vetted these; refuse anything unsafe regardless.
		const path = typeof ref === 'string' ? ref.replace(/^(\.\/)+/, '') : '';
		if (!/^[A-Za-z0-9._-]+(\/[A-Za-z0-9._-]+)*$/.test(path) || path.split('/').some((s) => s === '.' || s === '..')) {
			fail(`scenarios/${dir}: file reference ${JSON.stringify(ref)} is not a safe relative path`);
		}
		const source = join(root, 'scenarios', dir, path);
		if (!existsSync(source)) fail(`scenarios/${dir}: ${path} (listed in the scenario) does not exist`);
		mkdirSync(dirname(join(published, path)), { recursive: true });
		cpSync(source, join(published, path));
	}
	if (data.chapter !== undefined && !(Number.isInteger(data.chapter) && (data.chapter as number) >= 1)) {
		fail(`scenarios/${dir}: chapter must be a whole number from 1`);
	}
	index.push({
		id: meta.id,
		title: meta.title,
		objective: meta.objective,
		...(data.chapter === undefined ? {} : { chapter: data.chapter as number })
	});
}
// Campaign order: by chapter, then by id (cases without a chapter come last).
index.sort((a, b) => (a.chapter ?? Infinity) - (b.chapter ?? Infinity) || a.id.localeCompare(b.id));
writeFileSync(join(out, 'scenarios/index.json'), JSON.stringify(index, null, '\t') + '\n');

console.log(
	`build-registry: ${Object.keys(apps).length} apps, ${index.length} scenarios -> ${out.replace(root + '/', '')}`
);
