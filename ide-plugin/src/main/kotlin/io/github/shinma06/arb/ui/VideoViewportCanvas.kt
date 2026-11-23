package io.github.shinma06.arb.ui

import com.intellij.util.ui.JBUI
import io.github.shinma06.arb.shm.ShmFrameMeta
import io.github.shinma06.arb.state.APPROXIMATE_PREVIEW_BADGE_TEXT
import io.github.shinma06.arb.state.ActiveTapOverlay
import java.awt.BasicStroke
import java.awt.BorderLayout
import java.awt.Color
import java.awt.Dimension
import java.awt.Graphics
import java.awt.Graphics2D
import java.awt.Rectangle
import java.awt.RenderingHints
import java.awt.image.BufferedImage
import javax.swing.JPanel
import kotlin.math.abs

/**
 * High-DPI SHM video stage canvas:
 * - Renders `ShmFrameRingReader` frames aspect-preserved on a neutral dark IntUI backdrop
 * - Renders toggleable vector **Tap Ripple Overlay** for active taps within ±250ms of the frame
 * - Renders floating top-left glass-style **`近似プレビュー（位置確定前）`** pill ONLY while
 *   `meta.isApproximate == true` (mandatory `SPEC-03` honesty contract; clears on settled frame)
 */
class VideoViewportCanvas : JPanel(BorderLayout()) {
    private var currentImage: BufferedImage? = null
    private var currentMeta: ShmFrameMeta? = null
    private var activeTaps: List<ActiveTapOverlay> = emptyList()
    private var tapOverlayEnabled: Boolean = true
    private var approximateVisible: Boolean = false

    private val stageSurface = object : JPanel() {
        init {
            background = ArbTheme.stageBg
            isOpaque = true
            preferredSize = Dimension(JBUI.scale(520), JBUI.scale(340))
        }

        override fun paintComponent(g: Graphics) {
            super.paintComponent(g)
            val g2 = g.create() as Graphics2D
            try {
                g2.setRenderingHint(RenderingHints.KEY_ANTIALIASING, RenderingHints.VALUE_ANTIALIAS_ON)
                g2.setRenderingHint(RenderingHints.KEY_INTERPOLATION, RenderingHints.VALUE_INTERPOLATION_BILINEAR)
                g2.setRenderingHint(RenderingHints.KEY_TEXT_ANTIALIASING, RenderingHints.VALUE_TEXT_ANTIALIAS_ON)

                val img = currentImage
                val drawRect = if (img != null && img.width > 0 && img.height > 0) {
                    val pad = JBUI.scale(8)
                    val availW = (width - pad * 2).coerceAtLeast(1)
                    val availH = (height - pad * 2).coerceAtLeast(1)
                    val scale = minOf(availW.toDouble() / img.width.toDouble(), availH.toDouble() / img.height.toDouble())
                    val drawW = (img.width * scale).toInt().coerceAtLeast(1)
                    val drawH = (img.height * scale).toInt().coerceAtLeast(1)
                    val drawX = (width - drawW) / 2
                    val drawY = (height - drawH) / 2
                    g2.drawImage(img, drawX, drawY, drawW, drawH, null)
                    g2.color = Color(255, 255, 255, 22)
                    g2.drawRect(drawX, drawY, drawW - 1, drawH - 1)
                    Rectangle(drawX, drawY, drawW, drawH)
                } else {
                    g2.font = ArbTheme.uiFont(12f)
                    g2.color = ArbTheme.secondaryText
                    val msg = "フレーム読み込み待機中"
                    val fm = g2.fontMetrics
                    g2.drawString(msg, (width - fm.stringWidth(msg)) / 2, height / 2)
                    null
                }

                if (tapOverlayEnabled && drawRect != null && activeTaps.isNotEmpty()) {
                    paintTapRipples(g2, drawRect, img!!.width, img.height)
                }

                if (approximateVisible) {
                    paintApproximatePill(g2)
                }
            } finally {
                g2.dispose()
            }
        }
    }

    init {
        background = ArbTheme.stageBg
        isOpaque = true
        add(stageSurface, BorderLayout.CENTER)
    }

    fun updateFrame(
        meta: ShmFrameMeta?,
        image: BufferedImage?,
        taps: List<ActiveTapOverlay> = emptyList(),
        showTaps: Boolean = tapOverlayEnabled,
    ) {
        currentMeta = meta
        if (image != null) {
            currentImage = image
        }
        activeTaps = taps
        tapOverlayEnabled = showTaps
        approximateVisible = meta?.isApproximate == true
        stageSurface.repaint()
    }

    fun setTapOverlay(taps: List<ActiveTapOverlay>, enabled: Boolean) {
        activeTaps = taps
        tapOverlayEnabled = enabled
        stageSurface.repaint()
    }

    fun setApproximatePreview(isApproximate: Boolean) {
        if (approximateVisible != isApproximate) {
            approximateVisible = isApproximate
            stageSurface.repaint()
        }
    }

    // ---- Testable Inspection Accessors ----

    fun isApproximateBadgeVisible(): Boolean = approximateVisible

    fun approximateBadgeText(): String = APPROXIMATE_PREVIEW_BADGE_TEXT

    fun isTapOverlayEnabled(): Boolean = tapOverlayEnabled

    fun renderedTapOverlayCount(): Int = if (tapOverlayEnabled) activeTaps.size else 0

    private fun paintTapRipples(
        g2: Graphics2D,
        drawRect: Rectangle,
        videoW: Int,
        videoH: Int,
    ) {
        val srcW = (currentMeta?.width ?: videoW).coerceAtLeast(1)
        val srcH = (currentMeta?.height ?: videoH).coerceAtLeast(1)

        for (tap in activeTaps) {
            // Coordinates may be in device pixel space or normalized [0..1]
            val normX = if (tap.x in 0.0..1.0 && tap.y in 0.0..1.0) tap.x else (tap.x / srcW.toDouble())
            val normY = if (tap.x in 0.0..1.0 && tap.y in 0.0..1.0) tap.y else (tap.y / srcH.toDouble())
            val cx = drawRect.x + (normX.coerceIn(0.0, 1.0) * drawRect.width).toInt()
            val cy = drawRect.y + (normY.coerceIn(0.0, 1.0) * drawRect.height).toInt()

            val proximity = (1.0 - (abs(tap.deltaMs).toDouble() / 250.0)).coerceIn(0.2, 1.0)
            val outerR = JBUI.scale((14 + (1.0 - proximity) * 14).toInt())
            val innerR = JBUI.scale(5)
            val alphaOuter = (proximity * 180).toInt().coerceIn(45, 200)
            val alphaFill = (proximity * 70).toInt().coerceIn(20, 90)

            // Soft cyan fill + outer crisp vector ring
            g2.color = Color(0x3C, 0xB4, 0xD6, alphaFill)
            g2.fillOval(cx - outerR, cy - outerR, outerR * 2, outerR * 2)

            g2.stroke = BasicStroke(1.75f)
            g2.color = Color(0x3C, 0xB4, 0xD6, alphaOuter)
            g2.drawOval(cx - outerR, cy - outerR, outerR * 2, outerR * 2)

            // Center dot
            g2.color = Color(0xFF, 0xFF, 0xFF, (proximity * 240).toInt().coerceIn(80, 245))
            g2.fillOval(cx - innerR, cy - innerR, innerR * 2, innerR * 2)
            g2.color = Color(0x0E, 0x86, 0xA8, 220)
            g2.stroke = BasicStroke(1.25f)
            g2.drawOval(cx - innerR, cy - innerR, innerR * 2, innerR * 2)

            // Compact index tag (#1)
            g2.font = ArbTheme.monoFont(10f, bold = true)
            val fm = g2.fontMetrics
            val tagW = fm.stringWidth(tap.label) + JBUI.scale(8)
            val tagH = fm.height + JBUI.scale(2)
            val tagX = (cx + outerR + JBUI.scale(4)).coerceAtMost(drawRect.x + drawRect.width - tagW - 2)
            val tagY = (cy - tagH / 2).coerceIn(drawRect.y + 2, drawRect.y + drawRect.height - tagH - 2)
            g2.color = Color(0x14, 0x16, 0x19, 210)
            g2.fillRoundRect(tagX, tagY, tagW, tagH, JBUI.scale(4), JBUI.scale(4))
            g2.color = Color(0x3C, 0xB4, 0xD6, 220)
            g2.stroke = BasicStroke(1f)
            g2.drawRoundRect(tagX, tagY, tagW, tagH, JBUI.scale(4), JBUI.scale(4))
            g2.color = Color(0xE8, 0xF6, 0xFA)
            g2.drawString(tap.label, tagX + JBUI.scale(4), tagY + fm.ascent + JBUI.scale(1))
        }
    }

    private fun paintApproximatePill(g2: Graphics2D) {
        val text = APPROXIMATE_PREVIEW_BADGE_TEXT
        g2.font = ArbTheme.uiFont(11f, bold = true)
        val fm = g2.fontMetrics
        val padH = JBUI.scale(10)
        val padV = JBUI.scale(5)
        val pillW = fm.stringWidth(text) + padH * 2
        val pillH = fm.height + padV * 2
        val x = JBUI.scale(12)
        val y = JBUI.scale(12)
        val arc = JBUI.scale(6)

        g2.color = Color(0x2A, 0x20, 0x12, 220)
        g2.fillRoundRect(x, y, pillW, pillH, arc, arc)
        g2.stroke = BasicStroke(1f)
        g2.color = Color(0xE0, 0x9F, 0x3E, 230)
        g2.drawRoundRect(x, y, pillW, pillH, arc, arc)
        g2.color = Color(0xFE, 0xF3, 0xC7)
        g2.drawString(text, x + padH, y + padV + fm.ascent)
    }
}
