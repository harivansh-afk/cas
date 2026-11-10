//! Read complete telemetry records from one observed file-length snapshot.
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};

use serde_json::Value;

pub(super) fn refresh(
    file: &mut File,
    position: &mut u64,
    observe: impl FnMut(Value) -> io::Result<()>,
) -> io::Result<()> {
    let available = file
        .metadata()?
        .len()
        .checked_sub(*position)
        .ok_or_else(|| io::Error::other("telemetry file shrank"))?;
    file.seek(SeekFrom::Start(*position))?;
    read_available(file, available, position, observe)
}

fn read_available(
    reader: impl Read,
    available: u64,
    position: &mut u64,
    mut observe: impl FnMut(Value) -> io::Result<()>,
) -> io::Result<()> {
    // A growing 9p file can return ENODATA if a buffered read asks past its
    // current end. Stop at the observed length; the next poll sees appends.
    let mut reader = BufReader::new(reader.take(available));
    let mut line = String::new();
    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 || !line.ends_with('\n') {
            break;
        }
        if bytes > 1024 * 1024 {
            return Err(io::Error::other("telemetry record exceeds cap"));
        }
        observe(serde_json::from_str(&line)?)?;
        *position += bytes as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    struct NoDataAtEnd(Cursor<Vec<u8>>);

    impl Read for NoDataAtEnd {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.0.position() == self.0.get_ref().len() as u64 {
                return Err(io::Error::from_raw_os_error(libc::ENODATA));
            }
            self.0.read(buffer)
        }
    }

    #[test]
    fn a_growing_file_is_not_read_past_its_observed_end() {
        // 9p/netfs can return ENODATA when a tail read races an append.
        let bytes = b"{\"sample\":1}\n".to_vec();
        let length = bytes.len() as u64;
        let mut position = 0;
        let mut samples = Vec::new();
        read_available(
            NoDataAtEnd(Cursor::new(bytes)),
            length,
            &mut position,
            |value| {
                samples.push(value);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(position, length);
        assert_eq!(samples, [serde_json::json!({"sample": 1})]);
    }

    #[test]
    fn partial_records_are_retried_without_replaying_complete_records() {
        let mut writer = tempfile::NamedTempFile::new().unwrap();
        writer.write_all(b"{\"sample\":1}\n{\"sample\":").unwrap();
        let mut reader = File::open(writer.path()).unwrap();
        let mut position = 0;
        let mut samples = Vec::new();
        refresh(&mut reader, &mut position, |value| {
            samples.push(value);
            Ok(())
        })
        .unwrap();
        assert_eq!(position, 13);
        writer.write_all(b"2}\n").unwrap();
        refresh(&mut reader, &mut position, |value| {
            samples.push(value);
            Ok(())
        })
        .unwrap();
        assert_eq!(position, writer.as_file().metadata().unwrap().len());
        assert_eq!(
            samples,
            [
                serde_json::json!({"sample": 1}),
                serde_json::json!({"sample": 2})
            ]
        );
    }

    #[test]
    fn concurrent_appends_wait_for_the_next_snapshot() {
        let mut writer = tempfile::NamedTempFile::new().unwrap();
        writer.write_all(b"1\n").unwrap();
        let mut reader = File::open(writer.path()).unwrap();
        let mut position = 0;
        let mut samples = Vec::new();
        refresh(&mut reader, &mut position, |value| {
            if value == 1 {
                writer.write_all(b"2\n")?;
            }
            samples.push(value);
            Ok(())
        })
        .unwrap();
        assert_eq!(samples, [serde_json::json!(1)]);
        refresh(&mut reader, &mut position, |value| {
            samples.push(value);
            Ok(())
        })
        .unwrap();
        assert_eq!(samples, [serde_json::json!(1), serde_json::json!(2)]);
    }

    #[test]
    fn invalid_records_and_io_errors_are_still_rejected() {
        let mut position = 0;
        assert!(read_available(Cursor::new(b"invalid\n"), 8, &mut position, |_| Ok(())).is_err());
        assert_eq!(position, 0);
        let error = read_available(
            NoDataAtEnd(Cursor::new(Vec::new())),
            1,
            &mut position,
            |_| Ok(()),
        )
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENODATA));
        assert_eq!(position, 0);
        assert!(
            read_available(Cursor::new(b"1\n"), 2, &mut position, |_| {
                Err(io::Error::other("invalid sample"))
            })
            .is_err()
        );
        assert_eq!(position, 0);
    }
}
