package io.github.shinma06.replaybuffer.core

import java.nio.ByteBuffer
import java.nio.channels.FileChannel
import java.nio.file.OpenOption
import java.nio.file.Path
import java.nio.file.StandardOpenOption.READ
import java.nio.file.StandardOpenOption.WRITE

/**
 * Handle-relative filesystem behind [SaveOutputDirectory] and [SaveOutputFile]: every child operation is relative to a
 * held directory handle and never follows links. One implementation per OS, chosen by [HostPlatform].
 */
internal interface SaveHandles {
    /** Root directory of the absolute, normalized [path]. */
    fun openRoot(path: Path): Long
    fun openDirectory(parent: Long, name: String): Long
    /** Creates an owner-only directory and opens it. */
    fun createDirectory(parent: Long, name: String): Long
    fun openFile(parent: Long, name: String): Long
    /** Creates an owner-only file exclusively, open for reading and writing. */
    fun createFile(parent: Long, name: String): Long
    /** Device/inode-like identity; also rejects links and the wrong kind of item. */
    fun key(handle: Long, directory: Boolean): Pair<Long, Long>
    /** Renames without replacing an existing destination. */
    fun renameExclusive(sourceParent: Long, source: String, destinationParent: Long, destination: String)
    fun deleteFile(parent: Long, name: String)
    fun deleteEmptyDirectory(parent: Long, name: String)
    fun flush(directory: Long)
    fun entries(directory: Long): Set<String>
    fun channel(handle: Long, options: Set<OpenOption>): FileChannel
    fun requirePrivate(handle: Long, directory: Boolean)
    fun close(handle: Long)
    /** Rejects a single name that the OS would reinterpret; the portable rules are checked by the caller. */
    fun requireName(name: String) {}
    /** False where a directory cannot be renamed while files inside it are open. */
    val renamesDirectoryWithOpenFiles: Boolean
}

private val handles get() = HostPlatform.saveHandles

private fun childName(name: String) {
    require(name.isNotEmpty() && name != "." && name != ".." && '/' !in name && '\u0000' !in name)
    handles.requireName(name)
}

/**
 * Holds one directory descriptor; child operations accept a single name and key is device/inode identity.
 *
 * Open walks a normalized absolute path without following symlinks. Close releases the handle only;
 * the caller owns created output and must keep parent/child handles alive through verification/publication.
 */
internal class SaveOutputDirectory private constructor(internal val descriptor: Long) : AutoCloseable {
    private var closed = false
    val key = handles.key(descriptor, true)

    fun createDirectory(name: String): SaveOutputDirectory {
        check(!closed)
        childName(name)
        return fromDescriptor(handles.createDirectory(descriptor, name))
    }

    fun openChild(name: String): SaveOutputDirectory {
        check(!closed)
        childName(name)
        return fromDescriptor(handles.openDirectory(descriptor, name))
    }

    fun createFile(name: String): SaveOutputFile {
        check(!closed)
        childName(name)
        return SaveOutputFile.fromDescriptor(handles.createFile(descriptor, name))
    }

    fun openFile(name: String): SaveOutputFile {
        check(!closed)
        childName(name)
        return SaveOutputFile.fromDescriptor(handles.openFile(descriptor, name))
    }

    fun moveFile(name: String, destination: SaveOutputDirectory, target: String) {
        check(!closed && !destination.closed)
        childName(name)
        childName(target)
        handles.renameExclusive(descriptor, name, destination.descriptor, target)
    }

    fun deleteFile(name: String, output: SaveOutputFile) {
        verifyFile(name, output)
        handles.deleteFile(descriptor, name)
    }

    fun deleteEmptyDirectory(name: String) {
        check(!closed)
        childName(name)
        handles.deleteEmptyDirectory(descriptor, name)
    }

    fun flush() {
        check(!closed)
        handles.flush(descriptor)
    }

    fun verifyFile(name: String, output: SaveOutputFile) {
        check(!closed)
        childName(name)
        val current = handles.openFile(descriptor, name)
        try { check(handles.key(current, false) == output.key) { "保存出力の実体が変更されています" } }
        finally { handles.close(current) }
    }

    fun requirePrivate() = handles.requirePrivate(descriptor, true)

    fun requireEntries(outputs: Map<String, SaveOutputFile>) {
        requirePrivate()
        check(entries() == outputs.keys) { "未完成フォルダーに所有を確認できない出力があります" }
        outputs.forEach { (name, output) -> verifyFile(name, output) }
    }

    fun entries(): Set<String> {
        check(!closed)
        return handles.entries(descriptor)
    }

    override fun close() {
        if (!closed) { closed = true; handles.close(descriptor) }
    }

    companion object {
        /** False where a directory cannot be renamed while files inside it are open; close them before publishing. */
        val renamesWithOpenFiles get() = handles.renamesDirectoryWithOpenFiles

        fun open(path: Path): SaveOutputDirectory {
            require(path.isAbsolute && path == path.normalize())
            var current = fromDescriptor(handles.openRoot(path))
            try {
                for (component in path) {
                    val next = current.openChild(component.toString())
                    current.close()
                    current = next
                }
                return current
            } catch (e: Throwable) { current.close(); throw e }
        }

        private fun fromDescriptor(descriptor: Long): SaveOutputDirectory = try { SaveOutputDirectory(descriptor) }
            catch (e: Throwable) { handles.close(descriptor); throw e }
    }
}

/** Keep the descriptor alive across pathname changes; callers also close every separately opened channel. */
internal class SaveOutputFile private constructor(private val descriptor: Long) : AutoCloseable {
    private var closed = false
    val isClosed get() = closed
    val key = handles.key(descriptor, false)

    fun channel(): FileChannel {
        check(!closed)
        return handles.channel(descriptor, setOf(READ, WRITE))
    }

    fun requirePrivate() = handles.requirePrivate(descriptor, false)

    fun hash(): String {
        check(!closed)
        val digest = java.security.MessageDigest.getInstance("SHA-256")
        handles.channel(descriptor, setOf(READ)).use { channel ->
            channel.position(0)
            val bytes = ByteBuffer.allocate(8192)
            while (channel.read(bytes) >= 0) { bytes.flip(); digest.update(bytes); bytes.clear() }
        }
        return java.util.HexFormat.of().formatHex(digest.digest())
    }

    override fun close() {
        if (!closed) { closed = true; handles.close(descriptor) }
    }

    companion object {
        fun fromDescriptor(descriptor: Long): SaveOutputFile = try { SaveOutputFile(descriptor) }
            catch (e: Throwable) { handles.close(descriptor); throw e }
    }
}
