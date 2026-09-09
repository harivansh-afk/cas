//! Offline fixed-size content census. Callers supply immutable, normalized raw
//! images: filesystem allocation and clone ancestry cannot be inferred here.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::PathBuf;

use serde::Serialize;

pub const CHUNK_SIZES: [usize; 2] = [4096, 16384];

#[derive(Debug, Serialize)]
pub struct Census {
    pub chunk_bytes: usize,
    pub images: Vec<Image>,
    pub nonzero_chunk_bytes: u64,
    /// Sum of each image's unique bytes, as if deduplicated separately.
    pub independently_unique_bytes: u64,
    /// Unique bytes across all images, excluding all-zero chunks.
    pub fleet_unique_bytes: u64,
    pub within_image_duplicate_bytes: u64,
    pub cross_image_duplicate_bytes: u64,
}

#[derive(Debug, Default, Serialize)]
pub struct Image {
    pub path: PathBuf,
    pub blake3: String,
    pub logical_bytes: u64,
    pub zero_chunk_bytes: u64,
    pub nonzero_chunk_bytes: u64,
    pub unique_bytes: u64,
    // These five mutually exclusive categories partition nonzero_chunk_bytes.
    pub base_in_place_bytes: u64,
    pub base_elsewhere_bytes: u64,
    pub prior_image_only_bytes: u64,
    pub within_image_only_bytes: u64,
    pub new_unique_bytes: u64,
}

/// Count BLAKE3-equal chunks anchored at offset zero of each input. The first
/// input is the base. A short final chunk is hashed and counted at its actual
/// length, without padding. Equality at a base offset does not prove ancestry.
pub fn scan(paths: &[PathBuf], chunk_bytes: usize) -> io::Result<Census> {
    if paths.is_empty() || !CHUNK_SIZES.contains(&chunk_bytes) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "census requires images and a 4096 or 16384 byte chunk size",
        ));
    }
    let mut seen = HashMap::new();
    let mut base_offsets = Vec::new();
    let mut images = Vec::new();
    let mut fleet_unique_bytes = 0;
    for (index, path) in paths.iter().enumerate() {
        let mut input = BufReader::with_capacity(1024 * 1024, File::open(path)?);
        if !input.get_ref().metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected a regular raw file",
            ));
        }
        let before = input.get_ref().metadata()?;
        let mut image = Image {
            path: path.clone(),
            ..Image::default()
        };
        let mut local = HashSet::new();
        let mut digest = blake3::Hasher::new();
        let mut buffer = vec![0; chunk_bytes];
        let mut offset = 0;
        loop {
            let len = read_chunk(&mut input, &mut buffer)?;
            if len == 0 {
                break;
            }
            let bytes = &buffer[..len];
            digest.update(bytes);
            image.logical_bytes += len as u64;
            let hash = bytes
                .iter()
                .any(|&byte| byte != 0)
                .then(|| blake3::hash(bytes));
            if index == 0 {
                base_offsets.push(hash);
            }
            if let Some(hash) = hash {
                let len = len as u64;
                image.nonzero_chunk_bytes += len;
                if local.insert(hash) {
                    image.unique_bytes += len;
                }
                match seen.get(&hash) {
                    Some(&0) if index > 0 => {
                        if base_offsets.get(offset) == Some(&Some(hash)) {
                            image.base_in_place_bytes += len;
                        } else {
                            image.base_elsewhere_bytes += len;
                        }
                    }
                    Some(&first) if first < index => image.prior_image_only_bytes += len,
                    Some(_) => image.within_image_only_bytes += len,
                    None => {
                        seen.insert(hash, index);
                        image.new_unique_bytes += len;
                        fleet_unique_bytes += len;
                    }
                }
            } else {
                image.zero_chunk_bytes += len as u64;
            }
            offset += 1;
        }
        let after = input.get_ref().metadata()?;
        if before.len() != image.logical_bytes
            || after.len() != before.len()
            || after.modified()? != before.modified()?
        {
            return Err(io::Error::other(format!(
                "image changed while scanning: {}",
                path.display()
            )));
        }
        image.blake3 = digest.finalize().to_hex().to_string();
        images.push(image);
    }
    let nonzero_chunk_bytes = images
        .iter()
        .map(|image| image.nonzero_chunk_bytes)
        .sum::<u64>();
    let independently_unique_bytes = images.iter().map(|image| image.unique_bytes).sum::<u64>();
    Ok(Census {
        chunk_bytes,
        images,
        nonzero_chunk_bytes,
        independently_unique_bytes,
        fleet_unique_bytes,
        within_image_duplicate_bytes: nonzero_chunk_bytes - independently_unique_bytes,
        cross_image_duplicate_bytes: independently_unique_bytes - fleet_unique_bytes,
    })
}

fn read_chunk(input: &mut impl Read, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match input.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(len) => filled += len,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture(dir: &Path, name: &str, blocks: &[u8]) -> PathBuf {
        let path = dir.join(name);
        let bytes: Vec<_> = blocks.iter().flat_map(|&byte| [byte; 4096]).collect();
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn partitions_base_moved_local_and_prior_content_without_double_counting() {
        let dir = tempfile::tempdir().unwrap();
        let paths = [
            fixture(dir.path(), "base", &[1, 2, 0, 1]),
            fixture(dir.path(), "second", &[1, 1, 3, 3]),
            fixture(dir.path(), "third", &[3, 4, 0, 0]),
        ];
        let result = scan(&paths, 4096).unwrap();
        let second = &result.images[1];
        assert_eq!(second.base_in_place_bytes, 4096);
        assert_eq!(second.base_elsewhere_bytes, 4096);
        assert_eq!(second.within_image_only_bytes, 4096);
        assert_eq!(second.new_unique_bytes, 4096);
        assert_eq!(result.images[2].prior_image_only_bytes, 4096);
        assert_eq!(result.nonzero_chunk_bytes, 9 * 4096);
        assert_eq!(result.fleet_unique_bytes, 4 * 4096);
        assert_eq!(result.within_image_duplicate_bytes, 3 * 4096);
        assert_eq!(result.cross_image_duplicate_bytes, 2 * 4096);
        for image in &result.images {
            assert_eq!(
                image.logical_bytes,
                image.zero_chunk_bytes + image.nonzero_chunk_bytes
            );
            assert_eq!(
                image.nonzero_chunk_bytes,
                image.base_in_place_bytes
                    + image.base_elsewhere_bytes
                    + image.prior_image_only_bytes
                    + image.within_image_only_bytes
                    + image.new_unique_bytes
            );
        }
    }

    #[test]
    fn clone_and_changed_neighbor_distinguish_four_and_sixteen_kib() {
        let dir = tempfile::tempdir().unwrap();
        let base = fixture(dir.path(), "base", &[1, 2, 3, 4]);
        for size in CHUNK_SIZES {
            let clone = scan(&[base.clone(), base.clone()], size).unwrap();
            assert_eq!(clone.fleet_unique_bytes, 16384);
            assert_eq!(clone.cross_image_duplicate_bytes, 16384);
            assert_eq!(clone.images[1].base_in_place_bytes, 16384);
        }
        let changed = fixture(dir.path(), "changed", &[1, 2, 3, 5]);
        assert_eq!(
            scan(&[base.clone(), changed.clone()], 4096)
                .unwrap()
                .cross_image_duplicate_bytes,
            12288
        );
        assert_eq!(
            scan(&[base, changed], 16384)
                .unwrap()
                .cross_image_duplicate_bytes,
            0
        );
    }

    #[test]
    fn excludes_zero_chunks_and_counts_short_tails_without_padding() {
        let dir = tempfile::tempdir().unwrap();
        let zero = fixture(dir.path(), "zero", &[0; 4]);
        let empty = fixture(dir.path(), "empty", &[]);
        let tail = dir.path().join("tail");
        std::fs::write(&tail, [7; 3]).unwrap();
        for size in CHUNK_SIZES {
            let result = scan(
                &[zero.clone(), empty.clone(), tail.clone(), tail.clone()],
                size,
            )
            .unwrap();
            assert_eq!(result.images[0].zero_chunk_bytes, 16384);
            assert_eq!(result.nonzero_chunk_bytes, 6);
            assert_eq!(result.fleet_unique_bytes, 3);
            assert_eq!(result.cross_image_duplicate_bytes, 3);
        }
    }

    #[test]
    fn rejects_invalid_inputs_and_propagates_read_errors() {
        assert!(scan(&[], 4096).is_err());
        assert!(scan(&[PathBuf::from("missing")], 4096).is_err());
        assert!(scan(&[PathBuf::from(".")], 4096).is_err());
        assert!(scan(&[PathBuf::from(".")], 8192).is_err());
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("broken"))
            }
        }
        assert!(read_chunk(&mut Broken, &mut [0; 4]).is_err());
    }

    #[test]
    fn fills_chunks_across_short_and_interrupted_reads() {
        struct Short {
            interrupted: bool,
            data: io::Cursor<Vec<u8>>,
        }
        impl Read for Short {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                self.data.read(&mut buffer[..1])
            }
        }
        let mut input = Short {
            interrupted: false,
            data: io::Cursor::new(vec![1, 2, 3]),
        };
        let mut buffer = [0; 4];
        assert_eq!(read_chunk(&mut input, &mut buffer).unwrap(), 3);
        assert_eq!(buffer, [1, 2, 3, 0]);
    }
}
