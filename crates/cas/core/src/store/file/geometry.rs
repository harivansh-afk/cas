//! Durable geometry exists even for an empty store, outside the ticket namespace.
use super::*;
use crate::encoding::{checksum, put32, put64, u32_at, u64_at};
const NAME: &str = "chunk-format.v3";
pub(super) fn create(
    tickets: &Tickets,
    config: Config,
    size: ChunkSize,
    metadata: Arc<Budget>,
) -> io::Result<()> {
    let mut page = AlignedBuffer::try_new_in(BLOCK_SIZE, BudgetAllocator::new(metadata))?;
    let bytes = page.as_mut_slice();
    bytes[..8].copy_from_slice(b"CASSTO03");
    put32(bytes, 8, 3);
    put32(bytes, 12, size.bytes() as u32);
    bytes[16..32].copy_from_slice(&config.store);
    put64(bytes, 32, config.segment_bytes);
    put32(bytes, 4092, checksum(bytes, 4092));
    let file = direct::open(&tickets.root().join(NAME), true)?;
    direct::write_bytes(&file, page.as_slice(), 0)?;
    direct::sync_all(&file)?;
    direct::sync_all(tickets.root_file())
}
/// Whether this root already carries a durable geometry record.
pub(super) fn exists(tickets: &Tickets) -> io::Result<bool> {
    match tickets.root().join(NAME).symlink_metadata() {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
pub(super) fn read(
    tickets: &Tickets,
    config: Config,
    metadata: Arc<Budget>,
) -> io::Result<ChunkSize> {
    let (recorded, size) = self::config(tickets, metadata)?;
    require(
        recorded.store == config.store && recorded.segment_bytes == config.segment_bytes,
        "store format config mismatch",
    )?;
    Ok(size)
}

/// Readers share the marker: every WAL open and store inspection reads it,
/// concurrently across threads and processes, and the record never changes
/// after `create`. An exclusive lock here made concurrent readers fail with
/// `WouldBlock`.
pub(super) fn config(tickets: &Tickets, metadata: Arc<Budget>) -> io::Result<(Config, ChunkSize)> {
    let file = direct::open_readonly(&tickets.root().join(NAME))?;
    require(
        file.metadata()?.len() == BLOCK_SIZE as u64,
        "store format length",
    )?;
    let mut page = AlignedBuffer::try_new_in(BLOCK_SIZE, BudgetAllocator::new(metadata))?;
    direct::read_bytes(&file, page.as_mut_slice(), 0)?;
    let bytes = page.as_slice();
    require(
        &bytes[..8] == b"CASSTO03"
            && u32_at(bytes, 8) == 3
            && bytes[16..32] != [0; 16]
            && bytes[40..4092].iter().all(|b| *b == 0)
            && u32_at(bytes, 4092) == checksum(bytes, 4092),
        "store format identity/geometry/CRC",
    )?;
    let config = Config {
        store: bytes[16..32].try_into().unwrap(),
        segment_bytes: u64_at(bytes, 32),
    };
    config.validate()?;
    Ok((config, ChunkSize::from_bytes(u32_at(bytes, 12) as usize)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_geometry_readers_do_not_exclude_each_other() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let metadata = Budget::new(crate::budget::Amount {
            bytes: 1 << 20,
            requests: 0,
        });
        let tickets = Tickets::open(directory.path(), Arc::clone(&metadata)).unwrap();
        let config = Config {
            store: [7; 16],
            segment_bytes: 64 << 20,
        };
        create(&tickets, config, ChunkSize::from_bytes(BLOCK_SIZE).unwrap(), Arc::clone(&metadata))
            .unwrap();
        // A reader holding the marker open must not block another reader:
        // the daemon reads it from several threads (WAL opens, inspection).
        let held = direct::open_readonly(&tickets.root().join(NAME)).unwrap();
        let (recorded, size) = self::config(&tickets, Arc::clone(&metadata)).unwrap();
        assert_eq!(recorded.store, config.store);
        assert_eq!(recorded.segment_bytes, config.segment_bytes);
        assert_eq!(size.bytes(), BLOCK_SIZE);
        // An exclusive writer is still excluded while readers hold it.
        assert_eq!(
            direct::open(&tickets.root().join(NAME), false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        drop(held);
    }
}
