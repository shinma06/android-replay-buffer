package io.github.shinma06.replaybuffer.ui

import com.intellij.openapi.Disposable
import com.intellij.openapi.wm.ToolWindow
import io.github.shinma06.replaybuffer.core.ReplayDevice
import java.util.concurrent.CompletableFuture
import java.util.concurrent.CancellationException
import java.util.concurrent.TimeUnit
import java.util.function.BooleanSupplier
import javax.swing.JButton
import javax.swing.SwingUtilities

internal fun logcatTargetBuildReason(build: String): String? =
    when (build) {
        "AI-262.9437.185.2621.16467767", "AI-262.10968.63.2622.16730706" -> null
        else -> "このAndroid Studioのバージョンでは、対象端末を指定してLogcatを開けません。"
    }

/** EDT-only view of the IDE-owned window; callbacks must stop resolving it after content/project disposal. */
internal class LogcatToolWindowButton(
    private val alive: () -> Boolean,
    private val target: () -> ReplayDevice?,
    private val selectDevice: (ToolWindow, String, BooleanSupplier, Long) -> CompletableFuture<String?>,
    private val logcat: () -> ToolWindow?,
) : JButton(), Disposable {
    private var failure: String? = null
    private var failedSerial: String? = null
    @Volatile
    private var pending: CompletableFuture<String?>? = null
    @Volatile
    private var disposed = false

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
                } else {
                    val device = target()?.takeIf { it.connected && it.serial.isNotBlank() }
                    if (device != null) {
                        failedSerial = device.serial
                        failure = null
                        val request: CompletableFuture<String?> = try {
                            selectDevice(window, device.serial, BooleanSupplier {
                                isAlive() && target()?.let { it.connected && it.serial == device.serial } == true
                            }, deadlineNs)
                        } catch (_: CancellationException) {
                            CompletableFuture<String?>().apply { cancel(false) }
                        } catch (_: LinkageError) {
                            CompletableFuture.completedFuture("Android Studioとの互換性を確認できないため、対象端末のLogcatを開けません。")
                        } catch (_: RuntimeException) {
                            CompletableFuture.completedFuture("標準Logcatの対象端末を確認できません。接続と選択中のタブを確認して再試行してください。")
                        }
                        pending = request
                        request.whenComplete { reason, error ->
                            val finish = Runnable {
                                if (pending !== request) return@Runnable
                                pending = null
                                if (!isAlive()) return@Runnable
                                if (request.isCancelled) {
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
        if (window == null || window.isVisible || device?.connected != true || device.serial != failedSerial) cancelPending()
        if (window?.isVisible == true || device?.serial != failedSerial) failure = null
        val targetReason = when {
            device == null || device.serial.isBlank() -> "取得を有効にしてReplay Bufferの対象端末を確定してください。"
            !device.connected -> "Replay Bufferの対象端末が切断されています。再接続後に再試行してください。"
            else -> null
        }
        isEnabled = window != null && (window.isVisible || targetReason == null)
        text = when {
            window?.isVisible == true -> "Logcatを閉じる"
            pending != null -> "Logcatを開くのを中止"
            else -> "Logcatを開く"
        }
        toolTipText = when {
            window == null -> "このprojectではIDE標準Logcatを利用できません。"
            window.isVisible -> "IDE標準Logcatパネルを非表示にします。"
            targetReason != null -> targetReason
            pending != null -> "対象端末のLogcatを準備しています（最大5秒）。再度押すと中止します。"
            failure != null -> failure
            else -> "Replay Bufferの対象端末（${device?.serial}）に合わせてIDE標準Logcatを表示します。"
        }
        getAccessibleContext().accessibleName = text
        getAccessibleContext().accessibleDescription = toolTipText
    }

    private fun cancelPending() {
        val request = pending
        pending = null
        request?.cancel(false)
    }

    fun cancelOpen() {
        cancelPending()
        refresh()
    }

    override fun dispose() {
        disposed = true
        cancelPending()
    }

    private fun isAlive(): Boolean = !disposed && alive()

    private fun availableWindow(): ToolWindow? = if (isAlive()) {
        logcat()?.takeUnless { it.isDisposed || !it.isAvailable }
    } else {
        null
    }
}
