package io.github.shinma06.replaybuffer.core

import java.nio.file.Path

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
        require(partial.parent == complete.parent && partial.isAbsolute && complete.isAbsolute)
        verify(partial)
        HostPlatform.saveHandles.renameExclusive(parent.descriptor, partial.fileName.toString(), parent.descriptor, complete.fileName.toString())
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
