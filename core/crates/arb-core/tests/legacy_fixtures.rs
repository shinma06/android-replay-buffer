//! Integration tests reading real schema 4 repository fixtures (read-only)
//! plus synthetic schema 3 (`v0.2.0`) and log-only schema 4 manifests.

use std::fs;
use std::path::PathBuf;

use arb_core::legacy::{
    LegacyFramePresentation, LegacySaveKind, LegacySchemaVersion, LegacyUncertainty,
    parse_schema3_frames_jsonl, parse_schema3_session, parse_schema4_frames_jsonl,
    parse_schema4_logs_jsonl, parse_schema4_manifest, parse_schema4_touches_jsonl,
};
use arb_core::time::{TimeAlignment, UncertaintyReason};

fn repo_schema4_fixture_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/core/crates/arb-core
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .join("..")
        .join("..")
        .join("..")
        .join("plugin")
        .join("src")
        .join("test")
        .join("resources")
        .join("replay-trim-parity")
        .join("replay-20240101-000000-manual-feedface")
}

#[test]
fn parses_real_repo_schema4_fixture_manifest_read_only() {
    let dir = repo_schema4_fixture_dir();
    assert!(
        dir.is_dir(),
        "real schema 4 fixture directory must exist at {}",
        dir.display()
    );

    let manifest_text =
        fs::read_to_string(dir.join("replay.json")).expect("must read fixture replay.json");
    let meta =
        parse_schema4_manifest(&manifest_text).expect("must parse real schema 4 replay.json");

    assert_eq!(
        meta.schema,
        LegacySchemaVersion::Schema4,
        "fixture must decode as Schema4"
    );
    assert!(meta.complete, "fixture must be marked complete");
    assert_eq!(meta.save_id, "fixture-source", "save_id must match fixture");
    assert_eq!(
        meta.folder_name.as_deref(),
        Some("replay-20240101-000000-manual-feedface"),
        "folder_name must match fixture"
    );
    assert_eq!(
        meta.save_kind,
        LegacySaveKind::VideoAndLogs,
        "fixture is a video+logs save"
    );
    assert!(!meta.video_excluded, "fixture video is not excluded");
    assert_eq!(
        meta.files.video.as_deref(),
        Some("video.mp4"),
        "video filename must match"
    );
    assert_eq!(
        meta.files.video_frames.as_deref(),
        Some("video-frames.jsonl"),
        "video_frames filename must match"
    );
    assert_eq!(
        meta.files.touches.as_deref(),
        Some("touches.jsonl"),
        "touches filename must match"
    );
    assert_eq!(
        meta.counts.video_frames,
        Some(24),
        "manifest must report 24 video frames"
    );
    assert_eq!(
        meta.counts.device_log_rows,
        Some(5),
        "manifest must report 5 device log rows"
    );
    assert_eq!(
        meta.counts.app_log_rows,
        Some(3),
        "manifest must report 3 app log rows"
    );
    assert_eq!(
        meta.counts.touches,
        Some(2),
        "manifest must report 2 touches"
    );
    assert_eq!(meta.parts.len(), 2, "fixture has 2 video playback segments");
    assert_eq!(
        meta.video_movie_duration_us,
        Some(2_200_002),
        "fixture movie duration must match"
    );
    assert_eq!(
        meta.timebase.window_duration_ns,
        Some(6_200_000_000),
        "fixture window duration must match"
    );
    assert_eq!(
        meta.files_sha256.len(),
        7,
        "fixture files_sha256 has 7 payload entries"
    );
}

#[test]
fn parses_real_repo_schema4_fixture_jsonl_streams_read_only() {
    let dir = repo_schema4_fixture_dir();
    assert!(
        dir.is_dir(),
        "real schema 4 fixture directory must exist at {}",
        dir.display()
    );

    let frames_text = fs::read_to_string(dir.join("video-frames.jsonl"))
        .expect("must read fixture video-frames.jsonl");
    let frames =
        parse_schema4_frames_jsonl(&frames_text).expect("must parse fixture video-frames.jsonl");
    assert_eq!(frames.len(), 24, "fixture must contain 24 frame rows");
    assert!(frames[0].source_key, "frame 0 must be keyframe");
    assert_eq!(frames[0].t_ns, Some(0), "frame 0 t_ns must be 0");
    assert_eq!(
        frames[12].segment_index, 1,
        "frame 12 starts segment 1 after discontinuity"
    );
    assert_eq!(
        frames[12].t_ns,
        Some(5_000_000_000),
        "frame 12 t_ns must be 5_000_000_000"
    );
    assert!(
        matches!(
            frames[0].to_time_alignment("legacy-boot", 0),
            TimeAlignment::Anchored {
                boottime_ns: 0,
                half_width_ns: 0,
                ..
            }
        ),
        "aligned schema 4 frame must project to Anchored"
    );

    let device_logs_text = fs::read_to_string(dir.join("logcat-device.jsonl"))
        .expect("must read fixture logcat-device.jsonl");
    let device_logs = parse_schema4_logs_jsonl(&device_logs_text)
        .expect("must parse fixture logcat-device.jsonl");
    assert_eq!(device_logs.len(), 5, "fixture has 5 device log rows");
    assert_eq!(
        device_logs[4].t_ns, None,
        "5th fixture device log row has null t_ns"
    );
    assert!(
        matches!(
            device_logs[4].to_time_alignment("legacy-boot", 10_000),
            TimeAlignment::Unanchored {
                reason: UncertaintyReason::LegacySentinelUnknown,
                ..
            }
        ),
        "null-t_ns legacy log row must project to Unanchored"
    );

    let app_logs_text = fs::read_to_string(dir.join("logcat-app.jsonl"))
        .expect("must read fixture logcat-app.jsonl");
    let app_logs =
        parse_schema4_logs_jsonl(&app_logs_text).expect("must parse fixture logcat-app.jsonl");
    assert_eq!(app_logs.len(), 3, "fixture has 3 app log rows");

    let touches_text =
        fs::read_to_string(dir.join("touches.jsonl")).expect("must read fixture touches.jsonl");
    let touches =
        parse_schema4_touches_jsonl(&touches_text).expect("must parse fixture touches.jsonl");
    assert_eq!(touches.len(), 2, "fixture has 2 touch rows");
    assert_eq!(touches[0].action, "DOWN", "first touch is DOWN");
    assert_eq!(
        touches[0].t_ns,
        Some(500_000_000),
        "first touch t_ns is 500_000_000"
    );
    assert_eq!(touches[1].action, "UP", "second touch is UP");
    assert_eq!(touches[1].t_ns, None, "second touch has null t_ns");
    assert_eq!(
        touches[1].uncertainty,
        LegacyUncertainty::Missing,
        "second touch has null uncertainty"
    );
}

#[test]
fn parses_synthetic_schema3_bundle_and_logs() {
    let session = r#"{
        "schema": 3,
        "complete": true,
        "save_id": "v020-save",
        "capture_mode": "VIDEO_AND_LOGS",
        "window_clock_known": true,
        "window_start_ns": "10000000000",
        "window_end_ns": "40000000000",
        "window_end_uncertainty_ns": "2500000",
        "replay_seconds": 30,
        "created_at": "2026-11-10T02:49:18Z",
        "video_movie_duration_us": "30000000",
        "parts": [
            {
                "file": "video-001.mp4",
                "segment_index": 0,
                "generation": 1,
                "movie_start_us": "0",
                "movie_end_us": "30000000",
                "duration_us": "30000000",
                "window_start_ns": "0",
                "window_end_ns": "30000000000",
                "clock_alignment_known": true,
                "preroll_samples": 1
            }
        ],
        "files_sha256": {
            "video-001.mp4": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "frames.jsonl": "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210"
        },
        "extra_v021_field": "tolerated"
    }"#;

    let meta = parse_schema3_session(session).expect("schema 3 session must parse");
    assert_eq!(meta.schema, LegacySchemaVersion::Schema3, "must be Schema3");
    assert_eq!(
        meta.files.manifest, "session.json",
        "manifest is session.json"
    );
    assert_eq!(
        meta.files.video.as_deref(),
        Some("video-001.mp4"),
        "video is video-001.mp4"
    );
    assert_eq!(
        meta.files.video_frames.as_deref(),
        Some("frames.jsonl"),
        "frames file is frames.jsonl"
    );
    assert_eq!(
        meta.parts[0].preroll_samples, 1,
        "preroll_samples must be preserved"
    );

    let frames_jsonl = r#"{"part":"video-001.mp4","segment_index":0,"sample_index":0,"source_key":true,"output_key":true,"source_pts_us":"1000","media_pts_us":"0","movie_pts_us":null,"movie_end_us":null,"elapsed_ns":"9999000000","window_ns":"-1000000","uncertainty_ns":"1000","clock_alignment_known":true,"presented":false,"preroll":true,"clock_epoch":2}"#;
    let frames = parse_schema3_frames_jsonl(frames_jsonl).expect("schema 3 frames must parse");
    assert_eq!(frames.len(), 1, "one preroll frame expected");
    assert_eq!(
        frames[0].presentation,
        LegacyFramePresentation::Preroll,
        "frame 0 must be decoded as Preroll"
    );
    assert_eq!(
        frames[0].boot_segment.as_deref(),
        Some("2"),
        "numeric clock_epoch must normalize to string boot_segment"
    );

    let logs_jsonl = concat!(
        r#"{"record_id":"10","clock_epoch":2,"generation":1,"epoch_ns":"1700000000000000000","elapsed_ns":"10500000000","window_ns":"500000000","uncertainty_ns":"10000","uid":1000,"pid":123,"tid":124,"priority":4,"tag":"Main","message":"hello","app_membership":true}"#,
        "\n",
        r#"{"record_id":"11","clock_epoch":3,"generation":1,"window_ns":null,"uncertainty_ns":"9223372036854775807","message":"other boot","app_membership":null}"#
    );
    let logs = parse_schema4_logs_jsonl(logs_jsonl).expect("schema 3 logs must parse");
    assert_eq!(logs.len(), 2, "two log rows expected");
    assert_eq!(logs[0].t_ns, Some(500_000_000), "window_ns maps to t_ns");
    assert_eq!(
        logs[0].uncertainty,
        LegacyUncertainty::Known(10_000),
        "uncertainty_ns maps to Known"
    );
    assert_eq!(
        logs[1].uncertainty,
        LegacyUncertainty::UnknownSentinel,
        "Long.MAX_VALUE maps to UnknownSentinel"
    );
}
