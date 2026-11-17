package io.github.shinma06.replaybuffer.ui

import com.intellij.ide.AppLifecycleListener
import com.intellij.ide.plugins.DynamicPluginListener
import com.intellij.ide.plugins.IdeaPluginDescriptor
import com.intellij.openapi.Disposable
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
    @Volatile var closing = false
        private set
    private val editors = mutableMapOf<VirtualFile, ReplayPlayerEditor>()
    private val confirmed = mutableMapOf<VirtualFile, Pair<ReplayPlayerEditor, ReplayEditOrigin>>()
    private var revision = 0L

    init {
        val connection = project.messageBus.connect(this)
        connection.subscribe(FileEditorManagerListener.Before.FILE_EDITOR_MANAGER, object : FileEditorManagerListener.Before {
            override fun beforeFileOpened(source: FileEditorManager, file: VirtualFile) { clear(file) }
            override fun beforeFileClosed(source: FileEditorManager, file: VirtualFile) {
                // Unchecked/programmatic closes have no decision. Never prompt here or in dispose().
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
        app.subscribe(ProjectManager.TOPIC, object : ProjectManagerListener {
            override fun projectClosingBeforeSave(project: Project) { if (project === this@ReplayEditCloseManager.project) closing = true }
            override fun projectClosing(project: Project) { if (project === this@ReplayEditCloseManager.project) closing = true }
        })
        app.subscribe(AppLifecycleListener.TOPIC, object : AppLifecycleListener {
            override fun appClosing() { closing = true }
            override fun beforeAppWillBeClosed(isRestart: Boolean) { closing = true }
        })
        app.subscribe(DynamicPluginListener.TOPIC, object : DynamicPluginListener {
            override fun beforePluginUnload(pluginDescriptor: IdeaPluginDescriptor, isUpdate: Boolean) { closing = true }
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
    override fun dispose() { closing = true; editors.values.forEach { it.controller.closePolicy.origin = null }; editors.clear(); confirmed.clear() }
}
