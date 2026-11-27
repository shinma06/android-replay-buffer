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
    required_video_path,
};
use crate::protocol::{
    DEFAULT_OVERVIEW_ERROR_LOGS, ErrorLogHighlight, ExportShareMp4Params, ExportShareMp4Response,
    ExportTrimParams, InspectFramesMode, MAX_OVERVIEW_ERROR_LOGS, MediaDecodeAudioPcmResponse,
    MediaInspectFramesParams, MediaInspectFramesResponse, NoiseCandidate, OverviewTapItem,
    TapCropMeta, TimelineOverviewResponse, TimelineQueryParams,
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
/// `[in_frame_index..=out_frame_index]`. Only `keep_bundle` catalogs the selected range. Supports
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

    let times = crate::trim_range::time_pair(params.in_time_ns, params.out_time_ns)?;
    let source_video = required_video_path(
        reader.as_ref(),
        source_catalog_id.unwrap_or(&summary.bundle_id),
    )?;
    let src_track = Mp4VideoTrack::open(&source_video)?;
    let range = crate::trim_range::TrimRange::resolve(
        &src_track,
        (params.in_frame_index, params.out_frame_index),
        times,
    )?;
    let requested_trim =
        (range.realtime || range.first != 0 || range.last + 1 != src_track.frame_count())
            .then_some(range);

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

    let (av_mux_report, trimmed_bundle) = if params.keep_bundle {
        let trim = export_trim(
            ctx,
            source_bundle_dir,
            source_catalog_id,
            dest_parent,
            &ExportTrimParams {
                bundle_id_or_path: params.bundle_id.clone(),
                in_frame_index: Some(range.first),
                out_frame_index: Some(range.last),
                in_time_ns: params.in_time_ns,
                out_time_ns: params.out_time_ns,
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
        let report = mux_selected_media(
            ctx,
            reader.as_ref(),
            &source_video,
            requested_trim,
            params,
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
        in_time_ns: Some(range.start),
        out_time_ns: Some(range.end),
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

fn share_touch_events(
    taps: Vec<arb_core::bundle::TapRecord>,
    window: Option<&arb_core::selection::WindowSpec>,
    range: Option<crate::trim_range::TrimRange>,
) -> Vec<BurnInTouchEvent> {
    taps.into_iter()
        .filter(|tap| window.is_none_or(|w| tap_boot_id(tap) == Some(w.boot_id())))
        .filter_map(|mut tap| {
            if let Some(w) = window.filter(|_| range.is_some_and(|r| r.realtime))
                && (!crate::trim_range::clip_tap(&mut tap, w)
                    || arb_core::selection::classify_record(&tap, w)
                        != arb_core::selection::Membership::InRange)
            {
                return None;
            }
            BurnInTouchEvent::from_tap_record(&tap)
        })
        .collect()
}

fn mux_selected_media(
    ctx: &ExportContext<'_>,
    reader: &dyn BundleReader,
    source: &Path,
    range: Option<crate::trim_range::TrimRange>,
    params: &ExportShareMp4Params,
    output: &Path,
) -> Result<arb_media::AvMuxReport, DaemonError> {
    let summary = reader.manifest_summary();
    let tmp = ctx.data_dir.join("tmp");
    let job = next_id("share");
    let taps = if params.burn_taps {
        reader.query_taps(Some(RecordBucket::Main), TimeRange::all(), None)?
    } else {
        Vec::new()
    };
    let video_guard = if range.is_some() || !taps.is_empty() {
        fs::create_dir_all(&tmp)?;
        let guard = TempFileCleanup(tmp.join(format!("{job}.mp4")));
        let frames = reader.display_frames()?;
        let window = range
            .filter(|r| !r.realtime || !taps.is_empty())
            .map(|r| r.window(&frames))
            .transpose()?;
        let mut events = share_touch_events(taps, window.as_ref(), range);
        let geometry = BurnInDisplayGeometry {
            display_width: summary.streams.video.as_ref().and_then(|v| v.display_width),
            display_height: summary
                .streams
                .video
                .as_ref()
                .and_then(|v| v.display_height),
        };
        if let Some(r) = range.filter(|r| r.realtime) {
            if events.is_empty() {
                r.trim_video(source, &guard.0, ctx.backend)?;
            } else {
                let cut = TempFileCleanup(tmp.join(format!("{job}-cut.mp4")));
                r.trim_video(source, &cut.0, ctx.backend)?;
                let window = window
                    .as_ref()
                    .ok_or_else(|| DaemonError::Internal("missing trim window".into()))?;
                let clock = arb_core::bundle::FrameMediaClock::new(
                    &frames,
                    window.boot_id(),
                    window.start_boottime_ns(),
                );
                for event in &mut events {
                    event.rebase_on_clock(&clock);
                }
                arb_media::burn_in::burn_in_taps_time_range(
                    &cut.0,
                    &guard.0,
                    &r.sample_boundaries(&Mp4VideoTrack::open(source)?)?,
                    &events,
                    geometry,
                    ctx.backend,
                )?;
            }
        } else {
            let (first, last) = range.map_or_else(
                || Ok::<_, DaemonError>((0, Mp4VideoTrack::open(source)?.frame_count() - 1)),
                |r| Ok((r.first, r.last)),
            )?;
            let timestamps = frames
                .iter()
                .map(|f| frame_boottime_ns(f).unwrap_or(f.pts_device_mono_ns))
                .collect::<Vec<_>>();
            burn_in_taps_video(
                source,
                &guard.0,
                first,
                last,
                &timestamps,
                &events,
                geometry,
                ctx.backend,
            )?;
        }
        Some(guard)
    } else {
        None
    };
    let selected = video_guard.as_ref().map_or(source, |g| g.0.as_path());
    let audio = range
        .map(|bounds| crate::media_engine::trim_source_audio(reader, bounds, &tmp, &job))
        .transpose()?
        .flatten();
    let (audio_path, offset) = match range {
        Some(_) => (
            audio.as_ref().map(|(a, _)| a.tmp_m4a.clone()),
            audio.as_ref().map_or(0, |(a, _)| a.lead_in_offset_ns),
        ),
        None => (
            reader.audio_path(),
            summary
                .streams
                .audio
                .as_ref()
                .map_or(0, |a| a.audio_lead_in_offset_ns),
        ),
    };
    Ok(mux_share_mp4(
        selected,
        audio_path.as_deref(),
        offset,
        output,
    )?)
}

/// Builds a compact one-shot [`TimelineOverviewResponse`] for AI triage.
///
/// # Errors
///
/// Returns [`DaemonError`] if the bundle or its timeline tables cannot be read.
pub fn build_timeline_overview(
    bundle_dir: &Path,
    catalog_bundle_id: Option<&str>,
    params: &crate::protocol::TimelineOverviewParams,
) -> Result<TimelineOverviewResponse, DaemonError> {
    let reader = open_bundle(bundle_dir)?;
    let summary = reader.manifest_summary();
    let bundle_id = catalog_bundle_id.unwrap_or(&summary.bundle_id).to_owned();

    let frames = reader.display_frames()?;
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
    let duration_ms = if frames.is_empty() {
        reader.log_statistics()?.duration_ms
    } else {
        compute_bundle_duration_ms(video_duration_ns, &frames, &summary.epochs)
    };
    let fps_estimate = compute_fps_estimate(frame_count, duration_ms);

    let screens = screens_with_offsets(reader.as_ref(), &frames)?;
    let crashes = reader.crashes()?;
    let discontinuities = reader.discontinuities()?;

    let main_taps = reader.query_taps(Some(RecordBucket::Main), TimeRange::all(), None)?;
    let taps = build_overview_taps(&main_taps, &frames, base_boottime_ns);

    let OverviewErrors {
        highlights: error_log_highlights,
        total: total_error_logs,
        unique: unique_error_logs,
        noise_candidates,
    } = overview_highlights(reader.as_ref(), &frames, &crashes, params)?;

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
        logcat_completeness_detail: Some(summary.into()),
        total_error_logs,
        unique_error_logs,
        error_logs_truncated: unique_error_logs > error_log_highlights.len(),
        error_log_highlights,
        noise_candidates,
        unanchored_count: summary.unanchored_summary.included_count,
        unanchored_truncated_count: summary.unanchored_summary.truncated_count,
        app_logs: summary.app_logs.clone(),
    })
}

/// Reads only timing, geometry and interaction metadata. No decode or PNG cache write.
pub(super) fn inspect_metadata(
    catalog: &Catalog,
    path: &Path,
    source_id: Option<&str>,
    params: &MediaInspectFramesParams,
) -> Result<serde_json::Value, DaemonError> {
    use serde_json::json;
    let _lease = acquire_export_lease(catalog, source_id, std::process::id())?;
    let reader = open_bundle(path)?;
    let manifest = reader.manifest_summary();
    let id = source_id.unwrap_or(&manifest.bundle_id);
    let video = required_video_path(reader.as_ref(), id)?;
    let track = Mp4VideoTrack::open(&video)?;
    if track.frame_count() == 0 {
        return Err(DaemonError::NotFound(format!("{id}: no frames")));
    }
    let width = u32::from(track.width());
    let height = u32::from(track.height());
    let display = manifest.streams.video.as_ref();
    let dims = (
        params
            .display_width
            .or_else(|| display.and_then(|v| v.display_width)),
        params
            .display_height
            .or_else(|| display.and_then(|v| v.display_height)),
    );
    let mut taps = build_frame_tap_map(reader.as_ref(), width, height, dims)?;
    let indices = match params.mode {
        InspectFramesMode::ContactSheet => contact_indices(params, track.frame_count())?,
        InspectFramesMode::Single => vec![single_index(params)],
        InspectFramesMode::TapCrop => {
            let (i, x, y) = tap_target(reader.as_ref(), params, width, height, dims)?;
            taps.insert(i, (x, y));
            vec![i]
        }
    };
    let screens = screens_with_offsets(reader.as_ref(), &reader.display_frames()?)?;
    // The same media timeline `export.*` resolves `in_time_ns` / `out_time_ns` against.
    let (media_pts, _) = arb_media::trim::trim_timeline(&track)?;
    let mut frames = Vec::with_capacity(indices.len());
    for &i in &indices {
        let info = track.frame(i)?;
        let screen = screens.iter().find(|s| {
            s.start_frame_index
                .zip(s.end_frame_index)
                .is_some_and(|(a, b)| (a..=b).contains(&i))
        });
        let offset_ns = usize::try_from(i)
            .ok()
            .and_then(|k| media_pts.get(k).copied());
        frames.push(
            json!({"frame_index":i,"timestamp_ms":info.pts_ns/1_000_000,"screen":screen,
            "offset_ns":offset_ns,"offset_ms":offset_ns.map(|ns| ns / 1_000_000),
            "tap":taps.get(&i).map(|(x,y)|json!({"x":x,"y":y}))}),
        );
    }
    Ok(
        json!({"bundle_id":id,"mode":params.mode,"width":width,"height":height,"frame_indices_rendered":indices,"frames":frames}),
    )
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

    let video_path = required_video_path(reader.as_ref(), &bundle_id)?;
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

pub(super) fn query_log_response(
    reader: &dyn BundleReader,
    bucket: Option<RecordBucket>,
    params: &TimelineQueryParams,
    limit: usize,
) -> Result<crate::protocol::TimelineLogsResponse, DaemonError> {
    let page = query_logs_with_total(reader, bucket, params, limit)?;
    let records = reader.display_frames()?;
    let coordinates = LogCoordinates::new(&records, &reader.manifest_summary().epochs);
    let logs = page
        .logs
        .into_iter()
        .map(|record| coordinates.query_row(record))
        .collect::<Vec<_>>();
    Ok(crate::protocol::TimelineLogsResponse {
        unanchored_count: reader.count_logs(Some(RecordBucket::Unanchored), TimeRange::all())?,
        returned_count: logs.len(),
        logs,
        total_matched: page.total_matched,
        has_more: page.has_more,
        next_cursor: page
            .next_after_seq
            .filter(|_| page.has_more)
            .map(|s| s.to_string()),
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
    /// Exact across all pages when requested by the RPC.
    pub total_matched: usize,
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

// Immutable bundle identity and complete search conditions. Page size and cursor do not
// affect the total. Oversized one-off queries bypass the bounded process cache.
#[derive(Clone, PartialEq, Eq)]
struct LogCountKey {
    bundle: BundleLogCacheKey,
    bucket: Option<RecordBucket>,
    range: TimeRange,
    filters: [Option<String>; 4],
    whole_word: bool,
}

fn query_logs_with_total(
    reader: &dyn BundleReader,
    bucket: Option<RecordBucket>,
    params: &TimelineQueryParams,
    limit: usize,
) -> Result<FilteredLogPage, DaemonError> {
    static TOTALS: Mutex<VecDeque<(LogCountKey, usize)>> = Mutex::new(VecDeque::new());
    let filters = [&params.tag, &params.keyword, &params.level, &params.package];
    let cacheable = filters.iter().any(|v| v.is_some())
        && filters
            .iter()
            .filter_map(|v| v.as_ref())
            .map(String::len)
            .sum::<usize>()
            <= 4096;
    let key = if cacheable && let Some(bundle) = build_bundle_cache_key(reader) {
        Some(LogCountKey {
            bundle,
            bucket,
            range: resolve_log_query_time_range(reader, params)?,
            filters: filters.map(Clone::clone),
            whole_word: params.whole_word,
        })
    } else {
        None
    };
    let cached = key.as_ref().and_then(|key| {
        TOTALS
            .lock()
            .ok()?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, total)| *total)
    });
    if let Some(total) = cached {
        let mut page = scan_logs(reader, bucket, params, limit, CountMatches::PageOnly)?;
        page.total_matched = total;
        return Ok(page);
    }
    let page = scan_logs(reader, bucket, params, limit, CountMatches::EntireQuery)?;
    if let Some(key) = key
        && build_bundle_cache_key(reader).as_ref() == Some(&key.bundle)
        && let Ok(mut cache) = TOTALS.lock()
    {
        if cache.len() >= MAX_LOG_RESOLVER_CACHE_ENTRIES {
            cache.pop_front();
        }
        cache.push_back((key, page.total_matched));
    }
    Ok(page)
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
    whole_word: bool,
}

impl CompiledLogFilter<'_> {
    fn text_matches(&self, text: &str, needle: &str) -> bool {
        let text = text.to_ascii_lowercase();
        if !self.whole_word {
            return text.contains(needle);
        }
        let word = |c: char| c.is_alphanumeric() || c == '_';
        !needle.is_empty()
            && text.char_indices().any(|(i, _)| {
                text[i..].starts_with(needle)
                    && !text[..i].chars().next_back().is_some_and(word)
                    && !text[i + needle.len()..].chars().next().is_some_and(word)
            })
    }

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
                .is_some_and(|t| self.text_matches(t, want_tag));
            if !matched {
                return false;
            }
        }
        if let Some(ref want_kw) = self.keyword_lower {
            let in_msg = self.text_matches(&log.message, want_kw);
            let in_tag = log
                .tag
                .as_deref()
                .is_some_and(|t| self.text_matches(t, want_kw));
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
    let frames = reader.display_frames()?;
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
    scan_logs(reader, bucket, params, limit, CountMatches::PageOnly)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CountMatches {
    EntireQuery,
    PageOnly,
}

fn query_level(raw: Option<&str>) -> Result<Option<String>, DaemonError> {
    Ok(match raw {
        Some(raw) => {
            let norm = normalize_log_level(raw);
            if norm.is_empty() || raw.trim().eq_ignore_ascii_case("A") {
                return Err(DaemonError::InvalidParams(format!(
                    "unknown log level `{raw}`; expected V/D/I/W/E/F or VERBOSE/DEBUG/INFO/WARN/WARNING/ERROR/FATAL/ASSERT"
                )));
            }
            Some(norm.to_owned())
        }
        None => None,
    })
}

fn scan_logs(
    reader: &dyn BundleReader,
    bucket: Option<RecordBucket>,
    params: &TimelineQueryParams,
    limit: usize,
    count: CountMatches,
) -> Result<FilteredLogPage, DaemonError> {
    let count_all = count == CountMatches::EntireQuery;
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

    if !has_text_filter && !params.tail {
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
            total_matched: if count_all {
                reader.count_logs(bucket, range)?
            } else {
                candidate_rows_scanned
            },
            resolver_cache_hit: false,
        });
    }

    let level = query_level(params.level.as_deref())?;

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
        whole_word: params.whole_word,
        keyword_lower: params.keyword.as_deref().map(str::to_ascii_lowercase),
    };

    let batch_size = limit
        .saturating_mul(4)
        .clamp(MIN_FILTERED_LOG_BATCH_ROWS, MAX_FILTERED_LOG_BATCH_ROWS);
    let mut total_matched = 0;
    let mut cursor_seq = if count_all { None } else { params.after_seq };
    let mut matched = VecDeque::with_capacity(limit.min(MAX_FILTERED_LOG_BATCH_ROWS));
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
                total_matched += 1;
                if params.after_seq.is_some_and(|after| log.seq <= after) {
                    continue;
                }
                if params.tail && matched.len() == limit {
                    has_more |= matched.pop_front().is_some();
                }
                if matched.len() < limit {
                    matched.push_back(log);
                } else {
                    has_more = true;
                    if !count_all {
                        break;
                    }
                }
            }
        }
        if (has_more && !count_all && !params.tail) || batch_len < batch_size {
            break;
        }
    }

    let next_after_seq = matched.back().map(|r| r.seq).filter(|_| !params.tail);
    Ok(FilteredLogPage {
        logs: matched.into(),
        has_more,
        next_after_seq,
        candidate_rows_scanned,
        resolver_cache_hit,
        total_matched,
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
    let indices = contact_indices(params, track.frame_count())?;
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
    let idx = single_index(params);
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
    let (idx, tx, ty) = tap_target(reader, params, video_w, video_h, display_dims)?;
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

fn contact_indices(
    params: &MediaInspectFramesParams,
    frame_count: u64,
) -> Result<Vec<u64>, DaemonError> {
    Ok(if let Some(explicit) = &params.frame_indices {
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
    })
}

fn single_index(params: &MediaInspectFramesParams) -> u64 {
    params
        .frame_index
        .or_else(|| {
            params
                .frame_indices
                .as_ref()
                .and_then(|v| v.first().copied())
        })
        .unwrap_or(0)
}

fn tap_target(
    reader: &dyn BundleReader,
    params: &MediaInspectFramesParams,
    video_w: u32,
    video_h: u32,
    display_dims: (Option<u32>, Option<u32>),
) -> Result<(u64, u32, u32), DaemonError> {
    Ok(if let Some(tap_idx) = params.tap_index {
        let frames = reader.display_frames()?;
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
    })
}

fn build_frame_tap_map(
    reader: &dyn BundleReader,
    video_w: u32,
    video_h: u32,
    display_dims: (Option<u32>, Option<u32>),
) -> Result<BTreeMap<u64, (u32, u32)>, DaemonError> {
    let frames = reader.display_frames()?;
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

struct LogCoordinates<'a> {
    frames: BTreeMap<&'a str, (Vec<i64>, Vec<u64>)>,
    epochs: &'a [arb_core::bundle::EpochSpan],
}

impl<'a> LogCoordinates<'a> {
    fn new(records: &'a [FrameRecord], epochs: &'a [arb_core::bundle::EpochSpan]) -> Self {
        let mut frames = BTreeMap::<&str, (Vec<i64>, Vec<u64>)>::new();
        for frame in records {
            if let Some(t) = frame_boottime_ns(frame) {
                let (times, indices) = frames.entry(frame.boot_id.as_str()).or_default();
                times.push(times.last().map_or(t, |prev| t.max(*prev)));
                indices.push(frame.frame_index);
            }
        }
        Self { frames, epochs }
    }

    fn query_row(&self, record: LogRecord) -> crate::protocol::QueryLogRow {
        let timestamp_utc = record
            .header_epoch_ns
            .map(crate::time_util::format_epoch_ns_iso8601);
        let (offset_ms, frame_index) = self.locate(&record);
        crate::protocol::QueryLogRow {
            record,
            timestamp_utc,
            offset_ms,
            frame_index,
        }
    }

    fn locate(&self, log: &LogRecord) -> (Option<i64>, Option<u64>) {
        let (
            TimeAlignment::Anchored {
                boot_id,
                boottime_ns,
                ..
            },
            RecordBucket::Main,
        ) = (&log.alignment, log.bucket)
        else {
            return (None, None);
        };
        let table = self.frames.get(boot_id.as_str());
        let start = table.and_then(|(p, _)| p.first().copied()).or_else(|| {
            self.epochs
                .iter()
                .find(|e| e.boot_id == *boot_id)
                .map(|e| e.start_boottime_ns)
        });
        // Before the first frame's display start nothing is on screen yet: no frame is
        // fabricated (the offset stays negative relative to the video start).
        let frame = table.and_then(|(p, ids)| {
            p.first()
                .is_some_and(|first| *boottime_ns >= *first)
                .then(|| arb_media::timeline::frame_at_time_ns(p, *boottime_ns).map(|i| ids[i]))
                .flatten()
        });
        (
            start.map(|s| boottime_ns.saturating_sub(s) / 1_000_000),
            frame,
        )
    }
}

/// Error groups and advisory noise tags drawn from one filtered E/F row set.
struct OverviewErrors {
    highlights: Vec<ErrorLogHighlight>,
    total: usize,
    unique: usize,
    noise_candidates: Vec<NoiseCandidate>,
}

/// Minimum filtered `E` rows before a tag can be suggested as noise.
const NOISE_MIN_ROWS: usize = 5;
/// Row count that alone marks a tag as repetitive.
const NOISE_REPEAT_ROWS: usize = 10;
/// Maximum suggested tags.
const MAX_NOISE_CANDIDATES: usize = 5;

fn overview_highlights(
    reader: &dyn BundleReader,
    frames: &[FrameRecord],
    crashes: &[arb_core::analytics::CrashMarker],
    params: &crate::protocol::TimelineOverviewParams,
) -> Result<OverviewErrors, DaemonError> {
    let main_logs = reader.query_logs(Some(RecordBucket::Main), TimeRange::all(), None)?;
    let max_errors = params
        .max_error_logs
        .unwrap_or(DEFAULT_OVERVIEW_ERROR_LOGS)
        .min(MAX_OVERVIEW_ERROR_LOGS);
    let package = params
        .package
        .as_deref()
        .map(|name| load_or_build_package_resolver(reader).map(|(resolver, _)| (name, resolver)))
        .transpose()?;
    let filter = CompiledLogFilter {
        package,
        tag_lower: params.tag.as_ref().map(|s| s.to_ascii_lowercase()),
        level: None,
        keyword_lower: None,
        whole_word: false,
    };
    let coordinates = LogCoordinates::new(frames, &reader.manifest_summary().epochs);
    let (highlights, total, unique) = build_error_log_highlights(
        &main_logs,
        &coordinates,
        &filter,
        &params.exclude_tags,
        max_errors,
    );
    // Computed from the same rows, before the display cap, and never fed back into them.
    let protected = fatal_tags(&main_logs, crashes);
    let noise_candidates = noise_candidates(
        main_logs.iter().filter_map(|log| {
            error_level(log, &filter, &params.exclude_tags)
                .map(|level| (level, log.tag.as_deref(), log.message.as_str()))
        }),
        &protected,
    );
    Ok(OverviewErrors {
        highlights,
        total,
        unique,
        noise_candidates,
    })
}

/// Normalized `E`/`F` level of a main log row that passes the overview filters.
fn error_level(
    log: &LogRecord,
    filter: &CompiledLogFilter<'_>,
    exclude_tags: &[String],
) -> Option<&'static str> {
    let level = match normalize_log_level(log.level.as_deref().unwrap_or("")) {
        "E" => "E",
        "F" => "F",
        _ => return None,
    };
    let excluded = log
        .tag
        .as_ref()
        .is_some_and(|tag| exclude_tags.iter().any(|x| x.eq_ignore_ascii_case(tag)));
    (!excluded && filter.matches(log)).then_some(level)
}

/// Lower-cased tags that must never be suggested: any tag logged at `F` anywhere in the main
/// timeline, and the tag of every crash marker's source row.
fn fatal_tags(
    main_logs: &[LogRecord],
    crashes: &[arb_core::analytics::CrashMarker],
) -> std::collections::BTreeSet<String> {
    let crash_rows: std::collections::BTreeSet<u64> =
        crashes.iter().map(|c| c.source_log_seq).collect();
    main_logs
        .iter()
        .filter(|log| {
            normalize_log_level(log.level.as_deref().unwrap_or("")) == "F"
                || crash_rows.contains(&log.seq)
        })
        .filter_map(|log| log.tag.as_deref().map(str::to_ascii_lowercase))
        .collect()
}

/// Deterministic noise suggestion over filtered `(level, tag, message)` error rows.
///
/// A tag qualifies when it has no `F` row (here or in `protected`, lower-cased), at least
/// [`NOISE_MIN_ROWS`] `E` rows, and repeats: [`NOISE_REPEAT_ROWS`] rows or at least two rows
/// per distinct message. Ordered by count descending then tag, at most
/// [`MAX_NOISE_CANDIDATES`]. Pure: it reads the rows and changes nothing.
fn noise_candidates<'a>(
    rows: impl IntoIterator<Item = (&'a str, Option<&'a str>, &'a str)>,
    protected: &std::collections::BTreeSet<String>,
) -> Vec<NoiseCandidate> {
    let mut fatal = protected.clone();
    let mut by_tag: BTreeMap<&str, BTreeMap<&str, usize>> = BTreeMap::new();
    for (level, tag, message) in rows {
        let Some(tag) = tag else { continue };
        if level == "F" {
            fatal.insert(tag.to_ascii_lowercase());
        } else {
            *by_tag.entry(tag).or_default().entry(message).or_default() += 1;
        }
    }
    let mut out: Vec<NoiseCandidate> = by_tag
        .into_iter()
        .filter(|(tag, _)| !fatal.contains(&tag.to_ascii_lowercase()))
        .filter_map(|(tag, messages)| {
            let total_count: usize = messages.values().sum();
            let unique_messages = messages.len();
            let repeats = total_count >= NOISE_REPEAT_ROWS
                || total_count >= unique_messages.saturating_mul(2);
            let (sample, _) = messages
                .iter()
                .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))?;
            (total_count >= NOISE_MIN_ROWS && repeats).then(|| NoiseCandidate {
                tag: tag.to_owned(),
                total_count,
                unique_messages,
                sample_message: truncate_chars(sample, MAX_ERROR_LOG_MESSAGE_CHARS),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.total_count
            .cmp(&a.total_count)
            .then_with(|| a.tag.cmp(&b.tag))
    });
    out.truncate(MAX_NOISE_CANDIDATES);
    out
}

fn build_error_log_highlights(
    main_logs: &[LogRecord],
    coordinates: &LogCoordinates<'_>,
    filter: &CompiledLogFilter<'_>,
    exclude_tags: &[String],
    max_errors: usize,
) -> (Vec<ErrorLogHighlight>, usize, usize) {
    let mut groups = std::collections::HashMap::new();
    let mut out: Vec<ErrorLogHighlight> = Vec::with_capacity(max_errors);
    let mut total = 0;
    for log in main_logs.iter().rev() {
        let Some(level) = error_level(log, filter, exclude_tags) else {
            continue;
        };
        total += 1;
        let signature = (level, log.tag.as_deref(), log.message.as_str());
        let slot = match groups.entry(signature) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                if let Some(index) = *entry.get() {
                    let group: &mut ErrorLogHighlight = &mut out[index];
                    group.repeat_count += 1;
                    group.first_offset_ms = coordinates.locate(log).0;
                }
                continue;
            }
            std::collections::hash_map::Entry::Vacant(entry) => entry,
        };
        if out.len() == max_errors {
            slot.insert(None);
            continue;
        }
        slot.insert(Some(out.len()));
        let (t_ms, frame_index_hint) = coordinates.locate(log);
        out.push(ErrorLogHighlight {
            repeat_count: 1,
            first_offset_ms: t_ms,
            last_offset_ms: t_ms,
            timestamp_utc: log
                .header_epoch_ns
                .map(crate::time_util::format_epoch_ns_iso8601),
            seq: log.seq,
            level: level.to_owned(),
            frame_index_hint,
            t_ms,
            tag: log.tag.clone(),
            message: truncate_chars(&log.message, MAX_ERROR_LOG_MESSAGE_CHARS),
        });
    }
    out.reverse();
    (out, total, groups.len())
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

/// Display start of a frame read through `BundleReader::display_frames` (single time origin).
fn frame_boottime_ns(frame: &FrameRecord) -> Option<i64> {
    arb_core::bundle::frame_display_start_ns(frame, None)
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

fn tap_boot_id(tap: &TapRecord) -> Option<&str> {
    tap.boot_id.as_deref().or(match &tap.alignment {
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

/// Screen spans with offsets from their own boot's origin, the same origin log rows use
/// (first displayed frame of that boot, else that boot's epoch start). Boottime restarts at each
/// boot, so with several boots a span is placed only when its first frame names the boot;
/// otherwise its offsets stay `None` rather than being measured against another boot.
pub(super) fn screens_with_offsets(
    reader: &dyn BundleReader,
    frames: &[FrameRecord],
) -> Result<Vec<arb_core::analytics::ScreenSpan>, DaemonError> {
    let mut screens = reader.screens()?;
    place_screens(&mut screens, frames, &reader.manifest_summary().epochs);
    Ok(screens)
}

/// Pure placement used by [`screens_with_offsets`].
fn place_screens(
    screens: &mut [arb_core::analytics::ScreenSpan],
    frames: &[FrameRecord],
    epochs: &[arb_core::bundle::EpochSpan],
) {
    let boots: std::collections::BTreeSet<&str> = frames
        .iter()
        .map(|f| f.boot_id.as_str())
        .chain(epochs.iter().map(|e| e.boot_id.as_str()))
        .collect();
    let single = (boots.len() <= 1)
        .then(|| first_bundle_boottime_ns(frames, epochs))
        .flatten();
    for span in screens {
        let origin = if boots.len() <= 1 {
            single
        } else {
            span.start_frame_index
                .and_then(|i| frames.iter().find(|f| f.frame_index == i))
                .and_then(|f| boot_origin_ns(frames, epochs, &f.boot_id))
        };
        span.start_offset_ms = origin.map(|o| span.start_ns.saturating_sub(o) / 1_000_000);
        span.end_offset_ms = origin.map(|o| span.end_ns.saturating_sub(o) / 1_000_000);
    }
}

/// Origin of one boot on the shared display timeline (see [`LogCoordinates::locate`]).
fn boot_origin_ns(
    frames: &[FrameRecord],
    epochs: &[arb_core::bundle::EpochSpan],
    boot_id: &str,
) -> Option<i64> {
    frames
        .iter()
        .filter(|f| f.boot_id == boot_id)
        .find_map(frame_boottime_ns)
        .or_else(|| {
            epochs
                .iter()
                .find(|e| e.boot_id == boot_id)
                .map(|e| e.start_boottime_ns)
        })
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

pub(super) fn catalog_summary(
    reader: &dyn BundleReader,
) -> Result<crate::protocol::CatalogSummary, DaemonError> {
    catalog_summary_with_logs(reader, &reader.log_statistics()?)
}

pub(super) fn catalog_summary_with_logs(
    reader: &dyn BundleReader,
    logs: &arb_storage::bundle_io::LogStatistics,
) -> Result<crate::protocol::CatalogSummary, DaemonError> {
    let manifest = reader.manifest_summary();
    let video = reader.video_path();
    let duration_ms = video
        .as_ref()
        .map(Mp4VideoTrack::open)
        .transpose()?
        .map(|t| {
            u64::try_from(u128::from(t.duration_ticks()) * 1000 / u128::from(t.timescale()))
                .unwrap_or(u64::MAX)
        });
    let frame_count = if let Some(meta) = &manifest.streams.video {
        meta.frame_count
    } else if reader.is_legacy() {
        u64::try_from(reader.frames()?.len()).unwrap_or(u64::MAX)
    } else {
        0
    };
    let kind = if matches!(
        manifest.provenance,
        arb_core::bundle::Provenance::Trimmed { .. }
    ) {
        "trimmed"
    } else if manifest.bundle_kind == arb_core::bundle::BundleKind::LogsOnly {
        "logs_only"
    } else {
        "full"
    };
    Ok(crate::protocol::CatalogSummary {
        bundle_kind: Some(kind.to_owned()),
        duration_ms: Some(duration_ms.unwrap_or(logs.duration_ms)),
        frame_count: Some(frame_count),
        has_video: Some(video.is_some()),
        has_audio: Some(reader.audio_path().is_some()),
        log_count: Some(logs.count),
        crash_marker_count: Some(reader.crashes()?.len()),
        memo: manifest.manifest_v2.as_ref().and_then(|m| m.memo.clone()),
    })
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
    use super::{
        LogRecord, NoiseCandidate, RecordBucket, base64_encode, mapped_tap, prune_cache_dir,
    };

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

    proptest::proptest! {
        #[test]
        fn query_and_highlights_agree_at_vfr_boundaries(
            gaps in proptest::collection::vec(1_i64..90_000_000, 1..50), arbitrary_time in -100_i64..5_000_000_000
        ) {
            use arb_core::bundle::{FrameRecord, LogRecord, RecordBucket};
            use arb_core::time::{TimeAlignment,TimestampSemantics};
            let anchor = |ns| TimeAlignment::Anchored {boot_id:"boot".into(), boottime_ns:ns, half_width_ns:0, semantics:TimestampSemantics::LogcatDeviceReadBoottime};
            let mut pts = vec![0];
            for gap in gaps { pts.push(pts.last().unwrap()+gap); }
            let frames = pts.iter().enumerate().map(|(i,&ns)| FrameRecord {
                frame_index:u64::try_from(i).unwrap(), boot_id:"boot".into(), pts_device_mono_ns:ns, pc_ingest_cont_ns:ns,
                alignment:anchor(ns), is_keyframe:i==0,gop_id:0, mp4_sample_offset:0,mp4_sample_size:1,
            }).collect::<Vec<_>>();
            let samples = pts.iter().flat_map(|t|[t-1,*t,t+1]).chain([arbitrary_time]);
            let logs = samples.enumerate().map(|(i,t)| LogRecord {
                seq:u64::try_from(i).unwrap(),boot_id:Some("boot".into()),header_epoch_ns:Some(t), device_read_boottime_ns:Some(t),
                pc_ingest_cont_ns:t,alignment:anchor(t),bucket:RecordBucket::Main,level:Some("E".into()),tag:None,
                pid:None,tid:None,uid:None,package_name:None,message:i.to_string(),
            }).collect::<Vec<_>>();
            let coordinates=super::LogCoordinates::new(&frames,&[]);
            let filter=super::CompiledLogFilter{package:None,level:None,tag_lower:None,keyword_lower:None,whole_word:false};
            // Repeated system errors consume one group, even when the chosen limit is smaller than the burst.
            let mut burst = vec![logs[0].clone()];
            for seq in 1..=43 {
                let mut row = logs[1].clone();
                row.message = "repeated".into();
                row.seq = seq;
                burst.push(row);
            }
            let (groups, raw, distinct) = super::build_error_log_highlights(&burst, &coordinates, &filter, &[], 2);
            proptest::prop_assert_eq!((raw, distinct), (44,2));
            proptest::prop_assert_eq!(groups[1].repeat_count,43);
            proptest::prop_assert_eq!(groups[1].seq,43);
            let (highlights,total,_)=super::build_error_log_highlights(&logs,&coordinates,&filter,&[],logs.len());
            proptest::prop_assert_eq!(total,logs.len());
            for (log,h) in logs.into_iter().zip(highlights) {
                // Before the first displayed frame nothing is on screen: no frame (#564).
                let expected = pts.iter().rposition(|p| *p <= log.pc_ingest_cont_ns);
                let row=coordinates.query_row(log);
                proptest::prop_assert_eq!(row.frame_index,expected.map(|i| u64::try_from(i).unwrap()));
                proptest::prop_assert_eq!(row.frame_index,h.frame_index_hint);
                proptest::prop_assert_eq!(row.offset_ms,h.t_ms);
            }
        }
    }

    fn error_row(seq: u64, level: &str, tag: Option<&str>, message: &str) -> LogRecord {
        use arb_core::time::{TimeAlignment, TimestampSemantics};
        LogRecord {
            seq,
            boot_id: Some("boot".into()),
            header_epoch_ns: Some(1),
            device_read_boottime_ns: Some(1),
            pc_ingest_cont_ns: 1,
            alignment: TimeAlignment::Anchored {
                boot_id: "boot".into(),
                boottime_ns: i64::try_from(seq).unwrap(),
                half_width_ns: 0,
                semantics: TimestampSemantics::LogcatDeviceReadBoottime,
            },
            bucket: RecordBucket::Main,
            level: Some(level.into()),
            tag: tag.map(Into::into),
            pid: None,
            tid: None,
            uid: None,
            package_name: None,
            message: message.into(),
        }
    }

    fn no_filter() -> super::CompiledLogFilter<'static> {
        super::CompiledLogFilter {
            package: None,
            level: None,
            tag_lower: None,
            keyword_lower: None,
            whole_word: false,
        }
    }

    fn candidates_for(logs: &[LogRecord], exclude: &[String]) -> Vec<NoiseCandidate> {
        let filter = no_filter();
        super::noise_candidates(
            logs.iter().filter_map(|log| {
                super::error_level(log, &filter, exclude)
                    .map(|level| (level, log.tag.as_deref(), log.message.as_str()))
            }),
            &super::fatal_tags(logs, &[]),
        )
    }

    #[test]
    fn noise_candidates_follow_the_published_thresholds_and_order() {
        let mut logs = Vec::new();
        let mut push = |level: &str, tag: &str, message: String| {
            let seq = u64::try_from(logs.len()).unwrap();
            logs.push(error_row(seq, level, Some(tag), &message));
        };
        // 4 identical rows: below the minimum.
        (0..4).for_each(|_| push("E", "Few", "same".into()));
        // 5 rows, 2 messages: 5 >= 2 * 2 repeats.
        (0..5).for_each(|i| push("E", "Pair", format!("m{}", i % 2)));
        // 9 distinct rows: neither 10 rows nor two per message.
        (0..9).for_each(|i| push("E", "Unique", format!("u{i}")));
        // 10 distinct rows: the row count alone qualifies.
        (0..10).for_each(|i| push("E", "Ten", format!("t{i}")));
        // Repeating, but the same tag also logged a fatal row (any case).
        (0..20).for_each(|_| push("E", "Vendor", "spam".into()));
        push("F", "vendor", "abort".into());
        // Warnings never count.
        (0..30).for_each(|_| push("W", "Warn", "w".into()));
        let got = candidates_for(&logs, &[]);
        let tags: Vec<_> = got.iter().map(|c| c.tag.as_str()).collect();
        assert_eq!(tags, ["Ten", "Pair"]);
        assert_eq!(
            got[1],
            NoiseCandidate {
                tag: "Pair".into(),
                total_count: 5,
                unique_messages: 2,
                sample_message: "m0".into(),
            }
        );
        // A tag the caller already excludes is not suggested again.
        let excluded = candidates_for(&logs, &["ten".into()]);
        assert_eq!(
            excluded.iter().map(|c| c.tag.as_str()).collect::<Vec<_>>(),
            ["Pair"]
        );
    }

    #[test]
    fn crash_source_tags_and_limit_are_enforced() {
        let mut logs = Vec::new();
        for t in 0..7_u64 {
            for i in 0..(10 + t) {
                logs.push(error_row(t * 100 + i, "E", Some(&format!("T{t}")), "x"));
            }
        }
        let crash = arb_core::analytics::CrashMarker {
            boottime_ns: None,
            frame_index_hint: None,
            process_name: "app".into(),
            package_name: "app".into(),
            pid: None,
            exception_class: "E".into(),
            message: "boom".into(),
            source_log_seq: 600,
        };
        let filter = no_filter();
        let got = super::noise_candidates(
            logs.iter().filter_map(|log| {
                super::error_level(log, &filter, &[])
                    .map(|level| (level, log.tag.as_deref(), log.message.as_str()))
            }),
            &super::fatal_tags(&logs, std::slice::from_ref(&crash)),
        );
        let tags: Vec<_> = got.iter().map(|c| c.tag.as_str()).collect();
        // T6 owns the crash row; the rest are ordered by count and capped at five.
        assert_eq!(tags, ["T5", "T4", "T3", "T2", "T1"]);
    }

    proptest::proptest! {
        /// Suggestions never remove or alter evidence: highlights and totals are computed the
        /// same way with or without them, and every suggestion is an exact, non-fatal count.
        #[test]
        fn noise_candidates_never_hide_evidence(
            rows in proptest::collection::vec((0_usize..3, 0_usize..5, 0_usize..3), 0..120),
            exclude in proptest::collection::vec(0_usize..5, 0..3),
        ) {
            const LEVELS: [&str; 3] = ["E", "F", "W"];
            const TAGS: [Option<&str>; 5] = [Some("A"), Some("B"), Some("c"), Some("C"), None];
            let logs: Vec<_> = rows.iter().enumerate().map(|(i, (l, t, m))| {
                error_row(u64::try_from(i).unwrap(), LEVELS[*l], TAGS[*t], &format!("m{m}"))
            }).collect();
            let exclude: Vec<String> =
                exclude.iter().filter_map(|t| TAGS[*t].map(str::to_owned)).collect();
            let coordinates = super::LogCoordinates::new(&[], &[]);
            let filter = no_filter();
            let before = super::build_error_log_highlights(&logs, &coordinates, &filter, &exclude, 3);
            let got = candidates_for(&logs, &exclude);
            let after = super::build_error_log_highlights(&logs, &coordinates, &filter, &exclude, 3);
            proptest::prop_assert_eq!(&before, &after);
            let matched = logs.iter().filter(|l| super::error_level(l, &filter, &exclude).is_some()).count();
            proptest::prop_assert_eq!(before.1, matched);
            proptest::prop_assert!(got.len() <= 5);
            for pair in got.windows(2) {
                proptest::prop_assert!((std::cmp::Reverse(pair[0].total_count), &pair[0].tag)
                    < (std::cmp::Reverse(pair[1].total_count), &pair[1].tag));
            }
            for c in &got {
                let lower = c.tag.to_ascii_lowercase();
                proptest::prop_assert!(!exclude.iter().any(|x| x.eq_ignore_ascii_case(&c.tag)));
                proptest::prop_assert!(!logs.iter().any(|l| l.level.as_deref() == Some("F")
                    && l.tag.as_deref().is_some_and(|t| t.to_ascii_lowercase() == lower)));
                let rows = logs.iter().filter(|l| l.level.as_deref() == Some("E")
                    && l.tag.as_deref() == Some(c.tag.as_str())).count();
                proptest::prop_assert_eq!(c.total_count, rows);
                proptest::prop_assert!(c.total_count >= 5);
            }
        }
    }

    #[test]
    fn screen_offsets_use_their_own_boot_origin() {
        use arb_core::analytics::ScreenSpan;
        use arb_core::bundle::{EpochSpan, FrameRecord};
        use arb_core::time::{TimeAlignment, TimestampSemantics};
        let frame = |i: u64, boot: &str, ns: i64| FrameRecord {
            frame_index: i,
            boot_id: boot.into(),
            pts_device_mono_ns: ns,
            pc_ingest_cont_ns: ns,
            alignment: TimeAlignment::Anchored {
                boot_id: boot.into(),
                boottime_ns: ns,
                half_width_ns: 0,
                semantics: TimestampSemantics::VideoEncoderInputEstimate,
            },
            is_keyframe: true,
            gop_id: 0,
            mp4_sample_offset: 0,
            mp4_sample_size: 1,
        };
        let span = |start_ns: i64, start_frame: Option<u64>| ScreenSpan {
            start_ns,
            end_ns: start_ns + 2_000_000_000,
            start_frame_index: start_frame,
            end_frame_index: start_frame,
            package_name: "p".into(),
            activity_name: "p.A".into(),
            short_name: "A".into(),
            start_offset_ms: None,
            end_offset_ms: None,
        };
        let epoch = |boot: &str, start| EpochSpan {
            boot_id: boot.into(),
            start_boottime_ns: start,
            end_boottime_ns: start + 10_000_000_000,
        };
        // Boot b restarts its clock: its first frame at 5 s is offset 0, not -95 s.
        let frames = [frame(0, "a", 100_000_000_000), frame(1, "b", 5_000_000_000)];
        let epochs = [epoch("a", 100_000_000_000), epoch("b", 5_000_000_000)];
        let mut screens = [
            span(100_500_000_000, Some(0)),
            span(5_000_000_000, Some(1)),
            span(6_000_000_000, None),
        ];
        super::place_screens(&mut screens, &frames, &epochs);
        let offsets: Vec<_> = screens
            .iter()
            .map(|s| (s.start_offset_ms, s.end_offset_ms))
            .collect();
        assert_eq!(
            offsets,
            [
                (Some(500), Some(2_500)),
                (Some(0), Some(2_000)),
                (None, None)
            ]
        );
        // A single boot needs no frame to be placed.
        let mut one = [span(101_000_000_000, None)];
        super::place_screens(&mut one, &frames[..1], &epochs[..1]);
        assert_eq!(one[0].start_offset_ms, Some(1_000));
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
