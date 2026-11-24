package io.github.shinma06.arb.agent;

/**
 * Exact binary wire protocol constants matching {@code device-agent/PROTOCOL.md} (version 1).
 */
public final class Protocol {
    private Protocol() {}

    public static final byte MAGIC_0 = 0x41; // 'A'
    public static final byte MAGIC_1 = 0x52; // 'R'
    public static final int VERSION = 1;
    public static final int HEADER_SIZE = 20;
    public static final long MAX_PAYLOAD_LEN = 4L * 1024L * 1024L; // 4 MiB safety cap

    public static final String AGENT_VERSION = "0.1.0";

    // stream_id values
    public static final int STREAM_CONTROL = 0;
    public static final int STREAM_LOGCAT = 1;
    public static final int STREAM_INPUT = 2;
    public static final int STREAM_VIDEO = 3;
    public static final int STREAM_AUDIO = 4;

    // msg_type values
    public static final int MSG_HELLO = 0x01;
    public static final int MSG_HEARTBEAT = 0x02;
    public static final int MSG_PING = 0x03;
    public static final int MSG_PONG = 0x04;
    public static final int MSG_STREAM_STATE = 0x05;
    public static final int MSG_LOG_RECORD = 0x10;
    public static final int MSG_LOG_OVERFLOW_STATS = 0x11;
    public static final int MSG_TOUCH_EVENT = 0x20;
    public static final int MSG_SHUTDOWN = 0x30;

    // flags bitmask
    public static final int FLAG_NONE = 0x00;
    public static final int FLAG_URGENT = 0x01;
    public static final int FLAG_TRUNCATED = 0x02;
    public static final int FLAG_RECOVERED = 0x04;

    // stream state enum
    public static final int STATE_STARTING = 0;
    public static final int STATE_RUNNING = 1;
    public static final int STATE_STOPPED = 2;
    public static final int STATE_ERROR = 3;
    public static final int STATE_RESTARTING = 4;

    // boot_id_source enum
    public static final int BOOT_ID_KERNEL_PROC = 0;
    public static final int BOOT_ID_FALLBACK_BOOT_COUNT_BTIME = 1;
    public static final int BOOT_ID_FALLBACK_RANDOM_UUID = 2;

    // log overflow completeness enum
    public static final int COMPLETENESS_VERIFIED = 0;
    public static final int COMPLETENESS_OVERFLOW_DETECTED = 1;
    public static final int COMPLETENESS_UNVERIFIED = 2;

    // shutdown codes
    public static final int SHUTDOWN_NORMAL = 0;
    public static final int SHUTDOWN_HOST_DISCONNECT = 1;
    public static final int SHUTDOWN_FATAL_ERROR = 2;

    // logcat record flags
    public static final int LOG_RECORD_FLAG_BINARY_EVENT = 0x01;

    // unknown sentinel values
    public static final int UNKNOWN_U32 = 0xFFFFFFFF;
    public static final int UNKNOWN_U16 = 0xFFFF;
}
