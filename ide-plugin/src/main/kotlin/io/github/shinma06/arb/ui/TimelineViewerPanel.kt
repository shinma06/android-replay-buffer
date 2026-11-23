package io.github.shinma06.arb.ui

import io.github.shinma06.arb.model.LogRecord
import io.github.shinma06.arb.model.TimeAlignment
import io.github.shinma06.arb.state.LEGACY_READ_ONLY_BADGE_TEXT
import io.github.shinma06.arb.state.ProvenanceFormatter
import io.github.shinma06.arb.state.TimelineViewerReducer
import io.github.shinma06.arb.state.TimelineViewerState
import io.github.shinma06.arb.state.UNANCHORED_TAB_TITLE
import java.awt.BorderLayout
import java.awt.Color
import java.awt.FlowLayout
import java.awt.Font
import java.awt.image.BufferedImage
import java.nio.file.Path
import javax.swing.BorderFactory
import javax.swing.BoxLayout
import javax.swing.JButton
import javax.swing.JFileChooser
import javax.swing.JLabel
import javax.swing.JPanel
import javax.swing.JScrollPane
import javax.swing.JSlider
import javax.swing.JSpinner
import javax.swing.JSplitPane
import javax.swing.JTabbedPane
import javax.swing.JTable
import javax.swing.SpinnerNumberModel
import javax.swing.table.DefaultTableModel

/**
 * Main bundle timeline & video viewer panel:
 * - SHM video viewport with mandatory approximate-preview badge
 * - Filmstrip keyframe bar & scrubber (`scrub` on drag -> `seek_frame` on settle)
 * - Exact integer frame In/Out trim handles, `-1` / `+1` step buttons, Trim & Export PNG
 * - Discontinuity overlay bar
 * - Honest Provenance & Uncertainty panel
 * - Separate tabs for synchronized Main logs vs `"時刻不明の記録 (Unanchored)"` with truncation warning
 */
class TimelineViewerPanel(
    private val onScrub: (frameIndex: Long) -> Unit,
    private val onSeekExact: (frameIndex: Long) -> Unit,
    private val onTrimExport: (inFrameIndex: Long, outFrameIndex: Long) -> Unit,
    private val onPngExport: (frameIndex: Long, outputPath: Path) -> Unit,
    private val onRangeChanged: (inFrameIndex: Long, outFrameIndex: Long, playhead: Long) -> Unit = { _, _, _ -> },
) : JPanel(BorderLayout(6, 6)) {
    val videoCanvas = VideoViewportCanvas()
    val discontinuityBar = DiscontinuityTimelineBar()

    private val headerLabel = JLabel("読み込み中...")
    private val legacyBadgeLabel = JLabel(LEGACY_READ_ONLY_BADGE_TEXT).apply {
        isOpaque = true
        background = Color(0x47, 0x55, 0x69)
        foreground = Color.WHITE
        font = font.deriveFont(Font.BOLD)
        border = BorderFactory.createEmptyBorder(2, 8, 2, 8)
        isVisible = false
    }

    private val filmstripBar = JPanel(FlowLayout(FlowLayout.LEFT, 4, 2)).apply {
        border = BorderFactory.createTitledBorder("キーフレーム・フィルムストリップ")
    }

    private val scrubSlider = JSlider(0, 0, 0)
    private val stepBackButton = JButton("-1 フレーム")
    private val stepForwardButton = JButton("+1 フレーム")
    private val inFrameSpinner = JSpinner(SpinnerNumberModel(0, 0, 0, 1))
    private val outFrameSpinner = JSpinner(SpinnerNumberModel(0, 0, 0, 1))
    private val setInButton = JButton("現在を In に設定")
    private val setOutButton = JButton("現在を Out に設定")
    private val trimButton = JButton("フレーム単位トリム書き出し")
    private val pngButton = JButton("現在フレームを PNG 書き出し")

    private val provenanceLabel = JLabel("")
    private val videoSemanticsLabel = JLabel("")
    private val audioOffsetLabel = JLabel("")
    private val logcatCompletenessLabel = JLabel("")
    private val unanchoredSummaryLabel = JLabel("")
    private val actionStatusLabel = JLabel("")

    private val mainLogsModel = createLogTableModel()
    private val unanchoredLogsModel = createLogTableModel()
    private val unanchoredTruncatedBanner = JLabel("").apply {
        isOpaque = true
        background = Color(0x9A, 0x34, 0x12)
        foreground = Color.WHITE
        border = BorderFactory.createEmptyBorder(4, 8, 4, 8)
        isVisible = false
    }
    val logsTabbedPane = JTabbedPane()

    private var state: TimelineViewerState? = null
    private var suppressSliderEvents = false
    private var suppressSpinnerEvents = false

    init {
        val topHeader = JPanel(BorderLayout(8, 4)).apply {
            border = BorderFactory.createEmptyBorder(4, 8, 4, 8)
            add(headerLabel, BorderLayout.CENTER)
            add(legacyBadgeLabel, BorderLayout.EAST)
        }

        val transportControls = JPanel(FlowLayout(FlowLayout.LEFT, 6, 4)).apply {
            add(stepBackButton)
            add(stepForwardButton)
            add(JLabel("In フレーム:"))
            add(inFrameSpinner)
            add(setInButton)
            add(JLabel("Out フレーム:"))
            add(outFrameSpinner)
            add(setOutButton)
            add(trimButton)
            add(pngButton)
        }

        val provenancePanel = JPanel().apply {
            layout = BoxLayout(this, BoxLayout.Y_AXIS)
            border = BorderFactory.createTitledBorder("来歴・時刻意味・不確実性 (Provenance & Uncertainty)")
            add(provenanceLabel)
            add(videoSemanticsLabel)
            add(audioOffsetLabel)
            add(logcatCompletenessLabel)
            add(unanchoredSummaryLabel)
        }

        val upperPanel = JPanel(BorderLayout(4, 4)).apply {
            add(videoCanvas, BorderLayout.CENTER)
            val controlsStack = JPanel().apply {
                layout = BoxLayout(this, BoxLayout.Y_AXIS)
                add(scrubSlider)
                add(filmstripBar)
                add(transportControls)
                add(discontinuityBar)
                add(provenancePanel)
            }
            add(controlsStack, BorderLayout.SOUTH)
        }

        val mainLogsPanel = JPanel(BorderLayout()).apply {
            add(JScrollPane(JTable(mainLogsModel)), BorderLayout.CENTER)
        }
        val unanchoredLogsPanel = JPanel(BorderLayout()).apply {
            add(unanchoredTruncatedBanner, BorderLayout.NORTH)
            add(JScrollPane(JTable(unanchoredLogsModel)), BorderLayout.CENTER)
        }

        logsTabbedPane.addTab("主タイムライン (Main)", mainLogsPanel)
        logsTabbedPane.addTab(UNANCHORED_TAB_TITLE, unanchoredLogsPanel)

        val splitPane = JSplitPane(JSplitPane.VERTICAL_SPLIT, upperPanel, logsTabbedPane).apply {
            resizeWeight = 0.65
        }

        add(topHeader, BorderLayout.NORTH)
        add(splitPane, BorderLayout.CENTER)
        add(actionStatusLabel, BorderLayout.SOUTH)

        wireListeners()
    }

    private fun wireListeners() {
        scrubSlider.addChangeListener {
            if (suppressSliderEvents) return@addChangeListener
            val target = scrubSlider.value.toLong()
            if (scrubSlider.valueIsAdjusting) {
                onScrub(target)
            } else {
                onSeekExact(target)
                notifyRangeChanged(playhead = target)
            }
        }

        stepBackButton.addActionListener {
            val s = state ?: return@addActionListener
            val next = TimelineViewerReducer.stepFrame(s, -1L)
            state = next
            onSeekExact(next.currentFrameIndex)
            notifyRangeChanged(playhead = next.currentFrameIndex)
        }

        stepForwardButton.addActionListener {
            val s = state ?: return@addActionListener
            val next = TimelineViewerReducer.stepFrame(s, +1L)
            state = next
            onSeekExact(next.currentFrameIndex)
            notifyRangeChanged(playhead = next.currentFrameIndex)
        }

        setInButton.addActionListener {
            val s = state ?: return@addActionListener
            val next = TimelineViewerReducer.setInFrame(s, s.currentFrameIndex)
            state = next
            syncRangeSpinners(next)
            notifyRangeChanged()
        }

        setOutButton.addActionListener {
            val s = state ?: return@addActionListener
            val next = TimelineViewerReducer.setOutFrame(s, s.currentFrameIndex)
            state = next
            syncRangeSpinners(next)
            notifyRangeChanged()
        }

        inFrameSpinner.addChangeListener {
            if (suppressSpinnerEvents) return@addChangeListener
            val s = state ?: return@addChangeListener
            val next = TimelineViewerReducer.setInFrame(s, (inFrameSpinner.value as Number).toLong())
            state = next
            syncRangeSpinners(next)
            notifyRangeChanged()
        }

        outFrameSpinner.addChangeListener {
            if (suppressSpinnerEvents) return@addChangeListener
            val s = state ?: return@addChangeListener
            val next = TimelineViewerReducer.setOutFrame(s, (outFrameSpinner.value as Number).toLong())
            state = next
            syncRangeSpinners(next)
            notifyRangeChanged()
        }

        trimButton.addActionListener {
            val s = state ?: return@addActionListener
            onTrimExport(s.inFrameIndex, s.outFrameIndex)
        }

        pngButton.addActionListener {
            val s = state ?: return@addActionListener
            val chooser = JFileChooser().apply {
                dialogTitle = "フレーム #${s.currentFrameIndex} の PNG 保存先を選択"
                selectedFile = java.io.File("${s.bundleId}-frame-${s.currentFrameIndex}.png")
            }
            if (chooser.showSaveDialog(this) == JFileChooser.APPROVE_OPTION) {
                onPngExport(s.currentFrameIndex, chooser.selectedFile.toPath())
            }
        }
    }

    fun renderState(next: TimelineViewerState, image: BufferedImage?) {
        state = next
        headerLabel.text = "バンドル: ${next.bundleId} (${next.format.wire}) | フレーム数=${next.frameCount} | Lease=${next.leaseId}"
        legacyBadgeLabel.isVisible = next.isLegacy

        videoCanvas.updateFrame(next.currentFrameMeta, image)
        discontinuityBar.render(next.discontinuities)

        val maxFrame = (next.frameCount - 1L).coerceAtLeast(0L).toInt()
        suppressSliderEvents = true
        scrubSlider.minimum = 0
        scrubSlider.maximum = maxFrame
        scrubSlider.value = next.currentFrameIndex.toInt().coerceIn(0, maxFrame)
        suppressSliderEvents = false

        syncRangeSpinners(next)
        renderFilmstrip(next)

        val prov = ProvenanceFormatter.format(next.summary)
        provenanceLabel.text = prov.provenanceLine
        videoSemanticsLabel.text = prov.videoSemanticsLine
        audioOffsetLabel.text = prov.audioOffsetLine
        logcatCompletenessLabel.text = prov.logcatCompletenessLine
        unanchoredSummaryLabel.text = prov.unanchoredLine

        populateLogs(mainLogsModel, next.mainLogs)
        populateLogs(unanchoredLogsModel, next.unanchoredLogs)

        logsTabbedPane.setTitleAt(0, "主タイムライン (Main: ${next.mainLogs.size})")
        logsTabbedPane.setTitleAt(1, "$UNANCHORED_TAB_TITLE (${next.unanchoredLogs.size})")

        val truncNotice = next.unanchoredTruncatedNotice
        unanchoredTruncatedBanner.isVisible = truncNotice != null
        unanchoredTruncatedBanner.text = truncNotice.orEmpty()

        if (next.lastActionMessage != null) {
            actionStatusLabel.text = next.lastActionMessage
        }

        val hasVideo = next.frameCount > 0L
        stepBackButton.isEnabled = hasVideo
        stepForwardButton.isEnabled = hasVideo
        trimButton.isEnabled = hasVideo
        pngButton.isEnabled = hasVideo
    }

    fun showError(message: String) {
        actionStatusLabel.text = "エラー: $message"
    }

    fun mainLogRowCount(): Int = mainLogsModel.rowCount

    fun unanchoredLogRowCount(): Int = unanchoredLogsModel.rowCount

    fun isUnanchoredTruncatedBannerVisible(): Boolean = unanchoredTruncatedBanner.isVisible

    private fun syncRangeSpinners(s: TimelineViewerState) {
        val maxFrame = (s.frameCount - 1L).coerceAtLeast(0L).toInt()
        suppressSpinnerEvents = true
        (inFrameSpinner.model as SpinnerNumberModel).apply {
            minimum = 0
            maximum = maxFrame
            value = s.inFrameIndex.toInt().coerceIn(0, maxFrame)
        }
        (outFrameSpinner.model as SpinnerNumberModel).apply {
            minimum = 0
            maximum = maxFrame
            value = s.outFrameIndex.toInt().coerceIn(0, maxFrame)
        }
        suppressSpinnerEvents = false
    }

    private fun renderFilmstrip(s: TimelineViewerState) {
        filmstripBar.removeAll()
        if (s.filmstrip.isEmpty()) {
            filmstripBar.add(JLabel("キーフレームなし"))
        } else {
            for (entry in s.filmstrip.take(24)) {
                val btn = JButton("KF #${entry.frameIndex}").apply {
                    toolTipText = "pts=${entry.ptsNs / 1_000_000} ms (${entry.width}x${entry.height})"
                    addActionListener { onSeekExact(entry.frameIndex) }
                }
                filmstripBar.add(btn)
            }
        }
        filmstripBar.revalidate()
        filmstripBar.repaint()
    }

    private fun notifyRangeChanged(playhead: Long? = null) {
        val s = state ?: return
        onRangeChanged(s.inFrameIndex, s.outFrameIndex, playhead ?: s.currentFrameIndex)
    }

    private fun populateLogs(model: DefaultTableModel, records: List<LogRecord>) {
        model.rowCount = 0
        for (rec in records) {
            val timeSummary = when (val a = rec.alignment) {
                is TimeAlignment.Anchored -> "boottime=${a.boottimeNs / 1_000_000} ms (±${a.halfWidthNs / 1_000_000} ms)"
                is TimeAlignment.Bounded -> "[${a.minBoottimeNs / 1_000_000}..${a.maxBoottimeNs / 1_000_000}] ms (${a.reason.wire})"
                is TimeAlignment.Unanchored -> "時刻不明 (${a.reason.wire})"
            }
            model.addRow(
                arrayOf<Any>(
                    rec.seq,
                    timeSummary,
                    rec.level ?: "-",
                    rec.tag ?: "-",
                    rec.message,
                ),
            )
        }
    }

    private fun createLogTableModel(): DefaultTableModel = object : DefaultTableModel(
        arrayOf("Seq", "時刻 / 確度", "Level", "Tag", "メッセージ"),
        0,
    ) {
        override fun isCellEditable(row: Int, column: Int): Boolean = false
    }
}
