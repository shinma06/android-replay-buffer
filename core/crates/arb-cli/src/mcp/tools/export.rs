use super::super::{Failure, McpServer, ToolOutput};
use crate::mcp::args::{optional_abs_path, require_bundle_target};
use serde_json::{Map, Value, json};
impl McpServer {
    pub(in crate::mcp) fn tool_export_trim(
        &mut self,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        let bundle_id = require_bundle_target(args)?;
        let format = args
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("share_mp4");
        let trash_original = args
            .get("trash_original")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let burn_taps = args
            .get("burn_taps")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let include_share_mp4 = args
            .get("include_share_mp4")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        let (mut resp, path_field, mime) = match format {
            "share_mp4" => {
                let resp = self.call_daemon(
                    "export.share_mp4",
                    &json!({
                        "bundle_id": bundle_id,
                        "in_frame_index": args.get("in_frame_index"),
                        "out_frame_index": args.get("out_frame_index"),
                        "in_time_ns": args.get("in_time_ns"),
                        "out_time_ns": args.get("out_time_ns"),
                        "output_path": optional_abs_path(args, "output_path")?,
                        "dest_dir": optional_abs_path(args, "dest_dir")?,
                        "keep_bundle": args.get("keep_bundle").and_then(Value::as_bool).unwrap_or(false),
                        "burn_taps": burn_taps,
                        "trash_original": trash_original,
                        "holder_pid": std::process::id(),
                    }),
                )?;
                (resp, "share_mp4_path", "video/mp4")
            }
            "bundle" => {
                let resp = self.call_daemon(
                    "export.trim",
                    &json!({
                        "bundle_id_or_path": bundle_id,
                        "in_frame_index": args.get("in_frame_index"),
                        "out_frame_index": args.get("out_frame_index"),
                        "in_time_ns": args.get("in_time_ns"),
                        "out_time_ns": args.get("out_time_ns"),
                        "include_share_mp4": include_share_mp4,
                        "share_output_path": optional_abs_path(args, "output_path")?,
                        "burn_taps": burn_taps,
                        "dest_dir": optional_abs_path(args, "dest_dir")?,
                        "trash_original": trash_original,
                        "holder_pid": std::process::id(),
                    }),
                )?;
                (resp, "bundle_path", "inode/directory")
            }
            other => return Err(Failure::invalid(format!("未対応の出力形式: {other}"))),
        };
        let path = std::path::Path::new(
            resp[path_field]
                .as_str()
                .ok_or_else(|| Failure::unavailable("書き出し先がありません。"))?,
        );
        let uri = url::Url::from_file_path(path)
            .map_err(|()| Failure::unavailable("書き出し先のパスが不正です。"))?;
        let link = crate::mcp::resources::ResourceLink::file(path, uri, mime, "書き出し先");
        resp["output_path"] = resp[path_field].clone();
        resp["info_codes"] = if args.contains_key("output_path") || args.contains_key("dest_dir") {
            json!([])
        } else {
            json!(["default_export_dir"])
        };
        Ok(ToolOutput::new(resp).with_links(vec![link]))
    }

    pub(in crate::mcp) fn tool_export_frame_png(
        &mut self,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        let bundle_id = require_bundle_target(args)?;
        let frame_index = args
            .get("frame_index")
            .and_then(Value::as_u64)
            .ok_or_else(|| "arb_export_frame_png requires integer `frame_index`".to_owned())?;
        // Exactly one destination (schema `oneOf`). Neither form replaces an existing file.
        let resp = self.call_daemon(
            "export.png",
            &json!({
                "bundle_id_or_path": bundle_id,
                "frame_index": frame_index,
                "output_path": optional_abs_path(args, "output_path")?,
                "output_dir": optional_abs_path(args, "output_dir")?,
            }),
        )?;
        Ok(ToolOutput::new(resp))
    }
}
