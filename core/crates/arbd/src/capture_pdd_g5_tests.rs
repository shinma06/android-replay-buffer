//! Layer-3 PDD (G5): `SaveJobMaterializeParams` backward-compatible JSON and ring-chunk
//! encode/decode robustness.

#![allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::struct_excessive_bools,
    clippy::fn_params_excessive_bools,
    clippy::trivially_copy_pass_by_ref,
    clippy::print_stdout,
    clippy::collapsible_if,
    clippy::similar_names,
    clippy::format_collect,
    clippy::as_conversions,
    clippy::clone_on_ref_ptr,
    reason = "PDD model/property code: long linearized models and generators read best unsplit"
)]

use std::collections::BTreeMap;

use arb_adb::mock::CaptureEvent;
use arb_adb::scrcpy::{VideoPacket, VideoPacketKind};
use arb_core::bundle::DeviceInfo;
use arb_core::selection::{SelectionPolicy, WindowSpec};
use proptest::prelude::*;

use super::{SaveJobMaterializeParams, SaveMode, decode_ring_chunk, encode_ring_chunk};

fn full_params() -> SaveJobMaterializeParams {
    SaveJobMaterializeParams {
        note: None,
        bundle_id: "01JPDDG5000000000000000001".to_owned(),
        mode: SaveMode::Full,
        duration_sec: 30,
        trigger_utc: "2026-11-24T00:00:00Z".to_owned(),
        window: WindowSpec::RealElapsed {
            boot_id: "boot-g5".to_owned(),
            start_boottime_ns: 1,
            end_boottime_ns: 2,
            ingest_start_cont_ns: 3,
            ingest_end_cont_ns: 4,
        },
        policy: SelectionPolicy::new(),
        tail_may_be_incomplete: true,
        device: DeviceInfo {
            serial: "EMU".to_owned(),
            model: "Pixel".to_owned(),
            api_level: 33,
            is_emulator: true,
        },
        video_width: 1080,
        video_height: 2400,
        display_width: Some(1080),
        display_height: Some(2400),
        fallback_video_config: Some(vec![1, 2]),
        fallback_audio_config: Some(vec![3]),
        trigger_kind: arb_core::state::SaveTriggerKind::Manual,
        extra_discontinuities: Vec::new(),
        uid_packages: BTreeMap::from([(10_001, "com.example".to_owned())]),
    }
}

/// Keys present in the first durable params schema; everything added later must default.
const HISTORICAL_KEYS: [&str; 9] = [
    "bundle_id",
    "duration_sec",
    "trigger_utc",
    "window",
    "policy",
    "tail_may_be_incomplete",
    "device",
    "video_width",
    "video_height",
];

#[test]
fn g5_every_post_historical_field_is_serde_default() {
    let full = serde_json::to_value(full_params()).expect("serialize");
    let obj = full.as_object().expect("object");
    for key in obj
        .keys()
        .filter(|k| !HISTORICAL_KEYS.contains(&k.as_str()))
    {
        let mut v = full.clone();
        v.as_object_mut().expect("object").remove(key);
        serde_json::from_value::<SaveJobMaterializeParams>(v)
            .unwrap_or_else(|e| panic!("field `{key}` lacks #[serde(default)]: {e}"));
    }
    let minimal: serde_json::Map<_, _> = obj
        .iter()
        .filter(|(k, _)| HISTORICAL_KEYS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let p: SaveJobMaterializeParams =
        serde_json::from_value(serde_json::Value::Object(minimal)).expect("historical payload");
    assert_eq!(p.mode, SaveMode::Full);
    assert!(p.display_width.is_none() && p.uid_packages.is_empty());
}

#[test]
fn g5_golden_historical_json_deserializes() {
    let golden = include_str!("../tests/fixtures/pdd_g5_params_v2_minimal.json");
    let p: SaveJobMaterializeParams = serde_json::from_str(golden).expect("golden fixture");
    assert_eq!(p.bundle_id, "01JPDDG5000000000000000001");
    assert_eq!((p.video_width, p.video_height), (1080, 2400));
    let again = serde_json::to_value(&p).expect("reserialize");
    let back: SaveJobMaterializeParams = serde_json::from_value(again).expect("roundtrip");
    assert_eq!(back.trigger_utc, p.trigger_utc);
}

fn video_event(pc: i64, key: bool, data: Vec<u8>) -> CaptureEvent {
    CaptureEvent::VideoFrame {
        pc_cont_ns: pc,
        boot_id: "boot-g5".to_owned(),
        packet: VideoPacket {
            kind: if key {
                VideoPacketKind::Keyframe
            } else {
                VideoPacketKind::DeltaFrame
            },
            pts_us: u64::try_from(pc.max(0)).unwrap_or(0),
            data,
        },
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(64),
        ..ProptestConfig::default()
    })]

    #[test]
    fn g5_ring_chunk_roundtrip(evs in prop::collection::vec((any::<i64>(), any::<bool>(), prop::collection::vec(any::<u8>(), 0..64)), 0..6)) {
        let events: Vec<CaptureEvent> = evs.into_iter().map(|(pc, k, d)| video_event(pc, k, d)).collect();
        let bytes = encode_ring_chunk(events.clone()).expect("encode");
        let decoded = decode_ring_chunk(&bytes).expect("decode");
        prop_assert_eq!(format!("{decoded:?}"), format!("{events:?}"));
        for cut in [1usize, 7, bytes.len() / 2] {
            if cut < bytes.len() && !events.is_empty() {
                prop_assert!(decode_ring_chunk(&bytes[..bytes.len() - cut]).is_err(), "truncation must be rejected");
            }
        }
    }

    #[test]
    fn g5_ring_chunk_arbitrary_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..256), magic in any::<bool>()) {
        let mut input = if magic { super::RING_CHUNK_MAGIC.to_vec() } else { Vec::new() };
        input.extend_from_slice(&bytes);
        let _ = decode_ring_chunk(&input);
    }
}
