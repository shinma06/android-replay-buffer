use super::super::{Failure, McpServer, ToolOutput};
use crate::mcp::{
    args::require_bundle_target,
    image_block,
    project::{self, logs::Fields, time::Zone},
    resources, take_png, typed_image_block,
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

    /// Metadata comes first and is never replaced; `include_images` only adds the rendered image:
    /// a JPEG tile by default (quality 80, longer edge at most 1024 px) or the lossless PNG with
    /// `image_format: "png"` (#586-G). The cached PNG and `png_path` are the same either way.
    pub(in crate::mcp) fn tool_inspect_frames(
        &mut self,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        let bundle_id = require_bundle_target(args)?;
        let mut params = args.clone();
        params.insert("bundle_id".to_owned(), json!(bundle_id));
        let inline = params.remove("include_images") == Some(Value::Bool(true));
        let format = match params.remove("image_format") {
            Some(Value::String(f)) if f == "png" => ImageFormat::Png,
            _ => ImageFormat::Jpeg,
        };
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
            if format == ImageFormat::Jpeg {
                params.insert("image_format".into(), json!("jpeg"));
            }
            let mut rendered = self.call_daemon("media.inspect_frames", &Value::Object(params))?;
            project::inspect::attach_render(&mut body, &rendered);
            Some(match (format, rendered["jpeg"].take()) {
                (ImageFormat::Jpeg, tile @ Value::Object(_)) if tile["base64"].is_string() => {
                    project::inspect::attach_image(&mut body, "jpeg", &tile);
                    typed_image_block(tile["base64"].as_str().unwrap_or_default(), "image/jpeg")
                }
                (requested, _) => {
                    let png = take_png(&mut rendered)?;
                    let size = json!({
                        "bytes_len": rendered["png_bytes_len"],
                        "width": rendered["width"],
                        "height": rendered["height"],
                    });
                    project::inspect::attach_image(&mut body, "png", &size);
                    if requested == ImageFormat::Jpeg {
                        project::inspect::warn_jpeg_unavailable(&mut body);
                    }
                    image_block(&png)
                }
            })
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

/// The inline image encoding `arb_inspect_frames` was asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageFormat {
    Jpeg,
    Png,
}
