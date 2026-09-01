<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
	import { Diagram, Node, Edge, Group, Note } from '$lib/components/diagram';
</script>

<PageHead num="05" />
<p class="lede">
	The lineage-versus-content split this study measures for disks reappears in the KV cache inside
	LLM serving fleets.
	The first draft of this page (2026-09-01, morning) called the split unmeasured there and
	proposed a census.
	A second sweep the same day found the census already run, in May 2026, on Claude Code traces,
	with a small result.
	<mark>This page now records what that work found, the architecture it used, why a cynic reads
	it the way this page does, and the narrower questions it left open.</mark>
</p>

<h2>The object: what a KV cache is</h2>
<p>
	A transformer generates one token at a time, and to produce the next token, every layer's
	attention must look at every token that came before.
	The lookback needs two vectors per past token per layer, called the key and the value.
	Recomputing them for the whole history at every step would be quadratic, so the serving engine
	computes them once and keeps them: the KV cache.
	It is a pure function of the token sequence and the model weights, which is what makes caching
	it sound at all.
</p>
<p>
	The sizes are what make it a storage problem.
	For a Llama-3-70B-shaped model (80 layers, 8 KV heads of dimension 128, fp16), one token's KV
	is 2 × 80 × 8 × 128 × 2 bytes ≈ 320 KB, arithmetic from the architecture rather than a
	measurement.
	A 128K-token context is then ≈ 40 GB, for one request, against the ~80–140 GB an H100 or B200
	carries.
	Multi-head latent attention (MLA, the DeepSeek and Kimi lineage) compresses the row 10–25×
	and, as it turns out below, changes which caching designs are sound.
</p>
<p>
	Serving has two phases with opposite economics.
	<mark>Prefill</mark> ingests the prompt and builds its KV: compute-bound, parallel, and the
	expensive phase; on agent-heavy traffic most of the fleet's FLOPs are prefill.
	<mark>Decode</mark> emits tokens one at a time against the cached KV: memory-bandwidth-bound.
	A cache hit converts prefill compute into a copy, which is why every provider sells cached
	input at a discount: Anthropic charges 0.1× for cache reads, DeepSeek's disk cache served 56.3%
	of its input tokens in February 2025.
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
	caption="The KV hierarchy. Same picture as storage tiering, except a miss does not fall through to a slower medium: the GPU recomputes the bytes from scratch, so every tier must beat a compute price. Bandwidths are vendor or paper figures, not measured here."
>
	<Node x={30} y={30} w={430} h={52} title="GPU HBM · tens of GB" sub="working set · PagedAttention blocks of 16 tokens · TB/s to compute" tone="accent" />
	<Edge points={[[245, 82], [245, 104]]} tone="muted" />
	<Node x={30} y={104} w={430} h={52} title="host DRAM · hundreds of GB" sub="pinned swap tier · LMCache, vLLM connector · PCIe gen5 x16 ≈ 64 GB/s" />
	<Edge points={[[245, 156], [245, 178]]} tone="muted" />
	<Node x={30} y={178} w={430} h={52} title="local NVMe · TBs" sub="persistence tier · ~7–14 GB/s · IMPRESS, Strata live here" />
	<Edge points={[[245, 230], [245, 252]]} tone="muted" />
	<Node x={30} y={252} w={430} h={52} title="cluster pool · everyone's DRAM + SSD" sub="Mooncake Store, Dynamo KVBM, HiCache L3 · 100 GbE ≈ 12.5 GB/s" />

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
	Every deployed system keys cached blocks the same way: a block's name is the hash of its tokens
	<em>chained with the hash of everything before it</em>.
	vLLM hashes <code>(parent_hash, block_tokens, extra_keys)</code>; SGLang chains SHA-256 page
	digests; LMCache, Mooncake, and Dynamo inherit or reimplement the same rule, then namespace the
	key by model, tensor-parallel rank, and LoRA.
	Block N's key commits to blocks 0 through N−1.
	Sharing therefore works exactly like a snapshot chain: two requests share cached KV only along
	a common prefix, and one differing token invalidates every block after it.
</p>

<Diagram
	w={960}
	h={400}
	label="Top half: two requests whose blocks are named by chained hashes. Both contain the same document, but after different preambles, so the document's blocks get different names and its KV is computed twice. Bottom half: the content-addressed alternative names the document by its own text, stores canonical KV once, and re-bases position on load into each request."
	caption="Prefix-chain naming is lineage. The shared document is this domain's apt upgrade: identical content no chain can reach, because neither copy descends from the other. Content naming reaches it, at the price of re-basing (solved) and a context-dependence tax (open for GQA, mostly absent for MLA)."
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

	<Note x={30} y={190} text="CONTENT NAMING · IDENTITY = THE TEXT · position-independent caching (“PIC”)" size={9.5} />
	<Node x={110} y={210} w={240} h={40} title="doc X · key h(X)" sub="canonical KV · keys stored unrotated" tone="accent" />
	<Edge points={[[190, 250], [150, 300]]} tone="muted" />
	<Edge points={[[290, 250], [420, 300]]} tone="muted" />
	<Node x={40} y={300} w={220} h={30} title="request A · re-based to pos 812" tone="muted" />
	<Node x={310} y={300} w={220} h={30} title="request B · re-based to pos 96" tone="muted" />
	<Note x={614} y={214} text="two costs, both named in the sweep:" />
	<Note x={614} y={240} text={['1 · re-base keys to the new position:', 'solved exactly — RoPE composes', '(MiniPIC, MEPIC, Irminsul δ-rotation)']} tone="muted" />
	<Note x={614} y={296} text={['2 · KV also depends on preceding tokens:', 'GQA recomputes a fraction r, heuristic,', 'no error bound · MLA mostly sidesteps it']} tone="accent" />
</Diagram>

<p>
	Why the chain exists at all, and why naive content addressing fails: a token's KV is not a
	function of that token alone.
	It depends on absolute position (RoPE rotates the key by an angle proportional to position) and
	on every preceding token (attention mixes them in).
	Identical text at two positions produces different KV bytes, so hashing the KV bytes finds
	nothing, and hashing the text alone names an object whose bytes are context-dependent.
	The chain is the conservative fix: name the whole history, guaranteeing exactness.
	Every PIC paper is a bet on how much cheaper the liberal fix can get.
</p>

<h2>State of the field, swept 2026-09-01</h2>
<p>
	Two sweeps in one day.
	The morning sweep found the mechanism built and the measurement missing.
	The afternoon sweep, run with three parallel literature agents against arXiv and the engine
	repositories, found the measurement.
	Condensed verdicts, sources linked.
</p>

<div class="table-scroll">
	<table class="spec">
		<thead>
			<tr><th>Question</th><th>State</th><th>Key evidence</th></tr>
		</thead>
		<tbody>
			<tr>
				<td class="k">will KV still matter in 2029?</td>
				<td>yes, with an architecture caveat</td>
				<td>of ~10 notable frontier open releases Jan–Feb '26, ~7 keep per-token KV in every layer; <a href="https://www.minimax.io/news/why-did-m2-end-up-as-a-full-attention-model">MiniMax reverted</a> from linear attention to full attention citing production cache-hit rates. Caveat from below: hybrid SSM/linear models (Mamba2, GDN, KDA) save 0% prefill energy on a cache hit, so the Qwen linear-hybrid camp is out of scope for any KV cache design</td>
			</tr>
			<tr>
				<td class="k">can KV be made position-independent?</td>
				<td>solved for MLA; heuristic for GQA</td>
				<td>"PIC" is a named subfield (<a href="https://arxiv.org/abs/2410.15332">EPIC</a>, ICML '25). For GQA, <a href="https://arxiv.org/abs/2405.16444">CacheBlend</a> (EuroSys '25 best paper) recomputes r≈15% and an <a href="https://arxiv.org/html/2603.20218">independent 2026 comparison</a> of 11 methods puts it 7–18% F1 below full prefill on multi-hop; no method has an error bound. For MLA, <a href="https://arxiv.org/abs/2605.05696">Irminsul</a> (May '26) shows 89% of the KV row is position-free by construction and the remaining 64-dim slice rotates exactly (4.7 × 10⁻³ rel-L2 in bf16)</td>
			</tr>
			<tr>
				<td class="k">does any pool place KV by content?</td>
				<td>built in research, not deployed</td>
				<td><a href="https://arxiv.org/abs/2407.00079">Mooncake</a>, <a href="https://arxiv.org/abs/2510.09665">LMCache</a>, Dynamo, HiCache, Huawei EMS, ObjectCache: prefix-chain names everywhere in production. Content-only keys exist in LMCache's blend mode, <a href="https://arxiv.org/abs/2608.21362">KVBoost</a> (one RTX 4060, in-process dict), and Irminsul (SGLang radix cache, no public code). No paper measures a content-keyed, deduplicated, multi-host pool end to end</td>
			</tr>
			<tr>
				<td class="k">does any of it ship?</td>
				<td>no</td>
				<td>Nothing in stock vLLM or SGLang. vLLM's connector API is prefix-shaped (<code>get_num_new_matched_tokens</code> returns a count), so non-prefix reuse cannot live out of tree; <a href="https://github.com/vllm-project/vllm/issues/25950">RFC #25950</a> closed not-planned May '26, <a href="https://github.com/vllm-project/vllm/issues/44223">RFC #44223</a> open. LMCache's CacheBlend server is open source but its vLLM connector plugin is in a private repository. SGLang's PIC request (<a href="https://github.com/sgl-project/sglang/issues/30785">#30785</a>) has zero replies</td>
			</tr>
			<tr>
				<td class="k">is a cross-user content pool safe?</td>
				<td>no, and no cheap fix</td>
				<td><a href="https://arxiv.org/abs/2607.19957">HijackKV</a> (USENIX Security '26): poison a shared chunk through its prefix, 94% targeted success, survives 50% recompute; only 60–80% recompute drives it below 30%, which erases the savings. <a href="https://arxiv.org/abs/2606.21842">SpliceLeak</a>: timing side channel on fused non-prefix KV. <a href="https://arxiv.org/abs/2502.07776">Gu et al.</a> (ICML '25) audited 7 providers sharing prompt caches across users. Prefix chaining defends by accident, because the chain includes the system prompt</td>
			</tr>
			<tr>
				<td class="k">has anyone measured the split?</td>
				<td><mark>yes, once, on benchmark traces</mark></td>
				<td>Irminsul §3 decomposes 7,530 agent trajectories (3.4 × 10⁸ tokens) into prefix / same-bytes-shifted / novel. Detailed below. Production trace studies (<a href="https://arxiv.org/abs/2606.30560">TraceLab</a>, <a href="https://arxiv.org/abs/2608.00101">Copilot at scale</a>, <a href="https://arxiv.org/abs/2506.02634">KVCache in the Wild</a>) remain prefix-only. The semantic leaf has never been measured on any trace</td>
			</tr>
		</tbody>
	</table>
</div>

<h2>The measurement that exists: Irminsul</h2>
<p>
	<a href="https://arxiv.org/abs/2605.05696">Irminsul: MLA-Native Position-Independent Caching
	for Agentic LLM Serving</a> (Erlangen NHR, arXiv 2605.05696, 7 May 2026) is the disk study's
	frame applied to KV, four months before this page was drafted: content-hash keys over
	content-defined chunks, an offline census before building anything, and a "if the reuse fraction
	is small, the whole premise collapses" sentence in its own introduction.
	Its numbers, verified against the paper text:
</p>
<div class="table-scroll">
	<table class="spec">
		<thead>
			<tr><th>Corpus</th><th>Same bytes at a shifted position</th><th>Their reading</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">CC-Bench (Claude Code, 260 trajectories)</td><td>18.8% within-session · 4.6% cross-session</td><td>"correctly predicts minimal PIC ROI for code-editing deployments"</td></tr>
			<tr><td class="k">Toolathlon (tool use)</td><td>p95 48% within-session</td><td>the one corpus where the pool is large</td></tr>
			<tr><td class="k">hermes-agent-traces</td><td>within the 12–24% cross-session band</td><td>not broken out in the prose</td></tr>
			<tr><td class="k">all three</td><td>12–24% of unique content cross-session cacheable</td><td>"a lower bound on any single deployment"</td></tr>
		</tbody>
	</table>
</div>
<p>
	Method: every turn's tokens are fingerprinted with a 64-token sliding-window xxHash and each
	window is classed as prefix (exact match, served by SGLang today), PIC-cacheable (same bytes at
	a shifted position), or novel.
	The denominator is "unique content", which the prose does not fully define; the corpora are
	public benchmark trajectories, not production traffic; medians are not stated.
	Three findings beyond the headline are worth keeping because they constrain any successor.
</p>
<ul>
	<li>
		<strong>Fixed-block hashing finds almost none of it.</strong> Agentic messages start at
		arbitrary byte offsets, so SGLang-style aligned windows miss shifted content. Gear-hash CDC
		over tokens (expected chunk ~128 tokens, clamped to [32, 512]) plus a 128-token fixed-window
		fallback recovers 3.8–4.8× more. This is page 01's block-capturable versus CDC-only split,
		reproduced on token streams, and here CDC wins decisively.
	</li>
	<li>
		<strong>Whether stored KV is reusable is an architecture property, not a method property.</strong>
		A threshold-free ROC test (same content at five absolute positions versus random content) gives
		MLA's 512-dim latent <code>cKV</code> an AUC of 0.77 and its 64-dim rotated key <code>kr</code>
		0.43. GQA's whole key scores 0.45 and MHA's 0.40: below random, meaning naive reuse is worse
		than nothing and every hit pays a correction proportional to the full key width. Value vectors
		score 0.71–0.76 everywhere because they are never rotated.
	</li>
	<li>
		<strong>The prize varies 0–86% by architecture.</strong> Measured on stock SGLang with NVML
		counters, a prefix hit costs 14% of a miss's prefill energy on GQA (Qwen3-32B), 37% on MLA, 34%
		on MHA. Hybrid SSM/linear models (Nemotron-3-Nano, Qwen3.6-35B-A3B, Kimi-Linear) save 0%:
		the recurrent state neither appends nor forks, so a "hit" is not a defined operation at those
		layers. Partial recompute at 50% sits close to a miss on the smaller models, where fixed
		kernel-launch and routing cost dominates; the benefit concentrates in the full-hit case.
	</li>
</ul>

<h2>The architecture they went with</h2>
<Diagram
	w={960}
	h={380}
	label="Irminsul serve path. An incoming prompt first takes a standard exact-prefix hit from the radix cache. The remaining tail is chunked by content-defined chunking over tokens and each chunk is fingerprinted. Chunks at absolute position below 32 are prefilled normally to avoid the attention-sink regime. Other chunks are looked up by content hash in a registry shared across sessions; a hit returns the position-free latent plus a base rotated key, which is rotated by the position delta inside the attention kernel. A miss prefills the chunk and inserts it into the registry."
	caption="Irminsul's hot path, MLA only. The registry is a content-addressed store shared across sessions; the per-request part is a 64-dim rotation. A 64-token boundary marker placed by the prompt assembler pins CDC boundaries so identical content chunks identically regardless of what precedes it; without a cooperating assembler, the offline CDC-plus-fallback probe is the recovery path."
>
	<Node x={30} y={40} w={170} h={40} title="prompt tokens T" />
	<Edge points={[[200, 60], [260, 60]]} />
	<Node x={260} y={40} w={200} h={40} title="exact-prefix match" sub="stock RadixCache" tone="muted" />
	<Edge points={[[460, 60], [520, 60]]} label="tail" labelDy={-8} />
	<Node x={520} y={40} w={200} h={40} title="CDC over tokens" sub="Gear hash · ~128-tok chunks · xxHash64" tone="accent" />
	<Edge points={[[620, 80], [620, 130]]} label="per chunk at position p" labelSeg={0} labelDx={14} labelDy={6} />
	<Node x={490} y={130} w={260} h={32} kind="question" title="p < 32 ?" tone="accent" />
	<Edge points={[[750, 146], [800, 146]]} label="yes" labelDy={-8} />
	<Node x={800} y={126} w={140} h={44} title="ordinary prefill" sub="sink carve-out" tone="muted" />
	<Edge points={[[620, 162], [620, 214]]} label="no" labelSeg={0} labelDx={14} labelDy={6} />
	<Node x={490} y={214} w={260} h={32} kind="question" title="registry[xxhash(chunk)] hit?" tone="accent" />
	<Edge points={[[750, 230], [800, 230]]} label="yes" labelDy={-8} tone="accent" />
	<Node x={800} y={206} w={140} h={52} title="reuse cKV" sub="kr ← R(δ)·kr_base" tone="accent" />
	<Edge points={[[620, 246], [620, 300]]} label="no" labelSeg={0} labelDx={14} labelDy={6} />
	<Node x={490} y={300} w={260} h={44} title="prefill chunk · insert into registry" sub="(cKV, kr_base, p_src)" tone="muted" />

	<Group x={30} y={130} w={400} h={214} label="the registry" />
	<Note x={50} y={164} text={['key: xxHash64 of the chunk’s token IDs', 'value: 512-dim cKV (position-free) +', '64-dim kr at its source position']} />
	<Note x={50} y={224} text={['shared across sessions; per-request', 'state is only the 64-dim rotation,', 'fused into FlashMLA’s load path']} tone="muted" />
	<Note x={50} y={282} text={['scaffold: split TokenToKVPool, ~950 LoC', 'on SGLang · no public code found']} tone="accent" />
</Diagram>

<p>
	Quality evidence.
	On HotpotQA and MuSiQue across DeepSeek-V2-Lite, Moonlight-16B-A3B, and JoyAI-Flash 48B/3B,
	task F1 stays within Wilson SEM of full recompute, which the authors concede is too coarse to
	discriminate PIC from naive reuse on sparse extraction tasks.
	The load-bearing evidence is teacher-forced per-token KL against full prefill and the greedy
	first-divergence position: PIC matches or beats naive reuse on every populated cell except one,
	and stays identical to full prefill for up to 2.4× more tokens.
	The attention sink turns out to be sequence-start-local, not chunk-start-local, so only the
	first 32 absolute positions need ordinary prefill and later chunks need no boundary recompute
	(EPIC assumed otherwise).
	On synthetic agent-metadata rotation workloads the runtime recovers ~83% of prompt tokens above
	exact-prefix and saves 63% of prefill energy per hit.
	Those runtime numbers are not on the traces from §3.
</p>

<h2>Where prefix caching already stands</h2>
<p>
	The census number has to be multiplied by what is left after the deployed mechanism, and the
	deployed mechanism is nearly saturated on coding agents.
	<a href="https://arxiv.org/abs/2606.30560">TraceLab</a> (4,300 Claude Code and Codex sessions,
	provider-reported) measures a 95.7% token-weighted cache hit rate; fresh tokens are 19.0% of
	appended tokens.
	Microsoft's <a href="https://arxiv.org/abs/2608.00101">Copilot study</a> (761M calls) reports
	~90% within a turn and 55% across turn boundaries.
	The misses are not position shifts: they are idle gaps (past ten minutes idle, hit rate falls
	to 0–5%), context compaction (median drop 66 points), and model switches.
	Multiply out: PIC contends for a slice of the remaining ~5–10%, and Irminsul says roughly a
	fifth of coding-agent content in that slice is reachable.
	Call it one to two points of prefill on coding traffic, a product of published numbers rather
	than a measurement, and small either way.
</p>
<p>
	The number that breaks the pattern is elsewhere.
	The HKUST/Alibaba <a href="https://arxiv.org/abs/2608.15127">agentic characterization</a>
	(Aug '26, 35,037 production sessions) finds static-prompt apps like Claude Code at up to 99%
	prefix hit and dynamic-context apps like DeepResearch at ≤1%, because context is restructured
	and reordered every turn.
	That workload class is where shifted-position reuse could be large, and nobody has decomposed
	it.
</p>

<h2>Reasons to be cynical</h2>
<p>
	Each of these is a place the storage study's instincts apply, and each is a reason not to take
	a PIC headline at face value.
</p>
<ul>
	<li>
		<strong>The mechanism is architecture-bound.</strong> Irminsul's safety claim covers MLA
		only. On GQA (Llama, Qwen dense, Mistral) the key is below random on the ROC test and the
		correction is 16× wider; the training-free methods that exist there lose 7–18% F1. On hybrid
		SSM models the prize is zero. Any KV cache proposal should say which architecture it is for
		in its first sentence.
	</li>
	<li>
		<strong>Benchmark trajectories are not a fleet.</strong> CC-Bench, Toolathlon, and Hermes
		traces are heterogeneous public runs. Cross-session reuse on one team's repo, or one
		operator's fleet, is unmeasured. Irminsul calls its numbers a lower bound; a lower bound on a
		small number is still small until shown otherwise.
	</li>
	<li>
		<strong>The quality metric could not discriminate.</strong> F1 on sparse QA sat within noise
		for both PIC and naive reuse. KL and divergence position are better instruments but are not a
		bound, and no PIC method has one.
	</li>
	<li>
		<strong>Energy savings live in the full-hit case.</strong> 50% partial recompute lands close
		to a miss on 16B-class models because per-prefill fixed costs dominate. A census that counts
		partially reusable chunks as wins overstates the prize.
	</li>
	<li>
		<strong>Boundaries drift unless the client cooperates.</strong> CDC state depends on the
		preceding bytes; identical content chunks differently after different prefixes. Irminsul's fix
		is a 64-token marker inserted by the prompt assembler, which an API provider can do and a
		third-party client cannot. Tokenizer merges at span edges (LMCache issue #2026: the same chunk
		measuring 468 versus 550 tokens) are the same problem one layer down.
	</li>
	<li>
		<strong>The pool is an attack surface.</strong> A content-keyed cross-user registry is the
		exact design HijackKV targets. Prefix chains defend by accident; content keys need a
		replacement for that defense, and the recompute ratio that restores safety erases the saving.
	</li>
	<li>
		<strong>The engine API is the moat.</strong> vLLM's scheduler asks a connector how many prefix
		tokens matched; there is no way to say "these three spans in the middle." Two RFCs and one PR
		went stale. Shipping PIC means changing the engine's contract, and the maintainers have not
		agreed to.
	</li>
	<li>
		<strong>No public code.</strong> Irminsul, MEPIC, MiniPIC (IBM fork), and LMCache's vLLM
		plugin are all unreleased or private. Every reported speedup is unreproduced.
	</li>
</ul>

<h2>What is still open</h2>
<p>
	The frame survives; the coding-agent instance of it does not.
	Five questions remain unmeasured, in rough order of how much they would change a decision.
</p>
<ul>
	<li>
		<strong>The dynamic-context class.</strong> Research agents, multi-agent pipelines, and RAG
		with reranking reorder context every turn and drop prefix caching to ~1%. The
		prefix / shifted / semantic / novel decomposition on that traffic is the census page 05
		originally proposed, restricted to the one workload where the answer could be large.
	</li>
	<li>
		<strong>Production cross-session reuse.</strong> The same repo, the same tool schemas, the
		same documents across a fleet of users over weeks. Irminsul measured benchmarks; nobody has
		measured a deployment. Access to traces is the constraint, and the disk study's donor
		protocol is the template.
	</li>
	<li>
		<strong>The semantic leaf.</strong> Near-duplicate text (a file with one line changed, a
		document re-fetched with a new timestamp) has never been counted on any trace. KVShare and
		SemBlend build for it without measuring it.
	</li>
	<li>
		<strong>A quality bound.</strong> Every method reports a benchmark delta. An analytic or
		empirical bound on output divergence as a function of chunk length, position delta, and
		recompute fraction, per architecture, would let an operator set r instead of guessing it.
		This is an ML-evals problem; the GB10 can run a 7B-class sweep, and for MLA models the
		δ-rotation error term is already known.
	</li>
	<li>
		<strong>A safe content pool.</strong> Something between per-tenant prefix salting (which
		forfeits cross-user reuse entirely) and an open registry (which HijackKV owns). Publisher-signed
		document KV (<a href="https://arxiv.org/abs/2606.13361">"Can I Buy Your KV Cache?"</a>, which
		prices reuse at 9–50× cheaper than prefill on Qwen3-4B) is one framing: trust the source of the
		chunk, not the requester.
	</li>
</ul>
<p>
	Two of these are trace-access problems, one is evals, one is security, one is a mix.
	None is a storage problem, and the field publishes weekly where storage publishes yearly.
</p>

<h2>The kill test</h2>
<p>
	Before any of the above becomes a proposal, one offline experiment on spark decides whether
	there is a number worth chasing on traffic this author can actually see.
	Log full prompts from Hermes and from Claude Code through a local proxy for two weeks.
	Tokenize once, run Irminsul's decomposition (64-token sliding xxHash; prefix hit, seen
	elsewhere at a different offset, novel) and its CDC-plus-fallback probe.
	Report the shifted-position fraction per workload class and per session, with the same
	four-leaf discipline as page 01 (G1: leaves sum to 100% of tokens).
	No GPU, no engine, a few hundred lines.
	<mark>If the shifted bucket is under 10% of total tokens on every class, the question closes
	here and this page becomes one paragraph of future work.</mark>
	If some class clears 30%, that class and that number are the proposal.
</p>

<h2>Verdict and sequencing</h2>
<p>
	Content-addressed KV for coding agents is not worth a study: the census exists, the number is
	small, and the deployed mechanism already captures 90–96% of tokens.
	What would save money on a single vLLM host today is prefix caching plus an NVMe tier keyed
	by prefix chain and a byte-stable system prompt, which is what every operator in the sweep
	runs.
</p>
<p>
	Fall: the disk study, as specced, untouched.
	The kill test runs on spark in December alongside the report, since it needs only logs and a
	script.
	If it passes, the spring proposal is the dynamic-context census with the quality bound as its
	priced tax; if it fails, one paragraph in the fall paper's future work stakes the frame and
	cites Irminsul as its first instance:
	<mark>caches keyed by history miss what became equal, whether the pointer is a block address
	or an attention state</mark>, measured for disks here, measured for coding agents by others,
	open for the workloads that reorder their context.
</p>

<h2>Terms this page adds</h2>
<dl class="terms">
	<dt id="term-kv-cache">KV cache</dt>
	<dd>
		Per-token key and value vectors kept per layer so attention never recomputes the past.
		A pure function of (model, token sequence); ~320 KB per token for a 70B fp16 GQA model, from
		arithmetic.
	</dd>
	<dt id="term-prefill">prefill / decode</dt>
	<dd>
		Prefill builds the prompt's KV (compute-bound, the dominant fleet cost on agent traffic);
		decode generates tokens against it (bandwidth-bound). A cache hit converts prefill into a
		copy, or skips even that if the KV is already resident.
	</dd>
	<dt id="term-pic">position-independent caching (PIC)</dt>
	<dd>
		Reusing a text chunk's KV at a different position or after a different prefix. Exact
		re-basing of RoPE rotations, plus either selective recomputation of a token fraction r (GQA)
		or nothing beyond a 64-dim rotation (MLA). Named by EPIC (ICML '25); first measured against
		real agent traces by Irminsul (May '26).
	</dd>
	<dt id="term-mla">multi-head latent attention (MLA)</dt>
	<dd>
		The DeepSeek-V2 attention variant that stores each token's KV as a 512-dim position-free
		latent plus a 64-dim rotated key. The split is what makes content-addressed KV sound for
		this architecture and unsound, without heuristics, for GQA.
	</dd>
	<dt id="term-shifted">shifted-position reuse</dt>
	<dd>
		Irminsul's name for this page's non-prefix exact leaf: the same token bytes appearing at a
		different absolute offset, or after a different prefix, in an earlier request. Measured at
		4.6–18.8% for Claude Code trajectories and up to p95 48% for tool use.
	</dd>
	<dt id="term-recompute-tax">recompute tax</dt>
	<dd>
		The fraction r of a reused chunk's tokens that must be recomputed for acceptable output,
		plus the quality delta at that r. The KV analogue of the disk study's write amplification:
		the price of capture, unbounded in the literature, and near zero only for MLA.
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
