# RDMA on CloudLab c6525-100g: feasibility

Written 2026-09-01 from CloudLab docs, the cloudlab-users list, and papers that ran on these nodes. Anything I could not confirm is marked unverified.

## Short answer

RoCE between two c6525-100g nodes over the 100G experiment link works. Several groups have run it, including nvme-rdma target and initiator on exactly this pair. Nothing documents PFC or ECN on the shared Utah switches, so treat the fabric as lossy. For a two-node, single-switch test with no incast that is fine, but the paper should say so. Custom kernels are routine on CloudLab; a self-built 6.15+ kernel on the Ubuntu 24.04 image is the same procedure people already use for 6.13 and 6.17.

## 1. RoCE on c6525-100g

Evidence that it runs:

- BPF-oF (arXiv 2312.06808, section 6.1) used two c6525-100g nodes as NVMe-oF host and target over "a Dual-port Mellanox ConnectX-5 NIC with a 100 Gbps network", with both nvme-tcp and nvme-rdma. They report average roundtrip of 30 us for TCP and 18 us for RDMA. Software was Ubuntu 20.04 with a custom Linux 5.12.0. https://arxiv.org/pdf/2312.06808 and https://github.com/xrp-project/BPF-oF
- The Demikernel SOSP'21 artifact evaluation ran dmtr-rdma-server and client on c6525-100g with the small-lan profile, link speed set to 100Gbps, MLNX_OFED 5.4. https://sysartifacts.github.io/sosp2021/summaries/demikernel.html
- Emerson Ford's honors thesis has a "roce-cluster" profile (UUID fbcf91c3-93ba-11ec-9467-e4434b2381fc) used on d6515 and c6525-100g. https://github.com/emersonford/thesis
- A June 2022 list thread shows ib_write_bw and ib_read_bw on c6525-100g. The user saw 25G until they pointed perftest at the 100G device with -d. Staff (Mike Hibler) said the profile was fine. https://groups.google.com/g/cloudlab-users/c/rwXyip3mo-4
- The Demikernel CloudLab doc lists the RDMA-capable types (ConnectX-4 or newer) as Utah c6525-100g, c6525-25g, xl170, d6515 and Clemson r7525. https://github.com/microsoft/demikernel/blob/dev/doc/cloudlab.md

Lossless or lossy:

- The hardware page says nothing about RoCE, PFC, or lossless for any current Utah or Clemson switch. Its only RDMA sentence is that native InfiniBand is gone except at Apt. https://docs.cloudlab.us/hardware.html
- The one PFC thread on cloudlab-users (April 2022, "ROCE support on Mellanox switch") is a user configuring PFC themselves on a user-allocatable mlnx-sn2410 with xl170 nodes. Staff said "We do not have ONYX for those switches, they run MLNX-OS". Nobody claimed the shared switches run PFC. https://groups.google.com/g/cloudlab-users/c/YSPEN-sxvLo/m/ceMzxy-PAwAJ
- eRPC (NSDI'19) used the 200-node Utah xl170 cluster, calls it "Lossy Ethernet" in Table 1, and states "PFC is disabled in all experiments that use eRPC". They used UDP there rather than RoCE. https://arxiv.org/pdf/1806.00680
- The only switches you can make lossless yourself are the user-allocatable Dell S4048-ON and Mellanox MSN2410 units. Those attach to the 200 xl170 nodes through NetScout layer-1 switches, not to c6525. https://docs.cloudlab.us/advanced-topics.html (user-allocated switches section) and the hardware page.

My read: run it, label the fabric "lossy, PFC unverified", and keep the test point-to-point. ConnectX-5 RoCEv2 retransmits on loss, and one switch hop with two hosts will not build queues.

## 2. Alternatives with RDMA NICs and local NVMe

No CloudLab type is documented as lossless either. Candidates:

- Clemson r6525, 32 nodes. Two AMD EPYC 7543 (32 cores each), 256 GB, one 1.6 TB NVMe (Samsung PM1733 per Aquifer's Table 1), ConnectX-6 Dx 100G experiment NIC, ConnectX-5 25G control. BPF-oF used it for nvme-rdma with NIC offload. Aquifer (arXiv 2606.24079) ran RoCE over SR-IOV VFs on it with Ubuntu 24.04 stock 6.8.0. https://docs.cloudlab.us/hardware.html and https://arxiv.org/pdf/2606.24079
- Clemson r650, 32 nodes. Intel Ice Lake 72 cores, 256 GB, 1.6 TB NVMe, ConnectX-6 100G. Hardware page.
- Utah d6515, 28 nodes. AMD 7452 32 cores, 128 GB, ConnectX-5 100G with both ports for experiments, but SATA SSDs only. Used by the roce-cluster profile and Retina.
- Clemson c6420 has an Intel X710 10GbE NIC and spinning disks. No RDMA.
- Clemson r7525 has BlueField-2 2x100G plus ConnectX-5 25G, a 2 TB HDD, and GPUs. No NVMe.

## 3. Custom kernel 6.15+

Yes, and staff support it on the list:

- February 2026: a user booted a self-built Linux 6.17 on c6525-25g from the Ubuntu 24.04 image. David Johnson's fix was CONFIG_SATA_AHCI=m, then make modules_install and make install so the initramfs is generated, then check grub.cfg loads it. https://groups.google.com/g/cloudlab-users/c/DW8O4dW1pwQ
- April 2025: John Ousterhout upgraded the Ubuntu 24 image to 6.13.9. The emulab-ipod-dkms module broke on the new const proc_handler signature. CloudLab shipped a fixed package in July 2025. https://groups.google.com/g/cloudlab-users/c/H0LVeJCFoIo
- Grub must be installed in the partition 1 bootblock, not the MBR (Mike Hibler, December 2021). https://groups.google.com/g/cloudlab-users/c/Pk3DdwoToeU
- CloudLab rewrites GRUB_CMDLINE_LINUX when it loads an image. Put your parameters in GRUB_CMDLINE_LINUX_DEFAULT (David Johnson, July 2023). https://groups.google.com/g/cloudlab-users/c/ys5Moq5vesM
- Walkthrough: https://pages.cs.wisc.edu/~markm/kernel-build-cloudlab.html

Images: UBUNTU24-64-STD ships the stock Ubuntu 24.04 kernel. Aquifer states "Ubuntu 24.04 with its stock Linux 6.8.0 kernel" on a CloudLab node. UBUNTU22-64-STD and UBUNTU20-64-STD exist. Unverified: an official list of image names with kernel versions; I found none on docs.cloudlab.us. Snapshots persist a custom kernel across experiments (https://docs.cloudlab.us/advanced-topics.html, disk images). Current images have a 64 GB root partition (https://docs.cloudlab.us/advanced-storage.html).

I found no io_uring zero-copy work on CloudLab specifically. Nothing above blocks it once a 6.15+ kernel boots.

## 4. Duration, extensions, availability

- Default expiration is 16 hours. Source is Zain Ruan's MIT 6.5810 CloudLab notes ("The default expiration time is 16 hours.") and the UT CS356 setup page. The official docs only say "a few hours". https://abelay.github.io/6828seminar/notes/cloudlab.pdf, https://www.cs.utexas.edu/~venkatar/f24/cloudlab.html, https://docs.cloudlab.us/basic-concepts.html
- Extensions: "Short extensions are auto-approved, while longer ones require the intervention of CloudLab staff or, in the case of indefinite extensions, the steering committee." Numeric thresholds are unverified; the docs do not give them.
- Reservations guarantee a node count in a window. Admission control can refuse a start or extension that would collide with a future reservation. https://docs.cloudlab.us/reservations.html
- Availability: there are 36 c6525-100g nodes. Live free counts need a login (https://www.cloudlab.us/resinfo.php). The list has contention incidents for c6525, for example a December 2021 thread where the c6525-25g reservation system went oversubscribed. https://groups.google.com/g/cloudlab-users/c/6ZhHK7AG10E. Make a reservation for the pair. How contended the type is day to day is unverified.

## 5. Path between the pair

- Phase three (2021) text: "-100g" nodes have one 25Gb (Dell S5296F) and one 100Gb (Dell Z9264) experiment link, and "the experiment switches are interconnected via a single Dell Z9332 using 4-8 100Gb links each." All 36 c6525-100g 100G ports sit on that one Z9264 (64 ports of 100G), so a pair is one hop through one switch. The Z9332 uplinks only matter across node types. https://docs.cloudlab.us/hardware.html
- The 25G control link is on a separate S5296F. The 100G port is experiment-only.
- The profile must declare a link on the 100G interface with capacity set to 100Gbps or the interface stays down. From a February 2022 thread: "I have to create 2 interfaces in my profile in order to bring up the 100g interface." https://groups.google.com/g/cloudlab-users/c/82Qa3_gXZu0
- Sometimes a node does not detect its 100G NIC and needs a power cycle, which you can do yourself from the topology view (staff, October 2023). https://groups.google.com/g/cloudlab-users/c/BYP4MjqYZtE
- In an April 2025 latency thread, Mike Hibler said a c6525-100g pair was on a "Dell Z9432F switch via 400Gb -> 4 x 100Gb breakout cables" and one port showed FEC-corrected counters. So some c6525-100g ports may now hang off a Z9432F instead of the Z9264. https://groups.google.com/g/cloudlab-users/c/Yk_TAMq6I80/m/do13mtxkBAAJ
- Unverified: the Z9264F-ON non-blocking claim and its PFC/DCB support. Both Dell spec sheet mirrors I tried returned 404.

## 6. NICs

- 100G: lspci on c6525-100g shows "MT28800 Family [ConnectX-5 Ex]". The hardware page lists "Dual-port Mellanox ConnectX-5 Ex 100Gb NIC (PCIe v4.0)", one port usable for experiments. https://groups.google.com/g/cloudlab-users/c/82Qa3_gXZu0
- 25G: "Dual-port Mellanox ConnectX-5 25Gb NIC", one port for experiments, on a Dell S5296F. ConnectX-5 (MT27800) does RoCEv2, so 25G RoCE is a fallback. A 2022 thread shows ib_write_bw over 25G ConnectX ports on m510, with the warning to bind to the right --ib-dev or RDMA traffic leaks onto the control network. https://groups.google.com/g/cloudlab-users/c/qfbvPXrrOKo
- Unverified: whether the 25G S5296F port is any more lossless than the 100G one. No evidence either way.

## Not found

- Any doc, thread, or paper saying PFC or ECN is enabled on a current shared CloudLab switch.
- io_uring zero-copy receive on CloudLab.
- Official kernel versions for the Ubuntu 22.04 and 20.04 images.
