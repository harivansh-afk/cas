use cas_daemon::host_service::{self, Endpoint, Mode};
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Serve all existing catalog images from one governed local host")]
struct Args {
    #[arg(long)]
    root: PathBuf,
    #[arg(long, value_parser = host_service::parse_id)]
    store: [u8; 16],
    #[arg(long, value_enum)]
    mode: Mode,
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    segment_bytes: u64,
    #[arg(long, default_value_t = 1024 * 1024 * 1024)]
    staging_bytes: u64,
    /// Repeat once for every catalog image: 32hex-image-id=socket-path.
    #[arg(long = "image", required = true)]
    endpoints: Vec<Endpoint>,
    /// Existing directory on another filesystem; creates host.json and per-image reports.
    #[arg(long)]
    reports: PathBuf,
}
fn main() -> std::io::Result<()> {
    let args = Args::parse();
    host_service::serve(host_service::Config {
        root: args.root,
        store: args.store,
        mode: args.mode,
        segment_bytes: args.segment_bytes,
        staging_bytes: args.staging_bytes,
        endpoints: args.endpoints,
        reports: args.reports,
    })
}
