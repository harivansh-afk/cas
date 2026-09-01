<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
</script>

<PageHead num="04" />
<ul class="reqs">
	<li><span class="rid">MEAS-1</span>The backend records T0–T7 per request into a lock-free ring buffer. A drain thread writes ndjson. Cost per stage: one <code>clock_gettime</code> / <code>rdtsc</code>.</li>
	<li><span class="rid">MEAS-2</span>A one-time cross-check compares internal timestamps against bpftrace probes. The report states the delta.</li>
	<li><span class="rid">MEAS-3</span>Arms: backend &lbrace;raw, cas, raw+VDO&rbrace; × chunk &lbrace;4K, 16K, 64K&rbrace; × hash &lbrace;BLAKE3, SHA-256&rbrace; × hash mode &lbrace;inline, async&rbrace; × index &lbrace;RAM, flash&rbrace;. Run a chosen subset per claim, not the full cross product.</li>
	<li><span class="rid">MEAS-4</span>Benchmarks, three, and they double as the functional proof (F1–F3):
		<ol class="steps">
			<li><strong>fio micro:</strong> 4K randwrite, 4K randread, 128K seq write; QD ∈ &lbrace;1, 8, 32&rbrace;.</li>
			<li><strong>Macro:</strong> Linux kernel untar + defconfig build inside the guest; guest boot time.</li>
			<li><strong>Dedup corpus:</strong> ≥ 3 distro images plus 2 drifted clones written into one store; cross-image dedup ratio.</li>
		</ol>
	</li>
	<li><span class="rid">MEAS-5</span>Metrics: per-stage p50/p99/p999, IOPS, dedup ratio, index bytes per stored TB, write amplification.</li>
	<li><span class="rid">MEAS-6</span>Controls: pinned vCPUs, performance governor, discarded warm-up run, ≥ 5 repetitions, variance reported next to every number.</li>
	<li><span class="rid">MEAS-7</span>Validation gates, each mapped to a claim:
		<ul class="reqs">
			<li><span class="rid">V1</span><strong>(C1)</strong> The stage sums account for ≥ 90% of the measured p99 gap, raw vs cas.</li>
			<li><span class="rid">V2</span><strong>(C2)</strong> The curve holds ≥ 3 chunk sizes × 2 hashes.</li>
			<li><span class="rid">V3</span><strong>(C3)</strong> The async arm's p99 distance from raw is reported with the measured integrity window. Characterized, not promised.</li>
			<li><span class="rid">V4</span><strong>(F3)</strong> <code>fio --verify</code> is clean on every arm.</li>
			<li><span class="rid">V5</span>One command reruns the full harness on a second machine.</li>
		</ul>
	</li>
</ul>

<PageNav num="04" />
