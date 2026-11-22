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

    /// Sets an optional read timeout on the stream where supported by the OS.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::Ipc`] if configuring the timeout fails.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> Result<(), OsError> {
        self.inner.set_read_timeout(timeout, &self.endpoint)
    }

    /// Sets an optional write timeout on the stream where supported by the OS.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::Ipc`] if configuring the timeout fails.
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

/// Reads a single newline-delimited UTF-8 frame without allocating more than
/// `max_line_bytes` bytes.
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
    let mut line_bytes = Vec::new();
    loop {
        let available = match reader.fill_buf() {
            Ok(buf) => buf,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(OsError::Io {
                    operation: "read_frame_line",
                    source,
                });
            }
        };

        if available.is_empty() {
            if line_bytes.is_empty() {
                return Ok(None);
            }
            break;
        }

        if let Some(newline_idx) = available.iter().position(|&b| b == b'\n') {
            let chunk = &available[..newline_idx];
            if line_bytes.len().saturating_add(chunk.len()) > max_line_bytes {
                // Consume through newline to keep framing state bounded if caller recovers.
                let to_consume = newline_idx + 1;
                reader.consume(to_consume);
                return Err(OsError::FrameTooLarge {
                    max_bytes: max_line_bytes,
                });
            }
            line_bytes.extend_from_slice(chunk);
            reader.consume(newline_idx + 1);
            break;
        }

        if line_bytes.len().saturating_add(available.len()) > max_line_bytes {
            let len = available.len();
            reader.consume(len);
            return Err(OsError::FrameTooLarge {
                max_bytes: max_line_bytes,
            });
        }

        let len = available.len();
        line_bytes.extend_from_slice(available);
        reader.consume(len);
    }

    if line_bytes.last() == Some(&b'\r') {
        line_bytes.pop();
    }

    let text = String::from_utf8(line_bytes).map_err(|source| OsError::InvalidUtf8 { source })?;
    Ok(Some(text))
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
    use std::ffi::OsStr;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr;
    use std::sync::Mutex;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_BROKEN_PIPE, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ,
        GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, OPEN_EXISTING,
        PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
        WaitNamedPipeW,
    };

    use crate::error::OsError;

    const PIPE_PREFIX: &str = r"\\.\pipe\";
    const PIPE_BUFFER_BYTES: u32 = 64 * 1024;
    const PIPE_CONNECT_WAIT_MS: u32 = 2_000;
    const PIPE_CONNECT_RETRIES: usize = 3;

    #[derive(Debug)]
    struct OwnedPipeHandle {
        raw: HANDLE,
        is_server_instance: bool,
    }

    // SAFETY: Windows pipe `HANDLE`s are kernel object handles that are not tied
    // to a specific thread and may be moved between threads safely.
    unsafe impl Send for OwnedPipeHandle {}

    impl Drop for OwnedPipeHandle {
        fn drop(&mut self) {
            if self.is_server_instance {
                // SAFETY: `self.raw` is a valid server-side named pipe handle.
                let _ = unsafe { DisconnectNamedPipe(self.raw) };
            }
            // SAFETY: `self.raw` is a valid owned Windows handle closed exactly once.
            let _ = unsafe { CloseHandle(self.raw) };
        }
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
        let open_mode = match mode {
            PipeInstanceMode::First => PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PipeInstanceMode::Additional => PIPE_ACCESS_DUPLEX,
        };
        let pipe_mode =
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
        // SAFETY: `wide_name` is a NUL-terminated UTF-16 string and `ptr::null()`
        // uses the calling user's default security descriptor.
        let raw = unsafe {
            CreateNamedPipeW(
                wide_name.as_ptr(),
                open_mode,
                pipe_mode,
                PIPE_UNLIMITED_INSTANCES,
                PIPE_BUFFER_BYTES,
                PIPE_BUFFER_BYTES,
                0,
                ptr::null(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(OwnedPipeHandle {
            raw,
            is_server_instance: true,
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

            // SAFETY: `handle.raw` is a valid synchronous server-side named pipe handle.
            let ok = unsafe { ConnectNamedPipe(handle.raw, ptr::null_mut()) };
            if ok == 0 {
                // SAFETY: `GetLastError` reads the calling thread's last-error slot.
                let code = unsafe { GetLastError() };
                if code != ERROR_PIPE_CONNECTED {
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

            Ok(Stream { handle })
        }
    }

    #[derive(Debug)]
    pub(super) struct Stream {
        handle: OwnedPipeHandle,
    }

    impl Stream {
        pub(super) fn connect(path: &Path) -> Result<Self, OsError> {
            let wide_name = encode_pipe_name(path)?;
            for attempt in 0..=PIPE_CONNECT_RETRIES {
                // SAFETY: `wide_name` is a NUL-terminated UTF-16 pipe name.
                let raw = unsafe {
                    CreateFileW(
                        wide_name.as_ptr(),
                        GENERIC_READ | GENERIC_WRITE,
                        0,
                        ptr::null(),
                        OPEN_EXISTING,
                        0,
                        ptr::null_mut(),
                    )
                };
                if raw != INVALID_HANDLE_VALUE {
                    return Ok(Self {
                        handle: OwnedPipeHandle {
                            raw,
                            is_server_instance: false,
                        },
                    });
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

        pub(super) fn set_read_timeout(
            &self,
            _timeout: Option<Duration>,
            _endpoint: &Path,
        ) -> Result<(), OsError> {
            Ok(())
        }

        pub(super) fn set_write_timeout(
            &self,
            _timeout: Option<Duration>,
            _endpoint: &Path,
        ) -> Result<(), OsError> {
            Ok(())
        }

        pub(super) fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let to_read = u32::try_from(buf.len()).unwrap_or(u32::MAX);
            let mut bytes_read: u32 = 0;
            // SAFETY: `self.handle.raw` is a valid pipe handle, `buf` is
            // writable for `to_read` bytes, and `bytes_read` is valid stack storage.
            let ok = unsafe {
                ReadFile(
                    self.handle.raw,
                    buf.as_mut_ptr(),
                    to_read,
                    &raw mut bytes_read,
                    ptr::null_mut(),
                )
            };
            if ok == 0 {
                // SAFETY: `GetLastError` reads the calling thread's last-error slot.
                let code = unsafe { GetLastError() };
                if code == ERROR_BROKEN_PIPE {
                    return Ok(0);
                }
                return Err(io::Error::last_os_error());
            }
            usize::try_from(bytes_read)
                .map_err(|_| io::Error::other("ReadFile byte count overflowed usize"))
        }

        pub(super) fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let to_write = u32::try_from(buf.len()).unwrap_or(u32::MAX);
            let mut bytes_written: u32 = 0;
            // SAFETY: `self.handle.raw` is a valid pipe handle, `buf` is
            // readable for `to_write` bytes, and `bytes_written` is valid stack storage.
            let ok = unsafe {
                WriteFile(
                    self.handle.raw,
                    buf.as_ptr(),
                    to_write,
                    &raw mut bytes_written,
                    ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            usize::try_from(bytes_written)
                .map_err(|_| io::Error::other("WriteFile byte count overflowed usize"))
        }

        pub(super) fn flush(&mut self) -> io::Result<()> {
            // SAFETY: `self.handle.raw` is a valid open pipe handle.
            let ok = unsafe { FlushFileBuffers(self.handle.raw) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
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
}
