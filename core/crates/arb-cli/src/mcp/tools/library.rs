use super::super::{Failure, McpServer, ToolOutput};
use crate::bundle_page;
use crate::mcp::{args::require_str, fill_missing, project::time::Zone};
use serde_json::{Map, Value, json};
impl McpServer {
    /// One page of the catalog, newest first (#586-E). The daemon list stays complete.
    pub(super) fn list_bundles(
        &mut self,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        let state = args.get("state").cloned().unwrap_or(Value::Null);
        let mut resp = self.call_daemon("bundle.list", &json!({ "state": state }))?;
        let rows = match resp["bundles"].take() {
            Value::Array(rows) => rows,
            _ => Vec::new(),
        };
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(bundle_page::DEFAULT_MCP_LIMIT);
        let cursor = args.get("cursor").and_then(Value::as_str);
        let page =
            bundle_page::paginate(rows, limit, cursor).map_err(|_| invalid_cursor(cursor))?;
        let zone = Zone::from_args(args);
        let mut rows = page.rows;
        for row in &mut rows {
            Self::catalog_row(row);
            zone.localize(row, "created_at_utc", "created_local");
        }
        resp["bundles"] = Value::Array(rows);
        resp["total_count"] = json!(page.total_count);
        resp["has_more"] = json!(page.has_more);
        resp["next_cursor"] = json!(page.next_cursor);
        Ok(ToolOutput::new(resp))
    }
    pub(super) fn trash_bundle(
        &mut self,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        self.change_state("bundle.trash", args)
    }
    pub(super) fn restore_bundle(
        &mut self,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        self.change_state("bundle.restore", args)
    }
    /// Trash and restore keep the daemon RPC the IDE reads. MCP returns the four fields that
    /// changed or decide the next step unless `verbose: true` asks for the whole catalog row.
    fn change_state(
        &mut self,
        method: &str,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        let bundle_id = require_str(args, "bundle_id")?;
        let mut resp = self.call_daemon(method, &json!({ "bundle_id": bundle_id }))?;
        Self::catalog_row(&mut resp);
        if args.get("verbose") != Some(&Value::Bool(true)) {
            resp = json!({
                "bundle_id": resp["bundle_id"],
                "state": resp["state"],
                "trashed_at_utc": resp["trashed_at_utc"],
                "disk_state": resp["disk_state"],
            });
        }
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
    /// The published catalog row on every version: older daemons' gaps are null, the duplicate
    /// `created_utc` is dropped (`created_at_utc` is the one name) and `path` is its display form.
    fn catalog_row(row: &mut Value) {
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
        if let Some(obj) = row.as_object_mut() {
            obj.remove("created_utc");
        }
        bundle_page::display_row_path(row);
    }
}

fn invalid_cursor(cursor: Option<&str>) -> Failure {
    let mut failure = Failure::invalid(
        "cursor は前回の arb_list_bundles が返した next_cursor を指定してください",
    );
    failure.detail = Some(json!({"details": {"field": "cursor", "got": cursor}}));
    failure.hint = Some("cursor を省略すると最新の記録から一覧します。".to_owned());
    failure
}

#[cfg(test)]
mod tests {
    use super::McpServer;
    use serde_json::json;

    /// #586-E: the published row drops `created_utc` and shows a Windows extended-length path
    /// in its plain form; every other field is untouched.
    #[test]
    fn catalog_rows_drop_the_duplicate_time_and_show_plain_paths() {
        for (path, shown) in [
            (
                r"\\?\C:\Users\me\arb\bundles\b1",
                r"C:\Users\me\arb\bundles\b1",
            ),
            (r"\\?\UNC\nas\share\arb\b1", r"\\nas\share\arb\b1"),
            (r"\\?\C:\dir\CON", r"\\?\C:\dir\CON"),
            (
                "/home/me/.local/share/arb/bundles/b1",
                "/home/me/.local/share/arb/bundles/b1",
            ),
        ] {
            let mut row = json!({
                "bundle_id": "b1",
                "path": path,
                "created_at_utc": "2026-11-29T00:00:00.000Z",
                "created_utc": "2026-11-29T00:00:00.000Z",
                "state": "active",
            });
            McpServer::catalog_row(&mut row);
            assert_eq!(row["path"], shown, "{path}");
            assert!(row.get("created_utc").is_none());
            assert_eq!(row["created_at_utc"], "2026-11-29T00:00:00.000Z");
            assert_eq!(row["disk_state"], json!(null));
        }
    }
}
