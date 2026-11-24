//! macOS hardware H.264 decode and zero-copy head-GOP re-encode backend via direct C FFI
//! to Apple `VideoToolbox`, `CoreMedia`, `CoreVideo`, and `CoreFoundation` (zero external crates).
//!
//! Every `unsafe` block performs a single audited FFI operation with an explicit `// SAFETY:`
//! justification, and all `CoreFoundation` / `CoreVideo` / `VideoToolbox` handles are wrapped in RAII
//! guards so early returns never leak memory or pixel-buffer locks.

#![cfg(target_os = "macos")]

use std::ffi::c_void;
use std::ptr::{NonNull, null, null_mut};

use crate::codec::{
    DecodedFrame, EncodeParams, EncodedGop, EncodedSample, MutableYuvFrame, PixelFormat, RgbImage,
    VideoCodecBackend, convert_from_yuv420p, fit_thumbnail_dimensions, nv12_to_rgb24,
    nv12_to_thumb_rgb24, nv12_to_yuv420p,
};
use crate::error::MediaError;
use crate::mp4::{Mp4VideoTrack, build_avcc_payload, extract_inband_param_sets_and_strip};

type OSStatus = i32;
type CFTypeRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFStringRef = *const c_void;
type CFNumberRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFMutableDictionaryRef = *mut c_void;
type CFBooleanRef = *const c_void;
type CFArrayRef = *const c_void;
type CMFormatDescriptionRef = *const c_void;
type CMVideoFormatDescriptionRef = *const c_void;
type CMBlockBufferRef = *mut c_void;
type CMSampleBufferRef = *mut c_void;
type CVImageBufferRef = *mut c_void;
type CVPixelBufferRef = *mut c_void;
type VTDecompressionSessionRef = *mut c_void;
type VTCompressionSessionRef = *mut c_void;
type VTSessionRef = *mut c_void;

/// `FourCC` `'420v'`: bi-planar NV12 video-range pixel format native to Apple Silicon hardware decode.
const K_CV_PIXEL_FORMAT_420V: i32 = 0x3432_3076;
/// Unsigned `FourCC` `'420v'` for `CVPixelBufferCreate`.
const K_CV_PIXEL_FORMAT_420V_U32: u32 = 0x3432_3076;
/// `FourCC` `'avc1'`: H.264 video codec identifier for `VTCompressionSessionCreate`.
const K_CM_VIDEO_CODEC_H264: u32 = 0x6176_6331;
/// `kCFNumberSInt32Type` enum constant from `CoreFoundation`.
const K_CF_NUMBER_SINT32_TYPE: isize = 3;
/// `kCVPixelBufferLock_ReadOnly` flag from `CoreVideo`.
const K_CV_PIXEL_BUFFER_LOCK_READ_ONLY: u64 = 1;
/// `kVTDecodeInfo_FrameDropped` bit from `VideoToolbox`.
const K_VT_DECODE_INFO_FRAME_DROPPED: u32 = 1 << 1;

#[repr(C)]
#[derive(Copy, Clone, Debug)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

const K_CM_TIME_INVALID: CMTime = CMTime {
    value: 0,
    timescale: 0,
    flags: 0,
    epoch: 0,
};

const fn cm_time_make(value: i64, timescale: i32) -> CMTime {
    CMTime {
        value,
        timescale,
        flags: 1, // kCMTimeFlags_Valid
        epoch: 0,
    }
}

#[repr(C)]
#[derive(Copy, Clone)]
struct CMSampleTimingInfo {
    duration: CMTime,
    presentation_time_stamp: CMTime,
    decode_time_stamp: CMTime,
}

type VTDecompressionOutputCallback = extern "C" fn(
    decompression_output_ref_con: *mut c_void,
    source_frame_ref_con: *mut c_void,
    status: OSStatus,
    info_flags: u32,
    image_buffer: CVImageBufferRef,
    presentation_time_stamp: CMTime,
    presentation_duration: CMTime,
);

#[repr(C)]
struct VTDecompressionOutputCallbackRecord {
    decompression_output_callback: VTDecompressionOutputCallback,
    decompression_output_ref_con: *mut c_void,
}

type VTCompressionOutputCallback = extern "C" fn(
    output_callback_ref_con: *mut c_void,
    source_frame_ref_con: *mut c_void,
    status: OSStatus,
    info_flags: u32,
    sample_buffer: CMSampleBufferRef,
);

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFBooleanTrue: CFBooleanRef;
    static kCFBooleanFalse: CFBooleanRef;
    static kCFAllocatorNull: CFAllocatorRef;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;

    fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    fn CFRelease(cf: CFTypeRef);
    fn CFDictionaryCreateMutable(
        allocator: CFAllocatorRef,
        capacity: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFMutableDictionaryRef;
    fn CFDictionarySetValue(dict: CFMutableDictionaryRef, key: *const c_void, value: *const c_void);
    fn CFDictionaryGetValue(dict: CFDictionaryRef, key: *const c_void) -> *const c_void;
    fn CFNumberCreate(
        allocator: CFAllocatorRef,
        the_type: isize,
        value_ptr: *const c_void,
    ) -> CFNumberRef;
    fn CFArrayGetCount(the_array: CFArrayRef) -> isize;
    fn CFArrayGetValueAtIndex(the_array: CFArrayRef, idx: isize) -> *const c_void;
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    static kCVPixelBufferPixelFormatTypeKey: CFStringRef;
    static kCVPixelBufferWidthKey: CFStringRef;
    static kCVPixelBufferHeightKey: CFStringRef;

    fn CVPixelBufferCreate(
        allocator: CFAllocatorRef,
        width: usize,
        height: usize,
        pixel_format_type: u32,
        pixel_buffer_attributes: CFDictionaryRef,
        pixel_buffer_out: *mut CVPixelBufferRef,
    ) -> i32;
    fn CVPixelBufferLockBaseAddress(pixel_buffer: CVPixelBufferRef, lock_flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(pixel_buffer: CVPixelBufferRef, unlock_flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddressOfPlane(
        pixel_buffer: CVPixelBufferRef,
        plane_index: usize,
    ) -> *const u8;
    fn CVPixelBufferGetBytesPerRowOfPlane(
        pixel_buffer: CVPixelBufferRef,
        plane_index: usize,
    ) -> usize;
    fn CVPixelBufferGetWidth(pixel_buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetHeight(pixel_buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetWidthOfPlane(pixel_buffer: CVPixelBufferRef, plane_index: usize) -> usize;
    fn CVPixelBufferGetHeightOfPlane(pixel_buffer: CVPixelBufferRef, plane_index: usize) -> usize;
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    static kCMSampleAttachmentKey_NotSync: CFStringRef;

    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: CFAllocatorRef,
        parameter_set_count: usize,
        parameter_set_pointers: *const *const u8,
        parameter_set_sizes: *const usize,
        nal_unit_header_length: i32,
        format_description_out: *mut CMFormatDescriptionRef,
    ) -> OSStatus;

    fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        video_desc: CMFormatDescriptionRef,
        parameter_set_index: usize,
        parameter_set_pointer_out: *mut *const u8,
        parameter_set_size_out: *mut usize,
        parameter_set_count_out: *mut usize,
        nal_unit_header_length_out: *mut i32,
    ) -> OSStatus;

    fn CMBlockBufferCreateWithMemoryBlock(
        structure_allocator: CFAllocatorRef,
        memory_block: *mut c_void,
        block_length: usize,
        block_allocator: CFAllocatorRef,
        custom_block_source: *const c_void,
        offset_to_data: usize,
        data_length: usize,
        flags: u32,
        block_buffer_out: *mut CMBlockBufferRef,
    ) -> OSStatus;

    fn CMBlockBufferGetDataLength(the_buffer: CMBlockBufferRef) -> usize;
    fn CMBlockBufferCopyDataBytes(
        the_source_buffer: CMBlockBufferRef,
        offset_to_data: usize,
        data_length: usize,
        destination: *mut c_void,
    ) -> OSStatus;

    fn CMSampleBufferCreateReady(
        allocator: CFAllocatorRef,
        data_buffer: CMBlockBufferRef,
        format_description: CMFormatDescriptionRef,
        num_samples: isize,
        num_sample_timing_entries: isize,
        sample_timing_array: *const CMSampleTimingInfo,
        num_sample_size_entries: isize,
        sample_size_array: *const usize,
        sample_buffer_out: *mut CMSampleBufferRef,
    ) -> OSStatus;

    fn CMSampleBufferGetDataBuffer(sbuf: CMSampleBufferRef) -> CMBlockBufferRef;
    fn CMSampleBufferGetFormatDescription(sbuf: CMSampleBufferRef) -> CMVideoFormatDescriptionRef;
    fn CMSampleBufferGetSampleAttachmentsArray(
        sbuf: CMSampleBufferRef,
        create_if_necessary: u8,
    ) -> CFArrayRef;
}

#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C" {
    static kVTCompressionPropertyKey_AllowFrameReordering: CFStringRef;
    static kVTCompressionPropertyKey_AverageBitRate: CFStringRef;
    static kVTCompressionPropertyKey_RealTime: CFStringRef;
    static kVTCompressionPropertyKey_ProfileLevel: CFStringRef;
    static kVTProfileLevel_H264_High_AutoLevel: CFStringRef;
    static kVTEncodeFrameOptionKey_ForceKeyFrame: CFStringRef;

    fn VTDecompressionSessionCreate(
        allocator: CFAllocatorRef,
        video_format_description: CMVideoFormatDescriptionRef,
        video_decoder_specification: CFDictionaryRef,
        destination_image_buffer_attributes: CFDictionaryRef,
        output_callback: *const VTDecompressionOutputCallbackRecord,
        decompression_session_out: *mut VTDecompressionSessionRef,
    ) -> OSStatus;

    fn VTDecompressionSessionDecodeFrame(
        session: VTDecompressionSessionRef,
        sample_buffer: CMSampleBufferRef,
        decode_flags: u32,
        source_frame_ref_con: *mut c_void,
        info_flags_out: *mut u32,
    ) -> OSStatus;

    fn VTDecompressionSessionInvalidate(session: VTDecompressionSessionRef);

    fn VTCompressionSessionCreate(
        allocator: CFAllocatorRef,
        width: i32,
        height: i32,
        codec_type: u32,
        encoder_specification: CFDictionaryRef,
        source_image_buffer_attributes: CFDictionaryRef,
        compressed_data_allocator: CFAllocatorRef,
        output_callback: VTCompressionOutputCallback,
        output_callback_ref_con: *mut c_void,
        compression_session_out: *mut VTCompressionSessionRef,
    ) -> OSStatus;

    fn VTSessionSetProperty(
        session: VTSessionRef,
        property_key: CFStringRef,
        property_value: CFTypeRef,
    ) -> OSStatus;

    fn VTCompressionSessionPrepareToEncodeFrames(session: VTCompressionSessionRef) -> OSStatus;

    fn VTCompressionSessionEncodeFrame(
        session: VTCompressionSessionRef,
        image_buffer: CVImageBufferRef,
        presentation_time_stamp: CMTime,
        duration: CMTime,
        frame_properties: CFDictionaryRef,
        source_frame_ref_con: *mut c_void,
        info_flags_out: *mut u32,
    ) -> OSStatus;

    fn VTCompressionSessionCompleteFrames(
        session: VTCompressionSessionRef,
        complete_until_pts: CMTime,
    ) -> OSStatus;

    fn VTCompressionSessionInvalidate(session: VTCompressionSessionRef);
}

/// RAII wrapper around an owned CoreFoundation reference (`CFRelease` on drop).
struct OwnedCfRef(CFTypeRef);

impl OwnedCfRef {
    fn from_non_null(ptr: CFTypeRef) -> Result<Self, MediaError> {
        if ptr.is_null() {
            return Err(MediaError::Codec(
                "CoreFoundation allocation returned null".to_owned(),
            ));
        }
        Ok(Self(ptr))
    }

    const fn as_ptr(&self) -> CFTypeRef {
        self.0
    }
}

impl Drop for OwnedCfRef {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` is an owned non-null CoreFoundation reference created or retained by us.
            unsafe {
                CFRelease(self.0);
            }
        }
    }
}

/// RAII wrapper around an owned mutable CoreFoundation dictionary.
struct OwnedCfMutableDict(CFMutableDictionaryRef);

impl OwnedCfMutableDict {
    fn new(capacity: isize) -> Result<Self, MediaError> {
        // SAFETY: `kCFTypeDictionaryKeyCallBacks` and `kCFTypeDictionaryValueCallBacks` are
        // process-lifetime CoreFoundation static callback structs; null allocator uses default.
        let dict = unsafe {
            CFDictionaryCreateMutable(
                null(),
                capacity,
                &raw const kCFTypeDictionaryKeyCallBacks,
                &raw const kCFTypeDictionaryValueCallBacks,
            )
        };
        if dict.is_null() {
            return Err(MediaError::Codec(
                "CFDictionaryCreateMutable returned null".to_owned(),
            ));
        }
        Ok(Self(dict))
    }

    fn set_value(&mut self, key: CFStringRef, value: CFTypeRef) {
        // SAFETY: `self.0` is a valid mutable CFDictionary and `key`/`value` are valid CF objects
        // retained by CFDictionarySetValue via kCFTypeDictionary callbacks.
        unsafe {
            CFDictionarySetValue(self.0, key, value);
        }
    }

    const fn as_dict(&self) -> CFDictionaryRef {
        self.0.cast_const()
    }
}

impl Drop for OwnedCfMutableDict {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` is an owned CFDictionary created by `CFDictionaryCreateMutable`.
            unsafe {
                CFRelease(self.0.cast_const());
            }
        }
    }
}

fn cf_number_i32(value: i32) -> Result<OwnedCfRef, MediaError> {
    // SAFETY: `&raw const value` points to a valid initialized `i32` matching `K_CF_NUMBER_SINT32_TYPE`.
    let ptr = unsafe { CFNumberCreate(null(), K_CF_NUMBER_SINT32_TYPE, (&raw const value).cast()) };
    OwnedCfRef::from_non_null(ptr)
}

/// Retained `CVPixelBufferRef` released automatically on drop.
struct RetainedPixelBuffer(CVPixelBufferRef);

impl Drop for RetainedPixelBuffer {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` was retained via `CFRetain` inside `vt_decode_callback`.
            unsafe {
                CFRelease(self.0.cast_const());
            }
        }
    }
}

/// RAII lock guard ensuring `CVPixelBufferUnlockBaseAddress` runs even on early return.
struct PixelBufferReadLock<'a>(&'a RetainedPixelBuffer);

impl<'a> PixelBufferReadLock<'a> {
    fn acquire(pb: &'a RetainedPixelBuffer) -> Result<Self, MediaError> {
        // SAFETY: `pb.0` is a retained, non-null `CVPixelBufferRef`.
        let status =
            unsafe { CVPixelBufferLockBaseAddress(pb.0, K_CV_PIXEL_BUFFER_LOCK_READ_ONLY) };
        if status != 0 {
            return Err(MediaError::Codec(format!(
                "CVPixelBufferLockBaseAddress failed: {status}"
            )));
        }
        Ok(Self(pb))
    }

    fn width(&self) -> usize {
        // SAFETY: `self.0.0` is a locked, valid `CVPixelBufferRef`.
        unsafe { CVPixelBufferGetWidth(self.0.0) }
    }

    fn height(&self) -> usize {
        // SAFETY: `self.0.0` is a locked, valid `CVPixelBufferRef`.
        unsafe { CVPixelBufferGetHeight(self.0.0) }
    }

    /// Returns `(bytes, bytes_per_row, plane_width, plane_height)` for `plane_index`, using
    /// `CoreVideo`'s own plane extents (`ceil(h/2)` chroma rows for odd heights).
    fn plane(&self, plane_index: usize) -> Result<(&[u8], usize, usize), MediaError> {
        // SAFETY: `self.0.0` is a locked, valid `CVPixelBufferRef`.
        let row_count = unsafe { CVPixelBufferGetHeightOfPlane(self.0.0, plane_index) };
        // SAFETY: `self.0.0` is a locked, valid `CVPixelBufferRef`.
        let plane_width = unsafe { CVPixelBufferGetWidthOfPlane(self.0.0, plane_index) };
        let (bytes, stride) = self.plane_bytes(plane_index, row_count)?;
        Ok((bytes, stride, plane_width))
    }

    fn plane_bytes(
        &self,
        plane_index: usize,
        row_count: usize,
    ) -> Result<(&[u8], usize), MediaError> {
        // SAFETY: `self.0.0` is locked read-only and valid for plane queries.
        let base = unsafe { CVPixelBufferGetBaseAddressOfPlane(self.0.0, plane_index) };
        if base.is_null() {
            return Err(MediaError::Codec(format!(
                "CVPixelBuffer plane {plane_index} base pointer was null"
            )));
        }
        // SAFETY: `self.0.0` is locked read-only and valid for stride queries.
        let stride = unsafe { CVPixelBufferGetBytesPerRowOfPlane(self.0.0, plane_index) };
        let byte_len = stride.saturating_mul(row_count);
        // SAFETY: The pixel buffer is locked read-only for the lifetime of `&self`, and CoreVideo
        // guarantees `stride * row_count` readable bytes for plane `plane_index`.
        let slice = unsafe { std::slice::from_raw_parts(base, byte_len) };
        Ok((slice, stride))
    }
}

impl Drop for PixelBufferReadLock<'_> {
    fn drop(&mut self) {
        // SAFETY: Balances the successful `CVPixelBufferLockBaseAddress` in `acquire`.
        let _ =
            unsafe { CVPixelBufferUnlockBaseAddress(self.0.0, K_CV_PIXEL_BUFFER_LOCK_READ_ONLY) };
    }
}

/// RAII read-write lock guard for a newly allocated writable `CVPixelBuffer`.
struct PixelBufferWriteLock<'a>(&'a RetainedPixelBuffer);

impl<'a> PixelBufferWriteLock<'a> {
    fn acquire(pb: &'a RetainedPixelBuffer) -> Result<Self, MediaError> {
        // SAFETY: `pb.0` is an exclusively owned non-null `CVPixelBufferRef`.
        let status = unsafe { CVPixelBufferLockBaseAddress(pb.0, 0) };
        if status != 0 {
            return Err(MediaError::Codec(format!(
                "CVPixelBufferLockBaseAddress(rw) failed: {status}"
            )));
        }
        Ok(Self(pb))
    }

    fn plane_mut(&mut self, plane_index: usize) -> Result<(&mut [u8], usize, usize), MediaError> {
        // SAFETY: `self.0.0` is locked read-write and exclusively owned.
        let row_count = unsafe { CVPixelBufferGetHeightOfPlane(self.0.0, plane_index) };
        // SAFETY: `self.0.0` is locked read-write and exclusively owned.
        let stride = unsafe { CVPixelBufferGetBytesPerRowOfPlane(self.0.0, plane_index) };
        // SAFETY: `self.0.0` is locked read-write and exclusively owned.
        let base = unsafe { CVPixelBufferGetBaseAddressOfPlane(self.0.0, plane_index) }.cast_mut();
        if base.is_null() {
            return Err(MediaError::Codec(format!(
                "CVPixelBuffer writable plane {plane_index} base pointer was null"
            )));
        }
        let byte_len = stride.saturating_mul(row_count);
        // SAFETY: Exclusively locked writable pixel buffer guarantees `stride * row_count` bytes.
        let slice = unsafe { std::slice::from_raw_parts_mut(base, byte_len) };
        Ok((slice, stride, row_count))
    }
}

impl Drop for PixelBufferWriteLock<'_> {
    fn drop(&mut self) {
        // SAFETY: Balances `CVPixelBufferLockBaseAddress(self.0.0, 0)` in `acquire`.
        let _ = unsafe { CVPixelBufferUnlockBaseAddress(self.0.0, 0) };
    }
}

impl RetainedPixelBuffer {
    /// Allocates a fresh writable NV12 (`'420v'`) `CVPixelBuffer`, copies `self` into it,
    /// and invokes `painter` on the mutable Y and interleaved UV planes.
    fn clone_writable_and_paint(
        &self,
        painter: &mut dyn FnMut(&mut MutableYuvFrame<'_>),
    ) -> Result<Self, MediaError> {
        let src_lock = PixelBufferReadLock::acquire(self)?;
        let width = src_lock.width();
        let height = src_lock.height();
        let (src_y, src_y_stride, _) = src_lock.plane(0)?;
        let (src_uv, src_uv_stride, _) = src_lock.plane(1)?;

        let mut raw_dst: CVPixelBufferRef = null_mut();
        // SAFETY: `CVPixelBufferCreate` allocates a new CoreVideo pixel buffer owned by us
        // (released by `RetainedPixelBuffer::drop` via `CFRelease`).
        let st = unsafe {
            CVPixelBufferCreate(
                null(),
                width,
                height,
                K_CV_PIXEL_FORMAT_420V_U32,
                null(),
                &raw mut raw_dst,
            )
        };
        if st != 0 || raw_dst.is_null() {
            return Err(MediaError::Codec(format!(
                "CVPixelBufferCreate(420v) failed: {st}"
            )));
        }
        let dst = Self(raw_dst);
        {
            let mut dst_lock = PixelBufferWriteLock::acquire(&dst)?;
            let (dst_y_ptr, dst_y_stride, dst_y_rows) = {
                let (slice, stride, rows) = dst_lock.plane_mut(0)?;
                let copy_rows = rows.min(height);
                for row in 0..copy_rows {
                    let s_off = row * src_y_stride;
                    let d_off = row * stride;
                    let n = width.min(src_y.len().saturating_sub(s_off));
                    if d_off + n <= slice.len() {
                        slice[d_off..d_off + n].copy_from_slice(&src_y[s_off..s_off + n]);
                    }
                }
                (slice.as_mut_ptr(), stride, rows)
            };
            let (dst_uv_ptr, dst_uv_stride, dst_uv_rows) = {
                let (slice, stride, rows) = dst_lock.plane_mut(1)?;
                let chroma_h = height.div_ceil(2);
                let chroma_row_bytes = width.div_ceil(2).saturating_mul(2);
                let copy_rows = rows.min(chroma_h);
                for row in 0..copy_rows {
                    let s_off = row * src_uv_stride;
                    let d_off = row * stride;
                    let n = chroma_row_bytes.min(src_uv.len().saturating_sub(s_off));
                    if d_off + n <= slice.len() {
                        slice[d_off..d_off + n].copy_from_slice(&src_uv[s_off..s_off + n]);
                    }
                }
                (slice.as_mut_ptr(), stride, rows)
            };
            // SAFETY: Plane 0 (Y) of a bi-planar NV12 `CVPixelBuffer` is locked exclusively by `dst_lock`.
            let y_mut =
                unsafe { std::slice::from_raw_parts_mut(dst_y_ptr, dst_y_stride * dst_y_rows) };
            // SAFETY: Plane 1 (UV) of a bi-planar NV12 `CVPixelBuffer` is disjoint from plane 0 and locked exclusively by `dst_lock`.
            let uv_mut =
                unsafe { std::slice::from_raw_parts_mut(dst_uv_ptr, dst_uv_stride * dst_uv_rows) };
            let mut frame = MutableYuvFrame::Nv12 {
                width,
                height,
                y: y_mut,
                uv: uv_mut,
                y_stride: dst_y_stride,
                uv_stride: dst_uv_stride,
            };
            painter(&mut frame);
        }
        Ok(dst)
    }

    fn to_decoded_frame(
        &self,
        frame_index: u64,
        output_format: PixelFormat,
    ) -> Result<DecodedFrame, MediaError> {
        let lock = PixelBufferReadLock::acquire(self)?;
        let width = lock.width();
        let height = lock.height();
        let (y_slice, y_stride, _) = lock.plane(0)?;
        let (uv_slice, uv_stride, _) = lock.plane(1)?;
        let w_u32 = u32::try_from(width)
            .map_err(|_| MediaError::Codec("pixel buffer width overflows u32".to_owned()))?;
        let h_u32 = u32::try_from(height)
            .map_err(|_| MediaError::Codec("pixel buffer height overflows u32".to_owned()))?;

        match output_format {
            PixelFormat::Yuv420p => {
                let yuv = nv12_to_yuv420p(y_slice, uv_slice, y_stride, uv_stride, width, height);
                Ok(DecodedFrame {
                    frame_index,
                    width: w_u32,
                    height: h_u32,
                    format: PixelFormat::Yuv420p,
                    data: yuv,
                })
            }
            PixelFormat::Rgb24 => {
                let mut rgb = vec![0u8; width.saturating_mul(height).saturating_mul(3)];
                nv12_to_rgb24(
                    y_slice, uv_slice, y_stride, uv_stride, width, height, &mut rgb,
                );
                Ok(DecodedFrame {
                    frame_index,
                    width: w_u32,
                    height: h_u32,
                    format: PixelFormat::Rgb24,
                    data: rgb,
                })
            }
            PixelFormat::Rgba8 | PixelFormat::Bgra8 => {
                let yuv = nv12_to_yuv420p(y_slice, uv_slice, y_stride, uv_stride, width, height);
                convert_from_yuv420p(frame_index, w_u32, h_u32, yuv, output_format)
            }
        }
    }

    fn to_thumb_rgb24(&self, dst_w: usize, dst_h: usize) -> Result<RgbImage, MediaError> {
        let lock = PixelBufferReadLock::acquire(self)?;
        let width = lock.width();
        let height = lock.height();
        let (y_slice, y_stride, _) = lock.plane(0)?;
        let (uv_slice, uv_stride, _) = lock.plane(1)?;
        let data = nv12_to_thumb_rgb24(
            y_slice, uv_slice, y_stride, uv_stride, width, height, dst_w, dst_h,
        );
        let w_u32 = u32::try_from(dst_w)
            .map_err(|_| MediaError::Codec("thumb width overflows u32".to_owned()))?;
        let h_u32 = u32::try_from(dst_h)
            .map_err(|_| MediaError::Codec("thumb height overflows u32".to_owned()))?;
        Ok(RgbImage {
            width: w_u32,
            height: h_u32,
            data,
        })
    }
}

struct DecodeState {
    last_buf: Option<RetainedPixelBuffer>,
    last_status: OSStatus,
}

extern "C" fn vt_decode_callback(
    refcon: *mut c_void,
    _source_refcon: *mut c_void,
    status: OSStatus,
    info_flags: u32,
    image_buffer: CVImageBufferRef,
    _pts: CMTime,
    _duration: CMTime,
) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if refcon.is_null() {
            return;
        }
        // SAFETY: `refcon` is the `Box::into_raw` allocation owned by `VtDecoder::state`, which
        // is freed only after the session is invalidated. `VtDecoder` never holds a reference
        // into it while `VTDecompressionSessionDecodeFrame` (and thus this callback) runs.
        let state = unsafe { &mut *refcon.cast::<DecodeState>() };
        state.last_status = status;
        if status == 0
            && (info_flags & K_VT_DECODE_INFO_FRAME_DROPPED) == 0
            && !image_buffer.is_null()
        {
            // SAFETY: `image_buffer` is a valid non-null `CVImageBufferRef` provided by VideoToolbox;
            // retaining transfers an owned reference to `RetainedPixelBuffer`.
            let _ = unsafe { CFRetain(image_buffer.cast_const()) };
            state.last_buf = Some(RetainedPixelBuffer(image_buffer));
        }
    }));
}

/// NAL length prefix size the decoder's `CMFormatDescription` is always created with.
/// Every source sample is normalised to 4-byte lengths before decoding (R2 S2).
const DECODER_NAL_LENGTH_SIZE: usize = 4;

struct VtDecoder {
    fmt_desc: OwnedCfRef,
    session: VTDecompressionSessionRef,
    /// Callback state from `Box::into_raw`: never accessed through a `Box` (whose `noalias`
    /// retags would invalidate the pointer `VideoToolbox` holds); freed in `Drop` strictly after
    /// `VTDecompressionSessionInvalidate`.
    state: NonNull<DecodeState>,
    active_sps: Vec<Vec<u8>>,
    active_pps: Vec<Vec<u8>>,
    /// NAL length prefix size of the *source* samples (`avcC lengthSizeMinusOne + 1`).
    src_nal_length_size: usize,
    width: u16,
    height: u16,
}

impl Drop for VtDecoder {
    fn drop(&mut self) {
        if !self.session.is_null() {
            // SAFETY: `self.session` is a valid decompression session created by VideoToolbox.
            unsafe {
                VTDecompressionSessionInvalidate(self.session);
            }
            // SAFETY: Balances the ownership obtained from `VTDecompressionSessionCreate`.
            unsafe {
                CFRelease(self.session.cast_const());
            }
        }
        // SAFETY: `state` came from `Box::into_raw` in `VtDecoder::new` and is freed exactly
        // once, here, after the session was invalidated so no callback can still use it.
        drop(unsafe { Box::from_raw(self.state.as_ptr()) });
    }
}

impl VtDecoder {
    fn new(
        sps: &[Vec<u8>],
        pps: &[Vec<u8>],
        src_nal_length_size: usize,
        width: u16,
        height: u16,
    ) -> Result<Self, MediaError> {
        if !(1..=4).contains(&src_nal_length_size) || src_nal_length_size == 3 {
            return Err(MediaError::UnsupportedFeature(format!(
                "unsupported avcC NAL length size {src_nal_length_size} (expected 1, 2 or 4)"
            )));
        }
        let mut ptrs: Vec<*const u8> = Vec::with_capacity(sps.len() + pps.len());
        let mut sizes: Vec<usize> = Vec::with_capacity(sps.len() + pps.len());
        for s in sps {
            ptrs.push(s.as_ptr());
            sizes.push(s.len());
        }
        for p in pps {
            ptrs.push(p.as_ptr());
            sizes.push(p.len());
        }
        if ptrs.is_empty() {
            return Err(MediaError::InvalidContainer(
                "H.264 parameter sets are empty".to_owned(),
            ));
        }

        let nalu_hdr_len = i32::try_from(DECODER_NAL_LENGTH_SIZE).map_err(|_| {
            MediaError::InvalidContainer("NAL length size overflows i32".to_owned())
        })?;
        let mut fmt_desc_raw: CMFormatDescriptionRef = null();
        // SAFETY: `ptrs` and `sizes` have equal length and point to valid SPS/PPS slices for
        // the duration of this call; `fmt_desc_raw` receives an owned CoreMedia reference.
        let st = unsafe {
            CMVideoFormatDescriptionCreateFromH264ParameterSets(
                null(),
                ptrs.len(),
                ptrs.as_ptr(),
                sizes.as_ptr(),
                nalu_hdr_len,
                &raw mut fmt_desc_raw,
            )
        };
        if st != 0 {
            return Err(MediaError::Codec(format!(
                "CMVideoFormatDescriptionCreateFromH264ParameterSets failed: {st}"
            )));
        }
        let fmt_desc = OwnedCfRef::from_non_null(fmt_desc_raw)?;

        let mut dst_attrs = OwnedCfMutableDict::new(3)?;
        let num_fmt = cf_number_i32(K_CV_PIXEL_FORMAT_420V)?;
        let num_w = cf_number_i32(i32::from(width))?;
        let num_h = cf_number_i32(i32::from(height))?;
        // SAFETY: `kCVPixelBufferPixelFormatTypeKey` is a CoreVideo process-lifetime constant.
        let fmt_key = unsafe { kCVPixelBufferPixelFormatTypeKey };
        // SAFETY: `kCVPixelBufferWidthKey` is a CoreVideo process-lifetime constant.
        let w_key = unsafe { kCVPixelBufferWidthKey };
        // SAFETY: `kCVPixelBufferHeightKey` is a CoreVideo process-lifetime constant.
        let h_key = unsafe { kCVPixelBufferHeightKey };
        dst_attrs.set_value(fmt_key, num_fmt.as_ptr());
        dst_attrs.set_value(w_key, num_w.as_ptr());
        dst_attrs.set_value(h_key, num_h.as_ptr());

        let state = NonNull::from(Box::leak(Box::new(DecodeState {
            last_buf: None,
            last_status: 0,
        })));
        let cb_rec = VTDecompressionOutputCallbackRecord {
            decompression_output_callback: vt_decode_callback,
            decompression_output_ref_con: state.as_ptr().cast(),
        };

        let mut session: VTDecompressionSessionRef = null_mut();
        // SAFETY: `fmt_desc`, `dst_attrs`, and `cb_rec` are valid; `state` is a raw heap
        // allocation freed only after the session is invalidated (`Drop`) or below on failure.
        let st = unsafe {
            VTDecompressionSessionCreate(
                null(),
                fmt_desc.as_ptr(),
                null(),
                dst_attrs.as_dict(),
                &raw const cb_rec,
                &raw mut session,
            )
        };
        if st != 0 || session.is_null() {
            if !session.is_null() {
                // SAFETY: a non-null session returned alongside an error is still owned by us.
                unsafe { VTDecompressionSessionInvalidate(session) };
                // SAFETY: Balances the ownership obtained from `VTDecompressionSessionCreate`.
                unsafe { CFRelease(session.cast_const()) };
            }
            // SAFETY: no session references `state` any more; reclaim the leaked allocation.
            drop(unsafe { Box::from_raw(state.as_ptr()) });
            return Err(MediaError::Codec(format!(
                "VTDecompressionSessionCreate failed: {st}"
            )));
        }

        Ok(Self {
            fmt_desc,
            session,
            state,
            active_sps: sps.to_vec(),
            active_pps: pps.to_vec(),
            src_nal_length_size,
            width,
            height,
        })
    }

    /// Exclusive access to the callback state while no decode call is in flight.
    fn state_mut(&mut self) -> &mut DecodeState {
        // SAFETY: `state` is a live allocation owned by `self`; VideoToolbox only touches it
        // inside the synchronous `VTDecompressionSessionDecodeFrame`, during which this
        // reference is never held (`&mut self` is not re-borrowed across that call).
        unsafe { &mut *self.state.as_ptr() }
    }

    /// Ensures the active `CMFormatDescription` matches any in-band SPS/PPS carried by `sample`
    /// (enabling seamless decoding across single-`stsd` `avc1` + in-band SPS/PPS splice boundaries)
    /// and decodes the sample synchronously into a hardware `CVPixelBuffer`.
    fn decode_avcc_sample_sync(
        &mut self,
        avcc_sample: &[u8],
        pts_ticks: i64,
        timescale: u32,
        duration_ticks: u32,
    ) -> Result<RetainedPixelBuffer, MediaError> {
        // Always normalise to 4-byte NAL lengths (the decoder is created with 4), whatever the
        // source `lengthSizeMinusOne` and whether or not the sample carries SPS/PPS (R2 S2).
        let (seq_sets, pic_sets, stripped) =
            extract_inband_param_sets_and_strip(avcc_sample, self.src_nal_length_size);
        if !seq_sets.is_empty()
            && !pic_sets.is_empty()
            && (seq_sets != self.active_sps || pic_sets != self.active_pps)
        {
            *self = Self::new(
                &seq_sets,
                &pic_sets,
                self.src_nal_length_size,
                self.width,
                self.height,
            )?;
        }

        self.decode_vcl_sample_sync(&stripped, pts_ticks, timescale, duration_ticks)
    }

    fn decode_vcl_sample_sync(
        &mut self,
        vcl_sample: &[u8],
        pts_ticks: i64,
        timescale: u32,
        duration_ticks: u32,
    ) -> Result<RetainedPixelBuffer, MediaError> {
        if vcl_sample.is_empty() {
            return Err(MediaError::Codec(
                "cannot decode empty AVCC VCL sample".to_owned(),
            ));
        }
        {
            let state = self.state_mut();
            state.last_buf = None;
            state.last_status = 0;
        }

        // SAFETY: `kCFAllocatorNull` is a CoreFoundation process-lifetime constant.
        let null_alloc = unsafe { kCFAllocatorNull };
        let mut block_buf_raw: CMBlockBufferRef = null_mut();
        // SAFETY: `vcl_sample` lives for the entire synchronous decode call; `kCFAllocatorNull`
        // prevents CoreMedia from freeing our borrowed slice, and `CMBlockBufferCreateWithMemoryBlock`
        // only reads from `memory_block`.
        let st = unsafe {
            CMBlockBufferCreateWithMemoryBlock(
                null(),
                vcl_sample.as_ptr().cast_mut().cast(),
                vcl_sample.len(),
                null_alloc,
                null(),
                0,
                vcl_sample.len(),
                0,
                &raw mut block_buf_raw,
            )
        };
        if st != 0 || block_buf_raw.is_null() {
            return Err(MediaError::Codec(format!(
                "CMBlockBufferCreateWithMemoryBlock failed: {st}"
            )));
        }
        let block_buf = OwnedCfRef::from_non_null(block_buf_raw.cast_const())?;

        let ts_i32 = i32::try_from(timescale).unwrap_or(90_000);
        let timing = CMSampleTimingInfo {
            duration: cm_time_make(i64::from(duration_ticks), ts_i32),
            presentation_time_stamp: cm_time_make(pts_ticks, ts_i32),
            decode_time_stamp: K_CM_TIME_INVALID,
        };
        let sample_size = vcl_sample.len();
        let mut sbuf_raw: CMSampleBufferRef = null_mut();
        // SAFETY: `block_buf_raw`, `self.fmt_desc`, `timing`, and `sample_size` are valid.
        let st = unsafe {
            CMSampleBufferCreateReady(
                null(),
                block_buf_raw,
                self.fmt_desc.as_ptr(),
                1,
                1,
                &raw const timing,
                1,
                &raw const sample_size,
                &raw mut sbuf_raw,
            )
        };
        drop(block_buf);
        if st != 0 || sbuf_raw.is_null() {
            return Err(MediaError::Codec(format!(
                "CMSampleBufferCreateReady failed: {st}"
            )));
        }
        let sbuf = OwnedCfRef::from_non_null(sbuf_raw.cast_const())?;

        let mut info_flags: u32 = 0;
        // SAFETY: `decode_flags = 0` runs synchronously so `vt_decode_callback` completes before
        // `VTDecompressionSessionDecodeFrame` returns and before `vcl_sample` / `sbuf` drop.
        let st = unsafe {
            VTDecompressionSessionDecodeFrame(
                self.session,
                sbuf_raw,
                0,
                null_mut(),
                &raw mut info_flags,
            )
        };
        drop(sbuf);
        if st != 0 {
            return Err(MediaError::Codec(format!(
                "VTDecompressionSessionDecodeFrame failed: {st}"
            )));
        }
        let state = self.state_mut();
        if state.last_status != 0 {
            return Err(MediaError::Codec(format!(
                "VideoToolbox decode callback error: {}",
                state.last_status
            )));
        }
        state.last_buf.take().ok_or_else(|| {
            MediaError::Codec("VideoToolbox decode produced no pixel buffer".to_owned())
        })
    }
}

struct EncodeState {
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    samples: Vec<(Vec<u8>, bool)>,
    error: Option<OSStatus>,
}

fn vt_sample_is_keyframe(sample_buffer: CMSampleBufferRef) -> bool {
    // SAFETY: `sample_buffer` is a valid `CMSampleBufferRef` supplied by VideoToolbox.
    let attachments = unsafe { CMSampleBufferGetSampleAttachmentsArray(sample_buffer, 0) };
    if attachments.is_null() {
        return true;
    }
    // SAFETY: `attachments` was verified non-null.
    let count = unsafe { CFArrayGetCount(attachments) };
    if count <= 0 {
        return true;
    }
    // SAFETY: Index `0` is within `0..count`.
    let dict = unsafe { CFArrayGetValueAtIndex(attachments, 0) };
    if dict.is_null() {
        return true;
    }
    // SAFETY: `kCMSampleAttachmentKey_NotSync` is a CoreMedia process-lifetime constant.
    let key = unsafe { kCMSampleAttachmentKey_NotSync };
    // SAFETY: `dict` is a valid CFDictionary from the sample attachments array.
    let not_sync = unsafe { CFDictionaryGetValue(dict, key) };
    // SAFETY: `kCFBooleanTrue` is a CoreFoundation process-lifetime constant.
    let cf_true = unsafe { kCFBooleanTrue };
    not_sync != cf_true
}

fn vt_extract_param_set(fdesc: CMVideoFormatDescriptionRef, index: usize) -> Option<Vec<u8>> {
    let mut ptr: *const u8 = null();
    let mut len: usize = 0;
    let mut count: usize = 0;
    let mut nalu_hdr_len: i32 = 0;
    // SAFETY: `fdesc` is a valid format description from `CMSampleBufferGetFormatDescription`.
    let st = unsafe {
        CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            fdesc,
            index,
            &raw mut ptr,
            &raw mut len,
            &raw mut count,
            &raw mut nalu_hdr_len,
        )
    };
    if st != 0 || ptr.is_null() || len == 0 {
        return None;
    }
    // SAFETY: CoreMedia guarantees `ptr` points to `len` valid bytes while `fdesc` is alive.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    Some(bytes.to_vec())
}

extern "C" fn vt_encode_callback(
    refcon: *mut c_void,
    _source_refcon: *mut c_void,
    status: OSStatus,
    _info_flags: u32,
    sample_buffer: CMSampleBufferRef,
) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if refcon.is_null() {
            return;
        }
        // SAFETY: `refcon` is the `Box::into_raw` allocation owned by `VtEncoder::state`, freed
        // only after `CompleteFrames` + `Invalidate`; `VtEncoder` holds no reference into it
        // while VideoToolbox may run this callback.
        let state = unsafe { &mut *refcon.cast::<EncodeState>() };
        if status != 0 {
            state.error = Some(status);
            return;
        }
        if sample_buffer.is_null() {
            return;
        }

        let is_key = vt_sample_is_keyframe(sample_buffer);

        if is_key && (state.sps.is_none() || state.pps.is_none()) {
            // SAFETY: `sample_buffer` is non-null and valid inside the callback.
            let fdesc = unsafe { CMSampleBufferGetFormatDescription(sample_buffer) };
            if !fdesc.is_null() {
                if state.sps.is_none() {
                    state.sps = vt_extract_param_set(fdesc, 0);
                }
                if state.pps.is_none() {
                    state.pps = vt_extract_param_set(fdesc, 1);
                }
            }
        }

        // SAFETY: `sample_buffer` is non-null and valid inside the callback.
        let data_buf = unsafe { CMSampleBufferGetDataBuffer(sample_buffer) };
        if !data_buf.is_null() {
            // SAFETY: `data_buf` was verified non-null.
            let len = unsafe { CMBlockBufferGetDataLength(data_buf) };
            let mut bytes = vec![0u8; len];
            // SAFETY: `bytes` has exact length `len` matching `CMBlockBufferGetDataLength`.
            let copy_st =
                unsafe { CMBlockBufferCopyDataBytes(data_buf, 0, len, bytes.as_mut_ptr().cast()) };
            if copy_st == 0 {
                state.samples.push((bytes, is_key));
            } else {
                state.error = Some(copy_st);
            }
        }
    }));
}

struct VtEncoder {
    session: VTCompressionSessionRef,
    /// Callback state from `Box::into_raw` (see [`VtDecoder::state`]); freed in `Drop` strictly
    /// after `VTCompressionSessionCompleteFrames` + `VTCompressionSessionInvalidate`.
    state: NonNull<EncodeState>,
}

impl Drop for VtEncoder {
    fn drop(&mut self) {
        if !self.session.is_null() {
            // SAFETY: `self.session` is valid; draining every pending frame first guarantees no
            // encode callback is still in flight when the session is torn down (R2 C16).
            let _ = unsafe { VTCompressionSessionCompleteFrames(self.session, K_CM_TIME_INVALID) };
            // SAFETY: `self.session` is a valid compression session created by VideoToolbox.
            unsafe {
                VTCompressionSessionInvalidate(self.session);
            }
            // SAFETY: Balances ownership obtained from `VTCompressionSessionCreate`.
            unsafe {
                CFRelease(self.session.cast_const());
            }
        }
        // SAFETY: `state` came from `Box::into_raw` in `VtEncoder::new` and is freed exactly
        // once, here, after the session was completed and invalidated.
        drop(unsafe { Box::from_raw(self.state.as_ptr()) });
    }
}

impl VtEncoder {
    fn new(width: u16, height: u16, bitrate_bps: u32) -> Result<Self, MediaError> {
        let state = NonNull::from(Box::leak(Box::new(EncodeState {
            sps: None,
            pps: None,
            samples: Vec::new(),
            error: None,
        })));
        let mut session: VTCompressionSessionRef = null_mut();
        // SAFETY: `state` is a raw heap allocation freed only after the session is completed
        // and invalidated (`Drop`) or below on failure.
        let st = unsafe {
            VTCompressionSessionCreate(
                null(),
                i32::from(width),
                i32::from(height),
                K_CM_VIDEO_CODEC_H264,
                null(),
                null(),
                null(),
                vt_encode_callback,
                state.as_ptr().cast(),
                &raw mut session,
            )
        };
        if st != 0 || session.is_null() {
            if !session.is_null() {
                // SAFETY: a non-null session returned alongside an error is still owned by us.
                unsafe { VTCompressionSessionInvalidate(session) };
                // SAFETY: Balances ownership obtained from `VTCompressionSessionCreate`.
                unsafe { CFRelease(session.cast_const()) };
            }
            // SAFETY: no session references `state` any more; reclaim the leaked allocation.
            drop(unsafe { Box::from_raw(state.as_ptr()) });
            return Err(MediaError::Codec(format!(
                "VTCompressionSessionCreate failed: {st}"
            )));
        }

        let encoder = Self { session, state };

        // SAFETY: CoreFoundation / VideoToolbox process-lifetime static keys/booleans.
        let key_reorder = unsafe { kVTCompressionPropertyKey_AllowFrameReordering };
        // SAFETY: CoreFoundation process-lifetime boolean constant.
        let cf_false = unsafe { kCFBooleanFalse };
        // SAFETY: `encoder.session` is valid and owned by `encoder`.
        let _ = unsafe { VTSessionSetProperty(encoder.session, key_reorder, cf_false) };

        // SAFETY: VideoToolbox process-lifetime property key constant.
        let key_realtime = unsafe { kVTCompressionPropertyKey_RealTime };
        // SAFETY: `encoder.session` is valid and owned by `encoder`.
        let _ = unsafe { VTSessionSetProperty(encoder.session, key_realtime, cf_false) };

        // SAFETY: VideoToolbox process-lifetime property key constant.
        let key_profile = unsafe { kVTCompressionPropertyKey_ProfileLevel };
        // SAFETY: VideoToolbox process-lifetime profile constant.
        let val_profile = unsafe { kVTProfileLevel_H264_High_AutoLevel };
        // SAFETY: `encoder.session` is valid and owned by `encoder`.
        let _ = unsafe { VTSessionSetProperty(encoder.session, key_profile, val_profile) };

        let br_i32 = i32::try_from(bitrate_bps).unwrap_or(i32::MAX);
        let num_br = cf_number_i32(br_i32)?;
        // SAFETY: VideoToolbox process-lifetime property key constant.
        let key_bitrate = unsafe { kVTCompressionPropertyKey_AverageBitRate };
        // SAFETY: `encoder.session` and `num_br` are valid.
        let _ = unsafe { VTSessionSetProperty(encoder.session, key_bitrate, num_br.as_ptr()) };

        // SAFETY: `encoder.session` is valid and configured.
        let st = unsafe { VTCompressionSessionPrepareToEncodeFrames(encoder.session) };
        if st != 0 {
            return Err(MediaError::Codec(format!(
                "VTCompressionSessionPrepareToEncodeFrames failed: {st}"
            )));
        }

        Ok(encoder)
    }

    fn encode_pixel_buffer(
        &mut self,
        pb: &RetainedPixelBuffer,
        pts: i64,
        timescale: u32,
        duration: u32,
        mode: FrameEncodeMode,
    ) -> Result<(), MediaError> {
        let props_guard = match mode {
            FrameEncodeMode::ForceKeyframe => {
                let mut dict = OwnedCfMutableDict::new(1)?;
                // SAFETY: VideoToolbox process-lifetime frame option key constant.
                let key_force = unsafe { kVTEncodeFrameOptionKey_ForceKeyFrame };
                // SAFETY: CoreFoundation process-lifetime boolean constant.
                let cf_true = unsafe { kCFBooleanTrue };
                dict.set_value(key_force, cf_true);
                Some(dict)
            }
            FrameEncodeMode::Normal => None,
        };
        let props_ptr = props_guard
            .as_ref()
            .map_or(null(), OwnedCfMutableDict::as_dict);
        let ts_i32 = i32::try_from(timescale).unwrap_or(90_000);
        let mut info_flags: u32 = 0;

        // SAFETY: `self.session` and `pb.0` are valid; `props_guard` remains alive across the call.
        let st = unsafe {
            VTCompressionSessionEncodeFrame(
                self.session,
                pb.0,
                cm_time_make(pts, ts_i32),
                cm_time_make(i64::from(duration), ts_i32),
                props_ptr,
                null_mut(),
                &raw mut info_flags,
            )
        };
        if st != 0 {
            return Err(MediaError::Codec(format!(
                "VTCompressionSessionEncodeFrame failed: {st}"
            )));
        }
        Ok(())
    }

    fn finish(self) -> Result<VtEncodedStream, MediaError> {
        // SAFETY: `self.session` is valid; `K_CM_TIME_INVALID` flushes all pending frames synchronously.
        let st = unsafe { VTCompressionSessionCompleteFrames(self.session, K_CM_TIME_INVALID) };
        if st != 0 {
            return Err(MediaError::Codec(format!(
                "VTCompressionSessionCompleteFrames failed: {st}"
            )));
        }
        // SAFETY: `CompleteFrames` returned, so every callback has finished; the allocation is
        // live and owned by `self`, and no other reference into it exists.
        let state = unsafe { &mut *self.state.as_ptr() };
        if let Some(err) = state.error {
            return Err(MediaError::Codec(format!(
                "VideoToolbox encode callback error: {err}"
            )));
        }
        let sps = state
            .sps
            .take()
            .ok_or_else(|| MediaError::Codec("VideoToolbox encoder emitted no SPS".to_owned()))?;
        let pps = state
            .pps
            .take()
            .ok_or_else(|| MediaError::Codec("VideoToolbox encoder emitted no PPS".to_owned()))?;
        let samples = std::mem::take(&mut state.samples);
        Ok(VtEncodedStream { sps, pps, samples })
    }
}

#[derive(Clone, Copy)]
enum FrameEncodeMode {
    Normal,
    ForceKeyframe,
}

struct VtEncodedStream {
    sps: Vec<u8>,
    pps: Vec<u8>,
    samples: Vec<(Vec<u8>, bool)>,
}

/// macOS Apple `VideoToolbox` hardware H.264 codec backend (direct C FFI, zero external crates).
#[derive(Debug, Clone, Copy)]
pub struct VideoToolboxBackend;

impl VideoToolboxBackend {
    /// Probes Apple `VideoToolbox` hardware encoder initialization and returns a ready backend.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::Codec`] if `VideoToolbox` cannot initialize an H.264 session on this host.
    pub fn try_new() -> Result<Self, MediaError> {
        // Lightweight probe verifying VideoToolbox H.264 compression session creation succeeds.
        let probe = VtEncoder::new(64, 64, 500_000)?;
        drop(probe);
        Ok(Self)
    }
}

impl VideoCodecBackend for VideoToolboxBackend {
    fn name(&self) -> &'static str {
        "videotoolbox"
    }

    fn decode_frame(
        &self,
        track: &Mp4VideoTrack,
        frame_index: u64,
        output_format: PixelFormat,
    ) -> Result<DecodedFrame, MediaError> {
        let gop_start = track.decodable_gop_start(frame_index)?;
        let avcc = track.avcc();
        let mut dec = VtDecoder::new(
            &avcc.sps,
            &avcc.pps,
            avcc.nal_length_size,
            track.width(),
            track.height(),
        )?;
        let mut raw = Vec::new();

        for idx in gop_start..=frame_index {
            track.read_sample_into(idx, &mut raw)?;
            let info = track.frame(idx)?;
            let pb = dec.decode_avcc_sample_sync(
                &raw,
                info.pts_ticks,
                track.timescale(),
                info.duration_ticks,
            )?;
            if idx == frame_index {
                return pb.to_decoded_frame(frame_index, output_format);
            }
        }
        Err(MediaError::Codec(format!(
            "VideoToolbox did not emit target frame {frame_index}"
        )))
    }

    fn decode_gop(
        &self,
        track: &Mp4VideoTrack,
        gop_id: u64,
        output_format: PixelFormat,
    ) -> Result<Vec<DecodedFrame>, MediaError> {
        let (gop_start, gop_end) = track.decodable_gop_range(gop_id)?;
        let avcc = track.avcc();
        let mut dec = VtDecoder::new(
            &avcc.sps,
            &avcc.pps,
            avcc.nal_length_size,
            track.width(),
            track.height(),
        )?;
        let mut raw = Vec::new();
        let capacity = usize::try_from(gop_end - gop_start + 1).unwrap_or(0);
        let mut out = Vec::with_capacity(capacity);

        for idx in gop_start..=gop_end {
            track.read_sample_into(idx, &mut raw)?;
            let info = track.frame(idx)?;
            let pb = dec.decode_avcc_sample_sync(
                &raw,
                info.pts_ticks,
                track.timescale(),
                info.duration_ticks,
            )?;
            out.push(pb.to_decoded_frame(idx, output_format)?);
        }
        Ok(out)
    }

    fn decode_keyframes_thumbnails(
        &self,
        track: &Mp4VideoTrack,
        max_width: u32,
        max_height: u32,
        max_threads: usize,
    ) -> Result<Vec<(u64, RgbImage)>, MediaError> {
        if max_width == 0 || max_height == 0 {
            return Err(MediaError::InvalidArgument(
                "thumbnail max_width and max_height must be non-zero".to_owned(),
            ));
        }
        let keyframes = track.keyframes();
        if keyframes.is_empty() {
            return Ok(Vec::new());
        }
        let (thumb_width, thumb_height) = fit_thumbnail_dimensions(
            u32::from(track.width()),
            u32::from(track.height()),
            max_width,
            max_height,
        );
        let dst_w = usize::try_from(thumb_width)
            .map_err(|_| MediaError::Codec("thumbnail width overflow".to_owned()))?;
        let dst_h = usize::try_from(thumb_height)
            .map_err(|_| MediaError::Codec("thumbnail height overflow".to_owned()))?;

        // Cap concurrent VideoToolbox sessions (S1 showed 6 workers saturate Apple Silicon HW cleanly).
        let workers = max_threads.clamp(1, 6).min(keyframes.len());
        let chunk_size = keyframes.len().div_ceil(workers);

        let mut results: Vec<(u64, RgbImage)> = std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(workers);
            for chunk in keyframes.chunks(chunk_size) {
                handles.push(
                    scope.spawn(move || -> Result<Vec<(u64, RgbImage)>, MediaError> {
                        let avcc = track.avcc();
                        let mut dec = VtDecoder::new(
                            &avcc.sps,
                            &avcc.pps,
                            avcc.nal_length_size,
                            track.width(),
                            track.height(),
                        )?;
                        let mut raw = Vec::new();
                        let mut local = Vec::with_capacity(chunk.len());
                        for &kf_idx in chunk {
                            track.read_sample_into(kf_idx, &mut raw)?;
                            let info = track.frame(kf_idx)?;
                            let pb = dec.decode_avcc_sample_sync(
                                &raw,
                                info.pts_ticks,
                                track.timescale(),
                                info.duration_ticks,
                            )?;
                            let img = pb.to_thumb_rgb24(dst_w, dst_h)?;
                            local.push((kf_idx, img));
                        }
                        Ok(local)
                    }),
                );
            }
            let mut merged = Vec::with_capacity(keyframes.len());
            for handle in handles {
                let batch = handle.join().map_err(|_| {
                    MediaError::Codec("VideoToolbox filmstrip worker panicked".to_owned())
                })??;
                merged.extend(batch);
            }
            Ok::<Vec<(u64, RgbImage)>, MediaError>(merged)
        })?;

        results.sort_unstable_by_key(|(kf_idx, _)| *kf_idx);
        Ok(results)
    }

    fn reencode_frame_range(
        &self,
        track: &Mp4VideoTrack,
        start_frame: u64,
        end_frame_exclusive: u64,
        params: EncodeParams,
    ) -> Result<EncodedGop, MediaError> {
        let frame_count = track.frame_count();
        if start_frame >= end_frame_exclusive || end_frame_exclusive > frame_count {
            return Err(MediaError::InvalidFrameRange {
                in_frame_index: start_frame,
                out_frame_index: end_frame_exclusive.saturating_sub(1),
                frame_count,
            });
        }

        let gop_start = track.decodable_gop_start(start_frame)?;
        let avcc = track.avcc();
        let mut dec = VtDecoder::new(
            &avcc.sps,
            &avcc.pps,
            avcc.nal_length_size,
            track.width(),
            track.height(),
        )?;
        let mut enc = VtEncoder::new(track.width(), track.height(), params.bitrate_bps)?;
        let mut raw = Vec::new();
        let mut rebased_pts: i64 = 0;
        let expected_encoded = usize::try_from(end_frame_exclusive - start_frame).unwrap_or(0);
        let mut head_meta = Vec::with_capacity(expected_encoded);
        let mut decoded_frames = 0u64;

        for idx in gop_start..end_frame_exclusive {
            track.read_sample_into(idx, &mut raw)?;
            let info = track.frame(idx)?;
            // Zero-copy CVPixelBuffer stays in GPU/IOSurface memory directly from decode to encode!
            let pb = dec.decode_avcc_sample_sync(
                &raw,
                info.pts_ticks,
                track.timescale(),
                info.duration_ticks,
            )?;
            decoded_frames += 1;
            if idx >= start_frame {
                let mode = if idx == start_frame {
                    FrameEncodeMode::ForceKeyframe
                } else {
                    FrameEncodeMode::Normal
                };
                enc.encode_pixel_buffer(
                    &pb,
                    rebased_pts,
                    track.timescale(),
                    info.duration_ticks,
                    mode,
                )?;
                rebased_pts = rebased_pts.saturating_add(i64::from(info.duration_ticks));
                head_meta.push((idx, info.duration_ticks));
            }
        }

        let VtEncodedStream {
            sps,
            pps,
            samples: encoded_samples,
        } = enc.finish()?;
        if encoded_samples.len() != head_meta.len() {
            return Err(MediaError::Codec(format!(
                "VideoToolbox re-encoded {} frames, expected {}",
                encoded_samples.len(),
                head_meta.len()
            )));
        }

        let mut samples = Vec::with_capacity(encoded_samples.len());
        for ((avcc_data, is_keyframe), (frame_index, duration_ticks)) in
            encoded_samples.into_iter().zip(head_meta)
        {
            if frame_index == start_frame && !is_keyframe {
                return Err(MediaError::Codec(format!(
                    "VideoToolbox first re-encoded frame {frame_index} was not IDR"
                )));
            }
            samples.push(EncodedSample {
                frame_index,
                avcc_data,
                duration_ticks,
                is_keyframe,
            });
        }

        let avcc_payload = build_avcc_payload(&sps, &pps)?;
        Ok(EncodedGop {
            sps,
            pps,
            avcc_payload,
            decoded_frames,
            samples,
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "streaming VideoToolbox decode-paint-reencode pipeline with GOP keyframe pacing"
    )]
    fn reencode_frame_range_with_overlay(
        &self,
        track: &Mp4VideoTrack,
        start_frame: u64,
        end_frame_exclusive: u64,
        params: EncodeParams,
        keyframe_interval: u64,
        has_overlay: &dyn Fn(u64) -> bool,
        apply_overlay: &mut dyn FnMut(u64, &mut MutableYuvFrame<'_>),
    ) -> Result<EncodedGop, MediaError> {
        let frame_count = track.frame_count();
        if start_frame >= end_frame_exclusive || end_frame_exclusive > frame_count {
            return Err(MediaError::InvalidFrameRange {
                in_frame_index: start_frame,
                out_frame_index: end_frame_exclusive.saturating_sub(1),
                frame_count,
            });
        }

        let interval = keyframe_interval.max(1);
        let gop_start = track.decodable_gop_start(start_frame)?;
        let avcc = track.avcc();
        let mut dec = VtDecoder::new(
            &avcc.sps,
            &avcc.pps,
            avcc.nal_length_size,
            track.width(),
            track.height(),
        )?;
        let mut enc = VtEncoder::new(track.width(), track.height(), params.bitrate_bps)?;
        let mut raw = Vec::new();
        let mut rebased_pts: i64 = 0;
        let expected_encoded = usize::try_from(end_frame_exclusive - start_frame).unwrap_or(0);
        let mut head_meta = Vec::with_capacity(expected_encoded);
        let mut decoded_frames = 0u64;

        for idx in gop_start..end_frame_exclusive {
            track.read_sample_into(idx, &mut raw)?;
            let info = track.frame(idx)?;
            let pb = dec.decode_avcc_sample_sync(
                &raw,
                info.pts_ticks,
                track.timescale(),
                info.duration_ticks,
            )?;
            decoded_frames += 1;
            if idx >= start_frame {
                let rel_idx = idx.saturating_sub(start_frame);
                let mode = if rel_idx % interval == 0 {
                    FrameEncodeMode::ForceKeyframe
                } else {
                    FrameEncodeMode::Normal
                };
                if has_overlay(idx) {
                    let mut overlay_fn =
                        |frame: &mut MutableYuvFrame<'_>| apply_overlay(idx, frame);
                    let composited_pb = pb.clone_writable_and_paint(&mut overlay_fn)?;
                    enc.encode_pixel_buffer(
                        &composited_pb,
                        rebased_pts,
                        track.timescale(),
                        info.duration_ticks,
                        mode,
                    )?;
                } else {
                    // Untouched frames stay zero-copy in GPU/IOSurface memory!
                    enc.encode_pixel_buffer(
                        &pb,
                        rebased_pts,
                        track.timescale(),
                        info.duration_ticks,
                        mode,
                    )?;
                }
                rebased_pts = rebased_pts.saturating_add(i64::from(info.duration_ticks));
                head_meta.push((idx, info.duration_ticks));
            }
        }

        let VtEncodedStream {
            sps,
            pps,
            samples: encoded_samples,
        } = enc.finish()?;
        if encoded_samples.len() != head_meta.len() {
            return Err(MediaError::Codec(format!(
                "VideoToolbox overlay-encoded {} frames, expected {}",
                encoded_samples.len(),
                head_meta.len()
            )));
        }

        let mut samples = Vec::with_capacity(encoded_samples.len());
        for ((avcc_data, is_keyframe), (frame_index, duration_ticks)) in
            encoded_samples.into_iter().zip(head_meta)
        {
            if frame_index == start_frame && !is_keyframe {
                return Err(MediaError::Codec(format!(
                    "VideoToolbox first overlay-encoded frame {frame_index} was not IDR"
                )));
            }
            samples.push(EncodedSample {
                frame_index,
                avcc_data,
                duration_ticks,
                is_keyframe,
            });
        }

        let avcc_payload = build_avcc_payload(&sps, &pps)?;
        Ok(EncodedGop {
            sps,
            pps,
            avcc_payload,
            decoded_frames,
            samples,
        })
    }
}
