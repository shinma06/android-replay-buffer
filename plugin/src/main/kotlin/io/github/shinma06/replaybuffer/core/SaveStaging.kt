package io.github.shinma06.replaybuffer.core

import java.nio.channels.Channels
import java.nio.channels.FileChannel
import java.nio.file.Path

/**
 * Owned `.<name>.partial` staging under [parent], published without replacement to [parent]/[name].
 *
 * Every output stays handle-verified from creation to publication; failed or cancelled attempts keep
 * their output rather than deleting by name. See SavePublication for the rename-side guarantees.
 */
internal class SaveStaging private constructor(
    val partial: Path,
    val complete: Path,
    private val parent: SaveOutputDirectory,
    private val staging: SaveOutputDirectory,
) : AutoCloseable {
    private var acquired = false
    private val own = linkedMapOf<Path, SaveOutputFile>()
    private var closed = false

    /** Output paths under [partial], in creation order. */
    val outputs: Set<Path> get() = own.keys

    fun file(name: String): Path = partial.resolve(name)

    private fun directoryAt(directory: Path?): Boolean = directory?.let {
        runCatching {
            SaveOutputDirectory.open(directory.parent).use { it.key == parent.key } &&
                parent.openChild(directory.fileName.toString()).use { it.key == staging.key }
        }.getOrDefault(false)
    } ?: false

    /** Only after the first verified output: the attempt's owned staging directory at this path. */
    fun ownsDirectory(directory: Path?): Boolean = acquired && directoryAt(directory)

    fun output(path: Path): SaveOutputFile? = own[path]

    fun checkOwned(path: Path) {
        check(ownsDirectory(path.parent)) { "未完成フォルダーの所有を確認できません" }
        staging.verifyFile(path.fileName.toString(), own.getValue(partial.resolve(path.fileName)))
    }

    fun checkEntries() = staging.requireEntries(own.mapKeys { it.key.fileName.toString() })

    fun createFile(path: Path): FileChannel {
        check(!closed)
        check(directoryAt(partial)) { "未完成フォルダーの所有を確認できません" }
        if (own.isEmpty()) staging.requireEntries(emptyMap())
        val output = staging.createFile(path.fileName.toString())
        own[path] = output
        staging.verifyFile(path.fileName.toString(), output)
        if (!acquired) {
            checkEntries()
            check(directoryAt(partial)) { "未完成フォルダーの所有を確認できません" }
            acquired = true
        }
        return output.channel()
    }

    fun writer(path: Path) = Channels.newWriter(createFile(path), Charsets.UTF_8).buffered()

    /** A still-owned output reopened after [closeForRename]; throws when its identity changed. */
    fun reopenOutput(path: Path): SaveOutputFile {
        val held = own.getValue(path)
        if (!held.isClosed) return held
        return staging.openFile(path.fileName.toString()).also { reopened ->
            if (reopened.key != held.key) { reopened.close(); error("保存出力の実体が変更されています") }
        }
    }

    fun hashes(active: () -> Unit): Map<String, String> =
        own.entries.associate { (path, output) -> active(); checkOwned(path); path.fileName.toString() to output.hash() }

    fun forceEach(active: () -> Unit) {
        own.forEach { (path, output) ->
            checkOwned(path)
            output.channel().use { channel -> channel.force(true) }
            active()
        }
    }

    /** Renaming a directory with open files is not possible everywhere; verification needs only identities. */
    fun closeForRename() {
        if (!SaveOutputDirectory.renamesWithOpenFiles) own.values.forEach(SaveOutputFile::close)
    }

    fun publication() = SavePublication(partial, complete, parent, staging, ::checkEntries)

    override fun close() {
        if (closed) return
        closed = true
        own.values.forEach { it.close() }
        staging.close()
        parent.close()
    }

    companion object {
        fun create(parent: Path, name: String): SaveStaging {
            val handle = SaveOutputDirectory.open(parent)
            try {
                val partial = parent.resolve(".$name.partial")
                return SaveStaging(partial, parent.resolve(name), handle, handle.createDirectory(partial.fileName.toString()))
            } catch (e: Throwable) {
                handle.close()
                throw e
            }
        }
    }
}
