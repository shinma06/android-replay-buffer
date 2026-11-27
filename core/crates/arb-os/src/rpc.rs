//! Local IPC JSON-RPC 2.0 client and optional `arbd` auto-spawner for `arb-cli`.

use std::fs::{self, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::ipc::{DEFAULT_MAX_IPC_LINE_BYTES, LocalIpcStream, read_frame_line, write_frame_line};
use crate::paths::ArbPaths;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::process_os::configure_detached_daemon;

/// Control-plane IPC protocol version required by this `arb-cli` binary (SPEC-01 §1 / R3 S16c).
pub const EXPECTED_IPC_PROTOCOL_VERSION: u32 = 1;

const DEFAULT_AUTO_START_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Budget for methods that synchronously write bundles, run `adb`, or inspect media/timeline.
const DEFAULT_LONG_CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// Budget for heavy re-encode exports (`export.trim`, `export.share_mp4`) and full-window audio
/// PCM decoding (`media.decode_audio_pcm`), which can legitimately exceed 120 s on a 300 s clip.
const DEFAULT_EXPORT_CALL_TIMEOUT: Duration = Duration::from_mins(10);
/// Minimum budget for the last-chance hello after the spawned daemon lost the lock race.
const FINAL_HELLO_MIN_TIMEOUT: Duration = Duration::from_secs(1);

/// Cheap control-plane methods that only touch in-memory state or small catalog rows. Every other
/// method (saves, capture control, `adb`, GC, bundle opening, timeline/media decoding, export)
/// may legitimately outlast [`DEFAULT_CALL_TIMEOUT`]; timing one out would report a failure while
/// the daemon still completes it, inviting a duplicate.
fn is_quick_method(method: &str) -> bool {
    matches!(
        method,
        "system.hello"
            | "system.status"
            | "system.shutdown"
            | "config.get"
            | "save.list_jobs"
            | "save.discard"
            | "bundle.list"
            | "bundle.lease_renew"
            | "bundle.close"
            | "bundle.forget"
            | "edit.get_session"
            | "edit.update_session"
    )
}

/// Heavy media re-encode / full-window audio decode methods that can take several minutes on a
/// 300 s 1080p30 clip (e.g. `burn_taps=true` software H.264 re-encode).
fn is_export_method(method: &str) -> bool {
    matches!(
        method,
        "export.trim" | "export.share_mp4" | "media.decode_audio_pcm"
    )
}
const AUTO_START_POLL: Duration = Duration::from_millis(50);
const LOG_TAIL_MAX_BYTES: usize = 2_048;

/// Controls whether connecting to `arbd` should auto-spawn the sibling `arbd` binary.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoStartMode {
    /// Fail immediately if `arbd` is not already listening.
    Disabled,
    /// Spawn sibling `arbd` if the IPC endpoint is unavailable and wait for `system.hello`.
    Enabled,
}

/// Default application code for a tool or RPC failure without a more specific cause.
#[must_use]
pub const fn error_label(rpc_code: i32) -> &'static str {
    match rpc_code {
        -32602 => "INVALID_PARAMS",
        -32001 => "BUNDLE_NOT_FOUND",
        -32002 => "CONFLICT",
        -32601 => "DAEMON_UNAVAILABLE",
        _ => "OPERATION_FAILED",
    }
}

/// Stable category, recovery hint and retry policy for a tool error code.
#[must_use]
pub fn error_guidance(label: &str) -> (&'static str, &'static str, bool) {
    match label {
        "NO_VIDEO" => (
            "invalid_params",
            "映像を含むバンドルを指定してください。",
            false,
        ),
        "INVALID_PARAMS" => (
            "invalid_params",
            "ツールの引数名、型、値の範囲を確認してください。",
            false,
        ),
        "ALREADY_TRASHED" => (
            "conflict",
            "既にごみ箱にあります。復元には arb_restore_bundle を使います。",
            false,
        ),
        "ALREADY_ACTIVE" => (
            "conflict",
            "既に有効です。arb_list_bundles で確認できます。",
            false,
        ),
        "CONFLICT" => (
            "conflict",
            "対象の現在の状態と出力先を確認してください。",
            false,
        ),
        "DEVICE_NOT_FOUND" => (
            "not_found",
            "端末を接続し、arb_devices でシリアルと接続状態を確認してください。",
            true,
        ),
        "BUNDLE_NOT_FOUND" => (
            "not_found",
            "対象のバンドル ID またはパスを確認してください。",
            false,
        ),
        "FAILED_RETRYABLE" => (
            "save_failed",
            "保存ジョブを確認し、原因を解消して再試行してください。",
            true,
        ),
        "DAEMON_UNAVAILABLE" => (
            "daemon_unavailable",
            "デーモンの接続とバージョンを確認してください。",
            false,
        ),
        "CANCELLED" => (
            "cancelled",
            "必要に応じて処理の状態を確認してください。",
            false,
        ),
        _ => (
            "operation_failed",
            "エラーの詳細と対象の状態を確認してください。",
            false,
        ),
    }
}

/// Errors returned by [`RpcClient`].
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum RpcClientError {
    /// OS IPC transport error.
    #[error("ipc error: {0}")]
    Os(#[from] crate::OsError),
    /// Standard I/O error (e.g., spawning `arbd`).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON encoding or decoding error.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// Server returned a JSON-RPC 2.0 error response.
    #[error("rpc error {code}: {message}")]
    Rpc {
        /// Application-specific error code from JSON-RPC error.data, when available.
        error_code: Option<String>,
        /// Structured daemon context, including the diagnostic cause when available.
        details: Option<Value>,
        /// Numeric JSON-RPC error code.
        code: i32,
        /// Human-readable server error message.
        message: String,
    },
    /// The peer did not complete the exchange within the call budget.
    #[error(
        "arbd IPC request timed out; connection closed (a save or export may still finish: \
         check save.list_jobs / bundle.list or the output location before retrying)"
    )]
    Timeout,
    /// `arbd` rejected a frame it could not attribute to a request (`id: null`, e.g. oversized or
    /// unparsable). The stream framing is suspect, so the connection is dropped.
    #[error("arbd rejected the request frame ({code}): {message}")]
    Rejected {
        /// JSON-RPC error code reported by the daemon.
        code: i32,
        /// Human-readable daemon error message.
        message: String,
    },
    /// The received message could not be matched to the outstanding request.
    #[error("invalid arbd response: {0}")]
    InvalidResponse(String),
    /// Server closed the stream before returning a response.
    #[error("arbd closed IPC connection before responding")]
    UnexpectedEof,
    /// An earlier timeout or protocol error already dropped this connection; nothing was sent.
    #[error("arbd IPC connection was closed after an earlier failure; reconnect")]
    Closed,
    /// `system.hello` reported a different `protocol_version` than expected (R3 S16c).
    #[error(
        "arbd protocol_version mismatch: expected {expected}, got {actual:?}; restart arbd or align arb-cli/arbd versions"
    )]
    ProtocolVersionMismatch {
        /// Version expected by `arb-cli`.
        expected: u32,
        /// Version reported by `arbd`, if present.
        actual: Option<u64>,
    },
    /// Auto-started `arbd` exited before its IPC endpoint became ready (R3 S16b).
    #[error("auto-started arbd exited prematurely ({status}); log `{log_path}`:\n{log_tail}")]
    DaemonExited {
        /// Child exit status description.
        status: String,
        /// Path to `<runtime_dir>/arbd.log`.
        log_path: PathBuf,
        /// Tail of `arbd.log` (or placeholder when empty).
        log_tail: String,
    },
    /// Auto-starting `arbd` timed out before `system.hello` succeeded.
    #[error("timed out waiting for auto-started arbd at `{endpoint}`")]
    AutoStartTimeout {
        /// Endpoint path that never became reachable.
        endpoint: PathBuf,
    },
}

impl RpcClientError {
    /// `true` when the transport itself is unusable (I/O, EOF, protocol desync, or a
    /// wrong-protocol daemon) and the connection must be dropped, as opposed to an
    /// application-level [`RpcClientError::Rpc`] error the daemon answered cleanly.
    #[must_use]
    pub const fn is_transport(&self) -> bool {
        !matches!(self, Self::Rpc { .. })
    }
}

#[derive(Debug, Serialize)]
struct OutgoingRequest<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: Value,
}

#[derive(Debug, Deserialize)]
struct IncomingError {
    data: Option<Value>,
    code: i32,
    message: String,
}

fn parse_response(raw: &str, id: u64) -> Result<Value, RpcClientError> {
    let mut response: serde_json::Map<String, Value> = serde_json::from_str(raw)?;
    // `arbd` answers frames it cannot attribute to a request (oversized, unparsable) with
    // `id: null`. Keep its code and message, but as a transport-level error: the connection is dropped.
    if response.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
        && response.get("id").is_some_and(Value::is_null)
        && !response.contains_key("result")
        && !response.contains_key("method")
        && let Some(error) = response.remove("error")
    {
        let error: IncomingError = serde_json::from_value(error)?;
        return Err(RpcClientError::Rejected {
            code: error.code,
            message: error.message,
        });
    }
    if response.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || response.get("id").and_then(Value::as_u64) != Some(id)
        || response.contains_key("method")
    {
        return Err(RpcClientError::InvalidResponse(format!(
            "expected JSON-RPC 2.0 response to request {id}"
        )));
    }
    match (response.remove("result"), response.remove("error")) {
        (Some(result), None) => Ok(result),
        (None, Some(error)) => {
            let error: IncomingError = serde_json::from_value(error)?;
            Err(RpcClientError::Rpc {
                details: error
                    .data
                    .as_ref()
                    .and_then(|d| d.get("details"))
                    .filter(|v| v.is_object())
                    .cloned(),
                error_code: error
                    .data
                    .and_then(|d| d["error_code"].as_str().map(str::to_owned)),
                code: error.code,
                message: error.message,
            })
        }
        _ => Err(RpcClientError::InvalidResponse(
            "expected exactly one of result or error".into(),
        )),
    }
}

// Recompute the remaining budget at each syscall so a partial NDJSON line
// arriving one byte at a time cannot renew the deadline indefinitely.
#[derive(Debug)]
struct DeadlineStream {
    stream: LocalIpcStream,
    started: Instant,
    timeout: Duration,
}

/// macOS rejects `SO_RCVTIMEO` / `SO_SNDTIMEO` with `EINVAL` once the peer has closed its end, even
/// while the daemon's final reply is still buffered. The read or write that follows completes or
/// fails immediately in that state, so the missing deadline cannot hang; do not lose the reply.
fn tolerate_closed_peer(result: Result<(), crate::OsError>) -> io::Result<()> {
    match result {
        Err(crate::OsError::Ipc { source, .. }) if source.kind() == io::ErrorKind::InvalidInput => {
            Ok(())
        }
        other => other.map_err(io::Error::other),
    }
}

impl DeadlineStream {
    fn remaining(&self) -> io::Result<Duration> {
        self.timeout
            .checked_sub(self.started.elapsed())
            .filter(|t| !t.is_zero())
            .ok_or_else(|| io::ErrorKind::TimedOut.into())
    }
}

impl Read for DeadlineStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        tolerate_closed_peer(self.stream.set_read_timeout(Some(self.remaining()?)))?;
        self.stream.read(bytes)
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        tolerate_closed_peer(self.stream.set_write_timeout(Some(self.remaining()?)))?;
        self.stream.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// Persistent bidirectional JSON-RPC 2.0 client connected to `arbd` over local IPC.
#[derive(Debug)]
pub struct RpcClient {
    connection: Option<BufReader<DeadlineStream>>,
    next_id: u64,
    default_timeout: Duration,
    long_timeout: Duration,
    export_timeout: Duration,
}

/// Resolves the auto-start timeout from `ARB_AUTO_START_TIMEOUT_MS` (default 5000 ms, R3 S16d).
pub fn auto_start_timeout() -> Duration {
    auto_start_timeout_from_str(std::env::var("ARB_AUTO_START_TIMEOUT_MS").ok().as_deref())
}

fn auto_start_timeout_from_str(raw: Option<&str>) -> Duration {
    let ms = raw
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_AUTO_START_TIMEOUT_MS);
    Duration::from_millis(ms)
}

/// Resolves the budget for long-running RPCs from `ARB_RPC_LONG_TIMEOUT_MS` (default 120 s), so a
/// slow save or device operation is not reported as failed while `arbd` still completes it.
fn long_call_timeout() -> Duration {
    long_call_timeout_from_str(std::env::var("ARB_RPC_LONG_TIMEOUT_MS").ok().as_deref())
}

fn long_call_timeout_from_str(raw: Option<&str>) -> Duration {
    raw.and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&v| v > 0)
        .map_or(DEFAULT_LONG_CALL_TIMEOUT, Duration::from_millis)
}

/// Resolves the budget for heavy export / audio-decode RPCs from `ARB_RPC_EXPORT_TIMEOUT_MS`
/// (default `max(600 s, long_timeout)`), so a 300 s `burn_taps` re-encode does not time out while
/// `arbd` is still writing the output.
fn export_call_timeout(long_timeout: Duration) -> Duration {
    export_call_timeout_from_str(
        std::env::var("ARB_RPC_EXPORT_TIMEOUT_MS").ok().as_deref(),
        long_timeout,
    )
}

fn export_call_timeout_from_str(raw: Option<&str>, long_timeout: Duration) -> Duration {
    raw.and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&v| v > 0)
        .map_or_else(
            || DEFAULT_EXPORT_CALL_TIMEOUT.max(long_timeout),
            Duration::from_millis,
        )
}

impl RpcClient {
    /// Connects to `arbd` at `paths.ipc_endpoint()`, optionally auto-starting the sibling `arbd`
    /// binary when `auto_start == AutoStartMode::Enabled`, and verifies `protocol_version`.
    ///
    /// # Errors
    ///
    /// Returns [`RpcClientError`] if connecting, auto-starting, or `system.hello` verification
    /// fails.
    pub fn connect(paths: &ArbPaths, auto_start: AutoStartMode) -> Result<Self, RpcClientError> {
        let endpoint = paths.ipc_endpoint();
        match Self::connect_endpoint(&endpoint) {
            Ok(mut client) => {
                let _ = client.verify_hello()?;
                Ok(client)
            }
            Err(first_err) => {
                if auto_start == AutoStartMode::Disabled {
                    return Err(first_err);
                }
                let (mut child, log_path) = spawn_daemon_process(paths)?;
                wait_for_daemon(
                    &endpoint,
                    auto_start_timeout(),
                    Some(&mut child),
                    Some(&log_path),
                )
            }
        }
    }

    /// Connects directly to an already-listening `arbd` IPC endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`RpcClientError::Os`] if the endpoint cannot be connected.
    pub fn connect_endpoint(endpoint: &Path) -> Result<Self, RpcClientError> {
        let stream = LocalIpcStream::connect(endpoint)?;
        let connection = BufReader::new(DeadlineStream {
            stream,
            started: Instant::now(),
            timeout: Duration::ZERO,
        });
        let long_timeout = long_call_timeout();
        Ok(Self {
            connection: Some(connection),
            next_id: 1,
            default_timeout: DEFAULT_CALL_TIMEOUT,
            long_timeout,
            export_timeout: export_call_timeout(long_timeout),
        })
    }

    /// Sets the budget for quick control-plane RPCs on this connection. Saves, capture control,
    /// device listing and GC use [`Self::set_long_timeout`] (120 s by default), while heavy
    /// exports and full-window audio decoding use [`Self::set_export_timeout`] (600 s by default);
    /// callers can override per call with [`Self::call_with_timeout`].
    pub const fn set_default_timeout(&mut self, timeout: Duration) {
        self.default_timeout = timeout;
    }

    /// Sets the budget for long-running methods (saves, capture control, GC, timeline, ...).
    pub const fn set_long_timeout(&mut self, timeout: Duration) {
        self.long_timeout = timeout;
    }

    /// Sets the budget for heavy export / audio-decode methods (`export.trim`, `export.share_mp4`,
    /// `media.decode_audio_pcm`).
    pub const fn set_export_timeout(&mut self, timeout: Duration) {
        self.export_timeout = timeout;
    }

    /// Calls `system.hello` and verifies that `protocol_version` matches
    /// [`EXPECTED_IPC_PROTOCOL_VERSION`] (R3 S16c).
    ///
    /// # Errors
    ///
    /// Returns [`RpcClientError::ProtocolVersionMismatch`] if the daemon reports an
    /// incompatible protocol version, or transport/RPC errors.
    pub fn verify_hello(&mut self) -> Result<Value, RpcClientError> {
        self.verify_hello_with_timeout(self.default_timeout)
    }

    fn verify_hello_with_timeout(&mut self, timeout: Duration) -> Result<Value, RpcClientError> {
        let hello = self.call_with_timeout("system.hello", json!({}), timeout)?;
        let actual = hello.get("protocol_version").and_then(Value::as_u64);
        if actual != Some(u64::from(EXPECTED_IPC_PROTOCOL_VERSION)) {
            return Err(RpcClientError::ProtocolVersionMismatch {
                expected: EXPECTED_IPC_PROTOCOL_VERSION,
                actual,
            });
        }
        Ok(hello)
    }

    /// Invokes a JSON-RPC 2.0 method and returns its decoded `result` JSON [`Value`].
    ///
    /// # Errors
    ///
    /// Returns [`RpcClientError`] on transport, serialization, or server-reported RPC errors.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcClientError> {
        // Older daemons ignore unknown GC parameters. Verify this socket before any preview.
        if method == "bundle.gc"
            && params["dry_run"] == true
            && self.verify_hello()?["gc_dry_run"] != true
        {
            return Err(RpcClientError::Rpc {
                details: None,
                error_code: None,
                code: -32601,
                message: "GC プレビューには arbd の更新が必要です".into(),
            });
        }
        if matches!(
            method,
            "edit.update_session" | "export.trim" | "export.share_mp4"
        ) && (!params["in_time_ns"].is_null() || !params["out_time_ns"].is_null())
            && self.verify_hello()?["realtime_trim"] != true
        {
            return Err(RpcClientError::Rpc {
                details: None,
                error_code: None,
                code: -32601,
                message: "実時間トリムには arbd の更新が必要です".into(),
            });
        }
        // Older daemons eagerly catalog ranged MP4 exports and ignore dest_dir for generated names.
        if method == "export.share_mp4"
            && (params["in_frame_index"].is_u64()
                || params["keep_bundle"] == true
                || (params["dest_dir"].is_string() && params["output_path"].is_null()))
            && self.verify_hello()?["standalone_share_trim"] != true
        {
            return Err(RpcClientError::Rpc {
                details: None,
                error_code: None,
                code: -32601,
                message: "共有 MP4 の出力方式を選ぶには arbd の更新が必要です".into(),
            });
        }
        let timeout = if is_quick_method(method) {
            self.default_timeout
        } else if is_export_method(method) {
            self.export_timeout
        } else {
            self.long_timeout
        };
        self.call_with_timeout(method, params, timeout)
    }

    /// Executes one request within a total read/write budget. A transport or protocol failure
    /// drops the connection. Callers must reconnect explicitly before issuing another request.
    ///
    /// # Errors
    /// Returns a timeout, transport, protocol, or daemon-reported application error.
    pub fn call_with_timeout(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, RpcClientError> {
        if self.connection.is_none() {
            return Err(RpcClientError::Closed);
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let req = OutgoingRequest {
            jsonrpc: "2.0",
            id,
            method,
            params,
        };
        let line = serde_json::to_string(&req)?;
        let Some(mut connection) = self.connection.take() else {
            return Err(RpcClientError::Closed);
        };
        connection.get_mut().started = Instant::now();
        connection.get_mut().timeout = timeout;
        let written = write_frame_line(connection.get_mut(), &line, DEFAULT_MAX_IPC_LINE_BYTES);
        if let Err(
            err @ (crate::OsError::FrameTooLarge { .. } | crate::OsError::InvalidArgument { .. }),
        ) = written
        {
            // Rejected locally before any byte was written: the connection is still clean.
            self.connection = Some(connection);
            return Err(err.into());
        }
        let result = (|| {
            written?;
            let raw = read_frame_line(&mut connection, DEFAULT_MAX_IPC_LINE_BYTES)?
                .ok_or(RpcClientError::UnexpectedEof)?;
            parse_response(&raw, id)
        })();
        match result {
            Err(RpcClientError::Os(crate::OsError::Io { source, .. }))
                if matches!(
                    source.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                Err(RpcClientError::Timeout)
            }
            Ok(_) | Err(RpcClientError::Rpc { .. }) => {
                self.connection = Some(connection);
                result
            }
            _ => result,
        }
    }
}

/// Spawns a detached sibling `arbd` binary configured with `paths` (R3 S16a/b).
///
/// The child runs in its own process group so terminal signals (`SIGINT`/`SIGHUP`) delivered
/// to `arb-cli` do not kill the daemon, and its `stderr` is appended to
/// `<runtime_dir>/arbd.log` so startup failures are diagnosable.
///
/// # Errors
///
/// Returns [`RpcClientError::Io`] if creating the log file or spawning `arbd` fails.
pub fn spawn_daemon_process(paths: &ArbPaths) -> Result<(Child, PathBuf), RpcClientError> {
    fs::create_dir_all(&paths.runtime_dir)?;
    let log_path = paths.runtime_dir.join("arbd.log");
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    let arbd_bin = locate_arbd_binary();
    let mut cmd = Command::new(&arbd_bin);
    cmd.arg("--data-dir")
        .arg(&paths.data_dir)
        .arg("--cache-dir")
        .arg(&paths.cache_dir)
        .arg("--runtime-dir")
        .arg(&paths.runtime_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log_file));
    configure_detached_daemon(&mut cmd);
    let child = cmd.spawn()?;
    Ok((child, log_path))
}

/// Polls `endpoint` until `system.hello` succeeds with a matching `protocol_version`,
/// `child` exits prematurely, or `timeout` elapses (R3 S16b/c/d).
///
/// # Errors
///
/// Returns [`RpcClientError::ProtocolVersionMismatch`], [`RpcClientError::DaemonExited`], or
/// [`RpcClientError::AutoStartTimeout`].
pub fn wait_for_daemon(
    endpoint: &Path,
    timeout: Duration,
    mut child: Option<&mut Child>,
    log_path: Option<&Path>,
) -> Result<RpcClient, RpcClientError> {
    let started = Instant::now();
    loop {
        if let Ok(mut client) = RpcClient::connect_endpoint(endpoint) {
            match client.verify_hello_with_timeout(timeout.saturating_sub(started.elapsed())) {
                Ok(_) => return Ok(client),
                Err(err @ RpcClientError::ProtocolVersionMismatch { .. }) => return Err(err),
                Err(_) => {}
            }
        }
        if let Some(ch) = child.as_deref_mut()
            && let Ok(Some(status)) = ch.try_wait()
        {
            // A concurrent CLI auto-start race can cause our child to lose the lock and exit
            // while the winner is now serving; try connecting one final time before failing.
            if let Ok(mut client) = RpcClient::connect_endpoint(endpoint)
                && client
                    .verify_hello_with_timeout(
                        timeout
                            .saturating_sub(started.elapsed())
                            .max(FINAL_HELLO_MIN_TIMEOUT),
                    )
                    .is_ok()
            {
                return Ok(client);
            }
            let log_file = log_path.map_or_else(|| PathBuf::from("<unknown>"), Path::to_path_buf);
            let log_tail = log_path.map_or_default(read_log_tail);
            return Err(RpcClientError::DaemonExited {
                status: status.to_string(),
                log_path: log_file,
                log_tail,
            });
        }
        if started.elapsed() >= timeout {
            return Err(RpcClientError::AutoStartTimeout {
                endpoint: endpoint.to_path_buf(),
            });
        }
        thread::sleep(AUTO_START_POLL.min(timeout.saturating_sub(started.elapsed())));
    }
}

fn read_log_tail(path: &Path) -> String {
    let Ok(bytes) = fs::read(path) else {
        return "<log unavailable>".to_owned();
    };
    let start = bytes.len().saturating_sub(LOG_TAIL_MAX_BYTES);
    let text = String::from_utf8_lossy(&bytes[start..]).trim().to_owned();
    if text.is_empty() {
        "<empty>".to_owned()
    } else {
        text
    }
}

/// Resolves the configured, sibling, or PATH daemon executable.
#[must_use]
pub fn locate_arbd_binary() -> PathBuf {
    if let Some(explicit) =
        std::env::var_os("ARB_DAEMON_PATH").or_else(|| std::env::var_os("ARB_DAEMON_BIN"))
    {
        let p = PathBuf::from(explicit);
        if p.is_file() {
            return p;
        }
    }
    let bin_name = format!("arbd{}", std::env::consts::EXE_SUFFIX);
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join(&bin_name);
        if sibling.is_file() {
            return sibling;
        }
    }
    PathBuf::from(bin_name)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::BufReader;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use crate::ipc::{
        DEFAULT_MAX_IPC_LINE_BYTES, LocalIpcListener, read_frame_line, write_frame_line,
    };
    use crate::paths::ArbPaths;
    use serde_json::json;

    use super::{
        EXPECTED_IPC_PROTOCOL_VERSION, RpcClient, RpcClientError, auto_start_timeout_from_str,
        wait_for_daemon,
    };

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            // Keep the dir name short: macOS temp dirs sit under a ~50-char /var/folders
            // prefix, and Unix socket binds fail once the full path exceeds SUN_LEN.
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let dir =
                std::env::temp_dir().join(format!("arbc-{label}-{}-{nanos:x}", std::process::id()));
            fs::create_dir_all(&dir).expect("create test temp dir");
            Self(dir)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn response_cases() -> Vec<(serde_json::Value, bool)> {
        vec![
            (json!({"jsonrpc":"2.0","id":2,"result":null}), false),
            (json!({"jsonrpc":"2.0","result":null}), false),
            (
                json!({"jsonrpc":"2.0","method":"notice","params":{}}),
                false,
            ),
            (json!({"jsonrpc":"2.0","id":1}), false),
            (json!({"jsonrpc":"2.0","id":1,"result":null}), true),
            (
                json!({"jsonrpc":"2.0","id":1,"result":null,"error":null}),
                false,
            ),
        ]
    }

    #[test]
    fn validates_response_identity_and_result_presence() {
        for (response, valid) in response_cases() {
            assert_eq!(
                super::parse_response(&response.to_string(), 1).is_ok(),
                valid,
                "{response}"
            );
        }
    }

    #[test]
    fn null_id_daemon_error_is_a_transport_level_rejection_with_its_code_and_message() {
        let raw =
            json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"frame too large"}})
                .to_string();
        assert!(matches!(
            super::parse_response(&raw, 1),
            Err(RpcClientError::Rejected { code: -32600, message }) if message == "frame too large"
        ));
    }

    #[test]
    fn malformed_ipc_responses_close_connection_but_explicit_null_succeeds() {
        for (response, valid) in response_cases() {
            let dir = TestDir::new("id");
            let endpoint = ArbPaths {
                data_dir: dir.0.clone(),
                cache_dir: dir.0.clone(),
                runtime_dir: dir.0.clone(),
            }
            .ipc_endpoint();
            let listener = LocalIpcListener::bind(&endpoint).expect("bind");
            let server = thread::spawn(move || {
                let stream = listener.accept().expect("accept");
                let mut reader = BufReader::new(stream);
                read_frame_line(&mut reader, DEFAULT_MAX_IPC_LINE_BYTES)
                    .expect("request")
                    .unwrap();
                write_frame_line(
                    reader.get_mut(),
                    &response.to_string(),
                    DEFAULT_MAX_IPC_LINE_BYTES,
                )
                .expect("reply");
                assert!(
                    read_frame_line(&mut reader, DEFAULT_MAX_IPC_LINE_BYTES)
                        .expect("eof")
                        .is_none()
                );
            });
            let mut client = RpcClient::connect_endpoint(&endpoint).expect("connect");
            let res = client.call("save.trigger", json!({}));
            if valid {
                assert_eq!(res.unwrap(), serde_json::Value::Null);
            } else {
                assert!(matches!(res, Err(RpcClientError::InvalidResponse(_))));
                assert!(matches!(
                    client.call("save.trigger", json!({})),
                    Err(RpcClientError::Closed)
                ));
            }
            drop(client);
            server.join().unwrap();
        }
    }

    #[test]
    fn silent_peer_times_out_and_mutation_is_not_replayed() {
        let dir = TestDir::new("slow");
        let endpoint = ArbPaths {
            data_dir: dir.0.clone(),
            cache_dir: dir.0.clone(),
            runtime_dir: dir.0.clone(),
        }
        .ipc_endpoint();
        let listener = LocalIpcListener::bind(&endpoint).expect("bind");
        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(4)))
                .unwrap();
            let mut reader = BufReader::new(stream);
            let req = read_frame_line(&mut reader, DEFAULT_MAX_IPC_LINE_BYTES)
                .unwrap()
                .unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&req).unwrap()["method"],
                "save.trigger"
            );
            assert!(
                read_frame_line(&mut reader, DEFAULT_MAX_IPC_LINE_BYTES)
                    .unwrap()
                    .is_none()
            );
        });
        let mut client = RpcClient::connect_endpoint(&endpoint).unwrap();
        let start = std::time::Instant::now();
        let error = client
            .call_with_timeout("save.trigger", json!({}), Duration::from_millis(250))
            .unwrap_err();
        assert!(matches!(error, RpcClientError::Timeout), "{error}");
        assert!(start.elapsed() < Duration::from_secs(3));
        assert!(matches!(
            client.call("system.status", json!({})),
            Err(RpcClientError::Closed)
        ));
        server.join().unwrap();
    }

    #[test]
    fn slow_save_uses_the_long_budget_while_status_keeps_the_short_one() {
        let dir = TestDir::new("long");
        let endpoint = ArbPaths {
            data_dir: dir.0.clone(),
            cache_dir: dir.0.clone(),
            runtime_dir: dir.0.clone(),
        }
        .ipc_endpoint();
        let listener = LocalIpcListener::bind(&endpoint).expect("bind");
        let server = thread::spawn(move || {
            let mut reader = BufReader::new(listener.accept().expect("accept"));
            for delay_ms in [400, 400] {
                let line = read_frame_line(&mut reader, DEFAULT_MAX_IPC_LINE_BYTES)
                    .unwrap()
                    .unwrap();
                let id = serde_json::from_str::<serde_json::Value>(&line).unwrap()["id"].clone();
                thread::sleep(Duration::from_millis(delay_ms));
                let response = json!({"jsonrpc":"2.0","id":id,"result":{"ok":true}});
                // The second reply may race the client's disconnect after its timeout.
                let _ = write_frame_line(
                    reader.get_mut(),
                    &response.to_string(),
                    DEFAULT_MAX_IPC_LINE_BYTES,
                );
            }
        });
        let mut client = RpcClient::connect_endpoint(&endpoint).unwrap();
        client.set_default_timeout(Duration::from_millis(150));
        client.set_long_timeout(Duration::from_secs(5));
        assert_eq!(
            client.call("save.trigger", json!({})).unwrap()["ok"],
            json!(true)
        );
        assert!(matches!(
            client.call("system.status", json!({})),
            Err(RpcClientError::Timeout)
        ));
        server.join().unwrap();
    }

    #[test]
    fn only_cheap_control_methods_use_the_short_budget() {
        for method in [
            "system.hello",
            "system.status",
            "config.get",
            "save.list_jobs",
            "bundle.list",
        ] {
            assert!(super::is_quick_method(method), "{method}");
        }
        for method in [
            "save.trigger",
            "save.logs",
            "save.retry",
            "save.retarget",
            "capture.start",
            "config.update",
            "device.list",
            "bundle.gc",
            "bundle.open",
            "timeline.overview",
            "media.inspect_frames",
            "media.open_viewport",
            "export.trim",
            "media.decode_audio_pcm",
            "future.method",
        ] {
            assert!(!super::is_quick_method(method), "{method}");
        }
    }

    #[test]
    fn long_and_export_timeout_env_overrides_parse_and_default() {
        assert_eq!(
            super::long_call_timeout_from_str(None),
            Duration::from_secs(120)
        );
        assert_eq!(
            super::long_call_timeout_from_str(Some(" 600000 ")),
            Duration::from_secs(600)
        );
        for bad in ["0", "-1", "abc", ""] {
            assert_eq!(
                super::long_call_timeout_from_str(Some(bad)),
                Duration::from_secs(120),
                "{bad}"
            );
        }
        assert_eq!(
            super::export_call_timeout_from_str(None, Duration::from_secs(120)),
            Duration::from_mins(10)
        );
        assert_eq!(
            super::export_call_timeout_from_str(None, Duration::from_mins(15)),
            Duration::from_mins(15),
            "raising long_timeout above 600 s must also raise the default export budget"
        );
        assert_eq!(
            super::export_call_timeout_from_str(Some(" 1800000 "), Duration::from_secs(120)),
            Duration::from_mins(30)
        );
        for bad in ["0", "-1", "abc", ""] {
            assert_eq!(
                super::export_call_timeout_from_str(Some(bad), Duration::from_secs(120)),
                Duration::from_mins(10),
                "{bad}"
            );
        }
    }

    #[test]
    fn export_and_audio_decode_use_the_export_budget_above_the_long_budget() {
        for method in ["export.trim", "export.share_mp4", "media.decode_audio_pcm"] {
            assert!(super::is_export_method(method), "{method}");
            assert!(!super::is_quick_method(method), "{method}");
        }
        for method in ["save.trigger", "export.png", "media.inspect_frames"] {
            assert!(!super::is_export_method(method), "{method}");
        }

        let dir = TestDir::new("exp");
        let endpoint = ArbPaths {
            data_dir: dir.0.clone(),
            cache_dir: dir.0.clone(),
            runtime_dir: dir.0.clone(),
        }
        .ipc_endpoint();
        let listener = LocalIpcListener::bind(&endpoint).expect("bind");
        let server = thread::spawn(move || {
            let mut reader = BufReader::new(listener.accept().expect("accept"));
            for delay_ms in [350, 350] {
                let line = read_frame_line(&mut reader, DEFAULT_MAX_IPC_LINE_BYTES)
                    .unwrap()
                    .unwrap();
                let id = serde_json::from_str::<serde_json::Value>(&line).unwrap()["id"].clone();
                thread::sleep(Duration::from_millis(delay_ms));
                let response = json!({"jsonrpc":"2.0","id":id,"result":{"ok":true}});
                let _ = write_frame_line(
                    reader.get_mut(),
                    &response.to_string(),
                    DEFAULT_MAX_IPC_LINE_BYTES,
                );
            }
        });
        let mut client = RpcClient::connect_endpoint(&endpoint).unwrap();
        client.set_default_timeout(Duration::from_millis(100));
        client.set_long_timeout(Duration::from_millis(150));
        client.set_export_timeout(Duration::from_secs(5));
        assert_eq!(
            client.call("export.share_mp4", json!({})).unwrap()["ok"],
            json!(true)
        );
        assert!(matches!(
            client.call("save.trigger", json!({})),
            Err(RpcClientError::Timeout)
        ));
        server.join().unwrap();
    }

    #[test]
    fn oversized_request_is_rejected_locally_without_dropping_the_connection() {
        let dir = TestDir::new("big");
        let endpoint = ArbPaths {
            data_dir: dir.0.clone(),
            cache_dir: dir.0.clone(),
            runtime_dir: dir.0.clone(),
        }
        .ipc_endpoint();
        let listener = LocalIpcListener::bind(&endpoint).expect("bind");
        let server = thread::spawn(move || {
            let mut reader = BufReader::new(listener.accept().expect("accept"));
            let line = read_frame_line(&mut reader, DEFAULT_MAX_IPC_LINE_BYTES)
                .unwrap()
                .unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "system.status");
            let response = json!({"jsonrpc":"2.0","id":request["id"],"result":{"ok":true}});
            write_frame_line(
                reader.get_mut(),
                &response.to_string(),
                DEFAULT_MAX_IPC_LINE_BYTES,
            )
            .unwrap();
            reader.get_mut().drain().unwrap();
        });
        let mut client = RpcClient::connect_endpoint(&endpoint).unwrap();
        let huge = "x".repeat(DEFAULT_MAX_IPC_LINE_BYTES + 1);
        assert!(matches!(
            client.call("save.trigger", json!({ "note": huge })),
            Err(RpcClientError::Os(crate::OsError::FrameTooLarge { .. }))
        ));
        assert_eq!(
            client.call("system.status", json!({})).unwrap()["ok"],
            json!(true)
        );
        server.join().unwrap();
    }

    #[test]
    fn null_id_error_surfaces_the_daemon_error_and_closes_the_connection() {
        let dir = TestDir::new("nullid");
        let endpoint = ArbPaths {
            data_dir: dir.0.clone(),
            cache_dir: dir.0.clone(),
            runtime_dir: dir.0.clone(),
        }
        .ipc_endpoint();
        let listener = LocalIpcListener::bind(&endpoint).expect("bind");
        let server = thread::spawn(move || {
            let mut reader = BufReader::new(listener.accept().expect("accept"));
            read_frame_line(&mut reader, DEFAULT_MAX_IPC_LINE_BYTES)
                .unwrap()
                .unwrap();
            let response =
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"bad frame"}});
            write_frame_line(
                reader.get_mut(),
                &response.to_string(),
                DEFAULT_MAX_IPC_LINE_BYTES,
            )
            .unwrap();
            reader.get_mut().drain().unwrap();
        });
        let mut client = RpcClient::connect_endpoint(&endpoint).unwrap();
        assert!(matches!(
            client.call("system.status", json!({})),
            Err(RpcClientError::Rejected { code: -32600, .. })
        ));
        assert!(matches!(
            client.call("system.status", json!({})),
            Err(RpcClientError::Closed)
        ));
        server.join().unwrap();
    }

    #[test]
    fn tolerate_closed_peer_ignores_only_invalid_input() {
        assert!(super::tolerate_closed_peer(Ok(())).is_ok());
        assert!(
            super::tolerate_closed_peer(Err(crate::OsError::Ipc {
                operation: "set_read_timeout",
                endpoint: "test".into(),
                source: std::io::Error::from(std::io::ErrorKind::InvalidInput),
            }))
            .is_ok()
        );
        assert!(
            super::tolerate_closed_peer(Err(crate::OsError::Ipc {
                operation: "set_read_timeout",
                endpoint: "test".into(),
                source: std::io::Error::from(std::io::ErrorKind::BrokenPipe),
            }))
            .is_err()
        );
    }

    #[test]
    fn reply_delivered_when_daemon_drains_and_closes() {
        let dir = TestDir::new("drain");
        let endpoint = ArbPaths {
            data_dir: dir.0.clone(),
            cache_dir: dir.0.clone(),
            runtime_dir: dir.0.clone(),
        }
        .ipc_endpoint();
        let listener = LocalIpcListener::bind(&endpoint).expect("bind");
        let server = thread::spawn(move || {
            let mut stream = listener.accept().expect("accept");
            write_frame_line(
                &mut stream,
                r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#,
                DEFAULT_MAX_IPC_LINE_BYTES,
            )
            .expect("reply");
            // On Windows named pipes `drain` waits until the client consumes the buffer before
            // `DisconnectNamedPipe` runs on drop, so the client reads concurrently below.
            stream.drain().expect("drain");
            drop(stream);
        });
        let stream = super::LocalIpcStream::connect(&endpoint).expect("connect");
        let mut connection = BufReader::new(super::DeadlineStream {
            stream,
            started: std::time::Instant::now(),
            timeout: Duration::from_secs(5),
        });
        let line = read_frame_line(&mut connection, DEFAULT_MAX_IPC_LINE_BYTES)
            .expect("drained reply must be readable across all OSes")
            .expect("reply line");
        assert!(line.contains(r#""ok":true"#), "{line}");
        server.join().unwrap();
    }

    // Unix-only facility: Unix domain sockets retain unread reply bytes in the client receive
    // buffer after peer close, and macOS rejects `SO_RCVTIMEO` on that socket with `EINVAL`.
    // Windows named pipes discard unread bytes on `DisconnectNamedPipe` unless `drain()` waits for
    // the reader (covered by `reply_delivered_when_daemon_drains_and_closes`).
    #[cfg(unix)]
    #[test]
    fn reply_buffered_before_the_daemon_closed_is_still_delivered() {
        let dir = TestDir::new("closed");
        let endpoint = ArbPaths {
            data_dir: dir.0.clone(),
            cache_dir: dir.0.clone(),
            runtime_dir: dir.0.clone(),
        }
        .ipc_endpoint();
        let listener = LocalIpcListener::bind(&endpoint).expect("bind");
        let (closed_tx, closed_rx) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let mut stream = listener.accept().expect("accept");
            write_frame_line(
                &mut stream,
                r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#,
                DEFAULT_MAX_IPC_LINE_BYTES,
            )
            .expect("reply");
            drop(stream);
            closed_tx.send(()).unwrap();
        });
        let stream = super::LocalIpcStream::connect(&endpoint).expect("connect");
        closed_rx.recv().unwrap();
        server.join().unwrap();
        let mut connection = BufReader::new(super::DeadlineStream {
            stream,
            started: std::time::Instant::now(),
            timeout: Duration::from_secs(5),
        });
        let line = read_frame_line(&mut connection, DEFAULT_MAX_IPC_LINE_BYTES)
            .expect("buffered reply must be readable after the peer closed")
            .expect("reply line");
        assert!(line.contains(r#""ok":true"#), "{line}");
    }

    #[test]
    fn partial_response_does_not_extend_call_budget() {
        use std::io::Write;
        let dir = TestDir::new("part");
        let endpoint = ArbPaths {
            data_dir: dir.0.clone(),
            cache_dir: dir.0.clone(),
            runtime_dir: dir.0.clone(),
        }
        .ipc_endpoint();
        let listener = LocalIpcListener::bind(&endpoint).unwrap();
        let server = thread::spawn(move || {
            let mut stream = BufReader::new(listener.accept().unwrap());
            read_frame_line(&mut stream, DEFAULT_MAX_IPC_LINE_BYTES)
                .unwrap()
                .unwrap();
            for _ in 0..50 {
                if stream.get_mut().write_all(b" ").is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(25));
            }
        });
        let mut client = RpcClient::connect_endpoint(&endpoint).unwrap();
        let result =
            client.call_with_timeout("system.status", json!({}), Duration::from_millis(250));
        assert!(matches!(result, Err(RpcClientError::Timeout)), "{result:?}");
        server.join().unwrap();
    }

    #[test]
    fn r3_s16_auto_start_timeout_parses_env_override_and_defaults() {
        assert_eq!(
            auto_start_timeout_from_str(None),
            Duration::from_millis(5_000),
            "missing env var must use 5s default"
        );
        assert_eq!(
            auto_start_timeout_from_str(Some("12500")),
            Duration::from_millis(12_500),
            "valid positive ms must override default"
        );
        assert_eq!(
            auto_start_timeout_from_str(Some("0")),
            Duration::from_millis(5_000),
            "zero ms must fall back to default"
        );
        assert_eq!(
            auto_start_timeout_from_str(Some("not-a-number")),
            Duration::from_millis(5_000),
            "invalid env var must fall back to default"
        );
    }

    #[test]
    fn r3_s16_verify_hello_rejects_protocol_version_mismatch() {
        let dir = TestDir::new("proto");
        let paths = ArbPaths {
            data_dir: dir.0.join("data"),
            cache_dir: dir.0.join("cache"),
            runtime_dir: dir.0.join("run"),
        };
        let endpoint = paths.ipc_endpoint();
        let listener = LocalIpcListener::bind(&endpoint).expect("bind mock IPC");

        let server = thread::spawn(move || {
            let stream = listener.accept().expect("accept");
            let mut writer = stream.try_clone().expect("clone");
            let mut reader = BufReader::new(stream);
            let _req = read_frame_line(&mut reader, DEFAULT_MAX_IPC_LINE_BYTES).expect("read");
            let resp = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": { "protocol_version": 99 }
            });
            write_frame_line(&mut writer, &resp.to_string(), DEFAULT_MAX_IPC_LINE_BYTES)
                .expect("write");
            let _ = writer.drain();
        });

        let mut client = RpcClient::connect_endpoint(&endpoint).expect("connect");
        let err = client
            .verify_hello()
            .expect_err("mismatched protocol_version must fail");
        assert!(
            matches!(
                err,
                RpcClientError::ProtocolVersionMismatch {
                    expected: EXPECTED_IPC_PROTOCOL_VERSION,
                    actual: Some(99),
                }
            ),
            "expected ProtocolVersionMismatch, got {err:?}"
        );
        server.join().expect("server thread");
    }

    #[test]
    fn r3_s16_wait_for_daemon_reports_early_exit_with_log_tail() {
        let dir = TestDir::new("exit");
        let endpoint = dir.0.join("never-created.sock");
        let log_path = dir.0.join("arbd.log");
        fs::write(&log_path, "arbd error: corrupt catalog\n").expect("write log");

        // Run a child that exits immediately (using the current test binary with --help).
        let exe = std::env::current_exe().expect("current exe");
        let mut child = Command::new(exe)
            .arg("--help")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn short-lived child");

        let err = wait_for_daemon(
            &endpoint,
            Duration::from_secs(5),
            Some(&mut child),
            Some(&log_path),
        )
        .expect_err("exited child must fail immediately without timing out");
        match err {
            RpcClientError::DaemonExited { log_tail, .. } => {
                assert!(
                    log_tail.contains("corrupt catalog"),
                    "error must include arbd.log tail: {log_tail}"
                );
            }
            other => panic!("expected DaemonExited, got {other:?}"),
        }
    }
}
