<script lang="ts">
	let {
		hints,
		revealed,
		onrequest
	}: { hints: string[]; revealed: number; onrequest: () => void } = $props();

	const remaining = $derived(hints.length - revealed);
</script>

{#if hints.length > 0}
	<section aria-labelledby="hints-heading">
		<h2 id="hints-heading" class="font-typewriter text-xs tracking-widest text-ink-soft uppercase">
			Notes from the desk
		</h2>

		{#if revealed > 0}
			<ol class="mt-2 space-y-2 text-sm leading-relaxed">
				{#each hints.slice(0, revealed) as hint, i (i)}
					<li class="border-l-2 border-stamp/60 pl-3">
						<span class="font-typewriter text-xs text-stamp">Hint {i + 1}.</span>
						{hint}
					</li>
				{/each}
			</ol>
		{/if}

		<button
			type="button"
			class="mt-3 w-full rounded border border-ink/50 bg-paper-shade px-3 py-1.5 font-typewriter text-sm hover:bg-paper-shade/60 disabled:cursor-not-allowed disabled:opacity-50"
			disabled={remaining === 0}
			onclick={onrequest}
		>
			{remaining === 0 ? 'No more hints' : `Request a hint (${revealed}/${hints.length} used)`}
		</button>
	</section>
{/if}
