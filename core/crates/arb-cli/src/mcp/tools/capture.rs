use super::super::{Failure, McpServer, ToolOutput};
use crate::absolutize_path;
use crate::mcp::{
    args::{absolutize_optional_path_field, optional_abs_path},
    image_block,
    project::time::Zone,
};
use serde_json::{Map, Value, json};
use std::path::Path;
impl McpServer {
    pub(in crate::mcp) fn tool_status(
        &mut self,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        let mut hello = self.call_daemon("system.hello", &json!({}))?;
        if hello.get("gc_dry_run").is_none() {
            hello["gc_dry_run"] = json!(false);
        }
        let mut status = self.call_daemon("system.status", &json!({}))?;
        summarize_audio_reason(&mut status);
        let input_capture = status
            .pointer("/capture/input_capture")
            .cloned()
            .unwrap_or_else(|| json!(arb_core::input_capture::InputCapture::default()));
        let body = if args.get("verbose") == Some(&Value::Bool(true)) {
            json!({"hello":hello,"status":status,"input_capture":input_capture})
        } else {
            compact_status(&hello, &status, &input_capture)
        };
        Ok(ToolOutput::new(body))
    }

    pub(in crate::mcp) fn tool_capture_start(
        &mut self,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        let mut params = args.clone();
        for key in ["companion_jar", "scrcpy_jar"] {
            absolutize_optional_path_field(&mut params, key)?;
        }
        let mut resp = self.call_daemon("capture.start", &Value::Object(params))?;
        if resp.get("connected").is_none() {
            let status = self.call_daemon("system.status", &json!({}))?;
            let capture = &status["capture"];
            let current = resp["started"] == true && capture["serial"] == resp["serial"];
            resp["connected"] = json!(current && capture["connected"] == true);
            resp["audio_state"] = if current {
                capture["audio_state"].clone()
            } else {
                json!("unknown")
            };
            resp["audio_cause"] = if current {
                capture["audio_cause"].clone()
            } else {
                Value::Null
            };
            resp["wait_timed_out"] = json!(false);
        }
        Ok(ToolOutput::new(resp))
    }

    pub(in crate::mcp) fn tool_config_update(
        &mut self,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        let mut patch = args.clone();
        for key in ["output_dir_override", "screenshot_dir_override"] {
            if let Some(val) = patch.get(key).and_then(Value::as_str)
                && !val.is_empty()
            {
                let abs = absolutize_path(Path::new(val)).map_err(|e| e.message)?;
                patch.insert(key.to_owned(), json!(abs));
            }
        }
        let resp = self.call_daemon("config.update", &Value::Object(patch))?;
        Ok(ToolOutput::new(resp))
    }

    pub(in crate::mcp) fn tool_screenshot(
        &mut self,
        args: &Map<String, Value>,
    ) -> Result<ToolOutput, Failure> {
        let include_image = args
            .get("include_image")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut params = Map::new();
        if let Some(source) = args.get("source") {
            params.insert("source".into(), source.clone());
        }
        if let Some(serial) = args.get("serial") {
            params.insert("serial".to_owned(), serial.clone());
        }
        if let Some(out) = optional_abs_path(args, "output_path")? {
            params.insert("output_path".to_owned(), json!(out));
        }
        if let Some(out_dir) = optional_abs_path(args, "output_dir")? {
            params.insert("output_dir".to_owned(), json!(out_dir));
        }
        if let Some(overwrite) = args.get("overwrite") {
            params.insert("overwrite".to_owned(), overwrite.clone());
        }
        let mut resp = self.call_daemon("save.screenshot", &Value::Object(params))?;
        let path = Path::new(
            resp["path"]
                .as_str()
                .ok_or_else(|| Failure::unavailable("スクリーンショットの保存先がありません。"))?,
        );
        let uri = url::Url::from_file_path(path)
            .map_err(|()| Failure::unavailable("スクリーンショットの保存先が不正です。"))?;
        let digest = resp["sha256"]
            .as_str()
            .ok_or_else(|| Failure::unavailable("スクリーンショットの SHA-256 がありません。"))?;
        let image = if include_image {
            crate::mcp::resources::screenshot_blob(path, digest).map(|blob| image_block(&blob))
        } else {
            None
        };
        let link = crate::mcp::resources::ResourceLink::screenshot(path, uri.clone());
        self.remember_screenshot(&uri, digest);
        resp["file_uri"] = json!(uri.as_str());
        Zone::from_args(args).localize_unix_ms(
            &mut resp,
            "captured_at_unix_ms",
            "captured_at_local",
        );
        let output = ToolOutput::new(resp).with_links(vec![link]);
        Ok(match image {
            Some(image) => output.with_image(image),
            None => output,
        })
    }
}
pub(in crate::mcp) fn summarize_audio_reason(response: &mut Value) {
    if let Some(reason) = response.pointer_mut("/capture/audio_reason")
        && let Some(detail) = reason.as_str()
    {
        let message = detail.split("; scrcpy stderr:").next().unwrap_or(detail);
        *reason = Value::String(
            message
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(256)
                .collect(),
        );
    }
}

/// Project the two daemon responses without repeating capability flags or quiet counters.
fn compact_status(hello: &Value, status: &Value, input_capture: &Value) -> Value {
    let cap = &status["capture"];
    let autosave = &status["crash_autosave"];
    let mut attention = Vec::new();
    if let Some(recovery) = status["recovery"].as_object() {
        for (name, value) in recovery {
            if value.as_u64().is_some_and(|n| n > 0) {
                attention.push(json!({"kind":name,"count":value}));
            }
        }
    }
    if let Some(warning) = status["config_warning"].as_str().filter(|s| !s.is_empty()) {
        attention.push(json!({"kind":"config_warning","message":warning}));
    }
    if status["failed_save_jobs"].as_u64().is_some_and(|n| n > 0) {
        attention.push(json!({"kind":"failed_jobs","count":status["failed_save_jobs"]}));
    }
    let state = cap["buffer_state"].as_str().unwrap_or_else(|| {
        if cap["active"] == true {
            "live"
        } else if status["ring_chunk_count"].as_u64().unwrap_or(0) > 0
            || status["buffered_secs"].as_u64().unwrap_or(0) > 0
        {
            "retained_after_stop"
        } else {
            "empty"
        }
    });
    json!({
        "daemon": {"version":hello["daemon_version"], "build_id":hello["build_id"]},
        "capture": {
            "enabled":status["capture_enabled"], "active":cap["active"], "connected":cap["connected"],
            "serial":cap["serial"], "configured_device_serial":status["configured_device_serial"],
            "serial_mismatch":status["serial_mismatch"], "mode":cap["capture_mode"],
            "audio_state":cap["audio_state"],
            "buffer": {"buffered_secs":status["buffered_secs"], "state":state, "newest_age_secs":cap["newest_age_secs"]}
        },
        "input_capture": {"physical":input_capture["physical"], "injected":input_capture["injected"]},
        "device_clock_offset":status.get("device_clock_offset").or_else(|| cap.get("device_clock_offset")),
        "save": {
            "active_jobs":status["active_save_jobs"], "failed_jobs":status["failed_save_jobs"].as_u64().unwrap_or(0),
            "oldest_failed_job_id":status["oldest_failed_job_id"], "ring_pinned_chunk_count":status["ring_pinned_chunk_count"]
        },
        "crash_autosave": {
            "enabled":autosave["enabled"], "in_flight":autosave["in_flight"],
            "post_roll_until_utc":autosave["post_roll_until_utc"],
            "last_bundle_id":autosave["last_bundle_id"], "last_error":autosave["last_error"]
        },
        "attention":attention
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attention_reports_only_nonzero_counts_and_operator_actions() {
        let status = json!({"recovery":{"reaped_leases":0,"recovered_jobs":2,"failed_purging_bundle_ids":["b"]},
            "failed_save_jobs":1,"config_warning":"設定を確認","device_clock_offset":{"status":"available"},
            "crash_autosave":{"post_roll_until_utc":"2026-01-01T00:00:00Z"}});
        let body = compact_status(&Value::Null, &status, &Value::Null);
        assert_eq!(
            body["attention"],
            json!([{"kind":"recovered_jobs","count":2},
            {"kind":"config_warning","message":"設定を確認"},{"kind":"failed_jobs","count":1}])
        );
        assert_eq!(body["device_clock_offset"], status["device_clock_offset"]);
        assert_eq!(
            body["crash_autosave"]["post_roll_until_utc"],
            status["crash_autosave"]["post_roll_until_utc"]
        );
    }
}
