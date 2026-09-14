//! Interpret filesystem and libtest observations, not merely process exit codes.
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

use serde_json::Value;

use super::Build;
use crate::evidence::{self, require, u64_at};

const SECTORS: u64 = 16 * 1024 * 1024 / 512;

mod collection;
mod space;

#[derive(Debug)]
struct Extent {
    physical: u64,
    sectors: u64,
    shared: bool,
}

fn extents(text: &str) -> io::Result<Vec<Extent>> {
    let mut result = Vec::new();
    let mut next = 0;
    let number = |text: &str| text.parse::<u64>().map_err(io::Error::other);
    for line in text.lines().filter(|line| line.contains("..")) {
        let fields: Vec<_> = line.split_whitespace().collect();
        require(fields.len() == 5, "unexpected FIEMAP record")?;
        let logical = fields[1].trim_start_matches('[').trim_end_matches("]:");
        let (start, end) = logical
            .split_once("..")
            .ok_or_else(|| io::Error::other("FIEMAP logical range"))?;
        let (physical, physical_end) = fields[2]
            .split_once("..")
            .ok_or_else(|| io::Error::other("FIEMAP physical range"))?;
        let sectors = number(fields[3])?;
        require(
            sectors > 0
                && sectors <= SECTORS
                && number(start)? == next
                && number(end)?.checked_add(1) == next.checked_add(sectors),
            "FIEMAP hole or overlap",
        )?;
        let physical = number(physical)?;
        require(
            number(physical_end)?.checked_add(1) == physical.checked_add(sectors),
            "FIEMAP physical length",
        )?;
        let flags = u32::from_str_radix(fields[4].trim_start_matches("0x"), 16)
            .map_err(io::Error::other)?;
        require(
            flags & !0x2001 == 0,
            "FIEMAP has unresolved or unsupported extents",
        )?;
        result.push(Extent {
            physical,
            sectors,
            shared: flags & 0x2000 != 0,
        });
        next += sectors;
    }
    require(next == SECTORS, "FIEMAP did not cover the complete file")?;
    Ok(result)
}

fn sectors(extents: &[Extent]) -> impl Iterator<Item = (u64, bool)> + '_ {
    extents
        .iter()
        .flat_map(|e| (0..e.sectors).map(move |offset| (e.physical + offset, e.shared)))
}

fn reflink(source: &str, clone: &str, changed: bool) -> io::Result<()> {
    let source = extents(source)?;
    let clone = extents(clone)?;
    for (logical, ((a, a_shared), (b, b_shared))) in
        sectors(&source).zip(sectors(&clone)).enumerate()
    {
        let private = changed && logical < 8;
        require(
            if private {
                a != b && !a_shared && !b_shared
            } else {
                a == b && a_shared && b_shared
            },
            "reflink sharing or 4 KiB COW isolation differs",
        )?;
    }
    Ok(())
}

fn tests(module: &str, list: &str, log: &str) -> io::Result<()> {
    let prefix = format!("{module}::");
    let expected: BTreeSet<_> = list
        .lines()
        .filter_map(|line| line.strip_suffix(": test"))
        .collect();
    require(
        !expected.is_empty() && expected.iter().all(|name| name.starts_with(&prefix)),
        "missing or wrong native test inventory",
    )?;
    let actual: Vec<_> = log
        .lines()
        .filter_map(|line| line.strip_prefix("test ")?.strip_suffix(" ... ok"))
        .collect();
    require(
        actual.len() == expected.len()
            && actual.iter().copied().collect::<BTreeSet<_>>() == expected,
        "native tests were skipped, duplicated or failed",
    )?;
    require(
        log.lines().any(|line| {
            line.starts_with(&format!(
                "test result: ok. {} passed; 0 failed; 0 ignored; 0 measured;",
                expected.len()
            ))
        }),
        "native test summary disagrees with inventory",
    )
}

pub(super) fn verify(output: &Path, build: &Build) -> io::Result<()> {
    let guest = output.join("guest");
    let exit: Value = evidence::read_json(&output.join("guest-exit.json"))?;
    require(
        exit["exit_code"] == 0 && exit["error"].is_null(),
        "guest process failed",
    )?;
    evidence::read_json::<evidence::GuestCompletion>(&guest.join("completion.json"))?.verify()?;
    let qemu: Value = evidence::read_json(&output.join("qemu.json"))?;
    require(
        qemu["executable"].as_str() == build.qemu_executable.to_str(),
        "unexpected QEMU executable",
    )?;
    let argv = qemu["argv"]
        .as_array()
        .ok_or_else(|| io::Error::other("QEMU argv missing"))?;
    require(
        argv.first().and_then(Value::as_str) == build.qemu.to_str(),
        "unexpected QEMU launch command",
    )?;
    require(
        argv.iter().any(|v| {
            v.as_str()
                .is_some_and(|s| s.split(',').any(|option| option == "accel=kvm"))
        }) || argv.iter().any(|v| v == "-enable-kvm"),
        "fixture requires native KVM",
    )?;
    require(
        fs::read_to_string(guest.join("kernel.log"))?.contains(&build.guest_kernel),
        "wrong guest kernel",
    )?;
    let mount: Value = evidence::read_json(&guest.join("mount.json"))?;
    let mounts = mount["filesystems"]
        .as_array()
        .ok_or_else(|| io::Error::other("fixture mount missing"))?;
    require(
        mounts.len() == 1 && mounts[0]["target"] == "/fixture" && mounts[0]["fstype"] == "xfs",
        "fixture is not XFS",
    )?;
    if matches!(
        build.workload,
        super::Workload::Shared | super::Workload::Pressure
    ) {
        require(build.memory_mib == 4096, "shared outer RAM differs")?;
        let scenario = evidence::read_json(&guest.join("scenario.json"))?;
        return crate::shared::verify(
            &guest.join("shared"),
            &scenario,
            build.live_recovery,
            build.workload == super::Workload::Pressure,
        );
    }
    require(build.memory_mib == 2048, "core outer RAM differs")?;
    let hashes = |file| -> io::Result<Vec<String>> {
        fs::read_to_string(guest.join(file))?
            .lines()
            .map(|line| {
                let (hash, _) = line
                    .split_once(' ')
                    .ok_or_else(|| io::Error::other("invalid SHA256 evidence"))?;
                require(
                    hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit()),
                    "invalid SHA256",
                )?;
                Ok(hash.to_owned())
            })
            .collect()
    };
    let before = hashes("before.sha256")?;
    let after = hashes("after.sha256")?;
    require(
        before.len() == 2
            && after.len() == 2
            && before[0] == before[1]
            && before[0] == after[0]
            && after[0] != after[1],
        "clone mutation changed the source or did not change clone",
    )?;
    for (phase, changed) in [("before", false), ("after", true)] {
        reflink(
            &fs::read_to_string(guest.join(format!("source-{phase}.fiemap")))?,
            &fs::read_to_string(guest.join(format!("clone-{phase}.fiemap")))?,
            changed,
        )?;
    }
    for (module, namespace) in [
        ("space", "space::filesystem::tests"),
        ("store", "store::file::tests"),
        ("manifest", "manifest::file::tests"),
        ("append", "append::shared::tests"),
        ("catalog", "catalog"),
        ("runtime", "local::host::tests"),
    ] {
        tests(
            namespace,
            &fs::read_to_string(guest.join(format!("{module}.list")))?,
            &fs::read_to_string(guest.join(format!("{module}.log")))?,
        )?;
    }
    space::verify(&guest)?;
    collection::verify(&guest)?;
    Ok(())
}

#[cfg(test)]
mod controls {
    use super::*;

    #[test]
    fn ordinary_copy_holes_and_shared_mutation_fail() {
        let shared = "0: [0..32767]: 192..32959 32768 0x2001\n";
        let copy = "0: [0..32767]: 40000..72767 32768 0x1\n";
        let source = "0: [0..7]: 192..199 8 0x0\n1: [8..32767]: 200..32959 32760 0x2001\n";
        let clone = "0: [0..7]: 40000..40007 8 0x0\n1: [8..32767]: 200..32959 32760 0x2001\n";
        reflink(shared, shared, false).unwrap();
        reflink(source, clone, true).unwrap();
        for (a, b, changed) in [
            (shared, copy, false),
            (shared, shared, true),
            (source, copy, true),
            ("", shared, false),
        ] {
            assert!(reflink(a, b, changed).is_err());
        }
        assert!(extents(&shared.replace("[0..", "[1..")).is_err());
        assert!(extents(&shared.replace("0x2001", "0x2003")).is_err());
    }

    #[test]
    fn success_requires_every_enumerated_test() {
        let list =
            "store::file::tests::a: test\nstore::file::tests::b: test\n\n2 tests, 0 benchmarks\n";
        let log = "running 2 tests\ntest store::file::tests::a ... ok\ntest store::file::tests::b ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n";
        tests("store::file", list, log).unwrap();
        for invalid in [
            log.replace("b ... ok", "b ... FAILED"),
            log.replace("b ... ok", "a ... ok"),
            log.replace("0 ignored", "1 ignored"),
        ] {
            assert!(tests("store::file", list, &invalid).is_err());
        }
        assert!(tests("store::file", "", log).is_err());
        assert!(tests("manifest::file", list, log).is_err());
    }
}
