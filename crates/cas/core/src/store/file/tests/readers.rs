use super::*;
use std::{sync::mpsc, time::Duration};

#[test]
fn readers_progress_during_output_and_new_hashes_wait_for_sync() {
    for pause in [Fault::Allocate, Fault::ShortWrite, Fault::Sync] {
        for rotate in [false, true] {
            for fail in [false, true] {
                paused_output(pause, rotate, fail);
            }
        }
    }
}

fn paused_output(pause: Fault, rotate: bool, fail: bool) {
    let root = tempfile::tempdir().unwrap();
    let mut store = create(root.path());
    let blocks = (1..=if rotate { MAX_CHUNKS } else { 1 })
        .map(|byte| [byte as u8; BLOCK_SIZE])
        .collect::<std::vec::Vec<_>>();
    insert(&mut store, &blocks).unwrap();
    let previous_chunks = store.status().chunks;
    let reader = store.reader().unwrap();
    let (entered, observed) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || {
            faults::pause_before(pause, entered, resume);
            if fail {
                faults::inject(Fault::Sync);
            }
            let result = insert(&mut store, &[[64; BLOCK_SIZE]]);
            (store, result)
        });
        observed.recv_timeout(Duration::from_secs(3)).unwrap();
        let reading = reader.clone();
        let (completed, completion) = mpsc::channel();
        scope.spawn(move || {
            let old = reading.plan(hash(&[1; BLOCK_SIZE])).unwrap().unwrap();
            let mut data = AlignedBuffer::new(BLOCK_SIZE);
            old.load(data.as_mut_slice()).unwrap();
            let correct = data.as_slice() == [1; BLOCK_SIZE]
                && reading.plan(hash(&[64; BLOCK_SIZE])).unwrap().is_none()
                && !reading.status().failed;
            completed.send(correct).unwrap();
        });
        // Release even on regression so a held lookup lock cannot hang the test.
        let outcome = completion.recv_timeout(Duration::from_secs(1));
        release.send(()).unwrap();
        let (store, result) = writer.join().unwrap();
        assert!(outcome.unwrap(), "old read must finish before writer sync");
        assert_eq!(result.is_err(), fail);
        assert_eq!(reader.status().failed, fail);
        if fail {
            assert_eq!(reader.status().chunks, previous_chunks);
            assert!(reader.plan(hash(&[1; BLOCK_SIZE])).is_err());
            assert!(reader.plan(hash(&[64; BLOCK_SIZE])).is_err());
            assert!(store.reader().is_err());
        } else {
            let new = reader.plan(hash(&[64; BLOCK_SIZE])).unwrap().unwrap();
            let mut data = AlignedBuffer::new(BLOCK_SIZE);
            new.load(data.as_mut_slice()).unwrap();
            assert_eq!(data.as_slice(), &[64; BLOCK_SIZE]);
        }
    });
}

#[test]
fn reader_and_each_resumed_io_stage_retain_actual_file_exclusion() {
    let root = tempfile::tempdir().unwrap();
    let mut store = create(root.path());
    insert(&mut store, &[[4; BLOCK_SIZE], [5; BLOCK_SIZE]]).unwrap();
    let reader = store.reader().unwrap();
    drop(store);
    assert!(inspect(root.path()).is_err());
    let read = reader.plan(hash(&[5; BLOCK_SIZE])).unwrap().unwrap();
    drop(reader);
    assert!(inspect(root.path()).is_err());
    let mut page = AlignedBuffer::new(BLOCK_SIZE);
    direct::read_bytes(read.file(), page.as_mut_slice(), read.header_offset()).unwrap();
    let payload = read.payload(page.as_slice()).unwrap();
    for (batch, ordinal) in [(2, 1), (1, 2)] {
        let mut wrong = Builder::new(2).unwrap();
        wrong.push(Chunk::new(&[4; BLOCK_SIZE]).unwrap()).unwrap();
        wrong.push(Chunk::new(&[5; BLOCK_SIZE]).unwrap()).unwrap();
        let wrong = wrong
            .seal(read.address().segment(), batch, ordinal)
            .unwrap();
        assert!(read.payload(&wrong.bytes()[..BLOCK_SIZE]).is_err());
    }
    assert!(read.payload(&page.as_slice()[..BLOCK_SIZE - 1]).is_err());
    page.as_mut_slice()[100] ^= 1;
    assert!(read.payload(page.as_slice()).is_err());
    drop(read);
    assert!(inspect(root.path()).is_err());
    direct::read_bytes(payload.file(), page.as_mut_slice(), payload.offset()).unwrap();
    payload.verify(page.as_slice()).unwrap();
    assert_eq!(page.as_slice(), &[5; BLOCK_SIZE]);
    assert!(payload.verify(&page.as_slice()[..BLOCK_SIZE - 1]).is_err());
    page.as_mut_slice()[0] ^= 1;
    assert!(payload.verify(page.as_slice()).is_err());
    drop(payload);
    super::read(
        &inspect(root.path()).unwrap().recover().unwrap(),
        &[5; BLOCK_SIZE],
    );
}

#[test]
fn shared_lookup_tables_are_charged_once_until_the_last_reader_drops() {
    let root = tempfile::tempdir().unwrap();
    let metadata = metadata();
    let mut store = Store::create(
        Tickets::open(root.path(), Arc::clone(&metadata)).unwrap(),
        CONFIG,
        Arc::clone(&metadata),
        io_memory(),
    )
    .unwrap();
    insert(&mut store, &[[1; BLOCK_SIZE], [2; BLOCK_SIZE]]).unwrap();
    let charged = metadata.usage().current.bytes;
    let readers: [_; 8] = std::array::from_fn(|_| store.reader().unwrap());
    assert_eq!(metadata.usage().current.bytes, charged);
    drop(store);
    assert_eq!(metadata.usage().current.bytes, charged);
    drop(readers);
    assert_eq!(metadata.usage().current.bytes, 0);
}

#[test]
fn unwinding_output_poisons_all_shared_readers() {
    let root = tempfile::tempdir().unwrap();
    let mut store = create(root.path());
    insert(&mut store, &[[1; BLOCK_SIZE]]).unwrap();
    let reader = store.reader().unwrap();
    let (entered, observed) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        faults::pause_before(Fault::Sync, entered, resume);
        insert(&mut store, &[[2; BLOCK_SIZE]])
    });
    observed.recv_timeout(Duration::from_secs(3)).unwrap();
    // A disconnected pause deliberately panics at the syscall boundary.
    drop(release);
    assert!(writer.join().is_err());
    assert!(reader.status().failed);
    assert_eq!(reader.status().chunks, 1);
    assert!(reader.plan(hash(&[1; BLOCK_SIZE])).is_err());
    drop(reader);
    let recovered = inspect(root.path()).unwrap().recover().unwrap();
    read(&recovered, &[1; BLOCK_SIZE]);
    read(&recovered, &[2; BLOCK_SIZE]);
}
