<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
	import Mermaid from '$lib/components/Mermaid.svelte';

	const writePath = `sequenceDiagram
  participant G as guest
  participant R as virtio ring
  participant D as dispatch
  participant C as cas engine
  participant S as storage

  G->>R: guest write
  R->>D: pop · parse [T0→T1]
  D->>C: hash chunk, BLAKE3 [T2]
  C->>C: index lookup [T3]
  alt hit
    C->>S: write hash to block map + journal [T6]
  else miss
    C->>S: stage · chunk log append [T4→T5]
    C->>C: insert index entry
    C->>S: block map + journal [T6]
  end
  C-->>R: ack [T7]
  R-->>G: irq · write returns
  note over G,S: ack ≠ durable · volatile until FLUSH
  G->>R: FLUSH
  R->>S: fdatasync chunk log, then map journal
  S-->>G: ack · durable`;
</script>

<PageHead num="03" />
<p class="lede">The stages are the measurement units. The write path defines timestamps T0–T7. Every latency claim in the paper decomposes into these stages.</p>

<h2>Stage timestamps</h2>
<div class="table-scroll">
	<table class="spec">
		<thead><tr><th>Stamp</th><th>Event</th></tr></thead>
		<tbody>
			<tr><td class="k">T0</td><td>request popped from the ring</td></tr>
			<tr><td class="k">T1</td><td>descriptor chain parsed, extents split</td></tr>
			<tr><td class="k">T2</td><td>chunk hash complete</td></tr>
			<tr><td class="k">T3</td><td>index lookup complete (hit or miss)</td></tr>
			<tr><td class="k">T4</td><td>log append submitted (io_uring)</td></tr>
			<tr><td class="k">T5</td><td>log write complete</td></tr>
			<tr><td class="k">T6</td><td>block map + journal updated</td></tr>
			<tr><td class="k">T7</td><td>ack pushed to the ring</td></tr>
		</tbody>
	</table>
</div>
<p class="note">Read path analog: T2r index lookup, T3r log read submitted, T4r read complete, T5r verify (debug), T6r ack.</p>

<h2>Write path</h2>
<Mermaid
	code={writePath}
	caption="The write path. An index hit updates only the block map; a miss appends to the chunk log first. T0–T7 are the measured stages. The ack precedes durability: only an acked FLUSH means durable."
/>

<ol class="steps">
	<li>Pop the request. Parse the descriptor chain. <span class="note">(T0→T1)</span></li>
	<li>Split the write into chunk-aligned extents.</li>
	<li>For a full-chunk extent: hash the bytes. <span class="note">(T2)</span></li>
	<li>Look up the hash in the index. <span class="note">(T3)</span></li>
	<li>On hit: write the hash into the block map. Append one map-journal record. Skip to step 8.</li>
	<li>On miss: append the record to the staging buffer. Submit the log write. <span class="note">(T4, T5)</span> Insert the index entry.</li>
	<li>For a partial-chunk extent: read the old chunk, patch the bytes, go to step 3. This is the RMW path.</li>
	<li>Update the map. <span class="note">(T6)</span> Ack the request. <span class="note">(T7)</span> Data may stay volatile until FLUSH.</li>
</ol>

<h2>Read path</h2>
<ol class="steps">
	<li>Look up the map entry.</li>
	<li>Zero chunk: return zeros. Unmapped: return zeros.</li>
	<li>Otherwise: index → log offset → read the chunk.</li>
	<li>Debug build: verify the hash.</li>
	<li>Ack.</li>
</ol>

<h2>FLUSH</h2>
<ol class="steps">
	<li><code>fdatasync</code> the chunk log.</li>
	<li><code>fdatasync</code> the map journal.</li>
	<li>Ack. An acked FLUSH means durable. Nothing else does.</li>
</ol>

<h2>DISCARD</h2>
<p>Set the map entries to unmapped. Append journal records. Ack. The sweep reclaims the space later (CAS-18).</p>

<h2>Async-hash arm (C3)</h2>
<ol class="steps">
	<li>Append the bytes to the log. Ack after the append. The log is the WAL, so no data is lost.</li>
	<li>A worker hashes the bytes off the critical path.</li>
	<li>The worker inserts the index entry and dedups retroactively. A late duplicate rewrites the map entry; the sweep reclaims the orphan record.</li>
	<li>The integrity window is the ack-to-hash interval. The harness measures it. This is the inline vs post-process dedup distinction from the literature, measured per stage.</li>
</ol>

<PageNav num="03" />
