//! Explicit crash-test boundaries for the serial recovery reference.
use std::fs::File;
use std::io;
use std::path::PathBuf;

use clap::ValueEnum;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Point {
    BeforeSubmit,
    AfterStorage,
    AfterStatus,
    AfterUsed,
}

#[derive(clap::Args)]
pub struct FaultArgs {
    #[arg(long, value_enum, requires_all = ["pause_after", "pause_marker"])]
    pause_at: Option<Point>,
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..), requires = "pause_at")]
    pause_after: Option<u64>,
    #[arg(long, requires = "pause_at")]
    pause_marker: Option<PathBuf>,
}

impl FaultArgs {
    pub fn validate(self, restartable: bool) -> io::Result<Option<Fault>> {
        match (self.pause_at, self.pause_after, self.pause_marker) {
            (Some(point), Some(after), Some(marker)) if restartable => {
                if marker.symlink_metadata().is_ok() {
                    return Err(io::Error::other("pause marker already exists"));
                }
                Ok(Some(Fault {
                    point,
                    after,
                    marker,
                    writes: 0,
                    fired: false,
                }))
            }
            (None, None, None) => Ok(None),
            _ => Err(io::Error::other(
                "pause injection requires restartable staging and all pause options",
            )),
        }
    }
}

pub struct Fault {
    point: Point,
    after: u64,
    marker: PathBuf,
    writes: u64,
    fired: bool,
}

impl Fault {
    pub fn next_write(&mut self) {
        self.writes += 1;
    }

    pub fn hit(&mut self, point: Point) -> io::Result<()> {
        if self.fired || self.writes != self.after || point != self.point {
            return Ok(());
        }
        self.fired = true;
        let temporary = self.marker.with_extension("tmp");
        let file = File::options()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        serde_json::to_writer(
            file,
            &serde_json::json!({
                "schema_version": 1,
                "point": self.point.to_possible_value().expect("known point").get_name(),
                "writes": self.writes,
            }),
        )?;
        std::fs::rename(temporary, &self.marker)?;
        // SAFETY: raise delivers SIGSTOP to this process; the harness owns its
        // process group and can kill it even while all its threads are stopped.
        if unsafe { libc::raise(libc::SIGSTOP) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
