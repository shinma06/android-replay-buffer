//! Triple-buffered shared-memory frame ring for high-FPS zero-JSON video delivery.
//!
//! `arbd` publishes decoded frames through [`ShmFrameRingWriter`] into a
//! file-backed memory map containing 3 cache-line-aligned slots, while IDE or
//! viewer clients attach with [`ShmFrameRingReader`]. Each slot uses an atomic
//! `Release`/`Acquire` sequence lock paired with an atomic published slot index
//! and generation counter so readers never observe torn frames and always see
//! the exact `frame_index` and `is_approximate` preview badge state.
//!
//! # Memory-model soundness (H6)
//!
//! The ring is shared between threads *and* processes, so a writer may wrap
//! around all 3 slots while a reader is still copying one of them. A seqlock
//! only *detects* such overlap after the fact; under the Rust memory model the
//! overlapping accesses themselves must therefore be atomic, otherwise the
//! concurrent plain `memcpy` / volatile reads are a data race (undefined
//! behaviour) even if the torn result is discarded afterwards. Every access to
//! slot metadata and payload bytes is consequently an atomic `Relaxed`
//! `AtomicU64` word access (including the padded final word), bracketed by the
//! per-slot `Acquire`/`Release` sequence counter and fences. The binary wire
//! layout is unchanged (atomics have the same size and alignment as the plain
//! integers they replace), so the Kotlin `ShmFrameRingReader` stays compatible.

use std::fs::{self, File, OpenOptions};
use std::mem::{align_of, offset_of, size_of};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering, fence};

use memmap2::{Mmap, MmapMut};

use crate::error::OsError;

/// Number of frame slots in the triple-buffered ring.
pub const SHM_SLOT_COUNT: usize = 3;

const SHM_SLOT_COUNT_U8: u8 = 3;
const SHM_SLOT_COUNT_U32: u32 = 3;
const RING_MAGIC: [u8; 8] = *b"ARBSHM01";
const RING_VERSION: u32 = 1;
const CACHE_LINE_BYTES: usize = 64;
const RING_HEADER_BYTES: usize = 64;
const SLOT_HEADER_BYTES: usize = 64;
const HEADER_REGION_BYTES: usize = RING_HEADER_BYTES + SHM_SLOT_COUNT * SLOT_HEADER_BYTES;
const FLAG_IS_APPROXIMATE: u32 = 1 << 0;
const NO_PUBLISHED_SLOT: u32 = u32::MAX;
const SEQLOCK_CONTENTION: &str = "shared-memory frame seqlock did not stabilize under contention";
const MAX_SEQLOCK_RETRIES: usize = 64;
static TMP_SHM_SEQ: AtomicU64 = AtomicU64::new(1);

/// Pixel memory layout of frames carried through the shared-memory ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[repr(u32)]
pub enum PixelFormat {
    /// 32-bit BGRA packed pixels (4 bytes/pixel).
    Bgra8 = 1,
    /// 32-bit RGBA packed pixels (4 bytes/pixel).
    Rgba8 = 2,
    /// 24-bit RGB packed pixels (3 bytes/pixel).
    Rgb24 = 3,
    /// YUV 4:2:0 semi-planar (Y plane followed by interleaved UV plane).
    Nv12 = 4,
}

impl PixelFormat {
    /// Converts a raw wire integer into a [`PixelFormat`], returning `None` for
    /// unknown values.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            1 => Some(Self::Bgra8),
            2 => Some(Self::Rgba8),
            3 => Some(Self::Rgb24),
            4 => Some(Self::Nv12),
            _ => None,
        }
    }

    /// Encodes this pixel format into its stable wire `u32` representation.
    #[must_use]
    pub const fn to_raw(self) -> u32 {
        match self {
            Self::Bgra8 => 1,
            Self::Rgba8 => 2,
            Self::Rgb24 => 3,
            Self::Nv12 => 4,
        }
    }

    /// Computes the minimum required payload buffer size in bytes for the given
    /// frame dimensions and row stride, returning `None` if dimensions are zero,
    /// `stride` is narrower than a pixel row, or arithmetic overflows.
    #[must_use]
    pub fn required_buffer_bytes(self, width: u32, height: u32, stride: u32) -> Option<usize> {
        if width == 0 || height == 0 {
            return None;
        }
        let min_row_bytes: u64 = match self {
            Self::Bgra8 | Self::Rgba8 => u64::from(width).checked_mul(4)?,
            Self::Rgb24 => u64::from(width).checked_mul(3)?,
            Self::Nv12 => u64::from(width).next_multiple_of(2),
        };
        if u64::from(stride) < min_row_bytes {
            return None;
        }
        let total_rows: u64 = match self {
            Self::Bgra8 | Self::Rgba8 | Self::Rgb24 => u64::from(height),
            Self::Nv12 => u64::from(height).checked_add(u64::from(height.div_ceil(2)))?,
        };
        let total_bytes = u64::from(stride).checked_mul(total_rows)?;
        usize::try_from(total_bytes).ok()
    }
}

/// Metadata supplied by a producer when publishing a frame into the ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameDescriptor {
    /// Exact integer frame index within the active timeline or bundle.
    pub frame_index: i64,
    /// `true` while displaying an approximate preview (e.g., nearest keyframe during scrub).
    pub is_approximate: bool,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Row stride in bytes.
    pub stride: u32,
    /// Pixel memory layout of the payload bytes.
    pub pixel_format: PixelFormat,
    /// Device boottime timestamp in nanoseconds.
    pub boottime_ns: i64,
}

/// Metadata published atomically alongside each decoded frame in a ring slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameSlotMeta {
    /// Monotonically increasing frame publication generation number (`1, 2, ...`).
    pub generation: u64,
    /// Exact integer frame index within the active timeline or bundle.
    pub frame_index: i64,
    /// `true` while displaying an approximate preview (must show UI badge).
    pub is_approximate: bool,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Row stride in bytes.
    pub stride: u32,
    /// Pixel memory layout of the payload bytes.
    pub pixel_format: PixelFormat,
    /// Device boottime timestamp in nanoseconds.
    pub boottime_ns: i64,
}

#[repr(C, align(64))]
struct RawRingHeader {
    magic: [u8; 8],
    version: u32,
    slot_count: u32,
    slot_capacity: u64,
    published_slot: AtomicU32,
    _pad0: u32,
    published_generation: AtomicU64,
    _reserved: [u8; 24],
}

/// Per-slot frame metadata. Every field is an atomic with the exact size and
/// alignment of the plain integer it replaces, so the wire layout is identical
/// while concurrent writer/reader access stays free of data races.
#[repr(C)]
struct RawSlotFields {
    generation: AtomicU64,
    frame_index: AtomicI64,
    boottime_ns: AtomicI64,
    width: AtomicU32,
    height: AtomicU32,
    stride: AtomicU32,
    pixel_format: AtomicU32,
    payload_len: AtomicU32,
    flags: AtomicU32,
}

/// Plain snapshot of [`RawSlotFields`] taken by a reader inside one seqlock attempt.
#[derive(Clone, Copy)]
struct SlotFieldsSnapshot {
    generation: u64,
    frame_index: i64,
    boottime_ns: i64,
    width: u32,
    height: u32,
    stride: u32,
    pixel_format: u32,
    payload_len: u32,
    flags: u32,
}

impl RawSlotFields {
    fn store(&self, snapshot: &SlotFieldsSnapshot) {
        self.generation
            .store(snapshot.generation, Ordering::Relaxed);
        self.frame_index
            .store(snapshot.frame_index, Ordering::Relaxed);
        self.boottime_ns
            .store(snapshot.boottime_ns, Ordering::Relaxed);
        self.width.store(snapshot.width, Ordering::Relaxed);
        self.height.store(snapshot.height, Ordering::Relaxed);
        self.stride.store(snapshot.stride, Ordering::Relaxed);
        self.pixel_format
            .store(snapshot.pixel_format, Ordering::Relaxed);
        self.payload_len
            .store(snapshot.payload_len, Ordering::Relaxed);
        self.flags.store(snapshot.flags, Ordering::Relaxed);
    }

    fn load(&self) -> SlotFieldsSnapshot {
        SlotFieldsSnapshot {
            generation: self.generation.load(Ordering::Relaxed),
            frame_index: self.frame_index.load(Ordering::Relaxed),
            boottime_ns: self.boottime_ns.load(Ordering::Relaxed),
            width: self.width.load(Ordering::Relaxed),
            height: self.height.load(Ordering::Relaxed),
            stride: self.stride.load(Ordering::Relaxed),
            pixel_format: self.pixel_format.load(Ordering::Relaxed),
            payload_len: self.payload_len.load(Ordering::Relaxed),
            flags: self.flags.load(Ordering::Relaxed),
        }
    }
}

#[repr(C, align(64))]
struct RawSlotHeader {
    seq: AtomicU64,
    fields: RawSlotFields,
    _reserved: [u8; 8],
}

const _: () = {
    assert!(
        size_of::<RawRingHeader>() == RING_HEADER_BYTES,
        "RawRingHeader must be exactly 64 bytes"
    );
    assert!(
        size_of::<RawSlotHeader>() == SLOT_HEADER_BYTES,
        "RawSlotHeader must be exactly 64 bytes"
    );
    assert!(
        offset_of!(RawSlotHeader, fields) == 8,
        "RawSlotFields must sit at 8-byte aligned offset inside RawSlotHeader"
    );
    // Wire layout pinned for the Kotlin `ShmFrameRingReader` (byte offsets
    // relative to the slot header start).
    assert!(
        offset_of!(RawSlotHeader, fields) + offset_of!(RawSlotFields, generation) == 8,
        "generation must stay at slot offset 8"
    );
    assert!(
        offset_of!(RawSlotHeader, fields) + offset_of!(RawSlotFields, frame_index) == 16,
        "frame_index must stay at slot offset 16"
    );
    assert!(
        offset_of!(RawSlotHeader, fields) + offset_of!(RawSlotFields, boottime_ns) == 24,
        "boottime_ns must stay at slot offset 24"
    );
    assert!(
        offset_of!(RawSlotHeader, fields) + offset_of!(RawSlotFields, width) == 32,
        "width must stay at slot offset 32"
    );
    assert!(
        offset_of!(RawSlotHeader, fields) + offset_of!(RawSlotFields, payload_len) == 48,
        "payload_len must stay at slot offset 48"
    );
    assert!(
        offset_of!(RawSlotHeader, fields) + offset_of!(RawSlotFields, flags) == 52,
        "flags must stay at slot offset 52"
    );
    assert!(
        size_of::<AtomicU64>() == 8 && align_of::<AtomicU64>() == 8,
        "AtomicU64 must match the u64 wire word"
    );
    assert!(
        HEADER_REGION_BYTES.is_multiple_of(CACHE_LINE_BYTES),
        "payload region must start 64-byte (and thus AtomicU64) aligned"
    );
};

/// Producer side (`arbd`) of a triple-buffered shared-memory frame ring.
#[derive(Debug)]
pub struct ShmFrameRingWriter {
    path: PathBuf,
    mmap: MmapMut,
    slot_capacity: usize,
    aligned_slot_stride: usize,
    next_slot: u8,
    generation: u64,
}

impl ShmFrameRingWriter {
    /// Creates or re-initializes a triple-buffered shared-memory frame ring at
    /// `path`, with each of the 3 slots holding up to `slot_capacity` bytes.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::InvalidArgument`] if `slot_capacity == 0` or total
    /// ring size overflows, [`OsError::InvalidShmRing`] if the OS mapping is
    /// not 64-byte aligned, or [`OsError::Fs`] if creating, resizing, or
    /// mapping the backing file fails.
    pub fn create(path: impl AsRef<Path>, slot_capacity: usize) -> Result<Self, OsError> {
        if slot_capacity == 0 {
            return Err(OsError::InvalidArgument {
                name: "slot_capacity",
                reason: "slot_capacity must be greater than zero".to_owned(),
            });
        }
        let ShmLayout {
            aligned_slot_stride,
            total_bytes: total_size,
        } = compute_layout(slot_capacity).ok_or_else(|| OsError::InvalidArgument {
            name: "slot_capacity",
            reason: "total shared-memory ring size overflowed usize".to_owned(),
        })?;
        let total_size_u64 = u64::try_from(total_size).map_err(|_| OsError::InvalidArgument {
            name: "slot_capacity",
            reason: "total shared-memory ring size overflowed u64".to_owned(),
        })?;

        let path = path.as_ref().to_path_buf();
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty())
            .ok_or_else(|| OsError::InvalidArgument {
                name: "path",
                reason: format!(
                    "SHM ring path `{}` must have a valid file name",
                    path.display()
                ),
            })?
            .to_owned();
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        if parent != Path::new(".") {
            fs::create_dir_all(&parent).map_err(|source| OsError::Fs {
                operation: "create_shm_parent_dir",
                path: parent.clone(),
                source,
            })?;
        }

        // Never truncate an existing SHM ring file in place: attached readers
        // would fault with SIGBUS on Unix or fail creation with
        // ERROR_USER_MAPPED_FILE on Windows. Instead, create a fresh sibling
        // file with O_EXCL (and atomic 0600 permissions on Unix), initialize
        // its header, and publish it into `path`.
        let seq = TMP_SHM_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp_path = parent.join(format!(".{file_name}.tmp.{}.{seq}", std::process::id()));
        let mut cleanup = TempFileGuard::new(tmp_path.clone());

        let file = create_exclusive_owner_only_file(&tmp_path)?;

        file.set_len(total_size_u64).map_err(|source| OsError::Fs {
            operation: "resize_shm_file",
            path: tmp_path.clone(),
            source,
        })?;

        // SAFETY: `file` is freshly created via O_EXCL and sized to `total_size`;
        // we initialize the ring header before publishing the file at `path`.
        let mut mmap = unsafe { MmapMut::map_mut(&file) }.map_err(|source| OsError::Fs {
            operation: "mmap_shm_writer",
            path: tmp_path.clone(),
            source,
        })?;

        if mmap.as_ptr().align_offset(align_of::<RawRingHeader>()) != 0 {
            return Err(OsError::InvalidShmRing {
                reason: "memory mapping base address is not 64-byte aligned".to_owned(),
            });
        }

        init_ring_headers(&mut mmap, slot_capacity)?;

        publish_shm_file(&tmp_path, &path, &parent, &file_name)?;
        cleanup.disarm();

        Ok(Self {
            path,
            mmap,
            slot_capacity,
            aligned_slot_stride,
            next_slot: 0,
            generation: 0,
        })
    }

    /// Publishes a frame into the next ring slot and atomically exposes it to
    /// readers, returning the assigned monotonic `generation` number.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::InvalidArgument`] if frame dimensions/stride are
    /// invalid, if `pixels.len()` is smaller than required by `descriptor`, or
    /// if `pixels.len()` exceeds the configured `slot_capacity`.
    pub fn publish(&mut self, descriptor: FrameDescriptor, pixels: &[u8]) -> Result<u64, OsError> {
        let required = descriptor
            .pixel_format
            .required_buffer_bytes(descriptor.width, descriptor.height, descriptor.stride)
            .ok_or_else(|| OsError::InvalidArgument {
                name: "descriptor",
                reason: format!(
                    "invalid dimensions {}x{} with stride {} for {:?}",
                    descriptor.width, descriptor.height, descriptor.stride, descriptor.pixel_format
                ),
            })?;

        if pixels.len() < required {
            return Err(OsError::InvalidArgument {
                name: "pixels",
                reason: format!(
                    "pixel buffer too small: got {} bytes, need at least {required} bytes",
                    pixels.len()
                ),
            });
        }
        if pixels.len() > self.slot_capacity {
            return Err(OsError::InvalidArgument {
                name: "pixels",
                reason: format!(
                    "pixel buffer ({} bytes) exceeds ring slot capacity ({} bytes)",
                    pixels.len(),
                    self.slot_capacity
                ),
            });
        }

        let payload_len = u32::try_from(pixels.len()).map_err(|_| OsError::InvalidArgument {
            name: "pixels",
            reason: "pixel buffer length exceeds u32::MAX".to_owned(),
        })?;
        let next_gen = self
            .generation
            .checked_add(1)
            .ok_or_else(|| OsError::InvalidShmRing {
                reason: "frame generation counter overflowed u64".to_owned(),
            })?;

        let slot_idx_u8 = self.next_slot;
        let flags = if descriptor.is_approximate {
            FLAG_IS_APPROXIMATE
        } else {
            0
        };
        let fields = SlotFieldsSnapshot {
            generation: next_gen,
            frame_index: descriptor.frame_index,
            boottime_ns: descriptor.boottime_ns,
            width: descriptor.width,
            height: descriptor.height,
            stride: descriptor.stride,
            pixel_format: descriptor.pixel_format.to_raw(),
            payload_len,
            flags,
        };
        // SAFETY: `self.ring_view()` describes this writer's live, validated
        // mapping, `slot_idx_u8 < SHM_SLOT_COUNT`, and `pixels.len() <= slot_capacity`.
        unsafe { self.ring_view().publish_slot(slot_idx_u8, &fields, pixels) };

        self.generation = next_gen;
        self.next_slot = (slot_idx_u8 + 1) % SHM_SLOT_COUNT_U8;
        Ok(next_gen)
    }

    fn ring_view(&mut self) -> RingView {
        RingView {
            // Derived from `as_mut_ptr` so the pointer carries write provenance.
            base: self.mmap.as_mut_ptr().cast_const(),
            slot_capacity: self.slot_capacity,
            aligned_slot_stride: self.aligned_slot_stride,
        }
    }

    /// Returns the backing file path of this ring.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the maximum payload capacity in bytes per frame slot.
    #[must_use]
    pub const fn slot_capacity(&self) -> usize {
        self.slot_capacity
    }

    /// Returns the most recently published frame generation (`0` if none yet).
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

/// Consumer side (IDE plugin / viewer) of a triple-buffered shared-memory frame ring.
#[derive(Debug)]
pub struct ShmFrameRingReader {
    path: PathBuf,
    mmap: Mmap,
    slot_capacity: usize,
    aligned_slot_stride: usize,
}

impl ShmFrameRingReader {
    /// Opens an existing shared-memory frame ring at `path` for read-only
    /// lock-free consumption.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::Fs`] if opening or mapping the file fails, or
    /// [`OsError::InvalidShmRing`] if the header magic, version, slot count,
    /// alignment, or backing file size is invalid.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, OsError> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path).map_err(|source| OsError::Fs {
            operation: "open_shm_reader",
            path: path.clone(),
            source,
        })?;

        // SAFETY: The backing file is mapped read-only and all reads from
        // concurrently-written slots are guarded by atomic Acquire/Release
        // sequence locks plus volatile reads.
        let mmap = unsafe { Mmap::map(&file) }.map_err(|source| OsError::Fs {
            operation: "mmap_shm_reader",
            path: path.clone(),
            source,
        })?;

        if mmap.len() < HEADER_REGION_BYTES {
            return Err(OsError::InvalidShmRing {
                reason: format!(
                    "mapping too small for ring header: {} < {HEADER_REGION_BYTES}",
                    mmap.len()
                ),
            });
        }
        if mmap.as_ptr().align_offset(align_of::<RawRingHeader>()) != 0 {
            return Err(OsError::InvalidShmRing {
                reason: "reader mapping base address is not 64-byte aligned".to_owned(),
            });
        }

        let ring_hdr_ptr = ring_header_ptr_const(mmap.as_ptr());
        // SAFETY: `mmap.len() >= HEADER_REGION_BYTES` and base pointer is 64-byte aligned.
        let hdr = unsafe { &*ring_hdr_ptr };
        if hdr.magic != RING_MAGIC {
            return Err(OsError::InvalidShmRing {
                reason: "unexpected shared-memory ring magic bytes".to_owned(),
            });
        }
        if hdr.version != RING_VERSION {
            return Err(OsError::InvalidShmRing {
                reason: format!("unsupported ring version {}", hdr.version),
            });
        }
        if hdr.slot_count != SHM_SLOT_COUNT_U32 {
            return Err(OsError::InvalidShmRing {
                reason: format!("unexpected slot count {}", hdr.slot_count),
            });
        }

        let slot_capacity =
            usize::try_from(hdr.slot_capacity).map_err(|_| OsError::InvalidShmRing {
                reason: "ring slot_capacity exceeds usize".to_owned(),
            })?;
        if slot_capacity == 0 {
            return Err(OsError::InvalidShmRing {
                reason: "ring slot_capacity must be positive".to_owned(),
            });
        }

        let ShmLayout {
            aligned_slot_stride,
            total_bytes: required_len,
        } = compute_layout(slot_capacity).ok_or_else(|| OsError::InvalidShmRing {
            reason: "expected ring byte size overflowed usize".to_owned(),
        })?;
        if mmap.len() < required_len {
            return Err(OsError::InvalidShmRing {
                reason: format!(
                    "backing file truncated: {} bytes < expected {required_len} bytes",
                    mmap.len()
                ),
            });
        }

        Ok(Self {
            path,
            mmap,
            slot_capacity,
            aligned_slot_stride,
        })
    }

    /// Reads the latest published generation number without copying pixel data.
    ///
    /// Returns `0` before the producer has published its first frame.
    #[must_use]
    pub fn latest_generation(&self) -> u64 {
        let ring_hdr_ptr = ring_header_ptr_const(self.mmap.as_ptr());
        // SAFETY: `self.mmap` was validated in `open` to hold a 64-byte aligned
        // `RawRingHeader`.
        let hdr = unsafe { &*ring_hdr_ptr };
        hdr.published_generation.load(Ordering::Acquire)
    }

    /// Atomically reads the latest published frame and copies its pixel bytes
    /// into `out_pixels`, reusing the caller's allocation.
    ///
    /// Returns `Ok(None)` if no frame has been published yet.
    ///
    /// # Errors
    ///
    /// Returns [`OsError::InvalidShmRing`] if ring metadata is corrupted or the
    /// slot seqlock fails to stabilize after repeated concurrent overwrites.
    pub fn read_latest(&self, out_pixels: &mut Vec<u8>) -> Result<Option<FrameSlotMeta>, OsError> {
        self.read_latest_with_mid_copy_hook(out_pixels, || {})
    }

    /// [`Self::read_latest`] with `mid_copy` invoked halfway through every
    /// payload copy attempt. Production passes a no-op (monomorphized away);
    /// tests use it to deterministically wrap the writer around the whole ring
    /// while a copy is in flight.
    fn read_latest_with_mid_copy_hook(
        &self,
        out_pixels: &mut Vec<u8>,
        mid_copy: impl FnMut(),
    ) -> Result<Option<FrameSlotMeta>, OsError> {
        // SAFETY: `self.ring_view()` describes this reader's live mapping, whose
        // header, slot count, and size were validated in `open`.
        unsafe { self.ring_view().read_latest(out_pixels, mid_copy) }
    }

    fn ring_view(&self) -> RingView {
        RingView {
            base: self.mmap.as_ptr(),
            slot_capacity: self.slot_capacity,
            aligned_slot_stride: self.aligned_slot_stride,
        }
    }

    /// Returns the backing file path of this ring.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the maximum payload capacity in bytes per frame slot.
    #[must_use]
    pub const fn slot_capacity(&self) -> usize {
        self.slot_capacity
    }
}

/// Raw geometry of one mapped ring (writer mapping, reader mapping, or a test
/// allocation): the seqlock publish/read algorithms operate on this view so the
/// exact same code can be race-checked over a single shared allocation.
#[derive(Debug, Clone, Copy)]
struct RingView {
    base: *const u8,
    slot_capacity: usize,
    aligned_slot_stride: usize,
}

impl RingView {
    /// Publishes `pixels` + `fields` into slot `slot_idx_u8` under its seqlock
    /// and exposes it through the ring header.
    ///
    /// # Safety
    ///
    /// `self.base` must be a 64-byte aligned, initialized ring of at least
    /// `HEADER_REGION_BYTES + SHM_SLOT_COUNT * aligned_slot_stride` bytes that is
    /// only accessed atomically by other parties; the caller must be the single
    /// writer, `slot_idx_u8 < SHM_SLOT_COUNT`, and `pixels.len() <= slot_capacity`.
    unsafe fn publish_slot(&self, slot_idx_u8: u8, fields: &SlotFieldsSnapshot, pixels: &[u8]) {
        let next_gen = fields.generation;
        let slot_idx = usize::from(slot_idx_u8);
        let slot_idx_u32 = u32::from(slot_idx_u8);
        let base_ptr = self.base;
        let slot_base = slot_base_ptr_const(base_ptr, slot_idx);
        let seq_ptr = slot_seq_ptr(slot_base);
        let fields_ptr = slot_fields_ptr(slot_base);
        let payload_ptr = slot_payload_ptr(base_ptr, slot_idx, self.aligned_slot_stride);

        // SAFETY: `seq_ptr` points to the 64-byte aligned `AtomicU64` sequence
        // counter at offset 0 of slot `slot_idx` inside the live mapping.
        let seq_atomic = unsafe { &*seq_ptr };
        let seq_before = seq_atomic.load(Ordering::Relaxed);
        // Mark slot as in-progress (odd sequence number) before touching payload/meta.
        seq_atomic.store(seq_before.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);

        // SAFETY: `payload_ptr` is the 64-byte aligned start of slot `slot_idx`'s
        // payload region holding `slot_capacity >= pixels.len()` bytes inside
        // the live mapping; concurrent readers only touch it atomically.
        let payload = unsafe { AtomicPayload::new(payload_ptr, pixels.len()) };
        payload.store_from(pixels);

        // SAFETY: `fields_ptr` is the 8-byte aligned `RawSlotFields` of slot
        // `slot_idx` inside the live mapping; all accesses to it are atomic.
        unsafe { &*fields_ptr }.store(fields);

        // Publish completed slot (even sequence number) with Release semantics.
        seq_atomic.store(seq_before.wrapping_add(2), Ordering::Release);

        let ring_hdr_ptr = ring_header_ptr_const(base_ptr);
        // SAFETY: `ring_hdr_ptr` is 64-byte aligned and initialized in `create`.
        let ring_hdr = unsafe { &*ring_hdr_ptr };
        ring_hdr
            .published_slot
            .store(slot_idx_u32, Ordering::Release);
        ring_hdr
            .published_generation
            .store(next_gen, Ordering::Release);
    }

    /// Seqlock-reads the latest published slot into `out_pixels`.
    ///
    /// # Safety
    ///
    /// `self.base` must be a 64-byte aligned ring whose header was validated
    /// (magic/version/slot count) and that spans at least
    /// `HEADER_REGION_BYTES + SHM_SLOT_COUNT * aligned_slot_stride` bytes, only
    /// ever mutated atomically by its writer.
    unsafe fn read_latest(
        &self,
        out_pixels: &mut Vec<u8>,
        mut mid_copy: impl FnMut(),
    ) -> Result<Option<FrameSlotMeta>, OsError> {
        let base_ptr = self.base;
        let ring_hdr_ptr = ring_header_ptr_const(base_ptr);
        // SAFETY: `self.mmap` was validated in `open` to hold a 64-byte aligned
        // `RawRingHeader`.
        let ring_hdr = unsafe { &*ring_hdr_ptr };

        for _ in 0..MAX_SEQLOCK_RETRIES {
            let published_gen = ring_hdr.published_generation.load(Ordering::Acquire);
            if published_gen == 0 {
                return Ok(None);
            }
            let published_slot = ring_hdr.published_slot.load(Ordering::Acquire);
            if published_slot == NO_PUBLISHED_SLOT {
                return Ok(None);
            }
            let slot_idx =
                usize::try_from(published_slot).map_err(|_| OsError::InvalidShmRing {
                    reason: format!("published_slot {published_slot} overflowed usize"),
                })?;
            if slot_idx >= SHM_SLOT_COUNT {
                return Err(OsError::InvalidShmRing {
                    reason: format!("published_slot {slot_idx} out of range 0..{SHM_SLOT_COUNT}"),
                });
            }

            let slot_base = slot_base_ptr_const(base_ptr, slot_idx);
            let seq_ptr = slot_seq_ptr(slot_base);
            // SAFETY: `slot_idx < SHM_SLOT_COUNT` guarantees `seq_ptr` is within
            // the validated header region and 64-byte aligned.
            let seq_atomic = unsafe { &*seq_ptr };
            let seq1 = seq_atomic.load(Ordering::Acquire);
            if seq1 % 2 != 0 {
                std::hint::spin_loop();
                continue;
            }

            let fields_ptr = slot_fields_ptr(slot_base);
            // SAFETY: `fields_ptr` is the 8-byte aligned `RawSlotFields` within the
            // validated slot header; every field is loaded atomically and the
            // snapshot's consistency is checked against `seq2` below.
            let raw = unsafe { &*fields_ptr }.load();
            let payload_len = usize::try_from(raw.payload_len).unwrap_or(usize::MAX);
            if payload_len > self.slot_capacity {
                fence(Ordering::Acquire);
                if seq_atomic.load(Ordering::Relaxed) != seq1 {
                    std::hint::spin_loop();
                    continue;
                }
                return Err(OsError::InvalidShmRing {
                    reason: format!(
                        "slot payload_len ({payload_len}) exceeds slot_capacity ({})",
                        self.slot_capacity
                    ),
                });
            }

            out_pixels.resize(payload_len, 0);
            let payload_ptr = slot_payload_ptr(base_ptr, slot_idx, self.aligned_slot_stride);
            // SAFETY: `payload_ptr` is the 64-byte aligned payload start of a
            // validated slot and `payload_len <= self.slot_capacity`, so the
            // whole range lies inside the mapping; it is only read atomically.
            let payload = unsafe { AtomicPayload::new(payload_ptr, payload_len) };
            payload.load_into(out_pixels, &mut mid_copy);

            fence(Ordering::Acquire);
            let seq2 = seq_atomic.load(Ordering::Relaxed);
            if seq1 != seq2 {
                std::hint::spin_loop();
                continue;
            }

            let pixel_format =
                PixelFormat::from_raw(raw.pixel_format).ok_or_else(|| OsError::InvalidShmRing {
                    reason: format!("unknown pixel_format discriminant {}", raw.pixel_format),
                })?;
            let required = pixel_format
                .required_buffer_bytes(raw.width, raw.height, raw.stride)
                .ok_or_else(|| OsError::InvalidShmRing {
                    reason: format!(
                        "corrupt frame dimensions {}x{} stride {}",
                        raw.width, raw.height, raw.stride
                    ),
                })?;
            if payload_len < required {
                return Err(OsError::InvalidShmRing {
                    reason: format!(
                        "slot payload ({payload_len}) smaller than required ({required})"
                    ),
                });
            }

            return Ok(Some(FrameSlotMeta {
                generation: raw.generation,
                frame_index: raw.frame_index,
                is_approximate: (raw.flags & FLAG_IS_APPROXIMATE) != 0,
                width: raw.width,
                height: raw.height,
                stride: raw.stride,
                pixel_format,
                boottime_ns: raw.boottime_ns,
            }));
        }

        Err(OsError::InvalidShmRing {
            reason: SEQLOCK_CONTENTION.to_owned(),
        })
    }
}

fn init_ring_headers(mmap: &mut [u8], slot_capacity: usize) -> Result<(), OsError> {
    let slot_cap_u64 = u64::try_from(slot_capacity).map_err(|_| OsError::InvalidArgument {
        name: "slot_capacity",
        reason: "slot_capacity overflowed u64".to_owned(),
    })?;

    mmap[..HEADER_REGION_BYTES].fill(0);

    let ring_hdr_ptr = ring_header_ptr_mut(mmap.as_mut_ptr());
    let initial = RawRingHeader {
        magic: RING_MAGIC,
        version: RING_VERSION,
        slot_count: SHM_SLOT_COUNT_U32,
        slot_capacity: slot_cap_u64,
        published_slot: AtomicU32::new(NO_PUBLISHED_SLOT),
        _pad0: 0,
        published_generation: AtomicU64::new(0),
        _reserved: [0; 24],
    };
    // SAFETY: `ring_hdr_ptr` is 64-byte aligned, writable, and within `mmap`.
    unsafe {
        ptr::write(ring_hdr_ptr, initial);
    }
    Ok(())
}

#[expect(
    clippy::cast_ptr_alignment,
    reason = "mmap base pointer is verified 64-byte aligned in create() and open()"
)]
const fn ring_header_ptr_const(base: *const u8) -> *const RawRingHeader {
    base.cast::<RawRingHeader>()
}

#[expect(
    clippy::cast_ptr_alignment,
    reason = "mmap base pointer is verified 64-byte aligned in create()"
)]
const fn ring_header_ptr_mut(base: *mut u8) -> *mut RawRingHeader {
    base.cast::<RawRingHeader>()
}

const fn slot_base_ptr_const(base: *const u8, slot_idx: usize) -> *const u8 {
    let offset = RING_HEADER_BYTES + slot_idx * SLOT_HEADER_BYTES;
    base.wrapping_add(offset)
}

#[expect(
    clippy::cast_ptr_alignment,
    reason = "each slot header starts at a 64-byte aligned offset from the verified mmap base"
)]
const fn slot_seq_ptr(slot_base: *const u8) -> *const AtomicU64 {
    slot_base.cast::<AtomicU64>()
}

#[expect(
    clippy::cast_ptr_alignment,
    reason = "RawSlotFields sits at offset 8 inside each 64-byte aligned slot header"
)]
const fn slot_fields_ptr(slot_base: *const u8) -> *const RawSlotFields {
    slot_base
        .wrapping_add(offset_of!(RawSlotHeader, fields))
        .cast::<RawSlotFields>()
}

const fn slot_payload_ptr(base: *const u8, slot_idx: usize, aligned_stride: usize) -> *const u8 {
    let offset = HEADER_REGION_BYTES + slot_idx * aligned_stride;
    base.wrapping_add(offset)
}

/// Every shared access covers an aligned full word, even when successive frames
/// in the same slot have different lengths. The cache-line stride provides padding.
struct AtomicPayload<'a> {
    words: &'a [AtomicU64],
}

impl AtomicPayload<'_> {
    /// # Safety
    ///
    /// `ptr` must be aligned and valid for `len` rounded up to a whole word
    /// for the view's lifetime. Shared payload accesses must use aligned words.
    #[expect(
        clippy::cast_ptr_alignment,
        reason = "slot payload starts at a cache-line boundary"
    )]
    unsafe fn new(ptr: *const u8, len: usize) -> Self {
        // SAFETY: the final word fits within the caller-guaranteed padded slot.
        let words = unsafe { std::slice::from_raw_parts(ptr.cast::<AtomicU64>(), len.div_ceil(8)) };
        Self { words }
    }

    fn store_from(&self, src: &[u8]) {
        let (chunks, tail) = src.as_chunks::<8>();
        for (word, chunk) in self.words.iter().zip(chunks) {
            word.store(u64::from_ne_bytes(*chunk), Ordering::Relaxed);
        }
        if let Some(last) = self.words.get(chunks.len()) {
            let mut padded = [0; 8];
            padded[..tail.len()].copy_from_slice(tail);
            last.store(u64::from_ne_bytes(padded), Ordering::Relaxed);
        }
    }

    fn load_into(&self, dst: &mut [u8], mid_copy: &mut impl FnMut()) {
        let (chunks, tail) = dst.as_chunks_mut::<8>();
        let count = chunks.len();
        for (index, (word, chunk)) in self.words.iter().zip(chunks).enumerate() {
            if index == count / 2 {
                mid_copy();
            }
            *chunk = word.load(Ordering::Relaxed).to_ne_bytes();
        }
        if count == 0 {
            mid_copy();
        }
        if let Some(last) = self.words.get(count) {
            tail.copy_from_slice(&last.load(Ordering::Relaxed).to_ne_bytes()[..tail.len()]);
        }
    }
}

/// Byte layout of a shared-memory frame ring: header region followed by
/// [`SHM_SLOT_COUNT`] cache-line aligned payload slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ShmLayout {
    aligned_slot_stride: usize,
    total_bytes: usize,
}

/// Computes the ring layout for `slot_capacity` payload bytes per slot, or
/// `None` when the capacity is zero or any size overflows `usize`.
const fn compute_layout(slot_capacity: usize) -> Option<ShmLayout> {
    if slot_capacity == 0 {
        return None;
    }
    let Some(aligned_slot_stride) = align_up(slot_capacity, CACHE_LINE_BYTES) else {
        return None;
    };
    let Some(slots) = aligned_slot_stride.checked_mul(SHM_SLOT_COUNT) else {
        return None;
    };
    let Some(total_bytes) = slots.checked_add(HEADER_REGION_BYTES) else {
        return None;
    };
    Some(ShmLayout {
        aligned_slot_stride,
        total_bytes,
    })
}

const fn align_up(value: usize, align: usize) -> Option<usize> {
    let rem = value % align;
    if rem == 0 {
        Some(value)
    } else {
        value.checked_add(align - rem)
    }
}

struct TempFileGuard {
    path: PathBuf,
    armed: bool,
}

impl TempFileGuard {
    const fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    const fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(unix)]
fn create_exclusive_owner_only_file(path: &Path) -> Result<File, OsError> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| OsError::Fs {
            operation: "create_shm_file",
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(not(unix))]
fn create_exclusive_owner_only_file(path: &Path) -> Result<File, OsError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| OsError::Fs {
            operation: "create_shm_file",
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(unix)]
fn publish_shm_file(
    tmp_path: &Path,
    final_path: &Path,
    _parent: &Path,
    _file_name: &str,
) -> Result<(), OsError> {
    // POSIX rename atomically swaps the directory entry; existing readers keep
    // their mapped old inode intact without ever encountering SIGBUS.
    crate::fs::replace_file(tmp_path, final_path).map_err(|source| OsError::Fs {
        operation: "publish_shm_file",
        path: final_path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn publish_shm_file(
    tmp_path: &Path,
    final_path: &Path,
    parent: &Path,
    file_name: &str,
) -> Result<(), OsError> {
    // Best-effort sweep of previously retired mappings whose readers have closed.
    let retired_prefix = format!(".{file_name}.retired.");
    if let Ok(entries) = fs::read_dir(parent) {
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(&retired_prefix))
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    if fs::symlink_metadata(final_path).is_ok() {
        // On Windows, replacing a file with an active memory-mapped reader fails
        // with ERROR_USER_MAPPED_FILE, whereas moving it aside succeeds when
        // opened with FILE_SHARE_DELETE (Rust std::fs::File default), leaving
        // the reader's view valid until it drops.
        let seq = TMP_SHM_SEQ.fetch_add(1, Ordering::Relaxed);
        let retired = parent.join(format!(".{file_name}.retired.{}.{seq}", std::process::id()));
        crate::fs::replace_file(final_path, &retired).map_err(|source| OsError::Fs {
            operation: "retire_existing_shm_file",
            path: final_path.to_path_buf(),
            source,
        })?;
        // Best-effort removal: succeeds immediately if no reader still maps it.
        let _ = fs::remove_file(&retired);
    }

    crate::fs::replace_file(tmp_path, final_path).map_err(|source| OsError::Fs {
        operation: "publish_shm_file",
        path: final_path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::thread;

    use super::{FrameDescriptor, PixelFormat, ShmFrameRingReader, ShmFrameRingWriter};
    use crate::error::OsError;

    static SHM_SEQ: AtomicU64 = AtomicU64::new(1);

    struct TestShmDir {
        path: PathBuf,
    }

    impl TestShmDir {
        fn new(label: &str) -> Self {
            let seq = SHM_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "arb-os-shm-test-{label}-{}-{seq}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create shm test dir");
            Self { path }
        }
    }

    impl Drop for TestShmDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[cfg_attr(
        miri,
        ignore = "filesystem/mmap I/O is unsupported under Miri isolation"
    )]
    #[test]
    fn round_trip_1080p_bgra_triple_buffer_and_approximate_transitions() {
        const WIDTH: u32 = 1920;
        const HEIGHT: u32 = 1080;
        const STRIDE: u32 = WIDTH * 4;
        const FRAME_BYTES: usize = 1920 * 1080 * 4; // 8,294,400 bytes (~8.3 MB/slot)

        let tmp = TestShmDir::new("1080p");
        let shm_path = tmp.path.join("viewport-main.shm");

        let mut writer =
            ShmFrameRingWriter::create(&shm_path, FRAME_BYTES).expect("create 1080p SHM ring");
        let reader = ShmFrameRingReader::open(&shm_path).expect("open 1080p SHM ring reader");

        assert_eq!(
            writer.slot_capacity(),
            FRAME_BYTES,
            "writer slot capacity must match requested 1080p size"
        );
        assert_eq!(
            reader.slot_capacity(),
            FRAME_BYTES,
            "reader slot capacity must match requested 1080p size"
        );
        assert_eq!(
            reader.latest_generation(),
            0,
            "empty ring must report generation 0"
        );

        let mut read_buf = Vec::new();
        assert_eq!(
            reader
                .read_latest(&mut read_buf)
                .expect("read from empty ring should succeed"),
            None,
            "empty ring must return None before first publish"
        );

        // Publish 5 frames (> 3 slots) to verify wrap-around across all 3 slots
        // and verify `is_approximate` transitions (true -> false -> true -> false -> false).
        let sequence = [
            (100_i64, true, 0x11_u8, 1_000_000_000_i64),
            (101_i64, false, 0x22_u8, 1_033_333_333_i64),
            (150_i64, true, 0x33_u8, 2_666_666_666_i64),
            (150_i64, false, 0x44_u8, 2_666_666_666_i64),
            (151_i64, false, 0x55_u8, 2_700_000_000_i64),
        ];

        let mut pixel_buf = vec![0_u8; FRAME_BYTES];
        for (idx, &(frame_index, is_approximate, tag, boottime_ns)) in sequence.iter().enumerate() {
            // Paint deterministic header/middle/tail pattern so any tearing or
            // offset bug is caught immediately.
            pixel_buf.fill(tag);
            pixel_buf[0] = tag.wrapping_add(1);
            pixel_buf[FRAME_BYTES / 2] = tag.wrapping_add(2);
            pixel_buf[FRAME_BYTES - 1] = tag.wrapping_add(3);

            let expected_gen = u64::try_from(idx + 1).expect("index fits u64");
            let published_gen = writer
                .publish(
                    FrameDescriptor {
                        frame_index,
                        is_approximate,
                        width: WIDTH,
                        height: HEIGHT,
                        stride: STRIDE,
                        pixel_format: PixelFormat::Bgra8,
                        boottime_ns,
                    },
                    &pixel_buf,
                )
                .expect("publish 1080p frame must succeed");
            assert_eq!(
                published_gen, expected_gen,
                "generation must increment monotonically"
            );
            assert_eq!(
                reader.latest_generation(),
                expected_gen,
                "reader must immediately observe published generation"
            );

            let meta = reader
                .read_latest(&mut read_buf)
                .expect("read_latest must succeed")
                .expect("frame must be present after publish");
            assert_eq!(meta.generation, expected_gen, "meta generation mismatch");
            assert_eq!(meta.frame_index, frame_index, "meta frame_index mismatch");
            assert_eq!(
                meta.is_approximate, is_approximate,
                "meta is_approximate badge state mismatch"
            );
            assert_eq!(meta.width, WIDTH, "meta width mismatch");
            assert_eq!(meta.height, HEIGHT, "meta height mismatch");
            assert_eq!(meta.stride, STRIDE, "meta stride mismatch");
            assert_eq!(
                meta.pixel_format,
                PixelFormat::Bgra8,
                "meta pixel_format mismatch"
            );
            assert_eq!(meta.boottime_ns, boottime_ns, "meta boottime_ns mismatch");
            assert_eq!(
                read_buf.len(),
                FRAME_BYTES,
                "read pixel buffer must match full 1080p BGRA byte length"
            );
            assert_eq!(
                read_buf, pixel_buf,
                "read 1080p pixel bytes must match published frame exactly"
            );
        }
    }

    #[cfg_attr(
        miri,
        ignore = "filesystem/mmap I/O is unsupported under Miri isolation"
    )]
    #[test]
    fn concurrent_writer_and_reader_never_observe_torn_frames() {
        const WIDTH: u32 = 320;
        const HEIGHT: u32 = 180;
        const STRIDE: u32 = WIDTH * 4;
        const FRAME_BYTES: usize = 320 * 180 * 4;

        let tmp = TestShmDir::new("concurrent");
        let shm_path = tmp.path.join("concurrent.shm");

        let mut writer =
            ShmFrameRingWriter::create(&shm_path, FRAME_BYTES).expect("create concurrent ring");
        let reader = ShmFrameRingReader::open(&shm_path).expect("open concurrent reader");
        // Checkpoint every 20 publications so even a preempted reader validates
        // ten distinct generations. Reads between checkpoints still race the writer.
        let (validated, checkpoint) = std::sync::mpsc::channel();
        let reader_thread = thread::spawn(move || {
            let mut buf = Vec::with_capacity(FRAME_BYTES);
            let mut last_gen = 0_u64;
            while last_gen < 200 {
                match reader.read_latest(&mut buf) {
                    Ok(Some(meta)) => {
                        assert!(
                            meta.generation >= last_gen,
                            "generation moved backwards: {} < {}",
                            meta.generation,
                            last_gen
                        );
                        let expected_byte =
                            u8::try_from(meta.frame_index.rem_euclid(251)).expect("byte in 0..251");
                        assert!(
                            buf.iter().all(|&b| b == expected_byte),
                            "detected torn frame for frame_index={} generation={}",
                            meta.frame_index,
                            meta.generation
                        );
                        if meta.generation > last_gen && meta.generation % 20 == 0 {
                            validated.send(meta.generation).expect("writer active");
                        }
                        last_gen = meta.generation;
                    }
                    Ok(None) => thread::yield_now(),
                    Err(OsError::InvalidShmRing { reason })
                        if reason == super::SEQLOCK_CONTENTION =>
                    {
                        thread::yield_now();
                    }
                    Err(error) => panic!("unexpected read error: {error}"),
                }
            }
        });

        let mut frame = vec![0_u8; FRAME_BYTES];
        for i in 1_i64..=200 {
            let fill = u8::try_from(i.rem_euclid(251)).expect("byte in 0..251");
            frame.fill(fill);
            writer
                .publish(
                    FrameDescriptor {
                        frame_index: i,
                        is_approximate: i % 2 == 1,
                        width: WIDTH,
                        height: HEIGHT,
                        stride: STRIDE,
                        pixel_format: PixelFormat::Rgba8,
                        boottime_ns: i * 33_333_333,
                    },
                    &frame,
                )
                .expect("publish concurrent frame");
            if i % 20 == 0 {
                assert_eq!(
                    checkpoint
                        .recv_timeout(std::time::Duration::from_secs(30))
                        .expect("reader must validate the published generation"),
                    i.cast_unsigned()
                );
            }
        }
        reader_thread.join().expect("reader thread must not panic");
    }

    fn solid_frame(frame_index: i64, width: u32, height: u32) -> FrameDescriptor {
        FrameDescriptor {
            frame_index,
            is_approximate: false,
            width,
            height,
            stride: width * 4,
            pixel_format: PixelFormat::Rgba8,
            boottime_ns: frame_index * 1_000,
        }
    }

    /// H6: the writer laps the *entire* 3-slot ring while the reader is halfway
    /// through copying a slot. The interrupted attempt must be discarded and the
    /// retry must return the newest frame intact (never a torn mix).
    #[cfg_attr(
        miri,
        ignore = "filesystem/mmap I/O is unsupported under Miri isolation"
    )]
    #[test]
    fn writer_wrapping_whole_ring_mid_copy_never_returns_torn_frame() {
        const WIDTH: u32 = 64;
        const HEIGHT: u32 = 48;
        // Odd tail (not a multiple of 8) exercises the partial-word path too.
        const FRAME_BYTES: usize = 64 * 48 * 4 + 5;

        let tmp = TestShmDir::new("wrap-mid-copy");
        let shm_path = tmp.path.join("wrap.shm");
        let mut writer = ShmFrameRingWriter::create(&shm_path, FRAME_BYTES).expect("create ring");
        let reader = ShmFrameRingReader::open(&shm_path).expect("open reader");

        writer
            .publish(solid_frame(1, WIDTH, HEIGHT), &[0x01; FRAME_BYTES])
            .expect("publish frame 1");

        let mut hook_calls = 0_u32;
        let mut buf = Vec::new();
        let meta = reader
            .read_latest_with_mid_copy_hook(&mut buf, || {
                hook_calls += 1;
                if hook_calls == 1 {
                    // Lap the full ring: slot 0 (being copied) is overwritten by frame 4.
                    for (frame_index, fill) in [(2_i64, 0x02_u8), (3, 0x03), (4, 0x04)] {
                        writer
                            .publish(
                                solid_frame(frame_index, WIDTH, HEIGHT),
                                &[fill; FRAME_BYTES],
                            )
                            .expect("publish lapping frame");
                    }
                }
            })
            .expect("read_latest must recover after a mid-copy wrap")
            .expect("a frame is published");

        assert_eq!(
            hook_calls, 2,
            "the interrupted attempt must be detected and retried exactly once"
        );
        assert_eq!(meta.frame_index, 4, "retry must return the newest frame");
        assert_eq!(
            meta.generation, 4,
            "retry must report the newest generation"
        );
        assert_eq!(buf.len(), FRAME_BYTES, "payload length must match");
        assert!(
            buf.iter().all(|&b| b == 0x04),
            "returned payload must be the untorn newest frame"
        );
    }

    /// H6: when the writer keeps lapping the reader on every attempt, the reader
    /// reports stale/contended data as an error instead of a torn frame.
    #[cfg_attr(
        miri,
        ignore = "filesystem/mmap I/O is unsupported under Miri isolation"
    )]
    #[test]
    fn writer_lapping_every_attempt_yields_contention_error_not_torn_frame() {
        const FRAME_BYTES: usize = 16 * 16 * 4;

        let tmp = TestShmDir::new("wrap-every-attempt");
        let shm_path = tmp.path.join("wrap-every.shm");
        let mut writer = ShmFrameRingWriter::create(&shm_path, FRAME_BYTES).expect("create ring");
        let reader = ShmFrameRingReader::open(&shm_path).expect("open reader");
        writer
            .publish(solid_frame(0, 16, 16), &[0; FRAME_BYTES])
            .expect("publish frame 0");

        let mut next_index = 1_i64;
        let mut buf = Vec::new();
        let result = reader.read_latest_with_mid_copy_hook(&mut buf, || {
            for _ in 0..super::SHM_SLOT_COUNT {
                let fill = u8::try_from(next_index.rem_euclid(251)).expect("byte");
                writer
                    .publish(solid_frame(next_index, 16, 16), &[fill; FRAME_BYTES])
                    .expect("publish lapping frame");
                next_index += 1;
            }
        });
        assert!(
            matches!(&result, Err(OsError::InvalidShmRing { reason }) if reason == super::SEQLOCK_CONTENTION),
            "perpetual lapping must surface a contention error, got {result:?}"
        );
    }

    /// 64-byte aligned heap allocation standing in for one shared mapping.
    struct SharedRingAlloc {
        ptr: *mut u8,
        layout: std::alloc::Layout,
        view: super::RingView,
    }

    // SAFETY: the allocation is only accessed through the ring's atomic
    // seqlock protocol; the raw pointer is shared deliberately.
    unsafe impl Send for SharedRingAlloc {}
    // SAFETY: see `Send` above — all concurrent access is atomic.
    unsafe impl Sync for SharedRingAlloc {}

    impl SharedRingAlloc {
        fn new(slot_capacity: usize) -> Self {
            let stride = super::align_up(slot_capacity, super::CACHE_LINE_BYTES).expect("stride");
            let total = super::HEADER_REGION_BYTES + super::SHM_SLOT_COUNT * stride;
            let layout = std::alloc::Layout::from_size_align(total, 64).expect("layout");
            // SAFETY: `layout` has non-zero size.
            let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
            assert!(!ptr.is_null(), "allocation must succeed");
            // SAFETY: `ptr` is valid for `total` zeroed bytes owned exclusively here.
            let bytes = unsafe { std::slice::from_raw_parts_mut(ptr, total) };
            super::init_ring_headers(bytes, slot_capacity).expect("init headers");
            let view = super::RingView {
                base: ptr.cast_const(),
                slot_capacity,
                aligned_slot_stride: stride,
            };
            Self { ptr, layout, view }
        }
    }

    impl Drop for SharedRingAlloc {
        fn drop(&mut self) {
            // SAFETY: allocated in `new` with exactly `self.layout`.
            unsafe { std::alloc::dealloc(self.ptr, self.layout) };
        }
    }

    /// H6: writer and reader run concurrently over the *same* allocation (same
    /// virtual addresses, unlike two file mappings), so data-race detectors such
    /// as `ThreadSanitizer` (`-Zsanitizer=thread`) observe every overlapping
    /// access. With atomic metadata/payload access this is race-free and the
    /// reader never returns a torn frame.
    #[test]
    fn same_allocation_concurrent_publish_and_read_is_race_free_and_untorn() {
        const FRAME_BYTES: usize = 32 * 8 * 4 + 3;
        let alloc = Arc::new(SharedRingAlloc::new(FRAME_BYTES + 5));
        let view = alloc.view;
        let done = Arc::new(AtomicBool::new(false));
        // The writer only starts once the reader is spinning, so their accesses
        // genuinely overlap instead of being ordered by thread start-up.
        let reader_live = Arc::new(AtomicBool::new(false));

        let reader_alloc = Arc::clone(&alloc);
        let reader_done = Arc::clone(&done);
        let reader_started = Arc::clone(&reader_live);
        let reader = thread::spawn(move || {
            let view = reader_alloc.view;
            let mut buf = Vec::new();
            let mut observed = 0_u64;
            reader_started.store(true, Ordering::Relaxed);
            while !reader_done.load(Ordering::Relaxed) || observed == 0 {
                // SAFETY: `view` describes the initialized allocation kept alive
                // by `reader_alloc`; the writer only mutates it atomically.
                match unsafe { view.read_latest(&mut buf, || {}) } {
                    Ok(Some(meta)) => {
                        let expected =
                            u8::try_from(meta.frame_index.rem_euclid(251)).expect("byte");
                        assert!(
                            buf.iter().all(|&b| b == expected),
                            "torn frame for frame_index={}",
                            meta.frame_index
                        );
                        observed += 1;
                    }
                    Ok(None) => {}
                    Err(OsError::InvalidShmRing { reason })
                        if reason == super::SEQLOCK_CONTENTION => {}
                    Err(other) => panic!("unexpected read error: {other:?}"),
                }
            }
            observed
        });

        while !reader_live.load(Ordering::Relaxed) {
            std::hint::spin_loop();
        }
        let mut frame = vec![0_u8; FRAME_BYTES + 5];
        for i in 1_i64..=2_000 {
            let len = if i % 2 == 0 {
                FRAME_BYTES + 5
            } else {
                FRAME_BYTES
            };
            frame.fill(u8::try_from(i.rem_euclid(251)).expect("byte"));
            let slot = u8::try_from((i - 1).rem_euclid(3)).expect("slot");
            let fields = super::SlotFieldsSnapshot {
                generation: u64::try_from(i).expect("gen"),
                frame_index: i,
                boottime_ns: i,
                width: 32,
                height: 8,
                stride: 128,
                pixel_format: PixelFormat::Rgba8.to_raw(),
                payload_len: u32::try_from(len).expect("len"),
                flags: 0,
            };
            // SAFETY: single writer over the initialized allocation, slot < 3,
            // and the payload fits the slot capacity.
            unsafe { view.publish_slot(slot, &fields, &frame[..len]) };
        }
        done.store(true, Ordering::Relaxed);
        let observed = reader.join().expect("reader thread must not panic");
        assert!(observed > 0, "reader must observe at least one frame");
    }

    #[test]
    fn partial_payload_writes_a_complete_final_word() {
        let word = AtomicU64::new(u64::MAX);
        // SAFETY: one aligned word is live for the entire access.
        let view = unsafe { super::AtomicPayload::new(word.as_ptr().cast(), 3) };
        view.store_from(&[1, 2, 3]);
        assert_eq!(
            word.load(Ordering::Relaxed).to_ne_bytes(),
            [1, 2, 3, 0, 0, 0, 0, 0]
        );
    }

    /// H6: the atomic payload path round-trips every length class (empty word
    /// region, tail-only, word-only, word + tail) byte-exactly.
    #[cfg_attr(
        miri,
        ignore = "filesystem/mmap I/O is unsupported under Miri isolation"
    )]
    #[test]
    fn atomic_payload_round_trips_all_tail_lengths() {
        let tmp = TestShmDir::new("tail-lengths");
        let shm_path = tmp.path.join("tail.shm");
        let mut writer = ShmFrameRingWriter::create(&shm_path, 4 * 4 + 7).expect("create ring");
        let reader = ShmFrameRingReader::open(&shm_path).expect("open reader");
        let mut buf = Vec::new();
        for extra in 0..8_usize {
            let len = 4 * 4 + extra.min(7);
            let pixels: Vec<u8> = (0..len)
                .map(|i| u8::try_from((i * 37 + extra) % 256).expect("byte"))
                .collect();
            writer
                .publish(
                    solid_frame(i64::try_from(extra).expect("i64"), 2, 2),
                    &pixels,
                )
                .expect("publish");
            reader
                .read_latest(&mut buf)
                .expect("read")
                .expect("frame present");
            assert_eq!(
                buf, pixels,
                "payload of {len} bytes must round-trip exactly"
            );
        }
    }

    #[cfg_attr(
        miri,
        ignore = "filesystem/mmap I/O is unsupported under Miri isolation"
    )]
    #[test]
    fn shm_validates_pixel_formats_and_rejects_oversized_frames() {
        assert_eq!(PixelFormat::Nv12.required_buffer_bytes(1, 1, 1), None);
        assert_eq!(PixelFormat::Nv12.required_buffer_bytes(1, 1, 2), Some(4));
        assert_eq!(PixelFormat::Nv12.required_buffer_bytes(3, 3, 3), None);
        assert_eq!(PixelFormat::Nv12.required_buffer_bytes(3, 3, 4), Some(20));
        assert_eq!(
            PixelFormat::Nv12.required_buffer_bytes(1920, 1080, 1920),
            Some(1920 * (1080 + 540)),
            "NV12 1080p size should equal Y + half-height UV"
        );
        assert_eq!(
            PixelFormat::Rgb24.required_buffer_bytes(10, 10, 29),
            None,
            "stride narrower than width*3 must be rejected"
        );

        let tmp = TestShmDir::new("validate");
        let shm_path = tmp.path.join("small.shm");
        let mut writer = ShmFrameRingWriter::create(&shm_path, 64).expect("create small ring");
        let err = writer
            .publish(
                FrameDescriptor {
                    frame_index: 0,
                    is_approximate: false,
                    width: 8,
                    height: 8,
                    stride: 32,
                    pixel_format: PixelFormat::Bgra8,
                    boottime_ns: 0,
                },
                &[0_u8; 256],
            )
            .expect_err("oversized payload must be rejected");
        assert!(
            matches!(err, OsError::InvalidArgument { .. }),
            "expected InvalidArgument for oversized payload, got {err:?}"
        );
    }

    #[cfg_attr(
        miri,
        ignore = "filesystem/mmap I/O is unsupported under Miri isolation"
    )]
    #[test]
    fn recreate_ring_at_same_path_never_invalidates_existing_mapped_reader() {
        let tmp = TestShmDir::new("recreate-no-sigbus");
        let shm_path = tmp.path.join("viewport-recreate.shm");

        // 1. Create initial ring with 64 KiB slot capacity and publish frame 1.
        let old_cap = 64 * 1024;
        let mut writer1 =
            ShmFrameRingWriter::create(&shm_path, old_cap).expect("create initial SHM ring");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = fs::metadata(&shm_path)
                .expect("stat created SHM file")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                mode, 0o600,
                "SHM ring file must be created with owner-only 0600 permissions, got {mode:#o}"
            );
        }

        let old_pixels = vec![0xAA_u8; old_cap];
        writer1
            .publish(
                FrameDescriptor {
                    frame_index: 10,
                    is_approximate: false,
                    width: 128,
                    height: 128,
                    stride: 512,
                    pixel_format: PixelFormat::Bgra8,
                    boottime_ns: 1_000_000,
                },
                &old_pixels,
            )
            .expect("publish frame on initial ring");

        let reader1 = ShmFrameRingReader::open(&shm_path).expect("open reader on initial ring");
        let mut buf1 = Vec::new();
        let meta1 = reader1
            .read_latest(&mut buf1)
            .expect("initial read must succeed")
            .expect("initial frame must be present");
        assert_eq!(meta1.frame_index, 10, "initial frame_index mismatch");
        assert_eq!(buf1, old_pixels, "initial pixels mismatch");

        // 2. While `reader1` is still mapped, re-create the ring at the exact
        //    same path with a MUCH SMALLER slot capacity (which would truncate
        //    and SIGBUS `reader1` if opened in place).
        drop(writer1);
        let new_cap = 256;
        let mut writer2 = ShmFrameRingWriter::create(&shm_path, new_cap)
            .expect("re-creating SHM ring at same path while reader1 is mapped must succeed");
        let new_pixels = vec![0xBB_u8; new_cap];
        writer2
            .publish(
                FrameDescriptor {
                    frame_index: 99,
                    is_approximate: true,
                    width: 8,
                    height: 8,
                    stride: 32,
                    pixel_format: PixelFormat::Bgra8,
                    boottime_ns: 2_000_000,
                },
                &new_pixels,
            )
            .expect("publish frame on replacement ring");

        // 3. Touching the full 64 KiB mapping on `reader1` must NOT SIGBUS and
        //    must still read the old ring's intact frame.
        let mut buf1_again = Vec::new();
        let meta1_again = reader1
            .read_latest(&mut buf1_again)
            .expect("reading old mapped reader after ring replacement must succeed")
            .expect("old frame must remain intact");
        assert_eq!(
            meta1_again.frame_index, 10,
            "old reader must keep seeing old ring frame_index"
        );
        assert_eq!(
            buf1_again, old_pixels,
            "old reader mapping must remain intact without SIGBUS"
        );

        // 4. A freshly opened reader observes the new ring and new slot capacity.
        let reader2 = ShmFrameRingReader::open(&shm_path).expect("open reader on replacement ring");
        assert_eq!(
            reader2.slot_capacity(),
            new_cap,
            "new reader must see new ring slot_capacity"
        );
        let mut buf2 = Vec::new();
        let meta2 = reader2
            .read_latest(&mut buf2)
            .expect("new reader read_latest must succeed")
            .expect("new frame must be present");
        assert_eq!(meta2.frame_index, 99, "new reader frame_index mismatch");
        assert_eq!(buf2, new_pixels, "new reader pixels mismatch");
    }

    fn m1_fields(generation: u64, len: usize) -> super::SlotFieldsSnapshot {
        super::SlotFieldsSnapshot {
            generation,
            frame_index: i64::try_from(generation).expect("i64"),
            boottime_ns: 0,
            width: 1,
            height: 1,
            stride: 4,
            pixel_format: PixelFormat::Rgba8.to_raw(),
            payload_len: u32::try_from(len).expect("len"),
            flags: 0,
        }
    }

    /// M1 (Miri): the atomic payload copy round-trips every word/tail length class
    /// over a 64-byte aligned heap slot, never touching bytes past `len`.
    #[test]
    fn m1_atomic_payload_round_trips_boundary_lengths() {
        let alloc = SharedRingAlloc::new(24);
        let payload_ptr =
            super::slot_payload_ptr(alloc.view.base, 0, alloc.view.aligned_slot_stride);
        for len in [0_usize, 1, 7, 8, 9, 15, 16, 17] {
            let src: Vec<u8> = (0..len)
                .map(|b| u8::try_from((b * 31 + len) % 256).expect("byte"))
                .collect();
            // SAFETY: slot 0 payload is a live, 64-byte aligned region of 24 >= len bytes.
            let view = unsafe { super::AtomicPayload::new(payload_ptr, len) };
            view.store_from(&src);
            let mut dst = vec![0_u8; len];
            view.load_into(&mut dst, &mut || {});
            assert_eq!(dst, src, "len={len}");
        }
    }

    /// M1 (Miri): heap-backed ring publish/read across the boundary lengths, including a
    /// payload filling the slot exactly up to its unaligned tail; payloads shorter than the
    /// 1x1 RGBA requirement are rejected instead of decoded.
    #[test]
    fn m1_heap_ring_round_trips_boundary_lengths_and_full_tail() {
        const CAP: usize = 17;
        let alloc = SharedRingAlloc::new(CAP);
        let view = alloc.view;
        let mut buf = Vec::new();
        for (i, len) in [0_usize, 1, 7, 8, 9, 15, 16, 17].into_iter().enumerate() {
            let generation = u64::try_from(i + 1).expect("gen");
            let pixels: Vec<u8> = (0..len)
                .map(|b| u8::try_from((b * 31 + i) % 256).expect("byte"))
                .collect();
            let slot = u8::try_from(i % 3).expect("slot");
            // SAFETY: single writer over the initialized allocation, slot < 3, len <= CAP.
            unsafe { view.publish_slot(slot, &m1_fields(generation, len), &pixels) };
            // SAFETY: `view` describes the live allocation owned by `alloc`.
            let read = unsafe { view.read_latest(&mut buf, || {}) };
            if len < 4 {
                assert!(
                    matches!(read, Err(OsError::InvalidShmRing { .. })),
                    "len={len}: {read:?}"
                );
                continue;
            }
            let meta = read.expect("read").expect("published frame");
            assert_eq!(meta.generation, generation);
            assert_eq!(buf, pixels, "len={len}");
        }
    }

    /// M1 (Miri): one writer and two readers over the same allocation across many
    /// generations; Miri's data-race detector checks every overlapping access.
    #[test]
    fn m1_one_writer_two_readers_multi_generation_is_race_free() {
        const CAP: usize = 17;
        const GENERATIONS: u64 = if cfg!(miri) { 24 } else { 2_000 };
        let alloc = Arc::new(SharedRingAlloc::new(CAP));
        let done = Arc::new(AtomicBool::new(false));
        let readers: Vec<_> = (0..2)
            .map(|_| {
                let alloc = Arc::clone(&alloc);
                let done = Arc::clone(&done);
                thread::spawn(move || {
                    let view = alloc.view;
                    let mut buf = Vec::new();
                    let mut last = 0_u64;
                    while !done.load(Ordering::Acquire) {
                        // SAFETY: `view` describes the allocation kept alive by `alloc`.
                        match unsafe { view.read_latest(&mut buf, || {}) } {
                            Ok(Some(meta)) => {
                                let fill = u8::try_from(meta.generation % 251).expect("byte");
                                assert!(buf.iter().all(|&b| b == fill), "torn frame");
                                assert!(meta.generation >= last, "generation went backwards");
                                last = meta.generation;
                            }
                            Ok(None) => {}
                            Err(OsError::InvalidShmRing { reason })
                                if reason == super::SEQLOCK_CONTENTION => {}
                            Err(other) => panic!("unexpected read error: {other:?}"),
                        }
                        thread::yield_now();
                    }
                })
            })
            .collect();
        let view = alloc.view;
        for generation in 1..=GENERATIONS {
            let len = 4 + usize::try_from(generation % 14).expect("len");
            let frame = vec![u8::try_from(generation % 251).expect("byte"); len];
            let slot = u8::try_from((generation - 1) % 3).expect("slot");
            // SAFETY: single writer, slot < 3, len <= CAP.
            unsafe { view.publish_slot(slot, &m1_fields(generation, len), &frame) };
            thread::yield_now();
        }
        done.store(true, Ordering::Release);
        for r in readers {
            r.join().expect("reader must not panic");
        }
    }
}

/// Layer-1 PDD (K8): bounded model checking of SHM sizing and layout arithmetic.
#[cfg(kani)]
mod kani_proofs {
    use super::{
        CACHE_LINE_BYTES, HEADER_REGION_BYTES, PixelFormat, RING_HEADER_BYTES, SHM_SLOT_COUNT,
        SLOT_HEADER_BYTES, align_up, compute_layout,
    };

    fn any_format() -> PixelFormat {
        match kani::any::<u8>() % 4 {
            0 => PixelFormat::Bgra8,
            1 => PixelFormat::Rgba8,
            2 => PixelFormat::Rgb24,
            _ => PixelFormat::Nv12,
        }
    }

    /// Reference row count and minimum row bytes for `fmt`, in exact `u128` arithmetic.
    fn reference(fmt: PixelFormat, w: u32, h: u32) -> (u128, u128) {
        let min_row = match fmt {
            PixelFormat::Bgra8 | PixelFormat::Rgba8 => u128::from(w) * 4,
            PixelFormat::Rgb24 => u128::from(w) * 3,
            PixelFormat::Nv12 => u128::from(w.div_ceil(2)) * 2,
        };
        let rows = match fmt {
            PixelFormat::Nv12 => u128::from(h) + u128::from(h.div_ceil(2)),
            _ => u128::from(h),
        };
        (min_row, rows)
    }

    /// Full `u32` width/stride domain with the height bounded: proving two unbounded 64-bit
    /// multipliers equivalent is SAT-hard and did not finish within the CI timebox, while a
    /// 12-bit height keeps the multiplier small. The `u64` overflow edge, reachable only for
    /// heights above 2^31, is pinned by `k8_required_buffer_bytes_rejects_u64_overflow`.
    #[kani::proof]
    fn k8_required_buffer_bytes_exact_or_none() {
        let fmt = any_format();
        let (w, h, stride): (u32, u32, u32) = (kani::any(), kani::any(), kani::any());
        kani::assume(h <= 1 << 12);
        let (min_row, rows) = reference(fmt, w, h);
        let r = fmt.required_buffer_bytes(w, h, stride);
        assert_eq!(
            r.is_none(),
            w == 0 || h == 0 || u128::from(stride) < min_row,
            "None iff zero dimension or undersized stride (no overflow below 2^31 rows)"
        );
        if let Some(len) = r {
            assert_eq!(
                len as u128,
                u128::from(stride) * rows,
                "exact total, no truncation"
            );
        }
    }

    /// The only overflowing shapes: rows x stride above `u64::MAX` (NV12 with h > 2^31 and a
    /// near-`u32::MAX` stride) is rejected, while the largest packed RGBA frame still fits.
    #[kani::proof]
    fn k8_required_buffer_bytes_rejects_u64_overflow() {
        let w: u32 = kani::any();
        kani::assume(w >= 1 && u64::from(w) * 4 <= u64::from(u32::MAX));
        assert_eq!(
            PixelFormat::Nv12.required_buffer_bytes(w, u32::MAX, u32::MAX),
            None,
            "NV12 rows x stride overflows u64"
        );
        let rgba = PixelFormat::Rgba8.required_buffer_bytes(w, u32::MAX, u32::MAX);
        assert_eq!(
            rgba.map(|n| n as u128),
            Some(u128::from(u32::MAX) * u128::from(u32::MAX)),
            "(2^32-1)^2 still fits in u64"
        );
    }

    /// Every pixel (and both NV12 chroma bytes) of an accepted frame lies inside the buffer.
    ///
    /// `k8_required_buffer_bytes_exact_or_none` proves `len == stride * rows`, so byte
    /// `row * stride + col` is in bounds exactly when `row < rows` and `col < stride`. This
    /// harness proves those two linear facts over the full `u32` domain; asking CBMC for the
    /// symbolic product `y * stride` directly did not finish (nonlinear 64-bit arithmetic).
    #[kani::proof]
    fn k8_required_buffer_bytes_covers_every_pixel() {
        let fmt = any_format();
        let (w, h, stride): (u32, u32, u32) = (kani::any(), kani::any(), kani::any());
        kani::assume(h <= 1 << 12);
        if fmt.required_buffer_bytes(w, h, stride).is_none() {
            return;
        }
        let (x, y): (u32, u32) = (kani::any(), kani::any());
        kani::assume(x < w && y < h);
        let (_, rows) = reference(fmt, w, h);
        let (x, y, s) = (u128::from(x), u128::from(y), u128::from(stride));
        assert!(y < rows, "row in bounds");
        match fmt {
            PixelFormat::Bgra8 | PixelFormat::Rgba8 => assert!(x * 4 + 3 < s),
            PixelFormat::Rgb24 => assert!(x * 3 + 2 < s),
            PixelFormat::Nv12 => {
                assert!(x < s, "luma in bounds");
                assert!(u128::from(h) + y / 2 < rows, "chroma row in bounds");
                assert!((x / 2) * 2 + 1 < s, "U and V bytes in bounds");
            }
        }
    }

    #[kani::proof]
    fn k8_align_up_exact() {
        let value: usize = kani::any();
        let align = [1usize, 2, 8, CACHE_LINE_BYTES, 4096][usize::from(kani::any::<u8>() % 5)];
        match align_up(value, align) {
            Some(r) => {
                assert!(r >= value && r % align == 0 && r - value < align);
            }
            None => assert!(value > usize::MAX - (align - 1)),
        }
    }

    #[kani::proof]
    #[kani::unwind(4)]
    fn k8_compute_layout_slots_disjoint_and_aligned() {
        let cap: usize = kani::any();
        let Some(l) = compute_layout(cap) else {
            assert!(
                cap == 0
                    || align_up(cap, CACHE_LINE_BYTES)
                        .and_then(|s| s.checked_mul(SHM_SLOT_COUNT))
                        .and_then(|s| s.checked_add(HEADER_REGION_BYTES))
                        .is_none()
            );
            return;
        };
        assert!(l.aligned_slot_stride >= cap && l.aligned_slot_stride % CACHE_LINE_BYTES == 0);
        assert_eq!(HEADER_REGION_BYTES % CACHE_LINE_BYTES, 0);
        let mut prev_end = HEADER_REGION_BYTES;
        let mut i = 0;
        while i < SHM_SLOT_COUNT {
            let hdr = RING_HEADER_BYTES + i * SLOT_HEADER_BYTES;
            assert!(hdr % CACHE_LINE_BYTES == 0 && hdr + SLOT_HEADER_BYTES <= HEADER_REGION_BYTES);
            let payload = HEADER_REGION_BYTES + i * l.aligned_slot_stride;
            assert!(payload % CACHE_LINE_BYTES == 0, "payload 64-byte aligned");
            assert!(payload >= prev_end, "slots disjoint");
            prev_end = payload + cap;
            assert!(prev_end <= l.total_bytes, "slot inside mapping");
            i += 1;
        }
        assert_eq!(
            HEADER_REGION_BYTES + SHM_SLOT_COUNT * l.aligned_slot_stride,
            l.total_bytes
        );
    }
}
