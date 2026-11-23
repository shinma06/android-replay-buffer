package io.github.shinma06.arb.ui

import com.intellij.openapi.ui.ComboBox
import com.intellij.util.ui.JBUI
import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.ArbConfigPatch
import io.github.shinma06.arb.model.IdeDefaultSaveAction
import java.awt.BorderLayout
import java.awt.Component
import java.awt.FlowLayout
import java.awt.GridBagConstraints
import java.awt.GridBagLayout
import javax.swing.JButton
import javax.swing.JCheckBox
import javax.swing.JDialog
import javax.swing.JLabel
import javax.swing.JPanel
import javax.swing.JSpinner
import javax.swing.SpinnerNumberModel
import javax.swing.SwingUtilities

/**
 * Compact inline configuration dialog triggered from the Cockpit gear button (until T-5 wires
 * IntelliJ's `projectConfigurable`). All values persist to `arbd` via `config.update`.
 */
object ArbQuickConfigDialog {
    fun show(
        parent: Component?,
        current: ArbConfig,
        onSave: (ArbConfigPatch) -> Unit,
    ) {
        val owner = parent?.let { SwingUtilities.getWindowAncestor(it) }
        val dialog = JDialog(owner, "ARB 設定").apply {
            isModal = true
            layout = BorderLayout(8, 8)
        }

        val windowSpinner = JSpinner(
            SpinnerNumberModel(
                current.saveWindowSecs.coerceIn(ArbConfig.MIN_SAVE_WINDOW_SECS, ArbConfig.MAX_SAVE_WINDOW_SECS),
                ArbConfig.MIN_SAVE_WINDOW_SECS,
                ArbConfig.MAX_SAVE_WINDOW_SECS,
                5,
            ),
        )
        val defaultActionCombo = ComboBox(
            arrayOf(
                ActionOption(IdeDefaultSaveAction.SAVE_AND_OPEN, "保存して開く（統合スタジオ）"),
                ActionOption(IdeDefaultSaveAction.SAVE_ONLY, "保存のみ（バックグラウンド）"),
            ),
        ).apply {
            selectedItem = (0 until itemCount).map { getItemAt(it) }
                .firstOrNull { it.action == current.ideDefaultSaveAction }
        }
        val audioCheckbox = JCheckBox("端末音声を録音する (API 33+)", current.audioEnabled)
        val crashCheckbox = JCheckBox("アプリクラッシュ (am_crash) 時に自動保存する", current.crashAutosaveEnabled)
        val maxFpsCombo = ComboBox(arrayOf(15, 30, 60)).apply {
            selectedItem = current.videoMaxFps
        }
        val maxSizeCombo = ComboBox(arrayOf(1024, 1280, 1920)).apply {
            selectedItem = current.videoMaxSize
        }

        val form = JPanel(GridBagLayout()).apply {
            border = JBUI.Borders.empty(12, 14)
            background = ArbTheme.surfaceBg
        }
        val gbc = GridBagConstraints().apply {
            anchor = GridBagConstraints.WEST
            fill = GridBagConstraints.HORIZONTAL
            insets = JBUI.insets(4, 4)
        }

        fun addRow(row: Int, label: String, field: Component) {
            gbc.gridx = 0
            gbc.gridy = row
            gbc.weightx = 0.0
            form.add(JLabel(label).apply { foreground = ArbTheme.primaryText; font = ArbTheme.uiFont(12f) }, gbc)
            gbc.gridx = 1
            gbc.weightx = 1.0
            form.add(field, gbc)
        }

        addRow(0, "保存ウィンドウ（秒: 5〜600s）", windowSpinner)
        addRow(1, "主保存ボタンの動作", defaultActionCombo)
        addRow(2, "映像最大フレームレート (fps)", maxFpsCombo)
        addRow(3, "映像最大長辺 (px)", maxSizeCombo)

        gbc.gridx = 0
        gbc.gridy = 4
        gbc.gridwidth = 2
        form.add(audioCheckbox, gbc)

        gbc.gridy = 5
        form.add(crashCheckbox, gbc)

        val cancelBtn = ArbTheme.styleSecondaryButton(JButton("キャンセル")).apply {
            addActionListener { dialog.dispose() }
        }
        val saveBtn = ArbTheme.stylePrimaryButton(JButton("保存")).apply {
            addActionListener {
                val chosenAction = (defaultActionCombo.selectedItem as? ActionOption)?.action
                    ?: current.ideDefaultSaveAction
                val patch = ArbConfigPatch(
                    saveWindowSecs = (windowSpinner.value as Number).toInt(),
                    ideDefaultSaveAction = chosenAction,
                    audioEnabled = audioCheckbox.isSelected,
                    crashAutosaveEnabled = crashCheckbox.isSelected,
                    videoMaxFps = (maxFpsCombo.selectedItem as? Int) ?: current.videoMaxFps,
                    videoMaxSize = (maxSizeCombo.selectedItem as? Int) ?: current.videoMaxSize,
                )
                dialog.dispose()
                onSave(patch)
            }
        }

        val footer = JPanel(FlowLayout(FlowLayout.RIGHT, 8, 8)).apply {
            background = ArbTheme.surfaceBg
            border = ArbTheme.hairlineTopBorder(6, 10)
            add(cancelBtn)
            add(saveBtn)
        }

        dialog.add(form, BorderLayout.CENTER)
        dialog.add(footer, BorderLayout.SOUTH)
        dialog.pack()
        dialog.setLocationRelativeTo(owner)
        dialog.isVisible = true
    }

    private data class ActionOption(
        val action: IdeDefaultSaveAction,
        val label: String,
    ) {
        override fun toString(): String = label
    }
}
