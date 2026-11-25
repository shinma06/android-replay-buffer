//! Real IPC integration tests: drive [`arb_tray::controller::TrayController`] over the actual
//! `arb_os::ipc::LocalIpcStream` transport against a hand-rolled mock JSON-RPC 2.0 server (no
//! dependency on the `arbd` crate, avoiding a circular workspace dependency; see `SPEC-01`).
//!
//! Proves end to end that the controller:
//! - fetches `system.status` / `config.get` / `device.list` / `save.list_jobs` / `bundle.list` and
//!   derives the expected [`TrayViewState`] (including strict seconds notation & `save_window_secs`),
//! - dispatches `save.trigger`, `save.screenshot`, `save.logs`, and `save.retry` RPCs, and
//! - reconnects deterministically across all 3 OSes (Unix sockets & Windows Named Pipes) after the
//!   server drops connections mid-session.

use std::io::BufReader;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use arb_os::ipc::{
    DEFAULT_MAX_IPC_LINE_BYTES, LocalIpcListener, read_frame_line, write_frame_line,
};
use arb_os::paths::ArbPaths;
use arb_tray::controller::{
    CaptureStatus, TrayAlert, TrayController, TrayMenuEvent, build_menu, contains_emoji,
};
use arb_tray::rpc::AutoStartMode;
use serde_json::{Value, json};

const SYNC_TIMEOUT: Duration = Duration::from_secs(5);

/// One JSON-RPC request observed by the mock server, recorded for assertions after the fact.
#[derive(Debug, Clone)]
struct ObservedCall {
    method: String,
    params: Value,
}

fn status_reply() -> Value {
    json!({
        "result": {
            "pid": 999,
            "uptime_ms": 5000,
            "capture": {
                "active": true,
                "connected": true,
                "serial": "emulator-5554",
                "boot_id": "boot-a",
                "storage_pressure_stopped": false,
                "video_frames": 300,
                "audio_frames": 10,
                "log_records": 5,
                "tap_records": 0,
                "discontinuities": 0,
                "static_hold_events": 0,
            },
            "ring_chunk_count": 2,
            "ring_total_bytes": 1024,
            "ring_pinned_chunk_count": 0,
            "active_viewports": 0,
            "active_save_jobs": 1,
            "recovery": {
                "removed_staging_dirs": 0,
                "reaped_leases": 0,
                "recovered_jobs": 0,
                "completed_purging_bundles": 0,
            },
        }
    })
}

fn config_reply(save_window_secs: u32) -> Value {
    json!({
        "result": {
            "config": {
                "config_version": 1,
                "save_window_secs": save_window_secs,
                "video_max_fps": 30,
            },
            "config_path": "/data/arb/config.json",
        }
    })
}

fn devices_reply() -> Value {
    json!({
        "result": {
            "devices": [
                { "serial": "emulator-5554", "state": "device", "model": "sdk_gphone64_arm64" }
            ]
        }
    })
}

fn jobs_reply(job_state: &str) -> Value {
    json!({
        "result": {
            "jobs": [
                {
                    "job_id": "job-1",
                    "state": job_state,
                    "destination_path": "/data/arb/bundles/job-1",
                    "pinned_chunk_ids": [],
                    "last_error": if job_state == "failed_retryable" { Some("disk full") } else { None },
                    "created_at_utc": "2026-01-01T00:00:00Z",
                    "updated_at_utc": "2026-01-01T00:00:01Z",
                }
            ]
        }
    })
}

fn bundles_reply() -> Value {
    json!({ "result": { "bundles": [] } })
}

fn hello_reply() -> Value {
    json!({ "result": { "protocol_version": 1 } })
}

/// Reads one JSON-RPC request line and replies with `reply`, recording the call via `on_call`.
/// Returns `false` if the peer closed the stream (clean EOF) before sending a request.
fn handle_one_request(
    reader: &mut BufReader<arb_os::ipc::LocalIpcStream>,
    writer: &mut arb_os::ipc::LocalIpcStream,
    reply: &Value,
    on_call: &mpsc::Sender<ObservedCall>,
) -> bool {
    let Some(line) = read_frame_line(reader, DEFAULT_MAX_IPC_LINE_BYTES).expect("mock read line")
    else {
        return false;
    };
    let request: Value = serde_json::from_str(&line).expect("mock parse request json");
    let method = request["method"]
        .as_str()
        .expect("request has method")
        .to_owned();
    let params = request["params"].clone();
    let _ = on_call.send(ObservedCall { method, params });

    let mut response = reply.clone();
    response["jsonrpc"] = json!("2.0");
    response["id"] = request["id"].clone();
    let text = serde_json::to_string(&response).expect("serialize mock reply");
    write_frame_line(writer, &text, DEFAULT_MAX_IPC_LINE_BYTES).expect("mock write reply");
    true
}

/// Serves one full poll cycle (`system.status`, `config.get`, `device.list`, `save.list_jobs`,
/// `bundle.list`, in that order).
fn serve_poll_cycle(
    reader: &mut BufReader<arb_os::ipc::LocalIpcStream>,
    writer: &mut arb_os::ipc::LocalIpcStream,
    save_window_secs: u32,
    job_state: &str,
    on_call: &mpsc::Sender<ObservedCall>,
) -> bool {
    handle_one_request(reader, writer, &status_reply(), on_call)
        && handle_one_request(reader, writer, &config_reply(save_window_secs), on_call)
        && handle_one_request(reader, writer, &devices_reply(), on_call)
        && handle_one_request(reader, writer, &jobs_reply(job_state), on_call)
        && handle_one_request(reader, writer, &bundles_reply(), on_call)
}

/// Runs the full mock `arbd` server lifecycle:
/// - Connection 1: `system.hello`, 1 poll cycle, `save.trigger`, `save.screenshot`, `save.logs`,
///   `save.retry`, explicit `writer.drain()`, deterministic drop + `conn1_dropped_tx` barrier.
/// - Connection 2: `system.hello`, 1 poll cycle (with `job-1` completed), explicit `writer.drain()`,
///   then wait for `client_done_rx` before dropping the pipe handle so Windows `DisconnectNamedPipe`
///   can never race an unread final reply!
fn run_mock_arbd_server(
    listener: &LocalIpcListener,
    call_tx: &mpsc::Sender<ObservedCall>,
    conn1_dropped_tx: &mpsc::Sender<()>,
    client_done_rx: &mpsc::Receiver<()>,
) {
    // --- Connection 1 ---
    let stream = listener.accept().expect("accept first client connection");
    let mut writer = stream.try_clone().expect("clone server stream for writing");
    let mut reader = BufReader::new(stream);

    assert!(
        handle_one_request(&mut reader, &mut writer, &hello_reply(), call_tx),
        "client must send system.hello at connect time"
    );
    assert!(
        serve_poll_cycle(&mut reader, &mut writer, 180, "failed_retryable", call_tx),
        "client must complete the first poll cycle without disconnecting"
    );
    assert!(
        handle_one_request(
            &mut reader,
            &mut writer,
            &json!({ "result": { "job_id": "job-2", "state": "completed" } }),
            call_tx,
        ),
        "client must send save.trigger"
    );
    assert!(
        handle_one_request(
            &mut reader,
            &mut writer,
            &json!({ "result": { "path": "/data/arb/screenshots/s1.png", "width": 1080, "height": 2400 } }),
            call_tx,
        ),
        "client must send save.screenshot"
    );
    assert!(
        handle_one_request(
            &mut reader,
            &mut writer,
            &json!({ "result": { "bundle_id": "logs-1" } }),
            call_tx,
        ),
        "client must send save.logs"
    );
    assert!(
        handle_one_request(
            &mut reader,
            &mut writer,
            &json!({ "result": { "ok": true } }),
            call_tx,
        ),
        "client must send save.retry"
    );
    // Let the client consume the last reply first: on Windows, dropping the pipe runs
    // `DisconnectNamedPipe`, which discards unread bytes.
    writer.drain().expect("drain connection 1 server replies");
    // Simulate an unexpected socket drop: close both ends of connection 1 before signalling client.
    drop(reader);
    drop(writer);
    conn1_dropped_tx
        .send(())
        .expect("signal client that connection 1 is dropped");

    // --- Connection 2: deterministic reconnect + drain before teardown ---
    let stream = listener.accept().expect("accept reconnecting client");
    let mut writer = stream.try_clone().expect("clone reconnected server stream");
    let mut reader = BufReader::new(stream);
    assert!(
        handle_one_request(&mut reader, &mut writer, &hello_reply(), call_tx),
        "client must re-run system.hello on the reconnected transport"
    );
    assert!(
        serve_poll_cycle(&mut reader, &mut writer, 180, "completed", call_tx),
        "client must reconnect and complete a second poll cycle"
    );
    // Drain AND wait for the client to finish reading before dropping the Windows Named Pipe
    // server handle so `DisconnectNamedPipe` never discards the final `bundle.list` reply.
    writer.drain().expect("drain connection 2 server replies");
    client_done_rx
        .recv_timeout(SYNC_TIMEOUT)
        .expect("client must finish reading connection 2 replies");
}

#[test]
fn tray_controller_fetches_status_dispatches_save_screenshot_logs_retry_and_reconnects_after_drop()
{
    let tmp = tempfile::tempdir().expect("create temp runtime dir");
    let paths = ArbPaths {
        data_dir: tmp.path().join("data"),
        cache_dir: tmp.path().join("cache"),
        runtime_dir: tmp.path().join("run"),
    };
    let endpoint = paths.ipc_endpoint();
    let listener = LocalIpcListener::bind(&endpoint).expect("bind mock arbd listener");

    let (call_tx, call_rx) = mpsc::channel::<ObservedCall>();
    let (conn1_dropped_tx, conn1_dropped_rx) = mpsc::channel::<()>();
    let (client_done_tx, client_done_rx) = mpsc::channel::<()>();
    let server = thread::spawn(move || {
        run_mock_arbd_server(&listener, &call_tx, &conn1_dropped_tx, &client_done_rx);
    });

    let mut controller =
        TrayController::new(paths, AutoStartMode::Disabled, Duration::from_millis(5));

    // 1. First poll: connects, fetches status + config (180s), and surfaces FailedRetryable job.
    let (state, alerts) = controller.poll_once();
    assert!(state.daemon_online, "controller must connect to mock arbd");
    assert_eq!(
        state.capture,
        CaptureStatus::Capturing,
        "capture must read as Capturing"
    );
    assert_eq!(state.save_window_secs, 180, "must adopt config.get window");
    assert_eq!(state.buffered_secs, 10, "300 frames / 30 fps = 10s");
    assert_eq!(
        state.status_line(),
        "REC sdk_gphone64_arm64 (emulator-5554) · 10s / 180s",
        "status line must follow strict seconds notation"
    );
    let menu = build_menu(&state);
    assert!(
        menu.iter().any(|i| i.label == "直前 180s を保存"),
        "menu must reflect config.get save_window_secs in strict seconds notation"
    );
    assert!(
        menu.iter().all(|i| !contains_emoji(&i.label)),
        "menu must contain zero emoji"
    );
    assert_eq!(state.jobs.len(), 1, "one save job must be surfaced");
    assert!(
        state.jobs.iter().any(|job| job.job_id == "job-1"
            && job.state == arb_core::state::SaveJobState::FailedRetryable),
        "job-1 must be surfaced as FailedRetryable: {:?}",
        state.jobs
    );
    assert_eq!(
        alerts,
        Vec::<TrayAlert>::new(),
        "initial online poll must not spam alerts"
    );

    // 2. Dispatch SaveWindow, SaveScreenshot, SaveLogs, and RetryJob.
    controller
        .dispatch(&TrayMenuEvent::SaveWindow { duration_secs: 180 })
        .expect("save.trigger dispatch must succeed");
    controller
        .dispatch(&TrayMenuEvent::SaveScreenshot)
        .expect("save.screenshot dispatch must succeed");
    controller
        .dispatch(&TrayMenuEvent::SaveLogs { window_secs: 180 })
        .expect("save.logs dispatch must succeed");
    controller
        .dispatch(&TrayMenuEvent::RetryJob {
            job_id: "job-1".to_owned(),
        })
        .expect("save.retry dispatch must succeed");

    // Wait deterministically for the mock server to tear down connection 1 before probing it.
    conn1_dropped_rx
        .recv_timeout(SYNC_TIMEOUT)
        .expect("server must drop connection 1 promptly");

    // 3. Next poll observes the dropped connection cleanly -> DaemonOffline.
    let (offline_state, offline_alerts) = controller.poll_once();
    assert!(
        !offline_state.daemon_online,
        "poll after server-side drop must report offline, not stale data"
    );
    assert_eq!(
        offline_alerts,
        vec![TrayAlert::DaemonOffline],
        "dropping connection must emit exactly one DaemonOffline alert"
    );

    // 4. Following poll reconnects deterministically and observes job-1 completed.
    let (recovered_state, recovered_alerts) = controller.poll_once();
    client_done_tx
        .send(())
        .expect("signal server that connection 2 replies were consumed");
    assert_reconnected_with_completed_job(&recovered_state, &recovered_alerts);

    server
        .join()
        .expect("mock arbd server thread must not panic");

    // 5. Verify exact RPC sequence and parameter shapes.
    let observed: Vec<ObservedCall> = call_rx.try_iter().collect();
    assert_expected_rpc_call_sequence(&observed);
}

#[test]
fn tray_controller_reconnects_deterministically_across_repeated_drops() {
    const CYCLES: usize = 5;

    let tmp = tempfile::tempdir().expect("create temp runtime dir");
    let paths = ArbPaths {
        data_dir: tmp.path().join("data"),
        cache_dir: tmp.path().join("cache"),
        runtime_dir: tmp.path().join("run"),
    };
    let endpoint = paths.ipc_endpoint();
    let listener = LocalIpcListener::bind(&endpoint).expect("bind repeated-reconnect listener");

    let (call_tx, _call_rx) = mpsc::channel::<ObservedCall>();
    let (dropped_tx, dropped_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel::<()>();

    let server = thread::spawn(move || {
        for cycle in 0..CYCLES {
            let stream = listener.accept().expect("accept cycle connection");
            let mut writer = stream.try_clone().expect("clone server stream");
            let mut reader = BufReader::new(stream);
            assert!(
                handle_one_request(&mut reader, &mut writer, &hello_reply(), &call_tx),
                "cycle {cycle}: client must send system.hello"
            );
            assert!(
                serve_poll_cycle(&mut reader, &mut writer, 300, "completed", &call_tx),
                "cycle {cycle}: client must finish poll cycle"
            );
            writer.drain().expect("drain cycle replies");
            if cycle + 1 < CYCLES {
                drop(reader);
                drop(writer);
                dropped_tx.send(()).expect("signal drop");
            } else {
                done_rx
                    .recv_timeout(SYNC_TIMEOUT)
                    .expect("wait for final client poll");
            }
        }
    });

    let mut controller =
        TrayController::new(paths, AutoStartMode::Disabled, Duration::from_millis(5));

    for cycle in 0..CYCLES {
        let (state, _alerts) = controller.poll_once();
        assert!(
            state.daemon_online,
            "cycle {cycle}: controller must be online after connect/reconnect"
        );
        assert_eq!(
            state.capture,
            CaptureStatus::Capturing,
            "cycle {cycle}: capture must be Capturing"
        );
        if cycle + 1 < CYCLES {
            dropped_rx
                .recv_timeout(SYNC_TIMEOUT)
                .expect("wait for cycle server drop");
            let (offline, alerts) = controller.poll_once();
            assert!(!offline.daemon_online, "cycle {cycle}: must observe drop");
            assert_eq!(
                alerts,
                vec![TrayAlert::DaemonOffline],
                "cycle {cycle}: drop must emit DaemonOffline"
            );
        }
    }
    done_tx.send(()).expect("release server final connection");
    server.join().expect("server thread must join cleanly");
}

fn assert_reconnected_with_completed_job(
    recovered_state: &arb_tray::controller::TrayViewState,
    recovered_alerts: &[TrayAlert],
) {
    assert!(
        recovered_state.daemon_online,
        "controller must reconnect after the drop"
    );
    assert_eq!(
        recovered_state.capture,
        CaptureStatus::Capturing,
        "capture must still read as Capturing after reconnecting"
    );
    assert!(
        recovered_state
            .jobs
            .iter()
            .any(|job| job.job_id == "job-1"
                && job.state == arb_core::state::SaveJobState::Completed),
        "job-1 must now be observed as Completed: {:?}",
        recovered_state.jobs
    );
    assert_eq!(
        recovered_alerts,
        vec![
            TrayAlert::DaemonOnline,
            TrayAlert::CaptureStarted,
            TrayAlert::SaveCompleted {
                job_id: "job-1".to_owned(),
            }
        ],
        "reconnecting must report coming back online, capture resuming, and save completing"
    );
}

fn assert_expected_rpc_call_sequence(observed: &[ObservedCall]) {
    let methods: Vec<&str> = observed.iter().map(|call| call.method.as_str()).collect();
    assert_eq!(
        methods,
        vec![
            "system.hello",
            "system.status",
            "config.get",
            "device.list",
            "save.list_jobs",
            "bundle.list",
            "save.trigger",
            "save.screenshot",
            "save.logs",
            "save.retry",
            "system.hello",
            "system.status",
            "config.get",
            "device.list",
            "save.list_jobs",
            "bundle.list",
        ],
        "RPC call sequence must match the controller's handshake + poll + dispatch contract"
    );
    let trigger = observed
        .iter()
        .find(|call| call.method == "save.trigger")
        .expect("save.trigger must have been observed");
    assert_eq!(
        trigger.params,
        json!({ "duration_sec": 180 }),
        "SaveWindow 180s must send duration_sec: 180"
    );

    let screenshot = observed
        .iter()
        .find(|call| call.method == "save.screenshot")
        .expect("save.screenshot must have been observed");
    assert_eq!(
        screenshot.params,
        json!({}),
        "SaveScreenshot must send empty params object"
    );

    let logs = observed
        .iter()
        .find(|call| call.method == "save.logs")
        .expect("save.logs must have been observed");
    assert_eq!(
        logs.params,
        json!({ "window_secs": 180 }),
        "SaveLogs 180s must send window_secs: 180"
    );

    let retry = observed
        .iter()
        .find(|call| call.method == "save.retry")
        .expect("save.retry must have been observed");
    assert_eq!(
        retry.params,
        json!({ "job_id": "job-1" }),
        "RetryJob must send job_id: job-1"
    );
}
