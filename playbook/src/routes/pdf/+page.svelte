<script lang="ts">
	import { base } from '$app/paths';
</script>

<svelte:head>
	<title>Content-addressed deduplication · PDF</title>
	<meta name="description" content="The research specification, typeset as one PDF from the same pages the site serves." />
</svelte:head>

<div class="pagehead">
	<span class="eyebrow">research specification · PDF</span>
	<a class="direct" href="{base}/spec.pdf" download="spec.pdf">download</a>
</div>
<iframe class="viewer" src="{base}/spec.pdf" title="Content-addressed deduplication: a distributed-storage-system study (PDF)"></iframe>
<p class="note">
	If the viewer stays blank, your browser does not show PDFs inline. <a href="{base}/spec.pdf">Open the file directly.</a>
</p>

<style>
	.pagehead {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 0.75rem;
		margin-bottom: 0.75rem;
	}
	.direct {
		font-size: 0.75rem;
		color: var(--text-tertiary);
	}
	.direct:hover {
		color: var(--text-primary);
	}
	.viewer {
		display: block;
		width: 100%;
		height: calc(100vh - 11rem);
		min-height: 24rem;
		border: 1px solid var(--border);
		border-radius: var(--radius);
		background: var(--background-secondary);
	}
	.note {
		margin-top: 0.75rem;
	}
</style>
