# Claims and what each rests on

Each claim is a sentence the paper wants to print. Each row under it is
something that must be true first; "rests on" is what that row itself needs.
The CAS and ZFS columns say what must be run on each system; "none" means the
row is offline analysis of captured disks. Status is as of 15 November 2026.

## C0. VM images are almost never reused, and operators place by load

| Need | Rests on | CAS | ZFS | Status |
|---|---|---|---|---|
| Published image fanout | DSec's exact definition of fanout | none | none | Number read in source; definition unchecked |
| Operators place by load | DSec and Sandman statements | none | none | Both read in source; Sandman only partly |
| No counter-example in production | Two unread papers (Sun 2018, Graviet 2026) | none | none | Open |

## C1. X% of the bytes a task writes already exist on a disk with no shared lineage

| Need | Rests on | CAS | ZFS | Status |
|---|---|---|---|---|
| Enough captured disks | Capture runner restarted; target sample size fixed in advance | none | none | 73 coding tasks, 25 CI jobs; capture stopped |
| Lineage excluded | Chunks present in the base image are not counted | none | none | Done in the pilot analysis; re-check on full corpus |
| Deleted data not counted | Free blocks zeroed from the allocation map | none | none | Done in capture; re-check against code |
| Result independent of chunking | Same analysis at 4 KiB, 16 KiB and CDC | none | none | Pilot only |
| Result independent of arrival order | Analysis repeated under shuffled orders | none | none | Not done |
| Read side: blocks read before the first command | Which blocks each VM reads at start. The corpus has no read traces. | Start each real image in the store and count local hits and fetches | none | Not recorded; measurable only once real disks run on the nodes, or by adding read tracing to capture |
| Analysis matches a real store | Stored bytes after ingesting the corpus equal the predicted unique bytes | Import all disks; read stored bytes | Write all disks to one pool with deduplication; read allocated bytes | Not done |

## C2. The duplicates are mostly package data, and simpler schemes leave D% of them

| Need | Rests on | CAS | ZFS | Status |
|---|---|---|---|---|
| Duplicate bytes by category | File manifest with extents for every disk | none | none | Pilot only |
| One image per repository | A stated rule for building that image from the corpus | none | none | Not defined |
| Shared package cache | A stated rule for which paths the cache covers | none | none | Not defined |

## C3. Under load-based placement Y% of duplicates are on another host; under affinity, Z%

| Need | Rests on | CAS | ZFS | Status |
|---|---|---|---|---|
| The C1 corpus | C1 | none | none | See C1 |
| Placement policies defined | Load-based as DSec describes it; affinity by repository; points between | none | none | Random and affinity exist; load-based not defined |
| Load imbalance per policy | A stated imbalance metric and a task-duration model | none | none | Not defined |
| Simulation is believable | A real run on four hosts matches the simulation at four hosts | Four-host fleet; place real disks by each policy; bytes stored per host and bytes fetched | Four independent pools; same placements; bytes allocated per pool | Needs the 22-25 Nov reservation (pending), real disks on nodes, a four-node driver |

## C4. On one host, the store is within A% of ZFS deduplication in bytes and B% in latency

| Need | Rests on | CAS | ZFS | Status |
|---|---|---|---|---|
| Same workload on both | Real task disks and their I/O, or a stated synthetic workload | Run at 1, 4, 8, 16 guests | Run at 1, 4, 8, 16 guests | Synthetic workload only; ZFS and raw runners exist for 1 guest only |
| Equal settings | Same NVMe model, 4 KiB chunk and record size, compression off, same guest shape | Chunk size 4 KiB | `recordsize=4K`, deduplication on | Configured in the arm table; unverified on hardware |
| Testbed floor | Raw read, `fdatasync` and TCP round trip measured first | none | none | Runner exists; not run |
| Bytes stored | Measured after compaction and after the pool settles | Stored bytes after compaction | Allocated bytes and table size | Not run |
| Latency | Read, write and FLUSH percentiles, five repetitions, arm order rotated | Per-guest histograms | Per-guest histograms | Not run |
| Index memory | Both measured the same way | Fingerprint index bytes | Deduplication table bytes in memory | CAS reports it; ZFS method not chosen |
| No sharing baseline | Raw XFS at the same guest counts | none | none | Runner exists for 1 guest only |

## C5. Across hosts, a cold start moves M bytes and a remote read costs L microseconds

| Need | Rests on | CAS | ZFS | Status |
|---|---|---|---|---|
| Remote read cost | Guest-visible read of a chunk held only by the other host, from memory and from disk | Two hosts; read-by-source arms | none | Runner exists; development numbers only |
| Cold-start burst | Real distinct images on a host that holds none of them | Import real images on host A; start on host B | Full copy of each image as the control | Arm uses synthetic images |
| Resume on another host | A real paused task disk and its first recorded commands | Send manifest; replay commands; bytes fetched | `zfs send` full and incremental; replay commands | Arm uses a synthetic image; controls exist |
| Honest lineage case | Where disks share a snapshot, incremental send is reported even if it wins | none | Incremental send with a common snapshot | Development run: ZFS moved fewer bytes |
| Hedged requests | A loaded source and a second source | k = 2, hedge after the measured median | none | Optional; cut if the effect is unclear |

## C6. Per-host deduplication wins up to placement P and fleet size H; beyond it the fleet store wins

| Need | Rests on | CAS | ZFS | Status |
|---|---|---|---|---|
| Bytes stored per design and policy | C3 for the split; C1 "analysis matches a real store" | Fleet unique bytes plus replicas | Sum of per-host unique bytes | Not done |
| Bytes moved per start | C3 and C5 | Chunks the host lacks | Whole image, or incremental where lineage exists | Not done |
| Time cost of a miss | C5 remote read cost applied to the misses C3 predicts | From C5 | Local read from C4 | Not done |
| Memory cost | C4 index memory scaled to the corpus | Fingerprint index per host | Deduplication table per host | Not done |
| A stated model | How the four costs combine, fixed before the runs | none | none | Not written |

## Order

C0 and C1 gate everything. C2 and C3 need only the corpus. C4 and C5 need the
nodes. C6 needs all of them and is written last.
