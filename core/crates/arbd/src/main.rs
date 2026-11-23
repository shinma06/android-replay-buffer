#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "daemon binary entrypoint writes operational status and errors to stdio"
)]

//! Binary entrypoint for the `arbd` single-writer local daemon.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

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
            "-h" | "--help" => {
                println!(
                    "arbd {}\n\nUSAGE:\n  arbd [--data-dir <DIR>] [--cache-dir <DIR>] [--runtime-dir <DIR>]",
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

    let server = DaemonServer::start(DaemonConfig::new(paths)).map_err(|e| e.to_string())?;
    println!(
        "arbd ready pid={} endpoint={}",
        std::process::id(),
        server.endpoint().display()
    );
    server.wait();
    Ok(())
}
