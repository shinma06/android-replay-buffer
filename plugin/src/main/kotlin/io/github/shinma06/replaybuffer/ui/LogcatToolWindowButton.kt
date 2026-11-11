package io.github.shinma06.replaybuffer.ui

import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationInfo
import com.intellij.openapi.wm.ToolWindow
import io.github.shinma06.replaybuffer.core.ReplayDevice
import java.util.concurrent.CompletableFuture
import java.util.concurrent.CancellationException
import java.util.concurrent.TimeUnit
import java.util.function.BooleanSupplier
import javax.swing.JButton
import javax.swing.SwingUtilities

/** IntelliJ baseline + Android line of Studio releases whose Logcat ABI was verified; patch builds of a line share it. */
private val verifiedLogcatLines = setOf("262.9437.185.2621", "262.10968.63.2622")

/** Gates loading the selection bridge; a patch whose ABI still fails to link falls back to open-only at runtime. */
internal fun logcatTargetBuildReason(build: String): String? {
    val parts = build.removePrefix("AI-").takeIf { it != build }?.split('.')
    val known = parts != null && parts.size == 5 && parts.all { it.isNotEmpty() && it.all(Char::isDigit) } &&
        parts.take(4).joinToString(".") in verifiedLogcatLines
    return if (known) null else "このAndroid Studioのバージョンでは、対象端末を指定してLogcatを開けません。"
}

private fun currentBuildSelectsDevice(): Boolean = logcatTargetBuildReason(ApplicationInfo.getInstance().build.asString()) == null

private fun Throwable.isLinkageFailure(): Boolean = generateSequence(this) { it.cause }.take(8).any { it is LinkageError }

/** EDT-only view of the IDE-owned window; callbacks must stop resolving it after content/project disposal. */
internal class LogcatToolWindowButton(
    private val alive: () -> Boolean,
    private val target: () -> ReplayDevice?,
    private val selectDevice: (ToolWindow, String, BooleanSupplier, Long) -> CompletableFuture<String?>,
    /** True when Logcat has no tab to select a device in; the IDE creates one only when the window is shown. */
    private val needsTab: (ToolWindow) -> Boolean = { false },
    /** False on builds whose Logcat ABI is unverified: the button then only opens/closes the IDE's own selection. */
    private val canSelectDevice: () -> Boolean = ::currentBuildSelectsDevice,
    private val logcat: () -> ToolWindow?,
) : JButton(), Disposable {
    private var failure: String? = null
    private var failedSerial: String? = null
    @Volatile
    private var pending: CompletableFuture<String?>? = null
    @Volatile
    private var disposed = false
    /** The pending selection runs on a window shown first, so showing it must not cancel the request. */
    private var selectingShown = false
    /** A selection on a window shown first failed; keep its reason while that window stays visible. */
    private var shownFailure = false
    /** The selection bridge failed to link on this IDE; later opens skip it for this content's lifetime. */
    private var selectionBroken = false

    init {
        addActionListener {
            if (pending != null) {
                cancelPending()
                refresh()
                return@addActionListener
            }
            val deadlineNs = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
            // Re-read on click: the IDE may have changed visibility or availability since the last notification.
            availableWindow()?.let { window ->
                if (window.isVisible) {
                    window.hide()
                } else if (!selectsDevice()) {
                    failure = null
                    window.show()
                } else {
                    val device = target()?.takeIf { it.connected && it.serial.isNotBlank() }
                    if (device != null) {
                        failedSerial = device.serial
                        failure = null
                        val shown = try {
                            needsTab(window)
                        } catch (_: LinkageError) {
                            false
                        } catch (_: RuntimeException) {
                            false
                        }
                        if (shown) {
                            selectingShown = true
                            window.show()
                        }
                        val request: CompletableFuture<String?> = try {
                            selectDevice(window, device.serial, BooleanSupplier {
                                isAlive() && target()?.let { it.connected && it.serial == device.serial } == true
                            }, deadlineNs)
                        } catch (_: CancellationException) {
                            CompletableFuture<String?>().apply { cancel(false) }
                        } catch (_: LinkageError) {
                            openWithoutSelection(window)
                            refresh()
                            return@addActionListener
                        } catch (_: RuntimeException) {
                            CompletableFuture.completedFuture("標準Logcatの対象端末を確認できません。接続と選択中のタブを確認して再試行してください。")
                        }
                        pending = request
                        request.whenComplete { reason, error ->
                            val finish = Runnable {
                                if (pending !== request) return@Runnable
                                pending = null
                                val shown = selectingShown
                                selectingShown = false
                                if (!isAlive()) return@Runnable
                                if (request.isCancelled) {
                                    refresh()
                                    return@Runnable
                                }
                                if (error?.isLinkageFailure() == true) {
                                    if (availableWindow() === window) openWithoutSelection(window) else selectionBroken = true
                                    refresh()
                                    return@Runnable
                                }
                                val current = target()
                                failure = reason ?: error?.let { "標準Logcatの端末切替を確認できません。再試行してください。" }
                                if (current?.connected != true || current.serial != device.serial) {
                                    failedSerial = current?.serial
                                    failure = "Replay Bufferの対象端末が変わりました。再試行してください。"
                                } else if (failure == null && System.nanoTime() >= deadlineNs) {
                                    failure = "標準Logcatの準備が期限内に完了しませんでした。再試行してください。"
                                } else if (failure == null && availableWindow() === window && !window.isVisible) {
                                    window.show()
                                }
                                shownFailure = shown && failure != null
                                refresh()
                            }
                            if (SwingUtilities.isEventDispatchThread()) finish.run() else SwingUtilities.invokeLater(finish)
                        }
                    }
                }
            }
            refresh()
        }
    }

    fun refresh() {
        val window = availableWindow()
        val device = if (isAlive()) target() else null
        if (window == null || window.isVisible != selectingShown || device?.connected != true || device.serial != failedSerial) cancelPending()
        if (window?.isVisible != true) shownFailure = false
        if ((window?.isVisible == true && !shownFailure) || device?.serial != failedSerial) failure = null
        val targetReason = when {
            device == null || device.serial.isBlank() -> "取得を有効にしてReplay Bufferの対象端末を確定してください。"
            !device.connected -> "Replay Bufferの対象端末が切断されています。再接続後に再試行してください。"
            else -> null
        }
        val selects = selectsDevice()
        isEnabled = window != null && (window.isVisible || !selects || targetReason == null)
        text = when {
            pending != null -> "Logcatを開くのを中止"
            window?.isVisible == true -> "Logcatを閉じる"
            else -> "Logcatを開く"
        }
        toolTipText = when {
            window == null -> "このprojectではIDE標準Logcatを利用できません。"
            pending != null -> "対象端末のLogcatを準備しています（最大5秒）。再度押すと中止します。"
            window.isVisible && failure != null -> "$failure 押すとIDE標準Logcatパネルを非表示にします。"
            window.isVisible && !selects -> "IDE標準Logcatパネルを非表示にします。$UNMATCHED_DEVICE"
            window.isVisible -> "IDE標準Logcatパネルを非表示にします。"
            !selects -> "IDE標準Logcatを表示します。$UNMATCHED_DEVICE"
            targetReason != null -> targetReason
            failure != null -> failure
            else -> "Replay Bufferの対象端末（${device?.serial}）に合わせてIDE標準Logcatを表示します。"
        }
        getAccessibleContext().accessibleName = text
        getAccessibleContext().accessibleDescription = toolTipText
    }

    private fun selectsDevice(): Boolean = !selectionBroken && try {
        canSelectDevice()
    } catch (_: LinkageError) {
        false
    } catch (_: RuntimeException) {
        false
    }

    private fun openWithoutSelection(window: ToolWindow) {
        selectionBroken = true
        selectingShown = false
        shownFailure = false
        failure = null
        if (!window.isVisible) window.show()
    }

    private fun cancelPending() {
        val request = pending ?: return
        pending = null
        selectingShown = false
        request.cancel(false)
    }

    /** Called when the IDE shows Logcat; only a selection on an already shown window survives it. */
    fun cancelOpen() {
        if (!selectingShown) cancelPending()
        refresh()
    }

    override fun dispose() {
        disposed = true
        cancelPending()
    }

    private fun isAlive(): Boolean = !disposed && alive()

    private companion object {
        const val UNMATCHED_DEVICE = "このAndroid Studioのビルドでは対象端末にLogcatを合わせられないため、IDEで選択中の端末のログが表示されます。"
    }

    private fun availableWindow(): ToolWindow? = if (isAlive()) {
        logcat()?.takeUnless { it.isDisposed || !it.isAvailable }
    } else {
        null
    }
}
