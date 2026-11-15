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

**Limits.** FLUSH durability is local: data acknowledged but not yet replicated
is lost with its host. Membership is static. Memory state is not handled.
[TBD: implementation size, language, revision.]
