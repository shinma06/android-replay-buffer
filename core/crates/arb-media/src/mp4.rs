//! Pure-Rust ISO-BMFF (MP4) parser, frame/GOP indexer, and zero-`elst` `avc1` splice muxer.
//!
//! Incorporates Spike S1 findings:
//! - Parses H.264 tracks (`avc1`/`avc3` + `avcC`, `stts`, `ctts`, `stss`, `stsc`, `stsz`, `stco`/`co64`).
//! - Detects B-frame reordering (`has_b_frames`) via `ctts` / non-monotonic presentation order.
//! - Muxes faststart (`ftyp` + `moov` + `mdat`) MP4 files using a **single-`stsd` `avc1`**
//!   descriptor with length-prefixed in-band SPS/PPS prepended at IDR splice boundaries,
//!   and **strictly zero `edts`/`elst` boxes**.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::MediaError;
use crate::output::{Publish, write_atomically};

/// Nanoseconds per second used for tick-to-nanosecond conversion.
pub(crate) const NANOS_PER_SEC: i128 = 1_000_000_000;

/// Fixed byte length of the `VisualSampleEntry` header inside `avc1`/`avc3`.
const VISUAL_SAMPLE_ENTRY_HEADER_LEN: usize = 78;

/// Fixed byte length of a version-0 `AudioSampleEntry` header inside `mp4a`.
pub(crate) const AUDIO_SAMPLE_ENTRY_HEADER_LEN: usize = 28;

/// Fixed byte length of a version-1 `QuickTime` `AudioSampleEntry` header inside `mp4a`.
pub(crate) const AUDIO_SAMPLE_ENTRY_V1_HEADER_LEN: usize = 44;

/// Maximum MP4 `moov` box size accepted into memory (256 MiB safety cap).
pub(crate) const MAX_MOOV_BOX_BYTES: u64 = 256 * 1024 * 1024;

/// Sanity cap on the number of samples in one track (10 M ≈ 92 h at 30 fps).
///
/// Bounds every allocation driven by untrusted sample-table counts (`stts`/`ctts` expansion,
/// constant-size `stsz`) so a crafted few-hundred-byte `moov` can never request tens of GB
/// and abort the process (R2 S8).
pub const MAX_TRACK_SAMPLES: usize = 10_000_000;

/// Validates that a full box declaring `count` entries of `entry_size` bytes after a
/// `header_len`-byte prefix actually contains them, before anything is allocated.
pub(crate) fn checked_entry_count(
    body: &[u8],
    count: u32,
    header_len: usize,
    entry_size: usize,
    what: &str,
) -> Result<usize, MediaError> {
    let count = usize::try_from(count)
        .map_err(|_| MediaError::InvalidContainer(format!("{what} entry count overflow")))?;
    let available = body.len().saturating_sub(header_len) / entry_size;
    if count > available {
        return Err(MediaError::InvalidContainer(format!(
            "{what} declares {count} entries but the box only holds {available}"
        )));
    }
    Ok(count)
}

/// Controls whether SPS/PPS parameter sets are prepended during AVCC-to-Annex B conversion.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParamSetMode {
    /// Do not prepend `avcC` parameter sets.
    Omit,
    /// Always prepend `avcC` SPS and PPS before sample NAL units.
    Prepend,
    /// Prepend `avcC` SPS and PPS only when the sample does not already carry in-band SPS and PPS.
    PrependIfMissing,
}

/// Exact indexed metadata for a single video frame inside an MP4 track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedFrame {
    /// Zero-based sequential sample/frame index in decode order.
    pub frame_index: u64,
    /// Presentation timestamp in nanoseconds.
    pub pts_ns: i64,
    /// Decode timestamp in nanoseconds.
    pub dts_ns: i64,
    /// Frame duration in nanoseconds.
    pub duration_ns: u64,
    /// Whether this frame is an IDR / sync keyframe (`stss`).
    pub is_keyframe: bool,
    /// Zero-based GOP identifier incremented at each keyframe boundary.
    pub gop_id: u64,
    /// Absolute byte offset of the encoded sample inside the MP4 file.
    pub offset: u64,
    /// Encoded sample length in bytes.
    pub size: u32,
    /// Raw presentation timestamp in track timescale ticks.
    pub pts_ticks: i64,
    /// Raw decode timestamp in track timescale ticks.
    pub dts_ticks: u64,
    /// Raw sample duration in track timescale ticks.
    pub duration_ticks: u32,
}

/// Parsed ISO/IEC 14496-15 `AVCDecoderConfigurationRecord` (`avcC`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvccConfig {
    /// Raw `avcC` payload bytes (without the 8-byte box header).
    pub raw_avcc: Vec<u8>,
    /// Length in bytes of each AVCC NAL length prefix (typically 4).
    pub nal_length_size: usize,
    /// Extracted Sequence Parameter Set (SPS) NAL units (without start codes).
    pub sps: Vec<Vec<u8>>,
    /// Extracted Picture Parameter Set (PPS) NAL units (without start codes).
    pub pps: Vec<Vec<u8>>,
}

/// Indexed H.264 video track backed by an open read-only MP4 file handle.
#[derive(Debug)]
pub struct Mp4VideoTrack {
    file: File,
    path: PathBuf,
    file_len: u64,
    width: u16,
    height: u16,
    timescale: u32,
    duration_ticks: u64,
    avcc: AvccConfig,
    frames: Vec<IndexedFrame>,
    keyframes: Vec<u64>,
    has_b_frames: bool,
}

impl Mp4VideoTrack {
    /// Opens an MP4 file and indexes its primary H.264 (`avc1`/`avc3`) video track.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::Io`] if the file cannot be read, or [`MediaError::InvalidContainer`]
    /// if required ISO-BMFF boxes (`moov`, `trak`, `stsd`/`avcC`, `stts`, `stsz`, `stsc`, `stco`/`co64`)
    /// are missing or malformed.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MediaError> {
        let path = path.as_ref().to_path_buf();
        let mut file = File::open(&path)?;
        let file_len = file.metadata()?.len();

        let moov = read_moov_box(&mut file, file_len)?;
        let parsed = parse_moov(&moov, file_len)?;

        Ok(Self {
            file,
            path,
            file_len,
            width: parsed.width,
            height: parsed.height,
            timescale: parsed.timescale,
            duration_ticks: parsed.duration_ticks,
            avcc: parsed.avcc,
            frames: parsed.frames,
            keyframes: parsed.keyframes,
            has_b_frames: parsed.has_b_frames,
        })
    }

    /// Returns the path this track was opened from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the total MP4 file length in bytes.
    pub const fn file_len(&self) -> u64 {
        self.file_len
    }

    /// Returns the coded video width in pixels.
    pub const fn width(&self) -> u16 {
        self.width
    }

    /// Returns the coded video height in pixels.
    pub const fn height(&self) -> u16 {
        self.height
    }

    /// Returns the media timescale (ticks per second) from `mdhd`.
    pub const fn timescale(&self) -> u32 {
        self.timescale
    }

    /// Returns the track duration in `mdhd` timescale ticks.
    pub const fn duration_ticks(&self) -> u64 {
        self.duration_ticks
    }

    /// Returns the parsed `avcC` parameter-set configuration.
    pub const fn avcc(&self) -> &AvccConfig {
        &self.avcc
    }

    /// Returns all indexed frames in decode order.
    pub fn frames(&self) -> &[IndexedFrame] {
        &self.frames
    }

    /// Returns the zero-based frame indices of all sync keyframes (`stss`).
    pub fn keyframes(&self) -> &[u64] {
        &self.keyframes
    }

    /// Returns `true` if `ctts` / presentation-order analysis detected B-frame reordering.
    pub const fn has_b_frames(&self) -> bool {
        self.has_b_frames
    }

    /// Returns the total number of frames in the track.
    pub fn frame_count(&self) -> u64 {
        u64::try_from(self.frames.len()).unwrap_or(u64::MAX)
    }

    /// Returns indexed metadata for `frame_index`.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::FrameOutOfRange`] if `frame_index >= self.frame_count()`.
    pub fn frame(&self, frame_index: u64) -> Result<&IndexedFrame, MediaError> {
        let idx = usize::try_from(frame_index).map_err(|_| MediaError::FrameOutOfRange {
            frame_index,
            frame_count: self.frame_count(),
        })?;
        self.frames.get(idx).ok_or(MediaError::FrameOutOfRange {
            frame_index,
            frame_count: self.frame_count(),
        })
    }

    /// Reads the raw AVCC-encoded sample payload for `frame_index` into `buf` using lock-free
    /// positional I/O.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::FrameOutOfRange`] if `frame_index` is invalid, or [`MediaError::Io`]
    /// if the underlying file read fails.
    pub fn read_sample_into(&self, frame_index: u64, buf: &mut Vec<u8>) -> Result<(), MediaError> {
        let info = self.frame(frame_index)?;
        let len = usize::try_from(info.size).map_err(|_| {
            MediaError::InvalidContainer(format!("sample {frame_index} size exceeds address space"))
        })?;
        buf.resize(len, 0);
        read_exact_at(&self.file, buf, info.offset)?;
        Ok(())
    }

    /// Returns `(gop_start_frame_index, next_gop_start_frame_index)` for `frame_index`.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::FrameOutOfRange`] if `frame_index` is out of bounds.
    pub fn gop_bounds(&self, frame_index: u64) -> Result<(u64, Option<u64>), MediaError> {
        let _ = self.frame(frame_index)?;
        let bounds = match self.keyframes.binary_search(&frame_index) {
            Ok(pos) => (self.keyframes[pos], self.keyframes.get(pos + 1).copied()),
            Err(pos) => {
                let gop_start = if pos > 0 { self.keyframes[pos - 1] } else { 0 };
                let next_gop = self.keyframes.get(pos).copied();
                (gop_start, next_gop)
            }
        };
        Ok(bounds)
    }

    /// Rejects tracks with B-frame reordering for frame-index addressed operations.
    ///
    /// Decoders disagree on whether `frame_index` means decode or presentation order when
    /// frames are reordered (`VideoToolbox` vs `OpenH264`), so every frame-addressed entry point
    /// (`decode_frame`, `decode_gop`, PNG export, GOP cache, trim) refuses them identically on
    /// every OS instead of returning a different "exact" frame per platform.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::UnsupportedFeature`] when [`Self::has_b_frames`] is `true`.
    pub fn require_no_b_frames(&self, operation: &str) -> Result<(), MediaError> {
        if self.has_b_frames {
            return Err(MediaError::UnsupportedFeature(format!(
                "{operation}: input track contains B-frame (ctts) reordering; frame-index \
                 addressing is only defined for streams without reordering"
            )));
        }
        Ok(())
    }

    /// Returns the GOP start for `frame_index`, guaranteeing it is a real sync sample that a
    /// decoder can start from and that the track has no B-frame reordering.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::FrameOutOfRange`] for an invalid index, or
    /// [`MediaError::UnsupportedFeature`] for B-frame tracks or when `frame_index` precedes
    /// the first sync sample (no IDR to decode from).
    pub fn decodable_gop_start(&self, frame_index: u64) -> Result<u64, MediaError> {
        self.require_no_b_frames("decode")?;
        let (gop_start, _) = self.gop_bounds(frame_index)?;
        self.require_sync_sample(gop_start, frame_index)?;
        Ok(gop_start)
    }

    /// Returns the inclusive frame range of `gop_id`, guaranteeing it starts on a sync sample
    /// and that the track has no B-frame reordering.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::InvalidArgument`] if `gop_id` does not exist, or
    /// [`MediaError::UnsupportedFeature`] for B-frame tracks or a GOP without a leading IDR.
    pub fn decodable_gop_range(&self, gop_id: u64) -> Result<(u64, u64), MediaError> {
        self.require_no_b_frames("decode_gop")?;
        let (start, end) = self.gop_range(gop_id).ok_or_else(|| {
            MediaError::InvalidArgument(format!("gop_id {gop_id} does not exist in track"))
        })?;
        self.require_sync_sample(start, start)?;
        Ok((start, end))
    }

    fn require_sync_sample(&self, gop_start: u64, requested: u64) -> Result<(), MediaError> {
        if !self.frame(gop_start)?.is_keyframe {
            return Err(MediaError::UnsupportedFeature(format!(
                "frame {requested} precedes the first sync sample (stss); there is no IDR \
                 to decode from, refusing to emit an undecodable head"
            )));
        }
        Ok(())
    }

    /// Returns the inclusive `(start_frame_index, end_frame_index)` for `gop_id`, if present.
    pub fn gop_range(&self, gop_id: u64) -> Option<(u64, u64)> {
        if self.frames.is_empty() {
            return None;
        }
        let start_pos = self.frames.partition_point(|f| f.gop_id < gop_id);
        let first = self.frames.get(start_pos)?;
        if first.gop_id != gop_id {
            return None;
        }
        let end_exclusive = self.frames.partition_point(|f| f.gop_id <= gop_id);
        let last = self.frames.get(end_exclusive.checked_sub(1)?)?;
        Some((first.frame_index, last.frame_index))
    }
}

/// Scans top-level ISO-BMFF boxes and reads the bounded `moov` payload into memory.
pub(crate) fn read_moov_box(file: &mut File, file_len: u64) -> Result<Vec<u8>, MediaError> {
    let mut pos = 0u64;
    while pos.saturating_add(8) <= file_len {
        let (fourcc, box_size, header_len) = read_top_header(file, pos, file_len)?;
        if &fourcc == b"moov" {
            return read_moov_payload(file, box_size - header_len);
        }
        pos += box_size;
    }

    Err(MediaError::InvalidContainer("missing moov box".to_owned()))
}

fn read_top_header(
    file: &mut (impl Read + Seek),
    pos: u64,
    file_len: u64,
) -> Result<([u8; 4], u64, u64), MediaError> {
    let mut hdr = [0u8; 8];
    file.seek(SeekFrom::Start(pos))?;
    file.read_exact(&mut hdr)?;
    let size32 = u64::from(u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]));
    let fourcc = [hdr[4], hdr[5], hdr[6], hdr[7]];
    let (box_size, header_len) = if size32 == 1 {
        if pos.saturating_add(16) > file_len {
            return Err(MediaError::InvalidContainer(
                "truncated 64-bit top-level box header".to_owned(),
            ));
        }
        let mut ext = [0u8; 8];
        file.read_exact(&mut ext)?;
        (u64::from_be_bytes(ext), 16u64)
    } else if size32 == 0 {
        (file_len - pos, 8u64)
    } else {
        (size32, 8u64)
    };

    if box_size < header_len || pos.checked_add(box_size).is_none_or(|end| end > file_len) {
        return Err(MediaError::InvalidContainer(
            "top-level MP4 box size exceeds file bounds".to_owned(),
        ));
    }

    Ok((fourcc, box_size, header_len))
}
fn read_moov_payload(file: &mut impl Read, payload_len: u64) -> Result<Vec<u8>, MediaError> {
    if payload_len > MAX_MOOV_BOX_BYTES {
        return Err(MediaError::InvalidContainer(
            "moov box exceeds safety size limit".to_owned(),
        ));
    }
    let payload_usize = usize::try_from(payload_len).map_err(|_| {
        MediaError::InvalidContainer("moov payload exceeds address space".to_owned())
    })?;
    let mut buf = vec![0u8; payload_usize];
    file.read_exact(&mut buf)?;
    Ok(buf)
}

pub(crate) use arb_os::fs::read_exact_at;

struct ParsedTrack {
    width: u16,
    height: u16,
    timescale: u32,
    duration_ticks: u64,
    avcc: AvccConfig,
    frames: Vec<IndexedFrame>,
    keyframes: Vec<u64>,
    has_b_frames: bool,
}

pub(crate) fn read_u16_be(data: &[u8], offset: usize) -> Result<u16, MediaError> {
    let end = offset
        .checked_add(2)
        .ok_or_else(|| MediaError::InvalidContainer("u16 read offset overflow".to_owned()))?;
    let bytes: [u8; 2] = data
        .get(offset..end)
        .ok_or_else(|| MediaError::InvalidContainer("unexpected EOF reading u16".to_owned()))?
        .try_into()
        .map_err(|_| MediaError::InvalidContainer("short u16 slice".to_owned()))?;
    Ok(u16::from_be_bytes(bytes))
}

pub(crate) fn read_u32_be(data: &[u8], offset: usize) -> Result<u32, MediaError> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| MediaError::InvalidContainer("u32 read offset overflow".to_owned()))?;
    let bytes: [u8; 4] = data
        .get(offset..end)
        .ok_or_else(|| MediaError::InvalidContainer("unexpected EOF reading u32".to_owned()))?
        .try_into()
        .map_err(|_| MediaError::InvalidContainer("short u32 slice".to_owned()))?;
    Ok(u32::from_be_bytes(bytes))
}

fn read_i32_be(data: &[u8], offset: usize) -> Result<i32, MediaError> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| MediaError::InvalidContainer("i32 read offset overflow".to_owned()))?;
    let bytes: [u8; 4] = data
        .get(offset..end)
        .ok_or_else(|| MediaError::InvalidContainer("unexpected EOF reading i32".to_owned()))?
        .try_into()
        .map_err(|_| MediaError::InvalidContainer("short i32 slice".to_owned()))?;
    Ok(i32::from_be_bytes(bytes))
}

pub(crate) fn read_u64_be(data: &[u8], offset: usize) -> Result<u64, MediaError> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| MediaError::InvalidContainer("u64 read offset overflow".to_owned()))?;
    let bytes: [u8; 8] = data
        .get(offset..end)
        .ok_or_else(|| MediaError::InvalidContainer("unexpected EOF reading u64".to_owned()))?
        .try_into()
        .map_err(|_| MediaError::InvalidContainer("short u64 slice".to_owned()))?;
    Ok(u64::from_be_bytes(bytes))
}

pub(crate) type Mp4BoxSlice<'a> = ([u8; 4], &'a [u8]);

pub(crate) fn iter_boxes(data: &[u8]) -> Result<Vec<Mp4BoxSlice<'_>>, MediaError> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 8 <= data.len() {
        let size32 = usize::try_from(read_u32_be(data, pos)?)
            .map_err(|_| MediaError::InvalidContainer("box size exceeds usize".to_owned()))?;
        let fourcc = [data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]];
        let (box_size, hdr_len) = if size32 == 1 {
            if pos + 16 > data.len() {
                return Err(MediaError::InvalidContainer(
                    "truncated 64-bit box header".to_owned(),
                ));
            }
            let s64 = usize::try_from(read_u64_be(data, pos + 8)?).map_err(|_| {
                MediaError::InvalidContainer("64-bit box size exceeds usize".to_owned())
            })?;
            (s64, 16usize)
        } else if size32 == 0 {
            (data.len() - pos, 8usize)
        } else {
            (size32, 8usize)
        };
        if box_size < hdr_len || pos.checked_add(box_size).is_none_or(|end| end > data.len()) {
            return Err(MediaError::InvalidContainer(
                "nested MP4 box exceeds parent bounds".to_owned(),
            ));
        }
        out.push((fourcc, &data[pos + hdr_len..pos + box_size]));
        pos += box_size;
    }
    Ok(out)
}

pub(crate) fn find_box(data: &[u8], target: [u8; 4]) -> Result<Option<&[u8]>, MediaError> {
    for (cc, body) in iter_boxes(data)? {
        if cc == target {
            return Ok(Some(body));
        }
    }
    Ok(None)
}

fn parse_moov(moov: &[u8], file_len: u64) -> Result<ParsedTrack, MediaError> {
    for (cc, body) in iter_boxes(moov)? {
        if &cc == b"trak"
            && let Some(track) = parse_trak(body, file_len)?
        {
            return Ok(track);
        }
    }
    Err(MediaError::InvalidContainer(
        "no H.264 video track found in moov".to_owned(),
    ))
}

pub(crate) fn parse_mdhd(mdhd: &[u8]) -> Result<(u32, u64), MediaError> {
    let version = *mdhd
        .first()
        .ok_or_else(|| MediaError::InvalidContainer("empty mdhd box".to_owned()))?;
    let (timescale, duration_ticks) = if version == 0 {
        (read_u32_be(mdhd, 12)?, u64::from(read_u32_be(mdhd, 16)?))
    } else if version == 1 {
        (read_u32_be(mdhd, 20)?, read_u64_be(mdhd, 24)?)
    } else {
        return Err(MediaError::InvalidContainer(format!(
            "unsupported mdhd version {version}"
        )));
    };
    if timescale == 0 {
        return Err(MediaError::InvalidContainer(
            "mdhd timescale must be non-zero".to_owned(),
        ));
    }
    Ok((timescale, duration_ticks))
}

/// Parses `stts`, expanding at most `max_samples` per-sample durations (the `stsz` count).
pub(crate) fn parse_stts(stts: &[u8], max_samples: usize) -> Result<Vec<u32>, MediaError> {
    let entry_count = checked_entry_count(stts, read_u32_be(stts, 4)?, 8, 8, "stts")?;
    let mut durations = Vec::with_capacity(max_samples.min(entry_count.max(1)));
    for i in 0..entry_count {
        let off = 8 + i * 8;
        let count = usize::try_from(read_u32_be(stts, off)?)
            .map_err(|_| MediaError::InvalidContainer("stts sample count overflow".to_owned()))?;
        let delta = read_u32_be(stts, off + 4)?;
        let take = count.min(max_samples.saturating_sub(durations.len()));
        durations.extend(std::iter::repeat_n(delta, take));
        if durations.len() >= max_samples {
            break;
        }
    }
    Ok(durations)
}

/// Parses `ctts`, expanding at most `max_samples` per-sample offsets (the `stsz` count).
fn parse_ctts(ctts_box: &[u8], max_samples: usize) -> Result<Vec<i64>, MediaError> {
    let version = *ctts_box
        .first()
        .ok_or_else(|| MediaError::InvalidContainer("empty ctts box".to_owned()))?;
    let entry_count = checked_entry_count(ctts_box, read_u32_be(ctts_box, 4)?, 8, 8, "ctts")?;
    let mut offsets = Vec::with_capacity(max_samples.min(entry_count.max(1)));
    for i in 0..entry_count {
        let off = 8 + i * 8;
        let count = usize::try_from(read_u32_be(ctts_box, off)?)
            .map_err(|_| MediaError::InvalidContainer("ctts sample count overflow".to_owned()))?;
        let comp_offset = if version == 0 {
            i64::from(read_u32_be(ctts_box, off + 4)?)
        } else {
            i64::from(read_i32_be(ctts_box, off + 4)?)
        };
        let take = count.min(max_samples.saturating_sub(offsets.len()));
        offsets.extend(std::iter::repeat_n(comp_offset, take));
        if offsets.len() >= max_samples {
            break;
        }
    }
    Ok(offsets)
}

pub(crate) fn parse_stsz(stsz: &[u8], file_len: u64) -> Result<Vec<u32>, MediaError> {
    let default_size = read_u32_be(stsz, 4)?;
    let raw_count = read_u32_be(stsz, 8)?;
    let sample_count = if default_size == 0 {
        checked_entry_count(stsz, raw_count, 12, 4, "stsz")?
    } else {
        // Constant-size samples must all fit inside the file.
        if u64::from(raw_count).saturating_mul(u64::from(default_size)) > file_len {
            return Err(MediaError::InvalidContainer(format!(
                "stsz declares {raw_count} samples of {default_size} bytes, larger than the file"
            )));
        }
        usize::try_from(raw_count)
            .map_err(|_| MediaError::InvalidContainer("stsz sample count overflow".to_owned()))?
    };
    if sample_count > MAX_TRACK_SAMPLES {
        return Err(MediaError::InvalidContainer(format!(
            "stsz sample count {sample_count} exceeds the {MAX_TRACK_SAMPLES} sample limit"
        )));
    }
    if default_size != 0 {
        return Ok(vec![default_size; sample_count]);
    }
    let mut sizes = Vec::with_capacity(sample_count);
    for i in 0..sample_count {
        sizes.push(read_u32_be(stsz, 12 + i * 4)?);
    }
    Ok(sizes)
}

pub(crate) fn parse_stsc(stsc: &[u8]) -> Result<Vec<(usize, usize)>, MediaError> {
    let count = checked_entry_count(stsc, read_u32_be(stsc, 4)?, 8, 12, "stsc")?;
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let off = 8 + i * 12;
        let first_chunk = usize::try_from(read_u32_be(stsc, off)?)
            .map_err(|_| MediaError::InvalidContainer("stsc first_chunk overflow".to_owned()))?;
        let spc = usize::try_from(read_u32_be(stsc, off + 4)?).map_err(|_| {
            MediaError::InvalidContainer("stsc samples_per_chunk overflow".to_owned())
        })?;
        if spc == 0
            || (entries.is_empty() && first_chunk != 1)
            || entries.last().is_some_and(|&(prev, _)| prev >= first_chunk)
        {
            return Err(MediaError::InvalidContainer(
                "invalid stsc chunk sequence".into(),
            ));
        }
        entries.push((first_chunk, spc));
    }
    if entries.is_empty() {
        return Err(MediaError::InvalidContainer(
            "stsc has no entries".to_owned(),
        ));
    }
    Ok(entries)
}

pub(crate) fn parse_chunk_offsets(stbl: &[u8]) -> Result<Vec<u64>, MediaError> {
    if let Some(stco) = find_box(stbl, *b"stco")? {
        let count = checked_entry_count(stco, read_u32_be(stco, 4)?, 8, 4, "stco")?;
        let mut offsets = Vec::with_capacity(count);
        for i in 0..count {
            offsets.push(u64::from(read_u32_be(stco, 8 + i * 4)?));
        }
        Ok(offsets)
    } else if let Some(co64) = find_box(stbl, *b"co64")? {
        let count = checked_entry_count(co64, read_u32_be(co64, 4)?, 8, 8, "co64")?;
        let mut offsets = Vec::with_capacity(count);
        for i in 0..count {
            offsets.push(read_u64_be(co64, 8 + i * 8)?);
        }
        Ok(offsets)
    } else {
        Err(MediaError::InvalidContainer(
            "missing stco or co64 box".to_owned(),
        ))
    }
}

fn parse_sync_samples(
    stbl: &[u8],
    sample_count: usize,
) -> Result<(Vec<bool>, Vec<u64>), MediaError> {
    let mut is_key = vec![false; sample_count];
    let mut keyframes = Vec::new();
    if let Some(stss) = find_box(stbl, *b"stss")? {
        let count = checked_entry_count(stss, read_u32_be(stss, 4)?, 8, 4, "stss")?;
        for i in 0..count {
            let sample_1 = usize::try_from(read_u32_be(stss, 8 + i * 4)?).map_err(|_| {
                MediaError::InvalidContainer("stss sample index overflow".to_owned())
            })?;
            if sample_1 >= 1 && sample_1 <= sample_count {
                let idx0 = sample_1 - 1;
                if !is_key[idx0] {
                    is_key[idx0] = true;
                    keyframes.push(u64::try_from(idx0).unwrap_or(0));
                }
            }
        }
        keyframes.sort_unstable();
    } else {
        // Per ISO/IEC 14496-12, absent `stss` means every sample is a sync sample.
        for (idx, flag) in is_key.iter_mut().enumerate() {
            *flag = true;
            keyframes.push(u64::try_from(idx).unwrap_or(0));
        }
    }
    Ok((is_key, keyframes))
}

pub(crate) fn ticks_to_ns_signed(ticks: i64, timescale: u32) -> i64 {
    let scaled = i128::from(ticks) * NANOS_PER_SEC / i128::from(timescale);
    i64::try_from(scaled).unwrap_or(if scaled < 0 { i64::MIN } else { i64::MAX })
}

pub(crate) fn ticks_to_ns_unsigned(ticks: u64, timescale: u32) -> u64 {
    let scaled = u128::from(ticks) * 1_000_000_000u128 / u128::from(timescale);
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

fn parse_trak(trak: &[u8], file_len: u64) -> Result<Option<ParsedTrack>, MediaError> {
    let Some(mdia) = find_box(trak, *b"mdia")? else {
        return Ok(None);
    };
    let Some(hdlr) = find_box(mdia, *b"hdlr")? else {
        return Ok(None);
    };
    if hdlr.get(8..12) != Some(b"vide") {
        return Ok(None);
    }

    let mdhd = find_box(mdia, *b"mdhd")?
        .ok_or_else(|| MediaError::InvalidContainer("missing mdhd box".to_owned()))?;
    let (timescale, duration_ticks) = parse_mdhd(mdhd)?;

    let minf = find_box(mdia, *b"minf")?
        .ok_or_else(|| MediaError::InvalidContainer("missing minf box".to_owned()))?;
    let stbl = find_box(minf, *b"stbl")?
        .ok_or_else(|| MediaError::InvalidContainer("missing stbl box".to_owned()))?;

    let sample_desc_box = find_box(stbl, *b"stsd")?
        .ok_or_else(|| MediaError::InvalidContainer("missing stsd box".to_owned()))?;
    let (width, height, avcc) = parse_stsd(sample_desc_box)?;

    // `stsz` first: its (bounded) sample count caps every other table's expansion.
    let sample_size_box = find_box(stbl, *b"stsz")?
        .ok_or_else(|| MediaError::InvalidContainer("missing stsz box".to_owned()))?;
    let sizes = parse_stsz(sample_size_box, file_len)?;
    let sample_count = sizes.len();

    let time_to_sample_box = find_box(stbl, *b"stts")?
        .ok_or_else(|| MediaError::InvalidContainer("missing stts box".to_owned()))?;
    let durations = parse_stts(time_to_sample_box, sample_count)?;

    let ctts_offsets = match find_box(stbl, *b"ctts")? {
        Some(ctts) => Some(parse_ctts(ctts, sample_count)?),
        None => None,
    };

    let sample_to_chunk_box = find_box(stbl, *b"stsc")?
        .ok_or_else(|| MediaError::InvalidContainer("missing stsc box".to_owned()))?;
    let stsc_entries = parse_stsc(sample_to_chunk_box)?;
    let chunk_offsets = parse_chunk_offsets(stbl)?;
    let (is_key, keyframes) = parse_sync_samples(stbl, sample_count)?;

    let offsets = map_sample_offsets(&chunk_offsets, &stsc_entries, &sizes, file_len)?;

    let mut frames = Vec::with_capacity(sample_count);
    let mut dts_ticks = 0u64;
    let mut gop_id = 0u64;
    let mut prev_pts_ticks: Option<i64> = None;
    let mut has_non_monotonic_pts = false;
    let mut first_cts: Option<i64> = None;
    let mut has_varying_cts = false;

    for i in 0..sample_count {
        let frame_index = u64::try_from(i).unwrap_or(0);
        if is_key[i] && i > 0 {
            gop_id += 1;
        }
        let duration_tick = durations.get(i).copied().unwrap_or(1);
        let cts_tick = ctts_offsets
            .as_ref()
            .and_then(|v| v.get(i).copied())
            .unwrap_or(0);
        if ctts_offsets.is_some() {
            match first_cts {
                None => first_cts = Some(cts_tick),
                Some(first) if first != cts_tick => has_varying_cts = true,
                Some(_) => {}
            }
        }
        let dts_signed = i64::try_from(dts_ticks).map_err(|_| {
            MediaError::InvalidContainer("decode timestamp overflows i64".to_owned())
        })?;
        let pts_ticks = dts_signed.saturating_add(cts_tick);
        if let Some(prev) = prev_pts_ticks
            && pts_ticks < prev
        {
            has_non_monotonic_pts = true;
        }
        prev_pts_ticks = Some(pts_ticks);

        frames.push(IndexedFrame {
            frame_index,
            pts_ns: ticks_to_ns_signed(pts_ticks, timescale),
            dts_ns: ticks_to_ns_signed(dts_signed, timescale),
            duration_ns: ticks_to_ns_unsigned(u64::from(duration_tick), timescale),
            is_keyframe: is_key[i],
            gop_id,
            offset: offsets[i],
            size: sizes[i],
            pts_ticks,
            dts_ticks,
            duration_ticks: duration_tick,
        });
        dts_ticks = dts_ticks.saturating_add(u64::from(duration_tick));
    }

    let has_b_frames = has_non_monotonic_pts || has_varying_cts;

    Ok(Some(ParsedTrack {
        width,
        height,
        timescale,
        duration_ticks,
        avcc,
        frames,
        keyframes,
        has_b_frames,
    }))
}

pub(crate) fn map_sample_offsets(
    chunk_offsets: &[u64],
    stsc_entries: &[(usize, usize)],
    sizes: &[u32],
    file_len: u64,
) -> Result<Vec<u64>, MediaError> {
    let sample_count = sizes.len();
    let mut offsets = vec![0u64; sample_count];
    let mut sample_idx = 0usize;

    let mut run = 0;
    for (ci, &chunk_off) in chunk_offsets.iter().enumerate() {
        let chunk_num_1 = ci + 1;
        while stsc_entries
            .get(run + 1)
            .is_some_and(|&(start, _)| start <= chunk_num_1)
        {
            run += 1;
        }
        let spc = stsc_entries
            .get(run)
            .ok_or_else(|| MediaError::InvalidContainer("empty stsc".into()))?
            .1;
        let mut cur_off = chunk_off;
        for _ in 0..spc {
            if sample_idx >= sample_count {
                break;
            }
            let sample_end = cur_off
                .checked_add(u64::from(sizes[sample_idx]))
                .ok_or_else(|| MediaError::InvalidContainer("sample offset overflow".to_owned()))?;
            if sample_end > file_len {
                return Err(MediaError::InvalidContainer(format!(
                    "sample {sample_idx} exceeds file length"
                )));
            }
            offsets[sample_idx] = cur_off;
            cur_off = sample_end;
            sample_idx += 1;
        }
    }

    if sample_idx < sample_count {
        return Err(MediaError::InvalidContainer(format!(
            "stsc/stco mapped only {sample_idx} of {sample_count} samples"
        )));
    }
    Ok(offsets)
}

fn parse_stsd(stsd: &[u8]) -> Result<(u16, u16, AvccConfig), MediaError> {
    let entries = stsd
        .get(8..)
        .ok_or_else(|| MediaError::InvalidContainer("short stsd box".to_owned()))?;
    for (cc, body) in iter_boxes(entries)? {
        if &cc == b"avc1" || &cc == b"avc3" {
            if body.len() < VISUAL_SAMPLE_ENTRY_HEADER_LEN {
                return Err(MediaError::InvalidContainer(
                    "short avc1 sample entry".to_owned(),
                ));
            }
            let width = read_u16_be(body, 24)?;
            let height = read_u16_be(body, 26)?;
            let avcc_bytes = find_box(&body[VISUAL_SAMPLE_ENTRY_HEADER_LEN..], *b"avcC")?
                .ok_or_else(|| MediaError::InvalidContainer("missing avcC in avc1".to_owned()))?;
            let avcc = parse_avcc(avcc_bytes)?;
            return Ok((width, height, avcc));
        }
    }
    Err(MediaError::InvalidContainer(
        "no avc1 or avc3 entry found in stsd".to_owned(),
    ))
}

/// Parses an ISO/IEC 14496-15 `avcC` payload into [`AvccConfig`].
///
/// # Errors
///
/// Returns [`MediaError::InvalidContainer`] if the `avcC` record is truncated or invalid.
pub fn parse_avcc(data: &[u8]) -> Result<AvccConfig, MediaError> {
    if data.len() < 7 {
        return Err(MediaError::InvalidContainer(
            "short avcC payload".to_owned(),
        ));
    }
    let nal_length_size = usize::from((data[4] & 0x03) + 1);
    let sps_count = usize::from(data[5] & 0x1F);
    let mut pos = 6usize;
    let mut sps = Vec::with_capacity(sps_count);
    for _ in 0..sps_count {
        let len = usize::from(read_u16_be(data, pos)?);
        pos += 2;
        let end = pos
            .checked_add(len)
            .ok_or_else(|| MediaError::InvalidContainer("avcC SPS length overflow".to_owned()))?;
        let slice = data
            .get(pos..end)
            .ok_or_else(|| MediaError::InvalidContainer("truncated avcC SPS".to_owned()))?;
        sps.push(slice.to_vec());
        pos = end;
    }
    let pps_count = usize::from(
        *data
            .get(pos)
            .ok_or_else(|| MediaError::InvalidContainer("missing avcC PPS count".to_owned()))?,
    );
    pos += 1;
    let mut pps = Vec::with_capacity(pps_count);
    for _ in 0..pps_count {
        let len = usize::from(read_u16_be(data, pos)?);
        pos += 2;
        let end = pos
            .checked_add(len)
            .ok_or_else(|| MediaError::InvalidContainer("avcC PPS length overflow".to_owned()))?;
        let slice = data
            .get(pos..end)
            .ok_or_else(|| MediaError::InvalidContainer("truncated avcC PPS".to_owned()))?;
        pps.push(slice.to_vec());
        pos = end;
    }
    Ok(AvccConfig {
        raw_avcc: data.to_vec(),
        nal_length_size,
        sps,
        pps,
    })
}

/// Builds an ISO/IEC 14496-15 `avcC` payload from raw SPS and PPS NAL units (no start codes).
///
/// # Errors
///
/// Returns [`MediaError::InvalidContainer`] if SPS or PPS is empty or exceeds `u16::MAX` bytes.
pub fn build_avcc_payload(sps: &[u8], pps: &[u8]) -> Result<Vec<u8>, MediaError> {
    if sps.len() < 4 || pps.is_empty() {
        return Err(MediaError::InvalidContainer(
            "SPS must be at least 4 bytes and PPS must be non-empty".to_owned(),
        ));
    }
    let sps_len = u16::try_from(sps.len())
        .map_err(|_| MediaError::InvalidContainer("SPS length exceeds u16::MAX".to_owned()))?;
    let pps_len = u16::try_from(pps.len())
        .map_err(|_| MediaError::InvalidContainer("PPS length exceeds u16::MAX".to_owned()))?;

    let mut out = Vec::with_capacity(11 + sps.len() + pps.len());
    out.push(1); // configurationVersion
    out.push(sps[1]); // AVCProfileIndication
    out.push(sps[2]); // profile_compatibility
    out.push(sps[3]); // AVCLevelIndication
    out.push(0xFF); // 6 reserved bits + lengthSizeMinusOne = 3 (4-byte NAL length)
    out.push(0xE1); // 3 reserved bits + numOfSequenceParameterSets = 1
    out.extend_from_slice(&sps_len.to_be_bytes());
    out.extend_from_slice(sps);
    out.push(1); // numOfPictureParameterSets = 1
    out.extend_from_slice(&pps_len.to_be_bytes());
    out.extend_from_slice(pps);
    Ok(out)
}

/// Visits each non-empty length-prefixed NAL unit slice inside an AVCC sample without allocating.
pub(crate) fn for_each_avcc_nal<'a>(
    sample: &'a [u8],
    nal_length_size: usize,
    mut f: impl FnMut(&'a [u8]) -> Result<(), MediaError>,
) -> Result<(), MediaError> {
    if nal_length_size == 0 || nal_length_size > 4 {
        return Ok(());
    }
    let mut pos = 0usize;
    while pos + nal_length_size <= sample.len() {
        let mut nal_len = 0usize;
        for &byte in &sample[pos..pos + nal_length_size] {
            nal_len = (nal_len << 8) | usize::from(byte);
        }
        pos += nal_length_size;
        let Some(end) = pos.checked_add(nal_len) else {
            break;
        };
        if end > sample.len() {
            break;
        }
        if nal_len > 0 {
            f(&sample[pos..end])?;
        }
        pos = end;
    }
    Ok(())
}

/// Iterates length-prefixed NAL unit slices inside an AVCC sample.
pub(crate) fn split_avcc_nals(sample: &[u8], nal_length_size: usize) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let _ = for_each_avcc_nal(sample, nal_length_size, |nal| {
        nals.push(nal);
        Ok(())
    });
    nals
}

/// Extracts optional in-band SPS/PPS parameter sets and strips them from an AVCC sample,
/// returning `(inband_sps, inband_pps, stripped_avcc_sample)`.
pub(crate) fn extract_inband_param_sets_and_strip(
    sample: &[u8],
    nal_length_size: usize,
) -> (Vec<Vec<u8>>, Vec<Vec<u8>>, Vec<u8>) {
    let mut sps = Vec::new();
    let mut pps = Vec::new();
    let mut stripped = Vec::with_capacity(sample.len());
    for nal in split_avcc_nals(sample, nal_length_size) {
        let nal_type = nal[0] & 0x1F;
        match nal_type {
            7 => sps.push(nal.to_vec()),
            8 => pps.push(nal.to_vec()),
            _ => {
                if let Ok(len_u32) = u32::try_from(nal.len()) {
                    stripped.extend_from_slice(&len_u32.to_be_bytes());
                    stripped.extend_from_slice(nal);
                }
            }
        }
    }
    (sps, pps, stripped)
}

/// Converts a length-prefixed AVCC sample into Annex B (`00 00 00 01`), optionally prepending
/// `avcC` SPS/PPS parameter sets according to `mode`.
pub fn avcc_sample_to_annexb(
    sample: &[u8],
    avcc: &AvccConfig,
    mode: ParamSetMode,
    out: &mut Vec<u8>,
) {
    out.clear();
    let nals = split_avcc_nals(sample, avcc.nal_length_size);
    let has_seq_ps = nals.iter().any(|n| (n[0] & 0x1F) == 7);
    let has_pic_ps = nals.iter().any(|n| (n[0] & 0x1F) == 8);
    let should_prepend = match mode {
        ParamSetMode::Omit => false,
        ParamSetMode::Prepend => true,
        ParamSetMode::PrependIfMissing => !(has_seq_ps && has_pic_ps),
    };

    if should_prepend {
        for s in &avcc.sps {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(s);
        }
        for p in &avcc.pps {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(p);
        }
    }

    for nal in nals {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
    }
}

/// Splits an Annex B bitstream (`00 00 01` / `00 00 00 01`) into raw NAL unit slices.
pub fn split_annexb_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut starts: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i + 3 <= data.len() {
        if i + 4 <= data.len()
            && data[i] == 0
            && data[i + 1] == 0
            && data[i + 2] == 0
            && data[i + 3] == 1
        {
            starts.push((i, i + 4));
            i += 4;
        } else if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push((i, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut nals = Vec::with_capacity(starts.len());
    for (idx, &(_, payload_start)) in starts.iter().enumerate() {
        let end = starts
            .get(idx + 1)
            .map_or(data.len(), |&(next_start, _)| next_start);
        if payload_start < end {
            nals.push(&data[payload_start..end]);
        }
    }
    nals
}

/// Extracts SPS/PPS (when present) and packs remaining Annex B NAL units into a 4-byte
/// length-prefixed AVCC sample.
///
/// # Errors
///
/// Returns [`MediaError::InvalidContainer`] if any NAL unit exceeds `u32::MAX` bytes.
pub fn annexb_to_avcc_sample(
    annexb: &[u8],
    sps_out: &mut Option<Vec<u8>>,
    pps_out: &mut Option<Vec<u8>>,
) -> Result<Vec<u8>, MediaError> {
    let mut avcc = Vec::with_capacity(annexb.len());
    for nal in split_annexb_nals(annexb) {
        if nal.is_empty() {
            continue;
        }
        let nal_type = nal[0] & 0x1F;
        match nal_type {
            7 => {
                if sps_out.is_none() {
                    *sps_out = Some(nal.to_vec());
                }
            }
            8 => {
                if pps_out.is_none() {
                    *pps_out = Some(nal.to_vec());
                }
            }
            _ => {
                let len_u32 = u32::try_from(nal.len()).map_err(|_| {
                    MediaError::InvalidContainer("NAL unit exceeds u32::MAX".to_owned())
                })?;
                avcc.extend_from_slice(&len_u32.to_be_bytes());
                avcc.extend_from_slice(nal);
            }
        }
    }
    Ok(avcc)
}

/// Prepends 4-byte length-prefixed SPS and PPS NAL units (from `avcc_payload`) in front of
/// an AVCC sample's VCL/non-parameter-set NAL units.
///
/// Existing in-band SPS/PPS units inside `sample` are replaced cleanly so boundaries never
/// accumulate duplicate or stale parameter sets.
///
/// # Errors
///
/// Returns [`MediaError::InvalidContainer`] if `avcc_payload` is invalid or any NAL length
/// exceeds `u32::MAX`.
pub fn prepend_avcc_sps_pps_inband(
    avcc_payload: &[u8],
    sample: &[u8],
) -> Result<Vec<u8>, MediaError> {
    let cfg = parse_avcc(avcc_payload)?;
    let (_, _, stripped) = extract_inband_param_sets_and_strip(sample, cfg.nal_length_size);
    let mut out = Vec::with_capacity(stripped.len() + 128);
    for s in &cfg.sps {
        let len_u32 = u32::try_from(s.len())
            .map_err(|_| MediaError::InvalidContainer("SPS length exceeds u32::MAX".to_owned()))?;
        out.extend_from_slice(&len_u32.to_be_bytes());
        out.extend_from_slice(s);
    }
    for p in &cfg.pps {
        let len_u32 = u32::try_from(p.len())
            .map_err(|_| MediaError::InvalidContainer("PPS length exceeds u32::MAX".to_owned()))?;
        out.extend_from_slice(&len_u32.to_be_bytes());
        out.extend_from_slice(p);
    }
    out.extend_from_slice(&stripped);
    Ok(out)
}

/// One AVCC-encoded video sample to mux into an MP4 track.
#[derive(Debug, Clone)]
pub struct MuxSample {
    /// Length-prefixed AVCC sample bytes.
    pub data: Vec<u8>,
    /// Sample duration in track timescale ticks.
    pub duration_ticks: u32,
    /// Whether this sample is a sync keyframe (`stss`).
    pub is_keyframe: bool,
}

/// Contiguous sequence of samples sharing one `avcC` parameter-set configuration.
#[derive(Debug, Clone)]
pub struct MuxSegment {
    /// Raw `avcC` payload describing this segment's SPS/PPS.
    pub avcc_payload: Vec<u8>,
    /// Ordered samples belonging to this segment.
    pub samples: Vec<MuxSample>,
}

/// Writes a faststart (`ftyp` + `moov` + `mdat`) H.264 MP4 using a **single-`stsd` `avc1`**
/// descriptor with length-prefixed in-band SPS/PPS prepended at segment/IDR boundaries and
/// **strictly zero `edts`/`elst` boxes**.
///
/// # Errors
///
/// Returns [`MediaError::InvalidArgument`] if no samples are supplied or `timescale == 0`,
/// [`MediaError::InvalidContainer`] if `avcC` data is invalid, or [`MediaError::Io`] on write failure.
pub fn write_single_stsd_avc1_mp4(
    path: impl AsRef<Path>,
    width: u16,
    height: u16,
    timescale: u32,
    segments: &[MuxSegment],
) -> Result<(), MediaError> {
    if timescale == 0 {
        return Err(MediaError::InvalidArgument(
            "mp4 timescale must be non-zero".to_owned(),
        ));
    }
    let non_empty: Vec<&MuxSegment> = segments.iter().filter(|s| !s.samples.is_empty()).collect();
    if non_empty.is_empty() {
        return Err(MediaError::InvalidArgument(
            "cannot mux empty sample sequence".to_owned(),
        ));
    }

    // Every emitted sample uses 4-byte NAL lengths (R2 S5): segment heads / keyframes get
    // in-band SPS/PPS (re-emitted with 4-byte lengths), other samples are normalised only
    // when their segment's `avcC` declares a different length size (borrowed otherwise, so
    // no second copy of the payload is made — R2 S7).
    let mut flat: Vec<std::borrow::Cow<'_, [u8]>> = Vec::new();
    let mut metas: Vec<SampleMeta> = Vec::new();
    for seg in &non_empty {
        let cfg = parse_avcc(&seg.avcc_payload)?;
        require_supported_nal_length_size(cfg.nal_length_size)?;
        for (idx, sample) in seg.samples.iter().enumerate() {
            let data: std::borrow::Cow<'_, [u8]> = if idx == 0 || sample.is_keyframe {
                prepend_param_sets_4byte(&cfg, &sample.data)?.into()
            } else if cfg.nal_length_size == 4 {
                sample.data.as_slice().into()
            } else {
                normalize_avcc_nal_lengths(&sample.data, cfg.nal_length_size)?.into()
            };
            metas.push(SampleMeta {
                size: sample_size_u32(data.len())?,
                duration_ticks: sample.duration_ticks,
                is_keyframe: sample.is_keyframe,
            });
            flat.push(data);
        }
    }

    // Use the last (stream-copied remainder, or sole) segment's avcC as the single `avc1` entry,
    // declaring the 4-byte NAL length every sample now uses.
    let primary_avcc = avcc_with_4byte_lengths(
        &non_empty
            .last()
            .ok_or_else(|| MediaError::InvalidArgument("no non-empty segments".to_owned()))?
            .avcc_payload,
    );

    write_faststart_avc1_mp4(
        path.as_ref(),
        Publish::Replace,
        &Avc1TrackHeader {
            width,
            height,
            timescale,
            avcc_payload: &primary_avcc,
        },
        &metas,
        |idx, buf| {
            buf.clear();
            buf.extend_from_slice(&flat[idx]);
            Ok(())
        },
    )
}

/// Rejects `avcC` NAL length sizes that ISO/IEC 14496-15 does not allow (`3`) or that are
/// out of range.
pub(crate) fn require_supported_nal_length_size(nal_length_size: usize) -> Result<(), MediaError> {
    if matches!(nal_length_size, 1 | 2 | 4) {
        Ok(())
    } else {
        Err(MediaError::UnsupportedFeature(format!(
            "unsupported avcC NAL length size {nal_length_size} (expected 1, 2 or 4)"
        )))
    }
}

/// Re-emits every NAL unit of an AVCC `sample` with 4-byte length prefixes into `out` (reusing its
/// allocation).
///
/// # Errors
///
/// Returns [`MediaError::InvalidContainer`] if the sample is not a well-formed sequence of
/// `nal_length_size`-prefixed NAL units.
pub(crate) fn normalize_avcc_nal_lengths_into(
    sample: &[u8],
    nal_length_size: usize,
    out: &mut Vec<u8>,
) -> Result<(), MediaError> {
    require_supported_nal_length_size(nal_length_size)?;
    out.clear();
    let mut pos = 0usize;
    while pos < sample.len() {
        let prefix = sample.get(pos..pos + nal_length_size).ok_or_else(|| {
            MediaError::InvalidContainer("AVCC sample has a truncated NAL length prefix".to_owned())
        })?;
        let nal_len = prefix
            .iter()
            .fold(0usize, |acc, &byte| (acc << 8) | usize::from(byte));
        pos += nal_length_size;
        let nal = pos
            .checked_add(nal_len)
            .and_then(|end| sample.get(pos..end))
            .ok_or_else(|| {
                MediaError::InvalidContainer("AVCC NAL unit exceeds the sample".to_owned())
            })?;
        if !nal.is_empty() {
            let len = u32::try_from(nal.len()).map_err(|_| {
                MediaError::InvalidContainer("NAL unit exceeds u32::MAX".to_owned())
            })?;
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(nal);
        }
        pos += nal_len;
    }
    Ok(())
}

/// Computes the byte length that [`normalize_avcc_nal_lengths_into`] would emit without
/// materialising the rewritten sample.
pub(crate) fn normalized_avcc_sample_len(
    sample: &[u8],
    nal_length_size: usize,
) -> Result<usize, MediaError> {
    require_supported_nal_length_size(nal_length_size)?;
    let mut total = 0usize;
    let mut pos = 0usize;
    while pos < sample.len() {
        let prefix = sample.get(pos..pos + nal_length_size).ok_or_else(|| {
            MediaError::InvalidContainer("AVCC sample has a truncated NAL length prefix".to_owned())
        })?;
        let nal_len = prefix
            .iter()
            .fold(0usize, |acc, &byte| (acc << 8) | usize::from(byte));
        pos += nal_length_size;
        let nal = pos
            .checked_add(nal_len)
            .and_then(|end| sample.get(pos..end))
            .ok_or_else(|| {
                MediaError::InvalidContainer("AVCC NAL unit exceeds the sample".to_owned())
            })?;
        if !nal.is_empty() {
            let _ = u32::try_from(nal.len()).map_err(|_| {
                MediaError::InvalidContainer("NAL unit exceeds u32::MAX".to_owned())
            })?;
            total = total
                .checked_add(4)
                .and_then(|n| n.checked_add(nal.len()))
                .ok_or_else(|| {
                    MediaError::InvalidContainer("normalized sample length overflow".to_owned())
                })?;
        }
        pos += nal_len;
    }
    Ok(total)
}

/// Re-emits every NAL unit of an AVCC `sample` with 4-byte length prefixes.
///
/// # Errors
///
/// Returns [`MediaError::InvalidContainer`] if the sample is not a well-formed sequence of
/// `nal_length_size`-prefixed NAL units.
pub fn normalize_avcc_nal_lengths(
    sample: &[u8],
    nal_length_size: usize,
) -> Result<Vec<u8>, MediaError> {
    let mut out = Vec::with_capacity(sample.len() + 16);
    normalize_avcc_nal_lengths_into(sample, nal_length_size, &mut out)?;
    Ok(out)
}

/// Returns a copy of `avcc` whose `lengthSizeMinusOne` is 3 (4-byte NAL lengths).
pub(crate) fn avcc_with_4byte_lengths(avcc: &[u8]) -> Vec<u8> {
    let mut out = avcc.to_vec();
    if let Some(byte) = out.get_mut(4) {
        *byte |= 0x03;
    }
    out
}

/// [`prepend_avcc_sps_pps_inband`] with an already parsed `avcC`, writing directly into `out`
/// (output uses 4-byte NAL lengths; reuses `out`'s allocation).
pub(crate) fn prepend_param_sets_4byte_into(
    cfg: &AvccConfig,
    sample: &[u8],
    out: &mut Vec<u8>,
) -> Result<(), MediaError> {
    out.clear();
    for ps in cfg.sps.iter().chain(&cfg.pps) {
        let len_u32 = u32::try_from(ps.len()).map_err(|_| {
            MediaError::InvalidContainer("parameter set length exceeds u32::MAX".to_owned())
        })?;
        out.extend_from_slice(&len_u32.to_be_bytes());
        out.extend_from_slice(ps);
    }
    for_each_avcc_nal(sample, cfg.nal_length_size, |nal| {
        let nal_type = nal[0] & 0x1F;
        if nal_type != 7 && nal_type != 8 {
            let len_u32 = u32::try_from(nal.len()).map_err(|_| {
                MediaError::InvalidContainer("NAL unit exceeds u32::MAX".to_owned())
            })?;
            out.extend_from_slice(&len_u32.to_be_bytes());
            out.extend_from_slice(nal);
        }
        Ok(())
    })?;
    Ok(())
}

/// Computes the byte length that [`prepend_param_sets_4byte_into`] would emit without
/// materialising the rewritten sample.
pub(crate) fn prepended_param_sets_4byte_len(
    cfg: &AvccConfig,
    sample: &[u8],
) -> Result<usize, MediaError> {
    let mut total = 0usize;
    for ps in cfg.sps.iter().chain(&cfg.pps) {
        let _ = u32::try_from(ps.len()).map_err(|_| {
            MediaError::InvalidContainer("parameter set length exceeds u32::MAX".to_owned())
        })?;
        total = total
            .checked_add(4)
            .and_then(|n| n.checked_add(ps.len()))
            .ok_or_else(|| {
                MediaError::InvalidContainer("parameter-set sample length overflow".to_owned())
            })?;
    }
    for_each_avcc_nal(sample, cfg.nal_length_size, |nal| {
        let nal_type = nal[0] & 0x1F;
        if nal_type != 7 && nal_type != 8 {
            let _ = u32::try_from(nal.len()).map_err(|_| {
                MediaError::InvalidContainer("NAL unit exceeds u32::MAX".to_owned())
            })?;
            total = total
                .checked_add(4)
                .and_then(|n| n.checked_add(nal.len()))
                .ok_or_else(|| {
                    MediaError::InvalidContainer("parameter-set sample length overflow".to_owned())
                })?;
        }
        Ok(())
    })?;
    Ok(total)
}

/// [`prepend_avcc_sps_pps_inband`] with an already parsed `avcC` (output is 4-byte lengths).
pub(crate) fn prepend_param_sets_4byte(
    cfg: &AvccConfig,
    sample: &[u8],
) -> Result<Vec<u8>, MediaError> {
    let mut out = Vec::with_capacity(sample.len() + 128);
    prepend_param_sets_4byte_into(cfg, sample, &mut out)?;
    Ok(out)
}

/// Writes a frame-accurate trim: the optional re-encoded `head` segment (held in memory; at
/// most one GOP) followed by `track` samples `copy_range` (inclusive) **streamed from the
/// source file** with one reused buffer, so a long remainder is never held in RAM (R2 S7).
///
/// Copied samples are normalised exactly like [`write_single_stsd_avc1_mp4`]: the first copied
/// sample and every keyframe get the track's SPS/PPS in-band, and every sample uses 4-byte
/// NAL lengths (R2 S5). The output is published crash-safely without replacing an existing
/// file (R2 S6).
pub(crate) fn write_trim_mp4(
    path: &Path,
    track: &Mp4VideoTrack,
    head: Option<&MuxSegment>,
    copy_range: Option<(u64, u64)>,
) -> Result<u64, MediaError> {
    let cfg = track.avcc();
    require_supported_nal_length_size(cfg.nal_length_size)?;

    let mut head_data: Vec<Vec<u8>> = Vec::new();
    let mut metas: Vec<SampleMeta> = Vec::new();
    if let Some(seg) = head.filter(|s| !s.samples.is_empty()) {
        let head_cfg = parse_avcc(&seg.avcc_payload)?;
        require_supported_nal_length_size(head_cfg.nal_length_size)?;
        for (idx, sample) in seg.samples.iter().enumerate() {
            let data = if idx == 0 || sample.is_keyframe {
                prepend_param_sets_4byte(&head_cfg, &sample.data)?
            } else {
                normalize_avcc_nal_lengths(&sample.data, head_cfg.nal_length_size)?
            };
            metas.push(SampleMeta {
                size: sample_size_u32(data.len())?,
                duration_ticks: sample.duration_ticks,
                is_keyframe: sample.is_keyframe,
            });
            head_data.push(data);
        }
    }

    // Pass 1 (sizes only): samples copied verbatim need no read at all.
    let mut buf = Vec::new();
    let mut scratch = Vec::new();
    let copy_frames: Vec<u64> = copy_range.map_or_else(Vec::new, |(a, b)| (a..=b).collect());
    for (pos, &idx) in copy_frames.iter().enumerate() {
        let info = *track.frame(idx)?;
        let size = if pos == 0 || info.is_keyframe || cfg.nal_length_size != 4 {
            track.read_sample_into(idx, &mut buf)?;
            let transform = if pos == 0 || info.is_keyframe {
                CopyTransform::WithParamSets
            } else {
                CopyTransform::NormalizeOnly
            };
            transform_copied_sample(cfg, &buf, transform, &mut scratch)?;
            scratch.len()
        } else {
            usize::try_from(info.size).map_err(|_| {
                MediaError::InvalidContainer("sample size exceeds address space".to_owned())
            })?
        };
        metas.push(SampleMeta {
            size: sample_size_u32(size)?,
            duration_ticks: info.duration_ticks,
            is_keyframe: info.is_keyframe,
        });
    }
    if metas.is_empty() {
        return Err(MediaError::InvalidArgument(
            "cannot mux empty sample sequence".to_owned(),
        ));
    }

    let primary_avcc = if copy_frames.is_empty() {
        avcc_with_4byte_lengths(head.map_or(&[][..], |h| h.avcc_payload.as_slice()))
    } else {
        avcc_with_4byte_lengths(&cfg.raw_avcc)
    };
    let head_len = head_data.len();
    let mut written = 0u64;
    write_faststart_avc1_mp4(
        path,
        Publish::NoReplace,
        &Avc1TrackHeader {
            width: track.width(),
            height: track.height(),
            timescale: track.timescale(),
            avcc_payload: &primary_avcc,
        },
        &metas,
        |idx, out| {
            out.clear();
            if let Some(data) = head_data.get(idx) {
                out.extend_from_slice(data);
                return Ok(());
            }
            let pos = idx - head_len;
            let frame_idx = copy_frames[pos];
            let is_key = track.frame(frame_idx)?.is_keyframe;
            track.read_sample_into(frame_idx, &mut buf)?;
            if pos == 0 || is_key || cfg.nal_length_size != 4 {
                let transform = if pos == 0 || is_key {
                    CopyTransform::WithParamSets
                } else {
                    CopyTransform::NormalizeOnly
                };
                transform_copied_sample(cfg, &buf, transform, out)?;
            } else {
                out.extend_from_slice(&buf);
            }
            written += 1;
            Ok(())
        },
    )?;
    Ok(written)
}

/// How a stream-copied sample is rewritten for the spliced output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CopyTransform {
    /// Segment head or keyframe: in-band SPS/PPS + 4-byte NAL lengths.
    WithParamSets,
    /// Other samples: 4-byte NAL lengths only.
    NormalizeOnly,
}

fn avcc_sample_has_param_sets(sample: &[u8], nal_length_size: usize) -> Result<bool, MediaError> {
    let mut has_seq_ps = false;
    let mut has_pic_ps = false;
    for_each_avcc_nal(sample, nal_length_size, |nal| {
        match nal[0] & 0x1F {
            7 => has_seq_ps = true,
            8 => has_pic_ps = true,
            _ => {}
        }
        Ok(())
    })?;
    Ok(has_seq_ps && has_pic_ps)
}

fn transform_copied_sample(
    cfg: &AvccConfig,
    sample: &[u8],
    transform: CopyTransform,
    out: &mut Vec<u8>,
) -> Result<(), MediaError> {
    out.clear();
    match transform {
        CopyTransform::WithParamSets => {
            if avcc_sample_has_param_sets(sample, cfg.nal_length_size)? {
                normalize_avcc_nal_lengths_into(sample, cfg.nal_length_size, out)?;
            } else {
                prepend_param_sets_4byte_into(cfg, sample, out)?;
            }
        }
        CopyTransform::NormalizeOnly => {
            normalize_avcc_nal_lengths_into(sample, cfg.nal_length_size, out)?;
        }
    }
    Ok(())
}

/// Raw Annex B H.264 access unit captured from a device or synthetic generator.
#[derive(Debug, Clone)]
pub struct RawH264Packet {
    /// Annex B bitstream bytes (`00 00 00 01` start codes) for one frame.
    pub annex_b: Vec<u8>,
    /// Presentation timestamp in nanoseconds.
    pub pts_ns: i64,
    /// Hint indicating whether this packet is an IDR keyframe (also auto-detected from NAL type 5).
    pub is_keyframe: bool,
}

/// Summary returned by [`mux_raw_h264_to_mp4`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MuxReport {
    /// Number of video frames muxed into the output MP4.
    pub frame_count: u64,
    /// Number of IDR keyframes recorded in `stss`.
    pub keyframe_count: u64,
    /// Track timescale written to `mdhd`/`mvhd`.
    pub timescale: u32,
}

/// Muxes a sequence of captured Annex B H.264 packets with PTS timestamps into a clean faststart
/// v2 `video.mp4` (`single-stsd avc1` + in-band SPS/PPS, zero `edts`/`elst`).
///
/// # Errors
///
/// Returns [`MediaError::InvalidArgument`] if `packets` is empty or `timescale == 0`,
/// [`MediaError::InvalidContainer`] if no SPS/PPS parameter sets appear in the stream, or
/// [`MediaError::Io`] on write failure.
pub fn mux_raw_h264_to_mp4(
    path: impl AsRef<Path>,
    width: u16,
    height: u16,
    timescale: u32,
    default_frame_duration_ticks: u32,
    packets: &[RawH264Packet],
) -> Result<MuxReport, MediaError> {
    if timescale == 0 || default_frame_duration_ticks == 0 {
        return Err(MediaError::InvalidArgument(
            "timescale and default frame duration must be non-zero".to_owned(),
        ));
    }
    if packets.is_empty() {
        return Err(MediaError::InvalidArgument(
            "cannot mux empty packet slice".to_owned(),
        ));
    }

    let mut sps: Option<Vec<u8>> = None;
    let mut pps: Option<Vec<u8>> = None;
    let mut segments: Vec<MuxSegment> = Vec::new();
    let mut leading = Vec::new();
    let mut keyframe_count = 0u64;

    for (idx, pkt) in packets.iter().enumerate() {
        let has_idr_nal = split_annexb_nals(&pkt.annex_b)
            .iter()
            .any(|nal| !nal.is_empty() && (nal[0] & 0x1F) == 5);
        let is_keyframe = pkt.is_keyframe || has_idr_nal;
        if is_keyframe {
            keyframe_count += 1;
        }
        let mut sequence_set = None;
        let mut picture_set = None;
        let avcc_data = annexb_to_avcc_sample(&pkt.annex_b, &mut sequence_set, &mut picture_set)?;
        let changed = sequence_set
            .as_ref()
            .is_some_and(|v| Some(v) != sps.as_ref())
            || picture_set
                .as_ref()
                .is_some_and(|v| Some(v) != pps.as_ref());
        if let Some(value) = sequence_set {
            sps = Some(value);
        }
        if let Some(value) = picture_set {
            pps = Some(value);
        }
        if (changed || segments.is_empty())
            && let (Some(seq), Some(pic)) = (&sps, &pps)
        {
            if !segments.is_empty() && !has_idr_nal {
                return Err(MediaError::InvalidContainer(
                    "configuration changed without IDR".into(),
                ));
            }
            segments.push(MuxSegment {
                avcc_payload: build_avcc_payload(seq, pic)?,
                samples: std::mem::take(&mut leading),
            });
        }
        if avcc_data.is_empty() {
            return Err(MediaError::InvalidContainer(format!(
                "packet {idx} contained no VCL/slice NAL units"
            )));
        }

        let duration_ticks = packets
            .get(idx + 1)
            .and_then(|next| {
                let delta_ns = next.pts_ns.checked_sub(pkt.pts_ns)?;
                if delta_ns <= 0 {
                    return None;
                }
                let ticks = (i128::from(delta_ns) * i128::from(timescale) + NANOS_PER_SEC / 2)
                    / NANOS_PER_SEC;
                u32::try_from(ticks).ok().filter(|&t| t > 0)
            })
            .unwrap_or(default_frame_duration_ticks);

        segments
            .last_mut()
            .map_or(&mut leading, |s| &mut s.samples)
            .push(MuxSample {
                data: avcc_data,
                duration_ticks,
                is_keyframe,
            });
    }

    if segments.is_empty() {
        return Err(MediaError::InvalidContainer(
            "missing H.264 parameter sets".into(),
        ));
    }
    let frame_count = u64::try_from(packets.len()).unwrap_or(u64::MAX);
    write_single_stsd_avc1_mp4(path, width, height, timescale, &segments)?;

    Ok(MuxReport {
        frame_count,
        keyframe_count,
        timescale,
    })
}

/// Size/timing of one muxed sample; the payload itself is produced on demand while writing.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SampleMeta {
    pub(crate) size: u32,
    pub(crate) duration_ticks: u32,
    pub(crate) is_keyframe: bool,
}

/// Converts an in-memory sample length into the `stsz` `u32` size.
pub(crate) fn sample_size_u32(len: usize) -> Result<u32, MediaError> {
    u32::try_from(len)
        .map_err(|_| MediaError::InvalidContainer("sample length exceeds u32::MAX".to_owned()))
}

/// Track-level parameters of the single `avc1` sample entry.
pub(crate) struct Avc1TrackHeader<'a> {
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) timescale: u32,
    pub(crate) avcc_payload: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChunkOffsetMode {
    Stco32,
    Co64,
}

pub(crate) fn box_wrap(fourcc: [u8; 4], payload: &[u8]) -> Result<Vec<u8>, MediaError> {
    let total = u32::try_from(payload.len().saturating_add(8))
        .map_err(|_| MediaError::InvalidContainer("MP4 box exceeds 32-bit size".to_owned()))?;
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(&fourcc);
    out.extend_from_slice(payload);
    Ok(out)
}

pub(crate) fn full_box_wrap(
    fourcc: [u8; 4],
    version: u8,
    flags: u32,
    payload: &[u8],
) -> Result<Vec<u8>, MediaError> {
    let total = u32::try_from(payload.len().saturating_add(12))
        .map_err(|_| MediaError::InvalidContainer("MP4 full box exceeds 32-bit size".to_owned()))?;
    let mut out = Vec::with_capacity(payload.len() + 12);
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(&fourcc);
    out.push(version);
    out.push(u8::try_from((flags >> 16) & 0xFF).unwrap_or(0));
    out.push(u8::try_from((flags >> 8) & 0xFF).unwrap_or(0));
    out.push(u8::try_from(flags & 0xFF).unwrap_or(0));
    out.extend_from_slice(payload);
    Ok(out)
}

pub(crate) fn build_avc1_entry(
    width: u16,
    height: u16,
    avcc_payload: &[u8],
) -> Result<Vec<u8>, MediaError> {
    let mut body = Vec::with_capacity(VISUAL_SAMPLE_ENTRY_HEADER_LEN + 8 + avcc_payload.len());
    body.extend_from_slice(&[0u8; 6]); // reserved
    body.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index = 1
    body.extend_from_slice(&[0u8; 16]); // pre_defined + reserved
    body.extend_from_slice(&width.to_be_bytes());
    body.extend_from_slice(&height.to_be_bytes());
    body.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // horizresolution 72 dpi
    body.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // vertresolution 72 dpi
    body.extend_from_slice(&0u32.to_be_bytes()); // reserved
    body.extend_from_slice(&1u16.to_be_bytes()); // frame_count
    body.extend_from_slice(&[0u8; 32]); // compressorname
    body.extend_from_slice(&0x0018u16.to_be_bytes()); // depth = 24
    body.extend_from_slice(&(-1i16).to_be_bytes()); // pre_defined = -1
    let avcc_box = box_wrap(*b"avcC", avcc_payload)?;
    body.extend_from_slice(&avcc_box);
    box_wrap(*b"avc1", &body)
}

pub(crate) fn build_mvhd(timescale: u32, total_duration: u64) -> Result<Vec<u8>, MediaError> {
    build_mvhd_with_next_track_id(timescale, total_duration, 2)
}

/// [`build_mvhd`] with an explicit `next_track_ID` (3 for a two-track `share.mp4`).
pub(crate) fn build_mvhd_with_next_track_id(
    timescale: u32,
    total_duration: u64,
    next_track_id: u32,
) -> Result<Vec<u8>, MediaError> {
    let mut payload = Vec::with_capacity(112);
    let version = if let Ok(dur32) = u32::try_from(total_duration) {
        payload.extend_from_slice(&0u32.to_be_bytes()); // creation_time
        payload.extend_from_slice(&0u32.to_be_bytes()); // modification_time
        payload.extend_from_slice(&timescale.to_be_bytes());
        payload.extend_from_slice(&dur32.to_be_bytes());
        0u8
    } else {
        payload.extend_from_slice(&0u64.to_be_bytes());
        payload.extend_from_slice(&0u64.to_be_bytes());
        payload.extend_from_slice(&timescale.to_be_bytes());
        payload.extend_from_slice(&total_duration.to_be_bytes());
        1u8
    };
    payload.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
    payload.extend_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
    payload.extend_from_slice(&[0u8; 10]); // reserved
    for v in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
        payload.extend_from_slice(&v.to_be_bytes());
    }
    payload.extend_from_slice(&[0u8; 24]); // pre_defined
    payload.extend_from_slice(&next_track_id.to_be_bytes()); // next_track_ID
    full_box_wrap(*b"mvhd", version, 0, &payload)
}

fn build_tkhd(width: u16, height: u16, total_duration: u64) -> Result<Vec<u8>, MediaError> {
    let mut payload = Vec::with_capacity(96);
    let version = if let Ok(dur32) = u32::try_from(total_duration) {
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.extend_from_slice(&1u32.to_be_bytes()); // track_ID = 1
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.extend_from_slice(&dur32.to_be_bytes());
        0u8
    } else {
        payload.extend_from_slice(&0u64.to_be_bytes());
        payload.extend_from_slice(&0u64.to_be_bytes());
        payload.extend_from_slice(&1u32.to_be_bytes());
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.extend_from_slice(&total_duration.to_be_bytes());
        1u8
    };
    payload.extend_from_slice(&[0u8; 8]);
    payload.extend_from_slice(&0u16.to_be_bytes()); // layer
    payload.extend_from_slice(&0u16.to_be_bytes()); // alternate_group
    payload.extend_from_slice(&0u16.to_be_bytes()); // volume
    payload.extend_from_slice(&0u16.to_be_bytes()); // reserved
    for v in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
        payload.extend_from_slice(&v.to_be_bytes());
    }
    payload.extend_from_slice(&(u32::from(width) << 16).to_be_bytes());
    payload.extend_from_slice(&(u32::from(height) << 16).to_be_bytes());
    full_box_wrap(*b"tkhd", version, 3, &payload)
}

pub(crate) fn build_mdhd(timescale: u32, total_duration: u64) -> Result<Vec<u8>, MediaError> {
    let mut payload = Vec::with_capacity(32);
    let version = if let Ok(dur32) = u32::try_from(total_duration) {
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.extend_from_slice(&0u32.to_be_bytes());
        payload.extend_from_slice(&timescale.to_be_bytes());
        payload.extend_from_slice(&dur32.to_be_bytes());
        0u8
    } else {
        payload.extend_from_slice(&0u64.to_be_bytes());
        payload.extend_from_slice(&0u64.to_be_bytes());
        payload.extend_from_slice(&timescale.to_be_bytes());
        payload.extend_from_slice(&total_duration.to_be_bytes());
        1u8
    };
    payload.extend_from_slice(&0x55C4u16.to_be_bytes()); // 'und'
    payload.extend_from_slice(&0u16.to_be_bytes());
    full_box_wrap(*b"mdhd", version, 0, &payload)
}

fn build_stbl(
    width: u16,
    height: u16,
    avcc_payload: &[u8],
    samples: &[SampleMeta],
    mdat_payload_offset: u64,
    offset_mode: ChunkOffsetMode,
) -> Result<Vec<u8>, MediaError> {
    let sample_count_u32 = u32::try_from(samples.len())
        .map_err(|_| MediaError::InvalidContainer("sample count exceeds u32::MAX".to_owned()))?;

    // Single `stsd` entry (`avc1`) — proven in S1 to pass ffmpeg SW, VideoToolbox HW, AVAssetReader.
    let avc1 = build_avc1_entry(width, height, avcc_payload)?;
    let mut sample_desc_payload = Vec::with_capacity(4 + avc1.len());
    sample_desc_payload.extend_from_slice(&1u32.to_be_bytes());
    sample_desc_payload.extend_from_slice(&avc1);
    let sample_desc_box = full_box_wrap(*b"stsd", 0, 0, &sample_desc_payload)?;

    // stts (run-length encoded)
    let mut stts_runs: Vec<(u32, u32)> = Vec::new();
    for sm in samples {
        if let Some(last) = stts_runs.last_mut()
            && last.1 == sm.duration_ticks
        {
            last.0 += 1;
            continue;
        }
        stts_runs.push((1, sm.duration_ticks));
    }
    let stts_count = u32::try_from(stts_runs.len())
        .map_err(|_| MediaError::InvalidContainer("stts run count exceeds u32::MAX".to_owned()))?;
    let mut time_to_sample_payload = Vec::with_capacity(4 + stts_runs.len() * 8);
    time_to_sample_payload.extend_from_slice(&stts_count.to_be_bytes());
    for (cnt, delta) in stts_runs {
        time_to_sample_payload.extend_from_slice(&cnt.to_be_bytes());
        time_to_sample_payload.extend_from_slice(&delta.to_be_bytes());
    }
    let time_to_sample_box = full_box_wrap(*b"stts", 0, 0, &time_to_sample_payload)?;

    // stss (1-based sync sample indices)
    let mut syncs: Vec<u32> = Vec::new();
    for (idx, sm) in samples.iter().enumerate() {
        if sm.is_keyframe {
            let idx_1 = u32::try_from(idx + 1).map_err(|_| {
                MediaError::InvalidContainer("sync sample index exceeds u32::MAX".to_owned())
            })?;
            syncs.push(idx_1);
        }
    }
    let sync_count = u32::try_from(syncs.len())
        .map_err(|_| MediaError::InvalidContainer("stss count exceeds u32::MAX".to_owned()))?;
    let mut sync_table_payload = Vec::with_capacity(4 + syncs.len() * 4);
    sync_table_payload.extend_from_slice(&sync_count.to_be_bytes());
    for s in syncs {
        sync_table_payload.extend_from_slice(&s.to_be_bytes());
    }
    let sync_table_box = full_box_wrap(*b"stss", 0, 0, &sync_table_payload)?;

    // stsc: 1 single chunk containing all samples, referencing sample_description_index = 1
    let mut chunk_map_payload = Vec::with_capacity(16);
    chunk_map_payload.extend_from_slice(&1u32.to_be_bytes()); // entry_count = 1
    chunk_map_payload.extend_from_slice(&1u32.to_be_bytes()); // first_chunk = 1
    chunk_map_payload.extend_from_slice(&sample_count_u32.to_be_bytes()); // samples_per_chunk
    chunk_map_payload.extend_from_slice(&1u32.to_be_bytes()); // sample_description_index = 1
    let chunk_map_box = full_box_wrap(*b"stsc", 0, 0, &chunk_map_payload)?;

    // stsz
    let mut size_table_payload = Vec::with_capacity(8 + samples.len() * 4);
    size_table_payload.extend_from_slice(&0u32.to_be_bytes()); // variable sample sizes
    size_table_payload.extend_from_slice(&sample_count_u32.to_be_bytes());
    for sm in samples {
        size_table_payload.extend_from_slice(&sm.size.to_be_bytes());
    }
    let size_table_box = full_box_wrap(*b"stsz", 0, 0, &size_table_payload)?;

    // stco or co64
    let chunk_offset_box = match offset_mode {
        ChunkOffsetMode::Co64 => {
            let mut co64_p = Vec::with_capacity(12);
            co64_p.extend_from_slice(&1u32.to_be_bytes());
            co64_p.extend_from_slice(&mdat_payload_offset.to_be_bytes());
            full_box_wrap(*b"co64", 0, 0, &co64_p)?
        }
        ChunkOffsetMode::Stco32 => {
            let off32 = u32::try_from(mdat_payload_offset).map_err(|_| {
                MediaError::InvalidContainer("stco offset exceeds u32::MAX".to_owned())
            })?;
            let mut stco_p = Vec::with_capacity(8);
            stco_p.extend_from_slice(&1u32.to_be_bytes());
            stco_p.extend_from_slice(&off32.to_be_bytes());
            full_box_wrap(*b"stco", 0, 0, &stco_p)?
        }
    };

    let mut stbl_p = Vec::new();
    stbl_p.extend_from_slice(&sample_desc_box);
    stbl_p.extend_from_slice(&time_to_sample_box);
    stbl_p.extend_from_slice(&sync_table_box);
    stbl_p.extend_from_slice(&chunk_map_box);
    stbl_p.extend_from_slice(&size_table_box);
    stbl_p.extend_from_slice(&chunk_offset_box);
    box_wrap(*b"stbl", &stbl_p)
}

fn build_moov(
    track: &Avc1TrackHeader<'_>,
    samples: &[SampleMeta],
    mdat_payload_offset: u64,
    offset_mode: ChunkOffsetMode,
) -> Result<Vec<u8>, MediaError> {
    let Avc1TrackHeader {
        width,
        height,
        timescale,
        avcc_payload,
    } = *track;
    let total_duration = samples
        .iter()
        .try_fold(0u64, |sum, sm| {
            sum.checked_add(u64::from(sm.duration_ticks))
        })
        .ok_or_else(|| MediaError::InvalidContainer("track duration overflow".into()))?;

    let movie_header = build_mvhd(timescale, total_duration)?;
    let track_header = build_tkhd(width, height, total_duration)?;
    let media_header = build_mdhd(timescale, total_duration)?;

    let mut hdlr_p = Vec::with_capacity(33);
    hdlr_p.extend_from_slice(&0u32.to_be_bytes());
    hdlr_p.extend_from_slice(b"vide");
    hdlr_p.extend_from_slice(&[0u8; 12]);
    hdlr_p.extend_from_slice(b"VideoHandler\0");
    let hdlr = full_box_wrap(*b"hdlr", 0, 0, &hdlr_p)?;

    let vmhd = full_box_wrap(*b"vmhd", 0, 1, &[0u8; 8])?;

    let url_box = full_box_wrap(*b"url ", 0, 1, &[])?;
    let mut dref_p = Vec::with_capacity(4 + url_box.len());
    dref_p.extend_from_slice(&1u32.to_be_bytes());
    dref_p.extend_from_slice(&url_box);
    let dref = full_box_wrap(*b"dref", 0, 0, &dref_p)?;
    let dinf = box_wrap(*b"dinf", &dref)?;

    let stbl = build_stbl(
        width,
        height,
        avcc_payload,
        samples,
        mdat_payload_offset,
        offset_mode,
    )?;

    let mut minf_p = Vec::new();
    minf_p.extend_from_slice(&vmhd);
    minf_p.extend_from_slice(&dinf);
    minf_p.extend_from_slice(&stbl);
    let minf = box_wrap(*b"minf", &minf_p)?;

    let mut mdia_p = Vec::new();
    mdia_p.extend_from_slice(&media_header);
    mdia_p.extend_from_slice(&hdlr);
    mdia_p.extend_from_slice(&minf);
    let mdia = box_wrap(*b"mdia", &mdia_p)?;

    // Strictly ZERO `edts` / `elst` boxes written inside `trak`.
    let mut trak_p = Vec::new();
    trak_p.extend_from_slice(&track_header);
    trak_p.extend_from_slice(&mdia);
    let trak_box = box_wrap(*b"trak", &trak_p)?;

    let mut moov_p = Vec::new();
    moov_p.extend_from_slice(&movie_header);
    moov_p.extend_from_slice(&trak_box);
    box_wrap(*b"moov", &moov_p)
}

/// Writes a faststart (`ftyp` + `moov` + `mdat`) MP4 whose sample sizes are known up front
/// (`samples`); `fill(i, buf)` produces sample `i`'s exact bytes on demand (one reused buffer).
/// The file is written crash-safely via [`write_atomically`].
pub(crate) fn write_faststart_avc1_mp4(
    path: &Path,
    mode: Publish,
    track: &Avc1TrackHeader<'_>,
    samples: &[SampleMeta],
    mut fill: impl FnMut(usize, &mut Vec<u8>) -> Result<(), MediaError>,
) -> Result<(), MediaError> {
    let mut ftyp_payload = Vec::with_capacity(24);
    ftyp_payload.extend_from_slice(b"isom");
    ftyp_payload.extend_from_slice(&0x0000_0200u32.to_be_bytes());
    ftyp_payload.extend_from_slice(b"isomiso2avc1mp41");
    let ftyp = box_wrap(*b"ftyp", &ftyp_payload)?;

    let total_mdat_payload: u64 = samples
        .iter()
        .try_fold(0u64, |acc, sm| acc.checked_add(u64::from(sm.size)))
        .ok_or_else(|| MediaError::InvalidContainer("mdat payload size overflow".to_owned()))?;

    let use_large_mdat = total_mdat_payload > u64::from(u32::MAX) - 8;
    let mdat_header_len = if use_large_mdat { 16u64 } else { 8u64 };

    let moov_probe = build_moov(track, samples, 0, ChunkOffsetMode::Stco32)?;
    let payload_offset = |moov_len: usize| {
        ftyp.len()
            .checked_add(moov_len)
            .and_then(|len| u64::try_from(len).ok())
            .and_then(|n| n.checked_add(mdat_header_len))
            .ok_or_else(|| MediaError::InvalidContainer("mdat offset overflow".into()))
    };
    let probe_offset = payload_offset(moov_probe.len())?;
    let offset_mode = if probe_offset > u64::from(u32::MAX) {
        ChunkOffsetMode::Co64
    } else {
        ChunkOffsetMode::Stco32
    };

    let moov_pass1 = match offset_mode {
        ChunkOffsetMode::Co64 => build_moov(track, samples, 0, ChunkOffsetMode::Co64)?,
        ChunkOffsetMode::Stco32 => moov_probe,
    };
    let mdat_payload_offset = payload_offset(moov_pass1.len())?;
    let moov = build_moov(track, samples, mdat_payload_offset, offset_mode)?;

    write_atomically(path, mode, |writer| {
        writer.write_all(&ftyp)?;
        writer.write_all(&moov)?;
        if use_large_mdat {
            let mdat_total = total_mdat_payload
                .checked_add(16)
                .ok_or_else(|| MediaError::InvalidContainer("mdat size overflow".into()))?;
            writer.write_all(&1u32.to_be_bytes())?;
            writer.write_all(b"mdat")?;
            writer.write_all(&mdat_total.to_be_bytes())?;
        } else {
            let mdat_total = u32::try_from(total_mdat_payload + 8).map_err(|_| {
                MediaError::InvalidContainer("mdat 32-bit size overflow".to_owned())
            })?;
            writer.write_all(&mdat_total.to_be_bytes())?;
            writer.write_all(b"mdat")?;
        }
        let mut buf = Vec::new();
        for (idx, meta) in samples.iter().enumerate() {
            fill(idx, &mut buf)?;
            if u64::try_from(buf.len()).ok() != Some(u64::from(meta.size)) {
                return Err(MediaError::InvalidContainer(format!(
                    "sample {idx} produced {} bytes but {} were indexed",
                    buf.len(),
                    meta.size
                )));
            }
            writer.write_all(&buf)?;
        }
        writer.flush()?;
        Ok(())
    })?;
    Ok(())
}

/// Summary of one inspected ISO-BMFF atom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mp4BoxInfo {
    /// Four-character box type (e.g., `moov`, `avc1`, `stsd`).
    pub fourcc: [u8; 4],
    /// Nesting depth (`0` for top-level boxes).
    pub depth: usize,
    /// Byte offset from the start of the MP4 file.
    pub offset: u64,
    /// Total box size in bytes including header.
    pub size: u64,
}

/// Container atom audit returned by [`inspect_mp4_atoms`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mp4AtomReport {
    /// All parsed ISO-BMFF boxes in pre-order traversal.
    pub boxes: Vec<Mp4BoxInfo>,
    /// Number of `edts` boxes encountered (must be `0` for ARB outputs).
    pub edts_count: usize,
    /// Number of `elst` boxes encountered (must be `0` for ARB outputs).
    pub elst_count: usize,
    /// Number of `avc1` sample entries encountered.
    pub avc1_count: usize,
    /// Number of `avc3` sample entries encountered.
    pub avc3_count: usize,
    /// Number of `stsd` entries declared in the first video `stsd` box.
    pub stsd_entry_count: u32,
    /// `true` if `moov` appears before `mdat` (faststart layout).
    pub faststart: bool,
}

/// Recursively inspects the ISO-BMFF box hierarchy of an MP4 file.
///
/// # Errors
///
/// Returns [`MediaError::Io`] if the file cannot be read, or [`MediaError::InvalidContainer`] if
/// the file contains malformed box headers.
pub fn inspect_mp4_atoms(path: impl AsRef<Path>) -> Result<Mp4AtomReport, MediaError> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    inspect_atoms(&mut file, len)
}

fn inspect_atoms(file: &mut (impl Read + Seek), len: u64) -> Result<Mp4AtomReport, MediaError> {
    let mut report = Mp4AtomReport {
        boxes: Vec::new(),
        edts_count: 0,
        elst_count: 0,
        avc1_count: 0,
        avc3_count: 0,
        stsd_entry_count: 0,
        faststart: false,
    };
    let mut moov_offset: Option<u64> = None;
    let mut mdat_offset: Option<u64> = None;

    let mut pos = 0;
    while pos < len {
        let (fourcc, size, header) = read_top_header(file, pos, len)?;
        report.boxes.push(Mp4BoxInfo {
            fourcc,
            size,
            depth: 0,
            offset: pos,
        });
        if &fourcc == b"moov" {
            moov_offset.get_or_insert(pos);
            let payload = read_moov_payload(file, size - header)?;
            walk_atoms(
                &payload,
                pos + header,
                1,
                &mut report,
                &mut moov_offset,
                &mut mdat_offset,
            )?;
        } else if &fourcc == b"mdat" {
            mdat_offset.get_or_insert(pos);
        }
        pos += size;
    }

    if let (Some(moov_off), Some(mdat_off)) = (moov_offset, mdat_offset) {
        report.faststart = moov_off < mdat_off;
    }
    Ok(report)
}

const MAX_ATOM_DEPTH: usize = 32;

fn walk_atoms(
    slice: &[u8],
    base_offset: u64,
    depth: usize,
    report: &mut Mp4AtomReport,
    moov_offset: &mut Option<u64>,
    mdat_offset: &mut Option<u64>,
) -> Result<(), MediaError> {
    if depth > MAX_ATOM_DEPTH {
        return Err(MediaError::InvalidContainer(
            "atom nesting limit exceeded".into(),
        ));
    }
    let mut pos = 0usize;
    while pos + 8 <= slice.len() {
        let size32 = usize::try_from(read_u32_be(slice, pos)?)
            .map_err(|_| MediaError::InvalidContainer("atom size exceeds usize".to_owned()))?;
        let cc = [
            slice[pos + 4],
            slice[pos + 5],
            slice[pos + 6],
            slice[pos + 7],
        ];
        let (box_size, hdr_len) = if size32 == 1 {
            if pos + 16 > slice.len() {
                return Err(MediaError::InvalidContainer(
                    "truncated 64-bit atom header".to_owned(),
                ));
            }
            let s64 = usize::try_from(read_u64_be(slice, pos + 8)?).map_err(|_| {
                MediaError::InvalidContainer("64-bit atom size exceeds usize".to_owned())
            })?;
            (s64, 16usize)
        } else if size32 == 0 {
            (slice.len() - pos, 8usize)
        } else {
            (size32, 8usize)
        };
        if box_size < hdr_len
            || pos
                .checked_add(box_size)
                .is_none_or(|end| end > slice.len())
        {
            return Err(MediaError::InvalidContainer(
                "atom extends beyond container bounds".to_owned(),
            ));
        }

        let abs_offset = base_offset.saturating_add(u64::try_from(pos).unwrap_or(0));
        report.boxes.push(Mp4BoxInfo {
            fourcc: cc,
            depth,
            offset: abs_offset,
            size: u64::try_from(box_size).unwrap_or(0),
        });

        match &cc {
            b"moov" if depth == 0 && moov_offset.is_none() => *moov_offset = Some(abs_offset),
            b"mdat" if depth == 0 && mdat_offset.is_none() => *mdat_offset = Some(abs_offset),
            b"edts" => report.edts_count += 1,
            b"elst" => report.elst_count += 1,
            b"avc1" => report.avc1_count += 1,
            b"avc3" => report.avc3_count += 1,
            _ => {}
        }

        let body = &slice[pos + hdr_len..pos + box_size];
        let body_base = abs_offset.saturating_add(u64::try_from(hdr_len).unwrap_or(0));
        walk_atom_children(
            cc,
            body,
            body_base,
            depth + 1,
            report,
            moov_offset,
            mdat_offset,
        )?;

        pos += box_size;
    }
    Ok(())
}

fn walk_atom_children(
    cc: [u8; 4],
    body: &[u8],
    body_base: u64,
    child_depth: usize,
    report: &mut Mp4AtomReport,
    moov_offset: &mut Option<u64>,
    mdat_offset: &mut Option<u64>,
) -> Result<(), MediaError> {
    let skip = match &cc {
        b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"dinf" | b"edts" => Some(0usize),
        b"stsd" if body.len() > 8 => {
            let entry_count = read_u32_be(body, 4)?;
            if report.stsd_entry_count == 0 {
                report.stsd_entry_count = entry_count;
            }
            Some(8)
        }
        b"avc1" | b"avc3" if body.len() > VISUAL_SAMPLE_ENTRY_HEADER_LEN => {
            Some(VISUAL_SAMPLE_ENTRY_HEADER_LEN)
        }
        b"mp4a" if body.len() > AUDIO_SAMPLE_ENTRY_HEADER_LEN => {
            let hdr_len = if read_u16_be(body, 8)? == 1 {
                AUDIO_SAMPLE_ENTRY_V1_HEADER_LEN
            } else {
                AUDIO_SAMPLE_ENTRY_HEADER_LEN
            };
            (body.len() > hdr_len).then_some(hdr_len)
        }
        _ => None,
    };
    if let Some(offset) = skip {
        walk_atoms(
            &body[offset..],
            body_base.saturating_add(u64::try_from(offset).unwrap_or(0)),
            child_depth,
            report,
            moov_offset,
            mdat_offset,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod atom_io_tests {
    use super::*;
    use std::io;
    #[test]
    fn sparse_large_mdat_reads_only_headers() {
        use std::io::Write as _;
        struct CountingFile {
            file: File,
            bytes: usize,
        }
        impl Read for CountingFile {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let count = self.file.read(buf)?;
                self.bytes += count;
                Ok(count)
            }
        }
        impl Seek for CountingFile {
            fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
                self.file.seek(pos)
            }
        }
        let mut f = tempfile::tempfile().unwrap();
        let size = (1u64 << 32) + 1024;
        f.write_all(b"\x00\x00\x00\x08moov\x00\x00\x00\x01mdat")
            .unwrap();
        f.write_all(&(size - 8).to_be_bytes()).unwrap();
        f.set_len(size).unwrap();
        let mut reader = CountingFile { file: f, bytes: 0 };
        let report = inspect_atoms(&mut reader, size).unwrap();
        assert!(report.faststart);
        assert_eq!(reader.bytes, 24);
    }

    #[test]
    fn trim_preserves_inband_param_sets_across_multi_config_segments() {
        use crate::codec::{
            DecodedFrame, EncodeParams, OpenH264Backend, PixelFormat, VideoCodecBackend as _,
        };
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("multi.mp4");
        let trimmed = tmp.path().join("trimmed.mp4");
        let codec = OpenH264Backend::new();
        let make_frames = |w: u32, h: u32, y: u8, count: usize| {
            (0..count)
                .map(|i| DecodedFrame {
                    width: w,
                    height: h,
                    frame_index: u64::try_from(i).unwrap(),
                    format: PixelFormat::Yuv420p,
                    data: vec![y; usize::try_from(w * h * 3 / 2).unwrap()],
                })
                .collect::<Vec<_>>()
        };
        let mut packets = codec
            .encode_yuv420p_sequence(
                &make_frames(64, 96, 40, 2),
                5,
                40_000_000,
                EncodeParams::default(),
            )
            .unwrap();
        let mut second = codec
            .encode_yuv420p_sequence(
                &make_frames(96, 64, 180, 2),
                5,
                40_000_000,
                EncodeParams::default(),
            )
            .unwrap();
        for (idx, pkt) in second.iter_mut().enumerate() {
            pkt.pts_ns = (2 + i64::try_from(idx).unwrap()) * 40_000_000;
        }
        packets.extend(second);
        mux_raw_h264_to_mp4(&src, 96, 96, 90_000, 3600, &packets).unwrap();

        crate::trim::trim_video(&src, &trimmed, 0, 3, &codec).unwrap();
        let track = Mp4VideoTrack::open(&trimmed).unwrap();
        let f0 = codec.decode_frame(&track, 0, PixelFormat::Yuv420p).unwrap();
        assert_eq!((f0.width, f0.height), (64, 96));
        assert!(f0.data[0].abs_diff(40) <= 4, "frame 0 luma {}", f0.data[0]);
        let f2 = codec.decode_frame(&track, 2, PixelFormat::Yuv420p).unwrap();
        assert_eq!((f2.width, f2.height), (96, 64));
        assert!(f2.data[0].abs_diff(180) <= 4, "frame 2 luma {}", f2.data[0]);
    }
}
