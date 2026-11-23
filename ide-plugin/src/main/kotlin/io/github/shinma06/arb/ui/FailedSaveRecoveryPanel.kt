package io.github.shinma06.arb.ui

import io.github.shinma06.arb.state.FailedSaveRecoveryState
import java.awt.BorderLayout
import java.awt.Color
import java.awt.FlowLayout
import java.awt.Font
import javax.swing.BorderFactory
import javax.swing.BoxLayout
import javax.swing.JButton
import javax.swing.JFileChooser
import javax.swing.JLabel
import javax.swing.JPanel

/**
 * Recovery panel for `failed_retryable` save jobs whose ring chunks remain pinned.
 * Provides explicit **再試行 (Retry)**, **保存先を変更 (Change Destination)**, and **破棄 (Discard)** controls.
 */
class FailedSaveRecoveryPanel(
    private val onRetry: (jobId: String) -> Unit,
    private val onRetarget: (jobId: String, newDestDir: String) -> Unit,
    private val onDiscard: (jobId: String) -> Unit,
) : JPanel(BorderLayout(4, 4)) {
    private val listContainer = JPanel().apply {
        layout = BoxLayout(this, BoxLayout.Y_AXIS)
    }
    private val messageLabel = JLabel("")

    private var currentState = FailedSaveRecoveryState()

    init {
        border = BorderFactory.createTitledBorder("保存失敗リカバリ（チャンク Pin 保護中）")
        add(listContainer, BorderLayout.CENTER)
        add(messageLabel, BorderLayout.SOUTH)
        isVisible = false
    }

    fun render(state: FailedSaveRecoveryState) {
        currentState = state
        val retryable = state.retryableJobs
        isVisible = retryable.isNotEmpty() || !state.lastMessage.isNullOrBlank()
        messageLabel.text = state.lastMessage.orEmpty()

        listContainer.removeAll()
        for (job in retryable) {
            val isBusy = state.busyJobId == job.jobId
            val row = JPanel(BorderLayout(6, 4)).apply {
                border = BorderFactory.createCompoundBorder(
                    BorderFactory.createLineBorder(Color(0xB9, 0x1C, 0x1C)),
                    BorderFactory.createEmptyBorder(6, 8, 6, 8),
                )
                val header = JLabel("ジョブ ${job.jobId} — Pin 済みチャンク: ${job.pinnedChunkIds.size} 件").apply {
                    font = font.deriveFont(Font.BOLD)
                }
                val err = JLabel("保存先: ${job.destinationPath} | 原因: ${job.lastError ?: "書き込みエラー"}")
                val textBox = JPanel().apply {
                    layout = BoxLayout(this, BoxLayout.Y_AXIS)
                    add(header)
                    add(err)
                }

                val retryBtn = JButton("再試行 (Retry)").apply {
                    isEnabled = !isBusy
                    addActionListener { onRetry(job.jobId) }
                }
                val retargetBtn = JButton("保存先を変更 (Change Destination)").apply {
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
                val discardBtn = JButton("破棄 (Discard)").apply {
                    isEnabled = !isBusy
                    addActionListener { onDiscard(job.jobId) }
                }

                val buttons = JPanel(FlowLayout(FlowLayout.RIGHT, 4, 0)).apply {
                    add(retryBtn)
                    add(retargetBtn)
                    add(discardBtn)
                }

                add(textBox, BorderLayout.CENTER)
                add(buttons, BorderLayout.SOUTH)
            }
            listContainer.add(row)
        }
        revalidate()
        repaint()
    }

    fun displayedRetryableCount(): Int = currentState.retryableJobs.size
}
