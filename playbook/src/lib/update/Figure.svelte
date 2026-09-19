<script lang="ts">
	import type { Figure } from '$lib/updates';
	import HostFigure from './HostFigure.svelte';
	import StackFigure from './StackFigure.svelte';
	import LogFigure from './LogFigure.svelte';
	import RingFigure from './RingFigure.svelte';
	import ChunkFigure from './ChunkFigure.svelte';
	import UpdatesChart from './UpdatesChart.svelte';
	import PmemFigure from './PmemFigure.svelte';
	import TracksFigure from './TracksFigure.svelte';

	let { kind }: { kind: Figure } = $props();
</script>

{#if kind === 'built'}<HostFigure />
{:else if kind === 'stack'}<StackFigure />
{:else if kind === 'log'}<LogFigure />
{:else if kind === 'ring'}<RingFigure />
{:else if kind === 'chunk'}<ChunkFigure />
{:else if kind === 'updates'}<UpdatesChart />
{:else if kind === 'copies'}<PmemFigure />
{:else if kind === 'tracks'}<TracksFigure />{/if}
