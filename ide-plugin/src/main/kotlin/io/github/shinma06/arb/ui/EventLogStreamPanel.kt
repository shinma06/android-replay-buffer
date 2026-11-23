package io.github.shinma06.arb.ui

import com.intellij.icons.AllIcons
import com.intellij.ui.components.JBList
import com.intellij.ui.components.JBScrollPane
import com.intellij.ui.components.JBTabbedPane
import com.intellij.ui.components.JBTextField
import com.intellij.util.ui.JBUI
import io.github.shinma06.arb.model.LogRecord
import io.github.shinma06.arb.model.TimeAlignment
import io.github.shinma06.arb.state.ArbFormatters
import io.github.shinma06.arb.state.EventStreamFilter
import io.github.shinma06.arb.state.ProvenanceFormatter
import io.github.shinma06.arb.state.SourceJumpTarget
import io.github.shinma06.arb.state.SyncedEventKind
import io.github.shinma06.arb.state.SyncedEventRow
import io.github.shinma06.arb.state.TimelineViewerState
import io.github.shinma06.arb.state.UNANCHORED_TAB_TITLE
import java.awt.BorderLayout
import java.awt.Color
import java.awt.Component
import java.awt.Cursor
import java.awt.Dimension
import java.awt.FlowLayout
import java.awt.event.MouseAdapter
import java.awt.event.MouseEvent
import javax.swing.BorderFactory
import javax.swing.Box
import javax.swing.BoxLayout
import javax.swing.DefaultListModel
import javax.swing.JButton
import javax.swing.JLabel
import javax.swing.JList
import javax.swing.JPanel
import javax.swing.JTable
import javax.swing.ListCellRenderer
import javax.swing.ListSelectionModel
import javax.swing.event.DocumentEvent
import javax.swing.event.DocumentListener
import javax.swing.table.DefaultTableModel

/**
 * Right-hand Synchronized Event & Log Stream panel (~42% of studio split):
 * - 3 clean tabs: `操作＆ログ` (default) / `時刻不明 (N)` / `詳細` (Tier 3 technical metadata)
 * - Filter chips (`すべて` / `タップ` / `エラー・クラッシュ`) + keyword search
 * - Real-time Trim Range Sync (in-range full contrast + accent bar; out-of-range dimmed; playhead highlighted)
 * - Tap -> Following Logs Correlation (`0..300ms` visual link & badge)
 * - Hover `[In]` / `[Out]` snap buttons on every synced event row
 * - Clickable PC-side PSI Source Jump indicator on Activities, stack traces & crashes
 */
class EventLogStreamPanel(
    private val onSeekToFrame: (frameIndex: Long) -> Unit,
    private val onSnapInFrame: (frameIndex: Long) -> Unit,
    private val onSnapOutFrame: (frameIndex: Long) -> Unit,
    private val onSourceJump: (SourceJumpTarget) -> Unit,
) : JPanel(BorderLayout()) {
    val tabbedPane = JBTabbedPane()
    val discontinuityBar = DiscontinuityTimelineBar()

    private val filterButtons: Map<EventStreamFilter, JButton> = EventStreamFilter.entries.associateWith { f ->
        ArbTheme.styleFilterChip(JButton(f.label), active = f == EventStreamFilter.ALL).apply {
            addActionListener { setFilter(f) }
        }
    }

    private val searchField = JBTextField().apply {
        emptyText.text = "タグ・メッセージ検索..."
        font = ArbTheme.uiFont(11.5f)
    }

    private val syncedListModel = DefaultListModel<SyncedEventRow>()
    private var displayedRows: List<SyncedEventRow> = emptyList()
    private var hoveredRowIndex: Int = -1

    private val syncedEventList = JBList(syncedListModel).apply {
        selectionMode = ListSelectionModel.SINGLE_SELECTION
        fixedCellHeight = JBUI.scale(38)
        background = ArbTheme.surfaceBg
        cellRenderer = SyncedEventCellRenderer()
    }

    // Tab 2: Unanchored (strictly isolated!)
    private val unanchoredLogsModel = object : DefaultTableModel(
        arrayOf("Seq", "確度", "Level", "Tag", "メッセージ"),
        0,
    ) {
        override fun isCellEditable(row: Int, column: Int): Boolean = false
    }
    private val unanchoredTruncatedBanner = JLabel("", AllIcons.General.Warning, JLabel.LEFT).apply {
        isOpaque = true
        background = ArbTheme.warningAmberSoft
        foreground = ArbTheme.primaryText
        font = ArbTheme.uiFont(11f, bold = true)
        border = BorderFactory.createCompoundBorder(
            BorderFactory.createMatteBorder(0, JBUI.scale(3), 1, 0, ArbTheme.warningAmber),
            JBUI.Borders.empty(6, 8),
        )
        isVisible = false
    }

    // Tab 3: Tier 3 technical details (strictly kept out of the main view!)
    private val provenanceLabel = techLabel()
    private val videoSemanticsLabel = techLabel()
    private val audioOffsetLabel = techLabel()
    private val logcatCompletenessLabel = techLabel()
    private val unanchoredSummaryLabel = techLabel()
    private val leaseAndPathLabel = techLabel()
    private val epochsLabel = techLabel()

    private var currentState: TimelineViewerState? = null
    private var activeFilter: EventStreamFilter = EventStreamFilter.ALL

    private val inBtnWidth = JBUI.scale(34)
    private val outBtnWidth = JBUI.scale(38)
    private val actionGap = JBUI.scale(4)
    private val rightPad = JBUI.scale(8)

    init {
        background = ArbTheme.surfaceBg
        isOpaque = true

        val filterRow = JPanel(BorderLayout(6, 0)).apply {
            background = ArbTheme.surfaceBg
            isOpaque = true
            border = ArbTheme.hairlineBottomBorder(6, 8)
            val chips = JPanel(FlowLayout(FlowLayout.LEFT, 4, 0)).apply {
                isOpaque = false
                for (f in EventStreamFilter.entries) {
                    add(filterButtons.getValue(f))
                }
            }
            add(chips, BorderLayout.WEST)
            add(searchField, BorderLayout.CENTER)
        }

        val syncedTab = JPanel(BorderLayout()).apply {
            isOpaque = false
            add(filterRow, BorderLayout.NORTH)
            add(JBScrollPane(syncedEventList).apply { border = JBUI.Borders.empty() }, BorderLayout.CENTER)
        }

        val unanchoredTable = JTable(unanchoredLogsModel).apply {
            font = ArbTheme.monoFont(11f)
            rowHeight = JBUI.scale(24)
        }
        val unanchoredTab = JPanel(BorderLayout()).apply {
            isOpaque = false
            add(unanchoredTruncatedBanner, BorderLayout.NORTH)
            add(JBScrollPane(unanchoredTable).apply { border = JBUI.Borders.empty() }, BorderLayout.CENTER)
        }

        val detailsInner = JPanel().apply {
            layout = BoxLayout(this, BoxLayout.Y_AXIS)
            background = ArbTheme.surfaceBg
            isOpaque = true
            border = JBUI.Borders.empty(10, 12)

            fun addSection(title: String, vararg labels: JLabel) {
                add(JLabel(title).apply {
                    font = ArbTheme.uiFont(11.5f, bold = true)
                    foreground = ArbTheme.primaryText
                    alignmentX = LEFT_ALIGNMENT
                })
                add(Box.createVerticalStrut(JBUI.scale(4)))
                for (lbl in labels) {
                    lbl.alignmentX = LEFT_ALIGNMENT
                    add(lbl)
                    add(Box.createVerticalStrut(JBUI.scale(3)))
                }
                add(Box.createVerticalStrut(JBUI.scale(10)))
            }

            addSection("来歴・ストリーム誠実性 (Tier 3)", provenanceLabel, videoSemanticsLabel, audioOffsetLabel, logcatCompletenessLabel, unanchoredSummaryLabel)
            addSection("内部メタデータ (boottime_ns / Lease / Path)", epochsLabel, leaseAndPathLabel)
            add(discontinuityBar.apply { alignmentX = LEFT_ALIGNMENT })
        }
        val detailsTab = JBScrollPane(detailsInner).apply { border = JBUI.Borders.empty() }

        tabbedPane.addTab("操作＆ログ", syncedTab)
        tabbedPane.addTab("$UNANCHORED_TAB_TITLE (0)", unanchoredTab)
        tabbedPane.addTab("詳細", detailsTab)

        add(tabbedPane, BorderLayout.CENTER)

        wireInteractions()
    }

    private fun wireInteractions() {
        searchField.document.addDocumentListener(
            object : DocumentListener {
                override fun insertUpdate(e: DocumentEvent?) = rebuildSyncedRows()
                override fun removeUpdate(e: DocumentEvent?) = rebuildSyncedRows()
                override fun changedUpdate(e: DocumentEvent?) = rebuildSyncedRows()
            },
        )

        val mouse = object : MouseAdapter() {
            override fun mouseMoved(e: MouseEvent) {
                val idx = syncedEventList.locationToIndex(e.point)
                val nextHover = if (idx in displayedRows.indices && syncedEventList.getCellBounds(idx, idx)?.contains(e.point) == true) idx else -1
                if (nextHover != hoveredRowIndex) {
                    hoveredRowIndex = nextHover
                    syncedEventList.repaint()
                }
                syncedEventList.cursor = if (nextHover >= 0 && isOverInteractiveAction(e, nextHover)) {
                    Cursor.getPredefinedCursor(Cursor.HAND_CURSOR)
                } else {
                    Cursor.getDefaultCursor()
                }
            }

            override fun mouseExited(e: MouseEvent) {
                if (hoveredRowIndex != -1) {
                    hoveredRowIndex = -1
                    syncedEventList.repaint()
                }
            }

            override fun mouseClicked(e: MouseEvent) {
                val idx = syncedEventList.locationToIndex(e.point)
                if (idx !in displayedRows.indices) return
                val bounds = syncedEventList.getCellBounds(idx, idx) ?: return
                if (!bounds.contains(e.point)) return
                val row = displayedRows[idx]

                // Hit-test hover [In] / [Out] snap buttons on right side
                val outXEnd = bounds.x + bounds.width - rightPad
                val outXStart = outXEnd - outBtnWidth
                val inXEnd = outXStart - actionGap
                val inXStart = inXEnd - inBtnWidth

                val rowFrame = row.frameIndex
                if (rowFrame != null && e.x in inXStart..inXEnd) {
                    onSnapInFrame(rowFrame)
                    return
                }
                if (rowFrame != null && e.x in outXStart..outXEnd) {
                    onSnapOutFrame(rowFrame)
                    return
                }

                // Double-click or clicking on a source-jumpable row with Ctrl/Cmd/Shift or source chip triggers PSI Source Jump
                val srcTarget = row.sourceJumpTarget
                if (srcTarget != null && (e.clickCount >= 2 || e.isShiftDown || e.x in (inXStart - JBUI.scale(110)) until inXStart)) {
                    if (e.clickCount >= 2 || e.isShiftDown) {
                        onSourceJump(srcTarget)
                        return
                    }
                    // Single click directly on source badge area triggers jump too
                    if (e.x in (inXStart - JBUI.scale(96)) until (inXStart - JBUI.scale(4))) {
                        onSourceJump(srcTarget)
                        return
                    }
                }

                // Default single click seeks playhead to that event's exact frame
                row.frameIndex?.let(onSeekToFrame)
            }
        }
        syncedEventList.addMouseListener(mouse)
        syncedEventList.addMouseMotionListener(mouse)
    }

    private fun isOverInteractiveAction(e: MouseEvent, idx: Int): Boolean {
        val bounds = syncedEventList.getCellBounds(idx, idx) ?: return false
        val row = displayedRows.getOrNull(idx) ?: return false
        val outXEnd = bounds.x + bounds.width - rightPad
        val inXStart = outXEnd - outBtnWidth - actionGap - inBtnWidth
        if (row.frameIndex != null && e.x in inXStart..outXEnd) return true
        if (row.sourceJumpTarget != null && e.x in (inXStart - JBUI.scale(96)) until inXStart) return true
        return false
    }

    fun setFilter(filter: EventStreamFilter) {
        activeFilter = filter
        for ((f, btn) in filterButtons) {
            ArbTheme.styleFilterChip(btn, active = f == activeFilter)
        }
        rebuildSyncedRows()
    }

    fun setSearchQuery(query: String) {
        searchField.text = query
        rebuildSyncedRows()
    }

    fun selectDetailsTab() {
        tabbedPane.selectedIndex = 2
    }

    fun render(state: TimelineViewerState) {
        currentState = state
        rebuildSyncedRows()
        if (state.unanchoredLogs !== lastUnanchoredRecords) {
            lastUnanchoredRecords = state.unanchoredLogs
            populateUnanchored(state.unanchoredLogs)
            tabbedPane.setTitleAt(1, "$UNANCHORED_TAB_TITLE (${state.unanchoredLogs.size})")
        }

        val truncNotice = state.unanchoredTruncatedNotice
        unanchoredTruncatedBanner.isVisible = truncNotice != null
        unanchoredTruncatedBanner.text = truncNotice.orEmpty()

        val prov = ProvenanceFormatter.format(state.summary)
        provenanceLabel.text = prov.provenanceLine
        videoSemanticsLabel.text = prov.videoSemanticsLine
        audioOffsetLabel.text = prov.audioOffsetLine
        logcatCompletenessLabel.text = prov.logcatCompletenessLine
        unanchoredSummaryLabel.text = prov.unanchoredLine

        val epSummary = if (state.summary.epochs.isEmpty()) {
            "boottime_ns: 情報なし"
        } else {
            state.summary.epochs.joinToString("; ") { ep ->
                "boot=${ep.bootId} [${ep.startBoottimeNs}..${ep.endBoottimeNs} ns]"
            }
        }
        epochsLabel.text = epSummary
        leaseAndPathLabel.text = "Lease=${state.leaseId} | Path=${state.path}"
        discontinuityBar.render(state.discontinuities)
    }

    private var lastUnanchoredRecords: List<LogRecord>? = null
    private var lastStructureKey: List<Any?>? = null
    private var lastFlagKey: List<Any?>? = null

    /**
     * Rows are re-derived (regex parse + tap correlation over up to thousands of records) only when
     * the records, filter, keyword or time mapping change. Per-frame playhead / trim-range changes
     * only re-flag the existing rows, so the list keeps its selection and scroll position.
     */
    private fun rebuildSyncedRows() {
        val s = currentState ?: return
        val keyword = searchField.text
        val structureKey = listOf(
            System.identityHashCode(s.mainLogs), System.identityHashCode(s.mainTaps),
            System.identityHashCode(s.crashes), System.identityHashCode(s.screens),
            System.identityHashCode(s.summary.epochs), System.identityHashCode(s.filmstrip),
            s.frameCount, activeFilter, keyword,
        )
        if (structureKey != lastStructureKey) {
            lastStructureKey = structureKey
            lastFlagKey = listOf(s.currentFrameIndex, s.inFrameIndex, s.outFrameIndex)
            val rows = s.buildSyncedEventRows(filter = activeFilter, keyword = keyword)
            displayedRows = rows
            syncedListModel.clear()
            for (row in rows) {
                syncedListModel.addElement(row)
            }
            return
        }
        val flagKey = listOf(s.currentFrameIndex, s.inFrameIndex, s.outFrameIndex)
        if (flagKey == lastFlagKey) return
        lastFlagKey = flagKey
        val hasRange = s.frameCount > 0L
        val updated = displayedRows.mapIndexed { i, row ->
            val f = row.frameIndex
            val inRange = f == null || !hasRange || f in s.inFrameIndex..s.outFrameIndex
            val atPlayhead = f != null && f == s.currentFrameIndex
            if (row.isInTrimRange == inRange && row.isAtPlayhead == atPlayhead) {
                row
            } else {
                row.copy(isInTrimRange = inRange, isAtPlayhead = atPlayhead).also { syncedListModel.set(i, it) }
            }
        }
        displayedRows = updated
    }

    private fun populateUnanchored(records: List<LogRecord>) {
        unanchoredLogsModel.rowCount = 0
        for (rec in records) {
            val certainty = when (val a = rec.alignment) {
                is TimeAlignment.Unanchored -> "時刻不明 (${a.reason.wire})"
                is TimeAlignment.Bounded -> "区間 (${a.reason.wire})"
                is TimeAlignment.Anchored -> "確定"
            }
            unanchoredLogsModel.addRow(
                arrayOf<Any>(
                    rec.seq,
                    certainty,
                    rec.level ?: "-",
                    rec.tag ?: "-",
                    rec.message,
                ),
            )
        }
    }

    // ---- Testable Accessors ----

    fun syncedRowCount(): Int = displayedRows.size

    fun syncedRows(): List<SyncedEventRow> = displayedRows

    fun mainLogRowCount(): Int = displayedRows.count { it.kind == SyncedEventKind.LOG }

    fun unanchoredLogRowCount(): Int = unanchoredLogsModel.rowCount

    fun isUnanchoredTruncatedBannerVisible(): Boolean = unanchoredTruncatedBanner.isVisible

    fun triggerSnapInAtRow(rowIndex: Int) {
        displayedRows.getOrNull(rowIndex)?.frameIndex?.let(onSnapInFrame)
    }

    fun triggerSnapOutAtRow(rowIndex: Int) {
        displayedRows.getOrNull(rowIndex)?.frameIndex?.let(onSnapOutFrame)
    }

    fun triggerSourceJumpAtRow(rowIndex: Int) {
        displayedRows.getOrNull(rowIndex)?.sourceJumpTarget?.let(onSourceJump)
    }

    private fun techLabel(): JLabel = JLabel("").apply {
        font = ArbTheme.monoFont(11f)
        foreground = ArbTheme.secondaryText
    }

    private inner class SyncedEventCellRenderer : ListCellRenderer<SyncedEventRow> {
        private val root = JPanel(BorderLayout(6, 0)).apply {
            isOpaque = true
        }
        private val timeLabel = JLabel().apply {
            font = ArbTheme.monoFont(10.5f)
            preferredSize = Dimension(JBUI.scale(60), JBUI.scale(20))
        }
        private val kindBadge = JLabel().apply {
            isOpaque = true
            font = ArbTheme.monoFont(9.5f, bold = true)
            border = JBUI.Borders.empty(1, 4)
        }
        private val corrBadge = JLabel().apply {
            isOpaque = true
            background = ArbTheme.infoCyanSoft
            foreground = ArbTheme.infoCyan
            font = ArbTheme.monoFont(9.5f, bold = true)
            border = JBUI.Borders.empty(1, 4)
            isVisible = false
        }
        private val titleLabel = JLabel().apply {
            font = ArbTheme.uiFont(11.5f, bold = true)
        }
        private val detailLabel = JLabel().apply {
            font = ArbTheme.monoFont(10.5f)
        }
        private val sourceChip = JLabel().apply {
            isOpaque = true
            background = ArbTheme.accentBlueSoft
            foreground = ArbTheme.accentBlue
            font = ArbTheme.monoFont(10f)
            border = BorderFactory.createCompoundBorder(
                BorderFactory.createLineBorder(ArbTheme.accentBlue, 1),
                JBUI.Borders.empty(1, 5),
            )
            isVisible = false
        }
        private val inChip = JLabel("[In]").apply {
            isOpaque = true
            background = ArbTheme.elevatedBg
            foreground = ArbTheme.primaryText
            font = ArbTheme.monoFont(10f, bold = true)
            horizontalAlignment = JLabel.CENTER
            preferredSize = Dimension(inBtnWidth, JBUI.scale(20))
            border = BorderFactory.createLineBorder(ArbTheme.hairlineBorder, 1)
        }
        private val outChip = JLabel("[Out]").apply {
            isOpaque = true
            background = ArbTheme.elevatedBg
            foreground = ArbTheme.primaryText
            font = ArbTheme.monoFont(10f, bold = true)
            horizontalAlignment = JLabel.CENTER
            preferredSize = Dimension(outBtnWidth, JBUI.scale(20))
            border = BorderFactory.createLineBorder(ArbTheme.hairlineBorder, 1)
        }
        private val rightPanel = JPanel(FlowLayout(FlowLayout.RIGHT, actionGap, 6)).apply {
            isOpaque = false
            add(sourceChip)
            add(inChip)
            add(outChip)
        }
        private val leftPanel = JPanel(FlowLayout(FlowLayout.LEFT, 4, 6)).apply {
            isOpaque = false
            add(timeLabel)
            add(kindBadge)
            add(corrBadge)
        }
        private val centerPanel = JPanel().apply {
            layout = BoxLayout(this, BoxLayout.Y_AXIS)
            isOpaque = false
            add(titleLabel)
            add(detailLabel)
        }

        init {
            root.add(leftPanel, BorderLayout.WEST)
            root.add(centerPanel, BorderLayout.CENTER)
            root.add(rightPanel, BorderLayout.EAST)
        }

        override fun getListCellRendererComponent(
            list: JList<out SyncedEventRow>,
            value: SyncedEventRow,
            index: Int,
            isSelected: Boolean,
            cellHasFocus: Boolean,
        ): Component {
            val isHovered = index == hoveredRowIndex
            val isCorrelatedLog = value.kind == SyncedEventKind.LOG && value.correlatedTapIndex != null

            // Left accent stripe: solid accent blue when inside trim range; cyan when correlated to a tap; subtle otherwise
            val leftStripeColor: Color = when {
                value.kind == SyncedEventKind.CRASH -> ArbTheme.recRed
                isCorrelatedLog && value.isInTrimRange -> ArbTheme.infoCyan
                value.isInTrimRange -> ArbTheme.accentBlue
                else -> ArbTheme.subtleBorder
            }
            val leftStripeWidth = if (value.isInTrimRange) JBUI.scale(3) else JBUI.scale(1)
            val indentLeft = if (isCorrelatedLog) JBUI.scale(10) else JBUI.scale(4)

            root.border = BorderFactory.createCompoundBorder(
                BorderFactory.createMatteBorder(0, leftStripeWidth, 1, 0, leftStripeColor),
                JBUI.Borders.empty(2, indentLeft, 2, rightPad),
            )

            root.background = when {
                value.isAtPlayhead -> ArbTheme.rowSelectedBg
                isHovered -> ArbTheme.rowHoverBg
                isSelected -> ArbTheme.rowHoverBg
                isCorrelatedLog && value.isInTrimRange -> ArbTheme.infoCyanSoft
                else -> ArbTheme.cardBg
            }

            // Real-time trim range dimming for out-of-range rows
            val primaryFg = if (value.isInTrimRange) ArbTheme.primaryText else ArbTheme.dimmedText
            val secondaryFg = if (value.isInTrimRange) ArbTheme.secondaryText else ArbTheme.dimmedText

            timeLabel.text = value.relativeMs?.let(ArbFormatters::formatTimecode) ?: "--:--.--"
            timeLabel.foreground = secondaryFg

            val (badgeText, badgeBg, badgeFg) = when (value.kind) {
                SyncedEventKind.TAP -> Triple("TAP", ArbTheme.infoCyanSoft, ArbTheme.infoCyan)
                SyncedEventKind.CRASH -> Triple("CRASH", ArbTheme.recRedSoft, ArbTheme.recRed)
                SyncedEventKind.SCREEN_TRANSITION -> Triple("SCR", ArbTheme.accentBlueSoft, ArbTheme.accentBlue)
                SyncedEventKind.LOG -> {
                    val lvl = value.level?.uppercase()
                    when (lvl) {
                        "E", "F" -> Triple(lvl, ArbTheme.recRedSoft, ArbTheme.recRed)
                        "W" -> Triple("W", ArbTheme.warningAmberSoft, ArbTheme.warningAmber)
                        else -> Triple(lvl ?: "I", ArbTheme.elevatedBg, secondaryFg)
                    }
                }
            }
            kindBadge.text = badgeText
            kindBadge.background = badgeBg
            kindBadge.foreground = if (value.isInTrimRange) badgeFg else ArbTheme.dimmedText

            corrBadge.isVisible = isCorrelatedLog
            if (isCorrelatedLog) {
                corrBadge.text = "Tap #${value.correlatedTapIndex}"
            }

            titleLabel.text = value.title
            titleLabel.foreground = primaryFg
            detailLabel.text = value.detail
            detailLabel.foreground = secondaryFg

            val src = value.sourceJumpTarget
            sourceChip.isVisible = src != null
            if (src != null) {
                sourceChip.text = src.displayLabel
            }

            val showSnap = isHovered && value.frameIndex != null
            inChip.isVisible = showSnap
            outChip.isVisible = showSnap

            return root
        }
    }
}
