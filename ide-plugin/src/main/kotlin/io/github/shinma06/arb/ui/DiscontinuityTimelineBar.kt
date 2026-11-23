package io.github.shinma06.arb.ui

import com.intellij.util.ui.JBUI
import io.github.shinma06.arb.model.DiscontinuityEvent
import io.github.shinma06.arb.model.DiscontinuityKind
import io.github.shinma06.arb.state.DiscontinuityFormatter
import java.awt.Color
import java.awt.Dimension
import java.awt.FlowLayout
import javax.swing.BorderFactory
import javax.swing.JLabel
import javax.swing.JPanel

/**
 * Tier-3 detail visualization of timeline discontinuities (`DEVICE_REBOOT`, `DEVICE_SLEEP`,
 * `PC_SLEEP`, `CONNECTION_LOST`, `STREAM_GAP`, `STATIC_HOLD`, `STORAGE_PRESSURE_STOP`, etc.).
 */
class DiscontinuityTimelineBar : JPanel(FlowLayout(FlowLayout.LEFT, 4, 4)) {
    private var events: List<DiscontinuityEvent> = emptyList()

    init {
        background = ArbTheme.cardBg
        isOpaque = true
        border = ArbTheme.cardBorder(6, 8)
        preferredSize = Dimension(JBUI.scale(420), JBUI.scale(44))
        render(emptyList())
    }

    fun render(discontinuities: List<DiscontinuityEvent>) {
        events = discontinuities
        removeAll()
        if (discontinuities.isEmpty()) {
            add(
                JLabel("不連続・欠損イベントなし").apply {
                    font = ArbTheme.uiFont(11f)
                    foreground = ArbTheme.secondaryText
                },
            )
        } else {
            for (ev in discontinuities) {
                val (bg, fg) = colorsFor(ev.kind)
                val chip = JLabel(DiscontinuityFormatter.label(ev.kind)).apply {
                    isOpaque = true
                    background = bg
                    foreground = fg
                    font = ArbTheme.monoFont(10f, bold = true)
                    border = BorderFactory.createCompoundBorder(
                        BorderFactory.createLineBorder(fg, 1),
                        JBUI.Borders.empty(2, 6),
                    )
                    toolTipText = DiscontinuityFormatter.describe(ev)
                }
                add(chip)
            }
        }
        revalidate()
        repaint()
    }

    fun renderedEventCount(): Int = events.size

    private fun colorsFor(kind: DiscontinuityKind): Pair<Color, Color> = when (kind) {
        DiscontinuityKind.DEVICE_REBOOT,
        DiscontinuityKind.STORAGE_PRESSURE_STOP,
        -> ArbTheme.recRedSoft to ArbTheme.recRed
        DiscontinuityKind.CONNECTION_LOST,
        DiscontinuityKind.STREAM_GAP,
        DiscontinuityKind.DEVICE_WALL_JUMP,
        DiscontinuityKind.CLOCK_SYNC_STALE,
        -> ArbTheme.warningAmberSoft to ArbTheme.warningAmber
        DiscontinuityKind.DEVICE_SLEEP,
        DiscontinuityKind.PC_SLEEP,
        DiscontinuityKind.STATIC_HOLD,
        -> ArbTheme.infoCyanSoft to ArbTheme.infoCyan
    }
}
