<script lang="ts">
	import '../app.css';
	import favicon from '$lib/assets/favicon.svg';
	import berkeleyMono from '$lib/assets/fonts/BerkeleyMono-Variable.woff2';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { base } from '$app/paths';
	import { pages } from '$lib/pages';

	let { children } = $props();

	const nums = pages.map((p) => p.num);

	function current(): string | null {
		const m = page.url.pathname.slice(base.length).match(/^\/(0[0-6])\/?$/);
		return m ? m[1] : null;
	}

	const inPage = $derived(current() !== null);

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
	{#if inPage}
		<header class="site">
			<a class="site-title" href="{base}/">← index</a>
			<a class="byline" href="https://github.com/harivansh-afk" target="_blank" rel="noopener">
				<img src="https://github.com/harivansh-afk.png?size=64" alt="" width="16" height="16" loading="lazy" />
				harivansh-afk
			</a>
		</header>
	{/if}
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
	.byline {
		display: inline-flex;
		align-items: center;
		flex-shrink: 0;
		gap: 0.4rem;
		font-size: 0.75rem;
		color: var(--text-tertiary);
		white-space: nowrap;
	}
	.byline:hover {
		color: var(--text-primary);
	}
	.byline img {
		border-radius: 50%;
		display: block;
	}
</style>
