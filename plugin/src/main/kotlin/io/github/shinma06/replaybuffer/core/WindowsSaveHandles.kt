package io.github.shinma06.replaybuffer.core

import com.sun.jna.Library
import com.sun.jna.Memory
import com.sun.jna.Native
import com.sun.jna.Pointer
import com.sun.jna.WString
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.MappedByteBuffer
import java.nio.channels.ClosedChannelException
import java.nio.channels.FileChannel
import java.nio.channels.FileLock
import java.nio.channels.NonReadableChannelException
import java.nio.channels.NonWritableChannelException
import java.nio.channels.ReadableByteChannel
import java.nio.channels.WritableByteChannel
import java.nio.file.AccessDeniedException
import java.nio.file.FileAlreadyExistsException
import java.nio.file.FileSystemException
import java.nio.file.Files
import java.nio.file.NoSuchFileException
import java.nio.file.OpenOption
import java.nio.file.Path
import java.nio.file.StandardCopyOption.ATOMIC_MOVE
import java.nio.file.StandardCopyOption.REPLACE_EXISTING
import java.nio.file.StandardOpenOption.READ
import java.nio.file.StandardOpenOption.WRITE

private interface NtFiles : Library {
    fun NtCreateFile(handle: Memory, access: Int, attributes: Memory, status: Memory, allocation: Pointer?, fileAttributes: Int,
        share: Int, disposition: Int, options: Int, ea: Pointer?, eaLength: Int): Int
    fun NtSetInformationFile(handle: Pointer, status: Memory, information: Memory, length: Int, informationClass: Int): Int
    fun NtFlushBuffersFile(handle: Pointer, status: Memory): Int
    fun NtReadFile(handle: Pointer, event: Pointer?, apc: Pointer?, context: Pointer?, status: Memory, buffer: Pointer, length: Int,
        offset: Memory, key: Pointer?): Int
    fun NtWriteFile(handle: Pointer, event: Pointer?, apc: Pointer?, context: Pointer?, status: Memory, buffer: Pointer, length: Int,
        offset: Memory, key: Pointer?): Int
    fun NtQueryInformationFile(handle: Pointer, status: Memory, information: Memory, length: Int, informationClass: Int): Int
    fun NtClose(handle: Pointer): Int
}

/**
 * Studio keeps JNA's native part in `lib/jna/<arch>` beside its runtime and sets `jna.boot.library.path` for the IDE.
 * A packaged CLI started on Studio's runtime finds the same native part there, however it was launched.
 */
internal object StudioJna {
    init {
        if (System.getProperty("jna.boot.library.path") == null) {
            val arch = when (System.getProperty("os.arch")) { "amd64", "x86_64" -> "amd64"; "aarch64", "arm64" -> "aarch64"; else -> null }
            val home = Path.of(System.getProperty("java.home")).parent
            val directory = if (arch == null || home == null) null else home.resolve("lib").resolve("jna").resolve(arch)
            if (directory != null && Files.isRegularFile(directory.resolve(System.mapLibraryName("jnidispatch")))) {
                System.setProperty("jna.boot.library.path", directory.toString())
            }
        }
    }

    /** Call before the first use of any JNA class. */
    fun ensure() {}

    fun <T : Library> load(name: String, type: Class<T>): T = Native.load(name, type)
}

private interface Win32Files : Library {
    fun CreateFileW(name: WString, access: Int, share: Int, security: Pointer?, disposition: Int, flags: Int, template: Pointer?): Pointer
    fun GetFileInformationByHandle(handle: Pointer, information: Memory): Boolean
    fun GetFileInformationByHandleEx(handle: Pointer, informationClass: Int, buffer: Memory, size: Int): Boolean
    fun GetFinalPathNameByHandleW(handle: Pointer, buffer: CharArray, size: Int, flags: Int): Int
}

/**
 * Windows save handles: NT opens relative to a held directory handle
 * (like openat), never following reparse points, and identity is the volume serial plus file index.
 */
internal object WindowsSaveHandles : SaveHandles {
    private val nt by lazy { StudioJna.load("ntdll", NtFiles::class.java) }
    private val win32 by lazy { StudioJna.load("kernel32", Win32Files::class.java) }

    private const val SHARE_ALL = 7
    private const val SYNCHRONIZE = 0x100000
    private const val DELETE = 0x10000
    private const val DIRECTORY_ACCESS = 0x1200A1 // list, traverse, read attributes, read control, synchronize
    private const val FILE_READ = 0x80000000.toInt() or SYNCHRONIZE
    private const val FILE_READ_WRITE = 0xC0000000.toInt() or SYNCHRONIZE
    private const val FILE_WRITE = 0x40000000 or SYNCHRONIZE
    private const val SYNCHRONOUS = 0x20
    private const val NO_REPARSE = 0x200000
    private const val DIRECTORY = 0x1
    private const val NON_DIRECTORY = 0x40
    private const val FILE_OPEN = 1
    private const val FILE_CREATE = 2
    private const val NAME_COLLISION = 0xC0000035.toInt()
    private const val ATTRIBUTE_DIRECTORY = 0x10
    private const val ATTRIBUTE_REPARSE = 0x400
    private const val OPEN_REPARSE_POINT = 0x00200000
    private const val RENAME_REPLACE_POSIX = 0x3 // FILE_RENAME_REPLACE_IF_EXISTS | FILE_RENAME_POSIX_SEMANTICS

    override val renamesDirectoryWithOpenFiles = false

    init { StudioJna.ensure() }

    override fun requireName(name: String) {
        require(name.none { it in "\\:<>\"|?*" || it < ' ' } && !name.endsWith('.') && !name.endsWith(' '))
    }

    /** Win32 name that is not limited to MAX_PATH. */
    private fun win32Name(path: Path): WString {
        val name = path.toAbsolutePath().normalize().toString()
        return WString(when {
            name.startsWith("\\\\?\\") -> name
            name.startsWith("\\\\") -> "\\\\?\\UNC\\" + name.removePrefix("\\\\")
            else -> "\\\\?\\" + name
        })
    }

    private fun ntName(path: Path): String {
        val name = path.toAbsolutePath().normalize().toString()
        return when {
            name.startsWith("\\\\?\\") -> "\\??\\" + name.removePrefix("\\\\?\\")
            name.startsWith("\\\\") -> "\\??\\UNC\\" + name.removePrefix("\\\\")
            else -> "\\??\\" + name
        }
    }

    override fun openRoot(path: Path): Long {
        val root = path.root.toString()
        val name = if (root.startsWith("\\\\")) "\\\\?\\UNC\\" + root.removePrefix("\\\\") else "\\\\?\\$root"
        val handle = win32.CreateFileW(WString(name), DIRECTORY_ACCESS, SHARE_ALL, null, 3, 0x02000000 or 0x00200000, null)
        val value = Pointer.nativeValue(handle)
        if (value == -1L || value == 0L) throw IOException("保存出力を開けません（error=${Native.getLastError()}）")
        return value
    }

    override fun openDirectory(parent: Long, name: String) = open(parent, name, DIRECTORY_ACCESS, FILE_OPEN, DIRECTORY)
    override fun createDirectory(parent: Long, name: String) =
        WindowsPrivateFiles.withOwnerOnlyDescriptor(true) { open(parent, name, DIRECTORY_ACCESS, FILE_CREATE, DIRECTORY, it) }
    override fun openFile(parent: Long, name: String) = open(parent, name, FILE_READ, FILE_OPEN, NON_DIRECTORY)
    override fun createFile(parent: Long, name: String) =
        WindowsPrivateFiles.withOwnerOnlyDescriptor(false) { open(parent, name, FILE_READ_WRITE, FILE_CREATE, NON_DIRECTORY, it) }

    private fun open(parent: Long, name: String, access: Int, disposition: Int, kind: Int, security: Pointer? = null): Long {
        val chars = name.toCharArray()
        val buffer = Memory(chars.size * 2L + 2).apply { clear(); write(0, chars, 0, chars.size) }
        val unicode = Memory(16).apply {
            clear(); setShort(0, (chars.size * 2).toShort()); setShort(2, (chars.size * 2 + 2).toShort()); setPointer(8, buffer)
        }
        val attributes = Memory(48).apply {
            clear(); setInt(0, 48); setLong(8, parent); setPointer(16, unicode); setInt(24, 0x40) // OBJ_CASE_INSENSITIVE
            setPointer(32, security)
        }
        val handle = Memory(8).apply { clear() }
        val status = nt.NtCreateFile(handle, access, attributes, Memory(16), null, 0x80, SHARE_ALL, disposition,
            kind or SYNCHRONOUS or NO_REPARSE, null, 0)
        if (status == NAME_COLLISION) throw FileAlreadyExistsException(name)
        if (status != 0) throw IOException("保存出力を開けません（status=0x${Integer.toHexString(status)}）")
        return handle.getLong(0)
    }

    /** Device/inode-like identity; also rejects reparse points and the wrong kind of item. */
    override fun key(handle: Long, directory: Boolean): Pair<Long, Long> = Memory(52).use { information ->
        if (!win32.GetFileInformationByHandle(Pointer(handle), information)) throw IOException("保存出力の実体を確認できません")
        val attributes = information.getInt(0)
        check(attributes and ATTRIBUTE_REPARSE == 0 && (attributes and ATTRIBUTE_DIRECTORY != 0) == directory) {
            "保存出力の実体を確認できません"
        }
        (information.getInt(28).toLong() and 0xffffffffL) to
            ((information.getInt(44).toLong() shl 32) or (information.getInt(48).toLong() and 0xffffffffL))
    }

    /** Identity of an item addressed by path, without following a final reparse point. */
    fun identity(path: Path): Pair<Long, Long> {
        val handle = Pointer.nativeValue(win32.CreateFileW(win32Name(path), 0x80, SHARE_ALL, null, 3, 0x02000000 or 0x00200000, null))
        if (handle == -1L || handle == 0L) throw IOException("ファイルの実体を確認できません（error=${Native.getLastError()}）")
        try {
            Memory(52).use { information ->
                if (!win32.GetFileInformationByHandle(Pointer(handle), information)) throw IOException("ファイルの実体を確認できません")
                return (information.getInt(28).toLong() and 0xffffffffL) to
                    ((information.getInt(44).toLong() shl 32) or (information.getInt(48).toLong() and 0xffffffffL))
            }
        } finally { close(handle) }
    }

    /**
     * Atomically replaces [target] with [source] like POSIX rename(2): the replaced item may still be open, and the
     * caller's own access to both items and their directory is checked. MoveFileEx refuses an open target and lets an
     * elevated token bypass the directory's ACL.
     */
    fun replace(source: Path, target: Path) {
        val handle = Pointer.nativeValue(win32.CreateFileW(win32Name(source), DELETE or SYNCHRONIZE, SHARE_ALL, null, 3,
            OPEN_REPARSE_POINT, null))
        if (handle == -1L || handle == 0L) throw failure(Native.getLastError(), source, target)
        val status = try {
            val chars = ntName(target).toCharArray()
            val information = Memory(24L + chars.size * 2).apply {
                clear(); setInt(0, RENAME_REPLACE_POSIX); setInt(16, chars.size * 2); write(20, chars, 0, chars.size)
            }
            nt.NtSetInformationFile(Pointer(handle), Memory(16), information, information.size().toInt(), 65) // FileRenameInformationEx
        } finally { close(handle) }
        when (status) {
            0 -> return
            // Filesystems without POSIX rename keep the platform's replacing move.
            0xC0000003.toInt(), 0xC000000D.toInt(), 0xC00000BB.toInt() -> Files.move(source, target, ATOMIC_MOVE, REPLACE_EXISTING)
            0xC0000022.toInt() -> throw AccessDeniedException(source.toString(), target.toString(), null)
            0xC0000043.toInt() -> throw FileSystemException(source.toString(), target.toString(), "使用中のため置き換えられません")
            0xC0000034.toInt(), 0xC000003A.toInt() -> throw NoSuchFileException(source.toString(), target.toString(), null)
            else -> throw FileSystemException(source.toString(), target.toString(), "status=0x${Integer.toHexString(status)}")
        }
    }

    private fun failure(error: Int, source: Path, target: Path): IOException = when (error) {
        2, 3 -> NoSuchFileException(source.toString(), target.toString(), null)
        5 -> AccessDeniedException(source.toString(), target.toString(), null)
        32 -> FileSystemException(source.toString(), target.toString(), "使用中のため置き換えられません")
        else -> FileSystemException(source.toString(), target.toString(), "error=$error")
    }

    override fun renameExclusive(sourceParent: Long, source: String, destinationParent: Long, destination: String) {
        val handle = try { open(sourceParent, source, DELETE or SYNCHRONIZE, FILE_OPEN, 0) }
            catch (_: IOException) { error("完成先の公開に失敗しました（既存出力・対応filesystem・アクセス権を確認してください）") }
        try {
            val chars = destination.toCharArray()
            val information = Memory(24L + chars.size * 2).apply {
                clear(); setLong(8, destinationParent); setInt(16, chars.size * 2); write(20, chars, 0, chars.size)
            }
            val status = nt.NtSetInformationFile(Pointer(handle), Memory(16), information, information.size().toInt(), 10)
            check(status == 0) { "完成先の公開に失敗しました（既存出力・対応filesystem・アクセス権を確認してください）" }
        } finally { close(handle) }
    }

    override fun deleteFile(parent: Long, name: String) = delete(parent, name, false)

    override fun deleteEmptyDirectory(parent: Long, name: String) =
        runCatching { delete(parent, name, true) }.getOrElse { error("削除用の空フォルダを片付けられません。") }

    private fun delete(parent: Long, name: String, directory: Boolean) {
        val handle = open(parent, name, DELETE or SYNCHRONIZE, FILE_OPEN, if (directory) DIRECTORY else NON_DIRECTORY)
        try {
            val information = Memory(8).apply { clear(); setByte(0, 1) }
            check(nt.NtSetInformationFile(Pointer(handle), Memory(16), information, 1, 13) == 0) { "削除できません" }
        } finally { close(handle) }
    }

    override fun flush(directory: Long) {
        // NtFlushBuffersFile needs write access, so flush through a second handle to the same directory.
        val handle = open(directory, "", 0x2 or SYNCHRONIZE, FILE_OPEN, DIRECTORY)
        try { check(nt.NtFlushBuffersFile(Pointer(handle), Memory(16)) == 0) { "保存先への書込み完了を確認できません。" } }
        finally { close(handle) }
    }

    override fun entries(directory: Long): Set<String> {
        val buffer = Memory(64 * 1024)
        val names = linkedSetOf<String>()
        var informationClass = 15 // FileFullDirectoryRestartInfo, then FileFullDirectoryInfo
        while (true) {
            if (!win32.GetFileInformationByHandleEx(Pointer(directory), informationClass, buffer, buffer.size().toInt())) {
                if (Native.getLastError() == 18) { buffer.close(); return names } // ERROR_NO_MORE_FILES
                buffer.close()
                throw IOException("未完成フォルダーの内容を確認できません")
            }
            informationClass = 14
            var offset = 0L
            while (true) {
                val name = String(buffer.getCharArray(offset + 68, buffer.getInt(offset + 60) / 2))
                if (name != "." && name != "..") check(names.add(name)) { "未完成フォルダーの内容が変更されています" }
                val next = buffer.getInt(offset)
                if (next == 0) break
                offset += next
            }
        }
    }

    fun path(handle: Long): Path {
        val buffer = CharArray(32 * 1024)
        val length = win32.GetFinalPathNameByHandleW(Pointer(handle), buffer, buffer.size, 0)
        if (length <= 0 || length >= buffer.size) throw IOException("保存出力の場所を確認できません")
        val name = String(buffer, 0, length)
        return Path.of(if (name.startsWith("\\\\?\\UNC\\")) "\\\\" + name.removePrefix("\\\\?\\UNC\\") else name.removePrefix("\\\\?\\"))
    }

    /**
     * Java cannot adopt a Win32 handle, and opening by path lets another process swap the item in between, so reopen
     * the held item itself (a relative open with an empty name) and confirm the new handle is the same item.
     */
    override fun channel(handle: Long, options: Set<OpenOption>): FileChannel {
        val readable = READ in options
        val writable = WRITE in options
        require((readable || writable) && options.all { it == READ || it == WRITE })
        val access = if (readable && writable) FILE_READ_WRITE else if (readable) FILE_READ else FILE_WRITE
        return adopt(handle, open(handle, "", access, FILE_OPEN, NON_DIRECTORY), readable, writable)
    }

    /** Wraps [opened] only when it is the same item as [held]; otherwise closes it and fails the save. */
    internal fun adopt(held: Long, opened: Long, readable: Boolean, writable: Boolean): FileChannel = try {
        check(key(opened, false) == key(held, false)) { "保存出力の実体が変更されています" }
        HandleChannel(nt, opened, readable, writable)
    } catch (failure: Throwable) { close(opened); throw failure }

    override fun requirePrivate(handle: Long, directory: Boolean) {
        check(WindowsPrivateFiles.isPrivate(path(handle), directory)) { "未完成フォルダーのアクセス権を確認できません" }
    }

    override fun close(handle: Long) { nt.NtClose(Pointer(handle)) }
}

/**
 * FileChannel over an owned NT file handle. Every transfer passes an explicit offset, so the handle's own file pointer
 * is unused; like the JDK's channel, an interrupt closes it.
 */
private class HandleChannel(private val nt: NtFiles, private var handle: Long, private val readable: Boolean,
    private val writable: Boolean) : FileChannel() {
    private val lock = Any()
    private var position = 0L

    private inline fun <T> io(block: (Pointer) -> T): T {
        if (!isOpen) throw ClosedChannelException()
        var completed = false
        var result: T? = null
        // end() must run outside the lock: an interrupting thread closes the channel while holding the thread's interrupt lock.
        try {
            begin()
            synchronized(lock) { if (handle != 0L) { result = block(Pointer(handle)); completed = true } }
        } finally { end(completed) }
        if (!completed) throw ClosedChannelException()
        @Suppress("UNCHECKED_CAST") return result as T
    }

    private fun transfer(file: Pointer, buffer: ByteBuffer, offset: Long, write: Boolean): Int {
        if (write && !writable) throw NonWritableChannelException()
        if (!write && !readable) throw NonReadableChannelException()
        require(offset >= 0)
        require(write || !buffer.isReadOnly) { "Read-only buffer" }
        val count = minOf(buffer.remaining(), CHUNK)
        if (count == 0) return 0
        val copy = if (buffer.isDirect) null else Memory(count.toLong())
        try {
            val data = copy ?: Native.getDirectBufferPointer(buffer).share(buffer.position().toLong())
            if (write && copy != null) copy.write(0, ByteArray(count).also { buffer.duplicate().get(it) }, 0, count)
            val status = Memory(16).apply { clear() }
            val at = Memory(8).apply { setLong(0, offset) }
            val result = if (write) nt.NtWriteFile(file, null, null, null, status, data, count, at, null)
                else nt.NtReadFile(file, null, null, null, status, data, count, at, null)
            if (!write && result == END_OF_FILE) return -1
            if (result != 0) throw IOException("保存出力を${if (write) "書き込め" else "読み込め"}ません（status=0x${Integer.toHexString(result)}）")
            val done = status.getLong(8).toInt()
            if (!write && done == 0) return -1
            if (!write && copy != null) buffer.put(copy.getByteArray(0, done)) else buffer.position(buffer.position() + done)
            return done
        } finally { copy?.close() }
    }

    private fun size(file: Pointer): Long = Memory(24).use { information ->
        val result = nt.NtQueryInformationFile(file, Memory(16), information, 24, 5) // FileStandardInformation
        if (result != 0) throw IOException("保存出力の大きさを確認できません（status=0x${Integer.toHexString(result)}）")
        information.getLong(8)
    }

    override fun read(dst: ByteBuffer): Int = io { file -> transfer(file, dst, position, false).also { if (it > 0) position += it } }
    override fun write(src: ByteBuffer): Int = io { file -> transfer(file, src, position, true).also { position += it } }
    override fun read(dst: ByteBuffer, position: Long): Int = io { file -> transfer(file, dst, position, false) }
    override fun write(src: ByteBuffer, position: Long): Int = io { file -> transfer(file, src, position, true) }

    override fun read(dsts: Array<out ByteBuffer>, offset: Int, length: Int): Long {
        java.util.Objects.checkFromIndexSize(offset, length, dsts.size)
        return io { file ->
            var total = 0L
            for (index in offset until offset + length) while (dsts[index].hasRemaining()) {
                val done = transfer(file, dsts[index], position, false)
                if (done < 0) return@io if (total == 0L) -1L else total
                position += done; total += done
            }
            total
        }
    }

    override fun write(srcs: Array<out ByteBuffer>, offset: Int, length: Int): Long {
        java.util.Objects.checkFromIndexSize(offset, length, srcs.size)
        return io { file ->
            var total = 0L
            for (index in offset until offset + length) while (srcs[index].hasRemaining()) {
                val done = transfer(file, srcs[index], position, true)
                position += done; total += done
            }
            total
        }
    }

    override fun position(): Long = io { position }

    override fun position(newPosition: Long): FileChannel {
        require(newPosition >= 0)
        io { position = newPosition }
        return this
    }

    override fun size(): Long = io { file -> size(file) }

    override fun truncate(size: Long): FileChannel {
        require(size >= 0)
        if (!writable) throw NonWritableChannelException()
        io { file ->
            if (size < size(file)) Memory(8).use { information ->
                information.setLong(0, size)
                val result = nt.NtSetInformationFile(file, Memory(16), information, 8, 20) // FileEndOfFileInformation
                if (result != 0) throw IOException("保存出力を切り詰められません（status=0x${Integer.toHexString(result)}）")
            }
            if (position > size) position = size
        }
        return this
    }

    override fun force(metaData: Boolean) = io { file ->
        if (!writable) return@io
        val result = nt.NtFlushBuffersFile(file, Memory(16))
        if (result != 0) throw IOException("保存先への書込み完了を確認できません。")
    }

    override fun transferTo(position: Long, count: Long, target: WritableByteChannel): Long {
        require(position >= 0 && count >= 0)
        val buffer = ByteBuffer.allocate(CHUNK)
        var total = 0L
        while (total < count) {
            buffer.clear().limit(minOf(CHUNK.toLong(), count - total).toInt())
            if (read(buffer, position + total) <= 0) break
            buffer.flip()
            while (buffer.hasRemaining()) total += target.write(buffer)
        }
        return total
    }

    override fun transferFrom(src: ReadableByteChannel, position: Long, count: Long): Long {
        require(position >= 0 && count >= 0)
        val buffer = ByteBuffer.allocate(CHUNK)
        var total = 0L
        while (total < count) {
            buffer.clear().limit(minOf(CHUNK.toLong(), count - total).toInt())
            if (src.read(buffer) <= 0) break
            buffer.flip()
            while (buffer.hasRemaining()) total += write(buffer, position + total)
        }
        return total
    }

    override fun map(mode: FileChannel.MapMode, position: Long, size: Long): MappedByteBuffer =
        throw UnsupportedOperationException("保存出力はmapできません")

    override fun lock(position: Long, size: Long, shared: Boolean): FileLock =
        throw UnsupportedOperationException("保存出力はlockできません")

    override fun tryLock(position: Long, size: Long, shared: Boolean): FileLock =
        throw UnsupportedOperationException("保存出力はlockできません")

    override fun implCloseChannel() {
        synchronized(lock) { if (handle != 0L) { nt.NtClose(Pointer(handle)); handle = 0L } }
    }

    private companion object {
        const val CHUNK = 1 shl 20
        const val END_OF_FILE = 0xC0000011.toInt()
    }
}
