//! Parent-bound lifecycle of the real `arb-tray` binary (`--exit-on-stdin-eof`).
//!
//! Android Studio launches `arb-tray --exit-on-stdin-eof` and keeps the write end of its stdin
//! pipe. The test plays the parent: while the pipe is open the tray holds `arb-tray.lock`;
//! dropping the write end (what the kernel does when the parent exits, crashes or is `kill -9`ed)
//! must quit the tray through `QuitNow` with exit code 0 and release the lock. `--headless` runs
//! the same process lifecycle without a tray host, so this holds on CI runners with no GUI.

#![cfg(feature = "native-tray")]

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use arb_os::error::OsError;
use arb_os::fs::SingleInstanceLock;
use arb_os::paths::ArbPaths;

const PARENT_GONE_EXIT_BUDGET: Duration = Duration::from_secs(2);

struct Tray {
    child: Child,
}

impl Drop for Tray {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn hermetic_paths(root: &Path) -> ArbPaths {
    ArbPaths {
        data_dir: root.join("data"),
        cache_dir: root.join("cache"),
        runtime_dir: root.join("run"),
    }
}

/// Whether the child is started parent-bound (`--exit-on-stdin-eof`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
    ParentBound,
    Standalone,
}

fn spawn_tray(paths: &ArbPaths, lifecycle: Lifecycle) -> (Tray, ChildStdin) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_arb-tray"));
    cmd.arg("--headless")
        .arg("--data-dir")
        .arg(&paths.data_dir)
        .arg("--cache-dir")
        .arg(&paths.cache_dir)
        .arg("--runtime-dir")
        .arg(&paths.runtime_dir)
        .arg("--poll-interval-ms")
        .arg("100");
    if lifecycle == Lifecycle::ParentBound {
        cmd.arg("--exit-on-stdin-eof");
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn arb-tray");
    let stdin = child.stdin.take().expect("piped stdin");
    (Tray { child }, stdin)
}

/// Waits until the tray process holds `arb-tray.lock`.
fn await_lock_held(paths: &ArbPaths, tray: &mut Tray) {
    // A probe that takes the lock during startup can itself make the child
    // lose the election. The first status connection happens after election.
    let listener =
        arb_os::ipc::LocalIpcListener::bind(paths.ipc_endpoint()).expect("bind test daemon");
    let (ready, accepted) = std::sync::mpsc::sync_channel(1);
    let accept = thread::spawn(move || {
        let stream = listener.accept();
        drop(listener);
        let _ = ready.send(stream);
    });
    let connection = accepted
        .recv_timeout(Duration::from_secs(10))
        .expect("tray must initiate its status connection after taking the lock")
        .expect("accept status connection");
    accept.join().expect("status accept thread must finish");
    assert!(
        tray.child.try_wait().expect("poll tray").is_none(),
        "primary tray must be running"
    );
    assert!(
        matches!(
            SingleInstanceLock::try_acquire(paths.tray_lock_file()),
            Err(OsError::LockContended { .. })
        ),
        "primary tray must hold its single-instance lock"
    );
    drop(connection);
}

fn wait_exit(tray: &mut Tray, budget: Duration) -> Option<(ExitStatus, Duration)> {
    let started = Instant::now();
    while started.elapsed() < budget {
        if let Some(status) = tray.child.try_wait().expect("poll arb-tray") {
            return Some((status, started.elapsed()));
        }
        thread::sleep(Duration::from_millis(10));
    }
    None
}

#[test]
fn parent_pipe_eof_quits_the_tray_and_releases_its_lock() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = hermetic_paths(tmp.path());
    let (mut tray, mut stdin) = spawn_tray(&paths, Lifecycle::ParentBound);
    await_lock_held(&paths, &mut tray);

    stdin.write_all(b"keep-alive\n").expect("parent write");
    stdin.flush().expect("parent flush");
    thread::sleep(Duration::from_millis(400));
    assert!(
        tray.child.try_wait().expect("poll arb-tray").is_none(),
        "arb-tray must stay alive while the parent holds the pipe"
    );

    drop(stdin);
    let (exit, elapsed) = wait_exit(&mut tray, PARENT_GONE_EXIT_BUDGET)
        .expect("arb-tray must exit within 2 s after the parent pipe closes");
    assert_eq!(
        exit.code(),
        Some(0),
        "parent EOF must quit through QuitNow with exit 0 (took {elapsed:?})"
    );
    let relock = SingleInstanceLock::try_acquire(paths.tray_lock_file());
    assert!(relock.is_ok(), "arb-tray.lock must be released: {relock:?}");
    drop(relock);

    // The next parent-bound tray starts at once (no stale lock) and is bound again.
    let (mut next, next_stdin) = spawn_tray(&paths, Lifecycle::ParentBound);
    await_lock_held(&paths, &mut next);
    drop(next_stdin);
    let (next_exit, _) = wait_exit(&mut next, PARENT_GONE_EXIT_BUDGET)
        .expect("second parent-bound tray must also exit on EOF");
    assert_eq!(next_exit.code(), Some(0), "second tray exits cleanly");
}

#[test]
fn without_the_flag_stdin_eof_does_not_quit_a_standalone_tray() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = hermetic_paths(tmp.path());
    let (mut tray, stdin) = spawn_tray(&paths, Lifecycle::Standalone);
    await_lock_held(&paths, &mut tray);
    drop(stdin);
    thread::sleep(Duration::from_millis(800));
    assert!(
        tray.child.try_wait().expect("poll arb-tray").is_none(),
        "standalone arb-tray (no --exit-on-stdin-eof) must ignore stdin EOF"
    );
}

#[test]
fn concurrent_parent_bound_trays_have_one_owner_and_can_reacquire_after_owner_death() {
    const WORKER_ROOT: &str = "ARB_TEST_TRAY_PARENT_ROOT";
    if let Some(root) = std::env::var_os(WORKER_ROOT) {
        // This process owns the write end of the simulated IDE's parent pipe.
        std::io::stdin()
            .read_exact(&mut [0])
            .expect("release launch barrier");
        let (mut tray, _parent_pipe) =
            spawn_tray(&hermetic_paths(Path::new(&root)), Lifecycle::ParentBound);
        assert!(tray.child.wait().expect("wait for tray").success());
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let paths = hermetic_paths(tmp.path());
    let start_parent = || {
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "concurrent_parent_bound_trays_have_one_owner_and_can_reacquire_after_owner_death",
            ])
            .env(WORKER_ROOT, tmp.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn independent parent process");
        Tray { child }
    };
    let mut first = start_parent();
    let mut second = start_parent();
    for parent in [&mut first, &mut second] {
        parent.child.stdin.take().unwrap().write_all(b"+").unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let owner = loop {
        if let Some(status) = first.child.try_wait().unwrap() {
            assert!(status.success());
            break &mut second;
        }
        if let Some(status) = second.child.try_wait().unwrap() {
            assert!(status.success());
            break &mut first;
        }
        assert!(
            Instant::now() < deadline,
            "second tray did not yield the lock"
        );
        thread::sleep(Duration::from_millis(10));
    };
    await_lock_held(&paths, owner);
    owner.child.kill().unwrap();
    owner.child.wait().unwrap();
    // The winning tray is a separate, still-running child. Its parent just died;
    // EOF must make it release the lock before the next IDE launches.
    let deadline = Instant::now() + PARENT_GONE_EXIT_BUDGET;
    loop {
        match SingleInstanceLock::try_acquire(paths.tray_lock_file()) {
            Ok(_) => break,
            Err(OsError::LockContended { .. }) => {}
            Err(err) => panic!("unexpected lock error: {err}"),
        }
        assert!(Instant::now() < deadline, "orphaned tray retained the lock");
        thread::sleep(Duration::from_millis(10));
    }
    let (mut replacement, parent) = spawn_tray(&paths, Lifecycle::ParentBound);
    await_lock_held(&paths, &mut replacement);
    drop(parent);
    assert!(
        wait_exit(&mut replacement, PARENT_GONE_EXIT_BUDGET)
            .unwrap()
            .0
            .success()
    );
}
