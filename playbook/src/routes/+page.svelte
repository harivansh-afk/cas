<script lang="ts">
	import { base } from '$app/paths';
	import { pages, repo, source } from '$lib/pages';
	import GitHubIcon from '$lib/components/GitHubIcon.svelte';
	import PdfIcon from '$lib/components/PdfIcon.svelte';
</script>

<svelte:head>
	<title>Content-addressed deduplication: a distributed-storage-system study</title>
	<meta
		name="description"
		content="In a content-addressed store the hash is the address, and an address computed from the bytes is the same on every host. This study builds a content-addressed block backend under unmodified QEMU and measures, on two hosts, what that provides and what the remote cold read costs."
	/>
</svelte:head>

<div class="eyebrow-row">
	<span class="eyebrow">research specification</span>
	<span class="links">
		<a class="site-link" href="{base}/pdf" aria-label="PDF of the specification" title="PDF">
			<PdfIcon />
		</a>
		<a class="site-link" href={repo} target="_blank" rel="noopener" aria-label="source on GitHub" title="source on GitHub">
			<GitHubIcon />
		</a>
	</span>
</div>
<h2 class="title">Content-addressed deduplication: a distributed-storage-system study</h2>
<nav class="toc" aria-label="Pages">
	{#each pages as { num, title, description } (num)}
		<div class="row">
			<a class="page" href="{base}/{num}">
				<span class="toc-n">{num}</span>
				<span class="toc-t">{title}</span>
				<span class="toc-d">{description}</span>
			</a>
			<a class="src" href={source(num)} target="_blank" rel="noopener" aria-label="source of page {num} on GitHub" title="source on GitHub">
				<img src="https://github.com/harivansh-afk.png?size=64" alt="" width="16" height="16" loading="lazy" />
			</a>
		</div>
	{/each}
</nav>

<style>
	.eyebrow-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 1rem;
	}
	.title {
		margin: 0.5rem 0 1rem;
	}
	.title::before {
		content: none;
	}
	.links {
		display: inline-flex;
		align-items: center;
		flex-shrink: 0;
	}
	.site-link {
		display: inline-flex;
		align-items: center;
		padding: 0 0.5rem;
		color: var(--text-tertiary);
	}
	.site-link:hover {
		color: var(--text-primary);
	}
	.toc {
		margin-top: 0.875rem;
		border-top: 1px solid var(--border);
	}
	.row {
		display: flex;
		align-items: stretch;
		border-bottom: 1px solid var(--border-subtle);
	}
	.row:hover {
		background: var(--background-secondary);
	}
	.page {
		flex: 1;
		min-width: 0;
		display: grid;
		grid-template-columns: 2.25rem max-content 1fr;
		gap: 0 1rem;
		padding: 0.5rem 0.375rem;
		color: var(--text-secondary);
		align-items: baseline;
	}
	.src {
		display: inline-flex;
		align-items: center;
		padding: 0 0.5rem;
		opacity: 0.6;
	}
	.src:hover {
		opacity: 1;
	}
	.src img {
		border-radius: 50%;
		display: block;
	}
	.toc-n {
		color: var(--text-quaternary);
		font-variant-numeric: tabular-nums;
	}
	.toc-t {
		color: var(--text-primary);
		font-weight: var(--weight-strong);
		white-space: nowrap;
	}
	.toc-d {
		color: var(--text-tertiary);
		font-size: 0.8125rem;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	@media (max-width: 640px) {
		.page {
			grid-template-columns: 2.25rem 1fr;
		}
		.toc-d {
			grid-column: 2;
			white-space: normal;
		}
	}
</style>
