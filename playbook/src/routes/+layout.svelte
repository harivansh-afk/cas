<script lang="ts">
	import '../app.css';
	import favicon from '$lib/assets/favicon.svg';
	import berkeleyMono from '$lib/assets/fonts/BerkeleyMono-Variable.woff2';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { pages } from '$lib/pages';

	let { children } = $props();

	const nums = pages.map((p) => p.num);

	function current(): string | null {
		const m = page.url.pathname.match(/^\/(0[0-5])\/?$/);
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
			goto('/');
		} else if (e.key === 'ArrowRight') {
			if (cur === null) {
				goto('/00');
			} else {
				const i = nums.indexOf(cur);
				if (i < nums.length - 1) goto(`/${nums[i + 1]}`);
			}
		} else if (e.key === 'ArrowLeft') {
			if (cur === '00') {
				goto('/');
			} else if (cur !== null) {
				goto(`/${nums[nums.indexOf(cur) - 1]}`);
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
	{@render children()}
</main>
