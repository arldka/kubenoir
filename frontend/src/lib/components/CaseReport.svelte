<script lang="ts">
	import { formatDuration, type CaseStatus } from '../caseStatus.js';

	let {
		title,
		status,
		hintsUsed,
		open,
		onclose,
		onrestart
	}: {
		title: string;
		status: CaseStatus | null;
		hintsUsed: number;
		open: boolean;
		onclose: () => void;
		onrestart: () => void;
	} = $props();

	let dialog: HTMLDialogElement | undefined = $state();

	// The native dialog gives us the backdrop, focus trapping and Escape handling.
	$effect(() => {
		if (!dialog) return;
		if (open && !dialog.open) dialog.showModal();
		if (!open && dialog.open) dialog.close();
	});

	const report = $derived(status?.report ?? null);
	const costly = $derived(status?.outcome === 'closedWithCost');
	const timeUp = $derived(status?.outcome === 'timeUp');
	const found = $derived(status?.findings.found ?? []);
	const total = $derived(status?.findings.total ?? 0);
</script>

<dialog
	bind:this={dialog}
	{onclose}
	aria-labelledby="report-title"
	class="paper m-auto max-h-[90dvh] w-[calc(100%-2rem)] max-w-xl overflow-y-auto rounded-sm p-6"
>
	{#if status && report}
		<div class="flex items-start justify-between gap-3">
			<p class="font-typewriter text-xs tracking-widest text-ink-soft uppercase">Case report</p>
			<span class="stamp {costly || timeUp ? '' : 'stamp-ok'}" data-testid="report-outcome">
				{timeUp ? "Time's up" : costly ? 'Closed at a cost' : 'Case closed'}
			</span>
		</div>

		<h2 id="report-title" class="mt-2 font-typewriter text-2xl leading-tight">{title}</h2>
		<p class="mt-2 text-[0.95rem] leading-relaxed">
			{timeUp
				? 'The fifteen minutes are over and the case is still open. Here is what was not in place when the line went dead.'
				: costly
					? 'The service is back, but not everything came through unscathed.'
					: 'The service is stable again and nothing else was harmed. Nicely done, detective.'}
		</p>

		<dl class="mt-4 grid grid-cols-3 gap-3 border-y border-ink/30 py-3 text-sm">
			<div>
				<dt class="font-typewriter text-xs tracking-widest text-ink-soft uppercase">Time</dt>
				<dd class="text-lg font-bold tabular-nums">{formatDuration(status.solvedAtSeconds ?? status.elapsedSeconds)}</dd>
			</div>
			<div>
				<dt class="font-typewriter text-xs tracking-widest text-ink-soft uppercase">Commands</dt>
				<dd class="text-lg font-bold tabular-nums">{status.commands}</dd>
			</div>
			<div>
				<dt class="font-typewriter text-xs tracking-widest text-ink-soft uppercase">Hints used</dt>
				<dd class="text-lg font-bold tabular-nums">{hintsUsed}</dd>
			</div>
		</dl>

		<section class="mt-4">
			<h3 class="font-typewriter text-xs tracking-widest text-ink-soft uppercase">
				Findings: {found.length} of {total} uncovered
			</h3>
			<ul class="mt-2 space-y-1.5 text-sm leading-relaxed">
				{#each found as finding (finding.id)}
					<li class="flex gap-2"><span aria-hidden="true" class="text-ok">✓</span><span>{finding.clue}</span></li>
				{/each}
				{#each report.missedFindings as clue (clue)}
					<li class="flex gap-2 text-ink-soft">
						<span aria-hidden="true">✗</span>
						<span><span class="sr-only">Missed: </span><em>{clue}</em></span>
					</li>
				{/each}
			</ul>
		</section>

		<section class="mt-4">
			<h3 class="font-typewriter text-xs tracking-widest text-ink-soft uppercase">
				{timeUp ? 'To close the case' : 'The case was solved when'}
			</h3>
			<ul class="mt-2 space-y-1.5 text-sm">
				{#each report.assertions as item (item.label)}
					<li class="flex gap-2">
						{#if item.ok}
							<span aria-hidden="true" class="text-ok">✓</span><span>{item.label}</span>
						{:else}
							<span aria-hidden="true" class="text-stamp">✗</span>
							<span><span class="sr-only">Not yet: </span>{item.label}</span>
						{/if}
					</li>
				{/each}
			</ul>
		</section>

		{#if report.constraints.length > 0}
			<section class="mt-4">
				<h3 class="font-typewriter text-xs tracking-widest text-ink-soft uppercase">And along the way</h3>
				<ul class="mt-2 space-y-1.5 text-sm">
					{#each report.constraints as item (item.label)}
						<li class="flex gap-2">
							{#if item.ok}
								<span aria-hidden="true" class="text-ok">✓</span><span>{item.label}</span>
							{:else}
								<span aria-hidden="true" class="text-stamp">✗</span>
								<span>
									<span class="sr-only">Broken: </span>{item.label}
									{#if item.brokenAtSeconds !== null}
										<span class="text-ink-soft">(at {formatDuration(item.brokenAtSeconds)})</span>
									{/if}
								</span>
							{/if}
						</li>
					{/each}
				</ul>
			</section>
		{/if}

		<div class="mt-6 flex flex-wrap gap-3">
			<!-- svelte-ignore a11y_autofocus -->
			<button
				type="button"
				autofocus
				class="rounded border border-ink bg-ink px-4 py-1.5 font-typewriter text-paper"
				onclick={onclose}
			>
				Back to the terminal
			</button>
			<button
				type="button"
				class="rounded border border-ink/50 px-4 py-1.5 font-typewriter hover:bg-paper-shade/60"
				onclick={onrestart}
			>
				Play again
			</button>
		</div>
	{/if}
</dialog>
