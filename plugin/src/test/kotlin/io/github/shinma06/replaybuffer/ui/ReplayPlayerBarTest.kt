package io.github.shinma06.replaybuffer.ui

import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Canvas
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.drawscope.CanvasDrawScope
import androidx.compose.ui.graphics.drawscope.translate
import androidx.compose.ui.graphics.toPixelMap
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.LayoutDirection
import kotlin.test.Test
import kotlin.test.assertEquals

class ReplayPlayerBarTest {
    @Test
    fun playheadHasAnOpaqueThickLineOutlineAndLargerMarkerInBothThemesAndDensities() {
        for (foreground in listOf(Color.White, Color.Black)) {
            val outline = if (foreground == Color.White) Color.Black else Color.White
            for (density in listOf(1f, 1.5f, 2f)) {
                val pixels = render(160, 80f, density, foreground, outline).toPixelMap()
                fun at(x: Int, y: Int) = pixels[(x * density).toInt(), (y * density).toInt()]
                assertEquals(foreground, at(80, 32))
                assertEquals(outline, at(83, 32))
                assertEquals(Color.Red, at(85, 32))
                assertEquals(foreground, at(83, 8))
                assertEquals(outline, at(86, 8))
                assertEquals(Color.Red, at(89, 8))
            }
        }
    }

    @Test
    fun markersRemainVisibleAtBothExactTimelineEndpoints() {
        for (x in listOf(0f, 160f)) {
            val pixels = render(160, x).toPixelMap()
            val edge = if (x == 0f) 0 else 159
            val inward = if (x == 0f) 1 else -1
            assertEquals(Color.White, pixels[edge, 32])
            assertEquals(Color.White, pixels[edge + 3 * inward, 8])
            assertEquals(Color.Black, pixels[edge + 6 * inward, 8])
        }
    }

    @Test
    fun narrowStripsStillDrawTheExactPlayheadWithoutDividingOrMovingItsCoordinate() {
        for (width in listOf(1, 8, 23)) {
            val pixels = render(width, width / 2f).toPixelMap()
            assertEquals(Color.White, pixels[width / 2, 8])
            assertEquals(Color.White, pixels[width / 2, 40])
        }
    }

    @Test
    fun endpointMarkersDoNotPaintTheNonInteractiveParentMargins() {
        for (x in listOf(0f, 160f)) {
            val pixels = render(160, x, paddingDp = 12).toPixelMap()
            for (y in 0 until 48) for (margin in 0 until 12) {
                assertEquals(Color.Red, pixels[margin, y])
                assertEquals(Color.Red, pixels[172 + margin, y])
            }
        }
    }

    @Test
    fun overlappingPlayheadsPreserveBothExistingGripMarks() {
        val before = render(160, 0f, handleMarks = true, playhead = false).toPixelMap()
        for (x in listOf(4f, 6f, 154f, 156f)) {
            val after = render(160, x, handleMarks = true).toPixelMap()
            for (mark in listOf(5, 7, 153, 155)) for (y in 18..29) {
                assertEquals(before[mark, y], after[mark, y], "playhead=$x, grip=$mark,$y")
            }
        }
    }

    private fun render(widthDp: Int, xDp: Float, density: Float = 1f,
        foreground: Color = Color.White, outline: Color = Color.Black,
        paddingDp: Int = 0, handleMarks: Boolean = false, playhead: Boolean = true): ImageBitmap {
        val width = (widthDp * density).toInt()
        val height = (48 * density).toInt()
        val padding = (paddingDp * density).toInt()
        val bitmap = ImageBitmap(width + 2 * padding, height)
        CanvasDrawScope().draw(Density(density), LayoutDirection.Ltr, Canvas(bitmap), Size(width.toFloat(), height.toFloat())) {
            drawRect(Color.Red, size = Size(bitmap.width.toFloat(), height.toFloat()))
            translate(left = padding.toFloat()) {
                if (handleMarks) for (left in listOf(0f, width - 12 * density)) {
                    drawRect(Color.Blue, Offset(left, 0f), Size(12 * density, height.toFloat()))
                    for (dx in listOf(5, 7)) drawLine(Color.Black,
                        Offset(left + dx * density, height * 0.35f), Offset(left + dx * density, height * 0.65f),
                        strokeWidth = density)
                }
                if (playhead) drawPlayerPlayhead(xDp * density, foreground.copy(alpha = 0.5f), outline.copy(alpha = 0.2f),
                    0f..width.toFloat(), 12 * density)
            }
        }
        return bitmap
    }
}
