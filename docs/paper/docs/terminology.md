# Terminology

One term per concept. Each term is used exactly as written here, defined once
in the section named, and never reworded. A term marked **paper** is ours and
is defined explicitly in the text; every other term follows the cited source.
Sources were opened and the usage checked on 15 November 2026 unless marked
`[check]`.

## Storage

| Term | Meaning | Follows | Defined in |
|---|---|---|---|
| block | The guest's 4 KiB unit of disk I/O. | virtio-blk usage | Background |
| chunk | The store's unit of deduplication; fixed 4 KiB here unless stated. | LBFS: files divided into chunks indexed by hash | Background |
| fingerprint | The collision-resistant hash of a chunk, used as its address. | Venti (Quinlan and Dorward, FAST '02) | Background |
| content-addressed store | A store whose chunks are addressed by fingerprint. | Venti | Background |
| deduplication | Storing one copy of chunks with equal fingerprints. | Meyer and Bolosky (FAST '11); OpenZFS `zfsconcepts(7)` | Background |
| duplicate chunk, unique chunk | A chunk whose fingerprint is, or is not, already stored. | Brooker et al. (ATC '23) | Background |
| fixed-size chunking | Chunk boundaries at fixed offsets. | Meyer and Bolosky ("fixed block") | Block fanout |
| content-defined chunking (CDC) | Chunk boundaries chosen from the data. | LBFS (SOSP '01) | Block fanout |
| inline deduplication | Duplicates removed on the write path. | El-Shimi et al. (ATC '12); OpenZFS ("removed synchronously") | Background |
| post-process deduplication | Duplicates removed after the write is acknowledged. | El-Shimi et al. (ATC '12) | Background |
| deduplication table | ZFS's per-pool map from fingerprint to stored block. | OpenZFS docs `[check: exact wording]` | Background |
| fingerprint index | Our store's map from fingerprint to chunk location. | DDFS (Zhu et al., FAST '08) | System |
| manifest | The per-image list of chunk fingerprints by offset. | Brooker et al. ("manifest of chunks") | System |
| snapshot | A read-only point-in-time copy of an image. | OpenZFS `zfsconcepts(7)` | Background |
| clone | A writable image whose initial contents equal a snapshot. | OpenZFS `zfsconcepts(7)` | Background |
| write-ahead log (WAL) | The per-image log that makes writes durable before compaction. | standard | System |
| compaction | Moving logged writes into the chunk store and publishing the manifest. | standard (log-structured stores) | System |
| on-demand loading | Fetching a chunk when it is first read, not before start. | Brooker et al. (title); Slacker ("lazily") | Background |
| hedged request | A second request sent to another replica after a delay; first reply wins. | Dean and Barroso (CACM 2013) | Cost |

## Workload and fleet

| Term | Meaning | Follows | Defined in |
|---|---|---|---|
| VM | A Linux virtual machine with its own virtual disk holding a file system. Each runs one task. | standard | Introduction |
| task | One unit of agent work run in one VM: an RL episode, a coding-agent session or a CI job. DSec uses "task" for a job that creates many VMs; we do not. | **paper** | Introduction |
| image | The disk a VM starts from. | DSec | Introduction |
| host | One physical machine running VMs. | standard | Introduction |
| fleet | All hosts that can run a given task. | **paper** | Introduction |
| cold start | Starting a VM on a host that holds none of its image. | Brooker et al. | Background |
| image fanout | The number of VMs that use one image within one job. | DSec, Figure 8 (checked) | Introduction |
| block fanout | The number of VMs whose disks contain a given chunk. | **paper**, by analogy with image fanout | Block fanout |
| lineage | Two disks share lineage if one is a clone of a snapshot of the other, or both are clones of a common snapshot. | **paper** | Background |
| placement policy | The rule that chooses a host for a VM. | standard | Placement |
| load-based placement | Placement that considers only host load. | DSec ("selects the least loaded") | Placement |
| affinity placement | Placement that prefers hosts already holding similar content. | SMIO (ICS '13); Kubernetes ImageLocality | Placement |
| same-host duplicate, other-host duplicate | A duplicate chunk whose earlier copy is on the same host, or only on other hosts. | **paper** | Placement |
| replication factor (k) | The number of hosts that store each chunk. | standard | System |

## Words we do not use

| Not this | Use | Why |
|---|---|---|
| sandbox | VM | "Sandbox" covers processes, containers and VMs and cannot be defined precisely. It appears only in the titles of cited work. |
| census | deduplication analysis | repository coinage |
| novel bytes | unique chunks | repository coinage |
| raced read | hedged request | Dean and Barroso named it |
| shipping | replication | repository coinage |
| settled | compacted | repository coinage |
| resume elsewhere, burst-images, Frozen, Fast | describe the experiment in words | arm names are not vocabulary |
| lazy fetch, lazy pull | on-demand loading | one term per concept |
| hash (as a noun for a chunk's identity) | fingerprint | one term per concept |
| first, novel, nobody has | state what was measured | see decisions.md |
