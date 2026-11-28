//! PDD SSOT `docs/specs/ui-feedback-contract-table.json`: CLI exit codes, MCP `isError` and the
//! transport/business error split, checked row by row (the Kotlin IDE tests check the same rows).

use serde_json::Value;

use crate::rpc::RpcClientError;
use crate::{CliError, mcp, save_outcome_failed};

const TABLE: &str = include_str!("../../../../docs/specs/ui-feedback-contract-table.json");

fn rows(key: &str) -> Vec<Value> {
    let doc: Value = serde_json::from_str(TABLE).expect("contract table must be valid JSON");
    doc[key]
        .as_array()
        .unwrap_or_else(|| panic!("`{key}` must be an array"))
        .clone()
}

#[test]
fn save_rows_match_cli_exit_code_and_mcp_is_error() {
    let rows = rows("save_rows");
    assert!(rows.len() >= 5);
    for row in rows {
        let id = row["id"].as_str().unwrap();
        let response = &row["response"];
        let expected = &row["expected"];
        let exit_code = if save_outcome_failed(response) {
            CliError::save_failed_already_emitted().exit_code
        } else {
            0
        };
        assert_eq!(
            Some(u64::from(exit_code)),
            expected["cli_exit_code"].as_u64(),
            "{id}: CLI exit code"
        );
        let mcp = mcp::save_content(response.clone());
        assert_eq!(
            Some(mcp.is_err()),
            expected["mcp_is_error"].as_bool(),
            "{id}: MCP isError"
        );
        let echoed = match mcp {
            Ok(output) => output.body,
            Err(failure) => {
                failure.result(mcp::ProtocolVersion::default())["structuredContent"].clone()
            }
        };
        assert_eq!(
            echoed["error_code"], expected["mcp_error_code"],
            "{id}: structured error code"
        );
        assert_eq!(
            echoed["job"]["job_id"], response["job"]["job_id"],
            "{id}: MCP content preserves job_id"
        );
        assert_eq!(expected["mcp_preserves_job_id"], true, "{id}");
    }
}

#[test]
fn error_rows_match_cli_exit_code_and_transport_split() {
    let rows = rows("error_rows");
    assert!(rows.len() >= 8);
    for row in rows {
        let id = row["id"].as_str().unwrap();
        let failure = &row["failure"];
        let err = match failure["kind"].as_str().unwrap() {
            "rpc_error" => RpcClientError::Rpc {
                details: failure.get("details").cloned(),
                error_code: failure["error_code"].as_str().map(str::to_owned),
                code: i32::try_from(failure["code"].as_i64().unwrap()).unwrap(),
                message: "contract".to_owned(),
            },
            "eof" => RpcClientError::UnexpectedEof,
            "io" => RpcClientError::Io(std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
            "timeout" => RpcClientError::Timeout,
            other => panic!("{id}: unknown failure kind `{other}`"),
        };
        let expected = &row["expected"];
        assert_eq!(
            Some(err.is_transport()),
            expected["is_transport"].as_bool(),
            "{id}: transport vs business error"
        );
        let duplicate = match failure["kind"].as_str().unwrap() {
            "rpc_error" => RpcClientError::Rpc {
                details: failure.get("details").cloned(),
                error_code: failure["error_code"].as_str().map(str::to_owned),
                code: i32::try_from(failure["code"].as_i64().unwrap()).unwrap(),
                message: "contract".into(),
            },
            _ => RpcClientError::UnexpectedEof,
        };
        let body = mcp::Failure::from_rpc(duplicate).result(mcp::ProtocolVersion::default());
        assert_eq!(
            body["structuredContent"]["details"], failure["details"],
            "{id}"
        );
        if let Some(retryable) = expected.get("retryable") {
            assert_eq!(&body["structuredContent"]["retryable"], retryable, "{id}");
        }
        let cli = CliError::from_rpc(err);
        let cli_json: Value =
            serde_json::from_str(&crate::format_error(crate::OutputFormat::Json, &cli)).unwrap();
        assert_eq!(cli_json["error"]["details"], failure["details"], "{id}");
        if failure["kind"] == "rpc_error" {
            assert_eq!(
                cli_json["error"]["error_code"], expected["mcp_error_code"],
                "{id}"
            );
            assert_eq!(
                cli_json["error"]["hint"], body["structuredContent"]["hint"],
                "{id}"
            );
        }
        assert_eq!(
            body["structuredContent"]["error_code"], expected["mcp_error_code"],
            "{id}: MCP error"
        );
        assert_eq!(
            Some(u64::from(cli.exit_code)),
            expected["cli_exit_code"].as_u64(),
            "{id}: CLI exit code"
        );
    }
}
