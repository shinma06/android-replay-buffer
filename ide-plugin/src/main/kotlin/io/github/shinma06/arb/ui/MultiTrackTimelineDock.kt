package io.github.shinma06.arb.ui

import com.intellij.icons.AllIcons
import com.intellij.util.ui.JBUI
import io.github.shinma06.arb.model.ScreenSpan
import io.github.shinma06.arb.state.ArbFormatters
import io.github.shinma06.arb.state.SourceJumpParser
import io.github.shinma06.arb.state.SourceJumpTarget
import io.github.shinma06.arb.state.TimelineViewerState
import java.awt.BasicStroke
import java.awt.BorderLayout
import java.awt.Color
import java.awt.Cursor
import java.awt.Dimension
import java.awt.FlowLayout
import java.awt.Graphics
import java.awt.Graphics2D
import java.awt.Polygon
import java.awt.Rectangle
import java.awt.RenderingHints
import java.awt.event.MouseAdapter
import java.awt.event.MouseEvent
import javax.swing.BorderFactory
import javax.swing.JButton
import javax.swing.JCheckBox
import javax.swing.JPanel
import kotlin.math.abs
import kotlin.math.roundToLong

/**
 * Full-width bottom Multi-Track Timeline & Trim Dock:
 * - Time ruler + unified crisp vertical playhead across all tracks
 * - Track 1: Filmstrip band + darkened out-of-range masks + draggable dual In/Out trim handles
 *   (fires `onScrub` during drag -> `onSeekExact` on release!) + selection summary pill
 * - Track 2: Activity Screen Spans (`timeline.screens`), Tap pins, Crash markers, and hatched
 *   amber discontinuity gap blocks
 * - Bottom Control Strip: `[I] 開始点`, `[O] 終了点`, conditional `操作区間を選択` (taps > 0),
 *   conditional `クラッシュ前後を選択 (-5s/+2s)` (crashes > 0), `全選択に戻す`, and
 *   `書き出し後に元のバンドルをゴミ箱へ（復元可）` toggle (disabled for Legacy RO)
 */
class MultiTrackTimelineDock(
    private val onScrub: (frameIndex: Long) -> Unit,
    private val onSeekExact: (frameIndex: Long) -> Unit,
    private val onInFrameChanged: (frameIndex: Long, settled: Boolean) -> Unit,
    private val onOutFrameChanged: (frameIndex: Long, settled: Boolean) -> Unit,
    private val onSelectTapRange: () -> Unit,
    private val onSelectAroundCrash: () -> Unit,
    private val onResetFullRange: () -> Unit,
    private val onTrashOriginalToggled: (enabled: Boolean) -> Unit,
    private val onSourceJump: (SourceJumpTarget) -> Unit = {},
) : JPanel(BorderLayout()) {
    private var currentState: TimelineViewerState? = null
    private var suppressCheckboxEvent = false

    private val timelineCanvas = TimelineTracksCanvas()

    private val setInButton = ArbTheme.styleSecondaryButton(
        JButton("[I] 開始点"),
        compact = true,
    )
    private val setOutButton = ArbTheme.styleSecondaryButton(
        JButton("[O] 終了点"),
        compact = true,
    )
    private val selectTapRangeButton = ArbTheme.styleSecondaryButton(
        JButton(ArbFormatters.PRESET_TAP_RANGE_LABEL),
        compact = true,
    ).apply { isVisible = false }

    private val selectCrashRangeButton = ArbTheme.styleSecondaryButton(
        JButton(ArbFormatters.PRESET_CRASH_RANGE_LABEL, AllIcons.General.Error),
        compact = true,
    ).apply { isVisible = false }

    private val resetRangeButton = ArbTheme.styleSecondaryButton(
        JButton(ArbFormatters.PRESET_RESET_RANGE_LABEL),
        compact = true,
    )

    private val trashOriginalCheckBox = JCheckBox(ArbFormatters.TRASH_ORIGINAL_TOGGLE_LABEL).apply {
        isOpaque = false
        font = ArbTheme.uiFont(11f)
        foreground = ArbTheme.secondaryText
    }

    init {
        background = ArbTheme.timelineBg
        isOpaque = true
        border = ArbTheme.hairlineTopBorder(0, 0)

        val controlStrip = JPanel(BorderLayout(8, 0)).apply {
            background = ArbTheme.surfaceBg
            isOpaque = true
            border = ArbTheme.hairlineTopBorder(4, 10)

            val leftGroup = JPanel(FlowLayout(FlowLayout.LEFT, 4, 0)).apply {
                isOpaque = false
                add(setInButton)
                add(setOutButton)
                add(selectTapRangeButton)
                add(selectCrashRangeButton)
                add(resetRangeButton)
            }
            add(leftGroup, BorderLayout.WEST)
            add(trashOriginalCheckBox, BorderLayout.EAST)
        }

        add(timelineCanvas, BorderLayout.CENTER)
        add(controlStrip, BorderLayout.SOUTH)

        wireControls()
    }

    private fun wireControls() {
        setInButton.addActionListener {
            val s = currentState ?: return@addActionListener
            onInFrameChanged(s.currentFrameIndex, true)
        }
        setOutButton.addActionListener {
            val s = currentState ?: return@addActionListener
            onOutFrameChanged(s.currentFrameIndex, true)
        }
        selectTapRangeButton.addActionListener { onSelectTapRange() }
        selectCrashRangeButton.addActionListener { onSelectAroundCrash() }
        resetRangeButton.addActionListener { onResetFullRange() }
        trashOriginalCheckBox.addActionListener {
            if (!suppressCheckboxEvent) {
                onTrashOriginalToggled(trashOriginalCheckBox.isSelected)
            }
        }
    }

    fun render(state: TimelineViewerState) {
        currentState = state
        val hasVideo = state.frameCount > 0L

        setInButton.isEnabled = hasVideo
        setOutButton.isEnabled = hasVideo
        resetRangeButton.isEnabled = hasVideo

        // Deterministic presets strictly conditional on real recorded data!
        selectTapRangeButton.isVisible = state.canSelectTapRange
        selectCrashRangeButton.isVisible = state.canSelectAroundCrash

        suppressCheckboxEvent = true
        trashOriginalCheckBox.isEnabled = state.canTrashOriginalAfterExport
        trashOriginalCheckBox.isSelected = state.effectiveTrashOriginalAfterExport
        trashOriginalCheckBox.toolTipText = if (state.isLegacy) {
            "Legacy (RO) バンドルは読取専用のためゴミ箱へ移動できません"
        } else {
            "書き出しの検証完了後に元のバンドルを非破壊でゴミ箱へ移動します"
        }
        suppressCheckboxEvent = false

        timelineCanvas.repaint()
    }

    // ---- Testable Inspection Accessors ----

    fun isSelectTapRangeVisible(): Boolean = selectTapRangeButton.isVisible

    fun isSelectAroundCrashVisible(): Boolean = selectCrashRangeButton.isVisible

    fun isTrashOriginalEnabled(): Boolean = trashOriginalCheckBox.isEnabled

    fun isTrashOriginalSelected(): Boolean = trashOriginalCheckBox.isSelected

    fun clickSelectTapRange() = selectTapRangeButton.doClick()

    fun clickSelectAroundCrash() = selectCrashRangeButton.doClick()

    fun clickResetFullRange() = resetRangeButton.doClick()

    fun selectionSummaryText(): String = currentState?.selectionSummaryText.orEmpty()

    fun renderedScreenSpanCount(): Int = currentState?.screens?.size ?: 0

    fun renderedCrashMarkerCount(): Int = currentState?.crashes?.size ?: 0

    private enum class DragMode {
        NONE,
        PLAYHEAD,
        IN_HANDLE,
        OUT_HANDLE,
    }

    private inner class TimelineTracksCanvas : JPanel() {
        private val padLeft = JBUI.scale(14)
        private val padRight = JBUI.scale(14)
        private val rulerHeight = JBUI.scale(22)
        private val filmstripTop = rulerHeight + JBUI.scale(4)
        private val filmstripHeight = JBUI.scale(48)
        private val markerTop = filmstripTop + filmstripHeight + JBUI.scale(6)
        private val markerHeight = JBUI.scale(38)
        private val handleHitRadius = JBUI.scale(9)

        private var dragMode: DragMode = DragMode.NONE
        private var lastDragFrame: Long = -1L
        private val screenHitBoxes = mutableListOf<Pair<Rectangle, ScreenSpan>>()
        private val markerPoints = mutableListOf<Pair<Int, Long>>() // x -> frameIndex

        init {
            background = ArbTheme.timelineBg
            isOpaque = true
            preferredSize = Dimension(JBUI.scale(640), markerTop + markerHeight + JBUI.scale(10))
            minimumSize = Dimension(JBUI.scale(240), markerTop + markerHeight + JBUI.scale(10))

            val mouseHandler = object : MouseAdapter() {
                override fun mousePressed(e: MouseEvent) {
                    val s = currentState ?: return
                    if (s.frameCount <= 0L) return

                    // Check Screen Span hit in Track 2
                    if (e.y in markerTop..(markerTop + markerHeight)) {
                        for ((rect, span) in screenHitBoxes) {
                            if (rect.contains(e.point)) {
                                if (e.clickCount >= 2 || e.isShiftDown) {
                                    onSourceJump(SourceJumpParser.fromScreenSpan(span))
                                    return
                                }
                                val target = span.startFrameIndex ?: s.boottimeNsToFrameIndex(span.startNs)
                                if (target != null) {
                                    onSeekExact(target)
                                    return
                                }
                            }
                        }
                        // Check tap/crash marker pin snap within 6px
                        val hitPin = markerPoints.minByOrNull { abs(it.first - e.x) }
                        if (hitPin != null && abs(hitPin.first - e.x) <= JBUI.scale(7)) {
                            onSeekExact(hitPin.second)
                            return
                        }
                    }

                    val inX = frameToX(s.inFrameIndex, s.frameCount)
                    val outX = frameToX(s.outFrameIndex, s.frameCount)

                    dragMode = when {
                        e.y in (filmstripTop - 4)..(filmstripTop + filmstripHeight + 4) &&
                            abs(e.x - inX) <= handleHitRadius -> DragMode.IN_HANDLE
                        e.y in (filmstripTop - 4)..(filmstripTop + filmstripHeight + 4) &&
                            abs(e.x - outX) <= handleHitRadius -> DragMode.OUT_HANDLE
                        else -> DragMode.PLAYHEAD
                    }
                    handleDragAt(e.x, settled = false)
                }

                override fun mouseDragged(e: MouseEvent) {
                    if (dragMode != DragMode.NONE) {
                        handleDragAt(e.x, settled = false)
                    }
                }

                override fun mouseReleased(e: MouseEvent) {
                    if (dragMode != DragMode.NONE) {
                        handleDragAt(e.x, settled = true)
                        dragMode = DragMode.NONE
                        lastDragFrame = -1L
                    }
                }

                override fun mouseMoved(e: MouseEvent) {
                    val s = currentState ?: return
                    if (s.frameCount <= 0L) {
                        cursor = Cursor.getDefaultCursor()
                        return
                    }
                    val inX = frameToX(s.inFrameIndex, s.frameCount)
                    val outX = frameToX(s.outFrameIndex, s.frameCount)
                    cursor = when {
                        e.y in filmstripTop..(filmstripTop + filmstripHeight) &&
                            (abs(e.x - inX) <= handleHitRadius || abs(e.x - outX) <= handleHitRadius) ->
                            Cursor.getPredefinedCursor(Cursor.E_RESIZE_CURSOR)
                        e.y in markerTop..(markerTop + markerHeight) &&
                            screenHitBoxes.any { it.first.contains(e.point) } ->
                            Cursor.getPredefinedCursor(Cursor.HAND_CURSOR)
                        else -> Cursor.getDefaultCursor()
                    }
                }
            }
            addMouseListener(mouseHandler)
            addMouseMotionListener(mouseHandler)
        }

        private fun handleDragAt(x: Int, settled: Boolean) {
            val s = currentState ?: return
            if (s.frameCount <= 0L) return
            val target = xToFrame(x, s.frameCount)
            if (!settled && target == lastDragFrame) return
            lastDragFrame = target

            when (dragMode) {
                DragMode.IN_HANDLE -> {
                    onInFrameChanged(target, settled)
                    if (settled) onSeekExact(target) else onScrub(target)
                }
                DragMode.OUT_HANDLE -> {
                    onOutFrameChanged(target, settled)
                    if (settled) onSeekExact(target) else onScrub(target)
                }
                DragMode.PLAYHEAD -> {
                    if (settled) onSeekExact(target) else onScrub(target)
                }
                DragMode.NONE -> Unit
            }
        }

        private fun trackWidth(): Int = (width - padLeft - padRight).coerceAtLeast(1)

        private fun frameToX(frameIndex: Long, totalFrames: Long): Int {
            if (totalFrames <= 1L) return padLeft
            val ratio = frameIndex.coerceIn(0L, totalFrames - 1L).toDouble() / (totalFrames - 1L).toDouble()
            return padLeft + (ratio * trackWidth()).toInt()
        }

        private fun xToFrame(x: Int, totalFrames: Long): Long {
            if (totalFrames <= 1L) return 0L
            val ratio = ((x - padLeft).toDouble() / trackWidth().toDouble()).coerceIn(0.0, 1.0)
            return (ratio * (totalFrames - 1L).toDouble()).roundToLong().coerceIn(0L, totalFrames - 1L)
        }

        override fun paintComponent(g: Graphics) {
            super.paintComponent(g)
            val g2 = g.create() as Graphics2D
            try {
                g2.setRenderingHint(RenderingHints.KEY_ANTIALIASING, RenderingHints.VALUE_ANTIALIAS_ON)
                g2.setRenderingHint(RenderingHints.KEY_TEXT_ANTIALIASING, RenderingHints.VALUE_TEXT_ANTIALIAS_ON)

                screenHitBoxes.clear()
                markerPoints.clear()

                val s = currentState
                val tw = trackWidth()

                // 1. Time ruler
                paintRuler(g2, s, tw)

                // 2. Track 1 — Filmstrip + Dual In/Out Trim Handles
                paintFilmstripTrack(g2, s, tw)

                // 3. Track 2 — Activity Screen Spans + Tap / Crash / Discontinuity Lane
                paintMarkerTrack(g2, s, tw)

                // 4. Unified Full-Height Playhead
                if (s != null && s.frameCount > 0L) {
                    val px = frameToX(s.currentFrameIndex, s.frameCount)
                    g2.color = ArbTheme.primaryText
                    g2.stroke = BasicStroke(1.75f)
                    g2.drawLine(px, rulerHeight - JBUI.scale(4), px, markerTop + markerHeight + JBUI.scale(2))

                    // Crisp playhead diamond/cap on ruler
                    val cap = Polygon(
                        intArrayOf(px - JBUI.scale(5), px + JBUI.scale(5), px),
                        intArrayOf(rulerHeight - JBUI.scale(10), rulerHeight - JBUI.scale(10), rulerHeight - JBUI.scale(2)),
                        3,
                    )
                    g2.fillPolygon(cap)
                }
            } finally {
                g2.dispose()
            }
        }

        private fun paintRuler(g2: Graphics2D, s: TimelineViewerState?, tw: Int) {
            g2.color = ArbTheme.hairlineBorder
            g2.drawLine(padLeft, rulerHeight, padLeft + tw, rulerHeight)
            if (s == null || s.frameCount <= 0L) return

            g2.font = ArbTheme.monoFont(10f)
            val tickCount = (tw / JBUI.scale(90)).coerceIn(3, 12)
            for (i in 0..tickCount) {
                val ratio = i.toDouble() / tickCount.toDouble()
                val tx = padLeft + (ratio * tw).toInt()
                g2.color = ArbTheme.hairlineBorder
                g2.drawLine(tx, rulerHeight - JBUI.scale(5), tx, rulerHeight)
                val ms = (ratio * s.totalDurationMs.toDouble()).roundToLong()
                val label = ArbFormatters.formatDurationSeconds(ms)
                g2.color = ArbTheme.secondaryText
                val labelX = (tx - g2.fontMetrics.stringWidth(label) / 2).coerceIn(padLeft, padLeft + tw - g2.fontMetrics.stringWidth(label))
                g2.drawString(label, labelX, rulerHeight - JBUI.scale(7))
            }
        }

        private fun paintFilmstripTrack(g2: Graphics2D, s: TimelineViewerState?, tw: Int) {
            val arc = JBUI.scale(4)
            g2.color = ArbTheme.timelineTrackBg
            g2.fillRoundRect(padLeft, filmstripTop, tw, filmstripHeight, arc, arc)
            g2.color = ArbTheme.hairlineBorder
            g2.drawRoundRect(padLeft, filmstripTop, tw, filmstripHeight, arc, arc)

            if (s == null || s.frameCount <= 0L) return

            // Subtle filmstrip keyframe slots
            val slotCount = s.filmstrip.size.coerceIn(6, 20)
            val cellW = tw.toDouble() / slotCount.toDouble()
            for (i in 0 until slotCount) {
                val cx = padLeft + (i * cellW).toInt()
                val cw = (cellW - 1).toInt().coerceAtLeast(1)
                g2.color = if (i % 2 == 0) Color(255, 255, 255, 10) else Color(0, 0, 0, 12)
                g2.fillRect(cx + 1, filmstripTop + 1, cw, filmstripHeight - 2)
                if (i > 0) {
                    g2.color = ArbTheme.subtleBorder
                    g2.drawLine(cx, filmstripTop + 2, cx, filmstripTop + filmstripHeight - 2)
                }
            }

            // Keyframe tick markers from real filmstrip metadata
            for (kf in s.filmstrip) {
                val kx = frameToX(kf.frameIndex, s.frameCount)
                g2.color = ArbTheme.dimmedText
                g2.drawLine(kx, filmstripTop + filmstripHeight - JBUI.scale(6), kx, filmstripTop + filmstripHeight - 1)
            }

            val inX = frameToX(s.inFrameIndex, s.frameCount)
            val outX = frameToX(s.outFrameIndex, s.frameCount)

            // Darkened out-of-range masks
            g2.color = Color(0, 0, 0, 115)
            if (inX > padLeft) {
                g2.fillRect(padLeft, filmstripTop, inX - padLeft, filmstripHeight)
            }
            if (outX < padLeft + tw) {
                g2.fillRect(outX, filmstripTop, (padLeft + tw) - outX, filmstripHeight)
            }

            // Crisp IntUI accent trim selection frame
            val selW = (outX - inX).coerceAtLeast(2)
            g2.stroke = BasicStroke(2f)
            g2.color = ArbTheme.accentBlue
            g2.drawRect(inX, filmstripTop, selW, filmstripHeight)

            // Left & right draggable trim handles
            val handleW = JBUI.scale(8)
            g2.fillRoundRect(inX - handleW / 2, filmstripTop - 1, handleW, filmstripHeight + 2, JBUI.scale(3), JBUI.scale(3))
            g2.fillRoundRect(outX - handleW / 2, filmstripTop - 1, handleW, filmstripHeight + 2, JBUI.scale(3), JBUI.scale(3))

            // Subtle white grip lines on handles
            g2.stroke = BasicStroke(1f)
            g2.color = Color.WHITE
            val gripY1 = filmstripTop + filmstripHeight / 3
            val gripY2 = filmstripTop + (filmstripHeight * 2) / 3
            g2.drawLine(inX, gripY1, inX, gripY2)
            g2.drawLine(outX, gripY1, outX, gripY2)

            // Floating selection summary pill centered above selection
            val summaryText = s.selectionSummaryText
            g2.font = ArbTheme.monoFont(10.5f, bold = true)
            val fm = g2.fontMetrics
            val pillW = fm.stringWidth(summaryText) + JBUI.scale(14)
            val pillH = fm.height + JBUI.scale(4)
            val pillX = ((inX + outX) / 2 - pillW / 2).coerceIn(padLeft + 2, (padLeft + tw - pillW - 2).coerceAtLeast(padLeft + 2))
            val pillY = filmstripTop + JBUI.scale(5)
            g2.color = Color(0x14, 0x16, 0x19, 215)
            g2.fillRoundRect(pillX, pillY, pillW, pillH, JBUI.scale(4), JBUI.scale(4))
            g2.color = ArbTheme.accentBlue
            g2.drawRoundRect(pillX, pillY, pillW, pillH, JBUI.scale(4), JBUI.scale(4))
            g2.color = Color(0xEC, 0xF2, 0xFF)
            g2.drawString(summaryText, pillX + JBUI.scale(7), pillY + fm.ascent + JBUI.scale(2))
        }

        private fun paintMarkerTrack(g2: Graphics2D, s: TimelineViewerState?, tw: Int) {
            val arc = JBUI.scale(4)
            g2.color = ArbTheme.timelineTrackBg
            g2.fillRoundRect(padLeft, markerTop, tw, markerHeight, arc, arc)
            g2.color = ArbTheme.hairlineBorder
            g2.drawRoundRect(padLeft, markerTop, tw, markerHeight, arc, arc)

            if (s == null || s.frameCount <= 0L) return

            val screenBandH = JBUI.scale(18)
            val pinLaneY = markerTop + screenBandH + JBUI.scale(10)

            // 1. Activity Screen Spans (upper half of Track 2)
            for ((idx, span) in s.screens.withIndex()) {
                val sf = span.startFrameIndex ?: s.boottimeNsToFrameIndex(span.startNs) ?: continue
                val ef = span.endFrameIndex ?: s.boottimeNsToFrameIndex(span.endNs) ?: sf
                val x1 = frameToX(sf, s.frameCount)
                val x2 = frameToX(ef, s.frameCount).coerceAtLeast(x1 + JBUI.scale(8))
                val w = (x2 - x1).coerceAtMost(padLeft + tw - x1)
                val rect = Rectangle(x1, markerTop + 1, w, screenBandH)
                screenHitBoxes.add(rect to span)

                g2.color = ArbTheme.screenSpanPalette[idx % ArbTheme.screenSpanPalette.size]
                g2.fillRect(rect.x, rect.y, rect.width, rect.height)
                g2.color = ArbTheme.hairlineBorder
                g2.drawRect(rect.x, rect.y, rect.width, rect.height)

                if (rect.width >= JBUI.scale(28)) {
                    g2.font = ArbTheme.uiFont(10f, bold = false)
                    g2.color = ArbTheme.primaryText
                    val fm = g2.fontMetrics
                    val clipped = clipText(span.shortName, fm, rect.width - JBUI.scale(8))
                    g2.drawString(clipped, rect.x + JBUI.scale(4), rect.y + fm.ascent + JBUI.scale(2))
                }
            }

            // 2. Discontinuity / gap blocks (hatched amber across marker lane)
            for (disc in s.discontinuities) {
                val startBt = disc.startBoottimeNs ?: continue
                val endBt = disc.endBoottimeNs ?: startBt
                val sf = s.boottimeNsToFrameIndex(startBt) ?: continue
                val ef = s.boottimeNsToFrameIndex(endBt) ?: sf
                val x1 = frameToX(sf, s.frameCount)
                val x2 = frameToX(ef, s.frameCount).coerceAtLeast(x1 + JBUI.scale(6))
                val bw = (x2 - x1).coerceAtMost(padLeft + tw - x1)
                g2.color = Color(0xE0, 0x9F, 0x3E, 55)
                g2.fillRect(x1, markerTop + 1, bw, markerHeight - 2)
                g2.color = ArbTheme.warningAmber
                val step = JBUI.scale(5)
                var sx = x1
                while (sx < x1 + bw) {
                    g2.drawLine(sx, markerTop + markerHeight - 2, (sx + step).coerceAtMost(x1 + bw), markerTop + 2)
                    sx += step
                }
            }

            // 3. Tap pins (cyan circles in lower half of Track 2)
            val tapR = JBUI.scale(4)
            for (tap in s.mainTaps) {
                val bt = TimelineViewerState.alignmentBoottimeNs(tap.alignment) ?: continue
                val f = s.boottimeNsToFrameIndex(bt) ?: continue
                val tx = frameToX(f, s.frameCount)
                markerPoints.add(tx to f)
                g2.color = ArbTheme.infoCyan
                g2.fillOval(tx - tapR, pinLaneY - tapR, tapR * 2, tapR * 2)
                g2.color = Color.WHITE
                g2.stroke = BasicStroke(1f)
                g2.drawOval(tx - tapR, pinLaneY - tapR, tapR * 2, tapR * 2)
            }

            // 4. Crash markers (semantic red diamond + subtle vertical dashed line)
            val crashR = JBUI.scale(5)
            val dashed = BasicStroke(1.2f, BasicStroke.CAP_BUTT, BasicStroke.JOIN_MITER, 4f, floatArrayOf(3f, 3f), 0f)
            for (crash in s.crashes) {
                val f = crash.frameIndexHint ?: crash.boottimeNs?.let(s::boottimeNsToFrameIndex) ?: continue
                val cx = frameToX(f, s.frameCount)
                markerPoints.add(cx to f)
                g2.color = ArbTheme.recRed
                g2.stroke = dashed
                g2.drawLine(cx, filmstripTop, cx, markerTop + markerHeight)
                g2.stroke = BasicStroke(1f)
                val diamond = Polygon(
                    intArrayOf(cx, cx + crashR, cx, cx - crashR),
                    intArrayOf(pinLaneY - crashR, pinLaneY, pinLaneY + crashR, pinLaneY),
                    4,
                )
                g2.fillPolygon(diamond)
            }
        }

        private fun clipText(text: String, fm: java.awt.FontMetrics, maxWidth: Int): String {
            if (fm.stringWidth(text) <= maxWidth) return text
            var end = text.length
            while (end > 1 && fm.stringWidth(text.substring(0, end) + "..") > maxWidth) {
                end--
            }
            return text.substring(0, end) + ".."
        }
    }
}
