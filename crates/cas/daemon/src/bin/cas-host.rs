use cas_daemon::{
    host_service::{self, Endpoint, Mode},
    initialize::{self, Image},
};
use clap::{CommandFactory, FromArgMatches, Parser};
use std::path::PathBuf;

#[derive(clap::Args)]
struct Storage {
    #[arg(long)]
    root: PathBuf,
    #[arg(long, value_parser = host_service::parse_id)]
    store: [u8; 16],
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    segment_bytes: u64,
    #[arg(long, default_value_t = 1024 * 1024 * 1024)]
    staging_bytes: u64,
}

#[derive(Parser)]
#[command(about = "Serve all catalog images from one governed local host")]
struct Serve {
    #[command(flatten)]
    peer: cas_daemon::peer::Config,
    #[command(flatten)]
    storage: Storage,
    #[arg(long, value_enum)]
    mode: Mode,
    /// Image sockets. Fleet cold mode also accepts IDs awaiting provisioning/migration.
    #[arg(long = "image")]
    endpoints: Vec<Endpoint>,
    /// Existing directory on another filesystem; creates host.json and per-image reports.
    #[arg(long)]
    reports: PathBuf,
    /// Clean chunk-cache bytes; zero disables payload caching, bounded by the host cap.
    #[arg(long, default_value_t = cas_daemon::Resources::DEFAULT_CACHE_BYTES)]
    cache_bytes: usize,
    /// Sample every 500 ms; fail the run if telemetry exceeds 64 MiB or 4,096 samples.
    #[arg(long)]
    telemetry: bool,
    /// Operational sampling: rotate two files and report output failure without stopping storage.
    #[arg(long, requires = "telemetry")]
    telemetry_rotate: bool,
    /// One-shot process pause for development crash controls.
    #[arg(long, value_enum, requires = "pause_image")]
    pause_compaction: Option<cas_daemon::CompactionPoint>,
    #[arg(long, value_parser = host_service::parse_id, requires = "pause_compaction")]
    pause_image: Option<[u8; 16]>,
    #[arg(long, default_value = "1")]
    pause_after: std::num::NonZeroU64,
    #[arg(long, requires = "pause_compaction")]
    pause_wait_for_arm: bool,
    /// Seconds a cold guest has to connect and enable its queues before the attachment fails.
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..))]
    attach_timeout_secs: u64,
}

#[derive(Parser)]
#[command(
    name = "init",
    about = "Initialize an empty dedicated storage root; retain all output on error"
)]
struct Init {
    /// Fixed store chunk bytes; guest blocks remain 4096 bytes.
    #[arg(long, default_value_t = 4096)]
    chunk_bytes: usize,
    #[command(flatten)]
    storage: Storage,
    /// Repeat for each initial image: 32hex-image-id=size-in-bytes.
    #[arg(long = "image", required = true)]
    images: Vec<Image>,
}

fn command() -> clap::Command {
    Serve::command()
        .subcommand(Init::command())
        .subcommand_negates_reqs(true)
        .args_conflicts_with_subcommands(true)
}

fn main() -> std::io::Result<()> {
    let matches = command().get_matches();
    if let Some(("init", matches)) = matches.subcommand() {
        let args = Init::from_arg_matches(matches).unwrap_or_else(|error| error.exit());
        let size = cas_core::chunk::ChunkSize::from_bytes(args.chunk_bytes)?;
        let result = initialize::create_sized(
            initialize::Config {
                root: args.storage.root,
                store: cas_core::store::file::Config {
                    store: args.storage.store,
                    segment_bytes: args.storage.segment_bytes,
                },
                append: cas_core::append::Limits::default(),
                staging_bytes: args.storage.staging_bytes,
                images: args.images,
            },
            size,
        );
        match &result {
            Ok(report) => println!("{report:#}"),
            Err(error) => println!(
                "{:#}",
                serde_json::json!({
                    "schema_version": 1, "operation": "host_init", "success": false,
                    "error": error.to_string(), "output_retained": true,
                })
            ),
        }
        return result.map(|_| ());
    }
    let args = Serve::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
    host_service::serve(host_service::Config {
        peer: args.peer,
        root: args.storage.root,
        store: args.storage.store,
        mode: args.mode,
        segment_bytes: args.storage.segment_bytes,
        staging_bytes: args.storage.staging_bytes,
        endpoints: args.endpoints,
        reports: args.reports,
        cache_bytes: args.cache_bytes,
        telemetry: args.telemetry,
        telemetry_rotate: args.telemetry_rotate,
        pause: args
            .pause_compaction
            .map(|point| cas_daemon::CompactionPause {
                point,
                image: args.pause_image.expect("required with pause-compaction"),
                after: args.pause_after,
                wait_for_arm: args.pause_wait_for_arm,
            }),
        attach_timeout: std::time::Duration::from_secs(args.attach_timeout_secs),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serve_args<'a>(extra: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
        [
            "cas-host",
            "--root",
            "/fixture",
            "--store",
            "01010101010101010101010101010101",
            "--mode",
            "cold",
            "--reports",
            "/reports",
            "--image",
            "02020202020202020202020202020202=/socket",
        ]
        .into_iter()
        .chain(extra)
        .collect()
    }

    #[test]
    fn measurement_flags_keep_their_frozen_defaults_and_reject_zero() {
        let matches = command()
            .try_get_matches_from(serve_args(["--attach-timeout-secs", "240"]))
            .unwrap();
        let serve = Serve::from_arg_matches(&matches).unwrap();
        assert_eq!(serve.attach_timeout_secs, 240);
        assert!(
            command()
                .try_get_matches_from(serve_args(["--attach-timeout-secs", "0"]))
                .is_err()
        );
    }

    #[test]
    fn initialization_and_existing_serve_options_parse_without_mixing() {
        let storage = [
            "--root",
            "/fixture",
            "--store",
            "01010101010101010101010101010101",
        ];
        let image = "02020202020202020202020202020202=1048576";
        let matches = command()
            .try_get_matches_from(
                ["cas-host", "init"]
                    .into_iter()
                    .chain(storage)
                    .chain(["--image", image]),
            )
            .unwrap();
        let init = Init::from_arg_matches(matches.subcommand().unwrap().1).unwrap();
        assert_eq!(init.images[0].bytes, 1048576);
        let matches = command()
            .try_get_matches_from(["cas-host"].into_iter().chain(storage).chain([
                "--mode",
                "cold",
                "--reports",
                "/reports",
                "--image",
                "02020202020202020202020202020202=/socket",
            ]))
            .unwrap();
        let serve = Serve::from_arg_matches(&matches).unwrap();
        assert_eq!(serve.endpoints.len(), 1);
        assert_eq!(serve.attach_timeout_secs, 60);
        assert!(
            command()
                .try_get_matches_from(
                    ["cas-host", "--mode", "cold", "init"]
                        .into_iter()
                        .chain(storage)
                        .chain(["--image", image]),
                )
                .is_err()
        );
    }
}
