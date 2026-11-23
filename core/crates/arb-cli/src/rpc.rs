//! Local IPC JSON-RPC 2.0 client and optional `arbd` auto-spawner for `arb-cli`.

use std::fs::{self, OpenOptions};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use arb_os::ipc::{DEFAULT_MAX_IPC_LINE_BYTES, LocalIpcStream, read_frame_line, write_frame_line};
use arb_os::paths::ArbPaths;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::process_os::configure_detached_daemon;

/// Control-plane IPC protocol version required by this `arb-cli` binary (SPEC-01 §1 / R3 S16c).
pub(crate) const EXPECTED_IPC_PROTOCOL_VERSION: u32 = 1;

const DEFAULT_AUTO_START_TIMEOUT_MS: u64 = 5_000;
const AUTO_START_POLL: Duration = Duration::from_millis(50);
const LOG_TAIL_MAX_BYTES: usize = 2_048;

/// Controls whether connecting to `arbd` should auto-spawn the sibling `arbd` binary.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutoStartMode {
    /// Fail immediately if `arbd` is not already listening.
    Disabled,
    /// Spawn sibling `arbd` if the IPC endpoint is unavailable and wait for `system.hello`.
    Enabled,
}

/// Errors returned by [`RpcClient`].
#[non_exhaustive]
#[derive(Debug, Error)]
pub(crate) enum RpcClientError {
    /// OS IPC transport error.
    #[error("ipc error: {0}")]
    Os(#[from] arb_os::OsError),
    /// Standard I/O error (e.g., spawning `arbd`).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON encoding or decoding error.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// Server returned a JSON-RPC 2.0 error response.
    #[error("rpc error {code}: {message}")]
    Rpc {
        /// Numeric JSON-RPC error code.
        code: i32,
        /// Human-readable server error message.
        message: String,
    },
    /// Server closed the stream before returning a response.
    #[error("arbd closed IPC connection before responding")]
    UnexpectedEof,
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

#[derive(Debug, Serialize)]
struct OutgoingRequest<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: Value,
}

#[derive(Debug, Deserialize)]
struct IncomingError {
    code: i32,
    message: String,
}

#[derive(Debug, Deserialize)]
struct IncomingResponse {
    result: Option<Value>,
    error: Option<IncomingError>,
}

/// Persistent bidirectional JSON-RPC 2.0 client connected to `arbd` over local IPC.
#[derive(Debug)]
pub(crate) struct RpcClient {
    reader: BufReader<LocalIpcStream>,
    writer: LocalIpcStream,
    next_id: u64,
}

/// Resolves the auto-start timeout from `ARB_AUTO_START_TIMEOUT_MS` (default 5000 ms, R3 S16d).
pub(crate) fn auto_start_timeout() -> Duration {
    auto_start_timeout_from_str(std::env::var("ARB_AUTO_START_TIMEOUT_MS").ok().as_deref())
}

fn auto_start_timeout_from_str(raw: Option<&str>) -> Duration {
    let ms = raw
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_AUTO_START_TIMEOUT_MS);
    Duration::from_millis(ms)
}

impl RpcClient {
    /// Connects to `arbd` at `paths.ipc_endpoint()`, optionally auto-starting the sibling `arbd`
    /// binary when `auto_start == AutoStartMode::Enabled`, and verifies `protocol_version`.
    ///
    /// # Errors
    ///
    /// Returns [`RpcClientError`] if connecting, auto-starting, or `system.hello` verification
    /// fails.
    pub(crate) fn connect(
        paths: &ArbPaths,
        auto_start: AutoStartMode,
    ) -> Result<Self, RpcClientError> {
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
    pub(crate) fn connect_endpoint(endpoint: &Path) -> Result<Self, RpcClientError> {
        let stream = LocalIpcStream::connect(endpoint)?;
        let writer = stream.try_clone()?;
        let reader = BufReader::new(stream);
        Ok(Self {
            reader,
            writer,
            next_id: 1,
        })
    }

    /// Calls `system.hello` and verifies that `protocol_version` matches
    /// [`EXPECTED_IPC_PROTOCOL_VERSION`] (R3 S16c).
    ///
    /// # Errors
    ///
    /// Returns [`RpcClientError::ProtocolVersionMismatch`] if the daemon reports an
    /// incompatible protocol version, or transport/RPC errors.
    pub(crate) fn verify_hello(&mut self) -> Result<Value, RpcClientError> {
        let hello = self.call("system.hello", json!({}))?;
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
    pub(crate) fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcClientError> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);

        let req = OutgoingRequest {
            jsonrpc: "2.0",
            id,
            method,
            params,
        };
        let line = serde_json::to_string(&req)?;
        write_frame_line(&mut self.writer, &line, DEFAULT_MAX_IPC_LINE_BYTES)?;
        self.writer.drain()?;

        let raw = read_frame_line(&mut self.reader, DEFAULT_MAX_IPC_LINE_BYTES)?
            .ok_or(RpcClientError::UnexpectedEof)?;
        let resp: IncomingResponse = serde_json::from_str(&raw)?;
        if let Some(err) = resp.error {
            return Err(RpcClientError::Rpc {
                code: err.code,
                message: err.message,
            });
        }
        Ok(resp.result.unwrap_or(Value::Null))
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
pub(crate) fn spawn_daemon_process(paths: &ArbPaths) -> Result<(Child, PathBuf), RpcClientError> {
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
pub(crate) fn wait_for_daemon(
    endpoint: &Path,
    timeout: Duration,
    mut child: Option<&mut Child>,
    log_path: Option<&Path>,
) -> Result<RpcClient, RpcClientError> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(mut client) = RpcClient::connect_endpoint(endpoint) {
            match client.verify_hello() {
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
                && client.verify_hello().is_ok()
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
        if Instant::now() >= deadline {
            return Err(RpcClientError::AutoStartTimeout {
                endpoint: endpoint.to_path_buf(),
            });
        }
        thread::sleep(AUTO_START_POLL);
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

fn locate_arbd_binary() -> PathBuf {
    if let Some(explicit) = std::env::var_os("ARB_DAEMON_BIN") {
        let p = PathBuf::from(explicit);
        if p.is_file() {
            return p;
        }
    }
    let bin_name = if cfg!(windows) { "arbd.exe" } else { "arbd" };
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join(bin_name);
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

    use arb_os::ipc::{
        DEFAULT_MAX_IPC_LINE_BYTES, LocalIpcListener, read_frame_line, write_frame_line,
    };
    use arb_os::paths::ArbPaths;
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
