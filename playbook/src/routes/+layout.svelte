<script lang="ts">
	import '../app.css';
	import favicon from '$lib/assets/favicon.svg';
	import berkeleyMono from '$lib/assets/fonts/BerkeleyMono-Variable.woff2';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { base } from '$app/paths';
	import { pages, repo } from '$lib/pages';
	import GitHubIcon from '$lib/components/GitHubIcon.svelte';

	let { children } = $props();

	const nums = pages.map((p) => p.num);

	function current(): string | null {
		const m = page.url.pathname.slice(base.length).match(/^\/(0[0-6])\/?$/);
		return m ? m[1] : null;
	}

	function onkeydown(e: KeyboardEvent) {
		if (e.metaKey || e.ctrlKey || e.altKey) return;
		const t = e.target;
		if (t instanceof HTMLElement && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable)) {
			return;
		}
		const cur = current();
		if (e.key === 'Escape' && cur !== null) {
			goto(`${base}/`);
		} else if (e.key === 'ArrowRight') {
			if (cur === null) {
				goto(`${base}/00`);
			} else {
				const i = nums.indexOf(cur);
				if (i < nums.length - 1) goto(`${base}/${nums[i + 1]}`);
			}
		} else if (e.key === 'ArrowLeft') {
			if (cur === '00') {
				goto(`${base}/`);
			} else if (cur !== null) {
				goto(`${base}/${nums[nums.indexOf(cur) - 1]}`);
			}
		}
	}
</script>

<svelte:head>
	<link rel="icon" href={favicon} />
	<link rel="preload" href={berkeleyMono} as="font" type="font/woff2" crossorigin="anonymous" />
</svelte:head>

<svelte:window {onkeydown} />

<main>
	<header class="site">
		<a class="site-title" href="{base}/">content addressing across hosts</a>
		<a
			class="site-gh"
			href={repo}
			target="_blank"
			rel="noopener"
			aria-label="source on GitHub"
		>
			<GitHubIcon />
		</a>
	</header>
	{@render children()}
</main>

<style>
	.site {
		display: flex;
		justify-content: space-between;
		align-items: center;
		margin-bottom: 2.25rem;
	}
	.site-title {
		font-size: 0.875rem;
		font-weight: var(--weight-display);
		color: var(--text-primary);
	}
	.site-title:hover {
		color: var(--text-tertiary);
	}
	.site-gh {
		display: inline-flex;
		color: var(--text-tertiary);
	}
	.site-gh:hover {
		color: var(--text-primary);
	}
</style>
