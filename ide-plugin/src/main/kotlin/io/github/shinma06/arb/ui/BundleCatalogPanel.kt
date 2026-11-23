package io.github.shinma06.arb.ui

import com.intellij.icons.AllIcons
import com.intellij.ui.components.JBList
import com.intellij.ui.components.JBScrollPane
import com.intellij.util.ui.JBUI
import io.github.shinma06.arb.model.BundleKind
import io.github.shinma06.arb.state.BundleCatalogItem
import io.github.shinma06.arb.state.BundleCatalogState
import io.github.shinma06.arb.state.BundleFilter
import io.github.shinma06.arb.state.LEGACY_READ_ONLY_BADGE_TEXT
import java.awt.BorderLayout
import java.awt.Component
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
import javax.swing.JMenuItem
import javax.swing.JPanel
import javax.swing.JPopupMenu
import javax.swing.ListCellRenderer
import javax.swing.ListSelectionModel
import javax.swing.SwingUtilities

/**
 * Compact Filterable Library List for saved bundles (IntUI native, zero emoji):
 * - Filter chips: `すべて` / `トリム済み` / `クラッシュ` / `ゴミ箱`
 * - High-density rows (58px): thumbnail slot + title/timestamp + duration in seconds + restrained semantic tag
 * - 1-click opens Integrated Studio; context menu / action strip provides `Finder で表示` / `ゴミ箱へ` / `復元`
 */
class BundleCatalogPanel(
    private val onOpenBundle: (BundleCatalogItem) -> Unit,
    private val onTrashBundle: (bundleId: String) -> Unit,
    private val onRestoreBundle: (bundleId: String) -> Unit,
    private val onRunGc: () -> Unit,
    private val onRevealInFinder: (path: String) -> Unit = {},
) : JPanel(BorderLayout(0, 4)) {
    private val listModel = DefaultListModel<BundleCatalogItem>()
    private val bundleList = JBList(listModel).apply {
        selectionMode = ListSelectionModel.SINGLE_SELECTION
        fixedCellHeight = JBUI.scale(58)
        background = ArbTheme.surfaceBg
        cellRenderer = LibraryRowRenderer()
    }

    private val filterButtons: Map<BundleFilter, JButton> = BundleFilter.entries.associateWith { filter ->
        ArbTheme.styleFilterChip(JButton(filter.label), active = filter == BundleFilter.ALL).apply {
            addActionListener { selectFilter(filter) }
        }
    }

    private val gcButton = ArbTheme.styleSecondaryButton(
        JButton("GC", AllIcons.Actions.GC).apply {
            toolTipText = "猶予期間経過済みのゴミ箱バンドルを整理"
        },
        compact = true,
    )

    private val revealButton = ArbTheme.styleSecondaryButton(
        JButton("Finder で表示", AllIcons.Nodes.Folder),
        compact = true,
    )
    private val trashButton = ArbTheme.styleSecondaryButton(
        JButton("ゴミ箱へ", AllIcons.Actions.Cancel),
        compact = true,
    )
    private val restoreButton = ArbTheme.styleSecondaryButton(
        JButton("復元", AllIcons.Actions.Rollback),
        compact = true,
    )

    private val infoLabel = JLabel("クリックで統合スタジオを開きます").apply {
        font = ArbTheme.uiFont(11f)
        foreground = ArbTheme.secondaryText
    }

    private var fullState = BundleCatalogState()
    private var activeFilter: BundleFilter = BundleFilter.ALL
    private var displayedItems: List<BundleCatalogItem> = emptyList()

    init {
        isOpaque = false
        border = JBUI.Borders.empty(2, 6, 6, 6)

        val filterBar = JPanel(BorderLayout(4, 0)).apply {
            isOpaque = false
            val chips = JPanel(FlowLayout(FlowLayout.LEFT, 4, 2)).apply {
                isOpaque = false
                for (filter in BundleFilter.entries) {
                    add(filterButtons.getValue(filter))
                }
            }
            add(chips, BorderLayout.CENTER)
            add(gcButton, BorderLayout.EAST)
        }

        val scroll = JBScrollPane(bundleList).apply {
            border = BorderFactory.createLineBorder(ArbTheme.hairlineBorder, 1)
            viewport.background = ArbTheme.surfaceBg
        }

        val footerActions = JPanel(FlowLayout(FlowLayout.LEFT, 4, 2)).apply {
            isOpaque = false
            add(revealButton)
            add(trashButton)
            add(restoreButton)
        }

        val footer = JPanel(BorderLayout(4, 2)).apply {
            isOpaque = false
            add(footerActions, BorderLayout.NORTH)
            add(infoLabel, BorderLayout.SOUTH)
        }

        add(filterBar, BorderLayout.NORTH)
        add(scroll, BorderLayout.CENTER)
        add(footer, BorderLayout.SOUTH)

        wireInteractions()
        updateButtonStates()
    }

    private fun wireInteractions() {
        bundleList.addListSelectionListener { updateButtonStates() }

        bundleList.addMouseListener(
            object : MouseAdapter() {
                override fun mouseClicked(e: MouseEvent) {
                    val idx = bundleList.locationToIndex(e.point)
                    if (idx !in displayedItems.indices) return
                    bundleList.selectedIndex = idx
                    val item = displayedItems[idx]
                    if (SwingUtilities.isLeftMouseButton(e) && e.clickCount == 2) {
                        onOpenBundle(item)
                    }
                }

                override fun mousePressed(e: MouseEvent) = maybeShowPopup(e)
                override fun mouseReleased(e: MouseEvent) = maybeShowPopup(e)

                private fun maybeShowPopup(e: MouseEvent) {
                    if (!e.isPopupTrigger) return
                    val idx = bundleList.locationToIndex(e.point)
                    if (idx !in displayedItems.indices) return
                    bundleList.selectedIndex = idx
                    val item = displayedItems[idx]
                    val menu = JPopupMenu().apply {
                        add(JMenuItem("スタジオで開く", AllIcons.Actions.Execute).apply {
                            addActionListener { onOpenBundle(item) }
                        })
                        add(JMenuItem("Finder で表示", AllIcons.Nodes.Folder).apply {
                            addActionListener { onRevealInFinder(item.path) }
                        })
                        if (item.canTrash) {
                            add(JMenuItem("ゴミ箱へ", AllIcons.Actions.Cancel).apply {
                                addActionListener { onTrashBundle(item.bundleId) }
                            })
                        }
                        if (item.canRestore) {
                            add(JMenuItem("復元", AllIcons.Actions.Rollback).apply {
                                addActionListener { onRestoreBundle(item.bundleId) }
                            })
                        }
                    }
                    menu.show(e.component, e.x, e.y)
                }
            },
        )

        revealButton.addActionListener { selectedItem()?.let { onRevealInFinder(it.path) } }
        trashButton.addActionListener { selectedItem()?.let { onTrashBundle(it.bundleId) } }
        restoreButton.addActionListener { selectedItem()?.let { onRestoreBundle(it.bundleId) } }
        gcButton.addActionListener { onRunGc() }
    }

    fun selectFilter(filter: BundleFilter) {
        activeFilter = filter
        for ((f, btn) in filterButtons) {
            ArbTheme.styleFilterChip(btn, active = f == activeFilter)
        }
        rebuildList(preserveSelectionId = selectedItem()?.bundleId)
    }

    fun render(state: BundleCatalogState, statusNote: String? = null) {
        fullState = state
        updateFilterChipLabels(state)
        rebuildList(preserveSelectionId = selectedItem()?.bundleId)
        if (statusNote != null) {
            infoLabel.text = statusNote
        }
    }

    private fun updateFilterChipLabels(state: BundleCatalogState) {
        for (filter in BundleFilter.entries) {
            val count = state.countFor(filter)
            val btn = filterButtons.getValue(filter)
            btn.text = if (count > 0 && filter != BundleFilter.ALL) "${filter.label} ($count)" else filter.label
            ArbTheme.styleFilterChip(btn, active = filter == activeFilter)
        }
    }

    private fun rebuildList(preserveSelectionId: String?) {
        val next = fullState.copy(activeFilter = activeFilter).filteredItems
        displayedItems = next
        listModel.clear()
        for (item in next) {
            listModel.addElement(item)
        }
        if (preserveSelectionId != null) {
            val idx = next.indexOfFirst { it.bundleId == preserveSelectionId }
            if (idx >= 0) {
                bundleList.selectedIndex = idx
            }
        }
        updateButtonStates()
    }

    // ---- Testable Accessors (backwards compatible + new T-4 assertions) ----

    fun rowCount(): Int = listModel.size()

    fun badgeAtRow(row: Int): String = displayedItems[row].formatBadge

    fun itemAtRow(row: Int): BundleCatalogItem = displayedItems[row]

    fun activeFilter(): BundleFilter = activeFilter

    fun filterChipLabels(): List<String> = BundleFilter.entries.map { filterButtons.getValue(it).text }

    private fun selectedItem(): BundleCatalogItem? {
        val row = bundleList.selectedIndex
        return if (row in displayedItems.indices) displayedItems[row] else null
    }

    private fun updateButtonStates() {
        val item = selectedItem()
        revealButton.isEnabled = item != null
        trashButton.isEnabled = item?.canTrash == true
        restoreButton.isEnabled = item?.canRestore == true
    }

    private class LibraryRowRenderer : ListCellRenderer<BundleCatalogItem> {
        private val root = JPanel(BorderLayout(8, 0)).apply {
            isOpaque = true
            border = ArbTheme.hairlineBottomBorder(6, 8)
        }
        private val thumbSlot = JLabel(AllIcons.Actions.Execute).apply {
            horizontalAlignment = JLabel.CENTER
            preferredSize = Dimension(JBUI.scale(44), JBUI.scale(38))
            isOpaque = true
            background = ArbTheme.elevatedBg
            border = BorderFactory.createLineBorder(ArbTheme.hairlineBorder, 1)
        }
        private val titleLabel = JLabel().apply {
            font = ArbTheme.uiFont(12f, bold = true)
            foreground = ArbTheme.primaryText
        }
        private val metaLabel = JLabel().apply {
            font = ArbTheme.monoFont(10.5f)
            foreground = ArbTheme.secondaryText
        }
        private val tagsPanel = JPanel(FlowLayout(FlowLayout.RIGHT, 4, 0)).apply {
            isOpaque = false
        }

        init {
            val center = JPanel().apply {
                layout = BoxLayout(this, BoxLayout.Y_AXIS)
                isOpaque = false
                add(titleLabel)
                add(Box.createVerticalStrut(JBUI.scale(2)))
                add(metaLabel)
            }
            root.add(thumbSlot, BorderLayout.WEST)
            root.add(center, BorderLayout.CENTER)
            root.add(tagsPanel, BorderLayout.EAST)
        }

        override fun getListCellRendererComponent(
            list: JList<out BundleCatalogItem>,
            value: BundleCatalogItem,
            index: Int,
            isSelected: Boolean,
            cellHasFocus: Boolean,
        ): Component {
            root.background = if (isSelected) ArbTheme.rowSelectedBg else ArbTheme.cardBg
            thumbSlot.icon = when {
                value.hasCrash -> AllIcons.General.Error
                value.bundleKind == BundleKind.LOGS_ONLY -> AllIcons.FileTypes.Text
                value.isTrimmed -> AllIcons.Actions.Edit
                else -> AllIcons.Actions.Execute
            }
            titleLabel.text = value.displayTitle
            val timeShort = value.createdAtUtc.replace('T', ' ').removeSuffix("Z").take(19)
            metaLabel.text = "$timeShort · ${value.durationText}"

            tagsPanel.removeAll()
            val tags = value.semanticTags.ifEmpty { listOf(value.formatBadge) }
            for (tag in tags.take(2)) {
                val (bg, fg) = when (tag) {
                    "クラッシュ" -> ArbTheme.recRedSoft to ArbTheme.recRed
                    "トリム済み" -> ArbTheme.accentBlueSoft to ArbTheme.accentBlue
                    LEGACY_READ_ONLY_BADGE_TEXT -> ArbTheme.warningAmberSoft to ArbTheme.warningAmber
                    else -> ArbTheme.elevatedBg to ArbTheme.secondaryText
                }
                tagsPanel.add(
                    JLabel(tag).apply {
                        isOpaque = true
                        background = bg
                        foreground = fg
                        font = ArbTheme.uiFont(10f, bold = true)
                        border = BorderFactory.createCompoundBorder(
                            BorderFactory.createLineBorder(ArbTheme.hairlineBorder, 1),
                            JBUI.Borders.empty(1, 5),
                        )
                    },
                )
            }
            return root
        }
    }
}
