//! Local IPC JSON-RPC 2.0 client and optional `arbd` auto-spawner for `arb-tray`.
//!
//! `arb-tray` talks to `arbd` exclusively over `arb_os::ipc::LocalIpcStream`
//! (`SPEC-01`, `SPEC-03`): it performs zero direct ring/catalog/media file writes.

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use arb_os::ipc::{DEFAULT_MAX_IPC_LINE_BYTES, LocalIpcStream, read_frame_line, write_frame_line};
use arb_os::paths::ArbPaths;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

const AUTO_START_TIMEOUT: Duration = Duration::from_secs(5);
const AUTO_START_POLL: Duration = Duration::from_millis(50);

/// Controls whether connecting to `arbd` should auto-spawn the sibling `arbd` binary.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoStartMode {
    /// Fail immediately if `arbd` is not already listening.
    Disabled,
    /// Spawn sibling `arbd` if the IPC endpoint is unavailable and wait for `system.hello`.
    Enabled,
}

/// Errors returned by [`RpcClient`].
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum RpcClientError {
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
pub struct RpcClient {
    reader: BufReader<LocalIpcStream>,
    writer: LocalIpcStream,
    next_id: u64,
}

impl RpcClient {
    /// Connects to `arbd` at `paths.ipc_endpoint()`, optionally auto-starting the sibling `arbd`
    /// binary when `auto_start == AutoStartMode::Enabled`.
    ///
    /// # Errors
    ///
    /// Returns [`RpcClientError`] if connecting or auto-starting fails.
    pub fn connect(paths: &ArbPaths, auto_start: AutoStartMode) -> Result<Self, RpcClientError> {
        let endpoint = paths.ipc_endpoint();
        match Self::connect_endpoint(&endpoint) {
            Ok(client) => Ok(client),
            Err(first_err) => {
                if auto_start == AutoStartMode::Disabled {
                    return Err(first_err);
                }
                spawn_daemon_process(paths)?;
                wait_for_daemon(&endpoint, AUTO_START_TIMEOUT)
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
        let writer = stream.try_clone()?;
        let reader = BufReader::new(stream);
        Ok(Self {
            reader,
            writer,
            next_id: 1,
        })
    }

    /// Invokes a JSON-RPC 2.0 method and returns its decoded `result` JSON [`Value`].
    ///
    /// # Errors
    ///
    /// Returns [`RpcClientError`] on transport, serialization, or server-reported RPC errors.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcClientError> {
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

/// Spawns a detached sibling `arbd` binary configured with `paths`.
///
/// # Errors
///
/// Returns [`RpcClientError::Io`] if locating or spawning `arbd` fails.
pub fn spawn_daemon_process(paths: &ArbPaths) -> Result<(), RpcClientError> {
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
        .stderr(Stdio::null());
    let _child = cmd.spawn()?;
    Ok(())
}

/// Polls `endpoint` until `system.hello` succeeds or `timeout` elapses.
///
/// # Errors
///
/// Returns [`RpcClientError::AutoStartTimeout`] if the daemon never becomes ready.
pub fn wait_for_daemon(endpoint: &Path, timeout: Duration) -> Result<RpcClient, RpcClientError> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(mut client) = RpcClient::connect_endpoint(endpoint)
            && client.call("system.hello", json!({})).is_ok()
        {
            return Ok(client);
        }
        if Instant::now() >= deadline {
            return Err(RpcClientError::AutoStartTimeout {
                endpoint: endpoint.to_path_buf(),
            });
        }
        thread::sleep(AUTO_START_POLL);
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
