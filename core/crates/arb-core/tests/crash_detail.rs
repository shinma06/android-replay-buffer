//! #589: shared crash-detail / process-event contract table and their properties.
use arb_core::{
    analytics::{
        CrashMarker, CrashStackDetail, MAX_STACK_TOP_LINES, ProcessEventKind, crash_stack_rows,
        enrich_crash_marker, extract_process_events,
    },
    bundle::{LogRecord, RecordBucket},
    time::{TimeAlignment, TimestampSemantics},
};
use proptest::prelude::*;
use serde_json::{Value, json};

fn record(
    seq: u64,
    tag: &str,
    uid: Option<u32>,
    pid: i32,
    ms: i64,
    boot: &str,
    message: String,
) -> LogRecord {
    LogRecord {
        seq,
        boot_id: Some(boot.into()),
        header_epoch_ns: None,
        device_read_boottime_ns: None,
        pc_ingest_cont_ns: 0,
        alignment: TimeAlignment::Anchored {
            boot_id: boot.into(),
            boottime_ns: ms * 1_000_000,
            half_width_ns: 0,
            semantics: TimestampSemantics::LogcatHeaderWall,
        },
        bucket: RecordBucket::Main,
        level: Some("E".into()),
        tag: Some(tag.into()),
        pid: Some(pid),
        tid: None,
        uid,
        package_name: None,
        message,
    }
}

fn table() -> Value {
    serde_json::from_str(include_str!(
        "../../../../docs/specs/crash-detail-table.json"
    ))
    .expect("crash-detail-table.json is JSON")
}

fn logs_of(row: &Value) -> Vec<LogRecord> {
    row["logs"]
        .as_array()
        .expect("logs")
        .iter()
        .map(|l| {
            record(
                l["seq"].as_u64().expect("seq"),
                l["tag"].as_str().expect("tag"),
                l["uid"].as_u64().map(|u| u32::try_from(u).expect("uid")),
                i32::try_from(l["pid"].as_i64().expect("pid")).expect("pid"),
                l["ms"].as_i64().expect("ms"),
                l["boot"].as_str().expect("boot"),
                l["message"].as_str().expect("message").into(),
            )
        })
        .collect()
}

fn marker_of(m: &Value) -> CrashMarker {
    let package = m["package_name"].as_str().expect("package").to_owned();
    CrashMarker {
        boottime_ns: Some(m["ms"].as_i64().expect("ms") * 1_000_000),
        frame_index_hint: None,
        process_name: package.clone(),
        package_name: package,
        pid: Some(i32::try_from(m["pid"].as_i64().expect("pid")).expect("pid")),
        exception_class: "java.lang.RuntimeException".into(),
        message: String::new(),
        source_log_seq: m["source_log_seq"].as_u64().expect("seq"),
    }
}

#[test]
fn pdd_contract_tables_crash_detail() {
    let table = table();
    let crash_rows = table["crashRows"].as_array().expect("crashRows");
    assert!(!crash_rows.is_empty(), "crash rows present");
    for row in crash_rows {
        let detail = enrich_crash_marker(&marker_of(&row["marker"]), &logs_of(row));
        assert_eq!(json!(detail), row["expected"], "{}", row["id"]);
    }
    let process_rows = table["processRows"].as_array().expect("processRows");
    assert!(!process_rows.is_empty(), "process rows present");
    for row in process_rows {
        let crashes: Vec<CrashMarker> = row["crashes"]
            .as_array()
            .expect("crashes")
            .iter()
            .map(marker_of)
            .collect();
        let events = extract_process_events(&logs_of(row), &crashes);
        let expected = row["expected"].as_array().expect("expected");
        assert_eq!(events.len(), expected.len(), "{}: {events:?}", row["id"]);
        for (event, want) in events.iter().zip(expected) {
            let got = json!(event);
            for (key, value) in want.as_object().expect("object") {
                assert_eq!(&got[key], value, "{} {key}", row["id"]);
            }
            assert!(
                event.offset_ms.is_none() && event.frame_index_hint.is_none(),
                "placed by the reader"
            );
        }
    }
}

const TAGS: [&str; 6] = [
    "AndroidRuntime",
    "ActivityManager",
    "lowmemorykiller",
    "am_kill",
    "am_proc_died",
    "am_anr",
];

fn fragment(pid: i32) -> impl Strategy<Value = String> {
    prop_oneof![
        Just("FATAL EXCEPTION: main".to_owned()),
        Just(format!("Process: com.example.app, PID: {pid}")),
        (0_u8..4).prop_map(move |n| format!("at com.example.app.F{n}.run(F.kt:{n})")),
        (0_u8..4).prop_map(move |n| format!("\tat android.os.L{n}.loop(L.java:{n})")),
        Just("Caused by: java.lang.IllegalStateException: x".to_owned()),
        Just(format!(
            "Force stopping com.example.app appid=1 user=0: from pid {pid}"
        )),
        Just(format!(
            "Killing {pid}:com.example.app/u0a1 (adj 0): stop com.example.app due to x"
        )),
        Just(format!(
            "Process com.example.app (pid {pid}) has died: fg TOP"
        )),
        Just(format!("ANR in com.example.app\nPID: {pid}\nReason: x")),
        Just(format!(
            "Kill 'com.example.app' ({pid}), uid 10123, oom_score_adj 900 to free 1kB rss"
        )),
        Just(format!("[0,{pid},com.example.app,900,lmk]")),
        any::<String>(),
    ]
}

fn arbitrary_logs(
    uids: impl Strategy<Value = Option<u32>> + Clone + 'static,
) -> impl Strategy<Value = Vec<LogRecord>> {
    proptest::collection::vec(
        (
            0_usize..TAGS.len(),
            uids,
            40_i32..44,
            0_i64..6_000,
            any::<bool>(),
        ),
        0..40,
    )
    .prop_flat_map(|rows| {
        let parts: Vec<_> = rows
            .into_iter()
            .map(|(tag, uid, pid, ms, other_boot)| {
                proptest::collection::vec(fragment(pid), 1..3)
                    .prop_map(move |lines| (tag, uid, pid, ms, other_boot, lines.join("\n")))
            })
            .collect();
        parts
    })
    .prop_map(|rows| {
        rows.into_iter()
            .zip(1_u64..)
            .map(|((tag, uid, pid, ms, other_boot, message), seq)| {
                record(
                    seq,
                    TAGS[tag],
                    uid,
                    pid,
                    ms,
                    if other_boot { "b0" } else { "b1" },
                    message,
                )
            })
            .collect()
    })
}

proptest! {
    /// The stack is read only from the marker's own PID, never panics, and keeps the app frame.
    #[test]
    fn crash_stack_comes_only_from_the_crashing_pid(
        logs in arbitrary_logs(prop_oneof![Just(None), Just(Some(1_000)), (10_000_u32..20_000).prop_map(Some)]),
        pid in 40_i32..44,
        pick in any::<prop::sample::Index>(),
        ms in 0_i64..6_000,
    ) {
        let source_log_seq = if logs.is_empty() { 0 } else { logs[pick.index(logs.len())].seq };
        let marker = CrashMarker {
            boottime_ns: Some(ms * 1_000_000),
            frame_index_hint: None,
            process_name: "com.example.app".into(),
            package_name: "com.example.app".into(),
            pid: Some(pid),
            exception_class: "E".into(),
            message: String::new(),
            source_log_seq,
        };
        let rows = crash_stack_rows(&marker, &logs);
        for &i in &rows {
            prop_assert_eq!(logs[i].pid, Some(pid));
            prop_assert_eq!(logs[i].tag.as_deref(), Some("AndroidRuntime"));
        }
        let detail = enrich_crash_marker(&marker, &logs);
        let own_lines: Vec<&str> = rows.iter().flat_map(|&i| logs[i].message.lines().map(str::trim)).collect();
        if let Some(top) = &detail.stack_top {
            prop_assert!(!top.is_empty() && top.len() <= MAX_STACK_TOP_LINES);
            for line in top {
                let own = own_lines.iter().any(|l| {
                    *l == line.as_str() || line.strip_suffix('…').is_some_and(|head| l.starts_with(head))
                });
                prop_assert!(own, "{line} is from another row");
            }
        }
        if let Some(app) = &detail.first_app_frame {
            prop_assert!(detail.stack_top.as_ref().is_some_and(|t| t.contains(app)));
        }
        if rows.is_empty() {
            prop_assert_eq!(detail, CrashStackDetail::default());
        }
    }

    /// App UIDs cannot create process events, whatever they log under any system tag.
    #[test]
    fn app_uids_never_create_process_events(logs in arbitrary_logs((10_000_u32..20_000).prop_map(Some))) {
        prop_assert!(extract_process_events(&logs, &[]).is_empty());
    }

    /// Every event comes from a trusted row and is never a crash; extraction never panics.
    #[test]
    fn process_events_come_from_trusted_rows(
        logs in arbitrary_logs(prop_oneof![Just(None), Just(Some(0)), Just(Some(1_000)), Just(Some(1_069)), (10_000_u32..20_000).prop_map(Some)]),
    ) {
        for event in extract_process_events(&logs, &[]) {
            prop_assert!(!event.is_crash);
            let row = logs.iter().find(|l| l.seq == event.source_log_seq).expect("source row");
            let trusted = match row.uid {
                Some(0 | 1_000) => true,
                Some(1_069) => row.tag.as_deref() == Some("lowmemorykiller"),
                None => row.tag.as_deref().is_some_and(|t| t.starts_with("am_")),
                Some(_) => false,
            };
            prop_assert!(trusted, "{row:?} -> {event:?}");
            if row.uid == Some(1_069) {
                prop_assert_eq!(event.kind, ProcessEventKind::LmkKill);
            }
        }
    }
}
