#![cfg(target_os = "linux")]

use std::fs::{self, OpenOptions};
use std::os::unix::fs::FileExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use cas_core::BLOCK_SIZE;
use cas_core::staging::{Error, RECORD_SIZE, StagingLog};
use tempfile::TempDir;

fn directory() -> TempDir {
    // Exercise the checkout's filesystem, not a possibly tmpfs-backed /tmp.
    tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap()
}

fn image_bytes() -> u64 {
    (16 * BLOCK_SIZE) as u64
}

#[test]
fn flush_replay_preserves_last_write_and_discards_unflushed_overwrite() {
    let directory = directory();
    let path = directory.path().join("image.log");
    let mut log = StagingLog::create(&path, image_bytes()).unwrap();
    assert_eq!(log.read(0, BLOCK_SIZE).unwrap(), vec![0; BLOCK_SIZE]);
    log.write(0, &vec![1; 2 * BLOCK_SIZE]).unwrap();
    log.write(0, &vec![2; BLOCK_SIZE]).unwrap();
    assert_eq!(log.status().durable, 0);
    assert_eq!(log.flush().unwrap(), 3);
    log.write(0, &vec![3; BLOCK_SIZE]).unwrap();
    assert_eq!(log.read(0, BLOCK_SIZE).unwrap(), vec![3; BLOCK_SIZE]);
    drop(log);

    let mut log = StagingLog::open(&path).unwrap();
    assert_eq!(log.read(0, BLOCK_SIZE).unwrap(), vec![2; BLOCK_SIZE]);
    assert_eq!(
        log.read(BLOCK_SIZE as u64, BLOCK_SIZE).unwrap(),
        vec![1; BLOCK_SIZE]
    );
    assert_eq!(log.status().durable, 3);
    assert_eq!(log.status().recovered_tail_bytes, RECORD_SIZE as u64);
    assert_eq!(log.write(0, &vec![4; BLOCK_SIZE]).unwrap(), Some(4));
    log.flush().unwrap();
    drop(log);
    let log = StagingLog::open(&path).unwrap();
    assert_eq!(log.read(0, BLOCK_SIZE).unwrap(), vec![4; BLOCK_SIZE]);
}

#[test]
fn no_flush_recovers_the_initial_zero_image() {
    let directory = directory();
    let path = directory.path().join("image.log");
    let mut log = StagingLog::create(&path, image_bytes()).unwrap();
    log.write(0, &vec![1; BLOCK_SIZE]).unwrap();
    drop(log);
    let log = StagingLog::open(&path).unwrap();
    assert_eq!(log.status().appended, 0);
    assert_eq!(log.status().log_bytes, BLOCK_SIZE as u64);
    assert_eq!(log.read(0, BLOCK_SIZE).unwrap(), vec![0; BLOCK_SIZE]);
}

#[test]
fn empty_discard_has_no_sequence_and_large_zero_uses_one_record() {
    let directory = directory();
    let path = directory.path().join("image.log");
    let size = 1024 * 1024 * 1024 * 1024;
    let mut log = StagingLog::create(&path, size).unwrap();
    assert_eq!(log.zero(0, 0).unwrap(), None);
    assert_eq!(log.flush().unwrap(), 0);
    assert_eq!(
        log.write(size - BLOCK_SIZE as u64, &vec![9; BLOCK_SIZE])
            .unwrap(),
        Some(1)
    );
    let before = log.status().log_bytes;
    assert_eq!(log.zero(0, size).unwrap(), Some(2));
    assert_eq!(log.status().log_bytes - before, RECORD_SIZE as u64);
    assert_eq!(log.status().mapped_blocks, 0);
    assert_eq!(log.flush().unwrap(), 2);
    drop(log);
    let log = StagingLog::open(&path).unwrap();
    assert_eq!(
        log.read(size - BLOCK_SIZE as u64, BLOCK_SIZE).unwrap(),
        vec![0; BLOCK_SIZE]
    );
}

#[test]
fn invalid_requests_leave_the_append_order_unchanged() {
    let directory = directory();
    let path = directory.path().join("image.log");
    let mut log = StagingLog::create(&path, image_bytes()).unwrap();
    let initial = log.status();
    assert!(matches!(
        log.write(1, &vec![0; BLOCK_SIZE]),
        Err(Error::Range)
    ));
    assert!(matches!(log.write(0, &[0; 512]), Err(Error::Range)));
    assert!(matches!(
        log.zero(image_bytes(), BLOCK_SIZE as u64),
        Err(Error::Range)
    ));
    assert!(matches!(log.zero(u64::MAX - 4095, 8192), Err(Error::Range)));
    assert_eq!(log.status(), initial);
}

#[test]
fn one_writer_and_create_new_protect_existing_logs() {
    let directory = directory();
    let path = directory.path().join("image.log");
    let log = StagingLog::create(&path, image_bytes()).unwrap();
    assert!(StagingLog::open(&path).is_err());
    assert!(StagingLog::create(&path, image_bytes()).is_err());
    drop(log);
    StagingLog::open(&path).unwrap();
}

#[test]
fn truncated_and_zero_padded_tails_preserve_the_previous_flush() {
    let directory = directory();
    let original = directory.path().join("original.log");
    let mut log = StagingLog::create(&original, image_bytes()).unwrap();
    log.write(0, &vec![0x51; BLOCK_SIZE]).unwrap();
    log.flush().unwrap();
    let cut_start = log.status().log_bytes as usize;
    log.write(0, &vec![0x72; BLOCK_SIZE]).unwrap();
    log.flush().unwrap();
    drop(log);
    let complete = fs::read(&original).unwrap();
    let cuts = [
        0, 1, 8, 39, 4091, 4092, 4095, 4096, 4097, 8191, 8192, 8193, 12287, 12288, 12289, 16383,
        16384,
    ];
    for padded in [false, true] {
        for cut in cuts {
            let path = directory.path().join(format!("tail-{padded}-{cut}.log"));
            let mut bytes = complete[..cut_start + cut].to_vec();
            if padded {
                bytes.resize(complete.len() + RECORD_SIZE, 0);
            }
            fs::write(&path, bytes).unwrap();
            let log = StagingLog::open(&path).unwrap();
            // Once the meaningful fence header is complete, its zero payload
            // can already form a valid record in a preallocated zero tail.
            // Recovering an unacknowledged but complete batch is permitted.
            let seq = log.status().durable;
            assert!(seq == 1 || seq == 2, "padded={padded} cut={cut} seq={seq}");
            if cut < RECORD_SIZE + BLOCK_SIZE {
                assert_eq!(seq, 1, "padded={padded} cut={cut}");
            }
            if cut == 2 * RECORD_SIZE {
                assert_eq!(seq, 2);
            }
            let expected = if seq == 1 { 0x51 } else { 0x72 };
            assert_eq!(log.read(0, BLOCK_SIZE).unwrap(), vec![expected; BLOCK_SIZE]);
        }
    }
}

#[test]
fn corruption_before_a_valid_fence_is_rejected_without_truncation() {
    let directory = directory();
    let path = directory.path().join("image.log");
    let mut log = StagingLog::create(&path, image_bytes()).unwrap();
    log.write(0, &vec![0x51; BLOCK_SIZE]).unwrap();
    log.flush().unwrap();
    let length = log.status().log_bytes;
    drop(log);
    let file = OpenOptions::new().write(true).open(&path).unwrap();
    file.write_at(&[0x99], (2 * BLOCK_SIZE) as u64).unwrap();
    file.sync_all().unwrap();
    drop(file);
    assert!(matches!(StagingLog::open(&path), Err(Error::Corrupt(4096))));
    assert_eq!(fs::metadata(&path).unwrap().len(), length);
}

#[test]
fn guest_payload_cannot_forge_a_flush_record() {
    let directory = directory();
    let source = directory.path().join("source.log");
    let mut log = StagingLog::create(&source, image_bytes()).unwrap();
    log.write(0, &vec![1; BLOCK_SIZE]).unwrap();
    log.flush().unwrap();
    drop(log);
    let bytes = fs::read(source).unwrap();
    let fence_header = &bytes[BLOCK_SIZE + RECORD_SIZE..2 * BLOCK_SIZE + RECORD_SIZE];
    let path = directory.path().join("forged.log");
    let mut log = StagingLog::create(&path, image_bytes()).unwrap();
    log.write(0, fence_header).unwrap();
    drop(log);
    let log = StagingLog::open(&path).unwrap();
    assert_eq!(log.status().durable, 0);
}

#[test]
fn corrupted_superblock_is_rejected() {
    let directory = directory();
    let path = directory.path().join("image.log");
    drop(StagingLog::create(&path, image_bytes()).unwrap());
    let file = OpenOptions::new().write(true).open(&path).unwrap();
    file.write_at(&[1], 8).unwrap();
    drop(file);
    assert!(matches!(StagingLog::open(path), Err(Error::Header)));
}

#[test]
fn mixed_operations_match_a_byte_array_oracle_across_restarts() {
    let directory = directory();
    let path = directory.path().join("image.log");
    let mut log = StagingLog::create(&path, image_bytes()).unwrap();
    let mut visible = vec![0; image_bytes() as usize];
    let mut durable = visible.clone();
    let mut random = 0x6a09_e667_f3bc_c909u64;
    for step in 0..160 {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let start = ((random >> 8) as usize % 15) * BLOCK_SIZE;
        let length = (1 + (random as usize % 2)) * BLOCK_SIZE;
        match random % 5 {
            0 | 1 => {
                let payload = vec![step as u8; length];
                log.write(start as u64, &payload).unwrap();
                visible[start..start + length].copy_from_slice(&payload);
            }
            2 => {
                log.zero(start as u64, length as u64).unwrap();
                visible[start..start + length].fill(0);
            }
            3 => {
                log.flush().unwrap();
                durable.clone_from(&visible);
                let size = log.status().log_bytes;
                log.flush().unwrap();
                assert_eq!(log.status().log_bytes, size);
            }
            4 => {
                drop(log);
                log = StagingLog::open(&path).unwrap();
                visible.clone_from(&durable);
            }
            _ => unreachable!(),
        }
        assert_eq!(log.read(0, visible.len()).unwrap(), visible, "step={step}");
    }
}

#[test]
fn crash_child() {
    let Some(directory) = std::env::var_os("CAS_STAGING_CRASH_CHILD") else {
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    let mut log = StagingLog::create(directory.join("image.log"), image_bytes()).unwrap();
    log.write(0, &vec![0x51; BLOCK_SIZE]).unwrap();
    log.flush().unwrap();
    log.write(0, &vec![0x72; BLOCK_SIZE]).unwrap();
    fs::write(directory.join("ready"), b"flushed").unwrap();
    // Parent kills this process; there is no normal destructor/reopen path.
    loop {
        std::thread::park();
    }
}

#[test]
fn sigkill_preserves_the_acknowledged_flush() {
    let directory = directory();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_child", "--nocapture"])
        .env("CAS_STAGING_CRASH_CHILD", directory.path())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !directory.path().join("ready").exists() {
        if Instant::now() > deadline || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("crash fixture did not reach the post-FLUSH checkpoint");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    let log = StagingLog::open(directory.path().join("image.log")).unwrap();
    assert_eq!(log.status().durable, 1);
    assert_eq!(log.read(0, BLOCK_SIZE).unwrap(), vec![0x51; BLOCK_SIZE]);
}
