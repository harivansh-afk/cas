//! Two full virtio driver resets with the same guest and backend process.
use super::*;
use crate::evidence::LIVE_BYTES;

pub(super) fn verify_guest(results: &Path, evidence: &mut PhaseEvidence) -> io::Result<()> {
    read_json::<GuestCompletion>(&results.join("completion.json"))?.verify()?;
    let before = fs::read_to_string(results.join("boot-before.txt"))?;
    let after = fs::read_to_string(results.join("boot-after.txt"))?;
    if before.trim().is_empty() || before != after {
        return Err(io::Error::other("driver reset changed the guest boot"));
    }
    for generation in 1..=3 {
        read_json::<Fio>(&results.join(format!("reset-write-{generation}.json")))?.verify_jobs(
            "live-recovery",
            LIVE_BYTES,
            LIVE_BYTES,
            4,
        )?;
        if generation < 3 {
            read_json::<Fio>(&results.join(format!("reset-read-{generation}.json")))?.verify_jobs(
                "live-recovery",
                0,
                LIVE_BYTES,
                4,
            )?;
        }
    }
    evidence.written_bytes = Some(3 * LIVE_BYTES);
    evidence.verified_bytes = Some(5 * LIVE_BYTES);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reset_readback_requires_all_generations_and_the_same_guest() {
        let directory = tempfile::tempdir().unwrap();
        let results = directory.path();
        let write =
            |name: &str, value: Value| evidence::write_json(&results.join(name), &value).unwrap();
        write(
            "completion.json",
            json!({"schema_version":1,"service_result":"success",
            "exit_code":"exited","exit_status":"0"}),
        );
        fs::write(results.join("boot-before.txt"), "same-guest\n").unwrap();
        fs::write(results.join("boot-after.txt"), "same-guest\n").unwrap();
        let fio = |writes| {
            json!({"jobs": (0..4).map(|index| json!({
            "jobname":format!("live-recovery-{index}"), "error":0,
            "write":{"io_bytes":writes}, "read":{"io_bytes":LIVE_BYTES / 4}
        })).collect::<Vec<_>>()})
        };
        for generation in 1..=3 {
            write(
                &format!("reset-write-{generation}.json"),
                fio(LIVE_BYTES / 4),
            );
            if generation < 3 {
                write(&format!("reset-read-{generation}.json"), fio(0));
            }
        }
        let verify = || verify_guest(results, &mut PhaseEvidence::default());
        verify().unwrap();
        fs::write(results.join("boot-after.txt"), "new-guest\n").unwrap();
        assert!(verify().is_err());
        fs::write(results.join("boot-after.txt"), "same-guest\n").unwrap();
        fs::remove_file(results.join("reset-read-2.json")).unwrap();
        assert!(verify().is_err());
        let mut failed = fio(0);
        failed["jobs"][3]["error"] = json!(84);
        write("reset-read-2.json", failed);
        assert!(verify().is_err());
    }
}
