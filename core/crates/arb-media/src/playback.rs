//! Interactive playback primitives: external filmstrip cache, bounded GOP-LRU decoded-frame
//! cache, latest-wins scrub controller, and audio-driven presentation clock with monotonic fallback.

use std::collections::{HashMap, VecDeque};
use std::fmt::Debug;
use std::fs::{self, File};
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::codec::{DecodedFrame, PixelFormat, RgbImage, VideoCodecBackend};
use crate::error::MediaError;
use crate::mp4::Mp4VideoTrack;

/// File magic header for external `filmstrip.bin` caches.
const FILMSTRIP_MAGIC: &[u8; 8] = b"ARBFS001";

/// FNV-1a 64-bit offset basis for integrity verification of `filmstrip.bin`.
const FNV1A_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a 64-bit prime multiplier.
const FNV1A_PRIME: u64 = 0x0100_0000_01b3;

/// Default maximum width for filmstrip thumbnails.
pub const DEFAULT_FILMSTRIP_MAX_WIDTH: u32 = 320;

/// Default maximum height for filmstrip thumbnails.
pub const DEFAULT_FILMSTRIP_MAX_HEIGHT: u32 = 180;

/// Configuration for generating and caching keyframe filmstrip thumbnails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilmstripConfig {
    /// Maximum thumbnail width in pixels.
    pub max_width: u32,
    /// Maximum thumbnail height in pixels.
    pub max_height: u32,
    /// Maximum worker threads used during parallel keyframe decode.
    pub max_threads: usize,
}

impl Default for FilmstripConfig {
    fn default() -> Self {
        let threads = std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get);
        Self {
            max_width: DEFAULT_FILMSTRIP_MAX_WIDTH,
            max_height: DEFAULT_FILMSTRIP_MAX_HEIGHT,
            max_threads: threads,
        }
    }
}

/// One decoded keyframe thumbnail stored inside [`FilmstripCache`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilmstripEntry {
    /// Zero-based keyframe index in the source MP4.
    pub frame_index: u64,
    /// Presentation timestamp of this keyframe in nanoseconds.
    pub pts_ns: i64,
    /// Scaled RGB24 thumbnail image.
    pub image: RgbImage,
}

/// Indicates whether a [`FilmstripCache`] was loaded from disk or freshly generated.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilmstripOrigin {
    /// Reloaded from an existing validated `<cache_dir>/<bundle_id>/filmstrip.bin` file.
    DiskCache,
    /// Newly decoded from `track` and persisted to `<cache_dir>/<bundle_id>/filmstrip.bin`.
    Generated,
}

/// Result returned by [`FilmstripCache::load_or_generate`].
#[derive(Debug, Clone)]
pub struct FilmstripLoadResult {
    /// Loaded or newly generated filmstrip cache.
    pub cache: FilmstripCache,
    /// Whether thumbnails came from disk or were freshly decoded.
    pub origin: FilmstripOrigin,
    /// External path where `filmstrip.bin` lives.
    pub cache_path: PathBuf,
}

/// External keyframe thumbnail cache stored outside immutable WORM bundle directories
/// (`<cache_dir>/<bundle_id>/filmstrip.bin`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilmstripCache {
    source_file_len: u64,
    source_frame_count: u64,
    entries: Vec<FilmstripEntry>,
}

impl FilmstripCache {
    /// Resolves `<cache_dir>/<bundle_id>/filmstrip.bin` after validating `bundle_id` against
    /// path traversal.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::InvalidArgument`] if `bundle_id` is empty or contains path separators
    /// or `..`.
    pub fn cache_path(cache_dir: &Path, bundle_id: &str) -> Result<PathBuf, MediaError> {
        validate_bundle_id(bundle_id)?;
        Ok(cache_dir.join(bundle_id).join("filmstrip.bin"))
    }

    /// Loads `<cache_dir>/<bundle_id>/filmstrip.bin` if valid for `track`, or generates it via
    /// parallel keyframe decoding and writes it atomically into the external cache directory.
    ///
    /// Never writes inside the bundle directory containing the source MP4.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::InvalidArgument`] for an invalid `bundle_id` or zero thumbnail
    /// dimensions, or [`MediaError::Codec`] / [`MediaError::Io`] on decode or disk failure.
    pub fn load_or_generate(
        cache_dir: impl AsRef<Path>,
        bundle_id: &str,
        track: &Mp4VideoTrack,
        config: FilmstripConfig,
        backend: &dyn VideoCodecBackend,
    ) -> Result<FilmstripLoadResult, MediaError> {
        let path = Self::cache_path(cache_dir.as_ref(), bundle_id)?;
        if path.exists()
            && let Ok(cached) = Self::load_from_file(&path, track.file_len(), track.frame_count())
        {
            return Ok(FilmstripLoadResult {
                cache: cached,
                origin: FilmstripOrigin::DiskCache,
                cache_path: path,
            });
        }

        let thumbs = backend.decode_keyframes_thumbnails(
            track,
            config.max_width,
            config.max_height,
            config.max_threads,
        )?;
        let mut entries = Vec::with_capacity(thumbs.len());
        for (frame_index, image) in thumbs {
            let pts_ns = track.frame(frame_index)?.pts_ns;
            entries.push(FilmstripEntry {
                frame_index,
                pts_ns,
                image,
            });
        }

        let cache = Self {
            source_file_len: track.file_len(),
            source_frame_count: track.frame_count(),
            entries,
        };
        cache.save_to_file(&path)?;

        Ok(FilmstripLoadResult {
            cache,
            origin: FilmstripOrigin::Generated,
            cache_path: path,
        })
    }

    /// Loads and validates a `filmstrip.bin` file from disk.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::Io`] if the file cannot be read, or [`MediaError::CacheCorrupt`] if
    /// magic, checksum, framing, or source video fingerprint does not match.
    pub fn load_from_file(
        path: impl AsRef<Path>,
        expected_source_file_len: u64,
        expected_frame_count: u64,
    ) -> Result<Self, MediaError> {
        let bytes = fs::read(path)?;
        if bytes.len() < 8 + 8 + 8 + 4 + 8 {
            return Err(MediaError::CacheCorrupt(
                "filmstrip cache file is truncated".to_owned(),
            ));
        }
        let body_len = bytes.len() - 8;
        let stored_checksum = read_u64_le(&bytes, body_len)?;
        let actual_checksum = fnv1a_64(&bytes[..body_len]);
        if stored_checksum != actual_checksum {
            return Err(MediaError::CacheCorrupt(
                "filmstrip cache checksum mismatch".to_owned(),
            ));
        }

        if &bytes[0..8] != FILMSTRIP_MAGIC {
            return Err(MediaError::CacheCorrupt(
                "filmstrip cache magic mismatch".to_owned(),
            ));
        }
        let source_file_len = read_u64_le(&bytes, 8)?;
        let source_frame_count = read_u64_le(&bytes, 16)?;
        if source_file_len != expected_source_file_len || source_frame_count != expected_frame_count
        {
            return Err(MediaError::CacheCorrupt(
                "filmstrip cache fingerprint does not match source track".to_owned(),
            ));
        }
        let entry_count = usize::try_from(read_u32_le(&bytes, 24)?)
            .map_err(|_| MediaError::CacheCorrupt("entry count overflow".to_owned()))?;
        let mut pos = 28usize;
        let mut entries = Vec::with_capacity(entry_count);

        for _ in 0..entry_count {
            let frame_index = read_u64_le(&bytes, pos)?;
            let pts_ns = read_i64_le(&bytes, pos + 8)?;
            let width = read_u32_le(&bytes, pos + 16)?;
            let height = read_u32_le(&bytes, pos + 20)?;
            let rgb_len = usize::try_from(read_u32_le(&bytes, pos + 24)?)
                .map_err(|_| MediaError::CacheCorrupt("rgb_len overflow".to_owned()))?;
            pos += 28;
            let end = pos
                .checked_add(rgb_len)
                .ok_or_else(|| MediaError::CacheCorrupt("entry offset overflow".to_owned()))?;
            if end > body_len {
                return Err(MediaError::CacheCorrupt(
                    "truncated filmstrip entry payload".to_owned(),
                ));
            }
            let expected_rgb = usize::try_from(width)
                .ok()
                .and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h)))
                .and_then(|px| px.checked_mul(3));
            if expected_rgb != Some(rgb_len) {
                return Err(MediaError::CacheCorrupt(
                    "filmstrip entry dimensions do not match rgb payload size".to_owned(),
                ));
            }
            entries.push(FilmstripEntry {
                frame_index,
                pts_ns,
                image: RgbImage {
                    width,
                    height,
                    data: bytes[pos..end].to_vec(),
                },
            });
            pos = end;
        }

        if pos != body_len {
            return Err(MediaError::CacheCorrupt(
                "trailing unparsed bytes in filmstrip cache".to_owned(),
            ));
        }

        Ok(Self {
            source_file_len,
            source_frame_count,
            entries,
        })
    }

    fn save_to_file(&self, path: &Path) -> Result<(), MediaError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut buf = Vec::new();
        buf.extend_from_slice(FILMSTRIP_MAGIC);
        buf.extend_from_slice(&self.source_file_len.to_le_bytes());
        buf.extend_from_slice(&self.source_frame_count.to_le_bytes());
        let entry_count_u32 = u32::try_from(self.entries.len())
            .map_err(|_| MediaError::CacheCorrupt("too many filmstrip entries".to_owned()))?;
        buf.extend_from_slice(&entry_count_u32.to_le_bytes());
        for entry in &self.entries {
            buf.extend_from_slice(&entry.frame_index.to_le_bytes());
            buf.extend_from_slice(&entry.pts_ns.to_le_bytes());
            buf.extend_from_slice(&entry.image.width.to_le_bytes());
            buf.extend_from_slice(&entry.image.height.to_le_bytes());
            let rgb_len = u32::try_from(entry.image.data.len()).map_err(|_| {
                MediaError::CacheCorrupt("thumbnail payload exceeds u32::MAX".to_owned())
            })?;
            buf.extend_from_slice(&rgb_len.to_le_bytes());
            buf.extend_from_slice(&entry.image.data);
        }
        let checksum = fnv1a_64(&buf);
        buf.extend_from_slice(&checksum.to_le_bytes());

        let tmp_path = path.with_extension("bin.tmp");
        {
            let file = File::create(&tmp_path)?;
            let mut writer = BufWriter::new(file);
            writer.write_all(&buf)?;
            writer.flush()?;
        }
        if let Err(rename_err) = fs::rename(&tmp_path, path) {
            if path.exists() {
                fs::remove_file(path)?;
                fs::rename(&tmp_path, path)?;
            } else {
                let _ = fs::remove_file(&tmp_path);
                return Err(MediaError::Io(rename_err));
            }
        }
        Ok(())
    }

    /// Returns all cached keyframe thumbnail entries sorted by `frame_index`.
    pub fn entries(&self) -> &[FilmstripEntry] {
        &self.entries
    }

    /// Returns the number of cached keyframe thumbnails.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if no thumbnails are cached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the nearest cached keyframe entry to `target_frame_index` for instant scrub preview.
    pub fn nearest_keyframe(&self, target_frame_index: u64) -> Option<&FilmstripEntry> {
        if self.entries.is_empty() {
            return None;
        }
        match self
            .entries
            .binary_search_by_key(&target_frame_index, |e| e.frame_index)
        {
            Ok(idx) => self.entries.get(idx),
            Err(0) => self.entries.first(),
            Err(idx) if idx >= self.entries.len() => self.entries.last(),
            Err(idx) => {
                let prev = &self.entries[idx - 1];
                let next = &self.entries[idx];
                if target_frame_index.saturating_sub(prev.frame_index)
                    <= next.frame_index.saturating_sub(target_frame_index)
                {
                    Some(prev)
                } else {
                    Some(next)
                }
            }
        }
    }
}

fn validate_bundle_id(bundle_id: &str) -> Result<(), MediaError> {
    if bundle_id.is_empty()
        || bundle_id == "."
        || bundle_id == ".."
        || bundle_id.contains('/')
        || bundle_id.contains('\\')
        || bundle_id.contains("..")
    {
        return Err(MediaError::InvalidArgument(format!(
            "invalid bundle_id for external filmstrip cache: {bundle_id:?}"
        )));
    }
    Ok(())
}

fn fnv1a_64(data: &[u8]) -> u64 {
    let mut hash = FNV1A_OFFSET_BASIS;
    for &byte in data {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV1A_PRIME);
    }
    hash
}

fn read_u32_le(data: &[u8], offset: usize) -> Result<u32, MediaError> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| MediaError::CacheCorrupt("u32 offset overflow".to_owned()))?;
    let arr: [u8; 4] = data
        .get(offset..end)
        .ok_or_else(|| MediaError::CacheCorrupt("truncated u32".to_owned()))?
        .try_into()
        .map_err(|_| MediaError::CacheCorrupt("short u32 slice".to_owned()))?;
    Ok(u32::from_le_bytes(arr))
}

fn read_u64_le(data: &[u8], offset: usize) -> Result<u64, MediaError> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| MediaError::CacheCorrupt("u64 offset overflow".to_owned()))?;
    let arr: [u8; 8] = data
        .get(offset..end)
        .ok_or_else(|| MediaError::CacheCorrupt("truncated u64".to_owned()))?
        .try_into()
        .map_err(|_| MediaError::CacheCorrupt("short u64 slice".to_owned()))?;
    Ok(u64::from_le_bytes(arr))
}

fn read_i64_le(data: &[u8], offset: usize) -> Result<i64, MediaError> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| MediaError::CacheCorrupt("i64 offset overflow".to_owned()))?;
    let arr: [u8; 8] = data
        .get(offset..end)
        .ok_or_else(|| MediaError::CacheCorrupt("truncated i64".to_owned()))?
        .try_into()
        .map_err(|_| MediaError::CacheCorrupt("short i64 slice".to_owned()))?;
    Ok(i64::from_le_bytes(arr))
}

/// Cache outcome returned by [`GopLruCache::fetch_frame`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CacheLookup {
    /// Requested frame was already resident in a cached GOP.
    Hit,
    /// Requested GOP was decoded on demand and inserted into the LRU cache.
    Miss,
}

/// Runtime telemetry snapshot for [`GopLruCache`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GopLruStats {
    /// Total frame fetches satisfied directly from cached GOPs.
    pub cache_hits: u64,
    /// Total frame fetches that triggered a GOP decode.
    pub cache_misses: u64,
    /// Number of GOPs evicted due to `max_gops` or `max_bytes` limits.
    pub evictions: u64,
    /// Number of GOPs currently resident in memory.
    pub cached_gops: usize,
    /// Total decoded pixel bytes currently resident in memory.
    pub cached_bytes: usize,
}

#[derive(Debug)]
struct CachedGop {
    format: PixelFormat,
    start_frame_index: u64,
    frames: Vec<DecodedFrame>,
    total_bytes: usize,
}

/// Bounded LRU cache of decoded video frames grouped by `gop_id`.
///
/// Decodes an entire GOP on miss so subsequent forward `+1` and backward `-1` frame steps within
/// the GOP are served in constant time with zero decoder re-invocation.
#[derive(Debug)]
pub struct GopLruCache {
    max_gops: usize,
    max_bytes: usize,
    lru_order: VecDeque<u64>,
    entries: HashMap<u64, CachedGop>,
    cached_bytes: usize,
    cache_hits: u64,
    cache_misses: u64,
    evictions: u64,
}

impl GopLruCache {
    /// Creates a new bounded GOP-LRU cache.
    ///
    /// Both `max_gops` and `max_bytes` are clamped to at least 1 GOP / 1 byte so the most
    /// recently decoded GOP can always be retained.
    pub fn new(max_gops: usize, max_bytes: usize) -> Self {
        Self {
            max_gops: max_gops.max(1),
            max_bytes: max_bytes.max(1),
            lru_order: VecDeque::new(),
            entries: HashMap::new(),
            cached_bytes: 0,
            cache_hits: 0,
            cache_misses: 0,
            evictions: 0,
        }
    }

    /// Returns a cached frame if its GOP is currently resident with matching `format` (or any
    /// format when `required_format` is `None`), without touching the decoder.
    pub fn peek_frame(
        &self,
        track: &Mp4VideoTrack,
        frame_index: u64,
        required_format: Option<PixelFormat>,
    ) -> Option<&DecodedFrame> {
        let info = track.frame(frame_index).ok()?;
        let gop = self.entries.get(&info.gop_id)?;
        if let Some(req) = required_format
            && gop.format != req
        {
            return None;
        }
        let local_idx = usize::try_from(frame_index.checked_sub(gop.start_frame_index)?).ok()?;
        gop.frames.get(local_idx)
    }

    /// Fetches `frame_index` in `output_format`, returning `(frame, CacheLookup::Hit)` on a cache
    /// hit or decoding the containing GOP on a miss.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::FrameOutOfRange`] if `frame_index` is invalid, or [`MediaError::Codec`]
    /// / [`MediaError::Io`] if decoding fails.
    pub fn fetch_frame(
        &mut self,
        track: &Mp4VideoTrack,
        frame_index: u64,
        output_format: PixelFormat,
        backend: &dyn VideoCodecBackend,
    ) -> Result<(&DecodedFrame, CacheLookup), MediaError> {
        let info = track.frame(frame_index)?;
        let gop_id = info.gop_id;

        let is_hit = self
            .entries
            .get(&gop_id)
            .is_some_and(|gop| gop.format == output_format);

        if is_hit {
            self.touch_gop(gop_id);
            self.cache_hits += 1;
            let frame = self.lookup_frame_unchecked(gop_id, frame_index)?;
            return Ok((frame, CacheLookup::Hit));
        }

        let decoded_gop = backend.decode_gop(track, gop_id, output_format)?;
        let Some(first) = decoded_gop.first() else {
            return Err(MediaError::Codec(format!(
                "backend returned empty GOP {gop_id}"
            )));
        };
        let start_frame_index = first.frame_index;
        let total_bytes: usize = decoded_gop.iter().map(DecodedFrame::byte_len).sum();

        if let Some(prev) = self.entries.remove(&gop_id) {
            self.cached_bytes = self.cached_bytes.saturating_sub(prev.total_bytes);
            self.lru_order.retain(|&id| id != gop_id);
        }

        while !self.lru_order.is_empty()
            && (self.entries.len() >= self.max_gops
                || self.cached_bytes.saturating_add(total_bytes) > self.max_bytes)
        {
            if let Some(evicted_id) = self.lru_order.pop_front()
                && let Some(evicted) = self.entries.remove(&evicted_id)
            {
                self.cached_bytes = self.cached_bytes.saturating_sub(evicted.total_bytes);
                self.evictions += 1;
            }
        }

        self.entries.insert(
            gop_id,
            CachedGop {
                format: output_format,
                start_frame_index,
                frames: decoded_gop,
                total_bytes,
            },
        );
        self.lru_order.push_back(gop_id);
        self.cached_bytes = self.cached_bytes.saturating_add(total_bytes);
        self.cache_misses += 1;

        let frame = self.lookup_frame_unchecked(gop_id, frame_index)?;
        Ok((frame, CacheLookup::Miss))
    }

    /// Returns a snapshot of cache hit/miss/eviction counters and memory footprint.
    pub fn stats(&self) -> GopLruStats {
        GopLruStats {
            cache_hits: self.cache_hits,
            cache_misses: self.cache_misses,
            evictions: self.evictions,
            cached_gops: self.entries.len(),
            cached_bytes: self.cached_bytes,
        }
    }

    /// Drops all cached GOPs while preserving cumulative hit/miss counters.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.lru_order.clear();
        self.cached_bytes = 0;
    }

    fn touch_gop(&mut self, gop_id: u64) {
        self.lru_order.retain(|&id| id != gop_id);
        self.lru_order.push_back(gop_id);
    }

    fn lookup_frame_unchecked(
        &self,
        gop_id: u64,
        frame_index: u64,
    ) -> Result<&DecodedFrame, MediaError> {
        let gop = self.entries.get(&gop_id).ok_or_else(|| {
            MediaError::Codec(format!("cached GOP {gop_id} vanished during lookup"))
        })?;
        let offset = frame_index
            .checked_sub(gop.start_frame_index)
            .and_then(|d| usize::try_from(d).ok())
            .ok_or_else(|| {
                MediaError::Codec(format!(
                    "frame {frame_index} outside cached GOP {gop_id} starting at {}",
                    gop.start_frame_index
                ))
            })?;
        gop.frames.get(offset).ok_or_else(|| {
            MediaError::Codec(format!(
                "frame {frame_index} missing from cached GOP {gop_id}"
            ))
        })
    }
}

/// Frame delivered by [`ScrubController`] carrying the explicit `is_approximate` badge contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubFrame {
    /// Latest-wins monotonic scrub generation identifier.
    pub generation: u64,
    /// Exact frame index requested by the user/UI.
    pub requested_frame_index: u64,
    /// Actual frame index represented by [`Self::data`] (equals `requested_frame_index` when exact,
    /// or nearest keyframe index when `is_approximate == true`).
    pub displayed_frame_index: u64,
    /// `true` while showing an approximate keyframe thumbnail; UI must display an approximate badge.
    pub is_approximate: bool,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Pixel format of [`Self::data`].
    pub format: PixelFormat,
    /// Tightly packed pixel buffer.
    pub data: Vec<u8>,
}

/// Interactive timeline scrub controller with latest-wins cancellation semantics, instant
/// approximate preview via [`FilmstripCache`], and exact frame settling via [`GopLruCache`].
#[derive(Debug)]
pub struct ScrubController {
    generation: AtomicU64,
    gop_cache: GopLruCache,
    filmstrip: Option<FilmstripCache>,
}

impl ScrubController {
    /// Creates a new scrub controller backed by `gop_cache` and an optional [`FilmstripCache`].
    pub const fn new(gop_cache: GopLruCache, filmstrip: Option<FilmstripCache>) -> Self {
        Self {
            generation: AtomicU64::new(0),
            gop_cache,
            filmstrip,
        }
    }

    /// Attaches or replaces the [`FilmstripCache`] used for instant approximate previews.
    pub fn set_filmstrip(&mut self, filmstrip: FilmstripCache) {
        self.filmstrip = Some(filmstrip);
    }

    /// Returns the current latest-wins generation counter.
    pub fn latest_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Increments the generation counter so any stale in-flight scrub settles are cancelled.
    pub fn cancel_pending(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Returns a shared reference to the underlying [`GopLruCache`].
    pub const fn gop_cache(&self) -> &GopLruCache {
        &self.gop_cache
    }

    /// Returns a mutable reference to the underlying [`GopLruCache`].
    pub const fn gop_cache_mut(&mut self) -> &mut GopLruCache {
        &mut self.gop_cache
    }

    /// Handles an interactive scrub move to `target_frame_index` with latest-wins semantics.
    ///
    /// Returns immediately without invoking the decoder:
    /// - If `target_frame_index` is already cached in [`GopLruCache`], returns that exact frame
    ///   with `is_approximate = false`.
    /// - Otherwise, if a [`FilmstripCache`] is attached, returns the nearest keyframe thumbnail
    ///   with `is_approximate = true` (explicit badge contract).
    /// - Otherwise returns `Ok(None)`.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::FrameOutOfRange`] if `target_frame_index >= track.frame_count()`.
    pub fn preview_scrub(
        &self,
        track: &Mp4VideoTrack,
        target_frame_index: u64,
    ) -> Result<Option<ScrubFrame>, MediaError> {
        let _ = track.frame(target_frame_index)?;
        let generation = self.cancel_pending();

        if let Some(cached) = self.gop_cache.peek_frame(track, target_frame_index, None) {
            return Ok(Some(ScrubFrame {
                generation,
                requested_frame_index: target_frame_index,
                displayed_frame_index: cached.frame_index,
                is_approximate: false,
                width: cached.width,
                height: cached.height,
                format: cached.format,
                data: cached.data.clone(),
            }));
        }

        if let Some(entry) = self
            .filmstrip
            .as_ref()
            .and_then(|fs| fs.nearest_keyframe(target_frame_index))
        {
            return Ok(Some(ScrubFrame {
                generation,
                requested_frame_index: target_frame_index,
                displayed_frame_index: entry.frame_index,
                is_approximate: true,
                width: entry.image.width,
                height: entry.image.height,
                format: PixelFormat::Rgb24,
                data: entry.image.data.clone(),
            }));
        }

        Ok(None)
    }

    /// Decodes the exact frame at `target_frame_index` through [`GopLruCache`] and returns a
    /// [`ScrubFrame`] with `is_approximate = false`.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::FrameOutOfRange`], [`MediaError::Codec`], or [`MediaError::Io`].
    pub fn settle_exact(
        &mut self,
        track: &Mp4VideoTrack,
        target_frame_index: u64,
        output_format: PixelFormat,
        backend: &dyn VideoCodecBackend,
    ) -> Result<ScrubFrame, MediaError> {
        let generation = self.cancel_pending();
        let (frame, _) =
            self.gop_cache
                .fetch_frame(track, target_frame_index, output_format, backend)?;
        Ok(ScrubFrame {
            generation,
            requested_frame_index: target_frame_index,
            displayed_frame_index: frame.frame_index,
            is_approximate: false,
            width: frame.width,
            height: frame.height,
            format: frame.format,
            data: frame.data.clone(),
        })
    }

    /// Decodes exact `target_frame_index` only if `expected_generation` is still the latest
    /// generation (latest-wins cancellation). Returns `Ok(None)` if superseded before or after decode.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::FrameOutOfRange`], [`MediaError::Codec`], or [`MediaError::Io`].
    pub fn settle_exact_if_latest(
        &mut self,
        expected_generation: u64,
        track: &Mp4VideoTrack,
        target_frame_index: u64,
        output_format: PixelFormat,
        backend: &dyn VideoCodecBackend,
    ) -> Result<Option<ScrubFrame>, MediaError> {
        if self.generation.load(Ordering::SeqCst) != expected_generation {
            return Ok(None);
        }
        let (frame, _) =
            self.gop_cache
                .fetch_frame(track, target_frame_index, output_format, backend)?;
        if self.generation.load(Ordering::SeqCst) != expected_generation {
            return Ok(None);
        }
        Ok(Some(ScrubFrame {
            generation: expected_generation,
            requested_frame_index: target_frame_index,
            displayed_frame_index: frame.frame_index,
            is_approximate: false,
            width: frame.width,
            height: frame.height,
            format: frame.format,
            data: frame.data.clone(),
        }))
    }
}

/// Calibration provenance of the playback A/V synchronization offset.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvOffsetStatus {
    /// Offset has not been empirically calibrated (`av_offset_ms == 0` default, honestly disclosed).
    #[default]
    Uncalibrated,
    /// Offset was calibrated for the capture device/session.
    Calibrated,
    /// Offset was explicitly overridden by the user.
    UserOverride,
}

impl From<arb_core::bundle::AvOffsetStatus> for AvOffsetStatus {
    fn from(value: arb_core::bundle::AvOffsetStatus) -> Self {
        match value {
            arb_core::bundle::AvOffsetStatus::Calibrated => Self::Calibrated,
            arb_core::bundle::AvOffsetStatus::UserOverridden => Self::UserOverride,
            _ => Self::Uncalibrated,
        }
    }
}

impl From<AvOffsetStatus> for arb_core::bundle::AvOffsetStatus {
    fn from(value: AvOffsetStatus) -> Self {
        match value {
            AvOffsetStatus::Uncalibrated => Self::Uncalibrated,
            AvOffsetStatus::Calibrated => Self::Calibrated,
            AvOffsetStatus::UserOverride => Self::UserOverridden,
        }
    }
}

/// Active clock source driving [`PresentationClock`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClockDriveMode {
    /// Driven directly by the external audio output hardware/stream clock.
    AudioMaster,
    /// Driven by monotonic host elapsed time (no audio device, muted, or video-only bundle).
    MonotonicFallback,
}

/// Monotonic time source abstraction for deterministic testing and real host playback.
pub trait MonotonicTimeSource: Debug + Send + Sync {
    /// Returns monotonically non-decreasing elapsed nanoseconds from an arbitrary origin.
    fn now_ns(&self) -> i64;
}

/// Production monotonic time source backed by [`std::time::Instant`].
#[derive(Debug)]
pub struct SystemMonotonicClock {
    origin: Instant,
}

impl SystemMonotonicClock {
    /// Creates a new [`SystemMonotonicClock`] anchored at `Instant::now()`.
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemMonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl MonotonicTimeSource for SystemMonotonicClock {
    fn now_ns(&self) -> i64 {
        i64::try_from(self.origin.elapsed().as_nanos()).unwrap_or(i64::MAX)
    }
}

/// External audio playback clock queried by [`PresentationClock`].
pub trait AudioClockSource: Debug + Send + Sync {
    /// Returns the current audio playback position in media nanoseconds, or `None` when no audio
    /// output device is active, audio is absent, or audio playback is muted/unavailable.
    fn position_ns(&self) -> Option<i64>;
}

/// Snapshot returned by [`PresentationClock::sample_position`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockReading {
    /// Current presentation position on the media timeline in nanoseconds.
    pub media_position_ns: i64,
    /// Whether this reading came from the audio master clock or monotonic fallback.
    pub drive_mode: ClockDriveMode,
    /// Active A/V offset in milliseconds.
    pub av_offset_ms: i64,
    /// Honesty/calibration status of [`Self::av_offset_ms`].
    pub av_offset_status: AvOffsetStatus,
}

/// Media presentation clock driven by an external audio clock when available, with seamless
/// fallback to monotonic elapsed time when audio output is absent, muted, or unavailable.
///
/// Defaults strictly to `av_offset_ms = 0` and [`AvOffsetStatus::Uncalibrated`] — never hardcodes
/// unverified platform offsets.
#[derive(Debug)]
pub struct PresentationClock {
    time_source: Box<dyn MonotonicTimeSource>,
    av_offset_ms: i64,
    av_offset_status: AvOffsetStatus,
    playing: bool,
    speed_permille: u32,
    base_media_ns: i64,
    anchor_mono_ns: i64,
    last_drive_mode: ClockDriveMode,
}

impl Default for PresentationClock {
    fn default() -> Self {
        Self::new()
    }
}

impl PresentationClock {
    /// Creates a paused [`PresentationClock`] using [`SystemMonotonicClock`], `av_offset_ms = 0`,
    /// and [`AvOffsetStatus::Uncalibrated`].
    pub fn new() -> Self {
        Self::with_time_source(Box::new(SystemMonotonicClock::new()))
    }

    /// Creates a paused [`PresentationClock`] with a custom [`MonotonicTimeSource`].
    pub fn with_time_source(time_source: Box<dyn MonotonicTimeSource>) -> Self {
        let now = time_source.now_ns();
        Self {
            time_source,
            av_offset_ms: 0,
            av_offset_status: AvOffsetStatus::Uncalibrated,
            playing: false,
            speed_permille: 1_000,
            base_media_ns: 0,
            anchor_mono_ns: now,
            last_drive_mode: ClockDriveMode::MonotonicFallback,
        }
    }

    /// Returns the configured A/V offset in milliseconds (default `0`).
    pub const fn av_offset_ms(&self) -> i64 {
        self.av_offset_ms
    }

    /// Returns the calibration provenance of [`Self::av_offset_ms`].
    pub const fn av_offset_status(&self) -> AvOffsetStatus {
        self.av_offset_status
    }

    /// Updates the A/V offset and its explicit [`AvOffsetStatus`].
    pub const fn set_av_offset(&mut self, av_offset_ms: i64, status: AvOffsetStatus) {
        self.av_offset_ms = av_offset_ms;
        self.av_offset_status = status;
    }

    /// Returns `true` if the presentation clock is currently advancing.
    pub const fn is_playing(&self) -> bool {
        self.playing
    }

    /// Sets playback speed in permille (`1000` = 1.0x, `500` = 0.5x, `2000` = 2.0x).
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::InvalidArgument`] if `speed_permille == 0`.
    pub fn set_speed_permille(
        &mut self,
        speed_permille: u32,
        audio_clock: Option<&dyn AudioClockSource>,
    ) -> Result<(), MediaError> {
        if speed_permille == 0 {
            return Err(MediaError::InvalidArgument(
                "playback speed_permille must be non-zero".to_owned(),
            ));
        }
        let reading = self.sample_position(audio_clock);
        self.base_media_ns = reading.media_position_ns;
        self.anchor_mono_ns = self.time_source.now_ns();
        self.speed_permille = speed_permille;
        Ok(())
    }

    /// Starts or resumes playback anchored at `start_media_ns`.
    pub fn play(&mut self, start_media_ns: i64) {
        self.base_media_ns = start_media_ns;
        self.anchor_mono_ns = self.time_source.now_ns();
        self.playing = true;
    }

    /// Pauses playback and freezes the current position (sampling `audio_clock` if active).
    pub fn pause(&mut self, audio_clock: Option<&dyn AudioClockSource>) -> i64 {
        let reading = self.sample_position(audio_clock);
        self.base_media_ns = reading.media_position_ns;
        self.anchor_mono_ns = self.time_source.now_ns();
        self.playing = false;
        reading.media_position_ns
    }

    /// Seeks the clock to `media_ns` while preserving playing/paused state.
    pub fn seek(&mut self, media_ns: i64) {
        self.base_media_ns = media_ns;
        self.anchor_mono_ns = self.time_source.now_ns();
    }

    /// Samples the current media presentation position.
    ///
    /// Uses `audio_clock` whenever playing and `audio_clock.position_ns()` returns `Some(_)`,
    /// applying `av_offset_ms` and synchronizing the monotonic anchor so sudden audio device loss
    /// transitions seamlessly to [`ClockDriveMode::MonotonicFallback`].
    pub fn sample_position(&mut self, audio_clock: Option<&dyn AudioClockSource>) -> ClockReading {
        let now_mono = self.time_source.now_ns();
        if !self.playing {
            return ClockReading {
                media_position_ns: self.base_media_ns,
                drive_mode: self.last_drive_mode,
                av_offset_ms: self.av_offset_ms,
                av_offset_status: self.av_offset_status,
            };
        }

        if let Some(audio_pos_ns) = audio_clock.and_then(AudioClockSource::position_ns) {
            let offset_ns = self.av_offset_ms.saturating_mul(1_000_000);
            let media_position_ns = audio_pos_ns.saturating_add(offset_ns);
            // Keep monotonic fallback continuously anchored to audio master for glitch-free fallback.
            self.base_media_ns = media_position_ns;
            self.anchor_mono_ns = now_mono;
            self.last_drive_mode = ClockDriveMode::AudioMaster;
            return ClockReading {
                media_position_ns,
                drive_mode: ClockDriveMode::AudioMaster,
                av_offset_ms: self.av_offset_ms,
                av_offset_status: self.av_offset_status,
            };
        }

        let elapsed_mono_ns = now_mono.saturating_sub(self.anchor_mono_ns).max(0);
        let scaled_delta =
            i128::from(elapsed_mono_ns) * i128::from(self.speed_permille) / 1_000i128;
        let delta_ns = i64::try_from(scaled_delta).unwrap_or(i64::MAX);
        let media_position_ns = self.base_media_ns.saturating_add(delta_ns);
        self.last_drive_mode = ClockDriveMode::MonotonicFallback;

        ClockReading {
            media_position_ns,
            drive_mode: ClockDriveMode::MonotonicFallback,
            av_offset_ms: self.av_offset_ms,
            av_offset_status: self.av_offset_status,
        }
    }

    /// Resolves the active `frame_index` whose `pts_ns` is greatest `<= position_ns`.
    pub fn frame_index_for_position(track: &Mp4VideoTrack, position_ns: i64) -> Option<u64> {
        let frames = track.frames();
        if frames.is_empty() {
            return None;
        }
        let idx = frames.partition_point(|f| f.pts_ns <= position_ns);
        if idx == 0 {
            Some(frames[0].frame_index)
        } else {
            Some(frames[idx - 1].frame_index)
        }
    }
}
