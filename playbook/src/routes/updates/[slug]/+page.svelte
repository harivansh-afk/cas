<script lang="ts">
	import { base } from '$app/paths';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { onMount } from 'svelte';
	import Figure from '$lib/update/Figure.svelte';
	import type { PageData } from './$types';

	let { data }: { data: PageData } = $props();
	const update = $derived(data.update);
	const slides = $derived(update.slides);
	const selected = $derived(Math.max(0, slides.findIndex(({ id }) => `#${id}` === page.url.hash)));
	const current = $derived(slides[selected]);

	// Without JavaScript every slide is visible in order: a readable document.
	let ready = $state(false);
	onMount(() => {
		ready = true;
	});

	function move(index: number) {
		const slide = slides[index];
		if (slide) goto(`${page.url.pathname}#${slide.id}`, { noScroll: true, keepFocus: true });
	}

	function onkeydown(event: KeyboardEvent) {
		if (event.metaKey || event.ctrlKey || event.altKey || event.defaultPrevented) return;
		if (event.target instanceof HTMLElement && event.target.closest('input, textarea, select, [contenteditable="true"]')) return;
		const back = event.key === 'ArrowLeft' || event.key === 'PageUp' || event.key === 'h' || event.key === 'k';
		const forward = event.key === 'ArrowRight' || event.key === 'PageDown' || event.key === ' ' || event.key === 'l' || event.key === 'j';
		if (event.key === 'Escape') goto(`${base}/`);
		else if (back || forward) {
			event.preventDefault();
			move(selected + (back ? -1 : 1));
		} else if (event.key === 'Home' || event.key === 'g') move(0);
		else if (event.key === 'End' || event.key === 'G') move(slides.length - 1);
	}

	/** `[text](url)` inside a line or note becomes a link; everything else is text. */
	function segments(text: string): { text: string; href?: string }[] {
		const out: { text: string; href?: string }[] = [];
		const re = /\[([^\]]+)\]\(([^)\s]+)\)/g;
		let last = 0;
		for (const m of text.matchAll(re)) {
			if (m.index! > last) out.push({ text: text.slice(last, m.index) });
			out.push({ text: m[1], href: m[2] });
			last = m.index! + m[0].length;
		}
		if (last < text.length) out.push({ text: text.slice(last) });
		return out;
	}

	/** A line starting with "- " is a point under the line before it. */
	function point(line: string): { sub: boolean; text: string } {
		return line.startsWith('- ') ? { sub: true, text: line.slice(2) } : { sub: false, text: line };
	}

</script>

<svelte:head><title>Update {update.number} · {update.title}</title></svelte:head>
<svelte:window {onkeydown} />

{#snippet rich(text: string)}{#each segments(text) as seg, i (i)}{#if seg.href}<a href={seg.href} target="_blank" rel="noopener noreferrer">{seg.text}</a>{:else}{seg.text}{/if}{/each}{/snippet}

<a class="back" href="{base}/" aria-label="Back to index">←</a>

<div class="deck">
	{#each slides as slide, index (slide.id)}
		<section id={slide.id} class:inactive={ready && index !== selected} aria-label="Slide {index + 1} of {slides.length}">
			<div class="sheet">
				<div class="lines">
					{#each slide.lines as line, i (line)}
						{@const l = point(line)}
						{@const lead = !l.sub && i > 0 && (slide.lines[i + 1]?.startsWith('- ') ?? false)}
						<p class:thought={i === 0} class:point={l.sub} class:lead>{@render rich(l.text)}</p>
					{/each}
				</div>
				{#if slide.figure}
					<div class="figure"><Figure kind={slide.figure} /></div>
				{/if}
			</div>
		</section>
	{/each}
</div>

{#if ready}
	<span class="count" aria-hidden="true">{selected + 1} / {slides.length}</span>
{/if}

<style>
	.back {
		position: fixed;
		top: 1.25rem;
		left: 1.5rem;
		z-index: 3;
		font-size: 0.875rem;
		color: var(--text-quaternary);
		text-decoration: none;
		opacity: 0.6;
		transition: opacity 120ms, color 120ms;
	}
	.back:hover,
	.back:focus-visible {
		opacity: 1;
		color: var(--text-primary);
	}

	.count {
		position: fixed;
		right: 1.5rem;
		bottom: 1.125rem;
		z-index: 3;
		font-size: 0.75rem;
		color: var(--text-quaternary);
		font-variant-numeric: tabular-nums;
		opacity: 0.7;
	}

	section {
		min-height: 100svh;
		display: flex;
		flex-direction: column;
		justify-content: center;
		padding: clamp(2.5rem, 7vh, 5rem) clamp(1.5rem, 7vw, 7rem);
	}
	.inactive {
		display: none;
	}
	.sheet {
		display: flex;
		flex-direction: column;
		gap: clamp(1.25rem, 3vh, 2.25rem);
		width: 100%;
		max-width: 1100px;
		margin: 0 auto;
	}

	.lines {
		display: flex;
		flex-direction: column;
		gap: clamp(0.75rem, 1.6vh, 1.125rem);
		max-width: none;
	}
	.lines p {
		margin: 0;
		font-size: clamp(1rem, 1.35vw, 1.25rem);
		line-height: 1.5;
		color: var(--text-secondary);
		text-wrap: pretty;
	}
	/* a line followed by points is a heading for them */
	.lines p.lead {
		color: var(--text-primary);
		font-weight: var(--weight-medium);
		margin-bottom: -0.25rem;
	}
	.lines p.point + p.lead,
	.lines p.point + p:not(.point) {
		margin-top: 0.5rem;
	}
	.sheet:has(.point) .figure :global(svg) {
		max-height: min(36svh, 360px);
	}
	.lines p.point {
		position: relative;
		padding-left: 1.75rem;
		margin-top: -0.25rem;
		font-size: clamp(0.9375rem, 1.2vw, 1.125rem);
	}
	.lines p.point::before {
		content: '→';
		position: absolute;
		left: 0.25rem;
		color: var(--text-quaternary);
	}
	.lines a {
		color: inherit;
		text-decoration: underline;
		text-decoration-color: var(--text-quaternary);
		text-underline-offset: 3px;
	}
	.lines a:hover {
		text-decoration-color: currentColor;
	}
	.lines p.thought {
		font-size: clamp(1.25rem, 2vw, 1.75rem);
		line-height: 1.3;
		color: var(--text-primary);
		text-wrap: balance;
		margin-bottom: clamp(0.25rem, 1vh, 0.75rem);
	}

	.figure :global(figure) {
		width: auto;
		margin: 0;
	}
	.figure :global(svg) {
		display: block;
		max-height: min(42svh, 420px);
		width: auto;
		max-width: 100%;
		margin: 0;
	}


	@media (max-width: 760px) {
		section {
			min-height: 0;
			padding: 4rem 1.25rem 3rem;
		}
		.figure :global(svg) {
			max-height: none;
		}
	}
	@media print {
		@page {
			size: A4 landscape;
			margin: 10mm 12mm;
		}
		:global(html),
		:global(body) {
			background: white !important;
			color: #222 !important;
			padding: 0 !important;
			font-size: 12px !important;
		}
		.back,
		.count {
			display: none;
		}
		section,
		.inactive {
			display: flex;
			min-height: 0;
			padding: 0 0 4mm;
			break-after: page;
		}
		section:last-of-type {
			break-after: auto;
		}
		.sheet {
			gap: 5mm;
		}
		.lines p {
			font-size: 10pt;
			color: #444;
		}
		.lines p.thought {
			font-size: 15pt;
			color: #222;
		}
		.figure :global(svg) {
			max-height: 70mm;
		}
	}
</style>
