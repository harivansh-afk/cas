<script lang="ts">
	import { base } from '$app/paths';
	import { pages, repo, source } from '$lib/pages';
	import { updates, source as updateSource } from '$lib/updates';
	import { articles, articleSource } from '$lib/articles';
	import Toc from '$lib/components/Toc.svelte';
	import GitHubIcon from '$lib/components/GitHubIcon.svelte';
	import PdfIcon from '$lib/components/PdfIcon.svelte';

	const spec = pages.map(({ num, title, description }) => ({
		num,
		title,
		description,
		href: `${base}/${num}`,
		source: source(num)
	}));

	const meetings = updates.map((u) => ({
		num: u.number,
		title: u.title,
		description: u.date,
		href: `${base}/updates/${u.slug}/`,
		source: updateSource
	}));
	for (const article of articles) {
		meetings.push({
			num: article.num,
			title: article.title,
			description: article.date,
			href: `${base}/${article.route}/`,
			source: articleSource(article.route)
		});
	}
</script>

<svelte:head>
	<title>Content-addressed deduplication: a distributed-storage-system study</title>
	<meta
		name="description"
		content="A content-addressed store identifies each chunk by a cryptographic hash of its contents, so identical data receives an identical identifier on every host. Deduplication in ZFS and dm-vdo is confined to one pool, so a fleet stores a shared chunk once per host and migrates a guest by copying its image. This study implements a content-addressed block backend for virtual machines beneath unmodified QEMU. We predict that a guest is provisioned or migrated by moving only its manifest, that two hosts store at most 55% of what two per-host ZFS pools hold, that a cold 4 KiB read from a peer's memory over TCP arrives before one from local NVMe, and that single-host capture is within 10% of ZFS fast dedup at equal block size. The testbed is two hosts with static membership, Linux guests, and single-digit terabytes, and the cost measured is the network round trip on the cold read path and, in fleet class, on the FLUSH path."
	/>
</svelte:head>

<div class="eyebrow-row">
	<span class="eyebrow">research specification</span>
	<span class="links">
		<a class="site-link" href="{base}/spec.pdf" target="_blank" rel="noopener" aria-label="PDF of the specification" title="PDF">
			<PdfIcon />
		</a>
		<a class="site-link" href={repo} target="_blank" rel="noopener" aria-label="source on GitHub" title="source on GitHub">
			<GitHubIcon />
		</a>
	</span>
</div>
<h2 class="title">Content-addressed deduplication: a distributed-storage-system study</h2>
<Toc rows={spec} label="Pages" />

<h2 class="title section">Updates</h2>
<Toc rows={meetings} label="Updates" align="end" />

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
	.title.section {
		margin-top: 1rem;
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
</style>
