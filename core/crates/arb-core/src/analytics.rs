//! Pure Android logcat analytics: `am_crash` detection, crash markers,
//! debounced crash auto-save gating, Activity screen spans, binary event log
//! decoding, and PNG signature/IHDR validation.
//!
//! All functions in this module are deterministic, side-effect-free, and require
//! zero device or OS interaction.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::bundle::{FrameRecord, LogRecord, RecordBucket};
use crate::time::TimeAlignment;

mod crash_detail;
#[cfg(kani)]
pub(crate) use crash_detail::plan_stack_top;
pub use crash_detail::{
    CRASH_BLOCK_WINDOW_NS, CrashStackDetail, MAX_CRASH_BLOCK_ROWS, MAX_STACK_FRAMES,
    MAX_STACK_LINE_CHARS, MAX_STACK_TOP_LINES, PROCESS_EVENT_WINDOW_NS, ProcessEvent,
    ProcessEventKind, crash_stack_rows, enrich_crash_marker, extract_process_events,
};

/// Maximum PNG payload saved by the daemon or read back through an MCP screenshot link.
pub const MAX_SCREENSHOT_BYTES: usize = 64 * 1024 * 1024;

/// Android `events` logcat numeric tag for `am_resume_activity` (API <= 28) / `wm_resume_activity` (API 29+).
pub const AM_RESUME_ACTIVITY_EVENT_TAG: i32 = 30_007;

/// Android `events` logcat numeric tag for `am_crash` (`ActivityManager`).
pub const AM_CRASH_EVENT_TAG: i32 = 30_039;

/// Android `events` logcat numeric tag for `am_on_resume_called` (API <= 28) / `wm_on_resume_called` (API 29+).
pub const WM_ON_RESUME_CALLED_EVENT_TAG: i32 = 30_022;

/// Android `events` logcat numeric tag for `am_focused_activity` (API <= 27) /
/// `am_set_resumed_activity` (API 28) / `wm_set_resumed_activity` (API 29+).
pub const WM_RESUMED_ACTIVITY_EVENT_TAG: i32 = 30_043;

/// Android `system_server` UID (`AID_SYSTEM`).
pub const SYSTEM_UID: u32 = 1_000;

/// Standard 8-byte PNG file signature (`\x89PNG\r\n\x1a\n`).
pub const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Maximum window (5 seconds) used to deduplicate an `AndroidRuntime: FATAL EXCEPTION`
/// fallback against a structured `am_crash` event for the same PID/process.
const CRASH_DEDUP_WINDOW_NS: i64 = 5_000_000_000;

/// Android binary event log element type constants (`android_log_event_type_t`).
const EVENT_TYPE_INT: u8 = 0;
const EVENT_TYPE_LONG: u8 = 1;
const EVENT_TYPE_STRING: u8 = 2;
const EVENT_TYPE_LIST: u8 = 3;
const EVENT_TYPE_FLOAT: u8 = 4;

/// Decoded Android binary event log payload from `logcat -B` (`events` buffer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedBinaryEvent {
    /// Numeric Android event tag (e.g., `30039` for `am_crash`).
    pub event_tag: i32,
    /// Human-readable tag name for known Android event tags, or numeric string.
    pub tag_name: String,
    /// Bracket-formatted textual message matching `logcat -v threadtime -b events`.
    pub message: String,
}

/// Decodes a raw Android `logcat -B` binary event payload (`[tag: i32 LE][typed value]`)
/// into a human-readable `(tag_name, message)` representation.
///
/// Returns `None` if `raw` is truncated or contains invalid event type codes.
#[must_use]
pub fn decode_binary_android_event(raw: &[u8]) -> Option<DecodedBinaryEvent> {
    if raw.len() < 5 {
        return None;
    }
    let tag_bytes: [u8; 4] = raw[0..4].try_into().ok()?;
    let event_tag = i32::from_le_bytes(tag_bytes);
    let mut cursor = 4_usize;
    let message = decode_event_value(raw, &mut cursor, 0)?;

    let tag_name = match event_tag {
        AM_RESUME_ACTIVITY_EVENT_TAG => "am_resume_activity".to_owned(),
        AM_CRASH_EVENT_TAG => "am_crash".to_owned(),
        WM_ON_RESUME_CALLED_EVENT_TAG => "wm_on_resume_called".to_owned(),
        WM_RESUMED_ACTIVITY_EVENT_TAG => "wm_resumed_activity".to_owned(),
        other => other.to_string(),
    };
    Some(DecodedBinaryEvent {
        event_tag,
        tag_name,
        message,
    })
}

fn decode_event_value(raw: &[u8], cursor: &mut usize, depth: u8) -> Option<String> {
    if depth > 4 || *cursor >= raw.len() {
        return None;
    }
    let kind = raw[*cursor];
    *cursor += 1;
    match kind {
        EVENT_TYPE_INT => {
            let bytes: [u8; 4] = raw.get(*cursor..*cursor + 4)?.try_into().ok()?;
            *cursor += 4;
            Some(i32::from_le_bytes(bytes).to_string())
        }
        EVENT_TYPE_LONG => {
            let bytes: [u8; 8] = raw.get(*cursor..*cursor + 8)?.try_into().ok()?;
            *cursor += 8;
            Some(i64::from_le_bytes(bytes).to_string())
        }
        EVENT_TYPE_FLOAT => {
            let bytes: [u8; 4] = raw.get(*cursor..*cursor + 4)?.try_into().ok()?;
            *cursor += 4;
            Some(f32::from_le_bytes(bytes).to_string())
        }
        EVENT_TYPE_STRING => {
            let len_bytes: [u8; 4] = raw.get(*cursor..*cursor + 4)?.try_into().ok()?;
            *cursor += 4;
            let len = usize::try_from(u32::from_le_bytes(len_bytes)).ok()?;
            let slice = raw.get(*cursor..cursor.checked_add(len)?)?;
            *cursor += len;
            Some(String::from_utf8_lossy(slice).into_owned())
        }
        EVENT_TYPE_LIST => {
            let count = usize::from(*raw.get(*cursor)?);
            *cursor += 1;
            let mut items = Vec::with_capacity(count);
            for _ in 0..count {
                items.push(decode_event_value(raw, cursor, depth + 1)?);
            }
            Some(format!("[{}]", items.join(",")))
        }
        _ => None,
    }
}

/// Structured fields parsed from an Android `am_crash` event (8-field API 28-33 or 9-field API 34+).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedAmCrash {
    /// Android user ID (e.g., `0`).
    pub user: i32,
    /// Crashing process ID.
    pub pid: i32,
    /// Full process name (may include `:suffix`).
    pub process_name: String,
    /// Derived Android application package name (`process_name` before `:`).
    pub package_name: String,
    /// Process flags bitmask.
    pub flags: i32,
    /// Exception class name (e.g., `java.lang.NullPointerException` or `Native crash`).
    pub exception_class: String,
    /// Exception detail message.
    pub message: String,
    /// Source file reported by `am_crash`.
    pub file: String,
    /// Source line reported by `am_crash` (`-1` / `-2` when unavailable).
    pub line: i32,
    /// `true` only when the API 34+ 9th `Recoverable` field is non-zero (`1`).
    pub recoverable: bool,
}

impl ParsedAmCrash {
    /// Returns `true` when this crash is fatal (not marked recoverable on API 34+).
    #[must_use]
    pub const fn is_fatal(&self) -> bool {
        !self.recoverable
    }
}

/// Parses a binary `logcat -B` `am_crash` event (`tag == 30039`), supporting both
/// 8-field (API 28-33) and 9-field (API 34+) layouts.
#[must_use]
pub fn parse_am_crash_binary(raw: &[u8]) -> Option<ParsedAmCrash> {
    if raw.len() < 6 {
        return None;
    }
    let tag = i32::from_le_bytes(raw[0..4].try_into().ok()?);
    if tag != AM_CRASH_EVENT_TAG {
        return None;
    }
    if raw[4] != EVENT_TYPE_LIST {
        return None;
    }
    let count = raw[5];
    if count != 8 && count != 9 {
        return None;
    }
    let mut cursor = 6_usize;
    let user = read_binary_int(raw, &mut cursor)?;
    let pid = read_binary_int(raw, &mut cursor)?;
    let process_name = read_binary_string(raw, &mut cursor)?;
    if process_name.trim().is_empty() {
        return None;
    }
    let flags = read_binary_int(raw, &mut cursor)?;
    let exception_class = read_binary_string(raw, &mut cursor)?;
    // Same rule as `parse_am_crash_text`, so live detection and stored markers agree.
    if exception_class.trim().is_empty() {
        return None;
    }
    let message = read_binary_string(raw, &mut cursor)?;
    let file = read_binary_string(raw, &mut cursor)?;
    let line = read_binary_int(raw, &mut cursor)?;
    let recoverable = if count == 9 {
        let rec = read_binary_int(raw, &mut cursor)?;
        rec != 0
    } else {
        false
    };
    // Tolerate a trailing NUL / newline terminator, matching `decode_binary_android_event`
    // (which ignores trailing bytes), so live detection agrees with the stored bundle text.
    if raw
        .get(cursor..)
        .is_none_or(|tail| tail.iter().any(|b| !matches!(b, 0 | b'\n')))
    {
        return None;
    }
    let package_name = package_from_process(&process_name);
    Some(ParsedAmCrash {
        user,
        pid,
        process_name,
        package_name,
        flags,
        exception_class,
        message,
        file,
        line,
        recoverable,
    })
}

fn read_binary_int(raw: &[u8], cursor: &mut usize) -> Option<i32> {
    if *raw.get(*cursor)? != EVENT_TYPE_INT {
        return None;
    }
    *cursor += 1;
    let bytes: [u8; 4] = raw.get(*cursor..*cursor + 4)?.try_into().ok()?;
    *cursor += 4;
    Some(i32::from_le_bytes(bytes))
}

fn read_binary_string(raw: &[u8], cursor: &mut usize) -> Option<String> {
    if *raw.get(*cursor)? != EVENT_TYPE_STRING {
        return None;
    }
    *cursor += 1;
    let len_bytes: [u8; 4] = raw.get(*cursor..*cursor + 4)?.try_into().ok()?;
    *cursor += 4;
    let len = usize::try_from(u32::from_le_bytes(len_bytes)).ok()?;
    let slice = raw.get(*cursor..cursor.checked_add(len)?)?;
    *cursor += len;
    // Lossy, like `decode_binary_android_event`: liblog truncates long strings mid-character,
    // and live detection must agree with the text stored in the bundle.
    Some(String::from_utf8_lossy(slice).into_owned())
}

/// Like [`parse_am_crash_text`] but only accepts records whose logcat tag is `am_crash` /
/// `30039` (the events buffer), never an arbitrary app message that merely starts with
/// `am_crash:`, so an app cannot spoof crash markers or trigger auto-saves.
#[must_use]
pub fn parse_am_crash_text_for_event_tag(
    tag: Option<&str>,
    message: &str,
) -> Option<ParsedAmCrash> {
    if !matches!(tag.map(str::trim), Some("am_crash" | "30039")) {
        return None;
    }
    parse_am_crash_text(tag, message)
}

/// Parses a textual `am_crash` logcat record (both 8-field API 28-33 and 9-field API 34+
/// formats, tolerating commas inside the exception message).
///
/// Accepts:
/// - `tag = Some("am_crash")` or `Some("30039")` with `message = "[...]"`
/// - `message = "am_crash: [...]"` or `"30039: [...]"` regardless of tag
#[must_use]
pub fn parse_am_crash_text(tag: Option<&str>, message: &str) -> Option<ParsedAmCrash> {
    let trimmed = message.trim();
    let bracketed = if matches!(tag.map(str::trim), Some("am_crash" | "30039")) {
        trimmed
            .strip_prefix("am_crash:")
            .or_else(|| trimmed.strip_prefix("30039:"))
            .map_or(trimmed, str::trim)
    } else {
        let rest = trimmed
            .strip_prefix("am_crash:")
            .or_else(|| trimmed.strip_prefix("30039:"))?;
        rest.trim()
    };

    let inner = bracketed.strip_prefix('[')?.strip_suffix(']')?;
    let parts: Vec<&str> = inner.split(',').collect();
    if parts.len() < 8 {
        return None;
    }

    let user = parts[0].trim().parse::<i32>().ok()?;
    let pid = parts[1].trim().parse::<i32>().ok()?;
    let process_name = parts[2].trim().to_owned();
    if process_name.is_empty() {
        return None;
    }
    let flags = parts[3].trim().parse::<i32>().ok()?;
    let exception_class = parts[4].trim().to_owned();
    if exception_class.is_empty() {
        return None;
    }

    // Parse from the tail so commas inside the exception `message` (field 5) stay intact:
    // - 9-field (API 34+): ..., file, line(i32), recoverable(0|1)
    // - 8-field (API 28-33): ..., file, line(i32)
    let n = parts.len();
    let last_int = parts[n - 1].trim().parse::<i32>().ok()?;
    let (file, line, recoverable, msg_end) = if n >= 9
        && matches!(last_int, 0 | 1)
        && let Ok(line_num) = parts[n - 2].trim().parse::<i32>()
    {
        (
            parts[n - 3].trim().to_owned(),
            line_num,
            last_int != 0,
            n - 3,
        )
    } else {
        (parts[n - 2].trim().to_owned(), last_int, false, n - 2)
    };

    if msg_end < 5 {
        return None;
    }
    let crash_message = parts[5..msg_end].join(",").trim().to_owned();
    let package_name = package_from_process(&process_name);

    Some(ParsedAmCrash {
        user,
        pid,
        process_name,
        package_name,
        flags,
        exception_class,
        message: crash_message,
        file,
        line,
        recoverable,
    })
}

/// Structured crash marker derived from bundle logcat and anchored onto the video timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashMarker {
    /// Device `CLOCK_BOOTTIME` timestamp of the crash log record, if available.
    pub boottime_ns: Option<i64>,
    /// Nearest video frame index in the bundle, when video frames are present.
    pub frame_index_hint: Option<u64>,
    /// Full crashing process name (e.g., `com.example.app:worker`).
    pub process_name: String,
    /// Android package name derived from `process_name`.
    pub package_name: String,
    /// Crashing process ID, if known.
    pub pid: Option<i32>,
    /// Exception class name (e.g., `java.lang.IllegalStateException`).
    pub exception_class: String,
    /// Exception detail message.
    pub message: String,
    /// `LogRecord::seq` of the source log entry in `timeline.sqlite`.
    pub source_log_seq: u64,
}

/// Extracts structured [`CrashMarker`] items deterministically from `logs` and `frames`.
///
/// Recognizes:
/// 1. Structured `am_crash` lines (8-field API 28-33 and 9-field API 34+, ignoring
///    non-fatal `recoverable = 1` entries).
/// 2. `AndroidRuntime` `FATAL EXCEPTION:` + `Process: ..., PID: ...` + exception line
///    fallbacks (both single multi-line log records and consecutive per-line records).
///
/// When both `am_crash` and `FATAL EXCEPTION:` describe the same crash (matching PID or
/// process within 5 seconds), only the primary `am_crash` marker is emitted.
#[must_use]
pub fn extract_crash_markers(logs: &[LogRecord], frames: &[FrameRecord]) -> Vec<CrashMarker> {
    let mut am_crashes: Vec<CrashMarker> = Vec::new();
    let mut fatal_fallbacks: Vec<CrashMarker> = Vec::new();

    for (idx, log) in logs.iter().enumerate() {
        // Quarantined (unanchored) logs have no trustworthy time: never pin them to frames.
        if log.bucket != RecordBucket::Main {
            continue;
        }
        if is_system_server_uid(log.uid)
            && let Some(parsed) =
                parse_am_crash_text_for_event_tag(log.tag.as_deref(), &log.message)
        {
            if parsed.is_fatal() {
                let boottime_ns = log_boottime_hint(log);
                let boot_id = log_boot_id_hint(log);
                let frame_index_hint =
                    boottime_ns.and_then(|bt| nearest_frame_index_for_boot(frames, boot_id, bt));
                am_crashes.push(CrashMarker {
                    boottime_ns,
                    frame_index_hint,
                    process_name: parsed.process_name,
                    package_name: parsed.package_name,
                    pid: Some(parsed.pid),
                    exception_class: parsed.exception_class,
                    message: parsed.message,
                    source_log_seq: log.seq,
                });
            }
            continue;
        }

        // Only the framework's own `AndroidRuntime` tag may describe a fatal exception.
        if log.tag.as_deref().map(str::trim) != Some("AndroidRuntime") {
            continue;
        }
        if let Some(fallback) = parse_fatal_exception_at(logs, idx)
            // A real block is logged by the crashing process itself: ignore text naming
            // another PID than the record's own.
            .filter(|fb| {
                fb.pid
                    .zip(log.pid)
                    .is_none_or(|(parsed, own)| parsed == own)
            })
        {
            let boottime_ns = log_boottime_hint(log);
            let boot_id = log_boot_id_hint(log);
            let frame_index_hint =
                boottime_ns.and_then(|bt| nearest_frame_index_for_boot(frames, boot_id, bt));
            fatal_fallbacks.push(CrashMarker {
                boottime_ns,
                frame_index_hint,
                process_name: fallback.process_name,
                package_name: fallback.package_name,
                pid: fallback.pid.or(log.pid),
                exception_class: fallback.exception_class,
                message: fallback.message,
                source_log_seq: log.seq,
            });
        }
    }

    let mut out = am_crashes;
    for fb in fatal_fallbacks {
        let duplicated = out.iter().any(|existing| is_same_crash(existing, &fb));
        if !duplicated {
            out.push(fb);
        }
    }
    out.sort_by_key(|m| (m.boottime_ns.unwrap_or(i64::MAX), m.source_log_seq));
    out
}

#[derive(Debug)]
struct ParsedFatalFallback {
    process_name: String,
    package_name: String,
    pid: Option<i32>,
    exception_class: String,
    message: String,
}

fn parse_fatal_exception_at(logs: &[LogRecord], idx: usize) -> Option<ParsedFatalFallback> {
    let log = logs.get(idx)?;
    let first_line = log.message.lines().next()?.trim();
    if !first_line.starts_with("FATAL EXCEPTION:") {
        return None;
    }

    // Collect subsequent lines from either the same multi-line record or consecutive records
    // from the same PID/tag (up to 6 following lines).
    let mut lines: Vec<&str> = log.message.lines().skip(1).map(str::trim).collect();
    if lines.is_empty() {
        let limit = (idx + 7).min(logs.len());
        for follow in &logs[idx + 1..limit] {
            if follow.pid.is_some() && log.pid.is_some() && follow.pid != log.pid {
                continue;
            }
            if follow.tag.as_deref() != log.tag.as_deref() {
                continue;
            }
            let text = follow.message.trim();
            if text.starts_with("FATAL EXCEPTION:") {
                break;
            }
            lines.push(text);
        }
    }

    let mut process_name: Option<String> = None;
    let mut pid: Option<i32> = log.pid;
    let mut exception_class: Option<String> = None;
    let mut message = String::new();

    for line in lines {
        if line.is_empty() {
            continue;
        }
        if process_name.is_none()
            && let Some(rest) = line.strip_prefix("Process:")
        {
            let (proc_part, pid_part) = rest.split_once(", PID:").unwrap_or((rest, ""));
            let proc_trimmed = proc_part.trim();
            if !proc_trimmed.is_empty() {
                process_name = Some(proc_trimmed.to_owned());
            }
            if let Ok(parsed_pid) = pid_part.trim().parse::<i32>() {
                pid = Some(parsed_pid);
            }
            continue;
        }
        if line.starts_with("PID:") && pid.is_none() {
            if let Ok(parsed_pid) = line.trim_start_matches("PID:").trim().parse::<i32>() {
                pid = Some(parsed_pid);
            }
            continue;
        }
        if line.starts_with("at ") || line.starts_with("Caused by:") {
            if exception_class.is_some() {
                break;
            }
            continue;
        }
        if exception_class.is_none() && looks_like_exception_line(line) {
            if let Some((cls, msg)) = line.split_once(':') {
                exception_class = Some(cls.trim().to_owned());
                msg.trim().clone_into(&mut message);
            } else {
                exception_class = Some(line.to_owned());
            }
            break;
        }
    }

    let process_name = process_name.unwrap_or_else(|| "unknown".to_owned());
    let package_name = package_from_process(&process_name);
    let exception_class = exception_class.unwrap_or_else(|| "FATAL EXCEPTION".to_owned());

    Some(ParsedFatalFallback {
        process_name,
        package_name,
        pid,
        exception_class,
        message,
    })
}

fn looks_like_exception_line(line: &str) -> bool {
    let head = line.split_once(':').map_or(line, |(h, _)| h).trim();
    !head.is_empty()
        && !head.contains(' ')
        && (head.contains('.')
            || head.ends_with("Exception")
            || head.ends_with("Error")
            || head.ends_with("Throwable"))
}

fn is_same_crash(a: &CrashMarker, b: &CrashMarker) -> bool {
    let identity_match = match (a.pid, b.pid) {
        (Some(pa), Some(pb)) if pa == pb => true,
        _ => a.process_name == b.process_name && a.process_name != "unknown",
    };
    if !identity_match {
        return false;
    }
    match (a.boottime_ns, b.boottime_ns) {
        (Some(ta), Some(tb)) => {
            ta.abs_diff(tb) <= u64::try_from(CRASH_DEDUP_WINDOW_NS).unwrap_or(u64::MAX)
        }
        _ => true,
    }
}

/// Outcome of evaluating a live fatal `am_crash` event against [`CrashAutoSaveGate`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrashAutoSaveDecision {
    /// Proceed with an asynchronous background WORM save (`trigger = CrashAutoSave`).
    Trigger,
    /// Auto-save is disabled in user configuration (`crash_autosave_enabled = false`).
    Disabled,
    /// Suppressed because a previous crash auto-save ran within `cooldown_secs`.
    SuppressedCooldown,
    /// Suppressed because `STORAGE_PRESSURE_STOP` is active (REQ-04 data protection).
    SuppressedStoragePressure,
}

/// Parameters supplied to [`CrashAutoSaveGate::evaluate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrashAutoSaveEval {
    /// Whether live crash auto-save is enabled in user configuration.
    pub enabled: bool,
    /// Whether `STORAGE_PRESSURE_STOP` is currently active.
    pub storage_pressure_active: bool,
    /// Minimum cooldown between crash auto-saves in seconds.
    pub cooldown_secs: u32,
    /// Monotonic event timestamp in nanoseconds.
    pub now_ns: i64,
}

/// Pure, deterministic debounce and storage-pressure gate for live `am_crash` auto-saves.
#[derive(Debug, Clone, Default)]
pub struct CrashAutoSaveGate {
    last_trigger_ns: Option<i64>,
    /// Value of `last_trigger_ns` before the most recent `Trigger`, so a failed save can be
    /// rolled back with [`Self::abort_last_trigger`].
    previous_trigger_ns: Option<i64>,
}

impl CrashAutoSaveGate {
    /// Creates a new gate with no prior auto-save timestamp.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            last_trigger_ns: None,
            previous_trigger_ns: None,
        }
    }

    /// Evaluates whether a fatal crash described by `eval` should trigger an auto-save.
    ///
    /// Precedence:
    /// 1. Disabled (`!eval.enabled`) -> [`CrashAutoSaveDecision::Disabled`]
    /// 2. Storage pressure (`eval.storage_pressure_active`) -> [`CrashAutoSaveDecision::SuppressedStoragePressure`]
    /// 3. Within `eval.cooldown_secs` of `last_trigger_ns` -> [`CrashAutoSaveDecision::SuppressedCooldown`]
    /// 4. Otherwise records `last_trigger_ns = Some(eval.now_ns)` and returns [`CrashAutoSaveDecision::Trigger`].
    pub fn evaluate(&mut self, eval: CrashAutoSaveEval) -> CrashAutoSaveDecision {
        if !eval.enabled {
            return CrashAutoSaveDecision::Disabled;
        }
        if eval.storage_pressure_active {
            return CrashAutoSaveDecision::SuppressedStoragePressure;
        }
        let cooldown_ns = i64::from(eval.cooldown_secs).saturating_mul(1_000_000_000);
        // `now_ns < prev` means the event clock restarted (new replayed / mock session with its
        // own `pc_cont_ns` domain); the stale timestamp must not wedge the gate.
        if let Some(prev) = self.last_trigger_ns
            && eval.now_ns >= prev
            && eval.now_ns.saturating_sub(prev) < cooldown_ns
        {
            return CrashAutoSaveDecision::SuppressedCooldown;
        }
        self.previous_trigger_ns = self.last_trigger_ns;
        self.last_trigger_ns = Some(eval.now_ns);
        CrashAutoSaveDecision::Trigger
    }

    /// Rolls back the most recent [`CrashAutoSaveDecision::Trigger`] because its save produced
    /// no bundle, so a failed attempt does not suppress the next crash for a whole cooldown.
    pub const fn abort_last_trigger(&mut self) {
        self.last_trigger_ns = self.previous_trigger_ns;
    }

    /// Returns the monotonic nanosecond timestamp of the last triggered auto-save, if any.
    #[must_use]
    pub const fn last_trigger_ns(&self) -> Option<i64> {
        self.last_trigger_ns
    }
}

/// Contiguous Activity / screen span derived purely on the PC from stored bundle logcat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenSpan {
    /// Inclusive start of this screen span on `DeviceBoottime` in nanoseconds.
    pub start_ns: i64,
    /// Inclusive end of this screen span on `DeviceBoottime` in nanoseconds.
    pub end_ns: i64,
    /// First video frame index overlapping this screen span, if video frames exist.
    pub start_frame_index: Option<u64>,
    /// Last video frame index overlapping this screen span, if video frames exist.
    pub end_frame_index: Option<u64>,
    /// Android application package name (e.g., `com.example.app`).
    pub package_name: String,
    /// Fully qualified Activity class name (e.g., `com.example.app.ui.MainActivity`).
    pub activity_name: String,
    /// Short Activity class name (e.g., `MainActivity`).
    pub short_name: String,
    /// `start_ns` in milliseconds from the bundle origin (display start of the first frame,
    /// else the first epoch). Filled by the reader that knows the origin; `None` otherwise.
    #[serde(default)]
    pub start_offset_ms: Option<i64>,
    /// `end_ns` in milliseconds from the same bundle origin as `start_offset_ms`.
    #[serde(default)]
    pub end_offset_ms: Option<i64>,
}

/// Parsed Android Activity component identity `(package_name, activity_name, short_name)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedActivityComponent {
    /// Application package name.
    pub package_name: String,
    /// Fully qualified Activity class name.
    pub activity_name: String,
    /// Unqualified simple Activity class name.
    pub short_name: String,
}

const fn is_system_server_uid(uid: Option<u32>) -> bool {
    match uid {
        Some(u) => u == SYSTEM_UID,
        None => true,
    }
}

fn is_app_resume_event_tag(tag: Option<&str>) -> bool {
    matches!(
        tag.map(str::trim),
        Some("wm_on_resume_called" | "am_on_resume_called" | "30022")
    )
}

fn is_system_server_screen_tag(tag: Option<&str>) -> bool {
    matches!(
        tag.map(str::trim),
        Some(
            "wm_resumed_activity"
                | "wm_set_resumed_activity"
                | "am_focused_activity"
                | "am_set_resumed_activity"
                | "30043"
                | "am_resume_activity"
                | "wm_resume_activity"
                | "30007"
                | "ActivityTaskManager"
                | "ActivityManager"
        )
    )
}

/// `am_on_resume_called` / `wm_on_resume_called` (`30022`) is written from the app's own
/// process, so any app can emit one naming another app's Activity. When the record's UID
/// maps to an app package, that UID-authenticated package wins over the self-reported one.
fn uid_authenticated_component(
    log: &LogRecord,
    mut comp: ParsedActivityComponent,
) -> ParsedActivityComponent {
    if is_app_resume_event_tag(log.tag.as_deref())
        && let Some(own) = log
            .package_name
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty() && !matches!(*p, "android" | "system_server"))
    {
        own.clone_into(&mut comp.package_name);
    }
    comp
}

fn is_authentic_screen_transition_record(log: &LogRecord) -> bool {
    if is_app_resume_event_tag(log.tag.as_deref()) {
        return true;
    }
    if is_system_server_screen_tag(log.tag.as_deref()) {
        return is_system_server_uid(log.uid);
    }
    false
}

/// Parses a single logcat record for an Android Activity transition event across Android 9
/// (API 28 `am_*`) and Android 10+ (API 29+ `wm_*`) formats:
/// - `wm_on_resume_called` / `am_on_resume_called` (`30022`): `[TokenOrUser,ComponentName,Reason(,time)]`
/// - `am_resume_activity` / `wm_resume_activity` (`30007`): `[User,Token,TaskId,ComponentName]`
/// - `am_focused_activity` (API <= 27) / `am_set_resumed_activity` (API 28) /
///   `wm_set_resumed_activity` (API 29+) (`30043`): `[User,ComponentName(,Reason)]`
/// - `wm_resumed_activity` (`30043`): `[User,ComponentName,Reason]` or `[User,Token,ComponentName]`
/// - `ActivityTaskManager` / `ActivityManager`: `Displayed <ComponentName> ...`
#[must_use]
pub fn parse_screen_transition_line(
    tag: Option<&str>,
    message: &str,
) -> Option<ParsedActivityComponent> {
    let trimmed_tag = tag.map(str::trim);
    let trimmed_msg = message.trim();

    if matches!(
        trimmed_tag,
        Some("wm_on_resume_called" | "am_on_resume_called" | "30022")
    ) || trimmed_msg.starts_with("wm_on_resume_called:")
        || trimmed_msg.starts_with("am_on_resume_called:")
    {
        let body = trimmed_msg
            .strip_prefix("wm_on_resume_called:")
            .or_else(|| trimmed_msg.strip_prefix("am_on_resume_called:"))
            .map_or(trimmed_msg, str::trim);
        let inner = body.strip_prefix('[')?.strip_suffix(']')?;
        let mut fields = inner.split(',');
        let _token_or_user = fields.next()?;
        let component = fields.next()?.trim();
        return normalize_activity_component(component);
    }

    if matches!(
        trimmed_tag,
        Some("am_resume_activity" | "wm_resume_activity" | "30007")
    ) || trimmed_msg.starts_with("am_resume_activity:")
        || trimmed_msg.starts_with("wm_resume_activity:")
    {
        let body = trimmed_msg
            .strip_prefix("am_resume_activity:")
            .or_else(|| trimmed_msg.strip_prefix("wm_resume_activity:"))
            .map_or(trimmed_msg, str::trim);
        let inner = body.strip_prefix('[')?.strip_suffix(']')?;
        let mut fields = inner.split(',');
        let _user = fields.next()?;
        let _token = fields.next()?;
        let _task_id = fields.next()?;
        let component = fields.next()?.trim();
        return normalize_activity_component(component);
    }

    if matches!(
        trimmed_tag,
        Some("am_focused_activity" | "am_set_resumed_activity" | "wm_set_resumed_activity")
    ) || trimmed_msg.starts_with("am_focused_activity:")
        || trimmed_msg.starts_with("am_set_resumed_activity:")
        || trimmed_msg.starts_with("wm_set_resumed_activity:")
    {
        let body = trimmed_msg
            .strip_prefix("am_focused_activity:")
            .or_else(|| trimmed_msg.strip_prefix("am_set_resumed_activity:"))
            .or_else(|| trimmed_msg.strip_prefix("wm_set_resumed_activity:"))
            .map_or(trimmed_msg, str::trim);
        let inner = body.strip_prefix('[')?.strip_suffix(']')?;
        let mut fields = inner.split(',');
        let _user = fields.next()?;
        let component = fields.next()?.trim();
        return normalize_activity_component(component);
    }

    if matches!(trimmed_tag, Some("wm_resumed_activity" | "30043"))
        || trimmed_msg.starts_with("wm_resumed_activity:")
    {
        let body = trimmed_msg
            .strip_prefix("wm_resumed_activity:")
            .or_else(|| trimmed_msg.strip_prefix("wm_set_resumed_activity:"))
            .or_else(|| trimmed_msg.strip_prefix("am_focused_activity:"))
            .or_else(|| trimmed_msg.strip_prefix("30043:"))
            .map_or(trimmed_msg, str::trim);
        let inner = body.strip_prefix('[')?.strip_suffix(']')?;
        let mut fields = inner.split(',');
        let _user = fields.next()?;
        let second = fields.next()?.trim();
        // AOSP numeric event tag 30043 (`am_focused_activity` on API 28,
        // `wm_set_resumed_activity` on API 29+) emits `[User, ComponentName, Reason]`,
        // while `wm_resumed_activity` fixtures emit `[User, Token, ComponentName]`.
        if let Some(comp) = normalize_activity_component(second) {
            return Some(comp);
        }
        let third = fields.next()?.trim();
        return normalize_activity_component(third);
    }

    if matches!(trimmed_tag, Some("ActivityTaskManager" | "ActivityManager"))
        || trimmed_msg.starts_with("ActivityTaskManager:")
        || trimmed_msg.starts_with("ActivityManager:")
    {
        let body = trimmed_msg
            .strip_prefix("ActivityTaskManager:")
            .or_else(|| trimmed_msg.strip_prefix("ActivityManager:"))
            .map_or(trimmed_msg, str::trim);
        if let Some(after_displayed) = body.strip_prefix("Displayed ") {
            let token = after_displayed.split_whitespace().next()?;
            let component = token.trim_end_matches(':');
            return normalize_activity_component(component);
        }
    }

    None
}

/// Normalizes an Android component token (`pkg/.Sub.Act`, `pkg/full.Act`, `pkg/Act`, or `full.Act`)
/// into `(package_name, activity_name, short_name)`.
#[must_use]
pub fn normalize_activity_component(raw: &str) -> Option<ParsedActivityComponent> {
    let cleaned = raw.trim().trim_end_matches(':');
    if cleaned.is_empty() || cleaned.contains(' ') {
        return None;
    }

    let (package_name, activity_name) = if let Some((pkg, cls)) = cleaned.split_once('/') {
        let pkg = pkg.trim();
        let cls = cls.trim();
        if pkg.is_empty() || cls.is_empty() {
            return None;
        }
        let full = if cls.starts_with('.') {
            format!("{pkg}{cls}")
        } else if cls.contains('.') {
            cls.to_owned()
        } else {
            format!("{pkg}.{cls}")
        };
        (pkg.to_owned(), full)
    } else {
        let (pkg, _cls) = cleaned.rsplit_once('.')?;
        if pkg.is_empty() {
            return None;
        }
        (pkg.to_owned(), cleaned.to_owned())
    };

    let short_name = activity_name
        .rsplit('.')
        .next()
        .unwrap_or(&activity_name)
        .to_owned();
    if short_name.is_empty() {
        return None;
    }

    Some(ParsedActivityComponent {
        package_name,
        activity_name,
        short_name,
    })
}

/// Extracts contiguous [`ScreenSpan`] ranges purely from stored bundle `logs` and `frames`.
///
/// Consecutive transitions into the same `(package_name, activity_name)` (for example,
/// `wm_resumed_activity` followed immediately by `wm_on_resume_called` and `Displayed`)
/// are collapsed into a single contiguous span starting at the earliest timestamp.
#[must_use]
pub fn extract_screen_spans(logs: &[LogRecord], frames: &[FrameRecord]) -> Vec<ScreenSpan> {
    let mut epochs = BTreeMap::new();
    for log in logs {
        if let Some(id) = log_boot_id_hint(log) {
            epochs.entry(id).or_insert_with(Vec::new).push(log);
        }
    }
    let mut groups: Vec<_> = epochs.into_iter().collect();
    groups.sort_by_key(|(_, logs)| logs.iter().map(|log| log.pc_ingest_cont_ns).min());
    groups
        .into_iter()
        .flat_map(|(id, logs)| {
            let epoch_frames = frames
                .iter()
                .filter(|f| f.boot_id == id)
                .collect::<Vec<_>>();
            screen_spans_for_epoch(&logs, &epoch_frames)
        })
        .collect()
}

fn screen_spans_for_epoch(logs: &[&LogRecord], frames: &[&FrameRecord]) -> Vec<ScreenSpan> {
    let mut transitions: Vec<(i64, u64, ParsedActivityComponent)> = Vec::new();
    let mut latest_log_bt: Option<i64> = None;

    for log in logs {
        if log.bucket != RecordBucket::Main {
            continue;
        }
        let Some(bt) = log_boottime_hint(log) else {
            continue;
        };
        latest_log_bt = Some(latest_log_bt.map_or(bt, |prev| prev.max(bt)));

        // Decide on the tag (and system_server UID when present): an app message that merely
        // *starts with* a system event name or forges a system_server tag must not populate
        // `timeline.screens`. Note `am_on_resume_called` / `wm_on_resume_called` (`30022`) is
        // emitted by ActivityThread inside the app process itself.
        if !is_authentic_screen_transition_record(log) {
            continue;
        }
        if let Some(comp) = parse_screen_transition_line(log.tag.as_deref(), &log.message) {
            transitions.push((bt, log.seq, uid_authenticated_component(log, comp)));
        }
    }

    if transitions.is_empty() {
        return Vec::new();
    }

    transitions.sort_by_key(|(bt, seq, _)| (*bt, *seq));

    // Collapse consecutive duplicates of the same Activity into one continuous visit.
    let mut collapsed: Vec<(i64, ParsedActivityComponent)> = Vec::with_capacity(transitions.len());
    for (bt, _seq, comp) in transitions {
        // `wm_on_resume_called` only carries the class name, so its package is a guess (the
        // class's Java package); `wm_resumed_activity` / `Displayed` carry the real package,
        // always a prefix of the class name. Collapse on the class name and keep the shorter
        // (real) package.
        if let Some((_, prev)) = collapsed.last_mut()
            && prev.activity_name == comp.activity_name
        {
            if comp.package_name.len() < prev.package_name.len() {
                prev.package_name = comp.package_name;
            }
        } else {
            collapsed.push((bt, comp));
        }
    }

    let last_frame_bt = frames.iter().filter_map(|f| frame_boottime_hint(f)).max();
    let timeline_end_ns = match (last_frame_bt, latest_log_bt) {
        (Some(f), Some(l)) => f.max(l),
        (Some(f), None) => f,
        (None, Some(l)) => l,
        (None, None) => collapsed.last().map_or(0, |(bt, _)| *bt),
    };

    let mut spans = Vec::with_capacity(collapsed.len());
    for i in 0..collapsed.len() {
        let (start_ns, ref comp) = collapsed[i];
        let end_ns = if let Some((next_start, _)) = collapsed.get(i + 1) {
            (*next_start).max(start_ns)
        } else {
            timeline_end_ns.max(start_ns)
        };

        let (start_frame_index, end_frame_index) =
            frame_span_for_range(frames.iter().copied(), start_ns, end_ns);
        spans.push(ScreenSpan {
            start_ns,
            end_ns,
            start_frame_index,
            end_frame_index,
            package_name: comp.package_name.clone(),
            activity_name: comp.activity_name.clone(),
            short_name: comp.short_name.clone(),
            start_offset_ms: None,
            end_offset_ms: None,
        });
    }
    spans
}

/// Android system UI / launcher packages excluded from automatic foreground per-app log
/// extraction (unless the package itself crashed).
const EXCLUDED_SYSTEM_UI_PACKAGES: &[&str] = &[
    "android",
    "com.android.systemui",
    "com.google.android.apps.nexuslauncher",
    "com.android.launcher",
    "com.android.launcher2",
    "com.android.launcher3",
    "com.sec.android.app.launcher",
    "com.miui.home",
    "com.oppo.launcher",
    "com.huawei.android.launcher",
];

/// Returns `true` if `package_name` is an Android system UI or launcher package that should
/// not generate a dedicated `app-logs/<package>.log` file merely for being briefly visible.
#[must_use]
pub fn is_excluded_system_ui_package(package_name: &str) -> bool {
    let trimmed = package_name.trim();
    trimmed.is_empty()
        || EXCLUDED_SYSTEM_UI_PACKAGES
            .iter()
            .any(|excluded| excluded.eq_ignore_ascii_case(trimmed))
}

/// Sanitizes an Android package name into a safe single-segment filename stem for
/// `app-logs/<sanitized>.log` (guaranteed free of path separators or `..` traversal).
#[must_use]
pub fn sanitize_package_filename(package_name: &str) -> String {
    let mut out = String::with_capacity(package_name.len().min(128));
    let mut prev_dot = false;
    for ch in package_name.trim().chars().take(128) {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
            out.push(ch);
            prev_dot = false;
        } else if ch == '.' {
            if !prev_dot && !out.is_empty() {
                out.push('.');
                prev_dot = true;
            }
        } else {
            out.push('_');
            prev_dot = false;
        }
    }
    let trimmed = out.trim_matches(['.', '_']);
    if trimmed.is_empty() {
        "unknown".to_owned()
    } else if is_windows_reserved_stem(trimmed) {
        // Windows refuses to create files whose first dot-separated segment is a legacy device
        // name (`CON.log`, `nul.app.log`, ...); keep the save identical on every OS.
        format!("_{trimmed}")
    } else {
        trimmed.to_owned()
    }
}

/// Returns `true` when the first dot-separated segment of `stem` is a Windows reserved
/// device name (`CON`, `PRN`, `AUX`, `NUL`, `COM0`-`COM9`, `LPT0`-`LPT9`), in any case.
fn is_windows_reserved_stem(stem: &str) -> bool {
    let first = stem
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    match first.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        other => {
            other.len() == 4
                && (other.starts_with("COM") || other.starts_with("LPT"))
                && other.as_bytes()[3].is_ascii_digit()
        }
    }
}

/// Extracted per-app logcat slice ready to be written to `app-logs/<sanitized>.log`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedAppLogs {
    /// Android application package name (e.g., `com.example.app`).
    pub package_name: String,
    /// Normalized bundle-relative path (e.g., `app-logs/com.example.app.log`).
    pub relative_path: String,
    /// Number of crash markers attributed to this package in the bundle.
    pub crash_count: u64,
    /// Chronologically ordered logcat records attributed to this package.
    pub logs: Vec<LogRecord>,
}

/// Pure PC-side resolver attributing [`LogRecord`] entries to Android package names using
/// explicit `LogRecord::package_name`, PID-to-package observations (`wm_on_resume_called`,
/// `am_crash`, `FATAL EXCEPTION`, `ActivityManager: Start proc`), screen transitions, and
/// fully-qualified package tags.
#[derive(Debug, Clone, Default)]
pub struct LogPackageResolver {
    by_boot_pid: BTreeMap<(String, i32), String>,
    by_pid_fallback: BTreeMap<i32, String>,
}

impl LogPackageResolver {
    /// Builds a resolver from `logs` and optional canonical `known_packages` (such as
    /// foreground screen packages and crash packages) to collapse Activity sub-package guesses.
    #[must_use]
    pub fn from_logs(logs: &[LogRecord], known_packages: &BTreeSet<String>) -> Self {
        let mut resolver = Self::default();

        // Pass 1: high-confidence mappings (`log.package_name`, `am_crash`, `Start proc`, `FATAL EXCEPTION`).
        for (idx, log) in logs.iter().enumerate() {
            let boot = log_boot_id_hint(log).map(str::to_owned);
            if let (Some(pid), Some(pkg)) = (log.pid, log.package_name.as_deref()) {
                let norm = normalize_known_package(pkg, known_packages);
                resolver.record_pid(boot.as_deref(), pid, norm);
            }
            if is_system_server_uid(log.uid)
                && let Some(crash) =
                    parse_am_crash_text_for_event_tag(log.tag.as_deref(), &log.message)
                && !crash.package_name.is_empty()
            {
                let norm = normalize_known_package(&crash.package_name, known_packages);
                resolver.record_pid(boot.as_deref(), crash.pid, norm);
            }
            if log.tag.as_deref().map(str::trim) == Some("AndroidRuntime")
                && let Some(fb) = parse_fatal_exception_at(logs, idx)
                && let Some(pid) = fb.pid.or(log.pid)
                && !fb.package_name.is_empty()
                && fb.package_name != "unknown"
            {
                let norm = normalize_known_package(&fb.package_name, known_packages);
                resolver.record_pid(boot.as_deref(), pid, norm);
            }
            if is_system_server_uid(log.uid)
                && matches!(
                    log.tag.as_deref().map(str::trim),
                    Some("ActivityManager" | "ActivityTaskManager")
                )
                && let Some((pid, pkg)) = parse_start_proc_line(&log.message)
            {
                let norm = normalize_known_package(&pkg, known_packages);
                resolver.record_pid(boot.as_deref(), pid, norm);
            }
        }

        // Pass 2: `wm_on_resume_called` / `am_on_resume_called` (`30022`) is emitted by
        // ActivityThread inside the app's own PID.
        for log in logs {
            if is_app_resume_event_tag(log.tag.as_deref())
                && let Some(pid) = log.pid
                && let Some(comp) = parse_screen_transition_line(log.tag.as_deref(), &log.message)
            {
                let boot = log_boot_id_hint(log);
                let norm = normalize_known_package(&comp.package_name, known_packages);
                if resolver.package_for_pid(boot, pid).is_none() {
                    resolver.record_pid(boot, pid, norm);
                }
            }
        }

        resolver
    }

    fn record_pid(&mut self, boot_id: Option<&str>, pid: i32, package_name: String) {
        if package_name.trim().is_empty() {
            return;
        }
        if let Some(boot) = boot_id {
            self.by_boot_pid
                .insert((boot.to_owned(), pid), package_name.clone());
        }
        self.by_pid_fallback.entry(pid).or_insert(package_name);
    }

    /// Returns the resolved package name for `(boot_id, pid)`, if observed.
    #[must_use]
    pub fn package_for_pid(&self, boot_id: Option<&str>, pid: i32) -> Option<&str> {
        if let Some(boot) = boot_id
            && let Some(pkg) = self.by_boot_pid.get(&(boot.to_owned(), pid))
        {
            return Some(pkg.as_str());
        }
        self.by_pid_fallback.get(&pid).map(String::as_str)
    }

    /// Returns `true` if `log` belongs to `target_package`.
    #[must_use]
    pub fn matches_package(&self, log: &LogRecord, target_package: &str) -> bool {
        let target = target_package.trim();
        if target.is_empty() {
            return false;
        }
        if let Some(explicit) = log.package_name.as_deref() {
            if explicit == target {
                return true;
            }
            // If the log explicitly came from a *different* non-system app process,
            // never misattribute it to `target`.
            if !matches!(explicit, "android" | "system_server") {
                return false;
            }
        }
        if let Some(pid) = log.pid
            && let Some(resolved) = self.package_for_pid(log_boot_id_hint(log), pid)
            && resolved == target
        {
            return true;
        }
        matches_non_pid_package_signals(log, target)
    }
}

fn matches_non_pid_package_signals(log: &LogRecord, target: &str) -> bool {
    if is_system_server_uid(log.uid)
        && let Some(crash) = parse_am_crash_text_for_event_tag(log.tag.as_deref(), &log.message)
        && crash.package_name == target
    {
        return true;
    }
    if is_authentic_screen_transition_record(log)
        && let Some(comp) = parse_screen_transition_line(log.tag.as_deref(), &log.message)
        && (comp.package_name == target || comp.activity_name.starts_with(&format!("{target}.")))
    {
        return true;
    }
    if let Some(tag) = log.tag.as_deref().map(str::trim)
        && (tag == target || tag.starts_with(&format!("{target}.")))
    {
        return true;
    }
    false
}

#[derive(Clone, Copy)]
enum OwnerEvidence {
    ActivityClass,
    ProcessIdentity,
    ProcessStart,
}

#[derive(Debug, Default)]
struct PidOwners {
    inferred: Option<String>,
    confirmed: Vec<(u64, String)>,
}

impl PidOwners {
    fn observe(&mut self, seq: u64, pkg: &str, evidence: OwnerEvidence) {
        let pkg = pkg.trim();
        if pkg.is_empty() {
            return;
        }
        if matches!(evidence, OwnerEvidence::ActivityClass) {
            if self.confirmed.is_empty() && self.inferred.is_none() {
                self.inferred = Some(pkg.to_owned());
            }
            return;
        }
        if self
            .confirmed
            .last()
            .is_none_or(|(_, previous)| previous != pkg)
        {
            // The first positive identification (e.g. a crash) also identifies preceding
            // rows. A recorded process start gives a precise lower bound instead.
            let start = if self.confirmed.is_empty()
                && matches!(evidence, OwnerEvidence::ProcessIdentity)
            {
                0
            } else {
                seq
            };
            self.confirmed.push((start, pkg.to_owned()));
        }
    }

    fn at(&self, seq: u64) -> Option<&str> {
        let end = self.confirmed.partition_point(|(start, _)| *start <= seq);
        self.confirmed[..end]
            .last()
            .map(|(_, pkg)| pkg.as_str())
            .or(self.inferred.as_deref())
    }
}

/// Sequence-ordered package observations used by paginated log queries.
#[derive(Debug, Default)]
pub struct LogPackageTimeline {
    epochs: BTreeMap<Option<String>, BTreeMap<i32, PidOwners>>,
}

impl LogPackageTimeline {
    /// Builds ownership transitions for each PID independently in each boot.
    #[must_use]
    pub fn from_logs(logs: &[LogRecord]) -> Self {
        let mut timeline = Self::default();
        for (idx, log) in logs.iter().enumerate() {
            let mut observe = |pid, pkg: &str, evidence| {
                timeline
                    .epochs
                    .entry(log_boot_id_hint(log).map(str::to_owned))
                    .or_default()
                    .entry(pid)
                    .or_default()
                    .observe(log.seq, pkg, evidence);
            };
            if let (Some(pid), Some(pkg)) = (log.pid, log.package_name.as_deref()) {
                observe(pid, pkg, OwnerEvidence::ProcessIdentity);
            } else if is_app_resume_event_tag(log.tag.as_deref())
                && let Some(pid) = log.pid
                && let Some(comp) = parse_screen_transition_line(log.tag.as_deref(), &log.message)
            {
                observe(pid, &comp.package_name, OwnerEvidence::ActivityClass);
            }
            if is_system_server_uid(log.uid) {
                if let Some(crash) =
                    parse_am_crash_text_for_event_tag(log.tag.as_deref(), &log.message)
                {
                    observe(
                        crash.pid,
                        &crash.package_name,
                        OwnerEvidence::ProcessIdentity,
                    );
                }
                if matches!(
                    log.tag.as_deref().map(str::trim),
                    Some("ActivityManager" | "ActivityTaskManager")
                ) && let Some((pid, pkg)) = parse_start_proc_line(&log.message)
                {
                    observe(pid, &pkg, OwnerEvidence::ProcessStart);
                }
            }
            if log.tag.as_deref().map(str::trim) == Some("AndroidRuntime")
                && let Some(crash) = parse_fatal_exception_at(logs, idx)
                && let Some(pid) = crash.pid.or(log.pid)
                && crash.package_name != "unknown"
            {
                observe(pid, &crash.package_name, OwnerEvidence::ProcessIdentity);
            }
        }
        timeline
    }

    /// Applies the PID owner at the row's sequence and the usual non-PID signals.
    #[must_use]
    pub fn matches_package(&self, log: &LogRecord, target: &str) -> bool {
        if let Some(pkg) = log.package_name.as_deref() {
            if pkg == target {
                return true;
            }
            if !matches!(pkg, "android" | "system_server") {
                return false;
            }
        }
        if let Some(pid) = log.pid
            && let Some(epochs) = self
                .epochs
                .get(&log_boot_id_hint(log).map(str::to_owned))
                .and_then(|pids| pids.get(&pid))
            && epochs.at(log.seq).is_some_and(|pkg| {
                pkg == target
                    || pkg
                        .strip_prefix(target)
                        .is_some_and(|suffix| suffix.starts_with('.'))
            })
        {
            return true;
        }
        matches_non_pid_package_signals(log, target)
    }
}

fn normalize_known_package(candidate: &str, known_packages: &BTreeSet<String>) -> String {
    let trimmed = candidate.trim();
    if known_packages.contains(trimmed) {
        return trimmed.to_owned();
    }
    // If `candidate` is an Activity Java sub-package (e.g. `com.example.app.ui`) while
    // `com.example.app` is a known foreground/crashed app package, collapse to the shortest
    // matching known package prefix.
    if let Some(matched) = known_packages
        .iter()
        .filter(|pkg| trimmed.starts_with(&format!("{pkg}.")))
        .min_by_key(|pkg| pkg.len())
    {
        return matched.clone();
    }
    trimmed.to_owned()
}

/// Parses `ActivityManager: Start proc 12345:com.example.app:worker/u0a234 ...` into `(pid, package)`.
fn parse_start_proc_line(message: &str) -> Option<(i32, String)> {
    let rest = message.trim().strip_prefix("Start proc ")?;
    let token = rest.split_whitespace().next()?;
    let (pid_str, proc_and_uid) = token.split_once(':')?;
    let pid = pid_str.parse::<i32>().ok()?;
    let process_name = proc_and_uid.split('/').next()?;
    let pkg = package_from_process(process_name);
    (!pkg.is_empty()).then_some((pid, pkg))
}

/// Extracts per-app logcat files strictly for foreground non-system apps observed in
/// `timeline.screens` plus any app that crashed in `timeline.crashes`.
///
/// Background processes such as `com.google.android.gms` and system UI / launcher packages
/// are intentionally excluded so saved bundles stay clean and bounded.
#[must_use]
pub fn extract_foreground_app_logs(
    logs: &[LogRecord],
    frames: &[FrameRecord],
) -> Vec<ExtractedAppLogs> {
    extract_app_logs_for_packages(logs, frames, BTreeSet::new())
}

/// Extracts matching rows for current foreground/crash packages and packages inherited from a source bundle.
#[must_use]
pub fn extract_app_logs_for_packages(
    logs: &[LogRecord],
    frames: &[FrameRecord],
    mut target_packages: BTreeSet<String>,
) -> Vec<ExtractedAppLogs> {
    let screens = extract_screen_spans(logs, frames);
    let crashes = extract_crash_markers(logs, frames);

    for span in &screens {
        if !is_excluded_system_ui_package(&span.package_name) {
            target_packages.insert(span.package_name.clone());
        }
    }

    let mut crash_counts: BTreeMap<String, u64> = BTreeMap::new();
    for crash in &crashes {
        if !crash.package_name.trim().is_empty() && crash.package_name != "unknown" {
            target_packages.insert(crash.package_name.clone());
            *crash_counts.entry(crash.package_name.clone()).or_insert(0) += 1;
        }
    }

    if target_packages.is_empty() {
        return Vec::new();
    }

    let resolver = LogPackageResolver::from_logs(logs, &target_packages);
    let mut used_paths: BTreeSet<String> = BTreeSet::new();
    let mut extracted = Vec::new();

    for pkg in &target_packages {
        let matched: Vec<LogRecord> = logs
            .iter()
            .filter(|log| resolver.matches_package(log, pkg))
            .cloned()
            .collect();
        if matched.is_empty() {
            continue;
        }
        let stem = sanitize_package_filename(pkg);
        // De-duplicate case-insensitively: macOS / Windows filesystems would otherwise map
        // `com.Example.app` and `com.example.app` onto the same staged file.
        let mut rel_path = format!("app-logs/{stem}.log");
        let mut suffix = 2_u32;
        while !used_paths.insert(rel_path.to_ascii_lowercase()) {
            rel_path = format!("app-logs/{stem}-{suffix}.log");
            suffix = suffix.saturating_add(1);
        }
        extracted.push(ExtractedAppLogs {
            package_name: pkg.clone(),
            relative_path: rel_path,
            crash_count: crash_counts.get(pkg).copied().unwrap_or(0),
            logs: matched,
        });
    }

    extracted
}

/// Filters `logs` to those attributed to `package_name` using [`LogPackageResolver`].
#[must_use]
pub fn filter_logs_by_package(
    all_bundle_logs: &[LogRecord],
    candidate_logs: &[LogRecord],
    package_name: &str,
) -> Vec<LogRecord> {
    let target = package_name.trim();
    if target.is_empty() {
        return candidate_logs.to_vec();
    }
    let mut known = BTreeSet::new();
    known.insert(target.to_owned());
    let resolver = LogPackageResolver::from_logs(all_bundle_logs, &known);
    candidate_logs
        .iter()
        .filter(|log| resolver.matches_package(log, target))
        .cloned()
        .collect()
}

/// Validation failure returned by [`validate_png_bytes`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PngValidationError {
    /// Payload did not begin with the 8-byte PNG signature `\x89PNG\r\n\x1a\n`.
    #[error("invalid PNG signature")]
    InvalidSignature,
    /// First chunk was not a valid 13-byte `IHDR` header.
    #[error("missing or malformed PNG IHDR chunk")]
    InvalidIhdr,
    /// Declared PNG width or height was zero.
    #[error("PNG image dimensions must be positive (got {width}x{height})")]
    ZeroDimensions {
        /// Parsed image width.
        width: u32,
        /// Parsed image height.
        height: u32,
    },
    /// Chunk framing was truncated or lacked a terminating `IEND` chunk.
    #[error("truncated or incomplete PNG chunk stream (missing IEND)")]
    TruncatedStream,
}

/// Validates PNG signature, `IHDR` chunk dimensions, and chunk framing through `IEND`.
///
/// Returns `(width, height)` in pixels on success.
///
/// # Errors
///
/// Returns [`PngValidationError`] if the bytes are not a complete, well-formed PNG image.
pub fn validate_png_bytes(bytes: &[u8]) -> Result<(u32, u32), PngValidationError> {
    // Minimal PNG: 8-byte signature + 25-byte IHDR + 12-byte IEND = 45 bytes.
    if bytes.len() < PNG_SIGNATURE.len() || bytes[..8] != PNG_SIGNATURE {
        return Err(PngValidationError::InvalidSignature);
    }
    // IHDR must be the very first chunk and have length 13.
    if bytes.len() < 33 {
        return Err(PngValidationError::InvalidIhdr);
    }
    let ihdr_len = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    if ihdr_len != 13 || &bytes[12..16] != b"IHDR" {
        return Err(PngValidationError::InvalidIhdr);
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    if width == 0 || height == 0 {
        return Err(PngValidationError::ZeroDimensions { width, height });
    }

    // Walk chunk headers to verify the stream is not truncated and terminates with IEND.
    let mut pos = 8_usize;
    let mut saw_iend = false;
    while pos + 12 <= bytes.len() {
        let len_u32 =
            u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]);
        let chunk_type = &bytes[pos + 4..pos + 8];
        let Ok(chunk_len) = usize::try_from(len_u32) else {
            return Err(PngValidationError::TruncatedStream);
        };
        let Some(next_pos) = pos.checked_add(12).and_then(|v| v.checked_add(chunk_len)) else {
            return Err(PngValidationError::TruncatedStream);
        };
        if next_pos > bytes.len() {
            return Err(PngValidationError::TruncatedStream);
        }
        pos = next_pos;
        if chunk_type == b"IEND" {
            saw_iend = true;
            break;
        }
    }
    if !saw_iend {
        return Err(PngValidationError::TruncatedStream);
    }
    Ok((width, height))
}

fn package_from_process(process_name: &str) -> String {
    process_name
        .split(':')
        .next()
        .unwrap_or(process_name)
        .trim()
        .to_owned()
}

fn log_boottime_hint(log: &LogRecord) -> Option<i64> {
    match &log.alignment {
        TimeAlignment::Anchored { boottime_ns, .. } => Some(*boottime_ns),
        TimeAlignment::Bounded {
            min_boottime_ns, ..
        } => Some(*min_boottime_ns),
        TimeAlignment::Unanchored { .. } => log.device_read_boottime_ns,
    }
}

fn frame_boottime_hint(frame: &FrameRecord) -> Option<i64> {
    match &frame.alignment {
        TimeAlignment::Anchored { boottime_ns, .. } => Some(*boottime_ns),
        TimeAlignment::Bounded {
            min_boottime_ns, ..
        } => Some(*min_boottime_ns),
        TimeAlignment::Unanchored { .. } => None,
    }
}

fn log_boot_id_hint(log: &LogRecord) -> Option<&str> {
    log.boot_id.as_deref().or(match &log.alignment {
        TimeAlignment::Anchored { boot_id, .. } | TimeAlignment::Bounded { boot_id, .. } => {
            Some(boot_id.as_str())
        }
        TimeAlignment::Unanchored { .. } => None,
    })
}

fn nearest_frame_index_for_boot(
    frames: &[FrameRecord],
    boot_id: Option<&str>,
    target_boottime_ns: i64,
) -> Option<u64> {
    frames
        .iter()
        .filter(|f| boot_id.is_none_or(|want| f.boot_id == want))
        .filter_map(|f| {
            let bt = frame_boottime_hint(f)?;
            let dist = (i128::from(bt) - i128::from(target_boottime_ns)).unsigned_abs();
            Some((dist, f.frame_index))
        })
        .min_by_key(|&(dist, idx)| (dist, idx))
        .map(|(_, idx)| idx)
}

fn frame_span_for_range<'a>(
    frames: impl IntoIterator<Item = &'a FrameRecord>,
    start_ns: i64,
    end_ns: i64,
) -> (Option<u64>, Option<u64>) {
    let anchored: Vec<(i64, u64)> = frames
        .into_iter()
        .filter_map(|f| Some((frame_boottime_hint(f)?, f.frame_index)))
        .collect();
    if anchored.is_empty() {
        return (None, None);
    }
    let in_range: Vec<u64> = anchored
        .iter()
        .filter(|&&(bt, _)| bt >= start_ns && bt <= end_ns)
        .map(|&(_, idx)| idx)
        .collect();
    if let (Some(&first), Some(&last)) = (in_range.first(), in_range.last()) {
        return (Some(first), Some(last));
    }
    // No frame overlaps this span: report none rather than frames of a different screen.
    (None, None)
}

#[cfg(test)]
mod tests {
    use crate::bundle::{FrameRecord, LogRecord, RecordBucket};
    use crate::time::{TimeAlignment, TimestampSemantics};

    use super::{
        AM_CRASH_EVENT_TAG, CrashAutoSaveDecision, CrashAutoSaveEval, CrashAutoSaveGate,
        PNG_SIGNATURE, PngValidationError, WM_ON_RESUME_CALLED_EVENT_TAG,
        decode_binary_android_event, extract_crash_markers, extract_screen_spans,
        parse_am_crash_binary, parse_am_crash_text, parse_screen_transition_line,
        validate_png_bytes,
    };

    fn make_log(seq: u64, bt_ns: i64, tag: &str, pid: i32, message: &str) -> LogRecord {
        LogRecord {
            seq,
            boot_id: Some("boot-1".to_owned()),
            header_epoch_ns: Some(1_730_000_000_000_000_000 + bt_ns),
            device_read_boottime_ns: Some(bt_ns),
            pc_ingest_cont_ns: bt_ns,
            alignment: TimeAlignment::Anchored {
                boot_id: "boot-1".to_owned(),
                boottime_ns: bt_ns,
                half_width_ns: 1_000_000,
                semantics: TimestampSemantics::LogcatHeaderWall,
            },
            bucket: RecordBucket::Main,
            level: Some("I".to_owned()),
            tag: Some(tag.to_owned()),
            pid: Some(pid),
            tid: Some(pid),
            uid: None,
            package_name: None,
            message: message.to_owned(),
        }
    }

    fn make_frame(idx: u64, bt_ns: i64) -> FrameRecord {
        FrameRecord {
            frame_index: idx,
            boot_id: "boot-1".to_owned(),
            pts_device_mono_ns: bt_ns,
            pc_ingest_cont_ns: bt_ns,
            alignment: TimeAlignment::Anchored {
                boot_id: "boot-1".to_owned(),
                boottime_ns: bt_ns,
                half_width_ns: 0,
                semantics: TimestampSemantics::VideoEncoderInputEstimate,
            },
            is_keyframe: idx.is_multiple_of(30),
            gop_id: idx / 30,
            mp4_sample_offset: 0,
            mp4_sample_size: 100,
        }
    }

    struct BinaryAmCrashFixture<'a> {
        user: i32,
        pid: i32,
        process: &'a str,
        flags: i32,
        exception: &'a str,
        message: &'a str,
        file: &'a str,
        line: i32,
        recoverable: Option<i32>,
    }

    fn encode_binary_am_crash(f: &BinaryAmCrashFixture<'_>) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&AM_CRASH_EVENT_TAG.to_le_bytes());
        buf.push(3); // EVENT_TYPE_LIST
        buf.push(if f.recoverable.is_some() { 9 } else { 8 });
        let push_int = |b: &mut Vec<u8>, v: i32| {
            b.push(0);
            b.extend_from_slice(&v.to_le_bytes());
        };
        let push_str = |b: &mut Vec<u8>, s: &str| {
            b.push(2);
            let len = u32::try_from(s.len()).expect("len fits u32");
            b.extend_from_slice(&len.to_le_bytes());
            b.extend_from_slice(s.as_bytes());
        };
        push_int(&mut buf, f.user);
        push_int(&mut buf, f.pid);
        push_str(&mut buf, f.process);
        push_int(&mut buf, f.flags);
        push_str(&mut buf, f.exception);
        push_str(&mut buf, f.message);
        push_str(&mut buf, f.file);
        push_int(&mut buf, f.line);
        if let Some(rec) = f.recoverable {
            push_int(&mut buf, rec);
        }
        buf
    }

    #[test]
    fn parses_am_crash_8_field_and_9_field_binary_and_text_with_commas() {
        // 8-field API 28-33 text
        let parsed8 = parse_am_crash_text(
            Some("am_crash"),
            "[0,12345,com.example.app:remote,8929190,java.lang.NullPointerException,Attempt to invoke virtual method,MainActivity.kt,42]",
        )
        .expect("8-field text am_crash must parse");
        assert_eq!(parsed8.pid, 12_345, "pid must match");
        assert_eq!(parsed8.process_name, "com.example.app:remote", "process");
        assert_eq!(parsed8.package_name, "com.example.app", "package");
        assert_eq!(
            parsed8.exception_class, "java.lang.NullPointerException",
            "exception class"
        );
        assert_eq!(parsed8.line, 42, "line number");
        assert!(parsed8.is_fatal(), "8-field am_crash is fatal");

        // 9-field API 34+ text with commas inside the message!
        let parsed9 = parse_am_crash_text(
            None,
            "am_crash: [0,54321,com.example.shop,8929190,java.lang.IllegalStateException,Expected A, B, or C,Checkout.kt,88,0]",
        )
        .expect("9-field text am_crash with commas in message must parse");
        assert_eq!(parsed9.pid, 54_321, "pid");
        assert_eq!(
            parsed9.message, "Expected A, B, or C",
            "commas in message must be preserved"
        );
        assert_eq!(parsed9.file, "Checkout.kt", "file");
        assert_eq!(parsed9.line, 88, "line");
        assert!(parsed9.is_fatal(), "recoverable=0 must be fatal");

        // 9-field API 34+ recoverable=1 must NOT be fatal
        let rec = parse_am_crash_text(
            Some("am_crash"),
            "[0,54321,com.example.shop,8929190,java.lang.RuntimeException,soft,A.kt,10,1]",
        )
        .expect("recoverable am_crash must parse");
        assert!(!rec.is_fatal(), "recoverable=1 must not be fatal");

        // Binary 8-field and 9-field
        let bin8 = encode_binary_am_crash(&BinaryAmCrashFixture {
            user: 0,
            pid: 2222,
            process: "com.example.bin8",
            flags: 1,
            exception: "java.lang.ArithmeticException",
            message: "divide by zero",
            file: "Calc.java",
            line: 19,
            recoverable: None,
        });
        let p_bin8 = parse_am_crash_binary(&bin8).expect("binary 8-field must parse");
        assert_eq!(p_bin8.pid, 2222, "binary 8-field pid");
        assert!(p_bin8.is_fatal(), "binary 8-field fatal");

        let decoded_ev = decode_binary_android_event(&bin8).expect("generic decode");
        assert_eq!(decoded_ev.tag_name, "am_crash", "tag name");
        let reparsed =
            parse_am_crash_text(Some(&decoded_ev.tag_name), &decoded_ev.message).expect("reparse");
        assert_eq!(reparsed, p_bin8, "binary -> text -> parse round-trip");

        let bin9 = encode_binary_am_crash(&BinaryAmCrashFixture {
            user: 0,
            pid: 3333,
            process: "com.example.bin9",
            flags: 1,
            exception: "java.lang.Error",
            message: "boom",
            file: "Main.kt",
            line: 7,
            recoverable: Some(1),
        });
        let p_bin9 = parse_am_crash_binary(&bin9).expect("binary 9-field must parse");
        assert!(!p_bin9.is_fatal(), "binary recoverable=1 is non-fatal");
    }

    #[test]
    fn extracts_crash_markers_with_fatal_fallback_dedup_and_frame_hints() {
        let frames = vec![
            make_frame(0, 10_000_000_000),
            make_frame(1, 11_000_000_000),
            make_frame(2, 12_000_000_000),
            make_frame(3, 25_000_000_000),
        ];
        let logs = vec![
            // Crash 1: both AndroidRuntime FATAL EXCEPTION and am_crash -> deduplicated to 1
            make_log(
                1,
                11_050_000_000,
                "AndroidRuntime",
                4001,
                "FATAL EXCEPTION: main\nProcess: com.example.app, PID: 4001\njava.lang.NullPointerException: null ref\n    at com.example.app.A.run(A.kt:12)",
            ),
            make_log(
                2,
                11_100_000_000,
                "am_crash",
                1000,
                "[0,4001,com.example.app,8929190,java.lang.NullPointerException,null ref,A.kt,12,0]",
            ),
            // Recoverable crash -> ignored
            make_log(
                3,
                15_000_000_000,
                "am_crash",
                1000,
                "[0,4002,com.example.app,0,java.lang.RuntimeException,ignored,B.kt,1,1]",
            ),
            // Crash 2: multi-line consecutive AndroidRuntime records without am_crash
            make_log(
                4,
                24_900_000_000,
                "AndroidRuntime",
                5002,
                "FATAL EXCEPTION: bg",
            ),
            make_log(
                5,
                24_901_000_000,
                "AndroidRuntime",
                5002,
                "Process: com.other.svc:bg, PID: 5002",
            ),
            make_log(
                6,
                24_902_000_000,
                "AndroidRuntime",
                5002,
                "java.lang.IllegalArgumentException: bad input",
            ),
        ];

        let markers = extract_crash_markers(&logs, &frames);
        assert_eq!(
            markers.len(),
            2,
            "must emit 2 markers (1 deduped am_crash + 1 FATAL fallback)"
        );
        assert_eq!(markers[0].source_log_seq, 2, "must prefer am_crash seq 2");
        assert_eq!(markers[0].frame_index_hint, Some(1), "nearest frame is 1");
        assert_eq!(markers[0].package_name, "com.example.app", "pkg 1");

        assert_eq!(markers[1].source_log_seq, 4, "fallback seq 4");
        assert_eq!(markers[1].process_name, "com.other.svc:bg", "proc 2");
        assert_eq!(markers[1].package_name, "com.other.svc", "pkg 2");
        assert_eq!(
            markers[1].exception_class, "java.lang.IllegalArgumentException",
            "exception 2"
        );
        assert_eq!(markers[1].message, "bad input", "message 2");
        assert_eq!(markers[1].frame_index_hint, Some(3), "nearest frame is 3");
    }

    #[test]
    fn crash_autosave_gate_enforces_debounce_and_storage_pressure_suppression() {
        let mut gate = CrashAutoSaveGate::new();

        assert_eq!(
            gate.evaluate(CrashAutoSaveEval {
                enabled: false,
                storage_pressure_active: false,
                cooldown_secs: 15,
                now_ns: 1_000_000_000,
            }),
            CrashAutoSaveDecision::Disabled,
            "disabled config must not trigger"
        );
        assert_eq!(
            gate.evaluate(CrashAutoSaveEval {
                enabled: true,
                storage_pressure_active: true,
                cooldown_secs: 15,
                now_ns: 2_000_000_000,
            }),
            CrashAutoSaveDecision::SuppressedStoragePressure,
            "storage pressure must suppress auto-save"
        );
        assert_eq!(
            gate.evaluate(CrashAutoSaveEval {
                enabled: true,
                storage_pressure_active: false,
                cooldown_secs: 15,
                now_ns: 10_000_000_000,
            }),
            CrashAutoSaveDecision::Trigger,
            "first crash when enabled and healthy must trigger"
        );
        assert_eq!(
            gate.evaluate(CrashAutoSaveEval {
                enabled: true,
                storage_pressure_active: false,
                cooldown_secs: 15,
                now_ns: 24_999_999_999,
            }),
            CrashAutoSaveDecision::SuppressedCooldown,
            "crash within 15s cooldown must be debounced"
        );
        assert_eq!(
            gate.evaluate(CrashAutoSaveEval {
                enabled: true,
                storage_pressure_active: false,
                cooldown_secs: 15,
                now_ns: 25_000_000_000,
            }),
            CrashAutoSaveDecision::Trigger,
            "crash at exactly 15s after previous trigger must fire"
        );
        // Storage pressure arriving later still takes precedence over cooldown.
        assert_eq!(
            gate.evaluate(CrashAutoSaveEval {
                enabled: true,
                storage_pressure_active: true,
                cooldown_secs: 15,
                now_ns: 50_000_000_000,
            }),
            CrashAutoSaveDecision::SuppressedStoragePressure,
            "storage pressure must always suppress"
        );
    }

    #[test]
    fn screen_spans_preserve_boot_order_and_epoch_frame_bounds() {
        let a = make_log(0, 100, "wm_resumed_activity", 1000, "[0,1,com.test/.First]");
        let mut b = make_log(1, 5, "wm_resumed_activity", 1000, "[0,1,com.test/.Second]");
        b.boot_id = Some("boot-2".into());
        b.pc_ingest_cont_ns = 120;
        let mut frame = make_frame(1, 10);
        frame.boot_id = "boot-2".into();
        let spans = extract_screen_spans(&[a, b], &[make_frame(0, 110), frame]);
        let bounds: Vec<_> = spans
            .iter()
            .map(|s| (s.start_ns, s.end_ns, s.start_frame_index, s.end_frame_index))
            .collect();
        assert_eq!(
            bounds,
            [(100, 110, Some(0), Some(0)), (5, 10, Some(1), Some(1))]
        );
    }

    #[test]
    fn parses_screen_transitions_and_builds_collapsed_spans_with_frames() {
        let comp = parse_screen_transition_line(
            Some("wm_on_resume_called"),
            "[216375162,com.example.app/.ui.DetailActivity,RESUME_ACTIVITY,5]",
        )
        .expect("wm_on_resume_called must parse");
        assert_eq!(comp.package_name, "com.example.app", "pkg");
        assert_eq!(
            comp.activity_name, "com.example.app.ui.DetailActivity",
            "fqcn"
        );
        assert_eq!(comp.short_name, "DetailActivity", "short name");

        // Binary wm_on_resume_called decoding
        let mut bin_resume = Vec::new();
        bin_resume.extend_from_slice(&WM_ON_RESUME_CALLED_EVENT_TAG.to_le_bytes());
        bin_resume.push(3); // list
        bin_resume.push(3); // 3 items
        bin_resume.push(0); // int
        bin_resume.extend_from_slice(&99_i32.to_le_bytes());
        bin_resume.push(2); // string
        let cname = b"com.example.app.MainActivity";
        bin_resume.extend_from_slice(&u32::try_from(cname.len()).expect("len").to_le_bytes());
        bin_resume.extend_from_slice(cname);
        bin_resume.push(2);
        bin_resume.extend_from_slice(&6_u32.to_le_bytes());
        bin_resume.extend_from_slice(b"RESUME");
        let dec = decode_binary_android_event(&bin_resume).expect("decode wm_on_resume_called");
        assert_eq!(dec.tag_name, "wm_on_resume_called", "tag name");
        let parsed_bin =
            parse_screen_transition_line(Some(&dec.tag_name), &dec.message).expect("parse");
        assert_eq!(parsed_bin.short_name, "MainActivity", "short name from bin");

        let frames = vec![
            make_frame(0, 1_000_000_000),
            make_frame(1, 2_000_000_000),
            make_frame(2, 5_000_000_000),
            make_frame(3, 8_000_000_000),
        ];
        let logs = vec![
            make_log(
                1,
                1_000_000_000,
                "wm_resumed_activity",
                1000,
                "[0,111,com.example.app/.HomeActivity]",
            ),
            // Duplicate transition for HomeActivity 20ms later -> collapsed
            make_log(
                2,
                1_020_000_000,
                "ActivityTaskManager",
                1000,
                "Displayed com.example.app/.HomeActivity for user 0: +120ms",
            ),
            make_log(
                3,
                4_500_000_000,
                "ActivityManager",
                1000,
                "Displayed com.example.app/com.example.app.SettingsActivity: +85ms",
            ),
            make_log(
                4,
                7_500_000_000,
                "wm_on_resume_called",
                2000,
                "[222,com.example.app.HomeActivity,RESUME_ACTIVITY,3]",
            ),
        ];

        let spans = extract_screen_spans(&logs, &frames);
        assert_eq!(spans.len(), 3, "must produce 3 collapsed screen spans");
        assert_eq!(spans[0].short_name, "HomeActivity", "span 0");
        assert_eq!(spans[0].start_ns, 1_000_000_000, "span 0 start");
        assert_eq!(spans[0].end_ns, 4_500_000_000, "span 0 end");
        assert_eq!(spans[0].start_frame_index, Some(0), "span 0 start frame");
        assert_eq!(spans[0].end_frame_index, Some(1), "span 0 end frame");

        assert_eq!(spans[1].short_name, "SettingsActivity", "span 1");
        assert_eq!(spans[1].start_frame_index, Some(2), "span 1 start frame");
        assert_eq!(spans[1].end_frame_index, Some(2), "span 1 end frame");

        assert_eq!(spans[2].short_name, "HomeActivity", "span 2");
        assert_eq!(
            spans[2].end_ns, 8_000_000_000,
            "span 2 extends to last frame"
        );
        assert_eq!(spans[2].start_frame_index, Some(3), "span 2 start frame");
        assert_eq!(spans[2].end_frame_index, Some(3), "span 2 end frame");
    }

    #[test]
    fn validates_png_signature_ihdr_dimensions_and_iend() {
        let mut valid_png = Vec::new();
        valid_png.extend_from_slice(&PNG_SIGNATURE);
        valid_png.extend_from_slice(&13_u32.to_be_bytes());
        valid_png.extend_from_slice(b"IHDR");
        valid_png.extend_from_slice(&1080_u32.to_be_bytes());
        valid_png.extend_from_slice(&2400_u32.to_be_bytes());
        valid_png.extend_from_slice(&[8, 6, 0, 0, 0]); // bit depth, color type, etc.
        valid_png.extend_from_slice(&0_u32.to_be_bytes()); // CRC
        valid_png.extend_from_slice(&0_u32.to_be_bytes()); // IEND len
        valid_png.extend_from_slice(b"IEND");
        valid_png.extend_from_slice(&0_u32.to_be_bytes()); // CRC

        assert_eq!(
            validate_png_bytes(&valid_png),
            Ok((1080, 2400)),
            "valid PNG must report dimensions"
        );
        assert_eq!(
            validate_png_bytes(b"not a png"),
            Err(PngValidationError::InvalidSignature),
            "bad signature must fail"
        );
        let mut truncated = valid_png.clone();
        truncated.truncate(valid_png.len() - 4);
        assert_eq!(
            validate_png_bytes(&truncated),
            Err(PngValidationError::TruncatedStream),
            "truncated PNG must fail"
        );
    }

    const SEC: i64 = 1_000_000_000;

    #[test]
    fn crash_gate_resets_when_event_clock_restarts_in_a_new_session() {
        let mut gate = CrashAutoSaveGate::new();
        let eval = |now_ns| CrashAutoSaveEval {
            enabled: true,
            storage_pressure_active: false,
            cooldown_secs: 15,
            now_ns,
        };
        assert_eq!(
            gate.evaluate(eval(1_000 * SEC)),
            CrashAutoSaveDecision::Trigger
        );
        // New (replayed / mock) session whose `pc_cont_ns` domain restarted at a lower value.
        assert_eq!(
            gate.evaluate(eval(5 * SEC)),
            CrashAutoSaveDecision::Trigger,
            "a lower timestamp must not wedge the gate"
        );
        assert_eq!(
            gate.evaluate(eval(10 * SEC)),
            CrashAutoSaveDecision::SuppressedCooldown,
            "normal debounce still applies within the new session"
        );
    }

    #[test]
    fn binary_am_crash_tolerates_trailing_terminator_but_not_garbage() {
        let raw = encode_binary_am_crash(&BinaryAmCrashFixture {
            user: 0,
            pid: 42,
            process: "com.example.app",
            flags: 0,
            exception: "java.lang.RuntimeException",
            message: "boom",
            file: "A.kt",
            line: 7,
            recoverable: None,
        });
        assert!(parse_am_crash_binary(&raw).is_some(), "baseline parses");
        let mut with_newline = raw.clone();
        with_newline.push(b'\n');
        assert!(
            parse_am_crash_binary(&with_newline).is_some(),
            "trailing newline tolerated"
        );
        let mut with_garbage = raw;
        with_garbage.push(0x7f);
        assert!(
            parse_am_crash_binary(&with_garbage).is_none(),
            "garbage still rejected"
        );
    }

    #[test]
    fn frame_span_for_range_reports_none_when_no_frame_is_inside() {
        let frames = vec![make_frame(0, SEC), make_frame(1, 10 * SEC)];
        assert_eq!(
            super::frame_span_for_range(&frames, 3 * SEC, 5 * SEC),
            (None, None)
        );
        assert_eq!(
            super::frame_span_for_range(&frames, 0, 2 * SEC),
            (Some(0), Some(0))
        );
    }

    #[test]
    fn crash_gate_abort_rolls_back_a_failed_trigger() {
        let mut gate = CrashAutoSaveGate::new();
        let eval = |now_ns| CrashAutoSaveEval {
            enabled: true,
            storage_pressure_active: false,
            cooldown_secs: 15,
            now_ns,
        };
        assert_eq!(
            gate.evaluate(eval(100 * SEC)),
            CrashAutoSaveDecision::Trigger
        );
        gate.abort_last_trigger();
        assert_eq!(
            gate.last_trigger_ns(),
            None,
            "failed first save leaves gate open"
        );
        assert_eq!(
            gate.evaluate(eval(101 * SEC)),
            CrashAutoSaveDecision::Trigger
        );
        assert_eq!(
            gate.evaluate(eval(110 * SEC)),
            CrashAutoSaveDecision::SuppressedCooldown
        );
        // A later failed trigger restores the earlier successful timestamp, not `None`.
        assert_eq!(
            gate.evaluate(eval(120 * SEC)),
            CrashAutoSaveDecision::Trigger
        );
        gate.abort_last_trigger();
        assert_eq!(gate.last_trigger_ns(), Some(101 * SEC));
    }

    #[test]
    fn app_messages_cannot_spoof_am_crash_and_unanchored_logs_make_no_markers() {
        let body =
            "[0,12345,com.example.app,8929190,java.lang.NullPointerException,boom,Main.kt,42]";
        assert!(
            super::parse_am_crash_text_for_event_tag(Some("AppTag"), &format!("am_crash: {body}"))
                .is_none(),
            "arbitrary tag with am_crash: prefix must not be accepted"
        );
        assert!(
            super::parse_am_crash_text_for_event_tag(Some("am_crash"), body).is_some(),
            "events-buffer tag accepted"
        );

        let spoof = make_log(1, SEC, "AppTag", 99, &format!("am_crash: {body}"));
        assert!(
            extract_crash_markers(&[spoof], &[]).is_empty(),
            "spoofed record yields no marker"
        );

        let mut unanchored = make_log(2, SEC, "am_crash", 1000, body);
        unanchored.bucket = RecordBucket::Unanchored;
        assert!(
            extract_crash_markers(&[unanchored], &[make_frame(0, SEC)]).is_empty(),
            "quarantined logs must never become frame-pinned markers"
        );
        let anchored = make_log(3, SEC, "am_crash", 1000, body);
        assert_eq!(
            extract_crash_markers(&[anchored], &[make_frame(0, SEC)]).len(),
            1
        );
    }

    #[test]
    fn binary_am_crash_survives_truncated_utf8_in_message() {
        let mut raw = encode_binary_am_crash(&BinaryAmCrashFixture {
            user: 0,
            pid: 7,
            process: "com.example.app",
            flags: 0,
            exception: "java.lang.RuntimeException",
            message: "ab",
            file: "A.kt",
            line: 1,
            recoverable: None,
        });
        // Corrupt the 2-byte message into a lone UTF-8 lead byte pair (truncated character).
        let pos = raw
            .windows(2)
            .position(|w| w == b"ab")
            .expect("message bytes");
        raw[pos] = 0xE3;
        raw[pos + 1] = 0x81;
        let parsed = parse_am_crash_binary(&raw).expect("lossy decode must still parse");
        assert!(parsed.message.contains('\u{FFFD}'), "replacement char used");
    }

    #[test]
    fn forged_fatal_exception_and_screen_events_from_other_tags_are_ignored() {
        let fatal = "FATAL EXCEPTION: main\nProcess: com.evil, PID: 7\njava.lang.Error: x";
        let forged_fatal = make_log(1, SEC, "SomeApp", 7, fatal);
        assert_eq!(extract_crash_markers(&[forged_fatal], &[]), Vec::new());
        let real_fatal = make_log(2, SEC, "AndroidRuntime", 7, fatal);
        assert_eq!(extract_crash_markers(&[real_fatal], &[]).len(), 1);

        let forged_screen = make_log(
            3,
            SEC,
            "AppTag",
            9,
            "wm_resumed_activity: [0,1,com.evil/.Fake]",
        );
        assert_eq!(extract_screen_spans(&[forged_screen], &[]), Vec::new());
    }

    #[test]
    fn crash_dedup_distance_never_overflows() {
        let marker = |bt: i64| super::CrashMarker {
            boottime_ns: Some(bt),
            frame_index_hint: None,
            process_name: "p".to_owned(),
            package_name: "p".to_owned(),
            pid: Some(1),
            exception_class: "E".to_owned(),
            message: String::new(),
            source_log_seq: 0,
        };
        assert!(!super::is_same_crash(&marker(i64::MIN), &marker(i64::MAX)));
        assert!(super::is_same_crash(&marker(SEC), &marker(2 * SEC)));
    }

    #[test]
    fn binary_am_crash_with_empty_exception_class_is_rejected_like_text() {
        let raw = encode_binary_am_crash(&BinaryAmCrashFixture {
            user: 0,
            pid: 5,
            process: "com.example.app",
            flags: 0,
            exception: "",
            message: "m",
            file: "A.kt",
            line: 1,
            recoverable: None,
        });
        assert!(parse_am_crash_binary(&raw).is_none());
    }

    #[test]
    fn one_activity_visit_is_one_span_with_the_real_package() {
        let logs = [
            make_log(
                1,
                SEC,
                "wm_resumed_activity",
                1000,
                "[0,7,com.example/.ui.MainActivity]",
            ),
            make_log(
                2,
                SEC + 1,
                "wm_on_resume_called",
                1000,
                "[7,com.example.ui.MainActivity,RESUME]",
            ),
            make_log(
                3,
                3 * SEC,
                "wm_on_resume_called",
                1000,
                "[8,com.example.ui.DetailActivity,RESUME]",
            ),
            make_log(
                4,
                3 * SEC + 1,
                "wm_resumed_activity",
                1000,
                "[0,8,com.example/.ui.DetailActivity]",
            ),
        ];
        let spans = extract_screen_spans(&logs, &[]);
        assert_eq!(
            spans.len(),
            2,
            "no split between class-name and component forms: {spans:?}"
        );
        assert_eq!(spans[0].package_name, "com.example", "real package wins");
        assert_eq!(
            spans[1].package_name, "com.example",
            "real package wins in either order"
        );
    }

    #[test]
    fn fatal_exception_naming_another_pid_is_not_a_marker() {
        let forged = make_log(
            1,
            SEC,
            "AndroidRuntime",
            999,
            "FATAL EXCEPTION: main\nProcess: com.victim, PID: 111\njava.lang.Error: x",
        );
        assert_eq!(extract_crash_markers(&[forged], &[]), Vec::new());
        let own = make_log(
            2,
            SEC,
            "AndroidRuntime",
            111,
            "FATAL EXCEPTION: main\nProcess: com.victim, PID: 111\njava.lang.Error: x",
        );
        assert_eq!(extract_crash_markers(&[own], &[]).len(), 1);
    }

    #[test]
    fn multi_epoch_crash_markers_map_within_matching_boot_id_without_bleed() {
        let make_frame = |frame_index: u64, boot: &str, bt_ns: i64| FrameRecord {
            frame_index,
            boot_id: boot.to_owned(),
            pts_device_mono_ns: bt_ns,
            pc_ingest_cont_ns: bt_ns,
            alignment: TimeAlignment::Anchored {
                boot_id: boot.to_owned(),
                boottime_ns: bt_ns,
                half_width_ns: 1_000_000,
                semantics: TimestampSemantics::VideoEncoderInputEstimate,
            },
            is_keyframe: frame_index == 0 || frame_index == 10,
            gop_id: u64::from(frame_index >= 10),
            mp4_sample_offset: 0,
            mp4_sample_size: 100,
        };
        // Epoch 0: frames 0..=2 at boottime 10s..12s; Epoch 1 (after reboot): frames 10..=12 at 10s..12s
        let frames = vec![
            make_frame(0, "boot-0", 10 * SEC),
            make_frame(1, "boot-0", 11 * SEC),
            make_frame(2, "boot-0", 12 * SEC),
            make_frame(10, "boot-1", 10 * SEC),
            make_frame(11, "boot-1", 11 * SEC),
            make_frame(12, "boot-1", 12 * SEC),
        ];
        let mut crash_e0 = make_log(
            1,
            11 * SEC,
            "am_crash",
            1000,
            "[0,101,com.app,0,java.lang.RuntimeException,e0,A.kt,10,0]",
        );
        crash_e0.boot_id = Some("boot-0".to_owned());
        crash_e0.alignment = TimeAlignment::Anchored {
            boot_id: "boot-0".to_owned(),
            boottime_ns: 11 * SEC,
            half_width_ns: 1_000_000,
            semantics: TimestampSemantics::LogcatDeviceReadBoottime,
        };
        let mut crash_e1 = make_log(
            2,
            11 * SEC,
            "am_crash",
            1000,
            "[0,202,com.app,0,java.lang.IllegalStateException,e1,B.kt,20,0]",
        );
        crash_e1.boot_id = Some("boot-1".to_owned());
        crash_e1.alignment = TimeAlignment::Anchored {
            boot_id: "boot-1".to_owned(),
            boottime_ns: 11 * SEC,
            half_width_ns: 1_000_000,
            semantics: TimestampSemantics::LogcatDeviceReadBoottime,
        };

        let markers = extract_crash_markers(&[crash_e0, crash_e1], &frames);
        assert_eq!(markers.len(), 2);
        assert_eq!(
            markers[0].frame_index_hint,
            Some(1),
            "epoch-0 crash must map to epoch-0 frame"
        );
        assert_eq!(
            markers[1].frame_index_hint,
            Some(11),
            "epoch-1 crash must map to epoch-1 frame without bleeding to epoch-0"
        );
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "exercises multi-app foreground, system UI, GMS background, and crash extraction together"
    )]
    fn t6c_extract_foreground_app_logs_filters_background_gms_and_systemui() {
        use super::{
            extract_foreground_app_logs, filter_logs_by_package, is_excluded_system_ui_package,
            sanitize_package_filename,
        };

        assert!(is_excluded_system_ui_package("com.android.systemui"));
        assert!(is_excluded_system_ui_package(
            "com.google.android.apps.nexuslauncher"
        ));
        assert!(is_excluded_system_ui_package("com.android.launcher3"));
        assert!(is_excluded_system_ui_package("android"));
        assert!(!is_excluded_system_ui_package("com.example.shop"));
        assert_eq!(
            sanitize_package_filename("com.example.shop"),
            "com.example.shop"
        );
        assert_eq!(sanitize_package_filename("../evil/pkg"), "evil_pkg");

        let sec = 1_000_000_000_i64;
        let frames = vec![make_frame(0, 10 * sec), make_frame(1, 11 * sec)];

        let mut gms_log = make_log(1, 10 * sec, "GmsCore", 500, "background sync");
        gms_log.uid = Some(10012);
        gms_log.package_name = Some("com.google.android.gms".to_owned());

        let sysui_resume = make_log(
            2,
            10 * sec,
            "wm_resumed_activity",
            1000,
            "[0,111,com.android.systemui/.StatusBar]",
        );
        let launcher_resume = make_log(
            3,
            10 * sec + 100,
            "wm_resumed_activity",
            1000,
            "[0,112,com.google.android.apps.nexuslauncher/.NexusLauncherActivity]",
        );

        let app_resumed = make_log(
            4,
            10 * sec + 200,
            "wm_resumed_activity",
            1000,
            "[0,113,com.example.shop/.ui.CartActivity]",
        );
        let app_on_resume = make_log(
            5,
            10 * sec + 250,
            "wm_on_resume_called",
            4200,
            "[113,com.example.shop.ui.CartActivity,RESUME_ACTIVITY]",
        );
        let app_log_by_pid = make_log(6, 10 * sec + 300, "CartViewModel", 4200, "checkout tap");

        // A second app that crashed in the background should also be extracted.
        let crash_event = make_log(
            7,
            10 * sec + 400,
            "am_crash",
            1000,
            "[0,4300,com.example.payments,0,java.lang.IllegalStateException,card token expired,Pay.kt,42,0]",
        );
        let crash_app_log = make_log(8, 10 * sec + 390, "PayClient", 4300, "retrying payment");

        let all_logs = vec![
            gms_log,
            sysui_resume,
            launcher_resume,
            app_resumed,
            app_on_resume,
            app_log_by_pid,
            crash_event,
            crash_app_log,
        ];

        let extracted = extract_foreground_app_logs(&all_logs, &frames);
        let pkgs: Vec<&str> = extracted.iter().map(|e| e.package_name.as_str()).collect();
        assert_eq!(
            pkgs,
            vec!["com.example.payments", "com.example.shop"],
            "must include foreground app + crashed app, and exclude gms/systemui/launcher"
        );

        let shop = extracted
            .iter()
            .find(|e| e.package_name == "com.example.shop")
            .expect("shop app logs");
        assert_eq!(shop.relative_path, "app-logs/com.example.shop.log");
        assert_eq!(shop.crash_count, 0);
        assert!(
            shop.logs.iter().any(|l| l.message == "checkout tap"),
            "app PID log must be attributed via wm_on_resume_called"
        );
        assert!(
            !shop.logs.iter().any(|l| l.message == "background sync"),
            "background gms log must never leak into foreground app"
        );

        let payments = extracted
            .iter()
            .find(|e| e.package_name == "com.example.payments")
            .expect("crashed app logs");
        assert_eq!(payments.crash_count, 1);
        assert!(
            payments
                .logs
                .iter()
                .any(|l| l.message == "retrying payment"),
            "crashed app PID log must be attributed via am_crash"
        );

        let filtered = filter_logs_by_package(&all_logs, &all_logs, "com.example.shop");
        assert_eq!(filtered.len(), shop.logs.len());
    }

    #[test]
    fn t6c_app_log_paths_are_case_insensitively_unique_and_never_windows_reserved() {
        use super::{extract_foreground_app_logs, sanitize_package_filename};

        // Windows reserved device names (checked on the first dot-separated segment, any case).
        for reserved in [
            "CON",
            "nul",
            "Aux",
            "PRN",
            "com1",
            "LPT9",
            "con.example",
            "NUL.app",
        ] {
            let stem = sanitize_package_filename(reserved);
            let first = stem
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase();
            assert!(
                !matches!(
                    first.as_str(),
                    "CON" | "PRN" | "AUX" | "NUL" | "COM1" | "LPT9"
                ),
                "{reserved:?} sanitized to Windows-reserved stem {stem:?}"
            );
            assert_ne!(stem, "", "sanitized stem must not be empty");
        }
        assert_eq!(
            sanitize_package_filename("com.console.app"),
            "com.console.app"
        );
        assert_eq!(sanitize_package_filename("console"), "console");

        // Two foreground packages that differ only by case must never share a file on
        // case-insensitive filesystems (macOS / Windows).
        let sec = 1_000_000_000_i64;
        let frames = vec![make_frame(0, 10 * sec), make_frame(1, 11 * sec)];
        let logs = vec![
            make_log(
                1,
                10 * sec,
                "wm_resumed_activity",
                1000,
                "[0,1,com.Example.app/.Main]",
            ),
            make_log(2, 10 * sec + 10, "com.Example.app", 4100, "upper"),
            make_log(
                3,
                10 * sec + 20,
                "wm_resumed_activity",
                1000,
                "[0,2,com.example.app/.Main]",
            ),
            make_log(4, 10 * sec + 30, "com.example.app", 4200, "lower"),
        ];
        let extracted = extract_foreground_app_logs(&logs, &frames);
        assert_eq!(extracted.len(), 2, "both case-variant packages extracted");
        let lowered: std::collections::BTreeSet<String> = extracted
            .iter()
            .map(|e| e.relative_path.to_ascii_lowercase())
            .collect();
        assert_eq!(
            lowered.len(),
            extracted.len(),
            "app-log paths must be unique case-insensitively: {:?}",
            extracted
                .iter()
                .map(|e| &e.relative_path)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "covers Android 9 API 28 binary/text am_* tags, spoofing rejection, screen spans, and foreground app-logs"
    )]
    fn t6e_aos9_am_screen_tags_binary_and_text_build_spans_and_app_logs_safely() {
        use super::{
            AM_RESUME_ACTIVITY_EVENT_TAG, SYSTEM_UID, WM_RESUMED_ACTIVITY_EVENT_TAG,
            extract_foreground_app_logs,
        };

        // 1. Android 9 text formats: am_focused_activity, am_resume_activity, am_on_resume_called.
        let focused = parse_screen_transition_line(
            Some("am_focused_activity"),
            "[0,com.work.aos9/.MainActivity,startedActivity]",
        )
        .expect("am_focused_activity must parse");
        assert_eq!(focused.package_name, "com.work.aos9");
        assert_eq!(focused.activity_name, "com.work.aos9.MainActivity");
        assert_eq!(focused.short_name, "MainActivity");

        // NULL focus clear must return None cleanly.
        assert_eq!(
            parse_screen_transition_line(Some("am_focused_activity"), "[0,NULL,clear]"),
            None
        );

        let resumed = parse_screen_transition_line(
            Some("am_resume_activity"),
            "[0,12345,42,com.work.aos9/.ui.OrderActivity]",
        )
        .expect("am_resume_activity must parse");
        assert_eq!(resumed.package_name, "com.work.aos9");
        assert_eq!(resumed.activity_name, "com.work.aos9.ui.OrderActivity");
        assert_eq!(resumed.short_name, "OrderActivity");

        let on_resume = parse_screen_transition_line(
            Some("am_on_resume_called"),
            "[0,com.work.aos9.ui.OrderActivity,RESUME_ACTIVITY]",
        )
        .expect("am_on_resume_called must parse");
        assert_eq!(on_resume.activity_name, "com.work.aos9.ui.OrderActivity");
        assert_eq!(on_resume.short_name, "OrderActivity");

        // 2. Binary AOSP event tag 30007 (`am_resume_activity`: [User, Token, TaskId, Component])
        //    and 30043 (`am_focused_activity` / `wm_set_resumed_activity`: [User, Component, Reason]).
        let encode_list = |tag: i32, items: &[Result<i32, &str>]| -> Vec<u8> {
            let mut b = Vec::new();
            b.extend_from_slice(&tag.to_le_bytes());
            b.push(3); // EVENT_TYPE_LIST
            b.push(u8::try_from(items.len()).expect("len fits u8"));
            for item in items {
                match item {
                    Ok(v) => {
                        b.push(0);
                        b.extend_from_slice(&v.to_le_bytes());
                    }
                    Err(s) => {
                        b.push(2);
                        b.extend_from_slice(
                            &u32::try_from(s.len()).expect("str len").to_le_bytes(),
                        );
                        b.extend_from_slice(s.as_bytes());
                    }
                }
            }
            b
        };

        let bin_30007 = encode_list(
            AM_RESUME_ACTIVITY_EVENT_TAG,
            &[Ok(0), Ok(99999), Ok(42), Err("com.work.aos9/.MainActivity")],
        );
        let dec_30007 = decode_binary_android_event(&bin_30007).expect("decode 30007");
        assert_eq!(dec_30007.tag_name, "am_resume_activity");
        let parsed_30007 =
            parse_screen_transition_line(Some(&dec_30007.tag_name), &dec_30007.message)
                .expect("parse decoded 30007");
        assert_eq!(parsed_30007.package_name, "com.work.aos9");
        assert_eq!(parsed_30007.short_name, "MainActivity");

        let bin_30043 = encode_list(
            WM_RESUMED_ACTIVITY_EVENT_TAG,
            &[
                Ok(0),
                Err("com.work.aos9/.ui.OrderActivity"),
                Err("startedActivity"),
            ],
        );
        let dec_30043 = decode_binary_android_event(&bin_30043).expect("decode 30043");
        assert_eq!(dec_30043.tag_name, "wm_resumed_activity");
        let parsed_30043 =
            parse_screen_transition_line(Some(&dec_30043.tag_name), &dec_30043.message)
                .expect("parse decoded 30043 [User, Component, Reason]");
        assert_eq!(parsed_30043.package_name, "com.work.aos9");
        assert_eq!(parsed_30043.short_name, "OrderActivity");

        // 3. Full Android 9 timeline: screen spans + app-logs + UID spoofing protection.
        let frames = vec![
            make_frame(0, 10 * SEC),
            make_frame(1, 12 * SEC),
            make_frame(2, 15 * SEC),
        ];

        let mut am_focused = make_log(
            1,
            10 * SEC,
            "am_focused_activity",
            1000,
            "[0,com.work.aos9/.MainActivity,startedActivity]",
        );
        am_focused.uid = Some(SYSTEM_UID);

        // Spoofed am_focused_activity from an untrusted app UID must be ignored!
        let mut spoofed_focused = make_log(
            2,
            10 * SEC + 100,
            "am_focused_activity",
            7777,
            "[0,com.evil.spoof/.FakeActivity,startedActivity]",
        );
        spoofed_focused.uid = Some(10_999);

        // Spoofed am_resume_activity from an untrusted app UID must be ignored!
        let mut spoofed_resume = make_log(
            3,
            10 * SEC + 200,
            "am_resume_activity",
            7777,
            "[0,1,2,com.evil.spoof/.Fake2]",
        );
        spoofed_resume.uid = Some(10_999);

        // Genuine app-emitted am_on_resume_called carries the app's own UID (NOT 1000).
        let mut app_on_resume = make_log(
            4,
            10 * SEC + 300,
            "am_on_resume_called",
            5100,
            "[0,com.work.aos9.MainActivity,RESUME_ACTIVITY]",
        );
        app_on_resume.uid = Some(10_200);

        let mut app_log = make_log(5, 11 * SEC, "WorkApp", 5100, "loaded order screen");
        app_log.uid = Some(10_200);

        let mut am_resume_next = make_log(
            6,
            12 * SEC,
            "am_resume_activity",
            1000,
            "[0,555,77,com.work.aos9/.ui.OrderActivity]",
        );
        am_resume_next.uid = Some(SYSTEM_UID);

        let logs = vec![
            am_focused,
            spoofed_focused,
            spoofed_resume,
            app_on_resume,
            app_log,
            am_resume_next,
        ];

        let spans = extract_screen_spans(&logs, &frames);
        assert_eq!(
            spans.len(),
            2,
            "must emit 2 authentic AOS 9 spans: {spans:?}"
        );
        assert_eq!(spans[0].package_name, "com.work.aos9");
        assert_eq!(spans[0].short_name, "MainActivity");
        assert_eq!(spans[1].package_name, "com.work.aos9");
        assert_eq!(spans[1].short_name, "OrderActivity");
        assert!(
            !spans.iter().any(|s| s.package_name == "com.evil.spoof"),
            "non-system UID must never spoof am_focused_activity or am_resume_activity"
        );

        let extracted = extract_foreground_app_logs(&logs, &frames);
        assert_eq!(extracted.len(), 1, "only authentic foreground AOS 9 app");
        assert_eq!(extracted[0].package_name, "com.work.aos9");
        assert!(
            extracted[0]
                .logs
                .iter()
                .any(|l| l.message == "loaded order screen"),
            "app PID log must be attributed via AOS 9 am_on_resume_called"
        );
    }

    #[test]
    fn opus_review_app_uid_resume_event_cannot_forge_another_apps_screen_package() {
        let frames = vec![make_frame(0, 10 * SEC), make_frame(1, 20 * SEC)];

        // A third-party app (UID mapped to `com.evil.app`) writes EventLog 30022 naming a
        // banking Activity: the span must stay attributed to the writer's own package.
        let mut forged = make_log(
            1,
            10 * SEC,
            "am_on_resume_called",
            7777,
            "[0,com.bank.secure.LoginActivity,RESUME_ACTIVITY]",
        );
        forged.uid = Some(10_999);
        forged.package_name = Some("com.evil.app".to_owned());

        // A genuine app whose Activity lives outside its package namespace: the
        // UID-authenticated package is the real one, not the class's Java package.
        let mut genuine = make_log(
            2,
            15 * SEC,
            "wm_on_resume_called",
            5100,
            "[123,com.work.ui.MainActivity,RESUME_ACTIVITY]",
        );
        genuine.uid = Some(10_200);
        genuine.package_name = Some("com.work.app".to_owned());

        // Unknown / ambiguous (shared) UID: cannot authenticate, keep the parsed component.
        let mut unmapped = make_log(
            3,
            18 * SEC,
            "am_on_resume_called",
            6100,
            "[0,com.shared.app.HomeActivity,RESUME_ACTIVITY]",
        );
        unmapped.uid = Some(10_012);

        let spans = extract_screen_spans(&[forged, genuine, unmapped], &frames);
        assert_eq!(spans.len(), 3, "{spans:?}");
        assert!(
            spans.iter().all(|s| s.package_name != "com.bank.secure"),
            "an app UID must never attribute a screen span to another package: {spans:?}"
        );
        assert_eq!(spans[0].package_name, "com.evil.app");
        assert_eq!(spans[1].package_name, "com.work.app");
        assert_eq!(spans[1].short_name, "MainActivity");
        assert_eq!(spans[2].package_name, "com.shared.app");
    }

    #[test]
    fn opus_review_aos9_am_set_resumed_activity_text_tag_is_a_system_server_screen_tag() {
        let frames = vec![make_frame(0, 10 * SEC), make_frame(1, 20 * SEC)];
        let mut genuine = make_log(
            1,
            10 * SEC,
            "am_set_resumed_activity",
            1000,
            "[0,com.work.aos9/.MainActivity,resumeTopActivityInnerLocked]",
        );
        genuine.uid = Some(SYSTEM_UID_FOR_TESTS);
        let mut spoofed = make_log(
            2,
            12 * SEC,
            "am_set_resumed_activity",
            7777,
            "[0,com.evil.spoof/.Fake,resumeTopActivityInnerLocked]",
        );
        spoofed.uid = Some(10_999);

        let spans = extract_screen_spans(&[genuine, spoofed], &frames);
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert_eq!(spans[0].package_name, "com.work.aos9");
        assert_eq!(spans[0].short_name, "MainActivity");
    }

    const SYSTEM_UID_FOR_TESTS: u32 = super::SYSTEM_UID;
}
