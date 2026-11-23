//! Integration tests for the contact-sheet / tap-crop PNG engine and frame selector.

use arb_media::MediaError;
use arb_media::contact_sheet::{
    ContactSheetOptions, ContactSheetTile, MAX_CONTACT_SHEET_TILES, TapCropOptions,
    render_contact_sheet_png, render_tap_crop_png, select_contact_sheet_indices,
};

const CYAN: [u8; 3] = [0, 255, 255];

struct DecodedPng {
    width: usize,
    height: usize,
    channels: usize,
    data: Vec<u8>,
}

impl DecodedPng {
    fn px(&self, x: usize, y: usize) -> [u8; 3] {
        let at = (y * self.width + x) * self.channels;
        [self.data[at], self.data[at + 1], self.data[at + 2]]
    }
}

fn decode(png_bytes: &[u8]) -> DecodedPng {
    assert_eq!(&png_bytes[..8], b"\x89PNG\r\n\x1a\n", "PNG signature");
    let mut reader = png::Decoder::new(png_bytes)
        .read_info()
        .expect("valid PNG header");
    let mut data = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut data).expect("valid PNG data");
    assert_eq!(info.bit_depth, png::BitDepth::Eight, "8-bit PNG");
    let channels = match info.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        other => panic!("unexpected PNG color type {other:?}"),
    };
    data.truncate(info.buffer_size());
    DecodedPng {
        width: usize::try_from(info.width).expect("w"),
        height: usize::try_from(info.height).expect("h"),
        channels,
        data,
    }
}

fn solid_rgba(width: u32, height: u32, color: [u8; 3]) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..width * height {
        out.extend_from_slice(&[color[0], color[1], color[2], 255]);
    }
    out
}

fn tile(index: u64, ts_ms: i64, w: u32, h: u32, color: [u8; 3]) -> ContactSheetTile {
    ContactSheetTile {
        frame_index: index,
        timestamp_ms: ts_ms,
        width: w,
        height: h,
        rgba: solid_rgba(w, h, color),
        tap_point: None,
    }
}

fn is_white(px: [u8; 3]) -> bool {
    px == [255, 255, 255]
}

/// Counts pure-white label pixels in the label bar region of cell `(col, row)`.
fn white_pixels_in(png: &DecodedPng, x0: usize, y0: usize, w: usize, h: usize) -> usize {
    let mut n = 0;
    for y in y0..y0 + h {
        for x in x0..x0 + w {
            if is_white(png.px(x, y)) {
                n += 1;
            }
        }
    }
    n
}

// ---- select_contact_sheet_indices -------------------------------------------------------------

#[test]
fn select_indices_edge_cases() {
    assert!(
        select_contact_sheet_indices(0, 100, 0).is_empty(),
        "0 tiles"
    );
    assert_eq!(
        select_contact_sheet_indices(7, 7, 16),
        vec![7],
        "single frame"
    );
    assert_eq!(select_contact_sheet_indices(7, 100, 1), vec![7], "one tile");
    assert_eq!(
        select_contact_sheet_indices(9, 3, 16),
        vec![9],
        "inverted range"
    );
    assert_eq!(
        select_contact_sheet_indices(5, 6, 16),
        vec![5, 6],
        "2 frames"
    );
    assert_eq!(
        select_contact_sheet_indices(0, 100, 2),
        vec![0, 100],
        "2 tiles"
    );
    assert_eq!(
        select_contact_sheet_indices(10, 14, 16),
        vec![10, 11, 12, 13, 14],
        "fewer frames than tiles yields every frame"
    );
}

#[test]
fn select_indices_16_and_1000_frames_are_even_sorted_unique_and_capped() {
    let sixteen = select_contact_sheet_indices(0, 15, 16);
    assert_eq!(sixteen, (0..16).collect::<Vec<u64>>(), "exactly 16 frames");

    let thousand = select_contact_sheet_indices(0, 999, 16);
    assert_eq!(thousand.len(), 16);
    assert_eq!(thousand[0], 0);
    assert_eq!(thousand[15], 999);
    assert!(thousand.windows(2).all(|w| w[0] < w[1]), "sorted + unique");
    let steps: Vec<u64> = thousand.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(
        steps.iter().all(|&s| (66..=67).contains(&s)),
        "evenly spaced (66/67 apart): {steps:?}"
    );

    // Offset ranges keep both ends; requests above 16 are capped.
    let offset = select_contact_sheet_indices(372, 1372, 64);
    assert_eq!(offset.len(), MAX_CONTACT_SHEET_TILES);
    assert_eq!((offset[0], offset[15]), (372, 1372));

    // Deterministic.
    assert_eq!(thousand, select_contact_sheet_indices(0, 999, 16));

    // Extreme range does not overflow.
    let huge = select_contact_sheet_indices(0, u64::MAX, 16);
    assert_eq!((huge[0], huge[15]), (0, u64::MAX));
    assert_eq!(huge.len(), 16);
}

// ---- contact sheet ----------------------------------------------------------------------------

#[test]
fn contact_sheet_single_tile_has_dimensions_label_and_scaled_pixels() {
    let opts = ContactSheetOptions::default();
    let frame = tile(372, 12_400, 560, 280, [200, 30, 30]);
    let bytes = render_contact_sheet_png(std::slice::from_ref(&frame), &opts).expect("render");
    let png = decode(&bytes);

    // 560x280 fits 280 wide => 280x140; cell = 280 x (20 label + 140); gutter 4 around.
    assert_eq!(png.width, 4 + 280 + 4, "sheet width");
    assert_eq!(png.height, 4 + 160 + 4, "sheet height");
    assert_eq!(png.px(0, 0), [36, 36, 44], "gutter colour");

    // Label bar (cell at (4, 4), 20 px tall) carries white text pixels.
    let white = white_pixels_in(&png, 4, 4, 280, 20);
    assert!(
        white > 40,
        "label text must be burned in ({white} white px)"
    );
    // Scaled image: solid red survives box filtering exactly, centre of the image area.
    assert_eq!(png.px(4 + 140, 4 + 20 + 70), [200, 30, 30]);
    // Image area starts directly below the label bar.
    assert_eq!(png.px(4 + 5, 4 + 20 + 2), [200, 30, 30]);
}

#[test]
fn contact_sheet_label_text_depends_on_frame_and_time() {
    let opts = ContactSheetOptions::default();
    let a = decode(
        &render_contact_sheet_png(&[tile(372, 12_400, 100, 100, [0, 0, 0])], &opts).expect("a"),
    );
    let b = decode(
        &render_contact_sheet_png(&[tile(373, 12_400, 100, 100, [0, 0, 0])], &opts).expect("b"),
    );
    assert_ne!(a.data, b.data, "different frame index changes the label");
    let neg = decode(
        &render_contact_sheet_png(&[tile(372, -1_500, 100, 100, [0, 0, 0])], &opts).expect("neg"),
    );
    assert_ne!(a.data, neg.data, "negative timestamp renders a sign");
}

#[test]
fn contact_sheet_four_tiles_form_a_2x2_labelled_grid() {
    let opts = ContactSheetOptions {
        columns: 2,
        ..ContactSheetOptions::default()
    };
    let colors = [[255, 0, 0], [0, 200, 0], [0, 0, 255], [240, 240, 0]];
    let tiles: Vec<ContactSheetTile> = colors
        .iter()
        .enumerate()
        .map(|(i, c)| {
            tile(
                u64::try_from(i).expect("i") * 10,
                1_000 * i64::try_from(i).expect("i"),
                200,
                100,
                *c,
            )
        })
        .collect();
    let png = decode(&render_contact_sheet_png(&tiles, &opts).expect("render"));

    // 200x100 stays 1:1 (below 280): cell 200 x 120, gutter 4, 2 columns x 2 rows.
    assert_eq!(png.width, 4 + 2 * (200 + 4));
    assert_eq!(png.height, 4 + 2 * (120 + 4));
    for (i, color) in colors.iter().enumerate() {
        let (col, row) = (i % 2, i / 2);
        let (cx, cy) = (4 + col * 204, 4 + row * 124);
        assert_eq!(png.px(cx + 100, cy + 20 + 50), *color, "tile {i} pixels");
        assert!(
            white_pixels_in(&png, cx, cy, 200, 20) > 30,
            "tile {i} label burned in"
        );
    }
}

#[test]
fn contact_sheet_sixteen_tiles_form_a_4x4_grid_and_stay_compact() {
    let opts = ContactSheetOptions::default();
    // Portrait phone-like frames (1080x2400 scaled down to a 100x222 stand-in).
    let tiles: Vec<ContactSheetTile> = (0..16u64)
        .map(|i| {
            let shade = u8::try_from(i * 15).expect("shade");
            tile(
                i * 3,
                i64::try_from(i).expect("i") * 450,
                108,
                240,
                [shade, 80, 200],
            )
        })
        .collect();
    let png = decode(&render_contact_sheet_png(&tiles, &opts).expect("render"));

    // 108x240 is below the caps => 1:1; cell 108 x 260.
    assert_eq!(png.width, 4 + 4 * (108 + 4));
    assert_eq!(png.height, 4 + 4 * (260 + 4));
    for i in 0..16usize {
        let (cx, cy) = (4 + (i % 4) * 112, 4 + (i / 4) * 264);
        let shade = u8::try_from(i * 15).expect("shade");
        assert_eq!(png.px(cx + 54, cy + 20 + 120), [shade, 80, 200], "tile {i}");
        assert!(white_pixels_in(&png, cx, cy, 108, 20) > 20, "label {i}");
    }

    // Large frames are downscaled to the caps: 1080x2400 -> height-bound 252x560.
    let big = tile(1, 0, 1080, 2400, [10, 20, 30]);
    let sheet = decode(&render_contact_sheet_png(&[big], &opts).expect("big"));
    assert_eq!(sheet.width, 4 + 252 + 4);
    assert_eq!(sheet.height, 4 + 20 + 560 + 4);
}

#[test]
fn contact_sheet_box_filter_averages_and_accepts_rgb() {
    // 2x1 RGB frame: black and white average to mid grey when scaled to 1x1.
    let frame = ContactSheetTile {
        frame_index: 0,
        timestamp_ms: 0,
        width: 2,
        height: 1,
        rgba: vec![0, 0, 0, 255, 255, 255],
        tap_point: None,
    };
    let opts = ContactSheetOptions::default();
    // Tiny frames are never upscaled and render 1:1.
    let png = decode(&render_contact_sheet_png(&[frame], &opts).expect("rgb"));
    assert_eq!(png.px(4, 4 + 20), [0, 0, 0]);
    assert_eq!(png.px(5, 4 + 20), [255, 255, 255]);

    // 4x4 checker (RGBA) downscaled by a 16..=1024 cap is only reachable with big frames, so
    // verify the averaging with a 32x16 half-black/half-white frame capped to width 16.
    let mut rgba = Vec::new();
    for _ in 0..16 {
        for x in 0..32 {
            let v = if x < 16 { 0 } else { 255 };
            rgba.extend_from_slice(&[v, v, v, 255]);
        }
    }
    let frame = ContactSheetTile {
        frame_index: 0,
        timestamp_ms: 0,
        width: 32,
        height: 16,
        rgba,
        tap_point: None,
    };
    let small = ContactSheetOptions {
        max_tile_width: 16,
        max_tile_height: 16,
        ..ContactSheetOptions::default()
    };
    let png = decode(&render_contact_sheet_png(&[frame], &small).expect("down"));
    assert_eq!((png.width, png.height), (4 + 16 + 4, 4 + 20 + 8 + 4));
    assert_eq!(png.px(4, 4 + 20), [0, 0, 0], "left half stays black");
    assert_eq!(
        png.px(4 + 15, 4 + 20),
        [255, 255, 255],
        "right half stays white"
    );
}

#[test]
fn contact_sheet_draws_cyan_tap_marker_only_when_requested() {
    let opts = ContactSheetOptions::default();
    let plain = tile(5, 500, 280, 280, [90, 90, 90]);
    let mut tapped = plain.clone();
    tapped.tap_point = Some((140, 140));

    let no_marker = decode(&render_contact_sheet_png(&[plain], &opts).expect("plain"));
    let marker = decode(&render_contact_sheet_png(&[tapped], &opts).expect("tapped"));
    let count_cyan = |p: &DecodedPng| {
        (0..p.height)
            .flat_map(|y| (0..p.width).map(move |x| (x, y)))
            .filter(|&(x, y)| p.px(x, y) == CYAN)
            .count()
    };
    assert_eq!(count_cyan(&no_marker), 0, "no marker without tap_point");
    assert!(count_cyan(&marker) > 40, "ring + crosshair drawn");
    // Centre of a 280x280 tile (cell at 4,4; image below the 20 px label bar), 1:1 scale.
    let (cx, cy) = (4 + 140, 4 + 20 + 140);
    assert_eq!(marker.px(cx + 12, cy), CYAN, "right crosshair arm");
    assert_eq!(marker.px(cx, cy - 12), CYAN, "upper crosshair arm");
    assert_eq!(
        marker.px(cx, cy),
        [90, 90, 90],
        "tapped pixel stays visible"
    );
}

#[test]
fn contact_sheet_marker_at_frame_corner_is_clipped_inside_the_tile() {
    let opts = ContactSheetOptions::default();
    let mut corner = tile(1, 0, 100, 100, [60, 60, 60]);
    corner.tap_point = Some((0, 0));
    let png = decode(&render_contact_sheet_png(&[corner], &opts).expect("corner"));
    // Nothing leaks into the gutter or label bar.
    assert_eq!(png.px(2, 2), [36, 36, 44], "gutter untouched");
    assert!(
        !(0..100).any(|x| png.px(4 + x, 4 + 5) == CYAN),
        "label bar untouched"
    );
}

#[test]
fn contact_sheet_is_deterministic() {
    let opts = ContactSheetOptions::default();
    let tiles: Vec<ContactSheetTile> = (0..5u64)
        .map(|i| tile(i, 1000, 640, 360, [u8::try_from(i * 40).expect("c"), 9, 9]))
        .collect();
    assert_eq!(
        render_contact_sheet_png(&tiles, &opts).expect("a"),
        render_contact_sheet_png(&tiles, &opts).expect("b")
    );
}

#[test]
fn contact_sheet_rejects_invalid_input() {
    let opts = ContactSheetOptions::default();
    let invalid = |r: Result<Vec<u8>, MediaError>| {
        assert!(
            matches!(r, Err(MediaError::InvalidArgument(_))),
            "got {r:?}"
        );
    };
    invalid(render_contact_sheet_png(&[], &opts));
    let seventeen: Vec<ContactSheetTile> = (0..17).map(|i| tile(i, 0, 8, 8, [0, 0, 0])).collect();
    invalid(render_contact_sheet_png(&seventeen, &opts));
    invalid(render_contact_sheet_png(
        &[tile(0, 0, 0, 8, [0, 0, 0])],
        &opts,
    ));
    let mut short = tile(0, 0, 8, 8, [0, 0, 0]);
    short.rgba.pop();
    invalid(render_contact_sheet_png(&[short], &opts));
    let mut outside = tile(0, 0, 8, 8, [0, 0, 0]);
    outside.tap_point = Some((8, 0));
    invalid(render_contact_sheet_png(&[outside], &opts));
    invalid(render_contact_sheet_png(
        &[tile(0, 0, 8, 8, [0, 0, 0])],
        &ContactSheetOptions {
            max_tile_width: 0,
            ..opts
        },
    ));
    invalid(render_contact_sheet_png(
        &[tile(0, 0, 8, 8, [0, 0, 0])],
        &ContactSheetOptions { columns: 0, ..opts },
    ));
}

// ---- tap crop ---------------------------------------------------------------------------------

fn gradient_frame(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            out.extend_from_slice(&[
                u8::try_from(x % 200).expect("r"),
                u8::try_from(y % 200).expect("g"),
                50,
                255,
            ]);
        }
    }
    out
}

#[test]
fn tap_crop_center_tap_has_resolution_relative_size_and_crosshair() {
    let (w, h) = (1080u32, 2400u32);
    let frame = gradient_frame(w, h);
    let result =
        render_tap_crop_png(&frame, w, h, 540, 1200, &TapCropOptions::default()).expect("crop");

    // 0.28 * 1080 = 302.4 -> 302; 0.12 * 2400 = 288.
    assert_eq!((result.crop_width, result.crop_height), (302, 288));
    assert_eq!((result.crop_x, result.crop_y), (540 - 151, 1200 - 144));
    assert_eq!((result.tap_rel_x, result.tap_rel_y), (151, 144));

    let png = decode(&result.png_bytes);
    assert_eq!((png.width, png.height), (302, 288));
    let (rx, ry) = (151usize, 144usize);
    assert_eq!(png.px(rx + 4, ry), CYAN, "ring pixel right of the tap");
    assert_eq!(png.px(rx, ry + 4), CYAN, "ring pixel below the tap");
    assert_eq!(png.px(rx + 10, ry), CYAN, "crosshair arm right");
    assert_eq!(png.px(rx, ry - 10), CYAN, "crosshair arm up");
    // The tapped pixel itself keeps its source colour (x=540 -> 140, y=1200 -> 0, b=50).
    assert_eq!(png.px(rx, ry), [140, 0, 50], "tapped pixel stays visible");
    // Pixels far from the marker are the untouched source (native resolution, no scaling).
    assert_eq!(
        png.px(0, 0),
        [
            u8::try_from(389 % 200).expect("r"),
            u8::try_from(1056 % 200).expect("g"),
            50
        ]
    );
    assert_eq!(
        png.px(301, 287),
        [
            u8::try_from(690 % 200).expect("r"),
            u8::try_from(1343 % 200).expect("g"),
            50
        ]
    );
}

#[test]
fn tap_crop_edges_and_corners_are_clamped_inside_the_frame() {
    let (w, h) = (1080u32, 2400u32);
    let frame = gradient_frame(w, h);
    let opts = TapCropOptions::default();

    for (tx, ty) in [
        (0, 0),
        (w - 1, h - 1),
        (0, h - 1),
        (w - 1, 0),
        (5, 1200),
        (540, 0),
    ] {
        let r = render_tap_crop_png(&frame, w, h, tx, ty, &opts).expect("edge crop");
        assert!(
            r.crop_x + r.crop_width <= w,
            "x within frame for ({tx}, {ty})"
        );
        assert!(
            r.crop_y + r.crop_height <= h,
            "y within frame for ({tx}, {ty})"
        );
        assert_eq!(r.tap_rel_x, tx - r.crop_x);
        assert_eq!(r.tap_rel_y, ty - r.crop_y);
        assert!(r.tap_rel_x < r.crop_width && r.tap_rel_y < r.crop_height);
        let png = decode(&r.png_bytes);
        assert_eq!(
            (png.width, png.height),
            (
                usize::try_from(r.crop_width).expect("w"),
                usize::try_from(r.crop_height).expect("h")
            )
        );
    }

    let top_left = render_tap_crop_png(&frame, w, h, 0, 0, &opts).expect("top-left");
    assert_eq!((top_left.crop_x, top_left.crop_y), (0, 0));
    assert_eq!((top_left.tap_rel_x, top_left.tap_rel_y), (0, 0));
    let png = decode(&top_left.png_bytes);
    // Marker at the corner is clipped, never out of bounds; arms inside the crop remain.
    assert_eq!(png.px(10, 0), CYAN, "arm along the top edge");
    assert_eq!(png.px(0, 10), CYAN, "arm along the left edge");

    let bottom_right = render_tap_crop_png(&frame, w, h, w - 1, h - 1, &opts).expect("br");
    assert_eq!(bottom_right.crop_x + bottom_right.crop_width, w);
    assert_eq!(bottom_right.crop_y + bottom_right.crop_height, h);
    assert_eq!(bottom_right.tap_rel_x, bottom_right.crop_width - 1);
    assert_eq!(bottom_right.tap_rel_y, bottom_right.crop_height - 1);
}

#[test]
fn tap_crop_small_frames_use_minimums_and_never_exceed_the_frame() {
    let frame = gradient_frame(40, 30);
    let r = render_tap_crop_png(&frame, 40, 30, 39, 29, &TapCropOptions::default())
        .expect("tiny frame");
    assert_eq!(
        (r.crop_width, r.crop_height),
        (40, 30),
        "capped at the frame"
    );
    assert_eq!((r.crop_x, r.crop_y), (0, 0));

    let one =
        render_tap_crop_png(&[9, 8, 7, 255], 1, 1, 0, 0, &TapCropOptions::default()).expect("1x1");
    assert_eq!((one.crop_width, one.crop_height), (1, 1));
    decode(&one.png_bytes);

    // Large frames: fractions apply, minimums only raise tiny results.
    let wide = gradient_frame(400, 100);
    let r =
        render_tap_crop_png(&wide, 400, 100, 200, 50, &TapCropOptions::default()).expect("wide");
    assert_eq!(
        (r.crop_width, r.crop_height),
        (112, 64),
        "0.28*400, min 64 high"
    );
}

#[test]
fn tap_crop_accepts_rgb_and_rejects_invalid_input() {
    let rgb: Vec<u8> = (0..30 * 20 * 3)
        .map(|i| u8::try_from(i % 251).expect("b"))
        .collect();
    let r = render_tap_crop_png(&rgb, 30, 20, 15, 10, &TapCropOptions::default()).expect("rgb");
    assert_eq!((r.crop_width, r.crop_height), (30, 20));

    let frame = gradient_frame(100, 100);
    let opts = TapCropOptions::default();
    let invalid = |r: Result<arb_media::TapCropResult, MediaError>| {
        assert!(
            matches!(r, Err(MediaError::InvalidArgument(_))),
            "got {r:?}"
        );
    };
    invalid(render_tap_crop_png(&frame, 100, 100, 100, 0, &opts));
    invalid(render_tap_crop_png(&frame, 100, 100, 0, 100, &opts));
    invalid(render_tap_crop_png(&frame[1..], 100, 100, 0, 0, &opts));
    invalid(render_tap_crop_png(&frame, 0, 100, 0, 0, &opts));
    for bad in [0.0f32, -0.1, 1.5, f32::NAN, f32::INFINITY] {
        invalid(render_tap_crop_png(
            &frame,
            100,
            100,
            50,
            50,
            &TapCropOptions {
                width_fraction: bad,
                ..opts
            },
        ));
        invalid(render_tap_crop_png(
            &frame,
            100,
            100,
            50,
            50,
            &TapCropOptions {
                height_fraction: bad,
                ..opts
            },
        ));
    }
}

/// Writes a 16-tile contact sheet and a tap crop of a synthetic phone-like UI into
/// `$ARB_VER14_OUT_DIR` for visual inspection (VER-14). Run explicitly with `--ignored`.
#[test]
#[ignore = "driver for the manual visual inspection in VER-14"]
fn emit_contact_sheet_samples_for_visual_inspection() {
    let out_dir =
        std::path::PathBuf::from(std::env::var("ARB_VER14_OUT_DIR").expect("ARB_VER14_OUT_DIR"));
    std::fs::create_dir_all(&out_dir).expect("out dir");
    let (fw, fh) = (540u32, 1200u32);
    let ui = |seed: u32| -> Vec<u8> {
        let mut out = Vec::new();
        for y in 0..fh {
            for x in 0..fw {
                let button = (140..400).contains(&x) && (520..620).contains(&y);
                let bar = y < 80 || y > fh - 120;
                let (red, green, blue) = if button {
                    (30, 110 + u8::try_from(seed % 100).expect("seed"), 220)
                } else if bar {
                    (24, 24, 32)
                } else {
                    let v = 235 - u8::try_from((y / 12 + x / 20 + seed) % 40).expect("v");
                    (v, v, v)
                };
                out.extend_from_slice(&[red, green, blue, 255]);
            }
        }
        out
    };
    let indices = select_contact_sheet_indices(120, 640, 16);
    let tiles: Vec<ContactSheetTile> = indices
        .iter()
        .enumerate()
        .map(|(i, &frame)| ContactSheetTile {
            frame_index: frame,
            timestamp_ms: i64::try_from(frame).expect("frame") * 33,
            width: fw,
            height: fh,
            rgba: ui(u32::try_from(i).expect("i")),
            tap_point: (i == 7).then_some((270, 570)),
        })
        .collect();
    let sheet = render_contact_sheet_png(&tiles, &ContactSheetOptions::default()).expect("sheet");
    std::fs::write(out_dir.join("contact_sheet.png"), sheet).expect("write sheet");
    let crop = render_tap_crop_png(&ui(3), fw, fh, 270, 570, &TapCropOptions::default())
        .expect("tap crop");
    println!(
        "tap crop: {:?}",
        (
            crop.crop_x,
            crop.crop_y,
            crop.crop_width,
            crop.crop_height,
            crop.tap_rel_x,
            crop.tap_rel_y
        )
    );
    std::fs::write(out_dir.join("tap_crop.png"), crop.png_bytes).expect("write crop");
}
