//! Content identities for build inputs and retained evidence. Never follow symlinks.
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::process;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub bytes: u64,
    pub executable: bool,
    pub blake3: String,
    pub symlink: Option<PathBuf>,
}

pub type Manifest = BTreeMap<PathBuf, Entry>;

pub fn relative(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty()
        || !path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(io::Error::other(
            "manifest paths must be relative file names",
        ));
    }
    Ok(())
}

pub fn entry(path: &Path) -> io::Result<Entry> {
    let metadata = path.symlink_metadata()?;
    if metadata.is_symlink() {
        let target = fs::read_link(path)?;
        let bytes = target.as_os_str().as_encoded_bytes();
        return Ok(Entry {
            bytes: bytes.len() as u64,
            executable: false,
            blake3: blake3::hash(bytes).to_hex().to_string(),
            symlink: Some(target),
        });
    }
    if !metadata.is_file() {
        return Err(io::Error::other(format!(
            "not a regular input: {}",
            path.display()
        )));
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(fs::File::open(path)?)?;
    Ok(Entry {
        bytes: metadata.len(),
        executable: metadata.permissions().mode() & 0o111 != 0,
        blake3: hasher.finalize().to_hex().to_string(),
        symlink: None,
    })
}

pub fn scan(root: &Path) -> io::Result<Manifest> {
    fn visit(root: &Path, directory: &Path, entries: &mut Manifest) -> io::Result<()> {
        for item in fs::read_dir(directory)? {
            let item = item?;
            let path = item.path();
            if item.file_type()?.is_dir() {
                visit(root, &path, entries)?;
            } else {
                entries.insert(path.strip_prefix(root).unwrap().to_owned(), entry(&path)?);
            }
        }
        Ok(())
    }
    let mut entries = Manifest::new();
    visit(root, root, &mut entries)?;
    Ok(entries)
}

pub fn from_paths(root: &Path, paths: impl IntoIterator<Item = PathBuf>) -> io::Result<Manifest> {
    let mut manifest = Manifest::new();
    for path in paths {
        relative(&path)?;
        // Git still lists tracked paths removed in a dirty checkout.
        match entry(&root.join(&path)) {
            Ok(value) => {
                manifest.insert(path, value);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
    }
    Ok(manifest)
}

pub fn copy(root: &Path, destination: &Path, manifest: &Manifest) -> io::Result<()> {
    fs::create_dir(destination)?;
    for (path, item) in manifest {
        relative(path)?;
        let target = destination.join(path);
        fs::create_dir_all(target.parent().unwrap())?;
        match &item.symlink {
            Some(link) => symlink(link, target)?,
            None => {
                fs::copy(root.join(path), target)?;
            }
        }
    }
    compare(manifest, &scan(destination)?, "archived source")
}

pub fn compare(expected: &Manifest, actual: &Manifest, label: &str) -> io::Result<()> {
    if expected != actual {
        let path = expected
            .keys()
            .chain(actual.keys())
            .find(|path| expected.get(*path) != actual.get(*path))
            .expect("unequal maps have a differing entry");
        return Err(io::Error::other(format!(
            "{label} differs at {}",
            path.display()
        )));
    }
    Ok(())
}

pub fn text_command(argv: &[&str], cwd: &Path, directory: &Path) -> io::Result<String> {
    let mut command = Command::new(argv[0]);
    command.args(&argv[1..]).current_dir(cwd);
    let result = process::run_logged(&mut command, directory, Duration::from_secs(100))?;
    if result.exit_code != Some(0) || result.error.is_some() {
        return Err(io::Error::other(format!(
            "{} failed; see {}",
            argv[0],
            directory.display()
        )));
    }
    fs::read_to_string(directory.join("stdout.log"))
}

pub fn checkout_inputs(checkout: &Path, directory: &Path) -> io::Result<Manifest> {
    let files = text_command(
        &[
            "git",
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
        checkout,
        directory,
    )?;
    from_paths(
        checkout,
        files
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(PathBuf::from),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_includes_contents_executability_symlinks_and_file_set() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("input");
        fs::write(&file, "one").unwrap();
        symlink("input", directory.path().join("link")).unwrap();
        let original = scan(directory.path()).unwrap();
        fs::write(&file, "two").unwrap();
        assert!(compare(&original, &scan(directory.path()).unwrap(), "build").is_err());
        fs::write(&file, "one").unwrap();
        let mode = fs::metadata(&file).unwrap().permissions().mode();
        fs::set_permissions(&file, fs::Permissions::from_mode(mode | 0o100)).unwrap();
        assert!(compare(&original, &scan(directory.path()).unwrap(), "mode").is_err());
        fs::set_permissions(&file, fs::Permissions::from_mode(mode)).unwrap();
        fs::remove_file(directory.path().join("link")).unwrap();
        symlink("elsewhere", directory.path().join("link")).unwrap();
        assert!(compare(&original, &scan(directory.path()).unwrap(), "link").is_err());
        assert!(relative(Path::new("../escape")).is_err());
        assert!(relative(Path::new("/absolute")).is_err());
    }
}
