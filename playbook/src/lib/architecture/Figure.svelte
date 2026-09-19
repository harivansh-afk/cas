<script lang="ts">
	import { Diagram, Node, Edge, Group } from '$lib/components/diagram';
	import { figures, mermaid, type FigureName } from './figures';
	let { name }: { name: FigureName } = $props();
	const figure = $derived(figures[name]);
</script>

<div class="architecture-figure">
	<!-- svelte-ignore a11y_no_noninteractive_tabindex (Scrollable diagrams need keyboard focus for horizontal scrolling.) -->
	<div class="drawing" role="region" aria-label={figure.title} tabindex="0">
		<Diagram w={800} h={figure.h} label={figure.title}>
			{#if 'groups' in figure}
				{#each figure.groups as group}<Group {...group} />{/each}
			{/if}
			{#each figure.edges as edge}<Edge {...edge} />{/each}
			{#each figure.nodes as node}<Node {...node} />{/each}
		</Diagram>
	</div>
	<ol class="compact-map" aria-label={figure.title}>
		{#each figure.nodes as node}
			<li>
				<strong>{node.title}</strong>
				{#if node.sub}<span>{node.sub}</span>{/if}
				{#each figure.edges.filter(edge => edge.from === node.id) as edge}
					<p class="connection">→ {figure.nodes.find(target => target.id === edge.to)?.title}{'label' in edge && edge.label ? ` · ${edge.label}` : ''}</p>
				{/each}
			</li>
		{/each}
	</ol>
	<p class="caption">{figure.caption}</p>
	<details>
		<summary>Diagram markup · Mermaid</summary>
		<pre><code>{mermaid(figure)}</code></pre>
	</details>
</div>

<style>
	.architecture-figure { --figure-width: min(900px, 100vw - 3rem); width: var(--figure-width); margin: 1.75rem 0 2rem calc((100% - var(--figure-width)) / 2); border: 1px solid var(--border); border-radius: var(--radius); background: var(--surface); }
	.drawing { overflow-x: auto; padding: 1rem 0.5rem 0; scrollbar-width: thin; }
	.drawing:focus-visible { outline: 2px solid var(--text-primary); outline-offset: 2px; }
	.drawing :global(figure) { width: 800px; min-width: 800px; max-width: none; margin: 0 auto; }
	.caption { margin: 0; padding: 0.75rem 1.25rem 1rem; color: var(--text-tertiary); font-size: 0.8125rem; }
	.compact-map { display: none; list-style: none; padding: 1rem; margin: 0; }
	.compact-map li { padding: 0.75rem; margin: 0 0 0.75rem; border: 1px solid var(--border); border-radius: 4px; }
	.compact-map strong { display: block; font-size: 0.8125rem; color: var(--text-primary); }
	.compact-map span, .connection { display: block; font-size: 0.6875rem; color: var(--text-tertiary); }
	.connection { margin: 0.5rem 0 0; }
	details { border-top: 1px solid var(--border); padding: 0.6rem 1.25rem; font-size: 0.75rem; }
	summary { cursor: pointer; }
	pre { overflow: auto; margin: 1rem 0; line-height: 1.65; }
	code { border: 0; padding: 0; background: none; font-size: inherit; }
	@media screen and (max-width: 640px) { .drawing { display: none; } .compact-map { display: block; } }
	@media print { .architecture-figure { width: 100%; margin-left: 0; } .drawing { overflow: visible; } .drawing :global(figure) { width: 100%; min-width: 0; } details { display: none; } }
</style>
