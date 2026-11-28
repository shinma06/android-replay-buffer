//! Read-time crash stack details and non-crash process terminations.
//!
//! Both are derived from the saved logcat on every read, so bundles written before these
//! fields existed (WORM, never migrated) gain them without any change on disk. Nothing is
//! invented: a value absent from the log stays `None`.

use serde::{Deserialize, Serialize};

use super::{CrashMarker, log_boot_id_hint, log_boottime_hint, package_from_process};
use crate::bundle::{LogRecord, RecordBucket};

/// Leading `at ...` frames kept in [`CrashStackDetail::stack_top`].
pub const MAX_STACK_FRAMES: usize = 10;
/// Upper bound of [`CrashStackDetail::stack_top`]: the leading frames plus the first app frame
/// when it lies deeper.
pub const MAX_STACK_TOP_LINES: usize = MAX_STACK_FRAMES + 1;
/// `AndroidRuntime` rows searched on each side of the crash row, and the block's row cap.
pub const MAX_CRASH_BLOCK_ROWS: usize = 64;
/// Distance from the crash marker within which `AndroidRuntime` rows may belong to its block.
pub const CRASH_BLOCK_WINDOW_NS: i64 = 2_000_000_000;
/// Character cap of one returned stack line.
pub const MAX_STACK_LINE_CHARS: usize = 512;
/// Window within which a `died` event is the consequence of a crash or kill of the same PID,
/// and within which duplicate reports of one termination are merged.
pub const PROCESS_EVENT_WINDOW_NS: i64 = 5_000_000_000;
/// Rows after `Force stopping` searched for its `Killing ... stop <pkg>` lines.
const FORCE_STOP_FOLLOW_ROWS: usize = 16;
/// Rows after an `ANR in` record searched for its `PID:` / `Reason:` lines.
const ANR_FOLLOW_ROWS: usize = 4;
/// Android `lmkd` UID (`AID_LMKD`), which logs `lowmemorykiller` kills on API 29+.
const LMKD_UID: u32 = 1_069;

/// Stack evidence of one crash, read from its `AndroidRuntime` `FATAL EXCEPTION` block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashStackDetail {
    /// Up to [`MAX_STACK_FRAMES`] leading `at ...` lines, plus the first app frame if deeper.
    pub stack_top: Option<Vec<String>>,
    /// First `at <package>...` frame of the crashing package, when the log has one.
    pub first_app_frame: Option<String>,
    /// Last `Caused by: ...` line (the root cause), when the log has one.
    pub caused_by: Option<String>,
}

/// Indices into `logs` of the `AndroidRuntime` `FATAL EXCEPTION` block of `marker`.
///
/// A row qualifies only when it is a main-timeline `AndroidRuntime` row of the marker's own PID
/// and boot, within [`MAX_CRASH_BLOCK_ROWS`] rows and [`CRASH_BLOCK_WINDOW_NS`] of the marker's
/// row. The block starts at the qualifying `FATAL EXCEPTION:` row nearest the marker and ends
/// before the next one. Empty when the PID is unknown or no such block exists, so an `am_crash`
/// marker without its app-side block is never given a stack.
#[must_use]
pub fn crash_stack_rows(marker: &CrashMarker, logs: &[LogRecord]) -> Vec<usize> {
    let Some(pid) = marker.pid else {
        return Vec::new();
    };
    let Some(source) = logs.iter().position(|l| l.seq == marker.source_log_seq) else {
        return Vec::new();
    };
    let boot = log_boot_id_hint(&logs[source]);
    let lo = source.saturating_sub(MAX_CRASH_BLOCK_ROWS);
    let hi = source
        .saturating_add(MAX_CRASH_BLOCK_ROWS)
        .saturating_add(1)
        .min(logs.len());
    let member = |log: &LogRecord| {
        log.bucket == RecordBucket::Main
            && log.pid == Some(pid)
            && log.tag.as_deref().map(str::trim) == Some("AndroidRuntime")
            && log_boot_id_hint(log) == boot
            && log_boottime_hint(log)
                .zip(marker.boottime_ns)
                .is_none_or(|(a, b)| a.abs_diff(b) <= CRASH_BLOCK_WINDOW_NS.unsigned_abs())
    };
    let starts_block = |log: &LogRecord| log.message.trim_start().starts_with("FATAL EXCEPTION:");
    let Some(start) = (lo..hi)
        .filter(|&i| member(&logs[i]) && starts_block(&logs[i]))
        .min_by_key(|&i| (i.abs_diff(source), i))
    else {
        return Vec::new();
    };
    let mut rows = vec![start];
    for (i, log) in logs.iter().enumerate().take(hi).skip(start + 1) {
        if rows.len() == MAX_CRASH_BLOCK_ROWS {
            break;
        }
        if !member(log) {
            continue;
        }
        if starts_block(log) {
            break;
        }
        rows.push(i);
    }
    rows
}

/// Which frames of a stack become `stack_top` (K18).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StackTopPlan {
    /// The first `head` frames are kept (`min(len, MAX_STACK_FRAMES)`).
    pub(crate) head: usize,
    /// Position of the first app frame, if any.
    pub(crate) app: Option<usize>,
    /// The first app frame's position when it lies at or after `head`; it is appended.
    pub(crate) extra: Option<usize>,
}

impl StackTopPlan {
    /// Number of `stack_top` lines: never more than [`MAX_STACK_TOP_LINES`].
    pub(crate) fn len(self) -> usize {
        self.head + usize::from(self.extra.is_some())
    }
}

/// Keeps the first [`MAX_STACK_FRAMES`] frames, then the first `true` position of `app_frames`
/// when it lies deeper, so the app frame is always kept within [`MAX_STACK_TOP_LINES`].
pub(crate) fn plan_stack_top(app_frames: &[bool]) -> StackTopPlan {
    let app = app_frames.iter().position(|&is_app| is_app);
    let head = app_frames.len().min(MAX_STACK_FRAMES);
    StackTopPlan {
        head,
        app,
        extra: app.filter(|&pos| pos >= head),
    }
}

/// Whether `frame` (an `at ...` line) is code of `package` (`at <package>.` / `$` / `(`).
fn is_app_frame(frame: &str, package: &str) -> bool {
    let package = package.trim();
    if package.is_empty() || package == "unknown" {
        return false;
    }
    frame
        .strip_prefix("at ")
        .and_then(|rest| rest.trim_start().strip_prefix(package))
        .is_some_and(|rest| rest.starts_with(['.', '$', '(']))
}

fn bounded_line(line: &str) -> String {
    if line.chars().count() <= MAX_STACK_LINE_CHARS {
        return line.to_owned();
    }
    let mut out: String = line.chars().take(MAX_STACK_LINE_CHARS - 1).collect();
    out.push('…');
    out
}

/// Reads the stack of `marker` from its own `AndroidRuntime` block ([`crash_stack_rows`]).
///
/// Every field is `None` when the log does not contain it; nothing is synthesized from the
/// marker itself. Pure and total: any `logs` slice is accepted.
#[must_use]
pub fn enrich_crash_marker(marker: &CrashMarker, logs: &[LogRecord]) -> CrashStackDetail {
    let mut frames: Vec<String> = Vec::new();
    let mut caused_by = None;
    for row in crash_stack_rows(marker, logs) {
        for line in logs[row].message.lines().map(str::trim) {
            if line.starts_with("at ") {
                frames.push(bounded_line(line));
            } else if line.starts_with("Caused by:") {
                caused_by = Some(bounded_line(line));
            }
        }
    }
    let app_flags: Vec<bool> = frames
        .iter()
        .map(|f| is_app_frame(f, &marker.package_name))
        .collect();
    let plan = plan_stack_top(&app_flags);
    let kept = (0..plan.head).chain(plan.extra);
    CrashStackDetail {
        stack_top: (plan.len() > 0).then(|| kept.map(|i| frames[i].clone()).collect()),
        first_app_frame: plan.app.map(|i| frames[i].clone()),
        caused_by,
    }
}

/// Kind of a process termination that is not itself a crash.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessEventKind {
    /// `am force-stop` / Settings "Force stop" (`ActivityManager: Force stopping`).
    ForceStop,
    /// Low-memory kill (`lowmemorykiller` / `lmkd`, or `am_kill` with an LMK reason).
    LmkKill,
    /// Application Not Responding (`ActivityManager: ANR in`, `am_anr`).
    Anr,
    /// Process death without a crash or kill in the log (`has died`, `am_proc_died`).
    Died,
    /// Any other system kill (`ActivityManager: Killing`, `am_kill`).
    Killed,
}

/// A system-reported process termination that is not a crash marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessEvent {
    /// Termination kind.
    pub kind: ProcessEventKind,
    /// Android package derived from the reported process name.
    pub package_name: String,
    /// Terminated process ID, when the log names it.
    pub pid: Option<i32>,
    /// Reason text from the log, when present.
    pub reason: Option<String>,
    /// Device `CLOCK_BOOTTIME` of the source row, if known.
    pub boottime_ns: Option<i64>,
    /// Offset from the boot's timeline origin, set by the reader that knows the frames.
    pub offset_ms: Option<i64>,
    /// Displayed frame at the source row, set by the reader that knows the frames.
    pub frame_index_hint: Option<u64>,
    /// `LogRecord::seq` of the first row describing this termination.
    pub source_log_seq: u64,
    /// Always `false`: crashes are reported in `crashes[]`.
    pub is_crash: bool,
}

/// Which logger may report process terminations.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    /// `ActivityManager` text from `system_server`.
    ActivityManager,
    /// `lowmemorykiller` / `lmkd` text.
    Lmkd,
    /// A structured `events` buffer tag.
    Event(EventTag),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EventTag {
    Kill,
    ProcDied,
    Anr,
}

/// Authenticates the logger of `log` by tag and logd-stamped UID.
///
/// Text tags need a system UID (`0`, `1000`, or `lmkd` for its own tag). Structured `events`
/// tags also accept an unreported UID, the same rule as `am_crash`. An app UID never passes,
/// so an app cannot forge a termination of itself or of another app.
fn trusted_source(log: &LogRecord) -> Option<Source> {
    if log.bucket != RecordBucket::Main {
        return None;
    }
    let system = matches!(log.uid, Some(0 | super::SYSTEM_UID));
    match log.tag.as_deref().map(str::trim)? {
        "ActivityManager" if system => Some(Source::ActivityManager),
        "lowmemorykiller" | "lmkd" if system || log.uid == Some(LMKD_UID) => Some(Source::Lmkd),
        tag if system || log.uid.is_none() => match tag {
            "am_kill" | "30023" => Some(Source::Event(EventTag::Kill)),
            "am_proc_died" | "30011" => Some(Source::Event(EventTag::ProcDied)),
            "am_anr" | "30008" => Some(Source::Event(EventTag::Anr)),
            _ => None,
        },
        _ => None,
    }
}

/// One parsed report before merging.
struct Report {
    kind: ProcessEventKind,
    process: String,
    pid: Option<i32>,
    reason: Option<String>,
    /// `Some(pkg)` for a kill whose reason is `stop <pkg>` (part of a force stop).
    stop_of: Option<String>,
}

fn non_empty(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| bounded_line(text))
}

/// A kill reason names a low-memory kill as a word (`lmk`, `lowmemorykiller`, `low memory`).
/// Package and component names (words with `.` or `/`) are skipped, so a reason naming an app
/// such as `com.filmkit.app` is not taken for one.
fn kill_kind(reason: &str) -> ProcessEventKind {
    let words: Vec<String> = reason
        .split_whitespace()
        .filter(|w| !w.contains(['.', '/']))
        .map(|w| {
            w.trim_matches(|c: char| !c.is_ascii_alphanumeric())
                .to_ascii_lowercase()
        })
        .collect();
    let lmk = words
        .iter()
        .any(|w| matches!(w.as_str(), "lmk" | "lmkd" | "lowmemory" | "lowmemorykiller"))
        || words
            .windows(2)
            .any(|pair| pair[0] == "low" && pair[1] == "memory");
    if lmk {
        ProcessEventKind::LmkKill
    } else {
        ProcessEventKind::Killed
    }
}

fn stop_target(reason: &str) -> Option<String> {
    let rest = reason.trim().strip_prefix("stop ")?;
    let pkg = rest.split_whitespace().next()?;
    Some(pkg.to_owned())
}

fn kill_report(process: &str, pid: Option<i32>, reason: &str) -> Report {
    Report {
        kind: kill_kind(reason),
        process: process.trim().to_owned(),
        pid,
        reason: non_empty(reason),
        stop_of: stop_target(reason),
    }
}

/// `[a,b,...]` fields of a textual `events` record, with an optional `<tag>:` prefix.
fn event_fields<'a>(message: &'a str, names: [&str; 2]) -> Option<Vec<&'a str>> {
    let mut text = message.trim();
    for name in names {
        if let Some(rest) = text.strip_prefix(name).and_then(|r| r.strip_prefix(':')) {
            text = rest.trim();
        }
    }
    let inner = text.strip_prefix('[')?.strip_suffix(']')?;
    Some(inner.split(',').map(str::trim).collect())
}

fn parse_event(tag: EventTag, message: &str) -> Option<Report> {
    match tag {
        // [User,PID,Process Name,OomAdj,Reason(,Rss)]
        EventTag::Kill => {
            let f = event_fields(message, ["am_kill", "30023"])?;
            let pid = f.get(1)?.parse::<i32>().ok()?;
            let process = f.get(2).filter(|p| !p.is_empty())?;
            let mut end = f.len();
            if end >= 6 && f[end - 1].parse::<i64>().is_ok() {
                end -= 1;
            }
            let reason = f.get(4..end).map_or_default(|r| r.join(","));
            Some(kill_report(process, Some(pid), &reason))
        }
        // [User,PID,Process Name(,OomAdj,ProcState)]
        EventTag::ProcDied => {
            let f = event_fields(message, ["am_proc_died", "30011"])?;
            let pid = f.get(1)?.parse::<i32>().ok()?;
            let process = f.get(2).filter(|p| !p.is_empty())?;
            Some(Report {
                kind: ProcessEventKind::Died,
                process: (*process).to_owned(),
                pid: Some(pid),
                reason: None,
                stop_of: None,
            })
        }
        // [User,pid,Package Name,Flags,reason]
        EventTag::Anr => {
            let f = event_fields(message, ["am_anr", "30008"])?;
            let pid = f.get(1)?.parse::<i32>().ok()?;
            let process = f.get(2).filter(|p| !p.is_empty())?;
            let reason = f.get(4..).map_or_default(|r| r.join(","));
            Some(Report {
                kind: ProcessEventKind::Anr,
                process: (*process).to_owned(),
                pid: Some(pid),
                reason: non_empty(&reason),
                stop_of: None,
            })
        }
    }
}

/// `Kill 'proc' (pid), uid N, oom_score_adj N to free ...; reason: ...` (lmkd, API 29+), also
/// `Killing 'proc' (pid), ...`.
fn parse_lmkd(message: &str) -> Option<Report> {
    let line = message.trim();
    let rest = line
        .strip_prefix("Kill '")
        .or_else(|| line.strip_prefix("Killing '"))?;
    let (process, rest) = rest.split_once('\'')?;
    let pid = rest
        .trim_start()
        .strip_prefix('(')
        .and_then(|r| r.split_once(')'))
        .and_then(|(p, _)| p.trim().parse::<i32>().ok());
    let reason = line.split_once("reason: ").map_or("low memory", |(_, r)| r);
    Some(Report {
        kind: ProcessEventKind::LmkKill,
        process: process.trim().to_owned(),
        pid,
        reason: non_empty(reason),
        stop_of: None,
    })
}

/// `ActivityManager` text forms (API 28-37). `follow` are the rows after this one, used only
/// for the `PID:` / `Reason:` lines of an `ANR in` record split across rows.
fn parse_activity_manager(log: &LogRecord, follow: &[LogRecord]) -> Option<Report> {
    let mut lines = log.message.lines().map(str::trim);
    let first = lines.next()?;
    if let Some(rest) = first.strip_prefix("Force stopping ") {
        let pkg = rest.split_whitespace().next()?;
        let reason = rest.split_once(": ").map_or("", |(_, r)| r);
        return Some(Report {
            kind: ProcessEventKind::ForceStop,
            process: pkg.to_owned(),
            pid: None,
            reason: non_empty(reason),
            stop_of: None,
        });
    }
    if let Some(rest) = first.strip_prefix("Killing ") {
        // Killing <pid>:<process>/<uid> (adj N): <reason>
        let (pid, rest) = rest.split_once(':')?;
        let pid = pid.trim().parse::<i32>().ok()?;
        let process = rest.split(['/', ' ']).next()?;
        let reason = rest.split_once("): ").map_or("", |(_, r)| r);
        if process.is_empty() {
            return None;
        }
        return Some(kill_report(process, Some(pid), reason));
    }
    if let Some(rest) = first.strip_prefix("Process ") {
        // Process <process> (pid <pid>) has died[: <state>]
        let (process, rest) = rest.split_once(" (pid ")?;
        let (pid, rest) = rest.split_once(')')?;
        let state = rest.trim_start().strip_prefix("has died")?;
        return Some(Report {
            kind: ProcessEventKind::Died,
            process: process.trim().to_owned(),
            pid: pid.trim().parse::<i32>().ok(),
            reason: non_empty(state.trim_start_matches(':')),
            stop_of: None,
        });
    }
    let rest = first.strip_prefix("ANR in ")?;
    let process = rest.split_whitespace().next()?;
    let mut pid = None;
    let mut reason = None;
    let same_writer = |row: &&LogRecord| row.pid == log.pid && row.tag == log.tag;
    let tail = follow
        .iter()
        .take(ANR_FOLLOW_ROWS)
        .take_while(|row| !row.message.trim_start().starts_with("ANR in "))
        .filter(same_writer)
        .flat_map(|row| row.message.lines().map(str::trim));
    for line in lines.chain(tail) {
        if let Some(p) = line.strip_prefix("PID:") {
            pid = pid.or_else(|| p.trim().parse::<i32>().ok());
        } else if let Some(r) = line.strip_prefix("Reason:") {
            reason = reason.or_else(|| non_empty(r));
        }
    }
    Some(Report {
        kind: ProcessEventKind::Anr,
        process: process.to_owned(),
        pid,
        reason,
        stop_of: None,
    })
}

/// An event under construction, with the row facts needed for merging.
struct Pending {
    event: ProcessEvent,
    boot: Option<String>,
    row: usize,
}

fn within(a: Option<i64>, b: Option<i64>, window_ns: i64) -> bool {
    a.zip(b)
        .is_none_or(|(a, b)| a.abs_diff(b) <= window_ns.unsigned_abs())
}

fn same_boot(a: Option<&str>, b: Option<&str>) -> bool {
    a.zip(b).is_none_or(|(a, b)| a == b)
}

/// Extracts force stops, low-memory kills, ANRs, other kills and deaths from `logs`.
///
/// Only rows from authenticated system loggers are read: `ActivityManager` text from UID 0 or
/// 1000, `lowmemorykiller` / `lmkd` also from the lmkd UID, and the `am_kill` / `am_proc_died` /
/// `am_anr` events from UID 0, 1000 or an unreported UID; app UIDs never. Reports of one
/// termination are merged: `Force stopping <pkg>` absorbs the `Killing ... stop <pkg>` rows that
/// follow it (taking the first PID and the kill's reason) and any later report of an absorbed
/// kill (same PID and boot within [`PROCESS_EVENT_WINDOW_NS`]); a repeated report of the same
/// kind and PID within that window (for example `am_kill` and `Killing`) is kept once.
/// A `died` event within [`PROCESS_EVENT_WINDOW_NS`] of a crash or kill (including every kill
/// merged into a force stop) of the same PID is its consequence and is dropped; an ANR is not a
/// cause, since it does not end the process. Ordered by boottime, then source row. Pure and total.
#[must_use]
pub fn extract_process_events(logs: &[LogRecord], crashes: &[CrashMarker]) -> Vec<ProcessEvent> {
    let mut out: Vec<Pending> = Vec::new();
    // Every kill of a known PID, including those merged into a force stop or a duplicate, so the
    // deaths they cause are recognized even when the kill is not reported on its own.
    let mut kills: Vec<Cause> = Vec::new();
    // Kills merged into a force stop: a later report of the same kill (for example `am_kill`
    // just past the follow window) is that kill again, not a separate termination.
    let mut absorbed: Vec<Cause> = Vec::new();
    for (idx, log) in logs.iter().enumerate() {
        let Some(source) = trusted_source(log) else {
            continue;
        };
        let report = match source {
            Source::ActivityManager => parse_activity_manager(log, &logs[idx + 1..]),
            Source::Lmkd => parse_lmkd(&log.message),
            Source::Event(tag) => parse_event(tag, &log.message),
        };
        let Some(report) = report else { continue };
        let boottime_ns = log_boottime_hint(log);
        let boot = log_boot_id_hint(log).map(str::to_owned);
        let package_name = package_from_process(&report.process);
        if matches!(
            report.kind,
            ProcessEventKind::ForceStop | ProcessEventKind::LmkKill | ProcessEventKind::Killed
        ) && let Some(pid) = report.pid
        {
            kills.push((pid, boottime_ns, boot.clone()));
            if report.kind != ProcessEventKind::ForceStop
                && absorbed.iter().any(|(p, bt, b)| {
                    *p == pid
                        && same_boot(b.as_deref(), boot.as_deref())
                        && within(*bt, boottime_ns, PROCESS_EVENT_WINDOW_NS)
                })
            {
                continue;
            }
        }
        if let Some(pkg) = report.stop_of.as_deref()
            && let Some(stop) = out.iter_mut().rev().find(|p| {
                p.event.kind == ProcessEventKind::ForceStop
                    && p.event.package_name == pkg
                    && idx - p.row <= FORCE_STOP_FOLLOW_ROWS
                    && same_boot(p.boot.as_deref(), boot.as_deref())
                    && within(p.event.boottime_ns, boottime_ns, PROCESS_EVENT_WINDOW_NS)
            })
        {
            if let Some(pid) = report.pid {
                absorbed.push((pid, boottime_ns, boot.clone()));
            }
            if stop.event.pid.is_none() {
                stop.event.pid = report.pid;
                if report.reason.is_some() {
                    stop.event.reason = report.reason;
                }
            }
            continue;
        }
        if let Some(dup) = out.iter_mut().find(|p| {
            p.event.kind == report.kind
                && p.event.package_name == package_name
                && report.pid.is_some()
                && p.event.pid == report.pid
                && same_boot(p.boot.as_deref(), boot.as_deref())
                && within(p.event.boottime_ns, boottime_ns, PROCESS_EVENT_WINDOW_NS)
        }) {
            if dup.event.reason.is_none() {
                dup.event.reason = report.reason;
            }
            continue;
        }
        out.push(Pending {
            event: ProcessEvent {
                kind: report.kind,
                package_name,
                pid: report.pid,
                reason: report.reason,
                boottime_ns,
                offset_ms: None,
                frame_index_hint: None,
                source_log_seq: log.seq,
                is_crash: false,
            },
            boot,
            row: idx,
        });
    }
    without_consequential_deaths(out, logs, crashes, kills)
}

/// A termination cause: PID, boottime and boot of a crash or kill.
type Cause = (i32, Option<i64>, Option<String>);

/// Drops each `died` event explained by a crash or kill of the same PID and boot within
/// [`PROCESS_EVENT_WINDOW_NS`], and orders the rest by boottime, then source row.
fn without_consequential_deaths(
    out: Vec<Pending>,
    logs: &[LogRecord],
    crashes: &[CrashMarker],
    kills: Vec<Cause>,
) -> Vec<ProcessEvent> {
    let crash_boot = |c: &CrashMarker| {
        logs.iter()
            .find(|l| l.seq == c.source_log_seq)
            .and_then(log_boot_id_hint)
            .map(str::to_owned)
    };
    // An ANR does not end the process, so only crashes and kills explain a death.
    let causes: Vec<Cause> = crashes
        .iter()
        .filter_map(|c| Some((c.pid?, c.boottime_ns, crash_boot(c))))
        .chain(kills)
        .collect();
    let mut events: Vec<ProcessEvent> = out
        .into_iter()
        .filter(|p| {
            p.event.kind != ProcessEventKind::Died
                || !causes.iter().any(|(pid, bt, boot)| {
                    p.event.pid == Some(*pid)
                        && same_boot(p.boot.as_deref(), boot.as_deref())
                        && within(p.event.boottime_ns, *bt, PROCESS_EVENT_WINDOW_NS)
                })
        })
        .map(|p| p.event)
        .collect();
    events.sort_by_key(|e| (e.boottime_ns.unwrap_or(i64::MAX), e.source_log_seq));
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::TimeAlignment;

    fn row(seq: u64, tag: &str, uid: Option<u32>, pid: i32, ms: i64, message: &str) -> LogRecord {
        LogRecord {
            seq,
            boot_id: Some("boot".into()),
            header_epoch_ns: None,
            device_read_boottime_ns: None,
            pc_ingest_cont_ns: 0,
            alignment: TimeAlignment::Anchored {
                boot_id: "boot".into(),
                boottime_ns: ms * 1_000_000,
                half_width_ns: 0,
                semantics: crate::time::TimestampSemantics::LogcatHeaderWall,
            },
            bucket: RecordBucket::Main,
            level: Some("E".into()),
            tag: Some(tag.into()),
            pid: Some(pid),
            tid: None,
            uid,
            package_name: None,
            message: message.into(),
        }
    }

    fn marker(seq: u64, pid: i32, ms: i64) -> CrashMarker {
        CrashMarker {
            boottime_ns: Some(ms * 1_000_000),
            frame_index_hint: None,
            process_name: "com.example.app".into(),
            package_name: "com.example.app".into(),
            pid: Some(pid),
            exception_class: "java.lang.IllegalStateException".into(),
            message: "boom".into(),
            source_log_seq: seq,
        }
    }

    #[test]
    fn stack_keeps_a_deep_app_frame_and_the_root_cause() {
        let mut logs = vec![
            row(
                1,
                "AndroidRuntime",
                Some(10_123),
                42,
                100,
                "FATAL EXCEPTION: main",
            ),
            row(
                2,
                "AndroidRuntime",
                Some(10_123),
                42,
                100,
                "Process: com.example.app, PID: 42",
            ),
        ];
        for i in 0..12_u64 {
            logs.push(row(
                3 + i,
                "AndroidRuntime",
                Some(10_123),
                42,
                100,
                &format!("at android.Frame{i}.run(F.java:1)"),
            ));
        }
        logs.push(row(
            20,
            "AndroidRuntime",
            Some(10_123),
            42,
            100,
            "Caused by: java.lang.IllegalStateException: boom",
        ));
        logs.push(row(
            21,
            "AndroidRuntime",
            Some(10_123),
            42,
            100,
            "at com.example.app.Main.onCreate(Main.kt:9)",
        ));
        logs.push(row(
            22,
            "AndroidRuntime",
            Some(10_456),
            77,
            100,
            "at com.example.app.Forged.x(F.kt:1)",
        ));
        let detail = enrich_crash_marker(&marker(1, 42, 100), &logs);
        let top = detail.stack_top.expect("frames");
        assert_eq!(
            top.len(),
            MAX_STACK_TOP_LINES,
            "ten leading frames plus the app frame"
        );
        assert_eq!(
            top[0], "at android.Frame0.run(F.java:1)",
            "outermost frame first"
        );
        assert_eq!(
            detail.first_app_frame.as_deref(),
            Some("at com.example.app.Main.onCreate(Main.kt:9)"),
            "first frame of the crashing package"
        );
        assert_eq!(
            top.last(),
            detail.first_app_frame.as_ref(),
            "app frame kept"
        );
        assert_eq!(
            detail.caused_by.as_deref(),
            Some("Caused by: java.lang.IllegalStateException: boom"),
            "root cause line"
        );
    }

    #[test]
    fn am_crash_without_an_app_block_gets_nothing() {
        let logs = vec![row(
            5,
            "am_crash",
            Some(1_000),
            900,
            100,
            "[0,42,com.example.app,1,E,boom,F.kt,1]",
        )];
        assert_eq!(
            enrich_crash_marker(&marker(5, 42, 100), &logs),
            CrashStackDetail::default(),
            "no fabricated stack"
        );
    }

    #[test]
    fn kill_reason_names_lmk_only_as_a_word() {
        let kill = |reason: &str| {
            let logs = [row(
                1,
                "ActivityManager",
                Some(1_000),
                900,
                100,
                &format!("Killing 42:com.filmkit.app/u0a123 (adj 900): {reason}"),
            )];
            extract_process_events(&logs, &[])[0].kind
        };
        assert_eq!(
            kill("depends on provider com.filmkit.app/.P in dying proc com.x"),
            ProcessEventKind::Killed,
            "a package name containing `lmk` is not a low-memory kill"
        );
        assert_eq!(kill("empty #17"), ProcessEventKind::Killed, "other kill");
        for reason in ["lmk", "Low Memory", "lowmemorykiller: adj 900", "(lmkd)"] {
            assert_eq!(kill(reason), ProcessEventKind::LmkKill, "{reason}");
        }
    }

    #[test]
    fn force_stop_merges_its_kill_line_and_drops_the_death() {
        let logs = vec![
            row(
                1,
                "ActivityManager",
                Some(1_000),
                900,
                100,
                "Force stopping com.example.app appid=10123 user=0: from pid 4567",
            ),
            row(
                2,
                "ActivityManager",
                Some(1_000),
                900,
                101,
                "Killing 42:com.example.app/u0a123 (adj 0): stop com.example.app due to from pid 4567",
            ),
            row(
                3,
                "ActivityManager",
                Some(1_000),
                900,
                150,
                "Process com.example.app (pid 42) has died: fg  TOP",
            ),
        ];
        let events = extract_process_events(&logs, &[]);
        assert_eq!(events.len(), 1, "one termination: {events:?}");
        assert_eq!(events[0].kind, ProcessEventKind::ForceStop, "kind");
        assert_eq!(events[0].pid, Some(42), "pid from the kill line");
        assert_eq!(events[0].source_log_seq, 1, "first row");
    }
}
