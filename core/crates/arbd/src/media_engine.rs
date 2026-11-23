//! High-bandwidth SHM viewport playback engine and non-destructive trim/PNG exporters.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use arb_core::bundle::{
    DeviceInfo, EpochSpan, FrameRecord, Provenance, RecordBucket, RequestedWindow, StreamStatus,
    StreamsSummary, UnanchoredSummary, VideoStreamMeta,
};
use arb_core::selection::{SelectionPolicy, WindowSpec, select_for_trim_with_truncated};
use arb_core::time::{DiscontinuityEvent, TimeAlignment, TimestampSemantics};
use arb_media::codec::{PixelFormat as MediaPixelFormat, VideoCodecBackend, default_codec_backend};
use arb_media::mp4::Mp4VideoTrack;
use arb_media::playback::{
    FilmstripCache, FilmstripConfig, FilmstripOrigin, GopLruCache, ScrubController,
};
use arb_media::trim::{PngExportMetadata, export_frame_png_with_metadata, trim_video};
use arb_os::shm::{FrameDescriptor, PixelFormat as ShmPixelFormat, ShmFrameRingWriter};
use arb_storage::bundle_io::{
    BundleReader, MediaPayload, TimeRange, V2BundleDraft, V2BundleWriter, open_bundle,
};
use arb_storage::catalog::{Catalog, CatalogFormatVersion, LeaseHolderKind, NewBundleEntry};

use crate::error::DaemonError;
use crate::protocol::{
    ExportPngParams, ExportPngResponse, ExportTrimParams, ExportTrimResponse, FilmstripEntryMeta,
    MediaFilmstripParams, MediaFilmstripResponse, MediaFrameRequestParams,
    MediaOpenViewportResponse, MediaScrubResponse, MediaSeekFrameResponse,
};
use crate::time_util::{next_id, utc_now_iso8601};

const DEFAULT_GOP_CACHE_GOPS: usize = 4;
const DEFAULT_GOP_CACHE_BYTES: usize = 128 * 1024 * 1024;
const MIN_SHM_SLOT_CAPACITY: usize = 4096;

#[derive(Debug)]
struct OpenViewport {
    bundle_id: String,
    shm_path: PathBuf,
    width: u32,
    height: u32,
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
    viewports: BTreeMap<String, OpenViewport>,
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
            viewports: BTreeMap::new(),
        }
    }

    /// Returns the number of currently open viewports.
    #[must_use]
    pub fn active_viewport_count(&self) -> usize {
        self.viewports.len()
    }

    /// Opens a triple-buffered SHM viewport for `bundle_dir` and returns its descriptor.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::NotFound`] if the bundle has no video track, or OS/media errors.
    pub fn open_viewport(
        &mut self,
        bundle_dir: &Path,
        bundle_id_override: Option<&str>,
    ) -> Result<MediaOpenViewportResponse, DaemonError> {
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
        let scrub = ScrubController::new(
            GopLruCache::new(DEFAULT_GOP_CACHE_GOPS, DEFAULT_GOP_CACHE_BYTES),
            None,
        );

        self.viewports.insert(
            viewport_id.clone(),
            OpenViewport {
                bundle_id: bundle_id.clone(),
                shm_path: shm_path.clone(),
                width,
                height,
                track,
                frame_boottimes,
                scrub,
                shm_writer,
            },
        );

        Ok(MediaOpenViewportResponse {
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
        &mut self,
        params: &MediaFilmstripParams,
    ) -> Result<MediaFilmstripResponse, DaemonError> {
        let start = Instant::now();
        let vp = self.viewports.get_mut(&params.viewport_id).ok_or_else(|| {
            DaemonError::NotFound(format!("viewport `{}` not found", params.viewport_id))
        })?;

        let mut cfg = FilmstripConfig::default();
        if let Some(mw) = params.max_width {
            cfg.max_width = mw;
        }
        if let Some(mh) = params.max_height {
            cfg.max_height = mh;
        }

        let loaded = FilmstripCache::load_or_generate(
            &self.cache_dir,
            &vp.bundle_id,
            &vp.track,
            cfg,
            self.backend.as_ref(),
        )?;

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
        &mut self,
        params: &MediaFrameRequestParams,
    ) -> Result<MediaScrubResponse, DaemonError> {
        let start = Instant::now();
        let vp = self.viewports.get_mut(&params.viewport_id).ok_or_else(|| {
            DaemonError::NotFound(format!("viewport `{}` not found", params.viewport_id))
        })?;

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
        &mut self,
        params: &MediaFrameRequestParams,
    ) -> Result<MediaSeekFrameResponse, DaemonError> {
        let start = Instant::now();
        let vp = self.viewports.get_mut(&params.viewport_id).ok_or_else(|| {
            DaemonError::NotFound(format!("viewport `{}` not found", params.viewport_id))
        })?;

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
            width: vp.width,
            height: vp.height,
            cache_hits: stats.cache_hits,
            cache_misses: stats.cache_misses,
            elapsed_ms,
        })
    }

    /// Closes an open viewport and cleans up its SHM backing file.
    pub fn close_viewport(&mut self, viewport_id: &str) -> bool {
        self.viewports.remove(viewport_id).is_some()
    }

    /// Closes all active viewports during daemon shutdown.
    pub fn close_all(&mut self) {
        self.viewports.clear();
    }

    /// Exports an exact integer `frame_index` as a PNG with `arb:*` provenance `tEXt` chunks.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if the bundle or video track cannot be opened, or PNG export fails.
    pub fn export_png(
        &self,
        bundle_dir: &Path,
        params: &ExportPngParams,
    ) -> Result<ExportPngResponse, DaemonError> {
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
            self.backend.as_ref(),
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
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if the frame range is invalid, video trim fails, or atomic
    /// v2 bundle write fails.
    pub fn export_trim(
        &self,
        catalog: &Catalog,
        data_dir: &Path,
        source_bundle_dir: &Path,
        source_catalog_id: Option<&str>,
        params: &ExportTrimParams,
    ) -> Result<ExportTrimResponse, DaemonError> {
        let now_utc = utc_now_iso8601();
        let holder_pid = params.holder_pid.unwrap_or_else(std::process::id);

        // Acquire a Job Lease on the source bundle when registered in the catalog.
        let _lease_guard = if let Some(src_id) = source_catalog_id {
            let lease_id =
                catalog.acquire_lease(src_id, LeaseHolderKind::ExportJob, holder_pid, &now_utc)?;
            Some(LeaseDropGuard { catalog, lease_id })
        } else {
            None
        };

        let reader = open_bundle(source_bundle_dir)?;
        let summary = reader.manifest_summary();
        let parent_bundle_id = source_catalog_id.unwrap_or(&summary.bundle_id).to_owned();
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
        let tmp_dir = data_dir.join("tmp");
        fs::create_dir_all(&tmp_dir)?;
        let tmp_trimmed_mp4 = tmp_dir.join(format!("{trim_job_id}.mp4"));
        let _tmp_guard = FileCleanupGuard(tmp_trimmed_mp4.clone());

        let trim_report = trim_video(
            &video_path,
            &tmp_trimmed_mp4,
            params.in_frame_index,
            params.out_frame_index,
            self.backend.as_ref(),
        )?;

        let trimmed_track = Mp4VideoTrack::open(&tmp_trimmed_mp4)?;
        let trimmed_frames =
            build_trimmed_frame_records(&trimmed_track, &source_frames, &frame_window, params);

        let policy = SelectionPolicy::default();
        let (trimmed_logs, main_log_count, unanchored_log_count, log_truncated) =
            select_trimmed_logs(reader.as_ref(), summary, &frame_window, &policy)?;
        let (trimmed_taps, main_tap_count, unanchored_tap_count, tap_truncated) =
            select_trimmed_taps(reader.as_ref(), &frame_window, &policy)?;
        let trimmed_discs =
            filter_discontinuities_for_window(reader.discontinuities()?, &frame_window);

        let unanchored_summary = UnanchoredSummary {
            included_count: u64::try_from(unanchored_log_count + unanchored_tap_count)
                .unwrap_or(u64::MAX),
            truncated_count: u64::try_from(log_truncated.saturating_add(tap_truncated))
                .unwrap_or(u64::MAX),
        };

        let dest_parent = params
            .dest_dir
            .clone()
            .unwrap_or_else(|| catalog.managed_bundles_root().to_path_buf());
        let dest_bundle_dir = dest_parent.join(&new_bundle_id);

        let draft = build_trimmed_bundle_draft(
            &new_bundle_id,
            &parent_bundle_id,
            &now_utc,
            summary,
            &frame_window,
            params,
            trim_report.output_frame_count,
            unanchored_summary,
            tmp_trimmed_mp4,
            trimmed_frames,
            trimmed_discs,
            trimmed_logs,
            trimmed_taps,
        );

        let _manifest = V2BundleWriter::write(&dest_bundle_dir, &trim_job_id, &draft)?;
        catalog.register_bundle(&NewBundleEntry {
            bundle_id: new_bundle_id.clone(),
            path: dest_bundle_dir.clone(),
            format_version: CatalogFormatVersion::V2,
            parent_bundle_id: Some(parent_bundle_id),
            created_at_utc: now_utc,
        })?;

        Ok(ExportTrimResponse {
            bundle_id: new_bundle_id,
            bundle_path: dest_bundle_dir,
            trim_report,
            unanchored_summary,
            main_log_count,
            unanchored_log_count,
            main_tap_count,
            unanchored_tap_count,
        })
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
    output_frame_count: u64,
    unanchored_summary: UnanchoredSummary,
    tmp_trimmed_mp4: PathBuf,
    trimmed_frames: Vec<FrameRecord>,
    trimmed_discs: Vec<DiscontinuityEvent>,
    trimmed_logs: Vec<arb_core::bundle::LogRecord>,
    trimmed_taps: Vec<arb_core::bundle::TapRecord>,
) -> V2BundleDraft {
    let device = summary.device.clone().unwrap_or_else(|| DeviceInfo {
        serial: "unknown".to_owned(),
        model: "unknown".to_owned(),
        api_level: 0,
        is_emulator: false,
    });
    V2BundleDraft {
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
                frame_count: output_frame_count,
                tail_may_be_incomplete: false,
                semantics: TimestampSemantics::VideoEncoderInputEstimate,
            }),
            // The trimmed bundle carries no `audio.m4a` (audio is not cut), and
            // `verify_integrity` requires the file for a `Present` audio stream, so a
            // `Present` source is downgraded to `Partial`.
            audio: summary.streams.audio.clone().map(|mut audio| {
                if audio.status == StreamStatus::Present {
                    audio.status = StreamStatus::Partial;
                }
                audio
            }),
            logcat: summary.streams.logcat.clone(),
            input: summary.streams.input.clone(),
        },
        unanchored_summary,
        video_mp4: Some(MediaPayload::File(tmp_trimmed_mp4)),
        audio_m4a: None,
        frames: trimmed_frames,
        discontinuities: trimmed_discs,
        logs: trimmed_logs,
        taps: trimmed_taps,
        clock_anchors: Vec::new(),
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
            None => FrameRecord {
                frame_index,
                boot_id: frame_window.boot_id().to_owned(),
                pts_device_mono_ns: mp4_f.pts_ns,
                pc_ingest_cont_ns: frame_window.ingest_start_cont_ns(),
                alignment: TimeAlignment::Anchored {
                    boot_id: frame_window.boot_id().to_owned(),
                    boottime_ns: frame_window.start_boottime_ns(),
                    half_width_ns: 0,
                    semantics: TimestampSemantics::VideoEncoderInputEstimate,
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
    let source_main = reader.query_logs(Some(RecordBucket::Main), TimeRange::all(), None)?;
    let source_unanchored =
        reader.query_logs(Some(RecordBucket::Unanchored), TimeRange::all(), None)?;
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
                (None, None) => true,
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
        is_legacy: reader.is_legacy(),
        created_at_utc: summary.created_at_utc.clone(),
        provenance: summary.provenance.clone(),
        device: summary.device.clone(),
        streams: summary.streams.clone(),
        unanchored_summary: summary.unanchored_summary,
        epochs: summary.epochs.clone(),
        frame_count: frames.len(),
        keyframe_count,
        log_main_count: main_logs.len(),
        log_unanchored_count: unanchored_logs.len(),
        tap_main_count: main_taps.len(),
        tap_unanchored_count: unanchored_taps.len(),
        discontinuity_count: discs.len(),
        video_path: reader.video_path(),
        audio_path: reader.audio_path(),
    })
}
