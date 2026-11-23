//! Real IPC integration test: drives [`arb_tray::controller::TrayController`] over the actual
//! `arb_os::ipc::LocalIpcStream` transport against a hand-rolled mock JSON-RPC 2.0 server (no
//! dependency on the `arbd` crate, avoiding a circular workspace dependency; see `SPEC-01`).
//!
//! Proves end to end that the controller: fetches `system.status` / `device.list` /
//! `save.list_jobs` / `bundle.list` and derives the expected [`TrayViewState`], dispatches
//! `save.trigger` and `save.retry` RPCs for a `FailedRetryable` job, and reconnects cleanly after
//! the server drops the connection mid-session.

use std::io::BufReader;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use arb_os::ipc::{
    DEFAULT_MAX_IPC_LINE_BYTES, LocalIpcListener, read_frame_line, write_frame_line,
};
use arb_os::paths::ArbPaths;
use arb_tray::controller::{CaptureStatus, TrayAlert, TrayController, TrayMenuEvent};
use arb_tray::rpc::AutoStartMode;
use serde_json::{Value, json};

/// One JSON-RPC request observed by the mock server, recorded for assertions after the fact.
#[derive(Debug, Clone)]
struct ObservedCall {
    method: String,
    params: Value,
}

/// Builds the fixed `system.status` / `device.list` / `save.list_jobs` / `bundle.list` reply
/// payloads the mock server answers with for every poll cycle. `job_state` lets the test flip
/// the one tracked save job from `failed_retryable` to `completed` after a simulated retry.
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
                "video_frames": 10,
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

/// The `system.hello` handshake reply the controller now requires at connect time (S3):
/// a matching `protocol_version` is mandatory before any poll is attempted.
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

    let text = serde_json::to_string(reply).expect("serialize mock reply");
    write_frame_line(writer, &text, DEFAULT_MAX_IPC_LINE_BYTES).expect("mock write reply");
    true
}

/// Serves exactly one poll cycle (`system.status`, `device.list`, `save.list_jobs`,
/// `bundle.list`, in that order) over `reader`/`writer`. Returns `false` if the client closed
/// the connection partway through (used to detect the end of a connection's lifetime).
fn serve_poll_cycle(
    reader: &mut BufReader<arb_os::ipc::LocalIpcStream>,
    writer: &mut arb_os::ipc::LocalIpcStream,
    job_state: &str,
    on_call: &mpsc::Sender<ObservedCall>,
) -> bool {
    handle_one_request(reader, writer, &status_reply(), on_call)
        && handle_one_request(reader, writer, &devices_reply(), on_call)
        && handle_one_request(reader, writer, &jobs_reply(job_state), on_call)
        && handle_one_request(reader, writer, &bundles_reply(), on_call)
}

/// Runs the full mock `arbd` server lifecycle for this test: one poll cycle plus `save.trigger`
/// and `save.retry` on the first connection, a simulated socket drop, then a second connection
/// serving one more poll cycle (with the tracked job now `completed`) to prove reconnection.
fn run_mock_arbd_server(listener: &LocalIpcListener, call_tx: &mpsc::Sender<ObservedCall>) {
    // --- Connection 1: system.hello handshake, one poll cycle, then save.trigger +
    // save.retry, then drop. ---
    let stream = listener.accept().expect("accept first client connection");
    let mut writer = stream.try_clone().expect("clone server stream for writing");
    let mut reader = BufReader::new(stream);

    assert!(
        handle_one_request(&mut reader, &mut writer, &hello_reply(), call_tx),
        "client must send system.hello at connect time"
    );
    assert!(
        serve_poll_cycle(&mut reader, &mut writer, "failed_retryable", call_tx),
        "client must complete the first poll cycle without disconnecting"
    );
    assert!(
        handle_one_request(
            &mut reader,
            &mut writer,
            &json!({ "result": { "job_id": "job-2", "state": "pending" } }),
            call_tx,
        ),
        "client must send save.trigger"
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
    writer.drain().expect("drain server replies");
    // Simulate an unexpected socket drop: close both ends of connection 1 by dropping them.
    drop(reader);
    drop(writer);

    // --- Connection 2: the controller must reconnect, re-handshake, and complete
    // another poll cycle. ---
    let stream = listener.accept().expect("accept reconnecting client");
    let mut writer = stream.try_clone().expect("clone reconnected server stream");
    let mut reader = BufReader::new(stream);
    assert!(
        handle_one_request(&mut reader, &mut writer, &hello_reply(), call_tx),
        "client must re-run system.hello on the reconnected transport"
    );
    assert!(
        serve_poll_cycle(&mut reader, &mut writer, "completed", call_tx),
        "client must reconnect and complete a second poll cycle"
    );
}

#[test]
fn tray_controller_fetches_status_triggers_save_retries_job_and_reconnects_after_drop() {
    let tmp = tempfile::tempdir().expect("create temp runtime dir");
    let paths = ArbPaths {
        data_dir: tmp.path().join("data"),
        cache_dir: tmp.path().join("cache"),
        runtime_dir: tmp.path().join("run"),
    };
    let endpoint = paths.ipc_endpoint();
    let listener = LocalIpcListener::bind(&endpoint).expect("bind mock arbd listener");

    let (call_tx, call_rx) = mpsc::channel::<ObservedCall>();
    let server = thread::spawn(move || run_mock_arbd_server(&listener, &call_tx));

    let mut controller =
        TrayController::new(paths, AutoStartMode::Disabled, Duration::from_millis(5));

    // 1. First poll: connects, fetches status, and surfaces the FailedRetryable job.
    let (state, alerts) = controller.poll_once();
    assert!(
        state.daemon_online,
        "controller must connect to the mock arbd"
    );
    assert_eq!(
        state.capture,
        CaptureStatus::Capturing,
        "capture must read as Capturing (active + connected) from the status reply"
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
        "the very first observation must never spam alerts, even when already capturing with a \
         failed job"
    );

    // 2. Dispatch "Save Last 30s" and confirm the RPC call shape reaching the mock server.
    controller
        .dispatch(&TrayMenuEvent::SaveLast30)
        .expect("save.trigger dispatch must succeed");

    // 3. Dispatch Retry on the failed job and confirm it reaches the mock server too.
    controller
        .dispatch(&TrayMenuEvent::RetryJob {
            job_id: "job-1".to_owned(),
        })
        .expect("save.retry dispatch must succeed");

    // 4. Next poll happens against a connection the server already dropped: must surface as
    //    a clean, graceful "daemon offline" transition rather than panicking or hanging.
    let (offline_state, offline_alerts) = controller.poll_once();
    assert!(
        !offline_state.daemon_online,
        "poll after a server-side socket drop must report offline, not stale data"
    );
    assert_eq!(
        offline_alerts,
        vec![TrayAlert::DaemonOffline],
        "losing the connection must report exactly one DaemonOffline alert"
    );

    // 5. The following poll must reconnect cleanly and observe the retried job as completed.
    let (recovered_state, recovered_alerts) = controller.poll_once();
    assert_reconnected_with_completed_job(&recovered_state, &recovered_alerts);

    server
        .join()
        .expect("mock arbd server thread must not panic");

    // 6. Verify the exact RPC methods/params the controller sent to `arbd`, in order.
    let observed: Vec<ObservedCall> = call_rx.try_iter().collect();
    assert_expected_rpc_call_sequence(&observed);
}

/// Assertions for step 5: after reconnecting post-drop, the controller must be back online,
/// capturing, with `job-1` now observed as `Completed`, and must report exactly the three
/// transition alerts implied by going offline -> online while capturing resumes and a save
/// finishes.
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

/// Assertions for step 6: the mock server must have observed exactly the documented poll +
/// dispatch RPC method sequence, with `save.trigger`/`save.retry` carrying the right params.
fn assert_expected_rpc_call_sequence(observed: &[ObservedCall]) {
    let methods: Vec<&str> = observed.iter().map(|call| call.method.as_str()).collect();
    assert_eq!(
        methods,
        vec![
            "system.hello",
            "system.status",
            "device.list",
            "save.list_jobs",
            "bundle.list",
            "save.trigger",
            "save.retry",
            "system.hello",
            "system.status",
            "device.list",
            "save.list_jobs",
            "bundle.list",
        ],
        "RPC call sequence must match the controller's documented handshake + poll + \
         dispatch contract"
    );
    let trigger = observed
        .iter()
        .find(|call| call.method == "save.trigger")
        .expect("save.trigger must have been observed");
    assert_eq!(
        trigger.params,
        json!({ "duration_sec": 30 }),
        "Save Last 30s must dispatch save.trigger with duration_sec: 30"
    );
    let retry = observed
        .iter()
        .find(|call| call.method == "save.retry")
        .expect("save.retry must have been observed");
    assert_eq!(
        retry.params,
        json!({ "job_id": "job-1" }),
        "retrying job-1 must dispatch save.retry with job_id: job-1"
    );
}
