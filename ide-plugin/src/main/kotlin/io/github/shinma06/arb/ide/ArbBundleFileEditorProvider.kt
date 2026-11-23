package io.github.shinma06.arb.ide

import com.intellij.openapi.fileEditor.FileEditor
import com.intellij.openapi.fileEditor.FileEditorPolicy
import com.intellij.openapi.fileEditor.FileEditorProvider
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.VirtualFile

class ArbBundleFileEditorProvider : FileEditorProvider, DumbAware {
    override fun accept(project: Project, file: VirtualFile): Boolean {
        if (file is ArbBundleVirtualFile) return true
        if (file.fileType == ArbBundleFileType) return true
        return ArbBundleDetector.detectVirtualFile(file) != null
    }

    override fun createEditor(project: Project, file: VirtualFile): FileEditor {
        val service = ArbdDaemonService.getInstance()
        if (file is ArbBundleVirtualFile) {
            return ArbBundleFileEditor(
                project = project,
                backingFile = file,
                bundleIdOrPath = file.bundleIdOrPath,
                displayTitle = file.displayTitle,
                daemonService = service,
            )
        }
        val parentDir = file.parent
        val bundlePath = parentDir?.path ?: file.path
        val title = parentDir?.name ?: file.nameWithoutExtension
        return ArbBundleFileEditor(
            project = project,
            backingFile = file,
            bundleIdOrPath = bundlePath,
            displayTitle = title,
            daemonService = service,
        )
    }

    override fun getEditorTypeId(): String = EDITOR_TYPE_ID

    override fun getPolicy(): FileEditorPolicy = FileEditorPolicy.HIDE_DEFAULT_EDITOR

    companion object {
        const val EDITOR_TYPE_ID: String = "io.github.shinma06.arb.bundle.viewer"
    }
}
