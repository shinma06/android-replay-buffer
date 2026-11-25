//! PDD SSOT `docs/specs/ui-feedback-contract-table.json` against the `arbd` wire types and the
//! `arb-storage` disk-state / forget gates. The Kotlin client checks the same rows.

use arb_storage::catalog::{
    BundleDiskState, Catalog, CatalogFormatVersion, NewBundleEntry, probe_bundle_disk_state,
};
use arbd::protocol::{
    BundleEntryView, BundleForgetResponse, BundleGcResponse, SaveOutcomeResponse,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

const TABLE: &str = include_str!("../../../../docs/specs/ui-feedback-contract-table.json");

fn table() -> Value {
    serde_json::from_str(TABLE).expect("ui-feedback-contract-table.json must be valid JSON")
}

fn rows<'a>(doc: &'a Value, key: &str) -> &'a [Value] {
    doc[key]
        .as_array()
        .unwrap_or_else(|| panic!("`{key}` must be an array"))
}

/// The row's wire object must survive a Rust round trip unchanged: every field is a real
/// `arbd` field and none is dropped or renamed.
fn round_trip<T: DeserializeOwned + Serialize>(id: &str, wire: &Value) -> T {
    let parsed: T = serde_json::from_value(wire.clone())
        .unwrap_or_else(|e| panic!("{id}: not an arbd wire object: {e}"));
    assert_eq!(
        &serde_json::to_value(&parsed).expect("contract table"),
        wire,
        "{id}: wire round trip"
    );
    parsed
}

fn label_count(value: &Value) -> usize {
    value.as_array().map_or(0, Vec::len)
}

fn assert_buckets_surfaced(id: &str, labels: &Value, response: &Value, expected: &Value) {
    let tokens: Vec<&str> = expected["summary_tokens"]
        .as_array()
        .expect("contract table")
        .iter()
        .map(|t| t.as_str().expect("contract table"))
        .collect();
    for (field, label) in labels.as_object().expect("contract table") {
        let n = label_count(&response[field]);
        let token = format!("{}{n}", label.as_str().expect("contract table"));
        assert_eq!(
            tokens.contains(&token.as_str()),
            n > 0,
            "{id}: bucket `{field}` (count {n}) must be surfaced iff non-empty"
        );
    }
}

#[test]
fn gc_rows_are_arbd_responses_and_every_skip_or_failure_warns() {
    let doc = table();
    let labels = &doc["gc_bucket_labels"];
    let mut fields: Vec<&str> = labels
        .as_object()
        .expect("contract table")
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    let gc_rows = rows(&doc, "gc_rows");
    assert!(gc_rows.len() >= 10);
    for row in gc_rows {
        let id = row["id"].as_str().expect("contract table");
        let gc: BundleGcResponse = round_trip(id, &row["response"]);
        let mut wire_fields: Vec<String> = serde_json::to_value(&gc)
            .expect("contract table")
            .as_object()
            .expect("contract table")
            .keys()
            .cloned()
            .collect();
        wire_fields.sort_unstable();
        assert_eq!(wire_fields, fields, "{id}: labels must cover every bucket");
        let warns = !gc.skipped_leased_bundle_ids.is_empty()
            || !gc.skipped_active_job_bundle_ids.is_empty()
            || !gc.skipped_unmanaged_or_legacy_bundle_ids.is_empty()
            || !gc.skipped_in_grace_bundle_ids.is_empty()
            || !gc.failed.is_empty();
        let expected = &row["expected"];
        assert_eq!(
            expected["severity"],
            json!(if warns { "warning" } else { "info" }),
            "{id}: severity"
        );
        assert_buckets_surfaced(id, labels, &row["response"], expected);
    }
}

#[test]
fn forget_rows_are_arbd_responses_and_every_protection_warns() {
    let doc = table();
    let labels = &doc["forget_bucket_labels"];
    let forget_rows = rows(&doc, "forget_rows");
    assert!(forget_rows.len() >= 6);
    for row in forget_rows {
        let id = row["id"].as_str().expect("contract table");
        let res: BundleForgetResponse = round_trip(id, &row["response"]);
        let warns = !res.skipped_present_bundle_ids.is_empty()
            || !res.skipped_storage_unavailable_bundle_ids.is_empty()
            || !res.skipped_leased_bundle_ids.is_empty()
            || !res.skipped_active_job_bundle_ids.is_empty();
        let expected = &row["expected"];
        assert_eq!(
            expected["severity"],
            json!(if warns { "warning" } else { "info" }),
            "{id}: severity"
        );
        assert_buckets_surfaced(id, labels, &row["response"], expected);
    }
}

#[test]
fn save_rows_are_arbd_save_outcomes() {
    let doc = table();
    for row in rows(&doc, "save_rows") {
        let id = row["id"].as_str().expect("contract table");
        let outcome: SaveOutcomeResponse = round_trip(id, &row["response"]);
        assert_eq!(outcome.job.job_id, "job-42", "{id}");
    }
}

#[test]
fn disk_state_rows_match_probe_and_non_destructive_forget() {
    let doc = table();
    for row in rows(&doc, "disk_state_rows") {
        let id = row["id"].as_str().expect("contract table");
        let expected = &row["expected"];
        let view: BundleEntryView = serde_json::from_value(row["entry"].clone())
            .unwrap_or_else(|e| panic!("{id}: not a BundleEntryView: {e}"));
        let effective: BundleDiskState =
            serde_json::from_value(expected["effective_disk_state"].clone())
                .expect("contract table");
        assert_eq!(view.disk_state, effective, "{id}: wire disk_state");

        let tmp = tempfile::tempdir().expect("contract table");
        let managed = tmp.path().join("managed");
        let catalog =
            Catalog::open(tmp.path().join("catalog.sqlite"), &managed).expect("contract table");
        let bundle_path = match effective {
            BundleDiskState::Present => {
                let p = managed.join(&view.bundle_id);
                std::fs::create_dir_all(&p).expect("contract table");
                p
            }
            BundleDiskState::Missing => managed.join(&view.bundle_id),
            _ => tmp.path().join("unmounted").join(&view.bundle_id),
        };
        let entry = NewBundleEntry {
            bundle_id: view.bundle_id.clone(),
            path: bundle_path.clone(),
            format_version: CatalogFormatVersion::V2,
            parent_bundle_id: None,
            created_at_utc: view.created_at_utc.clone(),
        };
        catalog.register_bundle(&entry).expect("contract table");
        assert_eq!(
            probe_bundle_disk_state(&bundle_path),
            effective,
            "{id}: probe"
        );

        let now = "2026-11-25T01:00:00Z";
        let single = catalog
            .forget_missing_bundle(&view.bundle_id, now)
            .expect("contract table");
        let single = serde_json::to_value(BundleForgetResponse {
            forgotten_bundle_ids: single.forgotten_bundle_ids,
            skipped_present_bundle_ids: single.skipped_present_bundle_ids,
            skipped_storage_unavailable_bundle_ids: single.skipped_storage_unavailable_bundle_ids,
            skipped_leased_bundle_ids: single.skipped_leased_bundle_ids,
            skipped_active_job_bundle_ids: single.skipped_active_job_bundle_ids,
            reaped_lease_ids: single.reaped_lease_ids,
        })
        .expect("contract table");
        let bucket = expected["forget_bucket"].as_str().expect("contract table");
        assert_eq!(
            single[bucket],
            json!([view.bundle_id]),
            "{id}: bundle.forget bucket ({single})"
        );
        assert_eq!(
            expected["can_forget"],
            json!(bucket == "forgotten_bundle_ids"),
            "{id}: only a forgettable entry may be forgotten"
        );
        let bulk_catalog =
            Catalog::open(tmp.path().join("bulk.sqlite"), &managed).expect("catalog");
        bulk_catalog.register_bundle(&entry).expect("register");
        let bulk = bulk_catalog
            .prune_missing_bundles(now)
            .expect("bulk forget");
        let expected_ids = |field| {
            if bucket == field {
                vec![view.bundle_id.clone()]
            } else {
                vec![]
            }
        };
        assert_eq!(
            bulk.forgotten_bundle_ids,
            expected_ids("forgotten_bundle_ids"),
            "{id}: bulk forgotten"
        );
        assert_eq!(
            bulk.skipped_storage_unavailable_bundle_ids,
            expected_ids("skipped_storage_unavailable_bundle_ids"),
            "{id}: bulk unmounted"
        );
        assert!(
            bulk.skipped_present_bundle_ids.is_empty(),
            "{id}: all_missing excludes present entries from the report"
        );
        assert_eq!(expected["disk_untouched"], true);
        assert_eq!(
            bundle_path.is_dir(),
            effective == BundleDiskState::Present,
            "{id}: forget never creates or deletes bundle directories"
        );
    }
}
