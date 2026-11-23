package io.github.shinma06.arb.ide

import com.intellij.openapi.fileEditor.FileEditor
import com.intellij.openapi.fileEditor.FileEditorPolicy
import com.intellij.openapi.fileEditor.FileEditorProvider
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.VirtualFile

class ArbBundleFileEditorProvider : FileEditorProvider, DumbAware {
    override fun accept(project: Project, file: VirtualFile): Boolean = file is ArbBundleVirtualFile

    override fun createEditor(project: Project, file: VirtualFile): FileEditor {
        val arbFile = file as ArbBundleVirtualFile
        return ArbBundleFileEditor(project, arbFile, ArbdDaemonService.getInstance())
    }

    override fun getEditorTypeId(): String = EDITOR_TYPE_ID

    override fun getPolicy(): FileEditorPolicy = FileEditorPolicy.HIDE_DEFAULT_EDITOR

    companion object {
        const val EDITOR_TYPE_ID: String = "io.github.shinma06.arb.bundle.viewer"
    }
}
