<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
</script>

<PageHead num="01" />
<p class="lede">The store has three components. The chunk log holds the data. The index locates the data. The block map gives each disk image its view of the data.</p>

<h2>Chunk log</h2>
<ul class="reqs">
	<li><span class="rid">CAS-1</span>The chunk log is an append-only binary file. It is the only authoritative structure in the store.</li>
	<li><span class="rid">CAS-2</span>Each record has a fixed header: magic, length, hash, flags. The chunk bytes follow the header.</li>
	<li><span class="rid">CAS-3</span>The log is also the write-ahead log. A record is durable after <code>fdatasync</code>.</li>
	<li><span class="rid">CAS-4</span>Reclamation punches holes (<code>FALLOC_FL_PUNCH_HOLE</code>) over dead records. The log is never compacted. The filesystem reclaims the space.<span class="tag-stretch">stretch</span></li>
</ul>

<h2>Index</h2>
<ul class="reqs">
	<li><span class="rid">CAS-5</span>The index maps hash → log offset. It lives in RAM.</li>
	<li><span class="rid">CAS-6</span>The index is a cache of the log. A rescan of the log rebuilds it. The index is never authoritative.</li>
	<li><span class="rid">CAS-7</span>A periodic index snapshot makes restart fast. A stale snapshot is safe: replay the log tail.</li>
	<li><span class="rid">CAS-8</span>A flash-resident index is an experiment arm, not the default.</li>
</ul>

<h2>Block map</h2>
<ul class="reqs">
	<li><span class="rid">CAS-9</span>Each disk image has one block map: a flat mmap'd array with one entry per chunk-sized extent. Each entry is a 32-byte hash.</li>
	<li><span class="rid">CAS-10</span>Map updates append to a map journal. A checkpoint writes the full array and truncates the journal.</li>
	<li><span class="rid">CAS-11</span>A snapshot is a COW copy of the map.</li>
	<li><span class="rid">CAS-12</span>The zero chunk has a well-known hash and no storage. WRITE_ZEROES sets map entries to the zero chunk. DISCARD sets map entries to unmapped.</li>
</ul>

<h2>Integrity</h2>
<ul class="reqs">
	<li><span class="rid">CAS-13</span>Invariant: for every map entry, <code>BLAKE3(chunk bytes) == entry hash</code>.</li>
	<li><span class="rid">CAS-14</span>Debug builds verify the hash on every read. Release builds verify during scrub.</li>
	<li><span class="rid">CAS-15</span>Dedup trusts hash equality (BLAKE3, 256 bit). A verify-on-dedup byte compare is an experiment arm. Cite Henson, HotOS '03.</li>
</ul>

<h2>Dedup and liveness</h2>
<ul class="reqs">
	<li><span class="rid">CAS-16</span>The chunk log and the index are global. Block maps are per-image. Two images that write the same bytes share one record. Cross-VM dedup needs no extra mechanism.</li>
	<li><span class="rid">CAS-17</span>A chunk is live when at least one block map references it. The block maps are the only liveness roots.</li>
	<li><span class="rid">CAS-18</span>GC is mark-and-sweep: scan the maps, build a live bitmap, punch the dead records. No refcounts.<span class="tag-stretch">stretch</span></li>
	<li><span class="rid">CAS-19</span>No reclamation occurs inside an open snapshot epoch.</li>
</ul>

<h2>Parameters</h2>
<p>Chunk size ∈ &lbrace;4K, 16K, 64K&rbrace;. A guest write smaller than one chunk causes a read-modify-write. The study measures this cost; it does not hide it. Hash ∈ &lbrace;BLAKE3, SHA-256&rbrace;.</p>

<PageNav num="01" />
