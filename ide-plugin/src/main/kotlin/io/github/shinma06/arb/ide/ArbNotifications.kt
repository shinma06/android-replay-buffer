package io.github.shinma06.arb.ide

import com.intellij.ide.actions.RevealFileAction
import com.intellij.notification.NotificationAction
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.diagnostic.Logger
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.ide.CopyPasteManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.text.StringUtil
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.openapi.wm.ToolWindowManager
import io.github.shinma06.arb.state.ArbFormatters
import io.github.shinma06.arb.ui.ArbIcons
import java.awt.Desktop
import java.awt.Toolkit
import java.awt.datatransfer.StringSelection
import java.nio.file.Files
import java.nio.file.Path
import javax.swing.SwingUtilities

enum class ArbNotificationActionId(val icon: ArbIcons) {
    OPEN_VIEWER(ArbIcons.OpenViewer),
    OPEN_IMAGE(ArbIcons.OpenImage),
    OPEN_FOLDER(ArbIcons.OpenFolder),
    COPY_PATH(ArbIcons.CopyPath),
    MOVE_TO_TRASH(ArbIcons.Trash),
    UNDO_TRASH(ArbIcons.Restore),
    RESTORE_ORIGINAL(ArbIcons.Restore),
    OPEN_ARB_PANEL(ArbIcons.Panel),
}

data class ArbNotificationActionSpec(
    val id: ArbNotificationActionId,
    val label: String,
)

data class ArbNotificationSpec(
    val title: String,
    val content: String,
    val type: NotificationType,
    val actions: List<ArbNotificationActionSpec>,
)

/**
 * Unified IntelliJ balloon notification builder & dispatcher shared across Tool Window, Studio
 * FileEditor, and Running Devices / Tools Menu actions.
 *
 * Guarantees:
 * - Zero emoji and zero unescaped HTML in dynamic fields (paths, bundle IDs, error details).
 * - Rich 1-click actions (`ビューアで開く`, `画像を開く`, `フォルダを開く`, `パスをコピー`,
 *   `ゴミ箱へ` + `元に戻す`, `元バンドルを復元`, `ARB パネルを開く`).
 * - Disposed-project safe and off-EDT for all file-system / daemon I/O.
 * - Image trash uses OS trash (`Desktop.moveToTrash`) ONLY — never permanently deletes files.
 */
object ArbNotifications {
    const val NOTIFICATION_GROUP_ID: String = "Android Replay Buffer"
    const val NOTIFICATION_TITLE: String = "Android Replay Buffer"
    const val TOOL_WINDOW_ID: String = "ARB"

    private val logger = Logger.getInstance(ArbNotifications::class.java)

    /** Escapes dynamic strings inserted into IntelliJ notification HTML content. */
    fun escapeNotificationHtml(raw: String): String = StringUtil.escapeXmlEntities(raw)

    // ---------------- Pure spec builders (unit-tested headlessly) ----------------

    fun buildReplaySavedSpec(
        bundleId: String,
        bundlePath: String?,
        windowSecs: Long,
        openedInStudio: Boolean,
        warning: String? = null,
    ): ArbNotificationSpec {
        val dur = ArbFormatters.formatSeconds(windowSecs)
        val safeId = escapeNotificationHtml(bundleId)
        val warnSuffix = warning?.takeIf { it.isNotBlank() }?.let { " (${escapeNotificationHtml(it)})" }.orEmpty()
        val body = if (openedInStudio) {
            "直前 $dur を保存してビューアで開きました: $safeId$warnSuffix"
        } else {
            "直前 $dur を保存しました: $safeId$warnSuffix"
        }
        val actions = buildList {
            if (!openedInStudio) {
                add(ArbNotificationActionSpec(ArbNotificationActionId.OPEN_VIEWER, "ビューアで開く"))
            }
            if (!bundlePath.isNullOrBlank()) {
                add(ArbNotificationActionSpec(ArbNotificationActionId.OPEN_FOLDER, "フォルダを開く"))
                add(ArbNotificationActionSpec(ArbNotificationActionId.COPY_PATH, "パスをコピー"))
            }
            add(ArbNotificationActionSpec(ArbNotificationActionId.MOVE_TO_TRASH, "ゴミ箱へ"))
        }
        return ArbNotificationSpec(
            title = NOTIFICATION_TITLE,
            content = body,
            type = NotificationType.INFORMATION,
            actions = actions,
        )
    }

    fun buildLogsOnlySavedSpec(
        bundleId: String,
        bundlePath: String?,
        windowSecs: Long,
        warning: String? = null,
    ): ArbNotificationSpec {
        val dur = ArbFormatters.formatSeconds(windowSecs)
        val safeId = escapeNotificationHtml(bundleId)
        val warnSuffix = warning?.takeIf { it.isNotBlank() }?.let { " (${escapeNotificationHtml(it)})" }.orEmpty()
        val actions = buildList {
            add(ArbNotificationActionSpec(ArbNotificationActionId.OPEN_VIEWER, "ビューアで開く"))
            if (!bundlePath.isNullOrBlank()) {
                add(ArbNotificationActionSpec(ArbNotificationActionId.OPEN_FOLDER, "フォルダを開く"))
                add(ArbNotificationActionSpec(ArbNotificationActionId.COPY_PATH, "パスをコピー"))
            }
            add(ArbNotificationActionSpec(ArbNotificationActionId.MOVE_TO_TRASH, "ゴミ箱へ"))
        }
        return ArbNotificationSpec(
            title = NOTIFICATION_TITLE,
            content = "直前 $dur のログのみ保存しました: $safeId$warnSuffix",
            type = NotificationType.INFORMATION,
            actions = actions,
        )
    }

    fun buildImageSavedSpec(
        imagePath: String,
        width: Int,
        height: Int,
        kindLabel: String = "スクリーンショット",
    ): ArbNotificationSpec {
        val fileName = runCatching { Path.of(imagePath).fileName?.toString() }.getOrNull() ?: imagePath
        val safeName = escapeNotificationHtml(fileName)
        val safeKind = escapeNotificationHtml(kindLabel)
        return ArbNotificationSpec(
            title = NOTIFICATION_TITLE,
            content = "${safeKind}を保存しました: $safeName (${width}x${height})",
            type = NotificationType.INFORMATION,
            actions = listOf(
                ArbNotificationActionSpec(ArbNotificationActionId.OPEN_IMAGE, "画像を開く"),
                ArbNotificationActionSpec(ArbNotificationActionId.OPEN_FOLDER, "フォルダを開く"),
                ArbNotificationActionSpec(ArbNotificationActionId.COPY_PATH, "パスをコピー"),
                ArbNotificationActionSpec(ArbNotificationActionId.MOVE_TO_TRASH, "ゴミ箱へ"),
            ),
        )
    }

    fun buildExportCompletedSpec(
        summaryText: String,
        outputPath: String,
        trimmedBundleId: String? = null,
        originalTrashedBundleId: String? = null,
    ): ArbNotificationSpec {
        val actions = buildList {
            if (outputPath.isNotBlank()) {
                add(ArbNotificationActionSpec(ArbNotificationActionId.OPEN_FOLDER, "フォルダを開く"))
                add(ArbNotificationActionSpec(ArbNotificationActionId.COPY_PATH, "パスをコピー"))
            }
            if (!trimmedBundleId.isNullOrBlank()) {
                add(ArbNotificationActionSpec(ArbNotificationActionId.OPEN_VIEWER, "ビューアで開く"))
            }
            if (!originalTrashedBundleId.isNullOrBlank()) {
                add(ArbNotificationActionSpec(ArbNotificationActionId.RESTORE_ORIGINAL, "元バンドルを復元"))
            }
        }
        return ArbNotificationSpec(
            title = NOTIFICATION_TITLE,
            content = escapeNotificationHtml(summaryText),
            type = NotificationType.INFORMATION,
            actions = actions,
        )
    }

    fun buildActionFailedSpec(
        operationLabel: String,
        detail: String?,
    ): ArbNotificationSpec {
        val safeOp = escapeNotificationHtml(operationLabel)
        val safeDetail = escapeNotificationHtml(detail?.takeIf { it.isNotBlank() } ?: "詳細は ARB パネルを確認してください")
        return ArbNotificationSpec(
            title = NOTIFICATION_TITLE,
            content = "${safeOp}に失敗しました: $safeDetail",
            type = NotificationType.ERROR,
            actions = listOf(
                ArbNotificationActionSpec(ArbNotificationActionId.OPEN_ARB_PANEL, "ARB パネルを開く"),
            ),
        )
    }

    fun buildTrashedUndoSpec(bundleId: String): ArbNotificationSpec = ArbNotificationSpec(
        title = NOTIFICATION_TITLE,
        content = "ゴミ箱へ移しました: ${escapeNotificationHtml(bundleId)}",
        type = NotificationType.INFORMATION,
        actions = listOf(
            ArbNotificationActionSpec(ArbNotificationActionId.UNDO_TRASH, "元に戻す"),
        ),
    )

    // ---------------- IDE Notification Dispatchers ----------------

    fun notifyReplaySaved(
        project: Project?,
        daemonService: ArbdDaemonService,
        bundleId: String,
        bundlePath: String?,
        windowSecs: Long,
        openedInStudio: Boolean,
        warning: String? = null,
    ) {
        val spec = buildReplaySavedSpec(bundleId, bundlePath, windowSecs, openedInStudio, warning)
        emit(
            project = project,
            spec = spec,
            onAction = { actionId ->
                when (actionId) {
                    ArbNotificationActionId.OPEN_VIEWER -> openBundleInViewer(project, bundleId)
                    ArbNotificationActionId.OPEN_FOLDER -> bundlePath?.let(::openPathInFileManager)
                    ArbNotificationActionId.COPY_PATH -> bundlePath?.let(::copyTextToClipboard)
                    ArbNotificationActionId.MOVE_TO_TRASH -> trashBundleWithUndo(project, daemonService, bundleId)
                    else -> Unit
                }
            },
        )
    }

    fun notifyLogsOnlySaved(
        project: Project?,
        daemonService: ArbdDaemonService,
        bundleId: String,
        bundlePath: String?,
        windowSecs: Long,
        warning: String? = null,
    ) {
        val spec = buildLogsOnlySavedSpec(bundleId, bundlePath, windowSecs, warning)
        emit(
            project = project,
            spec = spec,
            onAction = { actionId ->
                when (actionId) {
                    ArbNotificationActionId.OPEN_VIEWER -> openBundleInViewer(project, bundleId)
                    ArbNotificationActionId.OPEN_FOLDER -> bundlePath?.let(::openPathInFileManager)
                    ArbNotificationActionId.COPY_PATH -> bundlePath?.let(::copyTextToClipboard)
                    ArbNotificationActionId.MOVE_TO_TRASH -> trashBundleWithUndo(project, daemonService, bundleId)
                    else -> Unit
                }
            },
        )
    }

    fun notifyImageSaved(
        project: Project?,
        imagePath: String,
        width: Int,
        height: Int,
        kindLabel: String = "スクリーンショット",
    ) {
        val spec = buildImageSavedSpec(imagePath, width, height, kindLabel)
        emit(
            project = project,
            spec = spec,
            onAction = { actionId ->
                when (actionId) {
                    ArbNotificationActionId.OPEN_IMAGE -> openImageFile(project, imagePath)
                    ArbNotificationActionId.OPEN_FOLDER -> openPathInFileManager(imagePath)
                    ArbNotificationActionId.COPY_PATH -> copyTextToClipboard(imagePath)
                    ArbNotificationActionId.MOVE_TO_TRASH -> moveImageToOsTrashSafely(project, imagePath)
                    else -> Unit
                }
            },
        )
    }

    fun notifyExportCompleted(
        project: Project?,
        daemonService: ArbdDaemonService,
        summaryText: String,
        outputPath: String,
        trimmedBundleId: String? = null,
        originalTrashedBundleId: String? = null,
    ) {
        val spec = buildExportCompletedSpec(
            summaryText = summaryText,
            outputPath = outputPath,
            trimmedBundleId = trimmedBundleId,
            originalTrashedBundleId = originalTrashedBundleId,
        )
        emit(
            project = project,
            spec = spec,
            onAction = { actionId ->
                when (actionId) {
                    ArbNotificationActionId.OPEN_FOLDER -> openPathInFileManager(outputPath)
                    ArbNotificationActionId.COPY_PATH -> copyTextToClipboard(outputPath)
                    ArbNotificationActionId.OPEN_VIEWER -> trimmedBundleId?.let { openBundleInViewer(project, it) }
                    ArbNotificationActionId.RESTORE_ORIGINAL -> originalTrashedBundleId?.let {
                        restoreBundleAsync(project, daemonService, it)
                    }
                    else -> Unit
                }
            },
        )
    }

    fun notifyActionFailed(
        project: Project?,
        operationLabel: String,
        detail: String?,
    ) {
        val spec = buildActionFailedSpec(operationLabel, detail)
        emit(
            project = project,
            spec = spec,
            onAction = { actionId ->
                if (actionId == ArbNotificationActionId.OPEN_ARB_PANEL) {
                    activateArbToolWindow(project)
                }
            },
        )
    }

    fun notifyInfo(project: Project?, message: String) {
        emit(
            project = project,
            spec = ArbNotificationSpec(
                title = NOTIFICATION_TITLE,
                content = escapeNotificationHtml(message),
                type = NotificationType.INFORMATION,
                actions = emptyList(),
            ),
            onAction = {},
        )
    }

    fun notifyWarning(project: Project?, message: String, includeOpenPanel: Boolean = false) {
        val actions = if (includeOpenPanel) {
            listOf(ArbNotificationActionSpec(ArbNotificationActionId.OPEN_ARB_PANEL, "ARB パネルを開く"))
        } else {
            emptyList()
        }
        emit(
            project = project,
            spec = ArbNotificationSpec(
                title = NOTIFICATION_TITLE,
                content = escapeNotificationHtml(message),
                type = NotificationType.WARNING,
                actions = actions,
            ),
            onAction = { actionId ->
                if (actionId == ArbNotificationActionId.OPEN_ARB_PANEL) {
                    activateArbToolWindow(project)
                }
            },
        )
    }

    /** Opens a bundle directory or file in the host file manager off the EDT. */
    fun openPathInFileManager(rawPath: String) {
        runOffEdt {
            runCatching {
                val path = Path.of(rawPath)
                if (Files.isDirectory(path)) {
                    RevealFileAction.openDirectory(path)
                } else {
                    RevealFileAction.openFile(path)
                }
            }.onFailure { err ->
                logger.warn("Failed to open path in file manager: $rawPath", err)
            }
        }
    }

    fun copyTextToClipboard(text: String) {
        runCatching {
            CopyPasteManager.getInstance().setContents(StringSelection(text))
        }.onFailure {
            runCatching {
                Toolkit.getDefaultToolkit().systemClipboard.setContents(StringSelection(text), null)
            }
        }
    }

    private fun emit(
        project: Project?,
        spec: ArbNotificationSpec,
        onAction: (ArbNotificationActionId) -> Unit,
    ) {
        if (project != null && project.isDisposed) return
        runCatching {
            val notification = NotificationGroupManager.getInstance()
                .getNotificationGroup(NOTIFICATION_GROUP_ID)
                .createNotification(spec.title, spec.content, spec.type)
            for (actionSpec in spec.actions) {
                notification.addAction(
                    NotificationAction.createSimpleExpiring(actionSpec.label) {
                        if (project?.isDisposed != true) onAction(actionSpec.id)
                    }.apply { templatePresentation.icon = actionSpec.id.icon.swing },
                )
            }
            notification.notify(project)
        }.onFailure {
            logger.info("ARB notification [${spec.type}]: ${spec.content}")
        }
    }

    private fun openBundleInViewer(project: Project?, bundleId: String) {
        if (project == null || project.isDisposed) return
        runOnEdt {
            if (!project.isDisposed) {
                val vf = ArbBundleVirtualFile(bundleId, bundleId, isLegacyHint = false)
                FileEditorManager.getInstance(project).openFile(vf, true)
            }
        }
    }

    /**
     * Why [imagePath] cannot be opened (blank / malformed path, or the file is gone), or `null`
     * when it can be tried. The user always gets this as a warning instead of a click that does nothing.
     */
    internal fun imagePathProblem(imagePath: String): String? {
        if (imagePath.isBlank()) return "画像のパスが空のため開けません。"
        val path = runCatching { Path.of(imagePath) }.getOrNull()
            ?: return "画像のパスが不正なため開けません: $imagePath"
        if (!Files.isRegularFile(path)) {
            return "画像ファイルが見つかりません（移動または削除された可能性があります）: $imagePath"
        }
        return null
    }

    private fun openImageFile(project: Project?, imagePath: String) {
        runOffEdt {
            val problem = imagePathProblem(imagePath)
            if (problem != null) {
                logger.warn("ARB cannot open image: $problem ($imagePath)")
                notifyWarning(project, problem)
                return@runOffEdt
            }
            val path = Path.of(imagePath)
            val vFile = runCatching { LocalFileSystem.getInstance().refreshAndFindFileByNioFile(path) }.getOrNull()
            if (vFile != null && project != null && !project.isDisposed) {
                runOnEdt {
                    if (!project.isDisposed) {
                        FileEditorManager.getInstance(project).openFile(vFile, true)
                    }
                }
                return@runOffEdt
            }
            val openedDesktop = runCatching {
                if (Desktop.isDesktopSupported() && Desktop.getDesktop().isSupported(Desktop.Action.OPEN)) {
                    Desktop.getDesktop().open(path.toFile())
                    true
                } else {
                    false
                }
            }.getOrDefault(false)
            if (!openedDesktop) {
                openPathInFileManager(imagePath)
            }
        }
    }

    private fun trashBundleWithUndo(
        project: Project?,
        daemonService: ArbdDaemonService,
        bundleId: String,
    ) {
        runOffEdt {
            try {
                daemonService.withClient { it.bundleTrash(bundleId) }
                daemonService.notifyCatalogChanged()
                emit(
                    project = project,
                    spec = buildTrashedUndoSpec(bundleId),
                    onAction = { actionId ->
                        if (actionId == ArbNotificationActionId.UNDO_TRASH) {
                            restoreBundleAsync(project, daemonService, bundleId)
                        }
                    },
                )
            } catch (t: Throwable) {
                logger.warn("Failed to trash bundle $bundleId from notification", t)
                notifyActionFailed(project, "ゴミ箱への移動", t.message)
            }
        }
    }

    private fun restoreBundleAsync(
        project: Project?,
        daemonService: ArbdDaemonService,
        bundleId: String,
    ) {
        runOffEdt {
            try {
                daemonService.withClient { it.bundleRestore(bundleId) }
                daemonService.notifyCatalogChanged()
                notifyInfo(project, "バンドルを復元しました: $bundleId")
            } catch (t: Throwable) {
                logger.warn("Failed to restore bundle $bundleId from notification", t)
                notifyActionFailed(project, "バンドルの復元", t.message)
            }
        }
    }

    private fun moveImageToOsTrashSafely(project: Project?, imagePath: String) {
        runOffEdt {
            val path = runCatching { Path.of(imagePath) }.getOrNull()
            if (path == null || !Files.exists(path)) {
                notifyWarning(project, "対象ファイルが見つかりませんでした。")
                return@runOffEdt
            }
            val moved = runCatching {
                Desktop.isDesktopSupported() &&
                    Desktop.getDesktop().isSupported(Desktop.Action.MOVE_TO_TRASH) &&
                    Desktop.getDesktop().moveToTrash(path.toFile())
            }.getOrDefault(false)
            if (moved) {
                notifyInfo(project, "画像を OS のゴミ箱へ移しました: ${path.fileName}")
            } else {
                // Never permanently delete when OS trash is unavailable!
                notifyWarning(project, "OS のゴミ箱機能を利用できませんでした。ファイルは削除していません。")
            }
        }
    }

    private fun activateArbToolWindow(project: Project?) {
        if (project == null || project.isDisposed) return
        runOnEdt {
            if (!project.isDisposed) {
                ToolWindowManager.getInstance(project).getToolWindow(TOOL_WINDOW_ID)?.activate(null)
            }
        }
    }

    private fun runOffEdt(block: () -> Unit) {
        val app = ApplicationManager.getApplication()
        if (app != null) {
            app.executeOnPooledThread(block)
        } else {
            block()
        }
    }

    private fun runOnEdt(block: () -> Unit) {
        if (SwingUtilities.isEventDispatchThread()) {
            block()
        } else {
            SwingUtilities.invokeLater(block)
        }
    }
}
