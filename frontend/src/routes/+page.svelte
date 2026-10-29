<script lang="ts">
	import { onMount } from 'svelte';
	import { asset } from '$app/paths';

	import CaseFile from '#lib/components/CaseFile.svelte';
	import CaseReport from '#lib/components/CaseReport.svelte';
	import Objective from '#lib/components/Objective.svelte';
	import Clock from '#lib/components/Clock.svelte';
	import Hints from '#lib/components/Hints.svelte';
	import RestartCase from '#lib/components/RestartCase.svelte';
	import Sound from '#lib/components/Sound.svelte';
	import { armSound } from '#lib/audio/index.js';
	import { caseStatus, engineState, recordHint, resetEngine, simTime, startClock } from '#lib/engine.js';
	import { pickCase, Registry, sourcesFromEnv } from '#lib/registry.js';
	import { caseNumber, parseScenarioMeta, type ScenarioMeta } from '#lib/scenario.js';
	import Terminal from '#lib/terminal/Terminal.svelte';

	/** Scenarios and apps: the local registry unless the build points elsewhere (docs/registry.md). */
	// `asset` is typed to files that exist at type-generation time; it only prefixes the base path.
	const localRegistry = asset('registry' as Parameters<typeof asset>[0]);
	const registry = new Registry(sourcesFromEnv(import.meta.env, localRegistry));

	let scenarioJson = $state<string | null>(null);
	let meta = $state<ScenarioMeta | null>(null);
	let loadError = $state<string | null>(null);

	let hintsRevealed = $state(0);
	/** Cluster time (ms) when the player started this attempt, for the "on the case" timer. */
	let startedAt = $state<number | null>(null);
	let sidebarOpen = $state(true);
	let reportOpen = $state(false);
	/** The report pops up once per attempt, the moment the case closes. */
	let reportShown = $state(false);

	onMount(() => {
		// On small screens the case file starts collapsed so the terminal is in reach.
		sidebarOpen = window.matchMedia('(min-width: 1024px)').matches;
		const disarmSound = armSound();
		let stopClock = () => {};
		let closed = false;
		void (async () => {
			try {
				const json = await pickCase(registry, new URLSearchParams(window.location.search), {
					dev: import.meta.env.DEV
				});
				const parsed = parseScenarioMeta(json);
				if (closed) return;
				scenarioJson = json;
				meta = parsed;
				await resetEngine(json, registry);
				if (!closed) stopClock = startClock();
			} catch (e) {
				loadError = e instanceof Error ? e.message : String(e);
			}
		})();
		return () => {
			closed = true;
			stopClock();
			disarmSound();
		};
	});

	$effect(() => {
		if ($engineState.status === 'ready' && startedAt === null && $simTime) {
			startedAt = Date.parse($simTime);
		}
	});

	$effect(() => {
		if ($caseStatus?.solved && !reportShown) {
			reportShown = true;
			reportOpen = true;
		}
	});

	async function restart() {
		hintsRevealed = 0;
		startedAt = null;
		reportShown = false;
		reportOpen = false;
		if (scenarioJson) await resetEngine(scenarioJson, registry);
	}
</script>

{#if loadError}
	<main class="grid min-h-dvh place-items-center p-6">
		<div class="paper max-w-lg rounded-sm p-6" role="alert">
			<h1 class="font-typewriter text-xl">This case file is damaged</h1>
			<p class="mt-2 text-sm break-words">{loadError}</p>
		</div>
	</main>
{:else if !meta}
	<main class="grid min-h-dvh place-items-center p-6">
		<p class="font-typewriter text-lg" aria-live="polite">Pulling the case file…</p>
	</main>
{:else}
	<div class="min-h-dvh p-3 sm:p-5 lg:h-dvh">
		<div class="mx-auto grid max-w-[112rem] gap-4 lg:h-full lg:grid-cols-[24rem_minmax(0,1fr)]">
			<aside class="min-h-0">
				<button
					type="button"
					class="mb-2 w-full rounded border border-brass/60 px-3 py-2 font-typewriter text-sm text-brass lg:hidden"
					aria-expanded={sidebarOpen}
					aria-controls="case-file"
					onclick={() => (sidebarOpen = !sidebarOpen)}
				>
					{sidebarOpen ? 'Hide' : 'Show'} case file
				</button>

				<div
					id="case-file"
					class="paper space-y-5 overflow-y-auto rounded-sm p-5 lg:block lg:h-full {sidebarOpen
						? 'block'
						: 'hidden'}"
				>
					<CaseFile
						number={caseNumber(meta.id)}
						title={meta.title}
						briefing={meta.briefing}
						outcome={$caseStatus?.outcome ?? null}
					/>
					<Objective objective={meta.objective} status={$caseStatus} />
					{#if $caseStatus?.solved}
						<button
							type="button"
							class="w-full rounded border border-ok bg-ok px-3 py-1.5 font-typewriter text-sm text-paper"
							onclick={() => (reportOpen = true)}
						>
							Open the case report
						</button>
					{/if}
					<Clock now={$simTime} since={startedAt} />
					<Sound />
					<Hints
						hints={meta.hints}
						revealed={hintsRevealed}
						onrequest={() => {
							recordHint(hintsRevealed);
							hintsRevealed = Math.min(hintsRevealed + 1, meta?.hints.length ?? 0);
						}}
					/>
					<RestartCase onrestart={restart} />
				</div>
			</aside>

			<section
				class="flex min-h-[70vh] min-w-0 flex-col overflow-hidden rounded-lg border-4 border-brass/70 bg-screen shadow-[inset_0_0_60px_rgb(0_0_0/0.85),0_12px_30px_rgb(0_0_0/0.6)] lg:min-h-0"
				aria-label="Terminal"
			>
				<div
					class="flex items-center justify-between border-b border-brass/30 px-4 py-1.5 font-typewriter text-xs tracking-widest text-brass uppercase"
				>
					<span>Terminal 01 · ops bastion</span>
					<span aria-live="polite">{$engineState.status === 'ready' ? 'Connected' : $engineState.status}</span>
				</div>

				<div class="min-h-0 flex-1 p-3">
					{#if $engineState.status === 'ready'}
						<Terminal scenarioId={meta.id} title={meta.title} />
					{:else if $engineState.status === 'error'}
						<div class="p-4 text-phosphor" role="alert">
							<p class="font-typewriter text-lg text-amber">The line went dead.</p>
							<p class="mt-2 text-sm">{$engineState.error}</p>
							<button
								type="button"
								class="mt-4 rounded border border-amber px-3 py-1.5 font-typewriter text-amber hover:bg-amber/10"
								onclick={restart}
							>
								Try again
							</button>
						</div>
					{:else}
						<p class="p-4 font-typewriter text-amber">Opening the case file…</p>
					{/if}
				</div>
			</section>
		</div>
	</div>

	<CaseReport
		title={meta.title}
		status={$caseStatus}
		hintsUsed={hintsRevealed}
		open={reportOpen}
		onclose={() => (reportOpen = false)}
		onrestart={restart}
	/>
{/if}
