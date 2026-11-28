use super::super::{Failure, McpServer, ToolOutput};
use crate::mcp::{
    args::save_params,
    fill_missing,
    project::{self, time::Zone},
    resources,
};
use serde_json::{Map, Value, json};
impl McpServer {
    pub(super) fn tool_jobs(&mut self, args: &Map<String, Value>) -> Result<ToolOutput, Failure> {
        let action = args["action"]
            .as_str()
            .ok_or_else(|| Failure::invalid("action を指定してください。"))?;
        if action == "list" {
            let mut result = self.call_daemon("save.list_jobs", &json!({}))?;
            for job in result["jobs"]
                .as_array_mut()
                .ok_or_else(|| Failure::unavailable("ジョブ一覧を取得できません。"))?
            {
                *job = project_job(job);
            }
            return Ok(ToolOutput::new(result));
        }
        let id = args
            .get("job_id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| Failure::invalid("job_id を指定してください。"))?;
        let mut params = json!({"job_id":id});
        let method = match action {
            "retarget" => {
                if let Some(dest) = crate::mcp::args::optional_abs_path(args, "dest_dir")? {
                    params["dest_dir"] = json!(dest);
                }
                "save.retarget"
            }
            "retry" => "save.retry",
            "discard" => "save.discard",
            _ => return Err(Failure::invalid("未知のジョブ操作です。")),
        };
        let result = self.call_daemon(method, &params)?;
        if action == "discard" {
            Ok(ToolOutput::new(json!({"job":project_job(&result)})))
        } else {
            save_content(result)
        }
    }
    pub(super) fn tool_save(
        &mut self,
        method: &str,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        let response = self.call_daemon(method, &save_params(args)?)?;
        save_content(response)
    }
    pub(in crate::mcp) fn save_and_inspect(
        &mut self,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        let method = if args.get("logs_only") == Some(&Value::Bool(true)) {
            "save.logs"
        } else {
            "save.trigger"
        };
        let mut result = self.tool_save(method, args)?;
        self.activity.notify(&self.session, "保存完了・証跡を確認");
        if let Err(mut error) = self.inspect_saved(args, &mut result) {
            error.add_save(result.body);
            return Err(error);
        }
        Ok(result)
    }

    /// Reads the saved bundle through the same overview as `arb_timeline_overview`, so error
    /// groups and noise candidates match it exactly for the same filters on every version.
    fn inspect_saved(
        &mut self,
        args: &Map<String, Value>,
        result: &mut ToolOutput,
    ) -> Result<(), Failure> {
        let id = result.body["bundle_id"].clone();
        let summary = self.call_daemon("timeline.summary", &json!({"bundle_id_or_path":id}))?;
        let limit = args.get("max_errors").cloned().unwrap_or(json!(20));
        let params = project::overview::params(id.clone(), Some(&limit), args);
        let mut overview = self.call_daemon("timeline.overview", &params)?;
        if !overview["unique_error_logs"].is_u64() {
            return Err(Failure::unavailable(
                "エラーを集約するには arbd を更新してください",
            ));
        }
        project::overview::project(&mut overview, self.version, Zone::from_args(args));
        let error_logs = project::overview::error_groups(&mut overview, &id, args);
        let screens = overview["screens"].take();
        result.add_link(resources::bundle_link(&id, "crashes"));
        if args.get("include_key_frame_links") != Some(&Value::Bool(false)) {
            for frame in screens
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s["start_frame_index"].as_u64())
            {
                result.add_link(resources::bundle_link(&id, &format!("frame/{frame}")));
            }
        }
        result.body = json!({"save":result.body,"summary":summary,"screens":screens,
            "carried_in":overview["carried_in"].take(),"crashes":overview["crashes"].take(),"error_logs":error_logs,
            "noise_candidates":overview["noise_candidates"].take(),"memo":args.get("note")});
        Ok(())
    }
}
/// `warning_details` for a daemon that predates it: each code paired with the display text at
/// the same position (older daemons omitted the text of a trailing `window_shortfall`).
fn legacy_warning_details(obj: &serde_json::Map<String, Value>) -> Value {
    let codes = obj.get("warning_codes").and_then(Value::as_array);
    let messages = obj.get("warnings").and_then(Value::as_array);
    Value::Array(
        codes
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(i, code)| {
                let message = messages
                    .and_then(|m| m.get(i))
                    .filter(|m| m.is_string())
                    .unwrap_or(code);
                json!({"code": code, "message": message})
            })
            .collect(),
    )
}

fn project_job(job: &Value) -> Value {
    let count = job["pinned_chunk_count"].as_u64().unwrap_or_else(|| {
        job["pinned_chunk_ids"]
            .as_array()
            .and_then(|ids| u64::try_from(ids.len()).ok())
            .unwrap_or(0)
    });
    json!({"job_id":job["job_id"],"state":job["state"],"failure_class":job["failure_class"],
        "recommended_action":job["recommended_action"],"last_error":job["last_error"],
        "pinned_chunk_count":count,"destination_path":job["destination_path"]})
}

pub(crate) fn save_content(mut response: Value) -> Result<ToolOutput, Failure> {
    if let Some(obj) = response.as_object_mut() {
        if let Some(job) = obj.get_mut("job") {
            *job = project_job(job);
        }
        obj.entry("warning_codes").or_insert_with(|| json!([]));
        if !obj.contains_key("warning_details") {
            let details = legacy_warning_details(obj);
            obj.insert("warning_details".into(), details);
        }
    }
    fill_missing(
        &mut response,
        &[
            "video_shortfall",
            "log_shortfall",
            "logcat_completeness",
            "requested_duration_secs",
            "actual_video_duration_ms",
            "actual_log_duration_ms",
            "video_frame_count",
            "log_count",
            "unanchored_log_count",
            "truncated_unanchored_count",
            "discontinuity_count",
            "window_shortfall",
            "warnings",
        ],
    );
    if crate::save_outcome_failed(&response) {
        return Err(Failure {
            rpc_code: -32006,
            label: match response["job"]["failure_class"].as_str() {
                Some("permanent_destination") => "SAVE_NEEDS_RETARGET",
                Some("integrity") => "SAVE_FAILED",
                _ => "FAILED_RETRYABLE",
            },
            message: "保存に失敗しました。ジョブの状態を確認してください。".into(),
            detail: Some(response),
            hint: None,
        });
    }
    let links = ["manifest", "logs", "screens"]
        .map(|r| resources::bundle_link(&response["bundle_id"], r))
        .into();
    Ok(ToolOutput::new(response).with_links(links))
}
