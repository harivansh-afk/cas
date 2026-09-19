import type { SystemNode } from '../graph';

/** Everything that builds, launches, measures and records: casctl, cas-harness, Nix, CI and the evidence chain. */
export const tooling: SystemNode[] = [
	{
		id: 'tooling',
		title: 'Build, run, measure',
		sub: 'Nix · casctl · cas-harness · evidence',
		what: 'The outer system. Nix pins the toolchain, builds the binaries and assembles NixOS guests; cas-harness launches QEMU and the daemon, drives workloads and refuses evidence that is incomplete or unbound from its source; casctl is the interactive front door; every result lands in docs/ as a record the site imports.',
		how: ['A run is one Nix wrapper that execs the packaged harness with a JSON build record naming the guest launcher, the daemon and the source revision.', 'Nothing in this layer produces research-gate evidence: every report hard-codes paper_gate: null. Checkpoints C1–C5 are development milestones.'],
		files: ['flake.nix', 'justfile', 'crates/harnesses/README.md'],
		docs: ['testbed', 'implementation']
	},
	{
		id: 'workspace',
		parent: 'tooling',
		title: 'Rust workspace',
		sub: 'four crates · pinned 1.98.1',
		what: 'cas-core, cas-daemon, cas-harness and cas-cli, edition 2024, with two vendored rust-vmm crates patched in through Cargo. The harness and CLI do not link the daemon crate: they run cas-daemon and cas-host by store path from build records.',
		how: ['Workspace lints deny unsafe_op_in_unsafe_fn and undocumented unsafe blocks.', 'just check runs rustfmt, clippy with warnings denied, the tests and a whitespace check; the same six checks open every checkpoint suite.'],
		numbers: [['native tests on main', '504 passed, 25 fixture-dependent ignored']],
		files: ['Cargo.toml', 'rust-toolchain.toml', 'crates/harnesses/Cargo.toml', 'crates/cas/cli/Cargo.toml']
	},
	{
		id: 'casctl',
		parent: 'tooling',
		title: 'casctl',
		sub: 'named labs · SSH · bench · census',
		tone: 'outline',
		what: 'The interactive CLI. casctl new boots a lab: a 4 GiB KVM outer VM owning an XFS disk and cas-host, with one to four TCG inner guests on private ext4 disks over one shared store. A user systemd unit owns the lab after the command returns.',
		how: [
			'Per lab: config.json, a Nix GC root for the pinned build, a sparse 4 GiB disk.raw, an ed25519 key and runs/<timestamp>/ with logs, memory samples and SSH config.',
			'Inside the outer VM, lab-host runs cas-host init once, then cas-host with rotating telemetry, and launches each inner guest with its own socket; SSH reaches vmN through a loopback port and a ProxyJump.',
			'bench runs four fio cases over SSH at QD1 and keeps every command, output and the memory and storage samples around it.',
			'staging-check exercises the v1 log; census hashes raw images at 4 KiB and 16 KiB.'
		],
		numbers: [['preflight', '/dev/kvm, ≥ 25 GiB free, ≥ 6 GiB available'], ['unit limits', 'MemoryMax 6G, no swap']],
		files: ['crates/cas/cli/src/main.rs', 'crates/harnesses/src/lab.rs', 'crates/harnesses/src/lab/client.rs', 'crates/harnesses/src/lab/runtime.rs', 'crates/harnesses/src/lab/bench.rs', 'nix/lab/host.nix', 'nix/lab/guest.nix'],
		docs: ['casctl']
	},
	{
		id: 'harness',
		parent: 'tooling',
		title: 'cas-harness',
		sub: 'launch · verify · bind to source',
		tone: 'outline',
		what: 'The Rust experiment driver. Every subcommand owns its child process group, applies deadlines, decodes guest and daemon reports into typed records with hard acceptance conditions, and records the exact source and executables that produced them.',
		how: ['SIGINT and SIGTERM set a flag every poll loop checks; a child group is killed on drop, exit or timeout.', 'A run refuses to start if the running binary is not the one in the build record, and refuses to pass if the checkout changed during the run.'],
		files: ['crates/harnesses/src/main.rs', 'crates/harnesses/src/process.rs', 'crates/harnesses/src/evidence.rs', 'crates/harnesses/src/source.rs'],
		docs: ['testbed']
	},
	{
		id: 'h-vm',
		parent: 'harness',
		title: 'vm runner',
		sub: 'smoke · recovery · live recovery · reset',
		what: 'One daemon plus one KVM guest against a fresh 128 MiB scratch image. Smoke runs fio with verification; recovery kills the daemon after a confirmed FLUSH and reads back in a fresh guest; live recovery pauses the daemon at a named fault point, kills it, optionally cycles replacement daemons, and requires the same guest to finish; device reset unbinds and rebinds the driver twice.',
		numbers: [['guest', '1024 MiB, 2 or 4 vCPUs, KVM'], ['IO', '64 MiB written and read; 128 MiB verified']],
		files: ['crates/harnesses/src/vm.rs', 'crates/harnesses/src/vm/live.rs', 'crates/harnesses/src/vm/reset.rs', 'crates/harnesses/src/vm/interactive.rs', 'crates/harnesses/src/qemu.rs'],
		docs: ['testbed', 'shared-crash-controls']
	},
	{
		id: 'h-suite',
		parent: 'harness',
		title: 'checkpoint suite',
		sub: 'C1 14 · C2 16 · C3 32 · C4 40 · C5 41',
		tone: 'outline',
		what: 'The cumulative scenario inventory. Six source checks, then the reference and crash-point VM runs, the persistence model from C3, the XFS and shared fixtures with six compaction crash cuts from C4, and the competing-guest pressure fixture at C5. verify-suite re-scans everything from disk.',
		how: ['Each scenario runs from a Nix wrapper bound to the same source path and flake.lock; a mismatch fails the scenario.', 'The C5 suite last passed in full on b8399ad; four scheduler and compactor changes have merged since with focused checks only.'],
		files: ['crates/harnesses/src/suite.rs', 'crates/harnesses/src/suite/scenarios.rs', 'nix/checkpoints.nix', 'nix/run-checkpoints.sh'],
		docs: ['checkpoint-c4', 'hosted-checkpoints']
	},
	{
		id: 'h-fixture',
		parent: 'harness',
		title: 'XFS + shared fixtures',
		sub: 'reflink proof · native tests in a guest · two guests',
		what: 'The XFS fixture boots a KVM guest with a real XFS disk, proves reflink and FIEMAP behaviour, and runs the native cas-core and cas-daemon test inventories there. The shared fixture nests two TCG guests with ext4 and SQLite over one cas-host, optionally SIGKILLs the host at a compaction cut and replaces it retained.',
		files: ['crates/harnesses/src/fixture.rs', 'crates/harnesses/src/fixture/checks.rs', 'crates/harnesses/src/shared.rs', 'crates/harnesses/src/shared/restart.rs', 'crates/harnesses/src/filesystem.rs', 'nix/fixture/workload.sh', 'nix/shared/outer.nix'],
		docs: ['xfs-fixture', 'shared-guest-fixture', 'compaction-crash-cuts']
	},
	{
		id: 'h-pressure',
		parent: 'harness',
		title: 'pressure harness',
		sub: '11 stages · file protocol · telemetry',
		what: 'Two guests and a controller exchange ready, request and completed files per stage: shared and disjoint reads, hot-set scans, displacement, four fio write shapes, calibration of the compactor\'s drain rate and a burst above it. It requires a sample with staging stopped, fresh-boot digests that match, and pool limits that equal the code defaults.',
		files: ['crates/harnesses/src/shared/pressure.rs', 'crates/harnesses/src/shared/pressure/checks.rs', 'crates/harnesses/src/pressure/guest.rs'],
		docs: ['guest-pressure', 'pressure-diagnosis']
	},
	{
		id: 'h-persistence',
		parent: 'harness',
		title: 'persistence oracle',
		sub: '841 crash schedules · no VM',
		what: 'A deterministic model of the WAL crash contract. It builds a small log, records every complete-batch prefix as an oracle image, then for each of 841 schedules persists a subset of the unsynced tail sectors (truncations, reversals, torn sectors, header-only, payload-only, shuffles), cold-opens the copy and requires the recovered prefix to be at least the required one and to read exactly like its oracle.',
		files: ['crates/harnesses/src/persistence.rs', 'crates/harnesses/src/persistence/schedule.rs', 'crates/harnesses/src/persistence/oracle.rs', 'crates/harnesses/src/persistence/verify.rs'],
		docs: ['persistence-model']
	},
	{
		id: 'h-fleet',
		parent: 'harness',
		title: 'census fleet + preflight',
		sub: 'cloud images · T0/T1/T2 · host inventory',
		tone: 'muted',
		what: 'The measurement behind Update 01: boots dated Ubuntu cloud images and clones through two upgrade epochs, normalizes their roots and runs the census. preflight records 14 host probes and check-disks proves the OS and data disks are distinct whole devices.',
		files: ['crates/harnesses/src/fleet.rs', 'crates/harnesses/src/host.rs', 'experiments/update-guest.sh', 'experiments/normalize-root.sh', 'nix/census.nix'],
		docs: ['census']
	},
	{
		id: 'nix',
		parent: 'tooling',
		title: 'Nix',
		sub: 'flake · guests · fixtures · host modules',
		tone: 'outline',
		what: 'The flake builds the workspace with the pinned toolchain, one NixOS guest per backend, the fixture and lab VMs, the checkpoint bundle and the census tools, and exports host modules for dedicated test machines. Every wrapper carries a build record so the harness can bind evidence to a source revision.',
		files: ['flake.nix', 'nix/package.nix', 'nix/smoke.nix', 'nix/checkpoints.nix', 'nix/census.nix', 'nix/lab/default.nix', 'nix/fixture/default.nix'],
		docs: ['testbed']
	},
	{
		id: 'nix-guest',
		parent: 'nix',
		title: 'NixOS guests',
		sub: 'smoke · fixture · lab · shared',
		what: 'Prerendered guest systems: an ephemeral tmpfs root, the pinned fio jobs under /etc/cas, a oneshot that runs the workload and a finish hook that writes completion.json and powers off. The QEMU command line comes from the pinned qemu-vm module plus the per-backend disk device.',
		how: ['Raw: -drive cache=none,aio=io_uring with virtio-blk-pci and 4 KiB logical blocks.', 'Daemon and host: -chardev socket plus vhost-user-blk-pci with num-queues=1,queue-size=128 or 4×256, and shared memory enabled so guest RAM is mappable.', 'Results travel through a 9p share; SSH keys and host keys are exchanged over the same share.'],
		files: ['nix/guest/default.nix', 'nix/guest/smoke.sh', 'nix/guest/finish.sh', 'nix/shared/guest.nix', 'nix/fixture/guest.nix', 'nix/lab/guest.sh', 'crates/harnesses/fio/smoke.fio']
	},
	{
		id: 'nix-host',
		parent: 'nix',
		title: 'Test-host modules',
		sub: 'disko · XFS at /srv/cas-testbed',
		tone: 'muted',
		what: 'NixOS modules for the dedicated hosts G1 still needs: key-only SSH, performance governor, an ESP plus ext4 root on the OS disk and one XFS partition on the data disk, installed with nixos-anywhere from the template. The two hosts do not yet exist.',
		files: ['nix/modules/test-host.nix', 'nix/modules/disks.nix', 'nix/modules/bare-metal.nix', 'templates/test-host/flake.nix', 'nix/checks/host-config.nix'],
		docs: ['testbed']
	},
	{
		id: 'ci',
		parent: 'tooling',
		title: 'CI',
		sub: 'GitHub mirror only',
		tone: 'muted',
		what: 'Three workflows on the GitHub mirror; Forgejo Actions stay off. implementation runs fmt, clippy and tests; nix formats, evaluates both architectures separately, builds every check and runs the smoke, recovery and live-recovery VMs plus the dev-vm SSH script; a manual dispatch runs the full C5 suite. pages would publish the site but is disabled.',
		files: ['.github/workflows/implementation.yml', '.github/workflows/nix.yml', '.github/workflows/pages.yml'],
		docs: ['ci-evaluation', 'hosted-checkpoints']
	},
	{
		id: 'evidence',
		parent: 'tooling',
		title: 'Evidence chain',
		sub: 'results/ → docs/validation → JSON → site',
		tone: 'outline',
		what: 'How a number reaches a page. Raw artifacts stay under ignored results/ on Spark or in an archive; each session appends to docs/validation.md and larger ones get their own record; measurements keep a README, the analysis script and its JSON; the site imports that JSON directly, so no number on a page is typed by hand.',
		how: ['TODO.md is the canonical tracker; an item is checked only when its acceptance record exists.', 'Retention: keep small evidence and failed attempts, delete VM disks after independent verification, never launch below 25 GiB free.'],
		files: ['TODO.md', 'AGENTS.md', 'docs/validation.md', 'docs/artifact-retention.md', 'docs/measurements/integration-2026-09-15/display.py', 'playbook/src/lib/articles.ts'],
		docs: ['validation', 'artifact-retention']
	}
];
