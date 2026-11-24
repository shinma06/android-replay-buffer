//! Big-Endian stream parsers for Apache-2.0 `scrcpy-server-v5.0.1` video and audio tunnels.
//!
//! Video tunnel framing (`device-agent/PROTOCOL.md` §1.2):
//! 1. 1-byte dummy byte (`0x00`) on the first socket.
//! 2. 4-byte codec ID (`0x68323634` = `"h264"`).
//! 3. Repeating Big-Endian records:
//!    - `high: i32 BE < 0` (`0x8000_0000` / `0x8000_0001`): session metadata (`width: i32 BE`, `height: i32 BE`).
//!    - `high: i32 BE >= 0`: frame packet — `low: u32 BE`, `length: i32 BE` (`1..=16 MiB`),
//!      followed by `length` raw H.264 Annex-B bytes (`bit 62 = config`, `bit 61 = keyframe`,
//!      low 61 bits = `CLOCK_MONOTONIC` PTS in microseconds).
//!
//! Audio tunnel framing (second socket when `audio=true`, Android API >= 33):
//! 1. 4-byte codec ID (`0x00616163` = AAC, `0` = disabled/refused, `1` = error).
//! 2. Repeating Big-Endian records: `flags: u64 BE`, `length: i32 BE` (`1..=64 KiB`),
//!    followed by `length` AAC-LC payload bytes (`bit 62 = config`, low 61 bits = PTS µs).

use std::io::{ErrorKind, Read, Write};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Supported `scrcpy-server` version deployed by ARB.
pub const SCRCPY_SERVER_VERSION: &str = "5.0.1";

/// Leading dummy handshake byte transmitted on the first `scrcpy` forward socket.
pub const SCRCPY_DUMMY_BYTE: u8 = 0x00;

/// `FourCC` codec ID for H.264 (`"h264"` = `0x6832_3634`).
pub const CODEC_ID_H264: u32 = 0x6832_3634;

/// `FourCC` codec ID for AAC (`"\0aac"` = `0x0061_6163`).
pub const CODEC_ID_AAC: u32 = 0x0061_6163;

/// `scrcpy` audio codec sentinel indicating playback audio was disabled or refused.
pub const AUDIO_CODEC_DISABLED: u32 = 0;

/// `scrcpy` audio codec sentinel indicating device-side audio initialization error.
pub const AUDIO_CODEC_ERROR: u32 = 1;

/// Official minimum supported Android API level (Android 9 / API 28).
///
/// Devices below this floor are NEVER hard-blocked; capture proceeds best-effort with `audio=false`
/// alongside an honest non-blocking diagnostic warning.
pub const MIN_SUPPORTED_ANDROID_API_LEVEL: u32 = 28;

/// Minimum Android API level at which upstream `scrcpy-server` can capture audio at all
/// (Android 11 / API 30). ARB only captures audio from [`MIN_AUDIO_PLAYBACK_API_LEVEL`],
/// because below it capture would mute the device's own playback.
pub const MIN_AUDIO_SCRCPY_API_LEVEL: u32 = 30;

/// Minimum Android API level required for non-muting `audio_source=playback audio_dup=true` (Android 13 / API 33).
pub const MIN_AUDIO_PLAYBACK_API_LEVEL: u32 = 33;

/// Maximum permitted H.264 video packet byte length (`16 MiB`).
pub const MAX_VIDEO_PACKET_LEN: usize = 16 * 1024 * 1024;

/// Maximum permitted AAC audio packet byte length (`64 KiB`).
pub const MAX_AUDIO_PACKET_LEN: usize = 64 * 1024;

/// Bit 62 in `scrcpy` 5.0.1 packet flags marks a codec configuration packet (SPS/PPS or ASC).
pub const PACKET_FLAG_CONFIG: u64 = 1u64 << 62;

/// Bit 61 in `scrcpy` 5.0.1 packet flags marks a video IDR keyframe.
pub const PACKET_FLAG_KEYFRAME: u64 = 1u64 << 61;

/// Low 61-bit mask extracting `CLOCK_MONOTONIC` presentation timestamp in microseconds.
pub const PACKET_PTS_US_MASK: u64 = (1u64 << 61) - 1;

/// Standard `scrcpy` 5.0.1 session metadata high-word discriminator (`0x8000_0000`).
pub const SESSION_META_TAG: u32 = 0x8000_0000;

/// Errors returned while handshaking with or parsing `scrcpy-server-v5.0.1` tunnels.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum ScrcpyError {
    /// Underlying socket or buffer I/O error.
    #[error("scrcpy tunnel I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Stream hit EOF in the middle of a handshake or packet record.
    #[error("unexpected EOF while reading {context}")]
    UnexpectedEof {
        /// Human-readable location in the framing state machine.
        context: &'static str,
    },
    /// First byte on the video tunnel was not `0x00`.
    #[error("invalid scrcpy dummy handshake byte: expected 0x00, got 0x{found:02x}")]
    InvalidDummyByte {
        /// Observed leading byte.
        found: u8,
    },
    /// Video tunnel negotiated an unsupported codec ID (only `h264` is supported).
    #[error("unsupported scrcpy video codec id: 0x{codec_id:08x} (expected 0x{CODEC_ID_H264:08x})")]
    UnsupportedVideoCodec {
        /// Observed 4-byte Big-Endian codec ID.
        codec_id: u32,
    },
    /// Audio tunnel negotiated an unsupported codec ID.
    #[error("unsupported scrcpy audio codec id: 0x{codec_id:08x} (expected 0x{CODEC_ID_AAC:08x})")]
    UnsupportedAudioCodec {
        /// Observed 4-byte Big-Endian codec ID.
        codec_id: u32,
    },
    /// Audio tunnel explicitly reported disabled/refused (`codec_id == 0`).
    #[error("scrcpy audio stream disabled or refused by device (codec_id = 0)")]
    AudioDisabled,
    /// Audio tunnel explicitly reported an initialization error (`codec_id == 1`).
    #[error("scrcpy audio stream failed to initialize on device (codec_id = 1)")]
    AudioServerError,
    /// Device Android API level is below 33 so playback audio duplication is unavailable.
    #[error(
        "playback audio capture unavailable on Android API {api_level} (requires API >= {min_required})"
    )]
    AudioUnavailableApiLevel {
        /// Reported device `Build.VERSION.SDK_INT`.
        api_level: u32,
        /// Minimum required API level (`33`).
        min_required: u32,
    },
    /// Video session metadata contained non-positive width or height.
    #[error("invalid scrcpy video dimensions: {width}x{height}")]
    InvalidSessionDimensions {
        /// Signed width from stream.
        width: i32,
        /// Signed height from stream.
        height: i32,
    },
    /// Packet length was `<= 0` or exceeded the per-stream hard cap.
    #[error("invalid scrcpy {stream} packet length {length} (allowed 1..={max_length})")]
    InvalidPacketLength {
        /// Stream label (`"video"` or `"audio"`).
        stream: &'static str,
        /// Declared signed 32-bit length from stream.
        length: i32,
        /// Maximum permitted length in bytes.
        max_length: usize,
    },
}

/// Explicit outcome of probing audio capture availability without failing other streams.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioTunnelStatus {
    /// Audio tunnel negotiated AAC-LC (`0x0061_6163`).
    Available {
        /// Negotiated 4-byte codec ID.
        codec_id: u32,
    },
    /// Device refused or disabled audio (`codec_id == 0`).
    DisabledByDevice,
    /// Device audio capture encountered an error (`codec_id == 1`).
    DeviceError,
    /// Device Android API level is `< 33`; video/logcat/input proceed unaffected.
    UnavailableApiLevel {
        /// Observed Android API level.
        api_level: u32,
        /// Minimum API level required (`33`).
        min_required: u32,
    },
}

/// Checks whether playback audio duplication (`audio_source=playback audio_dup=true`)
/// is supported on `api_level`.
///
/// # Errors
///
/// Returns [`ScrcpyError::AudioUnavailableApiLevel`] when `api_level < 33`.
pub const fn check_audio_availability(api_level: u32) -> Result<(), ScrcpyError> {
    if api_level < MIN_AUDIO_PLAYBACK_API_LEVEL {
        return Err(ScrcpyError::AudioUnavailableApiLevel {
            api_level,
            min_required: MIN_AUDIO_PLAYBACK_API_LEVEL,
        });
    }
    Ok(())
}

/// Returns a non-blocking diagnostic warning when `api_level` is below the official
/// Android 9 (API 28) support floor (`0 < api_level < 28`), or `None` for API 28+ or unknown (`0`).
#[must_use]
pub fn unsupported_android_api_warning(api_level: u32) -> Option<String> {
    (api_level > 0 && api_level < MIN_SUPPORTED_ANDROID_API_LEVEL).then(|| {
        format!(
            "Android 9 (API 28) 未満は非サポート対象です (API {api_level})。ベストエフォートで音声なしキャプチャを継続します。"
        )
    })
}

/// Returns an honest, user-facing diagnostic reason when audio capture must be proactively
/// disabled for `api_level`, or `None` when `api_level == 0` (unknown) or `>= 33`.
#[must_use]
pub fn audio_unavailable_reason_for_api(api_level: u32) -> Option<String> {
    if api_level == 0 {
        return None;
    }
    // `scrcpy-server` can capture audio from API 30, but only `audio_source=playback
    // audio_dup=true` (API 33+) keeps the device's own audio playing, so ARB never captures
    // audio below API 33 and the reason must not suggest Android 11/12 would.
    (api_level < MIN_AUDIO_PLAYBACK_API_LEVEL).then(|| {
        format!(
            "Audio capture requires Android 13+ (API 33+, non-muting playback capture); detected API {api_level}. Continuing with video + logs + taps."
        )
    })
}

/// Video session metadata header emitted before video packets and on rotation/resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VideoSessionMeta {
    /// Raw high 32-bit header tag (bit 31 set, e.g., `0x8000_0000` or `0x8000_0001`).
    pub header_tag: u32,
    /// Video frame width in pixels.
    pub width: u32,
    /// Video frame height in pixels.
    pub height: u32,
}

/// Classification of a decoded H.264 `scrcpy` video packet.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoPacketKind {
    /// Out-of-band / leading H.264 Annex-B SPS/PPS configuration packet (`bit 62`).
    Config,
    /// H.264 IDR sync keyframe (`bit 61`).
    Keyframe,
    /// Non-IDR inter-predicted (P) frame.
    DeltaFrame,
}

/// One parsed H.264 Annex-B video packet from the `scrcpy` 5.0.1 video tunnel.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VideoPacket {
    /// Packet role (config, keyframe, or delta frame).
    pub kind: VideoPacketKind,
    /// Raw device `CLOCK_MONOTONIC` presentation timestamp in microseconds (`0` for config).
    pub pts_us: u64,
    /// Raw H.264 Annex-B byte payload (`1..=16 MiB`).
    pub data: Vec<u8>,
}

impl VideoPacket {
    /// Returns `true` if this packet carries codec configuration (SPS/PPS).
    pub const fn is_config(&self) -> bool {
        matches!(self.kind, VideoPacketKind::Config)
    }

    /// Returns `true` if this packet is an IDR keyframe.
    pub const fn is_keyframe(&self) -> bool {
        matches!(self.kind, VideoPacketKind::Keyframe)
    }

    /// Converts `pts_us` into device `CLOCK_MONOTONIC` nanoseconds.
    pub fn pts_monotonic_ns(&self) -> Option<i64> {
        let ns = self.pts_us.checked_mul(1_000)?;
        i64::try_from(ns).ok()
    }
}

/// Record yielded by [`VideoTunnelReader::read_next`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VideoRecord {
    /// Stream geometry update (`width`, `height`).
    SessionMeta(VideoSessionMeta),
    /// Encoded H.264 Annex-B packet.
    Packet(VideoPacket),
}

/// Synchronous reader for a `scrcpy-server-v5.0.1` Big-Endian H.264 video tunnel.
#[derive(Debug)]
pub struct VideoTunnelReader<R> {
    reader: R,
    codec_id: u32,
    dimensions: Option<(u32, u32)>,
}

impl<R: Read> VideoTunnelReader<R> {
    /// Performs the full `scrcpy` 5.0.1 video handshake (1-byte `0x00` dummy + 4-byte `"h264"`).
    ///
    /// # Errors
    ///
    /// Returns [`ScrcpyError`] if the dummy byte is not `0x00`, the codec ID is not
    /// `"h264"`, EOF occurs early, or reading fails.
    pub fn handshake(mut reader: R) -> Result<Self, ScrcpyError> {
        let mut dummy = [0u8; 1];
        read_exact_ctx(&mut reader, &mut dummy, "scrcpy video dummy byte")?;
        if dummy[0] != SCRCPY_DUMMY_BYTE {
            return Err(ScrcpyError::InvalidDummyByte { found: dummy[0] });
        }
        Self::handshake_without_dummy(reader)
    }

    /// Performs the 4-byte `"h264"` codec handshake when the leading dummy byte was already consumed.
    ///
    /// # Errors
    ///
    /// Returns [`ScrcpyError`] if the codec ID is not `"h264"` or reading fails.
    pub fn handshake_without_dummy(mut reader: R) -> Result<Self, ScrcpyError> {
        let mut codec_bytes = [0u8; 4];
        read_exact_ctx(&mut reader, &mut codec_bytes, "scrcpy video codec id")?;
        let codec_id = u32::from_be_bytes(codec_bytes);
        if codec_id != CODEC_ID_H264 {
            return Err(ScrcpyError::UnsupportedVideoCodec { codec_id });
        }
        Ok(Self {
            reader,
            codec_id,
            dimensions: None,
        })
    }

    /// Returns the negotiated 4-byte codec ID (`CODEC_ID_H264`).
    pub const fn codec_id(&self) -> u32 {
        self.codec_id
    }

    /// Returns the most recently observed `(width, height)` session dimensions, if any.
    pub const fn current_dimensions(&self) -> Option<(u32, u32)> {
        self.dimensions
    }

    /// Consumes the reader and returns the underlying transport.
    pub fn into_inner(self) -> R {
        self.reader
    }

    /// Reads the next [`VideoRecord`], returning `Ok(None)` on clean EOF at a record boundary.
    ///
    /// # Errors
    ///
    /// Returns [`ScrcpyError`] on mid-record EOF, invalid dimensions, packet lengths
    /// outside `1..=16 MiB`, or underlying I/O failure.
    pub fn read_next(&mut self) -> Result<Option<VideoRecord>, ScrcpyError> {
        let mut high_bytes = [0u8; 4];
        let Some(()) = read_exact_or_clean_eof(
            &mut self.reader,
            &mut high_bytes,
            "scrcpy video record header",
        )?
        else {
            return Ok(None);
        };

        let high_signed = i32::from_be_bytes(high_bytes);
        if high_signed < 0 {
            let mut dim_bytes = [0u8; 8];
            read_exact_ctx(
                &mut self.reader,
                &mut dim_bytes,
                "scrcpy video session dimensions",
            )?;
            let width_i32 =
                i32::from_be_bytes([dim_bytes[0], dim_bytes[1], dim_bytes[2], dim_bytes[3]]);
            let height_i32 =
                i32::from_be_bytes([dim_bytes[4], dim_bytes[5], dim_bytes[6], dim_bytes[7]]);
            if width_i32 <= 0 || height_i32 <= 0 {
                return Err(ScrcpyError::InvalidSessionDimensions {
                    width: width_i32,
                    height: height_i32,
                });
            }
            let width =
                u32::try_from(width_i32).map_err(|_| ScrcpyError::InvalidSessionDimensions {
                    width: width_i32,
                    height: height_i32,
                })?;
            let height =
                u32::try_from(height_i32).map_err(|_| ScrcpyError::InvalidSessionDimensions {
                    width: width_i32,
                    height: height_i32,
                })?;
            self.dimensions = Some((width, height));
            return Ok(Some(VideoRecord::SessionMeta(VideoSessionMeta {
                header_tag: u32::from_be_bytes(high_bytes),
                width,
                height,
            })));
        }

        let mut tail = [0u8; 8];
        read_exact_ctx(&mut self.reader, &mut tail, "scrcpy video packet header")?;
        let high_u64 = u64::from(u32::from_be_bytes(high_bytes));
        let low_u64 = u64::from(u32::from_be_bytes([tail[0], tail[1], tail[2], tail[3]]));
        let flags = (high_u64 << 32) | low_u64;
        let length_i32 = i32::from_be_bytes([tail[4], tail[5], tail[6], tail[7]]);
        let length = validate_packet_len("video", length_i32, MAX_VIDEO_PACKET_LEN)?;

        let mut data = vec![0u8; length];
        read_exact_ctx(&mut self.reader, &mut data, "scrcpy video packet payload")?;

        let kind = if (flags & PACKET_FLAG_CONFIG) != 0 {
            VideoPacketKind::Config
        } else if (flags & PACKET_FLAG_KEYFRAME) != 0 {
            VideoPacketKind::Keyframe
        } else {
            VideoPacketKind::DeltaFrame
        };
        let pts_us = flags & PACKET_PTS_US_MASK;

        Ok(Some(VideoRecord::Packet(VideoPacket {
            kind,
            pts_us,
            data,
        })))
    }
}

/// Whether an AAC audio packet carries codec config (`AudioSpecificConfig`) or audio frames.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioPacketKind {
    /// AAC `AudioSpecificConfig` packet (`bit 62` set).
    Config,
    /// Raw AAC-LC access unit payload.
    Frame,
}

/// One parsed AAC-LC packet from the `scrcpy` 5.0.1 audio tunnel.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AudioPacket {
    /// Packet role (config or audio frame).
    pub kind: AudioPacketKind,
    /// Raw device `CLOCK_MONOTONIC` presentation timestamp in microseconds (`0` for config).
    pub pts_us: u64,
    /// Raw AAC-LC payload bytes (`1..=64 KiB`).
    pub data: Vec<u8>,
}

impl AudioPacket {
    /// Returns `true` if this packet carries `AudioSpecificConfig`.
    pub const fn is_config(&self) -> bool {
        matches!(self.kind, AudioPacketKind::Config)
    }

    /// Converts `pts_us` into device `CLOCK_MONOTONIC` nanoseconds.
    pub fn pts_monotonic_ns(&self) -> Option<i64> {
        let ns = self.pts_us.checked_mul(1_000)?;
        i64::try_from(ns).ok()
    }
}

/// Synchronous reader for a `scrcpy-server-v5.0.1` Big-Endian AAC-LC audio tunnel.
#[derive(Debug)]
pub struct AudioTunnelReader<R> {
    reader: R,
    codec_id: u32,
}

impl<R: Read> AudioTunnelReader<R> {
    /// Performs the 4-byte `scrcpy` audio codec handshake.
    ///
    /// # Errors
    ///
    /// Returns [`ScrcpyError::AudioDisabled`] for codec ID `0`,
    /// [`ScrcpyError::AudioServerError`] for codec ID `1`,
    /// [`ScrcpyError::UnsupportedAudioCodec`] for any other non-AAC codec ID,
    /// or I/O errors.
    pub fn handshake(mut reader: R) -> Result<Self, ScrcpyError> {
        let mut codec_bytes = [0u8; 4];
        read_exact_ctx(&mut reader, &mut codec_bytes, "scrcpy audio codec id")?;
        let codec_id = u32::from_be_bytes(codec_bytes);
        match codec_id {
            CODEC_ID_AAC => Ok(Self { reader, codec_id }),
            AUDIO_CODEC_DISABLED => Err(ScrcpyError::AudioDisabled),
            AUDIO_CODEC_ERROR => Err(ScrcpyError::AudioServerError),
            other => Err(ScrcpyError::UnsupportedAudioCodec { codec_id: other }),
        }
    }

    /// Validates `api_level >= 33` before performing the audio tunnel handshake.
    ///
    /// # Errors
    ///
    /// Returns [`ScrcpyError::AudioUnavailableApiLevel`] when `api_level < 33`
    /// without reading from `reader`, or any error returned by [`Self::handshake`].
    pub fn handshake_for_api(reader: R, api_level: u32) -> Result<Self, ScrcpyError> {
        check_audio_availability(api_level)?;
        Self::handshake(reader)
    }

    /// Returns the negotiated 4-byte codec ID (`CODEC_ID_AAC`).
    pub const fn codec_id(&self) -> u32 {
        self.codec_id
    }

    /// Consumes the reader and returns the underlying transport.
    pub fn into_inner(self) -> R {
        self.reader
    }

    /// Reads the next [`AudioPacket`], returning `Ok(None)` on clean EOF at a record boundary.
    ///
    /// # Errors
    ///
    /// Returns [`ScrcpyError`] on mid-record EOF, packet length outside `1..=64 KiB`,
    /// or underlying I/O failure.
    pub fn read_next(&mut self) -> Result<Option<AudioPacket>, ScrcpyError> {
        let mut hdr = [0u8; 12];
        let Some(()) =
            read_exact_or_clean_eof(&mut self.reader, &mut hdr, "scrcpy audio packet header")?
        else {
            return Ok(None);
        };
        let flags = u64::from_be_bytes([
            hdr[0], hdr[1], hdr[2], hdr[3], hdr[4], hdr[5], hdr[6], hdr[7],
        ]);
        let length_i32 = i32::from_be_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
        let length = validate_packet_len("audio", length_i32, MAX_AUDIO_PACKET_LEN)?;

        let mut data = vec![0u8; length];
        read_exact_ctx(&mut self.reader, &mut data, "scrcpy audio packet payload")?;

        let kind = if (flags & PACKET_FLAG_CONFIG) != 0 {
            AudioPacketKind::Config
        } else {
            AudioPacketKind::Frame
        };
        let pts_us = flags & PACKET_PTS_US_MASK;
        Ok(Some(AudioPacket { kind, pts_us, data }))
    }
}

/// Whether audio capture should be requested when launching `scrcpy-server`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioCaptureMode {
    /// Request `audio=true audio_source=playback audio_dup=true` on API >= 33.
    Enabled,
    /// Disable audio (`audio=false`) regardless of API level.
    Disabled,
}

/// Launch configuration for `com.genymobile.scrcpy.Server 5.0.1`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScrcpyLaunchOptions {
    /// 8-character lowercase hexadecimal `scid` naming `localabstract:scrcpy_<scid>`.
    pub scid: String,
    /// Maximum encoded video dimension (`1920` or `1024`).
    pub max_size: u32,
    /// Target maximum frame rate (`30` or `15`).
    pub max_fps: u32,
    /// Target H.264 bitrate in bits per second.
    pub video_bit_rate: u32,
    /// Target AAC bitrate in bits per second (`128_000` default).
    pub audio_bit_rate: u32,
    /// Requested audio capture mode.
    pub audio_mode: AudioCaptureMode,
}

impl ScrcpyLaunchOptions {
    /// Creates standard SPEC-03 FHD 30fps options with playback audio enabled.
    pub fn fhd30(scid: impl Into<String>) -> Self {
        Self {
            scid: scid.into(),
            max_size: 1920,
            max_fps: 30,
            video_bit_rate: 8_000_000,
            audio_bit_rate: 128_000,
            audio_mode: AudioCaptureMode::Enabled,
        }
    }

    /// Builds `com.genymobile.scrcpy.Server 5.0.1` arguments for a device at `api_level`.
    ///
    /// When `audio_mode` is [`AudioCaptureMode::Enabled`] on API < 33, audio is
    /// downgraded cleanly to `audio=false` and [`AudioTunnelStatus::UnavailableApiLevel`]
    /// is returned so video, logcat, and input capture proceed uninterrupted.
    pub fn build_server_args(&self, api_level: u32) -> (Vec<String>, AudioTunnelStatus) {
        let (enable_audio, audio_status) = match self.audio_mode {
            AudioCaptureMode::Disabled => (false, AudioTunnelStatus::DisabledByDevice),
            AudioCaptureMode::Enabled => match check_audio_availability(api_level) {
                Ok(()) => (
                    true,
                    AudioTunnelStatus::Available {
                        codec_id: CODEC_ID_AAC,
                    },
                ),
                Err(_) => (
                    false,
                    AudioTunnelStatus::UnavailableApiLevel {
                        api_level,
                        min_required: MIN_AUDIO_PLAYBACK_API_LEVEL,
                    },
                ),
            },
        };

        let mut args = vec![
            SCRCPY_SERVER_VERSION.to_owned(),
            format!("scid={}", self.scid),
            "tunnel_forward=true".to_owned(),
            "control=false".to_owned(),
            "video_codec=h264".to_owned(),
            "video_codec_options=max-bframes:int=0,i-frame-interval:int=1".to_owned(),
            "send_device_meta=false".to_owned(),
            "send_frame_meta=true".to_owned(),
            "send_stream_meta=true".to_owned(),
            "downsize_on_error=false".to_owned(),
            format!("max_size={}", self.max_size),
            format!("max_fps={}", self.max_fps),
            format!("video_bit_rate={}", self.video_bit_rate),
        ];

        if enable_audio {
            args.push("audio=true".to_owned());
            args.push("audio_source=playback".to_owned());
            args.push("audio_dup=true".to_owned());
            args.push("audio_codec=aac".to_owned());
            args.push(format!("audio_bit_rate={}", self.audio_bit_rate));
        } else {
            args.push("audio=false".to_owned());
        }

        (args, audio_status)
    }
}

// ---------------- Symmetric Big-Endian Writers (for tests & mock synthesis) ----------------

/// Writes a `scrcpy` 5.0.1 video tunnel handshake (`0x00` dummy byte + 4-byte codec ID).
///
/// # Errors
///
/// Returns [`ScrcpyError::Io`] if writing fails.
pub fn write_video_handshake<W: Write>(writer: &mut W, codec_id: u32) -> Result<(), ScrcpyError> {
    writer.write_all(&[SCRCPY_DUMMY_BYTE])?;
    writer.write_all(&codec_id.to_be_bytes())?;
    Ok(())
}

/// Writes a Big-Endian video session metadata record (`header_tag`, `width`, `height`).
///
/// # Errors
///
/// Returns [`ScrcpyError`] if dimensions do not fit in positive `i32` or writing fails.
pub fn write_video_session_meta<W: Write>(
    writer: &mut W,
    meta: VideoSessionMeta,
) -> Result<(), ScrcpyError> {
    let w_i32 = i32::try_from(meta.width).map_err(|_| ScrcpyError::InvalidSessionDimensions {
        width: i32::MAX,
        height: 0,
    })?;
    let h_i32 = i32::try_from(meta.height).map_err(|_| ScrcpyError::InvalidSessionDimensions {
        width: w_i32,
        height: i32::MAX,
    })?;
    if w_i32 <= 0 || h_i32 <= 0 {
        return Err(ScrcpyError::InvalidSessionDimensions {
            width: w_i32,
            height: h_i32,
        });
    }
    writer.write_all(&(meta.header_tag | SESSION_META_TAG).to_be_bytes())?;
    writer.write_all(&w_i32.to_be_bytes())?;
    writer.write_all(&h_i32.to_be_bytes())?;
    Ok(())
}

/// Writes a Big-Endian `scrcpy` 5.0.1 video packet record.
///
/// # Errors
///
/// Returns [`ScrcpyError::InvalidPacketLength`] if `packet.data.len()` is outside `1..=16 MiB`,
/// or [`ScrcpyError::Io`] if writing fails.
pub fn write_video_packet<W: Write>(
    writer: &mut W,
    packet: &VideoPacket,
) -> Result<(), ScrcpyError> {
    let len_i32 =
        i32::try_from(packet.data.len()).map_err(|_| ScrcpyError::InvalidPacketLength {
            stream: "video",
            length: i32::MAX,
            max_length: MAX_VIDEO_PACKET_LEN,
        })?;
    let _validated = validate_packet_len("video", len_i32, MAX_VIDEO_PACKET_LEN)?;
    let flag_bits = match packet.kind {
        VideoPacketKind::Config => PACKET_FLAG_CONFIG,
        VideoPacketKind::Keyframe => PACKET_FLAG_KEYFRAME,
        VideoPacketKind::DeltaFrame => 0,
    };
    let flags = flag_bits | (packet.pts_us & PACKET_PTS_US_MASK);
    writer.write_all(&flags.to_be_bytes())?;
    writer.write_all(&len_i32.to_be_bytes())?;
    writer.write_all(&packet.data)?;
    Ok(())
}

/// Writes a `scrcpy` 5.0.1 4-byte Big-Endian audio codec ID handshake.
///
/// # Errors
///
/// Returns [`ScrcpyError::Io`] if writing fails.
pub fn write_audio_handshake<W: Write>(writer: &mut W, codec_id: u32) -> Result<(), ScrcpyError> {
    writer.write_all(&codec_id.to_be_bytes())?;
    Ok(())
}

/// Writes a Big-Endian `scrcpy` 5.0.1 audio packet record.
///
/// # Errors
///
/// Returns [`ScrcpyError::InvalidPacketLength`] if `packet.data.len()` is outside `1..=64 KiB`,
/// or [`ScrcpyError::Io`] if writing fails.
pub fn write_audio_packet<W: Write>(
    writer: &mut W,
    packet: &AudioPacket,
) -> Result<(), ScrcpyError> {
    let len_i32 =
        i32::try_from(packet.data.len()).map_err(|_| ScrcpyError::InvalidPacketLength {
            stream: "audio",
            length: i32::MAX,
            max_length: MAX_AUDIO_PACKET_LEN,
        })?;
    let _validated = validate_packet_len("audio", len_i32, MAX_AUDIO_PACKET_LEN)?;
    let flag_bits = match packet.kind {
        AudioPacketKind::Config => PACKET_FLAG_CONFIG,
        AudioPacketKind::Frame => 0,
    };
    let flags = flag_bits | (packet.pts_us & PACKET_PTS_US_MASK);
    writer.write_all(&flags.to_be_bytes())?;
    writer.write_all(&len_i32.to_be_bytes())?;
    writer.write_all(&packet.data)?;
    Ok(())
}

// ---------------- Internal I/O Helpers ----------------

fn validate_packet_len(
    stream: &'static str,
    length: i32,
    max_length: usize,
) -> Result<usize, ScrcpyError> {
    if length <= 0 {
        return Err(ScrcpyError::InvalidPacketLength {
            stream,
            length,
            max_length,
        });
    }
    let len_usize = usize::try_from(length).map_err(|_| ScrcpyError::InvalidPacketLength {
        stream,
        length,
        max_length,
    })?;
    if len_usize > max_length {
        return Err(ScrcpyError::InvalidPacketLength {
            stream,
            length,
            max_length,
        });
    }
    Ok(len_usize)
}

fn read_exact_ctx<R: Read>(
    reader: &mut R,
    dst: &mut [u8],
    context: &'static str,
) -> Result<(), ScrcpyError> {
    let mut pos = 0usize;
    while pos < dst.len() {
        match reader.read(&mut dst[pos..]) {
            Ok(0) => return Err(ScrcpyError::UnexpectedEof { context }),
            Ok(n) => pos += n,
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) => return Err(ScrcpyError::Io(err)),
        }
    }
    Ok(())
}

fn read_exact_or_clean_eof<R: Read>(
    reader: &mut R,
    dst: &mut [u8],
    context: &'static str,
) -> Result<Option<()>, ScrcpyError> {
    let mut pos = 0usize;
    while pos < dst.len() {
        match reader.read(&mut dst[pos..]) {
            Ok(0) => {
                if pos == 0 {
                    return Ok(None);
                }
                return Err(ScrcpyError::UnexpectedEof { context });
            }
            Ok(n) => pos += n,
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) => return Err(ScrcpyError::Io(err)),
        }
    }
    Ok(Some(()))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{
        AUDIO_CODEC_DISABLED, AUDIO_CODEC_ERROR, AudioCaptureMode, AudioPacket, AudioPacketKind,
        AudioTunnelReader, AudioTunnelStatus, CODEC_ID_AAC, CODEC_ID_H264, MAX_AUDIO_PACKET_LEN,
        MAX_VIDEO_PACKET_LEN, SESSION_META_TAG, ScrcpyError, ScrcpyLaunchOptions, VideoPacket,
        VideoPacketKind, VideoRecord, VideoSessionMeta, VideoTunnelReader,
        check_audio_availability, write_audio_handshake, write_audio_packet, write_video_handshake,
        write_video_packet, write_video_session_meta,
    };

    #[test]
    fn parses_scrcpy_501_video_session_config_idr_and_p_frames() {
        let mut wire = Vec::new();
        write_video_handshake(&mut wire, CODEC_ID_H264).expect("write video handshake");
        let meta = VideoSessionMeta {
            header_tag: SESSION_META_TAG,
            width: 1920,
            height: 1080,
        };
        write_video_session_meta(&mut wire, meta).expect("write session meta");

        let config = VideoPacket {
            kind: VideoPacketKind::Config,
            pts_us: 0,
            data: vec![0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xE0, 0x28],
        };
        let idr = VideoPacket {
            kind: VideoPacketKind::Keyframe,
            pts_us: 488_042_148,
            data: vec![0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84],
        };
        let p_frame = VideoPacket {
            kind: VideoPacketKind::DeltaFrame,
            pts_us: 488_075_481,
            data: vec![0x00, 0x00, 0x00, 0x01, 0x41, 0x9A, 0x22],
        };

        write_video_packet(&mut wire, &config).expect("write config");
        write_video_packet(&mut wire, &idr).expect("write idr");
        write_video_packet(&mut wire, &p_frame).expect("write p-frame");

        let mut reader =
            VideoTunnelReader::handshake(Cursor::new(wire)).expect("video handshake must pass");
        assert_eq!(reader.codec_id(), CODEC_ID_H264, "video codec must be h264");
        assert_eq!(
            reader.read_next().expect("read meta"),
            Some(VideoRecord::SessionMeta(meta)),
            "first record must be SessionMeta"
        );
        assert_eq!(
            reader.current_dimensions(),
            Some((1920, 1080)),
            "reader must track latest video dimensions"
        );

        let r_cfg = reader.read_next().expect("read config");
        assert_eq!(
            r_cfg,
            Some(VideoRecord::Packet(config)),
            "second record must be Config packet"
        );
        let r_idr = reader.read_next().expect("read idr");
        assert_eq!(
            r_idr,
            Some(VideoRecord::Packet(idr.clone())),
            "third record must be Keyframe packet"
        );
        assert_eq!(
            idr.pts_monotonic_ns(),
            Some(488_042_148_000),
            "PTS µs must convert cleanly to ns"
        );
        let r_p = reader.read_next().expect("read p-frame");
        assert_eq!(
            r_p,
            Some(VideoRecord::Packet(p_frame)),
            "fourth record must be DeltaFrame packet"
        );
        assert_eq!(
            reader.read_next().expect("clean EOF"),
            None,
            "EOF at record boundary must yield None"
        );
    }

    #[test]
    fn parses_scrcpy_501_audio_and_handles_disabled_error_and_api_level() {
        // Normal AAC stream.
        let mut wire = Vec::new();
        write_audio_handshake(&mut wire, CODEC_ID_AAC).expect("write audio handshake");
        let asc = AudioPacket {
            kind: AudioPacketKind::Config,
            pts_us: 0,
            data: vec![0x11, 0x90],
        };
        let frame = AudioPacket {
            kind: AudioPacketKind::Frame,
            pts_us: 12_345_678,
            data: vec![0x21, 0x10, 0x05, 0x44],
        };
        write_audio_packet(&mut wire, &asc).expect("write asc");
        write_audio_packet(&mut wire, &frame).expect("write audio frame");

        let mut reader = AudioTunnelReader::handshake_for_api(Cursor::new(wire), 37)
            .expect("API 37 AAC audio handshake must pass");
        assert_eq!(reader.codec_id(), CODEC_ID_AAC, "audio codec must be AAC");
        assert_eq!(
            reader.read_next().expect("read asc"),
            Some(asc),
            "first audio record must be Config"
        );
        assert_eq!(
            reader.read_next().expect("read frame"),
            Some(frame),
            "second audio record must be Frame"
        );
        assert_eq!(
            reader.read_next().expect("clean EOF"),
            None,
            "EOF must yield None"
        );

        // Disabled codec_id = 0.
        let mut disabled = Vec::new();
        write_audio_handshake(&mut disabled, AUDIO_CODEC_DISABLED).expect("write disabled");
        assert!(
            matches!(
                AudioTunnelReader::handshake(Cursor::new(disabled)),
                Err(ScrcpyError::AudioDisabled)
            ),
            "codec_id 0 must return AudioDisabled"
        );

        // Error codec_id = 1.
        let mut errored = Vec::new();
        write_audio_handshake(&mut errored, AUDIO_CODEC_ERROR).expect("write error");
        assert!(
            matches!(
                AudioTunnelReader::handshake(Cursor::new(errored)),
                Err(ScrcpyError::AudioServerError)
            ),
            "codec_id 1 must return AudioServerError"
        );

        // API < 33 explicitly flagged without failing video launch options.
        assert!(
            matches!(
                check_audio_availability(32),
                Err(ScrcpyError::AudioUnavailableApiLevel {
                    api_level: 32,
                    min_required: 33
                })
            ),
            "API 32 must report AudioUnavailableApiLevel"
        );
        let opts = ScrcpyLaunchOptions::fhd30("1a2b3c4d");
        let (args_32, status_32) = opts.build_server_args(32);
        assert_eq!(
            status_32,
            AudioTunnelStatus::UnavailableApiLevel {
                api_level: 32,
                min_required: 33,
            },
            "API 32 launch args must flag UnavailableApiLevel"
        );
        assert!(
            args_32.iter().any(|a| a == "audio=false"),
            "API 32 launch args must disable audio cleanly"
        );
        assert!(
            args_32.iter().any(|a| a == "video_codec=h264"),
            "video options must remain active when audio is unavailable"
        );

        let (args_33, status_33) = opts.build_server_args(33);
        assert_eq!(
            status_33,
            AudioTunnelStatus::Available {
                codec_id: CODEC_ID_AAC
            },
            "API 33 must enable AAC playback audio"
        );
        assert!(
            args_33.iter().any(|a| a == "audio_source=playback"),
            "API 33 args must request playback audio"
        );
        assert_eq!(
            opts.audio_mode,
            AudioCaptureMode::Enabled,
            "options must remain unchanged"
        );
    }

    #[test]
    fn rejects_oversized_and_zero_length_packets_and_bad_dummy_byte() {
        // Bad dummy byte.
        let bad_dummy = [0x01u8, 0x68, 0x32, 0x36, 0x34];
        assert!(
            matches!(
                VideoTunnelReader::handshake(Cursor::new(bad_dummy)),
                Err(ScrcpyError::InvalidDummyByte { found: 0x01 })
            ),
            "non-zero dummy byte must be rejected"
        );

        // Oversized video packet (16 MiB + 1).
        let mut oversized_video = Vec::new();
        write_video_handshake(&mut oversized_video, CODEC_ID_H264).expect("handshake");
        oversized_video.extend_from_slice(&0u64.to_be_bytes()); // flags
        let over_v_len = i32::try_from(MAX_VIDEO_PACKET_LEN + 1).expect("fits in i32");
        oversized_video.extend_from_slice(&over_v_len.to_be_bytes());
        let mut v_reader =
            VideoTunnelReader::handshake(Cursor::new(oversized_video)).expect("handshake");
        assert!(
            matches!(
                v_reader.read_next(),
                Err(ScrcpyError::InvalidPacketLength {
                    stream: "video",
                    max_length: MAX_VIDEO_PACKET_LEN,
                    ..
                })
            ),
            "video packet > 16 MiB must be rejected before allocation"
        );

        // Oversized audio packet (64 KiB + 1) and zero-length audio packet.
        for bad_len in [0i32, i32::try_from(MAX_AUDIO_PACKET_LEN + 1).expect("fits")] {
            let mut bad_audio = Vec::new();
            write_audio_handshake(&mut bad_audio, CODEC_ID_AAC).expect("handshake");
            bad_audio.extend_from_slice(&0u64.to_be_bytes());
            bad_audio.extend_from_slice(&bad_len.to_be_bytes());
            let mut a_reader =
                AudioTunnelReader::handshake(Cursor::new(bad_audio)).expect("handshake");
            assert!(
                matches!(
                    a_reader.read_next(),
                    Err(ScrcpyError::InvalidPacketLength {
                        stream: "audio",
                        max_length: MAX_AUDIO_PACKET_LEN,
                        ..
                    })
                ),
                "audio packet length {bad_len} must be rejected"
            );
        }
    }

    #[test]
    fn t6e_aos9_api_28_proactively_disables_audio_with_clear_reason() {
        let opts = ScrcpyLaunchOptions::fhd30("1a2b3c4d");
        let (args_28, status_28) = opts.build_server_args(28);
        assert_eq!(
            status_28,
            AudioTunnelStatus::UnavailableApiLevel {
                api_level: 28,
                min_required: 33,
            }
        );
        assert!(
            args_28.iter().any(|a| a == "audio=false"),
            "API 28 must launch scrcpy-server with audio=false"
        );
        assert!(
            !args_28.iter().any(|a| a == "audio=true"),
            "API 28 must never pass audio=true"
        );
        assert!(
            args_28.iter().any(|a| a == "video_codec=h264"),
            "API 28 must preserve H.264 video capture"
        );

        let reason_28 = super::audio_unavailable_reason_for_api(28)
            .expect("API 28 must produce a clear diagnostic reason");
        // Audio is only ever captured on API 33+ (non-muting playback capture), so no reason
        // may suggest that Android 11/12 (API 30-32) would capture audio.
        assert_eq!(
            reason_28,
            "Audio capture requires Android 13+ (API 33+, non-muting playback capture); detected API 28. Continuing with video + logs + taps."
        );
        for api in [24_u32, 28, 29, 30, 31, 32] {
            let reason = super::audio_unavailable_reason_for_api(api).expect("API < 33 reason");
            assert!(
                reason.contains("API 33+") && reason.contains(&format!("detected API {api}.")),
                "{reason}"
            );
            assert!(
                !reason.contains("Android 11") && !reason.contains("API 30+"),
                "{reason}"
            );
        }
        assert_eq!(super::audio_unavailable_reason_for_api(33), None);
        assert_eq!(super::audio_unavailable_reason_for_api(0), None);

        // Support floor check: API < 28 emits non-blocking warning and never hard-blocks; API 28+ is None.
        let warn_27 = super::unsupported_android_api_warning(27)
            .expect("API 27 must emit non-blocking support floor warning");
        assert!(
            warn_27.contains("Android 9 (API 28) 未満は非サポート対象です (API 27)"),
            "warn_27={warn_27}"
        );
        assert_eq!(super::unsupported_android_api_warning(28), None);
        assert_eq!(super::unsupported_android_api_warning(0), None);
        let (args_27, _) = opts.build_server_args(27);
        assert!(args_27.iter().any(|a| a == "audio=false"));
    }
}
