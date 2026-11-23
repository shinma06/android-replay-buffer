package io.github.shinma06.arb.ui

import io.github.shinma06.arb.shm.ShmFrameMeta
import io.github.shinma06.arb.state.APPROXIMATE_PREVIEW_BADGE_TEXT
import java.awt.BorderLayout
import java.awt.Color
import java.awt.Dimension
import java.awt.Font
import java.awt.Graphics
import java.awt.Graphics2D
import java.awt.RenderingHints
import java.awt.image.BufferedImage
import javax.swing.BorderFactory
import javax.swing.JLabel
import javax.swing.JPanel

/**
 * Video frame viewport rendering frames decoded by `ShmFrameRingReader`.
 *
 * Enforces the mandatory ARB UX rule (`SPEC-03`): whenever `frameMeta.isApproximate == true`,
 * prominently displays the `"近似プレビュー（位置確定前）"` badge; clears it ONLY when a settled
 * frame arrives with `isApproximate == false`.
 */
class VideoViewportCanvas : JPanel(BorderLayout()) {
    private val approximateBadgeLabel = JLabel(APPROXIMATE_PREVIEW_BADGE_TEXT).apply {
        isOpaque = true
        background = Color(0xD9, 0x77, 0x06)
        foreground = Color.WHITE
        font = font.deriveFont(Font.BOLD, 13f)
        border = BorderFactory.createEmptyBorder(4, 10, 4, 10)
        isVisible = false
    }

    private val frameHudLabel = JLabel("フレーム未読込").apply {
        isOpaque = true
        background = Color(0x11, 0x18, 0x27)
        foreground = Color(0xE5, 0xE7, 0xEB)
        border = BorderFactory.createEmptyBorder(3, 8, 3, 8)
    }

    private var currentImage: BufferedImage? = null
    private var currentMeta: ShmFrameMeta? = null

    private val imageSurface = object : JPanel() {
        init {
            background = Color(0x0F, 0x17, 0x2A)
            preferredSize = Dimension(480, 320)
        }

        override fun paintComponent(g: Graphics) {
            super.paintComponent(g)
            val img = currentImage ?: return
            val g2 = g as Graphics2D
            g2.setRenderingHint(RenderingHints.KEY_INTERPOLATION, RenderingHints.VALUE_INTERPOLATION_BILINEAR)
            val scale = minOf(
                width.toDouble() / img.width.toDouble(),
                height.toDouble() / img.height.toDouble(),
            )
            val drawW = (img.width * scale).toInt().coerceAtLeast(1)
            val drawH = (img.height * scale).toInt().coerceAtLeast(1)
            val x = (width - drawW) / 2
            val y = (height - drawH) / 2
            g2.drawImage(img, x, y, drawW, drawH, null)
        }
    }

    init {
        val header = JPanel(BorderLayout()).apply {
            background = Color(0x11, 0x18, 0x27)
            add(frameHudLabel, BorderLayout.WEST)
            add(approximateBadgeLabel, BorderLayout.EAST)
        }
        add(header, BorderLayout.NORTH)
        add(imageSurface, BorderLayout.CENTER)
    }

    fun updateFrame(meta: ShmFrameMeta?, image: BufferedImage?) {
        currentMeta = meta
        if (image != null) {
            currentImage = image
        }
        val approx = meta?.isApproximate == true
        approximateBadgeLabel.isVisible = approx
        approximateBadgeLabel.text = APPROXIMATE_PREVIEW_BADGE_TEXT
        frameHudLabel.text = if (meta != null) {
            val mode = if (approx) "近似プレビュー" else "確定フレーム"
            "Frame #${meta.frameIndex} (${meta.width}x${meta.height}, gen=${meta.generation}, $mode)"
        } else {
            "フレーム未読込"
        }
        imageSurface.repaint()
    }

    fun isApproximateBadgeVisible(): Boolean = approximateBadgeLabel.isVisible

    fun approximateBadgeText(): String = approximateBadgeLabel.text
}
