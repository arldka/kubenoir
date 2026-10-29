<script lang="ts">
	let { onrestart }: { onrestart: () => void } = $props();

	let confirming = $state(false);
</script>

{#if confirming}
	<div class="flex items-center gap-2 text-sm" role="alert">
		<span class="flex-1">Close this case and start over?</span>
		<button
			type="button"
			class="rounded border border-stamp bg-stamp px-3 py-1 font-typewriter text-paper"
			onclick={() => {
				confirming = false;
				onrestart();
			}}
		>
			Restart
		</button>
		<button
			type="button"
			class="rounded border border-ink/50 px-3 py-1 font-typewriter"
			onclick={() => (confirming = false)}
		>
			Keep going
		</button>
	</div>
{:else}
	<button
		type="button"
		class="w-full rounded border border-ink/30 px-3 py-1.5 font-typewriter text-sm text-ink-soft hover:bg-paper-shade/50"
		onclick={() => (confirming = true)}
	>
		Restart case
	</button>
{/if}
