//! The mandatory scenarios are also the execution inventory; a skip cannot pass.
pub const CHECKS: &[(&str, &[&str])] = &[
    ("rust-format", &["cargo", "fmt", "--all", "--", "--check"]),
    (
        "clippy",
        &[
            "cargo",
            "clippy",
            "--workspace",
            "--all-targets",
            "--locked",
            "--",
            "-D",
            "warnings",
        ],
    ),
    ("rust-tests", &["cargo", "test", "--workspace", "--locked"]),
    ("diff-check", &["git", "diff", "HEAD", "--check"]),
    ("nix-format", &["nix", "fmt", "--", "--ci"]),
    ("nix-check", &["nix", "flake", "check"]),
];

type Vm = (&'static str, &'static str, &'static [&'static str]);

const REFERENCES: &[Vm] = &[
    ("raw-qemu", "raw", &[]),
    ("raw-daemon", "daemon", &[]),
    ("staging", "staging", &[]),
    ("fresh-recovery", "staging", &["--recovery"]),
    (
        "live-before-submit",
        "staging",
        &["--live-recovery", "--crash-at", "before-submit"],
    ),
    (
        "live-after-storage",
        "staging",
        &["--live-recovery", "--crash-at", "after-storage"],
    ),
    (
        "live-after-status",
        "staging",
        &["--live-recovery", "--crash-at", "after-status"],
    ),
    (
        "live-after-used",
        "staging",
        &["--live-recovery", "--crash-at", "after-used"],
    ),
];

const PACKED: &[Vm] = &[
    ("local-sync", "local", &[]),
    ("local-fresh-recovery", "local", &["--recovery"]),
];

const CONCURRENT: &[Vm] = &[
    ("async-io", "async", &[]),
    ("async-fresh-recovery", "async", &["--recovery"]),
    ("async-device-reset", "async", &["--device-reset"]),
    (
        "async-after-prepared",
        "async",
        &["--live-recovery", "--crash-at", "after-prepared"],
    ),
    (
        "async-after-active",
        "async",
        &["--live-recovery", "--crash-at", "after-active"],
    ),
    (
        "async-before-submit",
        "async",
        &["--live-recovery", "--crash-at", "before-submit"],
    ),
    (
        "async-after-append-cqe",
        "async",
        &["--live-recovery", "--crash-at", "after-append-cqe"],
    ),
    (
        "async-before-sync",
        "async",
        &["--live-recovery", "--crash-at", "before-sync"],
    ),
    (
        "async-after-sync",
        "async",
        &["--live-recovery", "--crash-at", "after-sync"],
    ),
    (
        "async-after-storage",
        "async",
        &["--live-recovery", "--crash-at", "after-storage"],
    ),
    (
        "async-after-status",
        "async",
        &["--live-recovery", "--crash-at", "after-status"],
    ),
    (
        "async-after-used",
        "async",
        &["--live-recovery", "--crash-at", "after-used"],
    ),
    (
        "async-repeat-replay",
        "async",
        &[
            "--live-recovery",
            "--crash-at",
            "before-submit",
            "--replay-crash-at",
            "after-replay-append",
            "--replay-restarts",
            "2",
        ],
    ),
    (
        "async-repeat-before-fence",
        "async",
        &[
            "--live-recovery",
            "--crash-at",
            "before-submit",
            "--replay-crash-at",
            "before-recovery-fence",
            "--replay-restarts",
            "2",
        ],
    ),
    (
        "async-repeat-after-fence",
        "async",
        &[
            "--live-recovery",
            "--crash-at",
            "before-submit",
            "--replay-crash-at",
            "after-recovery-fence",
            "--replay-restarts",
            "2",
        ],
    ),
];

pub fn vms(checkpoint: &str) -> Vec<Vm> {
    let mut cases = REFERENCES.to_vec();
    if matches!(checkpoint, "C2" | "C3") {
        cases.extend_from_slice(PACKED);
    }
    if checkpoint == "C3" {
        cases.extend_from_slice(CONCURRENT);
    }
    cases
}

pub fn required(checkpoint: &str) -> Vec<&'static str> {
    let mut required: Vec<_> = CHECKS
        .iter()
        .map(|(id, _)| *id)
        .chain(vms(checkpoint).into_iter().map(|(id, _, _)| id))
        .collect();
    if checkpoint == "C3" {
        required.push("persistence-model");
    }
    required
}
