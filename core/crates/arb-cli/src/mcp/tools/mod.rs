use super::{Failure, McpServer, ToolOutput};
use serde_json::{Map, Value, json};
pub(in crate::mcp) mod capture;
mod export;
mod inspect;
mod library;
pub(in crate::mcp) mod save;
use capture::summarize_audio_reason;
impl McpServer {
    pub(in crate::mcp) fn dispatch_tool(
        &mut self,
        name: &str,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        match name {
            "arb_status" => self.tool_status(),
            "arb_devices" => {
                let resp = self.call_daemon("device.list", &json!({}))?;
                Ok(ToolOutput::new(resp))
            }
            "arb_capture_start" => self.tool_capture_start(args),
            "arb_capture_stop" => {
                let mut resp = self.call_daemon("capture.stop", &json!({}))?;
                summarize_audio_reason(&mut resp);
                Ok(ToolOutput::new(resp))
            }
            "arb_config_get" => {
                let resp = self.call_daemon("config.get", &json!({}))?;
                Ok(ToolOutput::new(resp))
            }
            "arb_config_update" => self.tool_config_update(args),
            "arb_save_and_inspect" => self.save_and_inspect(args),
            "arb_save" => self.tool_save("save.trigger", args),
            "arb_screenshot" => self.tool_screenshot(args),
            "arb_save_logs" => self.tool_save("save.logs", args),
            "arb_list_bundles" => self.list_bundles(args),
            "arb_trash_bundle" => self.trash_bundle(args),
            "arb_restore_bundle" => self.restore_bundle(args),
            "arb_gc_bundles" => self.gc_bundles(args),
            "arb_forget_bundle" => self.forget_bundle(args),
            "arb_timeline_overview" => self.tool_timeline_overview(args),
            "arb_query_logs" => self.tool_query_logs(args),
            "arb_inspect_frames" => self.tool_inspect_frames(args),
            "arb_export_trim" => self.tool_export_trim(args),
            "arb_export_frame_png" => self.tool_export_frame_png(args),
            other => Err(Failure::invalid(format!("unknown tool `{other}`"))),
        }
    }
}
