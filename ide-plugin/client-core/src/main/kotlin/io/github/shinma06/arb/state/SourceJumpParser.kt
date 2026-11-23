package io.github.shinma06.arb.state

import io.github.shinma06.arb.model.CrashMarker
import io.github.shinma06.arb.model.ScreenSpan

/**
 * Pure PC-side source navigation target parsed from Android logcat stack traces,
 * crash markers, or Activity screen spans. Requires zero device interaction.
 */
data class SourceJumpTarget(
    val displayLabel: String,
    val classFqn: String? = null,
    val shortClassName: String? = null,
    val fileName: String? = null,
    val lineNumber: Int? = null,
) {
    val isResolvable: Boolean
        get() = !classFqn.isNullOrBlank() || !shortClassName.isNullOrBlank() || !fileName.isNullOrBlank()
}

/**
 * Deterministic parser for Java/Kotlin stack-trace frames, file:line references,
 * Activity screen spans, and Android crash markers.
 */
object SourceJumpParser {
    // Matches: "at com.example.app.ui.CheckoutViewModel.submit(CheckoutViewModel.kt:84)"
    // or "at com.example.app.Foo$Bar.invoke(Foo.kt:12)"
    private val STACK_FRAME_REGEX = Regex(
        """(?:^|\s)at\s+([A-Za-z_][A-Za-z0-9_$.]*)\.([A-Za-z0-9_<>$]+)\(([^():\s]+\.(?:kt|java|kts)):(\d+)\)""",
    )

    // Matches standalone "(LoginActivity.kt:42)" or "CheckoutActivity.java:108"
    private val FILE_LINE_REGEX = Regex(
        """\b([A-Z][A-Za-z0-9_]*\.(?:kt|java)):(\d+)\b""",
    )

    // Matches FQN ending in an Activity/Fragment/ViewModel/Service class token in log text
    private val FQN_CLASS_REGEX = Regex(
        """\b((?:[a-z][a-z0-9_]*\.){2,}[A-Z][A-Za-z0-9_]*)(?:\$[A-Za-z0-9_]+)?\b""",
    )

    fun fromScreenSpan(span: ScreenSpan): SourceJumpTarget = SourceJumpTarget(
        displayLabel = span.shortName,
        classFqn = span.activityName.substringBefore('$'),
        shortClassName = span.shortName.substringBefore('$'),
    )

    fun fromCrashMarker(marker: CrashMarker): SourceJumpTarget {
        parseLogText(marker.message)?.let { return it }
        val cleanClass = marker.exceptionClass.substringBefore('$').trim()
        val shortName = cleanClass.substringAfterLast('.')
        return SourceJumpTarget(
            displayLabel = shortName.ifEmpty { marker.processName },
            classFqn = cleanClass.takeIf { it.contains('.') },
            shortClassName = shortName.takeIf { it.isNotEmpty() },
        )
    }

    fun parseLogText(text: String): SourceJumpTarget? {
        if (text.isBlank()) return null

        STACK_FRAME_REGEX.findAll(text).firstOrNull { !isSystemNoiseClass(it.groupValues[1]) }?.let { m ->
            val rawClass = m.groupValues[1]
            val outerClass = rawClass.substringBefore('$')
            val fileName = m.groupValues[3]
            val line = m.groupValues[4].toIntOrNull()?.takeIf { it > 0 }
            val shortName = outerClass.substringAfterLast('.')
            val label = if (line != null) "$fileName:$line" else fileName
            return SourceJumpTarget(
                displayLabel = label,
                classFqn = outerClass,
                shortClassName = shortName.takeIf { it.isNotEmpty() },
                fileName = fileName,
                lineNumber = line,
            )
        }

        FILE_LINE_REGEX.find(text)?.let { m ->
            val fileName = m.groupValues[1]
            val line = m.groupValues[2].toIntOrNull()?.takeIf { it > 0 }
            val shortName = fileName.substringBeforeLast('.')
            val label = if (line != null) "$fileName:$line" else fileName
            return SourceJumpTarget(
                displayLabel = label,
                shortClassName = shortName,
                fileName = fileName,
                lineNumber = line,
            )
        }

        FQN_CLASS_REGEX.findAll(text).firstOrNull { !isSystemNoiseClass(it.groupValues[1]) }?.let { m ->
            val fqn = m.groupValues[1]
            val shortName = fqn.substringAfterLast('.')
            return SourceJumpTarget(
                displayLabel = shortName,
                classFqn = fqn,
                shortClassName = shortName,
            )
        }

        return null
    }

    private fun isSystemNoiseClass(fqn: String): Boolean =
        fqn.startsWith("java.") ||
            fqn.startsWith("javax.") ||
            fqn.startsWith("kotlin.") ||
            fqn.startsWith("kotlinx.") ||
            fqn.startsWith("dalvik.") ||
            fqn.startsWith("android.") ||
            fqn.startsWith("androidx.") ||
            fqn.startsWith("com.android.") ||
            fqn.startsWith("com.google.android.") ||
            fqn.startsWith("sun.")
}
