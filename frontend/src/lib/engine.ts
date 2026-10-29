import { writable, type Readable } from 'svelte/store';
import type { Engine } from './wasm/engine';
import { parseStatus, type CaseStatus } from './caseStatus';
import type { Registry, ScenarioSource } from './registry';
import type { Completion } from './terminal/lineEditor';

type EngineModule = typeof import('./wasm/engine');

export type EngineState =
	| { status: 'loading' }
	| { status: 'ready' }
	| { status: 'error'; error: string };

const state = writable<EngineState>({ status: 'loading' });
const clock = writable<string | null>(null);
const status = writable<CaseStatus | null>(null);

/** Async wrapper store: reports WASM load / scenario status. Browser-only, never runs during SSR. */
export const engineState: Readable<EngineState> = { subscribe: state.subscribe };

/** The cluster's simulated time (RFC 3339), or null before a scenario is loaded. */
export const simTime: Readable<string | null> = { subscribe: clock.subscribe };

/** One input to the simulation. Replaying these in order reproduces the cluster exactly. */
export type TranscriptEntry =
	| { kind: 'eval'; command: string }
	| { kind: 'advance'; seconds: number }
	| { kind: 'write'; path: string; text: string }
	| { kind: 'hint'; index: number };

/** How the case stands (findings so far, and the verdict once it is solved). */
export const caseStatus: Readable<CaseStatus | null> = { subscribe: status.subscribe };

let lastStatusJson = '';
/** Publish the judge's view of the case, only when it actually changed. */
function refreshStatus(): void {
	if (!current) return;
	const raw = current.status();
	if (raw === lastStatusJson) return;
	lastStatusJson = raw;
	status.set(parseStatus(raw));
}

let transcript: TranscriptEntry[] = [];

/** Note that the player asked for a hint; hints are part of a session's record. */
export function recordHint(index: number): void {
	transcript.push({ kind: 'hint', index });
}

/** Everything the player did since the scenario was loaded, in order (for score verification). */
export function getTranscript(): readonly TranscriptEntry[] {
	return transcript;
}

let modulePromise: Promise<EngineModule> | undefined;
// The Engine is a handle to Rust-owned memory in WASM; this module owns the single instance.
let current: Engine | undefined;

/** Load and initialize the WASM module once; later callers share the same promise. */
function loadModule(): Promise<EngineModule> {
	modulePromise ??= (async () => {
		const mod = await import('./wasm/engine');
		await mod.default();
		return mod;
	})();
	return modulePromise;
}

/**
 * Build a cluster from a scenario, replacing (and freeing) any previous one. The manifests it
 * references and the apps its images run are fetched from `registry` first (apps are cached
 * there across restarts).
 */
export async function resetEngine(source: ScenarioSource, registry: Registry): Promise<void> {
	state.set({ status: 'loading' });
	try {
		const mod = await loadModule();
		const scenarioJson = source.json;
		const [manifests, fetched] = await Promise.all([
			registry.fetchManifests(source, mod.requiredFiles(scenarioJson)),
			registry.fetchApps(mod.requiredApps(scenarioJson))
		]);
		// The AppSet is Rust memory; the Engine keeps its own copy, so free the set either way.
		const apps = new mod.AppSet();
		let next: Engine;
		try {
			for (const [ref, wasm] of fetched) apps.add(ref, wasm);
			next = new mod.Engine(scenarioJson, JSON.stringify(manifests), apps);
		} finally {
			apps.free();
		}
		current?.free();
		current = next;
		transcript = [];
		lastStatusJson = '';
		clock.set(next.now());
		refreshStatus();
		state.set({ status: 'ready' });
	} catch (e) {
		state.set({ status: 'error', error: e instanceof Error ? e.message : String(e) });
	}
}

/** Evaluate a terminal command line against the current cluster. */
export async function evalCommand(cmd: string): Promise<string> {
	if (!current) throw new Error('No scenario loaded');
	transcript.push({ kind: 'eval', command: cmd });
	const output = current.eval(cmd);
	refreshStatus();
	return output;
}

/** The file `vim` was asked to open by the last command, once; undefined if it did not. */
export function takeEditorRequest(): string | undefined {
	return current?.takeEditorRequest() ?? undefined;
}

/** The text of one of the terminal's files, or undefined if there is no such file. */
export function readFile(path: string): string | undefined {
	return current?.readFile(path) ?? undefined;
}

/** The editor saved `text` to `path`: part of the session, since `kubectl apply -f` reads it. */
export function writeFile(path: string, text: string): void {
	if (!current) return;
	transcript.push({ kind: 'write', path, text });
	current.writeFile(path, text);
}

/** What could complete the last word of `linePrefix`; empty if there is no cluster or no match. */
export function complete(linePrefix: string): Completion {
	const none: Completion = { start: linePrefix.length, candidates: [] };
	if (!current) return none;
	try {
		const parsed: unknown = JSON.parse(current.complete(linePrefix));
		const { start, candidates } = parsed as Partial<Completion>;
		if (typeof start === 'number' && Array.isArray(candidates) && candidates.every((c) => typeof c === 'string')) {
			return { start, candidates };
		}
	} catch {
		// fall through to "nothing to complete"
	}
	return none;
}

/**
 * Let `seconds` of simulated time pass (the engine caps a single call at one day). The cluster
 * only changes through `evalCommand` and `advance`, so replaying the same calls replays it.
 */
export function advance(seconds: number): void {
	if (!current || seconds < 1) return;
	const whole = Math.floor(seconds);
	transcript.push({ kind: 'advance', seconds: whole });
	current.advance(whole);
	clock.set(current.now());
	refreshStatus();
}

/**
 * Keep simulated time in step with real time: call `advance` with the whole seconds that have
 * passed (a throttled background tab catches up in one larger step). Returns a stop function.
 */
export function startClock(): () => void {
	let last = performance.now();
	let carry = 0;
	const id = setInterval(() => {
		const now = performance.now();
		carry += (now - last) / 1000;
		last = now;
		const whole = Math.floor(carry);
		if (whole > 0) {
			carry -= whole;
			advance(whole);
		}
	}, 250);
	return () => clearInterval(id);
}
