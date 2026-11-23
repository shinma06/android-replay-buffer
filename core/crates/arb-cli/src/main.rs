#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "CLI binary prints command output and diagnostics to stdio"
)]

//! Scriptable CLI client (`arb-cli`) communicating with `arbd` strictly over local IPC + SHM.

mod rpc;

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use arb_os::paths::ArbPaths;
use arb_os::shm::ShmFrameRingReader;
use serde_json::{Value, json};

use crate::rpc::{AutoStartMode, RpcClient, spawn_daemon_process, wait_for_daemon};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Clone)]
struct GlobalOptions {
    data_dir: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
    runtime_dir: Option<PathBuf>,
    output: OutputFormat,
    auto_start: AutoStartMode,
}

impl Default for GlobalOptions {
    fn default() -> Self {
        Self {
            data_dir: None,
            cache_dir: None,
            runtime_dir: None,
            output: OutputFormat::Human,
            auto_start: AutoStartMode::Disabled,
        }
    }
}

impl GlobalOptions {
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum CommandSpec {
    Help,
    Status,
    DaemonStart,
    DaemonStop,
    Devices,
    CaptureStart {
        serial: Option<String>,
        companion_jar: Option<PathBuf>,
        scrcpy_jar: Option<PathBuf>,
        max_size: Option<u32>,
        max_fps: Option<u32>,
        video_bit_rate: Option<u32>,
        audio_enabled: bool,
        logcat_enabled: bool,
        input_enabled: bool,
        heartbeat_ms: Option<u32>,
    },
    CaptureStop,
    Save {
        seconds: u64,
        dest: Option<PathBuf>,
        drain_timeout_ms: Option<u64>,
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
    },
    BundlesTrash {
        bundle_id: String,
    },
    BundlesRestore {
        bundle_id: String,
    },
    BundlesGc,
    Inspect {
        target: String,
    },
    Trim {
        target: String,
        in_frame: u64,
        out_frame: u64,
        dest: Option<PathBuf>,
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
}

fn main() -> ExitCode {
    let raw_args: Vec<String> = env::args().skip(1).collect();
    match parse_args(&raw_args).and_then(|(opts, cmd)| execute(&opts, cmd)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn parse_args(args: &[String]) -> Result<(GlobalOptions, CommandSpec), String> {
    let mut opts = GlobalOptions::default();
    let mut rest: Vec<String> = Vec::with_capacity(args.len());
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
            "--json" => {
                opts.output = OutputFormat::Json;
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

    let cmd = match rest[0].as_str() {
        "status" => CommandSpec::Status,
        "devices" => CommandSpec::Devices,
        "daemon" => parse_daemon_cmd(&rest[1..])?,
        "capture" => parse_capture_cmd(&rest[1..])?,
        "save" => parse_save_cmd(&rest[1..])?,
        "jobs" => parse_jobs_cmd(&rest[1..])?,
        "bundles" => parse_bundles_cmd(&rest[1..])?,
        "inspect" => {
            let target = rest
                .get(1)
                .ok_or_else(|| "inspect requires <bundle-id-or-path>".to_owned())?
                .clone();
            CommandSpec::Inspect { target }
        }
        "trim" => parse_trim_cmd(&rest[1..])?,
        "png" => parse_png_cmd(&rest[1..])?,
        "scrub-bench" => parse_scrub_bench_cmd(&rest[1..])?,
        other => return Err(format!("unknown subcommand `{other}` (run with --help)")),
    };

    Ok((opts, cmd))
}

fn parse_daemon_cmd(args: &[String]) -> Result<CommandSpec, String> {
    match args.first().map(String::as_str) {
        Some("start") => Ok(CommandSpec::DaemonStart),
        Some("stop") => Ok(CommandSpec::DaemonStop),
        other => Err(format!(
            "usage: arb-cli daemon <start|stop> (got {other:?})"
        )),
    }
}

fn parse_capture_cmd(args: &[String]) -> Result<CommandSpec, String> {
    match args.first().map(String::as_str) {
        Some("stop") => Ok(CommandSpec::CaptureStop),
        Some("start") => {
            let mut serial = None;
            let mut companion_jar = None;
            let mut scrcpy_jar = None;
            let mut max_size = None;
            let mut max_fps = None;
            let mut video_bit_rate = None;
            let mut audio_enabled = true;
            let mut logcat_enabled = true;
            let mut input_enabled = true;
            let mut heartbeat_ms = None;

            let mut i = 1_usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--serial" => {
                        serial = Some(require_val(args, i, "--serial")?.to_owned());
                        i += 2;
                    }
                    "--companion-jar" => {
                        companion_jar =
                            Some(PathBuf::from(require_val(args, i, "--companion-jar")?));
                        i += 2;
                    }
                    "--scrcpy-jar" => {
                        scrcpy_jar = Some(PathBuf::from(require_val(args, i, "--scrcpy-jar")?));
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
                        audio_enabled = false;
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
                    other => return Err(format!("unknown capture start option `{other}`")),
                }
            }

            Ok(CommandSpec::CaptureStart {
                serial,
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
        other => Err(format!(
            "usage: arb-cli capture <start|stop> (got {other:?})"
        )),
    }
}

fn parse_save_cmd(args: &[String]) -> Result<CommandSpec, String> {
    let mut seconds = 30_u64;
    let mut dest = None;
    let mut drain_timeout_ms = None;
    let mut i = 0_usize;

    while i < args.len() {
        match args[i].as_str() {
            "--seconds" => {
                seconds = parse_u64(require_val(args, i, "--seconds")?, "--seconds")?;
                i += 2;
            }
            "--dest" => {
                dest = Some(PathBuf::from(require_val(args, i, "--dest")?));
                i += 2;
            }
            "--drain-timeout-ms" => {
                drain_timeout_ms = Some(parse_u64(
                    require_val(args, i, "--drain-timeout-ms")?,
                    "--drain-timeout-ms",
                )?);
                i += 2;
            }
            other => return Err(format!("unknown save option `{other}`")),
        }
    }

    Ok(CommandSpec::Save {
        seconds,
        dest,
        drain_timeout_ms,
    })
}

fn parse_jobs_cmd(args: &[String]) -> Result<CommandSpec, String> {
    match args.first().map(String::as_str) {
        Some("list") => Ok(CommandSpec::JobsList),
        Some("retry") => {
            let job_id = args
                .get(1)
                .ok_or_else(|| "jobs retry requires <job-id>".to_owned())?
                .clone();
            Ok(CommandSpec::JobsRetry { job_id })
        }
        Some("discard") => {
            let job_id = args
                .get(1)
                .ok_or_else(|| "jobs discard requires <job-id>".to_owned())?
                .clone();
            Ok(CommandSpec::JobsDiscard { job_id })
        }
        Some("retarget") => {
            let job_id = args
                .get(1)
                .ok_or_else(|| "jobs retarget requires <job-id> --dest <dir>".to_owned())?
                .clone();
            let mut dest = None;
            let mut i = 2_usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--dest" => {
                        dest = Some(PathBuf::from(require_val(args, i, "--dest")?));
                        i += 2;
                    }
                    other => return Err(format!("unknown jobs retarget option `{other}`")),
                }
            }
            let dest = dest.ok_or_else(|| "jobs retarget requires --dest <dir>".to_owned())?;
            Ok(CommandSpec::JobsRetarget { job_id, dest })
        }
        other => Err(format!(
            "usage: arb-cli jobs <list|retry|retarget|discard> (got {other:?})"
        )),
    }
}

fn parse_bundles_cmd(args: &[String]) -> Result<CommandSpec, String> {
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
                    other => return Err(format!("unknown bundles list option `{other}`")),
                }
            }
            Ok(CommandSpec::BundlesList { state })
        }
        Some("open") => {
            let target = args
                .get(1)
                .ok_or_else(|| "bundles open requires <bundle-id-or-path>".to_owned())?
                .clone();
            Ok(CommandSpec::BundlesOpen { target })
        }
        Some("trash") => {
            let bundle_id = args
                .get(1)
                .ok_or_else(|| "bundles trash requires <bundle-id>".to_owned())?
                .clone();
            Ok(CommandSpec::BundlesTrash { bundle_id })
        }
        Some("restore") => {
            let bundle_id = args
                .get(1)
                .ok_or_else(|| "bundles restore requires <bundle-id>".to_owned())?
                .clone();
            Ok(CommandSpec::BundlesRestore { bundle_id })
        }
        Some("gc") => Ok(CommandSpec::BundlesGc),
        other => Err(format!(
            "usage: arb-cli bundles <list|open|trash|restore|gc> (got {other:?})"
        )),
    }
}

fn parse_trim_cmd(args: &[String]) -> Result<CommandSpec, String> {
    let target = args
        .first()
        .ok_or_else(|| {
            "usage: arb-cli trim <bundle-id-or-path> --in-frame <n> --out-frame <m> [--dest <dir>]"
                .to_owned()
        })?
        .clone();
    let mut in_frame = None;
    let mut out_frame = None;
    let mut dest = None;
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
                dest = Some(PathBuf::from(require_val(args, i, "--dest")?));
                i += 2;
            }
            other => return Err(format!("unknown trim option `{other}`")),
        }
    }

    let in_frame = in_frame.ok_or_else(|| "trim requires --in-frame <n>".to_owned())?;
    let out_frame = out_frame.ok_or_else(|| "trim requires --out-frame <m>".to_owned())?;
    Ok(CommandSpec::Trim {
        target,
        in_frame,
        out_frame,
        dest,
    })
}

fn parse_png_cmd(args: &[String]) -> Result<CommandSpec, String> {
    let target = args
        .first()
        .ok_or_else(|| {
            "usage: arb-cli png <bundle-id-or-path> --frame <n> --out <file.png>".to_owned()
        })?
        .clone();
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
                out = Some(PathBuf::from(require_val(args, i, "--out")?));
                i += 2;
            }
            other => return Err(format!("unknown png option `{other}`")),
        }
    }

    let frame = frame.ok_or_else(|| "png requires --frame <n>".to_owned())?;
    let out = out.ok_or_else(|| "png requires --out <file.png>".to_owned())?;
    Ok(CommandSpec::Png { target, frame, out })
}

fn parse_scrub_bench_cmd(args: &[String]) -> Result<CommandSpec, String> {
    let target = args
        .first()
        .ok_or_else(|| "usage: arb-cli scrub-bench <bundle-id-or-path> [--frame <n>]".to_owned())?
        .clone();
    let mut frame = None;
    let mut i = 1_usize;
    while i < args.len() {
        match args[i].as_str() {
            "--frame" => {
                frame = Some(parse_u64(require_val(args, i, "--frame")?, "--frame")?);
                i += 2;
            }
            other => return Err(format!("unknown scrub-bench option `{other}`")),
        }
    }
    Ok(CommandSpec::ScrubBench { target, frame })
}

fn require_val<'a>(args: &'a [String], idx: usize, flag: &str) -> Result<&'a str, String> {
    args.get(idx + 1)
        .map(String::as_str)
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn parse_u32(raw: &str, flag: &str) -> Result<u32, String> {
    raw.parse::<u32>()
        .map_err(|_| format!("{flag} expects an unsigned 32-bit integer, got `{raw}`"))
}

fn parse_u64(raw: &str, flag: &str) -> Result<u64, String> {
    raw.parse::<u64>()
        .map_err(|_| format!("{flag} expects an unsigned 64-bit integer, got `{raw}`"))
}

fn execute(opts: &GlobalOptions, cmd: CommandSpec) -> Result<(), String> {
    if matches!(cmd, CommandSpec::Help) {
        print_help();
        return Ok(());
    }

    let paths = opts.resolve_paths()?;

    if matches!(cmd, CommandSpec::DaemonStart) {
        let endpoint = paths.ipc_endpoint();
        let mut client = if let Ok(c) = RpcClient::connect_endpoint(&endpoint) {
            c
        } else {
            spawn_daemon_process(&paths).map_err(|e| e.to_string())?;
            wait_for_daemon(&endpoint, Duration::from_secs(5)).map_err(|e| e.to_string())?
        };
        let hello = client
            .call("system.hello", json!({}))
            .map_err(|e| e.to_string())?;
        emit_value(opts.output, &hello, "arbd started");
        return Ok(());
    }

    let mut client = RpcClient::connect(&paths, opts.auto_start).map_err(|e| e.to_string())?;
    dispatch_connected(&mut client, opts.output, cmd)
}

fn dispatch_connected(
    client: &mut RpcClient,
    output: OutputFormat,
    cmd: CommandSpec,
) -> Result<(), String> {
    match cmd {
        CommandSpec::Help | CommandSpec::DaemonStart => Ok(()),
        CommandSpec::Status => dispatch_status(client, output),
        CommandSpec::Inspect { target } => execute_inspect(client, output, &target),
        CommandSpec::ScrubBench { target, frame } => {
            execute_scrub_bench(client, output, &target, frame)
        }
        other => {
            let (method, params, label) = command_to_rpc(other);
            call_and_emit(client, output, method, params, label)
        }
    }
}

fn dispatch_status(client: &mut RpcClient, output: OutputFormat) -> Result<(), String> {
    let hello = client
        .call("system.hello", json!({}))
        .map_err(|e| e.to_string())?;
    let status = client
        .call("system.status", json!({}))
        .map_err(|e| e.to_string())?;
    emit_value(
        output,
        &json!({ "hello": hello, "status": status }),
        "arbd status",
    );
    Ok(())
}

fn command_to_rpc(cmd: CommandSpec) -> (&'static str, Value, &'static str) {
    match cmd {
        CommandSpec::DaemonStop => ("system.shutdown", json!({}), "arbd shutdown requested"),
        CommandSpec::Devices => ("device.list", json!({}), "devices"),
        CommandSpec::CaptureStart {
            serial,
            companion_jar,
            scrcpy_jar,
            max_size,
            max_fps,
            video_bit_rate,
            audio_enabled,
            logcat_enabled,
            input_enabled,
            heartbeat_ms,
        } => (
            "capture.start",
            json!({
                "serial": serial,
                "companion_jar": companion_jar,
                "scrcpy_jar": scrcpy_jar,
                "max_size": max_size,
                "max_fps": max_fps,
                "video_bit_rate": video_bit_rate,
                "audio_enabled": audio_enabled,
                "logcat_enabled": logcat_enabled,
                "input_enabled": input_enabled,
                "heartbeat_ms": heartbeat_ms,
            }),
            "capture started",
        ),
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
        CommandSpec::BundlesOpen { target } => (
            "bundle.open",
            json!({ "bundle_id_or_path": target, "holder_kind": "cli", "holder_pid": std::process::id() }),
            "bundle opened",
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
        CommandSpec::BundlesGc => ("bundle.gc", json!({}), "bundle gc"),
        CommandSpec::Trim {
            target,
            in_frame,
            out_frame,
            dest,
        } => (
            "export.trim",
            json!({
                "bundle_id_or_path": target,
                "in_frame_index": in_frame,
                "out_frame_index": out_frame,
                "dest_dir": dest,
                "holder_pid": std::process::id(),
            }),
            "trim result",
        ),
        CommandSpec::Png { target, frame, out } => (
            "export.png",
            json!({ "bundle_id_or_path": target, "frame_index": frame, "output_path": out }),
            "png export",
        ),
        CommandSpec::Help
        | CommandSpec::DaemonStart
        | CommandSpec::Status
        | CommandSpec::Inspect { .. }
        | CommandSpec::ScrubBench { .. } => ("system.status", json!({}), "arbd status"),
    }
}

fn call_and_emit(
    client: &mut RpcClient,
    output: OutputFormat,
    method: &str,
    params: Value,
    label: &str,
) -> Result<(), String> {
    let resp = client.call(method, params).map_err(|e| e.to_string())?;
    emit_value(output, &resp, label);
    Ok(())
}

fn execute_inspect(
    client: &mut RpcClient,
    output: OutputFormat,
    target: &str,
) -> Result<(), String> {
    let opened = client
        .call(
            "bundle.open",
            json!({
                "bundle_id_or_path": target,
                "holder_kind": "cli",
                "holder_pid": std::process::id(),
            }),
        )
        .map_err(|e| e.to_string())?;

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
        .map_err(|e| e.to_string());
    let discontinuities = client
        .call(
            "timeline.discontinuities",
            json!({ "bundle_id_or_path": resolved_id }),
        )
        .map_err(|e| e.to_string());

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
) -> Result<(), String> {
    let vp = client
        .call(
            "media.open_viewport",
            json!({ "bundle_id_or_path": target }),
        )
        .map_err(|e| e.to_string())?;
    let viewport_id = vp
        .get("viewport_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing viewport_id from media.open_viewport".to_owned())?
        .to_owned();
    let shm_path = vp
        .get("shm_path")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing shm_path from media.open_viewport".to_owned())?
        .to_owned();
    let frame_count = vp.get("frame_count").and_then(Value::as_u64).unwrap_or(0);
    if frame_count == 0 {
        let _ = client.call(
            "media.close_viewport",
            json!({ "viewport_id": viewport_id }),
        );
        return Err("bundle video track contains 0 frames".to_owned());
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
) -> Result<Value, String> {
    let shm_reader = ShmFrameRingReader::open(shm_path).map_err(|e| e.to_string())?;
    let mut pixel_buf = Vec::new();

    let t_fs = Instant::now();
    let fs_resp = client
        .call("media.filmstrip", json!({ "viewport_id": viewport_id }))
        .map_err(|e| e.to_string())?;
    let filmstrip_ms = t_fs.elapsed().as_secs_f64() * 1000.0;

    let t_scrub = Instant::now();
    let scrub_resp = client
        .call(
            "media.scrub",
            json!({ "viewport_id": viewport_id, "frame_index": target_frame }),
        )
        .map_err(|e| e.to_string())?;
    let scrub_meta = shm_reader
        .read_latest(&mut pixel_buf)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "SHM ring had no frame after media.scrub".to_owned())?;
    let scrub_ms = t_scrub.elapsed().as_secs_f64() * 1000.0;

    let t_seek = Instant::now();
    let seek_resp = client
        .call(
            "media.seek_frame",
            json!({ "viewport_id": viewport_id, "frame_index": target_frame }),
        )
        .map_err(|e| e.to_string())?;
    let seek_meta = shm_reader
        .read_latest(&mut pixel_buf)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "SHM ring had no frame after media.seek_frame".to_owned())?;
    let seek_ms = t_seek.elapsed().as_secs_f64() * 1000.0;

    let next_frame = (target_frame + 1).min(frame_count - 1);
    let t_step_fwd = Instant::now();
    let step_fwd_resp = client
        .call(
            "media.seek_frame",
            json!({ "viewport_id": viewport_id, "frame_index": next_frame }),
        )
        .map_err(|e| e.to_string())?;
    let _ = shm_reader
        .read_latest(&mut pixel_buf)
        .map_err(|e| e.to_string())?;
    let step_fwd_ms = t_step_fwd.elapsed().as_secs_f64() * 1000.0;

    let prev_frame = target_frame.saturating_sub(1);
    let t_step_back = Instant::now();
    let step_back_resp = client
        .call(
            "media.seek_frame",
            json!({ "viewport_id": viewport_id, "frame_index": prev_frame }),
        )
        .map_err(|e| e.to_string())?;
    let _ = shm_reader
        .read_latest(&mut pixel_buf)
        .map_err(|e| e.to_string())?;
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
        "arb-cli {} — Android Replay Buffer v2 CLI\n\n\
GLOBAL OPTIONS:\n  \
  --data-dir <DIR>       Override ARB_DATA_DIR\n  \
  --cache-dir <DIR>      Override ARB_CACHE_DIR\n  \
  --runtime-dir <DIR>    Override ARB_RUNTIME_DIR\n  \
  --json                 Emit machine-readable JSON\n  \
  --auto-start           Auto-spawn sibling arbd if not running\n\n\
COMMANDS:\n  \
  status\n  \
  daemon start | stop\n  \
  devices\n  \
  capture start [--serial <S>] [--max-size <N>] [--max-fps <N>] [--no-audio] [--no-logcat] [--no-input]\n  \
  capture stop\n  \
  save [--seconds <30>] [--dest <DIR>] [--drain-timeout-ms <500>]\n  \
  jobs list | retry <JOB_ID> | retarget <JOB_ID> --dest <DIR> | discard <JOB_ID>\n  \
  bundles list [--state active|trashed] | open <TARGET> | trash <ID> | restore <ID> | gc\n  \
  inspect <BUNDLE_ID_OR_PATH>\n  \
  trim <BUNDLE_ID_OR_PATH> --in-frame <N> --out-frame <M> [--dest <DIR>]\n  \
  png <BUNDLE_ID_OR_PATH> --frame <N> --out <FILE.png>\n  \
  scrub-bench <BUNDLE_ID_OR_PATH> [--frame <N>]",
        env!("CARGO_PKG_VERSION")
    );
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{CommandSpec, OutputFormat, parse_args};
    use crate::rpc::AutoStartMode;

    fn vec_str(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
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
        assert_eq!(
            opts.data_dir,
            Some(PathBuf::from("/tmp/d")),
            "data_dir mismatch"
        );
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
                dest: Some(PathBuf::from("/tmp/out")),
            },
            "trim command must match"
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
                dest: PathBuf::from("/tmp/new"),
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
                target: "/tmp/b1".to_owned(),
                frame: 23,
                out: PathBuf::from("/tmp/f23.png"),
            },
            "png command must match"
        );
    }
}
