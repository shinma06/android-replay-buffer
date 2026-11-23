package io.github.shinma06.arb.ui

import io.github.shinma06.arb.state.BundleCatalogItem
import io.github.shinma06.arb.state.BundleCatalogState
import java.awt.BorderLayout
import java.awt.FlowLayout
import javax.swing.BorderFactory
import javax.swing.JButton
import javax.swing.JLabel
import javax.swing.JPanel
import javax.swing.JScrollPane
import javax.swing.JTable
import javax.swing.ListSelectionModel
import javax.swing.table.DefaultTableModel

/**
 * Bundle catalog panel listing v2 WORM bundles and read-only Legacy (Schema 3/4) bundles
 * clearly badged `Legacy (RO)`, with safe Trash / Restore / GC and Viewer open actions.
 */
class BundleCatalogPanel(
    private val onOpenBundle: (BundleCatalogItem) -> Unit,
    private val onTrashBundle: (bundleId: String) -> Unit,
    private val onRestoreBundle: (bundleId: String) -> Unit,
    private val onRunGc: () -> Unit,
) : JPanel(BorderLayout(6, 6)) {
    private val tableModel = object : DefaultTableModel(
        arrayOf("ID", "形式", "状態", "Lease", "作成時刻 (UTC)", "パス"),
        0,
    ) {
        override fun isCellEditable(row: Int, column: Int): Boolean = false
    }
    private val table = JTable(tableModel).apply {
        selectionModel.selectionMode = ListSelectionModel.SINGLE_SELECTION
    }
    private val openButton = JButton("ビューアで開く")
    private val trashButton = JButton("ゴミ箱へ移動 (Trash)")
    private val restoreButton = JButton("復元 (Restore)")
    private val gcButton = JButton("不要バンドルを GC")
    private val infoLabel = JLabel("開いているバンドルは Viewer Lease により物理削除から保護されます。")

    private var visibleItems: List<BundleCatalogItem> = emptyList()

    init {
        border = BorderFactory.createTitledBorder("保存済みバンドル一覧 (v2 & Legacy RO)")

        val toolbar = JPanel(FlowLayout(FlowLayout.LEFT, 6, 4)).apply {
            add(openButton)
            add(trashButton)
            add(restoreButton)
            add(gcButton)
        }

        add(toolbar, BorderLayout.NORTH)
        add(JScrollPane(table), BorderLayout.CENTER)
        add(infoLabel, BorderLayout.SOUTH)

        table.selectionModel.addListSelectionListener { updateButtonStates() }

        openButton.addActionListener { selectedItem()?.let(onOpenBundle) }
        trashButton.addActionListener { selectedItem()?.let { onTrashBundle(it.bundleId) } }
        restoreButton.addActionListener { selectedItem()?.let { onRestoreBundle(it.bundleId) } }
        gcButton.addActionListener { onRunGc() }

        updateButtonStates()
    }

    fun render(state: BundleCatalogState, statusNote: String? = null) {
        val prevId = selectedItem()?.bundleId
        visibleItems = state.visibleItems
        tableModel.rowCount = 0
        for (item in visibleItems) {
            val leaseText = if (item.isProtectedByLease) "${item.activeLeases} (保護中)" else "0"
            tableModel.addRow(
                arrayOf(
                    item.bundleId,
                    item.formatBadge,
                    item.state.wire,
                    leaseText,
                    item.createdAtUtc,
                    item.path,
                ),
            )
        }
        if (prevId != null) {
            val idx = visibleItems.indexOfFirst { it.bundleId == prevId }
            if (idx >= 0) {
                table.setRowSelectionInterval(idx, idx)
            }
        }
        if (statusNote != null) {
            infoLabel.text = statusNote
        }
        updateButtonStates()
    }

    fun rowCount(): Int = tableModel.rowCount

    fun badgeAtRow(row: Int): String = tableModel.getValueAt(row, 1).toString()

    private fun selectedItem(): BundleCatalogItem? {
        val row = table.selectedRow
        return if (row in visibleItems.indices) visibleItems[row] else null
    }

    private fun updateButtonStates() {
        val item = selectedItem()
        openButton.isEnabled = item != null
        trashButton.isEnabled = item?.canTrash == true
        restoreButton.isEnabled = item?.canRestore == true
    }
}
