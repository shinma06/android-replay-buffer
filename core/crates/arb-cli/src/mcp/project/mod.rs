//! Projection of daemon responses into the published MCP tool shapes.
//!
//! Daemon RPC payloads stay backward compatible for the IDE and CLI. Every MCP-only reshaping
//! (light log rows, screen references, opt-in local wall clock, follow-up calls) happens here as
//! pure functions of the daemon JSON and the validated arguments. Projection only removes
//! repetition or adds derived fields: it never drops an evidence row.
pub(super) mod inspect;
pub(super) mod logs;
pub(super) mod overview;
pub(super) mod time;

use serde_json::Value;

/// Inserts `null` for each listed key an older daemon omitted, so the published shape holds.
fn fill_null(row: &mut Value, keys: &[&str]) {
    if let Some(obj) = row.as_object_mut() {
        for &key in keys {
            obj.entry(key).or_insert(Value::Null);
        }
    }
}

/// Screen spans from a daemon that predates bundle offsets keep the same published shape.
fn screen_offsets(screens: &mut Value) {
    for span in screens.as_array_mut().into_iter().flatten() {
        fill_null(span, &["start_offset_ms", "end_offset_ms"]);
    }
}
