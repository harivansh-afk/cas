<script lang="ts">
	import { setContext, type Snippet } from 'svelte';

	let {
		w,
		h,
		label,
		caption,
		children
	}: { w: number; h: number; label: string; caption?: string; children: Snippet } = $props();

	// One marker set per diagram so two figures on a page never share an id.
	const uid = $props.id();
	setContext('diagram', { uid });
</script>

<figure>
	<svg viewBox="0 0 {w} {h}" role="img" aria-label={label} style="max-width: 100%; height: auto;">
		<defs>
			<marker id="arr-{uid}" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
				<polygon points="0,0 8,4 0,8" fill="currentColor" />
			</marker>
			<marker id="arr-accent-{uid}" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
				<polygon points="0,0 8,4 0,8" fill="#d97706" />
			</marker>
			<marker id="arr-muted-{uid}" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
				<polygon points="0,0 8,4 0,8" fill="currentColor" opacity="0.45" />
			</marker>
		</defs>
		{@render children()}
	</svg>
	{#if caption}<figcaption>{caption}</figcaption>{/if}
</figure>
