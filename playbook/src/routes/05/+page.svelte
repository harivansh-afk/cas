<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
	import { Diagram, Node, Edge, Group, Note } from '$lib/components/diagram';
</script>

<PageHead num="05" />
<p class="lede">
	The lineage-versus-content split this study measures for disks reappears, unmeasured, in the
	hottest cache in computing: 

  the KV cache inside LLM serving fleets.
	This page explains the object from zero, maps the storage vocabulary onto it, reports a
	four-way literature sweep (2026-09-01), and drafts the follow-on study.
</p>

<h2>The object: what a KV cache is</h2>
<p>
	A transformer generates one token at a time, and to produce the next token, every layer's
	attention must look at every token that came before.
	The lookback needs two vectors per past token per layer, called the key and the value.
	Recomputing them for the whole history at every step would be quadratic, so the serving engine
	computes them once and keeps them: the KV cache.
	It is a pure function of the token sequence and the model weights — same model, same tokens,
	same vectors — which is what makes caching it sound at all.
</p>
<p>
	The sizes are what make it a storage problem.
	For a Llama-3-70B-shaped model (80 layers, 8 KV heads of dimension 128, fp16), one token's KV
	is 2 × 80 × 8 × 128 × 2 bytes ≈ 320 KB — arithmetic from the architecture, not a measurement.
	A 128K-token context is then ≈ 40 GB, for one request, against the ~80–140 GB an H100 or B200
	carries.
	Newer architectures shrink the constant (MLA compresses it 10–25×) but not the shape of the
	problem.
</p>
<p>
	Serving has two phases with opposite economics.
	<mark>Prefill</mark> ingests the prompt and builds its KV: compute-bound, parallel, and the
	expensive phase — on agent-heavy traffic most of the fleet's FLOPs are prefill.
	<mark>Decode</mark> emits tokens one at a time against the cached KV: memory-bandwidth-bound.
	A cache hit converts prefill compute into a copy, which is why every provider now sells cached
	input at a discount — Anthropic charges 0.1× for cache reads, DeepSeek about 0.1× via its disk
	cache.
	Prefill avoided is the entire prize in what follows.
</p>

<h2>Where it lives: the tiers</h2>
<p>
	The cache outgrew the GPU years ago; the deployed answer is a hierarchy, exactly like storage
	tiering but with a twist at the bottom of the miss path.
</p>

<Diagram
	w={960}
	h={340}
	label="Four KV cache tiers: GPU HBM holding the working set, host DRAM over PCIe, local NVMe, and a cluster-wide pool over RDMA. Data demotes downward as it cools and is fetched upward on a hit. A side panel gives the crossover rule: fetching cached KV beats recomputing prefill only when the link outruns the GPU's ability to regenerate the bytes."
	caption="The KV hierarchy. Same picture as storage tiering, except a miss does not fall through to a slower medium — the GPU recomputes the bytes from scratch, so every tier must beat a compute price. Bandwidths are vendor or paper figures, not measured here."
>
	<Node x={30} y={30} w={430} h={52} title="GPU HBM · tens of GB" sub="working set · PagedAttention blocks of 16 tokens · TB/s to compute" tone="accent" />
	<Edge points={[[245, 82], [245, 104]]} tone="muted" />
	<Node x={30} y={104} w={430} h={52} title="host DRAM · hundreds of GB" sub="pinned swap tier · LMCache, vLLM connector · PCIe gen5 x16 ≈ 64 GB/s" />
	<Edge points={[[245, 156], [245, 178]]} tone="muted" />
	<Node x={30} y={178} w={430} h={52} title="local NVMe · TBs" sub="persistence tier · ~7–14 GB/s · IMPRESS, Strata live here" />
	<Edge points={[[245, 230], [245, 252]]} tone="muted" />
	<Node x={30} y={252} w={430} h={52} title="cluster pool · everyone's DRAM + SSD" sub="Mooncake Store, Dynamo, HiCache L3 · 100 GbE ≈ 12.5 GB/s" />

	<Edge points={[[490, 270], [490, 60]]} label="fetch on hit" labelSeg={0} labelDx={44} labelDy={110} tone="muted" />
	<Note x={500} y={190} text="demote as it cools" tone="muted" />

	<Group x={640} y={30} w={300} h={274} label="the crossover rule" />
	<Note x={660} y={64} text={['a hit is worth taking only if moving the', 'bytes beats recomputing them']} />
	<Note x={660} y={112} text={['Cake (ICML ’25): H100 prefill ≈ a 32 Gbps', 'load; on slower links, recompute wins']} tone="muted" />
	<Note x={660} y={156} text={['LMCache: at 64–128 Gbps, loading wins', 'at every context length']} tone="muted" />
	<Note x={660} y={200} text={['Mooncake, per request: fetch iff', 'est. transfer < est. recompute']} tone="muted" />
	<Note x={660} y={248} text={['the twist: the bottom tier competes', 'with a GPU that can simply regenerate', 'the data']} tone="accent" />
</Diagram>

<h2>How today's caches name KV: the chain</h2>
<p>
	Every deployed system — vLLM, SGLang, Mooncake, LMCache, Dynamo — keys cached blocks the same
	way: a block's name is the hash of its tokens <em>chained with the hash of everything before
	it</em>.
	Block N's key commits to blocks 0 through N−1.
	Sharing therefore works exactly like a snapshot chain: two requests share cached KV only along
	a common prefix, and one differing token invalidates every block after it.
</p>

<Diagram
	w={960}
	h={400}
	label="Top half: two requests whose blocks are named by chained hashes. Both contain the same document, but after different preambles, so the document's blocks get different names and its KV is computed twice. Bottom half: the content-addressed alternative names the document by its own text, stores canonical KV once, and re-bases position on load into each request."
	caption="Prefix-chain naming is lineage. The shared document is this domain's apt upgrade: identical content no chain can reach, because neither copy descends from the other. Content naming reaches it, at the price of re-basing (solved) and a selective-recompute tax (open)."
>
	<Note x={30} y={30} text="PREFIX-CHAIN NAMING · IDENTITY = HISTORY · deployed everywhere" size={9.5} />
	<Note x={30} y={66} text="request A" tone="muted" />
	<Node x={110} y={48} w={130} h={30} title="system prompt" tone="muted" />
	<Node x={248} y={48} w={190} h={30} title="doc X · key h(h₁, X)" tone="outline" />
	<Node x={446} y={48} w={100} h={30} title="question" tone="muted" />
	<Note x={30} y={112} text="request B" tone="muted" />
	<Node x={110} y={94} w={160} h={30} title="different preamble" tone="muted" />
	<Node x={278} y={94} w={190} h={30} title="doc X · key h(h₁′, X)" tone="outline" />
	<Node x={476} y={94} w={100} h={30} title="question" tone="muted" />
	<Note x={614} y={62} text={['same document, different chain →', 'different keys → prefilled twice']} tone="accent" />
	<Note x={614} y={104} text={['the snapshot chain again: shares what', 'descends, blind to what became equal']} tone="muted" />

	<Edge points={[[30, 160], [930, 160]]} arrow={false} tone="muted" dashed />

	<Note x={30} y={190} text="CONTENT NAMING · IDENTITY = THE TEXT · the research frontier (“PIC”)" size={9.5} />
	<Node x={110} y={210} w={240} h={40} title="doc X · key h(X)" sub="canonical KV · keys stored unrotated" tone="accent" />
	<Edge points={[[190, 250], [150, 300]]} tone="muted" />
	<Edge points={[[290, 250], [420, 300]]} tone="muted" />
	<Node x={40} y={300} w={220} h={30} title="request A · re-based to pos 812" tone="muted" />
	<Node x={310} y={300} w={220} h={30} title="request B · re-based to pos 96" tone="muted" />
	<Note x={614} y={214} text="two costs, both named in the sweep:" />
	<Note x={614} y={240} text={['1 · re-base keys to the new position:', 'solved exactly — RoPE composes', '(MiniPIC, MEPIC, in-kernel)']} tone="muted" />
	<Note x={614} y={296} text={['2 · KV also depends on preceding tokens:', 'a fraction r is selectively recomputed —', 'heuristic, no error bounds · the open half']} tone="accent" />
</Diagram>

<p>
	Why the chain exists at all, and why naive content addressing fails: a token's KV is not a
	function of that token alone.
	It depends on absolute position (RoPE rotates the key by an angle proportional to position) and
	on every preceding token (attention mixes them in).
	Identical text at two positions produces different KV bytes, so hashing the KV bytes finds
	nothing, and hashing the text alone names an object whose bytes are context-dependent.
	The chain is the conservative fix — name the whole history, guaranteeing exactness.
	The research question is how much cheaper the liberal fix can get.
</p>

<h2>State of the field, swept 2026-09-01</h2>
<p>
	Four-way sweep: substrate durability, position-independent caching, pooled stores, and
	measurement studies. Condensed verdicts, sources linked.
</p>

<div class="table-scroll">
	<table class="spec">
		<thead>
			<tr><th>Question</th><th>State</th><th>Key evidence</th></tr>
		</thead>
		<tbody>
			<tr>
				<td class="k">will KV still matter in 2029?</td>
				<td>yes — safe past 2029 at block granularity</td>
				<td>of ~10 notable frontier open releases Jan–Feb '26, ~7 keep per-token KV in every layer; <a href="https://www.minimax.io/news/why-did-m2-end-up-as-a-full-attention-model">MiniMax reverted</a> from linear attention to full attention citing production cache-hit rates; MLA (DeepSeek/Kimi lineage) keeps a per-token, prefix-deterministic cache. Hedge: the Qwen linear-hybrid camp keeps full KV in only ~25% of layers</td>
			</tr>
			<tr>
				<td class="k">can KV be made position-independent?</td>
				<td>mechanism solved; quality open</td>
				<td>"PIC" is a named subfield (<a href="https://arxiv.org/abs/2410.15332">EPIC</a>, ICML '25); storing unrotated keys and rotating in-kernel ships in &lt;100 LOC on vLLM (<a href="https://arxiv.org/abs/2606.13126">MiniPIC</a>, IBM '26; <a href="https://arxiv.org/abs/2512.16822">MEPIC</a>); selective recompute (<a href="https://arxiv.org/abs/2405.16444">CacheBlend</a>, EuroSys '25 best paper, r≈15%) is heuristic — a <a href="https://arxiv.org/html/2603.20218">2026 comparative study</a> finds 7–18% F1 below full prefill on multi-hop at that r. No error bounds exist anywhere</td>
			</tr>
			<tr>
				<td class="k">does any pool place KV by content?</td>
				<td>no — directories and prefix chains everywhere</td>
				<td><a href="https://arxiv.org/abs/2407.00079">Mooncake</a>, <a href="https://arxiv.org/abs/2510.09665">LMCache</a>, Dynamo, HiCache all: prefix-chain names, central metadata/scheduler placement; a <a href="https://arxiv.org/abs/2607.02574">survey of 30+ systems</a> confirms, and calls eviction "described functionally but rarely measured". Only <a href="https://arxiv.org/abs/2608.21362">KVBoost</a> (preprint) keys by content hash; <a href="https://arxiv.org/abs/2607.19957">HijackKV</a> already documents the poisoning risk a cross-user content pool invites</td>
			</tr>
			<tr>
				<td class="k">has anyone measured the split?</td>
				<td><mark>no — the open gap</mark></td>
				<td>Alibaba's <a href="https://arxiv.org/abs/2506.02634">KVCache in the Wild</a> (ATC '25) is prefix-only: 62%/54% infinite-capacity hit rates, 97% of API hits from single-turn templates, cross-user reuse negligible. An <a href="https://arxiv.org/abs/2608.15127">agentic-workload study</a> (Aug '26) finds 27% of distinct search queries cover 67% of invocations — tool-level, never translated to KV terms. Nobody decomposes prefix vs non-prefix vs semantic reuse on one trace, or prices the tax</td>
			</tr>
		</tbody>
	</table>
</div>

<p>
	Read as one sentence: <mark>the substrate is durable, the reuse mechanism is built, the pools
	that would exploit it are built, and nobody has measured whether the reachable redundancy is
	worth any of it</mark>.
	That is the storage study's situation transposed — mechanisms shipped by two communities
	(serving engines with prefix chains, PIC papers with chunk reuse), and the deciding measurement
	missing between them.
</p>

<h2>The proposed sequel: a KV reuse census</h2>
<p>
	Take real traces. For every token span, ask the question the disk census (page 01) asks of
	every byte range — with one extra leaf, because approximate reuse exists here and never does
	on disk.
</p>

<Diagram
	w={960}
	h={360}
	label="The KV census decision flow. A token span is first tested against earlier requests' prefixes; a prefix match is already captured by today's caches. Otherwise, if the exact text appeared elsewhere at any position, it is non-prefix exact, reachable by position-independent caching at a recompute tax. Otherwise, if it is a near-duplicate of earlier text, it is semantic-only, reachable by approximate reuse at an accuracy cost. Otherwise it is unique and must be prefilled."
	caption="The KV census, per token span. Prefix-reachable is lineage-capturable; non-prefix exact is block-capturable; semantic-only is a leaf disks do not have. The headline is the size of the two middle leaves and the tax r they carry."
>
	<Node x={40} y={44} w={190} h={36} title="token span in a request" />
	<Edge points={[[230, 62], [310, 62]]} />
	<Node x={310} y={46} w={260} h={32} kind="question" title="prefix of an earlier request?" tone="accent" />
	<Edge points={[[570, 62], [650, 62]]} label="yes" labelDy={-8} />
	<Node x={650} y={40} w={280} h={44} title="prefix-reachable" sub="today's caches already get it" tone="muted" />
	<Edge points={[[440, 78], [440, 130]]} label="no" labelSeg={0} labelDx={16} labelDy={6} />
	<Node x={310} y={130} w={260} h={32} kind="question" title="same text elsewhere, any position?" tone="accent" />
	<Edge points={[[570, 146], [650, 146]]} label="yes" labelDy={-8} tone="accent" />
	<Node x={650} y={122} w={280} h={52} title="non-prefix exact" sub="PIC reaches it · pays recompute tax r" tone="accent" />
	<Edge points={[[440, 162], [440, 214]]} label="no" labelSeg={0} labelDx={16} labelDy={6} />
	<Node x={310} y={214} w={260} h={32} kind="question" title="near-duplicate of earlier text?" tone="accent" />
	<Edge points={[[570, 230], [650, 230]]} label="yes" labelDy={-8} tone="accent" />
	<Node x={650} y={206} w={280} h={52} title="semantic-only" sub="approximate reuse · pays accuracy" tone="outline" />
	<Edge points={[[440, 246], [440, 298]]} label="no" labelSeg={0} labelDx={16} labelDy={6} />
	<Node x={310} y={298} w={260} h={40} title="unique · must be prefilled" tone="muted" />
</Diagram>

<p>
	Two deliverables.
	The <strong>curve</strong>: reachable prefill savings per mechanism, per workload class (chat,
	RAG, coding agents), against cache capacity — the number every PIC paper currently replaces
	with a benchmark.
	The <strong>priced tax</strong>: a drift-versus-recompute sweep (output quality against r) on
	one 7B-class model, runnable on spark's GB10, turning "PIC reaches it" into "PIC reaches it at
	r=15% and this measured quality delta".
	Method, decomposition discipline, and gate structure port verbatim from pages 01–03; the fall
	study is the training run for this one.
</p>
<p>
	The binding constraint is trace fidelity, not compute:
</p>
<div class="table-scroll">
	<table class="spec">
		<thead>
			<tr><th>Trace</th><th>Content fidelity</th><th>Usable for</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">WildChat-1M · LMSYS-Chat-1M</td><td>full text</td><td>all three reuse leaves; chat-shaped only</td></tr>
			<tr><td class="k"><a href="https://arxiv.org/abs/2606.30560">TraceLab</a> coding-agent sessions</td><td>full text, ~4.3K sessions</td><td>the agentic class, where the non-prefix pool should be largest</td></tr>
			<tr><td class="k"><a href="https://github.com/alibaba-edu/qwen-bailian-usagetraces-anon">Alibaba qwen-bailian</a></td><td>salted per-token hashes</td><td>exact-match leaves at sub-prefix granularity (verify against the release); semantic leaf impossible</td></tr>
			<tr><td class="k">Mooncake open trace</td><td>prefix-chained block hashes</td><td>prefix leaf only — non-prefix reuse is invisible <em>by construction</em>, which is itself the point</td></tr>
			<tr><td class="k">Azure LLM traces · BurstGPT</td><td>lengths and timings</td><td>nothing here</td></tr>
		</tbody>
	</table>
</div>

<h2>Upside, window, and verdict</h2>
<p>
	Upside. Prefill dominates fleet cost on agentic traffic; measured prefix ceilings sit at
	50–62%; providers price a hit at 0.1×.
	If the census shows another 10–20 points reachable at a tolerable tax — an estimate, and
	exactly the number the census exists to produce — it becomes the motivation citation for the
	entire PIC subfield and tells Mooncake-class operators what to build next.
	If it shows 3%, the field learns prefix caching was already enough, which is the same honest
	negative the disk study is built to survive.
</p>
<p>
	Window. Months, not years: Alibaba has the data and is one measurement away; the
	agentic-workload group is one translation away. Call it even odds of a partial scoop by
	mid-2027 — a gut number.
	Mitigations: the full decomposition with a priced tax is a bigger claim than either group's
	natural next step, and the sweep surfaced a ready fallback (reuse-distance and eviction
	characterization of pooled KV, which the survey itself flags as unmeasured).
</p>
<p>
	What is deliberately not proposed: building the content-addressed KV pool.
	The mechanism layer is already crowded (MiniPIC made re-basing a 100-line patch), the quality
	layer is an ML-evals problem, the deployment layer needs a GPU fleet and an answer to
	HijackKV, and the field publishes weekly where storage publishes yearly.
	A solo 14-week effort competes there and loses; a measurement no one has done competes with
	nobody.
</p>
<p>
	Sequencing. Fall: the disk study, as specced, untouched.
	December: this page grows into a proposal; the weekend de-risk (one 7B model, one r-sweep, one
	plot) runs on spark.
	Spring: the census on content-bearing traces.
	One paragraph in the fall paper's future work stakes the frame: <mark>caches keyed by history
	miss what became equal, whether the pointer is a block address or an attention state</mark> —
	measured for disks here, for KV next.
</p>

<h2>Terms this page adds</h2>
<dl class="terms">
	<dt id="term-kv-cache">KV cache</dt>
	<dd>
		Per-token key and value vectors kept per layer so attention never recomputes the past.
		A pure function of (model, token sequence); ~320 KB per token for a 70B fp16 model, from
		arithmetic.
	</dd>
	<dt id="term-prefill">prefill / decode</dt>
	<dd>
		Prefill builds the prompt's KV (compute-bound, the dominant fleet cost on agent traffic);
		decode generates tokens against it (bandwidth-bound). A cache hit converts prefill into a
		copy — or skips even that, if the KV is already resident.
	</dd>
	<dt id="term-pic">position-independent caching (PIC)</dt>
	<dd>
		Reusing a text chunk's KV at a different position or after a different prefix: exact
		re-basing of RoPE rotations plus selective recomputation of a token fraction r to patch
		context dependence. Named by EPIC (ICML '25); mechanism shipped, quality bounds open.
	</dd>
	<dt id="term-recompute-tax">recompute tax</dt>
	<dd>
		The fraction r of a reused chunk's tokens that must be recomputed for acceptable output,
		plus the quality delta at that r. The KV analogue of the disk study's write amplification:
		the price of capture, and unpriced in the literature.
	</dd>
</dl>

<style>
	dl.terms dt {
		scroll-margin-top: 1.5rem;
		font-weight: var(--weight-strong);
		color: var(--text-primary);
		text-decoration: underline dotted;
		text-decoration-color: color-mix(in srgb, #d97706 55%, transparent);
		text-decoration-thickness: 1px;
		text-underline-offset: 0.2em;
	}
	dl.terms dd {
		margin: 0.25rem 0 0.875rem;
		color: var(--text-secondary);
	}
</style>

<PageNav num="05" />
