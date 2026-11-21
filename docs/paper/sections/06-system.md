# 6 The System

We measure a content-addressed block backend for VM disks. It is the system
under test; this section describes only what the measurements depend on.

**Interface.** One host process serves private disks to unmodified QEMU guests
as virtio-blk devices over vhost-user. Content addressing is invisible to the
guest.

**Write path.** Each image has a write-ahead log (WAL). A write is appended to
the log; a guest FLUSH is acknowledged after `fdatasync` on the local device.
Deduplication is post-process: a background compactor reads logged writes,
fingerprints each 4 KiB chunk with BLAKE3, stores chunks not already present in
a chunk store shared by all images on the host, and publishes the image's new
manifest. Logged data is reclaimed after it is compacted.

**Read path.** A read is served from the log if the block was written since the
last compaction, and otherwise from the manifest and the chunk store.

**Images.** A *manifest* maps each offset of an image to a chunk fingerprint. A
snapshot is a retained manifest; a clone is a copy of one.

**Across hosts.** Hosts form a static fleet over kernel TCP. Each chunk is
stored on k hosts chosen by its fingerprint (k = 1 or 2). A host that reads a
chunk it does not hold fetches it by fingerprint from a host that stores it and
verifies the fingerprint before use. Provisioning or moving an image sends its
manifest; chunks follow on demand.

**Implementation scope.** The store is a minimal implementation built to
measure bytes stored and bytes sent. The latency and downtime we report for the
store are upper bounds on the cost of content addressing. Three choices set the
slow numbers in §7.

- *Remote reads.* A host opens a configured number of TCP connections to each
  other host, shared by every VM on the host. A chunk's fingerprint selects
  the connection. The serving host answers each connection from one blocking
  thread. One request carries one 4 KiB chunk. The store has no readahead and
  no request for a range of chunks.
- *Moving an image.* The whole manifest is sent and installed while the VM is
  stopped. The store does not send the manifest before the stop, as pre-copy
  migration does for memory [@precopy] [check]. The store does not send only
  the manifest entries the destination lacks. The VM boots cold at the
  destination, because memory state is not transferred [@postcopy] [check].
- *Compaction and transfer.* Chunks are a fixed 4 KiB and are not compressed.
  Import and compaction process one image at a time.

We did not implement these techniques and we do not estimate what each would
gain.

**Limits.** FLUSH durability is local: data acknowledged but not yet replicated
is lost with its host. Membership is static. Memory state is not handled.
[TBD: implementation size, language, revision.]
