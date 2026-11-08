//! Durable membership, remote dependency receipts and per-image writer fences.
//! Only the host's locked store owner mutates this namespace. A failed publish
//! poisons that owner; restart reads canonical checksummed records, never temps.
use super::Placement;
use allocator_api2::vec::Vec;
use cas_core::budget::{Budget, BudgetAllocator};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

const MAGIC: &[u8; 8] = b"CASFLT01";
const MAX_RECORD: usize = 4096;
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Phase {
    Claiming,
    Active,
    Fenced,
    Receiving,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Root {
    pub image: [u8; 16],
    pub generation: u64,
    pub writer: u8,
    pub phase: Phase,
    pub durable: u64,
    pub manifest: [u8; 32],
}
impl Root {
    pub fn writable(&self, node: u8) -> bool {
        self.writer == node && self.phase == Phase::Active
    }
    pub fn handoff(&self, node: u8, cut: u64, digest: [u8; 32]) -> io::Result<Self> {
        if !self.writable(node) || cut < self.durable {
            return Err(invalid());
        }
        Ok(Self {
            generation: self.generation.checked_add(1).ok_or_else(invalid)?,
            writer: 1 - node,
            phase: Phase::Fenced,
            durable: cut,
            manifest: digest,
            ..*self
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Epoch {
    pub id: u64,
    pub swept: bool,
    pub released: bool,
}

pub struct State {
    directory: PathBuf,
    pub placement: Placement,
    remote: Vec<[u8; 32], BudgetAllocator>,
    pub failed: bool,
}
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
fn name(image: [u8; 16]) -> String {
    format!("root-{:032x}", u128::from_be_bytes(image))
}
impl State {
    pub fn inspect(root: &Path, placement: Placement, budget: &Arc<Budget>) -> io::Result<Self> {
        let directory = root.join("fleet");
        let configured = load::<Placement>(&directory.join("membership"))?;
        if configured.is_some_and(|p| p != placement) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "persisted fleet membership differs",
            ));
        }
        let mut remote = Vec::new_in(BudgetAllocator::new(budget.clone()));
        let path = directory.join("remote");
        match File::open(&path) {
            Ok(mut file) => {
                let length = usize::try_from(file.metadata()?.len()).map_err(|_| invalid())?;
                if length < 40 || (length - 40) % 32 != 0 || configured.is_none() {
                    return Err(invalid());
                }
                let mut header = [0; 40];
                file.read_exact(&mut header)?;
                if &header[..8] != MAGIC {
                    return Err(invalid());
                }
                remote
                    .try_reserve_exact((length - 40) / 32)
                    .map_err(|_| io::ErrorKind::OutOfMemory)?;
                let mut checksum = blake3::Hasher::new();
                for _ in 0..(length - 40) / 32 {
                    let mut hash = [0; 32];
                    file.read_exact(&mut hash)?;
                    checksum.update(&hash);
                    remote.push(hash);
                }
                if checksum.finalize().as_bytes() != &header[8..]
                    || remote.windows(2).any(|p| p[0] >= p[1])
                {
                    return Err(invalid());
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
        Ok(Self {
            directory,
            placement,
            remote,
            failed: false,
        })
    }
    /// Run under the caller's physical output reservation after all inspection.
    pub fn initialize(&mut self) -> io::Result<()> {
        match fs::create_dir(&self.directory) {
            Ok(()) => File::open(self.directory.parent().unwrap())?.sync_all()?,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && self.directory.is_dir() => (),
            Err(e) => return Err(e),
        }
        if load::<Placement>(&self.directory.join("membership"))?.is_none() {
            let placement = self.placement;
            self.save("membership", &placement)?;
        }
        Ok(())
    }
    pub fn remote(&self, hash: &[u8; 32]) -> bool {
        self.placement.remote(hash) && self.remote.binary_search(hash).is_ok()
    }
    pub fn confirmation_bytes(&self, extra: usize) -> io::Result<u64> {
        self.remote
            .len()
            .checked_add(extra)
            .and_then(|n| n.checked_mul(32))
            .and_then(|n| n.checked_add(40))
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(invalid)
    }
    pub fn confirm(&mut self, hashes: &[[u8; 32]]) -> io::Result<()> {
        if self.failed || hashes.iter().any(|h| !self.placement.remote(h)) {
            return Err(invalid());
        }
        self.remote
            .try_reserve(hashes.len())
            .map_err(|_| io::ErrorKind::OutOfMemory)?;
        self.remote.extend_from_slice(hashes);
        self.remote.sort_unstable();
        self.remote.dedup();
        self.failed = true;
        let mut digest = blake3::Hasher::new();
        for hash in &self.remote {
            digest.update(hash);
        }
        atomic(&self.directory, "remote", |out| {
            out.write_all(MAGIC)?;
            out.write_all(digest.finalize().as_bytes())?;
            for hash in &self.remote {
                out.write_all(hash)?;
            }
            Ok(())
        })?;
        self.failed = false;
        Ok(())
    }
    pub fn root(&self, image: [u8; 16]) -> io::Result<Option<Root>> {
        let value = load::<Root>(&self.directory.join(name(image)))?;
        if value
            .as_ref()
            .is_some_and(|r| r.image != image || r.writer > 1 || r.generation == 0)
        {
            return Err(invalid());
        }
        Ok(value)
    }
    pub fn put_root(&mut self, root: &Root) -> io::Result<()> {
        if root.generation == 0 || root.writer > 1 {
            return Err(invalid());
        }
        if let Some(old) = self.root(root.image)? {
            // Retry is idempotent. A handoff never rolls a generation back, and
            // activation can only finalize the exact durable receiving record.
            let next = old.generation.checked_add(1) == Some(root.generation)
                && old.writable(self.placement.node)
                && root.writer != self.placement.node
                && root.phase == Phase::Fenced;
            let activate = old.generation == root.generation
                && old.writer == root.writer
                && old.durable == root.durable
                && old.manifest == root.manifest
                && matches!(old.phase, Phase::Receiving | Phase::Claiming)
                && root.phase == Phase::Active;
            let receiving = old.generation.checked_add(1) == Some(root.generation)
                && old.phase == Phase::Fenced
                && old.writer != self.placement.node
                && root.phase == Phase::Receiving
                && root.writer == self.placement.node;
            if old != *root && !next && !activate && !receiving {
                return Err(invalid());
            }
        }
        self.save(&name(root.image), root)
    }
    pub fn epoch(&self) -> io::Result<Option<Epoch>> {
        load(&self.directory.join("epoch"))
    }
    pub fn set_epoch(&mut self, epoch: &Epoch) -> io::Result<()> {
        self.save("epoch", epoch)
    }
    fn save<T: serde::Serialize>(&mut self, name: &str, value: &T) -> io::Result<()> {
        if self.failed {
            return Err(io::Error::other("fleet metadata failed; restart required"));
        }
        let mut bytes = [0; MAX_RECORD];
        let mut body = &mut bytes[44..];
        serde_json::to_writer(&mut body, value)?;
        let len = MAX_RECORD - 44 - body.len();
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..12].copy_from_slice(&(len as u32).to_le_bytes());
        let digest = blake3::hash(&bytes[44..44 + len]);
        bytes[12..44].copy_from_slice(digest.as_bytes());
        self.failed = true;
        atomic(&self.directory, name, |out| out.write_all(&bytes))?;
        self.failed = false;
        Ok(())
    }
}
fn load<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if file.metadata()?.len() != MAX_RECORD as u64 {
        return Err(invalid());
    }
    let mut bytes = [0; MAX_RECORD];
    file.read_exact(&mut bytes)?;
    let len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    if &bytes[..8] != MAGIC
        || len > MAX_RECORD - 44
        || bytes[44 + len..].iter().any(|b| *b != 0)
        || blake3::hash(&bytes[44..44 + len]).as_bytes() != &bytes[12..44]
    {
        return Err(invalid());
    }
    Ok(Some(serde_json::from_slice(&bytes[44..44 + len])?))
}
fn atomic(
    directory: &Path,
    name: &str,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    // Unique attempts preserve rejected output. A directory sync is part of
    // success, including the first publication; no durable ACK precedes it.
    let mut attempt = 0u64;
    let (path, mut output) = loop {
        let path = directory.join(format!("pending-{name}-{attempt}"));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => break (path, f),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                attempt = attempt.checked_add(1).ok_or_else(invalid)?
            }
            Err(e) => return Err(e),
        }
    };
    write(&mut output)?;
    output.sync_all()?;
    fs::rename(path, directory.join(name))?;
    File::open(directory)?.sync_all()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durable_fence_never_rolls_back_or_grants_both_writers() {
        let dir = tempfile::tempdir().unwrap();
        let budget = Budget::new(cas_core::budget::Amount {
            bytes: 1024 * 1024,
            requests: 0,
        });
        let mut state = State::inspect(dir.path(), Placement::new(0, 1).unwrap(), &budget).unwrap();
        state.initialize().unwrap();
        let a = Root {
            image: [7; 16],
            generation: 1,
            writer: 0,
            phase: Phase::Active,
            durable: 0,
            manifest: [0; 32],
        };
        state.put_root(&a).unwrap();
        let b = a.handoff(0, 17, [3; 32]).unwrap();
        state.put_root(&b).unwrap();
        state.put_root(&b).unwrap();
        assert!(state.put_root(&a).is_err());
        drop(state);
        let state = State::inspect(dir.path(), Placement::new(0, 1).unwrap(), &budget).unwrap();
        let restored = state.root(a.image).unwrap().unwrap();
        assert_eq!(restored, b);
        assert!(!restored.writable(0));
        assert!(!restored.writable(1));
        assert!(State::inspect(dir.path(), Placement::new(1, 1).unwrap(), &budget).is_err());
    }
    #[test]
    fn receipts_survive_restart_and_corruption_is_not_permission() {
        let dir = tempfile::tempdir().unwrap();
        let budget = Budget::new(cas_core::budget::Amount {
            bytes: 1024 * 1024,
            requests: 0,
        });
        let p = Placement::new(0, 2).unwrap();
        let mut state = State::inspect(dir.path(), p, &budget).unwrap();
        state.initialize().unwrap();
        state.confirm(&[[7; 32], [4; 32], [7; 32]]).unwrap();
        drop(state);
        let state = State::inspect(dir.path(), p, &budget).unwrap();
        assert!(state.remote(&[7; 32]));
        assert!(!state.remote(&[5; 32]));
        fs::write(dir.path().join("fleet/remote"), [0; 40]).unwrap();
        assert!(State::inspect(dir.path(), p, &budget).is_err());
    }
}
