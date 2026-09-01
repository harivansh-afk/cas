<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
</script>

<PageHead num="04" />
<p class="lede">
	Every line the study's claims depend on is either <mark>a stock upstream release or new code
	in one repository</mark>.
	The hypervisor is never forked: QEMU speaks vhost-user-blk to an external process, so all new
	code lives in that process, the daemon.
</p>

<div class="table-scroll">
	<table class="spec">
		<thead>
			<tr><th>Component</th><th>Source</th><th>License</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">Hypervisor</td><td>stock <a href="https://www.qemu.org">QEMU</a>, unmodified (vhost-user-blk front end)</td><td>GPL-2.0</td></tr>
			<tr><td class="k">vhost-user protocol handling</td><td><a href="https://github.com/rust-vmm">rust-vmm</a> <code>vhost-user-backend</code>, <code>vm-memory</code>, <code>virtio-queue</code> crates; Cloud Hypervisor's <code>vhost_user_block</code> read as the reference backend</td><td>Apache-2.0</td></tr>
			<tr><td class="k">vhost-user reference for R0</td><td>stock <code>qemu-storage-daemon</code>, <code>--export type=vhost-user-blk</code> over the R0 file</td><td>GPL-2.0</td></tr>
			<tr><td class="k">Hashing</td><td>official <a href="https://github.com/BLAKE3-team/BLAKE3">blake3</a> crate</td><td>Apache-2.0/CC0</td></tr>
			<tr><td class="k">Content-defined chunking</td><td><a href="https://crates.io/crates/fastcdc">fastcdc</a> crate, or reimplemented from the FastCDC paper if the crate falls short</td><td>MIT</td></tr>
			<tr><td class="k">R1 baseline</td><td>stock <a href="https://github.com/openzfs/zfs">OpenZFS</a> ≥ 2.3 (fast dedup; 2.4.x current), unmodified</td><td>CDDL</td></tr>
			<tr><td class="k">Staging log, compactor, chunk store, index, maps, GC</td><td>written for this study</td><td>new code</td></tr>
			<tr><td class="k">Census pipeline, harness, analysis</td><td>written for this study</td><td>new code</td></tr>
		</tbody>
	</table>
</div>

<p>
	This split is what makes the measurements defensible. Because the hypervisor is unmodified, no
	result can be an artifact of a patched QEMU, and the R0 control runs the identical binary. It
	also bounds the build: the protocol plumbing comes from maintained crates, so the engineering
	budget is spent entirely on the components the paper is about.
</p>

<h2>Repository</h2>
<pre>{`chunkd/
  crates/
    daemon/        # vhost-user-blk backend: request loop, staging, FLUSH
    staging/       # append-only staging log, replay
    compact/       # FastCDC + BLAKE3 pass, epochs, back-pressure
    store/         # chunk log, index, hole-punch GC
    map/           # offset-tree and Merkle-paged map arms, journal, COW snapshots
    trace/         # per-request stage timestamps -> ndjson
  census/          # S1: corpus build scripts, chunkers, decomposition, folk-claim checks
  harness/         # rung configs (R0-R3), fio jobs, workloads, runner, aging protocol
  analyze/         # tables and figures from ndjson; uv-run python
  results/         # tagged ndjson and figures per run
  docs/            # this spec, methodology notes`}</pre>

<h2>Build order</h2>
<p>
	The census (<code>census/</code>) is standalone and starts on day one; its week-4 fixed-versus-CDC
	verdict picks the compactor's headline chunker (page 01). The daemon starts as
	passthrough (R0) to validate the vhost-user path against stock QEMU's raw driver and against
	<code>qemu-storage-daemon</code>'s export before any content addressing exists. The compactor and store land next (R2), then
	the ZFS rung configuration, then the second map arm (R3). Each rung is benchmarkable the week it
	lands; no step depends on a later one.
</p>

<PageNav num="04" />
