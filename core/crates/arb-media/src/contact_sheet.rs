//! Pure-Rust AI contact-sheet and tap-context crop PNG engine.
//!
//! Powers the MCP-first AI-precision trimming workflow (`arb_inspect_frames` in `contact_sheet`,
//! `single` and `tap_crop` modes) with **zero device load, zero external fonts, zero GPL crates**
//! and byte-identical output on Linux, macOS and Windows (integer-only resampling and drawing):
//!
//! - [`render_contact_sheet_png`]: up to [`MAX_CONTACT_SHEET_TILES`] decoded frames, each
//!   box-filter downscaled into a dark tile cell with a burned-in `#<frame>  MM:SS.T` label bar
//!   (built-in 5×7 bitmap font) and an optional cyan tap marker;
//! - [`render_tap_crop_png`]: a resolution-relative context crop around a tap with a 1 px
//!   crosshair and small centre ring at the exact tapped pixel;
//! - [`select_contact_sheet_indices`]: deterministic, evenly spaced frame selection that always
//!   includes both range ends.

use crate::error::MediaError;

/// Maximum number of tiles in one contact sheet.
pub const MAX_CONTACT_SHEET_TILES: usize = 16;

/// Largest accepted frame edge in pixels (guards allocation sizes).
const MAX_FRAME_EDGE: u32 = 16_384;

/// Largest accepted `max_tile_width` / `max_tile_height`.
const MAX_TILE_EDGE: u32 = 1024;

/// Smallest accepted `max_tile_width` / `max_tile_height`.
const MIN_TILE_EDGE: u32 = 16;

/// Largest accepted gutter in pixels.
const MAX_GUTTER: u32 = 64;

/// Glyph scale (pixels per font dot) of the burned-in label text.
const LABEL_SCALE: usize = 2;

/// Blank dots around the label text inside the label bar.
const LABEL_PAD: usize = 3;

/// Height of the label bar in pixels.
const LABEL_BAR_HEIGHT: usize = 7 * LABEL_SCALE + 2 * LABEL_PAD;

type Rgb = [u8; 3];

const SHEET_BACKGROUND: Rgb = [36, 36, 44];
const CELL_BACKGROUND: Rgb = [8, 8, 10];
const LABEL_BACKGROUND: Rgb = [0, 0, 0];
const LABEL_FOREGROUND: Rgb = [255, 255, 255];
const MARKER_COLOR: Rgb = [0, 255, 255];
const MARKER_HALO: Rgb = [0, 0, 0];

/// One decoded frame to place in a contact sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactSheetTile {
    /// Zero-based frame index burned into the label as `#<frame_index>`.
    pub frame_index: u64,
    /// Frame timestamp in milliseconds burned into the label as `MM:SS.T`.
    pub timestamp_ms: i64,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Tightly packed pixels: `width * height * 4` (RGBA, alpha ignored) or
    /// `width * height * 3` (RGB) bytes.
    pub rgba: Vec<u8>,
    /// Tap position in source-frame pixels; a cyan ring + crosshair is drawn there.
    pub tap_point: Option<(u32, u32)>,
}

/// Layout options for [`render_contact_sheet_png`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContactSheetOptions {
    /// Maximum scaled tile width in pixels (default `280`).
    pub max_tile_width: u32,
    /// Maximum scaled tile height in pixels, excluding the label bar (default `560`).
    pub max_tile_height: u32,
    /// Grid columns (default `4`, so 16 tiles form a 4×4 grid).
    pub columns: u32,
    /// Gutter between tiles and around the sheet in pixels (default `4`).
    pub gutter: u32,
}

impl Default for ContactSheetOptions {
    fn default() -> Self {
        Self {
            max_tile_width: 280,
            max_tile_height: 560,
            columns: 4,
            gutter: 4,
        }
    }
}

/// Options for [`render_tap_crop_png`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TapCropOptions {
    /// Crop width as a fraction of the frame width (default `0.28`).
    pub width_fraction: f32,
    /// Crop height as a fraction of the frame height (default `0.12`).
    pub height_fraction: f32,
    /// Minimum crop width in pixels (default `64`, capped at the frame width).
    pub min_width: u32,
    /// Minimum crop height in pixels (default `64`, capped at the frame height).
    pub min_height: u32,
}

impl Default for TapCropOptions {
    fn default() -> Self {
        Self {
            width_fraction: 0.28,
            height_fraction: 0.12,
            min_width: 64,
            min_height: 64,
        }
    }
}

/// Result of [`render_tap_crop_png`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TapCropResult {
    /// Encoded PNG bytes of the crop (native resolution, crosshair burned in).
    pub png_bytes: Vec<u8>,
    /// Left edge of the crop inside the source frame.
    pub crop_x: u32,
    /// Top edge of the crop inside the source frame.
    pub crop_y: u32,
    /// Crop width in pixels.
    pub crop_width: u32,
    /// Crop height in pixels.
    pub crop_height: u32,
    /// Tap x relative to the crop (`tap_x - crop_x`).
    pub tap_rel_x: u32,
    /// Tap y relative to the crop (`tap_y - crop_y`).
    pub tap_rel_y: u32,
}

/// Picks up to `max_tiles` (at most [`MAX_CONTACT_SHEET_TILES`]) frame indices from the inclusive
/// range `[start_frame, end_frame]`, sorted and deduplicated.
///
/// Both `start_frame` and `end_frame` are always included when `max_tiles >= 2` and
/// `end_frame > start_frame`; interior frames are spread evenly. A range with fewer frames than
/// `max_tiles` yields every frame. `max_tiles == 0` yields an empty list; `max_tiles == 1` or
/// `end_frame <= start_frame` yields just `start_frame`.
pub fn select_contact_sheet_indices(
    start_frame: u64,
    end_frame: u64,
    max_tiles: usize,
) -> Vec<u64> {
    let max_tiles = max_tiles.min(MAX_CONTACT_SHEET_TILES);
    if max_tiles == 0 {
        return Vec::new();
    }
    if max_tiles == 1 || end_frame <= start_frame {
        return vec![start_frame];
    }
    let span = end_frame - start_frame;
    let wanted = u64::try_from(max_tiles).unwrap_or(u64::MAX);
    if wanted > span {
        // Fewer frames than tiles: every frame in the range.
        return (start_frame..=end_frame).collect();
    }
    let steps = wanted - 1;
    let mut indices: Vec<u64> = (0..wanted)
        .map(|i| {
            let offset = u128::from(span) * u128::from(i) / u128::from(steps);
            start_frame + u64::try_from(offset).unwrap_or(span)
        })
        .collect();
    indices.sort_unstable();
    indices.dedup();
    indices
}

/// Renders a contact sheet PNG from `frames` (1 to [`MAX_CONTACT_SHEET_TILES`] tiles).
///
/// # Errors
///
/// Returns [`MediaError::InvalidArgument`] if `frames` is empty or longer than
/// [`MAX_CONTACT_SHEET_TILES`], a tile has a zero/oversized dimension, a pixel buffer whose
/// length matches neither RGBA nor RGB, or a `tap_point` outside its frame, or if `opts` is out
/// of range; or [`MediaError::Png`] if encoding fails.
pub fn render_contact_sheet_png(
    frames: &[ContactSheetTile],
    opts: &ContactSheetOptions,
) -> Result<Vec<u8>, MediaError> {
    if frames.is_empty() || frames.len() > MAX_CONTACT_SHEET_TILES {
        return Err(MediaError::InvalidArgument(format!(
            "contact sheet needs 1..={MAX_CONTACT_SHEET_TILES} tiles, got {}",
            frames.len()
        )));
    }
    validate_sheet_options(opts)?;

    let max_w = usize_from(opts.max_tile_width);
    let max_h = usize_from(opts.max_tile_height);
    let gutter = usize_from(opts.gutter);

    // Validate every tile and compute its scaled size before allocating the canvas.
    let mut sizes = Vec::with_capacity(frames.len());
    for (idx, tile) in frames.iter().enumerate() {
        let channels = validate_pixels(tile.width, tile.height, tile.rgba.len(), "tile", idx)?;
        if let Some((tx, ty)) = tile.tap_point
            && (tx >= tile.width || ty >= tile.height)
        {
            return Err(MediaError::InvalidArgument(format!(
                "tile {idx} tap point ({tx}, {ty}) is outside its {}x{} frame",
                tile.width, tile.height
            )));
        }
        let (sw, sh) = (usize_from(tile.width), usize_from(tile.height));
        sizes.push((fit_size(sw, sh, max_w, max_h), channels));
    }

    let cell_w = sizes.iter().map(|((dw, _), _)| *dw).max().unwrap_or(1);
    let image_h = sizes.iter().map(|((_, dh), _)| *dh).max().unwrap_or(1);
    let cell_h = LABEL_BAR_HEIGHT + image_h;
    let columns = usize_from(opts.columns).min(frames.len()).max(1);
    let rows = frames.len().div_ceil(columns);

    let sheet_w = gutter + columns * (cell_w + gutter);
    let sheet_h = gutter + rows * (cell_h + gutter);
    let mut canvas = Canvas::new(sheet_w, sheet_h, SHEET_BACKGROUND)?;

    for (idx, (tile, ((dw, dh), channels))) in frames.iter().zip(&sizes).enumerate() {
        let col = idx % columns;
        let row = idx / columns;
        let cell_x = gutter + col * (cell_w + gutter);
        let cell_y = gutter + row * (cell_h + gutter);
        canvas.fill_rect(cell_x, cell_y, cell_w, cell_h, CELL_BACKGROUND);

        // Label bar.
        canvas.fill_rect(cell_x, cell_y, cell_w, LABEL_BAR_HEIGHT, LABEL_BACKGROUND);
        let label = format_label(tile.frame_index, tile.timestamp_ms);
        canvas.draw_text(
            &label,
            cell_x + LABEL_PAD,
            cell_y + LABEL_PAD,
            LABEL_SCALE,
            LABEL_FOREGROUND,
            Rect {
                x: cell_x,
                y: cell_y,
                w: cell_w,
                h: LABEL_BAR_HEIGHT,
            },
        );

        // Downscaled frame, centred in the image area below the label bar.
        let (sw, sh) = (usize_from(tile.width), usize_from(tile.height));
        let scaled = box_downscale(&tile.rgba, *channels, sw, sh, *dw, *dh);
        let img_x = cell_x + (cell_w - dw) / 2;
        let img_y = cell_y + LABEL_BAR_HEIGHT + (image_h - dh) / 2;
        canvas.blit_rgb(&scaled, *dw, *dh, img_x, img_y);

        if let Some((tx, ty)) = tile.tap_point {
            let mx = scale_center(usize_from(tx), sw, *dw);
            let my = scale_center(usize_from(ty), sh, *dh);
            canvas.draw_tap_marker(
                signed(img_x + mx),
                signed(img_y + my),
                &SHEET_MARKER,
                Rect {
                    x: img_x,
                    y: img_y,
                    w: *dw,
                    h: *dh,
                },
            );
        }
    }

    encode_rgb_png(sheet_w, sheet_h, &canvas.data)
}

/// Renders a resolution-relative context crop around `(tap_x, tap_y)` of an RGBA (or RGB) frame,
/// with a 1 px crosshair and small centre ring burned in at the exact tapped pixel.
///
/// The crop is `width_fraction × height_fraction` of the frame (at least `min_width ×
/// min_height`, never larger than the frame), centred on the tap and shifted to stay inside
/// `[0, width) × [0, height)`, so taps on any edge or corner are safe.
///
/// # Errors
///
/// Returns [`MediaError::InvalidArgument`] if the frame has a zero/oversized dimension or a
/// buffer length matching neither RGBA nor RGB, the tap is outside the frame, or a fraction is
/// not finite and in `(0, 1]`; or [`MediaError::Png`] if encoding fails.
pub fn render_tap_crop_png(
    frame_rgba: &[u8],
    width: u32,
    height: u32,
    tap_x: u32,
    tap_y: u32,
    opts: &TapCropOptions,
) -> Result<TapCropResult, MediaError> {
    let channels = validate_pixels(width, height, frame_rgba.len(), "frame", 0)?;
    if tap_x >= width || tap_y >= height {
        return Err(MediaError::InvalidArgument(format!(
            "tap point ({tap_x}, {tap_y}) is outside the {width}x{height} frame"
        )));
    }
    let crop_width = crop_extent(width, opts.width_fraction, opts.min_width, "width_fraction")?;
    let crop_height = crop_extent(
        height,
        opts.height_fraction,
        opts.min_height,
        "height_fraction",
    )?;
    let crop_x = tap_x.saturating_sub(crop_width / 2).min(width - crop_width);
    let crop_y = tap_y
        .saturating_sub(crop_height / 2)
        .min(height - crop_height);
    let tap_rel_x = tap_x - crop_x;
    let tap_rel_y = tap_y - crop_y;

    let (cw, ch) = (usize_from(crop_width), usize_from(crop_height));
    let (fw, cx, cy) = (usize_from(width), usize_from(crop_x), usize_from(crop_y));
    let mut canvas = Canvas::new(cw, ch, CELL_BACKGROUND)?;
    for row in 0..ch {
        let src_start = ((cy + row) * fw + cx) * channels;
        let src_row = &frame_rgba[src_start..src_start + cw * channels];
        let dst_start = row * cw * 3;
        for (col, px) in src_row.chunks_exact(channels).enumerate() {
            canvas.data[dst_start + col * 3..dst_start + col * 3 + 3].copy_from_slice(&px[..3]);
        }
    }
    canvas.draw_tap_marker(
        signed(usize_from(tap_rel_x)),
        signed(usize_from(tap_rel_y)),
        &CROP_MARKER,
        Rect {
            x: 0,
            y: 0,
            w: cw,
            h: ch,
        },
    );

    Ok(TapCropResult {
        png_bytes: encode_rgb_png(cw, ch, &canvas.data)?,
        crop_x,
        crop_y,
        crop_width,
        crop_height,
        tap_rel_x,
        tap_rel_y,
    })
}

/// Encodes a single full-resolution RGBA (or RGB) frame into PNG bytes, optionally burning a
/// cyan crosshair + ring tap marker at `tap_point`.
///
/// # Errors
///
/// Returns [`MediaError::InvalidArgument`] if dimensions, pixel buffer length, or `tap_point`
/// are invalid, or [`MediaError::Png`] if PNG encoding fails.
pub fn render_single_frame_png(
    frame_rgba: &[u8],
    width: u32,
    height: u32,
    tap_point: Option<(u32, u32)>,
) -> Result<Vec<u8>, MediaError> {
    let channels = validate_pixels(width, height, frame_rgba.len(), "frame", 0)?;
    if let Some((tx, ty)) = tap_point
        && (tx >= width || ty >= height)
    {
        return Err(MediaError::InvalidArgument(format!(
            "tap point ({tx}, {ty}) is outside the {width}x{height} frame"
        )));
    }
    let (fw, fh) = (usize_from(width), usize_from(height));
    let mut canvas = Canvas::new(fw, fh, CELL_BACKGROUND)?;
    for (idx, px) in frame_rgba.chunks_exact(channels).enumerate() {
        let dst = idx * 3;
        canvas.data[dst..dst + 3].copy_from_slice(&px[..3]);
    }
    if let Some((tx, ty)) = tap_point {
        canvas.draw_tap_marker(
            signed(usize_from(tx)),
            signed(usize_from(ty)),
            &CROP_MARKER,
            Rect {
                x: 0,
                y: 0,
                w: fw,
                h: fh,
            },
        );
    }
    encode_rgb_png(fw, fh, &canvas.data)
}

// ---------------------------------------------------------------------------------------------
// Validation and geometry
// ---------------------------------------------------------------------------------------------

fn validate_sheet_options(opts: &ContactSheetOptions) -> Result<(), MediaError> {
    let edge = MIN_TILE_EDGE..=MAX_TILE_EDGE;
    if !edge.contains(&opts.max_tile_width) || !edge.contains(&opts.max_tile_height) {
        return Err(MediaError::InvalidArgument(format!(
            "max_tile_width/max_tile_height must be within {MIN_TILE_EDGE}..={MAX_TILE_EDGE}, \
             got {}x{}",
            opts.max_tile_width, opts.max_tile_height
        )));
    }
    if opts.columns == 0 || opts.columns > 16 {
        return Err(MediaError::InvalidArgument(format!(
            "columns must be within 1..=16, got {}",
            opts.columns
        )));
    }
    if opts.gutter > MAX_GUTTER {
        return Err(MediaError::InvalidArgument(format!(
            "gutter must be at most {MAX_GUTTER}, got {}",
            opts.gutter
        )));
    }
    Ok(())
}

/// Validates dimensions and buffer length; returns the channel count (3 or 4).
fn validate_pixels(
    width: u32,
    height: u32,
    len: usize,
    what: &str,
    idx: usize,
) -> Result<usize, MediaError> {
    if width == 0 || height == 0 || width > MAX_FRAME_EDGE || height > MAX_FRAME_EDGE {
        return Err(MediaError::InvalidArgument(format!(
            "{what} {idx} has invalid dimensions {width}x{height} (1..={MAX_FRAME_EDGE})"
        )));
    }
    let pixels = usize_from(width) * usize_from(height);
    if len == pixels * 4 {
        Ok(4)
    } else if len == pixels * 3 {
        Ok(3)
    } else {
        Err(MediaError::InvalidArgument(format!(
            "{what} {idx} buffer has {len} bytes; expected {} (RGBA) or {} (RGB) for {width}x{height}",
            pixels * 4,
            pixels * 3
        )))
    }
}

/// Fits `sw × sh` inside `max_w × max_h` preserving aspect ratio, never upscaling.
fn fit_size(sw: usize, sh: usize, max_w: usize, max_h: usize) -> (usize, usize) {
    if sw <= max_w && sh <= max_h {
        return (sw, sh);
    }
    if sw * max_h >= sh * max_w {
        (max_w, (sh * max_w / sw).max(1))
    } else {
        ((sw * max_h / sh).max(1), max_h)
    }
}

/// Maps the centre of source pixel `p` (of `src` pixels) into a `dst`-pixel axis.
fn scale_center(p: usize, src: usize, dst: usize) -> usize {
    ((2 * p + 1) * dst / (2 * src)).min(dst.saturating_sub(1))
}

fn crop_extent(total: u32, fraction: f32, min_px: u32, name: &str) -> Result<u32, MediaError> {
    if !fraction.is_finite() || fraction <= 0.0 || fraction > 1.0 {
        return Err(MediaError::InvalidArgument(format!(
            "{name} must be finite and within (0, 1], got {fraction}"
        )));
    }
    let wanted = round_to_u32(f64::from(total) * f64::from(fraction));
    Ok(wanted.max(min_px.min(total)).clamp(1, total))
}

/// Rounds a finite, non-negative `f64` to the nearest `u32` (saturating).
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "f64 -> u32 has no TryFrom; the value is rounded, non-negative and saturating"
)]
fn round_to_u32(value: f64) -> u32 {
    value.round() as u32
}

/// Widening conversion (`u32` always fits `usize` on supported targets).
fn usize_from(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

fn signed(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

// ---------------------------------------------------------------------------------------------
// Resampling
// ---------------------------------------------------------------------------------------------

/// Integer box-filter downscale of an RGB(A) buffer into tightly packed RGB. Deterministic on
/// every platform; an unscaled copy when the sizes match.
fn box_downscale(
    src: &[u8],
    channels: usize,
    sw: usize,
    sh: usize,
    dw: usize,
    dh: usize,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(dw * dh * 3);
    for dy in 0..dh {
        let sy0 = dy * sh / dh;
        let sy1 = ((dy + 1) * sh / dh).max(sy0 + 1).min(sh);
        for dx in 0..dw {
            let sx0 = dx * sw / dw;
            let sx1 = ((dx + 1) * sw / dw).max(sx0 + 1).min(sw);
            let mut sums = [0u64; 3];
            for sy in sy0..sy1 {
                let row = sy * sw * channels;
                for sx in sx0..sx1 {
                    let px = row + sx * channels;
                    sums[0] += u64::from(src[px]);
                    sums[1] += u64::from(src[px + 1]);
                    sums[2] += u64::from(src[px + 2]);
                }
            }
            let area = u64::try_from((sy1 - sy0) * (sx1 - sx0)).unwrap_or(1).max(1);
            for sum in sums {
                out.push(u8::try_from((sum + area / 2) / area).unwrap_or(u8::MAX));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Canvas, markers and text
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Rect {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

impl Rect {
    fn contains(&self, x: i64, y: i64) -> bool {
        x >= signed(self.x)
            && y >= signed(self.y)
            && x < signed(self.x + self.w)
            && y < signed(self.y + self.h)
    }
}

/// Geometry of a tap marker: a ring (`inner_sq..=outer_sq` squared radius band) plus four
/// crosshair arms spanning `arm_start..=arm_end` pixels from the centre, over a 1 px black halo.
#[derive(Debug, Clone, Copy)]
struct MarkerStyle {
    ring_inner_sq: i64,
    ring_outer_sq: i64,
    arm_start: i64,
    arm_end: i64,
}

/// Contact-sheet tap marker: bold enough to read on a downscaled tile.
const SHEET_MARKER: MarkerStyle = MarkerStyle {
    ring_inner_sq: 30,
    ring_outer_sq: 42,
    arm_start: 8,
    arm_end: 16,
};

/// Tap-crop marker: 1 px crosshair and tiny ring (radius ~4) around the tapped pixel.
const CROP_MARKER: MarkerStyle = MarkerStyle {
    ring_inner_sq: 13,
    ring_outer_sq: 19,
    arm_start: 7,
    arm_end: 18,
};

struct Canvas {
    width: usize,
    height: usize,
    /// Tightly packed RGB.
    data: Vec<u8>,
}

impl Canvas {
    fn new(width: usize, height: usize, fill: Rgb) -> Result<Self, MediaError> {
        let len = width
            .checked_mul(height)
            .and_then(|px| px.checked_mul(3))
            .ok_or_else(|| MediaError::InvalidArgument("canvas size overflow".to_owned()))?;
        let mut data = Vec::with_capacity(len);
        for _ in 0..width * height {
            data.extend_from_slice(&fill);
        }
        Ok(Self {
            width,
            height,
            data,
        })
    }

    fn put(&mut self, x: i64, y: i64, clip: Rect, color: Rgb) {
        if !clip.contains(x, y) {
            return;
        }
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return;
        };
        if x >= self.width || y >= self.height {
            return;
        }
        let at = (y * self.width + x) * 3;
        self.data[at..at + 3].copy_from_slice(&color);
    }

    fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: Rgb) {
        for row in y..(y + h).min(self.height) {
            for col in x..(x + w).min(self.width) {
                let at = (row * self.width + col) * 3;
                self.data[at..at + 3].copy_from_slice(&color);
            }
        }
    }

    fn blit_rgb(&mut self, rgb: &[u8], w: usize, h: usize, x: usize, y: usize) {
        for row in 0..h {
            if y + row >= self.height {
                break;
            }
            let copy_w = w.min(self.width.saturating_sub(x));
            let dst = ((y + row) * self.width + x) * 3;
            let src = row * w * 3;
            self.data[dst..dst + copy_w * 3].copy_from_slice(&rgb[src..src + copy_w * 3]);
        }
    }

    fn draw_text(&mut self, text: &str, x: usize, y: usize, scale: usize, color: Rgb, clip: Rect) {
        let advance = 6 * scale;
        for (i, ch) in text.chars().enumerate() {
            let gx = x + i * advance;
            for (row, bits) in glyph(ch).iter().enumerate() {
                for col in 0..5usize {
                    if (bits >> (4 - col)) & 1 == 0 {
                        continue;
                    }
                    for sy in 0..scale {
                        for sx in 0..scale {
                            self.put(
                                signed(gx + col * scale + sx),
                                signed(y + row * scale + sy),
                                clip,
                                color,
                            );
                        }
                    }
                }
            }
        }
    }

    fn draw_tap_marker(&mut self, cx: i64, cy: i64, style: &MarkerStyle, clip: Rect) {
        let mut points: Vec<(i64, i64)> = Vec::new();
        let reach = style.ring_outer_sq.max(1);
        let radius = (0..=reach)
            .take_while(|r| r * r <= reach)
            .last()
            .unwrap_or(0)
            + 1;
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                let d2 = dx * dx + dy * dy;
                if d2 >= style.ring_inner_sq && d2 <= style.ring_outer_sq {
                    points.push((cx + dx, cy + dy));
                }
            }
        }
        for k in style.arm_start..=style.arm_end {
            points.extend([(cx + k, cy), (cx - k, cy), (cx, cy + k), (cx, cy - k)]);
        }
        // Black halo first so the cyan stays visible on light and dark content alike.
        for &(px, py) in &points {
            for hy in -1..=1 {
                for hx in -1..=1 {
                    self.put(px + hx, py + hy, clip, MARKER_HALO);
                }
            }
        }
        for &(px, py) in &points {
            self.put(px, py, clip, MARKER_COLOR);
        }
    }
}

/// Formats the tile label, e.g. `#372  00:12.4` (negative timestamps get a leading `-`).
fn format_label(frame_index: u64, timestamp_ms: i64) -> String {
    let abs = timestamp_ms.unsigned_abs();
    let sign = if timestamp_ms < 0 { "-" } else { "" };
    format!(
        "#{frame_index}  {sign}{:02}:{:02}.{}",
        abs / 60_000,
        (abs / 1000) % 60,
        (abs / 100) % 10
    )
}

/// Returns the 5×7 glyph rows (low 5 bits, MSB = leftmost dot) for `ch`. Lowercase maps to
/// uppercase; anything unsupported renders as `?`.
fn glyph(ch: char) -> [u8; 7] {
    match ch.to_ascii_uppercase() {
        ' ' => [0; 7],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1E, 0x01, 0x01, 0x0E, 0x01, 0x01, 0x1E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        ':' => [0x00, 0x04, 0x04, 0x00, 0x04, 0x04, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        '#' => [0x0A, 0x0A, 0x1F, 0x0A, 0x1F, 0x0A, 0x0A],
        '/' => [0x01, 0x01, 0x02, 0x04, 0x08, 0x10, 0x10],
        _ => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04],
    }
}

fn encode_rgb_png(width: usize, height: usize, rgb: &[u8]) -> Result<Vec<u8>, MediaError> {
    let w = u32::try_from(width)
        .map_err(|_| MediaError::Png("PNG width exceeds u32::MAX".to_owned()))?;
    let h = u32::try_from(height)
        .map_err(|_| MediaError::Png("PNG height exceeds u32::MAX".to_owned()))?;
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, w, h);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|err| MediaError::Png(format!("failed to write PNG header: {err}")))?;
    writer
        .write_image_data(rgb)
        .map_err(|err| MediaError::Png(format!("failed to write PNG image data: {err}")))?;
    writer
        .finish()
        .map_err(|err| MediaError::Png(format!("failed to finalize PNG stream: {err}")))?;
    Ok(out)
}
