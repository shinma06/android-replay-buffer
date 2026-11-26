# ARB Device Agent Binary Wire Protocol (`PROTOCOL.md`)

Version: `1` (matches `SPEC-03` — Device Agent ↔ `arb-adb` binary framing contract).

## 1. Architecture & Transport

The device side consists of two cleanly separated `app_process` runtimes deployed to `/data/local/tmp/` without root, APK installation, or target-app modification:

1. **ARB Companion (`arb-companion.dex.jar`)**:
   - Owns **Stream 0 (Control/Beacon)**, **Stream 1 (Logcat)**, and **Stream 2 (Input/Taps)**.
   - Launched via ADB:
     ```sh
     CLASSPATH=/data/local/tmp/arb-companion.dex.jar \
       app_process / --nice-name=arb-companion \
       io.github.shinma06.arb.agent.CompanionMain \
       --socket arb-companion-<session-hex> [--heartbeat-ms 300] [--no-logcat] [--no-input]
     ```
     (Alternatively `--stdio` uses raw binary framing over `stdin`/`stdout` with diagnostic text strictly on `stderr`.)
   - In `--socket <abstract-name>` mode, the companion binds an Android `LocalServerSocket` on abstract Unix namespace `localabstract:<abstract-name>`, prints one readiness line to stdout (`ARB_COMPANION_READY socket=<abstract-name>`), and accepts a single host TCP connection forwarded via:
     ```sh
     adb -s <serial> forward tcp:0 localabstract:<abstract-name>
     ```
   - All multi-stream messages are multiplexed over this single full-duplex byte stream with strict 20-byte Little-Endian framing.

2. **Video & Audio Capture (`scrcpy-server-v5.0.1` / `replay-scrcpy-audio.dex.jar`, Apache 2.0)**:
   - Video and audio media bytes travel over the dedicated scrcpy ADB-forwarded tunnel (`com.genymobile.scrcpy.Server 5.0.1`, `tunnel_forward=true`, `video_codec=h264`, `video_codec_options=max-bframes:int=0,i-frame-interval:int=1`, `send_frame_meta=true`, `send_stream_meta=true`, `send_device_meta=false`, `control=false`; plus `audio=true audio_source=playback audio_dup=true audio_codec=aac audio_bit_rate=128000` on API >= 33 with the patched `AudioRecordReader`).
   - **scrcpy 5.0.1 Video Tunnel Framing (Big Endian)**:
     1. 1-byte dummy byte (`0x00`) on the first socket (video).
     2. 4-byte codec ID (`0x68323634` = `"h264"`).
     3. Repeating records: read `high: i32 BE`.
        - If `high < 0` (`0x80000000` or `0x80000001`): Session header (`width: i32 BE`, `height: i32 BE`).
        - If `high >= 0`: Frame packet — read `low: u32 BE` (`flags = (high << 32) | low`), `length: i32 BE`, followed by `length` raw H.264 Annex-B bytes (`is_config = bit 62`, `is_keyframe = bit 61`, `pts_us = flags & ((1 << 61) - 1)`, device `CLOCK_MONOTONIC` µs).
   - **scrcpy 5.0.1 Audio Tunnel Framing (Big Endian, second socket when `audio=true`)**:
     1. 4-byte codec ID (`0x00616163` = AAC, `0` = disabled/refused, `1` = error).
     2. Repeating records: `flags: u64 BE`, `length: i32 BE`, followed by `length` bytes (`is_config = bit 62`, `pts_us = flags & ((1 << 61) - 1)`, first-frame `CLOCK_MONOTONIC` µs via patched `AudioRecordReader`).
   - Stream IDs `3` (Video) and `4` (Audio) are reserved in the Beacon health table so the host/coordinator can correlate external media pipeline status alongside Companion streams.

---

## 2. Binary Frame Header (20 Bytes, Little Endian)

Every message in both directions consists of a **fixed 20-byte Little-Endian header** immediately followed by `payload_len` bytes of payload.

| Byte Offset | Size | Type | Field | Description |
| --- | ---: | --- | --- | --- |
| `0..2` | 2 | `[u8; 2]` | `magic` | ASCII `"AR"` (`0x41`, `0x52`) |
| `2..3` | 1 | `u8` | `version` | Protocol version, MUST be `1` |
| `3..4` | 1 | `u8` | `stream_id` | Owning stream ID (see §2.1) |
| `4..5` | 1 | `u8` | `msg_type` | Message type ID (see §2.2) |
| `5..6` | 1 | `u8` | `flags` | Bitmask flags (see §2.3) |
| `6..8` | 2 | `u16` LE | `reserved` | MUST be `0` on send; ignored on receive in v1 |
| `8..12` | 4 | `u32` LE | `seq` | Per-`stream_id` monotonic sequence starting at `1` (wraps mod 2³²; host requests may use `0` or host-monotonic) |
| `12..20` | 8 | `u64` LE | `payload_len` | Payload byte length immediately following header; hard safety cap `4_194_304` (4 MiB) |

### 2.1 `stream_id` Values

| Value | Name | Owner |
| ---: | --- | --- |
| `0` | `Control` / `Beacon` | Companion Stream 0 (`Hello`, `Heartbeat`, `Ping`, `Pong`, `StreamState`, `Shutdown`) |
| `1` | `Logcat` | Companion Stream 1 (`LogRecord`, `LogOverflowStats`, `StreamState`) |
| `2` | `Input` / `Taps` | Companion Stream 2 (`TouchEvent`, `StreamState`) |
| `3` | `Video` | Reserved for scrcpy-server video tunnel health reporting |
| `4` | `Audio` | Reserved for scrcpy-server audio tunnel health reporting |

### 2.2 `msg_type` Values

| Value | Name | Stream | Direction | Purpose |
| ---: | --- | ---: | --- | --- |
| `0x01` | `Hello` | `0` | Agent → Host | Initial session handshake (`boot_id`, device metadata) |
| `0x02` | `Heartbeat` | `0` | Agent → Host | Periodic 3-clock triple + per-stream health table |
| `0x03` | `Ping` | `0` | Host → Agent | Clock-sync / RTT probe request with `nonce` |
| `0x04` | `Pong` | `0` | Agent → Host | Immediate 4-timestamp clock-sync reply echoing `nonce` |
| `0x05` | `StreamState` | `0` / owning | Agent → Host | Stream lifecycle / fault-isolation state transition |
| `0x10` | `LogRecord` | `1` | Agent → Host | Single binary `logcat -B` record tagged with `device_read_boottime_ns` |
| `0x11` | `LogOverflowStats` | `1` | Agent → Host | Periodic `logcat -S` / `-g` drop-stat probe result & completeness |
| `0x20` | `TouchEvent` | `2` | Agent → Host | Single `getevent -lt` input event tagged with `device_read_boottime_ns` |
| `0x30` | `Shutdown` | `0` | Bidirectional | Graceful session termination request / final acknowledgement |

### 2.3 `flags` Bitmask

| Bit | Constant | Meaning |
| ---: | --- | --- |
| `0x01` | `FLAG_URGENT` | Control reply or high-priority notification (e.g., `Pong`, `Shutdown`) |
| `0x02` | `FLAG_TRUNCATED` | Variable-length field was truncated to safety cap |
| `0x04` | `FLAG_RECOVERED` | First data frame emitted after a fault-isolated worker restart |
| `0x08..0x80` | Reserved | MUST be `0` |

### 2.4 Primitive Payload Encoding Conventions

All multi-byte integers are **Little Endian** with **no implicit alignment padding**.

- `str16`: `len: u16` LE followed by `len` UTF-8 bytes (max 65,535 bytes).
- `bytes32`: `len: u32` LE followed by `len` raw bytes.
- All nanosecond timestamps are signed `i64` LE (`0` means not yet emitted / unavailable unless otherwise noted).

---

## 3. Payload Layouts

### 3.1 `0x01 Hello` (Stream 0, Agent → Host)

Sent exactly once immediately after the host connects, before any `Heartbeat` or data frames.

| Order | Type | Field | Description |
| ---: | --- | --- | --- |
| 1 | `u16` | `protocol_version` | Wire protocol version (`1`) |
| 2 | `u32` | `api_level` | `android.os.Build.VERSION.SDK_INT` |
| 3 | `i32` | `boot_count` | `Settings.Global.BOOT_COUNT` (`settings get global boot_count`), or `-1` if unreadable |
| 4 | `u8` | `boot_id_source` | `0 = KernelProcBootId` (`/proc/sys/kernel/random/boot_id`), `1 = FallbackBootCountBtime` (`boot_count` + `/proc/stat btime`), `2 = FallbackRandomUuid` (last-resort per-process UUID) |
| 5 | `u8` | `is_emulator` | `1` if emulator heuristics match (`ro.kernel.qemu`/`ro.boot.qemu`/`Build.*`), else `0` |
| 6 | `u32` | `heartbeat_interval_ms` | Configured heartbeat period in milliseconds (default `300`, clamped to `[100, 5000]`) |
| 7 | `str16` | `boot_id` | Canonical kernel boot UUID, or `fallback:bc=<boot_count>` (clock-independent; older agents sent `fallback:bc=<boot_count>:btime=<sec>`), or `fallback:btime=<sec>` when `boot_count` is unreadable, or `fallback:random:<uuid>`. The host treats the value as opaque. |
| 8 | `str16` | `model` | `android.os.Build.MODEL` |
| 9 | `str16` | `manufacturer` | `android.os.Build.MANUFACTURER` |
| 10 | `str16` | `agent_version` | Companion semantic version string (e.g., `"0.1.0"`) |
| 11 | `str16` | `boot_id_detail` | Empty if `boot_id_source == 0`, otherwise error/reason why `/proc/sys/kernel/random/boot_id` was unavailable |

**Host Reconnect Rule**: Compare `boot_id` (and `boot_id_source`) across reconnects:
- Same `boot_id` with `boot_id_source in {0, 1}` → `CONNECTION_RESTORED`.
- Changed `boot_id` with `boot_id_source in {0, 1}` → `DEVICE_REBOOT`.
- If `boot_id_source == 2` (`FallbackRandomUuid`), reboot vs reconnect cannot be guaranteed from `boot_id` alone; host must treat continuity as unverified.

---

### 3.2 `0x02 Heartbeat` (Stream 0, Agent → Host)

Emitted every `heartbeat_interval_ms` (default 300 ms; spec range 250–500 ms) from a dedicated scheduler thread isolated from Streams 1 and 2.

| Order | Type | Field | Description |
| ---: | --- | --- | --- |
| 1 | `i64` | `boottime_ns` | `SystemClock.elapsedRealtimeNanos()` (`CLOCK_BOOTTIME`, advances during suspend) |
| 2 | `i64` | `monotonic_ns` | `System.nanoTime()` (`CLOCK_MONOTONIC`, pauses during suspend) |
| 3 | `i64` | `realtime_ns` | `System.currentTimeMillis() * 1_000_000L` (`CLOCK_REALTIME` wall ms × 10⁶) |
| 4 | `u8` | `stream_count` | Number of `StreamHealthEntry` records following (`N`) |
| 5 | `[Entry; N]` | `streams` | `N` consecutive fixed 20-byte entries (see below) |

**`StreamHealthEntry` (fixed 20 bytes each)**:

| Offset | Size | Type | Field | Description |
| ---: | ---: | --- | --- | --- |
| `0` | 1 | `u8` | `stream_id` | `0=Beacon`, `1=Logcat`, `2=Input`, (`3=Video`, `4=Audio` if reported) |
| `1` | 1 | `u8` | `state` | `0=Starting`, `1=Running`, `2=Stopped`, `3=Error`, `4=Restarting` |
| `2` | 2 | `u16` | `restart_count` | Cumulative fault-isolated restarts since companion launch |
| `4` | 4 | `u32` | `emitted_seq` | Most recent frame `seq` emitted on this stream (`0` if none yet) |
| `8` | 8 | `i64` | `last_pts_ns` | Most recent `boottime_ns` emitted on this stream (`0` if none yet) |
| `16` | 4 | `u32` | `reserved` | `0` |

**Invariant**: The companion samples `monotonic_ns` immediately before `boottime_ns` in `Heartbeat`, so `boottime_ns >= monotonic_ns` holds strictly even when cumulative suspend is zero; in `Pong`, `monotonic_ns` is bracketed by `[boottime_before_ns, boottime_after_ns]` so `boottime_after_ns >= monotonic_ns` and `boottime_after_ns >= boottime_before_ns`. Across successive heartbeats within one boot, `boottime_ns` and `monotonic_ns` are strictly non-decreasing.

---

### 3.3 `0x03 Ping` (Stream 0, Host → Agent)

Fixed 8-byte payload.

| Order | Type | Field | Description |
| ---: | --- | --- | --- |
| 1 | `u64` | `nonce` | Arbitrary 64-bit host correlation nonce |

---

### 3.4 `0x04 Pong` (Stream 0, Agent → Host, `FLAG_URGENT`)

Fixed 40-byte payload. Emitted immediately upon receiving `Ping`.

| Order | Type | Field | Description |
| ---: | --- | --- | --- |
| 1 | `u64` | `nonce` | Echoed from `Ping` |
| 2 | `i64` | `boottime_before_ns` | `SystemClock.elapsedRealtimeNanos()` captured immediately before reading mono/wall |
| 3 | `i64` | `monotonic_ns` | `System.nanoTime()` |
| 4 | `i64` | `realtime_ns` | `System.currentTimeMillis() * 1_000_000L` |
| 5 | `i64` | `boottime_after_ns` | `SystemClock.elapsedRealtimeNanos()` captured immediately after reading mono/wall |

**Host Sync Uncertainty Formula**:
- Host records `host_send_ns` before sending `Ping` and `host_recv_ns` upon receiving `Pong`.
- `rtt_ns = host_recv_ns - host_send_ns`.
- Device read bracket `device_bracket_ns = boottime_after_ns - boottime_before_ns`.
- Half-width upper bound: `uncertainty_ns = rtt_ns / 2 + device_bracket_ns / 2`.

---

### 3.5 `0x05 StreamState` (Stream 0 or Owning Stream, Agent → Host)

Emitted immediately whenever a worker enters `Starting`, `Running`, `Stopped`, `Error`, or `Restarting`.

| Order | Type | Field | Description |
| ---: | --- | --- | --- |
| 1 | `u8` | `target_stream_id` | Stream whose state changed (`0`, `1`, `2`, ...) |
| 2 | `u8` | `state` | `0=Starting`, `1=Running`, `2=Stopped`, `3=Error`, `4=Restarting` |
| 3 | `u16` | `restart_count` | Cumulative fault-isolated restart count for `target_stream_id` |
| 4 | `i64` | `boottime_ns` | Device `CLOCK_BOOTTIME` when the transition occurred |
| 5 | `u32` | `emitted_seq` | Last emitted `seq` on `target_stream_id` |
| 6 | `str16` | `reason` | Machine-readable transition reason (e.g. `"started"`, `"no_touch_device"`, `"process_exit:137"`, `"backoff_ms:500"`, `"restarted"`) |

---

### 3.6 `0x10 LogRecord` (Stream 1, Agent → Host)

Emitted per binary `logger_entry` read from `logcat -b all -B -T 1`. After a `logcat` respawn the companion resumes with `-T '<sec>.<nsec>'` of the last emitted entry and drops the already-sent boundary entry (matched by timestamp + pid/tid/lid/priority/payload); `-T 1` is used only when nothing has been emitted yet. A `LogOverflowStats(OverflowDetected)` gap is emitted only for real loss (no resume timestamp, or the boundary entry was already evicted from the ring).

| Order | Type | Field | Description |
| ---: | --- | --- | --- |
| 1 | `i64` | `device_read_boottime_ns` | `SystemClock.elapsedRealtimeNanos()` stamped immediately when the record is read on device |
| 2 | `u32` | `log_sec` | `logger_entry.sec` (logd wall seconds) |
| 3 | `u32` | `log_nsec` | `logger_entry.nsec` (logd wall nanoseconds) |
| 4 | `i32` | `pid` | `logger_entry.pid` |
| 5 | `i32` | `tid` | `logger_entry.tid` |
| 6 | `u32` | `lid` | `logger_entry.lid` (`0=main`, `1=radio`, `2=events`, `3=system`, `4=crash`, `5=stats`, `6=security`, `7=kernel`, `0xFFFFFFFF=unknown`) |
| 7 | `u32` | `uid` | `logger_entry_v4.uid` if `hdr_size >= 28`, else `0xFFFFFFFF` |
| 8 | `u16` | `hdr_size` | Original binary `logger_entry` header size (`20`, `24`, `28`, or larger) |
| 9 | `u8` | `priority` | For text buffers: `payload[0]` (`2=V..7=F`); for binary event buffers (`lid in {2,5,6}`): `0` |
| 10 | `u8` | `record_flags` | Bit `0x01`: binary event payload (`lid in {2,5,6}`); other bits `0` |
| 11 | `bytes32` | `payload` | Raw `logger_entry` payload bytes (`[priority:1][tag\0][message\0]` for text buffers; raw event bytes for binary event buffers) |

---

### 3.7 `0x11 LogOverflowStats` (Stream 1, Agent → Host)

Emitted on Stream 1 startup, every 5 seconds, and immediately upon unexpected `logcat` EOF/process exit.

| Order | Type | Field | Description |
| ---: | --- | --- | --- |
| 1 | `i64` | `boottime_ns` | Device `CLOCK_BOOTTIME` when the stats probe completed |
| 2 | `u8` | `completeness` | `0 = Verified`, `1 = OverflowDetected`, `2 = Unverified` |
| 3 | `u8` | `reserved` | `0` |
| 4 | `u16` | `buffers_count` | Number of logd buffers successfully parsed (`0` when `Unverified`) |
| 5 | `i64` | `pruned_delta` | Non-negative counter delta observed since previous probe, or `-1` if `Unverified` |
| 6 | `i64` | `pruned_cumulative` | Cumulative pruned/dropped count if reported by `logcat -S`, or `-1` if `Unverified` |
| 7 | `str16` | `detail` | Concise diagnostic summary (e.g., `"logcat-S:total=1234,now=1234,pruned=0"` or reason for `Unverified`/`OverflowDetected`) |

The host counts a verified interval as zero lost records. For an overflow, `explicit_dropped_delta=N` provides a record count. The writer queue counts lost frames, including statistics frames. Ring pruning, mixed-frame loss, an unverified interval, or a host-side log gap makes the total number of missing log records unknown.

**Honesty Rule (`REQ-03`)**:
- `Verified (0)` is emitted ONLY when explicit drop/pruned counters are parsed cleanly from `logcat -S` AND no drop/EOF increase occurred AND the reader experienced zero unrecovered gaps since the previous check.
- `OverflowDetected (1)` is emitted immediately when `logcat` exits unexpectedly (EOF/SIGKILL), when explicit dropped-log counters increase, or when ring-buffer eviction outpaces consumption (e.g., logd `dropped` / `chatty` / `pruned` overflow signal).
- `Unverified (2)` is emitted whenever `logcat -S` / `-g` cannot be executed, returns non-zero, lacks explicit reader-drop counters on that Android build/SELinux policy, or cannot be unambiguously parsed. **Never report `Verified` when completeness cannot be proven.**

---

### 3.8 `0x20 TouchEvent` (Stream 2, Agent → Host)

Emitted per line read from `getevent -lt <touch-device>` after touch device discovery via `getevent -lp`.

| Order | Type | Field | Description |
| ---: | --- | --- | --- |
| 1 | `i64` | `device_read_boottime_ns` | `SystemClock.elapsedRealtimeNanos()` stamped immediately when the line is read on device |
| 2 | `i64` | `event_sec` | Kernel `input_event.time.tv_sec` (`CLOCK_MONOTONIC`) parsed from `[  sec.usec]`, or `-1` if absent |
| 3 | `i32` | `event_usec` | Kernel `input_event.time.tv_usec` (`0..999999`), or `-1` if absent |
| 4 | `u16` | `ev_type` | Linux input event type (`0x0000=EV_SYN`, `0x0001=EV_KEY`, `0x0003=EV_ABS`, `0xFFFF=unknown`) |
| 5 | `u16` | `ev_code` | Linux input event code (e.g. `0x0000=SYN_REPORT`, `0x002f=ABS_MT_SLOT`, `0x0035=ABS_MT_POSITION_X`, `0x0036=ABS_MT_POSITION_Y`, `0x0039=ABS_MT_TRACKING_ID`, `0x014a=BTN_TOUCH`, `0xFFFF=unknown`) |
| 6 | `i32` | `ev_value` | Signed 32-bit event value (`0` if unparseable) |
| 7 | `str16` | `device_path` | Input device path (e.g., `"/dev/input/event2"`) |
| 8 | `str16` | `raw_line` | Trimmed `getevent -lt` line (truncated to 256 UTF-8 bytes with `FLAG_TRUNCATED` if longer) |

If no multi-touch device (`ABS_MT_POSITION_X` + `ABS_MT_POSITION_Y`) is present in `getevent -lp`, Stream 2 cleanly emits `StreamState(stream=2, state=Stopped, reason="no_touch_device")` and remains quiescent without affecting Stream 0 or Stream 1.

---

### 3.9 `0x30 Shutdown` (Stream 0, Bidirectional, `FLAG_URGENT`)

| Order | Type | Field | Description |
| ---: | --- | --- | --- |
| 1 | `i64` | `boottime_ns` | Sender `CLOCK_BOOTTIME` (`0` when sent by host) |
| 2 | `u16` | `code` | `0 = Normal`, `1 = HostDisconnect`, `2 = FatalError` |
| 3 | `str16` | `reason` | Short UTF-8 reason string |

When the host sends `Shutdown`, the companion stops all child workers (`logcat`, `getevent`), emits a final `Shutdown` acknowledgement frame on Stream 0, flushes the socket, and exits with status `0`.

---

## 4. Fault Isolation Contract

- **Stream 0 (Control/Beacon)** runs its own scheduled heartbeat loop and command reader loop. It MUST NEVER block on Stream 1 (`logcat`) or Stream 2 (`getevent`) process I/O.
- **Stream 1 (Logcat)** and **Stream 2 (Input)** each run in dedicated daemon threads with full `try / catch (Throwable)` boundaries.
- Outbound frames are enqueued into per-stream bounded queues and written by one dedicated writer thread as atomic complete frames (20-byte header + payload in one buffer), so interleaved frames from Streams 0, 1, and 2 can never tear or corrupt framing. The writer always drains Stream 0 first, so `Heartbeat`/`Pong`/`Shutdown` never wait behind Stream 1/2 bursts.
- When the Stream 1 queue is full, non-urgent Stream 1 frames are dropped instead of blocking, and a `LogOverflowStats(completeness=OverflowDetected, detail="writer_queue_overflow:dropped=N")` frame is queued on Stream 1 as soon as there is room. Urgent Stream 1 frames and Stream 2 frames apply back-pressure only to their own producer thread.
- A transport write failure (e.g. broken pipe in `--stdio` mode, which writes to the raw stdout file descriptor rather than `System.out`) makes every later write throw, so the heartbeat loop shuts the session down immediately.
- String fields (`str16`) are truncated only on UTF-8 code point boundaries; a frame whose string field was truncated carries `FLAG_TRUNCATED`.
- If a child process (`logcat` or `getevent`) crashes, is killed (`SIGKILL`), or throws an I/O error:
  1. The owning worker immediately transitions to `Error` (emitting `StreamState`),
  2. Transitions to `Restarting` with bounded exponential backoff (`500 ms → 1000 ms → 2000 ms`, capped at `5000 ms`),
  3. Respawns the child process, increments `restart_count`, transitions back to `Running`, and tags the first post-recovery frame with `FLAG_RECOVERED`.
  4. Stream 0 Heartbeats and `Ping`/`Pong` continue uninterrupted throughout.
