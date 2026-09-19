<script lang="ts">
	import { onMount } from 'svelte';
	import { nodes, edges, flows, initiallyExpanded, type SystemNode } from './graph';
	import { layoutSystem, pathFor, mermaidFor, childrenOf, ancestorsOf, type Box, type Layout } from './layout';

	let { source, initial }: { source: (path: string) => string; initial: Layout } = $props();

	const byId = new Map(nodes.map((n) => [n.id, n]));
	const children = childrenOf(nodes);
	const uid = $props.id();
	const doc = (name: string) => source(`docs/${name}.md`);
	const expandable = nodes.filter((n) => (children.get(n.id)?.length ?? 0) > 0).map((n) => n.id);
	const MARGIN = 24;

	let expanded = $state(new Set<string>(initiallyExpanded));
	let selected = $state<string | null>(null);
	let flowId = $state<string | null>(null);
	let ready = $state(false);
	let busy = $state(false);
	// svelte-ignore state_referenced_locally (The prerendered layout is only the starting value; later views are computed here.)
	let layout = $state<Layout>(initial);
	let generation = 0;

	/* ---------- the camera: a viewBox over the drawing ---------- */
	// svelte-ignore state_referenced_locally (Starting camera: the whole prerendered drawing.)
	let view = $state({ x: -MARGIN, y: -MARGIN, w: initial.w + 2 * MARGIN, h: initial.h + 2 * MARGIN });
	let autoFit = $state(true);
	let frameW = $state(0);
	let frameH = $state(0);
	let svgEl = $state<SVGSVGElement | null>(null);
	const scale = $derived(frameW && frameH ? Math.min(frameW / view.w, frameH / view.h) : 0);
	const zoomPct = $derived(scale ? Math.round(scale * 100) : 100);
	const labelsAlways = $derived(scale >= 1.05);

	function fit() {
		view = { x: -MARGIN, y: -MARGIN, w: layout.w + 2 * MARGIN, h: layout.h + 2 * MARGIN };
		autoFit = true;
	}
	function zoomAt(factor: number, cx: number, cy: number) {
		const w = view.w / factor;
		const h = view.h / factor;
		const minW = Math.max(layout.w, 1) / 6;
		const maxW = Math.max(layout.w, frameW || 1) * 3;
		if (w < minW || w > maxW) return;
		view = { x: cx - (cx - view.x) / factor, y: cy - (cy - view.y) / factor, w, h };
		autoFit = false;
	}
	function zoomCenter(factor: number) {
		zoomAt(factor, view.x + view.w / 2, view.y + view.h / 2);
	}
	function actualSize() {
		if (!frameW || !frameH) return;
		const cx = view.x + view.w / 2;
		const cy = view.y + view.h / 2;
		view = { x: cx - frameW / 2, y: cy - frameH / 2, w: frameW, h: frameH };
		autoFit = false;
	}
	function toSvg(clientX: number, clientY: number) {
		const m = svgEl?.getScreenCTM();
		if (!m) return { x: 0, y: 0 };
		const p = new DOMPoint(clientX, clientY).matrixTransform(m.inverse());
		return { x: p.x, y: p.y };
	}
	function panBy(dxPx: number, dyPx: number) {
		if (!scale) return;
		view = { ...view, x: view.x - dxPx / scale, y: view.y - dyPx / scale };
		autoFit = false;
	}
	function onwheel(e: WheelEvent) {
		e.preventDefault();
		if (e.ctrlKey || e.metaKey) {
			const p = toSvg(e.clientX, e.clientY);
			zoomAt(Math.exp(-e.deltaY * 0.0025), p.x, p.y);
		} else panBy(-e.deltaX, -e.deltaY);
	}
	const pointers = new Map<number, { x: number; y: number }>();
	let dragStart: { x: number; y: number } | null = null;
	let dragged = $state(false);
	let pinchDist = 0;
	function onpointerdown(e: PointerEvent) {
		if (e.button !== 0 && e.pointerType === 'mouse') return;
		(e.currentTarget as Element).setPointerCapture(e.pointerId);
		pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
		if (pointers.size === 1) {
			dragStart = { x: e.clientX, y: e.clientY };
			dragged = false;
		} else if (pointers.size === 2) {
			const [a, b] = [...pointers.values()];
			pinchDist = Math.hypot(a.x - b.x, a.y - b.y);
		}
	}
	function onpointermove(e: PointerEvent) {
		const prev = pointers.get(e.pointerId);
		if (!prev) return;
		pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
		if (pointers.size === 2) {
			const [a, b] = [...pointers.values()];
			const d = Math.hypot(a.x - b.x, a.y - b.y);
			if (pinchDist > 0) {
				const mid = toSvg((a.x + b.x) / 2, (a.y + b.y) / 2);
				zoomAt(d / pinchDist, mid.x, mid.y);
			}
			pinchDist = d;
			dragged = true;
			return;
		}
		if (!dragStart) return;
		if (!dragged && Math.hypot(e.clientX - dragStart.x, e.clientY - dragStart.y) < 4) return;
		dragged = true;
		panBy(e.clientX - prev.x, e.clientY - prev.y);
	}
	function onpointerup(e: PointerEvent) {
		pointers.delete(e.pointerId);
		if (pointers.size === 0) {
			dragStart = null;
			pinchDist = 0;
			// A drag must not count as a click on the box it ended over.
			setTimeout(() => (dragged = false), 0);
		}
	}
	/** Frame a set of boxes with some room around them. */
	function fitTo(ids: string[]) {
		const rects = layout.boxes.filter((b) => ids.includes(b.id));
		if (!rects.length) return;
		const x0 = Math.min(...rects.map((b) => b.x)) - 48;
		const y0 = Math.min(...rects.map((b) => b.y)) - 48;
		const x1 = Math.max(...rects.map((b) => b.x + b.w)) + 48;
		const y1 = Math.max(...rects.map((b) => b.y + b.h)) + 48;
		view = { x: x0, y: y0, w: Math.max(x1 - x0, 320), h: Math.max(y1 - y0, 200) };
		autoFit = false;
	}
	/** A trace is framed on the boxes it visits. */
	function fitFlow() {
		if (!flow) return;
		fitTo([...new Set(flow.steps.map((s) => visibleFor(s.node)))]);
	}
	/** Bring a box into the camera when it is outside, without changing the zoom. */
	function showBox(id: string) {
		const b = layout.boxes.find((x) => x.id === id);
		if (!b || autoFit) return;
		const inside = b.x >= view.x && b.y >= view.y && b.x + b.w <= view.x + view.w && b.y + b.h <= view.y + view.h;
		if (inside) return;
		view = { ...view, x: b.x + b.w / 2 - view.w / 2, y: b.y + b.h / 2 - view.h / 2 };
	}

	// The starting view comes prerendered; every change is laid out again in the browser.
	$effect(() => {
		const exp = new Set(expanded);
		if (!ready) return;
		const mine = ++generation;
		busy = true;
		layoutSystem(nodes, edges, exp)
			.then((l) => {
				if (mine !== generation) return;
				layout = l;
				busy = false;
				if (flowId && !selected) fitFlow();
				else if (autoFit) fit();
				else if (selected) showBox(selected);
			})
			.catch(() => {
				if (mine === generation) busy = false;
			});
	});

	const markup = $derived(mermaidFor(nodes, edges, expanded));
	const chosen = $derived(selected ? byId.get(selected) ?? null : null);
	const flow = $derived(flowId ? flows.find((f) => f.id === flowId) ?? null : null);

	/** The visible box that stands for a node, following collapsed ancestors upward. */
	function visibleFor(id: string): string {
		let cur: string | undefined = id;
		while (cur && !ancestorsOf(cur, byId).every((a) => expanded.has(a))) cur = byId.get(cur)?.parent;
		return cur ?? id;
	}

	const flowOrder = $derived.by(() => {
		const map = new Map<string, number[]>();
		if (!flow) return map;
		flow.steps.forEach((step, i) => {
			const id = visibleFor(step.node);
			map.set(id, [...(map.get(id) ?? []), i + 1]);
		});
		return map;
	});
	const flowLines = $derived.by(() => {
		const set = new Set<string>();
		if (!flow) return set;
		for (const step of flow.steps) {
			if (!step.via) continue;
			const a = visibleFor(step.via[0]);
			const b = visibleFor(step.via[1]);
			for (const line of layout.lines) if ((line.from === a && line.to === b) || (line.from === b && line.to === a)) set.add(line.id);
		}
		return set;
	});
	const flowClusters = $derived.by(() => {
		const set = new Set<string>();
		if (!flow) return set;
		for (const step of flow.steps) for (const a of ancestorsOf(step.node, byId)) set.add(a);
		return set;
	});

	function toggle(id: string) {
		const next = new Set(expanded);
		if (next.has(id)) {
			for (const n of nodes) if (ancestorsOf(n.id, byId).includes(id)) next.delete(n.id);
			next.delete(id);
		} else next.add(id);
		expanded = next;
	}
	function reveal(id: string) {
		const need = ancestorsOf(id, byId).filter((a) => !expanded.has(a));
		if (!need.length) return;
		const next = new Set(expanded);
		for (const a of need) next.add(a);
		expanded = next;
	}
	function choose(id: string) {
		if (dragged) return;
		selected = id;
		reveal(id);
		showBox(id);
		if (typeof history !== 'undefined') history.replaceState(null, '', `#node-${id}`);
	}
	function expandAll() {
		expanded = new Set(expandable);
	}
	function collapseAll() {
		expanded = new Set();
	}
	function reset() {
		expanded = new Set(initiallyExpanded);
		selected = null;
		flowId = null;
		autoFit = true;
	}
	function chooseFlow(id: string) {
		flowId = flowId === id ? null : id;
		if (!flowId) return;
		const f = flows.find((x) => x.id === flowId)!;
		const next = new Set(expanded);
		for (const step of f.steps) for (const a of ancestorsOf(step.node, byId)) next.add(a);
		selected = null;
		if (next.size === expanded.size) fitFlow();
		else expanded = next;
	}
	function onBoxKey(event: KeyboardEvent, box: Box) {
		if (event.key === 'Enter') {
			event.preventDefault();
			choose(box.id);
		} else if (event.key === ' ' && (box.hidden || box.cluster)) {
			event.preventDefault();
			toggle(box.id);
		}
	}
	function onframekey(e: KeyboardEvent) {
		if (e.target !== e.currentTarget) return;
		if (e.key === '+' || e.key === '=') zoomCenter(1.25);
		else if (e.key === '-') zoomCenter(0.8);
		else if (e.key === '0') fit();
		else if (e.key === 'ArrowLeft') panBy(60, 0);
		else if (e.key === 'ArrowRight') panBy(-60, 0);
		else if (e.key === 'ArrowUp') panBy(0, 60);
		else if (e.key === 'ArrowDown') panBy(0, -60);
		else return;
		e.preventDefault();
	}

	const inbound = $derived(chosen ? edges.filter((e) => e.to === chosen.id) : []);
	const outbound = $derived(chosen ? edges.filter((e) => e.from === chosen.id) : []);
	const path = $derived(chosen ? ancestorsOf(chosen.id, byId).map((id) => byId.get(id)!) : []);
	const kids = $derived(chosen ? children.get(chosen.id) ?? [] : []);

	function fromHash() {
		const m = location.hash.match(/^#node-(.+)$/);
		if (m && byId.has(m[1])) choose(m[1]);
	}
	onMount(() => {
		ready = true;
		fromHash();
	});

	function tone(node: SystemNode) {
		return node.tone ?? 'default';
	}
	function stroke(node: SystemNode) {
		const t = tone(node);
		return t === 'accent' || t === 'outline' ? '#d97706' : 'currentColor';
	}
</script>

<svelte:window onhashchange={fromHash} />

<div class="system" id="system">
	<div class="controls" role="group" aria-label="Diagram controls">
		<span class="group">
			<button type="button" onclick={expandAll}>Expand all</button>
			<button type="button" onclick={collapseAll}>Collapse all</button>
			<button type="button" onclick={reset}>Reset</button>
		</span>
		<span class="group" role="group" aria-label="Follow an operation">
			{#each flows as f (f.id)}<button type="button" aria-pressed={flowId === f.id} onclick={() => chooseFlow(f.id)}>{f.name}</button>{/each}
		</span>
		<span class="group zoom" role="group" aria-label="Zoom">
			<button type="button" onclick={() => zoomCenter(0.8)} aria-label="Zoom out">−</button>
			<span class="pct" aria-live="polite">{busy ? '…' : `${zoomPct}%`}</span>
			<button type="button" onclick={() => zoomCenter(1.25)} aria-label="Zoom in">+</button>
			<button type="button" onclick={fit} aria-pressed={autoFit}>fit</button>
			<button type="button" onclick={actualSize}>1:1</button>
		</span>
	</div>

	<div class="stage">
		<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions (The canvas is a region that takes focus for zoom and pan keys and pointer drags; the boxes inside are the interactive elements.) -->
		<div class="drawing" class:busy class:dragging={dragged} role="region" aria-label="System diagram: drag to pan, ctrl and wheel or pinch to zoom, + − 0 and arrow keys" tabindex="0" bind:clientWidth={frameW} bind:clientHeight={frameH} onkeydown={onframekey} {onwheel} {onpointerdown} {onpointermove} {onpointerup} onpointercancel={onpointerup}>
			<svg bind:this={svgEl} viewBox="{view.x} {view.y} {view.w} {view.h}" preserveAspectRatio="xMidYMid meet" width="100%" height="100%" role="img" aria-label="Every component of the system and how they connect; choose a box for its description">
				<defs>
					<marker id="arr-{uid}" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><polygon points="0,0 8,4 0,8" fill="currentColor" /></marker>
					<marker id="arr-accent-{uid}" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><polygon points="0,0 8,4 0,8" fill="#d97706" /></marker>
				</defs>
				{#each layout.boxes.filter((b) => b.cluster) as box (box.id)}
					{@const on = !flow || flowClusters.has(box.id)}
					<g id={`node-${box.id}`} class="cluster" class:dim={!on}>
						<rect x={box.x} y={box.y} width={box.w} height={box.h} rx="6" fill="currentColor" fill-opacity={selected === box.id ? 0.06 : 0.025} stroke={stroke(box.node)} stroke-width="1" stroke-dasharray="5 4" opacity={tone(box.node) === 'accent' || tone(box.node) === 'outline' ? 0.9 : 0.5} />
						<g class="cluster-label" role="button" tabindex="0" aria-label="{box.node.title}: describe" onclick={() => choose(box.id)} onkeydown={(e) => onBoxKey(e, box)}>
							<rect x={box.x + 1} y={box.y + 1} width={Math.min(box.w - 2, box.node.title.length * 7.2 + (box.node.sub ? box.node.sub.length * 6.3 : 0) + 60)} height="24" rx="4" fill="transparent" />
							<text x={box.x + 12} y={box.y + 17} font-size="9.5" font-weight="600" letter-spacing="0.1em" fill={stroke(box.node)}>{box.node.title.toUpperCase()}</text>
							{#if box.node.sub}<text x={box.x + 12 + box.node.title.length * 7.2 + 14} y={box.y + 17} font-size="9.5" fill="currentColor" opacity="0.55">{box.node.sub}</text>{/if}
						</g>
						<g class="collapse" role="button" tabindex="0" aria-label="Collapse {box.node.title}" onclick={(e) => { e.stopPropagation(); if (!dragged) toggle(box.id); }} onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggle(box.id); } }}>
							<rect x={box.x + box.w - 26} y={box.y + 5} width="20" height="16" rx="3" fill="currentColor" fill-opacity="0.08" />
							<text x={box.x + box.w - 16} y={box.y + 17} text-anchor="middle" font-size="12" font-weight="600" fill="currentColor" opacity="0.7">−</text>
						</g>
					</g>
				{/each}
				{#each layout.lines as line (line.id)}
					{@const hot = flowLines.has(line.id)}
					{@const touches = !!selected && (line.from === selected || line.to === selected)}
					{@const lit = hot || touches}
					{@const dimmed = (flow && !hot) || (selected && !touches && !hot)}
					<g class="line" class:dim={dimmed} class:lit>
						<path d={pathFor(line.points)} fill="none" stroke="transparent" stroke-width="10" />
						<path d={pathFor(line.points)} fill="none" stroke={lit ? '#d97706' : 'currentColor'} stroke-width={lit ? 1.6 : 1.1} stroke-dasharray={line.dashed ? '4 3' : undefined} marker-end={lit ? `url(#arr-accent-${uid})` : `url(#arr-${uid})`} opacity={lit ? 1 : 0.5} />
						{#if line.label && line.labelAt}
							<g class="lbl" class:show={lit || (labelsAlways && !dimmed)}>
								<rect x={line.labelAt.x} y={line.labelAt.y} width={line.labelAt.w} height={line.labelAt.h} rx="3" class="label-bg" />
								<text x={line.labelAt.x + line.labelAt.w / 2} y={line.labelAt.y + 10.5} text-anchor="middle" font-size="10" fill={lit ? '#d97706' : 'currentColor'} opacity={lit ? 1 : 0.8}>{line.label}</text>
							</g>
						{/if}
					</g>
				{/each}
				{#each layout.boxes.filter((b) => !b.cluster) as box (box.id)}
					{@const node = box.node}
					{@const t = tone(node)}
					{@const steps = flowOrder.get(box.id)}
					{@const on = !flow || !!steps}
					{@const chosenBox = selected === box.id}
					{@const near = !!selected && !chosenBox && (inbound.some((e) => visibleFor(e.from) === box.id) || outbound.some((e) => visibleFor(e.to) === box.id))}
					<g id={`node-${box.id}`} class="box" class:dim={!on || (selected && !chosenBox && !near && !steps)} class:chosen={chosenBox} role="button" tabindex="0" aria-label="{node.title}{node.sub ? `, ${node.sub}` : ''}{box.hidden ? `; ${box.hidden} parts inside` : ''}" aria-pressed={chosenBox} onclick={() => choose(box.id)} onkeydown={(e) => onBoxKey(e, box)} ondblclick={() => box.hidden && toggle(box.id)}>
						<rect x={box.x} y={box.y} width={box.w} height={box.h} rx={t === 'ghost' ? 6 : 4} fill={t === 'accent' || steps ? '#d97706' : 'var(--surface)'} fill-opacity={t === 'accent' || steps ? 0.14 : 1} stroke={steps ? '#d97706' : stroke(node)} stroke-width={chosenBox ? 2 : t === 'accent' || steps ? 1.5 : t === 'muted' ? 1 : 1.25} stroke-dasharray={t === 'ghost' ? '4 3' : undefined} opacity={t === 'muted' ? 0.6 : 1} />
						{#if chosenBox}<rect x={box.x - 3} y={box.y - 3} width={box.w + 6} height={box.h + 6} rx="7" fill="none" stroke="currentColor" stroke-width="1" opacity="0.35" />{/if}
						<text x={box.x + box.w / 2 - (box.hidden ? 16 : 0)} y={box.y + (node.sub ? box.h / 2 - 4 : box.h / 2 + 4)} text-anchor="middle" font-size="11.5" font-weight={t === 'muted' ? 500 : 600} fill="currentColor">{node.title}</text>
						{#if node.sub}<text x={box.x + box.w / 2 - (box.hidden ? 16 : 0)} y={box.y + box.h / 2 + 12} text-anchor="middle" font-size="10.5" fill={t === 'accent' ? '#d97706' : 'currentColor'} opacity={t === 'accent' ? 1 : 0.62}>{node.sub}</text>{/if}
						{#if box.hidden}
							<g class="expand" role="button" tabindex="-1" aria-label="Expand {node.title}" onclick={(e) => { e.stopPropagation(); if (!dragged) toggle(box.id); }} onkeydown={(e) => { if (e.key === 'Enter') { e.stopPropagation(); toggle(box.id); } }}>
								<rect x={box.x + box.w - 30} y={box.y + box.h / 2 - 9} width="24" height="18" rx="4" fill="currentColor" fill-opacity="0.08" stroke="currentColor" stroke-opacity="0.25" stroke-width="1" />
								<text x={box.x + box.w - 18} y={box.y + box.h / 2 + 4} text-anchor="middle" font-size="10" font-weight="600" fill="currentColor" opacity="0.8">+{box.hidden}</text>
							</g>
						{/if}
						{#if steps}
							<g class="step">
								<circle cx={box.x} cy={box.y} r="9" fill="#d97706" />
								<text x={box.x} y={box.y + 3.5} text-anchor="middle" font-size="9.5" font-weight="700" fill="#fff">{steps.join(',')}</text>
							</g>
						{/if}
					</g>
				{/each}
			</svg>
			<span class="hint" aria-hidden="true">{layout.boxes.filter((b) => !b.cluster).length} boxes · {layout.lines.length} lines · drag to pan · ctrl+wheel to zoom</span>
		</div>

		<aside class="panel" aria-live="polite">
			{#if flow && !chosen}
				<span class="eyebrow">Operation · {flow.name}</span>
				<ol class="steps">
					{#each flow.steps as step, i (i)}
						<li><button type="button" class="link" onclick={() => choose(step.node)}><span class="n">{i + 1}</span><strong>{byId.get(step.node)?.title}</strong></button><p>{step.text}</p></li>
					{/each}
				</ol>
				<p class="result">{flow.result}</p>
			{:else if chosen}
				{#if path.length}<p class="crumbs">{#each path as p, i (p.id)}<button type="button" class="link" onclick={() => choose(p.id)}>{p.title}</button>{#if i < path.length - 1}<span> › </span>{/if}{/each}</p>{/if}
				<h3>{chosen.title}{#if chosen.sub}<span class="sub">{chosen.sub}</span>{/if}</h3>
				<p class="what">{chosen.what}</p>
				{#if chosen.how?.length}
					<ul class="how">{#each chosen.how as h, i (i)}<li>{h}</li>{/each}</ul>
				{/if}
				{#if chosen.numbers?.length}
					<dl class="numbers">{#each chosen.numbers as [k, v], i (i)}<div><dt>{k}</dt><dd>{v}</dd></div>{/each}</dl>
				{/if}
				{#if chosen.types?.length}
					<p class="kv"><span>Carried by</span> {#each chosen.types as t, i (i)}<code>{t}</code>{i < chosen.types.length - 1 ? ' ' : ''}{/each}</p>
				{/if}
				{#if kids.length}
					<p class="kv"><span>Inside</span> {#each kids as k, i (k.id)}<button type="button" class="link" onclick={() => choose(k.id)}>{k.title}</button>{i < kids.length - 1 ? ', ' : ''}{/each}
						<button type="button" class="mini" onclick={() => toggle(chosen.id)}>{expanded.has(chosen.id) ? 'collapse' : 'expand'}</button></p>
				{/if}
				{#if inbound.length || outbound.length}
					<ul class="links">
						{#each outbound as e, i (`o${i}`)}<li>→ <button type="button" class="link" onclick={() => choose(e.to)}>{byId.get(e.to)?.title}</button>{#if e.label}<span class="edge"> · {e.label}</span>{/if}</li>{/each}
						{#each inbound as e, i (`i${i}`)}<li>← <button type="button" class="link" onclick={() => choose(e.from)}>{byId.get(e.from)?.title}</button>{#if e.label}<span class="edge"> · {e.label}</span>{/if}</li>{/each}
					</ul>
				{/if}
				{#if chosen.files?.length || chosen.docs?.length}
					<ul class="files">
						{#each chosen.files ?? [] as f (f)}<li><a href={source(f)}><code>{f}</code></a></li>{/each}
						{#each chosen.docs ?? [] as d (d)}<li><a href={doc(d)}>docs/{d}.md</a></li>{/each}
					</ul>
				{/if}
			{:else}
				<span class="eyebrow">How to read it</span>
				<p class="what">The whole drawing is fitted to the frame. Zoom in with <code>+</code>, ctrl and the wheel, or a pinch; drag to pan; <code>fit</code> brings everything back. Boxes are parts and arrows point the way data or control moves. A box marked <code>+N</code> holds N parts: double-click it, press Space, or choose it and press <em>expand</em>.</p>
				<p class="what">Choose any box or cluster title for what it does, how it works, its numbers and its source files; the arrows touching it light up with their labels. The operation buttons trace one request or one background job through the parts it touches, in order. Amber marks the parts that carry the design’s invariants. Faded boxes exist for completeness.</p>
			{/if}
		</aside>
	</div>

	<details class="markup">
		<summary>Diagram markup for this view · Mermaid</summary>
		<pre><code>{markup}</code></pre>
	</details>
</div>

<style>
	.system { --figure-width: min(1680px, 100vw - 2rem); width: var(--figure-width); margin: 1rem 0 2.5rem calc((100% - var(--figure-width)) / 2); border: 1px solid var(--border); border-radius: var(--radius); background: var(--surface); }
	.controls { display: flex; flex-wrap: wrap; align-items: center; gap: 0.5rem 1.25rem; padding: 0.75rem 1rem; border-bottom: 1px solid var(--border); }
	.group { display: inline-flex; flex-wrap: wrap; align-items: center; gap: 0.35rem; }
	.group.zoom { margin-left: auto; }
	.controls button { font: inherit; font-size: 0.6875rem; padding: 0.35rem 0.6rem; border: 1px solid var(--border); border-radius: 4px; background: var(--background); color: var(--text-secondary); cursor: pointer; }
	.controls button[aria-pressed='true'] { background: var(--text-primary); color: var(--background); border-color: var(--text-primary); }
	.controls button:focus-visible, .link:focus-visible, .mini:focus-visible { outline: 2px solid var(--text-primary); outline-offset: 3px; }
	.pct { font-size: 0.6875rem; color: var(--text-tertiary); font-variant-numeric: tabular-nums; min-width: 3.2rem; text-align: center; }
	.stage { display: grid; grid-template-columns: minmax(0, 1fr) 21rem; }
	.drawing { position: relative; height: min(84vh, 1100px); min-height: 22rem; overflow: hidden; touch-action: none; cursor: grab; user-select: none; transition: opacity 120ms; background: var(--background); }
	.drawing.busy { opacity: 0.6; }
	.drawing.dragging { cursor: grabbing; }
	.drawing:focus-visible { outline: 2px solid var(--text-primary); outline-offset: -2px; }
	.drawing svg { display: block; width: 100%; height: 100%; }
	.hint { position: absolute; right: 0.75rem; bottom: 0.5rem; font-size: 0.625rem; color: var(--text-quaternary); pointer-events: none; }
	.panel { border-left: 1px solid var(--border); padding: 1rem 1.125rem; font-size: 0.75rem; line-height: 1.6; overflow: auto; height: min(84vh, 1100px); min-height: 22rem; }
	.panel h3 { font-size: 0.9375rem; margin: 0.25rem 0 0.5rem; color: var(--text-primary); }
	.panel h3 .sub { display: block; font-size: 0.6875rem; font-weight: 400; color: var(--text-tertiary); margin-top: 0.15rem; }
	.panel .what { margin: 0 0 0.75rem; color: var(--text-secondary); }
	.panel .how { padding-left: 1.1rem; margin: 0 0 0.75rem; }
	.panel .how li { margin-bottom: 0.4rem; }
	.panel .how li::marker { color: var(--text-quaternary); }
	.panel .numbers { margin: 0 0 0.75rem; border-top: 1px solid var(--border); font-size: 0.6875rem; }
	.panel .numbers div { display: flex; justify-content: space-between; gap: 0.75rem; padding: 0.3rem 0; border-bottom: 1px solid var(--border-subtle); }
	.panel .numbers dt { color: var(--text-tertiary); }
	.panel .numbers dd { margin: 0; text-align: right; color: var(--text-primary); font-variant-numeric: tabular-nums; }
	.panel .kv { margin: 0 0 0.5rem; }
	.panel .kv > span { color: var(--text-tertiary); font-size: 0.6875rem; text-transform: uppercase; letter-spacing: 0.08em; margin-right: 0.35rem; }
	.panel code { font-size: 0.6875rem; padding: 0.05em 0.3em; }
	.panel .links, .panel .files { list-style: none; padding: 0; margin: 0 0 0.75rem; }
	.panel .links li { margin: 0.15rem 0; }
	.panel .files { border-top: 1px solid var(--border); padding-top: 0.6rem; }
	.panel .files li { margin: 0.2rem 0; overflow-wrap: anywhere; }
	.panel .files a { font-size: 0.6875rem; }
	.panel .files code { background: none; border: 0; padding: 0; font-size: inherit; }
	.panel .edge { color: var(--text-tertiary); }
	.panel .crumbs { margin: 0 0 0.25rem; font-size: 0.6875rem; color: var(--text-tertiary); }
	.panel .steps { list-style: none; padding: 0; margin: 0.5rem 0 0; }
	.panel .steps li { margin: 0 0 0.75rem; }
	.panel .steps p { margin: 0.15rem 0 0 1.6rem; color: var(--text-secondary); }
	.panel .steps .n { display: inline-block; width: 1.15rem; height: 1.15rem; border-radius: 50%; background: #d97706; color: #fff; font-size: 0.625rem; font-weight: 700; text-align: center; line-height: 1.15rem; margin-right: 0.45rem; }
	.panel .result { border-top: 1px solid var(--border); padding-top: 0.6rem; margin: 0.5rem 0 0; color: var(--text-secondary); }
	.link { font: inherit; background: none; border: 0; padding: 0; color: var(--text-primary); cursor: pointer; border-bottom: 1px solid var(--border); }
	.link:hover { border-bottom-color: var(--text-tertiary); }
	.mini { font: inherit; font-size: 0.625rem; margin-left: 0.5rem; padding: 0.1rem 0.4rem; border: 1px solid var(--border); border-radius: 3px; background: var(--background); color: var(--text-secondary); cursor: pointer; }
	.markup { border-top: 1px solid var(--border); padding: 0.6rem 1.125rem; font-size: 0.75rem; }
	.markup summary { cursor: pointer; }
	.markup pre { overflow: auto; margin: 0.75rem 0 0.5rem; line-height: 1.6; font-size: 0.6875rem; }
	.markup code { border: 0; padding: 0; background: none; font-size: inherit; }

	svg .box, svg .cluster-label, svg .collapse, svg .expand { cursor: pointer; }
	svg .box:focus-visible, svg .cluster-label:focus-visible, svg .collapse:focus-visible { outline: none; }
	svg .box:focus-visible > rect:first-child, svg .cluster-label:focus-visible > rect { stroke: var(--text-primary); stroke-width: 2; stroke-dasharray: none; }
	svg .box:hover > rect:first-child { stroke-width: 1.75; }
	svg .dim { opacity: 0.22; }
	svg .label-bg { fill: var(--surface); stroke: var(--border); stroke-width: 0.75; }
	svg .lbl { opacity: 0; transition: opacity 120ms; pointer-events: none; }
	svg .lbl.show, svg .line:hover .lbl { opacity: 1; }
	svg .line:hover path:last-of-type { stroke-width: 1.8; opacity: 1; }
	svg text { font-family: var(--font-mono); user-select: none; }

	@media (max-width: 900px) { .stage { grid-template-columns: 1fr; } .drawing, .panel { height: min(70vh, 640px); } .panel { border-left: 0; border-top: 1px solid var(--border); } .hint { display: none; } }
	@media print { .controls, .markup, .hint { display: none; } .stage { grid-template-columns: 1fr; } .drawing { height: auto; overflow: visible; } .drawing svg { height: auto; aspect-ratio: var(--ratio, 1.2); } .panel { display: none; } svg .lbl { opacity: 1; } }
</style>
