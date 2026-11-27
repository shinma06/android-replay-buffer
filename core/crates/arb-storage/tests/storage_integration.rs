//! Integration and fault-injection tests for `arb-storage` (Zones 1, 2, and 3).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::SystemTime;

use arb_core::bundle::{
    DeviceInfo, EpochSpan, FrameRecord, InputStreamMeta, LogCompleteness, LogRecord,
    LogcatStreamMeta, Provenance, RecordBucket, RequestedWindow, StreamStatus, StreamsSummary,
    TapRecord, UnanchoredSummary, VideoStreamMeta,
};
use arb_core::gesture::GestureKind;
use arb_core::legacy::{LegacySaveKind, LegacySchemaVersion};
use arb_core::state::{BundleLifecycleState, RingBudgetPolicy, SaveJobState};
use arb_core::time::{
    DeviceClockTriple, DiscontinuityEvent, DiscontinuityKind, PcSyncProbe, TimeAlignment,
    TimestampSemantics, UncertaintyReason,
};
use arb_os::OsError;
use arb_os::fs::{DiskSpaceInfo, is_pid_alive};
use arb_storage::StorageError;
use arb_storage::bundle_io::{
    BundleFormat, BundleReader, ClockAnchorRecord, LegacyBundleReader, MediaPayload, TimeRange,
    V2BundleDraft, V2BundleReader, V2BundleWriter, open_bundle,
};
use arb_storage::catalog::{
    Catalog, CatalogFormatVersion, EditSession, LeaseHolderKind, NewBundleEntry,
};
use arb_storage::ring::{DiskSpaceProvider, RingChunkKind, RingStore};
use tempfile::TempDir;

#[derive(Debug, Clone)]
struct FakeDiskProvider {
    available: Arc<AtomicU64>,
    total: u64,
}

impl FakeDiskProvider {
    fn new(available: u64, total: u64) -> Self {
        Self {
            available: Arc::new(AtomicU64::new(available)),
            total,
        }
    }

    fn set_available(&self, bytes: u64) {
        self.available.store(bytes, Ordering::SeqCst);
    }
}

impl DiskSpaceProvider for FakeDiskProvider {
    fn disk_space(&self, _path: &Path) -> Result<DiskSpaceInfo, OsError> {
        Ok(DiskSpaceInfo {
            available_bytes: self.available.load(Ordering::SeqCst),
            total_bytes: self.total,
        })
    }
}

fn sample_frames() -> Vec<FrameRecord> {
    vec![
        FrameRecord {
            frame_index: 0,
            boot_id: "boot-alpha".to_owned(),
            pts_device_mono_ns: 1_000_000,
            pc_ingest_cont_ns: 10_000_000,
            alignment: TimeAlignment::Anchored {
                boot_id: "boot-alpha".to_owned(),
                boottime_ns: 2_000_000,
                half_width_ns: 1_000,
                semantics: TimestampSemantics::VideoEncoderInputEstimate,
            },
            is_keyframe: true,
            gop_id: 0,
            mp4_sample_offset: 0,
            mp4_sample_size: 256,
        },
        FrameRecord {
            frame_index: 1,
            boot_id: "boot-alpha".to_owned(),
            pts_device_mono_ns: 1_033_333,
            pc_ingest_cont_ns: 10_033_333,
            alignment: TimeAlignment::Anchored {
                boot_id: "boot-alpha".to_owned(),
                boottime_ns: 2_033_333,
                half_width_ns: 1_000,
                semantics: TimestampSemantics::VideoEncoderInputEstimate,
            },
            is_keyframe: false,
            gop_id: 0,
            mp4_sample_offset: 256,
            mp4_sample_size: 128,
        },
    ]
}

fn sample_logs() -> Vec<LogRecord> {
    vec![
        LogRecord {
            seq: 0,
            boot_id: Some("boot-alpha".to_owned()),
            header_epoch_ns: Some(1_700_000_000_000_000),
            device_read_boottime_ns: Some(2_010_000),
            pc_ingest_cont_ns: 10_010_000,
            alignment: TimeAlignment::Anchored {
                boot_id: "boot-alpha".to_owned(),
                boottime_ns: 2_010_000,
                half_width_ns: 500,
                semantics: TimestampSemantics::LogcatHeaderWall,
            },
            bucket: RecordBucket::Main,
            level: Some("I".to_owned()),
            tag: Some("Activity".to_owned()),
            pid: Some(1001),
            tid: Some(1002),
            uid: None,
            package_name: None,
            message: "main timeline log".to_owned(),
        },
        LogRecord {
            seq: 1,
            boot_id: None,
            header_epoch_ns: None,
            device_read_boottime_ns: None,
            pc_ingest_cont_ns: 10_050_000,
            alignment: TimeAlignment::Unanchored {
                pc_ingest_cont_ns: 10_050_000,
                reason: UncertaintyReason::NoClockAnchor,
            },
            bucket: RecordBucket::Unanchored,
            level: Some("W".to_owned()),
            tag: Some("Sidecar".to_owned()),
            pid: Some(2001),
            tid: Some(2002),
            uid: None,
            package_name: None,
            message: "quarantined unanchored log".to_owned(),
        },
    ]
}

#[expect(
    clippy::too_many_lines,
    reason = "integration test bundle draft fixture constructs all fields explicitly"
)]
fn build_draft(bundle_id: &str, video_bytes: Vec<u8>) -> V2BundleDraft {
    V2BundleDraft {
        memo: None,
        bundle_kind: arb_core::bundle::BundleKind::Full,
        bundle_id: bundle_id.to_owned(),
        created_at_utc: "2026-11-22T12:00:00Z".to_owned(),
        provenance: Provenance::Original,
        device: DeviceInfo {
            serial: "emulator-5554".to_owned(),
            model: "Pixel 9".to_owned(),
            api_level: 35,
            is_emulator: true,
        },
        requested_window: RequestedWindow::RealElapsedSec {
            duration_sec: 30,
            trigger_pc_wall_utc: "2026-11-22T12:00:00Z".to_owned(),
        },
        epochs: vec![EpochSpan {
            boot_id: "boot-alpha".to_owned(),
            start_boottime_ns: 1_000_000,
            end_boottime_ns: 31_000_000,
        }],
        streams: StreamsSummary {
            video: Some(VideoStreamMeta {
                status: StreamStatus::Present,
                frame_count: 2,
                tail_may_be_incomplete: false,
                semantics: TimestampSemantics::VideoEncoderInputEstimate,
                display_width: None,
                display_height: None,
                carried_in: None,
            }),
            audio: None,
            logcat: Some(LogcatStreamMeta {
                dropped_count: None,
                status: StreamStatus::Present,
                completeness: LogCompleteness::Unverified,
                overflow_events: 0,
                tail_may_be_incomplete: false,
            }),
            input: Some(InputStreamMeta {
                status: StreamStatus::Present,
                tail_may_be_incomplete: false,
            }),
        },
        unanchored_summary: UnanchoredSummary {
            included_count: 1,
            truncated_count: 0,
        },
        video_mp4: Some(MediaPayload::Bytes(video_bytes)),
        audio_m4a: None,
        share_mp4: None,
        frames: sample_frames(),
        discontinuities: vec![DiscontinuityEvent {
            kind: DiscontinuityKind::DeviceSleep,
            stream: None,
            boot_id: Some("boot-alpha".to_owned()),
            start_boottime_ns: Some(3_000_000),
            end_boottime_ns: Some(5_000_000),
            pc_cont_start_ns: 11_000_000,
            pc_cont_end_ns: Some(13_000_000),
            delta_ns: Some(2_000_000),
            detail: Some("simulated device sleep".to_owned()),
        }],
        logs: sample_logs(),
        taps: vec![TapRecord {
            seq: 0,
            boot_id: Some("boot-alpha".to_owned()),
            event_mono_ns: Some(1_015_000),
            pc_ingest_cont_ns: 10_015_000,
            alignment: TimeAlignment::Anchored {
                boot_id: "boot-alpha".to_owned(),
                boottime_ns: 2_015_000,
                half_width_ns: 250,
                semantics: TimestampSemantics::InputEventTime,
            },
            bucket: RecordBucket::Main,
            action: "DOWN".to_owned(),
            x: Some(420.0),
            y: Some(810.5),
            pointer_id: Some(0),
            gesture_kind: GestureKind::Tap,
            end_x: None,
            end_y: None,
            end_t_offset_ns: None,
            path: Vec::new(),
        }],
        clock_anchors: vec![
            ClockAnchorRecord::DeviceTriple(DeviceClockTriple {
                boot_id: "boot-alpha".to_owned(),
                boottime_ns: 2_000_000,
                monotonic_ns: 1_000_000,
                realtime_ns: 1_700_000_000_000_000,
            }),
            ClockAnchorRecord::PcSyncProbe(PcSyncProbe {
                boot_id: "boot-alpha".to_owned(),
                pc_send_cont_ns: 10_000_000,
                pc_send_active_ns: 9_000_000,
                dev_boottime_ns: 2_000_000,
                dev_monotonic_ns: 1_000_000,
                pc_recv_cont_ns: 10_002_000,
                pc_recv_active_ns: 9_002_000,
            }),
        ],
    }
}

fn spawn_and_reap_dead_pid() -> u32 {
    #[cfg(unix)]
    let mut child = Command::new("/usr/bin/true")
        .spawn()
        .or_else(|_| Command::new("true").spawn())
        .expect("spawning short-lived child process must succeed");
    #[cfg(windows)]
    let mut child = Command::new("cmd")
        .args(["/C", "exit", "0"])
        .spawn()
        .expect("spawning short-lived child process must succeed");

    let pid = child.id();
    let status = child.wait().expect("waiting for child exit must succeed");
    assert!(status.success(), "child process must exit cleanly");
    assert!(
        !is_pid_alive(pid),
        "reaped child PID {pid} must report not alive"
    );
    pid
}

fn snapshot_directory_readonly(dir: &Path) -> BTreeMap<String, (u64, SystemTime)> {
    let mut map = BTreeMap::new();
    let dir_meta = fs::metadata(dir).expect("directory metadata must be readable");
    map.insert(
        ".".to_owned(),
        (
            dir_meta.len(),
            dir_meta.modified().expect("dir mtime must be readable"),
        ),
    );
    for entry in fs::read_dir(dir).expect("read_dir must succeed") {
        let entry = entry.expect("dir entry must succeed");
        let name = entry.file_name().to_string_lossy().into_owned();
        let meta = entry.metadata().expect("entry metadata must succeed");
        map.insert(
            name,
            (
                meta.len(),
                meta.modified().expect("file mtime must be readable"),
            ),
        );
    }
    map
}

fn repo_schema4_fixture_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/core/crates/arb-storage
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("fixtures")
        .join("legacy-schema4")
        .join("replay-trim-parity")
        .join("replay-20240101-000000-manual-feedface")
}

// ===========================================================================
// Test 1: Two-phase Save failure recovery (pin survival + retarget)
// ===========================================================================

fn churn_ring_and_assert_evictions(ring: &mut RingStore<FakeDiskProvider>) {
    let mut total_evicted = 0_usize;
    for i in 0..8_u32 {
        let (_desc, evicted) = ring
            .write_closed_chunk(
                format!("chunk-churn-{i}"),
                RingChunkKind::Video,
                10_000 + i64::from(i) * 1_000,
                500_000,
                &[0xAB_u8; 96],
            )
            .expect("churn chunk writes must succeed by evicting unpinned chunks");
        total_evicted += evicted.len();
    }
    assert!(
        total_evicted >= 4,
        "ring churn must have evicted multiple unpinned chunks, got {total_evicted}"
    );
}

fn setup_failed_save_job(
    ring: &mut RingStore<FakeDiskProvider>,
    catalog: &Catalog,
    tmp_root: &Path,
    pinned_video: &[u8],
    pinned_log: &[u8],
) -> Vec<String> {
    ring.write_closed_chunk(
        "chunk-pin-v1",
        RingChunkKind::Video,
        1_000,
        1_000_000,
        pinned_video,
    )
    .expect("writing chunk-pin-v1 must succeed");
    ring.write_closed_chunk(
        "chunk-pin-l1",
        RingChunkKind::Logcat,
        2_000,
        1_000_000,
        pinned_log,
    )
    .expect("writing chunk-pin-l1 must succeed");

    let blocking_file = tmp_root.join("not-a-directory");
    fs::write(&blocking_file, b"blocker").expect("creating blocking file must succeed");
    let bad_dest = blocking_file.join("bundle-failed");

    let pinned_ids = vec!["chunk-pin-v1".to_owned(), "chunk-pin-l1".to_owned()];
    ring.pin_chunks(
        "save-job-42",
        &bad_dest,
        pinned_ids.clone(),
        "2026-11-22T12:00:00Z",
    )
    .expect("Phase-1 pin journal write must succeed");
    catalog
        .create_pinned_job(
            "save-job-42",
            &bad_dest,
            &pinned_ids,
            "2026-11-22T12:00:00Z",
        )
        .expect("catalog create_pinned_job must succeed");
    catalog
        .mark_draining("save-job-42", "2026-11-22T12:00:01Z")
        .expect("mark_draining must succeed");
    catalog
        .mark_materializing("save-job-42", "2026-11-22T12:00:02Z")
        .expect("mark_materializing must succeed");

    let draft = build_draft("bundle-retargeted-42", pinned_video.to_vec());
    let fail_err = V2BundleWriter::write(&bad_dest, "save-job-42", &draft)
        .expect_err("writing under a non-directory path must fail");
    assert!(
        !bad_dest.exists(),
        "failed write must never create final destination"
    );
    assert!(
        !tmp_root.join(".staging-save-job-42").exists(),
        "failed write must leave no staging directory behind"
    );

    let failed = catalog
        .mark_failed_retryable("save-job-42", &fail_err.to_string(), "2026-11-22T12:00:03Z")
        .expect("mark_failed_retryable must succeed");
    assert_eq!(
        failed.job.state,
        SaveJobState::FailedRetryable,
        "must be FailedRetryable"
    );
    assert_eq!(
        failed.job.pinned_chunk_ids, pinned_ids,
        "pins must stay intact"
    );
    pinned_ids
}

#[test]
fn fault_two_phase_save_failure_preserves_pins_and_succeeds_after_retarget() {
    let tmp = TempDir::new().expect("temp dir must be created");
    let data_dir = tmp.path().join("data");
    let managed_root = data_dir.join("bundles");
    let catalog = Catalog::open(data_dir.join("catalog.sqlite"), &managed_root)
        .expect("catalog open must succeed");

    let disk = FakeDiskProvider::new(100_000_000, 200_000_000);
    let policy = RingBudgetPolicy {
        max_ring_bytes: 512,
        max_ring_duration_ns: 60_000_000_000,
        min_free_disk_bytes: 1_000_000,
    };
    let mut ring = RingStore::open_with_disk_provider(&data_dir, policy, disk)
        .expect("ring store open must succeed");

    let pinned_video = b"PINNED_VIDEO_PAYLOAD_BYTES_0123456789".repeat(3);
    let pinned_log = b"PINNED_LOG_PAYLOAD_BYTES_ABCDEF".repeat(2);
    let _pinned_ids =
        setup_failed_save_job(&mut ring, &catalog, tmp.path(), &pinned_video, &pinned_log);

    churn_ring_and_assert_evictions(&mut ring);
    assert_eq!(
        ring.read_chunk("chunk-pin-v1")
            .expect("pinned video chunk must survive eviction"),
        pinned_video,
        "pinned video chunk must remain intact"
    );
    assert_eq!(
        ring.read_chunk("chunk-pin-l1")
            .expect("pinned log chunk must survive eviction"),
        pinned_log,
        "pinned log chunk must remain intact"
    );

    let good_dest = managed_root.join("bundle-retargeted-42");
    let retargeted = catalog
        .retarget_job("save-job-42", &good_dest, "2026-11-22T12:01:00Z")
        .expect("retarget_job must succeed");
    assert_eq!(
        retargeted.job.state,
        SaveJobState::Materializing,
        "must enter Materializing"
    );
    ring.pin_journal()
        .update_destination("save-job-42", &good_dest)
        .expect("pin journal destination update must succeed");

    let recovered_bytes = ring
        .read_chunk("chunk-pin-v1")
        .expect("pinned chunk must remain readable at retarget time");
    let manifest = V2BundleWriter::write(
        &good_dest,
        "save-job-42",
        &build_draft("bundle-retargeted-42", recovered_bytes.clone()),
    )
    .expect("retargeted v2 bundle write must succeed");
    assert_eq!(
        manifest.bundle_id, "bundle-retargeted-42",
        "manifest bundle_id must match"
    );

    let completed = catalog
        .complete_job("save-job-42", "2026-11-22T12:01:05Z")
        .expect("complete_job must succeed");
    assert_eq!(
        completed.job.state,
        SaveJobState::Completed,
        "job must be Completed"
    );
    assert!(
        !completed.job.has_pinned_chunks(),
        "completed job must clear pins"
    );
    assert!(
        ring.unpin_job("save-job-42")
            .expect("unpin_job must succeed"),
        "pin journal removed"
    );

    let reader = V2BundleReader::open(&good_dest).expect("retargeted bundle must open read-only");
    reader
        .verify_integrity()
        .expect("retargeted bundle must pass SHA-256 verification");
    let actual_video =
        fs::read(reader.video_path().expect("video.mp4 must exist")).expect("read video.mp4");
    assert_eq!(
        actual_video, recovered_bytes,
        "video.mp4 must match pinned chunk payload"
    );
}

// ===========================================================================
// Test 2: Concurrent edit / trash / restore / GC safety (+ external/legacy guard)
// ===========================================================================

fn register_v2_bundle(catalog: &Catalog, bundle_id: &str, dir: &Path, job_id: &str, data: &[u8]) {
    V2BundleWriter::write(dir, job_id, &build_draft(bundle_id, data.to_vec()))
        .expect("v2 bundle write must succeed");
    catalog
        .register_bundle(&NewBundleEntry {
            bundle_id: bundle_id.to_owned(),
            path: dir.to_path_buf(),
            format_version: CatalogFormatVersion::V2,
            parent_bundle_id: None,
            created_at_utc: "2026-11-22T12:00:00Z".to_owned(),
        })
        .expect("register_bundle must succeed");
}

fn assert_edit_session_and_leased_trash(catalog: &Catalog, managed_dir: &Path) -> String {
    let edit = EditSession {
        in_time_ns: None,
        out_time_ns: None,
        session_id: "edit-1".to_owned(),
        bundle_id: "bundle-managed-1".to_owned(),
        in_frame_index: Some(0),
        out_frame_index: Some(1),
        playhead_frame_index: Some(1),
        av_offset_override_ms: Some(-95),
        updated_at_utc: "2026-11-22T12:00:10Z".to_owned(),
    };
    catalog
        .upsert_edit_session(&edit)
        .expect("upsert_edit_session must succeed");
    assert_eq!(
        catalog
            .get_edit_session("edit-1")
            .expect("get_edit_session must succeed"),
        Some(edit),
        "edit session must persist outside immutable bundle"
    );

    let lease_id = catalog
        .acquire_lease(
            "bundle-managed-1",
            LeaseHolderKind::IdeViewer,
            std::process::id(),
            "2026-11-22T12:01:00Z",
        )
        .expect("acquire viewer lease must succeed");

    let trashed = catalog
        .trash_bundle("bundle-managed-1", "2026-11-22T12:02:00Z")
        .expect("trash_bundle must succeed");
    assert_eq!(
        trashed.state,
        BundleLifecycleState::Trashed,
        "bundle must be Trashed"
    );
    V2BundleReader::open(managed_dir)
        .expect("trashed bundle must remain readable while leased")
        .verify_integrity()
        .expect("trashed bundle files must stay untouched");
    lease_id
}

#[test]
fn fault_concurrent_edit_trash_restore_and_lease_guarded_gc_safety() {
    let tmp = TempDir::new().expect("temp dir must be created");
    let managed_root = tmp.path().join("managed-bundles");
    let external_root = tmp.path().join("external-bundles");
    fs::create_dir_all(&external_root).expect("external root must be created");
    let catalog = Catalog::open(tmp.path().join("catalog.sqlite"), &managed_root)
        .expect("catalog open must succeed");

    let managed_dir = managed_root.join("bundle-managed-1");
    let external_dir = external_root.join("bundle-external-1");
    register_v2_bundle(
        &catalog,
        "bundle-managed-1",
        &managed_dir,
        "job-m1",
        b"managed-video",
    );
    register_v2_bundle(
        &catalog,
        "bundle-external-1",
        &external_dir,
        "job-e1",
        b"external-video",
    );

    let lease_id = assert_edit_session_and_leased_trash(&catalog, &managed_dir);
    catalog
        .trash_bundle("bundle-external-1", "2026-11-22T12:02:00Z")
        .expect("trash external bundle must succeed");

    catalog
        .renew_lease(&lease_id, "2026-11-22T12:59:00Z")
        .unwrap();
    let gc1 = catalog
        .gc_with_grace("2026-11-22T13:00:00Z", 0)
        .expect("gc_trashed_bundles must succeed");
    assert!(
        gc1.purged_bundle_ids.is_empty(),
        "leased and unmanaged bundles must not be purged"
    );
    assert_eq!(
        gc1.skipped_leased_bundle_ids,
        vec!["bundle-managed-1".to_owned()],
        "leased skipped"
    );
    assert_eq!(
        gc1.skipped_unmanaged_or_legacy_bundle_ids,
        vec!["bundle-external-1".to_owned()],
        "unmanaged external bundle skipped"
    );
    assert!(
        managed_dir.is_dir() && external_dir.is_dir(),
        "both dirs must stay on disk"
    );

    let restored = catalog
        .restore_bundle("bundle-managed-1")
        .expect("restore must succeed");
    assert_eq!(
        restored.state,
        BundleLifecycleState::Active,
        "must restore to Active"
    );
    assert!(
        restored.trashed_at_utc.is_none(),
        "trashed_at_utc must be cleared"
    );

    catalog
        .trash_bundle("bundle-managed-1", "2026-11-22T12:05:00Z")
        .expect("re-trash must succeed");
    assert!(
        catalog
            .release_lease(&lease_id)
            .expect("release_lease must succeed"),
        "lease released"
    );

    let gc2 = catalog
        .gc_with_grace("2026-11-22T13:00:00Z", 0)
        .expect("gc after release must succeed");
    assert_eq!(
        gc2.purged_bundle_ids,
        vec!["bundle-managed-1".to_owned()],
        "managed purged"
    );
    assert!(
        !managed_dir.exists(),
        "purged managed dir must be deleted from disk"
    );
    assert!(
        catalog
            .bundle("bundle-managed-1")
            .expect("query bundle")
            .is_none(),
        "row removed"
    );
    assert!(
        external_dir.is_dir(),
        "external unmanaged bundle must NEVER be deleted"
    );
}

// ===========================================================================
// Test 3: Orphan lease & crash recovery
// ===========================================================================

fn seed_interrupted_jobs(
    catalog: &Catalog,
    managed_root: &Path,
) -> Vec<(&'static str, Vec<String>)> {
    let pins_a = vec!["c-a1".to_owned(), "c-a2".to_owned()];
    let pins_b = vec!["c-b1".to_owned()];
    let pins_c = vec!["c-c1".to_owned()];
    catalog
        .create_pinned_job(
            "job-interrupted-pinned",
            managed_root.join("out-a"),
            &pins_a,
            "2026-11-22T12:02:00Z",
        )
        .expect("creating pinned job must succeed");
    catalog
        .create_pinned_job(
            "job-interrupted-draining",
            managed_root.join("out-b"),
            &pins_b,
            "2026-11-22T12:02:01Z",
        )
        .expect("creating draining job must succeed");
    catalog
        .mark_draining("job-interrupted-draining", "2026-11-22T12:02:02Z")
        .expect("marking draining must succeed");
    catalog
        .create_pinned_job(
            "job-interrupted-materializing",
            managed_root.join("out-c"),
            &pins_c,
            "2026-11-22T12:02:03Z",
        )
        .expect("creating materializing job must succeed");
    catalog
        .mark_draining("job-interrupted-materializing", "2026-11-22T12:02:04Z")
        .expect("marking draining must succeed");
    catalog
        .mark_materializing("job-interrupted-materializing", "2026-11-22T12:02:05Z")
        .expect("marking materializing must succeed");
    vec![
        ("job-interrupted-pinned", pins_a),
        ("job-interrupted-draining", pins_b),
        ("job-interrupted-materializing", pins_c),
    ]
}

#[test]
fn external_staging_recovery_requires_a_matching_owner_and_no_completion_marker() {
    let tmp = TempDir::new().unwrap();
    let managed = tmp.path().join("bundles");
    let catalog = Catalog::open(tmp.path().join("cat"), &managed).unwrap();
    let external = tmp.path().join("external");
    for (id, owner, finished) in [
        ("owned", "owned", false),
        ("other", "wrong", false),
        ("finished", "finished", true),
    ] {
        catalog
            .create_pinned_job(id, external.join(id), &[], "2026-01-01T00:00:00Z")
            .unwrap();
        let stage = external.join(format!(".staging-{id}"));
        fs::create_dir_all(&stage).unwrap();
        fs::write(stage.join(".arb-staging-owner"), owner).unwrap();
        if finished {
            fs::write(stage.join("manifest.json"), b"{}").unwrap();
        }
    }
    let trim_stage = external.join(".staging-trim");
    fs::create_dir_all(&trim_stage).unwrap();
    fs::write(trim_stage.join(".arb-staging-owner"), "trim").unwrap();
    catalog
        .track_staging("trim", &external.join("export"))
        .unwrap();
    let report = catalog.recover_on_startup("2026-01-01T00:00:01Z").unwrap();
    assert_eq!(report.removed_staging_dirs.len(), 2);
    assert!(
        report
            .removed_staging_dirs
            .contains(&external.join(".staging-owned"))
    );
    assert!(report.removed_staging_dirs.contains(&trim_stage));
    assert!(!trim_stage.exists());
    assert!(external.join(".staging-other").is_dir());
    assert!(external.join(".staging-finished/manifest.json").is_file());
}

#[test]
fn p1_unmarked_staging_survives_recovery() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("bundles");
    let catalog = Catalog::open(tmp.path().join("db"), &root).unwrap();
    let stage = root.join(".staging-family");
    fs::create_dir_all(&stage).unwrap();
    fs::write(stage.join("keep"), b"mine").unwrap();
    catalog.recover_on_startup("2026-11-22T12:00:00Z").unwrap();
    assert_eq!(fs::read(stage.join("keep")).unwrap(), b"mine");
}

#[test]
fn p1_audit_external_staging_cleaned_even_when_managed_root_missing() {
    let tmp = TempDir::new().unwrap();
    let managed = tmp.path().join("bundles");
    let external = tmp.path().join("external");
    let catalog = Catalog::open(tmp.path().join("cat"), &managed).unwrap();
    catalog
        .create_pinned_job("ext-job", external.join("b1"), &[], "2026-01-01T00:00:00Z")
        .unwrap();
    let stage = external.join(".staging-ext-job");
    fs::create_dir_all(&stage).unwrap();
    fs::write(stage.join(".arb-staging-owner"), b"ext-job").unwrap();
    let tracked = external.join(".staging-ext-trim");
    fs::create_dir_all(&tracked).unwrap();
    fs::write(tracked.join(".arb-staging-owner"), b"ext-trim").unwrap();
    catalog
        .track_staging("ext-trim", &external.join("b2"))
        .unwrap();

    // Remove managed root before recovery: external staging entries must still be swept.
    fs::remove_dir_all(&managed).unwrap();
    let report = catalog.recover_on_startup("2026-01-01T00:00:01Z").unwrap();
    assert!(!stage.exists(), "external job staging must be removed");
    assert!(
        !tracked.exists(),
        "external tracked staging must be removed"
    );
    assert_eq!(report.removed_staging_dirs.len(), 2);
}

#[test]
fn fault_orphan_lease_staging_and_interrupted_jobs_recover_on_startup() {
    let tmp = TempDir::new().expect("temp dir must be created");
    let managed_root = tmp.path().join("bundles");
    let catalog = Catalog::open(tmp.path().join("catalog.sqlite"), &managed_root)
        .expect("catalog open must succeed");

    let good_bundle = managed_root.join("bundle-survives");
    register_v2_bundle(
        &catalog,
        "bundle-survives",
        &good_bundle,
        "job-good",
        b"keep-me",
    );

    let orphan_staging = managed_root.join(".staging-crash");
    fs::create_dir_all(&orphan_staging).expect("creating orphan staging dir must succeed");
    fs::write(orphan_staging.join(".arb-staging-owner"), b"crash").unwrap();
    fs::write(orphan_staging.join("partial.bin"), b"half-written")
        .expect("writing orphan staging file must succeed");

    let dead_pid = spawn_and_reap_dead_pid();
    let dead_lease_id = catalog
        .acquire_lease(
            "bundle-survives",
            LeaseHolderKind::IdeViewer,
            dead_pid,
            "2026-11-22T12:01:00Z",
        )
        .expect("inserting dead-PID lease must succeed");
    let live_lease_id = catalog
        .acquire_lease(
            "bundle-survives",
            LeaseHolderKind::Cli,
            std::process::id(),
            "2026-11-22T12:01:05Z",
        )
        .expect("inserting live-PID lease must succeed");

    let expected_jobs = seed_interrupted_jobs(&catalog, &managed_root);

    let report = catalog
        .recover_on_startup("2026-11-22T12:10:00Z")
        .expect("recover_on_startup must succeed");

    assert_eq!(
        report.removed_staging_dirs.len(),
        1,
        "orphan .staging-crash must be removed"
    );
    assert!(
        !orphan_staging.exists(),
        ".staging-crash must no longer exist on disk"
    );
    assert!(
        good_bundle.is_dir(),
        "committed bundle must remain untouched"
    );
    assert_eq!(
        report.reaped_lease_ids,
        vec![dead_lease_id],
        "dead-PID lease must be reaped"
    );

    let remaining_leases = catalog
        .active_leases_for_bundle("bundle-survives")
        .expect("active_leases_for_bundle must succeed");
    assert_eq!(
        remaining_leases.len(),
        1,
        "live-PID lease must survive recovery"
    );
    assert_eq!(
        remaining_leases[0].lease_id, live_lease_id,
        "surviving lease must be live PID"
    );
    assert_eq!(
        report.recovered_job_ids.len(),
        3,
        "all 3 interrupted jobs must recover"
    );

    for (job_id, expected_pins) in expected_jobs {
        let job = catalog
            .save_job(job_id)
            .expect("save_job query must succeed")
            .expect("recovered job must exist");
        assert_eq!(
            job.job.state,
            SaveJobState::FailedRetryable,
            "{job_id} -> FailedRetryable"
        );
        assert_eq!(
            job.job.pinned_chunk_ids, expected_pins,
            "{job_id} must preserve pins"
        );
    }
}

// ===========================================================================
// Test 4: Disk pressure protection
// ===========================================================================

#[test]
fn fault_disk_pressure_stops_new_ring_writes_while_protecting_pins_and_saved_bundles() {
    let tmp = TempDir::new().expect("temp dir must be created");
    let data_dir = tmp.path().join("data");
    let bundle_dir = data_dir.join("bundles").join("saved-bundle-1");

    // Existing committed v2 bundle on disk.
    let saved_draft = build_draft("saved-bundle-1", b"precious-saved-video".to_vec());
    V2BundleWriter::write(&bundle_dir, "job-saved-1", &saved_draft)
        .expect("initial saved bundle write must succeed");

    // Live ring store with 3 GiB safety floor and deterministic disk provider.
    let disk = FakeDiskProvider::new(10 * 1024 * 1024 * 1024, 50 * 1024 * 1024 * 1024);
    let policy = RingBudgetPolicy::default();
    let mut ring = RingStore::open_with_disk_provider(&data_dir, policy, disk.clone())
        .expect("ring store open must succeed");

    let pinned_bytes = b"PINNED_RING_DATA_NEVER_EVICT".to_vec();
    let unpinned_bytes = b"UNPINNED_RING_DATA_ALSO_PRESERVED_ON_PRESSURE".to_vec();
    ring.write_closed_chunk("pin-1", RingChunkKind::Video, 100, 1_000, &pinned_bytes)
        .expect("writing pin-1 must succeed");
    ring.write_closed_chunk(
        "unpin-1",
        RingChunkKind::ClockMetadata,
        200,
        1_000,
        &unpinned_bytes,
    )
    .expect("writing unpin-1 must succeed");
    ring.pin_chunks(
        "job-active-pin",
        data_dir.join("bundles").join("future-bundle"),
        vec!["pin-1".to_owned()],
        "2026-11-22T12:00:00Z",
    )
    .expect("pinning pin-1 must succeed");

    // Drop available disk space below 3 GiB floor (2 GiB available).
    disk.set_available(2 * 1024 * 1024 * 1024);

    let err = ring
        .write_closed_chunk(
            "rejected-chunk",
            RingChunkKind::Video,
            300,
            1_000,
            b"new-capture-bytes",
        )
        .expect_err("disk space below min_free_disk_bytes must stop capture");
    match err {
        StorageError::StoragePressureStop {
            host_free_bytes,
            min_free_disk_bytes,
            pinned_bytes: reported_pinned,
            event,
        } => {
            assert_eq!(
                host_free_bytes,
                2 * 1024 * 1024 * 1024,
                "reported free bytes must match simulated value"
            );
            assert_eq!(
                min_free_disk_bytes, policy.min_free_disk_bytes,
                "reported floor must match policy"
            );
            assert_eq!(
                reported_pinned,
                u64::try_from(pinned_bytes.len()).expect("len fits u64"),
                "reported pinned bytes must match pinned chunk"
            );
            assert_eq!(
                event.kind,
                DiscontinuityKind::StoragePressureStop,
                "discontinuity event must be StoragePressureStop"
            );
        }
        other => panic!("expected StoragePressureStop, got {other:?}"),
    }

    assert!(
        ring.chunk_path("rejected-chunk").is_none(),
        "rejected chunk must never be written to disk"
    );
    assert_eq!(
        ring.read_chunk("pin-1")
            .expect("pinned chunk must remain intact under disk pressure"),
        pinned_bytes,
        "pinned chunk bytes must remain 100% intact"
    );
    assert_eq!(
        ring.read_chunk("unpin-1")
            .expect("unpinned chunk must not be evicted on storage pressure stop"),
        unpinned_bytes,
        "existing chunk bytes must remain intact"
    );

    let reader = V2BundleReader::open(&bundle_dir)
        .expect("existing saved bundle must remain readable under disk pressure");
    reader
        .verify_integrity()
        .expect("existing saved bundle SHA-256 must remain 100% intact");
}

// ===========================================================================
// Test 5: Multi-reader concurrency (4 threads, zero errors, zero write locks)
// ===========================================================================

#[test]
fn concurrent_four_thread_v2_bundle_readers_succeed_with_zero_write_locks() {
    let tmp = TempDir::new().expect("temp dir must be created");
    let bundle_dir = tmp.path().join("bundle-concurrent");
    let draft = build_draft("bundle-concurrent", b"concurrent-mp4-stream".to_vec());
    V2BundleWriter::write(&bundle_dir, "job-conc", &draft)
        .expect("writing concurrent test bundle must succeed");

    let shared_reader =
        Arc::new(V2BundleReader::open(&bundle_dir).expect("initial v2 open must succeed"));
    let mut handles = Vec::new();

    for worker_idx in 0..4_u32 {
        let dir = bundle_dir.clone();
        let shared = Arc::clone(&shared_reader);
        handles.push(thread::spawn(move || {
            let local_reader =
                V2BundleReader::open(&dir).expect("per-thread V2BundleReader::open must succeed");
            for _iter in 0..10_u32 {
                local_reader
                    .verify_integrity()
                    .expect("per-thread integrity check must pass");
                assert_eq!(
                    local_reader
                        .frames()
                        .expect("per-thread frames query must succeed")
                        .len(),
                    2,
                    "worker {worker_idx} must observe 2 frames"
                );
                assert_eq!(
                    shared
                        .query_logs(Some(RecordBucket::Main), TimeRange::all(), None)
                        .expect("shared main log query must succeed")
                        .len(),
                    1,
                    "worker {worker_idx} must observe 1 main log"
                );
                assert_eq!(
                    shared
                        .query_logs(Some(RecordBucket::Unanchored), TimeRange::all(), None)
                        .expect("shared unanchored log query must succeed")
                        .len(),
                    1,
                    "worker {worker_idx} must observe 1 unanchored log"
                );
                assert_eq!(
                    local_reader
                        .query_taps(None, TimeRange::all(), Some(10))
                        .expect("taps query must succeed")
                        .len(),
                    1,
                    "worker {worker_idx} must observe 1 tap"
                );
                assert_eq!(
                    local_reader
                        .discontinuities()
                        .expect("discontinuities query must succeed")
                        .len(),
                    1,
                    "worker {worker_idx} must observe 1 discontinuity"
                );
                assert_eq!(
                    local_reader
                        .clock_anchors()
                        .expect("clock_anchors query must succeed")
                        .len(),
                    2,
                    "worker {worker_idx} must observe 2 clock anchors"
                );
            }
        }));
    }

    for handle in handles {
        handle
            .join()
            .expect("concurrent reader thread must not panic");
    }

    // Verify no SQLite -wal or -shm sidecars were ever created inside the immutable bundle!
    assert!(
        !bundle_dir.join("timeline.sqlite-wal").exists()
            && !bundle_dir.join("timeline.sqlite-shm").exists(),
        "read-only v2 bundle must never gain -wal or -shm sidecar files"
    );
}

// ===========================================================================
// Test 6: Legacy Read-Only Compatibility (real Schema 4, synthetic Schema 3, log-only Schema 4)
// ===========================================================================

fn assert_real_schema4_unanchored_streams(reader: &LegacyBundleReader) {
    let all_logs = reader
        .query_logs(None, TimeRange::all(), None)
        .expect("all logs query must succeed");
    assert_eq!(all_logs.len(), 5, "real fixture has 5 device log rows");
    let main_logs = reader
        .query_logs(Some(RecordBucket::Main), TimeRange::all(), None)
        .expect("main logs query must succeed");
    assert_eq!(
        main_logs.len(),
        0,
        "without window_start_elapsed_ns origin, no device logs may claim Anchored boottime"
    );
    let unanchored_logs = reader
        .query_logs(Some(RecordBucket::Unanchored), TimeRange::all(), None)
        .expect("unanchored logs query must succeed");
    assert_eq!(
        unanchored_logs.len(),
        5,
        "all 5 device logs must degrade honestly to Unanchored when origin is null"
    );
    assert_eq!(
        reader.app_logs().len(),
        3,
        "real fixture has 3 app-filtered log rows"
    );

    let taps = reader
        .query_taps(None, TimeRange::all(), None)
        .expect("taps query must succeed");
    assert_eq!(taps.len(), 2, "real fixture has 2 touch rows");
    assert_eq!(taps[0].action, "DOWN", "first touch must be DOWN");
    assert_eq!(
        taps[0].bucket,
        RecordBucket::Unanchored,
        "touch without window_start_elapsed_ns origin must degrade to Unanchored"
    );
    assert_eq!(taps[1].action, "UP", "second touch must be UP");
    assert_eq!(
        taps[1].bucket,
        RecordBucket::Unanchored,
        "null-t_ns touch must be in Unanchored bucket"
    );
}

#[test]
fn legacy_reader_opens_real_schema4_fixture_strictly_read_only() {
    let fixture_dir = repo_schema4_fixture_dir();
    assert!(
        fixture_dir.is_dir(),
        "real schema 4 fixture must exist at {}",
        fixture_dir.display()
    );

    let before = snapshot_directory_readonly(&fixture_dir);

    let reader = LegacyBundleReader::open(&fixture_dir)
        .expect("LegacyBundleReader must open real schema 4 fixture");
    reader
        .verify_integrity()
        .expect("real schema 4 fixture must pass read-only SHA-256 verification");

    assert!(reader.is_legacy(), "must report is_legacy() == true");
    assert_eq!(
        reader.manifest_summary().format,
        BundleFormat::LegacySchema4,
        "format must be LegacySchema4"
    );
    assert_eq!(
        reader.manifest_summary().bundle_id,
        "fixture-source",
        "bundle_id must match fixture save_id"
    );
    assert_eq!(
        reader.video_path(),
        Some(fixture_dir.join("video.mp4")),
        "video_path must point to video.mp4"
    );
    assert!(reader.audio_path().is_none(), "legacy has no audio.m4a");

    let frames = reader.frames().expect("frames must decode");
    assert_eq!(frames.len(), 24, "real fixture has 24 video frames");
    assert!(frames[0].is_keyframe, "frame 0 must be keyframe");
    // Real repo fixture has `window_start_elapsed_ns: null`, so LegacyBundleReader
    // must degrade window-relative `t_ns` honestly to Unanchored rather than placing
    // window-relative offsets directly into DeviceBoottime.
    assert!(
        reader
            .legacy_metadata()
            .timebase
            .device_boottime_origin_ns()
            .is_none(),
        "real repo fixture has null window_start_elapsed_ns"
    );
    assert!(
        frames
            .iter()
            .all(|f| matches!(f.alignment, TimeAlignment::Unanchored { .. })),
        "frames without a DeviceBoottime origin must degrade honestly to Unanchored"
    );

    let discontinuities = reader
        .discontinuities()
        .expect("discontinuities must decode");
    assert_eq!(
        discontinuities.len(),
        1,
        "two-part fixture must report 1 gap discontinuity between segments"
    );
    assert!(
        discontinuities[0].start_boottime_ns.is_none()
            && discontinuities[0].end_boottime_ns.is_none(),
        "discontinuity without DeviceBoottime origin must not fabricate boottime bounds"
    );

    assert_real_schema4_unanchored_streams(&reader);

    // Also verify unified open_bundle() auto-detection works cleanly.
    let boxed = open_bundle(&fixture_dir).expect("open_bundle must auto-detect schema 4");
    assert!(boxed.is_legacy(), "boxed reader must report legacy");

    let after = snapshot_directory_readonly(&fixture_dir);
    assert_eq!(
        before, after,
        "real schema 4 fixture directory and files must remain 100% untouched"
    );
}

#[test]
fn legacy_reader_opens_synthetic_schema3_strictly_read_only() {
    let tmp = TempDir::new().expect("temp dir must be created");
    let s3_dir = tmp.path().join("replay-2026-11-10T02-49-18Z-uuid");
    fs::create_dir_all(&s3_dir).expect("create schema 3 dir must succeed");
    fs::write(s3_dir.join("video-001.mp4"), b"synthetic-s3-mp4")
        .expect("write video-001.mp4 must succeed");
    fs::write(
        s3_dir.join("session.json"),
        r#"{
            "schema": 3, "complete": true, "save_id": "s3-compat-01",
            "capture_mode": "VIDEO_AND_LOGS", "created_at": "2026-11-10T02:49:18Z",
            "replay_seconds": 30, "window_clock_known": true,
            "window_start_ns": "1000000000", "window_end_ns": "31000000000",
            "window_end_uncertainty_ns": "5000", "video_movie_duration_us": "2000000",
            "parts": [{"file": "video-001.mp4", "segment_index": 0, "generation": 1,
                "movie_start_us": "0", "movie_end_us": "2000000", "duration_us": "2000000",
                "window_start_ns": "0", "window_end_ns": "2000000000",
                "clock_alignment_known": true, "preroll_samples": 0}],
            "files_sha256": {}
        }"#,
    )
    .expect("write session.json must succeed");
    fs::write(
        s3_dir.join("frames.jsonl"),
        concat!(
            r#"{"part":"video-001.mp4","segment_index":0,"sample_index":0,"source_key":true,"output_key":true,"source_pts_us":"0","movie_pts_us":"0","movie_end_us":"100000","window_ns":"0","uncertainty_ns":"1000","clock_alignment_known":true,"presented":true,"preroll":false,"clock_epoch":1}"#,
            "\n",
            r#"{"part":"video-001.mp4","segment_index":0,"sample_index":1,"source_key":false,"output_key":false,"source_pts_us":"100000","movie_pts_us":"100000","movie_end_us":"200000","window_ns":"100000000","uncertainty_ns":"1000","clock_alignment_known":true,"presented":true,"preroll":false,"clock_epoch":1}"#,
            "\n"
        ),
    )
    .expect("write frames.jsonl must succeed");
    fs::write(
        s3_dir.join("logcat-device.jsonl"),
        concat!(
            r#"{"record_id":"1","clock_epoch":1,"epoch_ns":"1700000000000000000","elapsed_ns":"1000500000","window_ns":"500000","uncertainty_ns":"1000","pid":42,"tid":43,"priority":4,"tag":"App","message":"s3 anchored log"}"#,
            "\n",
            r#"{"record_id":"2","clock_epoch":2,"window_ns":null,"uncertainty_ns":"9223372036854775807","priority":6,"tag":"App","message":"s3 sentinel log"}"#,
            "\n"
        ),
    )
    .expect("write logcat-device.jsonl must succeed");

    let s3_before = snapshot_directory_readonly(&s3_dir);
    let s3_reader =
        LegacyBundleReader::open(&s3_dir).expect("LegacyBundleReader must open schema 3 fixture");
    assert_eq!(
        s3_reader.legacy_metadata().schema,
        LegacySchemaVersion::Schema3,
        "must be Schema3"
    );
    assert_eq!(
        s3_reader.manifest_summary().format,
        BundleFormat::LegacySchema3,
        "LegacySchema3"
    );
    assert_eq!(
        s3_reader.video_path(),
        Some(s3_dir.join("video-001.mp4")),
        "video-001.mp4"
    );
    assert_eq!(
        s3_reader.frames().expect("s3 frames").len(),
        2,
        "must decode 2 frames"
    );
    let s3_logs = s3_reader
        .query_logs(None, TimeRange::all(), None)
        .expect("s3 logs");
    assert_eq!(s3_logs.len(), 2, "schema 3 must decode 2 log rows");
    assert_eq!(
        s3_logs[0].level.as_deref(),
        Some("I"),
        "priority 4 maps to I"
    );
    assert!(
        s3_reader
            .legacy_metadata()
            .timebase
            .device_boottime_origin_ns()
            .is_none(),
        "schema 3 has no device_boottime origin"
    );
    assert_eq!(
        s3_logs[0].bucket,
        RecordBucket::Unanchored,
        "schema 3 sequence-relative log degrades honestly to Unanchored"
    );
    assert_eq!(
        s3_logs[1].bucket,
        RecordBucket::Unanchored,
        "sentinel maps to Unanchored"
    );
    assert_eq!(
        s3_before,
        snapshot_directory_readonly(&s3_dir),
        "schema 3 dir untouched"
    );
}

#[test]
fn legacy_reader_opens_synthetic_log_only_schema4_strictly_read_only() {
    let tmp = TempDir::new().expect("temp dir must be created");
    let logs_only_dir = tmp
        .path()
        .join("replay-20261122-120000-manual-logs-deadbeef");
    fs::create_dir_all(&logs_only_dir).expect("create log-only dir must succeed");
    fs::write(
        logs_only_dir.join("replay.json"),
        r#"{
            "schema": 4, "capture_mode": "LOGS_ONLY", "complete": true,
            "save_id": "s4-logs-only-01",
            "folder_name": "replay-20261122-120000-manual-logs-deadbeef",
            "trigger": "manual",
            "files": {"readme": "README.md", "record": "replay.json", "video": null,
                "video_frames": null, "video_source": null,
                "logcat_device": "logcat-device.jsonl", "logcat_app": null, "touches": null},
            "counts": {"device_log_rows": 2, "app_log_rows": 0, "video_frames": 0},
            "save_request": {"content": "LOGS_ONLY", "origin": "CLI", "video_excluded": true},
            "coverage": {"video": {"state": "EXCLUDED"}},
            "timebase": {"domain": "device_boottime", "known": true,
                "window_start_elapsed_ns": "1000000000",
                "window_duration_ns": "10000000000", "window_end_uncertainty_ns": "0"},
            "parts": [], "files_sha256": {}
        }"#,
    )
    .expect("write log-only replay.json must succeed");
    fs::write(
        logs_only_dir.join("logcat-device.jsonl"),
        concat!(
            r#"{"record_id":"10","t_ns":"100000","t_uncertainty_ns":"0","priority":5,"tag":"Sync","message":"log only entry 1"}"#,
            "\n",
            r#"{"record_id":"11","t_ns":"200000","t_uncertainty_ns":"0","priority":6,"tag":"Sync","message":"log only entry 2"}"#,
            "\n"
        ),
    )
    .expect("write log-only logcat-device.jsonl must succeed");

    let lo_before = snapshot_directory_readonly(&logs_only_dir);
    let lo_reader = LegacyBundleReader::open(&logs_only_dir)
        .expect("LegacyBundleReader must open log-only schema 4 fixture");
    assert_eq!(
        lo_reader.legacy_metadata().save_kind,
        LegacySaveKind::LogsOnly,
        "LogsOnly"
    );
    assert_eq!(
        lo_reader
            .legacy_metadata()
            .timebase
            .device_boottime_origin_ns(),
        Some(1_000_000_000),
        "schema 4 with window_start_elapsed_ns must expose exact DeviceBoottime origin"
    );
    assert!(
        lo_reader.video_path().is_none(),
        "log-only save must have no video_path"
    );
    assert!(
        lo_reader.frames().expect("log-only frames").is_empty(),
        "0 frames"
    );
    let lo_logs = lo_reader
        .query_logs(
            Some(RecordBucket::Main),
            TimeRange::between(1_000_150_000, 1_000_250_000),
            None,
        )
        .expect("ranged log query must succeed");
    assert_eq!(
        lo_logs.len(),
        1,
        "rebased range [1_000_150_000, 1_000_250_000] must match 1 entry"
    );
    assert_eq!(
        lo_logs[0].message, "log only entry 2",
        "must return entry 2"
    );
    assert!(
        matches!(
            lo_logs[0].alignment,
            TimeAlignment::Anchored {
                boottime_ns: 1_000_200_000,
                half_width_ns: 0,
                ..
            }
        ),
        "log entry 2 must rebase onto origin + t_ns = 1_000_200_000"
    );
    assert_eq!(
        lo_before,
        snapshot_directory_readonly(&logs_only_dir),
        "dir untouched"
    );
}

#[test]
fn storage_error_exposes_published_bundle_path_on_committed_parent_sync_failure() {
    let published = PathBuf::from("/tmp/example-published-worm-bundle");
    let err = StorageError::Os(OsError::CommittedParentSyncFailed {
        path: published.clone(),
        source: std::io::Error::other("simulated parent dir fsync failure"),
    });
    assert_eq!(
        err.committed_bundle_path_on_parent_sync_error(),
        Some(published.as_path()),
        "CommittedParentSyncFailed must surface the published WORM bundle path"
    );
    let other = StorageError::NotFound {
        entity: "bundle",
        id: "missing".to_owned(),
    };
    assert!(
        other.committed_bundle_path_on_parent_sync_error().is_none(),
        "non-commit errors must return None"
    );
}

#[test]
fn fault_mid_staging_failure_cleans_staging_and_ring_reopen_preserves_pins_and_duration_eviction() {
    let tmp = TempDir::new().expect("temp dir must be created");
    let data_dir = tmp.path().join("data");
    let managed_root = data_dir.join("bundles");

    // 1. Mid-staging I/O error (missing source media file) cleans up .staging-<id> cleanly.
    let mut bad_draft = build_draft("bundle-mid-fail", Vec::new());
    bad_draft.video_mp4 = Some(MediaPayload::File(tmp.path().join("does-not-exist.mp4")));
    let final_dir = managed_root.join("bundle-mid-fail");
    let err = V2BundleWriter::write(&final_dir, "mid-fail-job", &bad_draft)
        .expect_err("missing media source file must fail mid-staging");
    assert!(
        matches!(err, StorageError::Io { .. }),
        "expected StorageError::Io on missing media file, got {err:?}"
    );
    assert!(
        !final_dir.exists(),
        "failed mid-staging write must never create final bundle dir"
    );
    assert!(
        !managed_root.join(".staging-mid-fail-job").exists(),
        "failed mid-staging write must clean up .staging directory automatically"
    );

    // 2. RingStore reopen reloads closed chunks and durable PinJournal; duration budget evicts old unpinned chunks.
    let disk = FakeDiskProvider::new(100_000_000, 200_000_000);
    let policy = RingBudgetPolicy {
        max_ring_bytes: 100_000,
        max_ring_duration_ns: 2_000_000,
        min_free_disk_bytes: 1_000_000,
    };
    {
        let mut ring = RingStore::open_with_disk_provider(&data_dir, policy, disk.clone())
            .expect("ring open 1 must succeed");
        ring.write_closed_chunk("c-pin", RingChunkKind::Video, 100, 1_500_000, b"pinned")
            .expect("write c-pin must succeed");
        ring.write_closed_chunk("c-old", RingChunkKind::Audio, 200, 1_500_000, b"old-audio")
            .expect("write c-old must succeed");
        ring.pin_chunks(
            "job-persist",
            managed_root.join("out"),
            vec!["c-pin".to_owned()],
            "2026-11-22T12:00:00Z",
        )
        .expect("pinning c-pin must succeed");
    }

    let mut reopened = RingStore::open_with_disk_provider(&data_dir, policy, disk)
        .expect("ring reopen must succeed");
    assert!(
        reopened
            .pinned_chunk_ids()
            .expect("pinned_chunk_ids must succeed")
            .contains("c-pin"),
        "reopened ring store must retain durable pin for c-pin"
    );
    let (_desc, evicted) = reopened
        .write_closed_chunk("c-new", RingChunkKind::Input, 300, 1_000_000, b"new-input")
        .expect("duration-exceeding write must evict old unpinned chunk");
    assert_eq!(
        evicted,
        vec!["c-old".to_owned()],
        "duration budget must evict unpinned c-old while preserving pinned c-pin"
    );
    assert_eq!(
        reopened
            .read_chunk("c-pin")
            .expect("pinned c-pin must survive restart and duration eviction"),
        b"pinned",
        "c-pin bytes must stay intact"
    );
}

#[test]
fn catalog_gc_protects_legacy_ro_and_active_job_targets_and_supports_retry_job() {
    let tmp = TempDir::new().expect("temp dir must be created");
    let managed_root = tmp.path().join("bundles");
    let catalog = Catalog::open(tmp.path().join("catalog.sqlite"), &managed_root)
        .expect("catalog open must succeed");

    let legacy_dir = managed_root.join("legacy-inside-root");
    fs::create_dir_all(&legacy_dir).expect("create legacy dir must succeed");
    catalog
        .register_bundle(&NewBundleEntry {
            bundle_id: "legacy-1".to_owned(),
            path: legacy_dir.clone(),
            format_version: CatalogFormatVersion::LegacyRo,
            parent_bundle_id: None,
            created_at_utc: "2026-11-22T12:00:00Z".to_owned(),
        })
        .expect("register legacy bundle must succeed");
    catalog
        .trash_bundle("legacy-1", "2026-11-22T12:01:00Z")
        .expect("trash legacy bundle must succeed");

    let job_target = managed_root.join("bundle-job-locked");
    register_v2_bundle(&catalog, "bundle-job-locked", &job_target, "job-init", b"v");
    catalog
        .trash_bundle("bundle-job-locked", "2026-11-22T12:01:00Z")
        .expect("trash bundle-job-locked must succeed");

    let pins = vec!["c1".to_owned()];
    catalog
        .create_pinned_job("job-retry-1", &job_target, &pins, "2026-11-22T12:02:00Z")
        .expect("create_pinned_job must succeed");
    catalog
        .mark_failed_retryable("job-retry-1", "transient error", "2026-11-22T12:02:05Z")
        .expect("mark_failed_retryable must succeed");
    let retried = catalog
        .retry_job("job-retry-1", "2026-11-22T12:02:10Z")
        .expect("retry_job must succeed");
    assert_eq!(
        retried.job.state,
        SaveJobState::Materializing,
        "retry_job must transition FailedRetryable -> Materializing"
    );
    assert_eq!(
        retried.job.pinned_chunk_ids, pins,
        "retry_job must preserve pinned chunks"
    );

    let gc = catalog
        .gc_with_grace("2026-11-22T13:00:00Z", 0)
        .expect("gc_trashed_bundles must succeed");
    assert!(
        gc.purged_bundle_ids.is_empty(),
        "neither legacy_ro nor active-job-targeted bundle may be purged"
    );
    assert_eq!(
        gc.skipped_unmanaged_or_legacy_bundle_ids,
        vec!["legacy-1".to_owned()],
        "legacy_ro bundle must never be deleted by GC even inside managed root"
    );
    assert_eq!(
        gc.skipped_active_job_bundle_ids,
        vec!["bundle-job-locked".to_owned()],
        "bundle referenced by active save job must be skipped by GC"
    );
    assert!(
        legacy_dir.is_dir() && job_target.is_dir(),
        "both protected directories must remain untouched on disk"
    );
}

// ===========================================================================
// R2 review regressions (S13, S14, S15, S16, S19)
// ===========================================================================

type JsonEdit = Box<dyn Fn(&mut serde_json::Value)>;

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create copy dir");
    for entry in fs::read_dir(src).expect("read fixture dir") {
        let entry = entry.expect("fixture entry");
        if entry.file_type().expect("file type").is_file() {
            fs::copy(entry.path(), dst.join(entry.file_name())).expect("copy fixture file");
        }
    }
}

fn edit_json(path: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(path).expect("read json")).expect("parse json");
    edit(&mut value);
    fs::write(
        path,
        serde_json::to_vec_pretty(&value).expect("serialize json"),
    )
    .expect("write json");
}

#[test]
fn r2_s13_pin_journals_and_catalog_jobs_are_reconciled_on_startup() {
    let tmp = TempDir::new().expect("temp dir must be created");
    let data_dir = tmp.path().join("data");
    let disk = FakeDiskProvider::new(50 * 1024 * 1024 * 1024, 100 * 1024 * 1024 * 1024);
    let ring = {
        let mut ring =
            RingStore::open_with_disk_provider(&data_dir, RingBudgetPolicy::default(), disk)
                .expect("ring open");
        for (i, id) in ["ca", "cb", "cc", "cd"].iter().enumerate() {
            let t = i64::try_from(i).expect("index");
            ring.write_closed_chunk(*id, RingChunkKind::Video, t, 1, id.as_bytes())
                .expect("chunk write");
        }
        ring
    };
    let catalog = Catalog::open(data_dir.join("catalog.sqlite"), data_dir.join("bundles"))
        .expect("catalog open");
    let now = "2026-11-23T00:00:00Z";
    let dest = |name: &str| data_dir.join("bundles").join(name);

    // A: completed in the catalog, but the crash hit before unpin_job -> leaked journal.
    catalog
        .create_pinned_job("job-a", dest("a"), &["ca".to_owned()], now)
        .expect("create a");
    ring.pin_chunks("job-a", dest("a"), vec!["ca".to_owned()], now)
        .expect("pin a");
    catalog.mark_draining("job-a", now).expect("drain a");
    catalog.mark_materializing("job-a", now).expect("mat a");
    catalog.complete_job("job-a", now).expect("complete a");
    // B: journal whose job the catalog never recorded.
    ring.pin_chunks("job-b", dest("b"), vec!["cb".to_owned()], now)
        .expect("pin b");
    // C: materializing in the catalog, journal already removed before the crash.
    catalog
        .create_pinned_job("job-c", dest("c"), &["cc".to_owned()], now)
        .expect("create c");
    catalog.mark_draining("job-c", now).expect("drain c");
    catalog.mark_materializing("job-c", now).expect("mat c");
    // D: healthy active job with its journal.
    catalog
        .create_pinned_job("job-d", dest("d"), &["cd".to_owned()], now)
        .expect("create d");
    ring.pin_chunks("job-d", dest("d"), vec!["cd".to_owned()], now)
        .expect("pin d");

    let (recovery, pins) = catalog
        .recover_on_startup_with_pins(now, ring.pin_journal())
        .expect("startup recovery");
    assert!(
        recovery.recovered_job_ids.contains(&"job-c".to_owned()),
        "interrupted job C becomes FailedRetryable"
    );
    assert_eq!(
        pins.released_job_ids,
        vec!["job-a".to_owned(), "job-b".to_owned()],
        "journals of completed/unknown jobs are released"
    );
    assert_eq!(
        pins.repinned_job_ids,
        vec!["job-c".to_owned()],
        "retryable job missing its journal is re-pinned"
    );
    let pinned = ring.pinned_chunk_ids().expect("pinned ids");
    assert_eq!(
        pinned.into_iter().collect::<Vec<_>>(),
        vec!["cc".to_owned(), "cd".to_owned()],
        "exactly the chunks of live jobs stay pinned"
    );

    // Idempotent: a second reconciliation changes nothing.
    let again = catalog
        .reconcile_pins(ring.pin_journal(), now)
        .expect("second reconcile");
    assert!(
        again.released_job_ids.is_empty() && again.repinned_job_ids.is_empty(),
        "reconciliation is idempotent"
    );
}

#[test]
fn r2_s14_retry_after_post_rename_crash_is_idempotent_and_stale_staging_is_removed() {
    let tmp = TempDir::new().expect("temp dir must be created");
    let external = tmp.path().join("external");
    let final_dir = external.join("bundle-x");
    let draft = build_draft("bundle-x", b"video-x".to_vec());

    let first = V2BundleWriter::write(&final_dir, "job-x", &draft).expect("first write");
    // Crash after the commit rename: the retry must succeed with the same manifest.
    let retried = V2BundleWriter::write(&final_dir, "job-x", &draft)
        .expect("retry of an already committed bundle is idempotent");
    assert_eq!(
        retried, first,
        "idempotent retry returns the committed manifest"
    );

    let other = build_draft("bundle-other", b"video-x".to_vec());
    assert!(
        matches!(
            V2BundleWriter::write(&final_dir, "job-o", &other),
            Err(StorageError::Os(OsError::AlreadyExists { .. }))
        ),
        "a different bundle at final_dir is never adopted"
    );
    fs::write(final_dir.join("video.mp4"), b"tampered").expect("tamper");
    assert!(
        matches!(
            V2BundleWriter::write(&final_dir, "job-x", &draft),
            Err(StorageError::Os(OsError::AlreadyExists { .. }))
        ),
        "a damaged bundle is never adopted"
    );

    // Stale `.staging-<job>` under an external destination (not swept by recovery).
    let stale = external.join(".staging-job-y");
    fs::create_dir_all(&stale).expect("stale staging");
    fs::write(stale.join(".arb-staging-owner"), b"job-y").unwrap();
    fs::write(stale.join("partial.bin"), b"half").expect("stale file");
    V2BundleWriter::write(
        external.join("bundle-y"),
        "job-y",
        &build_draft("bundle-y", b"video-y".to_vec()),
    )
    .expect("write succeeds despite a stale staging dir");
    assert!(!stale.exists(), "stale staging dir removed");
}

#[test]
fn r2_s15_v2_integrity_requires_timeline_and_present_media_in_manifest() {
    let tmp = TempDir::new().expect("temp dir must be created");
    let make = |name: &str, drop_file: &str| {
        let dir = tmp.path().join(name);
        V2BundleWriter::write(
            &dir,
            &format!("job-{name}"),
            &build_draft(name, b"v".to_vec()),
        )
        .expect("write");
        edit_json(&dir.join("manifest.json"), |m| {
            m["files"]
                .as_array_mut()
                .expect("files array")
                .retain(|f| f["path"] != drop_file);
        });
        V2BundleReader::open(&dir).expect("manifest still schema-valid")
    };

    let no_timeline = make("b-no-timeline", "timeline.sqlite");
    assert!(
        matches!(
            no_timeline.verify_integrity(),
            Err(StorageError::IntegrityMismatch { ref path, .. }) if path == "timeline.sqlite"
        ),
        "timeline.sqlite must be listed"
    );

    let no_video = make("b-no-video", "video.mp4");
    assert!(
        matches!(
            no_video.verify_integrity(),
            Err(StorageError::IntegrityMismatch { ref path, .. }) if path == "video.mp4"
        ),
        "video.mp4 must be listed when the video stream is Present"
    );
    assert!(
        no_video.video_path().is_none(),
        "an unlisted (unverified) video.mp4 is never served"
    );
    assert!(
        no_video.audio_path().is_none(),
        "an unlisted audio.m4a is never served"
    );

    let intact_dir = tmp.path().join("b-intact");
    V2BundleWriter::write(
        &intact_dir,
        "job-intact",
        &build_draft("b-intact", b"v".to_vec()),
    )
    .expect("write intact");
    let intact = V2BundleReader::open(&intact_dir).expect("open intact");
    intact.verify_integrity().expect("intact bundle verifies");
    assert!(intact.video_path().is_some(), "listed video is served");
}

#[test]
fn r2_s16_legacy_manifest_paths_cannot_escape_the_bundle_folder() {
    let tmp = TempDir::new().expect("temp dir must be created");
    fs::write(tmp.path().join("secret.jsonl"), b"{\"secret\":true}\n").expect("secret");
    let cases: [(&str, JsonEdit); 5] = [
        (
            "logcat-traversal",
            Box::new(|m| m["files"]["logcat_device"] = "../secret.jsonl".into()),
        ),
        (
            "deep-traversal",
            Box::new(|m| m["files"]["touches"] = "../../secret.jsonl".into()),
        ),
        (
            "absolute-video",
            Box::new(|m| m["files"]["video"] = "/etc/passwd".into()),
        ),
        (
            "drive-prefix",
            Box::new(|m| m["files"]["video_frames"] = "C:evil.jsonl".into()),
        ),
        (
            "sha-key-traversal",
            Box::new(|m| {
                m["files_sha256"]["../secret.jsonl"] =
                    "0000000000000000000000000000000000000000000000000000000000000000".into();
            }),
        ),
    ];
    for (name, edit) in cases {
        let dir = tmp.path().join("bundles").join(name);
        copy_dir(&repo_schema4_fixture_dir(), &dir);
        edit_json(&dir.join("replay.json"), |m| edit(m));
        let result = LegacyBundleReader::open(&dir);
        assert!(
            matches!(result, Err(StorageError::InvalidBundle { .. })),
            "{name}: escaping legacy path must be rejected, got {:?}",
            result.map(|_| ())
        );
    }

    let ok_dir = tmp.path().join("bundles").join("intact");
    copy_dir(&repo_schema4_fixture_dir(), &ok_dir);
    let reader = LegacyBundleReader::open(&ok_dir).expect("untouched legacy folder opens");
    reader.verify_integrity().expect("legacy integrity");
    assert!(
        reader.video_path().is_some_and(|p| p.starts_with(&ok_dir)),
        "video path stays inside the bundle"
    );
}

#[test]
fn r2_s19_gc_survives_failures_retries_purging_and_compares_time_numerically() {
    use arb_storage::catalog::parse_utc_timestamp_ns;

    let z = parse_utc_timestamp_ns("2026-11-22T12:00:00Z").expect("z");
    assert_eq!(
        parse_utc_timestamp_ns("2026-11-22T12:00:00.5Z"),
        Some(z + 500_000_000),
        "fractional seconds"
    );
    assert_eq!(
        parse_utc_timestamp_ns("2026-11-22T21:00:00+09:00"),
        Some(z),
        "offsets normalise to UTC"
    );
    assert_eq!(
        parse_utc_timestamp_ns("1970-01-01T00:00:00Z"),
        Some(0),
        "epoch"
    );
    assert!(
        parse_utc_timestamp_ns("not-a-time").is_none(),
        "garbage rejected"
    );

    let tmp = TempDir::new().expect("temp dir must be created");
    let managed = tmp.path().join("bundles");
    let catalog = Catalog::open(tmp.path().join("catalog.sqlite"), &managed).expect("catalog");

    // grace: "…00.5Z" sorts BEFORE "…00Z" as a string but is later in time.
    register_v2_bundle(&catalog, "b-grace", &managed.join("b-grace"), "j1", b"g");
    catalog
        .trash_bundle("b-grace", "2026-11-22T12:00:00.5Z")
        .expect("trash");
    // offset: 21:00+09:00 == 12:00Z, older than now.
    register_v2_bundle(&catalog, "b-offset", &managed.join("b-offset"), "j2", b"o");
    catalog
        .trash_bundle("b-offset", "2026-11-22T21:00:00+09:00")
        .expect("trash");
    // failing: its path is a regular file, so directory removal fails on every OS.
    let failing_path = managed.join("b-fail");
    fs::write(&failing_path, b"not a directory").expect("file in place of bundle dir");
    catalog
        .register_bundle(&NewBundleEntry {
            bundle_id: "b-fail".to_owned(),
            path: failing_path.clone(),
            format_version: CatalogFormatVersion::V2,
            parent_bundle_id: None,
            created_at_utc: "2026-11-22T11:00:00Z".to_owned(),
        })
        .expect("register failing bundle");
    catalog
        .trash_bundle("b-fail", "2026-11-22T11:00:00Z")
        .expect("trash");

    let report = catalog
        .gc_with_grace("2026-11-22T12:00:00.25Z", 0)
        .expect("gc run must not abort on one failure");
    assert_eq!(
        report.skipped_in_grace_bundle_ids,
        vec!["b-grace".to_owned()],
        "later fractional timestamp is still in grace"
    );
    assert_eq!(
        report.purged_bundle_ids,
        vec!["b-offset".to_owned()],
        "offset timestamp compared numerically and purged; run continued past the failure"
    );
    assert_eq!(report.failed.len(), 1, "one failure recorded");
    assert_eq!(report.failed[0].bundle_id, "b-fail", "failed bundle id");
    assert_eq!(
        catalog
            .bundle("b-fail")
            .expect("query")
            .expect("row kept")
            .state,
        BundleLifecycleState::Trashed,
        "a bundle that fails the identity check never enters Purging and is retried by GC"
    );

    // The obstruction disappears: the next GC retries the row.
    fs::remove_file(&failing_path).expect("unblock");
    let retry = catalog
        .gc_with_grace("2026-11-22T12:00:01Z", 0)
        .expect("second gc");
    assert!(
        retry.purged_bundle_ids.contains(&"b-fail".to_owned()),
        "failed rows are retried by GC"
    );
    assert!(
        retry.purged_bundle_ids.contains(&"b-grace".to_owned()),
        "grace expired"
    );
    assert!(retry.failed.is_empty(), "no failures left");
    assert!(
        matches!(
            catalog.gc_trashed_bundles("yesterday"),
            Err(StorageError::InvalidArgument { .. })
        ),
        "unparsable now_utc is rejected"
    );
}

#[test]
fn t6i_bundle_writer_emits_human_readable_sidecars_and_share_mp4_with_sha256_integrity() {
    use arb_storage::bundle_io::{
        EVENTS_JSONL_FILE, LOGCAT_LOG_FILE, SHARE_TAPS_MP4_FILE, ShareMp4Sidecar,
        UNANCHORED_LOG_FILE,
    };

    let tmp = TempDir::new().expect("temp dir");
    let bundle_dir = tmp.path().join("t6i-sidecars-full");
    let mut draft = build_draft("01JT6ISIDECAR0000000000001", b"fake-mp4-video".to_vec());
    draft.share_mp4 = Some(ShareMp4Sidecar {
        file_name: SHARE_TAPS_MP4_FILE.to_owned(),
        payload: MediaPayload::Bytes(b"fake-share-taps-mp4".to_vec()),
    });

    V2BundleWriter::write(&bundle_dir, "job-t6i-sidecars", &draft).expect("write bundle");
    let reader = V2BundleReader::open(&bundle_dir).expect("open bundle");
    reader
        .verify_integrity()
        .expect("all sidecars and share-taps.mp4 must pass SHA-256 verification");

    let logcat_text = fs::read_to_string(bundle_dir.join(LOGCAT_LOG_FILE)).expect("logcat.log");
    assert!(
        logcat_text.contains("Activity") && logcat_text.contains("main timeline log"),
        "logcat.log must contain human-readable main log line: {logcat_text}"
    );
    let unanchored_text =
        fs::read_to_string(bundle_dir.join(UNANCHORED_LOG_FILE)).expect("unanchored.log");
    assert!(
        unanchored_text.contains("quarantined unanchored log"),
        "unanchored.log must contain unanchored log line: {unanchored_text}"
    );
    let events_text = fs::read_to_string(bundle_dir.join(EVENTS_JSONL_FILE)).expect("events.jsonl");
    assert!(
        events_text.contains("\"kind\":\"tap\"")
            && events_text.contains("\"kind\":\"discontinuity\""),
        "events.jsonl must record tap & discontinuity entries: {events_text}"
    );
    assert_eq!(
        fs::read(bundle_dir.join(SHARE_TAPS_MP4_FILE)).expect("share-taps.mp4"),
        b"fake-share-taps-mp4",
        "share-taps.mp4 sidecar must be committed atomically"
    );

    // When there are zero unanchored logs, unanchored.log MUST NOT be emitted (zero empty clutter).
    let clean_dir = tmp.path().join("t6i-sidecars-no-unanchored");
    let mut clean_draft = build_draft("01JT6ISIDECAR0000000000002", b"fake-mp4-video-2".to_vec());
    clean_draft.logs.retain(|r| r.bucket == RecordBucket::Main);
    clean_draft.unanchored_summary = UnanchoredSummary {
        included_count: 0,
        truncated_count: 0,
    };
    V2BundleWriter::write(&clean_dir, "job-t6i-clean", &clean_draft).expect("write clean bundle");
    assert!(
        clean_dir.join(LOGCAT_LOG_FILE).is_file(),
        "logcat.log must exist"
    );
    assert!(
        !clean_dir.join(UNANCHORED_LOG_FILE).exists(),
        "unanchored.log must NOT be created when unanchored logs are empty"
    );
}

#[test]
fn empty_timeline_has_readable_sidecars_but_no_unanchored_log() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("bundle");
    let mut draft = build_draft("empty", vec![]);
    draft.logs.clear();
    draft.taps.clear();
    draft.discontinuities.clear();
    draft.frames.clear();
    draft.video_mp4 = None;
    if let Some(video) = draft.streams.video.as_mut() {
        video.status = StreamStatus::Absent;
        video.frame_count = 0;
    }
    let manifest = V2BundleWriter::write(&dir, "job", &draft).unwrap();
    for name in ["events.jsonl", "logcat.log"] {
        assert!(
            manifest
                .files
                .iter()
                .any(|f| f.path == name && f.bytes == 0)
        );
        assert_eq!(fs::metadata(dir.join(name)).unwrap().len(), 0);
    }
    assert!(!dir.join("unanchored.log").exists());
}

#[test]
fn current_bundle_rejects_external_media_and_timeline() {
    #[cfg(unix)]
    use std::os::unix::fs::symlink as symlink_file;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_file;
    let tmp = TempDir::new().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    for dir in [&a, &b] {
        V2BundleWriter::write(dir, "job", &build_draft("copy", vec![1, 2, 3])).unwrap();
    }
    let opened = V2BundleReader::open(&a).unwrap();
    for name in [
        "video.mp4",
        "timeline.sqlite",
        "manifest.json",
        "events.jsonl",
    ] {
        fs::rename(a.join(name), a.join("original")).unwrap();
        symlink_file(b.join(name), a.join(name)).unwrap();
        let err = V2BundleReader::open(&a).expect_err("external file must be rejected");
        assert!(err.to_string().contains("outside"), "{err}");
        if name == "events.jsonl" {
            let digest_error = opened
                .verify_integrity()
                .expect_err("check digest target again after opening");
            assert!(
                digest_error.to_string().contains("outside"),
                "{digest_error}"
            );
        }
        fs::remove_file(a.join(name)).unwrap();
        fs::rename(a.join("original"), a.join(name)).unwrap();
    }
}

#[test]
fn probe_bundle_disk_state_distinguishes_present_missing_and_storage_unavailable() {
    use arb_storage::catalog::{BundleDiskState, probe_bundle_disk_state};

    #[cfg(unix)]
    use std::os::unix::fs::symlink as symlink_dir;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_dir;

    let tmp = TempDir::new().expect("tempdir");
    let parent = tmp.path().join("bundles");
    fs::create_dir_all(&parent).expect("create parent");

    let bundle_dir = parent.join("bundle-present");
    fs::create_dir_all(&bundle_dir).expect("create bundle dir");
    assert_eq!(
        probe_bundle_disk_state(&bundle_dir),
        BundleDiskState::Present,
        "existing bundle directory must report Present"
    );

    fs::remove_dir_all(&bundle_dir).expect("manual delete bundle dir");
    assert_eq!(
        probe_bundle_disk_state(&bundle_dir),
        BundleDiskState::Missing,
        "manually deleted bundle under existing parent must report Missing"
    );

    fs::write(&bundle_dir, "replacement file").expect("create file");
    assert_eq!(
        probe_bundle_disk_state(&bundle_dir),
        BundleDiskState::StorageUnavailable
    );

    let link = parent.join("linked-bundle");
    let target = tmp.path().join("removable-target");
    fs::create_dir(&target).expect("create symlink target");
    symlink_dir(&target, &link).expect("create directory link");
    assert_eq!(probe_bundle_disk_state(&link), BundleDiskState::Present);
    fs::remove_dir(&target).expect("disconnect link target");
    assert_eq!(
        probe_bundle_disk_state(&link),
        BundleDiskState::StorageUnavailable
    );

    let unmounted_bundle = tmp.path().join("unmounted-volume").join("bundle-ext");
    assert_eq!(
        probe_bundle_disk_state(&unmounted_bundle),
        BundleDiskState::StorageUnavailable,
        "bundle whose parent directory is absent must report StorageUnavailable"
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "single matrix test covering every forget_missing_bundle and prune_missing_bundles guard"
)]
fn forget_missing_bundles_is_nondestructive_and_enforces_all_guards() {
    use arb_storage::catalog::{EditSession, LeaseHolderKind};

    let tmp = TempDir::new().expect("tempdir");
    let managed = tmp.path().join("bundles");
    let external = tmp.path().join("external");
    let unmounted = tmp.path().join("detached-drive");
    fs::create_dir_all(&managed).expect("create managed");
    fs::create_dir_all(&external).expect("create external");
    let catalog = Catalog::open(tmp.path().join("catalog.sqlite"), &managed).expect("open catalog");

    let present_dir = managed.join("b-present");
    let missing_job_dir = managed.join("b-missing-job");
    fs::create_dir_all(&present_dir).expect("create present");
    fs::write(present_dir.join("marker.txt"), b"keep").expect("write marker");

    for (id, path, fmt) in [
        ("b-present", present_dir.clone(), CatalogFormatVersion::V2),
        (
            "b-missing-managed",
            managed.join("b-missing-managed"),
            CatalogFormatVersion::V2,
        ),
        (
            "b-missing-ext",
            external.join("b-missing-ext"),
            CatalogFormatVersion::V2,
        ),
        (
            "b-missing-legacy",
            external.join("b-missing-legacy"),
            CatalogFormatVersion::LegacyRo,
        ),
        (
            "b-missing-leased",
            managed.join("b-missing-leased"),
            CatalogFormatVersion::V2,
        ),
        (
            "b-missing-job",
            missing_job_dir.clone(),
            CatalogFormatVersion::V2,
        ),
        (
            "b-unavail",
            unmounted.join("b-unavail"),
            CatalogFormatVersion::V2,
        ),
    ] {
        catalog
            .register_bundle(&NewBundleEntry {
                bundle_id: id.to_owned(),
                path,
                format_version: fmt,
                parent_bundle_id: None,
                created_at_utc: "2026-11-25T00:00:00Z".to_owned(),
            })
            .expect("register");
    }

    catalog
        .upsert_edit_session(&EditSession {
            in_time_ns: None,
            out_time_ns: None,
            session_id: "sess-missing-ext".to_owned(),
            bundle_id: "b-missing-ext".to_owned(),
            in_frame_index: Some(1),
            out_frame_index: Some(5),
            playhead_frame_index: Some(2),
            av_offset_override_ms: None,
            updated_at_utc: "2026-11-25T00:00:00Z".to_owned(),
        })
        .expect("upsert edit session");
    let live_lease = catalog
        .acquire_lease(
            "b-missing-leased",
            LeaseHolderKind::IdeViewer,
            std::process::id(),
            "2026-11-25T00:00:00Z",
        )
        .expect("acquire lease");
    catalog
        .create_pinned_job(
            "job-missing",
            &missing_job_dir,
            &["chunk-1".to_owned()],
            "2026-11-25T00:00:00Z",
        )
        .expect("create active job");

    let empty: Vec<String> = Vec::new();
    let skip_present = catalog
        .forget_missing_bundle("b-present", "2026-11-25T00:01:00Z")
        .expect("forget present");
    assert_eq!(skip_present.forgotten_bundle_ids, empty);
    assert_eq!(skip_present.skipped_present_bundle_ids, vec!["b-present"]);
    assert!(present_dir.join("marker.txt").is_file());

    let skip_unavail = catalog
        .forget_missing_bundle("b-unavail", "2026-11-25T00:01:00Z")
        .expect("forget unavail");
    assert_eq!(skip_unavail.forgotten_bundle_ids, empty);
    assert_eq!(
        skip_unavail.skipped_storage_unavailable_bundle_ids,
        vec!["b-unavail"]
    );

    let report = catalog
        .prune_missing_bundles("2026-11-25T00:01:00Z")
        .expect("prune missing");
    assert_eq!(
        report.forgotten_bundle_ids,
        vec!["b-missing-ext", "b-missing-legacy", "b-missing-managed"]
    );
    assert_eq!(report.skipped_present_bundle_ids, empty);
    assert_eq!(
        report.skipped_storage_unavailable_bundle_ids,
        vec!["b-unavail"]
    );
    assert_eq!(report.skipped_leased_bundle_ids, vec!["b-missing-leased"]);
    assert_eq!(report.skipped_active_job_bundle_ids, vec!["b-missing-job"]);
    assert!(present_dir.join("marker.txt").is_file());
    assert!(catalog.bundle("b-present").expect("lookup").is_some());
    assert!(catalog.bundle("b-unavail").expect("lookup").is_some());
    assert!(
        catalog
            .get_edit_session("sess-missing-ext")
            .expect("edit session lookup")
            .is_none(),
        "edit_sessions belonging to forgotten bundle must be cleaned up"
    );

    catalog.release_lease(&live_lease).expect("release lease");
    let after_release = catalog
        .forget_missing_bundle("b-missing-leased", "2026-11-25T00:02:00Z")
        .expect("forget after release");
    assert_eq!(after_release.forgotten_bundle_ids, vec!["b-missing-leased"]);
}

#[test]
fn gc_cleans_missing_unmanaged_and_legacy_trashed_rows_without_touching_present_ones() {
    let tmp = TempDir::new().expect("tempdir");
    let managed = tmp.path().join("bundles");
    let external = tmp.path().join("external");
    fs::create_dir_all(&managed).expect("create managed");
    fs::create_dir_all(&external).expect("create external");

    let catalog = Catalog::open(tmp.path().join("catalog.sqlite"), &managed).expect("open catalog");

    let ext_present = external.join("ext-present");
    let ext_missing = external.join("ext-missing");
    let leg_present = managed.join("leg-present");
    let leg_missing = managed.join("leg-missing");
    fs::create_dir_all(&ext_present).expect("create ext_present");
    fs::create_dir_all(&leg_present).expect("create leg_present");

    for (id, path, fmt) in [
        ("ext-present", &ext_present, CatalogFormatVersion::V2),
        ("ext-missing", &ext_missing, CatalogFormatVersion::V2),
        ("leg-present", &leg_present, CatalogFormatVersion::LegacyRo),
        ("leg-missing", &leg_missing, CatalogFormatVersion::LegacyRo),
    ] {
        catalog
            .register_bundle(&NewBundleEntry {
                bundle_id: id.to_owned(),
                path: path.clone(),
                format_version: fmt,
                parent_bundle_id: None,
                created_at_utc: "2026-11-25T00:00:00Z".to_owned(),
            })
            .expect("register");
        catalog
            .trash_bundle(id, "2026-11-25T00:00:00Z")
            .expect("trash");
    }

    let retained = catalog.gc_trashed_bundles("2026-11-25T00:00:05Z").unwrap();
    assert!(
        retained.purged_bundle_ids.is_empty(),
        "ordinary GC retains missing rows during grace"
    );
    assert_eq!(retained.skipped_in_grace_bundle_ids.len(), 2);

    let gc = catalog
        .gc_with_grace("2026-11-25T00:00:05Z", 0)
        .expect("gc must succeed");
    assert_eq!(
        gc.purged_bundle_ids,
        vec!["ext-missing", "leg-missing"],
        "missing trashed unmanaged/legacy rows must be cleaned from catalog"
    );
    assert_eq!(
        gc.skipped_unmanaged_or_legacy_bundle_ids,
        vec!["ext-present", "leg-present"],
        "present unmanaged/legacy rows must stay protected"
    );
    assert!(ext_present.is_dir(), "present external dir untouched");
    assert!(leg_present.is_dir(), "present legacy dir untouched");
    assert!(catalog.bundle("ext-missing").expect("lookup").is_none());
    assert!(catalog.bundle("leg-missing").expect("lookup").is_none());
}

#[test]
fn gc_keeps_catalog_rows_when_managed_parent_is_unavailable() {
    let tmp = TempDir::new().unwrap();
    let managed = tmp.path().join("bundles");
    let db = tmp.path().join("catalog.sqlite");
    let catalog = Catalog::open(&db, &managed).unwrap();
    for id in ["trashed", "purging"] {
        register_v2_bundle(&catalog, id, &managed.join(id), id, b"data");
        catalog.trash_bundle(id, "2026-11-25T00:00:00Z").unwrap();
    }
    rusqlite::Connection::open(db)
        .unwrap()
        .execute(
            "UPDATE bundles SET state='purging' WHERE bundle_id='purging'",
            [],
        )
        .unwrap();
    fs::rename(&managed, tmp.path().join("offline")).unwrap();
    catalog.gc_with_grace("2026-11-25T00:00:05Z", 0).unwrap();
    for id in ["trashed", "purging"] {
        assert!(
            catalog.bundle(id).unwrap().is_some(),
            "offline {id} retained"
        );
    }
}

fn tree_hashes(root: &Path) -> BTreeMap<PathBuf, [u8; 32]> {
    use sha2::{Digest, Sha256};
    let mut result = BTreeMap::new();
    let mut dirs = vec![root.to_owned()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(dir).expect("list replacement") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                result.insert(
                    path.strip_prefix(root)
                        .expect("fixture under root")
                        .to_owned(),
                    Sha256::digest(fs::read(path).expect("read replacement file")).into(),
                );
            }
        }
    }
    result
}

#[test]
fn gc_and_recovery_preserve_replacement_trees() {
    let now = "2026-11-22T12:00:00Z";
    for restart in [false, true] {
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("catalog.sqlite");
        let root = tmp.path().join("bundles");
        let catalog = Catalog::open(&db, &root).unwrap();
        for id in ["other", "personal", "valid"] {
            register_v2_bundle(&catalog, id, &root.join(id), id, b"original");
            catalog.trash_bundle(id, now).unwrap();
        }
        fs::rename(root.join("other"), tmp.path().join("moved-other")).unwrap();
        V2BundleWriter::write(
            root.join("other"),
            "different-job",
            &build_draft("different", vec![42]),
        )
        .unwrap();
        fs::rename(root.join("personal"), tmp.path().join("moved-personal")).unwrap();
        fs::create_dir_all(root.join("personal/nested")).unwrap();
        fs::write(root.join("personal/nested/document"), b"keep").unwrap();
        let before = [
            tree_hashes(&root.join("other")),
            tree_hashes(&root.join("personal")),
        ];
        if restart {
            drop(catalog);
            rusqlite::Connection::open(&db)
                .unwrap()
                .execute("UPDATE bundles SET state = 'purging'", [])
                .unwrap();
            let reopened = Catalog::open(&db, &root).unwrap();
            let recovery = reopened.recover_on_startup(now).unwrap();
            assert_eq!(recovery.completed_purging_bundle_ids, ["valid"]);
            assert_eq!(recovery.failed.len(), 2);
            assert!(
                recovery
                    .failed
                    .iter()
                    .all(|f| f.error.contains("identity mismatch"))
            );
        }
        let catalog = Catalog::open(&db, &root).unwrap();
        let report = catalog.gc_with_grace(now, 0).unwrap();
        if !restart {
            assert_eq!(report.purged_bundle_ids, ["valid"]);
        }
        assert!(!root.join("valid").exists());
        assert_eq!(report.failed.len(), 2);
        for (id, hashes) in ["other", "personal"].into_iter().zip(before) {
            assert!(
                report
                    .failed
                    .iter()
                    .any(|f| f.bundle_id == id && f.error.contains("identity mismatch"))
            );
            assert!(catalog.bundle(id).unwrap().is_some());
            assert_eq!(tree_hashes(&root.join(id)), hashes);
        }
    }
}

#[test]
fn interrupted_claim_is_recovered_even_if_public_path_is_reused() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("bundles");
    let db = tmp.path().join("db");
    let source = root.join("original");
    let now = "2026-11-22T12:00:00Z";
    let catalog = Catalog::open(&db, &root).unwrap();
    register_v2_bundle(&catalog, "original", &source, "job", b"payload");
    catalog.trash_bundle("original", now).unwrap();
    drop(catalog);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let token = "a".repeat(32);
    conn.execute("UPDATE bundles SET state = 'purging'", [])
        .unwrap();
    conn.execute("INSERT INTO purge_claims VALUES ('original', ?1)", [&token])
        .unwrap();
    drop(conn);
    let claim = root.join(format!(".purging-{token}"));
    fs::rename(&source, &claim).unwrap();
    let reopened = Catalog::open(&db, &root).unwrap();
    assert_eq!(
        reopened
            .prune_missing_bundles(now)
            .unwrap()
            .skipped_storage_unavailable_bundle_ids,
        ["original"]
    );
    fs::create_dir(&source).unwrap();
    fs::write(source.join("keep"), b"replacement").unwrap();
    let before = tree_hashes(&source);
    let report = reopened.recover_on_startup(now).unwrap();
    assert_eq!(report.completed_purging_bundle_ids, ["original"]);
    assert!(!claim.exists());
    assert_eq!(tree_hashes(&source), before);
}

#[test]
fn partially_deleted_claim_is_finished_not_restored_or_stuck() {
    // A crash or a locked file during `remove_dir_all` can leave the verified private claim
    // without its manifest. GC must finish it instead of handing a half-deleted bundle back to
    // the public path and failing forever.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("bundles");
    let db = tmp.path().join("db");
    let source = root.join("original");
    let now = "2026-11-22T12:00:00Z";
    let catalog = Catalog::open(&db, &root).unwrap();
    register_v2_bundle(&catalog, "original", &source, "job", b"payload");
    catalog.trash_bundle("original", now).unwrap();
    drop(catalog);
    let token = "b".repeat(32);
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("UPDATE bundles SET state = 'purging'", [])
        .unwrap();
    conn.execute("INSERT INTO purge_claims VALUES ('original', ?1)", [&token])
        .unwrap();
    drop(conn);
    let claim = root.join(format!(".purged-{token}"));
    fs::rename(&source, &claim).unwrap();
    fs::remove_file(claim.join("manifest.json")).unwrap();
    let reopened = Catalog::open(&db, &root).unwrap();
    let report = reopened.recover_on_startup(now).unwrap();
    assert_eq!(report.completed_purging_bundle_ids, ["original"]);
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert!(!claim.exists());
    assert!(!source.exists());
    assert!(reopened.bundle("original").unwrap().is_none());
}

#[test]
fn identity_mismatch_keeps_bundle_trashed_and_restorable() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("bundles");
    let catalog = Catalog::open(tmp.path().join("catalog"), &root).unwrap();
    let now = "2026-11-22T12:00:00Z";
    let path = root.join("owner");
    register_v2_bundle(&catalog, "owner", &path, "owner", b"private");
    catalog.trash_bundle("owner", now).unwrap();
    fs::write(path.join("manifest.json"), b"{").unwrap();
    let report = catalog.gc_with_grace(now, 0).unwrap();
    assert_eq!(report.failed.len(), 1);
    let row = catalog.bundle("owner").unwrap().unwrap();
    assert_eq!(row.state, arb_core::state::BundleLifecycleState::Trashed);
    catalog.restore_bundle("owner").unwrap();
}

#[test]
fn purging_row_failing_identity_returns_to_trashed_and_verified_claim_ignores_public_path() {
    #[cfg(unix)]
    use std::os::unix::fs::symlink as symlink_dir;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_dir;
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("bundles");
    let db = tmp.path().join("db");
    let now = "2026-11-22T12:00:00Z";
    let catalog = Catalog::open(&db, &root).unwrap();
    register_v2_bundle(&catalog, "foreign", &root.join("foreign"), "j1", b"x");
    register_v2_bundle(&catalog, "half", &root.join("half"), "j2", b"y");
    for id in ["foreign", "half"] {
        catalog.trash_bundle(id, now).unwrap();
    }
    drop(catalog);
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("UPDATE bundles SET state = 'purging'", [])
        .unwrap();
    let token = "c".repeat(32);
    conn.execute("INSERT INTO purge_claims VALUES ('half', ?1)", [&token])
        .unwrap();
    drop(conn);
    fs::write(root.join("foreign/manifest.json"), b"{").unwrap();
    // A verified half-deleted claim, while a link now occupies the public path.
    let claim = root.join(format!(".purged-{token}"));
    fs::rename(root.join("half"), &claim).unwrap();
    fs::remove_file(claim.join("manifest.json")).unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), b"keep").unwrap();
    symlink_dir(&outside, root.join("half")).unwrap();

    let reopened = Catalog::open(&db, &root).unwrap();
    let report = reopened.recover_on_startup(now).unwrap();
    assert_eq!(report.completed_purging_bundle_ids, ["half"]);
    assert_eq!(report.failed.len(), 1);
    assert!(!claim.exists());
    assert_eq!(fs::read(outside.join("keep")).unwrap(), b"keep");
    let foreign = reopened.bundle("foreign").unwrap().unwrap();
    assert_eq!(
        foreign.state,
        arb_core::state::BundleLifecycleState::Trashed
    );
    reopened.restore_bundle("foreign").unwrap();
}

#[test]
fn gc_rejects_corrupt_or_wrong_owner_manifest_and_links() {
    #[cfg(unix)]
    use std::os::unix::fs::symlink as symlink_dir;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_dir;
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("bundles");
    let catalog = Catalog::open(tmp.path().join("catalog"), &root).unwrap();
    let now = "2026-11-22T12:00:00Z";
    for id in ["corrupt", "schema", "owner", "link"] {
        let path = root.join(id);
        register_v2_bundle(&catalog, id, &path, id, b"private");
        catalog.trash_bundle(id, now).unwrap();
        let manifest = path.join("manifest.json");
        match id {
            "corrupt" => fs::write(manifest, b"{").unwrap(),
            "link" => {
                let moved = tmp.path().join("target");
                fs::rename(&path, &moved).unwrap();
                symlink_dir(&moved, &path).unwrap();
            }
            _ => {
                let mut value: serde_json::Value =
                    serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
                if id == "schema" {
                    value["schema_version"] = serde_json::json!(4);
                } else {
                    value["created_at_utc"] = serde_json::json!("2020-01-01T00:00:00Z");
                }
                fs::write(manifest, value.to_string()).unwrap();
            }
        }
    }
    let before = tree_hashes(&root);
    let report = catalog.gc_with_grace(now, 0).unwrap();
    assert_eq!(report.failed.len(), 4);
    assert!(
        report
            .failed
            .iter()
            .all(|f| f.error.contains("identity mismatch"))
    );
    assert_eq!(tree_hashes(&root), before);
    assert!(
        fs::symlink_metadata(root.join("link"))
            .unwrap()
            .is_symlink()
    );
}

#[test]
fn interrupted_unverified_claim_finishes_when_symlink_replaces_public_path() {
    #[cfg(unix)]
    use std::os::unix::fs::symlink as symlink_dir;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_dir;
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("bundles");
    let db = tmp.path().join("db");
    let source = root.join("claimed");
    let now = "2026-11-22T12:00:00Z";
    let catalog = Catalog::open(&db, &root).unwrap();
    register_v2_bundle(&catalog, "claimed", &source, "job", b"payload");
    catalog.trash_bundle("claimed", now).unwrap();
    drop(catalog);

    let token = "d".repeat(32);
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("UPDATE bundles SET state = 'purging'", [])
        .unwrap();
    conn.execute("INSERT INTO purge_claims VALUES ('claimed', ?1)", [&token])
        .unwrap();
    drop(conn);

    // Crash between `.purging-<token>` rename and `.purged-<token>` rename, while a symlink now
    // occupies the public path.
    let claim = root.join(format!(".purging-{token}"));
    fs::rename(&source, &claim).unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), b"keep").unwrap();
    symlink_dir(&outside, &source).unwrap();

    let reopened = Catalog::open(&db, &root).unwrap();
    let report = reopened.recover_on_startup(now).unwrap();
    assert_eq!(report.completed_purging_bundle_ids, ["claimed"]);
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert!(!claim.exists());
    assert!(fs::symlink_metadata(&source).unwrap().is_symlink());
    assert_eq!(fs::read(outside.join("keep")).unwrap(), b"keep");
}

#[test]
fn reided_bundle_inside_managed_root_verifies_recorded_manifest_id() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("bundles");
    let catalog = Catalog::open(tmp.path().join("catalog"), &root).unwrap();
    let now = "2026-11-22T12:00:00Z";

    let valid_path = root.join("copy-valid");
    let valid_manifest =
        V2BundleWriter::write(&valid_path, "job-valid", &build_draft("orig-id", vec![1])).unwrap();
    catalog
        .register_bundle_with_manifest_id(
            &NewBundleEntry {
                bundle_id: "ext-valid".to_owned(),
                path: valid_path.clone(),
                format_version: CatalogFormatVersion::V2,
                parent_bundle_id: None,
                created_at_utc: valid_manifest.created_at_utc,
            },
            "orig-id",
        )
        .unwrap();
    catalog.trash_bundle("ext-valid", now).unwrap();

    let foreign_path = root.join("copy-foreign");
    let foreign_manifest = V2BundleWriter::write(
        &foreign_path,
        "job-foreign",
        &build_draft("orig-id", vec![2]),
    )
    .unwrap();
    catalog
        .register_bundle_with_manifest_id(
            &NewBundleEntry {
                bundle_id: "ext-foreign".to_owned(),
                path: foreign_path.clone(),
                format_version: CatalogFormatVersion::V2,
                parent_bundle_id: None,
                created_at_utc: foreign_manifest.created_at_utc,
            },
            "orig-id",
        )
        .unwrap();
    catalog.trash_bundle("ext-foreign", now).unwrap();
    edit_json(&foreign_path.join("manifest.json"), |v| {
        v["bundle_id"] = serde_json::json!("other-id");
    });

    let report = catalog.gc_with_grace(now, 0).unwrap();
    assert_eq!(report.purged_bundle_ids, ["ext-valid"]);
    assert!(!valid_path.exists());
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].bundle_id, "ext-foreign");
    assert!(report.failed[0].error.contains("identity mismatch"));
    assert!(foreign_path.is_dir());
}

/// K15 / W3B: a draft that declares video without a usable `video.mp4`, or whose `frames`
/// rows disagree with `frame_count`, is never committed.
#[test]
fn w3b_commit_guard_refuses_video_declaration_without_matching_file_and_rows() {
    type Corrupt = fn(&mut V2BundleDraft);
    let cases: [(&str, Corrupt); 5] = [
        ("partial-without-file", |d| {
            d.video_mp4 = None;
            if let Some(v) = d.streams.video.as_mut() {
                v.status = StreamStatus::Partial;
            }
        }),
        ("present-with-empty-file", |d| {
            d.video_mp4 = Some(MediaPayload::Bytes(Vec::new()));
        }),
        ("row-count-mismatch", |d| {
            d.frames.truncate(1);
        }),
        ("absent-with-rows", |d| {
            d.video_mp4 = None;
            if let Some(v) = d.streams.video.as_mut() {
                v.status = StreamStatus::Absent;
                v.frame_count = 0;
            }
        }),
        ("absent-with-file", |d| {
            d.frames.clear();
            if let Some(v) = d.streams.video.as_mut() {
                v.status = StreamStatus::Absent;
                v.frame_count = 0;
            }
        }),
    ];
    for (name, corrupt) in cases {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("bundle");
        let mut draft = build_draft(name, b"mp4-bytes".to_vec());
        corrupt(&mut draft);
        let err = V2BundleWriter::write(&dir, "job", &draft)
            .expect_err("an inconsistent video declaration must not be committed");
        assert!(
            matches!(&err, StorageError::IntegrityMismatch { path, .. } if path == "video.mp4"),
            "{name}: {err:?}"
        );
        assert!(!dir.exists(), "{name}: nothing may be published");
        assert!(
            std::fs::read_dir(tmp.path()).unwrap().next().is_none(),
            "{name}: staging must be cleaned up"
        );
    }
    // A consistent Absent track commits.
    let tmp = TempDir::new().unwrap();
    let mut draft = build_draft("absent-ok", Vec::new());
    draft.frames.clear();
    draft.video_mp4 = None;
    if let Some(v) = draft.streams.video.as_mut() {
        v.status = StreamStatus::Absent;
        v.frame_count = 0;
    }
    V2BundleWriter::write(tmp.path().join("bundle"), "job", &draft).unwrap();
}
