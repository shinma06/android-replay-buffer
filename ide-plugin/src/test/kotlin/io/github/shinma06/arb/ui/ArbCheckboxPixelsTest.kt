@file:Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER") // Read the bundled IDE checkbox style for this rendering test.

package io.github.shinma06.arb.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CornerSize
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.ExperimentalComposeUiApi
import androidx.compose.ui.ImageComposeScene
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsNode
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.DpSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.jetbrains.jewel.bridge.icon.BridgeNewUiChecker
import org.jetbrains.jewel.bridge.theme.readCheckboxStyle
import org.jetbrains.jewel.foundation.BorderColors
import org.jetbrains.jewel.foundation.GlobalColors
import org.jetbrains.jewel.foundation.GlobalMetrics
import org.jetbrains.jewel.foundation.LocalGlobalColors
import org.jetbrains.jewel.foundation.LocalGlobalMetrics
import org.jetbrains.jewel.foundation.OutlineColors
import org.jetbrains.jewel.foundation.TextColors
import org.jetbrains.jewel.foundation.theme.LocalContentColor
import org.jetbrains.jewel.foundation.theme.LocalTextStyle
import org.jetbrains.jewel.foundation.theme.OverrideDarkMode
import org.jetbrains.jewel.ui.component.styling.CheckboxIcons
import org.jetbrains.jewel.ui.component.styling.CheckboxMetrics
import org.jetbrains.jewel.ui.component.styling.CheckboxStyle
import org.jetbrains.jewel.ui.component.styling.LocalCheckboxStyle
import org.jetbrains.jewel.ui.icon.LocalNewUiChecker
import org.jetbrains.jewel.ui.icon.PathIconKey
import org.jetbrains.jewel.ui.painter.CommonPainterHintsProvider
import org.jetbrains.jewel.ui.painter.LocalPainterHintsProvider
import org.jetbrains.skia.Bitmap
import org.junit.jupiter.api.Test
import kotlin.math.abs
import kotlin.math.ceil
import kotlin.math.roundToInt
import kotlin.test.assertTrue

@OptIn(ExperimentalComposeUiApi::class)
class ArbCheckboxPixelsTest {
    private val ink = Color.Black
    private val colors = GlobalColors(
        BorderColors(ink, ink, ink), OutlineColors(ink, ink, ink, ink, ink),
        TextColors(ink, ink, ink, ink, ink, ink, ink), Color.White, Color.White,
    )

    // Mirror the bundled Rabbit NewUiCheckboxMetrics and use its real 24px SVG in headless mode.
    private fun newUiStyle(): CheckboxStyle {
        val corners = CornerSize(3.dp)
        val normal = DpSize(16.dp, 16.dp)
        val selected = DpSize(20.dp, 20.dp)
        return CheckboxStyle(
            readCheckboxStyle().colors,
            CheckboxMetrics(DpSize(24.dp, 24.dp), corners, corners, corners, corners, normal, normal, selected, selected, 4.dp),
            CheckboxIcons(PathIconKey("themes/expUI/icons/dark/checkBox.svg", CheckboxIcons::class.java)),
        )
    }

    private data class Sample(val label: String, val width: Int, val wrapped: Boolean = false, val muted: Boolean = false)

    @Test
    fun indicatorAndFirstLineInkHaveTheSameCenter() {
        val samples = listOf(
            Sample("タップ・スワイプを動画に焼き込む", 300),
            Sample("自動追従", 120, muted = true),
            Sample("Android Studio の Logcat スクリーンショット保存先を継承する", 155, wrapped = true),
            Sample("端末の再生音を録音する\n次の行", 230, wrapped = true),
        )
        val failures = mutableListOf<String>()
        for (sample in samples) {
            for (density in listOf(Density(1f), Density(2f), Density(2f, 1.25f))) {
                for (style in listOf(readCheckboxStyle(), newUiStyle())) {
                    for (checked in listOf(false, true)) {
                        val (box, text) = centers(sample, density, style, checked)
                        val detail = "${sample.label.lineSequence().first()} $density checked=$checked icon=${style.metrics.checkboxSize} box=$box text=$text"
                        println(detail)
                        if (abs(box - text) > 1) failures += detail
                    }
                }
            }
        }
        assertTrue(failures.isEmpty(), failures.joinToString("\n"))
    }

    private fun centers(sample: Sample, density: Density, style: CheckboxStyle, checked: Boolean): Pair<Double, Double> {
        val scene = ImageComposeScene(800, 240, density) {
            CompositionLocalProvider(
                LocalGlobalColors provides colors, LocalGlobalMetrics provides GlobalMetrics(1.dp, 24.dp),
                LocalNewUiChecker provides BridgeNewUiChecker, LocalContentColor provides ink,
                LocalTextStyle provides TextStyle(lineHeight = 19.sp), LocalCheckboxStyle provides style,
                LocalPainterHintsProvider provides CommonPainterHintsProvider,
            ) {
                OverrideDarkMode(false) {
                    Box(Modifier.padding(6.dp).width(sample.width.dp)) {
                        ArbLabeledCheckbox(checked, {}, sample.label, muted = sample.muted)
                    }
                }
            }
        }
        try {
            scene.render().use { image ->
                Bitmap.makeFromImage(image).use { pixels ->
                    val node = textNode(scene.semanticsOwners.single().unmergedRootSemanticsNode)
                    val layouts = mutableListOf<TextLayoutResult>()
                    check(node.config[SemanticsActions.GetTextLayoutResult].action!!.invoke(layouts))
                    val layout = layouts.single()
                    check((layout.lineCount > 1) == sample.wrapped)
                    val bounds = node.boundsInRoot
                    val textX = bounds.left.roundToInt()
                    val firstLine = ceil(bounds.top + layout.getLineBottom(0)).toInt()

                    fun inkCenter(xs: IntRange, ys: IntRange): Double {
                        val rows = ys.filter { y ->
                            xs.any { x ->
                                val color = pixels.getColor(x, y)
                                check(color != Color.Magenta.toArgb()) { "Missing checkbox SVG" }
                                color.ushr(24) >= 96
                            }
                        }
                        check(rows.isNotEmpty()) { "No visible ink in $xs x $ys" }
                        return (rows.first() + rows.last()) / 2.0
                    }
                    return inkCenter(0 until textX, 0 until pixels.height) to
                        inkCenter(textX until ceil(bounds.right).toInt(), bounds.top.roundToInt() until firstLine)
                }
            }
        } finally {
            scene.close()
        }
    }

    private fun textNode(root: SemanticsNode): SemanticsNode {
        fun nodes(node: SemanticsNode): Sequence<SemanticsNode> = sequence {
            yield(node)
            node.children.forEach { yieldAll(nodes(it)) }
        }
        return nodes(root).single { it.config.getOrNull(SemanticsActions.GetTextLayoutResult) != null }
    }
}
