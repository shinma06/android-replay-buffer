//! Crash-safe output publishing shared by the muxers, [`crate::trim::trim_video`] and PNG
//! export (R2 S6).
//!
//! Every output is written to a unique sibling temp file `<out>.tmp-<pid>-<nonce>`, flushed and
//! `fsync`ed, and only then published at its final path, so a crash never leaves a truncated
//! file at the destination. Derived outputs (trim, PNG export) are published **without
//! replacing** an existing file and refuse to target their own input, so a source bundle
//! file (WORM, SPEC-02 §3) can never be clobbered.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::MediaError;

/// Whether publishing may overwrite an existing destination file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Publish {
    /// Atomically replace any existing file (muxer convenience APIs).
    Replace,
    /// Fail with [`io::ErrorKind::AlreadyExists`] if the destination exists.
    NoReplace,
}

static TEMP_NONCE: AtomicU64 = AtomicU64::new(0);

fn temp_sibling(final_path: &Path) -> Result<PathBuf, MediaError> {
    let file_name = final_path.file_name().ok_or_else(|| {
        MediaError::InvalidArgument(format!(
            "output path has no file name: {}",
            final_path.display()
        ))
    })?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let seq = TEMP_NONCE.fetch_add(1, Ordering::Relaxed);
    let mut name = file_name.to_os_string();
    name.push(format!(".tmp-{}-{seq:x}{nanos:x}", std::process::id()));
    Ok(final_path.with_file_name(name))
}

/// Canonical form of `path` even when it does not exist yet (canonical parent + file name).
fn canonical_target(path: &Path) -> io::Result<PathBuf> {
    if let Ok(canon) = fs::canonicalize(path) {
        return Ok(canon);
    }
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    Ok(fs::canonicalize(parent)?.join(file_name))
}

/// Refuses an `output` that resolves to the same file as `input` (symlinks included).
///
/// Hard-link aliases resolve to different paths, but they already exist, so the
/// [`Publish::NoReplace`] publish step refuses them as well.
pub(crate) fn ensure_output_is_not_input(input: &Path, output: &Path) -> Result<(), MediaError> {
    let canonical_input = fs::canonicalize(input)?;
    let canonical_output = canonical_target(output)?;
    if canonical_input == canonical_output {
        return Err(MediaError::InvalidArgument(format!(
            "output path {} is the input file; refusing to overwrite the source",
            output.display()
        )));
    }
    Ok(())
}

/// Writes `final_path` crash-safely: `write` fills a unique temp sibling, which is flushed,
/// `fsync`ed and then published according to `mode`. The temp file is removed on any error.
/// Returns the published file's length in bytes.
pub(crate) fn write_atomically(
    final_path: &Path,
    mode: Publish,
    write: impl FnOnce(&mut BufWriter<File>) -> Result<(), MediaError>,
) -> Result<u64, MediaError> {
    write_verified(final_path, mode, write, |_| Ok(()))
}

pub(crate) fn write_verified(
    final_path: &Path,
    mode: Publish,
    write: impl FnOnce(&mut BufWriter<File>) -> Result<(), MediaError>,
    verify: impl FnOnce(&Path) -> Result<(), MediaError>,
) -> Result<u64, MediaError> {
    if mode == Publish::NoReplace && final_path.exists() {
        return Err(MediaError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("output {} already exists", final_path.display()),
        )));
    }
    let tmp = temp_sibling(final_path)?;
    let result = (|| -> Result<u64, MediaError> {
        let file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        let mut writer = BufWriter::with_capacity(1 << 20, file);
        write(&mut writer)?;
        let file = writer
            .into_inner()
            .map_err(io::IntoInnerError::into_error)?;
        file.sync_all()?;
        let len = file.metadata()?.len();
        drop(file);
        verify(&tmp)?;
        publish(&tmp, final_path, mode)?;
        Ok(len)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    if result.is_ok()
        && let Some(parent) = final_path.parent()
    {
        // Best effort: persist the directory entry (not supported for directories everywhere).
        let _ = File::open(parent).and_then(|dir| dir.sync_all());
    }
    result
}

fn publish(tmp: &Path, final_path: &Path, mode: Publish) -> Result<(), MediaError> {
    match mode {
        Publish::Replace => fs::rename(tmp, final_path)?,
        Publish::NoReplace => match fs::hard_link(tmp, final_path) {
            // `link` is an atomic create-if-absent on every platform that supports it.
            Ok(()) => {
                fs::remove_file(tmp)?;
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                return Err(MediaError::Io(err));
            }
            // Filesystems without hard links (FAT/exFAT): best-effort check-then-rename.
            Err(_) => {
                if final_path.exists() {
                    return Err(MediaError::Io(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("output {} already exists", final_path.display()),
                    )));
                }
                fs::rename(tmp, final_path)?;
            }
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_verification_allows_retry_at_same_path() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("share.mp4");
        let rejected = write_verified(
            &path,
            Publish::NoReplace,
            |_| Ok(()),
            |_| Err(MediaError::InvalidContainer("invalid".into())),
        );
        assert!(rejected.is_err());
        assert!(!path.exists());
        write_verified(&path, Publish::NoReplace, |_| Ok(()), |_| Ok(())).unwrap();
    }
}
