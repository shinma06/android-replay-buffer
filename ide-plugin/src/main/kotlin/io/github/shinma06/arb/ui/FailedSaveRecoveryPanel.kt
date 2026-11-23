package io.github.shinma06.arb.ui

import com.intellij.icons.AllIcons
import com.intellij.util.ui.JBUI
import io.github.shinma06.arb.state.FailedSaveRecoveryState
import java.awt.BorderLayout
import java.awt.FlowLayout
import javax.swing.BorderFactory
import javax.swing.Box
import javax.swing.BoxLayout
import javax.swing.JButton
import javax.swing.JFileChooser
import javax.swing.JLabel
import javax.swing.JPanel

/**
 * Tier-2 conditional recovery banner for `failed_retryable` save jobs whose ring chunks remain
 * pinned. Height 0 (`isVisible = false`) when healthy; exposes inline `再試行` / `保存先変更` / `破棄`.
 */
class FailedSaveRecoveryPanel(
    private val onRetry: (jobId: String) -> Unit,
    private val onRetarget: (jobId: String, newDestDir: String) -> Unit,
    private val onDiscard: (jobId: String) -> Unit,
) : JPanel(BorderLayout(0, 4)) {
    private val listContainer = JPanel().apply {
        layout = BoxLayout(this, BoxLayout.Y_AXIS)
        isOpaque = false
    }
    private val messageLabel = JLabel("").apply {
        font = ArbTheme.uiFont(11f)
        foreground = ArbTheme.secondaryText
    }

    private var currentState = FailedSaveRecoveryState()

    init {
        isOpaque = false
        border = JBUI.Borders.empty(2, 6, 4, 6)
        add(listContainer, BorderLayout.CENTER)
        add(messageLabel, BorderLayout.SOUTH)
        isVisible = false
    }

    fun render(state: FailedSaveRecoveryState) {
        currentState = state
        val retryable = state.retryableJobs
        isVisible = retryable.isNotEmpty() || !state.lastMessage.isNullOrBlank()
        messageLabel.isVisible = !state.lastMessage.isNullOrBlank()
        messageLabel.text = state.lastMessage.orEmpty()

        listContainer.removeAll()
        for (job in retryable) {
            val isBusy = state.busyJobId == job.jobId
            val row = JPanel(BorderLayout(6, 4)).apply {
                background = ArbTheme.recRedSoft
                isOpaque = true
                border = BorderFactory.createCompoundBorder(
                    BorderFactory.createMatteBorder(0, JBUI.scale(3), 0, 0, ArbTheme.recRed),
                    BorderFactory.createCompoundBorder(
                        BorderFactory.createLineBorder(ArbTheme.hairlineBorder, 1),
                        JBUI.Borders.empty(6, 8),
                    ),
                )
                val header = JLabel(
                    "保存失敗 (${job.jobId}) — チャンク ${job.pinnedChunkIds.size} 件を保護中",
                    AllIcons.General.Warning,
                    JLabel.LEFT,
                ).apply {
                    font = ArbTheme.uiFont(11.5f, bold = true)
                    foreground = ArbTheme.primaryText
                }
                val err = JLabel("保存先: ${job.destinationPath} | ${job.lastError ?: "書き込みエラー"}").apply {
                    font = ArbTheme.uiFont(11f)
                    foreground = ArbTheme.secondaryText
                }
                val textBox = JPanel().apply {
                    layout = BoxLayout(this, BoxLayout.Y_AXIS)
                    isOpaque = false
                    add(header)
                    add(err)
                }

                val retryBtn = ArbTheme.styleSecondaryButton(
                    JButton("再試行", AllIcons.Actions.Refresh),
                    compact = true,
                ).apply {
                    isEnabled = !isBusy
                    addActionListener { onRetry(job.jobId) }
                }
                val retargetBtn = ArbTheme.styleSecondaryButton(
                    JButton("保存先変更", AllIcons.Nodes.Folder),
                    compact = true,
                ).apply {
                    isEnabled = !isBusy
                    addActionListener {
                        val chooser = JFileChooser().apply {
                            fileSelectionMode = JFileChooser.DIRECTORIES_ONLY
                            dialogTitle = "新しい保存先ディレクトリを選択"
                        }
                        if (chooser.showOpenDialog(this@FailedSaveRecoveryPanel) == JFileChooser.APPROVE_OPTION) {
                            onRetarget(job.jobId, chooser.selectedFile.absolutePath)
                        }
                    }
                }
                val discardBtn = ArbTheme.styleSecondaryButton(
                    JButton("破棄", AllIcons.Actions.Cancel),
                    compact = true,
                ).apply {
                    isEnabled = !isBusy
                    addActionListener { onDiscard(job.jobId) }
                }

                val buttons = JPanel(FlowLayout(FlowLayout.RIGHT, 4, 0)).apply {
                    isOpaque = false
                    add(retryBtn)
                    add(retargetBtn)
                    add(discardBtn)
                }

                add(textBox, BorderLayout.CENTER)
                add(buttons, BorderLayout.SOUTH)
            }
            listContainer.add(row)
            listContainer.add(Box.createVerticalStrut(JBUI.scale(4)))
        }
        revalidate()
        repaint()
    }

    fun displayedRetryableCount(): Int = currentState.retryableJobs.size
}
