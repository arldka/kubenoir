<script lang="ts">
	import { soundSettings } from '../audio/index.js';

	const percent = $derived(Math.round($soundSettings.volume * 100));
</script>

<section aria-labelledby="sound-heading">
	<div class="flex items-baseline justify-between">
		<h2 id="sound-heading" class="font-typewriter text-xs tracking-widest text-ink-soft uppercase">Sound</h2>
		<span class="text-xs text-ink-soft tabular-nums" aria-hidden="true">
			{$soundSettings.muted ? 'muted' : `${percent}%`}
		</span>
	</div>

	<div class="mt-2 flex items-center gap-3">
		<button
			type="button"
			data-testid="mute-toggle"
			class="grid h-8 w-8 shrink-0 place-items-center rounded border border-ink/40 hover:bg-paper-shade/60"
			aria-pressed={$soundSettings.muted}
			aria-label={$soundSettings.muted ? 'Unmute' : 'Mute'}
			onclick={() => soundSettings.toggleMuted()}
		>
			<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
				<path d="M11 5 6 9H3v6h3l5 4z" fill="currentColor" />
				{#if $soundSettings.muted}
					<path d="m16 9 5 6M21 9l-5 6" />
				{:else}
					<path d="M15.5 8.5a5 5 0 0 1 0 7" />
					<path d="M18.5 5.5a9 9 0 0 1 0 13" />
				{/if}
			</svg>
		</button>

		<input
			type="range"
			data-testid="volume-slider"
			class="h-2 w-full cursor-pointer accent-stamp"
			min="0"
			max="100"
			step="1"
			value={percent}
			aria-label="Volume"
			aria-valuetext="{percent} percent"
			oninput={(event) => {
				soundSettings.setVolume(event.currentTarget.valueAsNumber / 100);
				if ($soundSettings.muted && event.currentTarget.valueAsNumber > 0) soundSettings.setMuted(false);
			}}
		/>
	</div>
	<p class="mt-1 text-xs text-ink-soft">Rain, jazz and typewriter keys. Starts with your first click.</p>
</section>
