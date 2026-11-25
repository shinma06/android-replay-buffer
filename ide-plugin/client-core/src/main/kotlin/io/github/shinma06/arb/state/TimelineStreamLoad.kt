package io.github.shinma06.arb.state

/**
 * Auxiliary Studio streams that load independently of the video viewport. A failed load is a
 * state of its own ([StreamLoadState.Failed]); it must never be shown as "0 events".
 */
enum class AuxStream(val label: String) {
    FILMSTRIP("フィルムストリップ"),
    DISCONTINUITIES("欠損・不連続"),
    CRASHES("クラッシュ"),
    SCREENS("画面遷移"),
}

sealed interface StreamLoadState {
    /** Not requested yet (initial state while the loader is running). */
    data object Pending : StreamLoadState

    /** Loaded; an empty list here really means "none recorded". */
    data object Loaded : StreamLoadState

    /** The request failed; the data is unknown, not empty. */
    data class Failed(val reason: String) : StreamLoadState
}

/** Per-bucket log row display limit of the Studio (`timeline.query_logs` pages up to this many rows). */
const val LOG_DISPLAY_LIMIT: Int = 5_000

/** Normalises an exception into the short reason shown next to a failed stream. */
fun streamFailureReason(error: Throwable): String =
    error.message?.takeIf { it.isNotBlank() } ?: error::class.java.simpleName

/** One stream whose last load failed, as listed by the inline "取得できませんでした" banner. */
data class AuxStreamFailure(val stream: AuxStream, val reason: String)

/** Wording shared by the Studio banners (kept in client-core so tests assert the exact text). */
object StudioLoadMessages {
    /** Shown when a log bucket hit the per-bucket display limit and rows were dropped. */
    fun logLimitNotice(limit: Int): String =
        "ログは表示上限（${formatLimit(limit)} 行）に達したため一部省略しています。検索対象は各区分の先頭から表示上限までです。"

    fun streamFailureNotice(failure: AuxStreamFailure): String =
        "${failure.stream.label}を取得できませんでした（${failure.reason}）。0 件ではなく未取得です。"

    fun editSaveFailureNotice(reason: String): String =
        "編集範囲（IN/OUT）を保存できていません（$reason）。再保存するまで、再度開くと以前の範囲に戻ります。"

    private fun formatLimit(limit: Int): String = String.format(java.util.Locale.ROOT, "%,d", limit)
}
