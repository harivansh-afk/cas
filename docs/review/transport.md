# Remote chunk read transports: state of the art

Research sweep for the cross-host chunk fetch study: 4K–64K content-addressed chunks over 100 GbE (Mellanox ConnectX-5 Ex, RoCE-capable), PCIe 4.0 NVMe on both ends (~70–100 µs local 4K read), Rust userspace daemon. Date: 2026-09-01. "(not opened)" marks a source read only through search snippets or secondary summaries.

## Short version

On 100 GbE with ~80 µs of NVMe media under every read, the added latency of a 4K QD1 remote read is roughly: raw RDMA 3–5 µs, kernel nvme-rdma ~12 µs, SPDK NVMe/TCP ~17.5 µs, kernel nvme-tcp ~21 µs, a non-spinning userspace daemon over kernel TCP ~20–30 µs (estimate, see item 8), i10-style batched kernel TCP 50–100 µs. The transport tier is a 5–30% effect on the read. Wakeups and batching timers are what blow up the tails, not the wire.

## Findings by item

### 1. i10 (NSDI '20)

Source: https://www.usenix.org/system/files/nsdi20-paper-hwang.pdf (opened; mirror https://www.cs.cornell.edu/~ragarwal/pubs/i10.pdf).

- Mechanism: per-core "i10 lanes" (i10 queue + worker thread + TCP socket per core per target) between blk-mq and unmodified kernel TCP, NVMe/TCP wire format. Batches PDUs into "caravans" up to 64 KB (TSO max) or 16 requests, sent with one `kernel_sendmsg()`. Delayed doorbells: timer armed on first request, ring on 16 requests or timeout. Defaults: aggregation 16, timeout 50 µs. The paper admits low-load requests observe "timeout amount of latency". A per-request no-delay flag flushes immediately.
- Hardware: two hosts back-to-back, ConnectX-5 EX 100G, Xeon Gold 6128, Samsung PM1725a, kernel 4.20, jumbo 9000, fio libaio 4K direct.
- Throughput: kernel NVMe/TCP 96 kIOPS/core (Fig. 1). i10 and nvme-rdma saturate the SSD with 4 cores; nvme-tcp needs 2.5x more (Fig. 7a). RAM device: i10 2.8M IOPS at ~20 cores. Caravans and delayed doorbells give 38% and 23% of the 16-core throughput (Fig. 10c).
- Latency (Fig. 6, single core, 4K random read, low load, SSD): i10 avg 189 µs / p99 206 µs vs nvme-rdma 105 / 119 µs. NVMe/TCP values are plot-only, described as "comparable" to i10. The batching penalty is never isolated; blk-switch (below) says i10 defaults cost "~50–100 µs at low loads".
- Take: a CPU-efficiency result, not a latency result. Its design is the opposite of what a QD1 latency study wants.

### 2. blk-switch (OSDI '21)

Source: https://www.usenix.org/system/files/osdi21-hwang.pdf (opened; mirror https://www.cs.cornell.edu/~ragarwal/pubs/blk-switch.pdf).

- Mechanisms: per-core, per-device egress queues per application class with their own I/O threads, latency-sensitive threads at higher CPU priority; request steering for throughput apps when a core's outstanding bytes exceed a threshold (16 x 64 KB for remote); application steering that migrates threads under persistent overload (per-core L-app cap 100 KB).
- Hardware: kernel 5.4.43, two hosts back-to-back at 100G, Xeon Gold 6234, Samsung PM1735 (their SSD latency "~80 µs"). NIC not named in text; the sibling NetChannel paper on the same testbed used ConnectX-5. Remote path is always i10, never plain nvme-tcp.
- Numbers: isolated single core, remote RAM device (Fig. 2): Linux p99 118 µs at 26 Gbps/core; SPDK NVMe-oF/TCP "5x lower latency, 1.5x higher throughput" (~24 µs, ~39 Gbps, derived). Under contention (Fig. 7): 10–25x better p99 than Linux, 2–15x than SPDK. Fig. 12 (200G, 16 L + 16 T apps, 16 cores): 10 µs avg, 143 µs p99, 296 µs p99.9. Prioritisation alone gives the order-of-magnitude tail win; steering recovers throughput (Fig. 15).
- No journal follow-on exists (DBLP and both authors' pages checked). The cited extended tech report is absent from the GitHub repo.
- Take: tails come from CPU scheduling and head-of-line blocking on shared cores, not the fabric.

### 3. ReFlex (ASPLOS '17)

Source: https://people.ucsc.edu/~hlitz/papers/reflex.pdf (opened).

- Design: IX dataplane (Dune/VT-x, DPDK NIC driver) plus an NVMe driver derived from SPDK, run-to-completion per thread, adaptive batching up to 64, per-tenant SLO with a token-bucket scheduler (1 token = one 4K random read).
- Hardware: Xeon E5-2630 Sandy Bridge, Intel 82599ES 10 GbE, kernel 4.4.
- Table 2, unloaded 4K QD1 read avg/p95 (µs): local SPDK 78/90; ReFlex with IX client 99/113 ("adds 21 µs"); ReFlex with Linux client 117/135; Linux libaio remote 183/205; iSCSI 211/251. Throughput: 850K IOPS/core vs 75K for libaio+libevent.
- Take: the "Linux adds over 100 µs" figure is 2017 hardware on kernel 4.4 and 10 GbE. Do not reuse it as a modern kernel baseline.

### Related, newer

- "Understanding Host Network Stack Latency" (SIGCOMM '26, Zuo, Hwang, Tang, Agarwal, Cai): https://www.cs.cornell.edu/~ragarwal/pubs/understanding-latency.pdf (opened). ConnectX-7 400G, kernel 5.10 (+6.12 EEVDF). Kernel TCP floor: 64 B ping-pong avg 12.9 µs, p99.9 19 µs; busy-poll 11/14 µs; TAS userspace 9 µs p99.9. One thread with 32 in flight sustains 1.28M 64 B RPC/s per core at 90 µs p99.9. Tail attributed to rx scheduling, not packet processing.
- NetChannel (SIGCOMM '22): https://www.cs.cornell.edu/~ragarwal/pubs/netchannel.pdf (opened). Tail isolation via disaggregated kernel TCP pipeline; no nvme-tcp vs RDMA latency table.
- "zcIO: Enabling Transparent Zero-copy for NVMe/TCP" (FAST '27, Hwang): listed on https://sites.google.com/site/bekind/ (not opened, no PDF found).
- No peer-reviewed 2021–2026 paper tabulating kernel nvme-tcp vs nvme-rdma vs userspace at 100G was found. The SPDK reports below are the best substitute.

### 4. NVMe-oF: RDMA vs TCP, and file-backed nvmet

Best apples-to-apples numbers: SPDK 24.05 performance reports, same hosts (2x Xeon Gold 6348), ConnectX-5 100G, Fedora 37 kernel 6.0.18, null bdev (no media), target and initiator each pinned to one core, 4K QD1 random read, Test Case 3.

| Path | 4K read avg | p99 | p99.99 |
|---|---|---|---|
| RDMA, kernel target / kernel initiator | 12.10 µs | 12.6 | 18.6 |
| RDMA, SPDK target / kernel initiator | 9.39 µs | 9.8 | 22.7 |
| RDMA, SPDK / SPDK | 4.72 µs | 4.9 | 10.3 |
| TCP, kernel / kernel | 21.39 µs | 26.2 | 59.7 |
| TCP, SPDK target / kernel initiator | 20.43 µs | 27.2 | 106.0 |
| TCP, SPDK / SPDK | 17.50 µs | 25.7 | 83.9 |

Sources: https://review.spdk.io/download/performance-reports/SPDK_rdma_mlx_perf_report_2405.pdf and https://review.spdk.io/download/performance-reports/SPDK_tcp_mlx_perf_report_2405.pdf (both opened). Kernel TCP initiator ran fio io_uring, no poll queues, aRFS on. The RDMA kernel initiator used libaio because `--nr-poll-queues` oopsed on 6.0.18. Index of all reports: https://spdk.io/doc/performance_reports.html.

CPU (Test Case 4, kernel initiator, 14 Kioxia drives): RDMA kernel target 6174k IOPS on 10.2 cores (~605k IOPS/core); TCP kernel target 4018k IOPS on 20.5 cores (~196k IOPS/core). SPDK target: up to 8.17x more IOPS/core than the kernel target on RDMA at 16 connections, up to 1.69x on TCP.

Other measurements:
- Samsung, Systor '17 slides, ConnectX-4 100G RoCE, PM1725, 4K unloaded read: DAS 81.6 µs; nvme-rdma adds 11.7 µs (target modules 4.57, host modules 3.25, fabric 2.43, other 1.52); SPDK target adds 8.9 µs. https://www.systor.org/2017/slides/NVMe-over-Fabrics_Performance_Characterization.pdf (opened).
- Chelsio T6 100G TOE, kernel 5.4.45, back-to-back: local read 109.16 µs; +4.98 µs with SPDK target; +17.12 µs with kernel NVMe/TOE target and host. https://www.chelsio.com/wp-content/uploads/resources/t6-100g-spdk-nvmetoe.pdf (opened).
- Western Digital OpenFlex Data24, Feb 2025, 4K QD1 at the four-nines point: RoCE read 162.82 µs vs TCP 177.15 µs; writes 41.22 vs 122.37. Initiator kernel not stated. https://documents.westerndigital.com/content/dam/doc-library/en_us/assets/public/western-digital/collateral/white-paper/white-paper-open-flex-data24-roce-vs-tcp.pdf (opened).
- StarWind, kernel 4.19, ConnectX-4, Optane 900P, 8 jobs x QD4: local and SPDK-over-RDMA both ~587k IOPS at 50 µs. https://www.starwindsoftware.com/blog/nvme-part-1-linux-nvme-initiator-linux-spdk-nvmf-target/ (opened).
- Oracle UEK6 blog: "TCP latency was 30% higher than RDMA" on 40G ConnectX-5 (not opened, 403).
- Sagi Grimberg's SDC 2018 nvme-tcp slides (not opened, 403). simplyblock's "300–500 µs TCP / 100–150 µs RoCE" are marketing ranges with no setup; ignore.

File-backed nvmet namespaces:
- Yes. `drivers/nvme/target/io-cmd-file.c`, Chaitanya Kulkarni (Western Digital), merged by Christoph Hellwig into 4.18. `buffered_io` configfs attribute added in 4.19. Sources: https://github.com/torvalds/linux/commits/master/drivers/nvme/target/io-cmd-file.c and https://raw.githubusercontent.com/torvalds/linux/master/drivers/nvme/target/io-cmd-file.c (opened).
- Configuration: point the namespace's `device_path` at a regular file. `nvmet_ns_enable` tries the bdev backend, gets `-ENOTBLK`, falls through to `nvmet_file_ns_enable`. Set `buffered_io` before `enable` (the store refuses on an enabled namespace). nvmetcli docs never mention it.
- I/O path: opens `O_RDWR | O_LARGEFILE`, adds `O_DIRECT` unless `buffered_io=1`. Builds an `iov_iter_bvec` and calls the file's `read_iter`/`write_iter` directly, inline in the transport's context, completing via `ki_complete`. Buffered mode first tries `IOCB_NOWAIT` inline (page-cache hit completes synchronously), otherwise queues to the `nvmet-buffered-io-wq` workqueue. Exported LBA size is the filesystem block size capped at 4K. No O_DIRECT fallback for filesystems without it (tmpfs will not work in direct mode).
- No measurement of file-backed vs bdev-backed nvmet latency was found, nor of the XFS extent-lookup cost on the direct read path. Unmeasured.

nvme-tcp initiator knobs (from `drivers/nvme/host/tcp.c` and `fabrics.c`, opened):
- `TCP_NODELAY` is unconditional. `queue_size` default 128 (16–1024). `nr_io_queues` defaults to online CPUs. `--nr-poll-queues N` creates busy-poll queues (`sk_busy_loop`), used only by polled submissions (fio `hipri=1` / io_uring IOPOLL). Header and data digests off by default; data digest is crc32c inline per page and disables the last-fragment fast path.
- Submission sends inline from the submitter if it is on the queue's `io_cpu` and the queue is empty, otherwise via `nvme_tcp_wq` on `io_cpu` (`wq_unbound` module param changes this).
- Target side (`drivers/nvme/target/tcp.c`): `idle_poll_period_usecs` keeps target io_work spinning between requests; budgets recv 8, send 8, io_work 64.
- nvme-cli option reference: https://raw.githubusercontent.com/linux-nvme/nvme-cli/master/Documentation/fabrics-options.txt (opened).

### 5. io_uring zero-copy

send-zc (`IORING_OP_SEND_ZC` / `SENDMSG_ZC`, Linux 6.0):
- Pavel Begunkov's v5 numbers on a real NIC, req/s vs copy send: 4000 B +22%, 1500 B +4.5%, 1000 B +1.2%, 600 B +0.4%; with notification flush the small sizes go negative. https://lwn.net/Articles/900083/ (opened).
- 6.10 buffer coalescing: Jens Axboe, "the crossover point for send zerocopy being faster is now around 3000 byte packets". https://kernelnewbies.org/Linux_6.10 (opened).
- Field test on ConnectX-5/6 with liburing `send-zerocopy`, io-uring list 2024: on 6.8 with IOMMU on, zc was slower than copy on EPYC "in every single test"; on Xeon + CX-6 it wins from 12 KB registered / 16 KB normal buffers; on 6.11 with `iommu=pt`, 4 KB: 2288 MB/s zc vs 1685 copy. Bottleneck was per-page IOMMU mapping. https://lore.gnuweeb.org/io-uring/f1600745ba7b328019558611c1ad7684@yourcmc.ru/T/ (opened).
- Semantics: two CQEs per op (result, then `IORING_CQE_F_NOTIF` when the buffer is reusable); `IORING_RECVSEND_FIXED_BUF` with registered buffers avoids per-send pinning. https://man7.org/linux/man-pages/man3/io_uring_prep_send_zc.3.html.
- For this study: 4K replies are at or below the win threshold; 16K–64K win only with registered buffers and IOMMU passthrough or off.

zero-copy receive (zcrx, `IORING_OP_RECV_ZC`, Linux 6.15):
- Needs header/data split, flow steering, and RSS steering other flows away; TCP only. https://docs.kernel.org/networking/iou-zcrx.html (opened). Single flow on bnxt 200G: 116 Gbps vs 82 epoll. https://lists.openwall.net/netdev/2025/01/16/391.
- mlx5 support merged in 6.17 for "ConnectX-7 NICs and above"; the driver gates header/data split on the SHAMPO capability, which ConnectX-5 lacks. https://patchew.org/linux/1747950086-1246773-1-git-send-email-tariqt@nvidia.com/ (opened), verified against mainline `en_main.c`. ConnectX-5 cannot do zcrx. ice has no `queue_mgmt_ops` as of 7.3 either.
- Follow-ons: 6.16 dmabuf, 6.19 ifq sharing, 7.0 large rx buffers (30% less CPU with 32K buffers). Independent view: https://blog.tohojo.dk/2026/02/the-inner-workings-of-tcp-zero-copy.html (opened).

Rust exposure (crates.io and docs.rs checked 2026-09-01):

| Crate | Version | Released | Repo activity | Zero-copy ops |
|---|---|---|---|---|
| `io-uring` | 0.7.14 | 2026-08-11 | 2026-08-23 | `SendZc` (fixed-buffer index), `SendMsgZc`, `RecvZc`, `RecvMulti`, `ProvideBuffers`, `SendBundle`, `register_ifq`, `register_buffers`, SQPOLL, single-issuer, defer-taskrun |
| `compio` | 0.19.2 | 2026-08-18 | 2026-09-01 | `SendZc`, multishot recv on a managed buffer pool, SQPOLL; no zcrx |
| `tokio-uring` | 0.5.0 | 2024-05-27 | 2025-07-07, "seems inactive" | UDP send_zc only; TCP has read_fixed/write_fixed, no send_zc, no multishot |
| `monoio` | 0.2.4 | 2024-08-20 | 2026-05-29 | no SendZc, no RecvMulti; `zero-copy` feature sets MSG_ZEROCOPY for >= 10 MiB only |
| `glommio` | 0.9.0 | 2024-03-25 | 2025-04-21 | none |

Docs: https://docs.rs/io-uring/latest/io_uring/opcode/index.html, https://docs.rs/compio-driver/latest/compio_driver/struct.ProactorBuilder.html. Practical path: raw `io-uring` or compio, multishot recv on a provided-buffer ring for the 32 B requests, registered buffers with `SEND_ZC` for replies, `IORING_OP_READ` for the chunk file, SQPOLL optional.

### 6. RDMA from Rust, verbs shape, RoCE without PFC

Crates:

| Crate | Version | Released | Repo | Coverage |
|---|---|---|---|---|
| `ibverbs` (jonhoo) + `ibverbs-sys` | 0.9.2 | 2025-03-08 | active, last push 2026-07-09 | RC/UC/UD typed at compile time, SEND/RECV, READ/WRITE(+imm), atomics, SRQ, doorbell batching, completion channels, `rdmacm` feature; needs extended verbs (mlx5 fine, mlx4 not); RoCE needs a GID index picked by hand. https://github.com/jonhoo/rust-ibverbs |
| `sideway` | 0.4.3 | 2026-06-04 | active, last push 2026-08-13 | new `ibv_wr_*` / `ibv_start_poll` fast path, explicit GID table module, rdma_cm module, legacy post_send wrapped "no performance guarantee"; builds without rdma-core present. https://github.com/RDMA-Rust/sideway |
| `async-rdma` (datenlord) | 0.5.0 | 2023-02-01 | stale | tokio, RC + rdma_cm, GPL-3 |
| `rdma-sys` (datenlord) | 0.3.0 | 2023-02-01 | stale | raw bindings |
| `rdma` (Nugine) | 0.3.0 | 2022-06-01 | stale | RC/UD pingpong |
| `rrppcc` | 0.4.0 | 2024-04-15 | 3 stars | eRPC clone, "academic research purposes" |
| `rdma-rs` | none | | | not on crates.io |

Two-sided vs one-sided for "hash in, chunk out":
- eRPC (NSDI '19) Table 2, 32 B, same ToR: ConnectX-5 RDMA read 2.0 µs vs eRPC 2.3 µs; "at most 800 ns slower than RDMA reads". PFC disabled on the CX-5 cluster. https://arxiv.org/pdf/1806.00680 (opened).
- FaSST (OSDI '16): RPC over UD 1.7–2.15x more CPU-efficient than one-sided READs. https://www.usenix.org/system/files/conference/osdi16/osdi16-kalia.pdf (opened).
- Pilaf (ATC '13): one-sided over a remote hash index needs at least two round trips. https://www.usenix.org/system/files/conference/atc13/atc13-mitchell.pdf (opened).
- Conclusion: the client never knows the chunk's remote address, so one-sided READ costs an index round trip first. Use SEND(32 B hash), server reads NVMe, SEND(chunk) or WRITE-with-immediate into a client-registered buffer. Keep one-sided READ for a second experiment where the reply carries (addr, rkey).
- ConnectX-5 `ib_read_lat` at 4K/16K: no published table opened. Anchors: CX-5 datasheet 750 ns; eRPC 2.0 µs 32 B READ through a switch; ConnectX-7 back-to-back `ib_write_lat` 2 B 2.0 µs (https://contact.alessandrosangiorgi.net/posts/dgx-spark-nccl-collective-latency/, opened). Serialisation at 100 Gb/s: 0.33 µs for 4 KB, 1.31 µs for 16 KB, 5.2 µs for 64 KB (computed). Expect ~2.5–3 µs for a 4 KB READ back-to-back. Estimate, unmeasured.

RoCEv2 without PFC on a two-node link:
- NVIDIA CX-5 firmware 16.25 release notes define "Resilient RoCE" as "the ability to send RoCE traffic over a lossy network (a network without flow control enabled)" using ECN. https://network.nvidia.com/pdf/firmware/ConnectX5-FW-16_25_4062-release_notes.pdf (opened). The MLNX_OFED manual still says PFC is "the normal and optimal way". https://docs.nvidia.com/networking/display/mlnxofedv23100550/rdma+over+converged+ethernet+(roce) (opened). Resilient RoCE FAQ (not opened, JS-rendered).
- The knob: NVIDIA's doRoCE.sh `--lossy_buf` runs `mlxreg -y -d <bdf> --reg_name ROCE_ACCL --set "roce_adp_retrans_en=1,roce_tx_window_en=1,roce_slow_restart_en=1"`. https://github.com/NVIDIA/doroce-linux/blob/main/doRoCE.sh (opened).
- IRN (SIGCOMM '18): "the need for PFC is an artifact of current RoCE NIC designs"; unmodified go-back-N RoCE loses 1.35–3.5x under drops without PFC, selective-repeat loses under 1%. https://cs.nyu.edu/~apanda/assets/papers/sigcomm18-irn.pdf (opened).
- Back-to-back with one RC QP there is no switch buffer to overflow; the only drop sources are link errors and receiver PCIe backpressure. Run without PFC, enable adaptive retransmission and ECN (`/sys/class/net/<if>/ecn/roce_np|roce_rp`), and prove drop-free runs from `/sys/class/infiniband/mlx5_0/ports/1/hw_counters` (`out_of_sequence`, `packet_seq_err`, `local_ack_timeout_err`, `np_ecn_marked_roce_packets`). No homelab CX-5 back-to-back report was opened.

### 7. Homa, eRPC, DPDK/SPDK: necessary or overkill

- Homa/Linux (ATC '21) Table 2, kernel 5.4.80, ConnectX-4 25G, 40 nodes: 100 B RTT Homa 15.1 µs, TCP 23.4, DCTCP 24.1; kernel bypass on the same hardware 3.7 µs. The difference is polling (4 µs), SoftIRQ core selection (3–4 µs), no epoll (1 µs). https://www.usenix.org/system/files/atc21-ousterhout.pdf (opened). HomaModule perf.txt: 100 B RTT 23.2 µs on ConnectX-5 100G AMD, 14.5 µs on xl170. https://raw.githubusercontent.com/PlatformLab/HomaModule/main/perf.txt (opened). Upstreaming began Oct 2024, IP protocol 146 assigned; main tracks 6.17.
- eRPC: 2.3 µs median 32 B on CX-5; ~10M RPC/s per core; 75 Gbps with one core for large messages, >= 70% of RDMA write throughput at >= 32 kB; commenting out server copies raises it to 92 Gbps; 12 µs p99 GET at 14.3M/s (opened).
- Machnet, "Fast Userspace Networking for the Rest of Us" (arXiv 2502.09281, opened): in public cloud, Linux TCP is 2.6x / 4.2x / 7.4x higher than a DPDK stack at p50 / p99 / p99.9; footnote 3 dismisses io_uring as "batching syscalls, which is irrelevant to our latency target".
- Kernel vs DPDK HTTP on a 4 vCPU c5n: 1.0M vs 1.5M req/s, p99 333 vs 233 µs after tuning; gap narrowed from 4.2x to 1.5x. https://talawah.io/blog/linux-kernel-vs-dpdk-http-performance-showdown/ (opened).
- SPDK NVMe-oF RDMA end to end: 4.72 µs (item 4).
- Consensus reading: userspace stacks buy 10–20 µs per RPC and pay off when the RPC is the whole cost (KV stores, replication). With 70–100 µs of media under every read, none of the storage papers argue you need eRPC or DPDK for latency at 4K–64K; ReFlex and SPDK justify dataplanes by CPU per IOPS. Kernel TCP costs ~2.5–3x the CPU of RDMA at the same IOPS (SPDK CPU tables, i10 Fig. 7).

### 8. Price of a userspace daemon on the far side

- Direct comparison with the same kernel initiator (SPDK 24.05): the SPDK userspace target beats the kernel nvmet target by 2.7 µs on RDMA (9.39 vs 12.10) and 1.0 µs on TCP (20.43 vs 21.39). Systor '17: SPDK target adds 8.9 µs vs kernel 11.7. A polling userspace target is not slower than the kernel target. The hop costs latency only when it sleeps: blk-switch reports wakeups of 50–100 µs at low load; the SIGCOMM '26 paper attributes kernel TCP tail to rx scheduling.
- ublk vs kernel loop (Ming Lei, https://lwn.net/Articles/900690/, opened): 4K randread 143k vs 95k IOPS in a VM; no latency given. ublk_intro.pdf (not opened, 404).
- LightIOV (https://arxiv.org/pdf/2304.05148, opened): SPDK vhost-NVMe up to 29.4% higher latency than queue passthrough at 4K QD1; absolutes plot-only.
- NBD vs nvme-tcp latency: nothing credible found. tgt vs LIO: nothing found.
- Estimate for the study's daemon: kernel TCP RPC floor 13–23 µs RTT, plus file read completion 2–5 µs, plus a scheduler wakeup if not spinning. Same tier as kernel nvme-tcp unless the daemon polls. Unmeasured for this exact configuration.

### 9. Prefetch and batching at the transport level

No opened paper gives an "N reads ahead vs latency" table for remote block. What exists:
- Linux readahead doubles the window and fires the next window when the `PG_readahead` marker folio is touched, so a sequential guest already pipelines. https://docs.kernel.org/core-api/mm-api.html (opened).
- Ceph RBD ships librbd readahead for boot only: trigger after 10 sequential requests, 512 KiB max, disabled after 50 MiB "to allow the guest OS to take over read-ahead once it is booted". https://docs.ceph.com/en/latest/rbd/rbd-config-ref/ (opened). That is the field's implicit answer: the guest kernel does the prefetch, the transport supplies depth.
- nbdkit readahead filter: adaptive, parallel prefetch, no numbers, warns it can hurt when the kernel is already reading ahead. https://libguestfs.org/nbdkit-readahead-filter.1.html (opened).
- JuiceFS: object-store reads over 10 ms fall under 100 µs on readahead hits; 99% under 200 µs sequential; single-thread throughput 674 to 1418 MiB/s by raising the buffer that bounds concurrency. https://juicefs.com/en/blog/engineering/optimize-read-performance (opened).
- eStargz prefetches the recorded hot range in one HTTP range request before the container starts; no numbers. https://github.com/containerd/stargz-snapshotter/blob/main/docs/estargz.md (opened).
- eRPC and Homa both size the in-flight window to the bandwidth-delay product (credits, RTTbytes).
- Arithmetic for this study: 100 Gb/s x ~20 µs fabric = ~250 KB in flight; with 80 µs media, ~1.2 MB. That is ~20 x 64K or ~300 x 4K outstanding to hide remote latency fully on a sequential stream. Unmeasured.

## Answers

### (a) Consensus ranking by added latency, 4K–16K remote read, 100 GbE, QD1

On top of media, best sources in items 4, 6, 7:

1. Raw RDMA READ or two-sided verbs, polling: ~3–5 µs (eRPC 2.0–2.3 µs at 32 B; SPDK/SPDK NVMe-oF RDMA 4.72 µs at 4K).
2. SPDK target + kernel nvme-rdma initiator: ~9 µs.
3. Kernel nvme-rdma both ends: ~12 µs (SPDK 12.1; Systor 11.7 over DAS).
4. SPDK NVMe/TCP both ends: ~17.5 µs.
5. Kernel nvme-tcp both ends: ~21 µs. Kernel target vs SPDK target differs by 1 µs.
6. Userspace daemon over kernel TCP (epoll or io_uring), not spinning: ~20–30 µs. Estimate from the 13–23 µs kernel TCP ping-pong floor plus file I/O; no storage paper measured this configuration.
7. i10-style batched kernel TCP: +50–100 µs at low load.

Against 70–100 µs media the tiers are a 5–30% effect at QD1. Tails are set by wakeups. 64K adds ~5 µs of serialisation to every tier and raises TCP copy cost, which is where send-zc starts to pay.

### (b) What a solo undergraduate can build in ~40 h each

- Kernel TCP RPC in Rust: 10–20 h. One connection per core, `TCP_NODELAY`, pread or io_uring for the file.
- io_uring TCP daemon: 40 h is realistic with the `io-uring` crate. Multishot recv on a provided-buffer ring, registered buffers + `SEND_ZC` for replies, `IORING_OP_READ` for the chunk file, optional SQPOLL. send-zc wins only from ~16K and only with IOMMU passthrough. zcrx is impossible on ConnectX-5.
- nvme-tcp block export: ~4 h, zero code. nvmet file-backed namespace on the XFS chunk file, `buffered_io=0`.
- nvme-rdma export: ~8 h. Same configfs with `addr_trtype=rdma`, plus RoCE bring-up (GIDs, MTU, no PFC, adaptive retransmission via mlxreg).
- ibverbs RPC: 40 h is plausible with `ibverbs` or `sideway` for RC two-sided only. Time sinks are the QP handshake, GID selection, MR registration, CQ polling. Risky as a first verbs project.

Cheapest experiment spanning the interesting range: nvme-rdma and nvme-tcp exports of the same file (two config-only points at ~12 and ~21 µs), the Rust kernel-TCP daemon (the userspace hop), and perftest `ib_read_lat -s 4096` as the hardware floor. Four points, one of them code. Add ibverbs two-sided only if time remains.

### (c) A reviewer-credible "price of a remote chunk read" design

- Same two hosts, same NIC, same drive, same kernel for every transport. State kernel version, NIC firmware, MTU, IRQ affinity, DIM off, C-states, busy-poll settings, PFC/ECN state, and RoCE hw_counters proving zero retransmits.
- Two backends per transport: a null or RAM device (null_blk or brd) to isolate fabric plus stack cost, and the real NVMe file for end-to-end. This is the SPDK report method and is what lets the paper say "the transport costs X µs" rather than "remote is Y% slower".
- Local baseline on the far host: fio 4K/16K/64K QD1 randread `O_DIRECT` on the same file with the io_uring engine; report p50/p99/p99.9, not just mean.
- Block exports measured with fio and the same engine on the initiator. The daemon measured by a client that records per-request latency with the same clock and sizes; report spinning and sleeping variants, because the wakeup is the price being measured.
- Isolate the daemon hop: nvmet file-backed over TCP vs the daemon reading the same file over TCP. The difference is the userspace hop; the SPDK 1 µs target delta is the reference.
- QD sweep (1, 4, 16, 64) for throughput and CPU per IOPS on both ends (perf or sar). CPU efficiency is where TCP loses 2.5–3x.
- Prefetch: sequential read through the block map with 1, 2, 4, 8, 16, 32 chunks in flight at 4K and 64K; plot throughput vs depth and mark the bandwidth-delay point. Success is remote sequential throughput reaching local within the spread.
- Five runs x 30 s, drop caches between runs, medians with spread.

## Gaps

Not found or not opened: a peer-reviewed 2021–2026 table of kernel nvme-tcp vs nvme-rdma vs userspace at 100G; ConnectX-5 `ib_read_lat` at 4K; any measurement of file-backed vs bdev nvmet or the XFS extent cost; NBD vs nvme-tcp latency; a homelab CX-5 back-to-back no-PFC report; the Resilient RoCE FAQ; Sagi Grimberg's SDC 2018 nvme-tcp slides; Ming Lei's ublk_intro.pdf. The session's web search budget ran out midway, so these are "not found", not "does not exist".
