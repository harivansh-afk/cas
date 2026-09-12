//! Configure complete catalog membership and supervise its socket owners.
use crate::{
    Host, Resources,
    backend::Backend,
    deadline::{Deadline, RECOVERY_TIMEOUT},
    recovery::{self, Inspection, Prefixes, RetainedHost},
    service::{Control, Service},
};
use allocator_api2::vec::Vec as BudgetVec;
mod telemetry;
use cas_core::budget::{Budget, BudgetAllocator};
use std::{
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use telemetry::Telemetry;

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Mode {
    Cold,
    Retained,
}

#[derive(Clone)]
pub struct Endpoint {
    pub image: [u8; 16],
    pub socket: PathBuf,
}
impl std::str::FromStr for Endpoint {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (id, socket) = value
            .split_once('=')
            .ok_or("expected image-id=socket-path")?;
        if socket.is_empty() {
            return Err("empty socket path".into());
        }
        Ok(Self {
            image: parse_id(id)?,
            socket: socket.into(),
        })
    }
}
pub fn parse_id(value: &str) -> Result<[u8; 16], String> {
    if value.len() != 32 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("ID must contain exactly 32 hexadecimal digits".into());
    }
    let value = u128::from_str_radix(value, 16).map_err(|error| error.to_string())?;
    if value == 0 {
        return Err("ID cannot be zero".into());
    }
    Ok(value.to_be_bytes())
}

pub struct Config {
    pub root: PathBuf,
    pub store: [u8; 16],
    pub segment_bytes: u64,
    pub staging_bytes: u64,
    pub mode: Mode,
    pub endpoints: Vec<Endpoint>,
    pub reports: PathBuf,
    pub cache_bytes: usize,
    pub telemetry: bool,
    pub pause: Option<crate::CompactionPause>,
}

struct Output {
    image: [u8; 16],
    socket: PathBuf,
    report: File,
}
enum Runtime {
    Cold(Host),
    Retained(RetainedHost),
}
impl Runtime {
    fn fail(&self, reason: &str) {
        match self {
            Self::Cold(host) => host.fail_all(reason),
            Self::Retained(host) => host.fail_all(reason),
        }
    }
    fn attach(&mut self, image: [u8; 16], deadline: Deadline) -> io::Result<Backend> {
        match self {
            Self::Cold(host) => {
                host.attach_cold(image, deadline.remaining()?, crate::fault::Fault::default())
            }
            Self::Retained(host) => host.attach(image, crate::fault::Fault::default()),
        }
    }
    fn host(&mut self) -> io::Result<Option<&mut Host>> {
        match self {
            Self::Cold(host) => Ok(Some(host)),
            Self::Retained(host) => host.host(),
        }
    }
}

fn table<T>(count: usize, budget: &Arc<Budget>) -> io::Result<BudgetVec<T, BudgetAllocator>> {
    let mut table = BudgetVec::new_in(BudgetAllocator::new(Arc::clone(budget)));
    table
        .try_reserve_exact(count)
        .map_err(|_| io::ErrorKind::OutOfMemory)?;
    Ok(table)
}
fn create_report(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

fn outputs(
    config: &mut Config,
    resources: &Resources,
) -> io::Result<(BudgetVec<Output, BudgetAllocator>, File)> {
    config.endpoints.sort_by_key(|endpoint| endpoint.image);
    if config.endpoints.is_empty()
        || config.endpoints.iter().enumerate().any(|(i, endpoint)| {
            endpoint.image == [0; 16]
                || config.endpoints[..i]
                    .iter()
                    .any(|old| old.image == endpoint.image || old.socket == endpoint.socket)
        })
    {
        return Err(io::Error::other(
            "endpoints must name distinct nonzero images and sockets",
        ));
    }
    let storage_device = fs::metadata(&config.root)?.dev();
    let outside = |path: &Path| -> io::Result<()> {
        if fs::metadata(path)?.dev() == storage_device {
            return Err(io::Error::other(
                "socket/report directory must lie outside the storage filesystem",
            ));
        }
        Ok(())
    };
    outside(&config.reports)?;
    for endpoint in &config.endpoints {
        outside(
            endpoint
                .socket
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?;
        match endpoint.socket.symlink_metadata() {
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
            Ok(_) => return Err(io::Error::other("socket path already exists")),
        }
    }
    let mut outputs = table(config.endpoints.len(), &resources.metadata)?;
    let host_report = create_report(&config.reports.join("host.json"))?;
    for endpoint in &config.endpoints {
        let id = u128::from_be_bytes(endpoint.image);
        outputs.push(Output {
            image: endpoint.image,
            socket: endpoint.socket.clone(),
            report: create_report(&config.reports.join(format!("{id:032x}.json")))?,
        });
    }
    Ok((outputs, host_report))
}

pub fn serve(mut config: Config) -> io::Result<()> {
    if config.cache_bytes > Resources::DEFAULT_CACHE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "clean cache exceeds host cap",
        ));
    }
    let mut resources = Resources::default();
    resources.cache_bytes = config.cache_bytes;
    let (outputs, report) = outputs(&mut config, &resources)?;
    if let Some(pause) = config.pause.take() {
        if !config
            .endpoints
            .iter()
            .any(|endpoint| endpoint.image == pause.image)
        {
            return Err(io::Error::other(
                "compaction pause image is outside configured membership",
            ));
        }
        resources.pause_compaction(pause, config.reports.join("compaction-pause.json"))?;
    }
    let resources = Arc::new(resources);
    let telemetry = config
        .telemetry
        .then(|| Telemetry::new(&config.reports.join("telemetry.jsonl"), &resources.metadata))
        .transpose()?;
    let result = run(config, outputs, &resources, telemetry);
    let value = match &result {
        Ok(outcome) => serde_json::json!({
            "services_ok": outcome.services.is_ok(),
            "service_error": outcome.services.as_ref().err().map(ToString::to_string),
            "shutdown_error": outcome.shutdown.as_ref().err().map(ToString::to_string),
            "host": outcome.shutdown.as_ref().ok(), "metadata": resources.metadata.usage(),
        }),
        Err(error) => serde_json::json!({"services_ok": false, "startup_error": error.to_string(),
            "metadata": resources.metadata.usage()}),
    };
    serde_json::to_writer_pretty(report, &value)?;
    let outcome = result?;
    outcome.services?;
    outcome.shutdown.map(|_| ())
}

struct Outcome {
    services: io::Result<()>,
    shutdown: io::Result<serde_json::Value>,
}
fn run(
    config: Config,
    outputs: BudgetVec<Output, BudgetAllocator>,
    resources: &Arc<Resources>,
    telemetry: Option<Telemetry>,
) -> io::Result<Outcome> {
    drop(config.endpoints);
    drop(config.reports);
    let deadline = Deadline::after(RECOVERY_TIMEOUT);
    let worker_resources = Arc::clone(resources);
    let mut ids = table(outputs.len(), &resources.metadata)?;
    ids.extend(outputs.iter().map(|output| output.image));
    let staging_bytes = config.staging_bytes;
    let mut runtime = deadline.run(move || {
        let inspected = Inspection::scan(
            &config.root,
            recovery::Config {
                store: cas_core::store::file::Config {
                    store: config.store,
                    segment_bytes: config.segment_bytes,
                },
                append: cas_core::append::Limits::default(),
            },
            worker_resources,
        )?;
        if inspected.images().map(|(id, _)| id).ne(ids) {
            return Err(io::Error::other(
                "configured endpoints differ from the complete catalog image set",
            ));
        }
        let limits = cas_core::space::Limits::new(
            inspected.observation()?.capacity(),
            config.segment_bytes,
            cas_core::manifest::tree::MAX_TRANSACTION_BYTES as u64,
        )?;
        match config.mode {
            Mode::Cold => Ok(Runtime::Cold(
                inspected
                    .require(Prefixes::Cold)?
                    .recover_cold(limits)?
                    .into_host(staging_bytes)?,
            )),
            Mode::Retained => Ok(Runtime::Retained(RetainedHost::start(
                inspected,
                limits,
                staging_bytes,
                deadline,
            )?)),
        }
    })?;
    let mut services = table(outputs.len(), &resources.metadata)?;
    let mut controls = table(outputs.len(), &resources.metadata)?;
    for output in outputs {
        let service = Service::new(runtime.attach(output.image, deadline)?, output.report)?;
        controls.push((output.image, service.control()?));
        services.push((output.socket, service));
    }
    let result = run_services(
        services,
        &controls,
        &mut runtime,
        &resources.metadata,
        telemetry,
    );
    // Controls also retain backends; drop them before joining the shared owner.
    drop(controls);
    let until = Instant::now() + crate::local::IO_DEADLINE;
    let shutdown = loop {
        match runtime.host() {
            Ok(Some(host)) => match host.shutdown() {
                Ok(()) => break Ok(host.report()),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => (),
                Err(error) => break Err(error),
            },
            Ok(None) => (),
            Err(error) => break Err(error),
        }
        if Instant::now() >= until {
            break Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "host shutdown deadline expired",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    };
    Ok(Outcome {
        services: result,
        shutdown,
    })
}

fn run_services(
    services: BudgetVec<(PathBuf, Service), BudgetAllocator>,
    controls: &[([u8; 16], Control)],
    runtime: &mut Runtime,
    metadata: &Arc<Budget>,
    mut telemetry: Option<Telemetry>,
) -> io::Result<()> {
    let mut workers: BudgetVec<Option<JoinHandle<io::Result<()>>>, _> =
        table(services.len(), metadata)?;
    let mut failure = None;
    for (socket, service) in services {
        match thread::Builder::new()
            .name("cas-image-socket".into())
            .spawn(move || service.serve(&socket))
        {
            Ok(worker) => workers.push(Some(worker)),
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    let mut canceled = false;
    loop {
        if let Some(sampler) = &mut telemetry
            && let Err(error) = sampler.sample(runtime, controls)
        {
            failure.get_or_insert(error);
            telemetry = None;
        }
        for slot in &mut workers {
            if slot.as_ref().is_some_and(|worker| worker.is_finished()) {
                let result = slot
                    .take()
                    .unwrap()
                    .join()
                    .unwrap_or_else(|_| Err(io::Error::other("socket service panicked")));
                if let Err(error) = result {
                    failure.get_or_insert(error);
                }
            }
        }
        if let Some(error) = &failure
            && !canceled
        {
            let reason = error.to_string();
            runtime.fail(&reason);
            for (_, control) in controls {
                let _ = control.cancel(&reason);
            }
            canceled = true;
        }
        if !workers.iter().any(Option::is_some) {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    failure.map_or(Ok(()), Err)
}
