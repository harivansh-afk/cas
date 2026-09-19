<script lang="ts">
	import { source } from './source';
	let { paths }: { paths: string[] } = $props();
</script>

<ul class="code-paths" aria-label="Read the implementation">
	{#each paths as path}<li><a href={source(path)}><code>{path}</code></a></li>{/each}
</ul>

<style>
	.code-paths { list-style: none; padding: 0; margin: 1rem 0 1.5rem; }
	li { margin: 0.25rem 0; line-height: 1.6; }
	a { font-size: 0.6875rem; overflow-wrap: anywhere; }
	code { padding: 0; background: none; border: 0; font-size: inherit; }
</style>
