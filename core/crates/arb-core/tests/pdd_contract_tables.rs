//! PDD SSOT contract tables shared with the Kotlin client (`docs/specs/*.json`).
//!
//! Every row of `device-selection-policy-table.json` is evaluated against the production
//! `arb_core::device` policy; the Kotlin `PddContractTablesTest` checks the same rows.

use std::collections::HashSet;

use arb_core::device::{DeviceSelectionPolicy, is_valid_adb_serial, selectable_online_serials};
use arb_core::state::SaveJobState;
use serde_json::{Value, json};

const DEVICE_TABLE: &str =
    include_str!("../../../../docs/specs/device-selection-policy-table.json");
const UI_FEEDBACK_TABLE: &str =
    include_str!("../../../../docs/specs/ui-feedback-contract-table.json");

fn table(raw: &str) -> Value {
    let doc: Value = serde_json::from_str(raw).expect("contract table must be valid JSON");
    assert_eq!(
        doc["schema_version"], 1,
        "unsupported contract table schema"
    );
    doc
}

fn rows<'a>(doc: &'a Value, key: &str, min: usize) -> &'a [Value] {
    let rows = doc[key]
        .as_array()
        .unwrap_or_else(|| panic!("`{key}` must be an array"));
    assert!(
        rows.len() >= min,
        "`{key}` needs at least {min} rows, has {}",
        rows.len()
    );
    let ids: Vec<&str> = rows.iter().filter_map(|r| r["id"].as_str()).collect();
    if !ids.is_empty() {
        assert_eq!(
            ids.len(),
            ids.iter().collect::<HashSet<_>>().len(),
            "duplicate ids in `{key}`"
        );
    }
    rows
}

fn serial_of(row: &Value) -> String {
    if let Some(serial) = row["serial"].as_str() {
        return serial.to_owned();
    }
    let rep = &row["serial_repeat"];
    let unit = rep["unit"].as_str().expect("serial_repeat.unit");
    let count = usize::try_from(rep["count"].as_u64().expect("serial_repeat.count"))
        .expect("serial_repeat.count fits usize");
    unit.repeat(count)
}

#[test]
fn device_selection_rows_match_rust_policy() {
    let doc = table(DEVICE_TABLE);
    for row in rows(&doc, "selection_rows", 27) {
        let id = row["id"].as_str().unwrap();
        let preferred = row["preferred_serial"].as_str();
        let devices: Vec<(&str, bool)> = row["devices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| (d["serial"].as_str().unwrap(), d["state"] == "device"))
            .collect();
        let online = selectable_online_serials(devices);
        let outcome = DeviceSelectionPolicy::evaluate(preferred, &online);

        let expected = &row["expected"];
        let mut expected_selection = expected["selection"].clone();
        let auto_selected = expected_selection
            .as_object_mut()
            .unwrap()
            .remove("auto_selected");
        let actual = serde_json::to_value(&outcome).unwrap();
        assert_eq!(actual, expected_selection, "{id}: selection");

        let is_selected = actual["status"] == "selected";
        if is_selected {
            let explicit = preferred.is_some_and(|p| !p.trim().is_empty());
            assert_eq!(
                auto_selected,
                Some(json!(!explicit)),
                "{id}: auto_selected iff no explicit serial"
            );
        } else {
            assert_eq!(
                auto_selected, None,
                "{id}: auto_selected only on `selected`"
            );
        }
        assert_eq!(
            expected["ide"]["has_online_device"],
            json!(!online.is_empty()),
            "{id}: valid online device"
        );
    }
}

/// #583 capture-intent rows: the two-phase explicit start, judged by the production policy.
/// A failed start keeps `(enabled, serial)`; a persisted argument serial is one the policy
/// selects from `online`; while recording, only the active serial (or none) succeeds.
#[test]
fn capture_intent_rows_follow_two_phase_explicit_start() {
    let doc = table(DEVICE_TABLE);
    let ids: Vec<&str> = rows(&doc, "capture_intent_rows", 9)
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    for id in ["CI-06", "CI-07", "CI-08", "CI-09"] {
        assert!(ids.contains(&id), "missing {id}");
    }
    for row in rows(&doc, "capture_intent_rows", 9) {
        let id = &row["id"];
        let online: Vec<String> = selectable_online_serials(
            row["online"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| (s.as_str().unwrap(), true)),
        );
        let argument = row["argument"].as_str();
        let outcome = row["outcome"].as_str().unwrap();
        let changed = row["config_changed"].as_object().unwrap();
        let expected = match (row["active"].as_str(), argument) {
            (Some(active), Some(arg)) if arg != active => "conflict",
            (Some(_), _) => "already_active",
            (None, Some(arg)) => match DeviceSelectionPolicy::evaluate(Some(arg), &online) {
                arb_core::device::DeviceSelectionOutcome::Selected { .. } => "started",
                _ => "device_not_found",
            },
            (None, None) => {
                let preferred = row["configured"].as_str();
                match DeviceSelectionPolicy::evaluate(preferred, &online) {
                    arb_core::device::DeviceSelectionOutcome::Selected { .. } => "started",
                    _ => "waiting",
                }
            }
        };
        assert_eq!(outcome, expected, "{id}: outcome");
        if matches!(outcome, "device_not_found" | "conflict") {
            assert!(changed.is_empty(), "{id}: failure changes nothing");
            assert_eq!(row["persisted"], row["configured"], "{id}");
            assert_eq!(row["persisted_enabled"], row["enabled"], "{id}");
            assert_eq!(row["error_details"]["config_changed"], json!({}), "{id}");
            continue;
        }
        assert_eq!(row["persisted_enabled"], true, "{id}: a start arms capture");
        if let Some(arg) = argument {
            assert_eq!(row["persisted"], arg, "{id}: explicit serial is pinned");
            assert!(
                online.iter().any(|s| s == arg),
                "{id}: pinned only if Selected"
            );
        } else {
            assert_eq!(
                row["persisted"], row["configured"],
                "{id}: no-arg keeps the pin"
            );
        }
        let waiting = outcome == "waiting";
        assert_eq!(
            row["warning_codes"],
            if waiting {
                json!(["waiting_for_device"])
            } else {
                json!([])
            },
            "{id}"
        );
        for (key, before, after) in [
            (
                "capture_enabled",
                &row["enabled"],
                &row["persisted_enabled"],
            ),
            ("device_serial", &row["configured"], &row["persisted"]),
        ] {
            let diff = (before != after).then(|| json!([before, after]));
            assert_eq!(
                changed.get(key),
                diff.as_ref(),
                "{id}: config_changed.{key}"
            );
        }
    }
}

#[test]
fn serial_validation_rows_match_rust_validator() {
    let doc = table(DEVICE_TABLE);
    for row in rows(&doc, "serial_validation_rows", 46) {
        let serial = serial_of(row);
        assert_eq!(
            is_valid_adb_serial(&serial),
            row["valid"].as_bool().unwrap(),
            "serial {serial:?} ({})",
            row["why"]
        );
    }
}

#[test]
fn ui_feedback_save_rows_use_rust_save_job_states() {
    let doc = table(UI_FEEDBACK_TABLE);
    for row in rows(&doc, "save_rows", 7) {
        let id = row["id"].as_str().unwrap();
        let state: SaveJobState = serde_json::from_value(row["response"]["job"]["state"].clone())
            .unwrap_or_else(|e| panic!("{id}: unknown SaveJobState: {e}"));
        assert_eq!(
            row["expected"]["ide_recovery_lists_job"],
            json!(state == SaveJobState::FailedRetryable),
            "{id}: only failed_retryable jobs stay in the recovery panel"
        );
    }
    for key in ["gc_rows", "forget_rows", "disk_state_rows", "error_rows"] {
        rows(&doc, key, 4);
    }
}
