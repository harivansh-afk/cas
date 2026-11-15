# 7 What Sharing Costs

**Testbed.** [TBD: hosts, CPU, memory, NVMe, network, kernel, guest shape.
Testbed floor: raw 4 KiB read, `fdatasync`, TCP round trip.]

## 7.1 One host

We run the same workload on three backends on the same NVMe device: the
content-addressed store, ZFS with deduplication at 4 KiB records, and a raw
file on XFS with no sharing.

- **Bytes stored.** [TBD]
- **Latency.** [TBD: read, write and FLUSH, p50 and p99, at 1, 4, 8 and 16
  guests.]
- **Memory.** [TBD: fingerprint index against the ZFS deduplication table.]
- **Write amplification.** [TBD]

## 7.2 Across hosts

- **A remote read.** [TBD: guest-visible latency of a read whose chunk is on
  another host, from that host's memory and from its disk, against a local
  read.]
- **Cold-start burst.** [TBD: N distinct images started at once on a host
  that holds none of them: time to first command, bytes fetched, fraction of
  the image read.]
- **Resuming on another host.** [TBD: time and bytes to resume a paused
  VM elsewhere, against a full copy and an incremental `zfs send`. Where
  the two disks share lineage, incremental send moves fewer bytes; we report
  it.]
- **Hedged requests.** [TBD: p99 with and without a hedged request, and the
  extra bytes sent. Keep only if the effect is clear.]

**Summary.** [TBD: one sentence per subsection.]
