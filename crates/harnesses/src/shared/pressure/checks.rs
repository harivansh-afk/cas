//! Recompute acceptance from mandatory per-stage records and original observations.
use super::*;
use std::collections::BTreeSet;

fn require(ok: bool, message: &str) -> io::Result<()> {
    if ok {
        Ok(())
    } else {
        Err(io::Error::other(message))
    }
}
pub(super) fn number(value: &Value, path: &str) -> io::Result<u64> {
    value
        .pointer(path)
        .and_then(Value::as_u64)
        .ok_or_else(|| io::Error::other(format!("missing unsigned counter: {path}")))
}
fn array<'a>(value: &'a Value, path: &str) -> io::Result<&'a Vec<Value>> {
    value
        .pointer(path)
        .and_then(Value::as_array)
        .ok_or_else(|| io::Error::other(format!("missing array: {path}")))
}
fn budget(value: &Value, bytes: u64, requests: u64) -> io::Result<()> {
    for (name, limit) in [("bytes", bytes), ("requests", requests)] {
        let current = number(value, &format!("/current/{name}"))?;
        let peak = number(value, &format!("/peak/{name}"))?;
        require(
            current <= peak && peak <= limit,
            "reported budget exceeds its configured cap",
        )?;
    }
    Ok(())
}
fn pools(value: &Value, host: bool) -> io::Result<()> {
    let mib = 1024 * 1024;
    budget(&value["append"], if host { 64 * mib } else { 8 * mib }, 0)?;
    budget(&value["read"], if host { 64 * mib } else { 8 * mib }, 0)?;
    budget(&value["requests"], 0, if host { 1024 } else { 128 })?;
    budget(
        &value["control"],
        if host { 256 * 1024 } else { 64 * 1024 },
        if host { 32 } else { 8 },
    )
}
pub(super) fn sample(value: &Value) -> io::Result<()> {
    require(
        value["schema_version"] == 1 && value["phase"] == "live",
        "wrong live telemetry schema",
    )?;
    let host = &value["host"];
    require(
        host.get("failure") == Some(&Value::Null),
        "host failure or missing health",
    )?;
    pools(&host["pools"], true)?;
    budget(&host["metadata"], 128 * 1024 * 1024, 0)?;
    budget(&host["compaction_metadata"], 128 * 1024 * 1024, 0)?;
    for (name, bytes) in [
        ("cache", 4 * 1024 * 1024),
        ("metadata_cache", 16 * 1024 * 1024),
    ] {
        require(
            number(&host[name], "/capacity_bytes")? == bytes,
            "cache configuration differs",
        )?;
        budget(&host[name]["payload"], bytes, 0)?;
        require(
            number(&host[name], "/resident_bytes")? + number(&host[name], "/reader_held_bytes")?
                <= number(&host[name], "/payload/current/bytes")?,
            "cache owners exceed charged payload",
        )?;
        for key in ["hits", "misses", "fills", "evictions", "refused"] {
            number(&host[name], &format!("/counters/{key}"))?;
        }
    }
    let staging = &host["staging"];
    require(
        number(staging, "/capacity")? == 32 * 1024 * 1024
            && number(staging, "/allocated")? + number(staging, "/promised")? <= 32 * 1024 * 1024,
        "staging exceeds fixture cap",
    )?;
    for key in [
        "background",
        "demand",
        "background_requested_bytes",
        "demand_requested_bytes",
    ] {
        number(host, &format!("/io_scheduler/counters/{key}"))?;
    }
    for key in ["started", "joined", "completed", "failed"] {
        number(host, &format!("/fetches/counters/{key}"))?;
    }
    require(
        number(host, "/admission_scheduler/quantum_bytes")? == 1024 * 1024,
        "admission quantum differs",
    )?;
    require(
        array(host, "/admission_scheduler/images")?.len() == 2,
        "missing image admission counters",
    )?;
    let compaction = array(host, "/compaction")?;
    require(compaction.len() == 2, "missing image compaction counters")?;
    for totals in compaction {
        for field in [
            "batches",
            "input_bytes",
            "active_ns",
            "candidate_output_bytes",
            "deferred",
            "deferred_ns",
            "failed",
            "failed_ns",
        ] {
            number(totals, &format!("/{field}"))?;
        }
        require(
            totals["failed"] == 0,
            "unexpected failed compaction attempt",
        )?;
    }
    let images = array(value, "/images")?;
    require(images.len() == 2, "missing image telemetry")?;
    for (index, image) in images.iter().enumerate() {
        require(
            image["image"] == IMAGES[index],
            "image telemetry identity differs",
        )?;
        let report = &image["report"];
        require(
            report["errors"] == 0 && report.get("fatal_error") == Some(&Value::Null),
            "image error or missing health",
        )?;
        number(report, "/pending")?;
        pools(&report["local"], false)?;
        let status = &report["local"]["status"];
        require(status["failed"] == false, "local image failed")?;
        let d = number(status, "/compacted")?;
        let e = number(status, "/durable")?;
        require(
            d <= e
                && e <= number(status, "/published")?
                && number(status, "/published")? <= number(status, "/issued")?,
            "image prefix order differs",
        )?;
    }
    Ok(())
}
pub(super) fn drained(value: &Value) -> io::Result<bool> {
    for image in array(value, "/images")? {
        let status = &image["report"]["local"]["status"];
        let issued = number(status, "/issued")?;
        if number(status, "/compacted")? != issued
            || number(status, "/durable")? != issued
            || number(&image["report"], "/pending")? != 0
        {
            return Ok(false);
        }
    }
    Ok(number(value, "/host/compaction_metadata/current/bytes")? == 0)
}
pub(super) fn drain_rate(before: &Value, after: &Value) -> io::Result<f64> {
    let totals = |value: &Value, field: &str| -> io::Result<u64> {
        array(value, "/host/compaction")?
            .iter()
            .try_fold(0u64, |sum, v| {
                sum.checked_add(number(v, &format!("/{field}"))?)
                    .ok_or_else(|| io::Error::other("compaction counter overflow"))
            })
    };
    let bytes = totals(after, "input_bytes")?
        .checked_sub(totals(before, "input_bytes")?)
        .ok_or_else(|| io::Error::other("compaction bytes decreased"))?;
    let ns = totals(after, "active_ns")?
        .checked_sub(totals(before, "active_ns")?)
        .ok_or_else(|| io::Error::other("compaction time decreased"))?;
    require(
        bytes >= 2 * pressure::WRITE_BYTES && ns > 0,
        "calibration lacks complete payload drain",
    )?;
    Ok(bytes as f64 * 1e9 / ns as f64)
}
pub(super) fn completed(value: &Completed, stage: Stage, image: u8) -> io::Result<()> {
    require(
        value.stage == stage && value.image == image && value.elapsed_ns > 0 && value.bytes > 0,
        "stage result identity/progress differs",
    )?;
    if stage.fio().is_some() || matches!(stage, Stage::Calibration | Stage::Burst) {
        require(
            value.read_latency.is_none()
                && value.bytes
                    == stage
                        .fio()
                        .map_or(pressure::WRITE_BYTES, |control| control.size_bytes),
            "write byte count differs",
        )?;
    } else {
        let latency = value
            .read_latency
            .as_ref()
            .ok_or_else(|| io::Error::other("missing checked-read histogram"))?;
        require(
            latency.count > 0
                && latency.count * 4096 == value.bytes
                && latency.log2_microseconds.iter().sum::<u64>() == latency.count
                && latency.total_ns >= latency.maximum_ns
                && latency.maximum_ns > 0,
            "read histogram differs from checked IO count",
        )?;
        let expected = if stage == Stage::SharedMiss {
            4096
        } else {
            2 * pressure::SET_BYTES
        };
        require(
            stage == Stage::ScanHot || value.bytes == expected,
            "incomplete fixed read workload",
        )?;
        if stage == Stage::ScanHot {
            require(
                value.elapsed_ns >= 4_000_000_000,
                "scan/hot runtime incomplete",
            )?;
        }
    }
    Ok(())
}
pub(super) fn memory_bytes(text: &str, key: &str) -> io::Result<u64> {
    let fields = text
        .lines()
        .find(|line| line.starts_with(key))
        .ok_or_else(|| io::Error::other("missing smaps field"))?
        .split_whitespace()
        .collect::<Vec<_>>();
    require(fields.len() == 3 && fields[2] == "kB", "wrong smaps units")?;
    fields[1]
        .parse::<u64>()
        .map_err(io::Error::other)?
        .checked_mul(1024)
        .ok_or_else(|| io::Error::other("smaps overflow"))
}
fn memory(path: &Path) -> io::Result<Value> {
    let mut samples = 0;
    let mut previous = None;
    let mut identities = None;
    let mut peak_pss = 0;
    for line in BufReader::new(File::open(path)?).lines() {
        let value: Value = serde_json::from_str(&line?)?;
        let time = number(&value, "/elapsed_ns")?;
        require(
            previous.is_none_or(|last| time > last),
            "memory timestamps are not increasing",
        )?;
        previous = Some(time);
        let processes = array(&value, "/processes")?;
        require(
            processes.len() == 3 && value["guest_ram_capacity_bytes_each"] == 512 * 1024 * 1024,
            "memory cohort/configuration differs",
        )?;
        let actual: BTreeSet<_> = processes
            .iter()
            .map(|p| {
                (
                    p["pid"].to_string(),
                    p["start_ticks"].to_string(),
                    p["executable"].to_string(),
                )
            })
            .collect();
        require(
            actual.len() == 3
                && identities
                    .as_ref()
                    .is_none_or(|expected| expected == &actual),
            "memory process identity changed",
        )?;
        identities = Some(actual);
        let mut total = 0;
        for process in processes {
            let raw = process["smaps_rollup"]
                .as_str()
                .ok_or_else(|| io::Error::other("missing raw smaps"))?;
            let rss = memory_bytes(raw, "Rss:")?;
            let pss = memory_bytes(raw, "Pss:")?;
            require(
                rss >= pss
                    && rss == number(process, "/rss_bytes")?
                    && pss == number(process, "/pss_bytes")?,
                "memory summary differs from raw smaps",
            )?;
            total += pss;
        }
        require(
            total == number(&value, "/cohort_pss_bytes")?,
            "PSS cohort sum differs",
        )?;
        peak_pss = peak_pss.max(total);
        samples += 1;
    }
    require((2..=4096).contains(&samples), "memory coverage incomplete")?;
    Ok(serde_json::json!({"samples":samples,"peak_cohort_pss_bytes":peak_pss}))
}
fn file_inventory(report: &Value) -> io::Result<(String, String)> {
    let mut expected = vec![
        ("shared", pressure::SET_BYTES),
        ("private", pressure::SET_BYTES),
        ("calibration", pressure::WRITE_BYTES),
        ("burst", pressure::WRITE_BYTES),
    ];
    expected.extend(Stage::ALL.into_iter().filter_map(|stage| {
        stage
            .fio()
            .map(|control| (stage.name(), control.size_bytes))
    }));
    let files = array(report, "/files")?;
    require(
        files.len() == expected.len(),
        "guest content inventory incomplete",
    )?;
    let mut names = BTreeSet::new();
    for file in files {
        let name = file["name"]
            .as_str()
            .ok_or_else(|| io::Error::other("file name absent"))?;
        let bytes = expected
            .iter()
            .find(|(expected, _)| *expected == name)
            .map(|(_, bytes)| *bytes)
            .ok_or_else(|| io::Error::other("unexpected content file"))?;
        let hash = file["blake3"]
            .as_str()
            .ok_or_else(|| io::Error::other("file digest absent"))?;
        require(
            names.insert(name)
                && number(file, "/bytes")? == bytes
                && hash.len() == 64
                && hash.bytes().all(|b| b.is_ascii_hexdigit()),
            "duplicate file, wrong length or invalid digest",
        )?;
    }
    let hash = |name: &str| {
        files.iter().find(|file| file["name"] == name).unwrap()["blake3"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    Ok((hash("shared"), hash("private")))
}

pub(super) fn verify(output: &Path) -> io::Result<Value> {
    let root = output.join("write");
    let stages = root.join("pressure");
    let mut calibration = 0.0;
    let mut observations = Vec::new();
    for stage in Stage::ALL {
        let directory = stages.join(stage.name());
        let before: Value = evidence::read_json(&directory.join("before.json"))?;
        let after: Value = evidence::read_json(&directory.join("after.json"))?;
        sample(&before)?;
        sample(&after)?;
        require(
            drained(&before)? && drained(&after)?,
            "stage boundary did not drain",
        )?;
        require(
            number(&after, "/elapsed_ns")? > number(&before, "/elapsed_ns")?,
            "stage telemetry is stale",
        )?;
        let request: Request = evidence::read_json(&directory.join("request.json"))?;
        require(request.stage == stage, "stage request differs")?;
        for image in 0..2 {
            let guest = root
                .join(format!("guest-{image}/pressure"))
                .join(stage.name());
            let ready: Value = evidence::read_json(&guest.join("ready.json"))?;
            require(
                ready["image"] == image && ready["stage"] == serde_json::to_value(stage)?,
                "guest readiness differs",
            )?;
            let issued: Request = evidence::read_json(&guest.join("request.json"))?;
            require(
                issued.stage == stage
                    && issued.rate_bytes_per_second == request.rate_bytes_per_second,
                "guest request differs from controller",
            )?;
            completed(
                &evidence::read_json(&guest.join("completed.json"))?,
                stage,
                image,
            )?;
            if let Some(control) = stage.fio() {
                let command: process::CommandResult =
                    evidence::read_json(&guest.join("command/command.json"))?;
                require(
                    command.exit_code == Some(0) && command.error.is_none(),
                    "fio process failed",
                )?;
                for flag in [
                    format!("--bs={}", control.block_bytes),
                    format!("--iodepth={}", control.depth),
                    "--direct=1".into(),
                    "--verify=crc32c".into(),
                    "--do_verify=1".into(),
                ] {
                    require(command.argv.contains(&flag), "fio invocation differs")?;
                }
                evidence::read_json::<evidence::Fio>(&guest.join("fio.json"))?.verify(
                    stage.name(),
                    control.size_bytes,
                    control.size_bytes,
                )?;
                let fio: Value = evidence::read_json(&guest.join("fio.json"))?;
                require(
                    fio["jobs"][0]["iodepth_level"].is_object()
                        && fio["jobs"][0]["write"]["clat_ns"]["bins"].is_object(),
                    "fio depth/histogram absent",
                )?;
            }
        }
        if stage == Stage::Calibration {
            calibration = drain_rate(&before, &after)?;
        }
        let timing: Value = evidence::read_json(&directory.join("timing.json"))?;
        let elapsed = number(&timing, "/guest_elapsed_ns")?;
        require(
            elapsed > 0 && number(&timing, "/through_drain_elapsed_ns")? >= elapsed,
            "stage elapsed times differ",
        )?;
        let joined = number(&after, "/host/fetches/counters/joined")?
            .checked_sub(number(&before, "/host/fetches/counters/joined")?)
            .ok_or_else(|| io::Error::other("fetch counter decreased"))?;
        if stage == Stage::Burst {
            let plan: Value = evidence::read_json(&stages.join("burst-plan.json"))?;
            require(
                plan["calibrated_bytes_per_second"].as_f64() == Some(calibration)
                    && request.rate_bytes_per_second == Some(calibration.ceil().max(1.0) as u64)
                    && plan["per_guest_offered_bytes_per_second"]
                        == request.rate_bytes_per_second.unwrap()
                    && plan["aggregate_multiplier"] == 2,
                "burst offer differs from measured calibration",
            )?;
            let achieved = 2.0 * pressure::WRITE_BYTES as f64 * 1e9 / elapsed as f64;
            require(
                achieved > calibration,
                &format!(
                    "burst achieved {achieved:.0} bytes/s versus measured drain {calibration:.0} bytes/s; coverage remains pending"
                ),
            )?;
        }
        observations.push(
            serde_json::json!({"stage":stage,"joined_fetches":joined,"guest_elapsed_ns":elapsed}),
        );
    }
    let idle: Value = evidence::read_json(&stages.join("idle.json"))?;
    sample(&idle)?;
    require(drained(&idle)?, "final D/E drain missing")?;
    let mut seed_digests = Vec::new();
    for image in 0..2 {
        let reports = ["write", "verify"].map(|phase| {
            evidence::read_json::<Value>(
                &output
                    .join(phase)
                    .join(format!("guest-{image}/pressure/pressure.json")),
            )
        });
        let [written, read] = reports;
        let written = written?;
        let read = read?;
        for (phase, report) in [("write", &written), ("verify", &read)] {
            require(
                report["schema_version"] == 1
                    && report["passed"] == true
                    && report["phase"] == phase
                    && report["image"] == image,
                "guest content oracle failed",
            )?;
            file_inventory(report)?;
        }
        seed_digests.push(file_inventory(&written)?);
        require(
            written["files"] == read["files"],
            "fresh-boot file digests differ",
        )?;
    }
    require(
        seed_digests[0].0 == seed_digests[1].0 && seed_digests[0].1 != seed_digests[1].1,
        "shared/disjoint dataset digests differ from the workload contract",
    )?;
    // Validate every recorded sample, not only selected stage endpoints.
    for line in BufReader::new(File::open(root.join("daemon/telemetry.jsonl"))?).lines() {
        sample(&serde_json::from_str::<Value>(&line?)?)?;
    }
    let memory = memory(&root.join("memory.jsonl"))?;
    Ok(
        serde_json::json!({"schema_version":1,"passed":true,"calibrated_bytes_per_second":calibration,
        "stages":observations,"memory":memory,"guest_ram_counted_as":"configured capacity, not added to resident totals",
        "pss_scope":"three-process cohort; proportional shares include outside-cohort sharing",
        "paper_gates":[]}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn write_completion_requires_the_entire_configured_file() {
        for stage in Stage::ALL {
            let Some(control) = stage.fio() else {
                continue;
            };
            let mut result = Completed {
                stage,
                image: 0,
                bytes: control.size_bytes,
                elapsed_ns: 1,
                read_latency: None,
            };
            completed(&result, stage, 0).unwrap();
            result.bytes -= control.block_bytes;
            assert!(completed(&result, stage, 0).is_err());
        }
    }
    #[test]
    fn missing_counters_and_forged_memory_never_default_to_zero() {
        assert!(number(&serde_json::json!({}), "/host/compaction/input_bytes").is_err());
        assert!(sample(&serde_json::json!({"schema_version":1,"phase":"live"})).is_err());
        assert_eq!(
            memory_bytes("Rss: 16 kB\nPss: 8 kB\n", "Pss:").unwrap(),
            8192
        );
        assert!(memory_bytes("Pss: 8 MB\n", "Pss:").is_err());
        assert!(memory_bytes("Rss: 16 kB\n", "Pss:").is_err());
    }
    #[test]
    fn incomplete_reads_and_zero_progress_cannot_close_a_stage() {
        let mut result = Completed {
            stage: Stage::SharedMiss,
            image: 0,
            bytes: 4096,
            elapsed_ns: 10,
            read_latency: None,
        };
        assert!(completed(&result, Stage::SharedMiss, 0).is_err());
        let mut latency = pressure::Latency {
            count: 1,
            total_ns: 5,
            maximum_ns: 5,
            ..Default::default()
        };
        latency.log2_microseconds[0] = 1;
        result.read_latency = Some(latency);
        completed(&result, Stage::SharedMiss, 0).unwrap();
        assert!(completed(&result, Stage::SharedMiss, 1).is_err());
        result.bytes = 0;
        assert!(completed(&result, Stage::SharedMiss, 0).is_err());
    }
}
