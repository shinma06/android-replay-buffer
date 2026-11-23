package io.github.shinma06.arb.ui

import com.intellij.openapi.Disposable
import com.intellij.openapi.diagnostic.Logger
import io.github.shinma06.arb.ide.ArbdDaemonService
import io.github.shinma06.arb.model.CaptureStartParams
import io.github.shinma06.arb.model.SaveTriggerParams
import io.github.shinma06.arb.state.BundleCatalogState
import io.github.shinma06.arb.state.CaptureBarReducer
import io.github.shinma06.arb.state.CaptureBarState
import io.github.shinma06.arb.state.FailedSaveRecoveryReducer
import io.github.shinma06.arb.state.FailedSaveRecoveryState
import java.awt.BorderLayout
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import javax.swing.BoxLayout
import javax.swing.JPanel
import javax.swing.SwingUtilities

/**
 * Root ToolWindow panel (`ARB v2`) composing the Capture Bar, Failed-Save Recovery panel,
 * and Bundle Catalog panel. All business logic delegates strictly to `arbd` via IPC.
 */
class ArbToolWindowPanel(
    private val daemonService: ArbdDaemonService,
    private val onOpenBundle: (bundleIdOrPath: String, title: String, isLegacy: Boolean) -> Unit,
) : JPanel(BorderLayout(6, 6)), Disposable {
    private val logger = Logger.getInstance(ArbToolWindowPanel::class.java)
    private val executor = Executors.newSingleThreadScheduledExecutor { r ->
        Thread(r, "arb-toolwindow-worker").apply { isDaemon = true }
    }

    private var captureState = CaptureBarState()
    private var recoveryState = FailedSaveRecoveryState()
    private var catalogState = BundleCatalogState()

    val captureBarPanel = CaptureBarPanel(
        onStartCapture = { serial -> runAsync { daemonService.withClient { it.captureStart(CaptureStartParams(serial = serial)) }; refreshAll() } },
        onStopCapture = { runAsync { daemonService.withClient { it.captureStop() }; refreshAll() } },
        onSaveNow = { durationSec ->
            runAsync {
                daemonService.withClient { it.saveTrigger(SaveTriggerParams(durationSec = durationSec)) }
                refreshAll()
            }
        },
        onRefresh = { runAsync { refreshAll() } },
    )

    val failedSaveRecoveryPanel = FailedSaveRecoveryPanel(
        onRetry = { jobId ->
            runAsync {
                updateRecoveryUi(FailedSaveRecoveryReducer.markBusy(recoveryState, jobId))
                val res = daemonService.withClient { it.saveRetry(jobId) }
                updateRecoveryUi(
                    FailedSaveRecoveryReducer.applyJobUpdated(
                        recoveryState,
                        res.job,
                        "再試行結果: ${res.job.state.wire}",
                    ),
                )
                refreshAll()
            }
        },
        onRetarget = { jobId, newDest ->
            runAsync {
                updateRecoveryUi(FailedSaveRecoveryReducer.markBusy(recoveryState, jobId))
                val res = daemonService.withClient { it.saveRetarget(jobId, newDest) }
                updateRecoveryUi(
                    FailedSaveRecoveryReducer.applyJobUpdated(
                        recoveryState,
                        res.job,
                        "保存先変更後の結果: ${res.job.state.wire}",
                    ),
                )
                refreshAll()
            }
        },
        onDiscard = { jobId ->
            runAsync {
                updateRecoveryUi(FailedSaveRecoveryReducer.markBusy(recoveryState, jobId))
                val job = daemonService.withClient { it.saveDiscard(jobId) }
                updateRecoveryUi(
                    FailedSaveRecoveryReducer.applyJobUpdated(
                        recoveryState,
                        job,
                        "保存ジョブを破棄し、ピン留めチャンクを解放しました。",
                    ),
                )
                refreshAll()
            }
        },
    )

    val bundleCatalogPanel = BundleCatalogPanel(
        onOpenBundle = { item -> onOpenBundle(item.bundleId, item.bundleId, item.isLegacyReadOnly) },
        onTrashBundle = { bundleId -> runAsync { daemonService.withClient { it.bundleTrash(bundleId) }; refreshAll() } },
        onRestoreBundle = { bundleId -> runAsync { daemonService.withClient { it.bundleRestore(bundleId) }; refreshAll() } },
        onRunGc = {
            runAsync {
                val gc = daemonService.withClient { it.bundleGc() }
                val note = "GC 完了: 削除=${gc.purgedBundleIds.size}, Lease 保護=${gc.skippedLeasedBundleIds.size}, Legacy 保護=${gc.skippedUnmanagedOrLegacyBundleIds.size}"
                refreshAll(catalogNote = note)
            }
        },
    )

    init {
        val northStack = JPanel().apply {
            layout = BoxLayout(this, BoxLayout.Y_AXIS)
            add(captureBarPanel)
            add(failedSaveRecoveryPanel)
        }
        add(northStack, BorderLayout.NORTH)
        add(bundleCatalogPanel, BorderLayout.CENTER)

        executor.scheduleWithFixedDelay(
            { refreshAll() },
            0L,
            3L,
            TimeUnit.SECONDS,
        )
    }

    private fun runAsync(action: () -> Unit) {
        executor.submit {
            try {
                action()
            } catch (t: Throwable) {
                logger.warn("ARB toolwindow action failed", t)
                val next = CaptureBarReducer.disconnected(captureState, t.message ?: "arbd 通信エラー")
                captureState = next
                SwingUtilities.invokeLater { captureBarPanel.render(next) }
            }
        }
    }

    private fun refreshAll(catalogNote: String? = null) {
        try {
            val status = daemonService.withClient { it.systemStatus() }
            val devices = runCatching {
                daemonService.withClient { it.deviceList().devices }
            }.getOrDefault(captureState.devices)
            val jobs = daemonService.withClient { it.saveListJobs().jobs }
            val bundles = daemonService.withClient { it.bundleList().bundles }

            val nextCapture = CaptureBarReducer.fromStatus(captureState, status, devices)
            val nextRecovery = FailedSaveRecoveryReducer.withJobs(recoveryState, jobs)
            val nextCatalog = BundleCatalogState.fromEntries(bundles)

            captureState = nextCapture
            recoveryState = nextRecovery
            catalogState = nextCatalog

            SwingUtilities.invokeLater {
                captureBarPanel.render(nextCapture)
                failedSaveRecoveryPanel.render(nextRecovery)
                bundleCatalogPanel.render(nextCatalog, catalogNote)
            }
        } catch (t: Throwable) {
            logger.debug("ARB status poll failed: ${t.message}")
            val next = CaptureBarReducer.disconnected(captureState, t.message ?: "arbd 未起動")
            captureState = next
            SwingUtilities.invokeLater { captureBarPanel.render(next) }
        }
    }

    private fun updateRecoveryUi(next: FailedSaveRecoveryState) {
        recoveryState = next
        SwingUtilities.invokeLater { failedSaveRecoveryPanel.render(next) }
    }

    override fun dispose() {
        executor.shutdownNow()
    }
}
