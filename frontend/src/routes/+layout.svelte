<script lang="ts">
	import '@fontsource/special-elite';
	import '@fontsource/courier-prime/400.css';
	import '@fontsource/courier-prime/700.css';
	import './layout.css';
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();
</script>

<svelte:head>
	<title>Kubernetes Detective</title>
</svelte:head>

{@render children()}
