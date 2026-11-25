use std::{fs, path::Path};

pub(super) fn write_manifest(dir: &Path, id: &str, created: &str) {
    let manifest = serde_json::json!({
        "schema_version": 2, "bundle_id": id, "created_at_utc": created,
        "provenance": {"kind": "original"},
        "device": {"serial": "mock", "model": "mock", "api_level": 33, "is_emulator": true},
        "requested_window": {"kind": "real_elapsed_sec", "duration_sec": 1, "trigger_pc_wall_utc": created},
        "epochs": [], "streams": {}, "unanchored_summary": {"included_count": 0, "truncated_count": 0},
        "files": [{"path": "video.mp4", "bytes": 0, "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}]
    });
    fs::write(dir.join("video.mp4"), []).expect("empty fixture payload");
    fs::write(dir.join("manifest.json"), manifest.to_string()).expect("valid v2 manifest");
}
