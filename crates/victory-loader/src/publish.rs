//! Portable publication of derived files into a shared cache directory.
//!
//! Several copies of the runner can prepare the same title at once. Each
//! writer stages into its own file, so two of them cannot truncate each
//! other's bytes or rename a file the other already published. The final
//! rename is atomic within the directory, and the staging file is removed
//! if publication fails.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_STAGING: AtomicU64 = AtomicU64::new(0);

/// Writes `bytes` to `path` unless it is already there, using a staging file
/// unique to this process and call.
pub fn write_if_changed(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if std::fs::read(path).is_ok_and(|existing| existing == bytes) {
        return Ok(());
    }
    let (staging, mut file) = create_staging(path, &NEXT_STAGING)?;
    let written = file.write_all(bytes);
    drop(file);
    let published = written.and_then(|()| std::fs::rename(&staging, path));
    if published.is_err() {
        let _ = std::fs::remove_file(&staging);
    }
    published
}

/// A stale staging file may survive a process whose ID is later reused.
/// Open exclusively and retry only name collisions; never truncate that file.
fn create_staging(path: &Path, counter: &AtomicU64) -> io::Result<(PathBuf, File)> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("image");
    for _ in 0..128 {
        let sequence = counter.fetch_add(1, Ordering::Relaxed);
        let staging = path.with_file_name(format!(
            ".{name}.{}.{sequence}.tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)
        {
            Ok(file) => return Ok((staging, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not create a unique publication staging file after 128 attempts",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two publishers staging the same destination at once both succeed,
    /// and neither leaves a staging file behind.
    #[test]
    fn concurrent_publishers_do_not_share_a_staging_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.bin");
        let bytes = b"a prepared image".to_vec();
        let (first, second) = (path.clone(), path.clone());
        let (first_bytes, second_bytes) = (bytes.clone(), bytes.clone());
        let (one, two) = (
            std::thread::spawn(move || write_if_changed(&first, &first_bytes)),
            std::thread::spawn(move || write_if_changed(&second, &second_bytes)),
        );
        one.join().unwrap().unwrap();
        two.join().unwrap().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path() != path)
            .collect();
        assert!(leftovers.is_empty(), "staging files were removed: {leftovers:?}");
    }

    #[test]
    fn staging_collision_does_not_truncate_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.bin");
        let counter = AtomicU64::new(0);
        let stale = dir
            .path()
            .join(format!(".image.bin.{}.0.tmp", std::process::id()));
        std::fs::write(&stale, b"stale").unwrap();

        let (staging, mut file) = create_staging(&path, &counter).unwrap();
        file.write_all(b"fresh").unwrap();
        drop(file);
        assert_ne!(staging, stale);
        assert_eq!(std::fs::read(&staging).unwrap(), b"fresh");
        assert_eq!(std::fs::read(&stale).unwrap(), b"stale");
        assert_eq!(counter.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn staging_collision_retries_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.bin");
        let counter = AtomicU64::new(0);
        for n in 0..128 {
            std::fs::write(
                dir.path()
                    .join(format!(".image.bin.{}.{n}.tmp", std::process::id())),
                b"stale",
            )
            .unwrap();
        }
        let error = create_staging(&path, &counter).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(counter.load(Ordering::Relaxed), 128);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 128);
    }

    #[test]
    fn a_failed_publication_removes_its_staging_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.bin");
        // A directory at the destination makes rename fail after staging.
        std::fs::create_dir(&path).unwrap();
        assert!(write_if_changed(&path, b"bytes").is_err());
        assert!(path.is_dir());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn missing_parent_is_not_retried() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing-parent").join("image.bin");
        let counter = AtomicU64::new(0);
        let error = create_staging(&path, &counter).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }
}
