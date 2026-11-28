use super::super::{Failure, McpServer, ToolOutput};
use crate::mcp::{
    args::require_bundle_target,
    image_block,
    project::{self, logs::Fields, time::Zone},
    resources, take_png,
};
use serde_json::{Map, Value, json};
impl McpServer {
    pub(in crate::mcp) fn tool_timeline_overview(
        &mut self,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        let bundle_id = require_bundle_target(args)?;
        let params = project::overview::params(json!(bundle_id), args.get("max_error_logs"), args);
        let mut resp = self.call_daemon("timeline.overview", &params)?;
        project::overview::project(&mut resp, self.version, Zone::from_args(args));
        Ok(ToolOutput::new(resp))
    }

    pub(in crate::mcp) fn tool_query_logs(
        &mut self,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        let bundle_id = require_bundle_target(args)?;
        let mut params = args.clone();
        params.insert("bundle_id_or_path".to_owned(), Value::String(bundle_id));
        for local in ["bundle_id", "fields", "time_zone"] {
            params.remove(local);
        }
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
        let page = self.call_daemon("bundle.query_logs", &Value::Object(params))?;
        let fields = Fields::from_args(args);
        Ok(ToolOutput::new(project::logs::project(
            page,
            fields,
            Zone::from_args(args),
        )))
    }

    /// Metadata comes first and is never replaced; `include_images` only adds the rendered PNG.
    pub(in crate::mcp) fn tool_inspect_frames(
        &mut self,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        let bundle_id = require_bundle_target(args)?;
        let mut params = args.clone();
        params.insert("bundle_id".to_owned(), json!(bundle_id));
        let inline = params.remove("include_images") == Some(Value::Bool(true));
        params.insert("metadata_only".into(), json!(true));
        let meta = self.call_daemon("media.inspect_frames", &Value::Object(params.clone()))?;
        if !meta["frames"].is_array() {
            return Err(Failure::new(
                -32601,
                "DAEMON_UNAVAILABLE",
                "フレームの遅延取得には arbd の更新が必要です",
            ));
        }
        let resource_id = self.frame_resource_id(&bundle_id, &meta["bundle_id"])?;
        let links = meta["frame_indices_rendered"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|index| resources::bundle_link(&resource_id, &format!("frame/{index}")))
            .collect();
        let mut body = project::inspect::frames(meta);
        let image = if inline {
            params.insert("metadata_only".into(), json!(false));
            let mut rendered = self.call_daemon("media.inspect_frames", &Value::Object(params))?;
            let png = take_png(&mut rendered)?;
            project::inspect::attach_render(&mut body, &rendered);
            Some(image_block(&png))
        } else {
            None
        };
        let output = ToolOutput::new(body).with_links(links);
        Ok(match image {
            Some(image) => output.with_image(image),
            None => output,
        })
    }
}
