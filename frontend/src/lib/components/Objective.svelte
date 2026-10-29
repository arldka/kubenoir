<script lang="ts">
	import type { CaseStatus } from '../caseStatus.js';

	let { objective, status }: { objective: string; status: CaseStatus | null } = $props();

	const closed = $derived(status?.solved ?? false);
	const found = $derived(status?.findings.found ?? []);
	const total = $derived(status?.findings.total ?? 0);
	const undiscovered = $derived(Math.max(0, total - found.length));
</script>

<section aria-labelledby="objective-heading" class="space-y-3">
	<div>
		<div class="flex items-baseline justify-between">
			<h2 id="objective-heading" class="font-typewriter text-xs tracking-widest text-ink-soft uppercase">
				Objective
			</h2>
			<span
				class="font-typewriter text-xs tracking-widest uppercase {closed ? 'text-ok' : 'text-stamp'}"
				data-testid="objective-state"
			>
				{closed ? 'Closed' : 'Open'}
			</span>
		</div>
		<p class="mt-1 flex gap-2 text-sm leading-relaxed">
			<span aria-hidden="true" class="font-bold">{closed ? '☑' : '☐'}</span>
			<span>{objective}</span>
		</p>
	</div>

	{#if total > 0}
		<div>
			<h3 class="font-typewriter text-xs tracking-widest text-ink-soft uppercase">Hidden findings</h3>
			<p class="mt-1 text-sm" data-testid="findings-counter">
				<span aria-hidden="true" class="tracking-widest">
					{'●'.repeat(found.length)}{'○'.repeat(undiscovered)}
				</span>
				<span class="ml-1 font-bold">{found.length} / {total}</span>
				<span class="sr-only">findings uncovered</span>
			</p>
			<ul class="mt-2 space-y-2 text-sm leading-relaxed">
				{#each found as finding (finding.id)}
					<li class="border-l-2 border-ok/60 pl-3">
						<span class="font-typewriter text-xs text-ok">Finding.</span>
						{finding.clue}
					</li>
				{/each}
				{#each { length: undiscovered } as _, i (i)}
					<li class="border-l-2 border-ink/20 pl-3 text-ink-soft">
						<span aria-hidden="true">???</span>
						<span class="sr-only">Not yet discovered</span>
					</li>
				{/each}
			</ul>
		</div>
	{/if}
</section>
