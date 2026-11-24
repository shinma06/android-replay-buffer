#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "tray binary prints --headless-smoke JSON summaries and startup diagnostics to stdio"
)]

//! Lightweight cross-OS menu bar / system tray client (`arb-tray`) for Android Replay Buffer
//! (ARB), wired exclusively to `arbd` over local IPC (`SPEC-01`, `SPEC-03`).
//!
//! `arb-tray` performs zero direct ring/catalog/media file writes: all capture/save/bundle
//! mutations are JSON-RPC 2.0 calls dispatched to `arbd` by [`controller::TrayController`].

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use arb_os::paths::ArbPaths;
use arb_tray::controller::{self, TrayController, build_menu};
#[cfg(feature = "native-tray")]
use arb_tray::native_tray;
use arb_tray::rpc::AutoStartMode;
use serde_json::json;

#[derive(Debug, Clone)]
struct CliOptions {
    data_dir: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
    runtime_dir: Option<PathBuf>,
    auto_start: AutoStartMode,
    headless_smoke: bool,
    poll_interval: Duration,
    help: bool,
}

impl Default for CliOptions {
    fn default() -> Self {
        Self {
            data_dir: None,
            cache_dir: None,
            runtime_dir: None,
            auto_start: AutoStartMode::Disabled,
            headless_smoke: false,
            poll_interval: controller::DEFAULT_POLL_INTERVAL,
            help: false,
        }
    }
}

impl CliOptions {
    fn resolve_paths(&self) -> Result<ArbPaths, String> {
        let mut paths = ArbPaths::resolve().map_err(|e| e.to_string())?;
        if let Some(d) = &self.data_dir {
            paths.data_dir.clone_from(d);
        }
        if let Some(c) = &self.cache_dir {
            paths.cache_dir.clone_from(c);
        }
        if let Some(r) = &self.runtime_dir {
            paths.runtime_dir.clone_from(r);
        }
        Ok(paths)
    }
}

fn parse_args(args: &[String]) -> Result<CliOptions, String> {
    let mut opts = CliOptions::default();
    let mut i = 0_usize;
    while i < args.len() {
        match args[i].as_str() {
            "--data-dir" => {
                let val = args
                    .get(i + 1)
                    .ok_or_else(|| "--data-dir requires a path".to_owned())?;
                opts.data_dir = Some(PathBuf::from(val));
                i += 2;
            }
            "--cache-dir" => {
                let val = args
                    .get(i + 1)
                    .ok_or_else(|| "--cache-dir requires a path".to_owned())?;
                opts.cache_dir = Some(PathBuf::from(val));
                i += 2;
            }
            "--runtime-dir" => {
                let val = args
                    .get(i + 1)
                    .ok_or_else(|| "--runtime-dir requires a path".to_owned())?;
                opts.runtime_dir = Some(PathBuf::from(val));
                i += 2;
            }
            "--auto-start" => {
                opts.auto_start = AutoStartMode::Enabled;
                i += 1;
            }
            "--headless-smoke" => {
                opts.headless_smoke = true;
                i += 1;
            }
            "--poll-interval-ms" => {
                let val = args
                    .get(i + 1)
                    .ok_or_else(|| "--poll-interval-ms requires a number".to_owned())?;
                let ms: u64 = val
                    .parse()
                    .map_err(|_| format!("--poll-interval-ms expects an integer, got `{val}`"))?;
                opts.poll_interval = Duration::from_millis(ms.max(1));
                i += 2;
            }
            "-h" | "--help" | "help" => {
                opts.help = true;
                i += 1;
            }
            other => return Err(format!("unknown option `{other}` (run with --help)")),
        }
    }
    Ok(opts)
}

fn print_help() {
    println!(
        "arb-tray {} — Android Replay Buffer menu bar / system tray client\n\n\
OPTIONS:\n  \
  --data-dir <DIR>          Override ARB_DATA_DIR\n  \
  --cache-dir <DIR>         Override ARB_CACHE_DIR\n  \
  --runtime-dir <DIR>       Override ARB_RUNTIME_DIR\n  \
  --auto-start              Auto-spawn sibling arbd if not running\n  \
  --poll-interval-ms <N>    Status poll interval in milliseconds (default 1000)\n  \
  --headless-smoke          Connect, run one status/menu-build cycle, print JSON, exit 0\n  \
  -h, --help                Show this help",
        env!("CARGO_PKG_VERSION")
    );
}

/// Connects once, performs a single status/menu-build cycle, prints a JSON summary, and exits.
///
/// Enables automated CI/Devin smoke testing of `arb-tray` without ever opening a GUI loop.
fn run_headless_smoke(opts: &CliOptions) -> ExitCode {
    let paths = match opts.resolve_paths() {
        Ok(p) => p,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };

    let mut controller = TrayController::new(paths, opts.auto_start, opts.poll_interval);
    let (state, alerts) = controller.poll_once();
    let menu = build_menu(&state);
    let summary = json!({
        "daemon_online": state.daemon_online,
        "badge": state.badge,
        "status_line": state.status_line(),
        "state": state,
        "menu": menu,
        "alerts": alerts,
    });
    match serde_json::to_string_pretty(&summary) {
        Ok(text) => println!("{text}"),
        Err(err) => {
            eprintln!("error: failed to serialize smoke summary: {err}");
            return ExitCode::FAILURE;
        }
    }

    if state.daemon_online {
        ExitCode::SUCCESS
    } else {
        eprintln!("error: could not reach arbd (pass --auto-start, or start arbd first)");
        ExitCode::FAILURE
    }
}

fn main() -> ExitCode {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let opts = match parse_args(&raw_args) {
        Ok(opts) => opts,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };

    if opts.help {
        print_help();
        return ExitCode::SUCCESS;
    }

    if opts.headless_smoke {
        return run_headless_smoke(&opts);
    }

    let paths = match opts.resolve_paths() {
        Ok(p) => p,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };

    run_native_tray(paths, opts.auto_start, opts.poll_interval)
}

#[cfg(feature = "native-tray")]
fn run_native_tray(
    paths: ArbPaths,
    auto_start: AutoStartMode,
    poll_interval: Duration,
) -> ExitCode {
    match native_tray::run(paths, auto_start, poll_interval) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(feature = "native-tray"))]
fn run_native_tray(
    _paths: ArbPaths,
    _auto_start: AutoStartMode,
    _poll_interval: Duration,
) -> ExitCode {
    eprintln!(
        "error: this arb-tray build has no native tray UI (feature `native-tray` disabled); \
         use --headless-smoke"
    );
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_global_flags() {
        let args: Vec<String> = [
            "--data-dir",
            "/tmp/d",
            "--auto-start",
            "--headless-smoke",
            "--poll-interval-ms",
            "250",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let opts = parse_args(&args).expect("args must parse");
        assert_eq!(opts.data_dir, Some(PathBuf::from("/tmp/d")));
        assert_eq!(opts.auto_start, AutoStartMode::Enabled);
        assert!(opts.headless_smoke);
        assert_eq!(opts.poll_interval, Duration::from_millis(250));
    }

    #[test]
    fn rejects_unknown_option() {
        let args: Vec<String> = ["--bogus"].into_iter().map(str::to_owned).collect();
        assert!(parse_args(&args).is_err());
    }

    #[test]
    fn help_flag_is_recognized() {
        let args: Vec<String> = ["--help"].into_iter().map(str::to_owned).collect();
        let opts = parse_args(&args).expect("help must parse");
        assert!(opts.help);
    }
}
