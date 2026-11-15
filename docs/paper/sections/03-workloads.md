# 3 Workloads and Corpus

We study three workloads, chosen because each creates many sandboxes from
related but distinct images.

**RL rollout environments.** One sandbox per episode, started from a per-task
image and discarded. We use SWE-bench Verified (500 tasks, 12 repositories)
[@swebench-verified] and Terminal-Bench 2 [TBD: count] [check: cite]. Creation
arrives in bursts of many distinct images.

**Coding-agent sessions.** The same disk contents as an RL episode, but
long-lived: the session idles, is suspended, and resumes, possibly on another
host. We model this with the same tasks, paused after setup. [TBD: confirm
workload definition]

**CI jobs.** One sandbox per job, built from a runner image and a repository
checkout. [TBD: N] jobs from [TBD: N] repositories across [TBD: languages]. We
expect less duplication here and include it as a contrast.

**Capture.** Each task runs in a VM with a 16 GiB ext4 disk. We capture the
whole disk at fixed points: after setup and after the task for coding tasks,
after the job for CI. Free blocks are zeroed using the file system's allocation
map so that deleted data is not counted. [check: against the capture code]

**Counting.** We fingerprint each captured disk at 4 KiB and 16 KiB fixed-size
chunks and with content-defined chunking. We count only non-zero chunks that
are absent from the sandbox's base image, so that sharing by lineage is
excluded by construction. A file manifest (path, size, extents) attributes each
chunk to a category: dependency cache, installed dependencies, toolchain, build
output, repository, system.

**Corpus.** [TBD: table. Workload, tasks, disks, bytes written beyond the
base. Pilot: 10 coding tasks, 15 CI jobs.] [pilot] We release fingerprints,
manifests and capture scripts. [TBD: what cannot be released]
