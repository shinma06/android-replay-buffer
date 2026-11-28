//! Lightweight JPEG tiles for `arb_inspect_frames(include_images: true)` (#586-G).
//!
//! A baseline (sequential, Huffman) JFIF encoder written for this crate from ITU-T T.81: the
//! example quantization tables of Annex K.1, the typical Huffman tables of Annex K.3, YCbCr
//! 4:2:0 sampling and a separable floating-point DCT-II. No third-party code is involved.
//!
//! The lossless PNG the renderer caches is unchanged; a tile is a smaller copy of it whose longer
//! edge is at most [`DEFAULT_JPEG_MAX_EDGE`] pixels ([`fit_within`], K23).
use std::io::Cursor;

use crate::error::MediaError;

/// Longest edge of a default inspection tile, in pixels.
pub const DEFAULT_JPEG_MAX_EDGE: u32 = 1024;
/// Default JPEG quality (1..=100, the usual Annex K scaling).
pub const DEFAULT_JPEG_QUALITY: u8 = 80;
/// Largest edge the baseline frame header can carry.
const MAX_JPEG_EDGE: u32 = 65_535;

/// A JPEG copy of a rendered PNG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JpegTile {
    /// The JFIF bytes.
    pub bytes: Vec<u8>,
    /// Tile width in pixels.
    pub width: u32,
    /// Tile height in pixels.
    pub height: u32,
}

/// The size that fits `width x height` inside a `max_edge` square with the same aspect ratio.
///
/// An image that already fits is unchanged. Otherwise the longer edge becomes `max_edge` and the
/// shorter edge is rounded to the nearest integer, never below 1. Zero inputs are treated as 1,
/// so every result is in `1..=max(max_edge, 1)` (K23). The arithmetic is exact in `u64`.
#[must_use]
pub fn fit_within(width: u32, height: u32, max_edge: u32) -> (u32, u32) {
    let (w, h, max) = (width.max(1), height.max(1), max_edge.max(1));
    if w <= max && h <= max {
        return (w, h);
    }
    let (long, short) = (u64::from(w.max(h)), u64::from(w.min(h)));
    let max64 = u64::from(max);
    // Round half up. `short <= long` keeps the quotient at most `max`; the clamp states it.
    let scaled = ((short * max64 + long / 2) / long).clamp(1, max64);
    let scaled = u32::try_from(scaled).unwrap_or(max);
    if w >= h { (max, scaled) } else { (scaled, max) }
}

/// Area-average downscale of packed RGB8 pixels. Each output pixel averages the source pixels
/// its footprint covers, so thin UI lines fade instead of disappearing.
///
/// # Errors
///
/// [`MediaError::InvalidArgument`] when the buffer does not hold `width * height` pixels or an
/// output edge is zero or larger than its source edge.
pub fn resize_rgb_area(
    rgb: &[u8],
    width: u32,
    height: u32,
    out_width: u32,
    out_height: u32,
) -> Result<Vec<u8>, MediaError> {
    let (w, h) = (to_usize(width)?, to_usize(height)?);
    let (ow, oh) = (to_usize(out_width)?, to_usize(out_height)?);
    if w.checked_mul(h).and_then(|n| n.checked_mul(3)) != Some(rgb.len())
        || ow == 0
        || oh == 0
        || ow > w
        || oh > h
    {
        return Err(MediaError::InvalidArgument(format!(
            "cannot resize {width}x{height} ({} bytes) to {out_width}x{out_height}",
            rgb.len()
        )));
    }
    if (ow, oh) == (w, h) {
        return Ok(rgb.to_vec());
    }
    let span = |i: usize, out: usize, src: usize| {
        let start = i * src / out;
        let end = ((i + 1) * src / out).max(start + 1);
        (start, end)
    };
    let columns: Vec<_> = (0..ow).map(|x| span(x, ow, w)).collect();
    let mut out = Vec::with_capacity(ow * oh * 3);
    for y in 0..oh {
        let (y0, y1) = span(y, oh, h);
        for &(x0, x1) in &columns {
            let mut sum = [0_u64; 3];
            for row in y0..y1 {
                let line = &rgb[(row * w + x0) * 3..(row * w + x1) * 3];
                for px in line.as_chunks::<3>().0 {
                    for (acc, &c) in sum.iter_mut().zip(px) {
                        *acc += u64::from(c);
                    }
                }
            }
            let n = u64::try_from((y1 - y0) * (x1 - x0)).unwrap_or(u64::MAX);
            for acc in sum {
                out.push(u8::try_from((acc + n / 2) / n).unwrap_or(u8::MAX));
            }
        }
    }
    Ok(out)
}

/// Decodes a PNG, scales it to fit `max_edge` ([`fit_within`]) and encodes it as JPEG.
///
/// # Errors
///
/// [`MediaError::Png`] for an unreadable PNG, [`MediaError::InvalidArgument`] for an invalid
/// quality or size.
pub fn png_to_jpeg_tile(png: &[u8], max_edge: u32, quality: u8) -> Result<JpegTile, MediaError> {
    let (rgb, width, height) = decode_png_rgb(png)?;
    let (tw, th) = fit_within(width, height, max_edge);
    let pixels = resize_rgb_area(&rgb, width, height, tw, th)?;
    Ok(JpegTile {
        bytes: encode_jpeg_rgb(&pixels, tw, th, quality)?,
        width: tw,
        height: th,
    })
}

/// Encodes packed RGB8 pixels as a baseline 4:2:0 JFIF image.
///
/// # Errors
///
/// [`MediaError::InvalidArgument`] when `quality` is outside `1..=100`, an edge is zero or above
/// 65,535, or the buffer does not hold `width * height` pixels.
pub fn encode_jpeg_rgb(
    rgb: &[u8],
    width: u32,
    height: u32,
    quality: u8,
) -> Result<Vec<u8>, MediaError> {
    let (w, h) = (to_usize(width)?, to_usize(height)?);
    if !(1..=100).contains(&quality)
        || width == 0
        || height == 0
        || width > MAX_JPEG_EDGE
        || height > MAX_JPEG_EDGE
        || w.checked_mul(h).and_then(|n| n.checked_mul(3)) != Some(rgb.len())
    {
        return Err(MediaError::InvalidArgument(format!(
            "cannot encode {width}x{height} ({} bytes) at quality {quality}",
            rgb.len()
        )));
    }
    let luma_q = scaled_table(&LUMA_QUANT, quality);
    let chroma_q = scaled_table(&CHROMA_QUANT, quality);
    let tables = [
        Huffman::new(&DC_LUMA_BITS, &DC_LUMA_VALUES),
        Huffman::new(&AC_LUMA_BITS, &AC_LUMA_VALUES),
        Huffman::new(&DC_CHROMA_BITS, &DC_CHROMA_VALUES),
        Huffman::new(&AC_CHROMA_BITS, &AC_CHROMA_VALUES),
    ];
    let mut out = Vec::with_capacity(rgb.len() / 8 + 1024);
    write_headers(&mut out, width, height, &luma_q, &chroma_q);

    let planes = YCbCr::from_rgb(rgb, w, h);
    let dct = DctTable::new();
    let mut bits = BitWriter::new(out);
    let mut predictors = [0_i32; 3];
    let mut block = [0.0_f32; 64];
    for mcu_y in (0..h).step_by(16) {
        for mcu_x in (0..w).step_by(16) {
            for (dy, dx) in [(0, 0), (0, 8), (8, 0), (8, 8)] {
                planes.luma_block(mcu_x + dx, mcu_y + dy, &mut block);
                encode_block(
                    &mut bits,
                    &dct,
                    &block,
                    &luma_q,
                    &tables[0],
                    &tables[1],
                    &mut predictors[0],
                );
            }
            for (component, predictor) in [(1_usize, 1_usize), (2, 2)] {
                planes.chroma_block(component, mcu_x, mcu_y, &mut block);
                encode_block(
                    &mut bits,
                    &dct,
                    &block,
                    &chroma_q,
                    &tables[2],
                    &tables[3],
                    &mut predictors[predictor],
                );
            }
        }
    }
    let mut out = bits.finish();
    out.extend_from_slice(&[0xFF, 0xD9]);
    Ok(out)
}

fn to_usize(value: u32) -> Result<usize, MediaError> {
    usize::try_from(value).map_err(|_| MediaError::InvalidArgument("image too large".to_owned()))
}

/// The PNG's pixels as packed RGB8. Alpha is composited over black (renders are opaque).
fn decode_png_rgb(png: &[u8]) -> Result<(Vec<u8>, u32, u32), MediaError> {
    let mut decoder = png::Decoder::new(Cursor::new(png));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|e| MediaError::Png(format!("failed to read PNG header: {e}")))?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| MediaError::Png(format!("failed to decode PNG: {e}")))?;
    buf.truncate(info.buffer_size());
    let channels = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => {
            return Err(MediaError::Png("indexed PNG was not expanded".to_owned()));
        }
    };
    let pixels = buf
        .chunks_exact(channels)
        .flat_map(|px| {
            let alpha = if channels % 2 == 0 {
                u32::from(px[channels - 1])
            } else {
                255
            };
            let blend = |c: u8| u8::try_from(u32::from(c) * alpha / 255).unwrap_or(u8::MAX);
            if channels <= 2 {
                let g = blend(px[0]);
                [g, g, g]
            } else {
                [blend(px[0]), blend(px[1]), blend(px[2])]
            }
        })
        .collect();
    Ok((pixels, info.width, info.height))
}

/// Level-shifted (−128) Y, Cb and Cr planes; edges replicate the last row and column.
struct YCbCr {
    planes: [Vec<f32>; 3],
    width: usize,
    height: usize,
}

impl YCbCr {
    fn from_rgb(rgb: &[u8], width: usize, height: usize) -> Self {
        let mut planes = [
            Vec::with_capacity(width * height),
            Vec::with_capacity(width * height),
            Vec::with_capacity(width * height),
        ];
        for &[red, green, blue] in rgb.as_chunks::<3>().0 {
            let (red, green, blue) = (f32::from(red), f32::from(green), f32::from(blue));
            planes[0].push(0.299 * red + 0.587 * green + 0.114 * blue - 128.0);
            planes[1].push(-0.168_736 * red - 0.331_264 * green + 0.5 * blue);
            planes[2].push(0.5 * red - 0.418_688 * green - 0.081_312 * blue);
        }
        Self {
            planes,
            width,
            height,
        }
    }

    fn at(&self, component: usize, x: usize, y: usize) -> f32 {
        let (x, y) = (x.min(self.width - 1), y.min(self.height - 1));
        self.planes[component][y * self.width + x]
    }

    fn luma_block(&self, x0: usize, y0: usize, block: &mut [f32; 64]) {
        for (i, value) in block.iter_mut().enumerate() {
            *value = self.at(0, x0 + i % 8, y0 + i / 8);
        }
    }

    /// One 8x8 chroma block of a 16x16 MCU: each sample averages a 2x2 source square.
    fn chroma_block(&self, component: usize, x0: usize, y0: usize, block: &mut [f32; 64]) {
        for (i, value) in block.iter_mut().enumerate() {
            let (x, y) = (x0 + (i % 8) * 2, y0 + (i / 8) * 2);
            *value = (self.at(component, x, y)
                + self.at(component, x + 1, y)
                + self.at(component, x, y + 1)
                + self.at(component, x + 1, y + 1))
                / 4.0;
        }
    }
}

/// `basis[u][x] = C(u) / 2 * cos((2x + 1) u π / 16)`, the orthonormal 8-point DCT-II basis.
struct DctTable {
    basis: [[f32; 8]; 8],
}

impl DctTable {
    fn new() -> Self {
        let mut basis = [[0.0; 8]; 8];
        for (u, row) in basis.iter_mut().enumerate() {
            let scale = if u == 0 {
                std::f32::consts::FRAC_1_SQRT_2
            } else {
                1.0
            } / 2.0;
            for (x, value) in row.iter_mut().enumerate() {
                let steps = u16::try_from((2 * x + 1) * u).unwrap_or(0);
                *value = scale * (f32::from(steps) * std::f32::consts::PI / 16.0).cos();
            }
        }
        Self { basis }
    }

    /// Two-dimensional DCT of a row-major block: rows first, then columns.
    fn forward(&self, block: &[f32; 64]) -> [f32; 64] {
        let mut rows = [0.0_f32; 64];
        for y in 0..8 {
            for u in 0..8 {
                rows[y * 8 + u] = (0..8).map(|x| self.basis[u][x] * block[y * 8 + x]).sum();
            }
        }
        let mut out = [0.0_f32; 64];
        for u in 0..8 {
            for v in 0..8 {
                out[v * 8 + u] = (0..8).map(|y| self.basis[v][y] * rows[y * 8 + u]).sum();
            }
        }
        out
    }
}

fn encode_block(
    bits: &mut BitWriter,
    dct: &DctTable,
    block: &[f32; 64],
    quant: &[u16; 64],
    dc: &Huffman,
    ac: &Huffman,
    predictor: &mut i32,
) {
    let coefficients = dct.forward(block);
    let mut zz = [0_i32; 64];
    for (k, &natural) in ZIGZAG.iter().enumerate() {
        zz[k] = quantize(coefficients[natural], quant[k]);
    }
    let diff = zz[0] - *predictor;
    *predictor = zz[0];
    let size = magnitude_bits(diff);
    dc.emit(bits, size);
    bits.put(magnitude_code(diff, size), size);
    let mut run = 0_u8;
    for &value in &zz[1..] {
        if value == 0 {
            run += 1;
            continue;
        }
        while run >= 16 {
            ac.emit(bits, 0xF0);
            run -= 16;
        }
        let size = magnitude_bits(value);
        ac.emit(bits, (run << 4) | size);
        bits.put(magnitude_code(value, size), size);
        run = 0;
    }
    if run > 0 {
        ac.emit(bits, 0x00);
    }
}

/// Rounds `coefficient / step` to the nearest integer within ±1023, the largest magnitude
/// category (10 for AC, 11 for DC differences) the baseline Huffman tables carry.
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "f32 -> i32 has no TryFrom; the value is rounded and clamped to ±1023 first"
)]
fn quantize(coefficient: f32, step: u16) -> i32 {
    (coefficient / f32::from(step))
        .round()
        .clamp(-1023.0, 1023.0) as i32
}

/// The JPEG magnitude category: the number of bits of `|value|`.
fn magnitude_bits(value: i32) -> u8 {
    u8::try_from(32 - value.unsigned_abs().leading_zeros()).unwrap_or(0)
}

/// The `size` low bits of a value; negative values use the one's complement form.
fn magnitude_code(value: i32, size: u8) -> u32 {
    let v = if value < 0 { value - 1 } else { value };
    v.cast_unsigned() & ((1_u32 << size) - 1)
}

/// The Annex K example table scaled for `quality` (`5000 / q` below 50, `200 - 2q` from 50), in
/// zigzag order and clamped to 1..=255 for 8-bit tables.
fn scaled_table(base: &[u8; 64], quality: u8) -> [u16; 64] {
    let q = u32::from(quality.clamp(1, 100));
    let scale = if q < 50 { 5000 / q } else { 200 - 2 * q };
    let mut out = [0_u16; 64];
    for (k, &natural) in ZIGZAG.iter().enumerate() {
        let value = (u32::from(base[natural]) * scale + 50) / 100;
        out[k] = u16::try_from(value.clamp(1, 255)).unwrap_or(255);
    }
    out
}

fn write_headers(out: &mut Vec<u8>, width: u32, height: u32, luma: &[u16; 64], chroma: &[u16; 64]) {
    let be16 = |v: u32| {
        let [_, _, hi, lo] = v.to_be_bytes();
        [hi, lo]
    };
    out.extend_from_slice(&[0xFF, 0xD8]);
    // APP0 / JFIF 1.01, no thumbnail, 1:1 pixel aspect.
    out.extend_from_slice(&[
        0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00, 0x01, 0x01, 0x00, 0x00, 0x01, 0x00,
        0x01, 0x00, 0x00,
    ]);
    out.extend_from_slice(&[0xFF, 0xDB, 0x00, 0x84]);
    for (id, table) in [(0_u8, luma), (1, chroma)] {
        out.push(id);
        out.extend(table.iter().map(|&v| u8::try_from(v).unwrap_or(u8::MAX)));
    }
    out.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
    out.extend_from_slice(&be16(height));
    out.extend_from_slice(&be16(width));
    out.extend_from_slice(&[0x03, 0x01, 0x22, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01]);
    for (class_id, bits, values) in [
        (0x00_u8, &DC_LUMA_BITS, &DC_LUMA_VALUES[..]),
        (0x10, &AC_LUMA_BITS, &AC_LUMA_VALUES[..]),
        (0x01, &DC_CHROMA_BITS, &DC_CHROMA_VALUES[..]),
        (0x11, &AC_CHROMA_BITS, &AC_CHROMA_VALUES[..]),
    ] {
        out.extend_from_slice(&[0xFF, 0xC4]);
        out.extend_from_slice(&be16(u32::try_from(3 + 16 + values.len()).unwrap_or(0)));
        out.push(class_id);
        out.extend_from_slice(bits);
        out.extend_from_slice(values);
    }
    out.extend_from_slice(&[
        0xFF, 0xDA, 0x00, 0x0C, 0x03, 0x01, 0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x3F, 0x00,
    ]);
}

/// Canonical Huffman codes (T.81 Annex C) indexed by symbol.
struct Huffman {
    codes: [(u16, u8); 256],
}

impl Huffman {
    fn new(bits: &[u8; 16], values: &[u8]) -> Self {
        let mut codes = [(0_u16, 0_u8); 256];
        let mut code = 0_u16;
        let mut symbols = values.iter();
        for (length, &count) in (1_u8..=16).zip(bits) {
            for _ in 0..count {
                if let Some(&symbol) = symbols.next() {
                    codes[usize::from(symbol)] = (code, length);
                }
                code = code.wrapping_add(1);
            }
            code = code.wrapping_shl(1);
        }
        Self { codes }
    }

    fn emit(&self, bits: &mut BitWriter, symbol: u8) {
        let (code, length) = self.codes[usize::from(symbol)];
        debug_assert!(length > 0, "symbol {symbol:#x} has no code");
        bits.put(u32::from(code), length);
    }
}

/// Big-endian entropy-coded segment writer with `0xFF 0x00` byte stuffing.
struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    used: u8,
}

impl BitWriter {
    const fn new(out: Vec<u8>) -> Self {
        Self {
            out,
            acc: 0,
            used: 0,
        }
    }

    fn put(&mut self, value: u32, length: u8) {
        for i in (0..length).rev() {
            self.acc = (self.acc << 1) | ((value >> i) & 1);
            self.used += 1;
            if self.used == 8 {
                self.push_byte();
            }
        }
    }

    fn push_byte(&mut self) {
        let [.., byte] = self.acc.to_be_bytes();
        self.out.push(byte);
        if byte == 0xFF {
            self.out.push(0x00);
        }
        self.acc = 0;
        self.used = 0;
    }

    /// Pads the last byte with 1 bits, as T.81 F.1.2.3 requires.
    fn finish(mut self) -> Vec<u8> {
        if self.used > 0 {
            let pad = 8 - self.used;
            self.put((1 << pad) - 1, pad);
        }
        self.out
    }
}

/// Zigzag position `k` → natural (row-major) index.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// T.81 Table K.1 (luminance), row-major.
const LUMA_QUANT: [u8; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113,
    92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];

/// T.81 Table K.2 (chrominance), row-major.
const CHROMA_QUANT: [u8; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];

// T.81 Annex K.3: code-length counts (lengths 1..=16) and symbols in code order.
const DC_LUMA_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const DC_LUMA_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const DC_CHROMA_BITS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
const DC_CHROMA_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const AC_LUMA_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7D];
const AC_LUMA_VALUES: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08, 0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0A, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2A, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7,
    0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5,
    0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2,
    0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8,
    0xF9, 0xFA,
];
const AC_CHROMA_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
const AC_CHROMA_VALUES: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xA1, 0xB1, 0xC1, 0x09, 0x23, 0x33, 0x52, 0xF0,
    0x15, 0x62, 0x72, 0xD1, 0x0A, 0x16, 0x24, 0x34, 0xE1, 0x25, 0xF1, 0x17, 0x18, 0x19, 0x1A, 0x26,
    0x27, 0x28, 0x29, 0x2A, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5,
    0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3,
    0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA,
    0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8,
    0xF9, 0xFA,
];

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn gradient(width: u32, height: u32) -> Vec<u8> {
        let mut rgb = Vec::new();
        for row in 0..height {
            for col in 0..width {
                // A UI-like image: flat panels, a dark text bar and a smooth gradient.
                let bar = (row / 8) % 5 == 0 && col % 64 < 48;
                let pixel = if bar {
                    [20, 20, 24]
                } else {
                    [
                        u8::try_from(col * 255 / width.max(1)).unwrap(),
                        u8::try_from(row * 255 / height.max(1)).unwrap(),
                        200,
                    ]
                };
                rgb.extend_from_slice(&pixel);
            }
        }
        rgb
    }

    fn psnr(a: &[u8], b: &[u8]) -> f64 {
        let mse = a
            .iter()
            .zip(b)
            .map(|(&x, &y)| (f64::from(x) - f64::from(y)).powi(2))
            .sum::<f64>()
            / f64::from(u32::try_from(a.len()).unwrap());
        10.0 * (255.0_f64.powi(2) / mse.max(1e-9)).log10()
    }

    fn decode(jpeg: &[u8]) -> (Vec<u8>, u16, u16) {
        let mut decoder = jpeg_decoder::Decoder::new(jpeg);
        let pixels = decoder.decode().expect("a valid JPEG");
        let info = decoder.info().expect("frame info");
        assert_eq!(info.pixel_format, jpeg_decoder::PixelFormat::RGB24);
        (pixels, info.width, info.height)
    }

    #[test]
    fn huffman_tables_match_their_counts() {
        for (bits, len) in [
            (&DC_LUMA_BITS, DC_LUMA_VALUES.len()),
            (&DC_CHROMA_BITS, DC_CHROMA_VALUES.len()),
            (&AC_LUMA_BITS, AC_LUMA_VALUES.len()),
            (&AC_CHROMA_BITS, AC_CHROMA_VALUES.len()),
        ] {
            assert_eq!(bits.iter().map(|&b| usize::from(b)).sum::<usize>(), len);
        }
        let mut seen = [false; 64];
        for &i in &ZIGZAG {
            assert!(!seen[i]);
            seen[i] = true;
        }
    }

    #[test]
    fn independent_decoder_reads_every_size_back() {
        for (w, h) in [(1, 1), (7, 3), (16, 16), (17, 33), (280, 560), (1024, 461)] {
            let rgb = gradient(w, h);
            let jpeg = encode_jpeg_rgb(&rgb, w, h, DEFAULT_JPEG_QUALITY).unwrap();
            assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
            assert_eq!(&jpeg[jpeg.len() - 2..], &[0xFF, 0xD9]);
            let (pixels, dw, dh) = decode(&jpeg);
            assert_eq!((u32::from(dw), u32::from(dh)), (w, h));
            if w * h >= 256 {
                let quality = psnr(&rgb, &pixels);
                assert!(quality > 30.0, "{w}x{h}: PSNR {quality:.1} dB");
            }
        }
    }

    #[test]
    fn png_tiles_fit_the_edge_and_are_smaller() {
        let (w, h) = (1080, 2400);
        let rgb = gradient(w, h);
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, w, h);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&rgb)
                .unwrap();
        }
        let tile = png_to_jpeg_tile(&png, DEFAULT_JPEG_MAX_EDGE, DEFAULT_JPEG_QUALITY).unwrap();
        assert_eq!((tile.width, tile.height), (461, 1024));
        let (_, dw, dh) = decode(&tile.bytes);
        assert_eq!((u32::from(dw), u32::from(dh)), (461, 1024));
        assert!(
            tile.bytes.len() * 2 < png.len(),
            "{} vs {}",
            tile.bytes.len(),
            png.len()
        );
    }

    #[test]
    fn invalid_arguments_are_rejected() {
        assert!(encode_jpeg_rgb(&[0; 3], 1, 1, 0).is_err());
        assert!(encode_jpeg_rgb(&[0; 3], 1, 1, 101).is_err());
        assert!(encode_jpeg_rgb(&[0; 6], 1, 1, 80).is_err());
        assert!(encode_jpeg_rgb(&[], 0, 1, 80).is_err());
        assert!(resize_rgb_area(&[0; 12], 2, 2, 3, 1).is_err());
        assert!(png_to_jpeg_tile(b"not a png", 1024, 80).is_err());
    }

    #[test]
    fn fit_within_examples() {
        assert_eq!(fit_within(1080, 2400, 1024), (461, 1024));
        assert_eq!(fit_within(2400, 1080, 1024), (1024, 461));
        assert_eq!(fit_within(640, 480, 1024), (640, 480));
        assert_eq!(fit_within(1024, 1024, 1024), (1024, 1024));
        assert_eq!(fit_within(u32::MAX, 1, 1024), (1024, 1));
        assert_eq!(fit_within(0, 0, 1024), (1, 1));
        assert_eq!(fit_within(5000, 5000, 1024), (1024, 1024));
    }

    proptest! {
        /// K23 on random sizes: positive, inside the box, the longer edge reaches the box when
        /// scaling, and the shorter edge is the nearest integer to the exact ratio.
        #[test]
        fn fit_within_keeps_the_aspect_ratio(w in 1_u32.., h in 1_u32.., max in 1_u32..=4096) {
            let (ow, oh) = fit_within(w, h, max);
            prop_assert!((1..=max).contains(&ow) || (ow == w && w <= max));
            prop_assert!(ow >= 1 && oh >= 1 && ow <= max && oh <= max);
            if w <= max && h <= max {
                prop_assert_eq!((ow, oh), (w, h));
            } else {
                let (long, short, out_short) = if w >= h { (w, h, oh) } else { (h, w, ow) };
                prop_assert_eq!(ow.max(oh), max);
                let exact = u128::from(short) * u128::from(max);
                let err = (u128::from(out_short) * u128::from(long)).abs_diff(exact);
                prop_assert!(2 * err <= u128::from(long) || (out_short == 1 && 2 * exact < u128::from(long)));
            }
        }

        #[test]
        fn resize_averages_a_flat_colour_exactly(
            w in 1_u32..64, h in 1_u32..64, ow in 1_u32..64, oh in 1_u32..64, c in any::<[u8; 3]>()
        ) {
            prop_assume!(ow <= w && oh <= h);
            let rgb: Vec<u8> = (0..w * h).flat_map(|_| c).collect();
            let out = resize_rgb_area(&rgb, w, h, ow, oh).unwrap();
            prop_assert_eq!(out.len(), usize::try_from(ow * oh * 3).unwrap());
            prop_assert!(out.as_chunks::<3>().0.iter().all(|px| *px == c));
        }
    }
}
