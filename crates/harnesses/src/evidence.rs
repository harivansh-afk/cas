//! Decode the fields needed to decide a run, retaining original JSON artifacts.
use std::fs::File;
use std::io;
use std::path::Path;

use cas_core::BLOCK_SIZE;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub const IO_BYTES: u64 = 64 * 1024 * 1024;
pub const DISK_BYTES: u64 = 128 * 1024 * 1024;
pub const LIVE_BYTES: u64 = 4 * 1024 * 1024;

pub fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<T> {
    serde_json::from_reader(File::open(path)?)
        .map_err(|error| io::Error::other(format!("{}: {error}", path.display())))
}

pub fn write_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let mut file = File::options().write(true).create_new(true).open(path)?;
    write_json_to(&mut file, value)
}

pub fn write_json_to(mut output: impl io::Write, value: &impl Serialize) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut output, value)?;
    writeln!(output)
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    Raw,
    Daemon,
    Staging,
    Local,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Daemon => "daemon",
            Self::Staging => "staging",
            Self::Local => "local",
        }
    }
    pub fn storage(self) -> &'static str {
        match self {
            Self::Raw | Self::Daemon => "raw_io_uring",
            Self::Staging => "staging_sync",
            Self::Local => "local_sync",
        }
    }
}

#[derive(Deserialize)]
pub struct Build {
    pub system: String,
    #[serde(default)]
    pub interactive: bool,
    pub backend: Backend,
    pub daemon: Option<std::path::PathBuf>,
}

#[derive(Deserialize)]
pub struct GuestCompletion {
    schema_version: u32,
    service_result: String,
    exit_code: String,
    exit_status: String,
}

impl GuestCompletion {
    pub fn verify(&self) -> io::Result<()> {
        if self.schema_version != 1
            || self.service_result != "success"
            || self.exit_code != "exited"
            || self.exit_status != "0"
        {
            return Err(io::Error::other("guest service did not succeed"));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
pub struct Fio {
    jobs: Vec<FioJob>,
}
#[derive(Deserialize)]
struct FioJob {
    jobname: String,
    error: i32,
    read: FioDirection,
    write: FioDirection,
}
#[derive(Deserialize)]
struct FioDirection {
    io_bytes: u64,
}

impl Fio {
    pub fn verify(&self, name: &str, write_bytes: u64, read_bytes: u64) -> io::Result<()> {
        let [job] = self.jobs.as_slice() else {
            return Err(io::Error::other("fio did not report exactly one job"));
        };
        if job.jobname != name || job.error != 0 {
            return Err(io::Error::other("fio job failed or has an unexpected name"));
        }
        if job.write.io_bytes != write_bytes || job.read.io_bytes != read_bytes {
            return Err(io::Error::other(
                "fio did not complete the expected byte counts",
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
pub struct DaemonReport {
    schema_version: u32,
    backend: String,
    connection_ok: bool,
    flush_negotiated: bool,
    errors: u64,
    pending_at_disconnect: u64,
    queues: u64,
    write_bytes: u64,
    read_bytes: u64,
    flushes: u64,
    bounce_requests: u64,
    peak_inflight: u64,
    staging: Option<StagingReport>,
    local: Option<LocalReport>,
    #[serde(default)]
    guest_payload_copy_bytes: u64,
    #[serde(default)]
    writes: u64,
    #[serde(default)]
    restartable: bool,
    restored_used: Option<u16>,
    #[serde(default)]
    restored_pending: u16,
}
#[derive(Deserialize)]
struct StagingReport {
    image_bytes: u64,
    appended: u64,
    durable: u64,
}

#[derive(Deserialize)]
struct LocalReport {
    status: LocalStatus,
    metrics: LocalMetrics,
    requests: cas_core::budget::Usage,
    append: cas_core::budget::Usage,
    read: cas_core::budget::Usage,
    control: cas_core::budget::Usage,
    host_append: cas_core::budget::Usage,
    host_read: cas_core::budget::Usage,
}

#[derive(Deserialize)]
struct LocalStatus {
    image_bytes: u64,
    published: u64,
    durable: u64,
    failed: bool,
}

#[derive(Deserialize)]
struct LocalMetrics {
    gathered_bytes: u64,
    gather_calls: u64,
    batches_submitted: u64,
    allocation_identity_checks: u64,
    allocations_released: u64,
    encoded_bytes: u64,
    admission_retained_peak: usize,
    encoding_retained_peak: usize,
    completion_retained_peak: usize,
}

impl LocalReport {
    fn verify(&self, daemon: &DaemonReport) -> io::Result<()> {
        let metrics = &self.metrics;
        if self.status.image_bytes != DISK_BYTES
            || self.status.failed
            || self.status.published == 0
            || self.status.published != self.status.durable
            || metrics.gathered_bytes != daemon.write_bytes
            || daemon.guest_payload_copy_bytes != daemon.write_bytes
            || metrics.gather_calls != daemon.writes
            || metrics.allocation_identity_checks != metrics.batches_submitted
            || metrics.allocations_released != metrics.batches_submitted
            || self.append.admitted != metrics.batches_submitted
            || (daemon.write_bytes != 0 && (daemon.writes == 0 || metrics.batches_submitted == 0))
            || metrics.encoded_bytes
                != daemon.write_bytes + metrics.batches_submitted * BLOCK_SIZE as u64
        {
            return Err(io::Error::other(
                "local prefix, copy accounting or allocation identity failed",
            ));
        }
        for (usage, bytes, requests) in [
            (&self.requests, 0, 128),
            (&self.append, 8 * 1024 * 1024, 0),
            (&self.read, 8 * 1024 * 1024, 0),
            (&self.control, 64 * 1024, 8),
            (&self.host_append, 64 * 1024 * 1024, 0),
            (&self.host_read, 64 * 1024 * 1024, 0),
        ] {
            if usage.current != cas_core::budget::Amount::default()
                || usage.peak.bytes > bytes
                || usage.peak.requests > requests
                || usage.admitted != usage.released
            {
                return Err(io::Error::other(
                    "local resource credits leaked or exceeded their bounds",
                ));
            }
        }
        for peak in [
            metrics.admission_retained_peak,
            metrics.encoding_retained_peak,
            metrics.completion_retained_peak,
        ] {
            if peak > 8 * 1024 * 1024 || (daemon.write_bytes != 0 && peak < BLOCK_SIZE) {
                return Err(io::Error::other("invalid append lifetime measurement"));
            }
        }
        Ok(())
    }
}

impl DaemonReport {
    pub fn verify_live(&self, crash_at: &str) -> io::Result<()> {
        let expected = LIVE_BYTES / BLOCK_SIZE as u64
            + u64::from(matches!(crash_at, "after-storage" | "after-status"));
        if self.schema_version != 1
            || self.backend != "staging_sync"
            || !self.connection_ok
            || !self.flush_negotiated
            || !self.restartable
            || self.errors != 0
            || self.pending_at_disconnect != 0
            || self.queues != 1
            || self.peak_inflight != 1
            || self.restored_used.is_none_or(|used| used == 0)
            || self.restored_pending == 0
            || self.restored_pending > 128
            || self.read_bytes < LIVE_BYTES
            || self.write_bytes == 0
            || self.flushes == 0
            || self.staging.as_ref().is_none_or(|s| {
                s.image_bytes != DISK_BYTES || s.appended != expected || s.durable != expected
            })
        {
            return Err(io::Error::other(
                "live recovery did not report serial replay and durable IO",
            ));
        }
        Ok(())
    }
    pub fn verify(&self, backend: Backend, read_only: bool, interactive: bool) -> io::Result<()> {
        if self.schema_version != 1
            || self.backend != backend.storage()
            || !self.connection_ok
            || !self.flush_negotiated
            || self.errors != 0
            || self.pending_at_disconnect != 0
            || self.queues != 1
        {
            return Err(io::Error::other("daemon did not report a clean run"));
        }
        let expected = if read_only { IO_BYTES } else { 2 * IO_BYTES };
        let writes_ok = if read_only {
            self.write_bytes == 0
        } else {
            self.write_bytes >= expected
        };
        if !writes_ok
            || self.read_bytes < expected
            || self.bounce_requests == 0
            || self.peak_inflight < if read_only { 1 } else { 2 }
            || (!read_only && self.flushes < 2)
        {
            return Err(io::Error::other("daemon did not complete the expected IO"));
        }
        if backend == Backend::Staging {
            let staging = self
                .staging
                .as_ref()
                .ok_or_else(|| io::Error::other("missing staging report"))?;
            if staging.image_bytes != DISK_BYTES
                || staging.appended < expected / BLOCK_SIZE as u64
                || (!interactive && staging.appended != expected / BLOCK_SIZE as u64)
                || staging.durable != staging.appended
            {
                return Err(io::Error::other(
                    "staging did not confirm the expected durable prefix",
                ));
            }
        }
        if backend == Backend::Local {
            self.local
                .as_ref()
                .ok_or_else(|| io::Error::other("missing local report"))?
                .verify(self)?;
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FlushMarker {
    schema_version: u32,
    phase: String,
}
impl FlushMarker {
    pub fn verify(&self) -> io::Result<()> {
        if self.schema_version != 1 || self.phase != "write_flushed" {
            return Err(io::Error::other("invalid recovery FLUSH marker"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn local_evidence_rejects_extra_copies_leaks_and_failed_identity_checks() {
        let mut value = daemon(Backend::Local, false);
        let bytes = 2 * IO_BYTES;
        let writes = bytes / BLOCK_SIZE as u64;
        let batches = writes / 32;
        let usage = |admitted, peak| {
            json!({"current":{"bytes":0,"requests":0},
            "peak":{"bytes":peak,"requests":0},"admitted":admitted,"released":admitted,"rejected":0})
        };
        let allocation = 1024 * 1024 + BLOCK_SIZE;
        value["writes"] = json!(writes);
        value["guest_payload_copy_bytes"] = json!(bytes);
        value["local"] = json!({
            "status":{"image_bytes":DISK_BYTES,"published":writes,"durable":writes,"failed":false},
            "metrics":{"gathered_bytes":bytes,"gather_calls":writes,"batches_submitted":batches,
                "allocation_identity_checks":batches,"allocations_released":batches,
                "encoded_bytes":bytes + batches * BLOCK_SIZE as u64,
                "admission_retained_peak":allocation,"encoding_retained_peak":allocation,"completion_retained_peak":allocation},
            "requests":usage(writes,0),"append":usage(batches,allocation),"read":usage(writes,BLOCK_SIZE),
            "control":usage(2,BLOCK_SIZE),"host_append":usage(batches,allocation),"host_read":usage(writes,BLOCK_SIZE),
        });
        let verify = |value| {
            serde_json::from_value::<DaemonReport>(value)
                .unwrap()
                .verify(Backend::Local, false, false)
        };
        verify(value.clone()).unwrap();
        for (pointer, invalid) in [
            ("/guest_payload_copy_bytes", json!(2 * bytes)),
            ("/local/append/peak/bytes", json!(8 * 1024 * 1024 + 1)),
            ("/local/append/current/bytes", json!(1)),
            (
                "/local/metrics/allocation_identity_checks",
                json!(batches - 1),
            ),
            ("/local/status/durable", json!(0)),
        ] {
            let mut broken = value.clone();
            *broken.pointer_mut(pointer).unwrap() = invalid;
            assert!(verify(broken).is_err(), "{pointer}");
        }
    }

    fn completion() -> Value {
        json!({"schema_version":1,"service_result":"success","exit_code":"exited","exit_status":"0"})
    }
    fn fio(name: &str, write_bytes: u64, read_bytes: u64) -> Value {
        json!({"jobs":[{"jobname":name,"error":0,"write":{"io_bytes":write_bytes},"read":{"io_bytes":read_bytes}}]})
    }
    fn daemon(backend: Backend, read_only: bool) -> Value {
        let bytes = if read_only { IO_BYTES } else { 2 * IO_BYTES };
        json!({"schema_version":1,"backend":backend.storage(),"connection_ok":true,"flush_negotiated":true,
            "errors":0,"pending_at_disconnect":0,"queues":1,"write_bytes":if read_only {0} else {bytes},
            "read_bytes":bytes,"flushes":if read_only {0} else {514},"bounce_requests":65536,"peak_inflight":32,
            "staging":{"image_bytes":DISK_BYTES,"appended":bytes / BLOCK_SIZE as u64,"durable":bytes / BLOCK_SIZE as u64}})
    }
    fn valid_guest(value: Value) -> bool {
        serde_json::from_value::<GuestCompletion>(value).is_ok_and(|report| report.verify().is_ok())
    }
    fn valid_fio(value: Value, read_only: bool) -> bool {
        serde_json::from_value::<Fio>(value).is_ok_and(|report| {
            report
                .verify("test", if read_only { 0 } else { IO_BYTES }, IO_BYTES)
                .is_ok()
        })
    }
    fn valid_daemon(value: Value, backend: Backend, read_only: bool) -> bool {
        serde_json::from_value::<DaemonReport>(value)
            .is_ok_and(|report| report.verify(backend, read_only, false).is_ok())
    }

    #[test]
    fn failed_or_incomplete_guest_service_cannot_pass() {
        assert!(valid_guest(completion()));
        for (field, value) in [
            ("schema_version", json!(0)),
            ("service_result", json!("timeout")),
            ("exit_code", json!("killed")),
            ("exit_status", json!("1")),
        ] {
            let mut report = completion();
            report[field] = value;
            assert!(!valid_guest(report), "{field}");
        }
    }

    #[test]
    fn fio_requires_one_complete_verified_job() {
        let base = fio("test", IO_BYTES, IO_BYTES);
        assert!(valid_fio(base.clone(), false));
        for jobs in [json!([]), json!(null), json!([null]), json!([{}, {}])] {
            assert!(!valid_fio(json!({"jobs":jobs}), false));
        }
        for (field, value) in [("error", json!(84)), ("jobname", json!("wrong"))] {
            let mut report = base.clone();
            report["jobs"][0][field] = value;
            assert!(!valid_fio(report, false));
        }
        for direction in ["read", "write"] {
            let mut report = base.clone();
            report["jobs"][0][direction]["io_bytes"] = json!(IO_BYTES - BLOCK_SIZE as u64);
            assert!(!valid_fio(report, false));
        }
    }

    #[test]
    fn recovery_cannot_pass_after_rewriting_the_data() {
        assert!(valid_fio(fio("test", 0, IO_BYTES), true));
        assert!(!valid_fio(fio("test", IO_BYTES, IO_BYTES), true));
        assert!(valid_daemon(
            daemon(Backend::Staging, true),
            Backend::Staging,
            true
        ));
        let mut report = daemon(Backend::Staging, true);
        report["write_bytes"] = json!(BLOCK_SIZE);
        assert!(!valid_daemon(report, Backend::Staging, true));
    }

    #[test]
    fn daemon_errors_missing_features_and_incomplete_io_cannot_pass() {
        for backend in [Backend::Daemon, Backend::Staging] {
            let base = daemon(backend, false);
            assert!(valid_daemon(base.clone(), backend, false));
            for (field, value) in [
                ("connection_ok", json!(false)),
                ("errors", json!(1)),
                ("pending_at_disconnect", json!(1)),
                ("flush_negotiated", json!(false)),
                ("flush_negotiated", json!(null)),
                ("write_bytes", json!(IO_BYTES)),
                ("read_bytes", json!(0)),
                ("flushes", json!(0)),
                ("peak_inflight", json!(1)),
                ("backend", json!("raw")),
                ("bounce_requests", json!(null)),
            ] {
                let mut report = base.clone();
                report[field] = value;
                assert!(!valid_daemon(report, backend, false), "{backend:?} {field}");
            }
        }
    }

    #[test]
    fn staging_requires_the_exact_replayed_prefix() {
        let base = daemon(Backend::Staging, false);
        for value in [
            json!(null),
            json!({}),
            json!({"image_bytes":DISK_BYTES,"appended":32768,"durable":0}),
            json!({"image_bytes":DISK_BYTES,"appended":32769,"durable":32768}),
            json!({"image_bytes":4096,"appended":32768,"durable":32768}),
        ] {
            let mut report = base.clone();
            report["staging"] = value;
            assert!(!valid_daemon(report, Backend::Staging, false));
        }
    }

    #[test]
    fn interactive_io_may_extend_but_must_flush_the_staging_prefix() {
        let mut value = daemon(Backend::Staging, false);
        value["staging"]["appended"] = json!(33792);
        value["staging"]["durable"] = json!(33792);
        let report: DaemonReport = serde_json::from_value(value.clone()).unwrap();
        assert!(report.verify(Backend::Staging, false, true).is_ok());
        assert!(report.verify(Backend::Staging, false, false).is_err());
        value["staging"]["durable"] = json!(32768);
        let report: DaemonReport = serde_json::from_value(value).unwrap();
        assert!(report.verify(Backend::Staging, false, true).is_err());
    }

    #[test]
    fn wrong_json_types_and_missing_counts_are_rejected() {
        for value in [
            json!(true),
            json!("134217728"),
            json!(-1),
            json!(134217728.0),
            json!(null),
        ] {
            let mut report = daemon(Backend::Daemon, false);
            report["read_bytes"] = value;
            assert!(!valid_daemon(report, Backend::Daemon, false));
        }
        let mut report = daemon(Backend::Daemon, false);
        report.as_object_mut().unwrap().remove("errors");
        assert!(!valid_daemon(report, Backend::Daemon, false));
    }

    #[test]
    fn live_recovery_requires_serial_replay_and_the_expected_durable_prefix() {
        for point in [
            "before-submit",
            "after-storage",
            "after-status",
            "after-used",
        ] {
            let mut base = daemon(Backend::Staging, false);
            base["restartable"] = json!(true);
            base["restored_used"] = json!(31);
            base["restored_pending"] = json!(32);
            base["peak_inflight"] = json!(1);
            let blocks = LIVE_BYTES / BLOCK_SIZE as u64
                + u64::from(matches!(point, "after-storage" | "after-status"));
            base["staging"]["appended"] = json!(blocks);
            base["staging"]["durable"] = json!(blocks);
            let valid = |value| {
                serde_json::from_value::<DaemonReport>(value)
                    .is_ok_and(|report| report.verify_live(point).is_ok())
            };
            assert!(valid(base.clone()));
            for (field, value) in [
                ("restartable", json!(false)),
                ("restored_used", json!(null)),
                ("restored_pending", json!(0)),
                ("restored_pending", json!(129)),
                ("peak_inflight", json!(2)),
                ("errors", json!(1)),
                ("pending_at_disconnect", json!(1)),
                ("read_bytes", json!(LIVE_BYTES - 1)),
            ] {
                let mut report = base.clone();
                report[field] = value;
                assert!(!valid(report), "{point}: {field}");
            }
            for field in ["appended", "durable"] {
                let mut report = base.clone();
                report["staging"][field] = json!(blocks - 1);
                assert!(!valid(report));
            }
        }
    }

    #[test]
    fn recovery_marker_requires_the_flushed_phase() {
        for value in [
            json!({"schema_version":1,"phase":"writing"}),
            json!({"schema_version":0,"phase":"write_flushed"}),
            json!({"schema_version":1,"phase":"write_flushed","extra":true}),
        ] {
            assert!(
                !serde_json::from_value::<FlushMarker>(value)
                    .is_ok_and(|marker| marker.verify().is_ok())
            );
        }
        serde_json::from_value::<FlushMarker>(json!({"schema_version":1,"phase":"write_flushed"}))
            .unwrap()
            .verify()
            .unwrap();
    }
}
