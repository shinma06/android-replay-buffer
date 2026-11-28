//! Reads the active H.264 picture dimensions without decoding video or loading slice payloads.
use std::collections::BTreeMap;

use h264_reader::nal::{Nal as _, RefNal, sps::SeqParameterSet};
use h264_reader::rbsp::BitRead as _;

use super::{Mp4VideoTrack, read_exact_at};
use crate::error::MediaError;

impl Mp4VideoTrack {
    /// Visible frame dimensions, including SPS cropping. ARB changes configuration at IDRs.
    /// The track's sample entry can describe a different GOP after screen rotation.
    ///
    /// # Errors
    ///
    /// Returns an error if the frame has no sync sample or its slice refers to an unknown SPS.
    pub fn frame_dimensions(&self, index: u64) -> Result<(u32, u32), MediaError> {
        let start = self.decodable_gop_start(index)?;
        let mut cache = self
            .gop_dimensions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(&dims) = cache.get(&start) {
            return Ok(dims);
        }
        let sample = self.frame(start)?;
        let mut sets = ParameterSets::default();
        for nal in self.avcc.sps.iter().chain(&self.avcc.pps) {
            sets.observe(nal);
        }
        let len_size = self.avcc.nal_length_size;
        let len_bytes = u64::try_from(len_size).unwrap_or(4);
        let mut pos = sample.offset;
        let end = pos + u64::from(sample.size);
        // The MP4 index has already validated sample bounds and the 1..=4 length field.
        let mut len_buf = [0; 4];
        while end - pos > len_bytes {
            let len_field = &mut len_buf[4 - len_size..];
            read_exact_at(&self.file, len_field, pos)?;
            pos += len_bytes;
            let size = u64::from(u32::from_be_bytes(len_buf));
            if size > end - pos {
                break;
            }
            if size == 0 {
                continue;
            }
            let mut header = [0];
            read_exact_at(&self.file, &mut header, pos)?;
            let ty = header[0] & 31;
            let bytes = match ty {
                7 => u16::try_from(size).unwrap_or(u16::MAX), // SPS syntax, including cropping
                8 | 1 | 5 => u16::try_from(size).unwrap_or(u16::MAX).min(64), // only the first Exp-Golomb fields
                _ => 0,
            };
            if bytes != 0 {
                let mut prefix = vec![0; usize::from(bytes)];
                read_exact_at(&self.file, &mut prefix, pos)?;
                if let Some(dims) = sets.observe(&prefix) {
                    cache.insert(start, dims);
                    return Ok(dims);
                }
                if matches!(ty, 1 | 5) {
                    break;
                }
            }
            pos += size;
        }
        Err(MediaError::InvalidContainer(format!(
            "could not determine picture dimensions at sync sample {start}"
        )))
    }
}

#[derive(Default)]
struct ParameterSets {
    sps: BTreeMap<u32, (u32, u32)>,
    pps: BTreeMap<u32, u32>,
}

impl ParameterSets {
    /// Applies in-band overrides in bitstream order and resolves the first slice's PPS → SPS.
    fn observe(&mut self, bytes: &[u8]) -> Option<(u32, u32)> {
        let ty = *bytes.first()? & 31;
        let nal = RefNal::new(bytes, &[], true);
        let mut bits = nal.rbsp_bits();
        match ty {
            7 => {
                let seq = SeqParameterSet::from_bits(bits).ok()?;
                self.sps
                    .insert(u32::from(seq.id().id()), seq.pixel_dimensions().ok()?);
            }
            8 => {
                let pic = bits.read_ue("pic_parameter_set_id").ok()?;
                let seq = bits.read_ue("seq_parameter_set_id").ok()?;
                self.pps.insert(pic, seq);
            }
            1 | 5 => {
                bits.read_ue("first_mb_in_slice").ok()?;
                bits.read_ue("slice_type").ok()?;
                let pic = bits.read_ue("pic_parameter_set_id").ok()?;
                return self.sps.get(self.pps.get(&pic)?).copied();
            }
            _ => {}
        }
        None
    }
}
