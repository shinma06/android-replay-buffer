//! Exact Little-Endian binary wire protocol codec matching `device-agent/PROTOCOL.md` (v1).
//!
//! Every frame in both directions uses a fixed 20-byte Little-Endian header
//! immediately followed by `payload_len` bytes of Little-Endian payload data
//! with no implicit alignment padding. Both streaming incremental deframing
//! ([`FrameDeframer`]) and synchronous [`std::io::Read`] / [`std::io::Write`]
//! helpers enforce a strict `max_payload_len` guard before allocating buffers.

use std::io::{ErrorKind, Read, Write};

use arb_core::bundle::LogCompleteness;
use arb_core::time::StreamKind;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Two-byte ASCII frame magic (`"AR"` = `0x41`, `0x52`).
pub const MAGIC: [u8; 2] = *b"AR";

/// Supported binary wire protocol version (`1`).
pub const PROTOCOL_VERSION: u8 = 1;

/// Fixed size of every wire frame header in bytes (`20`).
pub const HEADER_SIZE: usize = 20;

/// Default hard payload safety cap (`4 MiB` = `4_194_304` bytes) matching `PROTOCOL.md`.
pub const DEFAULT_MAX_PAYLOAD_LEN: u64 = 4 * 1024 * 1024;

/// Fixed byte length of a `Ping` payload (`8` bytes).
pub const PING_PAYLOAD_LEN: usize = 8;

/// Fixed byte length of a `Pong` payload (`40` bytes).
pub const PONG_PAYLOAD_LEN: usize = 40;

/// Fixed byte length of a single `StreamHealthEntry` in `Heartbeat` (`20` bytes).
pub const STREAM_HEALTH_ENTRY_LEN: usize = 20;

/// Sentinel value for unknown unsigned 32-bit fields (`lid`, `uid`).
pub const UNKNOWN_U32: u32 = 0xFFFF_FFFF;

/// Sentinel value for unknown unsigned 16-bit fields (`ev_type`, `ev_code`).
pub const UNKNOWN_U16: u16 = 0xFFFF;

/// Bitmask bit in `LogRecordPayload::record_flags` indicating a binary event payload.
pub const LOG_RECORD_FLAG_BINARY_EVENT: u8 = 0x01;

/// Maximum UTF-8 byte length of a `str16` field (`65_535`).
pub const MAX_STR16_LEN: usize = 65_535;

/// Errors returned while encoding, decoding, or deframing ARB v1 wire messages.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum WireError {
    /// Underlying I/O failure while reading or writing framed bytes.
    #[error("wire I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Frame header did not start with ASCII `"AR"`.
    #[error("invalid frame magic: expected 0x41 0x52, got 0x{:02x} 0x{:02x}", found[0], found[1])]
    InvalidMagic {
        /// Observed first two bytes.
        found: [u8; 2],
    },
    /// Frame header declared an unsupported protocol version.
    #[error("unsupported wire protocol version: {found} (expected {PROTOCOL_VERSION})")]
    UnsupportedVersion {
        /// Observed protocol version byte.
        found: u8,
    },
    /// Declared payload length exceeded the configured denial-of-service safety cap.
    #[error("payload length {declared} exceeds max allowed {max_allowed} bytes")]
    PayloadTooLarge {
        /// Payload length declared in the header or field.
        declared: u64,
        /// Configured upper bound.
        max_allowed: u64,
    },
    /// Stream ended mid-header or mid-payload before all bytes arrived.
    #[error("truncated wire frame: expected {expected} bytes, got {actual}")]
    TruncatedFrame {
        /// Expected byte count.
        expected: usize,
        /// Observed byte count before EOF.
        actual: usize,
    },
    /// Payload ended before all required fields could be decoded.
    #[error("truncated payload while reading {field}: need {needed} bytes, remaining {remaining}")]
    TruncatedPayload {
        /// Field name being decoded.
        field: &'static str,
        /// Required byte count.
        needed: usize,
        /// Remaining bytes in the payload slice.
        remaining: usize,
    },
    /// Payload contained unexpected extra bytes after all fields were decoded.
    #[error("trailing bytes in {message} payload: {remaining} unconsumed byte(s)")]
    TrailingBytes {
        /// Message type label.
        message: &'static str,
        /// Count of unconsumed trailing bytes.
        remaining: usize,
    },
    /// Unknown wire enum discriminant.
    #[error("unknown {kind} wire discriminant: {value}")]
    UnknownDiscriminant {
        /// Enum kind label.
        kind: &'static str,
        /// Raw integer value.
        value: u16,
    },
    /// `str16` field did not contain valid UTF-8.
    #[error("invalid UTF-8 in wire string field '{field}': {source}")]
    InvalidUtf8 {
        /// Field name.
        field: &'static str,
        /// Underlying UTF-8 decoding error.
        #[source]
        source: std::string::FromUtf8Error,
    },
    /// String exceeded the 65,535-byte `str16` wire limit during encoding.
    #[error("string field '{field}' length {len} exceeds str16 maximum {MAX_STR16_LEN}")]
    StringTooLong {
        /// Field name.
        field: &'static str,
        /// Byte length of the string.
        len: usize,
    },
    /// Collection exceeded the wire count limit (e.g., > 255 heartbeat streams).
    #[error("collection field '{field}' length {len} exceeds maximum {max}")]
    CollectionTooLarge {
        /// Field name.
        field: &'static str,
        /// Observed element count.
        len: usize,
        /// Maximum allowed element count.
        max: usize,
    },
}

/// Bitmask flags carried in byte 5 of the 20-byte frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct FrameFlags(u8);

impl FrameFlags {
    /// No flags set (`0x00`).
    pub const NONE: Self = Self(0x00);
    /// Control reply or high-priority notification (`0x01`).
    pub const URGENT: Self = Self(0x01);
    /// Variable-length field was truncated to a safety cap (`0x02`).
    pub const TRUNCATED: Self = Self(0x02);
    /// First data frame emitted after a fault-isolated worker restart (`0x04`).
    pub const RECOVERED: Self = Self(0x04);

    /// Wraps raw wire flag bits.
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// Returns the raw wire byte representation.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Returns the bitwise union of `self` and `other`.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Returns `true` if all bits in `other` are set in `self`.
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Returns `true` if [`Self::URGENT`] is set.
    pub const fn is_urgent(self) -> bool {
        self.contains(Self::URGENT)
    }

    /// Returns `true` if [`Self::TRUNCATED`] is set.
    pub const fn is_truncated(self) -> bool {
        self.contains(Self::TRUNCATED)
    }

    /// Returns `true` if [`Self::RECOVERED`] is set.
    pub const fn is_recovered(self) -> bool {
        self.contains(Self::RECOVERED)
    }
}

/// Wire `stream_id` identifying the logical stream owning a frame or health entry.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamId {
    /// Stream 0: Control / Beacon (`Hello`, `Heartbeat`, `Ping`, `Pong`, `Shutdown`).
    Control = 0,
    /// Stream 1: Binary `logcat -B` and overflow stats.
    Logcat = 1,
    /// Stream 2: Input / touch events (`getevent -lt`).
    Input = 2,
    /// Stream 3: Reserved for `scrcpy-server` video health correlation.
    Video = 3,
    /// Stream 4: Reserved for `scrcpy-server` audio health correlation.
    Audio = 4,
}

impl StreamId {
    /// Encodes this stream identifier to its wire `u8` value.
    pub const fn to_wire(self) -> u8 {
        match self {
            Self::Control => 0,
            Self::Logcat => 1,
            Self::Input => 2,
            Self::Video => 3,
            Self::Audio => 4,
        }
    }

    /// Maps data streams to `arb_core::time::StreamKind`, returning `None` for `Control`.
    pub const fn to_core_stream_kind(self) -> Option<StreamKind> {
        match self {
            Self::Control => None,
            Self::Logcat => Some(StreamKind::Logcat),
            Self::Input => Some(StreamKind::Input),
            Self::Video => Some(StreamKind::Video),
            Self::Audio => Some(StreamKind::Audio),
        }
    }
}

impl TryFrom<u8> for StreamId {
    type Error = WireError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Control),
            1 => Ok(Self::Logcat),
            2 => Ok(Self::Input),
            3 => Ok(Self::Video),
            4 => Ok(Self::Audio),
            other => Err(WireError::UnknownDiscriminant {
                kind: "stream_id",
                value: u16::from(other),
            }),
        }
    }
}

/// Wire `msg_type` identifying the payload schema carried by a frame.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    /// `0x01`: Initial session handshake (`boot_id` and device metadata).
    Hello = 0x01,
    /// `0x02`: Periodic 3-clock triple and per-stream health table.
    Heartbeat = 0x02,
    /// `0x03`: Host-to-agent clock-sync RTT probe request.
    Ping = 0x03,
    /// `0x04`: Immediate 4-timestamp clock-sync reply echoing `nonce`.
    Pong = 0x04,
    /// `0x05`: Worker lifecycle / fault-isolation state transition.
    StreamState = 0x05,
    /// `0x10`: Binary `logcat -B` record tagged with `device_read_boottime_ns`.
    LogRecord = 0x10,
    /// `0x11`: Periodic `logcat -S` / `-g` drop-stat probe and completeness.
    LogOverflowStats = 0x11,
    /// `0x20`: Single `getevent -lt` input event tagged with `device_read_boottime_ns`.
    TouchEvent = 0x20,
    /// `0x30`: Graceful bidirectional session termination handshake.
    Shutdown = 0x30,
}

impl MessageType {
    /// Encodes this message type to its wire `u8` value.
    pub const fn to_wire(self) -> u8 {
        match self {
            Self::Hello => 0x01,
            Self::Heartbeat => 0x02,
            Self::Ping => 0x03,
            Self::Pong => 0x04,
            Self::StreamState => 0x05,
            Self::LogRecord => 0x10,
            Self::LogOverflowStats => 0x11,
            Self::TouchEvent => 0x20,
            Self::Shutdown => 0x30,
        }
    }
}

impl TryFrom<u8> for MessageType {
    type Error = WireError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::Hello),
            0x02 => Ok(Self::Heartbeat),
            0x03 => Ok(Self::Ping),
            0x04 => Ok(Self::Pong),
            0x05 => Ok(Self::StreamState),
            0x10 => Ok(Self::LogRecord),
            0x11 => Ok(Self::LogOverflowStats),
            0x20 => Ok(Self::TouchEvent),
            0x30 => Ok(Self::Shutdown),
            other => Err(WireError::UnknownDiscriminant {
                kind: "msg_type",
                value: u16::from(other),
            }),
        }
    }
}

/// Per-stream lifecycle state reported in `Heartbeat` and `StreamState` frames.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamLifecycleState {
    /// `0`: Worker is starting or initializing.
    Starting = 0,
    /// `1`: Worker is actively capturing.
    Running = 1,
    /// `2`: Worker is cleanly stopped (e.g., disabled or `no_touch_device`).
    Stopped = 2,
    /// `3`: Worker encountered a fault or unexpected subprocess exit.
    Error = 3,
    /// `4`: Worker is backing off before respawning.
    Restarting = 4,
}

impl StreamLifecycleState {
    /// Encodes this lifecycle state to its wire `u8` value.
    pub const fn to_wire(self) -> u8 {
        match self {
            Self::Starting => 0,
            Self::Running => 1,
            Self::Stopped => 2,
            Self::Error => 3,
            Self::Restarting => 4,
        }
    }
}

impl TryFrom<u8> for StreamLifecycleState {
    type Error = WireError;

    fn try_from(value: u8) -> Result<Self, WireError> {
        match value {
            0 => Ok(Self::Starting),
            1 => Ok(Self::Running),
            2 => Ok(Self::Stopped),
            3 => Ok(Self::Error),
            4 => Ok(Self::Restarting),
            other => Err(WireError::UnknownDiscriminant {
                kind: "stream_state",
                value: u16::from(other),
            }),
        }
    }
}

/// Source used by the device agent to obtain `boot_id`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootIdSource {
    /// `0`: Direct kernel UUID from `/proc/sys/kernel/random/boot_id`.
    KernelProcBootId = 0,
    /// `1`: Deterministic fallback built from `boot_count` and `/proc/stat btime`.
    FallbackBootCountBtime = 1,
    /// `2`: Last-resort per-process random UUID; continuity across reconnects is unverified.
    FallbackRandomUuid = 2,
}

impl BootIdSource {
    /// Encodes this `boot_id` source to its wire `u8` value.
    pub const fn to_wire(self) -> u8 {
        match self {
            Self::KernelProcBootId => 0,
            Self::FallbackBootCountBtime => 1,
            Self::FallbackRandomUuid => 2,
        }
    }

    /// Returns `true` if this source reliably distinguishes reconnects from reboots.
    pub const fn is_reliable_for_reboot_detection(self) -> bool {
        matches!(self, Self::KernelProcBootId | Self::FallbackBootCountBtime)
    }
}

impl TryFrom<u8> for BootIdSource {
    type Error = WireError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::KernelProcBootId),
            1 => Ok(Self::FallbackBootCountBtime),
            2 => Ok(Self::FallbackRandomUuid),
            other => Err(WireError::UnknownDiscriminant {
                kind: "boot_id_source",
                value: u16::from(other),
            }),
        }
    }
}

/// Wire logcat completeness classification emitted in `LogOverflowStats`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireLogCompleteness {
    /// `0`: Explicit stats parsed with zero pruning/drop delta and zero reader gaps.
    Verified = 0,
    /// `1`: Explicit overflow, drop, or unexpected `logcat` process termination occurred.
    OverflowDetected = 1,
    /// `2`: Completeness could not be proved under current device permissions or ring churn.
    Unverified = 2,
}

impl WireLogCompleteness {
    /// Encodes this completeness value to its wire `u8` value.
    pub const fn to_wire(self) -> u8 {
        match self {
            Self::Verified => 0,
            Self::OverflowDetected => 1,
            Self::Unverified => 2,
        }
    }

    /// Converts to the `arb_core::bundle::LogCompleteness` domain enum.
    pub const fn to_core_completeness(self) -> LogCompleteness {
        match self {
            Self::Verified => LogCompleteness::VerifiedNoOverflow,
            Self::OverflowDetected => LogCompleteness::OverflowDetected,
            Self::Unverified => LogCompleteness::Unverified,
        }
    }
}

impl TryFrom<u8> for WireLogCompleteness {
    type Error = WireError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Verified),
            1 => Ok(Self::OverflowDetected),
            2 => Ok(Self::Unverified),
            other => Err(WireError::UnknownDiscriminant {
                kind: "log_completeness",
                value: u16::from(other),
            }),
        }
    }
}

/// Termination reason code carried in `Shutdown` frames (`u16` LE).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownCode {
    /// `0`: Graceful normal shutdown.
    Normal = 0,
    /// `1`: Host transport disconnected.
    HostDisconnect = 1,
    /// `2`: Unrecoverable fatal agent error.
    FatalError = 2,
}

impl ShutdownCode {
    /// Encodes this shutdown code to its wire `u16` value.
    pub const fn to_wire(self) -> u16 {
        match self {
            Self::Normal => 0,
            Self::HostDisconnect => 1,
            Self::FatalError => 2,
        }
    }
}

impl TryFrom<u16> for ShutdownCode {
    type Error = WireError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Normal),
            1 => Ok(Self::HostDisconnect),
            2 => Ok(Self::FatalError),
            other => Err(WireError::UnknownDiscriminant {
                kind: "shutdown_code",
                value: other,
            }),
        }
    }
}

/// Decoded 20-byte Little-Endian frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FrameHeader {
    /// Wire protocol version (must be `1`).
    pub version: u8,
    /// Owning stream identifier.
    pub stream_id: StreamId,
    /// Payload message type.
    pub msg_type: MessageType,
    /// Frame bitmask flags.
    pub flags: FrameFlags,
    /// Reserved 16-bit field (`0` on send; preserved from wire on receive).
    pub reserved: u16,
    /// Per-stream monotonic sequence number.
    pub seq: u32,
    /// Byte length of the immediately following payload.
    pub payload_len: u64,
}

impl FrameHeader {
    /// Encodes this header into its exact 20-byte Little-Endian representation.
    pub fn to_bytes(self) -> [u8; HEADER_SIZE] {
        let mut out = [0u8; HEADER_SIZE];
        out[0] = MAGIC[0];
        out[1] = MAGIC[1];
        out[2] = self.version;
        out[3] = self.stream_id.to_wire();
        out[4] = self.msg_type.to_wire();
        out[5] = self.flags.bits();
        out[6..8].copy_from_slice(&self.reserved.to_le_bytes());
        out[8..12].copy_from_slice(&self.seq.to_le_bytes());
        out[12..20].copy_from_slice(&self.payload_len.to_le_bytes());
        out
    }

    /// Parses and validates a 20-byte Little-Endian frame header.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] if the magic bytes, protocol version, stream ID,
    /// message type, or `payload_len > max_payload_len` fail validation.
    pub fn from_bytes(bytes: &[u8; HEADER_SIZE], max_payload_len: u64) -> Result<Self, WireError> {
        let magic = [bytes[0], bytes[1]];
        if magic != MAGIC {
            return Err(WireError::InvalidMagic { found: magic });
        }
        let version = bytes[2];
        if version != PROTOCOL_VERSION {
            return Err(WireError::UnsupportedVersion { found: version });
        }
        let stream_id = StreamId::try_from(bytes[3])?;
        let msg_type = MessageType::try_from(bytes[4])?;
        let flags = FrameFlags::from_bits(bytes[5]);
        let reserved = u16::from_le_bytes([bytes[6], bytes[7]]);
        let seq = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        let payload_len = u64::from_le_bytes([
            bytes[12], bytes[13], bytes[14], bytes[15], bytes[16], bytes[17], bytes[18], bytes[19],
        ]);
        if payload_len > max_payload_len {
            return Err(WireError::PayloadTooLarge {
                declared: payload_len,
                max_allowed: max_payload_len,
            });
        }
        Ok(Self {
            version,
            stream_id,
            msg_type,
            flags,
            reserved,
            seq,
            payload_len,
        })
    }
}

/// Complete framed wire packet containing a validated header and raw payload bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WireFrame {
    /// Decoded 20-byte frame header.
    pub header: FrameHeader,
    /// Raw Little-Endian payload bytes.
    pub payload: Vec<u8>,
}

impl WireFrame {
    /// Constructs a v1 frame with `reserved = 0` and computed `payload_len`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::PayloadTooLarge`] if `payload.len()` exceeds
    /// [`DEFAULT_MAX_PAYLOAD_LEN`].
    pub fn new(
        stream_id: StreamId,
        msg_type: MessageType,
        flags: FrameFlags,
        seq: u32,
        payload: Vec<u8>,
    ) -> Result<Self, WireError> {
        let payload_len = u64::try_from(payload.len()).map_err(|_| WireError::PayloadTooLarge {
            declared: u64::MAX,
            max_allowed: DEFAULT_MAX_PAYLOAD_LEN,
        })?;
        if payload_len > DEFAULT_MAX_PAYLOAD_LEN {
            return Err(WireError::PayloadTooLarge {
                declared: payload_len,
                max_allowed: DEFAULT_MAX_PAYLOAD_LEN,
            });
        }
        Ok(Self {
            header: FrameHeader {
                version: PROTOCOL_VERSION,
                stream_id,
                msg_type,
                flags,
                reserved: 0,
                seq,
                payload_len,
            },
            payload,
        })
    }

    /// Serializes this frame (20-byte header + payload) into a contiguous byte buffer.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::PayloadTooLarge`] if `payload.len()` exceeds `max_payload_len`.
    pub fn encode_with_limit(&self, max_payload_len: u64) -> Result<Vec<u8>, WireError> {
        let payload_len =
            u64::try_from(self.payload.len()).map_err(|_| WireError::PayloadTooLarge {
                declared: u64::MAX,
                max_allowed: max_payload_len,
            })?;
        if payload_len > max_payload_len {
            return Err(WireError::PayloadTooLarge {
                declared: payload_len,
                max_allowed: max_payload_len,
            });
        }
        let mut header = self.header;
        header.payload_len = payload_len;
        let mut out = Vec::with_capacity(HEADER_SIZE + self.payload.len());
        out.extend_from_slice(&header.to_bytes());
        out.extend_from_slice(&self.payload);
        Ok(out)
    }

    /// Serializes this frame using [`DEFAULT_MAX_PAYLOAD_LEN`].
    ///
    /// # Errors
    ///
    /// Returns [`WireError::PayloadTooLarge`] if the payload exceeds 4 MiB.
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        self.encode_with_limit(DEFAULT_MAX_PAYLOAD_LEN)
    }

    /// Reads one complete frame from `reader`, returning `Ok(None)` on clean EOF at byte 0.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] on invalid magic, unsupported version, payload length
    /// exceeding `max_payload_len`, mid-frame EOF, or underlying I/O failure.
    pub fn read_from<R: Read>(
        reader: &mut R,
        max_payload_len: u64,
    ) -> Result<Option<Self>, WireError> {
        let mut hdr = [0u8; HEADER_SIZE];
        let mut read_so_far = 0usize;
        while read_so_far < HEADER_SIZE {
            match reader.read(&mut hdr[read_so_far..]) {
                Ok(0) => {
                    if read_so_far == 0 {
                        return Ok(None);
                    }
                    return Err(WireError::TruncatedFrame {
                        expected: HEADER_SIZE,
                        actual: read_so_far,
                    });
                }
                Ok(n) => read_so_far += n,
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Err(err)
                    if read_so_far > 0
                        && matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    return Err(WireError::TruncatedFrame {
                        expected: HEADER_SIZE,
                        actual: read_so_far,
                    });
                }
                Err(err) => return Err(WireError::Io(err)),
            }
        }

        let header = FrameHeader::from_bytes(&hdr, max_payload_len)?;
        let payload_len_usize =
            usize::try_from(header.payload_len).map_err(|_| WireError::PayloadTooLarge {
                declared: header.payload_len,
                max_allowed: max_payload_len,
            })?;
        let mut payload = vec![0u8; payload_len_usize];
        let mut payload_read = 0usize;
        while payload_read < payload_len_usize {
            match reader.read(&mut payload[payload_read..]) {
                Ok(0) => {
                    return Err(WireError::TruncatedFrame {
                        expected: payload_len_usize,
                        actual: payload_read,
                    });
                }
                Ok(n) => payload_read += n,
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return Err(WireError::TruncatedFrame {
                        expected: payload_len_usize,
                        actual: payload_read,
                    });
                }
                Err(err) => return Err(WireError::Io(err)),
            }
        }

        Ok(Some(Self { header, payload }))
    }

    /// Writes this complete frame atomically into `writer` and flushes it.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] if encoding exceeds [`DEFAULT_MAX_PAYLOAD_LEN`] or writing fails.
    pub fn write_to<W: Write>(&self, writer: &mut W) -> Result<(), WireError> {
        let bytes = self.encode()?;
        writer.write_all(&bytes)?;
        writer.flush()?;
        Ok(())
    }
}

/// Incremental streaming deframer for chunked TCP / socket reads.
#[derive(Debug, Clone)]
pub struct FrameDeframer {
    buf: Vec<u8>,
    max_payload_len: u64,
}

impl FrameDeframer {
    /// Creates a deframer enforcing [`DEFAULT_MAX_PAYLOAD_LEN`].
    pub const fn new() -> Self {
        Self {
            buf: Vec::new(),
            max_payload_len: DEFAULT_MAX_PAYLOAD_LEN,
        }
    }

    /// Creates a deframer enforcing an explicit `max_payload_len` safety cap.
    pub const fn with_max_payload_len(max_payload_len: u64) -> Self {
        Self {
            buf: Vec::new(),
            max_payload_len,
        }
    }

    /// Returns the configured maximum payload length in bytes.
    pub const fn max_payload_len(&self) -> u64 {
        self.max_payload_len
    }

    /// Returns the number of buffered, unconsumed bytes.
    pub fn buffered_len(&self) -> usize {
        self.buf.len()
    }

    /// Returns `true` if no partial bytes remain buffered.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Appends newly received wire bytes to the internal buffer.
    pub fn push_bytes(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Attempts to extract the next complete [`WireFrame`] from buffered bytes.
    ///
    /// Returns `Ok(None)` when more bytes are needed for the current frame.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] immediately upon inspecting an invalid magic,
    /// unsupported version, unknown header discriminant, or oversized payload length.
    pub fn next_frame(&mut self) -> Result<Option<WireFrame>, WireError> {
        if self.buf.len() < HEADER_SIZE {
            return Ok(None);
        }
        let mut hdr = [0u8; HEADER_SIZE];
        hdr.copy_from_slice(&self.buf[..HEADER_SIZE]);
        let header = FrameHeader::from_bytes(&hdr, self.max_payload_len)?;
        let payload_len =
            usize::try_from(header.payload_len).map_err(|_| WireError::PayloadTooLarge {
                declared: header.payload_len,
                max_allowed: self.max_payload_len,
            })?;
        let total_len = HEADER_SIZE
            .checked_add(payload_len)
            .ok_or(WireError::PayloadTooLarge {
                declared: header.payload_len,
                max_allowed: self.max_payload_len,
            })?;
        if self.buf.len() < total_len {
            return Ok(None);
        }
        let payload = self.buf[HEADER_SIZE..total_len].to_vec();
        self.buf.drain(..total_len);
        Ok(Some(WireFrame { header, payload }))
    }

    /// Verifies that no partial frame remains when the underlying transport hits EOF.
    ///
    /// # Errors
    ///
    /// Returns [`WireError::TruncatedFrame`] if buffered trailing bytes remain.
    pub fn finish(&self) -> Result<(), WireError> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let expected = if self.buf.len() >= HEADER_SIZE {
            let mut hdr = [0u8; HEADER_SIZE];
            hdr.copy_from_slice(&self.buf[..HEADER_SIZE]);
            let header = FrameHeader::from_bytes(&hdr, self.max_payload_len)?;
            let payload_len =
                usize::try_from(header.payload_len).map_err(|_| WireError::PayloadTooLarge {
                    declared: header.payload_len,
                    max_allowed: self.max_payload_len,
                })?;
            HEADER_SIZE.saturating_add(payload_len)
        } else {
            HEADER_SIZE
        };
        Err(WireError::TruncatedFrame {
            expected,
            actual: self.buf.len(),
        })
    }
}

impl Default for FrameDeframer {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------- Typed Payloads ----------------

/// `0x01 Hello` payload sent once immediately after host connection.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HelloPayload {
    /// Wire protocol version (`1`).
    pub protocol_version: u16,
    /// Android `Build.VERSION.SDK_INT`.
    pub api_level: u32,
    /// `Settings.Global.BOOT_COUNT`, or `-1` if unreadable.
    pub boot_count: i32,
    /// Source used to derive `boot_id`.
    pub boot_id_source: BootIdSource,
    /// `true` when emulator heuristics matched on device.
    pub is_emulator: bool,
    /// Configured heartbeat period in milliseconds (`100..=5000`).
    pub heartbeat_interval_ms: u32,
    /// Canonical kernel boot UUID or explicit fallback string.
    pub boot_id: String,
    /// `android.os.Build.MODEL`.
    pub model: String,
    /// `android.os.Build.MANUFACTURER`.
    pub manufacturer: String,
    /// Companion semantic version string (e.g., `"0.1.0"`).
    pub agent_version: String,
    /// Empty when `boot_id_source == KernelProcBootId`, else diagnostic detail.
    pub boot_id_detail: String,
}

/// Fixed 20-byte per-stream health snapshot entry carried in [`HeartbeatPayload`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StreamHealthEntry {
    /// Logical stream identifier.
    pub stream_id: StreamId,
    /// Current lifecycle state of this stream.
    pub state: StreamLifecycleState,
    /// Cumulative fault-isolated restart count since companion launch.
    pub restart_count: u16,
    /// Most recent frame sequence number emitted on this stream (`0` if none yet).
    pub emitted_seq: u32,
    /// Most recent `CLOCK_BOOTTIME` timestamp emitted on this stream (`0` if none yet).
    pub last_pts_ns: i64,
}

/// `0x02 Heartbeat` payload carrying the 3-clock triple and per-stream health table.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HeartbeatPayload {
    /// Device `CLOCK_BOOTTIME` (`elapsedRealtimeNanos`) in nanoseconds.
    pub boottime_ns: i64,
    /// Device `CLOCK_MONOTONIC` (`System.nanoTime`) in nanoseconds.
    pub monotonic_ns: i64,
    /// Device `CLOCK_REALTIME` wall clock in nanoseconds.
    pub realtime_ns: i64,
    /// Snapshot of per-stream health entries (at most 255).
    pub streams: Vec<StreamHealthEntry>,
}

/// `0x03 Ping` payload sent by the host for clock-sync RTT probing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PingPayload {
    /// Arbitrary 64-bit host correlation nonce.
    pub nonce: u64,
}

/// `0x04 Pong` payload echoed immediately by the companion with 4 bracketed timestamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PongPayload {
    /// Echoed host correlation nonce.
    pub nonce: u64,
    /// Device `CLOCK_BOOTTIME` captured immediately before reading mono/wall.
    pub boottime_before_ns: i64,
    /// Device `CLOCK_MONOTONIC` captured inside the bracket.
    pub monotonic_ns: i64,
    /// Device `CLOCK_REALTIME` captured inside the bracket.
    pub realtime_ns: i64,
    /// Device `CLOCK_BOOTTIME` captured immediately after reading mono/wall.
    pub boottime_after_ns: i64,
}

impl PongPayload {
    /// Returns the device read bracket `boottime_after_ns - boottime_before_ns`, or `None` if negative.
    pub fn device_bracket_ns(&self) -> Option<u64> {
        let diff = i128::from(self.boottime_after_ns) - i128::from(self.boottime_before_ns);
        u64::try_from(diff).ok()
    }

    /// Returns the midpoint device `CLOCK_BOOTTIME` between `boottime_before_ns` and `boottime_after_ns`.
    pub fn midpoint_boottime_ns(&self) -> i64 {
        let sum = i128::from(self.boottime_before_ns) + i128::from(self.boottime_after_ns);
        i64::try_from(sum / 2).unwrap_or(self.boottime_before_ns)
    }

    /// Computes `PROTOCOL.md` §3.4 sync half-width upper bound: `rtt_ns / 2 + device_bracket_ns / 2`.
    pub fn sync_uncertainty_ns(&self, rtt_ns: u64) -> Option<u64> {
        let bracket = self.device_bracket_ns()?;
        (rtt_ns / 2).checked_add(bracket / 2)
    }
}

/// `0x05 StreamState` payload emitted on worker lifecycle transitions.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StreamStatePayload {
    /// Stream whose lifecycle state changed.
    pub target_stream_id: StreamId,
    /// New lifecycle state.
    pub state: StreamLifecycleState,
    /// Cumulative fault-isolated restart count for `target_stream_id`.
    pub restart_count: u16,
    /// Device `CLOCK_BOOTTIME` when the transition occurred.
    pub boottime_ns: i64,
    /// Last emitted sequence number on `target_stream_id`.
    pub emitted_seq: u32,
    /// Machine-readable transition reason (e.g., `"process_exit:137"`).
    pub reason: String,
}

/// `0x10 LogRecord` payload wrapping a single binary `logger_entry` from `logcat -B`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LogRecordPayload {
    /// Device `CLOCK_BOOTTIME` stamped immediately when the record was read on device.
    pub device_read_boottime_ns: i64,
    /// `logger_entry.sec` (`logd` wall-clock seconds).
    pub log_sec: u32,
    /// `logger_entry.nsec` (`logd` wall-clock nanoseconds).
    pub log_nsec: u32,
    /// Process ID (`logger_entry.pid`).
    pub pid: i32,
    /// Thread ID (`logger_entry.tid`).
    pub tid: i32,
    /// Log buffer ID (`0=main..7=kernel`, `UNKNOWN_U32` if absent).
    pub lid: u32,
    /// Process UID (`logger_entry_v4.uid`, `UNKNOWN_U32` if `hdr_size < 28`).
    pub uid: u32,
    /// Original binary `logger_entry` header size (`20`, `24`, `28`, ...).
    pub hdr_size: u16,
    /// Android log priority (`2=V..7=F`) for text buffers, or `0` for binary event buffers.
    pub priority: u8,
    /// Bit `0x01` marks a binary event buffer (`lid in {2, 5, 6}`).
    pub record_flags: u8,
    /// Raw `logger_entry` payload bytes.
    pub payload: Vec<u8>,
}

impl LogRecordPayload {
    /// Computes the raw `logd` wall-clock header timestamp in nanoseconds since Unix epoch.
    pub fn header_epoch_ns(&self) -> Option<i64> {
        i64::from(self.log_sec)
            .checked_mul(1_000_000_000)?
            .checked_add(i64::from(self.log_nsec))
    }

    /// Returns `true` if this entry comes from a binary event log buffer.
    pub const fn is_binary_event(&self) -> bool {
        (self.record_flags & LOG_RECORD_FLAG_BINARY_EVENT) != 0
    }

    /// Extracts `(tag, message)` from a standard text logcat payload `[priority:1][tag\0][message\0]`.
    pub fn parse_text_tag_and_message(&self) -> Option<(String, String)> {
        if self.is_binary_event() || self.payload.len() < 2 {
            return None;
        }
        let body = &self.payload[1..];
        let tag_end = body.iter().position(|&b| b == 0)?;
        let tag = String::from_utf8_lossy(&body[..tag_end]).into_owned();
        let rest = &body[tag_end + 1..];
        let msg_bytes = rest.strip_suffix(&[0]).unwrap_or(rest);
        let msg = String::from_utf8_lossy(msg_bytes).into_owned();
        Some((tag, msg))
    }
}

/// `0x11 LogOverflowStats` payload reporting `logcat -S` / `-g` drop monitoring.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LogOverflowStatsPayload {
    /// Device `CLOCK_BOOTTIME` when the stats probe completed.
    pub boottime_ns: i64,
    /// Honest completeness determination (`Verified`, `OverflowDetected`, `Unverified`).
    pub completeness: WireLogCompleteness,
    /// Number of `logd` buffers parsed (`0` when `Unverified`).
    pub buffers_count: u16,
    /// Non-negative counter delta since the previous probe, or `-1` if `Unverified`.
    pub pruned_delta: i64,
    /// Cumulative pruned/dropped count if reported, or `-1` if `Unverified`.
    pub pruned_cumulative: i64,
    /// Concise diagnostic summary.
    pub detail: String,
}

/// `0x20 TouchEvent` payload carrying one `getevent -lt` input event.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TouchEventPayload {
    /// Device `CLOCK_BOOTTIME` stamped immediately when the line was read on device.
    pub device_read_boottime_ns: i64,
    /// Kernel `input_event.time.tv_sec` (`CLOCK_MONOTONIC`), or `-1` if absent.
    pub event_sec: i64,
    /// Kernel `input_event.time.tv_usec` (`0..999_999`), or `-1` if absent.
    pub event_usec: i32,
    /// Linux input event type (`0x0000=EV_SYN`, `0x0001=EV_KEY`, `0x0003=EV_ABS`, `0xFFFF=unknown`).
    pub ev_type: u16,
    /// Linux input event code (e.g., `0x0035=ABS_MT_POSITION_X`, `0xFFFF=unknown`).
    pub ev_code: u16,
    /// Signed 32-bit event value.
    pub ev_value: i32,
    /// Input device path (e.g., `"/dev/input/event1"`).
    pub device_path: String,
    /// Trimmed `getevent -lt` line (capped at 256 bytes on the agent).
    pub raw_line: String,
}

impl TouchEventPayload {
    /// Converts `(event_sec, event_usec)` into device `CLOCK_MONOTONIC` nanoseconds when valid.
    pub fn event_monotonic_ns(&self) -> Option<i64> {
        if self.event_sec < 0 || !(0..1_000_000).contains(&self.event_usec) {
            return None;
        }
        self.event_sec
            .checked_mul(1_000_000_000)?
            .checked_add(i64::from(self.event_usec).checked_mul(1_000)?)
    }
}

/// `0x30 Shutdown` payload for bidirectional graceful termination.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ShutdownPayload {
    /// Sender `CLOCK_BOOTTIME` (`0` when sent by host).
    pub boottime_ns: i64,
    /// Termination status code.
    pub code: ShutdownCode,
    /// Short UTF-8 termination reason.
    pub reason: String,
}

/// Typed union of all 9 ARB v1 wire messages defined in `device-agent/PROTOCOL.md`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WireMessage {
    /// `0x01 Hello`.
    Hello(HelloPayload),
    /// `0x02 Heartbeat`.
    Heartbeat(HeartbeatPayload),
    /// `0x03 Ping`.
    Ping(PingPayload),
    /// `0x04 Pong`.
    Pong(PongPayload),
    /// `0x05 StreamState`.
    StreamState(StreamStatePayload),
    /// `0x10 LogRecord`.
    LogRecord(LogRecordPayload),
    /// `0x11 LogOverflowStats`.
    LogOverflowStats(LogOverflowStatsPayload),
    /// `0x20 TouchEvent`.
    TouchEvent(TouchEventPayload),
    /// `0x30 Shutdown`.
    Shutdown(ShutdownPayload),
}

impl WireMessage {
    /// Returns the wire [`MessageType`] matching this variant.
    pub const fn msg_type(&self) -> MessageType {
        match self {
            Self::Hello(_) => MessageType::Hello,
            Self::Heartbeat(_) => MessageType::Heartbeat,
            Self::Ping(_) => MessageType::Ping,
            Self::Pong(_) => MessageType::Pong,
            Self::StreamState(_) => MessageType::StreamState,
            Self::LogRecord(_) => MessageType::LogRecord,
            Self::LogOverflowStats(_) => MessageType::LogOverflowStats,
            Self::TouchEvent(_) => MessageType::TouchEvent,
            Self::Shutdown(_) => MessageType::Shutdown,
        }
    }

    /// Returns the default owning [`StreamId`] for this message variant.
    pub const fn default_stream_id(&self) -> StreamId {
        match self {
            Self::Hello(_)
            | Self::Heartbeat(_)
            | Self::Ping(_)
            | Self::Pong(_)
            | Self::Shutdown(_) => StreamId::Control,
            Self::StreamState(s) => s.target_stream_id,
            Self::LogRecord(_) | Self::LogOverflowStats(_) => StreamId::Logcat,
            Self::TouchEvent(_) => StreamId::Input,
        }
    }

    /// Encodes the payload bytes according to `device-agent/PROTOCOL.md`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] if any `str16` field exceeds 65,535 UTF-8 bytes,
    /// a `Heartbeat` exceeds 255 streams, or a `bytes32` field exceeds [`DEFAULT_MAX_PAYLOAD_LEN`].
    pub fn encode_payload(&self) -> Result<Vec<u8>, WireError> {
        match self {
            Self::Hello(p) => encode_hello(p),
            Self::Heartbeat(p) => encode_heartbeat(p),
            Self::Ping(p) => Ok(encode_ping(*p)),
            Self::Pong(p) => Ok(encode_pong(*p)),
            Self::StreamState(p) => encode_stream_state(p),
            Self::LogRecord(p) => encode_log_record(p),
            Self::LogOverflowStats(p) => encode_log_overflow_stats(p),
            Self::TouchEvent(p) => encode_touch_event(p),
            Self::Shutdown(p) => encode_shutdown(p),
        }
    }

    /// Wraps this message in a [`WireFrame`] ready for transmission.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] if payload encoding or frame length limits fail.
    pub fn to_frame(
        &self,
        stream_id: StreamId,
        flags: FrameFlags,
        seq: u32,
    ) -> Result<WireFrame, WireError> {
        let payload = self.encode_payload()?;
        WireFrame::new(stream_id, self.msg_type(), flags, seq, payload)
    }

    /// Decodes a validated [`WireFrame`] into its typed [`WireMessage`].
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] if the payload is truncated, contains invalid UTF-8
    /// or unknown discriminants, exceeds safety caps, or has unconsumed trailing bytes.
    pub fn decode_frame(frame: &WireFrame) -> Result<Self, WireError> {
        Self::decode_payload(frame.header.msg_type, &frame.payload)
    }

    /// Decodes raw payload bytes for `msg_type`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] if decoding fails or trailing bytes remain.
    pub fn decode_payload(msg_type: MessageType, payload: &[u8]) -> Result<Self, WireError> {
        match msg_type {
            MessageType::Hello => decode_hello(payload).map(Self::Hello),
            MessageType::Heartbeat => decode_heartbeat(payload).map(Self::Heartbeat),
            MessageType::Ping => decode_ping(payload).map(Self::Ping),
            MessageType::Pong => decode_pong(payload).map(Self::Pong),
            MessageType::StreamState => decode_stream_state(payload).map(Self::StreamState),
            MessageType::LogRecord => decode_log_record(payload).map(Self::LogRecord),
            MessageType::LogOverflowStats => {
                decode_log_overflow_stats(payload).map(Self::LogOverflowStats)
            }
            MessageType::TouchEvent => decode_touch_event(payload).map(Self::TouchEvent),
            MessageType::Shutdown => decode_shutdown(payload).map(Self::Shutdown),
        }
    }
}

// ---------------- Payload Encoders ----------------

/// Encodes a [`HelloPayload`] into Little-Endian wire bytes.
///
/// # Errors
///
/// Returns [`WireError::StringTooLong`] if any `str16` field exceeds 65,535 bytes.
pub fn encode_hello(h: &HelloPayload) -> Result<Vec<u8>, WireError> {
    let mut w = PayloadWriter::with_capacity(128);
    w.put_u16(h.protocol_version);
    w.put_u32(h.api_level);
    w.put_i32(h.boot_count);
    w.put_u8(h.boot_id_source.to_wire());
    w.put_u8(u8::from(h.is_emulator));
    w.put_u32(h.heartbeat_interval_ms);
    w.put_str16("boot_id", &h.boot_id)?;
    w.put_str16("model", &h.model)?;
    w.put_str16("manufacturer", &h.manufacturer)?;
    w.put_str16("agent_version", &h.agent_version)?;
    w.put_str16("boot_id_detail", &h.boot_id_detail)?;
    Ok(w.into_bytes())
}

/// Encodes a [`HeartbeatPayload`] into Little-Endian wire bytes (`25 + 20 * N` bytes).
///
/// # Errors
///
/// Returns [`WireError::CollectionTooLarge`] if `hb.streams.len() > 255`.
pub fn encode_heartbeat(hb: &HeartbeatPayload) -> Result<Vec<u8>, WireError> {
    let count = u8::try_from(hb.streams.len()).map_err(|_| WireError::CollectionTooLarge {
        field: "heartbeat.streams",
        len: hb.streams.len(),
        max: usize::from(u8::MAX),
    })?;
    let mut w = PayloadWriter::with_capacity(25 + hb.streams.len() * STREAM_HEALTH_ENTRY_LEN);
    w.put_i64(hb.boottime_ns);
    w.put_i64(hb.monotonic_ns);
    w.put_i64(hb.realtime_ns);
    w.put_u8(count);
    for entry in &hb.streams {
        w.put_u8(entry.stream_id.to_wire());
        w.put_u8(entry.state.to_wire());
        w.put_u16(entry.restart_count);
        w.put_u32(entry.emitted_seq);
        w.put_i64(entry.last_pts_ns);
        w.put_u32(0); // reserved
    }
    Ok(w.into_bytes())
}

/// Encodes a [`PingPayload`] into 8 Little-Endian wire bytes.
pub fn encode_ping(p: PingPayload) -> Vec<u8> {
    p.nonce.to_le_bytes().to_vec()
}

/// Encodes a [`PongPayload`] into 40 Little-Endian wire bytes.
pub fn encode_pong(p: PongPayload) -> Vec<u8> {
    let mut w = PayloadWriter::with_capacity(PONG_PAYLOAD_LEN);
    w.put_u64(p.nonce);
    w.put_i64(p.boottime_before_ns);
    w.put_i64(p.monotonic_ns);
    w.put_i64(p.realtime_ns);
    w.put_i64(p.boottime_after_ns);
    w.into_bytes()
}

/// Encodes a [`StreamStatePayload`] into Little-Endian wire bytes.
///
/// # Errors
///
/// Returns [`WireError::StringTooLong`] if `reason` exceeds 65,535 UTF-8 bytes.
pub fn encode_stream_state(s: &StreamStatePayload) -> Result<Vec<u8>, WireError> {
    let mut w = PayloadWriter::with_capacity(64);
    w.put_u8(s.target_stream_id.to_wire());
    w.put_u8(s.state.to_wire());
    w.put_u16(s.restart_count);
    w.put_i64(s.boottime_ns);
    w.put_u32(s.emitted_seq);
    w.put_str16("reason", &s.reason)?;
    Ok(w.into_bytes())
}

/// Encodes a [`LogRecordPayload`] into Little-Endian wire bytes (`40 + payload.len()` bytes).
///
/// # Errors
///
/// Returns [`WireError::PayloadTooLarge`] if `r.payload.len()` exceeds [`DEFAULT_MAX_PAYLOAD_LEN`].
pub fn encode_log_record(r: &LogRecordPayload) -> Result<Vec<u8>, WireError> {
    let payload_len_u64 =
        u64::try_from(r.payload.len()).map_err(|_| WireError::PayloadTooLarge {
            declared: u64::MAX,
            max_allowed: DEFAULT_MAX_PAYLOAD_LEN,
        })?;
    if payload_len_u64 > DEFAULT_MAX_PAYLOAD_LEN {
        return Err(WireError::PayloadTooLarge {
            declared: payload_len_u64,
            max_allowed: DEFAULT_MAX_PAYLOAD_LEN,
        });
    }
    let payload_len_u32 =
        u32::try_from(r.payload.len()).map_err(|_| WireError::PayloadTooLarge {
            declared: payload_len_u64,
            max_allowed: DEFAULT_MAX_PAYLOAD_LEN,
        })?;
    let mut w = PayloadWriter::with_capacity(40 + r.payload.len());
    w.put_i64(r.device_read_boottime_ns);
    w.put_u32(r.log_sec);
    w.put_u32(r.log_nsec);
    w.put_i32(r.pid);
    w.put_i32(r.tid);
    w.put_u32(r.lid);
    w.put_u32(r.uid);
    w.put_u16(r.hdr_size);
    w.put_u8(r.priority);
    w.put_u8(r.record_flags);
    w.put_u32(payload_len_u32);
    w.put_raw(&r.payload);
    Ok(w.into_bytes())
}

/// Encodes a [`LogOverflowStatsPayload`] into Little-Endian wire bytes.
///
/// # Errors
///
/// Returns [`WireError::StringTooLong`] if `detail` exceeds 65,535 UTF-8 bytes.
pub fn encode_log_overflow_stats(s: &LogOverflowStatsPayload) -> Result<Vec<u8>, WireError> {
    let mut w = PayloadWriter::with_capacity(64);
    w.put_i64(s.boottime_ns);
    w.put_u8(s.completeness.to_wire());
    w.put_u8(0); // reserved
    w.put_u16(s.buffers_count);
    w.put_i64(s.pruned_delta);
    w.put_i64(s.pruned_cumulative);
    w.put_str16("detail", &s.detail)?;
    Ok(w.into_bytes())
}

/// Encodes a [`TouchEventPayload`] into Little-Endian wire bytes.
///
/// # Errors
///
/// Returns [`WireError::StringTooLong`] if `device_path` or `raw_line` exceeds 65,535 UTF-8 bytes.
pub fn encode_touch_event(e: &TouchEventPayload) -> Result<Vec<u8>, WireError> {
    let mut w = PayloadWriter::with_capacity(96);
    w.put_i64(e.device_read_boottime_ns);
    w.put_i64(e.event_sec);
    w.put_i32(e.event_usec);
    w.put_u16(e.ev_type);
    w.put_u16(e.ev_code);
    w.put_i32(e.ev_value);
    w.put_str16("device_path", &e.device_path)?;
    w.put_str16("raw_line", &e.raw_line)?;
    Ok(w.into_bytes())
}

/// Encodes a [`ShutdownPayload`] into Little-Endian wire bytes.
///
/// # Errors
///
/// Returns [`WireError::StringTooLong`] if `reason` exceeds 65,535 UTF-8 bytes.
pub fn encode_shutdown(s: &ShutdownPayload) -> Result<Vec<u8>, WireError> {
    let mut w = PayloadWriter::with_capacity(32);
    w.put_i64(s.boottime_ns);
    w.put_u16(s.code.to_wire());
    w.put_str16("reason", &s.reason)?;
    Ok(w.into_bytes())
}

// ---------------- Payload Decoders ----------------

/// Decodes a [`HelloPayload`] from Little-Endian wire bytes.
///
/// # Errors
///
/// Returns [`WireError`] on truncation, invalid UTF-8, unknown enum discriminants,
/// or trailing unconsumed bytes.
pub fn decode_hello(data: &[u8]) -> Result<HelloPayload, WireError> {
    let mut r = PayloadReader::new(data);
    let protocol_version = r.get_u16("hello.protocol_version")?;
    let api_level = r.get_u32("hello.api_level")?;
    let boot_count = r.get_i32("hello.boot_count")?;
    let boot_id_source = BootIdSource::try_from(r.get_u8("hello.boot_id_source")?)?;
    let is_emulator = r.get_u8("hello.is_emulator")? != 0;
    let heartbeat_interval_ms = r.get_u32("hello.heartbeat_interval_ms")?;
    let boot_id = r.get_str16("hello.boot_id")?;
    let model = r.get_str16("hello.model")?;
    let manufacturer = r.get_str16("hello.manufacturer")?;
    let agent_version = r.get_str16("hello.agent_version")?;
    let boot_id_detail = r.get_str16("hello.boot_id_detail")?;
    r.finish("Hello")?;
    Ok(HelloPayload {
        protocol_version,
        api_level,
        boot_count,
        boot_id_source,
        is_emulator,
        heartbeat_interval_ms,
        boot_id,
        model,
        manufacturer,
        agent_version,
        boot_id_detail,
    })
}

/// Decodes a [`HeartbeatPayload`] from Little-Endian wire bytes.
///
/// # Errors
///
/// Returns [`WireError`] on truncation, unknown stream/state discriminants,
/// or trailing unconsumed bytes.
pub fn decode_heartbeat(data: &[u8]) -> Result<HeartbeatPayload, WireError> {
    let mut r = PayloadReader::new(data);
    let boottime_ns = r.get_i64("heartbeat.boottime_ns")?;
    let monotonic_ns = r.get_i64("heartbeat.monotonic_ns")?;
    let realtime_ns = r.get_i64("heartbeat.realtime_ns")?;
    let count = usize::from(r.get_u8("heartbeat.stream_count")?);
    let mut streams = Vec::with_capacity(count);
    for _ in 0..count {
        let stream_id = StreamId::try_from(r.get_u8("heartbeat.stream.stream_id")?)?;
        let state = StreamLifecycleState::try_from(r.get_u8("heartbeat.stream.state")?)?;
        let restart_count = r.get_u16("heartbeat.stream.restart_count")?;
        let emitted_seq = r.get_u32("heartbeat.stream.emitted_seq")?;
        let last_pts_ns = r.get_i64("heartbeat.stream.last_pts_ns")?;
        let _reserved = r.get_u32("heartbeat.stream.reserved")?;
        streams.push(StreamHealthEntry {
            stream_id,
            state,
            restart_count,
            emitted_seq,
            last_pts_ns,
        });
    }
    r.finish("Heartbeat")?;
    Ok(HeartbeatPayload {
        boottime_ns,
        monotonic_ns,
        realtime_ns,
        streams,
    })
}

/// Decodes a [`PingPayload`] from 8 Little-Endian wire bytes.
///
/// # Errors
///
/// Returns [`WireError`] if `data.len() != 8`.
pub fn decode_ping(data: &[u8]) -> Result<PingPayload, WireError> {
    let mut r = PayloadReader::new(data);
    let nonce = r.get_u64("ping.nonce")?;
    r.finish("Ping")?;
    Ok(PingPayload { nonce })
}

/// Decodes a [`PongPayload`] from 40 Little-Endian wire bytes.
///
/// # Errors
///
/// Returns [`WireError`] if `data.len() != 40`.
pub fn decode_pong(data: &[u8]) -> Result<PongPayload, WireError> {
    let mut r = PayloadReader::new(data);
    let nonce = r.get_u64("pong.nonce")?;
    let boottime_before_ns = r.get_i64("pong.boottime_before_ns")?;
    let monotonic_ns = r.get_i64("pong.monotonic_ns")?;
    let realtime_ns = r.get_i64("pong.realtime_ns")?;
    let boottime_after_ns = r.get_i64("pong.boottime_after_ns")?;
    r.finish("Pong")?;
    Ok(PongPayload {
        nonce,
        boottime_before_ns,
        monotonic_ns,
        realtime_ns,
        boottime_after_ns,
    })
}

/// Decodes a [`StreamStatePayload`] from Little-Endian wire bytes.
///
/// # Errors
///
/// Returns [`WireError`] on truncation, unknown discriminants, invalid UTF-8, or trailing bytes.
pub fn decode_stream_state(data: &[u8]) -> Result<StreamStatePayload, WireError> {
    let mut r = PayloadReader::new(data);
    let target_stream_id = StreamId::try_from(r.get_u8("stream_state.target_stream_id")?)?;
    let state = StreamLifecycleState::try_from(r.get_u8("stream_state.state")?)?;
    let restart_count = r.get_u16("stream_state.restart_count")?;
    let boottime_ns = r.get_i64("stream_state.boottime_ns")?;
    let emitted_seq = r.get_u32("stream_state.emitted_seq")?;
    let reason = r.get_str16("stream_state.reason")?;
    r.finish("StreamState")?;
    Ok(StreamStatePayload {
        target_stream_id,
        state,
        restart_count,
        boottime_ns,
        emitted_seq,
        reason,
    })
}

/// Decodes a [`LogRecordPayload`] from Little-Endian wire bytes.
///
/// # Errors
///
/// Returns [`WireError`] on truncation, oversized `bytes32`, or trailing bytes.
pub fn decode_log_record(data: &[u8]) -> Result<LogRecordPayload, WireError> {
    let mut r = PayloadReader::new(data);
    let device_read_boottime_ns = r.get_i64("log_record.device_read_boottime_ns")?;
    let seconds = r.get_u32("log_record.log_sec")?;
    let subsec_nanos = r.get_u32("log_record.log_nsec")?;
    let pid = r.get_i32("log_record.pid")?;
    let tid = r.get_i32("log_record.tid")?;
    let lid = r.get_u32("log_record.lid")?;
    let uid = r.get_u32("log_record.uid")?;
    let hdr_size = r.get_u16("log_record.hdr_size")?;
    let priority = r.get_u8("log_record.priority")?;
    let record_flags = r.get_u8("log_record.record_flags")?;
    let payload = r.get_bytes32("log_record.payload", DEFAULT_MAX_PAYLOAD_LEN)?;
    r.finish("LogRecord")?;
    Ok(LogRecordPayload {
        device_read_boottime_ns,
        log_sec: seconds,
        log_nsec: subsec_nanos,
        pid,
        tid,
        lid,
        uid,
        hdr_size,
        priority,
        record_flags,
        payload,
    })
}

/// Decodes a [`LogOverflowStatsPayload`] from Little-Endian wire bytes.
///
/// # Errors
///
/// Returns [`WireError`] on truncation, unknown completeness, invalid UTF-8, or trailing bytes.
pub fn decode_log_overflow_stats(data: &[u8]) -> Result<LogOverflowStatsPayload, WireError> {
    let mut r = PayloadReader::new(data);
    let boottime_ns = r.get_i64("log_overflow_stats.boottime_ns")?;
    let completeness = WireLogCompleteness::try_from(r.get_u8("log_overflow_stats.completeness")?)?;
    let _reserved = r.get_u8("log_overflow_stats.reserved")?;
    let buffers_count = r.get_u16("log_overflow_stats.buffers_count")?;
    let pruned_delta = r.get_i64("log_overflow_stats.pruned_delta")?;
    let pruned_cumulative = r.get_i64("log_overflow_stats.pruned_cumulative")?;
    let detail = r.get_str16("log_overflow_stats.detail")?;
    r.finish("LogOverflowStats")?;
    Ok(LogOverflowStatsPayload {
        boottime_ns,
        completeness,
        buffers_count,
        pruned_delta,
        pruned_cumulative,
        detail,
    })
}

/// Decodes a [`TouchEventPayload`] from Little-Endian wire bytes.
///
/// # Errors
///
/// Returns [`WireError`] on truncation, invalid UTF-8, or trailing bytes.
pub fn decode_touch_event(data: &[u8]) -> Result<TouchEventPayload, WireError> {
    let mut r = PayloadReader::new(data);
    let device_read_boottime_ns = r.get_i64("touch_event.device_read_boottime_ns")?;
    let kernel_seconds = r.get_i64("touch_event.event_sec")?;
    let kernel_micros = r.get_i32("touch_event.event_usec")?;
    let ev_type = r.get_u16("touch_event.ev_type")?;
    let ev_code = r.get_u16("touch_event.ev_code")?;
    let ev_value = r.get_i32("touch_event.ev_value")?;
    let device_path = r.get_str16("touch_event.device_path")?;
    let raw_line = r.get_str16("touch_event.raw_line")?;
    r.finish("TouchEvent")?;
    Ok(TouchEventPayload {
        device_read_boottime_ns,
        event_sec: kernel_seconds,
        event_usec: kernel_micros,
        ev_type,
        ev_code,
        ev_value,
        device_path,
        raw_line,
    })
}

/// Decodes a [`ShutdownPayload`] from Little-Endian wire bytes.
///
/// # Errors
///
/// Returns [`WireError`] on truncation, unknown shutdown code, invalid UTF-8, or trailing bytes.
pub fn decode_shutdown(data: &[u8]) -> Result<ShutdownPayload, WireError> {
    let mut r = PayloadReader::new(data);
    let boottime_ns = r.get_i64("shutdown.boottime_ns")?;
    let code = ShutdownCode::try_from(r.get_u16("shutdown.code")?)?;
    let reason = r.get_str16("shutdown.reason")?;
    r.finish("Shutdown")?;
    Ok(ShutdownPayload {
        boottime_ns,
        code,
        reason,
    })
}

// ---------------- Internal Little-Endian Primitive Helpers ----------------

struct PayloadWriter {
    buf: Vec<u8>,
}

impl PayloadWriter {
    fn with_capacity(cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap),
        }
    }

    fn put_u8(&mut self, value: u8) {
        self.buf.push(value);
    }

    fn put_u16(&mut self, value: u16) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    fn put_u32(&mut self, value: u32) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    fn put_i32(&mut self, value: i32) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    fn put_u64(&mut self, value: u64) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    fn put_i64(&mut self, value: i64) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    fn put_raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    fn put_str16(&mut self, field: &'static str, value: &str) -> Result<(), WireError> {
        let bytes = value.as_bytes();
        let len = u16::try_from(bytes.len()).map_err(|_| WireError::StringTooLong {
            field,
            len: bytes.len(),
        })?;
        self.put_u16(len);
        self.buf.extend_from_slice(bytes);
        Ok(())
    }

    fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
}

struct PayloadReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> PayloadReader<'a> {
    const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    const fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn take(&mut self, field: &'static str, n: usize) -> Result<&'a [u8], WireError> {
        if self.remaining() < n {
            return Err(WireError::TruncatedPayload {
                field,
                needed: n,
                remaining: self.remaining(),
            });
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    fn get_u8(&mut self, field: &'static str) -> Result<u8, WireError> {
        let b = self.take(field, 1)?;
        Ok(b[0])
    }

    fn get_u16(&mut self, field: &'static str) -> Result<u16, WireError> {
        let b = self.take(field, 2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn get_u32(&mut self, field: &'static str) -> Result<u32, WireError> {
        let b = self.take(field, 4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn get_i32(&mut self, field: &'static str) -> Result<i32, WireError> {
        let b = self.take(field, 4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn get_u64(&mut self, field: &'static str) -> Result<u64, WireError> {
        let b = self.take(field, 8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn get_i64(&mut self, field: &'static str) -> Result<i64, WireError> {
        let b = self.take(field, 8)?;
        Ok(i64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn get_str16(&mut self, field: &'static str) -> Result<String, WireError> {
        let len = usize::from(self.get_u16(field)?);
        let raw = self.take(field, len)?.to_vec();
        String::from_utf8(raw).map_err(|source| WireError::InvalidUtf8 { field, source })
    }

    fn get_bytes32(
        &mut self,
        field: &'static str,
        max_payload_len: u64,
    ) -> Result<Vec<u8>, WireError> {
        let len_u32 = self.get_u32(field)?;
        let len_u64 = u64::from(len_u32);
        if len_u64 > max_payload_len {
            return Err(WireError::PayloadTooLarge {
                declared: len_u64,
                max_allowed: max_payload_len,
            });
        }
        let len = usize::try_from(len_u32).map_err(|_| WireError::PayloadTooLarge {
            declared: len_u64,
            max_allowed: max_payload_len,
        })?;
        Ok(self.take(field, len)?.to_vec())
    }

    const fn finish(&self, message: &'static str) -> Result<(), WireError> {
        if self.remaining() != 0 {
            return Err(WireError::TrailingBytes {
                message,
                remaining: self.remaining(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{
        BootIdSource, DEFAULT_MAX_PAYLOAD_LEN, FrameDeframer, FrameFlags, FrameHeader, HEADER_SIZE,
        HeartbeatPayload, HelloPayload, LOG_RECORD_FLAG_BINARY_EVENT, LogOverflowStatsPayload,
        LogRecordPayload, MAGIC, MessageType, PROTOCOL_VERSION, PingPayload, PongPayload,
        ShutdownCode, ShutdownPayload, StreamHealthEntry, StreamId, StreamLifecycleState,
        StreamStatePayload, TouchEventPayload, WireError, WireFrame, WireLogCompleteness,
        WireMessage, decode_heartbeat, decode_hello, decode_log_overflow_stats, decode_log_record,
        decode_ping, decode_pong, decode_shutdown, decode_stream_state, decode_touch_event,
        encode_heartbeat, encode_hello, encode_log_overflow_stats, encode_log_record, encode_ping,
        encode_pong, encode_shutdown, encode_stream_state, encode_touch_event,
    };

    #[test]
    fn wire_frame_header_matches_java_protocol_test_bit_for_bit() {
        // Exact vector from device-agent/companion/.../ProtocolTest.java::testWireFrameHeaderLayoutAndRoundTrip
        let payload = vec![0x11, 0x22, 0x33, 0x44, 0x55];
        let frame = WireFrame::new(
            StreamId::Logcat,
            MessageType::LogRecord,
            FrameFlags::RECOVERED,
            42,
            payload.clone(),
        )
        .expect("frame construction must succeed");

        let wire = frame.encode().expect("frame encode must succeed");
        assert_eq!(wire.len(), 25, "20-byte header + 5-byte payload");
        assert_eq!(wire[0], 0x41, "magic[0] must be 'A'");
        assert_eq!(wire[1], 0x52, "magic[1] must be 'R'");
        assert_eq!(wire[2], 1, "version must be 1");
        assert_eq!(wire[3], 1, "stream_id must be Logcat (1)");
        assert_eq!(wire[4], 0x10, "msg_type must be LogRecord (0x10)");
        assert_eq!(wire[5], 0x04, "flags must be FLAG_RECOVERED (0x04)");
        assert_eq!(&wire[6..8], &[0, 0], "reserved must be 0");
        assert_eq!(&wire[8..12], &42u32.to_le_bytes(), "seq must be 42 LE");
        assert_eq!(
            &wire[12..20],
            &5u64.to_le_bytes(),
            "payload_len must be 5 LE"
        );
        assert_eq!(&wire[20..], payload.as_slice(), "payload bytes must follow");

        let mut cursor = Cursor::new(wire.clone());
        let decoded = WireFrame::read_from(&mut cursor, DEFAULT_MAX_PAYLOAD_LEN)
            .expect("frame decode must succeed")
            .expect("frame must be present");
        assert_eq!(decoded, frame, "decoded frame must match original");
        assert_eq!(
            WireFrame::read_from(&mut cursor, DEFAULT_MAX_PAYLOAD_LEN)
                .expect("clean EOF must return Ok(None)"),
            None,
            "EOF at frame boundary must return None"
        );

        // Corrupt magic must be rejected.
        let mut bad_magic = wire;
        bad_magic[0] = 0x00;
        let err = WireFrame::read_from(&mut Cursor::new(bad_magic), DEFAULT_MAX_PAYLOAD_LEN)
            .expect_err("corrupt magic must fail");
        assert!(
            matches!(
                err,
                WireError::InvalidMagic {
                    found: [0x00, 0x52]
                }
            ),
            "expected InvalidMagic, got {err:?}"
        );
    }

    #[test]
    fn control_messages_round_trip_matching_java_vectors() {
        // 1. Hello (exact ProtocolTest.java values)
        let hello = HelloPayload {
            protocol_version: 1,
            api_level: 37,
            boot_count: 4,
            boot_id_source: BootIdSource::KernelProcBootId,
            is_emulator: true,
            heartbeat_interval_ms: 300,
            boot_id: "c729eaec-5428-4a38-b957-441109da33f7".into(),
            model: "sdk_gphone64_arm64".into(),
            manufacturer: "Google".into(),
            agent_version: "0.1.0".into(),
            boot_id_detail: String::new(),
        };
        let hello_bytes = encode_hello(&hello).expect("encode hello");
        assert_eq!(
            decode_hello(&hello_bytes).expect("decode hello"),
            hello,
            "Hello must round-trip losslessly"
        );

        // 2. Heartbeat (exact 25 + 3 * 20 = 85 bytes)
        let hb = HeartbeatPayload {
            boottime_ns: 500_000_000,
            monotonic_ns: 400_000_000,
            realtime_ns: 1_700_000_000_000_000_000,
            streams: vec![
                StreamHealthEntry {
                    stream_id: StreamId::Control,
                    state: StreamLifecycleState::Running,
                    restart_count: 0,
                    emitted_seq: 10,
                    last_pts_ns: 1000,
                },
                StreamHealthEntry {
                    stream_id: StreamId::Logcat,
                    state: StreamLifecycleState::Running,
                    restart_count: 1,
                    emitted_seq: 999,
                    last_pts_ns: 2000,
                },
                StreamHealthEntry {
                    stream_id: StreamId::Input,
                    state: StreamLifecycleState::Stopped,
                    restart_count: 0,
                    emitted_seq: 0,
                    last_pts_ns: 0,
                },
            ],
        };
        let hb_bytes = encode_heartbeat(&hb).expect("encode heartbeat");
        assert_eq!(
            hb_bytes.len(),
            25 + 3 * 20,
            "Heartbeat byte size must be exactly 25 + 20 * N"
        );
        assert_eq!(
            decode_heartbeat(&hb_bytes).expect("decode heartbeat"),
            hb,
            "Heartbeat must round-trip losslessly"
        );

        // 3 & 4. Ping & Pong
        let ping_req = PingPayload {
            nonce: 0xDEAD_BEEF_CAFE_BABE,
        };
        let req_wire = encode_ping(ping_req);
        assert_eq!(req_wire.len(), 8, "Ping must be 8 bytes");
        assert_eq!(
            decode_ping(&req_wire).expect("decode ping"),
            ping_req,
            "Ping must round-trip losslessly"
        );

        let pong_reply = PongPayload {
            nonce: 0xDEAD_BEEF_CAFE_BABE,
            boottime_before_ns: 1000,
            monotonic_ns: 900,
            realtime_ns: 1_700_000_000_000_000_000,
            boottime_after_ns: 1050,
        };
        let reply_wire = encode_pong(pong_reply);
        assert_eq!(reply_wire.len(), 40, "Pong must be 40 bytes");
        assert_eq!(
            decode_pong(&reply_wire).expect("decode pong"),
            pong_reply,
            "Pong must round-trip losslessly"
        );
        assert_eq!(
            pong_reply.device_bracket_ns(),
            Some(50),
            "device bracket must equal after - before"
        );
        assert_eq!(
            pong_reply.sync_uncertainty_ns(200),
            Some(125),
            "uncertainty must equal rtt/2 + bracket/2"
        );
    }

    #[test]
    fn stream_state_log_touch_and_shutdown_round_trip_matching_java_vectors() {
        // 5. StreamState
        let st = StreamStatePayload {
            target_stream_id: StreamId::Logcat,
            state: StreamLifecycleState::Error,
            restart_count: 2,
            boottime_ns: 123_456_789,
            emitted_seq: 55,
            reason: "process_exit:137".into(),
        };
        let st_bytes = encode_stream_state(&st).expect("encode stream state");
        assert_eq!(
            decode_stream_state(&st_bytes).expect("decode stream state"),
            st,
            "StreamState must round-trip losslessly"
        );

        // 6. LogRecord (exact 40 + body.len())
        let body = b"\x04MyTag\0Hello world\0".to_vec();
        let log = LogRecordPayload {
            device_read_boottime_ns: 987_654_321,
            log_sec: 1_700_000_000,
            log_nsec: 123_456_789,
            pid: 421,
            tid: 422,
            lid: 0,
            uid: 1000,
            hdr_size: 28,
            priority: 4,
            record_flags: 0,
            payload: body.clone(),
        };
        let log_bytes = encode_log_record(&log).expect("encode log record");
        assert_eq!(
            log_bytes.len(),
            40 + body.len(),
            "LogRecord byte size must be 40 + payload.len()"
        );
        let decoded_log = decode_log_record(&log_bytes).expect("decode log record");
        assert_eq!(decoded_log, log, "LogRecord must round-trip losslessly");
        assert_eq!(
            decoded_log.parse_text_tag_and_message(),
            Some(("MyTag".into(), "Hello world".into())),
            "text logcat tag and message must parse cleanly"
        );

        // 7. LogOverflowStats
        let stats = LogOverflowStatsPayload {
            boottime_ns: 777_777,
            completeness: WireLogCompleteness::Verified,
            buffers_count: 4,
            pruned_delta: 0,
            pruned_cumulative: 12,
            detail: "main:12 system:0".into(),
        };
        let stats_bytes = encode_log_overflow_stats(&stats).expect("encode overflow stats");
        assert_eq!(
            decode_log_overflow_stats(&stats_bytes).expect("decode overflow stats"),
            stats,
            "LogOverflowStats must round-trip losslessly"
        );

        // 8. TouchEvent
        let touch = TouchEventPayload {
            device_read_boottime_ns: 888_888,
            event_sec: 123,
            event_usec: 456_789,
            ev_type: 0x0003,
            ev_code: 0x0035,
            ev_value: 16_384,
            device_path: "/dev/input/event1".into(),
            raw_line: "[ 123.456789] EV_ABS ABS_MT_POSITION_X 00004000".into(),
        };
        let touch_bytes = encode_touch_event(&touch).expect("encode touch event");
        let decoded_touch = decode_touch_event(&touch_bytes).expect("decode touch event");
        assert_eq!(
            decoded_touch, touch,
            "TouchEvent must round-trip losslessly"
        );
        assert_eq!(
            decoded_touch.event_monotonic_ns(),
            Some(123_456_789_000),
            "event_monotonic_ns must convert sec/usec to ns"
        );

        // 9. Shutdown
        let shutdown = ShutdownPayload {
            boottime_ns: 999,
            code: ShutdownCode::Normal,
            reason: "host_requested".into(),
        };
        let shutdown_bytes = encode_shutdown(&shutdown).expect("encode shutdown");
        assert_eq!(
            decode_shutdown(&shutdown_bytes).expect("decode shutdown"),
            shutdown,
            "Shutdown must round-trip losslessly"
        );
    }

    #[test]
    fn rejects_oversized_payload_truncated_frames_and_bad_version_without_allocating() {
        // Oversized payload_len (u64::MAX and max_payload_len + 1) rejected at header check.
        let mut oversized_hdr = [0u8; HEADER_SIZE];
        oversized_hdr[0..2].copy_from_slice(&MAGIC);
        oversized_hdr[2] = PROTOCOL_VERSION;
        oversized_hdr[3] = StreamId::Control.to_wire();
        oversized_hdr[4] = MessageType::Ping.to_wire();
        oversized_hdr[12..20].copy_from_slice(&u64::MAX.to_le_bytes());

        let err = FrameHeader::from_bytes(&oversized_hdr, DEFAULT_MAX_PAYLOAD_LEN)
            .expect_err("u64::MAX payload_len must fail immediately");
        assert!(
            matches!(
                err,
                WireError::PayloadTooLarge {
                    declared: u64::MAX,
                    max_allowed: DEFAULT_MAX_PAYLOAD_LEN
                }
            ),
            "expected PayloadTooLarge, got {err:?}"
        );

        let err_stream = WireFrame::read_from(&mut Cursor::new(oversized_hdr), 1024)
            .expect_err("reader must reject payload exceeding cap before allocating");
        assert!(
            matches!(
                err_stream,
                WireError::PayloadTooLarge {
                    declared: u64::MAX,
                    max_allowed: 1024
                }
            ),
            "expected PayloadTooLarge from reader, got {err_stream:?}"
        );

        // Bad protocol version rejected.
        let mut bad_ver = oversized_hdr;
        bad_ver[2] = 2;
        bad_ver[12..20].copy_from_slice(&8u64.to_le_bytes());
        assert!(
            matches!(
                FrameHeader::from_bytes(&bad_ver, DEFAULT_MAX_PAYLOAD_LEN),
                Err(WireError::UnsupportedVersion { found: 2 })
            ),
            "version 2 must be rejected"
        );

        // Truncated header (10 of 20 bytes).
        let truncated_hdr = &bad_ver[..10];
        assert!(
            matches!(
                WireFrame::read_from(&mut Cursor::new(truncated_hdr), DEFAULT_MAX_PAYLOAD_LEN),
                Err(WireError::TruncatedFrame {
                    expected: 20,
                    actual: 10
                })
            ),
            "half header must return TruncatedFrame"
        );

        // Truncated payload (declares 8 bytes, only 3 present).
        let full = WireMessage::Ping(PingPayload { nonce: 7 })
            .to_frame(StreamId::Control, FrameFlags::NONE, 1)
            .expect("build ping")
            .encode()
            .expect("encode ping");
        let truncated_body = &full[..HEADER_SIZE + 3];
        assert!(
            matches!(
                WireFrame::read_from(&mut Cursor::new(truncated_body), DEFAULT_MAX_PAYLOAD_LEN),
                Err(WireError::TruncatedFrame {
                    expected: 8,
                    actual: 3
                })
            ),
            "truncated payload must return TruncatedFrame"
        );

        // Trailing bytes inside typed payload rejected.
        let mut ping_with_trailing = encode_ping(PingPayload { nonce: 1 });
        ping_with_trailing.push(0xFF);
        assert!(
            matches!(
                decode_ping(&ping_with_trailing),
                Err(WireError::TrailingBytes {
                    message: "Ping",
                    remaining: 1
                })
            ),
            "trailing payload bytes must be rejected"
        );
    }

    #[test]
    fn incremental_deframer_handles_single_byte_chunks_and_multiple_frames() {
        let m1 = WireMessage::Ping(PingPayload { nonce: 101 });
        let m2 = WireMessage::LogRecord(LogRecordPayload {
            device_read_boottime_ns: 55_000,
            log_sec: 1_700_000_001,
            log_nsec: 500,
            pid: 10,
            tid: 11,
            lid: 2,
            uid: 10_000,
            hdr_size: 28,
            priority: 0,
            record_flags: LOG_RECORD_FLAG_BINARY_EVENT,
            payload: vec![1, 2, 3, 4],
        });

        let mut stream = Vec::new();
        stream.extend_from_slice(
            &m1.to_frame(StreamId::Control, FrameFlags::URGENT, 1)
                .expect("frame 1")
                .encode()
                .expect("encode 1"),
        );
        stream.extend_from_slice(
            &m2.to_frame(StreamId::Logcat, FrameFlags::RECOVERED, 2)
                .expect("frame 2")
                .encode()
                .expect("encode 2"),
        );

        let mut deframer = FrameDeframer::new();
        let mut decoded = Vec::new();
        for byte in stream {
            deframer.push_bytes(&[byte]);
            while let Some(frame) = deframer.next_frame().expect("deframe step must succeed") {
                decoded.push(WireMessage::decode_frame(&frame).expect("decode message"));
            }
        }
        deframer.finish().expect("no trailing bytes should remain");
        assert_eq!(decoded, vec![m1, m2], "both messages must deframe cleanly");
    }
}

/// Layer-1 PDD (K9b): 20-byte frame header validation and roundtrip.
#[cfg(kani)]
mod kani_proofs {
    use super::{FrameHeader, HEADER_SIZE, MAGIC, MessageType, PROTOCOL_VERSION, StreamId};

    #[kani::proof]
    fn k9_frame_header_accepts_iff_valid_and_roundtrips() {
        let bytes: [u8; HEADER_SIZE] = kani::any();
        let max: u64 = kani::any();
        let len = u64::from_le_bytes([
            bytes[12], bytes[13], bytes[14], bytes[15], bytes[16], bytes[17], bytes[18], bytes[19],
        ]);
        let valid = bytes[0] == MAGIC[0]
            && bytes[1] == MAGIC[1]
            && bytes[2] == PROTOCOL_VERSION
            && StreamId::try_from(bytes[3]).is_ok()
            && MessageType::try_from(bytes[4]).is_ok()
            && len <= max;
        let parsed = FrameHeader::from_bytes(&bytes, max);
        assert_eq!(parsed.is_ok(), valid, "accept iff PROTOCOL.md §2 valid");
        if let Ok(h) = parsed {
            assert_eq!(
                h.to_bytes(),
                bytes,
                "valid header roundtrips byte-identically"
            );
            assert_eq!(h.payload_len, len);
            assert_eq!(
                StreamId::try_from(h.stream_id.to_wire()).ok(),
                Some(h.stream_id)
            );
            assert_eq!(
                MessageType::try_from(h.msg_type.to_wire()).ok(),
                Some(h.msg_type)
            );
        }
    }

    #[kani::proof]
    fn k9_frame_header_encode_decode_full_u64_len() {
        let Ok(stream_id) = StreamId::try_from(kani::any::<u8>()) else {
            return;
        };
        let Ok(msg_type) = MessageType::try_from(kani::any::<u8>()) else {
            return;
        };
        let h = FrameHeader {
            version: PROTOCOL_VERSION,
            stream_id,
            msg_type,
            flags: super::FrameFlags::from_bits(kani::any()),
            reserved: kani::any(),
            seq: kani::any(),
            payload_len: kani::any(),
        };
        assert_eq!(
            FrameHeader::from_bytes(&h.to_bytes(), u64::MAX).ok(),
            Some(h)
        );
    }
}
