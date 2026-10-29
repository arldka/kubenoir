<script lang="ts">
	let {
		number,
		title,
		briefing,
		outcome = null
	}: {
		number: number;
		title: string;
		briefing: string;
		/** The verdict once the case is closed; the stamp follows it. */
		outcome?: 'closed' | 'closedWithCost' | null;
	} = $props();
</script>

<header>
	<div class="flex items-start justify-between gap-3">
		<p class="font-typewriter text-xs tracking-widest text-ink-soft uppercase">
			Case file No. {number}
		</p>
		<span class="stamp text-sm {outcome === 'closed' ? 'stamp-ok' : ''}">
			{outcome === 'closed' ? 'Case closed' : outcome === 'closedWithCost' ? 'Closed at a cost' : 'Open case'}
		</span>
	</div>
	<h1 class="mt-2 font-typewriter text-2xl leading-tight">{title}</h1>
	<p class="mt-3 text-[0.95rem] leading-relaxed">{briefing}</p>
</header>
