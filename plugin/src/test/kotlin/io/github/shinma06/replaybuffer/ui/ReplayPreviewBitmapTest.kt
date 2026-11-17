package io.github.shinma06.replaybuffer.ui

import androidx.compose.ui.graphics.asSkiaBitmap
import androidx.compose.ui.graphics.toComposeImageBitmap
import io.github.shinma06.replaybuffer.core.MixedVideoNormalizer
import io.github.shinma06.replaybuffer.core.ReplayFrameExport
import io.github.shinma06.replaybuffer.core.ReplayPreviewImage
import org.jcodec.common.model.ColorSpace
import org.jcodec.common.model.Picture
import org.jetbrains.skia.Bitmap
import org.jetbrains.skia.Image
import org.jetbrains.skia.Rect
import org.jetbrains.skia.SamplingMode
import org.jetbrains.skia.Surface
import org.junit.jupiter.api.Test
import kotlin.random.Random
import kotlin.test.assertContentEquals
import kotlin.test.assertTrue

class ReplayPreviewBitmapTest {
    @Test fun packedCopyAndPinnedSkiaBilinearRenderEqualOriginalFullRgbReferenceExactly() {
        val random = Random(31)
        for (color in listOf(ColorSpace.YUV420, ColorSpace.YUV420J)) for (matrix in listOf(null, 1)) {
            val p = Picture.create(66, 34, color).apply { data.forEach { random.nextBytes(it) } }
            val frame = MixedVideoNormalizer.DisplayFrame(p, matrix)
            val original = ReplayFrameExport.rgbImage(frame)
            val preview = ReplayPreviewImage.convert(frame)
            for (height in listOf(11, 17, 34)) {
                val a = scaledThumbnail(original, height); val b = scaledThumbnail(preview, height)
                assertContentEquals(a.getRGB(0, 0, a.width, a.height, null, 0, a.width),
                    b.getRGB(0, 0, b.width, b.height, null, 0, b.width))
            }
            val old = original.toComposeImageBitmap().asSkiaBitmap()
            val packed = preview.previewBitmap().asSkiaBitmap()
            for ((w, h) in listOf(66 to 34, 17 to 9, 101 to 53)) {
                assertContentEquals(render(old, w, h), render(packed, w, h), "$color / $matrix / ${w}x$h")
            }
        }
    }

    private fun render(bitmap: Bitmap, w: Int, h: Int): IntArray = Surface.makeRasterN32Premul(w, h).use { surface ->
        Image.makeFromBitmap(bitmap).use { image ->
            surface.canvas.drawImageRect(image, Rect.makeWH(bitmap.width.toFloat(), bitmap.height.toFloat()),
                Rect.makeWH(w.toFloat(), h.toFloat()), SamplingMode.LINEAR, null, true)
        }
        Bitmap().use { read ->
            assertTrue(read.allocN32Pixels(w, h))
            assertTrue(surface.readPixels(read, 0, 0))
            IntArray(w * h) { i -> read.getColor(i % w, i / w) }
        }
    }
}
