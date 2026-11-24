//! Host `adb` binary discovery, session-scoped agent deployment, auto-reconnect
//! supervision, and live [`AdbSessionSource`] event ingestion.
//!
//! Safety invariant: this module NEVER invokes `adb kill-server`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::env;
use std::ffi::OsStr;
use std::io::Read;
use std::net::{Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use arb_core::time::{
    ClockTimeline, ClockTimelineConfig, DeviceClockTriple, DiscontinuityEvent, DiscontinuityKind,
    StreamKind,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::mock::{CaptureEvent, CaptureSourceError, DeviceEventSource};
use crate::scrcpy::{
    AudioPacket, AudioPacketKind, AudioTunnelReader, ScrcpyError, ScrcpyLaunchOptions, VideoRecord,
    VideoTunnelReader,
};
use crate::wire::{
    BootIdSource, DEFAULT_MAX_PAYLOAD_LEN, HeartbeatPayload, HelloPayload, StreamId,
    StreamLifecycleState, StreamStatePayload, WireError, WireFrame, WireMessage,
};

/// Environment variable overriding host `adb` binary discovery.
pub const ENV_ARB_ADB_PATH: &str = "ARB_ADB_PATH";

/// Default exponential backoff schedule in milliseconds (`200ms -> 500ms -> 1s -> 2s`).
pub const DEFAULT_RECONNECT_BACKOFF_MS: &[u64] = &[200, 500, 1_000, 2_000];

/// Default maximum backoff cap in milliseconds (`2s`).
pub const DEFAULT_MAX_RECONNECT_BACKOFF_MS: u64 = 2_000;

/// Default silent video threshold before classifying a live running video stream as `StaticHold` (`2s`).
pub const DEFAULT_STATIC_HOLD_THRESHOLD_NS: i64 = 2_000_000_000;

/// Default maximum age of the most recent companion heartbeat before declaring a beacon timeout (`1.5s`).
pub const DEFAULT_BEACON_TIMEOUT_NS: i64 = 1_500_000_000;

/// Default wall-clock bound for one host `adb` subprocess invocation (`60s`).
///
/// Generous enough for `adb push` of the session DEX jars over slow USB, while
/// guaranteeing a wedged `adb` (unauthorized device, hung shell, server restart)
/// can never block the caller (and therefore the reconnect backoff) forever.
pub const DEFAULT_ADB_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// Default period of the live [`AdbSessionSource`] video liveness tick (`250ms`).
pub const DEFAULT_LIVENESS_TICK_INTERVAL: Duration = Duration::from_millis(250);

/// Capacity of the bounded channel shared by the live per-stream reader threads.
pub const LIVE_EVENT_CHANNEL_CAPACITY: usize = 1_024;

/// Errors returned by host `adb` discovery, command execution, and agent deployment.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum AdbError {
    /// No executable `adb` binary was found via `ARB_ADB_PATH`, `PATH`, or `ANDROID_HOME`.
    #[error("adb binary not found via {ENV_ARB_ADB_PATH}, PATH, or ANDROID_HOME")]
    AdbNotFound,
    /// Configured `ARB_ADB_PATH` did not point to an existing file.
    #[error("configured {ENV_ARB_ADB_PATH} does not exist or is not a file: {path}")]
    InvalidConfiguredPath {
        /// Configured path value.
        path: PathBuf,
    },
    /// Spawning the `adb` process failed.
    #[error("failed to execute adb command '{args}': {source}")]
    CommandSpawn {
        /// Space-joined command arguments for diagnostics.
        args: String,
        /// Underlying OS spawn error.
        #[source]
        source: std::io::Error,
    },
    /// `adb` exited with a non-zero status code.
    #[error("adb command '{args}' failed (exit {exit_code:?}): {stderr}")]
    CommandFailed {
        /// Space-joined command arguments.
        args: String,
        /// Process exit code if available.
        exit_code: Option<i32>,
        /// Captured standard error output.
        stderr: String,
    },
    /// `adb forward tcp:0 ...` did not output a valid TCP port number.
    #[error("failed to parse forwarded TCP port from adb output '{output}'")]
    InvalidForwardedPort {
        /// Raw trimmed stdout from `adb forward`.
        output: String,
    },
    /// Session token contained unsafe characters for `/data/local/tmp/arb-<token>-*.jar`.
    #[error("invalid session token '{token}': must be non-empty ASCII alphanumeric, '-' or '_'")]
    InvalidSessionToken {
        /// Rejected token string.
        token: String,
    },
    /// A value interpolated into a remote `sh` command line contained unsafe characters.
    #[error("invalid remote shell argument {name}='{value}': {rule}")]
    InvalidShellArgument {
        /// Logical argument name (e.g., `socket_name`, `remote_jar`, `scid`).
        name: &'static str,
        /// Rejected value.
        value: String,
        /// Human-readable validation rule that was violated.
        rule: &'static str,
    },
}

/// Returns the platform-specific `adb` executable filename (`adb.exe` on Windows, `adb` elsewhere).
pub const fn adb_executable_name() -> &'static str {
    if cfg!(windows) { "adb.exe" } else { "adb" }
}

/// Resolves the host `adb` binary in priority order:
/// 1. `ARB_ADB_PATH`
/// 2. `PATH`
/// 3. `${ANDROID_HOME:-$HOME/Library/Android/sdk}/platform-tools/adb`
///
/// # Errors
///
/// Returns [`AdbError::InvalidConfiguredPath`] if `ARB_ADB_PATH` is set to a missing file,
/// or [`AdbError::AdbNotFound`] if no candidate exists.
pub fn resolve_adb_binary() -> Result<PathBuf, AdbError> {
    let env_adb = env::var_os(ENV_ARB_ADB_PATH).map(PathBuf::from);
    let path_var = env::var_os("PATH");
    let android_home = env::var_os("ANDROID_HOME")
        .or_else(|| env::var_os("ANDROID_SDK_ROOT"))
        .map(PathBuf::from);
    let home_dir = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let local_app_data = env::var_os("LOCALAPPDATA").map(PathBuf::from);
    resolve_adb_binary_full(
        env_adb.as_deref(),
        path_var.as_deref(),
        android_home.as_deref(),
        home_dir.as_deref(),
        local_app_data.as_deref(),
    )
}

/// Deterministic, pure helper implementing `adb` binary discovery for tests and callers.
///
/// # Errors
///
/// Returns [`AdbError::InvalidConfiguredPath`] when `env_adb_path` is explicitly provided
/// but does not exist as a file, or [`AdbError::AdbNotFound`] when no candidate exists.
pub fn resolve_adb_binary_with(
    env_adb_path: Option<&Path>,
    path_var: Option<&OsStr>,
    android_home: Option<&Path>,
    home_dir: Option<&Path>,
) -> Result<PathBuf, AdbError> {
    resolve_adb_binary_full(env_adb_path, path_var, android_home, home_dir, None)
}

fn resolve_adb_binary_full(
    env_adb_path: Option<&Path>,
    path_var: Option<&OsStr>,
    android_home: Option<&Path>,
    home_dir: Option<&Path>,
    local_app_data: Option<&Path>,
) -> Result<PathBuf, AdbError> {
    if let Some(configured) = env_adb_path
        && !configured.as_os_str().is_empty()
    {
        if configured.is_file() {
            return Ok(configured.to_path_buf());
        }
        return Err(AdbError::InvalidConfiguredPath {
            path: configured.to_path_buf(),
        });
    }

    let exe_name = adb_executable_name();

    if let Some(paths) = path_var {
        for dir in env::split_paths(paths) {
            let candidate = dir.join(exe_name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    if let Some(sdk) = android_home
        && !sdk.as_os_str().is_empty()
    {
        let candidate = sdk.join("platform-tools").join(exe_name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    if let Some(local_app) = local_app_data
        && !local_app.as_os_str().is_empty()
    {
        let win_default = local_app
            .join("Android")
            .join("Sdk")
            .join("platform-tools")
            .join(exe_name);
        if win_default.is_file() {
            return Ok(win_default);
        }
    }

    if let Some(home) = home_dir {
        let mac_default = home
            .join("Library")
            .join("Android")
            .join("sdk")
            .join("platform-tools")
            .join(exe_name);
        if mac_default.is_file() {
            return Ok(mac_default);
        }
        let linux_default = home
            .join("Android")
            .join("Sdk")
            .join("platform-tools")
            .join(exe_name);
        if linux_default.is_file() {
            return Ok(linux_default);
        }
        let win_home_default = home
            .join("AppData")
            .join("Local")
            .join("Android")
            .join("Sdk")
            .join("platform-tools")
            .join(exe_name);
        if win_home_default.is_file() {
            return Ok(win_home_default);
        }
    }

    Err(AdbError::AdbNotFound)
}

/// Connection state reported by `adb devices -l`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdbDeviceState {
    /// Device is online and ready for ADB shell / forward commands.
    Device,
    /// Device is temporarily offline.
    Offline,
    /// Host RSA key has not been authorized on the device.
    Unauthorized,
    /// Device is booted into recovery mode.
    Recovery,
    /// Device is in bootloader / fastboot mode.
    Bootloader,
    /// Unrecognized ADB device state token.
    Unknown(String),
}

impl AdbDeviceState {
    fn from_token(token: &str) -> Self {
        match token {
            "device" => Self::Device,
            "offline" => Self::Offline,
            "unauthorized" => Self::Unauthorized,
            "recovery" => Self::Recovery,
            "bootloader" => Self::Bootloader,
            other => Self::Unknown(other.to_owned()),
        }
    }
}

/// Single device row parsed from `adb devices -l`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AdbDeviceInfo {
    /// ADB transport serial (e.g., `"emulator-5554"` or USB serial).
    pub serial: String,
    /// Reported ADB state.
    pub state: AdbDeviceState,
    /// Optional `product:` metadata token.
    pub product: Option<String>,
    /// Optional `model:` metadata token.
    pub model: Option<String>,
    /// Optional `device:` metadata token.
    pub device: Option<String>,
    /// Optional `transport_id:` metadata token.
    pub transport_id: Option<String>,
}

/// Parses `adb shell getprop ro.build.version.sdk` stdout into a positive Android API level.
///
/// Returns `None` when the output is empty, non-numeric, or zero.
#[must_use]
pub fn parse_sdk_version_output(stdout: &str) -> Option<u32> {
    stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .and_then(|line| line.parse::<u32>().ok())
        .filter(|&sdk| sdk > 0)
}

/// Parses the stdout of `adb shell wm size` into physical display `(width, height)` pixels.
///
/// Prefers `Physical size: WxH` (raw `getevent` touch coordinates live in the physical panel
/// space, so tap mapping must not use a `wm size` override), falling back to
/// `Override size: WxH` if only an override line is valid. Returns `None` when dimensions are
/// absent, malformed, or zero.
#[must_use]
pub fn parse_wm_size_output(stdout: &str) -> Option<(u32, u32)> {
    let mut physical_size: Option<(u32, u32)> = None;
    let mut override_size: Option<(u32, u32)> = None;
    for raw_line in stdout.lines() {
        let line = raw_line.trim();
        if let Some(rest) = line.strip_prefix("Physical size:")
            && let Some(dims) = parse_wxh_dims(rest.trim())
        {
            physical_size = Some(dims);
        } else if let Some(rest) = line.strip_prefix("Override size:")
            && let Some(dims) = parse_wxh_dims(rest.trim())
        {
            override_size = Some(dims);
        }
    }
    physical_size.or(override_size)
}

/// Parses `adb shell cmd package list packages -U` stdout into a `uid -> package_name` map.
///
/// Expected line format: `package:<pkg_name> uid:<uid>` (or comma-separated UIDs for multi-user).
///
/// Only unambiguous application UIDs are returned: system UIDs (app id below
/// [`FIRST_APPLICATION_UID`], e.g. `1000` = `system_server`) and UIDs shared by several packages
/// (`sharedUserId`) are dropped so their logs are never attributed to an arbitrary package.
#[must_use]
pub fn parse_package_uid_list(stdout: &str) -> BTreeMap<u32, String> {
    let mut map: BTreeMap<u32, String> = BTreeMap::new();
    let mut ambiguous: BTreeSet<u32> = BTreeSet::new();
    for raw_line in stdout.lines() {
        let line = raw_line.trim();
        let Some(rest) = line.strip_prefix("package:") else {
            continue;
        };
        let mut parts = rest.split_whitespace();
        let Some(pkg) = parts.next().filter(|s| !s.is_empty()) else {
            continue;
        };
        let Some(uid_token) = parts.find_map(|tok| tok.strip_prefix("uid:")) else {
            continue;
        };
        for uid_str in uid_token.split(',') {
            let Ok(uid) = uid_str.trim().parse::<u32>() else {
                continue;
            };
            if uid % PER_USER_UID_RANGE < FIRST_APPLICATION_UID {
                continue;
            }
            match map.get(&uid) {
                Some(existing) if existing != pkg => {
                    ambiguous.insert(uid);
                }
                Some(_) => {}
                None => {
                    map.insert(uid, pkg.to_owned());
                }
            }
        }
    }
    for uid in ambiguous {
        map.remove(&uid);
    }
    map
}

/// First Android application UID (`android.os.Process.FIRST_APPLICATION_UID`).
pub const FIRST_APPLICATION_UID: u32 = 10_000;
/// Per-user UID range (`android.os.UserHandle.PER_USER_RANGE`).
pub const PER_USER_UID_RANGE: u32 = 100_000;

fn parse_wxh_dims(token: &str) -> Option<(u32, u32)> {
    let (w_str, h_str) = token.split_once('x').or_else(|| token.split_once('X'))?;
    let w = w_str.trim().parse::<u32>().ok()?;
    let h = h_str.trim().parse::<u32>().ok()?;
    (w > 0 && h > 0).then_some((w, h))
}

/// Parses the stdout of `adb devices -l` into structured [`AdbDeviceInfo`] rows.
pub fn parse_devices_output(stdout: &str) -> Vec<AdbDeviceInfo> {
    let mut devices = Vec::new();
    for raw_line in stdout.lines() {
        let line = raw_line.trim();
        if line.is_empty()
            || line.starts_with("List of devices attached")
            || line.starts_with("* daemon")
        {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(serial) = parts.next() else {
            continue;
        };
        let Some(state_tok) = parts.next() else {
            continue;
        };
        let mut info = AdbDeviceInfo {
            serial: serial.to_owned(),
            state: AdbDeviceState::from_token(state_tok),
            product: None,
            model: None,
            device: None,
            transport_id: None,
        };
        for field in parts {
            if let Some((key, val)) = field.split_once(':') {
                match key {
                    "product" => info.product = Some(val.to_owned()),
                    "model" => info.model = Some(val.to_owned()),
                    "device" => info.device = Some(val.to_owned()),
                    "transport_id" => info.transport_id = Some(val.to_owned()),
                    _ => {}
                }
            }
        }
        devices.push(info);
    }
    devices
}

/// Captured result of running one `adb` subprocess invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdbCommandOutput {
    /// Process exit code, if terminated normally.
    pub exit_code: Option<i32>,
    /// Standard output decoded as UTF-8.
    pub stdout: String,
    /// Standard error decoded as UTF-8.
    pub stderr: String,
}

impl AdbCommandOutput {
    /// Returns `true` when `exit_code == Some(0)`.
    pub const fn is_success(&self) -> bool {
        matches!(self.exit_code, Some(0))
    }
}

/// Abstraction over subprocess invocation so [`AdbClient`] is 100% unit-testable.
pub trait AdbCommandRunner: std::fmt::Debug {
    /// Executes `program` with `args` and returns its exit code and captured output.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] if spawning the process fails.
    fn run(&self, program: &Path, args: &[String]) -> Result<AdbCommandOutput, std::io::Error>;
}

/// Production [`AdbCommandRunner`] backed by [`std::process::Command`].
///
/// Every invocation is bounded by [`DEFAULT_ADB_COMMAND_TIMEOUT`]; on expiry the child
/// is killed and reaped, and [`std::io::ErrorKind::TimedOut`] is returned.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemAdbRunner;

impl AdbCommandRunner for SystemAdbRunner {
    fn run(&self, program: &Path, args: &[String]) -> Result<AdbCommandOutput, std::io::Error> {
        let mut cmd = Command::new(program);
        cmd.args(args);
        run_command_with_timeout(cmd, DEFAULT_ADB_COMMAND_TIMEOUT)
    }
}

/// [`AdbCommandRunner`] backed by [`std::process::Command`] with a caller-chosen timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeoutAdbRunner {
    /// Wall-clock bound for each subprocess invocation.
    pub timeout: Duration,
}

impl TimeoutAdbRunner {
    /// Creates a runner bounding each invocation by `timeout`.
    pub const fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl AdbCommandRunner for TimeoutAdbRunner {
    fn run(&self, program: &Path, args: &[String]) -> Result<AdbCommandOutput, std::io::Error> {
        let mut cmd = Command::new(program);
        cmd.args(args);
        run_command_with_timeout(cmd, self.timeout)
    }
}

/// Spawns `cmd` with piped stdout/stderr and waits at most `timeout` for it to exit.
///
/// Output pipes are drained on helper threads so a chatty child can never deadlock on
/// a full pipe. On expiry the child is `kill()`ed and `wait()`ed (no zombie), and an
/// [`std::io::ErrorKind::TimedOut`] error is returned.
///
/// # Errors
///
/// Returns [`std::io::Error`] if spawning or waiting fails, or `TimedOut` on expiry.
pub fn run_command_with_timeout(
    mut cmd: Command,
    timeout: Duration,
) -> Result<AdbCommandOutput, std::io::Error> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let stdout_handle = child.stdout.take().map(spawn_pipe_drain);
    let stderr_handle = child.stderr.take().map(spawn_pipe_drain);

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        let now = Instant::now();
        if now >= deadline {
            // Best effort: the child may have exited between try_wait and kill.
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("adb subprocess exceeded timeout of {timeout:?}"),
            ));
        }
        thread::sleep((deadline - now).min(Duration::from_millis(10)));
    };

    // A grandchild (e.g. a freshly forked adb server) may inherit the pipes; never let
    // that block the caller past the deadline (with a small grace for the final bytes).
    let drain_deadline = deadline.max(Instant::now() + Duration::from_millis(500));
    let stdout = stdout_handle.map_or_else(Vec::new, |rx| recv_until(&rx, drain_deadline));
    let stderr = stderr_handle.map_or_else(Vec::new, |rx| recv_until(&rx, drain_deadline));
    Ok(AdbCommandOutput {
        exit_code: status.code(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

fn recv_until(rx: &Receiver<Vec<u8>>, deadline: Instant) -> Vec<u8> {
    let wait = deadline.saturating_duration_since(Instant::now());
    rx.recv_timeout(wait).unwrap_or_default()
}

fn spawn_pipe_drain<R: Read + Send + 'static>(mut pipe: R) -> Receiver<Vec<u8>> {
    let (tx, rx) = sync_channel(1);
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    rx
}

/// Toggle for optional companion sub-streams without boolean parameters.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamEnable {
    /// Stream is enabled.
    Enabled,
    /// Stream is disabled (`--no-logcat` / `--no-input`).
    Disabled,
}

/// Remote `/data/local/tmp/arb-<token>-*.jar` paths deployed for one capture session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeployedAgentPaths {
    /// Session token used to scope remote files.
    pub session_token: String,
    /// Remote path to `arb-companion.dex.jar`.
    pub companion_jar: String,
    /// Optional remote path to `replay-scrcpy-audio.dex.jar`.
    pub scrcpy_jar: Option<String>,
}

/// Host ADB client for device discovery, DEX push, port forwarding, and remote cleanup.
///
/// Never exposes or invokes `adb kill-server`.
#[derive(Debug, Clone)]
pub struct AdbClient<R = SystemAdbRunner> {
    adb_path: PathBuf,
    runner: R,
}

impl AdbClient<SystemAdbRunner> {
    /// Creates an [`AdbClient`] targeting `adb_path` with [`SystemAdbRunner`].
    pub const fn new(adb_path: PathBuf) -> Self {
        Self {
            adb_path,
            runner: SystemAdbRunner,
        }
    }

    /// Discovers the host `adb` binary and creates a ready [`AdbClient`].
    ///
    /// # Errors
    ///
    /// Returns [`AdbError`] if no host `adb` binary can be resolved.
    pub fn discover() -> Result<Self, AdbError> {
        let path = resolve_adb_binary()?;
        Ok(Self::new(path))
    }
}

impl<R: AdbCommandRunner> AdbClient<R> {
    /// Creates an [`AdbClient`] with a custom [`AdbCommandRunner`] (e.g., mock runner in tests).
    pub const fn with_runner(adb_path: PathBuf, runner: R) -> Self {
        Self { adb_path, runner }
    }

    /// Returns the resolved path to the `adb` binary.
    pub fn adb_path(&self) -> &Path {
        &self.adb_path
    }

    /// Lists connected devices via `adb devices -l`.
    ///
    /// # Errors
    ///
    /// Returns [`AdbError`] if running `adb devices -l` fails.
    pub fn devices(&self) -> Result<Vec<AdbDeviceInfo>, AdbError> {
        let out = self.exec(&["devices".to_owned(), "-l".to_owned()])?;
        Ok(parse_devices_output(&out.stdout))
    }

    /// Pushes `local_path` to `remote_path` on `serial`.
    ///
    /// # Errors
    ///
    /// Returns [`AdbError`] if `adb -s <serial> push` fails.
    pub fn push(&self, serial: &str, local_path: &Path, remote_path: &str) -> Result<(), AdbError> {
        let args = vec![
            "-s".to_owned(),
            serial.to_owned(),
            "push".to_owned(),
            local_path.to_string_lossy().into_owned(),
            remote_path.to_owned(),
        ];
        let _out = self.exec(&args)?;
        Ok(())
    }

    /// Sets up `adb -s <serial> forward tcp:0 localabstract:<abstract_name>` and returns the allocated TCP port.
    ///
    /// # Errors
    ///
    /// Returns [`AdbError`] if forwarding fails or `adb` does not return a valid `u16` port.
    /// When `adb` succeeded but its output could not be parsed, every forward pointing at
    /// `localabstract:<abstract_name>` for `serial` is removed (best effort) so no orphan
    /// forward outlives the failed call.
    pub fn forward_tcp_to_abstract(
        &self,
        serial: &str,
        abstract_name: &str,
    ) -> Result<u16, AdbError> {
        validate_remote_shell_arg("abstract_name", abstract_name)?;
        let target = format!("localabstract:{abstract_name}");
        let args = vec![
            "-s".to_owned(),
            serial.to_owned(),
            "forward".to_owned(),
            "tcp:0".to_owned(),
            target.clone(),
        ];
        let out = self.exec(&args)?;
        let trimmed = out.stdout.trim();
        if let Ok(port) = trimmed.parse::<u16>() {
            return Ok(port);
        }
        self.remove_forwards_to(serial, &target);
        Err(AdbError::InvalidForwardedPort {
            output: trimmed.to_owned(),
        })
    }

    /// Best-effort removal of every forward for `serial` whose remote end is `target`
    /// (parsed from `adb forward --list`: `<serial> tcp:<port> <target>`).
    fn remove_forwards_to(&self, serial: &str, target: &str) {
        let Ok(list) = self.exec(&["forward".to_owned(), "--list".to_owned()]) else {
            return;
        };
        for port in parse_forward_list_ports(&list.stdout, serial, target) {
            let _ = self.remove_forward(serial, port);
        }
    }

    /// Creates a pre-journaled forward without replacing an existing mapping.
    ///
    /// # Errors
    ///
    /// Returns the ADB failure if the requested port is already in use.
    pub fn create_owned_forward(
        &self,
        serial: &str,
        port: u16,
        socket: &str,
    ) -> Result<(), AdbError> {
        validate_remote_shell_arg("socket", socket)?;
        self.exec(&[
            "-s".into(),
            serial.into(),
            "forward".into(),
            "--no-rebind".into(),
            format!("tcp:{port}"),
            format!("localabstract:{socket}"),
        ])?;
        Ok(())
    }

    /// Removes only exact serial, port and abstract-socket tuples still registered with ADB.
    ///
    /// # Errors
    ///
    /// Returns an ADB error if listing or removing a mapping fails.
    pub fn remove_owned_forwards(
        &self,
        serial: &str,
        forwards: &[(u16, String)],
    ) -> Result<(), AdbError> {
        if forwards.is_empty() {
            return Ok(());
        }
        let list = self.exec(&["forward".into(), "--list".into()])?;
        for (port, socket) in forwards {
            if parse_forward_list_ports(&list.stdout, serial, &format!("localabstract:{socket}"))
                .contains(port)
            {
                self.remove_forward(serial, *port)?;
            }
        }
        Ok(())
    }

    /// Removes a forwarded local TCP port via `adb -s <serial> forward --remove tcp:<port>`.
    ///
    /// # Errors
    ///
    /// Returns [`AdbError`] if removing the forward fails.
    pub fn remove_forward(&self, serial: &str, local_port: u16) -> Result<(), AdbError> {
        let args = vec![
            "-s".to_owned(),
            serial.to_owned(),
            "forward".to_owned(),
            "--remove".to_owned(),
            format!("tcp:{local_port}"),
        ];
        let _out = self.exec(&args)?;
        Ok(())
    }

    /// Runs `adb -s <serial> shell <args...>` and returns stdout.
    ///
    /// # Errors
    ///
    /// Returns [`AdbError`] if the shell command fails.
    pub fn shell(&self, serial: &str, shell_args: &[&str]) -> Result<String, AdbError> {
        let mut args = Vec::with_capacity(3 + shell_args.len());
        args.push("-s".to_owned());
        args.push(serial.to_owned());
        args.push("shell".to_owned());
        for arg in shell_args {
            args.push((*arg).to_owned());
        }
        let out = self.exec(&args)?;
        Ok(out.stdout)
    }

    /// Best-effort query of the device's Android API level via
    /// `adb -s <serial> shell getprop ro.build.version.sdk`.
    ///
    /// Returns `None` if the command fails or produces unrecognized output.
    #[must_use]
    pub fn query_sdk_version(&self, serial: &str) -> Option<u32> {
        let out = self
            .shell(serial, &["getprop", "ro.build.version.sdk"])
            .ok()?;
        parse_sdk_version_output(&out)
    }

    /// Best-effort query of the device's physical display size via `adb -s <serial> shell wm size`.
    ///
    /// Returns `None` if the command fails or produces unrecognized output.
    #[must_use]
    pub fn query_display_size(&self, serial: &str) -> Option<(u32, u32)> {
        let out = self.shell(serial, &["wm", "size"]).ok()?;
        parse_wm_size_output(&out)
    }

    /// Best-effort query of installed package UIDs via
    /// `adb -s <serial> shell cmd package list packages -U`, falling back cleanly to
    /// `adb -s <serial> shell pm list packages -U` if `cmd package` fails or returns no
    /// valid application UIDs (e.g., vendor Android 9 ROMs).
    ///
    /// Returns an empty map if both commands fail (never blocks capture).
    #[must_use]
    pub fn query_package_uids(&self, serial: &str) -> BTreeMap<u32, String> {
        if let Ok(out) = self.shell(serial, &["cmd", "package", "list", "packages", "-U"]) {
            let parsed = parse_package_uid_list(&out);
            if !parsed.is_empty() {
                return parsed;
            }
        }
        if let Ok(out) = self.shell(serial, &["pm", "list", "packages", "-U"]) {
            return parse_package_uid_list(&out);
        }
        BTreeMap::new()
    }

    /// Pushes `arb-companion.dex.jar` and optional `replay-scrcpy-audio.dex.jar`
    /// to session-scoped `/data/local/tmp/arb-<token>-*.jar` paths.
    ///
    /// # Errors
    ///
    /// Returns [`AdbError::InvalidSessionToken`] if `session_token` is unsafe,
    /// or [`AdbError`] if any `adb push` fails.
    pub fn deploy_session_agents(
        &self,
        serial: &str,
        session_token: &str,
        companion_jar: &Path,
        scrcpy_jar: Option<&Path>,
    ) -> Result<DeployedAgentPaths, AdbError> {
        validate_session_token(session_token)?;
        let remote_companion = format!("/data/local/tmp/arb-{session_token}-companion.dex.jar");
        if let Err(err) = self.push(serial, companion_jar, &remote_companion) {
            // A failed push may still have left a partial file behind.
            let _ = self.shell(serial, &["rm", "-f", remote_companion.as_str()]);
            return Err(err);
        }

        let remote_scrcpy = if let Some(local_scrcpy) = scrcpy_jar {
            let remote = format!("/data/local/tmp/arb-{session_token}-scrcpy-audio.dex.jar");
            if let Err(err) = self.push(serial, local_scrcpy, &remote) {
                // Never leak the already-pushed companion (or a partial scrcpy) jar.
                let _ = self.shell(
                    serial,
                    &["rm", "-f", remote_companion.as_str(), remote.as_str()],
                );
                return Err(err);
            }
            Some(remote)
        } else {
            None
        };

        Ok(DeployedAgentPaths {
            session_token: session_token.to_owned(),
            companion_jar: remote_companion,
            scrcpy_jar: remote_scrcpy,
        })
    }

    /// Best-effort teardown removing forwarded ports and remote `/data/local/tmp/arb-<token>-*.jar` files.
    ///
    /// Collects and returns any non-fatal errors so shutdown always attempts every cleanup step.
    pub fn cleanup_session(
        &self,
        serial: &str,
        deployed: &DeployedAgentPaths,
        forwarded_ports: &[u16],
    ) -> Vec<AdbError> {
        let mut errors = Vec::new();
        for &port in forwarded_ports {
            if let Err(err) = self.remove_forward(serial, port) {
                errors.push(err);
            }
        }

        let mut rm_args = vec!["rm", "-f", deployed.companion_jar.as_str()];
        if let Some(scrcpy) = deployed.scrcpy_jar.as_deref() {
            rm_args.push(scrcpy);
        }
        if let Err(err) = self.shell(serial, &rm_args) {
            errors.push(err);
        }
        errors
    }

    /// Builds the exact `app_process` command string to launch `arb-companion`.
    ///
    /// Prefer [`Self::try_companion_launch_command`], which rejects unsafe values. This
    /// infallible form never interpolates an unvalidated value verbatim: anything outside
    /// `[A-Za-z0-9._/-]` is single-quoted for the remote `sh`.
    pub fn companion_launch_command(
        remote_jar: &str,
        socket_name: &str,
        heartbeat_ms: u32,
        logcat: StreamEnable,
        input: StreamEnable,
    ) -> String {
        let remote_jar = shell_word(remote_jar);
        let socket_name = shell_word(socket_name);
        let clamped_hb = heartbeat_ms.clamp(100, 5_000);
        let mut cmd = format!(
            "CLASSPATH={remote_jar} app_process / --nice-name=arb-companion \
             io.github.shinma06.arb.agent.CompanionMain --socket {socket_name} \
             --heartbeat-ms {clamped_hb}"
        );
        if matches!(logcat, StreamEnable::Disabled) {
            cmd.push_str(" --no-logcat");
        }
        if matches!(input, StreamEnable::Disabled) {
            cmd.push_str(" --no-input");
        }
        cmd
    }

    /// Validating form of [`Self::companion_launch_command`].
    ///
    /// # Errors
    ///
    /// Returns [`AdbError::InvalidShellArgument`] if `remote_jar` or `socket_name` is not
    /// strictly `^[A-Za-z0-9._/-]+$` (at most 255 bytes).
    pub fn try_companion_launch_command(
        remote_jar: &str,
        socket_name: &str,
        heartbeat_ms: u32,
        logcat: StreamEnable,
        input: StreamEnable,
    ) -> Result<String, AdbError> {
        validate_remote_shell_arg("remote_jar", remote_jar)?;
        validate_remote_shell_arg("socket_name", socket_name)?;
        Ok(Self::companion_launch_command(
            remote_jar,
            socket_name,
            heartbeat_ms,
            logcat,
            input,
        ))
    }

    /// Builds the exact `app_process` command string to launch `scrcpy-server-v5.0.1`.
    ///
    /// Prefer [`Self::try_scrcpy_launch_command`], which rejects unsafe values. This
    /// infallible form single-quotes any value outside the safe character set.
    pub fn scrcpy_launch_command(
        remote_jar: &str,
        options: &ScrcpyLaunchOptions,
        api_level: u32,
    ) -> String {
        let (server_args, _audio_status) = options.build_server_args(api_level);
        let quoted: Vec<String> = server_args.iter().map(|a| shell_word(a)).collect();
        format!(
            "CLASSPATH={} app_process / --nice-name=arb-scrcpy \
             com.genymobile.scrcpy.Server {}",
            shell_word(remote_jar),
            quoted.join(" ")
        )
    }

    /// Validating form of [`Self::scrcpy_launch_command`].
    ///
    /// # Errors
    ///
    /// Returns [`AdbError::InvalidShellArgument`] if `remote_jar` is not strictly
    /// `^[A-Za-z0-9._/-]+$` or `options.scid` is not exactly `^[0-9a-f]{8}$`.
    pub fn try_scrcpy_launch_command(
        remote_jar: &str,
        options: &ScrcpyLaunchOptions,
        api_level: u32,
    ) -> Result<String, AdbError> {
        validate_remote_shell_arg("remote_jar", remote_jar)?;
        validate_scid(&options.scid)?;
        Ok(Self::scrcpy_launch_command(remote_jar, options, api_level))
    }

    fn exec(&self, args: &[String]) -> Result<AdbCommandOutput, AdbError> {
        let output =
            self.runner
                .run(&self.adb_path, args)
                .map_err(|source| AdbError::CommandSpawn {
                    args: args.join(" "),
                    source,
                })?;
        if !output.is_success() {
            return Err(AdbError::CommandFailed {
                args: args.join(" "),
                exit_code: output.exit_code,
                stderr: output.stderr.trim().to_owned(),
            });
        }
        Ok(output)
    }
}

fn validate_session_token(token: &str) -> Result<(), AdbError> {
    let valid = !token.is_empty()
        && token.len() <= 64
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !valid {
        return Err(AdbError::InvalidSessionToken {
            token: token.to_owned(),
        });
    }
    Ok(())
}

fn is_safe_shell_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-')
}

/// Validates a value interpolated into a remote `sh` command (`^[A-Za-z0-9._/-]{1,255}$`).
///
/// # Errors
///
/// Returns [`AdbError::InvalidShellArgument`] when `value` is empty, too long, or contains
/// any byte outside the safe set.
pub fn validate_remote_shell_arg(name: &'static str, value: &str) -> Result<(), AdbError> {
    if value.is_empty() || value.len() > 255 || !value.bytes().all(is_safe_shell_byte) {
        return Err(AdbError::InvalidShellArgument {
            name,
            value: value.to_owned(),
            rule: "must match ^[A-Za-z0-9._/-]{1,255}$",
        });
    }
    Ok(())
}

/// Validates a `scrcpy` session id (`^[0-9a-f]{8}$`).
///
/// # Errors
///
/// Returns [`AdbError::InvalidShellArgument`] when `scid` is not exactly 8 lowercase hex digits.
pub fn validate_scid(scid: &str) -> Result<(), AdbError> {
    let valid = scid.len() == 8
        && scid
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !valid {
        return Err(AdbError::InvalidShellArgument {
            name: "scid",
            value: scid.to_owned(),
            rule: "must match ^[0-9a-f]{8}$",
        });
    }
    Ok(())
}

/// Returns `value` verbatim when every byte is in the safe set (plus `=`, `,` and `:` used
/// by `key=value` server options), otherwise a single-quoted POSIX `sh` word.
fn shell_word(value: &str) -> String {
    let safe = !value.is_empty()
        && value
            .bytes()
            .all(|b| is_safe_shell_byte(b) || matches!(b, b'=' | b',' | b':'));
    if safe {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// Parses `adb forward --list` output, returning local TCP ports forwarded for `serial`
/// to exactly `target` (e.g., `localabstract:arb-companion-x`).
fn parse_forward_list_ports(stdout: &str, serial: &str, target: &str) -> Vec<u16> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let (s, local, remote) = (parts.next()?, parts.next()?, parts.next()?);
            if s != serial || remote != target {
                return None;
            }
            local.strip_prefix("tcp:")?.parse::<u16>().ok()
        })
        .collect()
}

// ---------------- Auto-Reconnect Policy & SessionSupervisor ----------------

/// Bounded exponential backoff policy for automatic ADB session recovery.
///
/// Defaults to `200ms -> 500ms -> 1s -> 2s` (capped at `2s`) per `SPEC-03`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReconnectPolicy {
    /// Ordered delay steps in milliseconds for consecutive reconnect attempts (`0, 1, 2, ...`).
    pub schedule_ms: Vec<u64>,
    /// Upper cap on any computed reconnect delay in milliseconds.
    pub max_delay_ms: u64,
}

impl ReconnectPolicy {
    /// Creates the default SPEC-03 reconnect policy (`200ms -> 500ms -> 1s -> 2s`, capped at `2s`).
    pub fn new() -> Self {
        Self {
            schedule_ms: DEFAULT_RECONNECT_BACKOFF_MS.to_vec(),
            max_delay_ms: DEFAULT_MAX_RECONNECT_BACKOFF_MS,
        }
    }

    /// Returns the backoff delay in milliseconds for zero-based `attempt`.
    pub fn delay_ms_for_attempt(&self, attempt: u32) -> u64 {
        if self.schedule_ms.is_empty() {
            return self.max_delay_ms;
        }
        let idx = usize::try_from(attempt).unwrap_or(usize::MAX);
        let raw = self
            .schedule_ms
            .get(idx)
            .copied()
            .or_else(|| self.schedule_ms.last().copied())
            .unwrap_or(self.max_delay_ms);
        raw.min(self.max_delay_ms)
    }

    /// Returns the backoff [`Duration`] for zero-based `attempt`.
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        Duration::from_millis(self.delay_ms_for_attempt(attempt))
    }
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self::new()
    }
}

/// Thresholds governing [`SessionSupervisor`] liveness and reconnect behavior.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SupervisorConfig {
    /// Exponential backoff schedule for automatic recovery.
    pub reconnect_policy: ReconnectPolicy,
    /// Minimum silent video span (ns) before reporting [`VideoLivenessStatus::StaticHold`].
    pub static_hold_threshold_ns: i64,
    /// Maximum time (ns) without a companion heartbeat before treating beacon as stale/stopped.
    pub beacon_timeout_ns: i64,
}

impl SupervisorConfig {
    /// Creates the default supervisor configuration.
    pub fn new() -> Self {
        Self {
            reconnect_policy: ReconnectPolicy::new(),
            static_hold_threshold_ns: DEFAULT_STATIC_HOLD_THRESHOLD_NS,
            beacon_timeout_ns: DEFAULT_BEACON_TIMEOUT_NS,
        }
    }
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Lifecycle state of a supervised device capture session.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SupervisorState {
    /// No active connection has been established yet.
    Disconnected,
    /// Actively attempting to connect or handshake with the device agent.
    Connecting {
        /// Zero-based reconnect attempt counter.
        attempt: u32,
    },
    /// Connected to a live companion session for a known `boot_id`.
    Connected {
        /// Active device `boot_id`.
        boot_id: String,
        /// Source used by the device agent to derive `boot_id`.
        boot_id_source: BootIdSource,
    },
    /// Waiting to reconnect after a transport loss.
    Reconnecting {
        /// Number of failed/scheduled reconnect attempts so far.
        attempt: u32,
        /// Scheduled delay in milliseconds before the next reconnect attempt.
        next_backoff_ms: u64,
        /// Last observed `boot_id` prior to disconnect, if any.
        last_boot_id: Option<String>,
        /// Source of `last_boot_id`, if any.
        last_boot_id_source: Option<BootIdSource>,
    },
    /// Permanently stopped by explicit caller request.
    Stopped,
}

/// Classification of a companion `Hello` handshake relative to prior session history.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "classification", rename_all = "snake_case")]
pub enum ReconnectClassification {
    /// First `Hello` observed by this supervisor.
    InitialConnection {
        /// Observed `boot_id`.
        boot_id: String,
        /// Source of `boot_id`.
        boot_id_source: BootIdSource,
    },
    /// Same reliable `boot_id` across reconnect (`CONNECTION_RESTORED`).
    ConnectionRestored {
        /// Confirmed unchanged `boot_id`.
        boot_id: String,
    },
    /// Reliable `boot_id` changed across reconnect (`DEVICE_REBOOT` hard boundary).
    DeviceReboot {
        /// Previous `boot_id` before reboot.
        previous_boot_id: String,
        /// New `boot_id` after reboot.
        new_boot_id: String,
    },
    /// At least one side used `FallbackRandomUuid`, so continuity across reconnects cannot be proved.
    ContinuityUnverified {
        /// Previous `boot_id`, if known.
        previous_boot_id: Option<String>,
        /// Newly reported `boot_id`.
        new_boot_id: String,
        /// Explanation why reboot vs restore could not be proved.
        reason: String,
    },
}

/// Distinguishes healthy static screens from stalled or broken video streams (`SPEC-03`).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum VideoLivenessStatus {
    /// Video frames are arriving normally within the static-hold threshold.
    Active,
    /// Video has been silent while both Beacon and Video stream remain `Running`.
    StaticHold {
        /// Elapsed nanoseconds since the last observed video frame.
        silent_duration_ns: i64,
    },
    /// Beacon stopped/timed out or the video stream left `Running` (real capture gap).
    StreamGap {
        /// Machine-readable reason for the detected video gap.
        reason: String,
    },
}

/// Deterministic state machine supervising ADB reconnect backoff, `boot_id`
/// reboot-vs-restore classification, and `StaticHold` vs `StreamGap` video liveness.
#[derive(Debug, Clone)]
pub struct SessionSupervisor {
    serial: String,
    config: SupervisorConfig,
    state: SupervisorState,
    last_boot_id: Option<String>,
    last_boot_id_source: Option<BootIdSource>,
    reconnect_attempts: u32,
    last_beacon_pc_cont_ns: Option<i64>,
    last_beacon_boottime_ns: Option<i64>,
    last_video_frame_pc_cont_ns: Option<i64>,
    stream_states: BTreeMap<StreamId, StreamLifecycleState>,
    media_transport_lost: BTreeSet<StreamId>,
}

impl SessionSupervisor {
    /// Creates a new supervisor for `serial` in [`SupervisorState::Disconnected`].
    pub fn new(serial: impl Into<String>, config: SupervisorConfig) -> Self {
        Self {
            serial: serial.into(),
            config,
            state: SupervisorState::Disconnected,
            last_boot_id: None,
            last_boot_id_source: None,
            reconnect_attempts: 0,
            last_beacon_pc_cont_ns: None,
            last_beacon_boottime_ns: None,
            last_video_frame_pc_cont_ns: None,
            stream_states: BTreeMap::new(),
            media_transport_lost: BTreeSet::new(),
        }
    }

    /// Returns the ADB device serial managed by this supervisor.
    pub fn serial(&self) -> &str {
        &self.serial
    }

    /// Returns the supervisor configuration.
    pub const fn config(&self) -> &SupervisorConfig {
        &self.config
    }

    /// Returns the current [`SupervisorState`].
    pub const fn state(&self) -> &SupervisorState {
        &self.state
    }

    /// Returns the most recently observed `boot_id`, if any.
    pub fn current_boot_id(&self) -> Option<&str> {
        self.last_boot_id.as_deref()
    }

    /// Returns the cumulative reconnect attempt count since the last healthy `Hello`.
    pub const fn reconnect_attempts(&self) -> u32 {
        self.reconnect_attempts
    }

    /// Returns the most recently reported lifecycle state for `stream_id`, if any.
    pub fn stream_state(&self, stream_id: StreamId) -> Option<StreamLifecycleState> {
        self.stream_states.get(&stream_id).copied()
    }

    /// Marks the start of a connection attempt.
    pub fn begin_connecting(&mut self) {
        self.state = SupervisorState::Connecting {
            attempt: self.reconnect_attempts,
        };
    }

    /// Processes a companion `Hello` handshake, classifying initial connection,
    /// `ConnectionRestored`, `DeviceReboot`, or `ContinuityUnverified`.
    ///
    /// Emits a [`DiscontinuityEvent`] with [`DiscontinuityKind::DeviceReboot`]
    /// strictly when a reliable `boot_id` change occurred.
    pub fn on_hello(
        &mut self,
        hello: &HelloPayload,
        pc_cont_ns: i64,
    ) -> (ReconnectClassification, Option<DiscontinuityEvent>) {
        let (classification, discontinuity) =
            match (self.last_boot_id.as_deref(), self.last_boot_id_source) {
                (None, _) => (
                    ReconnectClassification::InitialConnection {
                        boot_id: hello.boot_id.clone(),
                        boot_id_source: hello.boot_id_source,
                    },
                    None,
                ),
                (Some(prev_id), Some(prev_src))
                    if prev_src == hello.boot_id_source
                        && prev_src.is_reliable_for_reboot_detection() =>
                {
                    if prev_id == hello.boot_id {
                        (
                            ReconnectClassification::ConnectionRestored {
                                boot_id: hello.boot_id.clone(),
                            },
                            None,
                        )
                    } else {
                        let event = DiscontinuityEvent {
                            kind: DiscontinuityKind::DeviceReboot,
                            stream: None,
                            boot_id: Some(hello.boot_id.clone()),
                            start_boottime_ns: None,
                            end_boottime_ns: None,
                            pc_cont_start_ns: pc_cont_ns,
                            pc_cont_end_ns: Some(pc_cont_ns),
                            delta_ns: None,
                            detail: Some(format!(
                                "boot_id changed from '{prev_id}' to '{}'",
                                hello.boot_id
                            )),
                        };
                        (
                            ReconnectClassification::DeviceReboot {
                                previous_boot_id: prev_id.to_owned(),
                                new_boot_id: hello.boot_id.clone(),
                            },
                            Some(event),
                        )
                    }
                }
                (prev_id, prev_src) => {
                    let reason = match prev_src {
                        Some(prev) if prev != hello.boot_id_source => format!(
                            "boot_id_source changed from {prev:?} to {:?}; boot_id formats are \
                             not comparable, reboot vs restore unverified",
                            hello.boot_id_source
                        ),
                        _ => "boot_id_source is FallbackRandomUuid; reboot vs restore unverified"
                            .to_owned(),
                    };
                    (
                        ReconnectClassification::ContinuityUnverified {
                            previous_boot_id: prev_id.map(str::to_owned),
                            new_boot_id: hello.boot_id.clone(),
                            reason,
                        },
                        None,
                    )
                }
            };

        self.last_boot_id = Some(hello.boot_id.clone());
        self.last_boot_id_source = Some(hello.boot_id_source);
        self.reconnect_attempts = 0;
        self.stream_states.clear();
        self.stream_states
            .insert(StreamId::Control, StreamLifecycleState::Running);
        // Host-observed media transport loss (scrcpy socket EOF) is independent of the
        // companion connection and survives a companion reconnect.
        for lost in &self.media_transport_lost {
            self.stream_states
                .insert(*lost, StreamLifecycleState::Error);
        }
        self.state = SupervisorState::Connected {
            boot_id: hello.boot_id.clone(),
            boot_id_source: hello.boot_id_source,
        };

        (classification, discontinuity)
    }

    /// Records a transport disconnect, schedules exponential backoff, and emits
    /// a [`DiscontinuityKind::ConnectionLost`] event.
    pub fn on_disconnect(
        &mut self,
        pc_cont_ns: i64,
        reason: &str,
    ) -> (Duration, DiscontinuityEvent) {
        let backoff = self
            .config
            .reconnect_policy
            .delay_for_attempt(self.reconnect_attempts);
        let backoff_ms = self
            .config
            .reconnect_policy
            .delay_ms_for_attempt(self.reconnect_attempts);
        self.reconnect_attempts = self.reconnect_attempts.saturating_add(1);
        self.state = SupervisorState::Reconnecting {
            attempt: self.reconnect_attempts,
            next_backoff_ms: backoff_ms,
            last_boot_id: self.last_boot_id.clone(),
            last_boot_id_source: self.last_boot_id_source,
        };
        let event = DiscontinuityEvent {
            kind: DiscontinuityKind::ConnectionLost,
            stream: None,
            boot_id: self.last_boot_id.clone(),
            start_boottime_ns: self.last_beacon_boottime_ns,
            end_boottime_ns: None,
            pc_cont_start_ns: pc_cont_ns,
            pc_cont_end_ns: None,
            delta_ns: None,
            detail: Some(reason.to_owned()),
        };
        (backoff, event)
    }

    /// Records a failed reconnect attempt and returns the next bounded backoff delay.
    pub fn on_reconnect_attempt_failed(&mut self) -> Duration {
        let backoff = self
            .config
            .reconnect_policy
            .delay_for_attempt(self.reconnect_attempts);
        let backoff_ms = self
            .config
            .reconnect_policy
            .delay_ms_for_attempt(self.reconnect_attempts);
        self.reconnect_attempts = self.reconnect_attempts.saturating_add(1);
        self.state = SupervisorState::Reconnecting {
            attempt: self.reconnect_attempts,
            next_backoff_ms: backoff_ms,
            last_boot_id: self.last_boot_id.clone(),
            last_boot_id_source: self.last_boot_id_source,
        };
        backoff
    }

    /// Updates beacon freshness and per-stream health states from a `Heartbeat` frame.
    pub fn on_heartbeat(&mut self, hb: &HeartbeatPayload, pc_cont_ns: i64) {
        self.last_beacon_pc_cont_ns = Some(pc_cont_ns);
        self.last_beacon_boottime_ns = Some(hb.boottime_ns);
        for entry in &hb.streams {
            // The companion cannot observe the scrcpy sockets: never let a heartbeat
            // resurrect a media stream whose host-side transport is known dead.
            if self.media_transport_lost.contains(&entry.stream_id) {
                continue;
            }
            self.stream_states.insert(entry.stream_id, entry.state);
        }
    }

    /// Records arrival of a decoded video frame on `pc_cont_ns`.
    pub fn on_video_frame(&mut self, pc_cont_ns: i64) {
        self.last_video_frame_pc_cont_ns = Some(pc_cont_ns);
        self.media_transport_lost.remove(&StreamId::Video);
        self.stream_states
            .insert(StreamId::Video, StreamLifecycleState::Running);
    }

    /// Records that the host-side transport of media stream `stream_id` (`Video` / `Audio`
    /// scrcpy socket) ended or failed, transitions it to [`StreamLifecycleState::Error`],
    /// and returns the [`DiscontinuityKind::StreamGap`] to record.
    ///
    /// After this, [`Self::evaluate_video_liveness`] reports `StreamGap` (never
    /// `StaticHold`) for a dead video encoder, and heartbeats cannot flip it back to
    /// `Running`; only a newly received frame ([`Self::on_video_frame`]) does.
    pub fn on_media_transport_lost(
        &mut self,
        stream_id: StreamId,
        pc_cont_ns: i64,
        reason: &str,
    ) -> DiscontinuityEvent {
        self.media_transport_lost.insert(stream_id);
        self.stream_states
            .insert(stream_id, StreamLifecycleState::Error);
        DiscontinuityEvent {
            kind: DiscontinuityKind::StreamGap,
            stream: stream_id.to_core_stream_kind(),
            boot_id: self.last_boot_id.clone(),
            start_boottime_ns: self.last_beacon_boottime_ns,
            end_boottime_ns: None,
            pc_cont_start_ns: pc_cont_ns,
            pc_cont_end_ns: None,
            delta_ns: None,
            detail: Some(format!("{stream_id:?} transport lost on host: {reason}")),
        }
    }

    /// Applies an explicit `StreamState` transition and emits a [`DiscontinuityKind::StreamGap`]
    /// when a data stream enters `Error` (or unexpected non-shutdown `Stopped`).
    pub fn on_stream_state(
        &mut self,
        payload: &StreamStatePayload,
        pc_cont_ns: i64,
    ) -> Option<DiscontinuityEvent> {
        self.stream_states
            .insert(payload.target_stream_id, payload.state);

        let is_fault = match payload.state {
            StreamLifecycleState::Error => true,
            StreamLifecycleState::Stopped => {
                payload.reason != "shutdown" && payload.reason != "no_touch_device"
            }
            StreamLifecycleState::Starting
            | StreamLifecycleState::Running
            | StreamLifecycleState::Restarting => false,
        };

        if !is_fault {
            return None;
        }

        let stream = payload.target_stream_id.to_core_stream_kind();
        Some(DiscontinuityEvent {
            kind: DiscontinuityKind::StreamGap,
            stream,
            boot_id: self.last_boot_id.clone(),
            start_boottime_ns: Some(payload.boottime_ns),
            end_boottime_ns: None,
            pc_cont_start_ns: pc_cont_ns,
            pc_cont_end_ns: None,
            delta_ns: None,
            detail: Some(format!(
                "stream {:?} transitioned to {:?}: {}",
                payload.target_stream_id, payload.state, payload.reason
            )),
        })
    }

    /// Classifies current video liveness at host time `now_pc_cont_ns` into
    /// [`VideoLivenessStatus::Active`], [`VideoLivenessStatus::StaticHold`],
    /// or [`VideoLivenessStatus::StreamGap`].
    pub fn evaluate_video_liveness(&self, now_pc_cont_ns: i64) -> VideoLivenessStatus {
        let beacon_state = self.stream_states.get(&StreamId::Control).copied();
        if beacon_state != Some(StreamLifecycleState::Running) {
            return VideoLivenessStatus::StreamGap {
                reason: format!("beacon_not_running:{beacon_state:?}"),
            };
        }

        let Some(last_beacon_pc) = self.last_beacon_pc_cont_ns else {
            return VideoLivenessStatus::StreamGap {
                reason: "no_beacon_received".to_owned(),
            };
        };
        let beacon_age = now_pc_cont_ns.saturating_sub(last_beacon_pc);
        if beacon_age > self.config.beacon_timeout_ns {
            return VideoLivenessStatus::StreamGap {
                reason: format!("beacon_timeout_ns:{beacon_age}"),
            };
        }

        let video_state = self.stream_states.get(&StreamId::Video).copied();
        if video_state != Some(StreamLifecycleState::Running) {
            return VideoLivenessStatus::StreamGap {
                reason: format!("video_state_not_running:{video_state:?}"),
            };
        }

        let Some(last_video_pc) = self.last_video_frame_pc_cont_ns else {
            return VideoLivenessStatus::StreamGap {
                reason: "no_video_frame_received".to_owned(),
            };
        };
        let silent_duration_ns = now_pc_cont_ns.saturating_sub(last_video_pc);
        if silent_duration_ns >= self.config.static_hold_threshold_ns {
            return VideoLivenessStatus::StaticHold { silent_duration_ns };
        }

        VideoLivenessStatus::Active
    }

    /// Transitions the supervisor permanently into [`SupervisorState::Stopped`].
    pub fn stop(&mut self) {
        self.state = SupervisorState::Stopped;
    }
}

// ---------------- Live AdbSessionSource (Implements DeviceEventSource) ----------------

/// Host clock shared by the live reader threads (each stamps `pc_cont_ns` on read completion).
type SharedClock = Arc<Mutex<Box<dyn FnMut() -> i64 + Send>>>;

fn stamp(clock: &SharedClock) -> i64 {
    let mut guard = clock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    (guard)()
}

/// One read completion from a live per-stream reader thread, stamped on that thread.
#[derive(Debug)]
enum LiveMsg {
    Companion(i64, Result<Option<WireFrame>, WireError>),
    Video(i64, Result<Option<VideoRecord>, ScrcpyError>),
    Audio(i64, Result<Option<AudioPacket>, ScrcpyError>),
}

/// Pumps `read` on a dedicated thread until EOF/error, stamping each completion with
/// the shared host clock immediately after the read returns.
fn spawn_live_reader<T: 'static, E: 'static>(
    name: &str,
    tx: SyncSender<LiveMsg>,
    clock: SharedClock,
    mut read: impl FnMut() -> Result<Option<T>, E> + Send + 'static,
    wrap: fn(i64, Result<Option<T>, E>) -> LiveMsg,
) {
    let spawned = thread::Builder::new()
        .name(format!("arb-live-{name}"))
        .spawn(move || {
            loop {
                let result = read();
                let pc_cont_ns = stamp(&clock);
                let finished = !matches!(result, Ok(Some(_)));
                if tx.send(wrap(pc_cont_ns, result)).is_err() || finished {
                    break;
                }
            }
        });
    // Thread creation only fails on OS resource exhaustion; the session cannot work then.
    spawned.expect("failed to spawn live AdbSessionSource reader thread");
}

/// Zero-sized marker keeping the transport type parameters (always `Send + Sync`).
type TransportMarker<C, V, A> = std::marker::PhantomData<fn() -> (C, V, A)>;

/// Coarse classification of [`VideoLivenessStatus`] used to emit one event per episode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LivenessKind {
    Active,
    StaticHold,
    StreamGap,
}

/// Live [`DeviceEventSource`] ingesting framed companion wire messages and optional
/// `scrcpy-server-v5.0.1` video/audio tunnels through [`SessionSupervisor`] and [`ClockTimeline`].
///
/// Each transport (companion, video, audio) is read on its own thread feeding one bounded
/// channel, so the scrcpy sockets are drained continuously while the companion stays open
/// (independent error boundaries per SPEC-03 §1), and `pc_cont_ns` is stamped the moment
/// each read completes. [`DeviceEventSource::next_event`] only pops from that channel and
/// runs a periodic video liveness tick ([`Self::with_liveness_tick_interval`]).
///
/// Transport ends are reported, never swallowed:
/// - companion EOF / wire error → [`SessionSupervisor::on_disconnect`] and a
///   [`DiscontinuityKind::ConnectionLost`] event;
/// - video / audio EOF / error → [`SessionSupervisor::on_media_transport_lost`] and a
///   [`DiscontinuityKind::StreamGap`] for that stream (a dead scrcpy-server is never
///   reported as `StaticHold`).
///
/// After a companion `Shutdown`, transport ends are treated as graceful teardown.
pub struct AdbSessionSource<C, V = std::io::Empty, A = std::io::Empty> {
    rx: Receiver<LiveMsg>,
    supervisor: SessionSupervisor,
    timeline: ClockTimeline,
    clock: SharedClock,
    pending: VecDeque<CaptureEvent>,
    companion_eof: bool,
    video_eof: bool,
    audio_eof: bool,
    suppress_next_timeline_reboot_for: Option<String>,
    liveness_tick_interval: Duration,
    last_liveness_tick: Instant,
    last_liveness_kind: Option<LivenessKind>,
    shutdown_handles: Vec<TcpStream>,
    _transports: TransportMarker<C, V, A>,
}

impl<C, V, A> std::fmt::Debug for AdbSessionSource<C, V, A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdbSessionSource")
            .field("serial", &self.supervisor.serial())
            .field("state", &self.supervisor.state())
            .field("pending_len", &self.pending.len())
            .field("companion_eof", &self.companion_eof)
            .field("video_eof", &self.video_eof)
            .field("audio_eof", &self.audio_eof)
            .finish_non_exhaustive()
    }
}

impl<C, V, A> Drop for AdbSessionSource<C, V, A> {
    fn drop(&mut self) {
        // Unblock reader threads parked in a socket read; they exit once the channel
        // receiver is gone. Best effort: the peer may already have closed.
        for handle in &self.shutdown_handles {
            let _ = handle.shutdown(Shutdown::Both);
        }
    }
}

impl<C, V, A> AdbSessionSource<C, V, A>
where
    C: Read + Send + 'static,
    V: Read + Send + 'static,
    A: Read + Send + 'static,
{
    /// Creates an [`AdbSessionSource`] from arbitrary [`Read`] transports and a host clock
    /// provider, and immediately starts one reader thread per present transport.
    ///
    /// # Panics
    ///
    /// Panics if the OS refuses to create a reader thread.
    pub fn new(
        serial: impl Into<String>,
        companion: C,
        video: Option<VideoTunnelReader<V>>,
        audio: Option<AudioTunnelReader<A>>,
        supervisor_config: SupervisorConfig,
        timeline_config: ClockTimelineConfig,
        clock_fn: impl FnMut() -> i64 + Send + 'static,
    ) -> Self {
        let clock: SharedClock = Arc::new(Mutex::new(Box::new(clock_fn)));
        let (tx, rx) = sync_channel(LIVE_EVENT_CHANNEL_CAPACITY);
        let video_eof = video.is_none();
        let audio_eof = audio.is_none();

        let mut companion = companion;
        spawn_live_reader(
            "companion",
            tx.clone(),
            Arc::clone(&clock),
            move || WireFrame::read_from(&mut companion, DEFAULT_MAX_PAYLOAD_LEN),
            LiveMsg::Companion,
        );
        if let Some(mut video_reader) = video {
            spawn_live_reader(
                "video",
                tx.clone(),
                Arc::clone(&clock),
                move || video_reader.read_next(),
                LiveMsg::Video,
            );
        }
        if let Some(mut audio_reader) = audio {
            spawn_live_reader(
                "audio",
                tx.clone(),
                Arc::clone(&clock),
                move || audio_reader.read_next(),
                LiveMsg::Audio,
            );
        }
        drop(tx);

        Self {
            rx,
            supervisor: SessionSupervisor::new(serial, supervisor_config),
            timeline: ClockTimeline::new(timeline_config),
            clock,
            pending: VecDeque::new(),
            companion_eof: false,
            video_eof,
            audio_eof,
            suppress_next_timeline_reboot_for: None,
            liveness_tick_interval: DEFAULT_LIVENESS_TICK_INTERVAL,
            last_liveness_tick: Instant::now(),
            last_liveness_kind: None,
            shutdown_handles: Vec::new(),
            _transports: std::marker::PhantomData,
        }
    }
}

impl<C, V, A> AdbSessionSource<C, V, A> {
    /// Overrides the wall-clock period of the live video liveness tick
    /// (default [`DEFAULT_LIVENESS_TICK_INTERVAL`]).
    #[must_use]
    pub fn with_liveness_tick_interval(mut self, interval: Duration) -> Self {
        self.liveness_tick_interval = interval.max(Duration::from_millis(1));
        self
    }

    /// Returns a reference to the underlying [`SessionSupervisor`].
    pub const fn supervisor(&self) -> &SessionSupervisor {
        &self.supervisor
    }

    /// Returns a reference to the underlying [`ClockTimeline`].
    pub const fn timeline(&self) -> &ClockTimeline {
        &self.timeline
    }

    fn current_boot_id(&self) -> String {
        self.supervisor
            .current_boot_id()
            .unwrap_or("unknown")
            .to_owned()
    }

    fn ingest_hello(&mut self, hello: HelloPayload, pc_cont_ns: i64) {
        let prev_attempts = self.supervisor.reconnect_attempts();
        let (classification, reboot_disc) = self.supervisor.on_hello(&hello, pc_cont_ns);
        let is_restored = matches!(
            classification,
            ReconnectClassification::ConnectionRestored { .. }
        );
        if matches!(
            classification,
            ReconnectClassification::DeviceReboot { .. }
                | ReconnectClassification::ContinuityUnverified { .. }
        ) {
            // DeviceReboot: already reported by the supervisor. ContinuityUnverified:
            // PROTOCOL §3.1 forbids asserting a reboot. Either way the timeline must not
            // emit its own DeviceReboot for this boot_id.
            self.suppress_next_timeline_reboot_for = Some(hello.boot_id.clone());
        }
        let boot_id = hello.boot_id.clone();
        self.pending.push_back(CaptureEvent::Hello {
            pc_cont_ns,
            payload: hello,
            classification,
        });
        if let Some(disc) = reboot_disc {
            self.pending.push_back(CaptureEvent::Discontinuity(disc));
        }
        if is_restored {
            self.pending.push_back(CaptureEvent::ConnectionRestored {
                pc_cont_ns,
                boot_id,
                reconnect_attempts: prev_attempts,
            });
        }
    }

    fn ingest_heartbeat(&mut self, hb: HeartbeatPayload, pc_cont_ns: i64) {
        self.supervisor.on_heartbeat(&hb, pc_cont_ns);
        let boot_id = self.current_boot_id();
        let triple = DeviceClockTriple {
            boot_id: boot_id.clone(),
            boottime_ns: hb.boottime_ns,
            monotonic_ns: hb.monotonic_ns,
            realtime_ns: hb.realtime_ns,
        };
        let discs = self.timeline.ingest_device_triple(triple, pc_cont_ns);
        self.pending.push_back(CaptureEvent::Heartbeat {
            pc_cont_ns,
            boot_id: boot_id.clone(),
            payload: hb,
        });
        for disc in discs {
            if disc.kind == DiscontinuityKind::DeviceReboot
                && self.suppress_next_timeline_reboot_for.as_deref() == Some(boot_id.as_str())
            {
                self.suppress_next_timeline_reboot_for = None;
                continue;
            }
            self.pending.push_back(CaptureEvent::Discontinuity(disc));
        }
    }

    fn ingest_companion_frame(
        &mut self,
        frame: &WireFrame,
        pc_cont_ns: i64,
    ) -> Result<(), CaptureSourceError> {
        let msg = WireMessage::decode_frame(frame)?;
        let flags = frame.header.flags;
        match msg {
            WireMessage::Hello(hello) => self.ingest_hello(hello, pc_cont_ns),
            WireMessage::Heartbeat(hb) => self.ingest_heartbeat(hb, pc_cont_ns),
            WireMessage::StreamState(st) => {
                let boot_id = self.current_boot_id();
                let disc = self.supervisor.on_stream_state(&st, pc_cont_ns);
                self.pending.push_back(CaptureEvent::StreamState {
                    pc_cont_ns,
                    boot_id,
                    payload: st,
                });
                if let Some(d) = disc {
                    self.pending.push_back(CaptureEvent::Discontinuity(d));
                }
            }
            WireMessage::LogRecord(rec) => {
                let boot_id = self.current_boot_id();
                self.pending.push_back(CaptureEvent::LogRecord {
                    pc_cont_ns,
                    boot_id,
                    flags,
                    payload: rec,
                });
            }
            WireMessage::LogOverflowStats(stats) => {
                let boot_id = self.current_boot_id();
                self.pending.push_back(CaptureEvent::LogOverflowStats {
                    pc_cont_ns,
                    boot_id,
                    payload: stats,
                });
            }
            WireMessage::TouchEvent(touch) => {
                let boot_id = self.current_boot_id();
                self.pending.push_back(CaptureEvent::TouchEvent {
                    pc_cont_ns,
                    boot_id,
                    flags,
                    payload: touch,
                    display_value: None,
                    normalized: None,
                });
            }
            WireMessage::Shutdown(shutdown) => {
                self.supervisor.stop();
                self.pending.push_back(CaptureEvent::Shutdown {
                    pc_cont_ns,
                    payload: shutdown,
                });
            }
            WireMessage::Ping(_) | WireMessage::Pong(_) => {}
        }
        Ok(())
    }

    fn is_shutting_down(&self) -> bool {
        matches!(self.supervisor.state(), SupervisorState::Stopped)
    }

    fn on_companion_end(&mut self, pc_cont_ns: i64, reason: &str) {
        self.companion_eof = true;
        if self.is_shutting_down() {
            return;
        }
        let (_backoff, disc) = self.supervisor.on_disconnect(pc_cont_ns, reason);
        self.pending.push_back(CaptureEvent::Discontinuity(disc));
    }

    fn on_media_end(&mut self, stream_id: StreamId, pc_cont_ns: i64, reason: &str) {
        match stream_id {
            StreamId::Video => {
                self.video_eof = true;
                // The gap is reported here; the liveness tick must not re-report it.
                self.last_liveness_kind = Some(LivenessKind::StreamGap);
            }
            _ => self.audio_eof = true,
        }
        if self.is_shutting_down() {
            return;
        }
        let disc = self
            .supervisor
            .on_media_transport_lost(stream_id, pc_cont_ns, reason);
        self.pending.push_back(CaptureEvent::Discontinuity(disc));
    }

    fn ingest_live_msg(&mut self, msg: LiveMsg) -> Result<(), CaptureSourceError> {
        match msg {
            LiveMsg::Companion(pc_cont_ns, Ok(Some(frame))) => {
                self.ingest_companion_frame(&frame, pc_cont_ns)?;
            }
            LiveMsg::Companion(pc_cont_ns, Ok(None)) => {
                self.on_companion_end(pc_cont_ns, "companion_eof");
            }
            LiveMsg::Companion(pc_cont_ns, Err(err)) => {
                self.on_companion_end(pc_cont_ns, &format!("companion_wire_error: {err}"));
            }
            LiveMsg::Video(pc_cont_ns, Ok(Some(record))) => {
                let boot_id = self.current_boot_id();
                match record {
                    VideoRecord::SessionMeta(meta) => {
                        self.pending.push_back(CaptureEvent::VideoSession {
                            pc_cont_ns,
                            boot_id,
                            meta,
                        });
                    }
                    VideoRecord::Packet(packet) => {
                        if !packet.is_config() {
                            self.supervisor.on_video_frame(pc_cont_ns);
                        }
                        self.pending.push_back(CaptureEvent::VideoFrame {
                            pc_cont_ns,
                            boot_id,
                            packet,
                        });
                    }
                }
            }
            LiveMsg::Video(pc_cont_ns, Ok(None)) => {
                self.on_media_end(StreamId::Video, pc_cont_ns, "scrcpy_video_eof");
            }
            LiveMsg::Video(pc_cont_ns, Err(err)) => {
                self.on_media_end(
                    StreamId::Video,
                    pc_cont_ns,
                    &format!("scrcpy_video_error: {err}"),
                );
            }
            LiveMsg::Audio(pc_cont_ns, Ok(Some(packet))) => {
                let boot_id = self.current_boot_id();
                let event = match packet.kind {
                    AudioPacketKind::Config => CaptureEvent::AudioConfig {
                        pc_cont_ns,
                        boot_id,
                        packet,
                    },
                    AudioPacketKind::Frame => CaptureEvent::AudioFrame {
                        pc_cont_ns,
                        boot_id,
                        packet,
                    },
                };
                self.pending.push_back(event);
            }
            LiveMsg::Audio(pc_cont_ns, Ok(None)) => {
                self.on_media_end(StreamId::Audio, pc_cont_ns, "scrcpy_audio_eof");
            }
            LiveMsg::Audio(pc_cont_ns, Err(err)) => {
                self.on_media_end(
                    StreamId::Audio,
                    pc_cont_ns,
                    &format!("scrcpy_audio_error: {err}"),
                );
            }
        }
        Ok(())
    }

    /// Periodic video liveness evaluation (the live counterpart of the mock's
    /// `EvaluateVideoLiveness` step). Emits one event per liveness episode.
    fn maybe_tick_liveness(&mut self) {
        if self.video_eof || self.last_liveness_tick.elapsed() < self.liveness_tick_interval {
            return;
        }
        self.last_liveness_tick = Instant::now();
        let connected = matches!(self.supervisor.state(), SupervisorState::Connected { .. });
        if !connected
            || self.supervisor.last_beacon_pc_cont_ns.is_none()
            || self.supervisor.last_video_frame_pc_cont_ns.is_none()
        {
            return;
        }
        let now_pc = stamp(&self.clock);
        let status = self.supervisor.evaluate_video_liveness(now_pc);
        let kind = match status {
            VideoLivenessStatus::Active => LivenessKind::Active,
            VideoLivenessStatus::StaticHold { .. } => LivenessKind::StaticHold,
            VideoLivenessStatus::StreamGap { .. } => LivenessKind::StreamGap,
        };
        if self.last_liveness_kind == Some(kind) {
            return;
        }
        self.last_liveness_kind = Some(kind);
        let boot_id = self.current_boot_id();
        match status {
            VideoLivenessStatus::Active => {}
            VideoLivenessStatus::StaticHold { silent_duration_ns } => {
                self.pending.push_back(CaptureEvent::StaticHold {
                    pc_cont_ns: now_pc,
                    boot_id,
                    silent_duration_ns,
                });
            }
            VideoLivenessStatus::StreamGap { reason } => {
                self.pending
                    .push_back(CaptureEvent::Discontinuity(DiscontinuityEvent {
                        kind: DiscontinuityKind::StreamGap,
                        stream: Some(StreamKind::Video),
                        boot_id: Some(boot_id),
                        start_boottime_ns: None,
                        end_boottime_ns: None,
                        pc_cont_start_ns: now_pc,
                        pc_cont_end_ns: None,
                        delta_ns: None,
                        detail: Some(reason),
                    }));
            }
        }
    }

    const fn all_transports_finished(&self) -> bool {
        self.companion_eof && self.video_eof && self.audio_eof
    }
}

impl AdbSessionSource<TcpStream, TcpStream, TcpStream> {
    /// Connects live forwarded TCP sockets (`adb forward tcp:0 localabstract:...`) into an [`AdbSessionSource`].
    ///
    /// Dropping the returned source shuts the sockets down so the reader threads exit.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureSourceError::Scrcpy`] if the video or audio tunnel handshake fails,
    /// or [`CaptureSourceError::Io`] if a socket handle cannot be duplicated.
    ///
    /// # Panics
    ///
    /// Panics if the OS refuses to create a reader thread.
    pub fn from_tcp_streams(
        serial: impl Into<String>,
        companion: TcpStream,
        video: Option<TcpStream>,
        audio: Option<TcpStream>,
        supervisor_config: SupervisorConfig,
        timeline_config: ClockTimelineConfig,
        clock_fn: impl FnMut() -> i64 + Send + 'static,
    ) -> Result<Self, CaptureSourceError> {
        let mut shutdown_handles = vec![companion.try_clone()?];
        let video_reader = match video {
            Some(stream) => {
                shutdown_handles.push(stream.try_clone()?);
                Some(VideoTunnelReader::handshake(stream)?)
            }
            None => None,
        };
        let audio_reader = match audio {
            Some(stream) => {
                shutdown_handles.push(stream.try_clone()?);
                Some(AudioTunnelReader::handshake(stream)?)
            }
            None => None,
        };
        let mut source = Self::new(
            serial,
            companion,
            video_reader,
            audio_reader,
            supervisor_config,
            timeline_config,
            clock_fn,
        );
        source.shutdown_handles = shutdown_handles;
        Ok(source)
    }
}

impl<C, V, A> DeviceEventSource for AdbSessionSource<C, V, A> {
    fn serial(&self) -> &str {
        self.supervisor.serial()
    }

    fn next_event(&mut self) -> Result<Option<CaptureEvent>, CaptureSourceError> {
        loop {
            if let Some(ev) = self.pending.pop_front() {
                return Ok(Some(ev));
            }
            if self.all_transports_finished() {
                return Ok(None);
            }
            self.maybe_tick_liveness();
            if !self.pending.is_empty() {
                continue;
            }
            let wait = self
                .liveness_tick_interval
                .saturating_sub(self.last_liveness_tick.elapsed())
                .max(Duration::from_millis(1));
            match self.rx.recv_timeout(wait) {
                Ok(msg) => self.ingest_live_msg(msg)?,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    // Every reader thread is gone (they always report their end first,
                    // so this only follows an abnormal exit): report what is left.
                    let pc_cont_ns = stamp(&self.clock);
                    if !self.companion_eof {
                        self.on_companion_end(pc_cont_ns, "companion_reader_exited");
                    }
                    if !self.video_eof {
                        self.on_media_end(StreamId::Video, pc_cont_ns, "video_reader_exited");
                    }
                    if !self.audio_eof {
                        self.on_media_end(StreamId::Audio, pc_cont_ns, "audio_reader_exited");
                    }
                }
            }
        }
    }
}

/// Converts a wire [`StreamId`] into `arb_core::time::StreamKind` when it represents a data track.
pub const fn stream_id_to_core_kind(stream_id: StreamId) -> Option<StreamKind> {
    stream_id.to_core_stream_kind()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::io::Cursor;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use arb_core::time::{ClockTimelineConfig, DiscontinuityKind, StreamKind};

    use super::{
        AdbClient, AdbCommandOutput, AdbCommandRunner, AdbDeviceState, AdbError, AdbSessionSource,
        ReconnectClassification, ReconnectPolicy, SessionSupervisor, StreamEnable,
        SupervisorConfig, SupervisorState, VideoLivenessStatus, adb_executable_name,
        parse_devices_output, resolve_adb_binary_with,
    };
    use crate::mock::{CaptureEvent, DeviceEventSource};
    use crate::scrcpy::{
        CODEC_ID_H264, SESSION_META_TAG, ScrcpyLaunchOptions, VideoPacket, VideoPacketKind,
        VideoSessionMeta, VideoTunnelReader, write_video_handshake, write_video_packet,
        write_video_session_meta,
    };
    use crate::wire::{
        BootIdSource, FrameFlags, HeartbeatPayload, HelloPayload, StreamHealthEntry, StreamId,
        StreamLifecycleState, StreamStatePayload, WireMessage,
    };

    #[derive(Debug, Default)]
    struct MockRunner {
        invocations: RefCell<Vec<Vec<String>>>,
        responses: RefCell<Vec<AdbCommandOutput>>,
    }

    impl MockRunner {
        fn with_responses(responses: Vec<AdbCommandOutput>) -> Self {
            Self {
                invocations: RefCell::new(Vec::new()),
                responses: RefCell::new(responses),
            }
        }
    }

    impl AdbCommandRunner for MockRunner {
        fn run(
            &self,
            _program: &Path,
            args: &[String],
        ) -> Result<AdbCommandOutput, std::io::Error> {
            self.invocations.borrow_mut().push(args.to_vec());
            let mut responses = self.responses.borrow_mut();
            if responses.is_empty() {
                Ok(AdbCommandOutput {
                    exit_code: Some(0),
                    stdout: String::new(),
                    stderr: String::new(),
                })
            } else {
                Ok(responses.remove(0))
            }
        }
    }

    #[test]
    fn stale_forward_cleanup_matches_serial_port_and_socket() {
        let runner = MockRunner::with_responses(vec![AdbCommandOutput {
            exit_code:Some(0),stderr:String::new(),stdout:"A tcp:1234 localabstract:other\nB tcp:1235 localabstract:arb-token\nA tcp:1236 localabstract:arb-token\n".into()
        }]);
        let adb = AdbClient::with_runner(PathBuf::from("adb"), runner);
        adb.remove_owned_forwards(
            "A",
            &[
                (1234, "arb-token".into()),
                (1235, "arb-token".into()),
                (1236, "arb-token".into()),
            ],
        )
        .unwrap();
        let calls = adb.runner.invocations.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1], ["-s", "A", "forward", "--remove", "tcp:1236"]);
    }

    #[test]
    fn resolves_adb_binary_in_strict_priority_order() {
        let base = std::env::temp_dir().join(format!("arb-adb-discover-{}", std::process::id()));
        let env_dir = base.join("env-override");
        let path_dir = base.join("in-path");
        let sdk_dir = base.join("sdk").join("platform-tools");
        let home_sdk_dir = base
            .join("home")
            .join("Library")
            .join("Android")
            .join("sdk")
            .join("platform-tools");
        for dir in [&env_dir, &path_dir, &sdk_dir, &home_sdk_dir] {
            fs::create_dir_all(dir).expect("create temp test dir");
        }
        let exe = adb_executable_name();
        let env_adb = env_dir.join(exe);
        let path_adb = path_dir.join(exe);
        let sdk_adb = sdk_dir.join(exe);
        let home_adb = home_sdk_dir.join(exe);
        for file in [&env_adb, &path_adb, &sdk_adb, &home_adb] {
            fs::write(file, b"stub").expect("write stub adb");
        }

        // 1. ARB_ADB_PATH wins over PATH and ANDROID_HOME.
        let got1 = resolve_adb_binary_with(
            Some(&env_adb),
            Some(path_dir.as_os_str()),
            Some(&base.join("sdk")),
            Some(&base.join("home")),
        )
        .expect("ARB_ADB_PATH must resolve");
        assert_eq!(got1, env_adb, "ARB_ADB_PATH must have highest priority");

        // 2. PATH wins over ANDROID_HOME.
        let got2 = resolve_adb_binary_with(
            None,
            Some(path_dir.as_os_str()),
            Some(&base.join("sdk")),
            Some(&base.join("home")),
        )
        .expect("PATH must resolve");
        assert_eq!(got2, path_adb, "PATH must have second priority");

        // 3. ANDROID_HOME wins over HOME default.
        let got3 = resolve_adb_binary_with(
            None,
            None,
            Some(&base.join("sdk")),
            Some(&base.join("home")),
        )
        .expect("ANDROID_HOME must resolve");
        assert_eq!(got3, sdk_adb, "ANDROID_HOME must have third priority");

        // 4. HOME/Library/Android/sdk fallback.
        let got4 = resolve_adb_binary_with(None, None, None, Some(&base.join("home")))
            .expect("HOME SDK fallback must resolve");
        assert_eq!(got4, home_adb, "HOME fallback must resolve");

        // 5. Missing configured ARB_ADB_PATH returns InvalidConfiguredPath.
        let missing = base.join("does-not-exist-adb");
        assert!(
            matches!(
                resolve_adb_binary_with(Some(&missing), None, None, None),
                Err(AdbError::InvalidConfiguredPath { .. })
            ),
            "missing ARB_ADB_PATH must fail explicitly"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn parses_adb_devices_and_formats_launch_commands() {
        let devices_out = "\
* daemon not running; starting now at tcp:5037
* daemon started successfully
List of devices attached
emulator-5554          device product:sdk_gphone64_arm64 model:sdk_gphone64_arm64 device:emu64a transport_id:1
R5CX123456A            unauthorized usb:1-1 transport_id:2
";
        let parsed = parse_devices_output(devices_out);
        assert_eq!(parsed.len(), 2, "must parse 2 devices");
        assert_eq!(parsed[0].serial, "emulator-5554", "first serial");
        assert_eq!(parsed[0].state, AdbDeviceState::Device, "first state");
        assert_eq!(
            parsed[0].model.as_deref(),
            Some("sdk_gphone64_arm64"),
            "first model"
        );
        assert_eq!(
            parsed[1].state,
            AdbDeviceState::Unauthorized,
            "second state"
        );

        let launch_cmd = AdbClient::<MockRunner>::companion_launch_command(
            "/data/local/tmp/arb-sess-a1b2-companion.dex.jar",
            "arb-companion-sess-a1b2",
            300,
            StreamEnable::Enabled,
            StreamEnable::Disabled,
        );
        assert!(
            launch_cmd.contains("--no-input") && !launch_cmd.contains("--no-logcat"),
            "companion launch command must reflect stream toggles"
        );

        let scrcpy_cmd = AdbClient::<MockRunner>::scrcpy_launch_command(
            "/data/local/tmp/arb-sess-a1b2-scrcpy-audio.dex.jar",
            &ScrcpyLaunchOptions::fhd30("1234abcd"),
            32,
        );
        assert!(
            scrcpy_cmd.contains("audio=false") && scrcpy_cmd.contains("video_codec=h264"),
            "scrcpy command on API 32 must disable audio while preserving h264"
        );
    }

    #[test]
    fn adb_client_deploy_forward_cleanup_never_invokes_kill_server() {
        let ok = |stdout: &str| AdbCommandOutput {
            exit_code: Some(0),
            stdout: stdout.to_owned(),
            stderr: String::new(),
        };
        let runner = MockRunner::with_responses(vec![
            ok("List of devices attached\nemulator-5554 device\n"),
            ok("pushed"),
            ok("pushed"),
            ok("54321\n"),
            ok(""),
            ok(""),
        ]);
        let client = AdbClient::with_runner(PathBuf::from("/usr/bin/adb"), runner);
        assert_eq!(
            client.devices().expect("devices").len(),
            1,
            "devices call must return 1 entry"
        );
        let deployed = client
            .deploy_session_agents(
                "emulator-5554",
                "sess-a1b2",
                Path::new("/tmp/arb-companion.dex.jar"),
                Some(Path::new("/tmp/replay-scrcpy-audio.dex.jar")),
            )
            .expect("deploy must succeed");
        assert_eq!(
            deployed.companion_jar, "/data/local/tmp/arb-sess-a1b2-companion.dex.jar",
            "companion remote path must be session-scoped"
        );
        assert_eq!(
            deployed.scrcpy_jar.as_deref(),
            Some("/data/local/tmp/arb-sess-a1b2-scrcpy-audio.dex.jar"),
            "scrcpy remote path must be session-scoped"
        );

        let port = client
            .forward_tcp_to_abstract("emulator-5554", "arb-companion-sess-a1b2")
            .expect("forward must parse port");
        assert_eq!(port, 54321, "forwarded port must match adb output");

        let cleanup_errs = client.cleanup_session("emulator-5554", &deployed, &[port]);
        assert!(cleanup_errs.is_empty(), "cleanup must succeed cleanly");

        let calls = client.runner.invocations.borrow();
        assert!(
            calls
                .iter()
                .all(|args| !args.iter().any(|a| a == "kill-server")),
            "AdbClient must NEVER invoke adb kill-server"
        );
    }

    #[test]
    fn reconnect_policy_backoff_caps_at_2s() {
        let policy = ReconnectPolicy::new();
        assert_eq!(
            policy.delay_for_attempt(0),
            Duration::from_millis(200),
            "attempt 0 must be 200ms"
        );
        assert_eq!(
            policy.delay_for_attempt(1),
            Duration::from_millis(500),
            "attempt 1 must be 500ms"
        );
        assert_eq!(
            policy.delay_for_attempt(2),
            Duration::from_millis(1_000),
            "attempt 2 must be 1000ms"
        );
        assert_eq!(
            policy.delay_for_attempt(3),
            Duration::from_millis(2_000),
            "attempt 3 must be 2000ms"
        );
        assert_eq!(
            policy.delay_for_attempt(10),
            Duration::from_millis(2_000),
            "attempt 10 must remain capped at 2000ms"
        );
    }

    #[test]
    fn supervisor_classifies_restore_reboot_and_unverified_boot_id() {
        let mut sup = SessionSupervisor::new("emulator-5554", SupervisorConfig::new());
        let hello_a = HelloPayload {
            protocol_version: 1,
            api_level: 37,
            boot_count: 1,
            boot_id_source: BootIdSource::KernelProcBootId,
            is_emulator: true,
            heartbeat_interval_ms: 300,
            boot_id: "boot-A".into(),
            model: "sdk".into(),
            manufacturer: "Google".into(),
            agent_version: "0.1.0".into(),
            boot_id_detail: String::new(),
        };

        let (cls0, disc0) = sup.on_hello(&hello_a, 1_000);
        assert_eq!(
            cls0,
            ReconnectClassification::InitialConnection {
                boot_id: "boot-A".into(),
                boot_id_source: BootIdSource::KernelProcBootId,
            },
            "first Hello must be InitialConnection"
        );
        assert!(disc0.is_none(), "initial connect emits no discontinuity");

        let (d0, lost) = sup.on_disconnect(2_000, "usb_unplugged");
        assert_eq!(d0, Duration::from_millis(200), "first backoff 200ms");
        assert_eq!(
            lost.kind,
            DiscontinuityKind::ConnectionLost,
            "disconnect emits ConnectionLost"
        );
        assert_eq!(
            sup.on_reconnect_attempt_failed(),
            Duration::from_millis(500),
            "second backoff 500ms"
        );
        assert_eq!(
            sup.on_reconnect_attempt_failed(),
            Duration::from_millis(1_000),
            "third backoff 1000ms"
        );
        assert_eq!(
            sup.on_reconnect_attempt_failed(),
            Duration::from_millis(2_000),
            "fourth backoff 2000ms"
        );
        assert!(
            matches!(
                sup.state(),
                SupervisorState::Reconnecting { attempt: 4, .. }
            ),
            "supervisor must track attempt count in Reconnecting"
        );

        let (cls1, disc1) = sup.on_hello(&hello_a, 5_000);
        assert_eq!(
            cls1,
            ReconnectClassification::ConnectionRestored {
                boot_id: "boot-A".into()
            },
            "same boot_id must classify as ConnectionRestored"
        );
        assert!(disc1.is_none(), "same boot_id must not emit DeviceReboot");
        assert_eq!(sup.reconnect_attempts(), 0, "attempt count must reset");

        let (_d, _lost2) = sup.on_disconnect(6_000, "rebooting");
        let mut hello_b = hello_a;
        hello_b.boot_id = "boot-B".into();
        let (cls2, disc2) = sup.on_hello(&hello_b, 20_000);
        assert_eq!(
            cls2,
            ReconnectClassification::DeviceReboot {
                previous_boot_id: "boot-A".into(),
                new_boot_id: "boot-B".into(),
            },
            "changed reliable boot_id must classify as DeviceReboot"
        );
        assert_eq!(
            disc2.as_ref().map(|d| d.kind),
            Some(DiscontinuityKind::DeviceReboot),
            "changed boot_id must emit DeviceReboot discontinuity"
        );

        let mut hello_rand = hello_b;
        hello_rand.boot_id = "fallback:random:uuid".into();
        hello_rand.boot_id_source = BootIdSource::FallbackRandomUuid;
        let (cls3, disc3) = sup.on_hello(&hello_rand, 30_000);
        assert!(
            matches!(cls3, ReconnectClassification::ContinuityUnverified { .. }),
            "FallbackRandomUuid must classify ContinuityUnverified"
        );
        assert!(
            disc3.is_none(),
            "ContinuityUnverified must not fabricate DeviceReboot"
        );
    }

    #[test]
    fn supervisor_distinguishes_static_hold_from_stream_gap() {
        let mut sup = SessionSupervisor::new("emulator-5554", SupervisorConfig::new());
        let hb_running = HeartbeatPayload {
            boottime_ns: 12_000_000_000,
            monotonic_ns: 12_000_000_000,
            realtime_ns: 1_700_000_012_000_000_000,
            streams: vec![
                StreamHealthEntry {
                    stream_id: StreamId::Control,
                    state: StreamLifecycleState::Running,
                    restart_count: 0,
                    emitted_seq: 40,
                    last_pts_ns: 12_000_000_000,
                },
                StreamHealthEntry {
                    stream_id: StreamId::Video,
                    state: StreamLifecycleState::Running,
                    restart_count: 0,
                    emitted_seq: 1,
                    last_pts_ns: 0,
                },
            ],
        };
        sup.on_video_frame(0);
        sup.on_heartbeat(&hb_running, 12_000_000_000);

        // 12 seconds silent while Beacon & Video are Running -> StaticHold!
        assert_eq!(
            sup.evaluate_video_liveness(12_000_000_000),
            VideoLivenessStatus::StaticHold {
                silent_duration_ns: 12_000_000_000,
            },
            "silent video with healthy beacon and video Running must classify as StaticHold"
        );

        // Video transitions to Error -> StreamGap!
        let err_state = StreamStatePayload {
            target_stream_id: StreamId::Video,
            state: StreamLifecycleState::Error,
            restart_count: 0,
            boottime_ns: 13_000_000_000,
            emitted_seq: 1,
            reason: "encoder_stalled".into(),
        };
        let gap = sup
            .on_stream_state(&err_state, 13_000_000_000)
            .expect("video Error must emit StreamGap discontinuity");
        assert_eq!(gap.kind, DiscontinuityKind::StreamGap, "must be StreamGap");
        assert_eq!(gap.stream, Some(StreamKind::Video), "must target Video");
        assert!(
            matches!(
                sup.evaluate_video_liveness(13_000_000_000),
                VideoLivenessStatus::StreamGap { .. }
            ),
            "liveness must report StreamGap after video Error"
        );
    }

    #[test]
    fn adb_session_source_implements_device_event_source_over_wire_and_scrcpy() {
        let mut comp_bytes = Vec::new();
        let hello = WireMessage::Hello(HelloPayload {
            protocol_version: 1,
            api_level: 37,
            boot_count: 1,
            boot_id_source: BootIdSource::KernelProcBootId,
            is_emulator: true,
            heartbeat_interval_ms: 300,
            boot_id: "boot-live-1".into(),
            model: "Pixel".into(),
            manufacturer: "Google".into(),
            agent_version: "0.1.0".into(),
            boot_id_detail: String::new(),
        });
        comp_bytes.extend_from_slice(
            &hello
                .to_frame(StreamId::Control, FrameFlags::URGENT, 1)
                .expect("hello frame")
                .encode()
                .expect("encode hello"),
        );

        let mut vid_bytes = Vec::new();
        write_video_handshake(&mut vid_bytes, CODEC_ID_H264).expect("vid handshake");
        write_video_session_meta(
            &mut vid_bytes,
            VideoSessionMeta {
                header_tag: SESSION_META_TAG,
                width: 1920,
                height: 1080,
            },
        )
        .expect("vid meta");
        write_video_packet(
            &mut vid_bytes,
            &VideoPacket {
                kind: VideoPacketKind::Keyframe,
                pts_us: 1_000_000,
                data: vec![0, 0, 0, 1, 0x65],
            },
        )
        .expect("vid keyframe");

        let video_reader = VideoTunnelReader::handshake(Cursor::new(vid_bytes)).expect("handshake");
        let mut tick = 0i64;
        let mut source = AdbSessionSource::<_, _, std::io::Empty>::new(
            "emulator-5554",
            Cursor::new(comp_bytes),
            Some(video_reader),
            None,
            SupervisorConfig::new(),
            ClockTimelineConfig::new(),
            move || {
                tick += 10_000_000;
                tick
            },
        );

        let events = source.drain_all().expect("drain live source");
        let pos = |pred: &dyn Fn(&CaptureEvent) -> bool| events.iter().position(pred);
        let hello = pos(&|e| matches!(e, CaptureEvent::Hello { .. }));
        let session = pos(&|e| matches!(e, CaptureEvent::VideoSession { .. }));
        let frame = pos(&|e| matches!(e, CaptureEvent::VideoFrame { .. }));
        assert!(hello.is_some(), "must yield Hello");
        assert!(
            session.is_some() && frame.is_some() && session < frame,
            "VideoSession must precede VideoFrame (per-stream order is preserved)"
        );
        let lost = events
            .iter()
            .filter(|e| {
                matches!(e, CaptureEvent::Discontinuity(d) if d.kind == DiscontinuityKind::ConnectionLost)
            })
            .count();
        let video_gap = events
            .iter()
            .filter(|e| {
                matches!(e, CaptureEvent::Discontinuity(d)
                    if d.kind == DiscontinuityKind::StreamGap && d.stream == Some(StreamKind::Video))
            })
            .count();
        assert_eq!(
            lost, 1,
            "companion EOF must emit exactly one ConnectionLost"
        );
        assert_eq!(
            video_gap, 1,
            "video EOF must emit exactly one StreamGap(Video)"
        );
        assert_eq!(
            events.len(),
            5,
            "3 data events + 2 transport-end discontinuities"
        );
    }

    #[test]
    fn parse_wm_size_output_prefers_physical_and_handles_overrides() {
        // Raw getevent touch coordinates live in the physical panel space, so the physical
        // size must win over a (lower) `wm size` override for tap mapping.
        assert_eq!(
            super::parse_wm_size_output("Physical size: 1080x2400\r\n"),
            Some((1080, 2400))
        );
        assert_eq!(
            super::parse_wm_size_output("Physical size: 1440x3200\nOverride size: 1080x2400\n"),
            Some((1440, 3200)),
            "Physical size must take precedence over Override size"
        );
        assert_eq!(
            super::parse_wm_size_output("Override size: 1080x2400\nPhysical size: 1440x3200\n"),
            Some((1440, 3200)),
            "Physical size must take precedence regardless of line order"
        );
        assert_eq!(
            super::parse_wm_size_output("Override size: 720x1600\n"),
            Some((720, 1600))
        );
        assert_eq!(
            super::parse_wm_size_output("Physical size: 0x0\nOverride size: 720x1600\n"),
            Some((720, 1600)),
            "malformed Physical size falls back to Override size"
        );
        assert_eq!(
            super::parse_wm_size_output("Physical size: garbage\nOverride size: 1x\n"),
            None
        );
        assert_eq!(super::parse_wm_size_output("Physical size: 0x2400\n"), None);
        assert_eq!(super::parse_wm_size_output(""), None);
    }

    #[test]
    fn parse_package_uid_list_handles_standard_and_multi_user_lines() {
        let raw = "\
package:com.example.app uid:10234\r\n\
package:com.example.multi uid:10235,1010235\n\
package:com.google.android.gms uid:10012\n\
malformed line without prefix\n";
        let map = super::parse_package_uid_list(raw);
        assert_eq!(map.get(&10234).map(String::as_str), Some("com.example.app"));
        assert_eq!(
            map.get(&10235).map(String::as_str),
            Some("com.example.multi")
        );
        assert_eq!(
            map.get(&1_010_235).map(String::as_str),
            Some("com.example.multi")
        );
        assert_eq!(
            map.get(&10012).map(String::as_str),
            Some("com.google.android.gms")
        );
    }

    #[test]
    fn parse_package_uid_list_drops_system_and_shared_uids() {
        let raw = "\
package:android uid:1000\n\
package:com.android.settings uid:1000\n\
package:com.android.phone uid:1001\n\
package:com.google.android.gms uid:10012\n\
package:com.google.android.gsf uid:10012\n\
package:com.example.app uid:10234,1010234\n";
        let map = super::parse_package_uid_list(raw);
        assert_eq!(
            map.get(&1000),
            None,
            "system_server uid must never map to an app"
        );
        assert_eq!(
            map.get(&1001),
            None,
            "system uids below 10000 are not app uids"
        );
        assert_eq!(map.get(&10012), None, "sharedUserId uids are ambiguous");
        assert_eq!(map.get(&10234).map(String::as_str), Some("com.example.app"));
        assert_eq!(
            map.get(&1_010_234).map(String::as_str),
            Some("com.example.app")
        );
    }

    #[test]
    fn resolve_adb_falls_back_to_default_sdk_under_minimal_gui_path() {
        let base =
            std::env::temp_dir().join(format!("arb-adb-fallback-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let fake_home = base.join("home");
        let mac_adb = fake_home
            .join("Library")
            .join("Android")
            .join("sdk")
            .join("platform-tools")
            .join(super::adb_executable_name());
        std::fs::create_dir_all(mac_adb.parent().expect("parent")).expect("mkdir");
        std::fs::write(&mac_adb, b"stub").expect("write adb");

        // Even with minimal PATH=/usr/bin:/bin and a non-existent ANDROID_HOME,
        // fallback must still locate ~/Library/Android/sdk/platform-tools/adb.
        let found = super::resolve_adb_binary_full(
            None,
            Some(std::ffi::OsStr::new("/usr/bin:/bin")),
            Some(&base.join("missing-sdk")),
            Some(&fake_home),
            None,
        )
        .expect("fallback adb discovery");
        assert_eq!(found, mac_adb);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn t6e_query_sdk_version_parses_api_28_and_handles_invalid_outputs() {
        assert_eq!(super::parse_sdk_version_output("28\r\n"), Some(28));
        assert_eq!(super::parse_sdk_version_output("\n  34 \n"), Some(34));
        assert_eq!(super::parse_sdk_version_output("0\n"), None);
        assert_eq!(super::parse_sdk_version_output(""), None);
        assert_eq!(super::parse_sdk_version_output("not-a-number\n"), None);

        let ok_runner = MockRunner::with_responses(vec![AdbCommandOutput {
            exit_code: Some(0),
            stdout: "28\r\n".to_owned(),
            stderr: String::new(),
        }]);
        let client = AdbClient::with_runner(PathBuf::from("/usr/bin/adb"), ok_runner);
        assert_eq!(client.query_sdk_version("R58M123456"), Some(28));
        let calls = client.runner.invocations.borrow();
        assert_eq!(
            calls[0],
            vec![
                "-s",
                "R58M123456",
                "shell",
                "getprop",
                "ro.build.version.sdk"
            ],
            "must query ro.build.version.sdk via adb shell getprop"
        );
    }

    #[test]
    fn t6e_query_package_uids_falls_back_to_pm_list_packages_on_aos9_vendor_rom() {
        // Case 1: `cmd package list packages -U` fails on a vendor Android 9 ROM -> falls back to `pm`.
        let runner_fail = MockRunner::with_responses(vec![
            AdbCommandOutput {
                exit_code: Some(1),
                stdout: String::new(),
                stderr: "Can't find service: package".to_owned(),
            },
            AdbCommandOutput {
                exit_code: Some(0),
                stdout: "package:com.work.app uid:10128\npackage:android uid:1000\n".to_owned(),
                stderr: String::new(),
            },
        ]);
        let client = AdbClient::with_runner(PathBuf::from("/usr/bin/adb"), runner_fail);
        let uids = client.query_package_uids("aos9-serial");
        assert_eq!(uids.get(&10128).map(String::as_str), Some("com.work.app"));
        assert_eq!(uids.get(&1000), None);
        {
            let calls = client.runner.invocations.borrow();
            assert_eq!(calls.len(), 2, "must try cmd package then pm");
            assert_eq!(
                calls[0],
                vec![
                    "-s",
                    "aos9-serial",
                    "shell",
                    "cmd",
                    "package",
                    "list",
                    "packages",
                    "-U"
                ]
            );
            assert_eq!(
                calls[1],
                vec!["-s", "aos9-serial", "shell", "pm", "list", "packages", "-U"]
            );
        }

        // Case 2: `cmd package` exits 0 with empty stdout -> still falls back to `pm`.
        let runner_empty = MockRunner::with_responses(vec![
            AdbCommandOutput {
                exit_code: Some(0),
                stdout: "\n".to_owned(),
                stderr: String::new(),
            },
            AdbCommandOutput {
                exit_code: Some(0),
                stdout: "package:com.work.fallback uid:10228\n".to_owned(),
                stderr: String::new(),
            },
        ]);
        let client2 = AdbClient::with_runner(PathBuf::from("/usr/bin/adb"), runner_empty);
        let uids2 = client2.query_package_uids("aos9-serial");
        assert_eq!(
            uids2.get(&10228).map(String::as_str),
            Some("com.work.fallback")
        );
    }
}

#[cfg(test)]
#[path = "adb_live_tests.rs"]
mod live_tests;
