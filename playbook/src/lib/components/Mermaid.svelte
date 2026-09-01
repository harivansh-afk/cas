<script lang="ts">
	let { code, caption }: { code: string; caption?: string } = $props();

	let container: HTMLDivElement;

	const themes = {
		light: {
			background: '#fafaf9',
			mainBkg: '#ffffff',
			primaryColor: '#ffffff',
			primaryBorderColor: '#e7e5e4',
			primaryTextColor: '#1c1917',
			secondaryColor: '#f5f5f4',
			tertiaryColor: '#f5f5f4',
			lineColor: '#78716c',
			textColor: '#44403c',
			actorBkg: '#ffffff',
			actorBorder: '#e7e5e4',
			actorTextColor: '#1c1917',
			actorLineColor: '#e7e5e4',
			signalColor: '#78716c',
			signalTextColor: '#44403c',
			labelBoxBkgColor: '#f5f5f4',
			labelBoxBorderColor: '#e7e5e4',
			labelTextColor: '#1c1917',
			loopTextColor: '#44403c',
			noteBkgColor: '#f5f5f4',
			noteBorderColor: '#e7e5e4',
			noteTextColor: '#44403c',
			activationBkgColor: '#f5f5f4',
			activationBorderColor: '#e7e5e4'
		},
		dark: {
			background: '#161514',
			mainBkg: '#1c1b1a',
			primaryColor: '#1c1b1a',
			primaryBorderColor: '#292524',
			primaryTextColor: '#fafaf9',
			secondaryColor: '#1c1b1a',
			tertiaryColor: '#1c1b1a',
			lineColor: '#a8a29e',
			textColor: '#d6d3d1',
			actorBkg: '#1c1b1a',
			actorBorder: '#292524',
			actorTextColor: '#fafaf9',
			actorLineColor: '#292524',
			signalColor: '#a8a29e',
			signalTextColor: '#d6d3d1',
			labelBoxBkgColor: '#1c1b1a',
			labelBoxBorderColor: '#292524',
			labelTextColor: '#fafaf9',
			loopTextColor: '#d6d3d1',
			noteBkgColor: '#1c1b1a',
			noteBorderColor: '#292524',
			noteTextColor: '#d6d3d1',
			activationBkgColor: '#1c1b1a',
			activationBorderColor: '#292524'
		}
	};

	let seq = 0;

	$effect(() => {
		const mq = window.matchMedia('(prefers-color-scheme: dark)');
		let cancelled = false;

		async function render() {
			const { default: mermaid } = await import('mermaid');
			mermaid.initialize({
				startOnLoad: false,
				theme: 'base',
				fontFamily: "'Berkeley Mono', ui-monospace, monospace",
				themeVariables: {
					...(mq.matches ? themes.dark : themes.light),
					fontSize: '13px'
				},
				sequence: { mirrorActors: false }
			});
			const { svg } = await mermaid.render(`mmd-${++seq}`, code);
			if (!cancelled && container) container.innerHTML = svg;
		}

		render();
		mq.addEventListener('change', render);
		return () => {
			cancelled = true;
			mq.removeEventListener('change', render);
		};
	});
</script>

<figure>
	<div class="diagram" bind:this={container}></div>
	{#if caption}<figcaption>{caption}</figcaption>{/if}
</figure>

<style>
	.diagram {
		overflow-x: auto;
	}
	.diagram :global(svg) {
		display: block;
		max-width: 100%;
		height: auto;
	}
</style>
