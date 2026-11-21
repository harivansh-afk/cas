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

- **A remote read.** Every read is a 4 KiB read served from the other host's
  disk. Each run lasts 30 s and is repeated one to three times. The link
  between the hosts is 100 Gbit/s.

  *One read.* One VM with one outstanding read completes 4,200 reads/s at a
  median of 216 to 220 µs. The connection count does not change this number.
  [TBD: the store's local read and the ZFS local read on the same hosts, from
  §7.1.]

  *Many VMs, one connection.* 4 VMs complete 6,500 reads/s in total at a
  median of 618 µs each. 8 VMs complete 6,300 reads/s in total at a median of
  1,270 µs each. Throughput does not grow with VMs because one thread on the
  serving host answers the one connection (§6).

  *Many VMs, several connections.* With 4 connections, 8 VMs complete 17,300
  reads/s at a median of 420 µs. With 8 connections and 12 cores for the store
  process, 8 VMs complete 22,500 reads/s at 322 µs. With 16 connections, 8 VMs
  complete 24,600 reads/s at 305 µs. The gain from 8 to 16 connections is
  under 10%.

  *Sequential reads, 16 outstanding per VM.* 8 VMs read 39 MB/s in total with
  1 connection and 122 MB/s with 4. With 8 or 16 connections 8 VMs read 54 to
  65 MB/s, with a 99th percentile latency four times that of 4 connections. We
  have not found the cause. 122 MB/s is 1% of the link.
  [TBD: figure of reads/s against connection count at 8 VMs.]
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
