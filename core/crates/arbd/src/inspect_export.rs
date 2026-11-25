//! Standalone T-2B RPC operations running outside the live SHM viewport mutex:
//! - `media.decode_audio_pcm`: pure-Rust AAC-LC -> cached s16le RIFF/WAV decode
//! - `export.share_mp4`: full or mid-GOP trimmed single-file faststart zero-`elst` MP4 export
//! - `timeline.overview`: one-shot structured AI triage bundle summary
//! - `media.inspect_frames`: contact sheet / single frame / tap-crop PNG renderer

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use arb_core::analytics::LogPackageTimeline;
use arb_core::bundle::{FrameRecord, LogRecord, RecordBucket, StreamStatus, TapRecord};
use arb_core::time::TimeAlignment;
use arb_media::MediaError;
use arb_media::audio_decode::{decode_audio_m4a_to_wav, inspect_wav_header};
use arb_media::burn_in::{BurnInDisplayGeometry, BurnInTouchEvent, burn_in_taps_video};
use arb_media::codec::PixelFormat as MediaPixelFormat;
use arb_media::contact_sheet::{
    ContactSheetOptions, ContactSheetTile, MAX_CONTACT_SHEET_TILES, TapCropOptions,
    render_contact_sheet_png, render_single_frame_png, render_tap_crop_png,
    select_contact_sheet_indices,
};
use arb_media::mp4::Mp4VideoTrack;
use arb_media::mux_av::mux_share_mp4;
use arb_media::playback::GopLruCache;
use arb_storage::bundle_io::{BundleReader, TimeRange, open_bundle};
use arb_storage::catalog::{BundleEntry, Catalog};
use arb_storage::sha256_hex;

use crate::error::DaemonError;
use crate::media_engine::{
    ExportContext, acquire_export_lease, compute_epoch_frame_ranges, export_trim,
};
use crate::protocol::{
    DEFAULT_OVERVIEW_ERROR_LOGS, ErrorLogHighlight, ExportShareMp4Params, ExportShareMp4Response,
    ExportTrimParams, InspectFramesMode, MAX_OVERVIEW_ERROR_LOGS, MediaDecodeAudioPcmResponse,
    MediaInspectFramesParams, MediaInspectFramesResponse, OverviewTapItem, TapCropMeta,
    TimelineOverviewResponse, TimelineQueryParams,
};
use crate::time_util::next_id;

/// Maximum cached WAV files retained under `<cache_dir>/audio-pcm/`.
/// Cache entries younger than this are never evicted by [`prune_cache_dir`].
const CACHE_PRUNE_MIN_AGE: Duration = Duration::from_secs(120);
const AUDIO_PCM_CACHE_MAX_FILES: usize = 32;

/// Maximum aggregate bytes retained under `<cache_dir>/audio-pcm/` (`512 MiB`).
const AUDIO_PCM_CACHE_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// Maximum cached inspect PNG files retained under `<cache_dir>/inspect/`.
const INSPECT_CACHE_MAX_FILES: usize = 64;

/// Maximum aggregate bytes retained under `<cache_dir>/inspect/` (`128 MiB`).
const INSPECT_CACHE_MAX_BYTES: u64 = 128 * 1024 * 1024;

/// Maximum raw PNG size (2 MiB -> ~2.67 MiB base64) inlined in JSON-RPC responses so responses
/// stay comfortably within `DEFAULT_MAX_IPC_LINE_BYTES` (4 MiB).
const MAX_INLINE_PNG_BYTES: usize = 2 * 1024 * 1024;

/// Maximum number of taps returned in `timeline.overview` to keep AI context bounded.
const MAX_OVERVIEW_TAPS: usize = 500;

/// Maximum character length of one error log highlight excerpt in `timeline.overview`.
const MAX_ERROR_LOG_MESSAGE_CHARS: usize = 512;

/// Local GOP-LRU budget for one `media.inspect_frames` call.
const INSPECT_GOP_CACHE_GOPS: usize = 4;
const INSPECT_GOP_CACHE_BYTES: usize = 128 * 1024 * 1024;

/// Decodes a bundle's `audio.m4a` to a cached signed 16-bit LE PCM RIFF/WAV file under
/// `<cache_dir>/audio-pcm/<source_sha256>.wav`.
///
/// Returns `{ available: false, ... }` cleanly when the bundle has no audio track.
///
/// # Errors
///
/// Returns [`DaemonError`] if the bundle cannot be opened or AAC-LC decoding fails.
pub fn decode_audio_pcm(
    catalog: &Catalog,
    cache_dir: &Path,
    bundle_dir: &Path,
    source_catalog_id: Option<&str>,
) -> Result<MediaDecodeAudioPcmResponse, DaemonError> {
    let _lease = acquire_export_lease(catalog, source_catalog_id, std::process::id())?;
    let reader = open_bundle(bundle_dir)?;
    let summary = reader.manifest_summary();
    let audio_meta = summary.streams.audio.as_ref();
    let av_offset_ms = audio_meta.map_or(0, |a| a.av_offset_ms);
    let audio_lead_in_offset_ns = audio_meta.map_or(0, |a| a.audio_lead_in_offset_ns);

    let Some(audio_m4a) = reader.audio_path() else {
        return Ok(MediaDecodeAudioPcmResponse {
            available: false,
            wav_path: None,
            sample_rate_hz: 0,
            channels: 0,
            duration_ns: 0,
            av_offset_ms: 0,
            audio_lead_in_offset_ns: 0,
            cached: false,
        });
    };

    let cache_root = cache_dir.join("audio-pcm");
    fs::create_dir_all(&cache_root)?;
    let (_, safe_key) = arb_storage::bundle_io::stream_hash_file(&audio_m4a)?;
    let cache_wav = cache_root.join(format!("{safe_key}.wav"));

    // Fast path: reuse existing validated cached WAV.
    if cache_wav.is_file() {
        if let Ok(hdr) = inspect_wav_header(&cache_wav) {
            // Refresh recency so a just-served path is not the next eviction candidate.
            if let Ok(f) = OpenOptions::new().append(true).open(&cache_wav) {
                let _ = f.set_modified(SystemTime::now());
            }
            return Ok(MediaDecodeAudioPcmResponse {
                available: true,
                wav_path: Some(cache_wav),
                sample_rate_hz: hdr.sample_rate_hz,
                channels: hdr.channels,
                duration_ns: hdr.duration_ns,
                av_offset_ms,
                audio_lead_in_offset_ns,
                cached: true,
            });
        }
        // Corrupt or truncated cache entry: remove before re-decoding.
        let _ = fs::remove_file(&cache_wav);
    }

    prune_cache_dir(
        &cache_root,
        AUDIO_PCM_CACHE_MAX_FILES.saturating_sub(1),
        AUDIO_PCM_CACHE_MAX_BYTES,
        Some(&cache_wav),
    );

    match decode_audio_m4a_to_wav(&audio_m4a, &cache_wav) {
        Ok(report) => Ok(MediaDecodeAudioPcmResponse {
            available: true,
            wav_path: Some(cache_wav),
            sample_rate_hz: report.sample_rate_hz,
            channels: report.channels,
            duration_ns: report.duration_ns,
            av_offset_ms,
            audio_lead_in_offset_ns,
            cached: false,
        }),
        Err(MediaError::Io(ref io_err)) if io_err.kind() == ErrorKind::AlreadyExists => {
            // Concurrent request finished writing the same cache file first.
            let hdr = inspect_wav_header(&cache_wav)?;
            Ok(MediaDecodeAudioPcmResponse {
                available: true,
                wav_path: Some(cache_wav),
                sample_rate_hz: hdr.sample_rate_hz,
                channels: hdr.channels,
                duration_ns: hdr.duration_ns,
                av_offset_ms,
                audio_lead_in_offset_ns,
                cached: true,
            })
        }
        Err(err) => Err(err.into()),
    }
}

/// Exports a single-file faststart zero-`elst` `share.mp4`, optionally trimming to
/// `[in_frame_index..=out_frame_index]` via a new WORM trimmed bundle first, and optionally
/// moving the source bundle to non-destructive Trash **strictly after** all outputs are
/// fsynced, verified, and committed.
///
/// # Errors
///
/// Returns [`DaemonError`] if parameters or frame bounds are invalid, `trash_original` is
/// requested on a read-only legacy or uncataloged bundle, trim/mux fails, or output path exists.
pub fn export_share_mp4(
    ctx: &ExportContext<'_>,
    source_bundle_dir: &Path,
    source_entry: Option<&BundleEntry>,
    dest_parent: &Path,
    share_mp4_path: &Path,
    params: &ExportShareMp4Params,
) -> Result<ExportShareMp4Response, DaemonError> {
    let source_catalog_id = source_entry.map(|e| e.bundle_id.as_str());
    let holder_pid = params.holder_pid.unwrap_or_else(std::process::id);
    let lease_guard = acquire_export_lease(ctx.catalog, source_catalog_id, holder_pid)?;

    let reader = open_bundle(source_bundle_dir)?;
    let summary = reader.manifest_summary();
    if params.trash_original {
        crate::media_engine::validate_trash_source(reader.as_ref(), source_entry)?;
    }

    let source_video = reader.video_path().ok_or_else(|| {
        DaemonError::NotFound(format!(
            "source bundle `{}` has no video.mp4 track",
            source_catalog_id.unwrap_or(&summary.bundle_id)
        ))
    })?;
    let src_track = Mp4VideoTrack::open(&source_video)?;
    let requested_trim = resolve_share_trim_range(
        params.in_frame_index,
        params.out_frame_index,
        src_track.frame_count(),
    )?;

    // Refuse an existing output before any trimmed WORM bundle is committed, so a doomed
    // export never leaves an orphan trimmed bundle in the catalog.
    if share_mp4_path.symlink_metadata().is_ok() {
        return Err(DaemonError::Conflict(format!(
            "share.mp4 output already exists: {}",
            share_mp4_path.display()
        )));
    }

    // Ensure destination parent exists only after validation passes.
    if let Some(parent) = share_mp4_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }

    let (av_mux_report, trimmed_bundle) = if let Some((first, last)) = requested_trim {
        let trim = export_trim(
            ctx,
            source_bundle_dir,
            source_catalog_id,
            dest_parent,
            &ExportTrimParams {
                bundle_id_or_path: params.bundle_id.clone(),
                in_frame_index: first,
                out_frame_index: last,
                dest_dir: params.dest_dir.clone(),
                holder_pid: params.holder_pid,
                burn_taps: params.burn_taps,
                include_share_mp4: true,
                share_output_path: Some(share_mp4_path.to_owned()),
                trash_original: false,
            },
        )?;
        let report = trim
            .av_mux_report
            .ok_or_else(|| DaemonError::Internal("共有動画の検証結果がありません".into()))?;
        (
            report,
            (Some(trim.bundle_id), Some(trim.bundle_path), trim.warning),
        )
    } else {
        let (video, _cleanup) = if params.burn_taps {
            burn_in_share_video_if_nonempty(ctx, reader.as_ref(), &source_video)?
        } else {
            (source_video, None)
        };
        let offset = summary
            .streams
            .audio
            .as_ref()
            .map_or(0, |a| a.audio_lead_in_offset_ns);
        let report = mux_share_mp4(
            &video,
            reader.audio_path().as_deref(),
            offset,
            share_mp4_path,
        )?;
        (report, (None, None, None))
    };
    let (trimmed_bundle_id, trimmed_bundle_path, commit_warning) = trimmed_bundle;

    // Release temporary export lease before logical trash (source files remain untouched on disk).
    drop(lease_guard);

    let (original_trashed, warning) = crate::media_engine::finish_export(
        ctx.catalog,
        source_catalog_id.filter(|_| params.trash_original),
        commit_warning,
    );

    Ok(ExportShareMp4Response {
        share_mp4_path: share_mp4_path.to_path_buf(),
        trimmed_bundle_id,
        trimmed_bundle_path,
        audio_included: av_mux_report.audio_included,
        video_frame_count: av_mux_report.video_frame_count,
        edts_box_count: av_mux_report.edts_box_count,
        elst_box_count: av_mux_report.elst_box_count,
        output_bytes: av_mux_report.output_bytes,
        original_trashed,
        av_mux_report,
        warning,
    })
}

use crate::capture::TempFileGuard as TempFileCleanup;

fn resolve_share_trim_range(
    in_frame_index: Option<u64>,
    out_frame_index: Option<u64>,
    src_frame_count: u64,
) -> Result<Option<(u64, u64)>, DaemonError> {
    if src_frame_count == 0 {
        return Err(DaemonError::InvalidParams(
            "source bundle video track contains 0 frames".to_owned(),
        ));
    }
    match (in_frame_index, out_frame_index) {
        (Some(in_f), Some(out_f)) => {
            if in_f > out_f || out_f >= src_frame_count {
                return Err(MediaError::InvalidFrameRange {
                    in_frame_index: in_f,
                    out_frame_index: out_f,
                    frame_count: src_frame_count,
                }
                .into());
            }
            Ok((in_f > 0 || out_f + 1 < src_frame_count).then_some((in_f, out_f)))
        }
        (None, None) => Ok(None),
        _ => Err(DaemonError::InvalidParams(
            "開始と終了のフレーム番号を両方指定してください".into(),
        )),
    }
}

fn burn_in_share_video_if_nonempty(
    ctx: &ExportContext<'_>,
    bundle_reader: &dyn BundleReader,
    video_path: &Path,
) -> Result<(PathBuf, Option<TempFileCleanup>), DaemonError> {
    let frames = bundle_reader.frames()?;
    if frames.is_empty() {
        return Ok((video_path.to_path_buf(), None));
    }
    let taps = bundle_reader.query_taps(Some(RecordBucket::Main), TimeRange::all(), None)?;
    let events: Vec<BurnInTouchEvent> = taps
        .iter()
        .filter_map(BurnInTouchEvent::from_tap_record)
        .collect();
    if events.is_empty() {
        return Ok((video_path.to_path_buf(), None));
    }
    let frame_ts: Vec<i64> = frames
        .iter()
        .map(|f| frame_boottime_ns(f).unwrap_or(f.pts_device_mono_ns))
        .collect();
    let summary = bundle_reader.manifest_summary();
    let geom = BurnInDisplayGeometry {
        display_width: summary.streams.video.as_ref().and_then(|v| v.display_width),
        display_height: summary
            .streams
            .video
            .as_ref()
            .and_then(|v| v.display_height),
    };
    let tmp_dir = ctx.data_dir.join("tmp");
    fs::create_dir_all(&tmp_dir)?;
    let tmp_burned = tmp_dir.join(format!("{}-burned.mp4", next_id("share")));
    let last_frame = u64::try_from(frames.len().saturating_sub(1)).unwrap_or(0);
    let cleanup = TempFileCleanup(tmp_burned.clone());
    let _report = burn_in_taps_video(
        video_path,
        &tmp_burned,
        0,
        last_frame,
        &frame_ts,
        &events,
        geom,
        ctx.backend,
    )?;
    Ok((tmp_burned, Some(cleanup)))
}

/// Builds a compact one-shot [`TimelineOverviewResponse`] for AI triage.
///
/// # Errors
///
/// Returns [`DaemonError`] if the bundle or its timeline tables cannot be read.
pub fn build_timeline_overview(
    bundle_dir: &Path,
    catalog_bundle_id: Option<&str>,
    max_error_logs: Option<usize>,
) -> Result<TimelineOverviewResponse, DaemonError> {
    let reader = open_bundle(bundle_dir)?;
    let summary = reader.manifest_summary();
    let bundle_id = catalog_bundle_id.unwrap_or(&summary.bundle_id).to_owned();

    let frames = reader.frames()?;
    let frame_count = frames.len();
    let (width, height, video_duration_ns) = match reader.video_path() {
        Some(video_path) => match Mp4VideoTrack::open(&video_path) {
            Ok(track) => {
                let ts = u128::from(track.timescale().max(1));
                let dur_ns = u64::try_from(
                    u128::from(track.duration_ticks()).saturating_mul(1_000_000_000) / ts,
                )
                .unwrap_or(u64::MAX);
                (
                    Some(u32::from(track.width())),
                    Some(u32::from(track.height())),
                    Some(dur_ns),
                )
            }
            Err(_) => (None, None, None),
        },
        None => (None, None, None),
    };

    let base_boottime_ns = first_bundle_boottime_ns(&frames, &summary.epochs);
    let duration_ms = compute_bundle_duration_ms(video_duration_ns, &frames, &summary.epochs);
    let fps_estimate = compute_fps_estimate(frame_count, duration_ms);

    let screens = reader.screens()?;
    let crashes = reader.crashes()?;
    let discontinuities = reader.discontinuities()?;

    let main_taps = reader.query_taps(Some(RecordBucket::Main), TimeRange::all(), None)?;
    let taps = build_overview_taps(&main_taps, &frames, base_boottime_ns);

    let main_logs = reader.query_logs(Some(RecordBucket::Main), TimeRange::all(), None)?;
    let max_errors = max_error_logs
        .unwrap_or(DEFAULT_OVERVIEW_ERROR_LOGS)
        .min(MAX_OVERVIEW_ERROR_LOGS);
    let error_log_highlights =
        build_error_log_highlights(&main_logs, &frames, base_boottime_ns, max_errors);

    let audio_status = summary
        .streams
        .audio
        .as_ref()
        .map_or(StreamStatus::Absent, |a| a.status);
    let av_offset_ms = summary.streams.audio.as_ref().map_or(0, |a| a.av_offset_ms);
    let logcat_completeness = summary.streams.logcat.as_ref().map(|l| l.completeness);
    let display_width = summary.streams.video.as_ref().and_then(|v| v.display_width);
    let display_height = summary
        .streams
        .video
        .as_ref()
        .and_then(|v| v.display_height);
    let epoch_frame_ranges = compute_epoch_frame_ranges(&frames, &summary.epochs);

    Ok(TimelineOverviewResponse {
        bundle_id,
        kind: summary.bundle_kind,
        legacy_read_only: reader.is_legacy(),
        frame_count,
        duration_ms,
        width,
        height,
        display_width,
        display_height,
        epoch_frame_ranges,
        fps_estimate,
        audio_status,
        av_offset_ms,
        logcat_completeness,
        screens,
        taps,
        crashes,
        discontinuities,
        error_log_highlights,
        unanchored_count: summary.unanchored_summary.included_count,
        unanchored_truncated_count: summary.unanchored_summary.truncated_count,
        app_logs: summary.app_logs.clone(),
    })
}

/// Renders a contact sheet, single frame, or tap-crop PNG for AI visual inspection, caches it
/// atomically under `<cache_dir>/inspect/`, and returns metadata + inline base64 PNG (when small).
///
/// # Errors
///
/// Returns [`DaemonError`] if the bundle has no video, requested indices or tap coordinates are
/// out of range, or decoding / PNG rendering fails.
pub fn inspect_frames(
    ctx: &ExportContext<'_>,
    cache_dir: &Path,
    bundle_dir: &Path,
    source_catalog_id: Option<&str>,
    params: &MediaInspectFramesParams,
) -> Result<MediaInspectFramesResponse, DaemonError> {
    let _lease = acquire_export_lease(ctx.catalog, source_catalog_id, std::process::id())?;
    let reader = open_bundle(bundle_dir)?;
    let summary = reader.manifest_summary();
    let bundle_id = source_catalog_id.unwrap_or(&summary.bundle_id).to_owned();

    let video_path = reader.video_path().ok_or_else(|| {
        DaemonError::NotFound(format!("bundle `{bundle_id}` has no video.mp4 track"))
    })?;
    let track = Mp4VideoTrack::open(&video_path)?;
    let frame_count = track.frame_count();
    if frame_count == 0 {
        return Err(DaemonError::InvalidParams(format!(
            "bundle `{bundle_id}` video track contains 0 frames"
        )));
    }

    let video_w = u32::from(track.width());
    let video_h = u32::from(track.height());
    let effective_display_w = params
        .display_width
        .or_else(|| summary.streams.video.as_ref().and_then(|v| v.display_width));
    let effective_display_h = params.display_height.or_else(|| {
        summary
            .streams
            .video
            .as_ref()
            .and_then(|v| v.display_height)
    });
    let display_dims = (effective_display_w, effective_display_h);
    let mut gop_cache = GopLruCache::new(INSPECT_GOP_CACHE_GOPS, INSPECT_GOP_CACHE_BYTES);

    let (png_bytes, frame_indices_rendered, tap_crop) = match params.mode {
        InspectFramesMode::ContactSheet => render_inspect_contact_sheet(
            reader.as_ref(),
            &track,
            &mut gop_cache,
            ctx.backend,
            params,
            video_w,
            video_h,
            display_dims,
        )?,
        InspectFramesMode::Single => render_inspect_single(
            reader.as_ref(),
            &track,
            &mut gop_cache,
            ctx.backend,
            params,
            video_w,
            video_h,
            display_dims,
        )?,
        InspectFramesMode::TapCrop => render_inspect_tap_crop(
            reader.as_ref(),
            &track,
            &mut gop_cache,
            ctx.backend,
            params,
            video_w,
            video_h,
            display_dims,
        )?,
    };

    let (png_w, png_h) = parse_png_dimensions(&png_bytes)?;
    let inspect_dir = cache_dir.join("inspect");
    fs::create_dir_all(&inspect_dir)?;
    let safe_key = sanitize_cache_key(&bundle_id);
    let png_path = inspect_dir.join(format!("{safe_key}-{}.png", next_id("inspect")));
    write_bytes_atomic(&png_path, &png_bytes)?;
    prune_cache_dir(
        &inspect_dir,
        INSPECT_CACHE_MAX_FILES,
        INSPECT_CACHE_MAX_BYTES,
        Some(&png_path),
    );

    let png_base64 = if png_bytes.len() <= MAX_INLINE_PNG_BYTES {
        Some(base64_encode(&png_bytes))
    } else {
        None
    };

    Ok(MediaInspectFramesResponse {
        mode: params.mode,
        bundle_id,
        frame_indices_rendered,
        width: png_w,
        height: png_h,
        png_bytes_len: png_bytes.len(),
        png_path,
        png_base64,
        tap_crop,
    })
}

/// Resolves optional `start_frame` / `end_frame` bounds on [`TimelineQueryParams`] into a
/// [`TimeRange`] and filters `logs` by optional `package`, `tag`, `level`, and `keyword` criteria.
///
/// Maximum number of bundle-scoped PID-to-package resolvers retained in memory.
const MAX_LOG_RESOLVER_CACHE_ENTRIES: usize = 32;

/// Minimum and maximum batch sizes for cursor-chunked filtered log scans.
const MIN_FILTERED_LOG_BATCH_ROWS: usize = 1_024;
const MAX_FILTERED_LOG_BATCH_ROWS: usize = 16_384;

/// Structured page result from [`query_logs_filtered_page`], exposing pagination truncation
/// metadata (`has_more`, `next_after_seq`) and scan/cache instrumentation.
///
/// Wave 2 (H-4) can wire `has_more` and `next_after_seq` into `protocol::TimelineLogsResponse`
/// alongside IDE UI pagination while keeping the current `protocol.rs` wire schema untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilteredLogPage {
    /// Filtered log rows for the requested page (at most `limit`).
    pub logs: Vec<LogRecord>,
    /// `true` when at least one additional matching row exists after this page.
    pub has_more: bool,
    /// Exclusive sequence cursor (`last.seq`) to pass as `after_seq` for the next page.
    pub next_after_seq: Option<u64>,
    /// Number of candidate rows read from `reader` while scanning for this page.
    pub candidate_rows_scanned: usize,
    /// `true` when a `package` filter was satisfied from the bundle-scoped PID resolver cache
    /// without re-scanning all bundle logs.
    pub resolver_cache_hit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    path: PathBuf,
    size: u64,
    mtime: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BundleLogCacheKey {
    LegacySnapshot(String),
    Timeline {
        digest: String,
        files: [FileStamp; 2],
    },
}

#[derive(Default)]
struct PidResolverLruCache {
    entries: VecDeque<(BundleLogCacheKey, Arc<LogPackageTimeline>)>,
}

impl PidResolverLruCache {
    fn get(&mut self, key: &BundleLogCacheKey) -> Option<Arc<LogPackageTimeline>> {
        let pos = self.entries.iter().position(|(k, _)| k == key)?;
        let item = self.entries.remove(pos)?;
        let value = Arc::clone(&item.1);
        self.entries.push_back(item);
        Some(value)
    }

    fn insert(&mut self, key: BundleLogCacheKey, value: Arc<LogPackageTimeline>) {
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == &key) {
            let _ = self.entries.remove(pos);
        }
        while self.entries.len() >= MAX_LOG_RESOLVER_CACHE_ENTRIES {
            let _ = self.entries.pop_front();
        }
        self.entries.push_back((key, value));
    }
}

fn pid_resolver_cache() -> &'static Mutex<PidResolverLruCache> {
    static CACHE: OnceLock<Mutex<PidResolverLruCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(PidResolverLruCache::default()))
}

fn build_bundle_cache_key(reader: &dyn BundleReader) -> Option<BundleLogCacheKey> {
    if let Some(digest) = reader.legacy_log_digest() {
        return Some(BundleLogCacheKey::LegacySnapshot(digest.to_owned()));
    }
    let dir = fs::canonicalize(reader.timeline_bundle_dir()?).ok()?;
    let stamp = |name: &str| {
        let path = dir.join(name);
        let meta = fs::metadata(&path).ok()?;
        Some(FileStamp {
            path,
            size: meta.len(),
            mtime: meta.modified().ok()?,
        })
    };
    let digest = reader
        .manifest_summary()
        .manifest_v2
        .as_ref()?
        .files
        .iter()
        .find(|f| f.path == "timeline.sqlite")?
        .sha256
        .clone();
    Some(BundleLogCacheKey::Timeline {
        digest,
        files: [stamp("timeline.sqlite")?, stamp("manifest.json")?],
    })
}

fn load_or_build_package_resolver(
    reader: &dyn BundleReader,
) -> Result<(Arc<LogPackageTimeline>, bool), DaemonError> {
    let cache_key = build_bundle_cache_key(reader);
    if let Some(ref key) = cache_key
        && let Ok(mut guard) = pid_resolver_cache().lock()
        && let Some(hit) = guard.get(key)
    {
        return Ok((hit, true));
    }
    let all_logs = reader.query_logs(None, TimeRange::all(), None)?;
    let resolver = Arc::new(LogPackageTimeline::from_logs(&all_logs));
    if let Some(key) = cache_key
        && build_bundle_cache_key(reader).as_ref() == Some(&key)
        && let Ok(mut guard) = pid_resolver_cache().lock()
    {
        guard.insert(key, Arc::clone(&resolver));
    }
    Ok((resolver, false))
}

struct CompiledLogFilter<'a> {
    package: Option<(&'a str, Arc<LogPackageTimeline>)>,
    level: Option<String>,
    tag_lower: Option<String>,
    keyword_lower: Option<String>,
}

impl CompiledLogFilter<'_> {
    fn matches(&self, log: &LogRecord) -> bool {
        if let Some((target_pkg, ref resolver)) = self.package
            && !resolver.matches_package(log, target_pkg)
        {
            return false;
        }
        if let Some(ref want_level) = self.level {
            let actual = log.level.as_deref().map_or("", normalize_log_level);
            if actual != want_level {
                return false;
            }
        }
        if let Some(ref want_tag) = self.tag_lower {
            let matched = log
                .tag
                .as_deref()
                .is_some_and(|t| t.to_ascii_lowercase().contains(want_tag));
            if !matched {
                return false;
            }
        }
        if let Some(ref want_kw) = self.keyword_lower {
            let in_msg = log.message.to_ascii_lowercase().contains(want_kw);
            let in_tag = log
                .tag
                .as_deref()
                .is_some_and(|t| t.to_ascii_lowercase().contains(want_kw));
            if !in_msg && !in_tag {
                return false;
            }
        }
        true
    }
}

fn resolve_log_query_time_range(
    reader: &dyn BundleReader,
    params: &TimelineQueryParams,
) -> Result<TimeRange, DaemonError> {
    let mut range = TimeRange {
        start_ns: params.start_ns,
        end_ns: params.end_ns,
    };
    if params.start_frame.is_none() && params.end_frame.is_none() {
        return Ok(range);
    }
    let frames = reader.frames()?;
    if frames.is_empty() {
        return Err(DaemonError::InvalidParams(
            "cannot filter by start_frame/end_frame on a bundle with 0 video frames".to_owned(),
        ));
    }
    let max_frame = u64::try_from(frames.len().saturating_sub(1)).unwrap_or(0);
    let sf = params.start_frame.unwrap_or(0);
    let ef = params.end_frame.unwrap_or(max_frame);
    if sf > ef || ef > max_frame {
        return Err(DaemonError::InvalidParams(format!(
            "invalid frame filter [{sf}..={ef}] for bundle with {} frames",
            frames.len()
        )));
    }
    let start_bt = frames
        .iter()
        .find(|f| f.frame_index == sf)
        .and_then(frame_boottime_ns);
    let end_bt = frames
        .iter()
        .find(|f| f.frame_index == ef)
        .and_then(frame_boottime_ns);
    if params.start_frame.is_some()
        && let Some(s) = start_bt
    {
        range.start_ns = Some(range.start_ns.map_or(s, |prev| prev.max(s)));
    }
    if params.end_frame.is_some()
        && let Some(e) = end_bt
    {
        range.end_ns = Some(range.end_ns.map_or(e, |prev| prev.min(e)));
    }
    Ok(range)
}

/// Resolves optional `start_frame` / `end_frame` bounds on [`TimelineQueryParams`] into a
/// [`TimeRange`], filters `logs` by optional `package`, `tag`, `level`, and `keyword` criteria
/// using a bundle-scoped cached PID resolver and cursor-chunked reads, and returns a
/// [`FilteredLogPage`] with truncation metadata (`has_more`, `next_after_seq`).
///
/// # Errors
///
/// Returns [`DaemonError::InvalidParams`] if `start_frame > end_frame`, a requested frame
/// index exceeds the bundle's frame count, or `level` is unrecognized.
pub fn query_logs_filtered_page(
    reader: &dyn BundleReader,
    bucket: Option<RecordBucket>,
    params: &TimelineQueryParams,
    limit: usize,
) -> Result<FilteredLogPage, DaemonError> {
    let range = resolve_log_query_time_range(reader, params)?;

    let package_filter = params
        .package
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let has_text_filter = params.tag.is_some()
        || params.level.is_some()
        || params.keyword.is_some()
        || package_filter.is_some();

    if !has_text_filter {
        let fetch = limit.saturating_add(1);
        let mut logs = reader.query_logs_after(bucket, range, params.after_seq, Some(fetch))?;
        let candidate_rows_scanned = logs.len();
        let has_more = logs.len() > limit;
        logs.truncate(limit);
        let next_after_seq = logs.last().map(|r| r.seq);
        return Ok(FilteredLogPage {
            logs,
            has_more,
            next_after_seq,
            candidate_rows_scanned,
            resolver_cache_hit: false,
        });
    }

    let level = match params.level.as_deref() {
        Some(raw) => {
            let norm = normalize_log_level(raw);
            if norm.is_empty() {
                return Err(DaemonError::InvalidParams(format!(
                    "unknown log level `{raw}`; expected one of V, D, I, W, E, F"
                )));
            }
            Some(norm.to_owned())
        }
        None => None,
    };

    let (package, resolver_cache_hit) = if let Some(target_pkg) = package_filter {
        let (resolver, hit) = load_or_build_package_resolver(reader)?;
        (Some((target_pkg, resolver)), hit)
    } else {
        (None, false)
    };

    let filter = CompiledLogFilter {
        package,
        level,
        tag_lower: params.tag.as_deref().map(str::to_ascii_lowercase),
        keyword_lower: params.keyword.as_deref().map(str::to_ascii_lowercase),
    };

    let batch_size = limit
        .saturating_mul(4)
        .clamp(MIN_FILTERED_LOG_BATCH_ROWS, MAX_FILTERED_LOG_BATCH_ROWS);
    let mut cursor_seq = params.after_seq;
    let mut matched = Vec::with_capacity(limit.min(MAX_FILTERED_LOG_BATCH_ROWS));
    let mut has_more = false;
    let mut candidate_rows_scanned = 0_usize;

    loop {
        let batch = reader.query_logs_after(bucket, range, cursor_seq, Some(batch_size))?;
        let batch_len = batch.len();
        candidate_rows_scanned = candidate_rows_scanned.saturating_add(batch_len);
        if batch.is_empty() {
            break;
        }
        for log in batch {
            cursor_seq = Some(log.seq);
            if filter.matches(&log) {
                if matched.len() < limit {
                    matched.push(log);
                } else {
                    has_more = true;
                    break;
                }
            }
        }
        if has_more || batch_len < batch_size {
            break;
        }
    }

    let next_after_seq = matched.last().map(|r| r.seq);
    Ok(FilteredLogPage {
        logs: matched,
        has_more,
        next_after_seq,
        candidate_rows_scanned,
        resolver_cache_hit,
    })
}

/// Resolves optional `start_frame` / `end_frame` bounds on [`TimelineQueryParams`] into a
/// [`TimeRange`] and filters `logs` by optional `package`, `tag`, `level`, and `keyword` criteria.
///
/// # Errors
///
/// Returns [`DaemonError::InvalidParams`] if `start_frame > end_frame` or a requested frame
/// index exceeds the bundle's frame count.
pub fn query_logs_filtered(
    reader: &dyn BundleReader,
    bucket: Option<RecordBucket>,
    params: &TimelineQueryParams,
    limit: usize,
) -> Result<Vec<LogRecord>, DaemonError> {
    query_logs_filtered_page(reader, bucket, params, limit).map(|page| page.logs)
}

// ---------------- Helpers ----------------

type InspectRenderOutput = (Vec<u8>, Vec<u64>, Option<TapCropMeta>);

#[expect(
    clippy::too_many_arguments,
    reason = "internal helper threading decoded track, cache, backend, and display dimensions"
)]
fn render_inspect_contact_sheet(
    reader: &dyn BundleReader,
    track: &Mp4VideoTrack,
    gop_cache: &mut GopLruCache,
    backend: &dyn arb_media::codec::VideoCodecBackend,
    params: &MediaInspectFramesParams,
    video_w: u32,
    video_h: u32,
    display_dims: (Option<u32>, Option<u32>),
) -> Result<InspectRenderOutput, DaemonError> {
    let frame_count = track.frame_count();
    let indices = if let Some(explicit) = &params.frame_indices {
        if explicit.is_empty() || explicit.len() > MAX_CONTACT_SHEET_TILES {
            return Err(DaemonError::InvalidParams(format!(
                "frame_indices must contain 1..={MAX_CONTACT_SHEET_TILES} indices, got {}",
                explicit.len()
            )));
        }
        for &idx in explicit {
            if idx >= frame_count {
                return Err(MediaError::FrameOutOfRange {
                    frame_index: idx,
                    frame_count,
                }
                .into());
            }
        }
        explicit.clone()
    } else {
        let start = params.start_frame.unwrap_or(0);
        let end = params.end_frame.unwrap_or(frame_count - 1);
        if start > end || end >= frame_count {
            return Err(MediaError::InvalidFrameRange {
                in_frame_index: start,
                out_frame_index: end,
                frame_count,
            }
            .into());
        }
        let max_tiles = params
            .max_tiles
            .unwrap_or(MAX_CONTACT_SHEET_TILES)
            .clamp(1, MAX_CONTACT_SHEET_TILES);
        select_contact_sheet_indices(start, end, max_tiles)
    };

    let tap_map = if params.include_tap_marker {
        build_frame_tap_map(reader, video_w, video_h, display_dims)?
    } else {
        BTreeMap::new()
    };

    let mut tiles = Vec::with_capacity(indices.len());
    for &idx in &indices {
        let info = track.frame(idx)?;
        let (decoded, _) = gop_cache.fetch_frame(track, idx, MediaPixelFormat::Rgba8, backend)?;
        let tap_point = tap_map.get(&idx).copied();
        tiles.push(ContactSheetTile {
            frame_index: idx,
            timestamp_ms: info.pts_ns / 1_000_000,
            width: decoded.width,
            height: decoded.height,
            rgba: decoded.data.clone(),
            tap_point,
        });
    }

    let mut opts = ContactSheetOptions::default();
    if let Some(mw) = params.max_tile_width {
        opts.max_tile_width = mw;
    }
    if let Some(mh) = params.max_tile_height {
        opts.max_tile_height = mh;
    }
    if let Some(cols) = params.columns {
        opts.columns = cols;
    }
    let png = render_contact_sheet_png(&tiles, &opts)?;
    Ok((png, indices, None))
}

#[expect(
    clippy::too_many_arguments,
    reason = "internal helper threading decoded track, cache, backend, and display dimensions"
)]
fn render_inspect_single(
    reader: &dyn BundleReader,
    track: &Mp4VideoTrack,
    gop_cache: &mut GopLruCache,
    backend: &dyn arb_media::codec::VideoCodecBackend,
    params: &MediaInspectFramesParams,
    video_w: u32,
    video_h: u32,
    display_dims: (Option<u32>, Option<u32>),
) -> Result<InspectRenderOutput, DaemonError> {
    let frame_count = track.frame_count();
    let idx = params
        .frame_index
        .or_else(|| {
            params
                .frame_indices
                .as_ref()
                .and_then(|v| v.first().copied())
        })
        .unwrap_or(0);
    if idx >= frame_count {
        return Err(MediaError::FrameOutOfRange {
            frame_index: idx,
            frame_count,
        }
        .into());
    }
    let (decoded, _) = gop_cache.fetch_frame(track, idx, MediaPixelFormat::Rgba8, backend)?;
    let tap_point = if let (Some(tx), Some(ty)) = (params.tap_x, params.tap_y) {
        Some((tx, ty))
    } else if params.include_tap_marker {
        build_frame_tap_map(reader, video_w, video_h, display_dims)?
            .get(&idx)
            .copied()
    } else {
        None
    };
    let png = render_single_frame_png(&decoded.data, decoded.width, decoded.height, tap_point)?;
    Ok((png, vec![idx], None))
}

#[expect(
    clippy::too_many_arguments,
    reason = "internal helper threading decoded track, cache, backend, and display dimensions"
)]
fn render_inspect_tap_crop(
    reader: &dyn BundleReader,
    track: &Mp4VideoTrack,
    gop_cache: &mut GopLruCache,
    backend: &dyn arb_media::codec::VideoCodecBackend,
    params: &MediaInspectFramesParams,
    video_w: u32,
    video_h: u32,
    display_dims: (Option<u32>, Option<u32>),
) -> Result<InspectRenderOutput, DaemonError> {
    let frame_count = track.frame_count();
    let (idx, tx, ty) = if let Some(tap_idx) = params.tap_index {
        let frames = reader.frames()?;
        let main_taps = reader.query_taps(Some(RecordBucket::Main), TimeRange::all(), None)?;
        let selected_taps = select_overview_taps(&main_taps);
        let tap = selected_taps.get(tap_idx).copied().ok_or_else(|| {
            DaemonError::NotFound(format!(
                "tap_index {tap_idx} out of range (bundle has {} overview taps)",
                selected_taps.len()
            ))
        })?;
        let frame_idx = match params.frame_index {
            Some(f) => f,
            None => tap_boottime_ns(tap)
                .and_then(|bt| nearest_frame_index_for_epoch(&frames, tap_boot_id(tap), bt))
                .ok_or_else(|| {
                    DaemonError::InvalidParams(format!(
                        "tap_index {tap_idx} cannot be aligned to a video frame; pass an explicit frame_index"
                    ))
                })?,
        };
        let raw_x = tap.x.ok_or_else(|| {
            DaemonError::InvalidParams(format!("tap_index {tap_idx} has no x coordinate"))
        })?;
        let raw_y = tap.y.ok_or_else(|| {
            DaemonError::InvalidParams(format!("tap_index {tap_idx} has no y coordinate"))
        })?;
        let unmapped = || {
            DaemonError::InvalidParams(format!(
                "tap_index {tap_idx} lies outside the video frame; pass display_width/display_height \
                 (device display size) or explicit tap_x/tap_y in video pixels"
            ))
        };
        let mapped = mapped_tap(raw_x, raw_y, display_dims, video_w, video_h);
        let x = params
            .tap_x
            .or_else(|| mapped.map(|p| p.0))
            .ok_or_else(unmapped)?;
        let y = params
            .tap_y
            .or_else(|| mapped.map(|p| p.1))
            .ok_or_else(unmapped)?;
        (frame_idx, x, y)
    } else {
        let tx = params.tap_x.ok_or_else(|| {
            DaemonError::InvalidParams(
                "tap_crop requires either tap_index or (tap_x, tap_y)".to_owned(),
            )
        })?;
        let ty = params.tap_y.ok_or_else(|| {
            DaemonError::InvalidParams(
                "tap_crop requires either tap_index or (tap_x, tap_y)".to_owned(),
            )
        })?;
        let frame_idx = params.frame_index.unwrap_or(0);
        (frame_idx, tx, ty)
    };

    if idx >= frame_count {
        return Err(MediaError::FrameOutOfRange {
            frame_index: idx,
            frame_count,
        }
        .into());
    }

    let (decoded, _) = gop_cache.fetch_frame(track, idx, MediaPixelFormat::Rgba8, backend)?;
    let crop = render_tap_crop_png(
        &decoded.data,
        decoded.width,
        decoded.height,
        tx,
        ty,
        &TapCropOptions::default(),
    )?;
    let meta = TapCropMeta {
        crop_x: crop.crop_x,
        crop_y: crop.crop_y,
        crop_width: crop.crop_width,
        crop_height: crop.crop_height,
        tap_x: tx,
        tap_y: ty,
        tap_rel_x: crop.tap_rel_x,
        tap_rel_y: crop.tap_rel_y,
    };
    Ok((crop.png_bytes, vec![idx], Some(meta)))
}

fn build_frame_tap_map(
    reader: &dyn BundleReader,
    video_w: u32,
    video_h: u32,
    display_dims: (Option<u32>, Option<u32>),
) -> Result<BTreeMap<u64, (u32, u32)>, DaemonError> {
    let frames = reader.frames()?;
    let main_taps = reader.query_taps(Some(RecordBucket::Main), TimeRange::all(), None)?;
    let mut map = BTreeMap::new();
    for tap in select_overview_taps(&main_taps) {
        if let (Some(rx), Some(ry), Some(bt)) = (tap.x, tap.y, tap_boottime_ns(tap))
            && let Some(fi) = nearest_frame_index_for_epoch(&frames, tap_boot_id(tap), bt)
        {
            let mapped = mapped_tap(rx, ry, display_dims, video_w, video_h);
            if let Some(point) = mapped {
                map.entry(fi).or_insert(point);
            }
        }
    }
    Ok(map)
}

fn select_overview_taps(main_taps: &[TapRecord]) -> Vec<&TapRecord> {
    if main_taps.len() <= MAX_OVERVIEW_TAPS {
        return main_taps.iter().collect();
    }
    // Prefer discrete tap start/end actions when a high-frequency swipe floods `MOVE` rows.
    let discrete: Vec<&TapRecord> = main_taps
        .iter()
        .filter(|t| !t.action.eq_ignore_ascii_case("MOVE"))
        .take(MAX_OVERVIEW_TAPS)
        .collect();
    if !discrete.is_empty() {
        return discrete;
    }
    main_taps.iter().take(MAX_OVERVIEW_TAPS).collect()
}

fn build_overview_taps(
    main_taps: &[TapRecord],
    frames: &[FrameRecord],
    base_boottime_ns: Option<i64>,
) -> Vec<OverviewTapItem> {
    select_overview_taps(main_taps)
        .into_iter()
        .enumerate()
        .map(|(tap_index, tap)| {
            let bt = tap_boottime_ns(tap);
            let frame_index =
                bt.and_then(|t| nearest_frame_index_for_epoch(frames, tap_boot_id(tap), t));
            let t_ms =
                bt.and_then(|t| base_boottime_ns.map(|base| t.saturating_sub(base) / 1_000_000));
            let end_t_ms = tap
                .end_t_offset_ns
                .zip(t_ms)
                .map(|(off, base)| base.saturating_add(off / 1_000_000));
            OverviewTapItem {
                tap_index,
                seq: tap.seq,
                frame_index,
                t_ms,
                x: tap.x,
                y: tap.y,
                kind: tap.action.clone(),
                gesture_kind: tap.gesture_kind,
                end_x: tap.end_x,
                end_y: tap.end_y,
                end_t_ms,
            }
        })
        .collect()
}

fn build_error_log_highlights(
    main_logs: &[LogRecord],
    frames: &[FrameRecord],
    base_boottime_ns: Option<i64>,
    max_errors: usize,
) -> Vec<ErrorLogHighlight> {
    let mut out = Vec::new();
    if max_errors == 0 {
        return out;
    }
    for log in main_logs {
        let Some(level_raw) = log.level.as_deref() else {
            continue;
        };
        let level = normalize_log_level(level_raw);
        if !matches!(level, "E" | "F") {
            continue;
        }
        let bt = log_boottime_ns(log);
        let frame_index_hint =
            bt.and_then(|t| nearest_frame_index_for_epoch(frames, log_boot_id(log), t));
        let t_ms = bt.and_then(|t| base_boottime_ns.map(|base| t.saturating_sub(base) / 1_000_000));
        out.push(ErrorLogHighlight {
            seq: log.seq,
            level: level.to_owned(),
            frame_index_hint,
            t_ms,
            tag: log.tag.clone(),
            message: truncate_chars(&log.message, MAX_ERROR_LOG_MESSAGE_CHARS),
        });
        // Keep the LAST `max_errors` rows (nearest the end of the window, where crashes land).
        if out.len() > max_errors {
            out.remove(0);
        }
    }
    out
}

fn normalize_log_level(raw: &str) -> &str {
    match raw.trim().to_ascii_uppercase().as_str() {
        "E" | "ERROR" => "E",
        "F" | "FATAL" | "A" | "ASSERT" => "F",
        "W" | "WARN" | "WARNING" => "W",
        "I" | "INFO" => "I",
        "D" | "DEBUG" => "D",
        "V" | "VERBOSE" => "V",
        _ => "",
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn frame_boottime_ns(frame: &FrameRecord) -> Option<i64> {
    match &frame.alignment {
        TimeAlignment::Anchored { boottime_ns, .. } => Some(*boottime_ns),
        TimeAlignment::Bounded {
            min_boottime_ns, ..
        } => Some(*min_boottime_ns),
        _ => None,
    }
}

fn tap_boottime_ns(tap: &TapRecord) -> Option<i64> {
    match &tap.alignment {
        TimeAlignment::Anchored { boottime_ns, .. } => Some(*boottime_ns),
        TimeAlignment::Bounded {
            min_boottime_ns,
            max_boottime_ns,
            ..
        } => Some(
            min_boottime_ns.saturating_add(max_boottime_ns.saturating_sub(*min_boottime_ns) / 2),
        ),
        _ => None,
    }
}

fn log_boottime_ns(log: &LogRecord) -> Option<i64> {
    match &log.alignment {
        TimeAlignment::Anchored { boottime_ns, .. } => Some(*boottime_ns),
        TimeAlignment::Bounded {
            min_boottime_ns,
            max_boottime_ns,
            ..
        } => Some(
            min_boottime_ns.saturating_add(max_boottime_ns.saturating_sub(*min_boottime_ns) / 2),
        ),
        _ => log.device_read_boottime_ns,
    }
}

fn tap_boot_id(tap: &TapRecord) -> Option<&str> {
    tap.boot_id.as_deref().or(match &tap.alignment {
        TimeAlignment::Anchored { boot_id, .. } | TimeAlignment::Bounded { boot_id, .. } => {
            Some(boot_id.as_str())
        }
        _ => None,
    })
}

fn log_boot_id(log: &LogRecord) -> Option<&str> {
    log.boot_id.as_deref().or(match &log.alignment {
        TimeAlignment::Anchored { boot_id, .. } | TimeAlignment::Bounded { boot_id, .. } => {
            Some(boot_id.as_str())
        }
        _ => None,
    })
}

fn nearest_frame_index_for_epoch(
    frames: &[FrameRecord],
    boot_id: Option<&str>,
    target_bt_ns: i64,
) -> Option<u64> {
    let mut best: Option<(u64, u64)> = None;
    for frame in frames {
        if let Some(want) = boot_id
            && frame.boot_id != want
        {
            continue;
        }
        let Some(bt) = frame_boottime_ns(frame) else {
            continue;
        };
        let dist = bt.abs_diff(target_bt_ns);
        if best.is_none_or(|(best_dist, _)| dist < best_dist) {
            best = Some((dist, frame.frame_index));
        }
    }
    best.map(|(_, idx)| idx)
}

fn first_bundle_boottime_ns(
    frames: &[FrameRecord],
    epochs: &[arb_core::bundle::EpochSpan],
) -> Option<i64> {
    frames
        .iter()
        .find_map(frame_boottime_ns)
        .or_else(|| epochs.first().map(|e| e.start_boottime_ns))
}

fn compute_bundle_duration_ms(
    video_duration_ns: Option<u64>,
    frames: &[FrameRecord],
    epochs: &[arb_core::bundle::EpochSpan],
) -> u64 {
    if let Some(ns) = video_duration_ns
        && ns > 0
    {
        return ns / 1_000_000;
    }
    let frame_span =
        frames
            .iter()
            .filter_map(frame_boottime_ns)
            .fold(None::<(i64, i64)>, |acc, bt| {
                Some(match acc {
                    Some((lo, hi)) => (lo.min(bt), hi.max(bt)),
                    None => (bt, bt),
                })
            });
    if let Some((lo, hi)) = frame_span {
        return hi.saturating_sub(lo).unsigned_abs() / 1_000_000;
    }
    if let Some(ep) = epochs.first() {
        return ep
            .end_boottime_ns
            .saturating_sub(ep.start_boottime_ns)
            .unsigned_abs()
            / 1_000_000;
    }
    0
}

#[expect(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "fps_estimate is a human/AI summary metric rounded to 2 decimal places"
)]
fn compute_fps_estimate(frame_count: usize, duration_ms: u64) -> Option<f64> {
    if frame_count == 0 || duration_ms == 0 {
        return None;
    }
    let raw = (frame_count as f64) * 1000.0 / (duration_ms as f64);
    Some((raw * 100.0).round() / 100.0)
}

#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "tap coordinate is finite, rounded, and explicitly clamped into [0, dim - 1]"
)]
fn clamp_coord_to_frame(raw: f64, dim: u32) -> u32 {
    if !raw.is_finite() || dim == 0 {
        return 0;
    }
    let max_idx = f64::from(dim.saturating_sub(1));
    raw.round().clamp(0.0, max_idx) as u32
}

fn mapped_tap(
    x: f64,
    y: f64,
    display: (Option<u32>, Option<u32>),
    w: u32,
    h: u32,
) -> Option<(u32, u32)> {
    arb_core::gesture::map_touch_to_frame(x, y, display.0, display.1, w, h)
        .map(|(x, y)| (clamp_coord_to_frame(x, w), clamp_coord_to_frame(y, h)))
}

fn parse_png_dimensions(png_bytes: &[u8]) -> Result<(u32, u32), DaemonError> {
    if png_bytes.len() < 24
        || &png_bytes[0..8] != b"\x89PNG\r\n\x1a\n"
        || &png_bytes[12..16] != b"IHDR"
    {
        return Err(DaemonError::Internal(
            "rendered inspect PNG failed signature/IHDR verification".to_owned(),
        ));
    }
    let w = u32::from_be_bytes([png_bytes[16], png_bytes[17], png_bytes[18], png_bytes[19]]);
    let h = u32::from_be_bytes([png_bytes[20], png_bytes[21], png_bytes[22], png_bytes[23]]);
    Ok((w, h))
}

fn write_bytes_atomic(final_path: &Path, bytes: &[u8]) -> Result<(), DaemonError> {
    let tmp = final_path.with_extension(format!("tmp-{}.png", next_id("w")));
    let result = (|| -> Result<(), DaemonError> {
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        arb_os::fs::replace_file(&tmp, final_path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn sanitize_cache_key(bundle_id: &str) -> String {
    let safe = (1..=64).contains(&bundle_id.len())
        && bundle_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && !is_windows_reserved_name(bundle_id);
    if safe {
        bundle_id.to_owned()
    } else {
        format!("h-{}", &sha256_hex(bundle_id.as_bytes())[..32])
    }
}

fn is_windows_reserved_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.len() == 4
            && upper.as_bytes()[3].is_ascii_digit())
}

fn prune_cache_dir(dir: &Path, max_files: usize, max_bytes: u64, keep: Option<&Path>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    let mut total_bytes = 0_u64;
    let mut fresh_count = 0_usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || keep.is_some_and(|k| k == path) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let len = meta.len();
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        total_bytes = total_bytes.saturating_add(len);
        // Recently touched files may be another request's in-progress temp file or a path that
        // was just returned to a client; never evict them.
        let fresh = SystemTime::now()
            .duration_since(mtime)
            .is_ok_and(|age| age < CACHE_PRUNE_MIN_AGE);
        if fresh {
            fresh_count += 1;
            continue;
        }
        files.push((path, len, mtime));
    }
    let mut remaining = files.len() + fresh_count;
    if remaining <= max_files && total_bytes <= max_bytes {
        return;
    }
    // Oldest modified first.
    files.sort_by_key(|(_, _, mtime)| *mtime);
    for (path, len, _) in files {
        if remaining <= max_files && total_bytes <= max_bytes {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            remaining = remaining.saturating_sub(1);
            total_bytes = total_bytes.saturating_sub(len);
        }
    }
}

/// Pure-Rust RFC 4648 standard Base64 encoder (zero dependencies).
pub(crate) use arb_core::base64_encode;

#[cfg(test)]
mod tests {
    use super::{base64_encode, mapped_tap, prune_cache_dir};

    #[test]
    fn tap_coords_scale_from_display_space_and_refuse_out_of_frame() {
        let display = (Some(1080), Some(2400));
        assert_eq!(
            mapped_tap(540.0, 1200.0, display, 460, 1024),
            Some((230, 512))
        );
        assert_eq!(mapped_tap(1.0, 1.0, display, 460, 1024), Some((0, 0)));
        assert_eq!(
            mapped_tap(1.0, 1.0, (None, None), 460, 1024),
            Some((459, 1023))
        );
        assert_eq!(mapped_tap(2.0, 0.0, (None, None), 460, 1024), None);
        assert_eq!(mapped_tap(1.0, 1.0, display, 1024, 460), Some((1, 0)));
    }

    #[test]
    fn prune_keeps_fresh_files_even_over_limit() {
        let dir = std::env::temp_dir().join(format!("arb-prune-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        for i in 0..4 {
            std::fs::write(dir.join(format!("f{i}.wav")), b"x").expect("write");
        }
        prune_cache_dir(&dir, 1, 1024, None);
        let left = std::fs::read_dir(&dir).expect("read").count();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(left, 4, "just-written files must survive pruning");
    }

    #[test]
    fn base64_encode_matches_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "", "empty vector");
        assert_eq!(base64_encode(b"f"), "Zg==", "1-byte vector");
        assert_eq!(base64_encode(b"fo"), "Zm8=", "2-byte vector");
        assert_eq!(base64_encode(b"foo"), "Zm9v", "3-byte vector");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy", "6-byte vector");
    }

    #[test]
    fn nearest_frame_index_for_epoch_prevents_cross_epoch_bleed() {
        use arb_core::bundle::FrameRecord;
        use arb_core::time::{TimeAlignment, TimestampSemantics};

        let mk = |frame_index: u64, boot: &str, bt: i64| FrameRecord {
            frame_index,
            boot_id: boot.to_owned(),
            pts_device_mono_ns: bt,
            pc_ingest_cont_ns: bt,
            alignment: TimeAlignment::Anchored {
                boot_id: boot.to_owned(),
                boottime_ns: bt,
                half_width_ns: 1_000,
                semantics: TimestampSemantics::VideoEncoderInputEstimate,
            },
            is_keyframe: frame_index == 0 || frame_index == 10,
            gop_id: u64::from(frame_index >= 10),
            mp4_sample_offset: 0,
            mp4_sample_size: 100,
        };
        let frames = vec![
            mk(0, "boot-0", 10_000_000_000),
            mk(1, "boot-0", 11_000_000_000),
            mk(10, "boot-1", 10_000_000_000),
            mk(11, "boot-1", 11_000_000_000),
        ];
        assert_eq!(
            super::nearest_frame_index_for_epoch(&frames, Some("boot-0"), 11_000_000_000),
            Some(1)
        );
        assert_eq!(
            super::nearest_frame_index_for_epoch(&frames, Some("boot-1"), 11_000_000_000),
            Some(11)
        );
        assert_eq!(
            super::nearest_frame_index_for_epoch(&frames, Some("boot-missing"), 11_000_000_000),
            None,
            "unknown epoch must not bleed into frames from another boot"
        );
    }
}
