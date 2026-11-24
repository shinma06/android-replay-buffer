#![forbid(unsafe_code)]
#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "daemon binary entrypoint writes operational status and errors to stdio"
)]

//! Binary entrypoint for the `arbd` single-writer local daemon.

use arb_os::signal;
use std::io::Write as _;

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use arb_os::parent_watch::spawn_stdin_parent_watch;
use arb_os::paths::ArbPaths;
use arbd::{DaemonConfig, DaemonServer};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("arbd error: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut data_dir: Option<PathBuf> = None;
    let mut cache_dir: Option<PathBuf> = None;
    let mut runtime_dir: Option<PathBuf> = None;
    let mut exit_on_stdin_eof = false;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                let val = args
                    .next()
                    .ok_or_else(|| "--data-dir requires a path value".to_owned())?;
                data_dir = Some(PathBuf::from(val));
            }
            "--cache-dir" => {
                let val = args
                    .next()
                    .ok_or_else(|| "--cache-dir requires a path value".to_owned())?;
                cache_dir = Some(PathBuf::from(val));
            }
            "--runtime-dir" => {
                let val = args
                    .next()
                    .ok_or_else(|| "--runtime-dir requires a path value".to_owned())?;
                runtime_dir = Some(PathBuf::from(val));
            }
            "--exit-on-stdin-eof" => exit_on_stdin_eof = true,
            "-h" | "--help" => {
                println!(
                    "arbd {}\n\nUSAGE:\n  arbd [--data-dir <DIR>] [--cache-dir <DIR>] [--runtime-dir <DIR>] [--exit-on-stdin-eof]\n\n\
OPTIONS:\n  \
  --exit-on-stdin-eof   Parent-bound lifecycle: shut down cleanly (same path as system.shutdown)\n  \
                        when stdin reaches EOF, i.e. when the parent that holds the stdin pipe\n  \
                        exits, crashes or closes it. Off by default.",
                    env!("CARGO_PKG_VERSION")
                );
                return Ok(());
            }
            "--version" => {
                println!("arbd {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            other => {
                return Err(format!("unknown argument `{other}`"));
            }
        }
    }

    let mut paths = ArbPaths::resolve().map_err(|e| e.to_string())?;
    if let Some(d) = data_dir {
        paths.data_dir = d;
    }
    if let Some(c) = cache_dir {
        paths.cache_dir = c;
    }
    if let Some(r) = runtime_dir {
        paths.runtime_dir = r;
    }

    signal::install_shutdown_handlers();
    let mut config = DaemonConfig::new(paths);
    if exit_on_stdin_eof {
        config = config.bound_to_parent();
    }
    let server = DaemonServer::start(config).map_err(|e| e.to_string())?;
    let shutdown_handle = server.shutdown_handle();
    let signal_watcher = thread::Builder::new()
        .name("arbd-signal-watch".to_owned())
        .spawn(move || {
            while !shutdown_handle.is_shutting_down() {
                if signal::is_signalled() {
                    shutdown_handle.trigger();
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
        })
        .ok();
    if exit_on_stdin_eof {
        // Parent-bound lifecycle: the parent (Android Studio) holds our stdin pipe; the kernel
        // closes it when the parent dies for any reason, and EOF takes the exact graceful path
        // of `system.shutdown` (accept loop exit, capture stop, socket + lock release, exit 0).
        // The thread is not joined: after a normal shutdown it stays blocked in `read`.
        let parent_handle = server.shutdown_handle();
        spawn_stdin_parent_watch("arbd-parent-watch", move |gone| {
            parent_handle.trigger();
            let _ = writeln!(
                std::io::stderr(),
                "arbd: parent gone ({gone:?}); shutting down"
            );
        })
        .map_err(|e| format!("failed to start the stdin parent watch: {e}"))?;
    }
    println!(
        "arbd ready pid={} endpoint={}",
        std::process::id(),
        server.endpoint().display()
    );
    server.wait();
    if let Some(handle) = signal_watcher {
        let _ = handle.join();
    }
    Ok(())
}
