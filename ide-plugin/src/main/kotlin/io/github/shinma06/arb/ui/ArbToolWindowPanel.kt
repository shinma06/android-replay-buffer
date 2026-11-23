package io.github.shinma06.arb.ui

import com.intellij.ide.actions.RevealFileAction
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.Disposable
import com.intellij.openapi.diagnostic.Logger
import io.github.shinma06.arb.ide.ArbdDaemonService
import io.github.shinma06.arb.model.ArbConfig
import io.github.shinma06.arb.model.BundleKind
import io.github.shinma06.arb.model.CaptureStartParams
import io.github.shinma06.arb.model.SaveLogsParams
import io.github.shinma06.arb.model.SaveScreenshotParams
import io.github.shinma06.arb.model.SaveTriggerParams
import io.github.shinma06.arb.state.ArbFormatters
import io.github.shinma06.arb.state.BundleCatalogState
import io.github.shinma06.arb.state.CaptureBarReducer
import io.github.shinma06.arb.state.CaptureBarState
import io.github.shinma06.arb.state.FailedSaveRecoveryReducer
import io.github.shinma06.arb.state.FailedSaveRecoveryState
import java.awt.BorderLayout
import java.nio.file.Path
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import javax.swing.BoxLayout
import javax.swing.JPanel
import javax.swing.SwingUtilities

/**
 * Root ToolWindow panel (`ARB`) composing the Live Capture Cockpit Card, conditional Tier-2
 * banners / Failed-Save Recovery panel, and Compact Filterable Library List.
 * All business logic delegates strictly to `arbd` via IPC.
 */
class ArbToolWindowPanel(
    private val daemonService: ArbdDaemonService,
    private val onOpenBundle: (bundleIdOrPath: String, title: String, isLegacy: Boolean) -> Unit,
) : JPanel(BorderLayout(0, 4)), Disposable {
    private val logger = Logger.getInstance(ArbToolWindowPanel::class.java)
    private val executor = Executors.newSingleThreadScheduledExecutor { r ->
        Thread(r, "arb-toolwindow-worker").apply { isDaemon = true }
    }

    private var captureState = CaptureBarState()
    private var recoveryState = FailedSaveRecoveryState()
    private var catalogState = BundleCatalogState()

    val captureBarPanel: CaptureBarPanel = CaptureBarPanel(
        onStartCapture = { serial ->
            runAsync {
                daemonService.withClient { it.captureStart(CaptureStartParams(serial = serial)) }
                refreshAll()
            }
        },
        onStopCapture = {
            runAsync {
                daemonService.withClient { it.captureStop() }
                refreshAll()
            }
        },
        onSaveNow = { durationSec -> triggerSave(durationSec, openEditor = true) },
        onRefresh = { runAsync { refreshAll() } },
        onSaveAndOpen = { durationSec -> triggerSave(durationSec, openEditor = true) },
        onSaveOnly = { durationSec -> triggerSave(durationSec, openEditor = false) },
        onScreenshot = { serial ->
            runAsync {
                val res = daemonService.withClient {
                    it.saveScreenshot(SaveScreenshotParams(serial = serial))
                }
                notifyBalloon("スクリーンショットを保存しました: ${res.path} (${res.width}x${res.height})")
                refreshAll()
            }
        },
        onSaveLogs = { durationSec ->
            runAsync {
                val res = daemonService.withClient {
                    it.saveLogs(SaveLogsParams(windowSecs = durationSec))
                }
                val detail = res.bundleId ?: res.job.state.wire
                notifyBalloon("直前 ${ArbFormatters.formatSeconds(durationSec)} のログを保存しました: $detail")
                refreshAll()
            }
        },
        onOpenConfig = {
            SwingUtilities.invokeLater {
                ArbQuickConfigDialog.show(this, captureState.config) { patch ->
                    runAsync {
                        val updated = daemonService.withClient { it.configUpdate(patch) }.config
                        captureState = CaptureBarReducer.withConfig(captureState, updated)
                        SwingUtilities.invokeLater { captureBarPanel.render(captureState) }
                        notifyBalloon(
                            "設定を保存しました（保持ウィンドウ: ${ArbFormatters.formatSeconds(updated.saveWindowSecs)}）",
                        )
                        refreshAll()
                    }
                }
            }
        },
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
        onOpenBundle = { item -> onOpenBundle(item.bundleId, item.displayTitle, item.isLegacyReadOnly) },
        onTrashBundle = { bundleId ->
            runAsync {
                daemonService.withClient { it.bundleTrash(bundleId) }
                refreshAll(catalogNote = "ゴミ箱へ移動しました（ゴミ箱フィルタから復元可）")
            }
        },
        onRestoreBundle = { bundleId ->
            runAsync {
                daemonService.withClient { it.bundleRestore(bundleId) }
                refreshAll(catalogNote = "バンドルを復元しました")
            }
        },
        onRunGc = {
            runAsync {
                val gc = daemonService.withClient { it.bundleGc() }
                val note = "GC 完了: 削除=${gc.purgedBundleIds.size}, Lease 保護=${gc.skippedLeasedBundleIds.size}, Legacy 保護=${gc.skippedUnmanagedOrLegacyBundleIds.size}"
                refreshAll(catalogNote = note)
            }
        },
        onRevealInFinder = { path ->
            runCatching { RevealFileAction.openFile(Path.of(path)) }
                .onFailure { logger.warn("Failed to reveal $path", it) }
        },
    )

    init {
        background = ArbTheme.surfaceBg
        isOpaque = true

        val northStack = JPanel().apply {
            layout = BoxLayout(this, BoxLayout.Y_AXIS)
            isOpaque = false
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

    private fun triggerSave(durationSec: Long, openEditor: Boolean) {
        runAsync {
            val res = daemonService.withClient {
                it.saveTrigger(SaveTriggerParams(durationSec = durationSec))
            }
            refreshAll()
            val savedId = res.bundleId
            if (savedId != null) {
                if (openEditor) {
                    SwingUtilities.invokeLater {
                        onOpenBundle(savedId, savedId, false)
                    }
                } else {
                    notifyBalloon("直前 ${ArbFormatters.formatSeconds(durationSec)} を保存しました: $savedId")
                }
            } else {
                notifyBalloon(
                    "保存ジョブ状態: ${res.job.state.wire} (${res.job.lastError ?: "詳細を確認してください"})",
                    NotificationType.WARNING,
                )
            }
        }
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
            val config = runCatching {
                daemonService.withClient { it.configGet().config }
            }.getOrDefault(captureState.config)
            val devices = runCatching {
                daemonService.withClient { it.deviceList().devices }
            }.getOrDefault(captureState.devices)
            val jobs = daemonService.withClient { it.saveListJobs().jobs }
            val bundles = daemonService.withClient { it.bundleList().bundles }

            val crashIds = mutableSetOf<String>()
            val durations = mutableMapOf<String, Long>()
            val kinds = mutableMapOf<String, BundleKind>()

            for (entry in bundles.take(24)) {
                runCatching {
                    val summary = daemonService.withClient { it.timelineSummary(entry.bundleId) }
                    kinds[entry.bundleId] = summary.bundleKind
                    val spanNs = summary.epochs.firstOrNull()?.let { ep ->
                        (ep.endBoottimeNs - ep.startBoottimeNs).coerceAtLeast(0L)
                    }
                    val secs = when {
                        spanNs != null && spanNs > 0L -> (spanNs / 1_000_000_000L).coerceAtLeast(1L)
                        summary.frameCount > 0 -> (summary.frameCount / config.videoMaxFps.coerceAtLeast(1)).toLong().coerceAtLeast(1L)
                        else -> null
                    }
                    if (secs != null) durations[entry.bundleId] = secs
                }
                runCatching {
                    val crashes = daemonService.withClient { it.timelineCrashes(entry.bundleId).crashes }
                    if (crashes.isNotEmpty()) crashIds.add(entry.bundleId)
                }
            }

            val nextCapture = CaptureBarReducer.fromStatus(
                previous = captureState,
                status = status,
                devices = devices,
                config = config,
            )
            val nextRecovery = FailedSaveRecoveryReducer.withJobs(recoveryState, jobs)
            val nextCatalog = BundleCatalogState.fromEntries(
                entries = bundles,
                activeFilter = bundleCatalogPanel.activeFilter(),
                crashBundleIds = crashIds,
                durationsByBundleId = durations,
                kindsByBundleId = kinds,
            )

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

    private fun notifyBalloon(content: String, type: NotificationType = NotificationType.INFORMATION) {
        runCatching {
            NotificationGroupManager.getInstance()
                .getNotificationGroup("ARB v2")
                .createNotification("Android Replay Buffer", content, type)
                .notify(null)
        }.onFailure {
            logger.info("ARB notification: $content")
        }
    }

    override fun dispose() {
        executor.shutdownNow()
    }
}
