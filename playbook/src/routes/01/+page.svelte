<script lang="ts">
	import { base } from '$app/paths';
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
	import { Diagram, Node, Edge, Group, Note } from '$lib/components/diagram';
</script>

<PageHead num="01" />
<p class="lede">
	<mark>Phase 1 is the paper.</mark>
	Offline analysis of images at rest, requiring neither root nor a guest, producing the split
	on page 00 as a curve against fleet age.
	Six weeks, because corpora are the whole risk.
</p>

<h2>Corpora, in priority order</h2>
<p>
	<strong>Real fleets first.</strong>
	Every VM redundancy study people still cite measured a real fleet (DeDe, 113 VMs; Jayaram et
	al., 525 images).
	A scripted fleet gives clean numbers that say what it was designed to say, and a reviewer will
	say so.
	The target is at least two real fleets (gate G2): candidates are a university lab or department
	VM host, a Proxmox homelab donor, CloudLab's image library, and the author's own machines.
	The ask is hashes with ancestry metadata, not images; the pipeline runs at the donor's site and
	no image byte leaves it, so the privacy conversation is short.
	The first ask goes out in week 1.
</p>

<Diagram
	w={960}
	h={230}
	label="The donor protocol. Inside the donor's site, disk images and an ancestry file feed the census pipeline, which runs there. Only per-chunk hashes, offsets, sizes, and ancestry cross the site boundary to the analysis; no image byte leaves."
	caption="The donor protocol. The census runs where the images are; only chunk hashes, offsets, sizes, and the ancestry file cross the boundary. Published with the pipeline so a donor can audit it."
>
	<Group x={20} y={20} w={560} h={190} label="donor's site" />
	<Node x={50} y={70} w={170} h={52} title="disk images" sub="+ ancestry file" />
	<Note x={135} y={150} anchor="middle" tone="muted" text="never leave the site" />
	<Edge points={[[220, 96], [300, 96]]} />
	<Node x={300} y={70} w={230} h={52} title="census pipeline" sub="runs here · one binary · no root" tone="accent" />
	<Edge points={[[530, 96], [640, 96]]} label="hashes only" labelDy={-9} />
	<Node x={640} y={60} w={290} h={72} title="per-chunk hashes" sub={['offsets · sizes · ancestry', 'no image bytes']} tone="outline" />
	<Edge points={[[785, 132], [785, 160]]} tone="muted" />
	<Node x={700} y={160} w={170} h={44} title="analysis" sub="on the testbed" tone="muted" />
</Diagram>

<p>
	<strong>Synthetic controls second.</strong>
	Cloned fleet (golden image, N clones, scripted drift over simulated months; lineage's best case)
	and convergent installs (N independent installs updated to the same package set; lineage's
	structural blind spot).
	These bracket the real fleets: a real fleet's curve should fall between them, and where it does
	not is a finding.
</p>
<p>
	<strong>Non-VM classes third.</strong>
	A model family (base, fine-tunes, quantizations; where whole-file dedup collapses) and Nix store
	generations (successive closures of one flake; tvix-castore deploys FastCDC and BLAKE3 over the
	store, so the class has a shipped system and no published numbers).
	These are where H2 predicts CDC-only dominates; without them the study cannot show the axis
	move.
	Container layer stacks are dropped: DupHunter covered the class and it adds a week for a
	footnote.
</p>

<h2>Method</h2>
<p>
	Chunk every image at whole-file, CDC (FastCDC, 8K–64K, 16K mean), and fixed 4K and 16K aligned
	granularities, and compute duplicate bytes.
	Against each corpus's declared ancestry, split them by the tree below.
	<a class="term" href="{base}/00#term-lineage-capturable">Lineage-capturable</a> means identical and
	in-place relative to an ancestor, the ceiling for any COW system; a simulated COW at realistic
	record sizes and a declared snapshot cadence gives the practical figure, since sibling sharing
	depends on when snapshots were taken.
	The remainder is <a class="term" href="{base}/00#term-cross-lineage">cross-lineage</a>, split into
	<a class="term" href="{base}/00#term-block-capturable">block-capturable</a> (coincident at 4K and at
	16K alignment, reported at both) and <a class="term" href="{base}/00#term-cdc-only">CDC-only</a>.
	Block-capturable at the matching volblocksize is R1's ceiling and R2's; the sum is R4's.
</p>
<p>
	Duplicates within a single image are counted in these leaves and also reported as their own
	column, as Meyer and Bolosky and Jayaram et al. did, since a fleet study that hides them
	overstates cross-image sharing.
	CDC is computed whole-image and also extent-wise over the write pattern a guest block path would
	present, since that is what a post-process compactor sees (page 03); the gap between the two is
	reported.
	Compression in both orders and zeros excluded, per A7.
	A sample of every hash match is verified byte-for-byte and the sample size reported, per A3.
</p>

<Diagram
	w={1060}
	h={362}
	label="The census decision tree, left to right. A byte range is first tested for zeros or unallocated space, which are excluded. Non-zero bytes are tested for duplication in the corpus; unique bytes are one leaf. Duplicates identical and in-place against a declared ancestor are lineage-capturable. The rest are cross-lineage, and split by whether they coincide at an aligned fixed block into block-capturable, which an aligned dedup table finds, and CDC-only, which only content-defined chunking finds."
	caption="The census, per byte range. Gate G1 requires the leaves to sum to 100% of non-zero bytes at every time point. The two amber leaves together are the headline; their ratio is the week-6 gate."
>
	<Node x={20} y={144} w={110} title="byte range" />
	<Note x={75} y={210} anchor="middle" tone="muted" size={9.5} text={['per corpus,', 'per time point']} />

	<Edge points={[[130, 166], [148, 166], [148, 104], [165, 104]]} />
	<Node x={165} y={88} w={160} h={32} kind="question" title="zeros or unallocated?" />
	<Edge points={[[325, 104], [340, 104], [340, 42], [870, 42]]} label="yes" labelSeg={2} labelDy={-7} tone="muted" />
	<Edge points={[[245, 120], [245, 166], [355, 166]]} label="no" labelSeg={0} labelDx={14} labelDy={4} />

	<Node x={355} y={150} w={150} h={32} kind="question" title="duplicated in corpus?" />
	<Edge points={[[505, 166], [520, 166], [520, 104], [870, 104]]} label="no" labelSeg={2} labelDy={-7} tone="muted" />
	<Edge points={[[430, 182], [430, 228], [535, 228]]} label="yes" labelSeg={0} labelDx={16} labelDy={4} />

	<Node x={535} y={212} w={165} h={32} kind="question" title="in-place vs an ancestor?" />
	<Edge points={[[700, 228], [720, 228], [720, 166], [870, 166]]} label="yes" labelSeg={2} labelDy={-7} />
	<Edge points={[[617, 244], [617, 259], [735, 259]]} label="no" labelSeg={0} labelDx={14} labelDy={4} />

	<Node x={735} y={243} w={110} h={32} kind="question" title="aligned block?" tone="accent" />
	<Edge points={[[790, 243], [790, 228], [870, 228]]} label="yes" labelSeg={1} labelDy={-6} tone="accent" />
	<Edge points={[[790, 275], [790, 290], [870, 290]]} label="no" labelSeg={1} labelDy={14} tone="accent" />

	<Node x={870} y={20} w={170} title="excluded" sub="zeros · reported apart" tone="muted" />
	<Node x={870} y={82} w={170} title="unique" tone="muted" />
	<Node x={870} y={144} w={170} title="lineage-capturable" sub="a clone shares it free" />
	<Node x={870} y={206} w={170} title="block-capturable" sub="aligned table · R1–R3" tone="outline" />
	<Node x={870} y={268} w={170} title="CDC-only" sub="chunking only · R4" tone="accent" />
	<Note x={1040} y={334} anchor="end" tone="accent" size={10} text={['amber = cross-lineage, the headline', 'their ratio = the week-6 gate']} />
</Diagram>

<h2>The time axis</h2>
<p>
	Cross-lineage fraction is a function of time since clone.
	A freshly cloned fleet is all lineage; a fleet a year into independent update cycles is mostly
	content.
	The operator's question is when their fleet crosses over, so <mark>every leaf is computed at
	several points along each corpus's timeline</mark> and the output is a curve per corpus, not a
	number.
</p>
<p>
	For real fleets the time axis is image age since its recorded clone or install, from the
	ancestry file, aggregated across images.
	For synthetic fleets it is scripted drift at declared intervals.
	Snapshot cadence enters as a second axis on the lineage leaf: the practical COW figure is
	computed at daily, weekly, and never cadences.
</p>

<h2>The week-6 gate</h2>
<p>
	The H2 verdict is written down at the end of week 6, before any system work starts.
	<mark>If block-capturable at 4K is at least 90% of cross-lineage on the VM corpora, phase 3 is
	cancelled</mark> and the paper says why: an aligned dedup table already reaches what a
	chunk-addressed backend would, and the cost table on page 02 is the whole cost side.
	The 90% is a choice, chosen before the data arrives so it cannot move afterward.
</p>
<p>
	If the gate opens, the CDC-only residue is large enough that a system is worth building to
	price it, and the census has given that system its scope: the corpora where the residue lives
	and the chunk size that found it.
	If the gate stays closed, weeks 11 and 12 go to a third real fleet and tighter curves.
</p>

<h2>Standing claims settled as a side effect</h2>
<ul class="plain">
	<li>Whether compression captures most of dedup's win (the despairlabs position).</li>
	<li>Whether fixed blocks still approximate CDC on VM images (Jin and Miller 2009, retested at 4K and 16K).</li>
	<li>Whether whole-file dedup collapses on model corpora (ZipLLM, Xet).</li>
	<li>What fraction of observed sharing an explicit copy signal could ever have declared.</li>
	<li>How the intra-image column compares to the 2011 IBM numbers.</li>
</ul>

<h2>Pipeline</h2>
<p>
	A few thousand lines over the <code>blake3</code> and <code>fastcdc</code> crates, one binary,
	no root, runs on one node or on a donor's laptop.
	Input is a directory of images and an ancestry file (image, parent, clone date, snapshot dates).
	Output is one ndjson row per chunk and one decomposition table per corpus per time point.
	Analysis is <code>uv run</code> Python over the ndjson.
	First numbers on the synthetic controls in week 2.
</p>

<PageNav num="01" />
