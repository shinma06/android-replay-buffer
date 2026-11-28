use super::super::{Failure, McpServer, ToolOutput};
use crate::mcp::{args::require_str, fill_missing, project::time::Zone};
use serde_json::{Map, Value, json};
impl McpServer {
    pub(super) fn list_bundles(
        &mut self,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        let state = args.get("state").cloned().unwrap_or(Value::Null);
        let mut resp = self.call_daemon("bundle.list", &json!({ "state": state }))?;
        let zone = Zone::from_args(args);
        for row in resp["bundles"].as_array_mut().into_iter().flatten() {
            self.catalog_row(row);
            zone.localize(row, "created_at_utc", "created_local");
        }
        Ok(ToolOutput::new(resp))
    }
    pub(super) fn trash_bundle(
        &mut self,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        let bundle_id = require_str(args, "bundle_id")?;
        let mut resp = self.call_daemon("bundle.trash", &json!({ "bundle_id": bundle_id }))?;
        self.catalog_row(&mut resp);
        Ok(ToolOutput::new(resp))
    }
    pub(super) fn restore_bundle(
        &mut self,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        let bundle_id = require_str(args, "bundle_id")?;
        let mut resp = self.call_daemon("bundle.restore", &json!({ "bundle_id": bundle_id }))?;
        self.catalog_row(&mut resp);
        Ok(ToolOutput::new(resp))
    }
    pub(super) fn gc_bundles(&mut self, args: &Map<String, Value>) -> Result<ToolOutput, Failure> {
        let mut params = args.clone();
        let preview = *params.entry("dry_run").or_insert(Value::Bool(true)) == true;
        let mut resp = self.call_daemon("bundle.gc", &Value::Object(params))?;
        if preview {
            resp["hint"] = json!("候補と解放見込みを確認後、dry_run: false で実行してください。");
        }
        Ok(ToolOutput::new(resp))
    }
    pub(super) fn forget_bundle(
        &mut self,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        let resp = self.call_daemon("bundle.forget", &Value::Object(args.clone()))?;
        Ok(ToolOutput::new(resp))
    }
    fn catalog_row(&self, row: &mut Value) {
        fill_missing(
            row,
            &[
                "bundle_kind",
                "duration_ms",
                "frame_count",
                "has_video",
                "has_audio",
                "log_count",
                "crash_marker_count",
                "memo",
                "disk_state",
            ],
        );
        if self.version.legacy_result_fields() {
            fill_missing(row, &["created_utc"]);
        } else if let Some(obj) = row.as_object_mut() {
            obj.remove("created_utc");
        }
    }
}
