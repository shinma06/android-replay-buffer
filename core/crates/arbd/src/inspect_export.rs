//! Standalone T-2B RPC operations running outside the live SHM viewport mutex:
//! - `media.decode_audio_pcm`: pure-Rust AAC-LC -> cached s16le RIFF/WAV decode
//! - `export.share_mp4`: full or mid-GOP trimmed single-file faststart zero-`elst` MP4 export
//! - `timeline.overview`: one-shot structured AI triage bundle summary
//! - `media.inspect_frames`: contact sheet / single frame / tap-crop PNG renderer

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

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
use arb_media::mp4::{Mp4VideoTrack, inspect_mp4_atoms};
use arb_media::mux_av::AvMuxReport;
use arb_media::mux_av::mux_share_mp4;
use arb_media::playback::GopLruCache;
use arb_storage::bundle_io::{BundleReader, TimeRange, open_bundle};
use arb_storage::catalog::{BundleEntry, Catalog, CatalogFormatVersion, LeaseHolderKind};
use arb_storage::sha256_hex;

use crate::error::DaemonError;
use crate::media_engine::{ExportContext, compute_epoch_frame_ranges, export_trim};
use crate::protocol::{
    DEFAULT_OVERVIEW_ERROR_LOGS, ErrorLogHighlight, ExportShareMp4Params, ExportShareMp4Response,
    ExportTrimParams, InspectFramesMode, MAX_OVERVIEW_ERROR_LOGS, MediaDecodeAudioPcmResponse,
    MediaInspectFramesParams, MediaInspectFramesResponse, OverviewTapItem, TapCropMeta,
    TimelineOverviewResponse, TimelineQueryParams,
};
use crate::time_util::{next_id, utc_now_iso8601};

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

struct LeaseGuard<'a> {
    catalog: &'a Catalog,
    lease_id: String,
}

impl Drop for LeaseGuard<'_> {
    fn drop(&mut self) {
        let _ = self.catalog.release_lease(&self.lease_id);
    }
}

fn acquire_job_lease<'a>(
    catalog: &'a Catalog,
    source_catalog_id: Option<&str>,
    holder_pid: u32,
) -> Result<Option<LeaseGuard<'a>>, DaemonError> {
    let Some(bundle_id) = source_catalog_id else {
        return Ok(None);
    };
    let lease_id = catalog.acquire_lease(
        bundle_id,
        LeaseHolderKind::ExportJob,
        holder_pid,
        &utc_now_iso8601(),
    )?;
    Ok(Some(LeaseGuard { catalog, lease_id }))
}

/// Decodes a bundle's `audio.m4a` to a cached signed 16-bit LE PCM RIFF/WAV file under
/// `<cache_dir>/audio-pcm/<safe_bundle_id>.wav`.
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
    let _lease = acquire_job_lease(catalog, source_catalog_id, std::process::id())?;
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
    let resolved_id = source_catalog_id.unwrap_or(&summary.bundle_id);
    let safe_key = sanitize_cache_key(resolved_id);
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
    if params.in_frame_index.is_some() != params.out_frame_index.is_some() {
        return Err(DaemonError::InvalidParams(
            "in_frame_index and out_frame_index must either both be provided or both be omitted"
                .to_owned(),
        ));
    }

    let source_catalog_id = source_entry.map(|e| e.bundle_id.as_str());
    let holder_pid = params.holder_pid.unwrap_or_else(std::process::id);
    let lease_guard = acquire_job_lease(ctx.catalog, source_catalog_id, holder_pid)?;

    let reader = open_bundle(source_bundle_dir)?;
    let summary = reader.manifest_summary();
    let is_legacy = reader.is_legacy()
        || source_entry.is_some_and(|e| e.format_version == CatalogFormatVersion::LegacyRo);

    // Enforce REQ-04/07 safety invariant upfront: never trash read-only legacy or external bundles.
    if params.trash_original {
        if is_legacy {
            return Err(DaemonError::Conflict(format!(
                "refusing trash_original on read-only legacy bundle `{}`",
                source_catalog_id.unwrap_or(&summary.bundle_id)
            )));
        }
        if source_catalog_id.is_none() {
            return Err(DaemonError::Conflict(
                "refusing trash_original on uncataloged external bundle; open or register it first"
                    .to_owned(),
            ));
        }
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

    let prepared = prepare_share_mp4_sources(
        ctx,
        reader.as_ref(),
        source_bundle_dir,
        source_catalog_id,
        dest_parent,
        &source_video,
        requested_trim,
        params,
    )?;

    let av_mux_report = mux_and_verify_share_mp4(ctx, &prepared, share_mp4_path)?;

    // Release temporary export lease before logical trash (source files remain untouched on disk).
    drop(lease_guard);

    let mut original_trashed = false;
    let mut trash_warning: Option<String> = None;
    if params.trash_original
        && let Some(src_id) = source_catalog_id
    {
        // share.mp4 is already committed: a late trash failure (source already trashed, racing
        // trash) must not hide the export result, so it degrades to a warning.
        match ctx.catalog.trash_bundle(src_id, &utc_now_iso8601()) {
            Ok(_) => original_trashed = true,
            Err(err) => {
                trash_warning = Some(format!(
                    "share.mp4 was written but trashing the original failed: {err}"
                ));
            }
        }
    }

    Ok(ExportShareMp4Response {
        share_mp4_path: share_mp4_path.to_path_buf(),
        trimmed_bundle_id: prepared.trimmed_bundle_id,
        trimmed_bundle_path: prepared.trimmed_bundle_path,
        audio_included: av_mux_report.audio_included,
        video_frame_count: av_mux_report.video_frame_count,
        edts_box_count: av_mux_report.edts_box_count,
        elst_box_count: av_mux_report.elst_box_count,
        output_bytes: av_mux_report.output_bytes,
        original_trashed,
        av_mux_report,
        warning: match (prepared.warning, trash_warning) {
            (Some(a), Some(b)) => Some(format!("{a}; {b}")),
            (a, b) => a.or(b),
        },
    })
}

struct PreparedShareSources {
    trimmed_bundle_id: Option<String>,
    trimmed_bundle_path: Option<PathBuf>,
    video_path: PathBuf,
    audio_path: Option<PathBuf>,
    lead_in_offset_ns: i64,
    warning: Option<String>,
    _temp_burned_video: Option<TempFileCleanup>,
}

struct TempFileCleanup(PathBuf);

impl Drop for TempFileCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

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
        _ => Ok(None),
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
    Ok((tmp_burned.clone(), Some(TempFileCleanup(tmp_burned))))
}

#[expect(
    clippy::too_many_arguments,
    reason = "internal helper bundling validated inputs for full vs trimmed share.mp4 muxing"
)]
fn prepare_share_mp4_sources(
    ctx: &ExportContext<'_>,
    reader: &dyn BundleReader,
    source_bundle_dir: &Path,
    source_catalog_id: Option<&str>,
    dest_parent: &Path,
    source_video: &Path,
    requested_trim: Option<(u64, u64)>,
    params: &ExportShareMp4Params,
) -> Result<PreparedShareSources, DaemonError> {
    if let Some((in_f, out_f)) = requested_trim {
        let trim_params = ExportTrimParams {
            bundle_id_or_path: params.bundle_id.clone(),
            in_frame_index: in_f,
            out_frame_index: out_f,
            dest_dir: params.dest_dir.clone(),
            holder_pid: params.holder_pid,
            burn_taps: false,
            include_share_mp4: false,
            trash_original: false,
        };
        let trim_resp = export_trim(
            ctx,
            source_bundle_dir,
            source_catalog_id,
            dest_parent,
            &trim_params,
        )?;
        let trimmed_reader = open_bundle(&trim_resp.bundle_path)?;
        let trimmed_video = trimmed_reader.video_path().ok_or_else(|| {
            DaemonError::Internal(format!(
                "committed trimmed bundle `{}` is missing video.mp4",
                trim_resp.bundle_id
            ))
        })?;
        let trimmed_audio = trimmed_reader.audio_path();
        let lead_in_offset_ns = trimmed_reader
            .manifest_summary()
            .streams
            .audio
            .as_ref()
            .map_or(0, |a| a.audio_lead_in_offset_ns);
        let (video_path, temp_burned) = if params.burn_taps {
            burn_in_share_video_if_nonempty(ctx, trimmed_reader.as_ref(), &trimmed_video)?
        } else {
            (trimmed_video, None)
        };
        Ok(PreparedShareSources {
            trimmed_bundle_id: Some(trim_resp.bundle_id),
            trimmed_bundle_path: Some(trim_resp.bundle_path),
            video_path,
            audio_path: trimmed_audio,
            lead_in_offset_ns,
            warning: trim_resp.warning,
            _temp_burned_video: temp_burned,
        })
    } else {
        let lead_in_offset_ns = reader
            .manifest_summary()
            .streams
            .audio
            .as_ref()
            .map_or(0, |a| a.audio_lead_in_offset_ns);
        let (video_path, temp_burned) = if params.burn_taps {
            burn_in_share_video_if_nonempty(ctx, reader, source_video)?
        } else {
            (source_video.to_path_buf(), None)
        };
        Ok(PreparedShareSources {
            trimmed_bundle_id: None,
            trimmed_bundle_path: None,
            video_path,
            audio_path: reader.audio_path(),
            lead_in_offset_ns,
            warning: None,
            _temp_burned_video: temp_burned,
        })
    }
}

/// Muxes and verifies `share.mp4`; on failure the trimmed bundle created for this export is
/// logically trashed so no orphan is left in the catalog.
fn mux_and_verify_share_mp4(
    ctx: &ExportContext<'_>,
    prepared: &PreparedShareSources,
    share_mp4_path: &Path,
) -> Result<AvMuxReport, DaemonError> {
    let committed = mux_share_mp4(
        &prepared.video_path,
        prepared.audio_path.as_deref(),
        prepared.lead_in_offset_ns,
        share_mp4_path,
    )
    .map_err(DaemonError::from)
    .and_then(|report| {
        // Verify committed share.mp4 structure before any non-destructive trash of the source.
        verify_committed_share_mp4(share_mp4_path, report.video_frame_count)?;
        Ok(report)
    });
    if committed.is_err()
        && let Some(trimmed_id) = prepared.trimmed_bundle_id.as_deref()
    {
        let _ = ctx.catalog.trash_bundle(trimmed_id, &utc_now_iso8601());
    }
    committed
}

fn verify_committed_share_mp4(
    share_mp4_path: &Path,
    expected_frames: u64,
) -> Result<(), DaemonError> {
    let atoms = inspect_mp4_atoms(share_mp4_path)?;
    if atoms.edts_count != 0 || atoms.elst_count != 0 {
        return Err(DaemonError::Internal(format!(
            "verified share.mp4 unexpectedly contained edts={} elst={}",
            atoms.edts_count, atoms.elst_count
        )));
    }
    let verified_track = Mp4VideoTrack::open(share_mp4_path)?;
    if verified_track.frame_count() != expected_frames {
        return Err(DaemonError::Internal(format!(
            "verified share.mp4 frame count {} disagreed with mux report {expected_frames}",
            verified_track.frame_count(),
        )));
    }
    Ok(())
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
    let _lease = acquire_job_lease(ctx.catalog, source_catalog_id, std::process::id())?;
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
/// # Errors
///
/// Returns [`DaemonError::InvalidParams`] if `start_frame > end_frame` or a requested frame
/// index exceeds the bundle's frame count.
#[expect(
    clippy::too_many_lines,
    reason = "sequential frame-range, package, level, tag, and keyword log filter pipeline"
)]
pub fn query_logs_filtered(
    reader: &dyn BundleReader,
    bucket: Option<RecordBucket>,
    params: &TimelineQueryParams,
    limit: usize,
) -> Result<Vec<LogRecord>, DaemonError> {
    let mut range = TimeRange {
        start_ns: params.start_ns,
        end_ns: params.end_ns,
    };

    if params.start_frame.is_some() || params.end_frame.is_some() {
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
    }

    let package_filter = params
        .package
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let has_text_filter = params.tag.is_some()
        || params.level.is_some()
        || params.keyword.is_some()
        || package_filter.is_some();
    let fetch_limit = if has_text_filter { None } else { Some(limit) };
    let mut logs = reader.query_logs(bucket, range, fetch_limit)?;

    if let Some(target_pkg) = package_filter {
        // Build the PID/package resolver across all bundle logs so PID attributions learned
        // outside the requested frame/time slice still attribute logs inside the slice accurately.
        let all_logs = if range == TimeRange::all() && bucket.is_none() {
            logs.clone()
        } else {
            reader.query_logs(None, TimeRange::all(), None)?
        };
        logs = arb_core::analytics::filter_logs_by_package(&all_logs, &logs, target_pkg);
    }

    if has_text_filter {
        let tag_needle = params.tag.as_deref().map(str::to_ascii_lowercase);
        let level_needle = match params.level.as_deref() {
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
        let kw_needle = params.keyword.as_deref().map(str::to_ascii_lowercase);

        logs.retain(|log| {
            if let Some(ref want_level) = level_needle {
                let actual = log.level.as_deref().map_or("", normalize_log_level);
                if actual != want_level {
                    return false;
                }
            }
            if let Some(ref want_tag) = tag_needle {
                let matched = log
                    .tag
                    .as_deref()
                    .is_some_and(|t| t.to_ascii_lowercase().contains(want_tag));
                if !matched {
                    return false;
                }
            }
            if let Some(ref want_kw) = kw_needle {
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
        });
        logs.truncate(limit);
    }

    Ok(logs)
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
    let oriented = orient_display_dims(display_dims, video_w, video_h);
    let (display_w, display_h) = oriented.unwrap_or((None, None));
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
        let x = match params.tap_x {
            Some(x) => x,
            None if oriented.is_none() => return Err(unmapped()),
            None => map_tap_coord_to_frame(raw_x, display_w, video_w).ok_or_else(unmapped)?,
        };
        let y = match params.tap_y {
            Some(y) => y,
            None => map_tap_coord_to_frame(raw_y, display_h, video_h).ok_or_else(unmapped)?,
        };
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
    let Some((display_w, display_h)) = orient_display_dims(display_dims, video_w, video_h) else {
        return Ok(BTreeMap::new());
    };
    let frames = reader.frames()?;
    let main_taps = reader.query_taps(Some(RecordBucket::Main), TimeRange::all(), None)?;
    let mut map = BTreeMap::new();
    for tap in select_overview_taps(&main_taps) {
        if let (Some(rx), Some(ry), Some(bt)) = (tap.x, tap.y, tap_boottime_ns(tap))
            && let Some(fi) = nearest_frame_index_for_epoch(&frames, tap_boot_id(tap), bt)
        {
            let mapped = map_tap_coord_to_frame(rx, display_w, video_w)
                .zip(map_tap_coord_to_frame(ry, display_h, video_h));
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

/// Maps a recorded tap coordinate (display pixels) onto the encoded video.
///
/// With a known `display_dim` the coordinate is scaled by `video_dim / display_dim`. Without it
/// the coordinate is only trusted when it already lies inside the video frame; a coordinate
/// beyond the frame proves it is in a larger display space, so `None` is returned instead of
/// pinning it to the frame edge.
/// Checks that the (natural-orientation) display size matches the video orientation.
///
/// Raw tap axes are never rotated, so a landscape video of a portrait panel (or vice versa)
/// cannot be mapped faithfully: `None` means "orientation mismatch, leave taps unmapped".
fn orient_display_dims(
    display: (Option<u32>, Option<u32>),
    video_w: u32,
    video_h: u32,
) -> Option<(Option<u32>, Option<u32>)> {
    match display {
        (Some(dw), Some(dh))
            if video_w != video_h && dw != dh && (dw < dh) != (video_w < video_h) =>
        {
            None
        }
        other => Some(other),
    }
}

fn map_tap_coord_to_frame(raw: f64, display_dim: Option<u32>, video_dim: u32) -> Option<u32> {
    if !raw.is_finite() || raw < 0.0 || video_dim == 0 {
        return None;
    }
    match display_dim.filter(|d| *d > 0) {
        Some(display) => {
            // A value beyond the display proves the coordinate is in another space (e.g. a raw
            // evdev axis range), so it is unmapped rather than pinned to the frame edge.
            if raw > f64::from(display) {
                return None;
            }
            let scaled = raw * f64::from(video_dim) / f64::from(display);
            Some(clamp_coord_to_frame(scaled, video_dim))
        }
        None if raw < f64::from(video_dim) => Some(clamp_coord_to_frame(raw, video_dim)),
        None => None,
    }
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
        fs::rename(&tmp, final_path)?;
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
pub(crate) fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = usize::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, usize::from);
        let b2 = chunk.get(2).copied().map_or(0, usize::from);
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(char::from(TABLE[(n >> 18) & 0x3F]));
        out.push(char::from(TABLE[(n >> 12) & 0x3F]));
        if chunk.len() > 1 {
            out.push(char::from(TABLE[(n >> 6) & 0x3F]));
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(char::from(TABLE[n & 0x3F]));
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{base64_encode, map_tap_coord_to_frame, orient_display_dims, prune_cache_dir};

    #[test]
    fn tap_coords_scale_from_display_space_and_refuse_out_of_frame() {
        // 1080x2400 display encoded as 460x1024: (540,1200) maps to the frame centre.
        assert_eq!(map_tap_coord_to_frame(540.0, Some(1080), 460), Some(230));
        assert_eq!(map_tap_coord_to_frame(1200.0, Some(2400), 1024), Some(512));
        // Unknown display size: in-frame coordinates pass, out-of-frame ones are refused
        // instead of being pinned to the bottom-right edge.
        assert_eq!(map_tap_coord_to_frame(100.0, None, 460), Some(100));
        assert_eq!(map_tap_coord_to_frame(1200.0, None, 1024), None);
        assert_eq!(map_tap_coord_to_frame(f64::NAN, Some(10), 5), None);
        // Raw evdev values beyond the display are unmapped, never edge-clamped.
        assert_eq!(map_tap_coord_to_frame(16000.0, Some(1080), 460), None);
        // Orientation mismatch (landscape video of a portrait panel) is unmapped, not guessed.
        assert_eq!(
            orient_display_dims((Some(1080), Some(2400)), 2400, 1080),
            None
        );
        assert_eq!(
            orient_display_dims((Some(1080), Some(2400)), 460, 1024),
            Some((Some(1080), Some(2400)))
        );
        assert_eq!(
            orient_display_dims((Some(1080), Some(2400)), 128, 128),
            Some((Some(1080), Some(2400)))
        );
        assert_eq!(map_tap_coord_to_frame(-1.0, None, 5), None);
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
