<script lang="ts">
	import { base } from '$app/paths';
	import '$lib/architecture/article.css';
	import System from '$lib/system/System.svelte';
	import { nodes, edges, flows } from '$lib/system/graph';
	import { childrenOf, descendantCount } from '$lib/system/layout';
	import { sourceAt } from '$lib/architecture/source';
	import type { PageData } from './$types';

	let { data }: { data: PageData } = $props();

	const commit = '7a555f3fcab77ea2279985517a587d1880aab2b2';
	const rev = commit.slice(0, 7);
	const at = sourceAt(commit);
	const doc = (name: string) => at(`docs/${name}.md`);
	// Files this update adds link to the preserved pre-split main snapshot.
	const onMain = (path: string) => `https://git.harivan.sh/harivansh-afk/cas-research/src/commit/63354b6885179daff980e03f15a15a9c449e794a/${path}`;
	const children = childrenOf(nodes);
	const roots = nodes.filter((n) => !n.parent);
	const byId = new Map(nodes.map((n) => [n.id, n]));
	const sections = [
		['layers', 'Top down, in seven layers'],
		['operations', 'One request, one path'], ['reference', 'Every part, in words'],
		['limits', 'What the diagram does not show'], ['evidence', 'Revision and checks']
	];
	const layers: [string, string][] = [
		['guest', 'An unmodified Linux VM writes files; ext4 turns them into 4 KiB block requests and FLUSHes on one virtio-blk disk.'],
		['qemu', 'Stock QEMU shares guest RAM with the daemon, sets the queues up over a Unix socket, retains the inflight memfd, and leaves the data path.'],
		['host', 'One process serves every image: a frontend and a reactor thread per image, one compactor thread, shared caches, budgets, scheduler and gates.'],
		['core', 'The library holds the formats and their recovery parsers, the indexes and the copy-on-write manifest, the caches, the budgets and the disk accounting.'],
		['kernel', 'XFS with O_DIRECT, fdatasync, reflinks and hole punching; io_uring per reactor; eventfds, memfds, flock and timerfds for control.'],
		['disk', 'One store root: a catalog, shared chunk segments, a manifest and a private WAL per image, reflinked snapshots, archived rejects.'],
		['tooling', 'Nix builds and boots everything; cas-harness and casctl launch, verify and bind results to a revision; docs/ keeps the records the site imports.']
	];
	/** Descendants of a root in declaration order, depth first. */
	function under(id: string): { node: (typeof nodes)[number]; depth: number }[] {
		const out: { node: (typeof nodes)[number]; depth: number }[] = [];
		const walk = (pid: string, depth: number) => {
			for (const c of children.get(pid) ?? []) {
				out.push({ node: c, depth });
				walk(c.id, depth + 1);
			}
		};
		walk(id, 1);
		return out;
	}
	const counts = { nodes: nodes.length, edges: edges.length, flows: flows.length, files: new Set(nodes.flatMap((n) => n.files ?? [])).size, docs: new Set(nodes.flatMap((n) => n.docs ?? [])).size };
</script>

<svelte:head>
	<title>Update 04 · The whole system, one diagram</title>
	<meta name="description" content="Every part of the CAS backend and the tooling around it in one expandable diagram: the guest, QEMU, the cas-host process, the cas-core library, the host kernel, the bytes on disk and the harness, with what each part does, how it works, and where its source is." />
</svelte:head>

<article class="architecture" id="beginning">
	<header>
		<a class="back" href="{base}/">← index</a>
		<span class="eyebrow">Update 04 · 16 September 2026</span>
		<h1>The whole system, one diagram</h1>
		<p class="lede">Every part of the project at <code>{rev}</code>, top down: guest and QEMU, the <code>cas-host</code> process, the <code>cas-core</code> library, the kernel, the bytes on disk and the tooling. Zoom and drag; a <code>+N</code> box opens; choose any box for what it does and where its source is; an operation button traces one request.</p>
	</header>

	<System source={at} initial={data.initial} />

	<nav aria-label="Update contents"><ol>{#each sections as [id, title]}<li><a href={`#${id}`}>{title}</a></li>{/each}</ol></nav>

	<section id="layers">
		<h2>Top down, in seven layers</h2>
		<p>Read the diagram from the guest down. Each layer below links to its box; the parts inside are listed in <a href="#reference">the reference</a>.</p>
		<div class="table-scroll"><table class="spec"><thead><tr><th>Layer</th><th>What it is</th><th>Parts</th></tr></thead><tbody>
			{#each layers as [id, text] (id)}
				{@const n = byId.get(id)!}
				<tr><td><a href={`#node-${id}`}>{n.title}</a><br /><span class="dim">{n.sub}</span></td><td>{text}</td><td>{descendantCount(id, children)}</td></tr>
			{/each}
		</tbody></table></div>
		<p><strong>The boundaries that matter.</strong> The guest never learns about hashes; QEMU never touches a block after setup. The frontend admits and completes; the reactor executes in one write order; the compactor is the only writer of shared content. cas-core has no threads and no policy: it is formats, indexes and accounting with invariants carried by types. The kernel supplies the only durability primitive, fdatasync on O_DIRECT files, and the design’s crash model is exactly what that primitive promises.</p>
		<p><strong>Three prefixes explain most of the arrows.</strong> A WRITE is published at P, the last mutation whose batch completed in order. A FLUSH waits for E, the last mutation an fdatasync covered. Compaction moves D, the last mutation the committed manifest represents. During serving D ≤ E ≤ P; the staging index covers (D, P], the manifest covers everything at or below D, and reclamation frees WAL payload below D once no reader or replay identity pins it.</p>
	</section>

	<section id="operations">
		<h2>One request, one path</h2>
		<p>The operation buttons above the diagram expand the parts a request passes through and number them. Each ends with what the operation guarantees.</p>
		<div class="table-scroll"><table class="spec"><thead><tr><th>Operation</th><th>Steps</th><th>What it guarantees</th></tr></thead><tbody>
			{#each flows as f (f.id)}<tr><td>{f.name}</td><td>{f.steps.length}</td><td>{f.result}</td></tr>{/each}
		</tbody></table></div>
	</section>

	<section id="reference">
		<h2>Every part, in words</h2>
		<p>The same text the panel shows, laid out layer by layer so it can be read straight through, searched, or printed. Paths link to <code>{rev}</code>.</p>
		{#each roots as root (root.id)}
			<details id={`ref-${root.id}`}>
				<summary>{root.title}{#if root.sub}{' · '}{root.sub}{/if}{' — '}{descendantCount(root.id, children)} parts</summary>
				<div class="ref">
					<p class="what">{root.what}</p>
					{#if root.how?.length}<ul>{#each root.how as h, i (i)}<li>{h}</li>{/each}</ul>{/if}
					{#each under(root.id) as { node, depth } (node.id)}
						<div class="part" style:--depth={depth}>
							<h4 id={`part-${node.id}`}><a href={`#node-${node.id}`}>{node.title}</a>{#if node.sub}<span class="sub">{node.sub}</span>{/if}</h4>
							<p class="what">{node.what}</p>
							{#if node.how?.length}<ul>{#each node.how as h, i (i)}<li>{h}</li>{/each}</ul>{/if}
							{#if node.numbers?.length}<dl>{#each node.numbers as [k, v], i (i)}<div><dt>{k}</dt><dd>{v}</dd></div>{/each}</dl>{/if}
							{#if node.types?.length}<p class="types">{#each node.types as t, i (i)}<code>{t}</code>{i < node.types.length - 1 ? ' ' : ''}{/each}</p>{/if}
							{#if node.files?.length || node.docs?.length}<p class="paths">{#each node.files ?? [] as f, i (f)}<a href={at(f)}><code>{f}</code></a>{i < (node.files?.length ?? 0) - 1 || node.docs?.length ? ' · ' : ''}{/each}{#each node.docs ?? [] as d, i (d)}<a href={doc(d)}>docs/{d}.md</a>{i < (node.docs?.length ?? 0) - 1 ? ' · ' : ''}{/each}</p>{/if}
						</div>
					{/each}
				</div>
			</details>
		{/each}
	</section>

	<section id="limits">
		<h2>What the diagram does not show</h2>
		<ul class="findings">
			<li><strong>Structure is not behaviour.</strong> A box and an arrow say what exists and who calls whom at <code>{rev}</code>. Whether a path works under load, crash or exhaustion is established only by the records in <a href={doc('validation')}>the validation history</a>; the parts cite the notes that describe their acceptance, not proof of it.</li>
			<li><strong>Some notes are behind the code.</strong> The five-second admission deadline described in several design notes no longer exists; admission waits retry every 100 ms and never become IOERR. The manifest reclamation note describes bitmap windows the code replaced with one sorted live-offset list. The vhost-user note predates four queues, ZERO/DISCARD and retained recovery. The parts say so where it matters.</li>
			<li><strong>Nothing distributed exists yet.</strong> Remote reads, replication, ownership transfer and migration, the whole right-hand side of the study, have no box because they have no code. The census and the persistence oracle are the only measurement tools that touch the research questions directly.</li>
			<li><strong>Two engines, one drawing.</strong> The v1 staging log and the raw io_uring backend remain as controls for the checkpoint suite and are drawn faded; the diagram’s paths describe the local runtime that the shared host uses.</li>
		</ul>
	</section>

	<section id="evidence">
		<h2>Revision and checks</h2>
		<p><strong>How it was made.</strong> Five independent readers went through every first-party source file at <code>{rev}</code> and reported each module’s types, invariants, constants and edges. Those reports became the {counts.nodes} parts and {counts.edges} connections in the diagram. Where a design note and the code disagreed, the code won and the note is named in the part’s text. No experiment ran for this page.</p>
		<p>Every path on this page links to <a href={at('')}>{rev}</a>, the current main after Update 03. The graph names {counts.files} source files and {counts.docs} design notes, each checked to exist at that commit. The page ran <code>pnpm check</code>, <code>pnpm build</code>, a static check that every fragment link and pinned path resolves, and a browser check of the diagram’s expansion, selection and operation traces on Spark. <a href={onMain('docs/validation/2026-09-16-update-4.md')}>Session record</a> · <a href={onMain('playbook/src/lib/system/graph.ts')}>Graph definition</a> · <a href={at('TODO.md')}>Progress tracker</a>.</p>
		<p>The layout engine is ELK’s layered algorithm, the same one Mermaid uses for subgraph diagrams, loaded in the browser only when a view changes; the starting view is laid out at build time so the page is complete without JavaScript.</p>
	</section>

	<footer><a href="{base}/">← index</a><a href="#beginning">↑ beginning</a><a href="{base}/updates/3/">Update 03 ↗</a></footer>
</article>

<style>
	.dim { color: var(--text-tertiary); font-size: 0.6875rem; }
	.ref { font-size: 0.8125rem; }
	.ref > .what { margin-bottom: 0.5rem; }
	.ref ul { padding-left: 1.25rem; margin: 0 0 0.75rem; }
	.ref li { margin-bottom: 0.3rem; }
	.ref li::marker { color: var(--text-quaternary); }
	.part { margin: 1rem 0 0 calc((var(--depth, 1) - 1) * 1.25rem); padding-left: 0.875rem; border-left: 1px solid var(--border); }
	.part h4 { font-size: 0.8125rem; margin: 0 0 0.25rem; color: var(--text-primary); }
	.part h4 a { color: inherit; border-bottom: 1px solid var(--border); }
	.part h4 .sub { margin-left: 0.6rem; font-weight: 400; color: var(--text-tertiary); font-size: 0.6875rem; }
	.part .what { margin: 0 0 0.4rem; }
	.part dl { margin: 0 0 0.5rem; font-size: 0.6875rem; }
	.part dl div { display: flex; justify-content: space-between; gap: 1rem; border-bottom: 1px solid var(--border-subtle); padding: 0.2rem 0; }
	.part dd { margin: 0; text-align: right; color: var(--text-primary); }
	.part .types code { font-size: 0.6875rem; }
	.part .paths { font-size: 0.6875rem; overflow-wrap: anywhere; margin: 0.25rem 0 0; }
	.part .paths code { background: none; border: 0; padding: 0; font-size: inherit; }
</style>
