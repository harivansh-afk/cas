//! Local development comparison and read-only offline inspection; no guest/QEMU.
use cas_core::{
    BLOCK_SIZE, MAX_REQUEST_BYTES,
    aligned::AlignedBuffer,
    append::{
        Config as LogConfig, Limits, Log,
        format::{Builder, RequestId},
    },
    budget::{Amount, Budget},
    chunk::ChunkSize,
    manifest::file::{Identity, Manifest},
    segments::Tickets,
    store::file::{Config, Store},
};
use std::{fs, io, os::unix::fs::MetadataExt, path::Path, sync::Arc};
fn memory() -> Arc<Budget> {
    Budget::new(Amount {
        bytes: 512 * MAX_REQUEST_BYTES,
        requests: 0,
    })
}
fn compact(log: &mut Log, manifest: &mut Manifest, store: &mut Store) -> io::Result<()> {
    while let Some(selection) = log
        .select_compaction(memory(), memory())
        .map_err(io::Error::other)?
    {
        let input = selection.load_with_store(&store.reader()?)?;
        let compacted = input.write(store, manifest)?;
        log.publish_compaction(compacted)
            .map_err(io::Error::other)?;
    }
    let reclaimed = log
        .select_reclamation(None, memory())
        .map_err(io::Error::other)?
        .run()?;
    log.apply_reclamation(reclaimed).map_err(io::Error::other)?;
    Ok(())
}
fn id(sequence: u64) -> RequestId {
    RequestId {
        attachment: 1,
        serial: sequence,
        queue: 0,
        head: 0,
    }
}
fn allocated(path: &Path) -> io::Result<u64> {
    let mut total = 0;
    for file in fs::read_dir(path)? {
        let file = file?;
        let metadata = file.metadata()?;
        total += if metadata.is_dir() {
            allocated(&file.path())?
        } else {
            metadata.blocks() * 512
        };
    }
    Ok(total)
}
fn fixture(root: &Path, size: ChunkSize) -> io::Result<serde_json::Value> {
    fs::create_dir(root)?;
    let metadata = memory();
    let tickets = Tickets::open(root, Arc::clone(&metadata))?;
    let config = Config {
        store: [1; 16],
        segment_bytes: 2 * MAX_REQUEST_BYTES as u64,
    };
    let mut store = Store::create_sized(
        Arc::clone(&tickets),
        config,
        size,
        Arc::clone(&metadata),
        memory(),
    )?;
    let mut images = Vec::new();
    for image in [2u8, 3] {
        let path = root
            .join("images")
            .join(format!("{:032x}", u128::from_be_bytes([image; 16])));
        fs::create_dir_all(&path)?;
        let identity = Identity {
            store: config.store,
            image: [image; 16],
            image_bytes: MAX_REQUEST_BYTES as u64,
        };
        let mut manifest = Manifest::create_sized(&path, identity, size, Arc::clone(&metadata))?;
        let mut log = Log::create_shared(
            Arc::clone(&tickets),
            LogConfig {
                store: config.store,
                image: identity.image,
                image_bytes: identity.image_bytes,
                segment_bytes: config.segment_bytes,
            },
            Limits::default(),
            Arc::clone(&metadata),
            manifest.view()?,
        )
        .map_err(io::Error::other)?;
        let mut batch =
            Builder::new(identity.image_bytes, MAX_REQUEST_BYTES).map_err(io::Error::other)?;
        batch
            .write(id(1), 0, MAX_REQUEST_BYTES, |bytes| {
                for (block, bytes) in bytes.chunks_mut(BLOCK_SIZE).enumerate() {
                    bytes.fill(17);
                    bytes[..8].copy_from_slice(&(block as u64).to_le_bytes());
                }
                Ok(())
            })
            .map_err(io::Error::other)?;
        log.append(batch).map_err(io::Error::other)?;
        log.flush().map_err(io::Error::other)?;
        compact(&mut log, &mut manifest, &mut store)?;
        images.push((log, manifest));
    }
    // Change one guest block per 16 KiB extent in the second image.
    let (log, manifest) = &mut images[1];
    for group in (0..64).collect::<Vec<_>>().chunks(63) {
        let mut batch = Builder::new(MAX_REQUEST_BYTES as u64, group.len() * BLOCK_SIZE)
            .map_err(io::Error::other)?;
        for &extent in group {
            batch
                .write(id(extent + 2), extent * 16384, BLOCK_SIZE, |bytes| {
                    bytes.fill(29);
                    bytes[..8].copy_from_slice(&extent.to_le_bytes());
                    Ok(())
                })
                .map_err(io::Error::other)?;
        }
        log.append(batch).map_err(io::Error::other)?;
    }
    log.flush().map_err(io::Error::other)?;
    compact(log, manifest, &mut store)?;
    let mut collection = store.begin_collection()?;
    for (_, manifest) in &images {
        manifest.tree()?.walk(|extent| {
            if let Some(hash) = extent.hash {
                collection.mark(&hash)?;
            }
            Ok(())
        })?;
    }
    let mut sweep = collection.finish_marking()?;
    while sweep.clean_next()?.is_some() {}
    let collection = sweep.finish()?;
    // Independent expected guest bytes, verified after collection.
    for (index, (log, _)) in images.iter().enumerate() {
        let plan = log
            .read_plan(0, MAX_REQUEST_BYTES, log.status().published)
            .map_err(io::Error::other)?;
        let mut bytes = AlignedBuffer::new(MAX_REQUEST_BYTES);
        plan.read_with(&mut bytes, |hash, bytes| {
            store
                .plan(hash)?
                .ok_or_else(|| io::Error::other("missing fixture chunk"))?
                .load(bytes)
        })?;
        for (block, bytes) in bytes.as_slice().chunks(BLOCK_SIZE).enumerate() {
            let changed = index == 1 && block % 4 == 0;
            let number = if changed { block / 4 } else { block } as u64;
            assert_eq!(&bytes[..8], &number.to_le_bytes());
            assert!(
                bytes[8..]
                    .iter()
                    .all(|b| *b == if changed { 29 } else { 17 })
            );
        }
    }
    let guest: u64 = images
        .iter()
        .map(|(l, _)| l.status().guest_bytes_written)
        .sum();
    let wal: u64 = images.iter().map(|(l, _)| l.status().wal_bytes).sum();
    let manifest: u64 = images.iter().map(|(_, m)| m.bytes_written()).sum();
    let status = store.status();
    Ok(
        serde_json::json!({"chunk_bytes":size.bytes(), "guest_bytes_written":guest, "wal_bytes":wal,
        "manifest_bytes_written":manifest,"store":status,"collection":collection,
        "unique_payload_bytes": status.chunks * size.bytes(), "chunk_files_allocated_bytes":allocated(&root.join("chunks"))?, "root_allocated_bytes":allocated(root)?,
        "software_write_amplification": (wal + manifest + status.store_bytes_appended) as f64 / guest as f64,
        "compaction_read_amplification": status.compaction_read_bytes as f64 / guest as f64,
        "scope":"development fixture; software requested writes, not NVMe/device amplification; includes collection in store bytes"}),
    )
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 3 {
        return Err("usage: chunk-arm fixture NEW_DIRECTORY | inspect STORE_DIRECTORY".into());
    }
    let root = Path::new(&args[2]);
    let report = if args[1] == "fixture" {
        fs::create_dir(root)?;
        serde_json::json!([
            fixture(&root.join("4k"), ChunkSize::K4)?,
            fixture(&root.join("16k"), ChunkSize::K16)?
        ])
    } else if args[1] == "inspect" {
        let metadata = memory();
        let tickets = Tickets::open(root, Arc::clone(&metadata))?;
        let inspection = Store::inspect_recorded(tickets, metadata, memory())?;
        let status = inspection.status();
        serde_json::json!({
            "chunk_bytes": status.chunk_bytes,
            "chunks": status.chunks,
            "encoded_bytes": status.encoded_bytes,
            "index_memory": status.index_memory,
            "batch_table_bytes": status.batch_table_bytes,
            "clean_cache_allocated_bytes": null,
            "manifest_cache_allocated_bytes": null,
            "scope": "read-only rebuilt index; live cache allocations and traffic counters unavailable offline; use host telemetry"
        })
    } else {
        return Err("expected fixture or inspect".into());
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
