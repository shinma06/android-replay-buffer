//! `arb_query_logs` rows: `full` keeps every daemon field, `compact` keeps what an agent reads.
use super::time::Zone;
use crate::mcp::version::ProtocolVersion;
use serde_json::{Map, Value, json};

/// Row shape requested with `fields`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::mcp) enum Fields {
    Compact,
    Full,
}

impl Fields {
    pub(in crate::mcp) fn from_args(args: &Map<String, Value>, version: ProtocolVersion) -> Self {
        match args.get("fields").and_then(Value::as_str) {
            Some("full") => Self::Full,
            Some(_) => Self::Compact,
            None if version.default_compact_logs() => Self::Compact,
            None => Self::Full,
        }
    }
    const fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Full => "full",
        }
    }
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

/// Projects a `bundle.query_logs` page. Rows, their order and the paging fields are unchanged.
pub(in crate::mcp) fn project(mut page: Value, fields: Fields, zone: Zone) -> Value {
    let rows = page["logs"].as_array_mut().map(std::mem::take);
    let mut rows = rows.unwrap_or_default();
    for row in &mut rows {
        zone.localize(row, "timestamp_utc", "timestamp_local");
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
    for key in COMPACT_REQUIRED {
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
    use super::{Fields, project};
    use crate::mcp::{ToolOutput, project::time::Zone, version::ProtocolVersion};
    use arb_core::bundle::{LogRecord, RecordBucket};
    use arb_core::time::{TimeAlignment, TimestampSemantics};
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
                "timestamp_local",
                "timestamp_utc"
            ]
        );
        assert_eq!(rows[1]["timestamp_local"], "2026-11-28T10:23:01.123+09:00");
        for (c, f) in rows.iter().zip(full["logs"].as_array().unwrap()) {
            for key in [
                "seq",
                "timestamp_utc",
                "offset_ms",
                "frame_index",
                "level",
                "tag",
                "message",
            ] {
                assert_eq!(c[key], f[key], "{key}");
            }
        }
        // Mixed boots: nothing is lost, each row names its own boot.
        let mixed = project(page(4, &["boot-a", "boot-b"]), Fields::Compact, Zone::UTC);
        assert_eq!(mixed["boot_id"], Value::Null);
        assert_eq!(mixed["logs"][1]["boot_id"], "boot-b");
        let empty = project(page(0, &["boot-a"]), Fields::Compact, Zone::UTC);
        assert_eq!(empty["boot_id"], Value::Null);
    }

    #[test]
    fn version_selects_the_default_and_explicit_fields_win() {
        use ProtocolVersion::{V2024_11_05, V2025_03_26, V2025_06_18, V2025_11_25};
        let none = serde_json::Map::new();
        for (version, expected) in [
            (V2024_11_05, Fields::Full),
            (V2025_03_26, Fields::Full),
            (V2025_06_18, Fields::Compact),
            (V2025_11_25, Fields::Compact),
        ] {
            assert_eq!(Fields::from_args(&none, version), expected);
            for (arg, want) in [("full", Fields::Full), ("compact", Fields::Compact)] {
                let args = json!({"fields":arg});
                assert_eq!(Fields::from_args(args.as_object().unwrap(), version), want);
            }
        }
    }

    /// #571: a default 100-row page. The body (`structuredContent`) shrinks into the 20-35 KB
    /// range; the wire also repeats it as a JSON text block for older clients.
    #[test]
    fn hundred_row_page_is_measured_on_the_wire() {
        let measure = |fields| {
            let body = project(
                page(100, &["3f2a9c1e-7d4b-4e8a-9b61-0c5d2e8f4a17"]),
                fields,
                Zone::UTC,
            );
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
    }
}
