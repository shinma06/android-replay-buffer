package io.github.shinma06.arb.ui

import com.intellij.icons.AllIcons
import com.intellij.util.ui.JBUI
import io.github.shinma06.arb.model.IdeDefaultSaveAction
import io.github.shinma06.arb.state.ArbFormatters
import io.github.shinma06.arb.state.CaptureBanner
import io.github.shinma06.arb.state.CaptureBannerKind
import io.github.shinma06.arb.state.CaptureBarState
import java.awt.BorderLayout
import java.awt.Color
import java.awt.Dimension
import java.awt.FlowLayout
import java.awt.Graphics
import java.awt.Graphics2D
import java.awt.GridLayout
import java.awt.RenderingHints
import javax.swing.BorderFactory
import javax.swing.Box
import javax.swing.BoxLayout
import javax.swing.JButton
import javax.swing.JComboBox
import javax.swing.JLabel
import javax.swing.JPanel

/**
 * Redesigned Live Capture Cockpit Card (IntUI native, zero emoji chrome, strict seconds notation):
 * - Row 1: Device selector + status pill (`REC` / `待機中` / `再接続中`) + Settings gear button
 * - Row 2: Slim ring fill progress bar + tabular seconds readout (`272s / 300s · 420 MB`)
 * - Row 3: Full-width accent primary CTA (`直前 {N}s を保存して開く`, swappable with `保存のみ` via config)
 * - Row 4: Compact 4-action utility strip (`保存のみ` / `スクショ` / `ログのみ` / `停止|開始`)
 * - Conditional Tier-2 status banners (height 0 when healthy!)
 */
class CaptureBarPanel(
    private val onStartCapture: (serial: String?) -> Unit,
    private val onStopCapture: () -> Unit,
    private val onSaveNow: (durationSec: Long) -> Unit,
    private val onRefresh: () -> Unit,
    private val onSaveAndOpen: (durationSec: Long) -> Unit = onSaveNow,
    private val onSaveOnly: (durationSec: Long) -> Unit = onSaveNow,
    private val onScreenshot: (serial: String?) -> Unit = {},
    private val onSaveLogs: (durationSec: Long) -> Unit = {},
    private val onOpenConfig: () -> Unit = {},
) : JPanel(BorderLayout(0, 6)) {
    private val deviceCombo = JComboBox<String>().apply {
        font = ArbTheme.uiFont(11.5f)
    }

    private val statusPill = StatusPillLabel()

    private val settingsButton = ArbTheme.styleSecondaryButton(
        JButton(AllIcons.General.GearPlain).apply {
            toolTipText = "ARB 設定（保存秒数・主ボタン・クラッシュ自動保存）"
        },
        compact = true,
    )

    private val ringProgressBar = RingFillBar()

    private val ringMeterLabel = JLabel("0s / 300s · 0.0 MB").apply {
        font = ArbTheme.monoFont(11f)
        foreground = ArbTheme.secondaryText
    }

    private val audioIndicator = JLabel("音声 ON").apply {
        font = ArbTheme.uiFont(10.5f)
        foreground = ArbTheme.secondaryText
    }

    private val primarySaveButton = ArbTheme.stylePrimaryButton(
        JButton("直前 300s を保存して開く", AllIcons.Actions.MenuSaveall),
    )

    private val secondarySaveButton = ArbTheme.styleSecondaryButton(
        JButton("保存のみ", AllIcons.Actions.Download),
        compact = true,
    )

    private val screenshotButton = ArbTheme.styleSecondaryButton(
        JButton(ArbFormatters.SECONDARY_SCREENSHOT_LABEL, AllIcons.Actions.Dump),
        compact = true,
    )

    private val logsOnlyButton = ArbTheme.styleSecondaryButton(
        JButton(ArbFormatters.SECONDARY_LOGS_ONLY_LABEL, AllIcons.FileTypes.Text),
        compact = true,
    )

    private val startStopButton = ArbTheme.styleSecondaryButton(
        JButton(ArbFormatters.SECONDARY_START_CAPTURE_LABEL, AllIcons.Actions.Execute),
        compact = true,
    )

    private val bannerContainer = JPanel().apply {
        layout = BoxLayout(this, BoxLayout.Y_AXIS)
        isOpaque = false
        isVisible = false
    }

    private var currentState = CaptureBarState()

    init {
        isOpaque = false
        border = JBUI.Borders.empty(6, 6, 2, 6)

        val card = JPanel().apply {
            layout = BoxLayout(this, BoxLayout.Y_AXIS)
            background = ArbTheme.cardBg
            isOpaque = true
            border = ArbTheme.cardBorder(8, 10)
        }

        // Row 1: Device dropdown + status pill + settings gear
        val headerRow = JPanel(BorderLayout(6, 0)).apply {
            isOpaque = false
            maximumSize = Dimension(Int.MAX_VALUE, JBUI.scale(28))
            add(deviceCombo, BorderLayout.CENTER)
            val rightBox = JPanel(FlowLayout(FlowLayout.RIGHT, 4, 0)).apply {
                isOpaque = false
                add(statusPill)
                add(settingsButton)
            }
            add(rightBox, BorderLayout.EAST)
        }

        // Row 2: Slim ring meter bar + tabular seconds readout
        val ringMetaRow = JPanel(BorderLayout(6, 0)).apply {
            isOpaque = false
            maximumSize = Dimension(Int.MAX_VALUE, JBUI.scale(18))
            add(ringMeterLabel, BorderLayout.WEST)
            add(audioIndicator, BorderLayout.EAST)
        }

        // Row 3: Primary full-width CTA
        val primaryRow = JPanel(BorderLayout()).apply {
            isOpaque = false
            maximumSize = Dimension(Int.MAX_VALUE, JBUI.scale(34))
            add(primarySaveButton, BorderLayout.CENTER)
        }

        // Row 4: Compact 4-action utility strip
        val secondaryRow = JPanel(GridLayout(1, 4, JBUI.scale(4), 0)).apply {
            isOpaque = false
            maximumSize = Dimension(Int.MAX_VALUE, JBUI.scale(28))
            add(secondarySaveButton)
            add(screenshotButton)
            add(logsOnlyButton)
            add(startStopButton)
        }

        card.add(headerRow)
        card.add(Box.createVerticalStrut(JBUI.scale(6)))
        card.add(ringProgressBar)
        card.add(Box.createVerticalStrut(JBUI.scale(4)))
        card.add(ringMetaRow)
        card.add(Box.createVerticalStrut(JBUI.scale(8)))
        card.add(primaryRow)
        card.add(Box.createVerticalStrut(JBUI.scale(6)))
        card.add(secondaryRow)

        add(card, BorderLayout.NORTH)
        add(bannerContainer, BorderLayout.CENTER)

        wireActions()
    }

    private fun wireActions() {
        settingsButton.addActionListener { onOpenConfig() }

        primarySaveButton.addActionListener {
            val sec = currentState.saveWindowSecs.toLong()
            when (currentState.defaultSaveAction) {
                IdeDefaultSaveAction.SAVE_AND_OPEN -> onSaveAndOpen(sec)
                IdeDefaultSaveAction.SAVE_ONLY -> onSaveOnly(sec)
            }
        }

        secondarySaveButton.addActionListener {
            val sec = currentState.saveWindowSecs.toLong()
            when (currentState.defaultSaveAction) {
                IdeDefaultSaveAction.SAVE_AND_OPEN -> onSaveOnly(sec)
                IdeDefaultSaveAction.SAVE_ONLY -> onSaveAndOpen(sec)
            }
        }

        screenshotButton.addActionListener {
            onScreenshot(selectedDeviceSerial())
        }

        logsOnlyButton.addActionListener {
            onSaveLogs(currentState.saveWindowSecs.toLong())
        }

        startStopButton.addActionListener {
            if (currentState.capture?.active == true) {
                onStopCapture()
            } else {
                onStartCapture(selectedDeviceSerial())
            }
        }
    }

    private fun selectedDeviceSerial(): String? =
        (deviceCombo.selectedItem as? String)?.takeIf { it.isNotBlank() }

    fun render(state: CaptureBarState) {
        currentState = state
        val cap = state.capture

        statusPill.update(
            text = state.statusBadgeText,
            isRecording = state.isRecordingHealthy,
            isWarning = cap?.storagePressureStopped == true || (cap?.active == true && !cap.connected),
        )

        ringProgressBar.update(state.ringFillRatio, state.isRecordingHealthy)
        ringMeterLabel.text = state.ringMeterText
        audioIndicator.text = if (state.config.audioEnabled) "音声 ON" else "音声 OFF"
        audioIndicator.foreground = if (state.config.audioEnabled) ArbTheme.secondaryText else ArbTheme.dimmedText

        primarySaveButton.text = state.primarySaveLabel
        secondarySaveButton.text = state.secondarySaveLabel

        if (cap?.active == true) {
            startStopButton.text = ArbFormatters.SECONDARY_STOP_CAPTURE_LABEL
            startStopButton.icon = AllIcons.Actions.Suspend
        } else {
            startStopButton.text = ArbFormatters.SECONDARY_START_CAPTURE_LABEL
            startStopButton.icon = AllIcons.Actions.Execute
        }

        val existingItems = (0 until deviceCombo.itemCount).map { deviceCombo.getItemAt(it) }
        val newSerials = state.devices.map { it.serial }
        if (existingItems != newSerials) {
            deviceCombo.removeAllItems()
            for (s in newSerials) {
                deviceCombo.addItem(s)
            }
        }
        if (state.selectedSerial != null && newSerials.contains(state.selectedSerial)) {
            deviceCombo.selectedItem = state.selectedSerial
        }

        renderBanners(state.banners)
    }

    // ---- Testable Inspection Accessors ----

    fun visibleBannerKinds(): List<CaptureBannerKind> = currentState.banners.map { it.kind }

    fun isBannerContainerVisible(): Boolean = bannerContainer.isVisible

    fun primarySaveButtonText(): String = primarySaveButton.text

    fun secondarySaveButtonText(): String = secondarySaveButton.text

    fun ringMeterDisplayText(): String = ringMeterLabel.text

    fun statusBadgeDisplayText(): String = statusPill.currentText

    fun clickPrimarySave() = primarySaveButton.doClick()

    fun clickSecondarySave() = secondarySaveButton.doClick()

    fun clickScreenshot() = screenshotButton.doClick()

    fun clickSaveLogs() = logsOnlyButton.doClick()

    fun triggerRefresh() = onRefresh()

    private fun renderBanners(banners: List<CaptureBanner>) {
        bannerContainer.removeAll()
        bannerContainer.isVisible = banners.isNotEmpty()
        for (banner in banners) {
            val (bg, accent, icon) = when (banner.kind) {
                CaptureBannerKind.STORAGE_PRESSURE_STOP ->
                    Triple(ArbTheme.recRedSoft, ArbTheme.recRed, AllIcons.General.Error)
                CaptureBannerKind.CONNECTION_LOST ->
                    Triple(ArbTheme.warningAmberSoft, ArbTheme.warningAmber, AllIcons.General.Warning)
                CaptureBannerKind.STREAM_GAP, CaptureBannerKind.CONFIG_WARNING ->
                    Triple(ArbTheme.warningAmberSoft, ArbTheme.warningAmber, AllIcons.General.Warning)
                CaptureBannerKind.STATIC_HOLD ->
                    Triple(ArbTheme.infoCyanSoft, ArbTheme.infoCyan, AllIcons.General.Information)
            }
            val row = JPanel(BorderLayout(6, 2)).apply {
                background = bg
                isOpaque = true
                border = BorderFactory.createCompoundBorder(
                    BorderFactory.createMatteBorder(0, JBUI.scale(3), 0, 0, accent),
                    BorderFactory.createCompoundBorder(
                        BorderFactory.createLineBorder(ArbTheme.hairlineBorder, 1),
                        JBUI.Borders.empty(6, 8),
                    ),
                )
                val title = JLabel(banner.title, icon, JLabel.LEFT).apply {
                    foreground = ArbTheme.primaryText
                    font = ArbTheme.uiFont(11.5f, bold = true)
                }
                val detail = JLabel(banner.detail).apply {
                    foreground = ArbTheme.secondaryText
                    font = ArbTheme.uiFont(11f)
                }
                add(title, BorderLayout.NORTH)
                add(detail, BorderLayout.SOUTH)
            }
            bannerContainer.add(row)
            bannerContainer.add(Box.createVerticalStrut(JBUI.scale(4)))
        }
        bannerContainer.revalidate()
        bannerContainer.repaint()
    }

    /** Crisp vector REC / status pill (zero emoji). */
    private class StatusPillLabel : JPanel() {
        var currentText: String = "未接続"
            private set
        private var isRecording: Boolean = false
        private var isWarning: Boolean = false

        init {
            isOpaque = false
            preferredSize = Dimension(JBUI.scale(68), JBUI.scale(22))
        }

        fun update(text: String, isRecording: Boolean, isWarning: Boolean) {
            currentText = text
            this.isRecording = isRecording
            this.isWarning = isWarning
            val fm = getFontMetrics(ArbTheme.monoFont(10.5f, bold = true))
            val textW = fm.stringWidth(text)
            val dotSpace = if (isRecording) JBUI.scale(10) else 0
            preferredSize = Dimension(textW + dotSpace + JBUI.scale(16), JBUI.scale(22))
            revalidate()
            repaint()
        }

        override fun paintComponent(g: Graphics) {
            super.paintComponent(g)
            val g2 = g.create() as Graphics2D
            try {
                g2.setRenderingHint(RenderingHints.KEY_ANTIALIASING, RenderingHints.VALUE_ANTIALIAS_ON)
                val bg: Color
                val fg: Color
                val borderCol: Color
                when {
                    isRecording -> {
                        bg = ArbTheme.recRedSoft
                        fg = ArbTheme.recRed
                        borderCol = ArbTheme.recRed
                    }
                    isWarning -> {
                        bg = ArbTheme.warningAmberSoft
                        fg = ArbTheme.warningAmber
                        borderCol = ArbTheme.warningAmber
                    }
                    else -> {
                        bg = ArbTheme.elevatedBg
                        fg = ArbTheme.secondaryText
                        borderCol = ArbTheme.hairlineBorder
                    }
                }
                val arc = JBUI.scale(4)
                g2.color = bg
                g2.fillRoundRect(0, 0, width - 1, height - 1, arc, arc)
                g2.color = borderCol
                g2.drawRoundRect(0, 0, width - 1, height - 1, arc, arc)

                var textX = JBUI.scale(7)
                if (isRecording) {
                    val dot = JBUI.scale(6)
                    g2.color = ArbTheme.recRed
                    g2.fillOval(textX, (height - dot) / 2, dot, dot)
                    textX += dot + JBUI.scale(4)
                }
                g2.font = ArbTheme.monoFont(10.5f, bold = true)
                g2.color = fg
                val fm = g2.fontMetrics
                val textY = (height - fm.height) / 2 + fm.ascent
                g2.drawString(currentText, textX, textY)
            } finally {
                g2.dispose()
            }
        }
    }

    /** Slim 4px IntUI progress bar showing ring buffer fill ratio. */
    private class RingFillBar : JPanel() {
        private var ratio: Double = 0.0
        private var active: Boolean = false

        init {
            isOpaque = false
            preferredSize = Dimension(120, JBUI.scale(5))
            maximumSize = Dimension(Int.MAX_VALUE, JBUI.scale(5))
        }

        fun update(fillRatio: Double, recording: Boolean) {
            ratio = fillRatio.coerceIn(0.0, 1.0)
            active = recording
            repaint()
        }

        override fun paintComponent(g: Graphics) {
            super.paintComponent(g)
            val g2 = g.create() as Graphics2D
            try {
                g2.setRenderingHint(RenderingHints.KEY_ANTIALIASING, RenderingHints.VALUE_ANTIALIAS_ON)
                val h = height.coerceAtLeast(3)
                g2.color = ArbTheme.elevatedBg
                g2.fillRoundRect(0, 0, width, h, h, h)
                val fillW = (width * ratio).toInt().coerceIn(0, width)
                if (fillW > 0) {
                    g2.color = if (active) ArbTheme.accentBlue else ArbTheme.secondaryText
                    g2.fillRoundRect(0, 0, fillW, h, h, h)
                }
            } finally {
                g2.dispose()
            }
        }
    }
}
