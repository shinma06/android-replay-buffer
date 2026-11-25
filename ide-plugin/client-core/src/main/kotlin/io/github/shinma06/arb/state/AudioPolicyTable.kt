package io.github.shinma06.arb.state

import io.github.shinma06.arb.model.AudioCaptureState
import io.github.shinma06.arb.model.AudioCause
import io.github.shinma06.arb.model.CaptureStatusView

/** Presentation class of an [AudioCause]; mirrors `arb_core::audio_policy::AudioLabelClass`. */
enum class AudioLabelClass(val wire: String) {
    /** No audio banner. */
    NONE("none"),

    /** Audio intentionally off (no warning banner). */
    DISABLED("disabled"),

    /** "Unsupported" — the device OS is below the audio floor (Android 13 / API 33). */
    UNSUPPORTED_API("unsupported_api"),

    /** A concrete runtime failure on a capable device. */
    RUNTIME_FAILURE("runtime_failure"),
}

/**
 * PDD G8: the single place that turns the daemon's audio state and cause into user-facing text
 * (GUI parity with `system.status` for CLI/MCP). The word "非対応" (unsupported) appears only for
 * [AudioCause.API_BELOW_MIN]; every runtime failure on Android 13+ names its concrete cause.
 * The cause → class mapping is checked against `docs/specs/audio-policy-table.json`, the golden
 * table shared with the Rust `arb-core` tests.
 */
object AudioPolicyTable {
    private const val CONTINUING = "映像＋ログで継続中"

    /** Presentation class of [cause] (exhaustive; no `else`). */
    fun classOf(cause: AudioCause): AudioLabelClass = when (cause) {
        AudioCause.NONE -> AudioLabelClass.NONE
        AudioCause.USER_DISABLED -> AudioLabelClass.DISABLED
        AudioCause.LOGS_ONLY -> AudioLabelClass.DISABLED
        AudioCause.API_BELOW_MIN -> AudioLabelClass.UNSUPPORTED_API
        AudioCause.DEVICE_CAPTURE_FAILED -> AudioLabelClass.RUNTIME_FAILURE
        AudioCause.ENCODER_FATAL -> AudioLabelClass.RUNTIME_FAILURE
        AudioCause.HANDSHAKE_IO -> AudioLabelClass.RUNTIME_FAILURE
        AudioCause.TUNNEL_CONNECT_FAILED -> AudioLabelClass.RUNTIME_FAILURE
        AudioCause.LAUNCH_FAILED_AUDIO_TRUE -> AudioLabelClass.RUNTIME_FAILURE
        AudioCause.LAUNCH_FAILED_BOTH -> AudioLabelClass.RUNTIME_FAILURE
        AudioCause.RUNTIME_STICKY_DISABLED -> AudioLabelClass.RUNTIME_FAILURE
        AudioCause.UNKNOWN -> AudioLabelClass.RUNTIME_FAILURE
    }

    /**
     * Cause actually presented for [state]/[cause]: an unavailable state without a cause comes
     * from a daemon older than PDD G8 and is an unknown runtime failure, never "unsupported".
     */
    fun effectiveCause(state: AudioCaptureState, cause: AudioCause?): AudioCause? = when {
        cause != null -> cause
        state == AudioCaptureState.UNAVAILABLE -> AudioCause.UNKNOWN
        else -> null
    }

    /** Presentation class for a reported [state]/[cause] pair. */
    fun labelClass(state: AudioCaptureState, cause: AudioCause?): AudioLabelClass =
        effectiveCause(state, cause)?.let(::classOf) ?: AudioLabelClass.NONE

    /** Banner title for a cause that warrants one, or `null` for no banner. */
    fun bannerTitle(cause: AudioCause, apiLevel: Long?): String? = when (cause) {
        AudioCause.NONE -> null
        AudioCause.USER_DISABLED -> null
        AudioCause.LOGS_ONLY -> null
        AudioCause.API_BELOW_MIN ->
            "音声キャプチャ非対応（Android 13 / API 33 未満: API ${apiLevel?.toString() ?: "不明"}）— $CONTINUING"
        AudioCause.DEVICE_CAPTURE_FAILED ->
            "端末側で再生音キャプチャを開始できませんでした（他アプリ／ミラーリング競合や端末制限の可能性）— $CONTINUING"
        AudioCause.ENCODER_FATAL ->
            "端末の音声エンコーダ初期化に失敗したため音声を無効化しました — $CONTINUING"
        AudioCause.HANDSHAKE_IO ->
            "音声ストリームが接続直後に切断されました — $CONTINUING"
        AudioCause.TUNNEL_CONNECT_FAILED ->
            "音声ストリームへ接続できませんでした — $CONTINUING"
        AudioCause.LAUNCH_FAILED_AUDIO_TRUE ->
            "音声付きで録画を開始できなかったため音声なしで再起動しました — $CONTINUING"
        AudioCause.LAUNCH_FAILED_BOTH ->
            "録画サーバーを起動できませんでした（音声あり・なしとも失敗）— ログ＋タップのみ継続中"
        AudioCause.RUNTIME_STICKY_DISABLED ->
            "この端末起動中は音声を無効化しています（致命的な音声失敗のため。録画の停止→開始で再試行）— $CONTINUING"
        AudioCause.UNKNOWN ->
            "音声キャプチャに失敗しました（原因不明）— $CONTINUING"
    }

    /** Audio banner for [capture], or `null` when audio is healthy, pending or intentionally off. */
    fun banner(capture: CaptureStatusView): CaptureBanner? {
        if (!capture.active || !capture.connected || capture.effectiveAudioState != AudioCaptureState.UNAVAILABLE) return null
        val cause = effectiveCause(capture.effectiveAudioState, capture.audioCause) ?: return null
        val title = bannerTitle(cause, capture.deviceApiLevel) ?: return null
        return CaptureBanner(
            kind = CaptureBannerKind.CONFIG_WARNING,
            title = title,
            detail = capture.audioReason?.takeIf { it.isNotBlank() } ?: "詳細な理由は報告されていません。",
        )
    }

    /**
     * Compact Tool Window audio label: the configured setting plus the live state, so a failing
     * or unsupported device never reads as a plain "音声 ON".
     */
    fun statusLabel(audioEnabled: Boolean, logsOnly: Boolean, capture: CaptureStatusView?): String {
        if (logsOnly) return "映像・音声なし"
        if (capture == null || !capture.active) return if (audioEnabled) "音声 ON" else "音声 OFF"
        if (!capture.connected) return "音声: 接続待ち"
        val state = capture.effectiveAudioState
        val cause = effectiveCause(state, capture.audioCause)
        val live = when (state) {
            AudioCaptureState.AVAILABLE -> "音声 ON"
            AudioCaptureState.UNKNOWN -> if (audioEnabled) "音声 ON（確認中）" else "音声 OFF（確認中）"
            AudioCaptureState.DISABLED -> "音声 OFF"
            AudioCaptureState.UNAVAILABLE -> when {
                cause == null -> "音声: 失敗（映像のみ）"
                classOf(cause) == AudioLabelClass.UNSUPPORTED_API -> "音声: 非対応"
                cause == AudioCause.LAUNCH_FAILED_BOTH -> "音声: 失敗（ログのみ）"
                classOf(cause) == AudioLabelClass.RUNTIME_FAILURE -> "音声: 失敗（映像のみ）"
                else -> "音声 OFF"
            }
        }
        return when {
            state == AudioCaptureState.DISABLED && audioEnabled -> "$live（設定 ON）"
            state != AudioCaptureState.DISABLED && state != AudioCaptureState.UNKNOWN && !audioEnabled -> "$live（設定 OFF）"
            else -> live
        }
    }
}
