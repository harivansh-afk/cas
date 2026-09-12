use super::*;
use std::os::unix::fs::MetadataExt;
use std::time::Duration;

#[test]
#[ignore = "requires the packaged dedicated XFS fixture and CAS_SPACE_REPORT"]
fn dedicated_filesystem_observes_copying_gc_under_one_destination_promise() {
    use crate::space::{Governor, Limits, Observation};
    let report = std::path::PathBuf::from(
        std::env::var_os("CAS_SPACE_REPORT").expect("run the packaged XFS fixture"),
    )
    .with_file_name("chunk-gc-observations.json");
    let root = tempfile::tempdir().unwrap();
    let tickets = Tickets::open(root.path(), metadata()).unwrap();
    let initial = Observation::inspect(&tickets).unwrap();
    let mib = 1024 * 1024;
    let config = Config {
        segment_bytes: 4 * mib,
        ..CONFIG
    };
    let account = Governor::open(
        Arc::clone(&tickets),
        Limits::new(initial.capacity(), 4 * mib, 8 * mib).unwrap(),
    )
    .unwrap();
    let mut store = account
        .foreground(16 * mib)
        .unwrap()
        .run(|| Store::create(Arc::clone(&tickets), config, metadata(), io_memory()))
        .unwrap();
    let all = (1u64..=2520)
        .map(|number| {
            let mut block = [9; BLOCK_SIZE];
            block[..8].copy_from_slice(&number.to_le_bytes());
            block
        })
        .collect::<std::vec::Vec<_>>();
    for batch in all.chunks(MAX_CHUNKS) {
        account
            .foreground(20 * mib)
            .unwrap()
            .run(|| insert(&mut store, batch))
            .unwrap();
    }
    assert_eq!(store.status().segments, 3);
    let allocated = account.refresh().unwrap();
    let input_inode_bytes: u64 = store
        .shared
        .lock()
        .segments
        .iter()
        .map(|s| s.file.metadata().unwrap().blocks() * 512)
        .sum();
    // Earlier fixture cases can still have delayed frees. Input preallocation
    // is a file-local precondition; the GC result below uses actual FS samples.
    fs::write(
        report.with_file_name("chunk-gc-input.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "initial": initial, "allocated": allocated, "input_inode_bytes": input_inode_bytes,
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(input_inode_bytes >= 12 * mib);
    let mut collection = store.begin_collection().unwrap();
    let live = all
        .iter()
        .step_by(32)
        .copied()
        .collect::<std::vec::Vec<_>>();
    for block in &live {
        collection.mark(&hash(block)).unwrap();
    }
    let mut sweep = collection.finish_marking().unwrap();
    let mut steps = std::vec::Vec::new();
    while let Some(victim) = sweep.next_victim() {
        assert_eq!(victim.destination_bytes, config.segment_bytes);
        let promise = victim.destination_bytes + 16 * mib;
        let result = account
            .background(promise)
            .unwrap()
            .run(|| sweep.clean_next())
            .unwrap()
            .unwrap();
        steps.push(serde_json::json!({ "victim": victim, "promise": promise,
            "result": result, "observation": account.refresh().unwrap(), "status": account.status() }));
        assert_eq!(account.status().promised, 0);
    }
    let totals = sweep.finish().unwrap();
    let until = std::time::Instant::now() + Duration::from_secs(2);
    let reclaimed = loop {
        let observed = account.refresh().unwrap();
        if allocated.allocated.saturating_sub(observed.allocated) >= 8 * mib
            || std::time::Instant::now() >= until
        {
            break observed;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    fs::write(
        report,
        serde_json::to_vec_pretty(&serde_json::json!({
            "initial": initial, "allocated": allocated, "reclaimed": reclaimed,
            "steps": steps, "totals": totals, "status": account.status(),
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(allocated.allocated.saturating_sub(reclaimed.allocated) >= 8 * mib);
    assert_eq!(totals.chunks_copied, live.len());
    assert!(!account.status().failed && !account.status().background_active);
    assert_tail_free(&store);
    drop((account, tickets, store));
    let store = Store::inspect(
        Tickets::open(root.path(), metadata()).unwrap(),
        config,
        metadata(),
        io_memory(),
    )
    .unwrap()
    .recover()
    .unwrap();
    for block in &live {
        read(&store, block);
    }
}
