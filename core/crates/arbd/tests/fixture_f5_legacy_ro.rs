//! Shared fixture F5: OS / non-ASCII & space path (`録画 資料/端末 A`) + legacy schema 3/4
//! strict read-only preservation across open -> seek -> export-to-separate-destination ->
//! refused `trash_original` -> full relative-path & SHA-256 tree verification.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use arb_media::codec::{DecodedFrame, EncodeParams, OpenH264Backend, PixelFormat};
use arb_media::mp4::{Mp4VideoTrack, mux_raw_h264_to_mp4};
use arb_media::playback::GopLruCache;
use arb_media::trim::trim_video;
use arb_storage::bundle_io::{BundleFormat, open_bundle};
use arb_storage::catalog::Catalog;
use arb_storage::sha256_hex;
use arbd::capture::FaultInjection;
use arbd::error::DaemonError;
use arbd::inspect_export::export_share_mp4;
use arbd::media_engine::{ExportContext, export_trim};
use arbd::protocol::{ExportShareMp4Params, ExportTrimParams};
use tempfile::TempDir;

fn repo_schema4_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures/legacy-schema4/replay-trim-parity")
        .join("replay-20240101-000000-manual-feedface")
}

/// Captures `(relative_path -> (byte_len, sha256_hex))` for every file under `root`.
fn snapshot_tree_digests(root: &Path) -> BTreeMap<PathBuf, (u64, String)> {
    let mut map = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read dir for tree snapshot") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .expect("strip root prefix")
                    .to_path_buf();
                let bytes = fs::read(&path).expect("read file bytes");
                let len = u64::try_from(bytes.len()).expect("len fits u64");
                map.insert(rel, (len, sha256_hex(&bytes)));
            }
        }
    }
    map
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create dst dir");
    for entry in fs::read_dir(src).expect("read src dir") {
        let entry = entry.expect("src entry");
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path);
        } else {
            fs::copy(&src_path, &dst_path).expect("copy fixture file");
        }
    }
}

/// Generates a minimal, valid legacy Schema 3 (`v0.2.0`) bundle with a real H.264 `video-001.mp4`,
/// `frames.jsonl`, `logcat.log`, and `session.json` inside `dst`.
fn synthesize_schema3_bundle(dst: &Path) {
    fs::create_dir_all(dst).expect("create schema3 dir");
    let video_path = dst.join("video-001.mp4");
    let backend = OpenH264Backend::new();
    let frames: Vec<DecodedFrame> = (0..12_u64)
        .map(|i| {
            let mut data = vec![128_u8; 64 * 64 * 3 / 2];
            let luma = u8::try_from(40 + (i * 15) % 180).expect("luma fits u8");
            data[..64 * 64].fill(luma);
            DecodedFrame {
                frame_index: i,
                width: 64,
                height: 64,
                format: PixelFormat::Yuv420p,
                data,
            }
        })
        .collect();
    let packets = backend
        .encode_yuv420p_sequence(
            &frames,
            6,
            33_333_333,
            EncodeParams {
                bitrate_bps: 200_000,
                max_fps: 30,
            },
        )
        .expect("encode schema3 video");
    mux_raw_h264_to_mp4(&video_path, 64, 64, 90_000, 3_000, &packets)
        .expect("mux schema3 video-001.mp4");

    let mut frames_jsonl = String::new();
    for i in 0..12_u64 {
        use std::fmt::Write as _;
        let t_ns = i * 33_333_333;
        let elapsed_ns = 10_000_000_000 + t_ns;
        let pts_us = i * 33_333;
        let end_us = pts_us + 33_333;
        let is_key = i % 6 == 0;
        let _ = writeln!(
            frames_jsonl,
            r#"{{"part":"video-001.mp4","segment_index":0,"sample_index":{i},"source_key":{is_key},"output_key":{is_key},"source_pts_us":"{pts_us}","media_pts_us":"{pts_us}","movie_pts_us":"{pts_us}","movie_end_us":"{end_us}","elapsed_ns":"{elapsed_ns}","window_ns":"{t_ns}","uncertainty_ns":"1000","clock_alignment_known":true,"presented":true,"preroll":false,"clock_epoch":1}}"#,
        );
    }
    let frames_path = dst.join("frames.jsonl");
    fs::write(&frames_path, &frames_jsonl).expect("write frames.jsonl");

    let video_sha = sha256_hex(&fs::read(&video_path).expect("read schema3 video"));
    let frames_sha = sha256_hex(frames_jsonl.as_bytes());
    let session_json = format!(
        r#"{{
  "schema": 3,
  "complete": true,
  "save_id": "f5-schema3-legacy",
  "capture_mode": "VIDEO_AND_LOGS",
  "window_clock_known": true,
  "window_start_ns": "10000000000",
  "window_end_ns": "10400000000",
  "window_end_uncertainty_ns": "1000000",
  "replay_seconds": 30,
  "created_at": "2026-11-10T02:49:18Z",
  "video_movie_duration_us": "400000",
  "parts": [
    {{
      "file": "video-001.mp4",
      "segment_index": 0,
      "generation": 1,
      "movie_start_us": "0",
      "movie_end_us": "400000",
      "duration_us": "400000",
      "window_start_ns": "0",
      "window_end_ns": "400000000",
      "clock_alignment_known": true,
      "preroll_samples": 0
    }}
  ],
  "files_sha256": {{
    "video-001.mp4": "{video_sha}",
    "frames.jsonl": "{frames_sha}"
  }}
}}"#
    );
    fs::write(dst.join("session.json"), session_json).expect("write session.json");
}

#[derive(Clone, Copy)]
enum TrimAnchorExpectation {
    Anchored,
    Unanchored,
}

fn verify_legacy_share_and_trash_refusal(
    bundle_dir: &Path,
    ctx: &ExportContext<'_>,
    export_dest_dir: &Path,
    share_out_path: &Path,
) {
    let share_resp = export_share_mp4(
        ctx,
        bundle_dir,
        None,
        export_dest_dir,
        share_out_path,
        &ExportShareMp4Params {
            keep_bundle: false,
            bundle_id: bundle_dir.to_string_lossy().into_owned(),
            in_frame_index: None,
            out_frame_index: None,
            dest_dir: Some(export_dest_dir.to_path_buf()),
            output_path: Some(share_out_path.to_path_buf()),
            holder_pid: None,
            burn_taps: false,
            trash_original: false,
        },
    )
    .expect("export_share_mp4 from legacy RO bundle must succeed");
    assert!(
        share_resp.share_mp4_path.is_file(),
        "exported share.mp4 must exist"
    );

    let refused = export_share_mp4(
        ctx,
        bundle_dir,
        None,
        export_dest_dir,
        &export_dest_dir.join("must-not-be-created.mp4"),
        &ExportShareMp4Params {
            keep_bundle: false,
            bundle_id: bundle_dir.to_string_lossy().into_owned(),
            in_frame_index: None,
            out_frame_index: None,
            dest_dir: Some(export_dest_dir.to_path_buf()),
            output_path: None,
            holder_pid: None,
            burn_taps: false,
            trash_original: true,
        },
    );
    assert!(
        matches!(refused, Err(DaemonError::Conflict(_))),
        "trash_original on legacy RO bundle must fail with Conflict, got {refused:?}"
    );
}

fn exercise_legacy_open_seek_and_export(
    bundle_dir: &Path,
    expected_format: BundleFormat,
    expected_min_frames: u64,
    anchor_expectation: TrimAnchorExpectation,
    ctx: &ExportContext<'_>,
    export_dest_dir: &Path,
    share_out_path: &Path,
) {
    let reader = open_bundle(bundle_dir).expect("open legacy bundle under non-ASCII/space path");
    assert!(
        reader.is_legacy(),
        "legacy bundle must report is_legacy=true"
    );
    assert_eq!(
        reader.manifest_summary().format,
        expected_format,
        "detected bundle format must match expected schema"
    );

    let video_path = reader
        .video_path()
        .expect("legacy video bundle must expose video_path");
    let track = Mp4VideoTrack::open(&video_path).expect("open legacy mp4 track");
    assert!(
        track.frame_count() >= expected_min_frames,
        "legacy track frame_count {} must be >= {expected_min_frames}",
        track.frame_count()
    );

    let mut cache = GopLruCache::new(2, 32 * 1024 * 1024);
    let last_frame = track.frame_count() - 1;
    for idx in [0_u64, last_frame / 2, last_frame] {
        let (decoded, _) = cache
            .fetch_frame(&track, idx, PixelFormat::Rgba8, ctx.backend)
            .expect("seek and decode legacy frame");
        assert!(
            decoded.width > 0 && decoded.height > 0,
            "decoded legacy frame {idx} must have positive dimensions"
        );
    }

    let trim_out_idx = last_frame.min(8);
    let direct_trim_mp4 = export_dest_dir.join(format!(
        "trimmed-{}.mp4",
        bundle_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("legacy")
    ));
    let trim_video_report = trim_video(&video_path, &direct_trim_mp4, 2, trim_out_idx, ctx.backend)
        .expect("trim_video from legacy RO bundle to separate dir must succeed");
    assert_eq!(
        trim_video_report.output_frame_count,
        trim_out_idx - 2 + 1,
        "trimmed video frame count must match inclusive range"
    );

    let trim_res = export_trim(
        ctx,
        bundle_dir,
        None,
        export_dest_dir,
        &ExportTrimParams {
            bundle_id_or_path: bundle_dir.to_string_lossy().into_owned(),
            in_frame_index: 2,
            out_frame_index: trim_out_idx,
            dest_dir: Some(export_dest_dir.to_path_buf()),
            holder_pid: None,
            burn_taps: false,
            include_share_mp4: false,
            share_output_path: None,
            trash_original: false,
        },
    );
    match anchor_expectation {
        TrimAnchorExpectation::Anchored => {
            let trim_resp = trim_res.expect("anchored legacy export_trim must succeed");
            assert!(
                trim_resp.bundle_path.is_dir(),
                "exported trimmed WORM v2 bundle must exist at separate destination"
            );
        }
        TrimAnchorExpectation::Unanchored => {
            assert!(
                trim_res.is_err(),
                "unanchored legacy fixture must reject boottime trim cleanly"
            );
        }
    }

    verify_legacy_share_and_trash_refusal(bundle_dir, ctx, export_dest_dir, share_out_path);
}

#[test]
fn f5_non_ascii_space_path_legacy_schema3_and_schema4_stay_byte_identical() {
    let tmp = TempDir::new().expect("tempdir");
    let non_ascii_root = tmp.path().join("録画 資料").join("端末 A");
    let schema4_copy = non_ascii_root.join("replay-schema4-feedface");
    let schema3_dir = non_ascii_root.join("replay-schema3-v020");

    copy_dir_recursive(&repo_schema4_fixture_dir(), &schema4_copy);
    synthesize_schema3_bundle(&schema3_dir);

    let before_snapshot = snapshot_tree_digests(&non_ascii_root);
    assert!(
        before_snapshot.len() >= 10,
        "expected both schema4 and schema3 files in snapshot, got {}",
        before_snapshot.len()
    );

    let data_dir = tmp.path().join("daemon data");
    let export_dest = tmp.path().join("書き出し 先").join("出力 A");
    fs::create_dir_all(&data_dir).expect("create data dir");
    fs::create_dir_all(&export_dest).expect("create export dest");

    let catalog =
        Catalog::open(data_dir.join("catalog.sqlite"), &export_dest).expect("open catalog");
    let backend = OpenH264Backend::new();
    let faults = FaultInjection::default();
    let ctx = ExportContext {
        data_dir: &data_dir,
        catalog: &catalog,
        backend: &backend,
        faults: &faults,
    };

    exercise_legacy_open_seek_and_export(
        &schema4_copy,
        BundleFormat::LegacySchema4,
        24,
        TrimAnchorExpectation::Unanchored,
        &ctx,
        &export_dest,
        &export_dest.join("schema4-share.mp4"),
    );
    exercise_legacy_open_seek_and_export(
        &schema3_dir,
        BundleFormat::LegacySchema3,
        12,
        TrimAnchorExpectation::Unanchored,
        &ctx,
        &export_dest,
        &export_dest.join("schema3-share.mp4"),
    );

    // Also verify an anchored Schema 4 copy (with explicit `device_boottime` origin) completes
    // full `export_trim` to a separate WORM v2 bundle without mutating any legacy source file.
    let schema4_anchored = non_ascii_root.join("replay-schema4-anchored");
    copy_dir_recursive(&repo_schema4_fixture_dir(), &schema4_anchored);
    let replay_path = schema4_anchored.join("replay.json");
    let mut replay_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&replay_path).expect("read replay.json"))
            .expect("parse replay.json");
    if let Some(tb) = replay_json
        .get_mut("timebase")
        .and_then(|v| v.as_object_mut())
    {
        tb.insert(
            "domain".to_owned(),
            serde_json::Value::String("device_boottime".to_owned()),
        );
        tb.insert("window_known".to_owned(), serde_json::Value::Bool(true));
        tb.insert(
            "window_start_elapsed_ns".to_owned(),
            serde_json::Value::from(100_000_000_000_i64),
        );
    }
    fs::write(
        &replay_path,
        serde_json::to_vec_pretty(&replay_json).expect("serialize anchored replay.json"),
    )
    .expect("write anchored replay.json");

    let before_anchored = snapshot_tree_digests(&schema4_anchored);
    exercise_legacy_open_seek_and_export(
        &schema4_anchored,
        BundleFormat::LegacySchema4,
        24,
        TrimAnchorExpectation::Anchored,
        &ctx,
        &export_dest,
        &export_dest.join("schema4-anchored-share.mp4"),
    );
    let after_anchored = snapshot_tree_digests(&schema4_anchored);
    assert_eq!(
        after_anchored, before_anchored,
        "anchored legacy schema4 copy under `録画 資料/端末 A` must remain 100% byte-identical"
    );

    // Exclude the later-created `replay-schema4-anchored` subdir when comparing against the
    // initial snapshot of `schema4_copy` + `schema3_dir`.
    let mut after_initial = snapshot_tree_digests(&non_ascii_root);
    after_initial.retain(|rel, _| !rel.starts_with("replay-schema4-anchored"));
    assert_eq!(
        after_initial, before_snapshot,
        "legacy schema 3/4 tree under `録画 資料/端末 A` must remain 100% byte-identical (same relative paths, sizes, and SHA-256 digests)"
    );
}
