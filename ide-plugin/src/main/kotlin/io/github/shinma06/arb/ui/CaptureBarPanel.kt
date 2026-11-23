package io.github.shinma06.arb.ui

import io.github.shinma06.arb.state.CaptureBanner
import io.github.shinma06.arb.state.CaptureBannerKind
import io.github.shinma06.arb.state.CaptureBarState
import java.awt.BorderLayout
import java.awt.Color
import java.awt.FlowLayout
import java.awt.Font
import javax.swing.BorderFactory
import javax.swing.BoxLayout
import javax.swing.JButton
import javax.swing.JComboBox
import javax.swing.JLabel
import javax.swing.JPanel
import javax.swing.JSpinner
import javax.swing.SpinnerNumberModel

/**
 * Live capture bar displaying daemon/device status, 1-click Save (30s / configurable),
 * and explicit banners for `STORAGE_PRESSURE_STOP`, `CONNECTION_LOST`, `STREAM_GAP`, and `STATIC_HOLD`.
 */
class CaptureBarPanel(
    private val onStartCapture: (serial: String?) -> Unit,
    private val onStopCapture: () -> Unit,
    private val onSaveNow: (durationSec: Long) -> Unit,
    private val onRefresh: () -> Unit,
) : JPanel(BorderLayout(6, 6)) {
    private val statusLabel = JLabel("arbd 未接続")
    private val ringLabel = JLabel("リング: 0 チャンク")
    private val deviceCombo = JComboBox<String>()
    private val durationSpinner = JSpinner(SpinnerNumberModel(30, 5, 600, 5))
    private val startStopButton = JButton("キャプチャ開始")
    private val saveButton = JButton("直前を保存")
    private val refreshButton = JButton("更新")
    private val bannerContainer = JPanel().apply {
        layout = BoxLayout(this, BoxLayout.Y_AXIS)
    }

    private var currentState = CaptureBarState()

    init {
        border = BorderFactory.createTitledBorder("ライブキャプチャ & クイック保存")

        val controls = JPanel(FlowLayout(FlowLayout.LEFT, 6, 4)).apply {
            add(JLabel("端末:"))
            add(deviceCombo)
            add(startStopButton)
            add(JLabel("保存秒数:"))
            add(durationSpinner)
            add(saveButton)
            add(refreshButton)
        }

        val infoBar = JPanel(FlowLayout(FlowLayout.LEFT, 12, 2)).apply {
            add(statusLabel)
            add(ringLabel)
        }

        val topStack = JPanel().apply {
            layout = BoxLayout(this, BoxLayout.Y_AXIS)
            add(controls)
            add(infoBar)
        }

        add(topStack, BorderLayout.NORTH)
        add(bannerContainer, BorderLayout.CENTER)

        startStopButton.addActionListener {
            if (currentState.capture?.active == true) {
                onStopCapture()
            } else {
                val serial = deviceCombo.selectedItem as? String
                onStartCapture(serial?.takeIf { it.isNotBlank() })
            }
        }
        saveButton.addActionListener {
            val sec = (durationSpinner.value as Number).toLong()
            onSaveNow(sec)
        }
        refreshButton.addActionListener { onRefresh() }
    }

    fun render(state: CaptureBarState) {
        currentState = state
        val cap = state.capture
        statusLabel.text = when {
            !state.daemonConnected -> "arbd 未接続 (${state.lastStatusMessage ?: "待機中"})"
            cap == null -> "arbd 接続済み (PID=${state.daemonPid})"
            cap.storagePressureStopped -> "容量圧迫により停止中 (STORAGE_PRESSURE_STOP)"
            cap.active && cap.connected ->
                "録画中: ${cap.serial ?: "-"} (映像 ${cap.videoFrames}f / ログ ${cap.logRecords}件)"
            cap.active && !cap.connected ->
                "接続切断中 (再接続待機): ${cap.serial ?: "-"}"
            else -> "待機中 (arbd PID=${state.daemonPid})"
        }

        val mb = state.ringTotalBytes / (1024.0 * 1024.0)
        ringLabel.text = "リング: ${state.ringChunkCount} チャンク (%.1f MiB / Pin=${state.ringPinnedChunkCount})".format(mb)
        startStopButton.text = if (cap?.active == true) "キャプチャ停止" else "キャプチャ開始"

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

    fun visibleBannerKinds(): List<CaptureBannerKind> = currentState.banners.map { it.kind }

    private fun renderBanners(banners: List<CaptureBanner>) {
        bannerContainer.removeAll()
        for (banner in banners) {
            val bg = when (banner.kind) {
                CaptureBannerKind.STORAGE_PRESSURE_STOP -> Color(0x7F, 0x1D, 0x1D)
                CaptureBannerKind.CONNECTION_LOST -> Color(0x9A, 0x34, 0x12)
                CaptureBannerKind.STREAM_GAP -> Color(0x85, 0x4D, 0x0E)
                CaptureBannerKind.STATIC_HOLD -> Color(0x1E, 0x3A, 0x5F)
            }
            val row = JPanel(BorderLayout(4, 2)).apply {
                background = bg
                isOpaque = true
                border = BorderFactory.createEmptyBorder(6, 8, 6, 8)
                val title = JLabel(banner.title).apply {
                    foreground = Color.WHITE
                    font = font.deriveFont(Font.BOLD)
                }
                val detail = JLabel(banner.detail).apply {
                    foreground = Color(0xF3, 0xF4, 0xF6)
                }
                add(title, BorderLayout.NORTH)
                add(detail, BorderLayout.SOUTH)
            }
            bannerContainer.add(row)
        }
        bannerContainer.revalidate()
        bannerContainer.repaint()
    }
}
