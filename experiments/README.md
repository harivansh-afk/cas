# Experiment artifacts

`preflight.py` records the host environment, tool versions, repository revision,
and worktree state. It reports missing commands without substituting another
measurement. Run it through `uv` as shown in the root README.

Spark is a development host. The specification names a pair of CloudLab
c6525-100g nodes for paper measurements, with a dedicated NVMe under test.
Development checks do not supply R0 or establish any hypothesis.

The Nix flake now supplies a reusable development guest, a pinned QEMU launcher,
and a guest fio write/readback check. [The testbed guide](../docs/testbed.md)
describes its outputs and the dedicated-host template. Before R0 measurements,
add the workload matrix, CPU affinity, per-run filesystem/cache controls, and
device-counter capture. Run the same guest jobs against the passthrough daemon
for G1. Host-side fio alone cannot establish guest-visible latency.

Each measured repetition must retain:

- Host and guest inventories, exact source revision, dependency lockfiles,
  kernel/module/firmware versions, device identity, and run configuration.
- QEMU and fio commands; seeds; timestamps; warm-up and measurement intervals.
- Original fio `json+` histograms and host device counters before and after.
- Cache state, compactor state, chunk-size arm, durability class, and ownership
  mode where applicable.
- Failure records, exclusions with reasons, and the analysis version used to
  derive the table. Repetitions are kept separately; percentiles are not averaged
  into a pooled latency percentile.

The source specification requires at least five repetitions and spread beside
each result. Save raw artifacts under a new `results/<run>/` directory; commit
the scripts and configurations, and archive large results separately. A table
entry remains unmeasured until its corresponding correctness gate has passed.

The fio JSON+ format includes latency-bin counts for later analysis.
[fio documentation](https://fio.readthedocs.io/en/latest/fio_doc.html#json-output)
