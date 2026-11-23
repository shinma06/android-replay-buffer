//! One-click Android device screenshot capture (`save.screenshot`) and atomic PNG persistence.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use arb_adb::adb::{AdbClient, AdbDeviceState};
use arb_core::analytics::validate_png_bytes;
use arb_os::fs::{fsync_dir, fsync_file};

use crate::error::DaemonError;
use crate::time_util::next_id;

/// Maximum PNG payload accepted from `adb exec-out screencap -p` (`64 MiB`).
const MAX_SCREENSHOT_BYTES: usize = 64 * 1024 * 1024;
/// Wall-clock timeout for `adb exec-out screencap -p`.
const SCREENCAP_TIMEOUT: Duration = Duration::from_secs(15);

/// Injectable provider of raw PNG screenshots for an Android device.
///
/// Production code uses [`AdbScreenshotProvider`] (`adb -s <serial> exec-out screencap -p`);
/// unit and E2E tests inject deterministic mock providers to verify atomic write, PNG
/// signature/IHDR validation, overwrite protection, and SHA-256 reporting without ADB.
pub trait ScreenshotProvider: fmt::Debug + Send + Sync {
    /// Captures a PNG screenshot for `serial` (auto-selecting the sole online device when `None`).
    ///
    /// Returns `(resolved_serial, raw_png_bytes)`.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if ADB/device selection or screenshot capture fails.
    fn capture_png(&self, serial: Option<&str>) -> Result<(String, Vec<u8>), DaemonError>;
}

/// Shared cloneable handle wrapping any [`ScreenshotProvider`].
#[derive(Debug, Clone)]
pub struct SharedScreenshotProvider(Arc<dyn ScreenshotProvider>);

impl SharedScreenshotProvider {
    /// Wraps a concrete [`ScreenshotProvider`] inside an `Arc` handle.
    #[must_use]
    pub fn new(provider: Arc<dyn ScreenshotProvider>) -> Self {
        Self(provider)
    }

    /// Creates the default production provider backed by `adb exec-out screencap -p`.
    #[must_use]
    pub fn adb_default() -> Self {
        Self(Arc::new(AdbScreenshotProvider))
    }

    /// Delegates to the underlying [`ScreenshotProvider::capture_png`].
    ///
    /// # Errors
    ///
    /// Propagates any error returned by the underlying provider.
    pub fn capture_png(&self, serial: Option<&str>) -> Result<(String, Vec<u8>), DaemonError> {
        self.0.capture_png(serial)
    }
}

/// Production [`ScreenshotProvider`] invoking `adb -s <serial> exec-out screencap -p`.
#[derive(Debug, Default, Clone, Copy)]
pub struct AdbScreenshotProvider;

impl ScreenshotProvider for AdbScreenshotProvider {
    fn capture_png(&self, serial: Option<&str>) -> Result<(String, Vec<u8>), DaemonError> {
        let adb = AdbClient::discover()?;
        let target_serial = resolve_screenshot_serial(&adb, serial)?;
        let bytes = run_adb_screencap(adb.adb_path(), &target_serial, SCREENCAP_TIMEOUT)?;
        let _dims = validate_png_bytes(&bytes)
            .map_err(|err| DaemonError::Internal(format!("invalid PNG from screencap: {err}")))?;
        Ok((target_serial, bytes))
    }
}

fn resolve_screenshot_serial(
    adb: &AdbClient,
    requested: Option<&str>,
) -> Result<String, DaemonError> {
    if let Some(s) = requested.filter(|s| !s.trim().is_empty()) {
        return Ok(s.to_owned());
    }
    let online: Vec<String> = adb
        .devices()?
        .into_iter()
        .filter(|d| d.state == AdbDeviceState::Device)
        .map(|d| d.serial)
        .collect();
    match online.as_slice() {
        [only] => Ok(only.clone()),
        [] => Err(DaemonError::NotFound(
            "no online ADB devices found for screenshot".to_owned(),
        )),
        many => Err(DaemonError::InvalidParams(format!(
            "multiple online ADB devices found ({}); pass serial explicitly",
            many.join(", ")
        ))),
    }
}

fn run_adb_screencap(
    adb_path: &Path,
    serial: &str,
    timeout: Duration,
) -> Result<Vec<u8>, DaemonError> {
    let mut cmd = Command::new(adb_path);
    cmd.args(["-s", serial, "exec-out", "screencap", "-p"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let stdout_handle = stdout_pipe.map(|pipe| {
        thread::spawn(move || -> io::Result<Vec<u8>> {
            let mut limited =
                pipe.take(u64::try_from(MAX_SCREENSHOT_BYTES + 1).unwrap_or(u64::MAX));
            let mut buf = Vec::new();
            limited.read_to_end(&mut buf)?;
            Ok(buf)
        })
    });
    let stderr_handle = stderr_pipe.map(|mut pipe| {
        thread::spawn(move || -> Vec<u8> {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(DaemonError::Internal(format!(
                "adb screencap timed out after {}s",
                timeout.as_secs()
            )));
        }
        thread::sleep(Duration::from_millis(20));
    };

    let stdout_bytes = match stdout_handle {
        Some(h) => h
            .join()
            .map_err(|_| DaemonError::Internal("screencap stdout reader panicked".to_owned()))??,
        None => Vec::new(),
    };
    let stderr_bytes = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();

    if !status.success() {
        let stderr_text = String::from_utf8_lossy(&stderr_bytes).trim().to_owned();
        return Err(DaemonError::Internal(format!(
            "adb screencap failed ({status}): {stderr_text}"
        )));
    }
    if stdout_bytes.len() > MAX_SCREENSHOT_BYTES {
        return Err(DaemonError::Internal(format!(
            "adb screencap exceeded {MAX_SCREENSHOT_BYTES}-byte limit"
        )));
    }
    Ok(stdout_bytes)
}

/// Atomic overwrite policy for [`write_screenshot_atomic`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenshotOverwrite {
    /// Fail with [`DaemonError::Conflict`] if the destination file already exists.
    NoReplace,
    /// Atomically replace an existing destination file.
    Replace,
}

/// Validates PNG framing and writes `png_bytes` atomically to `dest_path`
/// (temp file + `fsync_file` + atomic publish + `fsync_dir`).
///
/// Returns `(width, height)` parsed from the validated PNG `IHDR` header.
///
/// # Errors
///
/// Returns [`DaemonError::InvalidParams`] if `png_bytes` fails PNG validation or `dest_path`
/// has no parent directory, [`DaemonError::Conflict`] when `overwrite == ScreenshotOverwrite::NoReplace`
/// and `dest_path` already exists, or [`DaemonError::Io`] / [`DaemonError::Os`] on write failure.
pub fn write_screenshot_atomic(
    dest_path: &Path,
    png_bytes: &[u8],
    overwrite: ScreenshotOverwrite,
) -> Result<(u32, u32), DaemonError> {
    let dims = validate_png_bytes(png_bytes)
        .map_err(|err| DaemonError::InvalidParams(format!("invalid PNG payload: {err}")))?;

    let parent = dest_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            DaemonError::InvalidParams(format!(
                "screenshot destination `{}` must have a parent directory",
                dest_path.display()
            ))
        })?;
    fs::create_dir_all(parent)?;

    if overwrite == ScreenshotOverwrite::NoReplace && path_exists(dest_path)? {
        return Err(DaemonError::Conflict(format!(
            "screenshot destination `{}` already exists; pass overwrite=true to replace",
            dest_path.display()
        )));
    }

    let tmp_name = format!(".tmp-{}.png", next_id("shot"));
    let tmp_path: PathBuf = parent.join(tmp_name);

    let result = (|| -> Result<(), DaemonError> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;
        file.write_all(png_bytes)?;
        fsync_file(&file)?;
        drop(file);

        match overwrite {
            ScreenshotOverwrite::Replace => {
                fs::rename(&tmp_path, dest_path)?;
            }
            ScreenshotOverwrite::NoReplace => {
                publish_no_replace(&tmp_path, dest_path)?;
            }
        }
        fsync_dir(parent)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result?;
    Ok(dims)
}

fn publish_no_replace(tmp_path: &Path, dest_path: &Path) -> Result<(), DaemonError> {
    match fs::hard_link(tmp_path, dest_path) {
        Ok(()) => {
            let _ = fs::remove_file(tmp_path);
            Ok(())
        }
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            Err(DaemonError::Conflict(format!(
                "screenshot destination `{}` already exists; pass overwrite=true to replace",
                dest_path.display()
            )))
        }
        // Some filesystems (e.g. FAT32/exFAT) do not support hard links. Fall back to an
        // exclusive `create_new` reservation of the destination (atomic: it fails if anything,
        // including a symlink, already exists) and only then rename the finished temp file over
        // our own placeholder, so a file created concurrently is never overwritten.
        Err(_) => {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dest_path)
            {
                Ok(placeholder) => drop(placeholder),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    return Err(DaemonError::Conflict(format!(
                        "screenshot destination `{}` already exists; pass overwrite=true to replace",
                        dest_path.display()
                    )));
                }
                Err(err) => return Err(err.into()),
            }
            if let Err(err) = fs::rename(tmp_path, dest_path) {
                let _ = fs::remove_file(dest_path);
                return Err(err.into());
            }
            Ok(())
        }
    }
}

fn path_exists(path: &Path) -> Result<bool, DaemonError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use arb_core::analytics::PNG_SIGNATURE;
    use tempfile::TempDir;

    use super::{ScreenshotOverwrite, write_screenshot_atomic};
    use crate::error::DaemonError;

    fn minimal_png(width: u32, height: u32) -> Vec<u8> {
        let mut png = Vec::new();
        png.extend_from_slice(&PNG_SIGNATURE);
        png.extend_from_slice(&13_u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&width.to_be_bytes());
        png.extend_from_slice(&height.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        png.extend_from_slice(&0_u32.to_be_bytes());
        png.extend_from_slice(&0_u32.to_be_bytes());
        png.extend_from_slice(b"IEND");
        png.extend_from_slice(&0_u32.to_be_bytes());
        png
    }

    #[test]
    fn atomic_screenshot_write_validates_png_and_protects_existing_files() {
        let tmp = TempDir::new().expect("tempdir");
        let dest = tmp.path().join("screen.png");
        let png1 = minimal_png(1080, 1920);
        let png2 = minimal_png(720, 1280);

        let dims = write_screenshot_atomic(&dest, &png1, ScreenshotOverwrite::NoReplace)
            .expect("initial write must succeed");
        assert_eq!(dims, (1080, 1920), "parsed PNG dimensions");
        assert_eq!(
            std::fs::read(&dest).expect("read written png"),
            png1,
            "file bytes must match"
        );

        // NoReplace must reject overwrite and preserve original bytes.
        let err = write_screenshot_atomic(&dest, &png2, ScreenshotOverwrite::NoReplace)
            .expect_err("must refuse overwrite by default");
        assert!(
            matches!(err, DaemonError::Conflict(_)),
            "expected Conflict, got {err:?}"
        );
        assert_eq!(
            std::fs::read(&dest).expect("original intact"),
            png1,
            "existing file must stay untouched"
        );

        // Explicit Replace must atomically overwrite.
        let dims2 = write_screenshot_atomic(&dest, &png2, ScreenshotOverwrite::Replace)
            .expect("explicit overwrite must succeed");
        assert_eq!(dims2, (720, 1280), "updated PNG dimensions");
        assert_eq!(
            std::fs::read(&dest).expect("read replaced png"),
            png2,
            "replaced file bytes must match"
        );

        // Invalid PNG must be rejected before creating target file.
        let bad_dest = tmp.path().join("bad.png");
        let bad_err =
            write_screenshot_atomic(&bad_dest, b"not a png", ScreenshotOverwrite::NoReplace)
                .expect_err("invalid PNG must fail");
        assert!(
            matches!(bad_err, DaemonError::InvalidParams(_)),
            "expected InvalidParams, got {bad_err:?}"
        );
        assert!(
            !bad_dest.exists(),
            "invalid PNG must not create target file"
        );
    }
}
