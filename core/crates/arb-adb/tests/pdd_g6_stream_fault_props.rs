//! Layer-3 PDD (G6): arbitrary byte splitting and `TimedOut` / `Interrupted` / EOF fault
//! injection at every offset for the ARB wire framing and the scrcpy video tunnel.

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

use std::io::{self, ErrorKind, Read};

use arb_adb::scrcpy::{
    CODEC_ID_H264, SESSION_META_TAG, VideoPacket, VideoPacketKind, VideoRecord, VideoSessionMeta,
    VideoTunnelReader, write_video_handshake, write_video_packet, write_video_session_meta,
};
use arb_adb::wire::{
    DEFAULT_MAX_PAYLOAD_LEN, FrameDeframer, FrameFlags, MessageType, StreamId, WireFrame,
};
use proptest::prelude::*;

#[derive(Debug, Clone, Copy)]
enum Fault {
    None,
    /// One `Interrupted` error at the offset; delivery continues afterwards.
    Interrupted,
    /// Persistent `TimedOut` from the offset on.
    TimedOut,
    /// Stream ends at the offset.
    Eof,
}

/// Delivers `data` in caller-chosen read sizes with a fault at byte offset `at`.
struct FaultReader {
    data: Vec<u8>,
    pos: usize,
    sizes: Vec<usize>,
    call: usize,
    fault: Fault,
    at: usize,
    fired: bool,
}

impl Read for FaultReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let limit = match self.fault {
            Fault::Eof | Fault::TimedOut => self.at.min(self.data.len()),
            _ => self.data.len(),
        };
        if self.pos >= self.at && !self.fired {
            match self.fault {
                Fault::Interrupted => {
                    self.fired = true;
                    return Err(ErrorKind::Interrupted.into());
                }
                Fault::TimedOut => return Err(ErrorKind::TimedOut.into()),
                _ => {}
            }
        }
        let step = self.sizes[self.call % self.sizes.len()].max(1);
        self.call += 1;
        let mut end = (self.pos + step).min(limit).min(self.pos + buf.len());
        if matches!(self.fault, Fault::Interrupted) && !self.fired && self.pos < self.at {
            end = end.min(self.at);
        }
        let n = end - self.pos;
        buf[..n].copy_from_slice(&self.data[self.pos..end]);
        self.pos = end;
        Ok(n)
    }
}

fn fault() -> impl Strategy<Value = Fault> {
    prop_oneof![
        Just(Fault::None),
        Just(Fault::Interrupted),
        Just(Fault::TimedOut),
        Just(Fault::Eof)
    ]
}

fn frames() -> impl Strategy<Value = Vec<WireFrame>> {
    prop::collection::vec(
        (any::<bool>(), prop::collection::vec(any::<u8>(), 0..48)),
        1..6,
    )
    .prop_map(|v| {
        v.into_iter()
            .enumerate()
            .map(|(i, (log, payload))| {
                let (s, m) = if log {
                    (StreamId::Logcat, MessageType::LogRecord)
                } else {
                    (StreamId::Control, MessageType::Heartbeat)
                };
                WireFrame::new(
                    s,
                    m,
                    FrameFlags::from_bits(0),
                    u32::try_from(i).expect("seq"),
                    payload,
                )
                .expect("valid frame")
            })
            .collect()
    })
}

/// Number of whole records contained in the first `cut` bytes, and whether `cut` is a boundary.
fn complete_prefix(bounds: &[usize], cut: usize) -> (usize, bool) {
    let n = bounds.iter().filter(|&&b| b <= cut).count();
    (n, cut == 0 || bounds.contains(&cut))
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(64),
        ..ProptestConfig::default()
    })]

    #[test]
    fn g6_wire_deframer_split_equals_contiguous(fs in frames(), splits in prop::collection::vec(1usize..40, 1..8)) {
        let bytes: Vec<u8> = fs.iter().flat_map(|f| f.encode().expect("encode")).collect();
        let mut d = FrameDeframer::new();
        let mut got = Vec::new();
        let mut pos = 0;
        let mut i = 0;
        while pos < bytes.len() {
            let end = (pos + splits[i % splits.len()]).min(bytes.len());
            d.push_bytes(&bytes[pos..end]);
            while let Some(f) = d.next_frame().expect("valid stream") {
                got.push(f);
            }
            pos = end;
            i += 1;
        }
        prop_assert!(d.finish().is_ok());
        prop_assert_eq!(got, fs);
    }

    #[test]
    fn g6_wire_reader_faults_yield_exact_prefix_and_explicit_gap(
        fs in frames(), sizes in prop::collection::vec(1usize..40, 1..6), f in fault(), at_seed in any::<usize>(),
    ) {
        let mut bytes = Vec::new();
        let mut bounds = Vec::new();
        for fr in &fs {
            bytes.extend(fr.encode().expect("encode"));
            bounds.push(bytes.len());
        }
        let at = at_seed % (bytes.len() + 1);
        let mut r = FaultReader { data: bytes.clone(), pos: 0, sizes, call: 0, fault: f, at, fired: false };
        let mut got = Vec::new();
        let outcome = loop {
            match WireFrame::read_from(&mut r, DEFAULT_MAX_PAYLOAD_LEN) {
                Ok(Some(fr)) => got.push(fr),
                Ok(None) => break Ok(()),
                Err(e) => break Err(e),
            }
        };
        match f {
            Fault::None | Fault::Interrupted => {
                prop_assert!(outcome.is_ok(), "Interrupted is retried transparently");
                prop_assert_eq!(&got, &fs);
            }
            Fault::Eof | Fault::TimedOut => {
                let (n, boundary) = complete_prefix(&bounds, at);
                prop_assert_eq!(&got[..], &fs[..n], "exact complete-record prefix");
                if boundary && matches!(f, Fault::Eof) {
                    prop_assert!(outcome.is_ok(), "EOF on a record boundary is a clean end");
                } else if at < bytes.len() {
                    prop_assert!(outcome.is_err(), "mid-record cut must surface an explicit gap");
                }
            }
        }
    }

    #[test]
    fn g6_scrcpy_video_faults_yield_exact_prefix(
        payloads in prop::collection::vec(prop::collection::vec(any::<u8>(), 1..32), 1..5),
        sizes in prop::collection::vec(1usize..24, 1..6), f in fault(), at_seed in any::<usize>(),
    ) {
        let mut wire = Vec::new();
        write_video_handshake(&mut wire, CODEC_ID_H264).expect("handshake");
        let start = wire.len();
        let meta = VideoSessionMeta { header_tag: SESSION_META_TAG, width: 64, height: 48 };
        write_video_session_meta(&mut wire, meta).expect("meta");
        let mut expected = vec![VideoRecord::SessionMeta(meta)];
        let mut bounds = vec![wire.len()];
        for (i, data) in payloads.into_iter().enumerate() {
            let p = VideoPacket {
                kind: if i == 0 { VideoPacketKind::Keyframe } else { VideoPacketKind::DeltaFrame },
                pts_us: 1_000 * u64::try_from(i).expect("i"),
                data,
            };
            write_video_packet(&mut wire, &p).expect("packet");
            expected.push(VideoRecord::Packet(p));
            bounds.push(wire.len());
        }
        let at = start + at_seed % (wire.len() - start + 1);
        let r = FaultReader { data: wire.clone(), pos: 0, sizes, call: 0, fault: f, at, fired: false };
        let mut reader = VideoTunnelReader::handshake(r).expect("handshake completes before the fault");
        let mut got = Vec::new();
        let outcome = loop {
            match reader.read_next() {
                Ok(Some(rec)) => got.push(rec),
                Ok(None) => break Ok(()),
                Err(e) => break Err(e),
            }
        };
        match f {
            Fault::None | Fault::Interrupted => {
                prop_assert!(outcome.is_ok());
                prop_assert_eq!(&got, &expected);
            }
            Fault::Eof | Fault::TimedOut => {
                let (n, boundary) = complete_prefix(&bounds, at);
                let boundary = boundary || at == start;
                prop_assert_eq!(&got[..], &expected[..n]);
                if boundary && matches!(f, Fault::Eof) {
                    prop_assert!(outcome.is_ok());
                } else if at < wire.len() {
                    prop_assert!(outcome.is_err(), "mid-record cut must surface an explicit gap");
                }
            }
        }
    }
}
