package io.github.shinma06.replaybuffer.core

import com.sun.jna.Library
import com.sun.jna.Native
import java.nio.file.Path

/** Use the IDE's existing JNA, not another runtime dependency or a bundled native binary. */
internal interface ExclusiveRename : Library {
    fun renameatx_np(sourceParent: Int, source: String, destinationParent: Int, destination: String, flags: Int): Int
    fun renameat2(sourceParent: Int, source: String, destinationParent: Int, destination: String, flags: Int): Int
}

private val os = System.getProperty("os.name")
private val exclusiveRename by lazy {
    Native.load("c", ExclusiveRename::class.java, mapOf(Library.OPTION_STRING_ENCODING to "UTF-8"))
}

private fun checkExclusiveRename() = check(os == "Mac OS X" || os == "Linux" || os.startsWith("Windows")) { "この環境は原子・非上書き保存に対応していません" }

/**
 * Bounded ownership observations surround descriptor-relative, exclusive publication on macOS, Linux and Windows.
 *
 * The caller keeps parent/staging handles alive. Verification can fail after rename, so an exception
 * does not imply the complete path is absent; the writer must retain and assess the actual output.
 */
internal class SavePublication(
    val partial: Path,
    val complete: Path,
    private val parent: SaveOutputDirectory,
    private val staging: SaveOutputDirectory,
    private val checkEntries: () -> Unit,
) {
    fun publish() {
        checkExclusiveRename()
        require(partial.parent == complete.parent && partial.isAbsolute && complete.isAbsolute)
        verify(partial)
        renameExclusive(parent.descriptor, partial.fileName.toString(), parent.descriptor, complete.fileName.toString())
        verify(complete)
    }

    private fun verify(path: Path) {
        check(SaveOutputDirectory.open(path.parent).use { it.key == parent.key } &&
            parent.openChild(path.fileName.toString()).use { it.key == staging.key }) {
            "未完成フォルダーの所有を確認できません"
        }
        checkEntries()
        // shortcut: observations do not exclude arbitrary later same-UID edits; stronger isolation needs a separate process boundary.
    }
}

internal fun renameExclusive(sourceParent: Long, source: String, destinationParent: Long, destination: String) {
    checkExclusiveRename()
    if (os.startsWith("Windows")) return WindowsSaveHandles.renameExclusive(sourceParent, source, destinationParent, destination)
    // RENAME_EXCL (Darwin) / RENAME_NOREPLACE (Linux): fail instead of replacing any existing target.
    val result = try {
        if (os == "Mac OS X") exclusiveRename.renameatx_np(sourceParent.toInt(), source, destinationParent.toInt(), destination, 0x4)
        else exclusiveRename.renameat2(sourceParent.toInt(), source, destinationParent.toInt(), destination, 0x1)
    } catch (_: LinkageError) { error("原子・非上書き保存を利用できません") }
    check(result == 0) { "完成先の公開に失敗しました（既存出力・対応filesystem・アクセス権を確認してください）" }
}
