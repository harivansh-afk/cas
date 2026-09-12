//! Coordinate both ordinary guests and retain counters at workload boundaries.
use super::*;
use crate::pressure::{self, COMMAND_TIMEOUT, Completed, Request, Stage};
use serde_json::Value;
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};

mod checks;

struct Samples {
    telemetry: File,
    position: u64,
    latest: Option<Value>,
    memory: File,
    identities: Vec<(u32, PathBuf, String)>,
    next: Instant,
    started: Instant,
    count: usize,
}
impl Samples {
    fn new(
        output: &Path,
        host: &ManagedChild,
        guests: &[(ManagedChild, PathBuf, bool)],
    ) -> io::Result<Self> {
        let mut pids = vec![host.pid()];
        for (_, directory, _) in guests {
            let qemu: Value = evidence::read_json(&directory.join("qemu.json"))?;
            pids.push(
                checks::number(&qemu, "/pid")?
                    .try_into()
                    .map_err(io::Error::other)?,
            );
        }
        let identities = pids
            .into_iter()
            .map(|pid| {
                let root = PathBuf::from(format!("/proc/{pid}"));
                Ok((pid, fs::read_link(root.join("exe"))?, start_time(&root)?))
            })
            .collect::<io::Result<_>>()?;
        Ok(Self {
            telemetry: File::open(output.join("daemon/telemetry.jsonl"))?,
            position: 0,
            latest: None,
            memory: File::options()
                .write(true)
                .create_new(true)
                .open(output.join("memory.jsonl"))?,
            identities,
            next: Instant::now(),
            started: Instant::now(),
            count: 0,
        })
    }
    fn refresh(&mut self) -> io::Result<()> {
        self.telemetry.seek(SeekFrom::Start(self.position))?;
        let mut reader = BufReader::new(&self.telemetry);
        let mut line = String::new();
        loop {
            line.clear();
            let bytes = reader.read_line(&mut line)?;
            if bytes == 0 || !line.ends_with('\n') {
                break;
            }
            if bytes > 1024 * 1024 {
                return Err(io::Error::other("telemetry record exceeds cap"));
            }
            let value: Value = serde_json::from_str(&line)?;
            checks::sample(&value)?;
            self.latest = Some(value);
            self.position += bytes as u64;
        }
        Ok(())
    }
    fn capture(&mut self) -> io::Result<()> {
        if Instant::now() < self.next {
            return Ok(());
        }
        if self.count >= 4096 {
            return Err(io::Error::other("memory sample cap exceeded"));
        }
        let mut processes = Vec::new();
        let mut pss = 0;
        for (pid, executable, identity) in &self.identities {
            let root = PathBuf::from(format!("/proc/{pid}"));
            let rollup = fs::read_to_string(root.join("smaps_rollup"))?;
            if fs::read_link(root.join("exe"))? != *executable || start_time(&root)? != *identity {
                return Err(io::Error::other("memory sample process identity changed"));
            }
            let rss_bytes = checks::memory_bytes(&rollup, "Rss:")?;
            let pss_bytes = checks::memory_bytes(&rollup, "Pss:")?;
            pss += pss_bytes;
            processes.push(
                serde_json::json!({"pid":pid,"executable":executable,"start_ticks":identity,
                "rss_bytes":rss_bytes,"pss_bytes":pss_bytes,"smaps_rollup":rollup}),
            );
        }
        serde_json::to_writer(
            &self.memory,
            &serde_json::json!({"elapsed_ns":self.started.elapsed().as_nanos(),
            "processes":processes,"cohort_pss_bytes":pss,"guest_ram_capacity_bytes_each":512*1024*1024}),
        )?;
        self.memory.write_all(b"\n")?;
        self.memory.flush()?;
        self.count += 1;
        self.next = Instant::now() + Duration::from_millis(500);
        Ok(())
    }
    fn tick(
        &mut self,
        host: &mut ManagedChild,
        guests: &mut [(ManagedChild, PathBuf, bool)],
        deadline: Instant,
    ) -> io::Result<()> {
        process::check_interrupt()?;
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        if host.poll()?.is_some() {
            return Err(io::Error::other("host exited during pressure"));
        }
        for (guest, _, _) in guests {
            if guest.poll()?.is_some() {
                return Err(io::Error::other("guest exited before pressure release"));
            }
        }
        self.refresh()?;
        self.capture()?;
        thread::sleep(process::POLL);
        Ok(())
    }
    fn snapshot(&self) -> io::Result<Value> {
        self.latest
            .clone()
            .ok_or_else(|| io::Error::other("no live host telemetry"))
    }
}
fn start_time(root: &Path) -> io::Result<String> {
    let stat = fs::read_to_string(root.join("stat"))?;
    stat.rsplit_once(") ")
        .and_then(|(_, rest)| rest.split_whitespace().nth(19))
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other("process start time absent"))
}
fn drain(
    samples: &mut Samples,
    host: &mut ManagedChild,
    guests: &mut [(ManagedChild, PathBuf, bool)],
    outer: Instant,
) -> io::Result<Value> {
    let deadline = outer.min(Instant::now() + COMMAND_TIMEOUT);
    let previous = samples
        .latest
        .as_ref()
        .and_then(|v| v["elapsed_ns"].as_u64())
        .unwrap_or(0);
    loop {
        samples.tick(host, guests, deadline)?;
        let latest = samples.snapshot()?;
        if checks::number(&latest, "/elapsed_ns")? > previous && checks::drained(&latest)? {
            return Ok(latest);
        }
    }
}

pub(super) fn coordinate(
    output: &Path,
    host: &mut ManagedChild,
    guests: &mut [(ManagedChild, PathBuf, bool)],
    deadline: Instant,
) -> io::Result<()> {
    let mut samples = Samples::new(output, host, guests)?;
    let stages = output.join("pressure");
    fs::create_dir(&stages)?;
    let mut rate = None;
    for stage in Stage::ALL {
        let until = deadline.min(Instant::now() + COMMAND_TIMEOUT);
        while !guests.iter().all(|(_, root, _)| {
            root.join("pressure")
                .join(stage.name())
                .join("ready.json")
                .exists()
        }) {
            samples.tick(host, guests, until)?;
        }
        // All seed writes and previous stages are compacted before measuring the next delta.
        let before = drain(&mut samples, host, guests, deadline)?;
        let directory = stages.join(stage.name());
        fs::create_dir(&directory)?;
        evidence::write_json(&directory.join("before.json"), &before)?;
        let request = Request {
            stage,
            rate_bytes_per_second: if stage == Stage::Burst { rate } else { None },
        };
        evidence::write_json(&directory.join("request.json"), &request)?;
        let started = Instant::now();
        for (_, root, _) in guests.iter() {
            pressure::publish(
                &root
                    .join("pressure")
                    .join(stage.name())
                    .join("request.json"),
                &request,
            )?;
        }
        while !guests.iter().all(|(_, root, _)| {
            root.join("pressure")
                .join(stage.name())
                .join("completed.json")
                .exists()
        }) {
            samples.tick(host, guests, until)?;
        }
        let guest_elapsed_ns = started.elapsed().as_nanos() as u64;
        for (image, (_, root, _)) in guests.iter().enumerate() {
            checks::completed(
                &evidence::read_json::<Completed>(
                    &root
                        .join("pressure")
                        .join(stage.name())
                        .join("completed.json"),
                )?,
                stage,
                image as u8,
            )?;
        }
        let after = drain(&mut samples, host, guests, deadline)?;
        evidence::write_json(&directory.join("after.json"), &after)?;
        evidence::write_json(
            &directory.join("timing.json"),
            &serde_json::json!({"guest_elapsed_ns":guest_elapsed_ns,
            "through_drain_elapsed_ns":started.elapsed().as_nanos()}),
        )?;
        if stage == Stage::Calibration {
            let observed = checks::drain_rate(&before, &after)?;
            // Two guests each offer the measured aggregate rate: total offer = 2x.
            rate = Some(observed.ceil().max(1.0) as u64);
            evidence::write_json(
                &stages.join("burst-plan.json"),
                &serde_json::json!({
                "calibrated_bytes_per_second":observed,"per_guest_offered_bytes_per_second":rate,
                "aggregate_multiplier":2,"bytes_per_guest":pressure::WRITE_BYTES}),
            )?;
        }
    }
    let final_sample = drain(&mut samples, host, guests, deadline)?;
    evidence::write_json(&stages.join("idle.json"), &final_sample)?;
    for (_, root, _) in guests {
        pressure::publish(
            &root.join("pressure/finish.json"),
            &serde_json::json!({"idle_d_equals_e":true}),
        )?;
    }
    Ok(())
}
pub(super) fn record(output: &Path) -> io::Result<()> {
    evidence::write_json(&output.join("pressure.json"), &checks::verify(output)?)
}
pub(super) fn verify(output: &Path) -> io::Result<()> {
    let actual: Value = evidence::read_json(&output.join("pressure.json"))?;
    if actual != checks::verify(output)? {
        return Err(io::Error::other(
            "pressure summary differs from retained observations",
        ));
    }
    Ok(())
}
