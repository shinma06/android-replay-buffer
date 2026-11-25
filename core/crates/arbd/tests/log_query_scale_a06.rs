//! P1 A06 scale & pagination regression tests for `inspect_export::query_logs_filtered_page`.
//!
//! Exercises a real 100,000-row WORM v2 bundle (`timeline.sqlite`) with:
//! - first, middle (multi-byte Japanese UTF-8), and last-row matches,
//! - same-boot and cross-boot PID reuse (`Start proc` switching PID 4200 from oldapp -> target),
//! - a large single-line payload boundary (256 KiB UTF-8),
//! - multi-page traversal asserting zero duplicates, zero gaps, `resolver_cache_hit == true`
//!   on page 2+, and O(N) total candidate rows scanned (not O(N²/pageSize)),
//! - automatic cache invalidation when the bundle files' size / mtime change on disk.

use std::collections::BTreeSet;
use std::path::Path;

use arb_core::bundle::{
    BundleKind, DeviceInfo, EpochSpan, LogCompleteness, LogRecord, LogcatStreamMeta, Provenance,
    RecordBucket, RequestedWindow, StreamStatus, StreamsSummary, UnanchoredSummary,
};
use arb_core::time::{TimeAlignment, TimestampSemantics};
use arb_storage::bundle_io::{V2BundleDraft, V2BundleWriter, open_bundle};
use arbd::inspect_export::{query_logs_filtered, query_logs_filtered_page};
use arbd::protocol::TimelineQueryParams;
use tempfile::TempDir;

const TOTAL_ROWS: u64 = 100_000;
const TARGET_PKG: &str = "com.example.target";
const OLD_PKG: &str = "com.example.oldapp";
const OTHER_PKG: &str = "com.example.other";
const NOISE_PKG: &str = "com.example.noise";
const REUSED_PID: i32 = 4200;
const TARGET_MAIN_PID: i32 = 5100;
const NOISE_PID: i32 = 6200;
const BASE_BT_NS: i64 = 100_000_000_000;
const HUGE_LINE_BYTES: usize = 256 * 1024;

struct LogFixtureOracle {
    expected_target_seqs: Vec<u64>,
    huge_row_seq: u64,
    huge_row_len: usize,
    japanese_row_seq: u64,
    last_row_seq: u64,
}

struct LogRowSpec<'a> {
    seq: u64,
    boot_id: &'a str,
    bt_ns: i64,
    level: &'a str,
    tag: &'a str,
    pid: i32,
    uid: Option<u32>,
    package_name: Option<&'a str>,
    message: String,
}

fn make_log_row(spec: LogRowSpec<'_>) -> LogRecord {
    LogRecord {
        seq: spec.seq,
        boot_id: Some(spec.boot_id.to_owned()),
        header_epoch_ns: Some(1_730_000_000_000_000_000 + spec.bt_ns),
        device_read_boottime_ns: Some(spec.bt_ns),
        pc_ingest_cont_ns: spec.bt_ns,
        alignment: TimeAlignment::Anchored {
            boot_id: spec.boot_id.to_owned(),
            boottime_ns: spec.bt_ns,
            half_width_ns: 1_000_000,
            semantics: TimestampSemantics::LogcatHeaderWall,
        },
        bucket: RecordBucket::Main,
        level: Some(spec.level.to_owned()),
        tag: Some(spec.tag.to_owned()),
        pid: Some(spec.pid),
        tid: Some(spec.pid),
        uid: spec.uid,
        package_name: spec.package_name.map(str::to_owned),
        message: spec.message,
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "deterministic 100k-row fixture builder with explicit oracle tracking"
)]
fn build_100k_log_bundle(bundle_dir: &Path, bundle_id: &str) -> LogFixtureOracle {
    let mut logs = Vec::with_capacity(usize::try_from(TOTAL_ROWS).expect("fits usize"));
    let mut expected_target_seqs = Vec::new();
    let japanese_row_seq = 50_005_u64;
    let huge_row_seq = 75_000_u64;
    let last_row_seq = TOTAL_ROWS;
    let mut huge_row_len = 0_usize;

    for seq in 1..=TOTAL_ROWS {
        let bt_ns = BASE_BT_NS + i64::try_from(seq * 1_000).expect("bt fits i64");
        let spec = match seq {
            1 => {
                expected_target_seqs.push(seq);
                LogRowSpec {
                    seq,
                    boot_id: "boot-1",
                    bt_ns,
                    level: "I",
                    tag: "TargetInit",
                    pid: TARGET_MAIN_PID,
                    uid: Some(10_200),
                    package_name: Some(TARGET_PKG),
                    message: "first row target startup".to_owned(),
                }
            }
            10 => LogRowSpec {
                seq,
                boot_id: "boot-1",
                bt_ns,
                level: "I",
                tag: "ActivityManager",
                pid: 1000,
                uid: Some(1000),
                package_name: Some("system_server"),
                message: format!("Start proc {REUSED_PID}:{OLD_PKG}/u0a100 for activity"),
            },
            20 | 10_000 | 40_000 => LogRowSpec {
                seq,
                boot_id: "boot-1",
                bt_ns,
                level: "D",
                tag: "Worker",
                pid: REUSED_PID,
                uid: None,
                package_name: None,
                message: format!("oldapp work before pid reuse seq={seq}"),
            },
            50_000 => LogRowSpec {
                seq,
                boot_id: "boot-1",
                bt_ns,
                level: "I",
                tag: "ActivityManager",
                pid: 1000,
                uid: Some(1000),
                package_name: Some("system_server"),
                message: format!("Start proc {REUSED_PID}:{TARGET_PKG}/u0a200 for activity"),
            },
            s if s == japanese_row_seq => {
                expected_target_seqs.push(seq);
                LogRowSpec {
                    seq,
                    boot_id: "boot-1",
                    bt_ns,
                    level: "E",
                    tag: "決済処理",
                    pid: REUSED_PID,
                    uid: None,
                    package_name: None,
                    message: "日本語ログ: 決済エラー確認 端末A 境界テスト".to_owned(),
                }
            }
            s if s == huge_row_seq => {
                expected_target_seqs.push(seq);
                let prefix = "巨大行先頭:";
                let suffix = ":巨大行末尾";
                let fill_len = HUGE_LINE_BYTES.saturating_sub(prefix.len() + suffix.len());
                let mut msg = String::with_capacity(HUGE_LINE_BYTES + 16);
                msg.push_str(prefix);
                for _ in 0..fill_len {
                    msg.push('A');
                }
                msg.push_str(suffix);
                huge_row_len = msg.len();
                LogRowSpec {
                    seq,
                    boot_id: "boot-1",
                    bt_ns,
                    level: "W",
                    tag: "HugeLine",
                    pid: REUSED_PID,
                    uid: None,
                    package_name: None,
                    message: msg,
                }
            }
            90_000 => LogRowSpec {
                seq,
                boot_id: "boot-2",
                bt_ns,
                level: "I",
                tag: "ActivityManager",
                pid: 1000,
                uid: Some(1000),
                package_name: Some("system_server"),
                message: format!("Start proc {REUSED_PID}:{OTHER_PKG}/u0a300 for activity"),
            },
            90_010 => LogRowSpec {
                seq,
                boot_id: "boot-2",
                bt_ns,
                level: "I",
                tag: "CrossBootWorker",
                pid: REUSED_PID,
                uid: None,
                package_name: None,
                message: "other app after reboot on reused pid".to_owned(),
            },
            s if s == last_row_seq => {
                expected_target_seqs.push(seq);
                LogRowSpec {
                    seq,
                    boot_id: "boot-1",
                    bt_ns,
                    level: "E",
                    tag: "FinalOnlyTag",
                    pid: REUSED_PID,
                    uid: None,
                    package_name: None,
                    message: "最終行のみ一致マーカー".to_owned(),
                }
            }
            s if s % 50 == 0 && s != 50_000 && s != 75_000 && s != 90_000 && s != 100_000 => {
                expected_target_seqs.push(seq);
                let pid = if s > 50_000 {
                    REUSED_PID
                } else {
                    TARGET_MAIN_PID
                };
                LogRowSpec {
                    seq,
                    boot_id: "boot-1",
                    bt_ns,
                    level: "I",
                    tag: "TargetTick",
                    pid,
                    uid: None,
                    package_name: None,
                    message: format!("target tick seq={seq}"),
                }
            }
            _ => LogRowSpec {
                seq,
                boot_id: "boot-1",
                bt_ns,
                level: "D",
                tag: "Noise",
                pid: NOISE_PID,
                uid: Some(10_999),
                package_name: Some(NOISE_PKG),
                message: "noise".to_owned(),
            },
        };
        logs.push(make_log_row(spec));
    }

    let draft = V2BundleDraft {
        bundle_kind: BundleKind::LogsOnly,
        bundle_id: bundle_id.to_owned(),
        created_at_utc: "2026-11-25T00:00:00Z".to_owned(),
        provenance: Provenance::Original,
        device: DeviceInfo {
            serial: "emulator-5554".to_owned(),
            model: "Pixel".to_owned(),
            api_level: 34,
            is_emulator: true,
        },
        requested_window: RequestedWindow::RealElapsedSec {
            duration_sec: 300,
            trigger_pc_wall_utc: "2026-11-25T00:00:00Z".to_owned(),
        },
        epochs: vec![EpochSpan {
            boot_id: "boot-1".to_owned(),
            start_boottime_ns: BASE_BT_NS,
            end_boottime_ns: BASE_BT_NS + i64::try_from(TOTAL_ROWS * 1_000).expect("i64"),
        }],
        streams: StreamsSummary {
            video: None,
            audio: None,
            logcat: Some(LogcatStreamMeta {
                status: StreamStatus::Present,
                completeness: LogCompleteness::VerifiedNoOverflow,
                overflow_events: 0,
                tail_may_be_incomplete: false,
            }),
            input: None,
        },
        unanchored_summary: UnanchoredSummary {
            included_count: 0,
            truncated_count: 0,
        },
        video_mp4: None,
        audio_m4a: None,
        share_mp4: None,
        frames: Vec::new(),
        discontinuities: Vec::new(),
        logs,
        taps: Vec::new(),
        clock_anchors: Vec::new(),
    };
    V2BundleWriter::write(bundle_dir, "job-a06-100k", &draft).expect("write 100k log bundle");

    LogFixtureOracle {
        expected_target_seqs,
        huge_row_seq,
        huge_row_len,
        japanese_row_seq,
        last_row_seq,
    }
}

fn query_params(bundle_dir: &Path, after_seq: Option<u64>, package: &str) -> TimelineQueryParams {
    TimelineQueryParams {
        bundle_id_or_path: bundle_dir.to_string_lossy().into_owned(),
        bucket: None,
        start_ns: None,
        end_ns: None,
        after_seq,
        limit: Some(500),
        start_frame: None,
        end_frame: None,
        package: Some(package.to_owned()),
        tag: None,
        level: None,
        keyword: None,
    }
}

fn verify_compat_and_final_tag(bundle_dir: &Path, oracle: &LogFixtureOracle, page_size: usize) {
    let reader = open_bundle(bundle_dir).expect("reopen for compat check");
    let compat_first = query_logs_filtered(
        reader.as_ref(),
        Some(RecordBucket::Main),
        &query_params(bundle_dir, None, TARGET_PKG),
        page_size,
    )
    .expect("compat query_logs_filtered");
    let compat_seqs: Vec<u64> = compat_first.iter().map(|r| r.seq).collect();
    assert_eq!(
        compat_seqs,
        oracle.expected_target_seqs[..page_size],
        "query_logs_filtered must stay 100% consistent with query_logs_filtered_page"
    );

    let mut last_tag_params = query_params(bundle_dir, None, TARGET_PKG);
    last_tag_params.tag = Some("FinalOnlyTag".to_owned());
    let last_only = query_logs_filtered_page(
        reader.as_ref(),
        Some(RecordBucket::Main),
        &last_tag_params,
        50,
    )
    .expect("query final-only tag");
    assert!(
        last_only.resolver_cache_hit,
        "tag+package query on same bundle must reuse cached resolver"
    );
    assert!(
        !last_only.has_more,
        "single matching last row must report has_more=false"
    );
    assert_eq!(
        last_only.logs.iter().map(|r| r.seq).collect::<Vec<_>>(),
        vec![oracle.last_row_seq],
        "last-row-only tag must return seq=100,000"
    );
}

#[test]
fn a06_100k_log_package_pagination_is_complete_non_quadratic_and_handles_pid_reuse() {
    let tmp = TempDir::new().expect("tempdir");
    let bundle_dir = tmp.path().join("bundle-a06-100k");
    let oracle = build_100k_log_bundle(&bundle_dir, "bundle-a06-100k");
    let page_size = 500_usize;

    let mut collected_seqs = Vec::new();
    let mut saw_japanese = false;
    let mut saw_huge = false;
    let mut saw_last = false;
    let mut total_candidate_rows_scanned = 0_usize;
    let mut page_count = 0_usize;
    let mut after_seq = None;

    loop {
        let reader = open_bundle(&bundle_dir).expect("open 100k bundle");
        let params = query_params(&bundle_dir, after_seq, TARGET_PKG);
        let page = query_logs_filtered_page(
            reader.as_ref(),
            Some(RecordBucket::Main),
            &params,
            page_size,
        )
        .expect("query page");
        page_count += 1;

        assert_eq!(
            page.resolver_cache_hit,
            page_count > 1,
            "page {page_count} cache-hit state must be false only on page 1"
        );
        total_candidate_rows_scanned =
            total_candidate_rows_scanned.saturating_add(page.candidate_rows_scanned);

        for row in &page.logs {
            collected_seqs.push(row.seq);
            if row.seq == oracle.japanese_row_seq {
                assert_eq!(
                    row.message, "日本語ログ: 決済エラー確認 端末A 境界テスト",
                    "multi-byte Japanese message must survive pagination intact"
                );
                saw_japanese = true;
            } else if row.seq == oracle.huge_row_seq {
                assert_eq!(
                    row.message.len(),
                    oracle.huge_row_len,
                    "256 KiB single-line message must survive pagination intact"
                );
                assert!(
                    row.message.starts_with("巨大行先頭:") && row.message.ends_with(":巨大行末尾"),
                    "huge line prefix and suffix must remain intact"
                );
                saw_huge = true;
            } else if row.seq == oracle.last_row_seq {
                assert_eq!(row.tag.as_deref(), Some("FinalOnlyTag"), "last-row tag");
                saw_last = true;
            }
        }

        if !page.has_more {
            break;
        }
        assert_eq!(page.logs.len(), page_size, "non-final page must be full");
        after_seq = page.next_after_seq;
        assert!(after_seq.is_some(), "non-final page must advance cursor");
    }

    assert!(
        page_count >= 4,
        "expected >= 4 pages of {page_size} rows, got {page_count}"
    );
    assert_eq!(
        collected_seqs.len(),
        collected_seqs
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .len(),
        "paginated traversal must contain zero duplicate seqs"
    );
    assert_eq!(
        collected_seqs, oracle.expected_target_seqs,
        "paginated traversal must match independent oracle with zero gaps and strict PID-reuse isolation"
    );
    assert!(saw_japanese, "multi-byte Japanese row must be returned");
    assert!(saw_huge, "huge 256 KiB line row must be returned");
    assert!(saw_last, "last row (seq=100,000) must be returned");

    let max_linear_scan = usize::try_from(TOTAL_ROWS).expect("usize") + page_count * 2_048;
    assert!(
        total_candidate_rows_scanned <= max_linear_scan,
        "total candidate rows scanned ({total_candidate_rows_scanned}) exceeded linear budget ({max_linear_scan}) across {page_count} pages"
    );

    verify_compat_and_final_tag(&bundle_dir, &oracle, page_size);
}

fn write_small_log_bundle(dir: &Path, pkg: &str, extra_msg: &str) {
    if dir.exists() {
        std::fs::remove_dir_all(dir).expect("clear previous bundle");
    }
    let logs = vec![
        make_log_row(LogRowSpec {
            seq: 1,
            boot_id: "boot-1",
            bt_ns: BASE_BT_NS + 1_000,
            level: "I",
            tag: "ActivityManager",
            pid: 1000,
            uid: Some(1000),
            package_name: Some("system_server"),
            message: format!("Start proc 7777:{pkg}/u0a50 for activity"),
        }),
        make_log_row(LogRowSpec {
            seq: 2,
            boot_id: "boot-1",
            bt_ns: BASE_BT_NS + 2_000,
            level: "I",
            tag: "AppTag",
            pid: 7777,
            uid: None,
            package_name: None,
            message: format!("payload for {pkg} {extra_msg}"),
        }),
    ];
    let draft = V2BundleDraft {
        bundle_kind: BundleKind::LogsOnly,
        bundle_id: "same-id-slot".to_owned(),
        created_at_utc: "2026-11-25T00:00:00Z".to_owned(),
        provenance: Provenance::Original,
        device: DeviceInfo {
            serial: "emu".to_owned(),
            model: "Pixel".to_owned(),
            api_level: 34,
            is_emulator: true,
        },
        requested_window: RequestedWindow::RealElapsedSec {
            duration_sec: 30,
            trigger_pc_wall_utc: "2026-11-25T00:00:00Z".to_owned(),
        },
        epochs: vec![EpochSpan {
            boot_id: "boot-1".to_owned(),
            start_boottime_ns: BASE_BT_NS,
            end_boottime_ns: BASE_BT_NS + 10_000,
        }],
        streams: StreamsSummary {
            video: None,
            audio: None,
            logcat: Some(LogcatStreamMeta {
                status: StreamStatus::Present,
                completeness: LogCompleteness::VerifiedNoOverflow,
                overflow_events: 0,
                tail_may_be_incomplete: false,
            }),
            input: None,
        },
        unanchored_summary: UnanchoredSummary {
            included_count: 0,
            truncated_count: 0,
        },
        video_mp4: None,
        audio_m4a: None,
        share_mp4: None,
        frames: Vec::new(),
        discontinuities: Vec::new(),
        logs,
        taps: Vec::new(),
        clock_anchors: Vec::new(),
    };
    V2BundleWriter::write(dir, "job-slot", &draft).expect("write small bundle");
}

#[test]
fn a06_pid_resolver_cache_invalidates_when_bundle_on_disk_changes() {
    let tmp = TempDir::new().expect("tempdir");
    let bundle_dir = tmp.path().join("mutable-slot-bundle");

    write_small_log_bundle(&bundle_dir, "com.alpha.app", "v1");
    let r1 = open_bundle(&bundle_dir).expect("open v1");
    let p1 = query_logs_filtered_page(
        r1.as_ref(),
        Some(RecordBucket::Main),
        &query_params(&bundle_dir, None, "com.alpha.app"),
        50,
    )
    .expect("query v1");
    assert_eq!(
        p1.logs.iter().map(|r| r.seq).collect::<Vec<_>>(),
        vec![2],
        "v1 must attribute pid 7777 to com.alpha.app"
    );
    let p1_second = query_logs_filtered_page(
        r1.as_ref(),
        Some(RecordBucket::Main),
        &query_params(&bundle_dir, None, "com.alpha.app"),
        50,
    )
    .expect("query v1 second time");
    assert!(
        p1_second.resolver_cache_hit,
        "second query on unchanged bundle must hit cache"
    );

    write_small_log_bundle(
        &bundle_dir,
        "com.beta.app",
        "v2-longer-content-changes-size-and-sha256",
    );
    let r2 = open_bundle(&bundle_dir).expect("open v2");
    let p2_alpha = query_logs_filtered_page(
        r2.as_ref(),
        Some(RecordBucket::Main),
        &query_params(&bundle_dir, None, "com.alpha.app"),
        50,
    )
    .expect("query v2 for old pkg");
    assert!(
        !p2_alpha.resolver_cache_hit,
        "replaced bundle must invalidate cached PID resolver"
    );
    assert_eq!(
        p2_alpha.logs,
        Vec::<LogRecord>::new(),
        "old package must not match replaced bundle"
    );
    let p2_beta = query_logs_filtered_page(
        r2.as_ref(),
        Some(RecordBucket::Main),
        &query_params(&bundle_dir, None, "com.beta.app"),
        50,
    )
    .expect("query v2 for new pkg");
    assert!(
        p2_beta.resolver_cache_hit,
        "same replaced bundle must hit freshly populated cache"
    );
    assert_eq!(
        p2_beta.logs.iter().map(|r| r.seq).collect::<Vec<_>>(),
        vec![2],
        "new package must match pid 7777 after invalidation"
    );
}
