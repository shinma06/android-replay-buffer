package io.github.shinma06.replaybuffer.ui

import com.intellij.openapi.fileTypes.FileType
import com.intellij.openapi.fileTypes.FileTypeRegistry
import com.intellij.openapi.util.io.ByteSequence
import com.intellij.openapi.vfs.VirtualFile
import io.github.shinma06.replaybuffer.core.ReplayFiles
import javax.swing.Icon

internal fun isReplayVideo(file: VirtualFile): Boolean = !file.isDirectory && file.name == ReplayFiles.VIDEO &&
    (hasPendingReplayVideo(file) || file.parent?.let { parent ->
        parent.findChild(ReplayFiles.RECORD)?.isDirectory == false &&
            parent.findChild(ReplayFiles.VIDEO_FRAMES)?.isDirectory == false
    } == true)

internal object ReplayVideoFileType : FileType {
    override fun getName() = "AndroidReplayBufferVideo"
    override fun getDisplayName() = "保存リプレイの動画"
    override fun getDescription() = "Android Replay Bufferの保存動画"
    override fun getDefaultExtension() = ""
    override fun getIcon(): Icon? = null
    override fun isBinary() = true
}

internal class ReplayVideoFileTypeDetector : FileTypeRegistry.FileTypeDetector {
    override fun getDesiredContentPrefixLength() = 0
    override fun detect(file: VirtualFile, firstBytes: ByteSequence, firstCharsIfText: CharSequence?): FileType? =
        ReplayVideoFileType.takeIf { isReplayVideo(file) }
}
