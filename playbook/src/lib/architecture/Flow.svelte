<script lang="ts">
	import Code from './Code.svelte';
	let selected = $state(0);
	const flows = [
		{
			name: 'WRITE', result: 'The block becomes readable after ordered publication. A FLUSH is still needed for durability.',
			steps: [
				{ owner: 'Guest Linux → Backend', wire: 'shared guest RAM + kick eventfd', action: 'Retain validated descriptor spans. Reserve capacity before assigning a mutation sequence; waiting write payload stays in guest RAM.' },
				{ owner: 'Backend → image reactor', wire: 'bounded Rust channel + eventfd', action: 'Gather guest bytes into the final aligned WAL buffer. Move its ownership to the reactor.' },
				{ owner: 'Image reactor → Linux / XFS', wire: 'io_uring WRITE on an O_DIRECT file', action: 'Append the packed batch. Keep its buffer and file alive until the kernel completes.' },
				{ owner: 'Image reactor → guest', wire: 'completion channel → used ring + call eventfd', action: 'Publish the contiguous write prefix and staging mapping, then return the guest completion.' }
			],
			paths: ['crates/cas/daemon/src/backend.rs', 'crates/cas/daemon/src/local/reactor.rs']
		},
		{
			name: 'FLUSH', result: 'Earlier admitted mutations are durable in the WAL. Deduplication can happen later.',
			steps: [
				{ owner: 'Guest → image sequencer', wire: 'virtqueue → bounded command channel', action: 'Wait for earlier discovered work to reach admission, then capture the mutation boundary. FLUSH receives no new mutation number.' },
				{ owner: 'Image reactor', wire: 'io_uring WRITE for a FENCE', action: 'Seal the batch and finish the covered writes. Hold later write submissions outside this finite cohort.' },
				{ owner: 'Image reactor → host kernel', wire: 'io_uring Fsync with DATASYNC', action: 'Sync the covered WAL prefix. A failed sync cannot produce a successful FLUSH.' },
				{ owner: 'Backend → guest', wire: 'status byte + used ring + call eventfd', action: 'Complete covered FLUSHes; the guest filesystem can finish the corresponding fsync.' }
			],
			paths: ['crates/cas/daemon/src/local/reactor.rs', 'crates/cas/core/src/append/submission.rs']
		},
		{
			name: 'READ', result: 'Recent staged writes and ZERO ranges override the committed manifest. The returned bytes are copied into guest RAM.',
			steps: [
				{ owner: 'Guest Linux → frontend', wire: 'shared guest RAM + kick eventfd', action: 'Own the descriptor snapshot. Independent reads may pass blocked writes, then reserve read credits through shared admission.' },
				{ owner: 'Image reactor → Log / View', wire: 'in-process lookup and lifetime pins', action: 'Wait for the captured write boundary; freeze the staging ranges and committed manifest root used by this read.' },
				{ owner: 'View → shared Reader', wire: 'B+tree lookup → hash-index lookup', action: 'Resolve uncovered blocks to content hashes. Holes return zero; cached chunks can satisfy the read immediately.' },
				{ owner: 'Reader → storage', wire: 'io_uring READ; eventfd / PollAdd for joined misses', action: 'One leader fetches a missing hash. Verify its header, CRC and BLAKE3; other readers join that result.' },
				{ owner: 'Reactor → Backend → guest', wire: 'owned response → shared guest RAM', action: 'Copy the response into guest descriptors, publish status and notify completion.' }
			],
			paths: ['crates/cas/daemon/src/backend/frontier.rs', 'crates/cas/daemon/src/local/host/fair.rs', 'crates/cas/daemon/src/local/reactor/read.rs', 'crates/cas/core/src/append/read.rs']
		},
		{
			name: 'COMPACT', result: 'The manifest advances only after its chunks are durable. Old WAL space becomes reclaimable after readers and replay identities retire.',
			steps: [
				{ owner: 'Image reactor → shared compactor', wire: 'bounded selection / reply channels', action: 'Select whole durable WAL batches above the current manifest prefix. One worker serves all images.' },
				{ owner: 'Compactor → Store', wire: 'direct pread / pwrite + sync_data', action: 'Verify input, omit covered versions, hash fixed 4 KiB blocks and store only missing nonzero chunks.' },
				{ owner: 'Compactor → Manifest', wire: 'direct file IO + sync_data', action: 'Write copy-on-write tree pages and a COMMIT. Return a receipt naming the exact new root.' },
				{ owner: 'Reactor ↔ compactor', wire: 'publication / reclamation replies', action: 'Install the new View under the completion gate. Punch eligible WAL payload and unlink retired segments.' }
			],
			paths: ['crates/cas/daemon/src/local/host/worker.rs', 'crates/cas/core/src/append/compaction/output.rs']
		},
		{
			name: 'COLLECT', result: 'Collection pauses new requests across every image. If live data still fills usable space, new writes keep waiting for capacity.',
			steps: [
				{ owner: 'Shared worker → all frontends', wire: 'admission gate + wake eventfds', action: 'Pause new admission, including reads. Drain accepted guest owners before changing storage reachability.' },
				{ owner: 'Shared worker ↔ image reactors', wire: 'Quiesce / Quiesced channel messages', action: 'Drain IO and fence each image. Retain the pause through the complete collection operation.' },
				{ owner: 'Compactor → manifests / chunks', wire: 'direct file IO, sync, punch and unlink', action: 'Mark active, snapshot and pinned roots. Copy live victims durably before deleting old files; reclaim dead manifest pages.' },
				{ owner: 'Governor → worker → frontends', wire: 'filesystem observation → resume wakeups', action: 'Reconcile real allocation. Resume the host pause after success; physical write admission still obeys its low watermark.' }
			],
			paths: ['crates/cas/daemon/src/local/host/collection.rs', 'crates/cas/core/src/store/file/collection.rs']
		},
		{
			name: 'RECONNECT', result: 'This path requires the original QEMU processes, guest RAM and inflight FDs. Losing the host requires cold recovery.',
			steps: [
				{ owner: 'Surviving QEMU → replacement host', wire: 'vhost-user socket + SCM_RIGHTS FD passing', action: 'Supply the original queues, guest-memory mappings and retained inflight carrier for every catalog image.' },
				{ owner: 'Recovery coordinator → storage', wire: 'file locks + read-only direct IO', action: 'Validate the complete dependency graph and saved published prefixes before shared repair.' },
				{ owner: 'Recovery coordinator → WAL', wire: 'owned guest-memory gather + direct file IO', action: 'Restore discovered requests to admission. Preserve present mutations and replay missing admitted mutations with their original identities.' },
				{ owner: 'All image reactors → guests', wire: 'recovery fences → shared barrier → used rings', action: 'Sync every image, install all reactors, then restore completions and open ordinary admission.' }
			],
			paths: ['crates/cas/daemon/src/local/host/recovery/frontend.rs', 'crates/cas/daemon/src/inflight/recovery.rs']
		}
	];
	const flow = $derived(flows[selected]);
</script>

<div class="flow">
	<div class="choices" role="group" aria-label="Follow an operation">
		{#each flows as item, i}<button type="button" aria-pressed={selected === i} onclick={() => selected = i}>{item.name}</button>{/each}
	</div>
	<div aria-live="polite" aria-atomic="true">
		<ol>
			{#each flow.steps as step, i}
				<li><span class="number" aria-hidden="true">{i + 1}</span><div><strong>{step.owner}</strong><span class="wire">{step.wire}</span><p>{step.action}</p></div></li>
			{/each}
		</ol>
		<p class="result">{flow.result}</p>
		<Code paths={flow.paths} />
	</div>
</div>

<style>
	.flow { margin: 1.5rem 0 2rem; border: 1px solid var(--border); border-radius: var(--radius); padding: 1.25rem; background: var(--surface); }
	.choices { display: flex; flex-wrap: wrap; gap: 0.4rem; margin-bottom: 1.75rem; }
	button { font: inherit; font-size: 0.6875rem; padding: 0.4rem 0.6rem; border: 1px solid var(--border); border-radius: 4px; background: var(--background); color: var(--text-secondary); cursor: pointer; }
	button[aria-pressed='true'] { background: var(--text-primary); color: var(--background); border-color: var(--text-primary); }
	button:focus-visible { outline: 2px solid var(--text-primary); outline-offset: 3px; }
	ol { list-style: none; margin: 0; padding: 0; }
	li { display: grid; grid-template-columns: 1.5rem 1fr; gap: 0.75rem; position: relative; padding-bottom: 1.5rem; margin: 0; }
	li:not(:last-child)::before { content: ''; position: absolute; top: 1.6rem; bottom: 0.1rem; left: 0.7rem; width: 1px; background: var(--border); }
	.number { width: 1.5rem; height: 1.5rem; border: 1px solid var(--border); border-radius: 50%; font-size: 0.6875rem; text-align: center; line-height: 1.4rem; }
	strong { color: var(--text-primary); font-size: 0.8125rem; }
	.wire { display: block; font-size: 0.6875rem; color: var(--text-tertiary); margin: 0.15rem 0 0.4rem; }
	li p { margin: 0; font-size: 0.8125rem; }
	.result { border-top: 1px solid var(--border); padding-top: 1rem; font-size: 0.8125rem; }
	@media print { .choices { display: none; } }
</style>
