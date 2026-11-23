//! Pure-Rust single-file H.264 + AAC-LC `share.mp4` muxer.
//!
//! [`mux_share_mp4`] combines an ARB v2 `video.mp4` (single-`stsd` `avc1`, in-band SPS/PPS on
//! IDR frames) and an optional `audio.m4a` (AAC-LC `mp4a`/`esds`) into ONE standalone,
//! universally playable ISO-BMFF file:
//!
//! - faststart layout (`ftyp` + `moov` + `mdat`), brands `isom`/`iso2`/`avc1`/`mp41`;
//! - video `trak` (`track_ID = 1`, `avc1`) and, when audio survives range selection, audio
//!   `trak` (`track_ID = 2`, `mp4a`/`esds`);
//! - **strictly zero `edts` / `elst` boxes**: both tracks start at media time `0`;
//! - samples are stream-copied (video keyframes keep their in-band SPS/PPS bit-for-bit) with
//!   one reused buffer, and interleaved in ~1 s chunks so progressive players need no far seeks;
//! - published crash-safely **without replacing** an existing file and refusing to target an
//!   input file (symlink-safe), exactly like the other `arb-media` writers.
//!
//! # `audio_lead_in_offset_ns`
//!
//! The offset is the position of the *first sample of `audio.m4a`* on the video's timeline
//! (`0` = both start together). Because no `edts`/`elst` may be written, the muxer cannot shift a
//! track; it handles the offset honestly instead:
//!
//! - `offset < 0` (audio started earlier): leading AAC access units whose **end** falls at or
//!   before video time `0` are skipped, so the muxed audio starts at `>= 0` with no negative
//!   timestamps. The first retained access unit may straddle `0` by less than one packet; that
//!   sub-packet remainder is reported in [`AvMuxReport::audio_residual_offset_ns`].
//! - `offset > 0` (audio started later): the audio track still starts at media time `0` (no
//!   `elst` delay, no synthesized silence); the whole requested offset is reported in
//!   [`AvMuxReport::audio_residual_offset_ns`] so callers can surface the residual skew.
//!
//! Audio access units whose (shifted) start is at or after the video's end are dropped so the
//! audio never outlasts the picture by more than one packet.

use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::audio::{Mp4AudioTrack, build_mp4a_sample_entry};
use crate::error::MediaError;
use crate::mp4::{
    AvccConfig, ChunkOffsetMode, IndexedFrame, MAX_TRACK_SAMPLES, Mp4VideoTrack,
    avcc_with_4byte_lengths, box_wrap, build_avc1_entry, build_mdhd, build_mvhd_with_next_track_id,
    full_box_wrap, normalize_avcc_nal_lengths, prepend_param_sets_4byte,
    require_supported_nal_length_size, sample_size_u32, split_avcc_nals, ticks_to_ns_unsigned,
};
use crate::output::{Publish, ensure_output_is_not_input, write_atomically};

/// Target interleave span (in each track's own timescale ticks per second) of one `mdat` chunk.
const CHUNK_SPAN_SECONDS: u64 = 1;

/// `track_ID` of the video `trak`.
const VIDEO_TRACK_ID: u32 = 1;

/// `track_ID` of the audio `trak`.
const AUDIO_TRACK_ID: u32 = 2;

/// Summary returned by [`mux_share_mp4`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvMuxReport {
    /// Video frame width in pixels (`avc1` / `tkhd`).
    pub width: u16,
    /// Video frame height in pixels (`avc1` / `tkhd`).
    pub height: u16,
    /// Number of H.264 samples muxed into the video track.
    pub video_frame_count: u64,
    /// Number of sync samples (`stss`) in the video track.
    pub video_keyframe_count: u64,
    /// Video track duration in nanoseconds.
    pub video_duration_ns: u64,
    /// `true` when an audio `trak` (`mp4a`/`esds`) was written.
    pub audio_included: bool,
    /// Number of AAC-LC access units muxed into the audio track (`0` when not included).
    pub audio_packet_count: u64,
    /// Leading access units skipped because they ended at or before video time `0`.
    pub audio_packets_skipped_leading: u64,
    /// Trailing access units dropped because they started at or after the video's end.
    pub audio_packets_dropped_trailing: u64,
    /// Audio track duration in nanoseconds (`0` when not included).
    pub audio_duration_ns: u64,
    /// Audio sample rate in Hz (`0` when not included).
    pub sample_rate_hz: u32,
    /// Audio channel count (`0` when not included).
    pub channels: u16,
    /// The caller-requested position of the first `audio.m4a` sample on the video timeline.
    pub requested_audio_lead_in_offset_ns: i64,
    /// Where the audio track starts on the shared movie timeline (`0` when included, because no
    /// `edts`/`elst` may shift it).
    pub audio_start_ns: i64,
    /// `ideal_start_of_first_muxed_audio_packet - audio_start_ns`: `> 0` means the audio plays
    /// that much **earlier** than ideal (audio began later than video and cannot be delayed
    /// without `elst`); `< 0` means up to one packet **later** than ideal (the first retained
    /// packet straddled video time `0`). `0` when exact or when audio is not included.
    pub audio_residual_offset_ns: i64,
    /// Number of `edts` boxes written (always `0`).
    pub edts_box_count: usize,
    /// Number of `elst` boxes written (always `0`).
    pub elst_box_count: usize,
    /// `true` when `moov` precedes `mdat` (always `true`).
    pub faststart: bool,
    /// Length of the published `share.mp4` in bytes.
    pub output_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackKind {
    Video,
    Audio,
}

/// Sample-table inputs shared by the video and audio `stbl` builders.
#[derive(Debug, Default)]
struct SampleTable {
    sizes: Vec<u32>,
    durations: Vec<u32>,
    /// 1-based sync sample numbers (video only).
    sync: Vec<u32>,
    /// Samples per chunk, in chunk order.
    chunk_counts: Vec<usize>,
    /// Chunk offsets relative to the `mdat` payload start, in chunk order.
    chunk_offsets: Vec<u64>,
}

impl SampleTable {
    fn total_ticks(&self) -> Result<u64, MediaError> {
        self.durations
            .iter()
            .try_fold(0u64, |acc, &d| acc.checked_add(u64::from(d)))
            .ok_or_else(|| MediaError::InvalidContainer("track duration overflow".to_owned()))
    }

    fn payload_bytes(&self) -> Result<u64, MediaError> {
        self.sizes
            .iter()
            .try_fold(0u64, |acc, &s| acc.checked_add(u64::from(s)))
            .ok_or_else(|| MediaError::InvalidContainer("mdat payload size overflow".to_owned()))
    }
}

struct VideoPlan {
    width: u16,
    height: u16,
    timescale: u32,
    avcc: Vec<u8>,
    table: SampleTable,
}

struct AudioPlan {
    timescale: u32,
    sample_rate_hz: u32,
    channels: u16,
    asc: Vec<u8>,
    max_bitrate: u32,
    avg_bitrate: u32,
    first_packet: usize,
    table: SampleTable,
}

/// One scheduled `mdat` chunk: `count` consecutive samples of `kind` starting at sample `first`.
#[derive(Debug, Clone, Copy)]
struct ChunkRef {
    kind: TrackKind,
    first: usize,
    count: usize,
}

/// Muxes `video_mp4` (and, when given, `audio_m4a`) into a single faststart zero-`elst`
/// `share.mp4` at `output_mp4`.
///
/// `audio_lead_in_offset_ns` is the position of the first `audio.m4a` sample on the video
/// timeline (see the [module docs](self) for the exact negative/positive handling). With
/// `audio_m4a == None`, or when no audio access unit remains inside the video's time range, a
/// clean video-only MP4 is written and [`AvMuxReport::audio_included`] is `false`.
///
/// # Errors
///
/// Returns [`MediaError::InvalidArgument`] if `output_mp4` is one of the input files (symlinks
/// included), or the video has no frames; [`MediaError::UnsupportedFeature`] for B-frame video
/// or unsupported `avcC` NAL length sizes; [`MediaError::InvalidContainer`] for malformed
/// inputs; and [`MediaError::Io`] (`AlreadyExists`) if `output_mp4` already exists or on any
/// filesystem failure. A failed mux leaves no output or temp file behind.
pub fn mux_share_mp4(
    video_mp4: impl AsRef<Path>,
    audio_m4a: Option<&Path>,
    audio_lead_in_offset_ns: i64,
    output_mp4: impl AsRef<Path>,
) -> Result<AvMuxReport, MediaError> {
    let video_path = video_mp4.as_ref();
    let out_path = output_mp4.as_ref();
    ensure_output_is_not_input(video_path, out_path)?;
    if let Some(audio_path) = audio_m4a {
        ensure_output_is_not_input(audio_path, out_path)?;
    }

    let video = Mp4VideoTrack::open(video_path)?;
    video.require_no_b_frames("share mux")?;
    let audio = audio_m4a.map(Mp4AudioTrack::open).transpose()?;

    let mut vplan = plan_video(&video)?;
    let video_ticks = vplan.table.total_ticks()?;
    let video_duration_ns = ticks_to_ns_unsigned(video_ticks, vplan.timescale);

    let selection = match &audio {
        Some(track) => select_audio(track, audio_lead_in_offset_ns, video_duration_ns),
        None => AudioSelection::default(),
    };
    let mut aplan = match (&audio, selection.count > 0) {
        (Some(track), true) => Some(plan_audio(track, &selection)?),
        _ => None,
    };

    let schedule = build_schedule(&mut vplan, aplan.as_mut())?;
    let output_bytes = write_share_file(
        out_path,
        &video,
        &vplan,
        audio.as_ref().zip(aplan.as_ref()),
        &schedule,
    )?;

    let video_keyframe_count = u64::try_from(vplan.table.sync.len()).unwrap_or(u64::MAX);
    let mut report = AvMuxReport {
        width: vplan.width,
        height: vplan.height,
        video_frame_count: u64::try_from(vplan.table.sizes.len()).unwrap_or(u64::MAX),
        video_keyframe_count,
        video_duration_ns,
        audio_included: false,
        audio_packet_count: 0,
        audio_packets_skipped_leading: selection.skipped_leading,
        audio_packets_dropped_trailing: selection.dropped_trailing,
        audio_duration_ns: 0,
        sample_rate_hz: 0,
        channels: 0,
        requested_audio_lead_in_offset_ns: audio_lead_in_offset_ns,
        audio_start_ns: 0,
        audio_residual_offset_ns: 0,
        edts_box_count: 0,
        elst_box_count: 0,
        faststart: true,
        output_bytes,
    };
    if let Some(plan) = &aplan {
        report.audio_included = true;
        report.audio_packet_count = u64::try_from(plan.table.sizes.len()).unwrap_or(u64::MAX);
        report.audio_duration_ns = ticks_to_ns_unsigned(plan.table.total_ticks()?, plan.timescale);
        report.sample_rate_hz = plan.sample_rate_hz;
        report.channels = plan.channels;
        report.audio_residual_offset_ns = selection.ideal_first_start_ns;
    }
    Ok(report)
}

// ---------------------------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------------------------

/// Whether the rendered sample needs reading and rewriting (parameter sets / NAL length size).
/// How [`render_video_sample`] rewrites a sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderMode {
    /// First sample or keyframe: in-band SPS/PPS + 4-byte NAL lengths.
    WithParamSets,
    /// Other samples: 4-byte NAL lengths only.
    Plain,
}

impl RenderMode {
    fn for_sample(pos: usize, info: &IndexedFrame) -> Self {
        if pos == 0 || info.is_keyframe {
            Self::WithParamSets
        } else {
            Self::Plain
        }
    }
}

/// Whether the sample must be read and rewritten (otherwise it is copied verbatim).
fn video_sample_needs_render(cfg: &AvccConfig, mode: RenderMode) -> bool {
    mode == RenderMode::WithParamSets || cfg.nal_length_size != 4
}

fn has_inband_param_sets(sample: &[u8]) -> bool {
    let mut sps = false;
    let mut pps = false;
    for nal in split_avcc_nals(sample, 4) {
        match nal[0] & 0x1F {
            7 => sps = true,
            8 => pps = true,
            _ => {}
        }
    }
    sps && pps
}

/// Renders video sample `raw` for the output: 4-byte NAL lengths and, for the first sample and
/// every keyframe, in-band SPS/PPS. Samples that already satisfy this are copied bit-for-bit.
fn render_video_sample(
    cfg: &AvccConfig,
    raw: &[u8],
    mode: RenderMode,
    out: &mut Vec<u8>,
) -> Result<(), MediaError> {
    out.clear();
    if mode == RenderMode::WithParamSets {
        if cfg.nal_length_size == 4 && has_inband_param_sets(raw) {
            out.extend_from_slice(raw);
        } else {
            out.extend_from_slice(&prepend_param_sets_4byte(cfg, raw)?);
        }
    } else if cfg.nal_length_size == 4 {
        out.extend_from_slice(raw);
    } else {
        out.extend_from_slice(&normalize_avcc_nal_lengths(raw, cfg.nal_length_size)?);
    }
    Ok(())
}

fn plan_video(video: &Mp4VideoTrack) -> Result<VideoPlan, MediaError> {
    let cfg = video.avcc();
    require_supported_nal_length_size(cfg.nal_length_size)?;
    if video.frames().is_empty() {
        return Err(MediaError::InvalidArgument(
            "cannot mux a video track without frames".to_owned(),
        ));
    }
    if video.timescale() == 0 {
        return Err(MediaError::InvalidContainer(
            "video timescale must be non-zero".to_owned(),
        ));
    }

    let mut table = SampleTable::default();
    let mut raw = Vec::new();
    let mut rendered = Vec::new();
    for (pos, info) in video.frames().iter().enumerate() {
        let mode = RenderMode::for_sample(pos, info);
        let size = if video_sample_needs_render(cfg, mode) {
            video.read_sample_into(info.frame_index, &mut raw)?;
            render_video_sample(cfg, &raw, mode, &mut rendered)?;
            sample_size_u32(rendered.len())?
        } else {
            info.size
        };
        table.sizes.push(size);
        table.durations.push(info.duration_ticks);
        if info.is_keyframe {
            let number = u32::try_from(pos + 1).map_err(|_| {
                MediaError::InvalidContainer("sync sample index exceeds u32::MAX".to_owned())
            })?;
            table.sync.push(number);
        }
    }
    Ok(VideoPlan {
        width: video.width(),
        height: video.height(),
        timescale: video.timescale(),
        avcc: avcc_with_4byte_lengths(&cfg.raw_avcc),
        table,
    })
}

/// Result of applying the lead-in offset and the video time range to the audio packets.
#[derive(Debug, Default, Clone, Copy)]
struct AudioSelection {
    /// Index of the first retained packet.
    first: usize,
    /// Number of retained packets.
    count: usize,
    skipped_leading: u64,
    dropped_trailing: u64,
    /// Ideal start (on the video timeline) of the first retained packet, minus its placed start `0`.
    ideal_first_start_ns: i64,
}

fn select_audio(track: &Mp4AudioTrack, offset_ns: i64, video_duration_ns: u64) -> AudioSelection {
    let packets = track.packets();
    let offset = i128::from(offset_ns);
    if offset >= i128::from(video_duration_ns) && offset > 0 {
        // The audio would only begin after the picture has ended: nothing lies in range.
        return AudioSelection {
            first: 0,
            count: 0,
            dropped_trailing: u64::try_from(packets.len()).unwrap_or(u64::MAX),
            ..AudioSelection::default()
        };
    }
    let mut first = 0usize;
    if offset < 0 {
        while let Some(p) = packets.get(first) {
            let end = i128::from(p.pts_ns) + i128::from(p.duration_ns) + offset;
            if end > 0 {
                break;
            }
            first += 1;
        }
    }
    let Some(first_packet) = packets.get(first) else {
        return AudioSelection {
            first,
            count: 0,
            skipped_leading: u64::try_from(packets.len()).unwrap_or(u64::MAX),
            ..AudioSelection::default()
        };
    };

    let mut count = 0usize;
    for p in &packets[first..] {
        let placed = ticks_to_ns_unsigned(
            p.pts_ticks.saturating_sub(first_packet.pts_ticks),
            track.timescale(),
        );
        if placed >= video_duration_ns {
            break;
        }
        count += 1;
    }
    let dropped = packets.len() - first - count;
    let ideal_first = i128::from(first_packet.pts_ns) + offset;
    AudioSelection {
        first,
        count,
        skipped_leading: u64::try_from(first).unwrap_or(u64::MAX),
        dropped_trailing: u64::try_from(dropped).unwrap_or(u64::MAX),
        ideal_first_start_ns: i64::try_from(ideal_first).unwrap_or(if ideal_first < 0 {
            i64::MIN
        } else {
            i64::MAX
        }),
    }
}

fn plan_audio(track: &Mp4AudioTrack, sel: &AudioSelection) -> Result<AudioPlan, MediaError> {
    if sel.count > MAX_TRACK_SAMPLES {
        return Err(MediaError::InvalidContainer(format!(
            "audio packet count {} exceeds the {MAX_TRACK_SAMPLES} sample limit",
            sel.count
        )));
    }
    let asc = track.audio_specific_config();
    let kept = &track.packets()[sel.first..sel.first + sel.count];
    let mut table = SampleTable::default();
    let mut total_bytes = 0u128;
    let mut total_ticks = 0u128;
    let mut max_bps = 0u128;
    let ts = u128::from(track.timescale());
    for p in kept {
        table.sizes.push(p.size);
        table.durations.push(p.duration_ticks);
        total_bytes += u128::from(p.size);
        total_ticks += u128::from(p.duration_ticks);
        if let Some(bps) = (u128::from(p.size) * 8 * ts).checked_div(u128::from(p.duration_ticks)) {
            max_bps = max_bps.max(bps);
        }
    }
    let avg_bps = (total_bytes * 8 * ts).checked_div(total_ticks).unwrap_or(0);
    let avg_bitrate = u32::try_from(avg_bps).unwrap_or(u32::MAX);
    let max_bitrate = u32::try_from(max_bps).unwrap_or(u32::MAX).max(avg_bitrate);
    Ok(AudioPlan {
        timescale: track.timescale(),
        sample_rate_hz: asc.sample_rate_hz,
        channels: asc.channel_count,
        asc: asc.raw_bytes.clone(),
        max_bitrate,
        avg_bitrate,
        first_packet: sel.first,
        table,
    })
}

/// Splits `durations` into ~1 s chunks, returning samples per chunk.
fn plan_chunk_counts(durations: &[u32], timescale: u32) -> Vec<usize> {
    let span = u64::from(timescale)
        .saturating_mul(CHUNK_SPAN_SECONDS)
        .max(1);
    let mut counts = Vec::new();
    let mut in_chunk = 0usize;
    let mut chunk_ticks = 0u64;
    for &d in durations {
        if in_chunk > 0 && chunk_ticks >= span {
            counts.push(in_chunk);
            in_chunk = 0;
            chunk_ticks = 0;
        }
        in_chunk += 1;
        chunk_ticks += u64::from(d);
    }
    if in_chunk > 0 {
        counts.push(in_chunk);
    }
    counts
}

/// Interleaves video and audio chunks by start time (video first on ties) and assigns each its
/// `mdat`-relative offset.
fn build_schedule(
    video: &mut VideoPlan,
    mut audio: Option<&mut AudioPlan>,
) -> Result<Vec<ChunkRef>, MediaError> {
    video.table.chunk_counts = plan_chunk_counts(&video.table.durations, video.timescale);
    if let Some(a) = audio.as_deref_mut() {
        a.table.chunk_counts = plan_chunk_counts(&a.table.durations, a.timescale);
    }

    // (start_ticks, timescale, chunk) per track, in start order.
    let starts = |table: &SampleTable| -> Vec<(u64, usize, usize)> {
        let mut out = Vec::with_capacity(table.chunk_counts.len());
        let mut first = 0usize;
        let mut ticks = 0u64;
        for &count in &table.chunk_counts {
            out.push((ticks, first, count));
            for d in &table.durations[first..first + count] {
                ticks += u64::from(*d);
            }
            first += count;
        }
        out
    };
    let v_chunks = starts(&video.table);
    let a_chunks = audio.as_deref().map_or_else(Vec::new, |a| starts(&a.table));
    let v_ts = u128::from(video.timescale);
    let a_ts = audio.as_deref().map_or(1u128, |a| u128::from(a.timescale));

    let mut schedule = Vec::with_capacity(v_chunks.len() + a_chunks.len());
    let (mut vi, mut ai) = (0usize, 0usize);
    while vi < v_chunks.len() || ai < a_chunks.len() {
        let take_video = match (v_chunks.get(vi), a_chunks.get(ai)) {
            (Some(v), Some(a)) => u128::from(v.0) * a_ts <= u128::from(a.0) * v_ts,
            (Some(_), None) => true,
            (None, _) => false,
        };
        if take_video {
            let (_, first, count) = v_chunks[vi];
            schedule.push(ChunkRef {
                kind: TrackKind::Video,
                first,
                count,
            });
            vi += 1;
        } else {
            let (_, first, count) = a_chunks[ai];
            schedule.push(ChunkRef {
                kind: TrackKind::Audio,
                first,
                count,
            });
            ai += 1;
        }
    }

    let mut offset = 0u64;
    for chunk in &schedule {
        let table = match (chunk.kind, audio.as_deref_mut()) {
            (TrackKind::Audio, Some(a)) => &mut a.table,
            _ => &mut video.table,
        };
        table.chunk_offsets.push(offset);
        for size in &table.sizes[chunk.first..chunk.first + chunk.count] {
            offset = offset
                .checked_add(u64::from(*size))
                .ok_or_else(|| MediaError::InvalidContainer("mdat offset overflow".to_owned()))?;
        }
    }
    Ok(schedule)
}

// ---------------------------------------------------------------------------------------------
// Box construction
// ---------------------------------------------------------------------------------------------

fn build_tkhd(
    track_id: u32,
    movie_duration: u64,
    kind: TrackKind,
    width: u16,
    height: u16,
) -> Result<Vec<u8>, MediaError> {
    let mut payload = Vec::with_capacity(96);
    let version = if let Ok(dur32) = u32::try_from(movie_duration) {
        payload.extend_from_slice(&0u32.to_be_bytes()); // creation_time
        payload.extend_from_slice(&0u32.to_be_bytes()); // modification_time
        payload.extend_from_slice(&track_id.to_be_bytes());
        payload.extend_from_slice(&0u32.to_be_bytes()); // reserved
        payload.extend_from_slice(&dur32.to_be_bytes());
        0u8
    } else {
        payload.extend_from_slice(&0u64.to_be_bytes());
        payload.extend_from_slice(&0u64.to_be_bytes());
        payload.extend_from_slice(&track_id.to_be_bytes());
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.extend_from_slice(&movie_duration.to_be_bytes());
        1u8
    };
    payload.extend_from_slice(&[0u8; 8]); // reserved[2]
    payload.extend_from_slice(&0u16.to_be_bytes()); // layer
    payload.extend_from_slice(&u16::from(kind == TrackKind::Audio).to_be_bytes()); // alternate_group
    payload.extend_from_slice(
        &(if kind == TrackKind::Audio {
            0x0100u16
        } else {
            0
        })
        .to_be_bytes(),
    ); // volume
    payload.extend_from_slice(&0u16.to_be_bytes()); // reserved
    for v in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
        payload.extend_from_slice(&v.to_be_bytes());
    }
    payload.extend_from_slice(&(u32::from(width) << 16).to_be_bytes());
    payload.extend_from_slice(&(u32::from(height) << 16).to_be_bytes());
    full_box_wrap(*b"tkhd", version, 3, &payload)
}

fn build_dinf() -> Result<Vec<u8>, MediaError> {
    let url_box = full_box_wrap(*b"url ", 0, 1, &[])?;
    let mut dref_p = Vec::with_capacity(4 + url_box.len());
    dref_p.extend_from_slice(&1u32.to_be_bytes());
    dref_p.extend_from_slice(&url_box);
    box_wrap(*b"dinf", &full_box_wrap(*b"dref", 0, 0, &dref_p)?)
}

fn build_hdlr(handler: [u8; 4], name: &[u8]) -> Result<Vec<u8>, MediaError> {
    let mut p = Vec::with_capacity(21 + name.len());
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&handler);
    p.extend_from_slice(&[0u8; 12]);
    p.extend_from_slice(name);
    full_box_wrap(*b"hdlr", 0, 0, &p)
}

/// Builds `stbl` from an already built `stsd` box and the track's sample table.
fn build_stbl(
    sample_desc: &[u8],
    table: &SampleTable,
    kind: TrackKind,
    base_offset: u64,
    mode: ChunkOffsetMode,
) -> Result<Vec<u8>, MediaError> {
    let sample_count = u32::try_from(table.sizes.len())
        .map_err(|_| MediaError::InvalidContainer("sample count exceeds u32::MAX".to_owned()))?;

    // stts (run-length encoded).
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &d in &table.durations {
        if let Some(last) = runs.last_mut()
            && last.1 == d
        {
            last.0 += 1;
            continue;
        }
        runs.push((1, d));
    }
    let mut time_p = Vec::with_capacity(4 + runs.len() * 8);
    time_p.extend_from_slice(&u32::try_from(runs.len()).unwrap_or(u32::MAX).to_be_bytes());
    for (count, delta) in runs {
        time_p.extend_from_slice(&count.to_be_bytes());
        time_p.extend_from_slice(&delta.to_be_bytes());
    }
    let time_box = full_box_wrap(*b"stts", 0, 0, &time_p)?;

    // stsc (run-length encoded over chunks).
    let mut stsc_runs: Vec<(u32, u32)> = Vec::new(); // (first_chunk, samples_per_chunk)
    for (idx, &count) in table.chunk_counts.iter().enumerate() {
        let count = u32::try_from(count)
            .map_err(|_| MediaError::InvalidContainer("chunk sample count overflow".to_owned()))?;
        if stsc_runs.last().is_some_and(|last| last.1 == count) {
            continue;
        }
        let first_chunk = u32::try_from(idx + 1)
            .map_err(|_| MediaError::InvalidContainer("chunk index overflow".to_owned()))?;
        stsc_runs.push((first_chunk, count));
    }
    let mut chunk_map_p = Vec::with_capacity(4 + stsc_runs.len() * 12);
    chunk_map_p.extend_from_slice(
        &u32::try_from(stsc_runs.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    for (first_chunk, count) in stsc_runs {
        chunk_map_p.extend_from_slice(&first_chunk.to_be_bytes());
        chunk_map_p.extend_from_slice(&count.to_be_bytes());
        chunk_map_p.extend_from_slice(&1u32.to_be_bytes()); // sample_description_index
    }
    let chunk_map = full_box_wrap(*b"stsc", 0, 0, &chunk_map_p)?;

    // stsz (variable sizes).
    let mut size_p = Vec::with_capacity(8 + table.sizes.len() * 4);
    size_p.extend_from_slice(&0u32.to_be_bytes());
    size_p.extend_from_slice(&sample_count.to_be_bytes());
    for size in &table.sizes {
        size_p.extend_from_slice(&size.to_be_bytes());
    }
    let size_box = full_box_wrap(*b"stsz", 0, 0, &size_p)?;

    // stco / co64 (one entry per chunk).
    let chunk_total = u32::try_from(table.chunk_offsets.len())
        .map_err(|_| MediaError::InvalidContainer("chunk count exceeds u32::MAX".to_owned()))?;
    let mut off_p = Vec::with_capacity(4 + table.chunk_offsets.len() * 8);
    off_p.extend_from_slice(&chunk_total.to_be_bytes());
    for rel in &table.chunk_offsets {
        let absolute = base_offset
            .checked_add(*rel)
            .ok_or_else(|| MediaError::InvalidContainer("chunk offset overflow".to_owned()))?;
        match mode {
            ChunkOffsetMode::Co64 => off_p.extend_from_slice(&absolute.to_be_bytes()),
            ChunkOffsetMode::Stco32 => {
                let off32 = u32::try_from(absolute).map_err(|_| {
                    MediaError::InvalidContainer("stco offset exceeds u32::MAX".to_owned())
                })?;
                off_p.extend_from_slice(&off32.to_be_bytes());
            }
        }
    }
    let offsets = match mode {
        ChunkOffsetMode::Co64 => full_box_wrap(*b"co64", 0, 0, &off_p)?,
        ChunkOffsetMode::Stco32 => full_box_wrap(*b"stco", 0, 0, &off_p)?,
    };

    let mut stbl_p = Vec::new();
    stbl_p.extend_from_slice(sample_desc);
    stbl_p.extend_from_slice(&time_box);
    if kind == TrackKind::Video {
        let mut sync_p = Vec::with_capacity(4 + table.sync.len() * 4);
        sync_p.extend_from_slice(
            &u32::try_from(table.sync.len())
                .unwrap_or(u32::MAX)
                .to_be_bytes(),
        );
        for n in &table.sync {
            sync_p.extend_from_slice(&n.to_be_bytes());
        }
        stbl_p.extend_from_slice(&full_box_wrap(*b"stss", 0, 0, &sync_p)?);
    }
    stbl_p.extend_from_slice(&chunk_map);
    stbl_p.extend_from_slice(&size_box);
    stbl_p.extend_from_slice(&offsets);
    box_wrap(*b"stbl", &stbl_p)
}

fn build_stsd(entry: &[u8]) -> Result<Vec<u8>, MediaError> {
    let mut p = Vec::with_capacity(4 + entry.len());
    p.extend_from_slice(&1u32.to_be_bytes());
    p.extend_from_slice(entry);
    full_box_wrap(*b"stsd", 0, 0, &p)
}

fn build_trak(
    tkhd: &[u8],
    mdhd: &[u8],
    hdlr: &[u8],
    media_header: &[u8],
    stbl: &[u8],
) -> Result<Vec<u8>, MediaError> {
    let mut minf_p = Vec::new();
    minf_p.extend_from_slice(media_header);
    minf_p.extend_from_slice(&build_dinf()?);
    minf_p.extend_from_slice(stbl);
    let minf = box_wrap(*b"minf", &minf_p)?;

    let mut mdia_p = Vec::new();
    mdia_p.extend_from_slice(mdhd);
    mdia_p.extend_from_slice(hdlr);
    mdia_p.extend_from_slice(&minf);
    let mdia = box_wrap(*b"mdia", &mdia_p)?;

    // Strictly ZERO `edts` / `elst` boxes inside `trak`.
    let mut trak_p = Vec::new();
    trak_p.extend_from_slice(tkhd);
    trak_p.extend_from_slice(&mdia);
    box_wrap(*b"trak", &trak_p)
}

fn build_share_moov(
    video: &VideoPlan,
    audio: Option<&AudioPlan>,
    base_offset: u64,
    mode: ChunkOffsetMode,
) -> Result<Vec<u8>, MediaError> {
    let movie_timescale = video.timescale;
    let video_ticks = video.table.total_ticks()?;
    let audio_movie_ticks = match audio {
        Some(a) => {
            let ticks = u128::from(a.table.total_ticks()?);
            let ts = u128::from(a.timescale);
            let scaled = (ticks * u128::from(movie_timescale)).div_ceil(ts);
            u64::try_from(scaled)
                .map_err(|_| MediaError::InvalidContainer("audio duration overflow".to_owned()))?
        }
        None => 0,
    };
    let movie_duration = video_ticks.max(audio_movie_ticks);
    let next_track_id = if audio.is_some() {
        AUDIO_TRACK_ID + 1
    } else {
        VIDEO_TRACK_ID + 1
    };

    let mut moov_p = build_mvhd_with_next_track_id(movie_timescale, movie_duration, next_track_id)?;

    // Video trak.
    let avc1 = build_avc1_entry(video.width, video.height, &video.avcc)?;
    let v_stbl = build_stbl(
        &build_stsd(&avc1)?,
        &video.table,
        TrackKind::Video,
        base_offset,
        mode,
    )?;
    let v_trak = build_trak(
        &build_tkhd(
            VIDEO_TRACK_ID,
            video_ticks,
            TrackKind::Video,
            video.width,
            video.height,
        )?,
        &build_mdhd(video.timescale, video_ticks)?,
        &build_hdlr(*b"vide", b"VideoHandler\0")?,
        &full_box_wrap(*b"vmhd", 0, 1, &[0u8; 8])?,
        &v_stbl,
    )?;
    moov_p.extend_from_slice(&v_trak);

    // Audio trak.
    if let Some(a) = audio {
        let mp4a = build_mp4a_sample_entry(
            a.sample_rate_hz,
            a.channels,
            &a.asc,
            a.max_bitrate,
            a.avg_bitrate,
        )?;
        let a_ticks = a.table.total_ticks()?;
        let a_stbl = build_stbl(
            &build_stsd(&mp4a)?,
            &a.table,
            TrackKind::Audio,
            base_offset,
            mode,
        )?;
        let a_trak = build_trak(
            &build_tkhd(AUDIO_TRACK_ID, audio_movie_ticks, TrackKind::Audio, 0, 0)?,
            &build_mdhd(a.timescale, a_ticks)?,
            &build_hdlr(*b"soun", b"SoundHandler\0")?,
            &full_box_wrap(*b"smhd", 0, 0, &[0u8; 4])?,
            &a_stbl,
        )?;
        moov_p.extend_from_slice(&a_trak);
    }
    box_wrap(*b"moov", &moov_p)
}

// ---------------------------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------------------------

fn write_share_file(
    path: &Path,
    video: &Mp4VideoTrack,
    vplan: &VideoPlan,
    audio: Option<(&Mp4AudioTrack, &AudioPlan)>,
    schedule: &[ChunkRef],
) -> Result<u64, MediaError> {
    let mut ftyp_payload = Vec::with_capacity(24);
    ftyp_payload.extend_from_slice(b"isom");
    ftyp_payload.extend_from_slice(&0x0000_0200u32.to_be_bytes());
    ftyp_payload.extend_from_slice(b"isomiso2avc1mp41");
    let ftyp = box_wrap(*b"ftyp", &ftyp_payload)?;

    let aplan = audio.map(|(_, plan)| plan);
    let total_payload = vplan
        .table
        .payload_bytes()?
        .checked_add(aplan.map_or(Ok(0), |a| a.table.payload_bytes())?)
        .ok_or_else(|| MediaError::InvalidContainer("mdat payload size overflow".to_owned()))?;
    let use_large_mdat = total_payload.saturating_add(8) > u64::from(u32::MAX);
    let mdat_header_len = if use_large_mdat { 16u64 } else { 8u64 };

    // `moov` length depends only on the offset width, not on the offset values.
    let ftyp_len = u64::try_from(ftyp.len()).unwrap_or(0);
    let probe = build_share_moov(vplan, aplan, 0, ChunkOffsetMode::Stco32)?;
    let probe_end = ftyp_len
        .saturating_add(u64::try_from(probe.len()).unwrap_or(0))
        .saturating_add(mdat_header_len)
        .saturating_add(total_payload);
    let (mode, sized) = if probe_end > u64::from(u32::MAX) {
        (
            ChunkOffsetMode::Co64,
            build_share_moov(vplan, aplan, 0, ChunkOffsetMode::Co64)?,
        )
    } else {
        (ChunkOffsetMode::Stco32, probe)
    };
    let payload_offset = ftyp_len
        .saturating_add(u64::try_from(sized.len()).unwrap_or(0))
        .saturating_add(mdat_header_len);
    let moov = build_share_moov(vplan, aplan, payload_offset, mode)?;

    let cfg = video.avcc();
    write_atomically(path, Publish::NoReplace, |writer| {
        writer.write_all(&ftyp)?;
        writer.write_all(&moov)?;
        if use_large_mdat {
            writer.write_all(&1u32.to_be_bytes())?;
            writer.write_all(b"mdat")?;
            writer.write_all(&total_payload.saturating_add(16).to_be_bytes())?;
        } else {
            let mdat_total = u32::try_from(total_payload.saturating_add(8)).map_err(|_| {
                MediaError::InvalidContainer("mdat 32-bit size overflow".to_owned())
            })?;
            writer.write_all(&mdat_total.to_be_bytes())?;
            writer.write_all(b"mdat")?;
        }

        let mut raw = Vec::new();
        let mut rendered = Vec::new();
        let mut written = 0u64;
        for chunk in schedule {
            for sample in chunk.first..chunk.first + chunk.count {
                if let (TrackKind::Audio, Some((track, plan))) = (chunk.kind, audio) {
                    let index = u64::try_from(plan.first_packet + sample).unwrap_or(u64::MAX);
                    track.read_packet_into(index, &mut raw)?;
                    write_checked(writer, &raw, plan.table.sizes[sample], "audio", sample)?;
                    written += u64::from(plan.table.sizes[sample]);
                } else {
                    let info = video.frames()[sample];
                    video.read_sample_into(info.frame_index, &mut raw)?;
                    let mode = RenderMode::for_sample(sample, &info);
                    let out: &[u8] = if video_sample_needs_render(cfg, mode) {
                        render_video_sample(cfg, &raw, mode, &mut rendered)?;
                        &rendered
                    } else {
                        &raw
                    };
                    write_checked(writer, out, vplan.table.sizes[sample], "video", sample)?;
                    written += u64::from(vplan.table.sizes[sample]);
                }
            }
        }
        if written != total_payload {
            return Err(MediaError::InvalidContainer(format!(
                "mdat payload wrote {written} bytes but {total_payload} were indexed"
            )));
        }
        writer.flush()?;
        Ok(())
    })
}

fn write_checked(
    writer: &mut impl Write,
    data: &[u8],
    expected: u32,
    what: &str,
    sample: usize,
) -> Result<(), MediaError> {
    if u64::try_from(data.len()).ok() != Some(u64::from(expected)) {
        return Err(MediaError::InvalidContainer(format!(
            "{what} sample {sample} produced {} bytes but {expected} were indexed",
            data.len()
        )));
    }
    writer.write_all(data)?;
    Ok(())
}
