//! Read-time crash stacks, process events and the `package` annotation of `timeline.overview`.
//!
//! Everything here is derived from the saved rows on every read (WORM bundles are never
//! rewritten). A `package` filter narrows only the error groups and noise candidates; every other
//! array keeps all of its rows in the same order and gains `matches_package` (taps:
//! `foreground_package`), so another app's crash or a screen change is never silently hidden
//! (#584, SPEC-06).

use std::collections::{BTreeMap, BTreeSet};

use arb_core::analytics::{
    CrashMarker, ScreenSpan, crash_stack_rows, enrich_crash_marker, extract_process_events,
};
use arb_core::bundle::{AppLogSummary, LogRecord};
use serde_json::{Map, Value, json};

use crate::protocol::{
    ErrorLogHighlight, OverviewAppLog, OverviewCrash, OverviewProcessEvent, OverviewScreen,
    OverviewTapItem, PackageFilterSummary, WarningDetail,
};

/// Arrays a `package` filter narrows.
pub(super) const PACKAGE_APPLIED_TO: [&str; 2] = ["error_log_highlights", "noise_candidates"];
/// Arrays a `package` filter annotates without removing rows.
pub(super) const PACKAGE_ANNOTATED: [&str; 5] =
    ["crashes", "process_events", "screens", "taps", "app_logs"];
/// Warning code when the requested package appears nowhere in the bundle.
pub(super) const PACKAGE_NOT_SEEN: &str = "package_not_seen";
/// Cap of `known_packages_sample`.
const MAX_KNOWN_PACKAGES: usize = 10;

/// Crash markers with their stack evidence from the main log rows (sorted by `seq`).
pub(super) fn overview_crashes(
    crashes: &[CrashMarker],
    main_logs: &[LogRecord],
) -> Vec<OverviewCrash> {
    crashes
        .iter()
        .map(|marker| OverviewCrash {
            stack: enrich_crash_marker(marker, main_logs),
            marker: marker.clone(),
            matches_package: None,
        })
        .collect()
}

/// Points each `AndroidRuntime` error group whose representative row belongs to a crash's
/// `FATAL EXCEPTION` block at that crash, so a truncated message leads to the full stack.
pub(super) fn link_crash_groups(
    highlights: &mut [ErrorLogHighlight],
    crashes: &[CrashMarker],
    main_logs: &[LogRecord],
) {
    let mut by_seq: BTreeMap<u64, usize> = BTreeMap::new();
    for (index, crash) in crashes.iter().enumerate() {
        for row in crash_stack_rows(crash, main_logs) {
            by_seq.entry(main_logs[row].seq).or_insert(index);
        }
    }
    for group in highlights {
        if group.tag.as_deref().map(str::trim) == Some("AndroidRuntime") {
            group.crash_index = by_seq.get(&group.seq).copied();
        }
    }
}

/// Process events placed with the same coordinates as log rows.
pub(super) fn overview_process_events(
    main_logs: &[LogRecord],
    crashes: &[CrashMarker],
    locate: impl Fn(&LogRecord) -> (Option<i64>, Option<u64>),
) -> Vec<OverviewProcessEvent> {
    extract_process_events(main_logs, crashes)
        .into_iter()
        .map(|mut event| {
            if let Ok(i) = main_logs.binary_search_by_key(&event.source_log_seq, |l| l.seq) {
                (event.offset_ms, event.frame_index_hint) = locate(&main_logs[i]);
            }
            OverviewProcessEvent {
                event,
                matches_package: None,
            }
        })
        .collect()
}

/// Package of the screen span on screen at `boottime_ns` of `boot`.
///
/// With several boots, `span_boots[i]` is the boot of `screens[i]` (from its first frame), and a
/// span whose boot is unknown or different is never used, since boottime restarts at each boot.
/// `None` means a single-boot bundle.
pub(super) fn foreground_package(
    screens: &[ScreenSpan],
    span_boots: Option<&[Option<&str>]>,
    boottime_ns: Option<i64>,
    boot: Option<&str>,
) -> Option<String> {
    let t = boottime_ns?;
    screens
        .iter()
        .enumerate()
        .filter(|(i, span)| {
            span.start_ns <= t
                && t <= span.end_ns
                && span_boots.is_none_or(|boots| {
                    boots
                        .get(*i)
                        .copied()
                        .flatten()
                        .zip(boot)
                        .is_some_and(|(a, b)| a == b)
                })
        })
        .max_by_key(|(_, span)| span.start_ns)
        .map(|(_, span)| span.package_name.clone())
}

/// Rows of an overview that a `package` filter annotates.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct OverviewRows {
    pub(super) crashes: Vec<OverviewCrash>,
    pub(super) process_events: Vec<OverviewProcessEvent>,
    pub(super) screens: Vec<OverviewScreen>,
    pub(super) taps: Vec<OverviewTapItem>,
    pub(super) app_logs: Vec<OverviewAppLog>,
}

impl OverviewRows {
    pub(super) fn new(
        crashes: Vec<OverviewCrash>,
        process_events: Vec<OverviewProcessEvent>,
        screens: Vec<ScreenSpan>,
        taps: Vec<OverviewTapItem>,
        app_logs: Vec<AppLogSummary>,
    ) -> Self {
        Self {
            crashes,
            process_events,
            screens: screens
                .into_iter()
                .map(|span| OverviewScreen {
                    span,
                    matches_package: None,
                })
                .collect(),
            taps,
            app_logs: app_logs
                .into_iter()
                .map(|summary| OverviewAppLog {
                    summary,
                    matches_package: None,
                })
                .collect(),
        }
    }

    /// Packages named by the rows themselves.
    fn packages(&self) -> BTreeSet<&str> {
        self.crashes
            .iter()
            .map(|c| c.marker.package_name.as_str())
            .chain(
                self.process_events
                    .iter()
                    .map(|e| e.event.package_name.as_str()),
            )
            .chain(self.screens.iter().map(|s| s.span.package_name.as_str()))
            .chain(
                self.app_logs
                    .iter()
                    .map(|a| a.summary.package_name.as_str()),
            )
            .collect()
    }
}

/// Log-side evidence for a `package` filter.
#[derive(Debug, Clone, Default)]
pub(super) struct LogEvidence {
    /// Main log rows of any level attributed to the package.
    pub(super) matched_logs: usize,
    /// `E`/`F` rows after every filter.
    pub(super) matched_error_logs: usize,
    /// Packages named by log rows.
    pub(super) log_packages: BTreeSet<String>,
    /// `foreground_package` of each tap, aligned with `OverviewRows::taps`.
    pub(super) tap_foreground: Vec<Option<String>>,
}

fn is_named_package(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty() && !matches!(name, "unknown" | "android" | "system_server")
}

/// Annotates `rows` for `package` without adding, removing or reordering any row, and returns
/// the `package_filter` summary and warnings. `package_not_seen` is returned exactly when no log
/// row, crash, process event, screen or app log names the package.
pub(super) fn annotate_package(
    package: Option<&str>,
    rows: &mut OverviewRows,
    evidence: &LogEvidence,
) -> (Option<PackageFilterSummary>, Vec<WarningDetail>) {
    let Some(package) = package else {
        return (None, Vec::new());
    };
    let mark = |name: &str, count: &mut usize| {
        let hit = name == package;
        *count += usize::from(hit);
        Some(hit)
    };
    let (mut matched_crashes, mut matched_process_events) = (0, 0);
    let (mut matched_screens, mut matched_app_logs) = (0, 0);
    for c in &mut rows.crashes {
        c.matches_package = mark(&c.marker.package_name, &mut matched_crashes);
    }
    for e in &mut rows.process_events {
        e.matches_package = mark(&e.event.package_name, &mut matched_process_events);
    }
    for s in &mut rows.screens {
        s.matches_package = mark(&s.span.package_name, &mut matched_screens);
    }
    for a in &mut rows.app_logs {
        a.matches_package = mark(&a.summary.package_name, &mut matched_app_logs);
    }
    for (i, tap) in rows.taps.iter_mut().enumerate() {
        tap.foreground_package = Some(evidence.tap_foreground.get(i).cloned().flatten());
    }

    let named: BTreeSet<&str> = rows.packages();
    let mut sample: BTreeSet<String> = named
        .iter()
        .copied()
        .filter(|p| is_named_package(p))
        .take(MAX_KNOWN_PACKAGES)
        .map(str::to_owned)
        .collect();
    for p in &evidence.log_packages {
        if sample.len() == MAX_KNOWN_PACKAGES {
            break;
        }
        if is_named_package(p) {
            sample.insert(p.clone());
        }
    }
    let known_packages_sample: Vec<String> = sample.into_iter().collect();
    let seen = evidence.matched_logs
        + matched_crashes
        + matched_process_events
        + matched_screens
        + matched_app_logs
        > 0;
    let warnings = if seen {
        Vec::new()
    } else {
        let mut details = Map::new();
        details.insert("package".into(), json!(package));
        details.insert(
            "known_packages_sample".into(),
            Value::from(known_packages_sample.clone()),
        );
        vec![WarningDetail {
            code: PACKAGE_NOT_SEEN.into(),
            message: format!(
                "パッケージ {package} はこのバンドルのログ・クラッシュ・プロセス終了・画面のどれにも現れません。error_log_highlights と noise_candidates は 0 件に絞られ、ほかの配列は matches_package=false の注釈付きで全件を返しています。known_packages_sample から正しいパッケージ名を選んでください。"
            ),
            details: Some(details),
        }]
    };
    let summary = PackageFilterSummary {
        package: package.to_owned(),
        applied_to: PACKAGE_APPLIED_TO.map(str::to_owned).to_vec(),
        annotated: PACKAGE_ANNOTATED.map(str::to_owned).to_vec(),
        matched_logs: evidence.matched_logs,
        matched_error_logs: evidence.matched_error_logs,
        matched_crashes,
        matched_process_events,
        matched_screens,
        known_packages_sample,
    };
    (Some(summary), warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arb_core::analytics::{CrashStackDetail, ProcessEvent, ProcessEventKind};
    use arb_core::gesture::GestureKind;
    use proptest::prelude::*;

    const PACKAGES: [&str; 4] = ["com.a", "com.b", "org.c", "android"];

    fn crash(package: &str, seq: u64) -> OverviewCrash {
        OverviewCrash {
            marker: CrashMarker {
                boottime_ns: Some(i64::try_from(seq).expect("seq")),
                frame_index_hint: None,
                process_name: package.into(),
                package_name: package.into(),
                pid: Some(1),
                exception_class: "E".into(),
                message: String::new(),
                source_log_seq: seq,
            },
            stack: CrashStackDetail::default(),
            matches_package: None,
        }
    }

    fn event(package: &str, seq: u64) -> OverviewProcessEvent {
        OverviewProcessEvent {
            event: ProcessEvent {
                kind: ProcessEventKind::Killed,
                package_name: package.into(),
                pid: Some(2),
                reason: None,
                boottime_ns: None,
                offset_ms: None,
                frame_index_hint: None,
                source_log_seq: seq,
                is_crash: false,
            },
            matches_package: None,
        }
    }

    fn span(package: &str, start_ns: i64) -> ScreenSpan {
        ScreenSpan {
            start_ns,
            end_ns: start_ns + 10,
            start_frame_index: None,
            end_frame_index: None,
            package_name: package.into(),
            activity_name: format!("{package}.Main"),
            short_name: "Main".into(),
            start_offset_ms: None,
            end_offset_ms: None,
        }
    }

    fn tap(seq: u64) -> OverviewTapItem {
        OverviewTapItem {
            source: arb_core::injected_input::InputEventSource::Getevent,
            injected: false,
            tap_index: usize::try_from(seq).expect("seq"),
            seq,
            frame_index: None,
            t_ms: None,
            x: None,
            y: None,
            kind: "TAP".into(),
            gesture_kind: GestureKind::Tap,
            end_x: None,
            end_y: None,
            end_t_ms: None,
            foreground_package: None,
        }
    }

    fn app_log(package: &str) -> AppLogSummary {
        AppLogSummary {
            package_name: package.into(),
            relative_path: format!("app-logs/{package}.jsonl"),
            line_count: 1,
            crash_count: 0,
            sha256: String::new(),
        }
    }

    fn rows(picks: &[(u8, usize)]) -> OverviewRows {
        let mut crashes = Vec::new();
        let mut events = Vec::new();
        let mut screens = Vec::new();
        let mut taps = Vec::new();
        let mut apps = Vec::new();
        for (seq, &(kind, pkg)) in (0_u64..).zip(picks) {
            let package = PACKAGES[pkg % PACKAGES.len()];
            match kind % 5 {
                0 => crashes.push(crash(package, seq)),
                1 => events.push(event(package, seq)),
                2 => screens.push(span(package, i64::try_from(seq).expect("seq") * 10)),
                3 => taps.push(tap(seq)),
                _ => apps.push(app_log(package)),
            }
        }
        OverviewRows::new(crashes, events, screens, taps, apps)
    }

    /// Drops the annotations, leaving the rows the overview reports without a filter.
    fn strip(mut rows: OverviewRows) -> OverviewRows {
        rows.crashes
            .iter_mut()
            .for_each(|c| c.matches_package = None);
        rows.process_events
            .iter_mut()
            .for_each(|e| e.matches_package = None);
        rows.screens
            .iter_mut()
            .for_each(|s| s.matches_package = None);
        rows.app_logs
            .iter_mut()
            .for_each(|a| a.matches_package = None);
        rows.taps
            .iter_mut()
            .for_each(|t| t.foreground_package = None);
        rows
    }

    proptest! {
        /// #584: a filter never adds, drops or reorders a row, annotates every row, and warns
        /// `package_not_seen` exactly when nothing in the bundle names the package.
        #[test]
        fn package_filter_annotates_without_hiding_rows(
            picks in proptest::collection::vec((any::<u8>(), 0_usize..4), 0..24),
            wanted in prop_oneof![Just("com.a"), Just("com.b"), Just("no.such.pkg")],
            matched_logs in 0_usize..3,
            log_packages in proptest::collection::btree_set(prop_oneof![Just("com.a".to_owned()), Just("x.y".to_owned())], 0..2),
        ) {
            let plain = rows(&picks);
            let mut unfiltered = plain.clone();
            let (none_summary, none_warnings) = annotate_package(None, &mut unfiltered, &LogEvidence::default());
            prop_assert!(none_summary.is_none() && none_warnings.is_empty());
            prop_assert_eq!(&unfiltered, &plain);

            let mut filtered = plain.clone();
            let evidence = LogEvidence { matched_logs, matched_error_logs: 0, log_packages, tap_foreground: Vec::new() };
            let (summary, warnings) = annotate_package(Some(wanted), &mut filtered, &evidence);
            let summary = summary.expect("summary with a package");
            prop_assert!(filtered.crashes.iter().all(|c| c.matches_package == Some(c.marker.package_name == wanted)));
            prop_assert!(filtered.process_events.iter().all(|e| e.matches_package == Some(e.event.package_name == wanted)));
            prop_assert!(filtered.screens.iter().all(|s| s.matches_package == Some(s.span.package_name == wanted)));
            prop_assert!(filtered.app_logs.iter().all(|a| a.matches_package.is_some()));
            prop_assert!(filtered.taps.iter().all(|t| t.foreground_package == Some(None)));
            prop_assert_eq!(&strip(filtered.clone()), &plain);

            let named = plain.packages().contains(wanted);
            let seen = matched_logs > 0 || named;
            prop_assert_eq!(warnings.len(), usize::from(!seen));
            if let Some(w) = warnings.first() {
                prop_assert_eq!(w.code.as_str(), PACKAGE_NOT_SEEN);
                let details = w.details.as_ref().expect("details");
                prop_assert_eq!(&details["package"], &json!(wanted));
                prop_assert_eq!(&details["known_packages_sample"], &json!(summary.known_packages_sample));
            }
            prop_assert_eq!(summary.matched_crashes, plain.crashes.iter().filter(|c| c.marker.package_name == wanted).count());
            prop_assert!(summary.known_packages_sample.len() <= MAX_KNOWN_PACKAGES);
            prop_assert!(!summary.known_packages_sample.iter().any(|p| p == "android"));
            prop_assert!(summary.known_packages_sample.windows(2).all(|w| w[0] < w[1]));
        }
    }

    #[test]
    fn foreground_follows_the_latest_covering_span_and_respects_boots() {
        let screens = [span("com.a", 0), span("com.b", 10)];
        let boots = [Some("b1"), Some("b2")];
        assert_eq!(
            foreground_package(&screens, None, Some(10), None).as_deref(),
            Some("com.b"),
            "later span at the shared edge"
        );
        assert_eq!(
            foreground_package(&screens, None, Some(5), None).as_deref(),
            Some("com.a"),
            "inside the first span"
        );
        assert_eq!(
            foreground_package(&screens, None, Some(99), None),
            None,
            "after every span"
        );
        assert_eq!(
            foreground_package(&screens, Some(&boots), Some(5), Some("b2")),
            None,
            "other boot"
        );
        assert_eq!(
            foreground_package(&screens, Some(&boots), Some(15), Some("b2")).as_deref(),
            Some("com.b"),
            "same boot"
        );
        assert_eq!(
            foreground_package(&screens, Some(&[None, None]), Some(5), Some("b1")),
            None,
            "unknown span boot"
        );
    }
}
