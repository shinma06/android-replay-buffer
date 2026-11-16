package io.github.shinma06.replaybuffer.ui

import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CornerSize
import androidx.compose.foundation.text.BasicText
import androidx.compose.ui.ImageComposeScene
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.layout.boundsInRoot
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.DpSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import io.github.shinma06.replaybuffer.core.ReplayTimeline
import org.jetbrains.jewel.ui.component.styling.ButtonMetrics
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class ReplayPlayerButtonsLayoutTest {
    private val normal = ButtonMetrics(CornerSize(4.dp), PaddingValues(horizontal = 14.dp, vertical = 4.dp),
        DpSize(72.dp, 28.dp), 1.dp, 2.dp)

    @Test
    fun compactMetricsKeepTheThemeHeightAndShapeButReserveLessHorizontalSpace() {
        val compact = compactPlayerButtonMetrics(normal)
        assertEquals(normal.cornerSize, compact.cornerSize)
        assertEquals(normal.minSize.height, compact.minSize.height)
        assertEquals(normal.padding.calculateTopPadding(), compact.padding.calculateTopPadding())
        assertEquals(normal.borderWidth, compact.borderWidth)
        assertEquals(normal.focusOutlineExpand, compact.focusOutlineExpand)
        assertEquals(40.dp, compact.minSize.width)
    }

    @Test
    fun allLabelsFitInANarrowRowIncludingCancellationInProgress() {
        val rules = TrimRules(FrameGrid(listOf(ReplayTimeline.Span(0, 1_000_000))), 1_000_000)
        val trim = rules.enterTrim(rules.initial())
        val running = rules.taskStarted(trim, TaskKind.TRIM)
        val png = rules.taskStarted(rules.initial(), TaskKind.FRAME)
        val states = listOf(rules.initial(), trim, running, rules.cancel(running).first, png, rules.cancel(png).first)
        val metrics = compactPlayerButtonMetrics(normal)
        for (scale in listOf(1f, 2f)) for (state in states) {
            val boxes = mutableListOf<Rect>()
            val overflows = mutableListOf<Boolean>()
            val buttons = rowOrder(playerButtons(rules, state, PlayerAvailability(false, null, false, previewReady = true)))
            val scene = ImageComposeScene((360 * scale).toInt(), (60 * scale).toInt(), Density(scale)) {
                Row(Modifier.width(360.dp).padding(horizontal = 22.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    buttons.forEach { button ->
                        BasicText(button.label, Modifier.defaultMinSize(metrics.minSize.width, metrics.minSize.height)
                            .padding(metrics.padding).onGloballyPositioned { boxes.add(it.boundsInRoot()) },
                            style = TextStyle(fontSize = 13.sp), maxLines = 1, softWrap = false,
                            overflow = TextOverflow.Ellipsis, onTextLayout = { overflows.add(it.didOverflowWidth) })
                    }
                }
            }
            try { scene.render() } finally { scene.close() }
            assertEquals(buttons.size, overflows.size)
            assertFalse(overflows.any { it }, "all button labels must fit: $state at density $scale")
            assertTrue(boxes.all { it.left >= 0 && it.right <= 360 * scale })
        }
    }
}
