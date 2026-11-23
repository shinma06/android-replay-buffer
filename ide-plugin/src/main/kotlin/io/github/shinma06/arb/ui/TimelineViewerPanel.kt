package io.github.shinma06.arb.ui

import com.intellij.icons.AllIcons
import com.intellij.ui.JBSplitter
import com.intellij.util.ui.JBUI
import io.github.shinma06.arb.playback.AudioState
import io.github.shinma06.arb.playback.PlaybackMode
import io.github.shinma06.arb.playback.PlaybackSpeed
import io.github.shinma06.arb.playback.PlaybackState
import io.github.shinma06.arb.state.ArbFormatters
import io.github.shinma06.arb.state.ExportCompletionInfo
import io.github.shinma06.arb.state.HonestySeverity
import io.github.shinma06.arb.state.SourceJumpTarget
import io.github.shinma06.arb.state.Tier2HonestyChip
import io.github.shinma06.arb.state.TimelineViewerReducer
import io.github.shinma06.arb.state.TimelineViewerState
import java.awt.BorderLayout
import java.awt.Dimension
import java.awt.FlowLayout
import java.awt.Toolkit
import java.awt.datatransfer.StringSelection
import java.awt.event.ActionEvent
import java.awt.event.KeyEvent
import java.awt.image.BufferedImage
import java.nio.file.Path
import javax.swing.AbstractAction
import javax.swing.BorderFactory
import javax.swing.JButton
import javax.swing.JComponent
import javax.swing.JFileChooser
import javax.swing.JLabel
import javax.swing.JPanel
import javax.swing.JTabbedPane
import javax.swing.KeyStroke

/**
 * Redesigned Integrated Viewer & Trim Studio (Android Studio IntUI native, zero emoji):
 * - Single seamless studio (no separate View vs Trim modes)
 * - Tier 1 Top Action Bar (40px): bundle title + muted device/time + conditional Tier-2 honesty chips
 *   (click opens `詳細` tab) + `PNG 保存` / `Finder` / primary CTA `選択範囲を書き出し ({dur}s)`
 * - Inline Export Completion Bar (`[Finder で表示]` `[パスをコピー]` `[新しいタブで開く]`)
 * - Resizable horizontal `JBSplitter`:
 *   - Left (~58%): High-DPI SHM `VideoViewportCanvas` (+ tap ripple overlay + approximate pill) +
 *     minimal transport bar wired to `PlaybackController` & `AudioSyncPlayer`
 *   - Right (~42%): `EventLogStreamPanel` (`操作＆ログ` / `時刻不明 (N)` / `詳細`, trim-range dimming,
 *     `0..300ms` tap-to-log correlation, hover `[In]`/`[Out]` snap, PSI source jump)
 * - Bottom Full-Width `MultiTrackTimelineDock` (filmstrip + dual handles + screen spans + markers +
 *   deterministic presets + non-destructive Trash-Original toggle)
 */
class TimelineViewerPanel(
    private val onScrub: (frameIndex: Long) -> Unit,
    private val onSeekExact: (frameIndex: Long) -> Unit,
    private val onTrimExport: (inFrameIndex: Long, outFrameIndex: Long) -> Unit,
    private val onPngExport: (frameIndex: Long, outputPath: Path) -> Unit,
    private val onRangeChanged: (inFrameIndex: Long, outFrameIndex: Long, playhead: Long) -> Unit = { _, _, _ -> },
    private val onPlayPauseToggle: () -> Unit = {},
    private val onStepFrame: (delta: Int) -> Unit = {},
    private val onSpeedChange: (PlaybackSpeed) -> Unit = {},
    private val onMuteToggle: () -> Unit = {},
    private val onSelectTapRange: () -> Unit = {},
    private val onSelectAroundCrash: () -> Unit = {},
    private val onResetFullRange: () -> Unit = {},
    private val onTrashOriginalToggled: (enabled: Boolean) -> Unit = {},
    private val onRevealInFinder: (path: String) -> Unit = {},
    private val onOpenExportedBundle: (bundleIdOrPath: String, title: String) -> Unit = { _, _ -> },
    private val onSourceJump: (SourceJumpTarget) -> Unit = {},
) : JPanel(BorderLayout()) {
    val videoCanvas = VideoViewportCanvas()

    val eventLogStreamPanel = EventLogStreamPanel(
        onSeekToFrame = { frame -> onSeekExact(frame) },
        onSnapInFrame = { frame -> applyInFrame(frame, notify = true) },
        onSnapOutFrame = { frame -> applyOutFrame(frame, notify = true) },
        onSourceJump = onSourceJump,
    )

    val multiTrackDock: MultiTrackTimelineDock = MultiTrackTimelineDock(
        onScrub = onScrub,
        onSeekExact = { frame ->
            onSeekExact(frame)
            notifyRangeChanged(playhead = frame)
        },
        onInFrameChanged = { frame, settled -> applyInFrame(frame, notify = settled) },
        onOutFrameChanged = { frame, settled -> applyOutFrame(frame, notify = settled) },
        onSelectTapRange = onSelectTapRange,
        onSelectAroundCrash = onSelectAroundCrash,
        onResetFullRange = onResetFullRange,
        onTrashOriginalToggled = { enabled ->
            val s = state ?: return@MultiTrackTimelineDock
            val next = TimelineViewerReducer.setTrashOriginalAfterExport(s, enabled)
            state = next
            multiTrackDock.render(next)
            onTrashOriginalToggled(next.effectiveTrashOriginalAfterExport)
        },
        onSourceJump = onSourceJump,
    )

    // Backwards-compatible public accessors used by existing & new headless tests
    val discontinuityBar: DiscontinuityTimelineBar
        get() = eventLogStreamPanel.discontinuityBar

    val logsTabbedPane: JTabbedPane
        get() = eventLogStreamPanel.tabbedPane

    // ---- Tier 1 Top Action Bar (single 40px row) ----
    private val titleLabel = JLabel("読み込み中...").apply {
        font = ArbTheme.uiFont(12.5f, bold = true)
        foreground = ArbTheme.primaryText
    }
    private val subtitleLabel = JLabel("").apply {
        font = ArbTheme.monoFont(11f)
        foreground = ArbTheme.secondaryText
    }
    private val honestyChipsPanel = JPanel(FlowLayout(FlowLayout.LEFT, 4, 0)).apply {
        isOpaque = false
    }
    private val pngButton = ArbTheme.styleSecondaryButton(
        JButton("PNG 保存", AllIcons.Actions.Dump),
        compact = true,
    )
    private val finderButton = ArbTheme.styleSecondaryButton(
        JButton("Finder", AllIcons.Nodes.Folder),
        compact = true,
    )
    private val exportButton = ArbTheme.stylePrimaryButton(
        JButton(ArbFormatters.EXPORT_FULL_LABEL, AllIcons.Actions.MenuSaveall),
    )

    // ---- Inline Export Completion Bar (hidden until export succeeds) ----
    private val exportToastSummary = JLabel("").apply {
        font = ArbTheme.uiFont(11.5f, bold = true)
        foreground = ArbTheme.primaryText
    }
    private val toastRevealBtn = ArbTheme.styleSecondaryButton(JButton("Finder で表示", AllIcons.Nodes.Folder), compact = true)
    private val toastCopyBtn = ArbTheme.styleSecondaryButton(JButton("パスをコピー", AllIcons.Actions.Copy), compact = true)
    private val toastOpenTabBtn = ArbTheme.styleSecondaryButton(JButton("新しいタブで開く", AllIcons.Actions.Execute), compact = true)
    private val toastDismissBtn = ArbTheme.styleSecondaryButton(JButton(AllIcons.Actions.Cancel), compact = true)
    private val exportCompletionBar = JPanel(BorderLayout(8, 0)).apply {
        background = ArbTheme.successGreenSoft
        isOpaque = true
        border = BorderFactory.createCompoundBorder(
            BorderFactory.createMatteBorder(0, JBUI.scale(3), 1, 0, ArbTheme.successGreen),
            JBUI.Borders.empty(5, 10),
        )
        add(exportToastSummary, BorderLayout.CENTER)
        val actions = JPanel(FlowLayout(FlowLayout.RIGHT, 4, 0)).apply {
            isOpaque = false
            add(toastRevealBtn)
            add(toastCopyBtn)
            add(toastOpenTabBtn)
            add(toastDismissBtn)
        }
        add(actions, BorderLayout.EAST)
        isVisible = false
    }

    // ---- Minimal Transport Bar (left Video Stage footer) ----
    private val playPauseButton = ArbTheme.styleSecondaryButton(
        JButton("再生", AllIcons.Actions.Execute),
        compact = true,
    )
    private val stepBackButton = ArbTheme.styleSecondaryButton(
        JButton("-1f", AllIcons.Actions.Back),
        compact = true,
    )
    private val stepForwardButton = ArbTheme.styleSecondaryButton(
        JButton("+1f", AllIcons.Actions.Forward),
        compact = true,
    )
    private val speedButton = ArbTheme.styleSecondaryButton(
        JButton(PlaybackSpeed.NORMAL.label),
        compact = true,
    )
    private val muteButton = ArbTheme.styleSecondaryButton(
        JButton("音声"),
        compact = true,
    )
    private val tapOverlayToggleButton = ArbTheme.styleFilterChip(
        JButton("タップ表示"),
        active = true,
    )
    private val timecodeLabel = JLabel("00:00.00 / 00:00.00 · #0").apply {
        font = ArbTheme.monoFont(11f, bold = true)
        foreground = ArbTheme.primaryText
    }
    private val errorStatusLabel = JLabel("").apply {
        font = ArbTheme.uiFont(11f)
        foreground = ArbTheme.recRed
        isVisible = false
    }

    private var state: TimelineViewerState? = null
    private var playbackState: PlaybackState = PlaybackState()

    init {
        background = ArbTheme.surfaceBg
        isOpaque = true

        // Top 40px Tier-1 Action Bar
        val topBar = JPanel(BorderLayout(10, 0)).apply {
            background = ArbTheme.cardBg
            isOpaque = true
            preferredSize = Dimension(JBUI.scale(720), JBUI.scale(42))
            border = ArbTheme.hairlineBottomBorder(5, 10)

            val leftTitle = JPanel(FlowLayout(FlowLayout.LEFT, 8, 2)).apply {
                isOpaque = false
                add(titleLabel)
                add(subtitleLabel)
            }
            val rightActions = JPanel(FlowLayout(FlowLayout.RIGHT, 6, 0)).apply {
                isOpaque = false
                add(pngButton)
                add(finderButton)
                add(exportButton)
            }
            add(leftTitle, BorderLayout.WEST)
            add(honestyChipsPanel, BorderLayout.CENTER)
            add(rightActions, BorderLayout.EAST)
        }

        val northStack = JPanel(BorderLayout()).apply {
            isOpaque = false
            add(topBar, BorderLayout.NORTH)
            add(exportCompletionBar, BorderLayout.CENTER)
            add(errorStatusLabel, BorderLayout.SOUTH)
        }

        // Left Video Stage + Minimal Transport Bar
        val transportBar = JPanel(BorderLayout(8, 0)).apply {
            background = ArbTheme.cardBg
            isOpaque = true
            border = ArbTheme.hairlineTopBorder(5, 8)

            val controls = JPanel(FlowLayout(FlowLayout.LEFT, 4, 0)).apply {
                isOpaque = false
                add(playPauseButton)
                add(stepBackButton)
                add(stepForwardButton)
                add(speedButton)
                add(muteButton)
                add(tapOverlayToggleButton)
            }
            add(controls, BorderLayout.WEST)
            add(timecodeLabel, BorderLayout.EAST)
        }

        val videoStagePanel = JPanel(BorderLayout()).apply {
            background = ArbTheme.stageBg
            isOpaque = true
            add(videoCanvas, BorderLayout.CENTER)
            add(transportBar, BorderLayout.SOUTH)
        }

        // Resizable horizontal JBSplitter (58% video stage / 42% synchronized event & log stream)
        val mainSplitter = JBSplitter(false, 0.58f).apply {
            firstComponent = videoStagePanel
            secondComponent = eventLogStreamPanel
            dividerWidth = JBUI.scale(4)
        }

        add(northStack, BorderLayout.NORTH)
        add(mainSplitter, BorderLayout.CENTER)
        add(multiTrackDock, BorderLayout.SOUTH)

        wireButtons()
        wireKeyboardShortcuts()
    }

    private fun wireButtons() {
        playPauseButton.addActionListener { onPlayPauseToggle() }

        stepBackButton.addActionListener {
            val s = state ?: return@addActionListener
            val next = TimelineViewerReducer.stepFrame(s, -1L)
            state = next
            onStepFrame(-1)
            onSeekExact(next.currentFrameIndex)
            notifyRangeChanged(playhead = next.currentFrameIndex)
        }

        stepForwardButton.addActionListener {
            val s = state ?: return@addActionListener
            val next = TimelineViewerReducer.stepFrame(s, +1L)
            state = next
            onStepFrame(+1)
            onSeekExact(next.currentFrameIndex)
            notifyRangeChanged(playhead = next.currentFrameIndex)
        }

        speedButton.addActionListener {
            val nextSpeed = when (playbackState.speed) {
                PlaybackSpeed.HALF -> PlaybackSpeed.NORMAL
                PlaybackSpeed.NORMAL -> PlaybackSpeed.DOUBLE
                PlaybackSpeed.DOUBLE -> PlaybackSpeed.HALF
            }
            playbackState = playbackState.copy(speed = nextSpeed)
            speedButton.text = nextSpeed.label
            onSpeedChange(nextSpeed)
        }

        muteButton.addActionListener { onMuteToggle() }

        tapOverlayToggleButton.addActionListener {
            val s = state ?: return@addActionListener
            val next = s.copy(showTapOverlay = !s.showTapOverlay)
            state = next
            ArbTheme.styleFilterChip(tapOverlayToggleButton, active = next.showTapOverlay)
            videoCanvas.setTapOverlay(next.activeTapsAtFrame(), next.showTapOverlay)
        }

        finderButton.addActionListener {
            state?.path?.let(onRevealInFinder)
        }

        exportButton.addActionListener {
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

        toastRevealBtn.addActionListener {
            state?.exportCompletion?.outputPath?.let(onRevealInFinder)
        }
        toastCopyBtn.addActionListener {
            val path = state?.exportCompletion?.outputPath ?: return@addActionListener
            runCatching {
                Toolkit.getDefaultToolkit().systemClipboard.setContents(StringSelection(path), null)
            }
        }
        toastOpenTabBtn.addActionListener {
            val info = state?.exportCompletion ?: return@addActionListener
            val id = info.bundleId ?: info.outputPath
            onOpenExportedBundle(id, info.bundleId ?: Path.of(info.outputPath).fileName.toString())
        }
        toastDismissBtn.addActionListener {
            exportCompletionBar.isVisible = false
            state = state?.copy(exportCompletion = null)
        }
    }

    private fun wireKeyboardShortcuts() {
        val inputMap = getInputMap(JComponent.WHEN_ANCESTOR_OF_FOCUSED_COMPONENT)
        val actionMap = actionMap

        fun bind(keyStroke: KeyStroke, name: String, block: () -> Unit) {
            inputMap.put(keyStroke, name)
            actionMap.put(
                name,
                object : AbstractAction() {
                    override fun actionPerformed(e: ActionEvent?) = block()
                },
            )
        }

        bind(KeyStroke.getKeyStroke(KeyEvent.VK_SPACE, 0), "arb.playPause") { onPlayPauseToggle() }
        bind(KeyStroke.getKeyStroke(KeyEvent.VK_LEFT, 0), "arb.stepBack") { stepBackButton.doClick() }
        bind(KeyStroke.getKeyStroke(KeyEvent.VK_RIGHT, 0), "arb.stepForward") { stepForwardButton.doClick() }
        bind(KeyStroke.getKeyStroke(KeyEvent.VK_I, 0), "arb.setIn") {
            state?.let { applyInFrame(it.currentFrameIndex, notify = true) }
        }
        bind(KeyStroke.getKeyStroke(KeyEvent.VK_O, 0), "arb.setOut") {
            state?.let { applyOutFrame(it.currentFrameIndex, notify = true) }
        }
    }

    private fun applyInFrame(frameIndex: Long, notify: Boolean) {
        val s = state ?: return
        val next = TimelineViewerReducer.setInFrame(s, frameIndex)
        state = next
        renderDerivedViews(next)
        if (notify) notifyRangeChanged()
    }

    private fun applyOutFrame(frameIndex: Long, notify: Boolean) {
        val s = state ?: return
        val next = TimelineViewerReducer.setOutFrame(s, frameIndex)
        state = next
        renderDerivedViews(next)
        if (notify) notifyRangeChanged()
    }

    fun renderState(next: TimelineViewerState, image: BufferedImage?) {
        state = next
        errorStatusLabel.isVisible = false

        titleLabel.text = next.bundleId
        val dev = next.summary.device?.let { "${it.model} (API ${it.apiLevel})" } ?: next.format.wire
        val created = next.summary.createdAtUtc?.replace('T', ' ')?.removeSuffix("Z")?.take(19).orEmpty()
        subtitleLabel.text = listOf(dev, created).filter { it.isNotBlank() }.joinToString(" · ")

        renderHonestyChips(next.tier2HonestyChips)

        videoCanvas.updateFrame(
            meta = next.currentFrameMeta,
            image = image,
            taps = next.activeTapsAtFrame(),
            showTaps = next.showTapOverlay,
        )
        ArbTheme.styleFilterChip(tapOverlayToggleButton, active = next.showTapOverlay)

        renderDerivedViews(next)
        renderExportCompletion(next.exportCompletion)

        val hasVideo = next.frameCount > 0L
        playPauseButton.isEnabled = hasVideo
        stepBackButton.isEnabled = hasVideo
        stepForwardButton.isEnabled = hasVideo
        speedButton.isEnabled = hasVideo
        exportButton.isEnabled = hasVideo
        pngButton.isEnabled = hasVideo
    }

    fun renderPlaybackState(pb: PlaybackState) {
        playbackState = pb
        val isPlaying = pb.mode == PlaybackMode.PLAYING
        playPauseButton.text = if (isPlaying) "一時停止" else "再生"
        playPauseButton.icon = if (isPlaying) AllIcons.Actions.Pause else AllIcons.Actions.Execute
        speedButton.text = pb.speed.label
        muteButton.text = when {
            pb.audioState == AudioState.UNAVAILABLE -> "音声なし"
            pb.muted || pb.audioState == AudioState.MUTED -> "ミュート"
            else -> "音声 ON"
        }
        muteButton.isEnabled = pb.audioState != AudioState.UNAVAILABLE

        videoCanvas.setApproximatePreview(pb.isApproximate)

        val s = state
        if (s != null) {
            val synced = s.copy(
                currentFrameIndex = pb.currentFrameIndex,
                inFrameIndex = pb.inFrameIndex,
                outFrameIndex = pb.outFrameIndex,
            )
            state = synced
            renderDerivedViews(synced)
        }
    }

    fun showError(message: String) {
        errorStatusLabel.text = "エラー: $message"
        errorStatusLabel.border = JBUI.Borders.empty(4, 10)
        errorStatusLabel.isVisible = true
    }

    // ---- Testable Inspection Accessors ----

    fun mainLogRowCount(): Int = eventLogStreamPanel.mainLogRowCount()

    fun unanchoredLogRowCount(): Int = eventLogStreamPanel.unanchoredLogRowCount()

    fun isUnanchoredTruncatedBannerVisible(): Boolean = eventLogStreamPanel.isUnanchoredTruncatedBannerVisible()

    fun visibleHonestyChipLabels(): List<String> = state?.tier2HonestyChips?.map { it.label }.orEmpty()

    fun exportButtonText(): String = exportButton.text

    fun timecodeDisplayText(): String = timecodeLabel.text

    fun isExportCompletionBarVisible(): Boolean = exportCompletionBar.isVisible

    fun playPauseButtonText(): String = playPauseButton.text

    private fun renderDerivedViews(s: TimelineViewerState) {
        val curMs = s.frameToRelativeMs(s.currentFrameIndex)
        val totalMs = s.totalDurationMs
        timecodeLabel.text = "${ArbFormatters.formatTimecode(curMs)} / ${ArbFormatters.formatTimecode(totalMs)} · #${s.currentFrameIndex}"
        exportButton.text = s.exportCtaLabel
        videoCanvas.setTapOverlay(s.activeTapsAtFrame(), s.showTapOverlay)
        eventLogStreamPanel.render(s)
        multiTrackDock.render(s)
    }

    private fun renderHonestyChips(chips: List<Tier2HonestyChip>) {
        honestyChipsPanel.removeAll()
        honestyChipsPanel.isVisible = chips.isNotEmpty()
        for (chip in chips) {
            val (bg, fg, borderCol) = when (chip.severity) {
                HonestySeverity.ERROR -> Triple(ArbTheme.recRedSoft, ArbTheme.recRed, ArbTheme.recRed)
                HonestySeverity.WARNING -> Triple(ArbTheme.warningAmberSoft, ArbTheme.warningAmber, ArbTheme.warningAmber)
                HonestySeverity.INFO -> Triple(ArbTheme.elevatedBg, ArbTheme.secondaryText, ArbTheme.hairlineBorder)
            }
            val btn = JButton(chip.label).apply {
                isFocusPainted = false
                isOpaque = true
                background = bg
                foreground = fg
                font = ArbTheme.monoFont(10.5f, bold = true)
                toolTipText = "${chip.detail}（クリックで詳細タブを表示）"
                border = BorderFactory.createCompoundBorder(
                    BorderFactory.createLineBorder(borderCol, 1),
                    JBUI.Borders.empty(2, 7),
                )
                addActionListener { eventLogStreamPanel.selectDetailsTab() }
            }
            honestyChipsPanel.add(btn)
        }
        honestyChipsPanel.revalidate()
        honestyChipsPanel.repaint()
    }

    private fun renderExportCompletion(info: ExportCompletionInfo?) {
        exportCompletionBar.isVisible = info != null
        if (info != null) {
            exportToastSummary.text = info.summaryText
            toastOpenTabBtn.isVisible = info.isTrimBundle && !info.bundleId.isNullOrBlank()
        }
    }

    private fun notifyRangeChanged(playhead: Long? = null) {
        val s = state ?: return
        onRangeChanged(s.inFrameIndex, s.outFrameIndex, playhead ?: s.currentFrameIndex)
    }
}
