# Corpus

Fixed on 18 November 2026, before capture. A change to this file after capture
starts is recorded as a dated line under Changes, with the reason.

## Rule

A random sample with a published seed from named public task sets. A task that
fails to build or replay is not replaced. It is reported as excluded, with the
reason, per group.

Seed for every draw: `20261118`.

## Groups

| Group | Source | Draw | Disks |
|---|---|---|---|
| Coding, Python | SWE-bench Verified | 10 tasks per repository | 101 |
| Coding, other languages | Multi-SWE-bench | 20 tasks per language, 7 languages | 140 |
| Coding, unrelated by design | Terminal-Bench 2.0 | all tasks | 89 |
| RL environments, Python | SWE-Gym | 5 tasks per repository | 55 |
| RL environments, other languages | Multi-SWE-RL | 10 tasks per language, 7 languages | 70 |
| RL rollouts | 20 tasks drawn from the tasks above that have at least 5 published agent trajectories | 5 runs each, each replaying a different trajectory | 100 |
| CI | Repositories drawn above | 5 repositories per language, the main Linux test job at the task's commit | 30 |

Full size is 585 disks: 156 Python, 210 in seven other languages, 89 mixed,
100 rollouts and 30 CI. The first pass halves every draw with the same seed, so
the first pass is a subset of the full corpus.

An RL environment is a task from a set built to train agents: a repository at a
commit, a problem and tests that give a reward. An RL rollout is one attempt at
a task. Environments measure sharing between different training tasks. Rollouts
measure sharing between attempts at one task. The sources that supply rollout
tasks are reported, since only some publish five trajectories per task.

A repository or language with fewer tasks than its draw contributes all of them.

## Capture

- Coding and RL tasks are captured twice, after setup and at the end.
- A task is replayed from a published agent trajectory with no model in the
  loop. Terminal-Bench 2.0 tasks run the solution shipped with the task.
- Versions are pinned to the source's published image or lock file. Runs with
  live package indexes are a separate measurement of drift.
- Free blocks are zeroed before fingerprinting.

## What counts as a duplicate

A 4 KiB block that belongs to a file present on the disk, whose content is on
another disk in the corpus, and that the two disks do not share by lineage.

Each duplicate is labelled, and labels are never summed into one figure.

| Label | Content | Used in claims |
|---|---|---|
| Discardable | Package indexes, logs, temporary files | Reported only |
| Needed | Installed software, downloaded packages, the repository, build output | Yes |

Content-defined chunking is reported once as an upper bound.

## Why these sources

DSec and Sandman name SWE-bench, SWE-Gym and Terminal-Bench as suites they run.
The selection follows Meyer and Bolosky (random sample from a named frame, bias
stated) and Slacker (exclusions counted). Sources and what was read are in the
private review `2026-11-18-corpus-selection.md`.

## Limits

- No machine learning training workloads. No public set has them with
  replayable runs.
- Draws are equal per repository and per language, not weighted by use. No
  public usage figures exist.
- The CI rule is untested. A repository without a usable Linux test job is an
  exclusion.
- A replay is tidier than a live agent.

## Pilot

The 83 SWE-bench Verified disks and 15 CI disks captured from 10 to 13
November were chosen before this rule. They are a pilot and are not part of the
corpus. Two findings from them shaped the labels above. conda's package index
was 73.5% of the bytes the 84 coding tasks wrote, and none of the 93 task
Dockerfiles read removes a package cache.

## Changes

- 2026-11-18, before any capture. Multi-SWE-bench raised from 10 to 20 per
  language and RL environments split between SWE-Gym and Multi-SWE-RL. The
  first version was 211 Python disks against 70 in other languages. Rollout
  tasks now require five published trajectories.
