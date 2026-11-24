//! Shared daemon client and tray-compatible startup helpers.
use arb_os::paths::ArbPaths;
pub use arb_os::rpc::{AutoStartMode, EXPECTED_IPC_PROTOCOL_VERSION, RpcClient, RpcClientError};
use std::{path::Path, time::Duration};

/// Starts a detached recording daemon.
///
/// # Errors
/// Returns an error if the process cannot be started.
pub fn spawn_daemon_process(paths: &ArbPaths) -> Result<(), RpcClientError> {
    arb_os::rpc::spawn_daemon_process(paths).map(drop)
}

/// Waits for the daemon handshake.
///
/// # Errors
/// Returns an error if the endpoint is unavailable or the handshake fails.
pub fn wait_for_daemon(endpoint: &Path, timeout: Duration) -> Result<RpcClient, RpcClientError> {
    arb_os::rpc::wait_for_daemon(endpoint, timeout, None, None)
}
