package io.github.shinma06.replaybuffer.menubar

import com.intellij.openapi.diagnostic.Logger
import java.util.IdentityHashMap

internal enum class WindowsTrayOpenRejection { ICON_REPLACED, REVISION_CHANGED, CLOSED, ALREADY_ACTIVE, NO_MENU }
internal enum class WindowsTrayOperation { SHOW, GUARDED }

/** Windows-only diagnostics. Neither logger failures nor diagnostic data may change menu behaviour. */
internal class WindowsTrayDiagnostics(
    private val warn: (String, Throwable?) -> Unit = { message, failure ->
        Logger.getInstance(WindowsTraySurface::class.java).warn(message, failure)
    },
) {
    fun rejected(reason: WindowsTrayOpenRejection) {
        runCatching { warn("Windowsトレイのメニュー表示を中止しました。reason=${reason.name}", null) }
    }

    fun <T> guard(operation: WindowsTrayOperation, action: () -> T, onFailure: () -> T): T =
        try { action() } catch (failure: Exception) {
            runCatching {
                warn("Windowsトレイの処理に失敗しました。operation=${operation.name}", windowsTrayDiagnostic(failure))
            }
            onFailure()
        }

    /** The queued callback still rejects a replaced/hidden icon before touching the menu state. */
    fun openIfCurrent(current: Boolean, open: () -> Unit) {
        if (current) open() else rejected(WindowsTrayOpenRejection.ICON_REPLACED)
    }
}

/*
 * Throwable messages are arbitrary free text, including paths, device names and credentials. Regex path stripping
 * cannot guarantee privacy. Retain exact public, fixed platform messages only; explicitly mark all other message
 * fields as redacted. Type, call order, method and line numbers remain useful even for an unknown message.
 * Never pass the original Throwable (including its causes/suppressed exceptions) to the IDE logger.
 */
private val PUBLIC_TRAY_MESSAGES = setOf(
    "The dialog is decorated", "The dialog is decorated.",
    "The dialog is displayable", "The dialog is displayable.",
    "The window is decorated", "The window is decorated.",
    "The window is displayable", "The window is displayable.",
    "The window is showing on screen.", "Window must not be displayable",
    "The owner is not a Frame or Dialog", "Wrong parent window",
    "This operation is not supported", "width and height must be > 0",
)
private val CLASS_NAME = Regex("[A-Za-z_$][A-Za-z0-9_$.]*")
private val METHOD_NAME = Regex("[A-Za-z_$][A-Za-z0-9_$]*|<init>|<clinit>")

/** Bounded, detached copy: no original free text, source paths, module names or class-loader names escape. */
internal fun windowsTrayDiagnostic(failure: Throwable): Throwable {
    val seen = IdentityHashMap<Throwable, Boolean>()
    var remaining = 32
    fun copy(original: Throwable, depth: Int): Throwable {
        val type = original.javaClass.name
        val stopped = when {
            seen.put(original, true) != null -> "[cycle omitted]"
            depth >= 8 -> "[cause depth truncated]"
            remaining-- <= 0 -> "[exception graph truncated]"
            else -> null
        }
        val message = stopped ?: original.message?.let {
            if (it in PUBLIC_TRAY_MESSAGES) it else "[redacted: free-text message]"
        }
        val result = WindowsTrayDiagnosticFailure(type, message)
        if (stopped != null) {
            result.stackTrace = emptyArray()
            return result
        }
        val frames = original.stackTrace
        val safeFrames = frames.take(128).map { frame ->
            StackTraceElement(
                frame.className.takeIf(CLASS_NAME::matches) ?: "[class redacted]",
                frame.methodName.takeIf(METHOD_NAME::matches) ?: "[method redacted]",
                "[source redacted]", frame.lineNumber,
            )
        }
        result.stackTrace = (safeFrames + if (frames.size > 128)
            listOf(StackTraceElement("[truncated]", "frames", "[${frames.size - 128} omitted]", -1))
        else emptyList()).toTypedArray()
        original.cause?.let { result.initCause(copy(it, depth + 1)) }
        original.suppressed.take(4).forEach { result.addSuppressed(copy(it, depth + 1)) }
        if (original.suppressed.size > 4) result.addSuppressed(
            WindowsTrayDiagnosticFailure("[truncated]", "[${original.suppressed.size - 4} suppressed omitted]")
                .apply { stackTrace = emptyArray() },
        )
        return result
    }
    return copy(failure, 0)
}

private class WindowsTrayDiagnosticFailure(private val originalType: String, message: String?) : RuntimeException(message) {
    override fun toString(): String = originalType + (message?.let { ": $it" } ?: "")
}
