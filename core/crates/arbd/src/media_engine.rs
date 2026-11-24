//! High-bandwidth SHM viewport playback engine and non-destructive trim/PNG exporters.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use arb_core::bundle::{
    BundleKind, DeviceInfo, EpochSpan, FrameRecord, Provenance, RecordBucket, RequestedWindow,
    StreamStatus, StreamsSummary, UnanchoredSummary, VideoStreamMeta,
};
use arb_core::selection::{SelectionPolicy, WindowSpec, select_for_trim_with_truncated};
use arb_core::time::{DiscontinuityEvent, TimeAlignment, TimestampSemantics, UncertaintyReason};
use arb_media::MediaError;
use arb_media::audio::trim_audio_m4a;
use arb_media::burn_in::{BurnInDisplayGeometry, BurnInTouchEvent, burn_in_taps_video};
use arb_media::codec::{PixelFormat as MediaPixelFormat, VideoCodecBackend, default_codec_backend};
use arb_media::mp4::{Mp4VideoTrack, inspect_mp4_atoms};
use arb_media::mux_av::mux_share_mp4;
use arb_media::playback::{
    FilmstripCache, FilmstripConfig, FilmstripOrigin, GopLruCache, ScrubController,
};
use arb_media::trim::{PngExportMetadata, export_frame_png_with_metadata, trim_video};
use arb_os::shm::{FrameDescriptor, PixelFormat as ShmPixelFormat, ShmFrameRingWriter};
use arb_storage::bundle_io::{
    BundleReader, ClockAnchorRecord, MediaPayload, SHARE_MP4_FILE, SHARE_TAPS_MP4_FILE,
    ShareMp4Sidecar, TimeRange, V2BundleDraft, V2BundleReader, V2BundleWriter, open_bundle,
};
use arb_storage::catalog::{Catalog, CatalogFormatVersion, LeaseHolderKind, NewBundleEntry};

use crate::capture::FaultInjection;
use crate::error::DaemonError;
use crate::protocol::{
    EpochFrameRange, ExportPngParams, ExportPngResponse, ExportTrimParams, ExportTrimResponse,
    FilmstripEntryMeta, MAX_VIEWPORTS, MediaFilmstripParams, MediaFilmstripResponse,
    MediaFrameRequestParams, MediaOpenViewportResponse, MediaScrubResponse, MediaSeekFrameResponse,
};
use crate::time_util::{next_id, utc_now_iso8601};

const DEFAULT_GOP_CACHE_GOPS: usize = 4;
const DEFAULT_GOP_CACHE_BYTES: usize = 128 * 1024 * 1024;
const MIN_SHM_SLOT_CAPACITY: usize = 4096;

/// The maximum JSON representation of 100,000 signed 64-bit timestamps is under 2.2 MiB.
fn viewport_timestamps(mut pts: impl ExactSizeIterator<Item = i64>) -> Vec<i64> {
    if pts.len() > 100_000 {
        return Vec::new();
    }
    let Some(origin) = pts.next() else {
        return Vec::new();
    };
    std::iter::once(0)
        .chain(pts.map(|t| t.saturating_sub(origin)))
        .collect()
}

#[derive(Debug)]
struct OpenViewport {
    bundle_id: String,
    shm_path: PathBuf,
    track: Mp4VideoTrack,
    frame_boottimes: Vec<i64>,
    scrub: ScrubController,
    shm_writer: ShmFrameRingWriter,
}

impl Drop for OpenViewport {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.shm_path);
    }
}

/// Manages open SHM viewports and the shared GPL-free [`VideoCodecBackend`].
#[derive(Debug)]
pub struct MediaEngine {
    cache_dir: PathBuf,
    shm_dir: PathBuf,
    backend: Box<dyn VideoCodecBackend>,
    viewports: Mutex<BTreeMap<String, Arc<Mutex<OpenViewport>>>>,
}

impl MediaEngine {
    /// Creates a new media engine storing external filmstrip caches under `cache_dir`
    /// and SHM viewport rings under `shm_dir`.
    #[must_use]
    pub fn new(cache_dir: PathBuf, shm_dir: PathBuf) -> Self {
        Self {
            cache_dir,
            shm_dir,
            backend: default_codec_backend(),
            viewports: Mutex::default(),
        }
    }

    /// Returns the number of currently open viewports.
    #[must_use]
    pub fn active_viewport_count(&self) -> usize {
        self.viewports
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Removes stale `*.shm` viewport files left in `shm_dir` by a crashed daemon (R3 M7).
    ///
    /// Must only run while the single-instance lock is held (no other daemon can own them).
    /// Returns the number of files removed.
    pub fn sweep_stale_shm_files(shm_dir: &Path) -> usize {
        let Ok(entries) = fs::read_dir(shm_dir) else {
            return 0;
        };
        let mut removed = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "shm")
                && path.is_file()
                && fs::remove_file(&path).is_ok()
            {
                removed += 1;
            }
        }
        removed
    }

    /// Opens a triple-buffered SHM viewport for `bundle_dir` and returns its descriptor.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::Conflict`] when [`MAX_VIEWPORTS`] viewports are already open
    /// (R3 M7), [`DaemonError::NotFound`] if the bundle has no video track, or OS/media errors.
    pub fn open_viewport(
        &self,
        bundle_dir: &Path,
        bundle_id_override: Option<&str>,
    ) -> Result<MediaOpenViewportResponse, DaemonError> {
        if self.active_viewport_count() >= MAX_VIEWPORTS {
            return Err(DaemonError::Conflict(format!(
                "too many open viewports (max {MAX_VIEWPORTS}); close one with media.close_viewport"
            )));
        }
        let reader = open_bundle(bundle_dir)?;
        let summary = reader.manifest_summary();
        let bundle_id = bundle_id_override.unwrap_or(&summary.bundle_id).to_owned();
        let video_path = reader.video_path().ok_or_else(|| {
            DaemonError::NotFound(format!("bundle `{bundle_id}` has no video.mp4 track"))
        })?;
        let track = Mp4VideoTrack::open(&video_path)?;
        let frames = reader.frames().unwrap_or_default();
        let frame_boottimes: Vec<i64> = frames
            .iter()
            .map(|f| match &f.alignment {
                TimeAlignment::Anchored { boottime_ns, .. } => *boottime_ns,
                TimeAlignment::Bounded {
                    min_boottime_ns, ..
                } => *min_boottime_ns,
                TimeAlignment::Unanchored {
                    pc_ingest_cont_ns, ..
                } => *pc_ingest_cont_ns,
                _ => f.pts_device_mono_ns,
            })
            .collect();

        let width = u32::from(track.width());
        let height = u32::from(track.height());
        let stride = width.saturating_mul(4);
        let slot_capacity = usize::try_from(stride)
            .unwrap_or(MIN_SHM_SLOT_CAPACITY)
            .saturating_mul(usize::try_from(height).unwrap_or(1))
            .max(MIN_SHM_SLOT_CAPACITY);

        let viewport_id = next_id("vp");
        let shm_path = self.shm_dir.join(format!("{viewport_id}.shm"));
        let shm_writer = ShmFrameRingWriter::create(&shm_path, slot_capacity)?;

        let frame_count = track.frame_count();
        let keyframe_count = track.keyframes().len();
        let frame_pts_ns = viewport_timestamps(track.frames().iter().map(|f| f.pts_ns));
        let scrub = ScrubController::new(
            GopLruCache::new(DEFAULT_GOP_CACHE_GOPS, DEFAULT_GOP_CACHE_BYTES),
            None,
        );

        let viewport = Arc::new(Mutex::new(OpenViewport {
            bundle_id: bundle_id.clone(),
            shm_path: shm_path.clone(),
            track,
            frame_boottimes,
            scrub,
            shm_writer,
        }));
        let mut registry = self
            .viewports
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if registry.len() >= MAX_VIEWPORTS {
            return Err(DaemonError::Conflict(format!(
                "too many open viewports (max {MAX_VIEWPORTS}); close one with media.close_viewport"
            )));
        }
        registry.insert(viewport_id.clone(), viewport);

        Ok(MediaOpenViewportResponse {
            frame_pts_ns,
            viewport_id,
            bundle_id,
            shm_path,
            width,
            height,
            stride,
            pixel_format: "rgba8".to_owned(),
            slot_capacity,
            frame_count,
            keyframe_count,
        })
    }

    /// Generates or loads external `<cache_dir>/<bundle_id>/filmstrip.bin` and attaches it
    /// to the open viewport's [`ScrubController`].
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::NotFound`] if `viewport_id` is unknown, or media errors.
    pub fn filmstrip(
        &self,
        params: &MediaFilmstripParams,
    ) -> Result<MediaFilmstripResponse, DaemonError> {
        self.filmstrip_with(params, |vp, cfg| {
            FilmstripCache::load_or_generate(
                &self.cache_dir,
                &vp.bundle_id,
                &vp.track,
                cfg,
                self.backend.as_ref(),
            )
        })
    }

    fn filmstrip_with(
        &self,
        params: &MediaFilmstripParams,
        load: impl FnOnce(
            &OpenViewport,
            FilmstripConfig,
        ) -> Result<arb_media::playback::FilmstripLoadResult, MediaError>,
    ) -> Result<MediaFilmstripResponse, DaemonError> {
        let start = Instant::now();
        let viewport = self.viewport(&params.viewport_id)?;
        let mut viewport_guard = viewport
            .lock()
            .map_err(|_| DaemonError::Internal("viewport lock poisoned".into()))?;
        let vp = &mut *viewport_guard;

        let mut cfg = FilmstripConfig::default();
        if let Some(mw) = params.max_width {
            cfg.max_width = mw;
        }
        if let Some(mh) = params.max_height {
            cfg.max_height = mh;
        }

        let loaded = load(vp, cfg)?;

        let entries: Vec<FilmstripEntryMeta> = loaded
            .cache
            .entries()
            .iter()
            .map(|e| FilmstripEntryMeta {
                frame_index: e.frame_index,
                pts_ns: e.pts_ns,
                width: e.image.width,
                height: e.image.height,
            })
            .collect();
        let count = entries.len();
        let origin = match loaded.origin {
            FilmstripOrigin::DiskCache => "disk_cache",
            _ => "generated",
        }
        .to_owned();

        vp.scrub.set_filmstrip(loaded.cache);
        let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        Ok(MediaFilmstripResponse {
            origin,
            cache_path: loaded.cache_path,
            count,
            elapsed_ms,
            entries,
        })
    }

    /// Publishes an instant scrub preview frame (`is_approximate = true` on cache miss,
    /// `false` if already decoded in GOP-LRU) into the viewport's SHM ring.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::NotFound`] if `viewport_id` is unknown, or media/SHM errors.
    pub fn scrub(
        &self,
        params: &MediaFrameRequestParams,
    ) -> Result<MediaScrubResponse, DaemonError> {
        let start = Instant::now();
        let viewport = self.viewport(&params.viewport_id)?;
        let mut viewport_guard = viewport
            .lock()
            .map_err(|_| DaemonError::Internal("viewport lock poisoned".into()))?;
        let vp = &mut *viewport_guard;

        let mut preview = vp.scrub.preview_scrub(&vp.track, params.frame_index)?;
        if preview.is_none() {
            // Auto-load/generate filmstrip if caller scrubbed before invoking `media.filmstrip`.
            let loaded = FilmstripCache::load_or_generate(
                &self.cache_dir,
                &vp.bundle_id,
                &vp.track,
                FilmstripConfig::default(),
                self.backend.as_ref(),
            )?;
            vp.scrub.set_filmstrip(loaded.cache);
            preview = vp.scrub.preview_scrub(&vp.track, params.frame_index)?;
        }

        let frame = preview.ok_or_else(|| {
            DaemonError::Media(arb_media::MediaError::Codec(
                "no preview frame available for scrub".to_owned(),
            ))
        })?;

        let rgba = to_rgba8_bytes(frame.format, &frame.data)?;
        let stride = frame.width.saturating_mul(4);
        let boottime_ns =
            lookup_frame_boottime(&vp.frame_boottimes, &vp.track, frame.displayed_frame_index);
        let generation = vp.shm_writer.publish(
            FrameDescriptor {
                frame_index: i64::try_from(frame.displayed_frame_index).unwrap_or(i64::MAX),
                is_approximate: frame.is_approximate,
                width: frame.width,
                height: frame.height,
                stride,
                pixel_format: ShmPixelFormat::Rgba8,
                boottime_ns,
            },
            &rgba,
        )?;
        let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        Ok(MediaScrubResponse {
            generation,
            requested_frame_index: frame.requested_frame_index,
            displayed_frame_index: frame.displayed_frame_index,
            is_approximate: frame.is_approximate,
            width: frame.width,
            height: frame.height,
            elapsed_ms,
        })
    }

    /// Decodes exact `frame_index` through [`GopLruCache`] and publishes it to SHM with
    /// `is_approximate = false`.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::NotFound`] if `viewport_id` is unknown, or media/SHM errors.
    pub fn seek_frame(
        &self,
        params: &MediaFrameRequestParams,
    ) -> Result<MediaSeekFrameResponse, DaemonError> {
        let start = Instant::now();
        let viewport = self.viewport(&params.viewport_id)?;
        let mut viewport_guard = viewport
            .lock()
            .map_err(|_| DaemonError::Internal("viewport lock poisoned".into()))?;
        let vp = &mut *viewport_guard;

        let settled = vp.scrub.settle_exact(
            &vp.track,
            params.frame_index,
            MediaPixelFormat::Rgba8,
            self.backend.as_ref(),
        )?;
        let stride = settled.width.saturating_mul(4);
        let boottime_ns = lookup_frame_boottime(
            &vp.frame_boottimes,
            &vp.track,
            settled.displayed_frame_index,
        );
        let generation = vp.shm_writer.publish(
            FrameDescriptor {
                frame_index: i64::try_from(settled.displayed_frame_index).unwrap_or(i64::MAX),
                is_approximate: false,
                width: settled.width,
                height: settled.height,
                stride,
                pixel_format: ShmPixelFormat::Rgba8,
                boottime_ns,
            },
            &settled.data,
        )?;
        let stats = vp.scrub.gop_cache().stats();
        let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        Ok(MediaSeekFrameResponse {
            generation,
            frame_index: settled.displayed_frame_index,
            is_approximate: false,
            width: settled.width,
            height: settled.height,
            cache_hits: stats.cache_hits,
            cache_misses: stats.cache_misses,
            elapsed_ms,
        })
    }

    fn viewport(&self, id: &str) -> Result<Arc<Mutex<OpenViewport>>, DaemonError> {
        self.viewports
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
            .ok_or_else(|| DaemonError::NotFound(format!("viewport {id} not found")))
    }

    /// Closes an open viewport and cleans up its SHM backing file.
    pub fn close_viewport(&self, viewport_id: &str) -> bool {
        let removed = self
            .viewports
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(viewport_id);
        removed.is_some()
    }

    /// Closes all active viewports during daemon shutdown.
    pub fn close_all(&self) {
        let removed = std::mem::take(
            &mut *self
                .viewports
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        drop(removed);
    }
}

/// Shared inputs for the standalone exporters, which run **outside** the media mutex `M`
/// so long exports never freeze live scrubbing in other viewports (R3 S9).
#[derive(Debug)]
pub struct ExportContext<'a> {
    /// Catalog used for the temporary `ExportJob` lease and trimmed-bundle registration.
    pub catalog: &'a Catalog,
    /// Daemon data directory (`<data_dir>/tmp` holds temporary MP4s).
    pub data_dir: &'a Path,
    /// Per-export codec backend (never the viewport engine's shared backend).
    pub backend: &'a dyn VideoCodecBackend,
    /// Test-only fault injection switches.
    pub faults: &'a FaultInjection,
}

/// Acquires a temporary `ExportJob` lease on a cataloged source bundle so `bundle.gc` cannot
/// purge it mid-export; the lease is released on every return path.
fn acquire_export_lease<'a>(
    catalog: &'a Catalog,
    source_catalog_id: Option<&str>,
    holder_pid: u32,
) -> Result<Option<LeaseDropGuard<'a>>, DaemonError> {
    let Some(src_id) = source_catalog_id else {
        return Ok(None);
    };
    let lease_id = catalog.acquire_lease(
        src_id,
        LeaseHolderKind::ExportJob,
        holder_pid,
        &utc_now_iso8601(),
    )?;
    Ok(Some(LeaseDropGuard { catalog, lease_id }))
}

/// Exports an exact integer `frame_index` as a PNG with `arb:*` provenance `tEXt` chunks.
///
/// Holds an `ExportJob` lease on the source bundle for the whole decode (R3 S7). The caller
/// must already have validated `params.output_path` (absolute, outside every bundle and the
/// daemon's private directories); the parent directory is only created afterwards.
///
/// # Errors
///
/// Returns [`DaemonError`] if the bundle or video track cannot be opened, or PNG export fails.
pub fn export_png(
    ctx: &ExportContext<'_>,
    bundle_dir: &Path,
    source_catalog_id: Option<&str>,
    params: &ExportPngParams,
) -> Result<ExportPngResponse, DaemonError> {
    let _lease_guard = acquire_export_lease(ctx.catalog, source_catalog_id, std::process::id())?;
    let reader = open_bundle(bundle_dir)?;
    let summary = reader.manifest_summary();
    let video_path = reader.video_path().ok_or_else(|| {
        DaemonError::NotFound(format!(
            "bundle `{}` has no video track for PNG export",
            summary.bundle_id
        ))
    })?;
    let frames = reader.frames().unwrap_or_default();
    let boottime_ns = frames
        .iter()
        .find(|f| f.frame_index == params.frame_index)
        .and_then(|f| match &f.alignment {
            TimeAlignment::Anchored { boottime_ns, .. } => Some(*boottime_ns),
            TimeAlignment::Bounded {
                min_boottime_ns, ..
            } => Some(*min_boottime_ns),
            _ => None,
        });
    if let Some(parent) = params.output_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let meta = PngExportMetadata {
        boottime_ns,
        source_bundle_id: Some(summary.bundle_id.clone()),
    };
    let report = export_frame_png_with_metadata(
        &video_path,
        params.frame_index,
        &params.output_path,
        &meta,
        ctx.backend,
    )?;
    Ok(ExportPngResponse {
        output_path: params.output_path.clone(),
        report,
    })
}

/// Performs a non-destructive frame-accurate integer `[in_frame_index..=out_frame_index]`
/// trim while holding a temporary `ExportJob` lease on the source bundle.
///
/// Source bundle files are NEVER modified. Output MP4 contains strictly zero `edts`/`elst`
/// boxes and carries forward `unanchored` records via [`select_for_trim_with_truncated`].
/// A commit whose only failure is the parent-directory `fsync` is treated as published and
/// registered idempotently (R3 M1 / S8a).
///
/// # Errors
///
/// Returns [`DaemonError`] if the frame range is invalid, video trim fails, or atomic
/// v2 bundle write fails.
#[expect(
    clippy::too_many_lines,
    reason = "end-to-end atomic trim pipeline coordinates lease, video/audio trim, optional tap burn-in, share MP4 mux, sidecar draft, and post-commit trash"
)]
pub fn export_trim(
    ctx: &ExportContext<'_>,
    source_bundle_dir: &Path,
    source_catalog_id: Option<&str>,
    dest_parent: &Path,
    params: &ExportTrimParams,
) -> Result<ExportTrimResponse, DaemonError> {
    let now_utc = utc_now_iso8601();
    let holder_pid = params.holder_pid.unwrap_or_else(std::process::id);
    let catalog = ctx.catalog;
    let lease_guard = acquire_export_lease(catalog, source_catalog_id, holder_pid)?;

    let reader = open_bundle(source_bundle_dir)?;
    let summary = reader.manifest_summary();
    let parent_bundle_id = source_catalog_id.unwrap_or(&summary.bundle_id).to_owned();

    // Enforce REQ-04/07 safety invariant upfront: never trash read-only legacy or uncataloged bundles.
    if params.trash_original {
        let entry = source_catalog_id
            .map(|id| catalog.bundle(id))
            .transpose()?
            .flatten();
        if reader.is_legacy()
            || entry
                .as_ref()
                .is_some_and(|e| e.format_version == CatalogFormatVersion::LegacyRo)
        {
            return Err(DaemonError::Conflict(format!(
                "refusing trash_original on read-only legacy bundle `{parent_bundle_id}`"
            )));
        }
        if !entry.is_some_and(|e| e.state == arb_core::state::BundleLifecycleState::Active) {
            return Err(DaemonError::Conflict(
                "refusing trash_original on uncataloged or non-active bundle; open or register it first"
                    .to_owned(),
            ));
        }
    }

    let video_path = reader.video_path().ok_or_else(|| {
        DaemonError::NotFound(format!(
            "source bundle `{parent_bundle_id}` has no video.mp4"
        ))
    })?;

    let source_frames = reader.frames()?;
    let frame_window = WindowSpec::from_frame_indices(
        &source_frames,
        params.in_frame_index,
        params.out_frame_index,
    )?;

    let trim_job_id = next_id("trim");
    let new_bundle_id = next_id("bundle");
    let tmp_dir = ctx.data_dir.join("tmp");
    fs::create_dir_all(&tmp_dir)?;
    let tmp_trimmed_mp4 = tmp_dir.join(format!("{trim_job_id}.mp4"));
    let _tmp_guard = FileCleanupGuard(tmp_trimmed_mp4.clone());

    let trim_report = trim_video(
        &video_path,
        &tmp_trimmed_mp4,
        params.in_frame_index,
        params.out_frame_index,
        ctx.backend,
    )?;

    // W11: cut the source `audio.m4a` (if any) to the trimmed video's time range.
    let (trimmed_audio, _tmp_audio_guard) =
        trim_source_audio(reader.as_ref(), &video_path, params, &tmp_dir, &trim_job_id)?.unzip();

    let trimmed_track = Mp4VideoTrack::open(&tmp_trimmed_mp4)?;
    let trimmed_frames =
        build_trimmed_frame_records(&trimmed_track, &source_frames, &frame_window, params);

    let policy = SelectionPolicy::default();
    let (trimmed_logs, main_log_count, unanchored_log_count, log_truncated) =
        select_trimmed_logs(reader.as_ref(), summary, &frame_window, &policy)?;
    let (trimmed_taps, main_tap_count, unanchored_tap_count, tap_truncated) =
        select_trimmed_taps(reader.as_ref(), &frame_window, &policy)?;
    let trimmed_discs = filter_discontinuities_for_window(reader.discontinuities()?, &frame_window);

    let clock_anchors: Vec<ClockAnchorRecord> = if reader.is_legacy() {
        Vec::new()
    } else {
        V2BundleReader::open(source_bundle_dir)?
            .clock_anchors()?
            .into_iter()
            .filter(|anchor| match anchor {
                ClockAnchorRecord::DeviceTriple(t) => t.boot_id == frame_window.boot_id(),
                ClockAnchorRecord::PcSyncProbe(p) => p.boot_id == frame_window.boot_id(),
                _ => false,
            })
            .collect()
    };

    let unanchored_summary = UnanchoredSummary {
        included_count: u64::try_from(unanchored_log_count + unanchored_tap_count)
            .unwrap_or(u64::MAX),
        truncated_count: u64::try_from(log_truncated.saturating_add(tap_truncated))
            .unwrap_or(u64::MAX),
    };

    // Optional ready-to-share MP4 (`share.mp4` or `share-taps.mp4`) staged inside the trimmed bundle.
    let emit_share_mp4 = params.include_share_mp4 || params.burn_taps;
    let mut burn_taps_applied = false;
    let mut _tmp_burned_guard: Option<FileCleanupGuard> = None;
    let mut _tmp_share_guard: Option<FileCleanupGuard> = None;
    let mut share_sidecar_name: Option<&'static str> = None;
    let mut share_sidecar: Option<ShareMp4Sidecar> = None;

    if emit_share_mp4 && !trimmed_frames.is_empty() {
        let share_name = if params.burn_taps {
            SHARE_TAPS_MP4_FILE
        } else {
            SHARE_MP4_FILE
        };
        let video_for_share: std::borrow::Cow<'_, Path> = if params.burn_taps {
            let burn_events: Vec<BurnInTouchEvent> = trimmed_taps
                .iter()
                .filter_map(BurnInTouchEvent::from_tap_record)
                .collect();
            let frame_ts: Vec<i64> = trimmed_frames
                .iter()
                .map(|f| frame_boottime_ns(f).unwrap_or(f.pts_device_mono_ns))
                .collect();
            let geom = BurnInDisplayGeometry {
                display_width: summary.streams.video.as_ref().and_then(|v| v.display_width),
                display_height: summary
                    .streams
                    .video
                    .as_ref()
                    .and_then(|v| v.display_height),
            };
            let tmp_burned = tmp_dir.join(format!("{trim_job_id}-burned.mp4"));
            let last_trimmed = u64::try_from(trimmed_frames.len().saturating_sub(1)).unwrap_or(0);
            _tmp_burned_guard = Some(FileCleanupGuard(tmp_burned.clone()));
            let burn_report = burn_in_taps_video(
                &tmp_trimmed_mp4,
                &tmp_burned,
                0,
                last_trimmed,
                &frame_ts,
                &burn_events,
                geom,
                ctx.backend,
            )?;
            burn_taps_applied = burn_report.reencoded;
            std::borrow::Cow::Owned(tmp_burned)
        } else {
            std::borrow::Cow::Borrowed(tmp_trimmed_mp4.as_path())
        };

        let tmp_share_mp4 = tmp_dir.join(format!("{trim_job_id}-{share_name}"));
        let lead_in = trimmed_audio.as_ref().map_or(0, |a| a.lead_in_offset_ns);
        _tmp_share_guard = Some(FileCleanupGuard(tmp_share_mp4.clone()));
        let _mux_report = mux_share_mp4(
            &video_for_share,
            trimmed_audio.as_ref().map(|a| a.tmp_m4a.as_path()),
            lead_in,
            &tmp_share_mp4,
        )?;
        let atoms = inspect_mp4_atoms(&tmp_share_mp4)?;
        if atoms.edts_count != 0 || atoms.elst_count != 0 {
            return Err(DaemonError::Internal(format!(
                "verified trimmed {share_name} unexpectedly contained edts={} elst={}",
                atoms.edts_count, atoms.elst_count
            )));
        }
        share_sidecar_name = Some(share_name);
        share_sidecar = Some(ShareMp4Sidecar {
            file_name: share_name.to_owned(),
            payload: MediaPayload::File(tmp_share_mp4),
        });
    }

    let dest_bundle_dir = dest_parent.join(&new_bundle_id);
    // R3 S8c: a trim that includes the source's last frame inherits its incomplete tail.
    let last_source_frame = source_frames.iter().map(|f| f.frame_index).max();
    let includes_source_tail = last_source_frame.is_some_and(|last| params.out_frame_index >= last);
    let tail_may_be_incomplete = includes_source_tail
        && summary
            .streams
            .video
            .as_ref()
            .is_some_and(|v| v.tail_may_be_incomplete);

    let draft = build_trimmed_bundle_draft(
        &new_bundle_id,
        &parent_bundle_id,
        &now_utc,
        summary,
        &frame_window,
        params,
        TrimmedVideo {
            output_frame_count: trim_report.output_frame_count,
            tail_may_be_incomplete,
            includes_source_tail,
            tmp_mp4: tmp_trimmed_mp4,
            frames: trimmed_frames,
        },
        trimmed_audio,
        share_sidecar,
        unanchored_summary,
        trimmed_discs,
        trimmed_logs,
        trimmed_taps,
        clock_anchors,
    );

    let (app_logs_count, commit_warning) = commit_trimmed_bundle(
        ctx,
        &trim_job_id,
        &draft,
        &summary.app_logs,
        &NewBundleEntry {
            bundle_id: new_bundle_id.clone(),
            path: dest_bundle_dir.clone(),
            format_version: CatalogFormatVersion::V2,
            parent_bundle_id: Some(parent_bundle_id),
            created_at_utc: now_utc,
        },
    )?;

    drop(lease_guard);

    let mut original_trashed = false;
    let mut trash_warning: Option<String> = None;
    if params.trash_original
        && let Some(src_id) = source_catalog_id
    {
        match catalog.trash_bundle(src_id, &utc_now_iso8601()) {
            Ok(_) => original_trashed = true,
            Err(err) => {
                trash_warning = Some(format!(
                    "trimmed bundle committed but trashing the original failed: {err}"
                ));
            }
        }
    }

    let share_mp4_path = share_sidecar_name.map(|name| dest_bundle_dir.join(name));
    let warning = match (commit_warning, trash_warning) {
        (Some(a), Some(b)) => Some(format!("{a}; {b}")),
        (a, b) => a.or(b),
    };

    Ok(ExportTrimResponse {
        bundle_id: new_bundle_id,
        bundle_path: dest_bundle_dir,
        trim_report,
        unanchored_summary,
        main_log_count,
        unanchored_log_count,
        main_tap_count,
        unanchored_tap_count,
        share_mp4_path,
        app_logs_count,
        original_trashed,
        burn_taps_applied,
        warning,
    })
}

/// Commits the trimmed bundle and registers it idempotently. A commit whose only failure
/// is the parent-directory `fsync` is published: it is registered and reported as a warning
/// instead of an error that would invite a second, duplicate trim (R3 M1 / S8a).
fn commit_trimmed_bundle(
    ctx: &ExportContext<'_>,
    trim_job_id: &str,
    draft: &V2BundleDraft,
    source_apps: &[arb_core::bundle::AppLogSummary],
    entry: &NewBundleEntry,
) -> Result<(usize, Option<String>), DaemonError> {
    let (app_logs_count, warning) = match V2BundleWriter::write_with_source_apps(
        &entry.path,
        trim_job_id,
        draft,
        source_apps,
    ) {
        Ok(manifest) => {
            let count = manifest.app_logs.len();
            match ctx.faults.maybe_fail_parent_sync(&entry.path) {
                Ok(()) => (count, None),
                Err(err) if err.committed_bundle_path_on_parent_sync_error().is_some() => (
                    count,
                    Some(format!(
                        "trimmed bundle committed but its parent directory could not be fsynced: {err}"
                    )),
                ),
                Err(err) => return Err(err.into()),
            }
        }
        Err(err) if err.committed_bundle_path_on_parent_sync_error().is_some() => {
            let count =
                V2BundleReader::open(&entry.path).map_or(0, |r| r.manifest().app_logs.len());
            (
                count,
                Some(format!(
                    "trimmed bundle committed but its parent directory could not be fsynced: {err}"
                )),
            )
        }
        Err(err) => return Err(err.into()),
    };
    ctx.catalog
        .register_bundle_idempotent(entry)
        .map_err(|err| {
            DaemonError::Internal(format!(
                "trimmed bundle `{}` is published at `{}` but could not be cataloged \
             (open it with bundle.open to register it): {err}",
                entry.bundle_id,
                entry.path.display()
            ))
        })?;
    Ok((app_logs_count, warning))
}

/// Trimmed video payload and its per-frame records for [`build_trimmed_bundle_draft`].
struct TrimmedVideo {
    output_frame_count: u64,
    tail_may_be_incomplete: bool,
    /// `true` when `out_frame_index` reaches the source's last frame (the trimmed streams
    /// inherit the source's incomplete tail, R3 S8c).
    includes_source_tail: bool,
    tmp_mp4: PathBuf,
    frames: Vec<FrameRecord>,
}

/// Trimmed `audio.m4a` payload for [`build_trimmed_bundle_draft`].
struct TrimmedAudio {
    tmp_m4a: PathBuf,
    /// Lead-in skew of the *trimmed* bundle: `D' = a0_sel + D - video_start_ns`
    /// (first selected packet's source audio pts + save-time skew − new video origin).
    lead_in_offset_ns: i64,
}

/// Cuts the source bundle's `audio.m4a` (when it has one) to the trimmed video's time range
/// and returns the staged payload alongside the guard that removes the temp file on drop.
///
/// Both `video.mp4` and `audio.m4a` are muxed rebased to `pts = 0`, but `audio.m4a` pts0 sits
/// `D = audio_lead_in_offset_ns` later than `video.mp4` pts0 on the shared device clock
/// (R4 M4): a video-axis window `[start, end]` therefore maps to audio-axis
/// `[start - D, end - D]`. The window lies entirely before the audio origin when `end <= D`
/// (clean `Absent`), and its start clamps to `0` when `start < D < end`. Returns `Ok(None)`
/// when the source has no audio file or no AAC access unit intersects the translated range;
/// only [`MediaError::NoAudioInRange`] maps to `None` — real `InvalidArgument` bugs and every
/// other error propagate so a corrupt source audio track can never silently produce a bundle
/// that claims trimmed audio (R3 M3, R4 S10).
fn trim_source_audio(
    reader: &dyn BundleReader,
    video_path: &Path,
    params: &ExportTrimParams,
    tmp_dir: &Path,
    trim_job_id: &str,
) -> Result<Option<(TrimmedAudio, FileCleanupGuard)>, DaemonError> {
    let Some(src_m4a) = reader.audio_path() else {
        return Ok(None);
    };
    // Save-time measured lead-in skew (R4 M4); bundles predating the field carry 0, which
    // is equivalent to the previous perfectly-aligned-origins assumption.
    let d_ns = reader
        .manifest_summary()
        .streams
        .audio
        .as_ref()
        .map_or(0, |audio| audio.audio_lead_in_offset_ns);

    let src_track = Mp4VideoTrack::open(video_path)?;
    let video_start_ns = src_track.frame(params.in_frame_index)?.pts_ns.max(0);
    let out_frame = src_track.frame(params.out_frame_index)?;
    let video_end_ns = out_frame
        .pts_ns
        .saturating_add(i64::try_from(out_frame.duration_ns).unwrap_or(i64::MAX))
        .max(video_start_ns.saturating_add(1));

    // Translate the video-axis window into audio-file coordinates.
    let start_ns = video_start_ns.saturating_sub(d_ns).max(0);
    let end_ns = video_end_ns.saturating_sub(d_ns);
    if end_ns <= start_ns {
        return Ok(None);
    }

    let tmp_m4a = tmp_dir.join(format!("{trim_job_id}.m4a"));
    match trim_audio_m4a(&src_m4a, &tmp_m4a, start_ns, end_ns) {
        Ok(report) => Ok(Some((
            TrimmedAudio {
                tmp_m4a: tmp_m4a.clone(),
                lead_in_offset_ns: report
                    .first_selected_pts_ns
                    .saturating_add(d_ns)
                    .saturating_sub(video_start_ns),
            },
            FileCleanupGuard(tmp_m4a),
        ))),
        Err(MediaError::NoAudioInRange { .. }) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "assembling a complete V2BundleDraft collects all independently selected streams"
)]
fn build_trimmed_bundle_draft(
    new_bundle_id: &str,
    parent_bundle_id: &str,
    now_utc: &str,
    summary: &arb_storage::bundle_io::BundleManifestSummary,
    frame_window: &WindowSpec,
    params: &ExportTrimParams,
    video: TrimmedVideo,
    trimmed_audio: Option<TrimmedAudio>,
    share_mp4: Option<ShareMp4Sidecar>,
    unanchored_summary: UnanchoredSummary,
    trimmed_discs: Vec<DiscontinuityEvent>,
    trimmed_logs: Vec<arb_core::bundle::LogRecord>,
    trimmed_taps: Vec<arb_core::bundle::TapRecord>,
    clock_anchors: Vec<ClockAnchorRecord>,
) -> V2BundleDraft {
    let device = summary.device.clone().unwrap_or_else(|| DeviceInfo {
        serial: "unknown".to_owned(),
        model: "unknown".to_owned(),
        api_level: 0,
        is_emulator: false,
    });
    V2BundleDraft {
        bundle_kind: BundleKind::Full,
        bundle_id: new_bundle_id.to_owned(),
        created_at_utc: now_utc.to_owned(),
        provenance: Provenance::Trimmed {
            parent_bundle_id: parent_bundle_id.to_owned(),
            in_frame_index: params.in_frame_index,
            out_frame_index: params.out_frame_index,
        },
        device,
        requested_window: RequestedWindow::TrimFrameRange {
            in_frame_index: params.in_frame_index,
            out_frame_index: params.out_frame_index,
        },
        epochs: vec![EpochSpan {
            boot_id: frame_window.boot_id().to_owned(),
            start_boottime_ns: frame_window.start_boottime_ns(),
            end_boottime_ns: frame_window.end_boottime_ns(),
        }],
        streams: StreamsSummary {
            video: Some(VideoStreamMeta {
                status: StreamStatus::Present,
                frame_count: video.output_frame_count,
                tail_may_be_incomplete: video.tail_may_be_incomplete,
                semantics: TimestampSemantics::VideoEncoderInputEstimate,
                display_width: summary.streams.video.as_ref().and_then(|v| v.display_width),
                display_height: summary
                    .streams
                    .video
                    .as_ref()
                    .and_then(|v| v.display_height),
            }),
            audio: summary.streams.audio.clone().map(|mut audio| {
                if let Some(trimmed) = &trimmed_audio {
                    // A trimmed `audio.m4a` was written: a `Present` source stays `Present`
                    // (the file satisfies `verify_integrity`, R2 S15), a `Partial` source
                    // stays `Partial`. The incomplete-tail flag only survives when the trim
                    // still reaches the source's last frame (R3 S8c, applied to audio).
                    audio.tail_may_be_incomplete =
                        video.includes_source_tail && audio.tail_may_be_incomplete;
                    // R4 M4: record the trimmed bundle's own rebased lead-in skew.
                    audio.av_offset_ms = trimmed.lead_in_offset_ns / 1_000_000;
                    audio.audio_lead_in_offset_ns = trimmed.lead_in_offset_ns;
                } else {
                    // No AAC access unit intersects the trimmed video range.
                    audio.status = StreamStatus::Absent;
                    audio.tail_may_be_incomplete = false;
                    audio.av_offset_ms = 0;
                    audio.audio_lead_in_offset_ns = 0;
                }
                audio
            }),
            logcat: summary.streams.logcat.clone(),
            input: summary.streams.input.clone(),
        },
        unanchored_summary,
        video_mp4: Some(MediaPayload::File(video.tmp_mp4)),
        audio_m4a: trimmed_audio.map(|a| MediaPayload::File(a.tmp_m4a)),
        share_mp4,
        frames: video.frames,
        discontinuities: trimmed_discs,
        logs: trimmed_logs,
        taps: trimmed_taps,
        clock_anchors,
    }
}

fn build_trimmed_frame_records(
    trimmed_track: &Mp4VideoTrack,
    source_frames: &[FrameRecord],
    frame_window: &WindowSpec,
    params: &ExportTrimParams,
) -> Vec<FrameRecord> {
    let trimmed_mp4_frames = trimmed_track.frames();
    let selected_source_frames: Vec<&FrameRecord> = source_frames
        .iter()
        .filter(|f| {
            f.frame_index >= params.in_frame_index && f.frame_index <= params.out_frame_index
        })
        .collect();

    let mut trimmed_frames = Vec::with_capacity(trimmed_mp4_frames.len());
    for (idx, mp4_f) in trimmed_mp4_frames.iter().enumerate() {
        let frame_index = u64::try_from(idx).unwrap_or(0);
        let record = match selected_source_frames.get(idx).copied() {
            Some(orig) => FrameRecord {
                frame_index,
                boot_id: orig.boot_id.clone(),
                pts_device_mono_ns: orig.pts_device_mono_ns,
                pc_ingest_cont_ns: orig.pc_ingest_cont_ns,
                alignment: orig.alignment.clone(),
                is_keyframe: mp4_f.is_keyframe,
                gop_id: mp4_f.gop_id,
                mp4_sample_offset: mp4_f.offset,
                mp4_sample_size: mp4_f.size,
            },
            // R3 S8b: never fabricate an exact anchor for a frame with no source record.
            None => FrameRecord {
                frame_index,
                boot_id: frame_window.boot_id().to_owned(),
                pts_device_mono_ns: mp4_f.pts_ns,
                pc_ingest_cont_ns: frame_window.ingest_start_cont_ns(),
                alignment: TimeAlignment::Unanchored {
                    pc_ingest_cont_ns: frame_window.ingest_start_cont_ns(),
                    reason: UncertaintyReason::NoClockAnchor,
                },
                is_keyframe: mp4_f.is_keyframe,
                gop_id: mp4_f.gop_id,
                mp4_sample_offset: mp4_f.offset,
                mp4_sample_size: mp4_f.size,
            },
        };
        trimmed_frames.push(record);
    }
    trimmed_frames
}

fn select_trimmed_logs(
    reader: &dyn BundleReader,
    summary: &arb_storage::bundle_io::BundleManifestSummary,
    frame_window: &WindowSpec,
    policy: &SelectionPolicy,
) -> Result<(Vec<arb_core::bundle::LogRecord>, usize, usize, usize), DaemonError> {
    let mut source_logs = reader.query_logs(None, TimeRange::all(), None)?;
    let packages = summary
        .app_logs
        .iter()
        .map(|app| app.package_name.clone())
        .collect();
    let resolver = arb_core::analytics::LogPackageResolver::from_logs(&source_logs, &packages);
    for log in &mut source_logs {
        if log.package_name.is_none()
            && let Some(package) = log
                .pid
                .and_then(|pid| resolver.package_for_pid(log.boot_id.as_deref(), pid))
            && packages.contains(package)
        {
            log.package_name = Some(package.to_owned());
        }
    }
    let (source_main, source_unanchored): (Vec<_>, Vec<_>) = source_logs
        .into_iter()
        .partition(|log| log.bucket == RecordBucket::Main);
    let source_truncated =
        usize::try_from(summary.unanchored_summary.truncated_count).unwrap_or(usize::MAX);
    let part = select_for_trim_with_truncated(
        source_main,
        source_unanchored,
        source_truncated,
        frame_window,
        policy,
    );
    let main_count = part.main.len();
    let unanchored_count = part.unanchored.len();
    let mut trimmed = Vec::with_capacity(main_count + unanchored_count);
    let mut seq = 1_u64;
    for mut rec in part.main {
        rec.seq = seq;
        rec.bucket = RecordBucket::Main;
        trimmed.push(rec);
        seq += 1;
    }
    for mut rec in part.unanchored {
        rec.seq = seq;
        rec.bucket = RecordBucket::Unanchored;
        trimmed.push(rec);
        seq += 1;
    }
    Ok((
        trimmed,
        main_count,
        unanchored_count,
        part.unanchored_truncated_count,
    ))
}

fn select_trimmed_taps(
    reader: &dyn BundleReader,
    frame_window: &WindowSpec,
    policy: &SelectionPolicy,
) -> Result<(Vec<arb_core::bundle::TapRecord>, usize, usize, usize), DaemonError> {
    let source_main = reader.query_taps(Some(RecordBucket::Main), TimeRange::all(), None)?;
    let source_unanchored =
        reader.query_taps(Some(RecordBucket::Unanchored), TimeRange::all(), None)?;
    let part =
        select_for_trim_with_truncated(source_main, source_unanchored, 0, frame_window, policy);
    let main_count = part.main.len();
    let unanchored_count = part.unanchored.len();
    let mut trimmed = Vec::with_capacity(main_count + unanchored_count);
    let mut seq = 1_u64;
    for mut rec in part.main {
        rec.seq = seq;
        rec.bucket = RecordBucket::Main;
        trimmed.push(rec);
        seq += 1;
    }
    for mut rec in part.unanchored {
        rec.seq = seq;
        rec.bucket = RecordBucket::Unanchored;
        trimmed.push(rec);
        seq += 1;
    }
    Ok((
        trimmed,
        main_count,
        unanchored_count,
        part.unanchored_truncated_count,
    ))
}

struct LeaseDropGuard<'a> {
    catalog: &'a Catalog,
    lease_id: String,
}

impl Drop for LeaseDropGuard<'_> {
    fn drop(&mut self) {
        let _ = self.catalog.release_lease(&self.lease_id);
    }
}

struct FileCleanupGuard(PathBuf);

impl Drop for FileCleanupGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn filter_discontinuities_for_window(
    discontinuities: Vec<DiscontinuityEvent>,
    window: &WindowSpec,
) -> Vec<DiscontinuityEvent> {
    let w_start = window.start_boottime_ns().min(window.end_boottime_ns());
    let w_end = window.start_boottime_ns().max(window.end_boottime_ns());
    discontinuities
        .into_iter()
        .filter(|d| {
            if let Some(b) = d.boot_id.as_deref()
                && b != window.boot_id()
            {
                return false;
            }
            match (d.start_boottime_ns, d.end_boottime_ns) {
                (Some(s), Some(e)) => s <= w_end && e >= w_start,
                (Some(s), None) => s >= w_start && s <= w_end,
                (None, Some(e)) => e >= w_start && e <= w_end,
                (None, None) => {
                    d.pc_cont_start_ns <= window.ingest_end_cont_ns()
                        && d.pc_cont_end_ns.unwrap_or(d.pc_cont_start_ns)
                            >= window.ingest_start_cont_ns()
                }
            }
        })
        .collect()
}

fn lookup_frame_boottime(frame_boottimes: &[i64], track: &Mp4VideoTrack, frame_index: u64) -> i64 {
    if let Ok(idx) = usize::try_from(frame_index)
        && let Some(&bt) = frame_boottimes.get(idx)
    {
        return bt;
    }
    track.frame(frame_index).map_or(0, |f| f.pts_ns)
}

fn to_rgba8_bytes(format: MediaPixelFormat, data: &[u8]) -> Result<Vec<u8>, DaemonError> {
    match format {
        MediaPixelFormat::Rgba8 => Ok(data.to_vec()),
        MediaPixelFormat::Rgb24 => {
            let mut out = Vec::with_capacity((data.len() / 3).saturating_mul(4));
            for px in data.as_chunks::<3>().0 {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            Ok(out)
        }
        MediaPixelFormat::Bgra8 => {
            let mut out = Vec::with_capacity(data.len());
            for px in data.as_chunks::<4>().0 {
                out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
            Ok(out)
        }
        other => Err(DaemonError::Media(arb_media::MediaError::Codec(format!(
            "unsupported pixel format for SHM RGBA8 conversion: {other:?}"
        )))),
    }
}

/// Computes per-epoch frame index and `DeviceBoottime` ranges by grouping contiguous runs of
/// `frames` sharing the same `boot_id`.
///
/// Falls back to matching [`EpochSpan`] bounds from the manifest when frames in a run lack
/// boottime alignment.
#[must_use]
pub(crate) fn compute_epoch_frame_ranges(
    frames: &[FrameRecord],
    epochs: &[EpochSpan],
) -> Vec<EpochFrameRange> {
    if frames.is_empty() {
        return Vec::new();
    }

    let mut ranges: Vec<EpochFrameRange> = Vec::new();
    let mut run_boot_id = frames[0].boot_id.as_str();
    let mut start_frame = frames[0].frame_index;
    let mut end_frame = frames[0].frame_index;
    let mut min_bt: Option<i64> = frame_boottime_ns(&frames[0]);
    let mut max_bt: Option<i64> = min_bt;

    let flush = |ranges: &mut Vec<EpochFrameRange>,
                 boot_id: &str,
                 start_frame_index: u64,
                 end_frame_index: u64,
                 min_bt: Option<i64>,
                 max_bt: Option<i64>| {
        let fallback = epochs.iter().find(|e| e.boot_id == boot_id);
        let start_boottime_ns = min_bt
            .or_else(|| fallback.map(|e| e.start_boottime_ns))
            .unwrap_or(0);
        let end_boottime_ns = max_bt
            .or_else(|| fallback.map(|e| e.end_boottime_ns))
            .unwrap_or(start_boottime_ns)
            .max(start_boottime_ns);
        ranges.push(EpochFrameRange {
            epoch_index: ranges.len(),
            boot_id: boot_id.to_owned(),
            start_frame_index,
            end_frame_index,
            start_boottime_ns,
            end_boottime_ns,
        });
    };

    for frame in &frames[1..] {
        if frame.boot_id == run_boot_id {
            end_frame = frame.frame_index;
            if let Some(bt) = frame_boottime_ns(frame) {
                min_bt = Some(min_bt.map_or(bt, |prev| prev.min(bt)));
                max_bt = Some(max_bt.map_or(bt, |prev| prev.max(bt)));
            }
        } else {
            flush(
                &mut ranges,
                run_boot_id,
                start_frame,
                end_frame,
                min_bt,
                max_bt,
            );
            run_boot_id = frame.boot_id.as_str();
            start_frame = frame.frame_index;
            end_frame = frame.frame_index;
            min_bt = frame_boottime_ns(frame);
            max_bt = min_bt;
        }
    }
    flush(
        &mut ranges,
        run_boot_id,
        start_frame,
        end_frame,
        min_bt,
        max_bt,
    );
    ranges
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

/// Computes a unified [`crate::protocol::TimelineSummaryResponse`] from any [`BundleReader`].
///
/// # Errors
///
/// Returns [`DaemonError::Storage`] if timeline tables cannot be read.
pub fn build_timeline_summary(
    reader: &dyn BundleReader,
    bundle_dir: &Path,
) -> Result<crate::protocol::TimelineSummaryResponse, DaemonError> {
    let summary = reader.manifest_summary();
    let frames = reader.frames()?;
    let keyframe_count = frames.iter().filter(|f| f.is_keyframe).count();
    let epoch_frame_ranges = compute_epoch_frame_ranges(&frames, &summary.epochs);
    let main_logs = reader.query_logs(Some(RecordBucket::Main), TimeRange::all(), None)?;
    let unanchored_logs =
        reader.query_logs(Some(RecordBucket::Unanchored), TimeRange::all(), None)?;
    let main_taps = reader.query_taps(Some(RecordBucket::Main), TimeRange::all(), None)?;
    let unanchored_taps =
        reader.query_taps(Some(RecordBucket::Unanchored), TimeRange::all(), None)?;
    let discs = reader.discontinuities()?;

    Ok(crate::protocol::TimelineSummaryResponse {
        bundle_id: summary.bundle_id.clone(),
        path: bundle_dir.to_path_buf(),
        format: summary.format,
        bundle_kind: summary.bundle_kind,
        is_legacy: reader.is_legacy(),
        created_at_utc: summary.created_at_utc.clone(),
        provenance: summary.provenance.clone(),
        device: summary.device.clone(),
        streams: summary.streams.clone(),
        unanchored_summary: summary.unanchored_summary,
        epochs: summary.epochs.clone(),
        epoch_frame_ranges,
        frame_count: frames.len(),
        keyframe_count,
        log_main_count: main_logs.len(),
        log_unanchored_count: unanchored_logs.len(),
        tap_main_count: main_taps.len(),
        tap_unanchored_count: unanchored_taps.len(),
        discontinuity_count: discs.len(),
        video_path: reader.video_path(),
        audio_path: reader.audio_path(),
        app_logs: summary.app_logs.clone(),
    })
}

#[cfg(test)]
mod tests {
    use arb_core::bundle::FrameRecord;
    use arb_core::selection::WindowSpec;
    use arb_core::time::{TimeAlignment, TimestampSemantics, UncertaintyReason};
    use arb_media::codec::{DecodedFrame, EncodeParams, OpenH264Backend, PixelFormat};
    use arb_media::mp4::{Mp4VideoTrack, mux_raw_h264_to_mp4};
    use tempfile::TempDir;

    use super::build_trimmed_frame_records;
    use crate::protocol::ExportTrimParams;

    #[test]
    fn viewport_timestamps_preserve_static_holds_and_bound_wire_size() {
        assert_eq!(
            super::viewport_timestamps([40, 50, 5_000_000_050].into_iter()),
            [0, 10, 5_000_000_010]
        );
        assert_eq!(
            super::viewport_timestamps((0..18_000).map(i64::from)).len(),
            18_000
        );
        assert_eq!(
            super::viewport_timestamps((0..100_001).map(i64::from)).len(),
            0
        );
    }

    #[test]
    fn host_only_gaps_are_trimmed_on_the_ingest_clock() {
        let window = WindowSpec::RealElapsed {
            boot_id: "boot".into(),
            start_boottime_ns: 10,
            end_boottime_ns: 20,
            ingest_start_cont_ns: 100,
            ingest_end_cont_ns: 200,
        };
        let gap = |start, end| arb_core::time::DiscontinuityEvent {
            kind: arb_core::time::DiscontinuityKind::StreamGap,
            stream: None,
            boot_id: None,
            start_boottime_ns: None,
            end_boottime_ns: None,
            pc_cont_start_ns: start,
            pc_cont_end_ns: end,
            delta_ns: None,
            detail: None,
        };
        let selected = super::filter_discontinuities_for_window(
            vec![gap(40, Some(90)), gap(50, Some(100)), gap(201, None)],
            &window,
        );
        assert_eq!(selected, vec![gap(50, Some(100))]);
    }

    #[test]
    fn r3_s8b_trimmed_unmatched_frame_uses_unanchored_never_fabricated_anchor() {
        let tmp = TempDir::new().expect("tempdir");
        let mp4_path = tmp.path().join("trimmed.mp4");
        let frames: Vec<DecodedFrame> = (0..3_u64)
            .map(|i| DecodedFrame {
                frame_index: i,
                width: 64,
                height: 64,
                format: PixelFormat::Yuv420p,
                data: vec![u8::try_from(i * 40).expect("fits u8"); 64 * 64 * 3 / 2],
            })
            .collect();
        let packets = OpenH264Backend::new()
            .encode_yuv420p_sequence(&frames, 3, 33_333_333, EncodeParams::default())
            .expect("encode must succeed");
        mux_raw_h264_to_mp4(&mp4_path, 64, 64, 90_000, 3_000, &packets).expect("mux");
        let track = Mp4VideoTrack::open(&mp4_path).expect("open mp4");
        assert_eq!(track.frame_count(), 3, "track must have 3 frames");

        // Provide only 2 source records for a 3-frame trimmed track so frame 2 has no match.
        let source_frames = vec![
            FrameRecord {
                frame_index: 0,
                boot_id: "boot-s8".to_owned(),
                pts_device_mono_ns: 1_000,
                pc_ingest_cont_ns: 10_000,
                alignment: TimeAlignment::Anchored {
                    boot_id: "boot-s8".to_owned(),
                    boottime_ns: 50_000,
                    half_width_ns: 500,
                    semantics: TimestampSemantics::VideoEncoderInputEstimate,
                },
                is_keyframe: true,
                gop_id: 0,
                mp4_sample_offset: 0,
                mp4_sample_size: 100,
            },
            FrameRecord {
                frame_index: 1,
                boot_id: "boot-s8".to_owned(),
                pts_device_mono_ns: 2_000,
                pc_ingest_cont_ns: 20_000,
                alignment: TimeAlignment::Anchored {
                    boot_id: "boot-s8".to_owned(),
                    boottime_ns: 60_000,
                    half_width_ns: 500,
                    semantics: TimestampSemantics::VideoEncoderInputEstimate,
                },
                is_keyframe: false,
                gop_id: 0,
                mp4_sample_offset: 100,
                mp4_sample_size: 100,
            },
        ];
        let window = WindowSpec::from_frame_indices(&source_frames, 0, 1).expect("window");
        let params = ExportTrimParams {
            bundle_id_or_path: "b1".to_owned(),
            in_frame_index: 0,
            out_frame_index: 1,
            dest_dir: None,
            holder_pid: None,
            burn_taps: false,
            include_share_mp4: false,
            trash_original: false,
        };

        let out = build_trimmed_frame_records(&track, &source_frames, &window, &params);
        assert_eq!(out.len(), 3, "all 3 MP4 frames must be indexed");
        assert!(
            matches!(
                out[0].alignment,
                TimeAlignment::Anchored {
                    boottime_ns: 50_000,
                    ..
                }
            ),
            "matched frame 0 keeps its original anchor"
        );
        assert!(
            matches!(
                out[2].alignment,
                TimeAlignment::Unanchored {
                    reason: UncertaintyReason::NoClockAnchor,
                    ..
                }
            ),
            "unmatched frame must be Unanchored, never fabricated Anchored: {:?}",
            out[2].alignment
        );
    }

    #[test]
    fn compute_epoch_frame_ranges_splits_multi_epoch_reboot_cleanly() {
        let make_frame = |frame_index: u64, boot_id: &str, bt: i64| FrameRecord {
            frame_index,
            boot_id: boot_id.to_owned(),
            pts_device_mono_ns: bt,
            pc_ingest_cont_ns: bt,
            alignment: TimeAlignment::Anchored {
                boot_id: boot_id.to_owned(),
                boottime_ns: bt,
                half_width_ns: 1_000,
                semantics: TimestampSemantics::VideoEncoderInputEstimate,
            },
            is_keyframe: frame_index == 0 || frame_index == 3,
            gop_id: u64::from(frame_index >= 3),
            mp4_sample_offset: 0,
            mp4_sample_size: 100,
        };
        let frames = vec![
            make_frame(0, "boot-0", 100_000_000_000),
            make_frame(1, "boot-0", 101_000_000_000),
            make_frame(2, "boot-0", 102_000_000_000),
            make_frame(3, "boot-1", 5_000_000_000),
            make_frame(4, "boot-1", 6_000_000_000),
        ];
        let ranges = super::compute_epoch_frame_ranges(&frames, &[]);
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].epoch_index, 0);
        assert_eq!(ranges[0].boot_id, "boot-0");
        assert_eq!(ranges[0].start_frame_index, 0);
        assert_eq!(ranges[0].end_frame_index, 2);
        assert_eq!(ranges[0].start_boottime_ns, 100_000_000_000);
        assert_eq!(ranges[0].end_boottime_ns, 102_000_000_000);

        assert_eq!(ranges[1].epoch_index, 1);
        assert_eq!(ranges[1].boot_id, "boot-1");
        assert_eq!(ranges[1].start_frame_index, 3);
        assert_eq!(ranges[1].end_frame_index, 4);
        assert_eq!(ranges[1].start_boottime_ns, 5_000_000_000);
        assert_eq!(ranges[1].end_boottime_ns, 6_000_000_000);
    }
}

#[cfg(test)]
mod concurrent_viewport_tests {
    use super::*;
    use arb_os::paths::ArbPaths;
    use serde_json::json;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn stalled_thumbnail_decode_allows_status_and_another_viewport_seek() {
        let tmp = tempfile::tempdir().unwrap();
        let server = crate::DaemonServer::start(crate::DaemonConfig::new(ArbPaths {
            data_dir: tmp.path().join("d"),
            cache_dir: tmp.path().join("c"),
            runtime_dir: tmp.path().join("r"),
        }))
        .unwrap();
        let state = &server.state;
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../fixtures/legacy-schema4/replay-trim-parity")
            .join("replay-20240101-000000-manual-feedface");
        let first = state.media.open_viewport(&fixture, None).unwrap();
        let second = state.media.open_viewport(&fixture, None).unwrap();
        let (ready, entered) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let (done, result) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                let response = state.media.filmstrip_with(
                    &MediaFilmstripParams {
                        viewport_id: first.viewport_id,
                        max_width: None,
                        max_height: None,
                    },
                    |_, _| {
                        ready.send(()).unwrap();
                        wait.recv_timeout(Duration::from_secs(10)).unwrap();
                        Err(MediaError::Codec("delayed test codec".into()))
                    },
                );
                assert!(response.is_err());
            });
            entered.recv_timeout(Duration::from_secs(5)).unwrap();
            scope.spawn(move || {
                assert_eq!(
                    state.dispatch("system.status", json!({})).unwrap()["active_viewports"],
                    2
                );
                let frame = state
                    .dispatch(
                        "media.seek_frame",
                        json!({"viewport_id":second.viewport_id,"frame_index":0}),
                    )
                    .unwrap();
                done.send(frame["frame_index"].clone()).unwrap();
            });
            let frame = result.recv_timeout(Duration::from_secs(2));
            release.send(()).unwrap();
            assert_eq!(frame.unwrap(), 0);
        });
    }
}
