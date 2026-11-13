# CloudLab campaign driver

`experiments/cloudlab/` turns the three reserved node-days into an unattended,
resumable, evidence-complete run of the arm table, so the nodes spend their
time measuring. It drives the existing [two-host runner](two-host-experiments.md)
and [native runner](native-benchmark.md); it adds no measurement of its own
beyond the R0 floor. Everything it records is bound to one source revision
and one set of binary hashes per campaign.

| File | Role |
| --- | --- |
| `setup.sh` | Per-node preparation: record hardware and settings first, install Nix, build the pinned revision from a git bundle, make one XFS on the named experiment disk, raise the descriptor limit, pin the NIC's IRQs, write `node-inventory.json`. `--inventory-only` records and changes nothing. |
| `inventory.py` | Assembles `node-inventory.json` from `setup.sh`'s raw records. |
| `r0.sh`, `pingpong.py` | Testbed floor: fio 4 KiB QD1 direct read and 4 KiB QD1 write+fdatasync on the experiment XFS, TCP ping-pong between the nodes, JSON with percentiles. |
| `campaign.py`, `campaignlib.py` | The driver (CLI and orchestration; pure logic) and its unit tests `test_campaign.py`. |
| `arms/*.json` | One config per arm; `plans/three-days.json` the day plan; `cloudlab.example.json` the node description to copy and edit. |

## setup.sh

```sh
sudo bash experiments/cloudlab/setup.sh \
  --device /dev/disk/by-id/nvme-SERIAL --mount /mnt/cas-exp \
  --bundle /var/tmp/cas.bundle --revision <sha> --source /home/experiment/cas \
  --nic enp65s0f0np0 --irq-cpus 0-1 --user experiment --out /var/tmp/node-inventory.json
```

Steps, each logged in `<out>-records/setup.log` and `steps.jsonl` with the
SHA-256 of every record file after the step: `hardware` (lscpu, lsblk,
`nvme id-ctrl`/`id-ns`/`smart-log` where present, ethtool link/driver/
firmware/rings/coalescing/channels, MTU, NUMA node, MSI IRQ list, the full
IRQ affinity map, cpufreq governors and limits, cpuidle states and
`max_cstate`, SMT state, THP, kernel version and command line, DMI/BIOS
strings, `dmidecode`, fstab, mounts, limits, KVM, Nix/fio/ZFS versions, load
and top processes), `nix` (upstream installer, `--daemon`, installer hash
recorded; flakes enabled), `checkout` (clone or fetch the bundle, detach at
the revision, require a clean tree equal to the requested SHA), `build`
(`nix build .#two-host` and `.#native`, store paths and binary hashes into
`build.json`), `disk`, `limits` (`limits.d` entry, 65,536 descriptors for
the experiment account), `irq` (stop irqbalance for the session, round-robin
the NIC's MSI IRQs over `--irq-cpus`, record before/after), `inventory`.
Each step can be skipped (`--skip-nix`, `--skip-build`, `--skip-disk`,
`--skip-irq`); skipped steps appear in the inventory's `skipped` list.

What it refuses to do: it never partitions anything and never touches a
disk that may hold data. The device must be given (`--device`, an absolute
`/dev` path, no default), must resolve to a whole disk (not a partition),
must not be the disk under `/`, `/boot`, `/boot/efi`, `/nix`, `/home` or
`/var`, must have no partitions or children, no filesystem type, no mount,
no `wipefs` signature and no RAID membership. The one exception is
idempotency: an XFS filesystem whose root carries `.cas-experiment-disk`
written by this script for this hostname is remounted and reused. Any other
XFS is refused as "without our marker; it may contain data". The rule is
therefore: **a human names the device by `/dev/disk/by-id/` serial after
checking `lsblk` and the CloudLab node's boot device; the script only checks
that the named disk is empty.** On an r6615 the two NVMe devices are
identical models; the OS lives on one of them and CloudLab's own local
datasets may already occupy the other, in which case the script refuses and
the human must decide whether to release that dataset.

## r0.sh

```sh
bash r0.sh --mount /mnt/cas-exp --output /var/tmp/r0/a --source ~/cas --cpus 6-7 \
  --role client --peer 10.10.1.2:17490 --seconds 30     # on A; B runs --role server
```

fio (the flake's pinned fio, found through `nix develop` when it is not on
`PATH`) writes a 1 GiB scratch file in a fresh directory under the mount,
then runs 30 s of 4 KiB QD1 direct random reads and 30 s of 4 KiB QD1 random
writes with `fdatasync=1`, both `json+` with percentiles 50/90/99/99.9/99.99.
The ping-pong uses `sockperf` if installed, else `pingpong.py`: a 64-byte
echo over one TCP connection with `TCP_NODELAY` on both ends, 1,000 warm-up
round trips, then timed round trips for the duration; its JSON is labelled
`python-tcp-pingpong` with the note that it includes interpreter overhead and
is not a NIC floor. `r0.json` holds `read_4k_qd1`, `fdatasync_4k_qd1` (the
write completion and the `sync` distributions separately) and `pingpong`.
The scratch directory is removed; nothing is formatted.

## Arms

Every arm states its pinning for `r6615` (32-core EPYC 9354P; the sets
assume SMT on, 64 logical CPUs), `c6525-25g` (16-core EPYC 7302P; sets
assume SMT on, 32 logical CPUs) and `loopback-8` (the development VM).
The sets are the same in every arm: IRQs on `0-1`, daemons on `2-5`,
controller and agent on `6-7`, guests on `8-23` (sixteen one-vCPU guests).
The driver checks each set against the node's inventoried CPU count, the
overlap rules and the doc's budget (`cores × N` guest threads + daemon CPUs
+ 2) before any unit runs. **These sets are a proposal**: on an r6615 they
use physical cores 0-23 only; on a c6525-25g with SMT on, CPUs 16-23 are the
siblings of 0-7, so the human should either disable SMT and drop the
sixteen-guest units or accept sibling sharing and say so. The `logical_cpus`
field makes the assumption explicit and the dry run fails if the inventory
disagrees.

| Arm | Runner | Flags and placement | Cases |
| --- | --- | --- | --- |
| `frozen` | two-host | `fast_settled_reads` false, `peer_race_us` null, k=1, burst owner cache 0 | reads-memory/storage at 1/4/8/16 guests, burst-images, resume-elsewhere, flows-k1, raw-rsync, zfs-incremental, independent-stores |
| `fast` | two-host | `--fast-settled-reads` | same |
| `raced` | two-host | fast plus `--peer-race-us RACE_BUDGET_US`, k=2 | same |
| `warm-owner` | two-host | fast, owner cache 2 GiB (`cache_bytes` and `burst.owner_cache_bytes`) | same |
| `burst` | two-host | fast, k=1, 16 images × (1 GiB base + 64 MiB unique), working set `WORKING_SET_FROM_BUNDLE` | burst-images |
| `resume` | two-host | fast, k=1, 16 GiB setup in a 32 GiB image, working set `WORKING_SET_FROM_BUNDLE` | resume-elsewhere |
| `native-raw` | native | `cas-native-run --backend raw`, `workload.sh baseline 30 512` | g1, g4, g8, g16 |
| `native-cas` | native | `cas-native-run --backend cas` | g1, g4, g8, g16 |
| `native-zfs` | none | raw image on ZFS fast dedup; no runner exists | g1, g4, g8, g16 (all blocked) |
| `r0` | r0.sh | floor | floor |

The runner's own `repeats` is 1 in every arm; repetition and rotation are the
campaign's. `fast`, `raced`, `warm-owner`, `burst` and `resume` carry
`requires_runner_settings`; the driver reads the built runner's source from
its Nix store path and blocks those units when the runner does not know the
setting, because a runner that ignores `fast_settled_reads` would silently
measure the frozen path under the fast label. The burst-harness revision's
runner lacks both settings; the integration branch's runner has them.

`RACE_BUDGET_US` is filled once per campaign, from `nodes.race_budget_us`
if set, else as the median over the finished frozen `reads-memory` units of
the quiet random QD1 cohort's merged clat p50 (fio bucket lower bound,
rounded to microseconds); sources and the rule are recorded in
`manifest.json` `fills.race_budget` and never recomputed. A raced unit
scheduled before any frozen reads-memory unit has finished is blocked.
`WORKING_SET_FROM_BUNDLE` is the first ten `command` strings of the
`actions.json` named by `nodes.bundle_actions`, verbatim, with the file's
hash and action count recorded in `fills.working_set`. The commands run in
the guest with `sh -eu -c` from `/mnt/cas`; whether the guest layout matches
the trajectory's `/testbed` is a decision for whoever prepares the image.

Known gaps the configs name rather than hide: `cas-native-run` boots one or
two guests on XFS only, so `native-raw`/`native-cas` at g4+ and every
`native-zfs` case are blocked; CAS load per host at 4/8/16 guests comes from
the two-host `reads-*-gN` arms; there is no nvme-tcp floor row.

## campaign.py

```sh
cp experiments/cloudlab/cloudlab.example.json /var/tmp/nodes.json   # edit
python3 experiments/cloudlab/campaign.py dry-run --nodes /var/tmp/nodes.json \
  --plan experiments/cloudlab/plans/three-days.json
python3 experiments/cloudlab/campaign.py start --nodes /var/tmp/nodes.json \
  --plan experiments/cloudlab/plans/three-days.json --export experiment@archive:/srv/evidence
python3 experiments/cloudlab/campaign.py status --nodes /var/tmp/nodes.json
```

The nodes file names the campaign, the export destination, the bundle, an
optional explicit race budget and two or more nodes (node 0 is the local
controller; the others have `ssh` targets whose host keys are already
verified). The campaign directory is `<node 0 output_root>/<campaign>`; it
holds `campaign.log`, `status.json`, `manifest.json`, `inventory/{start,end}/`,
`dry-run.json` or `preflight.json`, and `units/<id>/` with `unit.json`,
`config.json`, `runner.log`, the controller output and every remote node's
agent output pulled back as `host-<name>/`.

A day plan is `{"days": [{"day": 1, "units": [[arm, case, repetitions], ...]}]}`
(or a bare unit list for one day). Within a day the repetition is the outer
loop and the arm order rotates by one per repetition, so no arm always runs
first after an idle night or last behind a hot drive. Unit IDs are
`d<day>-<arm>-<case>-r<rep>`.

`run` (what `start` detaches, through `systemd-run --unit cas-campaign-<name>`
or `nohup setsid` when there is no systemd): snapshot both inventories
(`setup.sh --inventory-only`), write or verify the manifest (revision and
`git status` on every node, the wrapper's build-info, SHA-256 of `cas-host`,
`cas-fleet`, QEMU and the native binaries, equal on every node or the run
stops; arm config, plan, nodes-file and driver hashes), run the preflight
over the whole schedule, export the campaign-level files, then for each
scheduled unit: skip it if its outputs show success, refuse it if outputs
exist without success (`--redo` moves them to `<id>.attempt-N` and reruns),
otherwise run it, write `unit.json`, compute `SHA256SUMS` over the unit
tree and rsync it to `<export>/<campaign>/units/<id>/`. Afterwards the end
inventories are taken and the campaign-level files are exported again. A
unit failure is recorded and the next unit starts; `status.json` is updated
after every change and `campaign.py status` prints one screen: header,
counts, current unit, one line per unit. A second `run` while the recorded
PID is alive is refused.

`--dry-run` performs the inventories, the manifest and the preflight and
launches nothing: for every scheduled unit it checks the arm has a pinning
set for each node's type and that it fits the inventoried CPU count, that
the storage mount is XFS on every node, that the two-host or native runner
binary, `r0.sh`, `pingpong.py`, fio and `workload.sh` exist where needed,
that required runner settings are supported by the built runner, that
`zfs-incremental` units have a `zfs_parent`, that the bundle resolves, and
that the race budget has a source. The result is `dry-run.json` and a
nonzero exit if any unit is blocked.

## The day plan as the driver reads it

`plans/three-days.json`, expanded by `campaignlib.schedule` (repetition outer,
order rotated):

- Day 1: `r0 floor` once; then five rounds over `frozen reads-memory`,
  `frozen reads-storage`, `fast reads-memory`, `fast reads-storage`,
  `native-raw g1`, `native-cas g1`, `native-zfs g1` (blocked), the start
  of each round shifted by one arm.
- Day 2: five rounds over `fast reads-storage-g4/g8/g16`, `fast
  reads-memory-g16`, `raced reads-storage`, `raced reads-storage-g16`,
  `warm-owner reads-memory`, `warm-owner reads-memory-g16`, `native-raw
  g4/g16` and `native-cas g4/g16` (blocked).
- Day 3: five rounds over `burst burst-images`, `resume resume-elsewhere`,
  `raced burst-images`, `frozen raw-rsync`, `frozen zfs-incremental`, and
  three of `frozen flows-k1`.

The predictions document's "repeats of anything with spread over 15%" is a
human decision at the end of day three: add the units to a fourth-day plan
and run the same campaign directory again.

## Rehearsal

`uv run --no-project python -m unittest discover -s experiments/cloudlab`
covers the plan loader, arm and node validation, pinning rules,
placeholders, the race-budget rule, unit states and the driver's
skip/refuse/redo path. `plans/dev-loopback.json` with a `loopback: true`
nodes file (both members on one host, no ssh) rehearses the driver on a
development VM; its evidence is labelled `development` in every manifest
and `unit.json`. See [validation](validation.md) for the recorded run.
