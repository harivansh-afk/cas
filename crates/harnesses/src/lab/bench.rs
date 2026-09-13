use super::*;
use std::time::Instant;

#[derive(Clone, Copy, clap::ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Case {
    Write,
    Read,
    Flush,
    Sequential,
}
impl Case {
    fn name(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Read => "read",
            Self::Flush => "flush",
            Self::Sequential => "sequential",
        }
    }
}

pub(super) fn run(name: &str, repeats: u8, seconds: u8, case: Option<Case>) -> io::Result<()> {
    crate::process::install_signal_handlers()?;
    let lab_name = name.split('/').next().unwrap_or(name);
    let (directory, config) = load(lab_name)?;
    let active: Active = evidence::read_json(&directory.join("active.json"))?;
    let output = active.run.join("bench").join(timestamp()?);
    fs::create_dir_all(&output)?;
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("bench.lock"))?;
    lock.try_lock()
        .map_err(|_| io::Error::other("another benchmark is running in this lab"))?;
    let started = Instant::now();
    let mut runs = Vec::new();
    let execute = |label: &str, args: Vec<String>| -> io::Result<serde_json::Value> {
        let mut command = client::ssh(name, &args)?;
        let stdout = File::options()
            .write(true)
            .create_new(true)
            .open(output.join(format!("{label}.json")))?;
        let stderr = File::options()
            .write(true)
            .create_new(true)
            .open(output.join(format!("{label}.stderr")))?;
        evidence::write_json(&output.join(format!("{label}.command.json")), &args)?;
        for file in ["memory.json", "storage.json"] {
            let source = active.run.join(file);
            if source.exists() {
                fs::copy(source, output.join(format!("{label}.before-{file}")))?;
            }
        }
        command.stdout(stdout).stderr(stderr);
        let mut child = ManagedChild::spawn(&mut command)?;
        let status = child.wait(Duration::from_secs(100))?;
        evidence::write_json(
            &output.join(format!("{label}.exit.json")),
            &serde_json::json!({"exit":status.code()}),
        )?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "{label} failed; {}",
                output.display()
            )));
        }
        let value: serde_json::Value = evidence::read_json(&output.join(format!("{label}.json")))?;
        for file in ["memory.json", "storage.json"] {
            let source = active.run.join(file);
            if source.exists() {
                fs::copy(source, output.join(format!("{label}.after-{file}")))?;
            }
        }
        let jobs = value["jobs"]
            .as_array()
            .ok_or_else(|| io::Error::other("missing fio jobs"))?;
        if jobs.is_empty() || jobs.iter().any(|j| j["error"] != 0) {
            return Err(io::Error::other("fio reported an IO error"));
        }
        Ok(value)
    };
    let base = || {
        vec![
            "fio".into(),
            "--filename=/mnt/cas/casctl-bench".into(),
            "--size=32m".into(),
            "--direct=1".into(),
            "--ioengine=psync".into(),
            "--iodepth=1".into(),
            "--numjobs=1".into(),
            "--group_reporting=1".into(),
            "--output-format=json".into(),
            "--randrepeat=1".into(),
            "--randseed=12345".into(),
            "--refill_buffers=1".into(),
        ]
    };
    let result = (|| {
        eprintln!("Preparing 32 MiB working set in {name}…");
        let mut prepare = base();
        prepare.extend([
            "--name=prepare".into(),
            "--rw=write".into(),
            "--bs=1m".into(),
            "--end_fsync=1".into(),
        ]);
        execute("prepare", prepare)?;
        let cases = case.map_or(
            vec![Case::Write, Case::Read, Case::Flush, Case::Sequential],
            |v| vec![v],
        );
        for case in cases {
            for repeat in 1..=repeats {
                let label = format!("{}-{repeat}", case.name());
                eprintln!("{name}: {label}, {seconds}s");
                let mut args = base();
                args.extend([
                    format!("--name={label}"),
                    format!("--runtime={seconds}"),
                    "--time_based=1".into(),
                    "--end_fsync=1".into(),
                ]);
                args.extend(match case {
                    Case::Write => vec!["--rw=randwrite".into(), "--bs=4k".into()],
                    Case::Read => vec!["--rw=randread".into(), "--bs=4k".into()],
                    Case::Flush => vec![
                        "--rw=write".into(),
                        "--bs=4k".into(),
                        "--fdatasync=1".into(),
                    ],
                    Case::Sequential => vec!["--rw=read".into(), "--bs=1m".into()],
                });
                let value = execute(&label, args)?;
                let job = &value["jobs"][0];
                let direction = if matches!(case, Case::Read | Case::Sequential) {
                    "read"
                } else {
                    "write"
                };
                runs.push(serde_json::json!({"case":case,"repeat":repeat,"iops":job[direction]["iops"],"bw_bytes":job[direction]["bw_bytes"],"clat_p50_ns":job[direction]["clat_ns"]["percentile"]["50.000000"],"clat_p99_ns":job[direction]["clat_ns"]["percentile"]["99.000000"],"sync_p99_ns":job["sync"]["lat_ns"]["percentile"]["99.000000"],"source":format!("{label}.json")}));
            }
        }
        Ok(())
    })();
    let summary = serde_json::json!({"success":result.is_ok(),"error":result.as_ref().err().map(ToString::to_string),"backend":config.backend,"guest":name,"seconds":seconds,"repeats":repeats,"elapsed_seconds":started.elapsed().as_secs_f64(),"runs":runs,"build":config.build,"geometry":{"outer":"KVM, 4 GiB, XFS on a file","guest":"TCG, 512 MiB, ext4; CAS 4 queues, raw/daemon 1 queue","workload":"32 MiB direct IO, psync QD1, fixed seed; host/CAS caches not reset"},"paper_gates":[]});
    evidence::write_json(&output.join("summary.json"), &summary)?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    eprintln!("Results: {}", output.display());
    result
}
