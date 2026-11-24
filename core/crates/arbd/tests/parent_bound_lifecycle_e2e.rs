//! Parent-bound lifecycle of the real `arbd` binary (`--exit-on-stdin-eof`).
//!
//! Android Studio launches `arbd --exit-on-stdin-eof` and keeps the write end of its stdin pipe.
//! These tests play the parent: while the pipe is open `arbd` serves RPCs; dropping the write end
//! (what the kernel does when the parent exits, crashes or is `kill -9`ed) must shut `arbd` down
//! through the `system.shutdown` path — exit code 0, socket and lock released — so the next
//! instance starts at once. Without the flag the standalone behaviour is unchanged.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use arb_os::fs::SingleInstanceLock;
use arb_os::ipc::{DEFAULT_MAX_IPC_LINE_BYTES, LocalIpcStream, read_frame_line, write_frame_line};
use arb_os::paths::ArbPaths;
use serde_json::{Value, json};
use tempfile::TempDir;

/// The task contract: a dead parent stops `arbd` within 2 s.
const PARENT_GONE_EXIT_BUDGET: Duration = Duration::from_secs(1);

fn hermetic_paths(tmp: &TempDir) -> ArbPaths {
    ArbPaths {
        data_dir: tmp.path().join("data"),
        cache_dir: tmp.path().join("cache"),
        runtime_dir: tmp.path().join("run"),
    }
}

/// Kills the child on drop so a failed assertion never leaks a daemon.
struct Daemon {
    child: Child,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Whether the child is started parent-bound (`--exit-on-stdin-eof`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
    ParentBound,
    Standalone,
}

fn spawn_arbd(paths: &ArbPaths, missing_adb: &Path, lifecycle: Lifecycle) -> (Daemon, ChildStdin) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_arbd"));
    cmd.arg("--data-dir")
        .arg(&paths.data_dir)
        .arg("--cache-dir")
        .arg(&paths.cache_dir)
        .arg("--runtime-dir")
        .arg(&paths.runtime_dir);
    if lifecycle == Lifecycle::ParentBound {
        cmd.arg("--exit-on-stdin-eof");
    }
    let mut child = cmd
        // A missing configured adb keeps auto-attach away from any real host adb server.
        .env("ARB_ADB_PATH", missing_adb)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn arbd");
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let mut ready = String::new();
    BufReader::new(stdout)
        .read_line(&mut ready)
        .expect("read arbd ready line");
    assert!(
        ready.starts_with("arbd ready"),
        "arbd must report readiness, got {ready:?}"
    );
    (Daemon { child }, stdin)
}

fn rpc(endpoint: &Path, method: &str) -> Value {
    let stream = LocalIpcStream::connect(endpoint).expect("connect to arbd");
    let mut writer = stream.try_clone().expect("clone IPC stream");
    let mut reader = BufReader::new(stream);
    let line = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": {}}).to_string();
    write_frame_line(&mut writer, &line, DEFAULT_MAX_IPC_LINE_BYTES).expect("write frame");
    writer.drain().expect("drain frame");
    let raw = read_frame_line(&mut reader, DEFAULT_MAX_IPC_LINE_BYTES)
        .expect("read frame")
        .expect("response line");
    serde_json::from_str(&raw).expect("parse response JSON")
}

fn wait_exit(daemon: &mut Daemon, budget: Duration) -> Option<(ExitStatus, Duration)> {
    let started = Instant::now();
    while started.elapsed() < budget {
        if let Some(status) = daemon.child.try_wait().expect("poll arbd") {
            return Some((status, started.elapsed()));
        }
        thread::sleep(Duration::from_millis(10));
    }
    None
}

fn assert_released(paths: &ArbPaths) {
    let lock = SingleInstanceLock::try_acquire(paths.lock_file());
    assert!(
        lock.is_ok(),
        "arbd.lock must be released after exit: {lock:?}"
    );
    drop(lock);
    #[cfg(unix)]
    assert!(
        !paths.ipc_endpoint().exists(),
        "arbd.sock must be removed after a clean exit"
    );
    assert!(
        LocalIpcStream::connect(paths.ipc_endpoint()).is_err(),
        "nothing may still answer on the IPC endpoint"
    );
}

#[test]
fn parent_pipe_eof_shuts_arbd_down_cleanly_and_frees_the_next_start() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = hermetic_paths(&tmp);
    let missing_adb: PathBuf = tmp.path().join("no-adb");
    let endpoint = paths.ipc_endpoint();

    let (mut daemon, mut stdin) = spawn_arbd(&paths, &missing_adb, Lifecycle::ParentBound);
    // A live parent may write to the pipe; arbd must ignore the bytes and keep serving.
    stdin.write_all(b"keep-alive\n").expect("parent write");
    stdin.flush().expect("parent flush");
    thread::sleep(Duration::from_millis(300));
    let status = rpc(&endpoint, "system.status");
    assert!(
        status.get("error").is_none() && status.get("result").is_some(),
        "arbd must serve system.status while the parent pipe is open: {status}"
    );
    assert!(
        daemon.child.try_wait().expect("poll arbd").is_none(),
        "arbd must stay alive while the parent holds the pipe"
    );
    assert_eq!(
        rpc(&endpoint, "system.hello")["result"]["parent_bound"],
        true,
        "a parent-bound arbd must say so in system.hello"
    );

    // Parent death: the kernel closes the write end; here the test drops it.
    drop(stdin);
    let (exit, elapsed) = wait_exit(&mut daemon, PARENT_GONE_EXIT_BUDGET)
        .expect("arbd must exit within 1 s after the parent pipe closes");
    assert_eq!(
        exit.code(),
        Some(0),
        "parent EOF must take the clean shutdown path (exit 0), took {elapsed:?}"
    );
    assert_released(&paths);

    // The very next instance starts immediately and is again parent-bound.
    let (mut next, next_stdin) = spawn_arbd(&paths, &missing_adb, Lifecycle::ParentBound);
    assert!(
        rpc(&endpoint, "system.status").get("result").is_some(),
        "a fresh arbd must start right after the previous one exited"
    );
    drop(next_stdin);
    let (next_exit, _) = wait_exit(&mut next, PARENT_GONE_EXIT_BUDGET)
        .expect("second parent-bound arbd must also exit on EOF");
    assert_eq!(next_exit.code(), Some(0), "second instance exits cleanly");
    assert_released(&paths);
}

#[test]
fn parent_bound_arbd_still_honours_system_shutdown() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = hermetic_paths(&tmp);
    let missing_adb = tmp.path().join("no-adb");
    let (mut daemon, stdin) = spawn_arbd(&paths, &missing_adb, Lifecycle::ParentBound);

    let resp = rpc(&paths.ipc_endpoint(), "system.shutdown");
    assert_eq!(resp["result"]["ok"], true, "system.shutdown must succeed");
    let (exit, _) = wait_exit(&mut daemon, PARENT_GONE_EXIT_BUDGET)
        .expect("system.shutdown must still stop a parent-bound arbd while the pipe is open");
    assert_eq!(exit.code(), Some(0), "system.shutdown exits cleanly");
    drop(stdin);
    assert_released(&paths);
}

#[test]
fn without_the_flag_stdin_eof_does_not_stop_standalone_arbd() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = hermetic_paths(&tmp);
    let missing_adb = tmp.path().join("no-adb");
    let endpoint = paths.ipc_endpoint();
    let (mut daemon, stdin) = spawn_arbd(&paths, &missing_adb, Lifecycle::Standalone);

    assert_eq!(
        rpc(&endpoint, "system.hello")["result"]["parent_bound"],
        false,
        "a standalone arbd reports parent_bound = false"
    );
    drop(stdin);
    thread::sleep(Duration::from_millis(800));
    assert!(
        daemon.child.try_wait().expect("poll arbd").is_none(),
        "standalone arbd (no --exit-on-stdin-eof) must ignore stdin EOF"
    );
    assert!(
        rpc(&endpoint, "system.status").get("result").is_some(),
        "standalone arbd keeps serving after stdin EOF"
    );
    rpc(&endpoint, "system.shutdown");
    let (exit, _) =
        wait_exit(&mut daemon, Duration::from_secs(5)).expect("standalone arbd shuts down on RPC");
    assert_eq!(exit.code(), Some(0), "standalone shutdown exits cleanly");
}
