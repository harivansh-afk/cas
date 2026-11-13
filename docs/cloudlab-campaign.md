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
| `r0.sh`, `pingpong.py` | Testbed floor: fio 4 KiB QD1 direct read and 4 KiB QD1 write+fdatasync on the experiment XFS, TCP ping-pong between the nodes, the kernel nvme-tcp floor (B exports a file-backed nvmet namespace, A reads it), JSON with percentiles. |
| `campaign.py`, `campaignlib.py` | The driver (CLI and orchestration; pure logic) and its unit tests `test_campaign.py`. |
| `arms/*.json` | One config per arm; `plans/three-days.json` (r6615) and `plans/three-days-c6525.json` the day plans; `cloudlab.example.json` the node description to copy and edit. |

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
the NIC's MSI IRQs over `--irq-cpus`, record before/after), `smt` (write
`off` to `/sys/devices/system/cpu/smt/control` now and persist `nosmt` in
`/etc/default/grub.d/99-cas-nosmt.cfg` through `update-grub` for reboots;
before/after state, the command line and the online CPU count go to
`smt.json` and the inventory's `smt` block), `inventory`. Each step can be
skipped (`--skip-nix`, `--skip-build`, `--skip-disk`, `--skip-irq`,
`--skip-smt`); skipped steps appear in the inventory's `skipped` list.

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

With `--nvme-tcp` (the `r0` arm sets `nvme_tcp: true`; a campaign turns it
off with `r0_nvme_tcp: false`) the run adds the kernel nvme-tcp floor row,
following `docs/review/transport.md`: B loads `nvmet` and `nvmet-tcp`,
fills a 1 GiB file in its scratch directory on the experiment XFS, exports
it as a file-backed namespace (`buffered_io=0`, so reads are `O_DIRECT`
against the file) under the subsystem `nqn.2026-11.sh.harivan.cas:r0-floor`
on a TCP port bound to the experiment address, and records every configfs
attribute (`nvmet-target.txt`). A loads `nvme-tcp`, waits for B's gate
(ping-pong port + 1), `nvme connect`s, records the controller's sysfs
attributes and `nvme id-ctrl` (`nvme-tcp-initiator.txt`), runs the same 4
KiB QD1 direct random read with `--readonly` against the namespace device,
disconnects, and tells the gate it is done; B then removes the port,
disables the namespace and deletes the subsystem, and records that
`nvmet` is empty again. Both sides require root and refuse to start when
`nvmet` already has any subsystem or port (`nvme-tcp-prereqs.txt`).
`r0.sh --nvme-tcp-check` runs only that prerequisite check (modules, `nvme`
CLI, root, configfs, an unconfigured target) and is what the dry run uses.
The results appear under `r0.json` `nvme_tcp` with the status of each side.

## Arms

Every arm states its pinning for `r6615` (32-core EPYC 9354P), `c6525-25g`
(16-core EPYC 7302P) and `loopback-8` (the development VM). SMT is disabled
on both node types by `setup.sh`, so the sets name physical cores only and
`logical_cpus` is the core count (32 and 16). r6615: IRQs on `0-1`, daemons
on `2-5`, controller and agent on `6-7`, guests on `8-23` (sixteen one-vCPU
guests). c6525-25g: the same up to `6-7`, guests on `8-15`, so its plan
drops the sixteen-guest units and uses the eight-image `burst-8` arm. The
driver checks each set against the node's inventoried CPU count, the
overlap rules and the doc's budget (`cores × N` guest threads + daemon CPUs
+ 2), and refuses a native node whose inventory reports SMT other than off,
before any unit runs.

| Arm | Runner | Flags and placement | Cases |
| --- | --- | --- | --- |
| `frozen` | two-host | `fast_settled_reads` false, `peer_race_us` null, k=1, burst owner cache 0 | reads-memory/storage at 1/4/8/16 guests, burst-images, resume-elsewhere, flows-k1, raw-rsync, zfs-incremental, independent-stores |
| `fast` | two-host | `--fast-settled-reads` | same |
| `raced` | two-host | fast plus `--peer-race-us RACE_BUDGET_US`, k=2 | same |
| `warm-owner` | two-host | fast, owner cache 2 GiB (`cache_bytes` and `burst.owner_cache_bytes`) | same |
| `burst` | two-host | fast, k=1, 16 images × (1 GiB base + 64 MiB unique), working set from the bundle in a `/mnt/cas` chroot | burst-images |
| `burst-8` | two-host | the same with 8 images, for c6525-25g | burst-images |
| `resume` | two-host | fast, k=1, 16 GiB setup in a 32 GiB image, working set from the bundle in a `/mnt/cas` chroot | resume-elsewhere |
| `native-raw` | native | `cas-native-run --backend raw --guests N`, `workload.sh baseline 30 512` | g1, g4, g8, g16 |
| `native-cas` | native | `cas-native-run --backend cas --guests N` | g1, g4, g8, g16 |
| `native-zfs` | native | `cas-native-run --backend raw` with storage on a ZFS dataset (fast dedup, 4 KiB recordsize, compression off; the dataset and its device are the node's `zfs_storage_mount`/`zfs_device`) | g1, g4, g8, g16 |
| `r0` | r0.sh | floor, with the nvme-tcp row | floor |

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
`actions.json` named by `nodes.bundle_actions` (the swe bundle object with
`cwd`, `env`, `interpreter`, `timeout` and `steps`), verbatim, together
with the recorded `cwd` and `env`; the file's hash and action count are
recorded in `fills.working_set`. The `burst`, `burst-8` and `resume` arms
write `"working_set": {"chroot": "/mnt/cas", "bundle": "WORKING_SET_FROM_BUNDLE"}`,
which the driver merges into the two-host runner's object form: each
command runs as `chroot /mnt/cas /bin/bash -lc 'cd <cwd> && <cmd>'` under
exactly the recorded environment, so the data disk must be a root
filesystem image that contains `/bin/bash` and the trajectory's `/testbed`.
Preparing that image (the corpus's Stage A disk, imported with `casctl
import`) is outside this driver.

Known gaps the configs name rather than hide: `cas-native-run` boots up to
sixteen guests, each with its own disk and fio job, but the single-host
comparison at 4/8/16 guests is still a one-host arm; the `native-zfs` arm
needs a node with a ZFS pool on the second NVMe (`zfs_storage_mount` and
`zfs_device` in the nodes file) and is blocked without one; the nvme-tcp row
needs `nvme` CLI, the `nvmet`/`nvme-tcp` modules and root on both nodes.

## campaign.py

```sh
cp experiments/cloudlab/cloudlab.example.json /var/tmp/nodes.json   # edit
python3 experiments/cloudlab/campaign.py dry-run --nodes /var/tmp/nodes.json \
  --plan experiments/cloudlab/plans/three-days.json
python3 experiments/cloudlab/campaign.py start --nodes /var/tmp/nodes.json \
  --plan experiments/cloudlab/plans/three-days.json --export experiment@archive:/srv/evidence
python3 experiments/cloudlab/campaign.py status --nodes /var/tmp/nodes.json
```

The nodes file names the campaign, the revision it is bound to
(`expected_revision`: the example carries `INTEGRATION_REVISION_PLACEHOLDER`,
to be replaced by the integration branch's measured commit once
`feat/burst-harness` has merged there; `run` refuses the placeholder and any
node whose checkout differs from it or is dirty, loopback development
layouts may omit it), the export destination, the bundle, an optional
explicit race budget, `r0_nvme_tcp` and two or more nodes (node 0 is the
local controller; the others have `ssh` targets whose host keys are already
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

`plans/three-days-c6525.json` is the same plan for a c6525-25g pair without
the g16 units, with `burst-8` for `burst`, and without `raced burst-images`
(the raced arm's burst block is sixteen images).

The predictions document's "repeats of anything with spread over 15%" is
`campaign.py repeats --nodes N.json --out day4-repeats.json [--threshold
0.15]`: it reads every finished unit, takes one p99-class number per unit
(reads cases: the quiet random QD1 cohort's clat p99; burst: the slowest
guest's cold time to working set; resume: time to working set, or downtime
without a working set; r0: node A's 4 KiB read clat p99; native: the
`read-q1` aggregate's largest per-guest p99), computes (max − min) / median
over each (day, arm, case)'s repetitions and writes a day-4 plan listing
every group above the threshold with its original repetition count, plus
the per-group spread and the units without a usable number. Run the same
campaign directory again with that plan; the new units are `d4-...`.

## Rehearsal

`uv run --no-project python -m unittest discover -s experiments/cloudlab`
covers the plan loader, arm and node validation, pinning rules,
placeholders, the race-budget rule, unit states and the driver's
skip/refuse/redo path. `plans/dev-loopback.json` with a `loopback: true`
nodes file (both members on one host, no ssh) rehearses the driver on a
development VM; its evidence is labelled `development` in every manifest
and `unit.json`. See [validation](validation.md) for the recorded run.
