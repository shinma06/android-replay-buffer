//! Triple-buffered shared-memory frame ring for high-FPS zero-JSON video delivery.
//!
//! `arbd` publishes decoded frames through [`ShmFrameRingWriter`] into a
//! file-backed memory map containing 3 cache-line-aligned slots, while IDE or
//! viewer clients attach with [`ShmFrameRingReader`]. Each slot uses an atomic
//! `Release`/`Acquire` sequence lock paired with an atomic published slot index
//! and generation counter so readers never observe torn frames and always see
//! the exact `frame_index` and `is_approximate` preview badge state.

use std::fs::{self, File, OpenOptions};
use std::mem::{align_of, offset_of, size_of};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};

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
const MAX_SEQLOCK_RETRIES: usize = 64;

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
            Self::Nv12 => u64::from(width),
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

#[repr(C)]
#[derive(Clone, Copy)]
struct RawSlotFields {
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
        let aligned_slot_stride =
            align_up(slot_capacity, CACHE_LINE_BYTES).ok_or_else(|| OsError::InvalidArgument {
                name: "slot_capacity",
                reason: "slot_capacity alignment overflowed usize".to_owned(),
            })?;
        let total_size = aligned_slot_stride
            .checked_mul(SHM_SLOT_COUNT)
            .and_then(|s| s.checked_add(HEADER_REGION_BYTES))
            .ok_or_else(|| OsError::InvalidArgument {
                name: "slot_capacity",
                reason: "total shared-memory ring size overflowed usize".to_owned(),
            })?;
        let total_size_u64 = u64::try_from(total_size).map_err(|_| OsError::InvalidArgument {
            name: "slot_capacity",
            reason: "total shared-memory ring size overflowed u64".to_owned(),
        })?;

        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|source| OsError::Fs {
                operation: "create_shm_parent_dir",
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .map_err(|source| OsError::Fs {
                operation: "create_shm_file",
                path: path.clone(),
                source,
            })?;

        set_owner_only_file_permissions(&path)?;

        file.set_len(total_size_u64).map_err(|source| OsError::Fs {
            operation: "resize_shm_file",
            path: path.clone(),
            source,
        })?;

        // SAFETY: `file` is freshly created and sized to `total_size`; we
        // initialize the ring header before publishing any slot to readers.
        let mut mmap = unsafe { MmapMut::map_mut(&file) }.map_err(|source| OsError::Fs {
            operation: "mmap_shm_writer",
            path: path.clone(),
            source,
        })?;

        if mmap.as_ptr().align_offset(align_of::<RawRingHeader>()) != 0 {
            return Err(OsError::InvalidShmRing {
                reason: "memory mapping base address is not 64-byte aligned".to_owned(),
            });
        }

        init_ring_headers(&mut mmap, slot_capacity)?;

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
        let slot_idx = usize::from(slot_idx_u8);
        let slot_idx_u32 = u32::from(slot_idx_u8);
        let base_ptr = self.mmap.as_mut_ptr();
        let slot_base = slot_base_ptr_mut(base_ptr, slot_idx);
        let seq_ptr = slot_seq_ptr(slot_base.cast_const());
        let fields_ptr = slot_fields_ptr_mut(slot_base);
        let payload_ptr = slot_payload_ptr_mut(base_ptr, slot_idx, self.aligned_slot_stride);

        // SAFETY: `seq_ptr` points to the 64-byte aligned `AtomicU64` sequence
        // counter at offset 0 of slot `slot_idx` inside the live mapping.
        let seq_atomic = unsafe { &*seq_ptr };
        let seq_before = seq_atomic.load(Ordering::Relaxed);
        // Mark slot as in-progress (odd sequence number) before touching payload/meta.
        seq_atomic.store(seq_before.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);

        // SAFETY: `payload_ptr` points to `self.slot_capacity` writable bytes in
        // slot `slot_idx`, non-overlapping with `pixels`, and `pixels.len() <= self.slot_capacity`.
        unsafe {
            ptr::copy_nonoverlapping(pixels.as_ptr(), payload_ptr, pixels.len());
        }

        let flags = if descriptor.is_approximate {
            FLAG_IS_APPROXIMATE
        } else {
            0
        };
        let fields = RawSlotFields {
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
        // SAFETY: `fields_ptr` is 8-byte aligned inside slot `slot_idx` and
        // owned exclusively by the single writer while `seq` is odd.
        unsafe {
            ptr::write_volatile(fields_ptr, fields);
        }

        // Publish completed slot (even sequence number) with Release semantics.
        seq_atomic.store(seq_before.wrapping_add(2), Ordering::Release);

        let ring_hdr_ptr = ring_header_ptr_const(base_ptr.cast_const());
        // SAFETY: `ring_hdr_ptr` is 64-byte aligned and initialized in `create`.
        let ring_hdr = unsafe { &*ring_hdr_ptr };
        ring_hdr
            .published_slot
            .store(slot_idx_u32, Ordering::Release);
        ring_hdr
            .published_generation
            .store(next_gen, Ordering::Release);

        self.generation = next_gen;
        self.next_slot = (slot_idx_u8 + 1) % SHM_SLOT_COUNT_U8;
        Ok(next_gen)
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

        let aligned_slot_stride =
            align_up(slot_capacity, CACHE_LINE_BYTES).ok_or_else(|| OsError::InvalidShmRing {
                reason: "aligned slot stride overflowed usize".to_owned(),
            })?;
        let required_len = aligned_slot_stride
            .checked_mul(SHM_SLOT_COUNT)
            .and_then(|s| s.checked_add(HEADER_REGION_BYTES))
            .ok_or_else(|| OsError::InvalidShmRing {
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
        let base_ptr = self.mmap.as_ptr();
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

            let fields_ptr = slot_fields_ptr_const(slot_base);
            // SAFETY: `fields_ptr` is 8-byte aligned within the validated slot
            // header; consistency is checked against `seq2` below.
            let raw = unsafe { ptr::read_volatile(fields_ptr) };
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
            let payload_ptr = slot_payload_ptr_const(base_ptr, slot_idx, self.aligned_slot_stride);
            // SAFETY: `payload_len <= self.slot_capacity` and `out_pixels` has
            // been resized to `payload_len`, non-overlapping with the mmap.
            unsafe {
                ptr::copy_nonoverlapping(payload_ptr, out_pixels.as_mut_ptr(), payload_len);
            }

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
            reason: "shared-memory frame seqlock did not stabilize under contention".to_owned(),
        })
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

fn init_ring_headers(mmap: &mut MmapMut, slot_capacity: usize) -> Result<(), OsError> {
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

const fn slot_base_ptr_mut(base: *mut u8, slot_idx: usize) -> *mut u8 {
    let offset = RING_HEADER_BYTES + slot_idx * SLOT_HEADER_BYTES;
    base.wrapping_add(offset)
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
const fn slot_fields_ptr_mut(slot_base: *mut u8) -> *mut RawSlotFields {
    slot_base
        .wrapping_add(offset_of!(RawSlotHeader, fields))
        .cast::<RawSlotFields>()
}

#[expect(
    clippy::cast_ptr_alignment,
    reason = "RawSlotFields sits at offset 8 inside each 64-byte aligned slot header"
)]
const fn slot_fields_ptr_const(slot_base: *const u8) -> *const RawSlotFields {
    slot_base
        .wrapping_add(offset_of!(RawSlotHeader, fields))
        .cast::<RawSlotFields>()
}

const fn slot_payload_ptr_mut(base: *mut u8, slot_idx: usize, aligned_stride: usize) -> *mut u8 {
    let offset = HEADER_REGION_BYTES + slot_idx * aligned_stride;
    base.wrapping_add(offset)
}

const fn slot_payload_ptr_const(
    base: *const u8,
    slot_idx: usize,
    aligned_stride: usize,
) -> *const u8 {
    let offset = HEADER_REGION_BYTES + slot_idx * aligned_stride;
    base.wrapping_add(offset)
}

const fn align_up(value: usize, align: usize) -> Option<usize> {
    let rem = value % align;
    if rem == 0 {
        Some(value)
    } else {
        value.checked_add(align - rem)
    }
}

#[cfg(unix)]
fn set_owner_only_file_permissions(path: &Path) -> Result<(), OsError> {
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, Permissions::from_mode(0o600)).map_err(|source| OsError::Fs {
        operation: "chmod_shm_0600",
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn set_owner_only_file_permissions(_path: &Path) -> Result<(), OsError> {
    Ok(())
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
        let done = Arc::new(AtomicBool::new(false));
        let reader_done = Arc::clone(&done);

        let reader_thread = thread::spawn(move || {
            let mut buf = Vec::with_capacity(FRAME_BYTES);
            let mut last_gen = 0_u64;
            let mut observed = 0_u64;
            while !reader_done.load(Ordering::Acquire) || observed < 10 {
                if let Some(meta) = reader
                    .read_latest(&mut buf)
                    .expect("concurrent read_latest must not fail")
                {
                    assert!(
                        meta.generation >= last_gen,
                        "generation moved backwards: {} < {}",
                        meta.generation,
                        last_gen
                    );
                    last_gen = meta.generation;
                    let expected_byte =
                        u8::try_from(meta.frame_index.rem_euclid(251)).expect("byte in 0..251");
                    assert!(
                        buf.iter().all(|&b| b == expected_byte),
                        "detected torn frame for frame_index={} generation={}",
                        meta.frame_index,
                        meta.generation
                    );
                    observed += 1;
                }
            }
            observed
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
        }
        done.store(true, Ordering::Release);

        let observed = reader_thread.join().expect("reader thread must not panic");
        assert!(
            observed >= 10,
            "reader thread should observe frames during stress run, saw {observed}"
        );
    }

    #[test]
    fn shm_validates_pixel_formats_and_rejects_oversized_frames() {
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
}
