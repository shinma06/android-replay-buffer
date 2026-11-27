//! `timeline.overview` as published by `arb_timeline_overview` and `arb_save_and_inspect`.
//!
//! Both tools read one daemon overview with the same filters, so their error groups and noise
//! candidates are identical for the same bundle and arguments.
use super::{screen_offsets, time::Zone};
use crate::mcp::version::ProtocolVersion;
use serde_json::{Map, Value, json};

/// Filters shared by the overview, the save-and-inspect call and the follow-up queries.
pub(in crate::mcp) const FILTERS: [&str; 3] = ["package", "tag", "exclude_tags"];

/// Upper bound of `max_error_logs` accepted by the daemon.
const MAX_GROUPS: u64 = 200;

/// Daemon parameters for `timeline.overview` with the caller's filters.
pub(in crate::mcp) fn params(
    bundle_id: Value,
    limit: Option<&Value>,
    args: &Map<String, Value>,
) -> Value {
    let mut params = Map::new();
    params.insert("bundle_id".into(), bundle_id);
    if let Some(limit) = limit {
        params.insert("max_error_logs".into(), limit.clone());
    }
    for key in FILTERS {
        if let Some(value) = args.get(key) {
            params.insert(key.into(), value.clone());
        }
    }
    Value::Object(params)
}

/// Fills fields an older daemon omits and adds opt-in local times. Counts and rows are untouched.
pub(in crate::mcp) fn project(overview: &mut Value, version: ProtocolVersion, zone: Zone) {
    if let Some(obj) = overview.as_object_mut() {
        if !version.legacy_result_fields()
            && let Some(detail) = obj.remove("logcat_completeness_detail")
        {
            obj.insert("logcat_completeness".into(), detail);
        }
        obj.entry("carried_in").or_insert(Value::Null);
        obj.entry("noise_candidates").or_insert_with(|| json!([]));
    }
    screen_offsets(&mut overview["screens"]);
    for row in overview["error_log_highlights"]
        .as_array_mut()
        .into_iter()
        .flatten()
    {
        zone.localize(row, "timestamp_utc", "timestamp_local");
    }
}

/// The `ErrorGroups` view of a projected overview, with calls that read what it leaves out.
pub(in crate::mcp) fn error_groups(
    overview: &mut Value,
    bundle_id: &Value,
    args: &Map<String, Value>,
) -> Value {
    let logs = overview["error_log_highlights"].take();
    let returned = logs.as_array().map_or(0, Vec::len);
    let unique = overview["unique_error_logs"].as_u64().unwrap_or(0);
    let has_more = overview["error_logs_truncated"].as_bool().unwrap_or(false);
    let mut next = Map::new();
    next.insert("bundle_id".into(), bundle_id.clone());
    let next_limit = unique.min(MAX_GROUPS);
    next.insert("max_error_logs".into(), json!(next_limit));
    let mut raw = Map::new();
    raw.insert("bundle_id".into(), bundle_id.clone());
    raw.insert("min_level".into(), json!("E"));
    raw.insert("bucket".into(), json!("main"));
    for key in FILTERS.into_iter().chain(["time_zone"]) {
        if let Some(value) = args.get(key) {
            next.insert(key.into(), value.clone());
            // `arb_query_logs` applies `exclude_tags` with the overview's rule, so the raw rows
            // are the ungrouped rows behind the same groups.
            raw.insert(key.into(), value.clone());
        }
    }
    json!({
        "logs": logs,
        "total_matched": overview["total_error_logs"],
        "unique_count": unique,
        "returned_count": returned,
        "has_more": has_more,
        // Only a call that returns more groups; past the cap, `raw_query` pages the raw rows.
        "continuation": if has_more && next_limit > u64::try_from(returned).unwrap_or(u64::MAX) {
            json!({"tool":"arb_timeline_overview","arguments":next})
        } else {
            Value::Null
        },
        "raw_query": {"tool":"arb_query_logs","arguments":raw},
    })
}

#[cfg(test)]
mod tests {
    use super::{error_groups, params, project};
    use crate::mcp::{project::time::Zone, version::ProtocolVersion};
    use serde_json::{Value, json};

    #[test]
    fn continuation_is_null_when_the_cap_is_already_returned() {
        let rows: Vec<_> = (0..200).map(|i| json!({"seq":i})).collect();
        let mut capped = json!({"error_log_highlights":rows,"total_error_logs":400,"unique_error_logs":202,"error_logs_truncated":true});
        let groups = error_groups(&mut capped, &json!("b1"), &serde_json::Map::new());
        assert_eq!(groups["has_more"], true);
        assert_eq!(groups["continuation"], Value::Null);
        assert_eq!(groups["raw_query"]["tool"], "arb_query_logs");
        let rows: Vec<_> = (0..20).map(|i| json!({"seq":i})).collect();
        let mut short = json!({"error_log_highlights":rows,"total_error_logs":400,"unique_error_logs":202,"error_logs_truncated":true});
        let groups = error_groups(&mut short, &json!("b1"), &serde_json::Map::new());
        assert_eq!(groups["continuation"]["arguments"]["max_error_logs"], 200);
    }

    /// An overview whose `unique` groups exceed the single returned one when `unique > 1`.
    fn overview(unique: u64) -> Value {
        let truncated = unique > 1;
        json!({
            "error_log_highlights":[{"seq":7,"level":"E","tag":"A","message":"x","timestamp_utc":"2026-11-28T01:23:45.123Z","frame_index_hint":null,"t_ms":null}],
            "total_error_logs":40,"unique_error_logs":unique,"error_logs_truncated":truncated,
            "screens":[{"start_ns":1}],"logcat_completeness_detail":{"status":"complete"}
        })
    }

    #[test]
    fn continuation_appears_only_while_groups_remain() {
        let args =
            json!({"package":"com.example","tag":"Auth","exclude_tags":["Vendor"],"max_errors":1});
        let args = args.as_object().unwrap();
        let mut more = overview(3);
        project(&mut more, ProtocolVersion::V2025_11_25, Zone::UTC);
        assert_eq!(more["noise_candidates"], json!([]));
        assert_eq!(more["logcat_completeness"]["status"], "complete");
        assert_eq!(more["screens"][0]["start_offset_ms"], Value::Null);
        let groups = error_groups(&mut more, &json!("b1"), args);
        assert_eq!(groups["logs"][0]["level"], "E");
        assert_eq!(
            groups["continuation"],
            json!({"tool":"arb_timeline_overview","arguments":{"bundle_id":"b1","max_error_logs":3,"package":"com.example","tag":"Auth","exclude_tags":["Vendor"]}})
        );
        assert_eq!(
            groups["raw_query"],
            json!({"tool":"arb_query_logs","arguments":{"bundle_id":"b1","min_level":"E","bucket":"main","package":"com.example","tag":"Auth","exclude_tags":["Vendor"]}})
        );
        assert_eq!(
            (
                &groups["total_matched"],
                &groups["unique_count"],
                &groups["returned_count"],
                &groups["has_more"]
            ),
            (&json!(40), &json!(3), &json!(1), &json!(true))
        );
        let mut done = overview(1);
        let groups = error_groups(&mut done, &json!("b1"), &serde_json::Map::new());
        assert_eq!(groups["continuation"], Value::Null);
        assert_eq!(
            groups["raw_query"]["arguments"],
            json!({"bundle_id":"b1","min_level":"E","bucket":"main"})
        );
        // The daemon request carries the same filters the follow-up calls repeat.
        assert_eq!(
            params(json!("b1"), Some(&json!(1)), args),
            json!({"bundle_id":"b1","max_error_logs":1,"package":"com.example","tag":"Auth","exclude_tags":["Vendor"]})
        );
    }

    #[test]
    fn legacy_versions_keep_the_detail_field_and_host_time_is_opt_in() {
        let mut legacy = overview(1);
        project(
            &mut legacy,
            ProtocolVersion::V2025_03_26,
            Zone::fixed(9 * 3600),
        );
        assert_eq!(legacy["logcat_completeness_detail"]["status"], "complete");
        assert_eq!(
            legacy["error_log_highlights"][0]["timestamp_local"],
            "2026-11-28T10:23:45.123+09:00"
        );
    }
}
