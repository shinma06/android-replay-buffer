//! Atomic staging/commit filesystem helpers, disk-space queries, single-instance
//! process locks, and PID liveness checks.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::thread;
use std::time::Duration;

use crate::error::OsError;

/// Marker file written and `fsync`ed inside a staging directory immediately
/// before it is atomically renamed to its final destination.
pub const COMMIT_MARKER_FILE: &str = ".committed";

/// Exponential backoff delays (ms) used when retrying transient rename lock
/// errors on Windows (e.g., Defender or Explorer briefly opening staging files).
const RENAME_RETRY_DELAYS_MS: [u64; 5] = [10, 20, 40, 80, 160];

/// Available and total byte capacity of the filesystem containing a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DiskSpaceInfo {
    /// Bytes available to the current unprivileged user.
    pub available_bytes: u64,
    /// Total capacity of the filesystem in bytes.
    pub total_bytes: u64,
}

/// Queries available and total bytes on the filesystem hosting `path`.
///
/// # Errors
///
/// Returns [`OsError::Fs`] if the path does not exist or the OS filesystem
/// capacity query fails, or [`OsError::InvalidArgument`] if `path` contains an
/// interior NUL byte on Unix.
pub fn query_disk_space(path: &Path) -> Result<DiskSpaceInfo, OsError> {
    imp::query_disk_space(path)
}

/// Flushes all file data and metadata to durable storage.
///
/// # Errors
///
/// Returns [`OsError::Io`] if `File::sync_all` fails.
pub fn fsync_file(file: &File) -> Result<(), OsError> {
    file.sync_all().map_err(|source| OsError::Io {
        operation: "fsync_file",
        source,
    })
}

/// Flushes directory entries to durable storage on POSIX platforms and validates
/// directory existence cleanly on Windows.
///
/// # Errors
///
/// Returns [`OsError::Fs`] if `dir` cannot be opened or synced.
pub fn fsync_dir(dir: &Path) -> Result<(), OsError> {
    imp::fsync_dir(dir)
}

/// Checks whether a host OS process with the given PID is currently alive.
///
/// Returns `false` for PID `0` (reserved by OS kernels and unsafe to probe with
/// `kill(0, 0)` on POSIX).
#[must_use]
pub fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    imp::is_pid_alive(pid)
}

/// RAII staging directory that commits atomically into a final path without
/// ever overwriting an existing destination.
///
/// If dropped before [`AtomicStagingDir::commit`] succeeds, the staging
/// directory is removed on a best-effort basis so failed saves never leave
/// half-written final bundles behind.
#[derive(Debug)]
pub struct AtomicStagingDir {
    staging_path: PathBuf,
    final_path: PathBuf,
    committed: bool,
}

impl AtomicStagingDir {
    /// Creates a new `.staging-<staging_id>` directory inside the parent of
    /// `final_path`.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::InvalidArgument`] if `staging_id` is empty or contains
    /// path separators / traversal segments, or if `final_path` has no parent
    /// directory. Returns [`OsError::AlreadyExists`] if `final_path` already
    /// exists, or [`OsError::Fs`] if directory creation fails.
    pub fn create(final_path: impl AsRef<Path>, staging_id: &str) -> Result<Self, OsError> {
        validate_staging_id(staging_id)?;
        let final_path = final_path.as_ref().to_path_buf();
        let parent = final_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| OsError::InvalidArgument {
                name: "final_path",
                reason: format!(
                    "final path `{}` must have a parent directory",
                    final_path.display()
                ),
            })?;

        fs::create_dir_all(parent).map_err(|source| OsError::Fs {
            operation: "create_parent_dir",
            path: parent.to_path_buf(),
            source,
        })?;

        if path_exists(&final_path)? {
            return Err(OsError::AlreadyExists { path: final_path });
        }

        let staging_path = parent.join(format!(".staging-{staging_id}"));
        fs::create_dir(&staging_path).map_err(|source| OsError::Fs {
            operation: "create_staging_dir",
            path: staging_path.clone(),
            source,
        })?;

        Ok(Self {
            staging_path,
            final_path,
            committed: false,
        })
    }

    /// Returns the temporary `.staging-<id>` path on disk.
    #[must_use]
    pub fn staging_path(&self) -> &Path {
        &self.staging_path
    }

    /// Returns the intended final destination path.
    #[must_use]
    pub fn final_path(&self) -> &Path {
        &self.final_path
    }

    /// Writes `contents` to `rel_path` inside the staging directory and calls
    /// [`fsync_file`] before returning its staged path.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::InvalidArgument`] if `rel_path` escapes the staging
    /// directory, or [`OsError::Fs`] / [`OsError::Io`] on write or sync failure.
    pub fn write_file_synced(
        &self,
        rel_path: impl AsRef<Path>,
        contents: &[u8],
    ) -> Result<PathBuf, OsError> {
        let target = self.resolve_relative(rel_path.as_ref())?;
        let mut file = create_staged_file(&target)?;
        file.write_all(contents).map_err(|source| OsError::Fs {
            operation: "write_staged_file",
            path: target.clone(),
            source,
        })?;
        fsync_file(&file)?;
        Ok(target)
    }

    /// Creates a writable [`File`] at `rel_path` inside the staging directory
    /// for streaming large outputs (such as `video.mp4` or `timeline.sqlite`).
    ///
    /// Callers should pass the handle to [`fsync_file`] once writing completes.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::InvalidArgument`] if `rel_path` escapes the staging
    /// directory, or [`OsError::Fs`] if creating parent folders or the file fails.
    pub fn create_file_writer(&self, rel_path: impl AsRef<Path>) -> Result<File, OsError> {
        let target = self.resolve_relative(rel_path.as_ref())?;
        create_staged_file(&target)
    }

    /// Flushes the staging directory's directory entries to durable storage.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::Fs`] if [`fsync_dir`] fails.
    pub fn fsync_staging_dir(&self) -> Result<(), OsError> {
        fsync_dir(&self.staging_path)
    }

    /// Atomically commits the staging directory to [`AtomicStagingDir::final_path`].
    ///
    /// Writes and `fsync`s the `.committed` marker file, `fsync`s the staging
    /// directory, renames it to the final destination without ever overwriting
    /// an existing path (retrying transient lock errors on Windows), marks the
    /// staging directory committed immediately after rename, and `fsync`s the
    /// parent directory.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::AlreadyExists`] if the destination already exists,
    /// [`OsError::CommittedParentSyncFailed`] if the bundle was published to
    /// `final_path` successfully but syncing the parent directory failed
    /// afterwards (callers must treat the bundle as published), or
    /// [`OsError::Fs`] / [`OsError::Io`] if writing the marker, staging sync,
    /// or renaming fails. On pre-publish errors, the staging directory is
    /// cleaned up when `self` is dropped.
    pub fn commit(mut self) -> Result<PathBuf, OsError> {
        self.commit_with_parent_sync(fsync_dir)
    }

    fn commit_with_parent_sync<F>(&mut self, sync_parent: F) -> Result<PathBuf, OsError>
    where
        F: FnOnce(&Path) -> Result<(), OsError>,
    {
        self.write_file_synced(COMMIT_MARKER_FILE, b"committed\n")?;
        fsync_dir(&self.staging_path)?;

        if path_exists(&self.final_path)? {
            return Err(OsError::AlreadyExists {
                path: self.final_path.clone(),
            });
        }

        let staging_path = &self.staging_path;
        let final_path = &self.final_path;

        retry_transient_rename(
            || imp::rename_no_replace(staging_path, final_path),
            imp::is_transient_rename_error,
            || !staging_path.exists() && final_path.join(COMMIT_MARKER_FILE).is_file(),
            &RENAME_RETRY_DELAYS_MS,
            thread::sleep,
        )
        .map_err(|err| map_rename_error(err, final_path))?;

        // Mark committed immediately once the rename publishes the WORM bundle
        // so Drop never treats an already-published bundle as uncommitted if
        // syncing the parent directory fails.
        self.committed = true;

        if let Some(parent) = self.final_path.parent()
            && let Err(err) = sync_parent(parent)
        {
            let source = match err {
                OsError::Fs { source, .. } | OsError::Io { source, .. } => source,
                other => io::Error::other(other.to_string()),
            };
            return Err(OsError::CommittedParentSyncFailed {
                path: self.final_path.clone(),
                source,
            });
        }

        Ok(self.final_path.clone())
    }

    fn resolve_relative(&self, rel_path: &Path) -> Result<PathBuf, OsError> {
        if rel_path.as_os_str().is_empty() {
            return Err(OsError::InvalidArgument {
                name: "rel_path",
                reason: "relative staging path must not be empty".to_owned(),
            });
        }
        for component in rel_path.components() {
            match component {
                Component::Normal(_) | Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(OsError::InvalidArgument {
                        name: "rel_path",
                        reason: format!(
                            "relative staging path `{}` must stay inside staging directory",
                            rel_path.display()
                        ),
                    });
                }
            }
        }
        Ok(self.staging_path.join(rel_path))
    }
}

impl Drop for AtomicStagingDir {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_dir_all(&self.staging_path);
        }
    }
}

/// Exclusive non-blocking advisory lock guaranteeing a single daemon instance
/// per lock file path.
///
/// The lock is released automatically when dropped or when the owning process
/// exits.
#[derive(Debug)]
pub struct SingleInstanceLock {
    file: File,
    path: PathBuf,
}

impl SingleInstanceLock {
    /// Attempts to acquire an exclusive, non-blocking lock on `lock_path`.
    ///
    /// On success, writes the current process PID into the lock file for
    /// diagnostics.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::LockContended`] if another process already holds the
    /// lock, or [`OsError::Fs`] if opening or locking the file fails.
    pub fn try_acquire(lock_path: impl AsRef<Path>) -> Result<Self, OsError> {
        let path = lock_path.as_ref().to_path_buf();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|source| OsError::Fs {
                operation: "create_lock_parent_dir",
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| OsError::Fs {
                operation: "open_lock_file",
                path: path.clone(),
                source,
            })?;

        imp::try_lock_exclusive(&file, &path)?;

        // Write current PID after acquiring the exclusive lock so we never
        // truncate a lock file owned by an active daemon.
        let pid_line = format!("{}\n", std::process::id());
        let _ = file.set_len(0);
        let _ = file.seek(SeekFrom::Start(0));
        let _ = file.write_all(pid_line.as_bytes());
        let _ = file.flush();

        Ok(Self { file, path })
    }

    /// Returns the filesystem path of the acquired lock file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SingleInstanceLock {
    fn drop(&mut self) {
        imp::unlock_best_effort(&self.file);
    }
}

fn validate_staging_id(staging_id: &str) -> Result<(), OsError> {
    if staging_id.is_empty()
        || staging_id.contains('/')
        || staging_id.contains('\\')
        || staging_id.contains("..")
        || staging_id.contains('\0')
    {
        return Err(OsError::InvalidArgument {
            name: "staging_id",
            reason: format!("staging_id `{staging_id}` must be a simple non-empty path segment"),
        });
    }
    Ok(())
}

fn path_exists(path: &Path) -> Result<bool, OsError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(OsError::Fs {
            operation: "stat_target_path",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn create_staged_file(target: &Path) -> Result<File, OsError> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|source| OsError::Fs {
            operation: "create_staged_subdir",
            path: parent.to_path_buf(),
            source,
        })?;
    }
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(target)
        .map_err(|source| OsError::Fs {
            operation: "create_staged_file",
            path: target.to_path_buf(),
            source,
        })
}

fn map_rename_error(source: io::Error, final_path: &Path) -> OsError {
    if source.kind() == io::ErrorKind::AlreadyExists {
        return OsError::AlreadyExists {
            path: final_path.to_path_buf(),
        };
    }
    OsError::Fs {
        operation: "commit_rename",
        path: final_path.to_path_buf(),
        source,
    }
}

fn retry_transient_rename<RenameFn, IsTransientFn, IsCommittedFn, SleepFn>(
    mut rename_op: RenameFn,
    is_transient: IsTransientFn,
    is_already_committed: IsCommittedFn,
    backoff_ms: &[u64],
    mut sleep_fn: SleepFn,
) -> io::Result<()>
where
    RenameFn: FnMut() -> io::Result<()>,
    IsTransientFn: Fn(&io::Error) -> bool,
    IsCommittedFn: Fn() -> bool,
    SleepFn: FnMut(Duration),
{
    let mut attempt = 0_usize;
    loop {
        match rename_op() {
            Ok(()) => return Ok(()),
            Err(err) => {
                if is_already_committed() {
                    return Ok(());
                }
                if !is_transient(&err) || attempt >= backoff_ms.len() {
                    return Err(err);
                }
                sleep_fn(Duration::from_millis(backoff_ms[attempt]));
                attempt += 1;
            }
        }
    }
}

#[cfg(unix)]
mod imp {
    use std::ffi::CString;
    use std::fs::{self, File};
    use std::io;
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::AsRawFd;
    use std::path::Path;

    use crate::error::OsError;
    use crate::fs::DiskSpaceInfo;

    fn path_to_cstring(path: &Path) -> Result<CString, OsError> {
        CString::new(path.as_os_str().as_bytes()).map_err(|_| OsError::InvalidArgument {
            name: "path",
            reason: format!("path `{}` contains an interior NUL byte", path.display()),
        })
    }

    fn stat_field_to_u64<T: TryInto<u64>>(value: T) -> u64 {
        value.try_into().unwrap_or(0)
    }

    pub(super) fn query_disk_space(path: &Path) -> Result<DiskSpaceInfo, OsError> {
        let c_path = path_to_cstring(path)?;
        let mut stat = MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: `c_path` is a valid NUL-terminated C string and `stat.as_mut_ptr()`
        // points to writable stack storage for `libc::statvfs`.
        let rc = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
        if rc != 0 {
            return Err(OsError::Fs {
                operation: "statvfs",
                path: path.to_path_buf(),
                source: io::Error::last_os_error(),
            });
        }
        // SAFETY: `statvfs` returned 0, so `stat` is fully initialized.
        let stat = unsafe { stat.assume_init() };
        let frsize = stat_field_to_u64(stat.f_frsize);
        let bsize = stat_field_to_u64(stat.f_bsize);
        let unit = if frsize > 0 { frsize } else { bsize };
        let available_bytes = stat_field_to_u64(stat.f_bavail).saturating_mul(unit);
        let total_bytes = stat_field_to_u64(stat.f_blocks).saturating_mul(unit);
        Ok(DiskSpaceInfo {
            available_bytes,
            total_bytes,
        })
    }

    pub(super) fn fsync_dir(dir: &Path) -> Result<(), OsError> {
        let dir_file = File::open(dir).map_err(|source| OsError::Fs {
            operation: "open_dir_for_fsync",
            path: dir.to_path_buf(),
            source,
        })?;
        dir_file.sync_all().map_err(|source| OsError::Fs {
            operation: "fsync_dir",
            path: dir.to_path_buf(),
            source,
        })
    }

    #[cfg(target_os = "macos")]
    pub(super) fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
        let c_from = CString::new(from.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "interior NUL in source"))?;
        let c_to = CString::new(to.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "interior NUL in target"))?;
        // SAFETY: Both `c_from` and `c_to` are valid NUL-terminated C strings.
        let rc = unsafe { libc::renamex_np(c_from.as_ptr(), c_to.as_ptr(), libc::RENAME_EXCL) };
        if rc == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ENOTSUP) {
            return fallback_rename_no_replace(from, to);
        }
        if err.raw_os_error() == Some(libc::EEXIST) || err.raw_os_error() == Some(libc::ENOTEMPTY) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, err));
        }
        Err(err)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
        let c_from = CString::new(from.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "interior NUL in source"))?;
        let c_to = CString::new(to.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "interior NUL in target"))?;
        // SAFETY: `c_from` and `c_to` are valid NUL-terminated C strings, and
        // `AT_FDCWD` instructs `renameat2` to resolve relative paths from cwd.
        let rc = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                c_from.as_ptr(),
                libc::AT_FDCWD,
                c_to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if rc == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if matches!(err.raw_os_error(), Some(libc::ENOSYS | libc::EINVAL)) {
            return fallback_rename_no_replace(from, to);
        }
        if matches!(err.raw_os_error(), Some(libc::EEXIST | libc::ENOTEMPTY)) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, err));
        }
        Err(err)
    }

    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "linux")))]
    pub(super) fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
        fallback_rename_no_replace(from, to)
    }

    fn fallback_rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
        match fs::symlink_metadata(to) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "destination path already exists",
                ));
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        fs::rename(from, to)
    }

    pub(super) const fn is_transient_rename_error(_err: &io::Error) -> bool {
        false
    }

    pub(super) fn try_lock_exclusive(file: &File, path: &Path) -> Result<(), OsError> {
        let fd = file.as_raw_fd();
        // SAFETY: `fd` is an open file descriptor owned by `file` for the
        // duration of this call; `LOCK_EX | LOCK_NB` requests a non-blocking
        // exclusive advisory lock.
        let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) || err.raw_os_error() == Some(libc::EAGAIN)
        {
            return Err(OsError::LockContended {
                path: path.to_path_buf(),
            });
        }
        Err(OsError::Fs {
            operation: "flock",
            path: path.to_path_buf(),
            source: err,
        })
    }

    pub(super) fn unlock_best_effort(file: &File) {
        let fd = file.as_raw_fd();
        // SAFETY: `fd` is still open while `SingleInstanceLock::drop` runs
        // before `self.file` is dropped.
        let _ = unsafe { libc::flock(fd, libc::LOCK_UN) };
    }

    pub(super) fn is_pid_alive(pid: u32) -> bool {
        let Ok(raw_pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        if raw_pid <= 0 {
            return false;
        }
        // SAFETY: `kill(raw_pid, 0)` sends no signal; it only validates PID
        // existence and signaling permissions for a positive PID.
        let rc = unsafe { libc::kill(raw_pid, 0) };
        if rc == 0 {
            return true;
        }
        matches!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM))
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use std::ffi::OsStr;
    use std::fs::{self, File};
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS,
        ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION, GetLastError, STILL_ACTIVE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
        MoveFileExW, UnlockFileEx,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    use crate::error::OsError;
    use crate::fs::DiskSpaceInfo;

    fn encode_wide(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }

    pub(super) fn query_disk_space(path: &Path) -> Result<DiskSpaceInfo, OsError> {
        let wide = encode_wide(path.as_os_str());
        let mut available_bytes: u64 = 0;
        let mut total_bytes: u64 = 0;
        // SAFETY: `wide` is NUL-terminated UTF-16 and the out-pointers refer to
        // valid stack `u64` variables.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &raw mut available_bytes,
                &raw mut total_bytes,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(OsError::Fs {
                operation: "GetDiskFreeSpaceExW",
                path: path.to_path_buf(),
                source: io::Error::last_os_error(),
            });
        }
        Ok(DiskSpaceInfo {
            available_bytes,
            total_bytes,
        })
    }

    pub(super) fn fsync_dir(dir: &Path) -> Result<(), OsError> {
        let meta = fs::metadata(dir).map_err(|source| OsError::Fs {
            operation: "stat_dir_for_fsync",
            path: dir.to_path_buf(),
            source,
        })?;
        if !meta.is_dir() {
            return Err(OsError::InvalidArgument {
                name: "dir",
                reason: format!("`{}` is not a directory", dir.display()),
            });
        }
        Ok(())
    }

    pub(super) fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
        let wide_from = encode_wide(from.as_os_str());
        let wide_to = encode_wide(to.as_os_str());
        // SAFETY: `wide_from` and `wide_to` are valid NUL-terminated UTF-16
        // paths; flags=0 explicitly omits `MOVEFILE_REPLACE_EXISTING` so
        // Windows atomically refuses to overwrite an existing destination.
        let ok = unsafe { MoveFileExW(wide_from.as_ptr(), wide_to.as_ptr(), 0) };
        if ok != 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        // SAFETY: `GetLastError` reads the calling thread's last-error slot.
        let code = unsafe { GetLastError() };
        if code == ERROR_ALREADY_EXISTS || code == ERROR_FILE_EXISTS {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, err));
        }
        // On Windows, renaming a directory onto an existing destination
        // directory without MOVEFILE_REPLACE_EXISTING often reports
        // ERROR_ACCESS_DENIED; disambiguate before transient retry treats it as
        // a lock conflict.
        if code == ERROR_ACCESS_DENIED && fs::symlink_metadata(to).is_ok() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, err));
        }
        Err(err)
    }

    pub(super) fn is_transient_rename_error(err: &io::Error) -> bool {
        if err.kind() == io::ErrorKind::AlreadyExists {
            return false;
        }
        let Some(raw) = err.raw_os_error() else {
            return false;
        };
        let Ok(code) = u32::try_from(raw) else {
            return false;
        };
        code == ERROR_ACCESS_DENIED || code == ERROR_SHARING_VIOLATION
    }

    pub(super) fn try_lock_exclusive(file: &File, path: &Path) -> Result<(), OsError> {
        let handle = file.as_raw_handle();
        // SAFETY: `OVERLAPPED` is a plain C POD struct where all-zero bits
        // represent offset 0 and no event handle.
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        // SAFETY: `handle` is an open file handle owned by `file` and
        // `overlapped` is valid for the synchronous non-blocking lock request.
        let ok = unsafe {
            LockFileEx(
                handle,
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &raw mut overlapped,
            )
        };
        if ok != 0 {
            return Ok(());
        }
        // SAFETY: `GetLastError` reads the calling thread's last-error slot.
        let code = unsafe { GetLastError() };
        if code == ERROR_LOCK_VIOLATION {
            return Err(OsError::LockContended {
                path: path.to_path_buf(),
            });
        }
        Err(OsError::Fs {
            operation: "LockFileEx",
            path: path.to_path_buf(),
            source: io::Error::last_os_error(),
        })
    }

    pub(super) fn unlock_best_effort(file: &File) {
        let handle = file.as_raw_handle();
        // SAFETY: `OVERLAPPED` is a plain C POD struct where all-zero bits
        // represent offset 0.
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        // SAFETY: `handle` is still open while `SingleInstanceLock::drop` runs.
        let _ = unsafe { UnlockFileEx(handle, 0, 1, 0, &raw mut overlapped) };
    }

    pub(super) fn is_pid_alive(pid: u32) -> bool {
        // SAFETY: `OpenProcess` is thread-safe and returns null on failure.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            // SAFETY: `GetLastError` reads the calling thread's last-error slot.
            let code = unsafe { GetLastError() };
            return code == ERROR_ACCESS_DENIED;
        }
        let mut exit_code: u32 = 0;
        // SAFETY: `handle` is a valid non-null process handle and `exit_code`
        // points to writable stack storage.
        let ok = unsafe { GetExitCodeProcess(handle, &raw mut exit_code) };
        // SAFETY: `handle` was returned by `OpenProcess` and is closed exactly once.
        let _ = unsafe { CloseHandle(handle) };
        let still_active = u32::try_from(STILL_ACTIVE).unwrap_or(259);
        ok != 0 && exit_code == still_active
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use super::{
        AtomicStagingDir, COMMIT_MARKER_FILE, SingleInstanceLock, fsync_dir, fsync_file,
        is_pid_alive, query_disk_space, retry_transient_rename,
    };
    use crate::error::OsError;

    static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

    struct TestTempDir {
        path: PathBuf,
    }

    impl TestTempDir {
        fn new(label: &str) -> Self {
            let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "arb-os-fs-test-{label}-{}-{seq}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create test temp dir");
            Self { path }
        }
    }

    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn atomic_staging_commit_moves_files_and_writes_marker() {
        let tmp = TestTempDir::new("commit");
        let final_path = tmp.path.join("bundle-001");

        let staging = AtomicStagingDir::create(&final_path, "job-001")
            .expect("create staging dir should succeed");
        let staging_path = staging.staging_path().to_path_buf();
        assert!(
            staging_path.is_dir(),
            "staging dir must exist on disk: {}",
            staging_path.display()
        );
        assert!(
            !final_path.exists(),
            "final path must not exist before commit"
        );

        staging
            .write_file_synced("manifest.json", br#"{"schema_version":2}"#)
            .expect("write manifest into staging");
        let mut video = staging
            .create_file_writer("media/video.mp4")
            .expect("create nested video file in staging");
        std::io::Write::write_all(&mut video, b"ftypisom").expect("write staged video bytes");
        fsync_file(&video).expect("fsync staged video file");
        drop(video);
        staging.fsync_staging_dir().expect("fsync staging dir");

        let committed_path = staging.commit().expect("atomic commit must succeed");
        assert_eq!(
            committed_path, final_path,
            "committed path should match requested final path"
        );
        assert!(
            !staging_path.exists(),
            "staging path must disappear after commit"
        );
        assert!(
            final_path.join(COMMIT_MARKER_FILE).is_file(),
            "commit marker must exist in committed directory"
        );
        assert_eq!(
            fs::read(final_path.join("manifest.json")).expect("read committed manifest"),
            br#"{"schema_version":2}"#,
            "manifest contents must survive atomic commit intact"
        );
        assert_eq!(
            fs::read(final_path.join("media/video.mp4")).expect("read committed video"),
            b"ftypisom",
            "nested media file must survive atomic commit intact"
        );
        fsync_dir(&tmp.path).expect("fsync parent directory should succeed");
    }

    #[test]
    fn uncommitted_staging_dir_is_cleaned_up_on_drop() {
        let tmp = TestTempDir::new("drop-cleanup");
        let final_path = tmp.path.join("bundle-aborted");

        let staging_path = {
            let staging = AtomicStagingDir::create(&final_path, "job-abort")
                .expect("create staging dir should succeed");
            staging
                .write_file_synced("partial.bin", b"incomplete")
                .expect("write partial file");
            let p = staging.staging_path().to_path_buf();
            assert!(p.is_dir(), "staging dir should exist while live");
            p
        };

        assert!(
            !staging_path.exists(),
            "uncommitted staging dir must be removed on drop"
        );
        assert!(
            !final_path.exists(),
            "final path must not be created for aborted staging dir"
        );
    }

    #[test]
    fn atomic_staging_never_overwrites_existing_destination() {
        let tmp = TestTempDir::new("no-overwrite");
        let final_path = tmp.path.join("bundle-existing");

        // Case 1: destination already exists at create time.
        fs::create_dir(&final_path).expect("create existing empty destination dir");
        let create_err = AtomicStagingDir::create(&final_path, "job-dup")
            .expect_err("create must reject an already-existing final path");
        assert!(
            matches!(create_err, OsError::AlreadyExists { .. }),
            "expected AlreadyExists error, got {create_err:?}"
        );

        // Case 2: destination appears concurrently AFTER staging creation (even if empty!).
        let race_path = tmp.path.join("bundle-race");
        let staging =
            AtomicStagingDir::create(&race_path, "job-race").expect("create staging before race");
        staging
            .write_file_synced("manifest.json", b"new-data")
            .expect("write staged file");
        let staging_path = staging.staging_path().to_path_buf();

        fs::create_dir(&race_path).expect("simulate concurrent empty dir at destination");
        let commit_err = staging
            .commit()
            .expect_err("commit must never overwrite an existing destination");
        assert!(
            matches!(commit_err, OsError::AlreadyExists { .. }),
            "expected AlreadyExists on race commit, got {commit_err:?}"
        );
        assert!(
            !staging_path.exists(),
            "failed commit must still clean up staging directory on drop"
        );
        assert!(
            !race_path.join("manifest.json").exists(),
            "existing destination must remain untouched"
        );
    }

    #[test]
    fn staging_rejects_path_traversal_and_invalid_ids() {
        let tmp = TestTempDir::new("validation");
        let final_path = tmp.path.join("bundle-valid");

        assert!(
            AtomicStagingDir::create(&final_path, "../escape").is_err(),
            "staging_id with traversal must be rejected"
        );
        assert!(
            AtomicStagingDir::create(&final_path, "").is_err(),
            "empty staging_id must be rejected"
        );

        let staging = AtomicStagingDir::create(&final_path, "job-ok").expect("create staging");
        assert!(
            staging.write_file_synced("../escape.txt", b"bad").is_err(),
            "parent traversal relative path must be rejected"
        );
        assert!(
            staging.write_file_synced("/abs.txt", b"bad").is_err(),
            "absolute path must be rejected"
        );
    }

    #[test]
    fn retry_helper_handles_transient_errors_and_commit_marker_recovery() {
        let mut attempts = 0_u32;
        let mut sleeps = Vec::new();
        let res = retry_transient_rename(
            || {
                attempts += 1;
                if attempts < 3 {
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "transient lock",
                    ))
                } else {
                    Ok(())
                }
            },
            |err| err.kind() == io::ErrorKind::PermissionDenied,
            || false,
            &[10, 20, 40],
            |d| sleeps.push(d),
        );
        assert!(res.is_ok(), "rename should succeed on 3rd attempt: {res:?}");
        assert_eq!(attempts, 3, "expected exactly 3 attempts");
        assert_eq!(
            sleeps,
            vec![Duration::from_millis(10), Duration::from_millis(20)],
            "expected exponential backoff sleeps"
        );

        // If the rename actually succeeded on disk before returning an error,
        // the commit-marker check recovers cleanly without failing.
        let recovered = retry_transient_rename(
            || Err(io::Error::new(io::ErrorKind::NotFound, "staging moved")),
            |_| false,
            || true,
            &[10],
            |_| {},
        );
        assert!(
            recovered.is_ok(),
            "commit-marker recovery should succeed when final dir is committed"
        );
    }

    #[test]
    fn query_disk_space_returns_nonzero_capacity_for_temp_dir() {
        let info = query_disk_space(&std::env::temp_dir()).expect("disk space query must succeed");
        assert!(
            info.total_bytes > 0,
            "total_bytes must be positive, got {}",
            info.total_bytes
        );
        assert!(
            info.available_bytes <= info.total_bytes,
            "available_bytes ({}) must not exceed total_bytes ({})",
            info.available_bytes,
            info.total_bytes
        );
    }

    #[test]
    fn single_instance_lock_detects_contention_and_releases_on_drop() {
        let tmp = TestTempDir::new("lock");
        let lock_path = tmp.path.join("arbd.lock");

        let lock1 =
            SingleInstanceLock::try_acquire(&lock_path).expect("first lock acquire must succeed");
        assert_eq!(
            lock1.path(),
            lock_path.as_path(),
            "lock path accessor should match"
        );

        let contended = SingleInstanceLock::try_acquire(&lock_path)
            .expect_err("second concurrent lock acquire must fail with contention");
        assert!(
            matches!(contended, OsError::LockContended { .. }),
            "expected LockContended error, got {contended:?}"
        );

        let lock_contents = fs::read_to_string(&lock_path).expect("read PID from lock file");
        assert_eq!(
            lock_contents.trim(),
            std::process::id().to_string(),
            "lock file should record owning PID"
        );

        drop(lock1);

        let lock2 = SingleInstanceLock::try_acquire(&lock_path)
            .expect("lock acquire must succeed after previous holder drops");
        drop(lock2);
    }

    #[test]
    fn pid_liveness_distinguishes_current_process_from_exited_child() {
        assert!(
            is_pid_alive(std::process::id()),
            "current process PID must report alive"
        );
        assert!(!is_pid_alive(0), "PID 0 must never report alive");

        #[cfg(unix)]
        let mut child = Command::new("/usr/bin/true")
            .spawn()
            .expect("spawn short-lived unix child");
        #[cfg(windows)]
        let mut child = Command::new("cmd")
            .args(["/C", "exit", "0"])
            .spawn()
            .expect("spawn short-lived windows child");

        let child_pid = child.id();
        let status = child.wait().expect("reap short-lived child");
        assert!(status.success(), "child should exit cleanly");
        assert!(
            !is_pid_alive(child_pid),
            "reaped child PID {child_pid} must report dead"
        );
    }

    #[test]
    fn commit_marks_committed_before_parent_fsync_and_preserves_published_bundle() {
        let tmp = TestTempDir::new("parent-fsync-fail");
        let final_path = tmp.path.join("bundle-published");

        let mut staging =
            AtomicStagingDir::create(&final_path, "job-sync-fail").expect("create staging dir");
        staging
            .write_file_synced("manifest.json", br#"{"schema_version":2}"#)
            .expect("write staged manifest");

        let err = staging
            .commit_with_parent_sync(|parent| {
                Err(OsError::Fs {
                    operation: "fsync_dir",
                    path: parent.to_path_buf(),
                    source: io::Error::other("simulated parent fsync I/O failure"),
                })
            })
            .expect_err("parent fsync failure must surface distinct error");

        assert!(
            matches!(
                err,
                OsError::CommittedParentSyncFailed { ref path, .. } if path == &final_path
            ),
            "expected CommittedParentSyncFailed with final_path, got {err:?}"
        );
        assert!(
            staging.committed,
            "committed flag must already be true before parent fsync runs"
        );

        // Dropping `staging` after parent-fsync error must NOT delete the published bundle.
        drop(staging);
        assert!(
            final_path.join(COMMIT_MARKER_FILE).is_file(),
            "published bundle commit marker must remain intact on disk"
        );
        assert_eq!(
            fs::read(final_path.join("manifest.json")).expect("read published manifest"),
            br#"{"schema_version":2}"#,
            "published bundle contents must remain intact after parent fsync error"
        );
    }
}
