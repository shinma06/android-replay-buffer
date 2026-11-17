package io.github.shinma06.replaybuffer.ui

import com.intellij.ide.AppLifecycleListener
import com.intellij.ide.plugins.DynamicPluginListener
import com.intellij.ide.plugins.IdeaPluginDescriptor
import com.intellij.openapi.Disposable
import com.intellij.openapi.extensions.PluginId
import com.intellij.openapi.actionSystem.ActionManager
import com.intellij.openapi.actionSystem.ActionUiKind
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.AnActionResult
import com.intellij.openapi.actionSystem.IdeActions
import com.intellij.openapi.actionSystem.ex.AnActionListener
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.fileEditor.FileEditorManagerListener
import com.intellij.openapi.project.Project
import com.intellij.openapi.project.ProjectManager
import com.intellij.openapi.project.ProjectManagerListener
import com.intellij.openapi.ui.Messages
import com.intellij.openapi.vfs.VirtualFile
import com.intellij.openapi.vfs.VirtualFilePreCloseCheck
import io.github.shinma06.replaybuffer.core.ReplayEditOrigin
import io.github.shinma06.replaybuffer.ide.ReplayProjectService

internal class ReplayEditClosePolicy {
    var origin: ReplayEditOrigin? = null
    @Volatile var trimExported = false
    @Volatile var ambiguousOutput = false
    fun eligible(healthy: Boolean, busy: Boolean, closing: Boolean) =
        origin != null && healthy && !busy && !closing && !trimExported && !ambiguousOutput
}

internal fun confirmedReplayDiscard(eligible: Boolean, active: () -> Boolean, ask: () -> Boolean): Boolean =
    eligible && active() && runCatching { ask() && active() }.getOrDefault(false)

internal class ReplayEditCloseLifecycle {
    @Volatile var closing = false
    fun beforePluginUnload(id: PluginId) {
        if (id.idString == "io.github.shinma06.android-replay-buffer") closing = true
    }
}

/** The SDK calls this on checked closes (tab X / Close Editor), before beforeFileClosed. */
internal class ReplayVideoPreCloseCheck : VirtualFilePreCloseCheck {
    override fun canCloseFile(file: VirtualFile): Boolean {
        ProjectManager.getInstance().openProjects.filterNot { it.isDisposed }.forEach {
            runCatching { it.getServiceIfCreated(ReplayProjectService::class.java)?.confirmReplayClose(file) }
        }
        return true
    }
}

/** The file-close events, rather than dispose(), own this optional user decision. */
internal class ReplayEditCloseManager(private val project: Project, private val service: ReplayProjectService) : Disposable {
    private val lifecycle = ReplayEditCloseLifecycle()
    val closing get() = lifecycle.closing
    private val editors = mutableMapOf<VirtualFile, ReplayPlayerEditor>()
    private val confirmed = mutableMapOf<VirtualFile, Pair<ReplayPlayerEditor, ReplayEditOrigin>>()
    private val actions = mutableListOf<Pair<AnActionEvent, Boolean>>()
    private var revision = 0L

    init {
        val connection = project.messageBus.connect(this)
        connection.subscribe(FileEditorManagerListener.Before.FILE_EDITOR_MANAGER, object : FileEditorManagerListener.Before {
            override fun beforeFileOpened(source: FileEditorManager, file: VirtualFile) { clear(file) }
            override fun beforeFileClosed(source: FileEditorManager, file: VirtualFile) {
                // SDK CloseContent (Ctrl+F4 / Cmd+W) uses EditorTabs.close -> unchecked closeFile.
                // Only its live user action may use this fallback; tab eviction/replacement must retain.
                if (actions.lastOrNull()?.second == true) confirmClose(file)
                editors.remove(file)?.controller?.closePolicy?.origin = null
                if (!active()) confirmed.remove(file)
            }
        })
        connection.subscribe(FileEditorManagerListener.FILE_EDITOR_MANAGER, object : FileEditorManagerListener {
            override fun fileClosed(source: FileEditorManager, file: VirtualFile) {
                val (editor, origin) = confirmed.remove(file) ?: return
                if (!active() || source.isFileOpen(file)) return
                editor.controller.resourcesClosed.thenAccept { closed ->
                    if (closed && active()) service.relocateEditedReplay(origin) { active() }
                }
            }
        })
        val app = ApplicationManager.getApplication().messageBus.connect(this)
        app.subscribe(AnActionListener.TOPIC, object : AnActionListener {
            override fun beforeActionPerformed(action: AnAction, event: AnActionEvent) {
                val id = ActionManager.getInstance().getId(action)
                actions += event to (event.project === project &&
                    id == IdeActions.ACTION_CLOSE &&
                    (event.inputEvent != null || event.uiKind != ActionUiKind.NONE))
            }
            override fun afterActionPerformed(action: AnAction, event: AnActionEvent, result: AnActionResult) {
                actions.removeAll { it.first === event }
            }
        })
        app.subscribe(ProjectManager.TOPIC, object : ProjectManagerListener {
            override fun projectClosingBeforeSave(project: Project) { if (project === this@ReplayEditCloseManager.project) lifecycle.closing = true }
            override fun projectClosing(project: Project) { if (project === this@ReplayEditCloseManager.project) lifecycle.closing = true }
        })
        app.subscribe(AppLifecycleListener.TOPIC, object : AppLifecycleListener {
            override fun appClosing() { lifecycle.closing = true }
            override fun beforeAppWillBeClosed(isRestart: Boolean) { lifecycle.closing = true }
        })
        app.subscribe(DynamicPluginListener.TOPIC, object : DynamicPluginListener {
            override fun beforePluginUnload(pluginDescriptor: IdeaPluginDescriptor, isUpdate: Boolean) { lifecycle.beforePluginUnload(pluginDescriptor.pluginId) }
        })
    }

    private fun active() = !closing && !project.isDisposed && project.isOpen && !ApplicationManager.getApplication().isDisposeInProgress
    fun confirmClose(file: VirtualFile) {
        val editor = editors[file] ?: return
        val controller = editor.controller
        val origin = controller.closePolicy.origin
        val state = controller.state
        val eligible = controller.closePolicy.eligible(!state.loading && state.error == null && state.image != null,
            state.editor.task != null, !active()) && FileEditorManager.getInstance(project).getAllEditors(file).size == 1
        clear(file)
        val decisionRevision = revision
        if (origin == null || !confirmedReplayDiscard(eligible, { active() && revision == decisionRevision }) {
                Messages.showDialog(project,
                    "リプレイ本体も残しますか？\nPNGは保存先に残します。「残さない」は本体を復元可能な保管場所へ移します。自動削除はしません。",
                    "リプレイ本体の扱い", arrayOf("残す", "残さない（復元可能な場所へ移す）"), 0, Messages.getQuestionIcon()) == 1
            }) return
        val latest = controller.state
        if (controller.closePolicy.trimExported || controller.closePolicy.ambiguousOutput ||
            latest.loading || latest.error != null || latest.image == null || latest.editor.task != null ||
            FileEditorManager.getInstance(project).getAllEditors(file).singleOrNull() !== editor || controller.resourcesClosed.isDone) return
        val decision = editor to origin
        confirmed[file] = decision
        // Another pre-close checker may veto. The selected SDK closes synchronously; an unused decision expires.
        ApplicationManager.getApplication().invokeLater { if (confirmed[file] === decision) confirmed.remove(file) }
    }
    fun attach(file: VirtualFile, editor: ReplayPlayerEditor, origin: ReplayEditOrigin?) {
        clear(file)
        editor.controller.closePolicy.origin = origin
        if (origin != null && active()) editors[file] = editor
    }
    fun clear(file: VirtualFile) { revision++; editors.remove(file)?.controller?.closePolicy?.origin = null; confirmed.remove(file) }
    override fun dispose() { lifecycle.closing = true; editors.values.forEach { it.controller.closePolicy.origin = null }; editors.clear(); confirmed.clear() }
}
