use super::super::{Failure, McpServer, ToolOutput};
use crate::mcp::{args::require_bundle_target, image_block, resources, take_png};
use serde_json::{Map, Value, json};
impl McpServer {
    pub(in crate::mcp) fn tool_timeline_overview(
        &mut self,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        let bundle_id = require_bundle_target(args)?;
        let mut params = Map::new();
        params.insert("bundle_id".to_owned(), Value::String(bundle_id));
        for key in ["max_error_logs", "package", "tag", "exclude_tags"] {
            if let Some(value) = args.get(key) {
                params.insert(key.to_owned(), value.clone());
            }
        }
        let mut resp = self.call_daemon("timeline.overview", &Value::Object(params))?;
        if !self.version.legacy_result_fields()
            && let Some(obj) = resp.as_object_mut()
            && let Some(detail) = obj.remove("logcat_completeness_detail")
        {
            obj.insert("logcat_completeness".into(), detail);
        }
        Ok(ToolOutput::new(resp))
    }

    pub(in crate::mcp) fn tool_query_logs(
        &mut self,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        let bundle_id = require_bundle_target(args)?;
        let mut params = args.clone();
        params.insert("bundle_id_or_path".to_owned(), Value::String(bundle_id));
        params.remove("bundle_id");
        // The existing MCP schema advertises Android's legacy ASSERT abbreviation.
        if params
            .get("level")
            .and_then(Value::as_str)
            .is_some_and(|v| v.eq_ignore_ascii_case("A"))
        {
            params.insert("level".into(), Value::String("F".into()));
        }
        params.entry("limit").or_insert(json!(100));
        params.entry("bucket").or_insert(json!("main"));
        let resp = self.call_daemon("bundle.query_logs", &Value::Object(params))?;
        Ok(ToolOutput::new(resp))
    }

    pub(in crate::mcp) fn tool_inspect_frames(
        &mut self,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        let bundle_id = require_bundle_target(args)?;
        let mut params = args.clone();
        params.insert("bundle_id".to_owned(), json!(bundle_id));
        let inline = params.remove("include_images") == Some(Value::Bool(true));
        params.insert("metadata_only".into(), json!(!inline));
        let mut resp = self.call_daemon("media.inspect_frames", &Value::Object(params))?;

        if inline {
            let image = image_block(&take_png(&mut resp)?);
            return Ok(ToolOutput::new(resp)
                .with_image(image)
                .with_links(Vec::new()));
        }
        let mut links = Vec::new();

        if !resp["frames"].is_array() {
            return Err(Failure::new(
                -32601,
                "DAEMON_UNAVAILABLE",
                "フレームの遅延取得には arbd の更新が必要です",
            ));
        }
        let resource_id = self.frame_resource_id(&bundle_id, &resp["bundle_id"])?;
        for index in resp["frame_indices_rendered"]
            .as_array()
            .into_iter()
            .flatten()
        {
            links.push(resources::bundle_link(
                &resource_id,
                &format!("frame/{index}"),
            ));
        }
        Ok(ToolOutput::new(resp).with_links(links))
    }
}
