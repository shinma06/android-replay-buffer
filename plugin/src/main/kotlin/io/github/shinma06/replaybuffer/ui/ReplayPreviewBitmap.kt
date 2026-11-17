package io.github.shinma06.replaybuffer.ui

import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asComposeImageBitmap
import androidx.compose.ui.graphics.toComposeImageBitmap
import io.github.shinma06.replaybuffer.core.ReplayPreviewImage
import org.jetbrains.skia.Bitmap
import org.jetbrains.skia.ColorAlphaType
import org.jetbrains.skia.ColorType
import org.jetbrains.skia.ImageInfo
import java.awt.image.BufferedImage

/** One BGRA Java-to-native copy. No getRGB loop / repacking; GPU upload remains Skia's job. */
internal fun BufferedImage.previewBitmap(): ImageBitmap {
    if (this !is ReplayPreviewImage) return toComposeImageBitmap()
    val bitmap = Bitmap()
    try {
        check(bitmap.installPixels(ImageInfo(width, height, ColorType.BGRA_8888, ColorAlphaType.OPAQUE), pixels, width * 4))
        return bitmap.asComposeImageBitmap()
    } catch (failure: Throwable) { bitmap.close(); throw failure }
}
