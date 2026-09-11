//! Recheck retained model evidence without executing recovery a second time.
use super::*;
use serde_json::Value;

fn verify_manifest(root: &Path, report_name: &str, report: &Value) -> io::Result<()> {
    let expected: source::Manifest = serde_json::from_value(report["artifacts"].clone())?;
    let mut actual = source::scan(root)?;
    actual.remove(Path::new(report_name));
    source::compare(&expected, &actual, "persistence evidence")
}

pub fn verify(output: &Path) -> io::Result<()> {
    let report: Value = evidence::read_json(&output.join("model.json"))?;
    if report["schema_version"] != 1
        || report["model"] != "synced-prefix-unsynced-sector-schedules-v1"
        || report["seed"] != SEED
        || report["sector_bytes"] != SECTOR_BYTES
        || report["passed"] != true
        || !report["error"].is_null()
        || report["physical_power_loss_tested"] != false
        || report["paper_gates"] != serde_json::json!([])
    {
        return Err(io::Error::other("failed or unsupported persistence model"));
    }
    verify_manifest(output, "model.json", &report)?;
    let mut count = 0;
    for (name, required, sectors) in [
        ("same-segment", 2, 128),
        ("after-rollover", 2, 128),
        ("after-sync", 9, 0),
    ] {
        let window = output.join(name);
        // All mandatory IDs are generated from this version's frozen workload,
        // rather than trusting a report's list of completed cases.
        let headers: Vec<_> = [0, 32, 80, 120]
            .into_iter()
            .flat_map(|start| start..start + BLOCK_SIZE / SECTOR_BYTES)
            .collect();
        for schedule in schedules(sectors, &headers) {
            let directory = window.join(&schedule.id);
            let retained_schedule: Value = evidence::read_json(&directory.join("schedule.json"))?;
            if retained_schedule != serde_json::to_value(&schedule)? {
                return Err(io::Error::other(
                    "persistence schedule differs from the model",
                ));
            }
            let result: Value = evidence::read_json(&directory.join("result.json"))?;
            let prefix = result["status"]["published"]
                .as_u64()
                .ok_or_else(|| io::Error::other("missing recovered prefix"))?;
            if result["passed"] != true
                || !result["error"].is_null()
                || result["required_prefix"] != required
                || prefix < required
                || ![2, 5, 7, 9].contains(&prefix)
                || result["status"]["durable"] != prefix
            {
                return Err(io::Error::other("persistence case did not pass"));
            }
            let expected = fs::read(window.join(format!("oracles/prefix-{prefix}.bin")))?;
            let observed = fs::read(directory.join("observed.bin"))?;
            if expected.len() != IMAGE_BYTES || expected != observed {
                return Err(io::Error::other("retained image differs from its oracle"));
            }
            verify_manifest(&directory, "result.json", &result)?;
            count += 1;
        }
        let controls: Value = evidence::read_json(&window.join("negative-controls/result.json"))?;
        for key in [
            "passed",
            "required_corruption_refused_before_repair",
            "wrong_image_rejected",
            "missing_prefix_rejected",
            "partial_batch_rejected",
        ] {
            if controls[key] != true {
                return Err(io::Error::other("missing persistence negative control"));
            }
        }
    }
    if report["cases"] != count {
        return Err(io::Error::other("incomplete persistence case count"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_marker_without_required_cases_is_rejected() {
        let output = tempfile::tempdir().unwrap();
        evidence::write_json(
            &output.path().join("model.json"),
            &serde_json::json!({
                "schema_version": 1,
                "model": "synced-prefix-unsynced-sector-schedules-v1",
                "seed": SEED,
                "sector_bytes": SECTOR_BYTES,
                "passed": true,
                "error": null,
                "physical_power_loss_tested": false,
                "paper_gates": [],
                "artifacts": {},
                "cases": 841
            }),
        )
        .unwrap();
        assert!(verify(output.path()).is_err());
    }
}
