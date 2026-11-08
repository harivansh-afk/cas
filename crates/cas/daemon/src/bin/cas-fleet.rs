//! Explicit administrative operations against the configured private bulk port.
use cas_daemon::{fleet, host_service::parse_id};
use clap::{Parser, Subcommand};
use std::{net::SocketAddr, sync::Arc};

#[derive(Parser)]
#[command(about = "Operate a static two-host CAS fleet over its bulk/control ports")]
struct Args {
    #[command(subcommand)]
    operation: Operation,
}
#[derive(Subcommand)]
enum Operation {
    /// Copy a settled manifest into a new writable image identity.
    Provision {
        #[arg(long)]
        source: SocketAddr,
        #[arg(long)]
        destination: SocketAddr,
        #[arg(long, value_parser=parse_id)]
        image: [u8; 16],
        #[arg(long, value_parser=parse_id)]
        target: [u8; 16],
    },
    /// Freeze and fence the source, transfer its manifest and WAL tail, activate destination.
    Migrate {
        #[arg(long)]
        source: SocketAddr,
        #[arg(long)]
        destination: SocketAddr,
        #[arg(long, value_parser=parse_id)]
        image: [u8; 16],
    },
    /// Reconcile durable chunks on this member with its peer's owners.
    Sync {
        #[arg(long)]
        address: SocketAddr,
    },
    /// Quiesce both hosts, exchange complete live sets, sweep, then release both.
    Sweep {
        #[arg(long)]
        node0: SocketAddr,
        #[arg(long)]
        node1: SocketAddr,
    },
    /// Read cumulative application wire bytes and current store/cache occupancy.
    Report {
        #[arg(long)]
        address: SocketAddr,
    },
}
fn main() -> std::io::Result<()> {
    let metrics = Arc::new(fleet::Metrics::default());
    let result = match Args::parse().operation {
        Operation::Provision {
            source,
            destination,
            image,
            target,
        } => serde_json::to_value(fleet::transfer(
            source,
            destination,
            image,
            target,
            fleet::Mode::Provision,
            metrics.clone(),
        )?)?,
        Operation::Migrate {
            source,
            destination,
            image,
        } => serde_json::to_value(fleet::transfer(
            source,
            destination,
            image,
            image,
            fleet::Mode::Migration,
            metrics.clone(),
        )?)?,
        Operation::Sync { address } => {
            serde_json::json!({"chunks_sent":fleet::synchronize(address, metrics.clone())?})
        }
        Operation::Sweep { node0, node1 } => {
            serde_json::to_value(fleet::collect([node0, node1], metrics.clone())?)?
        }
        Operation::Report { address } => fleet::report(address)?,
    };
    println!(
        "{:#}",
        serde_json::json!({"result":result,"controller_wire":metrics.report()})
    );
    Ok(())
}
