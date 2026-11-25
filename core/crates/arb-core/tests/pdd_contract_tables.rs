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
    for row in rows(&doc, "selection_rows", 25) {
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
