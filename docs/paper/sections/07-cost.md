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

- **A remote read.** Every read is a random 4 KiB read served from the other
  host's disk, one outstanding read per VM, three runs of 30 s. With one
  connection between the hosts, 1 VM completes 4,700 reads/s at a median of
  194 µs. 4 VMs complete 6,900 reads/s in total at a median of 578 µs each.
  8 VMs complete 6,700 reads/s in total at a median of 1,204 µs each.
  Sequential reads reach about 50 MB/s at every VM count. The link between the
  hosts is 100 Gbit/s, so 50 MB/s uses 0.4% of the link. The limit is the one
  thread that answers the one connection (§6), not the network.
  [TBD: the same test with 4 and 8 connections. Latency against a local read.]
- **Cold-start burst.** [TBD: N distinct images started at once on a host
  that holds none of them: time to first command, bytes fetched, fraction of
  the image read.]
- **Resuming on another host.** A VM writes 4 GiB to a 32 GiB synthetic disk,
  stops, and resumes on the other host, three runs. The store sends 256 MB, of
  which 204 MB is the manifest. Downtime is 30.8 to 31.9 s. Sending and
  installing the whole manifest takes 19.6 s of the downtime, and starting the
  destination store process from cold takes 9.2 s (§6). After 8 MiB written to
  a 4 GiB disk, the store sends 1.2 MB and downtime is 11.7 to 12.7 s, against
  11.3 to 12.3 s for an incremental `zfs send` and 20.3 s for a full copy.
  [TBD: table with bytes for each method. Real disks in place of synthetic.]
- **Hedged requests.** [TBD: p99 with and without a hedged request, and the
  extra bytes sent. Keep only if the effect is clear.]

**Summary.** [TBD: one sentence per subsection.]
