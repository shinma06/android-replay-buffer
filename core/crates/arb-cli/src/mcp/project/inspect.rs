//! `arb_inspect_frames`: one body shape whether or not images are attached.
//!
//! Each distinct screen is listed once in `screens`; a frame refers to it by `screen_index`.
use super::{fill_null, screen_offsets};
use serde_json::{Value, json};

/// Projects `media.inspect_frames` metadata. Frames keep their order, indices, times and taps.
pub(in crate::mcp) fn frames(mut meta: Value) -> Value {
    let mut screens: Vec<Value> = Vec::new();
    for frame in meta["frames"].as_array_mut().into_iter().flatten() {
        let screen = frame
            .as_object_mut()
            .and_then(|f| f.remove("screen"))
            .filter(|s| !s.is_null());
        let index = screen.map(|screen| {
            screens
                .iter()
                .position(|s| *s == screen)
                .unwrap_or_else(|| {
                    screens.push(screen);
                    screens.len() - 1
                })
        });
        frame["screen_index"] = json!(index);
        fill_null(frame, &["offset_ms", "offset_ns", "tap"]);
    }
    fill_null(&mut meta, &["png_path", "png_bytes_len", "tap_crop"]);
    meta["screens"] = Value::Array(screens);
    screen_offsets(&mut meta["screens"]);
    meta
}

/// Adds where the rendered PNG was cached. The metadata fields stay as they are.
pub(in crate::mcp) fn attach_render(body: &mut Value, rendered: &Value) {
    for key in ["png_path", "png_bytes_len", "tap_crop"] {
        if let Some(value) = rendered.get(key).filter(|v| !v.is_null()) {
            body[key] = value.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{attach_render, frames};
    use serde_json::json;

    #[test]
    fn repeated_screens_are_listed_once_and_referenced() {
        let home = json!({"start_ns":0,"end_ns":9,"start_offset_ms":0,"end_offset_ms":0,"short_name":"Home"});
        let meta = json!({"bundle_id":"b","width":10,"height":20,"frame_indices_rendered":[0,1,2],"frames":[
            {"frame_index":0,"timestamp_ms":0,"offset_ms":0,"offset_ns":0,"screen":home,"tap":null},
            {"frame_index":1,"timestamp_ms":33,"offset_ms":33,"offset_ns":33_000_000,"screen":home,"tap":{"x":1,"y":2}},
            {"frame_index":2,"timestamp_ms":66,"screen":null,"tap":null}
        ]});
        let mut body = frames(meta);
        assert_eq!(body["screens"], json!([home]));
        let indices: Vec<_> = body["frames"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["screen_index"].clone())
            .collect();
        assert_eq!(indices, [json!(0), json!(0), json!(null)]);
        assert!(body["frames"][0].get("screen").is_none());
        // An older daemon without media offsets still yields the published fields.
        assert_eq!(body["frames"][2]["offset_ns"], json!(null));
        let before = body.clone();
        attach_render(
            &mut body,
            &json!({"png_path":"/tmp/x.png","png_bytes_len":5,"width":640,"frames":null}),
        );
        assert!(
            before
                .as_object()
                .unwrap()
                .keys()
                .eq(body.as_object().unwrap().keys())
        );
        assert_eq!(body["png_path"], "/tmp/x.png");
        // Rendering adds cache facts; the metadata shape and values are untouched.
        for key in [
            "frames",
            "screens",
            "width",
            "height",
            "frame_indices_rendered",
        ] {
            assert_eq!(body[key], before[key], "{key}");
        }
    }
}
