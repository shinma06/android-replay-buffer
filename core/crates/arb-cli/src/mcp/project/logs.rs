//! `arb_query_logs` rows: `full` keeps every daemon field, `compact` keeps what an agent reads.
//!
//! A page also fits a byte budget: when the projected body would exceed [`MAX_BODY_BYTES`], the
//! trailing rows move to the next page through the same exclusive `seq` cursor the daemon uses.
use super::time::Zone;
use serde_json::{Map, Value, json};

/// Largest `limit` an MCP page accepts. The daemon RPC keeps its own 100,000 cap for the IDE/CLI.
pub(in crate::mcp) const MAX_PAGE_ROWS: u64 = 500;
/// Largest serialized `arb_query_logs` body (48 KiB). The first row is always kept.
pub(in crate::mcp) const MAX_BODY_BYTES: usize = 49_152;

/// Row shape requested with `fields`. Every protocol version defaults to `compact`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::mcp) enum Fields {
    Compact,
    Full,
}

impl Fields {
    pub(in crate::mcp) fn from_args(args: &Map<String, Value>) -> Self {
        match args.get("fields").and_then(Value::as_str) {
            Some("full") => Self::Full,
            _ => Self::Compact,
        }
    }
    const fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Full => "full",
        }
    }
}

/// The paging remedy added to an out-of-range `limit`.
pub(in crate::mcp) fn limit_hint(hint: &mut String) {
    use std::fmt::Write as _;
    write!(
        hint,
        "{MAX_PAGE_ROWS} 行を超える分は、応答の next_cursor を cursor に渡して次のページで取得してください。"
    )
    .expect("writing to String");
}

/// Always present in a compact row (null when unknown), in this order.
const COMPACT_REQUIRED: [&str; 7] = [
    "seq",
    "timestamp_utc",
    "offset_ms",
    "frame_index",
    "level",
    "tag",
    "message",
];

/// Projects a `bundle.query_logs` page into at most [`MAX_BODY_BYTES`]. Rows and their order are
/// unchanged; rows beyond the budget are left to the next page.
pub(in crate::mcp) fn project(page: Value, fields: Fields, zone: Zone) -> Value {
    fit_byte_budget(shape(page, fields, zone), MAX_BODY_BYTES)
}

fn shape(mut page: Value, fields: Fields, zone: Zone) -> Value {
    let rows = page["logs"].as_array_mut().map(std::mem::take);
    let mut rows = rows.unwrap_or_default();
    for row in &mut rows {
        zone.localize_row(row, "timestamp_utc", "timestamp_local");
    }
    page["fields"] = json!(fields.as_str());
    if fields == Fields::Compact {
        let shared = shared_boot_id(&rows);
        rows = rows
            .into_iter()
            .map(|row| compact_row(row, shared.as_deref()))
            .collect();
        page["boot_id"] = shared.map_or(Value::Null, Value::String);
    }
    page["logs"] = Value::Array(rows);
    page
}

/// Keeps the longest row prefix whose body fits `budget` bytes (at least one row, so paging always
/// advances) and resumes after the last kept row's `seq`, exactly like a daemon page of that size.
fn fit_byte_budget(mut page: Value, budget: usize) -> Value {
    page["truncated_by_byte_budget"] = json!(false);
    let size = |v: &Value| serde_json::to_vec(v).map_or(usize::MAX, |b| b.len());
    if size(&page) <= budget {
        return page;
    }
    let Some(rows) = page["logs"].as_array_mut().map(std::mem::take) else {
        return page;
    };
    // The envelope as a truncated page: longest cursor, final counts and flags.
    let mut envelope = page.clone();
    envelope["has_more"] = json!(true);
    envelope["truncated_by_byte_budget"] = json!(true);
    envelope["next_cursor"] = json!(u64::MAX.to_string());
    envelope["returned_count"] = json!(rows.len());
    let mut used = size(&envelope);
    let mut keep: usize = 0;
    for row in &rows {
        // Each row costs its bytes plus a separating comma.
        let cost = size(row) + usize::from(keep > 0);
        if keep > 0 && used + cost > budget {
            break;
        }
        used += cost;
        keep += 1;
    }
    let cursor = rows
        .get(keep.saturating_sub(1))
        .and_then(|r| r["seq"].as_u64());
    let Some(cursor) = cursor.filter(|_| keep < rows.len()) else {
        page["logs"] = Value::Array(rows);
        return page;
    };
    let mut rows = rows;
    rows.truncate(keep);
    page["logs"] = Value::Array(rows);
    page["returned_count"] = json!(keep);
    page["has_more"] = json!(true);
    page["next_cursor"] = json!(cursor.to_string());
    page["truncated_by_byte_budget"] = json!(true);
    page
}

/// The boot every row shares, when there is exactly one known boot across a non-empty page.
fn shared_boot_id(rows: &[Value]) -> Option<String> {
    let first = rows.first()?["boot_id"].as_str()?;
    rows.iter()
        .all(|row| row["boot_id"].as_str() == Some(first))
        .then(|| first.to_owned())
}

/// A compact row; it names its own boot only when the page has no single `shared_boot`.
fn compact_row(row: Value, shared_boot: Option<&str>) -> Value {
    let Value::Object(mut full) = row else {
        return row;
    };
    let mut out = Map::new();
    // `time_zone: "host"` replaced the UTC time with `timestamp_local`.
    let local_only = full.contains_key("timestamp_local") && !full.contains_key("timestamp_utc");
    for key in COMPACT_REQUIRED {
        let key = if local_only && key == "timestamp_utc" {
            "timestamp_local"
        } else {
            key
        };
        out.insert(key.into(), full.remove(key).unwrap_or(Value::Null));
    }
    for key in ["timestamp_local", "package_name"] {
        if let Some(value) = full.remove(key).filter(|v| !v.is_null()) {
            out.insert(key.into(), value);
        }
    }
    if shared_boot.is_none() {
        out.insert(
            "boot_id".into(),
            full.remove("boot_id").unwrap_or(Value::Null),
        );
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::{Fields, MAX_BODY_BYTES, fit_byte_budget, project, shape};
    use crate::mcp::{ToolOutput, project::time::Zone, version::ProtocolVersion};
    use arb_core::bundle::{LogRecord, RecordBucket};
    use arb_core::time::{TimeAlignment, TimestampSemantics};
    use proptest::prelude::*;
    use serde_json::{Value, json};

    /// A daemon page built from the production row type.
    fn page(rows: usize, boots: &[&str]) -> Value {
        let logs: Vec<_> = (0..rows)
            .map(|i| {
                let seq = u64::try_from(i).unwrap();
                let boot = boots[i % boots.len()];
                let t = 1_795_829_025_123_000_000 + i64::try_from(i).unwrap() * 7_000_000;
                arbd::protocol::QueryLogRow {
                    record: LogRecord {
                        seq,
                        boot_id: Some(boot.into()),
                        header_epoch_ns: Some(t),
                        device_read_boottime_ns: Some(81_234_567_890 + t % 1_000_000_000),
                        pc_ingest_cont_ns: 9_876_543_210_123 + t % 1_000_000_000,
                        alignment: TimeAlignment::Anchored {
                            boot_id: boot.into(),
                            boottime_ns: 81_234_567_890 + t % 1_000_000_000,
                            half_width_ns: 1_500_000,
                            semantics: TimestampSemantics::LogcatDeviceReadBoottime,
                        },
                        bucket: RecordBucket::Main,
                        level: Some("E".into()),
                        tag: Some("VendorPowerHal".into()),
                        pid: Some(1234),
                        tid: Some(1290),
                        uid: Some(10_123),
                        package_name: Some("com.example.app".into()),
                        message: format!(
                            "Failed to refresh thermal throttling profile for sensor {i}: status=-22 (Invalid argument)"
                        ),
                    },
                    timestamp_utc: Some(format!("2026-11-28T01:23:{:02}.123Z", i % 60)),
                    offset_ms: Some(i64::try_from(i).unwrap() * 7),
                    frame_index: Some(seq),
                }
            })
            .collect();
        json!({"logs":logs,"total_matched":rows,"returned_count":rows,"has_more":false,"next_cursor":null,"unanchored_count":0})
    }

    #[test]
    fn compact_rows_keep_every_row_and_hoist_a_shared_boot() {
        let full = project(page(3, &["boot-a"]), Fields::Full, Zone::UTC);
        let compact = project(page(3, &["boot-a"]), Fields::Compact, Zone::fixed(9 * 3600));
        assert_eq!(full["fields"], "full");
        assert_eq!(compact["fields"], "compact");
        assert_eq!(compact["boot_id"], "boot-a");
        assert_eq!(full["logs"][0]["alignment"]["boot_id"], "boot-a");
        assert!(full["logs"][0].get("timestamp_local").is_none());
        let rows = compact["logs"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        let mut keys: Vec<_> = rows[0].as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "frame_index",
                "level",
                "message",
                "offset_ms",
                "package_name",
                "seq",
                "tag",
                "timestamp_local"
            ]
        );
        // `time_zone: "host"` replaces the UTC time with the offset-bearing local time, and it
        // names exactly the same instant.
        assert_eq!(rows[1]["timestamp_local"], "2026-11-28T10:23:01.123+09:00");
        for (c, f) in rows.iter().zip(full["logs"].as_array().unwrap()) {
            assert_eq!(
                crate::mcp::project::time::parse_ms(c["timestamp_local"].as_str().unwrap()),
                crate::mcp::project::time::parse_ms(f["timestamp_utc"].as_str().unwrap())
            );
            for key in ["seq", "offset_ms", "frame_index", "level", "tag", "message"] {
                assert_eq!(c[key], f[key], "{key}");
            }
        }
        let full_host = project(page(2, &["boot-a"]), Fields::Full, Zone::fixed(9 * 3600));
        assert!(full_host["logs"][0].get("timestamp_utc").is_none());
        assert_eq!(
            full_host["logs"][0]["timestamp_local"],
            "2026-11-28T10:23:00.123+09:00"
        );
        // Mixed boots: nothing is lost, each row names its own boot.
        let mixed = project(page(4, &["boot-a", "boot-b"]), Fields::Compact, Zone::UTC);
        assert_eq!(mixed["boot_id"], Value::Null);
        assert_eq!(mixed["logs"][1]["boot_id"], "boot-b");
        let empty = project(page(0, &["boot-a"]), Fields::Compact, Zone::UTC);
        assert_eq!(empty["boot_id"], Value::Null);
    }

    #[test]
    fn compact_is_the_default_and_explicit_fields_win() {
        let none = serde_json::Map::new();
        assert_eq!(Fields::from_args(&none), Fields::Compact);
        for (arg, want) in [("full", Fields::Full), ("compact", Fields::Compact)] {
            let args = json!({"fields":arg});
            assert_eq!(Fields::from_args(args.as_object().unwrap()), want);
        }
    }

    /// Replays `bundle.query_logs` paging over `rows`: matches after the exclusive cursor, up to
    /// `limit`, with the daemon's `has_more` / `next_cursor`.
    fn daemon_page(rows: &[Value], after: Option<u64>, limit: usize) -> Value {
        let rest: Vec<_> = rows
            .iter()
            .filter(|r| after.is_none_or(|a| r["seq"].as_u64().unwrap() > a))
            .collect();
        let logs: Vec<_> = rest.iter().take(limit).map(|r| (*r).clone()).collect();
        let has_more = rest.len() > logs.len();
        let next = logs
            .last()
            .filter(|_| has_more)
            .map(|r| r["seq"].to_string());
        json!({"logs":logs,"total_matched":rows.len(),"returned_count":logs.len(),"has_more":has_more,"next_cursor":next,"unanchored_count":0})
    }

    /// Follows `next_cursor` to the end and returns every row seen, checking each page's shape.
    fn drain(rows: &[Value], limit: usize, budget: usize, fields: Fields) -> Vec<Value> {
        let mut seen = Vec::new();
        let mut after = None;
        for _ in 0..=rows.len() {
            let page = fit_byte_budget(
                shape(daemon_page(rows, after, limit), fields, Zone::UTC),
                budget,
            );
            let logs = page["logs"].as_array().unwrap();
            assert_eq!(page["returned_count"], logs.len());
            let truncated = page["truncated_by_byte_budget"] == true;
            assert!(
                logs.len() == 1 || page.to_string().len() <= budget,
                "only a single oversized row may exceed the budget"
            );
            if truncated {
                assert_eq!(page["has_more"], true);
                assert!(!logs.is_empty() && logs.len() < limit);
            }
            seen.extend(logs.iter().map(|r| r["seq"].clone()));
            if page["has_more"] == false {
                assert!(page["next_cursor"].is_null());
                return seen;
            }
            let cursor = page["next_cursor"].as_str().unwrap();
            assert_eq!(cursor, logs.last().unwrap()["seq"].to_string());
            after = Some(cursor.parse().unwrap());
        }
        panic!("paging did not finish within one page per row");
    }

    fn rows_with_messages(sizes: &[usize]) -> Vec<Value> {
        let base = page(sizes.len(), &["boot-a"]);
        base["logs"]
            .as_array()
            .unwrap()
            .iter()
            .zip(sizes)
            .enumerate()
            .map(|(i, (row, size))| {
                let mut row = row.clone();
                // Sparse sequence numbers, as after filtering.
                row["seq"] = json!(i * 3 + 7);
                row["message"] = json!("é".repeat(*size));
                row
            })
            .collect()
    }

    /// A page within budget is returned untouched apart from the flag.
    #[test]
    fn small_pages_are_unchanged() {
        let body = shape(page(3, &["boot-a"]), Fields::Compact, Zone::UTC);
        let fitted = fit_byte_budget(body.clone(), MAX_BODY_BYTES);
        assert_eq!(fitted["truncated_by_byte_budget"], false);
        assert_eq!(fitted["logs"], body["logs"]);
        assert_eq!(fitted["has_more"], false);
    }

    /// 500 long rows split at 48 KB; one 200 KB row still advances by itself.
    #[test]
    fn full_pages_split_at_the_byte_budget() {
        let rows = rows_with_messages(&[400; 500]);
        let first = project(daemon_page(&rows, None, 500), Fields::Full, Zone::UTC);
        assert_eq!(first["truncated_by_byte_budget"], true);
        assert!(first.to_string().len() <= MAX_BODY_BYTES);
        assert_eq!(first["total_matched"], 500);
        let all = drain(&rows, 500, MAX_BODY_BYTES, Fields::Full);
        let expected: Vec<_> = rows.iter().map(|r| r["seq"].clone()).collect();
        assert_eq!(all, expected);
        let giant = rows_with_messages(&[10, 100_000, 10]);
        let page = project(daemon_page(&giant, None, 100), Fields::Compact, Zone::UTC);
        assert_eq!(page["returned_count"], 1);
        assert_eq!(page["next_cursor"], "7");
        assert_eq!(drain(&giant, 100, MAX_BODY_BYTES, Fields::Compact).len(), 3);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        /// Byte-budget paging never drops, repeats or reorders a matching row.
        #[test]
        fn byte_budget_paging_returns_every_row_once(
            sizes in proptest::collection::vec(0usize..6_000, 0..60),
            limit in 1usize..40,
            budget in 1_000usize..20_000,
            full in any::<bool>(),
        ) {
            let rows = rows_with_messages(&sizes);
            let fields = if full { Fields::Full } else { Fields::Compact };
            let all = drain(&rows, limit, budget, fields);
            let expected: Vec<_> = rows.iter().map(|r| r["seq"].clone()).collect();
            prop_assert_eq!(all, expected);
        }
    }

    /// #571: a default 100-row page. The body (`structuredContent`) shrinks into the 20-35 KB
    /// range; the wire also repeats it as a JSON text block for older clients. Before the byte
    /// budget, full rows are at least twice as large; with it, the full page moves its tail on.
    #[test]
    fn hundred_row_page_is_measured_on_the_wire() {
        let rows = || page(100, &["3f2a9c1e-7d4b-4e8a-9b61-0c5d2e8f4a17"]);
        let measure = |fields| {
            let body = shape(rows(), fields, Zone::UTC);
            let size = body.to_string().len();
            let wire = ToolOutput::new(body)
                .result(ProtocolVersion::V2025_11_25)
                .to_string()
                .len();
            (size, wire)
        };
        let (full, full_wire) = measure(Fields::Full);
        let (compact, compact_wire) = measure(Fields::Compact);
        eprintln!(
            "100 rows: body full={full} B compact={compact} B; wire full={full_wire} B compact={compact_wire} B"
        );
        assert!((20_000..=35_000).contains(&compact), "{compact}");
        assert!(full >= compact * 2, "{full} vs {compact}");
        assert!(
            full_wire >= compact_wire * 2,
            "{full_wire} vs {compact_wire}"
        );
        let compact = project(rows(), Fields::Compact, Zone::UTC);
        assert_eq!(compact["truncated_by_byte_budget"], false);
        assert_eq!(compact["returned_count"], 100);
        let full = project(rows(), Fields::Full, Zone::UTC);
        assert_eq!(full["truncated_by_byte_budget"], true);
        assert!(full.to_string().len() <= MAX_BODY_BYTES);
        let kept = full["logs"].as_array().unwrap();
        assert_eq!(full["next_cursor"], kept.last().unwrap()["seq"].to_string());
    }
}
