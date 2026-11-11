package io.github.shinma06.replaybuffer.ui

import io.github.shinma06.replaybuffer.core.ApplicationMode
import io.github.shinma06.replaybuffer.core.CaptureState
import io.github.shinma06.replaybuffer.core.DeviceKind
import io.github.shinma06.replaybuffer.core.MULTIPLE_DEVICES_REASON
import io.github.shinma06.replaybuffer.core.ReplaySnapshot
import io.github.shinma06.replaybuffer.core.SavePhase
import io.github.shinma06.replaybuffer.core.SaveSnapshot
import io.github.shinma06.replaybuffer.ide.streamName
import io.github.shinma06.replaybuffer.settings.ReplaySettings
import java.nio.file.Path

/** Severity shown with a symbol, a colour and a word together (docs/design/ui-ux.md). */
internal enum class Tone { OK, PENDING, WARNING, ERROR, OFF }

/** Result of the last screenshot requested from this panel; presentation-only. */
internal sealed interface ScreenshotResult {
    data class Saved(val time: String, val file: Path) : ScreenshotResult
    data class Failed(val time: String, val reason: String) : ScreenshotResult
}

/** Everything the ToolWindow shows, read from services on the EDT. */
internal data class ToolWindowInput(
    val snapshot: ReplaySnapshot?,
    val message: String?,
    val initializationError: String?,
    val busy: Boolean,
    val enabled: Boolean,
    val settings: ReplaySettings,
    val adb: Path?,
    val adbReason: String?,
    val screenshotPackage: String?,
    val screenshotDisabledReason: String?,
    val screenshotBusy: Boolean,
    val logcat: LogcatState = LogcatState("Logcatを開く", true, "", false),
    val savedAt: String? = null,
    val screenshot: ScreenshotResult? = null,
)

internal data class LogcatState(val label: String, val enabled: Boolean, val tooltip: String, val failed: Boolean)

internal data class Header(val tone: Tone, val symbol: String, val text: String, val tooltip: String)

/** A command; reason is shown under it only while it cannot run. */
internal data class Command(val label: String, val enabled: Boolean, val tooltip: String, val reason: String? = null)

internal data class Toggle(val selected: Boolean, val enabled: Boolean, val tooltip: String)

internal data class SaveFailure(
    val reason: String,
    val fixedRequest: String,
    val retainedOutput: String?,
    val actionsEnabled: Boolean,
)

internal data class RecentResult(
    val tone: Tone,
    val text: String,
    val secondary: List<String> = emptyList(),
    val openDirectory: Path? = null,
    val failure: SaveFailure? = null,
)

internal enum class NoticeAction { OPEN_SETTINGS }

internal data class Notice(val tone: Tone, val text: String, val action: NoticeAction? = null)

internal data class Detail(val name: String, val text: String)

internal data class ToolWindowModel(
    val header: Header,
    val toggle: Toggle,
    val save: Command,
    val screenshot: Command,
    val logcat: Command,
    val results: List<RecentResult>,
    val notices: List<Notice>,
    val details: List<Detail>,
    /** Capture-target chooser entries; empty (hidden) while fewer than two devices are in play. */
    val devices: List<DeviceChoice> = emptyList(),
) {
    val showsResults: Boolean get() = results.isNotEmpty()
    val showsFailure: Boolean get() = results.any { it.failure != null }
    val showsNotices: Boolean get() = notices.isNotEmpty()

    /** Details start collapsed; they hold every rarely needed fact so nothing is lost. */
    val detailsInitiallyExpanded: Boolean get() = false
}

internal const val TOGGLE_TOOLTIP = "ON/OFFはすぐ反映します。OFFにすると未保存の記録と再試行待ちの対象を破棄します。"
internal const val SETTINGS_TOOLTIP = "取得OFFでも編集できます。変更は設定画面の「適用」または「OK」で反映します。"
internal const val SCREENSHOT_TOOLTIP = "録画対象の端末の今の画面全体を保存します。取得OFFでは接続済みの1台から保存します。\n" +
    "撮影中は画面を切り替えないでください。通知・キーボードも写り、保護領域は空白になる場合があります。"
internal const val RETRY_TOOLTIP = "再試行はこの固定した区間を使います。破棄しても取得は続きます。"
internal const val DISCARD_WARNING = "取得OFFまたはproject終了で、この未保存の対象を破棄します。"
internal const val BUSY_MESSAGE = "設定・操作を反映中…（新規保存は完了まで待ってください）"

internal fun headerTone(input: ToolWindowInput): Tone {
    val snapshot = input.snapshot ?: return if (input.initializationError != null) Tone.ERROR else Tone.PENDING
    return when (snapshot.captureState) {
        CaptureState.DISABLED -> Tone.OFF
        CaptureState.WAITING -> Tone.PENDING
        CaptureState.CAPTURING -> if (snapshot.error != null) Tone.WARNING else Tone.OK
        CaptureState.RECOVERING, CaptureState.PARTIAL -> Tone.WARNING
        CaptureState.MULTIPLE_DEVICES -> Tone.ERROR
    }
}

internal fun toneSymbol(tone: Tone): String = when (tone) {
    Tone.OK -> "●"
    Tone.PENDING -> "◌"
    Tone.WARNING -> "▲"
    Tone.ERROR -> "✕"
    Tone.OFF -> "○"
}

internal fun toolWindowModel(input: ToolWindowInput): ToolWindowModel {
    val snapshot = input.snapshot
    val settings = input.settings
    val seconds = snapshot?.settings?.replaySeconds ?: settings.retentionSeconds
    val tone = headerTone(input)
    val state = snapshot?.captureDescription() ?: if (input.initializationError != null) "取得を初期化できません" else "取得を初期化中…"
    val header = Header(tone, toneSymbol(tone), listOfNotNull(
        state,
        snapshot?.device?.name?.takeIf { snapshot.enabled },
        snapshot?.settings?.application?.packageName?.takeIf { snapshot.enabled },
    ).joinToString(" · "), state)

    val saveReason = if (input.busy) "設定・操作を反映中です。" else snapshot?.saveDisabledReason
    val saveEnabled = !input.busy && snapshot?.canSave == true
    val save = Command("直前${seconds}秒を保存", saveEnabled, saveReason.orEmpty(),
        saveReason?.takeIf { !saveEnabled && it.isNotBlank() })
    // The notice already asks to choose a device; do not repeat it under the screenshot button.
    val repeatsChoice = snapshot?.captureState == CaptureState.MULTIPLE_DEVICES &&
        input.screenshotDisabledReason?.startsWith(MULTIPLE_DEVICES_REASON) == true
    val screenshot = Command(if (input.screenshotBusy) "スクショを保存中…" else "スクリーンショット",
        input.screenshotDisabledReason == null, input.screenshotDisabledReason ?: SCREENSHOT_TOOLTIP,
        input.screenshotDisabledReason?.takeIf { !input.screenshotBusy && !repeatsChoice })
    val logcat = Command(input.logcat.label, input.logcat.enabled, input.logcat.tooltip,
        input.logcat.tooltip.takeIf { (!input.logcat.enabled || input.logcat.failed) && it.isNotBlank() })

    val saved = snapshot?.save
    val results = buildList {
        saveResult(saved, input.busy, seconds, input.savedAt)?.let(::add)
        when (val shot = input.screenshot) {
            is ScreenshotResult.Saved -> add(RecentResult(Tone.OK, "スクリーンショットを保存しました ${shot.time}",
                listOf("保存したファイル: ${shot.file}"), shot.file.parent))
            is ScreenshotResult.Failed -> add(RecentResult(Tone.ERROR, "スクリーンショットを保存できませんでした ${shot.time}",
                listOf(shot.reason)))
            null -> {}
        }
    }

    val notices = buildList {
        val message = if (input.busy) BUSY_MESSAGE else input.message ?: input.initializationError ?: snapshot?.error
        if (!message.isNullOrBlank()) add(Notice(if (input.busy) Tone.PENDING else Tone.WARNING, message))
        val app = snapshot?.settings?.application
        if (snapshot?.enabled == true && app?.packageName == null && !app?.unresolvedReason.isNullOrBlank())
            add(Notice(Tone.WARNING, "アプリ: ${app.unresolvedReason}", NoticeAction.OPEN_SETTINGS))
        pendingSettings(input)?.let { add(Notice(Tone.PENDING, it)) }
        cleanupDescription(snapshot?.cleanupPendingCount ?: 0).takeIf { it.isNotEmpty() }?.let { add(Notice(Tone.WARNING, it)) }
    }

    val details = buildList {
        add(Detail("取得状態と取得モード", "取得状態: $state / 取得モード: ${captureModeDescription(snapshot?.settings?.captureMode ?: settings.captureMode)}"))
        add(Detail("取得対象の端末", snapshot?.device?.let {
            "端末: ${it.name}（${if (it.kind == DeviceKind.EMULATOR) "Emulator" else "実機"}・${if (it.connected) "接続済み" else "切断中"}）"
        } ?: if (snapshot?.captureState == CaptureState.MULTIPLE_DEVICES) "端末: 未選択（${snapshot.devices.size}台接続中）" else "端末: 未接続"))
        val app = snapshot?.settings?.application
        add(Detail("取得対象のアプリ", "アプリ: ${app?.packageName ?: app?.unresolvedReason ?: "確認中…"}（${if (app?.mode == ApplicationMode.MANUAL) "手動" else "自動"}）"))
        var video = streamDescription("動画", snapshot?.video)
        if ((snapshot?.settings?.captureMode ?: settings.captureMode).videoRequested)
            video += " / 指定品質: ${snapshot?.settings?.videoQuality ?: settings.videoQuality}"
        add(Detail("動画の取得状態", video))
        add(Detail("端末ログの取得状態", streamDescription("端末ログ", snapshot?.deviceLog)))
        add(Detail("アプリログの取得状態", streamDescription("アプリログ", snapshot?.appLog)))
        add(Detail("保持時間と現在の記録区間", "保持時間: ${seconds}秒 / 記録区間: ${recordTime(snapshot?.windowStartNs)}〜${recordTime(snapshot?.windowEndNs)}" +
            if (snapshot?.frozen == true) "（切断前で固定・無効化まで保持）" else "（現在の窓）"))
        currentGapDescription(snapshot).takeIf { it.isNotEmpty() }?.let { add(Detail("現在の保存窓の不足・未確認区間", it)) }
        add(Detail("動画・ログの保存先", "保存先: ${snapshot?.settings?.saveDirectory ?: "未指定（設定で指定してください）"}"))
        add(Detail("スクリーンショットの対象と保存先", "スクショ対象: ${input.screenshotPackage} / 保存先: " +
            if (settings.screenshotUsesLogcatDestination) "Logcatのスクショ保存先（保存時の最新設定）" else settings.screenshotDirectory))
        add(Detail("直前保存の結果", saveDescription(saved)))
        when (saved?.phase) {
            SavePhase.COMPLETED -> saved.directory?.let { add(Detail("完成した出力の場所", "保存したフォルダ: $it")) }
            else -> {}
        }
        videoTailDescription(saved).takeIf { it.isNotEmpty() }?.let { add(Detail("保存対象の動画末尾の確認状態", it)) }
        if (saved?.phase == SavePhase.FAILED) add(Detail("固定した保存対象の端末とアプリ履歴", frozenTargetDescription(saved)))
        add(Detail("クラッシュ自動保存の適用状態と直近の結果", autoSaveDescription(snapshot)))
        add(Detail("Android SDKの接続情報", "Android SDK: ${input.adb ?: input.adbReason ?: "確認中…"}"))
        add(Detail("記録セッション", "記録セッション: ${snapshot?.sequenceId ?: "未開始"}"))
    }

    return ToolWindowModel(
        header = header,
        toggle = Toggle(input.enabled, input.enabled || !input.busy, TOGGLE_TOOLTIP),
        save = save,
        screenshot = screenshot,
        logcat = logcat,
        results = results,
        notices = notices,
        details = details,
        devices = deviceChoices(snapshot),
    )
}

private fun saveResult(saved: SaveSnapshot?, busy: Boolean, seconds: Int, savedAt: String?): RecentResult? = when (saved?.phase) {
    null, SavePhase.IDLE -> null
    SavePhase.WRITING -> RecentResult(Tone.PENDING, saveDescription(saved))
    SavePhase.CANCELLED -> RecentResult(Tone.WARNING, saveDescription(saved))
    SavePhase.COMPLETED -> RecentResult(
        if (saved.missingKinds.isEmpty()) Tone.OK else Tone.WARNING,
        "保存しました" + (savedAt?.let { " $it" } ?: "") + if (saved.autoEventId != null) "（自動保存）" else "",
        listOfNotNull(saved.missingKinds.takeIf { it.isNotEmpty() }
            ?.let { "記録の不足・未確認: ${it.map(::streamName).joinToString("、")}" }),
        saved.directory,
    )
    SavePhase.FAILED -> RecentResult(Tone.ERROR, saveDescription(saved), failure = SaveFailure(
        reason = "保存失敗の理由: ${saved.error ?: "保存先を確認してください。"}",
        fixedRequest = "固定対象: ${recordTime(saved.windowStartNs)}〜${recordTime(saved.windowEndNs)} / ${saved.replaySeconds ?: seconds}秒" +
            " / セッション: ${saved.sequenceId ?: "未確定"}" +
            " / 記録の不足・未確認: ${saved.missingKinds.map(::streamName).joinToString("、").ifEmpty { "なし" }}",
        retainedOutput = saved.retainedPath?.let { "保全した出力: $it" },
        actionsEnabled = !busy,
    ))
}

private fun pendingSettings(input: ToolWindowInput): String? {
    val applied = input.snapshot?.settings ?: return null
    val stored = input.settings
    val pending = buildList {
        if (applied.captureMode != stored.captureMode) add("取得モード: ${captureModeDescription(stored.captureMode)}")
        if (stored.captureMode.videoRequested && applied.videoQuality != stored.videoQuality) add("動画品質")
        if (applied.autoSaveOnCrash != stored.autoSaveOnCrash) add("クラッシュ自動保存")
    }
    return pending.takeIf { it.isNotEmpty() }?.joinToString("、", prefix = "設定の反映待ち: ")
}
