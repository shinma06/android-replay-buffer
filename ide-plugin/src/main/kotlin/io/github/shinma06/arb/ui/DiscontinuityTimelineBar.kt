package io.github.shinma06.arb.ui

import io.github.shinma06.arb.model.DiscontinuityEvent
import io.github.shinma06.arb.model.DiscontinuityKind
import io.github.shinma06.arb.state.DiscontinuityFormatter
import java.awt.Color
import java.awt.Dimension
import java.awt.FlowLayout
import java.awt.Font
import javax.swing.BorderFactory
import javax.swing.JLabel
import javax.swing.JPanel

/**
 * Visualizes timeline discontinuities (`DEVICE_REBOOT`, `DEVICE_SLEEP`, `PC_SLEEP`,
 * `CONNECTION_LOST`, `STREAM_GAP`, `STATIC_HOLD`, `STORAGE_PRESSURE_STOP`, etc.).
 */
class DiscontinuityTimelineBar : JPanel(FlowLayout(FlowLayout.LEFT, 6, 4)) {
    private var events: List<DiscontinuityEvent> = emptyList()

    init {
        border = BorderFactory.createTitledBorder("不連続・欠損イベント (Discontinuities)")
        preferredSize = Dimension(480, 56)
        render(emptyList())
    }

    fun render(discontinuities: List<DiscontinuityEvent>) {
        events = discontinuities
        removeAll()
        if (discontinuities.isEmpty()) {
            add(JLabel("不連続・欠損イベントなし"))
        } else {
            for (ev in discontinuities) {
                val chip = JLabel(DiscontinuityFormatter.label(ev.kind)).apply {
                    isOpaque = true
                    background = colorFor(ev.kind)
                    foreground = Color.WHITE
                    font = font.deriveFont(Font.BOLD, 11f)
                    border = BorderFactory.createEmptyBorder(2, 6, 2, 6)
                    toolTipText = DiscontinuityFormatter.describe(ev)
                }
                add(chip)
            }
        }
        revalidate()
        repaint()
    }

    fun renderedEventCount(): Int = events.size

    private fun colorFor(kind: DiscontinuityKind): Color = when (kind) {
        DiscontinuityKind.DEVICE_REBOOT -> Color(0x99, 0x1B, 0x1B)
        DiscontinuityKind.STORAGE_PRESSURE_STOP -> Color(0x7F, 0x1D, 0x1D)
        DiscontinuityKind.CONNECTION_LOST -> Color(0xC2, 0x41, 0x0C)
        DiscontinuityKind.STREAM_GAP -> Color(0xB4, 0x53, 0x09)
        DiscontinuityKind.DEVICE_SLEEP, DiscontinuityKind.PC_SLEEP -> Color(0x43, 0x38, 0xCA)
        DiscontinuityKind.DEVICE_WALL_JUMP, DiscontinuityKind.CLOCK_SYNC_STALE -> Color(0x7E, 0x22, 0xCE)
        DiscontinuityKind.STATIC_HOLD -> Color(0x0F, 0x76, 0x6E)
    }
}
