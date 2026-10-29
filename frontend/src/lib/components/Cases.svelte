<script lang="ts">
	import type { ScenarioSummary } from '../registry.js';
	import { caseState } from '../progress.js';

	let {
		cases,
		completed,
		currentId,
		href
	}: {
		cases: ScenarioSummary[];
		completed: ReadonlySet<string>;
		currentId: string;
		/** Where playing `id` goes. */
		href: (id: string) => string;
	} = $props();

	const states = $derived(caseState(cases, completed));
</script>

{#if cases.length > 1}
	<section aria-labelledby="cases-heading">
		<h2 id="cases-heading" class="font-typewriter text-xs tracking-widest text-ink-soft uppercase">
			The case load
		</h2>
		<ol class="mt-2 space-y-1 text-sm" data-testid="case-list">
			{#each cases as c, i (c.id)}
				{@const state = states[i].state}
				<li class="flex items-baseline gap-2" data-state={state}>
					<span aria-hidden="true" class="w-4 text-center">{state === 'done' ? '✓' : state === 'locked' ? '·' : '▸'}</span>
					{#if state === 'locked'}
						<span class="text-ink-soft"><span class="sr-only">Locked: </span>{c.title}</span>
					{:else}
						<a
							class="underline-offset-2 hover:underline {c.id === currentId ? 'font-bold' : ''}"
							href={href(c.id)}
							aria-current={c.id === currentId ? 'page' : undefined}>{c.title}</a
						>
					{/if}
				</li>
			{/each}
		</ol>
		<p class="mt-2 text-xs text-ink-soft">Each case opens when the one before it is solved.</p>
	</section>
{/if}
