#![forbid(unsafe_code)]
#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "CLI binary prints command output and diagnostics to stdio"
)]

//! Scriptable CLI client (`arb-cli`) communicating with `arbd` strictly over local IPC + SHM.

mod mcp;
#[cfg(test)]
mod pdd_contract_tables;
use arb_os::rpc;
mod upgrade;

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use arb_core::config::CaptureMode;
use arb_core::device::validate_adb_serial;
use arb_os::paths::ArbPaths;
use arb_os::shm::ShmFrameRingReader;
use serde_json::{Value, json};

use crate::rpc::{
    AutoStartMode, RpcClient, RpcClientError, auto_start_timeout, spawn_daemon_process,
    wait_for_daemon,
};

// Exit codes documented in `--help` (R3 S17).
const EXIT_GENERAL_ERROR: u8 = 1;
const EXIT_USAGE_ERROR: u8 = 2;
const EXIT_SAVE_FAILED_RETRYABLE: u8 = 3;
const EXIT_NOT_FOUND: u8 = 4;
const EXIT_CONFLICT: u8 = 5;

// JSON-RPC 2.0 error codes returned by `arbd`.
const RPC_ERR_PARSE: i32 = -32_700;
const RPC_ERR_INVALID_REQUEST: i32 = -32_600;
const RPC_ERR_METHOD_NOT_FOUND: i32 = -32_601;
const RPC_ERR_INVALID_PARAMS: i32 = -32_602;
const RPC_ERR_NOT_FOUND: i32 = -32_001;
const RPC_ERR_CONFLICT: i32 = -32_002;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliError {
    exit_code: u8,
    rpc_code: Option<i32>,
    message: String,
    already_emitted: bool,
}

impl CliError {
    fn usage(message: impl Into<String>) -> Self {
        Self {
            exit_code: EXIT_USAGE_ERROR,
            rpc_code: None,
            message: message.into(),
            already_emitted: false,
        }
    }

    fn general(message: impl Into<String>) -> Self {
        Self {
            exit_code: EXIT_GENERAL_ERROR,
            rpc_code: None,
            message: message.into(),
            already_emitted: false,
        }
    }

    fn save_failed_already_emitted() -> Self {
        Self {
            exit_code: EXIT_SAVE_FAILED_RETRYABLE,
            rpc_code: None,
            message: "save failed (retryable); see emitted job status".to_owned(),
            already_emitted: true,
        }
    }

    fn from_rpc(err: RpcClientError) -> Self {
        match err {
            RpcClientError::Rpc { code, message } | RpcClientError::Rejected { code, message } => {
                Self {
                    exit_code: exit_code_for_rpc(code),
                    rpc_code: Some(code),
                    message,
                    already_emitted: false,
                }
            }
            other => Self::general(other.to_string()),
        }
    }
}

const fn exit_code_for_rpc(code: i32) -> u8 {
    match code {
        RPC_ERR_PARSE
        | RPC_ERR_INVALID_REQUEST
        | RPC_ERR_METHOD_NOT_FOUND
        | RPC_ERR_INVALID_PARAMS => EXIT_USAGE_ERROR,
        RPC_ERR_NOT_FOUND => EXIT_NOT_FOUND,
        RPC_ERR_CONFLICT => EXIT_CONFLICT,
        _ => EXIT_GENERAL_ERROR,
    }
}

#[derive(Debug, Clone)]
struct GlobalOptions {
    data_dir: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
    runtime_dir: Option<PathBuf>,
    output: OutputFormat,
    auto_start: AutoStartMode,
    upgrade_if_needed: bool,
}

impl Default for GlobalOptions {
    fn default() -> Self {
        Self {
            data_dir: None,
            cache_dir: None,
            runtime_dir: None,
            output: OutputFormat::Human,
            auto_start: AutoStartMode::Disabled,
            upgrade_if_needed: false,
        }
    }
}

impl GlobalOptions {
    fn resolve_paths(&self) -> Result<ArbPaths, CliError> {
        let mut paths = ArbPaths::resolve().map_err(|e| CliError::general(e.to_string()))?;
        if let Some(d) = &self.data_dir {
            paths.data_dir = absolutize_path(d)?;
        }
        if let Some(c) = &self.cache_dir {
            paths.cache_dir = absolutize_path(c)?;
        }
        if let Some(r) = &self.runtime_dir {
            paths.runtime_dir = absolutize_path(r)?;
        }
        Ok(paths)
    }
}

/// Resolves a relative path against the CLI's current working directory without requiring the
/// target to exist yet (R3 M8).
fn absolutize_path(path: &Path) -> Result<PathBuf, CliError> {
    let absolute = std::path::absolute(path).map_err(|err| {
        CliError::usage(format!(
            "cannot resolve path `{}` against current directory: {err}",
            path.display()
        ))
    })?;
    Ok(lexically_normalize(&absolute))
}

/// Resolves `.` and `..` segments textually on every OS. `std::path::absolute` keeps `..` on
/// POSIX but resolves it on Windows; arbd rejects `..` outright, so the CLI normalizes first to
/// behave identically on Linux, macOS and Windows.
fn lexically_normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                // Never pop past the root / prefix.
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Absolutizes a `bundle-id-or-path` argument when it looks like a filesystem path (contains a
/// path separator, starts with `.`, is already absolute, or exists on disk as a directory),
/// leaving catalog bundle IDs untouched (R3 M8).
fn absolutize_bundle_target(target: &str) -> Result<String, CliError> {
    let path = Path::new(target);
    let is_path_like = target.contains('/')
        || target.contains('\\')
        || target.starts_with('.')
        || path.is_absolute()
        || path.is_dir();
    if !is_path_like {
        return Ok(target.to_owned());
    }
    let abs = absolutize_path(path)?;
    Ok(abs.to_string_lossy().into_owned())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CommandSpec {
    Help,
    Version,
    Status,
    DaemonStart,
    DaemonStop,
    PrepareUpgrade {
        start: AutoStartMode,
    },
    Devices,
    CaptureStart {
        serial: Option<String>,
        capture_mode: Option<CaptureMode>,
        companion_jar: Option<PathBuf>,
        scrcpy_jar: Option<PathBuf>,
        max_size: Option<u32>,
        max_fps: Option<u32>,
        video_bit_rate: Option<u32>,
        audio_enabled: Option<bool>,
        logcat_enabled: bool,
        input_enabled: bool,
        heartbeat_ms: Option<u32>,
    },
    CaptureStop,
    Save {
        seconds: Option<u64>,
        dest: Option<PathBuf>,
        drain_timeout_ms: Option<u64>,
    },
    SaveLogs {
        window_secs: Option<u64>,
        dest: Option<PathBuf>,
        drain_timeout_ms: Option<u64>,
    },
    Screenshot {
        serial: Option<String>,
        out: Option<PathBuf>,
        out_dir: Option<PathBuf>,
        overwrite: bool,
    },
    ConfigGet,
    ConfigSet {
        patch: Value,
    },
    JobsList,
    JobsRetry {
        job_id: String,
    },
    JobsRetarget {
        job_id: String,
        dest: PathBuf,
    },
    JobsDiscard {
        job_id: String,
    },
    BundlesList {
        state: Option<String>,
    },
    BundlesOpen {
        target: String,
        holder_pid: Option<u32>,
    },
    BundlesClose {
        lease_id: String,
    },
    BundlesTrash {
        bundle_id: String,
    },
    BundlesRestore {
        bundle_id: String,
    },
    BundlesGc {
        ignore_grace: bool,
    },
    BundlesForget {
        bundle_id: Option<String>,
        all_missing: bool,
    },
    Inspect {
        target: String,
    },
    Trim {
        target: String,
        in_frame: u64,
        out_frame: u64,
        dest: Option<PathBuf>,
        burn_taps: bool,
        include_share_mp4: bool,
        trash_original: bool,
    },
    ShareMp4 {
        target: String,
        in_frame: Option<u64>,
        out_frame: Option<u64>,
        out: Option<PathBuf>,
        dest: Option<PathBuf>,
        burn_taps: bool,
        trash_original: bool,
    },
    Png {
        target: String,
        frame: u64,
        out: PathBuf,
    },
    ScrubBench {
        target: String,
        frame: Option<u64>,
    },
    Mcp,
}

fn format_error(output: OutputFormat, err: &CliError) -> String {
    match output {
        OutputFormat::Json => serde_json::to_string(&json!({
            "error": {
                "code": err.rpc_code,
                "exit_code": err.exit_code,
                "message": err.message,
            }
        }))
        .unwrap_or_else(|_| format!("{{\"error\":{{\"message\":\"{}\"}}}}", err.message)),
        OutputFormat::Human => format!("error: {}", err.message),
    }
}

fn main() -> ExitCode {
    let raw_args: Vec<String> = env::args().skip(1).collect();
    let json_requested = raw_args.iter().any(|arg| arg == "--json");
    let output_hint = if json_requested {
        OutputFormat::Json
    } else {
        OutputFormat::Human
    };

    match parse_args(&raw_args).and_then(|(opts, cmd)| execute(&opts, cmd)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            if !err.already_emitted {
                eprintln!("{}", format_error(output_hint, &err));
            }
            ExitCode::from(err.exit_code)
        }
    }
}

fn parse_args(args: &[String]) -> Result<(GlobalOptions, CommandSpec), CliError> {
    let mut opts = GlobalOptions::default();
    let mut rest: Vec<String> = Vec::with_capacity(args.len());
    let mut i = 0_usize;

    while i < args.len() {
        match args[i].as_str() {
            "--data-dir" => {
                let val = args
                    .get(i + 1)
                    .ok_or_else(|| CliError::usage("--data-dir requires a path"))?;
                opts.data_dir = Some(absolutize_path(Path::new(val))?);
                i += 2;
            }
            "--cache-dir" => {
                let val = args
                    .get(i + 1)
                    .ok_or_else(|| CliError::usage("--cache-dir requires a path"))?;
                opts.cache_dir = Some(absolutize_path(Path::new(val))?);
                i += 2;
            }
            "--runtime-dir" => {
                let val = args
                    .get(i + 1)
                    .ok_or_else(|| CliError::usage("--runtime-dir requires a path"))?;
                opts.runtime_dir = Some(absolutize_path(Path::new(val))?);
                i += 2;
            }
            "--json" => {
                opts.output = OutputFormat::Json;
                i += 1;
            }
            "--upgrade-if-needed" => {
                opts.upgrade_if_needed = true;
                i += 1;
            }
            "--auto-start" => {
                opts.auto_start = AutoStartMode::Enabled;
                i += 1;
            }
            _ => {
                rest.push(args[i].clone());
                i += 1;
            }
        }
    }

    if rest.is_empty() || matches!(rest[0].as_str(), "-h" | "--help" | "help") {
        return Ok((opts, CommandSpec::Help));
    }
    if matches!(rest[0].as_str(), "-V" | "--version") {
        return Ok((opts, CommandSpec::Version));
    }

    let cmd = match rest[0].as_str() {
        "status" => CommandSpec::Status,
        "devices" => CommandSpec::Devices,
        "daemon" => parse_daemon_cmd(&rest[1..])?,
        "capture" => parse_capture_cmd(&rest[1..])?,
        "capture-start" => parse_capture_start_flags(&rest[1..])?,
        "save" => parse_save_cmd(&rest[1..])?,
        "save-logs" => parse_save_logs_cmd(&rest[1..])?,
        "screenshot" => parse_screenshot_cmd(&rest[1..])?,
        "config" => parse_config_cmd(&rest[1..])?,
        "jobs" => parse_jobs_cmd(&rest[1..])?,
        "bundles" => parse_bundles_cmd(&rest[1..])?,
        "inspect" => {
            let raw = rest
                .get(1)
                .ok_or_else(|| CliError::usage("inspect requires <bundle-id-or-path>"))?;
            CommandSpec::Inspect {
                target: absolutize_bundle_target(raw)?,
            }
        }
        "trim" => parse_trim_cmd(&rest[1..])?,
        "share-mp4" => parse_share_mp4_cmd(&rest[1..])?,
        "png" => parse_png_cmd(&rest[1..])?,
        "scrub-bench" => parse_scrub_bench_cmd(&rest[1..])?,
        "mcp" => {
            if rest.len() > 1 {
                return Err(CliError::usage(format!(
                    "unknown mcp argument `{}` (usage: arb-cli mcp)",
                    rest[1]
                )));
            }
            CommandSpec::Mcp
        }
        other => {
            return Err(CliError::usage(format!(
                "unknown subcommand `{other}` (run with --help)"
            )));
        }
    };

    Ok((opts, cmd))
}

fn parse_daemon_cmd(args: &[String]) -> Result<CommandSpec, CliError> {
    match args.first().map(String::as_str) {
        Some("start") => Ok(CommandSpec::DaemonStart),
        Some("prepare-upgrade") if args.len() == 1 || args[1..] == ["--start"] => {
            Ok(CommandSpec::PrepareUpgrade {
                start: if args.len() == 2 {
                    AutoStartMode::Enabled
                } else {
                    AutoStartMode::Disabled
                },
            })
        }
        Some("stop") => Ok(CommandSpec::DaemonStop),
        other => Err(CliError::usage(format!(
            "usage: arb-cli daemon <start|stop> (got {other:?})"
        ))),
    }
}

fn parse_capture_mode(raw: &str, flag: &str) -> Result<CaptureMode, CliError> {
    match raw {
        "video_and_logs" => Ok(CaptureMode::VideoAndLogs),
        "logs_only" => Ok(CaptureMode::LogsOnly),
        _ => Err(CliError::usage(format!(
            "{flag} expects `video_and_logs` or `logs_only`, got `{raw}`"
        ))),
    }
}

fn parse_capture_cmd(args: &[String]) -> Result<CommandSpec, CliError> {
    match args.first().map(String::as_str) {
        Some("stop") => Ok(CommandSpec::CaptureStop),
        Some("start") => parse_capture_start_flags(&args[1..]),
        other => Err(CliError::usage(format!(
            "usage: arb-cli capture <start|stop> (got {other:?})"
        ))),
    }
}

fn parse_capture_start_flags(args: &[String]) -> Result<CommandSpec, CliError> {
    let mut serial = None;
    let mut capture_mode = None;
    let mut companion_jar = None;
    let mut scrcpy_jar = None;
    let mut max_size = None;
    let mut max_fps = None;
    let mut video_bit_rate = None;
    // `None` = defer to the persisted `config.audio_enabled` (GUI/CLI/MCP parity).
    let mut audio_enabled: Option<bool> = None;
    let mut logcat_enabled = true;
    let mut input_enabled = true;
    let mut heartbeat_ms = None;

    let mut i = 0_usize;
    while i < args.len() {
        match args[i].as_str() {
            "--serial" => {
                let raw = require_val(args, i, "--serial")?;
                validate_adb_serial(raw).map_err(|err| CliError::usage(err.to_string()))?;
                serial = Some(raw.to_owned());
                i += 2;
            }
            "--mode" | "--capture-mode" => {
                let flag = args[i].as_str();
                let raw = require_val(args, i, flag)?;
                capture_mode = Some(parse_capture_mode(raw, flag)?);
                i += 2;
            }
            "--companion-jar" => {
                companion_jar = Some(absolutize_path(Path::new(require_val(
                    args,
                    i,
                    "--companion-jar",
                )?))?);
                i += 2;
            }
            "--scrcpy-jar" => {
                scrcpy_jar = Some(absolutize_path(Path::new(require_val(
                    args,
                    i,
                    "--scrcpy-jar",
                )?))?);
                i += 2;
            }
            "--max-size" => {
                max_size = Some(parse_u32(
                    require_val(args, i, "--max-size")?,
                    "--max-size",
                )?);
                i += 2;
            }
            "--max-fps" => {
                max_fps = Some(parse_u32(require_val(args, i, "--max-fps")?, "--max-fps")?);
                i += 2;
            }
            "--video-bit-rate" => {
                video_bit_rate = Some(parse_u32(
                    require_val(args, i, "--video-bit-rate")?,
                    "--video-bit-rate",
                )?);
                i += 2;
            }
            "--heartbeat-ms" => {
                heartbeat_ms = Some(parse_u32(
                    require_val(args, i, "--heartbeat-ms")?,
                    "--heartbeat-ms",
                )?);
                i += 2;
            }
            "--no-audio" => {
                audio_enabled = Some(false);
                i += 1;
            }
            "--no-logcat" => {
                logcat_enabled = false;
                i += 1;
            }
            "--no-input" => {
                input_enabled = false;
                i += 1;
            }
            other => {
                return Err(CliError::usage(format!(
                    "unknown capture start option `{other}`"
                )));
            }
        }
    }

    Ok(CommandSpec::CaptureStart {
        serial,
        capture_mode,
        companion_jar,
        scrcpy_jar,
        max_size,
        max_fps,
        video_bit_rate,
        audio_enabled,
        logcat_enabled,
        input_enabled,
        heartbeat_ms,
    })
}

fn parse_save_cmd(args: &[String]) -> Result<CommandSpec, CliError> {
    // `None` = defer to the persisted `config.save_window_secs` (GUI/CLI/MCP parity).
    let mut seconds: Option<u64> = None;
    let mut dest = None;
    let mut drain_timeout_ms = None;
    let mut i = 0_usize;

    while i < args.len() {
        match args[i].as_str() {
            "--seconds" => {
                seconds = Some(parse_u64(require_val(args, i, "--seconds")?, "--seconds")?);
                i += 2;
            }
            "--dest" => {
                dest = Some(absolutize_path(Path::new(require_val(args, i, "--dest")?))?);
                i += 2;
            }
            "--drain-timeout-ms" => {
                drain_timeout_ms = Some(parse_u64(
                    require_val(args, i, "--drain-timeout-ms")?,
                    "--drain-timeout-ms",
                )?);
                i += 2;
            }
            other => return Err(CliError::usage(format!("unknown save option `{other}`"))),
        }
    }

    Ok(CommandSpec::Save {
        seconds,
        dest,
        drain_timeout_ms,
    })
}

fn parse_save_logs_cmd(args: &[String]) -> Result<CommandSpec, CliError> {
    let mut window_secs = None;
    let mut dest = None;
    let mut drain_timeout_ms = None;
    let mut i = 0_usize;

    while i < args.len() {
        match args[i].as_str() {
            "--seconds" | "--window-secs" => {
                let flag = args[i].as_str();
                window_secs = Some(parse_u64(require_val(args, i, flag)?, flag)?);
                i += 2;
            }
            "--dest" => {
                dest = Some(absolutize_path(Path::new(require_val(args, i, "--dest")?))?);
                i += 2;
            }
            "--drain-timeout-ms" => {
                drain_timeout_ms = Some(parse_u64(
                    require_val(args, i, "--drain-timeout-ms")?,
                    "--drain-timeout-ms",
                )?);
                i += 2;
            }
            other => {
                return Err(CliError::usage(format!(
                    "unknown save-logs option `{other}`"
                )));
            }
        }
    }

    Ok(CommandSpec::SaveLogs {
        window_secs,
        dest,
        drain_timeout_ms,
    })
}

fn parse_screenshot_cmd(args: &[String]) -> Result<CommandSpec, CliError> {
    let mut serial = None;
    let mut out = None;
    let mut out_dir = None;
    let mut overwrite = false;
    let mut i = 0_usize;

    while i < args.len() {
        match args[i].as_str() {
            "--serial" => {
                let raw = require_val(args, i, "--serial")?;
                validate_adb_serial(raw).map_err(|err| CliError::usage(err.to_string()))?;
                serial = Some(raw.to_owned());
                i += 2;
            }
            "--out" => {
                out = Some(absolutize_path(Path::new(require_val(args, i, "--out")?))?);
                i += 2;
            }
            "--out-dir" => {
                out_dir = Some(absolutize_path(Path::new(require_val(
                    args,
                    i,
                    "--out-dir",
                )?))?);
                i += 2;
            }
            "--overwrite" => {
                overwrite = true;
                i += 1;
            }
            other => {
                return Err(CliError::usage(format!(
                    "unknown screenshot option `{other}`"
                )));
            }
        }
    }

    if out.is_some() && out_dir.is_some() {
        return Err(CliError::usage(
            "screenshot accepts either --out <FILE.png> or --out-dir <DIR>, not both",
        ));
    }

    Ok(CommandSpec::Screenshot {
        serial,
        out,
        out_dir,
        overwrite,
    })
}

fn parse_config_cmd(args: &[String]) -> Result<CommandSpec, CliError> {
    match args.first().map(String::as_str) {
        Some("get") => Ok(CommandSpec::ConfigGet),
        Some("set") => parse_config_set_flags(&args[1..]),
        other => Err(CliError::usage(format!(
            "usage: arb-cli config <get|set ...> (got {other:?})"
        ))),
    }
}

fn parse_optional_dir_value(raw: &str) -> Result<Value, CliError> {
    if raw.is_empty() || raw.eq_ignore_ascii_case("none") || raw == "null" {
        Ok(Value::Null)
    } else {
        Ok(json!(absolutize_path(Path::new(raw))?))
    }
}

fn parse_config_set_flags(args: &[String]) -> Result<CommandSpec, CliError> {
    let mut patch = serde_json::Map::new();
    let mut i = 0_usize;
    while i < args.len() {
        let flag = args[i].as_str();
        let val = require_val(args, i, flag)?;
        match flag {
            "--save-window-secs" => {
                patch.insert("save_window_secs".to_owned(), json!(parse_u32(val, flag)?));
            }
            "--ring-budget-bytes" => {
                patch.insert("ring_budget_bytes".to_owned(), json!(parse_u64(val, flag)?));
            }
            "--min-free-disk-bytes" => {
                patch.insert(
                    "min_free_disk_bytes".to_owned(),
                    json!(parse_u64(val, flag)?),
                );
            }
            "--video-max-size" => {
                patch.insert("video_max_size".to_owned(), json!(parse_u32(val, flag)?));
            }
            "--video-max-fps" => {
                patch.insert("video_max_fps".to_owned(), json!(parse_u32(val, flag)?));
            }
            "--audio-capture-mode" => {
                let mode: arb_core::config::AudioCaptureMode = serde_json::from_value(json!(val))
                    .map_err(|_| {
                    CliError::usage("--audio-capture-mode expects playback_dup or output_compat")
                })?;
                patch.insert("audio_capture_mode".to_owned(), json!(mode));
            }
            "--audio-enabled" => {
                patch.insert("audio_enabled".to_owned(), json!(parse_bool(val, flag)?));
            }
            "--crash-autosave-enabled" => {
                patch.insert(
                    "crash_autosave_enabled".to_owned(),
                    json!(parse_bool(val, flag)?),
                );
            }
            "--crash-autosave-cooldown-secs" => {
                patch.insert(
                    "crash_autosave_cooldown_secs".to_owned(),
                    json!(parse_u32(val, flag)?),
                );
            }
            "--output-dir-override" => {
                patch.insert(
                    "output_dir_override".to_owned(),
                    parse_optional_dir_value(val)?,
                );
            }
            "--screenshot-dir-override" => {
                patch.insert(
                    "screenshot_dir_override".to_owned(),
                    parse_optional_dir_value(val)?,
                );
            }
            "--capture-mode" => {
                patch.insert(
                    "capture_mode".to_owned(),
                    json!(parse_capture_mode(val, flag)?),
                );
            }
            "--device-serial" => {
                if val.is_empty() || val.eq_ignore_ascii_case("none") || val == "null" {
                    patch.insert("device_serial".to_owned(), Value::Null);
                } else {
                    validate_adb_serial(val).map_err(|err| CliError::usage(err.to_string()))?;
                    patch.insert("device_serial".to_owned(), json!(val));
                }
            }
            "--ide-default-save-action" => {
                patch.insert("ide_default_save_action".to_owned(), json!(val));
            }
            other => {
                return Err(CliError::usage(format!(
                    "unknown config set option `{other}`"
                )));
            }
        }
        i += 2;
    }
    if patch.is_empty() {
        return Err(CliError::usage(
            "config set requires at least one option (run with --help)",
        ));
    }
    Ok(CommandSpec::ConfigSet {
        patch: Value::Object(patch),
    })
}

fn parse_bool(raw: &str, flag: &str) -> Result<bool, CliError> {
    match raw {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(CliError::usage(format!(
            "{flag} expects true or false, got `{raw}`"
        ))),
    }
}

fn parse_jobs_cmd(args: &[String]) -> Result<CommandSpec, CliError> {
    match args.first().map(String::as_str) {
        Some("list") => Ok(CommandSpec::JobsList),
        Some("retry") => {
            let job_id = args
                .get(1)
                .ok_or_else(|| CliError::usage("jobs retry requires <job-id>"))?
                .clone();
            Ok(CommandSpec::JobsRetry { job_id })
        }
        Some("discard") => {
            let job_id = args
                .get(1)
                .ok_or_else(|| CliError::usage("jobs discard requires <job-id>"))?
                .clone();
            Ok(CommandSpec::JobsDiscard { job_id })
        }
        Some("retarget") => {
            let job_id = args
                .get(1)
                .ok_or_else(|| CliError::usage("jobs retarget requires <job-id> --dest <dir>"))?
                .clone();
            let mut dest = None;
            let mut i = 2_usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--dest" => {
                        dest = Some(absolutize_path(Path::new(require_val(args, i, "--dest")?))?);
                        i += 2;
                    }
                    other => {
                        return Err(CliError::usage(format!(
                            "unknown jobs retarget option `{other}`"
                        )));
                    }
                }
            }
            let dest =
                dest.ok_or_else(|| CliError::usage("jobs retarget requires --dest <dir>"))?;
            Ok(CommandSpec::JobsRetarget { job_id, dest })
        }
        other => Err(CliError::usage(format!(
            "usage: arb-cli jobs <list|retry|retarget|discard> (got {other:?})"
        ))),
    }
}

fn parse_bundles_cmd(args: &[String]) -> Result<CommandSpec, CliError> {
    match args.first().map(String::as_str) {
        Some("list") => {
            let mut state = None;
            let mut i = 1_usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--state" => {
                        state = Some(require_val(args, i, "--state")?.to_owned());
                        i += 2;
                    }
                    other => {
                        return Err(CliError::usage(format!(
                            "unknown bundles list option `{other}`"
                        )));
                    }
                }
            }
            Ok(CommandSpec::BundlesList { state })
        }
        Some("open") => {
            let raw = args
                .get(1)
                .ok_or_else(|| CliError::usage("bundles open requires <bundle-id-or-path>"))?;
            let target = absolutize_bundle_target(raw)?;
            let mut holder_pid = None;
            let mut i = 2_usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--holder-pid" => {
                        holder_pid = Some(parse_u32(
                            require_val(args, i, "--holder-pid")?,
                            "--holder-pid",
                        )?);
                        i += 2;
                    }
                    other => {
                        return Err(CliError::usage(format!(
                            "unknown bundles open option `{other}`"
                        )));
                    }
                }
            }
            Ok(CommandSpec::BundlesOpen { target, holder_pid })
        }
        Some("close") => {
            let lease_id = args
                .get(1)
                .ok_or_else(|| CliError::usage("bundles close requires <lease-id>"))?
                .clone();
            Ok(CommandSpec::BundlesClose { lease_id })
        }
        Some("trash") => {
            let bundle_id = args
                .get(1)
                .ok_or_else(|| CliError::usage("bundles trash requires <bundle-id>"))?
                .clone();
            Ok(CommandSpec::BundlesTrash { bundle_id })
        }
        Some("restore") => {
            let bundle_id = args
                .get(1)
                .ok_or_else(|| CliError::usage("bundles restore requires <bundle-id>"))?
                .clone();
            Ok(CommandSpec::BundlesRestore { bundle_id })
        }
        Some("gc") => {
            let mut ignore_grace = false;
            for flag in args.iter().skip(1) {
                match flag.as_str() {
                    "--now" => ignore_grace = true,
                    other => {
                        return Err(CliError::usage(format!(
                            "unknown bundles gc option `{other}` (expected optional `--now`)"
                        )));
                    }
                }
            }
            Ok(CommandSpec::BundlesGc { ignore_grace })
        }
        Some("forget") => match args.get(1).map(String::as_str) {
            Some("--all-missing") if args.len() == 2 => Ok(CommandSpec::BundlesForget {
                bundle_id: None,
                all_missing: true,
            }),
            Some(id) if !id.starts_with("--") && args.len() == 2 => {
                Ok(CommandSpec::BundlesForget {
                    bundle_id: Some(id.to_owned()),
                    all_missing: false,
                })
            }
            _ => Err(CliError::usage(
                "usage: arb-cli bundles forget (<bundle-id> | --all-missing)",
            )),
        },
        other => Err(CliError::usage(format!(
            "usage: arb-cli bundles <list|open|close|trash|restore|gc|forget> (got {other:?})"
        ))),
    }
}

fn parse_trim_cmd(args: &[String]) -> Result<CommandSpec, CliError> {
    let raw = args.first().ok_or_else(|| {
        CliError::usage(
            "usage: arb-cli trim <bundle-id-or-path> --in-frame <n> --out-frame <m> [--dest <dir>] [--include-share-mp4] [--burn-taps] [--trash-original]",
        )
    })?;
    let target = absolutize_bundle_target(raw)?;
    let mut in_frame = None;
    let mut out_frame = None;
    let mut dest = None;
    let mut burn_taps = false;
    let mut include_share_mp4 = false;
    let mut trash_original = false;
    let mut i = 1_usize;

    while i < args.len() {
        match args[i].as_str() {
            "--in-frame" => {
                in_frame = Some(parse_u64(
                    require_val(args, i, "--in-frame")?,
                    "--in-frame",
                )?);
                i += 2;
            }
            "--out-frame" => {
                out_frame = Some(parse_u64(
                    require_val(args, i, "--out-frame")?,
                    "--out-frame",
                )?);
                i += 2;
            }
            "--dest" => {
                dest = Some(absolutize_path(Path::new(require_val(args, i, "--dest")?))?);
                i += 2;
            }
            "--burn-taps" => {
                burn_taps = true;
                include_share_mp4 = true;
                i += 1;
            }
            "--include-share-mp4" => {
                include_share_mp4 = true;
                i += 1;
            }
            "--trash-original" => {
                trash_original = true;
                i += 1;
            }
            other => return Err(CliError::usage(format!("unknown trim option `{other}`"))),
        }
    }

    let in_frame = in_frame.ok_or_else(|| CliError::usage("trim requires --in-frame <n>"))?;
    let out_frame = out_frame.ok_or_else(|| CliError::usage("trim requires --out-frame <m>"))?;
    Ok(CommandSpec::Trim {
        target,
        in_frame,
        out_frame,
        dest,
        burn_taps,
        include_share_mp4,
        trash_original,
    })
}

fn parse_share_mp4_cmd(args: &[String]) -> Result<CommandSpec, CliError> {
    let raw = args.first().ok_or_else(|| {
        CliError::usage(
            "usage: arb-cli share-mp4 <bundle-id-or-path> [--in-frame <n> --out-frame <m>] [--out <file.mp4>] [--dest <dir>] [--burn-taps] [--trash-original]",
        )
    })?;
    let target = absolutize_bundle_target(raw)?;
    let mut in_frame = None;
    let mut out_frame = None;
    let mut out = None;
    let mut dest = None;
    let mut burn_taps = false;
    let mut trash_original = false;
    let mut i = 1_usize;

    while i < args.len() {
        match args[i].as_str() {
            "--in-frame" => {
                in_frame = Some(parse_u64(
                    require_val(args, i, "--in-frame")?,
                    "--in-frame",
                )?);
                i += 2;
            }
            "--out-frame" => {
                out_frame = Some(parse_u64(
                    require_val(args, i, "--out-frame")?,
                    "--out-frame",
                )?);
                i += 2;
            }
            "--out" => {
                out = Some(absolutize_path(Path::new(require_val(args, i, "--out")?))?);
                i += 2;
            }
            "--dest" => {
                dest = Some(absolutize_path(Path::new(require_val(args, i, "--dest")?))?);
                i += 2;
            }
            "--burn-taps" => {
                burn_taps = true;
                i += 1;
            }
            "--trash-original" => {
                trash_original = true;
                i += 1;
            }
            other => {
                return Err(CliError::usage(format!(
                    "unknown share-mp4 option `{other}`"
                )));
            }
        }
    }

    if in_frame.is_some() != out_frame.is_some() {
        return Err(CliError::usage(
            "share-mp4 requires both --in-frame and --out-frame when trimming",
        ));
    }

    Ok(CommandSpec::ShareMp4 {
        target,
        in_frame,
        out_frame,
        out,
        dest,
        burn_taps,
        trash_original,
    })
}

fn parse_png_cmd(args: &[String]) -> Result<CommandSpec, CliError> {
    let raw = args.first().ok_or_else(|| {
        CliError::usage("usage: arb-cli png <bundle-id-or-path> --frame <n> --out <file.png>")
    })?;
    let target = absolutize_bundle_target(raw)?;
    let mut frame = None;
    let mut out = None;
    let mut i = 1_usize;

    while i < args.len() {
        match args[i].as_str() {
            "--frame" => {
                frame = Some(parse_u64(require_val(args, i, "--frame")?, "--frame")?);
                i += 2;
            }
            "--out" => {
                out = Some(absolutize_path(Path::new(require_val(args, i, "--out")?))?);
                i += 2;
            }
            other => return Err(CliError::usage(format!("unknown png option `{other}`"))),
        }
    }

    let frame = frame.ok_or_else(|| CliError::usage("png requires --frame <n>"))?;
    let out = out.ok_or_else(|| CliError::usage("png requires --out <file.png>"))?;
    Ok(CommandSpec::Png { target, frame, out })
}

fn parse_scrub_bench_cmd(args: &[String]) -> Result<CommandSpec, CliError> {
    let raw = args.first().ok_or_else(|| {
        CliError::usage("usage: arb-cli scrub-bench <bundle-id-or-path> [--frame <n>]")
    })?;
    let target = absolutize_bundle_target(raw)?;
    let mut frame = None;
    let mut i = 1_usize;
    while i < args.len() {
        match args[i].as_str() {
            "--frame" => {
                frame = Some(parse_u64(require_val(args, i, "--frame")?, "--frame")?);
                i += 2;
            }
            other => {
                return Err(CliError::usage(format!(
                    "unknown scrub-bench option `{other}`"
                )));
            }
        }
    }
    Ok(CommandSpec::ScrubBench { target, frame })
}

fn require_val<'a>(args: &'a [String], idx: usize, flag: &str) -> Result<&'a str, CliError> {
    args.get(idx + 1)
        .map(String::as_str)
        .ok_or_else(|| CliError::usage(format!("{flag} requires a value")))
}

fn parse_u32(raw: &str, flag: &str) -> Result<u32, CliError> {
    raw.parse::<u32>().map_err(|_| {
        CliError::usage(format!(
            "{flag} expects an unsigned 32-bit integer, got `{raw}`"
        ))
    })
}

fn parse_u64(raw: &str, flag: &str) -> Result<u64, CliError> {
    raw.parse::<u64>().map_err(|_| {
        CliError::usage(format!(
            "{flag} expects an unsigned 64-bit integer, got `{raw}`"
        ))
    })
}

fn execute(opts: &GlobalOptions, cmd: CommandSpec) -> Result<(), CliError> {
    if matches!(cmd, CommandSpec::Help) {
        print_help();
        return Ok(());
    }
    if matches!(cmd, CommandSpec::Version) {
        println!("arb-cli {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let paths = opts.resolve_paths()?;

    if let CommandSpec::PrepareUpgrade { start } = cmd {
        return upgrade::prepare(&paths, start);
    }
    if opts.upgrade_if_needed && !matches!(cmd, CommandSpec::DaemonStop) {
        upgrade::prepare(&paths, AutoStartMode::Enabled)?;
    }
    if matches!(cmd, CommandSpec::Mcp) {
        return mcp::run_mcp_stdio(&paths);
    }

    if matches!(cmd, CommandSpec::DaemonStart) {
        let endpoint = paths.ipc_endpoint();
        let mut client = if let Ok(c) = RpcClient::connect_endpoint(&endpoint) {
            c
        } else {
            let (mut child, log_path) = spawn_daemon_process(&paths).map_err(CliError::from_rpc)?;
            wait_for_daemon(
                &endpoint,
                auto_start_timeout(),
                Some(&mut child),
                Some(&log_path),
            )
            .map_err(CliError::from_rpc)?
        };
        let hello = client.verify_hello().map_err(CliError::from_rpc)?;
        emit_value(opts.output, &hello, "arbd started");
        return Ok(());
    }

    let mut client = RpcClient::connect(&paths, opts.auto_start).map_err(CliError::from_rpc)?;
    dispatch_connected(&mut client, opts.output, cmd)
}

fn dispatch_connected(
    client: &mut RpcClient,
    output: OutputFormat,
    cmd: CommandSpec,
) -> Result<(), CliError> {
    match cmd {
        CommandSpec::Help | CommandSpec::Version | CommandSpec::DaemonStart | CommandSpec::Mcp => {
            Ok(())
        }
        CommandSpec::Status => dispatch_status(client, output),
        CommandSpec::Inspect { target } => execute_inspect(client, output, &target),
        CommandSpec::ScrubBench { target, frame } => {
            execute_scrub_bench(client, output, &target, frame)
        }
        other => {
            let check = if matches!(
                other,
                CommandSpec::Save { .. }
                    | CommandSpec::SaveLogs { .. }
                    | CommandSpec::JobsRetry { .. }
                    | CommandSpec::JobsRetarget { .. }
            ) {
                OutcomeCheck::SaveJob
            } else {
                OutcomeCheck::Standard
            };
            let (method, params, label) = command_to_rpc(other);
            call_and_emit(client, output, method, params, label, check)
        }
    }
}

fn dispatch_status(client: &mut RpcClient, output: OutputFormat) -> Result<(), CliError> {
    let hello = client.verify_hello().map_err(CliError::from_rpc)?;
    let status = client
        .call("system.status", json!({}))
        .map_err(CliError::from_rpc)?;
    emit_value(
        output,
        &json!({ "hello": hello, "status": status }),
        "arbd status",
    );
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutcomeCheck {
    Standard,
    SaveJob,
}

#[expect(
    clippy::too_many_lines,
    reason = "centralized subcommand-to-JSON-RPC dispatch table"
)]
fn command_to_rpc(cmd: CommandSpec) -> (&'static str, Value, &'static str) {
    match cmd {
        CommandSpec::DaemonStop => ("system.shutdown", json!({}), "arbd shutdown requested"),
        CommandSpec::Devices => ("device.list", json!({}), "devices"),
        CommandSpec::CaptureStart { .. } => capture_start_to_rpc(cmd),
        CommandSpec::CaptureStop => ("capture.stop", json!({}), "capture stopped"),
        CommandSpec::Save {
            seconds,
            dest,
            drain_timeout_ms,
        } => (
            "save.trigger",
            json!({ "duration_sec": seconds, "dest_dir": dest, "drain_timeout_ms": drain_timeout_ms }),
            "save result",
        ),
        CommandSpec::SaveLogs {
            window_secs,
            dest,
            drain_timeout_ms,
        } => (
            "save.logs",
            json!({ "window_secs": window_secs, "dest_dir": dest, "drain_timeout_ms": drain_timeout_ms }),
            "log-only save result",
        ),
        CommandSpec::Screenshot {
            serial,
            out,
            out_dir,
            overwrite,
        } => (
            "save.screenshot",
            json!({ "serial": serial, "output_path": out, "output_dir": out_dir, "overwrite": overwrite }),
            "screenshot saved",
        ),
        CommandSpec::ConfigGet => ("config.get", json!({}), "config"),
        CommandSpec::ConfigSet { patch } => ("config.update", patch, "config updated"),
        CommandSpec::JobsList => ("save.list_jobs", json!({}), "save jobs"),
        CommandSpec::JobsRetry { job_id } => {
            ("save.retry", json!({ "job_id": job_id }), "job retried")
        }
        CommandSpec::JobsRetarget { job_id, dest } => (
            "save.retarget",
            json!({ "job_id": job_id, "dest_dir": dest }),
            "job retargeted",
        ),
        CommandSpec::JobsDiscard { job_id } => {
            ("save.discard", json!({ "job_id": job_id }), "job discarded")
        }
        CommandSpec::BundlesList { state } => ("bundle.list", json!({ "state": state }), "bundles"),
        CommandSpec::BundlesOpen { target, holder_pid } => (
            "bundle.open",
            json!({
                "bundle_id_or_path": target,
                "holder_kind": "cli",
                "holder_pid": holder_pid.unwrap_or_else(std::process::id),
            }),
            "bundle opened",
        ),
        CommandSpec::BundlesClose { lease_id } => (
            "bundle.close",
            json!({ "lease_id": lease_id }),
            "bundle lease closed",
        ),
        CommandSpec::BundlesTrash { bundle_id } => (
            "bundle.trash",
            json!({ "bundle_id": bundle_id }),
            "bundle trashed",
        ),
        CommandSpec::BundlesRestore { bundle_id } => (
            "bundle.restore",
            json!({ "bundle_id": bundle_id }),
            "bundle restored",
        ),
        CommandSpec::BundlesGc { ignore_grace } => {
            let params = if ignore_grace {
                json!({ "ignore_grace": true })
            } else {
                json!({})
            };
            ("bundle.gc", params, "bundle gc")
        }
        CommandSpec::BundlesForget {
            bundle_id,
            all_missing,
        } => {
            let params = if all_missing {
                json!({ "all_missing": true })
            } else {
                json!({ "bundle_id": bundle_id })
            };
            ("bundle.forget", params, "bundle forgotten")
        }
        CommandSpec::Trim {
            target,
            in_frame,
            out_frame,
            dest,
            burn_taps,
            include_share_mp4,
            trash_original,
        } => (
            "export.trim",
            json!({
                "bundle_id_or_path": target,
                "in_frame_index": in_frame,
                "out_frame_index": out_frame,
                "dest_dir": dest,
                "burn_taps": burn_taps,
                "include_share_mp4": include_share_mp4,
                "trash_original": trash_original,
                "holder_pid": std::process::id(),
            }),
            "trim result",
        ),
        CommandSpec::ShareMp4 { .. } => share_mp4_to_rpc(cmd),
        CommandSpec::Png { target, frame, out } => (
            "export.png",
            json!({ "bundle_id_or_path": target, "frame_index": frame, "output_path": out }),
            "png export",
        ),
        CommandSpec::Help
        | CommandSpec::Version
        | CommandSpec::PrepareUpgrade { .. }
        | CommandSpec::DaemonStart
        | CommandSpec::Mcp
        | CommandSpec::Status
        | CommandSpec::Inspect { .. }
        | CommandSpec::ScrubBench { .. } => ("system.status", json!({}), "arbd status"),
    }
}

fn share_mp4_to_rpc(cmd: CommandSpec) -> (&'static str, Value, &'static str) {
    let CommandSpec::ShareMp4 {
        target,
        in_frame,
        out_frame,
        out,
        dest,
        burn_taps,
        trash_original,
    } = cmd
    else {
        return ("system.status", json!({}), "arbd status");
    };
    (
        "export.share_mp4",
        json!({
            "bundle_id": target,
            "in_frame_index": in_frame,
            "out_frame_index": out_frame,
            "output_path": out,
            "dest_dir": dest,
            "burn_taps": burn_taps,
            "trash_original": trash_original,
            "holder_pid": std::process::id(),
        }),
        "share.mp4 export",
    )
}

fn capture_start_to_rpc(cmd: CommandSpec) -> (&'static str, Value, &'static str) {
    let CommandSpec::CaptureStart {
        serial,
        capture_mode,
        companion_jar,
        scrcpy_jar,
        max_size,
        max_fps,
        video_bit_rate,
        audio_enabled,
        logcat_enabled,
        input_enabled,
        heartbeat_ms,
    } = cmd
    else {
        return ("system.status", json!({}), "arbd status");
    };
    (
        "capture.start",
        json!({
            "serial": serial, "capture_mode": capture_mode, "companion_jar": companion_jar,
            "scrcpy_jar": scrcpy_jar, "max_size": max_size, "max_fps": max_fps,
            "video_bit_rate": video_bit_rate, "audio_enabled": audio_enabled,
            "logcat_enabled": logcat_enabled, "input_enabled": input_enabled,
            "heartbeat_ms": heartbeat_ms,
        }),
        "capture started",
    )
}

/// Checks whether a `save.trigger` / `save.logs` / `save.retry` / `save.retarget` response
/// indicates a failed-retryable outcome (`bundle_id == null` or `job.state == "failed_retryable"`,
/// R3 S17; shared by CLI and MCP).
fn save_outcome_failed(resp: &Value) -> bool {
    resp.get("bundle_id").is_none_or(Value::is_null) || resp["job"]["state"] == "failed_retryable"
}

fn call_and_emit(
    client: &mut RpcClient,
    output: OutputFormat,
    method: &str,
    params: Value,
    label: &str,
    check: OutcomeCheck,
) -> Result<(), CliError> {
    let resp = client.call(method, params).map_err(CliError::from_rpc)?;
    emit_value(output, &resp, label);
    if check == OutcomeCheck::SaveJob && save_outcome_failed(&resp) {
        return Err(CliError::save_failed_already_emitted());
    }
    Ok(())
}

fn execute_inspect(
    client: &mut RpcClient,
    output: OutputFormat,
    target: &str,
) -> Result<(), CliError> {
    let opened = client
        .call(
            "bundle.open",
            json!({
                "bundle_id_or_path": target,
                "holder_kind": "cli",
                "holder_pid": std::process::id(),
            }),
        )
        .map_err(CliError::from_rpc)?;

    let lease_id = opened
        .get("lease_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let resolved_id = opened
        .get("bundle_id")
        .and_then(Value::as_str)
        .unwrap_or(target);

    let summary = client
        .call(
            "timeline.summary",
            json!({ "bundle_id_or_path": resolved_id }),
        )
        .map_err(CliError::from_rpc);
    let discontinuities = client
        .call(
            "timeline.discontinuities",
            json!({ "bundle_id_or_path": resolved_id }),
        )
        .map_err(CliError::from_rpc);

    if let Some(lease) = lease_id {
        let _ = client.call("bundle.close", json!({ "lease_id": lease }));
    }

    let summary = summary?;
    let discontinuities = discontinuities?;
    let report = json!({
        "bundle_id": opened.get("bundle_id"),
        "path": opened.get("path"),
        "format": opened.get("format"),
        "is_legacy": opened.get("is_legacy"),
        "summary": summary,
        "discontinuities": discontinuities.get("discontinuities").cloned().unwrap_or_else(|| json!([])),
    });
    emit_value(output, &report, "bundle inspection");
    Ok(())
}

fn execute_scrub_bench(
    client: &mut RpcClient,
    output: OutputFormat,
    target: &str,
    requested_frame: Option<u64>,
) -> Result<(), CliError> {
    let vp = client
        .call(
            "media.open_viewport",
            json!({ "bundle_id_or_path": target }),
        )
        .map_err(CliError::from_rpc)?;
    let viewport_id = vp
        .get("viewport_id")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::general("missing viewport_id from media.open_viewport"))?
        .to_owned();
    let shm_path = vp
        .get("shm_path")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::general("missing shm_path from media.open_viewport"))?
        .to_owned();
    let frame_count = vp.get("frame_count").and_then(Value::as_u64).unwrap_or(0);
    if frame_count == 0 {
        let _ = client.call(
            "media.close_viewport",
            json!({ "viewport_id": viewport_id }),
        );
        return Err(CliError::general("bundle video track contains 0 frames"));
    }

    let target_frame = requested_frame.unwrap_or_else(|| (frame_count / 2).min(frame_count - 1));
    let bench_result =
        run_scrub_bench_inner(client, &viewport_id, &shm_path, frame_count, target_frame);
    let _ = client.call(
        "media.close_viewport",
        json!({ "viewport_id": viewport_id }),
    );
    let report = bench_result?;
    emit_value(output, &report, "scrub-bench");
    Ok(())
}

fn run_scrub_bench_inner(
    client: &mut RpcClient,
    viewport_id: &str,
    shm_path: &str,
    frame_count: u64,
    target_frame: u64,
) -> Result<Value, CliError> {
    let shm_reader =
        ShmFrameRingReader::open(shm_path).map_err(|e| CliError::general(e.to_string()))?;
    let mut pixel_buf = Vec::new();

    let t_fs = Instant::now();
    let fs_resp = client
        .call("media.filmstrip", json!({ "viewport_id": viewport_id }))
        .map_err(CliError::from_rpc)?;
    let filmstrip_ms = t_fs.elapsed().as_secs_f64() * 1000.0;

    let t_scrub = Instant::now();
    let scrub_resp = client
        .call(
            "media.scrub",
            json!({ "viewport_id": viewport_id, "frame_index": target_frame }),
        )
        .map_err(CliError::from_rpc)?;
    let scrub_meta = shm_reader
        .read_latest(&mut pixel_buf)
        .map_err(|e| CliError::general(e.to_string()))?
        .ok_or_else(|| CliError::general("SHM ring had no frame after media.scrub"))?;
    let scrub_ms = t_scrub.elapsed().as_secs_f64() * 1000.0;

    let t_seek = Instant::now();
    let seek_resp = client
        .call(
            "media.seek_frame",
            json!({ "viewport_id": viewport_id, "frame_index": target_frame }),
        )
        .map_err(CliError::from_rpc)?;
    let seek_meta = shm_reader
        .read_latest(&mut pixel_buf)
        .map_err(|e| CliError::general(e.to_string()))?
        .ok_or_else(|| CliError::general("SHM ring had no frame after media.seek_frame"))?;
    let seek_ms = t_seek.elapsed().as_secs_f64() * 1000.0;

    let next_frame = (target_frame + 1).min(frame_count - 1);
    let t_step_fwd = Instant::now();
    let step_fwd_resp = client
        .call(
            "media.seek_frame",
            json!({ "viewport_id": viewport_id, "frame_index": next_frame }),
        )
        .map_err(CliError::from_rpc)?;
    let _ = shm_reader
        .read_latest(&mut pixel_buf)
        .map_err(|e| CliError::general(e.to_string()))?;
    let step_fwd_ms = t_step_fwd.elapsed().as_secs_f64() * 1000.0;

    let prev_frame = target_frame.saturating_sub(1);
    let t_step_back = Instant::now();
    let step_back_resp = client
        .call(
            "media.seek_frame",
            json!({ "viewport_id": viewport_id, "frame_index": prev_frame }),
        )
        .map_err(CliError::from_rpc)?;
    let _ = shm_reader
        .read_latest(&mut pixel_buf)
        .map_err(|e| CliError::general(e.to_string()))?;
    let step_back_ms = t_step_back.elapsed().as_secs_f64() * 1000.0;

    Ok(json!({
        "frame_count": frame_count,
        "target_frame": target_frame,
        "filmstrip": {
            "origin": fs_resp.get("origin"),
            "count": fs_resp.get("count"),
            "cache_path": fs_resp.get("cache_path"),
            "elapsed_ms": filmstrip_ms,
        },
        "scrub": {
            "requested_frame_index": scrub_resp.get("requested_frame_index"),
            "displayed_frame_index": scrub_resp.get("displayed_frame_index"),
            "shm_generation": scrub_meta.generation,
            "shm_frame_index": scrub_meta.frame_index,
            "shm_is_approximate": scrub_meta.is_approximate,
            "shm_width": scrub_meta.width,
            "shm_height": scrub_meta.height,
            "elapsed_ms": scrub_ms,
        },
        "seek_exact": {
            "frame_index": seek_resp.get("frame_index"),
            "shm_generation": seek_meta.generation,
            "shm_frame_index": seek_meta.frame_index,
            "shm_is_approximate": seek_meta.is_approximate,
            "shm_width": seek_meta.width,
            "shm_height": seek_meta.height,
            "cache_hits": seek_resp.get("cache_hits"),
            "cache_misses": seek_resp.get("cache_misses"),
            "elapsed_ms": seek_ms,
        },
        "step_forward": {
            "frame_index": next_frame,
            "cache_hits": step_fwd_resp.get("cache_hits"),
            "cache_misses": step_fwd_resp.get("cache_misses"),
            "elapsed_ms": step_fwd_ms,
        },
        "step_backward": {
            "frame_index": prev_frame,
            "cache_hits": step_back_resp.get("cache_hits"),
            "cache_misses": step_back_resp.get("cache_misses"),
            "elapsed_ms": step_back_ms,
        },
    }))
}

fn emit_value(output: OutputFormat, value: &Value, label: &str) {
    match output {
        OutputFormat::Json => {
            if let Ok(text) = serde_json::to_string_pretty(value) {
                println!("{text}");
            }
        }
        OutputFormat::Human => {
            println!("== {label} ==");
            if let Ok(text) = serde_json::to_string_pretty(value) {
                println!("{text}");
            }
        }
    }
}

fn print_help() {
    println!(
        "arb-cli {} — Android Replay Buffer CLI\n\n\
GLOBAL OPTIONS:\n  \
  --data-dir <DIR>       Override ARB_DATA_DIR\n  \
  --cache-dir <DIR>      Override ARB_CACHE_DIR\n  \
  --runtime-dir <DIR>    Override ARB_RUNTIME_DIR\n  \
  --json                 Emit machine-readable JSON (structured JSON errors on stderr)\n  \
  --upgrade-if-needed    Upgrade an older daemon at the selected runtime before connecting\n\
  --auto-start           Auto-spawn detached sibling arbd if not running (timeout: ARB_AUTO_START_TIMEOUT_MS)\n\n\
COMMANDS:\n  \
  status\n  \
  daemon start | stop\n  \
  devices\n  \
  capture start [--serial <S>] [--mode <video_and_logs|logs_only>] [--companion-jar <JAR>] [--scrcpy-jar <JAR>] [--max-size <N>] [--max-fps <N>] [--no-audio] [--no-logcat] [--no-input]\n  \
  capture-start [--serial <S>] [--mode <video_and_logs|logs_only>] ...  (alias for `capture start`)\n  \
  capture stop\n  \
  save [--seconds <N, 5..=600; default: config save_window_secs>] [--dest <DIR>] [--drain-timeout-ms <500>]\n  \
  save-logs [--window-secs <N>] [--dest <DIR>] [--drain-timeout-ms <500>]\n  \
  screenshot [--serial <S>] [--out <FILE.png> | --out-dir <DIR>] [--overwrite]\n  \
  config get | set [--save-window-secs <N>] [--ring-budget-bytes <N>] [--min-free-disk-bytes <N>] [--video-max-size <N>] [--video-max-fps <N>] [--audio-enabled <true|false>] [--audio-capture-mode playback_dup|output_compat (録音中は端末消音)] [--capture-mode <video_and_logs|logs_only>] [--device-serial <SERIAL|none>] [--crash-autosave-enabled <true|false>] [--crash-autosave-cooldown-secs <N>] [--output-dir-override <DIR|none>] [--screenshot-dir-override <DIR|none>] [--ide-default-save-action <save_and_open|save_only>]\n  \
  jobs list | retry <JOB_ID> | retarget <JOB_ID> --dest <DIR> | discard <JOB_ID>\n  \
  bundles list [--state active|trashed]\n  \
  bundles open <TARGET> [--holder-pid <PID>]   (acquires lease; release via `bundles close` or reaped on PID exit)\n  \
  bundles close <LEASE_ID>\n  \
  bundles trash <ID> | restore <ID> | gc [--now] | forget (<ID> | --all-missing)\n  \
  inspect <BUNDLE_ID_OR_PATH>\n  \
  trim <BUNDLE_ID_OR_PATH> --in-frame <N> --out-frame <M> [--dest <DIR>] [--include-share-mp4] [--burn-taps] [--trash-original]\n  \
  share-mp4 <BUNDLE_ID_OR_PATH> [--in-frame <N> --out-frame <M>] [--out <FILE.mp4>] [--dest <DIR>] [--burn-taps] [--trash-original]\n  \
  png <BUNDLE_ID_OR_PATH> --frame <N> --out <FILE.png>\n  \
  scrub-bench <BUNDLE_ID_OR_PATH> [--frame <N>]\n  \
  mcp                    Run stdio JSON-RPC 2.0 MCP server (auto-starts arbd)\n  \
  -V, --version          Print the arb-cli version\n\n\
EXIT CODES:\n  \
  0  Success\n  \
  1  General / transport / internal error\n  \
  2  CLI usage or invalid parameters\n  \
  3  Save / retry / retarget ended in failed_retryable (pins preserved)\n  \
  4  Resource not found\n  \
  5  State conflict",
        env!("CARGO_PKG_VERSION")
    );
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use serde_json::{Value, json};

    use super::{
        CliError, CommandSpec, EXIT_CONFLICT, EXIT_GENERAL_ERROR, EXIT_NOT_FOUND,
        EXIT_SAVE_FAILED_RETRYABLE, EXIT_USAGE_ERROR, OutputFormat, RPC_ERR_CONFLICT,
        RPC_ERR_INVALID_PARAMS, RPC_ERR_NOT_FOUND, format_error, parse_args, save_outcome_failed,
    };
    use crate::rpc::{AutoStartMode, RpcClientError};

    fn vec_str(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    /// Mirrors `absolutize_path` so expectations stay valid on Windows where
    /// `std::path::absolute("/tmp/...")` resolves to a drive-qualified path.
    fn abs(raw: &str) -> PathBuf {
        std::path::absolute(Path::new(raw)).expect("test path must absolutize")
    }

    fn abs_string(raw: &str) -> String {
        abs(raw).to_string_lossy().into_owned()
    }

    #[test]
    fn version_flag_parses_without_a_daemon() {
        for flag in ["--version", "-V"] {
            let (_opts, cmd) = parse_args(&vec_str(&[flag])).expect("version must parse");
            assert!(matches!(cmd, CommandSpec::Version), "{flag}");
        }
    }

    #[test]
    fn parses_global_flags_and_trim_subcommand() {
        let args = vec_str(&[
            "--data-dir",
            "/tmp/d",
            "--json",
            "--auto-start",
            "trim",
            "bundle-1",
            "--in-frame",
            "17",
            "--out-frame",
            "49",
            "--dest",
            "/tmp/out",
        ]);
        let (opts, cmd) = parse_args(&args).expect("trim args must parse");
        assert_eq!(opts.data_dir, Some(abs("/tmp/d")), "data_dir mismatch");
        assert_eq!(
            opts.output,
            OutputFormat::Json,
            "json flag must set Json output"
        );
        assert_eq!(
            opts.auto_start,
            AutoStartMode::Enabled,
            "auto-start flag must enable AutoStartMode"
        );
        assert_eq!(
            cmd,
            CommandSpec::Trim {
                target: "bundle-1".to_owned(),
                in_frame: 17,
                out_frame: 49,
                dest: Some(abs("/tmp/out")),
                burn_taps: false,
                include_share_mp4: false,
                trash_original: false,
            },
            "trim command must match"
        );

        let (_, burn_trim) = parse_args(&vec_str(&[
            "trim",
            "bundle-1",
            "--in-frame",
            "1",
            "--out-frame",
            "10",
            "--burn-taps",
            "--include-share-mp4",
            "--trash-original",
        ]))
        .expect("trim --burn-taps must parse");
        assert_eq!(
            burn_trim,
            CommandSpec::Trim {
                target: "bundle-1".to_owned(),
                in_frame: 1,
                out_frame: 10,
                dest: None,
                burn_taps: true,
                include_share_mp4: true,
                trash_original: true,
            },
            "trim burn-taps flags must match"
        );
    }

    #[test]
    fn parses_jobs_retarget_and_png_commands() {
        let retarget = parse_args(&vec_str(&[
            "jobs", "retarget", "job-42", "--dest", "/tmp/new", "--json",
        ]))
        .expect("jobs retarget must parse");
        assert_eq!(
            retarget.1,
            CommandSpec::JobsRetarget {
                job_id: "job-42".to_owned(),
                dest: abs("/tmp/new"),
            },
            "retarget command must match"
        );

        let png = parse_args(&vec_str(&[
            "png",
            "/tmp/b1",
            "--frame",
            "23",
            "--out",
            "/tmp/f23.png",
        ]))
        .expect("png command must parse");
        assert_eq!(
            png.1,
            CommandSpec::Png {
                target: abs_string("/tmp/b1"),
                frame: 23,
                out: abs("/tmp/f23.png"),
            },
            "png command must match"
        );
    }

    #[test]
    fn review_cli_paths_with_dotdot_are_normalized_identically_on_every_os() {
        use super::absolutize_path;
        let cwd = std::env::current_dir().expect("cwd");
        let resolved = absolutize_path(Path::new("a/../b/./c")).expect("absolutize");
        assert_eq!(
            resolved,
            cwd.join("b").join("c"),
            "`..`/`.` resolved textually"
        );
        assert!(
            !resolved
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
            "no `..` may reach arbd"
        );
        let up = absolutize_path(Path::new("../x")).expect("absolutize");
        assert!(
            up.is_absolute() && !up.to_string_lossy().contains(".."),
            "{up:?}"
        );
    }

    #[test]
    fn review_cli_defers_audio_and_save_window_to_persisted_config() {
        let (_, start) = parse_args(&vec_str(&["capture", "start"])).expect("capture start");
        match start {
            CommandSpec::CaptureStart { audio_enabled, .. } => {
                assert_eq!(
                    audio_enabled, None,
                    "no flag must defer to config.audio_enabled"
                );
            }
            other => panic!("unexpected {other:?}"),
        }
        let (_, no_audio) =
            parse_args(&vec_str(&["capture", "start", "--no-audio"])).expect("--no-audio");
        match no_audio {
            CommandSpec::CaptureStart { audio_enabled, .. } => {
                assert_eq!(audio_enabled, Some(false), "--no-audio forces off");
            }
            other => panic!("unexpected {other:?}"),
        }
        let (_, save) = parse_args(&vec_str(&["save"])).expect("save");
        match save {
            CommandSpec::Save { seconds, .. } => {
                assert_eq!(
                    seconds, None,
                    "no --seconds must defer to config.save_window_secs"
                );
            }
            other => panic!("unexpected {other:?}"),
        }
        let (_, save_n) = parse_args(&vec_str(&["save", "--seconds", "45"])).expect("save 45");
        match save_n {
            CommandSpec::Save { seconds, .. } => assert_eq!(seconds, Some(45), "explicit"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn r3_m8_absolutizes_relative_cli_paths_and_keeps_bundle_ids() {
        let cwd = std::env::current_dir().expect("cwd");

        let (_, save_cmd) =
            parse_args(&vec_str(&["save", "--dest", "rel/out"])).expect("save parses");
        assert_eq!(
            save_cmd,
            CommandSpec::Save {
                seconds: None,
                dest: Some(cwd.join("rel/out")),
                drain_timeout_ms: None,
            },
            "relative --dest must be absolutized against CLI cwd"
        );

        let (_, png_cmd) = parse_args(&vec_str(&[
            "png",
            "./my-bundle",
            "--frame",
            "3",
            "--out",
            "f3.png",
        ]))
        .expect("png parses");
        match png_cmd {
            CommandSpec::Png { target, frame, out } => {
                assert!(
                    PathBuf::from(&target).is_absolute(),
                    "relative bundle path must be absolutized: {target}"
                );
                assert_eq!(frame, 3, "frame index mismatch");
                assert_eq!(
                    out,
                    cwd.join("f3.png"),
                    "relative --out must be absolutized"
                );
            }
            other => panic!("unexpected cmd: {other:?}"),
        }

        let (_, cap_cmd) = parse_args(&vec_str(&[
            "capture",
            "start",
            "--companion-jar",
            "jars/comp.jar",
            "--scrcpy-jar",
            "jars/scrcpy.jar",
        ]))
        .expect("capture start parses");
        match cap_cmd {
            CommandSpec::CaptureStart {
                companion_jar,
                scrcpy_jar,
                ..
            } => {
                assert_eq!(
                    companion_jar,
                    Some(cwd.join("jars/comp.jar")),
                    "companion_jar must be absolutized"
                );
                assert_eq!(
                    scrcpy_jar,
                    Some(cwd.join("jars/scrcpy.jar")),
                    "scrcpy_jar must be absolutized"
                );
            }
            other => panic!("unexpected cmd: {other:?}"),
        }

        let (_, inspect_id) =
            parse_args(&vec_str(&["inspect", "bundle-abc-123"])).expect("inspect parses");
        assert_eq!(
            inspect_id,
            CommandSpec::Inspect {
                target: "bundle-abc-123".to_owned(),
            },
            "plain catalog bundle ID must not be turned into a path"
        );
    }

    #[test]
    fn r3_s6_parses_bundles_close_and_open_holder_pid() {
        let (_, open_cmd) = parse_args(&vec_str(&[
            "bundles",
            "open",
            "bundle-1",
            "--holder-pid",
            "12345",
        ]))
        .expect("bundles open parses");
        assert_eq!(
            open_cmd,
            CommandSpec::BundlesOpen {
                target: "bundle-1".to_owned(),
                holder_pid: Some(12_345),
            },
            "bundles open must parse --holder-pid"
        );

        let (_, close_cmd) =
            parse_args(&vec_str(&["bundles", "close", "lease-99"])).expect("bundles close parses");
        assert_eq!(
            close_cmd,
            CommandSpec::BundlesClose {
                lease_id: "lease-99".to_owned(),
            },
            "bundles close must parse lease_id"
        );

        let (_, gc_default) = parse_args(&vec_str(&["bundles", "gc"])).expect("bundles gc");
        assert_eq!(
            gc_default,
            CommandSpec::BundlesGc {
                ignore_grace: false,
            }
        );
        let (_, gc_now) =
            parse_args(&vec_str(&["bundles", "gc", "--now"])).expect("bundles gc --now");
        assert_eq!(gc_now, CommandSpec::BundlesGc { ignore_grace: true });
        assert!(parse_args(&vec_str(&["bundles", "gc", "--bogus"])).is_err());

        let (_, forget_one) =
            parse_args(&vec_str(&["bundles", "forget", "b-1"])).expect("bundles forget <id>");
        assert_eq!(
            forget_one,
            CommandSpec::BundlesForget {
                bundle_id: Some("b-1".to_owned()),
                all_missing: false,
            }
        );
        let (_, forget_all) = parse_args(&vec_str(&["bundles", "forget", "--all-missing"]))
            .expect("bundles forget --all-missing");
        assert_eq!(
            forget_all,
            CommandSpec::BundlesForget {
                bundle_id: None,
                all_missing: true,
            }
        );
        assert!(parse_args(&vec_str(&["bundles", "forget"])).is_err());
        assert!(parse_args(&vec_str(&["bundles", "forget", "b-1", "--all-missing"])).is_err());
    }

    #[test]
    fn r3_s17_maps_rpc_errors_and_failed_save_to_distinct_exit_codes_and_json_errors() {
        let not_found = CliError::from_rpc(RpcClientError::Rpc {
            code: RPC_ERR_NOT_FOUND,
            message: "missing".to_owned(),
        });
        assert_eq!(not_found.exit_code, EXIT_NOT_FOUND, "not found exit code");

        let conflict = CliError::from_rpc(RpcClientError::Rpc {
            code: RPC_ERR_CONFLICT,
            message: "busy".to_owned(),
        });
        assert_eq!(conflict.exit_code, EXIT_CONFLICT, "conflict exit code");

        let invalid = CliError::from_rpc(RpcClientError::Rpc {
            code: RPC_ERR_INVALID_PARAMS,
            message: "bad path".to_owned(),
        });
        assert_eq!(
            invalid.exit_code, EXIT_USAGE_ERROR,
            "invalid params exit code"
        );

        let other = CliError::general("socket down");
        assert_eq!(
            other.exit_code, EXIT_GENERAL_ERROR,
            "general error exit code"
        );

        let save_err = CliError::save_failed_already_emitted();
        assert_eq!(
            save_err.exit_code, EXIT_SAVE_FAILED_RETRYABLE,
            "failed_retryable save must exit 3"
        );
        assert!(
            save_outcome_failed(
                &json!({ "bundle_id": null, "job": { "state": "failed_retryable" } })
            ),
            "null bundle_id must be detected as failed save"
        );
        assert!(
            save_outcome_failed(
                &json!({ "bundle_id": "bundle-1", "job": { "state": "failed_retryable" } })
            ),
            "failed_retryable state must be detected as failed save even when bundle_id is set"
        );
        assert!(
            !save_outcome_failed(
                &json!({ "bundle_id": "bundle-1", "job": { "state": "completed" } })
            ),
            "non-null bundle_id must succeed"
        );

        let formatted = format_error(OutputFormat::Json, &not_found);
        let parsed: Value = serde_json::from_str(&formatted).expect("JSON error must parse");
        assert_eq!(
            parsed["error"]["code"], RPC_ERR_NOT_FOUND,
            "JSON error code"
        );
        assert_eq!(
            parsed["error"]["exit_code"], EXIT_NOT_FOUND,
            "JSON error exit_code"
        );
        assert_eq!(parsed["error"]["message"], "missing", "JSON error message");
    }

    #[test]
    fn t1_parses_screenshot_save_logs_and_config_commands() {
        let cwd = std::env::current_dir().expect("cwd");

        let (_, shot) = parse_args(&vec_str(&[
            "screenshot",
            "--serial",
            "emu-5554",
            "--out",
            "shots/s1.png",
            "--overwrite",
        ]))
        .expect("screenshot parses");
        assert_eq!(
            shot,
            CommandSpec::Screenshot {
                serial: Some("emu-5554".to_owned()),
                out: Some(cwd.join("shots/s1.png")),
                out_dir: None,
                overwrite: true,
            },
            "screenshot command must match"
        );

        let (_, save_logs) = parse_args(&vec_str(&[
            "save-logs",
            "--window-secs",
            "90",
            "--dest",
            "logs-out",
        ]))
        .expect("save-logs parses");
        assert_eq!(
            save_logs,
            CommandSpec::SaveLogs {
                window_secs: Some(90),
                dest: Some(cwd.join("logs-out")),
                drain_timeout_ms: None,
            },
            "save-logs command must match"
        );

        let (_, cfg_get) = parse_args(&vec_str(&["config", "get"])).expect("config get parses");
        assert_eq!(cfg_get, CommandSpec::ConfigGet, "config get must match");

        let (_, cfg_set) = parse_args(&vec_str(&[
            "config",
            "set",
            "--save-window-secs",
            "120",
            "--audio-enabled",
            "false",
            "--audio-capture-mode",
            "output_compat",
            "--crash-autosave-cooldown-secs",
            "20",
            "--output-dir-override",
            "none",
            "--ide-default-save-action",
            "save_only",
        ]))
        .expect("config set parses");
        assert_eq!(
            cfg_set,
            CommandSpec::ConfigSet {
                patch: json!({
                    "save_window_secs": 120,
                    "audio_enabled": false,
                    "audio_capture_mode": "output_compat",
                    "crash_autosave_cooldown_secs": 20,
                    "output_dir_override": null,
                    "ide_default_save_action": "save_only",
                }),
            },
            "config set patch must match"
        );
    }

    #[test]
    fn t2b_parses_mcp_and_share_mp4_commands() {
        let cwd = std::env::current_dir().expect("cwd");
        let (_, mcp_cmd) = parse_args(&vec_str(&["mcp"])).expect("mcp parses");
        assert_eq!(mcp_cmd, CommandSpec::Mcp, "mcp command must match");

        let (_, share_cmd) = parse_args(&vec_str(&[
            "share-mp4",
            "bundle-7",
            "--in-frame",
            "5",
            "--out-frame",
            "20",
            "--out",
            "clips/out.mp4",
            "--trash-original",
        ]))
        .expect("share-mp4 parses");
        assert_eq!(
            share_cmd,
            CommandSpec::ShareMp4 {
                target: "bundle-7".to_owned(),
                in_frame: Some(5),
                out_frame: Some(20),
                out: Some(cwd.join("clips/out.mp4")),
                dest: None,
                burn_taps: false,
                trash_original: true,
            },
            "share-mp4 command must match"
        );

        let (_, share_burn) = parse_args(&vec_str(&["share-mp4", "bundle-7", "--burn-taps"]))
            .expect("share-mp4 burn");
        assert_eq!(
            share_burn,
            CommandSpec::ShareMp4 {
                target: "bundle-7".to_owned(),
                in_frame: None,
                out_frame: None,
                out: None,
                dest: None,
                burn_taps: true,
                trash_original: false,
            },
            "share-mp4 --burn-taps must parse"
        );

        let err = parse_args(&vec_str(&["share-mp4", "bundle-7", "--in-frame", "5"]))
            .expect_err("half-specified range must fail");
        assert_eq!(
            err.exit_code, EXIT_USAGE_ERROR,
            "half range must be usage error"
        );
    }

    #[test]
    fn t6c_parses_logs_only_capture_start_wifi_serial_and_screenshot_dir_flags() {
        let cwd = std::env::current_dir().expect("cwd");

        let (_, cap) = parse_args(&vec_str(&[
            "capture-start",
            "--mode",
            "logs_only",
            "--serial",
            "192.168.1.50:5555",
        ]))
        .expect("capture-start alias must parse");
        match cap {
            CommandSpec::CaptureStart {
                serial,
                capture_mode,
                ..
            } => {
                assert_eq!(serial.as_deref(), Some("192.168.1.50:5555"));
                assert_eq!(capture_mode, Some(super::CaptureMode::LogsOnly));
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let (_, cfg) = parse_args(&vec_str(&[
            "config",
            "set",
            "--capture-mode",
            "logs_only",
            "--device-serial",
            "adb-R3CN1234-AbCdEf._adb-tls-connect._tcp",
            "--screenshot-dir-override",
            "shots-dir",
        ]))
        .expect("config set T-6C flags must parse");
        assert_eq!(
            cfg,
            CommandSpec::ConfigSet {
                patch: json!({
                    "capture_mode": "logs_only",
                    "device_serial": "adb-R3CN1234-AbCdEf._adb-tls-connect._tcp",
                    "screenshot_dir_override": cwd.join("shots-dir"),
                }),
            }
        );

        let (_, shot_dir) = parse_args(&vec_str(&[
            "screenshot",
            "--serial",
            "[fe80::1]:5555",
            "--out-dir",
            "shots-dir",
        ]))
        .expect("screenshot --out-dir must parse");
        assert_eq!(
            shot_dir,
            CommandSpec::Screenshot {
                serial: Some("[fe80::1]:5555".to_owned()),
                out: None,
                out_dir: Some(cwd.join("shots-dir")),
                overwrite: false,
            }
        );

        let bad_serial = parse_args(&vec_str(&["capture-start", "--serial", "bad;rm -rf /"]))
            .expect_err("malformed serial must be rejected");
        assert_eq!(bad_serial.exit_code, EXIT_USAGE_ERROR);

        let both_out = parse_args(&vec_str(&[
            "screenshot",
            "--out",
            "a.png",
            "--out-dir",
            "shots",
        ]))
        .expect_err("passing both --out and --out-dir must fail");
        assert_eq!(both_out.exit_code, EXIT_USAGE_ERROR);
    }
}
