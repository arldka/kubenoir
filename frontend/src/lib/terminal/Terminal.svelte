<script lang="ts">
	import { onMount } from 'svelte';
	import { FitAddon } from '@xterm/addon-fit';
	import { Terminal } from '@xterm/xterm';
	import '@xterm/xterm/css/xterm.css';

	import { getSoundscape, soundForKey } from '../audio/index.js';
	import { complete, evalCommand } from '../engine.js';
	import { banner as makeBanner } from './builtins.js';
	import { History } from './history.js';
	import { TerminalSession } from './session.js';

	let { scenarioId, title }: { scenarioId: string; title: string } = $props();

	let host: HTMLDivElement;
	let term: Terminal | undefined;

	/** Rows between the cursor and the start of its wrapped input line (see SessionOptions). */
	function rowsAboveCursor(t: Terminal): number {
		const buffer = t.buffer.active;
		let row = buffer.baseY + buffer.cursorY;
		let up = 0;
		while (row > 0 && buffer.getLine(row)?.isWrapped) {
			row--;
			up++;
		}
		return up;
	}

	onMount(() => {
		let disposed = false;
		let cleanup = () => {};

		(async () => {
			// Glyph metrics are measured when the terminal opens, so the fonts must be loaded first.
			await Promise.all([
				document.fonts.load('16px "Courier Prime"'),
				document.fonts.load('bold 16px "Courier Prime"')
			]).catch(() => {});
			if (disposed) return;

			const t = new Terminal({
				fontFamily: '"Courier Prime", ui-monospace, "Courier New", monospace',
				fontSize: window.innerWidth < 640 ? 12 : 16,
				lineHeight: 1.15,
				cursorBlink: true,
				scrollback: 5000,
				theme: {
					background: '#17130f',
					foreground: '#e6d9bc',
					cursor: '#e0a63a',
					cursorAccent: '#17130f',
					selectionBackground: '#6b5430'
				}
			});
			term = t;
			const fit = new FitAddon();
			t.loadAddon(fit);
			t.open(host);
			fit.fit();

			// Ctrl+C copies when text is selected; otherwise it cancels the line as usual.
			t.attachCustomKeyEventHandler((event) => {
				if (event.type === 'keydown' && event.ctrlKey && event.key === 'c' && t.hasSelection()) {
					void navigator.clipboard?.writeText(t.getSelection()).catch(() => {});
					t.clearSelection();
					return false;
				}
				return true;
			});

			const session = new TerminalSession({
				write: (data) => t.write(data),
				cols: () => t.cols,
				evalCommand,
				complete,
				history: new History(`k8sd:history:${scenarioId}`),
				rowsAboveCursor: () => rowsAboveCursor(t),
				onKey: (key) => {
					const sound = soundForKey(key);
					if (sound) getSoundscape().keystroke(sound);
				},
				banner: makeBanner(title)
			});
			const input = t.onData((data) => void session.onData(data));
			const observer = new ResizeObserver(() => {
				fit.fit();
				session.resize();
			});
			observer.observe(host);

			session.start();
			t.focus();
			cleanup = () => {
				observer.disconnect();
				input.dispose();
				t.dispose();
				term = undefined;
			};
		})();

		return () => {
			disposed = true;
			cleanup();
		};
	});
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div
	bind:this={host}
	class="terminal-host h-full w-full"
	aria-label="Case terminal"
	onclick={() => term?.focus()}
></div>

<style>
	/* Fill the frame; xterm rounds its size down to whole rows, so let the screen colour show
	   through the leftover pixels instead of xterm's default black viewport. */
	.terminal-host {
		background-color: var(--color-screen);
	}
	.terminal-host :global(.xterm) {
		height: 100%;
	}
	.terminal-host :global(.xterm-viewport) {
		background-color: transparent !important;
	}
</style>
