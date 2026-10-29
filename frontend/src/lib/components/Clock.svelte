<script lang="ts">
	let { now, since }: { now: string | null; since: number | null } = $props();

	const time = new Intl.DateTimeFormat('en-GB', {
		timeZone: 'UTC',
		hour: '2-digit',
		minute: '2-digit',
		second: '2-digit'
	});
	const date = new Intl.DateTimeFormat('en-GB', {
		timeZone: 'UTC',
		weekday: 'short',
		day: 'numeric',
		month: 'short',
		year: 'numeric'
	});

	const ms = $derived(now ? Date.parse(now) : null);
	const elapsed = $derived.by(() => {
		if (ms === null || since === null) return '--:--:--';
		const total = Math.max(0, Math.floor((ms - since) / 1000));
		const h = String(Math.floor(total / 3600)).padStart(2, '0');
		const m = String(Math.floor((total % 3600) / 60)).padStart(2, '0');
		const s = String(total % 60).padStart(2, '0');
		return `${h}:${m}:${s}`;
	});
</script>

<dl class="grid grid-cols-2 gap-3 border-t border-b border-ink/30 py-3 text-sm">
	<div>
		<dt class="font-typewriter text-xs tracking-widest text-ink-soft uppercase">Cluster clock</dt>
		<dd class="text-lg font-bold tabular-nums">{ms === null ? '--:--:--' : time.format(ms)} UTC</dd>
		<dd class="text-xs text-ink-soft">{ms === null ? '' : date.format(ms)}</dd>
	</div>
	<div>
		<dt class="font-typewriter text-xs tracking-widest text-ink-soft uppercase">On the case</dt>
		<dd class="text-lg font-bold tabular-nums">{elapsed}</dd>
	</div>
</dl>
