<script lang="ts">
	// Parallel tracks of work after this update. Order left to right is priority.
	const tracks: { name: string; items: string[]; first?: boolean }[] = [
		{
			name: 'Write path',
			first: true,
			items: [
				'concurrent inflight recovery: the two vhost-user inflight handlers, or the lower-level vhost crate',
				'multiqueue: FLUSH covers the highest completed write on any queue',
				'batched IO in the daemon: one group of device writes per FLUSH',
				'bounded staging: a governor that paces compaction'
			]
		},
		{
			name: 'Compactor',
			items: [
				'settle window, then fixed 4 KiB chunks hashed with BLAKE3',
				'manifest: image offset to chunk hash, journaled',
				'trim the log below D once chunks and manifest are durable'
			]
		},
		{
			name: 'Storage backend',
			items: [
				'chunk store: append-only records, hash inline',
				'index: hash to offset, in memory, rebuilt by scanning the store',
				'GC sweep from the live set'
			]
		},
		{
			name: 'Read path',
			items: [
				'staging log, then chunk cache, then local store',
				'manifest and index lookup for settled data',
				'remote GET by hash, with the two-host work'
			]
		},
		{
			name: 'Testbed',
			items: [
				'finalize the Nix guest image and the XFS, ZFS and CAS host profiles',
				'CloudLab pair; fallback two OVHcloud servers on 25 GbE'
			]
		},
		{
			name: 'Measurements',
			items: [
				'G1: raw XFS against the passthrough guest, p99 within 10%',
				'write path: per-write direct vs buffered + fdatasync vs daemon-batched direct',
				'read cache: daemon chunk cache vs host page cache vs pmem for the base image',
				'ZFS two-clone control; content-defined chunking arm of the census'
			]
		}
	];
</script>

<figure aria-label="Six parallel tracks of upcoming work: write path, compactor, storage backend, read path, testbed, and measurements">
	{#each tracks as t (t.name)}
		<div class="track" class:first={t.first}>
			<h3>{t.name}</h3>
			<ul>
				{#each t.items as item (item)}<li>{item}</li>{/each}
			</ul>
		</div>
	{/each}
</figure>

<style>
	figure {
		margin: 0;
		width: auto;
		display: grid;
		grid-template-columns: repeat(3, minmax(0, 1fr));
		gap: 1.5rem 2.5rem;
	}
	.track {
		padding-top: 0.75rem;
		border-top: 1px solid var(--border);
	}
	.track.first {
		border-top-color: #d97706;
	}
	h3 {
		margin: 0 0 0.5rem;
		font-size: 0.875rem;
		font-weight: var(--weight-strong);
		color: var(--text-primary);
	}
	.first h3 {
		color: #d97706;
	}
	ul {
		margin: 0;
		padding-left: 1rem;
		font-size: 0.8125rem;
		line-height: 1.5;
		color: var(--text-secondary);
	}
	li + li {
		margin-top: 0.25rem;
	}
	li::marker {
		color: var(--text-quaternary);
	}
	@media (max-width: 900px) {
		figure {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
	}
	@media (max-width: 560px) {
		figure {
			grid-template-columns: 1fr;
		}
	}
</style>
