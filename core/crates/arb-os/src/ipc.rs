//! Local control-plane IPC transport (Unix Domain Sockets / Windows Named Pipes)
//! and length-bounded NDJSON framing helpers.

use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::OsError;

/// Default upper bound for a single NDJSON IPC line (4 MiB).
pub const DEFAULT_MAX_IPC_LINE_BYTES: usize = 4 * 1024 * 1024;

/// Local control-plane IPC server bound to a Unix Domain Socket (Unix) or
/// Named Pipe (Windows).
#[derive(Debug)]
pub struct LocalIpcListener {
    inner: imp::Listener,
    endpoint: PathBuf,
}

impl LocalIpcListener {
    /// Binds a new local IPC listener at `endpoint`.
    ///
    /// On Unix, creates the parent directory with `0700` permissions, removes a
    /// verified stale socket file (never removing an actively listening socket
    /// or non-socket file), binds the socket, and sets `0600` permissions on it.
    ///
    /// On Windows, creates a local-only Named Pipe (`\\.\pipe\...`) rejecting
    /// remote clients.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::AlreadyExists`] if a non-socket file occupies the path,
    /// [`OsError::InvalidArgument`] if the endpoint is malformed, or
    /// [`OsError::Ipc`] / [`OsError::Fs`] if binding fails.
    pub fn bind(endpoint: impl AsRef<Path>) -> Result<Self, OsError> {
        let endpoint = endpoint.as_ref().to_path_buf();
        let inner = imp::Listener::bind(&endpoint)?;
        Ok(Self { inner, endpoint })
    }

    /// Blocks until a local client connects and returns the bidirectional stream.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::Ipc`] if accepting a connection fails.
    pub fn accept(&self) -> Result<LocalIpcStream, OsError> {
        let stream = self.inner.accept(&self.endpoint)?;
        Ok(LocalIpcStream {
            inner: stream,
            endpoint: self.endpoint.clone(),
        })
    }

    /// Returns the socket path or Windows Named Pipe path this listener is bound to.
    #[must_use]
    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }
}

/// Bidirectional local IPC connection stream.
#[derive(Debug)]
pub struct LocalIpcStream {
    inner: imp::Stream,
    endpoint: PathBuf,
}

impl LocalIpcStream {
    /// Connects to a local IPC listener at `endpoint`.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::InvalidArgument`] if the endpoint is malformed, or
    /// [`OsError::Ipc`] if connecting fails.
    pub fn connect(endpoint: impl AsRef<Path>) -> Result<Self, OsError> {
        let endpoint = endpoint.as_ref().to_path_buf();
        let inner = imp::Stream::connect(&endpoint)?;
        Ok(Self { inner, endpoint })
    }

    /// Returns the socket path or Named Pipe path associated with this stream.
    #[must_use]
    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }

    /// Creates a new independently owned handle to the same underlying local
    /// IPC connection so callers can read and write on separate threads across
    /// all supported operating systems.
    ///
    /// On Unix this calls `UnixStream::try_clone`; on Windows this duplicates
    /// the Named Pipe handle via `DuplicateHandle`.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::Ipc`] if duplicating the underlying OS socket or
    /// pipe handle fails.
    pub fn try_clone(&self) -> Result<Self, OsError> {
        let inner = self.inner.try_clone(&self.endpoint)?;
        Ok(Self {
            inner,
            endpoint: self.endpoint.clone(),
        })
    }

    /// Waits for the peer to read all buffered outbound bytes before closing.
    ///
    /// On Windows this invokes `FlushFileBuffers` explicitly (unlike
    /// [`Write::flush`], which is a non-blocking no-op to avoid bidirectional
    /// JSON-RPC deadlocks). On Unix this delegates to `UnixStream::flush`.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::Ipc`] if draining the transport fails.
    pub fn drain(&mut self) -> Result<(), OsError> {
        self.inner.drain(&self.endpoint)
    }

    /// Sets an optional read timeout on the stream.
    ///
    /// Supported on Unix sockets and on Windows Named Pipes (enforced per call
    /// via overlapped I/O and `CancelIoEx`).
    ///
    /// # Errors
    ///
    /// Returns [`OsError::Ipc`] if `timeout` is `Some(Duration::ZERO)`
    /// (`InvalidInput`) or if configuring the timeout fails.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> Result<(), OsError> {
        self.inner.set_read_timeout(timeout, &self.endpoint)
    }

    /// Sets an optional write timeout on the stream.
    ///
    /// Supported on Unix sockets and on Windows Named Pipes (enforced per call
    /// via overlapped I/O and `CancelIoEx`).
    ///
    /// # Errors
    ///
    /// Returns [`OsError::Ipc`] if `timeout` is `Some(Duration::ZERO)`
    /// (`InvalidInput`) or if configuring the timeout fails.
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> Result<(), OsError> {
        self.inner.set_write_timeout(timeout, &self.endpoint)
    }
}

impl Read for LocalIpcStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Write for LocalIpcStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Writes a single NDJSON line terminated with `\n` and flushes `writer`.
///
/// Rejects payloads containing embedded `\n` or `\r` characters or exceeding
/// `max_line_bytes`.
///
/// # Errors
///
/// Returns [`OsError::InvalidArgument`] if `json_line` contains line breaks,
/// [`OsError::FrameTooLarge`] if `json_line.len() > max_line_bytes`, or
/// [`OsError::Io`] on write/flush failure.
pub fn write_frame_line(
    writer: &mut impl Write,
    json_line: &str,
    max_line_bytes: usize,
) -> Result<(), OsError> {
    if json_line.len() > max_line_bytes {
        return Err(OsError::FrameTooLarge {
            max_bytes: max_line_bytes,
        });
    }
    if json_line.bytes().any(|b| b == b'\n' || b == b'\r') {
        return Err(OsError::InvalidArgument {
            name: "json_line",
            reason: "NDJSON frame line must not contain embedded CR or LF bytes".to_owned(),
        });
    }

    writer
        .write_all(json_line.as_bytes())
        .map_err(|source| OsError::Io {
            operation: "write_frame_line",
            source,
        })?;
    writer.write_all(b"\n").map_err(|source| OsError::Io {
        operation: "write_frame_newline",
        source,
    })?;
    writer.flush().map_err(|source| OsError::Io {
        operation: "flush_frame_line",
        source,
    })
}

/// Stateful newline-delimited UTF-8 frame reader that preserves partial frame
/// bytes (and oversized-line discard state) across transient `TimedOut` /
/// `WouldBlock` read interruptions.
#[derive(Debug, Default)]
pub struct FrameLineReader {
    buf: Vec<u8>,
    discarding_oversized: bool,
}

impl FrameLineReader {
    /// Creates an empty frame reader with no buffered partial line.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: Vec::new(),
            discarding_oversized: false,
        }
    }

    /// Reads the next newline-delimited UTF-8 frame without buffering more than
    /// `max_line_bytes` bytes.
    ///
    /// If the underlying reader returns `TimedOut` or `WouldBlock` mid-frame,
    /// any bytes already read remain buffered inside `self` so the caller can
    /// retry after checking shutdown flags without losing frame data.
    ///
    /// Returns `Ok(None)` when the peer closes the stream cleanly before any
    /// bytes of a new line arrive. Trailing `\n` and optional `\r` are stripped.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::FrameTooLarge`] if a line exceeds `max_line_bytes`
    /// before its terminating `\n`, [`OsError::InvalidUtf8`] if the line is not
    /// valid UTF-8, or [`OsError::Io`] on underlying read errors (including
    /// `TimedOut` / `WouldBlock`).
    pub fn read_line(
        &mut self,
        reader: &mut impl BufRead,
        max_line_bytes: usize,
    ) -> Result<Option<String>, OsError> {
        loop {
            let available = match reader.fill_buf() {
                Ok(buf) => buf,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(source) => {
                    let operation = if self.discarding_oversized {
                        "drain_oversized_frame_line"
                    } else {
                        "read_frame_line"
                    };
                    return Err(OsError::Io { operation, source });
                }
            };

            if self.discarding_oversized {
                if available.is_empty() {
                    self.discarding_oversized = false;
                    return Err(OsError::FrameTooLarge {
                        max_bytes: max_line_bytes,
                    });
                }
                if let Some(newline_idx) = available.iter().position(|&b| b == b'\n') {
                    reader.consume(newline_idx + 1);
                    self.discarding_oversized = false;
                    return Err(OsError::FrameTooLarge {
                        max_bytes: max_line_bytes,
                    });
                }
                let len = available.len();
                reader.consume(len);
                continue;
            }

            if available.is_empty() {
                if self.buf.is_empty() {
                    return Ok(None);
                }
                break;
            }

            if let Some(newline_idx) = available.iter().position(|&b| b == b'\n') {
                let chunk = &available[..newline_idx];
                if self.buf.len().saturating_add(chunk.len()) > max_line_bytes {
                    // Consume through newline to keep framing state bounded if caller recovers.
                    reader.consume(newline_idx + 1);
                    self.buf.clear();
                    return Err(OsError::FrameTooLarge {
                        max_bytes: max_line_bytes,
                    });
                }
                self.buf.extend_from_slice(chunk);
                reader.consume(newline_idx + 1);
                break;
            }

            if self.buf.len().saturating_add(available.len()) > max_line_bytes {
                let len = available.len();
                reader.consume(len);
                self.buf.clear();
                self.discarding_oversized = true;
                continue;
            }

            let len = available.len();
            self.buf.extend_from_slice(available);
            reader.consume(len);
        }

        let mut line_bytes = std::mem::take(&mut self.buf);
        if line_bytes.last() == Some(&b'\r') {
            line_bytes.pop();
        }

        let text =
            String::from_utf8(line_bytes).map_err(|source| OsError::InvalidUtf8 { source })?;
        Ok(Some(text))
    }
}

/// Reads a single newline-delimited UTF-8 frame without allocating more than
/// `max_line_bytes` bytes.
///
/// For connections that poll with read timeouts and retry on `TimedOut` /
/// `WouldBlock`, use [`FrameLineReader`] so partial lines are preserved across
/// timeouts.
///
/// Returns `Ok(None)` when the peer closes the stream cleanly before any bytes
/// of a new line arrive. Trailing `\n` and optional `\r` are stripped.
///
/// # Errors
///
/// Returns [`OsError::FrameTooLarge`] if a line exceeds `max_line_bytes` before
/// its terminating `\n`, [`OsError::InvalidUtf8`] if the line is not valid
/// UTF-8, or [`OsError::Io`] on underlying read errors.
pub fn read_frame_line(
    reader: &mut impl BufRead,
    max_line_bytes: usize,
) -> Result<Option<String>, OsError> {
    FrameLineReader::new().read_line(reader, max_line_bytes)
}

#[cfg(unix)]
mod imp {
    use std::fs::{self, DirBuilder, Permissions};
    use std::io::{self, Read, Write};
    use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use crate::error::OsError;

    const DIR_MODE_OWNER_ONLY: u32 = 0o700;
    const SOCKET_MODE_OWNER_ONLY: u32 = 0o600;

    #[derive(Debug)]
    pub(super) struct Listener {
        listener: UnixListener,
        path: PathBuf,
    }

    impl Listener {
        pub(super) fn bind(path: &Path) -> Result<Self, OsError> {
            let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) else {
                return Err(OsError::InvalidArgument {
                    name: "endpoint",
                    reason: format!(
                        "socket path `{}` must have a parent directory",
                        path.display()
                    ),
                });
            };

            ensure_private_parent_dir(parent)?;
            remove_verified_stale_socket(path)?;

            let listener = UnixListener::bind(path).map_err(|source| OsError::Ipc {
                operation: "bind",
                endpoint: path.display().to_string(),
                source,
            })?;

            fs::set_permissions(path, Permissions::from_mode(SOCKET_MODE_OWNER_ONLY)).map_err(
                |source| OsError::Fs {
                    operation: "chmod_socket_0600",
                    path: path.to_path_buf(),
                    source,
                },
            )?;

            Ok(Self {
                listener,
                path: path.to_path_buf(),
            })
        }

        pub(super) fn accept(&self, endpoint: &Path) -> Result<Stream, OsError> {
            let (stream, _) = self.listener.accept().map_err(|source| OsError::Ipc {
                operation: "accept",
                endpoint: endpoint.display().to_string(),
                source,
            })?;
            Ok(Stream { stream })
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    fn ensure_private_parent_dir(parent: &Path) -> Result<(), OsError> {
        if !parent.exists() {
            DirBuilder::new()
                .recursive(true)
                .mode(DIR_MODE_OWNER_ONLY)
                .create(parent)
                .map_err(|source| OsError::Fs {
                    operation: "create_socket_dir",
                    path: parent.to_path_buf(),
                    source,
                })?;
        }
        fs::set_permissions(parent, Permissions::from_mode(DIR_MODE_OWNER_ONLY)).map_err(|source| {
            OsError::Fs {
                operation: "chmod_socket_dir_0700",
                path: parent.to_path_buf(),
                source,
            }
        })
    }

    fn remove_verified_stale_socket(path: &Path) -> Result<(), OsError> {
        let meta = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(OsError::Fs {
                    operation: "stat_existing_socket",
                    path: path.to_path_buf(),
                    source,
                });
            }
        };

        if !meta.file_type().is_socket() {
            return Err(OsError::AlreadyExists {
                path: path.to_path_buf(),
            });
        }

        match UnixStream::connect(path) {
            Ok(_) => Err(OsError::Ipc {
                operation: "bind",
                endpoint: path.display().to_string(),
                source: io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "an active process is already listening on this socket",
                ),
            }),
            Err(err)
                if err.kind() == io::ErrorKind::ConnectionRefused
                    || err.kind() == io::ErrorKind::NotFound =>
            {
                fs::remove_file(path).map_err(|source| OsError::Fs {
                    operation: "remove_stale_socket",
                    path: path.to_path_buf(),
                    source,
                })
            }
            Err(source) => Err(OsError::Ipc {
                operation: "probe_existing_socket",
                endpoint: path.display().to_string(),
                source,
            }),
        }
    }

    #[derive(Debug)]
    pub(super) struct Stream {
        stream: UnixStream,
    }

    impl Stream {
        pub(super) fn connect(path: &Path) -> Result<Self, OsError> {
            let stream = UnixStream::connect(path).map_err(|source| OsError::Ipc {
                operation: "connect",
                endpoint: path.display().to_string(),
                source,
            })?;
            Ok(Self { stream })
        }

        pub(super) fn try_clone(&self, endpoint: &Path) -> Result<Self, OsError> {
            let stream = self.stream.try_clone().map_err(|source| OsError::Ipc {
                operation: "try_clone",
                endpoint: endpoint.display().to_string(),
                source,
            })?;
            Ok(Self { stream })
        }

        pub(super) fn drain(&mut self, endpoint: &Path) -> Result<(), OsError> {
            self.stream.flush().map_err(|source| OsError::Ipc {
                operation: "drain",
                endpoint: endpoint.display().to_string(),
                source,
            })
        }

        pub(super) fn set_read_timeout(
            &self,
            timeout: Option<Duration>,
            endpoint: &Path,
        ) -> Result<(), OsError> {
            self.stream
                .set_read_timeout(timeout)
                .map_err(|source| OsError::Ipc {
                    operation: "set_read_timeout",
                    endpoint: endpoint.display().to_string(),
                    source,
                })
        }

        pub(super) fn set_write_timeout(
            &self,
            timeout: Option<Duration>,
            endpoint: &Path,
        ) -> Result<(), OsError> {
            self.stream
                .set_write_timeout(timeout)
                .map_err(|source| OsError::Ipc {
                    operation: "set_write_timeout",
                    endpoint: endpoint.display().to_string(),
                    source,
                })
        }

        pub(super) fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.stream.read(buf)
        }

        pub(super) fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.stream.write(buf)
        }

        pub(super) fn flush(&mut self) -> io::Result<()> {
            self.stream.flush()
        }
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use std::ffi::{OsStr, c_void};
    use std::io;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr;
    use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{
        CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_BROKEN_PIPE,
        ERROR_INSUFFICIENT_BUFFER, ERROR_IO_PENDING, ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY,
        ERROR_PIPE_CONNECTED, GENERIC_ALL, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
        INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAce, EqualSid,
        GetKernelObjectSecurity, GetLengthSid, GetSecurityDescriptorOwner, GetTokenInformation,
        InitializeAcl, InitializeSecurityDescriptor, OWNER_SECURITY_INFORMATION, PSID,
        SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SetSecurityDescriptorDacl,
        SetSecurityDescriptorOwner, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FlushFileBuffers,
        OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IDENTIFICATION,
        SECURITY_SQOS_PRESENT, WriteFile,
    };
    use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
        WaitNamedPipeW,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, GetCurrentProcess, OpenProcessToken, WaitForSingleObject,
    };

    use crate::error::OsError;

    const PIPE_PREFIX: &str = r"\\.\pipe\";
    /// `read_timeout_ms`/`write_timeout_ms` sentinel: block with no deadline.
    const NO_TIMEOUT_MS: u32 = u32::MAX;
    const PIPE_BUFFER_BYTES: u32 = 64 * 1024;
    const PIPE_CONNECT_WAIT_MS: u32 = 2_000;
    const PIPE_CONNECT_RETRIES: usize = 3;
    const SECURITY_DESCRIPTOR_REVISION_1: u32 = 1;

    #[derive(Debug)]
    struct OwnedPipeHandle {
        raw: HANDLE,
        /// Shared reference count across `try_clone()`d handles of a server
        /// pipe instance so `DisconnectNamedPipe` runs only when the final
        /// clone drops. `None` for client handles.
        server_refs: Option<Arc<AtomicUsize>>,
    }

    // SAFETY: Windows pipe `HANDLE`s are kernel object handles that are not tied
    // to a specific thread and may be moved between threads safely.
    unsafe impl Send for OwnedPipeHandle {}

    impl Drop for OwnedPipeHandle {
        fn drop(&mut self) {
            if let Some(refs) = &self.server_refs
                && refs.fetch_sub(1, Ordering::AcqRel) == 1
            {
                // SAFETY: `self.raw` is the last live handle to a server-side
                // named pipe instance.
                let _ = unsafe { DisconnectNamedPipe(self.raw) };
            }
            // SAFETY: `self.raw` is a valid owned Windows handle closed exactly once.
            let _ = unsafe { CloseHandle(self.raw) };
        }
    }

    struct OwnedTokenBuf {
        _buf: Vec<u8>,
        sid: PSID,
    }

    /// Per-call overlapped wait state: an `OVERLAPPED` plus a dedicated
    /// manual-reset event. Pipe file objects are opened `FILE_FLAG_OVERLAPPED`
    /// so a pending read never serializes a concurrent write issued through a
    /// `try_clone()`d handle (duplicates share one file object, and the I/O
    /// manager serializes synchronous IRPs per file object). Each IRP gets its
    /// own event so independent waits on the same object cannot consume each
    /// other's completion.
    struct OverlappedCall {
        overlapped: OVERLAPPED,
        event: HANDLE,
    }

    impl OverlappedCall {
        fn new() -> io::Result<Self> {
            // SAFETY: `CreateEventW` with null attributes creates an unnamed
            // manual-reset event that only this call waits on.
            let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
            if event.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `OVERLAPPED` is a plain C POD struct where all-zero bits
            // are valid; `hEvent` is then set to the dedicated wait event.
            let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
            overlapped.hEvent = event;
            Ok(Self { overlapped, event })
        }

        /// Waits for a `ERROR_IO_PENDING` IRP to complete, returning the
        /// transferred byte count, [`CallOutcome::TimedOut`] after
        /// `timeout_ms`, or the completion error. `u32::MAX` waits forever.
        fn wait(
            &mut self,
            raw: HANDLE,
            transferred: &mut u32,
            timeout_ms: u32,
        ) -> io::Result<CallOutcome> {
            if timeout_ms == u32::MAX {
                // SAFETY: `raw` is a valid pipe file-object handle, `overlapped`
                // identifies the in-flight IRP, and `transferred` is writable
                // stack storage. `bWait = 1` blocks until the IRP completes.
                let ok =
                    unsafe { GetOverlappedResult(raw, &raw mut self.overlapped, transferred, 1) };
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
                return Ok(CallOutcome::Completed);
            }

            // SAFETY: `event` is a valid manual-reset event owned by this call.
            let wait_rc = unsafe { WaitForSingleObject(self.event, timeout_ms) };
            match wait_rc {
                WAIT_OBJECT_0 => {
                    // SAFETY: the IRP already completed; `bWait = 0` only
                    // harvests its status and byte count.
                    let ok = unsafe {
                        GetOverlappedResult(raw, &raw mut self.overlapped, transferred, 0)
                    };
                    if ok == 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(CallOutcome::Completed)
                }
                WAIT_TIMEOUT => {
                    // SAFETY: `CancelIoEx` cancels only the IRP tied to this
                    // `overlapped`.
                    let _ = unsafe { CancelIoEx(raw, &raw mut self.overlapped) };
                    // SAFETY: reaping the IRP with `bWait = 1` keeps `overlapped`,
                    // `buf`, and `transferred` valid until kernel completion.
                    let ok = unsafe {
                        GetOverlappedResult(raw, &raw mut self.overlapped, transferred, 1)
                    };
                    if ok != 0 {
                        // The I/O completed between WAIT_TIMEOUT and CancelIoEx;
                        // report the completed transfer instead of dropping bytes
                        // or falsely claiming a write timed out.
                        return Ok(CallOutcome::Completed);
                    }
                    let err = io::Error::last_os_error();
                    if err.raw_os_error() == Some(ERROR_OPERATION_ABORTED.cast_signed()) {
                        return Ok(CallOutcome::TimedOut);
                    }
                    Err(err)
                }
                _ => {
                    let wait_err = io::Error::last_os_error();
                    // SAFETY: the IRP may still be pending; cancel it first so the
                    // blocking reap below does not hang indefinitely.
                    let _ = unsafe { CancelIoEx(raw, &raw mut self.overlapped) };
                    // SAFETY: reaping with `bWait = 1` keeps `overlapped`, `buf`,
                    // and `transferred` valid until the kernel completes the IRP.
                    let _ = unsafe {
                        GetOverlappedResult(raw, &raw mut self.overlapped, transferred, 1)
                    };
                    Err(wait_err)
                }
            }
        }
    }

    /// Result of an [`OverlappedCall`] wait.
    enum CallOutcome {
        Completed,
        TimedOut,
    }

    /// Converts an optional deadline to milliseconds (`NO_TIMEOUT_MS` when unset),
    /// rejecting `Some(Duration::ZERO)` like Unix and rounding sub-millisecond
    /// durations up so finite waits never collapse to 0 ms or collide with
    /// `NO_TIMEOUT_MS`.
    fn timeout_millis(timeout: Option<Duration>) -> io::Result<u32> {
        match timeout {
            None => Ok(NO_TIMEOUT_MS),
            Some(d) if d.is_zero() => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot set a 0 duration timeout",
            )),
            Some(d) => {
                let ms = d.as_nanos().div_ceil(1_000_000);
                Ok(u32::try_from(ms)
                    .unwrap_or(u32::MAX - 1)
                    .clamp(1, u32::MAX - 1))
            }
        }
    }

    impl Drop for OverlappedCall {
        fn drop(&mut self) {
            // SAFETY: `event` is a valid owned event handle closed exactly once,
            // and the associated IRP has already completed (or never started).
            let _ = unsafe { CloseHandle(self.event) };
        }
    }

    fn query_process_user_sid(proc_handle: HANDLE) -> io::Result<OwnedTokenBuf> {
        let mut token: HANDLE = ptr::null_mut();
        // SAFETY: `proc_handle` is a valid process handle and `token` is writable stack storage.
        let ok = unsafe { OpenProcessToken(proc_handle, TOKEN_QUERY, &raw mut token) };
        if ok == 0 || token.is_null() {
            return Err(io::Error::last_os_error());
        }

        let mut needed: u32 = 0;
        // SAFETY: Passing a null output buffer queries the required byte size.
        let _ =
            unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &raw mut needed) };
        let buf_len = usize::try_from(needed).unwrap_or(0);
        if buf_len < size_of::<TOKEN_USER>() {
            // SAFETY: `token` was opened by `OpenProcessToken` and is closed once.
            let _ = unsafe { CloseHandle(token) };
            return Err(io::Error::other(
                "GetTokenInformation returned invalid length",
            ));
        }

        let mut buf = vec![0_u8; buf_len];
        // SAFETY: `buf` is writable for `needed` bytes and `token` is valid.
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buf.as_mut_ptr().cast::<c_void>(),
                needed,
                &raw mut needed,
            )
        };
        // SAFETY: `token` was opened by `OpenProcessToken` and is closed once.
        let _ = unsafe { CloseHandle(token) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: `GetTokenInformation(TokenUser)` initialized a `TOKEN_USER` prefix in `buf`.
        let token_user = unsafe { ptr::read_unaligned(buf.as_ptr().cast::<TOKEN_USER>()) };
        let sid = token_user.User.Sid;
        if sid.is_null() {
            return Err(io::Error::other("process token contained null user SID"));
        }
        Ok(OwnedTokenBuf { _buf: buf, sid })
    }

    fn query_current_user_sid() -> io::Result<OwnedTokenBuf> {
        // SAFETY: `GetCurrentProcess` returns a pseudo-handle that does not need closing.
        let current = unsafe { GetCurrentProcess() };
        query_process_user_sid(current)
    }

    struct OwnerOnlySecurity {
        _token: OwnedTokenBuf,
        _acl_storage: Vec<u64>,
        descriptor: SECURITY_DESCRIPTOR,
    }

    impl OwnerOnlySecurity {
        fn build() -> io::Result<Self> {
            let token = query_current_user_sid()?;
            // SAFETY: `token.sid` points to a valid SID owned by `token._buf`.
            let sid_len_u32 = unsafe { GetLengthSid(token.sid) };
            let sid_len = usize::try_from(sid_len_u32)
                .map_err(|_| io::Error::other("SID length overflowed usize"))?;
            let acl_bytes = size_of::<ACL>()
                .saturating_add(size_of::<ACCESS_ALLOWED_ACE>())
                .saturating_add(sid_len);
            let acl_words = acl_bytes.div_ceil(size_of::<u64>());
            let mut acl_storage = vec![0_u64; acl_words];
            let acl_len_u32 = u32::try_from(acl_words.saturating_mul(size_of::<u64>()))
                .map_err(|_| io::Error::other("ACL size overflowed u32"))?;
            let acl_ptr = acl_storage.as_mut_ptr().cast::<ACL>();

            // SAFETY: `acl_ptr` is 8-byte aligned and points to `acl_len_u32` writable bytes.
            let init_acl_ok = unsafe { InitializeAcl(acl_ptr, acl_len_u32, ACL_REVISION) };
            if init_acl_ok == 0 {
                return Err(io::Error::last_os_error());
            }

            // SAFETY: `acl_ptr` is an initialized ACL large enough for one ACE carrying `token.sid`.
            let add_ace_ok =
                unsafe { AddAccessAllowedAce(acl_ptr, ACL_REVISION, GENERIC_ALL, token.sid) };
            if add_ace_ok == 0 {
                return Err(io::Error::last_os_error());
            }

            // SAFETY: `SECURITY_DESCRIPTOR` is a C POD struct initialized below before use.
            let mut descriptor: SECURITY_DESCRIPTOR = unsafe { std::mem::zeroed() };
            let sd_ptr = (&raw mut descriptor).cast::<c_void>();
            // SAFETY: `sd_ptr` points to writable `SECURITY_DESCRIPTOR` storage.
            let init_sd_ok =
                unsafe { InitializeSecurityDescriptor(sd_ptr, SECURITY_DESCRIPTOR_REVISION_1) };
            if init_sd_ok == 0 {
                return Err(io::Error::last_os_error());
            }

            // SAFETY: `sd_ptr` is an initialized security descriptor and `acl_ptr`
            // remains valid as long as `_acl_storage` is alive.
            let set_dacl_ok = unsafe { SetSecurityDescriptorDacl(sd_ptr, 1, acl_ptr, 0) };
            if set_dacl_ok == 0 {
                return Err(io::Error::last_os_error());
            }

            // Explicitly assign the server's `TokenUser` SID as the pipe owner:
            // an elevated admin token otherwise defaults new object owners to
            // `BUILTIN\Administrators`, which would cause non-elevated clients
            // of the same user account to fail `EqualSid` ownership verification.
            // SAFETY: `sd_ptr` is an initialized security descriptor and `token.sid`
            // remains valid as long as `_token` is alive.
            let set_owner_ok = unsafe { SetSecurityDescriptorOwner(sd_ptr, token.sid, 0) };
            if set_owner_ok == 0 {
                return Err(io::Error::last_os_error());
            }

            Ok(Self {
                _token: token,
                _acl_storage: acl_storage,
                descriptor,
            })
        }

        fn attributes(&mut self) -> SECURITY_ATTRIBUTES {
            let n_length = u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0);
            SECURITY_ATTRIBUTES {
                nLength: n_length,
                lpSecurityDescriptor: (&raw mut self.descriptor).cast::<c_void>(),
                bInheritHandle: 0,
            }
        }
    }

    fn verify_pipe_server_owned_by_current_user(pipe: HANDLE) -> io::Result<()> {
        // The pipe object's own security descriptor records the pipe creator —
        // i.e. the server process's user — as owner. `GetKernelObjectSecurity`
        // is a local kernel-object lookup, so it never issues an FSCTL against
        // the pipe and cannot wedge behind the peer's in-flight synchronous
        // pipe I/O the way `GetNamedPipeServerProcessId` (via `NtFsControlFile`)
        // can on a client handle.
        let mut needed: u32 = 0;
        // SAFETY: `pipe` is a valid client pipe handle; the zero-length probe
        // reports the required descriptor size and is expected to fail with
        // ERROR_INSUFFICIENT_BUFFER.
        let probe = unsafe {
            GetKernelObjectSecurity(
                pipe,
                OWNER_SECURITY_INFORMATION,
                ptr::null_mut(),
                0,
                &raw mut needed,
            )
        };
        // SAFETY: `GetLastError` reads the calling thread's last-error slot.
        if probe == 0 && unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
            return Err(io::Error::last_os_error());
        }
        let sd_len = usize::try_from(needed).unwrap_or(0);
        if sd_len == 0 {
            return Err(io::Error::other(
                "GetKernelObjectSecurity reported an empty security descriptor",
            ));
        }
        let mut sd = vec![0_u8; sd_len];
        // SAFETY: `sd` is writable for `needed` bytes and `pipe` remains valid.
        let ok = unsafe {
            GetKernelObjectSecurity(
                pipe,
                OWNER_SECURITY_INFORMATION,
                sd.as_mut_ptr().cast(),
                needed,
                &raw mut needed,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }

        let mut owner_sid: PSID = ptr::null_mut();
        let mut owner_defaulted: i32 = 0;
        // SAFETY: `sd` holds a valid self-relative `SECURITY_DESCRIPTOR` for the
        // duration of the call, and the out-parameters are writable stack storage.
        let owner_ok = unsafe {
            GetSecurityDescriptorOwner(
                sd.as_mut_ptr().cast::<c_void>(),
                &raw mut owner_sid,
                &raw mut owner_defaulted,
            )
        };
        if owner_ok == 0 || owner_sid.is_null() {
            return Err(io::Error::last_os_error());
        }

        let client_sid = query_current_user_sid()?;
        // SAFETY: `owner_sid` points into the live `sd` buffer and
        // `client_sid.sid` is a valid SID backed by `client_sid._buf`.
        let same_owner = unsafe { EqualSid(client_sid.sid, owner_sid) };
        if same_owner == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "named pipe server belongs to a different user account",
            ));
        }
        Ok(())
    }

    fn encode_pipe_name(path: &Path) -> Result<Vec<u16>, OsError> {
        let Some(text) = path.to_str() else {
            return Err(OsError::InvalidArgument {
                name: "endpoint",
                reason: "Windows named pipe endpoint must be valid UTF-8".to_owned(),
            });
        };
        if !text.starts_with(PIPE_PREFIX) || text.len() <= PIPE_PREFIX.len() {
            return Err(OsError::InvalidArgument {
                name: "endpoint",
                reason: format!("Windows named pipe must start with `{PIPE_PREFIX}`: `{text}`"),
            });
        }
        Ok(OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect())
    }

    #[derive(Clone, Copy)]
    enum PipeInstanceMode {
        First,
        Additional,
    }

    fn create_pipe_instance(
        wide_name: &[u16],
        mode: PipeInstanceMode,
    ) -> io::Result<OwnedPipeHandle> {
        // `FILE_FLAG_OVERLAPPED` is required: `LocalIpcStream::try_clone`
        // duplicates handles onto the same file object, and the I/O manager
        // serializes *synchronous* IRPs per file object. A pending `ReadFile`
        // would then block a `WriteFile` on a cloned handle, deadlocking the
        // documented split-reader/writer usage. Overlapped file objects have
        // no such serialization; `read`/`write`/`accept` still expose blocking
        // semantics by waiting on a per-call `OVERLAPPED`.
        let open_mode = match mode {
            PipeInstanceMode::First => {
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED
            }
            PipeInstanceMode::Additional => PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
        };
        let pipe_mode =
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
        let mut security = OwnerOnlySecurity::build()?;
        let sa = security.attributes();
        // SAFETY: `wide_name` is a NUL-terminated UTF-16 string and `&raw const sa`
        // points to a valid `SECURITY_ATTRIBUTES` backed by `security` for the
        // duration of `CreateNamedPipeW`.
        let raw = unsafe {
            CreateNamedPipeW(
                wide_name.as_ptr(),
                open_mode,
                pipe_mode,
                PIPE_UNLIMITED_INSTANCES,
                PIPE_BUFFER_BYTES,
                PIPE_BUFFER_BYTES,
                0,
                &raw const sa,
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(OwnedPipeHandle {
            raw,
            server_refs: Some(Arc::new(AtomicUsize::new(1))),
        })
    }

    #[derive(Debug)]
    pub(super) struct Listener {
        wide_name: Vec<u16>,
        pending: Mutex<Option<OwnedPipeHandle>>,
    }

    impl Listener {
        pub(super) fn bind(path: &Path) -> Result<Self, OsError> {
            let wide_name = encode_pipe_name(path)?;
            let first =
                create_pipe_instance(&wide_name, PipeInstanceMode::First).map_err(|source| {
                    OsError::Ipc {
                        operation: "CreateNamedPipeW",
                        endpoint: path.display().to_string(),
                        source,
                    }
                })?;
            Ok(Self {
                wide_name,
                pending: Mutex::new(Some(first)),
            })
        }

        pub(super) fn accept(&self, endpoint: &Path) -> Result<Stream, OsError> {
            let handle = {
                let mut guard = self.pending.lock().map_err(|_| OsError::Ipc {
                    operation: "lock_listener",
                    endpoint: endpoint.display().to_string(),
                    source: io::Error::other("ipc listener mutex poisoned"),
                })?;
                if let Some(existing) = guard.take() {
                    existing
                } else {
                    create_pipe_instance(&self.wide_name, PipeInstanceMode::Additional).map_err(
                        |source| OsError::Ipc {
                            operation: "CreateNamedPipeW",
                            endpoint: endpoint.display().to_string(),
                            source,
                        },
                    )?
                }
            };

            let mut call = OverlappedCall::new().map_err(|source| OsError::Ipc {
                operation: "create_connect_event",
                endpoint: endpoint.display().to_string(),
                source,
            })?;
            // SAFETY: `handle.raw` is a valid overlapped server-side pipe handle
            // and `call.overlapped` identifies this connect IRP.
            let ok = unsafe { ConnectNamedPipe(handle.raw, &raw mut call.overlapped) };
            if ok == 0 {
                // SAFETY: `GetLastError` reads the calling thread's last-error slot.
                let code = unsafe { GetLastError() };
                if code == ERROR_IO_PENDING {
                    // The pipe object is overlapped: wait for a client attach.
                    call.wait(handle.raw, &mut 0, NO_TIMEOUT_MS)
                        .map_err(|source| OsError::Ipc {
                            operation: "ConnectNamedPipe",
                            endpoint: endpoint.display().to_string(),
                            source,
                        })?;
                } else if code != ERROR_PIPE_CONNECTED {
                    return Err(OsError::Ipc {
                        operation: "ConnectNamedPipe",
                        endpoint: endpoint.display().to_string(),
                        source: io::Error::last_os_error(),
                    });
                }
            }

            // Pre-create the next server pipe instance so concurrent clients do
            // not encounter transient `ERROR_FILE_NOT_FOUND`.
            if let Ok(next_instance) =
                create_pipe_instance(&self.wide_name, PipeInstanceMode::Additional)
                && let Ok(mut guard) = self.pending.lock()
                && guard.is_none()
            {
                *guard = Some(next_instance);
            }

            Ok(Stream::with_no_timeouts(handle))
        }
    }

    #[derive(Debug)]
    pub(super) struct Stream {
        handle: OwnedPipeHandle,
        /// Per-call read deadline in milliseconds (`NO_TIMEOUT_MS` = block).
        read_timeout_ms: AtomicU32,
        /// Per-call write deadline in milliseconds (`NO_TIMEOUT_MS` = block).
        write_timeout_ms: AtomicU32,
    }

    impl Stream {
        fn with_no_timeouts(handle: OwnedPipeHandle) -> Self {
            Self {
                handle,
                read_timeout_ms: AtomicU32::new(NO_TIMEOUT_MS),
                write_timeout_ms: AtomicU32::new(NO_TIMEOUT_MS),
            }
        }
    }

    impl Stream {
        pub(super) fn connect(path: &Path) -> Result<Self, OsError> {
            let wide_name = encode_pipe_name(path)?;
            for attempt in 0..=PIPE_CONNECT_RETRIES {
                // SAFETY: `wide_name` is a NUL-terminated UTF-16 pipe name;
                // `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION` prevents
                // server-side impersonation of the client, and
                // `FILE_FLAG_OVERLAPPED` keeps concurrent IRPs on cloned
                // handles unsynchronized (see `create_pipe_instance`).
                let raw = unsafe {
                    CreateFileW(
                        wide_name.as_ptr(),
                        GENERIC_READ | GENERIC_WRITE,
                        0,
                        ptr::null(),
                        OPEN_EXISTING,
                        SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION | FILE_FLAG_OVERLAPPED,
                        ptr::null_mut(),
                    )
                };
                if raw != INVALID_HANDLE_VALUE {
                    let handle = OwnedPipeHandle {
                        raw,
                        server_refs: None,
                    };
                    verify_pipe_server_owned_by_current_user(handle.raw).map_err(|source| {
                        OsError::Ipc {
                            operation: "verify_pipe_server_owner",
                            endpoint: path.display().to_string(),
                            source,
                        }
                    })?;
                    return Ok(Self::with_no_timeouts(handle));
                }

                // SAFETY: `GetLastError` reads the calling thread's last-error slot.
                let code = unsafe { GetLastError() };
                if code == ERROR_PIPE_BUSY && attempt < PIPE_CONNECT_RETRIES {
                    // SAFETY: `wide_name` is a NUL-terminated UTF-16 pipe name.
                    let _ = unsafe { WaitNamedPipeW(wide_name.as_ptr(), PIPE_CONNECT_WAIT_MS) };
                    continue;
                }

                return Err(OsError::Ipc {
                    operation: "CreateFileW",
                    endpoint: path.display().to_string(),
                    source: io::Error::last_os_error(),
                });
            }

            Err(OsError::Ipc {
                operation: "CreateFileW",
                endpoint: path.display().to_string(),
                source: io::Error::new(io::ErrorKind::TimedOut, "named pipe remained busy"),
            })
        }

        pub(super) fn try_clone(&self, endpoint: &Path) -> Result<Self, OsError> {
            // SAFETY: `GetCurrentProcess` returns a pseudo-handle valid for this process.
            let current = unsafe { GetCurrentProcess() };
            let mut duplicated: HANDLE = ptr::null_mut();
            // SAFETY: `self.handle.raw` is a valid open pipe handle and `duplicated`
            // is writable stack storage receiving the newly owned handle.
            let ok = unsafe {
                DuplicateHandle(
                    current,
                    self.handle.raw,
                    current,
                    &raw mut duplicated,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                )
            };
            if ok == 0 || duplicated.is_null() {
                return Err(OsError::Ipc {
                    operation: "DuplicateHandle",
                    endpoint: endpoint.display().to_string(),
                    source: io::Error::last_os_error(),
                });
            }
            let server_refs = self.handle.server_refs.as_ref().map(|refs| {
                refs.fetch_add(1, Ordering::Relaxed);
                Arc::clone(refs)
            });
            Ok(Self {
                handle: OwnedPipeHandle {
                    raw: duplicated,
                    server_refs,
                },
                // The clone inherits the current deadlines; either handle may
                // retune its own later via `set_*_timeout`.
                read_timeout_ms: AtomicU32::new(self.read_timeout_ms.load(Ordering::Relaxed)),
                write_timeout_ms: AtomicU32::new(self.write_timeout_ms.load(Ordering::Relaxed)),
            })
        }

        pub(super) fn drain(&mut self, endpoint: &Path) -> Result<(), OsError> {
            // SAFETY: `self.handle.raw` is a valid open pipe handle.
            let ok = unsafe { FlushFileBuffers(self.handle.raw) };
            if ok == 0 {
                return Err(OsError::Ipc {
                    operation: "FlushFileBuffers",
                    endpoint: endpoint.display().to_string(),
                    source: io::Error::last_os_error(),
                });
            }
            Ok(())
        }

        pub(super) fn set_read_timeout(
            &self,
            timeout: Option<Duration>,
            endpoint: &Path,
        ) -> Result<(), OsError> {
            let ms = timeout_millis(timeout).map_err(|source| OsError::Ipc {
                operation: "set_read_timeout",
                endpoint: endpoint.display().to_string(),
                source,
            })?;
            // The pipe file object is overlapped, so a blocked `ReadFile` can be
            // cancelled per call via `CancelIoEx` — the deadline is enforced by
            // `OverlappedCall::wait` inside `read`.
            self.read_timeout_ms.store(ms, Ordering::Release);
            Ok(())
        }

        pub(super) fn set_write_timeout(
            &self,
            timeout: Option<Duration>,
            endpoint: &Path,
        ) -> Result<(), OsError> {
            let ms = timeout_millis(timeout).map_err(|source| OsError::Ipc {
                operation: "set_write_timeout",
                endpoint: endpoint.display().to_string(),
                source,
            })?;
            self.write_timeout_ms.store(ms, Ordering::Release);
            Ok(())
        }

        pub(super) fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let to_read = u32::try_from(buf.len()).unwrap_or(u32::MAX);
            let mut bytes_read: u32 = 0;
            let mut call = OverlappedCall::new()?;
            // SAFETY: `self.handle.raw` is a valid overlapped pipe handle, `buf`
            // is writable for `to_read` bytes, and `bytes_read` plus
            // `call.overlapped` stay valid until the IRP completes — either
            // synchronously or via `call.wait` below.
            let ok = unsafe {
                ReadFile(
                    self.handle.raw,
                    buf.as_mut_ptr(),
                    to_read,
                    &raw mut bytes_read,
                    &raw mut call.overlapped,
                )
            };
            if ok == 0 {
                // SAFETY: `GetLastError` reads the calling thread's last-error slot.
                let code = unsafe { GetLastError() };
                if code == ERROR_IO_PENDING {
                    let timeout = self.read_timeout_ms.load(Ordering::Acquire);
                    match call.wait(self.handle.raw, &mut bytes_read, timeout) {
                        Ok(CallOutcome::TimedOut) => {
                            return Err(io::Error::new(
                                io::ErrorKind::TimedOut,
                                "named pipe read timed out",
                            ));
                        }
                        // A broken pipe while the IRP was pending is the same
                        // clean-EOF condition `ReadFile` reports inline.
                        Err(source)
                            if source.raw_os_error() == Some(ERROR_BROKEN_PIPE.cast_signed()) =>
                        {
                            return Ok(0);
                        }
                        result => result.map(|_| ())?,
                    }
                } else if code == ERROR_BROKEN_PIPE {
                    return Ok(0);
                } else {
                    return Err(io::Error::last_os_error());
                }
            }
            usize::try_from(bytes_read)
                .map_err(|_| io::Error::other("ReadFile byte count overflowed usize"))
        }

        pub(super) fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let to_write = u32::try_from(buf.len()).unwrap_or(u32::MAX);
            let mut bytes_written: u32 = 0;
            let mut call = OverlappedCall::new()?;
            // SAFETY: `self.handle.raw` is a valid overlapped pipe handle, `buf`
            // is readable for `to_write` bytes, and `bytes_written` plus
            // `call.overlapped` stay valid until the IRP completes.
            let ok = unsafe {
                WriteFile(
                    self.handle.raw,
                    buf.as_ptr(),
                    to_write,
                    &raw mut bytes_written,
                    &raw mut call.overlapped,
                )
            };
            if ok == 0 {
                // SAFETY: `GetLastError` reads the calling thread's last-error slot.
                let code = unsafe { GetLastError() };
                if code == ERROR_IO_PENDING {
                    let timeout = self.write_timeout_ms.load(Ordering::Acquire);
                    match call.wait(self.handle.raw, &mut bytes_written, timeout) {
                        Ok(CallOutcome::TimedOut) => {
                            return Err(io::Error::new(
                                io::ErrorKind::TimedOut,
                                "named pipe write timed out",
                            ));
                        }
                        result => result.map(|_| ())?,
                    }
                } else {
                    return Err(io::Error::last_os_error());
                }
            }
            usize::try_from(bytes_written)
                .map_err(|_| io::Error::other("WriteFile byte count overflowed usize"))
        }

        #[expect(
            clippy::unused_self,
            clippy::unnecessary_wraps,
            reason = "signature mirrors `Write::flush`, which is fallible on other transports"
        )]
        pub(super) fn flush(&mut self) -> io::Result<()> {
            // Named pipe writes already enter the kernel pipe buffer. Calling
            // `FlushFileBuffers` here would block until the peer reads every
            // byte, deadlocking bidirectional JSON-RPC traffic; use `drain()`
            // explicitly when waiting for peer consumption before close.
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{BufReader, Cursor};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    use super::{
        DEFAULT_MAX_IPC_LINE_BYTES, LocalIpcListener, LocalIpcStream, read_frame_line,
        write_frame_line,
    };
    use crate::error::OsError;

    static IPC_SEQ: AtomicU64 = AtomicU64::new(1);

    struct TestIpcDir {
        path: PathBuf,
    }

    impl TestIpcDir {
        fn new(label: &str) -> Self {
            let seq = IPC_SEQ.fetch_add(1, Ordering::Relaxed);
            // Keep path short for macOS/Linux sun_path (~104 byte) limits.
            let path =
                std::env::temp_dir().join(format!("arb-ipc-{label}-{}-{seq}", std::process::id()));
            Self { path }
        }
    }

    impl Drop for TestIpcDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn uds_round_trip_multi_message_ndjson_and_permissions() {
        #[cfg_attr(
            windows,
            expect(
                unused_variables,
                reason = "Windows uses a named pipe, not a socket file"
            )
        )]
        let tmp = TestIpcDir::new("rt");
        #[cfg(unix)]
        let sock_path = tmp.path.join("run").join("arbd.sock");
        #[cfg(windows)]
        let sock_path = PathBuf::from(format!(
            r"\\.\pipe\arb-os-ipc-rt-{}-{}",
            std::process::id(),
            IPC_SEQ.fetch_add(1, Ordering::Relaxed)
        ));

        let listener = LocalIpcListener::bind(&sock_path).expect("bind local IPC listener");
        assert_eq!(
            listener.endpoint(),
            sock_path.as_path(),
            "listener endpoint should match bound socket path"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let dir_mode = fs::metadata(sock_path.parent().expect("socket parent"))
                .expect("stat socket parent dir")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                dir_mode, 0o700,
                "socket parent dir must have 0700 permissions, got {dir_mode:#o}"
            );

            let sock_mode = fs::metadata(&sock_path)
                .expect("stat socket file")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                sock_mode, 0o600,
                "socket file must have 0600 permissions, got {sock_mode:#o}"
            );
        }

        let server = thread::spawn(move || {
            let mut stream = listener.accept().expect("accept client connection");
            let mut reader = BufReader::new(&mut stream);
            let mut received = Vec::new();
            while let Some(line) =
                read_frame_line(&mut reader, DEFAULT_MAX_IPC_LINE_BYTES).expect("read server line")
            {
                received.push(line);
                if received.len() == 3 {
                    break;
                }
            }
            let inner_stream = reader.into_inner();
            for (idx, msg) in received.iter().enumerate() {
                let reply = format!(r#"{{"ack":{idx},"echo":{msg}}}"#);
                write_frame_line(inner_stream, &reply, DEFAULT_MAX_IPC_LINE_BYTES)
                    .expect("write server reply");
            }
            // Wait for the client to consume the replies before `Drop` runs
            // `DisconnectNamedPipe` on Windows (which otherwise discards unread pipe bytes).
            inner_stream.drain().expect("drain server replies");
            received
        });

        let mut client = LocalIpcStream::connect(&sock_path).expect("connect to local IPC");
        let messages = [
            r#"{"jsonrpc":"2.0","id":1,"method":"system.hello"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"device.list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"capture.status"}"#,
        ];
        for msg in messages {
            write_frame_line(&mut client, msg, DEFAULT_MAX_IPC_LINE_BYTES)
                .expect("write client request");
        }

        let mut client_reader = BufReader::new(client);
        let mut replies = Vec::new();
        for _ in 0..messages.len() {
            let reply = read_frame_line(&mut client_reader, DEFAULT_MAX_IPC_LINE_BYTES)
                .expect("read client reply")
                .expect("expected reply line");
            replies.push(reply);
        }

        let server_received = server.join().expect("server thread must not panic");
        assert_eq!(
            server_received,
            messages.map(ToOwned::to_owned),
            "server must receive all 3 NDJSON messages intact"
        );
        assert_eq!(replies.len(), 3, "client must receive all 3 replies");
        assert!(
            replies[0].contains(r#""ack":0"#) && replies[2].contains(r#""ack":2"#),
            "replies must match request order: {replies:?}"
        );
    }

    #[test]
    fn oversized_ndjson_line_is_rejected_on_read_and_write() {
        let mut sink = Vec::new();
        let write_err = write_frame_line(&mut sink, "123456789", 8)
            .expect_err("write_frame_line must reject line > max_line_bytes");
        assert!(
            matches!(write_err, OsError::FrameTooLarge { max_bytes: 8 }),
            "expected FrameTooLarge on write, got {write_err:?}"
        );

        let newline_err = write_frame_line(&mut sink, "line1\nline2", 64)
            .expect_err("write_frame_line must reject embedded newlines");
        assert!(
            matches!(newline_err, OsError::InvalidArgument { .. }),
            "expected InvalidArgument on embedded newline, got {newline_err:?}"
        );

        let payload = b"0123456789abcdef\n";
        let mut reader = BufReader::new(Cursor::new(payload));
        let read_err = read_frame_line(&mut reader, 8)
            .expect_err("read_frame_line must reject line > max_line_bytes");
        assert!(
            matches!(read_err, OsError::FrameTooLarge { max_bytes: 8 }),
            "expected FrameTooLarge on read, got {read_err:?}"
        );

        // Also test oversized stream with no newline at all (unbounded DoS guard).
        let huge_no_newline = vec![b'a'; 1024];
        let mut no_nl_reader = BufReader::with_capacity(16, Cursor::new(huge_no_newline));
        let dos_err = read_frame_line(&mut no_nl_reader, 32)
            .expect_err("must reject unbounded non-terminated stream");
        assert!(
            matches!(dos_err, OsError::FrameTooLarge { max_bytes: 32 }),
            "expected FrameTooLarge on unterminated stream, got {dos_err:?}"
        );

        // Clean EOF returns None and CRLF line endings are stripped cleanly.
        let mut crlf_reader = BufReader::new(Cursor::new(b"{\"ok\":true}\r\n"));
        assert_eq!(
            read_frame_line(&mut crlf_reader, 64).expect("CRLF line should decode cleanly"),
            Some("{\"ok\":true}".to_owned()),
            "CRLF terminator must be stripped"
        );
        assert_eq!(
            read_frame_line(&mut crlf_reader, 64).expect("EOF after last line should succeed"),
            None,
            "clean EOF must return Ok(None)"
        );
    }

    #[cfg(unix)]
    #[test]
    fn stale_socket_is_recovered_but_active_socket_and_regular_file_are_protected() {
        use std::os::unix::net::UnixListener;

        let tmp = TestIpcDir::new("stale");
        fs::create_dir_all(&tmp.path).expect("create temp dir");

        // 1. Leave behind a stale socket file (raw UnixListener does not unlink on drop).
        let stale_path = tmp.path.join("stale.sock");
        {
            let _raw_listener = UnixListener::bind(&stale_path).expect("bind raw socket");
        }
        assert!(
            stale_path.exists(),
            "raw UnixListener drop should leave a stale socket file"
        );

        let recovered = LocalIpcListener::bind(&stale_path)
            .expect("LocalIpcListener must recover a verified stale socket");

        // 2. While `recovered` is actively listening, a second bind must fail.
        let active_err = LocalIpcListener::bind(&stale_path)
            .expect_err("must refuse to replace an actively listening socket");
        assert!(
            matches!(active_err, OsError::Ipc { .. }),
            "expected Ipc error for active socket, got {active_err:?}"
        );
        drop(recovered);
        assert!(
            !stale_path.exists(),
            "LocalIpcListener drop should clean up its own socket file"
        );

        // 3. Regular file at socket path must NEVER be deleted.
        let file_path = tmp.path.join("not-a-socket.txt");
        fs::write(&file_path, b"keep me").expect("create regular file");
        let file_err = LocalIpcListener::bind(&file_path)
            .expect_err("must refuse to overwrite a regular file");
        assert!(
            matches!(file_err, OsError::AlreadyExists { .. }),
            "expected AlreadyExists for regular file, got {file_err:?}"
        );
        assert_eq!(
            fs::read(&file_path).expect("regular file must still exist"),
            b"keep me",
            "regular file contents must be untouched"
        );
    }

    #[test]
    fn oversized_line_drains_through_newline_so_tail_is_never_parsed_as_frame() {
        // Use a tiny BufReader capacity (8 bytes) so the oversized line spans
        // multiple internal buffer refills before reaching '\n'.
        let wire = b"aaaaaaaaaaaaaaaaaaaaaaaa{\"injected\":true}\n{\"valid\":1}\n";
        let mut reader = BufReader::with_capacity(8, Cursor::new(wire));

        let err = read_frame_line(&mut reader, 12)
            .expect_err("oversized frame must return FrameTooLarge");
        assert!(
            matches!(err, OsError::FrameTooLarge { max_bytes: 12 }),
            "expected FrameTooLarge(12), got {err:?}"
        );

        let next = read_frame_line(&mut reader, 64)
            .expect("next valid frame after drained oversized line must decode")
            .expect("expected valid frame");
        assert_eq!(
            next, r#"{"valid":1}"#,
            "trailing bytes of oversized line must be drained and never parsed as a new frame"
        );
        assert_eq!(
            read_frame_line(&mut reader, 64).expect("clean EOF after valid frame"),
            None,
            "stream must be at EOF after valid frame"
        );
    }

    #[test]
    fn stream_try_clone_supports_concurrent_split_reader_and_writer() {
        #[cfg_attr(
            windows,
            expect(
                unused_variables,
                reason = "Windows uses a named pipe, not a socket file"
            )
        )]
        let tmp = TestIpcDir::new("clone");
        #[cfg(unix)]
        let sock_path = tmp.path.join("run").join("clone.sock");
        #[cfg(windows)]
        let sock_path = PathBuf::from(format!(
            r"\\.\pipe\arb-os-ipc-clone-{}-{}",
            std::process::id(),
            IPC_SEQ.fetch_add(1, Ordering::Relaxed)
        ));

        let listener = LocalIpcListener::bind(&sock_path).expect("bind clone test listener");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept client");
            let mut writer = stream.try_clone().expect("server try_clone must succeed");
            let mut reader = BufReader::new(stream);

            let line = read_frame_line(&mut reader, DEFAULT_MAX_IPC_LINE_BYTES)
                .expect("server read line")
                .expect("expected client request");
            write_frame_line(
                &mut writer,
                &format!(r#"{{"pong":{line}}}"#),
                DEFAULT_MAX_IPC_LINE_BYTES,
            )
            .expect("server write reply via cloned handle");
            writer.drain().expect("server drain must succeed");
        });

        let client = LocalIpcStream::connect(&sock_path).expect("connect clone test client");
        let mut client_writer = client.try_clone().expect("client try_clone must succeed");
        let mut client_reader = BufReader::new(client);

        let writer_thread = thread::spawn(move || {
            write_frame_line(
                &mut client_writer,
                r#"{"ping":42}"#,
                DEFAULT_MAX_IPC_LINE_BYTES,
            )
            .expect("client write via cloned handle");
        });

        let reply = read_frame_line(&mut client_reader, DEFAULT_MAX_IPC_LINE_BYTES)
            .expect("client read reply")
            .expect("expected reply from server");
        assert_eq!(
            reply, r#"{"pong":{"ping":42}}"#,
            "cloned stream reader/writer round-trip mismatch"
        );

        writer_thread
            .join()
            .expect("client writer thread must not panic");
        server.join().expect("server thread must not panic");
    }

    #[test]
    fn frame_line_reader_preserves_partial_frame_and_oversized_discard_across_timeouts() {
        use std::collections::VecDeque;
        use std::io::{self, Read};

        struct ChunkedTimeoutReader {
            steps: VecDeque<io::Result<&'static [u8]>>,
        }

        impl Read for ChunkedTimeoutReader {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                match self.steps.pop_front() {
                    Some(Ok(chunk)) => {
                        let n = chunk.len().min(buf.len());
                        buf[..n].copy_from_slice(&chunk[..n]);
                        if n < chunk.len() {
                            self.steps.push_front(Ok(&chunk[n..]));
                        }
                        Ok(n)
                    }
                    Some(Err(err)) => Err(err),
                    None => Ok(0),
                }
            }
        }

        let source = ChunkedTimeoutReader {
            steps: VecDeque::from([
                Ok(&b"{\"id\":1,"[..]),
                Err(io::Error::new(io::ErrorKind::TimedOut, "poll tick")),
                Ok(&b"\"ok\":true}\n"[..]),
                Ok(&b"0123456789abcdef"[..]),
                Err(io::Error::new(io::ErrorKind::TimedOut, "mid-discard")),
                Ok(&b"tail\n{\"after\":2}\n"[..]),
            ]),
        };
        let mut reader = BufReader::with_capacity(8, source);
        let mut frame_reader = super::FrameLineReader::new();

        // First call reads partial bytes then surfaces TimedOut without dropping them.
        let err = frame_reader
            .read_line(&mut reader, 64)
            .expect_err("expected TimedOut on partial frame");
        assert!(
            matches!(err, OsError::Io { ref source, .. } if source.kind() == io::ErrorKind::TimedOut),
            "expected TimedOut, got {err:?}"
        );

        // Retry completes the exact frame with zero byte loss.
        let line = frame_reader
            .read_line(&mut reader, 64)
            .expect("retry after timeout must finish partial frame")
            .expect("expected complete frame");
        assert_eq!(line, r#"{"id":1,"ok":true}"#);

        // Oversized line interrupted by TimedOut mid-discard preserves discard mode.
        let discard_timeout = frame_reader
            .read_line(&mut reader, 12)
            .expect_err("expected TimedOut during oversized discard");
        assert!(
            matches!(discard_timeout, OsError::Io { ref source, .. } if source.kind() == io::ErrorKind::TimedOut),
            "expected TimedOut during discard, got {discard_timeout:?}"
        );

        let too_large = frame_reader
            .read_line(&mut reader, 12)
            .expect_err("expected FrameTooLarge once oversized newline arrives");
        assert!(
            matches!(too_large, OsError::FrameTooLarge { max_bytes: 12 }),
            "expected FrameTooLarge(12), got {too_large:?}"
        );

        let after = frame_reader
            .read_line(&mut reader, 64)
            .expect("subsequent valid frame must decode cleanly")
            .expect("expected subsequent frame");
        assert_eq!(after, r#"{"after":2}"#);
    }

    #[test]
    fn stream_timeouts_reject_zero_and_accept_sub_millisecond_durations() {
        use std::io::ErrorKind;
        use std::time::Duration;

        #[cfg_attr(
            windows,
            expect(
                unused_variables,
                reason = "Windows uses a named pipe, not a socket file"
            )
        )]
        let tmp = TestIpcDir::new("to");
        #[cfg(unix)]
        let sock_path = tmp.path.join("run").join("to.sock");
        #[cfg(windows)]
        let sock_path = PathBuf::from(format!(
            r"\\.\pipe\arb-os-ipc-timeout-{}-{}",
            std::process::id(),
            IPC_SEQ.fetch_add(1, Ordering::Relaxed)
        ));

        let listener = LocalIpcListener::bind(&sock_path).expect("bind timeout test listener");
        let server = thread::spawn(move || listener.accept().expect("accept client"));

        let client = LocalIpcStream::connect(&sock_path).expect("connect timeout test client");
        let _server_stream = server.join().expect("server thread must not panic");

        let zero_read = client
            .set_read_timeout(Some(Duration::ZERO))
            .expect_err("zero read timeout must be rejected");
        assert!(
            matches!(zero_read, OsError::Ipc { ref source, .. } if source.kind() == ErrorKind::InvalidInput),
            "expected InvalidInput for zero read timeout, got {zero_read:?}"
        );

        let zero_write = client
            .set_write_timeout(Some(Duration::ZERO))
            .expect_err("zero write timeout must be rejected");
        assert!(
            matches!(zero_write, OsError::Ipc { ref source, .. } if source.kind() == ErrorKind::InvalidInput),
            "expected InvalidInput for zero write timeout, got {zero_write:?}"
        );

        client
            .set_read_timeout(Some(Duration::from_micros(500)))
            .expect("sub-millisecond non-zero read timeout must succeed");
        client
            .set_write_timeout(Some(Duration::from_micros(500)))
            .expect("sub-millisecond non-zero write timeout must succeed");
        client
            .set_read_timeout(None)
            .expect("clearing read timeout must succeed");
        client
            .set_write_timeout(None)
            .expect("clearing write timeout must succeed");
    }
}
