//! Shared shell input contract and parser properties.
use arb_core::{
    bundle::{LogRecord, RecordBucket},
    injected_input::extract_injected_commands,
    time::{TimeAlignment, UncertaintyReason},
};
use proptest::prelude::*;
use serde_json::{Value, json};

fn record(tag: &str, uid: Option<u32>, message: String) -> LogRecord {
    LogRecord {
        seq: 7,
        boot_id: None,
        header_epoch_ns: None,
        device_read_boottime_ns: None,
        pc_ingest_cont_ns: 0,
        alignment: TimeAlignment::Unanchored {
            pc_ingest_cont_ns: 0,
            reason: UncertaintyReason::PcIngestOnly,
        },
        bucket: RecordBucket::Unanchored,
        level: None,
        tag: Some(tag.into()),
        pid: None,
        tid: None,
        uid,
        package_name: None,
        message,
    }
}

#[test]
fn pdd_contract_tables_injected_commands() {
    let rows: Vec<Value> = serde_json::from_str(include_str!(
        "../../../../docs/specs/injected-command-table.json"
    ))
    .unwrap();
    for row in rows {
        let log = record(
            row["tag"].as_str().unwrap(),
            serde_json::from_value(row["uid"].clone()).unwrap(),
            row["message"].as_str().unwrap().into(),
        );
        let result = extract_injected_commands(&[log], Some(37));
        let expected = row["expected"].as_array().unwrap();
        assert_eq!(result.inputs.len(), expected.len(), "{row}");
        assert_eq!(json!(result.unparsed_count), row["unparsed_count"], "{row}");
        for (input, want) in result.inputs.iter().zip(expected) {
            let actual = json!(input);
            for (k, v) in want.as_object().unwrap() {
                assert_eq!(&actual[k], v, "{row}");
            }
            assert_eq!(actual["source"], "adbd_command", "{row}");
            assert_eq!(actual["source_log_seq"], 7, "{row}");
            assert_eq!(actual["time_certainty"], "command_received", "{row}");
            assert_eq!(actual["latency_bound_ms"], 50, "{row}");
            assert!(input.boottime_ns.is_none(), "{row}");
        }
    }
}
proptest! {
    #[test]
    fn arbitrary_shell_data(text in any::<String>(),uid in 10000_u32..20000){
        let message=format!("adbd service requested 'shell:input {text}'");
        let _=extract_injected_commands(&[record("adbd",Some(0),message.clone())],None);
        let forged=extract_injected_commands(&[record("adbd",Some(uid),message)],None);
        prop_assert!(forged.inputs.is_empty());
        prop_assert_eq!(forged.unparsed_count,0);
        let quoted=text.replace('\'', "'\\''");
        let other=extract_injected_commands(&[record("adbd",Some(0),format!("adbd service requested 'shell:echo '{quoted}''"))],None);
        prop_assert!(other.inputs.is_empty());
        prop_assert_eq!(other.unparsed_count,0);
    }
}
